//! A request the request summaries state through a labelled HTTP client
//! sends the method the store's label states (carrick#4373 T1).
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/summary-method-from-label/` with the type sidecar live,
//! as the Action runs a scan. `@fixture/http`'s `remove` and `purge` always
//! send DELETE; each call passes an options bag that states no method,
//! which the summaries' own reading would call GET. The answer key is the
//! fixture's `README.md`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/summary-method-from-label")
}

/// The blob one offline scan of `repo` writes.
fn scan(repo: &Path) -> serde_json::Value {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env_remove("CARRICK_ALLOW_MISSING_TYPES");
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
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut blobs: Vec<serde_json::Value> = std::fs::read_dir(storage.path())
        .expect("storage dir")
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .map(|entry| {
            serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
                .expect("parse blob")
        })
        .collect();
    assert_eq!(blobs.len(), 1, "one service, one blob");
    blobs.remove(0)
}

/// The HTTP methods of the call rows at `file:line`, sorted.
fn methods_at(blob: &serde_json::Value, at: &str) -> Vec<String> {
    let mut methods: Vec<String> = blob["calls"]
        .as_array()
        .expect("calls")
        .iter()
        .filter(|call| call["file_path"] == at)
        .filter_map(|call| call["key"]["method"].as_str().map(str::to_string))
        .collect();
    methods.sort();
    methods
}

/// The HTTP methods of the graph's data calls at `file:line`, sorted.
fn graph_methods_at(blob: &serde_json::Value, at: &str) -> Vec<String> {
    let mut methods: Vec<String> = blob["mount_graph"]["data_calls"]
        .as_array()
        .expect("data_calls")
        .iter()
        .filter(|call| call["file_location"] == at)
        .filter_map(|call| call["method"].as_str().map(str::to_string))
        .collect();
    methods.sort();
    methods
}

/// The methods the consumer type entries at `file:line` are keyed by.
fn manifest_methods_at(blob: &serde_json::Value, file: &str, line: u64) -> Vec<String> {
    let mut methods: Vec<String> = blob["type_manifest"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|entry| {
            entry["role"] == "consumer"
                && entry["file_path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with(file))
                && entry["line_number"] == line
        })
        .filter_map(|entry| entry["method"].as_str().map(str::to_string))
        .collect();
    methods.sort();
    methods.dedup();
    methods
}

fn assert_states(blob: &serde_json::Value, at: &str, want: &[&str]) {
    let want: Vec<String> = want.iter().map(|method| method.to_string()).collect();
    assert_eq!(methods_at(blob, at), want, "call rows at {at}");
    assert_eq!(graph_methods_at(blob, at), want, "graph data calls at {at}");
}

#[test]
fn a_summary_row_of_a_labelled_send_states_the_labels_method() {
    let blob = scan(&fixture_dir());
    for at in [
        "src/counter.ts:7",
        "src/purge.ts:7",
        "src/xclear-caller.ts:3",
        "src/retawait.ts:6",
    ] {
        assert_states(&blob, at, &["DELETE"]);
    }
}

#[test]
fn a_yielded_or_forwarded_request_states_the_labels_method() {
    let blob = scan(&fixture_dir());
    for (file, line) in [
        ("src/query.ts", 7),
        ("src/fwd.ts", 7),
        ("src/xdrop-caller.ts", 3),
    ] {
        assert_states(&blob, &format!("{file}:{line}"), &["DELETE"]);
        assert!(
            !manifest_methods_at(&blob, file, line).contains(&"GET".to_string()),
            "no type entry keyed GET at {file}:{line}"
        );
    }
}

#[test]
fn two_requests_at_one_caller_keep_their_own_methods() {
    let blob = scan(&fixture_dir());
    assert_states(&blob, "src/pair-lines.ts:7", &["DELETE", "POST"]);
}

#[test]
fn unlabelled_sends_keep_the_summaries_method() {
    let blob = scan(&fixture_dir());
    assert_states(&blob, "src/fetcher.ts:5", &["GET"]);
    assert_states(&blob, "src/legacy.ts:7", &["GET"]);
    assert_states(&blob, "src/poster.ts:7", &["POST"]);
}

#[test]
fn a_slot_read_method_is_left_to_each_caller() {
    let blob = scan(&fixture_dir());
    assert_states(&blob, "src/jobs.ts:7", &["GET"]);
    assert_states(&blob, "src/jobs.ts:8", &["POST"]);
}
