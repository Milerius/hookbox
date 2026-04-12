# Emitter Fan-Out Design

> **Status:** Approved 2026-04-12. Supersedes the single-emitter section of the original MVP design (`2026-04-10-hookbox-design.md`); the rest of that document remains in force.

## Goal

Replace hookbox's single-emitter model with a fan-out architecture: every accepted webhook is delivered independently to N configured emitters, each with its own retry policy, attempt count, failure state, and dead-letter queue. Emitters are isolated — a flaky downstream cannot slow or block the others, and cannot block ingest.

## Architecture decisions

These five decisions were made during the design brainstorm and drive everything else in this document.

| # | Decision | Choice |
|---|---|---|
| 1 | Routing model | **All-to-all.** Every configured emitter receives every accepted event. Per-provider and per-event-type routing filters are deferred. |
| 2 | Emit timing and dispatch model | **Background dispatch.** Ingest ACKs on durable store; the per-emitter workers handle first attempts and all retries through one code path. The pipeline no longer awaits emit. |
| 3 | Config backwards compatibility | **Hybrid.** Both `[emitter]` (legacy) and `[[emitters]]` (new) parse. Legacy is internally normalized to a one-element `[[emitters]]` with `name = "default"` and emits a deprecation warning. Both forms cannot be used simultaneously. |
| 4 | Existing data migration | **Backfill, immutable.** The SQL migration inserts one synthetic delivery row per existing receipt with `emitter_name = 'legacy'`, state mapped from the receipt's old `processing_state`, and `immutable = true`. Workers explicitly skip immutable rows. |
| 5 | Worker topology | **One worker per configured emitter.** Each emitter gets its own tokio task with its own polling loop, retry policy, concurrency cap, health state, and metric labels. |

## Scope

### In scope (single PR)

- New `webhook_deliveries` table; one row per delivery attempt against a `(receipt_id, emitter_name)` pair; multiple rows allowed (audit history).
- `[[emitters]]` config array with hybrid backwards compat for the legacy `[emitter]` block.
- Background dispatch: ingest stops at `Stored`; the per-emitter workers handle first attempts and all retries through one code path.
- One `EmitterWorker` per configured emitter with its own poll interval, concurrency cap, retry policy (exponential backoff with cap and jitter), and `max_attempts → dead_lettered` promotion.
- Per-emitter health reporting feeding `/readyz`.
- Per-emitter metric labels on existing emit counters and histograms; new gauges for DLQ depth, pending count, and in-flight count; new histogram for terminal-state attempt counts.
- New admin endpoints for per-delivery inspection, replay, and per-emitter DLQ listing; CLI flags to filter and target specific emitters.
- One-shot SQL migration that creates the table and backfills one immutable historical row per existing receipt.
- New `scenario-tests/` top-level workspace member (package `hookbox-scenarios`) holding all Cucumber BDD tests; the existing `crates/hookbox/tests/bdd.rs` and feature files are migrated into it and deleted from `hookbox`.
- Documentation updates: `README.md`, `CLAUDE.md`, `docs/ROADMAP.md`, examples, changelog, and the original MVP design doc gets a "Revision history" pointer to this spec.

### Explicitly out of scope (deferred to roadmap)

- Per-emitter routing filters: `providers = [...]` allowlist, `event_types = [...]` allowlist. Considered and rejected for V1 in favor of YAGNI; the schema is forward-compatible — adding filters later is purely additive.
- Distributed worker leader election / multi-instance coordination. The `FOR UPDATE SKIP LOCKED` claim mechanism is already compatible with multi-instance, so when this work happens it's a deployment change, not a code change.
- DLQ alerting hooks (webhook/email notifications on new dead-lettered deliveries).
- Bulk DLQ replay (`POST /api/dlq/bulk-replay`). The single-delivery replay endpoint is sufficient for V1.
- Per-emitter pause/resume admin endpoint.
- Schema-registry / Avro / Protobuf serialization formats.
- LISTEN/NOTIFY-based dispatch wake-up. Polling at 5-second intervals is sufficient for V1.
- Dropping the legacy `processing_state` column from `webhook_receipts`. Stays as a derived/historical field; will be cleaned up in a separate future migration.

## Crate impact map

| Crate | Change |
|---|---|
| `hookbox` (core) | New types (`DeliveryId`, `DeliveryState`, `WebhookDelivery`, `RetryPolicy`); pipeline simplified — drops Stage 5 emit and the `E` generic parameter; new `transitions::compute_backoff` and `transitions::receipt_aggregate_state` pure functions; existing `Emitter` trait unchanged. |
| `hookbox-postgres` | New migration `0002_create_webhook_deliveries.sql`; the existing `Storage::store` is supplemented by `Storage::store_with_deliveries` (atomic insert of receipt + N delivery rows in one transaction); new `DeliveryStorage` trait + `PostgresStorage` impl for the worker operations (`claim_pending`, `mark_*`, `count_*`, `insert_replay`, `get_delivery`); the existing `query_for_retry` is deleted along with its trait method. |
| `hookbox-server` | `EmitterConfig` (per-emitter struct on `HookboxConfig.emitter`) is replaced by `EmitterEntry`, surfaced as `HookboxConfig.emitters: Vec<EmitterEntry>`; `emitter_factory::build_emitter` becomes `build_workers` returning `Vec<EmitterWorker>`; the existing single `RetryWorker` is replaced by per-emitter `EmitterWorker`s; new admin routes; `/readyz` aggregation; pipeline construction loses its emitter argument. |
| `hookbox-cli` | `serve` spawns N workers instead of 1; existing receipt commands gain `--emitter` filters; `dlq inspect` and `dlq retry` arguments change from `receipt-id` to `delivery-id`; new `hookbox emitters list` command. |
| `hookbox-scenarios` (NEW, top-level) | Single new workspace member holding both core (in-memory) and server (testcontainer + HTTP) Cucumber BDD harnesses. Receives the migrated `crates/hookbox/tests/bdd.rs` content and adds new fan-out features. |
| `crates/hookbox/tests/` | The `bdd.rs` file and `features/` directory are deleted; the `cucumber` dev-dependency is dropped from `crates/hookbox/Cargo.toml`. |

