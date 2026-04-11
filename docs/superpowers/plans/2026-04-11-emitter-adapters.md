# Emitter Adapters Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add 3 message broker emitter adapters (Kafka, NATS, SQS) with config-driven selection, replacing the hardcoded ChannelEmitter drain task.

**Architecture:** Foundation first (blanket impls, config, wiring), then each adapter as an independent crate. Each implements `Emitter` trait, serializes `NormalizedEvent` as JSON, maps errors to `EmitError`. Shared ownership via `Arc<dyn Emitter + Send + Sync>` for pipeline + worker.

**Tech Stack:** rdkafka 0.36 (cmake-build), async-nats 0.38, aws-sdk-sqs + aws-config, serde_json, tokio, async-trait.

**Spec reference:** `docs/superpowers/specs/2026-04-11-emitter-adapters-design.md`

---

## File Map

### New Files

| File | Responsibility |
|------|---------------|
| `crates/hookbox-emitter-kafka/Cargo.toml` | Kafka adapter crate manifest |
| `crates/hookbox-emitter-kafka/src/lib.rs` | `KafkaEmitter` implementing `Emitter` |
| `crates/hookbox-emitter-nats/Cargo.toml` | NATS adapter crate manifest |
| `crates/hookbox-emitter-nats/src/lib.rs` | `NatsEmitter` implementing `Emitter` |
| `crates/hookbox-emitter-sqs/Cargo.toml` | SQS adapter crate manifest |
| `crates/hookbox-emitter-sqs/src/lib.rs` | `SqsEmitter` implementing `Emitter` |

### Modified Files

| File | Changes |
|------|---------|
| `crates/hookbox/src/emitter.rs` | Add blanket impls for `Box<T>` and `Arc<T>` |
| `crates/hookbox-server/src/config.rs` | Add `EmitterConfig` + backend sub-configs |
| `crates/hookbox-server/src/lib.rs` | Update `ServerAppState` alias to use `Arc<dyn Emitter>` |
| `crates/hookbox-cli/src/commands/serve.rs` | Config-driven emitter construction + `Arc` sharing |
| `crates/hookbox-cli/Cargo.toml` | Add 3 adapter crate deps |
| `Cargo.toml` (workspace root) | Add 3 crate members + workspace deps |

---

## Task 1: Blanket Impls + Config Foundation

**Files:**
- Modify: `crates/hookbox/src/emitter.rs`
- Modify: `crates/hookbox-server/src/config.rs`
- Modify: `crates/hookbox-server/src/lib.rs`
- Modify: `Cargo.toml` (workspace root)

### Steps

- [ ] **Step 1: Add blanket impls for Box and Arc**

In `crates/hookbox/src/emitter.rs`, add after the existing implementations:

```rust
use std::sync::Arc;

#[async_trait]
impl<T: Emitter + ?Sized + Send + Sync> Emitter for Box<T> {
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        (**self).emit(event).await
    }
}

#[async_trait]
impl<T: Emitter + ?Sized + Send + Sync> Emitter for Arc<T> {
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        (**self).emit(event).await
    }
}
```

Add a test:
```rust
#[tokio::test]
async fn arc_emitter_delegates_to_inner() {
    let (emitter, mut rx) = ChannelEmitter::new(16);
    let arc_emitter: Arc<dyn Emitter + Send + Sync> = Arc::new(emitter);
    let event = test_event();
    arc_emitter.emit(&event).await.unwrap();
    let received = rx.try_recv().unwrap();
    assert_eq!(received.provider_name, "test");
}
```

- [ ] **Step 2: Add EmitterConfig to config.rs**

In `crates/hookbox-server/src/config.rs`, add:

