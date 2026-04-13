<h1 align="center">Hookbox</h1>

<p align="center">
  A durable webhook inbox for payment systems.<br>
  <b>Verify · Dedupe · Persist · Replay · Audit</b> — every event, before business logic touches it.
</p>

<p align="center">
  <a href="https://github.com/Milerius/hookbox/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/Milerius/hookbox/ci.yml?style=flat-square&logo=github&label=CI&branch=main" alt="CI"></a>
  <a href="https://github.com/Milerius/hookbox/actions/workflows/nightly.yml"><img src="https://img.shields.io/github/actions/workflow/status/Milerius/hookbox/nightly.yml?style=flat-square&logo=github&label=nightly" alt="Nightly"></a>
  <a href="https://codecov.io/gh/Milerius/hookbox"><img src="https://img.shields.io/codecov/c/github/Milerius/hookbox?style=flat-square&logo=codecov&label=coverage" alt="Coverage"></a>
  <img src="https://img.shields.io/badge/rust-1.85%2B-93450a?style=flat-square&logo=rust" alt="Rust 1.85+">
  <img src="https://img.shields.io/badge/edition-2024-blue?style=flat-square&logo=rust" alt="Rust edition 2024">
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue?style=flat-square" alt="License: MIT OR Apache-2.0"></a>
</p>

<p align="center">
  <a href="https://github.com/Milerius/hookbox/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/Milerius/hookbox/ci.yml?style=for-the-badge&label=tests&branch=main" alt="Tests"></a>
  <a href="https://codecov.io/gh/Milerius/hookbox"><img src="https://img.shields.io/codecov/c/github/Milerius/hookbox?style=for-the-badge&label=coverage" alt="Coverage"></a>
  <a href="https://github.com/Milerius/hookbox/actions/workflows/nightly.yml"><img src="https://img.shields.io/github/actions/workflow/status/Milerius/hookbox/nightly.yml?style=for-the-badge&label=kani%20%2B%20fuzz%20%2B%20mutants" alt="Nightly Verification"></a>
</p>

<p align="center">
  <b>Atomic fan-out · Per-emitter retry workers · Postgres-authoritative dedupe · Immutable raw bodies · Replayable forever</b>
</p>

---

## Why Hookbox?

Webhook delivery is messy by nature. Payment, banking, and crypto providers send duplicate webhooks, delayed callbacks, out-of-order events, inconsistent statuses, and retries at inconvenient times. Most teams end up rebuilding the same fragile boundary in every service.

Hookbox is that boundary, written once.

|                              | Hookbox                                          | Hand-rolled in your service                  |
|------------------------------|--------------------------------------------------|----------------------------------------------|
| **Signature verification**   | Pluggable trait, 7 built-in adapters             | Per-provider crypto code, drift over time    |
| **Deduplication**            | LRU fast path + Postgres unique constraint       | "Probably won't happen twice" 🤞              |
| **Receipt durability**       | ACK only after durable write                     | ACK then store, lose on crash                |
| **Fan-out to N consumers**   | Atomic per-receipt delivery rows, one transaction| Best-effort dual writes, partial failures    |
| **Replay**                   | Raw body preserved immutably, replay any receipt | "Re-trigger the webhook from the dashboard"  |
| **Per-consumer retry**       | Independent worker per `[[emitters]]` entry      | Shared queue, head-of-line blocking          |
| **Dead-letter queue**        | Per emitter, inspectable + retryable via CLI     | Logs and a runbook                            |
| **Observability**            | Tracing + Prometheus at every stage              | Whatever you remembered to add               |
| **Verification rigor**       | Property tests + Kani proofs + fuzzing nightly   | Unit tests for the happy path                |

---

## Architecture

Every incoming webhook passes through a four-stage ingest pipeline and is then fanned out to one background worker per configured emitter.

