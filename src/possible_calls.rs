//! Calls the scan can see but cannot name (carrick#2438, part of
//! carrick#2441; the cloud reader is carrick-cloud#2372): a call where a
//! library sends or receives data whose path, event name or method the code
//! does not state. It goes on the index blob as `possible_calls`, labelled,
//! for the agent to read. It is never a row.
//!
//! - **Never an input.** Nothing here feeds findings, the type check, the
//!   matcher or the PR result. The cloud reader shows them in a section of
//!   their own.
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

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::cloud_storage::CloudRepoData;
use crate::request_summary::Stop;
use crate::request_summary::decided_sites::{Decided, DecidedLead, DecidedSite, LabelledEnd};
use crate::request_summary::site_record::{Bucket, SiteRecord};

/// The most possible calls one service's blob carries.
pub const MAX_PER_SERVICE: usize = 2000;

/// The slot the code does not state, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unread {
    /// `name` (an event, topic or message name) or `path` (an HTTP path).
    pub slot: String,
    pub reason: Stop,
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
    /// Every labelled end the call reaches, in the order the walk found
    /// them, the first included (carrick#3818). Written only when the call
    /// reaches two or more distinct ends; the fields above stay the first
    /// end's, so a reader that does not know this key reads what it always
    /// read. At most [`MAX_ENDS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ends: Vec<PossibleEnd>,
}

/// The most ends one possible call lists.
pub const MAX_ENDS: usize = 8;

/// One end a possible call reaches, with its own kind and role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PossibleEnd {
    pub labelled_end: LabelledEndRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl PossibleEnd {
    /// The end `row` itself names, when it names one.
    fn of(row: &PossibleCall) -> Option<Self> {
        Some(PossibleEnd {
            labelled_end: row.labelled_end.clone()?,
            kind: row.kind.clone(),
            role: row.role.clone(),
        })
    }
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
pub(crate) fn kind_of(role: &str) -> Option<&'static str> {
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
pub(crate) fn role_of(role: &str, op: Option<&str>) -> Option<&'static str> {
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

pub(crate) fn labelled_end(end: &LabelledEnd) -> LabelledEndRef {
    LabelledEndRef {
        package: end.package.clone(),
        export: end.export.clone(),
        member: end.member.clone(),
    }
}

pub(crate) fn slot_of(role: &str) -> &'static str {
    if matches!(role, "http_client" | "server_framework") {
        "path"
    } else {
        "name"
    }
}

/// Why a decided site's name is unstated: an identifier the compiler reads
/// as `any` with no declaration is an import the scan could not read, the
/// narrower fact; except a name the code builds over an environment read,
/// whose `process` has no declaration either and which `env_value` names
/// (carrick#3846); else the hole the resolver met reading the name
/// (carrick#3688 U2), whose text reader reads every declaration it cannot
/// enter as `outside_service`. Otherwise nothing says, and the call was not
/// checked on this scan.
fn reason_of(record: &SiteRecord) -> Stop {
    if record.unread == Some(Stop::EnvValue) {
        return Stop::EnvValue;
    }
    let unreadable = record.compiler().is_some_and(|compiler| {
        compiler.names.iter().any(|name| {
            name.declaration.is_none()
                && name.package.is_none()
                && name.type_text.as_deref() == Some("any")
        })
    });
    if unreadable {
        return Stop::UnresolvedImport;
    }
    record.unread.unwrap_or(Stop::Unanswered)
}

pub(crate) fn from_lead(lead: &DecidedLead) -> PossibleCall {
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
            reason: lead.reason,
        },
        trace_depth: lead.depth,
        ends: Vec::new(),
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
            site.file == at.file
                && site.span_start == at.span_start
                && site.span_end == at.span_end
                && site.key == record.anchors.key
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
            ends: Vec::new(),
        });
    }
    out.extend(
        decided
            .leads
            .iter()
            .filter(|lead| !class_only_graphql(lead))
            .map(from_lead),
    );
    out
}

/// A call on a GraphQL client export the store labels with no op: GraphQL
/// operations are labelled by class only (the document names the operation,
/// read by [`crate::graphql_document_sites`]), so such a call is not listed
/// as a possible call. Every other export with no op is (carrick#2515).
pub(crate) fn class_only_graphql(lead: &DecidedLead) -> bool {
    lead.reason == Stop::NoOpClaim && lead.end.role == "graphql_client"
}