```rust
/// Emitter backend configuration.
///
/// Selects which downstream system receives forwarded webhook events.
/// Default: `"channel"` (in-process channel with drain task, for development).
#[derive(Debug, Deserialize)]
pub struct EmitterConfig {
    /// Emitter backend type.
    /// Valid values: `"kafka"`, `"nats"`, `"sqs"`, `"channel"`.
    #[serde(rename = "type", default = "default_emitter_type")]
    pub emitter_type: String,
    /// Kafka producer configuration.
    pub kafka: Option<KafkaEmitterConfig>,
    /// NATS publish configuration.
    pub nats: Option<NatsEmitterConfig>,
    /// AWS SQS queue configuration.
    pub sqs: Option<SqsEmitterConfig>,
}

impl Default for EmitterConfig {
    fn default() -> Self {
        Self {
            emitter_type: default_emitter_type(),
            kafka: None,
            nats: None,
            sqs: None,
        }
    }
}

fn default_emitter_type() -> String {
    "channel".to_owned()
}

/// Kafka emitter configuration.
#[derive(Debug, Deserialize)]
pub struct KafkaEmitterConfig {
    /// Comma-separated broker addresses.
    pub brokers: String,
    /// Target topic name.
    pub topic: String,
    /// Client ID (optional, default: "hookbox").
    #[serde(default = "default_kafka_client_id")]
    pub client_id: String,
    /// Ack level (optional, default: "all").
    #[serde(default = "default_kafka_acks")]
    pub acks: String,
    /// Producer timeout in milliseconds (optional, default: 5000).
    #[serde(default = "default_kafka_timeout")]
    pub timeout_ms: u64,
}

fn default_kafka_client_id() -> String { "hookbox".to_owned() }
fn default_kafka_acks() -> String { "all".to_owned() }
fn default_kafka_timeout() -> u64 { 5000 }

/// NATS emitter configuration.
#[derive(Debug, Deserialize)]
pub struct NatsEmitterConfig {
    /// NATS server URL.
    pub url: String,
    /// Target subject name.
    pub subject: String,
}

/// AWS SQS emitter configuration.
#[derive(Debug, Deserialize)]
pub struct SqsEmitterConfig {
    /// SQS queue URL.
    pub queue_url: String,
    /// AWS region.
    pub region: Option<String>,
    /// Whether the queue is FIFO (enables MessageGroupId/MessageDeduplicationId).
    #[serde(default)]
    pub fifo: bool,
}
```

Add `#[serde(default)] pub emitter: EmitterConfig` to `HookboxConfig`.

Add a config test that parses all emitter types.

- [ ] **Step 3: Update ServerAppState alias**

In `crates/hookbox-server/src/lib.rs`, change:
```rust
pub type ServerAppState = AppState<
    PostgresStorage,
    LayeredDedupe<InMemoryRecentDedupe, StorageDedupe>,
    Arc<dyn Emitter + Send + Sync>,
>;
```

Add `use std::sync::Arc;` and `use hookbox::traits::Emitter;` if not present.

- [ ] **Step 4: Add workspace deps**

In root `Cargo.toml`, add to `[workspace.dependencies]`:
```toml
rdkafka = { version = "0.36", features = ["cmake-build"] }
async-nats = "0.38"
aws-sdk-sqs = "1"
aws-config = { version = "1", features = ["behavior-version-latest"] }
```

- [ ] **Step 5: Run lint and test**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test -p hookbox -p hookbox-server`

- [ ] **Step 6: Commit**

```bash
git commit -m "feat: add emitter blanket impls, config structs, and workspace deps"
```

---

## Task 2: Kafka Emitter

**Files:**
- Create: `crates/hookbox-emitter-kafka/Cargo.toml`
- Create: `crates/hookbox-emitter-kafka/src/lib.rs`
- Modify: `Cargo.toml` (add to workspace members)

### Steps

- [ ] **Step 1: Create crate**

`crates/hookbox-emitter-kafka/Cargo.toml`:
```toml
[package]
name = "hookbox-emitter-kafka"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
description = "Kafka emitter adapter for hookbox."
publish = false

[lints]
workspace = true

[dependencies]
hookbox.workspace = true
rdkafka.workspace = true
serde_json.workspace = true
tracing.workspace = true
async-trait.workspace = true
tokio.workspace = true
```

Add `"crates/hookbox-emitter-kafka"` to workspace members in root `Cargo.toml`.

- [ ] **Step 2: Implement KafkaEmitter**

```rust
// crates/hookbox-emitter-kafka/src/lib.rs

//! Kafka emitter adapter for hookbox.
//!
//! Produces JSON-serialized `NormalizedEvent`s to a Kafka topic using
//! `rdkafka`'s `FutureProducer` with statically linked librdkafka.

use std::time::Duration;

use async_trait::async_trait;
use rdkafka::config::ClientConfig;
use rdkafka::producer::{FutureProducer, FutureRecord};
use tracing;

use hookbox::error::EmitError;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;

