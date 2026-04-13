//! Cucumber world types for hookbox BDD scenario suites.
//!
//! Provides the in-memory pipeline fixtures ([`IngestWorld`]) used by the
//! always-on `core_bdd` test binary, and the `#[cfg(feature = "bdd-server")]`
//! gated [`ServerWorld`] for HTTP-level scenarios.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use cucumber::World;
use http::HeaderMap;
use uuid::Uuid;

use hookbox::dedupe::InMemoryRecentDedupe;
use hookbox::error::StorageError;
use hookbox::pipeline::HookboxPipeline;
use hookbox::state::{
    DeliveryId, DeliveryState, IngestResult, ProcessingState, RetryPolicy, StoreResult,
    VerificationResult, VerificationStatus, WebhookDelivery,
};
use hookbox::traits::{Emitter, SignatureVerifier, Storage};
use hookbox::types::{NormalizedEvent, ReceiptFilter, WebhookReceipt};
use hookbox::{compute_backoff, receipt_aggregate_state};

use crate::fake_emitter::{EmitterBehavior, FakeEmitter};

// ── MemoryStorage ────────────────────────────────────────────────────────

/// In-memory [`Storage`] implementation for scenario tests.
///
/// Supports delivery-row tracking via [`Storage::store_with_deliveries`] override so
/// fan-out BDD scenarios can inspect per-emitter delivery state without a
/// real database.
pub struct MemoryStorage {
    receipts: Mutex<Vec<WebhookReceipt>>,
    deliveries: Mutex<Vec<WebhookDelivery>>,
}

impl std::fmt::Debug for MemoryStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryStorage").finish()
    }
}

impl MemoryStorage {
    /// Create an empty [`MemoryStorage`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            receipts: Mutex::new(Vec::new()),
            deliveries: Mutex::new(Vec::new()),
        }
    }

    /// Clone all delivery rows.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Internal`] if the mutex is poisoned.
    pub fn all_deliveries(&self) -> Result<Vec<WebhookDelivery>, StorageError> {
        self.deliveries
            .lock()
            .map(|g| g.clone())
            .map_err(|e| StorageError::Internal(e.to_string()))
    }

    /// Clone all delivery rows for a given receipt.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Internal`] if the mutex is poisoned.
    pub fn deliveries_for_receipt(
        &self,
        receipt_id: hookbox::state::ReceiptId,
    ) -> Result<Vec<WebhookDelivery>, StorageError> {
        self.deliveries
            .lock()
            .map(|g| {
                g.iter()
                    .filter(|d| d.receipt_id == receipt_id)
                    .cloned()
                    .collect()
            })
            .map_err(|e| StorageError::Internal(e.to_string()))
    }

    /// Apply `mutator` to the delivery row identified by `id`.
    ///
    /// No-ops silently when `id` is not found (mirrors Postgres `UPDATE 0`
    /// semantics — not an error in the BDD harness context).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Internal`] if the mutex is poisoned.
    pub fn update_delivery(
        &self,
        id: DeliveryId,
        mutator: impl FnOnce(&mut WebhookDelivery),
    ) -> Result<(), StorageError> {
        let mut guard = self
            .deliveries
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        if let Some(row) = guard.iter_mut().find(|d| d.delivery_id == id) {
            mutator(row);
        }
        Ok(())
    }

    /// Snapshot of all delivery rows in [`DeliveryState::Pending`] or
    /// [`DeliveryState::Failed`] state whose `next_attempt_at` is due.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Internal`] if the mutex is poisoned.
    pub fn pending_deliveries(&self) -> Result<Vec<WebhookDelivery>, StorageError> {
        let now = Utc::now();
        self.deliveries
            .lock()
            .map(|g| {
                g.iter()
                    .filter(|d| {
                        !d.immutable
                            && matches!(d.state, DeliveryState::Pending | DeliveryState::Failed)
                            && d.next_attempt_at <= now
                    })
                    .cloned()
                    .collect()
            })
            .map_err(|e| StorageError::Internal(e.to_string()))
    }
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Storage for MemoryStorage {
    async fn store(&self, receipt: &WebhookReceipt) -> Result<StoreResult, StorageError> {
        let mut receipts = self
            .receipts
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        if let Some(existing) = receipts.iter().find(|r| r.dedupe_key == receipt.dedupe_key) {
            return Ok(StoreResult::Duplicate {
                existing_id: existing.receipt_id,
            });
        }
        receipts.push(receipt.clone());
        Ok(StoreResult::Stored)
    }

    async fn get(&self, id: Uuid) -> Result<Option<WebhookReceipt>, StorageError> {
        let receipts = self
            .receipts
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(receipts.iter().find(|r| r.receipt_id.0 == id).cloned())
    }

    async fn update_state(
        &self,
        id: Uuid,
        state: ProcessingState,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        let mut receipts = self
            .receipts
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        if let Some(receipt) = receipts.iter_mut().find(|r| r.receipt_id.0 == id) {
            receipt.processing_state = state;
            receipt.last_error = error.map(String::from);
        }
        Ok(())
    }

    async fn query(&self, _filter: ReceiptFilter) -> Result<Vec<WebhookReceipt>, StorageError> {
        let receipts = self
            .receipts
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(receipts.clone())
    }

    /// Override the default: insert a delivery row per emitter name atomically
    /// alongside the receipt. Dedup short-circuits before any delivery rows
    /// are inserted — matching real Postgres transactional semantics.
    async fn store_with_deliveries(
        &self,
        receipt: &WebhookReceipt,
        emitter_names: &[String],
    ) -> Result<StoreResult, StorageError> {
        // Authoritative dedupe check first.
        let store_result = self.store(receipt).await?;
        if matches!(store_result, StoreResult::Duplicate { .. }) {
            return Ok(store_result);
        }
        // Insert one pending delivery row per emitter.
        let now = Utc::now();
        let mut deliveries = self
            .deliveries
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        for name in emitter_names {
            deliveries.push(WebhookDelivery {
                delivery_id: DeliveryId::new(),
                receipt_id: receipt.receipt_id,
                emitter_name: name.clone(),
                state: DeliveryState::Pending,
                attempt_count: 0,
                last_error: None,
                last_attempt_at: None,
                next_attempt_at: now,
                emitted_at: None,
                immutable: false,
                created_at: now,
            });
        }
        Ok(StoreResult::Stored)
    }
}

