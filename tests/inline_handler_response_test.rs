//! A route whose handler is passed INLINE serves the payload its send states
//! (carrick#1253).
//!
//! The shape is the common one: a registration call takes an anonymous arrow
//! function, and the payload is the ARGUMENT of a send on the context
//! (`ctx.json(widgets)`), never the handler's return value — which is the
//! transport the framework hands back. There is no named handler whose return
//! type states the contract, so everything the route says about its response
//! has to come out of the handler's body.
//!
//! What the arms hold constant is the source; what varies is the locator the
//! model reported for that body, because in production that field is the one
//! that moves:
//!
//! - the expression the source itself states, at its own line;
//! - nothing at all, which drops the request onto the registration call's span;
//! - an expression rendered on one line where the source spreads it over four,
//!   at a line that is not the send's;
//! - the send call rather than its argument;
//! - a `return-value` classification for a payload that is in fact the
//!   argument of a send.
//!
//! Every arm must reach the same payload. The fixture's two runtimes differ in
//! what the send RETURNS — the bare platform `Response` in `runtime.ts`, a
//! wrapper carrying the payload as a type argument in the installed
//! `@fixture-http/runtime` — and both are in the wild. The second is vendored
//! as a dependency rather than declared in `src/`, because a type's
//! declaration ORIGIN is part of what tells transport apart from a payload,
//! and a framework declared in the scanned repo's own source is not arranged
//! the way production is.
//!
//! Deleting the registration-span fallback in `collect_type_requests` fails
//! the three no-locator tests here and nothing else. Deleting the
//! reported-text locator fails one test, and only one: the send that the
//! handler does not RETURN, which is the shape the fallback cannot reach from
//! the registration. Every other arm is served by either locator, which is
//! what the arms are for — the reported field moves and the answer must not.

use std::path::PathBuf;
use std::process::Command;

const FIXTURE: &str = "inline-handler-response";

/// Files of the fixture no arm answers for: an uncovered file falls back to
/// the generated mock, which invents rows from the prompt.
const SILENT_FILES: [&str; 2] = ["runtime", "widgets"];

const NOTHING: &str = r#"{"mounts":[],"endpoints":[],"data_calls":[]}"#;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(FIXTURE)
}

/// One endpoint row of a model answer. `response` is the locator the model
/// reported for the payload: its text and the line it sits on, or nothing.
fn row(
    line: i64,
    method: &str,
    path: &str,
    emission_style: &str,
    response: Option<(&str, i64)>,
) -> String {
    let (text, response_line) = match response {
        Some((text, line)) => (
            format!("\"{}\"", text.replace('"', "\\\"")),
            line.to_string(),
        ),
        None => ("null".to_string(), "null".to_string()),
    };
    format!(
        r#"{{"candidate_id":"@line:{line}","line_number":{line},"owner_node":"router",
            "method":"{method}","path":"{path}","handler_name":"anonymous",
            "pattern_matched":"route registration","emission_style":"{emission_style}",
            "payload_expression_text":null,"payload_expression_line":null,
            "response_expression_text":{text},"response_expression_line":{response_line}}}"#
    )
}

fn answer(rows: &[String]) -> String {
    format!(
        r#"{{"mounts":[],"endpoints":[{}],"data_calls":[]}}"#,
        rows.join(",")
    )
}

/// Scan the fixture with `routes` answered for `src/routes.ts` and `typed` for
/// `src/typed-routes.ts`, and return the run's JSON projection.
fn scan(routes: &str, typed: &str) -> serde_json::Value {
    let dir = tempfile::tempdir().expect("tempdir");
    let analyze = dir.path().join("analyze-file");
    std::fs::create_dir_all(&analyze).expect("create analyze-file dir");
    std::fs::write(analyze.join("routes.json"), routes).expect("write routes cassette");
    std::fs::write(analyze.join("typed-routes.json"), typed).expect("write typed cassette");
    for stem in SILENT_FILES {
        std::fs::write(analyze.join(format!("{stem}.json")), NOTHING).expect("write cassette");
    }

    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(fixture_dir())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", dir.path().display()),
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
        "scanner exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("scanner stdout was not UTF-8");
    serde_json::from_str(&stdout).expect("scanner output was not valid JSON")
}

/// The response type the run states for the route at `file:line`, expanded.
///
/// A missing type reads as `<none>` rather than panicking: on a machine where
/// the sidecar was never built EVERY route reads that way, and the message
/// below should say which shape was expected rather than which key was absent.
fn response_at(projection: &serde_json::Value, file: &str, line: i64) -> String {
    let endpoints = projection["endpoints"]
        .as_array()
        .expect("the projection carries an endpoints array");
    let found: Vec<&serde_json::Value> = endpoints
        .iter()
        .filter(|row| row["file"] == file && row["line"].as_i64() == Some(line))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "expected exactly one row at {file}:{line}, got {found:#?} out of {endpoints:#?}"
    );
    found[0]["expanded_definition"]
        .as_str()
        .or_else(|| found[0]["resolved_definition"].as_str())
        .unwrap_or("<none>")
        .to_string()
}

const WIDGET: &str = "{ id: string; name: string; parts: number; }";

/// Every route of `routes.ts`, and the payload each one's send states.
fn assert_bare_routes(projection: &serde_json::Value) {
    let file = "src/routes.ts";
    assert_eq!(response_at(projection, file, 10), format!("{WIDGET}[]"));
    assert_eq!(
        response_at(projection, file, 15),
        format!("{{ widget: {WIDGET}; }}")
    );
    assert_eq!(
        response_at(projection, file, 20),
        "{ total: number; names: string[]; }"
    );
    assert_eq!(response_at(projection, file, 24), WIDGET);
}

