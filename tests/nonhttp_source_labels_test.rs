//! carrick#1626: what a pub/sub or socket row states about its source, its
//! producer evidence and its line, read off the blob a scan writes.
//!
//! Drives the real scanner binary, offline, over
//! `tests/fixtures/nonhttp-source-labels/`, with the model's answers replayed
//! from `__llm__/`. The fixture's README is the answer key.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nonhttp-source-labels")
}

/// One pub/sub or socket row as the blob holds it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Row {
    side: String,
    key: String,
    file_path: String,
    resolution_source: Option<String>,
    provenance: String,
}

struct Scan {
    rows: Vec<Row>,
    stderr: String,
}

/// A scan that stores its blob in `storage` and reads the previous one back
/// from there, so a second call takes the incremental path.
fn scan_in(storage: &Path, cache: &Path) -> Scan {
    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(fixture_dir())
        .env("CARRICK_LOCAL_STORAGE_DIR", storage)
        .env("CARRICK_CACHE_DIR", cache)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", fixture_dir().join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        // Nothing here reads a type.
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
        "fixture scan exited non-zero:\n{stderr}"
    );

    let blobs = std::fs::read_dir(storage)
        .expect("storage dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    assert_eq!(blobs.len(), 1, "expected one written blob, got {blobs:?}");
    let blob: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&blobs[0]).expect("read blob")).expect("parse blob");

    let mut rows = Vec::new();
    for side in ["endpoints", "calls"] {
        for row in blob[side].as_array().expect("blob side is an array") {
            let key = &row["key"];
            let key = match key["protocol"].as_str() {
                Some("pubsub") => format!("pubsub|{}", key["topic"].as_str().unwrap()),
                Some("socket") => format!(
                    "socket|{}|{}",
                    key["direction"].as_str().unwrap(),
                    key["event"].as_str().unwrap()
                ),
                _ => continue,
            };
            rows.push(Row {
                side: side.to_string(),
                key,
                file_path: row["file_path"].as_str().expect("file_path").to_string(),
                resolution_source: row
                    .get("resolution_source")
                    .and_then(|source| source.as_str())
                    .map(str::to_string),
                provenance: row["provenance"].as_str().unwrap_or("route").to_string(),
            });
        }
    }
    rows.sort();
    Scan { rows, stderr }
}

fn row(side: &str, key: &str, file_path: &str, source: Option<&str>, provenance: &str) -> Row {
    Row {
        side: side.to_string(),
        key: key.to_string(),
        file_path: file_path.to_string(),
        resolution_source: source.map(str::to_string),
        provenance: provenance.to_string(),
    }
}

/// The README's answer key.
fn expected() -> Vec<Row> {
    let mut rows = vec![
        row(
            "calls",
            "pubsub|orders.created",
            "src/orders.ts:6",
            Some("model"),
            "route",
        ),
        row(
            "endpoints",
            "pubsub|orders.cancelled",
            "src/orders.ts:10",
            None,
            "route",
        ),
        row(
            "endpoints",
            "pubsub|orders.shipped",
            "src/orders.ts:15",
            None,
            "route",
        ),
        row(
            "endpoints",
            "pubsub|payments.settled",
            "src/mocks/broker.ts:5",
            None,
            "mock",
        ),
        row(
            "endpoints",
            "socket|client_to_server|chat:send",
            "src/realtime.ts:7",
            None,
            "route",
        ),
    ];
    rows.sort();
    rows
}

/// The model's row says `model`; the rows the scanner's passes read state no
/// source; a producer under a mock tree is tagged; a chained call sits on its
/// method's line; a socket listener is not also a pub/sub row.
#[test]
fn each_row_states_its_source_provenance_and_line() {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let scan = scan_in(storage.path(), cache.path());
    assert_eq!(scan.rows, expected(), "{}", scan.stderr);
}

/// The second scan of an unchanged tree replays the model's cached answers on
/// the incremental path. The backfill marker is never cached, so the
/// backfill runs again and the scanner's rows still state no source.
#[test]
fn a_rescan_states_the_same_rows() {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let first = scan_in(storage.path(), cache.path());
    let second = scan_in(storage.path(), cache.path());
    assert!(
        second.stderr.contains("already analysed"),
        "the second scan did not reuse the cached answers, so it did not take the incremental path:\n{}",
        second.stderr
    );
    assert_eq!(first.rows, expected());
    assert_eq!(second.rows, expected());
}