// ── SharedMemoryStorage ─────────────────────────────────────────────────
//
// Newtype wrapper around `Arc<MemoryStorage>` that allows implementing the
// `Storage` trait without violating the orphan rule (which prohibits
// `impl Storage for Arc<MemoryStorage>` since `Arc` is defined in std).

/// Clonable, shared handle to a [`MemoryStorage`] instance.
///
/// Used by [`IngestWorld`] so the pipeline and the BDD harness share the same
/// backing storage without violating Rust's orphan rule.
#[derive(Clone, Debug)]
pub struct SharedMemoryStorage(pub Arc<MemoryStorage>);

impl SharedMemoryStorage {
    /// Create a new shared storage from a raw [`MemoryStorage`].
    pub fn new(inner: MemoryStorage) -> Self {
        Self(Arc::new(inner))
    }
}

#[async_trait]
impl Storage for SharedMemoryStorage {
    async fn store(&self, receipt: &WebhookReceipt) -> Result<StoreResult, StorageError> {
        self.0.store(receipt).await
    }

    async fn get(&self, id: Uuid) -> Result<Option<WebhookReceipt>, StorageError> {
        self.0.get(id).await
    }

    async fn update_state(
        &self,
        id: Uuid,
        state: ProcessingState,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        self.0.update_state(id, state, error).await
    }

    async fn query(&self, filter: ReceiptFilter) -> Result<Vec<WebhookReceipt>, StorageError> {
        self.0.query(filter).await
    }

    async fn store_with_deliveries(
        &self,
        receipt: &WebhookReceipt,
        emitter_names: &[String],
    ) -> Result<StoreResult, StorageError> {
        self.0.store_with_deliveries(receipt, emitter_names).await
    }
}

