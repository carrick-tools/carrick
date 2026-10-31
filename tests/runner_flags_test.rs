//! carrick#2207: `--repo <owner/name>` names the repository a scan compares
//! its stored blobs against, and `--no-upload` writes none.
//!
//! A scan run outside GitHub Actions has no `GITHUB_REPOSITORY`, so it used to
//! name itself after its folder. A checkout cloned into a folder not named like
//! the repository then found no previous generation (every file went to the
//! model again) and kept its own stored copy among the siblings it is joined
//! against.
//!
//! Drives the real binary on the offline harness: `CARRICK_LOCAL_STORAGE_DIR`
//! as the store, the fixture's own cassette as the model, no cloud, no spend.
//! One generation is stored from a folder named `api`; the same commit is then
//! scanned from a folder named `checkout`.

use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURE: &str = "env-var-whole-url";

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(FIXTURE)
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .status()
        .expect("git failed to spawn");
    assert!(status.success(), "git {args:?} failed");
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

/// What one scan printed and how it exited.
struct Scan {
    output: String,
    success: bool,
}

impl Scan {
    /// The files this scan sent to the analyzer, from its boundary line.
    fn files_sent(&self) -> usize {
        let line = Regex::new(r"(\d+) file\(s\) sent to the analyzer").unwrap();
        line.captures(&self.output)
            .unwrap_or_else(|| panic!("no boundary line:\n{}", self.output))[1]
            .parse()
            .unwrap()
    }
}

fn scan(repo: &Path, storage: &Path, flags: &[&str]) -> Scan {
    scan_with_env(repo, storage, flags, &[])
}

fn scan_with_env(repo: &Path, storage: &Path, flags: &[&str], env: &[(&str, &Path)]) -> Scan {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(repo)
        .args(flags)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", fixture().join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_LOCAL_STORAGE_DIR", storage);
    for (name, value) in env {
        cmd.env(name, value);
    }
    // CI sets these to the repo being built, and the first of them would name
    // every scan here `carrick`.
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_EVENT_NAME",
        "GITHUB_REF",
        "GITHUB_RUN_ID",
        "GITHUB_EVENT_PATH",
        "GITHUB_ACTIONS",
        "CI",
        "CARRICK_OUTPUT_JSON",
        "CARRICK_LOCAL_STORAGE_ISOLATE",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let result = cmd.output().expect("failed to spawn carrick");
    Scan {
        output: format!(
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        ),
        success: result.status.success(),
    }
}

/// One generation stored from a folder named `api`, and a copy of the same
/// commit in a folder named `checkout`. Returns (`checkout`, the store).
fn stored_then_moved(tmp: &Path) -> (PathBuf, PathBuf) {
    let api = tmp.join("api");
    copy_dir(&fixture().join("src"), &api.join("src"));
    run_git(&api, &["init", "-q"]);
    run_git(&api, &["add", "-A"]);
    run_git(&api, &["commit", "-q", "-m", "init"]);

    let storage = tmp.join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let first = scan(&api, &storage, &[]);
    assert!(first.success, "the first scan failed:\n{}", first.output);
    assert!(
        first.files_sent() > 0,
        "the first scan must ask the model, or a replay proves nothing:\n{}",
        first.output
    );

    let checkout = tmp.join("checkout");
    copy_dir(&api, &checkout);
    // A copy rewrites every mtime: refresh the index so the tree reads clean.
    run_git(&checkout, &["update-index", "-q", "--refresh"]);
    (checkout, storage)
}

/// With `--repo`, the moved checkout is the stored repository: every answer
/// replays, and its stored copy is its own rather than a sibling's.
#[test]
fn a_declared_repository_reuses_its_stored_generation_from_any_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let (checkout, storage) = stored_then_moved(tmp.path());

    let second = scan(&checkout, &storage, &["--repo", "acme/api"]);
    assert!(second.success, "{}", second.output);
    assert_eq!(second.files_sent(), 0, "{}", second.output);
    // The stored copy left the sibling side: the report names the one repo
    // rather than counting two.
    assert!(second.output.contains("Carrick · api"), "{}", second.output);
    assert!(!second.output.contains("2 repos"), "{}", second.output);
}

/// Without it the folder names the repository, as it always has: the stored
/// generation is a stranger's, so the files go to the model again and the
/// stored copy is joined against as a sibling.
#[test]
fn without_the_flag_the_folder_names_the_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let (checkout, storage) = stored_then_moved(tmp.path());

    let second = scan(&checkout, &storage, &[]);
    assert!(second.success, "{}", second.output);
    assert!(second.files_sent() > 0, "{}", second.output);
    assert!(second.output.contains("2 repos"), "{}", second.output);
}

