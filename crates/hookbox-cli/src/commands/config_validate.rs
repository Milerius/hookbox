//! `hookbox config validate` — load TOML, run normalization and validation,
//! print result. Does not connect to the database.

use std::path::PathBuf;

use anyhow::{Context as _, Result};

use hookbox_server::config::parse_and_normalize;

/// Validate a hookbox TOML config file without connecting to the database.
///
/// Prints `OK: N emitter(s) configured` plus a list on success, or exits
/// non-zero with a human-readable error message.
///
/// # Errors
///
/// Returns an error if the file cannot be read or if the TOML is invalid or
/// fails normalization.
#[expect(
    clippy::print_stdout,
    reason = "CLI command output is the entire purpose of this function"
)]
#[expect(
    clippy::print_stderr,
    reason = "warnings are surfaced on stderr per CLI convention"
)]
#[expect(
    clippy::unused_async,
    reason = "async is required for consistency with the rt.block_on dispatch pattern"
)]
pub async fn run(config_path: PathBuf) -> Result<()> {
    let raw = std::fs::read_to_string(&config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let (config, warnings) =
        parse_and_normalize(&raw).with_context(|| "normalizing config")?;
    for w in &warnings {
        eprintln!("WARNING: {w}");
    }
    println!("OK: {} emitter(s) configured", config.emitters.len());
    for e in &config.emitters {
        println!("  - {} (type={})", e.name, e.emitter_type);
    }
    Ok(())
}
