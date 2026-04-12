//! Shared transition functions for the hookbox ingest pipeline.
//!
//! This module provides pure functions for label derivation and retry state
//! logic. These are extracted from the pipeline so that property tests and
//! Kani proofs can reference the same authoritative logic rather than
//! re-implementing it in the test harness (which would make tests tautological).
//!
//! # Synchronisation note
//!
//! [`retry_next_state`] and [`is_findable_by_worker`] **mirror** the SQL
//! `CASE` expressions inside `PostgresStorage::retry_failed`. Any change to
//! the SQL retry logic must be reflected here and vice-versa.

use crate::state::{DedupeDecision, ProcessingState, RetryPolicy, VerificationStatus};
use std::time::Duration;

// ── Ingest result label constants ─────────────────────────────────────────────

/// Metrics label for a successfully accepted webhook.
pub const LABEL_ACCEPTED: &str = "accepted";

/// Metrics label for a webhook that was deduplicated.
pub const LABEL_DUPLICATE: &str = "duplicate";

/// Metrics label for a webhook rejected due to verification failure.
pub const LABEL_VERIFICATION_FAILED: &str = "verification_failed";

/// Metrics label for a webhook that could not be durably stored.
pub const LABEL_STORE_FAILED: &str = "store_failed";

/// Metrics label for a webhook where the dedupe check itself failed.
pub const LABEL_DEDUPE_FAILED: &str = "dedupe_failed";

/// All valid ingest result label strings, in a stable order.
///
/// Useful for exhaustive property tests that must cover every outcome.
pub const INGEST_RESULT_LABELS: &[&str] = &[
    LABEL_ACCEPTED,
    LABEL_DUPLICATE,
    LABEL_VERIFICATION_FAILED,
    LABEL_STORE_FAILED,
    LABEL_DEDUPE_FAILED,
];

// ── Label derivation functions ────────────────────────────────────────────────

/// Map a [`VerificationStatus`] to its canonical metrics label string.
///
/// The returned string is a `'static` slice so it can be used directly in
/// `metrics::counter!` calls without allocation.
#[must_use]
pub fn verification_status_label(status: VerificationStatus) -> &'static str {
    match status {
        VerificationStatus::Verified => "verified",
        VerificationStatus::Failed => "failed",
        VerificationStatus::Skipped => "skipped",
    }
}

/// Map a [`DedupeDecision`] to its canonical metrics label string.
///
/// The returned string is a `'static` slice so it can be used directly in
/// `metrics::counter!` calls without allocation.
#[must_use]
pub fn dedupe_decision_label(decision: DedupeDecision) -> &'static str {
    match decision {
        DedupeDecision::New => "new",
        DedupeDecision::Duplicate => "duplicate",
        DedupeDecision::Conflict => "conflict",
    }
}

// ── Retry state specification model ──────────────────────────────────────────

/// Compute the next `(emit_count, state)` after a failed retry emission.
///
/// This mirrors the SQL `CASE` expression in `PostgresStorage::retry_failed`:
///
/// ```sql
/// CASE
///   WHEN emit_count + 1 >= max_attempts THEN 'dead_lettered'
///   ELSE 'emit_failed'
/// END
/// ```
///
/// Returns `(new_emit_count, new_state)` where `new_emit_count` is always
/// `emit_count + 1`.
#[must_use]
pub fn retry_next_state(emit_count: i32, max_attempts: i32) -> (i32, ProcessingState) {
    let new_count = emit_count.saturating_add(1);
    let new_state = if new_count >= max_attempts {
        ProcessingState::DeadLettered
    } else {
        ProcessingState::EmitFailed
    };
    (new_count, new_state)
}

/// Compute the state after an explicit admin reset.
///
/// Always resets `emit_count` to `0` and transitions the receipt back to
/// [`ProcessingState::EmitFailed`] so the retry worker will pick it up again.
///
/// Returns `(new_emit_count, new_state)`.
#[must_use]
pub fn reset_state() -> (i32, ProcessingState) {
    (0, ProcessingState::EmitFailed)
}

/// Return `true` when the retry worker can pick up this receipt.
///
/// A receipt is findable when:
/// - Its state is [`ProcessingState::EmitFailed`], **and**
/// - Its `emit_count` is strictly less than `max_attempts`.
///
/// This mirrors the SQL `WHERE` clause used by the worker's polling query.
#[must_use]
pub fn is_findable_by_worker(emit_count: i32, state: ProcessingState, max_attempts: i32) -> bool {
    state == ProcessingState::EmitFailed && emit_count < max_attempts
}

// ── Backoff computation ───────────────────────────────────────────────────────

