//! Background workers for webhook delivery dispatch.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use hookbox::state::{RetryPolicy, WebhookDelivery};
use hookbox::traits::Emitter;
use hookbox::transitions::compute_backoff;
use hookbox::types::NormalizedEvent;
use hookbox_postgres::{DeliveryStorage, PostgresStorage};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use hookbox::traits::Storage;

/// Periodically retries receipts stuck in `EmitFailed` state.
pub struct RetryWorker {
    storage: PostgresStorage,
    emitter: Box<dyn Emitter + Send + Sync>,
    interval: Duration,
    max_attempts: i32,
}

impl RetryWorker {
    /// Create a new retry worker with the given storage, emitter, poll interval,
    /// and maximum retry attempts before dead-lettering.
    #[must_use]
    pub fn new(
        storage: PostgresStorage,
        emitter: Box<dyn Emitter + Send + Sync>,
        interval: Duration,
        max_attempts: i32,
    ) -> Self {
        Self {
            storage,
            emitter,
            interval,
            max_attempts,
        }
    }

    /// Spawn the worker as a background Tokio task.
    ///
    /// The returned [`JoinHandle`] can be used to monitor or cancel the
    /// worker.
    #[must_use]
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move { self.run().await })
    }

    /// Main loop: sleep, query, retry, repeat.
    async fn run(&self) {
        loop {
            tokio::time::sleep(self.interval).await;
            match self.storage.query_for_retry(self.max_attempts).await {
                Ok(receipts) => {
                    if !receipts.is_empty() {
                        tracing::info!(count = receipts.len(), "retrying failed receipts");
                    }
                    for receipt in receipts {
                        self.retry_one(&receipt).await;
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "retry worker: query failed");
                }
            }
        }
    }

    /// Attempt to re-emit a single failed receipt.
    async fn retry_one(&self, receipt: &hookbox::types::WebhookReceipt) {
        let event = NormalizedEvent::from_receipt(receipt);

        match self.emitter.emit(&event).await {
            Ok(()) => {
                if let Err(e) = self
                    .storage
                    .update_state(
                        receipt.receipt_id.0,
                        hookbox::ProcessingState::Emitted,
                        None,
                    )
                    .await
                {
                    tracing::error!(
                        receipt_id = %receipt.receipt_id,
                        error = %e,
                        "retry: emit succeeded but state update failed — receipt may be re-emitted (at-least-once)"
                    );
                } else {
                    tracing::info!(receipt_id = %receipt.receipt_id, "retry: emit succeeded");
                }
            }
            Err(e) => {
                tracing::warn!(
                    receipt_id = %receipt.receipt_id,
                    error = %e,
                    "retry: emit failed"
                );
                if let Err(db_err) = self
                    .storage
                    .retry_failed(receipt.receipt_id.0, self.max_attempts)
                    .await
                {
                    tracing::error!(
                        receipt_id = %receipt.receipt_id,
                        error = %db_err,
                        "retry: failed to update emit_count — receipt stuck in emit_failed"
                    );
                }
            }
        }
    }
}

// ── EmitterWorker ─────────────────────────────────────────────────────────────

/// Per-emitter background dispatch worker.
///
/// One `EmitterWorker` runs per configured emitter. It polls for pending
/// deliveries, attempts dispatch, and updates delivery state on success or
/// failure. Expired in-flight leases are reclaimed before each poll.
pub struct EmitterWorker<S: DeliveryStorage + Send + Sync + 'static> {
    /// Logical name of the emitter (matches the TOML config key).
    pub name: String,
    /// The downstream emitter to call for each delivery.
    pub emitter: Arc<dyn Emitter + Send + Sync>,
    /// Storage backend used to claim and update deliveries.
    pub storage: S,
    /// Per-emitter retry/backoff policy.
    pub policy: RetryPolicy,
    /// Maximum number of in-flight deliveries at one time.
    pub concurrency: usize,
    /// How often the worker wakes to poll for new work.
    pub poll_interval: Duration,
    /// How long an in-flight lease may be held before it is reclaimed.
    pub lease_duration: Duration,
    /// Live health snapshot, updated after every dispatch outcome.
    pub health: Arc<ArcSwap<EmitterHealth>>,
}

