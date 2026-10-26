//! Site answers become index rows (carrick#2439, part of carrick#2441), and
//! fold with the file reader's rows at the same site.
//!
//! Every decided site's record ([`SiteRecord`]) either decides each slot
//! itself (bucket `forced`), asks the cloud (`asked`, through
//! [`crate::site_questions`]), or names nothing (`lead`). This module turns
//! the first two into rows on the index's operation keys and folds them with
//! the rows the file reader stated at the same file, line and side.
//!
//! - **Kind to key.** `http` is an HTTP key; `topic` a pub/sub key with no
//!   name scope; `message` and `event` pub/sub keys scoped `global` and
//!   `service`, as library-claim rows are; a site labelled `in_process_bus`
//!   is `service`-scoped whatever its kind, as library-claim rows are
//!   (`topic` and `socket_event` included); `socket_event` a socket key whose
//!   direction is read from the label's side: a send on a server's socket
//!   goes server to client, a send on a client's client to server, and a
//!   handler the other way. A side the label does not state (`both`, or one
//!   decided by origin) is `Unknown`.
//! - **Provenance** (decided on carrick#2439, 2026-10-16). A row whose every
//!   slot is forced is a fact: `request_summary` for an HTTP send,
//!   `library_claim` for the rest. A row with any asked slot is `model`, a
//!   candidate, and carries [`Inference`]. No new resolution source.
//! - **The fold** (RULED by David, 2026-10-16). At one site and side: a fact
//!   the scan already states keeps the site and the site row is dropped. A
//!   fact site row replaces the reader's rows. An answer replaces them when
//!   its band slot is at or above [`BAND`] with both option orders agreeing;
//!   otherwise the reader's rows stand and carry the answer as
//!   `inference.alternative`. With no row at the site an answer in the band
//!   stands, and one below it is a possible call
//!   ([`crate::possible_calls`], reason `below_band`).
//! - **Callers** (carrick#2614). A call whose request is decided lower on
//!   its chain (`await login(…)`, where `login` writes the path) is no
//!   second request ([`crate::request_summary::decided_sites::Through`]): a
//!   reader row there folds onto the site when a row standing at the site
//!   after the fold agrees on its key ([`agrees`]). It folds as well when it
//!   disagrees (RULED, carrick#2704) where every row at the sites the call
//!   reaches is a fact and they state one operation: the reader misread
//!   that request. A disagreeing row stands beside a candidate, or beside
//!   facts that state several operations.
//! - **The band** (amended by David, 2026-10-17). An answer is in band only
//!   when every slot the model answered that the row depends on (kind, role,
//!   name, method, namespace: the slots that feed the key and the side) is at
//!   or above [`BAND`] with its option orders agreeing. Forced slots count as
//!   1.0 and agreeing; with no such slot asked, nothing in the key was the
//!   model's and the row is in band. [`SiteInference::confidence`] is the
//!   lowest of those slots and `orders_agree` is true only if all agree.
//! - **Unanswered.** An asked site the cloud did not answer states no row
//!   and withdraws none; it is a possible call (`unanswered`).
//! - **Corroboration** (RULED by David, 2026-10-18, carrick#2520). A slot
//!   the label forces with no value the code states is not a fact on its
//!   own. A row whose kind, role, method or name rests on such a slot is
//!   written only when the code states the name ([`Basis::Code`]: a literal
//!   or a path the request summaries compose) or a reader row at the same
//!   file and line agrees on its key ([`agrees`]). Otherwise the site is a
//!   possible call, `uncorroborated`. Kind and role are always the label's,
//!   so in practice: a row whose name the model answered needs an agreeing
//!   reader row.
//! - **Switch.** Nothing here runs unless `CARRICK_SITE_QUESTIONS=1`
//!   ([`SiteRows::enabled`]); with it off the blob is byte for byte as
//!   before.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agents::file_analyzer_agent::ResolutionSource;
use crate::analyzer::ApiEndpointDetails;
use crate::cloud_storage::CloudRepoData;
use crate::operation::{OperationKey, SocketDirection};
use crate::possible_calls::{PossibleCall, UnreadReason};
use crate::request_summary::decided_sites::{Decided, DecidedSite, SiteLabel, Through};
use crate::request_summary::site_record::{Basis, Bucket, SiteRecord, TypeAnchors};
use crate::site_questions::{Answered, SiteAnswers, SiteQuestion, Slot, SlotAnswer, SlotValue};

/// The fold band (RULED by David, 2026-10-16): an answer replaces the
/// reader's row at this confidence or above, with both option orders
/// agreeing. In the concept's run 2 the kind answers in this band were right
/// 43 times of 43.
pub const BAND: f64 = 0.8;

/// Who inferred a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceBy {
    /// The site questions (carrick#2437).
    SiteQuestion,
}

