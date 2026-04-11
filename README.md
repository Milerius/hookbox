# hookbox

**Hookbox** is a durable webhook inbox for payment systems. It verifies, deduplicates, persists, replays, and audits every webhook **before** business logic touches it.

Built for teams integrating payment providers, banking rails, and crypto infrastructure that need a safer boundary between external callbacks and internal systems.

## Why Hookbox exists

Webhook delivery is messy by nature.

Payment and banking providers send:
- duplicate webhooks
- delayed callbacks
- out-of-order events
- inconsistent statuses
- retries at inconvenient times

Most teams end up rebuilding the same infrastructure around that mess:
- signature verification
- deduplication
- durable receipt storage
- replay and redrive
- dead-letter handling
- auditability and observability

Hookbox provides that boundary so your application can focus on business logic instead of callback correctness.

## How it works

Every incoming webhook passes through a five-stage pipeline:

```text
Provider webhook
    → Receive (assign receipt ID)
    → Verify (signature check via provider adapter)
    → Dedupe (fast in-memory path + authoritative Postgres)
    → Store durably (ACK provider only after this succeeds)
    → Emit downstream (callback, channel, or message broker)
```

A few important rules:

- **verification failure** → receipt is stored, but not forwarded
- **duplicate receipt** → stored and marked, but not processed twice
- **emit failure** → receipt remains accepted and enters retry / DLQ flow
- **raw body is preserved immutably** → replay and re-verification stay possible

The result is a durable, replayable, auditable inbox between external webhook traffic and your internal systems.

## Use it as a library or as a standalone service

Hookbox is designed to work in two modes:

- **embedded library** for teams that want to integrate it into an existing Rust/Axum service
- **standalone service** for teams that want a dedicated webhook-ingestion boundary

### Embedded in your Axum app

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

### Standalone service

```bash
hookbox serve --config hookbox.toml
```

## What Hookbox gives you

- **Signature verification**  
  Pluggable verifier trait with Stripe, BVNK, and generic HMAC adapters.

- **Deduplication**  
  Configurable dedupe strategy with a fast in-memory path and Postgres as the source of truth.

- **Durable receipt storage**  
  Raw body bytes are preserved immutably. Providers are acknowledged only after durable write succeeds.

- **Replay and redrive**  
  Re-emit any receipt through the CLI or admin API.

- **Dead-letter queue support**  
  Failed emissions are captured, inspectable, and retryable.

- **Retry worker**  
  Background retries with configurable interval and max attempts.

- **Observability by default**  
  Structured tracing, Prometheus metrics, health/readiness endpoints, and operational visibility at every pipeline stage.

- **CLI tooling**  
  Inspect receipts, replay failures, and manage the dead-letter queue.

## Who this is for

Hookbox is useful for teams building or operating:
- payment processors
- payout systems
- treasury / wallet infrastructure
- banking integrations
- crypto-to-fiat rails
- webhook-heavy financial platforms

If your system depends on external callbacks but correctness, replayability, and auditability matter, Hookbox is designed for that problem.

## Workspace

```text
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
# Run server
hookbox serve --config hookbox.toml

# Inspect receipts
hookbox receipts list --database-url <url> --provider stripe --state failed
hookbox receipts inspect --database-url <url> <receipt_id>
hookbox receipts search --database-url <url> --external-ref pay_123

# Replay
hookbox replay id --database-url <url> <receipt_id>
hookbox replay failed --database-url <url> --since 1h --provider stripe

# Dead letter queue
hookbox dlq list --database-url <url> --provider stripe
hookbox dlq inspect --database-url <url> <receipt_id>
hookbox dlq retry --database-url <url> <receipt_id>
```

All commands except `serve` accept `--database-url` or the `DATABASE_URL` environment variable for direct database access.  
The `serve` command uses `--config` with a TOML file instead.

## Design principles

1. **Receive first, process second**  
   ACK the provider only after durable write succeeds.

2. **Idempotent by default**  
   Dedupe before business processing.

3. **Raw body immutable**  
   Preserve original bytes exactly for replay and signature verification.

4. **Replayable everything**  
   Any receipt can be re-processed later.

5. **Provider-agnostic core**  
   Traits live in the core crate; implementations live in extension crates.

6. **Observable by default**  
   Every pipeline stage emits logs and metrics.

## What Hookbox is not

Hookbox is a **durable webhook inbox**, not a full payment platform.

It is designed to sit at the boundary between external webhook traffic and your internal systems. It helps you receive, verify, deduplicate, persist, replay, and safely forward webhook events.

Hookbox does **not** replace:

- **Your ledger**  
  Hookbox stores webhook receipts, not financial balances, journal entries, or accounting truth.

- **Your payment orchestration engine**  
  Hookbox does not manage multi-step payment workflows, retries across external providers, or business-level state machines for payouts, settlements, or conversions.

- **Your reconciliation system**  
  Hookbox does not compare internal records against provider statements, ledger balances, or settlement reports.

- **Your queue or event bus**  
  Hookbox can emit downstream events, but it is not a general-purpose message broker or stream-processing platform.

- **Your provider SDKs or payment gateway**  
  Hookbox does not create payments, submit payouts, fetch balances, or replace direct integration with payment APIs.

- **Your compliance stack**  
  Hookbox does not perform KYC, AML, sanctions screening, or case management.

What Hookbox *does* replace is the repetitive, fragile webhook edge logic that teams otherwise rebuild in every service.

## Status

Early development. The design spec is in [`docs/`](docs/).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE) at your option.
