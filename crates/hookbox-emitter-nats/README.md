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

## Local Testing

Start a NATS server with Docker:

```bash
docker compose -f docker-compose.test.yml up nats -d
```

Or manually:

```bash
docker run -d --name hookbox-nats -p 4222:4222 nats:latest
```

Run the smoke test:

```bash
NATS_URL=nats://localhost:4222 cargo test -p hookbox-integration-tests --test emitter_smoke_test -- --ignored nats_emitter_smoke
```

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
