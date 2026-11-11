//! A service's library-store ask and its compiler-engine process start run
//! beside other work, and the scan states what it stated before they did.
//!
//! Drives the real scanner binary on `.`, over a committed copy of the
//! two-service fixture `tests/fixtures/socket-per-connection/`, offline, with
//! the fixture's cassettes: a full scan, then a comment edit and the
//! incremental scan that reads the first one's blobs, then a second edit and
//! an incremental scan with no lane asking ahead. Both services ask the store
//! (each names a registry package) and both start a compiler engine of their
//! own.
//!
//! The answer key `tests/fixtures/store-ask-beside/expected.json` holds what
//! the scanner at `23535eea` stated on every scan: every row, lead, unread
//! site and verdict of each blob. Write it again only when a change is meant
//! to move what the scan states, with `CARRICK_WRITE_EXPECTED=1`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The fields of a blob this test holds byte-identical: the rows, the leads,
/// the unread sites and the verdicts.
const STATED: &[&str] = &[
    "endpoints",
    "calls",
    "mounts",
    "possible_calls",
    "possible_calls_omitted",
    "unread_sites",
    "unread_sites_total",
    "unread_sites_by_reason",
    "compat_verdicts",
    "pair_outcomes",
    "call_through",
];

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/socket-per-connection")
}

fn expected_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/store-ask-beside/expected.json")
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "carrick-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "carrick-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        // Fixed dates: the blobs name their commit, so its id must not move
        // from run to run.
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
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

/// What one scan left: each service's stated fields, and the run's log.
struct Scanned {
    stated: BTreeMap<String, serde_json::Value>,
    log: String,
}

/// The scans, in order and by name: the full one; the incremental one after
/// a comment edit, with the lane that asks ahead; and the incremental one
/// after a second comment edit with no lane, where each service sends its
/// own store ask before its implementations ask.
type Scans = Vec<(&'static str, Scanned)>;

fn scan_once(repo: &Path, storage: &Path, home: &Path, cache: &Path, lane: bool) -> Scanned {
    let logs = home.join(".carrick/logs/runs");
    let before: Vec<PathBuf> = std::fs::read_dir(&logs)
        .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
        .unwrap_or_default();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(repo)
        .env("HOME", home)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage)
        .env("CARRICK_CACHE_DIR", cache)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
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
        "CARRICK_OUTPUT_JSON",
        "CARRICK_NO_MODEL",
        "CARRICK_ALLOW_MISSING_TYPES",
        "CARRICK_LOCAL_STORAGE_ISOLATE",
        "CARRICK_ANSWERS_AHEAD",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    if !lane {
        cmd.env("CARRICK_ANSWERS_AHEAD", "0");
    }
    let output = cmd.output().expect("spawn carrick");
    assert!(
        output.status.success(),
        "the scan failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let repo_text = repo.to_string_lossy().into_owned();
    let mut stated = BTreeMap::new();
    for entry in std::fs::read_dir(storage).expect("storage dir") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let blob: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read blob")).expect("parse blob");
        let fields: serde_json::Map<String, serde_json::Value> = STATED
            .iter()
            .map(|field| (field.to_string(), blob[*field].clone()))
            .collect();
        // The copy's directory is a fresh temporary one on every run.
        let text = serde_json::to_string(&fields)
            .unwrap()
            .replace(&repo_text, "<repo>");
        stated.insert(
            blob["service_name"].as_str().unwrap().to_string(),
            serde_json::from_str(&text).unwrap(),
        );
    }
    assert_eq!(
        stated.keys().map(String::as_str).collect::<Vec<_>>(),
        ["cli", "srv"],
        "every service uploads one blob"
    );
    let log = std::fs::read_dir(&logs)
        .expect("the run wrote a log")
        .map(|entry| entry.unwrap().path())
        .filter(|path| !before.contains(path))
        .map(|path| std::fs::read_to_string(path).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    Scanned { stated, log }
}

fn scan() -> Scans {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir(), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let (storage, home, cache) = (
        tmp.path().join("storage"),
        tmp.path().join("home"),
        tmp.path().join("cache"),
    );
    for dir in [&storage, &home, &cache] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let edit = |what: &str| {
        let client = repo.join("services/cli/src/client.ts");
        let mut text = std::fs::read_to_string(&client).unwrap();
        text.push_str(&format!("// {what}: an edit that moves no line\n"));
        std::fs::write(&client, text).unwrap();
        run_git(&repo, &["add", "-A", "-f"]);
        run_git(&repo, &["commit", "-q", "-m", what]);
    };
    let full = scan_once(&repo, &storage, &home, &cache, true);
    edit("first");
    let incremental = scan_once(&repo, &storage, &home, &cache, true);
    edit("second");
    let alone = scan_once(&repo, &storage, &home, &cache, false);
    vec![
        ("full", full),
        ("incremental", incremental),
        ("incremental_no_lane", alone),
    ]
}

fn scanned() -> &'static Scans {
    static SCANS: OnceLock<Scans> = OnceLock::new();
    SCANS.get_or_init(scan)
}

