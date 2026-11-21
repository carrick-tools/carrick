//! The stored record of the cloud's model step (`carrick.matched/2`): ends,
//! each one serving side or one call site in the index's own key terms. The
//! cloud pins the same bytes by the same hash
//! (carrick-cloud `lambdas/_shared/model_record.contract.json`); a change of
//! spelling on either side fails a test on that side.

use serde_json::Value;
use sha2::{Digest, Sha256};

const CONTRACT: &str = include_str!("fixtures/matched-record/contract.json");
const CONTRACT_SHA256: &str = "9c45f54cdf16551799245b5f73c9df09ecd80b1cc82a71d65e3b329e99b77a08";

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
const PROTOCOLS: &[&str] = &["http", "graphql", "socket", "pubsub"];
const REQUIRED_END_KEYS: &[&str] = &[
    "id",
    "side",
    "protocol",
    "kind",
    "file",
    "line",
    "line_sha256",
    "confidence",
    "basis",
    "carried_from",
];

/// The key fields each protocol states, as `OperationKey` keys it.
fn key_fields(protocol: &str) -> &'static [&'static str] {
    match protocol {
        "http" => &["method", "path"],
        "graphql" => &["name", "gql_kind"],
        "socket" => &["name", "direction"],
        "pubsub" => &["name"],
        _ => unreachable!("outside the protocol set"),
    }
}

#[test]
fn the_model_record_contract_is_the_one_the_cloud_pins() {
    assert_eq!(
        format!("{:x}", Sha256::digest(CONTRACT.as_bytes())),
        CONTRACT_SHA256
    );

    let record: Value = serde_json::from_str(CONTRACT).expect("contract parses");
    assert_eq!(record["schema"], "carrick.matched/2");
    assert!(record.get("pairs").is_none(), "a v2 record holds no pairs");

    let ends = record["ends"].as_array().expect("ends is an array");
    assert_eq!(ends.len(), 7);
    let mut protocols: Vec<&str> = Vec::new();
    for end in ends {
        for key in REQUIRED_END_KEYS {
            assert!(end.get(key).is_some(), "end lacks `{key}`");
        }
        let side = end["side"].as_str().expect("side is a string");
        assert!(side == "call" || side == "endpoint", "side `{side}`");
        let kind = end["kind"].as_str().expect("kind is a string");
        assert!(KINDS.contains(&kind), "kind `{kind}` is outside the set");
        let protocol = end["protocol"].as_str().expect("protocol is a string");
        assert!(PROTOCOLS.contains(&protocol), "protocol `{protocol}`");
        if protocol == "http" && end.get("procedure").is_some() {
            assert!(end["procedure"]["path"].is_string());
        } else {
            for field in key_fields(protocol) {
                assert!(end[*field].is_string(), "{protocol} end lacks `{field}`");
            }
        }
        if protocol == "http" {
            let method = end["method"].as_str().unwrap();
            assert_eq!(method, method.to_uppercase(), "the method is upper case");
        }
        assert_eq!(end["line_sha256"].as_str().unwrap().len(), 64);
        protocols.push(protocol);
    }
    protocols.sort_unstable();
    protocols.dedup();
    // Every protocol the index keys is in the contract: never HTTP only.
    assert_eq!(protocols, ["graphql", "http", "pubsub", "socket"]);
    let coverage = &record["run"]["coverage"];
    for key in ["kinds", "stopped_by", "turns", "usd"] {
        assert!(coverage.get(key).is_some(), "coverage lacks `{key}`");
    }
}
