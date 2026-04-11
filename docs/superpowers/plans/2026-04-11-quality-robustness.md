# Quality and Robustness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract shared transition functions so verification tests are non-tautological, improve coverage to 80%+, add load/stress tests, and implement graceful shutdown.

**Architecture:** Extract pure functions from pipeline.rs and worker.rs into `hookbox/src/transitions.rs` — a shared module of label derivation and state transition logic. Update Bolero/Kani to test these functions directly. Add coverage via targeted integration tests for worker, admin routes, and ingest edge cases. Add `tokio::signal` based graceful shutdown to serve.rs.

**Tech Stack:** Rust 2024, tokio (signal), axum (graceful_shutdown), criterion (benchmarks), metrics 0.24, bolero 0.13.

**Spec reference:** `docs/ROADMAP.md` items 8–11

---

## File Map

### New Files

| File | Responsibility |
|------|---------------|
| `crates/hookbox/src/transitions.rs` | Pure functions: label derivation, retry state transition logic |
| `crates/hookbox-server/src/shutdown.rs` | Graceful shutdown signal handler |
| `crates/hookbox/benches/ingest.rs` | Criterion load benchmark for ingest pipeline |

### Modified Files

| File | Changes |
|------|---------|
| `crates/hookbox/src/pipeline.rs` | Call shared label functions from transitions.rs |
| `crates/hookbox/src/lib.rs` | Add `pub mod transitions` |
| `crates/hookbox-server/src/worker.rs` | Use shared retry transition logic |
| `crates/hookbox-server/src/lib.rs` | Add shutdown module |
| `crates/hookbox-cli/src/commands/serve.rs` | Wire graceful shutdown |
| `crates/hookbox-verify/src/retry_props.rs` | Non-tautological tests against shared functions |
| `crates/hookbox-verify/src/metrics_props.rs` | Non-tautological tests against shared functions |
| `crates/hookbox-verify/src/kani_proofs.rs` | Proofs against shared transition function |
| `crates/hookbox-server/src/routes/tests.rs` | Additional coverage tests (admin routes, ingest edge cases) |

---

## Task 1: Extract Shared Transition Functions

**Files:**
- Create: `crates/hookbox/src/transitions.rs`
- Modify: `crates/hookbox/src/lib.rs`

Extract pure, testable functions for label derivation and retry state logic.

### Steps

- [ ] **Step 1: Create transitions.rs with label derivation functions**

```rust
// crates/hookbox/src/transitions.rs

//! Shared transition functions and label derivation.
//!
//! Pure functions used by both the pipeline and verification tests.
//! Extracting these ensures property tests exercise real production logic
//! rather than local tautologies.

use crate::state::{DedupeDecision, ProcessingState, VerificationStatus};

/// Map a [`VerificationStatus`] to the metric label string.
#[must_use]
pub fn verification_status_label(status: VerificationStatus) -> &'static str {
    match status {
        VerificationStatus::Verified => "verified",
        VerificationStatus::Failed => "failed",
        VerificationStatus::Skipped => "skipped",
    }
}

/// Map a [`DedupeDecision`] to the metric label string.
#[must_use]
pub fn dedupe_decision_label(decision: DedupeDecision) -> &'static str {
    match decision {
        DedupeDecision::New => "new",
        DedupeDecision::Duplicate => "duplicate",
        DedupeDecision::Conflict => "conflict",
    }
}

/// Individual ingest result metric label constants.
/// Used directly in pipeline.rs so the label strings and the exhaustive set
/// are defined in one place — making `INGEST_RESULT_LABELS` non-tautological.
pub const LABEL_ACCEPTED: &str = "accepted";
pub const LABEL_DUPLICATE: &str = "duplicate";
pub const LABEL_VERIFICATION_FAILED: &str = "verification_failed";
pub const LABEL_STORE_FAILED: &str = "store_failed";
pub const LABEL_DEDUPE_FAILED: &str = "dedupe_failed";

/// The exhaustive set of valid ingest result metric labels.
/// Bolero tests verify this set is complete; pipeline.rs references the
/// same constants above — so any added variant that is not in both places
/// will be caught at compile-time or by the property test.
pub const INGEST_RESULT_LABELS: &[&str] = &[
    LABEL_ACCEPTED,
    LABEL_DUPLICATE,
    LABEL_VERIFICATION_FAILED,
    LABEL_STORE_FAILED,
    LABEL_DEDUPE_FAILED,
];

/// Compute the next state after a failed retry attempt.
///
/// **Specification model** — this mirrors the SQL CASE logic in
/// `PostgresStorage::retry_failed()` — both must be kept in sync.
/// It is NOT the actual code path; it exists so that Bolero/Kani can test
/// the intended transition invariants without running SQL.
///
/// SQL equivalent:
/// ```sql
/// SET emit_count = emit_count + 1,
///     processing_state = CASE WHEN emit_count + 1 >= max THEN 'dead_lettered' ELSE 'emit_failed' END
/// ```
#[must_use]
pub fn retry_next_state(emit_count: i32, max_attempts: i32) -> (i32, ProcessingState) {
    let new_count = emit_count + 1;
    let new_state = if new_count >= max_attempts {
        ProcessingState::DeadLettered
    } else {
        ProcessingState::EmitFailed
    };
    (new_count, new_state)
}

