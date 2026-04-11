//! `receipts` subcommands — list, inspect, and search stored webhook receipts.

use std::io::Write as _;

use anyhow::Context as _;
use clap::Subcommand;
use uuid::Uuid;

use hookbox::state::ProcessingState;
use hookbox::traits::Storage;
use hookbox::types::ReceiptFilter;
use hookbox_postgres::PostgresStorage;

use crate::db;

/// Receipt inspection commands.
#[derive(Subcommand)]
pub enum ReceiptsCommand {
    /// List stored receipts with optional filters.
    List {
        /// Database connection URL.
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,

        /// Filter by provider name.
        #[arg(long)]
        provider: Option<String>,

        /// Filter by processing state (e.g. stored, emitted, `emit_failed`).
        #[arg(long)]
        state: Option<String>,

        /// Maximum number of receipts to return.
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },

    /// Inspect a single receipt by ID (JSON output).
    Inspect {
        /// Database connection URL.
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,

        /// Receipt UUID.
        receipt_id: Uuid,
    },

    /// Search receipts by external reference.
    ///
    /// Note: `external_reference` is only populated by provider-specific
    /// adapters that extract business references from the payload. The
    /// generic ingestion pipeline leaves this field `None`, so searches
    /// will return no results unless a custom adapter sets it.
    Search {
        /// Database connection URL.
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,

        /// External reference to search for.
        #[arg(long)]
        external_ref: String,

        /// Maximum number of results to return.
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
}

/// Execute a receipts subcommand.
///
/// # Errors
///
/// Returns an error if the database connection or query fails.
pub async fn run(command: ReceiptsCommand) -> anyhow::Result<()> {
    match command {
        ReceiptsCommand::List {
            database_url,
            provider,
            state,
            limit,
        } => {
            let pool = db::connect(&database_url).await?;
            let storage = PostgresStorage::new(pool);

            let processing_state = state
                .map(|s| parse_processing_state(&s))
                .transpose()
                .context("invalid --state value")?;

            let filter = ReceiptFilter {
                provider_name: provider,
                processing_state,
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
                    "receipt"
                );
            }
            tracing::info!(count = receipts.len(), "total receipts returned");
        }

        ReceiptsCommand::Inspect {
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

        ReceiptsCommand::Search {
            database_url,
            external_ref,
            limit,
        } => {
            let pool = db::connect(&database_url).await?;
            let storage = PostgresStorage::new(pool);

            let receipts = storage
                .query_by_external_reference(&external_ref, Some(limit))
                .await?;

            if receipts.is_empty() {
                tracing::warn!(
                    "no results — external_reference is only populated by provider-specific adapters"
                );
            }

            for r in &receipts {
                tracing::info!(
                    id = %r.receipt_id,
                    provider = %r.provider_name,
                    state = ?r.processing_state,
                    received_at = %r.received_at,
                    "receipt"
                );
            }
            tracing::info!(count = receipts.len(), "total receipts returned");
        }
    }
    Ok(())
}

/// Parse a CLI-friendly state string into a [`ProcessingState`].
fn parse_processing_state(s: &str) -> Result<ProcessingState, serde_json::Error> {
    let quoted = format!("\"{s}\"");
    serde_json::from_str(&quoted)
}