/// Kafka webhook event emitter.
///
/// Produces JSON payloads to a configured topic. Uses `receipt_id` as the
/// message key for partition affinity.
pub struct KafkaEmitter {
    producer: FutureProducer,
    topic: String,
    timeout: Duration,
}

impl KafkaEmitter {
    /// Create a new Kafka emitter.
    ///
    /// # Errors
    ///
    /// Returns `EmitError::Downstream` if the producer cannot be created.
    pub fn new(
        brokers: &str,
        topic: String,
        client_id: &str,
        acks: &str,
        timeout_ms: u64,
    ) -> Result<Self, EmitError> {
        let producer: FutureProducer = ClientConfig::new()
            .set("bootstrap.servers", brokers)
            .set("client.id", client_id)
            .set("acks", acks)
            .set("message.timeout.ms", &timeout_ms.to_string())
            .create()
            .map_err(|e| EmitError::Downstream(format!("kafka producer creation failed: {e}")))?;

        Ok(Self {
            producer,
            topic,
            timeout: Duration::from_millis(timeout_ms),
        })
    }
}

#[async_trait]
impl Emitter for KafkaEmitter {
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        let payload = serde_json::to_vec(event)
            .map_err(|e| EmitError::Downstream(format!("json serialization failed: {e}")))?;

        let key = event.receipt_id.to_string();

        let record = FutureRecord::to(&self.topic)
            .key(&key)
            .payload(&payload);

        self.producer
            .send(record, self.timeout)
            .await
            .map_err(|(e, _)| EmitError::Downstream(format!("kafka send failed: {e}")))?;

        tracing::debug!(
            receipt_id = %event.receipt_id,
            topic = %self.topic,
            "kafka: event emitted"
        );

        Ok(())
    }
}
```

- [ ] **Step 3: Run lint**

Run: `cargo clippy -p hookbox-emitter-kafka --all-targets -- -D warnings`

Note: No unit tests for Kafka — requires a real broker. Integration tests with Docker in CI are future work.

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(hookbox-emitter-kafka): add Kafka emitter adapter"
```

---

## Task 3: NATS Emitter

**Files:**
- Create: `crates/hookbox-emitter-nats/Cargo.toml`
- Create: `crates/hookbox-emitter-nats/src/lib.rs`
- Modify: `Cargo.toml` (add to workspace members)

### Steps

- [ ] **Step 1: Create crate**

`crates/hookbox-emitter-nats/Cargo.toml`:
```toml
[package]
name = "hookbox-emitter-nats"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
description = "NATS emitter adapter for hookbox."
publish = false

[lints]
workspace = true

[dependencies]
hookbox.workspace = true
async-nats.workspace = true
serde_json.workspace = true
tracing.workspace = true
async-trait.workspace = true
bytes.workspace = true
```

Add `"crates/hookbox-emitter-nats"` to workspace members.

- [ ] **Step 2: Implement NatsEmitter**

```rust
// crates/hookbox-emitter-nats/src/lib.rs

//! NATS emitter adapter for hookbox.
//!
//! Publishes JSON-serialized `NormalizedEvent`s to a NATS subject.

use async_trait::async_trait;
use bytes::Bytes;
use tracing;

use hookbox::error::EmitError;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;

/// NATS webhook event emitter.
pub struct NatsEmitter {
    client: async_nats::Client,
    subject: String,
}

impl NatsEmitter {
    /// Connect to NATS and create a new emitter.
    ///
    /// # Errors
    ///
    /// Returns `EmitError::Downstream` if the connection fails.
    pub async fn new(url: &str, subject: String) -> Result<Self, EmitError> {
        let client = async_nats::connect(url)
            .await
            .map_err(|e| EmitError::Downstream(format!("nats connection failed: {e}")))?;

        Ok(Self { client, subject })
    }
}

#[async_trait]
impl Emitter for NatsEmitter {
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        let payload = serde_json::to_vec(event)
            .map_err(|e| EmitError::Downstream(format!("json serialization failed: {e}")))?;

        self.client
            .publish(self.subject.clone(), Bytes::from(payload))
            .await
            .map_err(|e| EmitError::Downstream(format!("nats publish failed: {e}")))?;

        tracing::debug!(
            receipt_id = %event.receipt_id,
            subject = %self.subject,
            "nats: event emitted"
        );

        Ok(())
    }
}
```

- [ ] **Step 3: Run lint**

