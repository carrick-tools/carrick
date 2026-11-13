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

/// A call made where a module loads (`alerts.ts:3`) states the request
/// in a fact row of its own, beside the summary row of the wrapper's other
/// caller (`alerts.ts:2`): the request line states no row (carrick#4121).
#[test]
fn a_call_where_a_module_loads_states_the_request_at_its_callers() {
    let rows = rows();
    assert_eq!(
        rows.get("src/net/notify.ts:4"),
        None,
        "the request line restates its callers' requests: {rows:#?}"
    );
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
    BLOBS.get_or_init(|| scan_typed_fixture(&types_fixture_dir()))
}

/// Every service's blob from one offline scan of a two-service types
/// fixture (`repo/`, `library-claims.json`, `model-cassette/`), keyed by
/// service name.
fn scan_typed_fixture(fixture: &Path) -> BTreeMap<String, serde_json::Value> {
    {
        let tmp = tempfile::tempdir().expect("temp dir");
        let repo = tmp.path().join("workspace");
        copy_dir(&fixture.join("repo"), &repo);
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["add", "-A", "-f"]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        let cassettes = tmp.path().join("cassettes");
        std::fs::create_dir_all(cassettes.join("library-claims")).unwrap();
        std::fs::copy(
            fixture.join("library-claims.json"),
            cassettes.join("library-claims/default.json"),
        )
        .unwrap();
        copy_dir(&fixture.join("model-cassette"), &cassettes);
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
    }
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

// A wrapper called where a module loads (carrick#4121): `reorder` is called
// at module level (`boot.ts:2`, a fact row) and inside a function
// (`boot.ts:3`, a summary row). The request line states no row, and the
// request type its model row held moves to both callers.

const BOOT: &str = "web/src/boot.ts";

/// The request line states no row and keeps no type.
#[test]
fn a_request_line_its_module_level_caller_states_leaves_no_row_and_no_type() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let at_request_line: Vec<&serde_json::Value> = web()["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| {
            row["file_path"]
                .as_str()
                .unwrap_or_default()
                .starts_with("web/src/net/reorder.ts")
        })
        .collect();
    assert!(at_request_line.is_empty(), "{at_request_line:#?}");
    assert_eq!(consumer_entries("web/src/net/reorder.ts"), []);
}

/// The request side of the pair judged at one caller of `reorder`.
fn request_verdicts_at(line: u32) -> Vec<(String, String, String)> {
    verdicts_at(BOOT, line)
        .into_iter()
        .filter(|(_, side, _)| side == "request")
        .collect()
}

/// The module-level fact row takes the request type of the request line.
#[test]
fn a_module_level_fact_row_takes_the_request_type_of_the_request_line() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    assert!(
        consumer_entries(BOOT)
            .iter()
            .any(|(line, kind, _)| *line == 2 && kind == "request"),
        "{:#?}",
        consumer_entries(BOOT)
    );
    assert_eq!(
        request_verdicts_at(2),
        [(
            "http|POST|/v1/orders".to_string(),
            "request".to_string(),
            "compatible".to_string()
        )]
    );
}

/// The summary row of the wrapper's other caller takes it too.
#[test]
fn a_summary_caller_beside_a_module_level_caller_takes_the_request_type() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    assert!(
        consumer_entries(BOOT)
            .iter()
            .any(|(line, kind, _)| *line == 3 && kind == "request"),
        "{:#?}",
        consumer_entries(BOOT)
    );
    assert_eq!(
        request_verdicts_at(3),
        [(
            "http|POST|/v1/orders".to_string(),
            "request".to_string(),
            "compatible".to_string()
        )]
    );
}

// ── Wrappers that reshape what they hand back (carrick#3961) ────────────
//
// `tests/fixtures/wrapper-request-line-transform`: `web` calls `api` through
// `fetch` wrappers whose request lines leave the path to their callers. The
// response read at a request line is the one a caller holds only where the
// wrapper hands back the request's own value, or its parsed body (then the
// caller's own call is typed). A caller of any other wrapper holds the
// wrapper's own value (an envelope, a token, the raw `Response`): that is no
// wire response and is never judged against one. Every caller keeps the
// request type the request line sends.

const APP: &str = "web/src/app.ts";
const TOOLS: &str = "web/src/tools.ts";

fn transform_blobs() -> &'static BTreeMap<String, serde_json::Value> {
    static BLOBS: OnceLock<BTreeMap<String, serde_json::Value>> = OnceLock::new();
    BLOBS.get_or_init(|| {
        scan_typed_fixture(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/wrapper-request-line-transform"),
        )
    })
}

fn transform_web() -> &'static serde_json::Value {
    transform_blobs()
        .get("web")
        .unwrap_or_else(|| panic!("a blob for web: {:?}", transform_blobs().keys()))
}

/// The `web` consumer entries of one side, as `"<file>:<line>"`.
fn transform_entries(side: &str) -> BTreeSet<String> {
    transform_web()["type_manifest"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["role"] == "consumer" && entry["type_kind"] == side)
        .map(|entry| {
            format!(
                "{}:{}",
                entry["file_path"].as_str().unwrap_or_default(),
                entry["line_number"].as_u64().unwrap_or_default()
            )
        })
        .collect()
}

