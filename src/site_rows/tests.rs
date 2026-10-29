//! Site rows and the fold (carrick#2439), over hand-built records and the
//! cassette answers in `tests/fixtures/site_rows/answers.json`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::json;

use super::*;
use crate::possible_calls::UnreadReason;
use crate::request_summary::decided_sites::{
    Decided, DecidedSite, HandlerAt, LabelledEnd, PayloadBinding, SiteLabel,
};
use crate::request_summary::site_record::{
    AnchorAt, Basis, Bucket, HandlerAnchor, SiteRecord, TypeAnchors,
};
use crate::site_questions::{
    Answered, SiteAnswers, SiteQuestion, SiteState, Slot, SlotAnswer, SlotValue, read_response,
};

const LINE: u32 = 7;

/// The cassette, read as [`crate::site_questions::ask`] reads a response.
fn cassette() -> SiteAnswers {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site_rows/answers.json"),
    )
    .unwrap();
    let (question_set, answers) = read_response::<SiteQuestion>(&text);
    let mut out = SiteAnswers {
        question_set,
        ..SiteAnswers::default()
    };
    for (id, answer) in answers {
        match answer {
            Ok(answered) => {
                out.answered.insert(id, answered);
            }
            Err(failed) => {
                out.unanswered.insert(id, failed);
            }
        }
    }
    out
}

/// A decided site in `file`, at line [`LINE`], span 100..130, reaching
/// `export` of a package labelled `role` with op `op`.
fn site(file: &str, role: &str, op: &str, export: &str) -> DecidedSite {
    DecidedSite {
        file: file.to_string(),
        line: LINE,
        span_start: 100,
        span_end: 130,
        end: LabelledEnd {
            package: "@fixture/wire".to_string(),
            export: export.to_string(),
            member: Some(op.to_string()),
            role: role.to_string(),
            op: Some(op.to_string()),
            file: file.to_string(),
            line: LINE,
            span_start: 100,
            new: false,
            scope: None,
            span_end: 130,
        },
        name: Vec::new(),
        key: None,
        name_args: BTreeSet::new(),
        via: Vec::new(),
        payload: Some(PayloadBinding::Arg { arg: 1, key: None }),
        payload_slot: None,
        handler: Some(HandlerAt {
            file: file.to_string(),
            line: LINE,
            span_start: 120,
            span_end: 129,
            name: None,
        }),
        method: None,
        receiver: None,
        reached: None,
        instances: Vec::new(),
        composed: Vec::new(),
        composed_trimmed: false,
        procedure: None,
    }
}

fn at(file: &str, line: u32, span_start: u32, span_end: u32) -> AnchorAt {
    AnchorAt {
        file: file.to_string(),
        line,
        span_start,
        span_end,
    }
}

/// `site`'s record: `forced` slots stated, `asked` slots asked.
fn record(site: &DecidedSite, forced: &[(Slot, &str)], asked: &[Slot]) -> SiteRecord {
    let forced: BTreeMap<Slot, String> = forced
        .iter()
        .map(|(slot, text)| (*slot, text.to_string()))
        .collect();
    let state: SiteState = serde_json::from_value(json!({
        "point": { "at": format!("{}:{}", site.file, site.line), "code": "" },
        "compiler": { "names": [], "callees": [], "enclosing": null, "reached": [] },
        "syntax": [],
        "libraries": [],
        "call": null,
        "stated": { "names": [], "methods": [] },
        "asked": asked,
        "forced": forced,
    }))
    .unwrap();
    SiteRecord {
        bucket: if asked.is_empty() {
            Bucket::Forced
        } else {
            Bucket::Asked
        },
        state,
        anchors: TypeAnchors {
            call: at(&site.file, site.line, site.span_start, site.span_end),
            payload: site.payload.clone(),
            payload_at: Some(at(&site.file, site.line, 112, 118)),
            handler: Some(HandlerAnchor {
                at: at(&site.file, site.line, 120, 129),
                param: 0,
                param_name: Some("message".to_string()),
            }),
            key: site.key.clone(),
        },
        payloads: Default::default(),
        // A forced name is the code's; every other forced slot the label's.
        basis: forced
            .keys()
            .map(|slot| {
                let basis = if *slot == Slot::Name {
                    Basis::Code
                } else {
                    Basis::Label
                };
                (*slot, basis)
            })
            .collect(),
        lead: None,
        receiver: None,
    }
}

const ASKED: &[Slot] = &[Slot::Kind, Slot::Role, Slot::Name];

/// The labels: the server's socket and the client's.
fn labels() -> Vec<SiteLabel> {
    let label = |export: &str, side: Option<&str>| SiteLabel {
        by_origin: false,
        package: "@fixture/wire".to_string(),
        export: export.to_string(),
        member: Some("emit".to_string()),
        role: "socket".to_string(),
        op: Some("emit".to_string()),
        side: side.map(str::to_string),
        name: None,
        payload: None,
        handler: None,
        reserved: Vec::new(),
        runs: None,
        method: None,
        request: None,
        scope: None,
    };
    vec![
        label("Server", Some("server")),
        label("connect", Some("client")),
    ]
}

/// Every case the cassette answers, with a forced HTTP send and a forced
/// topic subscriber beside them.
fn cases() -> (Decided, Vec<SiteRecord>) {
    let sites = vec![
        site("src/http.ts", "http_client", "request", "get"),
        site("src/topic.ts", "broker", "send", "Queue"),
        site("src/message.ts", "broker", "receive", "Queue"),
        site("src/event.ts", "in_process_bus", "send", "Emitter"),
        site("src/server-socket.ts", "socket", "emit", "Server"),
        site("src/client-socket.ts", "socket", "emit", "connect"),
        site("src/low.ts", "broker", "send", "Queue"),
        site("src/disagree.ts", "broker", "send", "Queue"),
        site("src/no-name.ts", "broker", "send", "Queue"),
        site("src/neither.ts", "broker", "send", "Queue"),
        site("src/low-name.ts", "http_client", "request", "get"),
        site("src/mixed.ts", "broker", "send", "Queue"),
        site("src/role-split.ts", "broker", "send", "Queue"),
        site("src/failed.ts", "broker", "send", "Queue"),
        site("src/forced-http.ts", "http_client", "request", "get"),
        site("src/forced-topic.ts", "broker", "receive", "Queue"),
    ];
    let http_forced: &[(Slot, &str)] = &[
        (Slot::Kind, "http"),
        (Slot::Role, "sends"),
        (Slot::Method, "GET"),
    ];
    let records = sites
        .iter()
        .map(|site| match site.file.as_str() {
            "src/http.ts" | "src/low-name.ts" => record(site, http_forced, &[Slot::Name]),
            "src/forced-http.ts" => record(
                site,
                &[
                    (Slot::Kind, "http"),
                    (Slot::Role, "sends"),
                    (Slot::Method, "POST"),
                    (Slot::Name, "/v1/items"),
                ],
                &[],
            ),
            "src/forced-topic.ts" => record(
                site,
                &[
                    (Slot::Kind, "topic"),
                    (Slot::Role, "handles"),
                    (Slot::Name, "item.added"),
                ],
                &[],
            ),
            _ => record(site, &[], ASKED),
        })
        .collect();
    (
        Decided {
            sites,
            ..Decided::default()
        },
        records,
    )
}

