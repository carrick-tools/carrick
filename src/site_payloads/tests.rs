//! The join from a manifest entry to its site's payload slot, and the
//! hand-over of an alias's infer requests to the slot.

use std::path::Path;

use serde_json::json;

use super::*;
use crate::request_summary::site_record::{AnchorAt, Bucket, HandlerAnchor, TypeAnchors};
use crate::result_shapes::ResultIndex;
use crate::site_questions::SiteState;

const FILE: &str = "src/api.ts";
/// Line 2 of [`SOURCE`] holds the call; a multi-byte character on line 1
/// puts the scanner's byte spans and the sidecar's UTF-16 positions apart.
const SOURCE: &str = "// prices in € only\nexport const sent = http.post(\"/orders\", order);\n";

fn span_of(needle: &str) -> (u32, u32) {
    let start = SOURCE.find(needle).unwrap() as u32 + crate::swc_scanner::SWC_SPAN_BASE;
    (start, start + needle.len() as u32)
}

fn at(needle: &str) -> AnchorAt {
    let (span_start, span_end) = span_of(needle);
    AnchorAt {
        file: FILE.to_string(),
        line: 2,
        span_start,
        span_end,
    }
}

/// A record at line 2 of [`FILE`] for a site of a package labelled `role`,
/// a handler's when `handler` is set, carrying `payloads`.
fn record(role: &str, handler: bool, payloads: PayloadSlots) -> SiteRecord {
    let state: SiteState = serde_json::from_value(json!({
        "point": { "at": format!("{FILE}:2"), "code": "" },
        "compiler": { "names": [], "callees": [], "enclosing": null, "reached": [] },
        "syntax": [],
        "libraries": [{
            "package": "@fixture/http",
            "labels": [{ "export": "default", "role": role, "side": null }],
            "argument_labels": [],
            "in_store": true,
        }],
        "call": null,
        "stated": { "names": [], "methods": [] },
        "asked": [],
        "forced": {},
    }))
    .unwrap();
    let call = at("http.post(\"/orders\", order)");
    SiteRecord {
        state,
        anchors: TypeAnchors {
            call: call.clone(),
            payload: None,
            payload_at: None,
            handler: handler.then(|| HandlerAnchor {
                at: call,
                param: 0,
                param_name: Some("order".to_string()),
            }),
            key: None,
        },
        payloads,
        basis: Default::default(),
        bucket: Bucket::Forced,
        lead: None,
        receiver: None,
    }
}

fn call_slots() -> PayloadSlots {
    PayloadSlots {
        request: Some(PayloadAt::Value(at("order)").trimmed())),
        response: Some(PayloadAt::Result(at("http.post(\"/orders\", order)"))),
    }
}

trait Trim {
    fn trimmed(self) -> Self;
}

impl Trim for AnchorAt {
    /// The anchor without its last byte: `order)` -> `order`.
    fn trimmed(mut self) -> Self {
        self.span_end -= 1;
        self
    }
}

/// A manifest entry as the reader's call or route writes it, at `line`.
fn entry(
    key: serde_json::Value,
    role: &str,
    kind: &str,
    alias: &str,
    line: u32,
) -> TypeManifestEntry {
    let mut value = json!({
        "role": role,
        "type_kind": kind,
        "type_alias": alias,
        "file_path": FILE,
        "line_number": line,
        "is_explicit": false,
        "type_state": "unknown",
        "evidence": {
            "file_path": FILE,
            "line_number": line,
            "infer_kind": "call_result",
            "is_explicit": false,
            "type_state": "unknown",
        },
    });
    for (field, text) in key.as_object().unwrap() {
        value[field] = text.clone();
    }
    serde_json::from_value(value).unwrap()
}

fn http() -> serde_json::Value {
    json!({ "protocol": "http", "method": "POST", "path": "/orders" })
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join(FILE), SOURCE).unwrap();
    dir
}

/// Where `needle` starts and ends in the sidecar's numbering: UTF-16 units
/// from zero.
fn sidecar_span(needle: &str) -> (Option<u32>, Option<u32>) {
    let start = SOURCE.find(needle).unwrap();
    let units = |bytes: usize| SOURCE[..bytes].encode_utf16().count() as u32;
    (Some(units(start)), Some(units(start + needle.len())))
}

/// An HTTP call's request entry reads its body argument and its response
/// entry its result, at spans in the sidecar's numbering. The reader states
/// no locator for either (carrick-cloud#2522's shape), so these are the
/// only requests the aliases get.
#[test]
fn an_http_calls_entries_read_its_body_and_its_result() {
    let dir = repo();
    let payloads = SitePayloads::of(&[record("http_client", false, call_slots())]);
    let entries = [
        entry(http(), "consumer", "request", "Req", 2),
        entry(http(), "consumer", "response", "Res", 2),
    ];
    let requests = payloads.requests(
        &entries,
        &dir.path().to_string_lossy(),
        &ResultIndex::default(),
    );
    // The body argument: `order` before the call's closing parenthesis.
    let body = {
        let (start, end) = sidecar_span("order)");
        (start, end.map(|end| end - 1))
    };
    let read: Vec<_> = requests
        .iter()
        .map(|request| {
            (
                request.alias.as_deref().unwrap(),
                request.infer_kind.clone(),
                (request.span_start, request.span_end),
                request.line_number,
            )
        })
        .collect();
    assert_eq!(
        read,
        vec![
            ("Req", InferKind::RequestBody, body, 2),
            (
                "Res",
                InferKind::CallResult,
                sidecar_span("http.post(\"/orders\", order)"),
                2
            ),
        ]
    );
    assert!(
        requests
            .iter()
            .all(|request| request.file_path == dir.path().join(FILE).to_string_lossy()),
        "{requests:?}"
    );
}

