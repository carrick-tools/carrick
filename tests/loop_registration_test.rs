//! A route registered in a loop over values the source holds is one row per
//! value (carrick#2093).
//!
//! `for (const action of ACTIONS) app.post(`/x/${action}`, h)` registers one
//! route per element of `ACTIONS`. The model states one row at the call and
//! writes the hole however it likes (`${action}`, `:action`), and either way
//! no call matches it. The scan reads the loop and the array off the AST and
//! states the routes; everything it cannot read keeps the model's row.
//!
//! Each test drives the whole file analysis over the fixture with canned
//! model answers (`cached_model_results`), so no model is asked: what moves
//! is what the deterministic layer does with the rows the model states.

use carrick::agent_service::AgentService;
use carrick::agents::file_analyzer_agent::{FileAnalysisResult, ResolutionSource};
use carrick::agents::file_orchestrator::{FileCentricAnalysisResult, FileOrchestrator};
use carrick::agents::framework_guidance_agent::{FrameworkGuidance, ProtocolGuidance};
use carrick::framework_detector::DetectionResult;
use carrick::operation::Protocol;
use carrick::swc_scanner::SwcScanner;
use serial_test::serial;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/loop-registered-routes")
}

fn source(file: &str) -> String {
    std::fs::read_to_string(root().join(file)).expect("fixture file")
}

/// The 1-based line holding `needle`, which must appear once.
fn line_of(file: &str, needle: &str) -> usize {
    let text = source(file);
    let lines: Vec<usize> = text
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains(needle))
        .map(|(index, _)| index + 1)
        .collect();
    assert_eq!(lines.len(), 1, "{needle:?} in {file}: {lines:?}");
    lines[0]
}

/// The id the scanner gives the call that starts on `line`, as the model
/// would echo it.
fn candidate_at(file: &str, line: usize) -> String {
    let scanned = SwcScanner::new().scan_content(&root().join(file), &source(file), &[], &[]);
    scanned
        .candidates
        .iter()
        .find(|candidate| candidate.line_number == line)
        .unwrap_or_else(|| panic!("no candidate at {file}:{line}"))
        .candidate_id
        .clone()
}

/// A mount row the model states: `(needle on the call's line, parent,
/// child, mount_path, import_source)`.
type Mount<'a> = (&'a str, &'a str, &'a str, &'a str, Option<&'a str>);
/// An endpoint row the model states: `(needle, method, path)`, owned by `app`.
type Route<'a> = (&'a str, &'a str, &'a str);
/// A data call the model states: `(needle, method, target)`.
type Call<'a> = (&'a str, &'a str, &'a str);

fn answer(
    file: &str,
    mounts: &[Mount],
    routes: &[Route],
    calls: &[Call],
) -> (String, FileAnalysisResult) {
    let mounts: Vec<serde_json::Value> = mounts
        .iter()
        .map(|(needle, parent, child, path, import)| {
            serde_json::json!({
                "line_number": line_of(file, needle),
                "parent_node": parent,
                "child_node": child,
                "mount_path": path,
                "import_source": import,
                "pattern_matched": ".register(",
            })
        })
        .collect();
    let endpoints: Vec<serde_json::Value> = routes
        .iter()
        .map(|(needle, method, path)| {
            let line = line_of(file, needle);
            serde_json::json!({
                "candidate_id": candidate_at(file, line),
                "line_number": line,
                "owner_node": "app",
                "method": method,
                "path": path,
                "handler_name": "anonymous",
                "pattern_matched": ".get(",
                "payload_expression_text": null,
                "payload_expression_line": null,
                "response_expression_text": null,
                "response_expression_line": null,
                "primary_type_symbol": null,
                "type_import_source": null
            })
        })
        .collect();
    let data_calls: Vec<serde_json::Value> = calls
        .iter()
        .map(|(needle, method, target)| {
            let line = line_of(file, needle);
            serde_json::json!({
                "candidate_id": candidate_at(file, line),
                "line_number": line,
                "target": target,
                "method": method,
                "pattern_matched": "fetch(",
                "payload_expression_text": null,
                "payload_expression_line": null,
                "primary_type_symbol": null,
                "type_import_source": null
            })
        })
        .collect();
    let result: FileAnalysisResult = serde_json::from_value(serde_json::json!({
        "mounts": mounts,
        "endpoints": endpoints,
        "data_calls": data_calls,
    }))
    .expect("a canned answer");
    (root().join(file).to_string_lossy().into_owned(), result)
}

const PLAN: &str = "app.post(`/teams/:id/plan/actions/${action}`";
const REPORTS: &str = "app.get(`/reports/${kind}`";
const STATUS: &str = "app.get(`/status/${page}`";
const NESTED: &str = "app.get(`/${v}/${r}`";
const QUIET: &str = "app.get(`/quiet/${quiet}`";

