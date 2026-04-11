# hookbox-emitter-nats

NATS emitter adapter for hookbox. Forwards normalized webhook events to a NATS subject using `async-nats`.

Each event is serialized as JSON and published to the configured subject.

## Configuration

In `hookbox.toml`:

```toml
[emitter]
type = "nats"

[emitter.nats]
url = "nats://localhost:4222"
subject = "hookbox.events"
```

## Usage

The adapter is wired automatically by `hookbox-server` when `emitter.type = "nats"` is set in the configuration. No application code changes are needed.

For embedded usage:

```rust
use hookbox_emitter_nats::NatsEmitter;

let emitter = NatsEmitter::new("nats://localhost:4222", "hookbox.events".to_owned()).await?;
```

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
