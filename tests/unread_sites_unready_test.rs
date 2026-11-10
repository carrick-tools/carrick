//! A call into a library package the scan ended without an answer for is on
//! the index blob as an unread site with the reason `unanswered_package`
//! (carrick#3684).
//!
//! Drives the real scanner binary on `.`, as the Action runs it, over a
//! committed copy of `tests/fixtures/unread-sites-unready/repo` (one root
//! service), offline, with the fixture's store answer replayed as the library
//! store's: one package pending, one build-failed, one answered. The key is
//! `expected.json`.
//!
//! Protocols: the test is on the call's declaring package, not on a role; the
//! fixture's calls are a pub/sub-shaped publish and an event emit.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unread-sites-unready")
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

fn key() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(fixture_dir().join("expected.json")).unwrap())
        .unwrap()
}

fn sites_of(blob: &serde_json::Value, file: &str) -> Vec<serde_json::Value> {
    blob["unread_sites"]
        .as_array()
        .expect("unread_sites written")
        .iter()
        .filter(|site| site["file"] == file)
        .cloned()
        .collect()
}

/// Each file of the key holds an unread site with its reason (and `also`),
/// and the control holds none.
#[test]
fn a_call_into_a_package_the_scan_ended_without_is_saved_as_unread() {
    let blob = scan();
    let key = key();
    let mut wrong = Vec::new();
    for (file, want) in key["reasons"].as_object().unwrap() {
        let want = want.as_str().unwrap();
        let sites = sites_of(&blob, file);
        if !sites.iter().any(|site| site["reason"] == want) {
            wrong.push(format!("{file}: want {want}, got {sites:?}"));
        }
    }
    for (file, want) in key["also"].as_object().unwrap() {
        let sites = sites_of(&blob, file);
        if !sites.iter().any(|site| site["also"] == *want) {
            wrong.push(format!("{file}: want also {want}, got {sites:?}"));
        }
    }
    for file in key["none"].as_array().unwrap() {
        let file = file.as_str().unwrap();
        let sites = sites_of(&blob, file);
        if !sites.is_empty() {
            wrong.push(format!("{file}: want no unread site, got {sites:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The counts by reason, and the boundary's pending and failed counts.
#[test]
fn the_counts_and_the_boundary_state_the_unready_packages() {
    let blob = scan();
    let key = key();
    let by_reason = &blob["unread_sites_by_reason"];
    assert_eq!(
        by_reason["unanswered_package"].as_u64(),
        key["by_reason"]["unanswered_package"]["eq"].as_u64(),
        "{by_reason}"
    );
    assert!(
        by_reason["missed_inside"].as_u64().unwrap_or(0)
            >= key["by_reason"]["missed_inside"]["min"].as_u64().unwrap(),
        "{by_reason}"
    );
    let boundary = &blob["boundary"];
    for (name, want) in key["boundary"].as_object().unwrap() {
        assert_eq!(&boundary[name], want, "{name}: {boundary}");
    }
    assert!(
        boundary.get("library_ask_failed").is_none(),
        "the ask was answered: {boundary}"
    );
}
