//! `replay` subcommands — re-queue individual or failed receipts for retry.

use anyhow::Context as _;
use chrono::Utc;
use clap::Subcommand;
use uuid::Uuid;

use hookbox_postgres::PostgresStorage;

use crate::db;

/// Replay commands for re-processing failed receipts.
#[derive(Subcommand)]
pub enum ReplayCommand {
    /// Replay a single receipt by ID.
    Id {
        /// Database connection URL.
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,

        /// Receipt UUID to replay.
        receipt_id: Uuid,
    },

    /// Replay all failed receipts within a time window.
    Failed {
        /// Database connection URL.
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,

        /// Filter by provider name.
        #[arg(long)]
        provider: Option<String>,

        /// Time window to look back (e.g. "1h", "30m", "2d").
        #[arg(long)]
        since: String,

        /// Maximum number of receipts to replay.
        #[arg(long, default_value_t = 100)]
        limit: i64,
    },
}

/// Execute a replay subcommand.
///
/// # Errors
///
/// Returns an error if the database connection, migration, or query fails.
pub async fn run(command: ReplayCommand) -> anyhow::Result<()> {
    match command {
        ReplayCommand::Id {
            database_url,
            receipt_id,
        } => {
            let pool = db::connect(&database_url).await?;
            let storage = PostgresStorage::new(pool);
            storage.migrate().await.context("migration failed")?;

            storage.reset_for_retry(receipt_id).await?;
            tracing::info!(id = %receipt_id, "receipt queued for retry");
        }

        ReplayCommand::Failed {
            database_url,
            provider,
            since,
            limit,
        } => {
            let pool = db::connect(&database_url).await?;
            let storage = PostgresStorage::new(pool);
            storage.migrate().await.context("migration failed")?;

            let duration = parse_duration(&since)?;
            let since_dt = Utc::now() - duration;

            let receipts = storage
                .query_failed_since(provider.as_deref(), since_dt, Some(limit))
                .await?;

            let count = receipts.len();
            for r in &receipts {
                storage.reset_for_retry(r.receipt_id.0).await?;
                tracing::info!(id = %r.receipt_id, "receipt queued for retry");
            }
            tracing::info!(count, "total receipts replayed");
        }
    }
    Ok(())
}

/// Parse a human-friendly duration string like `"30s"`, `"5m"`, `"2h"`, `"1d"`.
///
/// # Errors
///
/// Returns an error if the format is unrecognised or the numeric part is invalid.
fn parse_duration(s: &str) -> anyhow::Result<chrono::Duration> {
    let s = s.trim();
    anyhow::ensure!(!s.is_empty(), "duration string must not be empty");

    let (num_str, unit) = s.split_at(s.len() - 1);
    let n: i64 = num_str
        .parse()
        .with_context(|| format!("invalid duration number: {num_str:?}"))?;

    let duration = match unit {
        "s" => chrono::Duration::seconds(n),
        "m" => chrono::Duration::minutes(n),
        "h" => chrono::Duration::hours(n),
        "d" => chrono::Duration::days(n),
        _ => anyhow::bail!("unknown duration unit {unit:?} — expected s, m, h, or d"),
    };

    Ok(duration)
}
