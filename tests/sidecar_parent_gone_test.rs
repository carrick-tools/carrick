//! A sidecar ends when the process that started it is gone, and only then
//! (carrick#2029).
//!
//! A sidecar already ends when its stdin closes, and the death of the process
//! that started it closes it, unless another process still holds the write
//! end. Then nothing reaches the sidecar but the change of its parent. So the
//! parent here hands every descriptor it has to a process that outlives it
//! before it is killed, and the sidecars must still end.
//!
//! The parent is this test binary run again as [`parent`], so that it can be
//! killed without killing the test. It starts its sidecars from several
//! threads at once, the way a pool and parallel tests start them.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Barrier;
use std::time::{Duration, Instant};

use carrick::services::TypeSidecar;

/// Set to a file path, it makes [`parent`] the parent, writing to that file.
const PARENT_ENV: &str = "CARRICK_TEST_SIDECAR_PARENT";

/// How many sidecars a parent starts, all at once.
const SIDECARS: usize = 4;

/// The sidecar looks at its parent once a second. This leaves room for a
/// loaded machine, and is far short of "never".
const BOUND: Duration = Duration::from_secs(10);

/// For anything that has to happen before the bound is measured.
const SETUP: Duration = Duration::from_secs(120);

fn sidecar_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js")
}

fn sidecar_built() -> bool {
    sidecar_script().exists()
}

/// `SIDECARS` sidecars started from as many threads at once, each ready.
fn start_sidecars(root: &Path) -> Vec<TypeSidecar> {
    let script = sidecar_script();
    let barrier = Barrier::new(SIDECARS);
    let sidecars: Vec<TypeSidecar> = std::thread::scope(|scope| {
        let started: Vec<_> = (0..SIDECARS)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    TypeSidecar::spawn(&script).expect("start a sidecar")
                })
            })
            .collect();
        started
            .into_iter()
            .map(|thread| thread.join().expect("a starting thread"))
            .collect()
    });
    for sidecar in &sidecars {
        sidecar.start_init(root, None);
        sidecar.wait_ready(SETUP).expect("a sidecar becomes ready");
    }
    sidecars
}

/// Whether `pid` is a running process. A zombie has ended, and reads as gone.
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

/// The parent's part, run only as the process the tests below start and kill:
/// it starts the sidecars, then a process that holds every descriptor it has,
/// each sidecar's stdin among them, and writes down all their pids.
#[test]
#[ignore = "the parent the other tests in this file start and kill"]
fn parent() {
    let Ok(report) = std::env::var(PARENT_ENV) else {
        return;
    };
    let root = tempfile::tempdir().expect("a root for the sidecars");
    let sidecars = start_sidecars(root.path());

    // Every descriptor made inheritable, so the next process started holds
    // the write end of each sidecar's stdin for as long as it runs. This
    // process has a few dozen; a number with none behind it is refused.
    for fd in 3..1024 {
        // SAFETY: clearing a flag on a descriptor of this process.
        unsafe { libc::fcntl(fd, libc::F_SETFD, 0) };
    }
    let mut holder = Command::new("sleep")
        .arg("600")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the holder");

    let mut pids = vec![holder.id().to_string()];
    pids.extend(sidecars.iter().map(|sidecar| sidecar.pid().to_string()));
    let partial = PathBuf::from(format!("{report}.partial"));
    std::fs::write(&partial, pids.join("\n")).expect("write the pids");
    std::fs::rename(&partial, &report).expect("publish the pids");

    // The test kills this process long before the holder ends.
    let _ = holder.wait();
}

/// Kills every process it names when dropped, so a failing test leaves none.
struct Reap(Vec<i32>);

impl Drop for Reap {
    fn drop(&mut self) {
        for &pid in &self.0 {
            // SAFETY: a process this test started, directly or through its parent.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

/// Start a parent, end it with `signal`, and return how long its sidecars
/// took to end, or fail naming the ones still running at the bound.
fn sidecars_after_parent_ended_by(signal: libc::c_int) -> Option<Duration> {
    if !sidecar_built() {
        eprintln!("Skipping: sidecar not built (cd src/sidecar && npm run build)");
        return None;
    }
    let dir = tempfile::tempdir().expect("a directory for the test");
    let report = dir.path().join("pids");
    let mut parent: Child = Command::new(std::env::current_exe().expect("this test binary"))
        .args(["parent", "--exact", "--ignored", "--nocapture"])
        .env(PARENT_ENV, &report)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the parent");
    let mut reap = Reap(vec![parent.id() as i32]);

    let start = Instant::now();
    while !report.exists() {
        if let Some(status) = parent.try_wait().expect("poll the parent") {
            panic!("the parent ended before its sidecars were up: {status}");
        }
        assert!(
            start.elapsed() < SETUP,
            "the parent never started its sidecars"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let pids: Vec<i32> = std::fs::read_to_string(&report)
        .expect("read the pids")
        .lines()
        .map(|line| line.parse().expect("a pid"))
        .collect();
    reap.0.extend(&pids);
    let (holder, sidecars) = (pids[0], &pids[1..]);
    assert_eq!(sidecars.len(), SIDECARS);
    assert!(
        sidecars.iter().all(|&pid| alive(pid)),
        "every sidecar runs while its parent does"
    );

    // SAFETY: a signal to a child this test spawned and has not reaped.
    assert_eq!(unsafe { libc::kill(parent.id() as libc::pid_t, signal) }, 0);
    parent.wait().expect("collect the parent");
    let ended = Instant::now();

    loop {
        let running: Vec<i32> = sidecars.iter().copied().filter(|&pid| alive(pid)).collect();
        if running.is_empty() {
            break;
        }
        assert!(
            ended.elapsed() < BOUND,
            "sidecar(s) {running:?} were still running {:.0}s after their parent ended",
            ended.elapsed().as_secs_f64()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let took = ended.elapsed();
    assert!(
        alive(holder),
        "the holder of their stdin went first, so this did not test a stdin that stays open"
    );
    Some(took)
}

#[test]
fn every_sidecar_ends_when_its_parent_is_killed() {
    if let Some(took) = sidecars_after_parent_ended_by(libc::SIGKILL) {
        eprintln!("the sidecars ended {took:?} after their parent was killed");
    }
}

#[test]
fn every_sidecar_ends_when_its_parent_is_interrupted() {
    if let Some(took) = sidecars_after_parent_ended_by(libc::SIGINT) {
        eprintln!("the sidecars ended {took:?} after their parent was interrupted");
    }
}

/// A parent that is alive but writes nothing for several of the sidecar's
/// looks at it keeps every sidecar, and each one still answers.
#[test]
fn a_parent_that_writes_nothing_for_a_while_keeps_its_sidecars() {
    if !sidecar_built() {
        eprintln!("Skipping: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let root = tempfile::tempdir().expect("a root for the sidecars");
    let sidecars = start_sidecars(root.path());

    std::thread::sleep(Duration::from_secs(5));

    for sidecar in &sidecars {
        assert!(
            alive(sidecar.pid() as i32),
            "a sidecar ended under a live parent"
        );
        sidecar.start_init(root.path(), None);
        sidecar
            .wait_ready(SETUP)
            .expect("a sidecar still answers after its parent was quiet");
    }
}
