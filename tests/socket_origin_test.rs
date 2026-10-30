//! Origin decides a `by_origin` socket's side at a decided site
//! (carrick#1670 T2).
//!
//! Drives the real scanner binary on `.`, over a committed copy of
//! `tests/fixtures/socket-origin/`, offline, with the fixture's cassettes and
//! the switches every cloud scan runs (site questions, position rows, fact
//! files, receiver trace and site payloads). The store labels the fixture's
//! `Peer` socket `by_origin`: it has no side of its own, so the side of each
//! `Peer` call is where its instance came from. One a `Hub` hands to the
//! function it runs is the server's end; one the service makes with `new` is
//! the client's; one nothing in the service hands in has no side. The
//! fixture's README is the answer key.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const SERVER: &str = "services/hub/src/server.ts";
const CLIENT: &str = "services/peer/src/client.ts";
const CONN: &str = "services/hub/src/conn.ts";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/socket-origin")
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

/// The socket rows of each service of a scan of the fixture, sorted, with
/// the five switches on or all off.
fn scan(switches: bool) -> BTreeMap<String, Vec<Row>> {
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
    for name in [
        "CARRICK_SITE_QUESTIONS",
        "CARRICK_POSITION_ROWS",
        "CARRICK_FACT_FILES",
        "CARRICK_RECEIVER_TRACE",
        "CARRICK_SITE_PAYLOADS",
    ] {
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
        ["hub", "peer"],
        "every service uploads one blob"
    );
    services
}

fn with_switches() -> &'static BTreeMap<String, Vec<Row>> {
    static ROWS: OnceLock<BTreeMap<String, Vec<Row>>> = OnceLock::new();
    ROWS.get_or_init(|| scan(true))
}

/// Peers a hub hands to the function it runs are the server's end: written in
/// place, handed by name, and reached through a call from either. A call whose
/// op states what its handler is handed (`yields`) gives that side, whatever
/// side its own export is on.
#[test]
fn a_peer_a_hub_hands_in_is_the_servers_end() {
    let rows = &with_switches()["hub"];
    for expected in [
        row(
            "calls",
            "socket|server_to_client|order.accepted",
            &format!("{SERVER}:9"),
        ),
        row(
            "calls",
            "socket|server_to_client|order.rejected",
            &format!("{SERVER}:16"),
        ),
        row(
            "calls",
            "socket|server_to_client|order.shipped",
            &format!("{SERVER}:20"),
        ),
        row(
            "calls",
            "socket|server_to_client|order.gated",
            &format!("{SERVER}:26"),
        ),
        row(
            "endpoints",
            "socket|client_to_server|order.placed",
            &format!("{SERVER}:6"),
        ),
    ] {
        assert!(rows.contains(&expected), "{expected:?} in {rows:#?}");
    }
}

/// A field set from one constructor parameter is that parameter (carrick#1670
/// T3): a parameter property, or one `this.field = parameter`, reads the peer
/// each `new` of the class is handed. A field written again, one whose class
/// lets `this` leave, and a class built with a peer nothing hands in have no
/// side.
#[test]
fn a_field_set_from_a_constructor_parameter_carries_its_origin() {
    let rows = &with_switches()["hub"];
    for expected in [
        row(
            "endpoints",
            "socket|client_to_server|run.start",
            &format!("{CONN}:10"),
        ),
        row(
            "calls",
            "socket|server_to_client|run.notify",
            &format!("{CONN}:16"),
        ),
        row(
            "calls",
            "socket|server_to_client|run.done",
            &format!("{CONN}:29"),
        ),
    ] {
        assert!(rows.contains(&expected), "{expected:?} in {rows:#?}");
    }
    for (event, line) in [("run.ping", 42), ("run.flush", 53), ("run.lost", 63)] {
        let unknown = row(
            "calls",
            &format!("socket|unknown|{event}"),
            &format!("{CONN}:{line}"),
        );
        assert!(rows.contains(&unknown), "{unknown:?} in {rows:#?}");
    }
}

/// A peer the service makes with `new` is the client's end, and a peer nothing
/// in the service hands in has no side: the row is not in the direction a
/// guess would put it.
#[test]
fn a_peer_the_service_makes_is_the_clients_end() {
    let rows = &with_switches()["peer"];
    for expected in [
        row(
            "calls",
            "socket|client_to_server|order.placed",
            &format!("{CLIENT}:5"),
        ),
        row(
            "endpoints",
            "socket|server_to_client|order.accepted",
            &format!("{CLIENT}:6"),
        ),
    ] {
        assert!(rows.contains(&expected), "{expected:?} in {rows:#?}");
    }
    let lost = row(
        "calls",
        "socket|unknown|order.lost",
        &format!("{CLIENT}:11"),
    );
    assert!(
        rows.contains(&lost),
        "a peer nothing hands in has no side: {rows:#?}"
    );
}

/// The two services state one key for each event, so they pair.
#[test]
fn the_two_services_pair_on_both_events() {
    let services = with_switches();
    let keys = |service: &str, side: &str| -> Vec<String> {
        services[service]
            .iter()
            .filter(|row| row.side == side)
            .map(|row| row.key.clone())
            .collect()
    };
    for (event, call_in, endpoint_in) in [
        ("socket|client_to_server|order.placed", "peer", "hub"),
        ("socket|server_to_client|order.accepted", "hub", "peer"),
    ] {
        assert!(
            keys(call_in, "calls").contains(&event.to_string()),
            "{call_in} calls {event}: {services:#?}"
        );
        assert!(
            keys(endpoint_in, "endpoints").contains(&event.to_string()),
            "{endpoint_in} serves {event}: {services:#?}"
        );
    }
}

/// No row of the fixture is the model's: every one is the library store's
/// claim.
#[test]
fn every_socket_row_is_a_library_claim() {
    for (service, rows) in with_switches() {
        for row in rows {
            assert_eq!(
                row.source.as_deref(),
                Some("library_claim"),
                "{service}: {row:?}"
            );
        }
    }
}

/// With every switch off there are no site rows and no instance walk, so the
/// scan states no socket row here.
#[test]
fn with_the_switches_off_no_side_is_read() {
    for (service, rows) in scan(false) {
        assert_eq!(rows, Vec::<Row>::new(), "{service}");
    }
}
