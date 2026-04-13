# hookbox

Core library for the hookbox durable webhook inbox.

Provides traits, domain types, and the ingest pipeline. This crate has no heavy dependencies — no Axum, no SQLx, no provider-specific crypto. Emitter backends, Postgres storage, signature verifiers, and the HTTP server all live in sibling crates and plug in through the traits defined here.

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
                    │  4. Store receipt + one     │──► ACK 200
                    │     pending delivery row    │
                    │     per configured emitter  │
                    └─────────────────────────────┘
                              │
                              ▼
                    ┌─────────────────────────────┐
                    │  EmitterWorker (background) │
                    │  (in hookbox-server)        │
                    │                             │
                    │  claim_pending → emit →     │
                    │  mark_emitted / mark_failed │
                    │  exhausted → DeadLettered   │
                    └─────────────────────────────┘
```

Inline emission was removed. `ingest()` now fans out by inserting one `webhook_deliveries` row per configured emitter name inside the same transaction as the receipt. A separate `EmitterWorker` (one per emitter, spawned by `hookbox serve`) claims pending rows and drives them through `InFlight → Emitted` or exhausted-retry → `DeadLettered`.

## Core Traits

Five extension points, all `Send + Sync`:

| Trait | Purpose | Implementations |
|-------|---------|-----------------|
| `SignatureVerifier` | Provider signature verification | `hookbox-providers` (Stripe, BVNK, generic HMAC, Adyen, Triple-A, Walapay) |
| `Storage` | Durable receipt persistence, authoritative dedupe via `StoreResult`, and fan-out via `store_with_deliveries` | `hookbox-postgres::PostgresStorage` |
| `DeliveryStorage` | Per-emitter delivery queue operations (`claim_pending`, `mark_emitted`, `mark_failed`, `insert_replays`, DLQ ops) | `hookbox-postgres::PostgresStorage` |
| `DedupeStrategy` | Advisory fast-path duplicate detection | `InMemoryRecentDedupe`, `LayeredDedupe`, `hookbox-postgres::StorageDedupe` |
| `Emitter` | Downstream event forwarding | `hookbox-emitter-{kafka,nats,sqs,redis}`; `CallbackEmitter` / `ChannelEmitter` for tests |

The pipeline is generic over `Storage` and `DedupeStrategy`. It does **not** hold an `Emitter` — emitters are driven out-of-band by `EmitterWorker` in the server crate.

## Key Types

```rust
WebhookReceipt          // Central entity — one per ingested webhook
WebhookDelivery         // Per-emitter delivery row (fan-out queue)
NormalizedEvent         // Handed to emitters on dispatch
ProcessingState         // Legacy receipt-level lifecycle (kept for compatibility)
DeliveryState           // Pending → InFlight → Emitted | Failed | DeadLettered
RetryPolicy             // max_attempts, backoff, jitter
VerificationStatus      // Verified | Failed | Skipped
DedupeDecision          // New | Duplicate | Conflict
StoreResult             // Stored | Duplicate { existing_id }
IngestResult            // Accepted | Duplicate | VerificationFailed
ReceiptId, DeliveryId   // Newtypes over Uuid
```

`receipt_aggregate_state` in `transitions` projects a receipt's many `WebhookDelivery` rows down to a single derived `ProcessingState` — that's what the admin API reports.

## Usage

```rust
use hookbox::{HookboxPipeline, InMemoryRecentDedupe, LayeredDedupe};
use hookbox_postgres::{PostgresStorage, StorageDedupe};

let storage = PostgresStorage::new(pool.clone());
let dedupe = LayeredDedupe::new(
    InMemoryRecentDedupe::new(10_000),
    StorageDedupe::new(pool.clone()),
);

let pipeline = HookboxPipeline::builder()
    .storage(storage)
    .dedupe(dedupe)
    .emitter_names(vec!["kafka".to_owned(), "sqs".to_owned()])
    .verifier(my_verifier)
    .build();

let result = pipeline.ingest("stripe", headers, body).await?;
```

`emitter_names` is the list of `[[emitters]]` names from config — `ingest()` writes one pending delivery row per name, atomically with the receipt.

## Design Invariants

- **ACK only after durable store** — `ingest()` returns `Accepted` only after the receipt **and** its delivery rows are persisted in a single transaction.
- **Fan-out is atomic** — if any delivery row fails to insert, the whole receipt rolls back. The system never ACKs a partially-fanned-out webhook.
- **Delivery failure does not invalidate acceptance** — once a receipt is stored, its delivery rows drive retry independently; a dead-lettered delivery does not un-ack the webhook.
- **Raw body bytes are immutable** — never modified after storage.
- **Payload hash always computed** — SHA-256 of raw body on every receipt.
- **Postgres is dedupe authority** — `DedupeStrategy` is advisory; `Storage::store_with_deliveries()` is truth.

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
