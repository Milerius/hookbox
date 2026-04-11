//! Shared database connection helper for CLI commands.
use anyhow::Context;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// Connect to the database using the provided URL or `DATABASE_URL` env var.
///
/// # Errors
///
/// Returns an error if no URL is available or the connection fails.
pub async fn connect(database_url: &str) -> anyhow::Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(2)
        .connect(database_url)
        .await
        .context("failed to connect to database")
}
