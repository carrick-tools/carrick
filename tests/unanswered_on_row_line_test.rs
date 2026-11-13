//! A site question nobody answered, on a line where the model's row stands,
//! is saved as an unread site (carrick#4123): the row is the model's answer,
//! so the slot the scan asked is still one the code did not settle. Runs the
//! real scanner binary on `.` over a committed copy of
//! `tests/fixtures/unanswered-on-row-line`, offline, and reads the stored
//! blob.
//!
//! Protocols: HTTP (the fixture's only question asks an HTTP method). The
//! fold that saves the site is protocol-blind.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unanswered-on-row-line")
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

/// The rows stated at `file:line` (a row's `file_path` carries its line).
fn rows_at(blob: &serde_json::Value, file: &str, line: u64) -> Vec<serde_json::Value> {
    let at = format!("{file}:{line}");
    blob["calls"]
        .as_array()
        .expect("calls written")
        .iter()
        .filter(|call| call["file_path"] == at.as_str())
        .cloned()
        .collect()
}

/// The unread sites saved at `file:line`.
fn unread_at(blob: &serde_json::Value, file: &str, line: u64) -> Vec<serde_json::Value> {
    blob["unread_sites"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|site| site["file"] == file && site["line"] == line)
        .cloned()
        .collect()
}

/// The method the code leaves open at `settings.ts:4` was asked and not
/// answered. The model's row stands on that line, and the site is saved as
/// unread with the slot it asked and why, not as a possible call.
#[test]
fn an_unanswered_question_on_a_model_row_line_is_an_unread_site() {
    let blob = scan();
    let rows = rows_at(&blob, "src/settings.ts", 4);
    assert_eq!(rows.len(), 1, "the model's row stands: {rows:#?}");
    assert_eq!(rows[0]["resolution_source"], "model", "{rows:#?}");
    let unread = unread_at(&blob, "src/settings.ts", 4);
    assert_eq!(
        unread.len(),
        1,
        "one unread site: {:#?}",
        blob["unread_sites"]
    );
    assert_eq!(unread[0]["reason"], "unanswered", "{unread:#?}");
    assert_eq!(unread[0]["slot"], "method", "{unread:#?}");
    let possible = blob["possible_calls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(possible.is_empty(), "no possible call: {possible:#?}");
}

/// Control: the code states `GET /api/ping` whole, so nothing was asked
/// there and nothing is saved as unread.
#[test]
fn a_call_the_code_states_saves_no_unread_site() {
    let blob = scan();
    let rows = rows_at(&blob, "src/settings.ts", 9);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0]["resolution_source"], "request_summary", "{rows:#?}");
    assert!(unread_at(&blob, "src/settings.ts", 9).is_empty());
}
