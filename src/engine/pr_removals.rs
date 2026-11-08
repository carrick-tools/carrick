//! What a pull request removed that main's copy of this repo states, and the
//! pairings main had, for marking the rows of a pairing the PR lost
//! (carrick#3092, S-B; the marking itself is
//! [`crate::pr_baseline::mark_lost_pairings`]).
//!
//! Two lists, each compared by key with this run's services and kept apart
//! from the displayed delta, which stays as it is:
//!
//! - removed operations: main endpoint rows whose `(service, delta_identity)`
//!   this run lacks, compared as `delta_candidates` compares them;
//! - removed sends: main socket and pub/sub call rows whose
//!   `(service, key, file)` this run lacks. A send is a call row, so the
//!   delta, which reads endpoints, never sees one go.
//!
//! Each list goes through the delta's diff anchor once
//! ([`crate::pr_delta_anchor::run`]): a removal it lists is one the PR's own
//! diff places. A run with no base, or a diff git cannot read, places none,
//! and the marking reads that as doubt.

use std::collections::{HashMap, HashSet};

use tracing::info;

use crate::agents::file_analyzer_agent::ResolutionSource;
use crate::analyzer::{CrossRepoMatch, location_file};
use crate::cloud_storage::CloudRepoData;
use crate::config::Config;
use crate::operation::{OperationKey, Protocol};
use crate::pr_baseline::{MainConsumer, MainPairing, MainProducer, RemovedOperation, RemovedSend};
use crate::pr_delta_anchor::{Candidate, MainSource, Place};

use super::{DeltaRow, ServiceEndpointKey, delta_identity, keep_first_spelling};

/// The key a removed operation and a pairing's producer are compared by: the
/// delta's identity, so a route only re-spelled is no removal (carrick#3079).
fn operation_identity(key: &OperationKey) -> String {
    delta_identity(key).canonical()
}

fn is_message_protocol(key: &OperationKey) -> bool {
    matches!(key.protocol(), Protocol::Websocket | Protocol::Pubsub)
}

/// The id a row's service is named by in a match: `service_name`, else the
/// repo.
fn service_id(service_name: &Option<String>, repo_name: &str) -> String {
    service_name
        .clone()
        .unwrap_or_else(|| repo_name.to_string())
}

/// What the PR removed of main's copy, each removal placed or not.
#[derive(Debug, Default)]
pub(super) struct Removals {
    pub(super) operations: Vec<RemovedOperation>,
    pub(super) sends: Vec<RemovedSend>,
}

impl Removals {
    pub(super) fn is_empty(&self) -> bool {
        self.operations.is_empty() && self.sends.is_empty()
    }

    /// How many removals the diff placed, for the log line a failed main
    /// side writes.
    pub(super) fn placed(&self) -> usize {
        self.operations.iter().filter(|op| op.placed).count()
            + self.sends.iter().filter(|send| send.placed).count()
    }

    /// The removals of `main_self` (main's copy of this repo) that `current`
    /// (this run's services) no longer states, placed through the diff from
    /// `base` when the run knows it.
    pub(super) fn of(
        repo_path: &str,
        base: Option<&str>,
        main_self: &[CloudRepoData],
        current: &[CloudRepoData],
        services: &[Config],
        changes: Option<&crate::change_scope::ChangedSince>,
    ) -> Self {
        let mut operations = removed_operations(main_self, current);
        let mut sends = removed_sends(main_self, current);
        let Some(base) = base else {
            if !operations.is_empty() || !sends.is_empty() {
                info!(
                    "This run does not know the PR's base commit, so no removal it finds is \
                     placed in the PR's diff"
                );
            }
            return Removals {
                operations: operations.into_iter().flat_map(|op| op.rows).collect(),
                sends: sends.into_iter().flat_map(|send| send.rows).collect(),
            };
        };

        let endpoint_places: Vec<Place> = current
            .iter()
            .flat_map(|data| {
                data.endpoints
                    .iter()
                    .map(|row| Place::parse(&data.service_name, &row.file_path.to_string_lossy()))
            })
            .collect();
        let call_places: Vec<Place> = current
            .iter()
            .flat_map(|data| {
                data.calls
                    .iter()
                    .filter(|row| is_message_protocol(&row.key))
                    .map(|row| Place::parse(&data.service_name, &row.file_path.to_string_lossy()))
            })
            .collect();
        let main_rows = |calls: bool| -> Vec<MainSource> {
            main_self
                .iter()
                .flat_map(|repo| {
                    let rows = if calls { &repo.calls } else { &repo.endpoints };
                    rows.iter().map(|row| MainSource {
                        place: Place::parse(&repo.service_name, &row.file_path.to_string_lossy()),
                        commit: repo.commit_hash.clone(),
                    })
                })
                .collect()
        };
        place(
            repo_path,
            base,
            &mut operations,
            &endpoint_places,
            main_rows(false),
            "operation",
            services,
            changes,
        );
        place(
            repo_path,
            base,
            &mut sends,
            &call_places,
            main_rows(true),
            "send",
            services,
            changes,
        );
        Removals {
            operations: operations.into_iter().flat_map(|op| op.rows).collect(),
            sends: sends.into_iter().flat_map(|send| send.rows).collect(),
        }
    }
}

