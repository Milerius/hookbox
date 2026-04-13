# hookbox-scenarios

Cucumber BDD suite for hookbox. Exercises the ingest pipeline, emitter fan-out, retry policies, and the full Axum server from the outside using Gherkin feature files.

Two independent test binaries share a common step/world library (`src/`):

- **`core_bdd`** — runs against an in-memory `HookboxPipeline`. No database, no HTTP. Uses a `FakeEmitter` to drive deterministic success / failure / latency scenarios. Fast; runs on every PR.
- **`server_bdd`** — runs against a real `hookbox-server` Axum instance on a testcontainers-managed Postgres. Sends real HTTP via `reqwest` and asserts against actual `webhook_deliveries` rows. Gated behind the `bdd-server` feature so laptops without Docker can still run `core_bdd`.

## Feature Files

```
scenario-tests/features/
├── core/                       # core_bdd — in-memory pipeline
│   ├── backoff.feature         # compute_backoff + retry schedule invariants
│   ├── derived_state.feature   # ProcessingState derivation from deliveries
│   ├── emitters.feature        # per-emitter fan-out behaviour
│   ├── fanout.feature          # delivery-row creation, one per configured name
│   ├── ingest.feature          # verify → dedupe → store happy and error paths
│   ├── providers.feature       # signature verifier routing / failure reasons
│   └── retry.feature           # FailFor / retry-then-succeed flows
└── server/                     # server_bdd — real HTTP + Postgres
    ├── dlq.feature             # DLQ population on exhausted retries
    ├── fanout.feature          # end-to-end fan-out with EmitterWorker
    └── retry.feature           # lease expiry, reclaim, per-emitter retry policy
```

## Running

```bash
# Core suite — no external dependencies
cargo test -p hookbox-scenarios --test core_bdd

# Server suite — requires Docker (testcontainers manages Postgres)
cargo test -p hookbox-scenarios --test server_bdd --features bdd-server

# Run a single feature file
cargo test -p hookbox-scenarios --test core_bdd -- --name fanout

# Filter to one scenario by name
cargo test -p hookbox-scenarios --test core_bdd -- --name "Accept webhook with emitter names configured"
```

## Layout

```
scenario-tests/
├── Cargo.toml                  # bdd-server feature gate lives here
├── features/                   # Gherkin sources
├── src/
│   ├── world.rs                # IngestWorld — in-memory pipeline state
│   ├── server_world.rs         # ServerWorld — HTTP + Postgres state
│   ├── server_harness.rs       # testcontainers + axum::serve bootstrap
│   ├── steps_core.rs           # core_bdd step definitions
│   ├── steps_server.rs         # server_bdd step definitions
│   ├── fake_emitter.rs         # deterministic Emitter test double
│   └── lib.rs
└── tests/
    ├── core_bdd.rs             # cucumber entry point for features/core
    └── server_bdd.rs           # cucumber entry point for features/server
```

## When to Add a Scenario Here

Use `scenario-tests` when the assertion is about **observable behaviour** of the system — a webhook arrives, a delivery row materialises, a retry schedule ticks, a DLQ fills. For unit-level assertions (individual function return values, constructor edge cases), prefer an inline `#[test]` in the owning crate.

For full-stack Postgres-backed integration tests without the Gherkin overhead, see [`integration-tests/`](../integration-tests/README.md).

## License

Licensed under either of [Apache License, Version 2.0](../LICENSE-APACHE) or [MIT License](../LICENSE-MIT) at your option.
