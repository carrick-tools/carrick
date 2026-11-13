//! One row per operation at a call line (carrick#3984): names a record
//! states that key alike once the query and fragment are stripped are one
//! row, in `calls`, in the mount graph's data calls and in the consumer
//! type manifest.
//!
//! Drives the real scanner binary on `.`, as the Action runs it, over a
//! committed copy of `tests/fixtures/one-row-per-operation`, offline. Rows
//! are collected into a list, never a set: a set hides the duplicate.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
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

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/one-row-per-operation")
}

/// The fixture scanned as the Action scans it; its stored blob.
fn scan() -> serde_json::Value {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("one-row-per-operation");
    copy_dir(&fixture_dir(), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let store = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(&repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", store.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("HOME", home.path())
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
    fs::read_dir(store.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap())
        .expect("the scan stored a blob")
}

fn repeats(values: &[serde_json::Value]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut repeated = Vec::new();
    for value in values {
        let text = value.to_string();
        if seen.contains(&text) {
            repeated.push(text);
        } else {
            seen.push(text);
        }
    }
    repeated
}

/// Each call line holds the operations the answer key lists, once each, and
/// no entry in the graph's data calls or the consumer manifest repeats.
#[test]
fn a_call_line_states_one_row_per_operation() {
    let blob = scan();
    let mut got: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for call in blob["calls"].as_array().into_iter().flatten() {
        if call["resolution_source"] == "model" {
            continue;
        }
        got.entry(call["file_path"].as_str().unwrap_or_default().to_string())
            .or_default()
            .push(format!(
                "{} {}",
                call["key"]["method"].as_str().unwrap_or_default(),
                call["key"]["path"].as_str().unwrap_or_default()
            ));
    }
    let key: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(fixture_dir().join("rows.expected.json")).unwrap(),
    )
    .unwrap();
    let mut want: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (line, rows) in key["calls"].as_object().unwrap() {
        let mut rows: Vec<String> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row.as_str().unwrap().to_string())
            .collect();
        rows.sort();
        want.insert(line.clone(), rows);
    }
    for rows in got.values_mut() {
        rows.sort();
    }
    assert_eq!(got, want);

    let data_calls = blob["mount_graph"]["data_calls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        repeats(&data_calls),
        Vec::<String>::new(),
        "data_calls repeat"
    );
    let manifest = blob["type_manifest"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        repeats(&manifest),
        Vec::<String>::new(),
        "type_manifest repeats"
    );
}