/// One key main states and this run lacks: every row of it on main, as the
/// removal rows the marking reads, and the places the anchor judges.
struct Removed<R> {
    rows: Vec<R>,
    places: Vec<Place>,
}

trait Placeable {
    fn set_placed(&mut self, placed: bool);
}

impl Placeable for RemovedOperation {
    fn set_placed(&mut self, placed: bool) {
        self.placed = placed;
    }
}

impl Placeable for RemovedSend {
    fn set_placed(&mut self, placed: bool) {
        self.placed = placed;
    }
}

/// Mark each of `removed` placed when the PR's diff lists it.
#[allow(clippy::too_many_arguments)]
fn place<R: Placeable>(
    repo_path: &str,
    base: &str,
    removed: &mut [Removed<R>],
    scan: &[Place],
    main: Vec<MainSource>,
    what: &str,
    services: &[Config],
    changes: Option<&crate::change_scope::ChangedSince>,
) {
    if removed.is_empty() {
        return;
    }
    let candidates: Vec<Candidate<usize>> = removed
        .iter()
        .enumerate()
        .map(|(index, removal)| Candidate {
            item: index,
            places: removal.places.clone(),
        })
        .collect();
    let reading_moved = |merge_base: &str| {
        crate::change_scope::reading_moved_services(repo_path, services, changes, merge_base)
    };
    match crate::pr_delta_anchor::run(
        repo_path,
        base,
        Vec::new(),
        candidates,
        scan,
        main,
        reading_moved,
        &[],
    ) {
        Ok(anchored) => {
            let listed: HashSet<usize> = anchored.removed.into_iter().collect();
            info!(
                "Removed {what}s against main: {} placed in the PR's diff, {} not",
                listed.len(),
                removed.len() - listed.len()
            );
            for (index, removal) in removed.iter_mut().enumerate() {
                for row in &mut removal.rows {
                    row.set_placed(listed.contains(&index));
                }
            }
        }
        Err(reason) => info!(
            "Git could not read this PR's diff ({}), so no removed {what} is placed in it",
            reason.trim()
        ),
    }
}

fn removed_operations(
    main_self: &[CloudRepoData],
    current: &[CloudRepoData],
) -> Vec<Removed<RemovedOperation>> {
    let is_documentation = |row: &crate::analyzer::ApiEndpointDetails| {
        crate::agents::FileOrchestrator::is_documentation_route(
            row.resolution_source,
            row.handler_name.as_deref(),
        )
    };
    let keys_of = |handlers_only: bool| -> HashSet<ServiceEndpointKey> {
        current
            .iter()
            .flat_map(|data| {
                data.endpoints
                    .iter()
                    .filter(move |row| !(handlers_only && is_documentation(row)))
                    .map(|row| (data.service_name.clone(), delta_identity(&row.key)))
            })
            .collect()
    };
    let current_keys = keys_of(false);
    // A documentation row keeps the operation it states alive only for a row
    // that documentation alone ever stated: a main row with a real handler
    // is removed when no current row with a real handler shares its key, so
    // a renamed path its documentation still states is seen to go (carrick#3252;
    // `check --changed` reads the same rule).
    let current_handler_keys = keys_of(true);
    // Main's spelling of each key, as the delta keeps it, beside each row.
    let mut spelled: HashMap<ServiceEndpointKey, DeltaRow> = HashMap::new();
    let mut rows: HashMap<ServiceEndpointKey, Vec<(String, String, Option<ResolutionSource>)>> =
        HashMap::new();
    for repo in main_self {
        for row in &repo.endpoints {
            let id = (repo.service_name.clone(), delta_identity(&row.key));
            let still_there = if is_documentation(row) {
                &current_keys
            } else {
                &current_handler_keys
            };
            if still_there.contains(&id) {
                continue;
            }
            keep_first_spelling(&mut spelled, &repo.service_name, row, &repo.commit_hash);
            rows.entry(id).or_default().push((
                service_id(&repo.service_name, &repo.repo_name),
                row.file_path.to_string_lossy().into_owned(),
                row.resolution_source,
            ));
        }
    }
    let mut removed: Vec<Removed<RemovedOperation>> = rows
        .into_iter()
        .map(|(id, rows)| {
            let (method, path) = spelled[&id].key.display_labels();
            let key = operation_identity(&id.1);
            Removed {
                places: rows
                    .iter()
                    .map(|(_, place, _)| Place::parse(&id.0, place))
                    .collect(),
                rows: rows
                    .into_iter()
                    .map(|(service, place, source)| RemovedOperation {
                        service,
                        key: key.clone(),
                        method: method.clone(),
                        path: path.clone(),
                        place,
                        placed: false,
                        source,
                    })
                    .collect(),
            }
        })
        .collect();
    for removal in &mut removed {
        removal.rows.sort_by(|a, b| a.place.cmp(&b.place));
    }
    removed.sort_by(|a, b| {
        let first = |r: &Removed<RemovedOperation>| {
            r.rows
                .first()
                .map(|row| (row.service.clone(), row.key.clone()))
        };
        first(a).cmp(&first(b))
    });
    removed
}

