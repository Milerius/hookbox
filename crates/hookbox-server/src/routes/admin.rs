//! Admin API endpoints for receipt management and replay.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use hookbox::state::{DeliveryId, WebhookDelivery};
use hookbox::traits::{DedupeStrategy, Storage};
use hookbox::types::{ReceiptFilter, WebhookReceipt};
use hookbox::{ProcessingState, receipt_aggregate_state, receipt_deliveries_summary};
use hookbox_postgres::DeliveryStorage;

use crate::AppState;
use crate::routes::health::EmitterHealthSnapshot;
use crate::worker::EmitterHealth;

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

/// Query parameters for the DLQ listing endpoint.
#[derive(Debug, Default, Deserialize)]
pub struct ListDlqQuery {
    /// Filter by emitter name.
    pub emitter: Option<String>,
    /// Maximum number of results to return (default: 100).
    pub limit: Option<i64>,
    /// Number of results to skip (default: 0).
    pub offset: Option<i64>,
}

/// Query parameters for replay endpoints.
#[derive(Debug, Default, Deserialize)]
pub struct ReplayReceiptQuery {
    /// Optional emitter name to replay to a specific emitter only.
    pub emitter: Option<String>,
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

/// Replay a previously stored receipt by creating new pending delivery rows.
///
/// Accepts an optional `?emitter=<name>` query parameter.  If present, a single
/// delivery row is created for that emitter; if absent, one row is created per
/// configured emitter.  Returns 202 Accepted with `{"delivery_ids": [...]}`.
///
/// The old `update_state` / `Replayed` transition has been removed; replay now
/// works by inserting new `pending` delivery rows that `EmitterWorker` picks up.
pub async fn replay_receipt<S, D>(
    State(state): State<Arc<AppState<S, D>>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(params): Query<ReplayReceiptQuery>,
) -> impl IntoResponse
where
    S: Storage + DeliveryStorage + 'static,
    D: DedupeStrategy + 'static,
{
    if let Err(resp) = check_auth(&state, &headers) {
        return resp.into_response();
    }
    // Verify the receipt exists before creating deliveries.
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

    // Determine which emitters to replay to.
    let emitter_names: Vec<String> = if let Some(ref name) = params.emitter {
        if !state.pipeline.emitter_names().contains(name) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": format!("emitter '{}' is not currently configured", name),
                    "hint": format!("use POST /api/receipts/{}/replay?emitter=<configured-name>", id)
                })),
            )
                .into_response();
        }
        vec![name.clone()]
    } else {
        state.pipeline.emitter_names().to_vec()
    };

    let mut delivery_ids: Vec<String> = Vec::with_capacity(emitter_names.len());
    for emitter_name in &emitter_names {
        match state
            .pipeline
            .storage()
            .insert_replay(receipt.receipt_id, emitter_name)
            .await
        {
            Ok(delivery_id) => delivery_ids.push(delivery_id.0.to_string()),
            Err(e) => {
                tracing::error!(error = %e, %id, emitter = %emitter_name, "failed to insert replay delivery");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": e.to_string()})),
                )
                    .into_response();
            }
        }
    }

    (
        StatusCode::ACCEPTED,
        Json(json!({"delivery_ids": delivery_ids})),
    )
        .into_response()
}

