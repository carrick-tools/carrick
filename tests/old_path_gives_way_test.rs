//! A fact replaces the old-path rows of its own request (carrick#4372).
//!
//! Scans `tests/fixtures/old-path-gives-way/repo` once, offline (mock, with
//! the fixture's library-store answer), and reads each call's rows, graph
//! rows and request entries from the blob. Where the fact layer states a
//! request at a call, a same-file wrapper's row there stating the same
//! operation gives way to the fact, unless the call's line also holds a
//! row of another operation: the fold cannot tell two calls on one line
//! apart, so that line keeps today's rows. A row keyed as another site
//! row on the line is that row's call (carrick#4433), so two calls that
//! each state a site row each replace their own.
//!
//! Fixture README: `tests/fixtures/old-path-gives-way/README.md`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/old-path-gives-way")
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

/// The consumer request entries at `file`:`line` for `path`, as
/// "state definition".
fn requests_at(file: &str, line: u64, path: &str) -> Vec<String> {
    scan().blob["type_manifest"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry["file_path"] == file
                && entry["line_number"] == line
                && entry["role"] == "consumer"
                && entry["type_kind"] == "request"
                && entry["path"] == path
        })
        .map(|entry| {
            format!(
                "{} {}",
                entry["type_state"].as_str().unwrap_or_default(),
                entry["expanded_definition"].as_str().unwrap_or("none"),
            )
        })
        .collect()
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

#[test]
fn a_fact_replaces_the_same_key_old_path_row_at_its_call() {
    assert_eq!(
        calls_at("src/two-lines.ts:3"),
        ["POST /z/a request_summary"]
    );
    assert_eq!(
        calls_at("src/two-lines.ts:4"),
        ["POST /z/b request_summary"]
    );
    assert_eq!(
        graph_at("src/two-lines.ts:3"),
        ["POST /z/a network_request client=send host=none base=relative"]
    );
    assert_eq!(
        graph_at("src/two-lines.ts:4"),
        ["POST /z/b network_request client=send host=none base=relative"]
    );
    assert_eq!(requests_at("src/two-lines.ts", 3, "/z/a"), ["implicit 1"]);
    assert_eq!(requests_at("src/two-lines.ts", 4, "/z/b"), ["implicit 2"]);
    // Two identical calls on one line: one row, the fact's.
    assert_eq!(
        calls_at("src/same-twice.ts:3"),
        ["POST /w/a request_summary"]
    );
    assert_eq!(
        graph_at("src/same-twice.ts:3"),
        ["POST /w/a network_request client=send host=none base=relative"]
    );
    assert_eq!(requests_at("src/same-twice.ts", 3, "/w/a"), ["implicit 1"]);
    // Kept out by a wrapper row, under the mixed guard: mixed-plain (1,
    // whose second call states no site row) and mixed.ts (1, whose line
    // also holds the summary row of another request). Kept out by the
    // first call's fact: same-twice's second call. A base-path row of the
    // fact's own operation gives way (carrick#4434): base-mixed's `/m/a`
    // is replaced, so no `literal_base_path` count. Two
    // calls on one line that each state a site row keep none out
    // (carrick#4433).
    let line = fold_line();
    assert!(
        line.contains(r#"kept_out={"request_summary": 1, "same_file_wrapper": 2}"#),
        "{line}"
    );
}

/// carrick#4433: two calls on one line that each state their own request
/// each replace the old-path row of their own key.
#[test]
fn both_calls_on_one_line_state_their_own_rows() {
    assert_eq!(
        calls_at("src/two-same.ts:3"),
        ["POST /x/a request_summary", "POST /x/b request_summary"]
    );
    assert_eq!(
        graph_at("src/two-same.ts:3"),
        [
            "POST /x/a network_request client=send host=none base=relative",
            "POST /x/b network_request client=send host=none base=relative"
        ]
    );
    assert_eq!(requests_at("src/two-same.ts", 3, "/x/a"), ["implicit 1"]);
    assert_eq!(requests_at("src/two-same.ts", 3, "/x/b"), ["implicit 2"]);
}

/// carrick#4433: each fact's graph row reads where its call goes from the
/// removed row of its own key, never from the other call on the line.
#[test]
fn two_wrappers_on_one_line_keep_their_own_client_host_and_base() {
    assert_eq!(
        calls_at("src/two-wrappers.ts:4"),
        [
            "POST ${process.env.B_BASE}/y/b request_summary",
            "POST https://a.example.test/y/a request_summary"
        ]
    );
    assert_eq!(
        graph_at("src/two-wrappers.ts:4"),
        [
            "POST ${process.env.B_BASE}/y/b network_request client=sendB host=none base=env",
            "POST https://a.example.test/y/a network_request client=sendA host=a.example.test base=none"
        ]
    );
}

/// carrick#4463: a call whose neighbour on the line states a base-path row
/// of another operation states its own row, and reads no graph values off
/// the neighbour's call: it names no host.
#[test]
fn a_plain_call_beside_a_base_path_call_states_its_own_row() {
    assert_eq!(
        calls_at("src/base-mixed.ts:3"),
        [
            "POST /m/b request_summary",
            "POST https://api.example.com/m/a request_summary"
        ]
    );
    assert_eq!(
        graph_at("src/base-mixed.ts:3"),
        [
            "POST /m/b none client= host=none base=none",
            "POST https://api.example.com/m/a none client=post host=api.example.com base=none"
        ]
    );
    // R6: `/m/b` names no host, whichever of #4434 and #4463 lands second.
    assert!(
        graph_at("src/base-mixed.ts:3")
            .iter()
            .any(|row| row.starts_with("POST /m/b ") && row.contains("host=none")),
        "{:?}",
        graph_at("src/base-mixed.ts:3")
    );
}

#[test]
fn a_call_with_a_row_of_another_request_keeps_today() {
    assert_eq!(
        calls_at("src/mixed-plain.ts:5"),
        ["POST /q/a same_file_wrapper", "POST /q/b same_file_wrapper"]
    );
    assert_eq!(
        calls_at("src/mixed.ts:4"),
        ["GET /v/b request_summary", "POST /v/a same_file_wrapper"]
    );
}

#[test]
fn an_unlabelled_wrapper_keeps_its_old_path_row() {
    assert_eq!(
        calls_at("src/unlabelled.ts:3"),
        ["POST /u/a same_file_wrapper"]
    );
    assert_eq!(
        graph_at("src/unlabelled.ts:3"),
        ["POST /u/a network_request client=send host=none base=relative"]
    );
}
