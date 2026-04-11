//! Triple-A fiat payout webhook signature verifier.
//!
//! Verifies RSA-SHA512 (PKCS#1 v1.5) signatures from Triple-A fiat payout webhooks.
//! The signature is placed in the `TripleA-Signature` header as a Base64-encoded string
//! and is verified against the raw request body using a 4096-bit RSA public key.
//!
//! Reference: <https://developers.triple-a.io/docs/fiat-payout-api-doc/519646ce696e6-webhook-notifications>

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use http::HeaderMap;
use rsa::RsaPublicKey;
use rsa::pkcs1v15::{Signature, VerifyingKey};
use rsa::pkcs8::DecodePublicKey;
use rsa::sha2::Sha512;
use rsa::signature::Verifier;

use hookbox::state::{VerificationResult, VerificationStatus};
use hookbox::traits::SignatureVerifier;

/// Verifies Triple-A fiat payout webhook signatures using RSA-SHA512 (PKCS#1 v1.5).
///
/// Triple-A signs fiat payout webhooks with an RSA private key and places the
/// Base64-encoded signature in the `TripleA-Signature` HTTP header. This verifier
/// checks that signature against the raw request body using the corresponding
/// RSA public key (PEM format).
pub struct TripleAFiatVerifier {
    provider: String,
    verifying_key: VerifyingKey<Sha512>,
}

impl TripleAFiatVerifier {
    /// Create a new [`TripleAFiatVerifier`].
    ///
    /// Returns `None` if the PEM public key cannot be parsed.
    #[must_use]
    pub fn new(provider: &str, pem_public_key: &str) -> Option<Self> {
        let public_key = RsaPublicKey::from_public_key_pem(pem_public_key).ok()?;
        let verifying_key = VerifyingKey::<Sha512>::new(public_key);
        Some(Self {
            provider: provider.to_owned(),
            verifying_key,
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
impl SignatureVerifier for TripleAFiatVerifier {
    fn provider_name(&self) -> &str {
        &self.provider
    }

    async fn verify(&self, headers: &HeaderMap, body: &[u8]) -> VerificationResult {
        // 1. Get the TripleA-Signature header.
        let Some(header_value) = headers.get("TripleA-Signature") else {
            return Self::failed("missing_signature_header");
        };

        // 2. Parse the header to a str.
        let Ok(header_str) = header_value.to_str() else {
            return Self::failed("invalid_signature_header_encoding");
        };

        // 3. Base64-decode the provided signature.
        let Ok(sig_bytes) = STANDARD.decode(header_str) else {
            return Self::failed("invalid_base64_signature");
        };

        // 4. Convert to an RSA PKCS#1 v1.5 signature.
        let Ok(signature) = Signature::try_from(sig_bytes.as_slice()) else {
            return Self::failed("invalid_rsa_signature_format");
        };

        // 5. Verify the signature against the body.
        if self.verifying_key.verify(body, &signature).is_ok() {
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
    use http::{HeaderMap, HeaderValue};
    use rsa::RsaPrivateKey;
    use rsa::pkcs1v15::SigningKey;
    use rsa::pkcs8::EncodePublicKey;
    use rsa::sha2::Sha512;
    use rsa::signature::{SignatureEncoding, Signer};

    use hookbox::state::VerificationStatus;
    use hookbox::traits::SignatureVerifier;

    use super::TripleAFiatVerifier;

    /// Generate a test RSA keypair and return (PEM public key, signing key).
    fn test_keypair() -> (String, SigningKey<Sha512>) {
        let mut rng = rand::thread_rng();
        let private_key = RsaPrivateKey::new(&mut rng, 2048).expect("keygen failed");
        let pem = private_key
            .to_public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .expect("PEM encoding failed");
        let signing_key = SigningKey::<Sha512>::new(private_key);
        (pem, signing_key)
    }

    fn sign_body(signing_key: &SigningKey<Sha512>, body: &[u8]) -> String {
        let signature = signing_key.sign(body);
        STANDARD.encode(signature.to_bytes())
    }

    #[tokio::test]
    async fn valid_rsa_signature_passes() {
        let (pem, signing_key) = test_keypair();
        let body = b"{\"payout_id\":\"abc123\",\"status\":\"completed\"}";
        let sig_b64 = sign_body(&signing_key, body);

        let verifier = TripleAFiatVerifier::new("triplea-fiat", &pem).expect("valid PEM");

        let mut headers = HeaderMap::new();
        headers.insert(
            "TripleA-Signature",
            HeaderValue::from_str(&sig_b64).expect("valid header value"),
        );

        let result = verifier.verify(&headers, body).await;

        assert_eq!(result.status, VerificationStatus::Verified);
        assert_eq!(result.reason.as_deref(), Some("signature_valid"));
    }

    #[tokio::test]
    async fn invalid_signature_fails() {
        let (pem, signing_key) = test_keypair();
        let body = b"{\"payout_id\":\"abc123\"}";
        let wrong_body = b"{\"payout_id\":\"WRONG\"}";
        // Sign the wrong body so the signature won't match.
        let sig_b64 = sign_body(&signing_key, wrong_body);

        let verifier = TripleAFiatVerifier::new("triplea-fiat", &pem).expect("valid PEM");

        let mut headers = HeaderMap::new();
        headers.insert(
            "TripleA-Signature",
            HeaderValue::from_str(&sig_b64).expect("valid header value"),
        );

        let result = verifier.verify(&headers, body).await;

        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("signature_mismatch"));
    }

    #[tokio::test]
    async fn missing_header_fails() {
        let (pem, _signing_key) = test_keypair();
        let verifier = TripleAFiatVerifier::new("triplea-fiat", &pem).expect("valid PEM");

        let result = verifier.verify(&HeaderMap::new(), b"body").await;

        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("missing_signature_header"));
    }

    #[tokio::test]
    async fn invalid_pem_returns_none() {
        let result = TripleAFiatVerifier::new("triplea-fiat", "not-a-valid-pem");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn provider_name_returns_configured() {
        let (pem, _signing_key) = test_keypair();
        let verifier = TripleAFiatVerifier::new("triplea-prod", &pem).expect("valid PEM");
        assert_eq!(verifier.provider_name(), "triplea-prod");
    }
}