fn rows() -> SiteRows {
    let (decided, records) = cases();
    of(&decided, &records, &labels(), &cassette())
}

fn row<'r>(rows: &'r SiteRows, file: &str) -> Option<&'r SiteRow> {
    rows.rows.iter().find(|row| row.file == file)
}

fn lead_reason(rows: &[PossibleCall], file: &str) -> Option<UnreadReason> {
    rows.iter()
        .find(|lead| lead.file_path == file)
        .map(|lead| lead.unread.reason)
}

/// Each kind names its key: HTTP, a topic, a message (global scope) and an
/// event (service scope), and a socket event whose direction is read from
/// the label's side.
#[test]
fn each_kind_names_its_key_and_scope() {
    let rows = rows();
    let key = |file: &str| row(&rows, file).map(|row| (row.key.clone(), row.side));
    assert_eq!(
        key("src/http.ts"),
        Some((OperationKey::http("GET", "/v1/orders"), RowSide::Call))
    );
    assert_eq!(
        key("src/topic.ts"),
        Some((OperationKey::pubsub("order.settled"), RowSide::Call))
    );
    assert_eq!(row(&rows, "src/topic.ts").unwrap().name_scope, None);
    assert_eq!(
        key("src/message.ts"),
        Some((OperationKey::pubsub("invoice.sent"), RowSide::Endpoint))
    );
    assert_eq!(
        row(&rows, "src/message.ts").unwrap().name_scope,
        Some(carrick_match::NameScope {
            scope: carrick_match::GLOBAL_SCOPE.to_string(),
            namespace: Some("billing".to_string()),
        })
    );
    assert_eq!(
        row(&rows, "src/event.ts")
            .unwrap()
            .name_scope
            .as_ref()
            .map(|scope| scope.scope.as_str()),
        Some(carrick_match::SERVICE_SCOPE)
    );
}

/// One row for a site labelled `role` whose model answered `kind` at 0.95
/// with the orders agreeing, the role and name stated.
fn row_for_kind(role: &str, kind: &str) -> SiteRow {
    let site = site("src/ipc.ts", role, "receive", "ChildProcess");
    let record = record(
        &site,
        &[(Slot::Role, "handles"), (Slot::Name, "message")],
        &[Slot::Kind],
    );
    let mut answers = SiteAnswers::default();
    answers.answered.insert(
        site_id(&record.anchors),
        Answered {
            kind: Some(SlotAnswer {
                value: Some(SlotValue::Text(kind.to_string())),
                confidence: 0.95,
                orders_agree: true,
            }),
            ..Answered::default()
        },
    );
    let decided = Decided {
        sites: vec![site],
        ..Decided::default()
    };
    let mut rows = of(&decided, &[record], &[], &answers).rows;
    assert_eq!(rows.len(), 1, "{role} {kind}");
    rows.remove(0)
}

/// A site whose labelled end is an in-process bus stays in its service
/// whatever kind the model names (carrick#2538), as library-claim rows do
/// (ruling D4); a broker keeps the scope its kind gives.
#[test]
fn an_in_process_site_stays_in_its_service_whatever_its_kind() {
    let service = carrick_match::NameScope {
        scope: carrick_match::SERVICE_SCOPE.to_string(),
        namespace: None,
    };
    for kind in ["topic", "message", "event", "socket_event"] {
        let row = row_for_kind("in_process_bus", kind);
        assert_eq!(row.side, RowSide::Endpoint, "{kind}");
        assert_eq!(row.name_scope, Some(service.clone()), "{kind}");
    }
    let global = carrick_match::NameScope {
        scope: carrick_match::GLOBAL_SCOPE.to_string(),
        namespace: None,
    };
    assert_eq!(
        row_for_kind("broker", "message").name_scope,
        Some(global.clone())
    );
    assert_eq!(row_for_kind("broker", "topic").name_scope, None);

    let scoped = |service, name_scope| carrick_match::ScopedRow {
        service,
        name_scope,
    };
    let row = row_for_kind("in_process_bus", "message");
    let publisher = scoped("billing", Some(&global));
    let listener = scoped("worker", row.name_scope.as_ref());
    assert!(!carrick_match::names_pair(publisher, listener));
    assert!(!carrick_match::names_pair(
        scoped("billing", None),
        scoped("worker", row.name_scope.as_ref())
    ));
    assert!(carrick_match::names_pair(
        scoped("worker", None),
        scoped("worker", row.name_scope.as_ref())
    ));
}

/// A site whose label comes from an export the store marks `by_origin` has no
/// side to read, so its socket event is keyed `UNKNOWN` (carrick#3011).
#[test]
fn a_by_origin_label_keys_its_socket_event_unknown() {
    let exports: Vec<crate::library_claims::ExportClaims> =
        serde_json::from_value(serde_json::json!([{
            "package": "@fixture/wire", "version": "1.0.0", "specifier": "@fixture/wire",
            "export": "Socket", "role": "socket", "side": "client", "side_rule": "by_origin",
            "claims": [
                { "kind": "op", "op": "send", "member": "emit", "on": "export",
                  "name": { "arg": 0 }, "payload": { "arg": 1 } }
            ]
        }]))
        .unwrap();
    let labels = SiteLabel::from_exports(&exports);
    assert!(
        labels
            .iter()
            .all(|label| label.by_origin && label.side.is_none())
    );
    let (decided, records) = cases();
    let decided = Decided {
        sites: decided
            .sites
            .into_iter()
            .map(|mut site| {
                if site.file == "src/server-socket.ts" {
                    site.end.export = "Socket".to_string();
                    site.end.op = Some("send".to_string());
                }
                site
            })
            .collect(),
        ..Decided::default()
    };
    let rows = of(&decided, &records, &labels, &cassette());
    assert_eq!(
        row(&rows, "src/server-socket.ts").map(|row| row.key.clone()),
        Some(OperationKey::socket("chat", SocketDirection::Unknown))
    );
}

/// A send on a server's socket goes server to client, a send on a
/// client's client to server; a handler takes the other direction, and a
/// side the label does not state is unknown.
#[test]
fn a_socket_event_takes_its_direction_from_the_side() {
    let rows = rows();
    assert_eq!(
        row(&rows, "src/server-socket.ts").map(|row| row.key.clone()),
        Some(OperationKey::socket(
            "chat",
            SocketDirection::ServerToClient
        ))
    );
    assert_eq!(
        row(&rows, "src/client-socket.ts").map(|row| row.key.clone()),
        Some(OperationKey::socket(
            "chat",
            SocketDirection::ClientToServer
        ))
    );
    assert_eq!(
        direction(Some("server"), RowSide::Endpoint),
        SocketDirection::ClientToServer
    );
    assert_eq!(
        direction(Some("client"), RowSide::Endpoint),
        SocketDirection::ServerToClient
    );
    for side in [None, Some("both")] {
        assert_eq!(direction(side, RowSide::Call), SocketDirection::Unknown);
    }
}

