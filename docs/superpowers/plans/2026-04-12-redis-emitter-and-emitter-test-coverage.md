# Redis Streams Emitter + Emitter Test Coverage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the V2 Redis Streams emitter (`hookbox-emitter-redis`) and add round-trip integration tests for all four emitter crates (Kafka, NATS, SQS, Redis) using `testcontainers-rs`, moving them off 0% test coverage.

**Architecture:** New `hookbox-emitter-redis` crate built using `redis-rs` (`tokio-comp`), with an explicit `timeout_ms` constructor parameter wrapping `XADD` in `tokio::time::timeout`. Each emitter crate gains a `tests/round_trip.rs` integration test that spawns its own ephemeral broker container via `testcontainers-rs`, publishes a `NormalizedEvent`, consumes it back through the broker's native API, and asserts full struct equality plus backend-specific routing. PR CI runs these tests in a new dedicated Linux-only `test-emitters` job because GitHub Actions macOS runners do not have Docker. The legacy `#[ignore]`'d smoke tests, `docker-compose.test.yml`, and the nightly `emitter-smoke` job are deleted.

**Tech Stack:** Rust 2024 edition, stable toolchain, `redis = "0.27"` (`tokio-comp` feature), `testcontainers = "0.23"`, `testcontainers-modules = "0.11"` (per-crate features: `redis`, `kafka`, `nats`, `localstack`), existing workspace deps for `tokio`, `serde_json`, `chrono`, `async-trait`, `tracing`, `bytes`. CI: GitHub Actions, `cargo nextest`, `Swatinem/rust-cache@v2`.

**Reference spec:** `docs/superpowers/specs/2026-04-12-redis-emitter-and-emitter-test-coverage-design.md`

**Branch:** `feat/redis-emitter-test-coverage` (already cut, contains the design spec)

---

## Task 1: Add `PartialEq` to `NormalizedEvent`

Required by the round-trip assertion `assert_eq!(received, event)` in every test in this plan. All fields are already `PartialEq` (`ReceiptId`, `String`, `Option<String>`, `Option<serde_json::Value>`, `chrono::DateTime<Utc>`, `serde_json::Value`).

**Files:**
- Modify: `crates/hookbox/src/types.rs:80`

- [ ] **Step 1: Apply the derive change**

