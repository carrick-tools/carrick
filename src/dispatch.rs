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
