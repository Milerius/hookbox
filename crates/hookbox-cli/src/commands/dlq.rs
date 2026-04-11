//! `dlq` subcommands — inspect and retry dead-lettered receipts.

use std::io::Write as _;

use anyhow::Context as _;
use clap::Subcommand;
use uuid::Uuid;

use hookbox::state::ProcessingState;
use hookbox::traits::Storage;
use hookbox::types::ReceiptFilter;
use hookbox_postgres::PostgresStorage;

use crate::db;

/// Dead-letter queue inspection and retry commands.
#[derive(Subcommand)]
pub enum DlqCommand {
    /// List dead-lettered receipts.
    List {
        /// Database connection URL.
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,

        /// Filter by provider name.
        #[arg(long)]
        provider: Option<String>,

        /// Maximum number of receipts to return.
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },

    /// Inspect a single dead-lettered receipt by ID (JSON output).
    Inspect {
        /// Database connection URL.
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,

        /// Receipt UUID.
        receipt_id: Uuid,
    },

    /// Retry a dead-lettered receipt by ID.
    Retry {
        /// Database connection URL.
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,

        /// Receipt UUID to retry.
        receipt_id: Uuid,
    },
}

/// Execute a DLQ subcommand.
///
/// # Errors
///
/// Returns an error if the database connection or query fails.
pub async fn run(command: DlqCommand) -> anyhow::Result<()> {
    match command {
        DlqCommand::List {
            database_url,
            provider,
            limit,
        } => {
            let pool = db::connect(&database_url).await?;
            let storage = PostgresStorage::new(pool);

            let filter = ReceiptFilter {
                provider_name: provider,
                processing_state: Some(ProcessingState::DeadLettered),
                limit: Some(limit),
                ..ReceiptFilter::default()
            };

            let receipts = storage.query(filter).await?;

            for r in &receipts {
                tracing::info!(
                    id = %r.receipt_id,
                    provider = %r.provider_name,
                    state = ?r.processing_state,
                    received_at = %r.received_at,
                    "dead-lettered receipt"
                );
            }
            tracing::info!(count = receipts.len(), "total dead-lettered receipts");
        }

        DlqCommand::Inspect {
            database_url,
            receipt_id,
        } => {
            let pool = db::connect(&database_url).await?;
            let storage = PostgresStorage::new(pool);

            let receipt = storage
                .get(receipt_id)
                .await?
                .context("receipt not found")?;

            let json =
                serde_json::to_string_pretty(&receipt).context("failed to serialize receipt")?;
            std::io::stdout()
                .write_all(json.as_bytes())
                .context("failed to write to stdout")?;
            std::io::stdout()
                .write_all(b"\n")
                .context("failed to write newline")?;
        }

        DlqCommand::Retry {
            database_url,
            receipt_id,
        } => {
            let pool = db::connect(&database_url).await?;
            let storage = PostgresStorage::new(pool);
            storage.migrate().await.context("migration failed")?;

            storage.reset_for_retry(receipt_id).await?;
            tracing::info!(id = %receipt_id, "dead-lettered receipt queued for retry");
        }
    }
    Ok(())
}
