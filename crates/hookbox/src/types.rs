//! Core domain types for webhook receipts, normalized events, and filters.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::state::{ProcessingState, VerificationStatus};

/// Opaque, strongly-typed identifier for a [`WebhookReceipt`].
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

/// Durable record of a single inbound webhook event.
///
/// A `WebhookReceipt` is written once (at the durable acceptance boundary)
/// and never mutated except for state transitions and emit bookkeeping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookReceipt {
    /// Unique identifier for this receipt.
    pub receipt_id: ReceiptId,
    /// Name of the webhook provider (e.g. `"stripe"`, `"github"`).
    pub provider_name: String,
    /// Provider-assigned event identifier, if present in the payload or headers.
    pub provider_event_id: Option<String>,
    /// Caller-supplied opaque reference (e.g. an internal order ID).
    pub external_reference: Option<String>,
    /// Stable key used for idempotent deduplication.
    pub dedupe_key: String,
    /// Hex-encoded SHA-256 hash of [`raw_body`](Self::raw_body).
    pub payload_hash: String,
    /// Immutable copy of the original request body bytes, base64-encoded in JSON.
    #[serde(with = "serde_bytes_base64")]
    pub raw_body: Vec<u8>,
    /// JSON-parsed representation of the payload, if parsing succeeded.
    pub parsed_payload: Option<serde_json::Value>,
    /// Raw HTTP headers from the inbound request, stored as a JSON object.
    pub raw_headers: serde_json::Value,
    /// Normalised event type string (e.g. `"payment.succeeded"`).
    pub normalized_event_type: Option<String>,
    /// Outcome of signature verification.
    pub verification_status: VerificationStatus,
    /// Human-readable explanation for the verification outcome, if any.
    pub verification_reason: Option<String>,
    /// Current lifecycle state of this receipt.
    pub processing_state: ProcessingState,
    /// Number of times this event has been forwarded to downstream emitters.
    pub emit_count: i32,
    /// Most recent error encountered during processing, if any.
    pub last_error: Option<String>,
    /// Timestamp when the raw HTTP request was received.
    pub received_at: DateTime<Utc>,
    /// Timestamp when processing completed (all emission targets succeeded).
    pub processed_at: Option<DateTime<Utc>>,
    /// Arbitrary provider- or caller-supplied metadata as a JSON object.
    pub metadata: serde_json::Value,
}

/// Normalised, provider-agnostic view of a webhook event, suitable for
/// downstream consumers that should not need to deal with raw bytes or
/// provider-specific fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedEvent {
    /// Identifier of the originating [`WebhookReceipt`].
    pub receipt_id: ReceiptId,
    /// Name of the originating webhook provider.
    pub provider_name: String,
    /// Normalised event type, if available.
    pub event_type: Option<String>,
    /// Caller-supplied opaque external reference, if present.
    pub external_reference: Option<String>,
    /// JSON-parsed payload, if parsing succeeded.
    pub parsed_payload: Option<serde_json::Value>,
    /// Hex-encoded SHA-256 hash of the original raw body.
    pub payload_hash: String,
    /// Timestamp when the raw HTTP request was received.
    pub received_at: DateTime<Utc>,
    /// Arbitrary metadata forwarded from the originating receipt.
    pub metadata: serde_json::Value,
}

/// Filter parameters for querying stored webhook receipts.
///
/// All fields are optional; omitting a field means "no constraint on that
/// dimension". `limit` and `offset` implement page-based pagination.
#[derive(Debug, Clone, Default)]
pub struct ReceiptFilter {
    /// Restrict results to receipts from this provider.
    pub provider_name: Option<String>,
    /// Restrict results to receipts in this processing state.
    pub processing_state: Option<ProcessingState>,
    /// Restrict results to receipts with this external reference.
    pub external_reference: Option<String>,
    /// Restrict results to receipts with this provider event ID.
    pub provider_event_id: Option<String>,
    /// Maximum number of receipts to return.
    pub limit: Option<i64>,
    /// Number of receipts to skip before returning results.
    pub offset: Option<i64>,
}

/// Private serde module for serialising `Vec<u8>` as base64 in JSON.
mod serde_bytes_base64 {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer, de};

    pub(super) fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        STANDARD.decode(s.as_bytes()).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_id_new_is_unique() {
        let a = ReceiptId::new();
        let b = ReceiptId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn receipt_id_display_is_uuid_format() {
        let id = ReceiptId::new();
        let displayed = id.to_string();
        // UUID v4 format: 8-4-4-4-12 hex chars separated by hyphens (36 chars total)
        assert_eq!(displayed.len(), 36);
        let parts: Vec<&str> = displayed.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[2].len(), 4);
        assert_eq!(parts[3].len(), 4);
        assert_eq!(parts[4].len(), 12);
    }

    #[test]
    fn receipt_id_display_matches_inner_uuid() {
        let uuid = Uuid::new_v4();
        let id = ReceiptId(uuid);
        assert_eq!(id.to_string(), uuid.to_string());
    }
}
