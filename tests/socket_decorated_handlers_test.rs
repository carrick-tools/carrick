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
fn scan() -> Scanned {
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
    let mut unread = BTreeMap::new();
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
        let name = blob["service_name"].as_str().unwrap().to_string();
        unread.insert(
            name.clone(),
            blob["unread_sites"].as_array().cloned().unwrap_or_default(),
        );
        services.insert(name, rows);
    }
    assert_eq!(
        services.keys().map(String::as_str).collect::<Vec<_>>(),
        ["lobby", "player"],
        "every service uploads one blob"
    );
    Scanned { services, unread }
}

/// What a scan of the fixture wrote, by service: the socket rows and the
/// blob's `unread_sites`.
struct Scanned {
    services: BTreeMap<String, Vec<Row>>,
    unread: BTreeMap<String, Vec<serde_json::Value>>,
}

fn scan_once() -> &'static Scanned {
    static SCAN: OnceLock<Scanned> = OnceLock::new();
    SCAN.get_or_init(scan)
}

fn scanned() -> &'static BTreeMap<String, Vec<Row>> {
    &scan_once().services
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
/// the gateway file has no socket row but its three listeners.
#[test]
fn a_decorator_with_no_label_states_nothing() {
    let lobby = &scanned()["lobby"];
    assert!(
        lobby
            .iter()
            .all(|row| !row.key.ends_with("lobby.trace") && !row.key.ends_with("lobby.idle")),
        "{lobby:#?}"
    );
    let in_gateway: Vec<_> = lobby
        .iter()
        .filter(|row| row.file_path.starts_with(&format!("{GATEWAY}:")))
        .collect();
    assert_eq!(in_gateway.len(), 3, "{lobby:#?}");
}

/// A decorator whose name slot is a type parameter with no constraint and a
/// default of `string` takes a name string: its method is a listener.
#[test]
fn a_decorator_whose_name_is_a_defaulted_type_parameter_is_a_listener() {
    let lobby = &scanned()["lobby"];
    let listener = row(
        "endpoints",
        "socket|client_to_server|lobby.leave",
        &format!("{GATEWAY}:20"),
    );
    assert!(lobby.contains(&listener), "{listener:?} in {lobby:#?}");
}

/// The same slot with no default says nothing about its argument: no row.
/// The label's check could not be run against the package's declarations,
/// so the site is saved once, as `label_unchecked` (carrick#4162).
#[test]
fn a_decorator_whose_name_is_an_unbounded_type_parameter_states_no_row_and_saves_its_site() {
    let scan = scan_once();
    let lobby = &scan.services["lobby"];
    assert!(
        lobby.iter().all(|row| !row.key.ends_with("lobby.nothing")),
        "{lobby:#?}"
    );
    let at: Vec<_> = scan.unread["lobby"]
        .iter()
        .filter(|site| site["file"] == GATEWAY && site["line"] == 23)
        .collect();
    assert_eq!(at.len(), 1, "{at:#?} of {:#?}", scan.unread["lobby"]);
    assert_eq!(at[0]["reason"], "label_unchecked", "{}", at[0]);
    assert_eq!(at[0]["slot"], "name", "{}", at[0]);
}

const WRAPPED: &str = "services/lobby/src/wrapped.ts";
const COMPOSED: &str = "services/lobby/src/composed.ts";
const IMPORTED: &str = "services/lobby/src/imported.ts";
const DECORATORS: &str = "services/lobby/src/decorators.ts";

/// The lobby's unread sites at `file:line`.
fn unread_at(file: &str, line: u64) -> Vec<&'static serde_json::Value> {
    scan_once().unread["lobby"]
        .iter()
        .filter(|site| site["file"] == file && site["line"] == line)
        .collect()
}

/// The lobby states the listener of `event` at `file:line`, the player's
/// send at `sent_at` of its client has the same key, and nothing is saved
/// unread at the method.
fn assert_listener_paired(event: &str, file: &str, line: u64, sent_at: u32) {
    let services = scanned();
    let (lobby, player) = (&services["lobby"], &services["player"]);
    let key = format!("socket|client_to_server|{event}");
    let listener = row("endpoints", &key, &format!("{file}:{line}"));
    assert!(lobby.contains(&listener), "{listener:?} in {lobby:#?}");
    let emit = row("calls", &key, &format!("{CLIENT}:{sent_at}"));
    assert!(player.contains(&emit), "{emit:?} in {player:#?}");
    let saved = unread_at(file, line);
    assert!(
        saved.is_empty(),
        "nothing is saved at the method: {saved:#?}"
    );
}

/// A method decorated through an in-repo decorator factory whose returned
/// value applies the labelled decorator (carrick#3781): the factory's call
/// of the decorator is the decorator the method gets, so the method is its
/// listener, named by what the method's decorator passes, and nothing is
/// saved there. A factory over a decorator the store does not label states
/// nothing and saves nothing at its method.
#[test]
fn a_method_decorated_through_an_in_repo_factory_is_the_decorators_listener() {
    assert_listener_paired("w:wrapped", WRAPPED, 20, 14);
    let lobby = &scanned()["lobby"];
    assert!(
        lobby.iter().all(|row| !row.key.ends_with("w:tagged")),
        "{lobby:#?}"
    );
    let tagged = unread_at(WRAPPED, 23);
    assert!(tagged.is_empty(), "{tagged:#?}");
}

/// The factory hands back a library combinator's call that holds the
/// labelled decorator's call among its arguments.
#[test]
fn a_factory_over_a_library_combinator_is_the_decorators_listener() {
    assert_listener_paired("w:combined", COMPOSED, 27, 18);
}

/// The factory is declared in another file and imported.
#[test]
fn a_factory_in_another_file_is_the_decorators_listener() {
    assert_listener_paired("w:imported", IMPORTED, 5, 26);
}

/// The decorator passes a module constant: its text is the name.
#[test]
fn a_factory_argument_read_through_a_constant_is_the_listeners_name() {
    assert_listener_paired("w:const", COMPOSED, 33, 22);
}

/// A factory that calls the labelled decorator but hands back another one
/// states nothing: the method keeps the factory's stop (carrick#3688 C1).
#[test]
fn a_factory_that_does_not_return_the_decorator_states_nothing() {
    let lobby = &scanned()["lobby"];
    assert!(
        lobby.iter().all(|row| !row.key.ends_with("w:discarded")),
        "{lobby:#?}"
    );
    let saved = unread_at(COMPOSED, 30);
    assert_eq!(saved.len(), 1, "{saved:#?}");
    assert_eq!(saved[0]["reason"], "own_function", "{}", saved[0]);
    assert_eq!(saved[0]["slot"], "name", "{}", saved[0]);
    assert_eq!(saved[0]["callee"], "Logs", "{}", saved[0]);
}

/// The unread sites at `file:line` that reached the labelled decorator.
fn saved_decorator_at(file: &str, line: u64) -> Vec<&'static serde_json::Value> {
    unread_at(file, line)
        .into_iter()
        .filter(|site| site["labelled_end"]["export"] == "OnMessage")
        .collect()
}

/// Once every method decorated through a factory is read, the factory's
/// body saves nothing of the labelled decorator: every use of its call was
/// read. (The unlabelled exports it also calls keep their own saves.)
#[test]
fn the_factory_body_saves_nothing_once_every_caller_is_read() {
    for (file, line) in [
        (WRAPPED, 13),
        (COMPOSED, 4),
        (COMPOSED, 21),
        (DECORATORS, 12),
    ] {
        let saved = saved_decorator_at(file, line);
        assert!(saved.is_empty(), "{file}:{line}: {saved:#?}");
    }
}