## Data model

### Schema: `webhook_deliveries`

```sql
CREATE TABLE webhook_deliveries (
    delivery_id      UUID PRIMARY KEY,
    receipt_id       UUID NOT NULL REFERENCES webhook_receipts(receipt_id) ON DELETE CASCADE,
    emitter_name     TEXT NOT NULL,
    state            TEXT NOT NULL,         -- pending | in_flight | emitted | failed | dead_lettered
    attempt_count    INTEGER NOT NULL DEFAULT 0,
    last_error       TEXT,
    last_attempt_at  TIMESTAMPTZ,
    next_attempt_at  TIMESTAMPTZ NOT NULL,
    emitted_at       TIMESTAMPTZ,
    immutable        BOOLEAN NOT NULL DEFAULT FALSE,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Non-unique on (receipt_id, emitter_name): replays insert new rows for audit history.
CREATE INDEX idx_webhook_deliveries_receipt_emitter
    ON webhook_deliveries (receipt_id, emitter_name);

-- The hot worker query: "give me deliveries for emitter X that are ready to dispatch".
CREATE INDEX idx_webhook_deliveries_dispatch
    ON webhook_deliveries (emitter_name, next_attempt_at)
    WHERE state IN ('pending', 'failed') AND immutable = FALSE;

-- DLQ depth + listing.
CREATE INDEX idx_webhook_deliveries_dlq
    ON webhook_deliveries (emitter_name, state)
    WHERE state = 'dead_lettered';

-- Receipt-detail joins.
CREATE INDEX idx_webhook_deliveries_receipt
    ON webhook_deliveries (receipt_id);
```

The unique constraint on `(receipt_id, emitter_name)` is **deliberately omitted** so that replays insert a new delivery row for the same `(receipt, emitter)` pair instead of mutating the original. The dispatcher relies on the `state = 'pending'` filter plus `FOR UPDATE SKIP LOCKED` for serialization, not on uniqueness.

### Delivery state machine

```
pending  ──dispatch──>  in_flight  ──emit_ok──>   emitted   (terminal happy)
                           │
                           └──emit_err──>  failed
                                              │
                              attempt < max ──> stays failed
                                                 (next_attempt_at = now + backoff)
                                                 (re-claimed when next_attempt_at <= now)
                                              │
                              attempt >= max ──> dead_lettered (terminal sad)
```

The dispatcher reads both `pending` (first attempts, freshly inserted by ingest or replay) and `failed` (retries whose `next_attempt_at` has elapsed). The claim is one statement: a `FOR UPDATE SKIP LOCKED` CTE selects the eligible rows for this emitter and the outer UPDATE flips them to `in_flight` atomically:

```sql
WITH claimed AS (
    SELECT delivery_id
    FROM webhook_deliveries
    WHERE emitter_name = $1
      AND state IN ('pending', 'failed')
      AND next_attempt_at <= now()
      AND immutable = FALSE
    ORDER BY next_attempt_at
    LIMIT $2
    FOR UPDATE SKIP LOCKED
)
UPDATE webhook_deliveries d
SET state = 'in_flight',
    last_attempt_at = now()
FROM claimed
WHERE d.delivery_id = claimed.delivery_id
RETURNING d.*;
```

`pending` rows have `next_attempt_at = created_at` so they are immediately eligible; `failed` rows have `next_attempt_at` pushed into the future by the backoff calculation. The single index `idx_webhook_deliveries_dispatch` covers both states.

After emit, the worker updates back to `emitted` / `failed` / `dead_lettered`. There is no path from a terminal state back to `pending` *except* via the explicit replay endpoint, which inserts a new row rather than mutating the old.

### New types in `hookbox::state`

```rust
pub struct DeliveryId(pub Uuid);

pub enum DeliveryState {
    Pending,
    InFlight,
    Emitted,
    Failed,
    DeadLettered,
}

pub struct WebhookDelivery {
    pub delivery_id:      DeliveryId,
    pub receipt_id:       ReceiptId,
    pub emitter_name:     String,
    pub state:            DeliveryState,
    pub attempt_count:    i32,
    pub last_error:       Option<String>,
    pub last_attempt_at:  Option<DateTime<Utc>>,
    pub next_attempt_at:  DateTime<Utc>,
    pub emitted_at:       Option<DateTime<Utc>>,
    pub immutable:        bool,
    pub created_at:       DateTime<Utc>,
}
```

### Receipt's `processing_state` after this change

It is no longer the source of truth for emit state — it tops out at `Stored`. The four post-emit variants (`Emitted`, `EmitFailed`, `DeadLettered`, `Replayed`) become **derived** labels computed from the deliveries:

| Derived state | Rule |
|---|---|
| `Stored` | The receipt has zero non-immutable deliveries. |
| `Emitted` | Every non-immutable delivery is in `emitted`. |
| `EmitFailed` | At least one non-immutable delivery is in `failed` and none is in `dead_lettered`. |
| `DeadLettered` | At least one non-immutable delivery is in `dead_lettered`. |

The computation lives in one helper:

```rust
pub fn receipt_aggregate_state(deliveries: &[WebhookDelivery]) -> ProcessingState
```

in `hookbox::transitions`. No DB-side trigger, no denormalized cache, no double-write hazard. The admin API uses this helper when serializing receipts. Immutable rows are filtered out before computation; if all deliveries are immutable (i.e. a backfilled legacy receipt), the function falls back to reading the receipt's stored `processing_state`.

### Why no separate DLQ table

Per-emitter DLQ is just `SELECT … WHERE emitter_name = $1 AND state = 'dead_lettered'`. A separate table would duplicate data and create a sync invariant.

## Pipeline change

### Old flow

```
Receive → Verify → Dedupe → Store → Emit (inline await)
```

### New flow

```
Receive → Verify → Dedupe → Store + insert N pending deliveries (single txn) → return Accepted
```

