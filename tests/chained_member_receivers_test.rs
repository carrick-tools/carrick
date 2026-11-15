//! A member called on a call's value or on a `new`'s value is keyed by the
//! type the compiler gives that value, as a member called on a held name is
//! (carrick#4498, READY-4498 R1-R7).
//!
//! Drives the real scanner binary on `.`, as the Action runs it, offline, with
//! the fixture's store answer replayed as the library store's. One root
//! service; four stub packages: an HTTP client, a pub/sub hub, a queue client
//! and a pub/sub client whose maker the verifier cannot answer.
//!
//! Protocols: HTTP, pub/sub (queues). The reach is protocol-free; the rows
//! name which protocol each fixture line belongs to.

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

/// One scan of the fixture: its `unread_sites` rows, written
/// `file:line reason slot export.member` (`-` where the labelled end is
/// null), and its `calls` rows (their `file_path` carries the line), written `file:line METHOD path`, both sorted.
fn scan(fixture: &str) -> (Vec<String>, Vec<String>) {
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
    let mut unread = Vec::new();
    let mut calls = Vec::new();
    for entry in std::fs::read_dir(&storage).expect("the store reads") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let blob: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for row in blob["unread_sites"].as_array().into_iter().flatten() {
            let end = match row["labelled_end"].as_object() {
                None => "-".to_string(),
                Some(end) => {
                    let export = end["export"].as_str().unwrap_or("-");
                    match end["member"].as_str() {
                        Some(member) => format!("{export}.{member}"),
                        None => export.to_string(),
                    }
                }
            };
            unread.push(format!(
                "{}:{} {} {} {}",
                row["file"].as_str().unwrap(),
                row["line"],
                row["reason"].as_str().unwrap(),
                row["slot"].as_str().unwrap_or("-"),
                end
            ));
        }
        for row in blob["calls"].as_array().into_iter().flatten() {
            let key = &row["key"];
            if key["protocol"] != "http" {
                continue;
            }
            calls.push(format!(
                "{} {} {}",
                row["file_path"].as_str().unwrap(),
                key["method"].as_str().unwrap(),
                key["path"].as_str().unwrap()
            ));
        }
    }
    unread.sort();
    calls.sort();
    (unread, calls)
}

#[test]
fn a_member_on_a_call_or_new_value_is_keyed_by_the_type_the_compiler_gives_it() {
    let (unread, calls) = scan("chained-member-receivers");
    let expected_unread = vec![
        "src/http.ts:12 no_op_claim path Courier.area",
        "src/http.ts:12 no_op_claim path Courier.connect",
        "src/http.ts:16 no_op_claim path Courier.connect",
        "src/http.ts:18 no_op_claim path Courier.area",
        "src/http.ts:5 no_op_claim path Courier.connect",
        "src/http.ts:6 no_op_claim path Courier.area",
        "src/pubsub.ts:13 no_op_claim name Hub.region",
        "src/pubsub.ts:13 no_op_claim name Region.channel",
        "src/pubsub.ts:13 unanswered name Channel.publish",
        "src/pubsub.ts:18 no_op_claim name Hub",
        "src/pubsub.ts:18 no_op_claim name Hub.region",
        "src/pubsub.ts:18 no_op_claim name Region.channel",
        "src/pubsub.ts:18 unanswered name Channel.publish",
        "src/pubsub.ts:5 no_op_claim name Hub",
        "src/pubsub.ts:6 no_op_claim name Hub.region",
        "src/pubsub.ts:7 no_op_claim name Region.channel",
        "src/pubsub.ts:8 unanswered name Channel.publish",
        "src/queue.ts:12 no_op_claim name Jobs.queue",
        "src/queue.ts:12 no_op_claim name connect",
        "src/queue.ts:12 unanswered kind Queue.add",
        "src/queue.ts:17 no_op_claim name Jobs",
        "src/queue.ts:21 no_op_claim name Jobs.queue",
        "src/queue.ts:21 unanswered kind Queue.add",
        "src/queue.ts:32 value_callee callee -",
        "src/queue.ts:5 no_op_claim name Jobs",
        "src/queue.ts:6 no_op_claim name Jobs.queue",
        "src/queue.ts:7 unanswered kind Queue.add",
        "src/unverified.ts:13 no_op_claim name Pool.pipe",
        "src/unverified.ts:13 no_op_claim name Tap.pool",
        "src/unverified.ts:13 unanswered name Pipe.write",
        "src/unverified.ts:18 no_op_claim name Pool.pipe",
        "src/unverified.ts:18 no_op_claim name Tap",
        "src/unverified.ts:18 no_op_claim name Tap.pool",
        "src/unverified.ts:18 unanswered name Pipe.write",
        "src/unverified.ts:5 no_op_claim name Tap",
        "src/unverified.ts:6 no_op_claim name Tap.pool",
        "src/unverified.ts:7 no_op_claim name Pool.pipe",
        "src/unverified.ts:8 unanswered name Pipe.write",
    ];
    assert_eq!(unread, expected_unread);
    // The sends are the same on both builds: no `calls` row moves.
    assert_eq!(
        calls,
        vec![
            "src/http.ts:12 POST /users",
            "src/http.ts:18 GET /orders",
            "src/http.ts:7 GET /users",
        ]
    );
}
