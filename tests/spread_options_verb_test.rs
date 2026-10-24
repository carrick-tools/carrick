//! carrick#1603: a helper that spreads its caller's request options into its
//! `fetch` states no verb, so the verb at a call through it stays the model's.
//!
//! Each client calls an imported helper and passes its request options one
//! property deep. The replayed model answer states the verb the client passes.
//! What the helper does with those options decides whether the scanner may
//! overrule it:
//!
//! - `sse.ts` spreads them into the `fetch` an event source makes, beside a
//!   literal header. The one header used to read as a bag naming no method, so
//!   a GET, and the client's PATCH was indexed as a GET.
//! - `upload.ts` writes its own `method` and spreads the options AFTER it, so
//!   the caller's method overwrites the helper's. It used to read as the
//!   helper's POST.
//! - `poll.ts` spreads the options and writes `method: "GET"` after them, so
//!   every request it sends is a GET whatever its caller passes.
//! - `status.ts` forwards nothing of its caller's and names no method: a GET.
//!
//! The first two keep the model's verb. The last two used to take the
//! helper's; that pass is deleted and they keep the model's too (#2676). The
//! test asserts every row exactly, those two at the verb they hold now, so
//! their move stays visible and the fix for #2676 must turn them back.
//!
//! See `tests/fixtures/spread-options-verb/README.md` for the answer key.

use std::process::Command;

#[test]
fn a_helper_that_spreads_its_caller_s_options_does_not_overrule_the_caller_s_verb() {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/spread-options-verb");
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
    let mut rows: Vec<(String, i64, String)> = projection["calls"]
        .as_array()
        .expect("projection carries a calls array")
        .iter()
        .map(|call| {
            (
                call["file"].as_str().unwrap_or_default().to_string(),
                call["line"].as_i64().unwrap_or_default(),
                call["method"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    rows.sort();

    let expected: Vec<(String, i64, String)> = [
        ("src/asset-client.ts", 4, "PUT"),
        ("src/build-client.ts", 7, "PATCH"),
        // Answer key: GET. `poll.ts` writes `method: "GET"` after the spread;
        // the row keeps the model's DELETE (#2676).
        ("src/job-client.ts", 4, "DELETE"),
        // Answer key: GET. `status.ts` forwards nothing and names no method;
        // the row keeps the model's POST (#2676).
        ("src/status-client.ts", 4, "POST"),
    ]
    .into_iter()
    .map(|(file, line, method)| (file.to_string(), line, method.to_string()))
    .collect();
    assert_eq!(rows, expected, "{projection:#}");
}
