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
        let dir = self.repo.join(root);
        let mut command = Command::new("bash");
        command
            .arg(script())
            .args(["install-one", dir.to_str().expect("utf-8 path"), manager])
            .env_remove("CARRICK_CLI")
            .env_remove("CARRICK_INSTALL_UNTRUSTED");
        if untrusted {
            command.env("CARRICK_INSTALL_UNTRUSTED", "1");
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
        unpinned.text().contains("pins no packageManager"),
        "an unpinned yarnPath is named in a warning: {}",
        unpinned.text()
    );
    let pinned = t.install_one("pinned", "yarn", true);
    assert!(
        !pinned.text().contains("pins no packageManager") && !t.marker("pinned").exists(),
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
