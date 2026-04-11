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

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
