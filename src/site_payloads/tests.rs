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
        names: Vec::new(),
        bucket: Bucket::Forced,
        lead: None,
        unread: None,
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
        &HashSet::new(),
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
    // Both are read in the site's own call: no call is named beside them.
    assert!(
        requests
            .iter()
            .all(|request| request.through_call.is_none()),
        "{requests:?}"
    );
}

/// carrick#4128: a body the wrapper builds is read in the wrapper's file,
/// and the request names the site's own call beside it, in the sidecar's
/// numbering, so a type parameter of the wrapper is read as that call
/// instantiates it.
#[test]
fn a_body_built_in_the_wrapper_names_the_call_that_reaches_it() {
    const WRAPPER: &str = "src/net.ts";
    const WRAPPER_SOURCE: &str =
        "export const place = <T>(item: T) => http.post(\"/orders\", { body: item });\n";
    let dir = repo();
    std::fs::write(dir.path().join(WRAPPER), WRAPPER_SOURCE).unwrap();
    let built = "{ body: item }";
    let start = WRAPPER_SOURCE.find(built).unwrap() as u32;
    let slots = PayloadSlots {
        request: Some(PayloadAt::Value(AnchorAt {
            file: WRAPPER.to_string(),
            line: 1,
            span_start: start + crate::swc_scanner::SWC_SPAN_BASE,
            span_end: start + built.len() as u32 + crate::swc_scanner::SWC_SPAN_BASE,
        })),
        response: None,
    };
    let payloads = SitePayloads::of(&[record("http_client", false, slots)]);
    let requests = payloads.requests(
        &[entry(http(), "consumer", "request", "Req", 2)],
        &dir.path().to_string_lossy(),
        &ResultIndex::default(),
        &HashSet::new(),
    );
    let [request] = requests.as_slice() else {
        panic!("{requests:?}");
    };
    assert_eq!(
        request.file_path,
        dir.path().join(WRAPPER).to_string_lossy()
    );
    assert_eq!(
        (request.span_start, request.span_end),
        (Some(start), Some(start + built.len() as u32))
    );
    let (call_start, call_end) = sidecar_span("http.post(\"/orders\", order)");
    assert_eq!(
        request.through_call,
        Some(ThroughCall {
            file_path: dir.path().join(FILE).to_string_lossy().into_owned(),
            span_start: call_start.unwrap(),
            span_end: call_end.unwrap(),
        })
    );
}

