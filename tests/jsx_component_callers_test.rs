//! JSX elements as call edges (carrick#1149).
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/jsx-component-callers/`, a React client where one component
//! is rendered from three files: through a relative import, from an arrow
//! component (twice, one edge), and through a tsconfig `@/` alias. Reads
//! `function_definitions[].calls` from the WRITTEN BLOB, because that is the
//! field `get_callers` inverts.
//!
//! Pre-fix baseline: no JSX element was recorded as a call site at all, so
//! every positive row below FAILS on the 0.3.72 scanner. The intrinsic
//! `<nav>` beside an exported `nav` function is the answer key for what must
//! still record no edge.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jsx-component-callers")
}

/// Caller key -> the callee names its edges record.
fn scan_calls() -> BTreeMap<String, BTreeSet<String>> {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    // No cassettes: every analyzer call answers empty, and call edges are
    // deterministic, so none are needed.
    let cassettes = tempfile::tempdir().expect("temp cassette dir");

    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(fixture_dir())
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.path().display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        // The assertion is on call edges; the type layer is not under test
        // and the fixture has no prepared install.
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
    assert!(
        output.status.success(),
        "fixture scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
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

    blob["function_definitions"]
        .as_object()
        .expect("blob has no function_definitions")
        .iter()
        .map(|(key, def)| {
            let callees = def["calls"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|call| call["name"].as_str().map(str::to_owned))
                .collect();
            (key.clone(), callees)
        })
        .collect()
}

fn callers_of(calls: &BTreeMap<String, BTreeSet<String>>, callee: &str) -> BTreeSet<String> {
    calls
        .iter()
        .filter(|(_, callees)| callees.contains(callee))
        .map(|(caller, _)| caller.clone())
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_component_rendered_in_three_files_has_three_callers() {
    let calls = scan_calls();
    assert_eq!(
        callers_of(&calls, "PriceTag"),
        set(&["CartPage", "CheckoutSummary", "ProductPage"])
    );
    // `<Icons.Star />` through a namespace import.
    assert_eq!(callers_of(&calls, "Star"), set(&["ProductPage"]));
    // `<nav>` is the host element even beside a function named `nav`.
    assert!(
        callers_of(&calls, "nav").is_empty(),
        "an intrinsic element must not resolve to a same-named function"
    );
    assert!(
        callers_of(&calls, "Cart").is_empty(),
        "an unrendered component has no callers"
    );
}
