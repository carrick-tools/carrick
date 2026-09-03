//! carrick#627: a request whose URL interpolates a base declared as a
//! module-level string literal states the same route as the absolute-host form,
//! instead of keeping the interpolation in its target and matching nothing.
//!
//! Deterministic end to end. The LLM is replayed from `__llm__/`, and the
//! cassette holds the target verbatim, which is what extraction emits for a
//! base it cannot see the value of. Before this change the `${BASE}` prefix
//! survived into the canonical key, so the assertions below fail on the pre-fix
//! scanner.
//!
//! See `tests/fixtures/literal-base-url/README.md` for the shape.

use std::process::Command;

fn calls() -> Vec<serde_json::Value> {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/literal-base-url");
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

fn call_at_line(calls: &[serde_json::Value], line: i64) -> &serde_json::Value {
    calls
        .iter()
        .find(|call| call["line"].as_i64() == Some(line))
        .unwrap_or_else(|| panic!("no row at line {line}: {calls:#?}"))
}

#[test]
fn a_base_declared_as_a_string_literal_resolves_to_its_url() {
    let calls = calls();
    assert_eq!(calls.len(), 2, "one row per call site: {calls:#?}");

    let statuses = call_at_line(&calls, 8);
    assert_eq!(statuses["method"], "GET");
    assert_eq!(statuses["target_url"], "http://localhost:8080/status");
    assert_eq!(
        statuses["path"], "/status",
        "the canonical key reduces to the route path once the base is a host"
    );

    let whoami = call_at_line(&calls, 10);
    assert_eq!(whoami["method"], "GET");
    assert_eq!(whoami["target_url"], "http://localhost:3030/api/v1/whoami");
    assert_eq!(whoami["path"], "/api/v1/whoami");
}
