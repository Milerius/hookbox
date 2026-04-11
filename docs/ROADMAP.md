# Hookbox Roadmap

Tracking next steps after the MVP and MVP gaps milestones.

## Completed

- [x] **PR #9 — MVP Implementation**: Core pipeline, Postgres storage, Stripe + HMAC providers, Axum server, CLI serve command, 6 verification tiers
- [x] **PR #10 — MVP Gaps**: Prometheus metrics, 8 CLI subcommands, retry worker, generic AppState, HTTP route tests, documentation
- [x] **Provider adapter pack**: Adyen (HMAC-SHA256, hex key), BVNK new hook service (Base64 HMAC-SHA256), Triple-A fiat (RSA-SHA512), Triple-A crypto (HMAC-SHA256, timestamped), Walapay/Svix (HMAC-SHA256)

---

## Next: Quality and Robustness (Priority)

### 8. Extract shared transition functions
Move label derivation and state transition logic into shared helpers so Bolero/Kani tests exercise real production code instead of local tautologies. CodeRabbit flagged the current property tests as vacuous — this fixes it.

- Extract metric label derivation from `pipeline.rs` into `hookbox/src/labels.rs`
- Extract retry state transition logic into a pure function testable by Bolero and Kani
- Update property tests to call shared functions
- Update Kani proofs to model the shared transition function

### 9. Coverage improvement
Currently ~66% line coverage. Main gaps: Postgres storage ops queries, CLI command handlers, server admin routes under auth.

- Add direct Postgres tests for all 5 ops queries (query_for_retry, retry_failed, reset_for_retry, query_by_external_reference, query_failed_since)
- Add HTTP integration tests for admin auth flows
- Add CLI integration tests that exercise the full command → DB → output path
- Target: 80%+ line coverage

### 10. Load and stress testing
Concurrent webhook ingest at scale to find race conditions, dedupe contention, and performance bottlenecks.

- Benchmark: ingest throughput (requests/sec) with real Postgres
- Stress: concurrent duplicate submissions (dedupe under contention)
- Stress: retry worker under high EmitFailed volume
- Tool: criterion benchmarks or custom load generator
- Identify: connection pool sizing, lock contention, index performance

### 11. Graceful shutdown
Drain in-flight retries and emits before process exit. Currently the server and retry worker are killed immediately on SIGTERM.

- Handle SIGTERM/SIGINT via `tokio::signal`
- Stop accepting new webhooks
- Wait for in-flight `ingest()` calls to complete
- Stop retry worker loop
- Wait for in-flight retry emissions to complete
- Drain the `ChannelEmitter` receiver
- Close database pool

---

## Phase 2: Features

### 1. ~~Provider adapter pack~~ (done)
Completed: Adyen, BVNK new hook service, Triple-A fiat (RSA-SHA512), Triple-A crypto (HMAC-SHA256 timestamped), Walapay/Svix — all as feature flags in `hookbox-providers`.

Remaining from original scope:
- Checkout.com (HMAC-SHA256)
- PayPal (certificate-based verification)

### 2. Kafka / NATS / SQS emitter adapters
Replace the `ChannelEmitter` drain task with real message broker delivery.

- New crates: `hookbox-emitter-kafka`, `hookbox-emitter-nats`, `hookbox-emitter-sqs`
- Each implements the `Emitter` trait
- Config-driven selection in `hookbox.toml`
- Delivery guarantees documented per adapter

### 3. Replay UI
Web dashboard for inspecting receipts, replaying, and managing the DLQ.

- Served by `hookbox-server` at `/ui/`
- Receipt list with filters (provider, state, time range)
- Receipt detail view (headers, body, verification result, state history)
- One-click replay and DLQ retry
- DLQ depth chart
- Tech: simple HTMX or static SPA

### 4. CLI API mode (`--api-url`)
CLI talks to the admin HTTP API instead of direct database access.

- All commands accept `--api-url` as alternative to `--database-url`
- Uses `reqwest` to call `/api/receipts`, `/api/receipts/:id/replay`, `/api/dlq`
- Auth via `--token` flag or `HOOKBOX_TOKEN` env var
- Same output format as direct-DB mode

### 5. CLI `--output json`
Machine-readable JSON output for scripting and piping.

- `--output json` flag on all commands
- Default: human-readable (current tracing-based output)
- JSON mode: one JSON object per line (NDJSON) for list commands, single object for inspect
- Enables `hookbox receipts list | jq '.provider_name'` workflows

### 6. Advanced retry
Sophisticated retry policies beyond fixed interval.

- Exponential backoff (configurable base, cap, jitter)
- Per-provider retry policies (different config per provider name)
- Retry budget / circuit breaker (stop retrying when failure rate exceeds threshold)
- DLQ alerting hooks (webhook/email notification on new dead-lettered receipts)
- Manual DLQ requeue via admin API (not just CLI)
- `hookbox_dlq_depth` gauge metric emitted by worker on each cycle
- Worker health reporting to `/readyz`
- Distributed worker coordination (leader election for multi-instance)

### 7. Remaining metrics
Metrics not included in MVP gaps.

- `hookbox_dlq_depth` gauge — periodic query from worker
- `hookbox_inflight_count` gauge — atomic counter across concurrent ingest requests
- `hookbox_replay_total` counter — instrument replay operations
- Metrics endpoint response format validation tests

---

## Phase 3: Advanced

- Ordering / causality hints for webhook event sequencing
- Inbox + outbox pairing for bidirectional webhook management
- Audit export (compliance-ready event logs)
- Reconciliation plugin (match internal records against provider records)
- Receipt / delivery lifecycle split (separate state machines)
- Enriched emitter context (receipt metadata passed through emit)
- `VerifierRegistry` abstraction (dynamic provider registration)
- Multi-region active-active deployment support
