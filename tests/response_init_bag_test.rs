//! carrick#1986: a call handed a path and an options object a response's
//! init could equally be (`headers`, `status`) states a request only where
//! the source says it sends one.
//!
//! Drives the real scanner binary over `tests/fixtures/response-init-bag/`
//! with the model stage off, so every row is one a pass states as a fact.
//! The fixture's README is the answer key.

use std::process::Command;
use std::sync::OnceLock;

fn calls() -> &'static [serde_json::Value] {
    static CALLS: OnceLock<Vec<serde_json::Value>> = OnceLock::new();
    CALLS.get_or_init(|| {
        let repo = env!("CARGO_MANIFEST_DIR");
        let fixture = format!("{repo}/tests/fixtures/response-init-bag");
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

fn rows_at(file: &str, line: i64) -> Vec<&'static serde_json::Value> {
    calls()
        .iter()
        .filter(|call| {
            call["line"].as_i64() == Some(line)
                && call["file"]
                    .as_str()
                    .is_some_and(|f| f.ends_with(&format!("apps/web/src/{file}")))
        })
        .collect()
}

fn assert_no_row(file: &str, line: i64, why: &str) {
    let rows = rows_at(file, line);
    assert!(rows.is_empty(), "{file}:{line} {why}; got {rows:#?}");
}

/// A `GET` to `path` at `file:line`, stated as a fact.
fn assert_get(file: &str, line: i64, path: &str) {
    let rows = rows_at(file, line);
    assert!(
        rows.iter()
            .any(|row| row["method"].as_str() == Some("GET") && row["path"].as_str() == Some(path)),
        "{file}:{line} sends GET {path}; expected a row stating it, got {rows:#?} \
         (every row: {:#?})",
        calls()
    );
}

#[test]
fn a_returned_response_helper_states_no_request() {
    assert_no_row(
        "security.server.ts",
        10,
        "builds a redirect from a path and a headers bag and returns it; nothing is sent",
    );
    assert_no_row(
        "settings.ts",
        7,
        "calls a helper that builds a redirect; nothing is sent",
    );
}

#[test]
fn a_thrown_response_helper_states_no_request() {
    assert_no_row(
        "security.server.ts",
        17,
        "throws a redirect built from a path and a status and headers bag",
    );
    assert_no_row(
        "settings.ts",
        4,
        "calls a helper that throws a redirect; nothing is sent",
    );
}

#[test]
fn a_waited_on_call_with_the_same_bag_is_a_request() {
    assert_get("profile.ts", 19, "/api/profile");
    assert_get("profile.ts", 26, "/api/orders");
}

#[test]
fn the_platform_fetch_with_the_same_bag_is_a_request() {
    assert_get("profile.ts", 38, "/api/notices");
}

#[test]
fn a_function_the_repository_defines_with_the_same_bag_is_a_request() {
    assert_get("profile.ts", 32, "/api/preferences");
}

#[test]
fn a_returned_package_call_with_the_same_bag_is_not_stated() {
    assert_no_row(
        "profile.ts",
        42,
        "is written exactly like the redirect at security.server.ts:10; the source \
         does not say which it is, so no fact is stated",
    );
}
