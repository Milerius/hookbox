//! Core extension-point traits for the hookbox ingest pipeline.
//!
//! These four traits define the entire pluggable surface of hookbox.
//! Downstream crates implement them to provide concrete backends; the
//! pipeline wires them together without knowing about any specific
//! implementation.
//!
//! # Design contracts
//!
//! - [`SignatureVerifier`] — provider-specific HMAC/JWT verification.
//! - [`Storage`] — durable persistence; **authoritative** dedupe answer.
//! - [`DedupeStrategy`] — fast-path advisory duplicate detection only.
//! - [`Emitter`] — downstream forwarding; failure does **not** invalidate
//!   a previously stored receipt.

use async_trait::async_trait;
use http::HeaderMap;
use uuid::Uuid;

use crate::error::{DedupeError, EmitError, StorageError};
use crate::state::{DedupeDecision, ProcessingState, StoreResult, VerificationResult};
use crate::types::{NormalizedEvent, ReceiptFilter, WebhookReceipt};

/// Verifies provider-specific webhook signatures from inbound HTTP requests.
///
/// Each provider (Stripe, GitHub, BVNK, …) has its own signing scheme.
/// Implementors encapsulate that logic and expose a uniform interface to
/// the pipeline.
///
/// # Contract
///
/// - Implementations **must** be stateless with respect to individual
///   requests; any shared state (e.g. cached public keys) must be
///   protected internally.
/// - Returning [`VerificationResult`] with `status == Failed` does **not**
///   raise an error — it is a legitimate (expected) outcome. An `Err` from
///   `verify` signals an unexpected internal failure, not an invalid
///   signature.
#[async_trait]
pub trait SignatureVerifier: Send + Sync {
    /// Returns the canonical name of the provider this verifier handles
    /// (e.g. `"stripe"`, `"github"`).
    fn provider_name(&self) -> &str;

    /// Verifies the provider signature contained in `headers` against the
    /// raw request `body`.
    ///
    /// Returns a [`VerificationResult`] that describes whether verification
    /// passed, failed, or was intentionally skipped.
    async fn verify(&self, headers: &HeaderMap, body: &[u8]) -> VerificationResult;
}

/// Durable persistence backend for [`WebhookReceipt`] records.
///
/// `Storage` is the **authoritative** source of truth for deduplication.
/// [`Storage::store`] must enforce a unique constraint on the dedupe key
/// and return [`StoreResult::Duplicate`] if a conflicting receipt already
/// exists — this is the final word on whether an event is new or a repeat.
///
/// # Contract
///
/// - [`store`](Storage::store) **must** be idempotent on the dedupe key:
///   two concurrent calls with the same key must result in exactly one
///   stored receipt.
/// - [`get`](Storage::get) and [`query`](Storage::query) are read-only and
///   must not mutate state.
/// - [`update_state`](Storage::update_state) is the only allowed mutation
///   after the initial store.
#[async_trait]
pub trait Storage: Send + Sync {
    /// Durably store a new [`WebhookReceipt`].
    ///
    /// Returns [`StoreResult::Stored`] on success or
    /// [`StoreResult::Duplicate`] when a receipt with the same dedupe key
    /// is already present.  Returns `Err` only for unexpected infrastructure
    /// failures.
    async fn store(&self, receipt: &WebhookReceipt) -> Result<StoreResult, StorageError>;

    /// Retrieve a single [`WebhookReceipt`] by its raw [`Uuid`].
    ///
    /// Returns `Ok(None)` when no receipt with the given ID exists.
    async fn get(&self, id: Uuid) -> Result<Option<WebhookReceipt>, StorageError>;

    /// Transition the [`ProcessingState`] of an existing receipt.
    ///
    /// `error` carries a short human-readable message when transitioning to
    /// an error state (e.g. [`ProcessingState::EmitFailed`]); pass `None`
    /// for successful transitions.
    async fn update_state(
        &self,
        id: Uuid,
        state: ProcessingState,
        error: Option<&str>,
    ) -> Result<(), StorageError>;

