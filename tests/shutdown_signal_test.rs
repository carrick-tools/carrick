//! The two cases that raise a real signal at their own process
//! (carrick#1235, carrick#1387).
//!
//! A binary of their own, because a signal is process-wide and so is the flag
//! it sets: beside the two thousand unit tests of the library, a scan-driving
//! test running in another thread would read this one's SIGHUP as its own
//! interruption. Here there is nothing else in the process to tell.
//!
//! Each proves its half by dying when the code it covers is deleted. Without
//! the registration, the raise takes the signal's default action and ends this
//! binary — which is the loudest way to state that the handler is what stands
//! between the signal and the process.

#![cfg(unix)]

use carrick::shutdown::{Ended, INTERRUPTION_REPORT_BUDGET, Interrupted, Shutdown};
use serial_test::serial;
use std::time::Duration;

/// Closing the terminal a scan is running in is one of the three ways a laptop
/// scan ends without asking, and the one that used to leave no handler between
/// the signal and the process (carrick#1235).
///
/// It also proves the reach the watcher exists for (carrick#1387): the flag is
/// set by the watcher's own task, so a pass that is not awaiting anything can
/// read it — which is what every phase boundary of a scan now does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(raises_a_signal)]
async fn a_closed_terminal_reaches_the_watcher_that_records_it() {
    // DELETION PROBE: no watcher, so no handler.
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
}

/// A signal a pass read off the flag must not be the answer to "has a second
/// one arrived" (carrick#1387).
///
/// The run that ends this way never consumed the signal that ended it, and the
/// report it then makes is raced against the next one. Undrained, that race
/// resolves on the signal that is already there and abandons the report in its
/// first poll — the escape #1235 built for a user pressing Ctrl-C twice, fired
/// by the first press.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(raises_a_signal)]
async fn a_signal_a_pass_noticed_is_not_read_as_the_second_one() {
    let mut shutdown = carrick::shutdown::watch();
    // SAFETY: a signal to this process, whose handler was installed above.
    unsafe { libc::raise(libc::SIGTERM) };
    // Wait for it to be IN the channel unconsumed, which is the state this is
    // about, and drop it there as the interruption path does.
    let drained = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if shutdown.drain() > 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(drained.is_ok(), "the watcher never delivered the signal");

    let ended = tokio::time::timeout(
        Duration::from_secs(300),
        carrick::shutdown::within_budget(
            std::future::ready(()),
            shutdown.recv(),
            INTERRUPTION_REPORT_BUDGET,
        ),
    )
    .await
    .expect("the report is not left without a way out");
    assert_eq!(ended, Ended::Reported);
}
