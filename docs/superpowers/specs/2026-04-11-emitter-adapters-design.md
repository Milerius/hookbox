# Emitter Adapters — Design Specification

Add 7 message broker emitter adapters so hookbox can forward webhook events to real infrastructure instead of draining to a log.

## Scope

7 new crates, each implementing the `Emitter` trait:

1. **Kafka** — `rdkafka` with static linking
2. **NATS** — `async-nats`
3. **AWS SQS** — `aws-sdk-sqs`
4. **Redis Streams** — `redis` crate
5. **RabbitMQ** — `lapin`
6. **Apache Pulsar** — `pulsar` crate
7. **gRPC** — `tonic` + `prost` with hookbox-defined proto

---

## Architecture

### Single emitter per instance

One emitter is configured per hookbox server instance via `[emitter]` in `hookbox.toml`. The `type` field selects the backend. Default: `"channel"` (existing `ChannelEmitter` + drain task for development).

### Serialization

All emitters serialize `NormalizedEvent` as JSON via `serde_json::to_vec`. No alternative formats in this pass.

### Shared ownership via Arc<dyn Emitter>

Both `HookboxPipeline` and `RetryWorker` need to hold a reference to the same emitter. A single `Box<dyn Emitter>` cannot be shared between two owners. The solution is `Arc<dyn Emitter + Send + Sync>`.

Add blanket impls to `hookbox` core for both smart pointer types:

```rust
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

In serve.rs, construct the emitter once and distribute clones:

```rust
let emitter: Arc<dyn Emitter + Send + Sync> = Arc::new(/* constructed emitter */);
let pipeline_emitter = Arc::clone(&emitter);
let worker_emitter = Arc::clone(&emitter);
```

This allows `Arc<dyn Emitter + Send + Sync>` to satisfy `E: Emitter` for both the pipeline and the retry worker without moving or copying the underlying emitter.

### Message key

All emitters use `receipt_id` as the message key/ID for:
- Partition affinity (Kafka, Pulsar)
- Deduplication (SQS FIFO, when FIFO queue is configured)
- Traceability (all backends)

Note: Redis Streams ordering is controlled by the server-assigned entry ID (the `*` auto-ID in `XADD`), not by `receipt_id`. The `receipt_id` is stored as a field within the stream entry for traceability only.

---

## Crate Structure

```
crates/hookbox-emitter-kafka/      # rdkafka, produces to topic
crates/hookbox-emitter-nats/       # async-nats, publishes to subject
crates/hookbox-emitter-sqs/        # aws-sdk-sqs, sends to queue URL
crates/hookbox-emitter-redis/      # redis, XADD to stream
crates/hookbox-emitter-rabbitmq/   # lapin, publishes to exchange
crates/hookbox-emitter-pulsar/     # pulsar, produces to topic
crates/hookbox-emitter-grpc/       # tonic client, calls unary RPC
```

Each crate:
- Depends only on `hookbox` core (for trait, types, errors)
- Has its own config struct (deserialized from `[emitter.<backend>]` TOML)
- Implements `Emitter` trait via `#[async_trait]`
- Serializes `NormalizedEvent` as JSON
- Maps backend errors to `EmitError::Downstream` or `EmitError::Timeout`
- Has unit tests (with mocks or embedded backends where feasible)

---

## Config

### `hookbox.toml` structure

```toml
[emitter]
type = "kafka"  # kafka | nats | sqs | redis | rabbitmq | pulsar | grpc | channel

[emitter.kafka]
brokers = "localhost:9092"
topic = "hookbox-events"
client_id = "hookbox"          # optional
acks = "all"                   # optional: all | 1 | 0
compression = "lz4"            # optional: none | gzip | snappy | lz4 | zstd
timeout_ms = 5000              # optional

[emitter.nats]
url = "nats://localhost:4222"
subject = "hookbox.events"

[emitter.sqs]
queue_url = "https://sqs.us-east-1.amazonaws.com/123456789/hookbox-events"
region = "us-east-1"

[emitter.redis]
url = "redis://localhost:6379"
stream = "hookbox:events"
maxlen = 10000                 # optional, approximate MAXLEN for trimming

[emitter.rabbitmq]
url = "amqp://guest:guest@localhost:5672"
exchange = "hookbox"
routing_key = "events"

[emitter.pulsar]
url = "pulsar://localhost:6650"
topic = "persistent://public/default/hookbox-events"

[emitter.grpc]
endpoint = "http://localhost:50051"
timeout_ms = 5000              # optional
```

### Config structs

All backend config structs are defined in `hookbox-server/src/config.rs`. They are plain data structs with `serde` derives — no logic, no trait impls. The adapter crates do not depend on server config types; they accept primitive constructor arguments. `serve.rs` extracts fields from the config structs and passes them to each adapter's constructor. This avoids any circular dependency.

