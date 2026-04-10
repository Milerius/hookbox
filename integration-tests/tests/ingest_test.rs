//! Integration tests for the full ingest pipeline with real Postgres.
//!
//! Requires a running Postgres instance.
//! Set `DATABASE_URL` or defaults to `<postgres://localhost/hookbox_test>`

#![expect(clippy::expect_used, reason = "expect is acceptable in test code")]

use bytes::Bytes;
use hookbox::HookboxPipeline;
use hookbox::dedupe::InMemoryRecentDedupe;
use hookbox::emitter::ChannelEmitter;
use hookbox::state::IngestResult;
use hookbox_postgres::PostgresStorage;
use http::HeaderMap;
use sqlx::PgPool;

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