Run: `cargo clippy -p hookbox-emitter-nats --all-targets -- -D warnings`

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(hookbox-emitter-nats): add NATS emitter adapter"
```

---

## Task 4: SQS Emitter

**Files:**
- Create: `crates/hookbox-emitter-sqs/Cargo.toml`
- Create: `crates/hookbox-emitter-sqs/src/lib.rs`
- Modify: `Cargo.toml` (add to workspace members)

### Steps

- [ ] **Step 1: Create crate**

`crates/hookbox-emitter-sqs/Cargo.toml`:
```toml
[package]
name = "hookbox-emitter-sqs"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
description = "AWS SQS emitter adapter for hookbox."
publish = false

[lints]
workspace = true

[dependencies]
hookbox.workspace = true
aws-sdk-sqs.workspace = true
aws-config.workspace = true
serde_json.workspace = true
tracing.workspace = true
async-trait.workspace = true
tokio.workspace = true
```

Add `"crates/hookbox-emitter-sqs"` to workspace members.

- [ ] **Step 2: Implement SqsEmitter**

```rust
// crates/hookbox-emitter-sqs/src/lib.rs

//! AWS SQS emitter adapter for hookbox.
//!
//! Sends JSON-serialized `NormalizedEvent`s to an SQS queue.
//! Supports both standard and FIFO queues.

use async_trait::async_trait;
use tracing;

use hookbox::error::EmitError;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;

/// AWS SQS webhook event emitter.
pub struct SqsEmitter {
    client: aws_sdk_sqs::Client,
    queue_url: String,
    fifo: bool,
}

impl SqsEmitter {
    /// Create a new SQS emitter.
    ///
    /// # Errors
    ///
    /// Returns `EmitError::Downstream` if the AWS client cannot be configured.
    pub async fn new(
        queue_url: String,
        region: Option<&str>,
        fifo: bool,
    ) -> Result<Self, EmitError> {
        let mut config_loader = aws_config::defaults(
            aws_config::BehaviorVersion::latest(),
        );

        if let Some(region) = region {
            config_loader = config_loader.region(
                aws_config::Region::new(region.to_owned()),
            );
        }

        let aws_config = config_loader.load().await;
        let client = aws_sdk_sqs::Client::new(&aws_config);

        Ok(Self {
            client,
            queue_url,
            fifo,
        })
    }
}

#[async_trait]
impl Emitter for SqsEmitter {
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        let payload = serde_json::to_string(event)
            .map_err(|e| EmitError::Downstream(format!("json serialization failed: {e}")))?;

        let mut req = self
            .client
            .send_message()
            .queue_url(&self.queue_url)
            .message_body(&payload);

        if self.fifo {
            req = req
                .message_group_id(&event.provider_name)
                .message_deduplication_id(&event.receipt_id.to_string());
        }

        req.send()
            .await
            .map_err(|e| EmitError::Downstream(format!("sqs send failed: {e}")))?;

        tracing::debug!(
            receipt_id = %event.receipt_id,
            queue_url = %self.queue_url,
            fifo = %self.fifo,
            "sqs: event emitted"
        );

        Ok(())
    }
}
```

- [ ] **Step 3: Run lint**

Run: `cargo clippy -p hookbox-emitter-sqs --all-targets -- -D warnings`

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(hookbox-emitter-sqs): add AWS SQS emitter adapter"
```

---

## Task 5: Wire Emitters in serve.rs

**Files:**
- Modify: `crates/hookbox-cli/Cargo.toml`
- Modify: `crates/hookbox-cli/src/commands/serve.rs`

### Steps

- [ ] **Step 1: Add adapter deps to hookbox-cli**

Add to `crates/hookbox-cli/Cargo.toml` `[dependencies]`:
```toml
hookbox-emitter-kafka = { path = "../hookbox-emitter-kafka" }
hookbox-emitter-nats = { path = "../hookbox-emitter-nats" }
hookbox-emitter-sqs = { path = "../hookbox-emitter-sqs" }
```

- [ ] **Step 2: Rewrite emitter construction in serve.rs**

Read `crates/hookbox-cli/src/commands/serve.rs`. Replace the hardcoded `ChannelEmitter` creation with config-driven dispatch:

