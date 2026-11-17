//! A text or line anchor that lands outside the handler of the row it is for
//! gives `unknown`, never another node's type (stale-position T2).
//!
//! A model that mis-states a hint line hands the scanner a text whose line is
//! in the previous route's handler, or inside a callback of the right handler.
//! The text also occurs there, so the sidecar found that node and published
//! its type as this route's contract. The scanner now sends the handler's
//! lines with the request and the sidecar refuses a node outside them.
//!
//! The tests drive the live path: the scanner's own request collection over an
//! analyzer result, then the real sidecar over those items. They need the
//! built sidecar (`cd src/sidecar && npm run build`) and refuse to pass
//! without it where CI has one.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use carrick::agent_service::AgentService;
use carrick::agents::file_analyzer_agent::{EmissionStyle, EndpointResult, FileAnalysisResult};
use carrick::agents::file_orchestrator::FileOrchestrator;
use carrick::config::Config;
use carrick::services::type_sidecar::{InferKind, InferRequestItem, TypeSidecar};
use carrick::url_normalizer::UrlNormalizer;
use tempfile::TempDir;

const ROUTES_TS: &str = r#"import { Server, RouteReply, RouteRequest } from './framework';
import { GadgetBody, OrderBody } from './shapes';
import { gated } from './gate';

export function registerRoutes(server: Server, ids: string[]) {
  server.post('/gadgets', async (request: RouteRequest<unknown>, reply: RouteReply) => {
    const payload: GadgetBody = request.body as GadgetBody;
    const a = 1;
    const b = 2;
    const c = 3;
    const d = 4;
    const e = 5;
    const f = 6;
    const g = 7;
    const h = 8;
    const i = 9;
    const j = 10;
    reply.send({ saved: payload.name, total: a + b + c + d + e + f + g + h + i + j });
  });

  server.post('/orders', async (request: RouteRequest<unknown>, reply: RouteReply) => {
    const payload: OrderBody = request.body as OrderBody;
    reply.send({ total: payload.quantity });
  });

  server.get('/slots', async (request: RouteRequest<unknown>, reply: RouteReply) => {
    const labels = ids.map((id) => ({ id, label: 'slot' }));
    return { count: labels.length, labels };
  });

  server.get('/wrapped', (request: RouteRequest<unknown>, reply: RouteReply) =>
    gated(reply, async () => {
      return { ok: true, attempts: 1 };
    }));
}
"#;

fn write_fixture_repo(repo: &Path) {
    fs::create_dir_all(repo.join("src")).expect("create src");
    fs::write(
        repo.join("package.json"),
        r#"{ "name": "anchor-bounds-api", "version": "1.0.0" }"#,
    )
    .unwrap();
    fs::write(
        repo.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "target": "ES2020",
    "module": "commonjs",
    "moduleResolution": "node",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true
  },
  "include": ["src/**/*"]
}"#,
    )
    .unwrap();
    fs::write(
        repo.join("src/framework.ts"),
        r#"export interface RouteRequest<TBody> {
  body: TBody;
}

export interface RouteReply {
  send(payload: unknown): void;
}

export interface Server {
  post(path: string, handler: (request: never, reply: RouteReply) => unknown): void;
  get(path: string, handler: (request: never, reply: RouteReply) => unknown): void;
}
"#,
    )
    .unwrap();
    fs::write(
        repo.join("src/shapes.ts"),
        r#"export interface GadgetBody {
  name: string;
  weightGrams: number;
}

export interface OrderBody {
  sku: string;
  quantity: number;
}
"#,
    )
    .unwrap();
    fs::write(
        repo.join("src/gate.ts"),
        r#"import { RouteReply } from './framework';

export function gated<T>(reply: RouteReply, run: () => Promise<T>): Promise<T> {
  return run();
}
"#,
    )
    .unwrap();
    fs::write(repo.join("src/routes.ts"), ROUTES_TS).unwrap();
}

/// 1-based line of the first line of `ROUTES_TS` holding `needle`.
fn line_of(needle: &str) -> i32 {
    ROUTES_TS
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("fixture has no line with {needle:?}")) as i32
        + 1
}

/// The call span of the registration that opens at `opening` (a text that
/// starts with `server.`), in the numbering the scanner records.
fn call_span(opening: &str) -> (u32, u32) {
    let start = ROUTES_TS.find(opening).expect("registration in fixture");
    let mut depth = 0usize;
    for (offset, ch) in ROUTES_TS[start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    // Recorded in the scanner's numbering, offset by its base.
                    let base = carrick::swc_scanner::SWC_SPAN_BASE;
                    return (start as u32 + base, (start + offset + 1) as u32 + base);
                }
            }
            _ => {}
        }
    }
    panic!("unclosed registration");
}

