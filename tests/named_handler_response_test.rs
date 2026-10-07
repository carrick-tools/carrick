//! A route whose handler is passed by name, and declared in another file,
//! serves the response that handler sends (carrick#1913).
//!
//! The shape: one module registers the routes
//! (`router.post('/widgets', requireKey, createWidget)`) and imports every
//! handler from somewhere else. The model reads the registration file. It
//! sees no handler body there, so it locates no response expression, and the
//! style it gives is a guess about a function it was not shown: `no-payload`
//! most often, sometimes one of the other two.
//!
//! What the arms hold constant is the source. What varies is that style,
//! because it is the field that moves in production and the answer must not
//! move with it: every handler below reads the same under all three.
//!
//! The handlers are the kinds a handler comes in:
//!
//! - it returns its payload;
//! - it returns the send it makes, behind a named gate that sends a refusal of
//!   its own;
//! - it sends through the response it was handed and returns nothing;
//! - it ends the response without a body;
//! - it sends on the path that succeeds and hands a failure on;
//! - it is bound to a call that wraps the function;
//! - it answers only with an error status;
//! - it is imported under another name, through a module that republishes
//!   it, as a default export, and through a path alias;
//! - it returns transport something else built from the request.
//!
//! Each row's response is the body the handler sends, or `unknown` with the
//! reason it has none. It is never `void` (what a handler that sends through
//! a parameter returns), never a function (the handler itself), never the path
//! (the registration's first argument) and never the router (the registration
//! call's own value): each of those is what a request located in the
//! registration file can read.
//!
//! Two rows are controls. A handler the registration's own file declares was
//! read by the model, so its row is asked what it always was. A router passed
//! by name is no handler, and its declaration is never offered as one.
//!
//! Reverting the branch in `collect_type_requests` that asks at the handler
//! fails the `no-payload` arm on every handler, and the other two on the
//! default export.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

const FIXTURE: &str = "named-handler-response";

/// The fixture's files no arm answers for: an uncovered file falls back to
/// the generated mock, which invents rows from the prompt.
const SILENT_FILES: [&str; 9] = [
    "barrel", "counting", "guarded", "handlers", "inner", "ping", "query", "renaming", "widgets",
];

const NOTHING: &str = r#"{"mounts":[],"endpoints":[],"data_calls":[]}"#;

const WIDGET: &str = "{ id: string; name: string; parts: number; }";

/// One registration of `src/routes.ts`: its line, its route and the name it
/// is handed.
struct Route {
    line: i64,
    method: &'static str,
    path: &'static str,
    handler: &'static str,
}

const fn route(
    line: i64,
    method: &'static str,
    path: &'static str,
    handler: &'static str,
) -> Route {
    Route {
        line,
        method,
        path,
        handler,
    }
}

/// The registrations whose handler another file declares.
const ELSEWHERE: [Route; 11] = [
    route(25, "GET", "/widgets", "listWidgets"),
    route(26, "GET", "/widgets/:id", "showWidget"),
    route(27, "POST", "/widgets", "createWidget"),
    route(28, "DELETE", "/widgets/:id", "removeWidget"),
    route(29, "GET", "/widgets/:id/audit", "auditWidget"),
    route(30, "POST", "/widgets/:id/name", "renameWidget"),
    route(31, "GET", "/widgets/retired", "retiredWidget"),
    route(32, "POST", "/widgets/:id/archive", "archive"),
    route(33, "GET", "/widgets/count", "countWidgets"),
    route(34, "GET", "/ping", "ping"),
    route(35, "ALL", "/query", "handleQuery"),
];

/// The registration whose handler `src/routes.ts` declares itself.
const SAME_FILE: Route = route(36, "GET", "/status", "localStatus");

/// The registration that is handed a router.
const ROUTER_BY_NAME: Route = route(37, "ALL", "/inner", "innerRouter");

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(FIXTURE)
}

/// One endpoint row of a model answer: the route and its handler's name, the
/// style under test, and no located expression.
fn row(route: &Route, emission_style: &str) -> String {
    format!(
        r#"{{"candidate_id":"@line:{line}","line_number":{line},"owner_node":"router",
            "method":"{method}","path":"{path}","handler_name":"{handler}",
            "pattern_matched":"route registration","emission_style":"{emission_style}",
            "payload_expression_text":null,"payload_expression_line":null,
            "response_expression_text":null,"response_expression_line":null}}"#,
        line = route.line,
        method = route.method,
        path = route.path,
        handler = route.handler,
    )
}

