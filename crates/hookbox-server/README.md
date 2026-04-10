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
secret = "whsec_..."
tolerance_seconds = 300

[providers.bvnk]
type = "hmac-sha256"
secret = "..."
header = "X-Webhook-Signature"

[dedupe]
lru_capacity = 10000

[admin]
bearer_token = "..."
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

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
