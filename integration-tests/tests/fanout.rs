//! Integration tests for the emitter fan-out pipeline.
//!
//! Run with: `cargo nextest run -p hookbox-integration-tests -- fanout`

#![expect(
    clippy::expect_used,
    reason = "expect/unwrap/panic are acceptable in test code"
)]
#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use hookbox::HookboxPipeline;
use hookbox::dedupe::InMemoryRecentDedupe;
use hookbox::error::EmitError;
use hookbox::receipt_aggregate_state;
use hookbox::state::{DeliveryId, DeliveryState, ProcessingState, RetryPolicy};
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;
use hookbox_postgres::{DeliveryStorage, PostgresStorage};
use hookbox_server::worker::{EmitterHealth, EmitterWorker};
use http::HeaderMap;
use sqlx::PgPool;
use uuid::Uuid;

// ── Test helpers ───────────────────────────────────────────────────────────────

/// Emitter that sends every event to a channel. Used to verify receipt.
struct ChannelEmitter(tokio::sync::mpsc::UnboundedSender<NormalizedEvent>);

#[async_trait]
impl Emitter for ChannelEmitter {
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        self.0
            .send(event.clone())
            .map_err(|_| EmitError::Downstream("channel closed".to_owned()))?;
        Ok(())
    }
}

/// Emitter that always fails. Used to exercise retry / dead-letter paths.
struct AlwaysFailEmitter;

#[async_trait]
impl Emitter for AlwaysFailEmitter {
    async fn emit(&self, _event: &NormalizedEvent) -> Result<(), EmitError> {
        Err(EmitError::Downstream("injected failure".to_owned()))
    }
}

/// Construct an `EmitterWorker` with sensible test defaults.
fn make_worker(
    storage: PostgresStorage,
    emitter: Arc<dyn Emitter + Send + Sync>,
    name: &str,
    policy: RetryPolicy,
    concurrency: usize,
) -> EmitterWorker<PostgresStorage> {
    EmitterWorker {
        name: name.to_owned(),
        emitter,
        storage,
        policy,
        concurrency,
        poll_interval: Duration::from_millis(50),
        lease_duration: Duration::from_secs(60),
        health: Arc::new(arc_swap::ArcSwap::from_pointee(EmitterHealth::default())),
    }
}

/// Drive a worker until `claim_pending` returns empty (or `max_cycles` reached).
async fn drive_to_terminal(worker: &EmitterWorker<PostgresStorage>, max_cycles: usize) {
    for _ in 0..max_cycles {
        let _ = worker
            .storage
            .reclaim_expired(&worker.name, worker.lease_duration)
            .await;
        #[expect(clippy::cast_possible_wrap, reason = "concurrency is small in tests")]
        let rows = worker
            .storage
            .claim_pending(&worker.name, worker.concurrency as i64)
            .await
            .expect("claim_pending");
        if rows.is_empty() {
            break;
        }
        futures::future::join_all(rows.into_iter().map(|(d, r)| worker.dispatch_one(d, r))).await;
    }
}

/// Seed a `webhook_receipts` row (verified / stored state).
async fn seed_receipt(pool: &PgPool, receipt_id: Uuid) {
    sqlx::query(
        "INSERT INTO webhook_receipts (
            receipt_id, provider_name, dedupe_key, payload_hash, raw_body,
            raw_headers, verification_status, processing_state, emit_count,
            received_at, metadata
        ) VALUES ($1, 'test', $2, 'h', $3, '{}'::jsonb, 'verified', 'stored', 0, now(), '{}'::jsonb)",
    )
    .bind(receipt_id)
    .bind(format!("test:{receipt_id}"))
    .bind(b"{}".as_slice())
    .execute(pool)
    .await
    .expect("seed receipt");
}

