//! carrick#2432 slice A: a call whose callee is a value (a parameter, a key
//! of one, a parameter's default, a local or an inline `a ?? b`, a function
//! an in-repo factory returns) is followed through what holds it, and its
//! request is a site row with its method and path.
//!
//! The fixture (`tests/fixtures/injected-callee/`) is one root service,
//! scanned as the Action scans it (`carrick .` from the repo root) with the
//! compiler rows and site questions on, models mocked, and the store's
//! label for the default library's `fetch` from its cassette. Each case is
//! a file of `src/`, described where it is written.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
        }
    }
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

/// The fixture scanned as the Action scans it; its stored blob.
fn scan() -> serde_json::Value {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("injected-callee");
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/injected-callee"),
        &repo,
    );
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let store = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(&repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", store.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env(
            "CARRICK_SIDECAR_DIR",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar"),
        );
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        "CARRICK_NO_MODEL",
        "CARRICK_ALLOW_MISSING_TYPES",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::read_dir(store.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap())
        .expect("the scan stored a blob")
}

/// Each call row of the blob as `file:line METHOD path`.
fn call_rows(blob: &serde_json::Value) -> Vec<String> {
    let mut rows: Vec<String> = blob["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|call| {
            format!(
                "{} {} {}",
                call["file_path"].as_str().unwrap_or_default(),
                call["key"]["method"].as_str().unwrap_or_default(),
                call["key"]["path"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    rows.sort();
    rows
}

/// Each possible call of the blob as `file:line reason`.
fn possible_calls(blob: &serde_json::Value) -> Vec<String> {
    let mut rows: Vec<String> = blob["possible_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|call| {
            format!(
                "{}:{} {}",
                call["file_path"].as_str().unwrap_or_default(),
                call["line_number"],
                call["unread"]["reason"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    rows.sort();
    rows
}

#[test]
fn a_callee_that_is_a_value_is_followed_to_its_request() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    if !script.exists() {
        assert!(
            std::env::var("CI").is_err(),
            "the built sidecar is missing in CI"
        );
        eprintln!("Skipping: the built sidecar is missing");
        return;
    }
    let blob = scan();
    let rows = call_rows(&blob);
    let leads = possible_calls(&blob);
    let has = |row: &str| rows.iter().any(|known| known == row);
    let mut missing = Vec::new();
    for (case, row) in [
        (
            "1 a destructured default",
            "src/events.ts:4 POST /api/events",
        ),
        (
            "2 a field filled two calls up",
            "src/sync.ts:7 PUT /api/sync",
        ),
        ("3 a local `x || fetch`", "src/mail.ts:8 POST /api/mail"),
        (
            "4 a factory's returned function",
            "src/items.ts:8 GET /api/items",
        ),
        (
            "5 a parameter invoked in a nested function",
            "src/health-check.ts:4 HEAD /api/health",
        ),
        (
            "6 the caller that hands `fetch`",
            "src/load.ts:12 GET /api/live",
        ),
        (
            "8 an inline `a ?? fetch`",
            "src/connection.ts:8 GET /api/status",
        ),
    ] {
        if !has(row) {
            missing.push(format!("case {case}: no row `{row}`"));
        }
    }
    assert!(
        missing.is_empty(),
        "{missing:#?}\nrows: {rows:#?}\npossible calls: {leads:#?}"
    );
    assert!(
        leads
            .iter()
            .any(|lead| lead == "src/load.ts:4 several_values"),
        "case 6: values that disagree are a possible call\n{leads:#?}"
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("/api/never") || row.contains("/api/replay")),
        "case 7 and the value that sends nothing state no row\n{rows:#?}"
    );
}
