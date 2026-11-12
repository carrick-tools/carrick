//! A wrapper whose request line leaves the path open to its callers
//! (`http.post(`/v1${endpoint}`)`) is stated by its callers' rows, one per
//! call, and not again at its own request line (carrick#3961). The model's
//! row at that line goes, whatever path it states, unless a call of the
//! wrapper was not read (a caller stop) or a function holding the request
//! open has no caller the summaries see: then the line keeps it.
//!
//! Drives the real scanner binary on `.`, as the Action runs it, over a
//! committed copy of `tests/fixtures/wrapper-request-line/repo` (one root
//! service), offline, with the fixture's store answer replayed as the
//! library store's and one model answer per wrapper file.
//!
//! Protocols: HTTP only. A socket emit or a topic publish through a wrapper
//! whose event or topic is open is read by the fact layer, not here: a gap.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wrapper-request-line")
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

fn blob() -> &'static serde_json::Value {
    static BLOB: OnceLock<serde_json::Value> = OnceLock::new();
    BLOB.get_or_init(run_scan)
}

fn run_scan() -> serde_json::Value {
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
    copy_dir(&fixture_dir().join("model-cassette"), &cassettes);
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
    let mut blobs = Vec::new();
    for entry in std::fs::read_dir(&storage).expect("the store reads") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|ext| ext == "json") {
            blobs.push(serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap());
        }
    }
    assert_eq!(blobs.len(), 1, "the scan stored one blob");
    blobs.pop().unwrap()
}

/// Every call row, by `file:line`: `METHOD path` and its source.
fn rows() -> BTreeMap<String, Vec<(String, String)>> {
    let mut rows: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for row in blob()["calls"].as_array().into_iter().flatten() {
        let key = &row["key"];
        let text = format!(
            "{} {}",
            key["method"].as_str().unwrap_or_default(),
            key["path"].as_str().unwrap_or_default()
        );
        rows.entry(row["file_path"].as_str().unwrap_or_default().to_string())
            .or_default()
            .push((
                text,
                row["resolution_source"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            ));
    }
    rows
}

fn key() -> serde_json::Value {
    serde_json::from_str(
        &std::fs::read_to_string(fixture_dir().join("rows.expected.json")).unwrap(),
    )
    .unwrap()
}

/// The one model row the key says a request line keeps.
fn keeps(at: &str) {
    let want = key()["kept"][at].as_str().unwrap().to_string();
    assert_eq!(
        rows().get(at).cloned().unwrap_or_default(),
        vec![(want, "model".to_string())],
        "{at}: {:#?}",
        rows()
    );
}

/// Each wrapper whose path its callers fill in is stated by its callers'
/// rows alone: the model's row at its request line goes, whether it reads
/// the path open (`orders`), the same as the one caller's (`ping`), or a
/// host the code does not write (`billing`).
#[test]
fn callers_state_the_request_once() {
    let rows = rows();
    let restated: Vec<String> = key()["stated_at_callers"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|at| {
            let at = at.as_str().unwrap();
            rows.get(at).map(|stated| format!("{at}: {stated:?}"))
        })
        .collect();
    assert!(
        restated.is_empty(),
        "a request line restates its callers' requests:\n{}\n{rows:#?}",
        restated.join("\n")
    );
}

/// A caller whose argument is not read leaves an unread site, not a row:
/// the request line keeps the model's row.
#[test]
fn a_caller_stop_keeps_the_request_line() {
    keeps("src/net/reports.ts:4");
}

/// A function holding the request open that nothing calls has callers this
/// scan does not see: with no caller at all (`relay`), and beside a caller
/// that fills it (`forward`, held open by `forwardAny`).
#[test]
fn a_function_holding_the_request_open_with_no_caller_keeps_the_request_line() {
    keeps("src/net/relay.ts:4");
    keeps("src/net/forward.ts:4");
}

/// A call made where a module is loaded states no row: the request line
/// keeps the model's row beside the row of its one read caller.
#[test]
fn a_call_where_a_module_loads_keeps_the_request_line() {
    keeps("src/net/notify.ts:4");
}

/// The callers' rows and the unread sites are what they were before the
/// request line's row went.
#[test]
fn caller_rows_and_unread_sites_are_unchanged() {
    let key = key();
    let stated: BTreeMap<String, Vec<String>> = rows()
        .into_iter()
        .filter_map(|(at, rows)| {
            let rows: Vec<String> = rows
                .into_iter()
                .filter(|(_, source)| source != "model")
                .map(|(text, _)| text)
                .collect();
            (!rows.is_empty()).then_some((at, rows))
        })
        .collect();
    let want: BTreeMap<String, Vec<String>> = key["caller_rows"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(at, row)| (at.clone(), vec![row.as_str().unwrap().to_string()]))
        .collect();
    assert_eq!(stated, want);
    let unread: BTreeSet<String> = blob()["unread_sites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|site| {
            format!(
                "{}:{} {}",
                site["file"].as_str().unwrap(),
                site["line"],
                site["reason"].as_str().unwrap()
            )
        })
        .collect();
    let want: BTreeSet<String> = key["unread_sites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|site| site.as_str().unwrap().to_string())
        .collect();
    assert_eq!(unread, want);
}

// The types a withdrawn request line held move to its callers' rows
// (carrick#3961), over `tests/fixtures/wrapper-request-line-types`: a
// consumer service `web` whose two wrappers leave the path to their callers
// and build the body themselves, and a producer service `api` serving both
// paths. The model's row at each request line states a response type and a
// payload; the callers' own calls type neither side. (A caller that types a
// side from its own call keeps its own: the type requests' unit test.)

const CHECKOUT: &str = "web/src/checkout.ts";

fn types_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wrapper-request-line-types")
}

