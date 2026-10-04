//! The Action's dependency-install step (carrick#706).
//!
//! `scripts/install-scanned-deps.sh` is what `action.yml` runs before "Run
//! analysis". These cases drive it directly on the fixtures under
//! `tests/fixtures/action-install`, so the posture is proven without a runner:
//! no lockfile skips, a lockfile installs with lifecycle scripts disabled, and
//! a lockfile that cannot install degrades to a `::warning::` while still
//! exiting 0 so the scan continues.
//!
//! WHICH roots it prepares is the other half (carrick#1312), and the three
//! shapes that differ are here: one lockfile at the root, a service carrying
//! its own lockfile below one, and a workspace whose members hoist to a single
//! lockfile. The set has to be the set the pre-flight checks, so the script
//! asks the real binary (`carrick derive`) for this repo's services and these
//! cases run it — a fixture that agreed with a second derivation written in
//! the test would prove nothing about the scan that follows.
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
    /// What `detect` writes as step outputs, and nothing else: the runner
    /// fails a step for a GITHUB_OUTPUT line that is not `key=value`.
    out: String,
    err: String,
    status: i32,
}

impl Run {
    /// Both streams, for an assertion about what the step SAID rather than
    /// about what it wrote to `$GITHUB_OUTPUT`.
    fn text(&self) -> String {
        format!("{}{}", self.out, self.err)
    }
}

/// The Carrick the script asks for this repository's services. The binary
/// built from this checkout, so a derivation change and this test move
/// together.
fn carrick_cli() -> &'static str {
    env!("CARGO_BIN_EXE_carrick")
}

fn run_with(args: &[&str], cli: Option<&str>) -> Run {
    let mut command = Command::new("bash");
    command.arg(script()).args(args);
    match cli {
        Some(path) => command.env("CARRICK_CLI", path),
        // An inherited one would decide a case that is about not having it.
        None => command.env_remove("CARRICK_CLI"),
    };
    let output = command.output().expect("run install-scanned-deps.sh");
    Run {
        out: String::from_utf8_lossy(&output.stdout).into_owned(),
        err: String::from_utf8_lossy(&output.stderr).into_owned(),
        status: output.status.code().unwrap_or(-1),
    }
}

fn run(args: &[&str]) -> Run {
    run_with(args, Some(carrick_cli()))
}

fn detect(dir: &Path) -> Run {
    run(&["detect", dir.to_str().expect("utf-8 path")])
}

/// One root with the manager named, which is how the posture cases drive an
/// install they have to choose the manager for.
fn install_one(dir: &Path, manager: &str) -> Run {
    run(&["install-one", dir.to_str().expect("utf-8 path"), manager])
}

/// Every root this checkout needs, which is what `action.yml` calls.
fn install(dir: &Path) -> Run {
    run(&["install", dir.to_str().expect("utf-8 path")])
}

#[test]
fn a_root_without_a_lockfile_installs_nothing() {
    let scratch = Scratch::of("no-lockfile");
    let detected = detect(&scratch.dir);

    assert_eq!(detected.status, 0, "detect exits 0: {}", detected.text());
    assert!(
        detected.text().contains("should_install=false"),
        "no lockfile, no install: {}",
        detected.text()
    );
    assert!(
        detected
            .out
            .contains("reason=no service of this repository has a lockfile that is not installed"),
        "the skip says why: {}",
        detected.text()
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
        detected.text().contains("should_install=true"),
        "a lockfile at the root installs: {}",
        detected.text()
    );
    assert!(
        detected.text().contains("managers=npm"),
        "package-lock.json names npm: {}",
        detected.text()
    );
    let hash = detected
        .out
        .lines()
        .find_map(|line| line.strip_prefix("lockfiles_sha256="))
        .expect("a lockfile hash for the cache key");
    assert_eq!(hash.len(), 64, "sha256 of the lockfile, got {hash:?}");
}

