//! Yarn's own folder is never the project's code (carrick#3683).
//!
//! Drives the real scanner binary, offline, over `tests/fixtures/yarn-folder/`,
//! with the model's answers replayed from `__llm__/`. The committed Yarn
//! release under `.yarn/` holds an event emitter, a listener and a function;
//! none of it may reach the blob. The fixture's README is the answer key.

use std::path::PathBuf;
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yarn-folder")
}

fn scanned_blob() -> serde_json::Value {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(fixture_dir())
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", fixture_dir().join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_ALLOW_MISSING_TYPES", "1");
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
        "fixture scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let blobs = std::fs::read_dir(storage.path())
        .expect("storage dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    assert_eq!(blobs.len(), 1, "expected one written blob, got {blobs:?}");
    serde_json::from_slice(&std::fs::read(&blobs[0]).expect("read blob")).expect("parse blob")
}

/// Every `file_path` of the blob's endpoint, call and function rows.
fn row_paths(blob: &serde_json::Value) -> Vec<String> {
    let mut paths = Vec::new();
    for key in ["endpoints", "calls"] {
        for row in blob[key].as_array().into_iter().flatten() {
            if let Some(path) = row["file_path"].as_str() {
                paths.push(path.to_string());
            }
        }
    }
    for row in blob["function_definitions"]
        .as_object()
        .into_iter()
        .flat_map(|map| map.values())
    {
        if let Some(path) = row["file_path"].as_str() {
            paths.push(path.to_string());
        }
    }
    paths
}

#[test]
fn nothing_under_the_yarn_folder_reaches_the_index() {
    let blob = scanned_blob();

    let functions = blob["function_definitions"]
        .as_object()
        .expect("function_definitions is a map");
    assert!(
        functions.values().any(|row| row["name"] == "placeOrder"),
        "control: a function under src/ is stated: {:?}",
        functions.keys().collect::<Vec<_>>()
    );
    let endpoints = blob["endpoints"].as_array().expect("endpoints is an array");
    assert!(
        endpoints.iter().any(|row| {
            row["key"]["protocol"] == "pubsub" && row["key"]["topic"] == "orderPlaced"
        }),
        "control: the repo's own bus listener is an endpoint: {endpoints:?}"
    );

    let paths = row_paths(&blob);
    assert!(!paths.is_empty(), "the blob states rows to check");
    let from_yarn: Vec<_> = paths
        .iter()
        .filter(|path| path.trim_start_matches("./").starts_with(".yarn/"))
        .collect();
    assert!(
        from_yarn.is_empty(),
        "no row may come from the Yarn folder: {from_yarn:?}"
    );

    let losses = &blob["boundary"]["final_losses"];
    assert!(
        losses.is_null() || losses["total"] == 0,
        "no final loss: {losses:?}"
    );
}