/// `line` without the terminal colour codes the run log keeps in its spans.
fn plain(line: &str) -> String {
    let mut text = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            text.push(c);
        }
    }
    text
}

/// The lines of `log` that belong to `service`'s analysis.
fn service_lines(log: &str, service: &str) -> Vec<String> {
    let span = format!("service{{name={service}}}");
    log.lines()
        .map(plain)
        .filter(|line| line.contains(&span))
        .collect()
}

/// The first line from `from` on that holds `needle`.
fn position(lines: &[String], from: usize, needle: &str, service: &str, scan: &str) -> usize {
    lines
        .iter()
        .skip(from)
        .position(|line| line.contains(needle))
        .map(|at| at + from)
        .unwrap_or_else(|| panic!("{scan} scan, {service}: no line with {needle:?}"))
}

/// Every scan states what the scanner stated before the store ask and the
/// engine start moved: every row, lead, unread site and verdict, byte for
/// byte.
#[test]
fn every_scan_states_what_it_stated_before() {
    let stated: serde_json::Map<String, serde_json::Value> = scanned()
        .iter()
        .map(|(scan, scanned)| (scan.to_string(), serde_json::json!(scanned.stated)))
        .collect();
    let text = format!("{}\n", serde_json::to_string_pretty(&stated).unwrap());
    if std::env::var("CARRICK_WRITE_EXPECTED").as_deref() == Ok("1") {
        std::fs::create_dir_all(expected_path().parent().unwrap()).unwrap();
        std::fs::write(expected_path(), &text).unwrap();
    }
    let expected = std::fs::read_to_string(expected_path()).expect("the answer key");
    assert!(
        text == expected,
        "the scans state something else than the answer key; diff against {}",
        expected_path().display()
    );
}

/// Each service's compiler engine is started before its discovery and
/// waited for after it: discovery reads no type, so the process gets ready
/// beside it.
#[test]
fn each_service_s_engine_gets_ready_beside_its_discovery() {
    for (scan, scanned) in scanned() {
        for service in ["cli", "srv"] {
            let lines = service_lines(&scanned.log, service);
            let started = position(
                &lines,
                0,
                &format!("Starting a sidecar for service '{service}'"),
                service,
                scan,
            );
            // The service's walk started engines of its own before; the one
            // read here is the first ready after the analysis started its own.
            let discovered = position(&lines, started, "Discovered ", service, scan);
            let ready = position(&lines, started, "Sidecar ready", service, scan);
            assert!(
                started < discovered && discovered < ready,
                "{scan} scan, {service}: the engine was started at line {started}, discovery \
                 ended at {discovered} and the engine was ready at {ready}"
            );
        }
    }
}

/// Each service asks the store exactly once per scan, from its own analysis:
/// the ask that now leaves early is the only one.
#[test]
fn each_service_asks_the_store_once_per_scan() {
    for (scan, scanned) in scanned() {
        for service in ["cli", "srv"] {
            let asks = service_lines(&scanned.log, service)
                .iter()
                .filter(|line| line.contains("Mock fixture hit for /library-claims"))
                .count();
            assert_eq!(asks, 1, "{scan} scan, {service}: store asks");
        }
        let all = scanned
            .log
            .lines()
            .filter(|line| line.contains("Mock fixture hit for /library-claims"))
            .count();
        assert_eq!(all, 2, "{scan} scan: store asks in the whole run");
    }
}