/// A forced site is a fact under the existing sources and carries no
/// inference; an asked one is the model's, with the answer's question set,
/// confidence and agreement.
#[test]
fn forced_rows_are_facts_and_asked_rows_are_candidates() {
    let rows = rows();
    let http = row(&rows, "src/forced-http.ts").unwrap();
    assert_eq!(http.source, ResolutionSource::RequestSummary);
    assert_eq!(http.inference, None);
    assert_eq!(http.key, OperationKey::http("POST", "/v1/items"));
    let topic = row(&rows, "src/forced-topic.ts").unwrap();
    assert_eq!(topic.source, ResolutionSource::LibraryClaim);
    assert_eq!(topic.inference, None);
    let asked = row(&rows, "src/topic.ts").unwrap();
    assert_eq!(asked.source, ResolutionSource::Model);
    assert_eq!(
        asked.inference,
        Some(SiteInference {
            by: InferenceBy::SiteQuestion,
            question_set: Some("qs-fixture".to_string()),
            confidence: 0.9,
            orders_agree: true,
        })
    );
}

/// The band reads every asked slot the row depends on: a row is in band
/// only when each is at 0.8 or above with both orders agreeing.
#[test]
fn the_band_reads_every_asked_slot_the_row_depends_on() {
    let rows = rows();
    let in_band = |file: &str| row(&rows, file).map(|row| row.in_band);
    assert_eq!(in_band("src/topic.ts"), Some(true));
    assert_eq!(in_band("src/low.ts"), Some(false));
    assert_eq!(in_band("src/disagree.ts"), Some(false));
    assert_eq!(in_band("src/http.ts"), Some(true));
    assert_eq!(in_band("src/low-name.ts"), Some(false));
    // Every asked slot counts: a high kind does not carry a low name or a
    // role whose orders split, and the row carries the lowest.
    assert_eq!(in_band("src/mixed.ts"), Some(false));
    assert_eq!(in_band("src/role-split.ts"), Some(false));
    assert_eq!(in_band("src/message.ts"), Some(true));
    let inference = |file: &str| row(&rows, file).and_then(|row| row.inference.clone());
    assert_eq!(inference("src/mixed.ts").map(|i| i.confidence), Some(0.5));
    assert_eq!(
        inference("src/role-split.ts").map(|i| i.orders_agree),
        Some(false)
    );
    assert_eq!(
        row(&rows, "src/low-name.ts")
            .and_then(|row| row.inference.as_ref())
            .map(|inference| inference.confidence),
        Some(0.58)
    );
}

/// A site that states no row: no option for the name is a lead below the
/// band, an unanswered site a lead unanswered, and a site that is no
/// network call nothing at all.
#[test]
fn sites_that_state_no_row() {
    let rows = rows();
    assert!(row(&rows, "src/no-name.ts").is_none());
    assert_eq!(
        lead_reason(&rows.leads, "src/no-name.ts"),
        Some(UnreadReason::BelowBand)
    );
    assert!(row(&rows, "src/failed.ts").is_none());
    assert_eq!(
        lead_reason(&rows.leads, "src/failed.ts"),
        Some(UnreadReason::Unanswered)
    );
    assert!(row(&rows, "src/neither.ts").is_none());
    assert_eq!(lead_reason(&rows.leads, "src/neither.ts"), None);
}

/// Only the asked records are asked, under their file and span.
#[test]
fn only_asked_records_are_asked() {
    let (_, records) = cases();
    let ids: Vec<String> = questions(&records)
        .into_iter()
        .map(|question| question.site_id)
        .collect();
    assert!(ids.contains(&"src/topic.ts:100-130".to_string()));
    assert!(!ids.iter().any(|id| id.starts_with("src/forced-")));
    assert_eq!(ids.len(), records.len() - 2);
}

// ---------------------------------------------------------------------------
// The fold.

fn blob_row(
    key: OperationKey,
    file: &str,
    line: u32,
    source: Option<ResolutionSource>,
) -> ApiEndpointDetails {
    let mut row: ApiEndpointDetails = serde_json::from_value(json!({
        "owner": null,
        "key": key,
        "params": [],
        "request_body": null,
        "response_body": null,
        "handler_name": null,
        "request_type": null,
        "response_type": null,
        "file_path": format!("{file}:{line}"),
    }))
    .unwrap();
    row.resolution_source = source;
    row
}

fn make(key: OperationKey, file: &Path, line: u32, _: RowSide) -> ApiEndpointDetails {
    blob_row(key, &file.to_string_lossy(), line, None)
}

/// The rows at `file` on `side`, as (key, source, inference).
fn at_site(
    rows: &[ApiEndpointDetails],
    file: &str,
) -> Vec<(OperationKey, Option<ResolutionSource>, Option<Inference>)> {
    rows.iter()
        .filter(|row| row.file_path.to_string_lossy() == format!("{file}:{LINE}"))
        .map(|row| {
            (
                row.key.clone(),
                row.resolution_source,
                row.inference.clone(),
            )
        })
        .collect()
}

/// One site's rows folded against `calls` and `endpoints`.
fn fold_one(
    file: &str,
    calls: &mut Vec<ApiEndpointDetails>,
    endpoints: &mut Vec<ApiEndpointDetails>,
) -> SiteFold {
    let all = rows();
    let one = SiteRows {
        enabled: true,
        rows: all
            .rows
            .into_iter()
            .filter(|row| row.file == file)
            .collect(),
        ..SiteRows::default()
    };
    fold_rows(calls, endpoints, &one, Path::new("."), make)
}

/// A fact site row folds the reader's row at its site, as library rows do.
#[test]
fn a_fact_folds_the_readers_row() {
    let file = "src/forced-http.ts";
    let mut calls = vec![blob_row(
        OperationKey::http("GET", "/v1/item"),
        file,
        LINE,
        Some(ResolutionSource::Model),
    )];
    let fold = fold_one(file, &mut calls, &mut Vec::new());
    assert_eq!(
        at_site(&calls, file),
        vec![(
            OperationKey::http("POST", "/v1/items"),
            Some(ResolutionSource::RequestSummary),
            None
        )]
    );
    assert_eq!(fold.withdrawn.len(), 1);
    assert_eq!(fold.stated.len(), 1);
}

/// A fact the scan already states at the site keeps it: the site row is
/// not stated a second time.
#[test]
fn a_stated_fact_keeps_its_site() {
    let file = "src/forced-http.ts";
    let summary = blob_row(
        OperationKey::http("POST", "/v1/items"),
        file,
        LINE,
        Some(ResolutionSource::RequestSummary),
    );
    let mut calls = vec![summary];
    let fold = fold_one(file, &mut calls, &mut Vec::new());
    assert_eq!(calls.len(), 1);
    assert!(fold.stated.is_empty() && fold.withdrawn.is_empty());
}

/// An answer in the band replaces the reader's row and carries its answer.
#[test]
fn an_answer_in_the_band_replaces_the_readers_row() {
    let file = "src/topic.ts";
    let mut calls = vec![blob_row(
        OperationKey::pubsub("orders"),
        file,
        LINE,
        Some(ResolutionSource::Model),
    )];
    let fold = fold_one(file, &mut calls, &mut Vec::new());
    let site = at_site(&calls, file);
    assert_eq!(site.len(), 1);
    assert_eq!(site[0].0, OperationKey::pubsub("order.settled"));
    assert_eq!(site[0].1, Some(ResolutionSource::Model));
    assert_eq!(
        site[0]
            .2
            .as_ref()
            .and_then(|inference| inference.answer.as_ref())
            .map(|answer| answer.confidence),
        Some(0.9)
    );
    assert_eq!(
        fold.withdrawn,
        vec![(
            file.to_string(),
            LINE,
            OperationKey::pubsub("orders"),
            RowSide::Call
        )]
    );
}

