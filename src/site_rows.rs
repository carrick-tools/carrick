//! Site answers become index rows (carrick#2439, part of carrick#2441), and
//! fold with the file reader's rows at the same site.
//!
//! Every decided site's record ([`SiteRecord`]) either decides each slot
//! itself (bucket `forced`), asks the cloud (`asked`, through
//! [`crate::site_questions`]), or names nothing (`lead`). This module turns
//! the first two into rows on the index's operation keys and folds them with
//! the rows the file reader stated at the same file, line and side.
//!
//! - **Kind to key.** `http` is an HTTP key; `topic`, `message`, `event` and
//!   `socket_event` are message keys whose name scope and socket direction
//!   are [`crate::message_key`]'s, the same the library-claim writer asks
//!   (carrick#3498): the scope the label's claim states wins, an
//!   `in_process_bus` is `service` whatever the claim says, then the kind
//!   (`message` `global`, `event` `service`), else no scope (a `topic` or
//!   `socket_event` nothing scopes crosses services, carrick#3531). A socket's
//!   direction is read from the label's side: a send on a server's socket
//!   goes server to client, a send on a client's client to server, and a
//!   handler the other way; `both` is `Unknown`. A label the store marks
//!   `by_origin` takes its side from the instance the receiver trace walked
//!   to (`socket_side`, carrick#1670). A side nothing states (no trace) is
//!   `Unknown`.
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
//! - **Ran.** Nothing folds unless the site pass read the service
//!   ([`SiteRows::enabled`]); a service it never read keeps its blob as the
//!   other passes wrote it.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agents::file_analyzer_agent::ResolutionSource;
use crate::analyzer::ApiEndpointDetails;
use crate::cloud_storage::CloudRepoData;
use crate::mount_graph::{ConsumerRole, DataFetchingCall};
use crate::operation::{OperationKey, SocketDirection};
use crate::possible_calls::PossibleCall;
use crate::request_summary::Stop;
use crate::request_summary::decided_sites::{
    Decided, DecidedSite, SiteLabel, Through, origin_side,
};
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
    /// Whether the decided call is not the labelled call itself: it reaches
    /// the library call through a function (carrick#2795). The mirrored graph
    /// row of such a fact is a `wrapper_call`, never a request of its own.
    pub through: bool,
    /// A dispatch row's handler (carrick#2474 R4b, [`dispatch`]): the
    /// functions in which a reader endpoint row agreeing on its key is the
    /// same route read again, at another line. Empty for a site's row.
    pub within: Vec<crate::request_summary::dispatch::LineRange>,
    /// The base an HTTP request the site states is written against, read
    /// from where its leading value ends and never from how the key spells it
    /// (carrick#3387): what [`crate::url_normalizer::UrlNormalizer::consumer_call_path`]
    /// keys it with, as it keys every consumer row from `DataCall.base`. Only
    /// its kind is read, so the defaults beside an environment read are not
    /// looked up here. `None` on a row that is not an HTTP request, and where
    /// the answered name is not the one the code writes.
    pub base: Option<crate::call_base::CallBaseResolution>,
    /// The procedure a client procedure call names (carrick#3186):
    /// its key is an HTTP key with the RPC word as its method and an empty
    /// path, and this is its identity. The repository is added where the
    /// row is written ([`fold`]).
    pub procedure: Option<crate::request_summary::discovery::ProcedureAt>,
}

impl SiteRow {
    fn fact(&self) -> bool {
        self.inference.is_none()
    }

    /// Whether [`fold_rows`] states this row wherever no fact holds its
    /// line: a fact or an answer in the band, on no label-only slot (a row
    /// that needs a reader stands only beside one that agrees).
    pub fn stands(&self) -> bool {
        (self.fact() || self.in_band) && !self.needs_reader
    }
}

/// A service's site rows and possible calls, before the fold.
#[derive(Debug, Clone, Default)]
pub struct SiteRows {
    /// Whether the site pass read the service: unread, nothing folds and
    /// nothing is written.
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
    /// The other calls the site pass considered and stopped at
    /// ([`crate::unread_sites::from_scan`], carrick#3688 U3). The fold keeps
    /// one only where no row stands at its line.
    pub unread: Vec<crate::unread_sites::UnreadSite>,
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
    /// How the site path came by the name of each message site it decided
    /// (carrick#3504 B10-0). Log-only: no row reads it.
    pub names: crate::name_census::SiteNames,
    /// The resolver's pieces read of each labelled call's name slot beside
    /// the site path's own (carrick#3504 B10-2a2). Log-only: no row reads it.
    pub dry_run_names: crate::name_census::DryRunNames,
    /// The request lines whose callers the summaries read, save direct calls
    /// made where a module is loaded, each of which the site pass decided to
    /// that request (carrick#4121). The fold withdraws the request line's
    /// reader row where a fact row stands at every one of those calls.
    pub held_open: Vec<HeldOpen>,
    /// The sites a scope check the trace's ask got no answer for could
    /// have decided ([`Decided::scopes_unanswered`], carrick#4155).
    pub scopes_unanswered: std::collections::BTreeSet<(String, u32, u32)>,
}

/// A request line held open by functions called directly where a module is
/// loaded (carrick#4121, [`SiteRows::held_open`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldOpen {
    /// Repo-relative, `/`-separated.
    pub file: String,
    pub line: u32,
    pub span_start: u32,
    /// The calls the summaries state no row for, by repo-relative file and
    /// line: each holds exactly one decided HTTP site, which ends at this
    /// request.
    pub fact_calls: Vec<(String, u32)>,
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
    /// The address each full path is composed of
    /// (`docs/reference/address-chain.md`), where it is recorded: the first
    /// chain that composes the path. Empty for a procedure's route and for
    /// one the cross-service join writes.
    pub addresses: std::collections::BTreeMap<String, crate::address::Address>,
    /// The handler's name where the call hands it by name.
    pub handler: Option<String>,
    /// The symbol of the binding the route is registered on.
    pub owner: Option<String>,
    /// Whether a reader row at the route's own path with no prefix takes
    /// the traced path (carrick#2474 row 4). A procedure's call writes no
    /// path of its own, so a row at its line states some other reading and
    /// stays (carrick#2474 R6c).
    pub corrects: bool,
    /// For a procedure's route: each full path with its key path and router
    /// ([`crate::request_summary::decided_sites::router_holder`]). Empty for
    /// every other route.
    pub procedures: Vec<crate::request_summary::decided_sites::ProcedureAt>,
}