```rust
#[derive(Debug, Deserialize)]
pub struct EmitterConfig {
    #[serde(rename = "type", default = "default_emitter_type")]
    pub emitter_type: String,
    pub kafka: Option<KafkaEmitterConfig>,
    pub nats: Option<NatsEmitterConfig>,
    pub sqs: Option<SqsEmitterConfig>,
    pub redis: Option<RedisEmitterConfig>,
    pub rabbitmq: Option<RabbitmqEmitterConfig>,
    pub pulsar: Option<PulsarEmitterConfig>,
    pub grpc: Option<GrpcEmitterConfig>,
}

fn default_emitter_type() -> String {
    "channel".to_string()
}
```

All 7 new adapter crates must be added to:
1. The `[workspace]` `members` array in the root `Cargo.toml`
2. The `[dependencies]` section of `hookbox-cli/Cargo.toml` (hookbox-cli, not hookbox-server, constructs and owns the emitter)

**Dependency chain:** `hookbox-cli` depends on each adapter crate. `hookbox-server` owns config parsing. Adapter crates depend only on `hookbox` core. No circular dependencies.

### Wiring in serve.rs

Unknown `type` values must be rejected with an error — they must not silently fall back to a dev drain in production. When `[emitter]` section is omitted entirely, the default is `"channel"` (via `default_emitter_type`). Each selected backend must have its sub-config present and be validated before construction.

```rust
let emitter: Arc<dyn Emitter + Send + Sync> = match config.emitter.emitter_type.as_str() {
    "kafka" => {
        let cfg = config.emitter.kafka.as_ref()
            .ok_or_else(|| anyhow!("[emitter.kafka] section required when type = \"kafka\""))?;
        Arc::new(KafkaEmitter::new(/* fields from cfg */)?)
    }
    "nats" => {
        let cfg = config.emitter.nats.as_ref()
            .ok_or_else(|| anyhow!("[emitter.nats] section required when type = \"nats\""))?;
        Arc::new(NatsEmitter::new(/* fields from cfg */).await?)
    }
    "sqs" => {
        let cfg = config.emitter.sqs.as_ref()
            .ok_or_else(|| anyhow!("[emitter.sqs] section required when type = \"sqs\""))?;
        Arc::new(SqsEmitter::new(/* fields from cfg */).await?)
    }
    "redis" => {
        let cfg = config.emitter.redis.as_ref()
            .ok_or_else(|| anyhow!("[emitter.redis] section required when type = \"redis\""))?;
        Arc::new(RedisEmitter::new(/* fields from cfg */)?)
    }
    "rabbitmq" => {
        let cfg = config.emitter.rabbitmq.as_ref()
            .ok_or_else(|| anyhow!("[emitter.rabbitmq] section required when type = \"rabbitmq\""))?;
        Arc::new(RabbitmqEmitter::new(/* fields from cfg */).await?)
    }
    "pulsar" => {
        let cfg = config.emitter.pulsar.as_ref()
            .ok_or_else(|| anyhow!("[emitter.pulsar] section required when type = \"pulsar\""))?;
        Arc::new(PulsarEmitter::new(/* fields from cfg */).await?)
    }
    "grpc" => {
        let cfg = config.emitter.grpc.as_ref()
            .ok_or_else(|| anyhow!("[emitter.grpc] section required when type = \"grpc\""))?;
        Arc::new(GrpcEmitter::new(/* fields from cfg */).await?)
    }
    "channel" => Arc::new(ChannelEmitter::new(/* spawn drain task */)),
    other => bail!("unknown emitter type {:?}; valid values: kafka, nats, sqs, redis, rabbitmq, pulsar, grpc, channel", other),
};

let pipeline_emitter = Arc::clone(&emitter);
let worker_emitter = Arc::clone(&emitter);
```

The pipeline and worker each receive an `Arc::clone` of the shared emitter.

**Note:** `ServerAppState` alias must be updated from `ChannelEmitter` to `Arc<dyn Emitter + Send + Sync>` to support runtime emitter selection.

---

## Individual Emitter Specifications

### Kafka (`hookbox-emitter-kafka`)

- **Crate dep**: `rdkafka = { version = "0.36", features = ["cmake-build"] }`
- **Producer**: `FutureProducer` for async send
- **Topic**: from config
- **Message key**: `receipt_id.to_string()` (partition affinity)
- **Payload**: JSON bytes
- **Acks**: configurable (default: all)
- **Errors**: `KafkaError` → `EmitError::Downstream`, timeout → `EmitError::Timeout`

### NATS (`hookbox-emitter-nats`)

- **Crate dep**: `async-nats = "0.38"`
- **Connection**: `async_nats::connect(url)`
- **Publish**: `client.publish(subject, payload).await`
- **Headers**: `Hookbox-Receipt-Id: {receipt_id}`
- **Errors**: connection/publish errors → `EmitError::Downstream`

