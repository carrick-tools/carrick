//! A two-row caller states its own request (carrick#4532).
//!
//! Scans `tests/fixtures/two-request-callers/repo` once, offline (mock, with
//! the fixture's library-store answer and model cassette). A wrapper with two
//! requests is called from a summary function that states both: each request
//! line the callers state is withdrawn per request, whatever else sits at the
//! call, where its type anchors go to the caller's row. That row's verb must
//! be its call's alone; where another row at the call shares the verb the line
//! keeps the model's row and its anchors, as before. A callee line that states
//! its own route keeps its row.
//!
//! HTTP only: socket, pub/sub and RPC request lines are not read by this path
//! (a gap).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/two-request-callers")
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

struct Scan {
    blob: serde_json::Value,
    log: String,
}

/// The fixture scanned once: its blob and the scan's log.
fn scan() -> &'static Scan {
    static SCAN: OnceLock<Scan> = OnceLock::new();
    SCAN.get_or_init(|| {
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
        copy_dir(&fixture_dir().join("model-cassette"), &cassettes);
        let storage = tmp.path().join("storage");
        std::fs::create_dir_all(&storage).unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
        cmd.arg(".")
            .arg("--verbose")
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
            .env("CARRICK_SKIP_INTENTS", "1")
            .env("RUST_LOG", "carrick=debug");
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
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "scan exited non-zero:\n{log}");
        let mut blobs: Vec<serde_json::Value> = Vec::new();
        for entry in std::fs::read_dir(&storage).expect("the store reads") {
            let path = entry.expect("dir entry").path();
            if path.extension().is_some_and(|ext| ext == "json") {
                blobs.push(serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap());
            }
        }
        assert_eq!(blobs.len(), 1, "the scan stored one blob");
        Scan {
            blob: blobs.remove(0),
            log,
        }
    })
}

/// The call rows at `place` (`file:line`), as "METHOD path source", sorted.
fn calls_at(place: &str) -> Vec<String> {
    let mut rows: Vec<String> = scan().blob["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["file_path"] == place)
        .map(|row| {
            format!(
                "{} {} {}",
                row["key"]["method"].as_str().unwrap_or_default(),
                row["key"]["path"].as_str().unwrap_or_default(),
                row["resolution_source"].as_str().unwrap_or("none"),
            )
        })
        .collect();
    rows.sort();
    rows
}

/// The graph rows at `place`, as "METHOD path role client=… host=… base=…",
/// sorted.
fn graph_at(place: &str) -> Vec<String> {
    let mut rows: Vec<String> = scan().blob["mount_graph"]["data_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["file_location"] == place)
        .map(|row| {
            format!(
                "{} {} {} client={} host={} base={}",
                row["method"].as_str().unwrap_or_default(),
                row["canonical_path"].as_str().unwrap_or_default(),
                row["role"].as_str().unwrap_or("none"),
                row["client"].as_str().unwrap_or("none"),
                row["host"].as_str().unwrap_or("none"),
                row["base"]["kind"].as_str().unwrap_or("none"),
            )
        })
        .collect();
    rows.sort();
    rows
}

/// `line` without its terminal colour codes.
fn plain(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The scan log's fold line, without colour codes.
fn fold_line() -> String {
    scan()
        .log
        .lines()
        .map(plain)
        .find(|line| line.contains("site questions: fold"))
        .unwrap_or_else(|| panic!("no fold line in the scan log:\n{}", scan().log))
}

fn log_text() -> String {
    scan().log.lines().map(plain).collect::<Vec<_>>().join("\n")
}

#[test]
fn a_request_line_its_two_row_callers_state_is_withdrawn() {
    for place in [
        "src/net/mixed.ts:4",
        "src/net/heldmix.ts:4",
        "src/net/single.ts:4",
    ] {
        assert_eq!(calls_at(place), Vec::<String>::new(), "{place} calls");
        assert_eq!(graph_at(place), Vec::<String>::new(), "{place} graph");
    }
}

#[test]
fn a_request_line_whose_verb_its_callers_call_shares_keeps_its_row() {
    for place in ["src/net/paired.ts:4", "src/net/shared.ts:4"] {
        assert_eq!(
            calls_at(place),
            ["POST /v1/:endpoint model"],
            "{place} calls"
        );
    }
}

#[test]
fn a_callee_line_that_states_its_own_route_keeps_its_row() {
    assert_eq!(calls_at("src/net/mixed.ts:5"), ["POST /v1/settle model"]);
    assert_eq!(calls_at("src/net/shared.ts:5"), ["POST /v1/close model"]);
    assert_eq!(calls_at("src/net/heldmix.ts:5"), ["POST /v3/settle model"]);
}

#[test]
fn every_caller_row_stands() {
    assert_eq!(calls_at("src/jobs.ts:8"), ["POST /v1/one request_summary"]);
    assert_eq!(
        calls_at("src/jobs.ts:11"),
        [
            "GET /v1/look request_summary",
            "POST /v1/settle request_summary"
        ]
    );
    assert_eq!(
        calls_at("src/jobs.ts:14"),
        [
            "POST /v1/pair request_summary",
            "POST /v2/pair request_summary"
        ]
    );
    assert_eq!(
        calls_at("src/jobs.ts:17"),
        [
            "POST /v1/close request_summary",
            "POST /v1/open request_summary"
        ]
    );
    assert_eq!(
        calls_at("src/jobs.ts:20"),
        [
            "GET /v3/job request_summary",
            "POST /v3/settle request_summary"
        ]
    );
    assert_eq!(calls_at("src/boot.ts:3"), ["GET /v3/boot request_summary"]);
}

#[test]
fn a_line_is_withdrawn_only_where_its_anchors_go_to_a_row() {
    let log = log_text();
    assert!(
        log.contains("Model rows withdrawn, their callers state the request: 2"),
        "{log}"
    );
    assert!(
        log.contains("Of those, type anchors moved to their callers' rows: 2"),
        "{log}"
    );
    let unread = log
        .lines()
        .filter(|line| line.contains("was withdrawn and no caller's row took its type anchors"))
        .count();
    assert_eq!(unread, 0, "{log}");
    assert!(
        fold_line().contains("stated_at_fact_callers=1"),
        "{}",
        fold_line()
    );
}