/// The `(side, verdict)` pairs stored for the call at `site`.
fn transform_verdicts(site: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = transform_blobs()
        .values()
        .flat_map(|blob| {
            blob["compat_verdicts"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .flat_map(|pair| {
            pair["sites"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|at| at["consumer_location"] == site)
                .flat_map(|at| {
                    ["request", "response"]
                        .into_iter()
                        .filter_map(|side| {
                            at[side]["verdict"]
                                .as_str()
                                .map(|verdict| (side.to_string(), verdict.to_string()))
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .collect();
    found.sort();
    found
}

fn sites(file: &str, lines: &[u32]) -> BTreeSet<String> {
    lines.iter().map(|line| format!("{file}:{line}")).collect()
}

/// Every wrapper's request line gives its row to its callers, so each case
/// below is read at a caller.
#[test]
fn the_transform_fixture_states_each_request_at_its_callers() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let rows: BTreeSet<String> = transform_web()["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| row["file_path"].as_str().unwrap_or_default().to_string())
        .collect();
    let mut callers = sites(APP, &[8, 9, 10, 11, 12, 16]);
    callers.extend(sites(TOOLS, &[24, 32]));
    assert_eq!(rows, callers);
}

/// A wrapper that hands back an envelope (`getJson`, app.ts:8 and
/// tools.ts:32), one field of the body (`exchangeToken`, app.ts:9 and
/// tools.ts:24) or the raw `Response` (`streamPost`, app.ts:11 and :16;
/// `send`, app.ts:12): its caller holds that value, so it carries no
/// response type and gets no response verdict.
#[test]
fn a_caller_of_a_wrapper_that_reshapes_the_response_is_not_judged_on_it() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    assert_eq!(transform_entries("response"), sites(APP, &[10]));
    let reshaped = sites(APP, &[8, 9, 11, 12, 16])
        .into_iter()
        .chain(sites(TOOLS, &[24, 32]));
    for site in reshaped {
        let judged: Vec<(String, String)> = transform_verdicts(&site)
            .into_iter()
            .filter(|(side, _)| side == "response")
            .collect();
        assert_eq!(judged, [], "{site}");
    }
}

/// A wrapper that hands back the parsed body unchanged (`readTicket`,
/// app.ts:10) makes its call's value the response, judged against the
/// producer.
#[test]
fn a_caller_of_a_wrapper_that_hands_back_the_body_is_judged_on_the_response() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    assert_eq!(
        transform_verdicts(&format!("{APP}:10")),
        [("response".to_string(), "compatible".to_string())]
    );
}

/// Each caller of a request line that sends a body keeps a request type,
/// whether the wrapper builds the body (`exchangeToken`), serialises its
/// parameter (`streamPost`) or passes its options through (`send`); a GET
/// caller keeps none. The built body is judged at the caller.
#[test]
fn a_withdrawn_request_line_keeps_its_request_type_at_each_caller() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let mut senders = sites(APP, &[9, 11, 12, 16]);
    senders.extend(sites(TOOLS, &[24]));
    assert_eq!(transform_entries("request"), senders);
    for site in [format!("{APP}:9"), format!("{TOOLS}:24")] {
        assert_eq!(
            transform_verdicts(&site),
            [("request".to_string(), "compatible".to_string())],
            "{site}"
        );
    }
}

// ── A through site takes the body its wrapper builds (carrick#4128) ─────
//
// `restock` builds its body (`{ body: 'book' }`) and leaves the path open.
// `stock.ts:3` calls it with a path the fact layer reads, so it is a
// decided site whose payload the wrapper builds: its request slot is that
// body. `stock.ts:2` is a lead, so `restock.ts:5` keeps its model row.

const STOCK: &str = "web/src/stock.ts";
const SHOP: &str = "web/src/shop.ts";
const GEN: &str = "web/src/gen.ts";

/// The request type printed for the consumer entry at `file:line`.
fn request_type_at(file: &str, line: u32) -> Option<String> {
    let alias = web()["type_manifest"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| {
            entry["role"] == "consumer"
                && entry["type_kind"] == "request"
                && entry["file_path"] == file
                && entry["line_number"] == line
        })?["type_alias"]
        .as_str()?
        .to_string();
    web()["bundled_types"].as_str()?.lines().find_map(|text| {
        text.strip_prefix(&format!("export type {alias} = "))
            .map(|rest| rest.trim_end_matches(';').trim().to_string())
    })
}

#[test]
fn a_through_fact_row_takes_the_body_its_wrapper_builds() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    assert_eq!(
        request_type_at(STOCK, 3).as_deref(),
        Some("{ body: string; }"),
        "{:#?}",
        consumer_entries(STOCK)
    );
    assert_eq!(
        request_verdicts_in(STOCK, 3),
        [(
            "http|POST|/v1/orders".to_string(),
            "request".to_string(),
            "compatible".to_string()
        )]
    );
    // The lead keeps the wrapper's model row.
    assert!(
        web()["calls"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|row| row["file_path"]
                .as_str()
                .unwrap_or_default()
                .starts_with("web/src/net/restock.ts")),
        "{:#?}",
        web()["calls"]
    );
}

/// The generic wrapper's own row (carrick#4164): `gen.ts:4` is a lead, so
/// `net/generic.ts:5` keeps its model row, which names no caller. Its body
/// `{ body: item }` holds the wrapper's type parameter, which only a caller
/// binds: the row publishes no type naming it, only a decided unknown.
#[test]
fn a_generic_wrappers_own_row_publishes_no_open_type_parameter() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    const GENERIC: &str = "web/src/net/generic.ts";
    let own = web()["type_manifest"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| {
            entry["role"] == "consumer"
                && entry["type_kind"] == "request"
                && entry["file_path"] == GENERIC
                && entry["line_number"] == 5
        })
        .unwrap_or_else(|| panic!("{:#?}", consumer_entries(GENERIC)));
    assert_eq!(own["type_state"], "unknown", "{own:#}");
    assert_eq!(
        own["any_provenance"][0]["reason"], "bound_by_callers",
        "{own:#}"
    );
    assert_ne!(request_type_at(GENERIC, 5).as_deref(), Some("{ body: T; }"));
}

fn request_verdicts_in(file: &str, line: u32) -> Vec<(String, String, String)> {
    verdicts_at(file, line)
        .into_iter()
        .filter(|(_, side, _)| side == "request")
        .collect()
}

/// A wrapper that sends `JSON.stringify(data)` forwards its parameter whole:
/// the caller's own argument is still the body. One that builds `{ body }`
/// from a loosely typed parameter reads the declared inner type, the right
/// shape; the caller's argument (a string) was the wrong one. One that builds
/// it from a generic parameter reads it as the caller instantiates the
/// wrapper (`gen.ts:2`); a caller that leaves the parameter open (`gen.ts:3`,
/// which passes its own) publishes no type naming it.
#[test]
fn a_body_built_from_a_loose_parameter_reads_the_declared_type() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    assert_eq!(
        request_type_at(SHOP, 3).as_deref(),
        Some("{ body: string; }")
    );
    assert_eq!(
        request_type_at(SHOP, 4).as_deref(),
        Some("{ body: unknown; }")
    );
    assert_eq!(
        request_type_at(GEN, 2).as_deref(),
        Some("{ body: string; }"),
        "{:#?}",
        consumer_entries(GEN)
    );
    assert_eq!(
        request_verdicts_in(GEN, 2),
        [(
            "http|POST|/v1/orders".to_string(),
            "request".to_string(),
            "compatible".to_string()
        )]
    );
    // The relay's entry is a decided unknown, with the reason it is one.
    let relay = web()["type_manifest"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| {
            entry["role"] == "consumer"
                && entry["type_kind"] == "request"
                && entry["file_path"] == GEN
                && entry["line_number"] == 3
        })
        .unwrap_or_else(|| panic!("{:#?}", consumer_entries(GEN)));
    assert_eq!(relay["type_state"], "unknown", "{relay:#}");
    assert_eq!(
        relay["any_provenance"][0]["reason"], "bound_by_callers",
        "{relay:#}"
    );
}

// `tests/fixtures/hand-back-object` (carrick#4185): `web` calls `api` through
// wrappers that hand back the transport response beside the parsed body. A
// caller's value is that object, never the body: the body is the member the
// wrapper fills from the body read, and where no one member is decided the
// site states no response contract.

const HAND_BACK_APP: &str = "web/src/app.ts";

fn hand_back_blobs() -> &'static BTreeMap<String, serde_json::Value> {
    static BLOBS: OnceLock<BTreeMap<String, serde_json::Value>> = OnceLock::new();
    BLOBS.get_or_init(|| {
        scan_typed_fixture(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hand-back-object"),
        )
    })
}

/// The `(side, verdict)` pairs stored for the call at `site`.
fn hand_back_verdicts(site: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = hand_back_blobs()
        .values()
        .flat_map(|blob| {
            blob["compat_verdicts"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .flat_map(|pair| {
            pair["sites"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|at| at["consumer_location"] == site)
                .flat_map(|at| {
                    ["request", "response"]
                        .into_iter()
                        .filter_map(|side| {
                            at[side]["verdict"]
                                .as_str()
                                .map(|verdict| (side.to_string(), verdict.to_string()))
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .collect();
    found.sort();
    found
}

/// `call` fills `json` from the body read in its one return: the caller is
/// judged on that member, not on `{ res, json }`.
#[test]
fn a_hand_back_object_is_judged_on_its_body_member() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    assert_eq!(
        hand_back_verdicts(&format!("{HAND_BACK_APP}:4")),
        [("response".to_string(), "compatible".to_string())]
    );
}

/// `callEither` fills a different member in each return: no member is the
/// body, so the pair is unverifiable rather than judged on the object.
#[test]
fn a_hand_back_object_with_no_decided_body_member_is_unverifiable() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    assert_eq!(
        hand_back_verdicts(&format!("{HAND_BACK_APP}:9")),
        [("response".to_string(), "unverifiable".to_string())]
    );
}
