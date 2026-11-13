//! A request wrapper whose options run through a helper that returns
//! `{ ...init, key: value }` (carrick#1561 C11, the `new_url_target` rule's
//! deletion): the request summaries read the method its callers write
//! through that spread, so a `new URL('/path', origin)` handed to the
//! wrapper states its path at each caller, host-free. Runs the real scanner
//! binary on `.` over a committed copy of
//! `tests/fixtures/model-row-runtime-base`, offline, and reads the stored
//! blob.
//!
//! Protocols: HTTP (the request summaries are HTTP-only).

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/model-row-runtime-base")
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "carrick-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "carrick-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// The one blob the scan stored; the fixture is scanned once per run.
fn scan() -> serde_json::Value {
    static BLOB: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    BLOB.get_or_init(scan_once).clone()
}

fn scan_once() -> serde_json::Value {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir(), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let cassettes = repo.join("__llm__");
    let storage = tmp.path().join("storage");
    std::fs::create_dir_all(&storage).unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(&repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", &storage)
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", tmp.path().join("cache"))
        .env("HOME", &home)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1");
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
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut blobs: Vec<serde_json::Value> = Vec::new();
    for entry in std::fs::read_dir(&storage).expect("the store reads") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|ext| ext == "json") {
            blobs.push(serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap());
        }
    }
    assert_eq!(blobs.len(), 1, "the scan stored one blob");
    blobs.remove(0)
}

/// The graph row at `src/client.ts:line`.
fn graph_row(blob: &serde_json::Value, line: u64) -> serde_json::Value {
    let at = format!("src/client.ts:{line}");
    blob["mount_graph"]["data_calls"]
        .as_array()
        .expect("graph calls written")
        .iter()
        .find(|call| {
            call["file_location"]
                .as_str()
                .is_some_and(|f| f.ends_with(&at))
        })
        .cloned()
        .unwrap_or_else(|| panic!("no graph row at {at}"))
}

/// The model's bare path at a call whose base the code read as a runtime
/// value states no base; a bare path the code reads states `relative`, and
/// the template twin states its injected base.
#[test]
fn a_model_row_over_an_unread_base_is_not_relative() {
    let blob = scan();
    let unread = graph_row(&blob, 6);
    assert_eq!(unread["target_url"], "/things/list", "{unread:#}");
    assert!(unread["base"].is_null(), "{unread:#}");
    let twin = graph_row(&blob, 11);
    assert_eq!(twin["base"]["kind"], "injected", "{twin:#}");
    let relative = graph_row(&blob, 16);
    assert_eq!(relative["base"]["kind"], "relative", "{relative:#}");
}
