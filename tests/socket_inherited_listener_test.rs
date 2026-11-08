//! A listener member a socket class inherits from another package is read
//! under the class the receiver holds (carrick#3390).
//!
//! Drives the real scanner binary on `.`, as the Action runs it, offline,
//! with the fixture's store answer replayed as the library store's. The
//! fixture's `Socket` class extends an `Emitter` declared in a second package;
//! the store labels `on`/`once` only under `Socket`.
//!
//! Protocols: socket (listeners). The lookup is protocol-free: any class a
//! base package declares members for, held by a receiver, reads the same way.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURE: &str = "socket-inherited-listener";

fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
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

/// Every endpoint row of the scan, as
/// `service file:line direction event resolution_source`.
fn endpoint_rows() -> BTreeSet<String> {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir(FIXTURE).join("repo"), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let cassettes = tmp.path().join("cassettes");
    std::fs::create_dir_all(cassettes.join("library-claims")).unwrap();
    std::fs::copy(
        fixture_dir(FIXTURE).join("library-claims.json"),
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
    let mut rows = BTreeSet::new();
    for entry in std::fs::read_dir(&storage).expect("the store reads") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let service = stem
            .strip_prefix("workspace__")
            .unwrap_or(&stem)
            .to_string();
        let blob: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for row in blob["endpoints"].as_array().into_iter().flatten() {
            rows.insert(format!(
                "{service} {} {} {} {}",
                row["file_path"].as_str().unwrap_or_default(),
                row["key"]["direction"].as_str().unwrap_or_default(),
                row["key"]["event"].as_str().unwrap_or_default(),
                row["resolution_source"].as_str().unwrap_or_default()
            ));
        }
    }
    rows
}

/// The three listeners (a module local, which the instance read already
/// decided, a parameter property, a getter receiver) are `library_claim` endpoint rows in the client, named by their
/// event.
#[test]
fn a_listener_inherited_from_a_base_package_is_a_library_row() {
    let rows = endpoint_rows();
    let listing = rows.iter().cloned().collect::<Vec<_>>().join("\n");
    for (line, event) in [(4, "presence"), (10, "ticker"), (22, "notice")] {
        let wanted = format!(
            "client services/client/src/client.ts:{line} server_to_client {event} library_claim"
        );
        assert!(
            rows.contains(&wanted),
            "missing `{wanted}`; scan holds:\n{listing}"
        );
    }
}
