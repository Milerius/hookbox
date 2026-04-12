# Hookbox — Durable Webhook Inbox

## Quick Reference

**Toolchain: stable** (edition 2024). No nightly features required.

```
Build:          cargo build
Build all:      cargo build --all-features
Test:           cargo test --all-features
Test nextest:   cargo nextest run --all-features
Lint:           cargo clippy --all-targets --all-features -- -D warnings
Format:         cargo fmt --all
Format check:   cargo fmt --all --check
Deny:           cargo deny check
Careful:        cargo +nightly careful test --all-features
Coverage:       cargo llvm-cov --all-features --html
Coverage branch: cargo llvm-cov --all-features --branch --html
Kani:           cargo kani -p hookbox-verify
Fuzz:           cargo +nightly fuzz run <target>
Doc:            cargo doc --no-deps --all-features --open
Metrics:        Prometheus metrics recorded in pipeline, exposed at GET /metrics
Retry worker:   Spawned by `hookbox serve`, configured via [retry] in hookbox.toml
CLI commands:   hookbox receipts list/inspect/search, hookbox replay id/failed, hookbox dlq list/inspect/retry
```

## Architecture

See `docs/superpowers/specs/2026-04-10-hookbox-design.md` for the full design specification.

### Background Worker

A retry worker runs alongside the server, periodically retrying EmitFailed
receipts. After max_attempts failures, receipts are promoted to DeadLettered.
Configured via [retry] in hookbox.toml.

### Ingest Pipeline

```
Provider webhook → Receive → Verify → Dedupe → Store durably → Emit downstream
```

- ACK provider only after durable store succeeds
- Dedupe: LRU fast path (advisory), Postgres unique constraint (authoritative)
- Emit failure does not invalidate acceptance
- Raw body bytes preserved immutably for replay verification

## Workspace Layout

```
crates/hookbox/           hookbox           Core: traits, types, pipeline, lightweight impls
crates/hookbox-postgres/  hookbox-postgres   PostgreSQL storage backend
crates/hookbox-providers/ hookbox-providers  Signature verifiers (Stripe, BVNK, generic HMAC)
crates/hookbox-server/    hookbox-server     Standalone Axum HTTP server
crates/hookbox-cli/       hookbox-cli        CLI binary (inspect, replay, serve)
```

## Error Handling

`thiserror` at trait boundaries, `anyhow` in leaf application code.

| Crate | Strategy | Why |
|-------|----------|-----|
| `hookbox` (core) | `thiserror` | Consumers need typed errors they can `match` on |
| `hookbox-postgres` | `thiserror` | Errors wrap SQLx errors with domain meaning |
| `hookbox-providers` | `thiserror` | Verification errors are structured |
| `hookbox-server` | `thiserror` + `anyhow` for startup | Request-path errors map to HTTP responses; startup failures just propagate |
| `hookbox-cli` | `anyhow` | Application code — propagate with context, print for humans |
| `hookbox-verify` | `anyhow` | Test code — errors are just reported |

**Rules:**
- Never `unwrap()` or `expect()` in library/production code. `unwrap()` is acceptable in tests.
- Use `thiserror` wherever a consumer might need to `match` on the error variant.
- Use `anyhow` only in code that is the final error handler (CLI output, test assertions).
- Trait error types (e.g. `StorageError`, `EmitError`) must be enums with meaningful variants, not stringly-typed.

## Code Style
- **Newtypes over primitives**: `ReceiptId(Uuid)` not raw `Uuid` where the type carries domain meaning.
- **`let...else` for early returns**: keep happy path unindented.
- **No wildcard matches**: explicit destructuring on all enums.
- **`#[inline]`**: only on measured hot functions, never speculatively.
- **Async traits**: use `#[async_trait]` on the four core extension traits (`SignatureVerifier`, `Storage`, `DedupeStrategy`, `Emitter`) and all their implementers. Native `async fn` in traits is not dyn-compatible in stable Rust, and the architecture relies on `Arc<dyn Emitter + Send + Sync>` for runtime emitter selection from TOML config (`crates/hookbox-cli/src/commands/serve.rs:98`). Do not propose migrating these traits to native `async fn` — it would break the dyn dispatch.
- **Builder pattern**: for complex construction (`HookboxPipeline::builder()`).
- **Imports**: group by std → external crates → workspace crates → local modules.

## Trait Design

Four core extension points — all `Send + Sync`:

1. **`SignatureVerifier`** — provider signature verification
2. **`Storage`** — durable receipt persistence (authoritative dedupe via `StoreResult`)
3. **`DedupeStrategy`** — advisory fast-path duplicate detection
4. **`Emitter`** — downstream event forwarding

## Priority Order

```
1. Correctness (tests, property tests, kani proofs where applicable)
2. Safety (no panics in production paths, typed errors)
3. Clarity (readable code, explicit types, good naming)
4. Performance (measured, not speculated)
5. Ergonomics
```

## Testing Strategy

- **Unit tests**: in each crate, co-located with source
- **Integration tests**: `integration-tests/` directory, full pipeline flows with real Postgres
- **Property tests (bolero)**: in `hookbox-verify` crate for pipeline invariants
- **Kani proofs**: in `hookbox-verify` for critical state machine properties
- **Fuzz targets**: in relevant crates for parsing/verification
- **Mutation testing**: nightly via `cargo-mutants`

## Verification Tiers

```
1. Unit tests (every PR)
2. Integration tests (every PR)
3. Property tests — bolero (every PR)
4. Kani proofs (nightly)
5. Fuzz testing (nightly)
6. Mutation testing (nightly)
```

## Commits

- Imperative mood, <=72 char subject, one logical change per commit
- Run fmt + clippy + test before committing
- Feature branches, never push directly to main
