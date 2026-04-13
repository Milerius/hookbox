# Hookbox Roadmap

Tracking next steps after the MVP and MVP gaps milestones.

## Completed

- [x] **PR #9 — MVP Implementation**: Core pipeline, Postgres storage, Stripe + HMAC providers, Axum server, CLI serve command, 6 verification tiers
- [x] **PR #10 — MVP Gaps**: Prometheus metrics, 8 CLI subcommands, retry worker, generic AppState, HTTP route tests, documentation, graceful shutdown, shared transition functions, criterion benchmarks
- [x] **PR #12 — Provider adapter pack**: Adyen (HMAC-SHA256, hex key), BVNK new hook service (Base64 HMAC-SHA256), Triple-A fiat (RSA-SHA512), Triple-A crypto (HMAC-SHA256, timestamped), Walapay/Svix (HMAC-SHA256)
- [x] **PR #13 — Emitter adapters (V1)**: Kafka (rdkafka), NATS (async-nats), SQS (aws-sdk-sqs) — config-driven selection, Arc<dyn Emitter> shared ownership, Docker smoke tests, nightly CI
- [x] **Shared transition functions**: `transitions.rs` at 100% coverage — Bolero/Kani test real production code
- [x] **Graceful shutdown**: `shutdown.rs` — SIGTERM/SIGINT drain with tokio::signal
- [x] **Coverage 85%+**: Line coverage at 85.26% (target was 80%)
- [x] **Criterion benchmarks**: `benches/ingest.rs` for ingest throughput
- [x] **PR #14 — Redis Streams emitter + emitter test coverage**: `hookbox-emitter-redis` (XADD, optional MAXLEN, configurable timeout), round-trip integration tests for all four emitters (Kafka, NATS, SQS, Redis) using `testcontainers-rs`, dedicated Linux-only `test-emitters` CI job, legacy `emitter_smoke_test.rs` deleted
- [x] **PR #16 — `async-trait` policy clarified**: CLAUDE.md updated to explain why the four core extension traits keep `#[async_trait]` (native `async fn` in traits is not dyn-compatible in stable Rust, and the architecture relies on `Arc<dyn Emitter + Send + Sync>` for runtime emitter selection). Issue #15 closed `wontfix` with the full rationale.
- [x] **PR #17 — Emitter factory + selection coverage**: extracted the kafka/nats/sqs/redis/channel selection match out of `serve` into `hookbox_server::emitter_factory::build_emitter`, returning a `BuiltEmitter` enum. Six new unit tests cover every validation arm without Docker. `serve.rs` shrinks from ~75 lines of match to 9 lines and the four emitter crates moved from `hookbox-cli` deps into `hookbox-server` deps.
- [x] **PR #18 — Emitter fan-out**: receipt/delivery lifecycle split with `webhook_deliveries` table, `DeliveryId` newtype, per-delivery `DeliveryState`, and `[[emitters]]` array config. Each receipt fans out to every configured emitter as an independent delivery row with its own retry policy, concurrency, and lease. Legacy `[emitter]` section is still accepted and rewritten to a single `"default"` entry with a deprecation warning. Per-emitter retry policies (`initial_backoff_seconds`, `max_backoff_seconds`, `backoff_multiplier`, `jitter`) replace the global `[retry]` worker defaults on a per-emitter basis. Added `hookbox-scenarios` BDD crate with in-memory fan-out/backoff/derived-state scenarios and Kani proofs for `compute_backoff` and `receipt_aggregate_state`.

---

## Next: Immediate candidates

- **Stress testing under contention**: concurrent duplicate submissions, retry worker under high EmitFailed volume, connection pool sizing. Criterion benchmarks exist but no multi-client stress harness yet.
- **PayPal provider adapter**: certificate-based verification (X.509 + RSA-SHA256), the last remaining V1 provider. Checkout.com is already supported via the `"checkout"` provider type (HMAC-SHA256 with `Cko-Signature` header) wired in PR #12.

---

## Phase 2: Features

### 1. ~~Provider adapter pack~~ (done)
Completed: Adyen, BVNK new hook service, Triple-A fiat (RSA-SHA512), Triple-A crypto (HMAC-SHA256 timestamped), Walapay/Svix, Checkout.com (via `"checkout"` provider type, HMAC-SHA256 with `Cko-Signature` header) — all as feature flags in `hookbox-providers`.

Remaining from original scope:
- PayPal (certificate-based verification)

### 2. ~~Emitter adapters V1~~ (done)
Completed: Kafka (rdkafka), NATS (async-nats), SQS (aws-sdk-sqs) — config-driven selection, `Arc<dyn Emitter>` shared ownership, `delivery.timeout.ms`, `endpoint_url` for LocalStack, Docker smoke tests, nightly CI.

Remaining emitter work:

**V2 (done):**
- ~~`hookbox-emitter-redis`~~ — Redis Streams via XADD ✅

**Deferred:**
- `hookbox-emitter-rabbitmq` — lapin, AMQP publish to exchange (legacy compatibility, sharp-edged exchange pre-existence)
- `hookbox-emitter-pulsar` — Apache Pulsar producer (sophisticated users can wait)

**Rejected for emitter family:**
- gRPC — breaks the "emit JSON to broker" model, different contract shape (proto vs JSON), semantic mismatch. Better suited as a separate integration/transport mode.

**Tier 2 / future broker adapters:**
- Google Cloud Pub/Sub
- AWS SNS
- Azure Service Bus
- AWS EventBridge
- Azure Event Hubs (Kafka-compatible — covered by Kafka emitter)
- HTTP/Webhook relay (reqwest POST)
- AWS Kinesis

**Future emitter architecture improvements:**
- ~~Fan-out to multiple emitters (`[[emitters]]` array)~~ ✅ (PR #18)
- ~~Per-emitter retry policies~~ ✅ (PR #18)
- Per-emitter routing filters (event type, provider name, header predicates)
- DLQ alerting hooks (webhook/email notification when a delivery dead-letters)
- Distributed worker leader election (multi-instance retry coordination)
- Bulk DLQ replay (replay all dead-lettered deliveries for an emitter)
- Drop the legacy single-emitter `processing_state` fallback once no tenants rely on the deprecation warning
- Emitter health reporting to `/readyz`
- Emitter-level metrics
- Dead-letter per emitter

**Future serialization formats:**
- CloudEvents envelope
- Protobuf with schema registry
- Avro with schema registry
- MessagePack

**Future Kafka enhancements:**
- Schema Registry (Confluent/Redpanda)
- Transactions / exactly-once
- Custom partitioner

**Future NATS enhancements:**
- JetStream for guaranteed delivery
- NATS KV for state

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