/// carrick#4128, carrick#4159: a GET sends no body, so the object its wrapper
/// builds (a query object) is not asked as the call's request body. The
/// request entry is left to read `no_request_body`; the response entry is
/// asked as before, and a POST through the same wrapper still asks its body.
#[test]
fn a_get_through_a_wrapper_that_builds_an_object_is_not_asked_a_request_body() {
    const WRAPPER: &str = "src/net.ts";
    const WRAPPER_SOURCE: &str =
        "export const list = (page: number) => http.get(\"/orders\", { query: { page } });\n";
    let dir = repo();
    std::fs::write(dir.path().join(WRAPPER), WRAPPER_SOURCE).unwrap();
    let built = "{ query: { page } }";
    let start = WRAPPER_SOURCE.find(built).unwrap() as u32;
    let slots = PayloadSlots {
        request: Some(PayloadAt::Value(AnchorAt {
            file: WRAPPER.to_string(),
            line: 1,
            span_start: start + crate::swc_scanner::SWC_SPAN_BASE,
            span_end: start + built.len() as u32 + crate::swc_scanner::SWC_SPAN_BASE,
        })),
        response: None,
    };
    let payloads = SitePayloads::of(&[record("http_client", false, slots)]);
    let root = dir.path().to_string_lossy().into_owned();
    let asked = |method: &str| {
        let mut key = http();
        key["method"] = json!(method);
        payloads.requests(
            &[entry(key, "consumer", "request", "Req", 2)],
            &root,
            &ResultIndex::default(),
            &HashSet::new(),
        )
    };
    let get = asked("GET");
    assert!(get.is_empty(), "a GET is asked no request body: {get:?}");
    assert_eq!(asked("POST").len(), 1, "a POST is asked its body");
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
    let labelled = payloads.requests(&entries, &root, &index, &HashSet::new());
    let bare = payloads.requests(&entries, &root, &ResultIndex::default(), &HashSet::new());
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
            .requests(&entries, &root, &elsewhere, &HashSet::new())
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
                &ResultIndex::default(),
                &HashSet::new()
            )
            .is_empty()
    );
    // Another protocol.
    let socket =
        json!({ "protocol": "socket", "event": "orders", "direction": "client_to_server" });
    let socket_entry = entry(socket, "consumer", "response", "Sock", 2);
    assert!(
        http_call
            .requests(
                &[socket_entry],
                &root,
                &ResultIndex::default(),
                &HashSet::new()
            )
            .is_empty()
    );
    // Another side: a route at the line of a client's call.
    assert!(
        http_call
            .requests(
                &[entry(http(), "producer", "request", "Route", 2)],
                &root,
                &ResultIndex::default(),
                &HashSet::new()
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
            &ResultIndex::default(),
            &HashSet::new()
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
        &HashSet::new(),
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
                &ResultIndex::default(),
                &HashSet::new()
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
        route_path: None,
        at_registration: false,
        result_unanswered: false,
        through_call: None,
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

    // The aliases a slot types are the ones the pub/sub collectors skip.
    assert_eq!(
        slotted_aliases(&slotted),
        BTreeSet::from(["Res".to_string()])
    );
    assert!(slotted_aliases(&[]).is_empty());
}
/// carrick#4197: an HTTP route handler's parameter is the request object the
/// server hands it (`req`, a context), never the route's body. The route's
/// request entry asks the body read at the registration call, where the
/// sidecar follows the handler to the member it reads; it is never asked as
/// the parameter. Another protocol's handler parameter is its payload, and
/// stays a parameter read.
#[test]
fn an_http_route_handlers_parameter_is_not_its_request_body() {
    let dir = repo();
    let root = dir.path().to_string_lossy().into_owned();
    // The handler's span is its own, apart from the registration call's, so
    // the request below is pinned to the call.
    let param = || PayloadSlots {
        request: Some(PayloadAt::Param {
            at: at("order)").trimmed(),
            name: "order".to_string(),
        }),
        response: None,
    };
    let route = SitePayloads::of(&[record("server_framework", true, param())]);
    let requests = route.requests(
        &[entry(http(), "producer", "request", "Route", 2)],
        &root,
        &ResultIndex::default(),
        &HashSet::new(),
    );
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].infer_kind, InferKind::RequestBody);
    assert_eq!(requests[0].param_name, None);
    assert_eq!(
        (requests[0].span_start, requests[0].span_end),
        sidecar_span("http.post(\"/orders\", order)")
    );
    // carrick#4204: marked as asked at the registration call, so an
    // unanswered read takes the placeholder, not the call's own type.
    assert!(requests[0].at_registration);

    // Socket, pub/sub and queue handlers are run with their payload: their
    // parameter read stands.
    for (role, key) in [
        (
            "socket",
            json!({ "protocol": "socket", "event": "orders", "direction": "client_to_server" }),
        ),
        ("pubsub", json!({ "protocol": "pubsub", "topic": "orders" })),
        (
            "queue",
            json!({ "protocol": "pubsub", "topic": "orders-queue" }),
        ),
    ] {
        let handler = SitePayloads::of(&[record(role, true, param())]);
        let requests = handler.requests(
            &[entry(key, "producer", "response", "Sub", 2)],
            &root,
            &ResultIndex::default(),
            &HashSet::new(),
        );
        assert_eq!(requests.len(), 1, "{role}: {requests:?}");
        assert_eq!(requests[0].infer_kind, InferKind::FunctionParam, "{role}");
        assert_eq!(requests[0].param_name.as_deref(), Some("order"), "{role}");
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
        &HashSet::new(),
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
        &HashSet::new(),
    );
    assert_eq!(value[0].infer_kind, InferKind::RequestBody);
    assert_eq!(value[0].member, None);
    // A consumer's body argument is a payload: never marked (carrick#4204).
    assert!(!value[0].at_registration);
    assert!(
        serde_json::to_value(&value[0])
            .unwrap()
            .get("member")
            .is_none()
    );
}

