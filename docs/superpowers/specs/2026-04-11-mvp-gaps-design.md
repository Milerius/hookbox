# Hookbox MVP Gaps — Design Specification

Close the remaining MVP gaps: Prometheus metrics recording, CLI subcommands, server HTTP tests, and a minimal retry/DLQ background worker.

## Scope

Four subsystems, all independent, shipped in one branch:

1. **Prometheus metrics** — instrument the pipeline, expose `/metrics`
2. **CLI subcommands** — 8 missing commands (receipts, replay, dlq)
3. **Server HTTP tests** — in-memory route tests + full-stack HTTP tests
4. **Minimal retry worker** — periodic retry of EmitFailed receipts

---

## 1. Prometheus Metrics

### What Gets Recorded

Metrics are recorded inside `HookboxPipeline::ingest()` alongside existing tracing spans. Uses the `metrics` crate macros (`counter!`, `histogram!`).

| Metric | Type | Labels | Where recorded |
|--------|------|--------|---------------|
| `hookbox_webhooks_received_total` | Counter | `provider` | Top of `ingest()` |
| `hookbox_ingest_results_total` | Counter | `provider`, `result` | At each return point (accepted/duplicate/verification_failed/store_failed) |
| `hookbox_verification_results_total` | Counter | `provider`, `status`, `reason` | After verify step |
| `hookbox_dedupe_checks_total` | Counter | `provider`, `result` | After dedupe step |
| `hookbox_emit_results_total` | Counter | `provider`, `result` | After emit step |
| `hookbox_ingest_duration_seconds` | Histogram | — | Wrap entire `ingest()` |
| `hookbox_store_duration_seconds` | Histogram | — | Wrap `storage.store()` call |
| `hookbox_emit_duration_seconds` | Histogram | — | Wrap `emitter.emit()` call |

**Label cardinality discipline:** Only `provider`, `status`, `reason`, `result` as metric labels. Never `receipt_id`, `external_reference`, or `dedupe_key` — those belong in logs/traces only.

### Where Exposed

New `GET /metrics` route in `hookbox-server` returning Prometheus text format. The `metrics-exporter-prometheus` crate provides a `PrometheusBuilder` that installs a global recorder and returns a `PrometheusHandle` for rendering.

**Initialization:** In `serve.rs`, before building the pipeline:
1. `PrometheusBuilder::new().install_recorder()` — returns `PrometheusHandle`
2. Store the handle in `AppState`
3. `/metrics` route calls `handle.render()` and returns the text

### Files Changed

- `crates/hookbox/src/pipeline.rs` — add `metrics::counter!` and `metrics::histogram!` calls
- `crates/hookbox-server/src/lib.rs` — add `PrometheusHandle` to `AppState`
- `crates/hookbox-server/src/routes/health.rs` — add `metrics()` handler
- `crates/hookbox-cli/src/commands/serve.rs` — initialize Prometheus recorder

---

## 2. CLI Subcommands

### Architecture

All CLI commands connect directly to Postgres via `--database-url` flag or `DATABASE_URL` env var. No running server required.

### Commands

```
hookbox serve --config hookbox.toml

hookbox receipts list    --database-url <url> --provider <name> --state <state> --limit <n>
hookbox receipts inspect --database-url <url> <receipt_id>
hookbox receipts search  --database-url <url> --ref <external_reference>

hookbox replay           --database-url <url> <receipt_id>
hookbox replay failed    --database-url <url> --provider <name> --since <duration>

hookbox dlq list         --database-url <url> --provider <name> --limit <n>
hookbox dlq inspect      --database-url <url> <receipt_id>
hookbox dlq retry        --database-url <url> <receipt_id>
```

### Implementation

Each subcommand:
1. Connects to Postgres via `PgPoolOptions`
2. Runs migrations via `PostgresStorage::migrate()`
3. Calls `storage.query()` or `storage.get()` with appropriate filters
4. Prints results to stdout as formatted text (human-readable)

### Replay and DLQ Retry in Direct-DB Mode

