//! Every call site the fact layer considered and did not state as a row,
//! with the one reason it stopped (carrick#3688 U3). It goes on the index
//! blob as `unread_sites`, beside its exact total and its exact count per
//! reason, for the agent to read. It is never a row.
//!
//! - **One producer.** [`from_scan`] reads the stops the site pass holds
//!   (discovery's unfollowed calls, its walks that stopped with no labelled
//!   end, its walks that missed a call inside, the decided pass's unplaced
//!   calls and caller cycles, and the class-only GraphQL calls), and the
//!   library reader's name leads ([`crate::library_claims::LibraryLead`],
//!   carrick#3504 B10-3) at a call none of those names. [`attach`]
//!   joins them to the possible calls the fold lists, and writes both
//!   fields from that one list: `possible_calls` is the projection of the
//!   listed rows ([`crate::possible_calls`]), byte for byte what it was
//!   before this field, until its readers move to this one (U5).
//! - **Never an input.** Nothing here feeds findings, the type check, the
//!   matcher or the PR result.
//! - **Cap.** At most [`MAX_PER_SERVICE`] rows per service. The rows that
//!   reached a labelled library call, or name an operation through their
//!   slot, stay first, then the plain calls wrapped around one of those;
//!   past the cap the rest are kept as whole files, sampled in
//!   sha256-of-path order, so no folder sorts out of the list. The listed rows are written by file, line and
//!   span. `unread_sites_total` and `unread_sites_by_reason` count every
//!   row, past the cap too, and `unread_sites_not_listed` names each file
//!   the cut left rows of, with its count (total = listed + not listed).
//! - **Words.** `reason` and `also` are [`Stop`] words; the phrase a reader
//!   shows for each is `npm/carrick/src/unread-reasons.json`. Files are
//!   repo-relative.
//!
//! The field's contract (keys, cap, size bound) is carrick#3688's "The
//! index field" section.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cloud_storage::CloudRepoData;
use crate::library_claims::LibraryLead;
use crate::possible_calls::{
    LabelledEndRef, PossibleCall, PossibleEnd, Unread, kind_of, labelled_end, role_of, slot_of,
};
use crate::request_summary::decided_sites::{Decided, DecidedStop};
use crate::request_summary::discovery::{Discovery, DiscoverySite};
use crate::request_summary::{CallerStop, Stop};

/// The most unread sites one service's blob carries.
pub const MAX_PER_SERVICE: usize = 2000;

/// The slot of a call whose callee the walk did not read.
const CALLEE: &str = "callee";

/// What a reader says for a site: the phrase of its one reason
/// (`npm/carrick/src/unread-reasons.json`, via [`Stop::phrase`]).
pub fn phrase_of(site: &UnreadSite) -> &'static str {
    site.reason.phrase()
}

/// One unread site, as the blob carries it (`unread_sites[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnreadSite {
    /// Repo-relative, `/`-separated.
    pub file: String,
    pub line: u32,
    /// The call's SWC span in the file parsed on its own.
    pub span_start: u32,
    pub span_end: u32,
    pub reason: Stop,
    /// The other ways the site's values stopped, distinct, at most four.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<Stop>,
    /// What the code does not state: `callee` (the walk did not read what
    /// is called), `path` (an HTTP path) or `name` (an event, topic or
    /// message name).
    pub slot: String,
    /// The callee as written, cut to
    /// [`crate::unfollowed_calls::RECEIVER_CHARS`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callee: Option<String>,
    /// `<file>:<line>` of the last function the walk entered, when it
    /// stopped below the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_at: Option<String>,
    /// Functions the walk entered from the call.
    pub depth: u32,
    /// The labelled library call the walk reached, when it reached one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labelled_end: Option<LabelledEndRef>,
    /// The protocol (`http`, `graphql`, `pubsub`, `socket`), when the label
    /// says one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// `producer` or `consumer`, when the label's op says one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Every labelled end a possible call reaches, first included
    /// (carrick#3818); empty unless it reaches two or more.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ends: Vec<PossibleEnd>,
    /// A possible call: the row `possible_calls` is written from. Not on
    /// the wire.
    #[serde(skip)]
    listed: bool,
}

impl UnreadSite {
    fn at(&self) -> (&str, u32, u32) {
        (&self.file, self.span_start, self.span_end)
    }

    /// The possible call `call` is, as an unread site.
    fn listed(call: &PossibleCall) -> Self {
        UnreadSite {
            file: call.file_path.clone(),
            line: call.line_number,
            span_start: call.span_start,
            span_end: call.span_end,
            reason: call.unread.reason,
            also: Vec::new(),
            slot: call.unread.slot.clone(),
            callee: None,
            stop_at: None,
            depth: call.trace_depth,
            labelled_end: call.labelled_end.clone(),
            kind: call.kind.clone(),
            role: call.role.clone(),
            ends: call.ends.clone(),
            listed: true,
        }
    }

    /// The possible call a listed row is, field for field.
    fn possible_call(&self) -> PossibleCall {
        PossibleCall {
            file_path: self.file.clone(),
            line_number: self.line,
            span_start: self.span_start,
            span_end: self.span_end,
            kind: self.kind.clone(),
            role: self.role.clone(),
            labelled_end: self.labelled_end.clone(),
            unread: Unread {
                slot: self.slot.clone(),
                reason: self.reason,
            },
            trace_depth: self.depth,
            ends: self.ends.clone(),
        }
    }

    /// `reason` beside the row's own, once, when the row has room: the one
    /// rule for a site already saved at the span another saves at (#3952's
    /// merge; carrick#3947's library leads).
    fn also(&mut self, reason: Stop) {
        if self.reason != reason && !self.also.contains(&reason) && self.also.len() < 4 {
            self.also.push(reason);
        }
    }

    /// A discovery record of a call whose walk stopped with `reason`.
    fn of_discovery(site: &DiscoverySite, reason: Stop) -> Self {
        UnreadSite {
            file: site.file.clone(),
            line: site.line,
            span_start: site.span_start,
            span_end: site.span_end,
            reason,
            also: Vec::new(),
            slot: CALLEE.to_string(),
            callee: callee(&site.callee),
            stop_at: site
                .chain
                .last()
                .map(|hop| format!("{}:{}", hop.file, hop.line)),
            depth: site.chain.len() as u32,
            labelled_end: None,
            kind: None,
            role: None,
            ends: Vec::new(),
            listed: false,
        }
    }

    /// A call the decided pass stopped at with a labelled end in hand.
    fn of_decided(stop: &DecidedStop) -> Self {
        UnreadSite {
            file: stop.file.clone(),
            line: stop.line,
            span_start: stop.span_start,
            span_end: stop.span_end,
            reason: stop.reason,
            also: Vec::new(),
            slot: slot_of(&stop.end.role).to_string(),
            callee: stop.callee.as_deref().and_then(callee),
            stop_at: None,
            depth: stop.depth,
            labelled_end: Some(labelled_end(&stop.end)),
            kind: kind_of(&stop.end.role).map(str::to_string),
            role: role_of(&stop.end.role, stop.end.op.as_deref()).map(str::to_string),
            ends: Vec::new(),
            listed: false,
        }
    }
}

