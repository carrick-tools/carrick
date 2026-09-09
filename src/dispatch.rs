//! Body dispatch: one route, many operations (carrick#831).
//!
//! A handler that reads a field off the request and switches on it serves
//! several operations behind one `(METHOD, path)`. Our own
//! `POST /types/check-or-upload` is nine of them. To the index that route is
//! one operation with one request type, so nine contracts collapse into one
//! and every consumer of any of them matches all nine — or, when the route
//! itself lives outside the source (an API gateway), none of them.
//!
//! The discriminator is read the same way a route is read: from the handler
//! and from the call site, by the model, and never inferred from the shape of
//! a `switch`. Three facts travel:
//!
//! - [`Dispatch`] on a PRODUCER row: the field this operation switches on and
//!   the literal this case answers.
//! - [`Dispatch`] on a CALL row: the literal that call sends for that field.
//! - [`DispatchTable`] on a service: a handler switches on this field and
//!   these are the values it answers. It is a fact about the HANDLER, stated
//!   whether or not the handler declares a route, and it is the only thing a
//!   routeless handler can state — the route is not in the source, so no
//!   operation row may be invented for it (that is what the `operations` block
//!   in `carrick.json` is for).
//!
//! Absence means a plain route, everywhere. A row without a dispatch is an
//! operation whose identity is its method and path, exactly as before; nothing
//! here changes what an ordinary route means.

use serde::{Deserialize, Serialize};

/// Where the dispatch field is read from.
///
/// Two locations, because those are the two a handler can switch on before it
/// has routed: a JSON body field and a request header. A query parameter is
/// part of the URL and belongs to the path, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchLocation {
    /// A field of the request body (`body.action`).
    Body,
    /// A request header (`x-carrick-action`).
    Header,
}

impl DispatchLocation {
    pub fn as_str(&self) -> &'static str {
        match self {
            DispatchLocation::Body => "body",
            DispatchLocation::Header => "header",
        }
    }
}

/// One dispatch case: the field a handler switches on, and the literal this
/// side states for it.
///
/// The same shape on both sides of the seam, deliberately. On a producer row
/// `value` is the case this operation answers; on a call row it is the value
/// that call sends. A matcher comparing the two is comparing like with like,
/// which is what makes [`carrick_match::dispatch_verdict`] a three-line
/// function rather than a pair of asymmetric rules.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Dispatch {
    pub location: DispatchLocation,
    /// The field name as written at the source, e.g. `action`. A nested field
    /// is written with dots (`meta.kind`); nothing here parses it.
    pub field: String,
    /// The literal this side states for that field, verbatim.
    pub value: String,
}

impl Dispatch {
    /// The canonical key text for this case — the string that makes nine
    /// operations behind one route nine keys. Delegates to the matcher crate
    /// so the scanner, the wasm consumer and every bookkeeping map build the
    /// same one.
    pub fn key(&self) -> String {
        carrick_match::dispatch_key(self.location.as_str(), &self.field, &self.value)
    }

    /// `Some(key)` for an optional dispatch, for handing straight to
    /// [`carrick_match::dispatch_verdict`].
    pub fn key_of(dispatch: Option<&Dispatch>) -> Option<String> {
        dispatch.map(|d| d.key())
    }

    /// The suffix a `METHOD:path` bookkeeping key needs so the cases of one
    /// dispatching route cannot fold into each other. Empty for a plain route,
    /// so an existing key is byte-identical to what it was before this field
    /// existed.
    pub fn key_suffix(dispatch: Option<&Dispatch>) -> String {
        match dispatch {
            Some(d) => format!("#{}", d.key()),
            None => String::new(),
        }
    }
}

/// What one handler switches on, and every value it answers.
///
/// A fact about the handler, carried per service. Two things read it: a human
/// or an agent asking what a routeless service actually serves, and the
/// config-suggestion path, which pre-fills an `operations` block from it. The
/// matcher does NOT read it — a table states no route, and v1 synthesises no
/// operation from one (carrick#831 ruling).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchTable {
    pub location: DispatchLocation,
    /// The field the handler switches on.
    pub field: String,
    /// Every value the handler answers, in the order extraction stated them.
    /// Deduplicated on the way in; a table with no values is dropped rather
    /// than carried, since it states nothing.
    pub values: Vec<String>,
    /// The handler function the switch lives in, as extraction named it
    /// (`anonymous` for an inline one). Same spelling as the extraction
    /// schema's `handler_name`, so the wire word is one word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler_name: Option<String>,
    /// Repo-relative path of the file the switch lives in.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file_path: String,
    /// 1-based line the switch opens on, when extraction stated one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_number: Option<u32>,
    /// Owning service, stamped during the cross-repo merge exactly like
    /// `ApiEndpointDetails::service_name`. `None` before the merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// Owning repo, stamped during the cross-repo merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_name: Option<String>,
}

