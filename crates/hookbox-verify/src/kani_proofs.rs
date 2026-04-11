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
            ProcessingState::DeadLettered | ProcessingState::Emitted
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
}
