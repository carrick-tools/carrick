//! Which producer a call reaches (carrick#3496), run from the
//! language-neutral vector file the cloud runs too. Every case must give
//! exactly its `expect`; the `identity` and `canonical` blocks pin
//! `route_identity` and `canonical_path`.
//!
//! The rows are read from `serde_json::Value` by hand, not through the
//! crate's serde derives: this test runs with the crate's default features
//! (`cargo test -p carrick-match`). The wasm smoke in `.github/workflows/ci.yml`
//! runs the same file through the serde shape, which is the native/wasm
//! parity proof.

use std::collections::BTreeSet;

use carrick_match::{
    Call, MatchEvidence, MatchRelationship, MatchVerdict, NameScope, PairOutcome, Pairing,
    PairingPool, ProcedureKey, Producer, RouterId, canonical_path, pair_winners, route_identity,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

const VECTORS: &[u8] = include_bytes!("fixtures/pairing.vectors.json");

/// The sha256 of the vector file. The cloud's copy
/// (`lambdas/mcp-server/src/utils/__tests__/fixtures/pairing.vectors.json`)
/// is byte-identical, so a change is a contract change: change the vectors
/// on both sides and re-pin both values together.
const PINNED_SHA256: &str = "48373db96c94a323dc1d8353ae0c1fc51e4ceb8838db13964b5ebff23741c3ff";

/// The cases that pin today's answer for a named product gap (the file's
/// `expected_red` marker). The change that closes a gap removes its marker
/// and this entry together.
const EXPECTED_RED: &[&str] = &["template-hole-inside-a-route-segment-is-literal"];

fn file() -> Value {
    serde_json::from_slice(VECTORS).expect("the vector file is JSON")
}

fn text(value: &Value, field: &str) -> Option<String> {
    match value.get(field) {
        None => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(other) => panic!("`{field}` is not a string: {other}"),
    }
}

fn required(value: &Value, field: &str) -> String {
    text(value, field).unwrap_or_else(|| panic!("`{field}` missing in {value}"))
}

/// The vector file's `name_scope` object, read strictly: the file is ours, so
/// a malformed entry is a broken vector.
fn name_scope(value: &Value) -> Option<NameScope> {
    let raw = value.get("name_scope")?;
    let namespace = match &raw["namespace"] {
        Value::Null => None,
        Value::String(namespace) => Some(namespace.clone()),
        other => panic!("namespace {other} is neither a string nor null"),
    };
    Some(NameScope {
        scope: required(raw, "scope"),
        namespace,
    })
}

/// The vector file's `procedure` object, read strictly.
fn procedure(value: &Value) -> Option<ProcedureKey> {
    let raw = value.get("procedure")?;
    let router = &raw["router"];
    Some(ProcedureKey {
        path: required(raw, "path"),
        router: RouterId {
            repo: required(router, "repo"),
            file: required(router, "file"),
            binding: required(router, "binding"),
        },
    })
}

fn producer(value: &Value) -> Producer {
    Producer {
        service: required(value, "service"),
        protocol: required(value, "protocol"),
        method: text(value, "method"),
        path: text(value, "path"),
        name: text(value, "name"),
        direction: text(value, "direction"),
        dispatch: text(value, "dispatch"),
        evidence: match text(value, "evidence").as_deref() {
            None | Some("route_definition") => MatchEvidence::RouteDefinition,
            Some("call_site") => MatchEvidence::CallSite,
            Some(other) => panic!("unknown evidence {other}"),
        },
        name_scope: name_scope(value),
        procedure: procedure(value),
    }
}

fn call(value: &Value) -> Call {
    Call {
        service: required(value, "service"),
        protocol: required(value, "protocol"),
        method: text(value, "method"),
        path: text(value, "path"),
        name: text(value, "name"),
        direction: text(value, "direction"),
        dispatch: text(value, "dispatch"),
        name_scope: name_scope(value),
        procedure: procedure(value),
    }
}

fn outcome_name(outcome: PairOutcome) -> &'static str {
    match outcome {
        PairOutcome::Matched => "matched",
        PairOutcome::RouteCaseUnknown => "route_case_unknown",
        PairOutcome::NoCaseAnswers => "no_case_answers",
        PairOutcome::UnknownCallPath => "unknown_call_path",
        PairOutcome::NoLiteralAgreement => "no_literal_agreement",
        PairOutcome::NoRoute => "no_route",
        PairOutcome::ProcedureUnserved => "procedure_unserved",
    }
}