The CLI is an ops tool for inspection and state management. In direct-DB mode:
- `hookbox replay <id>` — sets state back to `Stored` so the running server's retry worker picks it up
- `hookbox dlq retry <id>` — sets state from `DeadLettered` back to `EmitFailed` with `emit_count` reset to 0, so the retry worker retries it

Actual re-emission is the server's job. The CLI manages state transitions.

### Files Created

- `crates/hookbox-cli/src/commands/receipts.rs` — list, inspect, search
- `crates/hookbox-cli/src/commands/replay.rs` — replay single, replay failed
- `crates/hookbox-cli/src/commands/dlq.rs` — list, inspect, retry
- `crates/hookbox-cli/src/db.rs` — shared database connection helper

### Files Modified

- `crates/hookbox-cli/src/main.rs` — add subcommand variants to `Commands` enum
- `crates/hookbox-cli/src/commands/mod.rs` — add module declarations
- `crates/hookbox-cli/Cargo.toml` — add `hookbox-postgres` dependency (already present)

### Storage Additions

Need new query capabilities in `PostgresStorage`:
- `query_by_external_reference(ref: &str)` — for `receipts search --ref`
- `query_failed_since(provider: Option<&str>, since: DateTime<Utc>)` — for `replay failed --since`
- `reset_for_retry(id: Uuid)` — sets state to `EmitFailed`, `emit_count` to 0 (for `dlq retry`)

These are added as methods on `PostgresStorage` directly, not on the core `Storage` trait (they are ops-specific queries, not core pipeline operations).

---

## 3. Server HTTP Tests

### Layer A: In-memory Route Tests

**Location:** `crates/hookbox-server/src/routes/tests.rs` (or per-module test files)

**Approach:** Make `AppState` and `build_router` generic over the pipeline type parameters. The concrete type alias stays for `serve.rs`. Tests use `MemoryStorage` + `InMemoryRecentDedupe` + `ChannelEmitter`.

**Refactor:** Change `AppState` from concrete types to generic:
```rust
pub struct AppState<S, D, E> {
    pub pipeline: HookboxPipeline<S, D, E>,
    pub pool: Option<PgPool>,  // None in tests
    pub admin_token: Option<String>,
    pub prometheus: Option<PrometheusHandle>,
}
```

Or simpler: use a type alias for the concrete version and keep tests in `integration-tests/` with real types. **Decision: make AppState generic** — it's the cleanest approach and the generics are already on `HookboxPipeline`.

**Test coverage targets:**

| Route | Tests |
|-------|-------|
| `POST /webhooks/:provider` | accepted (200), duplicate (200), verification_failed (401), pipeline error (500) |
| `GET /healthz` | always 200 |
| `GET /api/receipts` | 200 with results, empty results, filter params |
| `GET /api/receipts/:id` | 200 found, 404 not found |
| `POST /api/receipts/:id/replay` | 200 replayed, 404 not found |
| `GET /api/dlq` | 200 with DeadLettered filter |
| Admin auth | 401 missing token, 401 wrong token, 200 correct token, 200 no token configured |

### Layer B: Full-stack HTTP Tests

**Location:** `integration-tests/tests/http_test.rs`

**Approach:** Start a real Axum server on port 0 (OS-assigned) with real Postgres, send HTTP requests via `reqwest`.

**Tests:**
- Full ingest → list receipts → inspect → replay flow over HTTP
- Deduplication visible in receipts list
- DLQ endpoint returns filtered results
- Metrics endpoint returns Prometheus text format

### Files Created

- `crates/hookbox-server/src/routes/tests.rs` — in-memory route tests
- `integration-tests/tests/http_test.rs` — full-stack HTTP tests

### Files Modified

- `crates/hookbox-server/src/lib.rs` — make `AppState` and `build_router` generic
- `crates/hookbox-server/Cargo.toml` — add `reqwest` or `hyper` as dev-dependency
- `integration-tests/Cargo.toml` — add `reqwest` dependency

---

## 4. Minimal Retry Worker

### Behavior

A background `tokio::spawn` task that runs a periodic retry loop:

