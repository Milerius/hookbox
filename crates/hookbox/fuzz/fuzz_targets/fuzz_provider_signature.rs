#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Test that Base64 decoding of arbitrary data never panics
    use base64::Engine;
    let _ = base64::engine::general_purpose::STANDARD.decode(data);

    // Test that hex decoding of arbitrary data never panics
    let _ = hex::decode(data);

    // Test that parsing "t=X,v1=Y" format never panics
    if let Ok(s) = std::str::from_utf8(data) {
        for part in s.split(',') {
            if let Some((_key, _value)) = part.split_once('=') {
                // parsing succeeded without panic
            }
        }
    }
});
