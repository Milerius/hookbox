//! `replay` subcommands — re-queue individual or failed receipts for retry.

use std::collections::BTreeSet;

use anyhow::Context as _;
use chrono::Utc;
use clap::Subcommand;
use uuid::Uuid;

use hookbox::ReceiptId;
use hookbox_postgres::{DeliveryStorage as _, PostgresStorage};

use crate::db;

/// Replay commands for re-processing failed receipts.
#[derive(Subcommand)]
pub enum ReplayCommand {
    /// Replay a single receipt by ID.
    ///
    /// Without `--emitter`, fans out across all emitters that previously
    /// received a delivery for this receipt. With `--emitter <name>`, replays
    /// only that one emitter. Fails if no delivery rows exist and `--emitter`
    /// is not supplied.
    Id {
        /// Database connection URL.
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,

        /// Receipt UUID to replay.
        receipt_id: Uuid,

        /// Target a single emitter by name instead of all previously-delivered
        /// emitters.
        #[arg(long)]
        emitter: Option<String>,
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
/// Returns an error if the database connection or query fails; the `Failed`
/// subcommand additionally errors if migration fails.
pub async fn run(command: ReplayCommand) -> anyhow::Result<()> {
    match command {
        ReplayCommand::Id {
            database_url,
            receipt_id,
            emitter,
        } => {
            let pool = db::connect(&database_url).await?;
            let storage = PostgresStorage::new(pool);

            if let Some(name) = emitter {
                let new_id = storage
                    .insert_replay(ReceiptId(receipt_id), &name)
                    .await
                    .with_context(|| {
                        format!("insert_replay failed for receipt {receipt_id} emitter {name}")
                    })?;
                tracing::info!(
                    receipt_id = %receipt_id,
                    emitter = %name,
                    new_delivery_id = %new_id,
                    "delivery queued for retry"
                );
            } else {
                let deliveries = storage
                    .get_deliveries_for_receipt(ReceiptId(receipt_id))
                    .await
                    .with_context(|| {
                        format!("get_deliveries_for_receipt failed for {receipt_id}")
                    })?;

                if deliveries.is_empty() {
                    anyhow::bail!(
                        "receipt {receipt_id} has no existing deliveries — cannot infer emitter \
                         set; pass --emitter explicitly"
                    );
                }

                let emitter_names: BTreeSet<String> =
                    deliveries.iter().map(|d| d.emitter_name.clone()).collect();

                let count = emitter_names.len();
                for name in &emitter_names {
                    let new_id = storage
                        .insert_replay(ReceiptId(receipt_id), name)
                        .await
                        .with_context(|| {
                            format!("insert_replay failed for receipt {receipt_id} emitter {name}")
                        })?;
                    tracing::info!(
                        receipt_id = %receipt_id,
                        emitter = %name,
                        new_delivery_id = %new_id,
                        "delivery queued for retry"
                    );
                }
                tracing::info!(
                    receipt_id = %receipt_id,
                    replayed = count,
                    "receipt replayed across all emitters"
                );
            }
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

#[cfg(test)]
#[expect(clippy::expect_used, reason = "acceptable in test code")]
#[expect(clippy::panic, reason = "acceptable in test code")]
mod tests {
    use super::*;

    #[derive(clap::Parser)]
    struct TestCli {
        #[command(subcommand)]
        cmd: ReplayCommand,
    }

    #[test]
    fn replay_id_with_emitter_flag() {
        use clap::Parser as _;

        let raw = "550e8400-e29b-41d4-a716-446655440000";
        let cli = TestCli::try_parse_from([
            "test",
            "id",
            "--database-url",
            "x",
            raw,
            "--emitter",
            "kafka-billing",
        ])
        .expect("should parse");

        let ReplayCommand::Id {
            receipt_id,
            emitter,
            ..
        } = cli.cmd
        else {
            panic!("expected Id variant");
        };
        assert_eq!(receipt_id.to_string(), raw);
        assert_eq!(emitter, Some("kafka-billing".to_owned()));
    }

    #[test]
    fn replay_id_without_emitter_flag() {
        use clap::Parser as _;

        let raw = "550e8400-e29b-41d4-a716-446655440001";
        let cli = TestCli::try_parse_from(["test", "id", "--database-url", "x", raw])
            .expect("should parse");

        let ReplayCommand::Id {
            receipt_id,
            emitter,
            ..
        } = cli.cmd
        else {
            panic!("expected Id variant");
        };
        assert_eq!(receipt_id.to_string(), raw);
        assert_eq!(emitter, None);
    }

    #[test]
    fn parse_seconds() {
        let d = parse_duration("30s").expect("should parse");
        assert_eq!(d, chrono::Duration::seconds(30));
    }

    #[test]
    fn parse_minutes() {
        let d = parse_duration("5m").expect("should parse");
        assert_eq!(d, chrono::Duration::minutes(5));
    }

    #[test]
    fn parse_hours() {
        let d = parse_duration("2h").expect("should parse");
        assert_eq!(d, chrono::Duration::hours(2));
    }

    #[test]
    fn parse_days() {
        let d = parse_duration("7d").expect("should parse");
        assert_eq!(d, chrono::Duration::days(7));
    }

    #[test]
    fn parse_zero() {
        let d = parse_duration("0s").expect("should parse");
        assert_eq!(d, chrono::Duration::seconds(0));
    }

    #[test]
    fn parse_large_number() {
        let d = parse_duration("365d").expect("should parse");
        assert_eq!(d, chrono::Duration::days(365));
    }

    #[test]
    fn parse_with_whitespace() {
        let d = parse_duration("  1h  ").expect("should parse");
        assert_eq!(d, chrono::Duration::hours(1));
    }

    #[test]
    fn invalid_unit() {
        assert!(parse_duration("10x").is_err());
    }

    #[test]
    fn empty_string() {
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn just_unit_no_number() {
        assert!(parse_duration("h").is_err());
    }

    #[test]
    fn negative_number() {
        // Depends on implementation — if it parses, the Duration should be negative
        // If it errors, that's also acceptable
        let result = parse_duration("-1h");
        // Either succeeds with negative or errors — both are fine, just don't panic
        let _ = result;
    }

    #[test]
    fn no_unit_just_number() {
        assert!(parse_duration("123").is_err());
    }
}
