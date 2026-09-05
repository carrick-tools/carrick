//! carrick#610: a request whose target is built as `new URL(path, base)` takes
//! its path from that first argument, not from whatever version extraction
//! reached for.
//!
//! Deterministic end to end. The LLM is replayed from `__llm__/`, and the
//! cassette holds what the deployed index recorded for this shape: the wrong
//! API version on the site that states its path inline, and a bare binding on
//! the two that build the URL a statement earlier. A bare binding is not
//! route-shaped, so those two were dropped before they became a row of any
//! kind, and the third named a route the package does not call.
//!
//! See `tests/fixtures/new-url-target/README.md` for the shape.

use std::process::Command;

fn calls() -> Vec<serde_json::Value> {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/new-url-target");
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

fn call_at<'a>(calls: &'a [serde_json::Value], file: &str, line: i64) -> &'a serde_json::Value {
    let matches: Vec<&serde_json::Value> = calls
        .iter()
        .filter(|call| call["file"] == file && call["line"].as_i64() == Some(line))
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one call row at {file}:{line}, got {matches:#?}"
    );
    matches[0]
}

#[test]
fn a_new_url_target_carries_the_path_its_first_argument_states() {
    let calls = calls();
    let catalogue: Vec<&serde_json::Value> = calls
        .iter()
        .filter(|call| call["file"] == "src/catalogue.ts")
        .collect();
    assert_eq!(
        catalogue.len(),
        4,
        "one row per request site in the catalogue client: {catalogue:#?}"
    );

    // Direct form: the URL object is the request's own argument. No base, so
    // the call matches by route path like any other host-free call.
    let list = call_at_line(&calls, 18);
    assert_eq!(list["method"], "GET");
    assert_eq!(list["target_url"], "/api/v2/things");
    assert_eq!(list["key"], "http|GET|/api/v2/things");

    // Binding form, read back through `.href`.
    let find = call_at_line(&calls, 27);
    assert_eq!(find["method"], "GET");
    assert_eq!(find["target_url"], "/api/v2/things/search");
    assert_eq!(find["key"], "http|GET|/api/v2/things/search");

    // Template form. The target keeps the source spelling, and the key the
    // matcher joins on carries the interpolated segment as a path parameter.
    let archive = call_at_line(&calls, 33);
    assert_eq!(archive["method"], "POST");
    assert_eq!(archive["target_url"], "/api/v2/things/${id}/archive");
    assert_eq!(archive["key"], "http|POST|/api/v2/things/:id/archive");
}

#[test]
fn the_decoy_call_is_left_exactly_as_it_was_written() {
    let calls = calls();

    // The one site that states its own path keeps it, base and all: the rule
    // fires on a `new URL` target and on nothing else.
    let token = call_at_line(&calls, 13);
    assert_eq!(token["method"], "POST");
    assert_eq!(token["target_url"], "${this.baseUrl}/api/v1/token");

    // And no site takes its version from that neighbour.
    for call in &calls {
        let target = call["target_url"].as_str().unwrap_or_default();
        assert!(
            !target.contains("/api/v1/things"),
            "nothing in this package calls the retired version of the route: {call:#?}"
        );
    }
}

/// carrick#697: the base argument of a `new URL(path, base)` is opaque, so a
/// base the model reports in front of the path it states is a paraphrase of
/// that opaque argument, not a reading of the source. Keeping it keys the row
/// on a base no route lookup can reach.
///
/// The one prefix the model can only have READ is a literal absolute origin,
/// which is also the one that classifies the call as external. That one is
/// kept; everything else is discarded and the stated path wins.
#[test]
fn a_new_url_row_is_keyed_on_the_path_its_source_states_not_the_models_prefix() {
    let calls = calls();
    let file = "src/checkpoints.ts";

    // The model invented an `${API_URL}` alias for the opaque base. Kept, it
    // keys the row on an undeclared env-var base, and the call disappears from
    // every route-path lookup.
    let suspend = call_at(&calls, file, 15);
    assert_eq!(suspend["method"], "POST");
    assert_eq!(
        suspend["target_url"],
        "/api/v1/runs/${runFriendlyId}/snapshots/${snapshotFriendlyId}/suspend"
    );
    assert_eq!(
        suspend["path"],
        "/api/v1/runs/:runFriendlyId/snapshots/:snapshotFriendlyId/suspend"
    );
    assert_eq!(
        suspend["key"],
        "http|POST|/api/v1/runs/:runFriendlyId/snapshots/:snapshotFriendlyId/suspend"
    );
    assert_eq!(
        suspend["base"]["kind"], "relative",
        "the row is host-free, not sitting behind an env-var base: {suspend:#?}"
    );

    // The model spelled the receiver back as the base. Same rule: the source
    // states the path, and nothing else.
    let continue_run = call_at(&calls, file, 30);
    assert_eq!(continue_run["method"], "POST");
    assert_eq!(
        continue_run["target_url"],
        "/api/v1/runs/${runFriendlyId}/snapshots/${snapshotFriendlyId}/continue"
    );
    assert_eq!(
        continue_run["key"],
        "http|POST|/api/v1/runs/:runFriendlyId/snapshots/:snapshotFriendlyId/continue"
    );

    // A literal absolute origin is the carve-out: the model read a host the
    // source does not state, and the origin is the classification.
    let complete = call_at(&calls, file, 46);
    assert_eq!(complete["method"], "POST");
    assert_eq!(
        complete["target_url"],
        "https://api.example.test/api/v1/runs/${runFriendlyId}/attempts/${attemptFriendlyId}/complete"
    );
}