/// carrick#4165: a call no call writes a body argument for is answered by
/// the scanner, never asked: its one request is `RequestConfig` naming no
/// key, which the orchestrator keeps from the sidecar and answers decided
/// `no_request_body`. The capture takes the placeholder for it, and a model
/// symbol for the same alias, which nothing sighted, does not win over the
/// answer. On another protocol nothing is asked or stated (carrick#4227).
#[test]
fn an_absent_slot_is_answered_here_and_never_asked() {
    use crate::services::type_sidecar::{CaptureAnchor, SymbolRequest};
    let dir = repo();
    let root = dir.path().to_string_lossy().into_owned();
    let absent = || PayloadSlots {
        request: Some(PayloadAt::Absent {
            at: at("http.post(\"/orders\", order)"),
        }),
        response: None,
    };
    let requests = SitePayloads::of(&[record("http_client", false, absent())]).requests(
        &[entry(http(), "consumer", "request", "Req", 2)],
        &root,
        &ResultIndex::default(),
        &HashSet::new(),
    );
    assert_eq!(requests.len(), 1, "{requests:?}");
    let request = &requests[0];
    assert_eq!(request.infer_kind, InferKind::RequestConfig);
    assert_eq!(request.member, None);
    assert!(request.answered_no_body());
    let answer = request.no_body_answer().expect("answered");
    assert_eq!(answer.alias, "Req");
    assert_eq!(answer.type_string, "unknown");
    assert_eq!(
        answer
            .any_provenance
            .iter()
            .map(|p| (p.path.as_str(), p.reason.as_str()))
            .collect::<Vec<_>>(),
        vec![("", "no_request_body")]
    );

    // The options literal's question is still asked.
    let unwritten = PayloadSlots {
        request: Some(PayloadAt::Unwritten {
            at: at("\"/orders\""),
            key: "body".to_string(),
        }),
        response: None,
    };
    let asked = SitePayloads::of(&[record("http_client", false, unwritten)]).requests(
        &[entry(http(), "consumer", "request", "Req", 2)],
        &root,
        &ResultIndex::default(),
        &HashSet::new(),
    );
    assert!(!asked[0].answered_no_body());
    assert!(asked[0].no_body_answer().is_none());

    // A model symbol for the alias is never asked, so it neither resolves
    // nor captures; another alias's stands.
    let symbol = |alias: &str| SymbolRequest {
        symbol_name: "Order".to_string(),
        source_file: FILE.to_string(),
        alias: Some(alias.to_string()),
        array_depth: Some(1),
        payload_borrow_witness: false,
        consumer_response: false,
    };
    let mut explicit = vec![symbol("Req"), symbol("Other")];
    drop_answered_symbols(&mut explicit, &requests);
    assert_eq!(
        explicit
            .iter()
            .filter_map(|request| request.alias.as_deref())
            .collect::<Vec<_>>(),
        vec!["Other"]
    );
    let mut kept = vec![symbol("Req")];
    drop_answered_symbols(&mut kept, &asked);
    assert_eq!(kept.len(), 1);
    // The capture: the placeholder, not the call's own value.
    explicit.clear();
    let anchors = crate::engine::type_compat_v2::derive_capture_anchors(
        &explicit,
        &requests,
        &[],
        &[answer],
        &["Req".to_string()],
        &root,
    );
    assert_eq!(anchors.len(), 1, "{anchors:?}");
    assert!(
        matches!(&anchors[0], CaptureAnchor::Literal { type_text, .. } if type_text == "unknown"),
        "{anchors:?}"
    );

    let socket =
        json!({ "protocol": "socket", "event": "orders", "direction": "client_to_server" });
    let none = SitePayloads::of(&[record("socket", false, absent())]).requests(
        &[entry(socket, "consumer", "response", "Pub", 2)],
        &root,
        &ResultIndex::default(),
        &HashSet::new(),
    );
    assert!(none.is_empty(), "{none:?}");
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
        &HashSet::new(),
    );
    assert!(requests.is_empty(), "{requests:?}");
}

/// A summary row at a caller as the request summaries state it, at line 2 of
/// [`FILE`], reaching the request written at `net.ts:1`.
fn caller_row(
    target: &str,
    request: &str,
    call_body: Option<crate::forwarded_body::CallBody>,
) -> crate::agents::file_analyzer_agent::DataCallResult {
    let (start, end) = span_of("http.post(\"/orders\", order)");
    crate::agents::file_analyzer_agent::DataCallResult {
        call_kind: None,
        candidate_id: format!("span:{start}-{end}"),
        line_number: 2,
        target: target.to_string(),
        method: Some("POST".to_string()),
        pattern_matched: "place".to_string(),
        call_expression_span_start: Some(start),
        call_expression_span_end: Some(end),
        call_expression_text: None,
        call_expression_line: None,
        payload_expression_text: None,
        payload_expression_line: None,
        primary_type_symbol: None,
        type_import_source: None,
        loopback_default_url: None,
        base: None,
        consumers_not_resolved: None,
        resolution_source: Some(
            crate::agents::file_analyzer_agent::ResolutionSource::RequestSummary,
        ),
        dispatch: None,
        reaches_request: Some(request.to_string()),
        body_literals: Default::default(),
        library_semantics: Vec::new(),
        at_caller: true,
        call_body,
        static_asset: None,
        request_line: None,
    }
}

