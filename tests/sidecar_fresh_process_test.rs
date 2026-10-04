//! A sidecar process can be replaced on request, and one that dies is
//! reported as having died (carrick#1916, carrick#1921).
//!
//! A capture builds a program of its own, so the scan asks it of a fresh
//! sidecar process rather than of the one that holds the service's program,
//! and asks once more of another if that process dies. Two things in the
//! client make that possible, and they are pinned here.
//!
//! 1. `restart` replaces the process with a fresh one scoped as the old one
//!    was, whether the old one is running or already gone. It is the restart
//!    a timed-out operation uses (`sidecar_operation_timeout_test.rs`).
//! 2. A process that ends while it is writing an answer is a process that
//!    died. The part of the answer that reached the pipe is not an answer.
//!
//! As in `sidecar_operation_timeout_test.rs`, these drive a stand-in sidecar
//! (a few lines of Node speaking the stdout protocol), and every wait runs on
//! its own thread behind a harness limit so a regression fails the test
//! instead of hanging the suite. They need `node` on PATH and nothing else.

use carrick::services::type_sidecar::{
    AnchorOrigin, CaptureAnchor, InferKind, InferRequestItem, SidecarError, SidecarResponse,
    TypeSidecar,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// How long the test itself waits before calling a wait unbounded.
const HARNESS_LIMIT: Duration = Duration::from_secs(30);

/// How long a readiness wait may take in these tests.
const READY: Duration = Duration::from_secs(20);

/// Whether these tests can run at all. A developer without node gets a skip;
/// CI does not, because a skip that reads as a pass would make this file's
/// green tick mean nothing.
fn node_available() -> bool {
    let available = std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok();
    assert!(
        available || std::env::var("CI").is_err(),
        "node is not on PATH in CI, so these tests would silently not run"
    );
    available
}

/// The stand-in sidecar. What a request does is named by its first item's
/// alias:
/// - `cut-off` writes the first 40 bytes of its answer, with no newline, and
///   exits: what the pipe holds when a process dies mid-answer;
/// - anything else answers at once, naming the alias and its own pid in
///   `errors`.
///
/// Each process appends its pid to `pids`, and each `init` appends the scope
/// it was given to `inits`.
fn stand_in_script(prelude: &str) -> String {
    format!(
        r#"
const fs = require('fs');
const path = require('path');
const readline = require('readline');
const here = __dirname;
{prelude}
const note = (file, text) => fs.appendFileSync(path.join(here, file), text + '\n');
const write = (frame) => process.stdout.write(JSON.stringify(frame) + '\n');
note('pids', String(process.pid));
readline.createInterface({{ input: process.stdin, terminal: false }}).on('line', (line) => {{
  const request = JSON.parse(line);
  const request_id = request.request_id;
  if (request.action === 'shutdown') process.exit(0);
  if (request.action === 'init') {{
    note('inits', JSON.stringify({{
      repo_root: request.repo_root,
      tsconfig_path: request.tsconfig_path ?? null,
      scan_root: request.scan_root ?? null,
    }}));
    return write({{ request_id, status: 'ready' }});
  }}
  const what = (request.requests ?? request.anchors)[0].alias;
  const answer = {{
    request_id,
    status: request.action === 'capture_v2' ? 'error' : 'success',
    errors: [what, String(process.pid)],
  }};
  if (what === 'cut-off') {{
    fs.writeSync(1, JSON.stringify(answer).slice(0, 40));
    process.exit(134);
  }}
  write(answer);
}});
"#
    )
}

/// A stand-in sidecar on disk, scoped to its own directory. The directory is
/// returned so the caller keeps it alive.
fn ready_stand_in(prelude: &str) -> (tempfile::TempDir, PathBuf, TypeSidecar) {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = dir.path().join("stand-in-sidecar.cjs");
    std::fs::write(&script, stand_in_script(prelude)).expect("write stand-in sidecar");
    let sidecar = TypeSidecar::spawn(&script).expect("spawn stand-in sidecar");
    sidecar.set_scan_root(dir.path());
    sidecar.start_init(dir.path(), Some("tsconfig.app.json"));
    let (sidecar, ready, _) = off_thread(sidecar, |sidecar| sidecar.wait_ready(READY));
    ready.expect("the stand-in answers init");
    (dir, script, sidecar)
}

/// Run one sidecar call on its own thread under the harness limit, so an
/// unbounded wait fails rather than hangs. Returns the sidecar, the call's
/// result and how long it took.
fn off_thread<T: Send + 'static>(
    sidecar: TypeSidecar,
    call: impl FnOnce(&TypeSidecar) -> T + Send + 'static,
) -> (TypeSidecar, T, Duration) {
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let started = Instant::now();
        let result = call(&sidecar);
        // Send first so the test fails on the limit rather than on the join.
        let _ = tx.send((result, started.elapsed()));
        sidecar
    });
    let (result, elapsed) = rx
        .recv_timeout(HARNESS_LIMIT)
        .expect("the sidecar call did not return within the harness limit");
    let sidecar = handle.join().expect("call thread");
    (sidecar, result, elapsed)
}

