//! carrick#1801 (`wrapper-call-value`): the call's value is what the helper
//! returns. A helper that writes the path and parses the body into a copy of
//! its own makes its call worth that copy, so the hook's row keeps its route
//! and states no consumer response type.
//!
//! Deterministic end to end: the model is replayed from `__llm__/`.

use std::path::PathBuf;
use std::process::Command;

/// Every service's blob from one offline scan of the fixture, with the type
/// sidecar optional: the manifest entries are under test, not the types.
fn blobs(fixture_dir: &str) -> Vec<serde_json::Value> {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture_dir);
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(&fixture)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", fixture.join("__llm__").display()),
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
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read_dir(storage.path())
        .expect("storage dir")
        .flatten()
        // Blobs only: a type-reach record sits beside each (carrick#3101).
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .map(|entry| {
            serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
                .expect("parse blob")
        })
        .collect()
}

/// `(role, type_kind)` of every manifest entry at `file:line`, sorted.
fn manifest_entries(blobs: &[serde_json::Value], file: &str, line: u64) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = blobs
        .iter()
        .flat_map(|blob| {
            blob["type_manifest"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter(|entry| entry["file_path"] == file && entry["line_number"] == line)
        .map(|entry| {
            (
                entry["role"].as_str().unwrap_or_default().to_string(),
                entry["type_kind"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    entries.sort();
    entries
}

#[test]
fn a_helper_s_caller_states_no_response_type_when_the_helper_maps_the_body() {
    let blobs = blobs("wrapper-call-value");

    let mut calls: Vec<(String, String, String)> = blobs
        .iter()
        .flat_map(|blob| blob["calls"].as_array().cloned().unwrap_or_default())
        .map(|call| {
            (
                call["file_path"].as_str().unwrap_or_default().to_string(),
                call["key"]["method"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                call["key"]["path"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    calls.sort();
    assert_eq!(
        calls,
        vec![
            (
                "src/hooks/useShelf.ts:4".to_string(),
                "GET".to_string(),
                "/v1/shelves/:shelfId".to_string()
            ),
            (
                "src/hooks/useShelfStats.ts:4".to_string(),
                "GET".to_string(),
                "/v1/shelves/stats".to_string()
            ),
        ],
        "both hooks keep the route their helper writes"
    );

    let consumer = |kinds: &[&str]| -> Vec<(String, String)> {
        kinds
            .iter()
            .map(|kind| ("consumer".to_string(), kind.to_string()))
            .collect()
    };
    assert_eq!(
        manifest_entries(&blobs, "src/hooks/useShelfStats.ts", 4),
        consumer(&["request"]),
        "the call is worth the helper's copy of the body, so the site states no consumer \
         response type; its request entry stays"
    );
    assert_eq!(
        manifest_entries(&blobs, "src/hooks/useShelf.ts", 4),
        consumer(&["request", "response"]),
        "the helper hands back its parsed body, so the call is worth the body"
    );
}