/// `--no-upload` writes nothing into a store that would otherwise be filled.
#[test]
fn no_upload_leaves_the_store_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let api = tmp.path().join("api");
    copy_dir(&fixture().join("src"), &api.join("src"));
    run_git(&api, &["init", "-q"]);
    run_git(&api, &["add", "-A"]);
    run_git(&api, &["commit", "-q", "-m", "init"]);
    let storage = tmp.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();

    let run = scan(&api, &storage, &["--no-upload"]);
    assert!(run.success, "{}", run.output);
    let written: Vec<_> = std::fs::read_dir(&storage).unwrap().flatten().collect();
    assert!(written.is_empty(), "{written:?}\n{}", run.output);
}

/// The two usage errors exit 2 with their own words, before any scan starts.
#[test]
fn the_runner_flags_refuse_what_they_cannot_do() {
    let tmp = tempfile::tempdir().unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .args(["--repo", "api"])
        .arg(tmp.path())
        .env("CARRICK_MOCK_ALL", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("carrick: --repo takes owner/name (for example acme/api), not 'api'."),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(tmp.path())
        .arg("--upload")
        .env("CARRICK_OUTPUT_JSON", "1")
        .env("CARRICK_MOCK_ALL", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(
            "carrick: --upload cannot be combined with CARRICK_OUTPUT_JSON, which never \
             uploads. Unset one of them."
        ),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git failed to spawn");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A repository with base commit B0, a branch head H1 and a merge commit M of
/// the two, checked out at M. Returns (repo, B0, H1).
fn merged_pr(tmp: &Path) -> (PathBuf, String, String) {
    let repo = tmp.join("api");
    copy_dir(&fixture().join("src"), &repo.join("src"));
    run_git(&repo, &["init", "-q", "-b", "main"]);
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "-q", "-m", "b0"]);
    let b0 = git_out(&repo, &["rev-parse", "HEAD"]);
    run_git(&repo, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(repo.join("notes.txt"), "pr").unwrap();
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "-q", "-m", "h1"]);
    let h1 = git_out(&repo, &["rev-parse", "HEAD"]);
    run_git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("main-side.txt"), "main").unwrap();
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "-q", "-m", "main moves"]);
    run_git(&repo, &["merge", "-q", "--no-ff", "-m", "m", "feature"]);
    (repo, b0, h1)
}

/// carrick#2229: a pull request scan outside Actions takes its number and head
/// from the flags, posts its result without a run id, and writes no blob.
#[test]
fn a_pr_scan_takes_its_context_from_the_flags_and_writes_no_index() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, _b0, h1) = merged_pr(tmp.path());
    let storage = tmp.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let out = tmp.path().join("pr-result.json");
    let base = "0".repeat(40);

    let run = scan_with_env(
        &repo,
        &storage,
        &["--pr", "7", "--pr-head", &h1, "--pr-base", &base],
        &[("CARRICK_PR_RESULT_OUT", &out)],
    );
    assert!(run.success, "{}", run.output);

    let posted: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).expect("a PR run posts its result"))
            .unwrap();
    assert_eq!(posted["pr_number"], 7);
    assert_eq!(posted["head_sha"], h1.as_str());
    assert!(posted.get("run_id").is_none(), "{posted:#}");
    let written: Vec<_> = std::fs::read_dir(&storage).unwrap().flatten().collect();
    assert!(written.is_empty(), "{written:?}\n{}", run.output);
}

/// The flags are all-or-none, validated, and not combined with `--upload`;
/// a checkout that is not the PR's tree is refused. Each exits 2, with its
/// own words, before any scan starts.
#[test]
fn the_pr_flags_refuse_what_they_cannot_do() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, b0, h1) = merged_pr(tmp.path());
    let storage = tmp.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let elsewhere = "2".repeat(40);

    let refused = |flags: &[&str], expected: &str| {
        let run = scan(&repo, &storage, flags);
        assert!(!run.success, "{flags:?}\n{}", run.output);
        assert!(
            run.output.contains(expected),
            "{flags:?}: wanted {expected:?}\n{}",
            run.output
        );
    };
    refused(
        &["--pr", "7"],
        "carrick: --pr, --pr-head and --pr-base go together; pass all three.",
    );
    refused(
        &["--pr", "0", "--pr-head", &h1, "--pr-base", &b0],
        "carrick: --pr takes a pull request number, not '0'.",
    );
    refused(
        &["--pr", "7", "--pr-head", "abc", "--pr-base", &b0],
        "carrick: --pr-head and --pr-base take a full commit id (40 hex characters), not 'abc'.",
    );
    refused(
        &["--pr", "7", "--pr-head", &h1, "--pr-base", &b0, "--upload"],
        "carrick: --upload cannot be combined with --pr: a pull request scan never writes the index.",
    );
    refused(
        &["--pr", "7", "--pr-head", &elsewhere, "--pr-base", &b0],
        &format!(
            "carrick: this checkout is neither the pull request's head ({elsewhere}) nor a merge \
             of it, so its results would describe another tree."
        ),
    );
    let written: Vec<_> = std::fs::read_dir(&storage).unwrap().flatten().collect();
    assert!(written.is_empty(), "{written:?}");
}

