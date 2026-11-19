//! A maker-bound name a caller fills with a value no walk reads is a lead
//! with the resolver's reason (carrick#4520), not an asked site.
//!
//! Drives the real scanner binary on `.`, as the Action runs it, offline, with
//! the fixture's store answer replayed as the library store's. The fixture is
//! `maker-name-unread-caller` (one root service).
//!
//! Protocol: pub/sub (queues); the rule is protocol-free (any maker-bound op).

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
    /// Each pub/sub producer possible call, as `file:line slot reason`.
    producer_leads: BTreeSet<String>,
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
            let at = format!(
                "{}:{} {} {}",
                lead["file_path"].as_str().unwrap(),
                lead["line_number"],
                lead["unread"]["slot"].as_str().unwrap(),
                lead["unread"]["reason"].as_str().unwrap()
            );
            if lead["role"] == "producer" {
                found.producer_leads.insert(at);
            }
        }
        blobs.insert(service, found);
    }
    blobs
}

/// Each maker-bound send whose name a caller fills with a value the walk
/// cannot read is a producer lead naming the slot and the resolver's reason.
/// Stated names, a half-reached maker and the pooled whole field keep their
/// answer.
#[test]
fn a_maker_name_a_caller_fills_with_an_unread_value_says_why() {
    let blobs = scan("maker-name-unread-caller");
    let jobs = &blobs["jobs"];
    let expected: BTreeSet<String> = [
        "src/a-records.ts:7 name unread_expression",
        "src/b-one-caller.ts:6 name unread_expression",
        "src/c-entry-param.ts:6 name unread_expression",
        "src/d-no-caller.ts:6 name name_never_bound",
        "src/e-literal.ts:6 kind unanswered",
        "src/f-whole-field.ts:10 name unread_expression",
        "src/f-whole-field.ts:15 name unread_expression",
        "src/g-mixed.ts:6 name unread_expression",
        "src/h-ctor.ts:10 name unread_expression",
        "src/i-half-reached.ts:9 name unanswered",
        "src/k-callback.ts:8 name callback_parameter",
    ]
    .iter()
    .map(|line| line.to_string())
    .collect();
    assert_eq!(jobs.producer_leads, expected);
    assert!(jobs.publishers.is_empty(), "{:?}", jobs.publishers);
}
