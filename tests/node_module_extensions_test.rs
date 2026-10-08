//! `.mjs`, `.cjs`, `.mts` and `.cts` files are source (carrick#904).
//!
//! Drives the real scanner binary offline over the two variants in
//! `tests/fixtures/node-module-extensions/`, reading the WRITTEN BLOB:
//!
//! - `javascript/` holds only `.mjs` and `.cjs` source: an ES module serving
//!   an HTTP route that calls into a CommonJS module. Before the fix its walk
//!   found no source and the scan stopped at discovery.
//! - `typescript/` holds `.mts` and `.cts` source, whose type annotations parse
//!   only when the file is read as TypeScript, and a `.d.mts` declaration file.
//!
//! The extension names how Node loads a module, not what the scanner reads in
//! it, so each file must answer exactly as its `.js`/`.ts` twin would.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

fn fixture_dir(variant: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/node-module-extensions")
        .join(variant)
}

struct Scan {
    /// Definition name -> the file it is declared in.
    defined_in: BTreeMap<String, String>,
    /// Definition name -> the callee names its edges record.
    calls: BTreeMap<String, BTreeSet<String>>,
}

fn scan(variant: &str) -> Scan {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    // No cassettes: every analyzer call answers empty. Definitions and call
    // edges are deterministic, so none are needed.
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
    let mut defined_in = BTreeMap::new();
    let mut calls = BTreeMap::new();
    for definition in definitions.values() {
        let name = definition["name"].as_str().unwrap_or_default().to_string();
        let file = definition["file_path"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let callees = definition["calls"]
            .as_array()
            .map(|edges| {
                edges
                    .iter()
                    .filter_map(|call| call["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        defined_in.insert(name.clone(), file);
        calls.insert(name, callees);
    }
    Scan { defined_in, calls }
}

fn assert_defined_in(scan: &Scan, name: &str, file: &str) {
    let found = scan
        .defined_in
        .get(name)
        .unwrap_or_else(|| panic!("no definition of {name}; got {:?}", scan.defined_in));
    assert!(
        found.ends_with(file),
        "{name} should be defined in {file}, got {found}"
    );
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_service_of_only_mjs_and_cjs_source_is_discovered_and_indexed() {
    let scan = scan("javascript");
    assert_defined_in(&scan, "handleOrder", "src/server.mjs");
    assert_defined_in(&scan, "startServer", "src/server.mjs");
    assert_defined_in(&scan, "priceOrder", "src/pricing.cjs");
    assert_defined_in(&scan, "describePrice", "src/pricing.cjs");
    // An ES-module import of a CommonJS export joins at its definition.
    assert_eq!(
        scan.calls.get("handleOrder").cloned().unwrap_or_default(),
        set(&["priceOrder"])
    );
    // `x.test.mjs` is a test file, as `x.test.js` is.
    assert!(
        !scan.defined_in.contains_key("checkPriceOrder"),
        "a .test.mjs file must be left out: {:?}",
        scan.defined_in
    );
}

#[test]
fn mts_and_cts_source_is_read_as_typescript() {
    let scan = scan("typescript");
    assert_defined_in(&scan, "quoteOrder", "src/server.mts");
    assert_defined_in(&scan, "roundCents", "src/rounding.cts");
    // `./rounding.cjs` names the emitted file; the source is `rounding.cts`.
    assert_eq!(
        scan.calls.get("quoteOrder").cloned().unwrap_or_default(),
        set(&["roundCents"])
    );
}
