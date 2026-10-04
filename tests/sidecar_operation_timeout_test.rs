//! What an operation's deadline means once it passes, and what it measures
//! (carrick#1914).
//!
//! The sidecar answers one request at a time, and a handler that runs the
//! compiler blocks its event loop until it returns. So a request sent while
//! another is running waits in the pipe. A scan that stopped waiting for one
//! answer and then asked the next question got no answer to that either: the
//! process was still working on the first, the second request's clock started
//! when it was sent, and it ran out while the request was still queued.
//!
//! Two promises are pinned here.
//!
//! 1. An operation that misses its deadline costs that operation and nothing
//!    after it: the process is killed, a fresh one is started and scoped as
//!    the old one was, and the next request runs there inside its own window.
//! 2. The deadline measures a stall, not the size of the job: a `progress`
//!    frame restarts the clock, on both readers, and a handler that blocks its
//!    event loop can still get one out.
//!
//! As in `sidecar_readiness_test.rs`, these drive a stand-in sidecar (a few
//! lines of Node speaking the stdout protocol) under a deadline of seconds, and
//! every wait runs on its own thread behind a harness limit so a regression
//! fails the test instead of hanging the suite. They need `node` on PATH and
//! nothing else.

use carrick::services::type_sidecar::{
    AnchorOrigin, CaptureAnchor, InferKind, InferRequestItem, SidecarError, SidecarResponse,
    TypeSidecar,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// The operation deadline these tests ask for, in place of fifteen minutes.
const DEADLINE: Duration = Duration::from_millis(1500);

/// How long the stand-in's slow request blocks its event loop: well past the
/// deadline, so a request queued behind it cannot be answered in time.
const SLOW_MS: u64 = 6000;

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
        "node is not on PATH in CI, so these timing tests would silently not run"
    );
    available
}

/// The stand-in sidecar. It handles one line at a time and blocks its event
/// loop while it works, as the real one does while the compiler runs.
///
/// What a request does is named by its first item's alias:
/// - `slow` blocks for `SLOW_MS`, notes that it finished, then answers;
/// - `steady` writes a `progress` frame and blocks for 500 ms, six times
///   over (three seconds in all), then answers;
/// - `stalls` writes two `progress` frames a second apart, then blocks for
///   `SLOW_MS`;
/// - anything else answers at once.
///
/// Every answer names the alias it answered in `errors`, so a test can tell
/// its own answer from a late one. Each process appends its pid to `pids`,
/// and each `init` appends the scope it was given to `inits`.
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
// Blocks the event loop: no timer fires and no queued write is flushed
// until it returns.
const block = (ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
note('pids', String(process.pid));
readline.createInterface({{ input: process.stdin, terminal: false }}).on('line', (line) => {{
  const request = JSON.parse(line);
  const request_id = request.request_id;
  const progress = (message) => write({{ request_id, status: 'progress', phase: 'work', message }});
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
  if (what === 'slow') {{
    block({SLOW_MS});
    note('finished', what);
  }}
  if (what === 'steady') {{
    for (let i = 0; i < 6; i++) {{
      progress(`${{i}} of 6`);
      block(500);
    }}
  }}
  if (what === 'stalls') {{
    progress('0 of 2');
    block(1000);
    progress('1 of 2');
    block({SLOW_MS});
  }}
  write({{ request_id, status: request.action === 'capture_v2' ? 'error' : 'success', errors: [what] }});
}});
"#
    )
}

/// A stand-in sidecar on disk, spawned with the test deadline and scoped to
/// its own directory. The directory is returned so the caller keeps it alive.
fn ready_stand_in(prelude: &str) -> (tempfile::TempDir, PathBuf, TypeSidecar) {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = dir.path().join("stand-in-sidecar.cjs");
    std::fs::write(&script, stand_in_script(prelude)).expect("write stand-in sidecar");
    let sidecar = TypeSidecar::spawn(&script)
        .expect("spawn stand-in sidecar")
        .with_operation_timeout(DEADLINE);
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
) -> (TypeSidecar, Result<SidecarResponse, SidecarError>, Duration) {
    off_thread(sidecar, move |sidecar| {
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
    })
}

