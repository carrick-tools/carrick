//! A listener is a pub/sub endpoint only when the object it listens on is one
//! the service receives messages from (carrick#941, carrick#2133).
//!
//! Drives the real scanner binary, offline, over
//! `tests/fixtures/listener-receivers/`, with the model's answers replayed
//! from `__llm__/`. The model reports no pub/sub operation, so every pub/sub
//! row in the blob is the in-process event-bus pass's. Detection, read from
//! the recorded answer and never from `carrick.json`, lists one messaging
//! client. The fixture's README lists the sites and why each one stays or
//! goes.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/listener-receivers")
}

/// The stored blob's endpoint keys, as `pubsub|<topic>`.
fn pubsub_endpoints() -> BTreeSet<String> {
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
    let blob: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&blobs[0]).expect("read blob")).expect("parse blob");
    blob["endpoints"]
        .as_array()
        .expect("endpoints is an array")
        .iter()
        .filter(|row| row["key"]["protocol"] == "pubsub")
        .map(|row| {
            format!(
                "pubsub|{}",
                row["key"]["topic"]
                    .as_str()
                    .expect("pub/sub row has a topic")
            )
        })
        .collect()
}

#[test]
fn only_listeners_on_objects_the_service_receives_from_are_endpoints() {
    let endpoints = pubsub_endpoints();
    assert!(
        endpoints.contains("pubsub|orderPlaced"),
        "a listener on the repo's own bus is a contract endpoint: {endpoints:?}"
    );
    assert!(
        endpoints.contains("pubsub|invoicePaid"),
        "a listener on a detected broker client is a contract endpoint: {endpoints:?}"
    );
    assert!(
        !endpoints.contains("pubsub|line"),
        "a line reader over stdin or a child's output reads the runtime's events, \
         not a contract: {endpoints:?}"
    );
}