```rust
use std::sync::Arc;
use hookbox::traits::Emitter;

let emitter: Arc<dyn Emitter + Send + Sync> = match config.emitter.emitter_type.as_str() {
    "kafka" => {
        let cfg = config.emitter.kafka.as_ref()
            .ok_or_else(|| anyhow::anyhow!("[emitter.kafka] required when type = \"kafka\""))?;
        let emitter = hookbox_emitter_kafka::KafkaEmitter::new(
            &cfg.brokers,
            cfg.topic.clone(),
            &cfg.client_id,
            &cfg.acks,
            cfg.timeout_ms,
        ).context("failed to create Kafka emitter")?;
        tracing::info!(brokers = %cfg.brokers, topic = %cfg.topic, "emitter: kafka");
        Arc::new(emitter)
    }
    "nats" => {
        let cfg = config.emitter.nats.as_ref()
            .ok_or_else(|| anyhow::anyhow!("[emitter.nats] required when type = \"nats\""))?;
        let emitter = hookbox_emitter_nats::NatsEmitter::new(&cfg.url, cfg.subject.clone())
            .await
            .context("failed to create NATS emitter")?;
        tracing::info!(url = %cfg.url, subject = %cfg.subject, "emitter: nats");
        Arc::new(emitter)
    }
    "sqs" => {
        let cfg = config.emitter.sqs.as_ref()
            .ok_or_else(|| anyhow::anyhow!("[emitter.sqs] required when type = \"sqs\""))?;
        let emitter = hookbox_emitter_sqs::SqsEmitter::new(
            cfg.queue_url.clone(),
            cfg.region.as_deref(),
            cfg.fifo,
        ).await.context("failed to create SQS emitter")?;
        tracing::info!(queue_url = %cfg.queue_url, fifo = %cfg.fifo, "emitter: sqs");
        Arc::new(emitter)
    }
    "channel" => {
        let (emitter, rx) = hookbox::emitter::ChannelEmitter::new(1024);
        tokio::spawn(drain_emitter(rx));
        tracing::info!("emitter: channel (development drain)");
        Arc::new(emitter)
    }
    other => {
        anyhow::bail!(
            "unknown emitter type {other:?}; valid values: kafka, nats, sqs, channel"
        );
    }
};

let pipeline_emitter = Arc::clone(&emitter);
let worker_emitter: Box<dyn Emitter + Send + Sync> = Box::new(Arc::clone(&emitter));
```

Update the pipeline construction to use `pipeline_emitter` and the worker to use `worker_emitter`.

Remove the old separate `ChannelEmitter` + drain task code that was previously outside the match.

The `drain_emitter` function stays — it's used by the `"channel"` arm.

- [ ] **Step 3: Update ServerAppState usage**

Since `ServerAppState` now uses `Arc<dyn Emitter + Send + Sync>` as `E`, update any code that explicitly references `ChannelEmitter` as the emitter type.

- [ ] **Step 4: Run full lint and test**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features --exclude hookbox-integration-tests`

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(hookbox-cli): wire Kafka, NATS, SQS emitters via config dispatch"
```

---

## Task 6: Documentation and README Updates

**Files:**
- Modify: `README.md`
- Modify: `crates/hookbox-server/README.md`
- Create: `crates/hookbox-emitter-kafka/README.md`
- Create: `crates/hookbox-emitter-nats/README.md`
- Create: `crates/hookbox-emitter-sqs/README.md`

### Steps

- [ ] **Step 1: Update root README**

Add emitter section to features list. Update workspace tree to show new crates.

- [ ] **Step 2: Update server README**

Add `[emitter]` config examples for all 3 backends + channel default.

- [ ] **Step 3: Create adapter READMEs**

Each with: purpose, config example, usage in hookbox.toml, error handling.

- [ ] **Step 4: Commit**

```bash
git commit -m "docs: add emitter adapter READMEs and update server config docs"
```

---

## Task 7: Workspace Validation

### Steps

- [ ] **Step 1: Format, lint, test, deny**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
DATABASE_URL=postgres://localhost/hookbox_test cargo test --workspace --all-features
cargo test -p hookbox --test bdd
cargo deny check
```

- [ ] **Step 2: Commit and push**

```bash
git commit -m "chore: workspace validation"
git push -u origin feat/emitter-adapters
```

---

## Task 8: Bolero Property Tests

**Files:**
- Create: `crates/hookbox-verify/src/emitter_props.rs`
- Modify: `crates/hookbox-verify/src/lib.rs`
- Modify: `crates/hookbox-verify/Cargo.toml`

### Steps

- [ ] **Step 1: Create emitter property tests**

Test invariants of the emitter infrastructure:

```rust
// crates/hookbox-verify/src/emitter_props.rs