/// What a site answer says about the row it stated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteInference {
    pub by: InferenceBy,
    /// The cloud's question-set id the answer was given under.
    pub question_set: Option<String>,
    /// The band slot's confidence ([`BAND`]).
    pub confidence: f64,
    /// Whether both option orders picked the same option for that slot.
    pub orders_agree: bool,
}

/// Which side of the index a row sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowSide {
    /// A call: an HTTP request, a publish, a socket send.
    Call,
    /// An endpoint: an HTTP route, a subscriber, a socket listener.
    Endpoint,
}

/// A site answer a reader row kept its place against: the answer, and the
/// operation it named.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alternative {
    #[serde(flatten)]
    pub answer: SiteInference,
    pub side: RowSide,
    #[serde(flatten)]
    pub key: OperationKey,
}

/// `inference` on an index row (carrick#2439): on a row a site answer
/// stated, the answer; on a reader row an answer below the band disagreed
/// with, only `alternative`. Absent on every other row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Inference {
    #[serde(flatten)]
    pub answer: Option<SiteInference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternative: Option<Alternative>,
}

/// One row a site states.
#[derive(Debug, Clone, PartialEq)]
pub struct SiteRow {
    /// Repo-relative, `/`-separated.
    pub file: String,
    pub line: u32,
    pub key: OperationKey,
    pub side: RowSide,
    pub source: ResolutionSource,
    /// `None` on a fact.
    pub inference: Option<SiteInference>,
    pub in_band: bool,
    pub name_scope: Option<carrick_match::NameScope>,
    pub anchors: TypeAnchors,
    /// The possible call the site is when it stands nowhere.
    pub lead: PossibleCall,
    /// Whether the row rests on a slot only the label forced and the code
    /// does not state its name (carrick#2520): it stands only where a reader
    /// row at its line agrees on the key.
    pub needs_reader: bool,
    /// A dispatch row's handler (carrick#2474 R4b, [`dispatch`]): the
    /// functions in which a reader endpoint row agreeing on its key is the
    /// same route read again, at another line. Empty for a site's row.
    pub within: Vec<crate::request_summary::dispatch::LineRange>,
}

impl SiteRow {
    fn fact(&self) -> bool {
        self.inference.is_none()
    }
}

/// A service's site rows and possible calls, before the fold.
#[derive(Debug, Clone, Default)]
pub struct SiteRows {
    /// Whether the switch was on: off, nothing folds and nothing is written.
    pub enabled: bool,
    pub rows: Vec<SiteRow>,
    /// The sites that are possible calls whatever the fold does: leads,
    /// unanswered sites, and sites whose answer states no row.
    pub leads: Vec<PossibleCall>,
    /// HTTP route sites no row can be written for ([`unwritable_route`],
    /// carrick#2517), each as the possible call it is. The fold keeps one
    /// only where no row stands at its line: elsewhere the index already
    /// holds the route, and the site is counted, not listed.
    pub routes: Vec<PossibleCall>,
    /// Calls above a decided site on its chain (carrick#2614), whose reader
    /// rows fold onto the row the site holds.
    pub through: Vec<Through>,
    /// With the receiver trace on, the route sites it serves at a full path
    /// (carrick#2474 R3 S1, [`traced`]): the engine writes them into the
    /// mount graph.
    pub traced: TracedRoutes,
    /// With the receiver trace on, where the service's route chains leave
    /// it (carrick#2474 R5a): settled across services at the engine's join
    /// point (carrick#2474 R5b, [`crate::route_boundaries`]).
    pub boundaries: crate::request_summary::decided_sites::RouteBoundaries,
}

/// A route the receiver trace serves at a full path it composed from the
/// top (carrick#2474 R3 S1): every instance end of its site is `top` and
/// every prefix is text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TracedRoute {
    /// Repo-relative, `/`-separated.
    pub file: String,
    pub line: u32,
    /// The method the label forces, upper case.
    pub method: String,
    /// The path the route's own call writes.
    pub path: String,
    /// Every full path it is served at, sorted, one per stated value.
    pub full_paths: Vec<String>,
    /// The handler's name where the call hands it by name.
    pub handler: Option<String>,
    /// The symbol of the binding the route is registered on.
    pub owner: Option<String>,
    /// Whether a reader row at the route's own path with no prefix takes
    /// the traced path (carrick#2474 row 4). A procedure's call writes no
    /// path of its own, so a row at its line states some other reading and
    /// stays (carrick#2474 R6c).
    pub corrects: bool,
}

/// The route sites the receiver trace serves at a full path, and the rest
/// counted by why not ([`traced`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TracedRoutes {
    pub routes: Vec<TracedRoute>,
    /// Route sites the trace does not serve at a full path, by reason:
    /// `method_unstated`, `trimmed`, `no_instance`, `mount_unverified`,
    /// `hole_<end>` (an instance end that is not the top), `unread_prefix`.
    pub untraced: std::collections::BTreeMap<String, usize>,
}

