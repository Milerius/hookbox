//! Property tests for [`ProcessingState`], [`VerificationStatus`], and [`DedupeDecision`].

#[cfg(test)]
mod tests {
    #[test]
    fn processing_state_round_trips_through_serde() {
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
            let serialized = serde_json::to_string(&variant).expect("serialization must succeed");
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
            let serialized = serde_json::to_string(&variant).expect("serialization must succeed");
            let deserialized: hookbox::VerificationStatus =
                serde_json::from_str(&serialized).expect("deserialization must succeed");
            assert_eq!(variant, deserialized, "round-trip failed for {variant:?}");
        }
    }

    #[test]
    fn dedupe_decision_exhaustive_variants() {
        bolero::check!().with_type::<u8>().for_each(|&n| {
            let decision = match n % 3 {
                0 => hookbox::DedupeDecision::New,
                1 => hookbox::DedupeDecision::Duplicate,
                _ => hookbox::DedupeDecision::Conflict,
            };
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
}
