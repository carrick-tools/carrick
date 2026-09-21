//! One test, because it raises a real signal at its own process
//! (carrick#1387). `tests/shutdown_signal_test.rs` says why that means a
//! binary each.

#![cfg(unix)]

use carrick::shutdown::{Ended, INTERRUPTION_REPORT_BUDGET};
use std::time::Duration;

/// A signal a pass read off the flag must not be the answer to "has a second
/// one arrived" (carrick#1387).
///
/// The run that ends this way never consumed the signal that ended it, and the
/// report it then makes is raced against the next one. Undrained, that race
/// resolves on the signal that is already there and abandons the report in its
/// first poll — the escape #1235 built for a user pressing Ctrl-C twice, fired
/// by the first press.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_signal_a_pass_noticed_is_not_read_as_the_second_one() {
    let mut shutdown = carrick::shutdown::watch();
    // SAFETY: a signal to this process, whose handler was installed above.
    unsafe { libc::raise(libc::SIGTERM) };
    // Wait for it to be IN the channel unconsumed, which is the state this is
    // about, and drop it there as the interruption path does. The count is
    // what says the signal waited for is this test's own.
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
