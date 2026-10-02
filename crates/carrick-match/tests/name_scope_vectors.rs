//! The name-scope pairing rule (carrick#1564 section 4), run from the
//! language-neutral vectors the cloud runs too (carrick-cloud#1570, its
//! `name-scope.test.ts`). Every case must give exactly its `pairs`.

use std::collections::BTreeSet;

use carrick_match::{NameScope, ScopedRow, names_pair};
use serde_json::Value;
use sha2::{Digest, Sha256};

const VECTORS: &[u8] = include_bytes!("fixtures/name-scope-pairing.vectors.json");

/// The sha256 of the vector file, as pinned on carrick-tools/carrick#1663 and
/// in the cloud's test. The copy here is byte-identical to the cloud's, so a
/// change is a contract change: change the vectors on both sides and re-pin
/// both values together.
const PINNED_SHA256: &str = "f0cb5daf4eb4931e5a9ff5232c8167c0c7b68fb1b6bfa95006df4dbaf9a16d07";

struct Row {
    id: String,
    producer: bool,
    service: String,
    protocol: String,
    name: String,
    direction: Option<String>,
    name_scope: Option<NameScope>,
}

impl Row {
    /// The operation key as the blob carries it: a socket row with no
    /// direction is `unknown`, as the cloud's `keyOf` reads it.
    fn key(&self) -> (&str, &str, &str) {
        let direction = match self.protocol.as_str() {
            "socket" => self.direction.as_deref().unwrap_or("unknown"),
            _ => "",
        };
        (&self.protocol, &self.name, direction)
    }

    fn scoped(&self) -> ScopedRow<'_> {
        ScopedRow {
            service: &self.service,
            name_scope: self.name_scope.as_ref(),
        }
    }
}

struct Case {
    id: String,
    rows: Vec<Row>,
    pairs: BTreeSet<(String, String)>,
}

fn text(value: &Value, field: &str) -> String {
    value[field]
        .as_str()
        .unwrap_or_else(|| panic!("`{field}` is not a string in {value}"))
        .to_string()
}

/// The vector file's `name_scope` object, read strictly: the file is ours, so
/// a malformed entry is a broken vector, not a row stating nothing.
fn name_scope(value: &Value) -> Option<NameScope> {
    let raw = value.get("name_scope")?;
    let namespace = match &raw["namespace"] {
        Value::Null => None,
        Value::String(namespace) => Some(namespace.clone()),
        other => panic!("namespace {other} is neither a string nor null"),
    };
    Some(NameScope {
        scope: text(raw, "scope"),
        namespace,
    })
}

fn cases() -> Vec<Case> {
    let file: Value = serde_json::from_slice(VECTORS).expect("the vector file is JSON");
    file["cases"]
        .as_array()
        .expect("`cases` is an array")
        .iter()
        .map(|case| Case {
            id: text(case, "id"),
            rows: case["rows"]
                .as_array()
                .expect("`rows` is an array")
                .iter()
                .map(|row| Row {
                    id: text(row, "id"),
                    producer: match text(row, "side").as_str() {
                        "producer" => true,
                        "consumer" => false,
                        side => panic!("unknown side {side}"),
                    },
                    service: text(row, "service"),
                    protocol: text(row, "protocol"),
                    name: text(row, "name"),
                    direction: row.get("direction").map(|_| text(row, "direction")),
                    name_scope: name_scope(row),
                })
                .collect(),
            pairs: case["pairs"]
                .as_array()
                .expect("`pairs` is an array")
                .iter()
                .map(|pair| {
                    let pair = pair.as_array().expect("a pair is an array");
                    assert_eq!(pair.len(), 2, "a pair is [consumer, producer]");
                    (
                        pair[0].as_str().expect("an id").to_string(),
                        pair[1].as_str().expect("an id").to_string(),
                    )
                })
                .collect(),
        })
        .collect()
}

#[test]
fn the_vectors_are_the_pinned_file() {
    let digest = Sha256::digest(VECTORS);
    let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(
        actual, PINNED_SHA256,
        "name-scope-pairing.vectors.json changed. It is the contract the cloud runs too \
         (carrick#1663): change its copy in carrick-cloud, then re-pin this value, the \
         cloud's test and the sha256 on the issue together."
    );
}

#[test]
fn the_vectors_are_well_formed() {
    let cases = cases();
    assert!(!cases.is_empty());
    let mut case_ids = BTreeSet::new();
    for case in &cases {
        assert!(case_ids.insert(case.id.as_str()), "case {} twice", case.id);
        let mut row_ids = BTreeSet::new();
        for row in &case.rows {
            assert!(
                row_ids.insert(row.id.as_str()),
                "{}: row {} twice",
                case.id,
                row.id
            );
        }
        for (consumer, producer) in &case.pairs {
            let side = |id: &str| {
                case.rows
                    .iter()
                    .find(|row| row.id == id)
                    .map(|row| row.producer)
            };
            assert_eq!(
                side(consumer),
                Some(false),
                "{}: {consumer} is no consumer",
                case.id
            );
            assert_eq!(
                side(producer),
                Some(true),
                "{}: {producer} is no producer",
                case.id
            );
        }
    }
}

#[test]
fn every_case_pairs_exactly_its_pairs() {
    for case in cases() {
        let mut actual = BTreeSet::new();
        for call in case.rows.iter().filter(|row| !row.producer) {
            for producer in case.rows.iter().filter(|row| row.producer) {
                if call.key() == producer.key() && names_pair(call.scoped(), producer.scoped()) {
                    actual.insert((call.id.clone(), producer.id.clone()));
                }
            }
        }
        assert_eq!(actual, case.pairs, "case {}", case.id);
    }
}