/// Below the band, or with the option orders split, the reader's row stands
/// and keeps the answer as `inference.alternative`.
#[test]
fn below_the_band_the_readers_row_stands_with_the_alternative() {
    for (file, name) in [
        ("src/low.ts", "order.maybe"),
        ("src/disagree.ts", "order.split"),
        ("src/mixed.ts", "order.guessed"),
        ("src/role-split.ts", "order.rolesplit"),
    ] {
        let reader = OperationKey::pubsub("orders");
        let mut calls = vec![blob_row(
            reader.clone(),
            file,
            LINE,
            Some(ResolutionSource::Model),
        )];
        let fold = fold_one(file, &mut calls, &mut Vec::new());
        let site = at_site(&calls, file);
        assert_eq!(site.len(), 1, "{file}");
        assert_eq!(site[0].0, reader);
        let alternative = site[0]
            .2
            .as_ref()
            .and_then(|inference| inference.alternative.as_ref())
            .expect("the answer is kept");
        assert_eq!(alternative.key, OperationKey::pubsub(name));
        assert!(site[0].2.as_ref().unwrap().answer.is_none());
        assert!(fold.stated.is_empty() && fold.leads.is_empty());
    }
}

/// With no row at the site, an answer in the band stands; below it, the
/// site is a possible call.
#[test]
fn with_no_reader_row_the_band_decides_row_or_lead() {
    let mut calls = Vec::new();
    let fold = fold_one("src/topic.ts", &mut calls, &mut Vec::new());
    assert_eq!(at_site(&calls, "src/topic.ts").len(), 1);
    assert_eq!(fold.stated.len(), 1);

    for file in ["src/low.ts", "src/mixed.ts", "src/role-split.ts"] {
        let mut calls = Vec::new();
        let fold = fold_one(file, &mut calls, &mut Vec::new());
        assert!(calls.is_empty(), "{file}");
        assert_eq!(
            lead_reason(&fold.leads, file),
            Some(UnreadReason::BelowBand),
            "{file}"
        );
    }
}

/// A row on the other side of the index is another site: a subscriber
/// beside a publish call is not the publish's reader row.
#[test]
fn the_fold_keeps_to_its_side() {
    let file = "src/topic.ts";
    let mut endpoints = vec![blob_row(
        OperationKey::pubsub("orders"),
        file,
        LINE,
        Some(ResolutionSource::Model),
    )];
    let mut calls = Vec::new();
    fold_one(file, &mut calls, &mut endpoints);
    assert_eq!(endpoints.len(), 1);
    assert_eq!(endpoints[0].inference, None);
    assert_eq!(calls.len(), 1);
}

fn lead_at(file: &str, line: u32, role: Option<&str>) -> PossibleCall {
    PossibleCall {
        file_path: file.to_string(),
        line_number: line,
        span_start: 0,
        span_end: 0,
        kind: None,
        role: role.map(str::to_string),
        labelled_end: None,
        unread: crate::possible_calls::Unread {
            slot: "path".to_string(),
            reason: UnreadReason::NoOpClaim,
        },
        trace_depth: 0,
    }
}

/// A lead on a line the blob already holds a row at is dropped, by side:
/// a consumer lead matches a call row, a producer lead an endpoint row, a
/// lead with no role either (carrick#2539).
#[test]
fn a_lead_on_a_line_with_a_row_is_dropped() {
    let mut calls = vec![blob_row(
        OperationKey::http("GET", "/a"),
        "src/a.ts",
        10,
        None,
    )];
    let mut endpoints = vec![blob_row(
        OperationKey::http("GET", "/b"),
        "src/b.ts",
        5,
        None,
    )];
    let site = SiteRows {
        enabled: true,
        rows: Vec::new(),
        leads: vec![
            lead_at("src/a.ts", 10, None),
            lead_at("src/a.ts", 20, None),
            lead_at("src/b.ts", 5, Some("consumer")),
            lead_at("src/b.ts", 5, Some("producer")),
        ],
        ..SiteRows::default()
    };
    let fold = fold_rows(&mut calls, &mut endpoints, &site, Path::new("."), make);
    let kept: Vec<(&str, u32, Option<&str>)> = fold
        .leads
        .iter()
        .map(|l| (l.file_path.as_str(), l.line_number, l.role.as_deref()))
        .collect();
    assert_eq!(
        kept,
        vec![("src/a.ts", 20, None), ("src/b.ts", 5, Some("consumer"))]
    );
    assert_eq!(fold.lead_on_row_line, 2);
}

/// Off, nothing folds and nothing is written: the rows are byte for byte
/// what they were.
#[test]
fn off_the_fold_changes_nothing() {
    let file = "src/topic.ts";
    let mut calls = vec![blob_row(
        OperationKey::pubsub("orders"),
        file,
        LINE,
        Some(ResolutionSource::Model),
    )];
    let before = serde_json::to_vec(&calls).unwrap();
    let mut off = rows();
    off.enabled = false;
    let fold = fold_rows(&mut calls, &mut Vec::new(), &off, Path::new("."), make);
    assert_eq!(serde_json::to_vec(&calls).unwrap(), before);
    assert!(fold.stated.is_empty() && fold.leads.is_empty() && fold.withdrawn.is_empty());
}

// ---------------------------------------------------------------------------
// The graph (carrick#2795).

fn graph_call(
    method: &str,
    path: &str,
    file: &str,
    source: ResolutionSource,
) -> crate::mount_graph::DataFetchingCall {
    serde_json::from_value(json!({
        "method": method, "target_url": path, "canonical_path": path,
        "client": "fetch", "file_location": format!("{file}:{LINE}"),
        "line": LINE, "resolution_source": source,
    }))
    .unwrap()
}

/// A blob holding `calls` and a mount graph holding `graph`.
fn blob_with_graph(
    calls: Vec<ApiEndpointDetails>,
    graph: Vec<crate::mount_graph::DataFetchingCall>,
) -> CloudRepoData {
    let mut data: CloudRepoData = serde_json::from_value(json!({
        "repo_name": "acme/app", "endpoints": [], "calls": [], "mounts": [],
        "apps": {}, "imported_handlers": [], "function_definitions": {},
        "last_updated": "2026-01-01T00:00:00Z", "commit_hash": "abc1234",
        "mount_graph": { "nodes": {}, "mounts": [], "endpoints": [], "data_calls": [] }
    }))
    .unwrap();
    data.calls = calls;
    data.mount_graph.as_mut().unwrap().data_calls = graph;
    data
}

/// [`rows`] with the forced HTTP send's call made through a function: its
/// labelled call is written elsewhere.
fn rows_through() -> SiteRows {
    let (mut decided, records) = cases();
    for site in &mut decided.sites {
        if site.file == "src/forced-http.ts" {
            site.end.span_start = 1;
            site.end.span_end = 9;
        }
    }
    of(&decided, &records, &labels(), &cassette())
}

