//! A leaf site row's call result takes the library store's `result` label
//! (carrick#2910, slice 1).
//!
//! Drives the real scanner binary on `.` in production form (every fact-layer
//! switch on), over the committed `tests/fixtures/call-through-result-label/`
//! (see its README): the client package's wrappers each make one call of an
//! invented library client, and the store labels `get` with the member `data`,
//! `send` with the whole value and `ping` with nothing. The client blob's
//! consumer response entry at each library call is the type the sidecar read
//! there, with and without the labels.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const CLIENT_FILE: &str = "packages/client/src/users.ts";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/call-through-result-label")
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

/// Every key named `result` removed, at any depth.
fn without_results(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.remove("result");
            map.values_mut().for_each(without_results);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(without_results),
        _ => {}
    }
}

/// What one scan of the fixture left: each service's blob and the run log.
struct Scan {
    blobs: BTreeMap<String, serde_json::Value>,
    log: String,
}

/// The fixture scanned as the Action scans it with the fact layer on,
/// offline. `labelled: false` strips every `result` from the store's answer.
fn scan(labelled: bool) -> Scan {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir(), &repo);
    if !labelled {
        let cassette = repo.join("__llm__/library-claims/default.json");
        let mut answer: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&cassette).unwrap()).unwrap();
        without_results(&mut answer);
        std::fs::write(&cassette, serde_json::to_vec_pretty(&answer).unwrap()).unwrap();
    }
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let storage = tmp.path().join("storage");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&storage).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(&repo)
        .env("HOME", &home)
        .env("CARRICK_LOCAL_STORAGE_DIR", &storage)
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", tmp.path().join("cache"))
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env(
            "CARRICK_SIDECAR_DIR",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar"),
        )
        .env("CARRICK_POSITION_ROWS", "1")
        .env("CARRICK_SITE_QUESTIONS", "1")
        .env("CARRICK_FACT_FILES", "1")
        .env("CARRICK_RECEIVER_TRACE", "1")
        .env("CARRICK_SITE_PAYLOADS", "1");
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
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let blobs: BTreeMap<String, serde_json::Value> = std::fs::read_dir(&storage)
        .unwrap()
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
        ["api", "cli", "client"],
        "every service uploads one blob"
    );
    let log = std::fs::read_dir(home.join(".carrick/logs/runs"))
        .expect("the scan keeps a run log")
        .filter_map(|entry| std::fs::read_to_string(entry.ok()?.path()).ok())
        .collect::<Vec<_>>()
        .join("\n");
    Scan { blobs, log }
}

fn labelled() -> &'static Scan {
    static SCAN: OnceLock<Scan> = OnceLock::new();
    SCAN.get_or_init(|| scan(true))
}

fn unlabelled() -> &'static Scan {
    static SCAN: OnceLock<Scan> = OnceLock::new();
    SCAN.get_or_init(|| scan(false))
}

/// The type the client's consumer response entry holds at each library call
/// of `users.ts`, by the line of the call: the right-hand side of its alias in
/// the blob's bundled declarations, on one line.
fn client_responses(scan: &Scan) -> BTreeMap<u64, String> {
    let blob = &scan.blobs["client"];
    let bundled = blob["bundled_types"].as_str().unwrap_or_default();
    blob["type_manifest"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| {
            entry["role"] == "consumer"
                && entry["type_kind"] == "response"
                && entry["file_path"] == CLIENT_FILE
        })
        .map(|entry| {
            let alias = entry["type_alias"].as_str().unwrap();
            let head = format!("export type {alias} = ");
            let shown = bundled
                .find(&head)
                .map(|at| {
                    let rest = &bundled[at + head.len()..];
                    let end = rest.find(";\n").map_or(rest.len(), |end| end + 1);
                    rest[..end].split_whitespace().collect::<Vec<_>>().join(" ")
                })
                .unwrap_or_default();
            (entry["line_number"].as_u64().unwrap(), shown)
        })
        .collect()
}

/// The line the first `needle` of a wrapper's body is on.
fn call_line(name: &str, needle: &str) -> u64 {
    let source = std::fs::read_to_string(fixture_dir().join(CLIENT_FILE)).unwrap();
    let start = source
        .lines()
        .position(|line| {
            line.starts_with(&format!("export async function {name}("))
                || line.starts_with(&format!("export function {name}("))
        })
        .unwrap_or_else(|| panic!("{name} is in the fixture"));
    source
        .lines()
        .enumerate()
        .skip(start)
        .find(|(_, line)| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle} is in {name}"))
        .0 as u64
        + 1
}

