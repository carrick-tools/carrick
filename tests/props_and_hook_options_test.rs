//! carrick#1949: a URL handed in through a hook's options or a component's
//! props, and fetched inside a callback or a closure, is stated where the
//! caller fills it in.
//!
//! Drives the real scanner binary over `tests/fixtures/props-and-hook-options/`
//! with the model stage off, so every row is one a pass states as a fact.
//! The fixture's README is the answer key.

use std::process::Command;
use std::sync::OnceLock;

fn calls() -> &'static [serde_json::Value] {
    static CALLS: OnceLock<Vec<serde_json::Value>> = OnceLock::new();
    CALLS.get_or_init(|| {
        let repo = env!("CARGO_MANIFEST_DIR");
        let fixture = format!("{repo}/tests/fixtures/props-and-hook-options");
        let cache = tempfile::tempdir().expect("cache dir");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
        cmd.arg(&fixture)
            .env("CARRICK_MOCK_ALL", "1")
            .env("CARRICK_NO_MODEL", "1")
            .env("CARRICK_OUTPUT_JSON", "1")
            .env("CARRICK_SKIP_INTENTS", "1")
            .env("CARRICK_CACHE_DIR", cache.path())
            // Nothing here reads a type.
            .env("CARRICK_ALLOW_MISSING_TYPES", "1");
        for var in [
            "GITHUB_REPOSITORY",
            "GITHUB_ACTIONS",
            "CI",
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
        ] {
            cmd.env_remove(var);
        }
        let output = cmd.output().expect("failed to spawn carrick binary");
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
    })
}

/// `(method, path)` of every row at `file:line`, sorted.
fn rows_at(file: &str, line: i64) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = calls()
        .iter()
        .filter(|call| {
            call["line"].as_i64() == Some(line)
                && call["file"]
                    .as_str()
                    .is_some_and(|f| f.ends_with(&format!("src/{file}")))
        })
        .map(|call| {
            (
                call["method"].as_str().unwrap_or_default().to_string(),
                call["path"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    rows.sort();
    rows
}

fn assert_rows(file: &str, line: i64, expected: &[(&str, &str)]) {
    let expected: Vec<(String, String)> = expected
        .iter()
        .map(|(method, path)| (method.to_string(), path.to_string()))
        .collect();
    assert_eq!(
        rows_at(file, line),
        expected,
        "{file}:{line}; every row: {:#?}",
        calls()
    );
}

/// Shape 1 on the ticket: the hook's caller writes the URL into the hook's
/// options, and the request is made in a callback handed to a package's hook.
#[test]
fn a_url_in_a_hooks_options_is_stated_where_the_hook_is_called() {
    assert_rows(
        "BoardPage.tsx",
        5,
        &[("POST", "/resources/boards/:boardId/widgets")],
    );
    assert_rows("useBoardEditor.ts", 6, &[]);
}

/// Shape 2 on the ticket: the element's props carry the URL, and the
/// requests are made in a closure held in a local and in a method handed to
/// a package's hook.
#[test]
fn a_url_in_a_components_props_is_stated_where_the_element_is_rendered() {
    assert_rows(
        "AgentPage.tsx",
        8,
        &[
            ("POST", "/resources/agents/:agentId/chat"),
            ("PUT", "/resources/agents/:agentId/chat"),
        ],
    );
    assert_rows("AgentPanel.tsx", 4, &[]);
}

/// A component that hands its own prop on leaves the URL to its own caller.
#[test]
fn a_prop_handed_on_is_stated_where_it_is_written() {
    assert_rows("AgentPage.tsx", 17, &[]);
    assert_rows(
        "AgentPage.tsx",
        9,
        &[
            ("POST", "/resources/agents/main/chat"),
            ("PUT", "/resources/agents/main/chat"),
        ],
    );
}

/// `props.noticesUrl` read in a closure is the key the element writes.
#[test]
fn a_key_of_the_props_object_read_in_a_closure_is_the_attribute() {
    assert_rows("AgentPage.tsx", 10, &[("DELETE", "/api/notices/:id")]);
}

/// What does not fill in a request states nothing: a package's component,
/// and a value glued inside a segment, which states a path the source does
/// not.
#[test]
fn nothing_else_is_stated_at_an_element() {
    assert_rows("AgentPage.tsx", 11, &[]);
    assert!(
        calls()
            .iter()
            .all(|call| !call["path"].as_str().unwrap_or_default().contains("/at")),
        "no row states the glued segment: {:#?}",
        calls()
    );
}