impl UnreadSite {
    /// A call of a function of the service whose request the send side could
    /// not write as a row (carrick#3688 F). The request is an HTTP one, sent
    /// by the call; the labelled end is the one discovery found at the
    /// request, when it found one.
    fn of_caller(caller: &CallerStop, root: &Path, discovery: &Discovery) -> Self {
        let request = (
            crate::site_rows::relative(root, &caller.request_at.0.to_string_lossy()),
            caller.request_at.1,
        );
        let end = discovery
            .sites
            .iter()
            .filter_map(|site| site.end.as_ref())
            .find(|end| {
                end.role == crate::services::type_sidecar::LibraryRole::HttpClient
                    && (end.file.as_str(), end.span_start) == (request.0.as_str(), request.1)
            });
        UnreadSite {
            file: crate::site_rows::relative(root, &caller.file.to_string_lossy()),
            line: caller.line,
            span_start: caller.span_start,
            span_end: caller.span_end,
            reason: caller.reason,
            also: caller.also.clone(),
            slot: "path".to_string(),
            callee: callee(&caller.callee),
            stop_at: None,
            depth: 0,
            labelled_end: end.map(|end| LabelledEndRef {
                package: end.package.clone(),
                export: end.export.clone(),
                member: end.member.clone(),
            }),
            kind: Some("http".to_string()),
            role: Some("consumer".to_string()),
            ends: Vec::new(),
            listed: false,
        }
    }
}

impl UnreadSite {
    /// A verified library site whose name the library reader states none
    /// for (carrick#3504 B10-3): the slot is its name.
    fn of_library(lead: &LibraryLead, root: &Path) -> Self {
        UnreadSite {
            file: crate::site_rows::relative(root, &lead.file.to_string_lossy()),
            line: lead.line,
            span_start: lead.span_start,
            span_end: lead.span_end,
            reason: lead.reason,
            also: Vec::new(),
            slot: "name".to_string(),
            callee: None,
            stop_at: None,
            depth: 0,
            labelled_end: Some(LabelledEndRef {
                package: lead.package.clone(),
                export: lead.export.clone(),
                member: lead.member.clone(),
            }),
            kind: lead.kind.map(str::to_string),
            role: lead.role.map(str::to_string),
            ends: Vec::new(),
            listed: false,
        }
    }
}

/// The callee text as the blob carries it; none when the call writes none.
fn callee(text: &str) -> Option<String> {
    let cut = crate::unfollowed_calls::cut_receiver(text);
    (!cut.is_empty()).then_some(cut)
}

/// The unread sites a service's site pass holds besides its possible calls:
/// every call discovery could not follow, every walk that stopped with no
/// labelled end or missed a call inside, the labelled calls the decided pass
/// could not place or stopped at in a caller cycle, and the class-only
/// GraphQL calls ([`crate::possible_calls::class_only_graphql`]), which
/// [`crate::site_rows::fold_rows`] keeps only where no row stands at their
/// line. Unordered and uncapped; [`attach`] orders and bounds them.
pub fn from_scan(
    discovery: &Discovery,
    decided: &Decided,
    callers: &[CallerStop],
    library: &[LibraryLead],
    root: &Path,
) -> Vec<UnreadSite> {
    let mut out: Vec<UnreadSite> = Vec::new();
    for site in &discovery.unfollowed {
        if let Some(reason) = site.stop {
            out.push(UnreadSite::of_discovery(site, reason));
        }
    }
    for site in discovery.sites.iter().filter(|site| site.end.is_none()) {
        if let Some(reason) = site.stop {
            out.push(UnreadSite::of_discovery(site, reason));
        }
    }
    for at in &discovery.missed_inside {
        let Some((line, text)) = discovery.missed_at.get(at) else {
            continue;
        };
        // A call whose walk reached only route registrations in a sibling's
        // code registers a route this service serves: an HTTP one, whose
        // labelled end is the sibling's (carrick#2474 R5b).
        let route = discovery.sibling_routes_at.get(at);
        out.push(UnreadSite {
            file: at.0.clone(),
            line: *line,
            span_start: at.1,
            span_end: at.2,
            reason: Stop::MissedInside,
            also: Vec::new(),
            slot: CALLEE.to_string(),
            callee: callee(text),
            stop_at: None,
            depth: 0,
            labelled_end: route.map(|end| LabelledEndRef {
                package: end.package.clone(),
                export: end.export.clone(),
                member: end.member.clone(),
            }),
            kind: route
                .and_then(|_| kind_of("server_framework"))
                .map(str::to_string),
            role: route
                .and_then(|_| role_of("server_framework", Some("receive")))
                .map(str::to_string),
            ends: Vec::new(),
            listed: false,
        });
    }
    // A walk that passed a call into a package the scan ended without an
    // answer for says so beside its own reason (carrick#3684).
    for row in &mut out {
        let at = (row.file.clone(), row.span_start, row.span_end);
        let unanswered = discovery
            .unanswered_at
            .get(&at)
            .is_some_and(|keys| !keys.is_empty());
        if unanswered {
            row.also(Stop::UnansweredPackage);
        }
    }
    out.extend(decided.stops.iter().map(UnreadSite::of_decided));
    out.extend(
        callers
            .iter()
            .map(|caller| UnreadSite::of_caller(caller, root, discovery)),
    );
    out.extend(
        decided
            .leads
            .iter()
            .filter(|lead| crate::possible_calls::class_only_graphql(lead))
            .map(|lead| UnreadSite {
                listed: false,
                ..UnreadSite::listed(&crate::possible_calls::from_lead(lead))
            }),
    );
    // The library reader's leads (carrick#3504 B10-3), at a call no other
    // row here names and the site pass decides no name at: the site pass's
    // own reading of a call stands.
    // A lead at a call another unread site already holds (a second chain's
    // lead, or a site another pass saved) joins that site's `also`.
    for lead in library {
        let site = UnreadSite::of_library(lead, root);
        let decided_here = decided
            .sites
            .iter()
            .any(|held| (held.file.as_str(), held.span_start, held.span_end) == site.at());
        if decided_here {
            continue;
        }
        match out.iter_mut().find(|held| held.at() == site.at()) {
            None => out.push(site),
            Some(held) => held.also(site.reason),
        }
    }
    // A call whose member declaration the receiver trace's ask left
    // unanswered (carrick#3991): the answer would have read its callee. A
    // row there that reached a labelled end is listed for another slot,
    // which the answer does not decide.
    for (file, lo, hi) in &discovery.declarations_unanswered {
        mark_unanswered(&mut out, (file.as_str(), *lo, *hi), |row| {
            row.labelled_end.is_none()
        });
    }
    out
}

