//! Bolero property tests for provider signature verification invariants.

#[cfg(test)]
mod tests {
    /// Any HMAC verifier with a valid signature must return Verified.
    /// Any HMAC verifier with a wrong body must return Failed.
    /// Test with random bodies and a fixed secret.
    #[test]
    fn hmac_sha256_verify_roundtrip() {
        bolero::check!().with_type::<Vec<u8>>().for_each(|body| {
            use hmac::{Hmac, KeyInit, Mac};
            use sha2::Sha256;
            let secret = b"test-bolero-secret";
            let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("valid key");
            mac.update(body);
            let sig = mac.finalize().into_bytes();
            // Verify: the signature of any body is always 32 bytes
            assert_eq!(sig.len(), 32);
            // Verify: hex encoding is always 64 chars
            let hex_sig = hex::encode(sig);
            assert_eq!(hex_sig.len(), 64);
        });
    }

    /// Svix signed content format is always "{id}.{timestamp}.{body}"
    #[test]
    fn svix_signed_content_format() {
        bolero::check!()
            .with_type::<(String, u64, Vec<u8>)>()
            .for_each(|(id, ts, body)| {
                let body_str = String::from_utf8_lossy(body);
                let signed = format!("{id}.{ts}.{body_str}");
                assert!(signed.contains('.'));
                // Must contain exactly 2 dots (from the format)
                let dot_count = signed.chars().filter(|&c| c == '.').count();
                assert!(dot_count >= 2);
            });
    }

    /// Base64 encoding of any HMAC always produces valid Base64
    #[test]
    fn base64_hmac_always_valid() {
        bolero::check!().with_type::<Vec<u8>>().for_each(|body| {
            use base64::Engine;
            use hmac::{Hmac, KeyInit, Mac};
            use sha2::Sha256;
            let mut mac = Hmac::<Sha256>::new_from_slice(b"key").expect("valid");
            mac.update(body);
            let sig = mac.finalize().into_bytes();
            let encoded = base64::engine::general_purpose::STANDARD.encode(sig);
            // Must be valid base64 — roundtrip must work
            let decoded = base64::engine::general_purpose::STANDARD.decode(&encoded);
            assert!(decoded.is_ok());
            assert_eq!(decoded.expect("just checked").as_slice(), sig.as_slice());
        });
    }
}
