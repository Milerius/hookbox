# hookbox-integration-tests

Full-stack integration suite for hookbox. Exercises the ingest pipeline, admin API, CLI, delivery storage, and background worker against a real Postgres instance.

Unlike `scenario-tests/` (which is Gherkin-driven and optionally spins its own Postgres via testcontainers), these tests require an **already-running Postgres** and speak directly to it — they are the canonical way to verify SQL behaviour, migration correctness, and end-to-end HTTP flows.

## Prerequisites

A reachable Postgres; defaults to `postgres://localhost/hookbox_test`. Override with `DATABASE_URL`:

```bash
export DATABASE_URL=postgres://hookbox:hookbox@localhost/hookbox_test
```

The suite calls `PostgresStorage::migrate()` on setup, so you only need the empty database; migrations `0001`, `0002`, and `0003` run automatically.

## Test Files

```
integration-tests/tests/
├── http_test.rs          # Axum in-process HTTP via tower::ServiceExt —
│                         # POST /webhooks/:provider happy path, duplicate
│                         # detection, 401 on verification failure, admin
│                         # auth header enforcement, /readyz / /metrics /dlq
├── fanout.rs             # End-to-end fan-out: ingest with multi-emitter config,
│                         # run EmitterWorker, assert delivery rows transition
│                         # Pending → InFlight → Emitted / DeadLettered
├── worker_test.rs        # Legacy RetryWorker: ingest, manually mark
│                         # EmitFailed, run the retry loop, assert promotions
├── admin_api_test.rs     # /api/receipts and /api/receipts/:id derive the
│                         # ProcessingState from the delivery rows rather than
│                         # reading the legacy column
├── delivery_storage.rs   # DeliveryStorage trait against real Postgres:
│                         # claim_pending, reclaim_expired, mark_emitted,
│                         # mark_failed, mark_dead_lettered, insert_replay(s),
│                         # get_delivery, list_dlq, count_dlq/pending/in_flight
├── migration_0002.rs     # Migration 0002: verifies webhook_deliveries,
│                         # its indexes, and the backfill work on top of a
│                         # database already containing receipts from 0001
├── ingest_test.rs        # HookboxPipeline::ingest against real Postgres —
│                         # duplicate dedupe, verification, store transaction
├── cli_test.rs           # Shell-level CLI invocations against a real DB
└── cli_handlers_test.rs  # Unit-style tests of CLI handlers against Postgres
```

## Running

```bash
# Full suite (nextest is faster — parallelises across tests)
cargo nextest run -p hookbox-integration-tests

# Plain cargo test fallback
cargo test -p hookbox-integration-tests

# One test file
cargo nextest run -p hookbox-integration-tests -- fanout
cargo nextest run -p hookbox-integration-tests -- delivery_storage

# One test within a file
cargo nextest run -p hookbox-integration-tests full_http_flow
```

Each test file that truncates or rewrites data takes a global mutex so tests running on a shared database don't clobber each other.

## When to Add a Test Here

Use `integration-tests` when the assertion requires **real Postgres behaviour** — SQL semantics (`SKIP LOCKED`, transaction rollback, unique constraint races, migration ordering, index correctness) or full request flow through Axum + pool + worker. For assertions that can be made with an in-memory fake, prefer a unit test in the owning crate or a Gherkin scenario in [`scenario-tests/`](../scenario-tests/README.md).

## License

Licensed under either of [Apache License, Version 2.0](../LICENSE-APACHE) or [MIT License](../LICENSE-MIT) at your option.
