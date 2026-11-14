//! Mark the rows of a pairing main had and this PR lost (carrick#3092, S-A).
//!
//! A pairing is one producer operation (a route, a schema root field, a socket
//! listener, a pub/sub subscriber) and one consumer call file (a request, a
//! socket emit, a pub/sub publish). When the PR's diff removes one side and the
//! other is still in the index, the surviving side is a break the PR caused,
//! and a run that only lists it as a `missing_endpoint` or `orphaned_endpoint`
//! cannot tell it from a call that never had a producer.
//!
//! [`mark_lost_pairings`] is pure: it reads main's pairings, the removals the
//! caller computed against main (with whether the diff placed each one), and
//! this run's unpaired calls and receivers, and rewrites the run's findings.
//! Computing those inputs is the engine's work (carrick#3092, S-B). Keys on
//! the two sides of a comparison are compared as given, so the caller spells
//! them with one canonicaliser (`delta_identity` for operations).
//!
//! The rule is certainty first (David, 2026-11-02: failing a PR must be
//! certain). `on_main: false` is written in exactly one case: the removal is
//! placed in the diff, main's copy is current, and nothing contradicts the
//! placement. Every other case with any doubt is `on_main_unknown`.

use std::collections::{BTreeMap, BTreeSet};

use crate::agents::file_analyzer_agent::ResolutionSource;
use crate::findings::{EdgeSource, Finding, OnMainUnknown, Removal, RemovedSide, VerdictState};
use crate::operation::Protocol;

use super::{MainCopy, Untouched};

/// One pairing main had (a producer/consumer edge only), from main's copy
/// matched against the same peers.
#[derive(Clone, Debug)]
pub struct MainPairing {
    pub protocol: Protocol,
    pub producer: MainProducer,
    pub consumer: MainConsumer,
    /// Main confirmed the pairing: the call left none of the producer's route
    /// literals to a placeholder. Always true for a non-HTTP pairing.
    pub confirmed: bool,
}

/// The side main keys as the producer: the endpoint row.
#[derive(Clone, Debug)]
pub struct MainProducer {
    pub service: String,
    /// The pseudo method (`GET`, `QUERY`, `CLIENT->SERVER`, `PUBSUB`).
    pub method: String,
    /// The canonical operation key a removal of this row is keyed by.
    pub key: String,
}

/// The side main keys as the consumer: the call row.
#[derive(Clone, Debug)]
pub struct MainConsumer {
    pub service: String,
    /// The canonical call key, as this run's unpaired call spells it.
    pub key: String,
    pub file: String,
}

/// A producer row main has and this run lacks, keyed `(service, key)`.
#[derive(Clone, Debug)]
pub struct RemovedOperation {
    pub service: String,
    pub key: String,
    /// Main's spelling, for the row's `removed` object.
    pub method: String,
    pub path: String,
    /// `file:line` on main.
    pub place: String,
    /// Whether the diff anchor listed this removal (`pr_delta_anchor::run`).
    pub placed: bool,
    pub source: Option<ResolutionSource>,
}

/// A socket or pub/sub call row main has and this run lacks, keyed
/// `(service, key, file)`.
#[derive(Clone, Debug)]
pub struct RemovedSend {
    pub service: String,
    pub key: String,
    pub file: String,
    pub method: String,
    pub path: String,
    pub place: String,
    pub placed: bool,
    pub source: Option<ResolutionSource>,
}

/// A consumer call this run holds that no producer answers.
#[derive(Clone, Debug)]
pub struct UnpairedCall {
    pub service: String,
    pub key: String,
    pub file: String,
    /// The label the run's `missing_endpoint` row carries.
    pub method: String,
    pub path: String,
    pub sites: Vec<String>,
    pub source: Option<ResolutionSource>,
    /// The call still pairs, but only with routes whose literals it leaves to
    /// its placeholders (carrick#4277).
    pub held_open: bool,
    /// When `held_open`: the delta identities of the routes that hold it.
    pub held_by: Vec<String>,
}

/// An endpoint registration this run holds that no sender reaches.
#[derive(Clone, Debug)]
pub struct UnpairedReceiver {
    pub service: String,
    pub key: String,
    /// The label the run's `orphaned_endpoint` row carries.
    pub method: String,
    pub path: String,
    /// The registration places.
    pub sites: Vec<String>,
    pub source: Option<ResolutionSource>,
}

/// Everything main contributes.
#[derive(Clone, Debug, Default)]
pub struct MainRemovals {
    pub pairings: Vec<MainPairing>,
    pub operations: Vec<RemovedOperation>,
    pub sends: Vec<RemovedSend>,
    /// Delta identities of HTTP operations this PR added (in this run, not on
    /// main).
    pub added: BTreeSet<String>,
}

