//! Place each route's handler function on its endpoint row (cloud#948).
//!
//! An endpoint row's location is the line its registration call opens on. For
//! an inline handler behind a validator chain that line is several lines above
//! the function itself, so a reader asking "is this function the handler of a
//! route?" by comparing the function's span with the row's line answers no, and
//! labels the handler a helper in a route file. The row therefore carries the
//! handler's own span, `handler_span`, 1-based and inclusive, in the row's file.
//!
//! The span is read from the registration call itself, never from a name the
//! scan builds. Every endpoint row joined to its call site carries that call's
//! span in `file_results` (`call_expression_span_start`/`_end`); the call's
//! LAST argument is the handler, because a route call passes its handler after
//! any middleware:
//!
//! - **A function literal** (`router.get("/x", auth, async (req, res) => …)`):
//!   the literal's own lines. They are the lines the function index gives the
//!   same literal, so a reader comparing a `function_definitions` row's span
//!   with the row's `handler_span` sees the same numbers.
//! - **An identifier** (`router.get("/x", listX)`): the one definition in the
//!   file with that name. Two same-named definitions, or none (an imported
//!   handler), place nothing.
//! - Anything else (a wrapper call around the handler, an object), and a row
//!   whose call is not found, fall back to the name the model gave its
//!   handler, read as the identifier is. An inline handler the model calls
//!   `anonymous` matches no definition and places nothing.
//!
//! A row with no call span in `file_results` (a receiver-trace or library-fact
//! row, or a model row joined to no call site) is read from the call its own
//! file and line name instead (carrick#3655): a call whose callee is a member
//! named by the row's method (`router.get(`, `app.post<…>(`) and which opens,
//! or whose member sits, on that line. Two such calls on one line read as
//! neither. A registration keyed some other way (`app.route({ … })`,
//! `app.on("GET", …)`, a decorator) is not found by its line.
//!
//! A named definition that opens above the registration line and contains it
//! is the function the route is registered FROM (a plugin), never the handler
//! (carrick#2824), and places nothing.

use crate::agents::file_analyzer_agent::FileAnalysisResult;
use crate::cloud_storage::CloudRepoData;
use crate::mount_graph::HandlerSpan;
use crate::operation::OperationKey;
use crate::visitor::FunctionDefinition;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use swc_common::{FileName, SourceMap, Span, Spanned, errors::Handler, sync::Lrc};
use swc_ecma_ast::{CallExpr, Callee, Expr, MemberProp};
use swc_ecma_visit::{Visit, VisitWith};

/// What a registration call hands in last.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HandedIn {
    /// A function literal, at these lines.
    Literal(HandlerSpan),
    /// A bare identifier.
    Named(String),
    /// Something the span cannot be read from.
    Other,
}

/// A call's span in SWC's numbering, as the candidate scanner records it.
type CallSpan = (u32, u32);

/// A registration named by a row's own place: its line and lower-cased method.
type CallLine = (u32, String);

/// The registration calls one file is asked about.
#[derive(Default)]
struct Wanted {
    spans: HashSet<CallSpan>,
    lines: HashSet<CallLine>,
}

/// What each asked-about call in one file hands in last.
#[derive(Default)]
struct HandedInByCall {
    by_span: HashMap<CallSpan, HandedIn>,
    by_line: HashMap<CallLine, HandedIn>,
}

