//! Full-stack HTTP integration tests for the hookbox Axum server.
//!
//! Requires a running Postgres instance.
//! Set `DATABASE_URL` or defaults to `postgres://localhost/hookbox_test`

#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "expect/unwrap are acceptable in test code"
)]

use std::sync::Arc;

use hookbox::HookboxPipeline;
use hookbox::dedupe::{InMemoryRecentDedupe, LayeredDedupe};
use hookbox::emitter::ChannelEmitter;
use hookbox_postgres::{PostgresStorage, StorageDedupe};
use hookbox_server::AppState;
use hookbox_server::build_router;
use sqlx::PgPool;
use tokio::net::TcpListener;

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

async fn drain(mut rx: tokio::sync::mpsc::Receiver<hookbox::NormalizedEvent>) {
    while rx.recv().await.is_some() {}
}

#[tokio::test]
async fn full_http_flow() {
    // Setup
    let pool = setup_pool().await;
    let storage = PostgresStorage::new(pool.clone());
    let (emitter, rx) = ChannelEmitter::new(16);
    tokio::spawn(drain(rx));

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
        .emitter(emitter)
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
}
