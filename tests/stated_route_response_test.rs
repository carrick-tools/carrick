//! A route the receiver trace writes types its response (carrick#4071).
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/stated-route-response/` with the type sidecar LIVE. The
//! library store's answer states both routes from the code; the model's
//! answer for `routes.ts` names none. The trace writes the rows, and no model
//! row stands behind them, so nothing but the trace's own request asks for
//! their response. The answer key is in the fixture's README.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stated-route-response")
}

/// `path` -> `(type_state, expanded_definition)` for every HTTP producer
/// RESPONSE entry of the service's type manifest.
fn scan_response_types() -> BTreeMap<String, (String, Option<String>)> {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(fixture_dir())
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/cassettes/", fixture_dir().display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1");
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
        // The type layer is under test: a sidecar that cannot start must
        // fail the scan, not pass as "types unavailable".
        "CARRICK_ALLOW_MISSING_TYPES",
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
    blob["type_manifest"]
        .as_array()
        .expect("type_manifest")
        .iter()
        .filter(|entry| {
            entry["protocol"] == "http"
                && entry["role"] == "producer"
                && entry["type_kind"] == "response"
        })
        .map(|entry| {
            (
                entry["path"].as_str().unwrap_or_default().to_string(),
                (
                    entry["type_state"].as_str().unwrap_or_default().to_string(),
                    entry["expanded_definition"].as_str().map(str::to_string),
                ),
            )
        })
        .collect()
}

#[test]
fn a_route_the_trace_writes_types_its_response_from_its_handler() {
    let types = scan_response_types();
    let typed = |path: &str| -> (String, Option<String>) {
        types
            .get(path)
            .unwrap_or_else(|| panic!("no response entry for {path}: {types:#?}"))
            .clone()
    };
    let item = "{ id: string; connected: boolean; }";
    assert_eq!(
        typed("/api/v1/items/:id"),
        ("explicit".to_string(), Some(item.to_string())),
        "{types:#?}"
    );
    assert_eq!(
        typed("/api/v1/items"),
        ("explicit".to_string(), Some(format!("{item}[]"))),
        "{types:#?}"
    );
}
