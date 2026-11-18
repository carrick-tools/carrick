//! A call into a package the service does not list, made on an instance a
//! listed, networked package built, is saved as an unread site with that
//! listed package's reason, `package_unmatched`: the library store is asked
//! only about listed packages, so nothing was asked about the package
//! declaring the call.
//!
//! Drives the real scanner binary on `.`, as the Action runs it, over a
//! committed copy of `tests/fixtures/unread-through-listed-client/repo` (one root
//! service), offline, with the fixture's store answer replayed as the library
//! store's. The key is `expected.json`.
//!
//! Protocols: the drop happens before a protocol is known, so the test is on
//! the call's declaring package, not on a role.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unread-through-listed-client")
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

/// The one blob the scan stored and the scan's run log; the fixture is
/// scanned once per run.
fn scan_all() -> &'static (serde_json::Value, String) {
    static SCAN: std::sync::OnceLock<(serde_json::Value, String)> = std::sync::OnceLock::new();
    SCAN.get_or_init(scan_once)
}

fn scan() -> serde_json::Value {
    scan_all().0.clone()
}

fn scan_once() -> (serde_json::Value, String) {
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
    let mut log = String::new();
    let runs = home.join(".carrick/logs/runs");
    if let Ok(entries) = std::fs::read_dir(&runs) {
        for entry in entries.flatten() {
            log.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
        }
    }
    (blobs.remove(0), log)
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

/// A call declared in an installed package the service does not list, made
/// through a listed client, is an unread site with `package_unmatched`; the
/// controls keep the reading they had.
#[test]
fn a_call_through_a_listed_client_is_saved_as_unread() {
    let blob = scan();
    let key = key();
    let mut wrong = Vec::new();
    for (at, want) in key["reasons"].as_object().unwrap() {
        let want = want.as_str().unwrap();
        let (file, line) = at.split_once(':').unwrap();
        let line: u64 = line.parse().unwrap();
        let sites = sites_of(&blob, file);
        if !sites
            .iter()
            .any(|site| site["reason"] == want && site["line"].as_u64() == Some(line))
        {
            wrong.push(format!("{at}: want {want}, got {sites:?}"));
        }
    }
    for (file, lines) in key["silent"].as_object().unwrap() {
        for line in lines.as_array().unwrap() {
            let sites = sites_of(&blob, file);
            if sites.iter().any(|site| site["line"] == *line) {
                wrong.push(format!("{file}:{line}: want no unread site, got {sites:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The counts by reason are the key's, whole: one call saved through the
/// listed client, one caller that enters it, and nothing else moved.
#[test]
fn the_counts_state_the_one_call_through_the_client() {
    let blob = scan();
    let want = key()["by_reason"].clone();
    assert_eq!(blob["unread_sites_by_reason"], want);
}

/// The run log names the pair the call arrived through.
#[test]
fn the_run_log_names_the_listed_package_and_the_declaring_one() {
    let want = format!(
        "arrived_through={}",
        key()["arrived_through"].as_str().unwrap()
    );
    let log = &scan_all().1;
    assert!(log.contains(&want), "no `{want}` in the run log:\n{log}");
}
