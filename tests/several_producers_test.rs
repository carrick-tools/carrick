//! carrick#4617: a call whose route two services serve is not a pairing the
//! scan can state. Nothing says which service the call's base reaches, so each
//! pair is a candidate with its own reason, never a `fact` that fails the PR
//! check on the wrong service.
//!
//! The fixture is `tests/fixtures/several-producers` (its README holds the
//! answer key): three repos in one shared store, and a PR on `web` that
//! regenerates a client's path. Mocked ($0, no model); the type pairs run the
//! real compiler engine.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/several-producers")
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git failed to spawn");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A fresh git repo holding a copy of one fixture service at its base commit.
fn repo(root: &Path, name: &str) -> PathBuf {
    let repo = std::fs::canonicalize(root).unwrap().join(name);
    copy_dir(&fixture().join(name), &repo);
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    repo
}

/// The scanner over `repo`, as the Action runs it: from inside the checkout,
/// with path `.`, replaying the fixture's model answers into the shared store.
/// With `event` it is a pull-request scan and returns the result it posted.
fn scan(repo: &Path, store: &Path, event: Option<&Path>) -> Option<serde_json::Value> {
    let out = repo.join("pr-result.json");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(repo)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", fixture().join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_LOCAL_STORAGE_DIR", store);
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_EVENT_PATH",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "CI",
        // A fill scan with this set writes nothing to the store.
        "CARRICK_OUTPUT_JSON",
        "CARRICK_LOCAL_STORAGE_ISOLATE",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    if let Some(event) = event {
        cmd.env("GITHUB_EVENT_PATH", event)
            .env("GITHUB_EVENT_NAME", "pull_request")
            .env("GITHUB_REF", "refs/pull/7/merge")
            .env("CARRICK_PR_RESULT_OUT", &out);
    }
    let run = cmd.output().expect("failed to spawn carrick");
    assert!(
        run.status.success(),
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    event.map(|_| {
        serde_json::from_str(&std::fs::read_to_string(&out).expect("the PR run posted no result"))
            .unwrap()
    })
}

/// What the PR run posts for `web` after the regenerated-client edit, with
/// the named services holding their base index in the store.
fn posted_findings(serving: &[&str]) -> Vec<serde_json::Value> {
    let root = tempfile::tempdir().unwrap();
    let store = root.path().join("store");
    std::fs::create_dir_all(&store).unwrap();
    for name in serving {
        scan(&repo(root.path(), name), &store, None);
    }
    let web = repo(root.path(), "web");
    scan(&web, &store, None);
    let base = git(&web, &["rev-parse", "HEAD"]);
    let client = web.join("src/gen/services/ShopService.ts");
    let source = std::fs::read_to_string(&client).unwrap();
    assert!(source.contains("/api/v1/refund-old/${refundId}"));
    std::fs::write(
        &client,
        source.replace("/api/v1/refund-old/", "/api/v1/refunds/"),
    )
    .unwrap();
    git(&web, &["add", "-A"]);
    git(&web, &["commit", "-q", "-m", "regen client"]);
    let event = root.path().join("event.json");
    std::fs::write(
        &event,
        serde_json::json!({ "pull_request": { "base": { "sha": base } } }).to_string(),
    )
    .unwrap();
    let result = scan(&web, &store, Some(&event)).unwrap();
    result["findings"].as_array().cloned().unwrap_or_default()
}

fn field<'a>(row: &'a serde_json::Value, key: &str) -> &'a str {
    row[key].as_str().unwrap_or("-")
}

fn type_mismatches(findings: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    findings
        .iter()
        .filter(|row| row["kind"] == "type_mismatch")
        .collect()
}

#[test]
fn a_route_two_services_serve_is_a_candidate_with_its_own_reason() {
    let findings = posted_findings(&["api", "legacy"]);
    let rows = type_mismatches(&findings);
    assert!(!rows.is_empty(), "{findings:#?}");
    for row in rows {
        assert_eq!(field(row, "edge_source"), "candidate", "{row:#}");
        assert_eq!(field(row, "break"), "candidate", "{row:#}");
        assert_eq!(
            field(row, "break_reason"),
            "several_services_serve_this_call",
            "{row:#}"
        );
    }
}

#[test]
fn the_right_producer_alone_posts_no_finding() {
    let findings = posted_findings(&["api"]);
    assert!(type_mismatches(&findings).is_empty(), "{findings:#?}");
}

#[test]
fn one_producer_of_the_route_stays_a_fact_and_certain() {
    let findings = posted_findings(&["legacy"]);
    let rows = type_mismatches(&findings);
    assert!(!rows.is_empty(), "{findings:#?}");
    for row in rows {
        assert_eq!(field(row, "edge_source"), "fact", "{row:#}");
        assert_eq!(field(row, "break"), "certain", "{row:#}");
        assert_eq!(field(row, "break_reason"), "-", "{row:#}");
    }
}
