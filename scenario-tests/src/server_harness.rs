//! In-process Axum server backed by a testcontainer Postgres for full-stack
//! server BDD scenarios.
//!
//! [`TestServer`] spins up a real `hookbox-server` router wired to a fresh
//! Postgres database and one [`EmitterWorker`] per registered
//! [`FakeEmitter`].  It bypasses the production `serve.rs` composition so
//! BDD scenarios can exercise the fan-out code path before the production
//! binary is rewired to the new worker.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use hookbox::dedupe::{InMemoryRecentDedupe, LayeredDedupe};
use hookbox::pipeline::HookboxPipeline;
use hookbox::state::RetryPolicy;
use hookbox::traits::Emitter;
use hookbox_postgres::{PostgresStorage, StorageDedupe};
use hookbox_server::AppState;
use hookbox_server::build_router;
use hookbox_server::worker::{EmitterHealth, EmitterWorker};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;

use crate::fake_emitter::FakeEmitter;
use crate::world::PassVerifier;

/// Fake-provider name used by `TestServer`.  Scenarios POST to
/// `/webhooks/fake` and the embedded [`PassVerifier`] accepts every request.
pub const FAKE_PROVIDER: &str = "fake";

/// Descriptor for an emitter wired into the test server.
pub struct EmitterSpec {
    /// Logical name — used as the routing key in `[[emitters]]`.
    pub name: String,
    /// Shared fake emitter; retained by the caller for assertions.
    pub fake: Arc<FakeEmitter>,
    /// Retry policy driving the corresponding [`EmitterWorker`].
    pub policy: RetryPolicy,
    /// Maximum concurrent in-flight dispatches.
    pub concurrency: usize,
}

impl EmitterSpec {
    /// Convenience constructor with a conservative default policy suitable
    /// for wallclock-polling BDD scenarios (tiny backoffs, 2 attempts).
    #[must_use]
    pub fn new(name: impl Into<String>, fake: Arc<FakeEmitter>) -> Self {
        Self {
            name: name.into(),
            fake,
            policy: RetryPolicy {
                max_attempts: 2,
                initial_backoff: Duration::from_secs(1),
                max_backoff: Duration::from_secs(2),
                backoff_multiplier: 2.0,
                jitter: 0.0,
            },
            concurrency: 1,
        }
    }

    /// Override the default retry policy.
    #[must_use]
    pub fn with_policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }
}

/// A running in-process hookbox server backed by testcontainer Postgres.
pub struct TestServer {
    /// Base URL (e.g. `http://127.0.0.1:54823`) for `reqwest` clients.
    pub base_url: String,
    /// Registered fake emitters keyed by name.
    pub emitters: BTreeMap<String, Arc<FakeEmitter>>,
    /// `sqlx` pool, held for direct assertions against `webhook_deliveries`.
    pub pool: sqlx::PgPool,
    shutdown_tx: watch::Sender<bool>,
    server_handle: Option<JoinHandle<()>>,
    worker_handles: Vec<JoinHandle<()>>,
    // `ContainerAsync` stops the database when dropped; kept last so it
    // outlives the server + worker handles above during teardown.
    _container: ContainerAsync<Postgres>,
}