1. Sleep for `interval_seconds`
2. Query `webhook_receipts` for `processing_state = 'emit_failed'` AND `emit_count < max_attempts`
3. For each receipt:
   - Build `NormalizedEvent` from receipt
   - Call `emitter.emit(&event)`
   - On success → `update_state(id, Emitted, None)` + increment `emit_count`
   - On failure → increment `emit_count`. If `emit_count >= max_attempts` → `update_state(id, DeadLettered, error_msg)`
4. Loop back to step 1

### Configuration

```toml
[retry]
interval_seconds = 30
max_attempts = 5
```

Add `RetryConfig` to `hookbox-server/src/config.rs`:
```rust
#[derive(Debug, Deserialize)]
pub struct RetryConfig {
    #[serde(default = "default_retry_interval")]
    pub interval_seconds: u64,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: i32,
}
```
Defaults: 30 seconds, 5 attempts.

### State Transitions

```
EmitFailed (emit_count < max) → retry → success → Emitted
EmitFailed (emit_count < max) → retry → failure → EmitFailed (emit_count += 1)
EmitFailed (emit_count >= max) → DeadLettered
```

### Storage Requirements

New method on `PostgresStorage` (not the core trait):
```rust
pub async fn query_for_retry(&self, max_attempts: i32) -> Result<Vec<WebhookReceipt>, StorageError>
```
Queries: `SELECT * FROM webhook_receipts WHERE processing_state = 'emit_failed' AND emit_count < $1 ORDER BY received_at ASC LIMIT 100`

Also need:
```rust
pub async fn increment_emit_count(&self, id: Uuid) -> Result<(), StorageError>
```
Updates: `UPDATE webhook_receipts SET emit_count = emit_count + 1 WHERE receipt_id = $1`

### Files Created

- `crates/hookbox-server/src/worker.rs` — `RetryWorker` with `spawn()` method

### Files Modified

- `crates/hookbox-server/src/config.rs` — add `RetryConfig`
- `crates/hookbox-server/src/lib.rs` — add retry config to worker setup
- `crates/hookbox-cli/src/commands/serve.rs` — spawn the worker alongside drain task
- `crates/hookbox-postgres/src/storage.rs` — add `query_for_retry()` and `increment_emit_count()`

---

## Future Work

Everything below is explicitly out of scope for this pass but tracked for follow-up.

### Metrics — Future

- `hookbox_dlq_depth` gauge — requires periodic query from worker or storage
- `hookbox_inflight_count` gauge — requires atomic tracking across concurrent ingest requests
- `hookbox_replay_total` counter — add when replay operations get dedicated instrumentation

### CLI — Future

- `--api-url` mode for CLI commands via HTTP admin API instead of direct DB
- `--output json` flag for machine-readable CLI output (piping, scripting)
- `receipts list --reason <verification_reason>` filter for debugging provider config issues
- Bulk operations beyond `replay failed` (e.g. `dlq retry-all --provider`)

### Testing — Future

- Load/stress testing (concurrent webhook ingest at scale)
- Concurrent ingest tests (race conditions, dedupe under contention)
- Rate limiting tests (when rate limiting is implemented)
- Metrics endpoint response format validation (parse Prometheus text, assert metric names/labels)
- Webhook provider end-to-end tests (real Stripe test mode webhooks)

### Retry Worker — Future

- Exponential backoff instead of fixed interval
- Per-provider retry policies (different retry behavior per provider)
- Retry budget / circuit breaker (stop retrying when failure rate is too high)
- DLQ alerting hooks (notify on new dead-lettered receipts)
- Manual DLQ requeue via admin API (currently CLI only sets state in DB)
- `hookbox_dlq_depth` gauge metric emitted by worker on each cycle
- Worker health reporting to `/readyz` (worker liveness as readiness signal)
- Graceful shutdown (drain in-flight retries before process exit)
- Distributed worker coordination (leader election for multi-instance deployments)

---

## Non-Goals (Same as Original Spec)

- Not a ledger, not a workflow engine, not a reconciliation engine
- No Kafka/NATS/SQS emitters (phase 2)
- No UI dashboard
- No multi-region active-active
