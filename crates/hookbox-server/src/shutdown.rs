//! Graceful shutdown signal handling.

/// Returns a future that completes when a shutdown signal is received.
///
/// Listens for SIGINT (Ctrl-C) and SIGTERM (container orchestrators).
///
/// If signal handler installation fails, logs the error and waits forever
/// (the server keeps running but cannot be gracefully stopped via signals).
pub async fn shutdown_signal() {
    let ctrl_c = async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {}
            Err(e) => {
                tracing::error!(error = %e, "failed to install Ctrl+C handler");
                // Wait forever since we can't listen for signals.
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to install SIGTERM handler");
                // Wait forever since we can't listen for signals.
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => { tracing::info!("received SIGINT, shutting down"); }
        () = terminate => { tracing::info!("received SIGTERM, shutting down"); }
    }
}
