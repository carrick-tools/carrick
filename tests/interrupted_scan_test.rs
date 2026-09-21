//! What a run does when it is stopped, driven at the built binary with real
//! signals and real streams (carrick#1386, carrick#1387).
//!
//! Both defects were found this way and neither can be reproduced any other
//! way: a mocked scan inside a test process has no terminal to lose and no
//! pass whose thread a signal has to reach. Each case here fails the way the
//! field report did — a run that ignores the signal and finishes, a run that
//! panics on a print — rather than by an assertion on an intermediate.

#![cfg(unix)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Far past what any of these runs needs, and short enough that a run which
/// ignores its signal fails the test rather than holding CI.
const DEADLINE: Duration = Duration::from_secs(120);

/// The exit code a run stopped by SIGTERM reports: 128 plus the signal.
const TERMINATED: i32 = 143;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A mocked scan of `target`, with a home of its own so its log file and
/// credential lookups cannot touch the developer's.
fn scan(target: &Path, home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_carrick"));
    command
        .arg(target)
        .arg("--allow-unprepared")
        .env("HOME", home)
        .env("CARRICK_MOCK_ALL", "1")
        // A fixture tree has no node_modules, so every endpoint it indexes
        // would be typeless; this run is about the signal, not the types.
        .env("CARRICK_ALLOW_MISSING_TYPES", "1")
        .env_remove("CARRICK_RUN_ID")
        .env_remove("CARRICK_RUN_PHASE")
        .env_remove("GITHUB_REPOSITORY")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("CI")
        .stdin(Stdio::null());
    command
}

fn send(child: &Child, signal: libc::c_int) {
    // SAFETY: a signal to a child this test spawned and has not reaped.
    let sent = unsafe { libc::kill(child.id() as libc::pid_t, signal) };
    assert_eq!(sent, 0, "could not signal the scan");
}

/// The run's exit code, or a failure naming how long it went on for.
fn code_within(child: &mut Child, deadline: Duration, what: &str) -> i32 {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("poll the scan") {
            return status.code().unwrap_or_else(|| {
                panic!("the scan was killed by a signal rather than ending itself")
            });
        }
        if start.elapsed() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{what} after {:.0}s", start.elapsed().as_secs_f64());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A signal during a CPU-bound pass ends the run, rather than being noticed
/// when the pass that was running happens to finish (carrick#1387).
///
/// The scan is of this repo's whole fixture tree, which takes several seconds
/// of parsing and analysis with nothing in it that awaits; the signal lands
/// while that is going on. Before the watcher, every such signal was acted on
/// only at the end: SIGTERM at one second of a nine-second scan ran to
/// completion and exited 0.
#[test]
fn a_signal_during_an_analysis_pass_ends_the_run() {
    let home = tempfile::tempdir().expect("a home for the run");
    let mut child = scan(&repo_root().join("tests/fixtures"), home.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start a scan");

    // Into the analysis, and nowhere near the end of it.
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        child.try_wait().expect("poll the scan").is_none(),
        "the scan was over before it could be signalled"
    );
    send(&child, libc::SIGTERM);

    let code = code_within(&mut child, DEADLINE, "the signalled scan was still running");
    assert_eq!(
        code, TERMINATED,
        "a signalled run ends with the signal's code, not with the run's"
    );
}

/// The same, for the wait on the type sidecar: a blocking wait, and where a
/// scan of a large repo spends its minutes (carrick#1387).
///
/// The sidecar here answers nothing at all, so the wait is the readiness
/// budget in full — five minutes, set higher than the deadline on purpose. A
/// run that can only notice the signal when the wait ends fails this by
/// outliving it.
#[test]
fn a_signal_during_the_wait_on_the_sidecar_ends_the_run() {
    let home = tempfile::tempdir().expect("a home for the run");
    let sidecar = home.path().join("sidecar/dist/src");
    std::fs::create_dir_all(&sidecar).expect("a directory for the sidecar");
    // Node is already required to run the real one, and this is the whole of
    // a sidecar that never becomes ready: it holds its streams open and
    // answers nothing.
    std::fs::write(
        sidecar.join("index.js"),
        "process.stdin.resume();\nsetInterval(() => {}, 60000);\n",
    )
    .expect("write the sidecar that never answers");

    let mut child = scan(&repo_root().join("examples/express-single"), home.path())
        .env("CARRICK_SIDECAR_DIR", home.path().join("sidecar"))
        .env("CARRICK_SIDECAR_READY_TIMEOUT_SECS", "600")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start a scan");

    std::thread::sleep(Duration::from_secs(2));
    assert!(
        child.try_wait().expect("poll the scan").is_none(),
        "the scan was over before it could be signalled"
    );
    send(&child, libc::SIGTERM);

    let code = code_within(
        &mut child,
        DEADLINE,
        "the scan served the whole readiness budget after being signalled",
    );
    assert_eq!(code, TERMINATED);
}

/// A run whose terminal goes away finishes instead of panicking on its next
/// print (carrick#1386).
///
/// The read end of the child's stdout is closed while the scan runs, which is
/// what a parent that held the pipes and died leaves behind — an agent harness
/// stopping a run, the case this was found in. Every write after that answers
/// `EPIPE`, and `println!` answers an error by panicking: on the main thread,
/// inside the analysis future, exit 101 before anything could be said to the
/// cloud.
#[test]
fn a_run_whose_output_nobody_reads_still_finishes() {
    let home = tempfile::tempdir().expect("a home for the run");
    let mut child = scan(&repo_root().join("examples/express-multi"), home.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start a scan");

    // The terminal goes away. Dropped now rather than after a wait, because
    // nothing is written to stdout until the run has something to report:
    // every write this run makes to it is a write to a stream that is already
    // gone, which is the case being proven and is not a race.
    drop(child.stdout.take().expect("the scan's stdout"));
    // Its stderr is held to the end, for the panic message a failing run
    // leaves there.
    let mut stderr = child.stderr.take().expect("the scan's stderr");
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let said = reader.join().expect("read what the run said");

    let code = code_within(&mut child, DEADLINE, "the scan never ended");
    assert_eq!(
        code, 0,
        "a run whose output nobody can read still finishes: it said {said:?}"
    );
    assert!(
        !said.contains("failed printing to"),
        "the run panicked on a print: {said}"
    );
}
