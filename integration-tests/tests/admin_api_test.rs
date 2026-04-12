//! Integration tests for the admin API endpoints that derive receipt state from
//! delivery rows (`GET /api/receipts` and `GET /api/receipts/:id`).
//!
//! Run with: `cargo nextest run -p hookbox-integration-tests -- admin_api`

#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "expect/unwrap are acceptable in test code"
)]
#![allow(clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use hookbox::HookboxPipeline;
use hookbox::dedupe::InMemoryRecentDedupe;
use hookbox_postgres::PostgresStorage;
use hookbox_server::AppState;
use hookbox_server::build_router;
use tower::ServiceExt as _;
use uuid::Uuid;

// ── Seed helpers ─────────────────────────────────────────────────────────────

async fn seed_receipt(pool: &sqlx::PgPool, receipt_id: Uuid) {
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
    pool: &sqlx::PgPool,
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

// ── Test router builder ───────────────────────────────────────────────────────

fn build_test_router(pool: sqlx::PgPool) -> axum::Router {
    let storage = PostgresStorage::new(pool.clone());
    let pipeline = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter_names(vec![])
        .build();

    let state = Arc::new(AppState {
        pipeline,
        pool: Some(pool),
        admin_token: None,
        prometheus: None,
        emitter_health: BTreeMap::new(),
    });

    build_router(state, 1_048_576)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn body_json(response: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// `GET /api/receipts` includes a `deliveries_summary` map keyed by emitter
/// name, with values reflecting each emitter's latest non-immutable state.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn get_receipts_includes_deliveries_summary(pool: sqlx::PgPool) {
    let receipt_id = Uuid::new_v4();
    seed_receipt(&pool, receipt_id).await;
    seed_delivery(
        &pool,
        Uuid::new_v4(),
        receipt_id,
        "a",
        "emitted",
        false,
        chrono::Utc::now(),
        Some(chrono::Utc::now()),
    )
    .await;
    seed_delivery(
        &pool,
        Uuid::new_v4(),
        receipt_id,
        "b",
        "failed",
        false,
        chrono::Utc::now(),
        Some(chrono::Utc::now()),
    )
    .await;

    let app = build_test_router(pool);
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/receipts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let items = body.as_array().expect("response must be an array");
    let item = items
        .iter()
        .find(|v| v["receipt_id"].as_str() == Some(&receipt_id.to_string()))
        .expect("seeded receipt must appear in list");

    assert_eq!(
        item["deliveries_summary"]["a"],
        serde_json::json!("emitted"),
        "emitter a should be emitted"
    );
    assert_eq!(
        item["deliveries_summary"]["b"],
        serde_json::json!("failed"),
        "emitter b should be failed"
    );
}

/// `GET /api/receipts/:id` includes an embedded `deliveries` array containing
/// one entry per delivery row.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn get_receipt_by_id_includes_embedded_deliveries_array(pool: sqlx::PgPool) {
    let receipt_id = Uuid::new_v4();
    seed_receipt(&pool, receipt_id).await;
    seed_delivery(
        &pool,
        Uuid::new_v4(),
        receipt_id,
        "a",
        "emitted",
        false,
        chrono::Utc::now(),
        Some(chrono::Utc::now()),
    )
    .await;
    seed_delivery(
        &pool,
        Uuid::new_v4(),
        receipt_id,
        "b",
        "pending",
        false,
        chrono::Utc::now(),
        None,
    )
    .await;

    let app = build_test_router(pool);
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/receipts/{receipt_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let deliveries = body["deliveries"]
        .as_array()
        .expect("deliveries must be an array");
    assert_eq!(deliveries.len(), 2, "expected two delivery rows");
    let emitter_names: Vec<&str> = deliveries
        .iter()
        .map(|d| {
            d["emitter_name"]
                .as_str()
                .expect("emitter_name must be a string")
        })
        .collect();
    assert!(
        emitter_names.contains(&"a"),
        "emitter 'a' must be in deliveries; got: {emitter_names:?}"
    );
    assert!(
        emitter_names.contains(&"b"),
        "emitter 'b' must be in deliveries; got: {emitter_names:?}"
    );
}

/// `GET /api/receipts/:id` returns `processing_state` derived from delivery
/// rows rather than the stored value on the receipt row.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn get_receipt_processing_state_is_derived_from_deliveries(pool: sqlx::PgPool) {
    let receipt_id = Uuid::new_v4();
    // Stored state is 'stored', but the delivery is dead_lettered.
    seed_receipt(&pool, receipt_id).await;
    seed_delivery(
        &pool,
        Uuid::new_v4(),
        receipt_id,
        "a",
        "dead_lettered",
        false,
        chrono::Utc::now(),
        Some(chrono::Utc::now()),
    )
    .await;

    let app = build_test_router(pool);
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/receipts/{receipt_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(
        body["processing_state"],
        serde_json::json!("dead_lettered"),
        "processing_state must be derived from deliveries (dead_lettered), not from stored receipt state"
    );
}
