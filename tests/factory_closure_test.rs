//! carrick#3504 S0b: a function a factory returns, called where the
//! factory's result is held, reads the factory's parameters from the call
//! that made it, and its request is a row at that call.
//!
//! The fixture (`tests/fixtures/factory-closure/`) is scanned as the Action
//! scans it (`carrick .` from the repo root, which lists one service), with
//! models mocked and the store's labels for `@fixture/send` and
//! `@fixture/pub` from its cassette. The answer key is the fixture's README.

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

/// The fixture scanned as the Action scans it; every blob it stored.
fn scan() -> Vec<serde_json::Value> {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/factory-closure");
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("factory-closure");
    copy_dir(&fixture.join("repo"), &repo);
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
            format!("{}/", fixture.join("__llm__").display()),
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
    let blobs: Vec<serde_json::Value> = fs::read_dir(store.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap())
        .collect();
    assert!(!blobs.is_empty(), "the scan stored a blob");
    blobs
}

/// Each call row of the blobs as `file:line METHOD path source`.
fn call_rows(blobs: &[serde_json::Value]) -> Vec<String> {
    let mut rows: Vec<String> = blobs
        .iter()
        .flat_map(|blob| blob["calls"].as_array().into_iter().flatten())
        .map(|call| {
            format!(
                "{} {} {} {}",
                call["file_path"].as_str().unwrap_or_default(),
                call["key"]["method"].as_str().unwrap_or_default(),
                call["key"]["path"].as_str().unwrap_or_default(),
                call["source"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    rows.sort();
    rows
}

/// Each possible call of the blobs as `file:line reason`.
fn possible_calls(blobs: &[serde_json::Value]) -> Vec<String> {
    let mut rows: Vec<String> = blobs
        .iter()
        .flat_map(|blob| blob["possible_calls"].as_array().into_iter().flatten())
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
fn a_closure_over_a_factory_parameter_states_its_row_at_the_caller() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    if !script.exists() {
        assert!(
            std::env::var("CI").is_err(),
            "the built sidecar is missing in CI"
        );
        eprintln!("Skipping: the built sidecar is missing");
        return;
    }
    let blobs = scan();
    let rows = call_rows(&blobs);
    let leads = possible_calls(&blobs);
    let mut wrong = Vec::new();
    for (at, path) in [
        ("s-closure-coalesce.ts:8", "/p/s-closure-coalesce"),
        ("s-closure-param.ts:7", "/p/s-closure-param"),
        ("s-closure-param-key.ts:7", "/p/s-closure-param-key"),
        ("s-two-level.ts:17", "/p/s-two-level"),
    ] {
        let found = rows
            .iter()
            .any(|row| row.contains(&format!("src/{at} POST {path}")));
        if !found {
            wrong.push(format!("no POST row `{path}` at {at}"));
        }
        if let Some(lead) = leads
            .iter()
            .find(|lead| lead.contains(&format!("src/{at} injected_callee")))
        {
            wrong.push(format!("a lead at {at}: {lead}"));
        }
    }
    // The two-level shape with the path holding the nested function's own
    // parameter: the callee decides, so what is left is the name
    // (carrick#3372), not the callee.
    if leads
        .iter()
        .any(|lead| lead.ends_with("src/t-real.ts:26 injected_callee"))
    {
        wrong.push("t-real.ts:26 is still an injected callee".to_string());
    }
    // The maker call fills `send` with a function that sends nothing.
    if rows
        .iter()
        .any(|row| row.contains("/p/s-closure-coalesce-filled"))
    {
        wrong.push("s-closure-coalesce-filled.ts:9 states a row".to_string());
    }
    assert!(
        wrong.is_empty(),
        "{wrong:#?}\nrows: {rows:#?}\npossible calls: {leads:#?}"
    );
}
