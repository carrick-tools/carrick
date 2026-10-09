//! carrick#2051: a method and a URL chosen by the same condition are paired
//! branch with branch.
//!
//! `src/notes.ts` saves a note with `PATCH /api/notes/:id` when one exists
//! and `POST /api/notes` when it does not, writing the choice twice: once for
//! the method and once for the URL. The row the field showed was the method of
//! one branch with the URL of the other (`POST /api/notes/:id`), which no
//! route serves.
//!
//! The model is replayed from `__llm__/`; its single row per site is the one
//! the field showed. The sites below it write the choice in a way the source
//! does not pair, and state no structural row.
//!
//! See `tests/fixtures/conditional-request/README.md` for the answer key.

use std::process::Command;
use std::sync::OnceLock;

fn calls() -> &'static [serde_json::Value] {
    static CALLS: OnceLock<Vec<serde_json::Value>> = OnceLock::new();
    CALLS.get_or_init(|| {
        let repo = env!("CARGO_MANIFEST_DIR");
        let fixture = format!("{repo}/tests/fixtures/conditional-request");
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
        let projection: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("scanner output was not valid JSON");
        projection["calls"]
            .as_array()
            .expect("projection carries a calls array")
            .clone()
    })
}

/// `(method, target, source)` of every row at `file:line`, sorted.
fn rows_at(file: &str, line: i64) -> Vec<(String, String, String)> {
    let mut rows: Vec<(String, String, String)> = calls()
        .iter()
        .filter(|call| call["file"].as_str() == Some(file) && call["line"].as_i64() == Some(line))
        .map(|call| {
            let text = |key: &str| call[key].as_str().unwrap_or_default().to_string();
            (
                text("method"),
                text("target_url"),
                text("resolution_source"),
            )
        })
        .collect();
    rows.sort();
    rows
}

fn stated(rows: &[(&str, &str)]) -> Vec<(String, String, String)> {
    rows.iter()
        .map(|(method, target)| {
            (
                method.to_string(),
                target.to_string(),
                "request_summary".to_string(),
            )
        })
        .collect()
}

const SAVE: [(&str, &str); 2] = [
    ("PATCH", "/api/notes/${existing.id}"),
    ("POST", "/api/notes"),
];

#[test]
fn a_method_and_a_url_one_test_chooses_are_paired_across_branches() {
    // Both held in locals.
    assert_eq!(rows_at("src/notes.ts", 6), stated(&SAVE));
}

#[test]
fn the_same_test_written_in_the_call_pairs_the_same_way() {
    assert_eq!(rows_at("src/notes.ts", 10), stated(&SAVE));
}

#[test]
fn a_method_a_test_chooses_beside_a_fixed_url_is_a_row_per_method() {
    assert_eq!(
        rows_at("src/notes.ts", 17),
        stated(&[
            ("POST", "/api/notes/reorder"),
            ("PUT", "/api/notes/reorder")
        ])
    );
}

#[test]
fn tests_that_differ_pair_nothing() {
    // Two parameters; a test that calls a function twice; a test whose name
    // the function assigns again between the two uses.
    for line in [23, 29, 36] {
        let rows = rows_at("src/notes.ts", line);
        assert!(
            rows.iter().all(|(_, _, source)| source == "model"),
            "{line}: {rows:?}"
        );
    }
}

#[test]
fn a_method_nothing_read_states_no_row() {
    assert_eq!(rows_at("src/notes.ts", 41), Vec::new());
}

#[test]
fn a_negated_test_pairs_with_its_branches_swapped() {
    // `!existing ? "POST" : "PATCH"` beside `existing ? … : …` is one test.
    assert_eq!(rows_at("src/notes.ts", 47), stated(&SAVE));
}

#[test]
fn two_tests_on_one_name_pair_nothing() {
    // `mode === "edit"` and `mode === "copy"` cannot both hold: no row
    // rather than a pairing the source cannot send.
    assert_eq!(rows_at("src/notes.ts", 53), Vec::new());
}

#[test]
fn a_shared_test_does_not_pair_two_tests_on_one_name() {
    // The method and the URL share `shared`, but the method also reads
    // `mode === "replace"` and the URL `mode === "append"`: no row rather
    // than a `PUT` to the append route.
    assert_eq!(rows_at("src/notes.ts", 59), Vec::new());
}

#[test]
fn one_else_if_chain_in_the_method_and_the_url_pairs_branch_with_branch() {
    assert_eq!(
        rows_at("src/notes.ts", 65),
        stated(&[
            ("PATCH", "/api/notes/${id}"),
            ("POST", "/api/notes"),
            ("PUT", "/api/notes/${id}/copy"),
        ])
    );
}

#[test]
fn a_chain_beside_one_of_its_own_tests_pairs_nothing() {
    // The URL is the chain, the method reads `mode === "copy"` alone: `PUT`
    // beside the `edit` branch is a pair the source cannot send.
    assert_eq!(rows_at("src/notes.ts", 71), Vec::new());
}
