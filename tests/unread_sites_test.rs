//! Every call site the fact layer considered and did not state as a row is on
//! the index blob as `unread_sites`, with its reason (carrick#3688 U3).
//!
//! Drives the real scanner binary on `.`, as the Action runs it, over a
//! committed copy of `tests/fixtures/unread-sites/repo` (one root service),
//! offline, with the fixture's store answer replayed as the library store's.
//! The key is `expected.json`: each file of `reasons` holds an unread site
//! with that reason, each file of `none` holds none. `blob.expected.json` is
//! the scanned blob's `unread_sites*` and `possible_calls` keys, which the
//! cloud's readers pin by hash.
//!
//! Protocols: HTTP. A walk that stops before any labelled call names no
//! protocol at all; the possible-call projection is protocol-blind.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unread-sites")
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
    run_scan(false)
}

/// The `CARRICK_OUTPUT_JSON` output of the same scan: it stores no blob.
fn scan_json() -> serde_json::Value {
    static OUT: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    OUT.get_or_init(|| run_scan(true)).clone()
}

fn run_scan(json: bool) -> serde_json::Value {
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
    if json {
        cmd.env("CARRICK_OUTPUT_JSON", "1");
    }
    let output = cmd.output().expect("spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if json {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let start = stdout
            .find("{\n")
            .expect("the JSON output starts with an object");
        return serde_json::from_str(&stdout[start..]).expect("the JSON output parses");
    }
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

fn key() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(fixture_dir().join("expected.json")).unwrap())
        .unwrap()
}

/// The reasons the blob's unread sites hold, by file.
fn reasons(blob: &serde_json::Value) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for site in blob["unread_sites"]
        .as_array()
        .expect("unread_sites written")
    {
        out.entry(site["file"].as_str().unwrap().to_string())
            .or_default()
            .insert(site["reason"].as_str().unwrap().to_string());
    }
    out
}

/// Each file of the key holds an unread site with its reason, and a file
/// whose call the scan states holds none. The totals count every row.
#[test]
fn every_unread_site_is_on_the_blob_with_its_reason() {
    let blob = scan();
    let got = reasons(&blob);
    let key = key();
    let mut wrong = Vec::new();
    for (file, want) in key["reasons"].as_object().unwrap() {
        let want = want.as_str().unwrap();
        if !got.get(file).is_some_and(|found| found.contains(want)) {
            wrong.push(format!("{file}: want {want}, got {:?}", got.get(file)));
        }
    }
    for file in key["none"].as_array().unwrap() {
        let file = file.as_str().unwrap();
        if let Some(found) = got.get(file) {
            wrong.push(format!("{file}: want no unread site, got {found:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    let rows = blob["unread_sites"].as_array().unwrap().len() as u64;
    assert_eq!(blob["unread_sites_total"].as_u64(), Some(rows));
    let by_reason: u64 = blob["unread_sites_by_reason"]
        .as_object()
        .unwrap()
        .values()
        .map(|count| count.as_u64().unwrap())
        .sum();
    assert_eq!(by_reason, rows);
}

/// A plain call the walk enters no code for (its callee is a value that is
/// no definition) is saved: before this field it was in memory only.
#[test]
fn a_plain_call_the_walk_enters_no_code_for_is_saved() {
    let got = reasons(&scan());
    assert!(
        got.get("src/value-callee.ts")
            .is_some_and(|found| found.contains("value_callee")),
        "{got:?}"
    );
}

/// A walk that stopped with no labelled end is saved with its stop: one that
/// reached a function it had already passed, and one that reached a
/// declaration with no body.
#[test]
fn a_walk_that_stops_with_no_labelled_end_is_saved() {
    let blob = scan();
    let got = reasons(&blob);
    for (file, want) in [
        ("src/recursion.ts", "recursion"),
        ("src/declared.ts", "interface_member"),
    ] {
        assert!(
            got.get(file).is_some_and(|found| found.contains(want)),
            "{file}: want {want}, got {got:?}"
        );
    }
}

/// `possible_calls` is the projection of the unread sites' possible calls:
/// each possible call is an unread site with the same place, reason, slot,
/// labelled end and depth.
#[test]
fn possible_calls_is_the_labelled_projection() {
    let blob = scan();
    let possible = blob["possible_calls"].as_array().expect("possible_calls");
    assert!(!possible.is_empty(), "the fixture holds a possible call");
    let unread = blob["unread_sites"].as_array().unwrap();
    for call in possible {
        let found = unread.iter().any(|site| {
            site["file"] == call["file_path"]
                && site["line"] == call["line_number"]
                && site["span_start"] == call["span_start"]
                && site["span_end"] == call["span_end"]
                && site["reason"] == call["unread"]["reason"]
                && site["slot"] == call["unread"]["slot"]
                && site["depth"] == call["trace_depth"]
                && site.get("labelled_end").unwrap_or(&serde_json::Value::Null)
                    == &call["labelled_end"]
        });
        assert!(found, "{call} is no unread site: {unread:?}");
    }
}

/// The blob's unread-site keys and possible calls are `blob.expected.json`,
/// the example the cloud's readers pin by hash.
#[test]
fn the_blob_keys_are_the_pinned_example() {
    let blob = scan();
    let got = serde_json::json!({
        "possible_calls": blob["possible_calls"],
        "unread_sites": blob["unread_sites"],
        "unread_sites_total": blob["unread_sites_total"],
        "unread_sites_by_reason": blob["unread_sites_by_reason"],
    });
    let want: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture_dir().join("blob.expected.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        got,
        want,
        "got:\n{}",
        serde_json::to_string_pretty(&got).unwrap()
    );
}

/// The `CARRICK_OUTPUT_JSON` output carries the three keys the blob carries
/// (carrick#3958), equal to the blob's for the same scan.
#[test]
fn the_json_output_carries_the_blobs_unread_site_keys() {
    let blob = scan();
    let out = scan_json();
    for key in [
        "unread_sites",
        "unread_sites_total",
        "unread_sites_by_reason",
    ] {
        assert!(!blob[key].is_null(), "the blob carries {key}");
        assert_eq!(out[key], blob[key], "{key} differs from the blob's");
    }
    // Present only when the cap cut rows; the JSON output carries it too.
    assert_eq!(
        out["unread_sites_not_listed"], blob["unread_sites_not_listed"],
        "unread_sites_not_listed differs from the blob's"
    );
}
