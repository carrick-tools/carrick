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
const EXITS: &str = "src/use-exits.ts";

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

/// Every `method target` stated at the caller on `line` of `use-branches.ts`,
/// sorted.
fn rows_at(line: i64) -> Vec<String> {
    rows_in(CALLERS, line)
}

/// Every `method target` stated at the caller on `line` of `file`, sorted.
fn rows_in(file: &str, line: i64) -> Vec<String> {
    let mut rows: Vec<String> = calls()
        .iter()
        .filter(|call| call["file"].as_str() == Some(file) && call["line"].as_i64() == Some(line))
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
    assert_eq!(rows_at(20), ["POST /api/items.list"]);
}

#[test]
fn a_url_local_reassigned_after_its_initialiser_is_read_at_every_caller() {
    assert_eq!(rows_at(21), ["POST /api/items.list"]);
}

#[test]
fn a_url_reading_a_branch_set_local_or_the_path_is_read() {
    let rows = rows_at(22);
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
    assert_eq!(rows_at(20), ["POST /api/items.list"]);
    // It is true for an absolute URL: only the URL itself is sent, an
    // outside host this projection holds no call for, so the based way
    // (`/api/https://…`) states nothing here.
    assert_eq!(rows_at(23), Vec::<String>::new());
}

#[test]
fn a_local_written_inside_a_callback_stays_unread() {
    assert_eq!(rows_at(24), Vec::<String>::new());
}

#[test]
fn the_branching_client_states_one_row_at_its_caller() {
    // `verb === "GET"` is filled `"POST"` by `post`, a hop away, and
    // `path.match(/^http/)` is false for `/items.list`.
    assert_eq!(rows_at(25), ["POST /api/items.list"]);
}

#[test]
fn a_local_written_from_another_in_one_branch_is_read_per_way() {
    // `target` is set from `query` in the branch that sets `query` just
    // before: the read of `query` there sees that write, and both ways of
    // `target` key the one route.
    assert_eq!(rows_at(26), ["POST /api/items.list"]);
}

#[test]
fn a_test_of_a_key_the_caller_leaves_to_a_hops_default_is_decided_by_it() {
    // `options.mode === "next"` reads a key neither the URL nor the method
    // reads. `post` writes `mode: "plain"` before spreading its caller's
    // options: a caller that omits them, or writes them without the key,
    // leaves `"plain"`, which rules the `/v2` way out.
    assert_eq!(rows_at(27), ["POST /api/items.list"]);
    assert_eq!(rows_at(28), ["POST /api/items.list"]);
    // A caller that writes the key decides the test with it.
    assert_eq!(rows_at(29), ["POST /api/v2/items.list"]);
}

/// Each row at `line` of `file` keys `route` (the query, when one is
/// written, keys the same route), and there is exactly one.
fn one_row_keying(file: &str, line: i64, route: &str) {
    let rows = rows_in(file, line);
    assert_eq!(rows.len(), 1, "one row at line {line}: {rows:?}");
    let row = &rows[0];
    assert!(
        row == route
            || row
                .strip_prefix(route)
                .is_some_and(|rest| rest.starts_with('?')),
        "line {line} keys {route}: {rows:?}"
    );
}

#[test]
fn a_branch_that_returns_sends_nothing_after_it() {
    assert_eq!(rows_in(EXITS, 32), ["POST /api/items.list"]);
}

#[test]
fn a_branch_a_caller_takes_and_that_returns_sends_nothing() {
    // `verb === "GET"` holds for this caller, and that branch returns.
    assert_eq!(rows_in(EXITS, 33), Vec::<String>::new());
}

#[test]
fn an_empty_initialiser_falls_through_or() {
    assert_eq!(rows_in(EXITS, 34), ["POST /api/items.list"]);
}

#[test]
fn a_null_initialiser_falls_through_or() {
    assert_eq!(rows_in(EXITS, 35), ["POST /api/items.list"]);
}

#[test]
fn an_empty_initialiser_does_not_fall_through_nullish() {
    // `"" ?? path` is `""`: the base alone is sent.
    assert_eq!(rows_in(EXITS, 36), ["POST /api"]);
}

