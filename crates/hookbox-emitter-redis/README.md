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
