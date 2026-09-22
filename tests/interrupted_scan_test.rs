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

/// And for the wait on a sidecar FRAME, which is where a ready sidecar keeps
/// the scan: `capture_v2` and `check_v2` read their answer there, for up to
/// fifteen minutes per frame, on the thread the analysis is on (carrick#1387).
///
/// The sidecar here becomes ready, answers everything the run asks before the
/// capture, and then goes silent on the capture itself. A run that can only
/// notice the signal when that frame arrives fails this by outliving the
/// deadline, which is an eighth of the budget it would be serving.
#[test]
fn a_signal_during_the_wait_on_a_sidecar_frame_ends_the_run() {
    let home = tempfile::tempdir().expect("a home for the run");
    let sidecar = home.path().join("sidecar/dist/src");
    std::fs::create_dir_all(&sidecar).expect("a directory for the sidecar");
    // Marker rather than a sleep: the run has to have ASKED for the capture
    // before the signal means anything, and how long it takes to get there is
    // the machine's business.
    let asked = home.path().join("capture-asked");
    std::fs::write(
        sidecar.join("index.js"),
        format!(
            r#"const fs = require("fs");
let buffered = "";
process.stdin.on("data", (chunk) => {{
  buffered += chunk;
  let end;
  while ((end = buffered.indexOf("\n")) >= 0) {{
    const line = buffered.slice(0, end);
    buffered = buffered.slice(end + 1);
    if (!line.trim()) continue;
    let request;
    try {{ request = JSON.parse(line); }} catch (e) {{ continue; }}
    const answer = (fields) =>
      process.stdout.write(
        JSON.stringify(Object.assign({{ request_id: request.request_id }}, fields)) + "\n"
      );
    switch (request.action) {{
      case "init":
        answer({{ status: "ready", init_time_ms: 1 }});
        break;
      case "bundle":
        answer({{ status: "success", dts_content: "", manifest: [], symbol_failures: [] }});
        break;
      case "infer":
        answer({{ status: "success", inferred_types: [] }});
        break;
      case "resolve_definitions":
        answer({{ status: "success", definitions: [] }});
        break;
      case "capture_v2":
        // Asked, and never answered: this is the wait under test.
        fs.writeFileSync({asked:?}, "asked");
        break;
      default:
        answer({{ status: "success" }});
    }}
  }}
}});
process.stdin.resume();
setInterval(() => {{}}, 60000);
"#,
            asked = asked.to_string_lossy()
        ),
    )
    .expect("write the sidecar that answers everything but the capture");

    let mut child = scan(&repo_root().join("examples/express-single"), home.path())
        .env("CARRICK_SIDECAR_DIR", home.path().join("sidecar"))
        .env("CARRICK_SIDECAR_READY_TIMEOUT_SECS", "600")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start a scan");

    let start = Instant::now();
    while !asked.exists() {
        if start.elapsed() > DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the scan never asked the sidecar for a capture");
        }
        assert!(
            child.try_wait().expect("poll the scan").is_none(),
            "the scan was over before it could be signalled"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    send(&child, libc::SIGTERM);

    let code = code_within(
        &mut child,
        DEADLINE,
        "the scan was still waiting on the capture frame",
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

/// A SIGTERM to a BUILD reaches the scan it is waiting on (carrick#1379).
///
/// A terminal sends Ctrl-C to the whole foreground process group, so both
/// processes hear it and the two agree. `kill <build pid>` does not: before
/// the forward, only the build heard it, wrote `interrupted by SIGTERM` into
/// its scan record and exited, while the scan it was waiting on carried on
/// writing an index — and a second `carrick index` in that window was refused
/// by a slot whose run the user had been told had stopped.
///
/// The scan here is held on the sidecar readiness budget, set far past this
/// test's deadline, so the child cannot end on its own inside the test: what
/// ends it is the forwarded signal or nothing.
#[test]
fn a_signal_to_a_build_reaches_the_scan_it_is_waiting_on() {
    let home = tempfile::tempdir().expect("a home for the run");
    let sidecar = home.path().join("sidecar/dist/src");
    std::fs::create_dir_all(&sidecar).expect("a directory for the sidecar");
    std::fs::write(
        sidecar.join("index.js"),
        "process.stdin.resume();\nsetInterval(() => {}, 60000);\n",
    )
    .expect("write the sidecar that never answers");

    let workspace = tempfile::tempdir().expect("a workspace for the build");
    let repo = workspace.path().join("api");
    copy_tree(&repo_root().join("examples/express-single"), &repo);
    git(&repo, &["init", "-q", "."]);
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=fixture@carrick.test",
            "-c",
            "user.name=fixture",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    std::fs::write(
        workspace.path().join("carrick-workspace.json"),
        "{ \"repos\": [\"./api\"] }\n",
    )
    .expect("write the workspace file");

    let mut build = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .args(["refresh", "--workspace", "."])
        .current_dir(workspace.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", workspace.path().join(".credentials"))
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_ALLOW_MISSING_TYPES", "1")
        .env("CARRICK_SIDECAR_DIR", home.path().join("sidecar"))
        .env("CARRICK_SIDECAR_READY_TIMEOUT_SECS", "600")
        .env_remove("CARRICK_TOKEN")
        .env_remove("CARRICK_RUN_ID")
        .env_remove("CARRICK_RUN_PHASE")
        .env_remove("GITHUB_REPOSITORY")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("CI")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start a build");

    let scan = scan_child_of(build.id(), DEADLINE);
    send(&build, libc::SIGTERM);

    let code = code_within(&mut build, DEADLINE, "the build never ended");
    assert_eq!(code, TERMINATED);

    // The point of the ticket. Without the forward this child serves the whole
    // readiness budget, which is set past this deadline on purpose.
    let start = Instant::now();
    while alive(scan) {
        if start.elapsed() > DEADLINE {
            // SAFETY: a descendant this test started, left running by a
            // failure it is about to report.
            unsafe { libc::kill(scan, libc::SIGKILL) };
            panic!(
                "the scan the build was waiting on was still running {:.0}s after the build was \
                 signalled",
                start.elapsed().as_secs_f64()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether `pid` still names a process. `kill(pid, 0)` answers exactly that
/// and sends nothing.
fn alive(pid: libc::pid_t) -> bool {
    // SAFETY: signal zero performs no delivery; it is the existence check.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// The scan subprocess `parent` starts, waited for rather than assumed.
///
/// Matched by NAME, not by being the first child listed. A build runs `git`
/// subprocesses of its own before it starts a scan, each of which lives a few
/// milliseconds — and a test that caught one of those would be watching a pid
/// that was about to exit whatever happened, which answers green to the
/// question this test asks whether the signal was forwarded or not.
///
/// Which moment the scan appears in is the machine's to decide, so this waits
/// for it rather than sleeping for a guess.
fn scan_child_of(parent: u32, deadline: Duration) -> libc::pid_t {
    let start = Instant::now();
    loop {
        let listed = Command::new("pgrep")
            .args(["-P", &parent.to_string(), "-x", "carrick"])
            .output()
            .expect("pgrep is what lists a process's children on both platforms this runs on");
        let first = String::from_utf8_lossy(&listed.stdout)
            .lines()
            .find_map(|line| line.trim().parse::<libc::pid_t>().ok());
        if let Some(pid) = first {
            return pid;
        }
        assert!(
            start.elapsed() <= deadline,
            "the build started no scan in {:.0}s",
            start.elapsed().as_secs_f64()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A copy of one fixture tree, without the build artefacts a checkout carries.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create the copy's directory");
    for entry in std::fs::read_dir(from).expect("read the fixture") {
        let entry = entry.expect("a fixture entry");
        let name = entry.file_name();
        if name == "node_modules" || name == ".git" {
            continue;
        }
        let target = to.join(&name);
        if entry.file_type().expect("a file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy a fixture file");
        }
    }
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(repo)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}
