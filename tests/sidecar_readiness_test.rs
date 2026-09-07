//! What `wait_ready` promises when the sidecar does not answer (carrick#748).
//!
//! The budget is a wall-clock promise: a scan that waits three minutes for a
//! type layer has to be able to say so, and the caller's number has to be the
//! number. Before this, the readiness read blocked in `read_line` on a pipe
//! that never delivered a line, so the elapsed check around it only ran once
//! one did — a stated 30 s budget was observed spending 57 s and then
//! reporting a timeout anyway.
//!
//! These tests drive a stand-in sidecar: a few lines of Node that speak the
//! same stdout protocol badly on purpose. They need `node` on PATH (CI has it
//! for the real sidecar's build) and nothing else — no `src/sidecar/dist`, no
//! TypeScript program, no network.
//!
//! Every wait runs on its own thread behind a `recv_timeout`, so a regression
//! that reintroduces an unbounded read fails the test instead of hanging the
//! suite.

use carrick::services::type_sidecar::{SidecarError, TypeSidecar};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// The budget these tests ask for. Small enough to run fast, large enough that
/// process spawn is noise against it.
const BUDGET: Duration = Duration::from_secs(2);

/// How long the test itself waits before calling a wait unbounded. Comfortably
/// past the budget and past any plausible doubling of it.
const HARNESS_LIMIT: Duration = Duration::from_secs(20);

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok()
}

/// Write a stand-in sidecar script and return its path. The directory is
/// returned alongside so the caller keeps it alive for the process's lifetime.
fn stand_in(body: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = dir.path().join("stand-in-sidecar.js");
    std::fs::write(&script, body).expect("write stand-in sidecar");
    (dir, script)
}

/// Spawn the stand-in, start an init, and wait for readiness on another
/// thread. Returns the sidecar (so the test can keep using it) with the wait's
/// result and how long it took.
fn wait_ready_off_thread(
    script: &Path,
    repo: &Path,
    budget: Duration,
) -> (TypeSidecar, Result<(), SidecarError>, Duration) {
    let sidecar = TypeSidecar::spawn(script).expect("spawn stand-in sidecar");
    sidecar.start_init(repo, None);
    time_wait(sidecar, budget)
}

/// Run one `wait_ready` on its own thread under a harness deadline, so an
/// unbounded wait fails rather than hangs.
fn time_wait(
    sidecar: TypeSidecar,
    budget: Duration,
) -> (TypeSidecar, Result<(), SidecarError>, Duration) {
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let started = Instant::now();
        let result = sidecar.wait_ready(budget);
        let elapsed = started.elapsed();
        // Send first so the test fails on a deadline rather than blocking on
        // the join; the sidecar comes back through the join handle.
        let _ = tx.send((result, elapsed));
        sidecar
    });

    let (result, elapsed) = rx.recv_timeout(HARNESS_LIMIT).expect(
        "wait_ready did not return within the harness deadline: the readiness wait is \
         unbounded again (carrick#748)",
    );
    let sidecar = handle.join().expect("wait thread");
    (sidecar, result, elapsed)
}

/// A sidecar that never answers must cost the stated budget, once.
///
/// The upper bound is the assertion that matters: the old shape spent the
/// budget in the init read and then polled for the budget again, so this
/// would land at roughly twice `BUDGET` — when it returned at all.
#[test]
fn a_silent_sidecar_times_out_at_the_stated_budget() {
    if !node_available() {
        eprintln!("Skipping: node is not on PATH");
        return;
    }

    // Reads stdin so the pipe stays open, writes nothing, ever.
    let (dir, script) = stand_in("process.stdin.resume();\nsetInterval(() => {}, 1000);\n");
    let (_sidecar, result, elapsed) = wait_ready_off_thread(&script, dir.path(), BUDGET);

    assert!(
        matches!(result, Err(SidecarError::Timeout)),
        "expected a timeout, got {result:?}"
    );
    assert!(
        elapsed >= BUDGET,
        "returned before the budget was spent: {elapsed:?}"
    );
    assert!(
        elapsed < BUDGET * 2,
        "the wait cost about twice the stated budget ({elapsed:?} against {BUDGET:?}): the \
         init read and the poll loop are each spending the whole budget again (carrick#748)"
    );
}

/// A line that is not an answer must draw the budget down, not reset it.
///
/// This is the 0.3.42 shape exactly: something reached stdout, the read loop
/// skipped it, and the elapsed check that only runs between reads then
/// reported a timeout long after the budget was gone.
#[test]
fn a_blank_line_does_not_extend_the_budget() {
    if !node_available() {
        eprintln!("Skipping: node is not on PATH");
        return;
    }

    let (dir, script) = stand_in(
        "process.stdin.resume();\n\
         setTimeout(() => process.stdout.write('\\n'), 500);\n\
         setInterval(() => {}, 1000);\n",
    );
    let (_sidecar, result, elapsed) = wait_ready_off_thread(&script, dir.path(), BUDGET);

    assert!(
        matches!(result, Err(SidecarError::Timeout)),
        "expected a timeout, got {result:?}"
    );
    assert!(
        elapsed < BUDGET * 2,
        "a skipped line restarted the clock: {elapsed:?} against a {BUDGET:?} budget"
    );
}

/// A sidecar that misses its budget is killed, and its late answer is never
/// read as the answer to the next question.
///
/// Without the kill, the `ready` frame this stand-in sends after the deadline
/// sits in the channel until the next service's re-init reads it — and that
/// service would then resolve types against a program built for a different
/// scan root.
#[test]
fn a_sidecar_that_misses_its_budget_is_killed() {
    if !node_available() {
        eprintln!("Skipping: node is not on PATH");
        return;
    }

    let (dir, script) = stand_in(
        "process.stdin.resume();\n\
         setTimeout(() => {\n\
         process.stdout.write(JSON.stringify({ request_id: 'init', status: 'ready' }) + '\\n');\n\
         }, 6000);\n\
         setInterval(() => {}, 1000);\n",
    );
    let (sidecar, result, _) = wait_ready_off_thread(&script, dir.path(), BUDGET);
    assert!(
        matches!(result, Err(SidecarError::Timeout)),
        "expected a timeout, got {result:?}"
    );

    // The second service's re-init. The late `ready` frame must not be
    // waiting for it, and it must not wait a second budget to find that out.
    sidecar.start_init(dir.path(), None);
    let (_sidecar, second, elapsed) = time_wait(sidecar, BUDGET);
    assert!(
        second.is_err(),
        "a killed sidecar reported itself ready: {second:?}"
    );
    assert!(
        elapsed < BUDGET,
        "the second wait spent the whole budget on a sidecar already known dead: {elapsed:?}"
    );
}

/// A sidecar that answers is not made to wait for its budget.
#[test]
fn a_prompt_sidecar_returns_immediately() {
    if !node_available() {
        eprintln!("Skipping: node is not on PATH");
        return;
    }

    let (dir, script) = stand_in(
        "process.stdin.on('data', () => {\n\
         process.stdout.write(JSON.stringify({ request_id: 'init', status: 'ready' }) + '\\n');\n\
         });\n\
         setInterval(() => {}, 1000);\n",
    );
    let (_sidecar, result, elapsed) = wait_ready_off_thread(&script, dir.path(), BUDGET);

    assert!(result.is_ok(), "expected ready, got {result:?}");
    assert!(
        elapsed < BUDGET,
        "a ready sidecar cost the whole budget: {elapsed:?}"
    );
}
