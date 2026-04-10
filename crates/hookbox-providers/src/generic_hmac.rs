//! Generic HMAC-SHA256 signature verifier for webhook providers.
//!
//! Any provider that signs the request body with HMAC-SHA256 and sends the
//! hex-encoded signature in a single HTTP header can be handled by
//! [`GenericHmacVerifier`].

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use http::HeaderMap;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use hookbox::state::{VerificationResult, VerificationStatus};
use hookbox::traits::SignatureVerifier;

type HmacSha256 = Hmac<Sha256>;

/// Configurable HMAC-SHA256 verifier for providers that sign the request body
/// and place the hex-encoded signature in a single HTTP header.
pub struct GenericHmacVerifier {
    provider: String,
    secret: Vec<u8>,
    header_name: String,
}

impl GenericHmacVerifier {
    /// Create a new [`GenericHmacVerifier`].
    ///
    /// # Parameters
    ///
    /// - `provider` — canonical provider name returned by [`provider_name`](SignatureVerifier::provider_name).
    /// - `secret` — raw bytes of the HMAC signing secret.
    /// - `header_name` — HTTP header that carries the hex-encoded signature.
    #[must_use]
    pub fn new(provider: &str, secret: Vec<u8>, header_name: String) -> Self {
        Self {
            provider: provider.to_owned(),
            secret,
            header_name,
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
impl SignatureVerifier for GenericHmacVerifier {
    fn provider_name(&self) -> &str {
        &self.provider
    }

    async fn verify(&self, headers: &HeaderMap, body: &[u8]) -> VerificationResult {
        // 1. Get the signature header.
        let Some(header_value) = headers.get(&self.header_name) else {
            return Self::failed("missing_signature_header");
        };

        // 2. Parse the header to a string.
        let Ok(provided_hex) = header_value.to_str() else {
            return Self::failed("invalid_signature_header_encoding");
        };

        // 3. Hex-decode the provided signature.
        let Ok(provided_sig) = hex::decode(provided_hex) else {
            return Self::failed("invalid_hex_signature");
        };

        // 4. Create HMAC-SHA256 with the secret.
        let Ok(mut mac) = HmacSha256::new_from_slice(&self.secret) else {
            return Self::failed("invalid_secret_length");
        };

        // 5. Compute the expected HMAC over the body.
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
mod tests {
    use hmac::{Hmac, KeyInit, Mac};
    use http::{HeaderMap, HeaderValue};
    use sha2::Sha256;

    use hookbox::state::VerificationStatus;
    use hookbox::traits::SignatureVerifier;

    use super::GenericHmacVerifier;

    type HmacSha256 = Hmac<Sha256>;

    fn compute_sig(secret: &[u8], body: &[u8]) -> Option<String> {
        let Ok(mut mac) = HmacSha256::new_from_slice(secret) else {
            return None;
        };
        mac.update(body);
        Some(hex::encode(mac.finalize().into_bytes()))
    }

    #[tokio::test]
    async fn valid_signature_passes() {
        let secret = b"test-secret";
        let body = b"hello world";
        let Some(sig) = compute_sig(secret, body) else {
            return;
        };
        let Ok(header_val) = HeaderValue::from_str(&sig) else {
            return;
        };

        let verifier =
            GenericHmacVerifier::new("test-provider", secret.to_vec(), "X-Signature".to_owned());

        let mut headers = HeaderMap::new();
        headers.insert("X-Signature", header_val);

        let result = verifier.verify(&headers, body).await;

        assert_eq!(result.status, VerificationStatus::Verified);
        assert_eq!(result.reason.as_deref(), Some("signature_valid"));
    }

    #[tokio::test]
    async fn invalid_signature_fails() {
        let secret = b"test-secret";
        let body = b"hello world";
        let bad_sig = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let Ok(header_val) = HeaderValue::from_str(bad_sig) else {
            return;
        };

        let verifier =
            GenericHmacVerifier::new("test-provider", secret.to_vec(), "X-Signature".to_owned());

        let mut headers = HeaderMap::new();
        headers.insert("X-Signature", header_val);

        let result = verifier.verify(&headers, body).await;

        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("signature_mismatch"));
    }

    #[tokio::test]
    async fn missing_header_fails() {
        let secret = b"test-secret";
        let body = b"hello world";

        let verifier =
            GenericHmacVerifier::new("test-provider", secret.to_vec(), "X-Signature".to_owned());

        let result = verifier.verify(&HeaderMap::new(), body).await;

        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("missing_signature_header"));
    }
}
