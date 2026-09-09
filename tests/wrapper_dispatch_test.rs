//! carrick#872: the value a wrapper writes for a body-dispatching route
//! reaches the rows emitted at the sites that call the wrapper.
//!
//! Deterministic end to end. The LLM is replayed from `__llm__/`, and the
//! cassette states the dispatch value ONLY where the model can really see it:
//! on the client's own requests, inside `src/api-client.ts`. The consumer
//! files hold what production holds — a row at the delegating site with a
//! method and a path and no value — so an assertion here that a site's row
//! carries a value is an assertion about the scanner's join, not about the
//! model.
//!
//! See `tests/fixtures/wrapper-dispatch/README.md` for the shape and the
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

/// The one call row a file states at `line`.
fn call_at<'a>(calls: &'a [serde_json::Value], file: &str, line: i64) -> &'a serde_json::Value {
    let matches: Vec<&serde_json::Value> = calls
        .iter()
        .filter(|call| {
            call["line"].as_i64() == Some(line)
                && call["file"].as_str().is_some_and(|f| f.ends_with(file))
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one call row at {file}:{line}, got {matches:#?}"
    );
    matches[0]
}

fn dispatch_value(call: &serde_json::Value) -> Option<&str> {
    call["dispatch"]["value"].as_str()
}

#[test]
fn a_site_calling_a_member_that_issues_the_request_carries_its_value() {
    let calls = calls("wrapper-dispatch");
    let site = call_at(&calls, "search.ts", 4);
    assert_eq!(
        dispatch_value(site),
        Some("search-by-intent"),
        "the value is written in `searchByIntent`'s own body, one file away"
    );
    assert_eq!(site["dispatch"]["field"].as_str(), Some("action"));
    assert_eq!(site["dispatch"]["location"].as_str(), Some("body"));
    assert_eq!(site["method"].as_str(), Some("POST"));
}

#[test]
fn a_site_calling_a_member_that_delegates_through_a_callback_carries_its_value() {
    let calls = calls("wrapper-dispatch");
    assert_eq!(
        dispatch_value(call_at(&calls, "graph.ts", 4)),
        Some("get-cross-repo-data"),
        "`getAllRepoData` reaches its request through the cache's callback"
    );
}

#[test]
fn a_site_two_delegations_from_the_request_carries_its_value() {
    let calls = calls("wrapper-dispatch");
    assert_eq!(
        dispatch_value(call_at(&calls, "services.ts", 4)),
        Some("get-cross-repo-data"),
        "`findService` -> `getAllRepoData` -> `fetchCrossRepoData`"
    );
}

#[test]
fn a_site_calling_a_member_that_issues_two_requests_carries_nothing() {
    let calls = calls("wrapper-dispatch");
    let site = call_at(&calls, "refresh.ts", 4);
    assert_eq!(
        dispatch_value(site),
        None,
        "`refreshEverything` writes two actions; which one this site asks for \
         is not something the source says"
    );
}

#[test]
fn a_same_file_wrapper_site_carries_the_value_the_wrapper_writes() {
    let calls = calls("wrapper-dispatch");
    let site = call_at(&calls, "local.ts", 11);
    assert_eq!(
        dispatch_value(site),
        Some("store-metadata"),
        "the row at this site is the same-file wrapper pass's own, and the \
         value is at the wrapper's request four lines up"
    );
    assert_eq!(
        site["resolution_source"].as_str(),
        Some("same_file_wrapper"),
        "the row is still the deterministic pass's; the carry stamps a value \
         and nothing else"
    );
}
