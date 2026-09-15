//! Place each route's handler function on its endpoint row (cloud#948).
//!
//! An endpoint row's location is the line its registration call opens on. For
//! an inline handler behind a validator chain that line is several lines above
//! the function itself, so a reader asking "is this function the handler of a
//! route?" by comparing the function's span with the row's line answers no, and
//! labels the handler a helper in a route file. The row therefore carries the
//! handler's own span, `handler_span`, 1-based and inclusive, in the row's file.
//!
//! The span is read from the function index this scan already built, not from
//! a second parse, because the question the cloud asks is about THOSE rows: the
//! function a search returns is a `function_definitions` row, and the span that
//! decides its role must be the same row's span. Two placements, and nothing is
//! guessed beyond them:
//!
//! - **A named handler declared in the same file** (`router.get("/x", listX)`
//!   with `function listX` in that file): the one definition in the file with
//!   that name. Two same-named definitions in the file place nothing.
//! - **An inline handler** (the model names it `anonymous`, or names none): the
//!   discovery pass indexes a function literal passed to a call under a name
//!   derived from the call (`get_users__id_handler`, see
//!   `visitor::derive_handler_name`). The row's method and registered path give
//!   the same name. The candidates are the definitions with that name that open
//!   at or after the registration line and before the next registration in the
//!   file; of those not nested inside another, the LAST is the handler, because
//!   a route call passes its handler after any inline middleware. A handler a
//!   later registration could own is not placed.

use crate::cloud_storage::CloudRepoData;
use crate::mount_graph::HandlerSpan;
use crate::operation::OperationKey;
use crate::visitor::FunctionDefinition;
use std::collections::HashMap;

/// Names the file analyzer gives a handler that has no name of its own.
const INLINE_HANDLER_NAMES: [&str; 3] = ["", "anonymous", "<anonymous>"];

