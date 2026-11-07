//! carrick#3522: a route declared with a trailing slash is stored in the
//! canonical form (`carrick_match::canonical_path`), so a call to the same
//! path without the slash pairs with it. The root route stays `/`.
//!
//! Scanned mocked ($0, no model). See `tests/fixtures/trailing-slash-route/`.

use std::path::PathBuf;
use std::process::Command;

fn projection() -> serde_json::Value {
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/trailing-slash-route");
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
}

#[test]
fn a_route_declared_with_a_trailing_slash_pairs_with_the_call_without_it() {
    let scan = projection();
    let mut routes: Vec<String> = scan["endpoints"]
        .as_array()
        .expect("an endpoints array")
        .iter()
        .map(|row| row["path"].as_str().expect("path").to_string())
        .collect();
    routes.sort();
    assert_eq!(
        routes,
        ["/", "/api/v1/environments"],
        "the trailing slash is dropped and the root stays `/`: {scan:#}"
    );

    let edges = scan["cross_repo_matches"]
        .as_array()
        .expect("a matches array");
    assert_eq!(edges.len(), 1, "one edge: {edges:#?}");
    assert_eq!(
        edges[0]["producer_key"], "http|GET|/api/v1/environments",
        "the edge reaches the route in its canonical form: {edges:#?}"
    );
}
