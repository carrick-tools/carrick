//! carrick#572: a request whose whole URL is read from an environment variable
//! is recorded, instead of vanishing for want of a route shape.
//!
//! Deterministic end to end. The LLM is replayed from `__llm__/`, and the
//! cassette holds what extraction can honestly say about `fetch(url, …)`: the
//! binding, and nothing else. A bare identifier is not route-shaped, so before
//! this change the call was dropped before it became a row of any kind — not a
//! matched edge, not an unmatched call, not an egress candidate — and the
//! assertion below fails on the pre-fix scanner.
//!
//! See `tests/fixtures/env-var-whole-url/README.md` for the shape.

use std::process::Command;

fn calls() -> Vec<serde_json::Value> {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/env-var-whole-url");
    let mock_dir = format!("{fixture}/__llm__/");

    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(&fixture)
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_MOCK_FIXTURE_DIR", &mock_dir)
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
    let stdout = String::from_utf8(output.stdout).expect("scanner stdout was not UTF-8");
    let projection: serde_json::Value =
        serde_json::from_str(&stdout).expect("scanner output was not valid JSON");
    projection["calls"]
        .as_array()
        .expect("projection carries a calls array")
        .clone()
}

#[test]
fn a_whole_url_read_from_an_env_var_is_recorded() {
    let calls = calls();
    assert_eq!(calls.len(), 2, "one row per call site: {calls:#?}");

    let ask = calls
        .iter()
        .find(|call| call["line"].as_i64() == Some(7))
        .unwrap_or_else(|| panic!("no row for the env-var URL call: {calls:#?}"));
    assert_eq!(ask["method"], "POST");
    assert_eq!(
        ask["target_url"], "${process.env.HELPDESK_URL}/api/answer",
        "the env var supplies the origin and the fallback literal supplies the path"
    );

    // The base-plus-path shape that already resolved must be untouched: the
    // whole-URL rule fires only where the target states no path of its own.
    let items = calls
        .iter()
        .find(|call| call["line"].as_i64() == Some(20))
        .unwrap_or_else(|| panic!("no row for the base-URL call: {calls:#?}"));
    assert_eq!(items["method"], "GET");
    assert_eq!(
        items["target_url"],
        "${process.env.CATALOG_URL}/api/v1/items"
    );
}
