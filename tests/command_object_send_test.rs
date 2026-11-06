//! A send written as a command object becomes a publisher row (carrick#3439
//! T1): `client.send(new SendMessageCommand({ QueueUrl, ... }))`, where the
//! class is the labelled export, its op is written in the `new` form and the
//! name is one key of the constructor's argument.
//!
//! Drives the real scanner binary on `.`, as the Action runs it, over a
//! committed copy of `tests/fixtures/command-object-send/repo` (one root
//! service), offline, with the fixture's store answer replayed as the library
//! store's. The key is `rows.expected.json`: a literal name is a publisher
//! row, a name read from the environment or a parameter is a possible call
//! (never dropped), and the rest of the blob's pub/sub is empty.
//!
//! Protocols: SQS and SNS sends (pub/sub). EventBridge and batch sends name
//! their topic inside an array (`Entries[].DetailType`), which the readers do
//! not walk yet; direct invokes and tables are other slices (see the
//! fixture README).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/command-object-send")
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

/// What one scan wrote for pub/sub.
struct Scan {
    /// Each publisher row, as `file:line topic resolution_source`.
    publishers: BTreeSet<String>,
    /// Each pub/sub producer possible call, as `file:line reason`.
    leads: BTreeSet<String>,
    /// Every pub/sub row of the blob, either side.
    pubsub_rows: usize,
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
    let mut publishers = BTreeSet::new();
    let mut leads = BTreeSet::new();
    let mut pubsub_rows = 0;
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
                if row["key"]["protocol"] != "pubsub" {
                    continue;
                }
                pubsub_rows += 1;
                if side == "calls" {
                    publishers.insert(format!(
                        "{} {} {}",
                        row["file_path"].as_str().unwrap(),
                        row["key"]["topic"].as_str().unwrap(),
                        row["resolution_source"].as_str().unwrap_or_default()
                    ));
                }
            }
        }
        for lead in blob["possible_calls"].as_array().into_iter().flatten() {
            if lead["kind"] == "pubsub" && lead["role"] == "producer" {
                leads.insert(format!(
                    "{}:{} {}",
                    lead["file_path"].as_str().unwrap(),
                    lead["line_number"],
                    lead["unread"]["reason"].as_str().unwrap()
                ));
            }
        }
    }
    assert_eq!(blobs, 1, "the scan stored one blob");
    Scan {
        publishers,
        leads,
        pubsub_rows,
    }
}

fn key(what: &str) -> BTreeSet<String> {
    let key: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture_dir().join("rows.expected.json")).unwrap(),
    )
    .unwrap();
    key[what]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap().to_string())
        .collect()
}

/// A command object's literal name is a publisher row at the line of the
/// `new`; a name from the environment, or from a parameter every caller
/// fills with an environment read, is a possible call and never dropped.
#[test]
fn a_command_object_send_is_a_publisher_row_or_a_possible_call() {
    let scan = scan();
    assert_eq!(scan.publishers, key("publishers"));
    assert_eq!(scan.leads, key("possible_calls"));
    assert_eq!(
        scan.pubsub_rows,
        key("publishers").len(),
        "no other pub/sub row is written"
    );
}
