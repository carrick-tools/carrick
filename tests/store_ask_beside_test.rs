//! A service's library-store ask and its compiler-engine process start run
//! beside other work, and the scan states what it stated before they did.
//!
//! Drives the real scanner binary on `.`, over a committed copy of the
//! two-service fixture `tests/fixtures/socket-per-connection/`, offline, with
//! the fixture's cassettes: a full scan, then a comment edit and the
//! incremental scan that reads the first one's blobs, with one service
//! analysed at a time so the lane asks ahead for the second (`Lane::Ahead`),
//! then a second edit and an incremental scan with no lane asking ahead. A
//! fourth test scans its own copy, where the lane's store ask panics. Both services ask the store
//! (each names a registry package) and both start a compiler engine of their
//! own.
//!
//! The answer key `tests/fixtures/store-ask-beside/expected.json` holds what
//! the scanner at `23535eea` stated on every scan: every row, lead, unread
//! site and verdict of each blob, with each verdict's `scanner_version`
//! masked (it is the release's, not the code's). Write it again only when a change is meant
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

/// What one scan left: each service's stated fields, the run's log and what
/// the binary wrote to its standard error.
struct Scanned {
    stated: BTreeMap<String, serde_json::Value>,
    log: String,
    stderr: String,
}

/// The scans, in order and by name: the full one; the incremental one after
/// a comment edit, with the lane that asks ahead; and the incremental one
/// after a second comment edit with no lane, where each service sends its
/// own store ask before its implementations ask.
type Scans = Vec<(&'static str, Scanned)>;

/// How a scan asks ahead.
#[derive(Clone, Copy)]
enum Lane {
    /// No lane: each service sends its own store ask.
    Off,
    /// The lane on, every service admitted at once, as the machine allows.
    /// Both services of this fixture then start together, so the lane is
    /// never ahead of either and asks nothing.
    Admitted,
    /// The lane on, one service analysed at a time: the second service waits
    /// its turn, and the lane asks ahead for it while the first is analysed.
    Ahead,
}

fn scan_once(repo: &Path, storage: &Path, home: &Path, cache: &Path, lane: Lane) -> Scanned {
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
        "CARRICK_SERVICE_CONCURRENCY",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    match lane {
        Lane::Off => {
            cmd.env("CARRICK_ANSWERS_AHEAD", "0");
        }
        Lane::Admitted => {}
        Lane::Ahead => {
            cmd.env("CARRICK_SERVICE_CONCURRENCY", "1");
        }
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
        let mut fields: serde_json::Value = serde_json::from_str(&text).unwrap();
        mask_scanner_version(&mut fields);
        stated.insert(blob["service_name"].as_str().unwrap().to_string(), fields);
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
    Scanned {
        stated,
        log,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// A committed copy of the fixture, and the directories its scans write to.
struct Tree {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    storage: PathBuf,
    home: PathBuf,
    cache: PathBuf,
}

impl Tree {
    fn new() -> Self {
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
        Self {
            _tmp: tmp,
            repo,
            storage,
            home,
            cache,
        }
    }

    /// A comment at the end of cli's one file, committed alone: a fixture
    /// file a test adds to the cassettes stays out of the commit, so every
    /// commit id is the answer key's.
    fn edit(&self, what: &str) {
        let client = self.repo.join("services/cli/src/client.ts");
        let mut text = std::fs::read_to_string(&client).unwrap();
        text.push_str(&format!("// {what}: an edit that moves no line\n"));
        std::fs::write(&client, text).unwrap();
        run_git(&self.repo, &["add", "-f", "services/cli/src/client.ts"]);
        run_git(&self.repo, &["commit", "-q", "-m", what]);
    }

    fn scan(&self, lane: Lane) -> Scanned {
        scan_once(&self.repo, &self.storage, &self.home, &self.cache, lane)
    }
}

fn scan() -> Scans {
    let tree = Tree::new();
    let full = tree.scan(Lane::Admitted);
    tree.edit("first");
    let incremental = tree.scan(Lane::Ahead);
    tree.edit("second");
    let alone = tree.scan(Lane::Off);
    vec![
        ("full", full),
        ("incremental", incremental),
        ("incremental_no_lane", alone),
    ]
}

/// Every `scanner_version` in `value`, masked: a verdict names the release
/// that judged it, which moves with every release on main and states nothing
/// about the scanned code.
fn mask_scanner_version(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, field) in map.iter_mut() {
                if key == "scanner_version" && field.is_string() {
                    *field = serde_json::Value::String("<scanner version>".to_string());
                } else {
                    mask_scanner_version(field);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(mask_scanner_version),
        _ => {}
    }
}

/// The answer key, as JSON.
fn expected() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(expected_path()).expect("the answer key"))
        .expect("the answer key parses")
}

/// The first field where `stated` and `expected` differ, named by scan,
/// service and field, with both values cut short: what a failure prints.
fn first_difference(stated: &serde_json::Value, expected: &serde_json::Value) -> Option<String> {
    let cut = |value: &serde_json::Value| {
        let text = value.to_string();
        text.chars().take(1500).collect::<String>()
    };
    let scans: std::collections::BTreeSet<&String> = stated
        .as_object()?
        .keys()
        .chain(expected.as_object()?.keys())
        .collect();
    for scan in scans {
        for service in ["cli", "srv"] {
            for field in STATED {
                let (got, want) = (
                    &stated[scan.as_str()][service][field],
                    &expected[scan.as_str()][service][field],
                );
                if got != want {
                    return Some(format!(
                        "{scan} scan, {service}, {field}:\n  stated:   {}\n  expected: {}",
                        cut(got),
                        cut(want)
                    ));
                }
            }
        }
    }
    None
}

/// How many times `service`'s own analysis asked the store in `log`.
fn own_store_asks(log: &str, service: &str) -> usize {
    service_lines(log, service)
        .iter()
        .filter(|line| line.contains("Mock fixture hit for /library-claims"))
        .count()
}

/// How many lines of `log` hold `needle`, colour codes stripped.
fn lines_with(log: &str, needle: &str) -> usize {
    log.lines()
        .map(plain)
        .filter(|line| line.contains(needle))
        .count()
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
    let expected_text = std::fs::read_to_string(expected_path()).expect("the answer key");
    assert!(
        text == expected_text,
        "the scans state something else than the answer key {}; first difference: {}",
        expected_path().display(),
        first_difference(&serde_json::Value::Object(stated.clone()), &expected())
            .unwrap_or_else(|| "none by value (the text differs)".to_string())
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

/// Where no lane asks ahead, a service's store request is answered before its
/// implementations ask is: the request left first. Two services that send
/// the same request take turns, and the second waits for the first's answer,
/// so at least one of them shows it in every scan; the scan with a lane keeps
/// its ask where the lane's hand-off is claimed.
#[test]
fn the_store_is_asked_before_the_implementations_with_no_lane() {
    for (scan, scanned) in scanned() {
        if *scan == "incremental" {
            continue;
        }
        let early: Vec<&str> = ["cli", "srv"]
            .into_iter()
            .filter(|service| {
                let lines = service_lines(&scanned.log, service);
                let asked = position(
                    &lines,
                    0,
                    "Mock fixture hit for /library-claims",
                    service,
                    scan,
                );
                let implementations = position(&lines, 0, "implementations:", service, scan);
                asked < implementations
            })
            .collect();
        assert!(
            !early.is_empty(),
            "{scan} scan: no service's store request left before its implementations ask"
        );
    }
}

/// Each service's store is asked exactly once per scan: the ask that now
/// leaves early is the only one. Where the lane asks ahead for cli, the ask
/// is the lane's (it logs under no service) and cli's analysis takes its
/// answers instead of asking again.
#[test]
fn each_service_asks_the_store_once_per_scan() {
    for (scan, scanned) in scanned() {
        let cli = if *scan == "incremental" { 0 } else { 1 };
        assert_eq!(
            own_store_asks(&scanned.log, "cli"),
            cli,
            "{scan} scan, cli: store asks"
        );
        assert_eq!(
            own_store_asks(&scanned.log, "srv"),
            1,
            "{scan} scan, srv: store asks"
        );
        assert_eq!(
            lines_with(&scanned.log, "Mock fixture hit for /library-claims"),
            2,
            "{scan} scan: store asks in the whole run"
        );
    }
}

/// The lane ran in the incremental scan that has it: the run log says it
/// asked the file reader ahead for cli, the service that waits its turn and
/// whose file changed, and that cli's analysis took its answer. The scans
/// without it say nothing of the kind: with every service admitted at once
/// (the full scan) the lane is never ahead, and with no lane nothing asks.
#[test]
fn the_lane_runs_in_the_scan_that_has_it() {
    for (scan, scanned) in scanned() {
        let asked = lines_with(&scanned.log, "Asking the file reader ahead for cli:");
        let taken = lines_with(
            &scanned.log,
            "Answers ahead for cli: asked 1 file(s), taken 1",
        );
        if *scan == "incremental" {
            assert_eq!(
                (asked, taken),
                (1, 1),
                "{scan} scan: the lane asked ahead for cli and its answer was taken"
            );
        } else {
            assert_eq!(asked, 0, "{scan} scan: no lane asks ahead");
        }
    }
}

/// carrick#3869 review: the lane's store ask for cli panics. That is a miss,
/// never a failed scan: cli's analysis asks the store for itself, the scan
/// succeeds without a crash report, and it states what the scan with a lane
/// that did not panic states.
#[test]
fn a_lane_store_ask_that_panics_is_asked_again_by_the_analysis() {
    let tree = Tree::new();
    tree.scan(Lane::Admitted);
    std::fs::write(
        tree.repo
            .join("__llm__/library-claims/panics_when_asked_ahead.txt"),
        "the lane's store ask panicked (carrick#3869 test)\n",
    )
    .unwrap();
    tree.edit("first");
    let scanned = tree.scan(Lane::Ahead);
    // Three store requests: the lane's for cli, which panicked before its
    // answer was logged (so the lane, with no store answer, asks the file
    // reader nothing), srv's, and cli's own.
    assert_eq!(
        lines_with(&scanned.log, "library-claims 3"),
        1,
        "the lane asked the store for cli, and cli's analysis asked again"
    );
    assert_eq!(
        own_store_asks(&scanned.log, "cli"),
        1,
        "cli's analysis asked the store for itself"
    );
    // The panic hook, for a panic nothing catches, prints Rust's own
    // `panicked at` line and logs `panic at <place>: <message>`.
    assert!(
        !scanned.stderr.contains("panicked at"),
        "the lane's panic is printed as the run dying:\n{}",
        scanned.stderr
    );
    assert_eq!(
        scanned
            .log
            .lines()
            .filter(|line| line.contains("panic at ") && line.contains("carrick#3869 test"))
            .count(),
        0,
        "the lane's panic is logged as the run dying"
    );
    let stated = serde_json::json!({ "incremental": scanned.stated });
    let expected = serde_json::json!({ "incremental": expected()["incremental"] });
    if let Some(difference) = first_difference(&stated, &expected) {
        panic!("the scan states something else than one whose lane did not panic: {difference}");
    }
}
