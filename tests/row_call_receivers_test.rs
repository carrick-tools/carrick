//! carrick#813: a member call the name resolver left without a target takes
//! its call-graph edge from the compiler's `call` row, whatever the receiver
//! is: the result of a method call (awaited or not), a property chain, a
//! destructured or union-typed binding, an optional call. A receiver typed
//! `any` states no edge. An interface member takes an edge to each
//! implementation (`row_call_implementations_test.rs`).
//!
//! Scanned as the Action scans it; the real sidecar walks the rows. See `tests/fixtures/row-call-receivers/`. Each
//! method of `Client` wraps a different protocol's stub transport (HTTP,
//! GraphQL, socket, queue, RPC); the edges are function to function, so the
//! protocol does not change them.

use std::collections::BTreeSet;
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

/// Every edge of the scan's blob: (caller, callee, callee file, call line).
fn scan_edges() -> BTreeSet<(String, String, String, u64)> {
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
    let blob: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&blob).expect("read blob")).expect("parse blob");
    let mut edges = BTreeSet::new();
    for (caller, definition) in blob["function_definitions"]
        .as_object()
        .expect("blob has function_definitions")
    {
        for call in definition["calls"].as_array().into_iter().flatten() {
            edges.insert((
                caller.clone(),
                call["name"].as_str().unwrap_or_default().to_string(),
                call["file_path"].as_str().unwrap_or_default().to_string(),
                call["call_site_line"].as_u64().unwrap_or_default(),
            ));
        }
    }
    edges
}

type Edge = (String, String, String, u64);

fn has(edges: &BTreeSet<Edge>, caller: Option<&str>, callee: &str, line: u64) -> bool {
    edges.iter().any(|(from, name, file, at)| {
        name == callee
            && file == "src/client.ts"
            && *at == line
            && caller.is_none_or(|wanted| wanted == from)
    })
}

#[test]
fn every_member_call_the_resolver_left_takes_its_edge_from_the_call_row() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let edges = scan_edges();
    for (caller, callee, line) in [
        // `const c = await ctx.getClient(); c.getUser()` (HTTP).
        (Some("viaMethodFactory"), "Client.getUser", 7),
        // The same through `this.getClient()` (GraphQL).
        (Some("Worker.viaThisMethodFactory"), "Client.runQuery", 13),
        // `holder.client.notify()` (socket).
        (Some("viaPropertyChain"), "Client.notify", 18),
        // `this.options.client.enqueue()` (queue).
        (Some("Chained.viaThisChain"), "Client.enqueue", 25),
        // `{ client }: Deps` (RPC).
        (Some("viaDestructured"), "Client.callProcedure", 30),
        // A call's value inside an anonymous callback: the index names the
        // anonymous caller itself.
        (None, "Client.getUser", 34),
        // `let current: Client | null`, read as `current!.runQuery()`.
        (Some("viaUnion"), "Client.runQuery", 39),
        // `current?.notify()`.
        (Some("viaOptional"), "Client.notify", 43),
        // An unannotated callback parameter.
        (None, "Client.enqueue", 46),
    ] {
        assert!(
            has(&edges, caller, callee, line),
            "missing {caller:?} -> {callee} at line {line}; the scan stated {edges:#?}"
        );
    }
}

#[test]
fn an_untyped_receiver_states_no_edge_and_an_interface_member_takes_its_implementation() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let edges = scan_edges();
    assert!(
        !edges.iter().any(|(from, ..)| from == "viaAny"),
        "viaAny states no edge: {edges:#?}"
    );
    // `store.read()` with `store: Store`: the closure the factory returns is
    // the one implementation, so the call reaches it.
    assert!(
        edges
            .iter()
            .any(|(from, name, file, at)| from == "viaInterface"
                && name == "read"
                && file == "src/store.ts"
                && *at == 49),
        "viaInterface reaches the one implementation of Store.read: {edges:#?}"
    );
}