/// Stamp `handler_span` onto every HTTP endpoint row the payload holds: the
/// mount graph's rows, and the top-level `endpoints[]` rows projected from
/// them. Run after `relativize_cloud_paths`, so a row's file and a
/// definition's file are in the same, repo-relative form. `read` returns a
/// file's text by its repo-relative path. Returns how many mount-graph rows
/// were placed.
pub fn attach_handler_spans(
    data: &mut CloudRepoData,
    read: impl Fn(&str) -> Option<String>,
) -> usize {
    let mut by_file: HashMap<String, Vec<&FunctionDefinition>> = HashMap::new();
    for def in data.function_definitions.values() {
        by_file
            .entry(normalize(&def.file_path.to_string_lossy()))
            .or_default()
            .push(def);
    }

    let calls = registration_calls(data.file_results.as_ref());
    let Some(graph) = data.mount_graph.as_mut() else {
        return 0;
    };

    // A row joined to a call is read at that call's span; any other row at the
    // call its own file and line name.
    let mut wanted: HashMap<String, Wanted> = HashMap::new();
    for endpoint in &graph.endpoints {
        let Some((file, line)) = split_location(&endpoint.file_location) else {
            continue;
        };
        let method = endpoint.method.to_ascii_lowercase();
        let call = calls.get(&(file.clone(), line, method.clone())).copied();
        let asked = wanted.entry(file).or_default();
        match call.flatten() {
            Some(span) => {
                asked.spans.insert(span);
            }
            None => {
                asked.lines.insert((line, method));
            }
        }
    }
    let handed_in: HashMap<String, HandedInByCall> = wanted
        .into_iter()
        .filter_map(|(file, asked)| {
            let text = read(&file)?;
            let found = last_arguments(&file, text, &asked);
            Some((file, found))
        })
        .collect();

    let mut placed_rows: HashMap<(String, OperationKey, Option<String>), HandlerSpan> =
        HashMap::new();
    let mut placed = 0;
    for endpoint in &mut graph.endpoints {
        let Some((file, line)) = split_location(&endpoint.file_location) else {
            continue;
        };
        let defs = by_file.get(&file).map(Vec::as_slice).unwrap_or_default();
        let method = endpoint.method.to_ascii_lowercase();
        let call = calls
            .get(&(file.clone(), line, method.clone()))
            .copied()
            .flatten();
        let found = handed_in.get(&file);
        let last = match call {
            Some(span) => Some(
                found
                    .and_then(|found| found.by_span.get(&span))
                    .cloned()
                    .unwrap_or(HandedIn::Other),
            ),
            None => found.and_then(|found| found.by_line.get(&(line, method)).cloned()),
        };
        let span = place(defs, line, last, endpoint.handler.as_deref());
        endpoint.handler_span = span;
        if let Some(span) = span {
            placed += 1;
            placed_rows.insert(
                (
                    endpoint.file_location.clone(),
                    OperationKey::http(&endpoint.method, endpoint.full_path.clone()),
                    endpoint.handler.clone(),
                ),
                span,
            );
        }
    }

    // The top-level rows are the graph's rows projected
    // (`mount_graph_to_api_details`), keyed by location, operation and handler.
    for op in &mut data.endpoints {
        if !matches!(op.key, OperationKey::Http { .. }) {
            continue;
        }
        let identity = (
            op.file_path.to_string_lossy().to_string(),
            op.key.clone(),
            op.handler_name.clone(),
        );
        op.handler_span = placed_rows.get(&identity).copied();
    }

    placed
}

/// The call span each endpoint row was joined to, keyed by the row's file,
/// line and lower-cased method (the mount graph's row is
/// `file:line_number` of the same result). Two results at one key with
/// different spans read as `None`: which call the graph row is cannot be told.
fn registration_calls(
    file_results: Option<&HashMap<String, FileAnalysisResult>>,
) -> HashMap<(String, u32, String), Option<CallSpan>> {
    let mut calls: HashMap<(String, u32, String), Option<CallSpan>> = HashMap::new();
    for (file, result) in file_results.into_iter().flatten() {
        for endpoint in &result.endpoints {
            let (Some(start), Some(end), Ok(line)) = (
                endpoint.call_expression_span_start,
                endpoint.call_expression_span_end,
                u32::try_from(endpoint.line_number),
            ) else {
                continue;
            };
            // The graph row's method is this one trimmed and upper-cased
            // (`FileOrchestrator`'s mount-graph build), its location
            // `<this key>:<line_number>`.
            let key = (
                normalize(file),
                line,
                endpoint.method.trim().to_ascii_lowercase(),
            );
            calls
                .entry(key)
                .and_modify(|held| {
                    if *held != Some((start, end)) {
                        *held = None;
                    }
                })
                .or_insert(Some((start, end)));
        }
    }
    calls
}

