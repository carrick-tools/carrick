//! The resident type daemon from the CLI's side (carrick#2884): a re-check
//! answers the same through it as without it, falls back to a process of its
//! own when it is gone or goes away mid-request, and never asks a daemon of
//! another build.
//!
//! The warm == cold proof of what the daemon itself answers is the sidecar's
//! (`src/sidecar/test/typed-*.test.ts`). These tests drive the binary, as a
//! post-edit hook and an agent do.

#![cfg(unix)]

use carrick::services::typed_daemon::{self, Probe};
use serial_test::serial;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn carrick() -> &'static str {
    env!("CARGO_BIN_EXE_carrick")
}

fn sidecar_entry() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js")
}

fn build_id() -> String {
    typed_daemon::build_id(&sidecar_entry()).expect("the sidecar is built")
}

/// The local-mode fixture copied into a fresh workspace, each repo committed.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let source =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/local-mode-workspace");
    let repos = ["catalog-web", "inventory-svc"];
    for repo in repos {
        copy_tree(&source.join(repo), &dir.path().join(repo));
        git(&dir.path().join(repo), &["init", "-q", "."]);
        git(&dir.path().join(repo), &["add", "-A"]);
        git(
            &dir.path().join(repo),
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
    }
    std::fs::write(
        dir.path().join("carrick-workspace.json"),
        "{ \"repos\": [\"./catalog-web\", \"./inventory-svc\"] }\n",
    )
    .expect("write the workspace file");
    dir
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Run carrick with the daemon on or off; exit code and stdout.
fn run(root: &Path, args: &[&str], daemon: bool) -> (Option<i32>, String) {
    let mut command = Command::new(carrick());
    command
        .args(args)
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .env("CARRICK_RECHECK_BUDGET_MS", "600000")
        .env("CARRICK_CHECK_BUDGET_MS", "600000")
        .env(
            "CARRICK_SIDECAR_DIR",
            sidecar_entry()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap(),
        );
    if !daemon {
        command.env("CARRICK_TYPED", "off");
    }
    let output = command.output().unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).expect("stdout was not UTF-8"),
    )
}

/// The answer with the one field that measures this run's wall clock removed.
fn without_timing(text: &str) -> serde_json::Value {
    let mut value: serde_json::Value =
        serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON: {e}\n{text}"));
    if let Some(recheck) = value.get_mut("recheck").and_then(|r| r.as_object_mut()) {
        recheck.remove("elapsed_ms");
    }
    if let Some(object) = value.as_object_mut() {
        object.remove("elapsed_ms");
    }
    value
}

fn serving(root: &Path) -> Option<typed_daemon::Status> {
    match typed_daemon::probe(root, &build_id(), Duration::from_secs(5)) {
        Probe::Serving(status) => Some(status),
        _ => None,
    }
}

fn wait_for(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < timeout, "gave up waiting for {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn replayed(status: &typed_daemon::Status) -> u64 {
    status.services.iter().map(|s| s.items.replayed).sum()
}

/// Stops the daemon a test started, whatever the test's outcome.
struct StopOnDrop(PathBuf);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        let _ = Command::new(carrick())
            .args(["daemon", "stop", "--workspace"])
            .arg(&self.0)
            .output();
    }
}

const ROUTE: &str = "catalog-web/app/routes/api.v1.widgets.$widgetId.ts";

fn break_the_producer(root: &Path) {
    let file = root.join(ROUTE);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("activeCount: number") && text.contains("activeCount: 3"));
    std::fs::write(
        &file,
        text.replace("activeCount: number", "activeCount: string")
            .replace("activeCount: 3", "activeCount: \"3\""),
    )
    .unwrap();
}

