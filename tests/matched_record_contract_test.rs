//! The stored record of model-placed call-to-server pairs
//! (`carrick.matched/1`). The cloud pins the same bytes by the same hash;
//! a change of spelling on either side fails a test on that side.

use serde_json::Value;
use sha2::{Digest, Sha256};

const CONTRACT: &str = include_str!("fixtures/matched-record/contract.json");
const CONTRACT_SHA256: &str = "10d6047678d751f8fb395eba8cb56f939f6b78ee170abce9194c35341848d065";

const KINDS: &[&str] = &[
    "route",
    "graphql_field",
    "procedure",
    "topic",
    "event",
    "queue",
    "table",
    "database_function",
    "function",
    "other",
];
const SIDES: &[&str] = &[
    "caller_sends",
    "caller_expects",
    "server_reads",
    "server_returns",
];
const GRADES: &[&str] = &[
    "fails_at_run_time",
    "type_level_only",
    "extra_field_ignored",
    "agree",
    "cannot_tell",
];
const REQUIRED_PAIR_KEYS: &[&str] = &[
    "id",
    "caller",
    "server",
    "operation",
    "basis",
    "model_confidence",
    "carried_from",
];

#[test]
fn the_matched_record_contract_is_the_one_the_cloud_pins() {
    assert_eq!(
        format!("{:x}", Sha256::digest(CONTRACT.as_bytes())),
        CONTRACT_SHA256
    );

    let record: Value = serde_json::from_str(CONTRACT).expect("contract parses");
    assert_eq!(record["schema"], "carrick.matched/1");

    let pairs = record["pairs"].as_array().expect("pairs is an array");
    assert_eq!(pairs.len(), 3);
    for pair in pairs {
        for key in REQUIRED_PAIR_KEYS {
            assert!(pair.get(key).is_some(), "pair lacks `{key}`");
        }
        let kind = pair["operation"]["kind"]
            .as_str()
            .expect("kind is a string");
        assert!(
            KINDS.contains(&kind),
            "kind `{kind}` is outside the closed set"
        );

        if let Some(sides) = pair.get("sides") {
            for (side, readings) in sides.as_object().expect("sides is an object") {
                assert!(SIDES.contains(&side.as_str()), "unknown side `{side}`");
                for source in readings.as_object().expect("side is an object").keys() {
                    assert!(
                        source == "compiler" || source == "model",
                        "side `{side}` holds reading source `{source}`"
                    );
                }
            }
        }
        if let Some(verdicts) = pair.get("verdicts") {
            for (source, verdict) in verdicts.as_object().expect("verdicts is an object") {
                assert!(source == "compiler" || source == "model");
                if let Some(grade) = verdict["grade"].as_str() {
                    assert!(
                        GRADES.contains(&grade),
                        "grade `{grade}` is outside the set"
                    );
                }
            }
        }
    }
}