/// What a scan states for one route's response.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Response {
    /// The type the manifest entry serves, expanded. `None` for `unknown`.
    definition: Option<String>,
    /// Why the entry's root is `unknown`, when a layer said.
    reasons: Vec<String>,
    /// The text the type check reads for this entry.
    checked_as: String,
}

fn typed(text: &str) -> Response {
    Response {
        definition: Some(text.to_string()),
        reasons: Vec::new(),
        checked_as: text.to_string(),
    }
}

fn unknown_because(reason: &str) -> Response {
    Response {
        definition: None,
        reasons: vec![reason.to_string()],
        checked_as: "unknown".to_string(),
    }
}

/// An entry nothing was asked about.
fn not_asked() -> Response {
    Response {
        definition: None,
        reasons: Vec::new(),
        checked_as: "unknown".to_string(),
    }
}

/// Scan the fixture with `routes` answered for `src/routes.ts` under
/// `emission_style`, and return each route's response by registration line.
fn scan(routes: &[&Route], emission_style: &str) -> BTreeMap<i64, Response> {
    let answers = tempfile::tempdir().expect("temp answers dir");
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let analyze = answers.path().join("analyze-file");
    std::fs::create_dir_all(&analyze).expect("create analyze-file dir");
    let rows: Vec<String> = routes
        .iter()
        .map(|route| row(route, emission_style))
        .collect();
    std::fs::write(
        analyze.join("routes.json"),
        format!(
            r#"{{"mounts":[],"endpoints":[{}],"data_calls":[]}}"#,
            rows.join(",")
        ),
    )
    .expect("write routes answer");
    for stem in SILENT_FILES {
        std::fs::write(analyze.join(format!("{stem}.json")), NOTHING).expect("write answer");
    }

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(fixture_dir())
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", answers.path().display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1");
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
        "CARRICK_ALLOW_MISSING_TYPES",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("failed to spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let entry = std::fs::read_dir(storage.path())
        .expect("storage dir")
        .flatten()
        .next()
        .expect("one blob");
    let blob: serde_json::Value =
        serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
            .expect("parse blob");
    let surface = blob["capture_stub"]["files"]["types/surface.d.ts"]
        .as_str()
        .expect(
            "the scan captured a type surface; with none, the type sidecar did not run \
             (build it: `npm ci && npm run build` in src/sidecar)",
        );

    blob["type_manifest"]
        .as_array()
        .expect("type_manifest")
        .iter()
        .filter(|entry| {
            entry["role"] == "producer"
                && entry["type_kind"] == "response"
                && entry["file_path"] == "src/routes.ts"
        })
        .map(|entry| {
            let alias = entry["type_alias"].as_str().expect("an alias");
            let reasons = entry["any_provenance"]
                .as_array()
                .map(|findings| {
                    findings
                        .iter()
                        .filter(|finding| finding["path"] == "")
                        .filter_map(|finding| finding["reason"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            (
                entry["line_number"].as_i64().expect("a line"),
                Response {
                    definition: entry["expanded_definition"]
                        .as_str()
                        .or_else(|| entry["resolved_definition"].as_str())
                        .map(str::to_string),
                    reasons,
                    checked_as: surface_text(surface, alias),
                },
            )
        })
        .collect()
}

/// The text the capture's surface declares `alias` as, on one line.
fn surface_text(surface: &str, alias: &str) -> String {
    let opening = format!("export type {alias} = ");
    let start = surface
        .find(&opening)
        .unwrap_or_else(|| panic!("the surface declares no {alias}:\n{surface}"))
        + opening.len();
    let rest = &surface[start..];
    let end = rest
        .find(";\nexport type ")
        .or_else(|| rest.rfind(';'))
        .unwrap_or(rest.len());
    rest[..end].split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What each handler declared in another file sends, by registration line.
fn what_the_handlers_send() -> BTreeMap<i64, Response> {
    BTreeMap::from([
        // Returns its payload.
        (25, typed(&format!("{WIDGET}[]"))),
        // Returns its send, behind a gate whose 401 body is not the route's.
        (
            26,
            typed(&format!("{{ widget: {WIDGET}; asked: string; }}")),
        ),
        // Sends through the response it was handed and returns nothing.
        (27, typed(WIDGET)),
        // Ends the response with a 204.
        (28, unknown_because("no_response_body")),
        // Sends in the `try`; the `catch` hands the failure on.
        (29, typed("{ audited: number; by: string; }")),
        // Bound to a call that wraps the function.
        (30, typed(WIDGET)),
        // Its one send states a 410.
        (31, unknown_because("no_success_payload")),
        // Imported under another name.
        (32, typed("{ archived: string; }")),
        // Reached through a module that republishes it.
        (33, typed("{ count: number; }")),
        // The default export of its module.
        (34, typed("{ pong: boolean; }")),
        // Returns transport built from the request, by something else.
        (35, unknown_because("handler_body_unread")),
    ])
}

/// The readings a request located in the registration file can publish, none
/// of which is a response.
fn assert_never_the_registration(responses: &BTreeMap<i64, Response>, routes: &[&Route]) {
    for route in routes {
        let response = responses
            .get(&route.line)
            .unwrap_or_else(|| panic!("no response entry for line {}", route.line));
        for text in response
            .definition
            .iter()
            .chain(std::iter::once(&response.checked_as))
        {
            assert_ne!(
                text, "void",
                "line {}: `void` is what a handler returns, not what it sends",
                route.line
            );
            assert!(
                !text.contains("=>"),
                "line {}: a function is not a body: {text}",
                route.line
            );
            assert_ne!(
                text,
                &format!("\"{}\"", route.path),
                "line {}: the path is the registration's first argument",
                route.line
            );
            assert!(
                !text.contains("Router"),
                "line {}: the router is the registration call's own value: {text}",
                route.line
            );
        }
    }
}

fn assert_handlers_elsewhere_are_read(emission_style: &str) -> BTreeMap<i64, Response> {
    let mut routes: Vec<&Route> = ELSEWHERE.iter().collect();
    routes.push(&SAME_FILE);
    let responses = scan(&routes, emission_style);
    assert_never_the_registration(&responses, &routes);
    for (line, sends) in what_the_handlers_send() {
        assert_eq!(
            responses.get(&line),
            Some(&sends),
            "line {line} under `{emission_style}`"
        );
    }
    responses
}

/// carrick#1913: the model, shown the registration file alone, says
/// `no-payload`. Before, that made no request at all and every row read
/// `unknown` with nothing to say why.
#[test]
fn a_no_payload_row_is_asked_at_the_handler_another_file_declares() {
    let responses = assert_handlers_elsewhere_are_read("no-payload");
    assert_eq!(
        responses.get(&SAME_FILE.line),
        Some(&not_asked()),
        "a handler the registration's own file declares was read by the model, and its \
         `no-payload` stands"
    );
}

/// The same rows answered `imperative-send` with nothing located. A request
/// at the registration reads the handler the registration can be followed to;
/// a default export is one it cannot, and the handler's own declaration is.
#[test]
fn an_imperative_send_row_with_nothing_located_is_asked_at_the_handler() {
    let responses = assert_handlers_elsewhere_are_read("imperative-send");
    assert_eq!(
        responses.get(&SAME_FILE.line),
        Some(&typed("{ status: string; }")),
        "a handler declared beside its registration is still asked there"
    );
}

/// The same rows answered `return-value` with nothing located.
#[test]
fn a_return_value_row_with_nothing_located_is_asked_at_the_handler() {
    let responses = assert_handlers_elsewhere_are_read("return-value");
    assert_eq!(
        responses.get(&SAME_FILE.line),
        Some(&typed("{ status: string; }")),
        "a handler declared beside its registration is still asked there"
    );
}

/// A router passed by name is bound to a registration call, not to a
/// function. Its declaration is not offered as a handler's, so the row's
/// response is not the router.
#[test]
fn a_router_passed_by_name_is_not_read_as_a_handler() {
    let responses = scan(&[&ROUTER_BY_NAME], "no-payload");
    assert_never_the_registration(&responses, &[&ROUTER_BY_NAME]);
    assert_eq!(responses.get(&ROUTER_BY_NAME.line), Some(&not_asked()));
}
