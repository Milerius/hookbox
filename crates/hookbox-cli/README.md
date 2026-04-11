# hookbox-cli

CLI binary for hookbox. Produces the `hookbox` executable — the single entry point for all operational commands and server management.

## Commands

```
hookbox
├── serve          Start the standalone webhook server
├── receipts
│   ├── list       List receipts with filters
│   ├── inspect    Show full details of one receipt
│   └── search     Search by external reference
├── replay
│   ├── <id>       Re-emit a single receipt
│   └── failed     Re-emit failed receipts with filters
└── dlq
    ├── list       List dead-lettered receipts
    ├── inspect    Show DLQ entry details
    └── retry      Retry a dead-lettered receipt
```

## Usage

### Server

```bash
# Start the webhook ingestion server
hookbox serve --config hookbox.toml

# With environment variable override
DATABASE_URL=postgres://localhost/hookbox hookbox serve
```

### Inspecting Receipts

```bash
# List failed receipts for a provider
hookbox receipts list --database-url postgres://localhost/hookbox --provider stripe --state failed

# Inspect a specific receipt (full payload, headers, verification details)
hookbox receipts inspect --database-url postgres://localhost/hookbox 550e8400-e29b-41d4-a716-446655440000

# Search by business reference
hookbox receipts search --database-url postgres://localhost/hookbox --external-ref pay_123
```

### Replay

```bash
# Re-emit a single receipt by ID
hookbox replay id --database-url postgres://localhost/hookbox 550e8400-e29b-41d4-a716-446655440000

# Re-emit all failed Stripe receipts from the last hour
hookbox replay failed --database-url postgres://localhost/hookbox --since 1h --provider stripe
```

### Dead Letter Queue

```bash
# List DLQ entries for a provider
hookbox dlq list --database-url postgres://localhost/hookbox --provider stripe

# Inspect a DLQ entry
hookbox dlq inspect --database-url postgres://localhost/hookbox 550e8400-e29b-41d4-a716-446655440000

# Retry a dead-lettered receipt
hookbox dlq retry --database-url postgres://localhost/hookbox 550e8400-e29b-41d4-a716-446655440000
```

## Connection

All commands accept `--database-url` for direct database access. Alternatively, set the `DATABASE_URL` environment variable:

```bash
export DATABASE_URL=postgres://localhost/hookbox
hookbox receipts list --provider stripe --state failed
```

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