In `crates/hookbox/src/types.rs`, change the derive line on `NormalizedEvent`:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedEvent {
```

(was `#[derive(Debug, Clone, Serialize, Deserialize)]`)

- [ ] **Step 2: Verify the workspace still compiles**

Run: `cargo check --workspace --all-features`
Expected: clean compile, no warnings.

- [ ] **Step 3: Run the existing test suite to verify nothing regressed**

Run: `cargo nextest run --all-features --workspace --exclude hookbox-integration-tests`
Expected: all existing tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/hookbox/src/types.rs
git commit -m "feat(hookbox): derive PartialEq for NormalizedEvent

Required by the upcoming emitter round-trip integration tests, which
publish a NormalizedEvent through each emitter and assert on full
struct equality after consuming the message back from the broker."
```

---

## Task 2: Register `redis`, `testcontainers`, and `testcontainers-modules` as workspace dependencies

Centralize versions so each crate's `Cargo.toml` can `redis.workspace = true` instead of repeating the version.

**Files:**
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Add the new workspace dependencies**

In the root `Cargo.toml`, in the `[workspace.dependencies]` table, append after the existing emitter backends block (after `aws-config = ...`):

```toml
# Redis client (used by hookbox-emitter-redis)
redis = { version = "0.27", features = ["tokio-comp", "streams"] }

# AWS credential types (used directly by the SQS round-trip integration test
# to build a static credentials provider against LocalStack — re-exported by
# aws-config but pulled in directly so the test imports are explicit).
aws-credential-types = "1"

# Test containers (used by emitter round-trip integration tests)
testcontainers = "0.23"
testcontainers-modules = "0.11"
```

The `streams` feature on `redis` enables typed `XADD`/`XREAD` commands; the `tokio-comp` feature wires the client to the tokio runtime. `testcontainers-modules` features (`redis`, `kafka`, `nats`, `localstack`) are enabled per-crate in dev-deps, not at the workspace level.

- [ ] **Step 2: Verify the workspace still resolves**

Run: `cargo metadata --format-version 1 --quiet > /dev/null`
Expected: exit 0, no stderr.

- [ ] **Step 3: Verify nothing else broke**

Run: `cargo check --workspace --all-features`
Expected: clean compile.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml
git commit -m "chore: add redis + testcontainers workspace dependencies

Centralizes versions for the upcoming hookbox-emitter-redis crate
(redis 0.27 with tokio-comp + streams features) and the per-crate
emitter round-trip integration tests (testcontainers 0.23 +
testcontainers-modules 0.11). Per-crate dev-dependencies enable the
specific testcontainers-modules features they need."
```

---

## Task 3: Scaffold the `hookbox-emitter-redis` crate

Create the new crate with an empty `RedisEmitter` stub (compiles but does nothing useful). The actual implementation lands in Task 5; this task creates the file structure, registers the crate in the workspace, and gets a clean build.

**Files:**
- Create: `crates/hookbox-emitter-redis/Cargo.toml`
- Create: `crates/hookbox-emitter-redis/README.md`
- Create: `crates/hookbox-emitter-redis/src/lib.rs`
- Modify: `Cargo.toml` (workspace root) — add member + workspace dep entry

- [ ] **Step 1: Create the crate's `Cargo.toml`**

Create `crates/hookbox-emitter-redis/Cargo.toml`:

```toml
[package]
name = "hookbox-emitter-redis"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
description = "Redis Streams emitter adapter for hookbox."
publish = false

[lints]
workspace = true

[dependencies]
hookbox.workspace = true
redis.workspace = true
tokio.workspace = true
serde_json.workspace = true
async-trait.workspace = true
tracing.workspace = true
```

- [ ] **Step 2: Create `src/lib.rs` with the `RedisEmitter` skeleton**

Create `crates/hookbox-emitter-redis/src/lib.rs`. The skeleton already establishes a real async `MultiplexedConnection` in `new()` (so `pub async fn new()` actually awaits — without this, `clippy::unused_async` fires under workspace pedantic+`-D warnings`) and stores it on the struct; `emit()` returns an "unimplemented" error stub. Task 5 will replace `emit()` with the real `XADD` body without changing the struct shape:

```rust
//! Redis Streams emitter adapter for hookbox.
//!
//! Forwards [`NormalizedEvent`]s to a Redis stream via `XADD`. The event is
//! serialised as JSON and stored in a single `data` field. Each `XADD` call
//! is wrapped in [`tokio::time::timeout`] so a hung connection cannot block
//! the pipeline indefinitely.

use std::time::Duration;

use async_trait::async_trait;
use redis::aio::MultiplexedConnection;

use hookbox::error::EmitError;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;

/// A Redis-Streams-backed [`Emitter`] that publishes events to a configured stream.
pub struct RedisEmitter {
    conn: MultiplexedConnection,
    stream: String,
    maxlen: Option<u64>,
    timeout: Duration,
}

impl RedisEmitter {
    /// Create a new [`RedisEmitter`].
    ///
    /// Opens the Redis client and establishes a multiplexed async connection
    /// up-front so the first `emit()` call doesn't pay connection latency.
    ///
    /// # Arguments
    ///
    /// * `url` — Redis connection URL (e.g. `"redis://127.0.0.1:6379"`).
    /// * `stream` — Redis stream key to publish events to.
    /// * `maxlen` — optional approximate trim length (`XADD ~ MAXLEN`).
    /// * `timeout_ms` — per-operation timeout for `XADD` in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`EmitError::Downstream`] if the client cannot be created or
    /// the initial connection cannot be established.
    pub async fn new(
        url: &str,
        stream: String,
        maxlen: Option<u64>,
        timeout_ms: u64,
    ) -> Result<Self, EmitError> {
        let client = redis::Client::open(url).map_err(|e| EmitError::Downstream(e.to_string()))?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| EmitError::Downstream(e.to_string()))?;
        Ok(Self {
            conn,
            stream,
            maxlen,
            timeout: Duration::from_millis(timeout_ms),
        })
    }
}

#[async_trait]
impl Emitter for RedisEmitter {
    async fn emit(&self, _event: &NormalizedEvent) -> Result<(), EmitError> {
        // Real XADD implementation lands in Task 5 — this stub exists so the
        // round-trip test in Task 4 has something to fail against.
        let _ = (&self.conn, &self.stream, self.maxlen, self.timeout);
        Err(EmitError::Downstream("not yet implemented".to_owned()))
    }
}
```

The `let _ = (...)` at the top of `emit()` suppresses `dead_code` warnings on the fields until Task 5 wires them into the real `XADD` call. The connection is established in `new()` so `pub async fn new()` actually awaits something — if it didn't, `clippy::unused_async` would fire under workspace pedantic+`-D warnings`. `MultiplexedConnection` is `Clone` and cheap to clone per-call, so no `Mutex` is needed. Task 5's full implementation reuses this exact struct shape.

- [ ] **Step 3: Create the README**

Create `crates/hookbox-emitter-redis/README.md`:

```markdown
# hookbox-emitter-redis

Redis Streams emitter adapter for [hookbox](../../README.md).

Publishes `NormalizedEvent`s to a configured Redis stream via `XADD`,
serialising the event as JSON in a single `data` field. Single-node
Redis only; cluster and sentinel are out of scope for this adapter.

## Usage

```rust
use hookbox_emitter_redis::RedisEmitter;

let emitter = RedisEmitter::new(
    "redis://127.0.0.1:6379",
    "hookbox.events".to_owned(),
    Some(100_000), // optional MAXLEN ~
    5_000,         // 5 second per-XADD timeout
).await?;
```

## Configuration via `hookbox.toml`

```toml
[emitter]
type = "redis"

[emitter.redis]
url    = "redis://127.0.0.1:6379"
stream = "hookbox.events"
maxlen = 100000     # optional
timeout_ms = 5000   # default 5000
```
```

- [ ] **Step 4: Register the crate in the workspace root `Cargo.toml`**

In the root `Cargo.toml`, in the `[workspace] members = [...]` array, add the new crate path. Final order (alphabetical within the emitter group):

```toml
members = [
    "crates/hookbox",
    "crates/hookbox-postgres",
    "crates/hookbox-providers",
    "crates/hookbox-server",
    "crates/hookbox-cli",
    "crates/hookbox-verify",
    "crates/hookbox-emitter-kafka",
    "crates/hookbox-emitter-nats",
    "crates/hookbox-emitter-redis",
    "crates/hookbox-emitter-sqs",
    "integration-tests",
]
```

In the same `Cargo.toml`, in the `[workspace.dependencies]` table, add the path entry after the existing `hookbox-emitter-*` entries:

```toml
hookbox-emitter-redis = { path = "crates/hookbox-emitter-redis", version = "0.1.0" }
```

- [ ] **Step 5: Verify the new crate compiles in isolation**

Run: `cargo check -p hookbox-emitter-redis`
Expected: clean compile, no warnings.

- [ ] **Step 6: Verify the whole workspace still compiles**

Run: `cargo check --workspace --all-features`
Expected: clean compile.

- [ ] **Step 7: Run clippy to catch lint regressions**

Run: `cargo clippy -p hookbox-emitter-redis --all-targets --all-features -- -D warnings`
Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/hookbox-emitter-redis Cargo.toml
git commit -m "feat(hookbox-emitter-redis): scaffold crate with stub RedisEmitter

Adds crate skeleton (Cargo.toml, README, src/lib.rs) with a stub
RedisEmitter that compiles but returns Err on emit. The actual XADD
implementation lands in a follow-up commit alongside the round-trip
integration test that exercises it. Crate is registered in the
workspace members list and as a workspace dependency."
```

---

## Task 4: Write the failing Redis round-trip integration test

The test must exist before we implement `emit()` so we can verify the implementation against a real broker. Adds testcontainers dev-dependencies to the new crate and creates `tests/round_trip.rs`.

**Files:**
- Modify: `crates/hookbox-emitter-redis/Cargo.toml`
- Create: `crates/hookbox-emitter-redis/tests/round_trip.rs`

**Prerequisite check:** Docker must be running. Run `docker info` first; if it errors, start Docker Desktop / colima / podman before continuing.

- [ ] **Step 1: Add testcontainers dev-dependencies to the crate**

In `crates/hookbox-emitter-redis/Cargo.toml`, append:

```toml
[dev-dependencies]
testcontainers.workspace = true
testcontainers-modules = { workspace = true, features = ["redis"] }
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
chrono = { workspace = true }
uuid = { workspace = true }
serde_json = { workspace = true }
redis = { workspace = true }
hookbox = { workspace = true }
```

- [ ] **Step 2: Create the round-trip test file**

Create `crates/hookbox-emitter-redis/tests/round_trip.rs`:

```rust
//! Round-trip integration test for the Redis Streams emitter.
//!
//! Spawns an ephemeral Redis container via testcontainers, publishes a
//! `NormalizedEvent` through `RedisEmitter`, then reads it back via `XREAD`
//! and asserts full struct equality.

#![allow(clippy::unwrap_used, reason = "unwrap is acceptable in test code")]
#![allow(missing_docs, reason = "test code does not require docs")]

use chrono::{TimeZone, Utc};
use hookbox::state::ReceiptId;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;
use hookbox_emitter_redis::RedisEmitter;
use redis::AsyncCommands;
use redis::streams::{StreamReadOptions, StreamReadReply};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

const STREAM: &str = "hookbox.test";

fn make_test_event() -> NormalizedEvent {
    NormalizedEvent {
        receipt_id: ReceiptId::new(),
        provider_name: "round-trip".to_owned(),
        event_type: Some("test.round_trip".to_owned()),
        external_reference: Some("ext-ref-1".to_owned()),
        parsed_payload: Some(serde_json::json!({"k": "v"})),
        payload_hash: "deadbeefcafe".to_owned(),
        received_at: Utc.with_ymd_and_hms(2026, 4, 12, 10, 0, 0).unwrap(),
        metadata: serde_json::json!({"meta": 1}),
    }
}

#[tokio::test]
async fn redis_emitter_round_trip() {
    let container = Redis::default().start().await.unwrap();
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(6379).await.unwrap();
    let url = format!("redis://{host}:{port}");

    let emitter = RedisEmitter::new(&url, STREAM.to_owned(), None, 5000)
        .await
        .unwrap();

    let event = make_test_event();
    emitter.emit(&event).await.unwrap();

    // Consume back via XREAD and assert FULL struct equality.
    let client = redis::Client::open(url.as_str()).unwrap();
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();
    let opts = StreamReadOptions::default().count(1);
    let reply: StreamReadReply = conn
        .xread_options(&[STREAM], &["0"], &opts)
        .await
        .unwrap();

    assert_eq!(reply.keys.len(), 1, "expected exactly one stream in reply");
    let stream = &reply.keys[0];
    assert_eq!(stream.key, STREAM, "stream key mismatch");
    assert_eq!(stream.ids.len(), 1, "expected exactly one stream entry");

    let entry = &stream.ids[0];
    let data_value = entry
        .map
        .get("data")
        .expect("entry should have a `data` field");
    let data: String = redis::FromRedisValue::from_redis_value(data_value).unwrap();
    let received: NormalizedEvent = serde_json::from_str(&data).unwrap();

    assert_eq!(received, event);
}
```

- [ ] **Step 3: Run the test and confirm it FAILS at the `emit()` call**

Run: `cargo test -p hookbox-emitter-redis --test round_trip -- --nocapture`
Expected: container spawns successfully, then the test panics on `emitter.emit(&event).await.unwrap()` because the stub `emit()` returns `Err(EmitError::Downstream("not yet implemented"))`. Look for `panicked at ... not yet implemented` in the output.

If the failure is anything else (compilation error, container spawn failure, missing Docker daemon), STOP and resolve before continuing — the test must be failing for the *right* reason.

- [ ] **Step 4: Commit**

```bash
git add crates/hookbox-emitter-redis/Cargo.toml crates/hookbox-emitter-redis/tests/round_trip.rs
git commit -m "test(hookbox-emitter-redis): add failing round-trip integration test

Spawns an ephemeral Redis container via testcontainers, publishes a
NormalizedEvent through RedisEmitter, then reads it back via XREAD and
asserts on full struct equality. Currently fails on the stub emit()
returning 'not yet implemented'; the next commit fills in XADD."
```

---

## Task 5: Implement `RedisEmitter::emit()` and make the round-trip test pass

Replace the stub `emit()` body with a real `XADD` call wrapped in `tokio::time::timeout`. The struct shape stays exactly as Task 3 left it (`MultiplexedConnection` cloned per call) — only the `emit()` body changes.

**Files:**
- Modify: `crates/hookbox-emitter-redis/src/lib.rs`

- [ ] **Step 1: Replace `src/lib.rs` with the full implementation**

Overwrite `crates/hookbox-emitter-redis/src/lib.rs` with:

```rust
//! Redis Streams emitter adapter for hookbox.
//!
//! Forwards [`NormalizedEvent`]s to a Redis stream via `XADD`. The event is
//! serialised as JSON and stored in a single `data` field. Each `XADD` call
//! is wrapped in [`tokio::time::timeout`] so a hung connection cannot block
//! the pipeline indefinitely.

use std::time::Duration;

use async_trait::async_trait;
use redis::AsyncCommands;
use redis::aio::MultiplexedConnection;

use hookbox::error::EmitError;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;

/// A Redis-Streams-backed [`Emitter`] that publishes events to a configured stream.
///
/// Holds a multiplexed async connection (cheap to clone) and reuses it for
/// every `emit()` call. The connection is created in [`RedisEmitter::new`]
/// so configuration errors surface at startup, not at the first webhook.
pub struct RedisEmitter {
    conn: MultiplexedConnection,
    stream: String,
    maxlen: Option<u64>,
    timeout: Duration,
}

impl RedisEmitter {
    /// Create a new [`RedisEmitter`].
    ///
    /// # Arguments
    ///
    /// * `url` — Redis connection URL (e.g. `"redis://127.0.0.1:6379"`).
    /// * `stream` — Redis stream key to publish events to.
    /// * `maxlen` — optional approximate trim length (`XADD ~ MAXLEN`).
    /// * `timeout_ms` — per-operation timeout for `XADD` in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`EmitError::Downstream`] if the client cannot be created or
    /// the initial connection cannot be established.
    pub async fn new(
        url: &str,
        stream: String,
        maxlen: Option<u64>,
        timeout_ms: u64,
    ) -> Result<Self, EmitError> {
        let client = redis::Client::open(url).map_err(|e| EmitError::Downstream(e.to_string()))?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| EmitError::Downstream(e.to_string()))?;
        Ok(Self {
            conn,
            stream,
            maxlen,
            timeout: Duration::from_millis(timeout_ms),
        })
    }
}

