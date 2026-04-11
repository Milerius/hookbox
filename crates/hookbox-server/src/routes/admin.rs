//! Admin API endpoints for receipt management and replay.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use hookbox::ProcessingState;
use hookbox::traits::{DedupeStrategy, Emitter, Storage};
use hookbox::types::{NormalizedEvent, ReceiptFilter};

use crate::AppState;

/// Check the `Authorization` header against the configured admin bearer token.
///
/// If no token is configured, all requests are allowed through.
fn check_auth<S: Storage, D: DedupeStrategy, E: Emitter>(
    state: &AppState<S, D, E>,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let Some(ref expected) = state.admin_token else {
        return Ok(());
    };
    let Some(auth) = headers.get("authorization") else {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "missing authorization header"})),
        ));
    };
    let Ok(auth_str) = auth.to_str() else {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid authorization header"})),
        ));
    };
    let expected_value = format!("Bearer {expected}");
    if auth_str != expected_value {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid token"})),
        ));
    }
    Ok(())
}

/// Query parameters for listing receipts.
#[derive(Debug, Default, Deserialize)]
pub struct ListReceiptsQuery {
    /// Filter by provider name.
    pub provider: Option<String>,
    /// Filter by processing state.
    pub state: Option<ProcessingState>,
    /// Maximum number of results to return.
    pub limit: Option<i64>,
    /// Number of results to skip.
    pub offset: Option<i64>,
}

/// List webhook receipts with optional filters.
pub async fn list_receipts<S: Storage, D: DedupeStrategy, E: Emitter>(
    State(state): State<Arc<AppState<S, D, E>>>,
    headers: HeaderMap,
    Query(params): Query<ListReceiptsQuery>,
) -> impl IntoResponse {
    if let Err(resp) = check_auth(&state, &headers) {
        return resp.into_response();
    }
    let filter = ReceiptFilter {
        provider_name: params.provider,
        processing_state: params.state,
        limit: params.limit,
        offset: params.offset,
        ..ReceiptFilter::default()
    };

    match state.pipeline.storage().query(filter).await {
        Ok(receipts) => (StatusCode::OK, Json(json!(receipts))).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to list receipts");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    }
}

/// Get a single webhook receipt by ID.
pub async fn get_receipt<S: Storage, D: DedupeStrategy, E: Emitter>(
    State(state): State<Arc<AppState<S, D, E>>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if let Err(resp) = check_auth(&state, &headers) {
        return resp.into_response();
    }
    match state.pipeline.storage().get(id).await {
        Ok(Some(receipt)) => (StatusCode::OK, Json(json!(receipt))).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "receipt not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, %id, "failed to get receipt");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    }
}

/// Replay a previously stored receipt by re-emitting its normalised event
/// and transitioning the receipt to the `Replayed` state.
pub async fn replay_receipt<S: Storage, D: DedupeStrategy, E: Emitter>(
    State(state): State<Arc<AppState<S, D, E>>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if let Err(resp) = check_auth(&state, &headers) {
        return resp.into_response();
    }
    // Fetch the receipt.
    let receipt = match state.pipeline.storage().get(id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "receipt not found"})),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, %id, "failed to fetch receipt for replay");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    // Build normalised event from receipt.
    let event = NormalizedEvent {
        receipt_id: receipt.receipt_id,
        provider_name: receipt.provider_name.clone(),
        event_type: receipt.normalized_event_type.clone(),
        external_reference: receipt.external_reference.clone(),
        parsed_payload: receipt.parsed_payload.clone(),
        payload_hash: receipt.payload_hash.clone(),
        received_at: receipt.received_at,
        metadata: receipt.metadata.clone(),
    };

    // Emit.
    if let Err(e) = state.pipeline.emitter().emit(&event).await {
        tracing::error!(error = %e, %id, "replay emit failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("emit failed: {e}")})),
        )
            .into_response();
    }

    // Transition to Replayed.
    if let Err(e) = state
        .pipeline
        .storage()
        .update_state(id, ProcessingState::Replayed, None)
        .await
    {
        tracing::error!(error = %e, %id, "failed to update state to replayed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("state update failed: {e}")})),
        )
            .into_response();
    }

    (StatusCode::OK, Json(json!({"status": "replayed"}))).into_response()
}

/// List dead-lettered receipts (receipts in the `DeadLettered` state).
pub async fn list_dlq<S: Storage, D: DedupeStrategy, E: Emitter>(
    State(state): State<Arc<AppState<S, D, E>>>,
    headers: HeaderMap,
    Query(params): Query<ListReceiptsQuery>,
) -> impl IntoResponse {
    if let Err(resp) = check_auth(&state, &headers) {
        return resp.into_response();
    }
    let filter = ReceiptFilter {
        provider_name: params.provider,
        processing_state: Some(ProcessingState::DeadLettered),
        limit: params.limit,
        offset: params.offset,
        ..ReceiptFilter::default()
    };

    match state.pipeline.storage().query(filter).await {
        Ok(receipts) => (StatusCode::OK, Json(json!(receipts))).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to list DLQ receipts");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    }
}
