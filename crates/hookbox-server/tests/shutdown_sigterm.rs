//! Covers the SIGTERM branch of `hookbox_server::shutdown::shutdown_signal`.
//!
//! This test raises SIGTERM against its own process *after* the tokio signal
//! handlers have been installed, so the runtime consumes the signal instead
//! of letting the default disposition terminate the test binary. Lives in
//! its own integration-test file so it runs in an isolated process.

#![cfg(unix)]
// libc::raise is the only way to fire a signal at the current process from
// the test harness; the call is FFI-unsafe but has no runtime preconditions
// beyond the signal number being valid.
#![allow(unsafe_code)]
#![expect(clippy::expect_used, reason = "acceptable in test code")]

use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_signal_completes_on_sigterm() {
    let handle = tokio::spawn(hookbox_server::shutdown::shutdown_signal());

    // Give tokio a moment to install both the ctrl_c and SIGTERM handlers.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // SAFETY: `libc::raise` is a FFI call with no preconditions beyond the
    // signal number being a valid signal; SIGTERM is guaranteed by POSIX.
    let rc = unsafe { libc::raise(libc::SIGTERM) };
    assert_eq!(rc, 0, "libc::raise(SIGTERM) returned {rc}");

    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("shutdown_signal did not observe SIGTERM within 2s")
        .expect("shutdown_signal task panicked");
}
