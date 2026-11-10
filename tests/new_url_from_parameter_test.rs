//! S8 slice (carrick#3679): a wrapper that builds its URL with
//! `new URL(path, base)` from its own parameter states its request at each
//! caller. The caller's path is the route; the base is not asserted.
//!
//! Drives the scanner as the Action does (`carrick .` from the repo root, one
//! root service) over a committed copy of `tests/fixtures/new-url-from-parameter/repo/`,
//! models mocked, with the store's label for the default library's `fetch`
//! from the cassette. Asserts on the uploaded blob.
//!
//! Needs the built compiler engine, as `sibling_sites_test.rs` does: skipped
//! where it is missing, and a failure in CI.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
        }
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .status()
        .expect("git failed to spawn");
    assert!(status.success(), "git {args:?} failed");
}

fn sidecar_built() -> bool {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    if !script.exists() {
        assert!(
            std::env::var("CI").is_err(),
            "the built sidecar is missing in CI"
        );
        eprintln!("Skipping: the built sidecar is missing");
        return false;
    }
    true
}

/// Each call row of the scan's one blob as `file:line METHOD path`.
fn rows() -> Option<&'static Vec<String>> {
    static ROWS: OnceLock<Option<Vec<String>>> = OnceLock::new();
    ROWS.get_or_init(|| {
        if !sidecar_built() {
            return None;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        copy_dir(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/new-url-from-parameter/repo"),
            &repo,
        );
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["add", "-A", "-f"]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        let store = tmp.path().join("store");
        fs::create_dir_all(&store).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
        cmd.arg(".")
            .args(["--repo", "fixture/new-url"])
            .current_dir(&repo)
            .env("HOME", home.path())
            .env("CARRICK_LOCAL_STORAGE_DIR", &store)
            .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
            .env("CARRICK_CACHE_DIR", cache.path())
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
            "GITHUB_SHA",
            "GITHUB_RUN_ID",
            "GITHUB_ACTIONS",
            "GITHUB_WORKSPACE",
            "CI",
            "CARRICK_NO_MODEL",
            "CARRICK_ALLOW_MISSING_TYPES",
            "CARRICK_OUTPUT_JSON",
            "CARRICK_SERVICE_CONCURRENCY",
            "RUST_LOG",
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
        let mut rows: Vec<String> = Vec::new();
        for entry in fs::read_dir(&store).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Ok(blob) =
                serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&path).unwrap())
            else {
                continue;
            };
            for call in blob["calls"].as_array().into_iter().flatten() {
                rows.push(format!(
                    "{} {} {}",
                    call["file_path"].as_str().unwrap_or_default(),
                    call["key"]["method"].as_str().unwrap_or_default(),
                    call["key"]["path"].as_str().unwrap_or_default(),
                ));
            }
        }
        rows.sort();
        Some(rows)
    })
    .as_ref()
}

/// The row at `file:line`, or `None` when the scan states none there.
fn row_at(rows: &[String], site: &str) -> Option<String> {
    let found: Vec<&String> = rows
        .iter()
        .filter(|row| row.starts_with(&format!("{site} ")))
        .collect();
    assert!(
        found.len() <= 1,
        "{site} states more than one row: {rows:#?}"
    );
    found.first().map(|row| row.to_string())
}

fn assert_row(site: &str, want: &str) {
    let Some(rows) = rows() else { return };
    assert_eq!(
        row_at(rows, site).as_deref(),
        Some(format!("{site} {want}").as_str()),
        "all rows: {rows:#?}"
    );
}

#[test]
fn a_url_built_with_new_url_from_a_parameter_is_read_at_the_caller() {
    // `fetch(new URL(path, base).toString())` and `fetch(new URL(path, base))`.
    assert_row("src/use-urls.ts:4", "POST /items.remove");
    assert_row("src/use-urls.ts:5", "POST /items.remove");
}

#[test]
fn a_builder_returning_new_url_from_its_parameters_is_read() {
    assert_row("src/use-urls.ts:6", "POST /items.drop");
}

#[test]
fn a_fetch_service_resolving_against_a_runtime_base_is_read_at_the_caller() {
    assert_row("src/use-fetch-service.ts:8", "POST /api/items.list");
}

#[test]
fn a_fetch_service_taken_from_an_own_container_is_read_at_the_caller() {
    assert_row("src/container.ts:14", "POST /api/items.list");
}

#[test]
fn an_absolute_url_through_new_url_is_that_url() {
    // `new URL(abs, base)` is `abs`: the scan never states the base's route
    // for it, and states no route of its own for an outside host.
    let Some(rows) = rows() else { return };
    let row = row_at(rows, "src/use-urls.ts:11");
    assert!(
        row.as_ref()
            .is_none_or(|row| row.contains("remote.example.com")),
        "{row:?}: {rows:#?}"
    );
}

#[test]
fn a_path_without_a_leading_slash_states_no_row() {
    let Some(rows) = rows() else { return };
    assert_eq!(row_at(rows, "src/use-urls.ts:17"), None, "{rows:#?}");
}
