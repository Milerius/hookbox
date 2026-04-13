# hookbox-server

Standalone Axum HTTP server for hookbox.

Wires together `hookbox` core, `hookbox-postgres`, `hookbox-providers`, and the emitter-backend crates into a ready-to-deploy webhook ingestion service with admin API, per-emitter health, and Prometheus metrics.

## Endpoints

```
┌─────────────────────────────────────────────────────────────────┐
│  hookbox-server                                                 │
│                                                                 │
│  Webhook Ingest                                                 │
│  ──────────────                                                 │
│  POST /webhooks/{provider}            Ingest a webhook          │
│                                                                 │
│  Health & Observability                                         │
│  ──────────────────────                                         │
│  GET  /healthz                        Liveness                  │
│  GET  /readyz                         DB + per-emitter health   │
│  GET  /metrics                        Prometheus scrape         │
│                                                                 │
│  Admin API — Receipts                                           │
│  ─────────────────────                                          │
│  GET  /api/receipts                   List / filter receipts    │
│  GET  /api/receipts/{id}              Inspect one receipt       │
│  POST /api/receipts/{id}/replay       Enqueue replay for all    │
│                                       configured emitters       │
│                                                                 │
│  Admin API — Deliveries                                         │
│  ───────────────────────                                        │
│  GET  /api/deliveries/{id}            Inspect one delivery row  │
│  POST /api/deliveries/{id}/replay     Enqueue replay of one     │
│                                       emitter's delivery        │
│                                                                 │
│  Admin API — Emitters & DLQ                                     │
│  ───────────────────────────                                    │
│  GET  /api/emitters                   List configured emitters  │
│                                       with queue depths         │
│  GET  /api/dlq                        List dead-lettered rows   │
└─────────────────────────────────────────────────────────────────┘
```

All `/api/*` endpoints honour the optional admin bearer token; `/webhooks/*`, `/healthz`, `/readyz`, and `/metrics` are always public.

### `GET /readyz`

Returns structured JSON aggregating the DB ping and every `[[emitters]]` entry's last snapshot:

```json
{
  "status": "healthy",
  "database": { "status": "healthy" },
  "emitters": {
    "kafka": {
      "status": "healthy",
      "last_success_at": "2026-04-13T05:00:00Z",
      "last_failure_at": null,
      "consecutive_failures": 0,
      "dlq_depth": 0,
      "pending_count": 0
    }
  }
}
```

Returns `503 Service Unavailable` if the database is unreachable **or** any emitter is `unhealthy`; `200 OK` (with `status: "degraded"`) when any emitter is degraded but none unhealthy. Configured emitters whose worker never registered a snapshot are synthesised as `unhealthy`, so a missing worker always trips readiness.

## Request Flow

```
Provider (Stripe, BVNK, Adyen, …)
    │
    │  POST /webhooks/stripe
    ▼
┌─────────┐     ┌───────────────────────┐     ┌─────────────────┐
│  Axum   │────►│  HookboxPipeline      │────►│  PostgreSQL     │
│ Router  │     │  verify → dedupe →    │     │  webhook_       │
│         │◄────│  store receipt + fan- │     │  receipts +     │
│  200 OK │     │  out delivery rows    │     │  deliveries     │
└─────────┘     └───────────────────────┘     └─────────────────┘
                                                       ▲
                                                       │ claim / mark
                                                       │
                                              ┌────────┴────────┐
                                              │ EmitterWorker   │
                                              │ (one per emitter│
                                              │  in config)     │
                                              └────────┬────────┘
                                                       │ emit
                                                       ▼
                                              Kafka / NATS / SQS /
                                              Redis Streams
```

Inline emission was removed. Ingest stores one `pending` delivery row per configured emitter name inside the receipt transaction and returns. Background `EmitterWorker` tasks claim pending rows with `SELECT ... FOR UPDATE SKIP LOCKED`, dispatch to their backend, and drive each row through `Emitted` or `DeadLettered`.

## Configuration

`hookbox.toml`:

