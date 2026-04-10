//! Payload hashing utilities for webhook receipt deduplication and integrity.

use sha2::{Digest, Sha256};

/// Compute the SHA-256 hash of `body` and return it as a lowercase hex string.
///
/// The returned hash is used as the `payload_hash` on [`crate::WebhookReceipt`]
/// and as the dedupe key component that detects body-level changes between
/// otherwise identical events.
#[must_use]
pub fn compute_payload_hash(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    hex::encode(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_body_produces_known_sha256() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let hash = compute_payload_hash(b"");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn same_input_produces_same_hash() {
        let body = b"hello, webhook!";
        assert_eq!(compute_payload_hash(body), compute_payload_hash(body));
    }

    #[test]
    fn different_input_produces_different_hash() {
        let hash_a = compute_payload_hash(b"payload-a");
        let hash_b = compute_payload_hash(b"payload-b");
        assert_ne!(hash_a, hash_b);
    }
}
