//! `hookbox config <subcommand>` — configuration utilities.

use anyhow::Result;
use clap::Subcommand;
use std::path::PathBuf;

use crate::commands::config_validate;

/// Subcommands for `hookbox config`.
#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Validate a hookbox TOML config file without connecting to the database.
    Validate {
        /// Path to the hookbox.toml file to validate.
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },
}

/// Dispatch a `config` subcommand to its handler.
///
/// # Errors
///
/// Returns an error if the subcommand handler fails.
pub async fn run(command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Validate { path } => config_validate::run(path).await,
    }
}