```text
                     ┌──────────────────────────────────────┐
                     │            Provider webhook           │
                     └──────────────────┬───────────────────┘
                                        │
              ┌─────────────────────────▼──────────────────────────┐
              │                                                    │
              │   1. Receive  → assign ReceiptId                   │
              │   2. Verify   → SignatureVerifier (per provider)   │
              │   3. Dedupe   → LRU fast path + Postgres unique    │
              │   4. Store    → receipt + 1 delivery row /emitter  │
              │                 in a single transaction            │
              │                                                    │
              │   ✓ ACK provider only after durable commit         │
              └─────────────────────────┬──────────────────────────┘
                                        │
                  ┌─────────────────────┼─────────────────────┐
                  │                     │                     │
        ┌─────────▼────────┐ ┌──────────▼─────────┐ ┌────────▼─────────┐
        │  EmitterWorker   │ │   EmitterWorker    │ │  EmitterWorker   │
        │   (kafka)        │ │     (sqs)          │ │     (nats)       │
        │                  │ │                    │ │                  │
        │ claim → dispatch │ │ claim → dispatch   │ │ claim → dispatch │
        │ retry  / DLQ     │ │ retry / DLQ        │ │ retry / DLQ      │
        └──────────────────┘ └────────────────────┘ └──────────────────┘
```

**A few important rules:**

- **verification failure** → receipt is stored, but never forwarded
- **duplicate receipt** → stored and marked, never processed twice
- **fan-out is atomic** → the receipt and every pending delivery row commit in one transaction; no partial ACKs
- **emit failure** → only the affected delivery is retried / DLQ-ed; the receipt stays accepted and sibling emitters are unaffected
- **raw body is preserved immutably** → replay and re-verification stay possible forever

---

## Highlights

🔐 **Pluggable signature verification** — Stripe, BVNK, Adyen, Triple-A (fiat RSA + crypto HMAC), Walapay/Svix, Checkout.com, and a generic HMAC fallback

🧱 **Authoritative dedupe** — LRU advisory layer plus a Postgres unique constraint as the source of truth; impossible to double-process by design

📦 **Atomic fan-out** — one receipt + one delivery row per `[[emitters]]` entry committed in a single transaction; no partial ACKs

🔁 **Per-emitter retry workers** — each emitter runs its own background worker with its own policy (max attempts, exponential backoff, jitter)

💀 **Per-emitter DLQ** — exhausted attempts land in that emitter's DLQ only; healthy emitters keep flowing

🎬 **Replayable forever** — raw body bytes preserved immutably; re-emit any receipt through CLI or admin API

📡 **Production emitter adapters** — Kafka (rdkafka), NATS (async-nats), AWS SQS, Redis Streams (XADD)

📊 **Observability by default** — structured tracing, Prometheus metrics, `/healthz` + `/readyz`, per-emitter health surfaced via `ArcSwap` snapshots

🛠️ **CLI tooling** — inspect receipts, search by external reference, replay failures, manage the DLQ

🧪 **Verification rigor** — unit · integration (12 testcontainer suites) · smoke · BDD scenarios (Cucumber, core + server) · property tests (Bolero, 8 modules) · Kani proofs · 4 fuzz targets · criterion benchmarks · mutation testing · `cargo careful` · `cargo deny`

---

## Crates

| Crate | Purpose |
|---|---|
| [`hookbox`](crates/hookbox/) | Core: traits (`SignatureVerifier`, `Storage`, `DedupeStrategy`, `Emitter`), pipeline, types, lightweight in-memory impls |
| [`hookbox-postgres`](crates/hookbox-postgres/) | PostgreSQL `Storage` + `DeliveryStorage` backend, migrations, retry/replay queries |
| [`hookbox-providers`](crates/hookbox-providers/) | Signature verifiers — Stripe, BVNK, Adyen, Triple-A, Walapay/Svix, Checkout.com, generic HMAC |
| [`hookbox-server`](crates/hookbox-server/) | Standalone Axum HTTP server: ingest router, admin API, retry workers, graceful shutdown |
| [`hookbox-cli`](crates/hookbox-cli/) | CLI binary: `serve`, `receipts`, `replay`, `dlq` |
| [`hookbox-emitter-kafka`](crates/hookbox-emitter-kafka/) | Kafka emitter adapter (rdkafka) |
| [`hookbox-emitter-nats`](crates/hookbox-emitter-nats/) | NATS emitter adapter (async-nats) |
| [`hookbox-emitter-sqs`](crates/hookbox-emitter-sqs/) | AWS SQS emitter adapter |
| [`hookbox-emitter-redis`](crates/hookbox-emitter-redis/) | Redis Streams emitter adapter (XADD) |
| [`hookbox-verify`](crates/hookbox-verify/) | Bolero property tests + Kani proofs for pipeline invariants |