/// The routes the receiver trace serves at a full path (carrick#2474 R3
/// S1): each record the label states as a route ([`label_states_a_route`])
/// whose method the label forces and whose decided site composed a path
/// for every instance end, each ending at the top. A site with any other
/// end, a prefix that is not text, or paths left out at the cap keeps the
/// reader's reading, counted by reason.
pub fn traced(decided: &Decided, records: &[SiteRecord]) -> TracedRoutes {
    let mut out = TracedRoutes::default();
    // Where the procedure walk stopped with no procedure to count it on:
    // counted apart from a procedure nothing serves.
    if decided.procedure_stops > 0 {
        *out.untraced
            .entry("procedure_walk_stopped".to_string())
            .or_default() += decided.procedure_stops;
    }
    for record in records {
        if record.bucket == Bucket::Lead || !label_states_a_route(record) {
            continue;
        }
        let Some(site) = site_of(decided, &record.anchors) else {
            continue;
        };
        if let Some(procedure) = &site.procedure {
            // Each adapter and key path that reached it and named no path,
            // and a procedure nothing serves.
            for (reason, count) in &procedure.unread {
                *out.untraced
                    .entry(format!("procedure_{reason}"))
                    .or_default() += count;
            }
            if procedure.full_paths.is_empty() {
                if procedure.unread.is_empty() {
                    *out.untraced
                        .entry("procedure_unserved".to_string())
                        .or_default() += 1;
                }
                continue;
            }
            match procedure_route(site, procedure, record) {
                Ok(route) => out.routes.push(route),
                Err(reason) => *out.untraced.entry(reason).or_default() += 1,
            }
            continue;
        }
        match traced_route(site, record) {
            Ok(route) => out.routes.push(route),
            Err(reason) => *out.untraced.entry(reason).or_default() += 1,
        }
    }
    out
}

/// The HTTP method the label forces at `record`'s site.
fn forced_method(record: &SiteRecord) -> Result<&String, String> {
    record
        .state
        .forced
        .get(&Slot::Method)
        .filter(|method| crate::type_manifest::is_producer_method(method))
        .ok_or_else(|| "method_unstated".to_string())
}

/// The route a procedure op's site is served as (carrick#2474 R6c), at
/// every path its adapters serve it at: the method the label forces, the
/// handler handed by name, and the router binding that collects it as its
/// owner. A member the label states no method for is no HTTP operation.
fn procedure_route(
    site: &DecidedSite,
    procedure: &crate::request_summary::decided_sites::Procedure,
    record: &SiteRecord,
) -> Result<TracedRoute, String> {
    let method = forced_method(record)?;
    let key_path = procedure.key_paths.first().cloned().unwrap_or_default();
    Ok(TracedRoute {
        file: site.file.clone(),
        line: site.line,
        method: method.to_uppercase(),
        path: crate::utils::join_prefix_and_path("/", &key_path),
        full_paths: procedure.full_paths.clone(),
        handler: site
            .handler
            .as_ref()
            .and_then(|handler| handler.name.clone()),
        owner: procedure.owner.clone(),
        corrects: false,
    })
}

/// `site`'s traced route, or why it has none.
fn traced_route(site: &DecidedSite, record: &SiteRecord) -> Result<TracedRoute, String> {
    let method = forced_method(record)?;
    if site.composed_trimmed {
        return Err("trimmed".to_string());
    }
    if site.composed.is_empty() {
        return Err("no_instance".to_string());
    }
    if let Some(end) = site
        .composed
        .iter()
        .map(|composed| composed.end.as_str())
        .find(|end| *end != "top")
    {
        return Err(match end {
            "mount_unverified" => end.to_string(),
            _ => format!("hole_{end}"),
        });
    }
    let (Some(path), Some(mut full_paths)) = (
        site.name_text(),
        site.composed
            .iter()
            .map(|composed| composed.path.clone())
            .collect::<Option<Vec<String>>>(),
    ) else {
        return Err("unread_prefix".to_string());
    };
    full_paths.sort();
    full_paths.dedup();
    Ok(TracedRoute {
        file: site.file.clone(),
        line: site.line,
        method: method.to_uppercase(),
        path,
        full_paths,
        handler: site
            .handler
            .as_ref()
            .and_then(|handler| handler.name.clone()),
        owner: site
            .receiver
            .as_ref()
            .and_then(|receiver| receiver.symbol.clone()),
        corrects: true,
    })
}

