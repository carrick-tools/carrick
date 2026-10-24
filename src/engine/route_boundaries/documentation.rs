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

/// The repo-relative directory of a service, from its serialized
/// `carrick.json` entry: `None` for the repository root, which owns every
/// file.
fn directory(service: &ServiceRoutes<'_>) -> Option<String> {
    let json: serde_json::Value =
        serde_json::from_str(service.data.config_json.as_deref()?).ok()?;
    let directory = json.get("directory")?.as_str()?;
    let directory = directory.trim_start_matches("./").trim_end_matches('/');
    (!directory.is_empty() && directory != ".").then(|| directory.to_string())
}

/// The file and line of a graph row's `file_location`, repo-relative.
fn place(location: &str, repo_path: &str) -> Option<(String, u32)> {
    let (file, line) = super::file_and_line(location)?;
    let normal = super::super::normalize_protocol_file(Path::new(file));
    let root = super::super::normalize_protocol_file(Path::new(repo_path));
    let relative = normal.strip_prefix(&root).unwrap_or(&normal);
    let relative = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Some((relative, line))
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
    let roots: Vec<Option<String>> = services.iter().map(directory).collect();
    let owns = |service: usize, file: &str| match &roots[service] {
        None => true,
        Some(root) => Path::new(file).starts_with(root),
    };
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
            // Another row of its own service serving the operation is the
            // in-service fold's to settle.
            if graph.endpoints.iter().any(same_operation) {
                continue;
            }
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
                    let at = place(&partner.file_location, repo_path);
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