/// Compute the backoff duration for the Nth failed dispatch attempt.
///
/// `attempt` is the post-increment stored `attempt_count` — the number of
/// failed dispatches *including the one that just failed*. The first failure
/// passes `attempt = 1` and gets `initial_backoff`. Passing `attempt = 0`
/// is a programmer error: panics in debug builds, returns `initial_backoff`
/// in release.
///
/// Formula: `base = initial_backoff × multiplier^(attempt-1)`, clamped to
/// `max_backoff`, then multiplied by `1.0 + jitter * rand(-1, 1)`.
#[must_use]
pub fn compute_backoff(attempt: i32, policy: &RetryPolicy) -> Duration {
    debug_assert!(attempt > 0, "attempt must be >= 1; got {attempt}");
    if attempt <= 0 {
        return policy.initial_backoff;
    }
    // attempt >= 1 here (guarded above), so attempt - 1 >= 0 and the
    // subtraction never wraps. `powi` takes i32 and attempt fits in i32.
    let exponent = attempt - 1;
    let base_secs = policy.initial_backoff.as_secs_f64() * policy.backoff_multiplier.powi(exponent);
    let base_secs = base_secs.min(policy.max_backoff.as_secs_f64());

    let jitter_factor = if policy.jitter > 0.0 {
        1.0 + fastrand::f64().mul_add(2.0 * policy.jitter, -policy.jitter)
    } else {
        1.0
    };
    let jittered = base_secs * jitter_factor;

    Duration::from_secs_f64(jittered)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod backoff_tests {
    use super::*;
    use crate::state::RetryPolicy;
    use std::time::Duration;

    fn policy_no_jitter() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 5,
            initial_backoff: Duration::from_secs(30),
            max_backoff: Duration::from_secs(3600),
            backoff_multiplier: 2.0,
            jitter: 0.0,
        }
    }

    #[test]
    fn attempt_1_returns_initial_backoff() {
        let d = compute_backoff(1, &policy_no_jitter());
        assert_eq!(d, Duration::from_secs(30));
    }

    #[test]
    fn attempt_2_doubles() {
        let d = compute_backoff(2, &policy_no_jitter());
        assert_eq!(d, Duration::from_secs(60));
    }

    #[test]
    fn attempt_5_clamped_to_max() {
        let p = RetryPolicy {
            max_attempts: 10,
            initial_backoff: Duration::from_secs(60),
            max_backoff: Duration::from_secs(120),
            backoff_multiplier: 4.0,
            jitter: 0.0,
        };
        // attempt 3: 60 * 4^2 = 960 -> clamped to 120
        let d = compute_backoff(3, &p);
        assert_eq!(d, Duration::from_secs(120));
    }

    #[test]
    fn attempt_0_panics_in_debug() {
        if cfg!(debug_assertions) {
            let result = std::panic::catch_unwind(|| compute_backoff(0, &policy_no_jitter()));
            assert!(result.is_err(), "expected panic for attempt=0 in debug");
        }
    }

    #[test]
    fn jitter_within_bounds() {
        let p = RetryPolicy {
            jitter: 0.2,
            ..policy_no_jitter()
        };
        for _ in 0..1000 {
            let d = compute_backoff(1, &p);
            let base_secs = 30.0_f64;
            let lo = Duration::from_secs_f64(base_secs * 0.8);
            let hi = Duration::from_secs_f64(base_secs * 1.2);
            assert!(d >= lo && d <= hi, "jitter out of bounds: {d:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_status_label_maps_all_variants() {
        assert_eq!(
            verification_status_label(VerificationStatus::Verified),
            "verified"
        );
        assert_eq!(
            verification_status_label(VerificationStatus::Failed),
            "failed"
        );
        assert_eq!(
            verification_status_label(VerificationStatus::Skipped),
            "skipped"
        );
    }

    #[test]
    fn dedupe_decision_label_maps_all_variants() {
        assert_eq!(dedupe_decision_label(DedupeDecision::New), "new");
        assert_eq!(
            dedupe_decision_label(DedupeDecision::Duplicate),
            "duplicate"
        );
        assert_eq!(dedupe_decision_label(DedupeDecision::Conflict), "conflict");
    }

    #[test]
    fn retry_next_state_emits_before_exhaustion() {
        // emit_count=0, max=3 → still has retries left → stays EmitFailed
        let (count, state) = retry_next_state(0, 3);
        assert_eq!(count, 1);
        assert_eq!(state, ProcessingState::EmitFailed);
    }

    #[test]
    fn retry_next_state_dead_letters_at_max() {
        // emit_count=2, max=3 → 2+1==3 >= 3 → dead-lettered
        let (count, state) = retry_next_state(2, 3);
        assert_eq!(count, 3);
        assert_eq!(state, ProcessingState::DeadLettered);
    }

    #[test]
    fn is_findable_by_worker_only_for_emit_failed_under_limit() {
        assert!(is_findable_by_worker(0, ProcessingState::EmitFailed, 3));
        assert!(!is_findable_by_worker(3, ProcessingState::EmitFailed, 3));
        assert!(!is_findable_by_worker(0, ProcessingState::Emitted, 3));
        assert!(!is_findable_by_worker(0, ProcessingState::DeadLettered, 3));
    }
}
