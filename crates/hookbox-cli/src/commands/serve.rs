//! `serve` subcommand — start the hookbox webhook ingestion server.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use sqlx::PgPool;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use hookbox::dedupe::{InMemoryRecentDedupe, LayeredDedupe};
use hookbox::emitter::ChannelEmitter;
use hookbox::pipeline::HookboxPipeline;
use hookbox_postgres::{PostgresStorage, StorageDedupe};
use hookbox_providers::{GenericHmacVerifier, StripeVerifier};
use hookbox_server::AppState;
use hookbox_server::build_router;
use hookbox_server::config::HookboxConfig;

/// Run the `serve` subcommand.
///
/// Reads and parses the TOML configuration at `config_path`, builds a Tokio
/// runtime, and blocks on [`run_server`].
///
/// # Errors
///
/// Returns an error if the config file cannot be read or parsed, or if the
/// server encounters a fatal startup error.
pub fn run(config_path: &str) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read config file: {config_path}"))?;

    let config: HookboxConfig =
        toml::from_str(&raw).with_context(|| format!("failed to parse config: {config_path}"))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build Tokio runtime")?;

    runtime.block_on(run_server(config))
}

/// Async server startup: tracing, database, migrations, pipeline, and HTTP serve.
async fn run_server(config: HookboxConfig) -> anyhow::Result<()> {
    // Initialise JSON tracing with an env-filter defaulting to INFO.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().json().with_env_filter(filter).init();

    tracing::info!("connecting to database");

    let pool = PgPool::connect(&config.database.url)
        .await
        .context("failed to connect to Postgres")?;

    tracing::info!("running database migrations");
    let storage = PostgresStorage::new(pool.clone());
    storage
        .migrate()
        .await
        .context("database migration failed")?;

    // Build advisory + authoritative dedupe layers.
    let lru = InMemoryRecentDedupe::new(config.dedupe.lru_capacity);
    let storage_dedupe = StorageDedupe::new(pool.clone());
    let dedupe = LayeredDedupe::new(lru, storage_dedupe);

    // Build the downstream emitter (channel-based; receiver is intentionally
    // dropped here since no consumer task is wired in the MVP).
    let (emitter, _rx) = ChannelEmitter::new(1024);

    // Build pipeline and register provider verifiers from config.
    let mut builder = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(dedupe)
        .emitter(emitter);

    for (name, provider) in &config.providers {
        if provider.verifier_type == "stripe" {
            let mut verifier = StripeVerifier::new(provider.secret.clone());
            if let Some(tolerance_secs) = provider.tolerance_seconds {
                verifier = verifier.with_tolerance(Duration::from_secs(tolerance_secs));
            }
            tracing::info!(provider = %name, "registering StripeVerifier");
            builder = builder.verifier(verifier);
        } else {
            // "hmac-sha256" or any unknown type falls back to the generic verifier.
            let header = provider
                .header
                .clone()
                .unwrap_or_else(|| format!("X-{name}-Signature"));
            let verifier = GenericHmacVerifier::new(
                name,
                provider.secret.as_bytes().to_vec(),
                header,
            );
            tracing::info!(
                provider = %name,
                verifier_type = %provider.verifier_type,
                "registering GenericHmacVerifier"
            );
            builder = builder.verifier(verifier);
        }
    }

    let pipeline = builder.build();

    let state = Arc::new(AppState {
        pipeline,
        pool,
    });

    let router = build_router(state);

    let bind_addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind to {bind_addr}"))?;

    tracing::info!(addr = %bind_addr, "hookbox server listening");

    axum::serve(listener, router)
        .await
        .context("server encountered a fatal error")?;

    Ok(())
}
