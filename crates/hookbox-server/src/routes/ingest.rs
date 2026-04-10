//! Webhook ingest endpoint.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use bytes::Bytes;
use serde_json::json;

use hookbox::IngestResult;

use crate::AppState;

/// Receive an inbound webhook event from the named provider and run it
/// through the full ingest pipeline (verify, dedupe, store, emit).
pub async fn ingest_webhook(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    match state.pipeline.ingest(&provider, headers, body).await {
        Ok(IngestResult::Accepted { receipt_id }) => (
            StatusCode::OK,
            Json(json!({
                "status": "accepted",
                "receipt_id": receipt_id.to_string(),
            })),
        ),
        Ok(IngestResult::Duplicate { existing_id }) => (
            StatusCode::OK,
            Json(json!({
                "status": "duplicate",
                "existing_id": existing_id.to_string(),
            })),
        ),
        Ok(IngestResult::VerificationFailed { reason }) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "status": "verification_failed",
                "reason": reason,
            })),
        ),
        Err(e) => {
            tracing::error!(error = %e, provider = %provider, "ingest pipeline error");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "status": "error",
                    "reason": e.to_string(),
                })),
            )
        }
    }
}
