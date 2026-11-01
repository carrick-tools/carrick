//! carrick#1491 through the whole scanner, mocked ($0, no model).
//!
//! `tests/fixtures/retype-http-client` is a two-service monorepo: `api`
//! answers `POST /checkout` with a `CheckoutResult`, and `web` calls it twice
//! through a generic client instance, once with a type argument and once
//! without, reading `response.data.x` after each. Neither call publishes a
//! comparable type, so before the retype check both pairs went unverified and
//! a read of a field the producer does not return went unflagged.
//!
//! The scan must flag both reads when the producer returns `{ y }`, and flag
//! neither when it returns `{ x }`. It must also say, in its log, why each
//! pair it could not verify is unverified.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/retype-http-client")
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create the destination");
    for entry in std::fs::read_dir(from).expect("read the fixture") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy a fixture file");
        }
    }
}

/// Scan `root` with the cassettes in its `__llm__`; returns the projection
/// and the scanner's combined log.
fn scan(root: &Path) -> (serde_json::Value, String) {
    scan_with(root, &[])
}

/// [`scan`] with `env` set for the scanner.
fn scan_with(root: &Path, env: &[(&str, &str)]) -> (serde_json::Value, String) {
    let cache = tempfile::tempdir().expect("temp cache dir");
    let cassettes = root.join("__llm__");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.envs(env.iter().copied());
    cmd.arg(root)
        .env("CARRICK_LOCAL_STORAGE_DIR", cache.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.display()),
        )
        .env("CARRICK_OUTPUT_JSON", "1");
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("failed to spawn carrick");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "scan failed:\n{stderr}");
    let start = stdout
        .find("\n{")
        .map(|i| i + 1)
        .or_else(|| stdout.starts_with('{').then_some(0))
        .unwrap_or_else(|| panic!("no JSON in scanner stdout:\n{stdout}"));
    let projection = serde_json::from_str(&stdout[start..])
        .unwrap_or_else(|e| panic!("projection parse failed: {e}\n{stdout}"));
    (projection, format!("{stdout}\n{stderr}"))
}

fn checkout_matches(projection: &serde_json::Value) -> Vec<&serde_json::Value> {
    projection["cross_repo_matches"]
        .as_array()
        .expect("projection has no `cross_repo_matches` array")
        .iter()
        .filter(|m| m["producer_key"].as_str() == Some("http|POST|/checkout"))
        .collect()
}

fn sidecar_built() -> bool {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/sidecar/dist/src/index.js")
        .exists()
}

#[test]
fn a_read_of_a_field_the_producer_does_not_return_is_flagged() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let (projection, log) = scan(&fixture_dir());

    let matches = checkout_matches(&projection);
    assert_eq!(
        matches.len(),
        3,
        "all three calls match the route: {matches:#?}"
    );
    let mut reasons: Vec<&str> = matches
        .iter()
        .filter_map(|m| m["mismatch_reason"].as_str())
        .collect();
    reasons.sort();
    assert_eq!(reasons.len(), 2, "the two reads are flagged: {matches:#?}");
    // The typed call reads at line 7, the untyped one at line 12.
    for (reason, line) in reasons.iter().zip([12, 7]) {
        assert!(
            reason.contains(&format!("web/src/checkout.ts:{line}:"))
                && reason.contains("Property 'x' does not exist on type '{ y: number; }'"),
            "the read is named at its line: {reason}"
        );
    }

    // The discarded response is the one pair left unverified, and the log
    // says why. The request halves state no body on either side and are not
    // logged: a passing scan's log carries only what is worth reading.
    let unverified: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("Types not verified:"))
        .collect();
    assert_eq!(unverified.len(), 1, "{unverified:#?}");
    assert!(
        unverified[0]
            .contains("POST /checkout response (web/src/checkout.ts:16 in web against api):")
            && unverified[0].contains("the consumer never reads the response"),
        "{}",
        unverified[0]
    );
}

