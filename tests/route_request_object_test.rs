//! carrick#4197: a route's request body is never the request object its
//! handler is handed.
//!
//! The fact layer places a route handler's payload at the handler's first
//! parameter. For an HTTP route that parameter is the server's request object
//! (`request: Request`, a Deno server's `c: Context<…>`), so typing the
//! route's request entry there published the request object as the body once
//! the definition step could read installed packages (carrick#3995 T5). The
//! body is what the handler reads off that object; these handlers read none,
//! so the route takes no request body.
//!
//! Drives the real scanner binary offline, with the type sidecar live, over
//! `tests/fixtures/pr-seeds/admin-api` (a named handler, an installed server
//! package) and `tests/fixtures/deno-npm-specifier` (an inline handler, a
//! Deno `npm:` import; needs the pinned `deno` on PATH, as
//! `deno_npm_specifier_test.rs` does).

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The service's blob after one offline scan of `root`.
fn scan_blob(root: &Path, deno_dir: Option<&Path>) -> serde_json::Value {
    let storage = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(root)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", root.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1");
    if let Some(deno_dir) = deno_dir {
        cmd.env("DENO_DIR", deno_dir);
    }
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
        "CARRICK_ALLOW_MISSING_TYPES",
        "CARRICK_OUTPUT_JSON",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("failed to spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let entry = std::fs::read_dir(storage.path())
        .unwrap()
        .flatten()
        .find(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .expect("one blob");
    serde_json::from_slice(&std::fs::read(entry.path()).unwrap()).unwrap()
}

/// The producer request entry of `method path`: it publishes no type, and
/// says the route takes no request body.
fn assert_no_request_body(blob: &serde_json::Value, method: &str, path: &str) {
    let entries: Vec<&serde_json::Value> = blob["type_manifest"]
        .as_array()
        .expect("type_manifest")
        .iter()
        .filter(|entry| {
            entry["role"] == "producer"
                && entry["type_kind"] == "request"
                && entry["method"] == method
                && entry["path"] == path
        })
        .collect();
    assert_eq!(entries.len(), 1, "{method} {path}: {entries:#?}");
    let entry = entries[0];
    assert_eq!(entry["type_state"], "unknown", "{method} {path}: {entry:#}");
    assert!(
        entry["expanded_definition"].is_null(),
        "{method} {path} publishes a body: {entry:#}"
    );
    let reasons: Vec<&str> = entry["any_provenance"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|finding| finding["path"] == "")
        .filter_map(|finding| finding["reason"].as_str())
        .collect();
    assert_eq!(reasons, ["no_request_body"], "{method} {path}: {entry:#}");
}

#[test]
fn a_named_handlers_request_parameter_is_not_the_routes_body() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("admin-api");
    copy_tree(&fixture("pr-seeds/admin-api"), &root);
    let blob = scan_blob(&root, None);
    assert_no_request_body(&blob, "POST", "/api/v1/admin/orders/:orderId/cancel");
}

#[cfg(unix)]
#[test]
fn an_inline_deno_handlers_context_is_not_the_routes_body() {
    let work = tempfile::tempdir().unwrap();
    let deno_dir = tempfile::tempdir().unwrap();
    let root = work.path().join("app");
    copy_tree(&fixture("deno-npm-specifier"), &root);
    let install = Command::new("deno")
        .args([
            "install",
            "--frozen",
            "--node-modules-dir=none",
            "--entrypoint",
            "src/app.ts",
        ])
        .current_dir(&root)
        .env("DENO_DIR", deno_dir.path())
        .env("DENO_NO_UPDATE_CHECK", "1")
        .output()
        .expect("a real `deno` must be on PATH for this test");
    assert!(
        install.status.success(),
        "deno install failed:\n{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let blob = scan_blob(&root, Some(deno_dir.path()));
    assert_no_request_body(&blob, "POST", "/items");
}
