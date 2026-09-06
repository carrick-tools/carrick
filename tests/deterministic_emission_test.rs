//! Deterministic-primary emission: the rows the scanner states for itself do
//! not depend on the model answering (carrick#623, #632, #641, #655 were all
//! one model silence away from being lost).
//!
//! Two cassettes hold the model constant at the two answers that used to lose a
//! row. The EMPTY cassette answers nothing for every file, so only what the
//! deterministic layer emits survives. The CONTRADICTING cassette answers with
//! a wrong verb and an invented path at every deterministic span, so what
//! survives is what the join is willing to overrule.
//!
//! What the empty cassette is expected NOT to produce, per fixture, because
//! those rows are the model's own reading and no deterministic source states
//! them:
//!
//! - `env-var-whole-url`: `helpdesk.ts:20` (a base-plus-path target — the
//!   whole-URL rule fires only where the call states no path of its own), and
//!   every `ledger.ts` row except line 31 (an injected base, a bare relative
//!   path, a schema-declared variable with no `??` path).
//! - `new-url-target`: `catalogue.ts:13`, the inline decoy, which states its
//!   own target and reaches no URL constructor.
//! - `imported-request-member`: `legacy.ts`'s local call, which resolves
//!   through no imported member.
//! - `literal-base-url`: every row. A base declared as a module-level string
//!   literal is resolved by REWRITING the model's target (carrick#627); it is
//!   not an emitting source in this phase, and becomes one when the structured
//!   URL resolver lands.
//! - `flat-routes-method-guard`: every row. The fixture carries no manifest, so
//!   no routing convention is bootstrapped for an end-to-end scan of it and the
//!   file-based pass is a no-op; its routes are pinned by
//!   `file_based_routing_test` at the pass itself. `remix-flat` DOES carry a
//!   manifest, so a scan of it bootstraps the convention and its route rows are
//!   emitted end-to-end — which is what the claimed-module test at the bottom
//!   of this file needs.
//! - `e2e-scaffolding`: every row. Its one endpoint comes from the generated
//!   mock reading the prompt, not from a deterministic source.
//!
//! The contradicting cassette is exercised on the three fixtures whose
//! deterministic rows sit at a candidate span the model can answer at. It is
//! inert for `class-controller-api`, `flat-routes-method-guard` and
//! `e2e-scaffolding` (their rows have no call-site candidate to answer at) and
//! for `literal-base-url` (it has no deterministic row to contradict).
//!
//! `file-route-model-twin` is the third arrangement (carrick#660): the model
//! answers for a route the file layout already states, so what is pinned is
//! not the method or the target but WHO the row says stated it.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Every source file the scanner will be asked about, so the empty cassette
/// covers all of them: an uncovered file falls back to the generated mock,
/// which invents rows from the prompt and would read as deterministic output.
fn source_stems(dir: &Path) -> Vec<String> {
    let mut stems = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "node_modules") {
                    continue;
                }
                stack.push(path);
                continue;
            }
            let is_source = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| matches!(ext, "ts" | "tsx" | "js" | "jsx" | "mts" | "cts"));
            if !is_source {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                stems.push(stem.to_string());
            }
        }
    }
    stems.sort();
    stems.dedup();
    stems
}

fn fixture_dir(fixture: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture)
}

/// A cassette directory in which every file of `fixture` is answered by
/// `body`, keyed the way the mock keys them (by file stem).
fn cassette(fixture: &str, body: &dyn Fn(&str) -> String) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let analyze = dir.path().join("analyze-file");
    std::fs::create_dir_all(&analyze).expect("create analyze-file dir");
    for stem in source_stems(&fixture_dir(fixture)) {
        std::fs::write(analyze.join(format!("{stem}.json")), body(&stem)).expect("write cassette");
    }
    dir
}

const NOTHING: &str = r#"{"mounts":[],"endpoints":[],"data_calls":[]}"#;

/// A data call the model did not make: a verb no site in these fixtures uses
/// and a path no source contains, at a line a deterministic source states.
fn contradiction(line: i64, extra: &str) -> String {
    format!(
        r#"{{"candidate_id":"@line:{line}","line_number":{line},"target":"/invented/by/the/model",
            "method":"OPTIONS","pattern_matched":"fetch","call_expression_text":null,
            "call_expression_line":{line},"payload_expression_text":null,
            "payload_expression_line":null{extra}}}"#
    )
}

