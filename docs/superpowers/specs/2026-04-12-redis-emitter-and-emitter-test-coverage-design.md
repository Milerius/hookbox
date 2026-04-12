# Redis Streams Emitter + Emitter Test Coverage — Design Specification

Ship the V2 Redis Streams emitter and close the 0% test-coverage gap on all four emitter crates (Kafka, NATS, SQS, Redis) with real round-trip verification against ephemeral broker containers.

> **Status:** Approved (design summary), revised after Codex (gpt-5.4) review — see "Revisions after Codex review" at the end. Pending written-spec re-review by user before implementation planning begins.

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

All emitter tests publish a `NormalizedEvent`, then consume it back from the broker (Kafka consumer / NATS subscribe / SQS `receive_message` / Redis `XREAD`), parse the JSON back into a `NormalizedEvent`, and assert on **full struct equality** plus the backend-specific routing surface:

- **`assert_eq!(received, event)`** — full `NormalizedEvent` round-trip, covering `receipt_id`, `provider_name`, `event_type`, `external_reference`, `parsed_payload`, `payload_hash`, `received_at`, and `metadata`. Any serialization or schema regression in any field fails the test.
- **Kafka**: assert on the consumed `BorrowedMessage::key()` to verify it equals the receipt ID — proves partition routing, not just delivery.
- **NATS**: assert on the subject the message arrived on (round-trips through the test subscription).
- **SQS**: when FIFO is enabled, request `MessageSystemAttributeNames=[MessageGroupId, MessageDeduplicationId]` on `ReceiveMessage` and assert the values equal `provider_name` and `receipt_id` respectively.
- **Redis**: assert the message key returned by `XREAD` came from the configured stream and that the `data` field round-trips through `serde_json`.

Note on `received_at`: `chrono::DateTime<Utc>` round-trips through `serde_json` losslessly with the default RFC 3339 formatter, so a direct `assert_eq!` works without precision-stripping. If a test ever flakes on this it indicates a real serialization regression.

Write-only smoke tests are not enough — they prove "the call didn't `Err`" but catch zero serialization, key-routing, or schema bugs. The existing `#[ignore]`'d smoke tests will be deleted once the round-trip tests cover the surface.

### Container orchestration — `testcontainers-rs`

Each test spawns its own broker container via the Docker API using `testcontainers-rs`. No `docker-compose up` step. No `#[ignore]` flag. `cargo test -p hookbox-emitter-redis` (etc.) Just Works on any machine with a Docker daemon.

**One container per test, no `OnceCell` sharing.** Each emitter crate has exactly one round-trip test, so per-file container sharing buys nothing and adds container-lifetime complexity. Container startup is paid once per crate test invocation.

**Container host comes from the testcontainers API**, not hardcoded `127.0.0.1`. All test code uses `container.get_host().await` and `container.get_host_port_ipv4(<port>).await` to construct the broker URL. This works under rootless Docker, podman, and remote Docker socket setups where `127.0.0.1` would be wrong.

Trade-offs accepted:
- Container startup cost (~2–10s per broker, ~15s for LocalStack cold).
- Docker daemon must be available wherever tests run. **macOS GitHub Actions runners do not have Docker** — see the CI placement decision below for how this is handled.
- Local dev without Docker: those contributors run `cargo test --workspace --exclude hookbox-emitter-kafka --exclude hookbox-emitter-nats --exclude hookbox-emitter-sqs --exclude hookbox-emitter-redis` and rely on CI for emitter coverage.

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

### CI placement — dedicated Linux-only emitter test job in `ci.yml`

The four round-trip integration tests run on every PR push, but they cannot run inside the existing `test` matrix job in `.github/workflows/ci.yml:44` because that job runs on `[ubuntu-latest, macos-latest]` and **GitHub Actions macOS runners do not have Docker**. testcontainers would fail on the macOS leg.

Layout:

