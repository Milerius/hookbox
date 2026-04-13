//! Integration tests for the admin API endpoints that derive receipt state from
//! delivery rows (`GET /api/receipts` and `GET /api/receipts/:id`).
//!
//! Run with: `cargo nextest run -p hookbox-integration-tests -- admin_api`

#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "expect/unwrap are acceptable in test code"
)]

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

// ── Task 18: GET /api/deliveries/:id ─────────────────────────────────────────

/// `GET /api/deliveries/:id` returns 200 with `{"delivery": ..., "receipt": ...}`.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn get_delivery_returns_delivery_plus_receipt(pool: sqlx::PgPool) {
    let receipt_id = Uuid::new_v4();
    let delivery_id = Uuid::new_v4();
    seed_receipt(&pool, receipt_id).await;
    seed_delivery(
        &pool,
        delivery_id,
        receipt_id,
        "test-emitter",
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
                .uri(format!("/api/deliveries/{delivery_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(
        body["delivery"]["delivery_id"].as_str().unwrap(),
        delivery_id.to_string(),
        "delivery_id must match"
    );
    assert_eq!(
        body["receipt"]["receipt_id"].as_str().unwrap(),
        receipt_id.to_string(),
        "receipt_id must match"
    );
}

/// `GET /api/deliveries/:id` returns 404 for an unknown delivery ID.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn get_delivery_unknown_id_returns_404(pool: sqlx::PgPool) {
    let app = build_test_router(pool);
    let unknown_id = Uuid::new_v4();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/deliveries/{unknown_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert!(
        body["error"].as_str().unwrap().contains("not found"),
        "error message must mention 'not found'"
    );
}

// ── Task 18: POST /api/deliveries/:id/replay ──────────────────────────────────

/// `POST /api/deliveries/:id/replay` creates a new pending delivery row and
/// returns 202 Accepted with `{"delivery_id": "<new-id>"}`.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn post_delivery_replay_creates_new_pending_row(pool: sqlx::PgPool) {
    let receipt_id = Uuid::new_v4();
    let delivery_id = Uuid::new_v4();
    seed_receipt(&pool, receipt_id).await;
    seed_delivery(
        &pool,
        delivery_id,
        receipt_id,
        "test-emitter",
        "dead_lettered",
        true,
        chrono::Utc::now(),
        Some(chrono::Utc::now()),
    )
    .await;

    // Build router with "test-emitter" configured so the endpoint accepts the replay.
    let storage = hookbox_postgres::PostgresStorage::new(pool.clone());
    let pipeline = hookbox::HookboxPipeline::builder()
        .storage(storage)
        .dedupe(hookbox::dedupe::InMemoryRecentDedupe::new(100))
        .emitter_names(vec!["test-emitter".to_owned()])
        .build();

    let state = Arc::new(hookbox_server::AppState {
        pipeline,
        pool: Some(pool.clone()),
        admin_token: None,
        prometheus: None,
        emitter_health: std::collections::BTreeMap::new(),
    });
    let app = hookbox_server::build_router(state, 1_048_576);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/deliveries/{delivery_id}/replay"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = body_json(response).await;
    let new_id_str = body["delivery_id"]
        .as_str()
        .expect("delivery_id must be present");
    // Verify the new delivery row exists in the DB.
    let new_id = Uuid::parse_str(new_id_str).expect("delivery_id must be a valid UUID");
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM webhook_deliveries WHERE delivery_id = $1 AND state = 'pending'",
    )
    .bind(new_id)
    .fetch_one(&pool)
    .await
    .expect("count query");
    assert_eq!(count, 1, "new pending delivery row must exist");
}

/// `POST /api/deliveries/:id/replay` returns 400 when the delivery's emitter
/// is not currently configured.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn post_delivery_replay_rejects_legacy_emitter_name(pool: sqlx::PgPool) {
    let receipt_id = Uuid::new_v4();
    let delivery_id = Uuid::new_v4();
    seed_receipt(&pool, receipt_id).await;
    seed_delivery(
        &pool,
        delivery_id,
        receipt_id,
        "legacy-emitter",
        "dead_lettered",
        true,
        chrono::Utc::now(),
        Some(chrono::Utc::now()),
    )
    .await;

    // Router configured with NO emitters, so "legacy-emitter" is unconfigured.
    let app = build_test_router(pool);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/deliveries/{delivery_id}/replay"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("not currently configured"),
        "error must mention 'not currently configured'"
    );
    assert!(
        body["hint"].as_str().is_some(),
        "hint field must be present"
    );
}

// ── Task 18: GET /api/emitters ────────────────────────────────────────────────