#[test]
fn an_installed_root_is_left_alone() {
    let scratch = Scratch::of("npm-lockfile");
    fs::create_dir_all(scratch.dir.join("node_modules")).expect("pre-installed tree");

    let detected = detect(&scratch.dir);
    assert!(
        detected.text().contains("should_install=false"),
        "a workflow that installed already is not installed over: {}",
        detected.text()
    );
    assert!(
        detected
            .out
            .contains("reason=no service of this repository has a lockfile that is not installed"),
        "the skip says why: {}",
        detected.text()
    );
}

/// The install itself. `local-lib` is a `file:` dependency, so `npm ci`
/// resolves it from the fixture with no network, and its `postinstall` is the
/// witness that lifecycle scripts stayed off.
#[test]
fn an_install_runs_with_lifecycle_scripts_disabled() {
    let scratch = Scratch::of("npm-lockfile");
    let installed = install_one(&scratch.dir, "npm");

    assert_eq!(installed.status, 0, "install exits 0: {}", installed.text());
    assert!(
        !installed.text().contains("::warning::"),
        "a clean install warns about nothing: {}",
        installed.text()
    );
    assert!(
        scratch.dir.join("node_modules/local-lib").exists(),
        "the dependency is on disk, so the type layer is not bare: {}",
        installed.text()
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
    let installed = install_one(&scratch.dir, "npm");

    assert_eq!(
        installed.status,
        0,
        "a failed install is not a failed scan: {}",
        installed.text()
    );
    assert!(
        installed.text().contains("::warning::"),
        "the degradation is announced: {}",
        installed.text()
    );
    assert!(
        installed.text().contains("refuse that service"),
        "the warning says what the scan does with it: {}",
        installed.text()
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
    let installed = install_one(&scratch.dir, "cargo");

    assert_eq!(installed.status, 0, "{}", installed.text());
    assert!(
        installed.text().contains("::warning::"),
        "the skip is announced: {}",
        installed.text()
    );
}

#[test]
fn deno_preparation_does_not_skip_existing_node_modules() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("deno.jsonc"), "{}").unwrap();
    fs::write(dir.path().join("deno.lock"), "{}").unwrap();
    fs::create_dir(dir.path().join("node_modules")).unwrap();
    let result = detect(dir.path());
    assert_eq!(result.status, 0);
    assert!(result.text().contains("managers=deno"), "{}", result.text());
    assert!(
        result.text().contains("should_install=true"),
        "{}",
        result.text()
    );
}

#[test]
fn deno_preparation_never_runs_declared_tasks_or_application_code() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("deno.json"),
        r#"{
      "nodeModulesDir":"auto", "allowScripts":["npm:fixture-dependency"],
      "tasks":{"install":"touch APPLICATION_RAN"},
      "imports":{"local":"./mod.ts"}
    }"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("mod.ts"),
        "Deno.writeTextFileSync('APPLICATION_RAN', 'bad'); export const value = 1;",
    )
    .unwrap();
    // Runtime installed by CI; missing runtime is an actual failed check.
    assert!(
        Command::new("deno")
            .arg("--version")
            .output()
            .expect("Deno must be installed for this integration test")
            .status
            .success()
    );
    let result = install_one(dir.path(), "deno");
    assert_eq!(result.status, 0);
    assert!(result.text().contains("Installed"), "{}", result.text());
    assert!(!dir.path().join("APPLICATION_RAN").exists());
    assert!(!dir.path().join("node_modules").exists());
    assert!(
        !dir.path().join("deno.lock").exists(),
        "frozen prep must not create a lockfile"
    );
}

