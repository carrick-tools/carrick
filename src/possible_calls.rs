//! Calls the scan can see but cannot name (carrick#2438, part of
//! carrick#2441; the cloud reader is carrick-cloud#2372): a call where a
//! library sends or receives data whose path, event name or method the code
//! does not state. It goes on the index blob as `possible_calls`, labelled,
//! for the agent to read. It is never a row.
//!
//! - **Never an input.** Nothing here feeds findings, the type check, the
//!   matcher or the PR result. The cloud reader shows them in a section of
//!   their own.
//! - **Switch.** Written only when `CARRICK_SITE_QUESTIONS=1`
//!   ([`crate::site_questions::enabled`]). With the switch off the blob has
//!   neither key, byte for byte as before.
//! - **Cap.** At most [`MAX_PER_SERVICE`] rows per service, in a fixed
//!   order; the count past that is `possible_calls_omitted`, so the blob
//!   stays inside the ingest ceilings. One cap, here: every producer of a
//!   possible call (the decided-site leads below, and the site answers that
//!   fall below the fold band, carrick#2439) goes through [`cap`].
//! - **Names.** The keys, the closed list of reasons and the kind and role
//!   words are what the reader reads (`lambdas/mcp-server/src/types.ts`
//!   `PossibleCall`, `utils/possible-calls.ts`). Files are repo-relative.
//!
//! Producers here: a decided site whose name is unstated (a record in
//! [`Bucket::Lead`](crate::request_summary::site_record::Bucket)), and a
//! discovered call whose name no call of this code decides
//! ([`DecidedLead`]).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::cloud_storage::CloudRepoData;
use crate::request_summary::decided_sites::{
    Decided, DecidedLead, DecidedSite, LabelledEnd, Undecided,
};
use crate::request_summary::site_record::{Bucket, SiteRecord};

/// The most possible calls one service's blob carries.
pub const MAX_PER_SERVICE: usize = 2000;

/// Why a call is a possible call: a closed list, one phrase for each in the
/// reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnreadReason {
    /// The name is only known when the program runs.
    NameNeverBound,
    /// The call could go to more than one implementation.
    SeveralImplementations,
    /// Too many implementations to follow.
    ImplementationsOverCap,
    /// The name comes from an import the scan could not read.
    UnresolvedImport,
    /// The function is passed in from elsewhere and nothing says which.
    InjectedCallee,
    /// The receiver's type is not known.
    UntypedReceiver,
    /// An answer was given and fell below the band that states a row.
    BelowBand,
    /// The call was not checked on this scan.
    Unanswered,
    /// The call reaches a library export the store labels with a network
    /// role and no op, so nothing says what it sends or receives
    /// (carrick#2515). The reader has no phrase for it yet: it keeps the
    /// row and the word, and shows no `why`.
    NoOpClaim,
    /// A site answer says a route is registered here and names no request
    /// method it serves (carrick#2517): a member that mounts an application
    /// under a prefix reads this way. No route is written for it.
    NoMethod,
    /// A site answer says a route is registered here, and the scan cannot
    /// tell which router it hangs from, so the prefix it is mounted under is
    /// unknown (carrick#2517). No route is written for it.
    OwnerUnknown,
    /// A site answer's row rests on a slot only the store's label forced,
    /// the code does not state its name, and no row the file reader stated
    /// at the line agrees (carrick#2520). No row is written for it.
    Uncorroborated,
}

/// The slot the code does not state, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unread {
    /// `name` (an event, topic or message name) or `path` (an HTTP path).
    pub slot: String,
    pub reason: UnreadReason,
}

/// The labelled library call a possible call reaches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelledEndRef {
    pub package: String,
    pub export: String,
    pub member: Option<String>,
}

/// One possible call, as the blob carries it (`possible_calls[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PossibleCall {
    /// Repo-relative, `/`-separated.
    pub file_path: String,
    pub line_number: u32,
    pub span_start: u32,
    pub span_end: u32,
    /// The protocol (`http`, `graphql`, `pubsub`, `socket`), when the label
    /// says one.
    pub kind: Option<String>,
    /// `producer` or `consumer`, when the label's op says one.
    pub role: Option<String>,
    pub labelled_end: Option<LabelledEndRef>,
    pub unread: Unread,
    /// How many functions the walk entered from the call to the labelled
    /// call.
    pub trace_depth: u32,
}

impl PossibleCall {
    fn sort_key(&self) -> (&str, u32, u32, u32) {
        (
            &self.file_path,
            self.line_number,
            self.span_start,
            self.span_end,
        )
    }
}