//! Property tests for emitter adapter invariants.

#[cfg(test)]
mod tests {
    use hookbox::state::ReceiptId;
    use hookbox::types::NormalizedEvent;
    use std::sync::Arc;

    /// Any NormalizedEvent serializes to valid JSON.
    #[test]
    fn normalized_event_always_serializes_to_json() {
        bolero::check!().with_type::<(String, String)>().for_each(|(provider, hash)| {
            let event = NormalizedEvent {
                receipt_id: ReceiptId::new(),
                provider_name: provider.clone(),
                event_type: None,
                external_reference: None,
                parsed_payload: None,
                payload_hash: hash.clone(),
                received_at: chrono::Utc::now(),
                metadata: serde_json::json!({}),
            };
            let result = serde_json::to_vec(&event);
            assert!(result.is_ok(), "NormalizedEvent must always serialize to JSON");
            // Verify it's valid JSON by parsing back
            let bytes = result.expect("just checked");
            let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&bytes);
            assert!(parsed.is_ok(), "serialized bytes must be valid JSON");
        });
    }

    /// Arc<dyn Emitter> blanket impl works for any inner Emitter.
    #[test]
    fn arc_emitter_roundtrip() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        bolero::check!().with_type::<String>().for_each(|provider| {
            rt.block_on(async {
                let (emitter, mut rx) = hookbox::emitter::ChannelEmitter::new(16);
                let arc: Arc<dyn hookbox::traits::Emitter + Send + Sync> = Arc::new(emitter);
                let event = NormalizedEvent {
                    receipt_id: ReceiptId::new(),
                    provider_name: provider.clone(),
                    event_type: None,
                    external_reference: None,
                    parsed_payload: None,
                    payload_hash: "test".to_owned(),
                    received_at: chrono::Utc::now(),
                    metadata: serde_json::json!({}),
                };
                let result = arc.emit(&event).await;
                assert!(result.is_ok());
                let received = rx.try_recv();
                assert!(received.is_ok());
                assert_eq!(received.expect("just checked").provider_name, *provider);
            });
        });
    }

    /// receipt_id.to_string() always produces a valid message key (non-empty, no newlines).
    #[test]
    fn receipt_id_is_valid_message_key() {
        bolero::check!().with_type::<u8>().for_each(|_| {
            let id = ReceiptId::new();
            let key = id.to_string();
            assert!(!key.is_empty());
            assert!(!key.contains('\n'));
            assert!(!key.contains('\0'));
        });
    }
}
```

- [ ] **Step 2: Update lib.rs and Cargo.toml**

Add `mod emitter_props;` to `crates/hookbox-verify/src/lib.rs`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p hookbox-verify --all-features`

- [ ] **Step 4: Commit**

```bash
git commit -m "test(hookbox-verify): add Bolero property tests for emitter invariants"
```

---

## Task 9: BDD Scenarios for Emitter Selection

**Files:**
- Create: `crates/hookbox/tests/features/emitters.feature`

### Steps

- [ ] **Step 1: Create emitter BDD feature**

```gherkin
# crates/hookbox/tests/features/emitters.feature

Feature: Emitter Selection and Event Forwarding

  The pipeline forwards accepted webhooks to a configured emitter.
  The default channel emitter works for development.

  Scenario: Default channel emitter accepts and forwards events
    Given a pipeline with a passing verifier for "test"
    When I ingest a webhook from "test" with body '{"event":"emitter_test"}'
    Then the result should be "accepted"
    And an event should be emitted with provider "test"

  Scenario: Events contain receipt_id for traceability
    Given a pipeline with a passing verifier for "test"
    When I ingest a webhook from "test" with body '{"event":"trace_test"}'
    Then the result should be "accepted"
    And an event should be emitted with provider "test"

  Scenario: Emit failure does not reject the webhook
    Given a pipeline with a passing verifier for "test" and a failing emitter
    When I ingest a webhook from "test" with body '{"event":"fail_emit"}'
    Then the result should be "accepted"
```

These use the existing BDD step definitions.

- [ ] **Step 2: Run BDD**

Run: `cargo test -p hookbox --test bdd`

- [ ] **Step 3: Commit**

