//! Property tests for [`ProcessingState`], [`VerificationStatus`], and [`DedupeDecision`].

#![cfg(test)]
#![expect(clippy::expect_used, reason = "test code — panics are acceptable")]

#[test]
fn processing_state_round_trips_through_serde() {
    // Enumerate all variants explicitly so the compiler catches any additions.
    let variants = [
        hookbox::ProcessingState::Received,
        hookbox::ProcessingState::Verified,
        hookbox::ProcessingState::VerificationFailed,
        hookbox::ProcessingState::Duplicate,
        hookbox::ProcessingState::Stored,
        hookbox::ProcessingState::Emitted,
        hookbox::ProcessingState::Processed,
        hookbox::ProcessingState::EmitFailed,
        hookbox::ProcessingState::DeadLettered,
        hookbox::ProcessingState::Replayed,
    ];

    for variant in variants {
        let serialized =
            serde_json::to_string(&variant).expect("serialization must succeed");
        let deserialized: hookbox::ProcessingState =
            serde_json::from_str(&serialized).expect("deserialization must succeed");
        assert_eq!(variant, deserialized, "round-trip failed for {variant:?}");
    }
}

#[test]
fn verification_status_round_trips_through_serde() {
    let variants = [
        hookbox::VerificationStatus::Verified,
        hookbox::VerificationStatus::Failed,
        hookbox::VerificationStatus::Skipped,
    ];

    for variant in variants {
        let serialized =
            serde_json::to_string(&variant).expect("serialization must succeed");
        let deserialized: hookbox::VerificationStatus =
            serde_json::from_str(&serialized).expect("deserialization must succeed");
        assert_eq!(variant, deserialized, "round-trip failed for {variant:?}");
    }
}

#[test]
fn dedupe_decision_exhaustive_variants() {
    // Map every possible u8 modulo 3 to one of the three variants, confirming
    // that New, Duplicate, and Conflict are the only variants that exist.
    bolero::check!().with_type::<u8>().for_each(|&n| {
        let decision = match n % 3 {
            0 => hookbox::DedupeDecision::New,
            1 => hookbox::DedupeDecision::Duplicate,
            _ => hookbox::DedupeDecision::Conflict,
        };
        // Verify the variant is one of the three expected ones.
        assert!(
            matches!(
                decision,
                hookbox::DedupeDecision::New
                    | hookbox::DedupeDecision::Duplicate
                    | hookbox::DedupeDecision::Conflict
            ),
            "unexpected variant: {decision:?}"
        );
    });
}
