//! Operation names written in a form the names lowering learns one step at
//! a time (carrick#3504 S2b): so far an object-pattern declaration
//! (`const { path } = ...`). Layout and key as in `constant_names_test.rs`.
//!
//! Drives the real scanner binary on `.`, as the Action runs it, over a
//! committed copy of `tests/fixtures/name-forms/repo` (one root
//! service), offline, with the fixture's store answer replayed as the
//! library store's. The key is `rows.expected.json`: each file of `names`
//! writes a fact row with that name, except a pub/sub file of `kind_asked`,
//! whose broker label leaves the kind to the model, so it stays a possible
//! call (its name is forced: the record-level test in discovery says so).
//! Each `unnamed` file, where a part does not fold, writes no fact row and
//! stays a possible call.
//!
//! Protocols: HTTP calls and routes, pub/sub topics, socket events.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/name-forms")
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

/// What one scan wrote.
struct Scan {
    /// The names of the rows not found by the model, by file.
    named: BTreeMap<String, BTreeSet<String>>,
    /// The files holding a possible call.
    leads: BTreeSet<String>,
}

fn scan() -> Scan {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir().join("repo"), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let cassettes = tmp.path().join("cassettes");
    std::fs::create_dir_all(cassettes.join("library-claims")).unwrap();
    std::fs::copy(
        fixture_dir().join("library-claims.json"),
        cassettes.join("library-claims/default.json"),
    )
    .unwrap();
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
    let mut named: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut leads = BTreeSet::new();
    let mut blobs = 0;
    for entry in std::fs::read_dir(&storage).expect("the store reads") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        blobs += 1;
        let blob: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for side in ["endpoints", "calls"] {
            for row in blob[side].as_array().into_iter().flatten() {
                if row["resolution_source"] == "model" {
                    continue;
                }
                let key = &row["key"];
                let name = ["path", "topic", "event", "field"]
                    .iter()
                    .find_map(|field| key[field].as_str())
                    .unwrap_or_default();
                let file = row["file_path"].as_str().unwrap_or_default();
                let file = file.rsplit_once(':').map_or(file, |(file, _)| file);
                named
                    .entry(file.to_string())
                    .or_default()
                    .insert(name.to_string());
            }
        }
        for lead in blob["possible_calls"].as_array().into_iter().flatten() {
            leads.insert(lead["file_path"].as_str().unwrap().to_string());
        }
    }
    assert_eq!(blobs, 1, "the scan stored one blob");
    Scan { named, leads }
}

fn key() -> serde_json::Value {
    serde_json::from_str(
        &std::fs::read_to_string(fixture_dir().join("rows.expected.json")).unwrap(),
    )
    .unwrap()
}

/// A name bound by an object pattern states its key's value, on every
/// protocol: a fact row with the whole name. One whose object is written
/// elsewhere is no fact row and stays a possible call.
#[test]
fn a_destructured_name_states_its_keys_value_on_every_protocol() {
    let scan = scan();
    let key = key();
    let mut wrong = Vec::new();
    let kind_asked: BTreeSet<&str> = key["kind_asked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file.as_str().unwrap())
        .collect();
    for (file, want) in key["names"].as_object().unwrap() {
        let want = want.as_str().unwrap();
        // A broker's label does not say topic or job name: the kind is
        // asked, so the row waits on the answer whatever the name.
        if kind_asked.contains(file.as_str()) {
            if !scan.leads.contains(file) {
                wrong.push(format!("{file}: want a possible call, got none"));
            }
            continue;
        }
        let got = scan.named.get(file).cloned().unwrap_or_default();
        if got != BTreeSet::from([want.to_string()]) {
            wrong.push(format!("{file}: want {want}, got {got:?}"));
        }
    }
    for file in key["unnamed"].as_array().unwrap() {
        let file = file.as_str().unwrap();
        if let Some(got) = scan.named.get(file) {
            wrong.push(format!("{file}: want no fact row, got {got:?}"));
        }
        if !scan.leads.contains(file) {
            wrong.push(format!("{file}: want a possible call, got none"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
