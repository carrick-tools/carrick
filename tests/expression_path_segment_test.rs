//! A path segment the code writes as an expression keeps its true row
//! (carrick#4514).
//!
//! Scans `tests/fixtures/expression-path-segment/repo` (answer key in its
//! README) as a cloud scan does, offline. A segment that holds a choice
//! between literals states a placeholder named `param`, never the name of
//! one of its values; an env read after a `/` is one segment of the path,
//! never the base URL that drops the prefix.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/expression-path-segment/repo")
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

/// `METHOD path` for each of `service`'s call rows at `place`.
fn paths_at(service: &str, place: &str) -> Vec<String> {
    calls_at(service, place)
        .into_iter()
        .map(|row| {
            row.rsplit_once(' ')
                .map_or(row.clone(), |(r, _)| r.to_string())
        })
        .collect()
}

fn assert_rows(service: &str, wanted: &[(&str, &str)]) {
    for (place, path) in wanted {
        assert_eq!(
            paths_at(service, place),
            [format!("GET {path}")],
            "{service} {place}"
        );
    }
}

/// A choice between literals is a `param`, and an env read in a path is a
/// segment of it.
#[test]
fn an_expression_segment_states_the_path_the_code_writes() {
    assert_rows(
        "app",
        &[
            ("services/app/src/kinds.ts:4", "/things/kinds/:param"),
            ("services/app/src/kinds.ts:10", "/things/kinds/:param"),
            ("services/app/src/kinds.ts:18", "/things/:param"),
            ("services/app/src/more.ts:15", "/things/kinds/:KIND"),
        ],
    );
    assert_rows(
        "api",
        &[
            ("packages/api/src/direct.ts:5", "/direct/:param"),
            ("packages/api/src/direct.ts:11", "/direct/:param"),
            (
                "packages/api/src/direct.ts:15",
                "/direct/:API_VERSION/users",
            ),
            ("packages/api/src/direct.ts:31", "/concat/:param/x"),
        ],
    );
    for service in ["app", "api"] {
        for row in blob(service)["calls"].as_array().into_iter().flatten() {
            assert_ne!(row["key"]["path"], "/", "{service}: {row}");
        }
    }
}

/// Rows that were right stay as they were.
#[test]
fn rows_that_were_right_stay() {
    assert_rows(
        "app",
        &[
            ("services/app/src/kinds.ts:14", "/things/kinds/archived"),
            ("services/app/src/more.ts:7", "/things/kinds/:kind"),
            ("services/app/src/more.ts:11", "/things/kinds/:n"),
            ("services/app/src/more.ts:19", "/things/kinds/:k"),
            ("services/app/src/things.ts:4", "/things/:id"),
            ("services/app/src/things.ts:8", "/things/kinds/:kind"),
        ],
    );
    assert_rows(
        "api",
        &[
            ("packages/api/src/direct.ts:19", "/direct/:kind"),
            ("packages/api/src/things.ts:13", "/things/:id"),
            ("packages/api/src/things.ts:17", "/things/kinds/:kind"),
        ],
    );
    assert_eq!(
        paths_at("app", "services/app/src/things.ts:12"),
        ["POST /things/:id"]
    );
}