fn removed_sends(
    main_self: &[CloudRepoData],
    current: &[CloudRepoData],
) -> Vec<Removed<RemovedSend>> {
    let send_key = |service_name: &Option<String>, row: &crate::analyzer::ApiEndpointDetails| {
        (
            service_name.clone(),
            row.key.canonical(),
            location_file(&row.file_path.to_string_lossy()),
        )
    };
    let current_keys: HashSet<(Option<String>, String, String)> = current
        .iter()
        .flat_map(|data| {
            data.calls
                .iter()
                .filter(|row| is_message_protocol(&row.key))
                .map(|row| send_key(&data.service_name, row))
        })
        .collect();
    let mut by_key: HashMap<(Option<String>, String, String), Removed<RemovedSend>> =
        HashMap::new();
    for repo in main_self {
        for row in repo
            .calls
            .iter()
            .filter(|row| is_message_protocol(&row.key))
        {
            let id = send_key(&repo.service_name, row);
            if current_keys.contains(&id) {
                continue;
            }
            let place = row.file_path.to_string_lossy().into_owned();
            let (method, path) = row.key.display_labels();
            let removal = by_key.entry(id.clone()).or_insert_with(|| Removed {
                rows: Vec::new(),
                places: Vec::new(),
            });
            removal
                .places
                .push(Place::parse(&repo.service_name, &place));
            removal.rows.push(RemovedSend {
                service: service_id(&repo.service_name, &repo.repo_name),
                key: id.1.clone(),
                file: id.2.clone(),
                method,
                path,
                place,
                placed: false,
                source: row.resolution_source,
            });
        }
    }
    let mut removed: Vec<Removed<RemovedSend>> = by_key.into_values().collect();
    for removal in &mut removed {
        removal.rows.sort_by(|a, b| a.place.cmp(&b.place));
    }
    removed.sort_by(|a, b| {
        let first = |r: &Removed<RemovedSend>| {
            r.rows
                .first()
                .map(|row| (row.service.clone(), row.key.clone(), row.file.clone()))
        };
        first(a).cmp(&first(b))
    });
    removed
}

