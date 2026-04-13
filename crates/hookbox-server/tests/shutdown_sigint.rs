//! Covers the SIGINT (Ctrl-C) branch of
//! `hookbox_server::shutdown::shutdown_signal`. See `shutdown_sigterm.rs`
//! for the companion test and rationale for the separate-file layout.

#![cfg(unix)]
// libc::raise is the only way to fire a signal at the current process from
// the test harness; the call is FFI-unsafe but has no runtime preconditions
// beyond the signal number being valid.
#![allow(unsafe_code)]
#![expect(clippy::expect_used, reason = "acceptable in test code")]

use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_signal_completes_on_sigint() {
    let handle = tokio::spawn(hookbox_server::shutdown::shutdown_signal());

    // Give tokio a moment to install both the ctrl_c and SIGTERM handlers.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // SAFETY: `libc::raise` is a FFI call with no preconditions beyond the
    // signal number being a valid signal; SIGINT is guaranteed by POSIX.
    let rc = unsafe { libc::raise(libc::SIGINT) };
    assert_eq!(rc, 0, "libc::raise(SIGINT) returned {rc}");

    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("shutdown_signal did not observe SIGINT within 2s")
        .expect("shutdown_signal task panicked");
}
