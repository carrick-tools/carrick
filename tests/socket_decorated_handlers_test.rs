//! A message handler written as a method decorator is a listener
//! (carrick#3043): `@OnMessage("lobby.enter")` on a class method, where the
//! store labels the decorator export with a `receive` op in the `decorate`
//! form, names the event in argument 0 and makes the decorated method its
//! handler.
//!
//! Drives the real scanner binary on `.`, over a committed copy of
//! `tests/fixtures/socket-decorated-handlers/`, offline, with the fixture's
//! store answer replayed. The fixture's README is the answer key.
//!
//! Protocols: socket listeners. HTTP routes, queue and message handlers and
//! GraphQL resolvers written as decorators read the same site and claim, and
//! are covered here only as far as the row writer for their role goes (a
//! broker's or a bus's `receive` is a subscriber row); a class decorator's
//! scope (a route prefix) is not read yet.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const GATEWAY: &str = "services/lobby/src/gateway.ts";
const CLIENT: &str = "services/player/src/client.ts";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/socket-decorated-handlers")
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

/// One socket row as the blob holds it: its side of the blob, its key, its
/// place and its source.
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
        ["lobby", "player"],
        "every service uploads one blob"
    );
    services
}

fn scanned() -> &'static BTreeMap<String, Vec<Row>> {
    static ROWS: OnceLock<BTreeMap<String, Vec<Row>>> = OnceLock::new();
    ROWS.get_or_init(scan)
}

/// A method decorated with the labelled decorator is the listener of the
/// event its argument names, at the method's name, client to server (the
/// export's side is the server's); the client's emit of the same event has
/// the same key. Another decorator on the same method changes nothing.
#[test]
fn a_method_decorated_with_a_labelled_message_decorator_is_a_listener() {
    let services = scanned();
    let lobby = &services["lobby"];
    let player = &services["player"];
    for (event, line, sent_at) in [("lobby.enter", 6, 6), ("lobby.chat", 12, 10)] {
        let key = format!("socket|client_to_server|{event}");
        let listener = row("endpoints", &key, &format!("{GATEWAY}:{line}"));
        assert!(lobby.contains(&listener), "{listener:?} in {lobby:#?}");
        let emit = row("calls", &key, &format!("{CLIENT}:{sent_at}"));
        assert!(player.contains(&emit), "{emit:?} in {player:#?}");
    }
}

/// A decorator the store does not label states nothing, whatever its
/// argument says: no socket row names the tracing decorator's labels, and
/// the gateway service has no socket row but its two listeners.
#[test]
fn a_decorator_with_no_label_states_nothing() {
    let lobby = &scanned()["lobby"];
    assert!(
        lobby
            .iter()
            .all(|row| !row.key.ends_with("lobby.trace") && !row.key.ends_with("lobby.idle")),
        "{lobby:#?}"
    );
    assert_eq!(lobby.len(), 2, "{lobby:#?}");
}