### AWS SQS (`hookbox-emitter-sqs`)

- **Crate dep**: `aws-sdk-sqs` (via `aws-config` for credential resolution)
- **Client**: `aws_sdk_sqs::Client::new(&config)`
- **Send (standard queue)**: `client.send_message().queue_url(url).message_body(json)`
- **Send (FIFO queue)**: additionally set `.message_group_id(provider_name).message_deduplication_id(receipt_id.to_string())`
- **FIFO support**: controlled by the `fifo` config field (default: `false`). `MessageGroupId` and `MessageDeduplicationId` are only set when `fifo = true`. Standard queues reject these fields and must not receive them.
- **Region**: from config or AWS defaults
- **Errors**: SDK errors → `EmitError::Downstream`

Config additions:

```toml
[emitter.sqs]
queue_url = "https://sqs.us-east-1.amazonaws.com/123456789/hookbox-events"
region = "us-east-1"
fifo = false   # set to true for .fifo queues; enables MessageGroupId + MessageDeduplicationId
```

### Redis Streams (`hookbox-emitter-redis`)

- **Crate dep**: `redis = { version = "0.27", features = ["aio", "tokio-comp"] }`
- **Connection**: `redis::Client::open(url)` → `get_multiplexed_tokio_connection()`
- **Command**: `XADD stream [MAXLEN ~ maxlen] * field1 value1 field2 value2 ...`
  - The `*` auto-ID instructs Redis to assign a monotonic entry ID (millisecond timestamp + sequence). hookbox does not control ordering — Redis stream ordering is by server-assigned entry ID.
  - Example: `XADD hookbox:events MAXLEN ~ 10000 * receipt_id <uuid> provider stripe event_type charge.succeeded payload <json>`
- **Fields stored per entry**: `receipt_id` (for traceability), `provider`, `event_type`, `payload` (JSON-encoded `NormalizedEvent`)
- **Ordering**: determined by Redis entry ID (`*`), not by `receipt_id`. `receipt_id` is stored as a field for lookup and traceability only, not for ordering.
- **Trimming**: approximate MAXLEN from config (default: no trim)
- **Errors**: connection/command errors → `EmitError::Downstream`

### RabbitMQ (`hookbox-emitter-rabbitmq`)

- **Crate dep**: `lapin = "2"`
- **Connection**: `Connection::connect(url, ConnectionProperties::default().with_tokio())`
- **Channel**: create channel, then publish directly — hookbox does **not** auto-declare exchanges
- **Publish**: `channel.basic_publish(exchange, routing_key, options, payload, properties)`
- **Properties**: `content_type = "application/json"`, `delivery_mode = 2` (persistent), `message_id = receipt_id`
- **Exchange lifecycle**: the exchange must pre-exist before hookbox starts. Use RabbitMQ Management UI or CLI to create it. hookbox will return `EmitError::Downstream` if the exchange does not exist.
- **Errors**: connection/publish errors → `EmitError::Downstream`

Config additions:

```toml
[emitter.rabbitmq]
url = "amqp://guest:guest@localhost:5672"
exchange = "hookbox"
routing_key = "events"
# exchange_type = "direct"  # documentation only; hookbox does not declare the exchange.
                             # hookbox assumes the exchange already exists.
                             # Use RabbitMQ management to create it before starting hookbox.
```

### Apache Pulsar (`hookbox-emitter-pulsar`)

- **Crate dep**: `pulsar = "6"`
- **Producer**: `Pulsar::builder(url).build().await` → `client.producer().with_topic(topic).build().await`
- **Send**: `producer.send(json_bytes).await` with key = receipt_id
- **Schema**: raw bytes (JSON)
- **Errors**: producer errors → `EmitError::Downstream`

### gRPC (`hookbox-emitter-grpc`)

- **Crate deps**: `tonic = "0.12"`, `prost = "0.13"`, `tonic-build = "0.12"` (build dep)
- **Wire format**: protobuf (this is the one exception to the JSON-only rule — gRPC uses protobuf over the wire by design). The JSON-only serialization rule applies to message-broker emitters (Kafka, NATS, SQS, Redis, RabbitMQ, Pulsar).
- **Proto**: `proto/hookbox_event.proto` defined by hookbox:

```protobuf
syntax = "proto3";
package hookbox.v1;

message WebhookEvent {
  string receipt_id = 1;
  string provider_name = 2;
  optional string event_type = 3;
  optional string external_reference = 4;
  optional string parsed_payload = 5;   // JSON-encoded string (serde_json::Value serialized to String)
  string payload_hash = 6;
  string received_at = 7;
  string metadata = 8;                  // JSON-encoded string (serde_json::Value serialized to String)
}

message EmitRequest {
  WebhookEvent event = 1;
}

message EmitResponse {
  bool success = 1;
  optional string error = 2;
}

service HookboxEmitter {
  rpc Emit(EmitRequest) returns (EmitResponse);
}
```

