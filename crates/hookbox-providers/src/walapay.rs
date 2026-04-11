//! Walapay/Svix webhook signature verifier.
//!
//! Verifies Svix-style webhook signatures used by Walapay, as documented at
//! <https://docs.walapay.io/docs/verifying-webhook-signature>.
//!
//! The secret has the format `whsec_<base64_key>`. The signed content is
//! `{svix-id}.{svix-timestamp}.{body}`, HMAC-SHA256 with the decoded key,
//! and the result is Base64-encoded and compared against the `v1,<sig>` entries
//! in the `svix-signature` header.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, KeyInit, Mac};
use http::HeaderMap;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use hookbox::state::{VerificationResult, VerificationStatus};
use hookbox::traits::SignatureVerifier;

type HmacSha256 = Hmac<Sha256>;

/// Default replay-attack tolerance: 5 minutes.
const DEFAULT_TOLERANCE_SECS: u64 = 300;

/// Verifies Walapay (Svix) webhook signatures.
///
/// Uses the `svix-id`, `svix-timestamp`, and `svix-signature` headers.
/// The secret must be in `whsec_<base64_key>` format. The signed payload is
/// `"{svix-id}.{svix-timestamp}.{body}"`.
pub struct WalapayVerifier {
    provider: String,
    /// Decoded HMAC key bytes (from the base64 portion after `whsec_`).
    key: Vec<u8>,
    tolerance: Duration,
}

impl WalapayVerifier {
    /// Create a new [`WalapayVerifier`].
    ///
    /// Returns `None` if `secret` does not start with `whsec_` or the base64
    /// suffix cannot be decoded.
    #[must_use]
    pub fn new(provider: &str, secret: &str) -> Option<Self> {
        let b64 = secret.strip_prefix("whsec_")?;
        let key = STANDARD.decode(b64).ok()?;
        Some(Self {
            provider: provider.to_owned(),
            key,
            tolerance: Duration::from_secs(DEFAULT_TOLERANCE_SECS),
        })
    }

    /// Override the timestamp tolerance window.
    #[must_use]
    pub fn with_tolerance(mut self, tolerance: Duration) -> Self {
        self.tolerance = tolerance;
        self
    }

    fn failed(reason: &str) -> VerificationResult {
        VerificationResult {
            status: VerificationStatus::Failed,
            reason: Some(reason.to_owned()),
        }
    }

    fn verified(reason: &str) -> VerificationResult {
        VerificationResult {
            status: VerificationStatus::Verified,
            reason: Some(reason.to_owned()),
        }
    }
}

#[async_trait]
impl SignatureVerifier for WalapayVerifier {
    fn provider_name(&self) -> &str {
        &self.provider
    }

