//! Hookbox CLI — inspect, replay, and serve.

use clap::{Parser, Subcommand};

mod commands;

/// Hookbox — a durable webhook inbox.
#[derive(Parser)]
#[command(name = "hookbox", version, about)]
struct Cli {
    /// Path to configuration file.
    #[arg(short, long, default_value = "hookbox.toml")]
    config: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the webhook ingestion server.
    Serve,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Serve => commands::serve::run(&cli.config),
    }
}
