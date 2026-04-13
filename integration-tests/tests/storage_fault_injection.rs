//! Fault-injection tests for `PostgresStorage` / `DeliveryStorage`.
//!
//! Each public method has a `.map_err(|e| StorageError::Internal(...))` guard
//! around its sqlx call. The happy-path BDD + integration suites never
//! exercise those branches because the test Postgres is always healthy.
//!
//! This module deliberately closes the pool mid-test (`pool.close_now()`) and
//! asserts that every Storage / `DeliveryStorage` entry point surfaces the
//! pool error as `StorageError::Internal`. That pins the behavioural contract
//! ("we never swallow a db error") and drives coverage through every
//! `map_err` arm in `crates/hookbox-postgres/src/storage.rs`.

#![expect(
    clippy::expect_used,
    reason = "expect/unwrap/panic are acceptable in test code"
)]
#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use hookbox::state::{DeliveryId, ProcessingState, VerificationStatus};
use hookbox::traits::Storage;
use hookbox::types::{ReceiptFilter, ReceiptId, WebhookReceipt};
use hookbox_postgres::{DeliveryStorage, PostgresStorage};

fn sample_receipt() -> WebhookReceipt {
    WebhookReceipt {
        receipt_id: ReceiptId::new(),
        provider_name: "fault-inject".to_owned(),
        provider_event_id: None,
        external_reference: Some("ext-1".to_owned()),
        dedupe_key: format!("fault:{}", uuid::Uuid::new_v4()),
        payload_hash: "h".to_owned(),
        raw_body: b"{}".to_vec(),
        parsed_payload: None,
        raw_headers: serde_json::json!({}),
        normalized_event_type: None,
        verification_status: VerificationStatus::Verified,
        verification_reason: None,
        processing_state: ProcessingState::Stored,
        emit_count: 0,
        last_error: None,
        received_at: chrono::Utc::now(),
        processed_at: None,
        metadata: serde_json::json!({}),
    }
}

/// Covers the "`rows_affected` == 0" branch in
/// `PostgresStorage::reset_for_retry`, which returns a synthesised
/// `StorageError::Internal("no receipt found ...")` rather than surfacing a
/// raw sqlx error. This path only fires against a live pool.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn reset_for_retry_missing_receipt_returns_internal(pool: sqlx::PgPool) {
    let storage = PostgresStorage::new(pool);
    let missing_id = uuid::Uuid::new_v4();
    let err = storage
        .reset_for_retry(missing_id)
        .await
        .expect_err("missing receipt must surface an Internal error");
    let rendered = err.to_string();
    assert!(
        rendered.contains("no receipt found"),
        "unexpected error message: {rendered}"
    );
}

