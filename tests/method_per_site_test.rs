//! carrick-cloud#1365: a call that names its own verb keeps it.
//!
//! The fixture's screen calls an imported client instance with `post` and
//! `delete`, in the line shapes the ticket reported: inside
//! `Promise.all(xs.map(...))`, inside a one-line `try` holding several
//! statements, and on both branches of a one-line `if/else`. The module that
//! creates the client issues a GET of its own.
//!
//! The replayed model answer states every method correctly, but none of its
//! `candidate_id`s joins a candidate, so every row reaches the post-join passes
//! without a span. The wrapper-shape propagation read that as a site delegating
//! to the imported module and gave all five rows the module's GET. This is a
//! regression net for that pass, not for the model.
//!
//! See `tests/fixtures/method-per-site/README.md` for the answer key.

use std::process::Command;

#[test]
fn calls_that_name_their_verb_keep_it_when_their_rows_carry_no_span() {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/method-per-site");
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
    let mut sites: Vec<(i64, String)> = projection["calls"]
        .as_array()
        .expect("projection carries a calls array")
        .iter()
        .filter(|call| call["file"] == "src/screens/TeamPanel.tsx")
        .map(|call| {
            (
                call["line"].as_i64().unwrap_or_default(),
                call["method"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    sites.sort();

    let expected: Vec<(i64, String)> = [
        (6, "GET"),
        (11, "POST"),
        (15, "DELETE"),
        (19, "POST"),
        (23, "DELETE"),
        (23, "POST"),
    ]
    .into_iter()
    .map(|(line, method)| (line, method.to_string()))
    .collect();
    assert_eq!(sites, expected, "{projection:#}");
}