/// The route sites the receiver trace serves at a full path, and the rest
/// counted by why not ([`traced`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TracedRoutes {
    pub routes: Vec<TracedRoute>,
    /// Route sites the trace does not serve at a full path, by reason:
    /// `method_unstated`, `trimmed`, `no_instance`, `mount_unverified`,
    /// `hole_<end>` (an instance end that is not the top), `unread_prefix`,
    /// `unread_own_part` (the path writes its first segment whole and then a
    /// value the code does not read, carrick#3237).
    pub untraced: std::collections::BTreeMap<String, usize>,
    /// Route sites whose own path the code writes in no whole way: a
    /// reader row there guesses at a value the code never states, so it is
    /// no route and the site stays a possible call ([`GuessedRoute`]).
    pub guessed: Vec<GuessedRoute>,
}

/// A route site whose own path the code writes in no whole way (the
/// record's name is unstated, or the call's own path holds a value no way
/// of a conditional or a default reads whole):`` `${base}health` ``, `base.replace(…)`, a local holding a value
/// read at runtime (ruling on PR #3182, 2026-11-02). The label states that
/// a route of `method` is registered here; the path it serves is a value
/// only the running program knows. Not a path that writes its first segment
/// whole before the value (`unread_own_part`, carrick#3237): its row stands.
/// A reader row of the method at the line is withdrawn ([`crate::engine`]'s `withdraw_guessed_routes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuessedRoute {
    /// Repo-relative, `/`-separated.
    pub file: String,
    pub line: u32,
    /// The method the label forces, upper case.
    pub method: String,
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
        if !label_states_a_route(record) {
            continue;
        }
        let Some(site) = site_of(decided, &record.anchors) else {
            continue;
        };
        // A route whose name the record leaves unstated is traced only where
        // the trace read its own path from the ways a conditional or a
        // default writes (carrick#2386); the record itself is unchanged.
        let ways_read = site.composed.iter().any(|composed| composed.own.is_some());
        if record.bucket == Bucket::Lead && !ways_read {
            // No whole way: a reader row of the method here is a guess,
            // unless the path writes its first segment whole before the
            // value (carrick#3237): then the row stands, counted apart.
            if site.procedure.is_none()
                && let Ok(method) = forced_method(record)
            {
                if states_first_segment(&site.name) {
                    *out.untraced
                        .entry("unread_own_part".to_string())
                        .or_default() += 1;
                    continue;
                }
                out.guessed.push(GuessedRoute {
                    file: site.file.clone(),
                    line: site.line,
                    method: method.to_uppercase(),
                });
            }
            continue;
        }
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
            Ok(routes) => out.routes.extend(routes),
            Err(Untraced::Counted(reason)) => *out.untraced.entry(reason).or_default() += 1,
            Err(Untraced::Guessed(method)) => out.guessed.push(GuessedRoute {
                file: site.file.clone(),
                line: site.line,
                method,
            }),
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
        addresses: std::collections::BTreeMap::new(),
        handler: site
            .handler
            .as_ref()
            .and_then(|handler| handler.name.clone()),
        owner: procedure.owner.clone(),
        corrects: false,
        procedures: procedure.at.clone(),
    })
}

/// Why a route site has no traced route ([`traced_route`]).
enum Untraced {
    /// Left to the reader, counted by this reason.
    Counted(String),
    /// The call's own path holds a value the code writes in no whole way:
    /// a guessed site of this method, upper case ([`GuessedRoute`]).
    Guessed(String),
}

impl From<String> for Untraced {
    fn from(reason: String) -> Self {
        Untraced::Counted(reason)
    }
}