Stage 5 (Emit) is **deleted from the pipeline**. The pipeline no longer holds an `Emitter` and is no longer generic over `E`. `HookboxPipeline<S, D, E>` becomes `HookboxPipeline<S, D>`. The builder loses `.emitter(...)`. Tests and bootstrap code that previously took `Emitter` lose the parameter.

### Atomic store + delivery rows

The store call becomes:

```rust
self.storage
    .store_with_deliveries(&receipt, &emitter_names)
    .await?;
```

Internally a single Postgres transaction:

1. `INSERT INTO webhook_receipts (...) VALUES (...)`
2. `INSERT INTO webhook_deliveries (delivery_id, receipt_id, emitter_name, state, next_attempt_at, ...) VALUES (...), (...), (...)` — one row per emitter, in one statement.

The transaction guarantees: either the receipt is durably stored *with* its delivery rows, or neither exists. There is no window where a `Stored` receipt has zero pending deliveries.

The pipeline learns the configured emitter names from a new builder field:

```rust
HookboxPipeline::builder()
    .storage(storage)
    .dedupe(dedupe)
    .emitter_names(vec!["kafka-billing".into(), "nats-audit".into()])
    .verifier(...)
    .build()
```

The pipeline doesn't hold the emitter *instances* — only their names. Instances live in the workers.

### Duplicate, verification-failed, and skipped paths

Unchanged from today. `StoreResult::Duplicate { existing_id }` short-circuits before any new delivery rows are written. Verification failures and skipped paths short-circuit before store. Duplicates are silently absorbed; their previously-stored deliveries already exist (and may already be processed).

### Replay

The existing `POST /api/receipts/:id/replay` endpoint inserts a new set of pending delivery rows. With no query parameter: one new row per currently-configured emitter (re-fanning out). With `?emitter=<name>`: one row for that emitter only. Returns `202 Accepted` with the IDs of the newly inserted delivery rows. The old rows are never mutated — each replay is a fresh attempt visible in the deliveries history.

## Worker model

### `EmitterWorker`

One per configured emitter, owning its loop, retry policy, concurrency cap, health state, and metric labels. Lives in `crates/hookbox-server/src/worker.rs` (replacing the existing `RetryWorker`).

```rust
pub struct EmitterWorker {
    name:          String,                              // "kafka-billing"
    emitter:       Arc<dyn Emitter + Send + Sync>,
    storage:       PostgresStorage,
    policy:        RetryPolicy,
    concurrency:   usize,
    poll_interval: Duration,
    health:        Arc<HealthState>,                    // shared with /readyz
}

pub struct RetryPolicy {
    pub max_attempts:       i32,           // default 5
    pub initial_backoff:    Duration,      // default 30s
    pub max_backoff:        Duration,      // default 1h
    pub backoff_multiplier: f64,           // default 2.0
    pub jitter:             f64,           // 0.0..=1.0, default 0.2
}
```

### Loop body

```text
loop {
    sleep(poll_interval).await
    rows = storage.claim_pending(emitter_name, batch_size = concurrency).await
    if rows.empty:
        health.mark_idle()
        continue
    join_all(rows.map(|row| dispatch_one(row)))
    health.mark_active()
}
```

`claim_pending` is the `FOR UPDATE SKIP LOCKED` Postgres pattern (defined on the new `DeliveryStorage` trait — see Section 8): each worker (and each loop iteration) atomically claims up to `concurrency` rows whose state is `pending` or `failed` and whose `next_attempt_at` has elapsed; concurrent workers skip locked rows and grab the next batch. This makes the design naturally compatible with deferred multi-instance leader election — when that work happens, multiple identical processes can run and the row-level locks coordinate them with no code change.

### `dispatch_one(row)`

1. Build a `NormalizedEvent` from the joined receipt row.
2. `emitter.emit(&event).await` (with the existing per-backend timeout).
3. On `Ok`:
   ```sql
   UPDATE webhook_deliveries
   SET state = 'emitted', emitted_at = now(), last_error = NULL
   WHERE delivery_id = $1
   ```
4. On `Err`:
   - `attempt_count += 1`
   - if `attempt_count >= policy.max_attempts`: state → `dead_lettered`, `last_error = $err`
   - else: compute `next_attempt_at = now() + min(initial_backoff * multiplier^attempt, max_backoff) * (1 + rand(-jitter, +jitter))`, state → `failed`
5. Update `HealthState`: success / failure rate windowed over the last N dispatches.

### Backoff math

Encapsulated in a pure function `transitions::compute_backoff(attempt: i32, policy: &RetryPolicy) -> Duration` so it is bolero-testable without DB or network. Same pattern as the existing transition helpers.

### `HealthState`

```rust
pub struct EmitterHealth {
    pub last_success_at:      Option<DateTime<Utc>>,
    pub last_failure_at:      Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    pub status:               HealthStatus,    // healthy | degraded | unhealthy
    pub dlq_depth:            u64,
    pub pending_count:        u64,
}
```

`status` rule (compile-time tunable, not config): `unhealthy` if `consecutive_failures >= 10`; `degraded` if last failure within the last 60s; otherwise `healthy`. `dlq_depth` and `pending_count` are written by the worker once per loop iteration so the `/readyz` handler can read them without a DB roundtrip.

The state struct is held inside `Arc<ArcSwap<EmitterHealth>>` so `/readyz` reads are lock-free.

### Time discipline

The worker uses `tokio::time::sleep` and `tokio::time::Instant::now()` for *scheduling* decisions (poll interval, backoff target). It uses `chrono::Utc::now()` only for the human-readable audit fields (`last_attempt_at`, `emitted_at`). This split lets `tokio::time::pause()` mock the scheduling clock during BDD tests without breaking the audit timestamps.

### Spawning and shutdown

`serve.rs` calls `build_workers(&config.emitters, &storage).await? -> Vec<EmitterWorker>`, then spawns each via a method that returns a `tokio::task::JoinHandle<()>`. Shutdown is cooperative: each worker watches a `tokio::sync::watch::Receiver<bool>` shutdown signal and finishes its current `join_all` batch before exiting. The graceful-shutdown path in `shutdown.rs` waits on all worker handles before returning.

### Defaults

