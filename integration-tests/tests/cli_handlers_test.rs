//! Integration tests that call CLI command handlers directly (in-process).
//!
//! These tests exercise the `receipts`, `dlq`, `replay`, and `db` modules
//! from `hookbox-cli` by constructing command enums and calling their
//! `run()` functions with a real Postgres backend.
//!
//! Requires a running Postgres instance.
//! Set `DATABASE_URL` or defaults to `postgres://localhost/hookbox_test`

#![expect(clippy::expect_used, reason = "expect is acceptable in test code")]

use hookbox::state::{ProcessingState, ReceiptId};
use hookbox::traits::Storage;
use hookbox_cli::commands::{dlq, receipts, replay};
use hookbox_cli::db;
use hookbox_postgres::{DeliveryStorage as _, PostgresStorage};
use sqlx::PgPool;
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/hookbox_test".to_owned())
}

/// Force a delivery row into the `dead_lettered` state via raw SQL, bypassing
/// the lease-guarded `mark_dead_lettered` path. Tests that need a pre-staged
/// DLQ row cannot use `mark_dead_lettered` directly because it only accepts
/// `in_flight` rows (see lease-guard in `crates/hookbox-postgres/src/storage.rs`).
async fn force_dead_lettered(pool: &PgPool, delivery_id: Uuid, note: &str) {
    sqlx::query(
        "UPDATE webhook_deliveries SET state = 'dead_lettered', last_error = $2 WHERE delivery_id = $1",
    )
    .bind(delivery_id)
    .bind(note)
    .execute(pool)
    .await
    .expect("force dead_lettered");
}

/// Write a minimal valid hookbox.toml containing a single `test-emitter`
/// channel entry. Returns the temp path — keep the returned `String` alive
/// for the duration of the test so the file is not deleted prematurely.
fn write_test_config() -> String {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("hookbox-cli-test-{}.toml", Uuid::new_v4()));
    let body = r#"
[database]
url = "postgres://localhost/hookbox_test"

[[emitters]]
name = "test-emitter"
type = "channel"
"#;
    std::fs::write(&path, body).expect("write test config");
    path.to_string_lossy().into_owned()
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
        emitter: None,
        limit: 20,
        offset: 0,
    };
    dlq::run(cmd).await.expect("dlq list should succeed");
}