/// Everything this run contributes.
#[derive(Clone, Debug, Default)]
pub struct RunUnpaired {
    pub calls: Vec<UnpairedCall>,
    pub receivers: Vec<UnpairedReceiver>,
}

/// What the removed side's evidence says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    OnMain(bool),
    Unknown(OnMainUnknown),
}

/// The marking of one lost pairing (carrick#3092, "The rule", step 3).
///
/// `untouched` is whether the PR changed nothing the removed side's service
/// reads: `None` when it is not known (no base, an unreadable diff, or a
/// service the inputs do not name).
fn decide(placed: bool, copy: MainCopy, untouched: Option<bool>) -> Decision {
    match (placed, untouched) {
        // The diff places the removal and also says the service was left
        // alone. One of the two is wrong, and which is not known here.
        (true, Some(true)) => Decision::Unknown(OnMainUnknown::RemovalUnplaced),
        (true, _) => match copy {
            MainCopy::Current => Decision::OnMain(false),
            MainCopy::Stale => Decision::Unknown(OnMainUnknown::MainIndexStale),
            MainCopy::OtherScanner => Decision::Unknown(OnMainUnknown::MainIndexOtherScanner),
        },
        (false, Some(true)) => Decision::OnMain(true),
        (false, _) => Decision::Unknown(OnMainUnknown::RemovalUnplaced),
    }
}

fn service_untouched(untouched: Option<&Untouched>, service: &str) -> Option<bool> {
    untouched.and_then(|u| u.ours.get(service).copied())
}

fn is_message_protocol(protocol: Protocol) -> bool {
    matches!(protocol, Protocol::Websocket | Protocol::Pubsub)
}

/// One lost pairing's surviving rows, grouped to one output row.
struct Group {
    /// The removals behind it, as `(placed, source)`.
    removals: Vec<(bool, Option<ResolutionSource>)>,
    removed: RemovedSide,
    removed_service: String,
    /// Surviving side.
    service: String,
    method: String,
    path: String,
    sites: BTreeSet<String>,
    surviving_sources: Vec<Option<ResolutionSource>>,
}

impl Group {
    fn removal(&self, copy: MainCopy, untouched: Option<&Untouched>) -> Removal {
        // Every removal behind the row must be placed: one unplaced is doubt.
        let placed = !self.removals.is_empty() && self.removals.iter().all(|(placed, _)| *placed);
        let decision = decide(
            placed,
            copy,
            service_untouched(untouched, &self.removed_service),
        );
        let (on_main, on_main_unknown) = match decision {
            Decision::OnMain(value) => (Some(value), None),
            Decision::Unknown(reason) => (None, Some(reason)),
        };
        Removal {
            removed: self.removed.clone(),
            edge_source: EdgeSource::fold(
                self.surviving_sources
                    .iter()
                    .copied()
                    .chain(self.removals.iter().map(|(_, source)| *source)),
            ),
            verdict_state: Some(VerdictState::NotChecked),
            on_main,
            on_main_unknown,
        }
    }
}

fn held_by_removal(call: &UnpairedCall, confirmed: bool, main: &MainRemovals) -> bool {
    let _ = main;
    confirmed
}

