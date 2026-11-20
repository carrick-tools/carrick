//! Calls on what a scope member returns (`db.from("notes").insert(...)`)
//! state one row each: the scope call's argument is the name, the library's
//! prefix stands before it, and the member's own method is the row's. An op
//! that applies only where a slot holds a literal (`select` with `{ head:
//! true }` sends HEAD) is taken over the op without that condition, and a
//! slot that is not a literal states no row and is saved as an unread site.
//!
//! Drives the real scanner binary on `.`, offline, over a committed copy of
//! `tests/fixtures/hosted-tables` (its answer key is the README there), with
//! the fixture's `library-claims.json` replayed as the store's answer.

use std::collections::BTreeSet;
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

/// The one blob a scan of the fixture writes (one service at the root).
fn scan() -> serde_json::Value {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("workspace");
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hosted-tables"),
        &repo,
    );
    std::fs::create_dir_all(repo.join("__llm__/library-claims")).unwrap();
    std::fs::copy(
        repo.join("library-claims.json"),
        repo.join("__llm__/library-claims/default.json"),
    )
    .unwrap();
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
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
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
    let mut blobs: Vec<serde_json::Value> = std::fs::read_dir(&storage)
        .expect("storage dir")
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| serde_json::from_slice(&std::fs::read(path).unwrap()).expect("parse blob"))
        .collect();
    assert_eq!(blobs.len(), 1, "one service, one blob");
    blobs.remove(0)
}

/// (`file:line`, method, path, label member) of every call row.
fn call_rows(blob: &serde_json::Value) -> BTreeSet<(String, String, String, Option<String>)> {
    blob["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            (
                row["file_path"].as_str().unwrap_or_default().to_string(),
                row["key"]["method"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                row["key"]["path"].as_str().unwrap_or_default().to_string(),
                row["label_member"].as_str().map(str::to_string),
            )
        })
        .collect()
}

/// The README's consumer rows.
fn expected() -> BTreeSet<(String, String, String, Option<String>)> {
    [
        ("src/read.ts:4", "GET", "/rest/v1/accounts", Some("select")),
        ("src/write.ts:4", "POST", "/rest/v1/notes", Some("insert")),
        // `.select()` on what `insert` returns is not the builder's `select`.
        ("src/written.ts:4", "POST", "/rest/v1/notes", Some("insert")),
        (
            "src/upsert.ts:4",
            "POST",
            "/rest/v1/accounts",
            Some("upsert"),
        ),
        ("src/edit.ts:4", "PATCH", "/rest/v1/notes", Some("update")),
        ("src/edit.ts:8", "DELETE", "/rest/v1/notes", Some("delete")),
        (
            "src/count.ts:4",
            "HEAD",
            "/rest/v1/accounts",
            Some("select"),
        ),
        // The table name is a module constant the value resolver reads.
        ("src/name.ts:6", "GET", "/rest/v1/notes", Some("select")),
        // A table the type file lacks still states its row.
        ("src/stale.ts:4", "GET", "/rest/v1/ghosts", Some("select")),
        // An op on the client itself, not on a scope's result: no member.
        ("src/fn.ts:4", "POST", "/rest/v1/rpc/bump_count", None),
    ]
    .into_iter()
    .map(|(at, method, path, member)| {
        (
            at.to_string(),
            method.to_string(),
            path.to_string(),
            member.map(str::to_string),
        )
    })
    .collect()
}

#[test]
fn each_table_call_states_its_row_with_its_method_and_prefix() {
    let blob = scan();
    assert_eq!(call_rows(&blob), expected());
}

#[test]
fn a_head_key_that_is_not_a_literal_states_no_row_and_is_saved() {
    let blob = scan();
    // Every site saved because its call sends one op or another at run
    // time, with the labelled member it is a call of.
    let saved: Vec<(String, u64, String)> = blob["unread_sites"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|site| site["reason"] == "several_operations")
        .filter_map(|site| {
            Some((
                site["file"].as_str()?.to_string(),
                site["line"].as_u64()?,
                site["labelled_end"]["member"].as_str()?.to_string(),
            ))
        })
        .collect();
    assert_eq!(
        saved,
        vec![("src/maybe.ts".to_string(), 5, "select".to_string())]
    );
}