fn fold_into(data: &mut CloudRepoData, site: &SiteRows) -> SiteFold {
    fold(data, site, Path::new("."), make)
}

/// A fact the fold states joins the graph in place of the reader's row it
/// replaced, the fact a request the cross-service pass can reach.
#[test]
fn a_fact_replaces_the_readers_graph_row() {
    let file = "src/forced-http.ts";
    let mut data = blob_with_graph(
        vec![blob_row(
            OperationKey::http("GET", "/v1/item"),
            file,
            LINE,
            Some(ResolutionSource::Model),
        )],
        vec![
            graph_call("GET", "/v1/item", file, ResolutionSource::Model),
            // Another site's row stays.
            graph_call("GET", "/other", "src/other.ts", ResolutionSource::Model),
        ],
    );
    let site = rows();
    fold_into(&mut data, &site);
    let graph = &data.mount_graph.as_ref().unwrap().data_calls;
    let at_file: Vec<_> = graph
        .iter()
        .filter(|call| call.file_location == format!("{file}:{LINE}"))
        .collect();
    assert_eq!(at_file.len(), 1, "{at_file:?}");
    let row = at_file[0];
    assert_eq!(
        (row.method.as_str(), row.canonical_path.as_str()),
        ("POST", "/v1/items")
    );
    assert_eq!(row.target_url, "/v1/items");
    assert_eq!(
        row.resolution_source,
        Some(ResolutionSource::RequestSummary)
    );
    assert_eq!(row.line, Some(LINE));
    assert_eq!(row.role, None, "the labelled call is the fact's own");
    assert_eq!(row.reaches_request, None);
    assert!(
        graph.iter().any(|call| call.canonical_path == "/other"),
        "a row at another site stays"
    );
    assert_eq!(graph.len(), 2);
}

/// A fact whose decided call is not its labelled call joins the graph as a
/// call through.
#[test]
fn a_fact_at_a_call_through_a_function_joins_as_a_wrapper_call() {
    let file = "src/forced-http.ts";
    assert!(!row(&rows(), file).unwrap().through);
    assert!(row(&rows_through(), file).unwrap().through);
    let mut data = blob_with_graph(Vec::new(), Vec::new());
    fold_into(&mut data, &rows_through());
    let graph = &data.mount_graph.as_ref().unwrap().data_calls;
    assert_eq!(graph.len(), 1, "{graph:?}");
    assert_eq!(
        graph[0].role,
        Some(crate::mount_graph::ConsumerRole::WrapperCall)
    );
}

/// The graph row of a fact keeps what the reader's row at the same call read
/// about where it goes: the base a relative key resolves through, and the host
/// of an absolute key, read off the key itself.
#[test]
fn a_fact_keeps_the_base_and_host_of_its_call() {
    let file = "src/forced-http.ts";
    let mut reader = graph_call("GET", "${API_URL}/v1/item", file, ResolutionSource::Model);
    reader.base = Some(crate::call_base::CallBaseResolution {
        written: "${process.env.API_URL}".to_string(),
        kind: crate::call_base::CallBaseKind::Env,
        env_var: Some("API_URL".to_string()),
        fallback: None,
        fallback_is_loopback: false,
        declared_optional: None,
    });
    let reader_call = |path: &str| {
        blob_row(
            OperationKey::http("GET", path),
            file,
            LINE,
            Some(ResolutionSource::Model),
        )
    };
    let relative = {
        let mut data = blob_with_graph(
            vec![reader_call("${API_URL}/v1/item")],
            vec![reader.clone()],
        );
        fold_into(&mut data, &rows());
        data.mount_graph.unwrap().data_calls
    };
    assert_eq!(relative.len(), 1, "{relative:?}");
    assert_eq!(
        relative[0]
            .base
            .as_ref()
            .and_then(|base| base.env_var.as_deref()),
        Some("API_URL")
    );
    assert_eq!(relative[0].host, None);

    // The same call keyed by an absolute URL names its host and no base.
    let mut absolute_rows = rows();
    for site_row in &mut absolute_rows.rows {
        if site_row.file == file {
            site_row.key = OperationKey::http("POST", "https://api.vendor.test/v1/items");
        }
    }
    let mut data = blob_with_graph(vec![reader_call("${API_URL}/v1/item")], vec![reader]);
    fold_into(&mut data, &absolute_rows);
    let graph = data.mount_graph.unwrap().data_calls;
    assert_eq!(graph.len(), 1, "{graph:?}");
    assert_eq!(graph[0].host.as_deref(), Some("api.vendor.test"));
    assert_eq!(graph[0].base, None);
}

/// The graph row of a fact keeps the reader's client and dispatch, as it
/// keeps the host and base: they describe the call as written.
#[test]
fn a_fact_keeps_the_client_and_dispatch_of_its_call() {
    let file = "src/forced-http.ts";
    let mut reader = graph_call("GET", "/v1/item", file, ResolutionSource::Model);
    reader.client = "axios".to_string();
    let dispatch = crate::dispatch::Dispatch {
        location: crate::dispatch::DispatchLocation::Body,
        field: "action".to_string(),
        value: "create".to_string(),
    };
    reader.dispatch = Some(dispatch.clone());
    let mut data = blob_with_graph(
        vec![blob_row(
            OperationKey::http("GET", "/v1/item"),
            file,
            LINE,
            Some(ResolutionSource::Model),
        )],
        vec![reader],
    );
    fold_into(&mut data, &rows());
    let graph = data.mount_graph.unwrap().data_calls;
    assert_eq!(graph.len(), 1, "{graph:?}");
    assert_eq!(graph[0].client, "axios");
    assert_eq!(graph[0].dispatch, Some(dispatch));
}

/// A candidate replacement (an answer in the band) leaves the graph as it
/// was: the reader's row stays, and nothing is added.
#[test]
fn a_candidate_leaves_the_graph_alone() {
    let file = "src/http.ts";
    let all = rows();
    assert!(row(&all, file).unwrap().inference.is_some(), "a candidate");
    let site = SiteRows {
        enabled: true,
        rows: all
            .rows
            .into_iter()
            .filter(|row| row.file == file)
            .collect(),
        ..SiteRows::default()
    };
    let reader = |source| graph_call("GET", "/v1/read", file, source);
    let mut data = blob_with_graph(
        vec![blob_row(
            OperationKey::http("GET", "/v1/read"),
            file,
            LINE,
            Some(ResolutionSource::Model),
        )],
        vec![reader(ResolutionSource::Model)],
    );
    let before = serde_json::to_vec(&data.mount_graph).unwrap();
    fold_into(&mut data, &site);
    assert_eq!(serde_json::to_vec(&data.mount_graph).unwrap(), before);
}

/// Off, the graph is byte for byte what it was.
#[test]
fn off_the_fold_leaves_the_graph_untouched() {
    let file = "src/forced-http.ts";
    let mut data = blob_with_graph(
        Vec::new(),
        vec![graph_call("GET", "/v1/item", file, ResolutionSource::Model)],
    );
    let before = serde_json::to_vec(&data.mount_graph).unwrap();
    let mut off = rows();
    off.enabled = false;
    fold_into(&mut data, &off);
    assert_eq!(serde_json::to_vec(&data.mount_graph).unwrap(), before);
}