/// The model reports the expression the source itself states, at its line.
#[test]
fn an_inline_handler_serves_the_payload_its_send_states() {
    let routes = answer(&[
        row(
            10,
            "GET",
            "/widgets",
            "imperative-send",
            Some(("widgets", 12)),
        ),
        row(
            15,
            "GET",
            "/widgets/:id",
            "imperative-send",
            Some(("{ widget }", 17)),
        ),
        row(
            20,
            "GET",
            "/widgets/summary",
            "imperative-send",
            Some(("await summarize()", 21)),
        ),
        row(
            24,
            "POST",
            "/widgets",
            "imperative-send",
            Some(("created", 28)),
        ),
    ]);
    assert_bare_routes(&scan(&routes, NOTHING));
}

/// carrick#1253: the model reports the route and no locator for its payload.
///
/// The request then carries the REGISTRATION call's span, and the payload is
/// two indirections in: the handler that call takes, and the argument of the
/// send inside it. Nothing about the route is reported that a reader could
/// have used instead, so this arm is the one that says the body is reachable
/// from the registration alone.
#[test]
fn a_route_with_no_reported_locator_still_serves_its_payload() {
    let routes = answer(&[
        row(10, "GET", "/widgets", "imperative-send", None),
        row(15, "GET", "/widgets/:id", "imperative-send", None),
        row(20, "GET", "/widgets/summary", "imperative-send", None),
        row(24, "POST", "/widgets", "imperative-send", None),
    ]);
    assert_bare_routes(&scan(&routes, NOTHING));
}

/// A gate sits in front of the handler and sends a body of its own on refusal.
/// The route's response is the handler's payload, not the gate's rejection —
/// and with no locator reported, the handler is the LAST function the
/// registration takes, not the first.
#[test]
fn a_gate_in_front_of_the_handler_does_not_state_the_response() {
    let routes = answer(&[row(
        51,
        "GET",
        "/widgets/:id/audit",
        "imperative-send",
        None,
    )]);
    let projection = scan(&routes, NOTHING);
    assert_eq!(
        response_at(&projection, "src/routes.ts", 51),
        "{ auditedId: string; }",
        "the gate's 401 body is not this route's response"
    );
}

/// The reported text is not the source's own: the payload object is spread
/// over four lines in the file and reported on one, at the line the send
/// opens on rather than the line the object does. A locator that cannot be
/// found verbatim must not cost the route its type.
#[test]
fn a_locator_that_is_not_the_sources_own_text_still_serves_the_payload() {
    let routes = answer(&[
        row(
            34,
            "GET",
            "/widgets/report",
            "imperative-send",
            Some(("{ widgets, generatedAt: new Date().toISOString() }", 36)),
        ),
        // Reported at the registration line, three lines above the send.
        row(
            42,
            "GET",
            "/widgets/count",
            "imperative-send",
            Some(("{ count }", 42)),
        ),
    ]);
    let projection = scan(&routes, NOTHING);
    assert_eq!(
        response_at(&projection, "src/routes.ts", 34),
        format!("{{ widgets: {WIDGET}[]; generatedAt: string; }}")
    );
    assert_eq!(
        response_at(&projection, "src/routes.ts", 42),
        "{ count: number; }"
    );
}

/// Two classifications the payload survives: the locator naming the SEND
/// rather than its argument, and a `return-value` claim for a handler whose
/// payload is in fact the argument of a send. Both publish the payload, never
/// the transport the send evaluates to.
#[test]
fn a_misreported_emission_style_does_not_publish_the_transport() {
    let routes = answer(&[
        row(
            10,
            "GET",
            "/widgets",
            "return-value",
            Some(("ctx.json(widgets)", 12)),
        ),
        row(15, "GET", "/widgets/:id", "return-value", None),
    ]);
    let projection = scan(&routes, NOTHING);
    assert_eq!(
        response_at(&projection, "src/routes.ts", 10),
        format!("{WIDGET}[]")
    );
    assert_eq!(
        response_at(&projection, "src/routes.ts", 15),
        format!("{{ widget: {WIDGET}; }}")
    );
}

/// The send is a statement and the handler returns nothing. Following the
/// handler's return from the registration reaches `void`, so the only thing
/// that states this route's response is the locator the model reported at the
/// send — this is the arm the reported text is for.
#[test]
fn a_send_the_handler_does_not_return_is_reached_by_its_locator() {
    let routes = answer(&[row(
        59,
        "GET",
        "/widgets/legacy",
        "imperative-send",
        Some(("widgets", 61)),
    )]);
    let projection = scan(&routes, NOTHING);
    assert_eq!(
        response_at(&projection, "src/routes.ts", 59),
        format!("{WIDGET}[]")
    );
}

/// The same inline shape where the send returns a wrapper that CARRIES the
/// payload's type rather than the bare platform response — the arrangement of
/// every framework whose handler returns what the send produced.
///
/// The payload is still the argument, and both locators reach it: the one the
/// model reported, and the registration span when it reported none. What is
/// published is the payload, never the wrapper it travelled in.
#[test]
fn a_payload_carrying_wrapper_serves_the_payload_and_not_the_wrapper() {
    let typed = answer(&[
        row(
            9,
            "GET",
            "/typed/widgets",
            "imperative-send",
            Some(("widgets", 11)),
        ),
        row(14, "GET", "/typed/widgets/summary", "imperative-send", None),
    ]);
    let projection = scan(NOTHING, &typed);
    let file = "src/typed-routes.ts";
    assert_eq!(response_at(&projection, file, 9), format!("{WIDGET}[]"));
    assert_eq!(
        response_at(&projection, file, 14),
        "{ total: number; names: string[]; }"
    );
}