#[expect(
    clippy::too_many_arguments,
    reason = "seed helper needs all delivery fields"
)]
async fn seed_delivery(
    pool: &PgPool,
    delivery_id: Uuid,
    receipt_id: Uuid,
    emitter: &str,
    state: &str,
    immutable: bool,
    next_attempt_at: chrono::DateTime<chrono::Utc>,
    last_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
) {
    sqlx::query(
        "INSERT INTO webhook_deliveries (
            delivery_id, receipt_id, emitter_name, state, attempt_count,
            next_attempt_at, last_attempt_at, immutable, created_at
        ) VALUES ($1, $2, $3, $4, 0, $5, $6, $7, now())",
    )
    .bind(delivery_id)
    .bind(receipt_id)
    .bind(emitter)
    .bind(state)
    .bind(next_attempt_at)
    .bind(last_attempt_at)
    .bind(immutable)
    .execute(pool)
    .await
    .expect("seed delivery");
}

/// Ingest a single unique event via the pipeline and return the receipt UUID.
async fn ingest_one(pool: &PgPool, emitter_names: Vec<String>) -> Uuid {
    let storage = PostgresStorage::new(pool.clone());
    let pipeline = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter_names(emitter_names)
        .build();

    let unique_body = Bytes::from(format!(
        r#"{{"event":"payment.completed","id":"{}"}}"#,
        Uuid::new_v4()
    ));
    let result = pipeline
        .ingest("test", HeaderMap::new(), unique_body)
        .await
        .expect("ingest should not error");

    let hookbox::IngestResult::Accepted { receipt_id } = result else {
        unreachable!("expected Accepted result")
    };
    receipt_id.0
}

// ── Tests ──────────────────────────────────────────────────────────────────────

/// Test 1: Ingesting via a two-emitter pipeline creates one delivery row per
/// emitter, and each worker dispatches exactly one event to its channel.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn test_fan_out_two_emitters(pool: PgPool) {
    let receipt_id = ingest_one(&pool, vec!["chan-a".to_owned(), "chan-b".to_owned()]).await;
    let storage = PostgresStorage::new(pool.clone());

    // Two delivery rows should exist — one per emitter.
    let deliveries = storage
        .get_deliveries_for_receipt(hookbox::state::ReceiptId(receipt_id))
        .await
        .expect("get_deliveries_for_receipt");
    assert_eq!(deliveries.len(), 2, "expected one delivery row per emitter");

    let emitter_names: HashSet<&str> = deliveries.iter().map(|d| d.emitter_name.as_str()).collect();
    assert!(emitter_names.contains("chan-a"), "missing chan-a delivery");
    assert!(emitter_names.contains("chan-b"), "missing chan-b delivery");

    // Drive chan-a.
    let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel();
    let worker_a = make_worker(
        PostgresStorage::new(pool.clone()),
        Arc::new(ChannelEmitter(tx_a)),
        "chan-a",
        RetryPolicy::default(),
        1,
    );
    drive_to_terminal(&worker_a, 10).await;

    // Drive chan-b.
    let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel();
    let worker_b = make_worker(
        PostgresStorage::new(pool.clone()),
        Arc::new(ChannelEmitter(tx_b)),
        "chan-b",
        RetryPolicy::default(),
        1,
    );
    drive_to_terminal(&worker_b, 10).await;

    // Both channels must have received exactly one event with the matching receipt_id.
    let evt_a = rx_a.try_recv().expect("chan-a must have received an event");
    assert_eq!(
        evt_a.receipt_id.0, receipt_id,
        "chan-a event has wrong receipt_id"
    );
    assert!(
        rx_a.try_recv().is_err(),
        "chan-a must have exactly one event"
    );

    let evt_b = rx_b.try_recv().expect("chan-b must have received an event");
    assert_eq!(
        evt_b.receipt_id.0, receipt_id,
        "chan-b event has wrong receipt_id"
    );
    assert!(
        rx_b.try_recv().is_err(),
        "chan-b must have exactly one event"
    );
}

