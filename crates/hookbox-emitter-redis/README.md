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
[[emitters]]
name = "redis"                 # used as the `emitter` label on metrics and /readyz
type = "redis"
poll_interval_seconds = 1
concurrency = 8

[emitters.redis]
url        = "redis://127.0.0.1:6379"
stream     = "hookbox.events"
maxlen     = 100000            # optional XADD MAXLEN ~
timeout_ms = 5000              # default 5000

[emitters.retry]
max_attempts = 8
initial_backoff_seconds = 2
max_backoff_seconds = 600
backoff_multiplier = 2.0
jitter = 0.2
```

Multiple `[[emitters]]` blocks with `type = "redis"` are allowed — each runs an independent worker with its own `name`, poll interval, concurrency, and retry policy. The adapter is wired automatically by `hookbox-server` for every entry with `type = "redis"`.

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
