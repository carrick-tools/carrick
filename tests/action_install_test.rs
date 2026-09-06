//! The Action's dependency-install step (carrick#706).
//!
//! `scripts/install-scanned-deps.sh` is what `action.yml` runs before "Run
//! analysis". These cases drive it directly on the three fixtures under
//! `tests/fixtures/action-install`, so the posture is proven without a runner:
//! no lockfile skips, a lockfile installs with lifecycle scripts disabled, and
//! a lockfile that cannot install degrades to a `::warning::` while still
//! exiting 0 so the scan continues.
//!
//! Every case copies its fixture to a scratch directory first: `npm ci` writes
//! `node_modules`, and a fixture tree that gains one stops being an answer key.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    repo_root().join("scripts/install-scanned-deps.sh")
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create scratch dir");
    for entry in fs::read_dir(from).expect("read fixture dir") {
        let entry = entry.expect("fixture entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy fixture file");
        }
    }
}

/// A fixture copied into a fresh scratch directory, removed on drop.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn of(fixture: &str) -> Self {
        // Cases run in parallel and several share a fixture, so the pid alone
        // would have two of them installing into one directory.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "carrick-action-install-{}-{}-{}",
            fixture,
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        copy_tree(
            &repo_root()
                .join("tests/fixtures/action-install")
                .join(fixture),
            &dir,
        );
        Self { dir }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

struct Run {
    stdout: String,
    status: i32,
}

fn run(args: &[&str]) -> Run {
    let output = Command::new("bash")
        .arg(script())
        .args(args)
        .output()
        .expect("run install-scanned-deps.sh");
    Run {
        stdout: format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        status: output.status.code().unwrap_or(-1),
    }
}

fn detect(dir: &Path) -> Run {
    run(&["detect", dir.to_str().expect("utf-8 path")])
}

fn install(dir: &Path, manager: &str) -> Run {
    run(&["install", dir.to_str().expect("utf-8 path"), manager])
}

#[test]
fn a_root_without_a_lockfile_installs_nothing() {
    let scratch = Scratch::of("no-lockfile");
    let detected = detect(&scratch.dir);

    assert_eq!(detected.status, 0, "detect exits 0: {}", detected.stdout);
    assert!(
        detected.stdout.contains("should_install=false"),
        "no lockfile, no install: {}",
        detected.stdout
    );
    assert!(
        detected
            .stdout
            .contains("reason=no lockfile at the scan root"),
        "the skip says why: {}",
        detected.stdout
    );
    assert!(
        !scratch.dir.join("node_modules").exists(),
        "nothing was installed"
    );
}

#[test]
fn a_lockfile_names_its_manager_and_the_key_the_cache_is_keyed_on() {
    let scratch = Scratch::of("npm-lockfile");
    let detected = detect(&scratch.dir);

    assert!(
        detected.stdout.contains("should_install=true"),
        "a lockfile at the root installs: {}",
        detected.stdout
    );
    assert!(
        detected.stdout.contains("manager=npm"),
        "package-lock.json names npm: {}",
        detected.stdout
    );
    let hash = detected
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("lockfile_sha256="))
        .expect("a lockfile hash for the cache key");
    assert_eq!(hash.len(), 64, "sha256 of the lockfile, got {hash:?}");
}

#[test]
fn an_installed_root_is_left_alone() {
    let scratch = Scratch::of("npm-lockfile");
    fs::create_dir_all(scratch.dir.join("node_modules")).expect("pre-installed tree");

    let detected = detect(&scratch.dir);
    assert!(
        detected.stdout.contains("should_install=false"),
        "a workflow that installed already is not installed over: {}",
        detected.stdout
    );
    assert!(
        detected
            .stdout
            .contains("reason=node_modules already present at the scan root"),
        "the skip says why: {}",
        detected.stdout
    );
}

/// The install itself. `local-lib` is a `file:` dependency, so `npm ci`
/// resolves it from the fixture with no network, and its `postinstall` is the
/// witness that lifecycle scripts stayed off.
#[test]
fn an_install_runs_with_lifecycle_scripts_disabled() {
    let scratch = Scratch::of("npm-lockfile");
    let installed = install(&scratch.dir, "npm");

    assert_eq!(installed.status, 0, "install exits 0: {}", installed.stdout);
    assert!(
        !installed.stdout.contains("::warning::"),
        "a clean install warns about nothing: {}",
        installed.stdout
    );
    assert!(
        scratch.dir.join("node_modules/local-lib").exists(),
        "the dependency is on disk, so the type layer is not bare: {}",
        installed.stdout
    );
    assert!(
        !scratch.dir.join("local-lib/postinstall-ran").exists(),
        "nothing in the scanned repo executed during the install"
    );
}

/// A lockfile out of sync with its manifest: `npm ci` refuses, and the scan
/// still has to happen.
#[test]
fn a_failing_install_warns_and_lets_the_scan_continue() {
    let scratch = Scratch::of("broken-lockfile");
    let installed = install(&scratch.dir, "npm");

    assert_eq!(
        installed.status, 0,
        "a failed install is not a failed scan: {}",
        installed.stdout
    );
    assert!(
        installed.stdout.contains("::warning::"),
        "the degradation is announced: {}",
        installed.stdout
    );
    assert!(
        installed.stdout.contains("bare checkout"),
        "the warning says what the scan loses: {}",
        installed.stdout
    );
    assert!(
        !scratch.dir.join("node_modules").exists(),
        "nothing half-installed was left behind"
    );
}

/// A manager the script has no command for is a skip with a warning, never a
/// crash: the Action's step must not turn an unknown lockfile into a red run.
#[test]
fn an_unknown_manager_warns_and_exits_zero() {
    let scratch = Scratch::of("no-lockfile");
    let installed = install(&scratch.dir, "cargo");

    assert_eq!(installed.status, 0, "{}", installed.stdout);
    assert!(
        installed.stdout.contains("::warning::"),
        "the skip is announced: {}",
        installed.stdout
    );
}
