//! Routes whose chains cross services, resolved and written at the join
//! point (carrick#2474 R5b).
//!
//! Each service records where its route chains leave it during its own
//! analysis ([`RouteBoundaries`], carrick#2474 R5a). Once every service is
//! in hand, [`settle`] reads those records across services
//! ([`boundaries::resolve`]), and writes each route that resolves to a full
//! path into the service that recorded it, with the writer one service's
//! traced routes use ([`super::write_traced_routes`]): a row that agrees is
//! relabelled `library_claim`, a row at the route's own path is corrected
//! (row 4), any other row stands and is counted, and a route no row holds
//! is added. The payload was already built from the graph, so its flat
//! endpoint list and producer manifest entries follow every row the writer
//! and the documentation fold touch: a corrected row's manifest entries
//! take its new key and keep their alias, which is what the cloud joins
//! them on (`get-types.ts`: the endpoint's own persisted key).
//!
//! The prefix a receiving service's scope states was read in that service
//! (2026-10-14 ruling 1): nothing here fills it from another's arguments.
//! A route served by several hosts takes the paths the hosts that state one
//! give, and each host whose path is not read is counted (carrick#2474 Q5,
//! ruled (ii) by David on 2026-10-23).

use std::collections::BTreeMap;
use std::path::Path;

use tracing::debug;

use crate::agents::file_analyzer_agent::ResolutionSource;
use crate::agents::file_orchestrator::FileOrchestrator;
use crate::cloud_storage::{CloudRepoData, ManifestRole};
use crate::mount_graph::ResolvedEndpoint;
use crate::operation::OperationKey;
use crate::request_summary::decided_sites::RouteBoundaries;
use crate::request_summary::decided_sites::boundaries::{self, FAN_OUT};
use crate::site_rows::TracedRoute;
use crate::type_manifest::normalize_manifest_method;

/// One service at the join point.
pub(super) struct ServiceRoutes<'a> {
    pub(super) data: &'a mut CloudRepoData,
    /// Where its route chains leave it.
    pub(super) boundaries: &'a RouteBoundaries,
    /// Analysed in this run, and so written. A held-back service is read for
    /// its boundaries and never changed. A carried one is never changed and
    /// has no boundaries until carrick#2474 R5c: on `--changed-only`, a
    /// route whose boundary only a carried service fills counts
    /// `routes_across_untraced_unmatched_boundary` (or, where its site also
    /// serves it in its own service, keeps that path and counts the
    /// boundary as an unread host).
    pub(super) writable: bool,
}

