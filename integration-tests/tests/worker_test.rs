//! Integration tests for the retry worker against real Postgres.
//!
//! Requires a running Postgres instance.
//! Set `DATABASE_URL` or defaults to `postgres://localhost/hookbox_test`

#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "expect/unwrap/panic are acceptable in test code"
)]

use std::sync::OnceLock;
use std::time::Duration;

use bytes::Bytes;
use http::HeaderMap;
use hookbox::HookboxPipeline;
use hookbox::dedupe::InMemoryRecentDedupe;
use hookbox::emitter::ChannelEmitter;
use hookbox::state::ProcessingState;
use hookbox::traits::Storage;
use hookbox_postgres::PostgresStorage;
use hookbox_server::worker::RetryWorker;
use sqlx::PgPool;
use tokio::sync::Mutex;
use uuid::Uuid;

/// Global mutex so worker tests do not interfere with each other when run in
/// the default multi-test-thread mode.
static WORKER_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn worker_test_lock() -> &'static Mutex<()> {
    WORKER_TEST_LOCK.get_or_init(|| Mutex::new(()))
}

async fn setup_pool() -> PgPool {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://localhost/hookbox_test".to_owned());
    let pool = PgPool::connect(&url).await.expect("connect to test db");
    let storage = PostgresStorage::new(pool.clone());
    storage.migrate().await.expect("run migrations");
    sqlx::query("DELETE FROM webhook_receipts")
        .execute(&pool)
        .await
        .expect("clean up");
    pool
}

/// Helper: ingest a single unique event and return the receipt UUID.
async fn ingest_one(pool: &PgPool) -> Uuid {
    let storage = PostgresStorage::new(pool.clone());
    let (emitter, mut rx) = ChannelEmitter::new(16);

    let pipeline = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter(emitter)
        .build();

    let unique_body = Bytes::from(format!(
        r#"{{"event":"payment.completed","id":"{}"}}"#,
        Uuid::new_v4()
    ));
    let result = pipeline
        .ingest("test", HeaderMap::new(), unique_body)
        .await
        .expect("ingest should not error");

    // Drain the channel so the emit count doesn't accumulate
    let _ = rx.try_recv();

    let hookbox::IngestResult::Accepted { receipt_id } = result else {
        unreachable!("expected Accepted result")
    };
    receipt_id.0
}

/// Test 1: Worker retries an EmitFailed receipt and moves it to Emitted.
#[tokio::test]
async fn worker_retries_emit_failed_receipt() {
    let _guard = worker_test_lock().lock().await;
    let pool = setup_pool().await;
    let storage = PostgresStorage::new(pool.clone());

    // --- Step 1: ingest with a dropped receiver so emit fails ---
    let unique_body = Bytes::from(format!(
        r#"{{"event":"payment.completed","id":"{}"}}"#,
        Uuid::new_v4()
    ));

    // Create emitter whose receiver is immediately dropped → emit will fail
    let (failing_emitter, _rx_drop) = ChannelEmitter::new(1);
    drop(_rx_drop); // receiver gone → send returns Err → EmitFailed

    let pipeline = HookboxPipeline::builder()
        .storage(PostgresStorage::new(pool.clone()))
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter(failing_emitter)
        .build();

    let result = pipeline
        .ingest("test", HeaderMap::new(), unique_body)
        .await
        .expect("ingest should not error");

    let hookbox::IngestResult::Accepted { receipt_id } = result else {
        unreachable!("expected Accepted result")
    };
    let id = receipt_id.0;

    // --- Step 2: verify receipt is in EmitFailed ---
    let receipt = storage
        .get(id)
        .await
        .expect("storage.get should succeed")
        .expect("receipt should exist");
    assert_eq!(
        receipt.processing_state,
        ProcessingState::EmitFailed,
        "receipt should be in EmitFailed state after dropped receiver"
    );

    // --- Step 3: create a working emitter (keep receiver alive via drain task) ---
    let (good_emitter, good_rx) = ChannelEmitter::new(16);
    tokio::spawn(async move {
        let mut rx = good_rx;
        while rx.recv().await.is_some() {}
    });

    // --- Step 4: spawn RetryWorker with short interval ---
    let worker = RetryWorker::new(
        PostgresStorage::new(pool.clone()),
        Box::new(good_emitter),
        Duration::from_millis(100),
        5,
    );
    let handle = worker.spawn();

    // --- Step 5: wait for worker to retry ---
    tokio::time::sleep(Duration::from_millis(500)).await;
    handle.abort();

    // --- Step 6: verify receipt is now Emitted ---
    let receipt = storage
        .get(id)
        .await
        .expect("storage.get should succeed")
        .expect("receipt should exist after retry");
    assert_eq!(
        receipt.processing_state,
        ProcessingState::Emitted,
        "receipt should be Emitted after worker retry"
    );
}

/// Test 2: Worker promotes a receipt to DeadLettered after max_attempts.
#[tokio::test]
async fn worker_promotes_to_dlq_after_max_attempts() {
    let _guard = worker_test_lock().lock().await;
    let pool = setup_pool().await;
    let storage = PostgresStorage::new(pool.clone());

    // --- Step 1: ingest normally (succeeds) ---
    let id = ingest_one(&pool).await;

    // --- Step 2: manually set to EmitFailed and simulate 4 prior retries ---
    // First put the receipt into EmitFailed state
    storage
        .update_state(id, ProcessingState::EmitFailed, Some("forced for test"))
        .await
        .expect("update_state should succeed");

    // Simulate 4 prior retries (emit_count goes 0→1→2→3→4) with max_attempts=5
    // After 4 calls, emit_count = 4, state = emit_failed (since 4 < 5)
    for _ in 0..4 {
        storage
            .retry_failed(id, 5)
            .await
            .expect("retry_failed should succeed");
    }

    // --- Step 3: verify emit_count is 4 ---
    let receipt = storage
        .get(id)
        .await
        .expect("storage.get should succeed")
        .expect("receipt should exist");
    assert_eq!(receipt.emit_count, 4, "emit_count should be 4 after 4 retries");
    assert_eq!(
        receipt.processing_state,
        ProcessingState::EmitFailed,
        "receipt should still be EmitFailed"
    );

    // --- Step 4: create failing emitter (dropped receiver) ---
    let (failing_emitter, _rx_drop) = ChannelEmitter::new(1);
    drop(_rx_drop);

    // --- Step 5: spawn RetryWorker with 100ms interval, max_attempts=5 ---
    let worker = RetryWorker::new(
        PostgresStorage::new(pool.clone()),
        Box::new(failing_emitter),
        Duration::from_millis(100),
        5,
    );
    let handle = worker.spawn();

    // --- Step 6: wait for worker to attempt one more retry ---
    tokio::time::sleep(Duration::from_millis(500)).await;
    handle.abort();

    // --- Step 7: verify receipt is DeadLettered with emit_count 5 ---
    let receipt = storage
        .get(id)
        .await
        .expect("storage.get should succeed")
        .expect("receipt should exist after DLQ promotion");
    assert_eq!(
        receipt.processing_state,
        ProcessingState::DeadLettered,
        "receipt should be DeadLettered after exhausting max_attempts"
    );
    assert_eq!(
        receipt.emit_count, 5,
        "emit_count should be 5 after final failed retry"
    );
}
