#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let hash = hookbox::compute_payload_hash(data);
    // Hash must always be 64 hex chars regardless of input
    assert_eq!(hash.len(), 64);
    assert!(hash.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
    // Must be deterministic
    assert_eq!(hash, hookbox::compute_payload_hash(data));
});