/// One capture request whose behaviour the stand-in reads from `alias`. The
/// stand-in answers a capture with an error frame naming the alias, so the
/// answer comes back as `CaptureFailed(alias)`.
fn capture(sidecar: TypeSidecar, alias: &'static str) -> (TypeSidecar, SidecarError, Duration) {
    let (sidecar, result, elapsed) = off_thread(sidecar, move |sidecar| {
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
    (sidecar, error, elapsed)
}

/// The lines a stand-in process noted in `file`, oldest first.
fn notes(dir: &Path, file: &str) -> Vec<String> {
    std::fs::read_to_string(dir.join(file))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The alias an answer says it answered.
fn answered(response: &SidecarResponse) -> Option<&str> {
    response.errors.as_ref()?.first().map(String::as_str)
}

/// The defect itself. The first request outlives its deadline; the request
/// after it must still be answered, by its own answer, inside its own window.
///
/// Before the fix the second request sat in the pipe behind the first, which
/// the process was still running, and its deadline passed while it waited.
#[test]
fn a_request_after_a_timed_out_one_runs_in_its_own_window() {
    if !node_available() {
        eprintln!("Skipping: node is not on PATH");
        return;
    }
    let (dir, _script, sidecar) = ready_stand_in("");

    let (sidecar, first, first_took) = infer(sidecar, "slow");
    assert!(
        matches!(first, Err(SidecarError::Timeout)),
        "expected the slow request to time out, got {first:?}"
    );
    assert!(
        first_took >= DEADLINE,
        "returned before the deadline was spent: {first_took:?}"
    );

    let (_sidecar, second, second_took) = infer(sidecar, "fast");
    let second = second.expect(
        "the request after a timed-out one was not answered: it queued behind work \
         nobody was waiting for (carrick#1914)",
    );
    assert_eq!(second.status, "success");
    assert_eq!(
        answered(&second),
        Some("fast"),
        "the answer read for the second request was not its own"
    );
    assert!(
        second_took < DEADLINE,
        "the second request did not run inside its own window: {second_took:?}"
    );
    assert_eq!(
        notes(dir.path(), "pids").len(),
        2,
        "the second request was not answered by a fresh process"
    );
}

/// The work nobody is waiting for stops. The process that missed the deadline
/// is killed, so it never reaches the end of the request it was running.
///
/// Before the fix it ran on to the end, a compiler-sized process working for
/// the rest of the scan, and the scan log showed it starting the next request
/// long after the scanner had recorded that request as timed out.
#[test]
fn the_work_nobody_waits_for_is_stopped() {
    if !node_available() {
        eprintln!("Skipping: node is not on PATH");
        return;
    }
    let (dir, _script, sidecar) = ready_stand_in("");
    let started = Instant::now();

    let (sidecar, first, _) = infer(sidecar, "slow");
    assert!(
        matches!(first, Err(SidecarError::Timeout)),
        "expected the slow request to time out, got {first:?}"
    );

    // Wait past the moment the slow request would have finished, with the
    // sidecar still held (dropping it kills whatever process it has).
    let would_finish = Duration::from_millis(SLOW_MS + 1500);
    thread::sleep(would_finish.saturating_sub(started.elapsed()));
    assert!(
        notes(dir.path(), "finished").is_empty(),
        "the timed-out request ran to completion after the scanner stopped waiting for it"
    );
    drop(sidecar);
}

/// The fresh process is the old one's replacement, not a blank one: it is
/// initialised to the root, tsconfig and scan root the old one was scoped to,
/// so the next request reads the same service's program.
#[test]
fn the_fresh_sidecar_is_scoped_as_the_old_one_was() {
    if !node_available() {
        eprintln!("Skipping: node is not on PATH");
        return;
    }
    let (dir, _script, sidecar) = ready_stand_in("");

    let (sidecar, first, _) = infer(sidecar, "slow");
    assert!(
        matches!(first, Err(SidecarError::Timeout)),
        "expected the slow request to time out, got {first:?}"
    );
    assert!(
        sidecar.is_ready(),
        "the sidecar was not ready again after its restart: {:?}",
        sidecar.get_state()
    );
    assert!(sidecar.is_scoped_to(dir.path(), Some("tsconfig.app.json")));

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

/// Both readers. A capture waits in the reader that already skipped progress
/// frames; its deadline passing must restart the sidecar all the same.
#[test]
fn a_timed_out_capture_restarts_the_sidecar_too() {
    if !node_available() {
        eprintln!("Skipping: node is not on PATH");
        return;
    }
    let (dir, _script, sidecar) = ready_stand_in("");

    let (sidecar, first, _) = capture(sidecar, "slow");
    assert!(
        matches!(first, SidecarError::Timeout),
        "expected the slow capture to time out, got {first:?}"
    );

    let (_sidecar, second, second_took) = infer(sidecar, "fast");
    let second = second.expect("the request after a timed-out capture was not answered");
    assert_eq!(answered(&second), Some("fast"));
    assert!(
        second_took < DEADLINE,
        "the request after a timed-out capture did not run inside its own window: {second_took:?}"
    );
    assert_eq!(notes(dir.path(), "pids").len(), 2);
}

/// When the sidecar cannot be started again, later requests fail at once and
/// say why, rather than each waiting out a deadline on a process that is gone.
#[test]
fn a_sidecar_that_cannot_be_restarted_fails_fast() {
    if !node_available() {
        eprintln!("Skipping: node is not on PATH");
        return;
    }
    // The script deletes itself once it is running, so the restart finds
    // nothing to run.
    let (_dir, script, sidecar) = ready_stand_in("fs.unlinkSync(__filename);");
    assert!(!script.exists(), "the stand-in did not delete its script");

    let (sidecar, first, _) = infer(sidecar, "slow");
    assert!(
        matches!(first, Err(SidecarError::Timeout)),
        "expected the slow request to time out, got {first:?}"
    );

    let (_sidecar, second, second_took) = infer(sidecar, "fast");
    assert!(
        matches!(second, Err(SidecarError::NotReady(_))),
        "expected a sidecar that could not restart to refuse the next request, got {second:?}"
    );
    assert!(
        second_took < Duration::from_millis(500),
        "the refusal waited on a process that is gone: {second_took:?}"
    );
}

/// A job longer than the deadline finishes when it keeps saying it is alive.
///
/// The stand-in works for three seconds against a deadline of one and a half,
/// writing a `progress` frame every half second from a handler that blocks its
/// event loop in between. So this pins two things at once: the reader that
/// `infer` waits in restarts its clock on a progress frame, and a frame
/// written by a blocked handler reaches the pipe while the handler is still
/// running. The second holds because this side drains the pipe on its own
/// thread, so a frame-sized write never finds it full and never has to wait
/// for the event loop.
///
/// Before the fix that reader kept one deadline for the whole call, and took
/// the first progress frame for the answer.
#[test]
fn progress_frames_keep_a_long_inference_alive() {
    if !node_available() {
        eprintln!("Skipping: node is not on PATH");
        return;
    }
    let (dir, _script, sidecar) = ready_stand_in("");

    let (_sidecar, result, took) = infer(sidecar, "steady");
    let response = result.expect(
        "an inference that reported progress throughout was cut off by a deadline that \
         measured the whole job (carrick#1914)",
    );
    assert_eq!(
        response.status, "success",
        "a progress frame was read as the answer"
    );
    assert_eq!(answered(&response), Some("steady"));
    assert!(
        took > DEADLINE,
        "the job was meant to outlast the deadline: {took:?}"
    );
    assert_eq!(
        notes(dir.path(), "pids").len(),
        1,
        "a sidecar that was making progress was restarted"
    );
}

/// The same job through the other reader, the one a capture waits in.
#[test]
fn progress_frames_keep_a_long_capture_alive() {
    if !node_available() {
        eprintln!("Skipping: node is not on PATH");
        return;
    }
    let (dir, _script, sidecar) = ready_stand_in("");

    let (_sidecar, error, took) = capture(sidecar, "steady");
    assert!(
        matches!(&error, SidecarError::CaptureFailed(detail) if detail == "steady"),
        "expected the capture's own answer, got {error:?}"
    );
    assert!(
        took > DEADLINE,
        "the job was meant to outlast the deadline: {took:?}"
    );
    assert_eq!(notes(dir.path(), "pids").len(), 1);
}

/// Progress frames move the deadline; they do not remove it. A sidecar that
/// reported progress and then went quiet is cut off one deadline after its
/// last frame, and restarted like any other that missed it.
#[test]
fn a_sidecar_that_stalls_after_progress_still_times_out() {
    if !node_available() {
        eprintln!("Skipping: node is not on PATH");
        return;
    }
    let (dir, _script, sidecar) = ready_stand_in("");

    let (sidecar, result, took) = infer(sidecar, "stalls");
    assert!(
        matches!(result, Err(SidecarError::Timeout)),
        "expected the stalled request to time out, got {result:?}"
    );
    // The last frame is written one second in, so the deadline falls due one
    // second later than it would have with no frames at all.
    let last_frame = Duration::from_millis(1000);
    assert!(
        took >= last_frame + DEADLINE - Duration::from_millis(100),
        "the wait was not measured from the last progress frame: {took:?}"
    );

    let (_sidecar, second, _) = infer(sidecar, "fast");
    let second = second.expect("the request after a stalled one was not answered");
    assert_eq!(answered(&second), Some("fast"));
    assert_eq!(notes(dir.path(), "pids").len(), 2);
}
