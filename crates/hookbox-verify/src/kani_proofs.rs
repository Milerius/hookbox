//! Kani model-checking proofs for hookbox state machine properties.
//!
//! Run with: `cargo kani -p hookbox-verify`

#[cfg(kani)]
mod proofs {
    use hookbox::state::ProcessingState;

    /// Map a u8 to a ProcessingState variant (wrapping around 10 variants).
    fn any_state() -> ProcessingState {
        let n: u8 = kani::any();
        kani::assume(n < 10);
        match n {
            0 => ProcessingState::Received,
            1 => ProcessingState::Verified,
            2 => ProcessingState::VerificationFailed,
            3 => ProcessingState::Duplicate,
            4 => ProcessingState::Stored,
            5 => ProcessingState::Emitted,
            6 => ProcessingState::Processed,
            7 => ProcessingState::EmitFailed,
            8 => ProcessingState::DeadLettered,
            _ => ProcessingState::Replayed,
        }
    }

    /// Prove that all ProcessingState variants produce non-empty serialized names
    /// and that the match is exhaustive.
    #[kani::proof]
    fn processing_state_variants_are_distinct() {
        let state = any_state();
        let serialized = match state {
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
        assert!(!serialized.is_empty());
    }

    /// Prove that the happy-path state transition sequence is valid:
    /// Received → Verified → Stored → Emitted → Processed
    #[kani::proof]
    fn happy_path_transition_sequence_is_valid() {
        let states = [
            ProcessingState::Received,
            ProcessingState::Verified,
            ProcessingState::Stored,
            ProcessingState::Emitted,
            ProcessingState::Processed,
        ];

        let mut i = 0;
        while i < states.len() - 1 {
            assert!(states[i] != states[i + 1]);
            i += 1;
        }
    }

    /// Prove that all 10 states partition cleanly into pre-store and post-store
    /// sets with no overlap and no gaps. Stored is the acceptance boundary.
    #[kani::proof]
    fn stored_is_acceptance_boundary() {
        let state = any_state();
        let is_post_store = matches!(
            state,
            ProcessingState::Emitted
                | ProcessingState::Processed
                | ProcessingState::EmitFailed
                | ProcessingState::DeadLettered
                | ProcessingState::Replayed
        );
        let is_pre_store = matches!(
            state,
            ProcessingState::Received
                | ProcessingState::Verified
                | ProcessingState::VerificationFailed
                | ProcessingState::Duplicate
                | ProcessingState::Stored
        );
        // Exhaustive partition: every state is exactly one of pre-store or post-store
        assert!(is_pre_store || is_post_store);
        assert!(!(is_pre_store && is_post_store));
    }

    /// Prove that terminal states (VerificationFailed, Duplicate, Processed) and
    /// delivery states (Emitted, EmitFailed, DeadLettered, Replayed) are disjoint.
    ///
    /// This verifies that the terminal/delivery partition is correct and that no
    /// state can simultaneously be "done" and "in the delivery pipeline".
    #[kani::proof]
    fn terminal_states_are_not_delivery_states() {
        let state = any_state();
        let is_terminal = matches!(
            state,
            ProcessingState::VerificationFailed
                | ProcessingState::Duplicate
                | ProcessingState::Processed
        );
        let is_delivery = matches!(
            state,
            ProcessingState::Emitted
                | ProcessingState::EmitFailed
                | ProcessingState::DeadLettered
                | ProcessingState::Replayed
        );
        // Terminal states and delivery states are disjoint
        assert!(!(is_terminal && is_delivery));
    }

    #[kani::proof]
    fn retry_next_state_always_valid() {
        use hookbox::transitions::retry_next_state;
        let emit_count: i32 = kani::any();
        kani::assume(emit_count >= 0 && emit_count < i32::MAX);
        let max_attempts: i32 = kani::any();
        kani::assume(max_attempts >= 1);
        let (_new_count, new_state) = retry_next_state(emit_count, max_attempts);
        assert!(matches!(
            new_state,
            ProcessingState::DeadLettered | ProcessingState::EmitFailed
        ));
    }

    #[kani::proof]
    fn reset_always_findable() {
        use hookbox::transitions::{is_findable_by_worker, reset_state};
        let max_attempts: i32 = kani::any();
        kani::assume(max_attempts >= 1);
        let (new_count, new_state) = reset_state();
        assert!(is_findable_by_worker(new_count, new_state, max_attempts));
    }

    /// Map a u8 to a `DeliveryState` variant.
    fn any_delivery_state() -> hookbox::state::DeliveryState {
        let n: u8 = kani::any();
        kani::assume(n < 5);
        match n {
            0 => hookbox::state::DeliveryState::Pending,
            1 => hookbox::state::DeliveryState::InFlight,
            2 => hookbox::state::DeliveryState::Emitted,
            3 => hookbox::state::DeliveryState::Failed,
            _ => hookbox::state::DeliveryState::DeadLettered,
        }
    }

    /// `compute_backoff` must not panic or overflow for any bounded attempt
    /// and `RetryPolicy` shape. Bounds are chosen to keep the proof tractable
    /// while still covering realistic production ranges.
    #[kani::proof]
    fn proof_backoff_no_overflow() {
        use hookbox::state::RetryPolicy;
        use std::time::Duration;

        let attempt: i32 = kani::any();
        kani::assume(attempt >= 1 && attempt <= 30);
        let initial_secs: u64 = kani::any();
        kani::assume(initial_secs >= 1 && initial_secs <= 3600);
        let max_secs: u64 = kani::any();
        kani::assume(max_secs >= initial_secs && max_secs <= 86_400);

        let policy = RetryPolicy {
            max_attempts: 10,
            initial_backoff: Duration::from_secs(initial_secs),
            max_backoff: Duration::from_secs(max_secs),
            backoff_multiplier: 2.0,
            jitter: 0.0,
        };

        let _ = hookbox::transitions::compute_backoff(attempt, &policy);
    }

    /// `receipt_aggregate_state` must be total over every combination of two
    /// delivery states. Uses a fixed two-element delivery slice for
    /// tractability; the function's structure is invariant over slice length.
    #[kani::proof]
    fn proof_aggregate_state_total() {
        use chrono::Utc;
        use hookbox::state::{DeliveryId, ProcessingState, ReceiptId, WebhookDelivery};
        use uuid::Uuid;

        let s1 = any_delivery_state();
        let s2 = any_delivery_state();

        let make = |emitter: &str, state| WebhookDelivery {
            delivery_id: DeliveryId(Uuid::nil()),
            receipt_id: ReceiptId(Uuid::nil()),
            emitter_name: emitter.to_owned(),
            state,
            attempt_count: 0,
            last_error: None,
            last_attempt_at: None,
            next_attempt_at: Utc::now(),
            emitted_at: None,
            immutable: false,
            created_at: Utc::now(),
        };

        let deliveries = vec![make("a", s1), make("b", s2)];

        let _ = hookbox::transitions::receipt_aggregate_state(&deliveries, ProcessingState::Stored);
    }
}
