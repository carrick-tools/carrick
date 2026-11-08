//! A dependency installed as an npm alias is asked of the library store
//! under its real package (carrick#3571, G3) and its rows land on the code
//! that imports the alias.
//!
//! `package.json` declares `"wirelink-v4": "npm:wirelink@4.x"`; the code
//! imports `wirelink-v4`; the store knows only `wirelink`. Drives the real
//! scanner binary on `.`, over a committed copy of
//! `tests/fixtures/npm-alias-install/`, offline, with the fixture's store
//! answer (for `wirelink`) replayed. The fixture's README is the answer key.
//!
//! Protocols: socket calls. The alias reading is protocol-free (it decides
//! what is asked, before any role); only the socket client row writer is
//! exercised here.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/npm-alias-install")
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

/// The socket rows `(side, key, file_path, resolution_source)` of a scan of
/// the fixture, sorted.
fn scan() -> Vec<(String, String, String, Option<String>)> {
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
    let mut rows = Vec::new();
    for entry in std::fs::read_dir(&storage).expect("storage dir") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let blob: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read blob")).expect("parse blob");
        for side in ["endpoints", "calls"] {
            for entry in blob[side].as_array().expect("blob side is an array") {
                let key = &entry["key"];
                if key["protocol"].as_str() != Some("socket") {
                    continue;
                }
                rows.push((
                    side.to_string(),
                    format!(
                        "socket|{}|{}",
                        key["direction"].as_str().unwrap(),
                        key["event"].as_str().unwrap()
                    ),
                    entry["file_path"].as_str().expect("file_path").to_string(),
                    entry
                        .get("resolution_source")
                        .and_then(|source| source.as_str())
                        .map(str::to_string),
                ));
            }
        }
    }
    rows.sort();
    rows
}

/// The store is asked about `wirelink`, never `wirelink-v4`; its answer
/// (stated under `wirelink`) is read back on the import of `wirelink-v4`,
/// so each `send` on the aliased client is a library-claim socket call.
#[test]
fn an_aliased_dependency_s_library_rows_land_on_the_code_that_imports_the_alias() {
    let rows = scan();
    let expected: Vec<(String, String, String, Option<String>)> = [
        ("hub.join", "src/client.ts:6"),
        ("hub.leave", "src/client.ts:10"),
    ]
    .into_iter()
    .map(|(event, at)| {
        (
            "calls".to_string(),
            format!("socket|client_to_server|{event}"),
            at.to_string(),
            Some("library_claim".to_string()),
        )
    })
    .collect();
    assert_eq!(rows, expected);
}
