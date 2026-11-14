//! A call straight into a sibling's function keeps its call-through row
//! after the fact layer replaces the sibling's same-file wrapper rows
//! (carrick#4372).
//!
//! Scans `tests/fixtures/call-through-fact-helper/repo` (answer key in its
//! README) as a cloud scan does, offline. The sibling package `api` sends
//! each request through its own generic helper, so its calls of the helper
//! hold a same-file wrapper row, and the fact layer states the same request
//! there and replaces that row. The app's calls into `getThing` and
//! `listThings` pass values the fact layer does not read, so no fact stands
//! at them: the call-through walk states their rows from the sibling's
//! request rows. Those rows were lost when the fact's graph row read as a
//! call through rather than the request.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/call-through-fact-helper/repo")
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "carrick-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "carrick-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
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

/// Each service's blob from one offline scan of a committed copy.
fn blobs() -> &'static Vec<serde_json::Value> {
    static BLOBS: OnceLock<Vec<serde_json::Value>> = OnceLock::new();
    BLOBS.get_or_init(|| {
        let tmp = tempfile::tempdir().expect("temp dir");
        let repo = tmp.path().join("workspace");
        copy_dir(&fixture_dir(), &repo);
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["add", "-A", "-f"]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        let storage = tmp.path().join("storage");
        std::fs::create_dir_all(&storage).unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
        cmd.arg(".")
            .args(["--repo", "fixture/siblings"])
            .current_dir(&repo)
            .env("HOME", &home)
            .env("CARRICK_LOCAL_STORAGE_DIR", &storage)
            .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
            .env("CARRICK_CACHE_DIR", tmp.path().join("cache"))
            .env("CARRICK_MOCK_ALL", "1")
            .env(
                "CARRICK_MOCK_FIXTURE_DIR",
                format!("{}/", repo.join("__llm__").display()),
            )
            .env("CARRICK_SKIP_INTENTS", "1")
            .env("CARRICK_ALLOW_UNPREPARED", "1")
            .env("CARRICK_NO_UPDATE_CHECK", "1")
            .env(
                "CARRICK_SIDECAR_DIR",
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar"),
            );
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
            "CARRICK_NO_MODEL",
            "CARRICK_ALLOW_MISSING_TYPES",
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
        assert_eq!(blobs.len(), 2, "the scan stored one blob per service");
        blobs
    })
}

fn blob(service: &str) -> &'static serde_json::Value {
    blobs()
        .iter()
        .find(|blob| blob["service_name"] == service)
        .unwrap_or_else(|| panic!("no blob for {service}"))
}

/// `METHOD path source` for each of `service`'s call rows at `place`.
fn calls_at(service: &str, place: &str) -> Vec<String> {
    let mut rows: Vec<String> = blob(service)["calls"]
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

/// `METHOD path role reaches=… client=…` for each of `service`'s graph rows
/// at `place`.
fn graph_at(service: &str, place: &str) -> Vec<String> {
    let mut rows: Vec<String> = blob(service)["mount_graph"]["data_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["file_location"] == place)
        .map(|row| {
            format!(
                "{} {} {} reaches={} client={}",
                row["method"].as_str().unwrap_or_default(),
                row["canonical_path"].as_str().unwrap_or_default(),
                row["role"].as_str().unwrap_or("none"),
                row["reaches_request"].as_str().unwrap_or("none"),
                row["client"].as_str().unwrap_or("none"),
            )
        })
        .collect();
    rows.sort();
    rows
}

/// The sibling's calls of its own helper: the fact replaces the same-file
/// wrapper row, and its graph row stays the request the wrapper row was.
#[test]
fn the_siblings_helper_calls_are_facts_and_stay_the_request() {
    assert_eq!(
        calls_at("api", "packages/api/src/things.ts:13"),
        ["GET /things/:id request_summary"]
    );
    assert_eq!(
        calls_at("api", "packages/api/src/things.ts:17"),
        ["GET /things/kinds/:kind request_summary"]
    );
    assert_eq!(
        graph_at("api", "packages/api/src/things.ts:13"),
        ["GET /things/:id network_request reaches=none client=request"]
    );
    assert_eq!(
        graph_at("api", "packages/api/src/things.ts:17"),
        ["GET /things/kinds/:kind network_request reaches=none client=request"]
    );
}

/// The app's calls into the sibling keep the row the call-through walk
/// states from the sibling's request.
#[test]
fn the_apps_calls_into_the_sibling_keep_their_call_through_rows() {
    assert_eq!(
        calls_at("app", "services/app/src/things.ts:4"),
        ["GET /things/:id request_summary"]
    );
    assert_eq!(
        calls_at("app", "services/app/src/things.ts:8"),
        ["GET /things/kinds/:kind request_summary"]
    );
    assert_eq!(
        graph_at("app", "services/app/src/things.ts:4"),
        ["GET /things/:id wrapper_call reaches=packages/api/src/things.ts:13 client=getThing"]
    );
    assert_eq!(
        graph_at("app", "services/app/src/things.ts:8"),
        [
            "GET /things/kinds/:kind wrapper_call reaches=packages/api/src/things.ts:17 client=listThings"
        ]
    );
}
