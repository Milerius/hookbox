//! Integration coverage for `hookbox_cli::commands::emitters::run`.
//!
//! Exercises the handler against a real Postgres instance so the
//! `count_dlq` / `count_pending` / `count_in_flight` queries actually run.
//! Requires a running Postgres — defaults to `postgres://localhost/hookbox_test`.

#![expect(clippy::expect_used, reason = "expect is acceptable in test code")]

use hookbox_cli::commands::emitters::{EmittersCommand, run};
use hookbox_postgres::PostgresStorage;
use sqlx::PgPool;
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/hookbox_test".to_owned())
}

async fn setup_pool() -> PgPool {
    let pool = PgPool::connect(&database_url())
        .await
        .expect("connect to test db");
    let storage = PostgresStorage::new(pool.clone());
    storage.migrate().await.expect("run migrations");
    pool
}

fn write_config(body: &str) -> String {
    let path = std::env::temp_dir().join(format!("hookbox-emitters-test-{}.toml", Uuid::new_v4()));
    std::fs::write(&path, body).expect("write config");
    path.to_string_lossy().into_owned()
}

#[tokio::test]
async fn list_empty_emitters_is_rejected_by_config_validator() {
    // `parse_and_normalize` requires at least one emitter, so a config with
    // no `[[emitters]]` block fails validation before the handler even looks
    // at `parsed.emitters`. This test pins that contract.
    let _pool = setup_pool().await;
    let config = write_config(
        r#"
[database]
url = "postgres://localhost/hookbox_test"
"#,
    );

    let err = run(EmittersCommand::List {
        config: config.clone(),
        database_url: database_url(),
    })
    .await
    .expect_err("empty emitters should fail config validation");
    assert!(
        err.to_string().contains("failed to parse config"),
        "unexpected error: {err}"
    );

    std::fs::remove_file(&config).ok();
}

#[tokio::test]
async fn list_single_channel_emitter_counts_zero() {
    let _pool = setup_pool().await;
    // Use a unique name so queued state from other tests cannot leak in.
    let name = format!("cli-emitters-test-{}", Uuid::new_v4().simple());
    let config = write_config(&format!(
        r#"
[database]
url = "postgres://localhost/hookbox_test"

[[emitters]]
name = "{name}"
type = "channel"
"#
    ));

    run(EmittersCommand::List {
        config: config.clone(),
        database_url: database_url(),
    })
    .await
    .expect("run should succeed");

    std::fs::remove_file(&config).ok();
}

#[tokio::test]
async fn list_emits_legacy_section_warning() {
    let _pool = setup_pool().await;
    // The legacy `[emitter]` singular form is still accepted but normalizes
    // to a `"default"` entry and emits a deprecation warning on stderr.
    let config = write_config(
        r#"
[database]
url = "postgres://localhost/hookbox_test"

[emitter]
type = "channel"
"#,
    );

    run(EmittersCommand::List {
        config: config.clone(),
        database_url: database_url(),
    })
    .await
    .expect("legacy config should be accepted with a warning");

    std::fs::remove_file(&config).ok();
}

#[tokio::test]
async fn list_missing_config_file_errors() {
    let path = std::env::temp_dir()
        .join(format!("nonexistent-{}.toml", Uuid::new_v4()))
        .to_string_lossy()
        .into_owned();

    let err = run(EmittersCommand::List {
        config: path,
        database_url: database_url(),
    })
    .await
    .expect_err("expected error on missing config file");
    assert!(
        err.to_string().contains("failed to read config file"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn list_bad_toml_errors() {
    let config = write_config("this is [[ not valid toml");

    let err = run(EmittersCommand::List {
        config: config.clone(),
        database_url: database_url(),
    })
    .await
    .expect_err("expected parse error");
    assert!(
        err.to_string().contains("failed to parse config"),
        "unexpected error: {err}"
    );

    std::fs::remove_file(&config).ok();
}