/// A decided site that states no row, for `reason` (carrick#2439): its
/// answer fell below the band, or it was not answered.
pub fn from_site(site: &DecidedSite, reason: Stop) -> PossibleCall {
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
        ends: Vec::new(),
    }
}

/// Order the rows (file, line, span), fold the repeats of one call into it,
/// and keep [`MAX_PER_SERVICE`]. The first row of a call by input order stays
/// as it is; a repeat that reaches another labelled end adds that end to its
/// `ends` (carrick#3818). Returns the rows kept and how many were left out.
pub fn cap(mut rows: Vec<PossibleCall>) -> (Vec<PossibleCall>, usize) {
    rows.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
    let mut at: BTreeMap<(String, u32, u32), usize> = BTreeMap::new();
    let mut kept: Vec<PossibleCall> = Vec::new();
    for row in rows {
        let key = (row.file_path.clone(), row.span_start, row.span_end);
        let Some(&i) = at.get(&key) else {
            at.insert(key, kept.len());
            kept.push(row);
            continue;
        };
        let first = &mut kept[i];
        let Some(end) = PossibleEnd::of(&row) else {
            continue;
        };
        if first.ends.is_empty() {
            first.ends.extend(PossibleEnd::of(first));
        }
        if first.ends.len() < MAX_ENDS && !first.ends.contains(&end) {
            first.ends.push(end);
        }
    }
    for row in &mut kept {
        if row.ends.len() < 2 {
            row.ends.clear();
        }
    }
    let omitted = kept.len().saturating_sub(MAX_PER_SERVICE);
    kept.truncate(MAX_PER_SERVICE);
    (kept, omitted)
}

