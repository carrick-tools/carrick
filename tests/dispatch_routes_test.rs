//! The dispatch rows in a scan (carrick#2474 R4b).
//!
//! Drives the real scanner binary on `.`, as the Action runs it, over a
//! committed copy of `tests/fixtures/dispatch-routes/repo` (one root
//! service), offline, with the fixture's store answer replayed as the
//! library store's, the handler questions answered from
//! `handler-answers.json` (one repeat of the live test's stored answers,
//! carrick#2474 comment 6358002149), and the fact layer on
//! (`CARRICK_POSITION_ROWS=1`, `CARRICK_SITE_QUESTIONS=1`).
//!
//! With `CARRICK_RECEIVER_TRACE=1` the blob holds the routes the rows key
//! (`rows.expected.json`) states, each at its comparison's line, and its
//! possible calls; the run log holds one `receiver trace: handler records`
//! summary with the records key's counts. Unset, the blob holds none of
//! them and the run log no summary.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dispatch-routes")
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "carrick-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "carrick-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        // One commit hash for every copy, so two scans' blobs compare.
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
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

/// One scan: its blobs (by file name, the run's own stamps removed), its
/// dispatch rows and possible calls, and the handler-records summary lines.
struct Scan {
    blobs: Vec<(String, String)>,
    /// Each endpoint row a site answer or the code's settled values state,
    /// as `file:line METHOD path source`.
    rows: BTreeSet<String>,
    /// Each HTTP producer possible call, as `file:line reason`.
    leads: BTreeSet<String>,
    /// The summary lines, each as `records=… asked=… forced=…`.
    records: Vec<String>,
}

/// One offline scan of a committed copy of the fixture with the fact layer
/// on, the trace switch set to `trace` or unset.
fn scan(trace: Option<&str>) -> Scan {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir().join("repo"), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let cassettes = tmp.path().join("cassettes");
    std::fs::create_dir_all(cassettes.join("library-claims")).unwrap();
    std::fs::create_dir_all(cassettes.join("site-questions")).unwrap();
    std::fs::copy(
        fixture_dir().join("library-claims.json"),
        cassettes.join("library-claims/default.json"),
    )
    .unwrap();
    std::fs::copy(
        fixture_dir().join("handler-answers.json"),
        cassettes.join("site-questions/handler.json"),
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
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_POSITION_ROWS", "1")
        .env("CARRICK_SITE_QUESTIONS", "1");
    for var in [
        "CARRICK_RECEIVER_TRACE",
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
    if let Some(trace) = trace {
        cmd.env("CARRICK_RECEIVER_TRACE", trace);
    }
    let output = cmd.output().expect("spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut blobs = Vec::new();
    let mut rows = BTreeSet::new();
    let mut leads = BTreeSet::new();
    for entry in std::fs::read_dir(&storage).expect("the store reads") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let mut blob: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for endpoint in blob["endpoints"].as_array().into_iter().flatten() {
            let source = endpoint["resolution_source"].as_str().unwrap_or_default();
            let by_site = endpoint["inference"]["by"] == "site_question";
            if endpoint["key"]["protocol"] != "http" || !(by_site || source == "library_claim") {
                continue;
            }
            rows.insert(format!(
                "{} {} {} {source}",
                endpoint["file_path"].as_str().unwrap(),
                endpoint["key"]["method"].as_str().unwrap(),
                endpoint["key"]["path"].as_str().unwrap()
            ));
        }
        for lead in blob["possible_calls"].as_array().into_iter().flatten() {
            if lead["kind"] == "http" && lead["role"] == "producer" {
                leads.insert(format!(
                    "{}:{} {}",
                    lead["file_path"].as_str().unwrap(),
                    lead["line_number"],
                    lead["unread"]["reason"].as_str().unwrap()
                ));
            }
        }
        if let Some(object) = blob.as_object_mut() {
            for stamp in [
                "scanner_version",
                "scanner_build",
                "last_updated",
                // Measured, so it differs run to run (carrick#3083).
                "memory_peaks",
            ] {
                object.remove(stamp);
            }
        }
        blobs.push((
            path.file_name().unwrap().to_string_lossy().into_owned(),
            serde_json::to_string(&blob).unwrap(),
        ));
    }
    blobs.sort();
    assert!(!blobs.is_empty(), "the scan stored a blob");
    let logs: Vec<String> = std::fs::read_dir(home.join(".carrick/logs/runs"))
        .expect("the scan keeps a run log")
        .filter_map(|entry| std::fs::read_to_string(entry.ok()?.path()).ok())
        .collect();
    let records: Vec<String> = logs
        .iter()
        .flat_map(|log| log.lines())
        .filter_map(|line| line.split_once("receiver trace: handler records "))
        .map(|(_, fields)| {
            let field = |name: &str| {
                fields
                    .split(' ')
                    .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
                    .unwrap_or_else(|| panic!("no {name} in {fields}"))
                    .to_string()
            };
            format!(
                "records={} asked={} forced={}",
                field("records"),
                field("asked"),
                field("forced")
            )
        })
        .collect();
    Scan {
        blobs,
        rows,
        leads,
        records,
    }
}

fn key(name: &str) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(fixture_dir().join(name)).unwrap()).unwrap()
}

/// The records key's counts, as the run log's summary line names them.
fn key_records() -> Vec<String> {
    let key = key("records.expected.json");
    let read = |what: &str| key["counts"][what].as_u64().unwrap_or_default();
    vec![format!(
        "records={} asked={} forced={}",
        read("records"),
        read("asked"),
        read("forced")
    )]
}

/// The rows key's rows and possible calls.
fn key_rows(what: &str) -> BTreeSet<String> {
    key("rows.expected.json")[what]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap().to_string())
        .collect()
}

/// carrick#2474 R4b: with the switch set, a scan writes the rows key's
/// routes, at their comparisons' lines, and its possible calls (beside the
/// scan's own, which the switch leaves as they are), the same in two scans;
/// no row carries a method the code did not choose. Unset, none.
#[test]
fn a_scan_with_the_receiver_trace_writes_each_dispatch_row() {
    let on = scan(Some("1"));
    let off = scan(None);
    assert_eq!(on.rows, key_rows("rows"));
    assert!(
        off.leads.is_subset(&on.leads),
        "the switch drops none of the scan's own possible calls: {:#?}",
        off.leads.difference(&on.leads).collect::<Vec<_>>()
    );
    assert_eq!(
        on.leads
            .difference(&off.leads)
            .cloned()
            .collect::<BTreeSet<_>>(),
        key_rows("possible_calls")
    );
    assert_eq!(on.records, key_records());
    let again = scan(Some("1"));
    assert_eq!(again.blobs, on.blobs, "two scans store the same blobs");
    assert!(off.records.is_empty(), "unset, no record is counted");
    assert!(
        off.rows.is_empty(),
        "unset, no dispatch row: {:#?}",
        off.rows
    );
}
