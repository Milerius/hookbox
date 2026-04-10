//! PostgreSQL-backed implementation of the [`DedupeStrategy`] trait.
//!
//! [`StorageDedupe`] is an advisory dedupe layer that checks the
//! `webhook_receipts` table directly.  It is slower than the in-memory LRU
//! fast-path but provides a durable, cross-process view of already-seen keys.
//!
//! `record` is intentionally a no-op: persistence is handled by
//! [`crate::storage::PostgresStorage::store`], which writes the authoritative
//! row.  Calling `record` after a successful store therefore has no extra cost.

use async_trait::async_trait;
use sqlx::PgPool;

use hookbox::error::DedupeError;
use hookbox::state::DedupeDecision;
use hookbox::traits::DedupeStrategy;

/// PostgreSQL-backed advisory deduplication strategy.
///
/// Checks `webhook_receipts.payload_hash` for a given `dedupe_key` to
/// determine whether an inbound event has been seen before.
#[derive(Debug, Clone)]
pub struct StorageDedupe {
    pool: PgPool,
}

impl StorageDedupe {
    /// Create a new [`StorageDedupe`] from an existing connection pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl DedupeStrategy for StorageDedupe {
    /// Check whether the given `dedupe_key` / `payload_hash` pair has been
    /// seen before.
    ///
    /// - Returns [`DedupeDecision::New`] when the key is absent from the store.
    /// - Returns [`DedupeDecision::Duplicate`] when the key exists *and* the
    ///   stored `payload_hash` matches the supplied one.
    /// - Returns [`DedupeDecision::Conflict`] when the key exists but with a
    ///   *different* `payload_hash` (same dedupe key, different content).
    async fn check(
        &self,
        dedupe_key: &str,
        payload_hash: &str,
    ) -> Result<DedupeDecision, DedupeError> {
        let stored_hash: Option<String> = sqlx::query_scalar(
            "SELECT payload_hash FROM webhook_receipts WHERE dedupe_key = $1",
        )
        .bind(dedupe_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| DedupeError::Internal(e.to_string()))?;

        match stored_hash {
            None => Ok(DedupeDecision::New),
            Some(h) if h == payload_hash => Ok(DedupeDecision::Duplicate),
            Some(_) => Ok(DedupeDecision::Conflict),
        }
    }

    /// No-op: persistence is handled by [`crate::storage::PostgresStorage::store`].
    ///
    /// The authoritative dedupe row is written atomically during `store`, so
    /// there is nothing additional to record here.
    async fn record(
        &self,
        _dedupe_key: &str,
        _payload_hash: &str,
    ) -> Result<(), DedupeError> {
        Ok(())
    }
}
