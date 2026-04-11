//! Standalone Axum HTTP server for hookbox.
//!
//! Provides [`AppState`], [`build_router`], and all HTTP route handlers for
//! webhook ingestion, health checks, and administrative operations.

pub mod config;
pub mod routes;
pub mod worker;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use metrics_exporter_prometheus::PrometheusHandle;
use sqlx::PgPool;

use hookbox::dedupe::{InMemoryRecentDedupe, LayeredDedupe};
use hookbox::emitter::ChannelEmitter;
use hookbox::pipeline::HookboxPipeline;
use hookbox_postgres::{PostgresStorage, StorageDedupe};

/// Shared application state, threaded through all Axum handlers via
/// `State<Arc<AppState>>`.
pub struct AppState {
    /// The ingest pipeline wired with concrete backend types.
    pub pipeline: HookboxPipeline<
        PostgresStorage,
        LayeredDedupe<InMemoryRecentDedupe, StorageDedupe>,
        ChannelEmitter,
    >,
    /// Database connection pool, used independently for health checks.
    pub pool: PgPool,
    /// Optional bearer token for admin API authentication.
    pub admin_token: Option<String>,
    /// Optional Prometheus metrics handle for the `/metrics` scrape endpoint.
    pub prometheus: Option<PrometheusHandle>,
}

/// Build the Axum [`Router`] with all hookbox routes wired to the given state.
///
/// `body_limit` sets the maximum request body size in bytes via
/// [`DefaultBodyLimit::max`].
pub fn build_router(state: Arc<AppState>, body_limit: usize) -> Router {
    Router::new()
        .route("/webhooks/{provider}", post(routes::ingest::ingest_webhook))
        .route("/healthz", get(routes::health::healthz))
        .route("/readyz", get(routes::health::readyz))
        .route("/api/receipts", get(routes::admin::list_receipts))
        .route("/api/receipts/{id}", get(routes::admin::get_receipt))
        .route(
            "/api/receipts/{id}/replay",
            post(routes::admin::replay_receipt),
        )
        .route("/api/dlq", get(routes::admin::list_dlq))
        .route("/metrics", get(routes::health::metrics))
        .with_state(state)
        .layer(DefaultBodyLimit::max(body_limit))
}
