//! carrick#3074: a call into a local helper is not a request whose body is the
//! helper's first argument.
//!
//! `apps/web/src/track.ts` exports `trackClick(label: string, now = false)`.
//! It builds its own event and hands it to a sender `install.ts` configures,
//! which posts it to `POST /events` through `@fixture/client` (a detected data
//! fetcher not on the checkout). Two call sites in `buttons.ts` pass a string;
//! the cassette states `POST /events` at those helper calls and at the client
//! call in `install.ts`. The producer reads the body as `{ name, at }`.
//!
//! The pair must read compatible or unverifiable. When the client call lost its
//! candidate (carrick#3037, reverted), the helper-call rows were no longer set
//! aside as echoes of it, and their request type was the helper's argument:
//! "Type 'string' is not assignable" at both `buttons.ts` sites.
//!
//! Drives the real scanner binary on `.`, offline, and asserts on the stored
//! verdicts. See `tests/fixtures/helper-argument-not-body/README.md`.

use std::path::{Path, PathBuf};
use std::process::Command;

const BUTTONS: &str = "apps/web/src/buttons.ts";
const INSTALL: &str = "apps/web/src/install.ts";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/helper-argument-not-body")
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

/// Every service's blob from one offline scan of a committed copy.
fn blobs() -> Vec<serde_json::Value> {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir(), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let storage = tmp.path().join("storage");
    std::fs::create_dir_all(&storage).unwrap();
    let home = tmp.path().join("home");
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
            format!("{}/", repo.join("__llm__").display()),
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
    assert!(
        output.status.success(),
        "fixture scan exited non-zero:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    std::fs::read_dir(&storage)
        .expect("storage dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| serde_json::from_slice(&std::fs::read(path).ok()?).ok())
        .collect()
}

/// `(site, request verdict, request detail)` for every stored pair site that
/// states a request verdict.
fn request_verdicts(blobs: &[serde_json::Value]) -> Vec<(String, String, String)> {
    blobs
        .iter()
        .flat_map(|blob| {
            blob["compat_verdicts"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .flat_map(|pair| pair["sites"].as_array().cloned().unwrap_or_default())
        .filter_map(|at| {
            let verdict = at["request"]["verdict"].as_str()?.to_string();
            let site = at["consumer_location"].as_str()?.to_string();
            Some((site, verdict, at["request"].to_string()))
        })
        .collect()
}

#[test]
fn a_string_passed_to_a_helper_is_not_judged_as_the_request_body() {
    let blobs = blobs();
    let verdicts = request_verdicts(&blobs);
    eprintln!("request verdicts: {verdicts:#?}");

    // Not vacuous: the pair was judged at some site.
    assert!(
        !verdicts.is_empty(),
        "no request verdict was stored for POST /events"
    );
    for (site, verdict, detail) in &verdicts {
        assert!(
            site.starts_with(INSTALL) || site.starts_with(BUTTONS),
            "a verdict at an unexpected site {site}: {detail}"
        );
        assert_ne!(
            verdict, "incompatible",
            "{site} was judged incompatible, so the helper's argument was read as the body: {detail}"
        );
    }
}
