//! carrick#829: a request whose path interpolates a call expression is
//! recorded, instead of vanishing for want of a route shape.
//!
//! Deterministic end to end. The LLM is replayed from `__llm__/`, and the
//! cassette holds what extraction honestly says about each call: the target
//! verbatim. What is under test is what the scanner then does with it.
//!
//! `is_valid_route_shape` rejected any route containing a parenthesis — a rule
//! written to catch leftover JavaScript source standing where a route should
//! be, applied to the whole target including its `${…}` placeholders. So an
//! ordinary encoder around a path value (`${encodeURIComponent(dataset)}`) cost
//! the whole call: not a matched edge, not an unmatched call, not an egress
//! candidate. The control on the next call site, an identifier in the same
//! position, was recorded throughout.
//!
//! See `tests/fixtures/env-var-template-call-segment/README.md` for the shape.

use std::process::Command;

fn calls() -> Vec<serde_json::Value> {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/env-var-template-call-segment");
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

fn call_at(calls: &[serde_json::Value], file: &str, line: i64) -> serde_json::Value {
    calls
        .iter()
        .find(|call| call["file"] == file && call["line"].as_i64() == Some(line))
        .unwrap_or_else(|| panic!("no row at {file}:{line}: {calls:#?}"))
        .clone()
}

#[test]
fn a_call_expression_in_a_path_segment_keeps_the_call() {
    let calls = calls();
    assert_eq!(calls.len(), 2, "one row per call site: {calls:#?}");

    let ingest = call_at(&calls, "src/ingest.ts", 10);
    assert_eq!(ingest["method"], "POST");
    assert_eq!(
        ingest["target_url"], "${process.env.INGEST_URL}/v1/ingest/${encodeURIComponent(DATASET)}",
        "the target is kept as written, with the alias resolved to the env var behind it"
    );
    assert_eq!(
        ingest["base"]["env_var"], "INGEST_URL",
        "and the base is the environment variable, not the local const: {ingest:#?}"
    );

    // The control: the same base and the same shape, an identifier where the
    // other site has a call. It was recorded before this change and must read
    // exactly as it did.
    let status = call_at(&calls, "src/ingest.ts", 24);
    assert_eq!(status["method"], "GET");
    assert_eq!(
        status["target_url"],
        "${process.env.INGEST_URL}/v1/status/${region}"
    );
    assert_eq!(status["base"]["env_var"], "INGEST_URL");
}
