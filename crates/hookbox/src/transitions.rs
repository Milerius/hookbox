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

use crate::state::{
    DedupeDecision, DeliveryState, ProcessingState, RetryPolicy, VerificationStatus,
    WebhookDelivery,
};
use std::collections::BTreeMap;
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
    debug_assert!(
        policy.jitter >= 0.0 && policy.jitter <= 1.0,
        "jitter must be in [0.0, 1.0]; got {}",
        policy.jitter
    );
    if attempt <= 0 {
        return policy.initial_backoff;
    }
    // attempt >= 1 here (guarded above), so attempt - 1 >= 0 and the
    // subtraction never wraps. `powi` takes i32 and attempt fits in i32.
    let exponent = attempt - 1;
    let max_secs = policy.max_backoff.as_secs_f64();
    let base_secs = policy.initial_backoff.as_secs_f64() * policy.backoff_multiplier.powi(exponent);
    // Clamp the base before jitter as a numerical safety valve for huge
    // multipliers, but the authoritative cap is applied after jitter below
    // so positive jitter cannot push the final delay past `max_backoff`.
    let base_secs = base_secs.min(max_secs);

    let jitter_factor = if policy.jitter > 0.0 {
        1.0 + fastrand::f64().mul_add(2.0 * policy.jitter, -policy.jitter)
    } else {
        1.0
    };
    // Release-mode safety net: clamp to non-negative so Duration::from_secs_f64
    // cannot panic if the policy was misconfigured (the debug_assert above
    // catches this in dev/test).
    let jitter_factor = jitter_factor.max(0.0);
    let jittered = (base_secs * jitter_factor).min(max_secs);

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

    #[test]
    fn jittered_delay_never_exceeds_max_backoff() {
        // With jitter=1.0, the pre-jitter base equals max_backoff, and
        // positive jitter could push the result up to 2× max_backoff if the
        // cap is applied only before jitter. The final clamp must hold.
        let p = RetryPolicy {
            max_attempts: 10,
            initial_backoff: Duration::from_secs(60),
            max_backoff: Duration::from_secs(120),
            backoff_multiplier: 4.0,
            jitter: 1.0,
        };
        let cap = p.max_backoff;
        for _ in 0..5000 {
            let d = compute_backoff(5, &p);
            assert!(
                d <= cap,
                "compute_backoff must clamp post-jitter: got {d:?}, cap {cap:?}"
            );
        }
    }

    #[test]
    fn invalid_jitter_does_not_panic_in_release() {
        // jitter > 1.0 is a programmer error caught by debug_assert in dev,
        // but must not panic in release. The release fallback clamps the
        // jitter_factor to non-negative.
        if !cfg!(debug_assertions) {
            let p = RetryPolicy {
                max_attempts: 5,
                initial_backoff: Duration::from_secs(30),
                max_backoff: Duration::from_secs(3600),
                backoff_multiplier: 2.0,
                jitter: 2.0, // out of contract
            };
            // 1000 iterations: with the clamp, no panic regardless of fastrand
            for _ in 0..1000 {
                let _ = compute_backoff(1, &p);
            }
        }
    }
}

// ── Fan-out aggregate helpers ─────────────────────────────────────────────────

/// Derive the visible [`ProcessingState`] for a receipt from its delivery rows.
///
/// Operates only on the latest non-immutable row per `emitter_name`. If the
/// resulting set is empty (all immutable or no rows), falls back to `fallback`
/// (the receipt's stored `processing_state`).
///
/// Aggregation precedence (highest to lowest):
/// 1. `DeadLettered` — at least one latest row is `dead_lettered`.
/// 2. `EmitFailed`   — at least one latest row is `failed`, none `dead_lettered`.
/// 3. `Emitted`      — every latest row is `emitted`.
/// 4. `Stored`       — all latest rows are `pending` or `in_flight`.
#[must_use]
pub fn receipt_aggregate_state(
    deliveries: &[WebhookDelivery],
    fallback: ProcessingState,
) -> ProcessingState {
    let latest = latest_mutable_per_emitter(deliveries);
    if latest.is_empty() {
        return fallback;
    }
    if latest.values().any(|s| *s == DeliveryState::DeadLettered) {
        return ProcessingState::DeadLettered;
    }
    if latest.values().any(|s| *s == DeliveryState::Failed) {
        return ProcessingState::EmitFailed;
    }
    if latest.values().all(|s| *s == DeliveryState::Emitted) {
        return ProcessingState::Emitted;
    }
    ProcessingState::Stored
}

/// Per-emitter delivery state from the latest non-immutable row for each emitter.
///
/// Returns an empty map when all rows are immutable.
#[must_use]
pub fn receipt_deliveries_summary(
    deliveries: &[WebhookDelivery],
) -> BTreeMap<String, DeliveryState> {
    latest_mutable_per_emitter(deliveries)
}

// -- internal -----------------------------------------------------------------

