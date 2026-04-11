//! Tests for CLI argument parsing.

#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "acceptable in test code"
)]

use std::process::Command;

#[test]
fn cli_help_works() {
    let output = Command::new(env!("CARGO_BIN_EXE_hookbox"))
        .arg("--help")
        .output()
        .expect("failed to run hookbox");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("serve"));
    assert!(stdout.contains("receipts"));
    assert!(stdout.contains("replay"));
    assert!(stdout.contains("dlq"));
}

#[test]
fn receipts_help_works() {
    let output = Command::new(env!("CARGO_BIN_EXE_hookbox"))
        .args(["receipts", "--help"])
        .output()
        .expect("failed to run hookbox");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("list"));
    assert!(stdout.contains("inspect"));
    assert!(stdout.contains("search"));
}

#[test]
fn replay_help_works() {
    let output = Command::new(env!("CARGO_BIN_EXE_hookbox"))
        .args(["replay", "--help"])
        .output()
        .expect("failed to run hookbox");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("id"));
    assert!(stdout.contains("failed"));
}

#[test]
fn dlq_help_works() {
    let output = Command::new(env!("CARGO_BIN_EXE_hookbox"))
        .args(["dlq", "--help"])
        .output()
        .expect("failed to run hookbox");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("list"));
    assert!(stdout.contains("inspect"));
    assert!(stdout.contains("retry"));
}

#[test]
fn serve_requires_no_extra_args() {
    // serve should fail because hookbox.toml doesn't exist, not because of arg parsing
    let output = Command::new(env!("CARGO_BIN_EXE_hookbox"))
        .arg("serve")
        .output()
        .expect("failed to run hookbox");
    // It will fail (no config file) but not with a clap error
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    // Should be a config read error, not a clap usage error
    assert!(!stderr.contains("Usage:"));
}

#[test]
fn missing_subcommand_shows_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_hookbox"))
        .output()
        .expect("failed to run hookbox");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Usage:") || stderr.contains("hookbox"));
}
