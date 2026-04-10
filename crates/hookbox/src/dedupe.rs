//! Built-in deduplification strategy implementations.
//!
//! Provides two ready-to-use [`crate::traits::DedupeStrategy`] implementations:
//!
//! - [`InMemoryRecentDedupe`] — bounded in-process LRU-style cache.
//! - [`LayeredDedupe`] — composes a fast-path strategy with an authoritative
//!   strategy, short-circuiting on the first non-`New` result from the fast
//!   path.

use std::collections::HashMap;
use std::collections::VecDeque;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::error::DedupeError;
use crate::state::DedupeDecision;
use crate::traits::DedupeStrategy;

/// Bounded in-memory cache for advisory deduplication.
///
/// Stores `dedupe_key → payload_hash` mappings up to a configured capacity.
/// When the cache is full the oldest entry is evicted (FIFO order).
///
/// This is an **advisory** implementation only — it must be paired with an
/// authoritative storage-level dedupe check.
pub struct InMemoryRecentDedupe {
    capacity: usize,
    inner: Mutex<InMemoryRecentDedupeInner>,
}

struct InMemoryRecentDedupeInner {
    map: HashMap<String, String>,
    order: VecDeque<String>,
}

impl InMemoryRecentDedupe {
    /// Create a new cache with the given maximum `capacity`.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "InMemoryRecentDedupe capacity must be > 0");
        Self {
            capacity,
            inner: Mutex::new(InMemoryRecentDedupeInner {
                map: HashMap::new(),
                order: VecDeque::new(),
            }),
        }
    }
}

#[async_trait]
impl DedupeStrategy for InMemoryRecentDedupe {
    async fn check(
        &self,
        dedupe_key: &str,
        payload_hash: &str,
    ) -> Result<DedupeDecision, DedupeError> {
        let inner = self.inner.lock().await;
        match inner.map.get(dedupe_key) {
            None => Ok(DedupeDecision::New),
            Some(stored_hash) => {
                if stored_hash == payload_hash {
                    Ok(DedupeDecision::Duplicate)
                } else {
                    Ok(DedupeDecision::Conflict)
                }
            }
        }
    }

    async fn record(
        &self,
        dedupe_key: &str,
        payload_hash: &str,
    ) -> Result<(), DedupeError> {
        let mut inner = self.inner.lock().await;

        // No-op if the key already exists.
        if inner.map.contains_key(dedupe_key) {
            return Ok(());
        }

        // Evict oldest entry when at capacity.
        if inner.map.len() >= self.capacity {
            if let Some(oldest_key) = inner.order.pop_front() {
                inner.map.remove(&oldest_key);
            }
        }

        inner.map.insert(dedupe_key.to_owned(), payload_hash.to_owned());
        inner.order.push_back(dedupe_key.to_owned());

        Ok(())
    }
}

/// Composes a fast-path [`DedupeStrategy`] with an authoritative one.
///
/// `check` consults the fast-path strategy first.  If the fast-path returns
/// [`DedupeDecision::New`] the call falls through to the authoritative
/// strategy.  Any `Duplicate` or `Conflict` result from either layer is
/// returned immediately.
///
/// `record` always records in **both** layers so they remain consistent.
pub struct LayeredDedupe<F, A> {
    fast: F,
    authoritative: A,
}

impl<F, A> LayeredDedupe<F, A>
where
    F: DedupeStrategy,
    A: DedupeStrategy,
{
    /// Create a new [`LayeredDedupe`] from a fast-path and an authoritative
    /// strategy.
    pub fn new(fast: F, authoritative: A) -> Self {
        Self { fast, authoritative }
    }
}

