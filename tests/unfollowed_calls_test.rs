//! carrick#813: a member call the scan could not tie to a definition is
//! written on the blob as `unfollowed_member_calls`, with the reason, so
//! `get_callers` can say "not followed" rather than nothing. A call that took
//! an edge, and a call into a library, write no row. Without the position
//! rows neither key is on the blob.
//!
//! Scanned as the Action scans it, with the real sidecar, over the fixture
//! `tests/fixtures/row-call-receivers/` (the receivers shapes of
//! `row_call_receivers_test.rs`): `viaInterface` calls a member declared on an
//! interface, `viaAny` calls one on a value typed `any`.

use std::path::PathBuf;
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/row-call-receivers")
}

/// Whether the live sidecar can run. CI builds it before the Rust tests, so
/// a skip there would make a green tick mean nothing.
fn sidecar_available() -> bool {
    let node = Command::new("node").arg("--version").output().is_ok();
    let built = std::env::var_os("CARRICK_SIDECAR_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar"))
        .join("dist/src/index.js")
        .exists();
    assert!(
        (node && built) || std::env::var("CI").is_err(),
        "node or the built sidecar is missing in CI, so this test would silently not run"
    );
    node && built
}

/// The blob the fixture scan stores.
fn scan_blob() -> serde_json::Value {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(fixture_dir())
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_SKIP_INTENTS", "1");
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
        "CARRICK_NO_MODEL",
        "CARRICK_ALLOW_MISSING_TYPES",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("failed to spawn carrick");
    assert!(
        output.status.success(),
        "fixture scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let blob = std::fs::read_dir(storage.path())
        .expect("storage dir")
        .map(|entry| entry.expect("dir entry").path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .expect("the scan stored a blob");
    serde_json::from_slice(&std::fs::read(&blob).expect("read blob")).expect("parse blob")
}

fn rows_of(blob: &serde_json::Value) -> Vec<serde_json::Value> {
    blob["unfollowed_member_calls"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn row_by_caller<'a>(rows: &'a [serde_json::Value], caller: &str) -> Vec<&'a serde_json::Value> {
    rows.iter().filter(|row| row["caller"] == caller).collect()
}

#[test]
fn an_interface_member_and_an_untyped_receiver_are_written_with_their_reason() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let blob = scan_blob();
    let rows = rows_of(&blob);

    // `store.read()` with `store: Store`: an interface declared at line 2 of
    // `src/store.ts`, whose one implementation is a closure the service
    // defines. It takes that edge, so it writes no row
    // (`row_call_implementations_test.rs` pins the `interface_member` row of
    // an interface nothing implements).
    assert!(
        row_by_caller(&rows, "viaInterface").is_empty(),
        "viaInterface took an edge, so no row: {rows:#?}"
    );

    // `c.getUser()` with `c: any`: the compiler names nothing.
    let untyped = row_by_caller(&rows, "viaAny");
    assert_eq!(untyped.len(), 1, "one row for viaAny: {rows:#?}");
    assert_eq!(
        untyped[0],
        &serde_json::json!({
            "file": "src/callers.ts",
            "line": 53,
            "caller": "viaAny",
            "member": "getUser",
            "receiver": "c",
            "reason": "untyped_receiver",
        })
    );

    // Sorted by file then line, and the total is the whole count.
    let at: Vec<(String, u64)> = rows
        .iter()
        .map(|row| {
            (
                row["file"].as_str().unwrap_or_default().to_string(),
                row["line"].as_u64().unwrap_or_default(),
            )
        })
        .collect();
    let mut sorted = at.clone();
    sorted.sort();
    assert_eq!(at, sorted, "rows sort by file then line");
    assert_eq!(
        blob["unfollowed_member_calls_total"].as_u64(),
        Some(rows.len() as u64)
    );
}

#[test]
fn a_call_that_has_an_edge_or_reaches_a_library_writes_no_row() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let blob = scan_blob();
    let rows = rows_of(&blob);

    // Every call the blob states an edge for, by caller and call line.
    let mut edges = 0;
    for (caller, definition) in blob["function_definitions"]
        .as_object()
        .expect("blob has function_definitions")
    {
        for call in definition["calls"].as_array().into_iter().flatten() {
            edges += 1;
            let line = call["call_site_line"].as_u64().unwrap_or_default();
            assert!(
                !rows
                    .iter()
                    .any(|row| row["caller"] == caller.as_str() && row["line"] == line),
                "{caller} line {line} has an edge and no row: {rows:#?}"
            );
        }
    }
    assert!(edges > 0, "the fixture states edges for this to bind on");

    // `xs.map(...)` on an array: the default library, nothing to follow.
    assert!(
        row_by_caller(&rows, "viaLibrary").is_empty(),
        "a library call writes no row: {rows:#?}"
    );
}
