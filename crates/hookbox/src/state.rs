//! Processing state types for the webhook ingest pipeline.

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Opaque, strongly-typed identifier for a [`crate::types::WebhookReceipt`].
///
/// Wraps a [`Uuid`] v4 to prevent accidental use of arbitrary UUIDs in
/// receipt-related APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReceiptId(pub Uuid);

impl ReceiptId {
    /// Create a new, randomly-generated [`ReceiptId`].
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ReceiptId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ReceiptId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

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
/// machine-readable explanation.
#[derive(Debug, Clone)]
pub struct VerificationResult {
    /// Whether verification passed, failed, or was skipped.
    pub status: VerificationStatus,
    /// Optional machine-readable explanation (e.g. `"signature_valid"`, `"timestamp_expired"`).
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
        existing_id: ReceiptId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_id_default_is_valid() {
        // Exercises the Default impl (lines 23-27).
        let id = ReceiptId::default();
        // Default should produce a non-nil UUID.
        assert_ne!(id.0, uuid::Uuid::nil());
    }

    #[test]
    fn delivery_state_serde_round_trip() {
        let cases = [
            (DeliveryState::Pending, "\"pending\""),
            (DeliveryState::InFlight, "\"in_flight\""),
            (DeliveryState::Emitted, "\"emitted\""),
            (DeliveryState::Failed, "\"failed\""),
            (DeliveryState::DeadLettered, "\"dead_lettered\""),
        ];
        for (state, expected_json) in &cases {
            let json = serde_json::to_string(state).unwrap();
            assert_eq!(json, *expected_json, "serialize {state}");
            let decoded: DeliveryState = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, *state, "deserialize {state}");
        }
    }

    #[test]
    fn delivery_id_display_is_uuid_string() {
        let id = DeliveryId(uuid::Uuid::nil());
        assert_eq!(id.to_string(), "00000000-0000-0000-0000-000000000000");
    }
}

/// High-level result of attempting to ingest a single webhook event.
#[derive(Debug, Clone)]
pub enum IngestResult {
    /// The event was accepted and durably stored.
    Accepted {
        /// The ID assigned to the newly stored receipt.
        receipt_id: ReceiptId,
    },
    /// The event is a duplicate of a previously stored receipt.
    Duplicate {
        /// The ID of the previously stored receipt.
        existing_id: ReceiptId,
    },
    /// The event was rejected because signature verification failed.
    VerificationFailed {
        /// Human-readable explanation of why verification failed.
        reason: String,
    },
}

/// Opaque identifier for a single delivery attempt row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeliveryId(pub Uuid);

impl DeliveryId {
    /// Create a new, randomly-generated [`DeliveryId`].
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for DeliveryId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for DeliveryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// State machine for a single `(receipt_id, emitter_name)` delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    /// The delivery has been queued but not yet attempted.
    Pending,
    /// The delivery is currently being attempted.
    InFlight,
    /// The delivery was successfully forwarded to the downstream emitter.
    Emitted,
    /// The delivery attempt failed; may be retried.
    Failed,
    /// All retry attempts have been exhausted; the delivery is in the dead-letter queue.
    DeadLettered,
}

impl fmt::Display for DeliveryState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeliveryState::Pending => write!(f, "pending"),
            DeliveryState::InFlight => write!(f, "in_flight"),
            DeliveryState::Emitted => write!(f, "emitted"),
            DeliveryState::Failed => write!(f, "failed"),
            DeliveryState::DeadLettered => write!(f, "dead_lettered"),
        }
    }
}

/// A single delivery row: one attempt of one receipt against one emitter.
///
/// Multiple rows may share the same `(receipt_id, emitter_name)` pair —
/// each replay inserts a fresh row for audit history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookDelivery {
    /// Unique identifier for this delivery row.
    pub delivery_id: DeliveryId,
    /// The receipt this delivery belongs to.
    pub receipt_id: ReceiptId,
    /// Name of the emitter this delivery targets (matches the TOML config key).
    pub emitter_name: String,
    /// Current lifecycle state of this delivery.
    pub state: DeliveryState,
    /// Number of delivery attempts made so far.
    pub attempt_count: i32,
    /// Human-readable error from the most recent failed attempt, if any.
    pub last_error: Option<String>,
    /// Timestamp of the most recent delivery attempt, if any.
    pub last_attempt_at: Option<DateTime<Utc>>,
    /// Earliest time at which the next attempt may be made.
    pub next_attempt_at: DateTime<Utc>,
    /// Timestamp at which the delivery was successfully emitted, if any.
    pub emitted_at: Option<DateTime<Utc>>,
    /// When `true`, no further retries will be scheduled for this row.
    pub immutable: bool,
    /// Timestamp at which this delivery row was created.
    pub created_at: DateTime<Utc>,
}

/// Per-emitter retry policy for the background dispatch worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of dispatch attempts before promoting to `dead_lettered`.
    pub max_attempts: i32,
    /// Backoff for attempt 1 (the first failure).
    pub initial_backoff: Duration,
    /// Hard cap on computed backoff.
    pub max_backoff: Duration,
    /// Exponential growth factor (`base *= multiplier` per attempt).
    pub backoff_multiplier: f64,
    /// Fractional jitter added to the computed base backoff.
    /// Must be in `[0.0, 1.0]`. `0.0` = deterministic.
    pub jitter: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            initial_backoff: Duration::from_secs(30),
            max_backoff: Duration::from_secs(3600),
            backoff_multiplier: 2.0,
            jitter: 0.2,
        }
    }
}