/// What each call `asked` names hands in last, read from the file's text parsed
/// on a fresh source map: the numbering the candidate scanner recorded the
/// spans in (`swc_scanner::SWC_SPAN_BASE`).
fn last_arguments(file: &str, text: String, asked: &Wanted) -> HandedInByCall {
    let source_map: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_emitter_writer(Box::new(std::io::sink()), None);
    let path = Path::new(file);
    let Some(module) = crate::parser::parse_source(
        path,
        FileName::Real(path.to_path_buf()),
        text,
        &source_map,
        &handler,
        None,
    ) else {
        return HandedInByCall::default();
    };
    let mut reader = LastArguments {
        source_map: &source_map,
        asked,
        found: HandedInByCall::default(),
    };
    module.visit_with(&mut reader);
    reader.found
}

struct LastArguments<'a> {
    source_map: &'a SourceMap,
    asked: &'a Wanted,
    found: HandedInByCall,
}

impl LastArguments<'_> {
    fn line(&self, at: swc_common::BytePos) -> u32 {
        self.source_map.lookup_char_pos(at).line as u32
    }

    fn lines(&self, span: Span) -> HandlerSpan {
        HandlerSpan {
            start_line: self.line(span.lo),
            end_line: self.line(span.hi),
        }
    }

    fn handed_in(&self, call: &CallExpr) -> HandedIn {
        let last = call.args.last().filter(|arg| arg.spread.is_none());
        match last.map(|arg| crate::graphql_document_sites::unwrap_expression(&arg.expr)) {
            Some(Expr::Arrow(arrow)) => HandedIn::Literal(self.lines(arrow.span)),
            Some(Expr::Fn(function)) => HandedIn::Literal(self.lines(function.function.span())),
            Some(Expr::Ident(ident)) => HandedIn::Named(ident.sym.to_string()),
            _ => HandedIn::Other,
        }
    }

    /// The asked-about rows this call is the registration of: its callee is a
    /// member named by the row's method, and the call opens, or that member
    /// sits, on the row's line.
    fn asked_lines(&self, call: &CallExpr) -> Vec<CallLine> {
        if self.asked.lines.is_empty() {
            return Vec::new();
        }
        let Callee::Expr(callee) = &call.callee else {
            return Vec::new();
        };
        let Expr::Member(member) = crate::graphql_document_sites::unwrap_expression(callee) else {
            return Vec::new();
        };
        let MemberProp::Ident(property) = &member.prop else {
            return Vec::new();
        };
        let method = property.sym.as_str().to_ascii_lowercase();
        let mut lines = vec![self.line(call.span.lo), self.line(property.span.lo)];
        lines.dedup();
        lines
            .into_iter()
            .map(|line| (line, method.clone()))
            .filter(|key| self.asked.lines.contains(key))
            .collect()
    }
}

impl Visit for LastArguments<'_> {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        let key = (call.span.lo.0, call.span.hi.0);
        if self.asked.spans.contains(&key) {
            let handed_in = self.handed_in(call);
            self.found.by_span.insert(key, handed_in);
        }
        for key in self.asked_lines(call) {
            // A second registration at one line and method: which one the
            // row is cannot be told.
            let handed_in = if self.found.by_line.contains_key(&key) {
                HandedIn::Other
            } else {
                self.handed_in(call)
            };
            self.found.by_line.insert(key, handed_in);
        }
        call.visit_children_with(self);
    }
}

/// The span of the handler registered at `line`, or `None` when it cannot be
/// placed in this file. `last` is what the registration call hands in last,
/// `None` when the row was joined to no call; `handler` is the model's name,
/// read only when the call states no literal and no identifier.
fn place(
    defs: &[&FunctionDefinition],
    line: u32,
    last: Option<HandedIn>,
    handler: Option<&str>,
) -> Option<HandlerSpan> {
    let name = match last {
        Some(HandedIn::Literal(span)) => return valid(span),
        Some(HandedIn::Named(name)) => name,
        Some(HandedIn::Other) | None => handler.map(str::trim).unwrap_or_default().to_string(),
    };
    let mut named = defs.iter().filter(|def| def.name == name);
    let def = named.next()?;
    if named.next().is_some() {
        return None;
    }
    // A route is never registered from inside its own handler, so a named
    // definition that opens above the registration line and contains it is
    // the function the route is registered FROM (a plugin), not the handler
    // (carrick#2824).
    if def.line_number < line && line <= def.end_line {
        return None;
    }
    span_of(def)
}

