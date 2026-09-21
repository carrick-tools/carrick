//! One test, because it raises a real signal at its own process
//! (carrick#1235, carrick#1387).
//!
//! The signal is process-wide and so is the flag it sets, and the flag keeps
//! the FIRST signal on purpose — the one the run reports and exits with. A
//! second test in this binary therefore poisons this one whichever order they
//! run in and however they are serialised: with the other test first, this
//! read `Some(Terminate)` where it asked for `Some(Hangup)`, which is how it
//! failed in the merge queue after passing on the pull request. One test per
//! process is the only thing that makes it deterministic; the other signal
//! case is `tests/shutdown_drain_test.rs`.
//!
//! It proves its half by dying when the code it covers is deleted: without the
//! registration the raise takes the signal's default action and ends this
//! binary (`signal: 1, SIGHUP`), which is the loudest way to state that the
//! handler is what stands between the signal and the process.

#![cfg(unix)]

use carrick::shutdown::{Interrupted, Shutdown};
use std::time::Duration;

/// Closing the terminal a scan is running in is one of the three ways a laptop
/// scan ends without asking, and the one that used to leave no handler between
/// the signal and the process (carrick#1235).
///
/// It also proves the reach the watcher exists for (carrick#1387): the flag is
/// set by the watcher's own task, so a pass that is not awaiting anything can
/// read it — which is what every phase boundary of a scan now does — and the
/// second signal leaves it alone, because what a run reports is what stopped
/// it rather than what was said next.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_terminal_reaches_the_watcher_that_records_it() {
    let mut shutdown = carrick::shutdown::watch();
    // SAFETY: a signal to this process, whose handler was installed above.
    unsafe { libc::raise(libc::SIGHUP) };
    let caught = tokio::time::timeout(Duration::from_secs(30), shutdown.recv())
        .await
        .expect("SIGHUP reached the watcher");
    assert_eq!(caught, Shutdown::Hangup);
    assert_eq!(caught.name(), "SIGHUP");
    assert_eq!(caught.exit_code(), 129);
    assert_eq!(carrick::shutdown::requested(), Some(Shutdown::Hangup));
    assert_eq!(
        carrick::shutdown::check(),
        Err(Interrupted { signal: caught })
    );

    // A second signal is an instruction to go now, not a correction of what
    // stopped the run: the reported reason and the exit code stay the first
    // signal's.
    // SAFETY: as above.
    unsafe { libc::raise(libc::SIGTERM) };
    let second = tokio::time::timeout(Duration::from_secs(30), shutdown.recv())
        .await
        .expect("the second signal reached the watcher too");
    assert_eq!(second, Shutdown::Terminate);
    assert_eq!(carrick::shutdown::requested(), Some(Shutdown::Hangup));
}
