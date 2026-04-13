# hookbox-emitter-nats

NATS emitter adapter for hookbox. Forwards normalized webhook events to a NATS subject using `async-nats`.

Each event is serialized as JSON and published to the configured subject.

## Configuration

In `hookbox.toml`:

```toml
[[emitters]]
name = "nats"                  # used as the `emitter` label on metrics and /readyz
type = "nats"
poll_interval_seconds = 1
concurrency = 8

[emitters.nats]
url = "nats://localhost:4222"
subject = "hookbox.events"

[emitters.retry]
max_attempts = 8
initial_backoff_seconds = 2
max_backoff_seconds = 600
backoff_multiplier = 2.0
jitter = 0.2
```

Multiple `[[emitters]]` blocks with `type = "nats"` are allowed — each runs an independent worker with its own `name`, poll interval, concurrency, and retry policy.

## Usage

The adapter is wired automatically by `hookbox-server` for every `[[emitters]]` entry with `type = "nats"`. No application code changes are needed.

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
