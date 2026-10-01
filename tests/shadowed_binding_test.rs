//! carrick#1648: a name declared again in an inner scope holds its own value,
//! and no deterministic row states the outer one there.
//!
//! Drives the real scanner binary over `tests/fixtures/shadowed-bindings/`
//! with the model stage off, so every row is one a pass states as a fact. The
//! fixture's README is the answer key: one test per source that reads a
//! binding, each asserting the shadowed site states nothing and the control
//! site still states its row.

use std::process::Command;
use std::sync::OnceLock;

fn calls() -> &'static [serde_json::Value] {
    static CALLS: OnceLock<Vec<serde_json::Value>> = OnceLock::new();
    CALLS.get_or_init(|| {
        let repo = env!("CARGO_MANIFEST_DIR");
        let fixture = format!("{repo}/tests/fixtures/shadowed-bindings");
        let cache = tempfile::tempdir().expect("cache dir");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
        cmd.arg(&fixture)
            .env("CARRICK_MOCK_ALL", "1")
            .env("CARRICK_NO_MODEL", "1")
            .env("CARRICK_OUTPUT_JSON", "1")
            .env("CARRICK_SKIP_INTENTS", "1")
            .env("CARRICK_CACHE_DIR", cache.path())
            // Nothing here reads a type.
            .env("CARRICK_ALLOW_MISSING_TYPES", "1");
        for var in [
            "GITHUB_REPOSITORY",
            "GITHUB_ACTIONS",
            "CI",
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
        ] {
            cmd.env_remove(var);
        }
        let output = cmd.output().expect("failed to spawn carrick binary");
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

fn rows_at(file: &str, line: i64) -> Vec<&'static serde_json::Value> {
    calls()
        .iter()
        .filter(|call| {
            call["line"].as_i64() == Some(line)
                && call["file"]
                    .as_str()
                    .is_some_and(|f| f.ends_with(&format!("src/{file}")))
        })
        .collect()
}

/// No row at `file:line`: the call there sends what an inner binding holds.
fn assert_no_row(file: &str, line: i64) {
    let rows = rows_at(file, line);
    assert!(
        rows.is_empty(),
        "{file}:{line} reads a binding declared again in an inner scope; \
         no row may state the outer one: {rows:#?}"
    );
}

/// The one row at `file:line`, stated by `source`, sending `method` to
/// `target_url`.
fn assert_row(file: &str, line: i64, source: &str, method: &str, target_url: &str) {
    let rows = rows_at(file, line);
    assert_eq!(
        rows.len(),
        1,
        "expected one row at {file}:{line}: {rows:#?}"
    );
    let row = rows[0];
    assert_eq!(row["resolution_source"], source, "{file}:{line}: {row:#}");
    assert_eq!(row["method"], method, "{file}:{line}: {row:#}");
    assert_eq!(row["target_url"], target_url, "{file}:{line}: {row:#}");
}

/// The issue's reproduction: a block-local `const` shadows a module constant.
#[test]
fn a_summary_never_reads_a_module_constant_a_block_declares_again() {
    assert_no_row("module-const.ts", 6);
    assert_row(
        "module-const.ts",
        12,
        "request_summary",
        "PUT",
        "/api/users",
    );
}

#[test]
fn a_summary_never_reads_a_functions_local_a_block_declares_again() {
    assert_no_row("function-local.ts", 5);
    assert_row(
        "function-local.ts",
        7,
        "request_summary",
        "GET",
        "/api/items",
    );
}

/// A `var` written again inside a block is the same binding, holding
/// whichever declaration ran last.
#[test]
fn a_summary_never_reads_a_var_declared_twice() {
    assert_no_row("var-redeclared.ts", 6);
}

/// A block-local `const` that shadows a parameter: the caller's argument is
/// not what the call sends.
#[test]
fn a_summary_never_fills_a_parameter_a_block_declares_again() {
    assert_no_row("parameter-caller.ts", 4);
    assert_row(
        "parameter-caller.ts",
        5,
        "request_summary",
        "PATCH",
        "/api/param-archive",
    );
}

#[test]
fn a_callback_never_reads_a_captured_local_it_declares_again() {
    assert_no_row("closure.ts", 7);
    assert_row("closure.ts", 9, "request_summary", "GET", "/api/orders");
}

/// A block's own function named `fetch` is not the platform's.
#[test]
fn a_block_local_fetch_is_not_the_platform_fetch() {
    assert_no_row("own-fetch.ts", 6);
    assert_row("own-fetch.ts", 12, "request_summary", "GET", "/api/reports");
}

#[test]
fn a_new_url_binding_is_read_in_its_own_scope() {
    assert_no_row("new-url.ts", 6);
    assert_row("new-url.ts", 12, "new_url", "GET", "/api/new-url-module");
    // The inner block's `link` is declared after the outer one, and before
    // the fix it overwrote it in the function's frame.
    assert_row("new-url-frame.ts", 7, "new_url", "POST", "/api/frame-outer");
}

/// A member another module calls is read through the same bindings.
#[test]
fn a_member_shadowing_its_modules_url_states_nothing_at_its_callers() {
    assert_no_row("member.ts", 6);
    assert_no_row("member-caller.ts", 4);
    assert_row("member.ts", 12, "new_url", "GET", "/api/member-module");
    assert_row(
        "member-caller.ts",
        5,
        "request_summary",
        "GET",
        "/api/member-module",
    );
}

#[test]
fn an_env_base_is_read_only_through_the_binding_the_alias_table_describes() {
    assert_no_row("env-base.ts", 6);
    assert_row(
        "env-base-control.ts",
        4,
        "env_base_path",
        "GET",
        "${process.env.ACCOUNTS_URL}/accounts",
    );
}

#[test]
fn a_whole_url_binding_is_read_only_where_the_alias_table_describes_it() {
    assert_no_row("whole-url.ts", 6);
    assert_row(
        "whole-url-control.ts",
        4,
        "whole_url_env",
        "POST",
        "${process.env.TICKETS_URL}/api/tickets",
    );
}

#[test]
fn a_literal_base_is_read_only_through_the_binding_it_was_declared_on() {
    assert_no_row("literal-base.ts", 6);
    assert_row(
        "literal-base-control.ts",
        4,
        "literal_base_path",
        "GET",
        "http://status.example.com/health",
    );
}