/// A re-check and `check --changed` answer the same, byte for byte and exit
/// code for exit code, cold, through a daemon that was just warmed, and again
/// through it once it has kept its answers. The warm runs did answer through
/// it: it held the service's program and answered kept items again.
#[test]
#[serial]
fn a_check_answers_the_same_through_the_daemon_as_without_it() {
    let dir = workspace();
    let root = dir.path().canonicalize().unwrap();
    let _stop = StopOnDrop(root.clone());
    let (code, _) = run(&root, &["refresh", "--workspace", "."], false);
    assert_eq!(code, Some(0));
    break_the_producer(&root);

    let recheck = ["check", ROUTE, "--workspace", ".", "--recheck", "--json"];
    let changed = ["check", "--changed", "--workspace", ".", "--json"];
    let (cold_code, cold) = run(&root, &recheck, false);
    let (cold_changed_code, cold_changed) = run(&root, &changed, false);
    assert!(
        cold.contains("type_mismatch"),
        "the cold re-check finds the break:\n{cold}"
    );
    assert!(
        typed_daemon::read_record(&root).is_none(),
        "with the daemon off, none is started"
    );

    // Started and warmed, as the session-start hook does.
    let (code, said) = run(
        &root,
        &["daemon", "start", "--warm", "--workspace", "."],
        true,
    );
    assert_eq!(code, Some(0), "{said}");
    let status = serving(&root).expect("a daemon of this build serves the workspace");
    assert!(
        !status.built().is_empty(),
        "the warm-up built a program: {status:?}"
    );

    for round in 0..2 {
        let before = replayed(&serving(&root).unwrap());
        let (code, warm) = run(&root, &recheck, true);
        assert_eq!(code, cold_code);
        assert_eq!(
            without_timing(&warm),
            without_timing(&cold),
            "round {round}"
        );
        let (code, warm_changed) = run(&root, &changed, true);
        assert_eq!(code, cold_changed_code);
        assert_eq!(
            without_timing(&warm_changed),
            without_timing(&cold_changed),
            "round {round}"
        );
        let after = serving(&root).expect("still serving");
        assert!(
            replayed(&after) > before,
            "round {round}: the checks answered through the daemon, which answered kept items \
             again: {before} -> {after:?}"
        );
    }

    let (code, text) = run(&root, &["status"], true);
    assert_eq!(code, Some(0));
    assert!(
        text.contains("Type daemon: pid"),
        "status names the daemon:\n{text}"
    );
    let pid = status.pid;
    let (code, said) = run(&root, &["daemon", "stop", "--workspace", "."], true);
    assert_eq!(code, Some(0), "{said}");
    assert!(!typed_daemon::pid_alive(pid), "stopped");
    assert!(
        typed_daemon::read_record(&root).is_none(),
        "and its record removed"
    );
}

/// A daemon killed outright leaves its record behind. The next re-check
/// answers cold, the same as with no daemon, and starts a fresh one in the
/// background for the run after it.
#[test]
#[serial]
fn a_daemon_that_died_is_replaced_and_its_run_answers_cold() {
    let dir = workspace();
    let root = dir.path().canonicalize().unwrap();
    let _stop = StopOnDrop(root.clone());
    run(&root, &["refresh", "--workspace", "."], false);
    break_the_producer(&root);
    let recheck = ["check", ROUTE, "--workspace", ".", "--recheck", "--json"];
    let (_, cold) = run(&root, &recheck, false);

    let (code, _) = run(&root, &["daemon", "start", "--workspace", "."], true);
    assert_eq!(code, Some(0));
    let dead = serving(&root).unwrap().pid;
    typed_daemon::kill(dead);
    assert!(
        typed_daemon::read_record(&root).is_some(),
        "a killed daemon leaves its record"
    );

    let (_, answered) = run(&root, &recheck, true);
    assert_eq!(without_timing(&answered), without_timing(&cold));
    wait_for("a fresh daemon", Duration::from_secs(120), || {
        serving(&root).is_some_and(|status| status.pid != dead)
    });
}