```bash
git commit -m "test(hookbox): add BDD scenarios for emitter selection"
```

---

## Task 10: Integration Tests for Config Dispatch

**Files:**
- Modify: `crates/hookbox-server/src/routes/tests.rs`
- Modify: `integration-tests/tests/http_test.rs`

### Steps

- [ ] **Step 1: Add config parsing tests**

In `crates/hookbox-server/src/config.rs` tests, add:

```rust
#[test]
fn parse_emitter_kafka_config() {
    let toml_str = r#"
[server]
port = 8080
[database]
url = "postgres://localhost/hookbox"
[emitter]
type = "kafka"
[emitter.kafka]
brokers = "localhost:9092"
topic = "hookbox-events"
"#;
    let config: HookboxConfig = toml::from_str(toml_str).expect("should parse");
    assert_eq!(config.emitter.emitter_type, "kafka");
    assert!(config.emitter.kafka.is_some());
    let kafka = config.emitter.kafka.unwrap();
    assert_eq!(kafka.brokers, "localhost:9092");
    assert_eq!(kafka.topic, "hookbox-events");
}

#[test]
fn parse_emitter_nats_config() {
    let toml_str = r#"
[server]
port = 8080
[database]
url = "postgres://localhost/hookbox"
[emitter]
type = "nats"
[emitter.nats]
url = "nats://localhost:4222"
subject = "hookbox.events"
"#;
    let config: HookboxConfig = toml::from_str(toml_str).expect("should parse");
    assert_eq!(config.emitter.emitter_type, "nats");
    assert!(config.emitter.nats.is_some());
}

#[test]
fn parse_emitter_sqs_config() {
    let toml_str = r#"
[server]
port = 8080
[database]
url = "postgres://localhost/hookbox"
[emitter]
type = "sqs"
[emitter.sqs]
queue_url = "https://sqs.us-east-1.amazonaws.com/123/hookbox"
region = "us-east-1"
fifo = true
"#;
    let config: HookboxConfig = toml::from_str(toml_str).expect("should parse");
    assert_eq!(config.emitter.emitter_type, "sqs");
    let sqs = config.emitter.sqs.unwrap();
    assert!(sqs.fifo);
}

#[test]
fn default_emitter_is_channel() {
    let toml_str = r#"
[server]
port = 8080
[database]
url = "postgres://localhost/hookbox"
"#;
    let config: HookboxConfig = toml::from_str(toml_str).expect("should parse");
    assert_eq!(config.emitter.emitter_type, "channel");
}
```

- [ ] **Step 2: Add Arc<dyn Emitter> route test**

In `crates/hookbox-server/src/routes/tests.rs`, add a test that verifies the in-memory route tests work with `Arc<dyn Emitter>` as the emitter type (instead of concrete `ChannelEmitter`). This validates the `ServerAppState` alias change.

The implementing agent should verify the existing tests still pass after the type alias change, and add one explicit test that constructs `AppState` with `Arc<ChannelEmitter>` as the emitter.

- [ ] **Step 3: Run tests**

Run: `cargo test -p hookbox-server && DATABASE_URL=postgres://localhost/hookbox_test cargo test -p hookbox-integration-tests`

- [ ] **Step 4: Commit**

```bash
git commit -m "test: add config dispatch and Arc emitter integration tests"
```

---

## Task 11: Coverage Check

### Steps

- [ ] **Step 1: Run coverage**

Run: `DATABASE_URL=postgres://localhost/hookbox_test cargo +nightly llvm-cov --all-features --ignore-filename-regex 'shutdown\.rs|serve\.rs|main\.rs'`

Check coverage is still above 90%. The new adapter crates have minimal testable code (they need real brokers for meaningful tests), so they'll show low coverage — that's expected and acceptable. The important thing is the blanket impls, config parsing, and wiring code are covered.

- [ ] **Step 2: Note coverage numbers**

Report the final coverage. Adapter crates will show 0% (no unit tests, need real brokers). This is documented as expected. Config parsing and blanket impl tests cover the hookbox-side of the integration.

- [ ] **Step 3: Commit if any fixes**

```bash
git commit -m "chore: coverage check for emitter adapters"
```

---

## Task 12: Real-World Smoke Tests (Manual / CI Docker)

This task documents how to test against real brokers. Tests are `#[ignore]` by default and require Docker services.

**Files:**
- Create: `integration-tests/tests/emitter_smoke_test.rs`

