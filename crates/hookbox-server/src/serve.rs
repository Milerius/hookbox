//! Core server orchestration: build the pipeline, spawn emitter workers,
//! mount the router, and run `axum::serve` against a pre-bound listener.
//!
//! This module is extracted from `hookbox-cli::commands::serve::run_server`
//! so the whole boot sequence is callable from integration tests without
//! touching process globals (Prometheus recorder, tracing subscriber) or
//! the host OS signal handlers.
//!
//! The CLI wrapper in `hookbox-cli` installs those globals and binds the
//! listener, then hands everything to [`serve_inner`].

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use arc_swap::ArcSwap;
use metrics_exporter_prometheus::PrometheusHandle;
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use hookbox::dedupe::{InMemoryRecentDedupe, LayeredDedupe};
use hookbox::pipeline::HookboxPipeline;
use hookbox::traits::Emitter;
use hookbox_postgres::{PostgresStorage, StorageDedupe};

use crate::bootstrap::{build_provider_verifiers, validate_retry_config};
use crate::build_router;
use crate::config::{EmitterEntry, HookboxConfig};
use crate::emitter_factory::{BuiltEmitter, build_emitter};
use crate::worker::{EmitterHealth, EmitterWorker, HealthStatus};
use crate::{AppState, ServerAppState};

/// Default lease duration for an `EmitterWorker` when the TOML entry omits
/// `lease_duration_seconds`.
const DEFAULT_LEASE_DURATION: Duration = Duration::from_secs(30);

/// Run the core hookbox server against a pre-bound listener.
///
/// All process-global side effects (installing the Prometheus recorder,
/// initialising tracing, handling OS signals) are the caller's
/// responsibility. This function owns:
///
/// 1. Connecting to Postgres and running migrations
/// 2. Building the dedupe layers and the `HookboxPipeline`
/// 3. Spawning one `EmitterWorker` per configured emitter
/// 4. Mounting the Axum router and running `axum::serve` against `listener`
/// 5. Draining workers once `shutdown` resolves
///
/// The generic `shutdown` future lets callers wire this up to real OS
/// signals in production and to a `watch`/`oneshot` in tests.
///
/// # Errors
///
/// Returns an error if the database connection, migration, emitter build,
/// or axum serve loop fails. A panic in any emitter task is converted to an
/// error on shutdown so tests can detect silent delivery outages.
pub async fn serve_inner<F>(
    config: HookboxConfig,
    config_warnings: Vec<String>,
    prometheus: PrometheusHandle,
    listener: TcpListener,
    shutdown: F,
) -> anyhow::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    validate_retry_config(&config.retry)?;

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

    let pipeline = build_pipeline(storage, pool.clone(), &config)?;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (emitter_health, worker_handles) =
        spawn_emitter_workers(&config, &pool, shutdown_rx).await?;

    let state: Arc<ServerAppState> = Arc::new(AppState {
        pipeline,
        pool: Some(pool),
        admin_token: config.admin.bearer_token.clone(),
        prometheus: Some(prometheus),
        emitter_health,
    });
    let router = build_router(state, config.server.body_limit);

    let shutdown_tx_for_axum = shutdown_tx.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown.await;
            let _ = shutdown_tx_for_axum.send(true);
        })
        .await
        .context("server encountered a fatal error")?;

    tracing::info!("server shut down gracefully, waiting for workers to drain");

    let _ = shutdown_tx.send(true);
    drain_worker_handles(worker_handles).await
}

/// Build the `HookboxPipeline` with dedupe layers and provider verifiers.
fn build_pipeline(
    storage: PostgresStorage,
    pool: PgPool,
    config: &HookboxConfig,
) -> anyhow::Result<
    HookboxPipeline<PostgresStorage, LayeredDedupe<InMemoryRecentDedupe, StorageDedupe>>,
> {
    let lru = InMemoryRecentDedupe::new(config.dedupe.lru_capacity);
    let storage_dedupe = StorageDedupe::new(pool);
    let dedupe = LayeredDedupe::new(lru, storage_dedupe);
    let emitter_names: Vec<String> = config.emitters.iter().map(|e| e.name.clone()).collect();

    let mut builder = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(dedupe)
        .emitter_names(emitter_names);

    for verifier in build_provider_verifiers(&config.providers)? {
        let name = verifier.provider_name().to_owned();
        tracing::info!(provider = %name, "registering verifier");
        builder = builder.verifier_boxed(verifier);
    }

    Ok(builder.build())
}

/// Spawn one `EmitterWorker` per configured emitter plus any drain tasks
/// needed by channel-backed dev emitters.
async fn spawn_emitter_workers(
    config: &HookboxConfig,
    pool: &PgPool,
    shutdown_rx: watch::Receiver<bool>,
) -> anyhow::Result<(
    BTreeMap<String, Arc<ArcSwap<EmitterHealth>>>,
    Vec<(String, JoinHandle<()>)>,
)> {
    let mut emitter_health: BTreeMap<String, Arc<ArcSwap<EmitterHealth>>> = BTreeMap::new();
    let mut worker_handles: Vec<(String, JoinHandle<()>)> = Vec::new();

    for entry in &config.emitters {
        let (emitter, drain_handle) = build_emitter_for_entry(entry).await?;
        if let Some(handle) = drain_handle {
            worker_handles.push((format!("{}-drain", entry.name), handle));
        }

        let health = Arc::new(ArcSwap::from_pointee(EmitterHealth::default()));
        emitter_health.insert(entry.name.clone(), Arc::clone(&health));
        let supervisor_health = Arc::clone(&health);

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
        let inner_handle = worker.spawn(shutdown_rx.clone());
        let supervisor = supervise_worker(entry.name.clone(), inner_handle, supervisor_health);
        worker_handles.push((entry.name.clone(), supervisor));
    }

    Ok((emitter_health, worker_handles))
}