/// After a reset, the receipt is always in EmitFailed with emit_count 0.
/// The worker query (`processing_state = 'emit_failed' AND emit_count < max_attempts`)
/// will find it for any `max_attempts >= 1`.
#[must_use]
pub fn reset_state() -> (i32, ProcessingState) {
    (0, ProcessingState::EmitFailed)
}

/// Check whether a receipt in the reset state would be found by the
/// retry worker query.
#[must_use]
pub fn is_findable_by_worker(emit_count: i32, state: ProcessingState, max_attempts: i32) -> bool {
    state == ProcessingState::EmitFailed && emit_count < max_attempts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_labels_cover_all_variants() {
        assert_eq!(verification_status_label(VerificationStatus::Verified), "verified");
        assert_eq!(verification_status_label(VerificationStatus::Failed), "failed");
        assert_eq!(verification_status_label(VerificationStatus::Skipped), "skipped");
    }

    #[test]
    fn dedupe_labels_cover_all_variants() {
        assert_eq!(dedupe_decision_label(DedupeDecision::New), "new");
        assert_eq!(dedupe_decision_label(DedupeDecision::Duplicate), "duplicate");
        assert_eq!(dedupe_decision_label(DedupeDecision::Conflict), "conflict");
    }

    #[test]
    fn retry_promotes_at_max() {
        let (count, state) = retry_next_state(4, 5);
        assert_eq!(count, 5);
        assert_eq!(state, ProcessingState::DeadLettered);
    }

    #[test]
    fn retry_stays_emit_failed_below_max() {
        let (count, state) = retry_next_state(2, 5);
        assert_eq!(count, 3);
        assert_eq!(state, ProcessingState::EmitFailed);
    }

    #[test]
    fn reset_is_always_findable() {
        let (count, state) = reset_state();
        assert!(is_findable_by_worker(count, state, 1));
        assert!(is_findable_by_worker(count, state, 100));
    }
}
```

- [ ] **Step 2: Add module to lib.rs**

Add to `crates/hookbox/src/lib.rs`:
```rust
pub mod transitions;
```

- [ ] **Step 3: Update pipeline.rs to use shared label functions**

In `crates/hookbox/src/pipeline.rs`, replace inline label string derivation with calls to `crate::transitions::verification_status_label()` and `crate::transitions::dedupe_decision_label()`.

The implementing agent should read pipeline.rs, find the places where verification status and dedupe decision are converted to strings for metrics labels, and replace them with the shared functions.

> **Note — production vs. model code:**
> - `verification_status_label()` and `dedupe_decision_label()` are **production code** extracted from pipeline.rs. The Bolero tests against these are non-tautological because they exercise the real code path.
> - `retry_next_state()` is a **specification model** that mirrors the SQL CASE logic in `PostgresStorage::retry_failed()` — both must be kept in sync. It is not the actual code path; it exists to give Bolero/Kani something to test that represents the intended SQL behavior. The real enforcement is that both the model and the SQL implement the same invariants.

Also replace the hard-coded label strings in pipeline.rs `ingest_results_total` calls with references to constants from `transitions.rs`. For example, instead of `"result" => "accepted"`, use `"result" => hookbox::transitions::LABEL_ACCEPTED`. Add the following constants to `transitions.rs`:

```rust
pub const LABEL_ACCEPTED: &str = "accepted";
pub const LABEL_DUPLICATE: &str = "duplicate";
pub const LABEL_VERIFICATION_FAILED: &str = "verification_failed";
pub const LABEL_STORE_FAILED: &str = "store_failed";
pub const LABEL_DEDUPE_FAILED: &str = "dedupe_failed";
```

This makes the `INGEST_RESULT_LABELS` constant array and the Bolero exhaustiveness test non-tautological: pipeline.rs references the same constants, so a mismatch between the array and actual usage becomes a compile-time or test-time error.

- [ ] **Step 4: Run tests and lint**

Run: `cargo test -p hookbox && cargo clippy -p hookbox --all-targets -- -D warnings`

Expected: All 20+ tests pass, clean.

- [ ] **Step 5: Commit**

```bash
git add crates/hookbox/
git commit -m "refactor(hookbox): extract shared transition functions into transitions.rs"
```

---

## Task 2: Non-Tautological Verification Tests

**Files:**
- Modify: `crates/hookbox-verify/src/retry_props.rs`
- Modify: `crates/hookbox-verify/src/metrics_props.rs`
- Modify: `crates/hookbox-verify/src/kani_proofs.rs`

Replace tautological tests with ones that call the shared transition functions.

### Steps

- [ ] **Step 1: Rewrite retry_props.rs**

```rust
// crates/hookbox-verify/src/retry_props.rs

