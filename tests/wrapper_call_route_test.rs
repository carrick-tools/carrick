//! carrick#1794: a call of an imported helper reaches the route the helper's
//! body requests, not the sibling route the model named at the call.
//!
//! The fixture's helper module declares one function per sibling route. The
//! cassette holds the answer the model really gives for this shape: the
//! shared-stats helper's own request line names its route, while the hook that
//! calls it is recorded on the plain stats route, which only the other helper
//! requests. Before the fix that row stayed, and once its value was typed it
//! was judged against the wrong route's producer.
//!
//! With the fix the row takes the helper's route, which the helper's own
//! request line already states, so the graph keeps the one row for the one
//! request (the wrapper-echo rule). The other hook, whose helper states no row
//! of its own, keeps its row on the stats route.
//!
//! Deterministic end to end: the model is replayed from `__llm__/`.

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

fn rows(calls: &[serde_json::Value]) -> Vec<(String, i64, String, String)> {
    let mut rows: Vec<(String, i64, String, String)> = calls
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
fn a_helper_s_caller_reaches_the_route_the_helper_requests() {
    let calls = calls("wrapper-call-route");

    assert_eq!(
        rows(&calls),
        vec![
            (
                "src/hooks/useShelfStats.ts".to_string(),
                4,
                "GET".to_string(),
                "/v1/shelves/stats".to_string()
            ),
            (
                "src/lib/shelves.ts".to_string(),
                22,
                "GET".to_string(),
                "/v1/shelves/shared-stats".to_string()
            ),
        ],
        "the shared-stats hook calls a helper that requests the shared route; no row \
         records it on the sibling route"
    );
}
