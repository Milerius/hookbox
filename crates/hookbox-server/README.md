# hookbox-server

Standalone Axum HTTP server for hookbox.

Wires together `hookbox` core, `hookbox-postgres`, and `hookbox-providers` into a ready-to-deploy webhook ingestion service with admin API, health endpoints, and Prometheus metrics.

## Endpoints

```
┌─────────────────────────────────────────────────────────┐
│  hookbox-server                                         │
│                                                         │
│  Webhook Ingest                                         │
│  ─────────────                                          │
│  POST /webhooks/:provider    Ingest a webhook           │
│                                                         │
│  Health & Observability                                 │
│  ─────────────────────                                  │
│  GET  /healthz               Liveness probe             │
│  GET  /readyz                Readiness probe (DB check) │
│  GET  /metrics               Prometheus scrape endpoint │
│                                                         │
│  Admin API                                              │
│  ─────────                                              │
│  GET  /api/receipts          List / filter receipts     │
│  GET  /api/receipts/:id      Inspect one receipt        │
│  POST /api/receipts/:id/replay  Re-emit a receipt       │
│  GET  /api/dlq               List dead-lettered items   │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

## Request Flow

```
Provider (Stripe, BVNK, ...)
    │
    │  POST /webhooks/stripe
    ▼
┌─────────┐     ┌───────────────────┐     ┌──────────────┐
│  Axum   │────►│  HookboxPipeline  │────►│  PostgreSQL   │
│  Router │     │  verify → dedupe  │     │  (durable     │
│         │◄────│  → store → emit   │     │   inbox)      │
│  200 OK │     └───────────────────┘     └──────────────┘
└─────────┘              │
                         │  NormalizedEvent
                         ▼
                  Downstream consumer
                  (callback / channel)
```

## Configuration

The server reads from `hookbox.toml`:

```toml
[server]
host = "0.0.0.0"
port = 8080

[database]
url = "postgres://localhost/hookbox"
max_connections = 10

[providers.stripe]
type = "stripe"
secret = "whsec_..."
tolerance_seconds = 300

[providers.bvnk]
type = "bvnk"
secret = "..."

[providers.adyen]
type = "adyen"
# Hex-encoded HMAC key from the Adyen Customer Area
secret = "abc123..."

[providers.triplea_fiat]
type = "triplea-fiat"
# RSA public key in PEM format
public_key = """
-----BEGIN PUBLIC KEY-----
...
-----END PUBLIC KEY-----
"""

[providers.triplea_crypto]
type = "triplea-crypto"
# notify_secret from the Triple-A developer portal
secret = "..."
tolerance_seconds = 300

[providers.walapay]
type = "walapay"
# whsec_<base64> format secret from the Walapay dashboard
secret = "whsec_..."
tolerance_seconds = 300

[dedupe]
lru_capacity = 10000

[admin]
bearer_token = "..."

# Emitter backend (default: "channel")
# Options: "channel", "kafka", "nats", "sqs"
[emitter]
type = "channel"
```

### Kafka emitter

```toml
[emitter]
type = "kafka"

[emitter.kafka]
brokers = "localhost:9092"
topic = "hookbox-events"
client_id = "hookbox"       # optional, default: "hookbox"
acks = "all"                # optional, default: "all"
timeout_ms = 5000           # optional, default: 5000
```

### NATS emitter

```toml
[emitter]
type = "nats"

[emitter.nats]
url = "nats://localhost:4222"
subject = "hookbox.events"
```

### SQS emitter

```toml
[emitter]
type = "sqs"

[emitter.sqs]
queue_url = "https://sqs.us-east-1.amazonaws.com/123456789012/hookbox-events"
region = "us-east-1"   # optional
fifo = false           # set true for FIFO queues
```

## Metrics

Prometheus metrics exposed at `/metrics`:

| Metric | Type | Labels |
|--------|------|--------|
| `hookbox_webhooks_received_total` | Counter | `provider` |
| `hookbox_ingest_results_total` | Counter | `provider`, `result` |
| `hookbox_verification_results_total` | Counter | `provider`, `status` |
| `hookbox_dedupe_checks_total` | Counter | `provider`, `result` |
| `hookbox_emit_results_total` | Counter | `provider`, `result` |
| `hookbox_ingest_duration_seconds` | Histogram | |
| `hookbox_store_duration_seconds` | Histogram | |
| `hookbox_emit_duration_seconds` | Histogram | |
| `hookbox_dlq_depth` | Gauge | `provider` |
| `hookbox_inflight_count` | Gauge | |

## Retry Worker

A background task periodically retries receipts stuck in `EmitFailed` state.

Configuration via `hookbox.toml`:

```toml
[retry]
interval_seconds = 30
max_attempts = 5
```

After `max_attempts` failed retries, receipts are atomically promoted to `DeadLettered`.

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
