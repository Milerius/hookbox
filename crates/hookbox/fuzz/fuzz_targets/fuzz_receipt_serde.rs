#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Try to parse arbitrary bytes as a WebhookReceipt JSON.
    // This should never panic — just return Ok or Err.
    let _: Result<hookbox::WebhookReceipt, _> = serde_json::from_slice(data);

    // Also test ProcessingState deserialization
    if let Ok(s) = std::str::from_utf8(data) {
        let _: Result<hookbox::ProcessingState, _> = serde_json::from_str(s);
        let _: Result<hookbox::VerificationStatus, _> = serde_json::from_str(s);
    }
});