    /// Query stored receipts using the supplied [`ReceiptFilter`].
    ///
    /// All filter fields are optional; an empty filter returns all receipts
    /// up to any backend-enforced maximum.
    async fn query(&self, filter: ReceiptFilter) -> Result<Vec<WebhookReceipt>, StorageError>;

    /// Atomically insert the receipt and one pending delivery row per emitter.
    ///
    /// Either all inserts succeed, or the entire transaction is rolled back.
    /// Returns [`StoreResult`] (same shape as [`store`](Storage::store);
    /// [`StoreResult::Duplicate`] short-circuits before any delivery rows are
    /// inserted).
    ///
    /// The default implementation delegates to [`store`](Storage::store) and
    /// ignores `emitter_names`, which keeps all existing mock implementations
    /// compiling without modification.  Only `PostgresStorage` (in the
    /// `hookbox-postgres` crate) and the pipeline-unit-test mock override
    /// this to provide real transactional behaviour.
    async fn store_with_deliveries(
        &self,
        receipt: &WebhookReceipt,
        emitter_names: &[String],
    ) -> Result<StoreResult, StorageError> {
        let _ = emitter_names;
        self.store(receipt).await
    }
}

/// Advisory fast-path duplicate detection.
///
/// `DedupeStrategy` provides a **best-effort** hint before hitting durable
/// storage.  It is intentionally advisory: the pipeline **must not** skip
/// [`Storage::store`] solely on the basis of a `Duplicate` decision here.
/// The authoritative answer always comes from [`Storage`].
///
/// Typical implementations use an in-process LRU cache, a Redis SET, or
/// similar low-latency store.
///
/// # Contract
///
/// - A `Duplicate` decision is a hint, not a guarantee; false negatives
///   (saying `New` for something already stored) are acceptable.
/// - False positives (saying `Duplicate` for something new) should be
///   avoided because they prevent storage entirely, but the storage layer
///   provides the safety net.
/// - [`record`](DedupeStrategy::record) must be called **after** a
///   successful [`Storage::store`] to keep the fast-path cache consistent.
#[async_trait]
pub trait DedupeStrategy: Send + Sync {
    /// Check whether `dedupe_key` / `payload_hash` has been seen before.
    ///
    /// Returns a [`DedupeDecision`] indicating whether to proceed with
    /// storage (`New`), skip it (`Duplicate`), or defer to storage due to
    /// a key conflict (`Conflict`).
    async fn check(
        &self,
        dedupe_key: &str,
        payload_hash: &str,
    ) -> Result<DedupeDecision, DedupeError>;

    /// Record a newly accepted event so future `check` calls can detect it.
    ///
    /// Must be called after a receipt is durably stored to keep the
    /// advisory cache in sync with the authoritative storage layer.
    async fn record(&self, dedupe_key: &str, payload_hash: &str) -> Result<(), DedupeError>;
}

/// Forwards normalised webhook events to downstream consumers.
///
/// Downstream consumers may be message queues, HTTP endpoints, database
/// triggers, or any other integration target.
///
/// # Contract
///
/// - **Emission failure does NOT invalidate the stored receipt.**  Once a
///   [`WebhookReceipt`] has been durably stored (the provider has been
///   ACK'd), the event is committed.  If `emit` returns `Err`, the
///   pipeline transitions the receipt to [`ProcessingState::EmitFailed`]
///   and may retry, but the stored receipt remains intact.
/// - Implementations must be idempotent where the downstream consumer
///   supports it (e.g. publish with a message ID derived from
///   [`NormalizedEvent::receipt_id`]).
#[async_trait]
pub trait Emitter: Send + Sync {
    /// Forward a [`NormalizedEvent`] to one or more downstream targets.
    ///
    /// Returns `Ok(())` when the event has been successfully handed off.
    /// Returning `Err` signals a transient or permanent failure; the
    /// pipeline will record the error but the stored receipt is unaffected.
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError>;
}