- `concurrency = 1` per worker (matches the current `RetryWorker`; conservative; per-emitter knob)
- `poll_interval_seconds = 5` (down from the legacy 30s, since first attempts now go through the worker; per-emitter knob)
- `max_attempts = 5`, `initial_backoff = 30s`, `max_backoff = 1h`, `backoff_multiplier = 2.0`, `jitter = 0.2`

## Config schema

### New TOML shape

```toml
[database]
url = "postgres://..."

[[emitters]]
name = "kafka-billing"
type = "kafka"
poll_interval_seconds = 5
concurrency = 1

[emitters.kafka]
brokers = "broker1:9092,broker2:9092"
topic = "billing-events"
client_id = "hookbox-billing"
acks = "all"
timeout_ms = 5000

[emitters.retry]
max_attempts = 5
initial_backoff_seconds = 30
max_backoff_seconds = 3600
backoff_multiplier = 2.0
jitter = 0.2

[[emitters]]
name = "nats-audit"
type = "nats"
# poll_interval_seconds and concurrency omitted -> defaults

[emitters.nats]
url = "nats://localhost:4222"
subject = "hookbox.audit"

[emitters.retry]
max_attempts = 10
initial_backoff_seconds = 5
max_backoff_seconds = 600
```

### `EmitterEntry`

```rust
pub struct EmitterEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub emitter_type: String,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_seconds: u64,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    pub kafka: Option<KafkaEmitterConfig>,
    pub nats:  Option<NatsEmitterConfig>,
    pub sqs:   Option<SqsEmitterConfig>,
    pub redis: Option<RedisEmitterConfig>,
    #[serde(default)]
    pub retry: RetryPolicyConfig,
}

pub struct RetryPolicyConfig {
    pub max_attempts: i32,
    pub initial_backoff_seconds: u64,
    pub max_backoff_seconds: u64,
    pub backoff_multiplier: f64,
    pub jitter: f64,
}
```

The per-backend config structs (`KafkaEmitterConfig`, etc.) are unchanged from today.

### Validation at startup

Fail loudly, fail fast:

- `[[emitters]]` is empty → error: "at least one emitter must be configured"
- Two entries with the same `name` → error: "duplicate emitter name {name}"
- Entry with `type = "kafka"` but no `[emitters.kafka]` section → error
- `name` does not match `^[a-zA-Z0-9_-]{1,64}$` → error (this constraint exists specifically to keep the `emitter` Prometheus label clean)
- `retry.jitter` outside `[0.0, 1.0]` → error
- `retry.max_attempts < 1` → error
- `concurrency == 0` → error

### Hybrid backwards compatibility

```rust
pub struct HookboxConfig {
    // existing fields unchanged
    #[serde(default)]
    pub emitter:  Option<EmitterConfig>,    // legacy, deprecated
    #[serde(default)]
    pub emitters: Vec<EmitterEntry>,        // new
}
```

A `normalize()` step runs after parsing:

1. Both `emitter` and `emitters` present → error: "use either `[emitter]` (legacy) or `[[emitters]]` (preferred), not both".
2. Only `emitter` present → log a `WARN` deprecation and rewrite to a one-element `Vec<EmitterEntry>` with `name = "default"` and the legacy retry settings copied from the top-level `[retry]`.
3. `emitters` non-empty AND top-level `[retry]` present → log a `WARN`: "`[retry]` is now per-emitter under `[emitters.retry]`; the top-level block is ignored when `[[emitters]]` is used".
4. Both empty → error: "no emitters configured".

After normalization, the rest of the system only sees `Vec<EmitterEntry>`. The legacy types stay in `config.rs` for one or two releases, then get deleted.

### `hookbox config validate` CLI

Small new subcommand: load `hookbox.toml`, run normalization + validation, print either `OK: N emitters configured` or the first error. Exits non-zero on failure. Useful for ops smoke tests and CI checks. Doesn't touch the database.

## Health and metrics

### `/readyz`

```rust
pub struct ReadyzResponse {
    pub status:   HealthStatus,
    pub database: HealthCheck,
    pub emitters: BTreeMap<String, EmitterHealthSnapshot>,
}

pub struct EmitterHealthSnapshot {
    pub status:               HealthStatus,
    pub last_success_at:      Option<DateTime<Utc>>,
    pub last_failure_at:      Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    pub dlq_depth:            u64,
    pub pending_count:        u64,
}
```

**Aggregation:** `overall = unhealthy` iff database is unhealthy OR any emitter is `unhealthy`. `overall = degraded` iff at least one emitter is `degraded` and none is `unhealthy`. Otherwise `healthy`.

**HTTP status codes:** 200 for `healthy` and `degraded`, 503 for `unhealthy`. `degraded` returns 200 because `degraded` means "we noticed something but the system is still serving" — kicking pods on degraded would cause flapping under transient downstream blips. The body always carries the full snapshot so probes and humans see the detail regardless of status.

**No DB roundtrip in the handler.** Each `EmitterWorker` writes `dlq_depth` and `pending_count` into its `HealthState` once per loop iteration; the handler reads cached values. Worst-case staleness equals `poll_interval_seconds`, which is fine for a health probe.

`AppState` gains `BTreeMap<String, Arc<HealthState>>` populated at server bootstrap from the workers' health refs.

### Metric changes

| Metric | Before | After |
|---|---|---|
| `hookbox_emit_results_total{provider, result}` | per-provider | gains `emitter` label → `{provider, emitter, result}` |
| `hookbox_emit_duration_seconds` | unlabeled | gains `emitter` label |
| `hookbox_dlq_depth{emitter}` | did not exist | new gauge, written once per worker tick |
| `hookbox_emit_pending{emitter}` | did not exist | new gauge — deliveries in `pending` or `failed` ready to dispatch |
| `hookbox_emit_in_flight{emitter}` | did not exist | new gauge — deliveries currently claimed |
| `hookbox_emit_attempt_count` | did not exist | new histogram of `attempt_count` at terminal state |
| `hookbox_webhooks_received_total{provider}` | unchanged | unchanged |
| `hookbox_ingest_duration_seconds` | included emit time | drops emit time (free latency improvement on dashboards) |

