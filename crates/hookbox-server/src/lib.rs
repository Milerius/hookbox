//! Standalone Axum HTTP server for hookbox.
//!
//! Provides [`AppState`], [`build_router`], and all HTTP route handlers for
//! webhook ingestion, health checks, and administrative operations.

pub mod config;
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
use hookbox::traits::{DedupeStrategy, Emitter, Storage};
use hookbox_postgres::{PostgresStorage, StorageDedupe};

/// Shared application state, threaded through all Axum handlers via
/// `State<Arc<AppState<S, D, E>>>`.
pub struct AppState<S: Storage, D: DedupeStrategy, E: Emitter> {
    /// The ingest pipeline wired with backend types.
    pub pipeline: HookboxPipeline<S, D, E>,
    /// Database connection pool, used independently for health checks.
    /// `None` when running without a database (e.g. in tests).
    pub pool: Option<PgPool>,
    /// Optional bearer token for admin API authentication.
    pub admin_token: Option<String>,
    /// Optional Prometheus metrics handle for the `/metrics` scrape endpoint.
    pub prometheus: Option<PrometheusHandle>,
}

/// Concrete [`AppState`] used by the production server binary.
///
/// The emitter is erased to `Arc<dyn Emitter + Send + Sync>` so that the
/// server can select the emitter backend at runtime from configuration
/// without changing the type signature of the router or handlers.
pub type ServerAppState = AppState<
    PostgresStorage,
    LayeredDedupe<InMemoryRecentDedupe, StorageDedupe>,
    Arc<dyn Emitter + Send + Sync>,
>;

/// Build the Axum [`Router`] with all hookbox routes wired to the given state.
///
/// `body_limit` sets the maximum request body size in bytes via
/// [`DefaultBodyLimit::max`].
pub fn build_router<S, D, E>(state: Arc<AppState<S, D, E>>, body_limit: usize) -> Router
where
    S: Storage + 'static,
    D: DedupeStrategy + 'static,
    E: Emitter + 'static,
{
    Router::new()
        .route(
            "/webhooks/{provider}",
            post(routes::ingest::ingest_webhook::<S, D, E>),
        )
        .route("/healthz", get(routes::health::healthz))
        .route("/readyz", get(routes::health::readyz::<S, D, E>))
        .route(
            "/api/receipts",
            get(routes::admin::list_receipts::<S, D, E>),
        )
        .route(
            "/api/receipts/{id}",
            get(routes::admin::get_receipt::<S, D, E>),
        )
        .route(
            "/api/receipts/{id}/replay",
            post(routes::admin::replay_receipt::<S, D, E>),
        )
        .route("/api/dlq", get(routes::admin::list_dlq::<S, D, E>))
        .route("/metrics", get(routes::health::metrics::<S, D, E>))
        .with_state(state)
        .layer(DefaultBodyLimit::max(body_limit))
}