/// The same row with the client name the model reports changed. The client is
/// a field determinism does not state, so it is the one to watch surviving the
/// join intact.
fn contradiction_naming(line: i64, client: &str) -> String {
    contradiction(line, "").replace(
        r#""pattern_matched":"fetch""#,
        &format!(r#""pattern_matched":"{client}""#),
    )
}

fn contradicting_file(lines: &[(i64, &str)]) -> String {
    let calls: Vec<String> = lines
        .iter()
        .map(|(line, extra)| contradiction(*line, extra))
        .collect();
    format!(
        r#"{{"mounts":[],"endpoints":[],"data_calls":[{}]}}"#,
        calls.join(",")
    )
}

fn scan(fixture: &str, mock_dir: &Path) -> serde_json::Value {
    let dir = fixture_dir(fixture);
    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(&dir)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", mock_dir.display()),
        )
        .env("CARRICK_OUTPUT_JSON", "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env_remove("GITHUB_REPOSITORY")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("CI")
        .output()
        .expect("failed to spawn carrick binary");
    assert!(
        output.status.success(),
        "scanner exited non-zero on {fixture}:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("scanner stdout was not UTF-8");
    serde_json::from_str(&stdout).expect("scanner output was not valid JSON")
}

fn empty_scan(fixture: &str) -> serde_json::Value {
    let dir = cassette(fixture, &|_| NOTHING.to_string());
    scan(fixture, dir.path())
}

fn rows(projection: &serde_json::Value, key: &str) -> Vec<serde_json::Value> {
    projection[key]
        .as_array()
        .unwrap_or_else(|| panic!("projection carries a {key} array"))
        .clone()
}

fn row_at(rows: &[serde_json::Value], file: &str, line: i64) -> serde_json::Value {
    let found: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|row| row["file"] == file && row["line"].as_i64() == Some(line))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "expected exactly one row at {file}:{line}, got {found:#?} out of {rows:#?}"
    );
    found[0].clone()
}

fn assert_none_at(rows: &[serde_json::Value], file: &str, line: i64) {
    assert!(
        !rows
            .iter()
            .any(|row| row["file"] == file && row["line"].as_i64() == Some(line)),
        "expected no row at {file}:{line}: {rows:#?}"
    );
}

// ---------------------------------------------------------------------------
// The empty cassette: the model answers nothing, anywhere.
// ---------------------------------------------------------------------------

/// carrick#572 / #632: a request whose whole URL is read from an environment
/// variable is stated by the binding's own AST, so it survives the model
/// saying nothing about the file.
#[test]
fn a_silent_model_keeps_every_whole_url_env_call() {
    let calls = rows(&empty_scan("env-var-whole-url"), "calls");

    let answer = row_at(&calls, "src/helpdesk.ts", 7);
    assert_eq!(answer["method"], "POST");
    assert_eq!(
        answer["target_url"],
        "${process.env.HELPDESK_URL}/api/answer"
    );
    assert_eq!(answer["path"], "/api/answer");

    let ask = row_at(&calls, "src/toolset.ts", 12);
    assert_eq!(ask["method"], "POST");
    assert_eq!(ask["target_url"], "${process.env.SERVICE_ASK_URL}/api/ask");
    assert_eq!(ask["path"], "/api/ask");

    let answered = row_at(&calls, "src/answered.ts", 11);
    assert_eq!(answered["method"], "POST");
    assert_eq!(
        answered["target_url"],
        "${process.env.SUPPORT_ASK_URL}/api/ask"
    );
    assert_eq!(answered["path"], "/api/ask");

    let quote = row_at(&calls, "src/ledger.ts", 31);
    assert_eq!(quote["method"], "POST");
    assert_eq!(quote["target_url"], "${process.env.GATEWAY_URL}/v1/quote");

    // The model's own readings, absent as documented at the top of the file.
    assert_none_at(&calls, "src/helpdesk.ts", 20);
    assert_none_at(&calls, "src/ledger.ts", 12);
    assert_none_at(&calls, "src/ledger.ts", 22);
}

/// carrick#610: the path a `new URL(path, base)` states is an AST fact at the
/// call site, so it is a row of its own rather than a rewrite waiting for one.
#[test]
fn a_silent_model_keeps_every_new_url_call() {
    let calls = rows(&empty_scan("new-url-target"), "calls");

    let list = row_at(&calls, "src/catalogue.ts", 18);
    assert_eq!(list["method"], "GET");
    assert_eq!(list["target_url"], "/api/v2/things");
    assert_eq!(list["key"], "http|GET|/api/v2/things");

    let find = row_at(&calls, "src/catalogue.ts", 27);
    assert_eq!(find["method"], "GET");
    assert_eq!(find["target_url"], "/api/v2/things/search");

    let archive = row_at(&calls, "src/catalogue.ts", 33);
    assert_eq!(archive["method"], "POST");
    assert_eq!(archive["target_url"], "/api/v2/things/${id}/archive");

    assert_none_at(&calls, "src/catalogue.ts", 13);
}

