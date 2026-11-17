//! A labelled socket receive whose label states no handler slot reads its
//! handler from the call (carrick#4643, sockets).
//!
//! The library store drops a handler slot whose parameter its lister does not
//! mark as a function with a declared signature, so a socket listener's label
//! names the event and nothing else. The call states the handler: among the
//! arguments beside the event name exactly one is a function or an identifier
//! (`receive_handler_arg`, `src/request_summary/decided_sites.rs`).
//!
//! Drives the real scanner binary on `.` over a committed copy of
//! `tests/fixtures/socket-listener-handler-anchor/` and reads the listener
//! entries of the `orders-server` blob's `type_manifest`. The cassette claim
//! is store-faithful: no `handler` on the socket `on` claim. Everything in the
//! fixture is invented.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/socket-listener-handler-anchor")
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

/// The scanner on `.`, offline, over the fixture's cassettes.
fn scan(repo: &Path, storage: &Path, home: &Path, cache: &Path) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(repo)
        .env("HOME", home)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage)
        .env("CARRICK_CACHE_DIR", cache)
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
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "the scan failed:\n{printed}");
    printed
}

type Blobs = BTreeMap<String, serde_json::Value>;

/// The fixture scanned once for the whole binary: one blob per service.
fn scanned() -> &'static Blobs {
    static SCAN: OnceLock<Blobs> = OnceLock::new();
    SCAN.get_or_init(|| {
        let tmp = tempfile::tempdir().expect("temp dir");
        let repo = tmp.path().join("workspace");
        copy_dir(&fixture_dir(), &repo);
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["add", "-A", "-f"]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        let (storage, home, cache) = (
            tmp.path().join("store"),
            tmp.path().join("home"),
            tmp.path().join("cache"),
        );
        for dir in [&storage, &home, &cache] {
            std::fs::create_dir_all(dir).unwrap();
        }
        scan(&repo, &storage, &home, &cache);
        let blobs: Blobs = std::fs::read_dir(&storage)
            .expect("storage dir")
            .filter_map(|entry| Some(entry.ok()?.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .filter_map(|path| {
                let blob: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
                Some((blob["service_name"].as_str()?.to_string(), blob))
            })
            .collect();
        assert_eq!(
            blobs.keys().map(String::as_str).collect::<Vec<_>>(),
            ["orders-client", "orders-server"],
            "every service uploads one blob"
        );
        blobs
    })
}

/// What the listener entries of `event` hold on the server: `(type_state,
/// primary_type_symbol)` of each, empty when the scan stores none.
fn listener_entries(event: &str) -> Vec<(String, Option<String>)> {
    scanned()["orders-server"]["type_manifest"]
        .as_array()
        .expect("type_manifest")
        .iter()
        .filter(|entry| entry["event"] == event && entry["role"] == "producer")
        .map(|entry| {
            (
                entry["type_state"].as_str().unwrap_or("").to_string(),
                entry["primary_type_symbol"].as_str().map(str::to_string),
            )
        })
        .collect()
}

fn typed_as_order_update(event: &str) -> bool {
    let entries = listener_entries(event);
    !entries.is_empty()
        && entries
            .iter()
            .all(|(_, symbol)| symbol.as_deref() == Some("OrderUpdate"))
}

fn stays_untyped(event: &str) {
    let entries = listener_entries(event);
    assert!(
        entries
            .iter()
            .all(|(state, symbol)| state == "unknown" && symbol.is_none()),
        "{event} must stay untyped: {entries:?}"
    );
}

#[test]
fn a_socket_listener_entry_is_typed_at_its_destructured_parameter() {
    assert!(
        typed_as_order_update("order:update"),
        "{:?}",
        listener_entries("order:update")
    );
}

#[test]
fn a_socket_listener_entry_is_typed_at_a_parameter_the_event_map_types() {
    assert!(
        typed_as_order_update("order:ping"),
        "{:?}",
        listener_entries("order:ping")
    );
}

#[test]
fn an_any_typed_listener_parameter_stays_unknown() {
    stays_untyped("order:any");
}

#[test]
fn a_socket_receive_with_two_function_arguments_gets_no_handler_anchor() {
    stays_untyped("order:twice");
}

#[test]
fn a_labelled_handler_slot_is_read_as_before() {
    assert!(
        typed_as_order_update("order:channel"),
        "{:?}",
        listener_entries("order:channel")
    );
}

#[test]
fn a_bus_receive_with_no_handler_slot_is_untouched() {
    // The broker entry names no event; it sits at the `subscribe` call (line
    // 53 of server.ts) and stays unknown.
    let entries: Vec<_> = scanned()["orders-server"]["type_manifest"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["line_number"] == 53)
        .collect();
    assert!(!entries.is_empty(), "the broker receive keeps its entry");
    assert!(
        entries
            .iter()
            .all(|entry| entry["type_state"] == "unknown"
                && entry["primary_type_symbol"].is_null()),
        "{entries:?}"
    );
}

#[test]
fn a_receive_with_no_event_name_gets_no_handler_anchor() {
    // `onAny((event, payload) => ...)` states no event: nothing is stored.
    let entries: Vec<_> = scanned()["orders-server"]["type_manifest"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["line_number"] == 35)
        .collect();
    assert!(entries.is_empty(), "{entries:?}");
}

#[test]
fn a_reserved_event_name_stays_untyped() {
    stays_untyped("disconnect");
}

#[test]
fn a_socket_listener_named_by_an_identifier_handler_is_typed() {
    assert!(
        typed_as_order_update("order:named"),
        "{:?}",
        listener_entries("order:named")
    );
}

#[test]
fn an_identifier_argument_that_is_not_a_function_stays_unknown() {
    stays_untyped("order:notfn");
}
