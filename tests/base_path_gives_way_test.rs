//! A fact replaces a base-path row stating its own request (carrick#4434).
//!
//! Scans `tests/fixtures/base-path-gives-way/repo` once, offline (mock, with
//! the fixture's library-store answer), and reads each call's rows and
//! graph rows from the blob. Where the fact layer states a request at a call
//! whose env or literal origin a base-path pass also reads, the pass's row
//! stating the same operation gives way to the fact, unless the call's line
//! also holds a row of another operation.
//!
//! Fixture README: `tests/fixtures/base-path-gives-way/README.md`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/base-path-gives-way")
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
fn a_fact_replaces_the_same_key_base_path_row_at_its_call() {
    assert_eq!(
        calls_at("src/one-env.ts:3"),
        ["POST ${process.env.API_URL}/e/one request_summary"]
    );
    assert_eq!(
        graph_at("src/one-env.ts:3"),
        ["POST ${process.env.API_URL}/e/one none client=post host=none base=env"]
    );
    assert_eq!(
        calls_at("src/one-literal.ts:3"),
        ["POST https://api.example.com/l/one request_summary"]
    );
    assert_eq!(
        graph_at("src/one-literal.ts:3"),
        ["POST https://api.example.com/l/one none client=post host=api.example.com base=none"]
    );
    // The mixed line's `/m/a` call: the fact's row stands. Its `/m/b` call is
    // not asserted here: it is carrick#4433's.
    assert!(
        calls_at("src/mixed.ts:3")
            .contains(&"POST https://api.example.com/m/a request_summary".to_string()),
        "{:?}",
        calls_at("src/mixed.ts:3")
    );
    let graph = graph_at("src/mixed.ts:3");
    assert_eq!(
        graph
            .iter()
            .filter(|row| row.contains("/m/a"))
            .collect::<Vec<_>>(),
        ["POST https://api.example.com/m/a none client=post host=api.example.com base=none"]
    );
}

#[test]
fn a_line_with_two_calls_keeps_its_base_path_rows() {
    assert_eq!(
        calls_at("src/two-literal.ts:3"),
        [
            "POST https://api.example.com/t/a literal_base_path",
            "POST https://api.example.com/t/b literal_base_path"
        ]
    );
    assert_eq!(
        calls_at("src/two-env.ts:3"),
        [
            "POST ${process.env.API_URL}/te/a env_base_path",
            "POST ${process.env.API_URL}/te/b env_base_path"
        ]
    );
}

#[test]
fn a_path_base_has_no_base_path_row() {
    assert_eq!(
        calls_at("src/prefix-control.ts:3"),
        ["POST /api/p/one request_summary"]
    );
}

#[test]
fn the_fold_line_counts_what_is_kept_out() {
    let line = fold_line();
    assert!(
        line.contains(
            r#"kept_out={"env_base_path": 2, "literal_base_path": 2, "request_summary": 1}"#
        ),
        "{line}"
    );
}