impl DispatchTable {
    /// Drop empty values, deduplicate, and drop a table that states no field
    /// or no value at all. Extraction is a model answer; this is the one place
    /// that decides what counts as a table worth carrying.
    pub fn normalized(mut self) -> Option<Self> {
        self.field = self.field.trim().to_string();
        if self.field.is_empty() {
            return None;
        }
        let mut seen = std::collections::HashSet::new();
        self.values
            .retain(|v| !v.trim().is_empty() && seen.insert(v.clone()));
        if self.values.is_empty() {
            return None;
        }
        Some(self)
    }
}

/// Every dispatch table the analysis stated, stamped with the file it was
/// found in, ready for the blob.
///
/// `None` when the scan found none — the field then reads as "this scanner
/// states dispatch tables and there were none here", which is the same shape
/// every other optional channel on the blob uses. The keys of `file_results`
/// are file paths as the scan held them; the caller relativizes the blob
/// afterwards, exactly as it does for every other path it writes.
pub fn collect_dispatch_tables(
    file_results: &std::collections::HashMap<
        String,
        crate::agents::file_analyzer_agent::FileAnalysisResult,
    >,
) -> Option<Vec<DispatchTable>> {
    let mut tables: Vec<DispatchTable> = Vec::new();
    // Sorted so two scans of one tree write the same array: a HashMap's order
    // is not the blob's to inherit.
    let mut files: Vec<&String> = file_results.keys().collect();
    files.sort();
    for file in files {
        for table in &file_results[file].dispatch_tables {
            let mut table = table.clone();
            table.file_path = file.clone();
            tables.push(table);
        }
    }
    (!tables.is_empty()).then_some(tables)
}

