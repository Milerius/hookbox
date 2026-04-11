//! Stripe webhook signature verifier.
//!
//! Verifies the `Stripe-Signature` header format `t=<timestamp>,v1=<signature>`
//! as documented in the Stripe webhooks guide.

use std::time::Duration;

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use http::HeaderMap;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use hookbox::state::{VerificationResult, VerificationStatus};
use hookbox::traits::SignatureVerifier;

type HmacSha256 = Hmac<Sha256>;

/// Default replay-attack tolerance: 5 minutes.
const DEFAULT_TOLERANCE_SECS: u64 = 300;

/// Verifies Stripe webhook signatures using the `Stripe-Signature` header.
///
/// Stripe signs webhooks with HMAC-SHA256 over `"{timestamp}.{body}"` and
/// encodes the result as a hex string placed in the `v1=` field of the header.
pub struct StripeVerifier {
    provider: String,
    secret: String,
    tolerance: Duration,
}

impl StripeVerifier {
    /// Create a new [`StripeVerifier`] with the default 300-second tolerance.
    #[must_use]
    pub fn new(provider: String, secret: String) -> Self {
        Self {
            provider,
            secret,
            tolerance: Duration::from_secs(DEFAULT_TOLERANCE_SECS),
        }
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
impl SignatureVerifier for StripeVerifier {
    fn provider_name(&self) -> &str {
        &self.provider
    }

    async fn verify(&self, headers: &HeaderMap, body: &[u8]) -> VerificationResult {
        // 1. Get the Stripe-Signature header.
        let Some(header_value) = headers.get("Stripe-Signature") else {
            return Self::failed("missing_signature_header");
        };

        // 2. Parse the header to a string.
        let Ok(header_str) = header_value.to_str() else {
            return Self::failed("invalid_signature_header_encoding");
        };

        // 3. Parse t=<timestamp> and all v1=<signature> values from comma-separated
        //    parts. Stripe can send multiple v1= signatures during secret rotation.
        let mut timestamp_str: Option<&str> = None;
        let mut v1_sigs: Vec<&str> = Vec::new();

        for part in header_str.split(',') {
            if let Some(ts) = part.strip_prefix("t=") {
                timestamp_str = Some(ts);
            } else if let Some(sig) = part.strip_prefix("v1=") {
                v1_sigs.push(sig);
            }
        }

        // 4. Ensure both fields are present.
        let Some(ts_str) = timestamp_str else {
            return Self::failed("missing_timestamp");
        };
        if v1_sigs.is_empty() {
            return Self::failed("missing_v1_signature");
        }

        let Ok(timestamp) = ts_str.parse::<u64>() else {
            return Self::failed("invalid_timestamp");
        };

        // 5. Check timestamp tolerance.
        let raw_now = chrono::Utc::now().timestamp();
        // Unix timestamps are non-negative for all practical webhook use cases;
        // a negative value means the system clock predates the epoch, which
        // we treat as timestamp 0 so the tolerance check rejects it correctly.
        let now = u64::try_from(raw_now).unwrap_or(0);
        if now.abs_diff(timestamp) > self.tolerance.as_secs() {
            return Self::failed("timestamp_expired");
        }

        // 6. Compute expected HMAC over "{timestamp}.{body}".
        let Ok(body_str) = std::str::from_utf8(body) else {
            return Self::failed("invalid_body_encoding");
        };
        let signed_payload = format!("{ts_str}.{body_str}");

        let Ok(mut mac) = HmacSha256::new_from_slice(self.secret.as_bytes()) else {
            return Self::failed("invalid_secret_length");
        };
        mac.update(signed_payload.as_bytes());
        let expected = mac.finalize().into_bytes();

        // 7. Check if any v1= signature matches (supports secret rotation).
        for provided_hex in &v1_sigs {
            let Ok(provided_sig) = hex::decode(provided_hex) else {
                continue; // skip malformed sigs, try next
            };
            // Constant-time compare.
            if expected.ct_eq(provided_sig.as_slice()).into() {
                return Self::verified("signature_valid");
            }
        }

        Self::failed("signature_mismatch")
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "expect is acceptable in test code")]
mod tests {
    use std::time::Duration;

    use hmac::{Hmac, KeyInit, Mac};
    use http::{HeaderMap, HeaderValue};
    use sha2::Sha256;

    use hookbox::state::VerificationStatus;
    use hookbox::traits::SignatureVerifier;

    use super::StripeVerifier;

    type HmacSha256 = Hmac<Sha256>;

    fn compute_stripe_sig(secret: &str, timestamp: u64, body: &[u8]) -> String {
        let body_str = std::str::from_utf8(body).expect("valid UTF-8 body");
        let signed_payload = format!("{timestamp}.{body_str}");
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("valid secret length");
        mac.update(signed_payload.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    fn now_secs() -> u64 {
        let raw = chrono::Utc::now().timestamp();
        u64::try_from(raw).unwrap_or(0)
    }

    #[tokio::test]
    async fn valid_stripe_signature() {
        let secret = "whsec_test_secret";
        let body = b"{\"type\":\"payment_intent.created\"}";
        let ts = now_secs();
        let sig = compute_stripe_sig(secret, ts, body);
        let header_str = format!("t={ts},v1={sig}");
        let header_val = HeaderValue::from_str(&header_str).expect("valid header value");

        let verifier = StripeVerifier::new("stripe".to_owned(), secret.to_owned());

        let mut headers = HeaderMap::new();
        headers.insert("Stripe-Signature", header_val);

        let result = verifier.verify(&headers, body).await;

        assert_eq!(result.status, VerificationStatus::Verified);
        assert_eq!(result.reason.as_deref(), Some("signature_valid"));
    }

    #[tokio::test]
    async fn expired_timestamp_fails() {
        let secret = "whsec_test_secret";
        let body = b"{\"type\":\"payment_intent.created\"}";
        // Timestamp 10 minutes in the past — beyond the default 300s tolerance.
        let ts = now_secs().saturating_sub(601);
        let sig = compute_stripe_sig(secret, ts, body);
        let header_str = format!("t={ts},v1={sig}");
        let header_val = HeaderValue::from_str(&header_str).expect("valid header value");

        let verifier = StripeVerifier::new("stripe".to_owned(), secret.to_owned());

        let mut headers = HeaderMap::new();
        headers.insert("Stripe-Signature", header_val);

        let result = verifier.verify(&headers, body).await;

        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("timestamp_expired"));
    }

    #[tokio::test]
    async fn wrong_secret_fails() {
        let body = b"{\"type\":\"payment_intent.created\"}";
        let ts = now_secs();
        // Sign with a different secret than the verifier will use.
        let sig = compute_stripe_sig("wrong_secret", ts, body);
        let header_str = format!("t={ts},v1={sig}");
        let header_val = HeaderValue::from_str(&header_str).expect("valid header value");

        let verifier = StripeVerifier::new("stripe".to_owned(), "correct_secret".to_owned());

        let mut headers = HeaderMap::new();
        headers.insert("Stripe-Signature", header_val);

        let result = verifier.verify(&headers, body).await;

        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("signature_mismatch"));
    }

    #[tokio::test]
    async fn missing_stripe_header_fails() {
        let verifier = StripeVerifier::new("stripe".to_owned(), "whsec_test_secret".to_owned());

        let result = verifier.verify(&HeaderMap::new(), b"irrelevant body").await;

        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("missing_signature_header"));
    }

    #[tokio::test]
    async fn with_tolerance_builder() {
        // Sanity check that `with_tolerance` compiles and doesn't change the
        // rejection logic when the tolerance is satisfied.
        let secret = "whsec_test_secret";
        let body = b"{}";
        let ts = now_secs();
        let sig = compute_stripe_sig(secret, ts, body);
        let header_str = format!("t={ts},v1={sig}");
        let header_val = HeaderValue::from_str(&header_str).expect("valid header value");

        let verifier = StripeVerifier::new("stripe".to_owned(), secret.to_owned())
            .with_tolerance(Duration::from_secs(60));

        let mut headers = HeaderMap::new();
        headers.insert("Stripe-Signature", header_val);

        let result = verifier.verify(&headers, body).await;
        assert_eq!(result.status, VerificationStatus::Verified);
    }

    #[tokio::test]
    async fn multiple_v1_signatures_during_rotation() {
        let old_secret = "whsec_old_secret";
        let new_secret = "whsec_new_secret";
        let body = b"{\"type\":\"charge.succeeded\"}";
        let ts = now_secs();

        let old_sig = compute_stripe_sig(old_secret, ts, body);
        let new_sig = compute_stripe_sig(new_secret, ts, body);

        // Stripe sends both signatures during rotation
        let header_str = format!("t={ts},v1={old_sig},v1={new_sig}");
        let header_val = HeaderValue::from_str(&header_str).expect("valid header value");

        // Verifier configured with the NEW secret should accept
        let verifier = StripeVerifier::new("stripe".to_owned(), new_secret.to_owned());
        let mut headers = HeaderMap::new();
        headers.insert("Stripe-Signature", header_val);

        let result = verifier.verify(&headers, body).await;
        assert_eq!(result.status, VerificationStatus::Verified);
        assert_eq!(result.reason.as_deref(), Some("signature_valid"));
    }

    #[tokio::test]
    async fn missing_timestamp_fails() {
        // Header with v1= but no t= field.
        let header_val = HeaderValue::from_str("v1=aabbccdd").expect("valid header value");
        let verifier = StripeVerifier::new("stripe".to_owned(), "whsec_test_secret".to_owned());
        let mut headers = HeaderMap::new();
        headers.insert("Stripe-Signature", header_val);

        let result = verifier.verify(&headers, b"{}").await;
        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("missing_timestamp"));
    }