#[test]
fn a_read_of_a_field_the_producer_returns_is_not_flagged() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("retype-http-client");
    copy_tree(&fixture_dir(), &root);
    let routes = root.join("api/src/routes.ts");
    let source = std::fs::read_to_string(&routes).expect("read routes");
    let renamed = source
        .replace("y: number;", "x: number;")
        .replace("{ y: 1 }", "{ x: 1 }");
    assert_ne!(source, renamed, "the fixture's producer shape moved");
    std::fs::write(&routes, renamed).expect("write routes");

    let (projection, log) = scan(&root);
    let matches = checkout_matches(&projection);
    assert_eq!(matches.len(), 3, "{matches:#?}");
    for m in matches {
        assert_ne!(m["type_compatible"], false, "nothing to flag: {m:#}");
        assert!(m["mismatch_reason"].is_null(), "{m:#}");
    }
    for line in [6, 11] {
        assert!(
            !log.contains(&format!("(web/src/checkout.ts:{line} in web")),
            "the reads at line {line} are now a fact:\n{log}"
        );
    }
}

/// carrick#1996: the retype check sends one request per consumer file, the
/// first to the scan's own sidecar and the rest to a pool of processes. Here
/// `web` calls the route from four files of three calls each. A scan forced
/// to three processes publishes every match exactly as a scan forced to one
/// does, and its log says three ran.
#[test]
fn a_pool_of_three_processes_judges_every_call_as_one_process_does() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("retype-http-client");
    copy_tree(&fixture_dir(), &root);
    let calls = std::fs::read_to_string(root.join("web/src/checkout.ts")).expect("read calls");
    let answer = std::fs::read_to_string(root.join("__llm__/analyze-file/checkout.json"))
        .expect("read the cassette");
    for stem in ["basket", "orders", "refunds"] {
        std::fs::write(root.join(format!("web/src/{stem}.ts")), &calls).expect("write calls");
        std::fs::write(
            root.join(format!("__llm__/analyze-file/{stem}.json")),
            &answer,
        )
        .expect("write the cassette");
    }

    let sorted = |projection: &serde_json::Value| {
        let mut rows: Vec<String> = checkout_matches(projection)
            .into_iter()
            .map(|m| m.to_string())
            .collect();
        rows.sort();
        rows
    };
    let (one, one_log) = scan_with(&root, &[("CARRICK_SIDECAR_POOL", "1")]);
    let (three, three_log) = scan_with(&root, &[("CARRICK_SIDECAR_POOL", "3")]);

    let rows = sorted(&one);
    assert_eq!(rows.len(), 12, "every call matches the route: {rows:#?}");
    let flagged = rows
        .iter()
        .filter(|row| row.contains("Property 'x' does not exist"))
        .count();
    assert_eq!(flagged, 8, "both reads of each file are flagged: {rows:#?}");
    assert_eq!(
        sorted(&three),
        rows,
        "three processes publish what one does"
    );

    let pool_line = |log: &str| {
        log.lines()
            .find(|line| line.contains("Retyping web's consumer calls: 12 call(s) in 4 file(s)"))
            .map(str::to_string)
    };
    assert!(
        pool_line(&three_log)
            .is_some_and(|line| line.contains("; 3 process(es) for the 3 after the first")),
        "three processes took the files after the first: {:?}",
        pool_line(&three_log)
    );
    assert!(
        pool_line(&one_log)
            .is_some_and(|line| line.contains("; 1 process(es) for the 3 after the first")),
        "one process took them all: {:?}",
        pool_line(&one_log)
    );
}

/// Run the binary over `repo` against the store at `store`, offline, with
/// `flags`. Returns the scan's combined output.
fn scan_stored(repo: &Path, store: &Path, flags: &[&str], env: &[(&str, &Path)]) -> String {
    let home = store.with_file_name("home");
    let _ = std::fs::remove_dir_all(&home);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(repo)
        .args(["--repo", "fixture/retype"])
        .args(flags)
        .env("HOME", &home)
        .env("CARRICK_LOCAL_STORAGE_DIR", store)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_ALLOW_UNPREPARED", "1")
        .env("CARRICK_NO_UPDATE_CHECK", "1");
    for (name, value) in env {
        cmd.env(name, value);
    }
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_EVENT_PATH",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        "CARRICK_OUTPUT_JSON",
        "CARRICK_LOCAL_STORAGE_ISOLATE",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("failed to spawn carrick");
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "scan {flags:?} failed:\n{log}");
    log
}