/// `GET /api/emitters` returns a JSON array with one entry per configured
/// emitter, including `name` and health fields.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn get_emitters_returns_all_configured_emitters_with_health(pool: sqlx::PgPool) {
    use arc_swap::ArcSwap;
    use hookbox_server::worker::{EmitterHealth, HealthStatus};

    let storage = hookbox_postgres::PostgresStorage::new(pool.clone());
    let pipeline = hookbox::HookboxPipeline::builder()
        .storage(storage)
        .dedupe(hookbox::dedupe::InMemoryRecentDedupe::new(100))
        .emitter_names(vec!["alpha".to_owned(), "beta".to_owned()])
        .build();

    let mut emitter_health = std::collections::BTreeMap::new();
    for name in ["alpha", "beta"] {
        let h = EmitterHealth {
            status: HealthStatus::Healthy,
            last_success_at: None,
            last_failure_at: None,
            consecutive_failures: 0,
            dlq_depth: 0,
            pending_count: 0,
        };
        emitter_health.insert(name.to_owned(), Arc::new(ArcSwap::from_pointee(h)));
    }

    let state = Arc::new(hookbox_server::AppState {
        pipeline,
        pool: Some(pool),
        admin_token: None,
        prometheus: None,
        emitter_health,
    });
    let app = hookbox_server::build_router(state, 1_048_576);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/emitters")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let items = body.as_array().expect("response must be an array");
    assert_eq!(items.len(), 2, "expected two emitter entries");
    // BTreeMap iteration is sorted: alpha before beta.
    assert_eq!(items[0]["name"].as_str().unwrap(), "alpha");
    assert_eq!(items[1]["name"].as_str().unwrap(), "beta");
    assert_eq!(items[0]["status"].as_str().unwrap(), "healthy");
}

// ── Task 18: POST /api/receipts/:id/replay?emitter= ───────────────────────────

/// `POST /api/receipts/:id/replay?emitter=test-emitter` creates exactly one
/// delivery row for the specified emitter and returns 202 with a single-element
/// `delivery_ids` array.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn post_receipt_replay_with_emitter_filter_creates_one_delivery(pool: sqlx::PgPool) {
    let receipt_id = Uuid::new_v4();
    seed_receipt(&pool, receipt_id).await;

    // Build router with "test-emitter" configured.
    let storage = hookbox_postgres::PostgresStorage::new(pool.clone());
    let pipeline = hookbox::HookboxPipeline::builder()
        .storage(storage)
        .dedupe(hookbox::dedupe::InMemoryRecentDedupe::new(100))
        .emitter_names(vec!["test-emitter".to_owned()])
        .build();

    let state = Arc::new(hookbox_server::AppState {
        pipeline,
        pool: Some(pool.clone()),
        admin_token: None,
        prometheus: None,
        emitter_health: std::collections::BTreeMap::new(),
    });
    let app = hookbox_server::build_router(state, 1_048_576);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/receipts/{receipt_id}/replay?emitter=test-emitter"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = body_json(response).await;
    let ids = body["delivery_ids"]
        .as_array()
        .expect("delivery_ids must be an array");
    assert_eq!(ids.len(), 1, "exactly one delivery_id expected");

    // Verify the pending row exists in DB.
    let new_id =
        Uuid::parse_str(ids[0].as_str().unwrap()).expect("delivery_id must be a valid UUID");
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM webhook_deliveries WHERE delivery_id = $1 AND state = 'pending'",
    )
    .bind(new_id)
    .fetch_one(&pool)
    .await
    .expect("count query");
    assert_eq!(count, 1, "pending delivery row must exist in DB");
}

// ── Task 18: GET /api/dlq?emitter= ───────────────────────────────────────────

/// `GET /api/dlq?emitter=foo` returns only dead-lettered deliveries for emitter
/// "foo", excluding those for other emitters.
#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn get_dlq_with_emitter_filter_returns_only_matching(pool: sqlx::PgPool) {
    let receipt_id = Uuid::new_v4();
    let foo_delivery_id = Uuid::new_v4();
    let bar_delivery_id = Uuid::new_v4();

    seed_receipt(&pool, receipt_id).await;
    seed_delivery(
        &pool,
        foo_delivery_id,
        receipt_id,
        "foo",
        "dead_lettered",
        true,
        chrono::Utc::now(),
        Some(chrono::Utc::now()),
    )
    .await;
    seed_delivery(
        &pool,
        bar_delivery_id,
        receipt_id,
        "bar",
        "dead_lettered",
        true,
        chrono::Utc::now(),
        Some(chrono::Utc::now()),
    )
    .await;

    let app = build_test_router(pool);
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/dlq?emitter=foo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let items = body.as_array().expect("response must be an array");
    assert_eq!(items.len(), 1, "only foo delivery should be returned");
    assert_eq!(
        items[0]["delivery"]["emitter_name"].as_str().unwrap(),
        "foo",
        "returned delivery must be for emitter 'foo'"
    );
    assert_eq!(
        items[0]["delivery"]["delivery_id"].as_str().unwrap(),
        foo_delivery_id.to_string(),
        "returned delivery_id must match the foo delivery"
    );
    // Verify receipt is also embedded.
    assert_eq!(
        items[0]["receipt"]["receipt_id"].as_str().unwrap(),
        receipt_id.to_string(),
        "receipt must be embedded"
    );
}
