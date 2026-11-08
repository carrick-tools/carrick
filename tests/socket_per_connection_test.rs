//! A fact socket row takes its site from the socket pass's row of the same
//! operation, and a socket-pass row stating another operation keeps it
//! (carrick#1670 parity, K3b).
//!
//! Drives the real scanner binary on `.`, over a committed copy of
//! `tests/fixtures/socket-per-connection/`, offline, with the fixture's
//! cassettes. The fixture's `socket.io` / `socket.io-client` declarations are
//! hand-written stubs; the store's answer labels `Socket` (server) and the
//! client package's `Socket` by side, and `Namespace` as a client end, which
//! the socket pass takes for the server's. The README is the answer key.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const SERVER: &str = "services/srv/src/server.ts";
const CLIENT: &str = "services/cli/src/client.ts";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/socket-per-connection")
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "carrick-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "carrick-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// One socket row as the blob holds it: which side of the blob it is on, its
/// key, its place and its source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Row {
    side: String,
    key: String,
    file_path: String,
    source: Option<String>,
}

fn row(side: &str, key: &str, file_path: &str) -> Row {
    Row {
        side: side.to_string(),
        key: key.to_string(),
        file_path: file_path.to_string(),
        source: Some("library_claim".to_string()),
    }
}

/// The socket rows of each service of a scan of the fixture, sorted.
fn scan() -> BTreeMap<String, Vec<Row>> {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir(), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let (storage, home, cache) = (
        tmp.path().join("storage"),
        tmp.path().join("home"),
        tmp.path().join("cache"),
    );
    for dir in [&storage, &home, &cache] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(&repo)
        .env("HOME", &home)
        .env("CARRICK_LOCAL_STORAGE_DIR", &storage)
        .env("CARRICK_CACHE_DIR", &cache)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_NO_UPDATE_CHECK", "1")
        .env(
            "CARRICK_SIDECAR_DIR",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar"),
        );
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_EVENT_PATH",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        "CARRICK_OUTPUT_JSON",
        "CARRICK_NO_MODEL",
        "CARRICK_ALLOW_MISSING_TYPES",
        "CARRICK_LOCAL_STORAGE_ISOLATE",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("spawn carrick");
    assert!(
        output.status.success(),
        "the scan failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut services = BTreeMap::new();
    for entry in std::fs::read_dir(&storage).expect("storage dir") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let blob: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read blob")).expect("parse blob");
        let mut rows = Vec::new();
        for side in ["endpoints", "calls"] {
            for entry in blob[side].as_array().expect("blob side is an array") {
                let key = &entry["key"];
                if key["protocol"].as_str() != Some("socket") {
                    continue;
                }
                rows.push(Row {
                    side: side.to_string(),
                    key: format!(
                        "socket|{}|{}",
                        key["direction"].as_str().unwrap(),
                        key["event"].as_str().unwrap()
                    ),
                    file_path: entry["file_path"].as_str().expect("file_path").to_string(),
                    source: entry
                        .get("resolution_source")
                        .and_then(|source| source.as_str())
                        .map(str::to_string),
                });
            }
        }
        rows.sort();
        services.insert(blob["service_name"].as_str().unwrap().to_string(), rows);
    }
    assert_eq!(
        services.keys().map(String::as_str).collect::<Vec<_>>(),
        ["cli", "srv"],
        "every service uploads one blob"
    );
    services
}

fn scanned() -> &'static BTreeMap<String, Vec<Row>> {
    static ROWS: OnceLock<BTreeMap<String, Vec<Row>>> = OnceLock::new();
    ROWS.get_or_init(scan)
}

fn with_source(side: &str, key: &str, file_path: &str, source: &str) -> Row {
    Row {
        source: Some(source.to_string()),
        ..row(side, key, file_path)
    }
}

/// Every per-connection server emit and every client listener is the fact
/// layer's row, under the key the socket pass gave it.
#[test]
fn per_connection_socket_rows_are_the_fact_layers() {
    let scanned = scanned();
    let server = &scanned["srv"];
    for expected in [
        row(
            "endpoints",
            "socket|client_to_server|chat.in",
            &format!("{SERVER}:6"),
        ),
        row(
            "calls",
            "socket|server_to_client|chat.hello",
            &format!("{SERVER}:9"),
        ),
        row(
            "calls",
            "socket|server_to_client|chat.reply",
            &format!("{SERVER}:13"),
        ),
        row(
            "calls",
            "socket|server_to_client|chat.fan",
            &format!("{SERVER}:18"),
        ),
    ] {
        assert!(server.contains(&expected), "{expected:?} in {server:#?}");
    }
    let client = &scanned["cli"];
    for expected in [
        row(
            "calls",
            "socket|client_to_server|chat.in",
            &format!("{CLIENT}:4"),
        ),
        row(
            "endpoints",
            "socket|server_to_client|chat.hello",
            &format!("{CLIENT}:5"),
        ),
        row(
            "endpoints",
            "socket|server_to_client|chat.reply",
            &format!("{CLIENT}:6"),
        ),
        row(
            "endpoints",
            "socket|server_to_client|chat.fan",
            &format!("{CLIENT}:7"),
        ),
        row(
            "endpoints",
            "socket|server_to_client|chat.notice",
            &format!("{CLIENT}:8"),
        ),
    ] {
        assert!(client.contains(&expected), "{expected:?} in {client:#?}");
    }
}

/// The fact layer reads `Namespace` as a client end, the socket pass as the
/// server's: the socket pass's row stays, and the key is the one it gave.
#[test]
fn a_disagreeing_socket_pass_row_stands_and_is_counted() {
    let server = &scanned()["srv"];
    let expected = with_source(
        "calls",
        "socket|server_to_client|chat.notice",
        &format!("{SERVER}:23"),
        "socket_call",
    );
    assert!(server.contains(&expected), "{expected:?} in {server:#?}");
    assert!(
        !server
            .iter()
            .any(|row| row.key == "socket|client_to_server|chat.notice"),
        "the fact's disagreeing key states no row: {server:#?}"
    );
}
