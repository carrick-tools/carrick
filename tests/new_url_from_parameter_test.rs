//! S8 slice (carrick#3679): a wrapper that builds its URL with
//! `new URL(path, base)` from its own parameter states its request at each
//! caller. The caller's path is the route; the base is not asserted.
//!
//! Deterministic end to end. The LLM is replayed from `__llm__/` and every
//! cassette is empty, so each row asserted here is read off the source by the
//! request summaries.
//!
//! See `tests/fixtures/new-url-from-parameter/` for the shapes.

use std::process::Command;
use std::sync::OnceLock;

fn calls() -> &'static [serde_json::Value] {
    static CALLS: OnceLock<Vec<serde_json::Value>> = OnceLock::new();
    CALLS.get_or_init(|| {
        let repo = env!("CARGO_MANIFEST_DIR");
        let fixture = format!("{repo}/tests/fixtures/new-url-from-parameter");
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
    })
}

fn rows_at(file: &str, line: i64) -> Vec<&'static serde_json::Value> {
    calls()
        .iter()
        .filter(|call| call["file"].as_str() == Some(file) && call["line"].as_i64() == Some(line))
        .collect()
}

/// The one row at `file:line`, read off the source, sending `method` to the
/// route `path`.
fn assert_row(file: &str, line: i64, method: &str, path: &str) {
    let rows = rows_at(file, line);
    assert_eq!(
        rows.len(),
        1,
        "expected one row at {file}:{line} ({method} {path}), got {rows:#?}; all rows: {:#?}",
        calls()
    );
    let row = rows[0];
    assert_eq!(
        row["resolution_source"].as_str(),
        Some("request_summary"),
        "{file}:{line} is read off the source: {row:#}"
    );
    assert_eq!(
        row["method"].as_str(),
        Some(method),
        "{file}:{line}: {row:#}"
    );
    assert_eq!(row["path"].as_str(), Some(path), "{file}:{line}: {row:#}");
}

#[test]
fn a_url_built_with_new_url_from_a_parameter_is_read_at_the_caller() {
    // `fetch(new URL(path, base).toString())` and `fetch(new URL(path, base))`.
    assert_row("src/use-urls.ts", 4, "POST", "/items.remove");
    assert_row("src/use-urls.ts", 5, "POST", "/items.remove");
}

#[test]
fn a_builder_returning_new_url_from_its_parameters_is_read() {
    assert_row("src/use-urls.ts", 6, "POST", "/items.drop");
}

#[test]
fn a_fetch_service_resolving_against_a_runtime_base_is_read_at_the_caller() {
    assert_row("src/use-fetch-service.ts", 7, "POST", "/api/items.list");
}

#[test]
fn a_fetch_service_taken_from_an_own_container_is_read_at_the_caller() {
    assert_row("src/container.ts", 13, "POST", "/api/items.list");
}

#[test]
fn an_absolute_url_through_new_url_is_that_url() {
    let rows = rows_at("src/use-urls.ts", 11);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(
        rows[0]["target_url"].as_str(),
        Some("https://remote.example.com/remote.remove"),
        "{:#}",
        rows[0]
    );
}

#[test]
fn a_path_without_a_leading_slash_states_no_row() {
    let rows = rows_at("src/use-urls.ts", 17);
    assert!(rows.is_empty(), "expected no row at line 17, got {rows:#?}");
}