/// Whether the live sidecar can run. CI builds it before the Rust tests, so
/// a skip there would make a green tick mean nothing.
fn sidecar_available() -> bool {
    let node = Command::new("node").arg("--version").output().is_ok();
    let built = std::env::var_os("CARRICK_SIDECAR_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar"))
        .join("dist/src/index.js")
        .exists();
    assert!(
        (node && built) || std::env::var("CI").is_err(),
        "node or the built sidecar is missing in CI, so this test would silently not run"
    );
    node && built
}

/// Every service's blob from one offline scan of the types fixture, keyed by
/// service name.
fn typed_blobs() -> &'static BTreeMap<String, serde_json::Value> {
    static BLOBS: OnceLock<BTreeMap<String, serde_json::Value>> = OnceLock::new();
    BLOBS.get_or_init(|| {
        let tmp = tempfile::tempdir().expect("temp dir");
        let repo = tmp.path().join("workspace");
        copy_dir(&types_fixture_dir().join("repo"), &repo);
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["add", "-A", "-f"]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        let cassettes = tmp.path().join("cassettes");
        std::fs::create_dir_all(cassettes.join("library-claims")).unwrap();
        std::fs::copy(
            types_fixture_dir().join("library-claims.json"),
            cassettes.join("library-claims/default.json"),
        )
        .unwrap();
        copy_dir(&types_fixture_dir().join("model-cassette"), &cassettes);
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
        let mut blobs = BTreeMap::new();
        for entry in std::fs::read_dir(&storage).expect("the store reads") {
            let path = entry.expect("dir entry").path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let blob: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                let service = blob["service_name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                blobs.insert(service, blob);
            }
        }
        blobs
    })
}

fn web() -> &'static serde_json::Value {
    typed_blobs()
        .get("web")
        .unwrap_or_else(|| panic!("a blob for web: {:?}", typed_blobs().keys()))
}

