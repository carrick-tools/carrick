//! A wrapper whose URL runs through a helper call the text reader cannot
//! read (carrick#4022): the hole keeps the parameters its arguments read, so
//! the callers that choose the path are saved, not the request line. Runs
//! the real scanner binary on `.` over a committed copy of
//! `tests/fixtures/wrapper-url-through-helper`, offline, and reads the
//! stored blob.
//!
//! Protocols: HTTP (the request summaries are HTTP-only).

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wrapper-url-through-helper")
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

/// The unread sites saved at `file:line`.
fn sites_at(blob: &serde_json::Value, file: &str, line: u64) -> Vec<serde_json::Value> {
    blob["unread_sites"]
        .as_array()
        .expect("unread_sites written")
        .iter()
        .filter(|site| site["file"] == file && site["line"] == line)
        .cloned()
        .collect()
}

/// The possible calls kept at `file:line`.
fn leads_at(blob: &serde_json::Value, file: &str, line: u64) -> Vec<serde_json::Value> {
    blob["possible_calls"]
        .as_array()
        .map(|calls| {
            calls
                .iter()
                .filter(|call| call["file_path"] == file && call["line_number"] == line)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// The rows stated at `file:line` (a row's `file_path` carries its line).
fn rows_at(blob: &serde_json::Value, file: &str, line: u64) -> Vec<serde_json::Value> {
    let at = format!("{file}:{line}");
    blob["calls"]
        .as_array()
        .expect("calls written")
        .iter()
        .filter(|call| call["file_path"] == at.as_str())
        .cloned()
        .collect()
}

/// Exactly one record at `file:line`: one unread site with `reason`, at
/// `depth`.
fn one_site(blob: &serde_json::Value, file: &str, line: u64, reason: &str, depth: u64) {
    let sites = sites_at(blob, file, line);
    assert_eq!(sites.len(), 1, "{file}:{line} {sites:#?}");
    assert_eq!(sites[0]["reason"], reason, "{:#}", sites[0]);
    assert_eq!(sites[0]["slot"], "path", "{:#}", sites[0]);
    assert_eq!(sites[0]["depth"], depth, "{:#}", sites[0]);
}

/// Nothing at `file:line`: no row, no possible call, no unread site.
fn nothing_at(blob: &serde_json::Value, file: &str, line: u64) {
    assert!(rows_at(blob, file, line).is_empty(), "{file}:{line} row");
    assert!(leads_at(blob, file, line).is_empty(), "{file}:{line} lead");
    assert!(sites_at(blob, file, line).is_empty(), "{file}:{line} site");
}

/// The callers of `signedFetch` (a `let` helper) and `twoReturnFetch` (a
/// two-return helper) choose the path: each is saved once, with the reason
/// the helper's hole holds, and the `fetch` lines inside the wrappers keep
/// nothing of their own. The two-return helper's returns state different
/// texts, so its hole reads as several paths.
#[test]
fn callers_of_a_wrapper_whose_url_runs_through_an_unread_helper_are_saved() {
    let blob = scan();
    for (line, reason) in [
        (6, "unread_expression"),
        (10, "unread_expression"),
        (14, "several_paths"),
    ] {
        one_site(&blob, "src/use-badges.ts", line, reason, 1);
        assert_eq!(
            leads_at(&blob, "src/use-badges.ts", line).len(),
            1,
            "use-badges.ts:{line}"
        );
        assert!(rows_at(&blob, "src/use-badges.ts", line).is_empty());
    }
    nothing_at(&blob, "src/net/signed.ts", 12);
    nothing_at(&blob, "src/net/two-way.ts", 11);
}

/// A helper call whose argument holds no parameter decides the name where
/// it is written: `fixedStamp`'s request keeps its record.
#[test]
fn a_helper_call_with_no_parameter_keeps_its_record_at_the_request() {
    let blob = scan();
    one_site(&blob, "src/net/uncalled.ts", 17, "unread_expression", 0);
}

/// A wrapper of this shape that nothing in the service calls: the walk
/// finds no caller to fill the path, so the request line is saved as
/// `name_never_bound`.
#[test]
fn an_uncalled_wrapper_of_this_shape_is_saved_as_never_bound() {
    let blob = scan();
    one_site(&blob, "src/net/uncalled.ts", 12, "name_never_bound", 0);
}

/// A literal template, a one-return helper, an unread helper call that is
/// the base of a written path, and one that is a whole path segment still
/// state the route at their callers.
#[test]
fn the_controls_keep_their_rows() {
    let blob = scan();
    for (line, path) in [
        (18, "/api/v1/badge"),
        (22, "/api/v1/badge"),
        (26, "/badges"),
        (30, "/api/v1/badges/:id"),
    ] {
        let rows = rows_at(&blob, "src/use-badges.ts", line);
        assert_eq!(rows.len(), 1, "use-badges.ts:{line} {rows:#?}");
        assert_eq!(rows[0]["key"]["method"], "GET", "{:#}", rows[0]);
        assert_eq!(rows[0]["key"]["path"], path, "{:#}", rows[0]);
        assert!(sites_at(&blob, "src/use-badges.ts", line).is_empty());
        assert!(leads_at(&blob, "src/use-badges.ts", line).is_empty());
    }
    nothing_at(&blob, "src/net/plain.ts", 24);
    nothing_at(&blob, "src/net/plain.ts", 29);
}

/// The request-body type the compiler engine infers at `file:line`, as the
/// type manifest states it: `(type_state, expanded_definition)`.
fn request_body_at(blob: &serde_json::Value, file: &str, line: u64) -> Vec<(String, String)> {
    blob["type_manifest"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry["file_path"] == file
                && entry["line_number"] == line
                && entry["evidence"]["infer_kind"] == "request_body"
        })
        .map(|entry| {
            (
                entry["evidence"]["type_state"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                entry["expanded_definition"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// A wrapper whose URL is an unread helper call on its base parameter glued
/// to its path parameter (`${normalizeBase(siteUrl)}${path}`), called with a
/// base that is itself such a call and a written path: the caller states the
/// route, so it stays the site, and the body it sends keeps the type the
/// compiler engine inferred. The hole of the unread base closing at the
/// caller leaves the route stated.
#[test]
fn a_caller_that_writes_the_path_keeps_its_request_body_type() {
    let blob = scan();
    for (line, body) in [
        (38, "{ title: string; slug: string; }"),
        (47, "{ title: string; }"),
    ] {
        let types = request_body_at(&blob, "src/net/site-client.ts", line);
        assert_eq!(
            types,
            vec![("implicit".to_string(), body.to_string())],
            "site-client.ts:{line} request body"
        );
    }
}
