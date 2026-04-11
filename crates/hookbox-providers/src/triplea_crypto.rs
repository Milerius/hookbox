//! Triple-A Crypto webhook signature verifier.
//!
//! Verifies the `triplea-signature` header format `t=<timestamp>,v1=<signature>`
//! as documented at <https://developers.triple-a.io/docs/triplea-api-doc/4c87b81419436-webhook-notifications>.

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

/// Verifies Triple-A Crypto webhook signatures using the `triplea-signature` header.
///
/// Triple-A signs webhooks with HMAC-SHA256 over `"{timestamp}.{body}"` using
/// the `notify_secret` as the raw key, and encodes the result as a hex string
/// placed in the `v1=` field of the header.
pub struct TripleACryptoVerifier {
    provider: String,
    secret: String,
    tolerance: Duration,
}

impl TripleACryptoVerifier {
    /// Create a new [`TripleACryptoVerifier`] with the default 300-second tolerance.
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
impl SignatureVerifier for TripleACryptoVerifier {
    fn provider_name(&self) -> &str {
        &self.provider
    }

    async fn verify(&self, headers: &HeaderMap, body: &[u8]) -> VerificationResult {
        // 1. Get the triplea-signature header.
        let Some(header_value) = headers.get("triplea-signature") else {
            return Self::failed("missing_signature_header");
        };

        // 2. Parse the header to a string.
        let Ok(header_str) = header_value.to_str() else {
            return Self::failed("invalid_signature_header_encoding");
        };

        // 3. Parse t=<timestamp> and all v1=<signature> values from comma-separated parts.
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

        // 7. Check if any v1= signature matches.
        for provided_hex in &v1_sigs {
            let Ok(provided_sig) = hex::decode(provided_hex) else {
                continue; // skip malformed sigs, try next
            };
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

    use super::TripleACryptoVerifier;

    type HmacSha256 = Hmac<Sha256>;

    fn compute_sig(secret: &str, timestamp: u64, body: &[u8]) -> String {
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

    /// Official sample data from the Triple-A documentation.
    #[tokio::test]
    async fn official_sample_data_verifies() {
        let body = br#"{"txs":[{"txid":"33d5d9af65fe63b12e1a4d67f6b05fcf01428764db840463aba621daa65323d3","status":"good","t3a_id":"164f404b07bd6c1868f35e8e70d3c2a245b6be14ec067f0ab50c9f0785b0b3cf","vout_n":0,"status_date":"2021-05-17T10:22:42.949Z","payment_tier":"good","order_currency":"USD","payment_amount":10,"receive_amount":10,"payment_currency":"USD","payment_tier_date":"2021-05-17T10:22:42.949Z","payment_crypto_amount":0.00023271}],"cart":{"items":[{"sku":"2736829","label":"A tale of 2 cities","amount":10.99,"quantity":1}],"tax_cost":0.73,"shipping_cost":2.57,"shipping_discount":0},"event":"payment","api_id":"HA1587722191gQKF_t","status":"good","status_date":"2021-05-17T10:22:42.954Z","order_amount":10,"payment_tier":"good","webhook_data":{"order_id":"ABC12345-2"},"crypto_amount":0.00023271,"exchange_rate":42971.44,"crypto_address":"n3XE3iEc2nyB44N43166XyzhEuKpcse9aQ","order_currency":"USD","payment_amount":10,"receive_amount":10,"crypto_currency":"testBTC","payment_currency":"USD","payment_reference":"PMA-401443-PMT","payment_tier_date":"2021-05-17T10:22:42.954Z","payment_crypto_amount":0.00023271}"#;
        let secret = "Cf9mx4nAvRuy5vwBY2FCtaKr!@#";
        let header =
            "t=1621246963,v1=a0749b4b490e15701ab2c488037da9367e8421d8cbddf2450071758e2c6c9f7d";

        // Use MAX tolerance because the timestamp is from 2021.
        let verifier = TripleACryptoVerifier::new("triplea".to_owned(), secret.to_owned())
            .with_tolerance(Duration::from_secs(u64::MAX / 2));

        let mut headers = HeaderMap::new();
        headers.insert(
            "triplea-signature",
            HeaderValue::from_str(header).expect("valid header value"),
        );

        let result = verifier.verify(&headers, body).await;

        assert_eq!(result.status, VerificationStatus::Verified);
        assert_eq!(result.reason.as_deref(), Some("signature_valid"));
    }

    #[tokio::test]
    async fn valid_signature_passes() {
        let secret = "test_notify_secret";
        let body = b"{\"event\":\"payment\",\"status\":\"good\"}";
        let ts = now_secs();
        let sig = compute_sig(secret, ts, body);
        let header_str = format!("t={ts},v1={sig}");

        let verifier = TripleACryptoVerifier::new("triplea".to_owned(), secret.to_owned());
        let mut headers = HeaderMap::new();
        headers.insert(
            "triplea-signature",
            HeaderValue::from_str(&header_str).expect("valid header value"),
        );

        let result = verifier.verify(&headers, body).await;

        assert_eq!(result.status, VerificationStatus::Verified);
        assert_eq!(result.reason.as_deref(), Some("signature_valid"));
    }

    #[tokio::test]
    async fn expired_timestamp_fails() {
        let secret = "test_notify_secret";
        let body = b"{\"event\":\"payment\"}";
        // Timestamp 10 minutes in the past — beyond the default 300s tolerance.
        let ts = now_secs().saturating_sub(601);
        let sig = compute_sig(secret, ts, body);
        let header_str = format!("t={ts},v1={sig}");

        let verifier = TripleACryptoVerifier::new("triplea".to_owned(), secret.to_owned());
        let mut headers = HeaderMap::new();
        headers.insert(
            "triplea-signature",
            HeaderValue::from_str(&header_str).expect("valid header value"),
        );

        let result = verifier.verify(&headers, body).await;

        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("timestamp_expired"));
    }

    #[tokio::test]
    async fn wrong_secret_fails() {
        let body = b"{\"event\":\"payment\"}";
        let ts = now_secs();
        let sig = compute_sig("wrong_secret", ts, body);
        let header_str = format!("t={ts},v1={sig}");

        let verifier =
            TripleACryptoVerifier::new("triplea".to_owned(), "correct_secret".to_owned());
        let mut headers = HeaderMap::new();
        headers.insert(
            "triplea-signature",
            HeaderValue::from_str(&header_str).expect("valid header value"),
        );

        let result = verifier.verify(&headers, body).await;

        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("signature_mismatch"));
    }

    #[tokio::test]
    async fn missing_header_fails() {
        let verifier =
            TripleACryptoVerifier::new("triplea".to_owned(), "test_notify_secret".to_owned());

        let result = verifier.verify(&HeaderMap::new(), b"irrelevant body").await;

        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("missing_signature_header"));
    }

    #[tokio::test]
    async fn with_tolerance_builder() {
        let secret = "test_notify_secret";
        let body = b"{}";
        let ts = now_secs();
        let sig = compute_sig(secret, ts, body);
        let header_str = format!("t={ts},v1={sig}");

        let verifier = TripleACryptoVerifier::new("triplea".to_owned(), secret.to_owned())
            .with_tolerance(Duration::from_secs(60));
        let mut headers = HeaderMap::new();
        headers.insert(
            "triplea-signature",
            HeaderValue::from_str(&header_str).expect("valid header value"),
        );

        let result = verifier.verify(&headers, body).await;
        assert_eq!(result.status, VerificationStatus::Verified);
    }

    #[tokio::test]
    async fn provider_name_returns_configured_name() {
        let verifier = TripleACryptoVerifier::new("triplea-prod".to_owned(), "secret".to_owned());
        assert_eq!(verifier.provider_name(), "triplea-prod");
    }
}
