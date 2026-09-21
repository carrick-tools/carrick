//! carrick#1371: one HTTP request reaches the index as ONE consumer row.
//!
//! The fixture writes one request four ways, which is how a React client
//! normally writes it: a query hook sets it up, an API-object method is
//! called, a same-file wrapper issues the request, and the body is read back.
//! Before this test the scan produced a row for each of those lines and a tool
//! counting consumers read four independent call sites.
//!
//! Deterministic end to end: the LLM is replayed from `__llm__/`, and the
//! cassette holds the answer the model really gives — a full target at the
//! hook's setup line and at the `response.json()` line, the second carrying
//! the only correct response anchor in the file. The assertions below fail on
//! a scanner without the fold, so this is a regression net for the machinery
//! and not for the model.
//!
//! See `tests/fixtures/one-row-per-request/README.md` for the shape and the
//! answer key.

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

fn sites(calls: &[serde_json::Value]) -> Vec<(String, i64)> {
    let mut sites: Vec<(String, i64)> = calls
        .iter()
        .map(|call| {
            (
                call["file"].as_str().unwrap_or_default().to_string(),
                call["line"].as_i64().unwrap_or_default(),
            )
        })
        .collect();
    sites.sort();
    sites
}

#[test]
fn one_request_written_four_ways_is_one_consumer_per_request_site() {
    let calls = calls("one-row-per-request");

    // The hook's `useQuery` setup line (6) and the `response.json()` line (21)
    // are gone. What is left is the two lines that reach the endpoint: the
    // call THROUGH the client method, and the request the method's own body
    // issues (carrick#1146 wants both, and which of the two is the network
    // request is the role field carrick#1371 leaves open).
    assert_eq!(
        sites(&calls),
        vec![
            ("src/hooks/useShelves.ts".to_string(), 8),
            ("src/lib/shelves.ts".to_string(), 17),
        ],
        "one row per request site: {calls:#?}"
    );

    for call in &calls {
        assert_eq!(call["method"], "GET", "{call:#?}");
        assert!(
            call["path"]
                .as_str()
                .expect("a call row states a path")
                .ends_with("/v1/me/shelves"),
            "{call:#?}"
        );
    }
}
