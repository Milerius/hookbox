# Redis Streams Emitter + Emitter Test Coverage — Design Specification

Ship the V2 Redis Streams emitter and close the 0% test-coverage gap on all four emitter crates (Kafka, NATS, SQS, Redis) with real round-trip verification against ephemeral broker containers.

> **Status:** Approved (design summary). Pending written-spec review by user before implementation planning begins.

---

## Goals

1. **Add `hookbox-emitter-redis`** — Redis Streams adapter, parity in shape with the existing Kafka/NATS/SQS adapters from PR #13.
2. **Move all four emitter crates from 0% test coverage to credible coverage** via round-trip integration tests (publish → consume → assert on parsed `NormalizedEvent`).
3. **Make `cargo test` Just Work** without requiring contributors to run `docker compose up` first.
4. **Remove the `#[ignore]` write-only smoke tests** in `integration-tests/tests/emitter_smoke_test.rs` once round-trip tests cover the same surface area more rigorously.

## Non-goals

- Fan-out to multiple emitters (`[[emitters]]` array) — explicitly Phase 3.
- Cluster / sentinel support for Redis — single-node only in V2.
- MAXLEN trimming as default behavior — opt-in only.
- Templated stream keys — static stream key in V2.
- Redis pub/sub or Redis as a queue — Streams only.
- Per-emitter retry policies, emitter health in `/readyz`, emitter metrics — explicitly future work.
- LocalStack into the existing nightly CI for SQS — handled as part of the testcontainers move, not as a separate step.

---

## Decisions

### Test depth — round-trip verification

All emitter tests publish a `NormalizedEvent`, then consume it back from the broker (Kafka consumer / NATS subscribe / SQS `receive_message` / Redis `XREAD`), parse the JSON back into a `NormalizedEvent`, and assert on:

- `receipt_id` round-trips intact
- `provider_name` round-trips intact
- Backend-specific routing: Kafka partition key, SQS FIFO `MessageGroupId` / `MessageDeduplicationId`, Redis stream key

Write-only smoke tests are not enough — they prove "the call didn't `Err`" but catch zero serialization, key-routing, or schema bugs. The existing `#[ignore]`'d smoke tests will be deleted once the round-trip tests cover the surface.

### Container orchestration — `testcontainers-rs`

Each test spawns its own broker container via the Docker API using `testcontainers-rs`. No `docker-compose up` step. No `#[ignore]` flag. `cargo test` and `cargo llvm-cov` Just Work locally and in CI.

Trade-offs accepted:
- Container startup cost (~2–10s per broker). Mitigated by sharing one broker per test file via `OnceCell`.
- Docker daemon must be available wherever tests run. CI: yes. Local dev without Docker: those contributors run `cargo test --workspace --exclude hookbox-emitter-*` and rely on CI for emitter coverage.

### Redis Streams emitter shape

| Decision | Choice | Rationale |
|---|---|---|
| Stream key | **Static** (single stream from config) | Parity with Kafka topic / NATS subject / SQS queue. Consumers filter by `provider_name` field. |
| Trimming | **Off by default**, opt-in via `maxlen` config (`XADD ~ MAXLEN`) | Matches Kafka/NATS/SQS — no retention policy in the emitter. |
| Field encoding | **Single `data` field** containing the JSON event | Symmetry with the other emitters; consumers parse one blob. |
| Topology | **Single-node only** | V2 ship size. Cluster/sentinel become future Redis enhancements in the roadmap. |

### Redis client library — `redis-rs`

The `redis` crate (`redis-rs`) with the `tokio-comp` feature, locked to a recent version (0.27+).

Reasoning:
- Most popular Rust Redis client; instantly recognizable to any reviewer.
- Scope discipline: we explicitly cut cluster, sentinel, and pooling from V2 — exactly the features that would justify `fred`. Picking `fred` for capabilities we won't use is YAGNI.
- `XADD` and `XREAD` work fine through the generic `cmd()` builder; round-trip tests have plenty of public examples.
- Migration path is local: the `Emitter` trait insulates the rest of the codebase, so swapping clients later (if cluster ever becomes a real need) touches only this one crate.

### CI placement — PR CI on every push

The four round-trip integration tests run as part of `ci.yml` on every PR push, not gated behind a feature flag and not `#[ignore]`'d.

Reasoning:
- The whole point of moving off `#[ignore]` is so `cargo llvm-cov` and PR CI see these tests. Gating them back behind a feature flag undoes the move.
- Catching emitter regressions on PRs is materially more valuable than catching them next morning. The Triple-A pitch needs emitter *trust*; nightly-only leaves a 24h trust gap.
- Splitting tests across PR + nightly (the hybrid option) makes the test surface harder to audit and reason about — every emitter test should run together.
- Estimated PR CI cost: ~60–120s warm cache, ~3–5 min cold cache. Acceptable for this repo; PR CI runtime is not currently a pain point.

