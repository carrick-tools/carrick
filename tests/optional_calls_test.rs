//! carrick#3044 S3: an optional call (`x?.m(...)`) states the row its plain
//! twin states, on every protocol.
//!
//! The scan decided every optional-call site of a library and then dropped
//! it at the record step, which found calls and constructions only: no row
//! and no lead. Drives the real scanner binary on `.`, as the Action runs
//! it, over a committed copy of `tests/fixtures/receiver-forms-optional/repo`,
//! offline, with the fixture's store answer replayed and no model answer.
//! The answer key is `tests/fixtures/receiver-forms-optional/expected.json`;
//! its README names each site.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/receiver-forms-optional")
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

/// The stored blob's rows, each as `file:line protocol [METHOD] name`: the
/// requests and sends in `calls`, the routes and receives in `endpoints`.
fn rows() -> (BTreeSet<String>, BTreeSet<String>) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir().join("repo"), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let storage = tmp.path().join("storage");
    let home = tmp.path().join("home");
    let cassettes = tmp.path().join("cassettes");
    std::fs::create_dir_all(&storage).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(cassettes.join("library-claims")).unwrap();
    std::fs::copy(
        fixture_dir().join("library-claims.json"),
        cassettes.join("library-claims/default.json"),
    )
    .unwrap();

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
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_ALLOW_UNPREPARED", "1");
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
    let blobs: Vec<serde_json::Value> = std::fs::read_dir(&storage)
        .expect("the store reads")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| serde_json::from_slice(&std::fs::read(path).ok()?).ok())
        .collect();
    assert_eq!(blobs.len(), 1, "the root service uploads one blob");
    let render = |side: &str| -> BTreeSet<String> {
        blobs[0][side]
            .as_array()
            .unwrap_or_else(|| panic!("{side} is a list"))
            .iter()
            .map(|row| {
                let key = &row["key"];
                let part = |field: &str| key[field].as_str().map(str::to_string);
                let name = ["path", "topic", "event"]
                    .iter()
                    .find_map(|field| part(field))
                    .unwrap_or_default();
                let method = part("method").map(|m| format!("{m} ")).unwrap_or_default();
                format!(
                    "{} {} {method}{name}",
                    row["file_path"].as_str().unwrap_or_default(),
                    part("protocol").unwrap_or_default(),
                )
            })
            .collect()
    };
    (render("calls"), render("endpoints"))
}

fn key(what: &str) -> BTreeSet<String> {
    let key: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture_dir().join("expected.json")).unwrap(),
    )
    .unwrap();
    key[what]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap().to_string())
        .collect()
}

/// An optional call is the site its plain twin is, and keeps the record its
/// plain twin has: a request through an imported client, a typed field, a
/// union-typed constant and a maker's instance; a send on a broker and on an
/// emitter field; a route on an app; a receive on an emitter field. Each
/// states a row, never nothing.
#[test]
fn optional_calls_state_rows_on_every_protocol() {
    let (calls, endpoints) = rows();
    assert_eq!(calls, key("calls"), "{calls:#?}");
    assert_eq!(endpoints, key("endpoints"), "{endpoints:#?}");
}