    #[tokio::test]
    async fn missing_v1_signature_fails() {
        // Header with t= but no v1= field.
        let ts = now_secs();
        let header_str = format!("t={ts}");
        let header_val = HeaderValue::from_str(&header_str).expect("valid header value");
        let verifier = StripeVerifier::new("stripe".to_owned(), "whsec_test_secret".to_owned());
        let mut headers = HeaderMap::new();
        headers.insert("Stripe-Signature", header_val);

        let result = verifier.verify(&headers, b"{}").await;
        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("missing_v1_signature"));
    }

    #[tokio::test]
    async fn invalid_timestamp_fails() {
        // t= field is non-numeric.
        let header_val =
            HeaderValue::from_str("t=notanumber,v1=aabbccdd").expect("valid header value");
        let verifier = StripeVerifier::new("stripe".to_owned(), "whsec_test_secret".to_owned());
        let mut headers = HeaderMap::new();
        headers.insert("Stripe-Signature", header_val);

        let result = verifier.verify(&headers, b"{}").await;
        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("invalid_timestamp"));
    }

    #[tokio::test]
    async fn non_utf8_body_fails() {
        // Body bytes that are not valid UTF-8.
        let ts = now_secs();
        // Use timestamp within tolerance so we pass the tolerance check.
        let header_str = format!("t={ts},v1=aabbccdd");
        let header_val = HeaderValue::from_str(&header_str).expect("valid header value");
        let verifier = StripeVerifier::new("stripe".to_owned(), "whsec_test_secret".to_owned());
        let mut headers = HeaderMap::new();
        headers.insert("Stripe-Signature", header_val);

        // Invalid UTF-8 bytes.
        let bad_body: &[u8] = &[0xFF, 0xFE, 0x00];
        let result = verifier.verify(&headers, bad_body).await;
        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("invalid_body_encoding"));
    }

    #[tokio::test]
    async fn invalid_hex_in_v1_skips_to_mismatch() {
        // v1= contains non-hex characters — the bad sig is skipped, no valid
        // sig exists, so the final result is signature_mismatch.
        let secret = "whsec_test_secret";
        let body = b"{}";
        let ts = now_secs();
        let header_str = format!("t={ts},v1=ZZZZZZ");
        let header_val = HeaderValue::from_str(&header_str).expect("valid header value");
        let verifier = StripeVerifier::new("stripe".to_owned(), secret.to_owned());
        let mut headers = HeaderMap::new();
        headers.insert("Stripe-Signature", header_val);

        let result = verifier.verify(&headers, body).await;
        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("signature_mismatch"));
    }

    #[tokio::test]
    async fn provider_name_returns_configured_name() {
        let verifier = StripeVerifier::new("stripe-prod".to_owned(), "secret".to_owned());
        assert_eq!(verifier.provider_name(), "stripe-prod");
    }
}