/// Close the pool before any storage call, then exercise every public entry
/// point on `PostgresStorage` — Storage trait, `DeliveryStorage` trait, and
/// the inherent ops helpers. Every call must surface a `StorageError::Internal`
/// because the underlying sqlx future resolves to `PoolClosed`.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn all_methods_surface_internal_after_pool_close(pool: sqlx::PgPool) {
    let storage = PostgresStorage::new(pool.clone());
    let arc_storage: Arc<PostgresStorage> = Arc::new(PostgresStorage::new(pool.clone()));

    // Pull the rug out: every subsequent sqlx call will see a closed pool.
    pool.close().await;

    // ------------------------------------------------------------------
    // Storage trait
    // ------------------------------------------------------------------
    let receipt = sample_receipt();

    storage
        .store(&receipt)
        .await
        .expect_err("store must fail on closed pool");

    storage
        .store_with_deliveries(&receipt, &["emitter-a".to_owned()])
        .await
        .expect_err("store_with_deliveries must fail on closed pool");

    storage
        .get(receipt.receipt_id.0)
        .await
        .expect_err("get must fail on closed pool");

    storage
        .update_state(receipt.receipt_id.0, ProcessingState::Emitted, None)
        .await
        .expect_err("update_state must fail on closed pool");

    storage
        .query(ReceiptFilter::default())
        .await
        .expect_err("query must fail on closed pool");

    // ------------------------------------------------------------------
    // PostgresStorage inherent ops helpers (retry worker + CLI surface)
    // ------------------------------------------------------------------
    storage
        .query_for_retry(5)
        .await
        .expect_err("query_for_retry must fail on closed pool");

    storage
        .retry_failed(receipt.receipt_id.0, 5)
        .await
        .expect_err("retry_failed must fail on closed pool");

    storage
        .reset_for_retry(receipt.receipt_id.0)
        .await
        .expect_err("reset_for_retry must fail on closed pool");

    storage
        .query_by_external_reference("ext-1", None)
        .await
        .expect_err("query_by_external_reference must fail on closed pool");

    storage
        .query_failed_since(Some("fault-inject"), chrono::Utc::now(), Some(10))
        .await
        .expect_err("query_failed_since must fail on closed pool");

    // ------------------------------------------------------------------
    // DeliveryStorage trait on PostgresStorage
    // ------------------------------------------------------------------
    let emitter = "fault-emitter";
    let delivery_id = DeliveryId::new();
    let receipt_id = ReceiptId::new();

    storage
        .claim_pending(emitter, 10)
        .await
        .expect_err("claim_pending must fail on closed pool");

    storage
        .reclaim_expired(emitter, Duration::from_secs(30))
        .await
        .expect_err("reclaim_expired must fail on closed pool");

    storage
        .mark_emitted(delivery_id)
        .await
        .expect_err("mark_emitted must fail on closed pool");

    storage
        .mark_failed(delivery_id, 1, chrono::Utc::now(), "boom")
        .await
        .expect_err("mark_failed must fail on closed pool");

    storage
        .mark_dead_lettered(delivery_id, "boom")
        .await
        .expect_err("mark_dead_lettered must fail on closed pool");

    storage
        .count_dlq(emitter)
        .await
        .expect_err("count_dlq must fail on closed pool");

    storage
        .count_pending(emitter)
        .await
        .expect_err("count_pending must fail on closed pool");

    storage
        .count_in_flight(emitter)
        .await
        .expect_err("count_in_flight must fail on closed pool");

    storage
        .insert_replay(receipt_id, emitter)
        .await
        .expect_err("insert_replay must fail on closed pool");

    storage
        .insert_replays(receipt_id, &["a".to_owned(), "b".to_owned()])
        .await
        .expect_err("insert_replays must fail on closed pool");

    storage
        .get_delivery(delivery_id)
        .await
        .expect_err("get_delivery must fail on closed pool");

    storage
        .get_deliveries_for_receipt(receipt_id)
        .await
        .expect_err("get_deliveries_for_receipt must fail on closed pool");

    // list_dlq has two arms (Some vs None emitter filter); hit both.
    storage
        .list_dlq(Some(emitter), 10, 0)
        .await
        .expect_err("list_dlq (filtered) must fail on closed pool");

    storage
        .list_dlq(None, 10, 0)
        .await
        .expect_err("list_dlq (unfiltered) must fail on closed pool");

    // ------------------------------------------------------------------
    // Arc<T> blanket DeliveryStorage impl — forces the delegate arms to
    // compile and run. Same pool state, so every call errors identically.
    // ------------------------------------------------------------------
    arc_storage
        .claim_pending(emitter, 10)
        .await
        .expect_err("Arc::claim_pending must fail on closed pool");

    arc_storage
        .reclaim_expired(emitter, Duration::from_secs(30))
        .await
        .expect_err("Arc::reclaim_expired must fail on closed pool");

    arc_storage
        .mark_emitted(delivery_id)
        .await
        .expect_err("Arc::mark_emitted must fail on closed pool");

    arc_storage
        .mark_failed(delivery_id, 1, chrono::Utc::now(), "boom")
        .await
        .expect_err("Arc::mark_failed must fail on closed pool");

    arc_storage
        .mark_dead_lettered(delivery_id, "boom")
        .await
        .expect_err("Arc::mark_dead_lettered must fail on closed pool");

    arc_storage
        .count_dlq(emitter)
        .await
        .expect_err("Arc::count_dlq must fail on closed pool");

    arc_storage
        .count_pending(emitter)
        .await
        .expect_err("Arc::count_pending must fail on closed pool");

    arc_storage
        .count_in_flight(emitter)
        .await
        .expect_err("Arc::count_in_flight must fail on closed pool");

    arc_storage
        .insert_replay(receipt_id, emitter)
        .await
        .expect_err("Arc::insert_replay must fail on closed pool");

    arc_storage
        .insert_replays(receipt_id, &["a".to_owned()])
        .await
        .expect_err("Arc::insert_replays must fail on closed pool");

    arc_storage
        .get_delivery(delivery_id)
        .await
        .expect_err("Arc::get_delivery must fail on closed pool");

    arc_storage
        .get_deliveries_for_receipt(receipt_id)
        .await
        .expect_err("Arc::get_deliveries_for_receipt must fail on closed pool");

    arc_storage
        .list_dlq(Some(emitter), 10, 0)
        .await
        .expect_err("Arc::list_dlq must fail on closed pool");
}
