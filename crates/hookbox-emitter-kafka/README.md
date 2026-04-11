# hookbox-emitter-kafka

Kafka emitter adapter for hookbox. Forwards normalized webhook events to a Kafka topic using `rdkafka`.

Each event is serialized as JSON with the receipt ID as the message key, ensuring deterministic partition assignment for events originating from the same receipt.

## Configuration

In `hookbox.toml`:

```toml
[emitter]
type = "kafka"

[emitter.kafka]
brokers = "localhost:9092"
topic = "hookbox-events"
client_id = "hookbox"       # optional, default: "hookbox"
acks = "all"                # optional, default: "all"
timeout_ms = 5000           # optional, default: 5000
```

## Usage

The adapter is wired automatically by `hookbox-server` when `emitter.type = "kafka"` is set in the configuration. No application code changes are needed.

For embedded usage:

```rust
use hookbox_emitter_kafka::KafkaEmitter;

let emitter = KafkaEmitter::new(
    "localhost:9092",
    "hookbox-events".to_owned(),
    "hookbox",
    "all",
    5000,
)?;
```

## Local Testing

Start a Kafka broker with Docker:

```bash
docker compose -f docker-compose.test.yml up kafka -d
```

Or manually:

```bash
docker run -d --name hookbox-kafka -p 9092:9092 \
  -e KAFKA_NODE_ID=1 \
  -e KAFKA_PROCESS_ROLES=broker,controller \
  -e KAFKA_LISTENERS=PLAINTEXT://0.0.0.0:9092,CONTROLLER://0.0.0.0:9093 \
  -e KAFKA_ADVERTISED_LISTENERS=PLAINTEXT://localhost:9092 \
  -e KAFKA_CONTROLLER_LISTENER_NAMES=CONTROLLER \
  -e KAFKA_LISTENER_SECURITY_PROTOCOL_MAP=CONTROLLER:PLAINTEXT,PLAINTEXT:PLAINTEXT \
  -e KAFKA_CONTROLLER_QUORUM_VOTERS=1@localhost:9093 \
  -e CLUSTER_ID=MkU3OEVBNTcwNTJENDM2Qk \
  -e KAFKA_OFFSETS_TOPIC_REPLICATION_FACTOR=1 \
  confluentinc/cp-kafka:7.6.0
```

Run the smoke test:

```bash
KAFKA_BROKERS=localhost:9092 cargo test -p hookbox-integration-tests --test emitter_smoke_test -- --ignored kafka_emitter_smoke
```

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
