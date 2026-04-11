//! `serve` subcommand — start the hookbox webhook ingestion server.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use metrics_exporter_prometheus::PrometheusBuilder;
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
    // Install the Prometheus metrics recorder so that metrics::counter! /
    // metrics::histogram! macros emit real data instead of no-ops.
    let prometheus = PrometheusBuilder::new()
        .install_recorder()
        .context("failed to install Prometheus recorder")?;

    // Initialise JSON tracing with an env-filter defaulting to INFO.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .init();

    tracing::info!("connecting to database");

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .connect(&config.database.url)
        .await
        .context("failed to connect to database")?;

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

    // Build the downstream emitter (channel-based).
    // Spawn a drain task so the receiver stays alive and emits don't fail.
    // In production, this would be replaced with a real consumer.
    let (emitter, rx) = ChannelEmitter::new(1024);
    tokio::spawn(drain_emitter(rx));

    // Build pipeline and register provider verifiers from config.
    let mut builder = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(dedupe)
        .emitter(emitter);

    for (name, provider) in &config.providers {
        if provider.verifier_type == "stripe" {
            let mut verifier = StripeVerifier::new(name.clone(), provider.secret.clone());
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
            let verifier =
                GenericHmacVerifier::new(name, provider.secret.as_bytes().to_vec(), header);
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
        admin_token: config.admin.bearer_token.clone(),
        prometheus: Some(prometheus),
    });

    let router = build_router(state, config.server.body_limit);

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

/// Drain the emitter channel, logging each received event.
///
/// In a real deployment this would forward events to a message broker,
/// trigger downstream processing, etc. For the MVP standalone server it
/// simply keeps the channel receiver alive so that emits don't fail.
async fn drain_emitter(mut rx: tokio::sync::mpsc::Receiver<hookbox::NormalizedEvent>) {
    while let Some(event) = rx.recv().await {
        tracing::info!(
            receipt_id = %event.receipt_id,
            provider = %event.provider_name,
            event_type = ?event.event_type,
            "event emitted (no consumer wired — drain task)"
        );
    }
    tracing::warn!("emitter channel closed — drain task exiting");
}
