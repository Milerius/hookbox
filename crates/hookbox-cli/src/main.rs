//! Hookbox CLI — inspect, replay, and serve.

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use hookbox_cli::commands;

/// Hookbox — a durable webhook inbox.
#[derive(Parser)]
#[command(name = "hookbox", version, about)]
struct Cli {
    /// Path to configuration file (used by `serve`).
    #[arg(short, long, default_value = "hookbox.toml")]
    config: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the webhook ingestion server.
    Serve,
    /// Inspect stored webhook receipts.
    Receipts {
        #[command(subcommand)]
        command: commands::receipts::ReceiptsCommand,
    },
    /// Replay failed webhook receipts.
    Replay {
        #[command(subcommand)]
        command: commands::replay::ReplayCommand,
    },
    /// Manage the dead-letter queue.
    Dlq {
        #[command(subcommand)]
        command: commands::dlq::DlqCommand,
    },
    /// Config-related utilities (validate, etc.).
    Config {
        #[command(subcommand)]
        command: commands::config::ConfigCommand,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        // `serve` installs its own JSON tracing subscriber.
        Commands::Serve => commands::serve::run(&cli.config),
        other => {
            init_cli_tracing();
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            match other {
                Commands::Receipts { command } => rt.block_on(commands::receipts::run(command)),
                Commands::Replay { command } => rt.block_on(commands::replay::run(command)),
                Commands::Dlq { command } => rt.block_on(commands::dlq::run(command)),
                Commands::Config { command } => rt.block_on(commands::config::run(command)),
                Commands::Serve => unreachable!(),
            }
        }
    }
}

/// Install a plain-text tracing subscriber for CLI (non-serve) commands.
fn init_cli_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .without_time()
        .init();
}
