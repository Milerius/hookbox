//! Property tests for [`InMemoryRecentDedupe`].

#[cfg(test)]
mod tests {
    use hookbox::DedupeStrategy as _;

    #[test]
    fn after_record_check_returns_duplicate() {
        let rt = tokio::runtime::Runtime::new().expect("runtime must be created");
        bolero::check!()
            .with_type::<(String, String)>()
            .for_each(|(key, hash)| {
                rt.block_on(async {
                    let cache = hookbox::InMemoryRecentDedupe::new(64);
                    cache.record(key, hash).await.expect("record must succeed");
                    let decision = cache.check(key, hash).await.expect("check must succeed");
                    assert_eq!(
                        decision,
                        hookbox::DedupeDecision::Duplicate,
                        "check after record with same hash must return Duplicate"
                    );
                });
            });
    }

    #[test]
    fn after_record_check_different_hash_returns_conflict() {
        let rt = tokio::runtime::Runtime::new().expect("runtime must be created");
        bolero::check!()
            .with_type::<(String, String, String)>()
            .for_each(|(key, hash1, hash2)| {
                if hash1 == hash2 {
                    return;
                }
                rt.block_on(async {
                    let cache = hookbox::InMemoryRecentDedupe::new(64);
                    cache.record(key, hash1).await.expect("record must succeed");
                    let decision = cache.check(key, hash2).await.expect("check must succeed");
                    assert_eq!(
                        decision,
                        hookbox::DedupeDecision::Conflict,
                        "check after record with different hash must return Conflict"
                    );
                });
            });
    }

    #[test]
    fn check_on_never_recorded_key_returns_new() {
        let rt = tokio::runtime::Runtime::new().expect("runtime must be created");
        bolero::check!()
            .with_type::<(String, String)>()
            .for_each(|(key, hash)| {
                rt.block_on(async {
                    let cache = hookbox::InMemoryRecentDedupe::new(64);
                    let decision = cache.check(key, hash).await.expect("check must succeed");
                    assert_eq!(
                        decision,
                        hookbox::DedupeDecision::New,
                        "check on never-recorded key must return New"
                    );
                });
            });
    }

    #[test]
    fn capacity_respected_first_key_evicted() {
        const CAPACITY: usize = 4;
        let rt = tokio::runtime::Runtime::new().expect("runtime must be created");
        bolero::check!()
            .with_type::<[String; 5]>()
            .for_each(|keys| {
                let unique_count = {
                    let mut seen = std::collections::HashSet::new();
                    keys.iter().filter(|k| seen.insert(k.as_str())).count()
                };
                if unique_count < 5 {
                    return;
                }

                rt.block_on(async {
                    let cache = hookbox::InMemoryRecentDedupe::new(CAPACITY);
                    for key in keys {
                        cache
                            .record(key.as_str(), "hash")
                            .await
                            .expect("record must succeed");
                    }
                    let decision = cache
                        .check(keys[0].as_str(), "hash")
                        .await
                        .expect("check must succeed");
                    assert_eq!(
                        decision,
                        hookbox::DedupeDecision::New,
                        "first key must be evicted after inserting capacity+1 entries"
                    );
                });
            });
    }
}
