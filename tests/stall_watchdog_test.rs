//! A scan stops itself when one discovery step makes no progress
//! (carrick#3766).
//!
//! Drives the real scanner binary (a debug build) on `.` over an invented
//! two-file service, offline. The debug-only hook `CARRICK_TEST_STALL_FILE`
//! holds the parse step of one file for five minutes, which is what a stuck
//! step looks like to the watch; `CARRICK_STALL_SECS` brings the limit down
//! from 600 s. The run must end through the signal path: the line naming the
//! file and step, exit 4, and well inside the 30 s after which the watch would
//! end the process directly.

#![cfg(unix)]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

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

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn a_step_that_makes_no_progress_stops_the_scan_with_exit_4_naming_the_file() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("workspace");
    write(
        &repo.join("package.json"),
        r#"{ "name": "stall-fixture", "version": "1.0.0" }"#,
    );
    write(
        &repo.join("src/load.ts"),
        "export async function load(id: string) {\n  return fetch(`/api/items/${id}`);\n}\n",
    );
    write(
        &repo.join("src/stuck.ts"),
        "export function render(n: number): string {\n  return String(n);\n}\n",
    );
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
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
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_TEST_STALL_FILE", "stuck.ts")
        .env("CARRICK_STALL_SECS", "3");
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
        "CARRICK_SLOW_FILE_SECS",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let started = Instant::now();
    let output = cmd.output().expect("spawn carrick");
    let took = started.elapsed();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        output.status.code(),
        Some(4),
        "exit 4 for a stalled scan:\n{stdout}{stderr}"
    );
    assert!(
        stderr
            .contains("Scan stopped: discovery made no progress on src/stuck.ts (step parse) for "),
        "the line names the file and the step:\n{stderr}"
    );
    assert!(
        !stderr.contains("Scan interrupted by"),
        "a stall is not reported as an outside signal:\n{stderr}"
    );
    // The signal path, not the 30 s fallback: about 3 s of stall plus the
    // run's start and its exit.
    assert!(
        took < Duration::from_secs(25),
        "ended through the signal path, took {took:?}:\n{stderr}"
    );
}
