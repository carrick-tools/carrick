//! A verb decorator's array of paths: an element that is no literal states no
//! route, and the blob's unread sites hold it at its own line (a decorator
//! route array, V1: every site Carrick could not read is saved with a reason).
//!
//! Drives the real scanner binary on `.`, offline, over a committed copy of
//! the fixture, and reads the blob it writes.

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// The blob the scan of the fixture writes.
fn scan_blob() -> serde_json::Value {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("workspace");
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/decorator-route-array"),
        &repo,
    );
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
    let path = std::fs::read_dir(&storage)
        .expect("storage dir")
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .expect("the scan wrote a blob");
    serde_json::from_slice(&std::fs::read(path).expect("read blob")).expect("parse blob")
}

#[test]
fn a_non_literal_element_of_a_decorator_path_array_is_an_unread_site() {
    let blob = scan_blob();
    let sites: Vec<&serde_json::Value> = blob["unread_sites"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|site| {
            site["file"]
                .as_str()
                .is_some_and(|f| f.ends_with("hooks.controller.ts"))
        })
        .collect();
    assert_eq!(sites.len(), 1, "unread sites: {sites:#?}");
    let site = sites[0];
    assert_eq!(site["slot"], "path");
    assert_eq!(site["reason"], "unread_expression");
    // The element `MIRROR` in `@Post(["/hooks/refunds", MIRROR])`.
    assert_eq!(site["line"], 14);
    let endpoints = blob["endpoints"].as_array().expect("endpoints");
    assert!(
        endpoints
            .iter()
            .any(|row| row["key"]["path"] == "/api/hooks/refunds"),
        "the literal element still states its route: {endpoints:#?}"
    );
}
