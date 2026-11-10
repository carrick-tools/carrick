//! carrick#2615: a member call on a `const` receiver a call initialised
//! (`const client = make("p1"); client.list()`) takes its call-graph edge
//! from the compiler's `call` row, whatever declares what `make` returns: an
//! inline function type, an alias another file declares, or an interface
//! member reached through a destructured parameter. A receiver the compiler
//! types `any` states no edge.
//!
//! Scanned as the Action scans it; the real sidecar walks the rows. `src/api-client.ts` declares the class, the
//! other files call it. See `tests/fixtures/row-call-edges/`.
//!
//! carrick#1910: the same edge where the receiver's class comes from a
//! workspace package whose declared types were never built
//! (`tests/fixtures/unbuilt-workspace/`).

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

/// Every edge of the scan's blob: (caller, callee, callee file, call line).
fn scan_edges(fixture: &str) -> BTreeSet<(String, String, String, u64)> {
    edges_of(&scan(fixture))
}

/// Scan `fixture` as the Action scans it, into a store of its own.
fn scan(fixture: &str) -> tempfile::TempDir {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(fixture_dir(fixture))
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
    storage
}

/// Every edge of the blob in `storage`.
fn edges_of(storage: &tempfile::TempDir) -> BTreeSet<(String, String, String, u64)> {
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

fn edge(caller: &str, callee: &str, line: u64) -> (String, String, String, u64) {
    (
        caller.to_string(),
        callee.to_string(),
        "src/api-client.ts".to_string(),
        line,
    )
}

#[test]
fn a_member_call_on_a_call_bound_receiver_takes_its_edge_from_the_call_row() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let edges = scan_edges("row-call-edges");
    for wanted in [
        // `make: (projectId: string) => ApiClient`, inline.
        edge("listInline", "ApiClient.list", 5),
        // `make: MakeClient`, an alias `src/types.ts` declares.
        edge("listAliased", "ApiClient.list", 5),
        // `{ make }: Deps`: an interface member, through a destructured
        // parameter, which no annotation at the binding names.
        edge("removeThrough", "ApiClient.remove", 5),
        // `open: OpenClient` returns a promise of the client, awaited.
        edge("listAwaited", "ApiClient.list", 5),
    ] {
        assert!(
            edges.contains(&wanted),
            "missing {wanted:?}; the scan stated {edges:#?}"
        );
    }
    assert!(
        !edges.iter().any(|(caller, ..)| caller == "listUntyped"),
        "a receiver the compiler types `any` states no edge: {edges:#?}"
    );
    assert!(
        !edges.iter().any(|(caller, ..)| caller == "listUnawaited"),
        "a promise of the client is not the client until it is awaited: {edges:#?}"
    );
}

/// carrick#1910: `tests/fixtures/unbuilt-workspace/` is a checkout an install
/// linked and nothing built. `@acme/ledger`'s `typings` names
/// `build/main/index.d.ts`, which does not exist, and the service's config
/// references no project. The scan's rows process is the one told to read
/// workspace packages from source (`SidecarRecipe::spawn_position_rows`), so
/// the call on the receiver `openLedger()` returns names the package's source
/// declaration instead of the error type. (The blob's edge for this call is
/// also read without the compiler, so the rows are what this test reads.)
#[test]
fn the_rows_process_reads_an_unbuilt_workspace_package_from_its_source() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let fixture = fixture_dir("unbuilt-workspace");
    assert!(
        !fixture.join("packages/ledger/build").exists(),
        "the package must stay unbuilt"
    );
    let storage = scan("unbuilt-workspace");
    let rows = std::fs::File::open(storage.path().join("positions/unbuilt-workspace.jsonl.gz"))
        .expect("the scan wrote the service's rows");
    let mut text = String::new();
    std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(rows), &mut text)
        .expect("rows decompress");
    let handler = text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|line| line["f"] == "src/handler.ts")
        .expect("rows for src/handler.ts");
    let source = std::fs::read_to_string(fixture.join("src/handler.ts")).expect("handler source");
    let start = source.find("ledger.total(account)").expect("the call") as u64;
    let end = start + "ledger.total(account)".len() as u64;
    let call = handler["r"]
        .as_array()
        .expect("rows")
        .iter()
        .find(|row| row[0] == "call" && row[1] == start && row[2] == end)
        .unwrap_or_else(|| panic!("no call row at {start}-{end}: {handler}"));
    assert_eq!(call[3], "total", "{call}");
    assert_eq!(call[4], "packages/ledger/src/index.ts", "{call}");
}