/// carrick#588 / #623 / #655: the method and URL an imported client member
/// states are literals in its own source, and the site's span is an AST fact.
#[test]
fn a_silent_model_keeps_every_imported_member_call() {
    let calls = rows(&empty_scan("imported-request-member"), "calls");

    for (file, line, method, path) in [
        ("src/artifacts.ts", 5, "PUT", "/api/v2/artifacts/:encoded"),
        ("src/artifacts.ts", 16, "GET", "/api/v1/artifacts/:encoded"),
        ("src/artifacts.ts", 25, "GET", "/api/v2/session"),
        ("src/envcmd.ts", 8, "GET", "/api/v1/artifacts/:encoded"),
        ("src/supervisor.ts", 12, "GET", "/api/v2/session"),
        ("src/uploads.ts", 7, "PUT", "/api/v2/artifacts/:encoded"),
        ("src/uploads.ts", 11, "GET", "/api/v1/artifacts/:encoded"),
        ("src/uploads.ts", 15, "GET", "/api/v2/session"),
        ("src/uploads.ts", 19, "GET", "/api/v1/artifacts"),
    ] {
        let row = row_at(&calls, file, line);
        assert_eq!(row["method"], method, "method at {file}:{line}");
        assert_eq!(row["path"], path, "path at {file}:{line}");
    }
}

/// #580: a route table that binds a literal path to an imported controller
/// states the whole route, across two files, with no call-site candidate in
/// either of them.
#[test]
fn a_silent_model_keeps_every_class_controller_route() {
    let endpoints = rows(&empty_scan("class-controller-api"), "endpoints");

    for (file, line, method, path) in [
        ("src/controllers/health.ts", 5, "GET", "/health"),
        ("src/controllers/profile.ts", 4, "GET", "/profile"),
        ("src/controllers/profile.ts", 8, "PATCH", "/profile"),
        ("src/controllers/report.ts", 7, "GET", "/report"),
        ("src/controllers/root.ts", 4, "GET", "/"),
        ("src/controllers/session.ts", 4, "GET", "/session"),
        ("src/controllers/session.ts", 8, "DELETE", "/session"),
        ("src/controllers/token.ts", 4, "POST", "/token"),
        ("src/controllers/widget-item.ts", 4, "GET", "/widget/:id"),
        ("src/controllers/widget-item.ts", 8, "PUT", "/widget/:id"),
        (
            "src/controllers/widget-item.ts",
            12,
            "DELETE",
            "/widget/:id",
        ),
        ("src/controllers/widget.ts", 4, "GET", "/widget"),
        ("src/controllers/widget.ts", 8, "POST", "/widget"),
    ] {
        let row = row_at(&endpoints, file, line);
        assert_eq!(row["method"], method, "method at {file}:{line}");
        assert_eq!(row["path"], path, "path at {file}:{line}");
    }
    assert_eq!(endpoints.len(), 13, "no other route: {endpoints:#?}");
}