**Cardinality.** `emitter` label has cardinality equal to the number of configured emitters per process (typically 1-5). `provider × emitter × result ≤ ~50` series for any realistic deployment. Safe.

**Breaking semantic change.** `hookbox_emit_results_total` previously fired exactly once per ingest. Going forward it fires once per delivery attempt — the count goes up by N per receipt and more on retries. This is the correct semantic but it breaks dashboards built against the old shape. The changelog must call this out.

## Admin API

### Existing endpoints — semantic changes

| Endpoint | Change |
|---|---|
| `GET /api/receipts` | Each receipt's `processing_state` is **derived** via `receipt_aggregate_state`. The serialized shape gains a `deliveries_summary` field: `{ "kafka-billing": "emitted", "nats-audit": "failed" }` so list views can render at-a-glance status without follow-up calls. |
| `GET /api/receipts/:id` | Same derivation. Adds an embedded `deliveries: [WebhookDelivery]` array sorted by `created_at`. |
| `POST /api/receipts/:id/replay` | Now accepts an optional `?emitter=<name>` query param. Without it: insert one new pending delivery row per currently-configured emitter. With it: insert one row for the named emitter only. Returns `202 Accepted` with the new delivery IDs. |
| `GET /api/dlq` | Now accepts `?emitter=<name>`. Without filter: returns all dead-lettered deliveries across all emitters joined to their receipts. The response shape becomes `[{ delivery: WebhookDelivery, receipt: WebhookReceipt }]` — matched pairs. |

### New endpoints

| Endpoint | Purpose |
|---|---|
| `GET /api/deliveries/:id` | Fetch one delivery row plus its receipt. |
| `POST /api/deliveries/:id/replay` | Insert a new pending delivery row for the same `(receipt_id, emitter_name)` as the target row; the original row is left untouched (audit history). Returns `202` with the new ID. |
| `GET /api/emitters` | List configured emitters with their current health snapshot — same shape as the `emitters` field in `/readyz` but a dedicated endpoint. |

### Auth

All admin endpoints (existing and new) keep the existing bearer-token check via `admin.bearer_token`. No new auth scopes.

### Not added

- `DELETE /api/deliveries/:id` — admins should not delete audit rows.
- `PATCH /api/deliveries/:id` — no in-place state mutation; replay is the only allowed transition.
- `POST /api/dlq/bulk-replay` — deferred.

### CLI surface

| Command | Behavior |
|---|---|
| `hookbox receipts inspect <id>` | Existing — now also prints `deliveries_summary` and the full deliveries list. |
| `hookbox dlq list [--emitter <name>]` | Existing — gains `--emitter` filter. |
| `hookbox dlq inspect <delivery-id>` | Existing — argument changes from `receipt-id` to `delivery-id`. **Breaking CLI change.** |
| `hookbox dlq retry <delivery-id>` | Existing — argument changes from `receipt-id` to `delivery-id`. **Breaking CLI change.** |
| `hookbox replay id <receipt-id> [--emitter <name>]` | Existing — gains `--emitter` filter, defaults to all. |
| `hookbox emitters list` | New — wraps `GET /api/emitters` (or queries the DB directly in DB mode). |
| `hookbox config validate` | New — loads the TOML, runs normalization + validation, exits non-zero on failure. |

## Migration

### `0002_create_webhook_deliveries.sql`

```sql
-- 1. Create the table.
CREATE TABLE webhook_deliveries (
    delivery_id      UUID PRIMARY KEY,
    receipt_id       UUID NOT NULL REFERENCES webhook_receipts(receipt_id) ON DELETE CASCADE,
    emitter_name     TEXT NOT NULL,
    state            TEXT NOT NULL,
    attempt_count    INTEGER NOT NULL DEFAULT 0,
    last_error       TEXT,
    last_attempt_at  TIMESTAMPTZ,
    next_attempt_at  TIMESTAMPTZ NOT NULL,
    emitted_at       TIMESTAMPTZ,
    immutable        BOOLEAN NOT NULL DEFAULT FALSE,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 2. Indexes (see Data Model section for the rationale).
CREATE INDEX idx_webhook_deliveries_receipt_emitter
    ON webhook_deliveries (receipt_id, emitter_name);
CREATE INDEX idx_webhook_deliveries_dispatch
    ON webhook_deliveries (emitter_name, next_attempt_at)
    WHERE state IN ('pending', 'failed') AND immutable = FALSE;
CREATE INDEX idx_webhook_deliveries_dlq
    ON webhook_deliveries (emitter_name, state)
    WHERE state = 'dead_lettered';
CREATE INDEX idx_webhook_deliveries_receipt
    ON webhook_deliveries (receipt_id);

-- 3. Backfill: one immutable row per existing receipt.
INSERT INTO webhook_deliveries
    (delivery_id, receipt_id, emitter_name, state, attempt_count,
     last_error, last_attempt_at, next_attempt_at, emitted_at,
     immutable, created_at)
SELECT
    gen_random_uuid(),
    receipt_id,
    'legacy',
    CASE processing_state
        WHEN 'emitted'       THEN 'emitted'
        WHEN 'processed'     THEN 'emitted'
        WHEN 'replayed'      THEN 'emitted'
        WHEN 'emit_failed'   THEN 'failed'
        WHEN 'dead_lettered' THEN 'dead_lettered'
        WHEN 'stored'        THEN 'emitted'
        ELSE 'emitted'
    END,
    COALESCE(emit_count, 0),
    last_error,
    processed_at,
    received_at,
    CASE WHEN processing_state IN ('emitted','processed','replayed')
         THEN processed_at END,
    TRUE,
    received_at
FROM webhook_receipts;
```

### Mapping rationale