/// The consumer type entries in `file`, as `(line, type_kind, path)`.
fn consumer_entries(file: &str) -> Vec<(u64, String, String)> {
    let mut entries: Vec<(u64, String, String)> = web()["type_manifest"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["role"] == "consumer" && entry["file_path"] == file)
        .map(|entry| {
            (
                entry["line_number"].as_u64().unwrap_or_default(),
                entry["type_kind"].as_str().unwrap_or_default().to_string(),
                entry["path"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    entries.sort();
    entries
}

/// The `(producer, side, verdict)` stored for the call at `file:line`.
fn verdicts_at(file: &str, line: u32) -> Vec<(String, String, String)> {
    let site = format!("{file}:{line}");
    let mut found: Vec<(String, String, String)> = typed_blobs()
        .values()
        .flat_map(|blob| {
            blob["compat_verdicts"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .flat_map(|pair| {
            let producer = pair["producer_key"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            pair["sites"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|at| at["consumer_location"] == site.as_str())
                .flat_map(|at| {
                    ["request", "response"]
                        .into_iter()
                        .filter_map(|side| {
                            at[side]["verdict"].as_str().map(|verdict| {
                                (producer.clone(), side.to_string(), verdict.to_string())
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .collect();
    found.sort();
    found
}

/// The request lines state no row and keep no type: their callers hold both.
#[test]
fn a_withdrawn_request_line_leaves_no_row_and_no_type_at_its_line() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let at_request_lines: Vec<&serde_json::Value> = web()["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| {
            let at = row["file_path"].as_str().unwrap_or_default();
            at.starts_with("web/src/net/orders.ts") || at.starts_with("web/src/net/refunds.ts")
        })
        .collect();
    assert!(at_request_lines.is_empty(), "{at_request_lines:#?}");
    assert_eq!(consumer_entries("web/src/net/orders.ts"), []);
    assert_eq!(consumer_entries("web/src/net/refunds.ts"), []);
}

/// Each caller row holds one request and one response type, read at the
/// request line it states.
#[test]
fn each_caller_row_holds_one_type_per_side() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let orders = "/v1/orders".to_string();
    let refunds = "/v1/refunds".to_string();
    let at_callers: Vec<(u64, String, String)> = consumer_entries(CHECKOUT)
        .into_iter()
        .filter(|(line, _, _)| (5..=7).contains(line))
        .collect();
    assert_eq!(
        at_callers,
        [
            (5, "request".to_string(), orders.clone()),
            (5, "response".to_string(), orders.clone()),
            (6, "request".to_string(), orders.clone()),
            (6, "response".to_string(), orders),
            (7, "request".to_string(), refunds.clone()),
            (7, "response".to_string(), refunds),
        ]
    );
}

/// A call that states two requests (`openSession` sends a token request
/// first) holds two rows, and one row's types cannot sit beside the other
/// on the same line: the request line keeps the model's row and its types.
#[test]
fn a_caller_with_two_requests_keeps_the_request_line() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let kept: Vec<String> = web()["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["file_path"] == "web/src/net/session.ts:5")
        .map(|row| {
            row["resolution_source"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(kept, ["model"]);
    assert_eq!(
        consumer_entries("web/src/net/session.ts"),
        [
            (5, "request".to_string(), "/v1/:endpoint".to_string()),
            (5, "response".to_string(), "/v1/:endpoint".to_string()),
        ]
    );
}

/// The pair against the producer is judged at each caller, on both sides.
#[test]
fn the_pair_is_judged_at_each_caller() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let both = |producer: &str| {
        vec![
            (
                producer.to_string(),
                "request".to_string(),
                "compatible".to_string(),
            ),
            (
                producer.to_string(),
                "response".to_string(),
                "compatible".to_string(),
            ),
        ]
    };
    assert_eq!(verdicts_at(CHECKOUT, 5), both("http|POST|/v1/orders"));
    assert_eq!(verdicts_at(CHECKOUT, 6), both("http|POST|/v1/orders"));
    assert_eq!(verdicts_at(CHECKOUT, 7), both("http|POST|/v1/refunds"));
}