/// For each `emitter_name`, return the `state` of the non-immutable row
/// with the greatest `created_at`. Mirrors the SQL:
/// `SELECT DISTINCT ON (emitter_name) state ... ORDER BY emitter_name, created_at DESC`
fn latest_mutable_per_emitter(deliveries: &[WebhookDelivery]) -> BTreeMap<String, DeliveryState> {
    let mut map: BTreeMap<String, (chrono::DateTime<chrono::Utc>, DeliveryState)> = BTreeMap::new();
    for d in deliveries {
        if d.immutable {
            continue;
        }
        map.entry(d.emitter_name.clone())
            .and_modify(|(ts, st)| {
                if d.created_at > *ts {
                    *ts = d.created_at;
                    *st = d.state;
                }
            })
            .or_insert((d.created_at, d.state));
    }
    map.into_iter().map(|(k, (_, s))| (k, s)).collect()
}

#[cfg(test)]
mod aggregate_tests {
    use super::*;
    use crate::state::{DeliveryId, DeliveryState, ProcessingState, ReceiptId, WebhookDelivery};
    use chrono::Utc;
    use uuid::Uuid;

    fn delivery(
        emitter: &str,
        state: DeliveryState,
        immutable: bool,
        created_offset_secs: i64,
    ) -> WebhookDelivery {
        WebhookDelivery {
            delivery_id: DeliveryId(Uuid::new_v4()),
            receipt_id: ReceiptId(Uuid::new_v4()),
            emitter_name: emitter.to_string(),
            state,
            attempt_count: 0,
            last_error: None,
            last_attempt_at: None,
            next_attempt_at: Utc::now(),
            emitted_at: None,
            immutable,
            created_at: Utc::now() + chrono::Duration::seconds(created_offset_secs),
        }
    }

    #[test]
    fn empty_slice_returns_stored() {
        assert_eq!(
            receipt_aggregate_state(&[], ProcessingState::Stored),
            ProcessingState::Stored
        );
    }

    #[test]
    fn all_immutable_falls_back_to_stored_processing_state() {
        let rows = vec![delivery("legacy", DeliveryState::Failed, true, 0)];
        assert_eq!(
            receipt_aggregate_state(&rows, ProcessingState::EmitFailed),
            ProcessingState::EmitFailed,
        );
    }

    #[test]
    fn all_emitted_returns_emitted() {
        let rows = vec![
            delivery("a", DeliveryState::Emitted, false, 0),
            delivery("b", DeliveryState::Emitted, false, 0),
        ];
        assert_eq!(
            receipt_aggregate_state(&rows, ProcessingState::Stored),
            ProcessingState::Emitted
        );
    }

    #[test]
    fn any_dead_lettered_returns_dead_lettered() {
        let rows = vec![
            delivery("a", DeliveryState::Emitted, false, 0),
            delivery("b", DeliveryState::DeadLettered, false, 0),
        ];
        assert_eq!(
            receipt_aggregate_state(&rows, ProcessingState::Stored),
            ProcessingState::DeadLettered
        );
    }

    #[test]
    fn dead_lettered_wins_over_failed() {
        let rows = vec![
            delivery("a", DeliveryState::Failed, false, 0),
            delivery("b", DeliveryState::DeadLettered, false, 0),
        ];
        assert_eq!(
            receipt_aggregate_state(&rows, ProcessingState::Stored),
            ProcessingState::DeadLettered
        );
    }

    #[test]
    fn any_failed_no_dead_lettered_returns_emit_failed() {
        let rows = vec![
            delivery("a", DeliveryState::Emitted, false, 0),
            delivery("b", DeliveryState::Failed, false, 0),
        ];
        assert_eq!(
            receipt_aggregate_state(&rows, ProcessingState::Stored),
            ProcessingState::EmitFailed
        );
    }

    #[test]
    fn pending_and_in_flight_collapse_to_stored() {
        let rows = vec![
            delivery("a", DeliveryState::Pending, false, 0),
            delivery("b", DeliveryState::InFlight, false, 0),
        ];
        assert_eq!(
            receipt_aggregate_state(&rows, ProcessingState::Stored),
            ProcessingState::Stored
        );
    }

    #[test]
    fn latest_mutable_row_wins_per_emitter() {
        let rows = vec![
            delivery("a", DeliveryState::DeadLettered, false, 0),
            delivery("a", DeliveryState::Emitted, false, 10),
        ];
        assert_eq!(
            receipt_aggregate_state(&rows, ProcessingState::Stored),
            ProcessingState::Emitted
        );
    }

    #[test]
    fn replay_history_does_not_poison_state() {
        let rows = vec![
            delivery("a", DeliveryState::DeadLettered, false, 0),
            delivery("a", DeliveryState::Emitted, false, 5),
            delivery("b", DeliveryState::Emitted, false, 0),
        ];
        assert_eq!(
            receipt_aggregate_state(&rows, ProcessingState::Stored),
            ProcessingState::Emitted
        );
    }

    #[test]
    fn deliveries_summary_one_entry_per_emitter_latest_row() {
        let rows = vec![
            delivery("a", DeliveryState::DeadLettered, false, 0),
            delivery("a", DeliveryState::Emitted, false, 5),
            delivery("b", DeliveryState::Failed, false, 0),
        ];
        let summary = receipt_deliveries_summary(&rows);
        assert_eq!(summary.len(), 2);
        assert_eq!(summary["a"], DeliveryState::Emitted);
        assert_eq!(summary["b"], DeliveryState::Failed);
    }

    #[test]
    fn deliveries_summary_ignores_immutable_rows() {
        let rows = vec![delivery("legacy", DeliveryState::Emitted, true, 0)];
        let summary = receipt_deliveries_summary(&rows);
        assert!(summary.is_empty());
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
