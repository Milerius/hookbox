//! Graceful shutdown signal handling.

/// Returns a future that completes when a shutdown signal is received.
///
/// Listens for SIGINT (Ctrl-C) and SIGTERM (container orchestrators).
///
/// # Panics
///
/// Panics if the OS signal handler cannot be installed. This is a fatal,
/// non-recoverable condition — the process cannot function without signal handling.
#[expect(clippy::expect_used, reason = "signal handler installation is fatal and non-recoverable")]
pub async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => { tracing::info!("received SIGINT, shutting down"); }
        () = terminate => { tracing::info!("received SIGTERM, shutting down"); }
    }
}
