//! A route registered once per element of a constant list is typed for its
//! own element (carrick#1029).
//!
//! ```ts
//! for (const [path, key] of [['alpha-rows', 'alpha'], ['beta-rows', 'beta']] as const)
//!   app.get(`/items/${path}`, async () => ({ items: rows()[key] }));
//! ```
//!
//! One handler serves `/items/alpha-rows` and `/items/beta-rows`. The index
//! holds one row per operation at the registration's line, and before this
//! each row's response was the handler's return as written: the union of
//! both payloads, so a consumer of either read as mismatched.
//!
//! The scanner sends each row's own path on the requests for a line that
//! serves several (`collect_type_requests`); the sidecar types the handler
//! with the list rewritten to the element the path names. A row with a line
//! of its own sends no path, and a list the code does not state keeps the
//! union.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use carrick::agent_service::AgentService;
use carrick::agents::file_analyzer_agent::{EndpointResult, FileAnalysisResult};
use carrick::agents::file_orchestrator::FileOrchestrator;
use carrick::config::Config;
use carrick::services::type_sidecar::{InferKind, InferRequestItem, TypeSidecar};
use carrick::url_normalizer::UrlNormalizer;
use tempfile::TempDir;

const ROUTES_TS: &str = r#"interface Alpha {
  alphaId: number;
}
interface Beta {
  betaName: string;
}
interface App {
  get(path: string, handler: () => Promise<unknown>): void;
}
declare const app: App;
declare function rows(): { alpha: Alpha[]; beta: Beta[] };
declare function listKinds(): Array<'alpha' | 'beta'>;

for (const [path, key] of [['alpha-rows', 'alpha'], ['beta-rows', 'beta']] as const) app.get(`/items/${path}`, async () => ({ items: rows()[key] }));

for (const kind of listKinds()) {
  app.get(`/listed/${kind}`, async () => ({ items: rows()[kind] }));
}

app.get('/health', async () => ({ ok: true }));
"#;

const TSCONFIG: &str = r#"{
  "compilerOptions": { "strict": true, "target": "es2022", "module": "esnext", "types": [] },
  "include": ["src/**/*.ts"]
}
"#;

fn line_of(needle: &str) -> i32 {
    let at = ROUTES_TS
        .find(needle)
        .expect("the fixture holds the needle");
    ROUTES_TS[..at].lines().count() as i32 + 1
}

/// A model row at `needle`'s line, its response located as the expression
/// the handler returns there (the shape a model states for such a route).
fn endpoint(path: &str, needle: &str, returned: &str) -> EndpointResult {
    let line = line_of(needle);
    assert_eq!(line_of(returned), line, "{returned} is on the route's line");
    EndpointResult {
        handler_declaration_line: None,
        registration_literal: None,
        view_module: false,
        candidate_id: format!("loop:{path}"),
        line_number: line,
        owner_node: "app".to_string(),
        method: "GET".to_string(),
        path: path.to_string(),
        handler_name: "anonymous".to_string(),
        pattern_matched: ".get(".to_string(),
        call_expression_span_start: None,
        call_expression_span_end: None,
        payload_expression_text: None,
        payload_expression_line: None,
        response_expression_text: Some(returned.to_string()),
        response_expression_line: Some(line),
        emission_style: None,
        primary_type_symbol: None,
        type_import_source: None,
        resolution_source: None,
        dispatch: None,
    }
}

