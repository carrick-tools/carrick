//! The ranker behind "which indexed function does the one I just wrote look
//! like" (carrick-cloud#3490): the bag, the BM25 score, the drops and the cut.
//!
//! `tests/fixtures/similar_bag_parity.json` is the frozen output of the cloud's
//! lexical leg over invented inputs, so the bag here and the one the hosted
//! search ranks on cannot drift apart. Every corpus below is invented.

use carrick_match::{
    SIMILAR_ROWS, SimilarDoc, SimilarIndex, SimilarQuery, similar_bag, similar_tokenize,
};
use serde_json::Value;

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn the_bag_equals_what_the_lexical_leg_builds() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/similar_bag_parity.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 30);
    for case in cases {
        let tokens = strings(&case["tokens"]);
        let bag = similar_bag(
            case["name"].as_str().unwrap(),
            case["intent"].as_str(),
            &tokens,
            case["signature"].as_str(),
        );
        assert_eq!(bag.terms, strings(&case["bag"]), "bag of {}", case["name"]);
        assert_eq!(
            bag.described_len as u64,
            case["described_length"].as_u64().unwrap(),
            "length of {}",
            case["name"]
        );
    }
}

#[test]
fn identifiers_split_the_way_the_lexical_leg_splits_them() {
    assert_eq!(
        similar_tokenize("XMLHttpRequest2"),
        ["xml", "http", "request", "2"]
    );
    assert_eq!(similar_tokenize("a_b-c.d"), ["a", "b", "c", "d"]);
    assert_eq!(similar_tokenize(""), Vec::<String>::new());
}

/// An owned function, so a test can lend it to the index.
struct Fn_ {
    name: &'static str,
    file: &'static str,
    intent: Option<&'static str>,
    tokens: Vec<String>,
    signature: Option<&'static str>,
    calls: Vec<&'static str>,
}

fn function(name: &'static str, file: &'static str, tokens: &[&str]) -> Fn_ {
    Fn_ {
        name,
        file,
        intent: None,
        tokens: tokens.iter().map(|t| t.to_string()).collect(),
        signature: None,
        calls: Vec::new(),
    }
}

fn index(corpus: &[Fn_]) -> SimilarIndex {
    SimilarIndex::build(corpus.iter().map(|f| SimilarDoc {
        name: f.name,
        file: f.file,
        intent: f.intent,
        tokens: &f.tokens,
        signature: f.signature,
        calls: f.calls.clone(),
    }))
}

fn query<'a>(
    name: &'a str,
    file: &'a str,
    tokens: &'a [String],
    callees: &'a [String],
) -> SimilarQuery<'a> {
    SimilarQuery {
        name,
        file,
        tokens,
        signature: None,
        callees,
    }
}

