//! Call edges through `require` bindings (carrick#1348).
//!
//! Drives the real scanner binary offline over the two variants in
//! `tests/fixtures/commonjs-require-callers/`: a CommonJS service and its ESM
//! twin, written to the same answer key. Reads
//! `function_definitions[].calls` from the WRITTEN BLOB, because that is the
//! field `get_callers` inverts.
//!
//! Pre-fix baseline: the ESM arm answered the whole key and the CommonJS arm
//! answered NOTHING — every caller row was an empty edge list, whatever form
//! its require took. The two arms differ only in how the same functions reach
//! each other, so the ESM arm is the control that says the key itself is
//! right.
//!
//! The CommonJS arm also requires a module whose specifier is computed. That
//! names no module, so its call must record no edge AND be reported on
//! stderr.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

fn fixture_dir(variant: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/commonjs-require-callers")
        .join(variant)
}

struct Scan {
    /// Caller key -> the callee names its edges record.
    calls: BTreeMap<String, BTreeSet<String>>,
    /// Caller key -> the files its edges point at.
    files: BTreeMap<String, BTreeSet<String>>,
    stderr: String,
}

impl Scan {
    /// Every caller with an edge to `callee`: what `get_callers` returns.
    fn callers_of(&self, callee: &str) -> BTreeSet<String> {
        self.calls
            .iter()
            .filter(|(_, callees)| callees.contains(callee))
            .map(|(caller, _)| caller.clone())
            .collect()
    }

    fn callees_of(&self, caller: &str) -> BTreeSet<String> {
        self.calls.get(caller).cloned().unwrap_or_default()
    }
}

fn scan(variant: &str) -> Scan {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    // No cassettes: every analyzer call answers empty, and call edges are
    // deterministic, so none are needed.
    let cassettes = tempfile::tempdir().expect("temp cassette dir");

    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(fixture_dir(variant))
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.path().display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        // The assertion is on call edges; the type layer is not under test.
        .env("CARRICK_ALLOW_MISSING_TYPES", "1");
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("failed to spawn carrick");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "{variant} fixture scan exited non-zero:\n{stderr}"
    );

    let mut blobs = std::fs::read_dir(storage.path())
        .expect("storage dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    blobs.sort();
    assert_eq!(blobs.len(), 1, "expected one written blob, got {blobs:?}");
    let blob: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&blobs[0]).expect("read blob")).expect("parse blob");

    let definitions = blob["function_definitions"]
        .as_object()
        .expect("blob has no function_definitions")
        .clone();
    let edges = |key: &str| -> Vec<serde_json::Value> {
        definitions[key]["calls"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    let calls = definitions
        .keys()
        .map(|key| {
            let callees = edges(key)
                .iter()
                .filter_map(|call| call["name"].as_str().map(str::to_owned))
                .collect();
            (key.clone(), callees)
        })
        .collect();
    let files = definitions
        .keys()
        .map(|key| {
            let paths = edges(key)
                .iter()
                .filter_map(|call| call["file_path"].as_str().map(str::to_owned))
                .collect();
            (key.clone(), paths)
        })
        .collect();
    Scan {
        calls,
        files,
        stderr,
    }
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// The key both arms answer: the same functions calling the same functions,
/// reached one way in CommonJS and the other way in ESM.
///
/// `only_here` names the callers of `computeTotal` the arm has that its twin
/// cannot: `import x = require(…)` has no ESM spelling, so the CommonJS arm
/// carries one caller more.
fn assert_shared_answer_key(scan: &Scan, only_here: &[&str]) {
    // A destructured binding, a member of a require, and a name reached
    // through a barrel all land on the same definition.
    let mut total_callers = set(&["auditOrder", "handleOrder", "summarise"]);
    total_callers.extend(set(only_here));
    assert_eq!(scan.callers_of("computeTotal"), total_callers);
    // A whole-module binding: `helpers.applyDiscount()`.
    assert_eq!(scan.callers_of("applyDiscount"), set(&["summarise"]));
    // A renamed binding on both sides — the local name matches neither the
    // published name nor the declaring one, so only the export table joins them.
    assert_eq!(scan.callers_of("formatMoneyImpl"), set(&["renderReceipt"]));
    assert_eq!(scan.callers_of("applyTax"), set(&["renderReceipt"]));
    // The barrel itself declares nothing, so no edge may stop there.
    assert!(
        scan.files
            .values()
            .all(|paths| !paths.iter().any(|path| path.ends_with("barrel.js"))),
        "an edge must point past the barrel at the declaring module"
    );
}

#[test]
fn require_bindings_record_caller_edges() {
    let scan = scan("commonjs");
    assert_shared_answer_key(&scan, &["legacyTotal"]);
    // `import helpers = require("./helpers")`, which only TypeScript writes.
    assert_eq!(scan.callees_of("legacyTotal"), set(&["computeTotal"]));
}

#[test]
fn the_esm_control_arm_answers_the_same_key() {
    assert_shared_answer_key(&scan("esm"), &[]);
}

#[test]
fn a_computed_require_records_no_edge_and_is_reported() {
    let scan = scan("commonjs");
    assert_eq!(
        scan.callees_of("runPlugin"),
        BTreeSet::new(),
        "nothing says which module a computed specifier loads"
    );
    // Found by what the line is about, not by a ticket ref: the line is cut to
    // what a reader acts on (carrick#1273).
    let report = scan
        .stderr
        .lines()
        .find(|line| line.contains("computed specifier"))
        .unwrap_or_else(|| panic!("no computed-require report on stderr:\n{}", scan.stderr));
    assert!(
        report.contains("1 require() call(s) take a computed specifier"),
        "the report must count them: {report}"
    );
    assert!(
        report.ends_with("Write the path as a string literal."),
        "and say what to do about it: {report}"
    );
}