/// Test 2: One emitter succeeds; the other always fails and eventually
/// dead-letters after `max_attempts` exhaustion. The derived aggregate state
/// is `DeadLettered`.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn test_one_emitter_fails_other_succeeds(pool: PgPool) {
    let receipt_id = ingest_one(&pool, vec!["chan-ok".to_owned(), "always-fail".to_owned()]).await;
    let storage = PostgresStorage::new(pool.clone());

    // Drive chan-ok (succeeds immediately).
    let (tx_ok, _rx_ok) = tokio::sync::mpsc::unbounded_channel();
    let worker_ok = make_worker(
        PostgresStorage::new(pool.clone()),
        Arc::new(ChannelEmitter(tx_ok)),
        "chan-ok",
        RetryPolicy::default(),
        1,
    );
    drive_to_terminal(&worker_ok, 10).await;

    // Drive always-fail with zero backoff so retries are immediately eligible.
    let fail_policy = RetryPolicy {
        max_attempts: 2,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        jitter: 0.0,
        ..RetryPolicy::default()
    };
    let worker_fail = make_worker(
        PostgresStorage::new(pool.clone()),
        Arc::new(AlwaysFailEmitter),
        "always-fail",
        fail_policy,
        1,
    );
    // max_attempts=2 means attempt_count 0→1 (Failed), then 1→2 (DeadLettered).
    drive_to_terminal(&worker_fail, 20).await;

    // Verify final delivery states.
    let deliveries = storage
        .get_deliveries_for_receipt(hookbox::state::ReceiptId(receipt_id))
        .await
        .expect("get_deliveries_for_receipt");
    assert_eq!(deliveries.len(), 2, "expected two delivery rows");

    let ok_row = deliveries
        .iter()
        .find(|d| d.emitter_name == "chan-ok")
        .expect("chan-ok delivery must exist");
    assert_eq!(
        ok_row.state,
        DeliveryState::Emitted,
        "chan-ok must be Emitted"
    );

    let fail_row = deliveries
        .iter()
        .find(|d| d.emitter_name == "always-fail")
        .expect("always-fail delivery must exist");
    assert_eq!(
        fail_row.state,
        DeliveryState::DeadLettered,
        "always-fail must be DeadLettered"
    );

    // Aggregate derived state from the delivery rows.
    let aggregate = receipt_aggregate_state(&deliveries, ProcessingState::Stored);
    assert_eq!(
        aggregate,
        ProcessingState::DeadLettered,
        "derived aggregate state must be DeadLettered"
    );
}

/// Test 3: After scenario 2 reaches terminal state, replaying chan-ok creates
/// a fresh Pending row while always-fail's `DeadLettered` row is unchanged.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn test_replay_one_emitter_only(pool: PgPool) {
    // Re-run scenario 2 to terminal.
    let receipt_id = ingest_one(&pool, vec!["chan-ok".to_owned(), "always-fail".to_owned()]).await;
    let storage = PostgresStorage::new(pool.clone());

    let (tx_ok, _rx_ok) = tokio::sync::mpsc::unbounded_channel();
    let worker_ok = make_worker(
        PostgresStorage::new(pool.clone()),
        Arc::new(ChannelEmitter(tx_ok)),
        "chan-ok",
        RetryPolicy::default(),
        1,
    );
    drive_to_terminal(&worker_ok, 10).await;

    let fail_policy = RetryPolicy {
        max_attempts: 2,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        jitter: 0.0,
        ..RetryPolicy::default()
    };
    let worker_fail = make_worker(
        PostgresStorage::new(pool.clone()),
        Arc::new(AlwaysFailEmitter),
        "always-fail",
        fail_policy,
        1,
    );
    drive_to_terminal(&worker_fail, 20).await;

    // Replay chan-ok only.
    storage
        .insert_replay(hookbox::state::ReceiptId(receipt_id), "chan-ok")
        .await
        .expect("insert_replay");

    let deliveries = storage
        .get_deliveries_for_receipt(hookbox::state::ReceiptId(receipt_id))
        .await
        .expect("get_deliveries_for_receipt");

    let ok_rows: Vec<_> = deliveries
        .iter()
        .filter(|d| d.emitter_name == "chan-ok")
        .collect();
    assert_eq!(
        ok_rows.len(),
        2,
        "chan-ok must have 2 rows (original + replay)"
    );

    let has_emitted = ok_rows.iter().any(|d| d.state == DeliveryState::Emitted);
    let has_pending = ok_rows.iter().any(|d| d.state == DeliveryState::Pending);
    assert!(has_emitted, "chan-ok must have original Emitted row");
    assert!(has_pending, "chan-ok must have new Pending replay row");

    let fail_rows: Vec<_> = deliveries
        .iter()
        .filter(|d| d.emitter_name == "always-fail")
        .collect();
    assert_eq!(
        fail_rows.len(),
        1,
        "always-fail must still have exactly 1 row"
    );
    assert_eq!(
        fail_rows[0].state,
        DeliveryState::DeadLettered,
        "always-fail row must remain DeadLettered"
    );
}