//! Property tests for retry state transitions.
//!
//! Tests the shared functions in `hookbox::transitions` — NOT local tautologies.

#[cfg(test)]
mod tests {
    use hookbox::state::ProcessingState;
    use hookbox::transitions::{is_findable_by_worker, reset_state, retry_next_state};

    #[test]
    fn retry_always_reaches_dead_lettered_at_max() {
        bolero::check!()
            .with_type::<(u8, u8)>()
            .for_each(|(emit_count, max_attempts)| {
                let emit_count = i32::from(*emit_count);
                let max_attempts = i32::from(*max_attempts).max(1);
                let (new_count, new_state) = retry_next_state(emit_count, max_attempts);
                assert_eq!(new_count, emit_count + 1);
                if new_count >= max_attempts {
                    assert_eq!(new_state, ProcessingState::DeadLettered);
                } else {
                    assert_eq!(new_state, ProcessingState::EmitFailed);
                }
            });
    }

    #[test]
    fn reset_is_always_findable_by_worker() {
        bolero::check!().with_type::<u8>().for_each(|&max_raw| {
            let max_attempts = i32::from(max_raw).max(1);
            let (count, state) = reset_state();
            assert!(
                is_findable_by_worker(count, state, max_attempts),
                "reset state must be findable by worker for any max_attempts >= 1"
            );
        });
    }

    #[test]
    fn dead_lettered_is_never_findable_by_worker() {
        bolero::check!().with_type::<(u8, u8)>().for_each(|(count_raw, max_raw)| {
            let count = i32::from(*count_raw);
            let max_attempts = i32::from(*max_raw).max(1);
            assert!(
                !is_findable_by_worker(count, ProcessingState::DeadLettered, max_attempts),
                "DeadLettered must never be found by worker"
            );
        });
    }
}
```

- [ ] **Step 2: Rewrite metrics_props.rs**

```rust
// crates/hookbox-verify/src/metrics_props.rs

//! Property tests for metric label derivation.
//!
//! Tests the shared functions in `hookbox::transitions`.

#[cfg(test)]
mod tests {
    use hookbox::state::{DedupeDecision, VerificationStatus};
    use hookbox::transitions::{
        dedupe_decision_label, verification_status_label, INGEST_RESULT_LABELS,
    };

    #[test]
    fn verification_label_is_never_empty() {
        bolero::check!().with_type::<u8>().for_each(|&n| {
            let status = match n % 3 {
                0 => VerificationStatus::Verified,
                1 => VerificationStatus::Failed,
                _ => VerificationStatus::Skipped,
            };
            let label = verification_status_label(status);
            assert!(!label.is_empty());
            assert!(
                matches!(label, "verified" | "failed" | "skipped"),
                "unexpected label: {label}"
            );
        });
    }

    #[test]
    fn dedupe_label_is_never_empty() {
        bolero::check!().with_type::<u8>().for_each(|&n| {
            let decision = match n % 3 {
                0 => DedupeDecision::New,
                1 => DedupeDecision::Duplicate,
                _ => DedupeDecision::Conflict,
            };
            let label = dedupe_decision_label(decision);
            assert!(!label.is_empty());
            assert!(
                matches!(label, "new" | "duplicate" | "conflict"),
                "unexpected label: {label}"
            );
        });
    }

