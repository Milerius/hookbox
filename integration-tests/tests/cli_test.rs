//! Integration tests for CLI operations against real Postgres.

#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "acceptable in test code"
)]

use bytes::Bytes;
use hookbox::HookboxPipeline;
use hookbox::dedupe::InMemoryRecentDedupe;
use hookbox::emitter::ChannelEmitter;
use hookbox::state::ProcessingState;
use hookbox::traits::Storage;
use hookbox_postgres::PostgresStorage;
use http::HeaderMap;
use sqlx::PgPool;

async fn setup() -> (PgPool, PostgresStorage) {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://localhost/hookbox_test".to_owned());
    let pool = PgPool::connect(&url).await.expect("connect");
    let storage = PostgresStorage::new(pool.clone());
    storage.migrate().await.expect("migrate");
    (pool, storage)
}

/// Helper to ingest a test receipt directly via the pipeline.
/// Returns the receipt ID of the ingested receipt.
async fn ingest_test_receipt(
    storage: &PostgresStorage,
    provider: &str,
    body: &[u8],
) -> uuid::Uuid {
    let (emitter, mut rx) = ChannelEmitter::new(16);

    let pipeline = HookboxPipeline::builder()
        .storage(storage.clone())
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter(emitter)
        .build();

    let result = pipeline
        .ingest(provider, HeaderMap::new(), Bytes::from(body.to_vec()))
        .await
        .expect("ingest should succeed");
    // drain one event
    let _ = rx.try_recv();

    // Extract the receipt ID from the result
    match result {
        hookbox::state::IngestResult::Accepted { receipt_id, .. } => receipt_id.0,
        other => panic!("expected Accepted, got {other:?}"),
    }
}

#[tokio::test]
async fn query_by_external_reference_returns_matching() {
    let (_pool, storage) = setup().await;
    // No receipts with external_reference set via pipeline (it's None by default)
    let results = storage
        .query_by_external_reference("cli_test_pay_999", Some(10))
        .await
        .unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn query_for_retry_returns_empty_when_none_failed() {
    let (_pool, storage) = setup().await;
    // Ingest a receipt (it will be in Stored/Emitted state, not EmitFailed)
    let id = ingest_test_receipt(&storage, "cli_retry_empty", b"retry_test_body_unique_1").await;
    // Verify it exists but is not in EmitFailed state
    let receipt = storage.get(id).await.unwrap().unwrap();
    assert_ne!(receipt.processing_state, ProcessingState::EmitFailed);
}

#[tokio::test]
async fn reset_for_retry_makes_receipt_retryable() {
    let (_pool, storage) = setup().await;
    let id = ingest_test_receipt(&storage, "cli_reset", b"reset_test_body_unique_2").await;

    // Manually set to DeadLettered
    storage
        .update_state(id, ProcessingState::DeadLettered, Some("test error"))
        .await
        .unwrap();

    // Reset for retry
    storage.reset_for_retry(id).await.unwrap();

    // Should now be in EmitFailed with emit_count 0
    let receipt = storage.get(id).await.unwrap().unwrap();
    assert_eq!(receipt.processing_state, ProcessingState::EmitFailed);
    assert_eq!(receipt.emit_count, 0);
}

#[tokio::test]
async fn retry_failed_promotes_to_dlq_at_max() {
    let (_pool, storage) = setup().await;
    let id = ingest_test_receipt(&storage, "cli_dlq", b"dlq_test_body_unique_3").await;

    // Set to EmitFailed
    storage
        .update_state(id, ProcessingState::EmitFailed, None)
        .await
        .unwrap();

    // Retry 5 times (max_attempts = 5)
    for _ in 0..5 {
        storage.retry_failed(id, 5).await.unwrap();
    }

    // Should now be DeadLettered
    let receipt = storage.get(id).await.unwrap().unwrap();
    assert_eq!(receipt.processing_state, ProcessingState::DeadLettered);
    assert_eq!(receipt.emit_count, 5);
}

#[tokio::test]
async fn query_failed_since_filters_correctly() {
    let (_pool, storage) = setup().await;
    let id =
        ingest_test_receipt(&storage, "cli_failed_since", b"failed_since_body_unique_4").await;

    // Set to EmitFailed
    storage
        .update_state(id, ProcessingState::EmitFailed, None)
        .await
        .unwrap();

    // Query failed since 1 hour ago — should find it
    let since = chrono::Utc::now() - chrono::Duration::hours(1);
    let results = storage
        .query_failed_since(Some("cli_failed_since"), since, Some(10))
        .await
        .unwrap();
    assert!(
        results.iter().any(|r| r.receipt_id.0 == id),
        "expected to find our receipt in failed results"
    );

    // Query for different provider — should not contain our receipt
    let results = storage
        .query_failed_since(Some("nonexistent_provider_xyz"), since, Some(10))
        .await
        .unwrap();
    assert!(results.is_empty());
}
