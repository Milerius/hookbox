//! Property tests for metrics label invariants.

#[cfg(test)]
mod tests {
    use hookbox::state::{DedupeDecision, VerificationStatus};
    use hookbox::transitions::{
        INGEST_RESULT_LABELS, dedupe_decision_label, verification_status_label,
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
            assert!(matches!(label, "verified" | "failed" | "skipped"));
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
            assert!(matches!(label, "new" | "duplicate" | "conflict"));
        });
    }

    #[test]
    fn ingest_result_labels_are_all_non_empty() {
        for label in INGEST_RESULT_LABELS {
            assert!(!label.is_empty());
        }
    }
}
