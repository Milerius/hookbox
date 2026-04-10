# hookbox

Core library for the hookbox durable webhook inbox.

Provides traits, domain types, pipeline orchestration, and lightweight built-in implementations. This crate has no heavy dependencies — no Axum, no SQLx, no provider-specific crypto.

## Architecture

```
                    ┌─────────────────────────────┐
                    │      HookboxPipeline        │
                    │                             │
  HTTP request ───► │  1. Receive (assign ID)     │
                    │         │                   │
                    │         ▼                   │
                    │  2. Verify (signature)      │──► VerificationFailed
                    │         │                   │
                    │         ▼                   │
                    │  3. Dedupe (fast + auth)    │──► Duplicate
                    │         │                   │
                    │         ▼                   │
                    │  4. Store (durable write)   │──► ACK 200
                    │         │                   │
                    │         ▼                   │
                    │  5. Emit (downstream)       │──► EmitFailed → DLQ
                    │                             │
                    └─────────────────────────────┘
```

## Core Traits

Four extension points, all `Send + Sync`:

| Trait | Purpose | MVP implementations |
|-------|---------|---------------------|
| `SignatureVerifier` | Provider signature verification | (in `hookbox-providers`) |
| `Storage` | Durable receipt persistence | (in `hookbox-postgres`) |
| `DedupeStrategy` | Advisory duplicate detection | `InMemoryLruDedupe`, `LayeredDedupe` |
| `Emitter` | Downstream event forwarding | `CallbackEmitter`, `ChannelEmitter` |

## Key Types

```rust
WebhookReceipt     // Central entity — one per ingested webhook
NormalizedEvent    // Emitted downstream after durable storage
ProcessingState    // Lifecycle: Received → Verified → Stored → Emitted → Processed
VerificationStatus // Verified | Failed | Skipped
DedupeDecision     // New | Duplicate | Conflict
StoreResult        // Stored | Duplicate { existing_id }
IngestResult       // Accepted | Duplicate | VerificationFailed
```

## Usage

```rust
use hookbox::{HookboxPipeline, CallbackEmitter, InMemoryLruDedupe, LayeredDedupe};

let pipeline = HookboxPipeline::builder()
    .storage(my_storage)
    .dedupe(LayeredDedupe::new(
        InMemoryLruDedupe::new(10_000),
        my_storage_dedupe,
    ))
    .emitter(CallbackEmitter::new(|event| async move {
        handle_event(event).await
    }))
    .verifier(my_verifier)
    .build();

let result = pipeline.ingest("stripe", headers, body).await?;
```

## Design Invariants

- **ACK only after durable store** — `ingest()` returns `Accepted` only after the receipt is persisted
- **Emit failure does not invalidate acceptance** — the receipt is safe; emission retries independently
- **Raw body bytes are immutable** — never modified after storage
- **Payload hash always computed** — SHA-256 of raw body on every receipt
- **Postgres is dedupe authority** — `DedupeStrategy` is advisory; `Storage::store()` is truth

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