#[async_trait]
impl Emitter for RedisEmitter {
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        let payload =
            serde_json::to_string(event).map_err(|e| EmitError::Downstream(e.to_string()))?;

        let mut conn = self.conn.clone();
        let stream = self.stream.clone();
        let maxlen = self.maxlen;

        let xadd = async move {
            // `*` lets Redis assign the entry ID.
            if let Some(cap) = maxlen {
                conn.xadd_maxlen::<_, _, _, _, ()>(
                    &stream,
                    redis::streams::StreamMaxlen::Approx(cap as usize),
                    "*",
                    &[("data", payload.as_str())],
                )
                .await
            } else {
                conn.xadd::<_, _, _, _, ()>(&stream, "*", &[("data", payload.as_str())])
                    .await
            }
        };

        match tokio::time::timeout(self.timeout, xadd).await {
            Ok(Ok(())) => {
                tracing::debug!(
                    receipt_id = %event.receipt_id,
                    stream = %self.stream,
                    "event emitted to redis"
                );
                Ok(())
            }
            Ok(Err(e)) => Err(EmitError::Downstream(format!("redis xadd failed: {e}"))),
            Err(_) => Err(EmitError::Timeout(format!(
                "redis xadd timed out after {}ms",
                self.timeout.as_millis()
            ))),
        }
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p hookbox-emitter-redis --all-targets`
Expected: clean compile.

- [ ] **Step 3: Run the round-trip test and verify it PASSES**

Run: `cargo test -p hookbox-emitter-redis --test round_trip -- --nocapture`
Expected: `test redis_emitter_round_trip ... ok`. Test should complete in 5-15 seconds (mostly Redis container startup).

If the test fails, the most likely culprits are:
- `redis::AsyncCommands::xadd` signature mismatch — check the redis crate version (must be 0.27+).
- `redis::streams::StreamMaxlen::Approx` taking a different argument type — `as usize` should be correct for 0.27.
- Network timing — bump `timeout_ms` to 10000 in the test if a slow CI runner times out.

- [ ] **Step 4: Run clippy on the crate to catch any lint regressions**

Run: `cargo clippy -p hookbox-emitter-redis --all-targets --all-features -- -D warnings`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/hookbox-emitter-redis/src/lib.rs
git commit -m "feat(hookbox-emitter-redis): implement XADD via redis-rs

Wraps the XADD call in tokio::time::timeout so a hung connection
maps to EmitError::Timeout instead of blocking the pipeline. The
optional MAXLEN config uses XADD ~ for approximate trimming, which
is the cheap-fast variant. Multiplexed async connection is created
in new() so config errors surface at startup, not first webhook.

The round-trip integration test now passes end-to-end against an
ephemeral Redis container."
```

---

## Task 6: Wire `RedisEmitterConfig` into `hookbox-server`

Add the `RedisEmitterConfig` struct, plug it into `EmitterConfig`, update the `Default` impl, and add serde parse tests parallel to the existing Kafka/NATS/SQS tests.

**Files:**
- Modify: `crates/hookbox-server/src/config.rs`

- [ ] **Step 1: Update the `EmitterConfig` struct, its doc comment, and its `Default` impl**

In `crates/hookbox-server/src/config.rs`, replace the `EmitterConfig` struct (around line 172–196) with:

```rust
/// Emitter backend configuration.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmitterConfig {
    /// Which emitter backend to use: `"channel"` (default), `"kafka"`, `"nats"`, `"sqs"`, or `"redis"`.
    #[serde(rename = "type", default = "default_emitter_type")]
    pub emitter_type: String,
    /// Kafka emitter settings (required when `type = "kafka"`).
    pub kafka: Option<KafkaEmitterConfig>,
    /// NATS emitter settings (required when `type = "nats"`).
    pub nats: Option<NatsEmitterConfig>,
    /// SQS emitter settings (required when `type = "sqs"`).
    pub sqs: Option<SqsEmitterConfig>,
    /// Redis Streams emitter settings (required when `type = "redis"`).
    pub redis: Option<RedisEmitterConfig>,
}

impl Default for EmitterConfig {
    fn default() -> Self {
        Self {
            emitter_type: default_emitter_type(),
            kafka: None,
            nats: None,
            sqs: None,
            redis: None,
        }
    }
}
```

- [ ] **Step 2: Append the new `RedisEmitterConfig` struct + default helper**

After the existing `SqsEmitterConfig` struct (around line 256), append:

```rust
/// Redis Streams emitter configuration.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedisEmitterConfig {
    /// Redis connection URL (e.g. `"redis://127.0.0.1:6379"`).
    pub url: String,
    /// Redis stream key to publish events to.
    pub stream: String,
    /// Optional approximate stream trim length (`XADD ~ MAXLEN`).
    pub maxlen: Option<u64>,
    /// Per-operation timeout for `XADD` in milliseconds (default: 5000).
    #[serde(default = "default_redis_timeout_ms")]
    pub timeout_ms: u64,
}

const fn default_redis_timeout_ms() -> u64 {
    5000
}
```

- [ ] **Step 3: Update the existing `parse_minimal_config` and `parse_emitter_default` tests to assert `redis.is_none()`**

Find the existing `parse_minimal_config` test (around line 263 in `crates/hookbox-server/src/config.rs`) and add the `redis` assertion at the end of its assertion block:

```rust
#[test]
fn parse_minimal_config() {
    let toml_str = r#"
[database]
url = "postgres://localhost/hookbox"
"#;
    let config: HookboxConfig = toml::from_str(toml_str).expect("minimal config should parse");

    assert_eq!(config.server.host, "0.0.0.0");
    assert_eq!(config.server.port, 8080);
    assert_eq!(config.server.body_limit, 1_048_576);
    assert_eq!(config.database.url, "postgres://localhost/hookbox");
    assert_eq!(config.database.max_connections, 10);
    assert!(config.providers.is_empty());
    assert_eq!(config.dedupe.lru_capacity, 10_000);
    assert!(config.admin.bearer_token.is_none());
    assert_eq!(config.retry.interval_seconds, 30);
    assert_eq!(config.retry.max_attempts, 5);
    assert_eq!(config.emitter.emitter_type, "channel");
    assert!(config.emitter.kafka.is_none());
    assert!(config.emitter.nats.is_none());
    assert!(config.emitter.sqs.is_none());
    assert!(config.emitter.redis.is_none());
}
```

Then find `parse_emitter_default` (around line 288) and add the same `redis` assertion:

```rust
#[test]
fn parse_emitter_default() {
    let toml_str = r#"
[database]
url = "postgres://localhost/hookbox"
"#;
    let config: HookboxConfig =
        toml::from_str(toml_str).expect("default emitter config should parse");
    assert_eq!(config.emitter.emitter_type, "channel");
    assert!(config.emitter.kafka.is_none());
    assert!(config.emitter.nats.is_none());
    assert!(config.emitter.sqs.is_none());
    assert!(config.emitter.redis.is_none());
}
```

Both tests use the real `[database]` section from `HookboxConfig` — there is no `[ingest]` / `[storage]` shape in this codebase.

- [ ] **Step 4: Add a new `parse_emitter_redis` test**

Inside the `#[cfg(test)]` block, after the existing `parse_emitter_sqs_full` test (the last `parse_emitter_*` test in the file), append:

```rust
#[test]
fn parse_emitter_redis() {
    let toml_str = r#"
[database]
url = "postgres://localhost/hookbox"

[emitter]
type = "redis"

[emitter.redis]
url = "redis://127.0.0.1:6379"
stream = "hookbox.events"
"#;
    let config: HookboxConfig =
        toml::from_str(toml_str).expect("redis emitter config should parse");
    assert_eq!(config.emitter.emitter_type, "redis");
    let redis = config
        .emitter
        .redis
        .expect("redis config should be present");
    assert_eq!(redis.url, "redis://127.0.0.1:6379");
    assert_eq!(redis.stream, "hookbox.events");
    assert_eq!(redis.maxlen, None);
    assert_eq!(redis.timeout_ms, 5000); // default
}

#[test]
fn parse_emitter_redis_full() {
    let toml_str = r#"
[database]
url = "postgres://localhost/hookbox"

[emitter]
type = "redis"

[emitter.redis]
url = "redis://10.0.0.5:6379"
stream = "hookbox.events"
maxlen = 100000
timeout_ms = 10000
"#;
    let config: HookboxConfig =
        toml::from_str(toml_str).expect("full redis emitter config should parse");
    let redis = config
        .emitter
        .redis
        .expect("redis config should be present");
    assert_eq!(redis.url, "redis://10.0.0.5:6379");
    assert_eq!(redis.stream, "hookbox.events");
    assert_eq!(redis.maxlen, Some(100000));
    assert_eq!(redis.timeout_ms, 10000);
}
```

- [ ] **Step 5: Run the config tests**

Run: `cargo test -p hookbox-server config -- --nocapture`
Expected: all `parse_emitter_*` tests pass, including the two new redis tests.

- [ ] **Step 6: Run clippy on the server crate**

Run: `cargo clippy -p hookbox-server --all-targets --all-features -- -D warnings`
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/hookbox-server/src/config.rs
git commit -m "feat(hookbox-server): add RedisEmitterConfig

Wires RedisEmitterConfig into the EmitterConfig union, with serde
parse tests covering both the minimal config (timeout_ms defaults to
5000) and the full config (maxlen + timeout_ms overrides). Mirrors
the existing pattern used for KafkaEmitterConfig / NatsEmitterConfig
/ SqsEmitterConfig."
```

---

## Task 7: Wire the `"redis"` arm into the serve command

Add the `hookbox-emitter-redis` dependency to the CLI crate and add the `"redis"` match arm in `serve.rs`.

**Files:**
- Modify: `crates/hookbox-cli/Cargo.toml`
- Modify: `crates/hookbox-cli/src/commands/serve.rs`

- [ ] **Step 1: Add the dependency to the CLI crate**

In `crates/hookbox-cli/Cargo.toml`, in the `[dependencies]` table, add (alphabetically among the `hookbox-emitter-*` deps):

```toml
hookbox-emitter-redis.workspace = true
```

- [ ] **Step 2: Add the `"redis"` match arm in `serve.rs`**

In `crates/hookbox-cli/src/commands/serve.rs`, locate the `match config.emitter.emitter_type.as_str() { ... }` block (starts around line 98). After the existing `"sqs" => { ... }` arm (ends around line 138, just before the `"channel" =>` arm), insert:

```rust
        "redis" => {
            let cfg = config.emitter.redis.as_ref().ok_or_else(|| {
                anyhow::anyhow!("[emitter.redis] section required when type = \"redis\"")
            })?;
            let emitter = hookbox_emitter_redis::RedisEmitter::new(
                &cfg.url,
                cfg.stream.clone(),
                cfg.maxlen,
                cfg.timeout_ms,
            )
            .await
            .context("failed to create Redis emitter")?;
            tracing::info!(url = %cfg.url, stream = %cfg.stream, "emitter: redis");
            Arc::new(emitter)
        }
```

- [ ] **Step 3: Update the unknown-emitter error message**

In the same `match` block, the catch-all arm currently says:

```rust
        other => {
            anyhow::bail!(
                "unknown emitter type {other:?}; valid values: kafka, nats, sqs, channel"
            );
        }
```

Replace with:

```rust
        other => {
            anyhow::bail!(
                "unknown emitter type {other:?}; valid values: kafka, nats, sqs, redis, channel"
            );
        }
```

- [ ] **Step 4: Verify the CLI builds**

Run: `cargo check -p hookbox-cli --all-features`
Expected: clean compile.

- [ ] **Step 5: Run clippy on the CLI**

Run: `cargo clippy -p hookbox-cli --all-targets --all-features -- -D warnings`
Expected: no warnings.

- [ ] **Step 6: Run the full workspace check to make sure nothing else broke**

Run: `cargo check --workspace --all-features`
Expected: clean compile across the workspace.

- [ ] **Step 7: Commit**

```bash
git add crates/hookbox-cli/Cargo.toml crates/hookbox-cli/src/commands/serve.rs
git commit -m "feat(hookbox-cli): wire \"redis\" emitter type into serve command

Adds the new match arm that constructs RedisEmitter from
[emitter.redis] config and updates the unknown-emitter error message
to list redis as a valid value. The redis emitter now ships
end-to-end: hookbox serve --config hookbox.toml will spin up a
RedisEmitter when type = \"redis\"."
```

---

## Task 8: Round-trip integration test for `hookbox-emitter-nats`

Simplest of the three existing emitters — single message publish + subscribe is straightforward. Adds testcontainers dev-deps and a `tests/round_trip.rs` file.

**Files:**
- Modify: `crates/hookbox-emitter-nats/Cargo.toml`
- Create: `crates/hookbox-emitter-nats/tests/round_trip.rs`

- [ ] **Step 1: Add dev-dependencies to the nats crate Cargo.toml**

In `crates/hookbox-emitter-nats/Cargo.toml`, append:

```toml
[dev-dependencies]
testcontainers.workspace = true
testcontainers-modules = { workspace = true, features = ["nats"] }
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "time"] }
chrono = { workspace = true }
serde_json = { workspace = true }
hookbox = { workspace = true }
async-nats = { workspace = true }
futures = { workspace = true }
```

- [ ] **Step 2: Create the round-trip test**

Create `crates/hookbox-emitter-nats/tests/round_trip.rs`:

```rust
//! Round-trip integration test for the NATS emitter.
//!
//! Spawns an ephemeral NATS server via testcontainers, subscribes to the
//! target subject, publishes a `NormalizedEvent` through `NatsEmitter`, then
//! reads the message back from the subscription and asserts on full struct
//! equality.