/// carrick#2910: the response entry's request carries the store's label for
/// the call it reads, found by the call's span under the repo-relative file;
/// the request entry and an index with no label at the call carry none, and
/// the labelled request is the unlabelled one with the label.
#[test]
fn a_calls_result_request_takes_the_label_at_its_call() {
    use crate::services::type_sidecar::ResultShape;
    let dir = repo();
    let root = dir.path().to_string_lossy().into_owned();
    let payloads = SitePayloads::of(&[record("http_client", false, call_slots())]);
    let entries = [
        entry(http(), "consumer", "request", "Req", 2),
        entry(http(), "consumer", "response", "Res", 2),
    ];
    let member = ResultShape::Member(["data".to_string()]);
    let (start, end) = span_of("http.post(\"/orders\", order)");
    let index = ResultIndex::labelled(FILE, (start, end), member.clone());
    let labelled = payloads.requests(&entries, &root, &index);
    let bare = payloads.requests(&entries, &root, &ResultIndex::default());
    assert_eq!(labelled.len(), 2, "{labelled:?}");
    let by_alias = |requests: &[InferRequestItem], alias: &str| {
        requests
            .iter()
            .find(|request| request.alias.as_deref() == Some(alias))
            .cloned()
            .unwrap()
    };
    assert_eq!(by_alias(&labelled, "Res").result, Some(member));
    assert_eq!(by_alias(&labelled, "Req").result, None);
    assert!(bare.iter().all(|request| request.result.is_none()));
    let mut unlabelled = by_alias(&labelled, "Res");
    unlabelled.result = None;
    assert_eq!(
        serde_json::to_value(&unlabelled).unwrap(),
        serde_json::to_value(by_alias(&bare, "Res")).unwrap()
    );
    // The same span in another file names another call.
    let elsewhere = ResultIndex::labelled("src/other.ts", (start, end), ResultShape::Whole(true));
    assert!(
        payloads
            .requests(&entries, &root, &elsewhere)
            .iter()
            .all(|request| request.result.is_none())
    );
}

/// An entry joins only a site on its line, of its protocol and on its side;
/// two sites that could both answer leave it to the reader.
#[test]
fn an_entry_joins_one_site_of_its_line_protocol_and_side() {
    let dir = repo();
    let root = dir.path().to_string_lossy().into_owned();
    let http_call = SitePayloads::of(&[record("http_client", false, call_slots())]);
    // Another line.
    assert!(
        http_call
            .requests(
                &[entry(http(), "consumer", "response", "Res", 3)],
                &root,
                &ResultIndex::default()
            )
            .is_empty()
    );
    // Another protocol.
    let socket =
        json!({ "protocol": "socket", "event": "orders", "direction": "client_to_server" });
    let socket_entry = entry(socket, "consumer", "response", "Sock", 2);
    assert!(
        http_call
            .requests(&[socket_entry], &root, &ResultIndex::default())
            .is_empty()
    );
    // Another side: a route at the line of a client's call.
    assert!(
        http_call
            .requests(
                &[entry(http(), "producer", "request", "Route", 2)],
                &root,
                &ResultIndex::default()
            )
            .is_empty()
    );
    // Two calls on the line.
    let two = SitePayloads::of(&[
        record("http_client", false, call_slots()),
        record("http_client", false, call_slots()),
    ]);
    assert!(
        two.requests(
            &[entry(http(), "consumer", "response", "Res", 2)],
            &root,
            &ResultIndex::default()
        )
        .is_empty()
    );
}

/// A handler's one payload is its parameter, read by name; an empty slot
/// asks nothing.
#[test]
fn a_handlers_payload_is_its_parameter_and_an_empty_slot_asks_nothing() {
    let dir = repo();
    let root = dir.path().to_string_lossy().into_owned();
    let param = PayloadSlots {
        request: Some(PayloadAt::Param {
            at: at("http.post(\"/orders\", order)"),
            name: "order".to_string(),
        }),
        response: None,
    };
    let socket =
        json!({ "protocol": "socket", "event": "orders", "direction": "client_to_server" });
    let handler = SitePayloads::of(&[record("socket", true, param)]);
    let requests = handler.requests(
        &[entry(socket.clone(), "producer", "response", "Sub", 2)],
        &root,
        &ResultIndex::default(),
    );
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].infer_kind, InferKind::FunctionParam);
    assert_eq!(requests[0].param_name.as_deref(), Some("order"));

    let empty = SitePayloads::of(&[record("http_client", false, PayloadSlots::default())]);
    assert!(
        empty
            .requests(
                &[entry(http(), "consumer", "request", "Req", 2)],
                &root,
                &ResultIndex::default()
            )
            .is_empty()
    );
}

