//! A pub/sub topic template whose hole only a framework-invoked resolver
//! argument fills (carrick#3784): each template call is a saved site at its
//! own line, with the reason the walk met at the callers, and no pub/sub
//! row states a placeholder topic. The answer key is
//! `tests/fixtures/pubsub-template-hole/README.md`.
//!
//! Drives the real scanner binary on `.`, as the Action runs it, offline,
//! with the fixture's store answer replayed as the library store's.
//!
//! Protocols: pub/sub. The rule applies to every message role (pub/sub,
//! queues, sockets); HTTP is left as it was, since a whole-segment path
//! parameter already states a route at the template call.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURE: &str = "pubsub-template-hole";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(FIXTURE)
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

/// What the one service's blob holds for pub/sub.
struct Blob {
    /// Each publisher row, as `file:line topic`.
    publishers: BTreeSet<String>,
    /// The topic of every pub/sub row, either side.
    topics: BTreeSet<String>,
    /// Each pub/sub producer possible call, as `file:line reason`, one
    /// entry per call the blob lists (repeats kept, to count them).
    producer_leads: Vec<String>,
    /// Each pub/sub unread site, as `file:line reason`, repeats kept.
    unread: Vec<String>,
}

fn scan() -> Blob {
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
    let path = std::fs::read_dir(&storage)
        .expect("the store reads")
        .map(|entry| entry.expect("dir entry").path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .expect("one blob");
    let blob: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut found = Blob {
        publishers: BTreeSet::new(),
        topics: BTreeSet::new(),
        producer_leads: Vec::new(),
        unread: Vec::new(),
    };
    for side in ["endpoints", "calls"] {
        for row in blob[side].as_array().into_iter().flatten() {
            if row["key"]["protocol"] != "pubsub" {
                continue;
            }
            let topic = row["key"]["topic"].as_str().unwrap().to_string();
            if side == "calls" {
                // A call row's `file_path` carries its line (`file:line`).
                found
                    .publishers
                    .insert(format!("{} {topic}", row["file_path"].as_str().unwrap()));
            }
            found.topics.insert(topic);
        }
    }
    for lead in blob["possible_calls"].as_array().into_iter().flatten() {
        if lead["kind"] == "pubsub" && lead["role"] == "producer" {
            found.producer_leads.push(format!(
                "{}:{} {}",
                lead["file_path"].as_str().unwrap(),
                lead["line_number"],
                lead["unread"]["reason"].as_str().unwrap()
            ));
        }
    }
    for site in blob["unread_sites"].as_array().into_iter().flatten() {
        if site["kind"] == "pubsub" {
            found.unread.push(format!(
                "{}:{} {}",
                site["file"].as_str().unwrap(),
                site["line"],
                site["reason"].as_str().unwrap()
            ));
        }
    }
    found
}

/// The line of the publish call that writes `topic` in the fixture file
/// `file` (the call may open on the line before the template).
fn call_line(file: &str, topic: &str) -> usize {
    let text = std::fs::read_to_string(fixture_dir().join("repo").join(file)).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.contains(topic))
        .unwrap_or_else(|| panic!("{topic} in {file}"));
    (0..=at)
        .rev()
        .find(|&i| lines[i].contains("publish("))
        .expect("a publish call")
        + 1
}

fn at(file: &str, topic: &str, reason: &str) -> String {
    format!("{file}:{} {reason}", call_line(file, topic))
}

/// The first line of the fixture file `file` that holds `text`.
fn line_of(file: &str, text: &str) -> usize {
    let source = std::fs::read_to_string(fixture_dir().join("repo").join(file)).unwrap();
    source
        .lines()
        .position(|line| line.contains(text))
        .unwrap_or_else(|| panic!("{text} in {file}"))
        + 1
}

/// The entries of `listed` at `file`'s line `line`, whatever their reason.
fn entries_at(listed: &[String], file: &str, line: usize) -> Vec<String> {
    let prefix = format!("{file}:{line} ");
    listed
        .iter()
        .filter(|entry| entry.starts_with(&prefix))
        .cloned()
        .collect()
}

/// The three template shapes (a resolver argument beside an enum member, one
/// hole in two branches, a member of a decorated parameter beside a call
/// result) are each saved at the template call with the reason their
/// framework-invoked callers leave the hole open, exactly once in both blob
/// lists. The board templates have two resolver callers that stop for the
/// same reason (one entry). The settings template has two callers that stop
/// for different reasons: both blob lists keep one entry per call, the first
/// stop the walk meets, and discovered sites are walked in source order
/// (file, span), so it is the first caller's reason (`enableAll`, whose
/// argument is a callback's parameter). Each caller keeps its own entry with
/// its own reason.
#[test]
fn each_template_a_resolver_fills_is_saved_once_at_its_own_call() {
    let blob = scan();
    for (file, topic, reason) in [
        (
            "src/settings/settings.service.ts",
            "`setting/${name}/changed`",
            "callback_parameter",
        ),
        (
            "src/boards/board.service.ts",
            "`board_root/${boardId}/reordered`",
            "name_never_bound",
        ),
        (
            "src/boards/board.service.ts",
            "`board_child/${boardId}/reordered`",
            "name_never_bound",
        ),
        (
            "src/members/members.service.ts",
            "`owner/${ownerId}/invited`",
            "name_never_bound",
        ),
    ] {
        let want = vec![at(file, topic, reason)];
        let line = call_line(file, topic);
        assert_eq!(
            entries_at(&blob.producer_leads, file, line),
            want,
            "possible calls: {:?}",
            blob.producer_leads
        );
        assert_eq!(
            entries_at(&blob.unread, file, line),
            want,
            "unread sites: {:?}",
            blob.unread
        );
    }
    let resolver = "src/settings/settings.resolver.ts";
    for (text, reason) in [
        ("this.settings.toggleSetting(each", "callback_parameter"),
        (
            "return this.settings.toggleSetting(name",
            "name_never_bound",
        ),
    ] {
        let want = format!("{resolver}:{} {reason}", line_of(resolver, text));
        assert!(
            blob.producer_leads.contains(&want),
            "{want} lost: {:?}",
            blob.producer_leads
        );
    }
}

/// No pub/sub row states a topic with a hole: topics pair by exact text, so
/// a placeholder would pair on its spelling. The literal control is one
/// publisher row, the local-hole control stays saved at its own line, and
/// the resolver frames keep their own saved sites.
#[test]
fn no_placeholder_topic_is_stated_and_the_controls_hold() {
    let blob = scan();
    for topic in &blob.topics {
        assert!(
            !topic.contains('{') && !topic.contains(':'),
            "a placeholder topic is stated: {topic}"
        );
    }
    let board = "src/boards/board.service.ts";
    assert_eq!(
        blob.publishers,
        BTreeSet::from([format!(
            "{board}:{} board/created",
            call_line(board, "\"board/created\"")
        )])
    );
    assert!(
        blob.producer_leads.contains(&at(
            board,
            "`board/${boardId}/archived`",
            "unread_expression"
        )),
        "{:?}",
        blob.producer_leads
    );
    for (file, text) in [
        (
            "src/boards/board.resolver.ts",
            "return this.boards.reorder(boardId, parentId)",
        ),
        (
            "src/boards/board.resolver.ts",
            "return this.boards.reorder(boardId, null)",
        ),
        (
            "src/members/members.resolver.ts",
            "return this.members.inviteMember(",
        ),
    ] {
        let frame = format!("{file}:{} name_never_bound", line_of(file, text));
        assert!(
            blob.producer_leads.contains(&frame),
            "{frame} lost: {:?}",
            blob.producer_leads
        );
    }
}