    async fn verify(&self, headers: &HeaderMap, body: &[u8]) -> VerificationResult {
        // 1. Extract required Svix headers.
        let Some(msg_id_val) = headers.get("svix-id") else {
            return Self::failed("missing_svix_id_header");
        };
        let Some(ts_val) = headers.get("svix-timestamp") else {
            return Self::failed("missing_svix_timestamp_header");
        };
        let Some(sig_val) = headers.get("svix-signature") else {
            return Self::failed("missing_svix_signature_header");
        };

        let Ok(msg_id) = msg_id_val.to_str() else {
            return Self::failed("invalid_svix_id_encoding");
        };
        let Ok(ts_str) = ts_val.to_str() else {
            return Self::failed("invalid_svix_timestamp_encoding");
        };
        let Ok(sig_str) = sig_val.to_str() else {
            return Self::failed("invalid_svix_signature_encoding");
        };

        // 2. Parse and validate the timestamp.
        let Ok(timestamp) = ts_str.parse::<u64>() else {
            return Self::failed("invalid_timestamp");
        };

        let raw_now = chrono::Utc::now().timestamp();
        let now = u64::try_from(raw_now).unwrap_or(0);
        if now.abs_diff(timestamp) > self.tolerance.as_secs() {
            return Self::failed("timestamp_expired");
        }

        // 3. Build signed content bytes: "{msg_id}.{timestamp}.{body}".
        // Use raw bytes directly so non-UTF-8 payloads are handled correctly.
        let mut signed_content = Vec::with_capacity(msg_id.len() + 1 + ts_str.len() + 1 + body.len());
        signed_content.extend_from_slice(msg_id.as_bytes());
        signed_content.push(b'.');
        signed_content.extend_from_slice(ts_str.as_bytes());
        signed_content.push(b'.');
        signed_content.extend_from_slice(body);

        // 4. Compute expected HMAC-SHA256 and Base64-encode it.
        let Ok(mut mac) = HmacSha256::new_from_slice(&self.key) else {
            return Self::failed("invalid_key_length");
        };
        mac.update(&signed_content);
        let expected_bytes = mac.finalize().into_bytes();
        let expected_b64 = STANDARD.encode(expected_bytes);

        // 5. Check all `v1,<sig>` entries in the space-separated svix-signature header.
        for entry in sig_str.split_whitespace() {
            let Some(provided_b64) = entry.strip_prefix("v1,") else {
                continue; // ignore unknown version prefixes
            };
            // Constant-time compare of the base64 strings.
            if expected_b64
                .as_bytes()
                .ct_eq(provided_b64.as_bytes())
                .into()
            {
                return Self::verified("signature_valid");
            }
        }

        Self::failed("signature_mismatch")
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "expect is acceptable in test code")]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use hmac::{Hmac, KeyInit, Mac};
    use http::{HeaderMap, HeaderValue};
    use sha2::Sha256;

    use hookbox::state::VerificationStatus;
    use hookbox::traits::SignatureVerifier;

    use super::WalapayVerifier;

    type HmacSha256 = Hmac<Sha256>;

    /// A fixed test secret in `whsec_<base64>` format.
    const TEST_SECRET: &str = "whsec_dGVzdF9rZXlfZm9yX3dhbGFwYXlfdGVzdHM=";
    /// The raw key behind `TEST_SECRET` (Base64 of `"test_key_for_walapay_tests"`).
    const TEST_KEY: &[u8] = b"test_key_for_walapay_tests";

    fn compute_sig(key: &[u8], msg_id: &str, timestamp: u64, body: &[u8]) -> String {
        let ts_str = timestamp.to_string();
        let mut signed_content = Vec::with_capacity(msg_id.len() + 1 + ts_str.len() + 1 + body.len());
        signed_content.extend_from_slice(msg_id.as_bytes());
        signed_content.push(b'.');
        signed_content.extend_from_slice(ts_str.as_bytes());
        signed_content.push(b'.');
        signed_content.extend_from_slice(body);
        let mut mac = HmacSha256::new_from_slice(key).expect("valid key");
        mac.update(&signed_content);
        STANDARD.encode(mac.finalize().into_bytes())
    }

    fn now_secs() -> u64 {
        let raw = chrono::Utc::now().timestamp();
        u64::try_from(raw).unwrap_or(0)
    }

    #[tokio::test]
    async fn valid_signature_passes() {
        let body = b"{\"event\":\"payment.completed\"}";
        let ts = now_secs();
        let msg_id = "msg_abc123";
        let sig = compute_sig(TEST_KEY, msg_id, ts, body);
        let header_sig = format!("v1,{sig}");

        let verifier = WalapayVerifier::new("walapay", TEST_SECRET).expect("valid secret format");
        let mut headers = HeaderMap::new();
        headers.insert("svix-id", HeaderValue::from_str(msg_id).expect("valid"));
        headers.insert(
            "svix-timestamp",
            HeaderValue::from_str(&ts.to_string()).expect("valid"),
        );
        headers.insert(
            "svix-signature",
            HeaderValue::from_str(&header_sig).expect("valid"),
        );

        let result = verifier.verify(&headers, body).await;
        assert_eq!(result.status, VerificationStatus::Verified);
        assert_eq!(result.reason.as_deref(), Some("signature_valid"));
    }

    #[tokio::test]
    async fn missing_svix_id_fails() {
        let verifier = WalapayVerifier::new("walapay", TEST_SECRET).expect("valid secret format");
        let ts = now_secs();
        let mut headers = HeaderMap::new();
        headers.insert(
            "svix-timestamp",
            HeaderValue::from_str(&ts.to_string()).expect("valid"),
        );
        headers.insert(
            "svix-signature",
            HeaderValue::from_str("v1,somesig").expect("valid"),
        );

        let result = verifier.verify(&headers, b"{}").await;
        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("missing_svix_id_header"));
    }

    #[tokio::test]
    async fn missing_svix_timestamp_fails() {
        let verifier = WalapayVerifier::new("walapay", TEST_SECRET).expect("valid secret format");
        let mut headers = HeaderMap::new();
        headers.insert("svix-id", HeaderValue::from_str("msg_abc").expect("valid"));
        headers.insert(
            "svix-signature",
            HeaderValue::from_str("v1,somesig").expect("valid"),
        );

        let result = verifier.verify(&headers, b"{}").await;
        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(
            result.reason.as_deref(),
            Some("missing_svix_timestamp_header")
        );
    }

    #[tokio::test]
    async fn expired_timestamp_fails() {
        let body = b"{\"event\":\"payment.completed\"}";
        // 10 minutes in the past — beyond the default 300s tolerance.
        let ts = now_secs().saturating_sub(601);
        let msg_id = "msg_expired";
        let sig = compute_sig(TEST_KEY, msg_id, ts, body);
        let header_sig = format!("v1,{sig}");

        let verifier = WalapayVerifier::new("walapay", TEST_SECRET).expect("valid secret format");
        let mut headers = HeaderMap::new();
        headers.insert("svix-id", HeaderValue::from_str(msg_id).expect("valid"));
        headers.insert(
            "svix-timestamp",
            HeaderValue::from_str(&ts.to_string()).expect("valid"),
        );
        headers.insert(
            "svix-signature",
            HeaderValue::from_str(&header_sig).expect("valid"),
        );

        let result = verifier.verify(&headers, body).await;
        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("timestamp_expired"));
    }

    #[tokio::test]
    async fn wrong_secret_fails() {
        let body = b"{\"event\":\"payment.completed\"}";
        let ts = now_secs();
        let msg_id = "msg_wrong";
        // Sign with a different key.
        let wrong_sig = compute_sig(b"wrong_key", msg_id, ts, body);
        let header_sig = format!("v1,{wrong_sig}");

        let verifier = WalapayVerifier::new("walapay", TEST_SECRET).expect("valid secret format");
        let mut headers = HeaderMap::new();
        headers.insert("svix-id", HeaderValue::from_str(msg_id).expect("valid"));
        headers.insert(
            "svix-timestamp",
            HeaderValue::from_str(&ts.to_string()).expect("valid"),
        );
        headers.insert(
            "svix-signature",
            HeaderValue::from_str(&header_sig).expect("valid"),
        );

        let result = verifier.verify(&headers, body).await;
        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("signature_mismatch"));
    }

    #[tokio::test]
    async fn multiple_signatures_any_match() {
        // Svix can send multiple space-separated signatures; any valid one should pass.
        let body = b"{\"event\":\"payment.completed\"}";
        let ts = now_secs();
        let msg_id = "msg_multi";
        let good_sig = compute_sig(TEST_KEY, msg_id, ts, body);
        let bad_sig = STANDARD.encode(b"not_a_real_signature");
        // Put the good sig second.
        let header_sig = format!("v1,{bad_sig} v1,{good_sig}");

        let verifier = WalapayVerifier::new("walapay", TEST_SECRET).expect("valid secret format");
        let mut headers = HeaderMap::new();
        headers.insert("svix-id", HeaderValue::from_str(msg_id).expect("valid"));
        headers.insert(
            "svix-timestamp",
            HeaderValue::from_str(&ts.to_string()).expect("valid"),
        );
        headers.insert(
            "svix-signature",
            HeaderValue::from_str(&header_sig).expect("valid"),
        );

        let result = verifier.verify(&headers, body).await;
        assert_eq!(result.status, VerificationStatus::Verified);
        assert_eq!(result.reason.as_deref(), Some("signature_valid"));
    }

    #[tokio::test]
    async fn invalid_secret_no_prefix_returns_none() {
        let result = WalapayVerifier::new("walapay", "no_whsec_prefix_here");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn invalid_secret_bad_base64_returns_none() {
        let result = WalapayVerifier::new("walapay", "whsec_!!!not_valid_base64!!!");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn provider_name_returns_configured_name() {
        let verifier = WalapayVerifier::new("walapay-prod", TEST_SECRET).expect("valid");
        assert_eq!(verifier.provider_name(), "walapay-prod");
    }
}