/// The library sites a scan could not verify (no verdict came back) added to
/// `stops`, one `unanswered` site each at the site's name slot. A site
/// already saved at the same span keeps its row and gets `unanswered` in
/// `also`; a site that is a possible call (`listed`) is already saved.
pub fn with_unanswered(
    mut stops: Vec<UnreadSite>,
    listed: &[PossibleCall],
    unanswered: &[crate::request_summary::LibrarySite],
    root: &Path,
) -> Vec<UnreadSite> {
    for site in unanswered {
        let file = crate::site_rows::relative(root, &site.file.to_string_lossy());
        let at = (file.as_str(), site.span_start, site.span_end);
        if listed
            .iter()
            .any(|call| (call.file_path.as_str(), call.span_start, call.span_end) == at)
        {
            continue;
        }
        if let Some(held) = stops.iter_mut().find(|row| row.at() == at) {
            held.also(Stop::Unanswered);
            continue;
        }
        let path: Vec<&str> = std::iter::once(site.export.as_str())
            .chain(site.path.iter().map(String::as_str))
            .chain(site.member.as_deref())
            .collect();
        stops.push(UnreadSite {
            file: file.clone(),
            line: site.line,
            span_start: site.span_start,
            span_end: site.span_end,
            reason: Stop::Unanswered,
            also: Vec::new(),
            slot: "name".to_string(),
            callee: callee(&path.join(".")),
            stop_at: None,
            depth: 0,
            labelled_end: None,
            kind: None,
            role: None,
            ends: Vec::new(),
            listed: false,
        });
    }
    stops
}

/// An element of a verb decorator's array of paths that is no literal
/// (`@Post(['/a', NAME])`): it states no route, so it is saved at its own
/// line, slot `path`, reason [`Stop::UnreadExpression`] (the same word a
/// value written in a form Carrick does not read has everywhere else).
pub(crate) fn of_decorator_path(
    file: &str,
    at: &crate::swc_scanner::UnreadDecoratorPath,
) -> UnreadSite {
    UnreadSite {
        file: file.to_string(),
        line: u32::try_from(at.line_number).unwrap_or(u32::MAX),
        span_start: at.span_start,
        span_end: at.span_end,
        reason: Stop::UnreadExpression,
        also: Vec::new(),
        slot: "path".to_string(),
        callee: callee(&at.decorator),
        stop_at: None,
        depth: 0,
        labelled_end: None,
        kind: None,
        role: None,
        ends: Vec::new(),
        listed: false,
    }
}

/// The calls through an interface member whose implementations the scan
/// asked for and got no answer to (a failed or short ask,
/// [`crate::request_summary::implementations::Implementations::unanswered`]).
/// A site saved at one of those spans with `interface_member` as its reason
/// gains `unanswered` in `also`; its reason stays. A site there with any
/// other reason is left alone, and no site is added.
pub fn with_unanswered_implementations(
    mut stops: Vec<UnreadSite>,
    asked: &[(std::path::PathBuf, u32, u32)],
    root: &Path,
) -> Vec<UnreadSite> {
    for (file, lo, hi) in asked {
        let file = crate::site_rows::relative(root, &file.to_string_lossy());
        mark_unanswered(&mut stops, (file.as_str(), *lo, *hi), |row| {
            row.reason == Stop::InterfaceMember
        });
    }
    stops
}

/// `unanswered` beside the reason of every saved site at `at` that an ask
/// with no answer would have decided (`applies`): the merge each failed
/// ask's sites share (carrick#3991).
fn mark_unanswered(
    stops: &mut [UnreadSite],
    at: (&str, u32, u32),
    applies: impl Fn(&UnreadSite) -> bool,
) {
    for row in stops
        .iter_mut()
        .filter(|row| row.at() == at && applies(row))
    {
        row.also(Stop::Unanswered);
    }
}

/// A saved site that reached a decorator export: where it is, and what the
/// label says of it.
struct DecoratorEnd {
    file: String,
    start: u32,
    end: LabelledEndRef,
    kind: Option<String>,
    role: Option<String>,
}

/// The unread sites of methods decorated through a decorator factory of the
/// service (carrick#3688 C1). A factory whose body holds a site the scan
/// already saved (`saved` and `leads`) with a labelled end that is a
/// decorator export gives each method that calls it one site at the
/// method's name: slot `name`, reason [`Stop::OwnFunction`], the factory as
/// the callee. The saved site is read, never the body again; a factory
/// holding none gives its methods nothing.
pub(crate) fn of_own_decorators(
    decorators: &[crate::request_summary::own_decorators::OwnDecoratorAt],
    claims: &[crate::library_claims::ExportClaims],
    saved: &[UnreadSite],
    leads: &[PossibleCall],
    root: &Path,
) -> Vec<UnreadSite> {
    let is_decorator = |end: &LabelledEndRef| {
        claims.iter().any(|export| {
            export.package == end.package
                && export.export == end.export
                && export
                    .claims
                    .iter()
                    .any(|claim| claim.is_decorate_form_op())
        })
    };
    // Each saved site that reached a decorator export: where it is, and
    // what the label says of it.
    let mut ends: Vec<DecoratorEnd> = Vec::new();
    for site in saved {
        if let Some(end) = site.labelled_end.as_ref().filter(|end| is_decorator(end)) {
            ends.push(DecoratorEnd {
                file: site.file.clone(),
                start: site.span_start,
                end: end.clone(),
                kind: site.kind.clone(),
                role: site.role.clone(),
            });
        }
    }
    for lead in leads {
        if let Some(end) = lead.labelled_end.as_ref().filter(|end| is_decorator(end)) {
            ends.push(DecoratorEnd {
                file: lead.file_path.clone(),
                start: lead.span_start,
                end: end.clone(),
                kind: lead.kind.clone(),
                role: lead.role.clone(),
            });
        }
    }
    let relative = |file: &Path| crate::site_rows::relative(root, &file.to_string_lossy());
    let mut out: Vec<UnreadSite> = Vec::new();
    for at in decorators {
        let factory_file = relative(&at.factory_file);
        let Some(found) = ends.iter().find(|found| {
            found.file == factory_file && at.factory.0 <= found.start && found.start <= at.factory.1
        }) else {
            continue;
        };
        out.push(UnreadSite {
            file: relative(&at.file),
            line: at.decorator.line,
            span_start: at.decorator.call.span_start,
            span_end: at.decorator.call.span_end,
            reason: Stop::OwnFunction,
            also: Vec::new(),
            slot: "name".to_string(),
            callee: callee(&at.decorator.callee),
            stop_at: None,
            depth: 1,
            labelled_end: Some(found.end.clone()),
            kind: found.kind.clone(),
            role: found.role.clone(),
            ends: Vec::new(),
            listed: false,
        });
    }
    out
}

impl UnreadSite {
    /// The row names an operation: a labelled end, or a slot other than
    /// `callee` (the rows that carry a `path` or `name`). Used only by [`cap`].
    fn names_an_operation(&self) -> bool {
        self.slot != CALLEE || self.labelled_end.is_some()
    }
}