/// Stamp `handler_span` onto every HTTP endpoint row the payload holds: the
/// mount graph's rows, and the top-level `endpoints[]` rows projected from
/// them. Run after `relativize_cloud_paths`, so a row's file and a
/// definition's file are in the same, repo-relative form. Returns how many
/// mount-graph rows were placed.
pub fn attach_handler_spans(data: &mut CloudRepoData) -> usize {
    let mut by_file: HashMap<String, Vec<&FunctionDefinition>> = HashMap::new();
    for def in data.function_definitions.values() {
        by_file
            .entry(normalize(&def.file_path.to_string_lossy()))
            .or_default()
            .push(def);
    }

    let Some(graph) = data.mount_graph.as_mut() else {
        return 0;
    };

    // Every registration line per file, so an inline handler is never handed
    // to a registration that opens before a later one.
    let mut registrations: HashMap<String, Vec<u32>> = HashMap::new();
    for endpoint in &graph.endpoints {
        if let Some((file, line)) = split_location(&endpoint.file_location) {
            registrations.entry(file).or_default().push(line);
        }
    }

    let mut placed_rows: HashMap<(String, OperationKey, Option<String>), HandlerSpan> =
        HashMap::new();
    let mut placed = 0;
    for endpoint in &mut graph.endpoints {
        let Some((file, line)) = split_location(&endpoint.file_location) else {
            continue;
        };
        let defs = by_file.get(&file).map(Vec::as_slice).unwrap_or_default();
        let lines = registrations
            .get(&file)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let span = place(
            defs,
            lines,
            line,
            endpoint.handler.as_deref(),
            &endpoint.method,
            &endpoint.path,
        );
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

/// The span of the handler registered at `line`, or `None` when it cannot be
/// placed in this file.
fn place(
    defs: &[&FunctionDefinition],
    registrations: &[u32],
    line: u32,
    handler: Option<&str>,
    method: &str,
    path: &str,
) -> Option<HandlerSpan> {
    let handler = handler.map(str::trim).unwrap_or_default();
    if !INLINE_HANDLER_NAMES.contains(&handler) {
        let mut named = defs.iter().filter(|def| def.name == handler);
        let def = named.next()?;
        if named.next().is_some() {
            return None;
        }
        return span_of(def);
    }

    let derived = crate::visitor::derive_handler_name(&method.to_lowercase(), Some(path));
    let next_registration = registrations
        .iter()
        .copied()
        .filter(|other| *other > line)
        .min()
        .unwrap_or(u32::MAX);
    let candidates: Vec<&FunctionDefinition> = defs
        .iter()
        .copied()
        .filter(|def| {
            def.name == derived && def.line_number >= line && def.line_number < next_registration
        })
        .collect();
    let outermost = candidates.iter().filter(|def| {
        !candidates.iter().any(|outer| {
            !std::ptr::eq(**def, *outer)
                && outer.line_number <= def.line_number
                && def.end_line <= outer.end_line
                && (outer.line_number, outer.end_line) != (def.line_number, def.end_line)
        })
    });
    let def = outermost.max_by_key(|def| def.line_number)?;
    span_of(def)
}

fn span_of(def: &FunctionDefinition) -> Option<HandlerSpan> {
    (def.line_number > 0 && def.end_line >= def.line_number).then_some(HandlerSpan {
        start_line: def.line_number,
        end_line: def.end_line,
    })
}

/// `"src/routes.ts:12"` → `("src/routes.ts", 12)`.
fn split_location(location: &str) -> Option<(String, u32)> {
    let (file, line) = location.rsplit_once(':')?;
    let line = line.parse::<u32>().ok().filter(|l| *l > 0)?;
    Some((normalize(file), line))
}

fn normalize(path: &str) -> String {
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
            boundary: None,
            dispatch_tables: None,
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

    const fn span(start_line: u32, end_line: u32) -> Option<HandlerSpan> {
        Some(HandlerSpan {
            start_line,
            end_line,
        })
    }

    /// The audit's shape: `router.get(` opens at 10, validators run to 13, and
    /// the arrow the discovery pass indexed as `get_invoices__id_matches_handler`
    /// spans 14-30. The row carries the arrow's span, on both row lists.
    #[test]
    fn an_inline_handler_behind_a_validator_chain_carries_the_literal_span() {
        let mut data = payload(
            vec![def(
                "get_invoices__id_matches_handler",
                "src/routes/invoices.ts",
                14,
                30,
            )],
            vec![row(
                "GET",
                "/invoices/:id/matches",
                "anonymous",
                "src/routes/invoices.ts:10",
            )],
        );

        assert_eq!(attach_handler_spans(&mut data), 1);
        assert_eq!(spans(&data), vec![span(14, 30)]);
        assert_eq!(data.endpoints[0].handler_span, span(14, 30));
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

        attach_handler_spans(&mut data);
        assert_eq!(spans(&data), vec![span(40, 52), None, None]);
    }

    /// A route call that passes an inline middleware before its inline handler
    /// has both indexed under the derived name; the handler is the last.
    #[test]
    fn the_last_inline_literal_of_a_registration_is_the_handler() {
        let mut data = payload(
            vec![
                def("post_orders_handler", "src/orders.ts", 4, 6),
                def("post_orders_handler", "src/orders.ts", 7, 12),
            ],
            vec![row("POST", "/orders", "anonymous", "src/orders.ts:3")],
        );

        attach_handler_spans(&mut data);
        assert_eq!(spans(&data), vec![span(7, 12)]);
    }

    /// An inline handler is never handed to a registration that opens before a
    /// later registration, and a derived-name definition that opens BEFORE the
    /// row belongs to an earlier call.
    #[test]
    fn an_inline_handler_a_later_registration_could_own_is_not_placed() {
        let mut data = payload(
            vec![def("get_health_handler", "src/app.ts", 8, 9)],
            vec![
                row("GET", "/health", "anonymous", "src/app.ts:2"),
                row("POST", "/other", "anonymous", "src/app.ts:5"),
                row("GET", "/health", "", "src/app.ts:12"),
            ],
        );

        attach_handler_spans(&mut data);
        assert_eq!(spans(&data), vec![None, None, None]);
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