fn texts(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

/// Filler functions, so a word is rare or common as it would be in a repo.
fn filler() -> Vec<Fn_> {
    vec![
        function(
            "loadInvoices",
            "billing/load.ts",
            &["invoice", "ledger", "rows"],
        ),
        function(
            "sendWelcomeMail",
            "mail/welcome.ts",
            &["smtp", "template", "welcome"],
        ),
        function(
            "resizeAvatar",
            "media/avatar.ts",
            &["sharp", "width", "height"],
        ),
        function(
            "pruneSessions",
            "auth/prune.ts",
            &["session", "expires", "delete"],
        ),
        function("listTeams", "teams/list.ts", &["team", "members", "page"]),
    ]
}

#[test]
fn a_renamed_near_copy_outranks_a_decoy_that_shares_the_name_words() {
    let mut corpus = filler();
    corpus.push(function(
        "renderDurationLabel",
        "ui/duration.ts",
        &[
            "seconds", "minutes", "hours", "padStart", "floor", "3600", "60", "join",
        ],
    ));
    corpus.push(function(
        "formatElapsedTimeSheet",
        "billing/timesheet.ts",
        &["invoice", "customer", "ledger", "rows"],
    ));
    let tokens = texts(&[
        "seconds", "minutes", "hours", "padStart", "floor", "3600", "60",
    ]);
    let rows = index(&corpus).rank(&query("formatElapsedTime", "ui/clock.ts", &tokens, &[]));
    let names: Vec<&str> = rows.iter().map(|r| corpus[r.doc].name).collect();
    assert_eq!(names.first(), Some(&"renderDurationLabel"), "{names:?}");
    assert!(names.contains(&"formatElapsedTimeSheet"), "{names:?}");
    assert!(rows[0].score > rows[1].score);
}

#[test]
fn a_function_the_query_calls_is_absent() {
    let mut corpus = filler();
    corpus.push(function(
        "padLeft",
        "util/pad.ts",
        &["pad", "left", "string"],
    ));
    corpus.push(function(
        "padRight",
        "util/pad.ts",
        &["pad", "right", "string"],
    ));
    let tokens = texts(&["pad", "string", "padLeft"]);
    let callees = texts(&["padLeft"]);
    let rows = index(&corpus).rank(&query("padBoth", "ui/pad.ts", &tokens, &callees));
    let names: Vec<&str> = rows.iter().map(|r| corpus[r.doc].name).collect();
    assert_eq!(names, ["padRight"]);
    // With no call recorded, the body's own tokens stand in for them.
    let rows = index(&corpus).rank(&query("padBoth", "ui/pad.ts", &tokens, &[]));
    let names: Vec<&str> = rows.iter().map(|r| corpus[r.doc].name).collect();
    assert_eq!(names, ["padRight"]);
}

#[test]
fn a_function_that_calls_the_query_is_absent() {
    let mut corpus = filler();
    let mut caller = function("renderPrice", "ui/price.ts", &["price", "cents", "format"]);
    caller.calls = vec!["formatCents"];
    corpus.push(caller);
    corpus.push(function(
        "formatEuros",
        "ui/euro.ts",
        &["price", "cents", "format"],
    ));
    let tokens = texts(&["price", "cents", "format"]);
    let rows = index(&corpus).rank(&query("formatCents", "ui/cents.ts", &tokens, &[]));
    let names: Vec<&str> = rows.iter().map(|r| corpus[r.doc].name).collect();
    assert_eq!(names, ["formatEuros"]);
}

#[test]
fn the_edited_file_callbacks_and_module_rows_never_come_back() {
    let mut corpus = filler();
    corpus.push(function(
        "sameFileTwin",
        "ui/clock.ts",
        &["seconds", "minutes"],
    ));
    corpus.push(function(
        "seconds_handler",
        "ui/other.ts",
        &["seconds", "minutes"],
    ));
    corpus.push(function("<module>", "ui/other.ts", &["seconds", "minutes"]));
    corpus.push(function(
        "otherTwin",
        "ui/other.ts",
        &["seconds", "minutes"],
    ));
    let tokens = texts(&["seconds", "minutes"]);
    let rows = index(&corpus).rank(&query("clockFace", "ui/clock.ts", &tokens, &[]));
    let names: Vec<&str> = rows.iter().map(|r| corpus[r.doc].name).collect();
    assert_eq!(names, ["otherTwin"]);
}

#[test]
fn a_bag_that_shares_no_word_gives_no_row() {
    let corpus = filler();
    let tokens = texts(&["zebra", "quartz"]);
    assert!(
        index(&corpus)
            .rank(&query("zigzag", "x/y.ts", &tokens, &[]))
            .is_empty()
    );
}

#[test]
fn at_most_six_rows_and_the_same_member_name_first() {
    let mut corpus = filler();
    for (name, file) in [
        ("weekOne", "w/1.ts"),
        ("weekTwo", "w/2.ts"),
        ("weekThree", "w/3.ts"),
        ("weekFour", "w/4.ts"),
        ("weekFive", "w/5.ts"),
        ("weekSix", "w/6.ts"),
        ("weekSeven", "w/7.ts"),
        ("weekEight", "w/8.ts"),
    ] {
        corpus.push(function(name, file, &["week", "number", "utc", "date"]));
    }
    // Weaker on words than the rest, but it carries the member name.
    corpus.push(function("Calendar.weekNine", "w/9.ts", &["date"]));
    assert_eq!(SIMILAR_ROWS, 6);
    let tokens = texts(&["week", "number", "utc", "date"]);
    let rows = index(&corpus).rank(&query("weekNine", "q/q.ts", &tokens, &[]));
    assert_eq!(rows.len(), 6);
    assert_eq!(corpus[rows[0].doc].name, "Calendar.weekNine");
    assert!(rows[1..].windows(2).all(|w| w[0].score >= w[1].score));
}

#[test]
fn the_score_is_bm25_over_the_bag_less_the_query_idf_mass() {
    // Three documents, one query word, hand-computed.
    let corpus = vec![
        function("alpha", "a.ts", &["ledger"]),
        function("beta", "b.ts", &["ledger", "ledger", "invoice"]),
        function("gamma", "c.ts", &["invoice"]),
    ];
    let tokens = texts(&["ledger"]);
    let rows = index(&corpus).rank(&query("zeta", "z.ts", &tokens, &[]));
    // Described lengths: alpha 2 (name + ledger), beta 4 (name + three), gamma 2; average 8/3.
    let (n, df, k1, b, avg) = (3.0_f64, 2.0_f64, 1.2_f64, 0.75_f64, 8.0_f64 / 3.0);
    let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
    // The query's other word, `zeta`, is in no document: it weighs in the mass only.
    let unseen = (1.0 + (n + 0.5) / 0.5_f64).ln();
    let bm = |tf: f64, len: f64| idf * tf * (k1 + 1.0) / (tf + k1 * (1.0 - b + b * len / avg));
    let want_alpha = bm(1.0, 2.0) / (idf + unseen);
    let want_beta = bm(2.0, 4.0) / (idf + unseen);
    let got = |name: &str| {
        rows.iter()
            .find(|r| corpus[r.doc].name == name)
            .map(|r| r.score)
            .unwrap()
    };
    assert!((got("alpha") - want_alpha).abs() < 1e-9);
    assert!((got("beta") - want_beta).abs() < 1e-9);
    assert_eq!(rows.len(), 2);
}
