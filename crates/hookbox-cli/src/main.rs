//! Hookbox CLI — inspect, replay, and serve.

use clap::{Parser, Subcommand};

mod commands;
mod db;

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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Serve => commands::serve::run(&cli.config),
        Commands::Receipts { command } => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(commands::receipts::run(command)),
        Commands::Replay { command } => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(commands::replay::run(command)),
        Commands::Dlq { command } => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(commands::dlq::run(command)),
    }
}