/// A committed two-service repo, `alpha` then `beta`, each an HTTP server
/// with one route, so each asks the type sidecar about its handler.
fn two_service_repo(root: &Path) -> PathBuf {
    service_repo(root, &["alpha", "beta"])
}

/// A committed repo of `services`, in that order, each an HTTP server with
/// one route.
fn service_repo(root: &Path, services: &[&str]) -> PathBuf {
    let repo = root.join("monorepo");
    let write = |relative: &str, contents: &str| {
        let path = repo.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    };
    let declared: Vec<String> = services
        .iter()
        .map(|service| format!(r#"  {{ "name": "{service}", "directory": "services/{service}" }}"#))
        .collect();
    write(
        "carrick.json",
        &format!("{{ \"services\": [\n{}\n] }}", declared.join(",\n")),
    );
    write("package.json", r#"{ "name": "monorepo", "private": true }"#);
    for service in services {
        write(
            &format!("services/{service}/package.json"),
            &format!(r#"{{ "name": "{service}", "dependencies": {{ "express": "4.18.2" }} }}"#),
        );
        write(
            &format!("services/{service}/src/server.ts"),
            &format!(
                "import express from \"express\";\n\n\
                 const app = express();\n\n\
                 app.get(\"/{service}/status\", async (_req, res) => {{\n  \
                 const upstream = await fetch(\"http://localhost:9000/api/health\");\n  \
                 res.json({{ ok: upstream.ok }});\n}});\n\n\
                 app.listen(3000);\n"
            ),
        );
    }
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

/// A stand-in type sidecar under `home`: it answers every request, until the
/// first one after an `init` scoped to `beta`. That one it meets as Node
/// meets a full heap: V8's fatal line on stderr, then an abort. It writes
/// `died` first, so the test knows the death happened rather than guessing.
fn sidecar_that_runs_out_of_heap_in_beta(home: &Path, died: &Path) {
    sidecar_stand_in(home, died, false, BetaDies::Always);
}

/// Which of `beta`'s processes the stand-in kills with a full heap.
#[derive(Clone, Copy)]
enum BetaDies {
    /// Every one.
    Always,
    /// The first, the one that finds no `died` file. A run that writes
    /// `died` beforehand has a `beta` that lives.
    Once,
}

/// The stand-in, and with `alpha_waits` its `alpha` process holds its first
/// type request until `beta`'s has died (30 seconds at most), and two
/// seconds more: `alpha` is still being analysed when the death is recorded.
/// Every process notes its scope and its V8 flags in `home/argv` at `init`.
fn sidecar_stand_in(home: &Path, died: &Path, alpha_waits: bool, beta_dies: BetaDies) {
    let dir = home.join("sidecar/dist/src");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("index.js"),
        format!(
            r#"const fs = require("fs");
const pause = new Int32Array(new SharedArrayBuffer(4));
const dies = {always} || !fs.existsSync({died:?});
let waited = false;
let root = "";
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
      fs.writeSync(1, JSON.stringify(Object.assign({{ request_id: request.request_id }}, fields)) + "\n");
    if (request.action === "shutdown") process.exit(0);
    if (request.action === "init") {{
      root = request.repo_root || "";
      fs.appendFileSync({argv:?}, root + "\t" + process.execArgv.join(" ") + "\n");
      answer({{ status: "ready", init_time_ms: 1 }});
      continue;
    }}
    if ({alpha_waits} && root.endsWith("alpha") && !waited) {{
      const since = Date.now();
      while (!fs.existsSync({died:?}) && Date.now() - since < 30000) Atomics.wait(pause, 0, 0, 20);
      Atomics.wait(pause, 0, 0, 2000);
      waited = true;
    }}
    if (root.endsWith("beta") && dies) {{
      fs.writeFileSync({died:?}, request.action);
      fs.writeSync(2, "FATAL ERROR: Reached heap limit Allocation failed - JavaScript heap out of memory\n");
      process.abort();
    }}
    switch (request.action) {{
      case "bundle":
        answer({{ status: "success", dts_content: "", manifest: [], symbol_failures: [] }});
        break;
      case "infer":
        answer({{ status: "success", inferred_types: [] }});
        break;
      case "resolve_definitions":
        answer({{ status: "success", definitions: [] }});
        break;
      default:
        answer({{ status: "error", errors: ["the stand-in answers nothing else"] }});
    }}
  }}
}});
process.stdin.on("end", () => process.exit(0));
process.stdin.resume();
"#,
            died = died.to_string_lossy(),
            alpha_waits = alpha_waits,
            always = matches!(beta_dies, BetaDies::Always),
            argv = home.join("argv").to_string_lossy(),
        ),
    )
    .unwrap();
}

