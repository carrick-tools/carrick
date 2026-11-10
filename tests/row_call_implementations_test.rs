//! carrick#813, fix G: a call through a declaration with no body (an interface
//! member, a function-typed property) takes an edge to each implementation
//! the service defines, as the analysis sidecar's `implementations` action
//! names them and discovery's walk reads them (the call graph writes what
//! the walk entered, `discovered_edges_test.rs`). None the service defines
//! states no edge, and the call's unfollowed row says why.
//!
//! Scanned with `CARRICK_POSITION_ROWS=1`; the real sidecar walks the rows
//! and answers the questions. See
//! `tests/fixtures/row-call-implementations/`: each implementation wraps a
//! different protocol's stub transport (HTTP, GraphQL, socket, queue, RPC);
//! the edges are function to function, so the protocol does not change them.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
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

/// The blob the fixture's scan stores.
fn scan_blob() -> serde_json::Value {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(fixture_dir("row-call-implementations"))
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_POSITION_ROWS", "1")
        .env_remove("CARRICK_RECEIVER_TRACE");
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

/// Every edge of the scan's blob: (caller, callee, callee file, call line).
fn edges_of(blob: &serde_json::Value) -> BTreeSet<Edge> {
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

fn has(edges: &BTreeSet<Edge>, caller: &str, callee: &str, file: &str, line: u64) -> bool {
    edges
        .iter()
        .any(|e| e == &(caller.into(), callee.into(), file.into(), line))
}

fn rows_of(blob: &serde_json::Value) -> Vec<serde_json::Value> {
    blob["unfollowed_member_calls"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn row_of<'a>(rows: &'a [serde_json::Value], caller: &str) -> Option<&'a serde_json::Value> {
    rows.iter().find(|row| row["caller"] == caller)
}

#[test]
fn a_call_through_an_interface_member_takes_its_edge_from_the_one_implementation() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let blob = scan_blob();
    let edges = edges_of(&blob);
    for (caller, callee, file, line) in [
        // `store: Store` (GraphQL): the closure `read` of the factory.
        ("viaAnnotated", "read", "src/store.ts", 5),
        // `{ store }: Deps` (RPC).
        ("viaDestructured", "write", "src/store.ts", 9),
        // `holder.deps.store.read()`, a property chain.
        ("viaChain", "read", "src/store.ts", 18),
        // `current?.read()` on `Store | null`.
        ("viaUnion", "read", "src/store.ts", 23),
        // `t.send()` (HTTP): the class that implements `Transport`; the
        // double under `__tests__` is dropped.
        (
            "viaImplements",
            "HttpTransport.send",
            "src/http-transport.ts",
            27,
        ),
        // An arrow in an object literal typed by the interface (RPC).
        (
            "viaArrowEntry",
            "procedures.getUser",
            "src/procedures.ts",
            35,
        ),
        // A function-typed property, `{ publish: publishTopic }` (queue).
        (
            "viaFunctionProperty",
            "publishTopic",
            "src/procedures.ts",
            39,
        ),
    ] {
        assert!(
            has(&edges, caller, callee, file, line),
            "missing {caller} -> {callee} at line {line}; the scan stated {edges:#?}"
        );
    }
    // The calls that took an edge write no unfollowed row.
    let rows = rows_of(&blob);
    for caller in [
        "viaAnnotated",
        "viaDestructured",
        "viaChain",
        "viaUnion",
        "viaImplements",
        "viaArrowEntry",
        "viaFunctionProperty",
    ] {
        assert!(
            row_of(&rows, caller).is_none(),
            "{caller} has a row: {rows:#?}"
        );
    }
    // An abstract member is keyed as a definition, so the call already
    // reaches it without a question (the socket channel's `open` is not
    // asked): unchanged from before the implementations pass.
    assert!(has(
        &edges,
        "viaAbstract",
        "Channel.open",
        "src/ports.ts",
        31
    ));
}

#[test]
fn several_implementations_take_an_edge_each_and_none_takes_no_edge() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let blob = scan_blob();
    let edges = edges_of(&blob);
    let rows = rows_of(&blob);
    // `n.notify()`: two classes implement `Notifier`, an edge to each.
    for callee in ["LogNotifier.notify", "MailNotifier.notify"] {
        assert!(
            has(&edges, "viaSeveral", callee, "src/notifiers.ts", 43),
            "viaSeveral -> {callee}: {edges:#?}"
        );
    }
    // `r.get()`: of its two entries, a destructured binding is no
    // definition the walk enters; the function is, so one edge.
    let binding: Vec<_> = edges
        .iter()
        .filter(|(from, ..)| from == "viaBinding")
        .collect();
    assert_eq!(
        binding,
        vec![&(
            "viaBinding".to_string(),
            "get".to_string(),
            "src/reader.ts".to_string(),
            47
        )],
        "{edges:#?}"
    );
    for caller in ["viaSeveral", "viaBinding"] {
        assert!(
            row_of(&rows, caller).is_none(),
            "{caller} took its edges, so no row: {rows:#?}"
        );
    }
    for caller in ["viaOrphan", "viaAny"] {
        assert!(
            !edges.iter().any(|(from, ..)| from == caller),
            "{caller} states no edge: {edges:#?}"
        );
    }
    // The one implementation of `Store.remove` is a method of an object
    // literal, which the extractor keys no definition for: the call joins to
    // nothing, so it stays unfollowed.
    assert!(
        !edges
            .iter()
            .any(|(from, _, _, line)| from == "viaFactory" && *line == 14),
        "viaFactory's `remove` call states no edge: {edges:#?}"
    );
    for (caller, reason, declared_at) in [
        ("viaOrphan", "interface_member", Some("src/ports.ts:25")),
        ("viaFactory", "interface_member", Some("src/ports.ts:6")),
        ("viaAny", "untyped_receiver", None),
    ] {
        let row = row_of(&rows, caller).unwrap_or_else(|| panic!("{caller} has a row: {rows:#?}"));
        assert_eq!(row["reason"], reason, "{caller}: {row}");
        assert_eq!(row["declared_at"].as_str(), declared_at, "{caller}: {row}");
    }
}
