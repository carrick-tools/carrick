//! The ranker over the `wasm` feature's JSON surface (carrick-cloud#3490 T5):
//! the cloud builds an index from function rows and ranks a `code` seed against
//! it at query time. The same vector file runs over the built wasm in the
//! `wasm-match` job of `.github/workflows/ci.yml`.
#![cfg(feature = "wasm")]

use carrick_match::SimilarRanker;
use serde_json::Value;

fn vectors() -> Value {
    serde_json::from_str(include_str!("fixtures/similar_wasm.vectors.json")).unwrap()
}

fn rank(ranker: &SimilarRanker, query: &Value) -> Vec<(u64, f64)> {
    let out: Value = serde_json::from_str(&ranker.rank_json(&query.to_string()).unwrap()).unwrap();
    out.as_array()
        .unwrap()
        .iter()
        .map(|row| (row["doc"].as_u64().unwrap(), row["score"].as_f64().unwrap()))
        .collect()
}

#[test]
fn a_code_seed_ranks_its_twin_first_and_ties_in_a_fixed_order() {
    let file = vectors();
    let ranker = SimilarRanker::from_json(&file["rows"].to_string()).unwrap();
    let rows = rank(&ranker, &file["query"]);
    let docs: Vec<u64> = rows.iter().map(|(doc, _)| *doc).collect();
    let expect: Vec<u64> = file["expect_docs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    assert_eq!(docs, expect);
    assert!(rows[0].1 > rows[1].1);
    assert_eq!(rows[1].1, rows[2].1, "the two tied rows score alike");
}

#[test]
fn the_same_rows_fed_in_reverse_give_the_same_names() {
    let file = vectors();
    let mut reversed = file["rows"].as_array().unwrap().clone();
    reversed.reverse();
    let ranker = SimilarRanker::from_json(&Value::Array(reversed.clone()).to_string()).unwrap();
    let names: Vec<&str> = rank(&ranker, &file["query"])
        .iter()
        .map(|(doc, _)| reversed[*doc as usize]["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["renderDurationLabel", "minutesLabelA", "minutesLabelB"]
    );
}