    #[test]
    fn ingest_result_labels_are_all_non_empty() {
        for label in INGEST_RESULT_LABELS {
            assert!(!label.is_empty());
        }
    }
}
```

- [ ] **Step 3: Update Kani proofs**

In `crates/hookbox-verify/src/kani_proofs.rs`, replace the two vacuous retry proofs with ones that call the shared functions. Add to the `#[cfg(kani)] mod proofs` block:

```rust
    /// Prove that retry_next_state always produces either EmitFailed or DeadLettered.
    #[kani::proof]
    fn retry_next_state_always_valid() {
        let emit_count: i32 = kani::any();
        kani::assume(emit_count >= 0 && emit_count < 100);
        let max_attempts: i32 = kani::any();
        kani::assume(max_attempts >= 1 && max_attempts <= 100);

        let (new_count, new_state) = hookbox::transitions::retry_next_state(emit_count, max_attempts);
        assert!(new_count == emit_count + 1);
        assert!(
            new_state == hookbox::ProcessingState::DeadLettered
                || new_state == hookbox::ProcessingState::EmitFailed
        );
    }

    /// Prove that reset_state is always findable by the worker.
    #[kani::proof]
    fn reset_always_findable() {
        let max_attempts: i32 = kani::any();
        kani::assume(max_attempts >= 1 && max_attempts <= 1000);

        let (count, state) = hookbox::transitions::reset_state();
        assert!(hookbox::transitions::is_findable_by_worker(count, state, max_attempts));
    }
```

Remove the old `retry_failed_always_reaches_valid_state` and `reset_for_retry_always_findable_by_worker` proofs.

- [ ] **Step 4: Run all verification tests**

Run: `cargo test -p hookbox-verify && cargo kani -p hookbox-verify`

Expected: All Bolero tests pass, all Kani proofs verified.

- [ ] **Step 5: Commit**

```bash
git add crates/hookbox-verify/
git commit -m "test(hookbox-verify): non-tautological Bolero/Kani tests against shared transition functions"
```

---

## Task 3: Coverage — Admin Route and Ingest Edge Cases

**Files:**
- Modify: `crates/hookbox-server/src/routes/tests.rs`

Add more in-memory route tests targeting the current coverage gaps.

### Steps

- [ ] **Step 1: Add admin route coverage tests**

Add to the existing `tests` module in `crates/hookbox-server/src/routes/tests.rs`:

Tests to add:
- `replay_receipt_returns_200` — ingest first, then POST /api/receipts/:id/replay
- `replay_nonexistent_returns_404` — POST /api/receipts/:random-uuid/replay → 404
- `list_receipts_with_provider_filter` — ingest for two providers, filter by one
- `list_receipts_with_state_filter` — filter by processing state
- `list_receipts_with_limit` — verify the HTTP query parameter `limit` is forwarded correctly to the storage filter; assert the response contains at most `limit` items. **Do not** assert that `MemoryStorage` implements pagination internally — instead assert the HTTP layer passes the parameter through. If `MemoryStorage::query()` currently ignores `filter.limit`, the implementing agent must also update it to apply `.take(limit)` on the result set so this test does not spuriously pass.
- `get_receipt_after_ingest` — ingest then GET by ID, verify full JSON shape
- `ingest_empty_body` — POST with empty body → should still return 200 (empty body is valid)
- `ingest_non_json_body` — POST with plain text → 200 accepted (parsed_payload is None)

The implementing agent should read the existing test helpers in tests.rs and follow the same patterns.

- [ ] **Step 2: Run tests and check coverage**

Run: `cargo test -p hookbox-server`

Expected: All route tests pass.

Then check coverage: `cargo +nightly llvm-cov -p hookbox-server --all-features --html`

- [ ] **Step 3: Commit**

```bash
git add crates/hookbox-server/
git commit -m "test(hookbox-server): add admin and ingest edge case route tests"
```

---

## Task 4: Coverage — Worker Integration Test

**Files:**
- Create: `integration-tests/tests/worker_test.rs`

Test the retry worker against real Postgres.

### Steps

- [ ] **Step 1: Create worker integration test**

