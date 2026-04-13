# hookbox-cli

CLI binary for hookbox. Produces the `hookbox` executable — the single entry point for running the server and operating on the durable inbox.

## Commands

```
hookbox
├── serve                Start the standalone webhook server
├── config
│   └── validate         Parse hookbox.toml and report errors
├── receipts
│   ├── list             List receipts with filters
│   ├── inspect          Show full details of one receipt
│   └── search           Search by external reference
├── replay
│   ├── id               Re-emit a single receipt
│   └── failed           Re-emit failed receipts matching filters
├── dlq
│   ├── list             List dead-lettered deliveries
│   ├── inspect          Show DLQ entry details
│   └── retry            Retry a dead-lettered delivery
└── emitters
    └── list             Per-emitter queue depths from the DB
```

## Usage

### Server

```bash
# Start the server. Spawns one EmitterWorker per [[emitters]] entry in the config.
hookbox serve --config hookbox.toml

# DATABASE_URL overrides [database].url from the config.
DATABASE_URL=postgres://localhost/hookbox hookbox serve --config hookbox.toml
```

### Config validation

```bash
# Parse the config, run the emitter validator, and fail non-zero on errors.
hookbox config validate --config hookbox.toml
```

### Receipts

```bash
hookbox receipts list --database-url postgres://localhost/hookbox \
    --provider stripe --state failed

hookbox receipts inspect --database-url postgres://localhost/hookbox \
    550e8400-e29b-41d4-a716-446655440000

hookbox receipts search --database-url postgres://localhost/hookbox \
    --external-ref pay_123
```

### Replay

```bash
# Re-emit one receipt — fan out to every configured emitter by inserting
# fresh pending delivery rows.
hookbox replay id --database-url postgres://localhost/hookbox \
    550e8400-e29b-41d4-a716-446655440000

# Re-emit every failed Stripe receipt from the last hour.
hookbox replay failed --database-url postgres://localhost/hookbox \
    --since 1h --provider stripe
```

### Dead Letter Queue

```bash
hookbox dlq list --database-url postgres://localhost/hookbox --emitter kafka

hookbox dlq inspect --database-url postgres://localhost/hookbox \
    <delivery_id>

# Re-enqueue a dead-lettered delivery. Rejects replays for emitters that
# are no longer present in the config.
hookbox dlq retry --database-url postgres://localhost/hookbox \
    --config hookbox.toml <delivery_id>
```

### Emitters

```bash
# Report pending / in-flight / dead-lettered depth for every [[emitters]] entry.
hookbox emitters list --config hookbox.toml \
    --database-url postgres://localhost/hookbox
```

## Connection

Every subcommand except `serve` and `config validate` takes `--database-url`, or reads `DATABASE_URL` from the environment:

```bash
export DATABASE_URL=postgres://localhost/hookbox
hookbox receipts list --provider stripe --state failed
```

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