```toml
[server]
host = "0.0.0.0"
port = 8080
body_limit = 1048576          # max ingest body size, bytes

[database]
url = "postgres://localhost/hookbox"
max_connections = 10

[dedupe]
lru_capacity = 10000

[admin]
bearer_token = "..."          # optional; when set, /api/* requires Bearer

[providers.stripe]
type = "stripe"
secret = "whsec_..."
tolerance_seconds = 300

[providers.adyen]
type = "adyen"
secret = "abc123..."          # hex-encoded HMAC from the Customer Area

[providers.walapay]
type = "walapay"
secret = "whsec_..."
tolerance_seconds = 300
```

### Emitters (`[[emitters]]`)

Each `[[emitters]]` entry spawns one `EmitterWorker`. `name` is the string you pass to `HookboxPipeline::emitter_names` and what shows up as the `emitter` label on metrics and in `/readyz`.

```toml
[[emitters]]
name = "kafka"                # unique per process, used in metrics + /readyz
type = "kafka"
poll_interval_seconds = 1     # how often to claim a new batch
concurrency = 8               # max in-flight deliveries
lease_duration_seconds = 60   # optional; reclaim stuck in_flight rows after

[emitters.kafka]
brokers = "localhost:9092"
topic = "hookbox-events"
client_id = "hookbox"
acks = "all"
timeout_ms = 5000

[emitters.retry]              # per-emitter retry policy
max_attempts = 8
initial_backoff_seconds = 2
max_backoff_seconds = 600
backoff_multiplier = 2.0
jitter = 0.2

[[emitters]]
name = "sqs-us-east"
type = "sqs"
poll_interval_seconds = 1
concurrency = 16

[emitters.sqs]
queue_url = "https://sqs.us-east-1.amazonaws.com/123456789012/hookbox-events"
region = "us-east-1"
fifo = false

[emitters.retry]
max_attempts = 5
initial_backoff_seconds = 5
max_backoff_seconds = 300
backoff_multiplier = 2.0
jitter = 0.1

[[emitters]]
name = "nats"
type = "nats"

[emitters.nats]
url = "nats://localhost:4222"
subject = "hookbox.events"

[[emitters]]
name = "redis"
type = "redis"

[emitters.redis]
url = "redis://localhost:6379"
stream = "hookbox-events"
maxlen = 1000000
timeout_ms = 5000
```

`type` is one of `"kafka" | "nats" | "sqs" | "redis"`. Unknown fields in any emitter entry are rejected at startup (`deny_unknown_fields`), and `poll_interval_seconds = 0` is a hard validation error to prevent busy-looping against Postgres.

## Metrics

Prometheus metrics exposed at `/metrics`:

| Metric | Type | Labels |
|--------|------|--------|
| `hookbox_webhooks_received_total` | Counter | `provider` |
| `hookbox_ingest_results_total` | Counter | `provider`, `result` |
| `hookbox_verification_results_total` | Counter | `provider`, `status`, `reason` |
| `hookbox_dedupe_checks_total` | Counter | `provider`, `result` |
| `hookbox_ingest_duration_seconds` | Histogram | — |
| `hookbox_store_duration_seconds` | Histogram | — |
| `hookbox_emit_results_total` | Counter | `emitter`, `result` |
| `hookbox_emit_duration_seconds` | Histogram | `emitter` |
| `hookbox_emit_attempt_count` | Histogram | `emitter` |
| `hookbox_emit_reclaimed_total` | Counter | `emitter` |
| `hookbox_emit_pending` | Gauge | `emitter` |
| `hookbox_emit_in_flight` | Gauge | `emitter` |
| `hookbox_dlq_depth` | Gauge | `emitter` |

Note: `hookbox_emit_results_total` now fires once **per delivery attempt** (not per ingest), so per-receipt fan-out and retries are both visible. Reclaim counter > 0 indicates worker crashes or a too-tight lease duration.

## Background Workers

`hookbox serve` spawns:

- **One `EmitterWorker` per `[[emitters]]` entry.** Each worker claims pending deliveries for its own `name`, dispatches them to the configured backend with the entry's retry policy, and publishes an `EmitterHealth` snapshot into `AppState.emitter_health` that `/readyz` reads. Each worker also runs a drain task that refreshes the queue-depth gauges and reclaims expired `in_flight` rows.
- **Graceful shutdown** waits for each worker + drain task to finish; any panicking task is logged with its name and returned as an aggregated startup error.

Legacy receipt-level retry (`RetryWorker`, `[retry]` in config) still compiles but is no longer spawned — fan-out with per-emitter retry policies replaces it.

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