/// One inference request whose behaviour the stand-in reads from `alias`.
fn infer(
    sidecar: TypeSidecar,
    alias: &'static str,
) -> (TypeSidecar, Result<SidecarResponse, SidecarError>) {
    let (sidecar, result, _) = off_thread(sidecar, move |sidecar| {
        sidecar.infer_types(
            &[InferRequestItem {
                file_path: "src/a.ts".to_string(),
                line_number: 1,
                span_start: None,
                span_end: None,
                expression_text: None,
                expression_line: None,
                infer_kind: InferKind::Expression,
                alias: Some(alias.to_string()),
                param_name: None,
            }],
            None,
        )
    });
    (sidecar, result)
}

/// One capture request whose behaviour the stand-in reads from `alias`. The
/// stand-in never answers a capture with a result, so this is always an
/// error: its own `CaptureFailed`, or what the client made of a death.
fn capture(sidecar: TypeSidecar, alias: &'static str) -> (TypeSidecar, SidecarError) {
    let (sidecar, result, _) = off_thread(sidecar, move |sidecar| {
        sidecar.capture_v2(
            "/repo",
            "service",
            &[CaptureAnchor::Literal {
                alias: alias.to_string(),
                type_text: "string".to_string(),
                anchor_origin: AnchorOrigin::DeterministicInfer,
                source_file: None,
                printed_names: Vec::new(),
                raw_text_read: false,
            }],
            "/out",
            None,
        )
    });
    let error = result.expect_err("the stand-in never answers a capture with a result");
    (sidecar, error)
}

fn restart(sidecar: TypeSidecar) -> (TypeSidecar, Result<(), SidecarError>, Duration) {
    off_thread(sidecar, |sidecar| sidecar.restart("a test asked for one"))
}