/// Test 4: 100 pending deliveries are dispatched concurrently (concurrency=4)
/// with no double-emits. The channel receives exactly 100 events and no rows
/// remain in-flight afterwards.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn test_concurrent_dispatch_no_double_emit(pool: PgPool) {
    use chrono::Utc;

    let storage = PostgresStorage::new(pool.clone());
    let receipt_id = Uuid::new_v4();
    seed_receipt(&pool, receipt_id).await;

    // Seed 100 eligible pending delivery rows (all share one receipt).
    for _ in 0..100 {
        let delivery_id = Uuid::new_v4();
        seed_delivery(
            &pool,
            delivery_id,
            receipt_id,
            "bulk-emitter",
            "pending",
            false,
            Utc::now() - chrono::Duration::seconds(1),
            None,
        )
        .await;
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<NormalizedEvent>();
    let worker = make_worker(
        PostgresStorage::new(pool.clone()),
        Arc::new(ChannelEmitter(tx)),
        "bulk-emitter",
        RetryPolicy::default(),
        4,
    );
    drive_to_terminal(&worker, 50).await;

    // Count events received — each dispatch sends exactly one.
    let mut count = 0usize;
    while rx.try_recv().is_ok() {
        count += 1;
    }
    assert_eq!(
        count, 100,
        "channel must receive exactly 100 events (no double-emits)"
    );

    // No rows should remain in-flight.
    let in_flight = storage
        .count_in_flight("bulk-emitter")
        .await
        .expect("count_in_flight");
    assert_eq!(in_flight, 0, "no rows should remain in-flight");
}