/// Resolve every service's route segments across `services` and write
/// those that resolve. Returns each service's fallback counts, by index:
/// `routes_across_{relabelled,corrected,added_untyped}`, the hosts written
/// around (`routes_across_unread_host`), the segments that do not resolve
/// (`routes_across_untraced_<reason>`), and the sites where a row disagrees
/// (`routes_trace_disagrees`, `routes_documented_conflict`).
pub(super) fn settle(
    services: &mut [ServiceRoutes<'_>],
    repo_path: &str,
) -> Vec<BTreeMap<String, usize>> {
    let mut counts: Vec<BTreeMap<String, usize>> = vec![BTreeMap::new(); services.len()];
    if services
        .iter()
        .all(|service| service.boundaries.routes.is_empty())
    {
        return counts;
    }
    let names: Vec<String> = (0..services.len()).map(|index| index.to_string()).collect();
    let facts: Vec<(&str, &RouteBoundaries)> = names
        .iter()
        .zip(services.iter())
        .map(|(name, service)| (name.as_str(), service.boundaries))
        .collect();
    let mut traced: Vec<Vec<TracedRoute>> = vec![Vec::new(); services.len()];
    for resolved in boundaries::resolve(&facts, FAN_OUT) {
        let Ok(index) = resolved.service.parse::<usize>() else {
            continue;
        };
        let count = &mut counts[index];
        if resolved.unread_hosts > 0 && resolved.untraced.is_none() {
            *count
                .entry("routes_across_unread_host".to_string())
                .or_default() += resolved.unread_hosts;
        }
        if let Some(reason) = &resolved.untraced {
            *count
                .entry(format!("routes_across_untraced_{reason}"))
                .or_default() += 1;
            continue;
        }
        traced[index].push(TracedRoute {
            file: resolved.file,
            line: resolved.line,
            method: resolved.method,
            path: resolved.path,
            full_paths: resolved.paths,
            handler: resolved.handler,
            owner: resolved.owner,
            corrects: true,
        });
    }
    for ((service, routes), count) in services.iter_mut().zip(traced).zip(counts.iter_mut()) {
        if !service.writable || routes.is_empty() {
            continue;
        }
        let applied = write(service.data, &routes, repo_path);
        for (key, value) in [
            ("routes_across_relabelled", applied.relabelled),
            ("routes_across_corrected", applied.corrected),
            ("routes_across_added_untyped", applied.added),
            ("routes_across_owner_unknown", applied.owner_unknown),
            ("routes_trace_disagrees", applied.disagrees),
            ("routes_documented_conflict", applied.documented_conflict),
            ("routes_across_twin", applied.twin),
        ] {
            if value > 0 {
                *count.entry(key.to_string()).or_default() += value;
            }
        }
        debug!(
            service = service.data.service_name.as_deref().unwrap_or("(root)"),
            relabelled = applied.relabelled,
            corrected = applied.corrected,
            added = applied.added,
            disagrees = applied.disagrees,
            refused = applied.refused,
            folded = applied.folded,
            "receiver trace: routes across services written at the join point"
        );
    }
    counts
}

/// The file and line of a graph row's `file_location`.
fn file_and_line(location: &str) -> Option<(&str, u32)> {
    let (file, line) = location.rsplit_once(':')?;
    Some((file, line.parse().ok()?))
}

/// The flat endpoint the payload projected from `row` while it stated
/// `full_path`.
fn flat_row<'d>(
    endpoints: &'d mut [crate::analyzer::ApiEndpointDetails],
    row: &ResolvedEndpoint,
    full_path: &str,
) -> Option<&'d mut crate::analyzer::ApiEndpointDetails> {
    let key = OperationKey::http(&row.method, full_path.to_string());
    endpoints.iter_mut().find(|endpoint| {
        endpoint.key == key
            && endpoint.dispatch == row.dispatch
            && endpoint.file_path == Path::new(&row.file_location)
    })
}

/// Whether an endpoint row written at `file:line` holds the possible call
/// `call`: a call at that line whose role is not `consumer`.
fn held_by_endpoint(call: &crate::possible_calls::PossibleCall, file: &str, line: u32) -> bool {
    call.file_path == file && call.line_number == line && call.role.as_deref() != Some("consumer")
}

/// Whether `entry` is a producer manifest entry of `row` while it stated
/// `full_path`.
fn manifest_of(
    entry: &crate::cloud_storage::TypeManifestEntry,
    row: &ResolvedEndpoint,
    full_path: &str,
) -> bool {
    let Some((file, line)) = file_and_line(&row.file_location) else {
        return false;
    };
    let method = normalize_manifest_method(&row.method);
    entry.role == ManifestRole::Producer
        && entry.file_path == file
        && entry.line_number == line
        && entry
            .key
            .as_http()
            .is_some_and(|(entry_method, entry_path)| {
                entry_method == method && entry_path == full_path
            })
}