/// The three fixtures whose rows are all the model's own reading. Pinned so a
/// change that starts emitting for them is a deliberate one, and so the list
/// at the top of this file stays honest.
#[test]
fn a_silent_model_leaves_the_model_only_fixtures_empty() {
    for fixture in [
        "literal-base-url",
        "flat-routes-method-guard",
        "e2e-scaffolding",
    ] {
        let projection = empty_scan(fixture);
        assert!(
            rows(&projection, "calls").is_empty(),
            "{fixture} emitted calls with no model answer: {projection:#?}"
        );
        assert!(
            rows(&projection, "endpoints").is_empty(),
            "{fixture} emitted endpoints with no model answer: {projection:#?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The contradicting cassette: the model answers with a wrong verb and an
// invented path at every deterministic span.
// ---------------------------------------------------------------------------

/// The deterministic method and target win over the model's, and the fields
/// determinism does not state still come from the model.
#[test]
fn a_contradicting_model_loses_the_method_and_the_target() {
    let dir = cassette("env-var-whole-url", &|stem| match stem {
        "helpdesk" => contradicting_file(&[(7, "")]),
        "toolset" => contradicting_file(&[(12, "")]),
        "answered" => contradicting_file(&[(11, "")]),
        // The enrichment probe: the client name is the model's to state, on a
        // row whose method and target are both wrong.
        "ledger" => format!(
            r#"{{"mounts":[],"endpoints":[],"data_calls":[{}]}}"#,
            contradiction_naming(31, "gatewayClient")
        ),
        _ => NOTHING.to_string(),
    });
    let calls = rows(&scan("env-var-whole-url", dir.path()), "calls");

    let answer = row_at(&calls, "src/helpdesk.ts", 7);
    assert_eq!(answer["method"], "POST", "the options bag states the verb");
    assert_eq!(
        answer["target_url"],
        "${process.env.HELPDESK_URL}/api/answer"
    );

    let ask = row_at(&calls, "src/toolset.ts", 12);
    assert_eq!(ask["method"], "POST");
    assert_eq!(ask["target_url"], "${process.env.SERVICE_ASK_URL}/api/ask");

    let answered = row_at(&calls, "src/answered.ts", 11);
    assert_eq!(answered["method"], "POST");
    assert_eq!(
        answered["target_url"],
        "${process.env.SUPPORT_ASK_URL}/api/ask"
    );

    let quote = row_at(&calls, "src/ledger.ts", 31);
    assert_eq!(quote["method"], "POST");
    assert_eq!(quote["target_url"], "${process.env.GATEWAY_URL}/v1/quote");
    assert_eq!(
        quote["handler"], "gatewayClient",
        "the model still contributes what determinism does not state"
    );

    for call in &calls {
        let target = call["target_url"].as_str().unwrap_or_default();
        assert!(
            !target.contains("/invented/"),
            "an invented path reached the index: {call:#?}"
        );
        assert_ne!(call["method"], "OPTIONS", "an invented verb: {call:#?}");
    }
}

/// The same, where the deterministic statement is the path a URL constructor
/// gives and the verb the call's own options bag gives.
#[test]
fn a_contradicting_model_loses_to_the_url_constructor() {
    let dir = cassette("new-url-target", &|stem| match stem {
        "catalogue" => contradicting_file(&[(18, ""), (27, ""), (33, "")]),
        _ => NOTHING.to_string(),
    });
    let calls = rows(&scan("new-url-target", dir.path()), "calls");

    let list = row_at(&calls, "src/catalogue.ts", 18);
    assert_eq!(list["method"], "GET");
    assert_eq!(list["target_url"], "/api/v2/things");

    let find = row_at(&calls, "src/catalogue.ts", 27);
    assert_eq!(find["method"], "GET");
    assert_eq!(find["target_url"], "/api/v2/things/search");

    let archive = row_at(&calls, "src/catalogue.ts", 33);
    assert_eq!(archive["method"], "POST");
    assert_eq!(archive["target_url"], "/api/v2/things/${id}/archive");
}

/// The same, where the deterministic statement is the member the site calls.
#[test]
fn a_contradicting_model_loses_to_the_imported_member() {
    let dir = cassette("imported-request-member", &|stem| match stem {
        "artifacts" => contradicting_file(&[(5, ""), (16, "")]),
        "uploads" => contradicting_file(&[(7, ""), (11, ""), (19, "")]),
        "envcmd" => contradicting_file(&[(8, "")]),
        "supervisor" => contradicting_file(&[(12, "")]),
        _ => NOTHING.to_string(),
    });
    let calls = rows(&scan("imported-request-member", dir.path()), "calls");

    for (file, line, method, path) in [
        ("src/artifacts.ts", 5, "PUT", "/api/v2/artifacts/:encoded"),
        ("src/artifacts.ts", 16, "GET", "/api/v1/artifacts/:encoded"),
        ("src/envcmd.ts", 8, "GET", "/api/v1/artifacts/:encoded"),
        ("src/supervisor.ts", 12, "GET", "/api/v2/session"),
        ("src/uploads.ts", 7, "PUT", "/api/v2/artifacts/:encoded"),
        ("src/uploads.ts", 11, "GET", "/api/v1/artifacts/:encoded"),
        ("src/uploads.ts", 19, "GET", "/api/v1/artifacts"),
    ] {
        let row = row_at(&calls, file, line);
        assert_eq!(row["method"], method, "method at {file}:{line}");
        assert_eq!(row["path"], path, "path at {file}:{line}");
    }
}

// ---------------------------------------------------------------------------
// The twin cassette: the model answers for a route the file layout states.
// ---------------------------------------------------------------------------

/// The model's answer for a file-declared route, anchored on the only
/// candidate that file offers.
const ROUTE_TWIN: &str = r#"{"mounts":[],"data_calls":[],"endpoints":[{
    "candidate_id":"@line:13","line_number":13,"owner_node":"app","method":"GET",
    "path":"/orders","handler_name":"listOrders","pattern_matched":"route handler",
    "payload_expression_text":null,"payload_expression_line":null,
    "response_expression_text":"order","response_expression_line":15,
    "primary_type_symbol":"Order","type_import_source":null}]}"#;