/// What a scan of the two-service repo with the stand-in sidecar did.
struct TypedScan {
    code: Option<i32>,
    stderr: String,
    /// Every line of the run's own log (`~/.carrick/logs`).
    log: String,
}

impl TypedScan {
    /// The services the run uploaded.
    fn uploaded(&self) -> Vec<String> {
        let line =
            Regex::new(r#"MOCK: Uploading repo data for repo: .* \(service: Some\("(\w+)"\)\)"#)
                .unwrap();
        line.captures_iter(&self.log)
            .map(|c| c[1].to_string())
            .collect()
    }
}

fn typed_scan(fail_on_type_memory: bool) -> TypedScan {
    let tmp = tempfile::tempdir().unwrap();
    let repo = two_service_repo(tmp.path());
    let home = tmp.path().join("home");
    let died = tmp.path().join("died");
    sidecar_that_runs_out_of_heap_in_beta(&home, &died);
    typed_scan_of(&repo, &home, &died, fail_on_type_memory, &[])
}

fn typed_scan_of(
    repo: &Path,
    home: &Path,
    died: &Path,
    fail_on_type_memory: bool,
    env: &[(&str, &str)],
) -> TypedScan {
    typed_scan_into(repo, home, died, fail_on_type_memory, env, None)
}

/// [`typed_scan_of`], its blobs written to `store` when given
/// (`CARRICK_LOCAL_STORAGE_DIR`) instead of the mock's log, and `env` set
/// last, so it can replace the 512 MB heap cap.
fn typed_scan_into(
    repo: &Path,
    home: &Path,
    died: &Path,
    fail_on_type_memory: bool,
    env: &[(&str, &str)],
    store: Option<&Path>,
) -> TypedScan {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(repo)
        .args(["--allow-unprepared", "--upload"])
        .env("HOME", home)
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_SIDECAR_DIR", home.join("sidecar"))
        .env("CARRICK_SIDECAR_MAX_OLD_SPACE_MB", "512");
    if fail_on_type_memory {
        cmd.env("CARRICK_FAIL_ON_TYPE_MEMORY", "1");
    } else {
        cmd.env_remove("CARRICK_FAIL_ON_TYPE_MEMORY");
    }
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_EVENT_NAME",
        "GITHUB_REF",
        "GITHUB_ACTIONS",
        "CI",
        "CARRICK_OUTPUT_JSON",
        "CARRICK_LOCAL_STORAGE_DIR",
        "CARRICK_LOCAL_STORAGE_ISOLATE",
        "CARRICK_ALLOW_MISSING_TYPES",
        "CARRICK_RUN_ID",
        "CARRICK_SERVICE_CONCURRENCY",
        "CARRICK_MEMORY_LIMIT_MB",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    cmd.envs(env.iter().copied());
    if let Some(store) = store {
        cmd.env("CARRICK_LOCAL_STORAGE_DIR", store);
    }
    let output = cmd.output().expect("failed to spawn carrick");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        died.exists(),
        "the stand-in was never asked a type request for beta:\n{stderr}"
    );
    let mut log = String::new();
    let mut dirs = vec![home.join(".carrick/logs")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                log.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    TypedScan {
        code: output.status.code(),
        stderr,
        log,
    }
}

/// carrick#2208: told to stop on a type-checker memory death, the run stops
/// at the service the checker died in. The service before it lands, that
/// one does not, the scan is marked failed with its stage, and the exit code
/// is 3, which a runner reads as "run again on a larger machine".
#[test]
fn a_memory_death_stops_a_run_told_to_stop_on_one_with_exit_3() {
    let run = typed_scan(true);
    assert_eq!(run.code, Some(3), "{}\n{}", run.stderr, run.log);
    assert_eq!(run.uploaded(), ["alpha"], "{}", run.log);
    assert!(
        run.log.contains("MOCK: Recording scan failure in "),
        "no scan-failed:\n{}",
        run.log
    );
    assert!(
        run.stderr
            .contains("types_out_of_memory: the type checker reached its heap limit")
            && run
                .stderr
                .contains("for service beta, under a heap cap of 512 MB"),
        "{}",
        run.stderr
    );
}

/// Without the variable the same death costs beta its types and nothing
/// else, as it always has: both services land and the run exits 0.
#[test]
fn without_the_variable_a_memory_death_costs_only_the_types() {
    let run = typed_scan(false);
    assert_eq!(run.code, Some(0), "{}\n{}", run.stderr, run.log);
    assert_eq!(run.uploaded(), ["alpha", "beta"], "{}", run.log);
    assert!(
        !run.log.contains("MOCK: Recording scan failure"),
        "{}",
        run.log
    );
    // The service's types are degraded with today's words.
    assert!(run.log.contains("Type resolution failed: "), "{}", run.log);
    // The one addition: a line naming the classification.
    assert!(
        run.log
            .contains("The type sidecar died of memory (heap_limit"),
        "the classification is logged:\n{}",
        run.log
    );
}

/// carrick#1066 with carrick#2208 and carrick#3084: services analysed side by
/// side, and the type checker of the second dies of memory, every time,
/// while the first is still being analysed. A death beside another service
/// no longer stops the run: the third starts, and the second is analysed
/// again alone at the run's cap after the pass. It dies there too, alone at
/// the run's cap, and that stops the run with exit 3, naming it. The first
/// and the third, whose checkers lived, land.
#[test]
fn a_memory_death_in_one_of_several_workers_stops_the_run_with_exit_3() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = service_repo(tmp.path(), &["alpha", "beta", "gamma"]);
    let home = tmp.path().join("home");
    let died = tmp.path().join("died");
    sidecar_stand_in(&home, &died, true, BetaDies::Always);
    let run = typed_scan_of(
        &repo,
        &home,
        &died,
        true,
        &[("CARRICK_SERVICE_CONCURRENCY", "2")],
    );
    assert_eq!(run.code, Some(3), "{}\n{}", run.stderr, run.log);
    assert!(
        run.stderr
            .contains("types_out_of_memory: the type checker reached its heap limit")
            && run
                .stderr
                .contains("for service beta, under a heap cap of 512 MB"),
        "{}",
        run.stderr
    );
    assert_eq!(run.uploaded(), ["alpha", "gamma"], "{}", run.log);
    assert_eq!(
        run.log.matches("Analyzing service beta (2/3)").count(),
        2,
        "beta was analysed beside alpha, then alone:\n{}",
        run.log
    );
    assert!(
        run.log.contains(
            "Service beta died of memory beside another service; analysing it again alone at \
             512 MB"
        ),
        "{}",
        run.log
    );
}