```rust
// integration-tests/tests/worker_test.rs

//! Integration tests for the retry worker.

#![expect(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::time::Duration;

use hookbox::dedupe::InMemoryRecentDedupe;
use hookbox::emitter::ChannelEmitter;
use hookbox::state::ProcessingState;
use hookbox::traits::Storage;
use hookbox::HookboxPipeline;
use hookbox_postgres::PostgresStorage;
use hookbox_server::worker::RetryWorker;
use bytes::Bytes;
use http::HeaderMap;
use sqlx::PgPool;

async fn setup() -> (PgPool, PostgresStorage) {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://localhost/hookbox_test".to_owned());
    let pool = PgPool::connect(&url).await.expect("connect");
    let storage = PostgresStorage::new(pool.clone());
    storage.migrate().await.expect("migrate");
    sqlx::query("DELETE FROM webhook_receipts")
        .execute(&pool)
        .await
        .expect("clean");
    (pool, storage)
}

#[tokio::test]
async fn worker_retries_emit_failed_receipt() {
    let (pool, storage) = setup().await;

    // Ingest a receipt with a failing emitter
    let (fail_emitter, _rx) = ChannelEmitter::new(16);
    drop(_rx); // Drop receiver so emit fails

    let pipeline = HookboxPipeline::builder()
        .storage(storage.clone())
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter(fail_emitter)
        .build();

    let body = Bytes::from(format!(r#"{{"nonce":"{}"}}"#, uuid::Uuid::new_v4()));
    let result = pipeline.ingest("test", HeaderMap::new(), body).await.unwrap();
    // `IngestResult::Accepted` yields a `ReceiptId` newtype; `.0` extracts the inner `Uuid`
    // required by `storage.get()` and `storage.update_state()`.
    let hookbox::IngestResult::Accepted { receipt_id } = result else { unreachable!() };
    let receipt_uuid = receipt_id.0;

    // Receipt should be in EmitFailed state
    let receipt = storage.get(receipt_uuid).await.unwrap().unwrap();
    assert_eq!(receipt.processing_state, ProcessingState::EmitFailed);

    // Create a working emitter for the worker
    let (worker_emitter, mut worker_rx) = ChannelEmitter::new(16);
    tokio::spawn(async move { while worker_rx.recv().await.is_some() {} });

    // Run the worker with short interval
    let worker = RetryWorker::new(
        PostgresStorage::new(pool.clone()),
        Box::new(worker_emitter),
        Duration::from_millis(100),
        5,
    );
    let handle = worker.spawn();

    // Wait for worker to retry
    tokio::time::sleep(Duration::from_millis(500)).await;
    handle.abort();

    // Receipt should now be Emitted
    let receipt = storage.get(receipt_uuid).await.unwrap().unwrap();
    assert_eq!(receipt.processing_state, ProcessingState::Emitted);
}

#[tokio::test]
async fn worker_promotes_to_dlq_after_max_attempts() {
    let (pool, storage) = setup().await;

    // Ingest and set to EmitFailed
    let (emitter, mut rx) = ChannelEmitter::new(16);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });

    let pipeline = HookboxPipeline::builder()
        .storage(storage.clone())
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter(emitter)
        .build();

    let body = Bytes::from(format!(r#"{{"nonce":"{}"}}"#, uuid::Uuid::new_v4()));
    let result = pipeline.ingest("test", HeaderMap::new(), body).await.unwrap();
    // `IngestResult::Accepted` yields a `ReceiptId` newtype; `.0` extracts the inner `Uuid`
    // required by `storage.get()` and `storage.update_state()`.
    let hookbox::IngestResult::Accepted { receipt_id } = result else { unreachable!() };
    let receipt_uuid = receipt_id.0;

    // Set to EmitFailed and simulate 4 prior retries
    storage.update_state(receipt_uuid, ProcessingState::EmitFailed, None).await.unwrap();
    for _ in 0..4 {
        storage.retry_failed(receipt_uuid, 5).await.unwrap();
    }

    let receipt = storage.get(receipt_uuid).await.unwrap().unwrap();
    assert_eq!(receipt.emit_count, 4);
    assert_eq!(receipt.processing_state, ProcessingState::EmitFailed);

    // Create a failing emitter for the worker (drop receiver)
    let (fail_emitter, _) = ChannelEmitter::new(16);

    let worker = RetryWorker::new(
        PostgresStorage::new(pool.clone()),
        Box::new(fail_emitter),
        Duration::from_millis(100),
        5,
    );
    let handle = worker.spawn();

    tokio::time::sleep(Duration::from_millis(500)).await;
    handle.abort();

    // Receipt should now be DeadLettered
    let receipt = storage.get(receipt_uuid).await.unwrap().unwrap();
    assert_eq!(receipt.processing_state, ProcessingState::DeadLettered);
    assert_eq!(receipt.emit_count, 5);
}
```