/// The scanner's response requests for the fixture's rows, by row path.
fn response_requests(repo: &Path) -> HashMap<String, InferRequestItem> {
    let routes_file = repo.join("src/routes.ts").to_string_lossy().to_string();
    let endpoints = vec![
        endpoint(
            "/items/alpha-rows",
            "for (const [path",
            "{ items: rows()[key] }",
        ),
        endpoint(
            "/items/beta-rows",
            "for (const [path",
            "{ items: rows()[key] }",
        ),
        endpoint(
            "/listed/alpha",
            "app.get(`/listed/",
            "{ items: rows()[kind] }",
        ),
        endpoint(
            "/listed/beta",
            "app.get(`/listed/",
            "{ items: rows()[kind] }",
        ),
        endpoint("/health", "app.get('/health'", "{ ok: true }"),
    ];
    let mut file_results = HashMap::new();
    file_results.insert(
        routes_file,
        FileAnalysisResult {
            graphql_consumer_locates: vec![],
            mounts: vec![],
            endpoints,
            data_calls: vec![],
            graphql_operations: vec![],
            pubsub_operations: vec![],
            dispatch_tables: Vec::new(),
        },
    );
    let orchestrator = FileOrchestrator::new(AgentService::new());
    let mount_graph = orchestrator.build_mount_graph(
        &file_results,
        &UrlNormalizer::default_permissive(),
        Path::new(""),
        Path::new(""),
    );
    let (_explicit, infer, _inline) = orchestrator.collect_type_requests(
        &file_results,
        &repo.to_string_lossy(),
        &mount_graph,
        &Config::default(),
        &carrick::workspace_resolver::WorkspaceIndex::build_with_aliases(repo, None),
    );
    let paths: HashMap<i32, Vec<&str>> = [
        (
            line_of("for (const [path"),
            vec!["/items/alpha-rows", "/items/beta-rows"],
        ),
        (
            line_of("app.get(`/listed/"),
            vec!["/listed/alpha", "/listed/beta"],
        ),
        (line_of("app.get('/health'"), vec!["/health"]),
    ]
    .into_iter()
    .collect();
    let mut out = HashMap::new();
    for item in infer
        .into_iter()
        .filter(|item| item.infer_kind == InferKind::ResponseBody)
    {
        let key = match &item.route_path {
            Some(path) => path.clone(),
            None => {
                let at = paths
                    .get(&(item.line_number as i32))
                    .expect("a request at a fixture route's line");
                assert_eq!(
                    at.len(),
                    1,
                    "a shared line's request names its path: {item:?}"
                );
                at[0].to_string()
            }
        };
        out.insert(key, item);
    }
    out
}

fn write_repo(repo: &Path) {
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("tsconfig.json"), TSCONFIG).unwrap();
    fs::write(repo.join("src/routes.ts"), ROUTES_TS).unwrap();
}

/// carrick#1029: each row on a line that serves several operations carries
/// its own path to the sidecar; a row with a line of its own carries none.
#[test]
fn a_shared_line_sends_each_rows_path() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("api");
    write_repo(&repo);
    let requests = response_requests(&repo);
    for path in [
        "/items/alpha-rows",
        "/items/beta-rows",
        "/listed/alpha",
        "/listed/beta",
    ] {
        let item = requests
            .get(path)
            .unwrap_or_else(|| panic!("{path}: {requests:?}"));
        assert_eq!(item.route_path.as_deref(), Some(path));
    }
    assert_eq!(requests["/health"].route_path, None);
}

fn sidecar_path() -> Option<PathBuf> {
    let node = Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    (node && path.exists()).then_some(path)
}

/// carrick#1029, through the real sidecar: each element's row is typed with
/// its own payload; a list the code does not state keeps the union.
#[test]
fn each_loop_element_is_typed_with_its_own_payload() {
    let Some(sidecar_path) = sidecar_path() else {
        eprintln!("Skipping: Node.js or the built sidecar is not available");
        return;
    };
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("api");
    write_repo(&repo);
    let requests = response_requests(&repo);
    let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
    sidecar.start_init(&repo, None);
    sidecar
        .wait_ready(Duration::from_secs(60))
        .expect("sidecar init");
    let items: Vec<InferRequestItem> = requests.values().cloned().collect();
    let result = sidecar
        .resolve_all_types(&[], &items, None, None)
        .expect("resolve_all_types");
    let typed = |path: &str| {
        let alias = requests[path].alias.clone().expect("an alias");
        let text = result
            .inferred_types
            .iter()
            .find(|t| t.alias == alias)
            .unwrap_or_else(|| panic!("{path} was typed: {:?}", result.inferred_types))
            .type_string
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        text
    };
    assert_eq!(
        typed("/items/alpha-rows"),
        "{ items: { alphaId: number; }[]; }"
    );
    assert_eq!(
        typed("/items/beta-rows"),
        "{ items: { betaName: string; }[]; }"
    );
    for path in ["/listed/alpha", "/listed/beta"] {
        assert_eq!(
            typed(path),
            "{ items: { alphaId: number; }[] | { betaName: string; }[]; }",
            "{path}"
        );
    }
}
