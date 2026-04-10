# hookbox

A durable webhook inbox for payment systems: verify, dedupe, persist, replay, and audit every event before business logic touches it.

## Problem

Payment providers and banking/crypto integrations send duplicate webhooks, delayed callbacks, out-of-order events, and inconsistent statuses. Most teams rebuild the same painful infrastructure: HMAC verification, deduplication, durable storage, replay, dead-letter handling, and auditability.

Hookbox handles all of that so your business logic doesn't have to.

## How it works

Every incoming webhook passes through a five-stage pipeline:

```
Provider webhook
    → Receive (assign receipt ID)
    → Verify (signature check via provider adapter)
    → Dedupe (fast LRU + authoritative Postgres)
    → Store durably (ACK provider only after this succeeds)
    → Emit downstream (callback, channel, or message broker)
```

If verification fails, the receipt is stored but not forwarded. If emission fails, the receipt is accepted and enters retry/DLQ. The webhook is never lost.

## Use it as a library or a service

**Embedded in your Axum app:**

```rust
let pipeline = HookboxPipeline::builder()
    .storage(PostgresStorage::new(pool).await?)
    .dedupe(LayeredDedupe::new(
        InMemoryLruDedupe::new(10_000),
        StorageDedupe::new(storage.clone()),
    ))
    .emitter(CallbackEmitter::new(|event| async move {
        my_business_logic(event).await
    }))
    .verifier(StripeVerifier::new("stripe".to_owned(), stripe_secret))
    .verifier(GenericHmacVerifier::new("bvnk", config))
    .build();

let app = Router::new()
    .route("/webhooks/:provider", post(hookbox::axum::handler(pipeline)));
```

**Standalone service:**

```bash
hookbox serve --config hookbox.toml
```

## Features

- **Signature verification** — pluggable trait with Stripe, BVNK, and generic HMAC adapters
- **Deduplication** — configurable strategy with LRU fast path and Postgres as source of truth
- **Durable storage** — raw body bytes preserved immutably; ACK only after durable write
- **Replay & redrive** — re-emit any receipt via CLI or admin API
- **Dead-letter queue** — failed emissions are captured, inspectable, and retryable
- **Observability** — structured tracing, Prometheus metrics, health/readiness endpoints
- **CLI tooling** — inspect receipts, replay failures, manage DLQ

## Workspace

```
hookbox/
├── crates/
│   ├── hookbox/              # core: traits, types, pipeline, lightweight impls
│   ├── hookbox-postgres/     # PostgreSQL storage backend
│   ├── hookbox-providers/    # Stripe, BVNK, generic HMAC verifiers
│   ├── hookbox-server/       # standalone Axum HTTP server
│   └── hookbox-cli/          # CLI binary (inspect, replay, serve)
├── integration-tests/
├── examples/
└── docs/
```

## CLI

```bash
# Inspect
hookbox receipts list --provider stripe --state failed
hookbox receipts inspect <receipt_id>
hookbox receipts search --ref "pay_abc123"

# Replay
hookbox replay <receipt_id>
hookbox replay failed --provider stripe --since 1h

# Dead letter queue
hookbox dlq list --provider stripe
hookbox dlq retry <receipt_id>

# Run server
hookbox serve --config hookbox.toml
```

## Design principles

1. **Receive first, process second** — ACK provider only after durable write
2. **Idempotent by default** — dedupe before business processing
3. **Raw body immutable** — original bytes preserved exactly for replay verification
4. **Replayable everything** — any receipt can be re-processed at any time
5. **Provider-agnostic core** — traits in core, implementations in extension crates
6. **Observable by default** — structured logs and metrics at every pipeline stage

## Status

Early development. The design spec is in [`docs/`](docs/).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE) at your option.
