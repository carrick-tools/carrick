//! carrick#2049: a request whose method is a parameter the source declares as
//! a closed set of HTTP verbs states one row per verb at the request's line.
//!
//! The model is replayed from `__llm__/`, and its answer for every request in
//! `src/members.ts` is the one the field showed: the target, no method, which
//! the scanner indexes as a GET. The method is the parameter's, so the source
//! says what it can be, and the rows say that instead.
//!
//! See `tests/fixtures/union-method-param/README.md` for the answer key.

use std::process::Command;
use std::sync::OnceLock;

fn calls() -> &'static [serde_json::Value] {
    static CALLS: OnceLock<Vec<serde_json::Value>> = OnceLock::new();
    CALLS.get_or_init(|| {
        let repo = env!("CARGO_MANIFEST_DIR");
        let fixture = format!("{repo}/tests/fixtures/union-method-param");
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

/// `(method, source)` of every row at `file:line`, sorted.
fn rows_at(file: &str, line: i64) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = calls()
        .iter()
        .filter(|call| call["file"].as_str() == Some(file) && call["line"].as_i64() == Some(line))
        .map(|call| {
            (
                call["method"].as_str().unwrap_or_default().to_string(),
                call["resolution_source"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    rows.sort();
    rows
}

fn targets_at(file: &str, line: i64) -> Vec<String> {
    let mut targets: Vec<String> = calls()
        .iter()
        .filter(|call| call["file"].as_str() == Some(file) && call["line"].as_i64() == Some(line))
        .map(|call| call["target_url"].as_str().unwrap_or_default().to_string())
        .collect();
    targets.sort();
    targets.dedup();
    targets
}

fn stated(methods: &[&str]) -> Vec<(String, String)> {
    methods
        .iter()
        .map(|method| (method.to_string(), "request_summary".to_string()))
        .collect()
}

#[test]
fn a_union_of_verbs_written_at_the_parameter_states_each_verb() {
    assert_eq!(
        rows_at("src/members.ts", 4),
        stated(&["DELETE", "POST"]),
        "the model's GET is replaced by the verbs the parameter is declared with"
    );
    assert_eq!(
        targets_at("src/members.ts", 4),
        vec!["/api/groups/${groupId}/members"]
    );
}

#[test]
fn an_alias_the_file_declares_is_the_same_statement() {
    assert_eq!(rows_at("src/members.ts", 11), stated(&["DELETE", "POST"]));
}

#[test]
fn a_url_held_in_a_binding_is_stated_the_same_way() {
    assert_eq!(rows_at("src/members.ts", 16), stated(&["DELETE", "POST"]));
}

#[test]
fn when_every_call_writes_a_verb_the_request_line_states_those() {
    // `archive` is declared `"POST" | "DELETE"` and called once, with
    // "DELETE": that is the one verb it sends, at its line and at the call.
    assert_eq!(rows_at("src/members.ts", 37), stated(&["DELETE"]));
    assert_eq!(rows_at("src/caller.ts", 4), stated(&["DELETE"]));
}

#[test]
fn a_call_nothing_reads_leaves_every_declared_verb() {
    // `touch` is called with "POST" and with a parameter of the caller's:
    // the second may be either verb.
    assert_eq!(rows_at("src/members.ts", 41), stated(&["DELETE", "POST"]));
    assert_eq!(rows_at("src/caller.ts", 8), stated(&["POST"]));
    assert_eq!(rows_at("src/caller.ts", 9), Vec::new());
}

#[test]
fn a_method_the_source_does_not_declare_states_no_verb() {
    let model = |method: &str| vec![(method.to_string(), "model".to_string())];
    // `method: string`: the source says nothing of the verbs.
    assert_eq!(rows_at("src/members.ts", 20), model("GET"));
    // An optional parameter may arrive as nothing.
    assert_eq!(rows_at("src/members.ts", 24), model("GET"));
    // A parameter the body assigns again holds something else by then.
    assert_eq!(rows_at("src/members.ts", 29), model("GET"));
    // A URL that leads with a base is read by the passes that settle a
    // base: this one keeps the row it had.
    assert_eq!(rows_at("src/members.ts", 33), model("GET"));
    // A function written inside another has callers this pass does not
    // resolve: the model's reading (POST) stands.
    assert_eq!(rows_at("src/members.ts", 46), model("POST"));
}
