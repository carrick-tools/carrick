//! carrick#588: a call site that reaches an endpoint through a client method
//! declared in another module takes its method and its path from that method,
//! not from whatever in its own file looks like a path.
//!
//! Deterministic end to end. The LLM is replayed from `__llm__/`, and the
//! cassette deliberately holds the WRONG answer — the one the model gives when
//! nothing supplies the truth: the verb guessed from the method name, and the
//! path lifted from the only path-shaped literal in the consumer file, which
//! is an error message naming neither call. The assertions below fail against
//! that cassette on the pre-fix scanner, so the test is a regression net for
//! the machinery and not for the model.
//!
//! See `tests/fixtures/imported-request-member/README.md` for the shape.

use std::process::Command;

fn calls(fixture_dir: &str) -> Vec<serde_json::Value> {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/{fixture_dir}");
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
    let matches: Vec<&serde_json::Value> = calls
        .iter()
        .filter(|call| call["line"].as_i64() == Some(line))
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one call row at line {line}, got {matches:#?}"
    );
    matches[0]
}

#[test]
fn client_method_states_the_verb_and_the_version_for_its_call_sites() {
    let calls = calls("imported-request-member");
    assert_eq!(calls.len(), 3, "one row per call site: {calls:#?}");

    // `client.createArtifactUrl(name)` reaches a PUT to /api/v2. The cassette
    // says POST, and says v2 only because the error message four lines below
    // happens to name v2.
    let upload = call_at_line(&calls, 5);
    assert_eq!(upload["method"], "PUT");
    assert_eq!(upload["path"], "/api/v2/artifacts/:encoded");
    assert_eq!(
        upload["target_url"],
        "${this.baseUrl}/api/v2/artifacts/${encoded}"
    );

    // `client.readArtifactUrl(name)` reaches a GET to /api/v1. The cassette has
    // the verb right by luck and the version wrong from the same literal.
    let download = call_at_line(&calls, 16);
    assert_eq!(download["method"], "GET");
    assert_eq!(download["path"], "/api/v1/artifacts/:encoded");
    assert_eq!(
        download["target_url"],
        "${this.baseUrl}/api/v1/artifacts/${encoded}"
    );

    // `apiClient.createArtifactUrl(name)` shares a name with the client's
    // method and is imported from somewhere else, so the join must not fire
    // and the row must keep what extraction gave it.
    let local = call_at_line(&calls, 21);
    assert_eq!(local["method"], "GET");
    assert_eq!(local["target_url"], "/legacy/handles");
}