#[test]
fn deno_action_suppresses_config_authorized_npm_lifecycle_scripts() {
    let output = Command::new("python3")
        .arg(repo_root().join("tests/fixtures/action-install/deno-registry.py"))
        .arg(script())
        .output()
        .expect("run local registry lifecycle fixture");
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A service carrying its own lockfile below one is prepared too
/// (carrick#1312). This is the live regression: the pre-flight refuses per
/// service, and an installer that prepared the scan root alone left every
/// nested service refused for dependencies the Action said it had installed.
///
/// The fixture is the shape that broke — `carrick.json` naming services in
/// their own directories, one of which carries its own `package-lock.json`
/// and one of which does not — so the set is two: the repository root, and
/// the service with a lockfile of its own. The hoisted service has none and
/// is prepared by the root's.
#[test]
fn a_service_with_its_own_lockfile_is_prepared_as_well_as_the_root() {
    let scratch = Scratch::of("nested-services");
    let detected = detect(&scratch.dir);

    assert!(
        detected.out.contains("should_install=true"),
        "two lockfiles state two installs: {}",
        detected.text()
    );
    assert!(
        detected.out.contains("roots=2"),
        "the root and the service that carries its own lockfile: {}",
        detected.text()
    );
    assert!(
        detected
            .err
            .contains("Carrick prepares services/api with npm"),
        "the log names the nested root it prepares: {}",
        detected.text()
    );

    let installed = install(&scratch.dir);
    assert_eq!(installed.status, 0, "install exits 0: {}", installed.text());
    assert!(
        scratch.dir.join("node_modules/root-lib").exists(),
        "the root was installed: {}",
        installed.text()
    );
    assert!(
        scratch
            .dir
            .join("services/api/node_modules/api-lib")
            .exists(),
        "the nested service was installed, so the pre-flight does not refuse it: {}",
        installed.text()
    );
}

/// A workspace that hoists is installed once, at the lockfile its members
/// reach by walking up — not once per member.
///
/// Detection only: what is under test is WHICH root the install runs in, and
/// the fixture states a pnpm workspace whose members depend on each other, so
/// running pnpm here would prove nothing the plan does not already say.
#[test]
fn a_workspace_that_hoists_to_one_lockfile_is_installed_once() {
    let scratch = Scratch::of("pnpm-workspace");
    // Both members are services, so the single install below is a fold of two
    // service roots onto one lockfile and not an empty derivation.
    let derived = Command::new(carrick_cli())
        .args([
            "derive",
            "--workspace",
            scratch.dir.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("carrick derive");
    let derived = String::from_utf8_lossy(&derived.stdout).into_owned();
    assert!(
        derived.contains("packages/a") && derived.contains("packages/b"),
        "the workspace members are the services: {derived}"
    );

    let detected = detect(&scratch.dir);

    assert!(
        detected.out.contains("roots=1"),
        "two members, one lockfile, one install: {}",
        detected.text()
    );
    assert!(
        detected.out.contains("managers=pnpm"),
        "pnpm-lock.yaml names pnpm: {}",
        detected.text()
    );
    assert!(
        detected
            .err
            .contains("Carrick prepares the repository root with pnpm"),
        "the one install runs at the workspace root: {}",
        detected.text()
    );
    assert!(
        !detected.err.contains("packages/"),
        "no member is prepared on its own: {}",
        detected.text()
    );
}

/// A repository whose only lockfile is at its root is installed exactly once,
/// as it was before the set grew past the scan root.
#[test]
fn a_single_lockfile_at_the_root_is_one_install() {
    let scratch = Scratch::of("npm-lockfile");
    let detected = detect(&scratch.dir);

    assert!(
        detected.out.contains("roots=1"),
        "one lockfile, one install: {}",
        detected.text()
    );
}

/// Everything `detect` writes to stdout is a step output, whatever happens to
/// the derivation.
///
/// The runner fails a step for any `$GITHUB_OUTPUT` line that is not
/// `key=value` or inside a heredoc block, so a warning printed on the path
/// where the services cannot be derived would turn a degradation the script
/// is written to survive into a red Action.
#[test]
fn a_derivation_that_fails_still_writes_only_step_outputs() {
    let scratch = Scratch::of("nested-services");
    let detected = run_with(
        &["detect", scratch.dir.to_str().expect("utf-8 path")],
        Some("/nonexistent/carrick"),
    );

    assert_eq!(detected.status, 0, "detect exits 0: {}", detected.text());
    assert!(
        detected.err.contains("::warning::"),
        "the degradation is announced, on stderr: {}",
        detected.text()
    );
    let mut inside_heredoc = false;
    for line in detected.out.lines() {
        if let Some(delimiter) = line.split_once("<<").map(|(_, end)| end) {
            inside_heredoc = true;
            assert!(!delimiter.is_empty(), "a heredoc output names its end");
            continue;
        }
        if inside_heredoc {
            inside_heredoc = !line.starts_with("CARRICK_");
            continue;
        }
        assert!(
            line.contains('='),
            "every line of a step output is key=value, got {line:?} in {}",
            detected.out
        );
    }
}

fn write(root: &Path, file: &str, body: &str) {
    let target = root.join(file);
    fs::create_dir_all(target.parent().expect("file parent")).expect("parent dir");
    fs::write(target, body).expect("write file");
}

/// A package that states an install: a manifest declaring a dependency, its
/// lockfile, and source.
fn package_with_lockfile(root: &Path, directory: &str, name: &str) {
    write(
        root,
        &format!("{directory}/package.json"),
        &format!(r#"{{"name":"{name}","dependencies":{{"left-pad":"1.3.0"}}}}"#),
    );
    write(root, &format!("{directory}/package-lock.json"), "{}");
    write(
        root,
        &format!("{directory}/src/index.ts"),
        "export const port = 3000;\n",
    );
}

/// The roots the installer says it prepares, as `detect` logs them, relative
/// to the scan root with the root itself as `.`.
fn prepared_roots(detected: &Run) -> Vec<String> {
    let mut roots: Vec<String> = detected
        .err
        .lines()
        .filter_map(|line| line.strip_prefix("Carrick prepares "))
        .filter_map(|rest| rest.rsplit_once(" with "))
        .map(|(root, _)| match root {
            "the repository root" => ".".to_string(),
            other => other.to_string(),
        })
        .collect();
    roots.sort();
    roots
}

/// The roots a scan of `root` would refuse as uninstalled: the pre-flight's
/// own answer for the services the scan derives.
fn refused_roots(root: &Path) -> Vec<String> {
    let services = carrick::service_derivation::resolve(root)
        .expect("the scan derives services")
        .services;
    let mut roots: Vec<String> = carrick::preflight::unprepared(root, &services)
        .into_iter()
        .filter_map(|row| match row {
            carrick::preflight::Unprepared::Dependencies { install_root, .. } => {
                Some(if install_root.is_empty() {
                    ".".to_string()
                } else {
                    install_root
                })
            }
            carrick::preflight::Unprepared::Mapping { .. } => None,
        })
        .collect();
    roots.sort();
    roots.dedup();
    roots
}

/// carrick#1858. The installer asks `carrick derive --workspace <scan root>`
/// for the services to prepare, and the scan derives its own from the same
/// root. At the root of one git repository whose packages sit in directories
/// below it, the command answered for a folder of repositories (each package
/// directory a repository of its own) and the scan for one repository, so the
/// two sets differed: the installer prepared what the scan never checked, and
/// would not prepare a package the scan derived deeper down.
///
/// Asks both, on one tree, and they have to agree: what the installer
/// prepares is what the pre-flight would otherwise refuse.
#[test]
fn the_installer_prepares_the_roots_the_scan_would_refuse_at_a_git_root() {
    let scratch = tempfile::tempdir().expect("scratch dir");
    let root = scratch.path().canonicalize().expect("canonical scratch");
    fs::create_dir(root.join(".git")).expect("a git checkout");
    package_with_lockfile(&root, ".", "shop");
    package_with_lockfile(&root, "web", "web");
    package_with_lockfile(&root, "tools/sync", "sync");

    let detected = detect(&root);
    assert_eq!(detected.status, 0, "detect exits 0: {}", detected.text());
    assert_eq!(
        prepared_roots(&detected),
        refused_roots(&root),
        "the installer and the scan name different roots: {}",
        detected.text()
    );
}
