//! Standalone Axum HTTP server for hookbox.
//!
//! Provides [`AppState`], [`build_router`], and all HTTP route handlers for
//! webhook ingestion, health checks, and administrative operations.

pub mod config;
pub mod emitter_factory;
pub mod routes;
pub mod shutdown;
pub mod worker;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use metrics_exporter_prometheus::PrometheusHandle;
use sqlx::PgPool;

use hookbox::dedupe::{InMemoryRecentDedupe, LayeredDedupe};
use hookbox::pipeline::HookboxPipeline;
use hookbox::traits::{DedupeStrategy, Storage};
use hookbox_postgres::{PostgresStorage, StorageDedupe};

/// Shared application state, threaded through all Axum handlers via
/// `State<Arc<AppState<S, D>>>`.
pub struct AppState<S: Storage, D: DedupeStrategy> {
    /// The ingest pipeline wired with backend types.
    pub pipeline: HookboxPipeline<S, D>,
    /// Database connection pool, used independently for health checks.
    /// `None` when running without a database (e.g. in tests).
    pub pool: Option<PgPool>,
    /// Optional bearer token for admin API authentication.
    pub admin_token: Option<String>,
    /// Optional Prometheus metrics handle for the `/metrics` scrape endpoint.
    pub prometheus: Option<PrometheusHandle>,
}

/// Concrete [`AppState`] used by the production server binary.
pub type ServerAppState = AppState<
    PostgresStorage,
    LayeredDedupe<InMemoryRecentDedupe, StorageDedupe>,
>;

/// Build the Axum [`Router`] with all hookbox routes wired to the given state.
///
/// `body_limit` sets the maximum request body size in bytes via
/// [`DefaultBodyLimit::max`].
pub fn build_router<S, D>(state: Arc<AppState<S, D>>, body_limit: usize) -> Router
where
    S: Storage + 'static,
    D: DedupeStrategy + 'static,
{
    Router::new()
        .route(
            "/webhooks/{provider}",
            post(routes::ingest::ingest_webhook::<S, D>),
        )
        .route("/healthz", get(routes::health::healthz))
        .route("/readyz", get(routes::health::readyz::<S, D>))
        .route(
            "/api/receipts",
            get(routes::admin::list_receipts::<S, D>),
        )
        .route(
            "/api/receipts/{id}",
            get(routes::admin::get_receipt::<S, D>),
        )
        .route(
            "/api/receipts/{id}/replay",
            post(routes::admin::replay_receipt::<S, D>),
        )
        .route("/api/dlq", get(routes::admin::list_dlq::<S, D>))
        .route("/metrics", get(routes::health::metrics::<S, D>))
        .with_state(state)
        .layer(DefaultBodyLimit::max(body_limit))
}
