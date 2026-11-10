//! A URL local written more than once (`let target; if (…) target = path;
//! else target = base + path;`, then `fetch(target)`) states one request per
//! write at each caller of the wrapper, and a write under a test of a
//! parameter the caller fills with a literal is kept or dropped by that test
//! (carrick#3679).
//!
//! Deterministic end to end: the LLM is replayed from `__llm__/` with empty
//! cassettes, so every row asserted here is read off the source by the request
//! summaries. See `tests/fixtures/wrapper-callers/README.md` for the shapes.

use std::process::Command;
use std::sync::OnceLock;

const CALLERS: &str = "src/use-branches.ts";

fn calls() -> &'static [serde_json::Value] {
    static CALLS: OnceLock<Vec<serde_json::Value>> = OnceLock::new();
    CALLS.get_or_init(|| {
        let repo = env!("CARGO_MANIFEST_DIR");
        let fixture = format!("{repo}/tests/fixtures/wrapper-callers");
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

/// Every `method target` stated at the caller on `line`, sorted.
fn rows_at(line: i64) -> Vec<String> {
    let mut rows: Vec<String> = calls()
        .iter()
        .filter(|call| {
            call["file"].as_str() == Some(CALLERS) && call["line"].as_i64() == Some(line)
        })
        .map(|row| {
            assert_eq!(
                row["resolution_source"].as_str(),
                Some("request_summary"),
                "line {line} is read off the source: {row:#}"
            );
            format!(
                "{} {}",
                row["method"].as_str().unwrap_or("?"),
                row["target_url"].as_str().unwrap_or("?")
            )
        })
        .collect();
    rows.sort();
    rows
}

#[test]
fn a_url_local_set_in_both_branches_is_read_at_every_caller() {
    assert_eq!(rows_at(18), ["POST /api/items.list"]);
}

#[test]
fn a_url_local_reassigned_after_its_initialiser_is_read_at_every_caller() {
    assert_eq!(rows_at(19), ["POST /api/items.list"]);
}

#[test]
fn a_url_reading_a_branch_set_local_or_the_path_is_read() {
    let rows = rows_at(20);
    assert!(!rows.is_empty(), "no row at the caller");
    for row in &rows {
        assert!(
            row == "POST /api/items.list" || row == "POST /api/items.list?q=1",
            "every way keys the one route: {rows:?}"
        );
    }
}

#[test]
fn a_way_the_callers_literal_rules_out_states_no_row() {
    // The prefix test is false for a path: the absolute way is dropped, so
    // no bare `/items.list` is stated.
    assert_eq!(rows_at(18), ["POST /api/items.list"]);
    // It is true for an absolute URL: only the URL itself is sent, an
    // outside host this projection holds no call for, so the based way
    // (`/api/https://…`) states nothing here.
    assert_eq!(rows_at(21), Vec::<String>::new());
}

#[test]
fn a_local_written_inside_a_callback_stays_unread() {
    assert_eq!(rows_at(22), Vec::<String>::new());
}

#[test]
fn the_branching_client_states_one_row_at_its_caller() {
    // `verb === "GET"` is filled `"POST"` by `post`, a hop away, and
    // `path.match(/^http/)` is false for `/items.list`.
    assert_eq!(rows_at(23), ["POST /api/items.list"]);
}

#[test]
fn a_local_written_from_another_the_same_statement_writes_stays_unread() {
    // `target` is set from `query` in the branch that sets `query` too:
    // nothing says what `query` holds there, so `target` states nothing,
    // whichever order the two locals are read in.
    assert_eq!(rows_at(24), Vec::<String>::new());
}