/// Main's pairings (producer/consumer edges only) from main's matches
/// against the same peers, each side keyed as this run's removals and
/// unpaired rows are: the producer by its delta identity, the consumer by
/// its key as the match spells it and its file.
pub(super) fn main_pairings(matches: &[CrossRepoMatch]) -> Vec<MainPairing> {
    matches
        .iter()
        .filter(|edge| edge.relationship == carrick_match::MatchRelationship::ProducerConsumer)
        .filter_map(|edge| {
            let producer = OperationKey::from_canonical(&edge.producer_key)?;
            let (method, _) = producer.display_labels();
            Some(MainPairing {
                protocol: producer.protocol(),
                producer: MainProducer {
                    service: edge.producer_repo.clone(),
                    method,
                    key: operation_identity(&producer),
                },
                consumer: MainConsumer {
                    service: edge.consumer_repo.clone(),
                    key: edge.consumer_key.clone(),
                    file: location_file(edge.consumer_location.as_deref()?),
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http_edge(producer_path: &str, consumer_location: &str) -> CrossRepoMatch {
        CrossRepoMatch {
            producer_repo: "orders-api".into(),
            producer_key: OperationKey::http("GET", producer_path).canonical(),
            consumer_repo: "storefront".into(),
            consumer_key: OperationKey::http("GET", "/orders/:encoded").canonical(),
            relationship: carrick_match::MatchRelationship::ProducerConsumer,
            consumer_location: Some(consumer_location.into()),
            match_score: 1.0,
            type_compatible: None,
            type_verdict: None,
            mismatch_reason: None,
            producer_provenance: Default::default(),
        }
    }

    /// A pairing's producer is keyed as a removal of its route is, whatever
    /// the route's parameters are called, and its consumer by file alone.
    #[test]
    fn a_pairing_is_keyed_as_the_removal_and_the_unpaired_call_are() {
        let pairings = main_pairings(&[http_edge("/orders/:orderId", "src/checkout.ts:4")]);
        assert_eq!(pairings.len(), 1);
        let pairing = &pairings[0];
        assert_eq!(pairing.protocol, Protocol::Http);
        assert_eq!(pairing.producer.method, "GET");
        assert_eq!(
            pairing.producer.key,
            operation_identity(&OperationKey::http("GET", "/orders/:id"))
        );
        assert_eq!(pairing.consumer.file, "src/checkout.ts");
        assert_eq!(
            pairing.consumer.key,
            OperationKey::http("GET", "/orders/:encoded").canonical()
        );
    }

    /// A shared external contract names no producer, so it is no pairing.
    #[test]
    fn a_shared_contract_is_no_pairing() {
        let mut edge = http_edge("/orders/:orderId", "src/checkout.ts:4");
        edge.relationship = carrick_match::MatchRelationship::SharedExternalContract;
        assert!(main_pairings(&[edge]).is_empty());
    }

    // carrick#3252: a handler renamed to a new path while a documentation row
    // still states the old one. The shapes mirror
    // tests/fixtures/documented-route-moved (the same pair `check --changed`
    // reads), as index rows, because this list compares two indexes.

    fn route(
        method: &str,
        path: &str,
        file: &str,
        documentation: bool,
    ) -> crate::analyzer::ApiEndpointDetails {
        crate::analyzer::ApiEndpointDetails {
            view_module: false,
            owner: None,
            key: OperationKey::http(method, path.to_string()),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: Some(if documentation {
                "__route_descriptor__".to_string()
            } else {
                "getWidget".to_string()
            }),
            request_type: None,
            response_type: None,
            file_path: std::path::PathBuf::from(file),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            resolution_source: documentation.then_some(ResolutionSource::DescriptorRoute),
            dispatch: None,
            schema_binding: None,
            undeclared_selection: None,
            handler_span: None,
            name_scope: None,
            library_semantics: Vec::new(),
            inference: None,
        }
    }

    fn repo(endpoints: Vec<crate::analyzer::ApiEndpointDetails>) -> CloudRepoData {
        CloudRepoData {
            repo_name: "widgets".to_string(),
            service_name: Some("widgets".to_string()),
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
            mount_graph: None,
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
            call_through: None,
            route_boundaries: None,
            memory_peaks: None,
            unfollowed_member_calls: Vec::new(),
            unfollowed_member_calls_total: 0,
            type_reach: None,
            pair_outcomes: None,
        }
    }

    fn removed_paths(removed: &[Removed<RemovedOperation>]) -> Vec<String> {
        removed
            .iter()
            .flat_map(|r| r.rows.iter().map(|row| row.path.clone()))
            .collect()
    }

    /// The handler moved to a new path; the documentation row still states
    /// the old one, in both indexes. The old path is removed.
    #[test]
    fn a_handler_path_only_documentation_still_states_is_removed() {
        let main = [repo(vec![
            route("GET", "/widgets/:id", "src/widgets.ts", false),
            route("GET", "/widgets/:id", "src/openapi.ts", true),
        ])];
        let now = [repo(vec![
            route("GET", "/items/:id", "src/widgets.ts", false),
            route("GET", "/widgets/:id", "src/openapi.ts", true),
        ])];
        let removed = removed_operations(&main, &now);
        assert_eq!(removed_paths(&removed), vec!["/widgets/:id".to_string()]);
        assert_eq!(removed[0].rows[0].place, "src/widgets.ts");
    }

    /// Control: a real deletion, no documentation anywhere, is removed.
    #[test]
    fn a_deleted_handler_is_still_removed() {
        let main = [repo(vec![route(
            "GET",
            "/widgets/:id",
            "src/widgets.ts",
            false,
        )])];
        let now = [repo(vec![])];
        assert_eq!(
            removed_paths(&removed_operations(&main, &now)),
            vec!["/widgets/:id".to_string()]
        );
    }

    /// Control: the same path served from another file is no removal.
    #[test]
    fn a_handler_moved_to_another_file_at_the_same_path_is_not_removed() {
        let main = [repo(vec![
            route("GET", "/widgets/:id", "src/widgets.ts", false),
            route("GET", "/widgets/:id", "src/openapi.ts", true),
        ])];
        let now = [repo(vec![
            route("GET", "/widgets/:id", "src/handlers.ts", false),
            route("GET", "/widgets/:id", "src/openapi.ts", true),
        ])];
        assert!(removed_operations(&main, &now).is_empty());
    }

    /// Control: an operation only documentation ever stated, still stated, is
    /// not removed.
    #[test]
    fn an_operation_only_documentation_stated_is_not_removed() {
        let docs = || vec![route("GET", "/legacy", "src/openapi.ts", true)];
        assert!(removed_operations(&[repo(docs())], &[repo(docs())]).is_empty());
    }
}