// ---------------------------------------------------------------------------
// The wire.

/// `inference` rides the wire on the rows that carry it, round-trips, and
/// is absent everywhere else: a row with none serialises without the key,
/// and a blob row written before the field reads as none.
#[test]
fn inference_on_the_wire() {
    let answer = SiteInference {
        by: InferenceBy::SiteQuestion,
        question_set: Some("qs-fixture".to_string()),
        confidence: 0.91,
        orders_agree: true,
    };
    let mut stated = blob_row(OperationKey::pubsub("a"), "src/a.ts", 1, None);
    stated.inference = Some(Inference {
        answer: Some(answer.clone()),
        alternative: None,
    });
    let value = serde_json::to_value(&stated).unwrap();
    assert_eq!(
        value["inference"],
        json!({ "by": "site_question", "question_set": "qs-fixture", "confidence": 0.91, "orders_agree": true })
    );
    let back: ApiEndpointDetails = serde_json::from_value(value).unwrap();
    assert_eq!(back.inference, stated.inference);

    let mut reader = blob_row(OperationKey::pubsub("a"), "src/a.ts", 1, None);
    reader.inference = Some(Inference {
        answer: None,
        alternative: Some(Alternative {
            answer,
            side: RowSide::Call,
            key: OperationKey::pubsub("b"),
        }),
    });
    let value = serde_json::to_value(&reader).unwrap();
    assert_eq!(
        value["inference"],
        json!({ "alternative": {
            "by": "site_question", "question_set": "qs-fixture", "confidence": 0.91,
            "orders_agree": true, "side": "call", "protocol": "pubsub", "topic": "b"
        } })
    );
    let back: ApiEndpointDetails = serde_json::from_value(value).unwrap();
    assert_eq!(back.inference, reader.inference);

    let plain = blob_row(OperationKey::pubsub("a"), "src/a.ts", 1, None);
    let value = serde_json::to_value(&plain).unwrap();
    assert!(value.get("inference").is_none());
    let old: ApiEndpointDetails = serde_json::from_value(value).unwrap();
    assert_eq!(old.inference, None);
}

// ---------------------------------------------------------------------------
// Routes a site cannot write (carrick#2517).

/// A made-up server framework's member at `file`, labelled `receive`.
fn route_site(file: &str, member: &str) -> DecidedSite {
    let mut site = site(file, "server_framework", "receive", "App");
    site.end.package = "@fixture/serve".to_string();
    site.end.member = Some(member.to_string());
    site
}

/// The record of a route site as the label states it: kind `http` and role
/// `handles` forced, the name forced, and the method `forced` or asked.
fn route_record(site: &DecidedSite, method: Option<&str>) -> SiteRecord {
    let mut forced = vec![
        (Slot::Kind, "http"),
        (Slot::Role, "handles"),
        (Slot::Name, "/items/:id"),
    ];
    let asked: &[Slot] = match method {
        Some(method) => {
            forced.push((Slot::Method, method));
            &[]
        }
        None => &[Slot::Method],
    };
    record(site, &forced, asked)
}

/// A reader's route at `file:LINE`, keyed with the prefix its router gives it.
fn reader_route(file: &str) -> ApiEndpointDetails {
    blob_row(
        OperationKey::http("GET", "/v1/items/:id"),
        file,
        LINE,
        Some(ResolutionSource::Model),
    )
}

/// One record's rows, folded against `endpoints`.
fn fold_route(
    site: &DecidedSite,
    record: SiteRecord,
    answers: &SiteAnswers,
    endpoints: &mut Vec<ApiEndpointDetails>,
) -> SiteFold {
    let decided = Decided {
        sites: vec![site.clone()],
        ..Decided::default()
    };
    let rows = of(&decided, &[record], &[], answers);
    assert!(rows.rows.is_empty(), "{:?}", rows.rows);
    fold_rows(&mut Vec::new(), endpoints, &rows, Path::new("."), make)
}

/// A route site the label states (kind `http`, role `handles`) is never
/// asked: no route can be written from its answer, because the site names no
/// router and so no prefix. Where the reader stated a route at its line the
/// reader's row stands untouched, the site is counted, and it is no lead.
#[test]
fn a_route_site_with_a_reader_row_is_not_asked_and_not_a_lead() {
    let file = "src/sub-app.ts";
    let site = route_site(file, "get");
    for method in [None, Some("GET")] {
        let record = route_record(&site, method);
        assert!(
            questions(std::slice::from_ref(&record)).is_empty(),
            "{method:?}"
        );
        let before = serde_json::to_string(&vec![reader_route(file)]).unwrap();
        let mut endpoints = vec![reader_route(file)];
        let fold = fold_route(&site, record, &SiteAnswers::default(), &mut endpoints);
        assert_eq!(serde_json::to_string(&endpoints).unwrap(), before);
        assert!(fold.leads.is_empty(), "{method:?}: {:?}", fold.leads);
        assert_eq!(fold.route_kept_reader, 1);
        assert!(fold.stated.is_empty() && fold.withdrawn.is_empty());
    }
}

/// The same route site with no reader row at its line is a call the index
/// lacks: one possible call, reason `owner_unknown`, and still no question.
#[test]
fn a_route_site_with_no_reader_row_is_one_owner_unknown_lead() {
    let file = "src/mount.ts";
    let site = route_site(file, "attach");
    for method in [None, Some("GET")] {
        let record = route_record(&site, method);
        assert!(
            questions(std::slice::from_ref(&record)).is_empty(),
            "{method:?}"
        );
        let mut endpoints = Vec::new();
        let fold = fold_route(&site, record, &SiteAnswers::default(), &mut endpoints);
        assert!(endpoints.is_empty());
        assert_eq!(fold.leads.len(), 1, "{method:?}: {:?}", fold.leads);
        let lead = &fold.leads[0];
        assert_eq!(lead.unread.reason, UnreadReason::OwnerUnknown);
        assert_eq!(lead.kind.as_deref(), Some("http"));
        assert_eq!(lead.role.as_deref(), Some("producer"));
        assert_eq!(fold.route_kept_reader, 0);
    }
}

/// A site whose role the label leaves open is asked. When the model answers
/// that it registers an HTTP route and names no method (or a word no route is
/// registered under), no endpoint is written: with no reader row at the
/// line it is a possible call `no_method`, and with one it is counted.
#[test]
fn an_asked_route_answer_with_no_method_writes_no_endpoint() {
    let file = "src/open.ts";
    let site = route_site(file, "use");
    let record = record(&site, &[(Slot::Kind, "http")], &[Slot::Role, Slot::Name]);
    assert_eq!(questions(std::slice::from_ref(&record)).len(), 1);
    for method in [None, Some("UNKNOWN"), Some("")] {
        let mut answers = SiteAnswers::default();
        let sure = |text: &str| SlotAnswer {
            value: Some(SlotValue::Text(text.to_string())),
            confidence: 1.0,
            orders_agree: true,
        };
        answers.answered.insert(
            site_id(&record.anchors),
            Answered {
                role: Some(sure("handles")),
                name: Some(sure("/items/:id")),
                method: method.map(sure),
                ..Answered::default()
            },
        );
        let mut endpoints = Vec::new();
        let fold = fold_route(&site, record.clone(), &answers, &mut endpoints);
        assert!(endpoints.is_empty(), "{method:?}");
        assert_eq!(
            lead_reason(&fold.leads, file),
            Some(UnreadReason::NoMethod),
            "{method:?}"
        );
        let mut endpoints = vec![reader_route(file)];
        let fold = fold_route(&site, record.clone(), &answers, &mut endpoints);
        assert!(fold.leads.is_empty(), "{method:?}: {:?}", fold.leads);
        assert_eq!(fold.route_kept_reader, 1);
    }
}