// ── Verifiers ────────────────────────────────────────────────────────────

/// Signature verifier that always reports a passing (verified) result.
pub struct PassVerifier {
    /// Provider name this verifier is registered for.
    pub provider: String,
}

#[async_trait]
impl SignatureVerifier for PassVerifier {
    fn provider_name(&self) -> &str {
        &self.provider
    }

    async fn verify(&self, _headers: &HeaderMap, _body: &[u8]) -> VerificationResult {
        VerificationResult {
            status: VerificationStatus::Verified,
            reason: Some("signature_valid".to_owned()),
        }
    }
}

/// Signature verifier that always reports a failing result.
pub struct FailVerifier {
    /// Provider name this verifier is registered for.
    pub provider: String,
}

#[async_trait]
impl SignatureVerifier for FailVerifier {
    fn provider_name(&self) -> &str {
        &self.provider
    }

    async fn verify(&self, _headers: &HeaderMap, _body: &[u8]) -> VerificationResult {
        VerificationResult {
            status: VerificationStatus::Failed,
            reason: Some("invalid_signature".to_owned()),
        }
    }
}

// ── IngestWorld ──────────────────────────────────────────────────────────

/// Type alias for the concrete pipeline type used in BDD scenarios.
///
/// Uses [`SharedMemoryStorage`] so the world can retain a `storage_handle`
/// for post-ingest assertions on delivery rows while the pipeline owns the
/// same backing `MemoryStorage` via an `Arc`.
pub type ScenarioPipeline = HookboxPipeline<SharedMemoryStorage, InMemoryRecentDedupe>;

/// Type-erased pipeline box so [`IngestWorld`] can derive [`std::fmt::Debug`].
struct PipelineBox(Box<dyn std::any::Any + Send>);

impl std::fmt::Debug for PipelineBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineBox").finish()
    }
}

/// Cucumber [`World`] for in-memory, no-database BDD scenarios.
///
/// Holds a [`ScenarioPipeline`] and accumulates [`IngestResult`]s across
/// scenario steps for assertion.
#[derive(Debug, Default, World)]
#[world(init = Self::new)]
pub struct IngestWorld {
    /// The pipeline under test (wrapped for dyn-compatibility with `Debug`).
    pipeline: Option<PipelineBox>,
    /// Results from each `ingest` call in the current scenario.
    pub results: Vec<IngestResult>,
    /// Registry of fake emitters keyed by logical name, so step definitions can
    /// assert per-emitter received counts after fan-out.
    pub emitters: BTreeMap<String, Arc<FakeEmitter>>,
    /// Shared handle to the in-memory storage so step defs can inspect delivery
    /// rows without going through the pipeline.
    pub storage_handle: Option<SharedMemoryStorage>,
    /// Scratch retry policy for backoff arithmetic scenarios.
    pub scratch_retry_policy: Option<RetryPolicy>,
    /// Scratch attempt counter for backoff arithmetic scenarios.
    pub scratch_attempt: i32,
}

