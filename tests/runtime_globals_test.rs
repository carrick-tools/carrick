//! carrick#2445: the scanner asks the store for a global the runtime's types
//! declare, and matches the answer, in a program that loads no DOM library.
//!
//! Drives the real scanner binary offline over `tests/fixtures/runtime-globals/`
//! as the Action does (`carrick <dir>`, the root service) with the type
//! sidecar live.
//! The fixture's service sets `"lib": ["ES2022"]` and `"types": ["node"]`, so
//! `fetch` is declared by the runtime's types
//! (`@types/node/web-globals/fetch.d.ts`), not by a default-library file.
//! `src/orders.ts` calls `fetch(path, { method })` in a helper, and
//! `placeOrder` (line 7) calls the helper with `"/api/orders"` and `"POST"`.
//! The cassette answers the key `node:web-globals/fetch` with `fetch`'s
//! HTTP claims. The store is asked for `node:web-globals/fetch` and answers,
//! and `POST /api/orders` is stated at `src/orders.ts:7`.
//!
//! The request summaries already state this minimal shape, so the row alone
//! cannot tell the ask from its absence; the boundary's library counters,
//! written only by a scan that asked the store, do. The site itself, matched
//! through the runtime's key, is pinned in the engine's unit tests
//! (`a_fetch_call_is_a_site_through_the_runtime_s_key`).

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/runtime-globals")
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

/// The blob's `calls` at `src/orders.ts`, as `METHOD path@line`.
fn calls_at_orders(blob: &serde_json::Value) -> Vec<String> {
    let mut calls: Vec<String> = blob["calls"]
        .as_array()
        .expect("calls")
        .iter()
        .filter(|call| {
            call["file_path"]
                .as_str()
                .is_some_and(|at| at.starts_with("src/orders.ts:"))
        })
        .map(|call| {
            format!(
                "{} {}@{}",
                call["key"]["method"].as_str().unwrap_or_default(),
                call["key"]["path"].as_str().unwrap_or_default(),
                call["file_path"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    calls.sort();
    calls
}

#[test]
fn a_global_the_runtime_declares_is_asked_and_its_answer_read() {
    let on = scan(&fixture_dir());
    // The store was asked, and its answer read: the boundary counts the
    // packages left pending or failed only for a scan that asked. Without
    // the runtime's key the service asks nothing (it imports no package and
    // loads no DOM library).
    assert_eq!(
        on["boundary"]["library_packages_pending"],
        serde_json::json!(0),
        "on: the store was asked: {}",
        on["boundary"]
    );
    assert_eq!(
        on["boundary"]["library_packages_failed"],
        serde_json::json!(0)
    );
    assert_eq!(
        calls_at_orders(&on),
        vec!["POST /api/orders@src/orders.ts:7".to_string()],
        "on: the helper's caller states the request"
    );
}
