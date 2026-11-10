//! Every call site the fact layer considered and did not state as a row,
//! with the one reason it stopped (carrick#3688 U3). It goes on the index
//! blob as `unread_sites`, beside its exact total and its exact count per
//! reason, for the agent to read. It is never a row.
//!
//! - **One producer.** [`from_scan`] reads the stops the site pass holds
//!   (discovery's unfollowed calls, its walks that stopped with no labelled
//!   end, its walks that missed a call inside, the decided pass's unplaced
//!   calls and caller cycles, and the class-only GraphQL calls). [`attach`]
//!   joins them to the possible calls the fold lists, and writes both
//!   fields from that one list: `possible_calls` is the projection of the
//!   listed rows ([`crate::possible_calls`]), byte for byte what it was
//!   before this field, until its readers move to this one (U5).
//! - **Never an input.** Nothing here feeds findings, the type check, the
//!   matcher or the PR result.
//! - **Cap.** At most [`MAX_PER_SERVICE`] rows per service, the rows that
//!   reached a labelled library call first, then by file, line and span.
//!   `unread_sites_total` and `unread_sites_by_reason` count every row,
//!   past the cap too.
//! - **Words.** `reason` and `also` are [`Stop`] words; the phrase a reader
//!   shows for each is `npm/carrick/src/unread-reasons.json`. Files are
//!   repo-relative.
//!
//! The field's contract (keys, cap, size bound) is carrick#3688's "The
//! index field" section.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::cloud_storage::CloudRepoData;
use crate::possible_calls::{
    LabelledEndRef, PossibleCall, Unread, kind_of, labelled_end, role_of, slot_of,
};
use crate::request_summary::decided_sites::{Decided, DecidedStop};
use crate::request_summary::discovery::{Discovery, DiscoverySite};
use crate::request_summary::{CallerStop, Stop};

/// The most unread sites one service's blob carries.
pub const MAX_PER_SERVICE: usize = 2000;

/// The slot of a call whose callee the walk did not read.
const CALLEE: &str = "callee";

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
            labelled_end: None,
            kind: None,
            role: None,
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
        if unanswered
            && row.reason != Stop::UnansweredPackage
            && !row.also.contains(&Stop::UnansweredPackage)
            && row.also.len() < 4
        {
            row.also.push(Stop::UnansweredPackage);
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
    out
}

/// What [`cap`] keeps, and what it counts.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Capped {
    pub rows: Vec<UnreadSite>,
    /// Every row once, past the cap too.
    pub total: usize,
    pub by_reason: BTreeMap<Stop, u32>,
}

/// Drop a repeat of one call (a possible call first, then a row that
/// reached a labelled call), count what is left, and keep
/// [`MAX_PER_SERVICE`]: the rows with a labelled end first, then by file,
/// line and span.
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
    let total = rows.len();
    rows.truncate(MAX_PER_SERVICE);
    Capped {
        rows,
        total,
        by_reason,
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
        rows,
        total,
        by_reason,
    } = cap(rows);
    data.unread_sites = Some(rows);
    data.unread_sites_total = (total > 0).then_some(total as u32);
    data.unread_sites_by_reason = (!by_reason.is_empty()).then_some(by_reason);
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
        assert_eq!(capped.rows.len(), MAX_PER_SERVICE);
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
        let wire = serde_json::to_value(&data).unwrap();
        for key in [
            "unread_sites",
            "unread_sites_total",
            "unread_sites_by_reason",
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
        let rows = from_scan(&discovery, &Decided::default());
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
        let rows = from_scan(&discovery, &decided);
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
}