| Legacy `processing_state` | Backfilled `state` | Why |
|---|---|---|
| `emitted` / `processed` / `replayed` | `emitted` | Successfully reached a downstream under the old single-emitter model. |
| `emit_failed` | `failed` | Was scheduled for retry under the old worker. Marked immutable so the new worker leaves it alone; operators can manually replay via `/api/deliveries/:id/replay` if they want it pushed through the new emitters. |
| `dead_lettered` | `dead_lettered` | Terminal sad state, immutable. |
| `stored` | `emitted` | **Crucial.** Pre-migration `Stored` receipts were never going to be retried under the old model (the old worker only picked up `EmitFailed`). Mapping them to `pending` would surprise-emit stale events post-migration. Mark them terminal-handled and move on. |
| `received` / `verified` / `verification_failed` / `duplicate` | `emitted` | These never reached the emit stage. Marked terminal-handled so the worker never picks them up. |

### Why immutable

The migration runs before the operator has had a chance to map their old single emitter to a new named emitter. We don't know what `'legacy'` is supposed to be. Immutable means "this is a historical record only; the worker leaves it alone forever, the admin UI shows it for context, that's it". Operators who want to push old `EmitFailed` rows through the new pipeline use the manual `/api/deliveries/:id/replay` endpoint, which inserts a new (mutable) delivery row against a real configured emitter and leaves the immutable row in place.

### Migration safety

- Single transaction (sqlx migrations are transactional by default).
- INSERT-only against a freshly-created table; no UPDATEs to `webhook_receipts`.
- Idempotent in practice: sqlx's migration tracking blocks re-runs.
- Cost: O(n) where n = existing receipt count. For 10M receipts the `INSERT ... SELECT` runs in seconds on modest hardware. Documented in the changelog: "expect a one-shot migration cost proportional to your receipt count; recommend a low-traffic window if you have >10M receipts".

### Legacy column retention

The `webhook_receipts.processing_state` column is **not** dropped in this migration. It stays as a fallback for `receipt_aggregate_state` when all of a receipt's deliveries are immutable (so the admin UI shows the historical state). It will be cleaned up in a separate future migration once it becomes load-bearing dead code.

### `query_for_retry` removal

The existing `PostgresStorage::query_for_retry` function and its trait method are **deleted** in this PR. Nothing calls it after the worker is reworked. The new dispatch query lives on a new `DeliveryStorage` trait:

- `claim_pending(emitter_name, batch_size) -> Vec<(WebhookDelivery, WebhookReceipt)>`
- `mark_emitted(delivery_id)`
- `mark_failed(delivery_id, attempt_count, next_attempt_at, last_error)`
- `mark_dead_lettered(delivery_id, last_error)`
- `count_dlq(emitter_name) -> u64`
- `count_pending(emitter_name) -> u64`
- `insert_replay(receipt_id, emitter_name) -> DeliveryId`
- `get_delivery(delivery_id) -> Option<(WebhookDelivery, WebhookReceipt)>`

## Testing strategy

### Tier 1 — Unit tests

