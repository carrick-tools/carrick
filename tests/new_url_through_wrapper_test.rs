//! A request wrapper whose options run through a helper that returns
//! `{ ...init, key: value }` (carrick#1561 C11, the `new_url_target` rule's
//! deletion): the request summaries read the method its callers write
//! through that spread, so a `new URL('/path', origin)` handed to the
//! wrapper states its path at each caller, host-free. Runs the real scanner
//! binary on `.` over a committed copy of
//! `tests/fixtures/new-url-through-wrapper`, offline, and reads the stored
//! blob.
//!
//! Protocols: HTTP (the request summaries are HTTP-only).

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/new-url-through-wrapper")
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "carrick-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "carrick-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
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

/// The one blob the scan stored; the fixture is scanned once per run.
fn scan() -> serde_json::Value {
    static BLOB: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    BLOB.get_or_init(scan_once).clone()
}

fn scan_once() -> serde_json::Value {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir(), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let cassettes = repo.join("__llm__");
    let storage = tmp.path().join("storage");
    std::fs::create_dir_all(&storage).unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(&repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", &storage)
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", tmp.path().join("cache"))
        .env("HOME", &home)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1");
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
    let mut blobs: Vec<serde_json::Value> = Vec::new();
    for entry in std::fs::read_dir(&storage).expect("the store reads") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|ext| ext == "json") {
            blobs.push(serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap());
        }
    }
    assert_eq!(blobs.len(), 1, "the scan stored one blob");
    blobs.remove(0)
}

/// The rows stated at `file:line` (a row's `file_path` carries its line).
fn rows_at(blob: &serde_json::Value, file: &str, line: u64) -> Vec<serde_json::Value> {
    let at = format!("{file}:{line}");
    blob["calls"]
        .as_array()
        .expect("calls written")
        .iter()
        .filter(|call| call["file_path"] == at.as_str())
        .cloned()
        .collect()
}

/// Exactly one request-summary row at `file:line`, keyed `method path`.
fn summary_row(blob: &serde_json::Value, file: &str, line: u64, method: &str, path: &str) {
    let rows = rows_at(blob, file, line);
    assert_eq!(rows.len(), 1, "{file}:{line} {rows:#?}");
    let row = &rows[0];
    assert_eq!(row["resolution_source"], "request_summary", "{row:#}");
    assert_eq!(row["key"]["method"], method, "{row:#}");
    assert_eq!(row["key"]["path"], path, "{row:#}");
}

/// The four ways a caller hands the wrapper a `new URL` it bound first (the
/// object, `.href`, `.toString()`, a template), with the origin a
/// destructured or plain parameter, and the URL built in the wrapper's
/// argument: each is the caller's request, keyed on the path alone. The
/// wrapper's options run through `withAgent`, whose `{ ...init, keepalive }`
/// return keeps the method each caller writes.
#[test]
fn a_new_url_handed_to_a_wrapper_states_its_path_at_each_caller() {
    let blob = scan();
    for (line, path) in [
        (5, "/api/releases/confirm"),
        (14, "/api/releases"),
        (20, "/api/bundles/preview"),
        (26, "/api/bundles/:name/upload"),
    ] {
        summary_row(&blob, "src/releases.ts", line, "POST", path);
    }
    summary_row(&blob, "src/usage.ts", 9, "POST", "/api/usage");
}

/// The control: the same `new URL` handed to `fetch` itself.
#[test]
fn a_new_url_handed_to_fetch_states_its_path() {
    let blob = scan();
    summary_row(&blob, "src/usage.ts", 16, "GET", "/api/ping");
}

/// A helper with one return, `{ ...init, signal }`: the caller's options
/// with a key added, read through the builder's parameter.
#[test]
fn a_one_return_helper_that_spreads_the_options_keeps_the_callers_method() {
    let blob = scan();
    summary_row(&blob, "src/usage.ts", 21, "POST", "/api/heartbeat");
}

/// A helper whose other return writes `method` after the spread: the method
/// the caller writes may be replaced, so no request-summary row states it.
#[test]
fn a_helper_that_may_overwrite_the_method_states_no_row() {
    let blob = scan();
    let rows = rows_at(&blob, "src/usage.ts", 26);
    assert!(
        rows.iter()
            .all(|row| row["resolution_source"] != "request_summary"),
        "{rows:#?}"
    );
}

/// The type the compiler engine infers at `file:line` for `kind`
/// (`request_body`, `call_result`), as the type manifest states it:
/// `(type_state, expanded_definition)`.
fn type_at(blob: &serde_json::Value, file: &str, line: u64, kind: &str) -> Vec<(String, String)> {
    blob["type_manifest"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry["file_path"] == file
                && entry["line_number"] == line
                && entry["evidence"]["infer_kind"] == kind
        })
        .map(|entry| {
            (
                entry["evidence"]["type_state"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                entry["expanded_definition"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// The body each caller sends and the response it casts keep their types
/// at the caller, through the wrapper and the helper.
#[test]
fn each_caller_keeps_its_request_and_response_types() {
    let blob = scan();
    let one = |state: &str, text: &str| vec![(state.to_string(), text.to_string())];
    for (file, line, kind, want) in [
        (
            "src/releases.ts",
            5,
            "request_body",
            one("implicit", "{ version: string; notes: string; }"),
        ),
        (
            "src/releases.ts",
            5,
            "call_result",
            one("explicit", "{ confirmed: boolean; }"),
        ),
        (
            "src/releases.ts",
            14,
            "request_body",
            one("implicit", "{ version: string; }"),
        ),
        (
            "src/releases.ts",
            14,
            "call_result",
            one("explicit", "{ id: string; }"),
        ),
        (
            "src/usage.ts",
            9,
            "request_body",
            one("implicit", "{ command: string; }"),
        ),
    ] {
        assert_eq!(
            type_at(&blob, file, line, kind),
            want,
            "{file}:{line} {kind}"
        );
    }
}
