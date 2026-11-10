//! A caller of a function the service declares, whose request the send side
//! could not write as a row, is saved as an unread site with its reason
//! (carrick#3688 F). Runs the real scanner binary on `.` over a committed copy
//! of `tests/fixtures/wrapper-caller-stops`, offline, and reads the stored blob.
//!
//! Protocols: HTTP (the request summaries are HTTP-only).

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wrapper-caller-stops")
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

fn sites_in(blob: &serde_json::Value, file: &str) -> Vec<serde_json::Value> {
    blob["unread_sites"]
        .as_array()
        .expect("unread_sites written")
        .iter()
        .filter(|site| site["file"] == file)
        .cloned()
        .collect()
}

/// A caller whose wrapper's route reads a field a subclass writes states no
/// row; it is saved at the call, with the labelled `fetch` it reaches.
#[test]
fn a_wrapper_caller_whose_request_does_not_render_is_an_unread_site() {
    let blob = scan();
    let sites = sites_in(&blob, "src/use-stores.ts");
    assert_eq!(sites.len(), 1, "{sites:#?}");
    let site = &sites[0];
    assert_eq!(site["line"], 6, "{site:#}");
    assert_eq!(site["slot"], "path", "{site:#}");
    assert_eq!(site["kind"], "http", "{site:#}");
    assert_eq!(site["role"], "consumer", "{site:#}");
    assert_eq!(
        site["labelled_end"]["package"], "lib:lib.dom.d.ts",
        "{site:#}"
    );
    assert_eq!(site["labelled_end"]["export"], "fetch", "{site:#}");
    assert!(
        site["callee"].as_str().is_some_and(|c| c.contains("list")),
        "{site:#}"
    );
    assert_eq!(site["reason"], "reassigned", "{site:#}");
}

/// A caller whose request renders as a row is no unread site.
#[test]
fn a_wrapper_caller_with_a_row_is_not_an_unread_site() {
    let blob = scan();
    assert!(sites_in(&blob, "src/use-plain.ts").is_empty());
    let rows: Vec<&serde_json::Value> = blob["calls"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|call| {
            call["file_path"]
                .as_str()
                .is_some_and(|p| p.contains("use-plain.ts"))
        })
        .collect();
    assert_eq!(rows.len(), 1, "{rows:#?}");
}