fn git_in(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git failed to spawn");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// The fixture in `dir`, made so `--changed-only` can carry both services:
/// `--changed-only` analyses every service with an import it cannot
/// resolve, and the fixture's libraries are ambient `declare module`
/// blocks.
fn carriable_fixture(dir: &Path) -> PathBuf {
    let root = dir
        .canonicalize()
        .expect("canonical temp dir")
        .join("retype-http-client");
    copy_tree(&fixture_dir(), &root);
    // `--changed-only` analyses every service with an import it cannot
    // resolve, and the fixture's libraries are ambient `declare module`
    // blocks. Each becomes a module file beside its importer, imported by a
    // relative path on the same line, so both services can be carried.
    for (dir, module) in [("web", "http-client"), ("api", "http-server")] {
        let ambient = root.join(format!("{dir}/src/{module}.d.ts"));
        let text = std::fs::read_to_string(&ambient).expect("read the declarations");
        let body: Vec<&str> = text.trim_end().lines().collect();
        let body = body[1..body.len() - 1]
            .iter()
            .map(|line| line.strip_prefix("  ").unwrap_or(line))
            .map(|line| line.replace("export function", "export declare function"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::remove_file(&ambient).expect("remove the ambient module");
        std::fs::write(root.join(format!("{dir}/src/{module}.ts")), body + "\n")
            .expect("write the module");
        std::fs::write(
            root.join(format!("__llm__/analyze-file/{module}.json")),
            r#"{ "mounts": [], "endpoints": [], "data_calls": [] }"#,
        )
        .expect("write the cassette");
        let importer = root.join(if dir == "web" {
            "web/src/checkout.ts"
        } else {
            "api/src/routes.ts"
        });
        let source = std::fs::read_to_string(&importer).expect("read the importer");
        let relative = source.replace(&format!("\"{module}\""), &format!("\"./{module}\""));
        assert_ne!(source, relative, "{module} is imported by name");
        std::fs::write(&importer, relative).expect("write the importer");
    }
    root
}

/// carrick#2567: `--changed-only` carries a consumer whose files did not
/// change, and its untyped calls are judged by retyping them, as a full scan
/// judges them. Commit A has `api` answer `{ x }`, which `web` reads; both
/// are stored at A. Commit B drops `x`. A `--pr` run of B with
/// `--changed-only` analyses `api` alone and carries `web`, and it posts
/// the findings and the checked-pair counts a full `--pr` run posts.
#[test]
fn a_carried_consumer_is_retyped_as_a_full_scan_retypes_it() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let root = carriable_fixture(dir.path());
    let routes = root.join("api/src/routes.ts");
    let dropped = std::fs::read_to_string(&routes).expect("read routes");
    let returned = dropped
        .replace("y: number;", "x: number;")
        .replace("{ y: 1 }", "{ x: 1 }");
    assert_ne!(dropped, returned, "the fixture's producer shape moved");
    std::fs::write(&routes, &returned).expect("write routes");
    git_in(&root, &["init", "-q", "-b", "main"]);
    git_in(&root, &["add", "-A"]);
    git_in(&root, &["commit", "-q", "-m", "A"]);
    let a = git_in(&root, &["rev-parse", "HEAD"]);
    let store = dir.path().join("store");
    std::fs::create_dir_all(&store).expect("create the store");
    scan_stored(&root, &store, &[], &[]);

    std::fs::write(&routes, &dropped).expect("write routes");
    git_in(&root, &["commit", "-q", "-am", "B: api drops x"]);
    let b = git_in(&root, &["rev-parse", "HEAD"]);

    let pr = |changed_only: bool| -> (serde_json::Value, String) {
        let out = dir.path().join(format!("pr-{changed_only}.json"));
        let mut flags = vec!["--pr", "7", "--pr-head", &b, "--pr-base", &a];
        if changed_only {
            flags.push("--changed-only");
        }
        let log = scan_stored(&root, &store, &flags, &[("CARRICK_PR_RESULT_OUT", &out)]);
        let posted = std::fs::read_to_string(&out)
            .unwrap_or_else(|error| panic!("the PR run posted no result ({error}):\n{log}"));
        (
            serde_json::from_str(&posted).expect("parse the PR result"),
            log,
        )
    };
    let (full, _) = pr(false);
    let (scoped, scoped_log) = pr(true);

    assert!(
        scoped_log.contains("scope: web carried"),
        "the scoped run carries web:\n{scoped_log}"
    );
    let mismatches = |payload: &serde_json::Value| -> Vec<String> {
        let mut sites: Vec<String> = payload["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .filter(|f| f["kind"] == "type_mismatch")
            .map(|f| f["call_sites"][0].as_str().unwrap_or_default().to_string())
            .collect();
        sites.sort();
        sites
    };
    assert_eq!(
        mismatches(&full),
        ["web/src/checkout.ts:11", "web/src/checkout.ts:6"],
        "the full run flags both reads by retyping them: {full:#}"
    );
    assert_eq!(
        scoped["findings"], full["findings"],
        "the scoped run posts the full run's findings"
    );
    assert_eq!(
        scoped["checked"]["pairs"], full["checked"]["pairs"],
        "the scoped run counts the pairs the full run counts"
    );
}

/// carrick#3104: a pull request whose change reaches neither service takes
/// main's stored outcome for every pair, so nothing is judged or retyped,
/// and it posts what the same run posts with the reach decision off, which
/// judges and retypes every pair. Main's outcomes include the two reads only
/// the retype finds.
#[test]
fn a_pr_that_reaches_no_pair_posts_mains_stored_outcomes() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let root = carriable_fixture(dir.path());
    git_in(&root, &["init", "-q", "-b", "main"]);
    git_in(&root, &["add", "-A"]);
    git_in(&root, &["commit", "-q", "-m", "A"]);
    let a = git_in(&root, &["rev-parse", "HEAD"]);
    let store = dir.path().join("store");
    std::fs::create_dir_all(&store).expect("create the store");
    scan_stored(&root, &store, &[], &[]);

    std::fs::write(root.join("NOTES.txt"), "unscanned\n").expect("write notes");
    git_in(&root, &["add", "NOTES.txt"]);
    git_in(&root, &["commit", "-q", "-m", "B: notes"]);
    let b = git_in(&root, &["rev-parse", "HEAD"]);

    let pr = |off: bool| -> (serde_json::Value, String) {
        let out = dir.path().join(format!("pr-{off}.json"));
        let flags = [
            "--pr",
            "7",
            "--pr-head",
            &b,
            "--pr-base",
            &a,
            "--changed-only",
        ];
        let mut env: Vec<(&str, &Path)> = vec![("CARRICK_PR_RESULT_OUT", &out)];
        if off {
            env.push(("CARRICK_TYPE_REACH", Path::new("off")));
        }
        let log = scan_stored(&root, &store, &flags, &env);
        let posted = std::fs::read_to_string(&out)
            .unwrap_or_else(|error| panic!("the PR run posted no result ({error}):\n{log}"));
        (
            serde_json::from_str(&posted).expect("parse the PR result"),
            log,
        )
    };
    let (judged, judged_log) = pr(true);
    let (settled, settled_log) = pr(false);

    let settled_line = settled_log
        .lines()
        .find(|line| line.contains("pairs settled from main"))
        .unwrap_or_else(|| panic!("no settle line:\n{settled_log}"));
    eprintln!("{settled_line}");
    let counts: Vec<usize> = settled_line
        .split_whitespace()
        .filter_map(|word| word.parse().ok())
        .collect();
    assert!(
        counts.len() == 2 && counts[0] > 0 && counts[0] == counts[1],
        "every pair settles: {settled_line}"
    );
    assert!(
        !settled_log.contains("Retyped web's consumer calls"),
        "nothing is retyped:\n{settled_log}"
    );
    assert!(
        judged_log.contains("Retyped web's consumer calls"),
        "the control retypes:\n{judged_log}"
    );
    assert!(
        !judged_log.contains("pairs settled from main"),
        "{judged_log}"
    );

    let mismatches = |payload: &serde_json::Value| -> Vec<String> {
        let mut sites: Vec<String> = payload["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .filter(|f| f["kind"] == "type_mismatch")
            .map(|f| f["call_sites"][0].as_str().unwrap_or_default().to_string())
            .collect();
        sites.sort();
        sites
    };
    assert_eq!(
        mismatches(&settled),
        ["web/src/checkout.ts:11", "web/src/checkout.ts:6"],
        "main's retyped reads are posted: {settled:#}"
    );
    assert_eq!(settled["findings"], judged["findings"]);
    assert_eq!(settled["checked"], judged["checked"]);
}