/// What [`cap`] keeps, and what it counts.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Capped {
    pub rows: Vec<UnreadSite>,
    /// Every row once, past the cap too.
    pub total: usize,
    pub by_reason: BTreeMap<Stop, u32>,
    /// Per file, the rows the cap left out. Empty when it cut nothing.
    pub not_listed: BTreeMap<String, u32>,
}

/// Drop a repeat of one call (a possible call first, then a row that
/// reached a labelled call), count what is left, and keep
/// [`MAX_PER_SERVICE`]. Past the cap, files are walked in sha256-of-path
/// order three times: first each file's operation rows (a labelled end, or
/// a slot other than `callee`), then each file's plain calls whose span
/// contains an operation row's, then the rest, each kept only whole and
/// only if it fits what is left (a file that does not fit is skipped and
/// the walk goes on). Every row
/// not kept adds one to its file in `not_listed`. The kept rows are sorted
/// labelled first, then by file, line and span.
pub fn cap(mut rows: Vec<UnreadSite>) -> Capped {
    rows.sort_by(|a, b| {
        (a.at(), !a.listed, a.labelled_end.is_none()).cmp(&(
            b.at(),
            !b.listed,
            b.labelled_end.is_none(),
        ))
    });
    let mut seen: BTreeSet<(String, u32, u32)> = BTreeSet::new();
    rows.retain(|row| seen.insert((row.file.clone(), row.span_start, row.span_end)));
    let mut by_reason: BTreeMap<Stop, u32> = BTreeMap::new();
    for row in &rows {
        *by_reason.entry(row.reason).or_default() += 1;
    }
    let total = rows.len();
    let mut not_listed: BTreeMap<String, u32> = BTreeMap::new();
    if total > MAX_PER_SERVICE {
        let mut files: BTreeMap<&str, [Vec<usize>; 3]> = BTreeMap::new();
        let mut operation_spans: BTreeMap<&str, Vec<(u32, u32)>> = BTreeMap::new();
        for row in rows.iter().filter(|row| row.names_an_operation()) {
            operation_spans
                .entry(row.file.as_str())
                .or_default()
                .push((row.span_start, row.span_end));
        }
        for (i, row) in rows.iter().enumerate() {
            let around = row.kind.is_none()
                && !row.names_an_operation()
                && operation_spans.get(row.file.as_str()).is_some_and(|spans| {
                    spans
                        .iter()
                        .any(|&(start, end)| row.span_start <= start && end <= row.span_end)
                });
            let tier = if row.names_an_operation() {
                0
            } else if around {
                1
            } else {
                2
            };
            files.entry(row.file.as_str()).or_default()[tier].push(i);
        }
        let mut order: Vec<(Vec<u8>, &str)> = files
            .keys()
            .map(|file| (Sha256::digest(file.as_bytes()).to_vec(), *file))
            .collect();
        order.sort();
        let mut keep = vec![false; total];
        let mut left = MAX_PER_SERVICE;
        for tier in 0..3 {
            for (_, file) in &order {
                let Some(group) = files[file].get(tier) else {
                    continue;
                };
                if group.len() <= left {
                    left -= group.len();
                    for &i in group {
                        keep[i] = true;
                    }
                }
            }
        }
        let mut kept = keep.iter();
        rows.retain(|row| {
            let keeps = *kept.next().unwrap_or(&false);
            if !keeps {
                *not_listed.entry(row.file.clone()).or_default() += 1;
            }
            keeps
        });
    }
    rows.sort_by(|a, b| {
        (
            a.labelled_end.is_none(),
            &a.file,
            a.line,
            a.span_start,
            a.span_end,
        )
            .cmp(&(
                b.labelled_end.is_none(),
                &b.file,
                b.line,
                b.span_start,
                b.span_end,
            ))
    });
    Capped {
        rows,
        total,
        by_reason,
        not_listed,
    }
}

/// Write a service's unread sites on its blob: the possible calls the fold
/// lists (`listed`) and the other stops it kept (`stops`), as one list.
/// `possible_calls` is written from that list's listed rows.
pub fn attach(data: &mut CloudRepoData, stops: Vec<UnreadSite>, listed: &[PossibleCall]) {
    let mut rows: Vec<UnreadSite> = listed.iter().map(UnreadSite::listed).collect();
    rows.extend(stops.into_iter().map(|row| UnreadSite {
        listed: false,
        ..row
    }));
    crate::possible_calls::attach(
        data,
        rows.iter()
            .filter(|row| row.listed)
            .map(UnreadSite::possible_call)
            .collect(),
    );
    let Capped {
        mut rows,
        total,
        by_reason,
        not_listed,
    } = cap(rows);
    // The ends the possible call at the same span reaches (carrick#3818).
    let ends: BTreeMap<(&str, u32, u32), &Vec<PossibleEnd>> = data
        .possible_calls
        .iter()
        .flatten()
        .filter(|call| !call.ends.is_empty())
        .map(|call| {
            (
                (call.file_path.as_str(), call.span_start, call.span_end),
                &call.ends,
            )
        })
        .collect();
    for row in rows.iter_mut().filter(|row| row.listed) {
        if let Some(found) = ends.get(&(row.file.as_str(), row.span_start, row.span_end)) {
            row.ends = (*found).clone();
        }
    }
    data.unread_sites = Some(rows);
    data.unread_sites_total = (total > 0).then_some(total as u32);
    data.unread_sites_by_reason = (!by_reason.is_empty()).then_some(by_reason);
    data.unread_sites_not_listed = (!not_listed.is_empty()).then_some(not_listed);
}

/// One row of the `CARRICK_OUTPUT_JSON` output's `unread_sites`: the blob's row,
/// and over several services the service it came from, since `file` is
/// service-relative (carrick#3958).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnreadSiteRow {
    #[serde(flatten)]
    pub site: UnreadSite,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
}

/// The unread-site keys [`attach`] writes on a blob, read back off the blobs
/// of one scan for the `CARRICK_OUTPUT_JSON` output (carrick#3958). It reads
/// what `attach` wrote and produces no list of its own. Over one service the
/// keys equal the blob's. Over several, the rows join in service order each
/// carrying its `service`, the counts add, and `unread_sites_not_listed` keys
/// are `<service>/<file>`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct UnreadSiteKeys {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread_sites: Option<Vec<UnreadSiteRow>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread_sites_total: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread_sites_by_reason: Option<BTreeMap<Stop, u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread_sites_not_listed: Option<BTreeMap<String, u32>>,
}

