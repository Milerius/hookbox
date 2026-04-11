//! Integration tests that call CLI command handlers directly (in-process).
//!
//! These tests exercise the `receipts`, `dlq`, `replay`, and `db` modules
//! from `hookbox-cli` by constructing command enums and calling their
//! `run()` functions with a real Postgres backend.
//!
//! Requires a running Postgres instance.
//! Set `DATABASE_URL` or defaults to `postgres://localhost/hookbox_test`

#![expect(clippy::expect_used, reason = "expect is acceptable in test code")]

use hookbox::state::ProcessingState;
use hookbox::traits::Storage;
use hookbox_cli::commands::{dlq, receipts, replay};
use hookbox_cli::db;
use hookbox_postgres::PostgresStorage;
use sqlx::PgPool;
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/hookbox_test".to_owned())
}

/// Connect and migrate, but do NOT delete existing data.
/// Tests that insert their own data by unique UUID will not collide.
async fn setup_pool() -> PgPool {
    let url = database_url();
    let pool = PgPool::connect(&url).await.expect("connect to test db");
    let storage = PostgresStorage::new(pool.clone());
    storage.migrate().await.expect("run migrations");
    pool
}

/// Ingest a minimal receipt into the database and return its UUID.
async fn ingest_receipt(pool: &PgPool, provider: &str) -> Uuid {
    let storage = PostgresStorage::new(pool.clone());

    let receipt_id = Uuid::new_v4();
    let now = chrono::Utc::now();
    let receipt = hookbox::types::WebhookReceipt {
        receipt_id: hookbox::state::ReceiptId(receipt_id),
        provider_name: provider.to_owned(),
        provider_event_id: None,
        external_reference: None,
        dedupe_key: format!("test:{receipt_id}"),
        payload_hash: format!("sha256:{receipt_id}"),
        raw_body: r#"{"test":true}"#.as_bytes().to_vec(),
        parsed_payload: Some(serde_json::json!({"test": true})),
        raw_headers: serde_json::json!({}),
        normalized_event_type: Some("test.event".to_owned()),
        verification_status: hookbox::state::VerificationStatus::Skipped,
        verification_reason: None,
        processing_state: ProcessingState::Stored,
        emit_count: 0,
        last_error: None,
        received_at: now,
        processed_at: None,
        metadata: serde_json::json!({}),
    };

    let result = storage.store(&receipt).await.expect("store receipt");
    assert!(
        matches!(result, hookbox::state::StoreResult::Stored),
        "expected Stored"
    );
    receipt_id
}

// ── db::connect ──────────────────────────────────────────────────────

