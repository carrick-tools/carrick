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

// ---------------------------------------------------------------------------
// carrick#2211: CARRICK_INSTALL_UNTRUSTED=1.
//
// A machine that scans repositories it does not own cannot let a file from the
// tree run, and `--ignore-scripts` alone leaves three ways in: a pnpm
// `.pnpmfile.cjs`, a Yarn `yarnPath` / `yarn-path` release, and Yarn Berry
// `plugins`. Each case below generates a tree with one such vector whose code
// writes a marker file OUTSIDE the tree, installs it both ways, and reads the
// marker. The default-mode half proves the case can see the vector at all;
// the untrusted half is the claim. Nothing here is third-party code.
// ---------------------------------------------------------------------------

/// A git repository the installer is pointed at, and a marker directory beside
/// it that the repository's code writes to when it runs.
struct Untrusted {
    _scratch: tempfile::TempDir,
    repo: PathBuf,
    markers: PathBuf,
}

fn have(tool: &str) -> bool {
    Command::new("bash")
        .args(["-c", &format!("command -v {tool}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn sh(dir: &Path, command: &str) {
    let output = Command::new("bash")
        .args(["-c", command])
        .current_dir(dir)
        .env("COREPACK_ENABLE_AUTO_PIN", "0")
        .output()
        .expect("run shell");
    assert!(
        output.status.success(),
        "`{command}` in {} failed: {}{}",
        dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

impl Untrusted {
    fn new() -> Self {
        let scratch = tempfile::tempdir().expect("scratch dir");
        let base = scratch.path().canonicalize().expect("canonical scratch");
        let repo = base.join("repo");
        let markers = base.join("markers");
        fs::create_dir_all(&repo).expect("repo dir");
        fs::create_dir_all(&markers).expect("marker dir");
        Self {
            _scratch: scratch,
            repo,
            markers,
        }
    }

    fn marker(&self, name: &str) -> PathBuf {
        self.markers.join(name)
    }

    /// JavaScript that records that it ran.
    fn witness(&self, name: &str) -> String {
        format!(
            "require('fs').writeFileSync({:?}, 'ran');\n",
            self.marker(name).to_str().expect("utf-8 path")
        )
    }

    /// Commit the tree as it stands, so `git status` afterwards names every
    /// tracked file the install changed.
    fn commit(&self) {
        sh(
            &self.repo,
            "printf 'node_modules\\n' > .gitignore && git init -q . \
             && git add -A && git -c user.email=t@t -c user.name=t commit -q -m tree",
        );
    }

    fn changed_tracked_files(&self) -> String {
        let output = Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=no"])
            .current_dir(&self.repo)
            .output()
            .expect("git status");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn install_one(&self, root: &str, manager: &str, untrusted: bool) -> Run {
        self.install_one_with(root, manager, untrusted, false)
    }

    /// `retry` sets CARRICK_INSTALL_LOCKFILE_RETRY=1, the hosted runner's lane
    /// policy (carrick#3069).
    fn install_one_with(&self, root: &str, manager: &str, untrusted: bool, retry: bool) -> Run {
        let dir = self.repo.join(root);
        let mut command = Command::new("bash");
        command
            .arg(script())
            .args(["install-one", dir.to_str().expect("utf-8 path"), manager])
            .env_remove("CARRICK_CLI")
            .env_remove("CARRICK_INSTALL_UNTRUSTED")
            .env_remove("CARRICK_INSTALL_LOCKFILE_RETRY");
        if retry {
            command.env("CARRICK_INSTALL_LOCKFILE_RETRY", "1");
        }
        // Untrusted mode asks the scanner to read Yarn's rc files, as the
        // hosted runner does through its own CARRICK_CLI (carrick#2246).
        if untrusted {
            command
                .env("CARRICK_INSTALL_UNTRUSTED", "1")
                .env("CARRICK_CLI", carrick_cli());
        }
        let output = command.output().expect("run install-scanned-deps.sh");
        Run {
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            err: String::from_utf8_lossy(&output.stderr).into_owned(),
            status: output.status.code().unwrap_or(-1),
        }
    }

    /// The claim common to every vector: the script exited 0, the root is
    /// either installed or named in a warning, and no tracked file changed.
    fn assert_safe(&self, root: &str, run: &Run) {
        assert_eq!(run.status, 0, "install exits 0: {}", run.text());
        let dir = self.repo.join(root);
        let dir_text = dir.to_str().expect("utf-8 path");
        let named = run.text().contains("::warning::") && run.text().contains(dir_text);
        let installed = run
            .text()
            .contains(&format!("Installed {dir_text} dependencies"));
        assert!(
            installed || named,
            "{root} is neither installed nor named in a warning: {}",
            run.text()
        );
        assert_eq!(
            self.changed_tracked_files(),
            "",
            "the install rewrote a tracked file: {}",
            run.text()
        );
    }
}

const EMPTY_PNPM_LOCK: &str = "lockfileVersion: '9.0'\n\nsettings:\n  autoInstallPeers: true\n  excludeLinksFromLockfile: false\n\nimporters:\n\n  .: {}\n";

#[test]
fn untrusted_mode_never_loads_a_pnpmfile() {
    if !have("corepack") {
        eprintln!("skipped: corepack is not on PATH");
        return;
    }
    let build = |t: &Untrusted| {
        write(
            &t.repo,
            "pnpm/package.json",
            r#"{"name":"p","private":true}"#,
        );
        write(&t.repo, "pnpm/pnpm-lock.yaml", EMPTY_PNPM_LOCK);
        write(&t.repo, "pnpm/.pnpmfile.cjs", &t.witness("pnpmfile"));
        t.commit();
    };

    let default = Untrusted::new();
    build(&default);
    let run = default.install_one("pnpm", "pnpm", false);
    assert!(
        default.marker("pnpmfile").exists(),
        "default mode runs the pnpmfile, so this case can see it: {}",
        run.text()
    );

    let untrusted = Untrusted::new();
    build(&untrusted);
    let run = untrusted.install_one("pnpm", "pnpm", true);
    assert!(
        !untrusted.marker("pnpmfile").exists(),
        "the pnpmfile ran in untrusted mode: {}",
        run.text()
    );
    untrusted.assert_safe("pnpm", &run);
}

#[test]
fn untrusted_mode_never_hands_a_yarn_install_to_a_committed_release() {
    if !have("yarn") {
        eprintln!("skipped: yarn is not on PATH");
        return;
    }
    // Classic: `.yarnrc` `yarn-path`.
    let classic = |t: &Untrusted| {
        write(
            &t.repo,
            "classic/package.json",
            r#"{"name":"c","private":true}"#,
        );
        write(&t.repo, "classic/yarn.lock", "# yarn lockfile v1\n\n\n");
        write(
            &t.repo,
            "classic/.yarnrc",
            "yarn-path \".yarn/release.cjs\"\n",
        );
        write(
            &t.repo,
            "classic/.yarn/release.cjs",
            &t.witness("yarn-path"),
        );
        t.commit();
    };
    // Berry: `.yarnrc.yml` `yarnPath`, with no `packageManager` pin.
    let berry = |t: &Untrusted| {
        write(
            &t.repo,
            "berry/package.json",
            r#"{"name":"b","private":true}"#,
        );
        write(&t.repo, "berry/yarn.lock", "__metadata:\n  version: 8\n");
        write(
            &t.repo,
            "berry/.yarnrc.yml",
            "yarnPath: .yarn/release.cjs\n",
        );
        write(&t.repo, "berry/.yarn/release.cjs", &t.witness("yarnPath"));
        t.commit();
    };

    for (root, marker, build) in [
        ("classic", "yarn-path", &classic as &dyn Fn(&Untrusted)),
        ("berry", "yarnPath", &berry as &dyn Fn(&Untrusted)),
    ] {
        let default = Untrusted::new();
        build(&default);
        let run = default.install_one(root, "yarn", false);
        assert!(
            default.marker(marker).exists(),
            "default mode runs the committed {root} release, so this case can see it: {}",
            run.text()
        );

        let untrusted = Untrusted::new();
        build(&untrusted);
        let run = untrusted.install_one(root, "yarn", true);
        assert!(
            !untrusted.marker(marker).exists(),
            "the committed {root} release ran in untrusted mode: {}",
            run.text()
        );
        untrusted.assert_safe(root, &run);
    }
}

#[test]
fn untrusted_mode_refuses_a_berry_root_it_cannot_install_without_the_trees_code() {
    if !have("yarn") {
        eprintln!("skipped: yarn is not on PATH");
        return;
    }
    let t = Untrusted::new();
    write(
        &t.repo,
        "plugin/package.json",
        r#"{"name":"p","private":true}"#,
    );
    write(&t.repo, "plugin/yarn.lock", "__metadata:\n  version: 8\n");
    write(
        &t.repo,
        "plugin/.yarnrc.yml",
        "plugins:\n  - path: .yarn/plugins/p.cjs\n",
    );
    write(&t.repo, "plugin/.yarn/plugins/p.cjs", &t.witness("plugin"));
    t.commit();

    let run = t.install_one("plugin", "yarn", true);
    assert!(
        !t.marker("plugin").exists(),
        "the plugin ran: {}",
        run.text()
    );
    assert!(
        run.text().contains("::warning::") && run.text().contains("lists plugins"),
        "a root listing plugins is named in a warning: {}",
        run.text()
    );
    assert!(
        !t.repo.join("plugin/node_modules").exists(),
        "a refused root is not installed"
    );
    t.assert_safe("plugin", &run);

    // A yarnPath with no packageManager pin is refused; with a pin it is not.
    let t = Untrusted::new();
    write(
        &t.repo,
        "pinned/package.json",
        r#"{"name":"b","packageManager":"yarn@4.9.2"}"#,
    );
    write(&t.repo, "pinned/yarn.lock", "__metadata:\n  version: 8\n");
    write(
        &t.repo,
        "pinned/.yarnrc.yml",
        "yarnPath: .yarn/release.cjs\n",
    );
    write(&t.repo, "pinned/.yarn/release.cjs", &t.witness("pinned"));
    write(&t.repo, "unpinned/package.json", r#"{"name":"b"}"#);
    write(&t.repo, "unpinned/yarn.lock", "__metadata:\n  version: 8\n");
    write(
        &t.repo,
        "unpinned/.yarnrc.yml",
        "yarnPath: .yarn/release.cjs\n",
    );
    t.commit();
    let unpinned = t.install_one("unpinned", "yarn", true);
    assert!(
        unpinned.text().contains("pins a packageManager"),
        "an unpinned yarnPath is named in a warning: {}",
        unpinned.text()
    );
    let pinned = t.install_one("pinned", "yarn", true);
    assert!(
        !pinned.text().contains("pins a packageManager") && !t.marker("pinned").exists(),
        "a pinned root is not refused for its yarnPath, and the file is not run: {}",
        pinned.text()
    );
}

/// A manifest one entry ahead of its lockfile is what an unfrozen install
/// rewrites. The lockfile is generated by the manager itself from a manifest
/// with one dependency, then the manifest gains a second.
fn ahead_of_its_lockfile(t: &Untrusted, root: &str, generate: &str) {
    for dep in ["a", "b"] {
        write(
            &t.repo,
            &format!("{root}/{dep}/package.json"),
            &format!(r#"{{"name":"{dep}","version":"1.0.0"}}"#),
        );
    }
    let manifest =
        |deps: &str| format!(r#"{{"name":"{root}","private":true,"dependencies":{{{deps}}}}}"#);
    write(
        &t.repo,
        &format!("{root}/package.json"),
        &manifest(r#""a":"file:./a""#),
    );
    sh(&t.repo.join(root), generate);
    let _ = fs::remove_dir_all(t.repo.join(root).join("node_modules"));
    write(
        &t.repo,
        &format!("{root}/package.json"),
        &manifest(r#""a":"file:./a","b":"file:./b""#),
    );
    t.commit();
}

#[test]
fn untrusted_mode_installs_classic_yarn_and_bun_frozen() {
    for (tool, root, generate) in [
        ("yarn", "classic", "yarn install --ignore-scripts"),
        ("bun", "bun", "bun install --ignore-scripts"),
    ] {
        if !have(tool) {
            eprintln!("skipped: {tool} is not on PATH");
            continue;
        }
        let default = Untrusted::new();
        ahead_of_its_lockfile(&default, root, generate);
        default.install_one(root, tool, false);
        assert_ne!(
            default.changed_tracked_files(),
            "",
            "an unfrozen {tool} install rewrites the lockfile, so this case can see it"
        );

        let untrusted = Untrusted::new();
        ahead_of_its_lockfile(&untrusted, root, generate);
        let run = untrusted.install_one(root, tool, true);
        untrusted.assert_safe(root, &run);
    }
}

/// A `file:` dependency whose postinstall writes a marker, under a lockfile
/// the manager itself generated (scripts off, so generating it fires nothing).
fn yarn_tree_with_postinstall(t: &Untrusted, root: &str, pin: Option<&str>, generate: &str) {
    write(
        &t.repo,
        &format!("{root}/dep/package.json"),
        r#"{"name":"dep","version":"1.0.0","scripts":{"postinstall":"node postinstall.cjs"}}"#,
    );
    write(
        &t.repo,
        &format!("{root}/dep/postinstall.cjs"),
        &t.witness("postinstall"),
    );
    let pin = pin
        .map(|p| format!(r#","packageManager":"{p}""#))
        .unwrap_or_default();
    write(
        &t.repo,
        &format!("{root}/package.json"),
        &format!(
            r#"{{"name":"{root}","private":true{pin},"dependencies":{{"dep":"file:./dep"}}}}"#
        ),
    );
    sh(&t.repo.join(root), generate);
    let _ = fs::remove_file(t.marker("postinstall"));
    let _ = fs::remove_dir_all(t.repo.join(root).join("node_modules"));
    t.commit();
}

// Classic Yarn accepts `--mode=skip-build` and ignores it, so trying it first
// ran every dependency's lifecycle script (carrick#2226).
#[test]
fn default_mode_installs_classic_yarn_without_running_dependency_scripts() {
    if !have("yarn") {
        eprintln!("skipped: yarn is not on PATH");
        return;
    }
    let t = Untrusted::new();
    yarn_tree_with_postinstall(&t, "classic", None, "yarn install --ignore-scripts");
    let run = t.install_one("classic", "yarn", false);
    assert_eq!(run.status, 0, "{}", run.text());
    assert!(
        !t.marker("postinstall").exists(),
        "classic Yarn ran a dependency's postinstall in default mode: {}",
        run.text()
    );
    assert!(
        t.repo.join("classic/node_modules/dep").exists(),
        "the install happened: {}",
        run.text()
    );
}

#[test]
fn default_mode_installs_yarn_berry_with_skip_build() {
    if !have("yarn") {
        eprintln!("skipped: yarn is not on PATH");
        return;
    }
    let t = Untrusted::new();
    yarn_tree_with_postinstall(
        &t,
        "berry",
        Some("yarn@4.9.2"),
        "touch yarn.lock && YARN_ENABLE_IMMUTABLE_INSTALLS=0 yarn install --mode=skip-build",
    );
    let run = t.install_one("berry", "yarn", false);
    assert_eq!(run.status, 0, "{}", run.text());
    assert!(
        !t.marker("postinstall").exists(),
        "Berry ran a dependency's postinstall in default mode: {}",
        run.text()
    );
    assert!(
        run.text().contains("Installed"),
        "the install happened: {}",
        run.text()
    );
}

#[test]
fn an_npm_file_dependency_cannot_run_its_preinstall() {
    let t = Untrusted::new();
    write(
        &t.repo,
        "npm/dep/package.json",
        r#"{"name":"dep","version":"1.0.0","scripts":{"preinstall":"node preinstall.cjs"}}"#,
    );
    write(&t.repo, "npm/dep/preinstall.cjs", &t.witness("preinstall"));
    write(
        &t.repo,
        "npm/package.json",
        r#"{"name":"n","private":true,"dependencies":{"dep":"file:./dep"}}"#,
    );
    sh(
        &t.repo.join("npm"),
        "npm install --package-lock-only --ignore-scripts",
    );
    t.commit();

    let run = t.install_one("npm", "npm", true);
    assert!(
        !t.marker("preinstall").exists(),
        "preinstall ran: {}",
        run.text()
    );
    t.assert_safe("npm", &run);
}

#[test]
fn untrusted_mode_leaves_bun_with_nothing_from_the_tree_to_run() {
    if !have("bun") {
        eprintln!("skipped: bun is not on PATH");
        return;
    }
    let t = Untrusted::new();
    write(
        &t.repo,
        "bun/dep/package.json",
        r#"{"name":"dep","version":"1.0.0","scripts":{"preinstall":"node preinstall.cjs","postinstall":"node preinstall.cjs"}}"#,
    );
    write(&t.repo, "bun/dep/preinstall.cjs", &t.witness("lifecycle"));
    write(
        &t.repo,
        "bun/package.json",
        r#"{"name":"d","private":true,"dependencies":{"dep":"file:./dep"}}"#,
    );
    sh(&t.repo.join("bun"), "bun install --ignore-scripts");
    let _ = fs::remove_dir_all(t.repo.join("bun/node_modules"));
    t.commit();
    let run = t.install_one("bun", "bun", true);
    assert!(
        !t.marker("lifecycle").exists(),
        "bun ran a lifecycle script: {}",
        run.text()
    );
    t.assert_safe("bun", &run);
}

// ---------------------------------------------------------------------------
// carrick#2246: the settings `--ignore-scripts` and the flags above leave open.
// ---------------------------------------------------------------------------

/// A shell program that records that it ran, then hands on to `then`.
fn witness_program(t: &Untrusted, name: &str, then: &str) -> String {
    format!(
        "#!/bin/sh\n: > {:?}\nexec {then} \"$@\"\n",
        t.marker(name).to_str().expect("utf-8 path")
    )
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

#[test]
#[cfg(unix)]
fn untrusted_mode_never_runs_a_program_an_npmrc_names() {
    // A git dependency is what makes npm start programs during `npm ci
    // --ignore-scripts`: it clones with the `git` setting, and a dependency
    // whose manifest declares `prepare` is prepared by a child npm that
    // starts with `node-options`. The tree's `.npmrc` names both.
    let build = |t: &Untrusted| {
        let dep = t.markers.parent().expect("scratch").join("dep");
        write(
            &dep,
            "package.json",
            r#"{"name":"dep","version":"1.0.0","scripts":{"prepare":"node -e 0"}}"#,
        );
        sh(
            &dep,
            "git init -q . && git add -A \
             && git -c user.email=t@t -c user.name=t commit -q -m dep",
        );
        write(
            &t.repo,
            "npm/package.json",
            &format!(
                r#"{{"name":"n","private":true,"dependencies":{{"dep":"git+file://{}"}}}}"#,
                dep.to_str().expect("utf-8 path")
            ),
        );
        sh(
            &t.repo.join("npm"),
            "npm install --package-lock-only --ignore-scripts",
        );
        write(&t.repo, "npm/git.sh", &witness_program(t, "git", "git"));
        make_executable(&t.repo.join("npm/git.sh"));
        write(&t.repo, "npm/hook.cjs", &t.witness("node-options"));
        write(
            &t.repo,
            "npm/.npmrc",
            &format!(
                "git=./git.sh\nnode-options=--require {}\n",
                t.repo.join("npm/hook.cjs").to_str().expect("utf-8 path")
            ),
        );
        t.commit();
    };

    let default = Untrusted::new();
    build(&default);
    let run = default.install_one("npm", "npm", false);
    for marker in ["git", "node-options"] {
        assert!(
            default.marker(marker).exists(),
            "default mode runs the .npmrc `{marker}` program, so this case can see it: {}",
            run.text()
        );
    }

    let untrusted = Untrusted::new();
    build(&untrusted);
    let run = untrusted.install_one("npm", "npm", true);
    for marker in ["git", "node-options"] {
        assert!(
            !untrusted.marker(marker).exists(),
            "the .npmrc `{marker}` program ran in untrusted mode: {}",
            run.text()
        );
    }
    assert!(
        untrusted.repo.join("npm/node_modules/dep").exists(),
        "the git dependency is still installed: {}",
        run.text()
    );
    untrusted.assert_safe("npm", &run);
}

#[test]
fn untrusted_mode_reads_every_yarnrc_up_to_the_clone_root_as_yaml() {
    if !have("yarn") {
        eprintln!("skipped: yarn is not on PATH");
        return;
    }
    // Yarn reads `.yarnrc.yml` in every parent of the directory it runs in,
    // and YAML spells one key many ways. Each tree pins its Yarn, so default
    // mode loads the plugin and the case can see it.
    let pinned_service = |t: &Untrusted| {
        write(
            &t.repo,
            "svc/package.json",
            r#"{"name":"s","private":true,"packageManager":"yarn@4.9.2"}"#,
        );
        write(&t.repo, "svc/yarn.lock", "__metadata:\n  version: 8\n");
    };
    let parent = |t: &Untrusted| {
        pinned_service(t);
        write(
            &t.repo,
            ".yarnrc.yml",
            "plugins:\n  - path: .yarn/plugins/p.cjs\n",
        );
        write(&t.repo, ".yarn/plugins/p.cjs", &plugin(t));
        t.commit();
    };
    let quoted = |t: &Untrusted| {
        pinned_service(t);
        write(
            &t.repo,
            "svc/.yarnrc.yml",
            "\"plugins\": [{ path: .yarn/plugins/p.cjs }]\n",
        );
        write(&t.repo, "svc/.yarn/plugins/p.cjs", &plugin(t));
        t.commit();
    };

    for (case, build) in [
        ("parent", &parent as &dyn Fn(&Untrusted)),
        ("quoted", &quoted as &dyn Fn(&Untrusted)),
    ] {
        let default = Untrusted::new();
        build(&default);
        let run = default.install_one("svc", "yarn", false);
        assert!(
            default.marker("plugin").exists(),
            "{case}: default mode loads the plugin, so this case can see it: {}",
            run.text()
        );

        let untrusted = Untrusted::new();
        build(&untrusted);
        let run = untrusted.install_one("svc", "yarn", true);
        assert!(
            !untrusted.marker("plugin").exists(),
            "{case}: the plugin ran in untrusted mode: {}",
            run.text()
        );
        assert!(
            run.text().contains("::warning::") && run.text().contains("lists plugins"),
            "{case}: the root is refused and named: {}",
            run.text()
        );
        untrusted.assert_safe("svc", &run);
    }
}

/// A Yarn plugin that records that it was loaded.
fn plugin(t: &Untrusted) -> String {
    format!(
        "{}module.exports = {{ name: 'p', factory: () => ({{}}) }};\n",
        t.witness("plugin")
    )
}

#[test]
fn untrusted_mode_refuses_a_yarn_root_it_has_no_reader_for() {
    let t = Untrusted::new();
    write(
        &t.repo,
        "svc/package.json",
        r#"{"name":"s","private":true}"#,
    );
    write(&t.repo, "svc/yarn.lock", "__metadata:\n  version: 8\n");
    t.commit();
    let output = Command::new("bash")
        .arg(script())
        .args([
            "install-one",
            t.repo.join("svc").to_str().expect("utf-8 path"),
            "yarn",
        ])
        .env_remove("CARRICK_CLI")
        .env("CARRICK_INSTALL_UNTRUSTED", "1")
        .output()
        .expect("run install-scanned-deps.sh");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("::warning::") && text.contains("CARRICK_CLI is unset"),
        "a Yarn root is refused when nothing can read its rc files: {text}"
    );
    assert!(!t.repo.join("svc/node_modules").exists(), "{text}");
}

/// carrick#2245. A service root outside the scan root is never prepared, and
/// its lockfile is never read: the containment check runs before the lookup.
///
/// `carrick derive` now refuses such a config, so a stand-in CLI names the
/// outside root here; this is the installer's own guard, for any answer that
/// still names one.
#[cfg(unix)]
#[test]
fn a_service_root_outside_the_scan_root_is_never_prepared() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("scratch dir");
    let root = dir.path().join("repo");
    package_with_lockfile(&root, ".", "inside");
    package_with_lockfile(dir.path(), "outside", "outside");
    let cli = dir.path().join("fake-carrick");
    write(
        dir.path(),
        "fake-carrick",
        &format!(
            "#!/bin/sh\nprintf '%s' '{}'\n",
            serde_json::json!({
                "repos": [{
                    "path": root,
                    "services": [{"directory": "."}, {"directory": "../outside"}],
                }],
            })
        ),
    );
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).expect("chmod");

    let detected = run_with(
        &["detect", root.to_str().expect("utf-8 path")],
        Some(cli.to_str().expect("utf-8 path")),
    );
    assert_eq!(detected.status, 0, "detect exits 0: {}", detected.text());
    assert!(
        detected.out.contains("roots=1"),
        "only the repository root is prepared: {}",
        detected.text()
    );
    assert_eq!(prepared_roots(&detected), ["."], "{}", detected.text());
    assert!(
        !detected.text().contains("outside"),
        "the outside root is never named, so its lockfile was never read: {}",
        detected.text()
    );
}

// ---------------------------------------------------------------------------
// carrick#2251: what a dependency the tree names brings with it. A git
// dependency is cloned and prepared by a package manager started inside the
// clone, which reads the clone's own config; a pnpm config dependency is a
// package whose pnpmfile pnpm loads. Both are served from this machine, so
// nothing leaves it.
// ---------------------------------------------------------------------------

/// A port nothing listens on yet.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind a free port")
        .local_addr()
        .expect("local addr")
        .port()
}

/// Wait until something accepts on `port`, or fail the case: a server that
/// never came up would make the default half fail for the wrong reason.
fn await_listener(port: u16, what: &str) {
    for _ in 0..200 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("{what} never listened on 127.0.0.1:{port}");
}

/// `git daemon` serving every repository under `base` as `git://127.0.0.1:<port>/<dir>`,
/// killed on drop. Yarn Berry's URL parser rejects `git+file://`, and the
/// `git://` protocol is a form every manager accepts with no network.
struct GitDaemon {
    child: std::process::Child,
    port: u16,
}

impl GitDaemon {
    fn serve(base: &Path) -> Self {
        let port = free_port();
        let base_text = base.to_str().expect("utf-8 path");
        let child = Command::new("git")
            .args([
                "daemon",
                "--reuseaddr",
                "--export-all",
                "--listen=127.0.0.1",
                &format!("--port={port}"),
                &format!("--base-path={base_text}"),
                base_text,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start git daemon");
        await_listener(port, "git daemon");
        Self { child, port }
    }

    fn url(&self, repo: &str) -> String {
        format!("git://127.0.0.1:{}/{repo}", self.port)
    }
}

impl Drop for GitDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Commit everything in `dir` as a repository of its own.
fn commit_repository(dir: &Path) {
    sh(
        dir,
        "git init -q . && git add -A \
         && git -c user.email=t@t -c user.name=t commit -q -m dep",
    );
}

#[test]
#[cfg(unix)]
fn untrusted_mode_never_lets_a_yarn_git_dependency_load_its_own_config() {
    if !have("yarn") {
        eprintln!("skipped: yarn is not on PATH");
        return;
    }
    // Yarn Berry prepares a git dependency by starting a package manager in
    // the clone (`prepareExternalProject`): Yarn, Yarn Classic or npm, chosen
    // by the clone's own files. Each reads the clone's config, so the clone's
    // `.yarnrc.yml` `plugins` load, and no setting of the parent install
    // reaches them. This dependency pins no manager, so the child is the
    // parent's own Yarn. Its commit carries the marker path, which is unique
    // to the case, so no cache can answer for the clone. Yarn 4.9.2
    // passes `-c core.autocrlf=false` to `git clone` as one argument, which a
    // current git rejects, so the clone (and the case) would fail before
    // anything is prepared; the root pins a Yarn that clones. Since then
    // Yarn fetches only the git repositories `approvedGitRepositories`
    // names, and the tree's own `.yarnrc.yml` is what names them.
    let build = |t: &Untrusted, daemon_base: &Path| -> GitDaemon {
        let dep = daemon_base.join("dep");
        write(&dep, "package.json", r#"{"name":"dep","version":"1.0.0"}"#);
        write(
            &dep,
            ".yarnrc.yml",
            "plugins:\n  - path: .yarn/plugins/p.cjs\n",
        );
        write(&dep, ".yarn/plugins/p.cjs", &plugin(t));
        commit_repository(&dep);
        let daemon = GitDaemon::serve(daemon_base);
        write(
            &t.repo,
            "svc/package.json",
            &format!(
                r#"{{"name":"s","private":true,"packageManager":"yarn@4.17.1","dependencies":{{"dep":"{}"}}}}"#,
                daemon.url("dep")
            ),
        );
        write(
            &t.repo,
            "svc/.yarnrc.yml",
            "approvedGitRepositories:\n  - \"**\"\n",
        );
        // The lockfile, written by Yarn itself. Writing it prepares the
        // dependency too, so the marker it leaves is cleared, and the cache it
        // fills is the case's own, so the installs below clone again. On CI
        // Yarn forbids creating a lockfile unless told it may.
        let global = t.markers.parent().expect("scratch").join("yarn-global");
        sh(
            &t.repo.join("svc"),
            &format!(
                "YARN_GLOBAL_FOLDER={} YARN_ENABLE_IMMUTABLE_INSTALLS=0 \
                 corepack yarn install --mode=skip-build \
                 && rm -rf node_modules .yarn .pnp.cjs .pnp.loader.mjs",
                global.to_str().expect("utf-8 path")
            ),
        );
        let _ = fs::remove_file(t.marker("plugin"));
        t.commit();
        daemon
    };

    let default = Untrusted::new();
    let base = default.markers.parent().expect("scratch").join("served");
    let _daemon = build(&default, &base);
    let run = default.install_one("svc", "yarn", false);
    assert!(
        default.marker("plugin").exists(),
        "default mode loads the git dependency's plugin, so this case can see it: {}",
        run.text()
    );

    let untrusted = Untrusted::new();
    let base = untrusted.markers.parent().expect("scratch").join("served");
    let _daemon = build(&untrusted, &base);
    let run = untrusted.install_one("svc", "yarn", true);
    assert!(
        !untrusted.marker("plugin").exists(),
        "the git dependency's plugin ran in untrusted mode: {}",
        run.text()
    );
    assert!(
        run.text().contains("::warning::") && run.text().contains("from a git repository"),
        "the root is refused and named: {}",
        run.text()
    );
    untrusted.assert_safe("svc", &run);
}

#[test]
#[cfg(unix)]
fn untrusted_mode_never_lets_classic_yarn_prepare_a_git_dependency() {
    if !have("yarn") {
        eprintln!("skipped: yarn is not on PATH");
        return;
    }
    // Classic Yarn fetches a git dependency that declares `prepare` by
    // cloning it and running an install in the clone, which runs `prepare`
    // whatever `--ignore-scripts` says (carrick#2258).
    let build = |t: &Untrusted, daemon_base: &Path| -> GitDaemon {
        let dep = daemon_base.join("dep");
        write(
            &dep,
            "package.json",
            r#"{"name":"dep","version":"1.0.0","scripts":{"prepare":"node prepare.cjs"}}"#,
        );
        write(&dep, "prepare.cjs", &t.witness("prepare"));
        commit_repository(&dep);
        let daemon = GitDaemon::serve(daemon_base);
        write(
            &t.repo,
            "classic/package.json",
            &format!(
                r#"{{"name":"c","private":true,"dependencies":{{"dep":"{}"}}}}"#,
                daemon.url("dep")
            ),
        );
        // The lockfile, written by Yarn itself into the case's own cache;
        // writing it prepares the dependency, so its marker is cleared.
        let cache = t.markers.parent().expect("scratch").join("yarn-cache");
        sh(
            &t.repo.join("classic"),
            &format!(
                "YARN_CACHE_FOLDER={} yarn install --ignore-scripts && rm -rf node_modules",
                cache.to_str().expect("utf-8 path")
            ),
        );
        let _ = fs::remove_file(t.marker("prepare"));
        t.commit();
        daemon
    };

    let default = Untrusted::new();
    let base = default.markers.parent().expect("scratch").join("served");
    let _daemon = build(&default, &base);
    let run = default.install_one("classic", "yarn", false);
    assert!(
        default.marker("prepare").exists(),
        "default mode runs the git dependency's prepare, so this case can see it: {}",
        run.text()
    );

    let untrusted = Untrusted::new();
    let base = untrusted.markers.parent().expect("scratch").join("served");
    let _daemon = build(&untrusted, &base);
    let run = untrusted.install_one("classic", "yarn", true);
    assert!(
        !untrusted.marker("prepare").exists(),
        "the git dependency's prepare ran in untrusted mode: {}",
        run.text()
    );
    assert!(
        run.text().contains("::warning::") && run.text().contains("from a git repository"),
        "the root is refused and named: {}",
        run.text()
    );
    untrusted.assert_safe("classic", &run);
}

#[test]
fn untrusted_mode_never_runs_a_yarn_exec_dependency() {
    if !have("yarn") {
        eprintln!("skipped: yarn is not on PATH");
        return;
    }
    // Yarn Berry's `exec:` protocol builds a package by running a file from
    // the tree (carrick#2257). The file's content carries the marker path, so
    // no cache can answer for it. Later Yarns refuse `exec:` when builds are
    // off, but the tree picks its Yarn, and 4.9.2 runs it.
    let build = |t: &Untrusted| {
        write(
            &t.repo,
            "svc/package.json",
            r#"{"name":"s","private":true,"packageManager":"yarn@4.9.2","dependencies":{"x":"exec:./gen.cjs"}}"#,
        );
        write(
            &t.repo,
            "svc/gen.cjs",
            &format!(
                "{}require('fs').writeFileSync(execEnv.buildDir + '/package.json', \
                 JSON.stringify({{ name: 'x', version: '1.0.0' }}));\n",
                t.witness("exec")
            ),
        );
        let global = t.markers.parent().expect("scratch").join("yarn-global");
        sh(
            &t.repo.join("svc"),
            &format!(
                "YARN_GLOBAL_FOLDER={} YARN_ENABLE_IMMUTABLE_INSTALLS=0 \
                 corepack yarn install --mode=skip-build \
                 && rm -rf node_modules .yarn .pnp.cjs .pnp.loader.mjs",
                global.to_str().expect("utf-8 path")
            ),
        );
        let _ = fs::remove_file(t.marker("exec"));
        t.commit();
    };

    let default = Untrusted::new();
    build(&default);
    let run = default.install_one("svc", "yarn", false);
    assert!(
        default.marker("exec").exists(),
        "default mode runs the exec: file, so this case can see it: {}",
        run.text()
    );

    let untrusted = Untrusted::new();
    build(&untrusted);
    let run = untrusted.install_one("svc", "yarn", true);
    assert!(
        !untrusted.marker("exec").exists(),
        "the exec: file ran in untrusted mode: {}",
        run.text()
    );
    assert!(
        run.text().contains("::warning::") && run.text().contains("with the exec: protocol"),
        "the root is refused and named: {}",
        run.text()
    );
    untrusted.assert_safe("svc", &run);
}

/// A registry on this machine serving one package: every `.tgz` path is the
/// tarball, every other path its packument. pnpm asks for the tarball at the
/// registry's conventional path, which a static directory cannot serve beside
/// a packument of the same name.
struct LocalRegistry {
    port: u16,
}

impl LocalRegistry {
    fn serve(packument: impl FnOnce(u16) -> String, tarball: Vec<u8>) -> Self {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind registry");
        let port = listener.local_addr().expect("registry addr").port();
        let packument = packument(port).into_bytes();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut request_line = String::new();
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                if reader.read_line(&mut request_line).is_err() {
                    continue;
                }
                // Drain the headers.
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    line.clear();
                }
                let path = request_line.split_whitespace().nth(1).unwrap_or("");
                let (body, kind) = if path.ends_with(".tgz") {
                    (&tarball, "application/octet-stream")
                } else {
                    (&packument, "application/json")
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(body);
            }
        });
        Self { port }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }
}

#[test]
fn untrusted_mode_never_loads_a_pnpmfile_a_config_dependency_ships() {
    if !have("corepack") {
        eprintln!("skipped: corepack is not on PATH");
        return;
    }
    // pnpm 10 installs `configDependencies` from the registry before anything
    // else, and loads the pnpmfile of one named `pnpm-plugin-*`. The package
    // is packed and served here.
    let build = |t: &Untrusted| -> LocalRegistry {
        let scratch = t.markers.parent().expect("scratch");
        let package = scratch.join("plugin");
        write(
            &package,
            "package.json",
            r#"{"name":"pnpm-plugin-x","version":"1.0.0"}"#,
        );
        write(
            &package,
            "pnpmfile.cjs",
            &format!(
                "{}module.exports = {{ hooks: {{}} }};\n",
                t.witness("pnpmfile")
            ),
        );
        let packed = Command::new("npm")
            .args(["pack", "--json", "--ignore-scripts"])
            .current_dir(&package)
            .output()
            .expect("npm pack");
        assert!(packed.status.success(), "npm pack failed");
        let packed: serde_json::Value =
            serde_json::from_slice(&packed.stdout).expect("npm pack --json");
        let integrity = packed[0]["integrity"]
            .as_str()
            .expect("integrity")
            .to_string();
        let filename = packed[0]["filename"].as_str().expect("filename");
        let tarball = fs::read(package.join(filename)).expect("read tarball");
        let registry = LocalRegistry::serve(
            |port| {
                serde_json::json!({
                    "name": "pnpm-plugin-x",
                    "dist-tags": {"latest": "1.0.0"},
                    "versions": {"1.0.0": {
                        "name": "pnpm-plugin-x",
                        "version": "1.0.0",
                        "dist": {
                            "tarball": format!("http://127.0.0.1:{port}/pnpm-plugin-x/-/pnpm-plugin-x-1.0.0.tgz"),
                            "integrity": integrity,
                        },
                    }},
                })
                .to_string()
            },
            tarball,
        );
        write(
            &t.repo,
            "pnpm/package.json",
            r#"{"name":"p","private":true,"packageManager":"pnpm@10.18.2"}"#,
        );
        write(
            &t.repo,
            "pnpm/.npmrc",
            &format!("registry={}\n", registry.url()),
        );
        write(
            &t.repo,
            "pnpm/pnpm-workspace.yaml",
            &format!("configDependencies:\n  pnpm-plugin-x: \"1.0.0+{integrity}\"\n"),
        );
        // The lockfile, written by pnpm itself; that loads the pnpmfile too,
        // so its marker is cleared. The store is the case's own.
        sh(
            &t.repo.join("pnpm"),
            &format!(
                "corepack pnpm install --store-dir {} && rm -rf node_modules",
                scratch.join("store").to_str().expect("utf-8 path")
            ),
        );
        let _ = fs::remove_file(t.marker("pnpmfile"));
        t.commit();
        registry
    };

    let default = Untrusted::new();
    let _registry = build(&default);
    let run = default.install_one("pnpm", "pnpm", false);
    assert!(
        default.marker("pnpmfile").exists(),
        "default mode loads the config dependency's pnpmfile, so this case can see it: {}",
        run.text()
    );

    let untrusted = Untrusted::new();
    let _registry = build(&untrusted);
    let run = untrusted.install_one("pnpm", "pnpm", true);
    assert!(
        !untrusted.marker("pnpmfile").exists(),
        "the config dependency's pnpmfile ran in untrusted mode: {}",
        run.text()
    );
    untrusted.assert_safe("pnpm", &run);
}

// ---------------------------------------------------------------------------
// carrick#3069: CARRICK_INSTALL_LOCKFILE_RETRY=1.
//
// The hosted runner sets it. A strict install that fails with anything but a
// timeout is retried once without the frozen lockfile, lifecycle scripts still
// disabled, and the tree stays the commit. Every failed attempt prints the head
// of its log, in every mode.
// ---------------------------------------------------------------------------

#[test]
fn retry_installs_a_root_whose_lockfile_is_behind_its_manifest() {
    for (tool, needs, root, generate) in [
        ("npm", "npm", "npm", "npm install --ignore-scripts"),
        (
            "pnpm",
            "corepack",
            "pnpm",
            "corepack pnpm install --ignore-scripts",
        ),
        ("yarn", "yarn", "classic", "yarn install --ignore-scripts"),
        ("bun", "bun", "bun", "bun install --ignore-scripts"),
    ] {
        if !have(needs) {
            eprintln!("skipped: {needs} is not on PATH");
            continue;
        }
        let t = Untrusted::new();
        ahead_of_its_lockfile(&t, root, generate);
        let run = t.install_one_with(root, tool, true, true);
        assert!(
            run.text().contains("retrying once"),
            "{tool}: the retry is announced: {}",
            run.text()
        );
        assert!(
            t.repo.join(root).join("node_modules/b").exists(),
            "{tool}: the dependency the lockfile lacked is installed: {}",
            run.text()
        );
        assert!(
            !run.text().contains("::warning::"),
            "{tool}: a root the retry installed is not a failed root: {}",
            run.text()
        );
        t.assert_safe(root, &run);
    }
}

/// A manager on `PATH` that counts its calls, prints numbered lines, can create
/// directories and sleep, and exits 1.
struct StubManager {
    _scratch: tempfile::TempDir,
    bin: PathBuf,
    root: PathBuf,
    counter: PathBuf,
}

impl StubManager {
    fn new(name: &str) -> Self {
        let scratch = tempfile::tempdir().expect("scratch dir");
        let base = scratch.path().canonicalize().expect("canonical scratch");
        let bin = base.join("bin");
        let root = base.join("root");
        fs::create_dir_all(&bin).expect("bin dir");
        fs::create_dir_all(&root).expect("root dir");
        fs::write(root.join("package.json"), r#"{"name":"s","private":true}"#).expect("manifest");
        let stub = bin.join(name);
        fs::write(
            &stub,
            "#!/bin/bash\n\
             echo call >> \"$STUB_COUNTER\"\n\
             if [ -n \"$STUB_SLEEP\" ]; then exec sleep \"$STUB_SLEEP\"; fi\n\
             i=1; while [ \"$i\" -le \"${STUB_LINES:-0}\" ]; do echo \"line $i\"; i=$((i+1)); done\n\
             for d in $STUB_MKDIRS; do mkdir -p \"$d\"; done\n\
             exit 1\n",
        )
        .expect("write stub");
        make_executable(&stub);
        Self {
            counter: base.join("counter"),
            _scratch: scratch,
            bin,
            root,
        }
    }

    fn calls(&self) -> usize {
        fs::read_to_string(&self.counter)
            .map(|c| c.lines().count())
            .unwrap_or(0)
    }

    fn run(&self, manager: &str, retry: bool, env: &[(&str, &str)]) -> Run {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = Command::new("bash");
        command
            .arg(script())
            .args(["install-one", self.root.to_str().expect("utf-8"), manager])
            .env("PATH", path)
            .env("STUB_COUNTER", &self.counter)
            .env_remove("CARRICK_INSTALL_UNTRUSTED")
            .env_remove("CARRICK_INSTALL_LOCKFILE_RETRY");
        if retry {
            command.env("CARRICK_INSTALL_LOCKFILE_RETRY", "1");
        }
        for (key, value) in env {
            command.env(key, value);
        }
        let output = command.output().expect("run install-scanned-deps.sh");
        Run {
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            err: String::from_utf8_lossy(&output.stderr).into_owned(),
            status: output.status.code().unwrap_or(-1),
        }
    }
}

fn warnings(run: &Run) -> usize {
    run.text().matches("::warning::").count()
}

#[test]
#[cfg(unix)]
fn a_failed_retry_leaves_no_node_modules_it_created() {
    let stub = StubManager::new("npm");
    // One directory that was there before the first attempt, two that were not.
    fs::create_dir_all(stub.root.join("kept/node_modules")).expect("pre-existing");
    let run = stub.run(
        "npm",
        true,
        &[("STUB_MKDIRS", "node_modules/x packages/a/node_modules")],
    );
    assert_eq!(run.status, 0, "{}", run.text());
    assert!(!stub.root.join("node_modules").exists(), "{}", run.text());
    assert!(
        !stub.root.join("packages/a/node_modules").exists(),
        "{}",
        run.text()
    );
    assert!(
        stub.root.join("kept/node_modules").exists(),
        "a directory that predates the attempts is left alone"
    );
    assert_eq!(warnings(&run), 1, "one warning per root: {}", run.text());
    assert_eq!(stub.calls(), 2, "strict, then one retry: {}", run.text());
}

#[test]
#[cfg(unix)]
fn a_timed_out_install_is_not_retried() {
    if !have("timeout") {
        eprintln!("skipped: timeout is not on PATH");
        return;
    }
    let stub = StubManager::new("npm");
    let run = stub.run(
        "npm",
        true,
        &[("STUB_SLEEP", "30"), ("CARRICK_INSTALL_TIMEOUT", "1")],
    );
    assert_eq!(stub.calls(), 1, "{}", run.text());
    assert!(run.text().contains("hit the 1s limit"), "{}", run.text());
    assert_eq!(warnings(&run), 1, "{}", run.text());
}

#[test]
#[cfg(unix)]
fn a_failed_install_prints_the_head_of_its_log() {
    let long = StubManager::new("npm");
    let run = long.run("npm", false, &[("STUB_LINES", "100")]);
    let lines: Vec<&str> = run.out.lines().collect();
    let has = |n: usize| lines.contains(&format!("line {n}").as_str());
    assert!((1..=30).all(has), "the head is printed: {}", run.text());
    assert!((81..=100).all(has), "the tail is printed: {}", run.text());
    assert!(!(31..=80).any(has), "the middle is cut: {}", run.text());
    assert!(
        lines.contains(&"... (50 lines omitted) ..."),
        "the marker counts the cut lines: {}",
        run.text()
    );
    assert_eq!(long.calls(), 1, "default mode does not retry");

    let short = StubManager::new("npm");
    let run = short.run("npm", false, &[("STUB_LINES", "40")]);
    assert!(
        (1..=40).all(|n| run.out.lines().any(|l| l == format!("line {n}"))),
        "a short log prints whole: {}",
        run.text()
    );
    assert!(!run.text().contains("lines omitted"), "{}", run.text());
}

#[test]
#[cfg(unix)]
fn berry_and_deno_are_not_retried() {
    let berry = StubManager::new("yarn");
    fs::write(berry.root.join(".yarnrc.yml"), "nodeLinker: node-modules\n").expect("yarnrc");
    let run = berry.run("yarn", true, &[]);
    assert_eq!(berry.calls(), 1, "Berry: {}", run.text());
    assert_eq!(warnings(&run), 1, "{}", run.text());

    let deno = StubManager::new("deno");
    let run = deno.run("deno", true, &[]);
    assert_eq!(deno.calls(), 1, "Deno: {}", run.text());
    assert_eq!(warnings(&run), 1, "{}", run.text());
}

#[test]
fn the_retry_never_loads_a_pnpmfile() {
    if !have("corepack") {
        eprintln!("skipped: corepack is not on PATH");
        return;
    }
    let build = |t: &Untrusted| {
        ahead_of_its_lockfile(t, "pnpm", "corepack pnpm install --ignore-scripts");
        // After the lockfile is generated, or the generation would run it.
        write(&t.repo, "pnpm/.pnpmfile.cjs", &t.witness("pnpmfile"));
        t.commit();
    };

    let default = Untrusted::new();
    build(&default);
    let run = default.install_one_with("pnpm", "pnpm", false, true);
    assert!(
        default.marker("pnpmfile").exists(),
        "without untrusted mode the retry runs the pnpmfile, so this case can see it: {}",
        run.text()
    );

    let untrusted = Untrusted::new();
    build(&untrusted);
    let run = untrusted.install_one_with("pnpm", "pnpm", true, true);
    assert!(
        run.text().contains("retrying once"),
        "the retry is the command under test: {}",
        run.text()
    );
    assert!(
        !untrusted.marker("pnpmfile").exists(),
        "the pnpmfile ran in the retry: {}",
        run.text()
    );
    untrusted.assert_safe("pnpm", &run);
}

#[test]
#[cfg(unix)]
fn the_retry_never_runs_a_program_an_npmrc_names() {
    if !have("npm") || !have("git") {
        eprintln!("skipped: npm or git is not on PATH");
        return;
    }
    let build = |t: &Untrusted| {
        let dep = t.markers.parent().expect("scratch").join("dep");
        write(
            &dep,
            "package.json",
            r#"{"name":"dep","version":"1.0.0","scripts":{"prepare":"node -e 0"}}"#,
        );
        sh(
            &dep,
            "git init -q . && git add -A \
             && git -c user.email=t@t -c user.name=t commit -q -m dep",
        );
        write(
            &t.repo,
            "npm/a/package.json",
            r#"{"name":"a","version":"1.0.0"}"#,
        );
        let manifest = |extra: &str| {
            format!(
                r#"{{"name":"n","private":true,"dependencies":{{"dep":"git+file://{}"{extra}}}}}"#,
                dep.to_str().expect("utf-8 path")
            )
        };
        write(&t.repo, "npm/package.json", &manifest(""));
        sh(
            &t.repo.join("npm"),
            "npm install --package-lock-only --ignore-scripts",
        );
        // One entry ahead of the lockfile: `npm ci` refuses, the retry installs.
        write(&t.repo, "npm/package.json", &manifest(r#","a":"file:./a""#));
        write(&t.repo, "npm/git.sh", &witness_program(t, "git", "git"));
        make_executable(&t.repo.join("npm/git.sh"));
        write(&t.repo, "npm/hook.cjs", &t.witness("node-options"));
        write(
            &t.repo,
            "npm/.npmrc",
            &format!(
                "git=./git.sh\nnode-options=--require {}\n",
                t.repo.join("npm/hook.cjs").to_str().expect("utf-8 path")
            ),
        );
        t.commit();
    };

    let default = Untrusted::new();
    build(&default);
    let run = default.install_one_with("npm", "npm", false, true);
    for marker in ["git", "node-options"] {
        assert!(
            default.marker(marker).exists(),
            "without untrusted mode the retry runs the .npmrc `{marker}` program: {}",
            run.text()
        );
    }

    let untrusted = Untrusted::new();
    build(&untrusted);
    let run = untrusted.install_one_with("npm", "npm", true, true);
    assert!(
        run.text().contains("retrying once"),
        "the retry is the command under test: {}",
        run.text()
    );
    for marker in ["git", "node-options"] {
        assert!(
            !untrusted.marker(marker).exists(),
            "the .npmrc `{marker}` program ran in the retry: {}",
            run.text()
        );
    }
    untrusted.assert_safe("npm", &run);
}
