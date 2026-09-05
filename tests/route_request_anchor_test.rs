//! Live-path coverage for the request contract of a schema-first route
//! (carrick#534).
//!
//! carrick#533 added two route-contract anchors to the sidecar — the handler's
//! parameter annotation and the registration's declared schema — and proved
//! them with SPAN locators. The live scan does not send a span for the request
//! side. `FileOrchestrator::collect_type_requests` tries the analyzer's
//! `payload_expression_text` FIRST and only falls back to the registration span
//! when no expression was reported, so on a route where the analyzer reports
//! one the sidecar gets a TEXT locator pointing inside the handler and never
//! reaches the registration the anchors run on.
//!
//! On a forwarding schema-first route the only request-shaped text in the file
//! is the handler's request OBJECT, whose type is the framework's request
//! machinery, or the forwarded controller call, whose type is the RESPONSE.
//! Both resolve to something, so the located expression won and the declared
//! contract was bypassed; in the cross-repo surface the machinery type decays
//! to `any` and every pair verdict against it is unverifiable.
//!
//! These tests drive the exact live path end to end: the Rust locator emission
//! from an analyzer result that carries a payload expression, then the real
//! sidecar over those same request items.

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

/// A schema-first service in the shape the live terrain uses: the route
/// declares its request in two places it never evaluates (the handler's
/// parameter annotation and the registration's `schema.body`) and forwards to a
/// controller. Written with local declarations only, so no install is needed.
const ROUTES_TS: &str = r#"import { RouteReply, Server } from './framework';
import { CreateWidgetRequest, widgetSchemas } from './widgets.schema';
import { createWidget } from './widgets.controller';

export function registerWidgetRoutes(server: Server) {
  server.post(
    '/widgets',
    {
      schema: {
        body: widgetSchemas.CreateWidget,
        response: {
          200: widgetSchemas.WidgetView,
        },
      },
    },
    async (request: CreateWidgetRequest, reply: RouteReply) =>
      createWidget(server, request, reply),
  );
}
"#;

/// 1-based line of `server.post(` in [`ROUTES_TS`] — the registration line the
/// scanner reports as the endpoint's `line_number`.
const REGISTRATION_LINE: i32 = 6;
/// 1-based line of the handler's parameter list, where a payload expression
/// naming the request object lands.
const HANDLER_LINE: i32 = 15;
/// 1-based line of the forwarded controller call.
const FORWARD_LINE: i32 = 16;

const ALIAS: &str = "Endpoint_widgets_Request";

fn write_fixture_repo(repo: &Path) {
    fs::create_dir_all(repo.join("src")).expect("create src");
    fs::write(
        repo.join("package.json"),
        r#"{ "name": "schema-first-api", "version": "1.0.0" }"#,
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
    // The framework surface: a request object exposing its parsed `body`, and a
    // registration taking route options alongside the handler.
    fs::write(
        repo.join("src/framework.ts"),
        r#"export interface RouteRequest<TBody> {
  body: TBody;
  headers: Record<string, string>;
  routerPath: string;
}

export interface RouteReply {
  send(payload: unknown): void;
}

export interface RouteOptions {
  schema?: {
    body?: unknown;
    response?: Record<string, unknown>;
  };
}

export interface Server {
  post(
    path: string,
    options: RouteOptions,
    handler: (request: never, reply: RouteReply) => unknown,
  ): void;
}
"#,
    )
    .unwrap();
    fs::write(
        repo.join("src/widgets.schema.ts"),
        r#"import { RouteRequest } from './framework';

export interface CreateWidgetBody {
  name: string;
  sizeCm: number;
}

export interface WidgetView {
  id: string;
  name: string;
  sizeCm: number;
  createdAt: string;
}

export const widgetSchemas = {
  CreateWidget: {
    parse: (input: unknown): CreateWidgetBody => input as CreateWidgetBody,
  },
  WidgetView: {
    parse: (input: unknown): WidgetView => input as WidgetView,
  },
};

export type CreateWidgetRequest = RouteRequest<CreateWidgetBody>;
"#,
    )
    .unwrap();
    fs::write(
        repo.join("src/widgets.controller.ts"),
        r#"import { RouteReply, Server } from './framework';
import { CreateWidgetRequest, WidgetView } from './widgets.schema';

export function createWidget(
  server: Server,
  request: CreateWidgetRequest,
  reply: RouteReply,
): WidgetView {
  const view: WidgetView = {
    id: 'w1',
    name: request.body.name,
    sizeCm: request.body.sizeCm,
    createdAt: '2020-01-01',
  };
  reply.send(view);
  return view;
}
"#,
    )
    .unwrap();
    fs::write(repo.join("src/widgets.routes.ts"), ROUTES_TS).unwrap();
}

