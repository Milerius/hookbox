//! Full-stack HTTP integration tests for the hookbox Axum server.
//!
//! Requires a running Postgres instance.
//! Set `DATABASE_URL` or defaults to `postgres://localhost/hookbox_test`

#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "expect/unwrap are acceptable in test code"
)]
#![allow(clippy::panic)]

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use axum::body::Body;
use hookbox::HookboxPipeline;
use hookbox::dedupe::{InMemoryRecentDedupe, LayeredDedupe};
use hookbox::state::{VerificationResult, VerificationStatus};
use hookbox::traits::SignatureVerifier;
use hookbox_postgres::{PostgresStorage, StorageDedupe};
use hookbox_server::AppState;
use hookbox_server::build_router;
use http::{HeaderMap, Request, StatusCode};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tower::ServiceExt as _;
use uuid::Uuid;

/// Global mutex so http tests do not interfere with ingest/worker tests when run
/// in the default multi-test-thread mode (specifically, serialises DB truncation).
static HTTP_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn http_test_lock() -> &'static Mutex<()> {
    HTTP_TEST_LOCK.get_or_init(|| Mutex::new(()))
}

/// Verifier that always rejects — used in the `ingest_verification_failed_returns_401` test.
struct AlwaysFailVerifier;

#[async_trait]
impl SignatureVerifier for AlwaysFailVerifier {
    fn provider_name(&self) -> &'static str {
        "reject-provider"
    }

    async fn verify(&self, _headers: &HeaderMap, _body: &[u8]) -> VerificationResult {
        VerificationResult {
            status: VerificationStatus::Failed,
            reason: Some("test_rejection".to_owned()),
        }
    }
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
async fn full_http_flow() {
    let _guard = http_test_lock().lock().await;
    // Setup
    let pool = setup_pool().await;
    let storage = PostgresStorage::new(pool.clone());

    let dedupe = LayeredDedupe::new(
        InMemoryRecentDedupe::new(100),
        StorageDedupe::new(pool.clone()),
    );

    // Install Prometheus recorder — can only succeed once per process, so use .ok()
    let prometheus = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .ok();

    let pipeline = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(dedupe)
        .emitter_names(vec![])
        .build();

    let state = Arc::new(AppState {
        pipeline,
        pool: Some(pool),
        admin_token: None,
        prometheus,
    });

    let app = build_router(state, 1_048_576);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Test 1: POST /webhooks/test → 200 accepted
    let resp = client
        .post(format!("{base}/webhooks/test"))
        .json(&serde_json::json!({"event": "payment.completed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "accepted");
    let receipt_id = body["receipt_id"].as_str().unwrap().to_owned();

    // Test 2: POST same body → 200 duplicate
    let resp = client
        .post(format!("{base}/webhooks/test"))
        .json(&serde_json::json!({"event": "payment.completed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "duplicate");

    // Test 3: GET /api/receipts → has results
    let resp = client
        .get(format!("{base}/api/receipts"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Test 4: GET /api/receipts/:id → found
    let resp = client
        .get(format!("{base}/api/receipts/{receipt_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Test 5: GET /healthz → 200
    let resp = client.get(format!("{base}/healthz")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // Test 6: GET /readyz → 200 (real pool)
    let resp = client.get(format!("{base}/readyz")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // Test 7: GET /metrics → 200 with hookbox_ metrics (or fallback if recorder not installed)
    let resp = client.get(format!("{base}/metrics")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    // Either has real metrics or the no-recorder fallback
    assert!(text.contains("hookbox_") || text.contains("no prometheus"));

    // Test 8: GET /api/dlq → 200 empty
    let resp = client.get(format!("{base}/api/dlq")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // Test 9: POST /api/receipts/:id/replay → 200 replayed
    let resp = client
        .post(format!("{base}/api/receipts/{receipt_id}/replay"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "replay should succeed");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "replayed");

    // Test 10: GET /api/receipts/:id → found (non-existent → 404)
    let fake_id = Uuid::new_v4();
    let resp = client
        .get(format!("{base}/api/receipts/{fake_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "non-existent receipt should be 404");
}

/// Test the admin auth check path with a configured token.
///
/// Covers `check_auth` branches: missing header → 401, wrong token → 401,
/// correct token → 200.
#[tokio::test]
async fn admin_auth_token_enforcement() {
    let pool = setup_pool_no_delete().await;
    let storage = PostgresStorage::new(pool.clone());

    let dedupe = LayeredDedupe::new(
        InMemoryRecentDedupe::new(100),
        StorageDedupe::new(pool.clone()),
    );

    let prometheus = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .ok();

    let pipeline = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(dedupe)
        .emitter_names(vec![])
        .build();

    let state = Arc::new(AppState {
        pipeline,
        pool: Some(pool),
        admin_token: Some("secret-token".to_owned()),
        prometheus,
    });

    let app = build_router(state, 1_048_576);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // Missing auth header → 401
    let resp = client
        .get(format!("{base}/api/receipts"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "missing auth header should be 401");

    // Wrong token → 401
    let resp = client
        .get(format!("{base}/api/receipts"))
        .header("Authorization", "Bearer wrong-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "wrong token should be 401");

    // Correct token → 200
    let resp = client
        .get(format!("{base}/api/receipts"))
        .header("Authorization", "Bearer secret-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "correct token should be 200");
}

/// Test admin auth with non-ASCII (opaque) authorization header bytes.
///
/// `reqwest` normalizes headers to valid ASCII, so we use `tower::ServiceExt`
/// to send a raw HTTP request with non-ASCII header bytes directly.
#[tokio::test]
async fn admin_auth_non_ascii_header_rejected() {
    let pool = setup_pool_no_delete().await;
    let storage = PostgresStorage::new(pool.clone());

    let dedupe = LayeredDedupe::new(
        InMemoryRecentDedupe::new(100),
        StorageDedupe::new(pool.clone()),
    );

    let prometheus = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .ok();

    let pipeline = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(dedupe)
        .emitter_names(vec![])
        .build();

    let state = Arc::new(AppState {
        pipeline,
        pool: Some(pool),
        admin_token: Some("secret".to_owned()),
        prometheus,
    });

    let app = build_router(state, 1_048_576);

    // Build a request with a non-ASCII Authorization header value (é = 0xC3 0xA9).
    // `HeaderValue::from_bytes` accepts Latin-1 but `to_str()` will reject it.
    let auth_bytes: &[u8] = &[0xC3, 0xA9]; // non-ASCII bytes
    let header_val = http::HeaderValue::from_bytes(auth_bytes).unwrap();
    let request = Request::builder()
        .method("GET")
        .uri("/api/receipts")
        .header("authorization", header_val)
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "non-ASCII auth header should be 401"
    );
}

/// Test ingest `VerificationFailed` HTTP path.
///
/// Uses a pipeline with a custom verifier that always rejects. Sends a webhook
/// to the server and expects a 401 Unauthorized response.
#[tokio::test]
async fn ingest_verification_failed_returns_401() {
    let pool = setup_pool_no_delete().await;
    let storage = PostgresStorage::new(pool.clone());

    let dedupe = LayeredDedupe::new(
        InMemoryRecentDedupe::new(100),
        StorageDedupe::new(pool.clone()),
    );

    let prometheus = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .ok();

    let pipeline = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(dedupe)
        .emitter_names(vec![])
        .verifier(AlwaysFailVerifier)
        .build();

    let state = Arc::new(AppState {
        pipeline,
        pool: Some(pool),
        admin_token: None,
        prometheus,
    });

    let app = build_router(state, 1_048_576);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    let resp = client
        .post(format!("{base}/webhooks/reject-provider"))
        .json(&serde_json::json!({"event": "test"}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401, "verification failure should return 401");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "verification_failed");
    assert_eq!(body["reason"], "test_rejection");
}

/// Test `GET /metrics` when no Prometheus recorder is installed.
///
/// Covers the `None => String::from("# no prometheus recorder installed\n")` branch.
#[tokio::test]
async fn metrics_no_recorder_returns_fallback_message() {
    let pool = setup_pool_no_delete().await;
    let storage = PostgresStorage::new(pool.clone());

    let dedupe = LayeredDedupe::new(
        InMemoryRecentDedupe::new(100),
        StorageDedupe::new(pool.clone()),
    );

    let pipeline = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(dedupe)
        .emitter_names(vec![])
        .build();

    // Explicitly set prometheus to None — do NOT install a recorder.
    let state = Arc::new(AppState {
        pipeline,
        pool: Some(pool),
        admin_token: None,
        prometheus: None,
    });

    let app = build_router(state, 1_048_576);

    let request = Request::builder()
        .method("GET")
        .uri("/metrics")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = std::str::from_utf8(&body_bytes).unwrap();
    assert!(
        body_str.contains("no prometheus"),
        "expected fallback metrics message, got: {body_str}"
    );
}

/// Test `GET /readyz` when no pool is configured — should return 503.
#[tokio::test]
async fn readyz_no_pool_returns_503() {
    let pool = setup_pool_no_delete().await;
    let storage = PostgresStorage::new(pool.clone());

    let dedupe = LayeredDedupe::new(
        InMemoryRecentDedupe::new(100),
        StorageDedupe::new(pool.clone()),
    );

    let pipeline = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(dedupe)
        .emitter_names(vec![])
        .build();

    // Explicitly set pool to None — readyz should return 503.
    let state = Arc::new(AppState {
        pipeline,
        pool: None,
        admin_token: None,
        prometheus: None,
    });

    let app = build_router(state, 1_048_576);

    let request = Request::builder()
        .method("GET")
        .uri("/readyz")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "/readyz with no pool should return 503"
    );
}

async fn setup_pool_no_delete() -> PgPool {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://localhost/hookbox_test".to_owned());
    let pool = PgPool::connect(&url).await.expect("connect to test db");
    let storage = PostgresStorage::new(pool.clone());
    storage.migrate().await.expect("run migrations");
    pool
}
