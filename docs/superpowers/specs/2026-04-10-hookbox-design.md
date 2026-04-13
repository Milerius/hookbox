# Hookbox — Design Specification

**A Rust webhook inbox for payment systems: verify, dedupe, persist, replay, and audit every event before business logic touches it.**

## Problem

Payment providers and banking/crypto integrations send duplicate webhooks, delayed callbacks, out-of-order events, inconsistent statuses, and responses that require follow-up API fetches. Most teams rebuild the same infrastructure: HMAC verification, deduplication, durable storage, replay, dead-letter handling, and auditability. This work is painful, error-prone, and not core secret sauce — making it a strong candidate for open-source infrastructure.

## Solution

Hookbox is an embeddable Rust library (with an optional standalone service) that receives provider webhooks, verifies signatures, deduplicates by configurable key, stores payloads durably, and only then forwards normalized events for business processing. It creates a clean separation between receipt correctness and business processing.

## Positioning

Stripe-grade webhook ingest, but designed for payment and fintech realities: dedupe, signatures, durable inbox, replay, and auditability.

## Non-Goals

Hookbox is not:

- **Not a ledger** — it stores webhook receipts, not financial balances or transactions.
- **Not a workflow engine** — it handles receipt and forwarding, not multi-step payment orchestration.
- **Not a reconciliation engine** — it does not match internal records against provider records.
- **Not a general payment gateway** — it does not initiate payments or talk to provider APIs.
- **Not a queue replacement** — it is a durable inbox that feeds into queues/handlers, not a queue itself.

---

## Core Design Principles

1. **Receive first, process second** — ACK the provider only after durable write succeeds.
2. **Idempotent by default** — dedupe before any business processing.
3. **Raw payload immutable** — never modify the original webhook body after storage.
4. **Replayable everything** — any receipt can be re-processed at any time.
5. **Provider-agnostic core** — traits in core, implementations in extension crates.
6. **Observable by default** — structured logs and metrics at every pipeline stage.

---

## Deployment Model

Library-first with an optional standalone binary.

- **Embedded**: teams add `hookbox` as a dependency, wire the pipeline into their existing Axum app, bring their own storage and emitter implementations.
- **Standalone**: teams run the `hookbox` binary in server mode (`hookbox serve`), which uses `hookbox-server` internally for the Axum service wiring, Postgres storage, and configurable providers.

---

## Architecture

### Ingest Pipeline

Every webhook passes through five sequential stages:

```
Provider HTTP POST
    │
    ▼
┌─────────────────────────────────────────────┐
│  hookbox boundary                           │
│                                             │
│  1. Receive — accept raw payload,           │
│               assign receipt_id             │
│       │                                     │
│       ▼                                     │
│  2. Verify — SignatureVerifier dispatches    │
│              to provider adapter            │
│       │  failure → store as unverified,     │
│       │            emit metric, stop        │
│       ▼                                     │
│  3. Dedupe — DedupeStrategy checks for      │
│              duplicate (LRU fast path,      │
│              Postgres authoritative)        │
│       │  duplicate → mark, store, stop      │
│       ▼                                     │
│  4. Store — Storage trait persists raw       │
│             payload + normalized envelope   │
│       │  ACK 200 only AFTER this succeeds   │
│       ▼                                     │
│  5. Emit — Emitter trait forwards           │
│            normalized event downstream      │
│       │  failure → mark failed, retry/DLQ   │
│       │  (does NOT invalidate acceptance)   │
└─────────────────────────────────────────────┘
    │
    ▼
Downstream consumer (callback / channel / Kafka phase 2)
```

### Hard Invariants