/// Test 5: After a replay, the old `DeadLettered` history row must NOT poison
/// the derived state once the new replay row succeeds.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn test_replay_history_does_not_poison_state(pool: PgPool) {
    // Run ok + broken to terminal.
    let receipt_id = ingest_one(&pool, vec!["ok".to_owned(), "broken".to_owned()]).await;
    let storage = PostgresStorage::new(pool.clone());

    // ok succeeds.
    let (tx_ok, _rx_ok) = tokio::sync::mpsc::unbounded_channel();
    let worker_ok = make_worker(
        PostgresStorage::new(pool.clone()),
        Arc::new(ChannelEmitter(tx_ok)),
        "ok",
        RetryPolicy::default(),
        1,
    );
    drive_to_terminal(&worker_ok, 10).await;

    // broken dead-letters.
    let fail_policy = RetryPolicy {
        max_attempts: 2,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        jitter: 0.0,
        ..RetryPolicy::default()
    };
    let worker_broken = make_worker(
        PostgresStorage::new(pool.clone()),
        Arc::new(AlwaysFailEmitter),
        "broken",
        fail_policy,
        1,
    );
    drive_to_terminal(&worker_broken, 20).await;

    // Verify aggregate is DeadLettered before replay.
    let deliveries_before = storage
        .get_deliveries_for_receipt(hookbox::state::ReceiptId(receipt_id))
        .await
        .expect("get_deliveries_for_receipt");
    assert_eq!(
        receipt_aggregate_state(&deliveries_before, ProcessingState::Stored),
        ProcessingState::DeadLettered,
        "pre-replay aggregate must be DeadLettered"
    );

    // Replay broken — this time with a working emitter.
    storage
        .insert_replay(hookbox::state::ReceiptId(receipt_id), "broken")
        .await
        .expect("insert_replay");

    let (tx_fixed, _rx_fixed) = tokio::sync::mpsc::unbounded_channel();
    let worker_fixed = make_worker(
        PostgresStorage::new(pool.clone()),
        Arc::new(ChannelEmitter(tx_fixed)),
        "broken",
        RetryPolicy::default(),
        1,
    );
    drive_to_terminal(&worker_fixed, 10).await;

    // The new replay row must be Emitted.
    let deliveries_after = storage
        .get_deliveries_for_receipt(hookbox::state::ReceiptId(receipt_id))
        .await
        .expect("get_deliveries_for_receipt");

    let broken_rows: Vec<_> = deliveries_after
        .iter()
        .filter(|d| d.emitter_name == "broken")
        .collect();
    assert_eq!(
        broken_rows.len(),
        2,
        "broken must have 2 rows (original DLQ + replay)"
    );

    let replay_row = broken_rows
        .iter()
        .find(|d| d.state == DeliveryState::Emitted)
        .expect("replay row must be Emitted");
    assert_eq!(replay_row.state, DeliveryState::Emitted);

    // Aggregate over ALL rows (including old DeadLettered) must now be Emitted.
    let aggregate = receipt_aggregate_state(&deliveries_after, ProcessingState::Stored);
    assert_eq!(
        aggregate,
        ProcessingState::Emitted,
        "old DeadLettered row must NOT poison derived state after successful replay"
    );
}

/// Test 6: Pending and `InFlight` delivery rows both collapse to the `Stored`
/// derived aggregate state; only `Emitted` elevates the aggregate to `Emitted`.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn test_aggregate_state_pending_in_flight_collapse_to_stored(pool: PgPool) {
    let receipt_id = ingest_one(&pool, vec!["slow".to_owned()]).await;
    let storage = PostgresStorage::new(pool.clone());

    // Step 1: row is Pending → derived state is Stored.
    let deliveries = storage
        .get_deliveries_for_receipt(hookbox::state::ReceiptId(receipt_id))
        .await
        .expect("get_deliveries_for_receipt");
    assert_eq!(deliveries.len(), 1, "expected exactly one delivery row");
    assert_eq!(deliveries[0].state, DeliveryState::Pending);
    assert_eq!(
        receipt_aggregate_state(&deliveries, ProcessingState::Stored),
        ProcessingState::Stored,
        "Pending delivery must derive Stored"
    );

    // Step 2: claim the row (transitions to InFlight) → derived state still Stored.
    let claimed = storage
        .claim_pending("slow", 1)
        .await
        .expect("claim_pending");
    assert_eq!(claimed.len(), 1, "must claim one row");
    let (in_flight_delivery, _receipt) = &claimed[0];
    assert_eq!(in_flight_delivery.state, DeliveryState::InFlight);

    let deliveries_in_flight = storage
        .get_deliveries_for_receipt(hookbox::state::ReceiptId(receipt_id))
        .await
        .expect("get_deliveries_for_receipt");
    assert_eq!(
        receipt_aggregate_state(&deliveries_in_flight, ProcessingState::Stored),
        ProcessingState::Stored,
        "InFlight delivery must derive Stored"
    );

    // Step 3: mark emitted → derived state is Emitted.
    storage
        .mark_emitted(DeliveryId(in_flight_delivery.delivery_id.0))
        .await
        .expect("mark_emitted");

    let deliveries_emitted = storage
        .get_deliveries_for_receipt(hookbox::state::ReceiptId(receipt_id))
        .await
        .expect("get_deliveries_for_receipt");
    assert_eq!(
        receipt_aggregate_state(&deliveries_emitted, ProcessingState::Stored),
        ProcessingState::Emitted,
        "Emitted delivery must derive Emitted"
    );
}