#[test]
fn a_write_after_the_send_is_not_sent() {
    assert_eq!(rows_in(EXITS, 37), ["POST /api/items.list"]);
}

#[test]
fn a_local_written_in_a_loop_stays_unread() {
    // Guard: passes on main.
    assert_eq!(rows_in(EXITS, 38), Vec::<String>::new());
}

#[test]
fn a_closure_reading_a_written_local_stays_unread() {
    // Guard: passes on main.
    assert_eq!(rows_in(EXITS, 39), Vec::<String>::new());
}

#[test]
fn a_branch_that_throws_sends_nothing_after_it() {
    assert_eq!(rows_in(EXITS, 40), ["POST /api/items.list"]);
}

#[test]
fn a_local_unwritten_on_the_callers_way_states_nothing() {
    // Guard: passes on main.
    assert_eq!(rows_in(EXITS, 41), Vec::<String>::new());
}

#[test]
fn a_url_local_extended_from_itself_is_read_per_way() {
    // carrick#3986: `target = target + "?q=1"` reads `target` as the ways
    // before the write. `post` fills `verb` with `"POST"`: the GET way is
    // ruled out.
    assert_eq!(rows_in(EXITS, 42), ["POST /api/items.list"]);
    // A direct `"GET"` keeps only the extended way.
    one_row_keying(EXITS, 43, "GET /api/items.list");
}

#[test]
fn a_test_in_the_same_block_decides_its_way() {
    assert_eq!(rows_in(EXITS, 44), ["POST /api/items.list"]);
}

/// Callers of requests behind tests on their path (carrick#4093).
const PATH_GUARDS: &str = "src/use-path-guards.ts";

#[test]
fn a_caller_that_takes_the_exit_before_a_send_is_not_given_its_row() {
    // `if (verb === "GET") return null;` before `fetch(base + path)`.
    assert_eq!(rows_in(PATH_GUARDS, 13), Vec::<String>::new());
    // A caller that passes the test keeps its row.
    assert_eq!(rows_in(PATH_GUARDS, 14), ["POST /api/items.list"]);
}

#[test]
fn a_test_the_caller_does_not_decide_keeps_the_row() {
    // `verb` is the caller's own parameter, and nothing calls the caller.
    assert_eq!(rows_in(PATH_GUARDS, 15), ["POST /api/items.list"]);
}

#[test]
fn a_branch_the_caller_picks_states_only_its_own_request() {
    // `if (kind === "PUSH") return this.post("/sync/push");` then
    // `this.post("/sync/pull")`.
    assert_eq!(rows_in(PATH_GUARDS, 16), ["POST /api/sync/push"]);
    assert_eq!(rows_in(PATH_GUARDS, 17), ["POST /api/sync/pull"]);
    // Each call inside the helper states its own request, as before.
    assert_eq!(
        rows_in("src/clients/path-guards.ts", 24),
        ["POST /api/sync/push"]
    );
    assert_eq!(
        rows_in("src/clients/path-guards.ts", 26),
        ["POST /api/sync/pull"]
    );
}

#[test]
fn a_prefix_test_of_an_options_key_that_throws_is_decided_by_the_caller() {
    assert_eq!(rows_in(PATH_GUARDS, 18), Vec::<String>::new());
    assert_eq!(rows_in(PATH_GUARDS, 19), ["POST /api/items.list"]);
}

#[test]
fn a_literal_url_after_an_exit_is_stated_only_for_callers_that_pass() {
    assert_eq!(rows_in(PATH_GUARDS, 20), Vec::<String>::new());
    assert_eq!(rows_in(PATH_GUARDS, 21), ["PUT /api/literal"]);
}

#[test]
fn a_query_written_under_a_truthiness_test_states_one_row() {
    // carrick#3984's snippet: both ways key one route, one row. The URL
    // waits on no caller, so the row is stated at the send itself.
    one_row_keying("src/clients/exits.ts", 170, "GET /api/c4");
}
