//! carrick-cloud#1365: a call spelled with its verb is indexed under that verb
//! when the model's row for it states none.
//!
//! Every call in the fixture is a verb-named member call on an imported client
//! instance (`client.post(...)`), with a template-literal URL. No deterministic
//! source states a row for that shape: the ones that state a verb all need a
//! target they can read, and a template literal gives them none. So the row is
//! the model's, and the replayed answer states no method for any of them. The
//! scanner used to index each one as a GET, the default for a missing method.
//! The call names its own verb, and that verb is what the row now carries.
//!
//! Each test covers one line shape from the ticket, and every `candidate_id` in
//! the cassettes is a real candidate, so each row joins the site it names:
//!
//! - a request inside `Promise.all(xs.map(...))`, answered once at the outer
//!   `Promise.all` candidate (which states no verb of its own) and once at the
//!   request itself;
//! - a request inside a one-line `try` holding several statements;
//! - a one-line `if/else` with a request on each branch.
//!
//! See `tests/fixtures/call-states-its-verb/README.md` for the answer key.

use std::process::Command;
use std::sync::OnceLock;

/// The fixture is scanned once; each test reads its own file's rows.
fn scanned() -> &'static serde_json::Value {
    static PROJECTION: OnceLock<serde_json::Value> = OnceLock::new();
    PROJECTION.get_or_init(|| {
        let repo = env!("CARGO_MANIFEST_DIR");
        let fixture = format!("{repo}/tests/fixtures/call-states-its-verb");
        let mock_dir = format!("{fixture}/__llm__/");
        let storage = tempfile::tempdir().expect("temp storage dir");

        let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
            .arg(&fixture)
            .env("CARRICK_MOCK_ALL", "1")
            .env("CARRICK_MOCK_FIXTURE_DIR", &mock_dir)
            .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
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
        serde_json::from_slice(&output.stdout).expect("scanner output was not valid JSON")
    })
}

/// `(line, method)` for every call the scan indexed in `file`, sorted.
fn methods_in(file: &str) -> Vec<(i64, String)> {
    let projection = scanned();
    let mut sites: Vec<(i64, String)> = projection["calls"]
        .as_array()
        .expect("projection carries a calls array")
        .iter()
        .filter(|call| call["file"] == file)
        .map(|call| {
            (
                call["line"].as_i64().unwrap_or_default(),
                call["method"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    sites.sort();
    sites
}

fn expect(sites: &[(i64, &str)]) -> Vec<(i64, String)> {
    sites
        .iter()
        .map(|(line, method)| (*line, method.to_string()))
        .collect()
}

/// Line 4's row names the outer `Promise.all` candidate, which is not a
/// request; the one request inside it states POST. Line 8's row names the
/// request itself, which states PUT.
#[test]
fn a_request_inside_promise_all_over_a_map_keeps_its_verb() {
    assert_eq!(
        methods_in("src/screens/LabelPicker.tsx"),
        expect(&[(4, "POST"), (8, "PUT")]),
        "{:#}",
        scanned()
    );
}

/// A one-line `try` holding several statements, with the request's result
/// bound (line 6) and awaited bare (line 10).
#[test]
fn a_request_inside_a_one_line_try_keeps_its_verb() {
    assert_eq!(
        methods_in("src/screens/InviteCard.tsx"),
        expect(&[(6, "POST"), (10, "PATCH")]),
        "{:#}",
        scanned()
    );
}

/// Both branches of a one-line `if/else`, each row joined to its own request.
#[test]
fn each_branch_of_a_one_line_if_else_keeps_its_verb() {
    assert_eq!(
        methods_in("src/screens/MemberToggle.tsx"),
        expect(&[(4, "DELETE"), (4, "POST")]),
        "{:#}",
        scanned()
    );
}
