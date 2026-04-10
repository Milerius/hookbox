//! Admin API endpoints for receipt management and replay.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use hookbox::ProcessingState;
use hookbox::traits::{Emitter, Storage};
use hookbox::types::{NormalizedEvent, ReceiptFilter};

use crate::AppState;

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
pub async fn list_receipts(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListReceiptsQuery>,
) -> impl IntoResponse {
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
pub async fn get_receipt(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
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
pub async fn replay_receipt(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
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
pub async fn list_dlq(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListReceiptsQuery>,
) -> impl IntoResponse {
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