#[async_trait]
impl<F, A> DedupeStrategy for LayeredDedupe<F, A>
where
    F: DedupeStrategy,
    A: DedupeStrategy,
{
    async fn check(
        &self,
        dedupe_key: &str,
        payload_hash: &str,
    ) -> Result<DedupeDecision, DedupeError> {
        let fast_result = self.fast.check(dedupe_key, payload_hash).await?;
        match fast_result {
            DedupeDecision::New => self.authoritative.check(dedupe_key, payload_hash).await,
            other => Ok(other),
        }
    }

    async fn record(
        &self,
        dedupe_key: &str,
        payload_hash: &str,
    ) -> Result<(), DedupeError> {
        self.fast.record(dedupe_key, payload_hash).await?;
        self.authoritative.record(dedupe_key, payload_hash).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lru_new_key_returns_new() {
        let cache = InMemoryRecentDedupe::new(10);
        let result = cache.check("key-1", "hash-abc").await;
        assert!(result.is_ok(), "check should not fail");
        assert_eq!(result.ok(), Some(DedupeDecision::New));
    }

    #[tokio::test]
    async fn lru_same_key_same_hash_returns_duplicate() {
        let cache = InMemoryRecentDedupe::new(10);
        assert!(cache.record("key-1", "hash-abc").await.is_ok());
        let result = cache.check("key-1", "hash-abc").await;
        assert!(result.is_ok(), "check should not fail");
        assert_eq!(result.ok(), Some(DedupeDecision::Duplicate));
    }

    #[tokio::test]
    async fn lru_same_key_different_hash_returns_conflict() {
        let cache = InMemoryRecentDedupe::new(10);
        assert!(cache.record("key-1", "hash-abc").await.is_ok());
        let result = cache.check("key-1", "hash-xyz").await;
        assert!(result.is_ok(), "check should not fail");
        assert_eq!(result.ok(), Some(DedupeDecision::Conflict));
    }

    #[tokio::test]
    async fn lru_eviction_makes_key_new_again() {
        // Capacity of 2: inserting a third entry evicts the first.
        let cache = InMemoryRecentDedupe::new(2);
        assert!(cache.record("key-1", "hash-1").await.is_ok());
        assert!(cache.record("key-2", "hash-2").await.is_ok());
        // This insert evicts key-1.
        assert!(cache.record("key-3", "hash-3").await.is_ok());

        // key-1 should be treated as New again.
        let evicted = cache.check("key-1", "hash-1").await;
        assert_eq!(evicted.ok(), Some(DedupeDecision::New));

        // key-2 and key-3 should still be present.
        let key2 = cache.check("key-2", "hash-2").await;
        assert_eq!(key2.ok(), Some(DedupeDecision::Duplicate));
        let key3 = cache.check("key-3", "hash-3").await;
        assert_eq!(key3.ok(), Some(DedupeDecision::Duplicate));
    }

    #[tokio::test]
    async fn layered_returns_fast_path_duplicate() {
        let fast = InMemoryRecentDedupe::new(10);
        let authoritative = InMemoryRecentDedupe::new(10);

        assert!(fast.record("key-1", "hash-abc").await.is_ok());
        // authoritative does NOT have the key.

        let layered = LayeredDedupe::new(fast, authoritative);
        let result = layered.check("key-1", "hash-abc").await;
        assert_eq!(result.ok(), Some(DedupeDecision::Duplicate));
    }

    #[tokio::test]
    async fn layered_falls_through_to_authoritative() {
        let fast = InMemoryRecentDedupe::new(10);
        let authoritative = InMemoryRecentDedupe::new(10);

        // Only authoritative has the key.
        assert!(authoritative.record("key-1", "hash-abc").await.is_ok());

        let layered = LayeredDedupe::new(fast, authoritative);
        let result = layered.check("key-1", "hash-abc").await;
        assert_eq!(result.ok(), Some(DedupeDecision::Duplicate));
    }

    #[tokio::test]
    async fn layered_new_in_both_returns_new() {
        let fast = InMemoryRecentDedupe::new(10);
        let authoritative = InMemoryRecentDedupe::new(10);

        let layered = LayeredDedupe::new(fast, authoritative);
        let result = layered.check("key-1", "hash-abc").await;
        assert_eq!(result.ok(), Some(DedupeDecision::New));
    }
}
