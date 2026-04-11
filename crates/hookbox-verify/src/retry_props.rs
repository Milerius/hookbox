//! Property tests for the retry state machine transitions.

#[cfg(test)]
mod tests {
    use hookbox::state::ProcessingState;
    use hookbox::transitions::{is_findable_by_worker, reset_state, retry_next_state};

    #[test]
    fn retry_always_reaches_dead_lettered_at_max() {
        bolero::check!()
            .with_type::<(u8, u8)>()
            .for_each(|(emit_count, max_attempts)| {
                let emit_count = i32::from(*emit_count);
                let max_attempts = i32::from(*max_attempts).max(1);
                let (new_count, new_state) = retry_next_state(emit_count, max_attempts);
                assert_eq!(new_count, emit_count + 1);
                if new_count >= max_attempts {
                    assert_eq!(new_state, ProcessingState::DeadLettered);
                } else {
                    assert_eq!(new_state, ProcessingState::Emitted);
                }
            });
    }

    #[test]
    fn reset_is_always_findable_by_worker() {
        bolero::check!().with_type::<u8>().for_each(|&max_attempts| {
            let max_attempts = i32::from(max_attempts).max(1);
            let (new_count, new_state) = reset_state();
            assert!(is_findable_by_worker(new_count, new_state, max_attempts));
        });
    }

    #[test]
    fn dead_lettered_is_never_findable_by_worker() {
        bolero::check!()
            .with_type::<(u8, u8)>()
            .for_each(|(count, max_attempts)| {
                let count = i32::from(*count);
                let max_attempts = i32::from(*max_attempts).max(1);
                assert!(!is_findable_by_worker(
                    count,
                    ProcessingState::DeadLettered,
                    max_attempts
                ));
            });
    }
}
