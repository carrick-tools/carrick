//! A queue named by an enum passed up a base-class constructor (carrick#3527),
//! pinned as it reads today. Slice S1 holds only what already passes; the
//! rows the later slices add are listed in each fixture's README and land in
//! the PR that turns them green.
//!
//! Drives the real scanner binary on `.`, as the Action runs it, offline, with
//! the fixture's store answer replayed as the library store's. Two fixtures:
//! `queue-enum-base-class` (five services, the issue's layout) and
//! `queue-enum-base-class-one` (one root service with control files).
//!
//! Protocols: pub/sub (queues). Constructor and `super(...)` reach and
//! maker-bound names are protocol-free; the library here is a queue client.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// What one service's blob holds for pub/sub.
#[derive(Default)]
struct Blob {
    /// Each publisher row, as `file:line topic resolution_source`.
    publishers: BTreeSet<String>,
    /// Each pub/sub producer possible call, as `file:line reason`.
    producer_leads: BTreeSet<String>,
    /// Every pub/sub row of the blob, either side.
    pubsub_rows: usize,
    /// The topic of every pub/sub row, either side.
    topics: BTreeSet<String>,
    /// Pub/sub possible calls of either role.
    pubsub_leads: usize,
}

/// One scan of the fixture, each service's blob by service name.
fn scan(fixture: &str) -> BTreeMap<String, Blob> {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir(fixture).join("repo"), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let cassettes = tmp.path().join("cassettes");
    std::fs::create_dir_all(cassettes.join("library-claims")).unwrap();
    std::fs::copy(
        fixture_dir(fixture).join("library-claims.json"),
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
    let mut blobs = BTreeMap::new();
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
        let mut found = Blob::default();
        for side in ["endpoints", "calls"] {
            for row in blob[side].as_array().into_iter().flatten() {
                if row["key"]["protocol"] != "pubsub" {
                    continue;
                }
                found.pubsub_rows += 1;
                found
                    .topics
                    .insert(row["key"]["topic"].as_str().unwrap().to_string());
                if side == "calls" {
                    found.publishers.insert(format!(
                        "{} {} {}",
                        row["file_path"].as_str().unwrap(),
                        row["key"]["topic"].as_str().unwrap(),
                        row["resolution_source"].as_str().unwrap_or_default()
                    ));
                }
            }
        }
        for lead in blob["possible_calls"].as_array().into_iter().flatten() {
            if lead["kind"] != "pubsub" {
                continue;
            }
            found.pubsub_leads += 1;
            if lead["role"] == "producer" {
                found.producer_leads.insert(format!(
                    "{}:{} {}",
                    lead["file_path"].as_str().unwrap(),
                    lead["line_number"],
                    lead["unread"]["reason"].as_str().unwrap()
                ));
            }
        }
        blobs.insert(service, found);
    }
    blobs
}

fn key(fixture: &str, what: &str) -> BTreeSet<String> {
    let key: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture_dir(fixture).join("rows.expected.json")).unwrap(),
    )
    .unwrap();
    key[what]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap().to_string())
        .collect()
}

/// The sender's two sends through the base-class wrapper stay producer
/// leads (the boundary rule moved them in); the bystander, which only builds
/// the wrapper's client, states nothing; and no pub/sub row in any service
/// is keyed on the job name the wrapper passes (`job`).
#[test]
fn the_senders_wrapped_sends_stay_leads_and_no_row_is_named_job() {
    let blobs = scan("queue-enum-base-class");
    let fixture = "queue-enum-base-class";
    assert_eq!(
        blobs.keys().cloned().collect::<BTreeSet<_>>(),
        key(fixture, "services"),
        "one blob per service"
    );
    let sender = &blobs["sender"];
    assert_eq!(sender.producer_leads, key(fixture, "sender_possible_calls"));
    assert!(sender.publishers.is_empty(), "{:?}", sender.publishers);
    let bystander = &blobs["bystander"];
    assert_eq!(bystander.pubsub_rows, 0);
    assert_eq!(bystander.pubsub_leads, 0);
    for (service, blob) in &blobs {
        assert!(
            !blob.topics.contains("job"),
            "{service} states a row named for the job: {:?}",
            blob.topics
        );
    }
}

/// With the store's corrected label, a queue named by a literal is one
/// publisher row, from the library-site reader, at the `.add` of its
/// module-scope queue; nothing else the controls hold states a pub/sub row,
/// and no row is named for a job (`job`, `direct-job`).
#[test]
fn a_literal_name_is_one_row_and_no_row_is_named_for_a_job() {
    let fixture = "queue-enum-base-class-one";
    let blobs = scan(fixture);
    let jobs = &blobs["jobs"];
    assert_eq!(jobs.publishers, key(fixture, "publishers"));
    assert_eq!(jobs.pubsub_rows, key(fixture, "publishers").len());
    for topic in ["job", "direct-job"] {
        assert!(!jobs.topics.contains(topic), "a row is named {topic}");
    }
}
