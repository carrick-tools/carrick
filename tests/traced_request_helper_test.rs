//! A request made inside a function handed to a package's export reads its
//! method off the enclosing helper's options parameter, so the fact layer
//! states the method the helper's caller writes (found tracing carrick#2676).
//!
//! The fixture (`tests/fixtures/traced-request-helper/`) is one root service,
//! scanned as the Action scans it (`carrick .` from the repo root) with the
//! compiler rows and site questions on, models mocked, and the store's labels
//! from its cassette. See its `README.md` for the answer key.
//!
//! - `src/traced.ts` makes its `fetch` inside a callback handed to a timing
//!   span, and passes its own `init` parameter on whole. The method is the
//!   `method` key of that parameter, which the caller in `src/tasks-api.ts`
//!   writes (`PUT`) or leaves out (`GET`, the label's default). Read inside
//!   the callback, the parameter is one function out, and its key used to
//!   be read as unknown, so the method was asked and the site stated no row.
//! - `src/rest.ts`'s `sendConfig` hands its whole options parameter to a
//!   labelled call inside the span, whose name and method are keys of it
//!   (`url`, `method`): both are read through the enclosing parameter.
//! - `src/plain.ts` is the same helper with no span: its site was already a
//!   fact, and stays one.
//! - `src/rest.ts` spreads the rest of a destructured options bag inside the
//!   span: already a fact, and stays one.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
        }
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .status()
        .expect("git failed to spawn");
    assert!(status.success(), "git {args:?} failed");
}

/// The fixture scanned as the Action scans it; its stored blob.
fn scan() -> serde_json::Value {
    scan_with_log().0
}

/// The fixture scanned as the Action scans it; its stored blob and its run
/// log.
fn scan_with_log() -> (serde_json::Value, String) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("traced-request-helper");
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/traced-request-helper"),
        &repo,
    );
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let store = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(&repo)
        .env("HOME", home.path())
        .env("CARRICK_LOCAL_STORAGE_DIR", store.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env(
            "CARRICK_SIDECAR_DIR",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar"),
        );
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        "CARRICK_NO_MODEL",
        "CARRICK_ALLOW_MISSING_TYPES",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let blob = fs::read_dir(store.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap())
        .expect("the scan stored a blob");
    let log = fs::read_dir(home.path().join(".carrick/logs/runs"))
        .expect("the scan keeps a run log")
        .filter_map(|entry| fs::read_to_string(entry.ok()?.path()).ok())
        .collect::<Vec<_>>()
        .join("\n");
    (blob, log)
}

/// Each call row of the blob as `file:line METHOD path source`.
fn call_rows(blob: &serde_json::Value) -> Vec<String> {
    let mut rows: Vec<String> = blob["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|call| {
            format!(
                "{} {} {} {}",
                call["file_path"].as_str().unwrap_or_default(),
                call["key"]["method"].as_str().unwrap_or_default(),
                call["key"]["path"].as_str().unwrap_or_default(),
                call["resolution_source"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    rows.sort();
    rows
}

#[test]
fn a_request_inside_a_handed_callback_reads_its_method_from_the_helper_s_caller() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    if !script.exists() {
        assert!(
            std::env::var("CI").is_err(),
            "the built sidecar is missing in CI"
        );
        eprintln!("Skipping: the built sidecar is missing");
        return;
    }
    let rows = call_rows(&scan());
    let has = |row: &str| rows.iter().any(|known| known == row);
    let mut missing = Vec::new();
    for (case, row) in [
        (
            "the caller writes PUT; the model said PATCH",
            "src/tasks-api.ts:6 PUT /v1/tasks/:id/status request_summary",
        ),
        // The label's default GET is a fact row: the summaries cannot state
        // a verb for a caller that writes none, and the site path's row at
        // that caller no longer yields to the summary row a sibling caller
        // of the same request holds (C11 T1).
        (
            "the caller writes no method: the label's GET",
            "src/tasks-api.ts:10 GET /v1/tasks/:id request_summary",
        ),
        (
            "the caller's whole bag, its `url` and `method` read inside the span",
            "src/workspace-api.ts:15 DELETE /v1/workspaces/:id request_summary",
        ),
        (
            "control: no span around the request",
            "src/tasks-api.ts:14 PUT /v1/tasks/:id/archive request_summary",
        ),
        (
            "control: a destructured bag's rest, spread inside the span",
            "src/workspace-api.ts:9 GET /v1/workspaces/:id request_summary",
        ),
        (
            "control: the same, PATCH where the model said POST",
            "src/workspace-api.ts:12 PATCH /v1/workspaces/:id request_summary",
        ),
    ] {
        if !has(row) {
            missing.push(format!("{case}: no row `{row}`"));
        }
    }
    assert!(missing.is_empty(), "{missing:#?}\nrows: {rows:#?}");
}

/// V1 rule C2 is deleted: an awaited call with a string argument is no
/// call-site signal. `src/use-wrapped.ts` awaits a local wrapper with string
/// arguments and makes no other call, so no signal raises it: discovery
/// reaches `fetch` through `callApi` and `send`, and the fact layer states
/// both rows itself, whether or not the reader reads the file.
#[test]
fn a_call_through_a_local_fetch_wrapper_keeps_its_row_with_no_signal() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    if !script.exists() {
        assert!(
            std::env::var("CI").is_err(),
            "the built sidecar is missing in CI"
        );
        eprintln!("Skipping: the built sidecar is missing");
        return;
    }
    let (blob, log) = scan_with_log();
    let rows: Vec<String> = call_rows(&blob)
        .into_iter()
        .filter(|row| row.starts_with("src/use-wrapped.ts:"))
        .collect();
    assert_eq!(
        rows,
        vec![
            "src/use-wrapped.ts:4 POST /v1/items request_summary",
            "src/use-wrapped.ts:8 GET /v1/items request_summary",
        ],
        "both caller rows, stated by the fact layer"
    );
    assert!(
        log.contains("fact files: C3 candidates"),
        "the run log counts the register signals that remain:\n{log}"
    );
    let c2: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("fact files: C2 candidates"))
        .collect();
    assert!(c2.is_empty(), "the await signal is deleted: {c2:#?}");
}