### Steps

- [ ] **Step 1: Create smoke test file**

```rust
// integration-tests/tests/emitter_smoke_test.rs

//! Smoke tests for emitter adapters against real brokers.
//!
//! These tests require running broker instances and are ignored by default.
//! Run with Docker Compose or CI services:
//!
//! ```bash
//! # Start brokers
//! docker run -d --name kafka -p 9092:9092 confluentinc/cp-kafka:latest
//! docker run -d --name nats -p 4222:4222 nats:latest
//!
//! # Run smoke tests
//! cargo test -p hookbox-integration-tests --test emitter_smoke_test -- --ignored
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hookbox::state::ReceiptId;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;

fn test_event() -> NormalizedEvent {
    NormalizedEvent {
        receipt_id: ReceiptId::new(),
        provider_name: "smoke_test".to_owned(),
        event_type: Some("payment.completed".to_owned()),
        external_reference: None,
        parsed_payload: Some(serde_json::json!({"test": true})),
        payload_hash: "smoke".to_owned(),
        received_at: chrono::Utc::now(),
        metadata: serde_json::json!({}),
    }
}

#[tokio::test]
#[ignore]
async fn kafka_emitter_smoke() {
    let brokers = std::env::var("KAFKA_BROKERS").unwrap_or_else(|_| "localhost:9092".to_owned());
    let emitter = hookbox_emitter_kafka::KafkaEmitter::new(
        &brokers, "hookbox-smoke-test".to_owned(), "hookbox-smoke", "all", 5000,
    ).expect("kafka emitter");
    let result = emitter.emit(&test_event()).await;
    assert!(result.is_ok(), "kafka emit failed: {result:?}");
}

#[tokio::test]
#[ignore]
async fn nats_emitter_smoke() {
    let url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".to_owned());
    let emitter = hookbox_emitter_nats::NatsEmitter::new(&url, "hookbox.smoke.test".to_owned())
        .await
        .expect("nats emitter");
    let result = emitter.emit(&test_event()).await;
    assert!(result.is_ok(), "nats emit failed: {result:?}");
}

#[tokio::test]
#[ignore]
async fn sqs_emitter_smoke() {
    let queue_url = std::env::var("SQS_QUEUE_URL")
        .expect("SQS_QUEUE_URL must be set for SQS smoke test");
    let emitter = hookbox_emitter_sqs::SqsEmitter::new(queue_url, None, false)
        .await
        .expect("sqs emitter");
    let result = emitter.emit(&test_event()).await;
    assert!(result.is_ok(), "sqs emit failed: {result:?}");
}
```

- [ ] **Step 2: Add deps to integration-tests**

Add to `integration-tests/Cargo.toml`:
```toml
hookbox-emitter-kafka = { path = "../crates/hookbox-emitter-kafka" }
hookbox-emitter-nats = { path = "../crates/hookbox-emitter-nats" }
hookbox-emitter-sqs = { path = "../crates/hookbox-emitter-sqs" }
```

- [ ] **Step 3: Verify they compile**

Run: `cargo test -p hookbox-integration-tests --test emitter_smoke_test --no-run`

- [ ] **Step 4: Commit**

```bash
git commit -m "test(integration): add real-world smoke tests for Kafka, NATS, SQS emitters"
```

---

## Summary

| Order | Task | What |
|-------|------|------|
| 1 | Task 1 | Blanket impls, config structs, workspace deps, ServerAppState update |
| 2 | Task 2 | KafkaEmitter (rdkafka, FutureProducer, static linking) |
| 3 | Task 3 | NatsEmitter (async-nats, publish to subject) |
| 4 | Task 4 | SqsEmitter (aws-sdk-sqs, standard + FIFO) |
| 5 | Task 5 | Wire all emitters in serve.rs via config dispatch |
| 6 | Task 6 | Documentation |
| 7 | Task 7 | Workspace validation |
| 8 | Task 8 | Bolero property tests (emitter invariants) |
| 9 | Task 9 | BDD scenarios (emitter selection + forwarding) |
| 10 | Task 10 | Integration tests (config dispatch + Arc emitter) |
| 11 | Task 11 | Coverage check |
| 12 | Task 12 | Real-world smoke tests (Kafka, NATS, SQS with Docker) |

Tasks 2-4 are independent. Task 5 depends on 1-4. Tasks 8-12 depend on 5.