/// The id a site is asked under: its file and call span, unique in a run.
pub fn site_id(anchors: &TypeAnchors) -> String {
    format!(
        "{}:{}-{}",
        anchors.call.file, anchors.call.span_start, anchors.call.span_end
    )
}

/// Whether the label states that `record`'s site registers an HTTP route:
/// kind `http` and role `handles` both forced. No route can be written from
/// such a site ([`unwritable_route`]: it names no router), so it is never
/// asked and never a row (carrick#2517).
fn label_states_a_route(record: &SiteRecord) -> bool {
    let forced = |slot| record.state.forced.get(&slot).map(String::as_str);
    forced(Slot::Kind) == Some("http") && forced(Slot::Role) == Some("handles")
}

/// The questions a service's records ask: one per record that asks a slot,
/// except a route site ([`label_states_a_route`]), whose answer could not
/// be written.
pub fn questions(records: &[SiteRecord]) -> Vec<SiteQuestion> {
    records
        .iter()
        .filter(|record| record.bucket == Bucket::Asked && !label_states_a_route(record))
        .map(|record| SiteQuestion {
            site_id: site_id(&record.anchors),
            state: record.state.clone(),
        })
        .collect()
}

/// A slot's value: forced (confidence 1, both orders), answered, or `None`
/// when the record neither forces nor asks it.
#[derive(Debug, Clone, PartialEq)]
struct Filled {
    text: Option<String>,
    confidence: f64,
    orders_agree: bool,
    asked: bool,
}

fn filled(record: &SiteRecord, answered: Option<&Answered>, slot: Slot) -> Option<Filled> {
    if let Some(text) = record.state.forced.get(&slot) {
        return Some(Filled {
            text: Some(text.clone()),
            confidence: 1.0,
            orders_agree: true,
            asked: false,
        });
    }
    let answer: &SlotAnswer = match slot {
        Slot::Kind => answered?.kind.as_ref(),
        Slot::Role => answered?.role.as_ref(),
        Slot::Name => answered?.name.as_ref(),
        Slot::Method => answered?.method.as_ref(),
        Slot::Namespace => answered?.namespace.as_ref(),
        Slot::NameArgument => answered?.name_argument.as_ref(),
        Slot::PayloadArgument => answered?.payload_argument.as_ref(),
    }?;
    Some(Filled {
        text: match &answer.value {
            Some(SlotValue::Text(text)) => Some(text.clone()),
            _ => None,
        },
        confidence: answer.confidence,
        orders_agree: answer.orders_agree,
        asked: true,
    })
}

/// The socket direction a call or handler on a socket the label says is a
/// `side` end takes. `None` for a side the label does not state.
fn direction(side: Option<&str>, row: RowSide) -> SocketDirection {
    match (side, row) {
        (Some("server"), RowSide::Call) | (Some("client"), RowSide::Endpoint) => {
            SocketDirection::ServerToClient
        }
        (Some("client"), RowSide::Call) | (Some("server"), RowSide::Endpoint) => {
            SocketDirection::ClientToServer
        }
        _ => SocketDirection::Unknown,
    }
}

/// The key a kind names for `name`, and its name scope.
fn key_of(
    kind: &str,
    name: String,
    method: Option<&str>,
    namespace: Option<String>,
    side: Option<&str>,
    row: RowSide,
) -> Option<(OperationKey, Option<carrick_match::NameScope>)> {
    let scoped = |scope: &str| carrick_match::NameScope {
        scope: scope.to_string(),
        namespace: namespace.clone(),
    };
    Some(match kind {
        "http" => (OperationKey::http(method.unwrap_or_default(), name), None),
        "topic" => (OperationKey::pubsub(name), None),
        "message" => (
            OperationKey::pubsub(name),
            Some(scoped(carrick_match::GLOBAL_SCOPE)),
        ),
        "event" => (
            OperationKey::pubsub(name),
            Some(scoped(carrick_match::SERVICE_SCOPE)),
        ),
        "socket_event" => (OperationKey::socket(name, direction(side, row)), None),
        _ => return None,
    })
}

/// The name scope of a non-HTTP row at a site whose labelled end has the role
/// `in_process_bus`: `service`, whatever kind the answer names, with the
/// answered namespace kept. An in-process name never leaves its service, as
/// the library-claim writer (`library_claims.rs`, `scoped`, ruling D4 on
/// carrick#1564) already holds. Every other role keeps the scope its kind
/// gave, and HTTP keys carry none.
fn in_process_scope(
    role: &str,
    key: &OperationKey,
    scope: Option<carrick_match::NameScope>,
) -> Option<carrick_match::NameScope> {
    if role != "in_process_bus" || matches!(key, OperationKey::Http { .. }) {
        return scope;
    }
    Some(carrick_match::NameScope {
        scope: carrick_match::SERVICE_SCOPE.to_string(),
        namespace: scope.and_then(|scope| scope.namespace),
    })
}

