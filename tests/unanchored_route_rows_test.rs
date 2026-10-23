//! carrick#2382 through the whole scanner, mocked ($0, no model): a route a
//! service states by comparing the request path by hand keeps its row when
//! the model echoes the id of a call the handler makes.
//!
//! Drives the real scanner binary on `.`, as the Action runs it, over a
//! committed copy of `tests/fixtures/hand-dispatch-worker/`: one file with no
//! router, whose `fetch` handler compares `pathname` by equality, by
//! `startsWith` through a constant, and in a `switch`. Nothing lists those
//! comparisons as candidates, so the model has no correct id to echo for the
//! four routes. The answer set in `hand-dispatch-worker.answers/listed` has
//! each echo the id of a call the handler makes, which the listed-id arm keeps.
//!
//! Asserts on the uploaded blob.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hand-dispatch-worker")
}

fn answers_dir(set: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hand-dispatch-worker.answers")
        .join(set)
}

fn expected() -> serde_json::Value {
    let path = fixture_dir().join("expected.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("read the answer key"))
        .expect("the answer key is JSON")
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

/// What one scan left: the service's blob.
struct Scan {
    blob: serde_json::Value,
}

/// Scan a committed copy of the fixture on `.`, offline, replaying the
/// answer set `set`.
fn scan(set: &str) -> Scan {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir(), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let storage = tmp.path().join("storage");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&storage).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(".")
        .current_dir(&repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", &storage)
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", tmp.path().join("cache"))
        .env("HOME", &home)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", answers_dir(set).display()),
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
    let output = cmd.output().expect("failed to spawn carrick");
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "fixture scan exited non-zero:\n{printed}"
    );

    let blobs: Vec<serde_json::Value> = std::fs::read_dir(&storage)
        .expect("storage dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| serde_json::from_slice(&std::fs::read(path).ok()?).ok())
        .collect();
    assert_eq!(blobs.len(), 1, "the root service uploads one blob");
    Scan {
        blob: blobs.into_iter().next().unwrap(),
    }
}

fn listed() -> &'static Scan {
    static SCAN: OnceLock<Scan> = OnceLock::new();
    SCAN.get_or_init(|| scan("listed"))
}

/// `(file, line, method, path)` of a row.
type Row = (String, u64, String, String);

/// The answer key's routes.
fn expected_routes() -> BTreeSet<Row> {
    expected()["endpoints"]
        .as_array()
        .expect("an endpoints array")
        .iter()
        .map(|row| {
            (
                row["file"].as_str().unwrap().to_string(),
                row["line"].as_u64().unwrap(),
                row["method"].as_str().unwrap().to_string(),
                row["path"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Every endpoint row of the blob's mount graph, after checking that each
/// is the model's reading kept at its own line with no span.
fn model_routes(scan: &Scan) -> BTreeSet<Row> {
    scan.blob["mount_graph"]["endpoints"]
        .as_array()
        .expect("the blob holds a mount graph")
        .iter()
        .map(|row| {
            assert_eq!(row["resolution_source"], "model", "{row}");
            assert!(
                row["call_expression_span_start"].is_null()
                    && row["call_expression_span_end"].is_null(),
                "a route kept at its own line claims no span: {row}"
            );
            let location = row["file_location"].as_str().expect("file_location");
            let (file, line) = location.rsplit_once(':').expect("file:line");
            (
                file.to_string(),
                line.parse().expect("a line number"),
                row["method"].as_str().expect("method").to_string(),
                row["path"].as_str().expect("path").to_string(),
            )
        })
        .collect()
}

/// The model's rows: the four routes and the one `/audit` call, which both
/// answer sets state at its own listed site.
fn assert_model_only_rows(scan: &Scan) {
    let calls: Vec<(String, String, String)> = scan.blob["mount_graph"]["data_calls"]
        .as_array()
        .expect("the blob holds a mount graph")
        .iter()
        .map(|call| {
            (
                call["file_location"].as_str().unwrap().to_string(),
                call["method"].as_str().unwrap().to_string(),
                call["resolution_source"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        calls,
        vec![(
            "src/worker.ts:16".to_string(),
            "POST".to_string(),
            "model".to_string()
        )]
    );
    assert_eq!(scan.blob["boundary"]["model_only_rows"], 5);
}

/// The push that kept the four routes: each echoes a listed call the handler
/// makes, and the listed-id arm keeps it at its own line.
#[test]
fn routes_that_echo_a_listed_call_keep_their_rows() {
    let scan = listed();
    assert_eq!(model_routes(scan), expected_routes());
    assert_model_only_rows(scan);
}
