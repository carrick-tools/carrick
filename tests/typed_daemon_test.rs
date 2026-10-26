//! The resident type daemon from the CLI's side (carrick#2884): a re-check
//! answers the same through it as without it, falls back to a process of its
//! own when it is gone or goes away mid-request, and never asks a daemon of
//! another build.
//!
//! The warm == cold proof of what the daemon itself answers is the sidecar's
//! (`src/sidecar/test/typed-*.test.ts`). These tests drive the binary, as a
//! post-edit hook and an agent do.

#![cfg(unix)]

use carrick::services::typed_daemon::{self, HelloMode, Probe};
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

/// SIGKILL `pid`, as an out-of-heap abort or a crash would end it.
fn sigkill(pid: u32) {
    let out = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .output()
        .unwrap();
    assert!(out.status.success(), "kill -9 {pid}");
    wait_for(
        "the killed daemon to be gone",
        Duration::from_secs(10),
        || {
            !typed_daemon::pid_alive(pid)
                || Command::new("ps")
                    .args(["-o", "stat=", "-p", &pid.to_string()])
                    .output()
                    .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim().starts_with('Z'))
        },
    );
}

/// A stand-in daemon on a socket in `root/.carrick`, with a record naming
/// this test process (alive, ours) as its pid and `claimed` as its workspace.
/// Every connection's `hello` is answered with that pid and workspace, then
/// `answer` answers each later frame; `None` reads `hello` and answers nothing.
struct StandIn {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl StandIn {
    fn start(
        root: &Path,
        claimed: &Path,
        answer: Option<fn(&serde_json::Value) -> serde_json::Value>,
    ) -> Self {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::atomic::Ordering;
        let carrick_dir = root.join(".carrick");
        std::fs::create_dir_all(&carrick_dir).unwrap();
        let socket = carrick_dir.join("typed.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let pid = std::process::id();
        std::fs::write(
            carrick_dir.join("typed.json"),
            serde_json::json!({
                "pid": pid,
                "socket": socket,
                "workspace": claimed,
                "token": "t",
                "started_at": chrono::Utc::now().to_rfc3339(),
            })
            .to_string(),
        )
        .unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = stop.clone();
        let claimed = claimed.to_path_buf();
        let thread = std::thread::spawn(move || {
            let mut held = Vec::new();
            while !stopping.load(Ordering::Relaxed) {
                let Ok((stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                let claimed = claimed.clone();
                held.push(stream.try_clone().unwrap());
                std::thread::spawn(move || {
                    let mut writer = stream.try_clone().unwrap();
                    let mut lines = BufReader::new(stream).lines();
                    let mut greeted = false;
                    while let Some(Ok(line)) = lines.next() {
                        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                        let id = request["request_id"].clone();
                        let frame = if !greeted {
                            greeted = true;
                            if answer.is_none() {
                                continue;
                            }
                            serde_json::json!({
                                "request_id": id, "status": "success",
                                "pid": pid, "workspace": claimed,
                            })
                        } else {
                            let Some(answer) = answer else { continue };
                            let mut frame = answer(&request);
                            frame["request_id"] = id;
                            frame
                        };
                        if writeln!(writer, "{frame}").is_err() {
                            return;
                        }
                    }
                });
            }
            drop(held);
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A `status` that says a warm-up is filling the daemon.
fn warming_status(request: &serde_json::Value) -> serde_json::Value {
    assert_eq!(request["action"], "status");
    serde_json::json!({
        "status": "success",
        "daemon": { "pid": std::process::id(), "socket": "", "services": [], "warming": true },
    })
}

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
    let record = typed_daemon::read_record(&root).unwrap();
    assert!(
        typed_daemon::identity(&record, &root),
        "it is this workspace's daemon"
    );
    sigkill(dead);
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

    let warm =
        TypeSidecar::connect_daemon(&sidecar_entry(), &socket, "any", HelloMode::Probe).unwrap();
    warm.start_init(&service, None);
    warm.wait_ready(Duration::from_secs(30)).unwrap();
    assert!(warm.on_daemon());
    let answered = inferred(&warm);
    assert!(!warm.on_daemon(), "and answered by a process of its own");
    assert_eq!(answered, expected);
    stand_in.join().unwrap();
}

/// The daemon answers a request with an error a process of its own would not
/// give (files kept changing under its held program): the request is answered
/// by a process of the sidecar's own instead, as without a daemon. A stand-in
/// daemon accepts `hello` and `init` and answers the next request with that
/// error, keeping the connection open.
#[test]
#[serial]
fn an_error_the_daemon_answers_is_answered_cold() {
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

    let sockets = tempfile::tempdir().unwrap();
    let socket = sockets.path().join("d.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let (done, finished) = std::sync::mpsc::channel::<()>();
    let stand_in = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut lines = BufReader::new(stream).lines();
        for answered in 0..3 {
            let Some(Ok(line)) = lines.next() else { return };
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            let id = request["request_id"].clone();
            let answer = match (answered, request["action"].as_str()) {
                (0, Some("hello")) => {
                    serde_json::json!({"request_id": id, "status": "success", "pid": 1})
                }
                (1, Some("init")) => serde_json::json!({"request_id": id, "status": "ready"}),
                _ => serde_json::json!({
                    "request_id": id,
                    "status": "error",
                    "errors": ["files kept changing while the request ran (3 answers dropped)"],
                }),
            };
            writeln!(writer, "{answer}").unwrap();
        }
        // Held open until the test is done: the error is the answer, not a
        // closed connection.
        let _ = finished.recv();
    });

    let warm =
        TypeSidecar::connect_daemon(&sidecar_entry(), &socket, "any", HelloMode::Probe).unwrap();
    warm.start_init(&service, None);
    warm.wait_ready(Duration::from_secs(30)).unwrap();
    assert!(warm.on_daemon());
    let answered = inferred(&warm);
    assert!(!warm.on_daemon(), "and answered by a process of its own");
    assert_eq!(answered, expected);
    done.send(()).unwrap();
    stand_in.join().unwrap();
}

/// A daemon that does not answer `hello` within a second is answering someone
/// else: the connection is given up at once, not after the readiness budget,
/// and the service answered by a process of its own.
#[test]
#[serial]
fn a_daemon_that_does_not_answer_hello_is_left_at_once() {
    use carrick::services::TypeSidecar;

    let sockets = tempfile::tempdir().unwrap();
    let socket = sockets.path().join("d.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let (done, finished) = std::sync::mpsc::channel::<()>();
    let stand_in = std::thread::spawn(move || {
        // Accepts, reads nothing, answers nothing.
        let (_stream, _) = listener.accept().unwrap();
        let _ = finished.recv();
    });
    let started = Instant::now();
    let connected = TypeSidecar::connect_daemon(&sidecar_entry(), &socket, "any", HelloMode::Probe);
    let waited = started.elapsed();
    done.send(()).unwrap();
    stand_in.join().unwrap();
    assert!(connected.is_err(), "a silent daemon is not used");
    assert!(
        waited < Duration::from_secs(5),
        "gave it up after {waited:?}"
    );
}

/// `daemon stop` signals only a process that is the workspace's daemon. A
/// record left by a daemon that was killed names a pid the kernel may have
/// handed to another process: that process is left running and the stale
/// record removed.
#[test]
#[serial]
fn daemon_stop_leaves_another_process_named_by_a_stale_record() {
    let dir = workspace();
    let root = dir.path().canonicalize().unwrap();
    let mut other = Command::new("sleep").arg("60").spawn().unwrap();
    let carrick_dir = root.join(".carrick");
    std::fs::create_dir_all(&carrick_dir).unwrap();
    std::fs::write(
        carrick_dir.join("typed.json"),
        serde_json::json!({
            "pid": other.id(),
            "socket": carrick_dir.join("typed.sock"),
        })
        .to_string(),
    )
    .unwrap();
    assert!(!typed_daemon::identity(
        &typed_daemon::read_record(&root).unwrap(),
        &root
    ));

    let (code, said) = run(&root, &["daemon", "stop", "--workspace", "."], true);
    assert_eq!(code, Some(0), "{said}");
    let still_running = other.try_wait().unwrap().is_none();
    let _ = other.kill();
    let _ = other.wait();
    assert!(still_running, "daemon stop killed another process: {said}");
    assert!(
        typed_daemon::read_record(&root).is_none(),
        "the stale record is removed"
    );
}

/// A record whose process is gone is a crash: the probe notes it and removes
/// the record. Two crashes of one build within the hour back it off: no daemon
/// is started, and `carrick status` says so.
#[test]
#[serial]
fn two_crashes_of_one_build_back_it_off() {
    let dir = workspace();
    let root = dir.path().canonicalize().unwrap();
    let carrick_dir = root.join(".carrick");
    std::fs::create_dir_all(&carrick_dir).unwrap();
    let id = build_id();
    for _ in 0..2 {
        let mut gone = Command::new("true").spawn().unwrap();
        gone.wait().unwrap();
        std::fs::write(
            carrick_dir.join("typed.json"),
            serde_json::json!({
                "pid": gone.id(),
                "socket": carrick_dir.join("typed.sock"),
                "build_id": id,
            })
            .to_string(),
        )
        .unwrap();
        assert!(matches!(
            typed_daemon::probe(&root, &id, Duration::from_secs(1)),
            Probe::Absent
        ));
        assert!(
            typed_daemon::read_record(&root).is_none(),
            "the dead record is removed"
        );
    }
    let crashes = std::fs::read_to_string(carrick_dir.join("typed.crashes")).unwrap();
    assert_eq!(crashes.lines().count(), 2, "{crashes}");
    assert!(typed_daemon::backed_off(&root, &id).is_some());

    let (code, _) = run(&root, &["daemon", "start", "--workspace", "."], true);
    assert_ne!(code, Some(0), "a backed-off build is not started");
    assert!(typed_daemon::read_record(&root).is_none());
    let (lines, _) = typed_daemon::status_report(&root, Some(&id));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("off until") && line.contains("2 crashes")),
        "{lines:?}"
    );
    // Another build starts afresh.
    assert!(typed_daemon::backed_off(&root, "another build").is_none());
}

/// A daemon a warm-up is filling is busy for a check: the check answers cold,
/// the same as with no daemon, and starts no second warm-up.
#[test]
#[serial]
fn a_warming_daemon_leaves_the_check_cold_and_starts_nothing() {
    let dir = workspace();
    let root = dir.path().canonicalize().unwrap();
    run(&root, &["refresh", "--workspace", "."], false);
    break_the_producer(&root);
    let recheck = ["check", ROUTE, "--workspace", ".", "--recheck", "--json"];
    let (cold_code, cold) = run(&root, &recheck, false);

    let stand_in = StandIn::start(&root, &root, Some(warming_status));
    let (code, answered) = run(&root, &recheck, true);
    assert_eq!(code, cold_code);
    assert_eq!(without_timing(&answered), without_timing(&cold));
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(
        typed_daemon::read_record(&root).map(|r| r.pid),
        Some(std::process::id()),
        "no daemon was started in its place"
    );
    assert!(
        !root.join(".carrick/typed-warm.pid").exists(),
        "no warm-up was started"
    );
    drop(stand_in);
}

/// A daemon that accepts a connection and never answers: the check's output
/// and exit code are those of `CARRICK_TYPED=off`.
#[test]
#[serial]
fn a_daemon_that_never_answers_leaves_the_check_as_with_none() {
    let dir = workspace();
    let root = dir.path().canonicalize().unwrap();
    run(&root, &["refresh", "--workspace", "."], false);
    break_the_producer(&root);
    for args in [
        vec!["check", ROUTE, "--workspace", ".", "--recheck", "--json"],
        vec!["check", "--changed", "--workspace", ".", "--json"],
    ] {
        let (cold_code, cold) = run(&root, &args, false);
        let stand_in = StandIn::start(&root, &root, None);
        let started = Instant::now();
        let (code, answered) = run(&root, &args, true);
        drop(stand_in);
        assert_eq!(code, cold_code, "{args:?}");
        assert_eq!(without_timing(&answered), without_timing(&cold), "{args:?}");
        assert!(started.elapsed() < Duration::from_secs(300));
    }
}

/// A record copied with a workspace names another workspace's daemon: its
/// `hello` names that other workspace, so the copy does not use it.
#[test]
#[serial]
fn a_copied_workspaces_record_is_not_used() {
    let original = tempfile::tempdir().unwrap();
    let copy = tempfile::tempdir().unwrap();
    let original_root = original.path().canonicalize().unwrap();
    let copy_root = copy.path().canonicalize().unwrap();
    let stand_in = StandIn::start(&copy_root, &original_root, Some(warming_status));
    assert!(matches!(
        typed_daemon::probe(&copy_root, &build_id(), Duration::from_secs(2)),
        Probe::Absent
    ));
    drop(stand_in);
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
