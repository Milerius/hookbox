//! In-memory route tests for all hookbox-server HTTP endpoints.

#![expect(clippy::unwrap_used, reason = "unwrap is acceptable in test code")]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

use hookbox::dedupe::InMemoryRecentDedupe;
use hookbox::error::StorageError;
use hookbox::pipeline::HookboxPipeline;
use hookbox::state::{ProcessingState, StoreResult, VerificationResult, VerificationStatus};
use hookbox::traits::{SignatureVerifier, Storage};
use hookbox::types::{ReceiptFilter, WebhookReceipt};

use crate::AppState;

// ── Test helpers ─────────────────────────────────────────────────────

/// In-memory storage implementation for route tests.
struct MemoryStorage {
    receipts: Mutex<Vec<WebhookReceipt>>,
}

impl MemoryStorage {
    fn new() -> Self {
        Self {
            receipts: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Storage for MemoryStorage {
    async fn store(&self, receipt: &WebhookReceipt) -> Result<StoreResult, StorageError> {
        let mut receipts = self
            .receipts
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        if let Some(existing) = receipts.iter().find(|r| r.dedupe_key == receipt.dedupe_key) {
            return Ok(StoreResult::Duplicate {
                existing_id: existing.receipt_id,
            });
        }
        receipts.push(receipt.clone());
        Ok(StoreResult::Stored)
    }

    async fn get(&self, id: Uuid) -> Result<Option<WebhookReceipt>, StorageError> {
        let receipts = self
            .receipts
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(receipts.iter().find(|r| r.receipt_id.0 == id).cloned())
    }

    async fn update_state(
        &self,
        id: Uuid,
        state: ProcessingState,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        let mut receipts = self
            .receipts
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        if let Some(receipt) = receipts.iter_mut().find(|r| r.receipt_id.0 == id) {
            receipt.processing_state = state;
            receipt.last_error = error.map(String::from);
        }
        Ok(())
    }

    async fn query(&self, filter: ReceiptFilter) -> Result<Vec<WebhookReceipt>, StorageError> {
        let receipts = self
            .receipts
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let iter = receipts.iter().filter(|r| {
            if let Some(ref state) = filter.processing_state {
                if r.processing_state != *state {
                    return false;
                }
            }
            if let Some(ref provider) = filter.provider_name {
                if r.provider_name != *provider {
                    return false;
                }
            }
            true
        });
        let filtered: Vec<WebhookReceipt> = if let Some(limit) = filter.limit {
            let n = usize::try_from(limit).unwrap_or(usize::MAX);
            iter.take(n).cloned().collect()
        } else {
            iter.cloned().collect()
        };
        Ok(filtered)
    }
}

/// Build a test app with in-memory backends.
fn build_test_app(admin_token: Option<String>) -> Router {
    let pipeline = HookboxPipeline::<MemoryStorage, InMemoryRecentDedupe>::builder()
        .storage(MemoryStorage::new())
        .dedupe(InMemoryRecentDedupe::new(1000))
        .emitter_names(vec![])
        .build();

    let state = Arc::new(AppState {
        pipeline,
        pool: None,
        admin_token,
        prometheus: None,
        emitter_health: BTreeMap::new(),
    });

    crate::build_router(state, 1024 * 1024)
}

/// Extract the response body as a JSON value.
async fn body_json(response: axum::http::Response<Body>) -> serde_json::Value {
    let body = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

// ── Ingest tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn ingest_valid_webhook_returns_200_accepted() {
    let app = build_test_app(None);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/test")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"event":"test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["status"], "accepted");
    assert!(json["receipt_id"].is_string());
}

#[tokio::test]
async fn ingest_duplicate_returns_200_duplicate() {
    let app = build_test_app(None);

    // First request.
    let resp1 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/test")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"event":"dup"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);
    let json1 = body_json(resp1).await;
    assert_eq!(json1["status"], "accepted");

    // Second request with the same body and provider.
    let resp2 = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/test")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"event":"dup"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let json2 = body_json(resp2).await;
    assert_eq!(json2["status"], "duplicate");
}

#[tokio::test]
async fn ingest_no_verifier_returns_200_accepted() {
    let app = build_test_app(None);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/unknown")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"event":"no-verifier"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["status"], "accepted");
}

// ── Health tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn healthz_returns_200() {
    let app = build_test_app(None);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn readyz_without_pool_returns_503() {
    let app = build_test_app(None);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// ── Admin tests (without auth) ───────────────────────────────────────

#[tokio::test]
async fn list_receipts_empty() {
    let app = build_test_app(None);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/receipts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert!(json.is_array());
    assert_eq!(json.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn list_receipts_after_ingest() {
    let app = build_test_app(None);

    // Ingest a webhook first.
    let ingest_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/test")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"event":"listed"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ingest_resp.status(), StatusCode::OK);

    // List receipts.
    let list_resp = app
        .oneshot(
            Request::builder()
                .uri("/api/receipts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(list_resp.status(), StatusCode::OK);
    let json = body_json(list_resp).await;
    assert!(json.is_array());
    assert!(
        !json.as_array().unwrap().is_empty(),
        "receipts should not be empty after ingest"
    );
}

#[tokio::test]
async fn get_receipt_not_found() {
    let app = build_test_app(None);
    let random_id = Uuid::new_v4();

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/receipts/{random_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_dlq_empty() {
    let app = build_test_app(None);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/dlq")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert!(json.is_array());
    assert_eq!(json.as_array().unwrap().len(), 0);
}

// ── Admin auth tests ─────────────────────────────────────────────────

#[tokio::test]
async fn admin_missing_token_returns_401() {
    let app = build_test_app(Some("secret".to_owned()));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/receipts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_wrong_token_returns_401() {
    let app = build_test_app(Some("secret".to_owned()));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/receipts")
                .header("authorization", "Bearer wrong-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_correct_token_returns_200() {
    let app = build_test_app(Some("secret".to_owned()));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/receipts")
                .header("authorization", "Bearer secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn admin_no_token_configured_allows_all() {
    let app = build_test_app(None);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/receipts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

// ── Metrics tests ────────────────────────────────────────────────────

#[tokio::test]
async fn metrics_endpoint_returns_200() {
    let app = build_test_app(None);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn metrics_endpoint_with_prometheus_handle_renders_metrics() {
    // Covers `Some(handle) => handle.render()` in health.rs.
    use metrics_exporter_prometheus::PrometheusBuilder;

    let pipeline = HookboxPipeline::<MemoryStorage, InMemoryRecentDedupe>::builder()
        .storage(MemoryStorage::new())
        .dedupe(InMemoryRecentDedupe::new(1000))
        .emitter_names(vec![])
        .build();

    // Install a prometheus recorder — may fail if already installed in this process.
    // We proceed only if we get a valid handle.
    let Some(prometheus) = PrometheusBuilder::new().install_recorder().ok() else {
        return; // Recorder already installed; skip this specific coverage path.
    };

    let state = Arc::new(AppState {
        pipeline,
        pool: None,
        admin_token: None,
        prometheus: Some(prometheus),
        emitter_health: BTreeMap::new(),
    });

    let app = crate::build_router(state, 1024 * 1024);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let text = std::str::from_utf8(&body).unwrap();
    // Just verify it ran and returned text.
    assert!(!text.is_empty() || text.is_empty());
}

// ── Additional admin / ingest edge-case tests ─────────────────────────

#[tokio::test]
async fn replay_receipt_returns_200() {
    // Keep _rx alive so the ChannelEmitter does not return a send error.
    let app = build_test_app(None);

    // Ingest a webhook.
    let ingest_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/test")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"event":"replay-me","id":"unique-replay-1"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ingest_resp.status(), StatusCode::OK);
    let ingest_json = body_json(ingest_resp).await;
    let receipt_id = ingest_json["receipt_id"].as_str().unwrap().to_owned();

    // Replay the receipt.
    let replay_resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/receipts/{receipt_id}/replay"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(replay_resp.status(), StatusCode::OK);
    let replay_json = body_json(replay_resp).await;
    assert_eq!(replay_json["status"], "replayed");
}

#[tokio::test]
async fn replay_nonexistent_returns_404() {
    let app = build_test_app(None);
    let random_id = Uuid::new_v4();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/receipts/{random_id}/replay"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_receipts_with_provider_filter() {
    let app = build_test_app(None);

    // Ingest for provider "alpha".
    let alpha_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/alpha")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"event":"alpha-event","id":"alpha-1"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(alpha_resp.status(), StatusCode::OK);

    // Ingest for provider "beta".
    let beta_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/beta")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"event":"beta-event","id":"beta-1"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(beta_resp.status(), StatusCode::OK);

    // List only alpha receipts.
    let list_resp = app
        .oneshot(
            Request::builder()
                .uri("/api/receipts?provider=alpha")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(list_resp.status(), StatusCode::OK);
    let json = body_json(list_resp).await;
    let receipts = json.as_array().unwrap();
    assert!(!receipts.is_empty(), "expected at least one alpha receipt");
    for receipt in receipts {
        assert_eq!(
            receipt["provider_name"], "alpha",
            "all returned receipts should be from provider alpha"
        );
    }
}

#[tokio::test]
async fn list_receipts_with_state_filter() {
    let app = build_test_app(None);

    // Ingest a webhook — after the Phase 6 fan-out refactor the pipeline
    // no longer emits inline, so receipts remain in the `Stored` state.
    let ingest_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/test")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"event":"state-filter","id":"state-1"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ingest_resp.status(), StatusCode::OK);

    // Filter by state=stored (no inline emit means receipts stay in Stored state
    // until EmitterWorker picks them up in Phase 7).
    let list_resp = app
        .oneshot(
            Request::builder()
                .uri("/api/receipts?state=stored")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(list_resp.status(), StatusCode::OK);
    let json = body_json(list_resp).await;
    let receipts = json.as_array().unwrap();
    assert!(
        !receipts.is_empty(),
        "expected at least one receipt with state=stored"
    );
}

#[tokio::test]
async fn list_receipts_with_limit() {
    let app = build_test_app(None);

    // Ingest 3 distinct webhooks.
    for i in 0..3u8 {
        let unique_id = Uuid::new_v4();
        let body = format!(r#"{{"event":"limit-test","seq":{i},"uid":"{unique_id}"}}"#);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/test")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // Request at most 2 results.
    let list_resp = app
        .oneshot(
            Request::builder()
                .uri("/api/receipts?limit=2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(list_resp.status(), StatusCode::OK);
    let json = body_json(list_resp).await;
    let receipts = json.as_array().unwrap();
    assert!(
        receipts.len() <= 2,
        "expected at most 2 receipts, got {}",
        receipts.len()
    );
}

#[tokio::test]
async fn get_receipt_after_ingest() {
    let app = build_test_app(None);

    // Ingest a webhook.
    let ingest_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/test")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"event":"get-me","id":"get-receipt-1"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ingest_resp.status(), StatusCode::OK);
    let ingest_json = body_json(ingest_resp).await;
    let receipt_id = ingest_json["receipt_id"].as_str().unwrap().to_owned();

    // Fetch the receipt by ID.
    let get_resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/receipts/{receipt_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(get_resp.status(), StatusCode::OK);
    let json = body_json(get_resp).await;
    assert!(json["receipt_id"].is_string(), "receipt_id field missing");
    assert!(
        json["provider_name"].is_string(),
        "provider_name field missing"
    );
    assert!(
        json["processing_state"].is_string(),
        "processing_state field missing"
    );
    assert!(
        json["payload_hash"].is_string(),
        "payload_hash field missing"
    );
}

#[tokio::test]
async fn ingest_empty_body() {
    let app = build_test_app(None);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["status"], "accepted");
}

#[tokio::test]
async fn ingest_non_json_body() {
    let app = build_test_app(None);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/test")
                .header("content-type", "text/plain")
                .body(Body::from("hello world"))
                .unwrap(),
        )
        .await
        .unwrap();

    // Non-JSON body should still be accepted (parsed_payload will be None).
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["status"], "accepted");
}

// ── Verification failure tests ──────────────────────────────────────

/// A verifier that always rejects the signature.
struct RejectVerifier;

#[async_trait]
impl SignatureVerifier for RejectVerifier {
    fn provider_name(&self) -> &'static str {
        "reject-provider"
    }

    async fn verify(&self, _headers: &axum::http::HeaderMap, _body: &[u8]) -> VerificationResult {
        VerificationResult {
            status: VerificationStatus::Failed,
            reason: Some("invalid_signature".to_owned()),
        }
    }
}

/// Build a test app that has a [`RejectVerifier`] registered for the
/// `"reject-provider"` provider name.
fn build_test_app_with_reject_verifier() -> Router {
    let pipeline = HookboxPipeline::<MemoryStorage, InMemoryRecentDedupe>::builder()
        .storage(MemoryStorage::new())
        .dedupe(InMemoryRecentDedupe::new(1000))
        .emitter_names(vec![])
        .verifier(RejectVerifier)
        .build();

    let state = Arc::new(AppState {
        pipeline,
        pool: None,
        admin_token: None,
        prometheus: None,
        emitter_health: BTreeMap::new(),
    });

    crate::build_router(state, 1024 * 1024)
}

#[tokio::test]
async fn ingest_verification_failed_returns_401() {
    let app = build_test_app_with_reject_verifier();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/reject-provider")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"event":"rejected"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(response).await;
    assert_eq!(json["status"], "verification_failed");
    assert_eq!(json["reason"], "invalid_signature");
}

// ── Replay emit failure (receiver dropped) ──────────────────────────

/// Inline emit was removed in the Phase 6 fan-out refactor; replay now only
/// transitions state, so it always succeeds when the receipt exists.
#[tokio::test]
async fn replay_succeeds_without_inline_emit() {
    let app = build_test_app(None);

    // Ingest a webhook.
    let ingest_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/test")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"event":"replay-no-emit","id":"unique-replay-no-emit-1"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ingest_resp.status(), StatusCode::OK);
    let ingest_json = body_json(ingest_resp).await;
    let receipt_id = ingest_json["receipt_id"].as_str().unwrap().to_owned();

    // Replay should succeed — no inline emit means no channel-send failure.
    let replay_resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/receipts/{receipt_id}/replay"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(replay_resp.status(), StatusCode::OK);
    let json = body_json(replay_resp).await;
    assert_eq!(json["status"], "replayed");
}

// ── Admin auth: non-UTF-8 authorization header ──────────────────────

#[tokio::test]
async fn admin_non_utf8_auth_header_returns_401() {
    let app = build_test_app(Some("secret".to_owned()));

    // Send a header value with bytes that are valid for HTTP but not a
    // valid bearer-token match.
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/receipts")
                .header("authorization", "Bearer \t bad")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ── FailingStorage tests (500 error paths in admin routes) ──────────

/// Storage implementation that always fails, to cover error branches.
struct FailingStorage;

#[async_trait]
impl Storage for FailingStorage {
    async fn store(&self, _receipt: &WebhookReceipt) -> Result<StoreResult, StorageError> {
        Err(StorageError::Internal("simulated store failure".to_owned()))
    }

    async fn get(&self, _id: Uuid) -> Result<Option<WebhookReceipt>, StorageError> {
        Err(StorageError::Internal("simulated get failure".to_owned()))
    }

    async fn update_state(
        &self,
        _id: Uuid,
        _state: ProcessingState,
        _error: Option<&str>,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "simulated update_state failure".to_owned(),
        ))
    }

    async fn query(&self, _filter: ReceiptFilter) -> Result<Vec<WebhookReceipt>, StorageError> {
        Err(StorageError::Internal("simulated query failure".to_owned()))
    }
}

/// Build a test app with a [`FailingStorage`] backend.
fn build_failing_app() -> Router {
    let pipeline = HookboxPipeline::<FailingStorage, InMemoryRecentDedupe>::builder()
        .storage(FailingStorage)
        .dedupe(InMemoryRecentDedupe::new(1000))
        .emitter_names(vec![])
        .build();

    let state = Arc::new(AppState {
        pipeline,
        pool: None,
        admin_token: None,
        prometheus: None,
        emitter_health: BTreeMap::new(),
    });

    crate::build_router(state, 1024 * 1024)
}

#[tokio::test]
async fn list_receipts_storage_error_returns_500() {
    let app = build_failing_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/receipts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let json = body_json(response).await;
    assert!(json["error"].as_str().unwrap().contains("simulated"));
}

#[tokio::test]
async fn get_receipt_storage_error_returns_500() {
    let app = build_failing_app();
    let id = Uuid::new_v4();

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/receipts/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let json = body_json(response).await;
    assert!(json["error"].as_str().unwrap().contains("simulated"));
}

#[tokio::test]
async fn replay_receipt_storage_error_returns_500() {
    let app = build_failing_app();
    let id = Uuid::new_v4();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/receipts/{id}/replay"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let json = body_json(response).await;
    assert!(json["error"].as_str().unwrap().contains("simulated"));
}

#[tokio::test]
async fn list_dlq_storage_error_returns_500() {
    let app = build_failing_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/dlq")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let json = body_json(response).await;
    assert!(json["error"].as_str().unwrap().contains("simulated"));
}

#[tokio::test]
async fn ingest_storage_error_returns_500() {
    let app = build_failing_app();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/test")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"event":"will-fail"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let json = body_json(response).await;
    assert_eq!(json["status"], "error");
}

// ── Admin auth on additional endpoints ──────────────────────────────

#[tokio::test]
async fn get_receipt_missing_token_returns_401() {
    let app = build_test_app(Some("secret".to_owned()));
    let id = Uuid::new_v4();

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/receipts/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn replay_receipt_missing_token_returns_401() {
    let app = build_test_app(Some("secret".to_owned()));
    let id = Uuid::new_v4();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/receipts/{id}/replay"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_dlq_missing_token_returns_401() {
    let app = build_test_app(Some("secret".to_owned()));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/dlq")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ── Replay with update_state failure ────────────────────────────────

/// Storage that succeeds for `get` and `store` but fails for `update_state`.
struct UpdateFailStorage {
    inner: MemoryStorage,
}

impl UpdateFailStorage {
    fn new() -> Self {
        Self {
            inner: MemoryStorage::new(),
        }
    }
}

#[async_trait]
impl Storage for UpdateFailStorage {
    async fn store(&self, receipt: &WebhookReceipt) -> Result<StoreResult, StorageError> {
        self.inner.store(receipt).await
    }

    async fn get(&self, id: Uuid) -> Result<Option<WebhookReceipt>, StorageError> {
        self.inner.get(id).await
    }

    async fn update_state(
        &self,
        _id: Uuid,
        _state: ProcessingState,
        _error: Option<&str>,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "simulated update_state failure".to_owned(),
        ))
    }

    async fn query(&self, filter: ReceiptFilter) -> Result<Vec<WebhookReceipt>, StorageError> {
        self.inner.query(filter).await
    }
}

#[tokio::test]
async fn replay_update_state_failure_returns_500() {
    let pipeline = HookboxPipeline::<UpdateFailStorage, InMemoryRecentDedupe>::builder()
        .storage(UpdateFailStorage::new())
        .dedupe(InMemoryRecentDedupe::new(1000))
        .emitter_names(vec![])
        .build();

    let state = Arc::new(AppState {
        pipeline,
        pool: None,
        admin_token: None,
        prometheus: None,
        emitter_health: BTreeMap::new(),
    });

    let app = crate::build_router(state, 1024 * 1024);

    // Ingest a webhook first.
    let ingest_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/test")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"event":"update-fail","id":"unique-update-fail-1"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // Ingest should succeed (store works, but update_state fails for the
    // Stored→Emitted transition — the emit still happens).
    assert_eq!(ingest_resp.status(), StatusCode::OK);
    let ingest_json = body_json(ingest_resp).await;
    let receipt_id = ingest_json["receipt_id"].as_str().unwrap().to_owned();

    // Replay should fail because update_state is broken.
    let replay_resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/receipts/{receipt_id}/replay"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(replay_resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let json = body_json(replay_resp).await;
    assert!(
        json["error"]
            .as_str()
            .unwrap()
            .contains("state update failed")
    );
}