/// Get a single delivery row by ID, along with its parent receipt.
///
/// Returns `{"delivery": <WebhookDelivery>, "receipt": <WebhookReceipt>}` on
/// success, 404 if not found, or 500 on storage error.
pub async fn get_delivery<S, D>(
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
    match state.pipeline.storage().get_delivery(DeliveryId(id)).await {
        Ok(Some((delivery, receipt))) => (
            StatusCode::OK,
            Json(json!({"delivery": delivery, "receipt": receipt})),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "delivery not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, delivery_id = %id, "failed to get delivery");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    }
}

/// Replay a specific delivery by creating a new pending delivery row for the
/// same emitter.
///
/// Returns 202 Accepted with `{"delivery_id": "<new-id>"}` on success.
/// Returns 400 if the delivery's emitter is no longer configured.
/// Returns 404 if the delivery is not found.
pub async fn replay_delivery<S, D>(
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

    // Load the delivery row.
    let (delivery, _receipt) = match state.pipeline.storage().get_delivery(DeliveryId(id)).await {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "delivery not found"})),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, delivery_id = %id, "failed to fetch delivery for replay");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    // Verify the emitter is still configured.
    if !state
        .pipeline
        .emitter_names()
        .contains(&delivery.emitter_name)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("emitter '{}' is not currently configured", delivery.emitter_name),
                "hint": format!("use POST /api/receipts/{}/replay?emitter=<configured-name>", delivery.receipt_id)
            })),
        )
            .into_response();
    }

    // Insert a new pending delivery row.
    match state
        .pipeline
        .storage()
        .insert_replay(delivery.receipt_id, &delivery.emitter_name)
        .await
    {
        Ok(new_delivery_id) => (
            StatusCode::ACCEPTED,
            Json(json!({"delivery_id": new_delivery_id.0.to_string()})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, delivery_id = %id, "failed to insert replay delivery");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    }
}

/// List all configured emitters with their current health snapshot.
///
/// Returns a JSON array sorted by emitter name (`BTreeMap` iteration order).
/// Each element has `name` plus all fields from the crate-private
/// `EmitterHealthSnapshot` struct (`state`, `consecutive_failures`,
/// `last_success_at`, `last_failure_at`, `last_error`).
pub async fn list_emitters<S, D>(
    State(state): State<Arc<AppState<S, D>>>,
    headers: HeaderMap,
) -> impl IntoResponse
where
    S: Storage + DeliveryStorage + 'static,
    D: DedupeStrategy + 'static,
{
    if let Err(resp) = check_auth(&state, &headers) {
        return resp.into_response();
    }

    let items: Vec<serde_json::Value> = state
        .emitter_health
        .iter()
        .map(|(name, arc_swap)| {
            let h: EmitterHealth = (**arc_swap.load()).clone();
            let snapshot = EmitterHealthSnapshot {
                status: h.status,
                last_success_at: h.last_success_at,
                last_failure_at: h.last_failure_at,
                consecutive_failures: h.consecutive_failures,
                dlq_depth: h.dlq_depth,
                pending_count: h.pending_count,
            };
            let mut val = serde_json::to_value(&snapshot).unwrap_or(serde_json::Value::Null);
            if let serde_json::Value::Object(ref mut map) = val {
                map.insert("name".to_owned(), serde_json::Value::String(name.clone()));
            }
            val
        })
        .collect();

    (StatusCode::OK, Json(json!(items))).into_response()
}

/// List dead-lettered deliveries with optional emitter filter.
///
/// Calls `storage.list_dlq(emitter_filter, limit, offset)` and returns
/// `[{"delivery": ..., "receipt": ...}, ...]`.  Defaults to `limit = 100`,
/// `offset = 0`.
pub async fn list_dlq<S, D>(
    State(state): State<Arc<AppState<S, D>>>,
    headers: HeaderMap,
    Query(params): Query<ListDlqQuery>,
) -> impl IntoResponse
where
    S: Storage + DeliveryStorage + 'static,
    D: DedupeStrategy + 'static,
{
    if let Err(resp) = check_auth(&state, &headers) {
        return resp.into_response();
    }
    let limit = params.limit.unwrap_or(100);
    let offset = params.offset.unwrap_or(0);
    let emitter_filter = params.emitter.as_deref();

    match state
        .pipeline
        .storage()
        .list_dlq(emitter_filter, limit, offset)
        .await
    {
        Ok(pairs) => {
            let items: Vec<serde_json::Value> = pairs
                .iter()
                .map(|(delivery, receipt)| json!({"delivery": delivery, "receipt": receipt}))
                .collect();
            (StatusCode::OK, Json(json!(items))).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to list DLQ deliveries");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    }
}