- **ACK only after durable store**: the HTTP 200 response to the provider is sent only after the receipt is durably persisted. This is non-negotiable.
- **Dedupe is a two-layer concern**: an optional fast-path strategy (LRU) may reject obvious recent duplicates before storage, but authoritative duplicate detection is finalized at durable store time via the Postgres unique constraint on `dedupe_key`. The fast path is advisory; the store is truth.
- **Emit failure does not invalidate acceptance**: the receipt is durable and accepted. Emission has its own independent retry/DLQ lifecycle.
- **`payload_hash` always computed**: SHA-256 of the raw body bytes, stored on every receipt for integrity checks and debugging.
- **Raw body bytes are immutable**: the original HTTP body is stored as raw bytes/text and never modified after initial storage. This preserves byte-for-byte fidelity for signature re-verification during replay.

---

## Workspace Structure

```
hookbox/
├── crates/
│   ├── hookbox/              # core domain library
│   ├── hookbox-postgres/     # Postgres storage backend
│   ├── hookbox-providers/    # provider verifiers + helpers
│   ├── hookbox-server/       # standalone Axum server
│   └── hookbox-cli/          # replay / inspect / ops CLI
├── integration-tests/
├── examples/
├── docs/
├── Cargo.toml                # workspace root
└── README.md
```

### Crate Responsibilities

**`hookbox` (core)**
- Domain types: `WebhookReceipt`, `NormalizedEvent`, `ProcessingState`, `VerificationStatus`
- Traits: `SignatureVerifier`, `Storage`, `DedupeStrategy`, `Emitter`
- Pipeline orchestration: `HookboxPipeline`
- Lightweight built-in implementations: `InMemoryLruDedupe`, `CallbackEmitter`, `ChannelEmitter`, `LayeredDedupe`
- Error model
- Tracing-friendly context types
- No heavy dependencies (no Axum, no SQLx, no provider-specific crypto)

**`hookbox-postgres`**
- `PostgresStorage` implementing the `Storage` trait
- `StorageDedupe` implementing the `DedupeStrategy` trait (queries Postgres for advisory dedupe checks)
- SQLx-based queries and connection management
- Database migrations
- Schema definition

**`hookbox-providers`**
- `StripeVerifier` — Stripe webhook signature verification with timestamp tolerance
- `BvnkVerifier` — BVNK-style HMAC verification
- `GenericHmacVerifier` — configurable HMAC-SHA256/SHA512 verifier (header name, signing key, encoding)
- Feature-gated internally: `stripe`, `bvnk`, `generic-hmac`

**`hookbox-server`**
- Axum HTTP server with webhook ingest endpoint
- Configuration loading (TOML)
- Wiring of core + storage + providers
- Health, readiness, and metrics endpoints
- Admin API for receipts and DLQ

**`hookbox-cli`**
- Produces the `hookbox` binary — the single entry point for all CLI and server operations
- Receipt inspection and search commands
- Replay commands (single and filtered batch)
- DLQ list/inspect/retry commands
- Server launch command (`hookbox serve`) — depends on `hookbox-server` for the actual Axum wiring

---

## Technology Stack

- **Language**: Rust
- **Async runtime**: Tokio
- **HTTP framework**: Axum (with Tower middleware)
- **Database**: PostgreSQL via SQLx
- **Logging**: `tracing` + `tracing-subscriber`
- **Metrics**: `metrics` crate with Prometheus exporter
- **CLI**: `clap`
- **Serialization**: `serde` + `serde_json`
- **License**: MIT OR Apache-2.0

---

## Data Model

### WebhookReceipt (central entity)

