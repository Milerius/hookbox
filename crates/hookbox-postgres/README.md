# hookbox-postgres

PostgreSQL storage backend for hookbox.

Implements `Storage`, `DedupeStrategy`, and `DeliveryStorage` from `hookbox` core using SQLx. This is the authoritative durability layer — the ingest pipeline writes through it, the background `EmitterWorker` (in `hookbox-server`) claims work from it, and the admin API reads receipt/delivery state from it.

## What This Crate Provides

```
┌──────────────────────────────────────────────────────────────┐
│  hookbox-postgres                                            │
│                                                              │
│  PostgresStorage ──────► implements Storage                  │
│    • store()                → INSERT receipt with UK dedupe  │
│    • store_with_deliveries()→ receipt + N delivery rows in   │
│                               one tx (fan-out primitive)     │
│    • get() / query()        → receipt reads                  │
│    • update_state()         → legacy processing_state write  │
│                                                              │
│  PostgresStorage ──────► implements DeliveryStorage          │
│    • claim_pending()        → SKIP LOCKED lease of N rows    │
│    • reclaim_expired()      → revive stuck InFlight rows     │
│    • mark_emitted()         → terminal success               │
│    • mark_failed()          → bump attempts, schedule retry  │
│    • mark_dead_lettered()   → terminal failure               │
│    • insert_replay[s]()     → atomic fan-out replay          │
│    • count_pending / _dlq / _in_flight                       │
│    • get_delivery / get_deliveries_for_receipt / list_dlq    │
│                                                              │
│  StorageDedupe ────────► implements DedupeStrategy           │
│    • check()                → SELECT EXISTS by dedupe_key    │
│    • record()               → no-op (store is authority)     │
│                                                              │
│  Migrations ───────────► schema management                   │
│    • 0001_initial           — webhook_receipts + indexes     │
│    • 0002_webhook_deliveries — per-emitter fan-out queue     │
│    • 0003_in_flight_reclaim — partial index for reclaim loop │
└──────────────────────────────────────────────────────────────┘
```

## Schema

### `webhook_receipts` — authoritative record of every ingested webhook

```
receipt_id          UUID PRIMARY KEY
provider_name       TEXT NOT NULL
provider_event_id   TEXT
external_reference  TEXT
dedupe_key          TEXT NOT NULL  (UNIQUE INDEX)
payload_hash        TEXT NOT NULL
raw_body            BYTEA NOT NULL
parsed_payload      JSONB
raw_headers         JSONB NOT NULL
normalized_event_type TEXT
verification_status TEXT NOT NULL
verification_reason TEXT
processing_state    TEXT NOT NULL  (legacy — derived from deliveries)
received_at         TIMESTAMPTZ NOT NULL
processed_at        TIMESTAMPTZ
metadata            JSONB NOT NULL DEFAULT '{}'
```

### `webhook_deliveries` — per-emitter delivery queue (fan-out)

```
delivery_id       UUID PRIMARY KEY
receipt_id        UUID NOT NULL REFERENCES webhook_receipts ON DELETE CASCADE
emitter_name      TEXT NOT NULL
state             TEXT NOT NULL   ('pending' | 'in_flight' | 'failed' |
                                   'emitted' | 'dead_lettered')
attempt_count     INTEGER NOT NULL DEFAULT 0
last_error        TEXT
last_attempt_at   TIMESTAMPTZ
next_attempt_at   TIMESTAMPTZ NOT NULL
emitted_at        TIMESTAMPTZ
immutable         BOOLEAN NOT NULL DEFAULT FALSE
created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
```

### Key Indexes

| Index | Columns | Purpose |
|-------|---------|---------|
| `webhook_receipts.dedupe_key` | unique | Authoritative duplicate detection |
| `webhook_receipts` | `(provider_name, provider_event_id)` | Lookup by provider event |
| `webhook_receipts` | `external_reference` | Lookup by business reference |
| `webhook_receipts` | `received_at` | Time-range, retention |
| `idx_webhook_deliveries_dispatch` | `(emitter_name, next_attempt_at) WHERE state IN ('pending','failed') AND NOT immutable` | Hot worker claim query |
| `idx_webhook_deliveries_dlq` | `(emitter_name, state) WHERE state = 'dead_lettered'` | DLQ depth + listing |
| `webhook_deliveries_in_flight_reclaim_idx` | `(emitter_name, last_attempt_at) WHERE state = 'in_flight' AND NOT immutable` | Reclaim loop (migration 0003) |
| `idx_webhook_deliveries_receipt` | `receipt_id` | Receipt → deliveries join |
| `idx_webhook_deliveries_receipt_emitter` | `(receipt_id, emitter_name)` | Latest-per-emitter projection |

## Dedupe Authority

`PostgresStorage::store_with_deliveries()` uses the unique constraint on `dedupe_key` as the authoritative duplicate check. Conflict → `StoreResult::Duplicate { existing_id }`. `StorageDedupe` is an advisory fast-path only; the store layer always has the final word. Fan-out delivery rows are inserted in the **same transaction** as the receipt, so the system never ACKs a partially-fanned-out webhook.

## Usage

```rust
use hookbox::{HookboxPipeline, InMemoryRecentDedupe, LayeredDedupe};
use hookbox_postgres::{PostgresStorage, StorageDedupe};

let pool = sqlx::PgPool::connect("postgres://localhost/hookbox").await?;
let storage = PostgresStorage::new(pool.clone());
storage.migrate().await?;

let dedupe = LayeredDedupe::new(
    InMemoryRecentDedupe::new(10_000),
    StorageDedupe::new(pool.clone()),   // takes PgPool, not PostgresStorage
);

let pipeline = HookboxPipeline::builder()
    .storage(storage)
    .dedupe(dedupe)
    .emitter_names(vec!["kafka".to_owned(), "sqs".to_owned()])
    .build();
```

## Ops Queries

Methods on `PostgresStorage` beyond the core traits, used by the CLI and server admin API:

| Method | Purpose |
|--------|---------|
| `query_for_retry(max_attempts)` | Fetch EmitFailed receipts eligible for retry (legacy receipt-level) |
| `retry_failed(id, max_attempts)` | Atomic: increment emit_count, promote to DeadLettered at the limit |
| `reset_for_retry(id)` | Reset receipt for replay via CLI |
| `query_by_external_reference(ref, limit)` | Search by business reference |
| `query_failed_since(provider, since, limit)` | Failed receipts since timestamp |

Plus every `DeliveryStorage` method (`claim_pending`, `reclaim_expired`, `mark_emitted`, `mark_failed`, `mark_dead_lettered`, `insert_replay`, `insert_replays`, `count_pending`, `count_in_flight`, `count_dlq`, `get_delivery`, `get_deliveries_for_receipt`, `list_dlq`).

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