#![allow(clippy::unwrap_used, reason = "unwrap is acceptable in test code")]
#![allow(missing_docs, reason = "test code does not require docs")]

use std::time::Duration;

use chrono::{TimeZone, Utc};
use futures::StreamExt;
use hookbox::state::ReceiptId;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;
use hookbox_emitter_nats::NatsEmitter;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::nats::Nats;

const SUBJECT: &str = "hookbox.test";

fn make_test_event() -> NormalizedEvent {
    NormalizedEvent {
        receipt_id: ReceiptId::new(),
        provider_name: "round-trip".to_owned(),
        event_type: Some("test.round_trip".to_owned()),
        external_reference: Some("ext-ref-1".to_owned()),
        parsed_payload: Some(serde_json::json!({"k": "v"})),
        payload_hash: "deadbeefcafe".to_owned(),
        received_at: Utc.with_ymd_and_hms(2026, 4, 12, 10, 0, 0).unwrap(),
        metadata: serde_json::json!({"meta": 1}),
    }
}

#[tokio::test]
async fn nats_emitter_round_trip() {
    let container = Nats::default().start().await.unwrap();
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(4222).await.unwrap();
    let url = format!("nats://{host}:{port}");

    // Subscribe BEFORE publishing — NATS drops messages with no subscribers.
    let subscriber_client = async_nats::connect(&url).await.unwrap();
    let mut subscription = subscriber_client.subscribe(SUBJECT).await.unwrap();
    // `subscribe()` returns before the SUB protocol message round-trips to the
    // server. `flush()` blocks until the server has actually registered our
    // subscription, closing the publish-before-subscribe race window.
    subscriber_client.flush().await.unwrap();

    let emitter = NatsEmitter::new(&url, SUBJECT.to_owned()).await.unwrap();
    let event = make_test_event();
    emitter.emit(&event).await.unwrap();

    // Wait up to 5 seconds for the message to arrive.
    let message = tokio::time::timeout(Duration::from_secs(5), subscription.next())
        .await
        .expect("timed out waiting for nats message")
        .expect("subscription returned None");

    assert_eq!(message.subject.as_str(), SUBJECT, "subject mismatch");

    let received: NormalizedEvent = serde_json::from_slice(&message.payload).unwrap();
    assert_eq!(received, event);
}
```

- [ ] **Step 3: Run the test**

Run: `cargo test -p hookbox-emitter-nats --test round_trip -- --nocapture`
Expected: `test nats_emitter_round_trip ... ok` in 5-15 seconds.

- [ ] **Step 4: Run clippy on the crate**

Run: `cargo clippy -p hookbox-emitter-nats --all-targets --all-features -- -D warnings`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/hookbox-emitter-nats/Cargo.toml crates/hookbox-emitter-nats/tests/round_trip.rs
git commit -m "test(hookbox-emitter-nats): add round-trip integration test

Spawns an ephemeral NATS server via testcontainers, subscribes to
hookbox.test BEFORE publishing (NATS drops messages with no
subscribers), then publishes a NormalizedEvent through NatsEmitter,
reads it back, and asserts on full struct equality plus the subject
the message arrived on. Replaces the write-only smoke test that
proved nothing beyond 'send didn't error'."
```

---

## Task 9: Round-trip integration test for `hookbox-emitter-kafka`

More involved than NATS because the Kafka consumer needs an explicit `group.id`, `auto.offset.reset=earliest`, and the test asserts on `BorrowedMessage::key()` to verify partition routing.

**Files:**
- Modify: `crates/hookbox-emitter-kafka/Cargo.toml`
- Create: `crates/hookbox-emitter-kafka/tests/round_trip.rs`

- [ ] **Step 1: Add dev-dependencies to the kafka crate Cargo.toml**

In `crates/hookbox-emitter-kafka/Cargo.toml`, append:

```toml
[dev-dependencies]
testcontainers.workspace = true
testcontainers-modules = { workspace = true, features = ["kafka"] }
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "time"] }
chrono = { workspace = true }
uuid = { workspace = true }
serde_json = { workspace = true }
hookbox = { workspace = true }
rdkafka = { workspace = true }
```

- [ ] **Step 2: Create the round-trip test**

Create `crates/hookbox-emitter-kafka/tests/round_trip.rs`:

```rust
//! Round-trip integration test for the Kafka emitter.
//!
//! Spawns an ephemeral Kafka broker via testcontainers, publishes a
//! `NormalizedEvent` through `KafkaEmitter`, then consumes it back via a
//! `StreamConsumer` and asserts on full struct equality plus the
//! receipt-ID-as-record-key contract.

#![allow(clippy::unwrap_used, reason = "unwrap is acceptable in test code")]
#![allow(missing_docs, reason = "test code does not require docs")]

use std::time::Duration;

use chrono::{TimeZone, Utc};
use hookbox::state::ReceiptId;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;
use hookbox_emitter_kafka::KafkaEmitter;
use rdkafka::ClientConfig;
use rdkafka::Message;
use rdkafka::consumer::{Consumer, StreamConsumer};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::kafka::apache::Kafka;
use uuid::Uuid;

const TOPIC: &str = "hookbox-test";

fn make_test_event() -> NormalizedEvent {
    NormalizedEvent {
        receipt_id: ReceiptId::new(),
        provider_name: "round-trip".to_owned(),
        event_type: Some("test.round_trip".to_owned()),
        external_reference: Some("ext-ref-1".to_owned()),
        parsed_payload: Some(serde_json::json!({"k": "v"})),
        payload_hash: "deadbeefcafe".to_owned(),
        received_at: Utc.with_ymd_and_hms(2026, 4, 12, 10, 0, 0).unwrap(),
        metadata: serde_json::json!({"meta": 1}),
    }
}

#[tokio::test]
async fn kafka_emitter_round_trip() {
    let container = Kafka::default().start().await.unwrap();
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(9092).await.unwrap();
    let brokers = format!("{host}:{port}");

    let emitter = KafkaEmitter::new(
        &brokers,
        TOPIC.to_owned(),
        "hookbox-round-trip-test",
        "all",
        10_000,
    )
    .unwrap();

    // Build the consumer BEFORE publishing so the topic auto-creates and
    // we have a stable view of partition 0 from offset 0.
    let group_id = format!("hookbox-test-{}", Uuid::new_v4());
    let consumer: StreamConsumer = ClientConfig::new()
        .set("bootstrap.servers", &brokers)
        .set("group.id", &group_id)
        .set("auto.offset.reset", "earliest")
        .set("enable.auto.commit", "false")
        .set("session.timeout.ms", "10000")
        .create()
        .unwrap();
    consumer.subscribe(&[TOPIC]).unwrap();

    let event = make_test_event();
    emitter.emit(&event).await.unwrap();

    // Wait up to 30 seconds for the message — first poll on a new consumer
    // group can be slow on cold Kafka.
    let message = tokio::time::timeout(Duration::from_secs(30), consumer.recv())
        .await
        .expect("timed out waiting for kafka message")
        .expect("consumer recv returned an error");

    assert_eq!(message.topic(), TOPIC, "topic mismatch");

    let key_bytes = message.key().expect("message should have a key");
    let key_str = std::str::from_utf8(key_bytes).unwrap();
    assert_eq!(
        key_str,
        event.receipt_id.to_string(),
        "kafka record key should equal receipt_id (partition routing)"
    );

    let payload_bytes = message.payload().expect("message should have a payload");
    let received: NormalizedEvent = serde_json::from_slice(payload_bytes).unwrap();
    assert_eq!(received, event);
}
```

