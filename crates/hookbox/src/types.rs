//! Core domain types for webhook receipts, normalized events, and filters.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::state::{ProcessingState, VerificationStatus};

pub use crate::state::ReceiptId;

/// Stable key used for idempotent deduplication of webhook events.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DedupeKey(pub String);

impl fmt::Display for DedupeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Hex-encoded SHA-256 hash of a webhook event's raw body bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PayloadHash(pub String);

impl fmt::Display for PayloadHash {
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[expect(clippy::expect_used, reason = "expect is acceptable in test code")]
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::state::{ProcessingState, VerificationStatus};

    #[test]
    fn dedupe_key_display() {
        let key = DedupeKey("stripe:abc123".to_owned());
        assert_eq!(key.to_string(), "stripe:abc123");
    }

    #[test]
    fn payload_hash_display() {
        let hash = PayloadHash("deadbeef".to_owned());
        assert_eq!(hash.to_string(), "deadbeef");
    }

    #[test]
    fn raw_body_deserialize_rejects_bad_base64() {
        // Provide an invalid base64 string for raw_body; deserialize must fail.
        let bad_json = r#"{
            "receipt_id": "00000000-0000-0000-0000-000000000001",
            "provider_name": "test",
            "provider_event_id": null,
            "external_reference": null,
            "dedupe_key": "k",
            "payload_hash": "h",
            "raw_body": "!!!not-valid-base64!!!",
            "parsed_payload": null,
            "raw_headers": {},
            "normalized_event_type": null,
            "verification_status": "skipped",
            "verification_reason": null,
            "processing_state": "stored",
            "emit_count": 0,
            "last_error": null,
            "received_at": "2024-01-01T00:00:00Z",
            "processed_at": null,
            "metadata": {}
        }"#;
        let result = serde_json::from_str::<WebhookReceipt>(bad_json);
        assert!(
            result.is_err(),
            "expected deserialization to fail on bad base64"
        );
    }

    #[test]
    fn receipt_id_new_is_unique() {
        let a = ReceiptId::new();
        let b = ReceiptId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn receipt_id_display_matches_inner_uuid() {
        let uuid = Uuid::new_v4();
        let id = ReceiptId(uuid);
        assert_eq!(id.to_string(), uuid.to_string());
    }

    #[test]
    fn webhook_receipt_raw_body_round_trips_via_serde() {
        let raw_body = b"hello webhook payload".to_vec();
        let receipt = WebhookReceipt {
            receipt_id: ReceiptId::new(),
            provider_name: "test-provider".to_string(),
            provider_event_id: None,
            external_reference: None,
            dedupe_key: "key-abc".to_string(),
            payload_hash: "deadbeef".to_string(),
            raw_body: raw_body.clone(),
            parsed_payload: None,
            raw_headers: json!({}),
            normalized_event_type: None,
            verification_status: VerificationStatus::Skipped,
            verification_reason: None,
            processing_state: ProcessingState::Stored,
            emit_count: 0,
            last_error: None,
            received_at: Utc::now(),
            processed_at: None,
            metadata: json!({}),
        };

        let serialized = serde_json::to_string(&receipt).expect("serialization should succeed");
        let deserialized: WebhookReceipt =
            serde_json::from_str(&serialized).expect("deserialization should succeed");
        assert_eq!(deserialized.raw_body, raw_body);
    }
}
