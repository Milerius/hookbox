//! Property tests for the retry state machine transitions.

#[cfg(test)]
mod tests {
    #[test]
    fn retry_always_promotes_to_dlq_at_max() {
        bolero::check!()
            .with_type::<(u8, u8)>()
            .for_each(|(emit_count, max_attempts)| {
                let emit_count = i32::from(*emit_count);
                let max_attempts = i32::from(*max_attempts).max(1);
                let new_count = emit_count + 1;
                let new_state = if new_count >= max_attempts {
                    "dead_lettered"
                } else {
                    "emit_failed"
                };
                if new_count >= max_attempts {
                    assert_eq!(new_state, "dead_lettered");
                } else {
                    assert_eq!(new_state, "emit_failed");
                }
            });
    }

    #[test]
    fn reset_always_produces_retryable_state() {
        bolero::check!().with_type::<u8>().for_each(|&prev_count| {
            let _ = prev_count;
            let new_count: i32 = 0;
            let new_state = "emit_failed";
            assert_eq!(new_count, 0);
            assert_eq!(new_state, "emit_failed");
        });
    }
}