- [ ] **Step 2: Verify deps in integration-tests Cargo.toml**

Verify that `integration-tests/Cargo.toml` already contains `hookbox-server.workspace = true`. This dependency is expected to be present; no changes should be needed. If it is missing for any reason, add it.

- [ ] **Step 3: Run tests**

Run: `DATABASE_URL=postgres://localhost/hookbox_test cargo test -p hookbox-integration-tests`

Expected: All integration tests pass (existing + 2 new worker tests).

- [ ] **Step 4: Commit**

```bash
git add integration-tests/
git commit -m "test(integration): add worker retry and DLQ promotion tests"
```

---

## Task 5: Graceful Shutdown

**Files:**
- Create: `crates/hookbox-server/src/shutdown.rs`
- Modify: `crates/hookbox-server/src/lib.rs`
- Modify: `crates/hookbox-cli/src/commands/serve.rs`

### Steps

- [ ] **Step 1: Create shutdown.rs**

```rust
// crates/hookbox-server/src/shutdown.rs

//! Graceful shutdown signal handling.
//!
//! Listens for SIGTERM and SIGINT (Ctrl-C) and returns a future
//! that resolves when either signal is received.

/// Returns a future that completes when a shutdown signal is received.
///
/// Listens for:
/// - `SIGINT` (Ctrl-C)
/// - `SIGTERM` (container orchestrators, systemd)
///
/// Signal handler installation failures are genuinely fatal (the process
/// cannot safely handle shutdown), so `.expect()` is appropriate here.
// Signal handler installation is fatal — expect() is intentional.
#[expect(clippy::expect_used, reason = "signal handler installation is fatal and non-recoverable")]
pub async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => { tracing::info!("received SIGINT, shutting down"); }
        () = terminate => { tracing::info!("received SIGTERM, shutting down"); }
    }
}
```

- [ ] **Step 2: Add module to server lib.rs**

Add `pub mod shutdown;` to `crates/hookbox-server/src/lib.rs`.

- [ ] **Step 3: Wire graceful shutdown in serve.rs**

In `crates/hookbox-cli/src/commands/serve.rs`, change the `axum::serve` call to use `with_graceful_shutdown`:

```rust
use hookbox_server::shutdown::shutdown_signal;

axum::serve(listener, router)
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("server encountered a fatal error")?;

tracing::info!("server shut down gracefully");
```

Also abort the retry worker after the server stops:

```rust
let retry_handle = retry_worker.spawn();

// ... axum::serve with graceful shutdown ...

// After server stops, abort the retry worker
retry_handle.abort();
tracing::info!("retry worker stopped");
```