/// The routes file answered with every loop site's path as the template
/// text, the quiet site left out.
fn plan_routes(plan_path: &str) -> (String, FileAnalysisResult) {
    answer(
        "src/plan-routes.ts",
        &[],
        &[
            (PLAN, "POST", plan_path),
            (REPORTS, "GET", "/reports/${kind}"),
            (STATUS, "GET", "/status/:page"),
            (NESTED, "GET", "/${v}/${r}"),
        ],
        &[],
    )
}

async fn analyze(answers: Vec<(String, FileAnalysisResult)>) -> FileCentricAnalysisResult {
    // SAFETY: serial test; env vars are process-global. Every file has a
    // canned answer, so nothing is dispatched; the mock is a backstop.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
    }
    let root = root();
    let files: Vec<PathBuf> = answers
        .iter()
        .map(|(path, _)| PathBuf::from(path))
        .collect();
    let cached: HashMap<String, FileAnalysisResult> = answers.into_iter().collect();
    FileOrchestrator::new(AgentService::new())
        .analyze_files(
            &files,
            &cached,
            // Guidance shapes only the prompt, and nothing is prompted here.
            &ProtocolGuidance::from([(
                Protocol::Http,
                FrameworkGuidance {
                    mount_patterns: vec![],
                    endpoint_patterns: vec![],
                    middleware_patterns: vec![],
                    data_fetching_patterns: vec![],
                    triage_hints: String::new(),
                    parsing_notes: String::new(),
                    guidance_key: None,
                },
            )]),
            &DetectionResult {
                frameworks: vec![],
                data_fetchers: vec![],
                messaging_clients: vec![],
                socket_clients: vec![],
                notes: String::new(),
                client_semantics: None,
            },
            &root,
            &root,
            &[],
            &Default::default(),
            &Default::default(),
            &carrick::url_normalizer::UrlNormalizer::default_permissive(),
            &carrick::workspace_resolver::WorkspaceIndex::build_with_aliases(&root, None),
            None,
        )
        .await
        .expect("the analysis ran")
}

fn served(result: &FileCentricAnalysisResult) -> BTreeSet<String> {
    result
        .mount_graph
        .get_resolved_endpoints()
        .iter()
        .map(|endpoint| format!("{} {}", endpoint.method, endpoint.full_path))
        .collect()
}

fn set(routes: &[&str]) -> BTreeSet<String> {
    routes.iter().map(|route| route.to_string()).collect()
}

#[tokio::test]
#[serial]
async fn an_imported_const_array_states_one_route_per_value() {
    let result = analyze(vec![plan_routes("/teams/:id/plan/actions/${action}")]).await;
    let plan: Vec<_> = result
        .mount_graph
        .get_resolved_endpoints()
        .iter()
        .filter(|endpoint| endpoint.full_path.contains("/plan/"))
        .collect();
    assert_eq!(
        plan.iter()
            .map(|endpoint| format!("{} {}", endpoint.method, endpoint.full_path))
            .collect::<Vec<_>>(),
        [
            "POST /teams/:id/plan/actions/create",
            "POST /teams/:id/plan/actions/preview",
            "POST /teams/:id/plan/actions/apply",
        ]
    );
    for endpoint in &plan {
        assert_eq!(
            endpoint.resolution_source,
            Some(ResolutionSource::InlineLiteral)
        );
    }
    assert!(
        served(&result).iter().all(|route| !route.contains("${")),
        "{:?}",
        served(&result)
    );
    assert_eq!(result.stats.loop_registrations_expanded, 4);
    assert_eq!(result.stats.loop_routes_stated, 3 + 2 + 2 + 4);
}

#[tokio::test]
#[serial]
async fn a_tuple_loop_answered_as_a_catch_all_states_each_file() {
    let result = analyze(vec![answer(
        "src/assets.ts",
        &[],
        &[("app.get(`/${file}`", "GET", "/:file")],
        &[],
    )])
    .await;
    assert_eq!(served(&result), set(&["GET /favicon.svg", "GET /logo.png"]));
}

#[tokio::test]
#[serial]
async fn same_file_arrays_for_each_and_nested_loops_are_read() {
    let result = analyze(vec![plan_routes("/teams/:id/plan/actions/${action}")]).await;
    let served = served(&result);
    for route in [
        "GET /reports/daily",
        "GET /reports/weekly",
        "GET /status/up",
        "GET /status/down",
        "GET /v1/users",
        "GET /v1/teams",
        "GET /v2/users",
        "GET /v2/teams",
    ] {
        assert!(served.contains(route), "{route} not in {served:?}");
    }
    assert_eq!(served.len(), 3 + 2 + 2 + 4, "{served:?}");
}

