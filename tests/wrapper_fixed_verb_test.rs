//! A call through a helper whose own request writes the verb takes that verb.
//!
//! The helper in `catalog.ts` requests `POST /catalog/sync`. The screen calls
//! it and the replayed model answer states no method at the call, which would
//! index as a GET. Every row for the route must carry the helper's POST.
//!
//! See `tests/fixtures/wrapper-fixed-verb/README.md` for the answer key.

use std::process::Command;

#[test]
fn a_call_through_a_helper_that_fixes_the_verb_is_not_indexed_as_a_get() {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/wrapper-fixed-verb");
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

    let projection: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("scanner output was not valid JSON");
    let mut rows: Vec<(String, i64, String, String)> = projection["calls"]
        .as_array()
        .expect("projection carries a calls array")
        .iter()
        .map(|call| {
            (
                call["file"].as_str().unwrap_or_default().to_string(),
                call["line"].as_i64().unwrap_or_default(),
                call["method"].as_str().unwrap_or_default().to_string(),
                call["path"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    rows.sort();

    assert!(
        rows.iter()
            .any(|(file, _, method, path)| file == "src/lib/catalog.ts"
                && method == "POST"
                && path == "/catalog/sync"),
        "the helper's own request is a POST row: {projection:#}"
    );
    let off_verb: Vec<_> = rows
        .iter()
        .filter(|(_, _, method, path)| path == "/catalog/sync" && method != "POST")
        .collect();
    assert!(
        off_verb.is_empty(),
        "every row for the route carries the helper's POST, found {off_verb:?}: {projection:#}"
    );
}