fn verdict_name(verdict: MatchVerdict) -> &'static str {
    match verdict {
        MatchVerdict::Matched => "matched",
        MatchVerdict::UnknownCallPath => "unknown_call_path",
        MatchVerdict::CatchAllRoute => "catch_all_route",
        MatchVerdict::NoRouteMatch => "no_route_match",
        MatchVerdict::NoLiteralAgreement => "no_literal_agreement",
        MatchVerdict::DispatchValueUnknown => "dispatch_value_unknown",
        MatchVerdict::DispatchValueMismatch => "dispatch_value_mismatch",
    }
}

fn relationship_name(relationship: MatchRelationship) -> &'static str {
    match relationship {
        MatchRelationship::ProducerConsumer => "producer_consumer",
        MatchRelationship::SharedExternalContract => "shared_external_contract",
    }
}

/// The result as the vector file writes it: snake_case names, and an absent
/// field (never `null`) where the result carries none.
fn as_json(pairing: &Pairing) -> Value {
    let winners: Vec<Value> = pairing
        .winners
        .iter()
        .map(|w| {
            let mut row = Map::new();
            row.insert("producer".into(), json!(w.producer));
            row.insert("verdict".into(), json!(verdict_name(w.verdict)));
            if let Some(agreement) = w.agreement {
                row.insert("agreement".into(), json!(agreement));
            }
            row.insert("same_service".into(), json!(w.same_service));
            row.insert(
                "relationship".into(),
                json!(relationship_name(w.relationship)),
            );
            if let Some(flag) = w.call_param_over_route_literal {
                row.insert("call_param_over_route_literal".into(), json!(flag));
            }
            Value::Object(row)
        })
        .collect();
    let routed: Vec<Value> = pairing
        .routed
        .iter()
        .map(|r| {
            json!({
                "producer": r.producer,
                "verdict": verdict_name(r.verdict),
                "agreement": r.agreement,
            })
        })
        .collect();
    json!({
        "outcome": outcome_name(pairing.outcome),
        "winners": winners,
        "routed": routed,
    })
}

#[test]
fn the_vectors_are_the_pinned_file() {
    let digest = Sha256::digest(VECTORS);
    let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(
        actual, PINNED_SHA256,
        "pairing.vectors.json changed. It is the contract the cloud runs too \
         (carrick#3496): change its copy in carrick-cloud, then re-pin this value \
         and the cloud's test together."
    );
}

#[test]
fn the_vectors_are_well_formed() {
    let file = file();
    let cases = file["cases"].as_array().expect("`cases` is an array");
    assert!(!cases.is_empty());
    let mut ids = BTreeSet::new();
    let mut red = BTreeSet::new();
    for case in cases {
        let id = required(case, "id");
        assert!(ids.insert(id.clone()), "case {id} twice");
        if case.get("expected_red").is_some() {
            assert!(
                !required(case, "expected_red").trim().is_empty(),
                "{id}: expected_red names no reason"
            );
            red.insert(id);
        }
    }
    let wanted: BTreeSet<String> = EXPECTED_RED.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        red, wanted,
        "the expected_red markers changed: a closed gap drops its marker here too"
    );
}

#[test]
fn every_case_pairs_exactly_its_expect() {
    let file = file();
    // Every wrong case is named, not only the first, so a change to the rule
    // shows its whole blast radius in one run.
    let mut wrong = Vec::new();
    for case in file["cases"].as_array().expect("`cases` is an array") {
        let id = required(case, "id");
        let producers: Vec<Producer> = case["input"]["producers"]
            .as_array()
            .expect("`producers` is an array")
            .iter()
            .map(producer)
            .collect();
        let call = call(&case["input"]["call"]);
        let pooled = PairingPool::new(producers.clone()).pair(&call);
        assert_eq!(
            pair_winners(&producers, &call),
            pooled,
            "case {id}: pair_winners and PairingPool disagree"
        );
        let actual = as_json(&pooled);
        if actual != case["expect"] {
            wrong.push(format!(
                "{id}\n  expect {}\n  actual {actual}",
                case["expect"]
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} case(s) wrong:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

#[test]
fn every_identity_row_folds_as_pinned() {
    let file = file();
    for row in file["identity"].as_array().expect("`identity` is an array") {
        let path = required(row, "path");
        assert_eq!(
            route_identity(&path),
            required(row, "route_identity"),
            "route_identity({path})"
        );
    }
}

#[test]
fn every_canonical_row_is_the_write_side_form() {
    let file = file();
    for row in file["canonical"]
        .as_array()
        .expect("`canonical` is an array")
    {
        let path = required(row, "path");
        assert_eq!(
            canonical_path(&path),
            required(row, "canonical_path"),
            "canonical_path({path})"
        );
    }
}