#[tokio::test]
#[serial]
async fn a_prefix_the_model_copied_from_the_mount_is_served_once() {
    let result = analyze(vec![
        answer(
            "src/server.ts",
            &[(
                "app.register(planRoutes",
                "app",
                "planRoutes",
                "/api/v1",
                Some("./plan-routes.js"),
            )],
            &[],
            &[],
        ),
        // The model copied the last segment of the mount into the route.
        plan_routes("/v1/teams/:id/plan/actions/${action}"),
    ])
    .await;
    // The mount reaches the routes the model wrote no prefix for.
    assert!(
        served(&result).contains("GET /api/v1/reports/daily"),
        "{:?}",
        served(&result)
    );
    let plan: BTreeSet<String> = served(&result)
        .into_iter()
        .filter(|route| route.contains("/plan/"))
        .collect();
    assert_eq!(
        plan,
        set(&[
            "POST /api/v1/teams/:id/plan/actions/apply",
            "POST /api/v1/teams/:id/plan/actions/create",
            "POST /api/v1/teams/:id/plan/actions/preview",
        ])
    );
}

#[tokio::test]
#[serial]
async fn loops_over_values_the_source_does_not_hold_keep_the_model_row() {
    let routes: [Route; 7] = [
        ("app.post(`/x/${action}`", "POST", "/x/${action}"),
        ("app.get(`/meta${p}`", "GET", "/meta/:path"),
        ("app.get(`/mixed/${m}`", "GET", "/mixed/${m}"),
        ("app.get(`/later/${l}`", "GET", "/later/:l"),
        ("app.get(`/grown/${g}`", "GET", "/grown/${g}"),
        ("app.get(`/shadow/${s}`", "GET", "/shadow/${s}"),
        ("app.get(`/${V}/${r}`", "GET", "/${V}/${r}"),
    ];
    let result = analyze(vec![answer("src/unread.ts", &[], &routes, &[])]).await;
    let expected: BTreeSet<String> = routes
        .iter()
        .map(|(_, method, path)| format!("{method} {path}"))
        .collect();
    assert_eq!(served(&result), expected);
    assert_eq!(result.stats.loop_registrations_expanded, 0);
}

#[tokio::test]
#[serial]
async fn literal_registrations_descriptors_and_calls_in_loops_are_unchanged() {
    let result = analyze(vec![answer(
        "src/kept.ts",
        &[],
        &[
            ("app.get('/:id'", "GET", "/:id"),
            ("app.get('/*'", "GET", "/*"),
        ],
        &[("fetch(`/reports/${kind}`)", "GET", "/reports/${kind}")],
    )])
    .await;
    let served = served(&result);
    assert!(served.contains("GET /:id"), "{served:?}");
    assert!(served.contains("GET /*"), "{served:?}");
    assert!(served.contains("GET /health"), "{served:?}");
    assert!(
        served.iter().all(|route| !route.contains("/reports/")),
        "{served:?}"
    );
    let calls: Vec<String> = result
        .mount_graph
        .get_data_calls()
        .iter()
        .map(|call| format!("{} {}", call.method, call.target_url))
        .collect();
    assert_eq!(calls, ["GET /reports/${kind}"]);
    assert_eq!(result.stats.loop_registrations_expanded, 0);
}

#[tokio::test]
#[serial]
async fn a_model_that_answers_each_value_still_gets_one_row_per_value() {
    let result = analyze(vec![answer(
        "src/plan-routes.ts",
        &[],
        &[
            (PLAN, "POST", "/teams/:id/plan/actions/create"),
            (PLAN, "POST", "/teams/:id/plan/actions/preview"),
            (PLAN, "POST", "/teams/:id/plan/actions/apply"),
        ],
        &[],
    )])
    .await;
    let plan: Vec<String> = result
        .mount_graph
        .get_resolved_endpoints()
        .iter()
        .map(|endpoint| format!("{} {}", endpoint.method, endpoint.full_path))
        .collect();
    assert_eq!(
        plan,
        [
            "POST /teams/:id/plan/actions/create",
            "POST /teams/:id/plan/actions/preview",
            "POST /teams/:id/plan/actions/apply",
        ]
    );
    assert!(
        result
            .mount_graph
            .get_resolved_endpoints()
            .iter()
            .all(|endpoint| endpoint.resolution_source == Some(ResolutionSource::InlineLiteral))
    );
}

#[tokio::test]
#[serial]
async fn a_loop_site_the_model_states_no_row_at_has_none() {
    let result = analyze(vec![plan_routes("/teams/:id/plan/actions/${action}")]).await;
    let line = line_of("src/plan-routes.ts", QUIET);
    assert!(
        served(&result)
            .iter()
            .all(|route| !route.contains("/quiet")),
        "{:?}",
        served(&result)
    );
    assert!(
        result
            .mount_graph
            .get_resolved_endpoints()
            .iter()
            .all(|endpoint| !endpoint.file_location.ends_with(&format!(":{line}"))),
    );
}