/// Wrap an emitter worker's `JoinHandle` in a supervising task that flips
/// the health snapshot to `Unhealthy` the moment the inner task panics —
/// without waiting for shutdown — so `/readyz` surfaces silent delivery
/// outages in real time. The supervisor then resumes the panic so its own
/// handle still returns `Err(JoinError)` and `drain_worker_handles` records
/// the outage at shutdown, matching the pre-supervisor contract.
fn supervise_worker(
    name: String,
    inner: JoinHandle<()>,
    health: Arc<ArcSwap<EmitterHealth>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        match inner.await {
            Ok(()) => {}
            Err(join_err) => {
                tracing::error!(
                    emitter = %name,
                    error = %join_err,
                    "emitter worker task terminated abnormally — marking Unhealthy"
                );
                let previous = health.load_full();
                let degraded = EmitterHealth {
                    last_success_at: previous.last_success_at,
                    last_failure_at: Some(chrono::Utc::now()),
                    consecutive_failures: previous.consecutive_failures.saturating_add(1),
                    status: HealthStatus::Unhealthy,
                    dlq_depth: previous.dlq_depth,
                    pending_count: previous.pending_count,
                };
                health.store(Arc::new(degraded));
                if join_err.is_panic() {
                    std::panic::resume_unwind(join_err.into_panic());
                }
            }
        }
    })
}

/// Await every spawned emitter task and aggregate any panics.
async fn drain_worker_handles(worker_handles: Vec<(String, JoinHandle<()>)>) -> anyhow::Result<()> {
    let mut panicked_tasks: Vec<String> = Vec::new();
    for (task_name, handle) in worker_handles {
        if let Err(e) = handle.await {
            tracing::error!(
                task = %task_name,
                error = %e,
                "emitter task terminated with a JoinError — this is a silent delivery outage",
            );
            panicked_tasks.push(task_name);
        }
    }
    if panicked_tasks.is_empty() {
        tracing::info!("emitter workers stopped");
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "shutdown completed but {} emitter task(s) panicked: {:?}",
            panicked_tasks.len(),
            panicked_tasks,
        ))
    }
}

/// Build one [`EmitterWorker`]'s downstream emitter from an [`EmitterEntry`].
///
/// Returns the type-erased emitter plus, for the development `channel`
/// variant, a join handle for the drain task that keeps the receiver alive.
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

#[cfg(test)]
#[expect(clippy::expect_used, reason = "expect is acceptable in test code")]
#[expect(clippy::panic, reason = "panic is acceptable in test assertions")]
mod tests {
    use super::*;

    /// A clean exit from the inner worker must leave the health snapshot
    /// untouched and let the supervisor handle resolve successfully.
    #[tokio::test]
    async fn supervise_worker_leaves_health_untouched_on_clean_exit() {
        let health = Arc::new(ArcSwap::from_pointee(EmitterHealth::default()));
        let inner: JoinHandle<()> = tokio::spawn(async {});
        let supervisor = supervise_worker("ok-emitter".to_owned(), inner, Arc::clone(&health));

        supervisor.await.expect("supervisor should exit cleanly");

        let snapshot = health.load_full();
        assert_eq!(snapshot.status, HealthStatus::Healthy);
        assert!(snapshot.last_failure_at.is_none());
        assert_eq!(snapshot.consecutive_failures, 0);
    }

    /// A panicking inner worker must flip the `ArcSwap` health to `Unhealthy`
    /// *before* shutdown, and re-raise the panic so the supervisor handle
    /// still returns `Err(JoinError)` for `drain_worker_handles` to catch.
    #[tokio::test]
    async fn supervise_worker_marks_unhealthy_and_propagates_on_panic() {
        let health = Arc::new(ArcSwap::from_pointee(EmitterHealth::default()));
        let inner: JoinHandle<()> =
            tokio::spawn(async { panic!("simulated emitter worker panic") });
        let supervisor = supervise_worker("panicky-emitter".to_owned(), inner, Arc::clone(&health));

        let result = supervisor.await;
        assert!(
            result.is_err(),
            "supervisor must propagate panics as JoinError",
        );
        let join_err = result.expect_err("panic already asserted");
        assert!(
            join_err.is_panic(),
            "supervisor JoinError should carry the original panic",
        );

        let snapshot = health.load_full();
        assert_eq!(
            snapshot.status,
            HealthStatus::Unhealthy,
            "panic must flip health to Unhealthy immediately, not at shutdown",
        );
        assert!(
            snapshot.last_failure_at.is_some(),
            "panic must stamp last_failure_at",
        );
        assert_eq!(snapshot.consecutive_failures, 1);
    }
}
