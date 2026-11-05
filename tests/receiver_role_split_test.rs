//! carrick#2441 row R3: what a typed receiver IS decides whether its call is
//! a route or a request.
//!
//! The deleted rule (the typed-receiver arm, carrick#695) split two sites
//! written alike, `x.get("/lit", arg)`, by the package the receiver's type
//! came from. Its replacement is the library store's role for the package
//! and the compiler's declaration of the receiver (the receiver trace, with
//! the fact layer on). Built: a receiver a maker call binds, and a receiver
//! handed in as a typed parameter. Open (carrick#2413 slice E): the
//! frozen import-by-specifier fallback in `request_summary` still reads an
//! import without the compiler's `decl_package`.
//!
//! Drives the real scanner binary on `.`, as the Action runs it, over a
//! committed copy of `tests/fixtures/receiver-role-split/repo`, offline,
//! with the fixture's store answer replayed and no model answer. The
//! answer key is `tests/fixtures/receiver-role-split/expected.json`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/receiver-role-split")
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

/// The rows one scan left in the service's mount graph, each as
/// `file:line METHOD path source`.
struct Rows {
    routes: BTreeSet<String>,
    requests: BTreeSet<String>,
}

/// One offline scan of a committed copy of the fixture, with the fact layer
/// and the receiver trace on. `installed` keeps the fixture's declaration
/// files in `node_modules`; without them no receiver's type resolves.
fn scan(installed: bool) -> Rows {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir().join("repo"), &repo);
    if !installed {
        std::fs::remove_dir_all(repo.join("node_modules")).unwrap();
    }
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let storage = tmp.path().join("storage");
    let home = tmp.path().join("home");
    let cassettes = tmp.path().join("cassettes");
    std::fs::create_dir_all(&storage).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(cassettes.join("library-claims")).unwrap();
    std::fs::copy(
        fixture_dir().join("library-claims.json"),
        cassettes.join("library-claims/default.json"),
    )
    .unwrap();

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
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_ALLOW_UNPREPARED", "1")
        .env("CARRICK_POSITION_ROWS", "1")
        .env("CARRICK_SITE_QUESTIONS", "1")
        .env("CARRICK_RECEIVER_TRACE", "1");
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
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let blobs: Vec<serde_json::Value> = std::fs::read_dir(&storage)
        .expect("the store reads")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| serde_json::from_slice(&std::fs::read(path).ok()?).ok())
        .collect();
    assert_eq!(blobs.len(), 1, "the root service uploads one blob");
    let graph = &blobs[0]["mount_graph"];
    let row = |row: &serde_json::Value, path: &str| {
        format!(
            "{} {} {} {}",
            row["file_location"].as_str().unwrap_or_default(),
            row["method"].as_str().unwrap_or_default(),
            row[path].as_str().unwrap_or_default(),
            row["resolution_source"].as_str().unwrap_or_default(),
        )
    };
    Rows {
        routes: graph["endpoints"]
            .as_array()
            .expect("endpoints")
            .iter()
            .map(|r| row(r, "full_path"))
            .collect(),
        requests: graph["data_calls"]
            .as_array()
            .expect("data_calls")
            .iter()
            .map(|r| row(r, "target_url"))
            .collect(),
    }
}

/// The answer key's rows, each given the source the scan labels it with.
fn key(what: &str, source: &str) -> BTreeSet<String> {
    let key: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture_dir().join("expected.json")).unwrap(),
    )
    .unwrap();
    key[what]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| format!("{} {source}", line.as_str().unwrap()))
        .collect()
}

/// Two sites written alike split on what their receivers are: the server
/// package's is a route, the client package's is a request, for a receiver a
/// maker binds and for one handed in as a typed parameter. Nothing about the
/// call's shape separates them.
#[test]
fn two_identically_shaped_sites_split_on_what_their_receivers_are() {
    let rows = scan(true);
    assert_eq!(rows.routes, key("routes", "library_claim"), "{rows:#?}");
    assert_eq!(
        rows.requests,
        key("requests", "request_summary"),
        "{rows:#?}"
    );
}

/// A checkout whose declarations do not resolve states neither role: no row
/// is claimed from a library the receiver was never read as.
#[test]
fn a_checkout_with_no_installed_declarations_states_neither_role() {
    let rows = scan(false);
    assert!(
        rows.routes
            .iter()
            .chain(rows.requests.iter())
            .all(|row| !row.ends_with("library_claim") && !row.ends_with("request_summary")),
        "no row claims a role it could not read: {rows:#?}"
    );
    assert!(rows.requests.is_empty(), "{rows:#?}");
}
