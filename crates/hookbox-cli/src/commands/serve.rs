//! `serve` subcommand — start the hookbox webhook ingestion server.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use arc_swap::ArcSwap;
use metrics_exporter_prometheus::PrometheusBuilder;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;
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
use hookbox_server::config::{EmitterEntry, HookboxConfig, parse_and_normalize};
use hookbox_server::emitter_factory::{BuiltEmitter, build_emitter};
use hookbox_server::shutdown::shutdown_signal;
use hookbox_server::worker::{EmitterHealth, EmitterWorker};

/// Default lease duration for an `EmitterWorker` when the TOML entry omits
/// `lease_duration_seconds`. Matches the conservative value used by the
/// server BDD harness and gives ample headroom over the default poll
/// interval so a single poll tick can't orphan in-flight rows.
const DEFAULT_LEASE_DURATION: Duration = Duration::from_secs(30);

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

    let (config, warnings) = parse_and_normalize(&raw)
        .with_context(|| format!("failed to parse config: {config_path}"))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build Tokio runtime")?;

    runtime.block_on(run_server(config, warnings))
}

/// Async server startup: tracing, database, migrations, pipeline, and HTTP serve.
#[expect(
    clippy::too_many_lines,
    reason = "provider dispatch match arms require all types inline"
)]
async fn run_server(config: HookboxConfig, config_warnings: Vec<String>) -> anyhow::Result<()> {
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

    for warning in &config_warnings {
        tracing::warn!(warning = %warning, "config normalization warning");
    }

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

    // Collect the declared emitter names so the pipeline can insert one
    // pending delivery row per emitter on accept.  `parse_and_normalize`
    // guarantees `config.emitters` is non-empty.
    let emitter_names: Vec<String> = config.emitters.iter().map(|e| e.name.clone()).collect();

    // Build pipeline with emitter names wired so `store_with_deliveries`
    // inserts one row per emitter on accept.  Provider verifiers are
    // registered from config below.
    let mut builder = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(dedupe)
        .emitter_names(emitter_names.clone());

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

    // Spawn one `EmitterWorker` per configured emitter. Each worker owns its
    // health snapshot (published via `/readyz`) and its own shutdown receiver
    // so the whole pool can be stopped atomically from the graceful-shutdown
    // path below.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut emitter_health: BTreeMap<String, Arc<ArcSwap<EmitterHealth>>> = BTreeMap::new();
    let mut worker_handles: Vec<JoinHandle<()>> = Vec::new();

    for entry in &config.emitters {
        let (emitter, drain_handle) = build_emitter_for_entry(entry).await?;
        if let Some(handle) = drain_handle {
            // Channel drain tasks stay alive for the process lifetime; abort
            // on shutdown so the binary exits cleanly.
            worker_handles.push(handle);
        }

        let health = Arc::new(ArcSwap::from_pointee(EmitterHealth::default()));
        emitter_health.insert(entry.name.clone(), Arc::clone(&health));

        let worker = EmitterWorker {
            name: entry.name.clone(),
            emitter,
            storage: PostgresStorage::new(pool.clone()),
            policy: entry.retry.clone().into_policy(),
            concurrency: entry.concurrency,
            poll_interval: Duration::from_secs(entry.poll_interval_seconds),
            lease_duration: entry
                .lease_duration_seconds
                .map_or(DEFAULT_LEASE_DURATION, Duration::from_secs),
            health,
        };
        tracing::info!(
            emitter = %entry.name,
            emitter_type = %entry.emitter_type,
            concurrency = entry.concurrency,
            "spawning EmitterWorker"
        );
        worker_handles.push(worker.spawn(shutdown_rx.clone()));
    }

    let state: Arc<ServerAppState> = Arc::new(ServerAppState {
        pipeline,
        pool: Some(pool),
        admin_token: config.admin.bearer_token.clone(),
        prometheus: Some(prometheus),
        emitter_health,
    });

    let router = build_router(state, config.server.body_limit);

    let bind_addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind to {bind_addr}"))?;

    tracing::info!(addr = %bind_addr, "hookbox server listening");

    // Hold the sender inside the graceful-shutdown future so the same signal
    // that stops axum also flips the workers' shutdown watch.
    let shutdown_tx_for_axum = shutdown_tx.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            let _ = shutdown_tx_for_axum.send(true);
        })
        .await
        .context("server encountered a fatal error")?;

    tracing::info!("server shut down gracefully, waiting for workers to drain");

    // Make sure every worker sees the shutdown even if axum returned for
    // another reason (panic, listener drop, etc.).
    let _ = shutdown_tx.send(true);
    for handle in worker_handles {
        let _ = handle.await;
    }
    tracing::info!("emitter workers stopped");

    Ok(())
}

/// Build one [`EmitterWorker`]'s downstream emitter from an [`EmitterEntry`].
///
/// Returns the type-erased emitter plus, for the development `channel`
/// variant, a join handle for the drain task that keeps the receiver alive.
/// Production backends (kafka / nats / sqs / redis) return `Ok((_, None))`.
async fn build_emitter_for_entry(
    entry: &EmitterEntry,
) -> anyhow::Result<(Arc<dyn Emitter + Send + Sync>, Option<JoinHandle<()>>)> {
    match build_emitter(&entry.to_emitter_config()).await? {
        BuiltEmitter::Ready(emitter) => Ok((emitter, None)),
        BuiltEmitter::Channel { emitter, rx } => {
            let handle = tokio::spawn(drain_emitter(entry.name.clone(), rx));
            Ok((emitter, Some(handle)))
        }
    }
}

/// Drain the channel-backed emitter's receiver, logging each received event.
///
/// In a real deployment this would forward events to a message broker,
/// trigger downstream processing, etc. For the MVP standalone server it
/// simply keeps the channel receiver alive so that emits don't fail.
async fn drain_emitter(
    emitter_name: String,
    mut rx: tokio::sync::mpsc::Receiver<hookbox::NormalizedEvent>,
) {
    while let Some(event) = rx.recv().await {
        tracing::info!(
            emitter = %emitter_name,
            receipt_id = %event.receipt_id,
            provider = %event.provider_name,
            event_type = ?event.event_type,
            "event emitted (no consumer wired — drain task)"
        );
    }
    tracing::warn!(emitter = %emitter_name, "emitter channel closed — drain task exiting");
}
