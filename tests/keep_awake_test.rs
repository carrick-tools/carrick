//! The hold a scan takes on the machine's idle sleep, driven at the built
//! binary on every way a scan ends (carrick#1889).
//!
//! The one outcome that is not allowed is a hold that outlives its scan, and
//! the paths that could leave one are the ones no in-process test reaches: a
//! `process::exit`, a signal, a crash, a `kill -9`. None of them runs a
//! destructor, so what releases the hold there is the helper noticing that the
//! pid it watches has gone. These tests start real scans, end them each way,
//! and look for the helper afterwards.
//!
//! The helper is a stand-in, named through the variable that exists for it,
//! because the platform's own exists on one platform and CI runs on another.
//! It does the two things the real one does that matter here: it is started
//! with `-i -w <pid>`, and it stays for exactly as long as that pid does. It
//! also writes down its own pid and what it was started with, which is how a
//! test knows the hold was asked for, how many times, and for whom. The last
//! test runs the real helper where there is one.
//!
//! Serial, because each test writes its stand-in and then has a scan execute
//! it: a file another thread's fork still holds open for writing cannot be
//! executed, and tests that never overlap never fork across that write.

#![cfg(unix)]

use serial_test::serial;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use carrick::keep_awake::{HELPER_ENV, LINE, OFF_ENV, OFF_FLAG, SAID_ENV};

/// The variable the stand-in reads for where to write its record. Its own,
/// not the scanner's: the scanner passes its environment on and knows nothing
/// of it.
const RECORD_ENV: &str = "KEEP_AWAKE_TEST_RECORD";

/// Far past what any of these needs, and short enough that a helper which
/// never goes fails the test rather than holding CI.
const DEADLINE: Duration = Duration::from_secs(60);

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Where a test keeps its stand-in, its record and the scan's home.
struct Bench {
    dir: tempfile::TempDir,
}

