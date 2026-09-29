//! carrick#1555: calls through a client are stated from the client's source.
//!
//! Deterministic end to end. The LLM is replayed from `__llm__/`, and the
//! cassette is wrong the way production's model was — a `GET` for a `POST`,
//! actions invented from method names, no dispatch at the client's own
//! requests — so every assertion here is about what the scanner read, never
//! about what the cassette said.
//!
//! See `tests/fixtures/request-summary/README.md` for the shape and the
//! answer key.

use std::process::Command;

const ROUTE: &str = "/types/check-or-upload";

fn calls() -> Vec<serde_json::Value> {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/request-summary");
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

fn rows_at<'a>(
    calls: &'a [serde_json::Value],
    file: &str,
    line: i64,
) -> Vec<&'a serde_json::Value> {
    calls
        .iter()
        .filter(|call| {
            call["line"].as_i64() == Some(line)
                && call["file"].as_str().is_some_and(|f| f.ends_with(file))
        })
        .collect()
}

/// The one row at `file:line`, stated by the summaries, sending `POST` to the
/// gateway with `action` — or with no action when `action` is `None`.
fn assert_gateway_row(calls: &[serde_json::Value], file: &str, line: i64, action: Option<&str>) {
    let rows = rows_at(calls, file, line);
    assert_eq!(
        rows.len(),
        1,
        "expected one row at {file}:{line}, got {rows:#?}"
    );
    let row = rows[0];
    assert_eq!(
        row["resolution_source"].as_str(),
        Some("request_summary"),
        "{file}:{line} is stated by the source, not the model: {row:#}"
    );
    assert_eq!(
        row["method"].as_str(),
        Some("POST"),
        "{file}:{line}: {row:#}"
    );
    assert_eq!(row["path"].as_str(), Some(ROUTE), "{file}:{line}: {row:#}");
    assert_eq!(
        row["dispatch"]["value"].as_str(),
        action,
        "{file}:{line}: the action the source writes, whatever the model said: {row:#}"
    );
    if action.is_some() {
        assert_eq!(row["dispatch"]["field"].as_str(), Some("action"));
        assert_eq!(row["dispatch"]["location"].as_str(), Some("body"));
    }
}

/// A known gap in the fixture's README: production sends `sent` as the
/// action at `file:line`, and the scanner states the row with no action.
/// Asserted as the scanner states it today, so closing the gap fails here
/// and the README's Known gaps table is updated with it.
fn assert_known_gap(calls: &[serde_json::Value], file: &str, line: i64, sent: &str, issue: &str) {
    let rows = rows_at(calls, file, line);
    let dispatch = rows
        .first()
        .map(|row| row["dispatch"]["value"].clone())
        .unwrap_or_default();
    assert!(
        dispatch.is_null(),
        "known gap {issue}: production sends action={sent} at {file}:{line}, and the scanner \
         was expected to state no action, but states {dispatch}; if the gap is closed, move \
         the row out of the README's Known gaps"
    );
    assert_gateway_row(calls, file, line, None);
}

#[test]
fn the_clients_own_requests_are_stated_through_the_field_that_holds_the_url() {
    let calls = calls();
    assert_gateway_row(&calls, "api-client.ts", 93, Some("search-by-intent"));
    assert_gateway_row(&calls, "api-client.ts", 113, Some("analysis-job-status"));
    assert_gateway_row(&calls, "api-client.ts", 147, Some("get-cross-repo-data"));
}

/// A key written after a spread of the caller's params is stated. One
/// written before it is what production sends too, but the scanner does
/// not state it: a known gap (see the fixture's README).
#[test]
fn an_action_the_params_spread_may_overwrite_is_not_stated() {
    let calls = calls();
    assert_gateway_row(&calls, "api-client.ts", 171, Some("refresh"));
    assert_known_gap(
        &calls,
        "api-client.ts",
        103,
        "find-similar",
        "#1585: the key is written before a spread whose declared type cannot carry it",
    );
}

#[test]
fn a_helper_whose_body_its_caller_writes_states_no_action_of_its_own() {
    assert_gateway_row(&calls(), "api-client.ts", 129, None);
}

#[test]
fn a_request_to_a_url_the_source_does_not_state_has_no_row() {
    let calls = calls();
    assert!(
        rows_at(&calls, "api-client.ts", 163).is_empty(),
        "the presigned read's URL arrives at run time"
    );
}

#[test]
fn a_call_reaching_its_request_through_a_callback_the_cache_invokes_is_the_request() {
    let calls = calls();
    // The model said GET with no action here, and GET with the right action
    // at the second: both are the POST the client body writes.
    assert_gateway_row(&calls, "tools/graph.ts", 4, Some("get-cross-repo-data"));
    assert_gateway_row(
        &calls,
        "tools/check-compat.ts",
        4,
        Some("get-cross-repo-data"),
    );
}

#[test]
fn an_invented_action_is_replaced_by_the_literal_the_body_writes() {
    let calls = calls();
    // `findService` -> `getAllRepoData` -> the cache -> `fetchCrossRepoData`.
    assert_gateway_row(&calls, "tools/services.ts", 4, Some("get-cross-repo-data"));
    assert_gateway_row(&calls, "server.ts", 13, Some("get-cross-repo-data"));
}

/// Where the request line states no action, the row the source states
/// carries none: the model's invented `findSimilar` is never kept on it
/// (carrick#1564 review, finding 9). Production sends `find-similar`
/// there, which the scanner does not state: a known gap.
#[test]
fn a_fact_row_never_carries_the_models_action() {
    assert_known_gap(
        &calls(),
        "tools/find-similar.ts",
        4,
        "find-similar",
        "#1585: the request line writes the action before a typed spread",
    );
}

#[test]
fn a_body_serialised_by_a_helper_is_the_one_its_caller_writes() {
    assert_gateway_row(&calls(), "tools/projects.ts", 4, Some("list-projects"));
}

#[test]
fn a_call_inside_an_injected_lambda_is_stated_where_it_is_written() {
    assert_gateway_row(&calls(), "server.ts", 9, Some("analysis-job-status"));
}

#[test]
fn a_call_the_candidate_scanner_raised_nothing_for_is_stated_by_the_call_graph() {
    // The receiver is named `gateway`: no candidate, no model call, and still
    // one row, because the parameter's declared type says what it is.
    assert_gateway_row(&calls(), "tools/lookup.ts", 6, Some("search-by-intent"));
}

#[test]
fn a_call_whose_callee_sends_nothing_loses_the_models_row() {
    let calls = calls();
    assert!(
        rows_at(&calls, "server.ts", 12).is_empty(),
        "`invalidateCache` clears the cache and sends nothing"
    );
}

#[test]
fn a_call_whose_callee_constructs_a_class_is_never_proven_silent() {
    // `startPolling` makes no call, and the `Poller` it constructs sends a
    // request from its constructor. Nothing here follows a construction, so
    // the model's row stands as the model's.
    let calls = calls();
    let rows = rows_at(&calls, "server.ts", 19);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0]["resolution_source"].as_str(), Some("model"));
}

#[test]
fn calls_shaped_like_requests_that_send_nothing_state_no_row() {
    let calls = calls();
    for file in ["negatives.ts", "tools/misc.ts"] {
        let rows: Vec<_> = calls
            .iter()
            .filter(|call| call["file"].as_str().is_some_and(|f| f.ends_with(file)))
            .collect();
        assert!(rows.is_empty(), "{file} sends nothing: {rows:#?}");
    }
}
