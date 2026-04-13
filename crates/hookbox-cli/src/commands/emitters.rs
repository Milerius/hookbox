//! `hookbox emitters list` — print configured emitters with their DB queue counts.

use std::io::Write as _;

use anyhow::Context as _;
use clap::Subcommand;

use hookbox_postgres::{DeliveryStorage as _, PostgresStorage};
use hookbox_server::config::parse_and_normalize;

use crate::db;

/// Emitter inspection commands.
#[derive(Subcommand)]
pub enum EmittersCommand {
    /// List configured emitters with current DB queue counts.
    ///
    /// Reads the configured emitter set from the TOML config and reports per-
    /// emitter `dlq`, `pending`, and `in_flight` counts from the deliveries
    /// table. Runtime health fields (consecutive failures, last success/failure
    /// timestamps) are not visible from the CLI; query `GET /api/emitters` or
    /// Prometheus for live status.
    List {
        /// Path to hookbox.toml.
        #[arg(long, default_value = "hookbox.toml", value_name = "PATH")]
        config: String,

        /// Database connection URL.
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,
    },
}

/// Execute an `emitters` subcommand.
///
/// # Errors
///
/// Returns an error if the config file cannot be read/parsed, the database
/// connection fails, or any per-emitter count query fails.
pub async fn run(command: EmittersCommand) -> anyhow::Result<()> {
    match command {
        EmittersCommand::List {
            config,
            database_url,
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

            // `parse_and_normalize` rejects configs with zero emitters, so
            // `parsed.emitters` is guaranteed non-empty here.
            let mut out = std::io::stdout();
            writeln!(
                out,
                "{:<24} {:<10} {:>8} {:>8} {:>10}",
                "NAME", "TYPE", "DLQ", "PENDING", "IN_FLIGHT"
            )
            .context("failed to write header")?;

            for emitter in &parsed.emitters {
                let dlq = storage
                    .count_dlq(&emitter.name)
                    .await
                    .with_context(|| format!("count_dlq failed for {}", emitter.name))?;
                let pending = storage
                    .count_pending(&emitter.name)
                    .await
                    .with_context(|| format!("count_pending failed for {}", emitter.name))?;
                let in_flight = storage
                    .count_in_flight(&emitter.name)
                    .await
                    .with_context(|| format!("count_in_flight failed for {}", emitter.name))?;

                writeln!(
                    out,
                    "{:<24} {:<10} {:>8} {:>8} {:>10}",
                    emitter.name, emitter.emitter_type, dlq, pending, in_flight
                )
                .context("failed to write row")?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "acceptable in test code")]
mod tests {
    use super::*;

    #[derive(clap::Parser)]
    struct TestCli {
        #[command(subcommand)]
        cmd: EmittersCommand,
    }

    #[test]
    fn list_parses_config_and_database_url() {
        use clap::Parser as _;

        let cli = TestCli::try_parse_from([
            "test",
            "list",
            "--config",
            "foo.toml",
            "--database-url",
            "bar",
        ])
        .expect("should parse");

        let EmittersCommand::List {
            config,
            database_url,
        } = cli.cmd;
        assert_eq!(config, "foo.toml");
        assert_eq!(database_url, "bar");
    }

    #[test]
    fn list_defaults_config_to_hookbox_toml() {
        use clap::Parser as _;

        let cli = TestCli::try_parse_from([
            "test",
            "list",
            "--database-url",
            "postgres://localhost/test",
        ])
        .expect("should parse");

        let EmittersCommand::List { config, .. } = cli.cmd;
        assert_eq!(config, "hookbox.toml");
    }
}
