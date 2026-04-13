//! Cucumber step definitions for the core BDD scenarios.
//!
//! All step definitions share the steps exercised by the feature files in
//! `scenario-tests/features/core/`. No speculative machinery is added here —
//! additional steps belong to future tasks when the corresponding feature files
//! are written.

#![expect(clippy::panic, reason = "panic is acceptable in BDD test assertions")]
#![expect(clippy::unused_async, reason = "cucumber step functions must be async")]

use std::time::Duration;

use bytes::Bytes;
use chrono::Utc;
use cucumber::{given, then, when};

use hookbox::dedupe::InMemoryRecentDedupe;
use hookbox::pipeline::HookboxPipeline;
use hookbox::state::{IngestResult, ProcessingState, RetryPolicy};
use hookbox::transitions::compute_backoff;

use crate::fake_emitter::EmitterBehavior;
use crate::world::{FailVerifier, IngestWorld, MemoryStorage, PassVerifier, SharedMemoryStorage};

// ── Given — original ─────────────────────────────────────────────────────

#[given(expr = "a pipeline with a passing verifier for {string}")]
async fn given_pipeline_with_passing_verifier(world: &mut IngestWorld, provider: String) {
    let storage = SharedMemoryStorage::new(MemoryStorage::new());
    world.storage_handle = Some(storage.clone());
    let pipeline = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter_names(vec![])
        .verifier(PassVerifier {
            provider: provider.clone(),
        })
        .build();
    world.set_pipeline(pipeline);
}

#[given(expr = "a pipeline with a failing verifier for {string}")]
async fn given_pipeline_with_failing_verifier(world: &mut IngestWorld, provider: String) {
    let storage = SharedMemoryStorage::new(MemoryStorage::new());
    world.storage_handle = Some(storage.clone());
    let pipeline = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter_names(vec![])
        .verifier(FailVerifier {
            provider: provider.clone(),
        })
        .build();
    world.set_pipeline(pipeline);
}

#[given("a pipeline with no verifiers")]
async fn given_pipeline_with_no_verifiers(world: &mut IngestWorld) {
    let storage = SharedMemoryStorage::new(MemoryStorage::new());
    world.storage_handle = Some(storage.clone());
    let pipeline = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter_names(vec![])
        .build();
    world.set_pipeline(pipeline);
}

// ── Given — fan-out / derived state ──────────────────────────────────────

#[given(expr = "the pipeline is configured with emitters {string}")]
async fn given_pipeline_with_emitters(world: &mut IngestWorld, emitters_csv: String) {
    let names: Vec<String> = emitters_csv
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    world.build_pipeline_with_emitters(&names);
}

#[given(expr = "emitter {string} permanently fails")]
async fn given_emitter_permanently_fails(world: &mut IngestWorld, name: String) {
    world.set_emitter_behavior(&name, EmitterBehavior::AlwaysFail);
}

#[given(expr = "emitter {string} is paused")]
async fn given_emitter_is_paused(world: &mut IngestWorld, name: String) {
    // A very long delay simulates a paused emitter — deliveries won't complete
    // within the BDD scenario so the receipt stays in Stored state.
    world.set_emitter_behavior(&name, EmitterBehavior::Slow(Duration::from_secs(3600)));
}

// ── Given — backoff arithmetic ────────────────────────────────────────────

#[given(
    expr = "a retry policy with initial_backoff={int}s and multiplier={float} and jitter={float}"
)]
async fn given_retry_policy_with_jitter(
    world: &mut IngestWorld,
    initial_backoff: u64,
    multiplier: f64,
    jitter: f64,
) {
    world.scratch_retry_policy = Some(RetryPolicy {
        max_attempts: 10,
        initial_backoff: Duration::from_secs(initial_backoff),
        max_backoff: Duration::from_secs(u64::MAX / 2),
        backoff_multiplier: multiplier,
        jitter,
    });
}

#[given(
    expr = "a retry policy with initial_backoff={int}s and multiplier={float} and max_backoff={int}s"
)]
async fn given_retry_policy_with_max_backoff(
    world: &mut IngestWorld,
    initial_backoff: u64,
    multiplier: f64,
    max_backoff: u64,
) {
    world.scratch_retry_policy = Some(RetryPolicy {
        max_attempts: 10,
        initial_backoff: Duration::from_secs(initial_backoff),
        max_backoff: Duration::from_secs(max_backoff),
        backoff_multiplier: multiplier,
        jitter: 0.0,
    });
}

// ── When — original ───────────────────────────────────────────────────────

#[when(expr = "I ingest a webhook from {string} with body {string}")]
#[expect(
    clippy::expect_used,
    reason = "test assertion — ingest failure is fatal"
)]
async fn when_ingest_webhook(world: &mut IngestWorld, provider: String, body: String) {
    world
        .ingest(&provider, Bytes::from(body))
        .await
        .expect("ingest failed");
}

// ── When — fan-out / derived state ────────────────────────────────────────