#[tokio::test]
async fn db_connect_succeeds() {
    let url = database_url();
    let pool = db::connect(&url).await.expect("db::connect should succeed");
    // Verify the pool works by running a simple query.
    let row: (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(&pool)
        .await
        .expect("simple query");
    assert_eq!(row.0, 1);
}

#[tokio::test]
async fn db_connect_bad_url_fails() {
    let result = db::connect("postgres://invalid:5432/nonexistent").await;
    assert!(result.is_err(), "bad URL should fail");
}

// ── receipts::run ────────────────────────────────────────────────────

#[tokio::test]
async fn receipts_list_runs_successfully() {
    let pool = setup_pool().await;
    let _id = ingest_receipt(&pool, "test-provider").await;

    let cmd = receipts::ReceiptsCommand::List {
        database_url: database_url(),
        provider: None,
        state: None,
        limit: 20,
    };
    receipts::run(cmd)
        .await
        .expect("receipts list should succeed");
}

#[tokio::test]
async fn receipts_list_with_provider_filter() {
    let pool = setup_pool().await;
    let _id = ingest_receipt(&pool, "alpha").await;

    let cmd = receipts::ReceiptsCommand::List {
        database_url: database_url(),
        provider: Some("alpha".to_owned()),
        state: None,
        limit: 20,
    };
    receipts::run(cmd)
        .await
        .expect("receipts list with provider filter should succeed");
}

#[tokio::test]
async fn receipts_list_with_state_filter() {
    let pool = setup_pool().await;
    let _id = ingest_receipt(&pool, "test-provider").await;

    let cmd = receipts::ReceiptsCommand::List {
        database_url: database_url(),
        provider: None,
        state: Some("stored".to_owned()),
        limit: 10,
    };
    receipts::run(cmd)
        .await
        .expect("receipts list with state filter should succeed");
}

#[tokio::test]
async fn receipts_list_with_invalid_state_fails() {
    let _pool = setup_pool().await;

    let cmd = receipts::ReceiptsCommand::List {
        database_url: database_url(),
        provider: None,
        state: Some("not_a_real_state".to_owned()),
        limit: 10,
    };
    let result = receipts::run(cmd).await;
    assert!(result.is_err(), "invalid state should fail");
}

#[tokio::test]
async fn receipts_inspect_existing() {
    let pool = setup_pool().await;
    let id = ingest_receipt(&pool, "test-provider").await;

    let cmd = receipts::ReceiptsCommand::Inspect {
        database_url: database_url(),
        receipt_id: id,
    };
    receipts::run(cmd)
        .await
        .expect("receipts inspect should succeed");
}

#[tokio::test]
async fn receipts_inspect_nonexistent() {
    let _pool = setup_pool().await;
    let random_id = Uuid::new_v4();

    let cmd = receipts::ReceiptsCommand::Inspect {
        database_url: database_url(),
        receipt_id: random_id,
    };
    let result = receipts::run(cmd).await;
    assert!(
        result.is_err(),
        "inspecting non-existent receipt should fail"
    );
}

#[tokio::test]
async fn receipts_search_empty_results() {
    let _pool = setup_pool().await;

    let cmd = receipts::ReceiptsCommand::Search {
        database_url: database_url(),
        external_ref: "nonexistent-ref".to_owned(),
        limit: 20,
    };
    receipts::run(cmd)
        .await
        .expect("receipts search should succeed even with no results");
}

// ── dlq::run ─────────────────────────────────────────────────────────

#[tokio::test]
async fn dlq_list_empty() {
    let _pool = setup_pool().await;

    let cmd = dlq::DlqCommand::List {
        database_url: database_url(),
        provider: None,
        limit: 20,
    };
    dlq::run(cmd).await.expect("dlq list should succeed");
}

#[tokio::test]
async fn dlq_list_with_provider_filter() {
    let _pool = setup_pool().await;

    let cmd = dlq::DlqCommand::List {
        database_url: database_url(),
        provider: Some("some-provider".to_owned()),
        limit: 20,
    };
    dlq::run(cmd)
        .await
        .expect("dlq list with provider should succeed");
}

#[tokio::test]
async fn dlq_inspect_nonexistent() {
    let _pool = setup_pool().await;
    let random_id = Uuid::new_v4();

    let cmd = dlq::DlqCommand::Inspect {
        database_url: database_url(),
        receipt_id: random_id,
    };
    let result = dlq::run(cmd).await;
    assert!(
        result.is_err(),
        "inspecting non-existent DLQ receipt should fail"
    );
}

#[tokio::test]
async fn dlq_inspect_existing() {
    let pool = setup_pool().await;
    let id = ingest_receipt(&pool, "test-provider").await;

    // Transition to DeadLettered so it shows up in DLQ queries.
    let storage = PostgresStorage::new(pool.clone());
    storage
        .update_state(id, ProcessingState::DeadLettered, Some("test dead letter"))
        .await
        .expect("set dead-lettered");

    let cmd = dlq::DlqCommand::Inspect {
        database_url: database_url(),
        receipt_id: id,
    };
    dlq::run(cmd).await.expect("dlq inspect should succeed");
}

#[tokio::test]
async fn dlq_retry_nonexistent() {
    let _pool = setup_pool().await;
    let random_id = Uuid::new_v4();

    let cmd = dlq::DlqCommand::Retry {
        database_url: database_url(),
        receipt_id: random_id,
    };
    let result = dlq::run(cmd).await;
    assert!(
        result.is_err(),
        "retrying non-existent DLQ receipt should fail"
    );
}

#[tokio::test]
async fn dlq_retry_existing() {
    let pool = setup_pool().await;
    let id = ingest_receipt(&pool, "test-provider").await;

    // Transition to DeadLettered.
    let storage = PostgresStorage::new(pool.clone());
    storage
        .update_state(id, ProcessingState::DeadLettered, Some("test dead letter"))
        .await
        .expect("set dead-lettered");

    let cmd = dlq::DlqCommand::Retry {
        database_url: database_url(),
        receipt_id: id,
    };
    dlq::run(cmd).await.expect("dlq retry should succeed");

    // Verify the receipt is now in EmitFailed state (reset_for_retry sets it).
    let receipt = storage
        .get(id)
        .await
        .expect("get receipt")
        .expect("receipt exists");
    assert_eq!(
        receipt.processing_state,
        ProcessingState::EmitFailed,
        "receipt should be in EmitFailed after retry"
    );
}

// ── replay::run ──────────────────────────────────────────────────────

#[tokio::test]
async fn replay_single_existing() {
    let pool = setup_pool().await;
    let id = ingest_receipt(&pool, "test-provider").await;

    let cmd = replay::ReplayCommand::Id {
        database_url: database_url(),
        receipt_id: id,
    };
    replay::run(cmd)
        .await
        .expect("replay single should succeed");

    // Verify the receipt is in EmitFailed state (reset_for_retry).
    let storage = PostgresStorage::new(pool.clone());
    let receipt = storage.get(id).await.expect("get").expect("exists");
    assert_eq!(receipt.processing_state, ProcessingState::EmitFailed);
}

#[tokio::test]
async fn replay_single_nonexistent() {
    let _pool = setup_pool().await;
    let random_id = Uuid::new_v4();

    let cmd = replay::ReplayCommand::Id {
        database_url: database_url(),
        receipt_id: random_id,
    };
    let result = replay::run(cmd).await;
    assert!(
        result.is_err(),
        "replaying non-existent receipt should fail"
    );
}

#[tokio::test]
async fn replay_failed_empty() {
    let _pool = setup_pool().await;

    let cmd = replay::ReplayCommand::Failed {
        database_url: database_url(),
        provider: None,
        since: "1h".to_owned(),
        limit: 100,
    };
    replay::run(cmd)
        .await
        .expect("replay failed with no results should succeed");
}

#[tokio::test]
async fn replay_failed_with_provider_filter() {
    let _pool = setup_pool().await;

    let cmd = replay::ReplayCommand::Failed {
        database_url: database_url(),
        provider: Some("nonexistent-provider".to_owned()),
        since: "1h".to_owned(),
        limit: 100,
    };
    replay::run(cmd)
        .await
        .expect("replay failed with provider filter should succeed");
}

#[tokio::test]
async fn replay_failed_replays_emit_failed_receipts() {
    let pool = setup_pool().await;
    let id = ingest_receipt(&pool, "test-provider").await;

    // Transition to EmitFailed so it shows up in the failed query.
    let storage = PostgresStorage::new(pool.clone());
    storage
        .update_state(id, ProcessingState::EmitFailed, Some("test failure"))
        .await
        .expect("set emit_failed");

    let cmd = replay::ReplayCommand::Failed {
        database_url: database_url(),
        provider: None,
        since: "1h".to_owned(),
        limit: 100,
    };
    replay::run(cmd)
        .await
        .expect("replay failed should succeed");
}

/// Ingest a receipt with a specific external_reference.
async fn ingest_receipt_with_ref(pool: &PgPool, provider: &str, external_ref: &str) -> Uuid {
    let storage = PostgresStorage::new(pool.clone());
    let receipt_id = Uuid::new_v4();
    let now = chrono::Utc::now();
    let receipt = hookbox::types::WebhookReceipt {
        receipt_id: hookbox::state::ReceiptId(receipt_id),
        provider_name: provider.to_owned(),
        provider_event_id: None,
        external_reference: Some(external_ref.to_owned()),
        dedupe_key: format!("test-ref:{receipt_id}"),
        payload_hash: format!("sha256:{receipt_id}"),
        raw_body: r#"{"test":true}"#.as_bytes().to_vec(),
        parsed_payload: Some(serde_json::json!({"test": true})),
        raw_headers: serde_json::json!({}),
        normalized_event_type: Some("test.event".to_owned()),
        verification_status: hookbox::state::VerificationStatus::Skipped,
        verification_reason: None,
        processing_state: ProcessingState::Stored,
        emit_count: 0,
        last_error: None,
        received_at: now,
        processed_at: None,
        metadata: serde_json::json!({}),
    };
    storage.store(&receipt).await.expect("store receipt");
    receipt_id
}

/// Test that `receipts search` with results logs each receipt (covers receipts.rs:152).
#[tokio::test]
async fn receipts_search_with_results() {
    let pool = setup_pool().await;
    let unique_ref = format!("order-{}", Uuid::new_v4());
    ingest_receipt_with_ref(&pool, "test-provider", &unique_ref).await;

    let cmd = receipts::ReceiptsCommand::Search {
        database_url: database_url(),
        external_ref: unique_ref,
        limit: 20,
    };
    receipts::run(cmd)
        .await
        .expect("receipts search with results should succeed");
}

/// Test that `dlq list` with dead-lettered receipts logs each entry (covers dlq.rs:80).
#[tokio::test]
async fn dlq_list_with_results() {
    let pool = setup_pool().await;
    let id = ingest_receipt(&pool, "test-provider").await;
    let storage = PostgresStorage::new(pool.clone());
    storage
        .update_state(id, ProcessingState::DeadLettered, Some("test"))
        .await
        .expect("set dead-lettered");

    let cmd = dlq::DlqCommand::List {
        database_url: database_url(),
        provider: None,
        limit: 20,
    };
    dlq::run(cmd)
        .await
        .expect("dlq list with results should succeed");
}
