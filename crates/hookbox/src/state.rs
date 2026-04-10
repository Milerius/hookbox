//! Processing state types for the webhook ingest pipeline.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Lifecycle state of a webhook receipt through the ingest pipeline.
///
/// `Stored` is the durable acceptance boundary: once a receipt reaches this
/// state the provider has been ACK'd and the event is guaranteed to be
/// processed at least once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingState {
    /// The raw HTTP request has been received but not yet verified.
    Received,
    /// Signature verification passed.
    Verified,
    /// Signature verification failed; the receipt will not be processed further.
    VerificationFailed,
    /// The event was identified as a duplicate and will not be stored again.
    Duplicate,
    /// The receipt has been durably stored — the durable acceptance boundary.
    Stored,
    /// The event has been forwarded to at least one downstream emitter.
    Emitted,
    /// All downstream processing has completed successfully.
    Processed,
    /// Emission to one or more downstream targets failed.
    EmitFailed,
    /// The receipt has been moved to the dead-letter queue after exhausting retries.
    DeadLettered,
    /// The receipt is being replayed (e.g. manual re-emission after a fix).
    Replayed,
}

/// Outcome of signature verification for an inbound webhook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    /// The provider signature was present and valid.
    Verified,
    /// The provider signature was present but invalid, or required and absent.
    Failed,
    /// Verification was intentionally skipped (e.g. provider does not sign events).
    Skipped,
}

/// Full result of a signature verification attempt, including an optional
/// human-readable explanation.
#[derive(Debug, Clone)]
pub struct VerificationResult {
    /// Whether verification passed, failed, or was skipped.
    pub status: VerificationStatus,
    /// Optional human-readable explanation (e.g. error message on failure).
    pub reason: Option<String>,
}

/// Advisory decision produced by the fast-path dedupe strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupeDecision {
    /// The event has not been seen before; proceed with storage.
    New,
    /// The event matches a previously stored receipt; skip storage.
    Duplicate,
    /// The deduplication key conflicts with a different event; treat as new and
    /// let the authoritative storage layer resolve the conflict.
    Conflict,
}

/// Authoritative result returned by the storage layer after an attempted insert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreResult {
    /// The receipt was durably stored.
    Stored,
    /// A receipt with the same dedupe key already exists.
    Duplicate {
        /// The ID of the previously stored receipt.
        existing_id: Uuid,
    },
}

/// High-level result of attempting to ingest a single webhook event.
#[derive(Debug, Clone)]
pub enum IngestResult {
    /// The event was accepted and durably stored.
    Accepted {
        /// The ID assigned to the newly stored receipt.
        receipt_id: Uuid,
    },
    /// The event is a duplicate of a previously stored receipt.
    Duplicate {
        /// The ID of the previously stored receipt.
        existing_id: Uuid,
    },
    /// The event was rejected because signature verification failed.
    VerificationFailed {
        /// Human-readable explanation of why verification failed.
        reason: String,
    },
}
