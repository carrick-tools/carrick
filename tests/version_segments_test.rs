//! A path segment, topic or event a client reads from a destructured options
//! key with a default (`const { version = 'v1' } = options || {}`) is the
//! value each caller's object writes there, else the default, also through
//! a spread of the caller's own options (carrick#3608 T1). The `method` key
//! moves the same way: the client's default is the verb a caller omits.
//!
//! Drives the real scanner binary on `.`, as the Action runs it, over a
//! committed copy of `tests/fixtures/version-segments/repo` (one root
//! service), offline, with the fixture's store answer replayed as the
//! library store's. The key is `rows.expected.json`: each `rows` entry is a
//! call line and the one row the scan writes there, not found by the model
//! (`METHOD path`, or the event name). A `kind_asked` line is a broker
//! call: its label leaves the kind to the model, so it stays a possible
//! call whatever its name.
//!
//! Protocols: HTTP calls, pub/sub topics, socket events. GraphQL names come
//! from documents, not from these keys: not covered.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/version-segments")
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

/// What one scan wrote.
struct Scan {
    /// The rows not found by the model, by `file:line`: `METHOD path`, or
    /// the topic or event name.
    rows: BTreeMap<String, BTreeSet<String>>,
    /// The `file:line` of each possible call.
    leads: BTreeSet<String>,
}

fn scan() -> &'static Scan {
    static SCAN: OnceLock<Scan> = OnceLock::new();
    SCAN.get_or_init(run_scan)
}

fn run_scan() -> Scan {
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
        "scan exited non-zero:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut rows: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut leads = BTreeSet::new();
    let mut blobs = 0;
    for entry in std::fs::read_dir(&storage).expect("the store reads") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        blobs += 1;
        let blob: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for row in blob["calls"].as_array().into_iter().flatten() {
            if row["resolution_source"] == "model" {
                continue;
            }
            let key = &row["key"];
            let name = ["path", "topic", "event", "field"]
                .iter()
                .find_map(|field| key[field].as_str())
                .unwrap_or_default();
            let row_text = match key["method"].as_str() {
                Some(method) => format!("{method} {name}"),
                None => name.to_string(),
            };
            let at = row["file_path"].as_str().unwrap_or_default();
            rows.entry(at.to_string()).or_default().insert(row_text);
        }
        for lead in blob["possible_calls"].as_array().into_iter().flatten() {
            let file = lead["file_path"].as_str().unwrap_or_default();
            let line = lead["line_number"].as_u64().unwrap_or_default();
            leads.insert(format!("{file}:{line}"));
        }
    }
    assert_eq!(blobs, 1, "the scan stored one blob");
    Scan { rows, leads }
}

fn key() -> serde_json::Value {
    serde_json::from_str(
        &std::fs::read_to_string(fixture_dir().join("rows.expected.json")).unwrap(),
    )
    .unwrap()
}

/// Every answer-key line in a file of `files` holds exactly its row, or for
/// a broker line a possible call.
fn check(files: &[&str]) {
    let scan = scan();
    let key = key();
    let in_scope = |at: &str| {
        files
            .iter()
            .any(|file| at.rsplit_once(':').is_some_and(|(path, _)| path == *file))
    };
    let mut wrong = Vec::new();
    for (at, want) in key["rows"].as_object().unwrap() {
        if !in_scope(at) {
            continue;
        }
        let want = want.as_str().unwrap();
        let got = scan.rows.get(at).cloned().unwrap_or_default();
        if got != BTreeSet::from([want.to_string()]) {
            wrong.push(format!("{at}: want {want}, got {got:?}"));
        }
    }
    for at in key["kind_asked"].as_array().unwrap() {
        let at = at.as_str().unwrap();
        if in_scope(at) && !scan.leads.contains(at) {
            wrong.push(format!("{at}: want a possible call, got none"));
        }
    }
    assert!(
        wrong.is_empty(),
        "{}\n\nrows: {:#?}\nleads: {:#?}",
        wrong.join("\n"),
        scan.rows,
        scan.leads
    );
}

/// A caller that omits the key, or writes an object without it, sends the
/// client's default; one that writes it sends its own value. On HTTP (a
/// body destructure, a destructured parameter, after a base the code does
/// not state), on a socket event, and on a topic, whose kind stays asked.
#[test]
fn a_defaulted_key_states_the_version_each_caller_sends() {
    check(&[
        "src/use-param-key.ts",
        "src/use-based.ts",
        "src/use-event.ts",
        "src/use-topic.ts",
    ]);
    // The wrappers that write their object whole (`{ method: 'GET' }`, and
    // `{ version: 'v2', method: 'GET' }`).
    let scan = scan();
    for (at, want) in [
        ("src/use-options.ts:2", "GET /v1/a"),
        ("src/use-options.ts:3", "GET /v2/b"),
    ] {
        assert_eq!(
            scan.rows.get(at).cloned().unwrap_or_default(),
            BTreeSet::from([want.to_string()]),
            "{at}: {:#?}",
            scan.rows
        );
    }
}

/// A wrapper that spreads its caller's options after its own keys: the
/// caller's key wins, else the key the wrapper writes before the spread,
/// else the client's default.
#[test]
fn a_spread_of_the_callers_options_keeps_the_key_written_before_it() {
    check(&["src/use-options.ts", "src/use-override.ts"]);
}

/// A caller that writes a value the code does not state keeps the segment
/// open; the literal callers of the same client state theirs. A key that is
/// a whole path segment (`/${version}/items`) keeps the callee's
/// placeholder whatever its callers write: the summary's whole-segment rule
/// (carrick#3608 T2 decides when the callers' text is the segment).
#[test]
fn a_runtime_version_stays_a_placeholder() {
    check(&["src/use-runtime.ts", "src/use-whole.ts"]);
}

/// A library that reads the verb from the `method` key sends the client's
/// default where the caller omits it, not the library's own default.
#[test]
fn a_defaulted_method_key_is_the_method_a_caller_omits() {
    check(&["src/use-method.ts"]);
}