| Field | Type | Purpose |
|-------|------|---------|
| `receipt_id` | `Uuid` | Internal unique ID, assigned on receive |
| `provider_name` | `String` | e.g. "stripe", "bvnk", "adyen" |
| `provider_event_id` | `Option<String>` | Provider's own event ID (e.g. evt_1234) |
| `external_reference` | `Option<String>` | Business reference (payment ID, order ID) |
| `dedupe_key` | `String` | Computed key for deduplication (unique constraint) |
| `payload_hash` | `String` | SHA-256 fingerprint of raw body bytes |
| `raw_body` | `Vec<u8>` / `TEXT` | Immutable original HTTP body bytes (preserves byte-for-byte fidelity) |
| `parsed_payload` | `Option<JsonValue>` | Parsed JSON projection of raw_body (convenience, not authoritative) |
| `raw_headers` | `JsonValue` | Original HTTP headers (needed for replay verification) |
| `normalized_event_type` | `Option<String>` | Canonical event type (e.g. "payment.completed") |
| `verification_status` | `VerificationStatus` | Verified / Failed / Skipped |
| `verification_reason` | `Option<String>` | Why verification passed/failed (e.g. "timestamp_expired") |
| `processing_state` | `ProcessingState` | Current lifecycle state |
| `emit_count` | `i32` | Number of emission attempts |
| `last_error` | `Option<String>` | Last error from emit or processing failure |
| `received_at` | `DateTime<Utc>` | When hookbox received this webhook |
| `processed_at` | `Option<DateTime<Utc>>` | When downstream emission succeeded |
| `metadata` | `JsonValue` | Extensible key-value metadata |

### ProcessingState (lifecycle)

```
Received
    │
    ▼
Verified ──────► VerificationFailed
    │
    ▼
Stored ────────► Duplicate
    │
    ▼
Emitted
    │
    ▼
Processed

Emitted ───(fail)──► EmitFailed ───(retries exhausted)──► DeadLettered

DeadLettered ───(manual)──► Replayed
```

`Stored` is the durable acceptance boundary; all subsequent states describe downstream delivery progress. This distinction matters: retries, DLQ handling, and ops tooling should treat `Stored` as the point where the webhook is safely received, regardless of what happens downstream.

**Evolution path**: in a future version, `processing_state` may split into two independent lifecycles — a receipt lifecycle (received/verified/duplicate/stored) and a delivery lifecycle (emit_pending/emitted/emit_failed/dead_lettered/replayed) — for more precise retry and DLQ handling.

### VerificationStatus

- **Verified** — signature valid, timestamp within tolerance
- **Failed** — signature invalid or verification error
- **Skipped** — no verifier configured for this provider

`verification_reason` provides machine-readable detail: `"signature_valid"`, `"signature_mismatch"`, `"timestamp_expired"`, `"missing_signature_header"`, `"unsupported_algorithm"`, `"no_verifier_configured"`.

---

## Core Traits

### SignatureVerifier

```rust
pub trait SignatureVerifier: Send + Sync {
    fn provider_name(&self) -> &str;

    async fn verify(
        &self,
        headers: &HeaderMap,
        body: &[u8],
    ) -> VerificationResult;
}

pub struct VerificationResult {
    pub status: VerificationStatus,
    pub reason: Option<String>,
}
```

### Storage

```rust
pub trait Storage: Send + Sync {
    async fn store(&self, receipt: &WebhookReceipt) -> Result<StoreResult, StorageError>;
    async fn get(&self, id: Uuid) -> Result<Option<WebhookReceipt>, StorageError>;
    async fn update_state(
        &self,
        id: Uuid,
        state: ProcessingState,
        error: Option<&str>,
    ) -> Result<(), StorageError>;
    async fn query(&self, filter: ReceiptFilter) -> Result<Vec<WebhookReceipt>, StorageError>;
}

pub enum StoreResult {
    Stored,
    Duplicate { existing_id: Uuid },
}
```

`Storage::store()` returns `StoreResult` rather than a plain `()` so the pipeline can handle duplicates without encoding that logic outside storage. This is the authoritative dedupe answer — the Postgres unique constraint on `dedupe_key` decides truth.

Note: `exists_by_dedupe_key()` is intentionally not in the core trait. The `StorageDedupe` adapter in `hookbox-postgres` can query its own backing store directly for advisory fast-path checks, but that is an implementation detail of the adapter, not a core storage contract.

### DedupeStrategy