/// Receive-side rows with no prefix to lose are written as before: a
/// subscriber whose kind, role and name are forced.
#[test]
fn a_receive_row_with_no_prefix_is_still_written() {
    let rows = rows();
    let topic = row(&rows, "src/forced-topic.ts").expect("the subscriber is a row");
    assert_eq!(topic.side, RowSide::Endpoint);
    assert_eq!(topic.key, OperationKey::pubsub("item.added"));
}

/// Off, the fold leaves a route table byte for byte as it was, and states no
/// possible call and no count.
#[test]
fn off_a_route_site_leaves_the_route_table_byte_identical() {
    let file = "src/sub-app.ts";
    let site = route_site(file, "get");
    let decided = Decided {
        sites: vec![site.clone()],
        ..Decided::default()
    };
    for endpoints in [vec![reader_route(file)], Vec::new()] {
        let mut rows = of(
            &decided,
            &[route_record(&site, Some("GET"))],
            &[],
            &SiteAnswers::default(),
        );
        rows.enabled = false;
        let mut endpoints = endpoints;
        let before = serde_json::to_string(&endpoints).unwrap();
        let fold = fold_rows(&mut Vec::new(), &mut endpoints, &rows, Path::new("."), make);
        assert_eq!(serde_json::to_string(&endpoints).unwrap(), before);
        assert!(fold.leads.is_empty());
        assert_eq!(fold.route_kept_reader, 0);
    }
}

// ---------------------------------------------------------------------------
// Corroboration (carrick#2520): a row on a label-only slot with a name the
// model answered stands only beside a reader row that agrees on its key.

/// Label forces kind, role and method; the model answers the name; no
/// reader row stands at the line: no row, and the site is a possible call,
/// `uncorroborated`.
#[test]
fn a_label_only_row_with_a_model_name_and_no_reader_is_uncorroborated() {
    let mut calls = Vec::new();
    let fold = fold_one("src/http.ts", &mut calls, &mut Vec::new());
    assert!(at_site(&calls, "src/http.ts").is_empty(), "{calls:?}");
    assert!(fold.stated.is_empty());
    assert_eq!(
        lead_reason(&fold.leads, "src/http.ts"),
        Some(UnreadReason::Uncorroborated)
    );
}

/// The same labels with a name the code states: a row, no reader needed.
#[test]
fn a_code_stated_name_needs_no_reader() {
    let rows = rows();
    assert!(!row(&rows, "src/forced-http.ts").unwrap().needs_reader);
    let mut calls = Vec::new();
    let fold = fold_one("src/forced-http.ts", &mut calls, &mut Vec::new());
    assert_eq!(
        at_site(&calls, "src/forced-http.ts"),
        vec![(
            OperationKey::http("POST", "/v1/items"),
            Some(ResolutionSource::RequestSummary),
            None
        )]
    );
    assert_eq!(lead_reason(&fold.leads, "src/forced-http.ts"), None);
}

/// A reader row at the line agreeing on the key corroborates the row: the
/// fold keeps one row there. A reader row naming another operation does
/// not: the reader stands and the site is `uncorroborated`.
#[test]
fn an_agreeing_reader_row_corroborates() {
    let file = "src/http.ts";
    let reader = |path: &str| {
        blob_row(
            OperationKey::http("GET", path),
            file,
            LINE,
            Some(ResolutionSource::Model),
        )
    };
    let mut calls = vec![reader("/v1/orders")];
    let fold = fold_one(file, &mut calls, &mut Vec::new());
    let here = at_site(&calls, file);
    assert_eq!(here.len(), 1, "{here:?}");
    assert_eq!(here[0].0, OperationKey::http("GET", "/v1/orders"));
    assert_eq!(lead_reason(&fold.leads, file), None);

    let mut calls = vec![reader("/v2/invoices")];
    let fold = fold_one(file, &mut calls, &mut Vec::new());
    assert_eq!(
        at_site(&calls, file),
        vec![(
            OperationKey::http("GET", "/v2/invoices"),
            Some(ResolutionSource::Model),
            None
        )]
    );
    assert_eq!(
        lead_reason(&fold.leads, file),
        Some(UnreadReason::Uncorroborated)
    );
}

/// Agreement reads a base written as a value and a segment written as a
/// value as the reader's parameter: `${base}/tasks/${id}` agrees with
/// `/tasks/:taskId`; another method, or no literal segment in common, does
/// not.
#[test]
fn agreement_reads_written_values_as_parameters() {
    let site = OperationKey::http("PUT", "${process.env.API}/tasks/${taskId}/status");
    assert!(agrees(
        &OperationKey::http("PUT", "/tasks/:id/status"),
        &site
    ));
    assert!(!agrees(
        &OperationKey::http("GET", "/tasks/:id/status"),
        &site
    ));
    assert!(!agrees(
        &OperationKey::http("PUT", "/files/:id/status2"),
        &site
    ));
    assert!(agrees(
        &OperationKey::pubsub("order.settled"),
        &OperationKey::pubsub("order.settled")
    ));
    assert!(!agrees(
        &OperationKey::pubsub("order.settled"),
        &OperationKey::pubsub("order.placed")
    ));
}

/// carrick#2704: where the caller's line holds a second call, a disagreeing
/// reader row there may be that call's, so only the agreeing row folds.
#[test]
fn a_second_call_on_the_callers_line_keeps_its_row() {
    let model = Some(ResolutionSource::Model);
    let mut calls = vec![
        blob_row(
            OperationKey::http("POST", "/api/login"),
            "src/client.ts",
            12,
            Some(ResolutionSource::RequestSummary),
        ),
        blob_row(
            OperationKey::http("POST", "/api/login"),
            "src/page.ts",
            4,
            model,
        ),
        blob_row(
            OperationKey::http("GET", "/api/user"),
            "src/page.ts",
            4,
            model,
        ),
    ];
    let site = SiteRows {
        enabled: true,
        through: vec![Through {
            file: "src/page.ts".to_string(),
            line: 4,
            site_file: "src/client.ts".to_string(),
            site_line: 12,
            other_call: true,
        }],
        ..SiteRows::default()
    };
    fold_rows(&mut calls, &mut Vec::new(), &site, Path::new("."), make);
    let left: Vec<String> = calls.iter().map(|row| format!("{:?}", row.key)).collect();
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(left[1].contains("/api/user"));
}

