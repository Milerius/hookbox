//! Property tests for [`compute_payload_hash`].

#![cfg(test)]

#[test]
fn hash_is_deterministic() {
    bolero::check!().with_type::<Vec<u8>>().for_each(|input| {
        let hash1 = hookbox::compute_payload_hash(input);
        let hash2 = hookbox::compute_payload_hash(input);
        assert_eq!(hash1, hash2, "same input must always produce the same hash");
    });
}

#[test]
fn hash_output_is_64_hex_characters() {
    bolero::check!().with_type::<Vec<u8>>().for_each(|input| {
        let hash = hookbox::compute_payload_hash(input);
        assert_eq!(
            hash.len(),
            64,
            "SHA-256 hex digest must be exactly 64 characters, got {} for input {:?}",
            hash.len(),
            input
        );
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit()),
            "hash must consist only of hex digits, got: {hash}"
        );
    });
}

#[test]
fn different_inputs_produce_different_hashes() {
    bolero::check!()
        .with_type::<(Vec<u8>, Vec<u8>)>()
        .for_each(|(a, b)| {
            if a == b {
                return;
            }
            let hash_a = hookbox::compute_payload_hash(a);
            let hash_b = hookbox::compute_payload_hash(b);
            assert_ne!(
                hash_a, hash_b,
                "distinct inputs must produce distinct SHA-256 hashes (collision found)"
            );
        });
}