/// Why an HTTP route a site answer states cannot be written as an endpoint
/// (carrick#2517), or `None` for any other row.
///
/// - **No method.** A route is keyed on its method; with none, or a word no
///   route is registered under, [`OperationKey::http`] would write `UNKNOWN`.
///   A member that mounts an application under a prefix answers this way.
/// - **No owner.** A route's served path is its registration's path under
///   every prefix its router is mounted at. The reader composes it from the
///   router each row hangs from (`owner_node`, which
///   [`crate::registration_scope::apply_registration_scopes`] re-points to a
///   [`crate::registration_scope::scope_id`] inside an inline plugin) when it
///   resolves the mount graph. A site carries no router: its record names the
///   call and the library member it reaches, not the instance it is made on,
///   and the mount graph is resolved before site rows are read. Written
///   as it stands, a route under a prefix would lose it, and in band it
///   would replace the reader's prefixed row at the same line. Until a site
///   can name its router it goes to [`SiteRows::routes`]: a possible call
///   only where no row stands at its line ([`fold_rows`]).
fn unwritable_route(kind: &str, side: RowSide, method: Option<&str>) -> Option<UnreadReason> {
    if kind != "http" || side != RowSide::Endpoint {
        return None;
    }
    if !method.is_some_and(crate::type_manifest::is_producer_method) {
        return Some(UnreadReason::NoMethod);
    }
    Some(UnreadReason::OwnerUnknown)
}

/// Whether `record`'s row rests on a slot only the label forced, with a
/// name the code does not state (carrick#2520, module docs).
fn needs_reader(record: &SiteRecord) -> bool {
    let rests_on_label = [Slot::Kind, Slot::Role, Slot::Method, Slot::Name]
        .iter()
        .any(|slot| record.basis.get(slot) == Some(&Basis::Label));
    rests_on_label && record.basis.get(&Slot::Name) != Some(&Basis::Code)
}

/// An HTTP path as the agreement check reads it: a leading base written as a
/// value (`${process.env.API}`) dropped, and every segment written as a
/// value (`${id}`) a `:` parameter, so `${base}/tasks/${id}` and
/// `/tasks/:taskId` read alike.
fn route_shape(path: &str) -> String {
    let written = |segment: &str| segment.starts_with("${") && segment.ends_with('}');
    let mut segments: Vec<&str> = path.split('/').collect();
    if segments.first().is_some_and(|first| written(first)) {
        segments[0] = "";
    }
    segments
        .into_iter()
        .map(|segment| if written(segment) { ":p" } else { segment })
        .collect::<Vec<_>>()
        .join("/")
}

/// Whether a reader row's key agrees with a site row's: the same key, or
/// for HTTP the same method and paths ([`route_shape`]) that match with at
/// least one literal segment in common ([`carrick_match::match_agreement`]),
/// either way round.
pub(crate) fn agrees(reader: &OperationKey, site: &OperationKey) -> bool {
    match (reader, site) {
        (
            OperationKey::Http {
                method: reader_method,
                path: reader_path,
            },
            OperationKey::Http {
                method: site_method,
                path: site_path,
            },
        ) => {
            let (reader_path, site_path) = (route_shape(reader_path), route_shape(site_path));
            let agreement = |a: &str, b: &str| {
                carrick_match::match_agreement(a, b).is_some_and(|literal| literal > 0)
            };
            reader_method.eq_ignore_ascii_case(site_method)
                && (agreement(&reader_path, &site_path) || agreement(&site_path, &reader_path))
        }
        _ => reader == site,
    }
}

/// Whether a reader row's key is a dispatch row's: for HTTP the same method
/// and the same path, as written.
fn same_route(reader: &OperationKey, site: &OperationKey) -> bool {
    match (reader, site) {
        (
            OperationKey::Http {
                method: reader_method,
                path: reader_path,
            },
            OperationKey::Http {
                method: site_method,
                path: site_path,
            },
        ) => reader_method.eq_ignore_ascii_case(site_method) && reader_path == site_path,
        _ => reader == site,
    }
}

/// The decided site a record was built for.
fn site_of<'d>(decided: &'d Decided, anchors: &TypeAnchors) -> Option<&'d DecidedSite> {
    decided.sites.iter().find(|site| {
        site.file == anchors.call.file
            && site.span_start == anchors.call.span_start
            && site.span_end == anchors.call.span_end
            && site.key == anchors.key
    })
}