fn only_row(rows: &[serde_json::Value]) -> serde_json::Value {
    assert_eq!(rows.len(), 1, "expected exactly one row: {rows:#?}");
    rows[0].clone()
}

/// carrick#660: a route stated by the file layout keeps the convention as its
/// source when the model describes it too. The model contributes the handler
/// and the type anchors — the fields determinism does not state — and does not
/// relabel the row as its own reading.
///
/// The label is the whole point: a reader that cannot tell a route the layout
/// declares from one only the model reported has to treat both the same way,
/// and the row that is a structural fact is the one that should not need
/// corroborating.
#[test]
fn a_model_answer_at_a_file_route_keeps_the_convention_as_the_source() {
    // The convention alone: the file layout gives the path, the exported
    // handler name gives the verb.
    let silent = only_row(&rows(&empty_scan("file-route-model-twin"), "endpoints"));
    assert_eq!(silent["method"], "GET");
    assert_eq!(silent["path"], "/orders");
    assert_eq!(silent["handler"], "GET");
    assert_eq!(
        silent["resolution_source"], "file_based_route",
        "the convention states this row"
    );

    // The same route, with the model answering at the file's one candidate.
    let dir = cassette("file-route-model-twin", &|stem| match stem {
        "route" => ROUTE_TWIN.to_string(),
        _ => NOTHING.to_string(),
    });
    let joined = only_row(&rows(
        &scan("file-route-model-twin", dir.path()),
        "endpoints",
    ));
    assert_eq!(joined["method"], "GET");
    assert_eq!(joined["path"], "/orders");
    assert_eq!(
        joined["resolution_source"], "file_based_route",
        "a joined row keeps the source of the deterministic twin it folded into"
    );
    assert_eq!(
        joined["handler"], "listOrders",
        "the model still contributes what determinism does not state"
    );
    assert_eq!(
        joined["primary_type_symbol"], "Order",
        "and the type anchor it read"
    );
}

/// carrick#703 / #704: in a file a routing convention CLAIMED, the route set is
/// the module's exported handlers and nothing else, so an endpoint the model
/// states there and the convention does not is discarded.
///
/// The fixture's decoy module dispatches its write handler on a form
/// discriminator (`z.literal("update-build-settings")`), which appears in its
/// bytes exactly the way a route path would. The cassette answers at the
/// module's one real candidate — its outbound `fetch` — with a route nothing
/// registers, and with the call that IS there. The call surviving is the
/// control: without it, "no invented endpoint" would also be what a file that
/// never reached the analyzer looks like.
#[test]
fn a_model_route_invented_in_a_claimed_module_is_discarded() {
    const DECOY_CALL_LINE: i64 = 19;
    let dir = cassette("remix-flat", &|stem| match stem {
        "settings.builds" => format!(
            r#"{{"mounts":[],"endpoints":[{{"candidate_id":"@line:{line}",
                "line_number":{line},"owner_node":"app","method":"POST",
                "path":"/api/build-settings","handler_name":"action",
                "pattern_matched":"router.post","payload_expression_text":null,
                "payload_expression_line":null,"response_expression_text":null,
                "response_expression_line":null,"primary_type_symbol":null,
                "type_import_source":null}}],
                "data_calls":[{}]}}"#,
            contradiction(DECOY_CALL_LINE, "").replace("/invented/by/the/model", "/internal/audit"),
            line = DECOY_CALL_LINE,
        ),
        _ => NOTHING.to_string(),
    });
    let projection = scan("remix-flat", dir.path());
    let endpoints = rows(&projection, "endpoints");
    let calls = rows(&projection, "calls");

    assert!(
        !endpoints
            .iter()
            .any(|row| row["path"] == "/api/build-settings"),
        "a schema literal is not a route: {endpoints:#?}"
    );
    assert_none_at(
        &endpoints,
        "app/routes/settings.builds.tsx",
        DECOY_CALL_LINE,
    );

    // The convention's own rows for that module survive.
    for (method, path) in [("GET", "/settings/builds"), ("POST", "/settings/builds")] {
        assert!(
            endpoints.iter().any(|row| row["method"] == method
                && row["path"] == path
                && row["file"] == "app/routes/settings.builds.tsx"),
            "the module's derived {method} {path} must survive: {endpoints:#?}"
        );
    }

    // The control: the cassette WAS applied to this file.
    let call = row_at(&calls, "app/routes/settings.builds.tsx", DECOY_CALL_LINE);
    assert_eq!(call["path"], "/internal/audit");
}