/// The lines a stand-in process noted in `file`, oldest first.
fn notes(dir: &Path, file: &str) -> Vec<String> {
    std::fs::read_to_string(dir.join(file))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The pid of the process that wrote an answer.
fn answered_by(response: &SidecarResponse) -> String {
    response.errors.as_ref().expect("errors")[1].clone()
}

/// The defect in the ticket's log. A sidecar that ran out of heap while
/// writing its capture answer left part of the answer in the pipe, and the
/// scan recorded `Deserialization error: EOF while parsing a string`. The
/// process dying is what happened, and it is what the caller has to be told:
/// it is the difference between a capture worth asking again and one that
/// answered nonsense.
#[test]
fn a_capture_answer_cut_off_by_the_process_dying_reads_as_the_death() {
    if !node_available() {
        eprintln!("Skipping: node is not on PATH");
        return;
    }
    let (_dir, _script, sidecar) = ready_stand_in("");

    let (_sidecar, error) = capture(sidecar, "cut-off");
    assert!(
        matches!(error, SidecarError::ProcessDied),
        "expected the death to be reported, got {error:?}"
    );
    assert!(error.is_process_death());
}

/// The same through the other reader, the one an inference waits in.
#[test]
fn an_inference_answer_cut_off_by_the_process_dying_reads_as_the_death() {
    if !node_available() {
        eprintln!("Skipping: node is not on PATH");
        return;
    }
    let (_dir, _script, sidecar) = ready_stand_in("");

    let (_sidecar, result) = infer(sidecar, "cut-off");
    assert!(
        matches!(result, Err(SidecarError::ProcessDied)),
        "expected the death to be reported, got {result:?}"
    );
}

/// A restart on request: the old process is gone, the next request is
/// answered by a new one, and the new one was initialised to the root,
/// tsconfig and scan root the old one was scoped to.
#[test]
fn a_restart_gives_a_fresh_process_scoped_as_the_old_one_was() {
    if !node_available() {
        eprintln!("Skipping: node is not on PATH");
        return;
    }
    let (dir, _script, sidecar) = ready_stand_in("");

    let (sidecar, first) = infer(sidecar, "before");
    let first = answered_by(&first.expect("the stand-in answers"));

    let (sidecar, restarted, _) = restart(sidecar);
    restarted.expect("the restart");
    assert!(
        sidecar.is_ready(),
        "the sidecar was not ready after its restart: {:?}",
        sidecar.get_state()
    );
    assert!(sidecar.is_scoped_to(dir.path(), Some("tsconfig.app.json")));

    let (_sidecar, second) = infer(sidecar, "after");
    let second = answered_by(&second.expect("the fresh process answers"));
    assert_ne!(first, second, "the same process answered after the restart");
    assert_eq!(notes(dir.path(), "pids"), [first.clone(), second]);
    assert!(
        !process_is_running(&first),
        "the process that was replaced is still running"
    );

    let inits = notes(dir.path(), "inits");
    assert_eq!(inits.len(), 2, "expected one init per process: {inits:?}");
    assert_eq!(
        inits[0], inits[1],
        "the fresh process was scoped differently"
    );
    let scope: serde_json::Value = serde_json::from_str(&inits[1]).expect("init note");
    assert_eq!(scope["repo_root"], dir.path().to_string_lossy().as_ref());
    assert_eq!(scope["tsconfig_path"], "tsconfig.app.json");
    assert_eq!(scope["scan_root"], dir.path().to_string_lossy().as_ref());
}

/// Whether a process with this pid exists. `kill -0` asks without signalling.
fn process_is_running(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// A process that has died is replaced the same way. Without the restart the
/// next request is written to a closed pipe, and every request after it.
#[test]
fn a_restart_replaces_a_process_that_has_died() {
    if !node_available() {
        eprintln!("Skipping: node is not on PATH");
        return;
    }
    let (dir, _script, sidecar) = ready_stand_in("");

    let (sidecar, error) = capture(sidecar, "cut-off");
    assert!(matches!(error, SidecarError::ProcessDied), "{error:?}");

    let (sidecar, restarted, _) = restart(sidecar);
    restarted.expect("a dead process can be replaced");

    let (_sidecar, answer) = infer(sidecar, "after");
    let answer = answer.expect("the request after the restart was not answered");
    assert_eq!(notes(dir.path(), "pids").len(), 2);
    assert_eq!(answered_by(&answer), notes(dir.path(), "pids")[1]);
}

/// When no fresh process can be started the restart says so, and so does
/// every request after it, at once.
#[test]
fn a_restart_that_cannot_start_a_process_says_so() {
    if !node_available() {
        eprintln!("Skipping: node is not on PATH");
        return;
    }
    // The script deletes itself once it is running, so the restart finds
    // nothing to run.
    let (_dir, script, sidecar) = ready_stand_in("fs.unlinkSync(__filename);");
    assert!(!script.exists(), "the stand-in did not delete its script");

    let (sidecar, restarted, _) = restart(sidecar);
    assert!(
        matches!(&restarted, Err(SidecarError::NotReady(reason)) if reason.contains("a test asked for one")),
        "expected the restart to fail and say what it was for, got {restarted:?}"
    );

    let (_sidecar, result, took) = off_thread(sidecar, |sidecar| {
        sidecar.resolve_definitions("/stub", &["A".to_string()])
    });
    assert!(
        matches!(result, Err(SidecarError::NotReady(_))),
        "expected a sidecar that could not restart to refuse the next request, got {result:?}"
    );
    assert!(
        took < Duration::from_millis(500),
        "the refusal waited on a process that is gone: {took:?}"
    );
}