/// The rows and possible calls a service's records come to, read with the
/// labels the store answered and the cloud's answers. The lead-bucket
/// records and the undecided calls are [`crate::possible_calls::from_sites`]'s.
pub fn of(
    decided: &Decided,
    records: &[SiteRecord],
    labels: &[SiteLabel],
    answers: &SiteAnswers,
) -> SiteRows {
    let mut out = SiteRows {
        enabled: true,
        rows: Vec::new(),
        leads: crate::possible_calls::from_sites(decided, records),
        routes: Vec::new(),
        through: decided.through.clone(),
        traced: TracedRoutes::default(),
        boundaries: Default::default(),
    };
    for record in records {
        if record.bucket == Bucket::Lead {
            continue;
        }
        let Some(site) = site_of(decided, &record.anchors) else {
            continue;
        };
        let lead = |reason| crate::possible_calls::from_site(site, reason);
        if label_states_a_route(record) {
            out.routes.push(lead(UnreadReason::OwnerUnknown));
            continue;
        }
        let answered = answers.answered.get(&site_id(&record.anchors));
        if record.bucket == Bucket::Asked && answered.is_none() {
            out.leads.push(lead(UnreadReason::Unanswered));
            continue;
        }
        let slot = |slot| filled(record, answered, slot);
        let (Some(kind), Some(role), Some(name)) =
            (slot(Slot::Kind), slot(Slot::Role), slot(Slot::Name))
        else {
            out.leads.push(lead(UnreadReason::BelowBand));
            continue;
        };
        let (Some(kind_text), Some(role_text), Some(name_text)) =
            (kind.text.clone(), role.text.clone(), name.text.clone())
        else {
            // The model chose none of the options for a slot the key needs.
            out.leads.push(lead(UnreadReason::BelowBand));
            continue;
        };
        let side = match role_text.as_str() {
            "sends" => RowSide::Call,
            "handles" => RowSide::Endpoint,
            // `neither` is no network call, and a scope site names a
            // prefix, not an operation.
            _ => continue,
        };
        let label = SiteLabel::find(labels, &site.end);
        let method_slot = slot(Slot::Method);
        let namespace_slot = slot(Slot::Namespace);
        let method = method_slot.as_ref().and_then(|method| method.text.clone());
        let namespace = namespace_slot
            .as_ref()
            .and_then(|namespace| namespace.text.clone());
        if let Some(reason) = unwritable_route(&kind_text, side, method.as_deref()) {
            out.routes.push(lead(reason));
            continue;
        }
        let Some((key, name_scope)) = key_of(
            &kind_text,
            name_text,
            method.as_deref(),
            namespace,
            label.and_then(|label| label.side.as_deref()),
            side,
        ) else {
            out.leads.push(lead(UnreadReason::BelowBand));
            continue;
        };
        let name_scope = in_process_scope(&site.end.role, &key, name_scope);
        let fact = record.bucket == Bucket::Forced;
        // Every slot the model answered that feeds the key or the side:
        // forced slots are 1.0 and agreeing, so only asked ones can lower it.
        let (confidence, orders_agree) = [Some(&kind), Some(&role), Some(&name)]
            .into_iter()
            .chain([method_slot.as_ref(), namespace_slot.as_ref()])
            .flatten()
            .filter(|slot| slot.asked)
            .fold((1.0_f64, true), |(low, agree), slot| {
                (low.min(slot.confidence), agree && slot.orders_agree)
            });
        let source = match (fact, &key) {
            (true, OperationKey::Http { .. }) if side == RowSide::Call => {
                ResolutionSource::RequestSummary
            }
            (true, _) => ResolutionSource::LibraryClaim,
            (false, _) => ResolutionSource::Model,
        };
        out.rows.push(SiteRow {
            file: site.file.clone(),
            line: site.line,
            key,
            side,
            source,
            inference: (!fact).then(|| SiteInference {
                by: InferenceBy::SiteQuestion,
                question_set: answers.question_set.clone(),
                confidence,
                orders_agree,
            }),
            in_band: fact || (confidence >= BAND && orders_agree),
            name_scope,
            anchors: record.anchors.clone(),
            lead: lead(UnreadReason::BelowBand),
            needs_reader: needs_reader(record),
            within: Vec::new(),
        });
    }
    out
}

/// What the fold did, for the type manifest and the possible calls.
#[derive(Debug, Clone, Default)]
pub struct SiteFold {
    /// The site rows the index now holds, which the type manifest anchors
    /// at their records' anchors.
    pub stated: Vec<SiteRow>,
    /// The reader rows the fold replaced, as `(file, line, key, side)`:
    /// their manifest entries would anchor an operation no row holds.
    pub withdrawn: Vec<(String, u32, OperationKey, RowSide)>,
    /// Every possible call: the leads, the answers below the band at a site
    /// with no row, and the route sites with no row at their line.
    pub leads: Vec<PossibleCall>,
    /// Route sites ([`SiteRows::routes`]) where a row already stands at the
    /// line: the index holds the route, so the site is no possible call.
    pub route_kept_reader: usize,
    /// Leads dropped because the blob already holds a row at their line
    /// (carrick#2539): the index states that site, so it is no possible call.
    pub lead_on_row_line: usize,
    /// Reader rows at a call above a decided site folded onto the row the
    /// site holds (carrick#2614), as `(file, line, key)`. Each is in
    /// `withdrawn` as well.
    pub through_folded: Vec<(String, u32, OperationKey)>,
}