/// Every blob in a local store, by service, without the fields that state
/// the run rather than the service: when it ran and what it measured.
fn stored_blobs(store: &Path) -> std::collections::BTreeMap<String, serde_json::Value> {
    std::fs::read_dir(store)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let mut blob: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
            let service = blob["service_name"].as_str()?.to_string();
            let object = blob.as_object_mut()?;
            object.remove("last_updated");
            object.remove("memory_peaks");
            Some((service, blob))
        })
        .collect()
}

/// carrick#3084: told to stop on a memory death, a run whose second service's
/// type checker dies of memory beside the first does not stop. The second is
/// analysed again alone after the pass, its process lives there, and every
/// service lands with its types and exit 0. What it lands is what a run one
/// at a time, in which its process lived, lands.
#[test]
fn a_memory_death_beside_another_service_is_analysed_again_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = service_repo(tmp.path(), &["alpha", "beta", "gamma"]);
    let home = tmp.path().join("home");
    let died = tmp.path().join("died");
    let store = tmp.path().join("store");
    sidecar_stand_in(&home, &died, true, BetaDies::Once);
    let run = typed_scan_into(
        &repo,
        &home,
        &died,
        true,
        &[("CARRICK_SERVICE_CONCURRENCY", "2")],
        Some(&store),
    );
    assert_eq!(run.code, Some(0), "{}\n{}", run.stderr, run.log);
    assert!(
        run.log.contains(
            "Service beta died of memory beside another service; analysing it again alone at \
             512 MB"
        ),
        "{}",
        run.log
    );
    let landed = stored_blobs(&store);
    assert_eq!(
        landed.keys().collect::<Vec<_>>(),
        ["alpha", "beta", "gamma"],
        "{}",
        run.log
    );
    for (service, blob) in &landed {
        assert!(
            blob.get("types_degraded").is_none_or(|v| v.is_null()),
            "{service} lost its types: {}\n{}",
            blob["types_degraded"],
            run.log
        );
    }

    // The same fixture one at a time, in a run whose beta lives.
    let one_home = tmp.path().join("one-home");
    let one_store = tmp.path().join("one-store");
    let lived = tmp.path().join("lived");
    std::fs::write(&lived, "written first, so beta lives").unwrap();
    sidecar_stand_in(&one_home, &lived, false, BetaDies::Once);
    let one = typed_scan_into(
        &repo,
        &one_home,
        &lived,
        true,
        &[("CARRICK_SERVICE_CONCURRENCY", "1")],
        Some(&one_store),
    );
    assert_eq!(one.code, Some(0), "{}\n{}", one.stderr, one.log);
    assert_eq!(
        serde_json::to_string_pretty(&landed["beta"]).unwrap(),
        serde_json::to_string_pretty(&stored_blobs(&one_store)["beta"]).unwrap()
    );
}