struct Route {
    opening: &'static str,
    method: &'static str,
    path: &'static str,
    emission: Option<EmissionStyle>,
    payload: Option<(&'static str, i32)>,
    response: Option<(&'static str, i32)>,
}

fn analyzer_result(routes_file: &str, routes: &[Route]) -> HashMap<String, FileAnalysisResult> {
    let endpoints = routes
        .iter()
        .map(|route| {
            let (start, end) = call_span(route.opening);
            EndpointResult {
                handler_declaration_line: None,
                registration_literal: None,
                view_module: false,
                candidate_id: format!("span:{start}-{end}"),
                line_number: line_of(route.opening),
                owner_node: "server".to_string(),
                method: route.method.to_string(),
                path: route.path.to_string(),
                handler_name: "anonymous".to_string(),
                pattern_matched: format!(".{}(", route.method.to_lowercase()),
                call_expression_span_start: Some(start),
                call_expression_span_end: Some(end),
                payload_expression_text: route.payload.map(|(text, _)| text.to_string()),
                payload_expression_line: route.payload.map(|(_, line)| line),
                response_expression_text: route.response.map(|(text, _)| text.to_string()),
                response_expression_line: route.response.map(|(_, line)| line),
                emission_style: route.emission,
                primary_type_symbol: None,
                type_import_source: None,
                resolution_source: None,
                dispatch: None,
            }
        })
        .collect();
    let mut file_results = HashMap::new();
    file_results.insert(
        routes_file.to_string(),
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
    file_results
}

fn collect(repo: &Path, routes: &[Route]) -> Vec<InferRequestItem> {
    let routes_file = repo.join("src/routes.ts").to_string_lossy().to_string();
    let file_results = analyzer_result(&routes_file, routes);
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
    infer
}

fn sidecar_path() -> Option<PathBuf> {
    let node = Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !node {
        eprintln!("Skipping test: Node.js not available");
        return None;
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    if !path.exists() {
        eprintln!("Skipping test: sidecar not built (run: cd src/sidecar && npm run build)");
        return None;
    }
    Some(path)
}

/// What the scan stores for the one request of `kind` the routes collect: the
/// inferred type's text, and the declaration the bundle writes for its alias
/// (`unknown` where the request abstained). `None` when no sidecar is built.
fn stored(routes: &[Route], kind: InferKind, opening: &str) -> Option<(String, String)> {
    let sidecar_path = sidecar_path()?;
    let temp = TempDir::new().expect("temp dir");
    let repo = temp.path().join("api");
    write_fixture_repo(&repo);
    let mut items = collect(&repo, routes);
    items.retain(|item| item.infer_kind == kind);
    let alias = {
        let item = items
            .iter()
            .find(|item| item.line_number as i32 == line_of(opening))
            .unwrap_or_else(|| panic!("no {kind:?} request for {opening}: {items:?}"));
        item.alias.clone().expect("alias")
    };
    items.retain(|item| item.alias.as_deref() == Some(alias.as_str()));

    let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
    sidecar.start_init(&repo, None);
    sidecar
        .wait_ready(Duration::from_secs(60))
        .expect("sidecar init");
    let result = sidecar
        .resolve_all_types(&[], &items, None, None)
        .expect("resolve_all_types");
    let inferred = result
        .inferred_types
        .iter()
        .find(|t| t.alias == alias)
        .map(|t| t.type_string.clone())
        .unwrap_or_else(|| "<none>".to_string());
    let declared = result
        .dts_content
        .as_deref()
        .and_then(|dts| {
            dts.lines()
                .find(|line| line.contains(&format!("type {alias}")))
        })
        .unwrap_or("<no declaration>")
        .to_string();
    Some((inferred, declared))
}

fn gadget_and_order(order_payload: (&'static str, i32)) -> Vec<Route> {
    vec![
        Route {
            opening: "server.post('/gadgets'",
            method: "POST",
            path: "/gadgets",
            emission: None,
            payload: Some(("payload", line_of("const payload: GadgetBody"))),
            response: Some((
                "{ saved: payload.name",
                line_of("reply.send({ saved: payload.name"),
            )),
        },
        Route {
            opening: "server.post('/orders'",
            method: "POST",
            path: "/orders",
            emission: None,
            payload: Some(order_payload),
            response: Some((
                "{ total: payload.quantity }",
                line_of("reply.send({ total: payload.quantity"),
            )),
        },
    ]
}

/// (s1) The payload hint for `/orders` sits in the previous route's handler.
#[test]
fn a_request_hint_in_the_previous_routes_handler_is_not_this_routes_request() {
    let routes = gadget_and_order(("payload", line_of("const payload: GadgetBody")));
    let Some((inferred, declared)) =
        stored(&routes, InferKind::RequestBody, "server.post('/orders'")
    else {
        return;
    };
    let both = format!("{inferred} | {declared}");
    assert!(
        !both.contains("weightGrams"),
        "the previous route's request type was stored: {both}"
    );
    assert!(
        both.contains("unknown"),
        "the request should be unknown: {both}"
    );
}

/// Control: the same route with its own hint line keeps its own type.
#[test]
fn a_request_hint_inside_its_own_handler_keeps_the_request_type() {
    let routes = gadget_and_order(("payload", line_of("const payload: OrderBody")));
    let Some((inferred, declared)) =
        stored(&routes, InferKind::RequestBody, "server.post('/orders'")
    else {
        return;
    };
    let both = format!("{inferred} | {declared}");
    assert!(both.contains("quantity"), "own request type lost: {both}");
}

fn slots_route(response: (&'static str, i32)) -> Vec<Route> {
    vec![Route {
        opening: "server.get('/slots'",
        method: "GET",
        path: "/slots",
        emission: Some(EmissionStyle::ReturnValue),
        payload: None,
        response: Some(response),
    }]
}

/// (s2) The response hint sits inside a `.map` callback of the handler: the
/// callback's return is not the route's response.
#[test]
fn a_response_hint_inside_a_nested_callback_is_not_the_handlers_return() {
    let routes = slots_route(("{ id, label: 'slot' }", line_of("ids.map((id)")));
    let Some((inferred, declared)) =
        stored(&routes, InferKind::FunctionReturn, "server.get('/slots'")
    else {
        return;
    };
    let both = format!("{inferred} | {declared}");
    assert!(
        !both.contains("label"),
        "the map callback's return was stored as the response: {both}"
    );
}

/// Control: a hint at the handler's own return keeps the handler's return.
#[test]
fn a_response_hint_at_the_handlers_own_return_keeps_the_return_type() {
    let routes = slots_route((
        "{ count: labels.length, labels }",
        line_of("return { count: labels.length"),
    ));
    let Some((inferred, declared)) =
        stored(&routes, InferKind::FunctionReturn, "server.get('/slots'")
    else {
        return;
    };
    let both = format!("{inferred} | {declared}");
    assert!(both.contains("count"), "own return type lost: {both}");
}

/// Control: a handler that returns a wrapper's call has its callback on its
/// own return path; that callback's return is the response.
#[test]
fn a_response_hint_in_a_returned_wrappers_callback_keeps_the_return_type() {
    let routes = vec![Route {
        opening: "server.get('/wrapped'",
        method: "GET",
        path: "/wrapped",
        emission: Some(EmissionStyle::ReturnValue),
        payload: None,
        response: Some((
            "{ ok: true, attempts: 1 }",
            line_of("return { ok: true, attempts: 1 }"),
        )),
    }];
    let Some((inferred, declared)) =
        stored(&routes, InferKind::FunctionReturn, "server.get('/wrapped'")
    else {
        return;
    };
    let both = format!("{inferred} | {declared}");
    assert!(both.contains("attempts"), "wrapped return lost: {both}");
}

/// The scanner sends the lines of an inline handler with each producer
/// request it locates by text, and none for a request it locates another way.
#[test]
fn a_text_located_route_request_carries_its_inline_handlers_lines() {
    let temp = TempDir::new().expect("temp dir");
    let repo = temp.path().join("api");
    write_fixture_repo(&repo);
    let routes = gadget_and_order(("payload", line_of("const payload: OrderBody")));
    let items = collect(&repo, &routes);
    let orders = items
        .iter()
        .find(|item| {
            item.infer_kind == InferKind::RequestBody
                && item.line_number as i32 == line_of("server.post('/orders'")
        })
        .expect("orders request");
    let lines = orders.handler_lines.expect("handler lines on the request");
    assert_eq!(lines.start_line as i32, line_of("server.post('/orders'"));
    assert_eq!(
        lines.end_line as i32,
        line_of("reply.send({ total: payload.quantity }") + 1
    );
}
