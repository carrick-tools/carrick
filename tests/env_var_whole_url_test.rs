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

fn call_at(calls: &[serde_json::Value], file: &str, line: i64) -> serde_json::Value {
    calls
        .iter()
        .find(|call| call["file"] == file && call["line"].as_i64() == Some(line))
        .unwrap_or_else(|| panic!("no row at {file}:{line}: {calls:#?}"))
        .clone()
}

#[test]
fn a_whole_url_read_from_an_env_var_is_recorded() {
    let calls = calls();
    assert_eq!(calls.len(), 4, "one row per call site: {calls:#?}");

    let ask = call_at(&calls, "src/helpdesk.ts", 7);
    assert_eq!(ask["method"], "POST");
    assert_eq!(
        ask["target_url"], "${process.env.HELPDESK_URL}/api/answer",
        "the env var supplies the origin and the fallback literal supplies the path"
    );
    assert_eq!(
        ask["path"], "/api/answer",
        "the loopback default states the origin, so the key is the route it requests"
    );

    // The base-plus-path shape that already resolved must be untouched: the
    // whole-URL rule fires only where the target states no path of its own.
    let items = call_at(&calls, "src/helpdesk.ts", 20);
    assert_eq!(items["method"], "GET");
    assert_eq!(
        items["target_url"],
        "${process.env.CATALOG_URL}/api/v1/items"
    );
    assert_eq!(
        items["path"], "${process.env.CATALOG_URL}/api/v1/items",
        "an undeclared env-var BASE is still kept verbatim: the whole-URL rule \
         reads the fallback, and this target states its own path"
    );
}

/// carrick#632: the same URL at a site the extraction returns no row for. The
/// resolution above is a rewrite, so it had nothing to act on and the call was
/// absent from the index entirely, not merely wrong.
#[test]
fn a_whole_url_site_the_extraction_answered_nothing_for_is_recorded() {
    let calls = calls();

    let ask = call_at(&calls, "src/toolset.ts", 12);
    assert_eq!(
        ask["method"], "POST",
        "the method is the literal in the call's own options bag"
    );
    assert_eq!(
        ask["target_url"], "${process.env.SERVICE_ASK_URL}/api/ask",
        "the env var supplies the origin and the fallback literal supplies the path"
    );
    assert_eq!(ask["path"], "/api/ask");
}

/// carrick#632 (the live shape): the same call at a site extraction DID answer,
/// paraphrasing the binding as the bare env-var name. #633 read that row as
/// covering the site and emitted nothing, so the call stayed keyed on an
/// env-var origin and `get_operation(POST /api/ask)` could not find it.
#[test]
fn a_whole_url_site_the_extraction_answered_wrongly_is_corrected() {
    let calls = calls();

    let ask = call_at(&calls, "src/answered.ts", 11);
    assert_eq!(ask["method"], "POST");
    assert_eq!(
        ask["target_url"], "${process.env.SUPPORT_ASK_URL}/api/ask",
        "the analyzer's paraphrase is corrected to what the binding states"
    );
    assert_eq!(
        ask["path"], "/api/ask",
        "the call is findable under the route it requests, the way the same \
         fetch against the fallback literal already was"
    );
}
