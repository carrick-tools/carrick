//! The documentation fold across services (carrick#2474 R5d).
//!
//! A service that documents another service's route as data (a bare
//! `descriptor_route` row of its own) states an operation whose handler row
//! the other service holds. In one service the fold drops the documentation
//! row and labels its handler row `descriptor_route`
//! ([`FileOrchestrator::fold_documented_operations`]). Across services the
//! same fold stands only where the join point has evidence the two describe
//! one operation: the handler row's route was composed through a scope or
//! register call written in the documenting service's files, so that
//! service's code is what gave the route its path. Anything short of that
//! keeps both rows. Matching does not change.
//!
//! The fold keeps the in-service fold's rule about types: the documentation
//! row's types go with it, so the handler row must hold types of its own at
//! its site. A handler row written untyped by [`super::settle`] leaves the
//! documentation in place.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::agents::file_analyzer_agent::ResolutionSource;
use crate::agents::file_orchestrator::FileOrchestrator;
use crate::mount_graph::{MountGraph, ResolvedEndpoint};
use crate::operation::OperationKey;

use super::{ServiceRoutes, flat_row, manifest_of};

/// A route a service's boundary facts resolved to full paths, with the
/// files of the calls it was composed through.
pub(super) struct Composed {
    pub(super) service: usize,
    /// Repo-relative.
    pub(super) file: String,
    pub(super) line: u32,
    pub(super) method: String,
    pub(super) scope_files: BTreeMap<String, BTreeSet<String>>,
}

/// The directory of a service as path segments, from its serialized
/// `carrick.json` entry: empty for the repository root, `None` for a
/// service whose entry is missing or unreadable (it owns no file).
fn directory(service: &ServiceRoutes<'_>) -> Option<Vec<String>> {
    let json: serde_json::Value =
        serde_json::from_str(service.data.config_json.as_deref()?).ok()?;
    let directory = json.get("directory").and_then(serde_json::Value::as_str);
    Some(
        crate::config::directory_segments(directory)
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )
}

/// Whether the service at `service` owns the repo-relative `file`: the
/// file sits under its directory and under no other service's directory
/// that is strictly longer, the rule `Config::resolve_nested` gives a
/// nested service's files (carrick#553).
fn owns(directories: &[Option<Vec<String>>], service: usize, file: &str) -> bool {
    let Some(own) = &directories[service] else {
        return false;
    };
    let file = crate::config::directory_segments(Some(file));
    let under = |directory: &Vec<String>| {
        file.len() >= directory.len()
            && directory
                .iter()
                .zip(&file)
                .all(|(segment, part)| segment == part)
    };
    under(own)
        && !directories
            .iter()
            .flatten()
            .any(|other| other.len() > own.len() && under(other))
}

/// Where one documentation row's fold goes: the service and graph row of
/// its handler row, or why it does not fold.
enum Fold {
    Onto { service: usize, row: usize },
    Refused,
}

