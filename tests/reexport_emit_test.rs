//! A call on a class a labelled package re-exports from its dependency
//! (carrick#4878): the call's declaration is in the dependency, so the
//! verifier is asked about the labelled export's member too, and the call
//! ends at the export as a lead plus an unread site, as a call on a class the
//! package declares itself does. The answer key is
//! `tests/fixtures/reexport-emit/README.md`.
//!
//! Drives the real scanner binary on `.`, as the Action runs it, offline,
//! with each arm's store answer replayed as the library store's. Needs the
//! built compiler engine (the verifier).
//!
//! Protocols: pub/sub (the in-process bus role). The ask reads every labelled
//! op and scope claim whatever its role, so queues and sockets ask the same
//! way; HTTP routes are left as they were.

use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURE: &str = "reexport-emit";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(FIXTURE)
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

fn scan(cassette: &str) -> Vec<(String, serde_json::Value)> {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir().join("repo"), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let cassettes = tmp.path().join("cassettes");
    std::fs::create_dir_all(cassettes.join("library-claims")).unwrap();
    std::fs::copy(
        fixture_dir().join(cassette),
        cassettes.join("library-claims/default.json"),
    )
    .unwrap();
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
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.display()),
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
    let output = cmd.output().expect("spawn carrick");
    assert!(
        output.status.success(),
        "scan failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut blobs = Vec::new();
    for entry in std::fs::read_dir(&storage).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "json") {
            let blob: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            blobs.push((
                path.file_name().unwrap().to_string_lossy().to_string(),
                blob,
            ));
        }
    }
    blobs
}

/// The lead and the unread site a service's blob holds at its `orders.ts:7`
/// call, as `(package, export, member)` of each one's labelled end.
fn ends_at_call(
    blobs: &[(String, serde_json::Value)],
    service: &str,
) -> (Vec<String>, Vec<String>) {
    let blob = blobs
        .iter()
        .map(|(_, blob)| blob)
        .find(|blob| blob["service_name"] == service)
        .unwrap_or_else(|| panic!("no blob for {service}"));
    let end = |row: &serde_json::Value| {
        let end = &row["labelled_end"];
        format!("{}/{}/{}", end["package"], end["export"], end["member"])
    };
    let file = format!("services/{service}/src/orders.ts");
    let leads = blob["possible_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|lead| lead["file_path"] == file.as_str() && lead["line_number"] == 7)
        .map(end)
        .collect();
    let unread = blob["unread_sites"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|site| {
            site["file"] == file.as_str()
                && site["line"] == 7
                && site["reason"] == "unanswered"
                && site["kind"] == "pubsub"
        })
        .map(end)
        .collect();
    (leads, unread)
}

fn assert_lead(cassette: &str, service: &str, export: &str) {
    let blobs = scan(cassette);
    let (leads, unread) = ends_at_call(&blobs, service);
    let want = format!("\"hub-kit\"/\"{export}\"/\"fire\"");
    assert_eq!(leads, vec![want.clone()], "{service} lead in {cassette}");
    assert_eq!(unread, vec![want], "{service} unread site in {cassette}");
}

/// The control: the class is declared in the labelled package.
#[test]
fn a_class_the_package_declares_ends_at_its_export() {
    for cassette in [
        "library-claims-kit.json",
        "library-claims-kit-listed.json",
        "library-claims-kit-instance.json",
    ] {
        assert_lead(cassette, "control", "Local");
    }
}

/// The class is re-exported from the dependency and the service imports the
/// labelled package by value elsewhere: the package is asked and answered, the
/// call lands in the dependency. Fails on main for `kit` and `kit-instance`.
#[test]
fn a_re_exported_class_ends_at_the_labelled_export() {
    for cassette in [
        "library-claims-kit.json",
        "library-claims-kit-instance.json",
    ] {
        assert_lead(cassette, "asked", "Hub");
    }
}

/// The store lists the dependency under the labelled package
/// (`type_packages`): the call already ends at the export on main.
#[test]
fn a_re_exported_class_listed_by_the_store_ends_at_the_labelled_export() {
    assert_lead("library-claims-kit-listed.json", "asked", "Hub");
}

/// The labelled package names no dependency, so it cannot re-export the class
/// the call landed in: the call is not asked on it and ends nowhere labelled.
#[test]
fn a_package_that_does_not_depend_on_the_landing_package_is_not_asked() {
    for cassette in [
        "library-claims-kit.json",
        "library-claims-kit-instance.json",
    ] {
        let blobs = scan(cassette);
        let (leads, _) = ends_at_call(&blobs, "nodep");
        assert!(leads.is_empty(), "nodep lead in {cassette}: {leads:?}");
        let (leads, _) = ends_at_call(&blobs, "asked");
        assert_eq!(leads, vec!["\"hub-kit\"/\"Hub\"/\"fire\""]);
    }
}

/// A const with a call signature the labelled package re-exports from its
/// dependency: the call on what it returns lands in the dependency, so its
/// `handler` op and the export's `call` maker are asked, and the call with
/// its literal topic is a row read through both labels. It holds with no
/// dependency read too: a guard for the producer rows that read this shape.
#[test]
fn a_member_called_on_a_re_exported_callable_const_is_a_labelled_row() {
    let blobs = scan("library-claims-kit-callable.json");
    let blob = blobs
        .iter()
        .map(|(_, blob)| blob)
        .find(|blob| blob["service_name"] == "callable")
        .expect("callable blob");
    let rows: Vec<&serde_json::Value> = blob["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["file_path"] == "services/callable/src/orders.ts:7")
        .collect();
    assert_eq!(rows.len(), 1, "one row at the call: {rows:?}");
    assert_eq!(rows[0]["key"]["topic"], "orders.shipped");
    assert_eq!(
        rows[0]["library_semantics"],
        serde_json::json!([
            "hub-kit@1:defineFn:make:call:()",
            "hub-kit@1:defineFn:op:send:handler:of:instance:()"
        ])
    );
}

/// The dependency is the labelled package: a service that declares it ends
/// there.
#[test]
fn a_service_that_declares_the_dependency_ends_at_its_export() {
    let blobs = scan("library-claims-core.json");
    let (leads, _) = ends_at_call(&blobs, "declared");
    assert_eq!(leads, vec!["\"hub-core\"/\"Hub\"/\"fire\""]);
}

/// A call the answer holds no label for keeps its reason: `forRoot` is a
/// member of a labelled package that no claim names.
#[test]
fn an_unlabelled_member_keeps_its_own_reason() {
    let blobs = scan("library-claims-kit-instance.json");
    let blob = blobs
        .iter()
        .map(|(_, blob)| blob)
        .find(|blob| blob["service_name"] == "asked")
        .expect("asked blob");
    assert!(
        blob["unread_sites"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|site| {
                site["file"] == "services/asked/src/module.ts"
                    && site["line"] == 4
                    && site["reason"] == "package_unmatched"
            }),
        "forRoot stays package_unmatched"
    );
}
