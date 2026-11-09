//! A model row at a hook call is not a second row for the request a call
//! inside the hook's callback states.
//!
//! `apps/web/src/feed-hooks.ts` hands `useMutation` a callback that calls one
//! of the app's own functions, which calls the client package, which writes
//! the request. The call inside the callback is the leaf call: its row states
//! the request and carries what that call returns. The model answers at the
//! `useMutation(` call instead, whose value is the hook's own state.
//!
//! - `useRefreshFeed`: the model names the path's holes its own way
//!   (`:boardId/:feedId` against the client's `:board/:feed`). Both rows used
//!   to stand, and the type check judged the response against the hook's
//!   state.
//! - `useArchiveFeed`: the model names them as the client does. Its row at
//!   the hook holds the leaf, so the only row is the hook's. Recorded as it
//!   is today, pending the ruling on carrick#3640 (the same rule the
//!   `call-through-client` fixture's `nested.ts:7` answer key pins).
//!
//! The refresh request now has one row, at the call inside the callback, and
//! its hook's line holds none. See `tests/fixtures/hook-row-over-leaf/README.md`.

use std::process::Command;
use std::sync::OnceLock;

const HOOKS: &str = "apps/web/src/feed-hooks.ts";

/// The scanner's JSON projection of the fixture, scanned once.
fn projection() -> &'static serde_json::Value {
    static SCAN: OnceLock<serde_json::Value> = OnceLock::new();
    SCAN.get_or_init(|| {
        let repo = env!("CARGO_MANIFEST_DIR");
        let fixture = format!("{repo}/tests/fixtures/hook-row-over-leaf");
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

/// Every call row the scan indexed in the hooks file, sorted by line.
fn hook_rows() -> Vec<&'static serde_json::Value> {
    let mut rows: Vec<&serde_json::Value> = projection()["calls"]
        .as_array()
        .expect("projection carries a calls array")
        .iter()
        .filter(|call| call["file"] == HOOKS)
        .collect();
    rows.sort_by_key(|call| call["line"].as_i64());
    rows
}

/// `(line, method, path, source)` of one row.
fn summary(row: &serde_json::Value) -> (i64, String, String, String) {
    let text = |field: &str| row[field].as_str().unwrap_or_default().to_string();
    (
        row["line"].as_i64().unwrap_or_default(),
        text("method"),
        text("path"),
        text("resolution_source"),
    )
}

fn expected(line: i64, path: &str, source: &str) -> (i64, String, String, String) {
    (
        line,
        "POST".to_string(),
        path.to_string(),
        source.to_string(),
    )
}

/// Each request is one row, at the call inside its hook's callback; no row
/// sits on either hook's line (7, 17), so no type anchored there is judged.
/// The model names the refresh path's holes its own way (7) and the archive
/// path's exactly as the client does (17); both give way (carrick#3640).
#[test]
fn a_hook_row_beside_the_leaf_row_of_its_request_goes() {
    let rows: Vec<_> = hook_rows().into_iter().map(summary).collect();
    assert_eq!(
        rows,
        vec![
            expected(
                9,
                "/api/v1/boards/:board/feeds/:feed/refresh",
                "request_summary"
            ),
            expected(
                19,
                "/api/v1/boards/:board/feeds/:feed/archive",
                "request_summary"
            ),
        ],
        "{rows:#?}"
    );
}

/// The leaf row keeps what its call returns: the client's value, not the
/// hook's state.
#[test]
fn the_leaf_row_carries_its_own_calls_type() {
    let rows = hook_rows();
    let refresh = rows
        .iter()
        .find(|row| row["line"] == 9)
        .expect("a row at the call inside the callback");
    assert_eq!(
        refresh["expanded_definition"].as_str(),
        Some("{ data: unknown; }"),
        "{refresh:#?}"
    );
}
