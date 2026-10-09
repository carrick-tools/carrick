//! carrick#2050: a URL chosen by a conditional states one row per branch.
//!
//! `src/content.ts` builds `/api/content/<kind>/<id>/publish` from a segment a
//! test picks, in the three forms a service writes it: the segment in a
//! binding and the URL a template at the call, the URL itself in a binding,
//! and the conditional as the URL argument. Each branch writes two segments,
//! so the one-placeholder path the summaries used to state (`:segment`)
//! matched neither route.
//!
//! The model is replayed from `__llm__/`. For the two sites whose URL is
//! written at the call it states the row it states in the field: the target
//! with the conditional's placeholder.
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

const PUBLISH: [(&str, &str); 2] = [
    ("POST", "/api/content/drafts/${id}/publish"),
    ("POST", "/api/content/posts/${id}/publish"),
];

#[test]
fn a_segment_a_test_picks_states_a_row_per_branch() {
    // The segment in a binding, the URL a template written at the call.
    assert_eq!(rows_at("src/content.ts", 5), stated(&PUBLISH));
}

#[test]
fn a_url_held_in_a_binding_states_a_row_per_branch() {
    assert_eq!(rows_at("src/content.ts", 11), stated(&PUBLISH));
}

#[test]
fn a_conditional_as_the_url_states_a_row_per_branch() {
    // The model stated both routes at this site, each joined to the row that
    // states the same route: two rows, not four.
    assert_eq!(rows_at("src/content.ts", 15), stated(&PUBLISH));
}

#[test]
fn a_branch_nothing_read_is_no_path_parameter() {
    // One branch writes a path and the other is a call's value, which may
    // hold any number of segments: the slot is no one placeholder.
    assert_eq!(rows_at("src/content.ts", 21), Vec::new());
    // Written at the call, the row is the model's, which no structural pass
    // withdraws.
    let rows = rows_at("src/content.ts", 26);
    assert!(
        rows.iter().all(|(_, _, source)| source == "model"),
        "{rows:?}"
    );
}

#[test]
fn a_conditional_that_leads_the_url_is_a_base_as_it_was() {
    assert_eq!(
        rows_at("src/content.ts", 32),
        stated(&[("GET", "${base}/health")])
    );
}

#[test]
fn two_values_nothing_read_are_one_path_parameter_as_they_were() {
    assert_eq!(
        rows_at("src/content.ts", 38),
        stated(&[("GET", "/api/content/${owner}/items")])
    );
}

#[test]
fn a_query_a_test_adds_is_no_part_of_the_route() {
    assert_eq!(
        rows_at("src/content.ts", 44),
        stated(&[("GET", "/api/content")])
    );
}
