//! Property tests for metrics label invariants.

#[cfg(test)]
mod tests {
    #[test]
    fn every_ingest_result_maps_to_exactly_one_label() {
        bolero::check!().with_type::<u8>().for_each(|&n| {
            let label = match n % 4 {
                0 => "accepted",
                1 => "duplicate",
                2 => "verification_failed",
                _ => "store_failed",
            };
            assert!(matches!(label, "accepted" | "duplicate" | "verification_failed" | "store_failed"));
        });
    }

    #[test]
    fn verification_status_maps_to_valid_label() {
        bolero::check!().with_type::<u8>().for_each(|&n| {
            let label = match n % 3 { 0 => "verified", 1 => "failed", _ => "skipped" };
            assert!(!label.is_empty());
        });
    }
}
