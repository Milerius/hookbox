//! `dlq` subcommands — inspect and retry dead-lettered deliveries.

use std::io::Write as _;

use anyhow::{Context as _, anyhow};
use clap::Subcommand;
use uuid::Uuid;

use hookbox::DeliveryId;
use hookbox_postgres::{DeliveryStorage, PostgresStorage};
use hookbox_server::config::parse_and_normalize;

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
    ///
    /// The delivery's emitter must still be present in the supplied config —
    /// retrying a delivery for an emitter that has since been removed would
    /// leave the row stranded with no worker to process it.
    Retry {
        /// Database connection URL.
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,

        /// Path to hookbox.toml — used to validate that the delivery's
        /// emitter is still configured before queuing a replay.
        #[arg(long, default_value = "hookbox.toml", value_name = "PATH")]
        config: String,

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
            config,
            delivery_id,
        } => {
            let raw = std::fs::read_to_string(&config)
                .with_context(|| format!("failed to read config file: {config}"))?;
            let (parsed, warnings) = parse_and_normalize(&raw)
                .with_context(|| format!("failed to parse config: {config}"))?;
            for warning in &warnings {
                let mut err = std::io::stderr();
                writeln!(err, "warning: {warning}").context("failed to write warning")?;
            }

            let pool = db::connect(&database_url).await?;
            let storage = PostgresStorage::new(pool);

            let (delivery, receipt) = storage
                .get_delivery(DeliveryId(delivery_id))
                .await
                .context("failed to fetch delivery")?
                .with_context(|| format!("delivery {delivery_id} not found"))?;

            if !parsed
                .emitters
                .iter()
                .any(|e| e.name == delivery.emitter_name)
            {
                return Err(anyhow!(
                    "refusing to replay delivery {delivery_id}: emitter {:?} is no longer present in {config}; configured emitters: {:?}",
                    delivery.emitter_name,
                    parsed.emitters.iter().map(|e| &e.name).collect::<Vec<_>>(),
                ));
            }

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