/// Materialise the `operations` blocks a service declares into its mount
/// graph (carrick#831), replacing whatever inference produced for the same
/// handler.
///
/// Returns how many operation rows were written. Declared rows REPLACE rather
/// than join: the repo has stated what this handler serves, and two sets for
/// one handler would leave a reader guessing which is current.
///
/// Two shapes, and they differ only in how the route is found:
///
/// - A block with a `route` names it outright. This is the routeless-handler
///   case — an API-gateway lambda, where the route is in infrastructure and
///   not in the source — and it is the only way such a handler gets operation
///   rows at all. Any existing row on that exact `(METHOD, path)` is replaced,
///   including a plain one: a plain row would go on matching every call to the
///   route whatever it sends, which is the collapse this whole change exists
///   to remove. What such a row knew — its handler name and its location — is
///   carried onto the declared rows when there was exactly one of it, so the
///   type layer's anchors survive the promotion.
/// - A block with no `route` promotes whatever inference already found: every
///   route in the graph carrying a dispatch on the same (location, field).
///   A block that matches nothing is WARNED about, not failed: inference is a
///   model reading, so a block can be right about a handler on a run where
///   the model said nothing about it.
pub fn apply_declared_operations(
    graph: &mut crate::mount_graph::MountGraph,
    config: &crate::config::Config,
) -> usize {
    use crate::agents::file_analyzer_agent::ResolutionSource;

    let mut written = 0usize;
    for block in &config.declared_operations {
        // Validated at config load; a bad route cannot reach here.
        let declared_route = block.parsed_route().ok().flatten();
        let routes: Vec<(String, String)> = match &declared_route {
            Some(route) => vec![route.clone()],
            None => {
                let mut inferred: Vec<(String, String)> = graph
                    .endpoints
                    .iter()
                    .filter(|endpoint| {
                        endpoint.dispatch.as_ref().is_some_and(|d| {
                            d.location == block.dispatch.location && d.field == block.dispatch.field
                        })
                    })
                    .map(|endpoint| (endpoint.method.clone(), endpoint.full_path.clone()))
                    .collect();
                inferred.sort();
                inferred.dedup();
                if inferred.is_empty() {
                    tracing::warn!(
                        "carrick.json declares operations on '{}' for field '{}', and no route \
                         in this scan dispatches on it. Add a `route` to the block, or remove it.",
                        block.service,
                        block.dispatch.field
                    );
                }
                inferred
            }
        };

        for (method, path) in routes {
            let replaced: Vec<crate::mount_graph::ResolvedEndpoint> = graph
                .endpoints
                .iter()
                .filter(|endpoint| {
                    endpoint.method.eq_ignore_ascii_case(&method) && endpoint.full_path == path
                })
                .cloned()
                .collect();
            graph.endpoints.retain(|endpoint| {
                !(endpoint.method.eq_ignore_ascii_case(&method) && endpoint.full_path == path)
            });
            // The anchors a single replaced row carried. Several rows and we
            // keep none of them: picking one would attribute every declared
            // operation to whichever route row happened to sort first.
            let carried = (replaced.len() == 1).then(|| replaced[0].clone());

            for operation in &block.operations {
                let (file_location, handler) = declared_location(operation, carried.as_ref());
                graph.endpoints.push(crate::mount_graph::ResolvedEndpoint {
                    method: method.clone(),
                    path: path.clone(),
                    full_path: path.clone(),
                    handler,
                    owner: carried
                        .as_ref()
                        .map(|row| row.owner.clone())
                        .unwrap_or_else(|| block.service.clone()),
                    file_location,
                    middleware_chain: Vec::new(),
                    repo_name: None,
                    service_name: None,
                    provenance: carried
                        .as_ref()
                        .map(|row| row.provenance)
                        .unwrap_or_default(),
                    // A declaration states a route the service SERVES. It is
                    // never call-site evidence, whatever the row it replaced
                    // was: the repo is not describing a request it makes.
                    evidence: carrick_match::MatchEvidence::RouteDefinition,
                    resolution_source: Some(ResolutionSource::DeclaredOperation),
                    view_module: carried.as_ref().is_some_and(|row| row.view_module),
                    dispatch: Some(Dispatch {
                        location: block.dispatch.location,
                        field: block.dispatch.field.clone(),
                        value: operation.value.clone(),
                    }),
                });
                written += 1;
            }
        }
    }
    written
}

/// Where a declared operation points: the `file:symbol` the block names, or
/// the row it replaced, or nothing.
///
/// The declared handler is written as `file:symbol` and a row's location is
/// `file:line`, so the symbol travels as the handler NAME and the location
/// keeps the file with line 0 — a location a reader can open, and one that
/// `parse_file_location` still reads as a location rather than as a path with
/// a symbol glued to it.
fn declared_location(
    operation: &crate::config::DeclaredOperation,
    carried: Option<&crate::mount_graph::ResolvedEndpoint>,
) -> (String, Option<String>) {
    if let Some(handler) = operation.handler.as_deref()
        && let Some((file, symbol)) = handler.rsplit_once(':')
        && !file.trim().is_empty()
    {
        return (format!("{file}:0"), Some(symbol.to_string()));
    }
    match carried {
        Some(row) => (row.file_location.clone(), row.handler.clone()),
        None => (String::new(), operation.handler.clone()),
    }
}