#[tokio::test]
async fn dlq_list_with_provider_filter() {
    let _pool = setup_pool().await;

    let cmd = dlq::DlqCommand::List {
        database_url: database_url(),
        emitter: Some("some-emitter".to_owned()),
        limit: 20,
        offset: 0,
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
        delivery_id: random_id,
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
    let receipt_id = ingest_receipt(&pool, "test-provider").await;

    // Create a delivery row, then transition it to DeadLettered so the
    // delivery-level DLQ inspect handler can find it.
    let storage = PostgresStorage::new(pool.clone());
    let delivery_id = storage
        .insert_replay(ReceiptId(receipt_id), "test-emitter")
        .await
        .expect("insert delivery row");
    force_dead_lettered(&pool, delivery_id.0, "test dead letter").await;

    let cmd = dlq::DlqCommand::Inspect {
        database_url: database_url(),
        delivery_id: delivery_id.0,
    };
    dlq::run(cmd).await.expect("dlq inspect should succeed");
}

#[tokio::test]
async fn dlq_retry_nonexistent() {
    let _pool = setup_pool().await;
    let random_id = Uuid::new_v4();

    let cmd = dlq::DlqCommand::Retry {
        database_url: database_url(),
        config: write_test_config(),
        delivery_id: random_id,
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
    let receipt_id = ingest_receipt(&pool, "test-provider").await;

    // Seed a dead-lettered delivery row that the retry handler can target.
    let storage = PostgresStorage::new(pool.clone());
    let original = storage
        .insert_replay(ReceiptId(receipt_id), "test-emitter")
        .await
        .expect("insert original delivery");
    force_dead_lettered(&pool, original.0, "test dead letter").await;

    let cmd = dlq::DlqCommand::Retry {
        database_url: database_url(),
        config: write_test_config(),
        delivery_id: original.0,
    };
    dlq::run(cmd).await.expect("dlq retry should succeed");

    // Fan-out retry inserts a fresh pending delivery row; the original stays
    // in DeadLettered. Verify there are now two rows for this receipt.
    let deliveries = storage
        .get_deliveries_for_receipt(ReceiptId(receipt_id))
        .await
        .expect("get_deliveries_for_receipt");
    assert_eq!(
        deliveries.len(),
        2,
        "retry should have inserted a second delivery row"
    );
}

// ── replay::run ──────────────────────────────────────────────────────

#[tokio::test]
async fn replay_single_existing_explicit_emitter() {
    let pool = setup_pool().await;
    let receipt_id = ingest_receipt(&pool, "test-provider").await;

    // --emitter forces single-emitter path, which does not require
    // pre-existing delivery rows.
    let cmd = replay::ReplayCommand::Id {
        database_url: database_url(),
        config: write_test_config(),
        receipt_id,
        emitter: Some("test-emitter".to_owned()),
    };
    replay::run(cmd)
        .await
        .expect("replay single should succeed");

    // A fresh pending delivery row should now exist for the target emitter.
    let storage = PostgresStorage::new(pool.clone());
    let deliveries = storage
        .get_deliveries_for_receipt(ReceiptId(receipt_id))
        .await
        .expect("get_deliveries_for_receipt");
    assert_eq!(deliveries.len(), 1, "one delivery row should exist");
    assert_eq!(deliveries[0].emitter_name, "test-emitter");
}

#[tokio::test]
async fn replay_single_without_emitter_bails_without_history() {
    let pool = setup_pool().await;
    let receipt_id = ingest_receipt(&pool, "test-provider").await;

    // No pre-existing delivery rows + no --emitter: handler must bail.
    let cmd = replay::ReplayCommand::Id {
        database_url: database_url(),
        config: write_test_config(),
        receipt_id,
        emitter: None,
    };
    let result = replay::run(cmd).await;
    assert!(
        result.is_err(),
        "replay id without --emitter should fail when no deliveries exist"
    );
}

#[tokio::test]
async fn replay_single_nonexistent() {
    let _pool = setup_pool().await;
    let random_id = Uuid::new_v4();

    let cmd = replay::ReplayCommand::Id {
        database_url: database_url(),
        config: write_test_config(),
        receipt_id: random_id,
        emitter: None,
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

/// Write a hookbox config with an explicit list of channel-type emitter
/// names. Used by the replay tests that need to seed deliveries for one
/// set of emitters and then ensure the handler walks the historical set.
fn write_test_config_with_emitters(emitters: &[&str]) -> String {
    use std::fmt::Write as _;
    let dir = std::env::temp_dir();
    let path = dir.join(format!("hookbox-cli-test-{}.toml", Uuid::new_v4()));
    let mut body = String::from(
        "
[database]
url = \"postgres://localhost/hookbox_test\"
",
    );
    for name in emitters {
        write!(
            body,
            "\n[[emitters]]\nname = \"{name}\"\ntype = \"channel\"\n"
        )
        .expect("write to String never fails");
    }
    std::fs::write(&path, body).expect("write test config");
    path.to_string_lossy().into_owned()
}

/// `--emitter` names an emitter that is not present in the config file:
/// the handler must refuse and produce an error whose message mentions
/// both the requested emitter name and the receipt id.
#[tokio::test]
async fn replay_single_explicit_emitter_not_in_config_errors() {
    let pool = setup_pool().await;
    let receipt_id = ingest_receipt(&pool, "test-provider").await;

    let cmd = replay::ReplayCommand::Id {
        database_url: database_url(),
        config: write_test_config(),
        receipt_id,
        emitter: Some("ghost-emitter".to_owned()),
    };
    let err = replay::run(cmd)
        .await
        .expect_err("stale --emitter must error");
    let rendered = err.to_string();
    assert!(
        rendered.contains("ghost-emitter") && rendered.contains(&receipt_id.to_string()),
        "unexpected error message: {rendered}"
    );
}

/// Without `--emitter`, the handler infers the emitter set from
/// historical delivery rows. If any historical emitter is no longer in
/// the config file, it must refuse rather than silently skip them.
#[tokio::test]
async fn replay_single_historical_emitters_stale_errors() {
    let pool = setup_pool().await;
    let storage = PostgresStorage::new(pool.clone());
    let receipt_id = Uuid::new_v4();
    let now = chrono::Utc::now();
    let receipt = hookbox::types::WebhookReceipt {
        receipt_id: ReceiptId(receipt_id),
        provider_name: "test-provider".to_owned(),
        provider_event_id: None,
        external_reference: None,
        dedupe_key: format!("replay-stale:{receipt_id}"),
        payload_hash: format!("sha256:{receipt_id}"),
        raw_body: b"{}".to_vec(),
        parsed_payload: None,
        raw_headers: serde_json::json!({}),
        normalized_event_type: None,
        verification_status: hookbox::state::VerificationStatus::Skipped,
        verification_reason: None,
        processing_state: ProcessingState::Stored,
        emit_count: 0,
        last_error: None,
        received_at: now,
        processed_at: None,
        metadata: serde_json::json!({}),
    };
    storage
        .store_with_deliveries(&receipt, &["legacy-emitter".to_owned()])
        .await
        .expect("seed receipt + legacy delivery");

    // Config only knows "test-emitter" — the historical "legacy-emitter"
    // row is stale and the handler must refuse the replay.
    let cmd = replay::ReplayCommand::Id {
        database_url: database_url(),
        config: write_test_config(),
        receipt_id,
        emitter: None,
    };
    let err = replay::run(cmd)
        .await
        .expect_err("stale historical emitter must error");
    let rendered = err.to_string();
    assert!(
        rendered.contains("legacy-emitter"),
        "unexpected error message: {rendered}"
    );
}

/// Without `--emitter` and with every historical emitter still
/// configured, the handler must fan out one new pending delivery row per
/// historical emitter via `insert_replays`.
#[tokio::test]
async fn replay_single_fans_out_across_historical_emitters() {
    let pool = setup_pool().await;
    let storage = PostgresStorage::new(pool.clone());
    let receipt_id = Uuid::new_v4();
    let now = chrono::Utc::now();
    let receipt = hookbox::types::WebhookReceipt {
        receipt_id: ReceiptId(receipt_id),
        provider_name: "test-provider".to_owned(),
        provider_event_id: None,
        external_reference: None,
        dedupe_key: format!("replay-fanout:{receipt_id}"),
        payload_hash: format!("sha256:{receipt_id}"),
        raw_body: b"{}".to_vec(),
        parsed_payload: None,
        raw_headers: serde_json::json!({}),
        normalized_event_type: None,
        verification_status: hookbox::state::VerificationStatus::Skipped,
        verification_reason: None,
        processing_state: ProcessingState::Stored,
        emit_count: 0,
        last_error: None,
        received_at: now,
        processed_at: None,
        metadata: serde_json::json!({}),
    };
    storage
        .store_with_deliveries(&receipt, &["emitter-a".to_owned(), "emitter-b".to_owned()])
        .await
        .expect("seed receipt + two deliveries");

    let config = write_test_config_with_emitters(&["emitter-a", "emitter-b"]);
    let cmd = replay::ReplayCommand::Id {
        database_url: database_url(),
        config,
        receipt_id,
        emitter: None,
    };
    replay::run(cmd)
        .await
        .expect("fan-out replay should succeed");

    // After replay, each historical emitter should have 2 rows (the
    // original + the freshly inserted replay).
    let deliveries = storage
        .get_deliveries_for_receipt(ReceiptId(receipt_id))
        .await
        .expect("get_deliveries_for_receipt");
    let a_count = deliveries
        .iter()
        .filter(|d| d.emitter_name == "emitter-a")
        .count();
    let b_count = deliveries
        .iter()
        .filter(|d| d.emitter_name == "emitter-b")
        .count();
    assert_eq!(a_count, 2, "expected 2 rows for emitter-a, got {a_count}");
    assert_eq!(b_count, 2, "expected 2 rows for emitter-b, got {b_count}");
}

/// Configs that use the legacy singular `[emitter]` block still parse but
/// emit a deprecation warning. `run_replay_id` writes each warning to
/// stderr before continuing — this test pins that the warning-write path
/// is exercised without blowing up.
#[tokio::test]
async fn replay_single_explicit_emitter_writes_legacy_warning() {
    let pool = setup_pool().await;
    let receipt_id = ingest_receipt(&pool, "test-provider").await;

    // Legacy `[emitter]` singular form normalises to a single emitter
    // named "default" and produces a deprecation warning.
    let dir = std::env::temp_dir();
    let path = dir.join(format!("hookbox-cli-legacy-{}.toml", Uuid::new_v4()));
    let body = r#"
[database]
url = "postgres://localhost/hookbox_test"

[emitter]
type = "channel"
"#;
    // Pin the contract that this body actually triggers a deprecation
    // warning during normalization — without this, a regression that
    // dropped the warning would still let the test pass because
    // replay::run only writes warnings to stderr and returns Ok.
    let (_parsed, warnings) =
        hookbox_server::config::parse_and_normalize(body).expect("legacy config should parse");
    assert!(
        !warnings.is_empty(),
        "expected a deprecation warning for legacy [emitter] section, got none"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("deprecated") || w.contains("[[emitters]]")),
        "expected a deprecation warning naming the legacy section, got {warnings:?}"
    );

    std::fs::write(&path, body).expect("write legacy config");
    let config = path.to_string_lossy().into_owned();

    let cmd = replay::ReplayCommand::Id {
        database_url: database_url(),
        config: config.clone(),
        receipt_id,
        emitter: Some("default".to_owned()),
    };
    replay::run(cmd)
        .await
        .expect("legacy-config replay should still succeed");

    std::fs::remove_file(&path).ok();
}

/// Ingest a receipt with a specific `external_reference`.
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

/// Test that `dlq list` with dead-lettered deliveries logs each entry (covers dlq.rs:80).
#[tokio::test]
async fn dlq_list_with_results() {
    let pool = setup_pool().await;
    let receipt_id = ingest_receipt(&pool, "test-provider").await;

    // Create a dead-lettered delivery row scoped to a unique emitter name so
    // the assertion below is deterministic regardless of other test data.
    let emitter_name = format!("test-emitter-{}", Uuid::new_v4());
    let storage = PostgresStorage::new(pool.clone());
    let delivery_id = storage
        .insert_replay(ReceiptId(receipt_id), &emitter_name)
        .await
        .expect("insert delivery row");
    force_dead_lettered(&pool, delivery_id.0, "test").await;

    let cmd = dlq::DlqCommand::List {
        database_url: database_url(),
        emitter: Some(emitter_name.clone()),
        limit: 20,
        offset: 0,
    };
    dlq::run(cmd)
        .await
        .expect("dlq list with results should succeed");

    // The list handler only logs; assert the underlying storage call also
    // returns the row we just inserted.
    let rows = storage
        .list_dlq(Some(&emitter_name), 20, 0)
        .await
        .expect("list_dlq");
    assert_eq!(rows.len(), 1, "one dead-lettered delivery should be listed");
    assert_eq!(rows[0].0.delivery_id, delivery_id);
}