---

## Quick Start

```bash
# Build everything
cargo build --all-features

# Run the test suite
cargo test --all-features

# Lint with pedantic clippy
cargo clippy --all-targets --all-features -- -D warnings

# HTML coverage report
cargo llvm-cov --all-features --html

# Run the server
hookbox serve --config hookbox.toml
```

A minimal fan-out configuration with two downstream emitters:

```toml
[database]
url = "postgres://hookbox:hookbox@localhost:5432/hookbox"

[server]
host = "0.0.0.0"
port = 8080

[providers.stripe]
type   = "stripe"
secret = "whsec_..."

[[emitters]]
name        = "kafka-billing"
type        = "kafka"
concurrency = 4

[emitters.retry]
max_attempts            = 8
initial_backoff_seconds = 30
max_backoff_seconds     = 3600
backoff_multiplier      = 2.0
jitter                  = 0.1

[emitters.kafka]
brokers = "localhost:9092"
topic   = "billing.webhooks"
acks    = "all"

[[emitters]]
name        = "sqs-audit"
type        = "sqs"
concurrency = 2

[emitters.sqs]
queue_url = "https://sqs.us-east-1.amazonaws.com/123456789012/audit"
region    = "us-east-1"
```

Each receipt is stored once and fanned out to every configured emitter as an independent delivery row. Emitters run concurrently and retry on their own schedule — a failing Kafka broker does not block SQS delivery, and vice versa.

---

<details>
<summary><h2>📚 CLI Reference</h2></summary>

```bash
# Run server
hookbox serve --config hookbox.toml

# Inspect receipts
hookbox receipts list    --database-url <url> --provider stripe --state failed
hookbox receipts inspect --database-url <url> <receipt_id>
hookbox receipts search  --database-url <url> --external-ref pay_123

# Replay
hookbox replay id     --database-url <url> <receipt_id>
hookbox replay failed --database-url <url> --since 1h --provider stripe

# Dead letter queue
hookbox dlq list    --database-url <url> --provider stripe
hookbox dlq inspect --database-url <url> <receipt_id>
hookbox dlq retry   --database-url <url> <receipt_id>
```

All commands except `serve` accept `--database-url` or the `DATABASE_URL` environment variable for direct database access. The `serve` command uses `--config` with a TOML file instead.

</details>

<details>
<summary><h2>📦 Embedded vs Standalone</h2></summary>

Hookbox is designed to work in two modes:

- **Embedded library** for teams that want to integrate it into an existing Rust/Axum service. Depend on `hookbox` (core) plus the storage / provider / emitter crates you actually need, and wire the pipeline into your own router.
- **Standalone service** for teams that want a dedicated webhook-ingestion boundary. Run the `hookbox` binary against a TOML config and let it own the ingest router, retry workers, and admin API.

Both modes share the exact same pipeline code path. There is no "lite" version.

</details>

<details>
<summary><h2>🔁 Migrating from the legacy <code>[emitter]</code> section</h2></summary>

The pre-fan-out single-emitter shape is still accepted and automatically rewritten to a `"default"` entry, but emits a deprecation warning. Migrate by wrapping your existing block in `[[emitters]]` and giving it a name:

```toml
# before
[emitter]
type = "kafka"
[emitter.kafka]
brokers = "localhost:9092"
topic   = "events"

# after
[[emitters]]
name = "kafka-primary"
type = "kafka"
[emitters.kafka]
brokers = "localhost:9092"
topic   = "events"
```

</details>

<details>
<summary><h2>🧪 Verification Tiers</h2></summary>

