//! carrick#1624: a call handed a payload and no method is not read as a GET,
//! so a module holding one does not overrule the verb at a call through it.
//!
//! Each screen calls a member of an imported helper object, and the replayed
//! model answer states a verb there. What the helper module's own calls prove
//! decides whether the scanner may overrule it:
//!
//! - `lib/orders.ts` hands `{ data }` to a decode helper, `lib/refunds.ts` logs
//!   `{ body }`, and `lib/profile.ts` hands `{ data }` to a form helper. Each
//!   real request goes through a client member the scanner cannot see, and the
//!   payload call used to read as a request naming no method, so a GET.
//! - `lib/catalog.ts` issues a `fetch` whose options name no method and no
//!   payload: a GET.
//! - `lib/uploads.ts` sends a payload with `method: "POST"` written beside it.
//!
//! The first three keep the model's verb; the last two take the helper's.
//!
//! See `tests/fixtures/payload-without-method/README.md` for the answer key.

use std::process::Command;

#[test]
fn a_payload_with_no_method_does_not_state_a_get() {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/payload-without-method");
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
        ("src/checkout.ts", 4, "POST"),
        ("src/profile-screen.ts", 4, "PUT"),
        ("src/refund-screen.ts", 4, "POST"),
        ("src/search-screen.ts", 4, "GET"),
        ("src/upload-screen.ts", 4, "POST"),
    ]
    .into_iter()
    .map(|(file, line, method)| (file.to_string(), line, method.to_string()))
    .collect();
    assert_eq!(rows, expected, "{projection:#}");
}