/// carrick#2614: a reader row at a call above a decided site folds onto the
/// row standing at the site when the two agree. carrick#2704: it folds as
/// well when it disagrees with a fact there, and stands when it disagrees
/// with the reader's own row.
#[test]
fn a_reader_row_above_a_decided_site_folds_onto_the_sites_row() {
    let through = |file: &str, line: u32, site_line: u32| Through {
        file: file.to_string(),
        line,
        site_file: "src/client.ts".to_string(),
        site_line,
        other_call: false,
    };
    let model = Some(ResolutionSource::Model);
    let mut calls = vec![
        // The site rows: one the scan states, one only the reader states.
        blob_row(
            OperationKey::http("POST", "/api/login"),
            "src/client.ts",
            12,
            Some(ResolutionSource::RequestSummary),
        ),
        blob_row(
            OperationKey::http("GET", "/api/me"),
            "src/client.ts",
            20,
            model,
        ),
        // Folds: the same operation as the scan's site row.
        blob_row(
            OperationKey::http("POST", "/api/login"),
            "src/page.ts",
            4,
            model,
        ),
        // Folds: the same operation as the reader's site row.
        blob_row(
            OperationKey::http("GET", "/api/me"),
            "src/page.ts",
            6,
            model,
        ),
        // Stands: another operation than the site states, where only the
        // reader states it.
        blob_row(
            OperationKey::http("GET", "/api/other"),
            "src/page.ts",
            7,
            model,
        ),
        // Folds: another operation than the site states, where a fact states
        // it (carrick#2704).
        blob_row(
            OperationKey::http("DELETE", "/api/session"),
            "src/page.ts",
            9,
            model,
        ),
        // Stands: no row at its site.
        blob_row(
            OperationKey::http("POST", "/api/audit"),
            "src/page.ts",
            11,
            model,
        ),
        // Stands: the scan states it, not the reader.
        blob_row(
            OperationKey::http("POST", "/api/login"),
            "src/page.ts",
            13,
            Some(ResolutionSource::RequestSummary),
        ),
        // Stands: no call above a site is recorded at its line.
        blob_row(
            OperationKey::http("POST", "/api/login"),
            "src/page.ts",
            15,
            model,
        ),
    ];
    let site = SiteRows {
        enabled: true,
        through: vec![
            through("src/page.ts", 4, 12),
            through("src/page.ts", 6, 20),
            through("src/page.ts", 7, 20),
            through("src/page.ts", 9, 12),
            through("src/page.ts", 11, 30),
            through("src/page.ts", 13, 12),
        ],
        ..SiteRows::default()
    };
    let fold = fold_rows(&mut calls, &mut Vec::new(), &site, Path::new("."), make);
    let left: Vec<String> = calls
        .iter()
        .map(|row| row.file_path.to_string_lossy().to_string())
        .collect();
    assert_eq!(
        left,
        [
            "src/client.ts:12",
            "src/client.ts:20",
            "src/page.ts:7",
            "src/page.ts:11",
            "src/page.ts:13",
            "src/page.ts:15",
        ]
    );
    assert_eq!(fold.through_folded.len(), 3);
    assert_eq!(
        fold.withdrawn,
        [
            (
                "src/page.ts".to_string(),
                4,
                OperationKey::http("POST", "/api/login"),
                RowSide::Call
            ),
            (
                "src/page.ts".to_string(),
                6,
                OperationKey::http("GET", "/api/me"),
                RowSide::Call
            ),
            (
                "src/page.ts".to_string(),
                9,
                OperationKey::http("DELETE", "/api/session"),
                RowSide::Call
            ),
        ]
    );

    // Off, nothing folds.
    let mut before = calls.clone();
    before.push(blob_row(
        OperationKey::http("POST", "/api/login"),
        "src/page.ts",
        4,
        model,
    ));
    let mut after = before.clone();
    let off = SiteRows {
        enabled: false,
        ..site
    };
    fold_rows(&mut after, &mut Vec::new(), &off, Path::new("."), make);
    assert_eq!(after.len(), before.len());
}

// ---------------------------------------------------------------------------
// Routes the receiver trace serves at a full path (carrick#2474 R3 S1).

/// A route site in `file` at span `start..start + 30` whose instance ends
/// `ends`, each with `path` composed, and whose own path is `/items/:id`.
fn traced_site(file: &str, start: u32, ends: &[(&str, Option<&str>)]) -> DecidedSite {
    let mut site = route_site(file, "get");
    site.span_start = start;
    site.span_end = start + 30;
    site.name = vec![crate::request_summary::Piece::Lit("/items/:id".to_string())];
    site.receiver = Some(crate::request_summary::discovery::ReceiverAt {
        symbol: Some("items".to_string()),
        decl_file: None,
        decl_line: None,
        kind: crate::request_summary::discovery::ReceiverKind::Binding,
    });
    site.composed = ends
        .iter()
        .map(
            |(end, path)| crate::request_summary::decided_sites::Composed {
                path: path.map(str::to_string),
                edges: Vec::new(),
                end: end.to_string(),
            },
        )
        .collect();
    site
}

/// Each reason a route site is left to the reader, counted once, beside
/// the one route the trace serves (PR #2687 review).
#[test]
fn every_route_the_trace_does_not_serve_is_counted_by_why() {
    let served = traced_site("src/served.ts", 100, &[("top", Some("/v1/items/:id"))]);
    let unstated = traced_site("src/unstated.ts", 100, &[("top", Some("/v1/items/:id"))]);
    let trimmed = DecidedSite {
        composed_trimmed: true,
        ..traced_site("src/trimmed.ts", 100, &[("top", Some("/v1/items/:id"))])
    };
    let no_instance = traced_site("src/none.ts", 100, &[]);
    let unverified = traced_site("src/mounted.ts", 100, &[("mount_unverified", None)]);
    let hole = traced_site(
        "src/hole.ts",
        100,
        &[("top", Some("/v1/items/:id")), ("entry_point", None)],
    );
    let unread = traced_site("src/unread.ts", 100, &[("top", None)]);
    let unnamed = DecidedSite {
        name: Vec::new(),
        ..traced_site("src/unnamed.ts", 100, &[("top", Some("/v1/items/:id"))])
    };
    let sites = [
        &served,
        &unstated,
        &trimmed,
        &no_instance,
        &unverified,
        &hole,
        &unread,
        &unnamed,
    ];
    let records: Vec<SiteRecord> = sites
        .iter()
        .map(|site| {
            let method = (site.file != "src/unstated.ts").then_some("get");
            route_record(site, method)
        })
        .collect();
    let decided = Decided {
        sites: sites.iter().map(|site| (*site).clone()).collect(),
        ..Decided::default()
    };
    let routes = traced(&decided, &records);
    assert_eq!(
        routes.routes,
        vec![TracedRoute {
            file: "src/served.ts".to_string(),
            line: LINE,
            method: "GET".to_string(),
            path: "/items/:id".to_string(),
            full_paths: vec!["/v1/items/:id".to_string()],
            handler: None,
            owner: Some("items".to_string()),
            corrects: true,
        }]
    );
    assert_eq!(
        routes.untraced,
        BTreeMap::from([
            ("hole_entry_point".to_string(), 1),
            ("method_unstated".to_string(), 1),
            ("mount_unverified".to_string(), 1),
            ("no_instance".to_string(), 1),
            ("trimmed".to_string(), 1),
            ("unread_prefix".to_string(), 2),
        ])
    );
}