1. **Modify the existing `test` job** to exclude the four emitter crates from the workspace test pass, e.g. `cargo nextest run ... --workspace --exclude hookbox-integration-tests --exclude hookbox-emitter-kafka --exclude hookbox-emitter-nats --exclude hookbox-emitter-sqs --exclude hookbox-emitter-redis`. The matrix continues to run on both Linux and macOS for the rest of the workspace.
2. **Add a new `test-emitters` job** in `ci.yml`, Linux-only (`runs-on: ubuntu-latest`), runs `cargo test -p hookbox-emitter-kafka -p hookbox-emitter-nats -p hookbox-emitter-sqs -p hookbox-emitter-redis --all-features`. Includes `Swatinem/rust-cache@v2` and the rdkafka system deps the existing jobs already install.
3. **Delete** the `emitter-smoke` job from `.github/workflows/nightly.yml` — superseded.

Reasoning:
- The whole point of moving off `#[ignore]` is so `cargo llvm-cov` and PR CI see these tests on at least one OS. Gating them back behind a feature flag undoes the move.
- Catching emitter regressions on PRs is materially more valuable than catching them next morning. The Triple-A pitch needs emitter *trust*; nightly-only leaves a 24h trust gap.
- Splitting Linux/macOS for emitter coverage is acceptable: the production target for hookbox is Linux, and emitter SDKs (rdkafka, async-nats, aws-sdk-sqs, redis-rs) are all platform-portable. macOS is a developer-machine convenience, not a deployment target.
- Estimated added PR CI cost: ~2–4 minutes for the dedicated `test-emitters` job, running in parallel with the existing `test` matrix job. PR CI wall time grows by ~0 (parallelism), worker minutes grow by ~3–5.

Mitigations for cold-cache slowness:
- Pin `testcontainers-modules` image tags in code so Docker cache layers hit consistently.
- Wire `Swatinem/rust-cache@v2` into the new `test-emitters` job.
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