/// Write a service's possible calls on its blob. Its one caller is
/// [`crate::unread_sites::attach`], which writes them from the unread
/// sites' listed rows (carrick#3688 U3).
pub fn attach(data: &mut CloudRepoData, rows: Vec<PossibleCall>) {
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
                reason: Stop::Unanswered,
            },
            trace_depth: 0,
            ends: Vec::new(),
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

    /// carrick#3818: a wrapper whose call reaches a `fetch` and an
    /// `Agent.close` lists both, in order; the first end's fields stay the
    /// row's own, and a call with one end writes no `ends` key.
    #[test]
    fn a_call_reaching_two_ends_lists_both_and_keeps_the_first() {
        let end = |export: &str, member: Option<&str>, role: Option<&str>| {
            let mut row = call("a.ts", 3);
            row.labelled_end = Some(LabelledEndRef {
                package: "undici".into(),
                export: export.into(),
                member: member.map(str::to_string),
            });
            row.kind = role.map(|_| "http".to_string());
            row.role = role.map(str::to_string);
            row
        };
        let close = end("Agent", Some("close"), None);
        let fetch = end("fetch", None, Some("consumer"));
        let (kept, _) = cap(vec![close.clone(), fetch.clone(), close.clone()]);
        assert_eq!(kept.len(), 1);
        let row = &kept[0];
        assert_eq!(row.labelled_end, close.labelled_end);
        assert_eq!(row.role, None);
        assert_eq!(row.ends.len(), 2);
        assert_eq!(row.ends[0].labelled_end.export, "Agent");
        assert_eq!(row.ends[1].labelled_end.export, "fetch");
        assert_eq!(row.ends[1].role.as_deref(), Some("consumer"));
        let json = serde_json::to_value(&kept).unwrap();
        assert_eq!(json[0]["ends"].as_array().unwrap().len(), 2);

        let (one, _) = cap(vec![close.clone(), close]);
        assert!(one[0].ends.is_empty());
        assert!(serde_json::to_value(&one).unwrap()[0].get("ends").is_none());
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

    /// The rows and the omitted count are written; no count when none were
    /// left out.
    #[test]
    fn the_rows_and_the_count_are_written() {
        let mut data = blob();
        attach(&mut data, vec![call("a.ts", 1)]);
        let wire = serde_json::to_value(&data).unwrap();
        assert_eq!(wire["possible_calls"].as_array().map(Vec::len), Some(1));
        assert!(wire.get("possible_calls_omitted").is_none());

        let mut over = blob();
        let rows = (1..=MAX_PER_SERVICE as u32 + 3)
            .map(|n| call("a.ts", n))
            .collect();
        attach(&mut over, rows);
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
        assert_eq!(rows[0].unread.reason, Stop::UnresolvedImport);
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
            let parsed: Stop = serde_json::from_value(serde_json::json!(reason)).unwrap();
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
            scope: None,
            procedure: None,
            span_end: 20,
        };
        let lead = |end: LabelledEnd| DecidedLead {
            file: "src/a.ts".to_string(),
            line: 3,
            span_start: 10,
            span_end: 20,
            depth: 0,
            end,
            reason: Stop::NoOpClaim,
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

    fn lead_of(reason: Stop) -> DecidedLead {
        DecidedLead {
            file: "src/a.ts".to_string(),
            line: 3,
            span_start: 10,
            span_end: 20,
            depth: 1,
            end: LabelledEnd {
                package: "a-http-lib".to_string(),
                export: "Client".to_string(),
                member: Some("get".to_string()),
                role: "http_client".to_string(),
                op: Some("get".to_string()),
                file: "src/a.ts".to_string(),
                line: 3,
                span_start: 10,
                new: false,
                scope: None,
                procedure: None,
                span_end: 20,
            },
            reason,
        }
    }

    /// The reason a lead is written with is the lead's own stop, whatever it
    /// is: no map to a coarser word (carrick#3688 U1).
    fn written_reason(reason: Stop) -> String {
        let decided = Decided {
            leads: vec![lead_of(reason)],
            ..Decided::default()
        };
        let found = from_sites(&decided, &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        serde_json::to_value(&found[0]).unwrap()["unread"]["reason"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn a_lead_whose_values_disagree_reads_several_values() {
        assert_eq!(written_reason(Stop::SeveralValues), "several_values");
    }

    #[test]
    fn a_callback_parameter_lead_reads_callback_parameter() {
        assert_eq!(
            written_reason(Stop::CallbackParameter),
            "callback_parameter"
        );
    }

    #[test]
    fn several_operations_reads_several_operations() {
        assert_eq!(
            written_reason(Stop::SeveralOperations),
            "several_operations"
        );
    }

    /// A lead record built from a name the resolver read to a hole
    /// (`unread`, carrick#3688 U2), whose compiler names nothing typed
    /// `any`: a reason chosen by the record's own names alone would say
    /// `unanswered`. The row says the hole.
    fn lead_record(unread: Option<Stop>) -> SiteRecord {
        lead_record_with(unread, Vec::new())
    }

    fn lead_record_with(unread: Option<Stop>, names: Vec<serde_json::Value>) -> SiteRecord {
        use crate::request_summary::site_record::{AnchorAt, TypeAnchors};
        let state = serde_json::from_value(serde_json::json!({
            "point": { "at": "src/emit.ts:4", "code": "" },
            "compiler": { "names": names, "callees": [], "enclosing": null, "reached": [] },
            "syntax": [],
            "libraries": [],
            "call": null,
            "stated": { "names": [], "methods": [] },
            "asked": [],
            "forced": {},
        }))
        .unwrap();
        SiteRecord {
            state,
            anchors: TypeAnchors {
                call: AnchorAt {
                    file: "src/emit.ts".to_string(),
                    line: 4,
                    span_start: 40,
                    span_end: 60,
                },
                payload: None,
                payload_at: None,
                handler: None,
                key: None,
            },
            payloads: Default::default(),
            basis: Default::default(),
            names: Vec::new(),
            bucket: Bucket::Lead,
            lead: Some("name_unstated"),
            unread,
            receiver: None,
        }
    }

    #[test]
    fn a_lead_record_names_its_hole() {
        let reason = |unread| {
            let found = from_sites(&Decided::default(), &[lead_record(unread)]);
            assert_eq!(found.len(), 1, "{found:?}");
            found[0].unread.reason
        };
        assert_eq!(reason(Some(Stop::Reassigned)), Stop::Reassigned);
        // The control: a record whose name met no hole keeps today's word.
        assert_eq!(reason(None), Stop::Unanswered);
    }

    /// A name built over an environment read: `process` is an undeclared
    /// `any` to the compiler, which would read as an unreadable import; the
    /// record's `env_value` is the narrower word (carrick#3846).
    #[test]
    fn an_environment_read_beats_the_undeclared_any_it_reads_through() {
        let process = serde_json::json!({
            "text": "process", "at": "src/emit.ts:4", "declaration": null,
            "declaration_text": null, "package": null, "type": "any", "literal": null,
        });
        let reason = |unread| {
            let record = lead_record_with(unread, vec![process.clone()]);
            from_sites(&Decided::default(), &[record])[0].unread.reason
        };
        assert_eq!(reason(Some(Stop::EnvValue)), Stop::EnvValue);
        assert_eq!(reason(None), Stop::UnresolvedImport);
    }
}