| Tier | Tool                                | Cadence       | What it catches                                                       |
|------|-------------------------------------|---------------|------------------------------------------------------------------------|
| 1    | Unit tests                          | Every PR      | Per-module behavior, co-located with source                            |
| 2    | Integration tests (Postgres)        | Every PR      | 12 testcontainer suites: ingest, fan-out, admin API, CLI, migrations, fault injection |
| 3    | Smoke test (`serve_smoke`)          | Every PR      | End-to-end `hookbox serve` boot + ingest + emit on a real port         |
| 4    | BDD scenarios (Cucumber)            | Every PR      | Product behavior in two modes — core in-memory and server testcontainer |
| 5    | Property tests (Bolero, 8 modules)  | Every PR      | Pipeline invariants: dedupe, retry, backoff, hash, state, metrics, providers, emitter |
| 6    | Criterion benchmarks                | Local / opt-in | Ingest path throughput + regression detection                         |
| 7    | `cargo deny`                        | Every PR      | License + advisory + dependency hygiene                                |
| 8    | `cargo careful`                     | Every PR      | Extra UB detection beyond standard tests                               |
| 9    | Coverage (`cargo llvm-cov`)         | Every PR      | Branch + line coverage tracked via Codecov                             |
| 10   | Kani proofs                         | Nightly       | Bounded model checking of state-machine invariants                     |
| 11   | Fuzz targets (4 total)              | Nightly       | Crash bugs in payload hashing, signature verification, receipt serde, config normalization |
| 12   | Mutation testing (`cargo-mutants`)  | Nightly       | Test-suite quality regression detection                                |

Every PR runs: fmt → clippy (pedantic, `-D warnings`) → test → integration → smoke → BDD (core + server) → property → coverage → deny → careful → doc.

</details>

<details>
<summary><h2>🏛️ Design Principles</h2></summary>

1. **Receive first, process second** — ACK the provider only after durable write succeeds.
2. **Idempotent by default** — dedupe before business processing, with Postgres as the authoritative source of truth.
3. **Raw body immutable** — preserve original bytes exactly for replay and signature verification.
4. **Replayable everything** — any receipt can be re-processed later, through CLI or admin API.
5. **Provider-agnostic core** — traits live in the core crate; implementations live in extension crates.
6. **Observable by default** — every pipeline stage emits logs and metrics.
7. **Typed errors at trait boundaries** — `thiserror` enums where consumers might `match`; `anyhow` only at the leaf application.

See [CLAUDE.md](CLAUDE.md) for the full development guide.

</details>

<details>
<summary><h2>🎯 Who this is for</h2></summary>

Hookbox is useful for teams building or operating:

- payment processors
- payout systems
- treasury / wallet infrastructure
- banking integrations
- crypto-to-fiat rails
- webhook-heavy financial platforms

If your system depends on external callbacks but correctness, replayability, and auditability matter, Hookbox is designed for that problem.

</details>

<details>
<summary><h2>🚫 What Hookbox is <i>not</i></h2></summary>

Hookbox is a **durable webhook inbox**, not a full payment platform. It sits at the boundary between external webhook traffic and your internal systems.

Hookbox does **not** replace:

- **Your ledger** — Hookbox stores webhook receipts, not balances or journal entries.
- **Your payment orchestration engine** — no multi-step workflows, no business state machines for payouts.
- **Your reconciliation system** — Hookbox does not compare internal records against provider statements.
- **Your queue or event bus** — Hookbox can emit downstream events but is not a general-purpose broker.
- **Your provider SDKs** — Hookbox does not create payments, submit payouts, or fetch balances.
- **Your compliance stack** — no KYC, AML, sanctions screening, or case management.

What Hookbox *does* replace is the repetitive, fragile webhook edge logic that teams otherwise rebuild in every service.

</details>

<details>
<summary><h2>📋 Project Status</h2></summary>

Early development. The full design specification lives in [`docs/superpowers/specs/2026-04-10-hookbox-design.md`](docs/superpowers/specs/2026-04-10-hookbox-design.md).

**Done:**
- 4-stage ingest pipeline with atomic fan-out
- 7 provider verifiers (Stripe, BVNK, Adyen, Triple-A, Walapay/Svix, Checkout.com, generic HMAC)
- Postgres `Storage` + `DeliveryStorage` with retry / replay queries
- Per-emitter retry workers with worker supervision and `/readyz` health flips on panic
- Kafka, NATS, SQS, Redis Streams emitter adapters
- CLI: `serve`, `receipts list/inspect/search`, `replay id/failed`, `dlq list/inspect/retry`
- BDD core + server suites, integration tests on real Postgres, property tests, Kani proofs
- Branch + line coverage tracked via Codecov on every PR

**Next:**
- Admin HTTP API parity with CLI
- More provider verifiers
- Per-emitter circuit breakers
- Public crates.io release

</details>

---

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
