//! carrick#3422: a decorator-routed service that enables URI versioning is
//! indexed under the version segment each route is served at, and a consumer
//! call pairs with exactly those routes.
//!
//! Deterministic end to end: the model is silent for every file of
//! `tests/fixtures/nestjs-uri-versioning` (see its `README.md` and
//! `expected.json`), so every producer row is the decorator pass's.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nestjs-uri-versioning")
}

fn answer_key() -> serde_json::Value {
    serde_json::from_str(
        &std::fs::read_to_string(fixture().join("expected.json")).expect("read the answer key"),
    )
    .expect("the answer key is JSON")
}

fn scan() -> &'static serde_json::Value {
    static SCAN: OnceLock<serde_json::Value> = OnceLock::new();
    SCAN.get_or_init(|| {
        let root = fixture();
        let cache = tempfile::tempdir().expect("temp cache dir");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
        cmd.arg(&root)
            .env("CARRICK_MOCK_ALL", "1")
            .env(
                "CARRICK_MOCK_FIXTURE_DIR",
                format!("{}/", root.join("__llm__").display()),
            )
            .env("CARRICK_SKIP_INTENTS", "1")
            .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
            .env("CARRICK_LOCAL_STORAGE_DIR", cache.path())
            .env("CARRICK_OUTPUT_JSON", "1");
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
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "scan failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let start = stdout
            .find("\n{")
            .map(|i| i + 1)
            .or_else(|| stdout.starts_with('{').then_some(0))
            .unwrap_or_else(|| panic!("no JSON in scanner stdout:\n{stdout}"));
        serde_json::from_str(&stdout[start..])
            .unwrap_or_else(|e| panic!("projection parse failed: {e}\n{stdout}"))
    })
}

fn rows(value: &serde_json::Value) -> BTreeSet<(String, String)> {
    value
        .as_array()
        .expect("an array of rows")
        .iter()
        .map(|row| {
            (
                row["method"].as_str().expect("method").to_string(),
                row["path"].as_str().expect("path").to_string(),
            )
        })
        .collect()
}

/// The silent model leaves the deterministic rows alone, so the service's
/// routes are exactly the answer key's: each under the version it is served
/// at, and the decoy class nowhere.
#[test]
fn every_route_carries_the_uri_version_nest_serves_it_under() {
    let scan = scan();
    let key = answer_key();
    let endpoints = scan["endpoints"].as_array().expect("endpoints");
    let produced = rows(&serde_json::Value::Array(endpoints.clone()));
    let expected = rows(&key["endpoints"]);
    assert_eq!(produced, expected, "endpoints: {endpoints:#?}");
    for row in endpoints {
        assert_eq!(
            row["resolution_source"], "decorator_route",
            "stated by the decorator pass: {row}"
        );
    }
}

#[test]
fn every_versioned_call_pairs_and_the_decoys_do_not() {
    let scan = scan();
    let key = answer_key();
    let matches = scan["cross_repo_matches"].as_array().expect("matches");
    let paired: BTreeSet<String> = matches
        .iter()
        .map(|edge| {
            assert_eq!(edge["producer_repo"], "catalog-api", "{edge}");
            assert_eq!(edge["consumer_repo"], "storefront", "{edge}");
            edge["producer_key"].as_str().expect("key").to_string()
        })
        .collect();
    let expected: BTreeSet<String> = key["endpoints"]
        .as_array()
        .expect("endpoints")
        .iter()
        .map(|row| {
            format!(
                "http|{}|{}",
                row["method"].as_str().unwrap(),
                row["path"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(paired, expected, "every route is called: {matches:#?}");

    // One edge per versioned call, and none for a decoy.
    assert_eq!(matches.len(), 8, "{matches:#?}");
    for decoy in key["decoy_calls"].as_array().expect("decoys") {
        let (method, path) = (
            decoy["method"].as_str().unwrap(),
            decoy["path"].as_str().unwrap(),
        );
        assert!(
            !matches.iter().any(|edge| {
                edge["consumer_key"].as_str() == Some(&format!("http|{method}|{path}"))
            }),
            "decoy {method} {path} must not pair: {matches:#?}"
        );
    }
}
