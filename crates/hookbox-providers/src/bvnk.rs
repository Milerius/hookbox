//! BVNK webhook signature verifier.
//!
//! Handles BVNK's new hook service (Base64 HMAC-SHA256).
//! The signature is placed in the `x-signature` header and is Base64-encoded.
//!
//! Note: The older Standard Webhooks format uses hex — use `GenericHmacVerifier` for that.

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

/// Verifies BVNK webhook signatures (new hook service) using the `x-signature` header.
///
/// BVNK signs webhooks with HMAC-SHA256 over the raw request body and
/// encodes the result as a Base64 string placed in the `x-signature` header.
///
/// This handles BVNK's new hook service (Base64 HMAC). The older Standard Webhooks
/// format uses hex — use `GenericHmacVerifier` for that.
pub struct BvnkVerifier {
    provider: String,
    secret: Vec<u8>,
}

impl BvnkVerifier {
    /// Create a new [`BvnkVerifier`].
    #[must_use]
    pub fn new(provider: &str, secret: Vec<u8>) -> Self {
        Self {
            provider: provider.to_owned(),
            secret,
        }
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
impl SignatureVerifier for BvnkVerifier {
    fn provider_name(&self) -> &str {
        &self.provider
    }

    async fn verify(&self, headers: &HeaderMap, body: &[u8]) -> VerificationResult {
        // 1. Get the x-signature header.
        let Some(header_value) = headers.get("x-signature") else {
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

        // 4. Create HMAC-SHA256 with the raw secret bytes.
        let Ok(mut mac) = HmacSha256::new_from_slice(&self.secret) else {
            return Self::failed("invalid_secret_length");
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

    use super::BvnkVerifier;

    type HmacSha256 = Hmac<Sha256>;

    fn compute_sig(secret: &[u8], body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret).expect("valid secret");
        mac.update(body);
        STANDARD.encode(mac.finalize().into_bytes())
    }

    #[tokio::test]
    async fn valid_signature_passes() {
        let secret = b"bvnk-test-secret";
        let body = b"{\"type\":\"PAYMENT_RECEIVED\",\"amount\":100}";
        let sig = compute_sig(secret, body);
        let header_val = HeaderValue::from_str(&sig).expect("valid header value");

        let verifier = BvnkVerifier::new("bvnk", secret.to_vec());

        let mut headers = HeaderMap::new();
        headers.insert("x-signature", header_val);

        let result = verifier.verify(&headers, body).await;

        assert_eq!(result.status, VerificationStatus::Verified);
        assert_eq!(result.reason.as_deref(), Some("signature_valid"));
    }

    #[tokio::test]
    async fn invalid_signature_fails() {
        let secret = b"bvnk-test-secret";
        let body = b"{\"type\":\"PAYMENT_RECEIVED\"}";
        // Wrong signature — valid base64 but wrong HMAC.
        let header_val = HeaderValue::from_str("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .expect("valid header value");

        let verifier = BvnkVerifier::new("bvnk", secret.to_vec());

        let mut headers = HeaderMap::new();
        headers.insert("x-signature", header_val);

        let result = verifier.verify(&headers, body).await;

        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("signature_mismatch"));
    }

    #[tokio::test]
    async fn missing_header_fails() {
        let verifier = BvnkVerifier::new("bvnk", b"bvnk-test-secret".to_vec());

        let result = verifier.verify(&HeaderMap::new(), b"irrelevant body").await;

        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("missing_signature_header"));
    }

    #[tokio::test]
    async fn invalid_base64_signature_fails() {
        let secret = b"bvnk-test-secret";
        let header_val = HeaderValue::from_str("!!!not-base64!!!").expect("valid header bytes");

        let verifier = BvnkVerifier::new("bvnk", secret.to_vec());

        let mut headers = HeaderMap::new();
        headers.insert("x-signature", header_val);

        let result = verifier.verify(&headers, b"body").await;

        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("invalid_base64_signature"));
    }

    #[tokio::test]
    async fn provider_name_returns_configured() {
        let verifier = BvnkVerifier::new("bvnk-prod", b"secret".to_vec());
        assert_eq!(verifier.provider_name(), "bvnk-prod");
    }
}
