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

### Blanket impl for Box<dyn Emitter>

Add to `hookbox` core:

```rust
#[async_trait]
impl<T: Emitter + ?Sized> Emitter for Box<T> {
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        (**self).emit(event).await
    }
}
```

This allows `Box<dyn Emitter + Send + Sync>` to be used as the emitter type in the pipeline and retry worker, enabling config-driven emitter selection at runtime.

### Message key

All emitters use `receipt_id` as the message key/ID for:
- Partition affinity (Kafka, Pulsar)
- Deduplication (SQS FIFO)
- Ordering (Redis Streams)
- Traceability (all backends)

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

Add `EmitterConfig` to `hookbox-server/src/config.rs`:

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
```

Each backend config struct lives in its own crate and is re-exported or duplicated in the server config. The simplest approach: define them in the server crate since it owns the TOML parsing.

### Wiring in serve.rs

```rust
let emitter: Box<dyn Emitter + Send + Sync> = match config.emitter.emitter_type.as_str() {
    "kafka" => { /* construct KafkaEmitter */ }
    "nats" => { /* construct NatsEmitter */ }
    "sqs" => { /* construct SqsEmitter */ }
    "redis" => { /* construct RedisEmitter */ }
    "rabbitmq" => { /* construct RabbitmqEmitter */ }
    "pulsar" => { /* construct PulsarEmitter */ }
    "grpc" => { /* construct GrpcEmitter */ }
    "channel" | _ => { /* existing ChannelEmitter + drain task */ }
};
```

The pipeline and worker both receive this `Box<dyn Emitter>`.

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
- **Send**: `client.send_message().queue_url(url).message_body(json).message_group_id(provider).message_deduplication_id(receipt_id)`
- **FIFO support**: `MessageGroupId` = provider name, `MessageDeduplicationId` = receipt_id
- **Region**: from config or AWS defaults
- **Errors**: SDK errors → `EmitError::Downstream`

### Redis Streams (`hookbox-emitter-redis`)

- **Crate dep**: `redis = { version = "0.27", features = ["aio", "tokio-comp"] }`
- **Connection**: `redis::Client::open(url)` → `get_multiplexed_tokio_connection()`
- **Command**: `XADD stream MAXLEN ~ maxlen * receipt_id json_payload`
- **Fields**: `receipt_id`, `provider`, `event_type`, `payload` (JSON)
- **Trimming**: approximate MAXLEN from config (default: no trim)
- **Errors**: connection/command errors → `EmitError::Downstream`

### RabbitMQ (`hookbox-emitter-rabbitmq`)

- **Crate dep**: `lapin = "2"`
- **Connection**: `Connection::connect(url, ConnectionProperties::default().with_tokio())`
- **Channel**: create channel, declare exchange if needed
- **Publish**: `channel.basic_publish(exchange, routing_key, options, payload, properties)`
- **Properties**: `content_type = "application/json"`, `delivery_mode = 2` (persistent), `message_id = receipt_id`
- **Errors**: connection/publish errors → `EmitError::Downstream`

### Apache Pulsar (`hookbox-emitter-pulsar`)

- **Crate dep**: `pulsar = "6"`
- **Producer**: `Pulsar::builder(url).build().await` → `client.producer().with_topic(topic).build().await`
- **Send**: `producer.send(json_bytes).await` with key = receipt_id
- **Schema**: raw bytes (JSON)
- **Errors**: producer errors → `EmitError::Downstream`

### gRPC (`hookbox-emitter-grpc`)

- **Crate deps**: `tonic = "0.12"`, `prost = "0.13"`, `tonic-build = "0.12"` (build dep)
- **Proto**: `proto/hookbox_event.proto` defined by hookbox:

```protobuf
syntax = "proto3";
package hookbox.v1;

message WebhookEvent {
  string receipt_id = 1;
  string provider_name = 2;
  optional string event_type = 3;
  optional string external_reference = 4;
  optional string parsed_payload = 5;
  string payload_hash = 6;
  string received_at = 7;
  string metadata = 8;
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

- **Client**: `HookboxEmitterClient::connect(endpoint).await`
- **Call**: `client.emit(EmitRequest { event }).await`
- **Timeout**: configurable (default: 5s)
- **Errors**: tonic `Status` → `EmitError::Downstream`, timeout → `EmitError::Timeout`

---

## Changes to Existing Code

### `hookbox` core (`crates/hookbox/src/emitter.rs`)

Add blanket impl:
```rust
#[async_trait]
impl<T: Emitter + ?Sized> Emitter for Box<T> {
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        (**self).emit(event).await
    }
}
```

### `hookbox-server` config

Add `EmitterConfig` with `emitter_type` + optional backend sub-configs.
Add `#[serde(default)]` to `HookboxConfig` for `emitter` field.

### `hookbox-cli` serve.rs

Replace hardcoded `ChannelEmitter` with config-driven emitter construction.
Both pipeline and worker receive `Box<dyn Emitter + Send + Sync>`.

### `AppState` and pipeline generics

Currently `AppState` is generic over `E: Emitter`. With `Box<dyn Emitter>`:
- Either keep generics and use `Box<dyn Emitter + Send + Sync>` as the concrete `E`
- Or simplify to store `Box<dyn Emitter + Send + Sync>` directly

The blanket impl approach means `Box<dyn Emitter + Send + Sync>` satisfies `E: Emitter`, so no refactor needed — just pass the box where `ChannelEmitter` was used.

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
