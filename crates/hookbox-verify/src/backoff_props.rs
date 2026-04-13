//! Bolero property tests for `hookbox::transitions::compute_backoff`.
//!
//! Properties verified:
//! - Monotone under zero jitter (consecutive attempts never decrease)
//! - First failure (`attempt = 1`) returns `initial_backoff` exactly
//! - Result is always bounded by `max_backoff`

#[cfg(test)]
mod tests {
    use hookbox::state::RetryPolicy;
    use hookbox::transitions::compute_backoff;
    use std::time::Duration;

    fn bounded_policy(
        initial_secs: u8,
        max_secs_delta: u8,
        multiplier_x10: u8,
        max_attempts: u8,
    ) -> RetryPolicy {
        let initial = Duration::from_secs(u64::from(initial_secs).max(1));
        let max = initial + Duration::from_secs(u64::from(max_secs_delta));
        RetryPolicy {
            max_attempts: i32::from(max_attempts.max(1)),
            initial_backoff: initial,
            max_backoff: max,
            backoff_multiplier: (f64::from(multiplier_x10.max(10)) / 10.0).min(4.0),
            jitter: 0.0,
        }
    }

    #[test]
    fn prop_backoff_monotonic_under_no_jitter() {
        bolero::check!()
            .with_type::<(u8, u8, u8, u8)>()
            .for_each(|(i, d, m, a)| {
                let p = bounded_policy(*i, *d, *m, *a);
                for attempt in 1..15_i32 {
                    let lo = compute_backoff(attempt, &p);
                    let hi = compute_backoff(attempt + 1, &p);
                    assert!(
                        lo <= hi,
                        "not monotone: attempt={attempt} lo={lo:?} hi={hi:?}"
                    );
                }
            });
    }

    #[test]
    fn prop_backoff_first_failure_is_initial_under_no_jitter() {
        bolero::check!()
            .with_type::<(u8, u8, u8, u8)>()
            .for_each(|(i, d, m, a)| {
                let p = bounded_policy(*i, *d, *m, *a);
                let got = compute_backoff(1, &p);
                assert_eq!(
                    got, p.initial_backoff,
                    "attempt=1 must equal initial_backoff"
                );
            });
    }

    #[test]
    fn prop_backoff_within_max_backoff() {
        bolero::check!()
            .with_type::<(u8, u8, u8, u8, u8)>()
            .for_each(|(i, d, m, a, attempt_raw)| {
                let p = bounded_policy(*i, *d, *m, *a);
                let attempt = i32::from((*attempt_raw).max(1));
                let got = compute_backoff(attempt, &p);
                assert!(
                    got <= p.max_backoff,
                    "backoff {got:?} exceeded max_backoff {:?}",
                    p.max_backoff
                );
            });
    }
}
