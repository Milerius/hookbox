//! Health-check endpoints.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use tokio::time::{Duration, timeout};

use crate::AppState;

/// Liveness probe — always returns `200 OK`.
pub async fn healthz() -> StatusCode {
    StatusCode::OK
}

/// Readiness probe — returns `200 OK` if the database is reachable within
/// 2 seconds, `503 Service Unavailable` otherwise.
pub async fn readyz(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let ping = sqlx::query("SELECT 1").execute(&state.pool);
    match timeout(Duration::from_secs(2), ping).await {
        Ok(Ok(_)) => StatusCode::OK,
        Ok(Err(_)) | Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}