/// The protocol word for a store role.
fn kind_of(role: &str) -> Option<&'static str> {
    match role {
        "http_client" | "server_framework" => Some("http"),
        "graphql_client" => Some("graphql"),
        "broker" | "in_process_bus" => Some("pubsub"),
        "socket" => Some("socket"),
        _ => None,
    }
}

/// `producer` or `consumer` from the label's role and op. An HTTP client
/// that requests or sends consumes a route; a server both receives and
/// replies as part of the route it serves, so both produce; elsewhere a send
/// produces and a receive consumes.
fn role_of(role: &str, op: Option<&str>) -> Option<&'static str> {
    if role == "server_framework" {
        return matches!(op?, "receive" | "send").then_some("producer");
    }
    let http = matches!(role, "http_client" | "graphql_client");
    match (op?, http) {
        ("request" | "send", true) => Some("consumer"),
        ("send", false) => Some("producer"),
        ("receive", false) => Some("consumer"),
        _ => None,
    }
}

fn labelled_end(end: &LabelledEnd) -> LabelledEndRef {
    LabelledEndRef {
        package: end.package.clone(),
        export: end.export.clone(),
        member: end.member.clone(),
    }
}

fn slot_of(role: &str) -> &'static str {
    if matches!(role, "http_client" | "server_framework") {
        "path"
    } else {
        "name"
    }
}

/// Why a decided site's name is unstated: an identifier the compiler reads
/// as `any` with no declaration is an import the scan could not read.
/// Otherwise nothing says, and the call was not checked on this scan.
fn reason_of(record: &SiteRecord) -> UnreadReason {
    let unreadable = record.compiler().is_some_and(|compiler| {
        compiler.names.iter().any(|name| {
            name.declaration.is_none()
                && name.package.is_none()
                && name.type_text.as_deref() == Some("any")
        })
    });
    if unreadable {
        UnreadReason::UnresolvedImport
    } else {
        UnreadReason::Unanswered
    }
}

fn reason_of_undecided(reason: Undecided) -> UnreadReason {
    match reason {
        // A parameter of a callback this code does not call is, like a
        // parameter nothing fills, a name only the running program knows.
        Undecided::NameNeverBound | Undecided::CallbackParameter => UnreadReason::NameNeverBound,
        // The program picks the name when it runs, by the call's arguments.
        Undecided::SeveralOperations => UnreadReason::NameNeverBound,
        Undecided::NoOpClaim => UnreadReason::NoOpClaim,
        // A value passed in from elsewhere, and its values disagree on which
        // labelled export runs.
        Undecided::SeveralValues => UnreadReason::InjectedCallee,
    }
}

fn from_undecided(lead: &DecidedLead) -> PossibleCall {
    PossibleCall {
        file_path: lead.file.clone(),
        line_number: lead.line,
        span_start: lead.span_start,
        span_end: lead.span_end,
        kind: kind_of(&lead.end.role).map(str::to_string),
        role: role_of(&lead.end.role, lead.end.op.as_deref()).map(str::to_string),
        labelled_end: Some(labelled_end(&lead.end)),
        unread: Unread {
            slot: slot_of(&lead.end.role).to_string(),
            reason: reason_of_undecided(lead.reason),
        },
        trace_depth: lead.depth,
    }
}

/// The possible calls a service's decided sites hold: the records in the
/// lead bucket, and the discovered calls no call of this code decides.
/// Unordered and uncapped; [`cap`] orders and bounds them.
pub fn from_sites(decided: &Decided, records: &[SiteRecord]) -> Vec<PossibleCall> {
    let mut out: Vec<PossibleCall> = Vec::new();
    for record in records.iter().filter(|r| r.bucket == Bucket::Lead) {
        let at = &record.anchors.call;
        // The record's own decided site carries the labelled end and the
        // depth the record does not.
        let site = decided.sites.iter().find(|site| {
            site.file == at.file && site.span_start == at.span_start && site.span_end == at.span_end
        });
        out.push(PossibleCall {
            file_path: at.file.clone(),
            line_number: at.line,
            span_start: at.span_start,
            span_end: at.span_end,
            kind: site.and_then(|s| kind_of(&s.end.role)).map(str::to_string),
            role: site
                .and_then(|s| role_of(&s.end.role, s.end.op.as_deref()))
                .map(str::to_string),
            labelled_end: site.map(|s| labelled_end(&s.end)),
            unread: Unread {
                slot: site.map_or("name", |s| slot_of(&s.end.role)).to_string(),
                reason: reason_of(record),
            },
            trace_depth: site.map_or(0, |s| s.via.len() as u32),
        });
    }
    out.extend(
        decided
            .leads
            .iter()
            .filter(|lead| !class_only_graphql(lead))
            .map(from_undecided),
    );
    out
}