impl Bench {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a directory for the test");
        let helper = dir.path().join("stand-in-helper");
        // `$3` is the pid after `-i -w`. A zombie still answers `kill -0`, so
        // a test reaps its scan before it looks for this to have gone; the
        // real helper is told by the kernel at the exit itself.
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\n\
                 printf '%s %s\\n' \"$$\" \"$*\" >> \"${RECORD_ENV}\"\n\
                 while kill -0 \"$3\" 2>/dev/null; do sleep 0.1; done\n"
            ),
        )
        .expect("write the stand-in helper");
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755))
            .expect("make the stand-in executable");
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn record(&self) -> PathBuf {
        self.path().join("record")
    }

    /// Every hold that was asked for: the helper's pid, and its arguments.
    fn holds(&self) -> Vec<(i32, String)> {
        std::fs::read_to_string(self.record())
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let (pid, arguments) = line.split_once(' ')?;
                Some((pid.parse().ok()?, arguments.to_string()))
            })
            .collect()
    }

    /// Wait for the first hold to be asked for, and answer it.
    fn first_hold(&self) -> (i32, String) {
        let start = Instant::now();
        loop {
            if let Some(hold) = self.holds().into_iter().next() {
                return hold;
            }
            assert!(
                start.elapsed() < DEADLINE,
                "the scan never asked for a hold"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The environment every run here shares: the stand-in as the helper, and
    /// nothing inherited that would decide the hold for it. A CI runner sets
    /// `CI`, and these runs are about a machine that is not one.
    fn configure(&self, command: &mut Command) {
        command
            .env(HELPER_ENV, self.path().join("stand-in-helper"))
            .env(RECORD_ENV, self.record())
            .env("XDG_CONFIG_HOME", self.path().join("credentials"))
            .env_remove("CARRICK_TOKEN")
            .env_remove(OFF_ENV)
            .env_remove(SAID_ENV)
            .env_remove("CARRICK_RUN_ID")
            .env_remove("CARRICK_RUN_PHASE")
            .env_remove("CARRICK_PROGRESS")
            .env_remove("GITHUB_REPOSITORY")
            .env_remove("GITHUB_ACTIONS")
            .env_remove("CI")
            .stdin(Stdio::null());
    }

    /// A mocked scan of `target`: the plain path the Action and the kits run.
    fn scan(&self, target: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_carrick"));
        command
            .arg(target)
            .arg("--allow-unprepared")
            .env("HOME", self.path())
            .env("CARRICK_MOCK_ALL", "1");
        self.configure(&mut command);
        command
    }

    /// A scan that finishes on its own in a few seconds. The fixture has no
    /// dependencies installed, so it is typeless, which this is not about.
    fn short_scan(&self) -> Command {
        let mut command = self.scan(&repo_root().join("examples/express-single"));
        command.env("CARRICK_ALLOW_MISSING_TYPES", "1");
        command
    }

    /// A scan that is still in its first wait when the test acts on it: its
    /// sidecar holds its streams open and never answers, for `budget_secs`.
    fn waiting_scan(&self, budget_secs: u64) -> Command {
        let sidecar = self.path().join("sidecar/dist/src");
        std::fs::create_dir_all(&sidecar).expect("a directory for the sidecar");
        std::fs::write(
            sidecar.join("index.js"),
            "process.stdin.resume();\nsetInterval(() => {}, 60000);\n",
        )
        .expect("write the sidecar that never answers");
        let mut command = self.scan(&repo_root().join("examples/express-single"));
        command
            .env("CARRICK_SIDECAR_DIR", self.path().join("sidecar"))
            .env(
                "CARRICK_SIDECAR_READY_TIMEOUT_SECS",
                budget_secs.to_string(),
            )
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }
}

/// Whether a process is still there to hold anything. One that has exited and
/// not been collected holds nothing, and reads as gone.
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 delivers nothing; it asks whether the pid exists.
    if unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    let state = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default();
    !state.is_empty() && !state.starts_with('Z')
}

/// The helper goes, or the test fails saying how long it stayed.
fn assert_released(helper: i32, after: &str) {
    let start = Instant::now();
    while alive(helper) {
        assert!(
            start.elapsed() < DEADLINE,
            "the hold outlived the scan: its helper (pid {helper}) was still running {:.0}s after \
             {after}",
            start.elapsed().as_secs_f64()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn occurrences(text: &str, of: &str) -> usize {
    text.matches(of).count()
}

fn send(child: &Child, signal: libc::c_int) {
    // SAFETY: a signal to a child this test spawned and has not reaped.
    let sent = unsafe { libc::kill(child.id() as libc::pid_t, signal) };
    assert_eq!(sent, 0, "could not signal the scan");
}

/// Start a scan that is waiting, see that it holds, end it with `signal`, and
/// see that the hold went with it.
fn a_scan_ended_by(signal: libc::c_int, name: &str) -> std::process::ExitStatus {
    let bench = Bench::new();
    let mut child = bench.waiting_scan(600).spawn().expect("start a scan");
    let scan = child.id();

    let (helper, arguments) = bench.first_hold();
    assert_eq!(
        arguments,
        format!("-i -w {scan}"),
        "the hold is tied to the scan's own pid"
    );
    assert!(alive(helper), "the scan holds while it runs");
    assert!(
        child.try_wait().expect("poll the scan").is_none(),
        "the scan was over before it could be ended"
    );

    send(&child, signal);
    let status = child.wait().expect("collect the scan");
    assert_released(helper, &format!("the scan was ended by {name}"));
    assert_eq!(bench.holds().len(), 1, "one hold for one scan");
    status
}

/// A scan that finishes: one hold, tied to the scan's own pid, one line saying
/// so on the stream the scan's other lines are on, and no helper afterwards.
#[test]
#[serial]
fn a_scan_that_finishes_held_once_said_so_once_and_released() {
    let bench = Bench::new();
    let child = bench
        .short_scan()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start a scan");
    let scan = child.id();
    let output = child.wait_with_output().expect("the scan ends");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "the scan failed:\n{stderr}");

    let holds = bench.holds();
    assert_eq!(holds.len(), 1, "one hold for one scan: {holds:?}");
    assert_eq!(holds[0].1, format!("-i -w {scan}"));
    assert_eq!(
        occurrences(&stderr, LINE),
        1,
        "the line is said once:\n{stderr}"
    );
    assert_eq!(
        occurrences(&stdout, LINE),
        0,
        "stdout is the scan's report, and the line is not part of it"
    );
    assert_released(holds[0].0, "the scan finished");
}

/// A scan that fails after it started holding: the sidecar never becomes
/// ready, the run stops rather than index without types, and the hold goes.
#[test]
#[serial]
fn a_scan_that_fails_releases_its_hold() {
    let bench = Bench::new();
    let mut child = bench.waiting_scan(1).spawn().expect("start a scan");
    let status = child.wait().expect("the scan ends");
    assert_eq!(status.code(), Some(1), "the scan fails");

    let holds = bench.holds();
    assert_eq!(holds.len(), 1, "it held while it waited: {holds:?}");
    assert_released(holds[0].0, "the scan failed");
}

/// A signal the scan handles: it reports and leaves through `process::exit`,
/// which runs no destructor.
#[test]
#[serial]
fn a_scan_that_is_terminated_releases_its_hold() {
    let status = a_scan_ended_by(libc::SIGTERM, "SIGTERM");
    assert_eq!(status.code(), Some(143));
}

/// A kill: nothing in the scan runs at all, so nothing in it can release
/// anything. The case the helper's watch on the pid exists for.
#[test]
#[serial]
fn a_scan_that_is_killed_releases_its_hold() {
    use std::os::unix::process::ExitStatusExt;
    let status = a_scan_ended_by(libc::SIGKILL, "SIGKILL");
    assert_eq!(status.signal(), Some(libc::SIGKILL));
}

/// A crash: the process is ended by a signal it has no handler for, the way an
/// abort ends one.
///
/// Not run on a Mac, where the system writes a crash report for it into the
/// developer's own reports folder. The kill above is the same case as far as
/// the hold is concerned, and the last test runs it against the real helper.
#[test]
#[serial]
#[cfg_attr(
    target_os = "macos",
    ignore = "macOS files a crash report for every abort; the kill covers the same path"
)]
fn a_scan_that_crashes_releases_its_hold() {
    use std::os::unix::process::ExitStatusExt;
    let status = a_scan_ended_by(libc::SIGABRT, "SIGABRT");
    assert_eq!(status.signal(), Some(libc::SIGABRT));
}

/// Each way of turning it off: the flag, the variable, and a CI runner under
/// either of the names one goes by. Nothing is held and nothing is said.
#[test]
#[serial]
fn a_scan_told_not_to_hold_asks_for_nothing_and_says_nothing() {
    type Case = (&'static str, fn(&mut Command));
    let cases: [Case; 4] = [
        ("the flag", |command| {
            command.arg(OFF_FLAG);
        }),
        ("the variable", |command| {
            command.env(OFF_ENV, "1");
        }),
        ("CI", |command| {
            command.env("CI", "true");
        }),
        ("GITHUB_ACTIONS", |command| {
            command.env("GITHUB_ACTIONS", "true");
        }),
    ];
    for (name, apply) in cases {
        let bench = Bench::new();
        let mut command = bench.short_scan();
        apply(&mut command);
        let output = command.output().expect("run a scan");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{name}: the scan failed:\n{stderr}"
        );
        assert!(
            bench.holds().is_empty(),
            "{name}: a hold was asked for: {:?}",
            bench.holds()
        );
        assert_eq!(occurrences(&stderr, LINE), 0, "{name}:\n{stderr}");
        assert_eq!(
            occurrences(&String::from_utf8_lossy(&output.stdout), LINE),
            0,
            "{name}"
        );
    }
}

/// Copy a fixture's repos into the bench as a workspace of git repositories.
fn workspace(bench: &Bench, repos: &[&str]) -> PathBuf {
    let root = bench.path().join("workspace");
    let source = repo_root().join("tests/fixtures/local-mode-workspace");
    for repo in repos {
        copy_tree(&source.join(repo), &root.join(repo));
        std::fs::write(root.join(repo).join("carrick.json"), "{}\n").expect("write a config");
        git(&root.join(repo), &["init", "-q", "."]);
        git(&root.join(repo), &["add", "-A"]);
        git(
            &root.join(repo),
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
    let listed: Vec<String> = repos.iter().map(|repo| format!("\"./{repo}\"")).collect();
    std::fs::write(
        root.join("carrick-workspace.json"),
        format!("{{ \"repos\": [{}] }}\n", listed.join(", ")),
    )
    .expect("write the workspace file");
    root
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create the destination");
    for entry in std::fs::read_dir(from).unwrap_or_else(|e| panic!("read {}: {e}", from.display()))
    {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy a fixture file");
        }
    }
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} in {}: {e}", repo.display()));
    assert!(
        output.status.success(),
        "git {args:?} failed in {}:\n{}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A build command in the workspace: `index`, `resume` or `refresh`.
fn build(bench: &Bench, root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_carrick"));
    command
        .args(args)
        .current_dir(root)
        .env("CARRICK_MOCK_ALL", "1");
    bench.configure(&mut command);
    command
}

/// A build of two repos is three scans, each a process of its own: one per
/// repo and the join. The build holds once for all of them, tied to its own
/// pid, and says so once, on the stream a renderer shows as it stands.
#[test]
#[serial]
fn a_build_holds_once_for_every_scan_it_runs() {
    let bench = Bench::new();
    let root = workspace(&bench, &["catalog-web", "inventory-svc"]);
    let child = build(&bench, &root, &["refresh", "--workspace", "."])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start a build");
    let build_pid = child.id();
    let output = child.wait_with_output().expect("the build ends");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "the build failed:\n{stderr}");
    assert!(
        stdout.contains("indexed 2 repo(s)"),
        "both repos were scanned:\n{stdout}"
    );

    let holds = bench.holds();
    assert_eq!(
        holds.len(),
        1,
        "one hold for the build, none for the scans it ran: {holds:?}"
    );
    assert_eq!(holds[0].1, format!("-i -w {build_pid}"));
    assert_eq!(
        occurrences(&stdout, LINE),
        1,
        "the line is said once, on stdout:\n{stdout}"
    );
    assert!(
        stdout.find(LINE) < stdout.find("indexed 2 repo(s)"),
        "and before the build, not after it:\n{stdout}"
    );
    assert_eq!(occurrences(&stderr, LINE), 0, "{stderr}");
    assert_released(holds[0].0, "the build finished");
}

/// `index --detach` answers at once and the build it started does the
/// holding: the hold is tied to the build's pid, not to the command that has
/// already gone, the line is said where the user is looking and not again in
/// the log, and the hold goes when the build does.
#[test]
#[serial]
fn a_detached_build_holds_for_itself_and_the_command_that_started_it_says_so() {
    let bench = Bench::new();
    let root = workspace(&bench, &["catalog-web", "inventory-svc"]);
    let output = build(&bench, &root, &["index", "--workspace", ".", "--detach"])
        .output()
        .expect("start a detached build");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "the command failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        occurrences(&stdout, LINE),
        1,
        "the command the user is reading says the line:\n{stdout}"
    );
    let detached: u32 = stdout
        .split("(pid ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("the answer names the build's pid:\n{stdout}"));

    let (helper, arguments) = bench.first_hold();
    assert_eq!(
        arguments,
        format!("-i -w {detached}"),
        "the hold is tied to the build that is still running"
    );

    // The build is not this test's child, so there is nothing to collect: it
    // is over when its index is written and its helper has gone.
    let index = root.join(".carrick").join("index.json");
    let start = Instant::now();
    while !index.is_file() {
        assert!(
            start.elapsed() < Duration::from_secs(300),
            "the detached build wrote no index"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    assert_released(helper, "the detached build finished");
    assert_eq!(bench.holds().len(), 1, "one hold for one build");

    let scan_id = stdout
        .split_whitespace()
        .nth(1)
        .expect("the id is the second word");
    let log = std::fs::read_to_string(root.join(".carrick").join(format!("scan-{scan_id}.log")))
        .expect("the build's log");
    assert!(log.contains("indexed 2 repo(s)"), "{log}");
    assert_eq!(
        occurrences(&log, LINE),
        0,
        "the line was said once already, where the user was looking:\n{log}"
    );
}

/// The real helper, where the platform has one. Nothing stands in: the scan is
/// killed and the `caffeinate` it started is looked for by the pid it watches.
#[cfg(target_os = "macos")]
#[test]
#[serial]
fn on_a_mac_the_real_helper_goes_when_the_scan_is_killed() {
    fn helpers_watching(pid: u32) -> Vec<i32> {
        let output = Command::new("pgrep")
            .args(["-f", &format!("caffeinate -i -w {pid}$")])
            .output()
            .expect("run pgrep");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect()
    }

    let bench = Bench::new();
    let mut command = bench.waiting_scan(600);
    command.env_remove(HELPER_ENV);
    let mut child = command.spawn().expect("start a scan");
    let scan = child.id();

    let start = Instant::now();
    let helper = loop {
        if let Some(helper) = helpers_watching(scan).into_iter().next() {
            break helper;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "the scan started no caffeinate for its own pid"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    send(&child, libc::SIGKILL);
    child.wait().expect("collect the scan");
    assert_released(helper, "the scan was killed");
    assert!(helpers_watching(scan).is_empty());
}
