//! `serve` subcommand — start the hookbox webhook ingestion server.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use metrics_exporter_prometheus::PrometheusBuilder;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use hookbox::dedupe::{InMemoryRecentDedupe, LayeredDedupe};
use hookbox::pipeline::HookboxPipeline;
use hookbox::traits::Emitter;
use hookbox_postgres::{PostgresStorage, StorageDedupe};
use hookbox_providers::{
    AdyenVerifier, BvnkVerifier, GenericHmacVerifier, StripeVerifier, TripleACryptoVerifier,
    TripleAFiatVerifier, WalapayVerifier,
};
use hookbox_server::ServerAppState;
use hookbox_server::build_router;
use hookbox_server::config::HookboxConfig;
use hookbox_server::emitter_factory::{BuiltEmitter, build_emitter};
use hookbox_server::shutdown::shutdown_signal;
use hookbox_server::worker::RetryWorker;

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
#[expect(
    clippy::too_many_lines,
    reason = "provider dispatch match arms require all types inline"
)]
async fn run_server(config: HookboxConfig) -> anyhow::Result<()> {
    anyhow::ensure!(
        config.retry.interval_seconds >= 1,
        "retry.interval_seconds must be >= 1"
    );
    anyhow::ensure!(
        config.retry.max_attempts >= 1,
        "retry.max_attempts must be >= 1"
    );
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

    // Build the downstream emitter from config. The factory returns the
    // production emitters as `Ready`; the development `channel` variant
    // additionally yields a receiver that this binary drains for logging.
    // TODO(Task 14 / fan-out): replace with build_workers once serve.rs is rewired.
    let legacy_emitter_cfg = config.emitter.clone().unwrap_or_default();
    let emitter: Arc<dyn Emitter + Send + Sync> = match build_emitter(&legacy_emitter_cfg).await? {
        BuiltEmitter::Ready(emitter) => emitter,
        BuiltEmitter::Channel { emitter, rx } => {
            tokio::spawn(drain_emitter(rx));
            emitter
        }
    };

    // Clone the emitter for the retry worker before moving into the pipeline.
    let worker_emitter: Box<dyn Emitter + Send + Sync> = Box::new(Arc::clone(&emitter));

    // Build pipeline and register provider verifiers from config.
    // Emitter names are not yet wired here — Phase 7 (EmitterWorker) will
    // plumb them from config.  For now the pipeline creates no delivery rows.
    let mut builder = HookboxPipeline::builder().storage(storage).dedupe(dedupe);

    for (name, provider) in &config.providers {
        match provider.verifier_type.as_str() {
            "stripe" => {
                let secret = provider
                    .secret
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("provider '{name}' requires a non-empty secret")
                    })?;
                let mut verifier = StripeVerifier::new(name.clone(), secret.to_owned());
                if let Some(tolerance_secs) = provider.tolerance_seconds {
                    verifier = verifier.with_tolerance(Duration::from_secs(tolerance_secs));
                }
                tracing::info!(provider = %name, "registering StripeVerifier");
                builder = builder.verifier(verifier);
            }
            "adyen" => {
                let secret = provider
                    .secret
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "adyen provider '{name}' requires a non-empty secret (hex-encoded HMAC key)"
                        )
                    })?;
                let Some(verifier) = AdyenVerifier::new(name, secret) else {
                    anyhow::bail!("invalid hex key for Adyen provider '{name}'");
                };
                tracing::info!(provider = %name, "registering AdyenVerifier");
                builder = builder.verifier(verifier);
            }
            "bvnk" => {
                let secret = provider
                    .secret
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("bvnk provider '{name}' requires a non-empty secret")
                    })?;
                let verifier = BvnkVerifier::new(name, secret.as_bytes().to_vec());
                tracing::info!(provider = %name, "registering BvnkVerifier");
                builder = builder.verifier(verifier);
            }
            "triplea-fiat" => {
                let pem = provider
                    .public_key
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "triplea-fiat provider '{name}' requires a non-empty public_key"
                        )
                    })?;
                let Some(verifier) = TripleAFiatVerifier::new(name, pem) else {
                    anyhow::bail!("invalid PEM public key for Triple-A fiat provider '{name}'");
                };
                tracing::info!(provider = %name, "registering TripleAFiatVerifier");
                builder = builder.verifier(verifier);
            }
            "triplea-crypto" => {
                let secret = provider
                    .secret
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "triplea-crypto provider '{name}' requires a non-empty secret (notify_secret)"
                        )
                    })?;
                let mut verifier = TripleACryptoVerifier::new(name.clone(), secret.to_owned());
                if let Some(tolerance) = provider.tolerance_seconds {
                    verifier = verifier.with_tolerance(std::time::Duration::from_secs(tolerance));
                }
                tracing::info!(provider = %name, "registering TripleACryptoVerifier");
                builder = builder.verifier(verifier);
            }
            "walapay" => {
                let secret = provider
                    .secret
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "walapay provider '{name}' requires a non-empty secret (whsec_...)"
                        )
                    })?;
                let Some(mut verifier) = WalapayVerifier::new(name, secret) else {
                    anyhow::bail!(
                        "invalid Svix secret for Walapay provider '{name}' (expected whsec_...)"
                    );
                };
                if let Some(tolerance) = provider.tolerance_seconds {
                    verifier = verifier.with_tolerance(Duration::from_secs(tolerance));
                }
                tracing::info!(provider = %name, "registering WalapayVerifier");
                builder = builder.verifier(verifier);
            }
            "checkout" => {
                let secret = provider
                    .secret
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("checkout provider '{name}' requires a non-empty secret")
                    })?;
                let header = provider
                    .header
                    .clone()
                    .unwrap_or_else(|| "Cko-Signature".to_owned());
                let verifier = GenericHmacVerifier::new(name, secret.as_bytes().to_vec(), header);
                tracing::info!(provider = %name, "registering GenericHmacVerifier (Checkout.com)");
                builder = builder.verifier(verifier);
            }
            "hmac-sha256" => {
                let secret = provider
                    .secret
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("provider '{name}' requires a non-empty secret")
                    })?;
                let header = provider
                    .header
                    .clone()
                    .unwrap_or_else(|| format!("X-{name}-Signature"));
                let verifier = GenericHmacVerifier::new(name, secret.as_bytes().to_vec(), header);
                tracing::info!(
                    provider = %name,
                    verifier_type = %provider.verifier_type,
                    "registering GenericHmacVerifier"
                );
                builder = builder.verifier(verifier);
            }
            other => {
                tracing::warn!(provider = %name, provider_type = %other, "unknown provider type, falling back to GenericHmacVerifier");
                let secret = provider
                    .secret
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("provider '{name}' requires a non-empty secret")
                    })?;
                let header = provider
                    .header
                    .clone()
                    .unwrap_or_else(|| format!("X-{name}-Signature"));
                let verifier = GenericHmacVerifier::new(name, secret.as_bytes().to_vec(), header);
                builder = builder.verifier(verifier);
            }
        }
    }

    let pipeline = builder.build();

    let retry_worker = RetryWorker::new(
        PostgresStorage::new(pool.clone()),
        worker_emitter,
        Duration::from_secs(config.retry.interval_seconds),
        config.retry.max_attempts,
    );
    let retry_handle = retry_worker.spawn();

    let state: Arc<ServerAppState> = Arc::new(ServerAppState {
        pipeline,
        pool: Some(pool),
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
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server encountered a fatal error")?;

    tracing::info!("server shut down gracefully");

    retry_handle.abort();
    tracing::info!("retry worker stopped");

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