/// A call on a GraphQL client export the store labels with no op: GraphQL
/// operations are labelled by class only (the document names the operation,
/// read by [`crate::graphql_document_sites`]), so such a call is not listed
/// as a possible call. Every other export with no op is (carrick#2515).
fn class_only_graphql(lead: &DecidedLead) -> bool {
    lead.reason == Undecided::NoOpClaim && lead.end.role == "graphql_client"
}

/// A decided site that states no row, for `reason` (carrick#2439): its
/// answer fell below the band, or it was not answered.
pub fn from_site(site: &DecidedSite, reason: UnreadReason) -> PossibleCall {
    PossibleCall {
        file_path: site.file.clone(),
        line_number: site.line,
        span_start: site.span_start,
        span_end: site.span_end,
        kind: kind_of(&site.end.role).map(str::to_string),
        role: role_of(&site.end.role, site.end.op.as_deref()).map(str::to_string),
        labelled_end: Some(labelled_end(&site.end)),
        unread: Unread {
            slot: slot_of(&site.end.role).to_string(),
            reason,
        },
        trace_depth: site.via.len() as u32,
    }
}

/// Order the rows (file, line, span), drop a repeat of one call, and keep
/// [`MAX_PER_SERVICE`]. Returns the rows kept and how many were left out.
pub fn cap(mut rows: Vec<PossibleCall>) -> (Vec<PossibleCall>, usize) {
    rows.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
    let mut seen: BTreeSet<(String, u32, u32)> = BTreeSet::new();
    rows.retain(|row| seen.insert((row.file_path.clone(), row.span_start, row.span_end)));
    let omitted = rows.len().saturating_sub(MAX_PER_SERVICE);
    rows.truncate(MAX_PER_SERVICE);
    (rows, omitted)
}

/// Write a service's possible calls on its blob when the site-question
/// switch is on; with it off the blob is left as it was.
pub fn attach(data: &mut CloudRepoData, rows: Vec<PossibleCall>) {
    attach_when(crate::site_questions::enabled(), data, rows);
}

