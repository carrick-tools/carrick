//! The config a derived service names is the one that governs it (carrick#3497).
//!
//! Drives the real scanner binary offline over the workspace layouts in
//! `tests/fixtures/governing-config/` (see its README) and reads
//! `function_definitions[].calls` from the WRITTEN BLOB, the field
//! `get_callers` inverts. `viaAlias` imports `get` through `@/lib/api`;
//! `viaRelative` imports it through `./lib/api`.
//!
//! `a-ws`: the derived service once named the workspace root's
//! `tsconfig.json` (`{}`), so neither the type sidecar nor the scanner read
//! the member's `paths`. `a-cj`, `a-solo`, `b-ws`, `b-solo` and `refs-named`
//! are the layouts the scanner's own rule missed before it took the sidecar's
//! (carrick#3497 S2). `pos-ws`, `j-only` and `j-under-cj` are controls.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

/// Caller key -> the callee names its edges record.
fn scan_calls(variant: &str) -> BTreeMap<String, BTreeSet<String>> {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let cassettes = tempfile::tempdir().expect("temp cassette dir");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/governing-config")
        .join(variant);

    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(fixture)
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
    assert!(
        output.status.success(),
        "{variant} scan exited non-zero:\n{}",
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

/// Whether `caller` (a function name) records an edge to `get`.
fn calls_get(calls: &BTreeMap<String, BTreeSet<String>>, caller: &str) -> bool {
    calls
        .iter()
        .any(|(key, callees)| key.starts_with(caller) && callees.contains("get"))
}

#[test]
fn alias_declared_in_the_member_root_app_config_resolves() {
    let calls = scan_calls("a-ws");
    assert!(calls_get(&calls, "viaRelative"), "control: {calls:?}");
    assert!(calls_get(&calls, "viaAlias"), "alias edge: {calls:?}");
}

#[test]
fn alias_in_the_member_tsconfig_json_resolves() {
    let calls = scan_calls("pos-ws");
    assert!(calls_get(&calls, "viaRelative"), "control: {calls:?}");
    assert!(calls_get(&calls, "viaAlias"), "alias edge: {calls:?}");
}

#[test]
fn alias_in_a_member_jsconfig_resolves() {
    let calls = scan_calls("j-only");
    assert!(calls_get(&calls, "viaRelative"), "control: {calls:?}");
    assert!(calls_get(&calls, "viaAlias"), "alias edge: {calls:?}");
}

/// The service names no config: the rule finds `tsconfig.app.json` at its
/// root (step 2) rather than the nearest `tsconfig.json`, the root's `{}`.
#[test]
fn a_named_service_without_a_tsconfig_reads_its_root_app_config() {
    let calls = scan_calls("a-cj");
    assert!(calls_get(&calls, "viaRelative"), "control: {calls:?}");
    assert!(calls_get(&calls, "viaAlias"), "alias edge: {calls:?}");
}

/// The service scanned alone: `tsconfig.app.json` is its only config, and no
/// directory holds a `tsconfig.json`.
#[test]
fn a_lone_app_config_governs_the_repo() {
    let calls = scan_calls("a-solo");
    assert!(calls_get(&calls, "viaRelative"), "control: {calls:?}");
    assert!(calls_get(&calls, "viaAlias"), "alias edge: {calls:?}");
}

/// The member's `tsconfig.json` lists nothing and references
/// `tsconfig.app.json`, which lists the file: the reference owns it (step 5).
#[test]
fn a_referencing_member_config_hands_the_file_to_its_reference() {
    let calls = scan_calls("b-ws");
    assert!(calls_get(&calls, "viaRelative"), "control: {calls:?}");
    assert!(calls_get(&calls, "viaAlias"), "alias edge: {calls:?}");
}

#[test]
fn a_referencing_config_scanned_alone_hands_the_file_to_its_reference() {
    let calls = scan_calls("b-solo");
    assert!(calls_get(&calls, "viaRelative"), "control: {calls:?}");
    assert!(calls_get(&calls, "viaAlias"), "alias edge: {calls:?}");
}

/// A service-root `jsconfig.json` under a root `tsconfig.json` `{}`: the
/// jsconfig outranks the ancestor (step 3), as in the type sidecar.
#[test]
fn a_member_jsconfig_outranks_an_ancestor_tsconfig() {
    let calls = scan_calls("j-under-cj");
    assert!(calls_get(&calls, "viaRelative"), "control: {calls:?}");
    assert!(calls_get(&calls, "viaAlias"), "alias edge: {calls:?}");
}

/// carrick#1425: the named root config references two leaves, each with its
/// own `paths`; each leaf's file is read under its own leaf.
#[test]
fn a_named_root_config_hands_each_file_to_the_leaf_that_lists_it() {
    let calls = scan_calls("refs-named");
    for leaf in ["One", "Two"] {
        assert!(
            calls_get(&calls, &format!("viaRelative{leaf}")),
            "control {leaf}: {calls:?}"
        );
        assert!(
            calls_get(&calls, &format!("viaAlias{leaf}")),
            "alias edge {leaf}: {calls:?}"
        );
    }
}