/// The wrapper opening a library call, and the line of that call.
const CALLS: [(&str, &str); 9] = [
    ("loadUser", "api.get<User>"),
    ("loadStatus", "api.get<User>"),
    ("createUser", "api.send<User>"),
    ("pingUsers", "api.ping"),
    ("loadPage", "api.get<UserPage>"),
    ("loadModel", "api.get<User>"),
    ("loadFrozen", "api.send<User>"),
    ("loadThen", "api.get<User>"),
    ("loadCaught", "api.send<User>"),
];

/// The type read at each wrapper's library call, as the scan states it.
fn at_each_call(scan: &Scan) -> Vec<(&'static str, String)> {
    let responses = client_responses(scan);
    CALLS
        .iter()
        .map(|(wrapper, needle)| {
            let line = call_line(wrapper, needle);
            let shown = responses
                .get(&line)
                .unwrap_or_else(|| panic!("{wrapper}: a response entry at {line}: {responses:?}"));
            (*wrapper, shown.clone())
        })
        .collect()
}

fn expect(rows: &[(&'static str, &str)]) -> Vec<(&'static str, String)> {
    rows.iter()
        .map(|(wrapper, shown)| (*wrapper, shown.to_string()))
        .collect()
}

/// The labelled call's own value is what the consumer response states, not
/// what the wrapper does with it: a member the store labels as the body gives
/// that member, a call labelled whole gives the whole value, and a call the
/// store labels with nothing keeps the type read at its use. The wrapper's
/// own reads (`.status`, `.then`, `.catch`, `toModel(...)`) do not show here.
#[test]
fn a_leaf_site_row_states_the_type_the_store_labels() {
    assert!(
        labelled().log.contains("result shapes: verified results"),
        "the store's labels were asked for and read"
    );
    assert!(!unlabelled().log.contains("result shapes: verified results"));
    assert_eq!(
        at_each_call(labelled()),
        expect(&[
            ("loadUser", "{ id: string; };"),
            ("loadStatus", "{ id: string; };"),
            ("createUser", "{ id: string; };"),
            ("pingUsers", "boolean;"),
            ("loadPage", "{ items: { id: string; }[]; };"),
            ("loadModel", "{ id: string; };"),
            ("loadFrozen", "{ id: string; };"),
            ("loadThen", "{ id: string; };"),
            ("loadCaught", "{ id: string; };"),
        ])
    );
}

/// A store that labels nothing leaves each row as the type read at its use:
/// the value the wrapper makes of the call.
#[test]
fn a_store_without_labels_leaves_each_row_as_it_was() {
    assert_eq!(
        at_each_call(unlabelled()),
        expect(&[
            (
                "loadUser",
                "import(\"fixture-reply-client@1.0.0\").Reply<User>;"
            ),
            ("loadStatus", "number;"),
            ("createUser", "User;"),
            ("pingUsers", "boolean;"),
            (
                "loadPage",
                "import(\"fixture-reply-client@1.0.0\").Reply<UserPage>;"
            ),
            ("loadModel", "{ id: string; loadedAt: number; };"),
            ("loadFrozen", "{ readonly id: string; };"),
            ("loadThen", "User;"),
            ("loadCaught", "User | null;"),
        ])
    );
}

/// The rows other than the labelled calls' responses are the same with and
/// without the labels: the client's request entries and the CLI's caller
/// rows, whose anchors are not labelled calls.
#[test]
fn only_the_labelled_calls_responses_move() {
    let (with, without) = (labelled(), unlabelled());
    for service in ["client", "cli"] {
        let entries = |scan: &Scan| -> Vec<serde_json::Value> {
            scan.blobs[service]["type_manifest"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|entry| {
                    !(service == "client"
                        && entry["role"] == "consumer"
                        && entry["type_kind"] == "response")
                })
                .collect()
        };
        assert_eq!(entries(with), entries(without), "{service}");
        assert_eq!(
            with.blobs[service]["calls"], without.blobs[service]["calls"],
            "{service}: calls"
        );
    }
    // The CLI's own caller rows hold the types they held.
    let cli = |scan: &Scan| scan.blobs["cli"]["bundled_types"].clone();
    assert_eq!(cli(with), cli(without));
}
