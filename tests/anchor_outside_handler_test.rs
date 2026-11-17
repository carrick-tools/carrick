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

const ROUTES_TS: &str = r#"import { Server, RouteReply, RouteRequest, Store } from './framework';
import {
  DigestBody,
  DraftBody,
  GadgetBody,
  NoteBody,
  InvoiceRow,
  OrderBody,
  SummaryBody,
  TallyBody,
  TicketBody,
} from './shapes';
import { gated } from './gate';
import { listReceipts } from './receipts';

function listInvoices(request: RouteRequest<unknown>) {
  const rows = [{ invoiceNo: 'n1', cents: 100 }];
  return { invoices: rows, pages: 1 };
}

async function listCredits(request: RouteRequest<unknown>) {
  const creditRows = [{ creditNo: 'c1', cents: 5 }];
  return { credits: creditRows };
}

const saveDraft = async (request: RouteRequest<unknown>, reply: RouteReply) => {
  const draft = await request.json<DraftBody>();
  return { stored: draft.draftTitle };
};

function issueInvoice(request: RouteRequest<unknown>, reply: RouteReply) {
  const invoice: InvoiceRow = { invoiceRef: 'i1', amountDue: 10 };
  return reply.status(201).send(invoice);
}

function sayPlain(reply: RouteReply, words: string) {
  return reply.text(words);
}

const saveNote = async (request: RouteRequest<unknown>, reply: RouteReply) => {
  const note = request.body as NoteBody;
  return { kept: note.noteText };
};

function sayPage(reply: RouteReply, words: string) {
  return reply.page(words);
}

export function registerRoutes(server: Server, ids: string[], store: Store, legacy: any) {
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

  server.get('/invoices', (request: RouteRequest<unknown>, reply: RouteReply) => listInvoices(request));

  server.get('/credits', async (request: RouteRequest<unknown>, reply: RouteReply) => {
    return await listCredits(request);
  });

  server.get('/receipts', async (request: RouteRequest<unknown>, reply: RouteReply) => {
    return await listReceipts(request);
  });

  server.post('/checkout', (request: RouteRequest<unknown>, reply: RouteReply) =>
    store.transaction(async (tx) => {
      tx.insert('sku-1');
      return { receiptId: 'r1', settled: true };
    }));

  server.get('/tags', (request: RouteRequest<unknown>, reply: RouteReply) =>
    ids.map((id) => ({ tag: id, weight: 1 })));

  server.get('/legacy', (request: RouteRequest<unknown>, reply: RouteReply) =>
    legacy.map((row: unknown) => ({ legacyRow: 1 })));

  server.get('/late', // the late list
    async (request: RouteRequest<unknown>, reply: RouteReply) => {
      return { lateCount: 2 };
    });

  server.post('/drafts', (request: RouteRequest<unknown>, reply: RouteReply) => saveDraft(request, reply));

  server.post('/bills', (request: RouteRequest<unknown>, reply: RouteReply) => issueInvoice(request, reply));

  server.patch('/bills', (request: RouteRequest<unknown>, reply: RouteReply) => issueInvoice(request, reply));

  server.get('/summary', async (request: RouteRequest<unknown>, reply: RouteReply) => {
    const response: SummaryBody = { openCount: 1 };
    response.openCount += 1;
    return response;
  });

  server.get('/digest', async (request: RouteRequest<unknown>, reply: RouteReply) => {
    const response: DigestBody = { digestLines: [] };
    return response;
  });

  server.post('/tickets', async (request: RouteRequest<unknown>, reply: RouteReply) => {
    const ticket = request.body as TicketBody;
    reply.send({ filed: ticket.ticketTitle });
  });
  server.post('/tickets/bulk', async (request: RouteRequest<unknown>, reply: RouteReply) => {
    const ticket = request.body as TicketBody;
    reply.send({ filedMany: ticket.ticketTitle });
  });

  server.get('/ping', (request: RouteRequest<unknown>, reply: RouteReply) => { sayPlain(reply, 'ping'); });

  server.get('/pong', (request: RouteRequest<unknown>, reply: RouteReply) => { sayPlain(reply, 'pong'); });

  server.post('/touch', (request: RouteRequest<unknown>, reply: RouteReply) => { reply.send({ touched: true }); });

  server.post('/flag', (request: RouteRequest<unknown>, reply: RouteReply) => { reply.send({ payload: true }); });

  server.post('/drafts/inline', async (request: RouteRequest<unknown>, reply: RouteReply) => {
    const inlineDraft = await request.json<DraftBody>();
    return { stored: inlineDraft.draftTitle };
  });

  server.post('/notes', (request: RouteRequest<unknown>, reply: RouteReply) => saveNote(request, reply));

  server.get('/hello', (request: RouteRequest<unknown>, reply: RouteReply) => sayPage(reply, 'hello'));

  server.get('/bye', (request: RouteRequest<unknown>, reply: RouteReply) => sayPage(reply, 'bye'));

  server.get('/left', async (request: RouteRequest<unknown>, reply: RouteReply) => {
    const row: TallyBody = { tallied: 1 };
    reply.send(row);
  });

  server.get('/right', async (request: RouteRequest<unknown>, reply: RouteReply) => {
    if (ids.length > 0) {
      const row: TallyBody = { tallied: 2 };
      reply.send(row);
      return;
    }
    const row: DigestBody = { digestLines: [] };
    reply.send(row);
  });
}