/// Fold each documentation row of a writable service onto the handler row
/// of another that the documenting service's scope call composed, and count
/// the documentation rows that have such a handler row and do not fold
/// (`routes_documented_across_unfolded`, on the documenting service).
pub(super) fn fold(
    services: &mut [ServiceRoutes<'_>],
    routes: &[Composed],
    repo_path: &str,
    counts: &mut [BTreeMap<String, usize>],
) {
    if routes.is_empty() {
        return;
    }
    let directories: Vec<Option<Vec<String>>> = services.iter().map(directory).collect();
    let owns = |service: usize, file: &str| owns(&directories, service, file);
    let repo_root = super::super::normalize_protocol_file(Path::new(repo_path));
    let mut taken: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut folds: Vec<(usize, usize, Fold)> = Vec::new();
    for (x, documenting) in services.iter().enumerate() {
        if !documenting.writable {
            continue;
        }
        let Some(graph) = documenting.data.mount_graph.as_ref() else {
            continue;
        };
        for (index, doc) in graph.endpoints.iter().enumerate() {
            if !FileOrchestrator::is_documentation_row(doc) {
                continue;
            }
            let same_operation = |row: &ResolvedEndpoint| {
                row.resolution_source != Some(ResolutionSource::DescriptorRoute)
                    && row.method == doc.method
                    && MountGraph::paths_equal_modulo_param_names(&row.full_path, &doc.full_path)
                    && row.dispatch == doc.dispatch
            };
            // The routes another service resolved to this operation through
            // a call written in this service's files.
            let composed: Vec<&Composed> = routes
                .iter()
                .filter(|route| {
                    route.service != x
                        && route.method.eq_ignore_ascii_case(&doc.method)
                        && route.scope_files.iter().any(|(path, files)| {
                            MountGraph::paths_equal_modulo_param_names(path, &doc.full_path)
                                && files.iter().any(|file| owns(x, file))
                        })
                })
                .collect();
            if composed.is_empty() {
                continue;
            }
            // Another row of its own service serving the operation is the
            // in-service fold's to settle: the documentation row stays here,
            // and counts.
            if graph.endpoints.iter().any(same_operation) {
                folds.push((x, index, Fold::Refused));
                continue;
            }
            let partners: Vec<(usize, usize)> = services
                .iter()
                .enumerate()
                .filter(|(y, _)| *y != x)
                .flat_map(|(y, other)| {
                    other
                        .data
                        .mount_graph
                        .iter()
                        .flat_map(|graph| graph.endpoints.iter().enumerate())
                        .filter(|(_, row)| same_operation(row))
                        .map(move |(row, _)| (y, row))
                })
                .collect();
            let fold = match partners.as_slice() {
                [(y, row)] if !taken.contains(&(*y, *row)) && services[*y].writable => {
                    let partner = &services[*y].data.mount_graph.as_ref().unwrap().endpoints[*row];
                    let at = super::super::row_place(&partner.file_location, &repo_root)
                        .map(|(_, relative, line)| (relative, line));
                    let at_route = composed.iter().any(|route| {
                        route.service == *y && at == Some((route.file.clone(), route.line))
                    });
                    let typed = services[*y]
                        .data
                        .type_manifest
                        .iter()
                        .flatten()
                        .any(|entry| manifest_of(entry, partner, &partner.full_path));
                    if at_route && typed {
                        taken.insert((*y, *row));
                        Fold::Onto {
                            service: *y,
                            row: *row,
                        }
                    } else {
                        Fold::Refused
                    }
                }
                _ => Fold::Refused,
            };
            folds.push((x, index, fold));
        }
    }
    let mut dropped: Vec<Vec<usize>> = vec![Vec::new(); services.len()];
    for (x, index, fold) in folds {
        match fold {
            Fold::Refused => {
                *counts[x]
                    .entry("routes_documented_across_unfolded".to_string())
                    .or_default() += 1;
            }
            Fold::Onto { service, row } => {
                let partner = &mut services[service];
                let CloudRepoDataParts {
                    graph, endpoints, ..
                } = parts(partner);
                graph.endpoints[row].resolution_source = Some(ResolutionSource::DescriptorRoute);
                let partner_row = graph.endpoints[row].clone();
                if let Some(flat) = flat_row(endpoints, &partner_row, &partner_row.full_path) {
                    flat.resolution_source = Some(ResolutionSource::DescriptorRoute);
                }
                dropped[x].push(index);
            }
        }
    }
    for (x, indices) in dropped.into_iter().enumerate() {
        if indices.is_empty() {
            continue;
        }
        let CloudRepoDataParts {
            graph,
            endpoints,
            manifest,
        } = parts(&mut services[x]);
        let rows: Vec<ResolvedEndpoint> = indices
            .iter()
            .map(|index| graph.endpoints[*index].clone())
            .collect();
        for row in &rows {
            let key = OperationKey::http(&row.method, row.full_path.clone());
            let file_path = Path::new(&row.file_location);
            endpoints.retain(|endpoint| {
                !(endpoint.key == key
                    && endpoint.dispatch == row.dispatch
                    && endpoint.file_path == file_path)
            });
            if let Some(manifest) = manifest.as_mut() {
                manifest.retain(|entry| !manifest_of(entry, row, &row.full_path));
            }
        }
        let mut index = 0;
        graph.endpoints.retain(|_| {
            let keep = !indices.contains(&index);
            index += 1;
            keep
        });
    }
}

/// The parts of a service's data a fold writes.
struct CloudRepoDataParts<'d> {
    graph: &'d mut MountGraph,
    endpoints: &'d mut Vec<crate::analyzer::ApiEndpointDetails>,
    manifest: &'d mut Option<Vec<crate::cloud_storage::TypeManifestEntry>>,
}