/// `file` relative to `root`, `/`-separated, whichever form it is held in
/// ([`crate::utils::repo_relative_source_path`]).
pub fn relative(root: &Path, file: &str) -> String {
    crate::utils::repo_relative_source_path(file, &root.to_string_lossy()).replace('\\', "/")
}

/// A row's `file:line`, relative to `root`.
fn place(root: &Path, row: &ApiEndpointDetails) -> Option<(String, u32)> {
    let text = row.file_path.to_string_lossy();
    let (file, line) = text.rsplit_once(':')?;
    Some((relative(root, file), line.parse().ok()?))
}

/// Fold `site`'s rows into `data` (module docs). `make` builds a row the
/// way the index builds a protocol row of that side. Off, nothing changes.
pub fn fold(
    data: &mut CloudRepoData,
    site: &SiteRows,
    root: &Path,
    make: impl Fn(OperationKey, &Path, u32, RowSide) -> ApiEndpointDetails,
) -> SiteFold {
    let fold = fold_rows(&mut data.calls, &mut data.endpoints, site, root, make);
    // A folded caller's row leaves the graph's calls too: the cross-service
    // pass reads them (`call_through::settle`), and would read the folded
    // row as a call of its own (carrick#2704).
    if let Some(graph) = data.mount_graph.as_mut() {
        graph.data_calls.retain(|call| {
            !fold.through_folded.iter().any(|(file, line, key)| {
                call.resolution_source == Some(ResolutionSource::Model)
                    && call.file_location.rsplit_once(':').is_some_and(|(f, l)| {
                        relative(root, f) == *file && l.parse::<u32>().ok() == Some(*line)
                    })
                    && key.as_http().is_some_and(|(method, path)| {
                        call.method.eq_ignore_ascii_case(method) && call.canonical_path == path
                    })
            })
        });
    }
    fold
}