/// Whether a route's own path writes its first segment whole before any value
/// the code does not read (`/users/${id}`, `/docs/spec${tail}`): the text of
/// its leading literal pieces holds a slash, a name, and a slash. Such a path
/// says where the route lives, so a reader's row there is no guess
/// (carrick#3237). `/v${n}/x` and a value first (`${base}health`) do not.
fn states_first_segment(name: &[crate::request_summary::Piece]) -> bool {
    let head: String = name
        .iter()
        .map_while(|piece| match piece {
            crate::request_summary::Piece::Lit(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let Some(rest) = head.strip_prefix('/') else {
        return false;
    };
    rest.split_once('/')
        .is_some_and(|(segment, _)| !segment.is_empty())
}

/// The last part of a traced route's address: the path `own` its own call
/// writes, at that call, as `pieces` where the compiler folded it from
/// constants (their text is `own`), else as its text.
fn own_part(
    site: &DecidedSite,
    own: &str,
    pieces: &[crate::address::Piece],
) -> crate::address::Part {
    use crate::address;
    let text: Option<String> = pieces.iter().map(address::Piece::text).collect();
    let pieces = match text {
        Some(text) if !pieces.is_empty() && text == own => pieces.to_vec(),
        _ => vec![address::Piece::Lit {
            text: own.to_string(),
        }],
    };
    address::Part {
        kind: address::PartKind::Own,
        dimension: address::Dimension::Path,
        at: format!("{}:{}", site.file, site.line),
        member: site.end.member.clone(),
        value: address::Value::Text {
            values: vec![pieces],
        },
        via: address::Via::Label,
    }
}

/// `site`'s traced routes, or why it has none: one, or where the call writes
/// its own path through a conditional or a default, one per way written
/// whole (carrick#2386), each at the full paths that way joins.
fn traced_route(site: &DecidedSite, record: &SiteRecord) -> Result<Vec<TracedRoute>, Untraced> {
    let method = forced_method(record)?;
    if site.composed_trimmed {
        return Err("trimmed".to_string().into());
    }
    if site.composed.is_empty() {
        return Err("no_instance".to_string().into());
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
        }
        .into());
    }
    // The call's own path holds a value the code writes in no whole way (a
    // base joined with text, rewritten by a call, held in a local, or a loop
    // element typed as `string`) and no way of a conditional or a default
    // is read: the code states no single literal path here, so a reader row
    // of the method is a guess (ruling on PR #3182, 2026-11-02). A site
    // whose own path is read but whose prefix is not stays `unread_prefix`.
    // A path that writes its first segment whole before the value is counted
    // `unread_own_part` and keeps the row (carrick#3237).
    let own_read =
        site.name_text().is_some() || site.composed.iter().any(|composed| composed.own.is_some());
    if !own_read
        && site
            .name
            .iter()
            .any(|piece| matches!(piece, crate::request_summary::Piece::Opaque { .. }))
    {
        if states_first_segment(&site.name) {
            return Err("unread_own_part".to_string().into());
        }
        return Err(Untraced::Guessed(method.to_uppercase()));
    }
    // Each own path with the full paths it joins: the call's whole text, or
    // each way it writes through a conditional or a default.
    type Served = (
        Vec<String>,
        std::collections::BTreeMap<String, crate::address::Address>,
    );
    let mut by_own: std::collections::BTreeMap<String, Served> = std::collections::BTreeMap::new();
    for composed in &site.composed {
        let (Some(own), Some(path)) = (
            composed.own.clone().or_else(|| site.name_text()),
            composed.path.clone(),
        ) else {
            return Err("unread_prefix".to_string().into());
        };
        let mut parts = composed.parts.clone();
        parts.push(own_part(site, &own, &composed.own_pieces));
        let (paths, addresses) = by_own.entry(own).or_default();
        addresses
            .entry(path.clone())
            .or_insert_with(|| crate::address::Address::of(parts));
        paths.push(path);
    }
    Ok(by_own
        .into_iter()
        .map(|(path, (mut full_paths, addresses))| {
            full_paths.sort();
            full_paths.dedup();
            TracedRoute {
                procedures: Vec::new(),
                file: site.file.clone(),
                line: site.line,
                method: method.to_uppercase(),
                path,
                full_paths,
                addresses,
                handler: site
                    .handler
                    .as_ref()
                    .and_then(|handler| handler.name.clone()),
                owner: site
                    .receiver
                    .as_ref()
                    .and_then(|receiver| receiver.symbol.clone()),
                corrects: true,
            }
        })
        .collect())
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

impl Filled {
    /// A value the record decides: confidence 1, both orders.
    fn stated(text: String) -> Self {
        Filled {
            text: Some(text),
            confidence: 1.0,
            orders_agree: true,
            asked: false,
        }
    }
}

/// The slot an unanswered site left open: the name slot (`name_slot`, the
/// role's word for it) when the question asked the name, else the first slot
/// it asked. The code stated every slot it did not ask, so naming the name
/// slot for a question that asked only the method names the wrong hole
/// (carrick#4123).
fn asked_slot(asked: &[Slot], name_slot: &str) -> String {
    if asked.contains(&Slot::Name) {
        return name_slot.to_string();
    }
    match asked.first() {
        None | Some(Slot::Name) => name_slot,
        Some(Slot::Kind) => "kind",
        Some(Slot::Role) => "role",
        Some(Slot::NameArgument) => "name_argument",
        Some(Slot::PayloadArgument) => "payload_argument",
        Some(Slot::Method) => "method",
        Some(Slot::Namespace) => "namespace",
    }
    .to_string()
}

fn filled(record: &SiteRecord, answered: Option<&Answered>, slot: Slot) -> Option<Filled> {
    if let Some(text) = record.state.forced.get(&slot) {
        return Some(Filled::stated(text.clone()));
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
    crate::message_key::socket_direction(side, row).unwrap_or(SocketDirection::Unknown)
}

/// The side of the connection a socket site's call is made on. The label's
/// own, or for an export the store marks `by_origin` the side its instance
/// came from (carrick#1670): what the instance walk ended at. `None` where
/// neither states one, as a scan without the receiver trace.
fn socket_side(label: Option<&SiteLabel>, site: &DecidedSite) -> Option<String> {
    let label = label?;
    if label.by_origin {
        origin_side(&site.instances, &label.package).map(str::to_string)
    } else {
        label.side.clone()
    }
}

/// The key a kind names for `name`, and its name scope. A message kind's
/// scope is [`crate::message_key::name_scope`]'s, the same the library-claim
/// writer asks (carrick#3498): `claimed` is the scope the label's claim
/// states, `in_process` whether the labelled end is an in-process bus.
#[allow(clippy::too_many_arguments)]
fn key_of(
    kind: &str,
    name: String,
    method: Option<&str>,
    namespace: Option<String>,
    side: Option<&str>,
    row: RowSide,
    claimed: Option<carrick_match::NameScope>,
    in_process: bool,
) -> Option<(OperationKey, Option<carrick_match::NameScope>)> {
    let scoped = |kind_scope: Option<&str>| {
        crate::message_key::name_scope(in_process, claimed.clone(), kind_scope, namespace.clone())
    };
    Some(match kind {
        "http" => (OperationKey::http(method.unwrap_or_default(), name), None),
        "topic" => (OperationKey::pubsub(name), scoped(None)),
        "message" => (
            OperationKey::pubsub(name),
            scoped(Some(carrick_match::GLOBAL_SCOPE)),
        ),
        "event" => (
            OperationKey::pubsub(name),
            scoped(Some(carrick_match::SERVICE_SCOPE)),
        ),
        "socket_event" => (
            OperationKey::socket(name, direction(side, row)),
            scoped(None),
        ),
        _ => return None,
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
fn unwritable_route(kind: &str, side: RowSide, method: Option<&str>) -> Option<Stop> {
    if kind != "http" || side != RowSide::Endpoint {
        return None;
    }
    if !method.is_some_and(crate::type_manifest::is_producer_method) {
        return Some(Stop::NoMethod);
    }
    Some(Stop::OwnerUnknown)
}

/// Why a route site, whose router the site cannot name, stays a possible
/// call: `owner_unknown`, unless every chain the trace walked up from its
/// instance ended at a stop, and then that stop's reason (the first chain's),
/// so the possible call says where the walk stopped (carrick#3688 U6).
///
/// Word only: it never adds or drops a possible call. A site keeps
/// `owner_unknown` where any chain ends at the top (the path is composed
/// elsewhere), where the trace left chains out at its cap, where a chain
/// ends at a boundary the cross-service join may still continue
/// (`entry_point`, `callback_option`: [`crate::engine`]'s
/// `route_boundaries::settle` runs after this), where a chain ends in no
/// stop (`handed`).
///
/// A procedure's route with no full path says what the procedure walk
/// found instead: the first adapter's reason, or that no adapter reached it
/// (carrick#3688, A7).
fn owner_stop(site: &DecidedSite) -> Stop {
    if let Some(procedure) = &site.procedure {
        return match (procedure.full_paths.is_empty(), procedure.stop) {
            (true, Some(stop)) => stop,
            (true, None) => Stop::ProcedureNoAdapter,
            (false, _) => Stop::OwnerUnknown,
        };
    }
    if site.composed_trimmed || site.composed.is_empty() {
        return Stop::OwnerUnknown;
    }
    let ends: Option<Vec<Stop>> = site
        .composed
        .iter()
        .map(|composed| {
            Stop::parse(&composed.end)
                .filter(|stop| !matches!(stop, Stop::EntryPoint | Stop::CallbackOption))
        })
        .collect();
    ends.and_then(|ends| ends.first().copied())
        .unwrap_or(Stop::OwnerUnknown)
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

/// [`agrees`], for a site row whose procedure has the key path
/// `procedure` (carrick#3186): its key's path is empty, so it
/// agrees with an HTTP reader row at its call whose path is an unknown call
/// path, or whose last segment is the key path (`/api/rpc/shelf.list`). Any
/// method: the reader spells the transport's, the site the RPC word.
fn agrees_at(reader: &OperationKey, site: &OperationKey, procedure: Option<&str>) -> bool {
    let Some(procedure) = procedure else {
        return agrees(reader, site);
    };
    match reader.as_http() {
        Some((_, path)) => {
            carrick_match::is_unknown_call_path(path) || path.rsplit('/').next() == Some(procedure)
        }
        None => false,
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

/// The base of an HTTP request a site states, from where the name's leading
/// value ends (carrick#3387). `None` for any other row, and where `name` is
/// not the name the code writes (a model's choice of another option), since
/// the end describes the written name only.
fn request_base(
    site: &DecidedSite,
    name: &str,
    key: &OperationKey,
    side: RowSide,
) -> Option<crate::call_base::CallBaseResolution> {
    if side != RowSide::Call || !matches!(key, OperationKey::Http { .. }) {
        return None;
    }
    if site.name_text().as_deref() != Some(name) {
        return None;
    }
    crate::call_base::stated_base(
        name,
        site.base_end().as_ref(),
        &Default::default(),
        &Default::default(),
    )
}

/// Whether the call a site decides is not its labelled end: the labelled call
/// is written inside a function the decided call enters (carrick#2795).
fn is_through(site: &DecidedSite) -> bool {
    site.file != site.end.file
        || site.span_start != site.end.span_start
        || site.span_end != site.end.span_end
}

/// How the site path came by the name of each message site in `records`
/// (carrick#3504 B10-0, [`crate::name_census`]): a broker, an in-process bus
/// or a socket. A name written whole at the labelled call is a literal; one
/// whole at a caller of the function the labelled call is in was filled by
/// the walk; any other name a record holds rests on a value the resolver read
/// or on several. A name the model is asked, and one nothing states, are
/// counted as such.
pub fn site_names(decided: &Decided, records: &[SiteRecord]) -> crate::name_census::SiteNames {
    use crate::name_census::NameSource;
    use crate::request_summary::Piece;
    let mut out = crate::name_census::SiteNames::default();
    for record in records {
        let Some(site) = site_of(decided, &record.anchors) else {
            continue;
        };
        if !matches!(
            site.end.role.as_str(),
            "broker" | "in_process_bus" | "socket"
        ) {
            continue;
        }
        let written_whole = site.key.is_some()
            || (!site.name.is_empty()
                && site.name.iter().all(|piece| matches!(piece, Piece::Lit(_))));
        out.note(if record.bucket == Bucket::Lead {
            NameSource::Unstated
        } else if record.state.asked.contains(&Slot::Name) {
            NameSource::Asked
        } else if !record.names.is_empty() {
            NameSource::ResolverFold
        } else if written_whole && site.via.is_empty() {
            NameSource::LiteralPiece
        } else if written_whole {
            NameSource::FilledParam
        } else {
            NameSource::ResolverFold
        });
    }
    out
}

/// Key every HTTP request a site row states as every consumer row is keyed:
/// on its canonical call path ([`UrlNormalizer::consumer_call_path`]), never
/// the target as written, with the base the row states. A record's rows that
/// now key alike are one row (carrick#3984): the names of one call that
/// differ only in a query or a fragment state one operation. The first in
/// record order stays, with its base. Rows that differ in their anchors (the
/// call), side, key, procedure or name scope are distinct operations and stay.
pub fn key_requests_canonically(
    rows: &mut SiteRows,
    normalizer: &crate::url_normalizer::UrlNormalizer,
) {
    for row in &mut rows.rows {
        if row.side == RowSide::Call
            && let OperationKey::Http { path, .. } = &mut row.key
        {
            *path = normalizer.consumer_call_path(path, row.base.as_ref());
        }
    }
    let mut kept: Vec<SiteRow> = Vec::with_capacity(rows.rows.len());
    for row in std::mem::take(&mut rows.rows) {
        let repeat = kept.iter().any(|earlier| {
            earlier.anchors == row.anchors
                && earlier.side == row.side
                && earlier.key == row.key
                && earlier.procedure == row.procedure
                && earlier.name_scope == row.name_scope
        });
        if !repeat {
            kept.push(row);
        }
    }
    rows.rows = kept;
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
        unread: Vec::new(),
        through: decided.through.clone(),
        traced: TracedRoutes::default(),
        boundaries: Default::default(),
        names: site_names(decided, records),
        dry_run_names: decided.dry_run_names.clone(),
        held_open: Vec::new(),
        scopes_unanswered: decided.scopes_unanswered.clone(),
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
            out.routes.push(lead(owner_stop(site)));
            continue;
        }
        let answered = answers.answered.get(&site_id(&record.anchors));
        if record.bucket == Bucket::Asked && answered.is_none() {
            let mut unanswered = lead(Stop::Unanswered);
            unanswered.unread.slot = asked_slot(&record.state.asked, &unanswered.unread.slot);
            out.leads.push(unanswered);
            continue;
        }
        let slot = |slot| filled(record, answered, slot);
        // A name the code states as several values is one row each
        // (carrick#3504 S2b-8); else the one the slot holds. Names that key
        // alike are folded once keyed ([`key_requests_canonically`]).
        let names: Vec<Filled> = if record.names.is_empty() {
            slot(Slot::Name).into_iter().collect()
        } else {
            record
                .names
                .iter()
                .map(|name| Filled::stated(name.clone()))
                .collect()
        };
        let (Some(kind), Some(role), false) =
            (slot(Slot::Kind), slot(Slot::Role), names.is_empty())
        else {
            out.leads.push(lead(Stop::BelowBand));
            continue;
        };
        let name_texts: Option<Vec<String>> = names.iter().map(|name| name.text.clone()).collect();
        let (Some(kind_text), Some(role_text), Some(name_texts)) =
            (kind.text.clone(), role.text.clone(), name_texts)
        else {
            // The model chose none of the options for a slot the key needs.
            out.leads.push(lead(Stop::BelowBand));
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
            let reason = if reason == Stop::OwnerUnknown {
                owner_stop(site)
            } else {
                reason
            };
            out.routes.push(lead(reason));
            continue;
        }
        let fact = record.bucket == Bucket::Forced;
        // Every slot the model answered that feeds the key or the side:
        // forced slots are 1.0 and agreeing, so only asked ones can lower it.
        let (confidence, orders_agree) = [Some(&kind), Some(&role)]
            .into_iter()
            .chain(names.iter().map(Some))
            .chain([method_slot.as_ref(), namespace_slot.as_ref()])
            .flatten()
            .filter(|slot| slot.asked)
            .fold((1.0_f64, true), |(low, agree), slot| {
                (low.min(slot.confidence), agree && slot.orders_agree)
            });
        for name_text in name_texts {
            let name_written = name_text.clone();
            let Some((key, name_scope)) = key_of(
                &kind_text,
                name_text,
                method.as_deref(),
                namespace.clone(),
                socket_side(label, site).as_deref(),
                side,
                label
                    .and_then(|label| label.name_scope.as_ref())
                    .map(crate::library_claims::wire_scope_of),
                site.end.role == "in_process_bus",
            ) else {
                out.leads.push(lead(Stop::BelowBand));
                continue;
            };
            // A client procedure call (carrick#3186): its name is the
            // procedure's key path, which is identity, never a path. The row
            // is an HTTP call with the RPC word as its method, an empty path
            // and the procedure; it states no base.
            let procedure = site.end.procedure.clone().filter(|_| side == RowSide::Call);
            let key = match (&procedure, key) {
                (Some(_), OperationKey::Http { method, .. }) => {
                    OperationKey::http(&method, String::new())
                }
                (_, key) => key,
            };
            let base = match procedure {
                Some(_) => None,
                None => request_base(site, &name_written, &key, side),
            };
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
                lead: lead(Stop::BelowBand),
                needs_reader: needs_reader(record),
                through: is_through(site),
                within: Vec::new(),
                base,
                procedure,
            });
        }
    }
    out
}

/// The rows the requests a service's functions build and return state
/// (carrick#3470, [`crate::request_summary::descriptors`]): one HTTP call
/// row at each object literal, source `request_descriptor`, a candidate.
///
/// No site question asked anything, so the row carries no inference, stands
/// wherever no fact holds its line and is mirrored into the graph as the
/// fold mirrors a fact; its source is what makes it a candidate. It is
/// anchored at the literal, with its body typed as a request body. The
/// literal is no call, so nothing types a response there.
pub fn descriptor_rows(decided: &Decided) -> Vec<SiteRow> {
    decided
        .descriptors
        .iter()
        .map(|descriptor| {
            let at = &descriptor.at;
            let key = OperationKey::http(&descriptor.method, descriptor.target.clone());
            SiteRow {
                file: at.file.clone(),
                line: at.line,
                key,
                side: RowSide::Call,
                source: ResolutionSource::RequestDescriptor,
                inference: None,
                in_band: true,
                name_scope: None,
                anchors: TypeAnchors {
                    call: at.clone(),
                    payload: None,
                    payload_at: descriptor.body.clone(),
                    handler: None,
                    key: None,
                },
                lead: PossibleCall {
                    file_path: at.file.clone(),
                    line_number: at.line,
                    span_start: at.span_start,
                    span_end: at.span_end,
                    kind: Some("http".to_string()),
                    role: Some("consumer".to_string()),
                    labelled_end: None,
                    unread: crate::possible_calls::Unread {
                        slot: "path".to_string(),
                        reason: Stop::NameNeverBound,
                    },
                    trace_depth: 0,
                    ends: Vec::new(),
                },
                needs_reader: false,
                through: false,
                within: Vec::new(),
                base: crate::call_base::stated_base(
                    &descriptor.target,
                    descriptor.base_end.as_ref(),
                    &Default::default(),
                    &Default::default(),
                ),
                procedure: None,
            }
        })
        .collect()
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
    /// Leads dropped from the possible calls because the blob already holds
    /// a row at their line (carrick#2539): that row states the site. Each is
    /// saved in `unread` unless a row the code decided stands there.
    pub lead_on_row_line: usize,
    /// The unread sites ([`SiteRows::unread`]) at a line no row the code
    /// decided stands at, and the leads of `lead_on_row_line` no row the
    /// code decided stands beside (carrick#4123, carrick#4151).
    pub unread: Vec<crate::unread_sites::UnreadSite>,
    /// The unread sites dropped because a row the code decided stands at
    /// their line: the index states that call.
    pub unread_on_row_line: usize,
    /// Reader rows at a call above a decided site folded onto the row the
    /// site holds (carrick#2614), as `(file, line, key)`. Each is in
    /// `withdrawn` as well.
    pub through_folded: Vec<(String, u32, OperationKey)>,
    /// Of `withdrawn`, the call-side reader rows a FACT site row replaced,
    /// at its own line or at a request line its module-level callers' facts
    /// state (carrick#4121), as `(file, line, key)`: the mount graph's copy
    /// of each goes too (carrick#2795). A row an in-band candidate replaced
    /// is not here: the graph keeps the reader's row beside the candidate.
    pub replaced_by_fact: Vec<(String, u32, OperationKey)>,
    /// The request lines ([`SiteRows::held_open`]) whose reader row went
    /// because a fact row stands at each of their calls the summaries state
    /// no row for (carrick#4121, carrick#4127), as `(file, span start)`. The
    /// engine drops the model's row there from the file results and moves
    /// its request type to the callers.
    pub stated_at_fact_callers: Vec<(String, u32)>,
    /// Fact site rows kept out of their site by a row already there, by the
    /// resolution source of the row that kept it (`socket_call`,
    /// `event_bus`, `library_claim`, ...). The measure of how much a frozen
    /// pass still hides from the fact layer.
    pub kept_out: std::collections::BTreeMap<String, usize>,
    /// Of `kept_out`, the fact socket rows a socket-pass row kept out: the
    /// frozen pass and the fact layer disagree on the operation (or the
    /// fact states no direction).
    pub socket_disagrees: usize,
}

impl SiteFold {
    /// The calls a fact site row stands at, by repo-relative file and line:
    /// the fact layer followed each to a labelled send (carrick#2448).
    pub fn fact_call_lines(&self) -> std::collections::HashSet<(String, u32)> {
        self.stated
            .iter()
            .filter(|row| row.side == RowSide::Call && row.fact())
            .map(|row| (row.file.clone(), row.line))
            .collect()
    }
}

/// `file` relative to `root`, `/`-separated, whichever form it is held in
/// ([`crate::utils::repo_relative_source_path`]).
pub fn relative(root: &Path, file: &str) -> String {
    crate::utils::repo_relative_source_path(file, &root.to_string_lossy()).replace('\\', "/")
}

/// The graph's model rows at a call whose URL the site pass stopped on
/// (`slot` is `path`, carrick#4171) state no base. A bare path the model
/// answered reads as `relative` ([`crate::call_base::stated_base`]), which
/// claims the same origin, where the code read the base as a value it could
/// not state (`fetch(new URL("a/b", apiBase()))`). A row the code decided
/// keeps the base its own pass read.
fn unstate_relative_where_path_unread(
    graph: &mut crate::mount_graph::MountGraph,
    unread: &[crate::unread_sites::UnreadSite],
    root: &Path,
) {
    use crate::call_base::CallBaseKind;
    for call in graph.data_calls.iter_mut().filter(|call| {
        call.resolution_source == Some(ResolutionSource::Model)
            && call
                .base
                .as_ref()
                .is_some_and(|base| base.kind == CallBaseKind::Relative)
    }) {
        let Some((file, line)) = call.file_location.rsplit_once(':') else {
            continue;
        };
        let (file, line) = (relative(root, file), line.parse::<u32>().ok());
        if unread
            .iter()
            .any(|site| site.slot == "path" && site.file == file && Some(site.line) == line)
        {
            call.base = None;
        }
    }
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
    let repo = data.repo_name.clone();
    let fold = fold_rows(
        &mut data.calls,
        &mut data.endpoints,
        site,
        root,
        &repo,
        make,
    );
    // A folded caller's row leaves the graph's calls too: the cross-service
    // pass reads them (`call_through::settle`), and would read the folded
    // row as a call of its own (carrick#2704).
    if let Some(graph) = data.mount_graph.as_mut() {
        unstate_relative_where_path_unread(graph, &site.unread, root);
        // A fact site row replaces the reader's row in the graph as it does
        // in the calls, and the facts join the graph beside the rows the
        // reader stated (carrick#2795): the cross-service pass and the cloud
        // read the graph's HTTP rows, and a fact held only in `calls` is
        // invisible to them while the reader's stale row there stands.
        // A standing candidate joins only where the graph holds no row at
        // its line (carrick-cloud#3018); a candidate over a reader's row
        // leaves it.
        // An old-path row a fact replaced goes from the graph as the
        // reader's does (carrick#3283).
        let is_model_row_of =
            |call: &DataFetchingCall, file: &str, line: u32, key: &OperationKey| {
                (call.resolution_source == Some(ResolutionSource::Model)
                    || call.resolution_source == Some(ResolutionSource::RequestSpec)
                    || call.resolution_source.is_some_and(is_old_path_source))
                    && call.file_location.rsplit_once(':').is_some_and(|(f, l)| {
                        relative(root, f) == file && l.parse::<u32>().ok() == Some(line)
                    })
                    && key.as_http().is_some_and(|(method, path)| {
                        call.method.eq_ignore_ascii_case(method) && call.canonical_path == path
                    })
            };
        let (removed, kept): (Vec<DataFetchingCall>, Vec<DataFetchingCall>) =
            std::mem::take(&mut graph.data_calls)
                .into_iter()
                .partition(|call| {
                    fold.through_folded
                        .iter()
                        .chain(fold.replaced_by_fact.iter())
                        .any(|(file, line, key)| is_model_row_of(call, file, *line, key))
                });
        graph.data_calls = kept;
        let normalizer = crate::url_normalizer::UrlNormalizer::default();
        // A candidate that stands where the graph holds no row at its line
        // joins it too (carrick-cloud#3018): the reader stated nothing there,
        // and a call in `calls` alone is on no list. Where a row stands the
        // reader's row stays, as for any candidate. The places are taken
        // before any push, so two candidates on one line add one row.
        let held: std::collections::HashSet<(String, u32)> = graph
            .data_calls
            .iter()
            .filter_map(|call| {
                let (file, line) = call.file_location.rsplit_once(':')?;
                Some((relative(root, file), line.parse().ok()?))
            })
            .collect();
        let mut joined: std::collections::HashSet<(String, u32)> = Default::default();
        for row in fold.stated.iter().filter(|row| {
            row.side == RowSide::Call
                && matches!(row.key, OperationKey::Http { .. })
                && (row.fact()
                    || (row.inference.is_some()
                        && !held.contains(&(row.file.clone(), row.line))
                        && joined.insert((row.file.clone(), row.line))))
        }) {
            let Some((method, path)) = row.key.as_http() else {
                continue;
            };
            let own_route = crate::call_through::serves_own_route(graph, method, path);
            // Where the call goes is read off the call as written, which the
            // reader's row at the same call held: the host a literal absolute
            // URL names (the external-call inventory classifies on it), and
            // the base a configured one resolves through (the unlisted
            // variable suggestions read it). A fact keyed by an absolute URL
            // names its own host and has no base. The client and the dispatch
            // are read off the call too, so they come from the same row.
            let read = removed.iter().find(|call| {
                call.file_location.rsplit_once(':').is_some_and(|(f, l)| {
                    relative(root, f) == row.file && l.parse::<u32>().ok() == Some(row.line)
                })
            });
            let host = normalizer.absolute_host(path);
            let base = match host {
                Some(_) => None,
                // A candidate had no reader's row to read it from: the row's
                // own reading of its base stands.
                None if !row.fact() => row.base.clone(),
                None => read.and_then(|call| call.base.clone()),
            };
            let host = host.or_else(|| read.and_then(|call| call.host.clone()));
            graph.data_calls.push(DataFetchingCall {
                method: method.to_string(),
                target_url: path.to_string(),
                canonical_path: path.to_string(),
                client: read.map(|call| call.client.clone()).unwrap_or_default(),
                file_location: format!("{}:{}", row.file, row.line),
                call_kind: None,
                repo_name: None,
                service_name: None,
                host,
                line: Some(row.line),
                base,
                consumers_not_resolved: None,
                resolution_source: Some(row.source),
                dispatch: read.and_then(|call| call.dispatch.clone()),
                role: row.through.then_some(ConsumerRole::WrapperCall),
                reaches_request: None,
                library_semantics: Vec::new(),
                own_route,
                static_asset: None,
                procedure: row.procedure.as_ref().map(|procedure| procedure.key(&repo)),
            });
        }
    }
    fold
}

/// [`fold`] over a blob's calls and endpoints.
pub fn fold_rows(
    calls: &mut Vec<ApiEndpointDetails>,
    endpoints: &mut Vec<ApiEndpointDetails>,
    site: &SiteRows,
    root: &Path,
    repo: &str,
    make: impl Fn(OperationKey, &Path, u32, RowSide) -> ApiEndpointDetails,
) -> SiteFold {
    let mut out = SiteFold::default();
    if !site.enabled {
        return out;
    }
    // A lead on a line the blob already holds a row at is that row's site.
    // A consumer lead matches a call row, a producer lead an endpoint row,
    // and a lead with no role either. A message's producer is the other way
    // round: a publish is a call row and a subscriber an endpoint row, so a
    // pub/sub producer lead matches a call row (carrick#3439 T1).
    //
    // A stop there, of any reason, still left a slot open unless a row the
    // code decided ([`code_decided`]) stands on the line: the model's row, or
    // an old-path pass's, states that slot as an answer, not as something the
    // code read. That site is saved as unread with its reason, and stays out
    // of the possible calls (carrick#4123, carrick#4151).
    let held_at = |rows: &[ApiEndpointDetails], file: &str, line: u32, code_only: bool| {
        rows.iter().any(|existing| {
            (!code_only || code_decided(existing))
                && place(root, existing).is_some_and(|(f, l)| f == file && l == line)
        })
    };
    for lead in &site.leads {
        let side = crate::message_key::lead_side(lead.kind.as_deref(), lead.role.as_deref());
        let held = |code_only| {
            let on_call = held_at(calls, &lead.file_path, lead.line_number, code_only);
            let on_endpoint = held_at(endpoints, &lead.file_path, lead.line_number, code_only);
            match side {
                Some(RowSide::Call) => on_call,
                Some(RowSide::Endpoint) => on_endpoint,
                None => on_call || on_endpoint,
            }
        };
        if held(false) {
            out.lead_on_row_line += 1;
            if !held(true) {
                out.unread
                    .push(crate::unread_sites::UnreadSite::unread_lead(lead));
            }
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
        let procedure = row.procedure.as_ref().map(|procedure| procedure.key(repo));
        // The rows one record states for a name the code gives several
        // values (carrick#3504 S2b-8) stand side by side: a row with a
        // sibling's key is that sibling's, and neither keeps the site from
        // this row nor gives way to it. Siblings key apart: rows that key
        // alike are one row already ([`key_requests_canonically`]).
        let siblings: Vec<&OperationKey> = site
            .rows
            .iter()
            .filter(|other| {
                !std::ptr::eq(*other, row) && other.side == row.side && other.anchors == row.anchors
            })
            .map(|other| &other.key)
            .collect();
        let here: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, existing)| {
                place(root, existing)
                    .is_some_and(|(file, line)| file == row.file && line == row.line)
                    && !siblings.contains(&&existing.key)
                    // Two procedure calls on one line are two operations,
                    // whose keys are alike (carrick#3186): a row
                    // naming another procedure is not this site's.
                    && !(row.procedure.is_some()
                        && existing.procedure.is_some()
                        && existing.procedure != procedure)
            })
            .map(|(index, _)| index)
            .collect();
        // A fact the scan states keeps the site. An old-path row that states
        // another operation than a fact site row does not: the fact
        // replaces it as it replaces the reader's row (carrick#3283). One
        // that states the same operation keeps the site, which then changes
        // nothing.
        // A socket-pass row gives way only to a fact stating the same
        // operation with a direction: the fact takes the site and its
        // provenance, the key unchanged (the frozen pass retires under
        // carrick#1670). On any disagreement the socket pass's row stays and
        // the fold counts it.
        // A request spec (a verb-named call handed `{ url }`) that states
        // the operation a fact states at the same call gives its site and
        // its provenance to the fact, the key unchanged (carrick#2448): the
        // fact reached a labelled send, and the spec is a reading of the
        // call's spelling alone.
        let gives_way = |existing: &ApiEndpointDetails| {
            is_reader(existing)
                || (row.fact()
                    && existing.resolution_source == Some(ResolutionSource::RequestSpec)
                    && existing.key == row.key)
                || (row.fact() && is_old_path(existing) && existing.key != row.key)
                || (row.fact()
                    && existing.resolution_source == Some(ResolutionSource::SocketCall)
                    && existing.key == row.key
                    && matches!(
                        &row.key,
                        OperationKey::Socket { direction, .. }
                            if *direction != SocketDirection::Unknown
                    ))
        };
        if let Some(&kept) = here.iter().find(|&&index| !gives_way(&rows[index])) {
            if row.fact() {
                let source = rows[kept].resolution_source;
                let name = source
                    .and_then(|source| serde_json::to_value(source).ok())
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_else(|| "unsourced".to_string());
                *out.kept_out.entry(name).or_default() += 1;
                if source == Some(ResolutionSource::SocketCall)
                    && matches!(row.key, OperationKey::Socket { .. })
                {
                    out.socket_disagrees += 1;
                }
            }
            continue;
        }
        let readers: Vec<usize> = here;
        // A row on a label-only slot stands only with a reader row that
        // agrees (carrick#2520).
        if row.needs_reader
            && !readers.iter().any(|&index| {
                let path = row
                    .procedure
                    .as_ref()
                    .map(|procedure| procedure.path.as_str());
                agrees_at(&rows[index].key, &row.key, path)
            })
        {
            let mut lead = row.lead.clone();
            lead.unread.reason = Stop::Uncorroborated;
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
            if row.fact() && row.side == RowSide::Call {
                out.replaced_by_fact
                    .push((row.file.clone(), row.line, gone.key.clone()));
            }
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
        stated.procedure = procedure;
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
    // No row there, a candidate, an old-path row, or several operations, and
    // a disagreeing caller's row stands.
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
        // A procedure row's key is identity only with its procedure.
        let procedures: Vec<Option<carrick_match::ProcedureKey>> =
            stated.iter().map(|row| row.procedure.clone()).collect();
        // A second call on the caller's line may be the reader row's own.
        // An old-path row reads the site's spelling, not its request
        // (carrick#3822): a caller's row that disagrees with it stands for
        // `call_through::settle` to restate.
        let one_fact = !stated.is_empty()
            && !throughs.iter().any(|through| through.other_call)
            && stated.iter().all(|row| is_fact(row) && !is_old_path(row))
            && keys.iter().all(|key| *key == keys[0])
            && procedures
                .iter()
                .all(|procedure| *procedure == procedures[0]);
        let folds = |row: &ApiEndpointDetails| {
            is_reader(row)
                && at(row, file, line)
                && (one_fact
                    || keys.iter().zip(&procedures).any(|(key, procedure)| {
                        let path = procedure.as_ref().map(|key| key.path.as_str());
                        agrees_at(&row.key, key, path)
                    }))
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
    // A request line held open by functions called directly where a module
    // is loaded (carrick#4121): where each of those calls holds exactly one
    // row and that row is a fact this fold stated, the calls state the
    // request, and the one reader row at the request line is the same
    // request stated again. It goes as a row a fact replaced. Any call
    // without its fact row (a lead, a row kept out, a second row there)
    // keeps it. Decided over every line first, then removed.
    let mut stated_at_module: Vec<(usize, &HeldOpen)> = Vec::new();
    for held in &site.held_open {
        let facts_stand = !held.fact_calls.is_empty()
            && held.fact_calls.iter().all(|(file, line)| {
                let here: Vec<&ApiEndpointDetails> =
                    calls.iter().filter(|row| at(row, file, *line)).collect();
                matches!(here.as_slice(), [row] if is_fact(row)
                && out.stated.iter().any(|stated| {
                    stated.side == RowSide::Call
                        && stated.fact()
                        && stated.file == *file
                        && stated.line == *line
                        && stated.key == row.key
                }))
            });
        if !facts_stand {
            continue;
        }
        let readers: Vec<usize> = calls
            .iter()
            .enumerate()
            .filter(|(_, row)| is_reader(row) && at(row, &held.file, held.line))
            .map(|(index, _)| index)
            .collect();
        if let [index] = readers.as_slice() {
            stated_at_module.push((*index, held));
        }
    }
    stated_at_module.sort_by_key(|(index, _)| std::cmp::Reverse(*index));
    stated_at_module.dedup_by_key(|(index, _)| *index);
    for (index, held) in stated_at_module {
        let gone = calls.remove(index);
        out.replaced_by_fact
            .push((held.file.clone(), held.line, gone.key.clone()));
        out.withdrawn
            .push((held.file.clone(), held.line, gone.key, RowSide::Call));
        out.stated_at_fact_callers
            .push((held.file.clone(), held.span_start));
    }
    // An unread site at a line a row the code decided stands at once the
    // fold is done, of either side, is that row's call (a class-only GraphQL
    // call is its document's row). Beside only the model's or an old-path
    // row it is kept (carrick#4151).
    let held_at = |rows: &[ApiEndpointDetails], file: &str, line: u32| {
        rows.iter().any(|existing| {
            code_decided(existing)
                && place(root, existing).is_some_and(|(f, l)| f == file && l == line)
        })
    };
    for unread in &site.unread {
        if held_at(calls, &unread.file, unread.line)
            || held_at(endpoints, &unread.file, unread.line)
        {
            out.unread_on_row_line += 1;
        } else {
            out.unread.push(unread.clone());
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

/// A row the code decided: its source is no candidate (the model's, an
/// inline literal's, a request descriptor's: [`ResolutionSource::is_candidate`])
/// and no old-path pass wrote it. It states every slot a stop on its line
/// asks about, so that stop is that row's site. A row with no source is not
/// decided. Socket-pass and event-bus rows count as decided: both state a
/// whole row from a literal name only. The one test for both drops in
/// [`fold_rows`] (carrick#4151, carrick#4153).
fn code_decided(row: &ApiEndpointDetails) -> bool {
    row.resolution_source
        .is_some_and(|source| !source.is_candidate() && !is_old_path_source(source))
}

/// A row the file reader stated, which no site answer has touched.
fn is_reader(row: &ApiEndpointDetails) -> bool {
    row.resolution_source == Some(ResolutionSource::Model) && row.inference.is_none()
}

/// Whether `row` is one an old-path pass states ([`is_old_path_source`]).
fn is_old_path(row: &ApiEndpointDetails) -> bool {
    row.resolution_source.is_some_and(is_old_path_source)
}

/// Whether `source` is a pass of the old path, frozen until it is deleted
/// (carrick#1561): an imported request member (`imported_request_member`)
/// or a same-file wrapper (`local_http_wrapper`). The fact layer's site row at
/// the same call is the reading that replaces each: an imported member's row, for one, leaves a
/// base its module composes from a constant unread.
fn is_old_path_source(source: ResolutionSource) -> bool {
    matches!(
        source,
        ResolutionSource::ImportedMember | ResolutionSource::SameFileWrapper
    )
}

pub mod dispatch;

#[cfg(test)]
pub(crate) mod tests;
