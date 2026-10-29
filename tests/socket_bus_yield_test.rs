//! In-process event-bus rows and the model's pub/sub twins give way to every
//! socket row the scan states, not only the socket pass's (carrick#1670 T4).
//!
//! Drives the real scanner binary on `.`, over a committed copy of
//! `tests/fixtures/socket-bus-yield/`, offline, with the fixture's cassettes.
//! With the switches every cloud scan runs (site questions, position rows,
//! fact files, receiver trace and site payloads), a call on an instance of a
//! socket class the store labels is a decided site whose row is a socket row,
//! even where the receiver is a parameter and no library row can be read.
//! The event pass records the same `emit` as a pub/sub publisher with no
//! source; before the fix that row held the site and the socket row was
//! dropped behind it, and the model's pub/sub row for the listener's event
//! stood beside the socket row. The fixture's README is the answer key.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const ANNOUNCE: &str = "src/announce.ts";
const TWIN: &str = "src/twin.ts";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/socket-bus-yield")
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

/// One pub/sub or socket row as the blob holds it: side, key, place and
/// source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Row {
    side: String,
    key: String,
    file_path: String,
    source: Option<String>,
}

fn row(side: &str, key: &str, file_path: &str, source: Option<&str>) -> Row {
    Row {
        side: side.to_string(),
        key: key.to_string(),
        file_path: file_path.to_string(),
        source: source.map(str::to_string),
    }
}

/// The pub/sub and socket rows of a scan of the fixture, sorted, with the
/// five switches on or all off.
fn scan(switches: bool) -> Vec<Row> {
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
    let names = [
        "CARRICK_SITE_QUESTIONS",
        "CARRICK_POSITION_ROWS",
        "CARRICK_FACT_FILES",
        "CARRICK_RECEIVER_TRACE",
        "CARRICK_SITE_PAYLOADS",
    ];
    for name in names {
        if switches {
            cmd.env(name, "1");
        } else {
            cmd.env_remove(name);
        }
    }
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
    let blobs: Vec<PathBuf> = std::fs::read_dir(&storage)
        .expect("storage dir")
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert_eq!(blobs.len(), 1, "expected one written blob, got {blobs:?}");
    let blob: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&blobs[0]).expect("read blob")).expect("parse blob");
    let mut rows = Vec::new();
    for side in ["endpoints", "calls"] {
        for entry in blob[side].as_array().expect("blob side is an array") {
            let key = &entry["key"];
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
                file_path: entry["file_path"].as_str().expect("file_path").to_string(),
                source: entry
                    .get("resolution_source")
                    .and_then(|source| source.as_str())
                    .map(str::to_string),
            });
        }
    }
    rows.sort();
    rows
}

fn with_switches() -> &'static [Row] {
    static ROWS: OnceLock<Vec<Row>> = OnceLock::new();
    ROWS.get_or_init(|| scan(true))
}

/// The emit on a parameter is a socket call, stated by its site row, and no
/// pub/sub row stands for it.
#[test]
fn a_socket_site_row_holds_its_call_against_the_bus_row() {
    let rows = with_switches();
    let at = format!("{ANNOUNCE}:4");
    assert!(
        rows.contains(&row(
            "calls",
            "socket|client_to_server|chat.sent",
            &at,
            Some("library_claim")
        )),
        "the socket site row stands at {at}: {rows:#?}"
    );
    assert!(
        !rows.iter().any(|row| row.key == "pubsub|chat.sent"),
        "no pub/sub row for the socket call: {rows:#?}"
    );
}

/// The model's pub/sub row for the listener's event, at another line of the
/// same file, folds onto the socket site row as it would onto a socket pass
/// row.
#[test]
fn a_model_pubsub_twin_folds_onto_a_socket_site_row() {
    let rows = with_switches();
    let at = format!("{TWIN}:4");
    assert!(
        rows.contains(&row(
            "endpoints",
            "socket|server_to_client|chat.read",
            &at,
            Some("library_claim")
        )),
        "the socket site row stands at {at}: {rows:#?}"
    );
    assert!(
        !rows.iter().any(|row| row.key == "pubsub|chat.read"),
        "the model's pub/sub twin folded: {rows:#?}"
    );
}

/// With every switch off there are no site rows, so nothing yields: the scan
/// states what it stated before the fix, the event pass's publisher (the
/// model is not asked about `src/twin.ts` with the switches off).
#[test]
fn with_the_switches_off_the_bus_row_stands() {
    assert_eq!(
        scan(false),
        vec![row(
            "calls",
            "pubsub|chat.sent",
            &format!("{ANNOUNCE}:4"),
            None
        )]
    );
}