/// Snapshot of an emitter worker's operational health.
#[derive(Debug, Clone, Default)]
pub struct EmitterHealth {
    /// Timestamp of the most recent successful emit.
    pub last_success_at: Option<chrono::DateTime<Utc>>,
    /// Timestamp of the most recent failed emit.
    pub last_failure_at: Option<chrono::DateTime<Utc>>,
    /// Number of consecutive failures since the last success.
    pub consecutive_failures: u32,
    /// Derived health status.
    pub status: HealthStatus,
    /// Current depth of the dead-letter queue for this emitter.
    pub dlq_depth: u64,
    /// Number of pending deliveries awaiting dispatch for this emitter.
    pub pending_count: u64,
}

/// Coarse health classification for an emitter worker.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    /// No recent failures.
    #[default]
    Healthy,
    /// Recent failures but below the unhealthy threshold.
    Degraded,
    /// Ten or more consecutive failures.
    Unhealthy,
}

impl<S: DeliveryStorage + Send + Sync + 'static> EmitterWorker<S> {
    /// Spawn the worker loop and return a handle.
    ///
    /// The worker watches `shutdown_rx` and exits after finishing its
    /// current batch when the signal fires.
    pub fn spawn(self, mut shutdown_rx: watch::Receiver<bool>) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = tokio::time::sleep(self.poll_interval) => {}
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() { break; }
                    }
                }

                // Reclaim orphaned in-flight rows before claiming new work.
                match self
                    .storage
                    .reclaim_expired(&self.name, self.lease_duration)
                    .await
                {
                    Ok(n) if n > 0 => {
                        tracing::warn!(
                            emitter = %self.name,
                            reclaimed = n,
                            "reclaimed expired in-flight deliveries"
                        );
                        metrics::counter!(
                            "hookbox_emit_reclaimed_total",
                            "emitter" => self.name.clone()
                        )
                        .increment(n);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::error!(emitter = %self.name, err = %e, "reclaim_expired failed");
                    }
                }

                #[expect(
                    clippy::cast_possible_wrap,
                    reason = "concurrency is always small in practice"
                )]
                let concurrency_i64 = self.concurrency as i64;
                let rows = match self
                    .storage
                    .claim_pending(&self.name, concurrency_i64)
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::error!(emitter = %self.name, err = %e, "claim_pending failed");
                        continue;
                    }
                };

                if rows.is_empty() {
                    self.refresh_gauges().await;
                    continue;
                }

                futures::future::join_all(
                    rows.into_iter()
                        .map(|(delivery, receipt)| self.dispatch_one(delivery, receipt)),
                )
                .await;

                self.refresh_gauges().await;

                // Check shutdown synchronously after completing the batch.
                // `changed()` only fires on *transitions*, so if the signal
                // arrived while the batch was running it may already be
                // "seen" by the time the loop returns to `select!`.
                // `borrow_and_update` marks the value seen AND reads it,
                // so the next `changed()` call correctly waits for the next
                // transition rather than immediately resolving.
                if *shutdown_rx.borrow_and_update() {
                    break;
                }
            }
        })
    }

    /// Attempt to dispatch a single claimed delivery and update its state.
    pub async fn dispatch_one(
        &self,
        delivery: WebhookDelivery,
        receipt: hookbox::types::WebhookReceipt,
    ) {
        let event = NormalizedEvent::from_receipt(&receipt);
        let start = std::time::Instant::now();
        let result = self.emitter.emit(&event).await;
        let elapsed = start.elapsed().as_secs_f64();
        let next_attempt = delivery.attempt_count + 1;

        metrics::histogram!("hookbox_emit_duration_seconds", "emitter" => self.name.clone())
            .record(elapsed);

        match result {
            Ok(()) => {
                metrics::counter!(
                    "hookbox_emit_results_total",
                    "provider" => receipt.provider_name.clone(),
                    "emitter"  => self.name.clone(),
                    "result"   => "success",
                )
                .increment(1);
                metrics::histogram!(
                    "hookbox_emit_attempt_count",
                    "emitter" => self.name.clone()
                )
                .record(f64::from(next_attempt));
                if let Err(e) = self.storage.mark_emitted(delivery.delivery_id).await {
                    tracing::error!(emitter = %self.name, err = %e, "mark_emitted failed");
                }
                self.update_health(true);
            }
            Err(err) => {
                metrics::counter!(
                    "hookbox_emit_results_total",
                    "provider" => receipt.provider_name.clone(),
                    "emitter"  => self.name.clone(),
                    "result"   => "failure",
                )
                .increment(1);
                if next_attempt >= self.policy.max_attempts {
                    metrics::histogram!(
                        "hookbox_emit_attempt_count",
                        "emitter" => self.name.clone()
                    )
                    .record(f64::from(next_attempt));
                    if let Err(e) = self
                        .storage
                        .mark_dead_lettered(delivery.delivery_id, &err.to_string())
                        .await
                    {
                        tracing::error!(
                            emitter = %self.name,
                            err = %e,
                            "mark_dead_lettered failed"
                        );
                    }
                } else {
                    let backoff = compute_backoff(next_attempt, &self.policy);
                    let next_delta = chrono::Duration::from_std(backoff).unwrap_or_else(|_| {
                        tracing::warn!(
                            emitter = %self.name,
                            attempt = next_attempt,
                            ?backoff,
                            "compute_backoff returned a duration that exceeds \
                             chrono::Duration range; saturating to 1 day"
                        );
                        chrono::Duration::days(1)
                    });
                    let next_at = Utc::now() + next_delta;
                    if let Err(e) = self
                        .storage
                        .mark_failed(
                            delivery.delivery_id,
                            next_attempt,
                            next_at,
                            &err.to_string(),
                        )
                        .await
                    {
                        tracing::error!(emitter = %self.name, err = %e, "mark_failed failed");
                    }
                }
                self.update_health(false);
            }
        }
    }

    /// Update the health snapshot after a dispatch outcome.
    ///
    /// Success resets `consecutive_failures` to 0. Failure increments it.
    /// Status rules:
    /// - `≥ 10` consecutive failures → `Unhealthy`
    /// - `last_failure_at` within 60 s → `Degraded`
    /// - otherwise → `Healthy`
    ///
    /// # Concurrency note
    ///
    /// This is intentionally a non-atomic load-clone-store on the [`ArcSwap`].
    /// Under `concurrency > 1`, two concurrent calls from [`Self::dispatch_one`]
    /// futures (driven by `join_all`) can race: both read the same snapshot,
    /// both compute `consecutive_failures + 1`, and one write is silently
    /// dropped. The counter is therefore **approximate** — it drives
    /// operator-facing health status, not correctness invariants. Introducing
    /// a `Mutex` or CAS loop would add coordination cost that is not worth the
    /// precision for monitoring data.
    pub fn update_health(&self, success: bool) {
        let current = self.health.load();
        let mut next = (**current).clone();
        if success {
            next.last_success_at = Some(Utc::now());
            next.consecutive_failures = 0;
        } else {
            next.last_failure_at = Some(Utc::now());
            next.consecutive_failures += 1;
        }
        next.status = derive_status(next.consecutive_failures, next.last_failure_at);
        self.health.store(Arc::new(next));
    }

    /// Refresh DLQ depth, pending count, and in-flight count gauges.
    ///
    /// Writes Prometheus gauges for operator dashboards and updates the
    /// in-process health snapshot consumed by `/readyz`. Also re-evaluates
    /// the `Degraded → Healthy` decay: [`Self::update_health`] only runs
    /// after a dispatch, so an emitter that failed and then went idle
    /// would remain `Degraded` indefinitely. Recomputing the status here
    /// on each tick lets the in-process snapshot decay on its own.
    async fn refresh_gauges(&self) {
        let dlq = self.storage.count_dlq(&self.name).await.unwrap_or(0);
        let pending = self.storage.count_pending(&self.name).await.unwrap_or(0);
        let in_flight = self.storage.count_in_flight(&self.name).await.unwrap_or(0);

        // Update Prometheus gauges (operator-visible).
        #[expect(
            clippy::cast_precision_loss,
            reason = "u64 → f64 for gauge values; counts are bounded in practice"
        )]
        {
            metrics::gauge!("hookbox_dlq_depth", "emitter" => self.name.clone()).set(dlq as f64);
            metrics::gauge!("hookbox_emit_pending", "emitter" => self.name.clone())
                .set(pending as f64);
            metrics::gauge!("hookbox_emit_in_flight", "emitter" => self.name.clone())
                .set(in_flight as f64);
        }

        // Update in-process health snapshot (consumed by /readyz).
        let current = self.health.load();
        let mut next = (**current).clone();
        next.dlq_depth = dlq;
        next.pending_count = pending;
        next.status = derive_status(next.consecutive_failures, next.last_failure_at);
        self.health.store(Arc::new(next));
    }
}

