//! The call graph writes an edge to each function of the repository
//! discovery's walk entered from a call that neither resolution by name nor
//! the compiler's rows give a target: a value callee, to each function its
//! callers pass; an interface member, to each implementation. Written once
//! the intents are in, so no intent's input moves.
//!
//! Scanned as the Action scans it (path ".", the root service), with
//! `CARRICK_POSITION_ROWS=1`; the real sidecar walks the rows and answers the
//! implementations question. See `tests/fixtures/discovered-edges/`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/discovered-edges")
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

/// The blob the fixture's scan stores. `intents` runs the (mocked) intent
/// pass, so each definition carries the hash of its intent's inputs.
fn scan_blob(intents: bool) -> serde_json::Value {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(fixture_dir())
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_POSITION_ROWS", "1")
        .env_remove("CARRICK_SKIP_INTENTS")
        .env_remove("CARRICK_RECEIVER_TRACE");
    if !intents {
        cmd.env("CARRICK_SKIP_INTENTS", "1");
    }
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

/// (caller, callee, callee file, call line).
type Edge = (String, String, String, u64);

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

fn edges_from(edges: &BTreeSet<Edge>, caller: &str) -> Vec<Edge> {
    edges
        .iter()
        .filter(|(from, ..)| from == caller)
        .cloned()
        .collect()
}

fn edge(caller: &str, callee: &str, file: &str, line: u64) -> Edge {
    (caller.into(), callee.into(), file.into(), line)
}

#[test]
fn a_value_callee_takes_an_edge_to_each_function_its_callers_pass() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let edges = edges_of(&scan_blob(false));
    // `deps.get("a")`, `deps` filled by `placeOrder`'s `{ get: loadOrder }`.
    assert!(
        edges.contains(&edge("runWith", "loadOrder", "src/values.ts", 7)),
        "runWith -> loadOrder: {edges:#?}"
    );
    // `get("b")`, `get` filled by `makeLoader()`, which returns `loadInvoice`.
    assert!(
        edges.contains(&edge("runLoader", "loadInvoice", "src/values.ts", 24)),
        "runLoader -> loadInvoice: {edges:#?}"
    );
}

#[test]
fn an_interface_member_call_takes_an_edge_to_each_implementation() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let edges = edges_of(&scan_blob(false));
    assert_eq!(
        edges_from(&edges, "writeAll"),
        vec![
            edge("writeAll", "FileSink.put", "src/sinks.ts", 6),
            edge("writeAll", "QueueSink.put", "src/sinks.ts", 6),
        ],
        "two implementations, two edges: {edges:#?}"
    );
    assert_eq!(
        edges_from(&edges, "mailOne"),
        vec![edge("mailOne", "SmtpMailer.send", "src/sinks.ts", 11)],
        "one implementation, one edge: {edges:#?}"
    );
}

#[test]
fn a_call_the_rows_resolve_keeps_its_one_edge() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let edges = edges_of(&scan_blob(false));
    assert_eq!(
        edges_from(&edges, "plainCall"),
        vec![edge("plainCall", "loadOrder", "src/values.ts", 33)],
        "{edges:#?}"
    );
    assert_eq!(
        edges_from(&edges, "writeFile"),
        vec![edge("writeFile", "FileSink.put", "src/sinks.ts", 17)],
        "{edges:#?}"
    );
}

#[test]
fn a_call_that_took_a_discovered_edge_leaves_the_unfollowed_list() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let blob = scan_blob(false);
    let rows = blob["unfollowed_member_calls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for caller in ["writeAll", "mailOne", "runWith"] {
        assert!(
            rows.iter().all(|row| row["caller"] != caller),
            "{caller} took an edge, so no row: {rows:#?}"
        );
    }
}

/// The hash of each definition's intent inputs, as a scan with no
/// discovered edges writes them (`intent-hashes.expected.json`, written by
/// the scanner before the call graph read discovery's walk). The edges are
/// written after the intents, so none moves. An `INTENT_CACHE_VERSION` bump
/// moves every one: rewrite the file then, and only then.
#[test]
fn discovered_edges_move_no_intent_hash() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let blob = scan_blob(true);
    let hashes: BTreeMap<String, String> = blob["function_definitions"]
        .as_object()
        .expect("blob has function_definitions")
        .iter()
        .filter_map(|(key, definition)| {
            Some((
                key.clone(),
                definition["intent_input_hash"].as_str()?.to_string(),
            ))
        })
        .collect();
    let expected: BTreeMap<String, String> = serde_json::from_slice(
        &std::fs::read(fixture_dir().join("intent-hashes.expected.json"))
            .expect("read the expected hashes"),
    )
    .expect("parse the expected hashes");
    assert!(!expected.is_empty(), "the answer key holds hashes");
    assert_eq!(hashes, expected);
    // The edges are there all the same.
    assert!(edges_of(&blob).contains(&edge("runWith", "loadOrder", "src/values.ts", 7)));
}

/// A named function handed to a library call whose declared return is the
/// result of the function it is handed (the compiler's "runs handed" row)
/// is walked as a function written there is: the caller takes an edge to it
/// at that call (carrick#3442).
#[test]
fn a_named_function_a_runner_is_handed_takes_an_edge() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let edges = edges_of(&scan_blob(false));
    assert_eq!(
        edges_from(&edges, "chained"),
        vec![edge("chained", "handle", "src/runners.ts", 10)],
        "`pending.then(handle)`: {edges:#?}"
    );
    assert_eq!(
        edges_from(&edges, "traced"),
        vec![edge("traced", "work", "src/runners.ts", 18)],
        "`run(work)`: {edges:#?}"
    );
}

/// A named function handed to a library call that keeps it (a listener
/// registration: its row does not say it runs what it is handed) takes no
/// edge; a named function an in-repo callee runs takes its edge at the
/// callee's own call, as before, and none at the call that hands it over.
#[test]
fn a_named_listener_registration_takes_no_edge() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let edges = edges_of(&scan_blob(false));
    assert_eq!(
        edges_from(&edges, "subscribe"),
        Vec::<Edge>::new(),
        "{edges:#?}"
    );
    assert_eq!(
        edges_from(&edges, "viaLocal"),
        vec![edge("viaLocal", "runLocal", "src/runners.ts", 35)],
        "{edges:#?}"
    );
    assert_eq!(
        edges_from(&edges, "runLocal"),
        vec![edge("runLocal", "work", "src/runners.ts", 31)],
        "{edges:#?}"
    );
}