```rust
pub trait DedupeStrategy: Send + Sync {
    async fn check(
        &self,
        dedupe_key: &str,
        payload_hash: &str,
    ) -> Result<DedupeDecision, DedupeError>;

    async fn record(
        &self,
        dedupe_key: &str,
        payload_hash: &str,
    ) -> Result<(), DedupeError>;
}

pub enum DedupeDecision {
    New,
    Duplicate,
    Conflict,  // same dedupe_key, different payload_hash
}
```

`DedupeStrategy::check()` takes both `dedupe_key` and `payload_hash` because there is an operationally important difference between a true duplicate (same key + same payload) and a conflict (same key + different payload). Conflicts should be flagged distinctly.

`LayeredDedupe<F, S>` composes a fast strategy (LRU) with an authoritative strategy (storage-backed), checking the fast path first and falling through to authoritative on miss.

**Relationship between `DedupeStrategy` and `Storage`**: `DedupeStrategy` is advisory — the pipeline asks it for a hint before attempting storage. `Storage::store()` returning `StoreResult::Duplicate` is the authoritative final answer, backed by the Postgres unique constraint. The pipeline flow is: ask dedupe strategy → if `New`, attempt store → trust `StoreResult` as truth. This means even if the fast-path dedupe misses a duplicate (LRU eviction, cold start), the store layer catches it. The `StorageDedupe` adapter in `hookbox-postgres` may query its own store for advisory checks, but this is an internal optimization — not a core trait method.

### Emitter

```rust
pub trait Emitter: Send + Sync {
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError>;
}

pub struct NormalizedEvent {
    pub receipt_id: Uuid,
    pub provider_name: String,
    pub event_type: Option<String>,
    pub external_reference: Option<String>,
    pub parsed_payload: Option<JsonValue>,
    pub payload_hash: String,
    pub received_at: DateTime<Utc>,
    pub metadata: JsonValue,
}
```

`NormalizedEvent.parsed_payload` is the JSON projection of the original webhook body — a convenience field for downstream consumers who want structured access. It is `Option` because some webhook bodies may not be valid JSON. The authoritative original body (`raw_body`) is not included in the emitted event to avoid passing large byte buffers through the emit path; consumers who need raw bytes can retrieve the full receipt from storage by `receipt_id`.

**Evolution path**: `emit()` may take a richer context object in a future version (including receipt ID, emit attempt count, provider event ID, verification metadata) to support advanced retry and DLQ logic.

### Pipeline

```rust
pub struct HookboxPipeline<S, D, E>
where
    S: Storage,
    D: DedupeStrategy,
    E: Emitter,
{
    storage: S,
    dedupe: D,
    emitter: E,
    verifiers: HashMap<String, Box<dyn SignatureVerifier>>,
}

impl<S, D, E> HookboxPipeline<S, D, E> {
    pub async fn ingest(
        &self,
        provider: &str,
        headers: HeaderMap,
        body: Bytes,
    ) -> Result<IngestResult, IngestError>;
}

pub enum IngestResult {
    Accepted { receipt_id: Uuid },
    Duplicate { existing_id: Uuid },
    VerificationFailed { reason: String },
}
```

`HookboxPipeline` is constructed via a builder pattern. `ingest()` returns `Accepted` only after durable store succeeds. Emit runs after accept — its failure is handled asynchronously via state transitions and retry/DLQ.

### Embedded Usage Example

```rust
let storage = PostgresStorage::new(pool).await?;
let dedupe = LayeredDedupe::new(
    InMemoryLruDedupe::new(10_000),
    StorageDedupe::new(storage.clone()),
);
let emitter = CallbackEmitter::new(|event| async move {
    my_business_logic(event).await
});

let pipeline = HookboxPipeline::builder()
    .storage(storage)
    .dedupe(dedupe)
    .emitter(emitter)
    .verifier(StripeVerifier::new(stripe_secret))
    .verifier(GenericHmacVerifier::new("bvnk", config))
    .build();

let app = your_router
    .route("/webhooks/:provider", post(hookbox::axum::handler(pipeline)));
```

---

## Observability

### Structured Logging