/// carrick#3084: a service whose previous scan stored a 1,000 MB peak starts
/// its sidecar with a 2,048 MB heap cap, under a run cap of 4,096 MB; one
/// with no stored peak starts at the run's cap.
#[test]
fn a_stored_peak_caps_the_service_s_sidecar() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = two_service_repo(tmp.path());
    let home = tmp.path().join("home");
    let lived = tmp.path().join("lived");
    let store = tmp.path().join("store");
    std::fs::write(&lived, "written first, so beta lives").unwrap();
    sidecar_stand_in(&home, &lived, false, BetaDies::Once);
    let run_cap = [("CARRICK_SIDECAR_MAX_OLD_SPACE_MB", "4096")];
    let first = typed_scan_into(&repo, &home, &lived, false, &run_cap, Some(&store));
    assert_eq!(first.code, Some(0), "{}\n{}", first.stderr, first.log);

    // Beta's stored peak, as a previous scan measured it; alpha's, none.
    for (service, peaks) in [
        ("beta", serde_json::json!({ "analysis_mb": 1000 })),
        ("alpha", serde_json::Value::Null),
    ] {
        let (path, mut blob) = stored_blob_file(&store, service);
        blob["memory_peaks"] = peaks;
        std::fs::write(&path, serde_json::to_string_pretty(&blob).unwrap()).unwrap();
    }
    std::fs::remove_file(home.join("argv")).unwrap();

    let second = typed_scan_into(&repo, &home, &lived, false, &run_cap, Some(&store));
    assert_eq!(second.code, Some(0), "{}\n{}", second.stderr, second.log);
    let argv = std::fs::read_to_string(home.join("argv")).unwrap();
    let flags_of = |service: &str| -> Vec<String> {
        argv.lines()
            .filter(|line| line.split('\t').next().unwrap_or("").ends_with(service))
            .map(|line| line.split('\t').nth(1).unwrap_or("").to_string())
            .collect()
    };
    // Every process of each, the one a capture restarts included.
    let (beta, alpha) = (flags_of("beta"), flags_of("alpha"));
    assert!(
        !beta.is_empty()
            && beta
                .iter()
                .all(|flags| flags == "--max-old-space-size=2048"),
        "{argv}\n{}",
        second.log
    );
    assert!(
        !alpha.is_empty()
            && alpha
                .iter()
                .all(|flags| flags == "--max-old-space-size=4096"),
        "{argv}"
    );
    assert!(
        second
            .log
            .contains("Service beta started: heap cap 2048 MB (stored peak 1000 MB)"),
        "{}",
        second.log
    );
}

/// The path and the parsed blob of `service` in a local store.
fn stored_blob_file(store: &Path, service: &str) -> (PathBuf, serde_json::Value) {
    std::fs::read_dir(store)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .find_map(|path| {
            let blob: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
            (blob["service_name"] == service).then_some((path, blob))
        })
        .unwrap_or_else(|| panic!("{service}'s blob is stored"))
}
