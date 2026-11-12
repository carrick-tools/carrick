//! A bare `fetch(...)` is read through the store's label for the global, not
//! through its name.
//!
//! Drives the real scanner binary offline over `tests/fixtures/global-fetch-site/`
//! as the Action does (`carrick <dir>`, the root service). `src/a.ts` calls the
//! runtime's `fetch`, which the cassette labels; `src/shadow.ts` declares its own
//! function named `fetch`, which is not the global and has no label. The canned
//! answer for `shadow.ts` states a row; a scan that reads a call by the name
//! `fetch` sends that file to the model.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/global-fetch-site")
}

/// The blob one offline scan of `repo` writes, and the scan's stderr.
fn scan(repo: &Path) -> (serde_json::Value, String) {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env_remove("CARRICK_ALLOW_MISSING_TYPES");
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
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut blobs: Vec<serde_json::Value> = std::fs::read_dir(storage.path())
        .expect("storage dir")
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .map(|entry| {
            serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
                .expect("parse blob")
        })
        .collect();
    assert_eq!(blobs.len(), 1, "one service, one blob");
    (
        blobs.remove(0),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// The blob's `calls`, as `METHOD path@file:line`, sorted.
fn calls(blob: &serde_json::Value) -> Vec<String> {
    let mut calls: Vec<String> = blob["calls"]
        .as_array()
        .expect("calls")
        .iter()
        .map(|call| {
            format!(
                "{} {}@{}",
                call["key"]["method"].as_str().unwrap_or_default(),
                call["key"]["path"].as_str().unwrap_or_default(),
                call["file_path"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    calls.sort();
    calls
}

#[test]
fn a_bare_fetch_is_read_through_its_label_not_its_name() {
    let (blob, stderr) = scan(&fixture_dir());
    assert_eq!(
        calls(&blob),
        vec![
            "GET /api/orders@src/a.ts:2".to_string(),
            "POST /api/items@src/a.ts:6".to_string(),
        ],
        "the global fetch states its two rows, the local function named fetch none"
    );
    // `shadow.ts` holds no call to a labelled export and no signal fires on it,
    // so the model is asked about `a.ts` alone. Reading a call by the name
    // `fetch` would ask about both.
    assert!(
        stderr.contains("requests analyze-file 1,"),
        "only a.ts is sent to the model:\n{stderr}"
    );
}
