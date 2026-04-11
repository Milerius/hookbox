//! Integration tests for the full ingest pipeline with real Postgres.
//!
//! Requires a running Postgres instance.
//! Set `DATABASE_URL` or defaults to `<postgres://localhost/hookbox_test>`

#![expect(clippy::expect_used, reason = "expect is acceptable in test code")]
#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::OnceLock;

use bytes::Bytes;
use chrono::Utc;
use hookbox::HookboxPipeline;
use hookbox::dedupe::InMemoryRecentDedupe;
use hookbox::emitter::ChannelEmitter;
use hookbox::state::{IngestResult, ProcessingState};
use hookbox::traits::Storage as _;
use hookbox::types::ReceiptFilter;
use hookbox_postgres::PostgresStorage;
use http::HeaderMap;
use sqlx::PgPool;
use tokio::sync::Mutex;
use uuid::Uuid;

/// Global mutex so ingest tests do not interfere with each other when run in
/// the default multi-test-thread mode.
static INGEST_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn ingest_test_lock() -> &'static Mutex<()> {
    INGEST_TEST_LOCK.get_or_init(|| Mutex::new(()))
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

#[tokio::test]
async fn ingest_stores_and_deduplicates() {
    let _guard = ingest_test_lock().lock().await;
    let pool = setup_pool().await;
    let storage = PostgresStorage::new(pool.clone());
    let (emitter, mut rx) = ChannelEmitter::new(16);

    let pipeline = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter(emitter)
        .build();

    // First ingest — should be accepted
    let body = Bytes::from(r#"{"event":"payment.completed","id":"pay_123"}"#);
    let result = pipeline
        .ingest("test", HeaderMap::new(), body.clone())
        .await
        .expect("ingest should not error");
    assert!(matches!(result, IngestResult::Accepted { .. }));

    // Should have emitted an event
    let event = rx.try_recv().expect("expected emitted event");
    assert_eq!(event.provider_name, "test");

    // Second ingest with same body — should be duplicate
    let result2 = pipeline
        .ingest("test", HeaderMap::new(), body)
        .await
        .expect("ingest should not error");
    assert!(matches!(result2, IngestResult::Duplicate { .. }));
}

/// Test that `query_failed_since` returns receipts in failed states.
#[tokio::test]
async fn query_failed_since_returns_failed_receipts() {
    let _guard = ingest_test_lock().lock().await;
    let pool = setup_pool().await;
    let storage = PostgresStorage::new(pool.clone());

    // Ingest a receipt — it starts Stored/Emitted.
    let (emitter, mut rx) = ChannelEmitter::new(16);
    let pipeline = HookboxPipeline::builder()
        .storage(PostgresStorage::new(pool.clone()))
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter(emitter)
        .build();

    let body = Bytes::from(format!(
        r#"{{"event":"payment.failed","id":"{}"}}"#,
        Uuid::new_v4()
    ));
    let result = pipeline
        .ingest("stripe", HeaderMap::new(), body)
        .await
        .expect("ingest should not error");
    let _ = rx.try_recv();

    let hookbox::IngestResult::Accepted { receipt_id } = result else {
        panic!("expected Accepted")
    };

    // Move the receipt to EmitFailed so it shows up in query_failed_since.
    storage
        .update_state(
            receipt_id.0,
            ProcessingState::EmitFailed,
            Some("test error"),
        )
        .await
        .expect("update_state should succeed");

    let since = Utc::now() - chrono::Duration::hours(1);

    // Query without provider filter.
    let failed = storage
        .query_failed_since(None, since, None)
        .await
        .expect("query_failed_since should succeed");
    assert!(
        failed.iter().any(|r| r.receipt_id == receipt_id),
        "failed receipt should appear in query_failed_since results"
    );

    // Query with matching provider filter.
    let failed_stripe = storage
        .query_failed_since(Some("stripe"), since, None)
        .await
        .expect("query_failed_since with provider should succeed");
    assert!(
        failed_stripe.iter().any(|r| r.receipt_id == receipt_id),
        "failed receipt should appear when filtering by provider"
    );

    // Query with non-matching provider filter.
    let failed_other = storage
        .query_failed_since(Some("github"), since, None)
        .await
        .expect("query_failed_since with non-matching provider should succeed");
    assert!(
        !failed_other.iter().any(|r| r.receipt_id == receipt_id),
        "receipt should not appear when filtering by a different provider"
    );
}

/// Test `query` with `provider_event_id` filter.
#[tokio::test]
async fn query_with_provider_event_id_filter() {
    let _guard = ingest_test_lock().lock().await;
    let pool = setup_pool().await;
    let storage = PostgresStorage::new(pool.clone());

    // Ingest a unique receipt.
    let (emitter, mut rx) = ChannelEmitter::new(16);
    let pipeline = HookboxPipeline::builder()
        .storage(PostgresStorage::new(pool.clone()))
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter(emitter)
        .build();

    let unique_id = Uuid::new_v4().to_string();
    let body = Bytes::from(format!(r#"{{"event":"order","id":"{unique_id}"}}"#));
    let result = pipeline
        .ingest("stripe", HeaderMap::new(), body)
        .await
        .expect("ingest should not error");
    let _ = rx.try_recv();
    assert!(matches!(result, IngestResult::Accepted { .. }));

    // Query with a provider_event_id filter that matches nothing.
    let filter = ReceiptFilter {
        provider_event_id: Some("nonexistent-event-id".to_owned()),
        ..Default::default()
    };
    let results = storage.query(filter).await.expect("query should succeed");
    assert!(
        results.is_empty(),
        "filtering by nonexistent provider_event_id should return empty"
    );
}

/// Test `retry_failed` on a non-existent receipt ID (`rows_affected` == 0).
#[tokio::test]
async fn retry_failed_on_nonexistent_id_logs_warn_and_succeeds() {
    let _guard = ingest_test_lock().lock().await;
    let pool = setup_pool().await;
    let storage = PostgresStorage::new(pool.clone());

    // Call retry_failed with a random UUID that doesn't exist in the DB.
    let nonexistent = Uuid::new_v4();
    let result = storage.retry_failed(nonexistent, 5).await;
    // Should succeed (no error) even when no row is matched.
    assert!(
        result.is_ok(),
        "retry_failed on nonexistent id should succeed"
    );
}

/// Test `update_state` on a non-existent receipt ID (`rows_affected` == 0).
#[tokio::test]
async fn update_state_on_nonexistent_id_logs_warn_and_succeeds() {
    let _guard = ingest_test_lock().lock().await;
    let pool = setup_pool().await;
    let storage = PostgresStorage::new(pool.clone());

    let nonexistent = Uuid::new_v4();
    let result = storage
        .update_state(nonexistent, ProcessingState::Emitted, None)
        .await;
    assert!(
        result.is_ok(),
        "update_state on nonexistent id should succeed"
    );
}
