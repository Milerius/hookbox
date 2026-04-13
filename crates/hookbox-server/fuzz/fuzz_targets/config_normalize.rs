#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        if let Ok(config) = toml::from_str::<hookbox_server::config::HookboxConfig>(s) {
            let _ = hookbox_server::config::normalize(config);
        }
    }
});