/// A daemon of another build never answers: its `hello` is refused, the
/// re-check answers cold, and the stale daemon is killed and replaced.
#[test]
#[serial]
fn a_daemon_of_another_build_is_replaced_and_never_asked() {
    let dir = workspace();
    let root = dir.path().canonicalize().unwrap();
    let _stop = StopOnDrop(root.clone());
    run(&root, &["refresh", "--workspace", "."], false);
    break_the_producer(&root);
    let recheck = ["check", ROUTE, "--workspace", ".", "--recheck", "--json"];
    let (_, cold) = run(&root, &recheck, false);

    // Another build: a copy of the sidecar with one byte more in one file.
    let other = tempfile::tempdir().unwrap();
    let sidecar = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar");
    copy_tree(&sidecar.join("dist"), &other.path().join("dist"));
    std::os::unix::fs::symlink(
        sidecar.join("node_modules"),
        other.path().join("node_modules"),
    )
    .unwrap();
    let entry = other.path().join("dist/src/index.js");
    let mut text = std::fs::read_to_string(&entry).unwrap();
    text.push('\n');
    std::fs::write(&entry, text).unwrap();
    typed_daemon::start_detached(&root, &entry).unwrap();
    let other_id = typed_daemon::build_id(&entry).unwrap();
    assert_ne!(other_id, build_id());
    let stale = typed_daemon::wait_serving(&root, &other_id, Duration::from_secs(30))
        .expect("the other build serves")
        .pid;
    assert!(matches!(
        typed_daemon::probe(&root, &build_id(), Duration::from_secs(5)),
        Probe::Mismatch { .. }
    ));

    let (_, answered) = run(&root, &recheck, true);
    assert_eq!(without_timing(&answered), without_timing(&cold));
    // The stale daemon is this test's child, so a kill leaves it a zombie
    // here rather than gone: what is checked is that it no longer serves.
    wait_for("a daemon of this build", Duration::from_secs(120), || {
        serving(&root).is_some_and(|status| status.pid != stale)
    });
}

/// The daemon goes away in the middle of a request: the request is answered
/// by a process of the sidecar's own, with the answer a scan without a daemon
/// reads. A stand-in daemon accepts `hello` and `init` and closes the
/// connection on the next request.
#[test]
#[serial]
fn a_request_the_daemon_drops_is_answered_cold() {
    use carrick::services::TypeSidecar;
    use std::io::{BufRead, BufReader, Write};

    let service = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/local-mode-workspace/inventory-svc")
        .canonicalize()
        .unwrap();
    let cold = TypeSidecar::spawn(&sidecar_entry()).unwrap();
    cold.start_init(&service, None);
    cold.wait_ready(Duration::from_secs(120)).unwrap();
    let item = || carrick::services::type_sidecar::InferRequestItem {
        file_path: service
            .join("src/inventory.ts")
            .to_string_lossy()
            .into_owned(),
        line_number: 5,
        span_start: None,
        span_end: None,
        expression_text: None,
        expression_line: None,
        infer_kind: carrick::services::type_sidecar::InferKind::SignatureReturn,
        alias: Some("CountActive".to_string()),
        param_name: None,
        member: None,
        result: None,
    };
    let inferred = |sidecar: &TypeSidecar| {
        let response = sidecar.infer_types(&[item()], None).expect("an answer");
        serde_json::to_string(&response.inferred_types).unwrap()
    };
    let expected = inferred(&cold);
    assert!(expected.contains("Promise"), "{expected}");

    let sockets = tempfile::tempdir().unwrap();
    let socket = sockets.path().join("d.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let stand_in = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut lines = BufReader::new(stream).lines();
        for answered in 0.. {
            let Some(Ok(line)) = lines.next() else { return };
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            let id = request["request_id"].clone();
            let answer = match (answered, request["action"].as_str()) {
                (0, Some("hello")) => {
                    serde_json::json!({"request_id": id, "status": "success", "pid": 1})
                }
                (1, Some("init")) => serde_json::json!({"request_id": id, "status": "ready"}),
                // Gone, mid-request.
                _ => return,
            };
            writeln!(writer, "{answer}").unwrap();
        }
    });

    let warm = TypeSidecar::connect_daemon(&sidecar_entry(), &socket, "any").unwrap();
    warm.start_init(&service, None);
    warm.wait_ready(Duration::from_secs(30)).unwrap();
    assert!(warm.on_daemon());
    let answered = inferred(&warm);
    assert!(!warm.on_daemon(), "and answered by a process of its own");
    assert_eq!(answered, expected);
    stand_in.join().unwrap();
}

/// The build id the CLI computes is the one the sidecar computes: a daemon
/// compares every `hello` against its own.
#[test]
fn the_build_id_is_the_sidecars_own() {
    let out = Command::new("node")
        .arg(sidecar_entry())
        .arg("--build-id")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), build_id());
}