/// [`attach`] with the switch given.
pub fn attach_when(on: bool, data: &mut CloudRepoData, rows: Vec<PossibleCall>) {
    if !on {
        return;
    }
    let (rows, omitted) = cap(rows);
    data.possible_calls = Some(rows);
    data.possible_calls_omitted = (omitted > 0).then_some(omitted as u32);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(file: &str, line: u32) -> PossibleCall {
        PossibleCall {
            file_path: file.to_string(),
            line_number: line,
            span_start: line * 10,
            span_end: line * 10 + 5,
            kind: None,
            role: None,
            labelled_end: None,
            unread: Unread {
                slot: "name".into(),
                reason: UnreadReason::Unanswered,
            },
            trace_depth: 0,
        }
    }

    fn blob() -> CloudRepoData {
        serde_json::from_value(serde_json::json!({
            "repo_name": "r",
            "endpoints": [], "calls": [], "mounts": [], "apps": {},
            "imported_handlers": [], "function_definitions": {},
            "config_json": null, "package_json": null, "packages": null,
            "last_updated": "2026-10-17T00:00:00Z", "commit_hash": "abc"
        }))
        .unwrap()
    }

    /// A server's reply is part of the route it serves: `receive` and `send`
    /// of a server framework both produce; a client's request and send
    /// consume.
    #[test]
    fn a_server_reply_is_the_producer_side() {
        assert_eq!(
            role_of("server_framework", Some("receive")),
            Some("producer")
        );
        assert_eq!(role_of("server_framework", Some("send")), Some("producer"));
        assert_eq!(role_of("http_client", Some("request")), Some("consumer"));
        assert_eq!(role_of("http_client", Some("send")), Some("consumer"));
        assert_eq!(role_of("graphql_client", Some("send")), Some("consumer"));
        assert_eq!(role_of("server_framework", Some("request")), None);
        assert_eq!(role_of("server_framework", None), None);
    }

    /// Past the cap the rows kept are the first by file and line, in that
    /// order, and the rest are counted.
    #[test]
    fn past_the_cap_the_rest_are_counted() {
        let rows: Vec<PossibleCall> = (0..MAX_PER_SERVICE as u32 + 7)
            .rev()
            .map(|n| call("a.ts", n + 1))
            .collect();
        let (kept, omitted) = cap(rows);
        assert_eq!(kept.len(), MAX_PER_SERVICE);
        assert_eq!(omitted, 7);
        assert_eq!(kept[0].line_number, 1);
        assert_eq!(
            kept[MAX_PER_SERVICE - 1].line_number,
            MAX_PER_SERVICE as u32
        );
    }

    /// At the cap nothing is omitted, and one call found twice is kept once.
    #[test]
    fn at_the_cap_nothing_is_omitted_and_a_repeat_is_dropped() {
        let mut rows: Vec<PossibleCall> = (1..=MAX_PER_SERVICE as u32)
            .map(|n| call("a.ts", n))
            .collect();
        rows.push(call("a.ts", 1));
        let (kept, omitted) = cap(rows);
        assert_eq!((kept.len(), omitted), (MAX_PER_SERVICE, 0));
    }

    /// With the switch off the blob is as it was: neither key is written.
    #[test]
    fn with_the_switch_off_the_blob_is_unchanged() {
        let before = serde_json::to_string(&blob()).unwrap();
        let mut data = blob();
        attach_when(false, &mut data, vec![call("a.ts", 1)]);
        assert_eq!(serde_json::to_string(&data).unwrap(), before);
        assert!(!before.contains("possible_calls"));
    }

    /// With it on the rows and the omitted count are written; no count when
    /// none were left out.
    #[test]
    fn with_the_switch_on_the_rows_and_the_count_are_written() {
        let mut data = blob();
        attach_when(true, &mut data, vec![call("a.ts", 1)]);
        let wire = serde_json::to_value(&data).unwrap();
        assert_eq!(wire["possible_calls"].as_array().map(Vec::len), Some(1));
        assert!(wire.get("possible_calls_omitted").is_none());

        let mut over = blob();
        let rows = (1..=MAX_PER_SERVICE as u32 + 3)
            .map(|n| call("a.ts", n))
            .collect();
        attach_when(true, &mut over, rows);
        let wire = serde_json::to_value(&over).unwrap();
        assert_eq!(
            wire["possible_calls"].as_array().map(Vec::len),
            Some(MAX_PER_SERVICE)
        );
        assert_eq!(wire["possible_calls_omitted"], 3);
    }

    /// A blob written before the fields existed reads with neither.
    #[test]
    fn an_old_blob_reads_with_neither_field() {
        let data = blob();
        assert!(data.possible_calls.is_none() && data.possible_calls_omitted.is_none());
    }

    /// The rows are the reader's rows: the cloud's ingest fixture
    /// (`lambdas/check-or-upload/index_blob_unknown_keys.fixture.json`,
    /// `possible_calls`) reads into the type and writes back as it was.
    #[test]
    fn the_rows_are_the_readers_rows() {
        let reader = serde_json::json!([
            {"file_path": "src/events.ts", "line_number": 12, "span_start": 240, "span_end": 290,
             "kind": "pubsub", "role": "producer",
             "labelled_end": {"package": "a-queue-lib", "export": "Queue", "member": "publish"},
             "unread": {"slot": "name", "reason": "unresolved_import"}, "trace_depth": 2},
            {"file_path": "src/client.ts", "line_number": 30, "span_start": 600, "span_end": 640,
             "kind": null, "role": null, "labelled_end": null,
             "unread": {"slot": "path", "reason": "injected_callee"}, "trace_depth": 1}
        ]);
        let rows: Vec<PossibleCall> = serde_json::from_value(reader.clone()).unwrap();
        assert_eq!(rows[0].unread.reason, UnreadReason::UnresolvedImport);
        assert_eq!(serde_json::to_value(&rows).unwrap(), reader);
        // Every reason the reader names reads.
        for reason in [
            "name_never_bound",
            "several_implementations",
            "implementations_over_cap",
            "unresolved_import",
            "injected_callee",
            "untyped_receiver",
            "below_band",
            "unanswered",
        ] {
            let parsed: UnreadReason = serde_json::from_value(serde_json::json!(reason)).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), reason);
        }
    }

    /// A call on a GraphQL client export with no op is not listed: GraphQL
    /// operations are labelled by class only. Another export with no op is.
    #[test]
    fn a_class_only_graphql_export_is_not_listed() {
        let end = |package: &str, role: &str| LabelledEnd {
            package: package.to_string(),
            export: "Client".to_string(),
            member: None,
            role: role.to_string(),
            op: None,
            file: "src/a.ts".to_string(),
            line: 3,
            span_start: 10,
            new: false,
            span_end: 20,
        };
        let lead = |end: LabelledEnd| DecidedLead {
            file: "src/a.ts".to_string(),
            line: 3,
            span_start: 10,
            span_end: 20,
            depth: 0,
            end,
            reason: Undecided::NoOpClaim,
        };
        let decided = Decided {
            leads: vec![
                lead(end("a-graphql-lib", "graphql_client")),
                lead(end("a-storage-lib", "http_client")),
            ],
            ..Decided::default()
        };
        let found = from_sites(&decided, &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0]
                .labelled_end
                .as_ref()
                .map(|end| end.package.as_str()),
            Some("a-storage-lib")
        );
    }
}
