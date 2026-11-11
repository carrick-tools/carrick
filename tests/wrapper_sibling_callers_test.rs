//! Callers of an own wrapper keep their rows when a sibling caller of the
//! same request has a summary row (C11 T1). A site yields to the summaries
//! only where a summary row's chain passes through the site's own line
//! (carrick#2524's case); a caller the summaries could not compose keeps the
//! row the site path stated. Runs the real scanner binary on `.` over a
//! committed copy of `tests/fixtures/wrapper-sibling-callers`, offline, and
//! reads the stored blob.
//!
//! Protocols: HTTP (the yield reads only `http_client` sites).

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wrapper-sibling-callers")
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

/// Every HTTP call row in `file`, as `(line, method, path)`, sorted.
fn rows_in(blob: &serde_json::Value, file: &str) -> Vec<(u64, String, String)> {
    let mut rows: Vec<(u64, String, String)> = blob["calls"]
        .as_array()
        .expect("calls written")
        .iter()
        .filter(|call| call["key"]["protocol"] == "http")
        .filter_map(|call| {
            let (path, line) = call["file_path"].as_str()?.rsplit_once(':')?;
            path.ends_with(file).then(|| {
                (
                    line.parse().unwrap_or(0),
                    call["key"]["method"].as_str().unwrap_or("").to_string(),
                    call["key"]["path"].as_str().unwrap_or("").to_string(),
                )
            })
        })
        .collect();
    rows.sort();
    rows
}

/// One caller of `send` has a summary row; the two callers of `sendBytes`,
/// which the summaries do not compose, keep the rows the site path states.
#[test]
fn callers_of_an_outer_wrapper_keep_their_rows_beside_a_summary_row() {
    let blob = scan();
    let paths: Vec<(String, String)> = rows_in(&blob, "src/items.ts")
        .into_iter()
        .map(|(_, method, path)| (method, path))
        .collect();
    assert_eq!(
        paths,
        vec![
            ("GET".to_string(), "/api/items/:id/one".to_string()),
            ("GET".to_string(), "/api/items/:id/two".to_string()),
            ("GET".to_string(), "/api/items/three".to_string()),
        ],
        "{:#?}",
        blob["calls"]
    );
}

/// #2524's shape: the summaries place the request's row at the wrapper's
/// caller, up the site's own chain, and the site at the request yields.
/// One request, one row.
#[test]
fn a_site_whose_request_a_summary_places_up_its_chain_still_yields() {
    let blob = scan();
    let rows = rows_in(&blob, "src/notices.ts");
    assert_eq!(
        rows,
        vec![(6, "GET".to_string(), "/api/notices".to_string())],
        "{:#?}",
        blob["calls"]
    );
}

/// The site stops at a wrapper's call (`page('/api/listing', query)`, where
/// the path is complete) and the summaries carry the request one caller up,
/// where the query is filled: the row's chain passes through the site's
/// line, so the site yields. One request, one row, at the outer caller.
#[test]
fn a_site_at_a_wrapper_call_yields_to_the_row_composed_through_it() {
    let blob = scan();
    let rows = rows_in(&blob, "src/listing.ts");
    assert_eq!(
        rows,
        vec![(10, "GET".to_string(), "/api/listing".to_string())],
        "{:#?}",
        blob["calls"]
    );
}
