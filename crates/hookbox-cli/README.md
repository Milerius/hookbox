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
# List recent receipts from Stripe
hookbox receipts list --provider stripe --limit 20

# List failed receipts
hookbox receipts list --provider stripe --state failed

# Inspect a specific receipt (full payload, headers, verification details)
hookbox receipts inspect 550e8400-e29b-41d4-a716-446655440000

# Search by business reference
hookbox receipts search --ref "pay_abc123"
```

### Replay

```bash
# Re-emit a single receipt
hookbox replay 550e8400-e29b-41d4-a716-446655440000

# Re-emit all failed Stripe receipts from the last hour
hookbox replay failed --provider stripe --since 1h
```

### Dead Letter Queue

```bash
# List all DLQ entries
hookbox dlq list

# List DLQ entries for a specific provider
hookbox dlq list --provider stripe

# Inspect a DLQ entry
hookbox dlq inspect 550e8400-e29b-41d4-a716-446655440000

# Retry a dead-lettered receipt
hookbox dlq retry 550e8400-e29b-41d4-a716-446655440000
```

## Connection

The CLI connects to the hookbox database directly for read operations (list, inspect, search) and to the admin API for write operations (replay, retry). Connection details come from `--config` or environment variables.

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