Mitigations for cold-cache slowness:
- Pin `testcontainers-modules` image tags in code so Docker cache layers hit consistently.
- Ensure `Swatinem/rust-cache@v2` is wired into `ci.yml` (already present in `nightly.yml`).
- Use Redpanda instead of Confluent Kafka in tests if startup time becomes a problem (Redpanda boots in ~2s vs Kafka's ~10s; wire-compatible with rdkafka).

---

## Architecture

### `hookbox-emitter-redis` crate

New workspace member at `crates/hookbox-emitter-redis/`. Mirrors the layout of `hookbox-emitter-nats` (the simplest of the three V1 emitters):

```
crates/hookbox-emitter-redis/
├── Cargo.toml
├── README.md
├── src/
│   └── lib.rs        # RedisEmitter struct + Emitter impl
└── tests/
    └── round_trip.rs # testcontainers-based round-trip test
```

`RedisEmitter` constructor signature (mirroring `NatsEmitter::new`):

```rust
pub async fn new(
    url: &str,            // redis://host:port
    stream: String,       // static stream key
    maxlen: Option<u64>,  // opt-in approximate trimming
) -> Result<Self, EmitError>
```

`emit()` calls `XADD <stream> [MAXLEN ~ <n>] * data <json>` via the chosen client. Errors map to `EmitError::Downstream` and `EmitError::Timeout`.

### Server config wiring

`crates/hookbox-server/src/config.rs:175` (`EmitterConfig`) gains a new variant:

```rust
pub struct EmitterConfig {
    pub emitter_type: String,        // "channel" | "kafka" | "nats" | "sqs" | "redis"
    pub kafka: Option<KafkaEmitterConfig>,
    pub nats:  Option<NatsEmitterConfig>,
    pub sqs:   Option<SqsEmitterConfig>,
    pub redis: Option<RedisEmitterConfig>,  // new
}

pub struct RedisEmitterConfig {
    pub url: String,
    pub stream: String,
    pub maxlen: Option<u64>,
}
```

`crates/hookbox-cli/src/commands/serve.rs` gets a new `match` arm constructing `RedisEmitter::new(...)` and wrapping it in `Arc<dyn Emitter>`.

### Test architecture

Each emitter crate gains a `tests/round_trip.rs` integration test file. Pattern (Redis as the example):

```rust
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

#[tokio::test]
async fn redis_emitter_round_trip() {
    let container = Redis::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(6379).await.unwrap();
    let url = format!("redis://127.0.0.1:{port}");

    let emitter = RedisEmitter::new(&url, "hookbox.test".to_owned(), None)
        .await
        .unwrap();

    let event = make_test_event();
    emitter.emit(&event).await.unwrap();

    // Consume back via XREAD and assert
    let received: NormalizedEvent = read_one_from_stream(&url, "hookbox.test").await;
    assert_eq!(received.receipt_id, event.receipt_id);
    assert_eq!(received.provider_name, event.provider_name);
}
```

Each emitter's test file uses the matching `testcontainers-modules` image:

| Crate | Container module |
|---|---|
| `hookbox-emitter-kafka` | `testcontainers_modules::kafka::Kafka` (or `redpanda` for faster startup) |
| `hookbox-emitter-nats` | `testcontainers_modules::nats::Nats` |
| `hookbox-emitter-sqs` | `testcontainers_modules::localstack::LocalStack` |
| `hookbox-emitter-redis` | `testcontainers_modules::redis::Redis` |

A `make_test_event()` helper provides identical input across all four files. Mechanism (small dev-dep crate vs duplicated per-file constant) is deferred to the implementation plan — both are trivial and the choice doesn't affect any other decision in this spec.

### Cleanup

- **Delete** `integration-tests/tests/emitter_smoke_test.rs` once the four round-trip files are green.
- **Delete** `docker-compose.test.yml` — round-trip tests no longer depend on it.
- **Delete** the `emitter-smoke` job from `.github/workflows/nightly.yml` — PR CI now covers this surface more rigorously via the round-trip tests.

---

## Error handling

Round-trip tests assert on the happy path. The Q1 decision was pure round-trip (option B), so error-path tests (timeout, broker down, malformed config) are **not** included in this scope. Those become a follow-up if/when we want to push past the round-trip baseline.

Inside the production code, error mapping stays consistent with the V1 emitters:

- Connection / send failure → `EmitError::Downstream(String)`
- Operation timeout → `EmitError::Timeout(String)`
- Serialization failure → `EmitError::Downstream(String)` (matches Kafka emitter)

---

## Testing strategy

Per the locked-in decisions:

1. **Per-crate round-trip integration tests** — `tests/round_trip.rs` in each of the four emitter crates, using `testcontainers-rs` + `testcontainers-modules`.
2. **No unit tests in this scope** — pure round-trip, no error-path/unit additions.
3. **Coverage measurement** — `cargo llvm-cov --all-features` picks up the round-trip tests because they are not `#[ignore]`'d. Target: meaningfully above 0% on each emitter crate (specific number not committed in this spec; whatever round-trip naturally yields).
4. **CI placement** — PR CI on every push, as part of the standard `cargo test --all-features` job in `.github/workflows/ci.yml`. See the "CI placement — PR CI on every push" decision section above for full reasoning.

---

## Effort estimate

| Workstream | Estimate |
|---|---|
| `hookbox-emitter-redis` crate + config wiring + Redis round-trip test | ~1.5 days |
| Round-trip tests for Kafka, NATS, SQS (3 files) | ~1.5 days |
| Cleanup (delete smoke test, compose file, nightly emitter-smoke job) | ~0.5 day |
| **Total** | **~3.5 days focused work** |

This is at the lower end of the roadmap's "1.5 + 2–3 days" estimate because every fork picked the simpler option: single-node Redis, static stream key, `redis-rs` over `fred`, no error-path tests, no fan-out, no sentinel.

---

## Out of scope (explicit, do not creep)

- Multi-emitter fan-out
- Per-provider retry policies, circuit breakers, DLQ alerting
- Redis cluster / sentinel
- Redis pub/sub
- Templated stream keys
- MAXLEN as default
- LocalStack added to docker-compose.test.yml as a separate workstream (testcontainers replaces it)
- Changes to the four V1 emitter crates' production code beyond what the round-trip tests require
- Checkout.com / PayPal verifiers (separate Phase 2 item)