#[when("a valid webhook is ingested")]
#[expect(
    clippy::expect_used,
    reason = "test assertion — ingest failure is fatal"
)]
async fn when_valid_webhook_ingested(world: &mut IngestWorld) {
    world
        .ingest("stripe", Bytes::from(r#"{"event":"test"}"#))
        .await
        .expect("ingest failed");
}

#[when("the same webhook is ingested again")]
#[expect(
    clippy::expect_used,
    reason = "test assertion — ingest failure is fatal"
)]
async fn when_same_webhook_ingested_again(world: &mut IngestWorld) {
    // Same body → same dedupe key → duplicate.
    world
        .ingest("stripe", Bytes::from(r#"{"event":"test"}"#))
        .await
        .expect("ingest failed");
}

#[when("all deliveries complete successfully")]
async fn when_all_deliveries_complete(world: &mut IngestWorld) {
    world.run_deliveries_to_completion().await;
}

#[when("workers run until all retries exhausted")]
async fn when_workers_run_until_exhausted(world: &mut IngestWorld) {
    world.run_until_exhausted().await;
}

// ── When — backoff arithmetic ─────────────────────────────────────────────

#[when("an emitter fails for the first time")]
async fn when_emitter_fails_first_time(world: &mut IngestWorld) {
    world.scratch_attempt = 1;
}

#[when(expr = "an emitter has failed {int} times")]
async fn when_emitter_has_failed_n_times(world: &mut IngestWorld, n: i32) {
    world.scratch_attempt = n;
}

// ── Then — original ───────────────────────────────────────────────────────

#[then(expr = "the result should be {string}")]
async fn then_result_should_be(world: &mut IngestWorld, expected: String) {
    assert_eq!(world.results.len(), 1, "expected exactly one result");
    assert_result_matches(&world.results[0], &expected);
}

#[then(expr = "the first result should be {string}")]
async fn then_first_result_should_be(world: &mut IngestWorld, expected: String) {
    assert!(
        !world.results.is_empty(),
        "expected at least one result, got {}",
        world.results.len()
    );
    assert_result_matches(&world.results[0], &expected);
}

#[then(expr = "the second result should be {string}")]
async fn then_second_result_should_be(world: &mut IngestWorld, expected: String) {
    assert!(
        world.results.len() >= 2,
        "expected at least two results, got {}",
        world.results.len()
    );
    assert_result_matches(&world.results[1], &expected);
}

// ── Then — fan-out ────────────────────────────────────────────────────────

#[then(regex = r#"^emitter "([^"]+)" receives (\d+) events?$"#)]
async fn then_emitter_receives_n_events(world: &mut IngestWorld, name: String, expected: usize) {
    let emitter = world
        .emitter(&name)
        .unwrap_or_else(|| panic!("emitter {name:?} not registered"));
    let got = emitter.received_count();
    assert_eq!(
        got, expected,
        "emitter {name:?}: expected {expected} event(s), got {got}"
    );
}

#[then(regex = r"^the receipt has (\d+) delivery rows?$")]
async fn then_receipt_has_n_delivery_rows(world: &mut IngestWorld, expected: usize) {
    let rows = world.last_receipt_deliveries();
    assert_eq!(
        rows.len(),
        expected,
        "expected {expected} delivery row(s), got {}",
        rows.len()
    );
}

// ── Then — derived state ──────────────────────────────────────────────────

#[then(expr = "the receipt processing_state is {string}")]
async fn then_receipt_processing_state_is(world: &mut IngestWorld, expected: String) {
    let state = world.last_receipt_state();
    let label = match state {
        ProcessingState::Received => "received",
        ProcessingState::Verified => "verified",
        ProcessingState::VerificationFailed => "verification_failed",
        ProcessingState::Duplicate => "duplicate",
        ProcessingState::Stored => "stored",
        ProcessingState::Emitted => "emitted",
        ProcessingState::Processed => "processed",
        ProcessingState::EmitFailed => "emit_failed",
        ProcessingState::DeadLettered => "dead_lettered",
        ProcessingState::Replayed => "replayed",
    };
    assert_eq!(
        label, expected,
        "expected processing_state {expected:?}, got {label:?}"
    );
}

// ── Then — backoff arithmetic ─────────────────────────────────────────────

#[then(expr = "the next_attempt_at is approximately {int} seconds in the future")]
#[expect(
    clippy::expect_used,
    reason = "test assertion — missing policy is a fatal scenario error"
)]
async fn then_next_attempt_at_approximately(world: &mut IngestWorld, expected_secs: u64) {
    let policy = world
        .scratch_retry_policy
        .as_ref()
        .expect("scratch_retry_policy must be set before this step");
    let before = Utc::now();
    let backoff = compute_backoff(world.scratch_attempt, policy);
    let after = Utc::now();

    // The computed backoff should be within ±1 second of expected.
    let expected = Duration::from_secs(expected_secs);
    let tolerance = Duration::from_secs(1);
    // Use saturating sub to avoid underflow when backoff < tolerance.
    let lower = expected.saturating_sub(tolerance);
    let upper = expected + tolerance;

    assert!(
        backoff >= lower && backoff <= upper,
        "backoff {backoff:?} not within [{lower:?}, {upper:?}] of expected {expected:?} \
         (attempt={}, policy={policy:?}, elapsed={:?})",
        world.scratch_attempt,
        after - before,
    );
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Assert that an [`IngestResult`] matches a human-readable label.
fn assert_result_matches(result: &IngestResult, expected: &str) {
    match expected {
        "accepted" => {
            assert!(
                matches!(result, IngestResult::Accepted { .. }),
                "expected Accepted, got {result:?}"
            );
        }
        "duplicate" => {
            assert!(
                matches!(result, IngestResult::Duplicate { .. }),
                "expected Duplicate, got {result:?}"
            );
        }
        "verification_failed" => {
            assert!(
                matches!(result, IngestResult::VerificationFailed { .. }),
                "expected VerificationFailed, got {result:?}"
            );
        }
        other => panic!("unknown expected result: {other}"),
    }
}