fn item(alias: &str, line: u32) -> InferRequestItem {
    InferRequestItem {
        member: None,
        result: None,
        file_path: FILE.to_string(),
        line_number: line,
        span_start: None,
        span_end: None,
        expression_text: Some("x".to_string()),
        expression_line: None,
        infer_kind: InferKind::CallResult,
        alias: Some(alias.to_string()),
        param_name: None,
    }
}

/// The slot's request replaces every request the reader's locator gave the
/// same alias; other aliases keep theirs, and no slot changes nothing.
#[test]
fn a_slot_takes_over_its_alias_and_nothing_else() {
    let reader = vec![item("Res", 2), item("Other", 9), item("Res", 2)];
    let slotted = vec![item("Res", 40)];

    let mut infer = reader.clone();
    take_over(&mut infer, &slotted);
    let read: Vec<_> = infer
        .iter()
        .map(|request| (request.alias.as_deref().unwrap(), request.line_number))
        .collect();
    assert_eq!(read, vec![("Other", 9), ("Res", 40)]);

    let mut unchanged = reader.clone();
    take_over(&mut unchanged, &[]);
    let lines = |items: &[InferRequestItem]| -> Vec<(Option<String>, u32)> {
        items
            .iter()
            .map(|request| (request.alias.clone(), request.line_number))
            .collect()
    };
    assert_eq!(lines(&unchanged), lines(&reader));
}

#[test]
fn the_switch_is_on_only_at_one() {
    assert!(enabled_from(Some("1")));
    for off in [None, Some("0"), Some("true"), Some(""), Some("yes")] {
        assert!(!enabled_from(off), "{off:?}");
    }
}

/// The seam: every infer kind this module sends is a kind the sidecar's
/// request type declares, spelled as the scanner writes it.
#[test]
fn every_kind_sent_is_one_the_sidecar_declares() {
    let types = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/src/types.ts"),
    )
    .unwrap();
    let union = types
        .split("export type InferKind =")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("the sidecar declares InferKind");
    for kind in [
        InferKind::RequestBody,
        InferKind::Expression,
        InferKind::CallResult,
        InferKind::FunctionParam,
        InferKind::RequestConfig,
    ] {
        let spelled = serde_json::to_value(kind).unwrap();
        let spelled = spelled.as_str().unwrap();
        assert!(
            union.contains(&format!("'{spelled}'")),
            "{spelled} is not in the sidecar's InferKind"
        );
    }
}

/// carrick#2674 P3-1a: an options literal that states the body key absent
/// asks the sidecar the `request_config` question at the literal, with the
/// key as `member`. Every other request carries no member, so it serialises
/// as it did.
#[test]
fn an_unwritten_slot_asks_the_request_config_question_with_its_key() {
    let dir = repo();
    let root = dir.path().to_string_lossy().into_owned();
    let literal = PayloadSlots {
        request: Some(PayloadAt::Unwritten {
            at: at("\"/orders\""),
            key: "body".to_string(),
        }),
        response: None,
    };
    let requests = SitePayloads::of(&[record("http_client", false, literal)]).requests(
        &[entry(http(), "consumer", "request", "Req", 2)],
        &root,
        &ResultIndex::default(),
    );
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].infer_kind, InferKind::RequestConfig);
    assert_eq!(requests[0].member.as_deref(), Some("body"));
    assert_eq!(requests[0].alias.as_deref(), Some("Req"));
    let wire = serde_json::to_value(&requests[0]).unwrap();
    assert_eq!(wire["infer_kind"], "request_config");
    assert_eq!(wire["member"], "body");

    let value = SitePayloads::of(&[record("http_client", false, call_slots())]).requests(
        &[entry(http(), "consumer", "request", "Req", 2)],
        &root,
        &ResultIndex::default(),
    );
    assert_eq!(value[0].infer_kind, InferKind::RequestBody);
    assert_eq!(value[0].member, None);
    assert!(
        serde_json::to_value(&value[0])
            .unwrap()
            .get("member")
            .is_none()
    );
}

/// An unwritten slot is an HTTP options question: on any other protocol the
/// reader's request stands, and nothing is asked.
#[test]
fn an_unwritten_slot_is_not_asked_of_another_protocol() {
    let dir = repo();
    let root = dir.path().to_string_lossy().into_owned();
    let literal = PayloadSlots {
        request: Some(PayloadAt::Unwritten {
            at: at("\"/orders\""),
            key: "body".to_string(),
        }),
        response: None,
    };
    let socket =
        json!({ "protocol": "socket", "event": "orders", "direction": "client_to_server" });
    let requests = SitePayloads::of(&[record("socket", false, literal)]).requests(
        &[entry(socket, "producer", "response", "Sub", 2)],
        &root,
        &ResultIndex::default(),
    );
    assert!(requests.is_empty(), "{requests:?}");
}
