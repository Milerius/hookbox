//! Cucumber step definitions for the core BDD scenarios.
//!
//! All step definitions share the 7 steps exercised by the feature files in
//! `scenario-tests/features/core/`. No speculative machinery is added here —
//! additional steps belong to future tasks when the corresponding feature files
//! are written.

#![expect(clippy::panic, reason = "panic is acceptable in BDD test assertions")]
#![expect(
    clippy::unused_async,
    reason = "cucumber step functions must be async"
)]

use bytes::Bytes;
use cucumber::{given, then, when};

use hookbox::dedupe::InMemoryRecentDedupe;
use hookbox::pipeline::HookboxPipeline;
use hookbox::state::IngestResult;

use crate::world::{FailVerifier, IngestWorld, MemoryStorage, PassVerifier};

// ── Given ────────────────────────────────────────────────────────────────

#[given(expr = "a pipeline with a passing verifier for {string}")]
async fn given_pipeline_with_passing_verifier(world: &mut IngestWorld, provider: String) {
    let pipeline = HookboxPipeline::builder()
        .storage(MemoryStorage::new())
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
    let pipeline = HookboxPipeline::builder()
        .storage(MemoryStorage::new())
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
    let pipeline = HookboxPipeline::builder()
        .storage(MemoryStorage::new())
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter_names(vec![])
        .build();
    world.set_pipeline(pipeline);
}

// ── When ─────────────────────────────────────────────────────────────────

#[when(expr = "I ingest a webhook from {string} with body {string}")]
#[expect(clippy::expect_used, reason = "test assertion — ingest failure is fatal")]
async fn when_ingest_webhook(world: &mut IngestWorld, provider: String, body: String) {
    world
        .ingest(&provider, Bytes::from(body))
        .await
        .expect("ingest failed");
}

// ── Then ─────────────────────────────────────────────────────────────────

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