export class AuditRoutes {
  constructor(private server: Server) {}

  private wrap<T>(run: () => Promise<T>): Promise<T> {
    return run();
  }

  register() {
    this.server.get('/audit', (request: RouteRequest<unknown>, reply: RouteReply) =>
      this.wrap(async () => {
        return { auditId: 7, actor: 'system' };
      }));
  }
}
"#;

const RECEIPTS_TS: &str = r#"import { RouteRequest } from './framework';

// Receipts are read from the ledger, one page only. The return below sits on
// the line of an expression outside every handler in routes.ts.
export async function listReceipts(request: RouteRequest<unknown>) {
  const receiptTotal = 3;
  return { receipts: ['r1'], receiptTotal };
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
  json<T>(): Promise<T>;
}

export interface RouteReply {
  send(payload: unknown): void;
  status(code: number): RouteReply;
  text(words: string): string;
  page(words: string): Response;
}

export interface Tx {
  insert(sku: string): void;
}

export interface Store {
  transaction<T>(run: (tx: Tx) => Promise<T>): Promise<T>;
}

export interface Server {
  post(path: string, handler: (request: never, reply: RouteReply) => unknown): void;
  get(path: string, handler: (request: never, reply: RouteReply) => unknown): void;
  patch(path: string, handler: (request: never, reply: RouteReply) => unknown): void;
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

export interface DraftBody {
  draftTitle: string;
  pages: number;
}

export interface InvoiceRow {
  invoiceRef: string;
  amountDue: number;
}

export interface SummaryBody {
  openCount: number;
}

export interface DigestBody {
  digestLines: string[];
}

export interface TicketBody {
  ticketTitle: string;
  severity: number;
}

export interface NoteBody {
  noteText: string;
}

export interface TallyBody {
  tallied: number;
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
    fs::write(repo.join("src/receipts.ts"), RECEIPTS_TS).unwrap();
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

/// What the scan stores for the one request of `kind` the routes collect.
struct Stored {
    /// The inferred type's text (`<none>` where the inferrer answered none).
    inferred: String,
    /// Whether the inferrer read a type the source states (the row's
    /// `explicit` label); `None` where it answered none.
    explicit: Option<bool>,
    /// The declaration the bundle writes for the alias (`unknown` where the
    /// request abstained).
    declared: String,
}

impl Stored {
    fn both(&self) -> String {
        format!("{} | {}", self.inferred, self.declared)
    }
}

/// What the scan stores for the one request of `kind` the routes collect: the
/// inferred type's text, and the declaration the bundle writes for its alias
/// (`unknown` where the request abstained). `None` when no sidecar is built.
fn stored(routes: &[Route], kind: InferKind, opening: &str) -> Option<(String, String)> {
    stored_row(routes, kind, opening).map(|row| (row.inferred, row.declared))
}

fn stored_row(routes: &[Route], kind: InferKind, opening: &str) -> Option<Stored> {
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
    let inferred = result.inferred_types.iter().find(|t| t.alias == alias);
    let declared = result
        .dts_content
        .as_deref()
        .and_then(|dts| {
            dts.lines()
                .find(|line| line.contains(&format!("type {alias}")))
        })
        .unwrap_or("<no declaration>")
        .to_string();
    let row = Stored {
        inferred: inferred
            .map(|t| t.type_string.clone())
            .unwrap_or_else(|| "<none>".to_string()),
        explicit: inferred.map(|t| t.is_explicit),
        declared,
    };
    eprintln!(
        "{opening} {kind:?}: inferred={} explicit={:?} declared={}",
        row.inferred, row.explicit, row.declared
    );
    Some(row)
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
/// Its text, `payload`, names one value inside `/orders`' own handler (the
/// local and its read; a member name or key would not count), so that value
/// is the request, with the label the same hint carries inside the handler:
/// never the previous route's type.
#[test]
fn a_request_hint_in_the_previous_routes_handler_is_not_this_routes_request() {
    let stale = gadget_and_order(("payload", line_of("const payload: GadgetBody")));
    let own = gadget_and_order(("payload", line_of("const payload: OrderBody")));
    let (Some(row), Some(control)) = (
        stored_row(&stale, InferKind::RequestBody, "server.post('/orders'"),
        stored_row(&own, InferKind::RequestBody, "server.post('/orders'"),
    ) else {
        return;
    };
    assert!(
        !row.both().contains("weightGrams"),
        "the previous route's request type was stored: {}",
        row.both()
    );
    assert!(
        row.both().contains("quantity"),
        "the text's one value in the route's own handler is its request: {}",
        row.both()
    );
    assert_eq!(
        row.explicit, control.explicit,
        "label differs from the own-line hint"
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

/// One `ReturnValue` route whose response hint is `hint` at `line`.
fn returning_route(
    opening: &'static str,
    path: &'static str,
    hint: &'static str,
    line: i32,
) -> Vec<Route> {
    vec![Route {
        opening,
        method: "GET",
        path,
        emission: Some(EmissionStyle::ReturnValue),
        payload: None,
        response: Some((hint, line)),
    }]
}

fn stored_return(routes: &[Route], opening: &str) -> Option<String> {
    stored(routes, InferKind::FunctionReturn, opening).map(|(inferred, declared)| {
        eprintln!("{opening}: inferred={inferred} declared={declared}");
        format!("{inferred} | {declared}")
    })
}

/// (A) A thin handler returns a helper's call: the hint is the helper's
/// return, outside the handler's lines, and the helper's return is the
/// handler's own.
#[test]
fn a_response_hint_in_the_helper_the_handler_returns_keeps_the_return_type() {
    let routes = returning_route(
        "server.get('/invoices'",
        "/invoices",
        "{ invoices: rows, pages: 1 }",
        line_of("return { invoices: rows, pages: 1 }"),
    );
    let Some(both) = stored_return(&routes, "server.get('/invoices'") else {
        return;
    };
    assert!(both.contains("invoiceNo"), "helper's return lost: {both}");
}

/// (A, another file) The handler awaits a helper declared in another file;
/// the hint is the helper's return at its line in that file.
#[test]
fn a_response_hint_in_a_helper_in_another_file_keeps_the_return_type() {
    let helper_line = RECEIPTS_TS
        .lines()
        .position(|line| line.contains("return { receipts"))
        .expect("helper return") as i32
        + 1;
    let routes = returning_route(
        "server.get('/receipts'",
        "/receipts",
        "{ receipts: ['r1'], receiptTotal }",
        helper_line,
    );
    let Some(both) = stored_return(&routes, "server.get('/receipts'") else {
        return;
    };
    assert!(
        both.contains("receiptTotal"),
        "other-file helper's return lost: {both}"
    );
}

/// (B) The handler returns a member call whose callback's return is the
/// call's result (a transaction).
#[test]
fn a_response_hint_in_a_returned_transaction_callback_keeps_the_return_type() {
    let routes = returning_route(
        "server.post('/checkout'",
        "/checkout",
        "{ receiptId: 'r1', settled: true }",
        line_of("return { receiptId: 'r1'"),
    );
    let routes: Vec<Route> = routes
        .into_iter()
        .map(|route| Route {
            method: "POST",
            ..route
        })
        .collect();
    let Some(both) = stored_return(&routes, "server.post('/checkout'") else {
        return;
    };
    assert!(
        both.contains("settled"),
        "transaction callback's return lost: {both}"
    );
}

/// (B, `this`) The handler returns `this.wrap(callback)`.
#[test]
fn a_response_hint_in_a_returned_method_wrappers_callback_keeps_the_return_type() {
    let routes = returning_route(
        "this.server.get('/audit'",
        "/audit",
        "{ auditId: 7, actor: 'system' }",
        line_of("return { auditId: 7"),
    );
    let Some(both) = stored_return(&routes, "this.server.get('/audit'") else {
        return;
    };
    assert!(
        both.contains("auditId"),
        "method wrapper's callback return lost: {both}"
    );
}

/// The handler returns `ids.map(callback)`: the call's result is a list of
/// the callback's return, so the callback's return is a part, never the
/// response.
#[test]
fn a_response_hint_in_a_returned_map_callback_is_not_the_handlers_return() {
    let routes = returning_route(
        "server.get('/tags'",
        "/tags",
        "({ tag: id, weight: 1 })",
        line_of("ids.map((id) => ({ tag: id"),
    );
    let Some(both) = stored_return(&routes, "server.get('/tags'") else {
        return;
    };
    assert!(
        !both.contains("weight"),
        "the map callback's return was stored as the response: {both}"
    );
}

/// A handler whose literal opens on the line after its registration, behind
/// a comment and `async`, keeps its own return.
#[test]
fn a_response_hint_in_a_handler_opening_after_a_comment_keeps_the_return_type() {
    let routes = returning_route(
        "server.get('/late'",
        "/late",
        "{ lateCount: 2 }",
        line_of("return { lateCount: 2 }"),
    );
    let Some(both) = stored_return(&routes, "server.get('/late'") else {
        return;
    };
    assert!(both.contains("lateCount"), "own return lost: {both}");
}

/// The stamped lines of a literal that opens behind a comment and `async`
/// on the line after its registration are the literal's own lines, the ones
/// the sidecar matches a function on.
#[test]
fn a_handler_opening_after_a_comment_is_stamped_with_the_literals_lines() {
    let temp = TempDir::new().expect("temp dir");
    let repo = temp.path().join("api");
    write_fixture_repo(&repo);
    let routes = returning_route(
        "server.get('/late'",
        "/late",
        "{ lateCount: 2 }",
        line_of("return { lateCount: 2 }"),
    );
    let items = collect(&repo, &routes);
    let late = items
        .iter()
        .find(|item| item.infer_kind == InferKind::FunctionReturn)
        .expect("late request");
    let lines = late.handler_lines.expect("handler lines on the request");
    assert_eq!(lines.start_line as i32, line_of("server.get('/late'") + 1);
    assert_eq!(
        lines.end_line as i32,
        line_of("return { lateCount: 2 }") + 1
    );
}

/// The handler returns an untyped receiver's `.map(callback)`: the call's
/// type is `any`, which every type relates to, so it is no evidence that the
/// callback's return is the response.
#[test]
fn a_response_hint_in_a_callback_of_an_untyped_call_is_not_the_handlers_return() {
    let routes = returning_route(
        "server.get('/legacy'",
        "/legacy",
        "({ legacyRow: 1 })",
        line_of("legacy.map((row: unknown)"),
    );
    let Some(both) = stored_return(&routes, "server.get('/legacy'") else {
        return;
    };
    assert!(
        !both.contains("legacyRow"),
        "an untyped call's callback return was stored as the response: {both}"
    );
}

/// (A, awaited) The handler's block body returns an awaited helper's call.
#[test]
fn a_response_hint_in_an_awaited_helper_the_handler_returns_keeps_the_return_type() {
    let routes = returning_route(
        "server.get('/credits'",
        "/credits",
        "{ credits: creditRows }",
        line_of("return { credits: creditRows }"),
    );
    let Some(both) = stored_return(&routes, "server.get('/credits'") else {
        return;
    };
    assert!(
        both.contains("creditNo"),
        "awaited helper's return lost: {both}"
    );
}

/// A return hint that sits in the previous route's handler is that
/// handler's return, not this route's.
#[test]
fn a_response_hint_in_the_previous_routes_return_is_not_this_routes_return() {
    let routes = returning_route(
        "server.get('/wrapped'",
        "/wrapped",
        "{ count: labels.length, labels }",
        line_of("return { count: labels.length"),
    );
    let Some(both) = stored_return(&routes, "server.get('/wrapped'") else {
        return;
    };
    assert!(
        !both.contains("count"),
        "the previous route's return was stored: {both}"
    );
}

/// One route row: `payload` and `response` hints as the model stated them.
fn route(
    opening: &'static str,
    method: &'static str,
    path: &'static str,
    emission: Option<EmissionStyle>,
    payload: Option<(&'static str, i32)>,
    response: Option<(&'static str, i32)>,
) -> Route {
    Route {
        opening,
        method,
        path,
        emission,
        payload,
        response,
    }
}

/// (1) A thin arrow returns a helper declared above it, and the request body
/// is read inside that helper: the helper is on the handler's return path, so
/// its read is this route's request.
#[test]
fn a_request_read_in_the_helper_the_handler_returns_keeps_the_request_type() {
    let routes = vec![route(
        "server.post('/drafts'",
        "POST",
        "/drafts",
        None,
        Some((
            "request.json<DraftBody>()",
            line_of("const draft = await request.json<DraftBody>()"),
        )),
        None,
    )];
    let Some(row) = stored_row(&routes, InferKind::RequestBody, "server.post('/drafts'") else {
        return;
    };
    assert!(
        row.both().contains("draftTitle") || row.both().contains("DraftBody"),
        "the helper's request read was dropped: {}",
        row.both()
    );
}

/// (2) Two routes return one module-level helper whose own return is a
/// reply-chain send: the sent value is each route's response.
#[test]
fn a_send_in_the_helper_two_routes_return_is_each_routes_response() {
    let hint = ("invoice", line_of("return reply.status(201).send(invoice)"));
    let routes = vec![
        route(
            "server.post('/bills'",
            "POST",
            "/bills",
            None,
            None,
            Some(hint),
        ),
        route(
            "server.patch('/bills'",
            "PATCH",
            "/bills",
            None,
            None,
            Some(hint),
        ),
    ];
    for opening in ["server.post('/bills'", "server.patch('/bills'"] {
        let Some(row) = stored_row(&routes, InferKind::ResponseBody, opening) else {
            return;
        };
        assert!(
            row.both().contains("amountDue") || row.both().contains("InvoiceRow"),
            "{opening}: the helper's sent response was dropped: {}",
            row.both()
        );
    }
}

/// (3) The handler returns a typed local; the text locator lands on the same
/// text in the next route. The text occurs once (as a read) inside this
/// route's handler, so that node is read, never the next route's.
#[test]
fn a_return_text_found_in_a_later_route_is_read_in_its_own_handler() {
    let routes = vec![route(
        "server.get('/summary'",
        "GET",
        "/summary",
        Some(EmissionStyle::ReturnValue),
        None,
        Some(("response", line_of("return response") + 1)),
    )];
    let Some(row) = stored_row(&routes, InferKind::FunctionReturn, "server.get('/summary'") else {
        return;
    };
    assert!(
        !row.both().contains("digestLines") && !row.both().contains("DigestBody"),
        "the later route's return was stored: {}",
        row.both()
    );
    assert!(
        row.both().contains("openCount") || row.both().contains("SummaryBody"),
        "the handler's own return was dropped: {}",
        row.both()
    );
}

/// (4) Two adjacent routes read the same request text. The hint line is
/// nearer the first route's read, so the inferrer's locator found that one
/// and the inferrer answered none, while the capture found this route's: the
/// type text survived on the capture alone and lost its `explicit` label.
/// The inferrer now reads the one match inside this route's handler.
#[test]
fn a_request_text_nearer_the_previous_route_keeps_its_explicit_label() {
    let routes = vec![
        route(
            "server.post('/tickets'",
            "POST",
            "/tickets",
            None,
            Some((
                "request.body as TicketBody",
                line_of("const ticket = request.body as TicketBody"),
            )),
            None,
        ),
        route(
            "server.post('/tickets/bulk'",
            "POST",
            "/tickets/bulk",
            None,
            Some((
                "request.body as TicketBody",
                line_of("reply.send({ filed: ticket.ticketTitle })"),
            )),
            None,
        ),
    ];
    let Some(row) = stored_row(
        &routes,
        InferKind::RequestBody,
        "server.post('/tickets/bulk'",
    ) else {
        return;
    };
    assert!(
        row.both().contains("ticketTitle") || row.both().contains("TicketBody"),
        "the request type was dropped: {}",
        row.both()
    );
    assert_eq!(
        row.explicit,
        Some(true),
        "the stated request lost its explicit label: {}",
        row.both()
    );
}

/// Must abstain: two one-line arrows call a shared helper that answers with a
/// text reply, and neither returns it. The helper's reply is not either
/// route's response.
#[test]
fn a_text_reply_in_a_helper_the_handler_does_not_return_stays_unknown() {
    let hint = ("reply.text(words)", line_of("return reply.text(words)"));
    let routes = vec![
        route("server.get('/ping'", "GET", "/ping", None, None, Some(hint)),
        route("server.get('/pong'", "GET", "/pong", None, None, Some(hint)),
    ];
    for opening in ["server.get('/ping'", "server.get('/pong'"] {
        let Some(row) = stored_row(&routes, InferKind::ResponseBody, opening) else {
            return;
        };
        assert!(
            !row.both().contains("string"),
            "{opening}: the helper's text reply was stored: {}",
            row.both()
        );
    }
}

/// Must abstain: a handler that reads no body, given another route's read as
/// its request hint.
#[test]
fn a_request_hint_for_a_handler_that_reads_no_body_stays_unknown() {
    let routes = vec![route(
        "server.post('/touch'",
        "POST",
        "/touch",
        None,
        Some(("payload", line_of("const payload: OrderBody"))),
        None,
    )];
    let Some(row) = stored_row(&routes, InferKind::RequestBody, "server.post('/touch'") else {
        return;
    };
    assert!(
        !row.both().contains("quantity") && !row.both().contains("OrderBody"),
        "another route's request was stored: {}",
        row.both()
    );
}

/// The text found outside occurs inside the right handler as two reads of
/// different types: which one the model meant is not decided, so the row
/// abstains rather than pick one.
#[test]
fn a_text_found_outside_that_reads_two_types_inside_the_handler_stays_unknown() {
    let routes = vec![route(
        "server.get('/right'",
        "GET",
        "/right",
        None,
        None,
        Some(("row", line_of("reply.send(row)"))),
    )];
    let Some(row) = stored_row(&routes, InferKind::ResponseBody, "server.get('/right'") else {
        return;
    };
    assert!(
        !row.both().contains("tallied")
            && !row.both().contains("TallyBody")
            && !row.both().contains("digestLines")
            && !row.both().contains("DigestBody"),
        "an ambiguous text was read: {}",
        row.both()
    );
}

/// Must abstain: the hint's text is spelled inside the handler only as an
/// object key, which is not a read of anything.
#[test]
fn a_request_hint_spelled_only_as_a_key_in_the_handler_stays_unknown() {
    let routes = vec![route(
        "server.post('/flag'",
        "POST",
        "/flag",
        None,
        Some(("payload", line_of("const payload: OrderBody"))),
        None,
    )];
    let Some(row) = stored_row(&routes, InferKind::RequestBody, "server.post('/flag'") else {
        return;
    };
    assert!(
        !row.both().contains("boolean") && !row.both().contains("quantity"),
        "a key or another route's read was stored: {}",
        row.both()
    );
}

/// (1, label) A request read in the helper the handler returns carries the
/// label the same read carries inside a handler; a cast read there is
/// explicit.
#[test]
fn a_request_read_in_a_returned_helper_keeps_the_label_of_an_inline_read() {
    let inline = vec![route(
        "server.post('/drafts/inline'",
        "POST",
        "/drafts/inline",
        None,
        Some((
            "request.json<DraftBody>()",
            line_of("const inlineDraft = await request.json<DraftBody>()"),
        )),
        None,
    )];
    let helper = vec![route(
        "server.post('/drafts'",
        "POST",
        "/drafts",
        None,
        Some((
            "request.json<DraftBody>()",
            line_of("const draft = await request.json<DraftBody>()"),
        )),
        None,
    )];
    let cast = vec![route(
        "server.post('/notes'",
        "POST",
        "/notes",
        None,
        Some((
            "request.body as NoteBody",
            line_of("const note = request.body as NoteBody"),
        )),
        None,
    )];
    let (Some(inline), Some(helper), Some(cast)) = (
        stored_row(
            &inline,
            InferKind::RequestBody,
            "server.post('/drafts/inline'",
        ),
        stored_row(&helper, InferKind::RequestBody, "server.post('/drafts'"),
        stored_row(&cast, InferKind::RequestBody, "server.post('/notes'"),
    ) else {
        return;
    };
    assert_eq!(
        helper.explicit,
        inline.explicit,
        "helper read {} against inline read {}",
        helper.both(),
        inline.both()
    );
    assert!(
        cast.both().contains("noteText"),
        "cast read lost: {}",
        cast.both()
    );
    assert_eq!(
        cast.explicit,
        Some(true),
        "cast read in the helper: {}",
        cast.both()
    );
}

/// Two one-line arrows return a helper that answers with a framework page
/// reply (`Response`): the reply is the arrows' answer, and a `Response` is
/// no body contract, so the row stays unknown.
#[test]
fn a_response_reply_a_returned_helper_answers_with_stays_unknown() {
    let hint = ("reply.page(words)", line_of("return reply.page(words)"));
    let routes = vec![
        route(
            "server.get('/hello'",
            "GET",
            "/hello",
            None,
            None,
            Some(hint),
        ),
        route("server.get('/bye'", "GET", "/bye", None, None, Some(hint)),
    ];
    for opening in ["server.get('/hello'", "server.get('/bye'"] {
        let Some(row) = stored_row(&routes, InferKind::ResponseBody, opening) else {
            return;
        };
        assert!(
            row.inferred == "<none>" && row.declared.contains("= unknown;"),
            "{opening}: the helper's page reply was stored: {}",
            row.both()
        );
    }
}