Every ingest is wrapped in a parent `tracing` span with `receipt_id`. Each pipeline step emits structured span data:

- `receipt_id`, `provider`, `provider_event_id`, `dedupe_key`
- `verification_status`, `verification_reason`
- `processing_state`, `is_duplicate`, `payload_hash`
- `emit_result`, `duration_ms`

JSON output for production, pretty output for development.

### Prometheus Metrics

**Counters:**
- `hookbox_webhooks_received_total` — labels: `provider`
- `hookbox_ingest_results_total` — labels: `provider`, `result` (accepted/duplicate/verification_failed/store_failed)
- `hookbox_verification_results_total` — labels: `provider`, `status`, `reason`
- `hookbox_dedupe_checks_total` — labels: `provider`, `result` (new/duplicate/conflict)
- `hookbox_emit_results_total` — labels: `provider`, `result` (success/failure)
- `hookbox_replay_total` — labels: `provider`

**Histograms:**
- `hookbox_ingest_duration_seconds`
- `hookbox_store_duration_seconds`
- `hookbox_emit_duration_seconds`

**Gauges:**
- `hookbox_dlq_depth` — labels: `provider`
- `hookbox_inflight_count`

**Label cardinality discipline**: only `provider`, `status`, `reason`, `result` as metric labels. Never `receipt_id`, `external_reference`, or `dedupe_key` — those belong in logs/traces only.

### Health Endpoints

- `GET /healthz` — liveness probe
- `GET /readyz` — readiness probe (DB connected for MVP; evolution: migration state, emitter readiness, worker health)
- `GET /metrics` — Prometheus scrape endpoint

---

## Server Endpoints

### Webhook Ingest

- `POST /webhooks/:provider` — main ingest endpoint

### Admin API (MVP)

- `GET /api/receipts` — list/filter receipts
- `GET /api/receipts/:id` — inspect one receipt
- `POST /api/receipts/:id/replay` — re-emit a receipt
- `GET /api/dlq` — list dead-lettered receipts

---

## CLI Commands

### Inspection

```
hookbox receipts list --provider stripe --state failed
hookbox receipts inspect <receipt_id>
hookbox receipts search --ref "pay_abc123"
```

### Replay

```
hookbox replay <receipt_id>
hookbox replay failed --provider stripe --since 1h
```

### Dead Letter Queue

```
hookbox dlq list --provider stripe
hookbox dlq inspect <receipt_id>
hookbox dlq retry <receipt_id>
```

### Server

```
hookbox serve --config hookbox.toml
```

CLI and admin API mirror each other — same operations available through both interfaces.

**Evolution path**: `--reason` filter for debugging provider config issues (e.g. `hookbox receipts list --reason timestamp_expired`).

---

## Security Considerations

- **Constant-time signature comparison** — all signature verification must use constant-time comparison to prevent timing attacks.
- **Timestamp tolerance / replay window** — verifiers that support timestamps (e.g. Stripe) must enforce a configurable tolerance window (default: 5 minutes) to reject replayed old webhooks.
- **Body size limits** — the ingest endpoint must enforce a maximum body size (configurable, default: 1 MB) to prevent resource exhaustion.
- **Header normalization** — headers used for verification must be handled case-insensitively per HTTP spec.
- **Secrets handling** — signing secrets must never appear in logs, metrics, or error messages. Support rotation by allowing multiple active secrets per provider.
- **Admin API authentication** — the admin API (`/api/*`) must require authentication. MVP: a shared bearer token from config. Evolution: pluggable auth middleware.
- **PII retention** — webhook payloads may contain PII. Document that operators are responsible for retention policies. Provide a configurable TTL for receipt cleanup as a future feature.
- **Replay restrictions** — replay operations should be gated behind admin API auth and logged as audit events.

---

## Database Schema Notes

### Key Indexes and Constraints