fn valid(span: HandlerSpan) -> Option<HandlerSpan> {
    (span.start_line > 0 && span.end_line >= span.start_line).then_some(span)
}

fn span_of(def: &FunctionDefinition) -> Option<HandlerSpan> {
    valid(HandlerSpan {
        start_line: def.line_number,
        end_line: def.end_line,
    })
}

/// `"src/routes.ts:12"` → `("src/routes.ts", 12)`.
pub(crate) fn split_location(location: &str) -> Option<(String, u32)> {
    let (file, line) = location.rsplit_once(':')?;
    let line = line.parse::<u32>().ok().filter(|l| *l > 0)?;
    Some((normalize(file), line))
}

pub(crate) fn normalize(path: &str) -> String {
    path.strip_prefix("./").unwrap_or(path).replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::ApiEndpointDetails;
    use crate::mount_graph::{MountGraph, ResolvedEndpoint};
    use std::path::PathBuf;

    fn def(name: &str, file: &str, start: u32, end: u32) -> FunctionDefinition {
        FunctionDefinition {
            name: name.to_string(),
            file_path: PathBuf::from(file),
            node_type: Default::default(),
            arguments: vec![],
            body_source: None,
            is_exported: false,
            line_number: start,
            end_line: end,
            intent: None,
            calls: vec![],
            tokens: vec![],
            return_type: None,
            return_is_explicit: false,
            signature: None,
            intent_input_hash: None,
            dispatch_table: None,
        }
    }

    fn row(method: &str, path: &str, handler: &str, location: &str) -> ResolvedEndpoint {
        ResolvedEndpoint {
            declared_at: None,
            procedure: None,
            method: method.to_string(),
            path: path.to_string(),
            full_path: format!("/api{path}"),
            handler: Some(handler.to_string()),
            owner: "router".to_string(),
            file_location: location.to_string(),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
            resolution_source: None,
            view_module: false,
            dispatch: None,
            handler_span: None,
            address: None,
        }
    }

    fn payload(defs: Vec<FunctionDefinition>, rows: Vec<ResolvedEndpoint>) -> CloudRepoData {
        let mut graph = MountGraph::new();
        graph.endpoints = rows;
        let (endpoints, _) = crate::cloud_storage::mount_graph_to_api_details(&graph);
        let mut data = CloudRepoData {
            repo_name: "orders".to_string(),
            service_name: None,
            endpoints,
            calls: vec![],
            mounts: vec![],
            apps: HashMap::new(),
            imported_handlers: vec![],
            function_definitions: HashMap::new(),
            config_json: None,
            package_json: None,
            packages: None,
            last_updated: chrono::Utc::now(),
            commit_hash: "abc123".to_string(),
            dirty: None,
            mount_graph: Some(graph),
            bundled_types: None,
            type_manifest: None,
            file_results: None,
            cached_detection: None,
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: None,
            cache_version: None,
            type_extraction_status: None,
            types_degraded: None,
            compat_verdicts: None,
            capture_stub: None,
            external_call_candidates: None,
            sdk_surface: None,
            sdk_edges: None,
            sdk_unresolved: None,
            scanner_version: None,
            scanner_build: None,
            boundary: None,
            dispatch_tables: None,
            possible_calls: None,
            possible_calls_omitted: None,
            unread_sites: None,
            unread_sites_total: None,
            unread_sites_not_listed: None,
            unread_sites_by_reason: None,
            call_through: None,
            route_boundaries: None,
            memory_peaks: None,
            unfollowed_member_calls: Vec::new(),
            unfollowed_member_calls_total: 0,
            type_reach: None,
            pair_outcomes: None,
        };
        for (i, d) in defs.into_iter().enumerate() {
            data.function_definitions
                .insert(format!("{}#{i}", d.name), d);
        }
        data
    }

    fn spans(data: &CloudRepoData) -> Vec<Option<HandlerSpan>> {
        data.mount_graph
            .as_ref()
            .unwrap()
            .endpoints
            .iter()
            .map(|e| e.handler_span)
            .collect()
    }

    /// The source a test reads: one file, `file`, holding `text`.
    fn reader(file: &'static str, text: &'static str) -> impl Fn(&str) -> Option<String> {
        move |asked: &str| (asked == file).then(|| text.to_string())
    }

    /// Join each row to the call the candidate scanner records at its line,
    /// as the model-row join does: `file_results` carries the call's span.
    fn join_calls(data: &mut CloudRepoData, file: &str, text: &str) {
        let candidates = crate::swc_scanner::SwcScanner::new()
            .scan_every_call(Path::new(file), text)
            .candidates;
        let rows = data.mount_graph.as_ref().unwrap().endpoints.clone();
        let endpoints = rows
            .iter()
            .filter_map(|row| {
                let (_, line) = split_location(&row.file_location)?;
                let call = candidates
                    .iter()
                    .find(|c| c.line_number as u32 == line)
                    .unwrap_or_else(|| panic!("no candidate at line {line}"));
                Some(
                    serde_json::from_value(serde_json::json!({
                        "candidate_id": call.candidate_id,
                        "line_number": line,
                        "owner_node": row.owner,
                        "method": row.method,
                        "path": row.path,
                        "handler_name": row.handler,
                        "pattern_matched": "",
                        "call_expression_span_start": call.span_start,
                        "call_expression_span_end": call.span_end,
                    }))
                    .unwrap(),
                )
            })
            .collect();
        data.file_results = Some(HashMap::from([(
            file.to_string(),
            FileAnalysisResult {
                endpoints,
                ..Default::default()
            },
        )]));
    }

    const fn span(start_line: u32, end_line: u32) -> Option<HandlerSpan> {
        Some(HandlerSpan {
            start_line,
            end_line,
        })
    }

    /// The audit's shape: `router.get(` opens at 1, validators run to 4, and
    /// the arrow handed in last spans 5-7. The row carries the arrow's span,
    /// on both row lists.
    #[test]
    fn an_inline_handler_behind_a_validator_chain_carries_the_literal_span() {
        const FILE: &str = "src/routes/invoices.ts";
        const TEXT: &str = "router.get(\n  \"/invoices/:id/matches\",\n  param(\"id\").isUUID(),\n  validate,\n  async (req, res) => {\n    res.json(await matches(req.params.id));\n  },\n);\n";
        let mut data = payload(
            vec![def("get_invoices__id_matches_handler", FILE, 5, 7)],
            vec![row(
                "GET",
                "/invoices/:id/matches",
                "anonymous",
                &format!("{FILE}:1"),
            )],
        );
        join_calls(&mut data, FILE, TEXT);

        assert_eq!(attach_handler_spans(&mut data, reader(FILE, TEXT)), 1);
        assert_eq!(spans(&data), vec![span(5, 7)]);
        assert_eq!(data.endpoints[0].handler_span, span(5, 7));
    }

    /// N32-a: the model names an inline handler `"handler"`, a name nothing in
    /// the file declares, and two inline middlewares open before it. The
    /// function index holds only the first literal under the name it builds
    /// from the route (`visitor::derive_handler_name` indexes one literal per
    /// built name), so neither the model's name nor the built one reaches the
    /// handler. The registration call's last argument does.
    #[test]
    fn an_inline_handler_behind_inline_middlewares_is_placed_from_the_call() {
        const FILE: &str = "src/orders.ts";
        const TEXT: &str = "router.post(\n  \"/orders\",\n  (req, res, next) => next(),\n  (req, res, next) => {\n    next();\n  },\n  async (req, res) => {\n    res.status(201).json(await create(req.body));\n  },\n);\n";
        let mut data = payload(
            vec![def("post_orders_handler", FILE, 3, 3)],
            vec![row("POST", "/orders", "handler", &format!("{FILE}:1"))],
        );
        join_calls(&mut data, FILE, TEXT);

        assert_eq!(attach_handler_spans(&mut data, reader(FILE, TEXT)), 1);
        assert_eq!(spans(&data), vec![span(7, 9)]);
    }

    /// A call that hands in an identifier is placed at the one definition of
    /// that name in the file, whatever the model called the handler.
    #[test]
    fn an_identifier_handed_in_last_is_placed_by_its_own_name() {
        const FILE: &str = "src/orders.ts";
        const TEXT: &str = "router.get(\"/orders\", requireAuth, listOrders);\n\nfunction listOrders(req, res) {\n  res.json([]);\n}\n";
        let mut data = payload(
            vec![def("listOrders", FILE, 3, 5)],
            vec![row("GET", "/orders", "anonymous", &format!("{FILE}:1"))],
        );
        join_calls(&mut data, FILE, TEXT);

        attach_handler_spans(&mut data, reader(FILE, TEXT));
        assert_eq!(spans(&data), vec![span(3, 5)]);
    }

    /// A named handler declared in the same file is placed by name, wherever
    /// in the file it is declared; one imported from elsewhere is not placed.
    #[test]
    fn a_named_handler_is_placed_only_when_declared_once_in_the_same_file() {
        let mut data = payload(
            vec![
                def("listOrders", "src/orders.ts", 40, 52),
                def("getOrder", "src/handlers/get-order.ts", 3, 9),
                def("dup", "src/orders.ts", 60, 61),
                def("dup", "src/orders.ts", 70, 71),
            ],
            vec![
                row("GET", "/orders", "listOrders", "src/orders.ts:5"),
                row("GET", "/orders/:id", "getOrder", "src/orders.ts:6"),
                row("POST", "/orders", "dup", "src/orders.ts:7"),
            ],
        );

        attach_handler_spans(&mut data, |_: &str| None);
        assert_eq!(spans(&data), vec![span(40, 52), None, None]);
    }

    /// The plugin function the routes are registered from names every row, but
    /// its definition encloses each registration: each row is placed at the
    /// literal its own call hands in.
    #[test]
    fn a_named_handler_that_encloses_its_registration_is_read_as_inline() {
        const FILE: &str = "src/routes/report.ts";
        const TEXT: &str = "export async function reportRoutes(app) {\n  app.get(\"/trend/a\", async () => {\n    return a();\n  });\n  app.get(\"/trend/c/:id\", async (req) => {\n    return c(req.params.id);\n  });\n}\n";
        let mut data = payload(
            vec![
                def("reportRoutes", FILE, 1, 8),
                def("get_trend_a_handler", FILE, 2, 4),
            ],
            vec![
                row("GET", "/trend/a", "reportRoutes", &format!("{FILE}:2")),
                row("GET", "/trend/c/:id", "reportRoutes", &format!("{FILE}:5")),
            ],
        );
        join_calls(&mut data, FILE, TEXT);

        attach_handler_spans(&mut data, reader(FILE, TEXT));
        assert_eq!(spans(&data), vec![span(2, 4), span(5, 7)]);
    }

    /// An enclosing named handler with no inline definition indexed places
    /// nothing, rather than the plugin's whole span.
    #[test]
    fn an_enclosing_named_handler_with_no_inline_definition_places_nothing() {
        let mut data = payload(
            vec![def("reportRoutes", "src/routes/report.ts", 11, 43)],
            vec![row(
                "GET",
                "/trend/a",
                "reportRoutes",
                "src/routes/report.ts:12",
            )],
        );

        attach_handler_spans(&mut data, |_: &str| None);
        assert_eq!(spans(&data), vec![None]);
    }

    /// A route call that passes an inline middleware before its inline handler
    /// on one line: the handler is the last literal.
    #[test]
    fn the_last_inline_literal_of_a_registration_is_the_handler() {
        const FILE: &str = "src/orders.ts";
        const TEXT: &str = "app.post(\"/orders\", (req, res, next) => next(), (req, res) => {\n  res.sendStatus(201);\n});\n";
        let mut data = payload(
            vec![def("post_orders_handler", FILE, 1, 1)],
            vec![row("POST", "/orders", "anonymous", &format!("{FILE}:1"))],
        );
        join_calls(&mut data, FILE, TEXT);

        attach_handler_spans(&mut data, reader(FILE, TEXT));
        assert_eq!(spans(&data), vec![span(1, 3)]);
    }

    /// A row joined to no call places no inline handler: the name the scan
    /// builds from the method and path (`get_health_handler`) is never read.
    #[test]
    fn a_row_joined_to_no_call_is_not_placed_by_a_built_name() {
        let mut data = payload(
            vec![def("get_health_handler", "src/app.ts", 3, 5)],
            vec![row("GET", "/health", "anonymous", "src/app.ts:2")],
        );

        attach_handler_spans(&mut data, |_: &str| None);
        assert_eq!(spans(&data), vec![None]);
    }

    /// carrick#3655: a receiver-trace or library-fact row has no call span in
    /// `file_results`. Its own file and line name the registration call, and
    /// the arrow that call hands in after its middleware is the handler.
    #[test]
    fn a_trace_row_with_no_call_span_is_placed_from_the_call_at_its_line() {
        const FILE: &str = "src/routes/engagements.ts";
        const TEXT: &str = "const router = new Hono()\n\nrouter.get('/:id', zValidator('param', idParam), async (c) => {\n  const { id } = c.req.valid('param')\n  return c.json(await getEngagement(id))\n})\n";
        let mut data = payload(
            vec![],
            vec![row("GET", "/:id", "anonymous", &format!("{FILE}:3"))],
        );
        assert!(data.file_results.is_none());

        assert_eq!(attach_handler_spans(&mut data, reader(FILE, TEXT)), 1);
        assert_eq!(spans(&data), vec![span(3, 6)]);
        assert_eq!(data.endpoints[0].handler_span, span(3, 6));
    }

    /// The same row whose call hands in a handler imported from another file
    /// places nothing, as before: the file declares no function of that name.
    #[test]
    fn a_trace_row_whose_handler_is_imported_places_nothing() {
        const FILE: &str = "src/routes/orders.ts";
        const TEXT: &str = "import { listOrders } from '../handlers/orders'\n\nrouter.get('/orders', requireAuth, listOrders)\n";
        let mut data = payload(
            vec![def("listOrders", "src/handlers/orders.ts", 3, 9)],
            vec![row("GET", "/orders", "listOrders", &format!("{FILE}:3"))],
        );

        assert_eq!(attach_handler_spans(&mut data, reader(FILE, TEXT)), 0);
        assert_eq!(spans(&data), vec![None]);
    }

    /// Wire spelling on both structs, and the field is absent, not null, on a
    /// row the scan did not place.
    #[test]
    fn handler_span_wire_spelling_and_absence() {
        let mut placed = row("GET", "/x", "anonymous", "src/a.ts:1");
        placed.handler_span = span(2, 4);
        let json = serde_json::to_value(&placed).unwrap();
        assert_eq!(
            json["handler_span"],
            serde_json::json!({ "start_line": 2, "end_line": 4 })
        );

        let (endpoints, _) = crate::cloud_storage::mount_graph_to_api_details(&MountGraph {
            endpoints: vec![placed],
            ..MountGraph::new()
        });
        let json = serde_json::to_value(&endpoints[0]).unwrap();
        assert_eq!(
            json["handler_span"],
            serde_json::json!({ "start_line": 2, "end_line": 4 })
        );

        let unplaced = serde_json::to_string(&row("GET", "/y", "h", "src/a.ts:9")).unwrap();
        assert!(!unplaced.contains("handler_span"), "got: {unplaced}");
    }

    /// Rows written by a scanner that predates the field still load.
    #[test]
    fn rows_without_handler_span_deserialize() {
        let endpoint: ResolvedEndpoint = serde_json::from_value(serde_json::json!({
            "method": "GET",
            "path": "/x",
            "full_path": "/x",
            "handler": "anonymous",
            "owner": "app",
            "file_location": "src/a.ts:1",
            "middleware_chain": []
        }))
        .unwrap();
        assert_eq!(endpoint.handler_span, None);

        let mut details = serde_json::to_value(
            &crate::cloud_storage::mount_graph_to_api_details(&MountGraph {
                endpoints: vec![endpoint],
                ..MountGraph::new()
            })
            .0[0],
        )
        .unwrap();
        details.as_object_mut().unwrap().remove("handler_span");
        let back: ApiEndpointDetails = serde_json::from_value(details).unwrap();
        assert_eq!(back.handler_span, None);
    }
}