/// The advisory for every handler that switches on a request field and has no
/// `operations` block declaring what it serves (carrick#831).
///
/// One finding per (service, location, field). It carries the material a
/// reader needs to accept the model's reading rather than author it: the
/// values, the route consumers name (when any do), and the call sites already
/// stating a value. A table whose field IS declared produces nothing — the
/// question it asks has been answered.
///
/// Never a failure and never a claim that anything is wrong: the handler
/// works. What is missing is in the index, not in the code.
pub fn dispatch_operation_findings(
    repos: &[crate::cloud_storage::CloudRepoData],
) -> Vec<crate::findings::Finding> {
    let mut findings = Vec::new();
    for repo in repos {
        let Some(tables) = repo.dispatch_tables.as_ref() else {
            continue;
        };
        let service = repo
            .service_name
            .clone()
            .unwrap_or_else(|| repo.repo_name.clone());
        let declared = declared_fields(repo);
        for table in tables {
            if declared.contains(&(table.location, table.field.clone())) {
                continue;
            }
            // The route and the call sites come from the CONSUMERS, because
            // the case this exists for is the handler whose route is not in
            // the source at all. A call counts when it states a value for
            // this field that this handler answers — that, and not the path,
            // is what ties it to this table.
            let mut routes: std::collections::BTreeMap<String, usize> = Default::default();
            let mut call_sites: Vec<String> = Vec::new();
            for other in repos {
                let other_service = other
                    .service_name
                    .clone()
                    .unwrap_or_else(|| other.repo_name.clone());
                if other_service == service {
                    continue;
                }
                for call in &other.calls {
                    let Some(dispatch) = call.dispatch.as_ref() else {
                        continue;
                    };
                    if dispatch.location != table.location || dispatch.field != table.field {
                        continue;
                    }
                    if !table.values.contains(&dispatch.value) {
                        continue;
                    }
                    if let Some((method, path)) = call.key.as_http() {
                        *routes.entry(format!("{method} {path}")).or_default() += 1;
                    }
                    call_sites.push(call.file_path.to_string_lossy().into_owned());
                }
            }
            // The route the most consumers name. A tie is broken by the route
            // text so two scans of one tree suggest the same block.
            let route = routes
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
                .map(|(route, _)| route);
            call_sites.sort();
            call_sites.dedup();
            findings.push(crate::findings::Finding::DispatchOperations {
                service: service.clone(),
                route,
                dispatch_location: table.location.as_str().to_string(),
                dispatch_field: table.field.clone(),
                values: table.values.clone(),
                call_sites,
            });
        }
    }
    findings
}

/// The `(location, field)` pairs this repo's `carrick.json` already declares
/// operations for, read back off the config the blob carries.
fn declared_fields(
    repo: &crate::cloud_storage::CloudRepoData,
) -> std::collections::HashSet<(DispatchLocation, String)> {
    let Some(config_json) = repo.config_json.as_deref() else {
        return Default::default();
    };
    let Ok(config) = serde_json::from_str::<crate::config::Config>(config_json) else {
        return Default::default();
    };
    config
        .declared_operations
        .iter()
        .map(|block| (block.dispatch.location, block.dispatch.field.clone()))
        .collect()
}