/// The analyzer result for the route, with the payload expression the model
/// reported. `payload_expression_text` is the only field that varies between
/// the live shapes under test.
fn analyzer_result(
    routes_file: &str,
    payload_expression_text: &str,
    payload_expression_line: i32,
) -> HashMap<String, FileAnalysisResult> {
    let registration_span = ROUTES_TS
        .find("server.post(")
        .expect("fixture must register the route") as u32;
    let mut depth = 0usize;
    let mut span_end = registration_span;
    for (offset, ch) in ROUTES_TS[registration_span as usize..].char_indices() {
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                span_end = registration_span + offset as u32 + 1;
                break;
            }
        }
    }

    let endpoint = EndpointResult {
        candidate_id: format!("span:{}-{}", registration_span, span_end),
        line_number: REGISTRATION_LINE,
        owner_node: "server".to_string(),
        method: "POST".to_string(),
        path: "/widgets".to_string(),
        handler_name: "anonymous".to_string(),
        pattern_matched: ".post(".to_string(),
        call_expression_span_start: Some(registration_span),
        call_expression_span_end: Some(span_end),
        payload_expression_text: Some(payload_expression_text.to_string()),
        payload_expression_line: Some(payload_expression_line),
        response_expression_text: Some("createWidget(server, request, reply)".to_string()),
        response_expression_line: Some(FORWARD_LINE),
        emission_style: None,
        primary_type_symbol: None,
        type_import_source: None,
        resolution_source: None,
    };

    let mut file_results = HashMap::new();
    file_results.insert(
        routes_file.to_string(),
        FileAnalysisResult {
            graphql_consumer_locates: vec![],
            mounts: vec![],
            endpoints: vec![endpoint],
            data_calls: vec![],
            graphql_operations: vec![],
            pubsub_operations: vec![],
        },
    );
    file_results
}

/// Run the scanner's own locator emission, then assert it produced the live
/// request-side shape (a text locator, no span) and return it.
fn live_request_infer_item(
    repo: &Path,
    payload_expression_text: &str,
    payload_expression_line: i32,
) -> InferRequestItem {
    let routes_file = repo
        .join("src/widgets.routes.ts")
        .to_string_lossy()
        .to_string();
    let file_results = analyzer_result(
        &routes_file,
        payload_expression_text,
        payload_expression_line,
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
    );

    let item = infer
        .into_iter()
        .find(|item| item.infer_kind == InferKind::RequestBody)
        .expect("a POST endpoint must request request-body inference");

    // The live shape this issue is about: the analyzer's payload expression is
    // tried BEFORE the registration span, so the sidecar is handed a text
    // locator pointing inside the handler and the registration span is never
    // sent. Locking it here keeps the sidecar-side assertions below honest
    // about which locator they are exercising.
    assert_eq!(
        item.expression_text.as_deref(),
        Some(payload_expression_text),
        "the analyzer's payload expression must be the locator: {:?}",
        item
    );
    assert!(
        item.span_start.is_none() && item.span_end.is_none(),
        "an expression locator must suppress the registration span: {:?}",
        item
    );
    assert_eq!(item.line_number, REGISTRATION_LINE as u32);
    item
}