impl IngestWorld {
    /// Create a fresh [`IngestWorld`] with no pipeline and no results.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pipeline: None,
            results: Vec::new(),
            emitters: BTreeMap::new(),
            storage_handle: None,
            scratch_retry_policy: None,
            scratch_attempt: 0,
        }
    }

    /// Install a [`ScenarioPipeline`] into the world.
    pub fn set_pipeline(&mut self, pipeline: ScenarioPipeline) {
        self.pipeline = Some(PipelineBox(Box::new(pipeline)));
    }

    /// Register a fake emitter under `name` so step definitions can look it up.
    pub fn register_emitter(&mut self, name: impl Into<String>, emitter: Arc<FakeEmitter>) {
        self.emitters.insert(name.into(), emitter);
    }

    /// Look up a registered fake emitter by name.
    #[must_use]
    pub fn emitter(&self, name: &str) -> Option<&Arc<FakeEmitter>> {
        self.emitters.get(name)
    }

    /// Borrow the installed pipeline.
    ///
    /// # Panics
    ///
    /// Panics if no pipeline has been installed via [`Self::set_pipeline`].
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "panics by design when called before set_pipeline"
    )]
    pub fn pipeline(&self) -> &ScenarioPipeline {
        self.pipeline
            .as_ref()
            .and_then(|b| b.0.downcast_ref::<ScenarioPipeline>())
            .expect("pipeline must be set before accessing it in a step")
    }

    /// Convenience: ingest a webhook and push the result.
    ///
    /// # Errors
    ///
    /// Propagates any [`hookbox::error::IngestError`] returned by the pipeline.
    ///
    /// # Panics
    ///
    /// Never panics in practice — the `expect` guards a `Vec::last()` call
    /// immediately after a `push`, so the vector is guaranteed non-empty.
    #[expect(clippy::expect_used, reason = "Vec::last after push is infallible")]
    pub async fn ingest(
        &mut self,
        provider: &str,
        body: impl Into<Bytes>,
    ) -> Result<&IngestResult, hookbox::error::IngestError> {
        let result = self
            .pipeline()
            .ingest(provider, HeaderMap::new(), body.into())
            .await?;
        self.results.push(result);
        Ok(self.results.last().expect("just pushed"))
    }

    /// Build a pipeline pre-configured with healthy fake emitters for each
    /// name in `names`. Stashes a `storage_handle` so step definitions can
    /// inspect delivery rows after ingestion.
    pub fn build_pipeline_with_emitters(&mut self, names: &[String]) {
        let storage = SharedMemoryStorage::new(MemoryStorage::new());
        self.storage_handle = Some(storage.clone());

        for name in names {
            let emitter = Arc::new(FakeEmitter::new(name.clone(), EmitterBehavior::Healthy));
            self.emitters.insert(name.clone(), emitter);
        }

        let pipeline = HookboxPipeline::builder()
            .storage(storage)
            .dedupe(InMemoryRecentDedupe::new(100))
            .emitter_names(names.to_vec())
            .verifier(PassVerifier {
                provider: "stripe".to_owned(),
            })
            .build();
        self.set_pipeline(pipeline);
    }

    /// Change the behavior of a named emitter.
    ///
    /// # Panics
    ///
    /// Panics if the emitter name is not registered or the mutex is poisoned.
    #[expect(
        clippy::expect_used,
        reason = "panics by design when emitter is not registered or mutex is poisoned"
    )]
    pub fn set_emitter_behavior(&self, name: &str, behavior: EmitterBehavior) {
        self.emitters
            .get(name)
            .expect("unknown emitter")
            .set_behavior(behavior)
            .expect("mutex poisoned");
    }

    /// Clone delivery rows for the most-recently-ingested receipt.
    ///
    /// Returns an empty vec if no receipt has been ingested yet.
    ///
    /// # Panics
    ///
    /// Panics if `storage_handle` has not been set.
    #[expect(
        clippy::expect_used,
        reason = "panics by design when storage_handle is not set"
    )]
    #[must_use]
    pub fn last_receipt_deliveries(&self) -> Vec<WebhookDelivery> {
        let storage = self
            .storage_handle
            .as_ref()
            .expect("storage_handle must be set before calling last_receipt_deliveries");

        // Find the most-recent accepted receipt from `self.results`.
        let last_accepted = self.results.iter().rev().find_map(|r| {
            if let IngestResult::Accepted { receipt_id, .. } = r {
                Some(*receipt_id)
            } else {
                None
            }
        });

        let Some(receipt_id) = last_accepted else {
            return Vec::new();
        };

        storage
            .0
            .deliveries_for_receipt(receipt_id)
            .expect("mutex poisoned")
    }

    /// Compute the derived [`ProcessingState`] for the last-ingested receipt
    /// by aggregating its delivery rows.
    ///
    /// Returns [`ProcessingState::Stored`] when no deliveries exist yet.
    ///
    /// # Panics
    ///
    /// Panics if `storage_handle` has not been set.
    #[must_use]
    pub fn last_receipt_state(&self) -> ProcessingState {
        let deliveries = self.last_receipt_deliveries();
        receipt_aggregate_state(&deliveries, ProcessingState::Stored)
    }

    /// Drive all pending / failed deliveries to completion (or exhaustion)
    /// using the registered fake emitters.
    ///
    /// Iterates until no pending-or-failed rows remain, or for at most 50
    /// rounds to guard against infinite loops with `AlwaysFail` emitters.
    ///
    /// # Panics
    ///
    /// Panics if `storage_handle` has not been set or if required emitters
    /// are missing from the registry.
    #[expect(
        clippy::expect_used,
        reason = "panics by design in test harness when required objects are missing"
    )]
    pub async fn run_deliveries_to_completion(&self) {
        let shared = self
            .storage_handle
            .as_ref()
            .expect("storage_handle must be set");
        let inner = &shared.0;
        let policy = RetryPolicy::default();

        for _round in 0..50 {
            let pending = inner
                .pending_deliveries()
                .expect("mutex poisoned — pending_deliveries");
            if pending.is_empty() {
                break;
            }

            for delivery in pending {
                // Find the receipt so we can build the NormalizedEvent.
                let maybe_receipt = inner
                    .get(delivery.receipt_id.0)
                    .await
                    .expect("storage get failed");
                let Some(receipt) = maybe_receipt else {
                    continue;
                };
                let event = NormalizedEvent::from_receipt(&receipt);

                let emitter = self
                    .emitters
                    .get(&delivery.emitter_name)
                    .expect("emitter not registered");

                match emitter.emit(&event).await {
                    Ok(()) => {
                        inner
                            .update_delivery(delivery.delivery_id, |row| {
                                row.state = DeliveryState::Emitted;
                                row.attempt_count += 1;
                                row.last_attempt_at = Some(Utc::now());
                                row.emitted_at = Some(Utc::now());
                                // Do NOT set immutable here — `latest_mutable_per_emitter`
                                // skips immutable rows, which would cause
                                // `receipt_aggregate_state` to fall back to `Stored`.
                            })
                            .expect("update_delivery failed");
                    }
                    Err(err) => {
                        let err_str = err.to_string();
                        let new_attempt = delivery.attempt_count + 1;
                        if new_attempt >= policy.max_attempts {
                            inner
                                .update_delivery(delivery.delivery_id, |row| {
                                    row.state = DeliveryState::DeadLettered;
                                    row.attempt_count = new_attempt;
                                    row.last_attempt_at = Some(Utc::now());
                                    row.last_error = Some(err_str.clone());
                                    // Do NOT set immutable — `latest_mutable_per_emitter`
                                    // skips immutable rows, so keeping them mutable
                                    // allows `receipt_aggregate_state` to observe
                                    // the DeadLettered state.
                                })
                                .expect("update_delivery failed");
                        } else {
                            let backoff = compute_backoff(new_attempt, &policy);
                            inner
                                .update_delivery(delivery.delivery_id, |row| {
                                    row.state = DeliveryState::Failed;
                                    row.attempt_count = new_attempt;
                                    row.last_attempt_at = Some(Utc::now());
                                    row.last_error = Some(err_str.clone());
                                    row.next_attempt_at = Utc::now() + backoff;
                                })
                                .expect("update_delivery failed");
                        }
                    }
                }
            }
        }
    }

    /// Like [`Self::run_deliveries_to_completion`] but keeps re-running rounds until
    /// ALL rows are in a terminal state (`Emitted` or `DeadLettered`), or the
    /// iteration budget is hit.
    ///
    /// Used by the "workers run until all retries exhausted" step.
    ///
    /// # Panics
    ///
    /// Panics if `storage_handle` has not been set.
    #[expect(
        clippy::expect_used,
        reason = "panics by design in test harness when required objects are missing"
    )]
    pub async fn run_until_exhausted(&self) {
        let shared = self
            .storage_handle
            .as_ref()
            .expect("storage_handle must be set");
        let inner = &shared.0;
        let policy = RetryPolicy::default();

        for _round in 0..200 {
            // Check if all deliveries are terminal.
            let all = inner
                .all_deliveries()
                .expect("mutex poisoned — all_deliveries");
            let any_active = all.iter().any(|d| {
                !matches!(
                    d.state,
                    DeliveryState::Emitted | DeliveryState::DeadLettered
                )
            });
            if !any_active {
                break;
            }

            // Advance any pending/failed rows whose next_attempt_at has passed.
            // For exhaustion scenarios we force next_attempt_at to now so the
            // loop makes progress regardless of the computed backoff.
            let due = {
                let mut guard = inner
                    .deliveries
                    .lock()
                    .expect("mutex poisoned — deliveries lock");
                // Make all pending/failed rows immediately due.
                for row in guard.iter_mut() {
                    if !row.immutable
                        && matches!(row.state, DeliveryState::Pending | DeliveryState::Failed)
                    {
                        row.next_attempt_at = Utc::now();
                    }
                }
                guard
                    .iter()
                    .filter(|d| {
                        !d.immutable
                            && matches!(d.state, DeliveryState::Pending | DeliveryState::Failed)
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            };

            if due.is_empty() {
                break;
            }

            for delivery in due {
                let maybe_receipt = inner
                    .get(delivery.receipt_id.0)
                    .await
                    .expect("storage get failed");
                let Some(receipt) = maybe_receipt else {
                    continue;
                };
                let event = NormalizedEvent::from_receipt(&receipt);

                let emitter = self
                    .emitters
                    .get(&delivery.emitter_name)
                    .expect("emitter not registered");

                match emitter.emit(&event).await {
                    Ok(()) => {
                        inner
                            .update_delivery(delivery.delivery_id, |row| {
                                row.state = DeliveryState::Emitted;
                                row.attempt_count += 1;
                                row.last_attempt_at = Some(Utc::now());
                                row.emitted_at = Some(Utc::now());
                                // Do NOT set immutable here — see note in
                                // `run_deliveries_to_completion`.
                            })
                            .expect("update_delivery failed");
                    }
                    Err(err) => {
                        let err_str = err.to_string();
                        let new_attempt = delivery.attempt_count + 1;
                        if new_attempt >= policy.max_attempts {
                            inner
                                .update_delivery(delivery.delivery_id, |row| {
                                    row.state = DeliveryState::DeadLettered;
                                    row.attempt_count = new_attempt;
                                    row.last_attempt_at = Some(Utc::now());
                                    row.last_error = Some(err_str.clone());
                                    // Do NOT set immutable — see note in
                                    // `run_deliveries_to_completion`.
                                })
                                .expect("update_delivery failed");
                        } else {
                            let backoff = compute_backoff(new_attempt, &policy);
                            inner
                                .update_delivery(delivery.delivery_id, |row| {
                                    row.state = DeliveryState::Failed;
                                    row.attempt_count = new_attempt;
                                    row.last_attempt_at = Some(Utc::now());
                                    row.last_error = Some(err_str.clone());
                                    row.next_attempt_at = Utc::now() + backoff;
                                })
                                .expect("update_delivery failed");
                        }
                    }
                }
            }
        }
    }
}

// ── ServerWorld ──────────────────────────────────────────────────────────

#[cfg(feature = "bdd-server")]
pub use server_world::ServerWorld;

#[cfg(feature = "bdd-server")]
mod server_world {
    use cucumber::World;

    /// Cucumber [`World`] for full-stack HTTP BDD scenarios.
    ///
    /// Manages a live `hookbox-server` instance backed by a testcontainer
    /// Postgres database and a `reqwest` HTTP client for exercising the
    /// `/webhook/:provider` endpoint end-to-end.
    #[derive(Debug, Default, World)]
    #[world(init = Self::new)]
    pub struct ServerWorld {
        /// Base URL of the running server (e.g. `http://127.0.0.1:PORT`).
        pub base_url: Option<String>,
    }

    impl ServerWorld {
        /// Create a fresh [`ServerWorld`].
        #[must_use]
        pub fn new() -> Self {
            Self { base_url: None }
        }
    }
}