fn parts<'d>(service: &'d mut ServiceRoutes<'_>) -> CloudRepoDataParts<'d> {
    let data = &mut *service.data;
    CloudRepoDataParts {
        graph: data
            .mount_graph
            .as_mut()
            .expect("a service folded into or from has a graph"),
        endpoints: &mut data.endpoints,
        manifest: &mut data.type_manifest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_storage::{
        CloudRepoData, ManifestRole, ManifestTypeKind, ManifestTypeState, TypeEvidence,
        TypeManifestEntry,
    };
    use crate::request_summary::decided_sites::RouteBoundaries;
    use crate::services::type_sidecar::InferKind;

    fn row(location: &str, handler: &str, source: Option<ResolutionSource>) -> ResolvedEndpoint {
        ResolvedEndpoint {
            procedure: None,
            view_module: false,
            method: "GET".to_string(),
            path: "/users".to_string(),
            full_path: "/api/users".to_string(),
            handler: Some(handler.to_string()),
            owner: "router".to_string(),
            file_location: location.to_string(),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
            resolution_source: source,
            dispatch: None,
            handler_span: None,
            address: None,
        }
    }

    fn producer_entry(file: &str, line: u32) -> TypeManifestEntry {
        TypeManifestEntry {
            key: OperationKey::http("GET", "/api/users"),
            role: ManifestRole::Producer,
            type_kind: ManifestTypeKind::Response,
            type_alias: "Users".to_string(),
            file_path: file.to_string(),
            line_number: line,
            is_explicit: true,
            type_state: ManifestTypeState::Explicit,
            evidence: TypeEvidence {
                file_path: file.to_string(),
                span_start: None,
                span_end: None,
                expression_text: None,
                expression_line: None,
                line_number: line,
                infer_kind: InferKind::FunctionReturn,
                is_explicit: true,
                type_state: ManifestTypeState::Explicit,
            },
            resolved_definition: Some("User[]".to_string()),
            expanded_definition: Some("User[]".to_string()),
            primary_type_symbol: None,
            defined_in: None,
            any_provenance: Vec::new(),
            unwidened_definition: None,
            unwidened_out_of_budget: false,
            response_modes: None,
            stated_values: Default::default(),
            v1_state_before_demotion: None,
        }
    }

    fn data(
        config_json: &str,
        rows: Vec<ResolvedEndpoint>,
        manifest: Vec<TypeManifestEntry>,
    ) -> CloudRepoData {
        let mut graph = MountGraph::new();
        graph.endpoints = rows;
        let mut data: CloudRepoData = serde_json::from_value(serde_json::json!({
            "repo_name": "repo",
            "endpoints": [],
            "calls": [],
            "mounts": [],
            "apps": {},
            "imported_handlers": [],
            "function_definitions": {},
            "config_json": config_json,
            "package_json": null,
            "packages": null,
            "last_updated": "2026-10-24T00:00:00Z",
            "commit_hash": "c0ffee",
        }))
        .expect("a minimal blob");
        data.mount_graph = Some(graph);
        data.type_manifest = Some(manifest);
        data
    }

    /// Fold a repository-root service's documentation row of `GET
    /// /api/users` against the `services/api` service's typed handler row,
    /// composed through a scope call in `scope_file`, with `services/web`
    /// beside them. Whether the documentation row folded.
    fn root_folds_through(scope_file: &str) -> bool {
        let (folded, unfolded) = fold_root(scope_file, false);
        assert_eq!(unfolded, 0, "no handler row composed through its files");
        folded
    }

    /// [`root_folds_through`], with the root serving the operation itself
    /// at `src/users.ts` when `root_serves_it`: whether the documentation
    /// row folded, and the root's `routes_documented_across_unfolded`.
    fn fold_root(scope_file: &str, root_serves_it: bool) -> (bool, usize) {
        let boundaries = RouteBoundaries::default();
        let mut root_rows = vec![row(
            "src/docs.ts:3",
            "__route_descriptor__",
            Some(ResolutionSource::DescriptorRoute),
        )];
        if root_serves_it {
            root_rows.push(row("src/users.ts:7", "list", None));
        }
        let mut root = data(r#"{"serviceName":"root"}"#, root_rows, vec![]);
        let mut api = data(
            r#"{"serviceName":"api","directory":"./services/api"}"#,
            vec![row("services/api/routes.ts:10", "list", None)],
            vec![producer_entry("services/api/routes.ts", 10)],
        );
        let mut web = data(
            r#"{"serviceName":"web","directory":"services/web/"}"#,
            vec![],
            vec![],
        );
        let routes = vec![Composed {
            service: 1,
            file: "services/api/routes.ts".to_string(),
            line: 10,
            method: "GET".to_string(),
            scope_files: BTreeMap::from([(
                "/api/users".to_string(),
                BTreeSet::from([scope_file.to_string()]),
            )]),
        }];
        let mut services = vec![
            ServiceRoutes {
                data: &mut root,
                boundaries: &boundaries,
                writable: true,
            },
            ServiceRoutes {
                data: &mut api,
                boundaries: &boundaries,
                writable: true,
            },
            ServiceRoutes {
                data: &mut web,
                boundaries: &boundaries,
                writable: true,
            },
        ];
        let mut counts = vec![BTreeMap::new(); 3];
        fold(&mut services, &routes, "/repo", &mut counts);
        let documentation_left = root
            .mount_graph
            .as_ref()
            .unwrap()
            .endpoints
            .iter()
            .filter(|row| FileOrchestrator::is_documentation_row(row))
            .count();
        let handler = &api.mount_graph.as_ref().unwrap().endpoints[0];
        let relabelled = handler.resolution_source == Some(ResolutionSource::DescriptorRoute);
        assert_eq!(
            documentation_left == 0,
            relabelled,
            "the documentation row goes exactly when its handler row is relabelled"
        );
        let unfolded = counts[0]
            .get("routes_documented_across_unfolded")
            .copied()
            .unwrap_or_default();
        (relabelled, unfolded)
    }

    /// PR #2750 review: a documentation row whose own service also serves
    /// the operation is the in-service fold's, and stays; where another
    /// service's handler row was composed through its files it counts as
    /// unfolded rather than leaving silently.
    #[test]
    fn a_documentation_row_its_own_service_serves_counts_as_unfolded() {
        assert_eq!(fold_root("src/plugin.ts", true), (false, 1));
        assert_eq!(fold_root("services/api/plugin.ts", true), (false, 0));
    }

    /// PR #2750 review: the repository root does not own the files under a
    /// nested service's directory (`Config::resolve_nested`, carrick#553),
    /// so a scope call written in the partner's files, or a third
    /// service's, is no evidence the root's code composed the route. A
    /// call in the root's own files is.
    #[test]
    fn a_root_service_folds_only_through_a_call_in_its_own_files() {
        assert!(root_folds_through("src/plugin.ts"));
        assert!(!root_folds_through("services/api/plugin.ts"));
        assert!(!root_folds_through("services/web/scope.ts"));
    }
}