/// Write `traced` into `data`'s graph with the shared writer, then make its
/// flat endpoints, producer manifest entries and possible calls follow every
/// row the writer and the documentation fold touched.
fn write(
    data: &mut CloudRepoData,
    traced: &[TracedRoute],
    repo_path: &str,
) -> super::TracedApplied {
    let CloudRepoData {
        mount_graph,
        endpoints,
        type_manifest,
        possible_calls,
        ..
    } = data;
    let Some(graph) = mount_graph.as_mut() else {
        return super::TracedApplied::default();
    };
    let mut write = super::write_traced_routes(graph, traced, repo_path);
    for &index in &write.relabelled {
        let row = &graph.endpoints[index];
        if let Some(flat) = flat_row(endpoints, row, &row.full_path) {
            flat.resolution_source = row.resolution_source;
        }
    }
    for (index, before) in &write.corrected {
        let row = &graph.endpoints[*index];
        if let Some(flat) = flat_row(endpoints, row, before) {
            flat.key = OperationKey::http(&row.method, row.full_path.clone());
            flat.resolution_source = row.resolution_source;
        }
        // The key changes and the alias stays, as the call-through re-key
        // does: the alias names the site the types were read at. Entries
        // name a row only by file, line and key, so a twin the writer left
        // at the old path (`routes_across_twin`) gives its entries up too.
        let key = OperationKey::http(
            &normalize_manifest_method(&row.method),
            row.full_path.clone(),
        );
        for entry in type_manifest
            .iter_mut()
            .flatten()
            .filter(|entry| manifest_of(entry, row, before))
        {
            entry.key = key.clone();
        }
    }
    for index in write.added.clone() {
        let row = &graph.endpoints[index];
        endpoints.push(crate::cloud_storage::endpoint_details(row));
        // A possible call at the line now has a row, unless it is a
        // consumer's: an endpoint row is no call row (the in-service rule,
        // `site_rows::fold_rows`).
        if let (Some(calls), Some((file, line))) =
            (possible_calls.as_mut(), file_and_line(&row.file_location))
        {
            calls.retain(|call| !held_by_endpoint(call, file, line));
        }
    }
    if write.applied.added > 0 || write.applied.corrected > 0 {
        // A row added here has no type anchor this scan: documentation that
        // states its operation keeps its own row, and its types.
        let added = write.added.clone();
        let folds = FileOrchestrator::documented_folds_onto(graph, |index| !added.contains(&index));
        for (index, row) in graph.endpoints.iter().enumerate() {
            if folds.folded_into[index]
                && let Some(flat) = flat_row(endpoints, row, &row.full_path)
            {
                flat.resolution_source = Some(ResolutionSource::DescriptorRoute);
            }
            if folds.drop[index] {
                let key = OperationKey::http(&row.method, row.full_path.clone());
                let file_path = Path::new(&row.file_location);
                endpoints.retain(|endpoint| {
                    !(endpoint.key == key
                        && endpoint.dispatch == row.dispatch
                        && endpoint.file_path == file_path)
                });
                if let Some(manifest) = type_manifest.as_mut() {
                    manifest.retain(|entry| !manifest_of(entry, row, &row.full_path));
                }
            }
        }
        write.applied.folded = FileOrchestrator::apply_documented_folds(graph, &folds);
    }
    write.applied
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::possible_calls::{PossibleCall, Unread, UnreadReason};

    fn call(line: u32, role: Option<&str>) -> PossibleCall {
        PossibleCall {
            file_path: "src/a.ts".to_string(),
            line_number: line,
            span_start: 0,
            span_end: 1,
            kind: Some("http".to_string()),
            role: role.map(str::to_string),
            labelled_end: None,
            unread: Unread {
                slot: "path".to_string(),
                reason: UnreadReason::NameNeverBound,
            },
            trace_depth: 0,
        }
    }

    /// PR #2734 review: a row added at a line holds the possible calls of
    /// a producer, or of no role, there; a consumer's call at that line is
    /// no endpoint's and stays, as in one service (`site_rows::fold_rows`).
    #[test]
    fn an_added_row_keeps_a_consumers_possible_call_at_its_line() {
        assert!(held_by_endpoint(&call(4, Some("producer")), "src/a.ts", 4));
        assert!(held_by_endpoint(&call(4, None), "src/a.ts", 4));
        assert!(!held_by_endpoint(&call(4, Some("consumer")), "src/a.ts", 4));
        assert!(!held_by_endpoint(&call(5, Some("producer")), "src/a.ts", 4));
    }
}