fn is_node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn sidecar_path() -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    path.exists().then_some(path)
}

/// Resolve the request type through the real sidecar for one live locator
/// shape, or `None` when the sidecar is not available to run.
fn resolve_request_type(
    payload_expression_text: &str,
    payload_expression_line: i32,
) -> Option<String> {
    if !is_node_available() {
        eprintln!("Skipping test: Node.js not available");
        return None;
    }
    let Some(sidecar_path) = sidecar_path() else {
        eprintln!("Skipping test: sidecar not built (run: cd src/sidecar && npm run build)");
        return None;
    };

    let temp_dir = TempDir::new().expect("temp dir");
    let repo = temp_dir.path().join("api");
    write_fixture_repo(&repo);

    let mut item = live_request_infer_item(&repo, payload_expression_text, payload_expression_line);
    item.alias = Some(ALIAS.to_string());

    let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
    sidecar.start_init(&repo, None);
    sidecar
        .wait_ready(Duration::from_secs(60))
        .expect("sidecar init");

    let result = sidecar
        .resolve_all_types(&[], &[item], None)
        .expect("resolve_all_types");
    let inferred = result
        .inferred_types
        .into_iter()
        .find(|t| t.alias == ALIAS)
        .expect("expected an inferred request type for the route");
    Some(inferred.type_string)
}

/// Assertions shared by every live locator shape: the resolved request is the
/// DECLARED body, not the request object and not the response.
fn assert_is_declared_request_body(type_string: &str) {
    assert_ne!(type_string.trim(), "any", "request must not resolve to any");
    assert!(
        type_string.contains("name: string"),
        "request must carry the declared body members, got: {type_string}"
    );
    assert!(
        type_string.contains("sizeCm: number"),
        "request must carry the declared body members, got: {type_string}"
    );
    // The request OBJECT's own members: resolving these means the located
    // expression won and the route's declared contract was bypassed.
    assert!(
        !type_string.contains("headers"),
        "request must be the declared body, not the request object, got: {type_string}"
    );
    assert!(
        !type_string.contains("routerPath"),
        "request must be the declared body, not the request object, got: {type_string}"
    );
    // A response-only member: resolving it means the forwarded controller
    // call's type was captured as the request contract.
    assert!(
        !type_string.contains("createdAt"),
        "request must be the declared body, not the response, got: {type_string}"
    );
}

/// The live shape from the reference terrain: the analyzer reports the handler's
/// request object as the payload expression. Before the fix the located
/// expression's own type — the request machinery, generic in the body — was
/// published as the contract and decayed to `any` in the cross-repo surface.
#[test]
fn test_live_text_locator_on_request_object_resolves_declared_request_body() {
    let Some(type_string) = resolve_request_type("request", HANDLER_LINE) else {
        return;
    };
    assert_is_declared_request_body(&type_string);
}

/// The other expression a forwarding route offers: the controller call. Its
/// type is the RESPONSE, which read as the request contract manufactures a
/// mismatch against every consumer rather than an honest unknown.
#[test]
fn test_live_text_locator_on_forwarded_call_resolves_declared_request_body() {
    let Some(type_string) =
        resolve_request_type("createWidget(server, request, reply)", FORWARD_LINE)
    else {
        return;
    };
    assert_is_declared_request_body(&type_string);
}

/// Fixture control. A payload expression naming the body read directly
/// resolved the contract before this change through the expression path, and
/// resolves the same contract after it through the declared anchor. Reverting
/// the source change leaves this case green while the two above fail, which is
/// what shows the two above are measuring the anchors rather than the fixture.
/// The expression path itself stays covered by #533's route that declares
/// neither anchor.
#[test]
fn test_live_text_locator_on_body_read_resolves_declared_request_body() {
    let Some(type_string) = resolve_request_type("request.body", FORWARD_LINE) else {
        return;
    };
    assert_is_declared_request_body(&type_string);
}
