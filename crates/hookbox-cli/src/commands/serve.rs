//! `serve` subcommand — start the hookbox webhook ingestion server.
//!
//! This file owns the *process-global* side effects: installing the
//! Prometheus recorder, initialising the tracing subscriber, binding the
//! TCP listener, and handing control to [`hookbox_server::serve::serve_inner`]
//! with a real OS signal future. The rest of the boot sequence lives in
//! `hookbox-server` so integration tests can drive it without touching
//! globals.

use anyhow::Context as _;
use metrics_exporter_prometheus::PrometheusBuilder;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use hookbox_server::config::{HookboxConfig, parse_and_normalize};
use hookbox_server::serve::serve_inner;
use hookbox_server::shutdown::shutdown_signal;

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

/// Install process-global side effects (Prometheus recorder + tracing
/// subscriber), bind the TCP listener, and hand control to
/// [`serve_inner`].
async fn run_server(config: HookboxConfig, config_warnings: Vec<String>) -> anyhow::Result<()> {
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

    let bind_addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind to {bind_addr}"))?;

    tracing::info!(addr = %bind_addr, "hookbox server listening");

    serve_inner(
        config,
        config_warnings,
        prometheus,
        listener,
        shutdown_signal(),
    )
    .await
}
