//! `dlq` subcommands — inspect and retry dead-lettered deliveries.

use std::io::Write as _;

use anyhow::Context as _;
use clap::Subcommand;
use uuid::Uuid;

use hookbox::DeliveryId;
use hookbox_postgres::{DeliveryStorage, PostgresStorage};

use crate::db;

/// Dead-letter queue inspection and retry commands.
#[derive(Subcommand)]
pub enum DlqCommand {
    /// List dead-lettered deliveries.
    ///
    /// Note: --provider was removed in the fan-out release — DLQ is now per-delivery,
    /// filter by --emitter instead.
    List {
        /// Database connection URL.
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,

        /// Filter by emitter name.
        #[arg(long)]
        emitter: Option<String>,

        /// Maximum number of dead-lettered deliveries to return.
        #[arg(long, default_value_t = 20)]
        limit: i64,

        /// Number of rows to skip (pagination).
        #[arg(long, default_value_t = 0)]
        offset: i64,
    },

    /// Inspect a single dead-lettered delivery by ID (JSON output).
    Inspect {
        /// Database connection URL.
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,

        /// Delivery UUID.
        delivery_id: Uuid,
    },

    /// Retry a dead-lettered delivery by ID.
    Retry {
        /// Database connection URL.
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,

        /// Delivery UUID to retry.
        delivery_id: Uuid,
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
            emitter,
            limit,
            offset,
        } => {
            let pool = db::connect(&database_url).await?;
            let storage = PostgresStorage::new(pool);

            let rows = storage
                .list_dlq(emitter.as_deref(), limit, offset)
                .await
                .context("failed to list dead-lettered deliveries")?;

            for (delivery, receipt) in &rows {
                tracing::info!(
                    delivery_id = %delivery.delivery_id,
                    receipt_id = %receipt.receipt_id,
                    emitter_name = %delivery.emitter_name,
                    state = ?delivery.state,
                    attempt_count = delivery.attempt_count,
                    next_attempt_at = %delivery.next_attempt_at,
                    "dead-lettered delivery"
                );
            }
            tracing::info!(count = rows.len(), "total dead-lettered deliveries");
        }

        DlqCommand::Inspect {
            database_url,
            delivery_id,
        } => {
            let pool = db::connect(&database_url).await?;
            let storage = PostgresStorage::new(pool);

            let (delivery, receipt) = storage
                .get_delivery(DeliveryId(delivery_id))
                .await
                .context("failed to fetch delivery")?
                .with_context(|| format!("delivery {delivery_id} not found"))?;

            let json = serde_json::to_string_pretty(
                &serde_json::json!({"delivery": delivery, "receipt": receipt}),
            )
            .context("failed to serialize delivery")?;
            std::io::stdout()
                .write_all(json.as_bytes())
                .context("failed to write to stdout")?;
            std::io::stdout()
                .write_all(b"\n")
                .context("failed to write newline")?;
        }

        DlqCommand::Retry {
            database_url,
            delivery_id,
        } => {
            let pool = db::connect(&database_url).await?;
            let storage = PostgresStorage::new(pool);

            let (delivery, receipt) = storage
                .get_delivery(DeliveryId(delivery_id))
                .await
                .context("failed to fetch delivery")?
                .with_context(|| format!("delivery {delivery_id} not found"))?;

            let new_id = storage
                .insert_replay(receipt.receipt_id, &delivery.emitter_name)
                .await
                .context("failed to insert replay delivery")?;

            tracing::info!(
                original_delivery_id = %delivery_id,
                new_delivery_id = %new_id,
                emitter = %delivery.emitter_name,
                "delivery queued for retry"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "acceptable in test code")]
#[expect(clippy::panic, reason = "acceptable in test code")]
mod tests {
    use super::*;

    #[derive(clap::Parser)]
    struct TestCli {
        #[command(subcommand)]
        cmd: DlqCommand,
    }

    #[test]
    fn list_parses_emitter_limit_offset() {
        use clap::Parser as _;

        let cli = TestCli::try_parse_from([
            "test",
            "list",
            "--database-url",
            "postgres://localhost/test",
            "--emitter",
            "kafka-billing",
            "--limit",
            "50",
            "--offset",
            "10",
        ])
        .expect("should parse");

        let DlqCommand::List {
            emitter,
            limit,
            offset,
            ..
        } = cli.cmd
        else {
            panic!("expected List variant");
        };
        assert_eq!(emitter, Some("kafka-billing".to_owned()));
        assert_eq!(limit, 50);
        assert_eq!(offset, 10);
    }

    #[test]
    fn inspect_parses_delivery_id() {
        use clap::Parser as _;

        let raw = "550e8400-e29b-41d4-a716-446655440000";
        let cli = TestCli::try_parse_from([
            "test",
            "inspect",
            "--database-url",
            "postgres://localhost/test",
            raw,
        ])
        .expect("should parse");

        let DlqCommand::Inspect { delivery_id, .. } = cli.cmd else {
            panic!("expected Inspect variant");
        };
        assert_eq!(delivery_id.to_string(), raw);
    }

    #[test]
    fn retry_parses_delivery_id() {
        use clap::Parser as _;

        let raw = "550e8400-e29b-41d4-a716-446655440001";
        let cli = TestCli::try_parse_from([
            "test",
            "retry",
            "--database-url",
            "postgres://localhost/test",
            raw,
        ])
        .expect("should parse");

        let DlqCommand::Retry { delivery_id, .. } = cli.cmd else {
            panic!("expected Retry variant");
        };
        assert_eq!(delivery_id.to_string(), raw);
    }

    #[test]
    fn list_rejects_old_provider_flag() {
        use clap::Parser as _;

        let result = TestCli::try_parse_from([
            "test",
            "list",
            "--database-url",
            "postgres://localhost/test",
            "--provider",
            "foo",
        ]);
        assert!(result.is_err(), "should reject unknown --provider flag");
    }

    #[test]
    fn inspect_rejects_missing_positional() {
        use clap::Parser as _;

        let result = TestCli::try_parse_from([
            "test",
            "inspect",
            "--database-url",
            "postgres://localhost/test",
        ]);
        assert!(
            result.is_err(),
            "should reject missing delivery_id positional"
        );
    }
}