- **JSON fields in proto**: `parsed_payload` and `metadata` are `serde_json::Value` in Rust but `string` in proto. They are JSON-encoded strings (i.e., the `serde_json::Value` is serialized to a JSON string, then placed in the proto string field). This double-encoding is expected and standard for flexible schemas in protobuf when a schema registry is not used.
- **Client**: `HookboxEmitterClient::connect(endpoint).await`
- **Call**: `client.emit(EmitRequest { event }).await`
- **Timeout**: configurable (default: 5s)
- **Response mapping**: `EmitResponse { success: false, error }` → `EmitError::Downstream(error.unwrap_or_default())`
- **Errors**: tonic `Status` → `EmitError::Downstream`, timeout → `EmitError::Timeout`

---

## Changes to Existing Code

### `hookbox` core (`crates/hookbox/src/emitter.rs`)

Add blanket impls for both `Box<T>` and `Arc<T>`:

```rust
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

### `hookbox-server` config

Add `EmitterConfig` with `emitter_type` + optional backend sub-configs (all structs in `hookbox-server/src/config.rs`).
Add `#[serde(default)]` to `HookboxConfig` for `emitter` field so omitting `[emitter]` defaults to `type = "channel"`.

### `hookbox-cli` serve.rs

Replace hardcoded `ChannelEmitter` with config-driven emitter construction.
Construct one `Arc<dyn Emitter + Send + Sync>` and distribute `Arc::clone`s to the pipeline and the retry worker.

### `AppState` and pipeline generics

Currently `AppState` is generic over `E: Emitter`. With `Arc<dyn Emitter + Send + Sync>`:
- The blanket `Arc<T>` impl means `Arc<dyn Emitter + Send + Sync>` satisfies `E: Emitter` — no generics refactor needed.
- `ServerAppState` type alias must be updated from `ChannelEmitter` to `Arc<dyn Emitter + Send + Sync>` to support runtime emitter selection.

---

## Testing Strategy

### Unit tests per crate

Each emitter crate has `#[cfg(test)] mod tests` with:
- Mock/embedded backend where feasible (Redis via `redis-test`, NATS via `nats-server` test util)
- Config parsing tests
- Error mapping tests
- Serialization format verification (JSON output matches expected structure)

### Integration tests

For backends that need real infrastructure (Kafka, SQS, Pulsar):
- Tests marked `#[ignore]` by default
- CI can run them with Docker services (like the Postgres integration tests)
- Environment variables for connection strings

### Config dispatch test

In `hookbox-server` tests: verify that each `emitter_type` value parses and constructs without panicking (using mock configs).

---

## Future Work

Everything below is out of scope for this pass but tracked for follow-up.

### Emitter Architecture
- Fan-out to multiple emitters (`[[emitters]]` array config)
- Per-emitter retry policies (independent of pipeline retry)
- Emitter health reporting to `/readyz`
- Emitter-level metrics (`hookbox_emit_<backend>_total`, `hookbox_emit_<backend>_duration_seconds`)
- Dead-letter per emitter (separate from pipeline DLQ)
- Emitter connection pooling and reconnection strategies

### Serialization Formats
- CloudEvents envelope wrapper (industry standard)
- Protobuf native serialization with schema registry
- Avro serialization with schema registry
- MessagePack binary serialization

### gRPC Enhancements
- User-provided proto definitions (custom event schema)
- Bidirectional streaming (not just unary RPC)
- TLS/mTLS configuration
- Load balancing across multiple gRPC endpoints

### NATS Enhancements
- JetStream support for guaranteed delivery
- NATS KV for state management

### Kafka Enhancements
- Schema Registry integration (Confluent/Redpanda compatible)
- Transactions / exactly-once semantics
- Custom partitioner function
- Header propagation from webhook to Kafka message

### Additional Emitters (Tier 2)
- Google Cloud Pub/Sub (`google-cloud-pubsub`)
- AWS SNS (`aws-sdk-sns`)
- Azure Service Bus (`azure_messaging_servicebus`)
- AWS EventBridge (`aws-sdk-eventbridge`)
- Azure Event Hubs (Kafka-compatible protocol — covered by Kafka emitter)
- HTTP/Webhook relay (`reqwest` POST to configurable URL)
- AWS Kinesis (`aws-sdk-kinesis`)

---

## Non-Goals

- No fan-out (single emitter per instance)
- No schema registry integration
- No exactly-once semantics (at-least-once by design)
- No custom serialization formats (JSON only)
- No emitter-specific metrics (use existing pipeline metrics)
