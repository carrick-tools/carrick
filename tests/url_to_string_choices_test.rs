//! A URL a conditional chooses, sent through `.toString()`, reads as the
//! choice. Runs the real scanner binary on `.` over a committed copy of
//! `tests/fixtures/url-to-string-choices`, offline, and reads the stored blob.
//!
//! Protocols: HTTP (the request summaries are HTTP-only).

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/url-to-string-choices")
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

/// The rows the blob holds at `src/choices.ts:line`, in stored order,
/// duplicates kept.
fn rows_at(blob: &serde_json::Value, line: u32) -> Vec<String> {
    let at = format!("src/choices.ts:{line}");
    blob["calls"]
        .as_array()
        .expect("calls written")
        .iter()
        .filter(|call| call["file_path"] == at.as_str())
        .map(|call| {
            format!(
                "{} {}",
                call["key"]["method"].as_str().unwrap_or(""),
                call["key"]["path"].as_str().unwrap_or("")
            )
        })
        .collect()
}

fn leads_at(blob: &serde_json::Value, line: u32) -> Vec<serde_json::Value> {
    blob["possible_calls"]
        .as_array()
        .map(|leads| {
            leads
                .iter()
                .filter(|lead| lead["file_path"] == "src/choices.ts" && lead["line_number"] == line)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// A query choice sent through `.toString()` states its path once.
#[test]
fn a_query_choice_sent_through_to_string_states_its_route() {
    let blob = scan();
    assert_eq!(rows_at(&blob, 3), ["GET /api/s1"]);
    assert!(leads_at(&blob, 3).is_empty(), "{:#?}", leads_at(&blob, 3));
}

/// A path choice sent through `.toString()` states each way.
#[test]
fn a_path_choice_sent_through_to_string_states_each_route() {
    let blob = scan();
    assert_eq!(rows_at(&blob, 8), ["GET /api/s2/one", "GET /api/s2/two"]);
    assert!(leads_at(&blob, 8).is_empty(), "{:#?}", leads_at(&blob, 8));
}

/// The forms that read before this change keep their rows, no more and no fewer.
#[test]
fn forms_read_today_keep_their_rows() {
    let blob = scan();
    assert_eq!(rows_at(&blob, 12), ["GET /api/s3/one", "GET /api/s3/two"]);
    assert_eq!(rows_at(&blob, 17), ["GET /api/s4/:id"]);
    assert_eq!(rows_at(&blob, 22), ["GET /api/s5"]);
}
