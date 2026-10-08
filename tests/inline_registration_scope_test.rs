//! Routes registered inside an inline plugin take the prefix the registration
//! states, and nothing else (carrick#2092).
//!
//! `app.register(async (api) => { api.register(items) }, { prefix: … })`
//! hands the callback an instance of its own. Every inline plugin names that
//! instance the same way, so the scan has to tell them apart by where they
//! are registered, and read the prefix only where the source states it as a
//! literal.
//!
//! Each test drives the whole file analysis over the fixture with canned
//! model answers (`cached_model_results`), so no model is asked: what moves
//! is what the deterministic layer does with the rows the model states.

use carrick::agent_service::AgentService;
use carrick::agents::file_analyzer_agent::FileAnalysisResult;
use carrick::agents::file_orchestrator::{FileCentricAnalysisResult, FileOrchestrator};
use carrick::agents::framework_guidance_agent::{FrameworkGuidance, ProtocolGuidance};
use carrick::framework_detector::DetectionResult;
use carrick::operation::Protocol;
use carrick::swc_scanner::SwcScanner;
use serial_test::serial;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/inline-registration-scope")
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
/// An endpoint row the model states: `(needle, owner, method, path)`.
type Route<'a> = (&'a str, &'a str, &'a str, &'a str);

fn answer(file: &str, mounts: &[Mount], routes: &[Route]) -> (String, FileAnalysisResult) {
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
        .map(|(needle, owner, method, path)| {
            let line = line_of(file, needle);
            serde_json::json!({
                "candidate_id": candidate_at(file, line),
                "line_number": line,
                "owner_node": owner,
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
    let result: FileAnalysisResult = serde_json::from_value(serde_json::json!({
        "mounts": mounts,
        "endpoints": endpoints,
        "data_calls": [],
    }))
    .expect("a canned answer");
    (root().join(file).to_string_lossy().into_owned(), result)
}

fn items() -> (String, FileAnalysisResult) {
    answer(
        "src/items.ts",
        &[],
        &[("app.get('/items'", "app", "GET", "/items")],
    )
}

fn orders() -> (String, FileAnalysisResult) {
    answer(
        "src/orders.ts",
        &[],
        &[("app.get('/orders'", "app", "GET", "/orders")],
    )
}

/// The registering file's answer when the model states the outer
/// registration with `prefix_text` as its mount path.
fn stated(file: &str, prefix_text: &str) -> (String, FileAnalysisResult) {
    answer(
        file,
        &[
            ("app.register(", "app", "api", prefix_text, None),
            (
                "api.register(itemRoutes)",
                "api",
                "itemRoutes",
                "",
                Some("./items"),
            ),
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
async fn a_nullish_default_prefix_reaches_the_routes() {
    let result = analyze(vec![
        stated("src/nullish.ts", "opts.prefix ?? '/api/v1'"),
        items(),
    ])
    .await;
    assert!(
        served(&result).contains("GET /api/v1/items"),
        "{:?}",
        served(&result)
    );
    assert!(result.unread_mount_prefixes.is_empty());
}

#[tokio::test]
#[serial]
async fn a_logical_or_default_prefix_reaches_the_routes() {
    let result = analyze(vec![
        stated("src/either.ts", "opts.prefix || '/api/v2'"),
        items(),
    ])
    .await;
    assert_eq!(served(&result), set(&["GET /api/v2/items"]));
}

#[tokio::test]
#[serial]
async fn a_ternary_of_literals_serves_both_prefixes() {
    let result = analyze(vec![
        stated("src/ternary.ts", "opts.legacy ? '/legacy' : '/api/v3'"),
        items(),
    ])
    .await;
    assert_eq!(
        served(&result),
        set(&["GET /api/v3/items", "GET /legacy/items"])
    );
}

#[tokio::test]
#[serial]
async fn a_parameter_default_prefix_reaches_the_routes() {
    let result = analyze(vec![stated("src/param-default.ts", "prefix"), items()]).await;
    assert_eq!(served(&result), set(&["GET /api/v4/items"]));
}

#[tokio::test]
#[serial]
async fn a_const_bound_prefix_reaches_the_routes() {
    let result = analyze(vec![stated("src/const-prefix.ts", "API_PREFIX"), items()]).await;
    assert_eq!(served(&result), set(&["GET /api/v5/items"]));
}

#[tokio::test]
#[serial]
async fn an_unreadable_prefix_is_recorded_not_guessed() {
    let result = analyze(vec![stated("src/unread.ts", "opts.prefix"), items()]).await;
    assert_eq!(served(&result), set(&["GET /items"]));
    let line = line_of("src/unread.ts", "app.register(");
    let sites: Vec<&str> = result
        .unread_mount_prefixes
        .iter()
        .map(|record| record.site.as_str())
        .collect();
    assert_eq!(sites, vec![format!("src/unread.ts:{line}").as_str()]);
    assert_eq!(result.unread_mount_prefixes[0].expression, "opts.prefix");
    assert_eq!(result.stats.mount_prefixes_unread, 1);
}

#[tokio::test]
#[serial]
async fn two_inline_scopes_with_one_parameter_name_stay_apart() {
    let file = "src/two-scopes.ts";
    let text = source(file);
    let registrations: Vec<usize> = text
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("app.register("))
        .map(|(index, _)| index + 1)
        .collect();
    let (path, mut answer) = answer(
        file,
        &[
            (
                "api.register(itemRoutes)",
                "api",
                "itemRoutes",
                "",
                Some("./items"),
            ),
            (
                "api.register(orderRoutes)",
                "api",
                "orderRoutes",
                "",
                Some("./orders"),
            ),
        ],
        &[],
    );
    for (line, prefix) in registrations.iter().zip(["/a", "/b"]) {
        answer.mounts.push(
            serde_json::from_value(serde_json::json!({
                "line_number": line,
                "parent_node": "app",
                "child_node": "api",
                "mount_path": prefix,
                "import_source": null,
                "pattern_matched": ".register(",
            }))
            .expect("a mount row"),
        );
    }
    let result = analyze(vec![(path, answer), items(), orders()]).await;
    assert_eq!(served(&result), set(&["GET /a/items", "GET /b/orders"]));
}

#[tokio::test]
#[serial]
async fn a_registration_the_model_left_unstated_is_recorded() {
    let file = "src/silent.ts";
    let silent = answer(
        file,
        &[(
            "api.register(itemRoutes)",
            "api",
            "itemRoutes",
            "",
            Some("./items"),
        )],
        &[],
    );
    let result = analyze(vec![silent, items()]).await;
    assert_eq!(served(&result), set(&["GET /items"]));
    let line = line_of(file, "app.register(");
    let scope = format!("api@{file}:{line}");
    assert!(
        result
            .mount_graph
            .get_mounts()
            .iter()
            .any(|edge| edge.parent == "app" && edge.child == scope && edge.path_prefix.is_empty()),
        "the registration is an edge of its own: {:?}",
        result.mount_graph.get_mounts()
    );
    assert_eq!(result.unread_mount_prefixes.len(), 1);
    assert_eq!(
        result.unread_mount_prefixes[0].expression,
        "{ prefix: opts.prefix ?? '/api/v1' }"
    );
}

#[tokio::test]
#[serial]
async fn a_registration_with_no_options_is_an_edge_and_no_record() {
    let file = "src/no-options.ts";
    let quiet = answer(
        file,
        &[(
            "scope.register(orderRoutes)",
            "scope",
            "orderRoutes",
            "",
            Some("./orders"),
        )],
        &[],
    );
    let result = analyze(vec![quiet, orders()]).await;
    assert_eq!(served(&result), set(&["GET /orders"]));
    assert!(result.unread_mount_prefixes.is_empty());
}

#[tokio::test]
#[serial]
async fn inline_callbacks_that_head_no_rows_emit_nothing() {
    let callbacks = answer(
        "src/callbacks.ts",
        &[],
        &[("app.get('/ids'", "app", "GET", "/ids")],
    );
    let result = analyze(vec![callbacks]).await;
    assert!(result.mount_graph.get_mounts().is_empty());
    assert_eq!(served(&result), set(&["GET /ids"]));
    assert!(result.unread_mount_prefixes.is_empty());
}

#[tokio::test]
#[serial]
async fn a_group_callback_with_a_first_argument_prefix() {
    let group = answer(
        "src/group.ts",
        &[
            ("app.group(", "app", "g", "/v2", None),
            ("app.group(", "g", "orderRoutes", "", Some("./orders")),
        ],
        &[],
    );
    let result = analyze(vec![group, orders()]).await;
    assert_eq!(served(&result), set(&["GET /v2/orders"]));
}

#[tokio::test]
#[serial]
async fn a_route_registered_directly_in_the_scope_takes_its_prefix() {
    let file = "src/nullish.ts";
    let (path, mut nullish) = stated(file, "opts.prefix ?? '/api/v1'");
    let (_, health) = answer(file, &[], &[("api.get('/health'", "api", "GET", "/health")]);
    nullish.endpoints = health.endpoints;
    let result = analyze(vec![(path, nullish), items()]).await;
    assert_eq!(
        served(&result),
        set(&["GET /api/v1/health", "GET /api/v1/items"])
    );
}

#[tokio::test]
#[serial]
async fn a_scope_inside_a_mounted_plugin_composes_under_both() {
    // `index.ts` mounts the plugin `nullish.ts` exports. The file-first
    // identity hands every owner in a mounted file to that binding; a scope
    // id must keep its own node, or the inner prefix is lost.
    let file = "src/nullish.ts";
    let (path, mut nullish) = stated(file, "opts.prefix ?? '/api/v1'");
    let (_, health) = answer(file, &[], &[("api.get('/health'", "api", "GET", "/health")]);
    nullish.endpoints = health.endpoints;
    let index = answer(
        "src/index.ts",
        &[(
            "server.register(api",
            "server",
            "api",
            "/root",
            Some("./nullish"),
        )],
        &[],
    );
    let result = analyze(vec![index, (path, nullish), items()]).await;
    assert_eq!(
        served(&result),
        set(&["GET /root/api/v1/health", "GET /root/api/v1/items"])
    );
}

#[test]
fn the_fixture_root_is_on_disk() {
    assert!(Path::new(&root()).join("src/nullish.ts").is_file());
}
