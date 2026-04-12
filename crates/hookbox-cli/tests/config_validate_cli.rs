//! Integration tests for `hookbox config validate`.

#[expect(clippy::unwrap_used, reason = "test code")]
#[expect(clippy::expect_used, reason = "test code")]
#[test]
fn config_validate_exits_nonzero_on_bad_toml() {
    use std::io::Write as _;

    let path = std::env::temp_dir().join(format!("hookbox-test-{}.toml", uuid::Uuid::new_v4()));
    let mut f = std::fs::File::create(&path).expect("create temp file");
    f.write_all(b"[this is not valid toml!!!")
        .expect("write bad toml");
    drop(f);

    let status = std::process::Command::new(env!("CARGO_BIN_EXE_hookbox"))
        .args(["config", "validate", path.to_str().unwrap()])
        .status()
        .expect("run hookbox binary");

    let _ = std::fs::remove_file(&path);
    assert!(!status.success(), "invalid TOML must cause non-zero exit");
}

#[expect(clippy::unwrap_used, reason = "test code")]
#[expect(clippy::expect_used, reason = "test code")]
#[test]
fn config_validate_ok_output_on_valid_config() {
    use std::io::Write as _;

    let path = std::env::temp_dir().join(format!("hookbox-test-{}.toml", uuid::Uuid::new_v4()));
    let mut f = std::fs::File::create(&path).expect("create temp file");
    f.write_all(
        br#"
[database]
url = "postgres://localhost/test"

[[emitters]]
name = "default"
type = "channel"
"#,
    )
    .expect("write valid toml");
    drop(f);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_hookbox"))
        .args(["config", "validate", path.to_str().unwrap()])
        .output()
        .expect("run hookbox binary");

    let _ = std::fs::remove_file(&path);
    assert!(output.status.success(), "valid config must exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("OK: 1 emitter(s) configured"),
        "stdout must contain OK message; got: {stdout}"
    );
}
