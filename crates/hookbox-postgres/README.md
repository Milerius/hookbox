# hookbox-postgres

`PostgreSQL` storage backend for hookbox.

Implements the `Storage` and `DedupeStrategy` traits from `hookbox` core using SQLx and `PostgreSQL`.

## What This Crate Provides

```
┌──────────────────────────────────────────────────┐
│  hookbox-postgres                                │
│                                                  │
│  PostgresStorage ──────► implements Storage       │
│    • store()        → INSERT with dedupe_key UK  │
│    • get()          → SELECT by receipt_id       │
│    • update_state() → UPDATE processing_state    │
│    • query()        → filtered SELECT            │
│                                                  │
│  StorageDedupe ────────► implements DedupeStrategy │
│    • check()        → SELECT EXISTS by dedupe_key│
│    • record()       → (no-op, store handles it)  │
│                                                  │
│  Migrations ───────────► schema management       │
│    • webhook_receipts table                      │
│    • indexes on dedupe_key, provider, state, ref │
└──────────────────────────────────────────────────┘
```

## Schema

The `webhook_receipts` table is the single source of truth:

```
webhook_receipts
├── receipt_id          UUID PRIMARY KEY
├── provider_name       TEXT NOT NULL
├── provider_event_id   TEXT
├── external_reference  TEXT
├── dedupe_key          TEXT NOT NULL  (UNIQUE INDEX)
├── payload_hash        TEXT NOT NULL
├── raw_body            BYTEA NOT NULL
├── parsed_payload      JSONB
├── raw_headers         JSONB NOT NULL
├── normalized_event_type TEXT
├── verification_status TEXT NOT NULL
├── verification_reason TEXT
├── processing_state    TEXT NOT NULL
├── emit_count          INTEGER NOT NULL DEFAULT 0
├── last_error          TEXT
├── received_at         TIMESTAMPTZ NOT NULL
├── processed_at        TIMESTAMPTZ
└── metadata            JSONB NOT NULL DEFAULT '{}'
```

### Key Indexes

| Index | Columns | Purpose |
|-------|---------|---------|
| Unique | `dedupe_key` | Authoritative duplicate detection |
| B-tree | `(provider_name, provider_event_id)` | Lookup by provider event |
| B-tree | `external_reference` | Lookup by business reference |
| B-tree | `processing_state` | Filter by lifecycle state |
| B-tree | `received_at` | Time-range queries, retention |
| B-tree | `(provider_name, processing_state)` | Common compound query |

## Dedupe Authority

`PostgresStorage::store()` uses the unique constraint on `dedupe_key` as the authoritative duplicate check. If an INSERT conflicts, it returns `StoreResult::Duplicate` with the existing receipt ID. This is the source of truth — the `StorageDedupe` adapter provides an advisory fast-path check, but the store layer always has the final word.

## Usage

```rust
use hookbox_postgres::PostgresStorage;

let pool = sqlx::PgPool::connect("postgres://localhost/hookbox").await?;
let storage = PostgresStorage::new(pool).await?;

// Use as Storage
pipeline.builder().storage(storage.clone());

// Use as advisory dedupe (wraps the same pool)
let dedupe = StorageDedupe::new(storage.clone());
```

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
