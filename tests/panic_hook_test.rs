//! A panic in a scan is written to that run's log, and the scan path installs
//! the hook that writes it (carrick#1470).
//!
//! One test, in a binary of its own, for the reason every real-signal test
//! here has one: a panic hook is process-global, so a test that installs one
//! reports for every other test sharing the process. What the hook SENDS is
//! proven against a stub in `src/panic_report.rs`; this is the half that can
//! only be proven by panicking.

#![cfg(unix)]

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A panic reaches this run's log file, and the scan path is what put the hook
/// there.
///
/// Both halves matter and neither covers the other: a hook nobody installs
/// writes nothing, and an install of a hook that writes nothing says nothing.
/// The second half is read from the source because a panic cannot be asked of
/// the built binary without a switch in it that exists for no other reason.
#[test]
fn a_panic_is_written_to_this_run_s_log_by_the_hook_the_scan_installs() {
    let home = tempfile::tempdir().expect("a home for the run");
    // Before the subscriber is built, and before any other thread exists: the
    // log directory is resolved from this variable when `init` runs.
    // SAFETY: one test, one process, no threads started yet.
    unsafe {
        std::env::set_var("HOME", home.path());
    }
    carrick::logging::init(false, carrick::logging::LogSink::Run);
    let log = carrick::logging::run_log_file()
        .expect("this run has a log file of its own")
        .to_path_buf();

    carrick::panic_report::install(repo_root().to_string_lossy().to_string());

    let panicked = std::panic::catch_unwind(|| panic!("a scan that could not go on"));
    assert!(panicked.is_err(), "the panic was raised");

    let written = std::fs::read_to_string(&log).expect("read this run's log");
    assert!(
        written.contains(carrick::panic_report::PANIC_REASON),
        "the run log names the panic: {written}"
    );
    assert!(
        written.contains("a scan that could not go on"),
        "the run log carries what was panicked with: {written}"
    );
    assert!(
        written.contains("panic_hook_test.rs:"),
        "the run log carries where it happened: {written}"
    );

    let main = std::fs::read_to_string(repo_root().join("src/main.rs")).expect("read src/main.rs");
    assert!(
        main.contains("panic_report::install("),
        "the scan path installs the hook, or nothing above is reached in production"
    );
}