`RedisEmitter` constructor signature (mirroring `KafkaEmitter::new`'s explicit timeout parameter):

```rust
pub async fn new(
    url: &str,            // redis://host:port
    stream: String,       // static stream key
    maxlen: Option<u64>,  // opt-in approximate trimming
    timeout_ms: u64,      // per-operation timeout for XADD
) -> Result<Self, EmitError>
```

`emit()` calls `XADD <stream> [MAXLEN ~ <n>] * data <json>` via the chosen client, wrapping the call in `tokio::time::timeout(self.timeout, ...)`. Errors map as follows:

- `tokio::time::timeout` elapsed → `EmitError::Timeout(String)`
- `redis::RedisError` (connection / send / protocol) → `EmitError::Downstream(String)`
- `serde_json::Error` (serialization) → `EmitError::Downstream(String)` — matches `KafkaEmitter`

The explicit timeout matches `KafkaEmitter` (`timeout_ms`) and the hardcoded 10-second timeout in `SqsEmitter`. Without this, `redis-rs` calls block indefinitely on a hung connection because the client does not enforce a timeout by default.

The corresponding `RedisEmitterConfig` in `hookbox-server` exposes `timeout_ms` with a sensible default (e.g. 5000) so common deployments don't have to think about it.

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
    #[serde(default = "default_redis_timeout_ms")]
    pub timeout_ms: u64,   // default 5000
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
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(6379).await.unwrap();
    let url = format!("redis://{host}:{port}");

    let emitter = RedisEmitter::new(&url, "hookbox.test".to_owned(), None, 5000)
        .await
        .unwrap();

    let event = make_test_event();
    emitter.emit(&event).await.unwrap();

    // Consume back via XREAD and assert FULL struct equality
    let received: NormalizedEvent = read_one_from_stream(&url, "hookbox.test").await;
    assert_eq!(received, event);
}
```

Two non-obvious points enforced by every test file:
- `container.get_host()` is used instead of hardcoding `127.0.0.1`, so tests work under rootless Docker, podman, and remote Docker socket setups.
- Assertion is `assert_eq!(received, event)` (full `NormalizedEvent` equality), not field-by-field. This catches every serialization regression, not just `receipt_id`/`provider_name` ones.

Each emitter's test file uses the matching `testcontainers-modules` image:

| Crate | Container module |
|---|---|
| `hookbox-emitter-kafka` | `testcontainers_modules::kafka::Kafka` (or `redpanda` for faster startup) |
| `hookbox-emitter-nats` | `testcontainers_modules::nats::Nats` |
| `hookbox-emitter-sqs` | `testcontainers_modules::localstack::LocalStack` |
| `hookbox-emitter-redis` | `testcontainers_modules::redis::Redis` |

A `make_test_event()` helper provides identical input across all four files. Mechanism (small dev-dep crate vs duplicated per-file constant) is deferred to the implementation plan — both are trivial and the choice doesn't affect any other decision in this spec.

#### Per-backend bootstrap details

Each round-trip test must do more than spawn a container — the broker has to be brought to a state where the emitter can actually publish. These bootstrap steps are part of the test code, not the production crate:

**Kafka (`hookbox-emitter-kafka`)**
- Spawn container, read advertised port via `get_host_port_ipv4(9092)`.
- Topic auto-creation is enabled by default in the testcontainers Kafka image, so no explicit `CreateTopic` call is needed. The test publishes to `"hookbox-test"` and reads it back via a `StreamConsumer` configured with a unique `group.id` per test run and `auto.offset.reset=earliest`.
- Assert on `BorrowedMessage::key()` to verify the receipt-ID-as-partition-key contract.

**NATS (`hookbox-emitter-nats`)**
- Spawn container, read port via `get_host_port_ipv4(4222)`.
- Subscribe to `"hookbox.test"` *before* the emitter publishes to avoid the race where a NATS subject has no subscribers and the message is dropped.
- Read one message off the subscription, parse, full `assert_eq!`.

**SQS (`hookbox-emitter-sqs`) — most involved**
- Spawn LocalStack container, read port via `get_host_port_ipv4(4566)`.
- Inject dummy AWS credentials for the SDK: set `AWS_ACCESS_KEY_ID=test`, `AWS_SECRET_ACCESS_KEY=test`, `AWS_REGION=us-east-1` for the test process (LocalStack accepts any non-empty creds; the SDK fails with `NoCredentialsProviderError` if these are absent).
- Use the `aws-sdk-sqs::Client` directly to call `CreateQueue { queue_name: "hookbox-test.fifo", attributes: { FifoQueue: "true", ContentBasedDeduplication: "false" } }` and capture the returned `queue_url`.
- Construct `SqsEmitter::new(queue_url, Some("us-east-1"), true /* fifo */, Some(<localstack-endpoint>))` and `emit()`.
- Call `ReceiveMessage` with `MessageSystemAttributeNames=[MessageGroupId, MessageDeduplicationId]` and `MaxNumberOfMessages=1`.
- Assert: full `NormalizedEvent` round-trip via `Message::body`, `MessageGroupId == event.provider_name`, `MessageDeduplicationId == event.receipt_id.to_string()`.
- Run a parallel non-FIFO test against a standard queue to cover the `fifo = false` code path.

**Redis (`hookbox-emitter-redis`)**
- Spawn container, read port via `get_host_port_ipv4(6379)`.
- No bootstrap — `XADD` creates the stream on first write.
- Read back via `XREAD COUNT 1 STREAMS hookbox-test 0`, parse the `data` field as `serde_json::from_slice::<NormalizedEvent>`, full `assert_eq!`.

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
4. **CI placement** — PR CI on every push via a new dedicated Linux-only `test-emitters` job in `.github/workflows/ci.yml`. The four emitter crates are excluded from the existing `test` matrix job to keep the macOS leg green (no Docker on macOS GHA runners). See the "CI placement — dedicated Linux-only emitter test job in `ci.yml`" decision section above for full reasoning.

---

## Effort estimate

| Workstream | Estimate |
|---|---|
| `hookbox-emitter-redis` crate (incl. timeout wrap) + config wiring + Redis round-trip test | ~1.5 days |
| Round-trip tests for Kafka, NATS (2 files) | ~1 day |
| SQS round-trip test (CreateQueue, dummy creds, FIFO system attribute requesting, both FIFO and standard cases) | ~1 day |
| New `test-emitters` job in `ci.yml` + exclude emitters from existing matrix `test` job | ~0.5 day |
| Cleanup (delete smoke test, compose file, nightly emitter-smoke job) | ~0.5 day |
| **Total** | **~4.5 days focused work** |

This is +1 day from the pre-Codex-review estimate (3.5 → 4.5) because the SQS bootstrap is more involved than the rest, and the CI split into a dedicated job is its own small workstream. Still inside the roadmap's "1.5 + 2–3 days" envelope.

---

## Out of scope (explicit, do not creep)

- Multi-emitter fan-out
- Per-provider retry policies, circuit breakers, DLQ alerting
- Redis cluster / sentinel
- Redis pub/sub
- Templated stream keys
- MAXLEN as default
- LocalStack added to docker-compose.test.yml as a separate workstream (testcontainers replaces it)
- Changes to the four V1 emitter crates' production code beyond what the round-trip tests require (the timeout wrap on Redis is the only new behavior; Kafka/NATS/SQS production code is untouched)
- Checkout.com / PayPal verifiers (separate Phase 2 item)

---

## Revisions after Codex (gpt-5.4) review

The first draft of this spec was reviewed by Codex with model `gpt-5.4`. The review surfaced 5 findings (2 high, 3 medium), all of which were applied inline. Summary of what changed and why:

### High-severity fixes

**1. CI placement was incompatible with the existing `test` matrix.**
The original spec said the round-trip tests would run "as part of the standard `cargo test --all-features` job in `ci.yml`." That job runs on `[ubuntu-latest, macos-latest]`, and macOS GitHub Actions runners do not have Docker, so testcontainers would fail on the macOS leg. **Fix:** introduced a dedicated Linux-only `test-emitters` job in `ci.yml`, and the emitter crates are explicitly excluded from the existing matrix `test` job. See the revised "CI placement" decision section.

**2. SQS/LocalStack test plan was underspecified.**
The original spec said "use `testcontainers_modules::localstack::LocalStack`" and stopped there. It did not specify queue creation, AWS credential injection, FIFO attribute setup, or `MessageSystemAttributeNames` requesting on `ReceiveMessage` — without which the FIFO routing assertions cannot work. **Fix:** added the "Per-backend bootstrap details" subsection with explicit steps for all four brokers, with the SQS path covering `CreateQueue` with `FifoQueue=true`, dummy `AWS_ACCESS_KEY_ID=test` / `AWS_SECRET_ACCESS_KEY=test` / `AWS_REGION=us-east-1`, `MessageSystemAttributeNames=[MessageGroupId, MessageDeduplicationId]` on receive, and a parallel non-FIFO test for the `fifo = false` code path.

### Medium-severity fixes

**3. Redis timeout was claimed but not designed.**
The original `RedisEmitter::new` constructor had no timeout parameter, but the spec mapped `emit()` errors to both `EmitError::Downstream` and `EmitError::Timeout`. `redis-rs` does not enforce timeouts unless the call is wrapped explicitly. **Fix:** added a `timeout_ms: u64` parameter to `RedisEmitter::new`, the `emit()` body wraps `XADD` in `tokio::time::timeout`, and `RedisEmitterConfig` exposes `timeout_ms` with a default of 5000. Brings parity with `KafkaEmitter::new`'s `timeout_ms` and `SqsEmitter`'s hardcoded 10s timeout.

**4. Round-trip assertions were too weak.**
The original example only checked `receipt_id` and `provider_name`, missing potential regressions in `event_type`, `external_reference`, `parsed_payload`, `payload_hash`, `received_at`, and `metadata`. For Kafka, "partition key" was ambiguous because the test didn't actually check the consumed record key. **Fix:** the test pattern is now `assert_eq!(received, event)` for full `NormalizedEvent` equality, plus explicit backend routing assertions (Kafka `BorrowedMessage::key()`, SQS FIFO group/dedup, Redis stream key). The "Test depth" section enumerates all backend-specific routing assertions.

**5. Premature `OnceCell` optimization + hardcoded `127.0.0.1`.**
The original spec recommended sharing one broker per test file via `OnceCell`. With one round-trip test per crate this adds container-lifetime complexity for zero benefit. The Redis example also hardcoded `127.0.0.1`, which breaks under rootless Docker, podman, and remote Docker socket setups. **Fix:** dropped the `OnceCell` recommendation entirely (one container per test, period), and the test code uses `container.get_host().await` instead of hardcoding the loopback address.

### Net impact

- **Effort:** +1 day total (3.5 → 4.5), driven by the SQS bootstrap (~+0.5d) and the CI split (~+0.5d). All other fixes are minor edits.
- **Architecture:** unchanged. None of the findings invalidated a core decision — the Redis emitter shape, the testcontainers choice, the redis-rs client choice, and the test depth all stand.
- **Risk profile:** lower. The original spec would have shipped a CI green-light that broke on macOS or an SQS test that flaked on missing credentials. Both are now closed.