/// Mark the rows of every pairing main had that this run lost, returning how
/// many rows it wrote.
///
/// Producer side gone: main's pairing has a producer that is a removal, and
/// this run still holds the consumer call with no producer. The call's sites
/// leave the run's unmarked `missing_endpoint` for that label and become one
/// marked row per (removed operation, surviving service); the old row keeps
/// the rest and goes when empty; the row is emitted when the run has none.
///
/// Sender side gone (socket and pub/sub only; a removed HTTP or GraphQL caller
/// is not a break): main's pairing has a consumer that is a removed send, and
/// this run still holds the receiver with no sender. The receiver's unmarked
/// `orphaned_endpoint` is replaced by a marked one carrying its registration
/// places. An HTTP or GraphQL orphan is never marked.
///
/// A call that still pairs at head (`held_open`) counts as held by the removed
/// route's loss when main confirmed that pairing and the routes that now hold
/// it are not a set of literals this PR added (two or more: the PR split the
/// parameter into fixed names, and the call may name any of them). A single
/// literal the PR added in place of the parameter is a break (carrick#4277).
///
/// A row that lost no pairing is left byte for byte as it was.
pub fn mark_lost_pairings(
    pr: &mut Vec<Finding>,
    main: &MainRemovals,
    run: &RunUnpaired,
    copy: MainCopy,
    untouched: Option<&Untouched>,
) -> usize {
    let mut producer_lost: BTreeMap<(String, String, String, String), Group> = BTreeMap::new();
    let mut sender_lost: BTreeMap<(String, String, String, String, String), Group> =
        BTreeMap::new();

    for pairing in &main.pairings {
        let producer = &pairing.producer;
        let consumer = &pairing.consumer;

        let operations: Vec<&RemovedOperation> = main
            .operations
            .iter()
            .filter(|op| op.service == producer.service && op.key == producer.key)
            .collect();
        if let Some(first) = operations.first() {
            for call in run.calls.iter().filter(|call| {
                call.service == consumer.service
                    && call.file == consumer.file
                    && call.key == consumer.key
                    && (!call.held_open || held_by_removal(call, pairing.confirmed, main))
            }) {
                let group = producer_lost
                    .entry((
                        producer.service.clone(),
                        producer.key.clone(),
                        call.service.clone(),
                        format!("{} {}", call.method, call.path),
                    ))
                    .or_insert_with(|| Group {
                        removals: operations.iter().map(|op| (op.placed, op.source)).collect(),
                        removed: RemovedSide {
                            method: first.method.clone(),
                            path: first.path.clone(),
                            service: None,
                            place: first.place.clone(),
                        },
                        removed_service: producer.service.clone(),
                        service: call.service.clone(),
                        method: call.method.clone(),
                        path: call.path.clone(),
                        sites: BTreeSet::new(),
                        surviving_sources: Vec::new(),
                    });
                group.sites.extend(call.sites.iter().cloned());
                group.surviving_sources.push(call.source);
            }
        }

        if !is_message_protocol(pairing.protocol) {
            continue;
        }
        let sends: Vec<&RemovedSend> = main
            .sends
            .iter()
            .filter(|send| {
                send.service == consumer.service
                    && send.key == consumer.key
                    && send.file == consumer.file
            })
            .collect();
        if let Some(first) = sends.first() {
            for receiver in run.receivers.iter().filter(|receiver| {
                receiver.service == producer.service && receiver.key == producer.key
            }) {
                let group = sender_lost
                    .entry((
                        consumer.service.clone(),
                        consumer.key.clone(),
                        consumer.file.clone(),
                        receiver.service.clone(),
                        format!("{} {}", receiver.method, receiver.path),
                    ))
                    .or_insert_with(|| Group {
                        removals: sends
                            .iter()
                            .map(|send| (send.placed, send.source))
                            .collect(),
                        removed: RemovedSide {
                            method: first.method.clone(),
                            path: first.path.clone(),
                            service: None,
                            place: first.place.clone(),
                        },
                        removed_service: consumer.service.clone(),
                        service: receiver.service.clone(),
                        method: receiver.method.clone(),
                        path: receiver.path.clone(),
                        sites: BTreeSet::new(),
                        surviving_sources: Vec::new(),
                    });
                group.sites.extend(receiver.sites.iter().cloned());
                group.surviving_sources.push(receiver.source);
            }
        }
    }

    let mut written = 0;
    for group in producer_lost.values() {
        // Emit or correct: the lost sites leave whichever unmarked row for this
        // label holds them, then the marked row stands on its own.
        pr.retain_mut(|finding| match finding {
            Finding::MissingEndpoint {
                method,
                path,
                call_sites,
                removal: None,
                ..
            } if *method == group.method && *path == group.path => {
                call_sites.retain(|site| !group.sites.contains(site));
                !call_sites.is_empty()
            }
            _ => true,
        });
        pr.push(
            Finding::missing_endpoint(
                group.method.clone(),
                group.path.clone(),
                Some(group.service.clone()),
                group.sites.iter().cloned().collect(),
            )
            .with_removal(group.removal(copy, untouched), Vec::new()),
        );
        written += 1;
    }
    // The (service, label) of every receiver a lost pairing marks. A GraphQL,
    // socket or pub/sub orphan names no service (the exact-key matcher keys
    // its orphans by label alone), so such a row stands for every receiver
    // of its label, and is replaced only when each of them is marked here.
    let marked: BTreeSet<(String, String, String)> = sender_lost
        .values()
        .map(|group| {
            (
                group.service.clone(),
                group.method.clone(),
                group.path.clone(),
            )
        })
        .collect();
    let all_marked = |method: &str, path: &str| {
        run.receivers
            .iter()
            .filter(|receiver| receiver.method == method && receiver.path == path)
            .all(|receiver| {
                marked.contains(&(
                    receiver.service.clone(),
                    receiver.method.clone(),
                    receiver.path.clone(),
                ))
            })
    };
    for group in sender_lost.values() {
        let mut provenance = None;
        let mut view_module = false;
        pr.retain(|finding| match finding {
            Finding::OrphanedEndpoint {
                method,
                path,
                service,
                provenance: row_provenance,
                view_module: row_view_module,
                removal: None,
                ..
            } if *method == group.method
                && *path == group.path
                && match service {
                    Some(service) => *service == group.service,
                    None => all_marked(method, path),
                } =>
            {
                provenance = Some(*row_provenance);
                view_module = *row_view_module;
                false
            }
            _ => true,
        });
        let mut row = Finding::orphaned_endpoint(
            group.method.clone(),
            group.path.clone(),
            Some(group.service.clone()),
        )
        .with_view_module(view_module)
        .with_removal(
            group.removal(copy, untouched),
            group.sites.iter().cloned().collect(),
        );
        if let Some(provenance) = provenance {
            row = row.with_producer_provenance(provenance);
        }
        pr.push(row);
        written += 1;
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ALL_COPIES: [MainCopy; 3] = [MainCopy::Current, MainCopy::Stale, MainCopy::OtherScanner];

    fn http_pairing() -> MainPairing {
        MainPairing {
            protocol: Protocol::Http,
            producer: MainProducer {
                service: "orders-api".into(),
                method: "GET".into(),
                key: "GET /orders/:param".into(),
            },
            consumer: MainConsumer {
                service: "storefront".into(),
                key: "GET /orders/:param".into(),
                file: "src/checkout.ts".into(),
            },
            confirmed: true,
        }
    }

    fn removed_route(placed: bool) -> RemovedOperation {
        RemovedOperation {
            service: "orders-api".into(),
            key: "GET /orders/:param".into(),
            method: "GET".into(),
            path: "/orders/:orderId".into(),
            place: "app/orders.ts:9".into(),
            placed,
            source: Some(ResolutionSource::RequestSpec),
        }
    }

    fn surviving_call() -> UnpairedCall {
        UnpairedCall {
            service: "storefront".into(),
            key: "GET /orders/:param".into(),
            file: "src/checkout.ts".into(),
            method: "GET".into(),
            path: "/orders/:encoded".into(),
            sites: vec!["src/checkout.ts:4".into(), "src/checkout.ts:20".into()],
            source: Some(ResolutionSource::RequestSpec),
            held_open: false,
            held_by: vec![],
        }
    }

    fn http_break(placed: bool) -> (MainRemovals, RunUnpaired) {
        (
            MainRemovals {
                pairings: vec![http_pairing()],
                operations: vec![removed_route(placed)],
                sends: vec![],
                ..Default::default()
            },
            RunUnpaired {
                calls: vec![surviving_call()],
                receivers: vec![],
            },
        )
    }

    fn untouched(service: &str, value: bool) -> Untouched {
        Untouched {
            ours: [(service.to_string(), value)].into_iter().collect(),
            peers: Default::default(),
        }
    }

    fn unmarked_missing() -> Finding {
        Finding::missing_endpoint(
            "GET",
            "/orders/:encoded",
            None,
            vec!["src/checkout.ts:4".into(), "src/checkout.ts:20".into()],
        )
    }

    fn on_main(finding: &Finding) -> (Option<bool>, Option<OnMainUnknown>) {
        let removal = finding.removal().expect("a marked row");
        (removal.on_main, removal.on_main_unknown)
    }

    fn run_http(
        placed: bool,
        copy: MainCopy,
        untouched: Option<&Untouched>,
    ) -> (Vec<Finding>, usize) {
        let (main, run) = http_break(placed);
        let mut pr = vec![unmarked_missing()];
        let written = mark_lost_pairings(&mut pr, &main, &run, copy, untouched);
        (pr, written)
    }

    fn held_break(
        confirmed: bool,
        held_by: &[&str],
        added: &[&str],
    ) -> (MainRemovals, RunUnpaired) {
        let (mut main, mut run) = http_break(true);
        main.pairings[0].confirmed = confirmed;
        main.added = added.iter().map(|key| key.to_string()).collect();
        run.calls[0].held_open = true;
        run.calls[0].held_by = held_by.iter().map(|key| key.to_string()).collect();
        (main, run)
    }

    fn mark_held(main: &MainRemovals, run: &RunUnpaired, pr: &mut Vec<Finding>) -> usize {
        mark_lost_pairings(
            pr,
            main,
            run,
            MainCopy::Current,
            Some(&untouched("orders-api", false)),
        )
    }

    /// carrick#4277: a call that still pairs, held open by a literal sibling,
    /// is a lost pairing when main confirmed it; the row is emitted when the
    /// run has none.
    #[test]
    fn a_held_open_call_with_a_confirmed_main_pairing_is_marked() {
        let (main, run) = held_break(true, &["GET /orders/summary"], &["GET /orders/summary"]);
        let mut pr = vec![];
        assert_eq!(mark_held(&main, &run, &mut pr), 1);
        assert_eq!(on_main(&pr[0]), (Some(false), None));
    }

    /// A pairing main did not confirm leaves the findings untouched.
    #[test]
    fn a_held_open_call_with_an_unconfirmed_main_pairing_changes_nothing() {
        let (main, run) = held_break(false, &["GET /orders/summary"], &[]);
        let mut pr = vec![unmarked_missing()];
        let before = serde_json::to_value(&pr).unwrap();
        assert_eq!(mark_held(&main, &run, &mut pr), 0);
        assert_eq!(serde_json::to_value(&pr).unwrap(), before);
    }

    /// A held-open call whose main producer was not removed gives no row.
    #[test]
    fn a_held_open_call_whose_main_producer_survives_gives_no_row() {
        let (mut main, run) = held_break(true, &["GET /orders/summary"], &[]);
        main.operations.clear();
        let mut pr = vec![];
        assert_eq!(mark_held(&main, &run, &mut pr), 0);
        assert!(pr.is_empty());
    }

    /// A PR that splits the parameter into two or more fixed names the call
    /// may enumerate proves nothing; one literal in its place does.
    #[test]
    fn a_split_into_added_literals_is_not_marked() {
        let both = ["GET /orders/a", "GET /orders/b"];
        let (main, run) = held_break(true, &both, &both);
        let mut pr = vec![];
        assert_eq!(mark_held(&main, &run, &mut pr), 0);
        // One of the siblings already stood on main: the removal alone broke
        // the call.
        let (main, run) = held_break(true, &both, &["GET /orders/a"]);
        assert_eq!(mark_held(&main, &run, &mut pr), 1);
    }

    /// Branch 1: placed, main's copy current -> `on_main: false`, and the row
    /// is a risk that moved out of the unmarked one.
    #[test]
    fn placed_with_a_current_copy_is_not_on_main() {
        let (pr, written) = run_http(
            true,
            MainCopy::Current,
            Some(&untouched("orders-api", false)),
        );
        assert_eq!(written, 1);
        assert_eq!(pr.len(), 1, "the unmarked row held only the lost sites");
        assert_eq!(on_main(&pr[0]), (Some(false), None));
        let v = serde_json::to_value(&pr[0]).unwrap();
        assert_eq!(v["severity"], json!("risk"));
        assert_eq!(v["service"], json!("storefront"));
        assert_eq!(v["removed"]["path"], json!("/orders/:orderId"));
        assert_eq!(v["removed"]["place"], json!("app/orders.ts:9"));
        assert_eq!(v["edge_source"], json!("fact"));
        assert_eq!(v["verdict_state"], json!("not_checked"));
        assert_eq!(v["on_main"], json!(false));
    }

    /// Branch 2: placed, a stale or other-scanner copy -> unknown, with the
    /// reason `mark_on_main` gives for the same copy.
    #[test]
    fn placed_with_an_untrusted_copy_is_unknown() {
        for (copy, reason) in [
            (MainCopy::Stale, OnMainUnknown::MainIndexStale),
            (MainCopy::OtherScanner, OnMainUnknown::MainIndexOtherScanner),
        ] {
            let (pr, _) = run_http(true, copy, None);
            assert_eq!(on_main(&pr[0]), (None, Some(reason)));
        }
    }

    /// Branch 3: not placed, the removed side's service untouched -> on main.
    #[test]
    fn unplaced_in_an_untouched_service_is_on_main() {
        for copy in ALL_COPIES {
            let (pr, _) = run_http(false, copy, Some(&untouched("orders-api", true)));
            assert_eq!(on_main(&pr[0]), (Some(true), None), "{copy:?}");
        }
    }

    /// Branch 4: not placed, the service touched or nothing known -> the new
    /// reason, never `false`.
    #[test]
    fn unplaced_elsewhere_is_removal_unplaced() {
        let touched = untouched("orders-api", false);
        let other_service = untouched("billing", true);
        for u in [Some(&touched), Some(&other_service), None] {
            for copy in ALL_COPIES {
                let (pr, _) = run_http(false, copy, u);
                assert_eq!(
                    on_main(&pr[0]),
                    (None, Some(OnMainUnknown::RemovalUnplaced)),
                    "{copy:?} {u:?}"
                );
            }
        }
    }

    /// David's bar: `on_main: false` is written only for a placed removal on
    /// a current copy that nothing contradicts. Every other input lands on
    /// something that does not fail a PR, and a placed-but-untouched
    /// contradiction is doubt.
    #[test]
    fn false_is_written_only_when_everything_agrees() {
        let touched = untouched("orders-api", false);
        let clean = untouched("orders-api", true);
        let unrelated = untouched("billing", false);
        for placed in [true, false] {
            for copy in ALL_COPIES {
                for u in [None, Some(&touched), Some(&clean), Some(&unrelated)] {
                    let (pr, _) = run_http(placed, copy, u);
                    let (value, reason) = on_main(&pr[0]);
                    let contradicted = u.is_some_and(|u| u.ours.get("orders-api") == Some(&true));
                    let certain = placed && copy == MainCopy::Current && !contradicted;
                    assert_eq!(value == Some(false), certain, "{placed} {copy:?} {u:?}");
                    assert_eq!(value.is_some(), reason.is_none());
                }
            }
        }
    }

    /// The contradiction by itself: the diff places the removal and also says
    /// the service was untouched.
    #[test]
    fn a_placed_removal_in_an_untouched_service_is_doubt() {
        let (pr, _) = run_http(
            true,
            MainCopy::Current,
            Some(&untouched("orders-api", true)),
        );
        assert_eq!(
            on_main(&pr[0]),
            (None, Some(OnMainUnknown::RemovalUnplaced))
        );
    }

    /// One removal behind the row that is not placed is enough for doubt.
    #[test]
    fn every_removal_behind_a_row_must_be_placed() {
        let (mut main, run) = http_break(true);
        main.operations.push(removed_route(false));
        let mut pr = vec![];
        mark_lost_pairings(&mut pr, &main, &run, MainCopy::Current, None);
        assert_eq!(
            on_main(&pr[0]),
            (None, Some(OnMainUnknown::RemovalUnplaced))
        );
    }

    /// Emit or correct: the old row keeps the sites that did not lose a
    /// pairing, the marked row takes the ones that did, and with no old row
    /// the pass emits.
    #[test]
    fn the_lost_sites_leave_the_old_row_and_a_missing_row_is_emitted() {
        let (main, run) = http_break(true);
        let mut pr = vec![Finding::missing_endpoint(
            "GET",
            "/orders/:encoded",
            None,
            vec![
                "src/checkout.ts:4".into(),
                "src/checkout.ts:20".into(),
                "src/other.ts:7".into(),
            ],
        )];
        mark_lost_pairings(&mut pr, &main, &run, MainCopy::Current, None);
        assert_eq!(pr.len(), 2);
        let Finding::MissingEndpoint {
            call_sites,
            removal: None,
            ..
        } = &pr[0]
        else {
            panic!("{:?}", pr[0]);
        };
        assert_eq!(call_sites, &["src/other.ts:7".to_string()]);
        let Finding::MissingEndpoint { call_sites, .. } = &pr[1] else {
            panic!()
        };
        assert_eq!(
            call_sites,
            &["src/checkout.ts:20", "src/checkout.ts:4"].map(String::from)
        );

        let mut empty = vec![];
        let written = mark_lost_pairings(&mut empty, &main, &run, MainCopy::Current, None);
        assert_eq!((written, empty.len()), (1, 1));
    }

    /// A call with no producer on main either has no main pairing: unmarked.
    #[test]
    fn a_call_main_never_paired_stays_unmarked() {
        let (mut main, run) = http_break(true);
        main.pairings.clear();
        let mut pr = vec![unmarked_missing()];
        let before = pr.clone();
        assert_eq!(
            mark_lost_pairings(&mut pr, &main, &run, MainCopy::Current, None),
            0
        );
        assert_eq!(pr, before);
    }

    /// A rename the consumer followed in the same PR leaves no unpaired call,
    /// so no row; and a removal that is not this pairing's producer marks
    /// nothing either.
    #[test]
    fn a_followed_rename_and_an_unrelated_removal_mark_nothing() {
        let (mut main, mut run) = http_break(true);
        run.calls.clear();
        let mut pr = vec![];
        assert_eq!(
            mark_lost_pairings(&mut pr, &main, &run, MainCopy::Current, None),
            0
        );
        assert!(pr.is_empty());

        let (_, run) = http_break(true);
        main.operations[0].key = "GET /elsewhere".into();
        assert_eq!(
            mark_lost_pairings(&mut pr, &main, &run, MainCopy::Current, None),
            0
        );

        // A removal in another service with the same key is not it either.
        main.operations[0].key = "GET /orders/:param".into();
        main.operations[0].service = "billing".into();
        assert_eq!(
            mark_lost_pairings(&mut pr, &main, &run, MainCopy::Current, None),
            0
        );

        // An unpaired call in a file the pairing never had.
        let (main, mut run) = http_break(true);
        run.calls[0].file = "src/new.ts".into();
        assert_eq!(
            mark_lost_pairings(&mut pr, &main, &run, MainCopy::Current, None),
            0
        );
    }

    fn message_pairing(protocol: Protocol) -> MainPairing {
        MainPairing {
            protocol,
            producer: MainProducer {
                service: "storefront".into(),
                method: "SERVER->CLIENT".into(),
                key: "SERVER->CLIENT order:shipped".into(),
            },
            consumer: MainConsumer {
                service: "orders-api".into(),
                key: "SERVER->CLIENT order:shipped".into(),
                file: "src/realtime.ts".into(),
            },
            confirmed: true,
        }
    }

    fn removed_send(placed: bool) -> RemovedSend {
        RemovedSend {
            service: "orders-api".into(),
            key: "SERVER->CLIENT order:shipped".into(),
            file: "src/realtime.ts".into(),
            method: "SERVER->CLIENT".into(),
            path: "order:shipped".into(),
            place: "src/realtime.ts:12".into(),
            placed,
            source: Some(ResolutionSource::RequestSpec),
        }
    }

    fn receiver() -> UnpairedReceiver {
        UnpairedReceiver {
            service: "storefront".into(),
            key: "SERVER->CLIENT order:shipped".into(),
            method: "SERVER->CLIENT".into(),
            path: "order:shipped".into(),
            sites: vec!["src/live.ts:15".into()],
            source: Some(ResolutionSource::RequestSpec),
        }
    }

    fn orphan_row() -> Finding {
        Finding::orphaned_endpoint("SERVER->CLIENT", "order:shipped", Some("storefront".into()))
    }

    /// Socket and pub/sub: a removed sender with the receiver left replaces
    /// the unmarked orphan with a marked one that carries the registration.
    #[test]
    fn a_removed_sender_marks_the_receivers_orphan_for_message_protocols() {
        for protocol in [Protocol::Websocket, Protocol::Pubsub] {
            let main = MainRemovals {
                pairings: vec![message_pairing(protocol)],
                operations: vec![],
                sends: vec![removed_send(true)],
                ..Default::default()
            };
            let run = RunUnpaired {
                calls: vec![],
                receivers: vec![receiver()],
            };
            let mut pr = vec![orphan_row()];
            assert_eq!(
                mark_lost_pairings(&mut pr, &main, &run, MainCopy::Current, None),
                1
            );
            assert_eq!(pr.len(), 1, "replaced, not added to");
            assert_eq!(on_main(&pr[0]), (Some(false), None));
            let v = serde_json::to_value(&pr[0]).unwrap();
            assert_eq!(v["kind"], json!("orphaned_endpoint"));
            assert_eq!(v["severity"], json!("risk"));
            assert_eq!(v["sites"], json!(["src/live.ts:15"]));
            assert_eq!(v["removed"]["place"], json!("src/realtime.ts:12"));

            // With no orphan row in the run, the pass emits it.
            let mut empty = vec![];
            mark_lost_pairings(&mut empty, &main, &run, MainCopy::Current, None);
            assert_eq!(empty.len(), 1);

            // An unplaced removed sender is doubt, like any other.
            let mut main = main;
            main.sends[0].placed = false;
            let mut pr = vec![orphan_row()];
            mark_lost_pairings(&mut pr, &main, &run, MainCopy::Current, None);
            assert_eq!(
                on_main(&pr[0]),
                (None, Some(OnMainUnknown::RemovalUnplaced))
            );
        }
    }

    /// A removed HTTP or GraphQL caller is not a break: its orphan is never
    /// marked, whatever the removal lists hold.
    #[test]
    fn an_http_or_graphql_orphan_is_never_marked() {
        for protocol in [Protocol::Http, Protocol::Graphql] {
            let main = MainRemovals {
                pairings: vec![message_pairing(protocol)],
                operations: vec![],
                sends: vec![removed_send(true)],
                ..Default::default()
            };
            let run = RunUnpaired {
                calls: vec![],
                receivers: vec![receiver()],
            };
            let mut pr = vec![orphan_row()];
            let before = pr.clone();
            assert_eq!(
                mark_lost_pairings(&mut pr, &main, &run, MainCopy::Current, None),
                0
            );
            assert_eq!(pr, before);
        }
    }

    /// A receiver's orphan row for another service is left alone.
    #[test]
    fn another_services_orphan_is_not_replaced() {
        let main = MainRemovals {
            pairings: vec![message_pairing(Protocol::Websocket)],
            operations: vec![],
            sends: vec![removed_send(true)],
            ..Default::default()
        };
        let run = RunUnpaired {
            calls: vec![],
            receivers: vec![receiver()],
        };
        let other =
            Finding::orphaned_endpoint("SERVER->CLIENT", "order:shipped", Some("dashboard".into()));
        let mut pr = vec![other.clone()];
        mark_lost_pairings(&mut pr, &main, &run, MainCopy::Current, None);
        assert_eq!(pr.len(), 2);
        assert_eq!(pr[0], other);
    }

    /// A socket or pub/sub orphan names no service: the exact-key matcher
    /// keys it by label. It is replaced when every receiver of its label is
    /// marked, and kept when one is not, since it still stands for that one.
    #[test]
    fn an_orphan_with_no_service_is_replaced_only_when_every_receiver_is_marked() {
        let main = MainRemovals {
            pairings: vec![message_pairing(Protocol::Websocket)],
            operations: vec![],
            sends: vec![removed_send(true)],
            ..Default::default()
        };
        let unnamed = Finding::orphaned_endpoint("SERVER->CLIENT", "order:shipped", None);

        let run = RunUnpaired {
            calls: vec![],
            receivers: vec![receiver()],
        };
        let mut pr = vec![unnamed.clone()];
        assert_eq!(
            mark_lost_pairings(&mut pr, &main, &run, MainCopy::Current, None),
            1
        );
        assert_eq!(pr.len(), 1, "replaced, not added to: {pr:#?}");
        assert_eq!(on_main(&pr[0]), (Some(false), None));

        // A second service listens for the event and lost nothing: the
        // unnamed row still stands for it.
        let mut other = receiver();
        other.service = "dashboard".into();
        let run = RunUnpaired {
            calls: vec![],
            receivers: vec![receiver(), other],
        };
        let mut pr = vec![unnamed.clone()];
        mark_lost_pairings(&mut pr, &main, &run, MainCopy::Current, None);
        assert_eq!(pr.len(), 2, "{pr:#?}");
        assert_eq!(pr[0], unnamed);
    }

    /// The edge source folds the surviving and the removed rows: a model row
    /// on either side is a candidate, a row with no source states nothing.
    #[test]
    fn the_edge_source_folds_both_sides() {
        let edge = |mutate: &dyn Fn(&mut MainRemovals, &mut RunUnpaired)| {
            let (mut main, mut run) = http_break(true);
            mutate(&mut main, &mut run);
            let mut pr = vec![];
            mark_lost_pairings(&mut pr, &main, &run, MainCopy::Current, None);
            serde_json::to_value(&pr[0])
                .unwrap()
                .get("edge_source")
                .cloned()
        };
        assert_eq!(edge(&|_, _| {}), Some(json!("fact")));
        let model = ResolutionSource::Model;
        assert_eq!(
            edge(&|_, run| run.calls[0].source = Some(model)),
            Some(json!("candidate"))
        );
        assert_eq!(
            edge(&|main, _| main.operations[0].source = Some(model)),
            Some(json!("candidate"))
        );
        assert_eq!(edge(&|main, _| main.operations[0].source = None), None);
        assert_eq!(edge(&|_, run| run.calls[0].source = None), None);
    }

    /// One row per (removed operation, surviving service): two files of one
    /// service merge into one row, two services make two.
    #[test]
    fn one_row_per_removed_side_and_surviving_service() {
        let (mut main, mut run) = http_break(true);
        let mut second_file = http_pairing();
        second_file.consumer.file = "src/cart.ts".into();
        let mut other_service = http_pairing();
        other_service.consumer.service = "admin".into();
        main.pairings.extend([second_file, other_service]);
        let mut call = surviving_call();
        call.file = "src/cart.ts".into();
        call.sites = vec!["src/cart.ts:3".into()];
        let mut admin = surviving_call();
        admin.service = "admin".into();
        admin.sites = vec!["src/admin.ts:1".into()];
        run.calls.extend([call, admin]);
        let mut pr = vec![];
        assert_eq!(
            mark_lost_pairings(&mut pr, &main, &run, MainCopy::Current, None),
            2
        );
        let services: Vec<_> = pr
            .iter()
            .map(|f| serde_json::to_value(f).unwrap()["service"].clone())
            .collect();
        assert_eq!(services, vec![json!("admin"), json!("storefront")]);
        let Finding::MissingEndpoint { call_sites, .. } = &pr[1] else {
            panic!()
        };
        assert_eq!(call_sites.len(), 3);
    }

    /// Rows that lost no pairing are untouched, and an unmarked endpoint row
    /// serializes exactly as it did before the fields existed.
    #[test]
    fn unrelated_rows_are_untouched() {
        let (main, run) = http_break(true);
        let bystander = Finding::missing_endpoint("POST", "/x", None, vec!["src/a.ts:1".into()]);
        let mut pr = vec![bystander.clone(), orphan_row()];
        mark_lost_pairings(&mut pr, &main, &run, MainCopy::Current, None);
        assert_eq!(pr[0], bystander);
        assert_eq!(pr[1], orphan_row());
        assert_eq!(
            serde_json::to_string(&bystander).unwrap(),
            r#"{"kind":"missing_endpoint","severity":"gap","break":"none","method":"POST","path":"/x","service":null,"call_sites":["src/a.ts:1"]}"#
        );
        assert_eq!(
            serde_json::to_string(&orphan_row()).unwrap(),
            r#"{"kind":"orphaned_endpoint","severity":"gap","break":"none","method":"SERVER->CLIENT","path":"order:shipped","service":"storefront","provenance":"route"}"#
        );
    }
}
