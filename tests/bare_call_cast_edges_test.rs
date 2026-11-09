//! carrick#3446: a bare call (`formatTotal(c)`) the resolver by name cannot
//! place takes its call-graph edge from the compiler's `call` row. The
//! binding is `const { formatTotal } = require("./vendor/money") as typeof
//! import("../common/money")`: the required path names no file, the cast
//! does, and only the compiler reads it. A require that resolves wins over
//! its cast; an uncast require of a missing path, and a parameter typed
//! `typeof formatTotal`, state no edge.
//!
//! Scanned as the Action scans it, two services that both include
//! `common/`; the real sidecar walks the rows. See
//! `tests/fixtures/bare-call-through-cast/`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bare-call-through-cast")
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

/// One edge of a blob: (service, caller, callee, callee file, call line).
/// Edges to one callee fold to the first call's line, so a second call that
/// took an edge it should not have moves the line, never adds a row.
type Edge = (String, String, String, String, u64);

/// Every edge of every service's blob.
fn scan_edges() -> BTreeSet<Edge> {
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
    let mut edges = BTreeSet::new();
    let mut services = BTreeSet::new();
    for path in std::fs::read_dir(storage.path())
        .expect("storage dir")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
    {
        let blob: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read blob")).expect("parse blob");
        let service = blob["service_name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        services.insert(service.clone());
        for (caller, definition) in blob["function_definitions"]
            .as_object()
            .expect("blob has function_definitions")
        {
            for call in definition["calls"].as_array().into_iter().flatten() {
                edges.insert((
                    service.clone(),
                    caller.clone(),
                    call["name"].as_str().unwrap_or_default().to_string(),
                    call["file_path"].as_str().unwrap_or_default().to_string(),
                    call["call_site_line"].as_u64().unwrap_or_default(),
                ));
            }
        }
    }
    assert_eq!(
        services,
        BTreeSet::from(["billing".to_string(), "orders".to_string()]),
        "one blob per service"
    );
    edges
}

/// The edge `caller` -> `callee` in `common/money.ts`, if the scan stated it:
/// its call line.
fn into_money(edges: &BTreeSet<Edge>, caller: &str, callee: &str) -> Option<u64> {
    edges
        .iter()
        .find(|(_, from, to, file, _)| {
            from == caller && to == callee && file.ends_with("common/money.ts")
        })
        .map(|(.., line)| *line)
}

#[test]
fn a_bare_call_the_resolver_cannot_place_takes_its_edge_from_the_call_row() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let edges = scan_edges();

    // Known positive: an ESM import the resolver places.
    assert_eq!(
        into_money(&edges, "invoice", "formatTotal"),
        Some(4),
        "the scan stated {edges:#?}"
    );
    // A destructured require of a path that names no file, read through its
    // `typeof import` cast.
    assert_eq!(
        into_money(&edges, "checkout", "formatTotal"),
        Some(6),
        "the scan stated {edges:#?}"
    );
    // A require that resolves wins over a cast naming another file.
    assert_eq!(
        into_money(&edges, "viaLocal", "formatTotal"),
        Some(3),
        "the scan stated {edges:#?}"
    );
    // The member call through the cast whole-module binding (line 8) is the
    // only `roundCents` edge: the uncast require of a missing path (line 7)
    // names nothing, so the edge keeps line 8.
    assert_eq!(
        into_money(&edges, "checkout", "roundCents"),
        Some(8),
        "the scan stated {edges:#?}"
    );
    // A parameter typed `typeof formatTotal` is no caller of it.
    assert!(
        !edges.iter().any(|(_, caller, ..)| caller == "applyFee"),
        "a value typed as a function is not that function: {edges:#?}"
    );
    // Nothing reaches the decoy the losing cast names.
    assert!(
        !edges
            .iter()
            .any(|(_, _, _, file, ..)| file.ends_with("orders/decoy.ts")),
        "a cast that loses to a resolvable require names no callee: {edges:#?}"
    );
}