- **Unique index** on `dedupe_key` — authoritative duplicate detection
- **Index** on `(provider_name, provider_event_id)` — lookup by provider event
- **Index** on `external_reference` — lookup by business reference
- **Index** on `processing_state` — filter by lifecycle state (DLQ queries, failed receipt scans)
- **Index** on `received_at` — time-range queries, retention cleanup
- **Index** on `(provider_name, processing_state)` — common compound query (e.g. "all failed Stripe receipts")

The exact SQL schema is an implementation detail for `hookbox-postgres`, but these indexes are architecturally required for the query patterns the admin API and CLI depend on.

---

## MVP Scope

### In scope

- HTTP webhook listener (Axum)
- Provider signature verification interface + 3 implementations (Stripe, BVNK, generic HMAC)
- Durable inbox persistence (PostgreSQL)
- Configurable dedupe layer (LRU + Postgres)
- Canonical normalized event envelope
- Pluggable emitter (callback + channel implementations)
- Replay/redrive CLI
- Dead-letter support
- Per-event lifecycle states
- Searchable by external reference / provider event ID
- Structured logging + Prometheus metrics
- Health/readiness endpoints
- Builder pattern for pipeline construction
- Embedded library usage with Axum handler helper

### Out of scope for MVP

- Full UI dashboard
- Kafka / NATS / SQS emitter adapters
- Complete workflow orchestration
- Reconciliation engine
- Multi-region active-active
- Alerting hooks and policies
- Bulk replay with rich filtering
- Advanced DLQ lifecycle management
- SLA/SLO reporting
- Receipt/delivery lifecycle split (documented evolution path)
- Richer emitter context object (documented evolution path)
- `/readyz` extended health checks (documented evolution path)

---

## Phase 2 Ideas

- Provider adapter pack (Adyen, Checkout.com, etc.)
- Kafka / NATS / SQS emitter adapters
- Replay UI
- Ordering / causality hints
- Inbox + outbox pairing
- Alerting rules
- Audit export
- Reconciliation plugin
- Simulator integration (chaos sandbox for webhook failure patterns)
- Receipt/delivery lifecycle split
- `VerifierRegistry` abstraction
- Enriched emitter context

---

## Project Metadata

- **Name**: hookbox
- **License**: MIT OR Apache-2.0
- **Language**: Rust
- **Crate family**: hookbox, hookbox-postgres, hookbox-providers, hookbox-server, hookbox-cli, hookbox-verify, hookbox-scenarios, hookbox-emitter-{kafka,nats,sqs,redis}

---

## Revision History

| Date       | Spec                                                                  | Summary                                                                                                                                    |
|------------|-----------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------|
| 2026-04-10 | (this document)                                                       | Initial MVP design: single-emitter pipeline, Postgres storage, core traits, five-stage ingest flow.                                        |
| 2026-04-11 | [emitter-adapters](2026-04-11-emitter-adapters-design.md)             | Kafka / NATS / SQS emitter adapters with config-driven selection and `Arc<dyn Emitter>` shared ownership.                                  |
| 2026-04-11 | [mvp-gaps](2026-04-11-mvp-gaps-design.md)                             | Prometheus metrics, retry worker, CLI subcommands, graceful shutdown, shared transition functions.                                         |
| 2026-04-12 | [redis-emitter-and-emitter-test-coverage](2026-04-12-redis-emitter-and-emitter-test-coverage-design.md) | Redis Streams emitter (XADD) and round-trip testcontainers coverage for all four production emitters.                                     |
| 2026-04-12 | [emitter-fan-out](2026-04-12-emitter-fan-out-design.md)               | Receipt/delivery lifecycle split: `[[emitters]]` array config, `webhook_deliveries` table, per-delivery retry, derived `receipt.state`.    |

The current on-disk pipeline shape reflects the **emitter-fan-out** revision:
receipts are persisted once and fan out to one or more independently retried
delivery rows. Read this document for the original core architecture, then
read the later specs in order for the incremental changes that supersede the
single-emitter assumptions made here (notably around `ProcessingState` and the
`Emitter` trait's position in the pipeline).
