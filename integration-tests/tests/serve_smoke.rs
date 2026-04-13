//! Smoke test for [`hookbox_server::serve::serve_inner`].
//!
//! Drives the full boot sequence (connect, migrate, build pipeline, spawn
//! workers, mount router, serve) without touching the process-global
//! Prometheus recorder, tracing subscriber, or OS signal handler. This
//! pins the contract that the inner serve path is testable in isolation
//! and exercises the happy-path branches in
//! `crates/hookbox-server/src/serve.rs`.
//!
//! Requires a running Postgres — defaults to `postgres://localhost/hookbox_test`.

#![expect(clippy::expect_used, reason = "expect is acceptable in test code")]

use std::time::Duration;

use hookbox_server::config::parse_and_normalize;
use hookbox_server::serve::serve_inner;
use metrics_exporter_prometheus::PrometheusBuilder;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/hookbox_test".to_owned())
}

/// Boot the inner serve path, probe `/healthz` and `/metrics`, then drive a
/// graceful shutdown. Any panic in an emitter task surfaces through the
/// returned `JoinHandle`'s error, so this doubles as a regression guard
/// against silent delivery-worker outages.
#[tokio::test]
async fn serve_inner_boots_probes_and_shuts_down_cleanly() {
    let url = database_url();
    // Unique emitter name so the per-emitter delivery queue cannot collide
    // with rows left by other integration tests hitting the same database.
    let emitter_name = format!("serve-smoke-{}", Uuid::new_v4().simple());
    let raw_config = format!(
        r#"
[database]
url = "{url}"

[[emitters]]
name = "{emitter_name}"
type = "channel"
"#
    );

    let (config, warnings) = parse_and_normalize(&raw_config).expect("parse config");

    // Build a Prometheus handle WITHOUT installing the global recorder —
    // `install_recorder()` is a process-singleton and would clash with any
    // other test that also installs one. The handle holds its own
    // `Arc<Inner>` so it outlives the local recorder.
    let prom_handle = PrometheusBuilder::new().build_recorder().handle();

    // Bind an ephemeral port so parallel test workers do not fight for a
    // fixed address. We hand the bound listener to `serve_inner` and keep
    // the resolved address here to drive HTTP probes.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown = async move {
        let _ = shutdown_rx.await;
    };

    let join = tokio::spawn(async move {
        serve_inner(config, warnings, prom_handle, listener, shutdown)
            .await
            .expect("serve_inner returned an error");
    });

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    // Poll /healthz until the server is listening or we time out.
    let mut healthz_ok = false;
    for _ in 0..50 {
        if let Ok(resp) = client.get(format!("{base}/healthz")).send().await
            && resp.status().is_success()
        {
            healthz_ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(healthz_ok, "healthz never became ready");

    // /readyz exercises the per-emitter health map built in
    // `spawn_emitter_workers` and returns 503 until the worker reports
    // ready. We only care that it renders without panicking.
    let readyz_resp = client
        .get(format!("{base}/readyz"))
        .send()
        .await
        .expect("readyz request");
    assert!(
        readyz_resp.status().is_success() || readyz_resp.status() == 503,
        "unexpected /readyz status: {}",
        readyz_resp.status()
    );

    // /metrics should render the non-installed recorder without panicking.
    let metrics_resp = client
        .get(format!("{base}/metrics"))
        .send()
        .await
        .expect("metrics request");
    assert!(metrics_resp.status().is_success());

    // Trigger graceful shutdown and wait for the serve task to drain.
    shutdown_tx.send(()).expect("trigger shutdown");
    tokio::time::timeout(Duration::from_secs(10), join)
        .await
        .expect("serve_inner did not shut down within 10s")
        .expect("serve_inner task panicked");
}