impl std::fmt::Debug for TestServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestServer")
            .field("base_url", &self.base_url)
            .field("emitters", &self.emitters.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl TestServer {
    /// Build and start a `TestServer` with the given emitters.
    ///
    /// Each [`EmitterSpec`] becomes a row in the pipeline's emitter-name
    /// list and gets its own [`EmitterWorker`] backed by the supplied
    /// [`FakeEmitter`].  The returned [`TestServer`] exposes:
    ///
    /// * `base_url` for `reqwest` calls,
    /// * `emitters` for per-emitter assertions,
    /// * `pool` for direct DB queries.
    ///
    /// # Errors
    ///
    /// Returns any error encountered starting the container, running
    /// migrations, binding the TCP listener, or spawning the server.
    pub async fn start(specs: Vec<EmitterSpec>) -> anyhow::Result<Self> {
        let container = Postgres::default()
            .with_tag("16-alpine")
            .start()
            .await
            .map_err(|e| anyhow::anyhow!("start postgres container: {e}"))?;
        let host = container
            .get_host()
            .await
            .map_err(|e| anyhow::anyhow!("get host: {e}"))?;
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .map_err(|e| anyhow::anyhow!("get port: {e}"))?;
        let db_url = format!("postgres://postgres:postgres@{host}:{port}/postgres");

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(30))
            .connect(&db_url)
            .await?;

        let storage = PostgresStorage::new(pool.clone());
        storage.migrate().await?;

        let names: Vec<String> = specs.iter().map(|s| s.name.clone()).collect();

        let lru = InMemoryRecentDedupe::new(1024);
        let storage_dedupe = StorageDedupe::new(pool.clone());
        let dedupe = LayeredDedupe::new(lru, storage_dedupe);

        let pipeline = HookboxPipeline::builder()
            .storage(PostgresStorage::new(pool.clone()))
            .dedupe(dedupe)
            .emitter_names(names.clone())
            .verifier(PassVerifier {
                provider: FAKE_PROVIDER.to_owned(),
            })
            .build();

        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        // Spawn one EmitterWorker per spec and collect health handles.
        let mut emitter_health: BTreeMap<String, Arc<ArcSwap<EmitterHealth>>> = BTreeMap::new();
        let mut emitter_map: BTreeMap<String, Arc<FakeEmitter>> = BTreeMap::new();
        let mut worker_handles: Vec<JoinHandle<()>> = Vec::new();

        for spec in specs {
            let health = Arc::new(ArcSwap::from_pointee(EmitterHealth::default()));
            emitter_health.insert(spec.name.clone(), health.clone());
            emitter_map.insert(spec.name.clone(), spec.fake.clone());

            let emitter_dyn: Arc<dyn Emitter + Send + Sync> = spec.fake.clone();
            let worker = EmitterWorker {
                name: spec.name.clone(),
                emitter: emitter_dyn,
                storage: PostgresStorage::new(pool.clone()),
                policy: spec.policy,
                concurrency: spec.concurrency,
                poll_interval: Duration::from_millis(100),
                lease_duration: Duration::from_secs(30),
                health,
            };
            worker_handles.push(worker.spawn(shutdown_rx.clone()));
        }

        let state = Arc::new(AppState {
            pipeline,
            pool: Some(pool.clone()),
            admin_token: None,
            prometheus: None,
            emitter_health,
        });

        let router = build_router(state, 1_048_576);

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let local_addr = listener.local_addr()?;
        let base_url = format!("http://{local_addr}");

        let mut server_shutdown_rx = shutdown_rx.clone();
        let server_handle = tokio::spawn(async move {
            let serve = axum::serve(listener, router).with_graceful_shutdown(async move {
                let _ = server_shutdown_rx.changed().await;
            });
            if let Err(e) = serve.await {
                tracing::error!(error = %e, "test server exited with error");
            }
        });

        Ok(Self {
            base_url,
            emitters: emitter_map,
            pool,
            shutdown_tx,
            server_handle: Some(server_handle),
            worker_handles,
            _container: container,
        })
    }

    /// Look up a fake emitter by name.
    #[must_use]
    pub fn emitter(&self, name: &str) -> Option<Arc<FakeEmitter>> {
        self.emitters.get(name).cloned()
    }

    /// Shut down the server and all emitter workers.
    pub async fn shutdown(&mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(handle) = self.server_handle.take() {
            let _ = handle.await;
        }
        for handle in std::mem::take(&mut self.worker_handles) {
            let _ = handle.await;
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        for handle in &self.worker_handles {
            handle.abort();
        }
        if let Some(handle) = &self.server_handle {
            handle.abort();
        }
    }
}
