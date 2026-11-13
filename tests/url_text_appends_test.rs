//! `+=` of literal text on a URL local reads as the ways that reach it,
//! followed by the text (carrick#3679 S8, `flow.rs`). Runs the real scanner binary on `.` over a committed copy of
//! `tests/fixtures/url-text-appends`, offline, and reads the stored blob.
//!
//! Protocols: HTTP (the request summaries are HTTP-only).

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/url-text-appends")
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

/// The rows the blob holds at `src/<file>:line`, in stored order,
/// duplicates kept.
fn rows_in(blob: &serde_json::Value, file: &str, line: u32) -> Vec<String> {
    let at = format!("src/{file}:{line}");
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

fn rows_at(blob: &serde_json::Value, line: u32) -> Vec<String> {
    rows_in(blob, "appends.ts", line)
}

/// A query appended in a branch states the route with the query hole
/// stripped (R3a (a)).
#[test]
fn an_append_in_a_branch_keeps_the_route() {
    let blob = scan();
    assert_eq!(rows_at(&blob, 6), ["GET /api/notes"]);
}

/// An append that runs on every path replaces the text it extends.
#[test]
fn an_unconditional_append_replaces_the_text() {
    let blob = scan();
    assert_eq!(rows_at(&blob, 12), ["GET /api/items/all"]);
}

/// Appends and rewrites apply in the order they run.
#[test]
fn appends_follow_the_order_they_run_in() {
    let blob = scan();
    assert_eq!(
        rows_at(&blob, 19),
        ["GET /api/twice/a", "GET /api/twice/a/b"]
    );
    assert_eq!(rows_at(&blob, 26), ["GET /api/first", "GET /api/second"]);
}

/// Each branch appends to the way that reaches it: never `/x/y` (R3a (b)).
#[test]
fn an_append_in_each_branch_states_each_way_not_both() {
    let blob = scan();
    assert_eq!(rows_at(&blob, 68), ["GET /api/both/x", "GET /api/both/y"]);
}

/// An append inside an object method handed to a call states its route.
#[test]
fn an_append_in_an_object_method_handed_to_a_call_states_its_route() {
    let blob = scan();
    assert_eq!(
        rows_in(&blob, "provider.ts", 14),
        ["GET /api/catalog/entries"]
    );
}

/// Appends no reader places stay unread: a loop, a closure, a number, a
/// right side that is not literal text.
#[test]
fn appends_no_reader_places_stay_unread() {
    let blob = scan();
    for line in [34, 43, 61] {
        assert!(
            rows_at(&blob, line).is_empty(),
            "{line}: {:?}",
            rows_at(&blob, line)
        );
    }
    assert_eq!(rows_at(&blob, 55), ["GET /api/pages/:page"]);
}

/// `u = u + x` reads per way as `u += x` does (carrick#3986).
#[test]
fn a_local_extended_from_itself_reads_like_an_append() {
    let blob = scan();
    assert_eq!(rows_at(&blob, 49), ["GET /api/self"]);
}