fn results_with(
    rows: Vec<crate::agents::file_analyzer_agent::DataCallResult>,
) -> std::collections::HashMap<String, crate::agents::file_analyzer_agent::FileAnalysisResult> {
    let mut results = std::collections::HashMap::new();
    results.insert(
        FILE.to_string(),
        crate::agents::file_analyzer_agent::FileAnalysisResult {
            graphql_consumer_locates: vec![],
            mounts: vec![],
            endpoints: vec![],
            data_calls: rows,
            graphql_operations: vec![],
            pubsub_operations: vec![],
            dispatch_tables: Vec::new(),
        },
    );
    results
}

/// carrick#4260: a slot a yielded site carries types only the request entry
/// of the row whose own send that site is, read through the row's own call.
/// A second row on the same line (another request, another path) is not
/// joined, nor is a row that sends a parameter unchanged.
#[test]
fn a_carried_slot_types_only_its_own_row_s_request_entry() {
    const WRAPPER: &str = "src/net.ts";
    const WRAPPER_SOURCE: &str = "export const place = (d: D) => http.post(\"/a\", { body: d });\n";
    let dir = repo();
    std::fs::write(dir.path().join(WRAPPER), WRAPPER_SOURCE).unwrap();
    let built = "{ body: d }";
    let start = WRAPPER_SOURCE.find(built).unwrap() as u32;
    let mut payloads = SitePayloads::default();
    payloads.carried.push(Carried {
        request: (WRAPPER.to_string(), 1),
        rows: vec![(FILE.to_string(), 2)],
        request_slot: PayloadAt::Value(AnchorAt {
            file: WRAPPER.to_string(),
            line: 1,
            span_start: start + crate::swc_scanner::SWC_SPAN_BASE,
            span_end: start + built.len() as u32 + crate::swc_scanner::SWC_SPAN_BASE,
        }),
    });
    let declared = crate::forwarded_body::CallBody::Param(crate::forwarded_body::DeclaredParam {
        file: WRAPPER.to_string(),
        span_start: 1,
        span_end: 2,
        line: 1,
    });
    let results = results_with(vec![
        caller_row(
            "/a",
            "src/net.ts:1",
            Some(crate::forwarded_body::CallBody::Built),
        ),
        // Another request on the same line: the slot is not its own.
        caller_row(
            "/b",
            "src/net.ts:7",
            Some(crate::forwarded_body::CallBody::Built),
        ),
        // A row that sends a parameter unchanged keeps its declared type.
        caller_row("/c", "src/net.ts:1", Some(declared)),
    ]);
    let root = dir.path().to_string_lossy().into_owned();
    let joined = payloads.joined_to_rows(
        &results,
        &root,
        &crate::url_normalizer::UrlNormalizer::new(&crate::config::Config::default()),
    );
    let at_path = |path: &str| {
        entry(
            json!({ "protocol": "http", "method": "POST", "path": path }),
            "consumer",
            "request",
            &format!("Req{path}"),
            2,
        )
    };
    let entries = [at_path("/a"), at_path("/b"), at_path("/c")];
    assert!(joined.types_row(&entries[0]));
    assert!(!joined.types_row(&entries[1]));
    assert!(!joined.types_row(&entries[2]));

    let requests = joined.requests(&entries, &root, &ResultIndex::default(), &HashSet::new());
    let [request] = requests.as_slice() else {
        panic!("{requests:?}");
    };
    assert_eq!(request.alias.as_deref(), Some("Req/a"));
    let (call_start, call_end) = sidecar_span("http.post(\"/orders\", order)");
    assert_eq!(
        request.through_call,
        Some(ThroughCall {
            file_path: dir.path().join(FILE).to_string_lossy().into_owned(),
            span_start: call_start.unwrap(),
            span_end: call_end.unwrap(),
        })
    );
    // A row the slot does not reach is joined to nothing without the carry.
    let none = SitePayloads::default().joined_to_rows(
        &results,
        &root,
        &crate::url_normalizer::UrlNormalizer::new(&crate::config::Config::default()),
    );
    assert!(!none.types_row(&entries[0]));
}