/// Copy each table onto the function row that declares its handler
/// (carrick#831), joined by `handler_name` + `line_number` in the same file.
///
/// Returns how many joined. The array stays the record either way: a table
/// whose handler the function index never saw (an inline handler, a file the
/// definition pass skipped) is carried there and simply has no row to sit on,
/// which is why this reports its count instead of asserting one.
pub fn stamp_dispatch_tables_on_functions(
    function_definitions: &mut std::collections::HashMap<
        String,
        crate::visitor::FunctionDefinition,
    >,
    tables: &[DispatchTable],
) -> usize {
    let mut stamped = 0usize;
    for table in tables {
        let Some(handler_name) = table.handler_name.as_deref() else {
            continue;
        };
        // Same name AND same file. One repo declares `handler` in a dozen
        // files, so the name alone would stamp the fact onto every one of
        // them.
        let candidates: Vec<String> = function_definitions
            .iter()
            .filter(|(_, definition)| definition.name == handler_name)
            .filter(|(_, definition)| {
                definition
                    .file_path
                    .to_string_lossy()
                    .ends_with(table.file_path.trim_start_matches("./"))
            })
            .map(|(key, _)| key.clone())
            .collect();

        // The line extraction stated for the declaration decides between
        // several same-named functions in one file. When it matches none of
        // them, a SOLE candidate still takes it: the line is the model's
        // reading of where a declaration opens and is worth a few lines of
        // slack, while the name and the file together already identify the
        // function. Several candidates and no line agreement stamps nothing
        // — the array is the record either way.
        let by_line: Vec<&String> = candidates
            .iter()
            .filter(|key| Some(function_definitions[*key].line_number) == table.line_number)
            .collect();
        let chosen: Vec<String> = if !by_line.is_empty() {
            by_line.into_iter().cloned().collect()
        } else if candidates.len() == 1 {
            candidates
        } else {
            Vec::new()
        };

        for key in chosen {
            if let Some(definition) = function_definitions.get_mut(&key) {
                definition.dispatch_table = Some(table.clone());
                stamped += 1;
            }
        }
    }
    stamped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(field: &str, value: &str) -> Dispatch {
        Dispatch {
            location: DispatchLocation::Body,
            field: field.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn location_wire_values_are_the_contract_the_cloud_reads() {
        let json = serde_json::to_string(&body("action", "upload-logs")).unwrap();
        assert_eq!(
            json,
            r#"{"location":"body","field":"action","value":"upload-logs"}"#
        );
        let header: DispatchLocation =
            serde_json::from_str(r#""header""#).expect("header is a wire value");
        assert_eq!(header, DispatchLocation::Header);
    }

    #[test]
    fn a_plain_route_keeps_the_key_it_always_had() {
        assert_eq!(Dispatch::key_suffix(None), "");
        assert_eq!(
            Dispatch::key_suffix(Some(&body("action", "upload-logs"))),
            "#body:action=upload-logs"
        );
    }

    fn block(route: Option<&str>, values: &[&str]) -> crate::config::DeclaredOperations {
        crate::config::DeclaredOperations {
            service: "check-or-upload".to_string(),
            route: route.map(str::to_string),
            dispatch: crate::config::DeclaredDispatch {
                location: DispatchLocation::Body,
                field: "action".to_string(),
            },
            operations: values
                .iter()
                .map(|value| crate::config::DeclaredOperation {
                    value: value.to_string(),
                    handler: Some(format!("lambdas/check-or-upload/index.ts:handle-{value}")),
                })
                .collect(),
        }
    }

    fn config_with(blocks: Vec<crate::config::DeclaredOperations>) -> crate::config::Config {
        crate::config::Config {
            service_name: Some("check-or-upload".to_string()),
            declared_operations: blocks,
            ..Default::default()
        }
    }

    /// carrick#831: a declared block with a route materialises one operation
    /// per value on a graph that had none — the routeless-lambda case, where
    /// the route lives in infrastructure and the scanner may not invent it.
    #[test]
    fn a_declared_route_materialises_one_operation_per_value() {
        use crate::agents::file_analyzer_agent::ResolutionSource;

        let mut graph = crate::mount_graph::MountGraph::new();
        let written = apply_declared_operations(
            &mut graph,
            &config_with(vec![block(
                Some("POST /types/check-or-upload"),
                &["search-by-intent", "upload-logs"],
            )]),
        );

        assert_eq!(written, 2);
        assert_eq!(graph.endpoints.len(), 2);
        let values: Vec<&str> = graph
            .endpoints
            .iter()
            .map(|e| e.dispatch.as_ref().unwrap().value.as_str())
            .collect();
        assert_eq!(values, vec!["search-by-intent", "upload-logs"]);
        for endpoint in &graph.endpoints {
            assert_eq!(endpoint.method, "POST");
            assert_eq!(endpoint.full_path, "/types/check-or-upload");
            assert_eq!(
                endpoint.resolution_source,
                Some(ResolutionSource::DeclaredOperation),
                "a declared operation is a fact, not the model's reading"
            );
            assert_eq!(
                endpoint.file_location, "lambdas/check-or-upload/index.ts:0",
                "the row points at the handler, not at carrick.json"
            );
            assert_eq!(
                endpoint.evidence,
                carrick_match::MatchEvidence::RouteDefinition
            );
        }
    }

    /// A declaration REPLACES what inference produced for the same route,
    /// including a plain row: a plain row would go on matching every call to
    /// the route whatever it sends, which is the collapse being removed.
    #[test]
    fn a_declaration_replaces_the_inferred_rows_for_its_route() {
        let mut graph = crate::mount_graph::MountGraph::new();
        graph.endpoints.push(crate::mount_graph::ResolvedEndpoint {
            method: "POST".to_string(),
            path: "/types/check-or-upload".to_string(),
            full_path: "/types/check-or-upload".to_string(),
            handler: Some("handler".to_string()),
            owner: "app".to_string(),
            file_location: "lambdas/check-or-upload/index.ts:390".to_string(),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
            resolution_source: None,
            view_module: false,
            dispatch: None,
        });

        apply_declared_operations(
            &mut graph,
            &config_with(vec![block(
                Some("POST /types/check-or-upload"),
                &["search-by-intent"],
            )]),
        );

        assert_eq!(
            graph.endpoints.len(),
            1,
            "the plain row is gone, not kept beside the declared one"
        );
        assert_eq!(
            graph.endpoints[0].dispatch.as_ref().unwrap().value,
            "search-by-intent"
        );
    }

    /// A block with no route promotes whatever inference found for the same
    /// field, and the values it declares are the ones that survive.
    #[test]
    fn a_routeless_block_promotes_the_inferred_dispatching_route() {
        use crate::agents::file_analyzer_agent::ResolutionSource;

        let mut graph = crate::mount_graph::MountGraph::new();
        let inferred = |value: &str| crate::mount_graph::ResolvedEndpoint {
            method: "POST".to_string(),
            path: "/types/check-or-upload".to_string(),
            full_path: "/types/check-or-upload".to_string(),
            handler: Some("handler".to_string()),
            owner: "app".to_string(),
            file_location: "lambdas/check-or-upload/index.ts:390".to_string(),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
            resolution_source: Some(ResolutionSource::Model),
            view_module: false,
            dispatch: Some(Dispatch {
                location: DispatchLocation::Body,
                field: "action".to_string(),
                value: value.to_string(),
            }),
        };
        // Inference found two of the three, and misread one of them.
        graph.endpoints.push(inferred("search-by-intent"));
        graph.endpoints.push(inferred("searchByIntent"));

        let written = apply_declared_operations(
            &mut graph,
            &config_with(vec![block(
                None,
                &["search-by-intent", "upload-logs", "download-file"],
            )]),
        );

        assert_eq!(written, 3);
        let mut values: Vec<&str> = graph
            .endpoints
            .iter()
            .map(|e| e.dispatch.as_ref().unwrap().value.as_str())
            .collect();
        values.sort();
        assert_eq!(
            values,
            vec!["download-file", "search-by-intent", "upload-logs"],
            "the declared set is the whole set; the misread case is gone"
        );
        assert!(
            graph
                .endpoints
                .iter()
                .all(|e| e.resolution_source == Some(ResolutionSource::DeclaredOperation)),
            "every surviving row is the repo's statement"
        );
    }

    fn table(values: &[&str]) -> DispatchTable {
        DispatchTable {
            location: DispatchLocation::Body,
            field: "action".to_string(),
            values: values.iter().map(|v| v.to_string()).collect(),
            handler_name: Some("handler".to_string()),
            file_path: "lambdas/check-or-upload/index.ts".to_string(),
            line_number: Some(419),
            service_name: None,
            repo_name: None,
        }
    }

    fn blob(name: &str, tables: Option<Vec<DispatchTable>>) -> crate::cloud_storage::CloudRepoData {
        let mut data = crate::cloud_storage::CloudRepoData {
            repo_name: name.to_string(),
            ..serde_json::from_value(serde_json::json!({
                "repo_name": name,
                "endpoints": [],
                "calls": [],
                "mounts": [],
                "apps": {},
                "imported_handlers": [],
                "function_definitions": {},
                "last_updated": "2026-01-01T00:00:00Z",
                "commit_hash": "deadbeef"
            }))
            .expect("a minimal blob deserializes")
        };
        data.service_name = Some(name.to_string());
        data.dispatch_tables = tables;
        data
    }

    /// carrick#831: the advisory carries the material for the block, and the
    /// route comes from the consumers, because the handler it is about may
    /// have no route in its own source at all.
    #[test]
    fn the_advisory_names_the_route_its_consumers_call() {
        let producer = blob(
            "check-or-upload",
            Some(vec![table(&["search-by-intent", "upload-logs"])]),
        );
        let mut consumer = blob("mcp-server", None);
        consumer.calls.push(crate::analyzer::ApiEndpointDetails {
            owner: None,
            key: crate::operation::OperationKey::http("POST", "/types/check-or-upload"),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: None,
            response_type: None,
            file_path: std::path::PathBuf::from("lambdas/mcp-server/src/api-client.ts:115"),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            resolution_source: None,
            view_module: false,
            dispatch: Some(Dispatch {
                location: DispatchLocation::Body,
                field: "action".to_string(),
                value: "search-by-intent".to_string(),
            }),
        });

        let findings = dispatch_operation_findings(&[producer, consumer]);
        assert_eq!(findings.len(), 1);
        let crate::findings::Finding::DispatchOperations {
            service,
            route,
            dispatch_field,
            values,
            call_sites,
            ..
        } = &findings[0]
        else {
            panic!("expected a dispatch_operations finding: {:?}", findings[0]);
        };
        assert_eq!(service, "check-or-upload");
        assert_eq!(route.as_deref(), Some("POST /types/check-or-upload"));
        assert_eq!(dispatch_field, "action");
        assert_eq!(values.len(), 2);
        assert_eq!(call_sites, &["lambdas/mcp-server/src/api-client.ts:115"]);
        assert_eq!(
            findings[0].severity(),
            crate::findings::Severity::Advisory,
            "never a failure: the handler works, the index is what is short"
        );
    }

    /// A table whose field the repo already declares asks nothing, so it says
    /// nothing.
    #[test]
    fn a_declared_field_produces_no_advisory() {
        let mut producer = blob("check-or-upload", Some(vec![table(&["upload-logs"])]));
        producer.config_json = Some(
            serde_json::to_string(&crate::config::Config {
                service_name: Some("check-or-upload".to_string()),
                declared_operations: vec![crate::config::DeclaredOperations {
                    service: "check-or-upload".to_string(),
                    route: Some("POST /types/check-or-upload".to_string()),
                    dispatch: crate::config::DeclaredDispatch {
                        location: DispatchLocation::Body,
                        field: "action".to_string(),
                    },
                    operations: vec![crate::config::DeclaredOperation {
                        value: "upload-logs".to_string(),
                        handler: None,
                    }],
                }],
                ..Default::default()
            })
            .unwrap(),
        );

        assert!(dispatch_operation_findings(&[producer]).is_empty());
    }

    /// The table lands on the handler's own function row, and a table whose
    /// handler the function index never saw is not lost — it stays in the
    /// array, which is the record.
    #[test]
    fn a_table_is_stamped_onto_its_handlers_function_row() {
        use crate::visitor::{FunctionDefinition, FunctionNodeType};

        let definition = |name: &str, file: &str, line: u32| FunctionDefinition {
            name: name.to_string(),
            file_path: std::path::PathBuf::from(file),
            node_type: FunctionNodeType::Placeholder,
            arguments: vec![],
            body_source: None,
            is_exported: true,
            line_number: line,
            end_line: line + 10,
            intent: None,
            calls: vec![],
            return_type: None,
            return_is_explicit: false,
            signature: None,
            tokens: vec![],
            intent_input_hash: None,
            dispatch_table: None,
        };
        let mut functions = std::collections::HashMap::from([
            (
                "handler".to_string(),
                definition("handler", "lambdas/check-or-upload/index.ts", 419),
            ),
            (
                "other".to_string(),
                definition("handler", "lambdas/mcp-server/src/lambda.ts", 12),
            ),
        ]);

        let stamped = stamp_dispatch_tables_on_functions(
            &mut functions,
            &[table(&["upload-logs"]), {
                let mut orphan = table(&["x"]);
                orphan.handler_name = Some("nobody".to_string());
                orphan
            }],
        );

        assert_eq!(stamped, 1, "the handler in the right file, and only it");
        assert!(functions["handler"].dispatch_table.is_some());
        assert!(
            functions["other"].dispatch_table.is_none(),
            "same name, different file: not this handler"
        );
    }

    #[test]
    fn a_table_that_states_nothing_is_dropped() {
        let empty_values = DispatchTable {
            location: DispatchLocation::Body,
            field: "action".to_string(),
            values: vec![" ".to_string()],
            handler_name: None,
            file_path: String::new(),
            line_number: None,
            service_name: None,
            repo_name: None,
        };
        assert!(empty_values.normalized().is_none());

        let dupes = DispatchTable {
            location: DispatchLocation::Body,
            field: " action ".to_string(),
            values: vec!["a".to_string(), "a".to_string(), "b".to_string()],
            handler_name: None,
            file_path: String::new(),
            line_number: None,
            service_name: None,
            repo_name: None,
        };
        let kept = dupes.normalized().expect("a table with values is kept");
        assert_eq!(kept.field, "action");
        assert_eq!(kept.values, vec!["a".to_string(), "b".to_string()]);
    }
}