/// [`fold`] over a blob's calls and endpoints.
pub fn fold_rows(
    calls: &mut Vec<ApiEndpointDetails>,
    endpoints: &mut Vec<ApiEndpointDetails>,
    site: &SiteRows,
    root: &Path,
    make: impl Fn(OperationKey, &Path, u32, RowSide) -> ApiEndpointDetails,
) -> SiteFold {
    let mut out = SiteFold::default();
    if !site.enabled {
        return out;
    }
    // A lead on a line the blob already holds a row at is that row's site.
    // A consumer lead matches a call row, a producer lead an endpoint row,
    // and a lead with no role either.
    let held_at = |rows: &[ApiEndpointDetails], file: &str, line: u32| {
        rows.iter()
            .any(|existing| place(root, existing).is_some_and(|(f, l)| f == file && l == line))
    };
    for lead in &site.leads {
        let on_call = held_at(calls, &lead.file_path, lead.line_number);
        let on_endpoint = held_at(endpoints, &lead.file_path, lead.line_number);
        let held = match lead.role.as_deref() {
            Some("consumer") => on_call,
            Some("producer") => on_endpoint,
            _ => on_call || on_endpoint,
        };
        if held {
            out.lead_on_row_line += 1;
        } else {
            out.leads.push(lead.clone());
        }
    }
    // The same file:line match as the rows below, on the endpoint side.
    for route in &site.routes {
        let held = endpoints.iter().any(|existing| {
            place(root, existing)
                .is_some_and(|(file, line)| file == route.file_path && line == route.line_number)
        });
        if held {
            out.route_kept_reader += 1;
        } else {
            out.leads.push(route.clone());
        }
    }
    for row in &site.rows {
        let rows = match row.side {
            RowSide::Call => &mut *calls,
            RowSide::Endpoint => &mut *endpoints,
        };
        let here: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, existing)| {
                place(root, existing)
                    .is_some_and(|(file, line)| file == row.file && line == row.line)
            })
            .map(|(index, _)| index)
            .collect();
        // A fact the scan states keeps the site.
        if here.iter().any(|&index| !is_reader(&rows[index])) {
            continue;
        }
        let readers: Vec<usize> = here;
        // A row on a label-only slot stands only with a reader row that
        // agrees (carrick#2520).
        if row.needs_reader
            && !readers
                .iter()
                .any(|&index| agrees(&rows[index].key, &row.key))
        {
            let mut lead = row.lead.clone();
            lead.unread.reason = UnreadReason::Uncorroborated;
            out.leads.push(lead);
            continue;
        }
        let replaces = row.fact() || row.in_band;
        if !readers.is_empty() && !replaces {
            // The reader's rows stand, carrying the answer.
            let answer = row
                .inference
                .clone()
                .expect("a candidate carries its answer");
            for index in readers {
                rows[index].inference = Some(Inference {
                    answer: None,
                    alternative: Some(Alternative {
                        answer: answer.clone(),
                        side: row.side,
                        key: row.key.clone(),
                    }),
                });
            }
            continue;
        }
        if readers.is_empty() && !replaces {
            out.leads.push(row.lead.clone());
            continue;
        }
        // Replaced: the reader's rows go, and the site row stands.
        let mut withdrawn: Vec<usize> = readers;
        withdrawn.sort_unstable();
        for index in withdrawn.into_iter().rev() {
            let gone = rows.remove(index);
            out.withdrawn
                .push((row.file.clone(), row.line, gone.key, row.side));
        }
        // A dispatch row: a reader row in its handler with its key is the
        // same route read again at another line (carrick#2474 R4b). Only the
        // same key: `/api/:id` or `/api/*` beside a compared `/api/one` is
        // another route of the handler (carrick#2693).
        let mut index = 0;
        while !row.within.is_empty() && index < rows.len() {
            let inside = place(root, &rows[index]).filter(|(file, line)| {
                row.within
                    .iter()
                    .any(|range| range.file == *file && range.first <= *line && *line <= range.last)
            });
            match inside {
                Some((file, line))
                    if is_reader(&rows[index]) && same_route(&rows[index].key, &row.key) =>
                {
                    let gone = rows.remove(index);
                    out.withdrawn.push((file, line, gone.key, row.side));
                }
                _ => index += 1,
            }
        }
        let mut stated = make(row.key.clone(), Path::new(&row.file), row.line, row.side);
        stated.resolution_source = Some(row.source);
        stated.name_scope = row.name_scope.clone();
        stated.inference = row.inference.clone().map(|answer| Inference {
            answer: Some(answer),
            alternative: None,
        });
        rows.push(stated);
        out.stated.push(row.clone());
    }
    // A reader row at a call above a decided site, for an operation a row at
    // the site states, is that request read again at its caller
    // (carrick#2614). The site's row is whichever stands there after the
    // fold above: the scan's, the site's, or the reader's. A row that agrees
    // with one of them folds. So does one that disagrees, where every row at
    // the sites the call reaches is a fact and together they state one
    // operation (carrick#2704): the call makes that request, and the
    // reader's other key is a wrong reading of it, not a second request.
    // No row there, a candidate, or several operations, and a disagreeing
    // caller's row stands.
    let at = |row: &ApiEndpointDetails, file: &str, line: u32| {
        place(root, row).is_some_and(|(f, l)| f == file && l == line)
    };
    let mut callers: BTreeMap<(&str, u32), Vec<&Through>> = BTreeMap::new();
    for through in &site.through {
        callers
            .entry((through.file.as_str(), through.line))
            .or_default()
            .push(through);
    }
    for ((file, line), throughs) in callers {
        let stated: Vec<&ApiEndpointDetails> = calls
            .iter()
            .filter(|row| {
                throughs
                    .iter()
                    .any(|through| at(row, &through.site_file, through.site_line))
            })
            .collect();
        let keys: Vec<OperationKey> = stated.iter().map(|row| row.key.clone()).collect();
        // A second call on the caller's line may be the reader row's own.
        let one_fact = !stated.is_empty()
            && !throughs.iter().any(|through| through.other_call)
            && stated.iter().all(|row| is_fact(row))
            && keys.iter().all(|key| *key == keys[0]);
        let folds = |row: &ApiEndpointDetails| {
            is_reader(row)
                && at(row, file, line)
                && (one_fact || keys.iter().any(|key| agrees(&row.key, key)))
        };
        let mut index = 0;
        while index < calls.len() {
            if folds(&calls[index]) {
                let gone = calls.remove(index);
                out.through_folded
                    .push((file.to_string(), line, gone.key.clone()));
                out.withdrawn
                    .push((file.to_string(), line, gone.key, RowSide::Call));
            } else {
                index += 1;
            }
        }
    }
    out
}

/// A row a fact states: the scan's own, or a site row every slot of which
/// the code decided. A site answer is the model's, a candidate.
fn is_fact(row: &ApiEndpointDetails) -> bool {
    row.resolution_source
        .is_some_and(|source| source != ResolutionSource::Model)
}

/// A row the file reader stated, which no site answer has touched.
fn is_reader(row: &ApiEndpointDetails) -> bool {
    row.resolution_source == Some(ResolutionSource::Model) && row.inference.is_none()
}

pub mod dispatch;

#[cfg(test)]
mod tests;
