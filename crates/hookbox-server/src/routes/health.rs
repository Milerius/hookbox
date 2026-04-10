//! Health-check endpoints.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::AppState;

/// Liveness probe — always returns `200 OK`.
pub async fn healthz() -> StatusCode {
    StatusCode::OK
}

/// Readiness probe — returns `200 OK` if the database is reachable,
/// `503 Service Unavailable` otherwise.
pub async fn readyz(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match sqlx::query("SELECT 1").execute(&state.pool).await {
        Ok(_) => StatusCode::OK,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}
