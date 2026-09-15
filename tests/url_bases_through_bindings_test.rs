//! carrick#1150, #1152, #1153: a call's base read through a binding resolves
//! to what the binding states, instead of staying a raw `${…}` interpolation
//! that matches nothing or dropping the call outright.
//!
//! Deterministic end to end. The LLM is replayed from `__llm__/`, and each
//! cassette holds the target verbatim, which is what extraction emits for a
//! base it cannot see the value of. What is under test is what the scanner
//! then does with it:
//!
//! - #1150: a module-level path literal (`${ACCOUNTS_PATH}/${id}`) and a query
//!   string appended by a ternary (`/admin/jobs${q ? `?${q}` : ""}`). Before,
//!   both were skipped: no literal path segment, and a non-route target.
//! - #1152: an imported config object whose property reads `import.meta.env`
//!   or `Deno.env.get` with a `||`/`??` default. Before, the base stayed
//!   `${config.X}` and the call never keyed on its route.
//! - #1153: an imported string constant holding a vendor origin. Before, the
//!   SCREAMING_SNAKE name read as an env-var base and a third-party request was
//!   indexed as an internal call with no producer.
//!
//! See `tests/fixtures/url-bases-through-bindings/README.md` for the shapes.

use std::process::Command;

fn calls() -> Vec<serde_json::Value> {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/url-bases-through-bindings");
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
}

fn call_at(calls: &[serde_json::Value], file: &str, line: i64) -> serde_json::Value {
    calls
        .iter()
        .find(|call| call["file"] == file && call["line"].as_i64() == Some(line))
        .unwrap_or_else(|| panic!("no row at {file}:{line}: {calls:#?}"))
        .clone()
}

#[test]
fn every_call_site_is_one_row() {
    let calls = calls();
    assert_eq!(calls.len(), 6, "one row per call site: {calls:#?}");
}

/// carrick#1150: the prefix const is substituted, so the route is the path the
/// source writes.
#[test]
fn a_same_file_path_const_states_the_route() {
    let calls = calls();

    let one = call_at(&calls, "src/admin.ts", 4);
    assert_eq!(one["method"], "GET");
    assert_eq!(one["target_url"], "/admin/accounts/${accountId}");
    assert_eq!(one["path"], "/admin/accounts/:accountId");

    let page = call_at(&calls, "src/admin.ts", 9);
    assert_eq!(page["method"], "GET");
    assert_eq!(
        page["path"], "/admin/accounts",
        "the query string is not part of the route"
    );
}

/// carrick#1150: a query string sent only when there is one is still a
/// request to the bare route.
#[test]
fn a_conditional_query_tail_keeps_the_call() {
    let calls = calls();
    let jobs = call_at(&calls, "src/admin.ts", 14);
    assert_eq!(jobs["method"], "GET");
    assert_eq!(jobs["target_url"], "/admin/jobs?${query}");
    assert_eq!(jobs["path"], "/admin/jobs");
}

/// carrick#1152: the imported config object's property resolves to the env
/// var its initializer reads, and a declared internal one keys on the route.
#[test]
fn an_imported_config_object_resolves_to_its_env_var() {
    let calls = calls();

    let order = call_at(&calls, "src/orders.ts", 4);
    assert_eq!(order["method"], "GET");
    assert_eq!(
        order["base"]["env_var"], "VITE_ORDERS_API_URL",
        "the base is the build-time env var, not the config property: {order:#?}"
    );
    assert_eq!(
        order["path"], "/v1/orders/:orderId",
        "a declared internal env-var base reduces to the route path"
    );

    // The runtime-API spelling, undeclared: an expected find, so the base is
    // named and kept rather than stripped.
    let reports = call_at(&calls, "src/reports.ts", 4);
    assert_eq!(reports["method"], "GET");
    assert_eq!(reports["base"]["env_var"], "REPORTS_URL", "{reports:#?}");
    assert_eq!(
        reports["target_url"],
        "${process.env.REPORTS_URL}/v1/reports"
    );
}

/// carrick#1153: an imported constant holding an absolute origin makes the
/// call a third-party request, keyed with its host.
#[test]
fn an_imported_string_const_base_is_an_external_host() {
    let calls = calls();
    let charge = call_at(&calls, "src/vendor.ts", 4);
    assert_eq!(charge["method"], "POST");
    assert_eq!(
        charge["target_url"],
        "https://api.payments-vendor.example/v2/charges"
    );
    assert_eq!(
        charge["key"], "http|POST|https://api.payments-vendor.example/v2/charges",
        "a third-party origin stays on the match key"
    );
    assert!(
        charge["base"].is_null(),
        "a literal origin is not an env-var base: {charge:#?}"
    );
}