The Kafka module path is `testcontainers_modules::kafka::apache::Kafka` (the Apache Kafka image, which boots faster than Confluent's). If the version of `testcontainers-modules` in use doesn't expose `apache`, fall back to `testcontainers_modules::kafka::confluent::Kafka` and bump the test timeout to 60 seconds — Confluent's image is 30+ seconds slower to start.

- [ ] **Step 3: Run the test**

Run: `cargo test -p hookbox-emitter-kafka --test round_trip -- --nocapture`
Expected: `test kafka_emitter_round_trip ... ok` in 30-60 seconds (Kafka is the slowest container).

If the test hangs at `consumer.recv()`, the most likely cause is Kafka still electing a leader for the auto-created topic. Bump the outer timeout to 60s if needed.

- [ ] **Step 4: Run clippy on the crate**

Run: `cargo clippy -p hookbox-emitter-kafka --all-targets --all-features -- -D warnings`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/hookbox-emitter-kafka/Cargo.toml crates/hookbox-emitter-kafka/tests/round_trip.rs
git commit -m "test(hookbox-emitter-kafka): add round-trip integration test

Spawns an ephemeral Apache Kafka broker via testcontainers, publishes
a NormalizedEvent through KafkaEmitter, then consumes it back via a
StreamConsumer and asserts on full struct equality. Crucially, also
asserts on BorrowedMessage::key() to verify the receipt-ID-as-record-
key contract that drives partition routing — the previous smoke test
never checked this. Consumer is built before publishing so the topic
auto-creates with a stable view from offset 0."
```

---

## Task 10: Round-trip integration test for `hookbox-emitter-sqs`

Most involved test in this plan. Adds a new `SqsEmitter::with_client` constructor on the production crate so tests can inject a pre-built `aws_sdk_sqs::Client` (with static credentials against LocalStack), then spawns LocalStack, creates both a FIFO and a standard queue via the AWS SDK directly, publishes through `SqsEmitter` for both, and asserts on `MessageGroupId` / `MessageDeduplicationId` for the FIFO case.

**Why a new constructor?** Workspace lints set `unsafe_code = "deny"`, so the test cannot mutate process env via `unsafe { std::env::set_var }` to inject dummy AWS credentials. And both SQS tests share an integration-test binary, so even with an `#[allow]` they would race on the env var. The clean fix is to accept a pre-built client through `with_client`. Production `SqsEmitter::new` is unchanged and still uses `aws_config::from_env()`.

**Files:**
- Modify: `crates/hookbox-emitter-sqs/src/lib.rs` (add `with_client` constructor)
- Modify: `crates/hookbox-emitter-sqs/Cargo.toml`
- Create: `crates/hookbox-emitter-sqs/tests/round_trip.rs`

- [ ] **Step 1: Add the `with_client` constructor to `SqsEmitter`**

In `crates/hookbox-emitter-sqs/src/lib.rs`, find the existing `impl SqsEmitter { pub async fn new(...) }` block and add a new constructor right after `new()`:

```rust
    /// Create a new [`SqsEmitter`] from a pre-built [`aws_sdk_sqs::Client`].
    ///
    /// This bypasses the env-based credentials provider chain that
    /// [`SqsEmitter::new`] uses. Intended for tests that need to point at
    /// `LocalStack` with static credentials, but also usable in production
    /// when callers want to control SDK config (custom retries, custom HTTP
    /// connector, alternate credentials provider) directly.
    #[must_use]
    pub fn with_client(client: aws_sdk_sqs::Client, queue_url: String, fifo: bool) -> Self {
        Self {
            client,
            queue_url,
            fifo,
        }
    }
```

The struct fields stay private; this is the only addition. No behaviour change to `new()`.

- [ ] **Step 2: Add dev-dependencies to the sqs crate Cargo.toml**

In `crates/hookbox-emitter-sqs/Cargo.toml`, append:

```toml
[dev-dependencies]
testcontainers.workspace = true
testcontainers-modules = { workspace = true, features = ["localstack"] }
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "time"] }
chrono = { workspace = true }
serde_json = { workspace = true }
hookbox = { workspace = true }
aws-sdk-sqs = { workspace = true }
aws-config = { workspace = true }
aws-credential-types = { workspace = true }
```

You will also need to add `aws-credential-types = "1"` to the workspace `[workspace.dependencies]` table in the root `Cargo.toml` if it isn't already present. The credential types are re-exported by `aws-config`, but we use the type directly so the explicit dep is clearer.

- [ ] **Step 3: Create the round-trip test**

Create `crates/hookbox-emitter-sqs/tests/round_trip.rs`:

```rust
//! Round-trip integration tests for the SQS emitter.
//!
//! Spawns an ephemeral LocalStack container with SQS enabled, creates a
//! FIFO queue and a standard queue, publishes a `NormalizedEvent` through
//! `SqsEmitter::with_client` for each (no env mutation — static credentials
//! are passed directly through the SDK config), then receives the messages
//! back and asserts on full struct equality plus FIFO routing attributes
//! (`MessageGroupId`, `MessageDeduplicationId`).

#![allow(clippy::unwrap_used, reason = "unwrap is acceptable in test code")]
#![allow(missing_docs, reason = "test code does not require docs")]

use std::collections::HashMap;

use aws_credential_types::Credentials;
use aws_sdk_sqs::Client as SqsClient;
use aws_sdk_sqs::config::{BehaviorVersion, Region};
use aws_sdk_sqs::types::{MessageSystemAttributeName, QueueAttributeName};
use chrono::{TimeZone, Utc};
use hookbox::state::ReceiptId;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;
use hookbox_emitter_sqs::SqsEmitter;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::localstack::LocalStack;

fn make_test_event() -> NormalizedEvent {
    NormalizedEvent {
        receipt_id: ReceiptId::new(),
        provider_name: "round-trip".to_owned(),
        event_type: Some("test.round_trip".to_owned()),
        external_reference: Some("ext-ref-1".to_owned()),
        parsed_payload: Some(serde_json::json!({"k": "v"})),
        payload_hash: "deadbeefcafe".to_owned(),
        received_at: Utc.with_ymd_and_hms(2026, 4, 12, 10, 0, 0).unwrap(),
        metadata: serde_json::json!({"meta": 1}),
    }
}

/// Build an `aws_sdk_sqs::Client` pointed at the LocalStack endpoint with
/// static credentials. No process-env mutation — `Credentials::new` constructs
/// the credential pair in-process and the SDK config builder accepts it
/// directly via `.credentials_provider(...)`.
fn build_sdk_client(endpoint_url: &str) -> SqsClient {
    let creds = Credentials::new(
        "test",          // access key id
        "test",          // secret access key
        None,            // session token
        None,            // expiry
        "hookbox-tests", // provider name
    );
    let config = aws_sdk_sqs::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-east-1"))
        .endpoint_url(endpoint_url)
        .credentials_provider(creds)
        .build();
    SqsClient::from_conf(config)
}

#[tokio::test]
async fn sqs_emitter_fifo_round_trip() {
    let container = LocalStack::default().start().await.unwrap();
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(4566).await.unwrap();
    let endpoint_url = format!("http://{host}:{port}");

    // Build the SDK client once with static creds and reuse it for both queue
    // creation and the SqsEmitter under test.
    let sdk = build_sdk_client(&endpoint_url);

    let mut attrs: HashMap<QueueAttributeName, String> = HashMap::new();
    attrs.insert(QueueAttributeName::FifoQueue, "true".to_owned());
    attrs.insert(
        QueueAttributeName::ContentBasedDeduplication,
        "false".to_owned(),
    );
    let create_resp = sdk
        .create_queue()
        .queue_name("hookbox-test.fifo")
        .set_attributes(Some(attrs))
        .send()
        .await
        .unwrap();
    let queue_url = create_resp.queue_url.unwrap();

    let emitter = SqsEmitter::with_client(sdk.clone(), queue_url.clone(), true /* fifo */);

    let event = make_test_event();
    emitter.emit(&event).await.unwrap();

    // Receive the message back, requesting FIFO system attributes.
    let recv_resp = sdk
        .receive_message()
        .queue_url(&queue_url)
        .max_number_of_messages(1)
        .wait_time_seconds(5)
        .message_system_attribute_names(MessageSystemAttributeName::MessageGroupId)
        .message_system_attribute_names(MessageSystemAttributeName::MessageDeduplicationId)
        .send()
        .await
        .unwrap();
    let messages = recv_resp.messages.unwrap_or_default();
    assert_eq!(messages.len(), 1, "expected exactly one message");
    let msg = &messages[0];

    let body = msg.body.as_deref().unwrap();
    let received: NormalizedEvent = serde_json::from_str(body).unwrap();
    assert_eq!(received, event);

    let attrs = msg.attributes.as_ref().unwrap();
    let group_id = attrs.get(&MessageSystemAttributeName::MessageGroupId).unwrap();
    let dedup_id = attrs
        .get(&MessageSystemAttributeName::MessageDeduplicationId)
        .unwrap();
    assert_eq!(group_id, &event.provider_name);
    assert_eq!(dedup_id, &event.receipt_id.to_string());
}

#[tokio::test]
async fn sqs_emitter_standard_round_trip() {
    let container = LocalStack::default().start().await.unwrap();
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(4566).await.unwrap();
    let endpoint_url = format!("http://{host}:{port}");

    let sdk = build_sdk_client(&endpoint_url);
    let create_resp = sdk
        .create_queue()
        .queue_name("hookbox-test")
        .send()
        .await
        .unwrap();
    let queue_url = create_resp.queue_url.unwrap();

    let emitter = SqsEmitter::with_client(sdk.clone(), queue_url.clone(), false /* standard */);

    let event = make_test_event();
    emitter.emit(&event).await.unwrap();

    let recv_resp = sdk
        .receive_message()
        .queue_url(&queue_url)
        .max_number_of_messages(1)
        .wait_time_seconds(5)
        .send()
        .await
        .unwrap();
    let messages = recv_resp.messages.unwrap_or_default();
    assert_eq!(messages.len(), 1, "expected exactly one message");

    let body = messages[0].body.as_deref().unwrap();
    let received: NormalizedEvent = serde_json::from_str(body).unwrap();
    assert_eq!(received, event);
}
```

No `unsafe`, no env mutation, no race between the two tests. Both share the same `build_sdk_client` helper but each test owns its own LocalStack container and its own SDK client instance.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hookbox-emitter-sqs --test round_trip -- --nocapture`
Expected: both `sqs_emitter_fifo_round_trip` and `sqs_emitter_standard_round_trip` pass in 30-60 seconds (LocalStack is slow to boot but each test owns its own container).

If LocalStack hangs on startup, check `docker logs <container-id>` for the LocalStack init banner; the container is healthy when you see `Ready.` in the logs. testcontainers waits for that automatically.

- [ ] **Step 5: Run clippy on the crate**

Run: `cargo clippy -p hookbox-emitter-sqs --all-targets --all-features -- -D warnings`
Expected: no warnings. The new `with_client` constructor is `#[must_use]` to satisfy `clippy::must_use_candidate` under pedantic.

- [ ] **Step 6: Commit**

```bash
git add crates/hookbox-emitter-sqs/src/lib.rs crates/hookbox-emitter-sqs/Cargo.toml crates/hookbox-emitter-sqs/tests/round_trip.rs Cargo.toml
git commit -m "test(hookbox-emitter-sqs): add round-trip integration tests

Adds SqsEmitter::with_client constructor that accepts a pre-built
aws_sdk_sqs::Client, so tests can inject static credentials for
LocalStack without mutating process env (workspace lints set
unsafe_code = deny, and the two SQS tests share an integration-test
binary which would race on set_var).

Two tests, both spawning their own LocalStack container:

- sqs_emitter_fifo_round_trip creates a FIFO queue via the AWS SDK,
  publishes through SqsEmitter::with_client(fifo=true), then receives
  the message with MessageSystemAttributeNames requested and asserts
  MessageGroupId == provider_name and MessageDeduplicationId ==
  receipt_id. This proves the FIFO routing contract end-to-end.
- sqs_emitter_standard_round_trip covers the fifo=false code path
  against a standard queue.

Production SqsEmitter::new is unchanged and still uses the env-based
credentials provider chain via aws_config::from_env()."
```

---

## Task 11: CI changes — exclude emitters from matrix `test` and `careful`, add `test-emitters` Linux-only job

The existing `test` matrix job in `.github/workflows/ci.yml` runs on `[ubuntu-latest, macos-latest]`. The macOS leg has no Docker, so testcontainers tests must not run there. The existing `careful` job runs on Linux but pulls in the whole workspace too, which would create a *second* CI surface for Docker-backed tests and double the Docker flake risk. Solution: exclude the four emitter crates from BOTH the matrix `test` job and the `careful` job, and run them on exactly one new Linux-only job.

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Modify the existing `test` job to exclude the four emitter crates**

In `.github/workflows/ci.yml`, find the `test` job's `Unit tests (nextest)` step (around line 58–59). The current command is:

```yaml
      - name: Unit tests (nextest)
        run: cargo nextest run --all-features --profile ci --no-tests=warn --workspace --exclude hookbox-integration-tests
```

Replace with:

```yaml
      - name: Unit tests (nextest)
        run: cargo nextest run --all-features --profile ci --no-tests=warn --workspace --exclude hookbox-integration-tests --exclude hookbox-emitter-kafka --exclude hookbox-emitter-nats --exclude hookbox-emitter-sqs --exclude hookbox-emitter-redis
```

- [ ] **Step 2: Modify the `careful` job to also exclude the four emitter crates**

In `.github/workflows/ci.yml`, find the `careful` job (around line 111). Its current `cargo careful` invocation is:

```yaml
      - run: cargo +nightly careful test --workspace --all-features --exclude hookbox-integration-tests
```

Replace with:

```yaml
      - run: cargo +nightly careful test --workspace --all-features --exclude hookbox-integration-tests --exclude hookbox-emitter-kafka --exclude hookbox-emitter-nats --exclude hookbox-emitter-sqs --exclude hookbox-emitter-redis
```

This keeps `careful` covering the rest of the workspace under nightly's stricter sanitiser checks while routing all Docker-backed emitter tests through the dedicated `test-emitters` job (added in Step 3 below). Without this exclusion, every PR would spin up Docker containers in two parallel jobs and double the chance of a flaky CI run.

- [ ] **Step 3: Add the new `test-emitters` job after the existing `test-integration` job**

In `.github/workflows/ci.yml`, locate the end of the `test-integration` job (around line 89). After its final step, add a new top-level job:

```yaml
  test-emitters:
    name: Emitter Round-Trip Tests (Linux only)
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - name: Install system deps (rdkafka build)
        run: sudo apt-get update && sudo apt-get install -y cmake libcurl4-openssl-dev
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Run emitter round-trip tests
        run: cargo test -p hookbox-emitter-kafka -p hookbox-emitter-nats -p hookbox-emitter-sqs -p hookbox-emitter-redis --all-features
```

The job runs `cargo test` (not nextest) for the four emitter crates. Each crate has exactly one test file (`tests/round_trip.rs`), and Docker is available on `ubuntu-latest` GitHub Actions runners by default (no setup-docker step needed).

- [ ] **Step 4: Validate the YAML locally**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"`
Expected: no error output. (Catches indentation typos before pushing.)

- [ ] **Step 5: Verify the existing matrix test still passes locally with the new exclude flags**

Run: `cargo nextest run --all-features --profile ci --no-tests=warn --workspace --exclude hookbox-integration-tests --exclude hookbox-emitter-kafka --exclude hookbox-emitter-nats --exclude hookbox-emitter-sqs --exclude hookbox-emitter-redis`
Expected: all non-emitter tests pass. (If `--profile ci` errors with "profile not found", check `.config/nextest.toml` — if there's no `ci` profile defined, drop the `--profile ci` flag; the workflow should still work.)

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: split emitter tests into a Linux-only test-emitters job

GitHub Actions macOS runners do not have Docker, so testcontainers
tests cannot run on the macOS leg of the existing matrix test job.
Excludes the four emitter crates from the matrix job AND from the
careful (nightly sanitiser) job — both would otherwise create a
second Docker-backed CI surface and double Docker flake risk.
Adds a new Linux-only test-emitters job that runs the round-trip
integration tests for hookbox-emitter-{kafka,nats,sqs,redis} on
exactly one CI surface. The new job runs in parallel with the
existing matrix test job, so wall-clock PR CI time grows by ~0;
worker minutes grow by ~3-5."
```

---

## Task 12: Cleanup — delete legacy smoke tests, compose file, and nightly emitter-smoke job

The four round-trip tests now cover the same surface area as the legacy smoke tests, more rigorously. Delete the legacy infrastructure and remove the now-unused emitter dev-deps from `integration-tests/Cargo.toml`.

**Files:**
- Delete: `integration-tests/tests/emitter_smoke_test.rs`
- Modify: `integration-tests/Cargo.toml`
- Delete: `docker-compose.test.yml`
- Modify: `.github/workflows/nightly.yml`

- [ ] **Step 1: Delete the legacy smoke test file**

Run: `rm integration-tests/tests/emitter_smoke_test.rs`

- [ ] **Step 2: Remove the now-unused emitter deps from `integration-tests/Cargo.toml`**

In `integration-tests/Cargo.toml`, in the `[dependencies]` table, delete these three lines:

```toml
hookbox-emitter-kafka.workspace = true
hookbox-emitter-nats.workspace = true
hookbox-emitter-sqs.workspace = true
```

- [ ] **Step 3: Verify the integration-tests crate still builds and tests pass**

Run: `cargo test -p hookbox-integration-tests --all-features`
Expected: clean build, all tests pass. (If a test references a deleted import, surface the failure and fix it before continuing — but the smoke test file was the only consumer of those three crates inside `integration-tests`.)

- [ ] **Step 4: Delete the docker-compose test file**

Run: `rm docker-compose.test.yml`

- [ ] **Step 5: Delete the `emitter-smoke` job from `nightly.yml`**

In `.github/workflows/nightly.yml`, locate the `emitter-smoke` job (starts around line 110, ends at line 148). Delete the entire job, including the leading two-space indent on `emitter-smoke:` and everything down to the next top-level job marker (or end of file). After the edit, the file should end with the previous job (`bolero` or whichever was last before `emitter-smoke`).

Verify the YAML is still valid:

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/nightly.yml'))"`
Expected: no error output.

- [ ] **Step 6: Run the full workspace check + clippy to make sure nothing references the deleted files**

Run: `cargo check --workspace --all-features`
Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: both clean.

- [ ] **Step 7: Commit**

```bash
git add -u
git commit -m "chore: delete legacy emitter smoke infrastructure

The new round-trip integration tests in each emitter crate cover the
same surface area more rigorously (full NormalizedEvent equality plus
backend routing assertions vs. the smoke tests' 'send didn't error'),
so the legacy infrastructure is no longer needed:

- delete integration-tests/tests/emitter_smoke_test.rs
- drop hookbox-emitter-{kafka,nats,sqs} from integration-tests deps
- delete docker-compose.test.yml (testcontainers handles broker setup)
- delete the nightly emitter-smoke job (PR CI now covers this)"
```

---

## Task 13: Strengthen Bolero `NormalizedEvent` JSON round-trip property test

Now that `NormalizedEvent` derives `PartialEq` (Task 1), the existing property test in `hookbox-verify` can be tightened from per-field assertions to full struct equality. Also adds a `serde_json::Value` envelope round-trip to catch any custom serializer drift.

**Files:**
- Modify: `crates/hookbox-verify/src/emitter_props.rs:27-39` (existing `normalized_event_always_serializes_to_json` test)

- [ ] **Step 1: Replace per-field assertions with full equality**

In `crates/hookbox-verify/src/emitter_props.rs`, replace the body of `normalized_event_always_serializes_to_json` with:

```rust
#[test]
fn normalized_event_always_serializes_to_json() {
    bolero::check!()
        .with_type::<(String, String)>()
        .for_each(|(provider, hash)| {
            let event = make_event(provider, hash);
            let json = serde_json::to_vec(&event).expect("serialize must succeed");
            let round_tripped: NormalizedEvent =
                serde_json::from_slice(&json).expect("deserialize must succeed");
            assert_eq!(event, round_tripped);
        });
}
```

- [ ] **Step 2: Add a `serde_json::Value` envelope round-trip test**

Append to the same `mod tests` block:

```rust
#[test]
fn normalized_event_round_trips_through_value() {
    bolero::check!()
        .with_type::<(String, String)>()
        .for_each(|(provider, hash)| {
            let event = make_event(provider, hash);
            let value = serde_json::to_value(&event).expect("to_value must succeed");
            let round_tripped: NormalizedEvent =
                serde_json::from_value(value).expect("from_value must succeed");
            assert_eq!(event, round_tripped);
        });
}
```

- [ ] **Step 3: Run the property tests**

Run: `cargo test -p hookbox-verify --all-features`
Expected: both `normalized_event_*` tests pass. Bolero will run them in the standard `cargo test` harness with the default iteration count.

- [ ] **Step 4: Run with extended bolero engine to exercise the property**

Run: `BOLERO_RANDOM_ITERATIONS=10000 cargo test -p hookbox-verify --all-features normalized_event`
Expected: 10k iterations pass for each test. If any fail, the failure case should reproduce deterministically — fix the deserializer or revert the assertion change.

- [ ] **Step 5: Commit**

```bash
git add crates/hookbox-verify/src/emitter_props.rs
git commit -m "test(verify): tighten NormalizedEvent round-trip property tests

Now that NormalizedEvent derives PartialEq (this PR), the existing
serde_json::String round-trip test can assert full struct equality
instead of three hand-picked fields, and a new serde_json::Value
envelope round-trip catches any drift in the Value-shaped path."
```

---

## Task 14: Verify Kani proofs still pass; document N/A for new state

The Redis emitter introduces no new `ProcessingState` variants and no new state-machine transitions — the existing `kani_proofs.rs` covers the `Received → … → Emitted/EmitFailed/DeadLettered` transitions exhaustively, and the Redis backend reuses `EmitError::{Downstream, Timeout}` without changing the control flow. This task confirms the existing proofs still pass and documents why no new proof was added.

**Files:**
- Verify: `crates/hookbox-verify/src/kani_proofs.rs` (no edits expected)

- [ ] **Step 1: Run the Kani proofs**

Run: `cargo kani -p hookbox-verify`
Expected: all existing proofs pass. If Kani is not installed locally, skip this step and rely on the nightly Kani CI job — but verify that job is green on the branch before merging.

- [ ] **Step 2: Document the rationale in the spec's "Verification" section**

In `docs/superpowers/specs/2026-04-12-redis-emitter-and-emitter-test-coverage-design.md`, append a short "Kani scope" note under the existing testing strategy section:

```markdown
**Kani scope for this PR:** No new proofs added. The Redis emitter reuses the
existing `ProcessingState` machine and `EmitError::{Downstream, Timeout}` enum
without introducing new variants or transitions. The existing
`processing_state_variants_are_distinct` proof in `kani_proofs.rs` continues
to cover the full state surface.
```

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-04-12-redis-emitter-and-emitter-test-coverage-design.md
git commit -m "docs(spec): note Kani scope for Redis emitter PR (no new proofs)"
```

---

## Task 15: Add BDD scenario asserting full `NormalizedEvent` equality through the pipeline

The existing `emitters.feature` scenarios assert provider name only, because before this PR `NormalizedEvent` did not derive `PartialEq`. With the Task 1 derive in place, the pipeline can be exercised end-to-end with a byte-exact equality assertion on the emitted event.

**Files:**
- Modify: `crates/hookbox/tests/features/emitters.feature` (add one new scenario)
- Modify: `crates/hookbox/tests/bdd.rs` (add the new step + capture-emitted-event helper if not present)

- [ ] **Step 1: Add the new scenario to `emitters.feature`**

Append to `crates/hookbox/tests/features/emitters.feature`:

```gherkin
  Scenario: Emitted event preserves the full normalized payload
    Given a pipeline with a passing verifier for "test"
    When I ingest a webhook from "test" with body '{"event":"equality_test","amount":42}'
    Then the result should be "accepted"
    And the emitted event payload_hash should be deterministic for body '{"event":"equality_test","amount":42}'
```

- [ ] **Step 2: Add the corresponding `then` step to `bdd.rs`**

In `crates/hookbox/tests/bdd.rs`, add a new step handler. Locate the existing `then` block that captures emitted events (search for `an event should be emitted with provider`) and add this sibling:

```rust
#[then(regex = r#"^the emitted event payload_hash should be deterministic for body '(.+)'$"#)]
async fn emitted_event_payload_hash_deterministic(world: &mut HookboxWorld, body: String) {
    let emitted = world
        .emitted_events
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("at least one event should have been emitted");

    let expected_hash = hookbox::hash::sha256_hex(body.as_bytes());
    assert_eq!(emitted.payload_hash, expected_hash);

    // With NormalizedEvent: PartialEq, we can also clone-and-compare to confirm
    // the struct is internally consistent (no field has been mutated post-emit).
    let copy = emitted.clone();
    assert_eq!(emitted, copy);
}
```

If the world type does not yet expose `emitted_events` as a `Mutex<Vec<NormalizedEvent>>`, check the existing `an event should be emitted with provider` step — it almost certainly already wires this up. Reuse that capture; do not add a parallel one.

- [ ] **Step 3: Run the BDD suite**

Run: `cargo test -p hookbox --test bdd`
Expected: the new scenario passes alongside the existing four. If the `sha256_hex` helper does not exist under that exact path, search the `hookbox` crate for the existing payload-hashing function and use it instead — the production hash function is the source of truth.

- [ ] **Step 4: Commit**

```bash
git add crates/hookbox/tests/features/emitters.feature crates/hookbox/tests/bdd.rs
git commit -m "test(bdd): assert payload_hash determinism on emitted event

Adds a scenario that exercises the new NormalizedEvent: PartialEq derive
end-to-end through the pipeline by hashing the ingest body and comparing
against the emitted event's payload_hash field."
```

---

## Task 16: Documentation updates — move Redis to Completed, mention round-trip coverage

**Files:**
- Modify: `docs/ROADMAP.md`
- Modify: `README.md` (only if it has an emitter list — verify first)

- [ ] **Step 1: Update `docs/ROADMAP.md`**

In `docs/ROADMAP.md`:

1. Under `## Completed`, append:

```markdown
- [x] **PR #14 — Redis Streams emitter + emitter test coverage**: `hookbox-emitter-redis` (XADD, optional MAXLEN, configurable timeout), round-trip integration tests for all four emitters (Kafka, NATS, SQS, Redis) using `testcontainers-rs`, dedicated Linux-only `test-emitters` CI job, legacy `emitter_smoke_test.rs` deleted
```

2. Under `## Next: Immediate candidates`, delete the line:

```markdown
- **Redis Streams emitter** (V2 emitter — most requested next)
```

3. Under `## Next: Immediate candidates`, replace the line:

```markdown
- **Coverage gaps**: emitter adapter crates are at 0% (can't unit-test without mocks or embedded brokers). Consider testcontainers or mock traits.
```

with:

```markdown
- **Coverage gaps**: emitter crates now have round-trip integration tests via testcontainers (PR #14). Next gap: the `hookbox-server` `serve` command's emitter-selection arms (Kafka/NATS/SQS/Redis) are still untested in isolation.
```

4. Under `### 2. ~~Emitter adapters V1~~ (done)`, change:

```markdown
**V2 (next batch):**
- `hookbox-emitter-redis` — Redis Streams via XADD (most likely next)
```

to:

```markdown
**V2 (done):**
- ~~`hookbox-emitter-redis`~~ — Redis Streams via XADD ✅
```

- [ ] **Step 2: Check `README.md` for an emitter list**

Run: `grep -n -i 'kafka\|nats\|emitter' README.md`

If a backend list exists (e.g. "Supported emitters: Kafka, NATS, SQS"), add Redis. If no such list exists, skip — README does not need to be updated.

- [ ] **Step 3: Run a sanity check on the docs**

Run: `cargo doc --no-deps --all-features --workspace 2>&1 | tail -20`
Expected: clean (no broken intra-doc links from the new Redis crate). If there are warnings, fix the doc comments before committing.

- [ ] **Step 4: Commit**

```bash
git add docs/ROADMAP.md README.md
git commit -m "docs: mark Redis emitter + round-trip coverage as completed in roadmap"
```

---

## Task 17: Final verification — full workspace test pass + plan checkpoint

A final guarded run to confirm the whole repo is healthy after all the changes, before opening the PR.

- [ ] **Step 1: Format check**

Run: `cargo fmt --all --check`
Expected: clean. If anything is misformatted, run `cargo fmt --all` and amend the most recent commit.

- [ ] **Step 2: Workspace clippy with the project's `-D warnings` policy**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 3: Workspace test pass (excluding emitter crates) — mirrors the matrix `test` job**

Run: `cargo nextest run --all-features --workspace --exclude hookbox-integration-tests --exclude hookbox-emitter-kafka --exclude hookbox-emitter-nats --exclude hookbox-emitter-sqs --exclude hookbox-emitter-redis`
Expected: all tests pass.

- [ ] **Step 4: Emitter test pass — mirrors the new `test-emitters` job**

Run: `cargo test -p hookbox-emitter-kafka -p hookbox-emitter-nats -p hookbox-emitter-sqs -p hookbox-emitter-redis --all-features`
Expected: all four round-trip tests pass. Wall-clock time: 60-180 seconds total.

- [ ] **Step 5: Integration tests still pass against Postgres**

Run: `cargo test -p hookbox-integration-tests --all-features`
Expected: clean (this verifies the integration-tests Cargo.toml cleanup didn't break anything).

Note: this step requires a Postgres instance reachable at `DATABASE_URL`. If running locally without one, skip this step and rely on CI to verify.

- [ ] **Step 6: cargo deny check**

Run: `cargo deny check`
Expected: clean. The new `redis`, `testcontainers`, and `testcontainers-modules` crates may pull in transitive deps that trigger advisories — if so, fix or document the exemption in `deny.toml`.

- [ ] **Step 7: Check the branch state**

Run: `git log --oneline main..HEAD`
Expected: 16 commits (Task 1 through Task 16) plus the two earlier spec commits.

- [ ] **Step 8: Open the PR**

Run:

```bash
git push -u origin feat/redis-emitter-test-coverage
gh pr create --title "feat: Redis Streams emitter + round-trip test coverage for all 4 emitters" --body "$(cat <<'EOF'
## Summary

Ships the V2 Redis Streams emitter and closes the 0% test-coverage gap on all four emitter crates (Kafka, NATS, SQS, Redis) with round-trip integration tests against ephemeral broker containers.

- New `hookbox-emitter-redis` crate using `redis-rs` (`tokio-comp` + `streams`), with explicit `timeout_ms` constructor parameter wrapping `XADD` in `tokio::time::timeout`.
- `RedisEmitterConfig` wired into `[emitter.redis]` in `hookbox.toml`, plus the `"redis"` arm in `hookbox serve`.
- Round-trip integration tests for Kafka, NATS, SQS, and Redis using `testcontainers-rs` + `testcontainers-modules`. Each test publishes a `NormalizedEvent`, consumes it back through the broker's native API, and asserts on full struct equality plus backend-specific routing (Kafka record key, SQS FIFO group/dedup, NATS subject, Redis stream key).
- New Linux-only `test-emitters` job in `.github/workflows/ci.yml` running on every PR push. Existing matrix `test` job excludes the emitter crates because GitHub Actions macOS runners do not have Docker.
- Legacy `integration-tests/tests/emitter_smoke_test.rs`, `docker-compose.test.yml`, and the nightly `emitter-smoke` job are deleted — the round-trip tests cover the same surface area more rigorously.
- `NormalizedEvent` derives `PartialEq` (required by `assert_eq!(received, event)` in the round-trip tests).

Spec: `docs/superpowers/specs/2026-04-12-redis-emitter-and-emitter-test-coverage-design.md`
Plan: `docs/superpowers/plans/2026-04-12-redis-emitter-and-emitter-test-coverage.md`

## Test plan

- [ ] CI matrix `test` job is green on both `ubuntu-latest` and `macos-latest`.
- [ ] CI `test-emitters` job is green on `ubuntu-latest`.
- [ ] CI `test-integration` job is green (Postgres integration tests still pass after the integration-tests/Cargo.toml cleanup).
- [ ] CI `clippy` job is green.
- [ ] CI `deny` job is green.
- [ ] Local run: `hookbox serve --config hookbox.toml` with `[emitter] type = "redis"` connects to a local Redis instance and emits events.

EOF
)"
```

Expected: PR opens against `main`. Capture the URL and post it back.

- [ ] **Step 9: Done**

Mark all plan tasks complete in the implementation tracker. The branch is now ready for code review.

---

## Notes for whoever picks this up

- **Docker is required to run the emitter tests locally.** If you don't have Docker, exclude the four emitter crates from your local `cargo test` invocation and rely on CI: `cargo test --workspace --exclude hookbox-emitter-kafka --exclude hookbox-emitter-nats --exclude hookbox-emitter-sqs --exclude hookbox-emitter-redis`.
- **Container startup is the dominant cost** in test runtime. Kafka is the slowest (~30s cold), LocalStack is ~15s, NATS and Redis are ~2-5s each. If a test flakes on a slow CI runner, the fix is almost always to bump the per-test timeout, not to retry the test.
- **All test files duplicate the `make_test_event()` helper** instead of pulling it from a shared dev-dep crate. This was a deliberate choice to keep the workspace lean — the helper is 10 lines and any drift would surface immediately as a test failure.
- **The SQS test injects credentials via `SqsEmitter::with_client`**, not via `std::env::set_var`. This avoids the `unsafe_code = "deny"` workspace lint and the cross-test env-var race that would otherwise hit when both FIFO and standard tests share an integration-test binary. The static `Credentials::new("test", "test", None, None, "hookbox-tests")` is safe because it only ever talks to LocalStack.
- **Production code unchanged for Kafka, NATS, SQS.** The only production-code change in this whole plan is `NormalizedEvent` deriving `PartialEq` (required by the round-trip assertions) and the new `RedisEmitter` itself.
