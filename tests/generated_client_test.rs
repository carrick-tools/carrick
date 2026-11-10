//! carrick#2448: a REST client written into the repository, in the shape an
//! OpenAPI generator writes. A call through an object an in-repo factory
//! returns enters the function at that key (T1), and its request is a row
//! at the call.
//!
//! The fixture (`tests/fixtures/generated-client/`) is scanned as the Action
//! scans it (`carrick .` from the repo root, which lists one service), with
//! models mocked and the store's label for the language library's `fetch`
//! from its cassette. The answer key is the fixture's README.

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
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/generated-client");
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("generated-client");
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
                call["resolution_source"].as_str().unwrap_or_default(),
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

/// Each unfollowed member call of the blobs as `file:line`.
fn unfollowed_member_calls(blobs: &[serde_json::Value]) -> Vec<String> {
    let mut rows: Vec<String> = blobs
        .iter()
        .flat_map(|blob| {
            blob["unfollowed_member_calls"]
                .as_array()
                .into_iter()
                .flatten()
        })
        .map(|row| {
            format!(
                "{}:{}",
                row["file"].as_str().unwrap_or_default(),
                row["line"]
            )
        })
        .collect();
    rows.sort();
    rows
}

/// carrick#2448 T1: a call through the object a factory returns is a site at
/// its own line, a row from the store's fact, with no lead at the arrow that
/// sends and no unfollowed member call at the call.
#[test]
fn a_generated_style_client_call_is_a_site_at_its_own_line() {
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
    let unfollowed = unfollowed_member_calls(&blobs);
    let mut wrong = Vec::new();
    for (at, method, path) in [
        ("r1/sdk.ts:5", "GET", "/r1/items"),
        ("r1/sdk.ts:8", "POST", "/r1/items"),
        ("r1/sdk.ts:10", "GET", "/r1/direct"),
    ] {
        let at_line: Vec<&String> = rows
            .iter()
            .filter(|row| row.starts_with(&format!("src/{at} ")))
            .collect();
        match at_line[..] {
            [row]
                if row.ends_with(" request_summary")
                    && row.contains(&format!(" {method} {path} ")) => {}
            _ => wrong.push(format!(
                "want one {method} {path} fact row at {at}: {at_line:?}"
            )),
        }
    }
    for lead in &leads {
        if lead.starts_with("src/r1/client.ts:10 ") || lead.starts_with("src/r1/client.ts:11 ") {
            wrong.push(format!("a lead at the arrow that sends: {lead}"));
        }
    }
    if unfollowed.iter().any(|row| row == "src/r1/sdk.ts:10") {
        wrong.push("r1/sdk.ts:10 is still an unfollowed member call".to_string());
    }
    // The client written through reads as before: no fact row from the arrow.
    if rows
        .iter()
        .any(|row| row.starts_with("src/n1/escaped.ts:11 ") && row.ends_with(" request_summary"))
    {
        wrong.push("n1/escaped.ts:11 states a fact row".to_string());
    }
    assert!(
        wrong.is_empty(),
        "{wrong:#?}\nrows: {rows:#?}\npossible calls: {leads:#?}\nunfollowed: {unfollowed:#?}"
    );
}

/// The rows the scan stores at `at` (`file:line`, under `src/`).
fn rows_at<'a>(rows: &'a [String], at: &str) -> Vec<&'a String> {
    rows.iter()
        .filter(|row| row.starts_with(&format!("src/{at} ")))
        .collect()
}

/// Whether the built sidecar is present; outside CI a missing one skips.
fn sidecar_built() -> bool {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    if script.exists() {
        return true;
    }
    assert!(
        std::env::var("CI").is_err(),
        "the built sidecar is missing in CI"
    );
    eprintln!("Skipping: the built sidecar is missing");
    false
}

/// carrick#2448 T4: the faithful generated shape. Each SDK function hands
/// `{ url: '…', ...options }` to a client method whose walk reaches the
/// language library's `fetch`, and the URL the send is handed is built by
/// string operations no composition reads. The carried request descriptor
/// states the row at the SDK call: one row per call, from the fact layer,
/// with the method and path the `request_spec` rule states today.
#[test]
fn the_generated_shape_states_one_fact_row_per_sdk_call() {
    if !sidecar_built() {
        return;
    }
    let blobs = scan();
    let rows = call_rows(&blobs);
    let mut wrong = Vec::new();
    for (at, method, path) in [
        ("r3/sdk.ts:11", "GET", "/api/v1/widgets"),
        ("r3/sdk.ts:18", "GET", "/api/v1/widgets/:id"),
        ("r3/sdk.ts:25", "POST", "/api/v1/widgets"),
    ] {
        let at_line = rows_at(&rows, at);
        match at_line[..] {
            [row]
                if row.ends_with(" request_summary")
                    && row.contains(&format!(" {method} {path} ")) => {}
            _ => wrong.push(format!(
                "want one {method} {path} fact row at {at}: {at_line:?}"
            )),
        }
    }
    // Callers reach these rows only while they are the request itself, not a
    // call through a declaration (`call_through.rs` `is_request_row`).
    let through: Vec<String> = blobs
        .iter()
        .flat_map(|blob| blob["calls"].as_array().into_iter().flatten())
        .filter(|call| {
            call["file_path"]
                .as_str()
                .is_some_and(|at| at.starts_with("src/r3/sdk.ts:"))
                && call["role"] == "wrapper_call"
        })
        .map(|call| call["file_path"].to_string())
        .collect();
    assert!(wrong.is_empty(), "{wrong:#?}\nrows: {rows:#?}");
    assert!(
        through.is_empty(),
        "SDK rows read as calls through: {through:?}"
    );
}

/// carrick#2448 T4: the same `{ url }` call shape whose walk ends at a broker
/// (`n2/jobs.ts:20`) or at no labelled call at all (`n1/cache.ts`) states no
/// fact-layer HTTP row. Their `request_spec` rows go with that rule (#2413
/// slice F), not here.
#[test]
fn a_descriptor_carried_to_a_non_http_end_states_no_http_row() {
    if !sidecar_built() {
        return;
    }
    let blobs = scan();
    let rows = call_rows(&blobs);
    let wrong: Vec<&String> = ["n2/jobs.ts:20", "n1/cache.ts:19", "n1/cache.ts:21"]
        .iter()
        .flat_map(|at| rows_at(&rows, at))
        .filter(|row| row.ends_with(" request_summary"))
        .collect();
    assert!(
        wrong.is_empty(),
        "fact rows off a non-HTTP end: {wrong:#?}\nrows: {rows:#?}"
    );
}
