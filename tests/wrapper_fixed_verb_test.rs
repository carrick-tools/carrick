//! A call through a helper whose own request writes the verb.
//!
//! The helper in `catalog.ts` requests `POST /catalog/sync`. The screen calls
//! it and the replayed model answer states no method at the call, which would
//! index as a GET.
//!
//! - Every row for the route carries the helper's POST.
//! - The screen's row is a call through the helper that names the request the
//!   helper issues (`role` and `reaches_request`), not a second request.
//!
//! See `tests/fixtures/wrapper-fixed-verb/README.md` for the answer key.

use std::process::Command;

/// `(file, line, method, path)` of every call row after one offline scan.
fn rows() -> Vec<(String, i64, String, String)> {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/wrapper-fixed-verb");
    let mock_dir = format!("{fixture}/__llm__/");
    let storage = tempfile::tempdir().expect("temp storage dir");

    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(&fixture)
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_MOCK_FIXTURE_DIR", &mock_dir)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_OUTPUT_JSON", "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env_remove("GITHUB_REPOSITORY")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("CI")
        .output()
        .expect("failed to spawn carrick binary");
    assert!(
        output.status.success(),
        "scanner exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let projection: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("scanner output was not valid JSON");
    let mut rows: Vec<(String, i64, String, String)> = projection["calls"]
        .as_array()
        .expect("projection carries a calls array")
        .iter()
        .map(|call| {
            (
                call["file"].as_str().unwrap_or_default().to_string(),
                call["line"].as_i64().unwrap_or_default(),
                call["method"].as_str().unwrap_or_default().to_string(),
                call["path"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    rows.sort();
    rows
}

#[test]
fn a_call_through_a_helper_that_fixes_the_verb_is_not_indexed_as_a_get() {
    let rows = rows();
    assert!(
        rows.iter()
            .any(|(file, _, method, path)| file == "src/lib/catalog.ts"
                && method == "POST"
                && path == "/catalog/sync"),
        "the helper's own request is a POST row: {rows:?}"
    );
    let off_verb: Vec<_> = rows
        .iter()
        .filter(|(_, _, method, path)| path == "/catalog/sync" && method != "POST")
        .collect();
    assert!(
        off_verb.is_empty(),
        "every row for the route carries the helper's POST, found {off_verb:?}"
    );
}

/// The mount graph's call rows of the service after one offline scan, with the
/// storage dir holding the blob.
fn mount_graph_calls() -> Vec<serde_json::Value> {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/wrapper-fixed-verb");
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(&fixture)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_MOCK_FIXTURE_DIR", format!("{fixture}/__llm__/"))
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
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let entry = std::fs::read_dir(storage.path())
        .expect("storage dir")
        .flatten()
        // Blobs only: a type-reach record sits beside each (carrick#3101).
        .find(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .expect("one blob");
    let blob: serde_json::Value =
        serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
            .expect("parse blob");
    blob["mount_graph"]["data_calls"]
        .as_array()
        .expect("the blob holds a mount graph")
        .clone()
}

#[test]
fn a_call_through_a_helper_reaches_the_request_its_helper_issues() {
    let calls = mount_graph_calls();
    let at = |site: &str| -> &serde_json::Value {
        calls
            .iter()
            .find(|call| call["file_location"] == site)
            .unwrap_or_else(|| panic!("a row at {site}: {calls:?}"))
    };
    let request = at("src/lib/catalog.ts:2");
    assert_ne!(
        request["role"], "wrapper_call",
        "the helper's own line is the request: {request}"
    );
    let call = at("src/screens/sync-screen.ts:3");
    assert_eq!(
        call["role"], "wrapper_call",
        "the screen's row is a call through the helper, not a second request: {call}"
    );
    assert_eq!(
        call["reaches_request"], "src/lib/catalog.ts:2",
        "the screen's row names the request it reaches: {call}"
    );
}
