//! Adyen webhook signature verifier.
//!
//! Verifies the `HmacSignature` header using HMAC-SHA256.
//! The key is provided as a hex-encoded string and decoded to binary bytes.
//! The signature is Base64-encoded.

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

/// Verifies Adyen webhook signatures using the `HmacSignature` header.
///
/// Adyen signs webhooks with HMAC-SHA256 over the raw request body and
/// encodes the result as a Base64 string placed in the `HmacSignature` header.
/// The signing key is provided as a hex-encoded string.
pub struct AdyenVerifier {
    provider: String,
    key_bytes: Vec<u8>,
}

impl AdyenVerifier {
    /// Create a new [`AdyenVerifier`].
    ///
    /// Returns `None` if `hex_key` cannot be decoded as a valid hex string.
    #[must_use]
    pub fn new(provider: &str, hex_key: &str) -> Option<Self> {
        let key_bytes = hex::decode(hex_key).ok()?;
        Some(Self {
            provider: provider.to_owned(),
            key_bytes,
        })
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
impl SignatureVerifier for AdyenVerifier {
    fn provider_name(&self) -> &str {
        &self.provider
    }

    async fn verify(&self, headers: &HeaderMap, body: &[u8]) -> VerificationResult {
        // 1. Get the HmacSignature header.
        let Some(header_value) = headers.get("HmacSignature") else {
            return Self::failed("missing_signature_header");
        };

        // 2. Parse the header to a str.
        let Ok(header_str) = header_value.to_str() else {
            return Self::failed("invalid_signature_header_encoding");
        };

        // 3. Base64-decode the provided signature.
        let Ok(provided_sig) = STANDARD.decode(header_str) else {
            return Self::failed("invalid_base64_signature");
        };

        // 4. Create HMAC-SHA256 with the decoded key bytes.
        let Ok(mut mac) = HmacSha256::new_from_slice(&self.key_bytes) else {
            return Self::failed("invalid_key_length");
        };

        // 5. Update with body and finalize.
        mac.update(body);
        let expected = mac.finalize().into_bytes();

        // 6. Constant-time compare.
        if expected.ct_eq(provided_sig.as_slice()).into() {
            Self::verified("signature_valid")
        } else {
            Self::failed("signature_mismatch")
        }
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

    use super::AdyenVerifier;

    type HmacSha256 = Hmac<Sha256>;

    fn compute_sig(key_bytes: &[u8], body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(key_bytes).expect("valid key");
        mac.update(body);
        STANDARD.encode(mac.finalize().into_bytes())
    }

    #[tokio::test]
    async fn valid_signature_passes() {
        let hex_key = "deadbeefdeadbeef";
        let key_bytes = hex::decode(hex_key).expect("valid hex");
        let body = b"{\"live\":\"false\",\"notificationItems\":[]}";
        let sig = compute_sig(&key_bytes, body);
        let header_val = HeaderValue::from_str(&sig).expect("valid header value");

        let verifier = AdyenVerifier::new("adyen", hex_key).expect("valid hex key");

        let mut headers = HeaderMap::new();
        headers.insert("HmacSignature", header_val);

        let result = verifier.verify(&headers, body).await;

        assert_eq!(result.status, VerificationStatus::Verified);
        assert_eq!(result.reason.as_deref(), Some("signature_valid"));
    }

    #[tokio::test]
    async fn invalid_signature_fails() {
        let hex_key = "deadbeefdeadbeef";
        let body = b"{\"live\":\"false\"}";
        // Wrong signature — just some valid base64.
        let header_val = HeaderValue::from_str("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .expect("valid header value");

        let verifier = AdyenVerifier::new("adyen", hex_key).expect("valid hex key");

        let mut headers = HeaderMap::new();
        headers.insert("HmacSignature", header_val);

        let result = verifier.verify(&headers, body).await;

        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("signature_mismatch"));
    }

    #[tokio::test]
    async fn missing_header_fails() {
        let verifier = AdyenVerifier::new("adyen", "deadbeefdeadbeef").expect("valid hex key");

        let result = verifier.verify(&HeaderMap::new(), b"irrelevant body").await;

        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("missing_signature_header"));
    }

    #[tokio::test]
    async fn invalid_hex_key_returns_none() {
        let result = AdyenVerifier::new("adyen", "not-valid-hex!!");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn invalid_base64_signature_fails() {
        let hex_key = "deadbeefdeadbeef";
        // Header value contains characters that are not valid base64.
        let header_val = HeaderValue::from_str("!!!not-base64!!!").expect("valid header bytes");

        let verifier = AdyenVerifier::new("adyen", hex_key).expect("valid hex key");

        let mut headers = HeaderMap::new();
        headers.insert("HmacSignature", header_val);

        let result = verifier.verify(&headers, b"body").await;

        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("invalid_base64_signature"));
    }

    #[tokio::test]
    async fn provider_name_returns_configured() {
        let verifier = AdyenVerifier::new("adyen-prod", "deadbeefdeadbeef").expect("valid hex key");
        assert_eq!(verifier.provider_name(), "adyen-prod");
    }
}