/// Derive an [`HealthStatus`] from failure counters. Shared between
/// [`EmitterWorker::update_health`] (dispatch-driven) and
/// [`EmitterWorker::refresh_gauges`] (tick-driven) so the tick path can
/// decay `Degraded → Healthy` on its own once the failure window expires.
fn derive_status(
    consecutive_failures: u32,
    last_failure_at: Option<DateTime<Utc>>,
) -> HealthStatus {
    if consecutive_failures >= 10 {
        HealthStatus::Unhealthy
    } else if last_failure_at
        .is_some_and(|t| Utc::now().signed_duration_since(t).num_seconds() < 60)
    {
        HealthStatus::Degraded
    } else {
        HealthStatus::Healthy
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "unwrap is acceptable in test code")]
#[expect(clippy::expect_used, reason = "expect is acceptable in test code")]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use chrono::Utc;
    use hookbox::error::EmitError;
    use hookbox::state::{
        DeliveryId, DeliveryState, ProcessingState, ReceiptId, RetryPolicy, WebhookDelivery,
    };
    use hookbox::traits::Emitter;
    use hookbox::types::{NormalizedEvent, WebhookReceipt};
    use hookbox_postgres::DeliveryStorage;
    use uuid::Uuid;

    // ── FakeEmitter ──────────────────────────────────────────────────────────

    struct FakeEmitter {
        should_succeed: Arc<Mutex<bool>>,
    }

    impl FakeEmitter {
        fn always_ok() -> Arc<Self> {
            Arc::new(Self {
                should_succeed: Arc::new(Mutex::new(true)),
            })
        }

        fn always_err() -> Arc<Self> {
            Arc::new(Self {
                should_succeed: Arc::new(Mutex::new(false)),
            })
        }
    }

    #[async_trait]
    impl Emitter for FakeEmitter {
        async fn emit(&self, _event: &NormalizedEvent) -> Result<(), EmitError> {
            if *self.should_succeed.lock().unwrap() {
                Ok(())
            } else {
                Err(EmitError::Downstream("fake failure".to_owned()))
            }
        }
    }

    // ── MemDeliveryStorage ───────────────────────────────────────────────────

    /// In-memory `DeliveryStorage` for unit tests.
    struct MemDeliveryStorage {
        deliveries: Mutex<HashMap<Uuid, WebhookDelivery>>,
        receipts: Mutex<HashMap<Uuid, WebhookReceipt>>,
    }

    impl MemDeliveryStorage {
        fn with_in_flight(delivery_id: Uuid, receipt_id: Uuid, attempt_count: i32) -> Arc<Self> {
            let mut deliveries = HashMap::new();
            let delivery = WebhookDelivery {
                delivery_id: DeliveryId(delivery_id),
                receipt_id: ReceiptId(receipt_id),
                emitter_name: "test".to_owned(),
                state: DeliveryState::InFlight,
                attempt_count,
                last_error: None,
                last_attempt_at: Some(Utc::now()),
                next_attempt_at: Utc::now(),
                emitted_at: None,
                immutable: false,
                created_at: Utc::now(),
            };
            deliveries.insert(delivery_id, delivery);

            let mut receipts = HashMap::new();
            let receipt = WebhookReceipt {
                receipt_id: ReceiptId(receipt_id),
                provider_name: "test".to_owned(),
                provider_event_id: None,
                external_reference: None,
                dedupe_key: format!("test:{receipt_id}"),
                payload_hash: "h".to_owned(),
                raw_body: b"{}".to_vec(),
                parsed_payload: None,
                raw_headers: serde_json::Value::Object(serde_json::Map::default()),
                normalized_event_type: None,
                verification_status: hookbox::state::VerificationStatus::Verified,
                verification_reason: None,
                processing_state: ProcessingState::Stored,
                emit_count: 0,
                last_error: None,
                received_at: Utc::now(),
                processed_at: None,
                metadata: serde_json::Value::Object(serde_json::Map::default()),
            };
            receipts.insert(receipt_id, receipt);

            Arc::new(Self {
                deliveries: Mutex::new(deliveries),
                receipts: Mutex::new(receipts),
            })
        }

        fn get(&self, id: Uuid) -> Option<WebhookDelivery> {
            self.deliveries.lock().unwrap().get(&id).cloned()
        }
    }

    #[async_trait]
    impl DeliveryStorage for MemDeliveryStorage {
        type Error = hookbox::error::StorageError;

        async fn claim_pending(
            &self,
            _emitter_name: &str,
            _limit: i64,
        ) -> Result<Vec<(WebhookDelivery, WebhookReceipt)>, Self::Error> {
            Ok(Vec::new())
        }

        async fn reclaim_expired(
            &self,
            _emitter_name: &str,
            _lease: Duration,
        ) -> Result<u64, Self::Error> {
            Ok(0)
        }

        async fn mark_emitted(&self, id: DeliveryId) -> Result<(), Self::Error> {
            let mut d = self.deliveries.lock().unwrap();
            if let Some(row) = d.get_mut(&id.0) {
                row.state = DeliveryState::Emitted;
                row.emitted_at = Some(Utc::now());
            }
            Ok(())
        }

        async fn mark_failed(
            &self,
            id: DeliveryId,
            attempt_count: i32,
            next_at: chrono::DateTime<Utc>,
            error: &str,
        ) -> Result<(), Self::Error> {
            let mut d = self.deliveries.lock().unwrap();
            if let Some(row) = d.get_mut(&id.0) {
                row.state = DeliveryState::Failed;
                row.attempt_count = attempt_count;
                row.next_attempt_at = next_at;
                row.last_error = Some(error.to_owned());
            }
            Ok(())
        }

        async fn mark_dead_lettered(&self, id: DeliveryId, error: &str) -> Result<(), Self::Error> {
            let mut d = self.deliveries.lock().unwrap();
            if let Some(row) = d.get_mut(&id.0) {
                row.state = DeliveryState::DeadLettered;
                row.last_error = Some(error.to_owned());
            }
            Ok(())
        }

        async fn count_dlq(&self, _emitter_name: &str) -> Result<u64, Self::Error> {
            Ok(0)
        }

        async fn count_pending(&self, _emitter_name: &str) -> Result<u64, Self::Error> {
            Ok(0)
        }

        async fn count_in_flight(&self, _emitter_name: &str) -> Result<u64, Self::Error> {
            Ok(0)
        }

        async fn insert_replay(
            &self,
            _receipt_id: ReceiptId,
            _emitter_name: &str,
        ) -> Result<DeliveryId, Self::Error> {
            Ok(DeliveryId(Uuid::new_v4()))
        }

        async fn get_delivery(
            &self,
            id: DeliveryId,
        ) -> Result<Option<(WebhookDelivery, WebhookReceipt)>, Self::Error> {
            let deliveries = self.deliveries.lock().unwrap();
            let receipts = self.receipts.lock().unwrap();
            let Some(delivery) = deliveries.get(&id.0).cloned() else {
                return Ok(None);
            };
            let Some(receipt) = receipts.get(&delivery.receipt_id.0).cloned() else {
                return Ok(None);
            };
            Ok(Some((delivery, receipt)))
        }

        async fn get_deliveries_for_receipt(
            &self,
            _receipt_id: ReceiptId,
        ) -> Result<Vec<WebhookDelivery>, Self::Error> {
            Ok(Vec::new())
        }

        async fn list_dlq(
            &self,
            _emitter_filter: Option<&str>,
            _limit: i64,
            _offset: i64,
        ) -> Result<Vec<(WebhookDelivery, WebhookReceipt)>, Self::Error> {
            Ok(Vec::new())
        }
    }

    // ── Helper ───────────────────────────────────────────────────────────────

    fn make_worker_with_storage(
        emitter: Arc<dyn Emitter + Send + Sync>,
        storage: Arc<MemDeliveryStorage>,
        policy: RetryPolicy,
    ) -> EmitterWorker<Arc<MemDeliveryStorage>> {
        EmitterWorker {
            name: "test".to_owned(),
            emitter,
            storage,
            policy,
            concurrency: 1,
            poll_interval: Duration::from_secs(5),
            lease_duration: Duration::from_secs(60),
            health: Arc::new(ArcSwap::from_pointee(EmitterHealth::default())),
        }
    }

    // ── Unit tests ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn dispatch_one_success_marks_emitted() {
        let delivery_id = Uuid::new_v4();
        let receipt_id = Uuid::new_v4();
        let storage = MemDeliveryStorage::with_in_flight(delivery_id, receipt_id, 0);
        let emitter = FakeEmitter::always_ok();
        let policy = RetryPolicy {
            max_attempts: 5,
            ..RetryPolicy::default()
        };
        let worker = make_worker_with_storage(emitter, storage.clone(), policy);

        let delivery = storage.get(delivery_id).expect("seeded delivery");
        let receipt = storage.receipts.lock().unwrap()[&receipt_id].clone();
        worker.dispatch_one(delivery, receipt).await;

        let row = storage.get(delivery_id).expect("row must exist");
        assert_eq!(
            row.state,
            DeliveryState::Emitted,
            "successful emit must set state=emitted"
        );
        assert!(row.emitted_at.is_some(), "emitted_at must be populated");
    }

    #[tokio::test]
    async fn dispatch_one_failure_below_max_marks_failed_with_backoff() {
        let delivery_id = Uuid::new_v4();
        let receipt_id = Uuid::new_v4();
        let storage = MemDeliveryStorage::with_in_flight(delivery_id, receipt_id, 0);
        let emitter = FakeEmitter::always_err();
        let policy = RetryPolicy {
            max_attempts: 5,
            initial_backoff: Duration::from_secs(30),
            jitter: 0.0,
            ..RetryPolicy::default()
        };
        let worker = make_worker_with_storage(emitter, storage.clone(), policy);

        let delivery = storage.get(delivery_id).expect("seeded delivery");
        let receipt = storage.receipts.lock().unwrap()[&receipt_id].clone();
        worker.dispatch_one(delivery, receipt).await;

        let row = storage.get(delivery_id).expect("row must exist");
        assert_eq!(
            row.state,
            DeliveryState::Failed,
            "failure below max must set state=failed"
        );
        assert_eq!(
            row.attempt_count, 1,
            "attempt_count must be incremented to 1"
        );

        let earliest_next = Utc::now() + chrono::Duration::seconds(25);
        let latest_next = Utc::now() + chrono::Duration::seconds(35);
        assert!(
            row.next_attempt_at >= earliest_next && row.next_attempt_at <= latest_next,
            "next_attempt_at must be ≈ now + 30s; got {:?}",
            row.next_attempt_at
        );
    }

    #[tokio::test]
    async fn dispatch_one_failure_at_max_marks_dead_lettered() {
        let delivery_id = Uuid::new_v4();
        let receipt_id = Uuid::new_v4();
        // attempt_count = 4 means next failure (attempt 5) hits max_attempts = 5
        let storage = MemDeliveryStorage::with_in_flight(delivery_id, receipt_id, 4);
        let emitter = FakeEmitter::always_err();
        let policy = RetryPolicy {
            max_attempts: 5,
            ..RetryPolicy::default()
        };
        let worker = make_worker_with_storage(emitter, storage.clone(), policy);

        let delivery = storage.get(delivery_id).expect("seeded delivery");
        let receipt = storage.receipts.lock().unwrap()[&receipt_id].clone();
        worker.dispatch_one(delivery, receipt).await;

        let row = storage.get(delivery_id).expect("row must exist");
        assert_eq!(
            row.state,
            DeliveryState::DeadLettered,
            "failure at max_attempts must set state=dead_lettered"
        );
    }

    #[tokio::test]
    async fn health_state_consecutive_failures_threshold() {
        let health = Arc::new(ArcSwap::from_pointee(EmitterHealth::default()));
        let worker = EmitterWorker {
            name: "test".to_owned(),
            emitter: FakeEmitter::always_ok(),
            storage: MemDeliveryStorage::with_in_flight(Uuid::nil(), Uuid::nil(), 0),
            policy: RetryPolicy::default(),
            concurrency: 1,
            poll_interval: Duration::from_secs(5),
            lease_duration: Duration::from_secs(60),
            health: health.clone(),
        };

        // 9 failures → Degraded (last_failure_at is within 60s)
        for _ in 0..9 {
            worker.update_health(false);
        }
        assert_eq!(
            health.load().status,
            HealthStatus::Degraded,
            "9 failures must be Degraded, not Unhealthy"
        );

        // 10th failure → Unhealthy
        worker.update_health(false);
        assert_eq!(
            health.load().status,
            HealthStatus::Unhealthy,
            "10 consecutive failures must set status=Unhealthy"
        );

        // A success resets consecutive_failures to 0
        worker.update_health(true);
        assert_eq!(
            health.load().consecutive_failures,
            0,
            "success must reset consecutive_failures"
        );
    }
}