| Crate | New unit tests |
|---|---|
| `hookbox` | `transitions::compute_backoff` (exponential growth, jitter bounds, max-cap clamping, zero-jitter determinism, `attempt = 0` returns `initial`); `transitions::receipt_aggregate_state` (table-driven over every state combination + the all-immutable fallback); `state::DeliveryState` round-trip serde; pipeline builder rejects `.emitter(...)` (compile-time); pipeline writes deliveries via the storage call (mock storage records the `emitter_names` arg). |
| `hookbox-postgres` | Storage trait tests against a testcontainer Postgres: `claim_pending` honors `FOR UPDATE SKIP LOCKED` (two concurrent claims see disjoint row sets); `mark_*` atomicity; `insert_replay` creates a fresh row leaving the original; `count_dlq` and `count_pending` correctness; immutable rows are skipped by `claim_pending`. |
| `hookbox-server` | `EmitterWorker::dispatch_one` with a fake `Emitter` and in-memory storage stub: success → `emitted`; failure → `failed` + correct backoff; failure at `attempt = max` → `dead_lettered`. Config normalization for every case enumerated above. `emitter_factory::build_workers` happy path + every validation arm without Docker (same pattern as PR #17). `routes::admin::list_dlq` with `?emitter=` filter against in-memory storage. `/readyz` aggregation: `degraded → 200`, `unhealthy → 503`, `healthy → 200`, all-emitters-healthy + db-down → 503. |
| `hookbox-cli` | `hookbox emitters list` parse + render via fake API responses. |

### Tier 2 — Integration tests (`integration-tests/`, real Postgres testcontainer)

| Test | Asserts |
|---|---|
| `test_fan_out_two_emitters` | Two channel emitters; one webhook; both receive it exactly once; receipt has 2 delivery rows. |
| `test_one_emitter_fails_other_succeeds` | One channel + one fake-failing; the channel delivery → `emitted`; the failing delivery cycles `failed → pending` until `dead_lettered`; receipt's derived state is `DeadLettered`. |
| `test_replay_one_emitter_only` | After the previous test, `POST /replay?emitter=channel` inserts a new pending row for `channel` only. |
| `test_per_delivery_replay` | After dead-letter, `POST /api/deliveries/:id/replay` creates a new row for the same `(receipt, emitter)`; the original row is unchanged. |
| `test_dlq_filter_by_emitter` | Three emitters, one webhook, two permanently fail; `GET /api/dlq` returns 2 rows; filtered queries return the right subsets. |
| `test_legacy_emitter_block_normalizes` | Boot with legacy `[emitter] type = "channel"`; server starts; deprecation warning logged; `/api/emitters` returns one entry named `default`; ingest works. |
| `test_migration_backfills_history` | Apply 0001 only; insert receipts in each pre-existing state; apply 0002; assert deliveries with `immutable = TRUE` and the right state mapping. |
| `test_concurrent_dispatch_no_double_emit` | One emitter, `concurrency = 4`, 100 pending deliveries; worker drains; channel receiver sees exactly 100 events, no duplicates, no rows in `in_flight`. Validates `FOR UPDATE SKIP LOCKED` under contention within a single worker. |

### Tier 3 — Property tests (bolero, `hookbox-verify`)

- `prop_backoff_monotonic_under_no_jitter` — for any `policy` with `jitter = 0`, `compute_backoff(n) <= compute_backoff(n+1)` until `max_backoff`, then clamped.
- `prop_backoff_within_jitter_bounds` — for any `policy` with `jitter > 0`, the result is within `[base * (1 - jitter), base * (1 + jitter)]`.
- `prop_aggregate_state_deterministic` — `receipt_aggregate_state` is total and pure on every `Vec<DeliveryState>` shape.
- `prop_state_machine_no_resurrection` — terminal states (`emitted`, `dead_lettered`) are never left for non-terminal states except via the explicit replay endpoint, which inserts a new row rather than mutating.

### Tier 4 — Kani proofs (nightly, `hookbox-verify`)

- `proof_backoff_no_overflow` — `compute_backoff` does not panic on integer overflow over the full `i32` attempt range and any reasonable policy.
- `proof_aggregate_state_total` — `receipt_aggregate_state` is total over all `Vec<DeliveryState>` shapes.

### Tier 5 — Fuzz targets (nightly)

- `fuzz_target_config_normalize` — feed arbitrary TOML bytes through the parser + normalization pipeline; assert no panics; either return a `Vec<EmitterEntry>` or a typed error. Lives in `crates/hookbox-server/fuzz/`.

### Tier 6 — Mutation testing (nightly, `cargo-mutants`)

The new code is automatically picked up by the existing run. High-value targets: `compute_backoff`, `receipt_aggregate_state`, the worker's claim/update sequence — all heavily unit-tested, expected mutation score in the same band as today (>80%).

### Tier 7 — Cucumber BDD scenarios

A new top-level workspace member `scenario-tests/` (package `hookbox-scenarios`, peer of `integration-tests/`) holds **all** Cucumber BDD tests for hookbox. The current `crates/hookbox/tests/bdd.rs` and feature files are migrated into it as part of this PR; the `cucumber` dev-dependency is dropped from `crates/hookbox/Cargo.toml`.

#### Crate layout

```text
scenario-tests/
├── Cargo.toml
├── src/
│   └── lib.rs                    # shared fixtures, fake emitters, gherkin step helpers
├── tests/
│   ├── core_bdd.rs               # entrypoint for in-memory pipeline scenarios
│   └── server_bdd.rs             # entrypoint for testcontainer-Postgres + HTTP scenarios
└── features/
    ├── core/
    │   ├── ingest.feature        # migrated from crates/hookbox/tests/features/
    │   ├── providers.feature     # migrated
    │   ├── retry.feature         # migrated
    │   ├── emitters.feature      # migrated, expanded with single-emitter fan-out parity
    │   ├── fanout.feature        # NEW
    │   ├── derived_state.feature # NEW
    │   └── backoff.feature       # NEW
    └── server/
        ├── fanout_durability.feature
        ├── migration.feature
        ├── replay.feature
        ├── runtime.feature
        └── cli_dlq.feature
```

#### Layer 1 — Core BDD

`tests/core_bdd.rs` runs Cucumber against `features/core/` with an `IngestWorld` that holds in-memory storage and fake emitters. Same shape as the current `crates/hookbox/tests/bdd.rs`, updated for the new `Vec<EmitterEntry>` shape and the dropped `E` generic on the pipeline. Fast, no infra; runs in the same CI tier as unit tests.

The migrated existing scenarios still pass without text changes because a new `Given the pipeline is configured with emitters "<comma-separated-names>"` step defaults to `["default"]` if not called explicitly. The `PipelineVariant` enum and `PipelineBox` indirection in the current `bdd.rs` are deleted (no longer needed without the `E` generic).

New core feature files cover: multi-emitter fan-out, duplicate ingest does not create new deliveries, derived receipt state from delivery combinations, immutable rows excluded from derivation, backoff math (deterministic at `jitter = 0`, table-driven across attempts).

#### Layer 2 — Server BDD

`tests/server_bdd.rs` runs Cucumber against `features/server/` with a `ServerWorld` that boots a real `hookbox-server` against a testcontainer Postgres with programmable fake emitters. Uses `tokio::time::pause()` for time advancement. Slow; gated behind `--features bdd-server`. Default `cargo test -p hookbox-scenarios` only runs the core suite — server BDD is opt-in or CI-only.

`ServerWorld` shape:

```rust
struct ServerWorld {
    pg:             PostgresContainer,
    server:         ServerHandle,
    fake_emitters:  BTreeMap<String, FakeEmitter>,
    http:           reqwest::Client,
    last_response:  Option<reqwest::Response>,
    last_receipt_id:  Option<Uuid>,
    last_delivery_id: Option<Uuid>,
}
```

`FakeEmitter` is a programmable test double implementing `Emitter` with `behavior: Arc<Mutex<EmitterBehavior>>` where `EmitterBehavior` is one of `Healthy`, `FailFor(Duration)`, `FailUntilAttempt(u32)`, `Slow(Duration)`. Steps mutate the behavior mid-scenario to simulate downstream recovery. The fake records every received `NormalizedEvent` for assertions.

Server scenario coverage:

| Scenario | Validates |
|---|---|
| **Flaky downstream eventually catches up** | Two emitters, one fails for 600 simulated seconds; 100 webhooks; advance time; both emitters receive 100; DLQ empty; no rows in `in_flight`. The "durable inbox heals itself" promise. |
| **Permanent downstream failure dead-letters its deliveries** | Two emitters, one permanently broken; 50 webhooks; advance past `max_attempts * max_backoff`; broken DLQ has 50; healthy emitter has 50 emitted; `/readyz` reports broken as `unhealthy` and overall as 503. The isolation + per-emitter DLQ promise. |
| **Legacy `[emitter]` block normalizes** | Boot with the legacy block; ingest 20 webhooks; deprecation warning logged; `/api/emitters` returns one entry named `default`; all deliveries on `default` in `emitted`. The hybrid backwards-compat promise. |
| **Mixed-state migration boots cleanly** | Pre-populate the database with receipts in every legacy state; run migration 0002; assert deliveries table has the right rows with `immutable = true`; the worker never picks up immutable rows. The Section 9 backfill mapping end-to-end. |
| **Replay one emitter only** | After the dead-letter scenario, `POST /api/receipts/:id/replay?emitter=healthy` inserts a new pending row for `healthy` only; the broken emitter still has only its dead-lettered row. |
| **Per-delivery replay leaves audit history** | After dead-letter, swap the broken fake to healthy; `POST /api/deliveries/:id/replay` creates a new row in `emitted` for the same `(receipt, emitter)`; the original `dead_lettered` row is unchanged. |
| **Graceful shutdown drains in-flight deliveries** | Slow fake (200ms/emit), `concurrency = 4`, 50 webhooks; SIGTERM after 100ms; every claimed `in_flight` completes before exit; no rows left in `in_flight`; ingest stops accepting new requests during shutdown. |
| **Background dispatch decouples ingest latency from emit** | Slow fake (50ms/emit), 600 webhooks at 20 RPS; ingest p99 latency stays under 50ms; eventual emit count is 600. The Question 2 promise. |
| **CLI bulk DLQ replay loop** | After the dead-letter scenario, fix the broken emitter, run `hookbox dlq retry` for every dead delivery in a shell loop, advance time; DLQ empty; 50 new rows in `emitted`; the 50 original rows unchanged. |

#### Migration fixture seeding

The `Given the database is preloaded with receipts in legacy states` step uses raw SQL inserts directly against the testcontainer's connection — the easiest way to materialize old-format rows without going through the (already-migrated) Rust code.

#### `Cargo.toml` for `scenario-tests/`

```toml
[package]
name    = "hookbox-scenarios"
version.workspace = true
edition.workspace = true
publish = false

[features]
default     = []
bdd-server  = ["dep:testcontainers", "dep:reqwest", "dep:hookbox-server", "dep:hookbox-postgres"]

[dependencies]
hookbox          = { workspace = true }
hookbox-server   = { workspace = true, optional = true }
hookbox-postgres = { workspace = true, optional = true }
async-trait.workspace = true
bytes.workspace      = true
chrono.workspace     = true
cucumber             = "0.21"
http.workspace       = true
serde_json.workspace = true
tokio                = { workspace = true, features = ["macros", "rt-multi-thread", "sync", "time", "test-util"] }
tracing.workspace    = true
uuid.workspace       = true
testcontainers       = { version = "*", optional = true }
reqwest              = { workspace = true, optional = true }

[[test]]
name    = "core_bdd"
harness = false

[[test]]
name              = "server_bdd"
harness           = false
required-features = ["bdd-server"]
```

#### CI jobs

```yaml
bdd-core:
  runs-on: ubuntu-latest
  steps: cargo test -p hookbox-scenarios --test core_bdd

bdd-server:
  runs-on: ubuntu-latest
  needs: [test-emitters]
  steps: cargo test -p hookbox-scenarios --test server_bdd --features bdd-server
```

`bdd-core` runs in parallel with the unit-test jobs (no infra). `bdd-server` runs after `test-emitters` since both pull testcontainers; sequencing them keeps Docker pressure manageable in CI.

### Coverage target

Stays at 85%. The new code is mostly straight-line worker logic and pure functions, both of which are easy to test exhaustively. Realistic estimate: this PR improves coverage rather than dragging it down.

### Out-of-scope tests

- LISTEN/NOTIFY (deferred, doesn't exist).
- Multi-process leader election (deferred, doesn't exist).
- Real Kafka/NATS/SQS/Redis fan-out behavior — the existing per-emitter testcontainer round-trip tests already cover those individually; combining them in fan-out doesn't change their semantics, only the orchestration.

## Documentation deliverables

- `README.md` — replace the single-emitter quickstart with a multi-emitter one; add a "migrating from `[emitter]` to `[[emitters]]`" section; update the Testing section to point at `scenario-tests/`.
- `CLAUDE.md` — update the "Background Worker" section (now N workers, per-emitter retry policies); update the "Ingest Pipeline" diagram (drop the "Emit" stage, replace with "store + insert deliveries"); add a brief note that emit timing is now decoupled from ingest ACK; add `cargo test -p hookbox-scenarios` to the Quick Reference.
- `docs/superpowers/specs/2026-04-10-hookbox-design.md` — append a "Revision history" entry pointing at this spec; do not rewrite the original.
- `docs/ROADMAP.md` — mark the five "Future emitter architecture improvements" bullets as completed; add new deferred bullets for per-emitter routing filters, DLQ alerting hooks, distributed worker leader election, and bulk DLQ replay.
- Examples shipped under `examples/` get updated to the new shape, with at least one example showing two emitters and per-emitter retry policies.
- Changelog (or release notes draft) calls out the breaking changes: `hookbox_emit_results_total` semantics, `hookbox dlq inspect/retry` argument shape, `[emitter]` deprecation warning, the pipeline generic param change for anyone embedding `HookboxPipeline` directly.

## Future work tracked in roadmap

When this PR lands, `docs/ROADMAP.md` is updated to add these deferred items as their own bullets so they aren't lost:

- **Per-emitter routing filters** — `providers = [...]` and `event_types = [...]` allowlists on `[[emitters]]` entries. The all-to-all model is fine for V1; filters become valuable when an operator wants to isolate downstreams (e.g., "billing-kafka only sees Stripe").
- **DLQ alerting hooks** — webhook/email notifications when a delivery enters `dead_lettered`. Pushes the operator-toil burden out of the system.
- **Distributed worker leader election** — multi-instance hookbox deployments where the workers coordinate via Postgres advisory locks. The `FOR UPDATE SKIP LOCKED` claim mechanism is already compatible; this is a deployment + locking change, not a worker rewrite.
- **Bulk DLQ replay** — `POST /api/dlq/bulk-replay` with throttling and partial-failure semantics. The single-delivery replay is sufficient for V1; bulk becomes useful at scale.
- **Per-emitter pause/resume** — `POST /api/emitters/:name/pause` so an operator can stop a misbehaving emitter without restarting the process.
- **LISTEN/NOTIFY-based wake-up** — replace polling with `LISTEN` channels for tighter dispatch latency. Polling at 5-second intervals is fine for V1; this becomes valuable if anyone runs hookbox at scale where 5s of dispatch latency matters.