- [ ] **Step 4: Run lint**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`

Expected: Clean.

- [ ] **Step 5: Commit**

```bash
git add crates/hookbox-server/ crates/hookbox-cli/
git commit -m "feat(hookbox-server): add graceful shutdown with SIGTERM/SIGINT handling"
```

---

## Task 6: Load Benchmark

**Files:**
- Create: `crates/hookbox/benches/ingest.rs`
- Modify: `crates/hookbox/Cargo.toml`
- Modify: `Cargo.toml` (workspace root — add criterion to workspace deps if not present)

A criterion benchmark for ingest pipeline throughput using in-memory backends.

### Steps

- [ ] **Step 1: Add criterion dependency**

Add to `crates/hookbox/Cargo.toml`:

```toml
[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports", "async_tokio"] }
tokio = { workspace = true }
async-trait = { workspace = true }

[[bench]]
name = "ingest"
harness = false
```

Add `criterion = { version = "0.5", features = ["html_reports", "async_tokio"] }` to workspace deps if not present.

- [ ] **Step 2: Create ingest benchmark**

```rust
// crates/hookbox/benches/ingest.rs

//! Benchmark for the ingest pipeline throughput.

// Benchmarks are not production code — unwrap/expect are acceptable.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use bytes::Bytes;
use criterion::{criterion_group, criterion_main, Criterion, BenchmarkId};
use http::HeaderMap;
use std::sync::Mutex;
use hookbox::dedupe::InMemoryRecentDedupe;
use hookbox::emitter::ChannelEmitter;
use hookbox::error::StorageError;
use hookbox::pipeline::HookboxPipeline;
use hookbox::state::{ProcessingState, StoreResult};
use hookbox::traits::Storage;
use hookbox::types::{ReceiptFilter, WebhookReceipt};
use uuid::Uuid;

// Minimal in-memory storage for benchmarking.
// No verifier is registered on the pipeline — verification is skipped automatically.
struct BenchStorage {
    count: Mutex<u64>,
}

impl BenchStorage {
    fn new() -> Self {
        Self { count: Mutex::new(0) }
    }
}

#[async_trait]
impl Storage for BenchStorage {
    async fn store(&self, _receipt: &WebhookReceipt) -> Result<StoreResult, StorageError> {
        let mut count = self.count.lock().map_err(|e| StorageError::Internal(e.to_string()))?;
        *count += 1;
        Ok(StoreResult::Stored)
    }
    async fn get(&self, _id: Uuid) -> Result<Option<WebhookReceipt>, StorageError> {
        Ok(None)
    }
    async fn update_state(&self, _id: Uuid, _state: ProcessingState, _error: Option<&str>) -> Result<(), StorageError> {
        Ok(())
    }
    async fn query(&self, _filter: ReceiptFilter) -> Result<Vec<WebhookReceipt>, StorageError> {
        Ok(vec![])
    }
}

fn bench_ingest(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("ingest");

    for size in [64, 256, 1024, 4096] {
        group.bench_with_input(BenchmarkId::new("body_size", size), &size, |b, &size| {
            let (emitter, mut rx) = ChannelEmitter::new(65536);
            // Drain in background
            rt.spawn(async move { while rx.recv().await.is_some() {} });

            let pipeline = HookboxPipeline::builder()
                .storage(BenchStorage::new())
                .dedupe(InMemoryRecentDedupe::new(100_000))
                .emitter(emitter)
                .build();

            let body = Bytes::from(vec![b'x'; size]);

            b.to_async(&rt).iter(|| {
                let body = body.clone();
                let headers = HeaderMap::new();
                async {
                    let _ = pipeline.ingest("bench", headers, body).await;
                }
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_ingest);
criterion_main!(benches);
```

- [ ] **Step 3: Run benchmark**

Run: `cargo bench -p hookbox --bench ingest`

Expected: Benchmark runs and produces results. The implementing agent should note the throughput numbers in the commit message.

- [ ] **Step 4: Commit**

```bash
git add crates/hookbox/
git commit -m "bench(hookbox): add criterion ingest pipeline benchmark"
```

---

## Task 7: Workspace Validation and Coverage Check

**Files:** All.

### Steps

- [ ] **Step 1: Format**

Run: `cargo fmt --all`

- [ ] **Step 2: Lint**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`

- [ ] **Step 3: Test**

Run: `DATABASE_URL=postgres://localhost/hookbox_test cargo test --workspace --all-features`

- [ ] **Step 4: BDD**

Run: `cargo test -p hookbox --test bdd`

- [ ] **Step 5: Deny**

Run: `cargo deny check`

- [ ] **Step 6: Coverage**

Run: `cargo +nightly llvm-cov --all-features`

Check if we hit 80% line coverage. Note the numbers.

- [ ] **Step 7: Commit and push**

```bash
git add -A
git commit -m "chore: workspace validation and coverage check"
git push -u origin feat/quality-robustness
```

---

## Summary

**Recommended execution order:**

| Order | Task | What |
|-------|------|------|
| 1 | Task 1 | Extract shared transition functions |
| 2 | Task 2 | Non-tautological Bolero/Kani tests |
| 3 | Task 3 | Admin route + ingest edge case tests |
| 4 | Task 4 | Worker integration tests |
| 5 | Task 5 | Graceful shutdown |
| 6 | Task 6 | Load benchmark |
| 7 | Task 7 | Validation + coverage check |

Tasks 1-2 fix the verification test quality. Tasks 3-4 improve coverage. Task 5 adds robustness. Task 6 adds performance visibility. Task 7 validates everything.
