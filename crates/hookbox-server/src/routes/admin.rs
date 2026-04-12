//! Admin API endpoints for receipt management and replay.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use hookbox::state::WebhookDelivery;
use hookbox::traits::{DedupeStrategy, Storage};
use hookbox::types::{ReceiptFilter, WebhookReceipt};
use hookbox::{ProcessingState, receipt_aggregate_state, receipt_deliveries_summary};
use hookbox_postgres::DeliveryStorage;

use crate::AppState;

/// Build a `serde_json::Value` for a receipt list item by serialising the base
/// receipt, then overriding `processing_state` with the derived value and
/// inserting a `deliveries_summary` map.
fn receipt_list_item_json(
    receipt: &WebhookReceipt,
    deliveries: &[WebhookDelivery],
) -> serde_json::Value {
    let derived_state = receipt_aggregate_state(deliveries, receipt.processing_state);
    let summary = receipt_deliveries_summary(deliveries);
    let mut val = serde_json::to_value(receipt).unwrap_or(serde_json::Value::Null);
    if let serde_json::Value::Object(ref mut map) = val {
        map.insert(
            "processing_state".to_owned(),
            serde_json::to_value(derived_state).unwrap_or(serde_json::Value::Null),
        );
        map.insert(
            "deliveries_summary".to_owned(),
            serde_json::to_value(&summary).unwrap_or(serde_json::Value::Null),
        );
    }
    val
}

/// Build a `serde_json::Value` for a receipt detail by serialising the base
/// receipt, then overriding `processing_state`, inserting `deliveries_summary`,
/// and embedding the `deliveries` array.
fn receipt_detail_json(
    receipt: &WebhookReceipt,
    deliveries: &[WebhookDelivery],
) -> serde_json::Value {
    let derived_state = receipt_aggregate_state(deliveries, receipt.processing_state);
    let summary = receipt_deliveries_summary(deliveries);
    let mut val = serde_json::to_value(receipt).unwrap_or(serde_json::Value::Null);
    if let serde_json::Value::Object(ref mut map) = val {
        map.insert(
            "processing_state".to_owned(),
            serde_json::to_value(derived_state).unwrap_or(serde_json::Value::Null),
        );
        map.insert(
            "deliveries_summary".to_owned(),
            serde_json::to_value(&summary).unwrap_or(serde_json::Value::Null),
        );
        map.insert(
            "deliveries".to_owned(),
            serde_json::to_value(deliveries).unwrap_or(serde_json::Value::Array(Vec::new())),
        );
    }
    val
}

// Note: the helper functions above use `serde_json::to_value(...).unwrap_or(...)` which
// cannot actually fail for well-formed domain types — the unwrap_or branches are defensive
// fallbacks and are not reachable in practice.  This is acceptable in handler code.

/// Check the `Authorization` header against the configured admin bearer token.
///
/// If no token is configured, all requests are allowed through.
fn check_auth<S: Storage, D: DedupeStrategy>(
    state: &AppState<S, D>,
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
///
/// Returns each receipt with a derived `processing_state` (aggregated from
/// delivery rows) and a per-emitter `deliveries_summary` map.
pub async fn list_receipts<S, D>(
    State(state): State<Arc<AppState<S, D>>>,
    headers: HeaderMap,
    Query(params): Query<ListReceiptsQuery>,
) -> impl IntoResponse
where
    S: Storage + DeliveryStorage + 'static,
    D: DedupeStrategy + 'static,
{
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

    let receipts = match state.pipeline.storage().query(filter).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "failed to list receipts");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let mut items: Vec<serde_json::Value> = Vec::with_capacity(receipts.len());
    for receipt in receipts {
        // TODO(perf): batch-load deliveries for all receipts in a single query.
        let deliveries = match state
            .pipeline
            .storage()
            .get_deliveries_for_receipt(receipt.receipt_id)
            .await
        {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(error = %e, receipt_id = %receipt.receipt_id, "failed to load deliveries");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": e.to_string()})),
                )
                    .into_response();
            }
        };
        items.push(receipt_list_item_json(&receipt, &deliveries));
    }

    (StatusCode::OK, Json(json!(items))).into_response()
}

/// Get a single webhook receipt by ID.
///
/// Returns the receipt with a derived `processing_state`, a per-emitter
/// `deliveries_summary` map, and an embedded `deliveries` array sorted by
/// `created_at` ASC.
pub async fn get_receipt<S, D>(
    State(state): State<Arc<AppState<S, D>>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse
where
    S: Storage + DeliveryStorage + 'static,
    D: DedupeStrategy + 'static,
{
    if let Err(resp) = check_auth(&state, &headers) {
        return resp.into_response();
    }
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
            tracing::error!(error = %e, %id, "failed to get receipt");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let mut deliveries = match state
        .pipeline
        .storage()
        .get_deliveries_for_receipt(receipt.receipt_id)
        .await
    {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, %id, "failed to load deliveries for receipt");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    // Sort deliveries by created_at ASC for stable, chronological output.
    deliveries.sort_by_key(|d| d.created_at);

    let detail = receipt_detail_json(&receipt, &deliveries);

    (StatusCode::OK, Json(json!(detail))).into_response()
}

/// Replay a previously stored receipt by transitioning it to the `Replayed`
/// state.
///
/// Inline emission has been removed in the Phase 6 fan-out refactor.
/// Downstream delivery is now handled asynchronously by `EmitterWorker`
/// (Phase 7).  This endpoint marks the receipt as replayed so that the
/// worker will re-queue it on its next poll cycle.
pub async fn replay_receipt<S: Storage, D: DedupeStrategy>(
    State(state): State<Arc<AppState<S, D>>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if let Err(resp) = check_auth(&state, &headers) {
        return resp.into_response();
    }
    // Verify the receipt exists before transitioning.
    match state.pipeline.storage().get(id).await {
        Ok(Some(_)) => {}
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
pub async fn list_dlq<S: Storage, D: DedupeStrategy>(
    State(state): State<Arc<AppState<S, D>>>,
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