impl UnreadSiteKeys {
    pub fn of_blobs<'a>(blobs: impl IntoIterator<Item = &'a CloudRepoData>) -> Self {
        let blobs: Vec<&CloudRepoData> = blobs.into_iter().collect();
        let several = blobs.len() > 1;
        let mut keys = Self::default();
        for blob in blobs {
            let service = blob
                .service_name
                .clone()
                .unwrap_or_else(|| blob.repo_name.clone());
            if let Some(rows) = &blob.unread_sites {
                keys.unread_sites
                    .get_or_insert_with(Vec::new)
                    .extend(rows.iter().map(|site| UnreadSiteRow {
                        site: site.clone(),
                        service: several.then(|| service.clone()),
                    }));
            }
            if let Some(total) = blob.unread_sites_total {
                *keys.unread_sites_total.get_or_insert(0) += total;
            }
            if let Some(by_reason) = &blob.unread_sites_by_reason {
                let into = keys
                    .unread_sites_by_reason
                    .get_or_insert_with(BTreeMap::new);
                for (reason, count) in by_reason {
                    *into.entry(*reason).or_insert(0) += count;
                }
            }
            if let Some(not_listed) = &blob.unread_sites_not_listed {
                let into = keys
                    .unread_sites_not_listed
                    .get_or_insert_with(BTreeMap::new);
                for (file, count) in not_listed {
                    let key = if several {
                        format!("{service}/{file}")
                    } else {
                        file.clone()
                    };
                    *into.entry(key).or_insert(0) += count;
                }
            }
        }
        keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request_summary::decided_sites::LabelledEnd;

    fn blob() -> CloudRepoData {
        serde_json::from_value(serde_json::json!({
            "repo_name": "r",
            "endpoints": [], "calls": [], "mounts": [], "apps": {},
            "imported_handlers": [], "function_definitions": {},
            "config_json": null, "package_json": null, "packages": null,
            "last_updated": "2026-11-09T00:00:00Z", "commit_hash": "abc"
        }))
        .unwrap()
    }

    /// carrick#3958: over several services each JSON row names its service and
    /// the not-listed counts stay per service; over one the keys are the blob's.
    #[test]
    fn the_json_keys_name_the_service_over_several_and_carry_not_listed() {
        let row = serde_json::json!({
            "file": "src/a.ts", "line": 1, "span_start": 1, "span_end": 2,
            "reason": "value_callee", "slot": "callee", "depth": 0,
        });
        let mut first = blob();
        first.service_name = Some("one".into());
        first.unread_sites = Some(vec![serde_json::from_value(row.clone()).unwrap()]);
        first.unread_sites_total = Some(3);
        first.unread_sites_not_listed = Some(BTreeMap::from([("src/a.ts".to_string(), 2)]));
        let mut second = blob();
        second.service_name = Some("two".into());
        second.unread_sites = Some(vec![serde_json::from_value(row).unwrap()]);
        second.unread_sites_total = Some(1);
        second.unread_sites_not_listed = Some(BTreeMap::from([("src/a.ts".to_string(), 5)]));

        let one = serde_json::to_value(UnreadSiteKeys::of_blobs([&first])).unwrap();
        assert!(one["unread_sites"][0].get("service").is_none());
        assert_eq!(
            one["unread_sites_not_listed"],
            serde_json::json!({"src/a.ts": 2})
        );

        let both = serde_json::to_value(UnreadSiteKeys::of_blobs([&first, &second])).unwrap();
        assert_eq!(both["unread_sites"][0]["service"], "one");
        assert_eq!(both["unread_sites"][1]["service"], "two");
        assert_eq!(both["unread_sites_total"], 4);
        assert_eq!(
            both["unread_sites_not_listed"],
            serde_json::json!({"one/src/a.ts": 2, "two/src/a.ts": 5})
        );
    }

    /// carrick#3989 S0: a word a later release added reads, and a reader
    /// says the raw word with no phrase; the boundary line says the same.
    #[test]
    fn a_reason_word_from_a_later_release_is_said_as_the_raw_word() {
        let site: UnreadSite = serde_json::from_value(serde_json::json!({
            "file": "src/later.ts", "line": 7, "span_start": 30, "span_end": 40,
            "reason": "a_word_from_a_later_release", "slot": "path", "depth": 0,
        }))
        .expect("an unknown reason word reads");
        assert_eq!(phrase_of(&site), "a_word_from_a_later_release");
        let known: UnreadSite = serde_json::from_value(serde_json::json!({
            "file": "src/known.ts", "line": 3, "span_start": 1, "span_end": 2,
            "reason": "value_callee", "slot": "callee", "depth": 0,
        }))
        .unwrap();
        assert_eq!(phrase_of(&known), Stop::ValueCallee.phrase());
        assert_ne!(phrase_of(&known), "value_callee");
    }

    fn stop(file: &str, line: u32, reason: Stop) -> UnreadSite {
        UnreadSite {
            file: file.to_string(),
            line,
            span_start: line * 10,
            span_end: line * 10 + 5,
            reason,
            also: Vec::new(),
            slot: CALLEE.to_string(),
            callee: Some("this.handlers[type]".to_string()),
            stop_at: None,
            depth: 0,
            labelled_end: None,
            kind: None,
            role: None,
            ends: Vec::new(),
            listed: false,
        }
    }

    fn possible(file: &str, line: u32, end: bool) -> PossibleCall {
        PossibleCall {
            file_path: file.to_string(),
            line_number: line,
            span_start: line * 10,
            span_end: line * 10 + 5,
            kind: end.then(|| "http".to_string()),
            role: end.then(|| "consumer".to_string()),
            labelled_end: end.then(|| LabelledEndRef {
                package: "a-http-lib".to_string(),
                export: "Client".to_string(),
                member: Some("get".to_string()),
            }),
            unread: Unread {
                slot: "path".to_string(),
                reason: Stop::Unanswered,
            },
            trace_depth: 1,
            ends: Vec::new(),
        }
    }

    /// Past the cap the rows kept are the labelled ones first, and the total
    /// and the count per reason still count every row.
    #[test]
    fn past_the_cap_the_totals_stay_exact() {
        let mut rows: Vec<UnreadSite> = (1..=MAX_PER_SERVICE as u32 + 9)
            .map(|n| stop("a.ts", n, Stop::ValueCallee))
            .collect();
        rows.push(stop("z.ts", 1, Stop::Recursion));
        let labelled = UnreadSite {
            labelled_end: Some(LabelledEndRef {
                package: "a-http-lib".to_string(),
                export: "Client".to_string(),
                member: None,
            }),
            ..stop("zz.ts", 7, Stop::Unplaced)
        };
        rows.push(labelled.clone());
        let capped = cap(rows);
        // `a.ts` (2,009 rows) is cut whole: the rows listed are `zz.ts`
        // and `z.ts`, the cost of keeping whole files (carrick#3688 U7).
        assert_eq!(capped.rows.len(), 2);
        assert_eq!(
            capped.not_listed,
            BTreeMap::from([("a.ts".to_string(), 2009)])
        );
        assert_eq!(capped.total, MAX_PER_SERVICE + 11);
        assert_eq!(capped.rows[0], labelled, "the labelled row leads");
        assert_eq!(
            capped.by_reason,
            BTreeMap::from([
                (Stop::ValueCallee, MAX_PER_SERVICE as u32 + 9),
                (Stop::Unplaced, 1),
                (Stop::Recursion, 1),
            ])
        );
    }

    fn file_rows(file: &str, n: u32) -> Vec<UnreadSite> {
        (1..=n).map(|l| stop(file, l, Stop::ValueCallee)).collect()
    }

    fn labelled_row(file: &str, line: u32) -> UnreadSite {
        UnreadSite {
            labelled_end: Some(LabelledEndRef {
                package: "a-http-lib".to_string(),
                export: "Client".to_string(),
                member: None,
            }),
            ..stop(file, line, Stop::Unplaced)
        }
    }

    /// 358 files of 7 rows under `a/` and 71 under `z/`.
    fn two_folders() -> Vec<UnreadSite> {
        let mut rows = Vec::new();
        for i in 0..358 {
            rows.extend(file_rows(&format!("a/f{i}.ts"), 7));
        }
        for i in 0..71 {
            rows.extend(file_rows(&format!("z/f{i}.ts"), 7));
        }
        rows
    }

    fn operation_row(file: &str, slot: &str, reason: Stop, kind: Option<&str>) -> UnreadSite {
        UnreadSite {
            slot: slot.to_string(),
            kind: kind.map(str::to_string),
            role: kind.map(|_| "consumer".to_string()),
            line: 3000,
            span_start: 30000,
            span_end: 30060,
            ..stop(file, 3000, reason)
        }
    }

    #[test]
    fn a_row_that_names_an_operation_is_listed_past_the_cap() {
        let mut rows = Vec::new();
        for file in ["relay/bus.ts", "gate/send.ts", "gate/routes.ts"] {
            rows.extend(file_rows(file, 2100));
        }
        rows.push(operation_row(
            "relay/bus.ts",
            "name",
            Stop::Unanswered,
            None,
        ));
        rows.push(operation_row(
            "gate/send.ts",
            "path",
            Stop::Unanswered,
            Some("http"),
        ));
        rows.push(operation_row(
            "gate/routes.ts",
            "path",
            Stop::UnreadExpression,
            None,
        ));
        let capped = cap(rows);
        assert_eq!(capped.total, 6303);
        assert_eq!(capped.rows.len(), 3);
        assert!(capped.rows.iter().all(|r| r.line == 3000));
        assert_eq!(
            capped.not_listed,
            BTreeMap::from([
                ("gate/routes.ts".to_string(), 2100),
                ("gate/send.ts".to_string(), 2100),
                ("relay/bus.ts".to_string(), 2100),
            ])
        );
    }

    #[test]
    fn a_call_around_an_operation_row_is_listed_with_it() {
        let mut rows = file_rows("relay/bus.ts", 2100);
        let at = |start: u32, end: u32, reason: Stop| UnreadSite {
            line: 3000,
            span_start: start,
            span_end: end,
            ..stop("relay/bus.ts", 3000, reason)
        };
        rows.push(UnreadSite {
            span_start: 30000,
            span_end: 30100,
            line: 3000,
            ..labelled_row("relay/bus.ts", 3000)
        });
        rows.push(at(29990, 30110, Stop::MissedInside));
        rows.push(at(30020, 30040, Stop::ValueCallee));
        rows.push(at(30200, 30210, Stop::ValueCallee));
        let capped = cap(rows);
        assert_eq!(capped.rows.len(), 2);
        assert!(capped.rows.iter().any(|r| r.labelled_end.is_some()));
        assert!(
            capped
                .rows
                .iter()
                .any(|r| r.reason == Stop::MissedInside && r.span_start == 29990)
        );
        assert_eq!(capped.not_listed["relay/bus.ts"], 2102);
    }

    #[test]
    fn a_folder_that_sorts_last_keeps_rows_past_the_cap() {
        let capped = cap(two_folders());
        assert!(capped.rows.iter().any(|r| r.file.starts_with("z/")));
    }

    #[test]
    fn past_the_cap_every_file_is_whole_or_counted() {
        let capped = cap(two_folders());
        let mut listed: BTreeMap<&str, u32> = BTreeMap::new();
        for row in &capped.rows {
            *listed.entry(row.file.as_str()).or_default() += 1;
        }
        assert!(capped.rows.len() <= MAX_PER_SERVICE);
        assert_eq!(capped.total, 3003);
        for (file, n) in &listed {
            assert_eq!(*n, 7, "{file} is split");
            assert!(!capped.not_listed.contains_key(*file), "{file} both");
        }
        for (file, n) in &capped.not_listed {
            assert_eq!(*n, 7, "{file}");
            assert!(*n >= 1);
        }
        let cut: u32 = capped.not_listed.values().sum();
        assert_eq!(capped.total, capped.rows.len() + cut as usize);
    }

    /// The big file's name hashes first in sha256 order (computed by running
    /// the hash over candidate names): `big0.ts` is the first of the 11 files.
    #[test]
    fn a_file_larger_than_the_cap_is_counted_not_split() {
        let mut names: Vec<String> = (0..10).map(|i| format!("s{i}.ts")).collect();
        let big = (0..)
            .map(|i| format!("big{i}.ts"))
            .find(|b| {
                let h = |f: &str| Sha256::digest(f.as_bytes()).to_vec();
                names.iter().all(|n| h(b) < h(n))
            })
            .unwrap();
        let mut rows = file_rows(&big, 2100);
        for n in names.drain(..) {
            rows.extend(file_rows(&n, 3));
        }
        let capped = cap(rows);
        assert_eq!(capped.rows.len(), 30);
        assert_eq!(capped.not_listed, BTreeMap::from([(big, 2100)]));
    }

    #[test]
    fn every_labelled_row_stays_listed_under_the_labelled_cap() {
        let mut rows = Vec::new();
        for i in 0..300 {
            for l in 1..=5 {
                rows.push(labelled_row(&format!("f{i}.ts"), l));
            }
        }
        for i in 0..250 {
            for l in 100..108 {
                rows.push(stop(&format!("f{i}.ts"), l, Stop::ValueCallee));
            }
        }
        let capped = cap(rows);
        let labelled = capped.rows.iter().filter(|r| r.labelled_end.is_some());
        assert_eq!(labelled.count(), 1500);
        assert!(capped.rows.len() <= MAX_PER_SERVICE);
        assert_eq!(capped.total, 3500);
    }

    #[test]
    fn under_the_cap_nothing_moves() {
        let mut rows = vec![
            stop("b.ts", 2, Stop::ValueCallee),
            stop("a.ts", 9, Stop::ValueCallee),
            labelled_row("z.ts", 1),
        ];
        for l in 1..=27 {
            rows.push(stop("c.ts", l, Stop::Recursion));
        }
        let capped = cap(rows);
        assert_eq!(capped.rows.len(), 30);
        assert!(capped.not_listed.is_empty());
        assert_eq!(capped.rows[0].file, "z.ts");
        assert_eq!(capped.rows[1].file, "a.ts");
        assert_eq!(capped.rows[2].file, "b.ts");
        assert_eq!(capped.rows[3].file, "c.ts");
        let mut data = blob();
        attach(&mut data, capped.rows, &[]);
        assert!(data.unread_sites_not_listed.is_none());
    }

    /// One call found twice is one row: the possible call's.
    #[test]
    fn a_repeat_of_a_possible_call_is_the_possible_call() {
        let listed = possible("a.ts", 3, false);
        let mut data = blob();
        attach(
            &mut data,
            vec![stop("a.ts", 3, Stop::ValueCallee)],
            &[listed],
        );
        let rows = data.unread_sites.unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].reason, Stop::Unanswered);
        assert_eq!(data.unread_sites_total, Some(1));
    }

    /// `possible_calls` is the listed rows, field for field what
    /// [`crate::possible_calls::attach`] writes from the same calls; a stop
    /// that reached a labelled call but is no possible call stays out of it.
    #[test]
    fn possible_calls_is_the_listed_projection() {
        let listed = vec![
            possible("b.ts", 9, true),
            possible("a.ts", 2, false),
            possible("a.ts", 2, false),
        ];
        let mut want = blob();
        crate::possible_calls::attach(&mut want, listed.clone());
        let mut data = blob();
        let labelled_stop = UnreadSite {
            labelled_end: Some(LabelledEndRef {
                package: "a-http-lib".to_string(),
                export: "Client".to_string(),
                member: None,
            }),
            ..stop("c.ts", 4, Stop::Recursion)
        };
        attach(
            &mut data,
            vec![stop("a.ts", 7, Stop::ValueCallee), labelled_stop],
            &listed,
        );
        assert_eq!(
            serde_json::to_value(&data.possible_calls).unwrap(),
            serde_json::to_value(&want.possible_calls).unwrap()
        );
        assert_eq!(data.possible_calls_omitted, want.possible_calls_omitted);
        assert_eq!(data.unread_sites_total, Some(4));
    }

    /// A blob written before the fields existed reads as not stated, never
    /// as none.
    #[test]
    fn an_old_blob_reads_as_not_stated() {
        let data = blob();
        assert!(data.unread_sites.is_none());
        assert!(data.unread_sites_total.is_none());
        assert!(data.unread_sites_by_reason.is_none());
        assert!(data.unread_sites_not_listed.is_none());
        let wire = serde_json::to_value(&data).unwrap();
        for key in [
            "unread_sites",
            "unread_sites_total",
            "unread_sites_by_reason",
            "unread_sites_not_listed",
        ] {
            assert!(wire.get(key).is_none(), "{key} written on an old blob");
        }
    }

    /// Every key is spelled on the wire as the readers read it, and the
    /// optional ones are left out when empty; the rows read back as written.
    #[test]
    fn every_key_is_spelled_on_the_wire() {
        let mut data = blob();
        let full = UnreadSite {
            also: vec![Stop::FanOutCap],
            stop_at: Some("src/b.ts:40".to_string()),
            depth: 2,
            labelled_end: Some(LabelledEndRef {
                package: "a-http-lib".to_string(),
                export: "default".to_string(),
                member: Some("post".to_string()),
            }),
            kind: Some("http".to_string()),
            role: Some("consumer".to_string()),
            ..stop("src/a.ts", 12, Stop::SeveralValues)
        };
        attach(
            &mut data,
            vec![
                full,
                UnreadSite {
                    callee: None,
                    ..stop("src/a.ts", 20, Stop::ValueCallee)
                },
            ],
            &[],
        );
        let wire = serde_json::to_value(&data).unwrap();
        assert_eq!(
            wire["unread_sites"],
            serde_json::json!([
                {"file": "src/a.ts", "line": 12, "span_start": 120, "span_end": 125,
                 "reason": "several_values", "also": ["fan_out_cap"], "slot": "callee",
                 "callee": "this.handlers[type]", "stop_at": "src/b.ts:40", "depth": 2,
                 "labelled_end": {"package": "a-http-lib", "export": "default", "member": "post"},
                 "kind": "http", "role": "consumer"},
                {"file": "src/a.ts", "line": 20, "span_start": 200, "span_end": 205,
                 "reason": "value_callee", "slot": "callee", "depth": 0}
            ])
        );
        assert_eq!(wire["unread_sites_total"], 2);
        assert!(wire.get("unread_sites_not_listed").is_none());
        data.unread_sites_not_listed = Some(BTreeMap::from([("src/c.ts".to_string(), 3)]));
        assert_eq!(
            serde_json::to_value(&data).unwrap()["unread_sites_not_listed"],
            serde_json::json!({"src/c.ts": 3})
        );
        data.unread_sites_not_listed = None;
        assert_eq!(
            wire["unread_sites_by_reason"],
            serde_json::json!({"value_callee": 1, "several_values": 1})
        );
        // No possible calls, so the field is written empty.
        assert_eq!(wire["possible_calls"], serde_json::json!([]));
        let back: CloudRepoData = serde_json::from_value(wire).unwrap();
        assert_eq!(back.unread_sites.map(|rows| rows.len()), Some(2));
        assert_eq!(back.unread_sites_by_reason.map(|m| m.len()), Some(2));
    }

    /// carrick#3684: a walk that passed a call into a package the scan ended
    /// without an answer for keeps its own reason and says so in `also`.
    #[test]
    fn a_walk_that_passed_an_unready_package_says_so_in_also() {
        let mut discovery = Discovery::default();
        let at = ("src/o.ts".to_string(), 10, 20);
        discovery.missed_inside.push(at.clone());
        discovery
            .missed_at
            .insert(at.clone(), (4, "announce(b)".to_string()));
        let other = ("src/p.ts".to_string(), 30, 40);
        discovery.missed_inside.push(other.clone());
        discovery
            .missed_at
            .insert(other, (5, "plain(b)".to_string()));
        discovery
            .unanswered_at
            .insert(at, ["fixture-pending".to_string()].into());
        let rows = from_scan(
            &discovery,
            &Decided::default(),
            &[],
            &[],
            Path::new("/repo"),
        );
        let by_file = |file: &str| rows.iter().find(|row| row.file == file).unwrap();
        assert_eq!(by_file("src/o.ts").reason, Stop::MissedInside);
        assert_eq!(by_file("src/o.ts").also, vec![Stop::UnansweredPackage]);
        assert!(by_file("src/p.ts").also.is_empty());
    }

    /// A walk that stopped with no labelled end keeps the last function it
    /// entered and how deep it went; a stop with a labelled end keeps the
    /// end's kind, role and slot.
    #[test]
    fn a_stop_states_where_it_stopped() {
        let site = DiscoverySite {
            file: "src/a.ts".to_string(),
            line: 3,
            span_start: 30,
            span_end: 44,
            callee: "  send(path)".to_string(),
            end: None,
            chain: vec![crate::request_summary::discovery::ChainHop {
                function: "send".to_string(),
                file: "src/b.ts".to_string(),
                line: 40,
                made: None,
            }],
            stop: Some(Stop::NoBody),
            receiver: None,
            reached: None,
            bound: Vec::new(),
            also: Vec::new(),
        };
        let discovery = Discovery {
            sites: vec![site],
            ..Discovery::default()
        };
        let end = LabelledEnd {
            package: "a-queue-lib".to_string(),
            export: "bus".to_string(),
            member: Some("publish".to_string()),
            role: "broker".to_string(),
            op: Some("send".to_string()),
            file: "src/c.ts".to_string(),
            line: 9,
            span_start: 90,
            span_end: 99,
            new: false,
            scope: None,
            procedure: None,
        };
        let decided = Decided {
            stops: vec![DecidedStop {
                file: "src/c.ts".to_string(),
                line: 5,
                span_start: 50,
                span_end: 60,
                callee: Some("emit".to_string()),
                depth: 1,
                end,
                reason: Stop::Recursion,
            }],
            ..Decided::default()
        };
        let rows = from_scan(&discovery, &decided, &[], &[], Path::new("/repo"));
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].reason, Stop::NoBody);
        assert_eq!(rows[0].stop_at.as_deref(), Some("src/b.ts:40"));
        assert_eq!(
            (rows[0].depth, rows[0].callee.as_deref()),
            (1, Some("send(path)"))
        );
        assert_eq!(rows[1].reason, Stop::Recursion);
        assert_eq!(
            (
                rows[1].kind.as_deref(),
                rows[1].role.as_deref(),
                rows[1].slot.as_str()
            ),
            (Some("pubsub"), Some("producer"), "name")
        );
    }

    /// carrick#3991: a call the dead declaration ask would have decided says
    /// `unanswered` beside its reason; a row there that reached a labelled
    /// end is listed for a slot the answer does not decide, and is left alone.
    #[test]
    fn a_dead_declaration_ask_marks_only_rows_with_no_labelled_end() {
        let site = DiscoverySite {
            file: "src/a.ts".to_string(),
            line: 3,
            span_start: 30,
            span_end: 44,
            callee: "app.get".to_string(),
            end: None,
            chain: Vec::new(),
            stop: Some(Stop::PackageUnmatched),
            receiver: None,
            reached: None,
            bound: Vec::new(),
            also: Vec::new(),
        };
        let end = LabelledEnd {
            package: "a-server-lib".to_string(),
            export: "default".to_string(),
            member: Some("get".to_string()),
            role: "server_framework".to_string(),
            op: Some("receive".to_string()),
            file: "src/c.ts".to_string(),
            line: 5,
            span_start: 50,
            span_end: 60,
            new: false,
            scope: None,
            procedure: None,
        };
        let discovery = Discovery {
            unfollowed: vec![site],
            declarations_unanswered: [
                ("src/a.ts".to_string(), 30, 44),
                ("src/c.ts".to_string(), 50, 60),
            ]
            .into(),
            ..Discovery::default()
        };
        let decided = Decided {
            stops: vec![DecidedStop {
                file: "src/c.ts".to_string(),
                line: 5,
                span_start: 50,
                span_end: 60,
                callee: Some("app.get".to_string()),
                depth: 0,
                end,
                reason: Stop::Recursion,
            }],
            ..Decided::default()
        };
        let rows = from_scan(&discovery, &decided, &[], &[], Path::new("/repo"));
        let by_file = |file: &str| rows.iter().find(|row| row.file == file).unwrap();
        assert_eq!(by_file("src/a.ts").reason, Stop::PackageUnmatched);
        assert_eq!(by_file("src/a.ts").also, vec![Stop::Unanswered]);
        assert_eq!(by_file("src/c.ts").reason, Stop::Recursion);
        assert!(by_file("src/c.ts").also.is_empty());
    }

    /// A caller the send side could not compose is one unread site at the
    /// call, with the reason and the HTTP consumer role the request has.
    #[test]
    fn a_caller_stop_is_an_unread_site_at_the_call() {
        let caller = CallerStop {
            file: "/repo/src/use.ts".into(),
            line: 6,
            span_start: 60,
            span_end: 72,
            callee: "items.list".to_string(),
            request_at: ("/repo/src/store.ts".into(), 20),
            reason: Stop::Reassigned,
            also: vec![Stop::UnreadExpression],
        };
        let rows = from_scan(
            &Discovery::default(),
            &Decided::default(),
            &[caller],
            &[],
            Path::new("/repo"),
        );
        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = &rows[0];
        assert_eq!((row.file.as_str(), row.line), ("src/use.ts", 6));
        assert_eq!(row.reason, Stop::Reassigned);
        assert_eq!(row.also, vec![Stop::UnreadExpression]);
        assert_eq!(
            (row.slot.as_str(), row.kind.as_deref(), row.role.as_deref()),
            ("path", Some("http"), Some("consumer"))
        );
    }

    /// Two chains' leads at one call, each with its own stop, are one row:
    /// the first lead's stop is the reason and the second's joins `also`.
    #[test]
    fn two_library_leads_at_one_call_are_both_kept() {
        let lead = |reason: Stop| LibraryLead {
            file: "/repo/src/use.ts".into(),
            line: 2,
            span_start: 20,
            span_end: 30,
            reason,
            package: "fixture-bus".to_string(),
            export: "bus".to_string(),
            member: Some("publish".to_string()),
            kind: Some("pubsub"),
            role: Some("producer"),
        };
        let rows = from_scan(
            &Discovery::default(),
            &Decided::default(),
            &[],
            &[
                lead(Stop::UnreadExpression),
                lead(Stop::FanOutCap),
                lead(Stop::FanOutCap),
            ],
            Path::new("/repo"),
        );
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].reason, Stop::UnreadExpression);
        assert_eq!(rows[0].also, vec![Stop::FanOutCap]);
    }

    /// carrick#3947, ruled on #4146: a library lead at a call another pass
    /// already saved is not dropped. The held site keeps its reason and gains
    /// the lead's word in `also`, by the same rule an unanswered ask joins.
    #[test]
    fn a_library_lead_at_a_site_already_saved_joins_its_also() {
        let mut discovery = Discovery::default();
        let at = ("src/use.ts".to_string(), 20, 30);
        discovery.missed_inside.push(at.clone());
        discovery
            .missed_at
            .insert(at, (2, "bus.publish(topic, {})".to_string()));
        let rows = from_scan(
            &discovery,
            &Decided::default(),
            &[],
            &[LibraryLead {
                file: "/repo/src/use.ts".into(),
                line: 2,
                span_start: 20,
                span_end: 30,
                reason: Stop::Uncorroborated,
                package: "fixture-bus".to_string(),
                export: "bus".to_string(),
                member: Some("publish".to_string()),
                kind: Some("pubsub"),
                role: Some("producer"),
            }],
            Path::new("/repo"),
        );
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].reason, Stop::MissedInside);
        assert_eq!(rows[0].also, vec![Stop::Uncorroborated]);
    }
}
