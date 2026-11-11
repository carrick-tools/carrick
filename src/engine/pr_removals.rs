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

use tracing::{debug, info};

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

/// The services (by `service_name`, `None` for a root service) whose
/// procedure walk stopped anywhere in this run
/// ([`crate::site_rows::TracedRoutes::procedure_stopped`]).
pub(super) type ProcedureStops = HashSet<Option<String>>;

/// Whether the removal of a main row of `service` is not proven (carrick#3778
/// R5): it is a procedure row, and this run's procedure walk stopped in that
/// service, so the row may still be there, unread. The break row a removal
/// gives and the delta row that lists it read this one answer.
pub(super) fn procedure_removal_in_doubt(
    stopped: &ProcedureStops,
    service: &Option<String>,
    procedure: bool,
) -> bool {
    procedure && stopped.contains(service)
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
    /// `base` when the run knows it. A removed procedure row of a service in
    /// `stopped` is in doubt ([`procedure_removal_in_doubt`]).
    pub(super) fn of(
        repo_path: &str,
        base: Option<&str>,
        main_self: &[CloudRepoData],
        current: &[CloudRepoData],
        services: &[Config],
        changes: Option<&crate::change_scope::ChangedSince>,
        stopped: &ProcedureStops,
    ) -> Self {
        let mut operations = removed_operations(main_self, current, stopped);
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
        changes,
        base,
        Vec::new(),
        candidates,
        scan,
        main,
        reading_moved,
        &[],
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
    stopped: &ProcedureStops,
) -> Vec<Removed<RemovedOperation>> {
    let current_keys: HashSet<ServiceEndpointKey> = current
        .iter()
        .flat_map(|data| {
            data.endpoints
                .iter()
                .map(|row| (data.service_name.clone(), delta_identity(&row.key)))
        })
        .collect();
    // Main's spelling of each key, as the delta keeps it, beside each row.
    let mut spelled: HashMap<ServiceEndpointKey, DeltaRow> = HashMap::new();
    type Row = (String, String, Option<ResolutionSource>, bool);
    let mut rows: HashMap<ServiceEndpointKey, Vec<Row>> = HashMap::new();
    for repo in main_self {
        for row in &repo.endpoints {
            let id = (repo.service_name.clone(), delta_identity(&row.key));
            if current_keys.contains(&id) {
                continue;
            }
            keep_first_spelling(&mut spelled, &repo.service_name, row, &repo.commit_hash);
            rows.entry(id).or_default().push((
                service_id(&repo.service_name, &repo.repo_name),
                row.file_path.to_string_lossy().into_owned(),
                row.resolution_source,
                procedure_removal_in_doubt(stopped, &repo.service_name, row.procedure.is_some()),
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
                    .map(|(_, place, _, _)| Place::parse(&id.0, place))
                    .collect(),
                rows: rows
                    .into_iter()
                    .map(|(service, place, source, in_doubt)| RemovedOperation {
                        service,
                        key: key.clone(),
                        method: method.clone(),
                        path: path.clone(),
                        place,
                        placed: false,
                        source,
                        in_doubt,
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
///
/// A procedure call's edge states no consumer path (carrick#3778): its
/// consumer key takes the procedure of the producer row it paired with, read
/// from `endpoints` (main's route rows, keyed as the edge keys its producer),
/// as this run's unpaired call takes its own. Where main has rows of two
/// different procedures under the producer's key, which one the call paired
/// with is not known, so that pairing is left out. Every other edge keeps
/// its key.
pub(super) fn main_pairings(
    matches: &[CrossRepoMatch],
    endpoints: &[crate::mount_graph::ResolvedEndpoint],
) -> Vec<MainPairing> {
    let mut procedures: HashMap<(String, String), HashSet<&carrick_match::ProcedureKey>> =
        HashMap::new();
    for endpoint in endpoints {
        let (Some(service), Some(procedure)) = (
            endpoint
                .service_name
                .as_ref()
                .or(endpoint.repo_name.as_ref()),
            endpoint.procedure.as_ref(),
        ) else {
            continue;
        };
        procedures
            .entry((
                service.clone(),
                OperationKey::http(&endpoint.method, endpoint.full_path.clone()).canonical(),
            ))
            .or_default()
            .insert(procedure);
    }
    matches
        .iter()
        .filter(|edge| edge.relationship == carrick_match::MatchRelationship::ProducerConsumer)
        .filter_map(|edge| {
            let producer = OperationKey::from_canonical(&edge.producer_key)?;
            let (method, _) = producer.display_labels();
            let states_no_path = OperationKey::from_canonical(&edge.consumer_key)
                .as_ref()
                .and_then(OperationKey::as_http)
                .is_some_and(|(_, path)| path.is_empty());
            let consumer_key = if states_no_path {
                let mut held = procedures
                    .get(&(edge.producer_repo.clone(), edge.producer_key.clone()))?
                    .iter();
                let (Some(procedure), None) = (held.next(), held.next()) else {
                    debug!(
                        "Main's pairing of {} with {} is left out: its key holds more than one \
                         procedure",
                        edge.producer_key, edge.consumer_repo
                    );
                    return None;
                };
                crate::pr_baseline::procedure_consumer_key(&edge.consumer_key, procedure)
            } else {
                edge.consumer_key.clone()
            };
            Some(MainPairing {
                protocol: producer.protocol(),
                producer: MainProducer {
                    service: edge.producer_repo.clone(),
                    method,
                    key: operation_identity(&producer),
                },
                consumer: MainConsumer {
                    service: edge.consumer_repo.clone(),
                    key: consumer_key,
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
        let pairings = main_pairings(&[http_edge("/orders/:orderId", "src/checkout.ts:4")], &[]);
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
        assert!(main_pairings(&[edge], &[]).is_empty());
    }

    // ---- Procedure calls (carrick#3778) ----------------------------------

    const SHELF_FILE: &str = "web/src/shelf.ts";

    fn shelf_procedure(path: &str) -> carrick_match::ProcedureKey {
        carrick_match::ProcedureKey {
            path: path.into(),
            router: carrick_match::RouterId {
                repo: "shop".into(),
                file: "api/src/router.ts".into(),
                binding: "appRouter".into(),
            },
        }
    }

    /// A route row of `api` serving `procedure` (when given) at
    /// `GET /api/rpc/<path>`. `service` `None` is a row named by its repo
    /// alone, as a peer repo's blob is.
    fn shelf_row(
        service: Option<&str>,
        path: &str,
        procedure: Option<&str>,
    ) -> crate::mount_graph::ResolvedEndpoint {
        serde_json::from_value(serde_json::json!({
            "method": "GET",
            "path": format!("/{path}"),
            "full_path": format!("/api/rpc/{path}"),
            "handler": null,
            "owner": "app",
            "file_location": "api/src/router.ts:5",
            "middleware_chain": [],
            "repo_name": "shop-api",
            "service_name": service,
            "procedure": procedure.map(|p| serde_json::to_value(shelf_procedure(p)).unwrap()),
        }))
        .unwrap()
    }

    /// Main's edge of a call at `SHELF_FILE:<line>` to `GET /api/rpc/<path>`,
    /// with the call's own path `consumer_path` (empty for a procedure call).
    fn shelf_edge(producer: &str, path: &str, consumer_path: &str, line: u32) -> CrossRepoMatch {
        CrossRepoMatch {
            producer_repo: producer.into(),
            producer_key: OperationKey::http("GET", format!("/api/rpc/{path}")).canonical(),
            consumer_repo: "web".into(),
            consumer_key: OperationKey::http("GET", consumer_path).canonical(),
            relationship: carrick_match::MatchRelationship::ProducerConsumer,
            consumer_location: Some(format!("{SHELF_FILE}:{line}")),
            match_score: 1.0,
            type_compatible: None,
            type_verdict: None,
            mismatch_reason: None,
            producer_provenance: Default::default(),
        }
    }

    /// This run's unpaired procedure call at `SHELF_FILE:<line>`, keyed as
    /// the analyzer keys it.
    fn shelf_call(path: &str, line: u32) -> crate::pr_baseline::UnpairedCall {
        crate::pr_baseline::UnpairedCall {
            service: "web".into(),
            key: crate::pr_baseline::procedure_consumer_key(
                &OperationKey::http("GET", "").canonical(),
                &shelf_procedure(path),
            ),
            file: SHELF_FILE.into(),
            method: "GET".into(),
            path: String::new(),
            sites: vec![format!("{SHELF_FILE}:{line}")],
            source: Some(ResolutionSource::RequestSpec),
        }
    }

    /// Main's `api` row of `shelf.get`, removed by the PR and placed.
    fn removed_get(service: &str) -> RemovedOperation {
        RemovedOperation {
            service: service.into(),
            key: operation_identity(&OperationKey::http("GET", "/api/rpc/shelf.get")),
            method: "GET".into(),
            path: "/api/rpc/shelf.get".into(),
            place: "api/src/router.ts:5".into(),
            placed: true,
            source: Some(ResolutionSource::RequestSpec),
            in_doubt: false,
        }
    }

    /// The break rows `mark_lost_pairings` writes for main's `pairings`,
    /// the removal of `shelf.get` and this run's unpaired `calls`, as
    /// `<removed path> <service> <sites>`.
    fn marked(
        pairings: Vec<MainPairing>,
        removed: RemovedOperation,
        calls: Vec<crate::pr_baseline::UnpairedCall>,
    ) -> Vec<String> {
        let mut pr = Vec::new();
        crate::pr_baseline::mark_lost_pairings(
            &mut pr,
            &crate::pr_baseline::MainRemovals {
                pairings,
                operations: vec![removed],
                sends: vec![],
            },
            &crate::pr_baseline::RunUnpaired {
                calls,
                receivers: vec![],
            },
            crate::pr_baseline::MainCopy::Current,
            None,
        );
        pr.iter()
            .map(|finding| {
                let v = serde_json::to_value(finding).unwrap();
                format!(
                    "{} {} {}",
                    v["removed"]["path"], v["service"], v["call_sites"]
                )
            })
            .collect()
    }

    /// A procedure edge states no consumer path: its consumer key takes the
    /// procedure of the row it paired with, as the run's unpaired call does.
    #[test]
    fn a_procedure_edge_keys_its_consumer_by_procedure() {
        let pairings = main_pairings(
            &[shelf_edge("api", "shelf.get", "", 8)],
            &[shelf_row(Some("api"), "shelf.get", Some("shelf.get"))],
        );
        assert_eq!(pairings.len(), 1);
        assert_eq!(pairings[0].consumer.key, shelf_call("shelf.get", 8).key);
        assert_ne!(
            pairings[0].consumer.key,
            OperationKey::http("GET", "").canonical()
        );
    }

    /// A plain call of a procedure's route states its path, so its key is
    /// the key it has today, whatever procedure the route serves.
    #[test]
    fn a_plain_call_to_a_procedure_route_keeps_its_key() {
        let pairings = main_pairings(
            &[shelf_edge("api", "shelf.get", "/api/rpc/shelf.get", 8)],
            &[shelf_row(Some("api"), "shelf.get", Some("shelf.get"))],
        );
        assert_eq!(pairings.len(), 1);
        assert_eq!(
            pairings[0].consumer.key,
            OperationKey::http("GET", "/api/rpc/shelf.get").canonical()
        );
    }

    /// Two procedures under one key of one service (two routers at one
    /// mount): which one the call paired with is not known, so the pairing
    /// is left out, never guessed. A row with no procedure there adds none.
    #[test]
    fn two_procedures_under_one_key_skip_the_pairing() {
        let mut other = shelf_row(Some("api"), "shelf.get", Some("shelf.get"));
        other.procedure.as_mut().unwrap().router.binding = "otherRouter".into();
        let edge = shelf_edge("api", "shelf.get", "", 8);
        assert!(
            main_pairings(
                std::slice::from_ref(&edge),
                &[
                    shelf_row(Some("api"), "shelf.get", Some("shelf.get")),
                    other
                ],
            )
            .is_empty()
        );
        assert_eq!(
            main_pairings(
                std::slice::from_ref(&edge),
                &[
                    shelf_row(Some("api"), "shelf.get", Some("shelf.get")),
                    shelf_row(Some("api"), "shelf.get", None),
                ],
            )
            .len(),
            1
        );
    }

    /// The never-served `shelf.gone` call shares its file and its HTTP key
    /// (`http|GET|`) with the call of the removed `shelf.get`, but not its
    /// procedure, so it stays out of the break row.
    #[test]
    fn a_never_served_procedure_call_in_the_file_of_a_removed_one_stays_out_of_its_row() {
        let pairings = main_pairings(
            &[shelf_edge("api", "shelf.get", "", 8)],
            &[shelf_row(Some("api"), "shelf.get", Some("shelf.get"))],
        );
        assert_eq!(
            marked(
                pairings,
                removed_get("api"),
                vec![shelf_call("shelf.get", 8), shelf_call("shelf.gone", 10)],
            ),
            [r#""/api/rpc/shelf.get" "web" ["web/src/shelf.ts:8"]"#]
        );
    }

    /// A producer carried in a peer blob is named by its repo alone, as the
    /// edge names it: main's pairing through it is found and marked.
    #[test]
    fn a_procedure_pairing_main_had_through_a_peer_blob_is_marked() {
        let pairings = main_pairings(
            &[shelf_edge("shop-api", "shelf.get", "", 8)],
            &[shelf_row(None, "shelf.get", Some("shelf.get"))],
        );
        assert_eq!(
            marked(
                pairings,
                removed_get("shop-api"),
                vec![shelf_call("shelf.get", 8)],
            ),
            [r#""/api/rpc/shelf.get" "web" ["web/src/shelf.ts:8"]"#]
        );
    }

    /// The doubt predicate: a procedure row of a service whose walk stopped.
    #[test]
    fn a_removal_is_in_doubt_only_for_a_procedure_row_of_a_stopped_service() {
        let stopped: ProcedureStops = [Some("api".to_string())].into_iter().collect();
        let api = Some("api".to_string());
        assert!(procedure_removal_in_doubt(&stopped, &api, true));
        assert!(!procedure_removal_in_doubt(&stopped, &api, false));
        assert!(!procedure_removal_in_doubt(
            &stopped,
            &Some("web".to_string()),
            true
        ));
        assert!(!procedure_removal_in_doubt(&stopped, &None, true));
    }
}
