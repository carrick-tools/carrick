//! The compiler engine's pass over a model's pairs (`carrick __matched-pass`),
//! run as the cloud runner runs it: scan the fixture into a local store
//! offline, then hand the pass the stored record `carrick.matched/1`
//! (`tests/fixtures/matched-pairs/record.json`, invented) with the scanned
//! tree and the store.
//!
//! The record's pairs cover every rule the pass decides: the caller line it
//! locates, the service a file belongs to, the callee that goes through the
//! repository's own code, the wrapper callers read off the scan's rows, and
//! how each kind of pair is read (a route and a deployed function at both
//! ends; a table and a database function at the caller only; a topic at
//! neither). A3 (`scan_blob_unchanged_by_matched_pass`): the pass leaves
//! the scan's store and tree untouched, and the scan after it writes what the
//! scan before it did.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use serde_json::Value;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Whether the built sidecar is there. A local run without the build skips;
/// in CI a missing build fails, so a green tick means the test ran.
fn sidecar_built() -> bool {
    let built = manifest_dir()
        .join("src/sidecar/dist/src/index.js")
        .exists();
    assert!(
        built || std::env::var("CI").is_err(),
        "the built sidecar is missing in CI (cd src/sidecar && npm run build)"
    );
    if !built {
        eprintln!("Skipping: sidecar not built (cd src/sidecar && npm run build)");
    }
    built
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Every file under `dir`, by its path relative to `dir`, with its bytes.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.insert(
                    path.strip_prefix(dir).unwrap().to_path_buf(),
                    std::fs::read(&path).unwrap(),
                );
            }
        }
    }
    files
}

fn carrick(args: &[&std::ffi::OsStr]) -> Command {
    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.args(args);
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
        "CARRICK_LOCAL_STORAGE_DIR",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

/// Scan `tree` offline into `store`, as the runner's scan lands its blobs and
/// rows before the pass runs.
fn scan(tree: &Path, store: &Path, scratch: &Path) -> Output {
    let cache = scratch.join("cache");
    let cassettes = scratch.join("cassettes");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&cassettes).unwrap();
    let output = carrick(&[tree.as_os_str()])
        .env("CARRICK_LOCAL_STORAGE_DIR", store)
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", &cache)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .output()
        .expect("spawn carrick");
    assert!(
        output.status.success(),
        "scan failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn pass(tree: &Path, store: &Path, out: &Path) -> Value {
    let record = tree.join("record.json");
    let output = carrick(&[
        "__matched-pass".as_ref(),
        record.as_os_str(),
        "--repo".as_ref(),
        tree.as_os_str(),
        "--store".as_ref(),
        store.as_os_str(),
        "--out".as_ref(),
        out.as_os_str(),
    ])
    .output()
    .expect("spawn carrick");
    assert!(
        output.status.success(),
        "pass failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&std::fs::read(out).unwrap()).unwrap()
}

struct Run {
    input: Value,
    output: Value,
    tree_before: BTreeMap<PathBuf, Vec<u8>>,
    tree_after: BTreeMap<PathBuf, Vec<u8>>,
    store_before: BTreeMap<PathBuf, Vec<u8>>,
    store_after: BTreeMap<PathBuf, Vec<u8>>,
    /// The blobs of a second scan of the same tree after the pass.
    rescan: BTreeMap<PathBuf, Vec<u8>>,
    scan_stdout: Vec<u8>,
    rescan_stdout: Vec<u8>,
    _dir: tempfile::TempDir,
}

/// One scan, one pass and one rescan, shared by the tests: the pass starts
/// several compiler processes.
fn run() -> Option<&'static Run> {
    static RUN: OnceLock<Option<Run>> = OnceLock::new();
    RUN.get_or_init(|| {
        if !sidecar_built() {
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        // The scan names the repository by its folder; the record names it
        // `example-org/widgets`.
        let tree = dir.path().join("widgets");
        copy_tree(&manifest_dir().join("tests/fixtures/matched-pairs"), &tree);
        let store = dir.path().join("store");
        let scanned = scan(&tree, &store, &dir.path().join("scan-1"));
        let tree_before = snapshot(&tree);
        let store_before = snapshot(&store);
        let input: Value =
            serde_json::from_slice(&std::fs::read(tree.join("record.json")).unwrap()).unwrap();
        let output = pass(&tree, &store, &dir.path().join("out.json"));
        let tree_after = snapshot(&tree);
        let store_after = snapshot(&store);
        let store_2 = dir.path().join("store-2");
        let rescanned = scan(&tree, &store_2, &dir.path().join("scan-2"));
        Some(Run {
            input,
            output,
            tree_before,
            tree_after,
            store_before,
            store_after,
            rescan: snapshot(&store_2),
            scan_stdout: scanned.stdout,
            rescan_stdout: rescanned.stdout,
            _dir: dir,
        })
    })
    .as_ref()
}

/// The output pair at `file:line` naming `name`.
fn pair<'a>(record: &'a Value, file: &str, line: u64, name: &str) -> &'a Value {
    record["pairs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pair| {
            pair["caller"]["file"] == file
                && pair["caller"]["line"] == line
                && pair["operation"]["name"] == name
        })
        .unwrap_or_else(|| panic!("no pair {file}:{line} {name}"))
}

fn compiler<'a>(pair: &'a Value, side: &str) -> &'a Value {
    &pair["sides"][side]["compiler"]
}

fn reason<'a>(pair: &'a Value, side: &str) -> &'a Value {
    &compiler(pair, side)["reason"]
}

fn has_text(pair: &Value, side: &str) -> bool {
    compiler(pair, side)["text"].is_string()
}

const CLIENT: &str = "apps/web/src/client.ts";
const PASS: &str = "apps/web/src/pass.ts";

#[test]
fn matched_pass_end_to_end() {
    let Some(run) = run() else { return };
    let out = &run.output;
    assert_eq!(out["engine"]["status"], "done", "{}", out["engine"]);

    // A typed direct call and the route's handler: both read and judged.
    let typed = pair(out, CLIENT, 21, "/w/:id");
    assert!(has_text(typed, "caller_expects"), "{typed:#}");
    assert!(has_text(typed, "server_returns"), "{typed:#}");
    assert_eq!(typed["verdicts"]["compiler"]["grade"], "agree", "{typed:#}");
    // The model's reading beside it is kept as it was.
    assert_eq!(
        typed["sides"]["caller_expects"]["model"],
        run.input["pairs"][0]["sides"]["caller_expects"]["model"]
    );
    assert_eq!(
        typed["verdicts"]["model"],
        run.input["pairs"][0]["verdicts"]["model"]
    );

    let mismatch = pair(out, CLIENT, 26, "/w/:id");
    assert_eq!(mismatch["verdicts"]["compiler"]["grade"], Value::Null);
    assert_eq!(
        mismatch["verdicts"]["compiler"]["compiler_verdict"],
        "incompatible"
    );

    // An own helper that hands back the parsed body: judged.
    let helper = pair(out, CLIENT, 31, "/w/:id");
    assert_eq!(
        helper["verdicts"]["compiler"]["compiler_verdict"], "incompatible",
        "{helper:#}"
    );
    // An own helper that reshapes the body: typed, not judged.
    let reshaped = pair(out, CLIENT, 16, "/list");
    assert!(has_text(reshaped, "caller_expects"), "{reshaped:#}");
    assert_eq!(reshaped["verdicts"]["compiler"]["grade"], "cannot_tell");
    assert!(
        reshaped["verdicts"]["compiler"]["detail"]
            .as_str()
            .unwrap()
            .contains("own code"),
        "{reshaped:#}"
    );

    // A route answered by a SQL file.
    let sql = pair(out, CLIENT, 36, "/sql/:id");
    assert!(has_text(sql, "caller_expects"));
    assert_eq!(reason(sql, "server_returns"), "not_typescript_source");
    assert_eq!(sql["verdicts"]["compiler"]["grade"], "cannot_tell");

    // A topic: neither end read in this build.
    let topic = pair(out, CLIENT, 41, "widget.created");
    for side in ["caller_expects", "server_returns"] {
        assert_eq!(reason(topic, side), "protocol_not_read", "{topic:#}");
    }

    // A `.then` chain is read at the call it is chained on.
    let chained = pair(out, PASS, 16, "/w/:id");
    assert!(has_text(chained, "server_returns"), "{chained:#}");
    assert!(
        compiler(chained, "caller_expects").get("text").is_some()
            || compiler(chained, "caller_expects").get("reason").is_some()
    );
    // Two pairs on one line with different operations name no one call.
    for name in ["/w/:id", "/list"] {
        let several = pair(out, PASS, 21, name);
        assert_eq!(reason(several, "caller_expects"), "several_sites_on_line");
        assert_eq!(several["wrapper"]["reason"], "several_sites_on_line");
    }
    // An options line holds no call; a deleted file holds no line.
    assert_eq!(
        reason(pair(out, PASS, 25, "/w/:id"), "caller_expects"),
        "not_located"
    );
    let gone = pair(out, "apps/web/src/gone.ts", 3, "/w/:id");
    assert_eq!(reason(gone, "caller_expects"), "not_located");
    assert_eq!(gone["wrapper"]["reason"], "not_located");

    // A deployed function: read at both ends and judged like a route.
    let function = pair(out, PASS, 29, "widget");
    assert!(has_text(function, "caller_expects"), "{function:#}");
    assert!(has_text(function, "server_returns"), "{function:#}");
    assert_eq!(
        function["verdicts"]["compiler"]["grade"], "agree",
        "{function:#}"
    );

    // Every pair says the request sides are not read.
    for pair in out["pairs"].as_array().unwrap() {
        for side in ["caller_sends", "server_reads"] {
            assert_eq!(reason(pair, side), "request_side_not_read", "{pair:#}");
        }
    }
}

#[test]
fn a_table_pair_reads_the_caller_only() {
    let Some(run) = run() else { return };
    // The server is a schema written in TypeScript: still no server type.
    let table = pair(&run.output, PASS, 34, "stock");
    assert!(has_text(table, "caller_expects"), "{table:#}");
    assert_eq!(reason(table, "server_returns"), "protocol_not_read");
    let verdict = &table["verdicts"]["compiler"];
    assert_eq!(verdict["grade"], "cannot_tell");
    assert_eq!(verdict["compiler_verdict"], "unverifiable");
    assert!(
        verdict["detail"].as_str().unwrap().contains("table"),
        "{verdict}"
    );

    let function = pair(&run.output, PASS, 39, "widget_by_id");
    assert!(has_text(function, "caller_expects"), "{function:#}");
    assert_eq!(reason(function, "server_returns"), "not_typescript_source");
    assert_eq!(function["verdicts"]["compiler"]["grade"], "cannot_tell");
}

#[test]
fn wrapper_callers_from_rows() {
    let Some(run) = run() else { return };
    let inside = pair(&run.output, PASS, 2, "/w/:id");
    assert_eq!(
        inside["wrapper"],
        serde_json::json!({
            "name": "apiGet",
            "file": PASS,
            "line": 1,
            "callers": [
                { "file": PASS, "line": 6 },
                { "file": PASS, "line": 10 },
                { "file": PASS, "line": 13 },
            ],
            "callers_total": 3,
        })
    );
    // A call at a module's top level sits in no wrapper; one inside a
    // function nothing calls has a wrapper with no callers.
    let typed = pair(&run.output, CLIENT, 21, "/w/:id");
    assert_eq!(typed["wrapper"]["name"], "readTotal");
    assert_eq!(typed["wrapper"]["callers_total"], 0);
}

/// The cloud refuses a whole record over one bad pair
/// (`validateRecordForStore` in carrick-cloud): every written field holds to
/// its rules.
#[test]
fn every_written_pair_is_one_the_cloud_stores() {
    let Some(run) = run() else { return };
    assert_eq!(
        run.output["pairs"].as_array().unwrap().len(),
        run.input["pairs"].as_array().unwrap().len()
    );
    for (pair, input) in run.output["pairs"]
        .as_array()
        .unwrap()
        .iter()
        .zip(run.input["pairs"].as_array().unwrap())
    {
        // Nothing the pass does not own moved.
        for key in [
            "id",
            "caller",
            "server",
            "operation",
            "basis",
            "model_confidence",
            "carried_from",
        ] {
            assert_eq!(pair[key], input[key], "{key}");
        }
        for (side, readings) in pair["sides"].as_object().unwrap() {
            let reading = &readings["compiler"];
            if let Some(text) = reading["text"].as_str() {
                assert!(
                    !matches!(text.trim(), "any" | "unknown" | ""),
                    "{side}: {text}"
                );
                assert!(text.len() <= 16_384);
                assert!(reading["evidence"]["file"].is_string(), "{side}");
                assert!(reading["evidence"]["line"].as_u64().unwrap() >= 1, "{side}");
            } else {
                let word = reading["reason"].as_str().expect("a reason");
                assert!(!word.is_empty());
            }
        }
        let verdict = &pair["verdicts"]["compiler"];
        assert!(verdict["compiler_verdict"].is_string(), "{verdict}");
        if verdict["grade"].is_null() {
            assert_eq!(verdict["compiler_verdict"], "incompatible");
        }
        if let Some(detail) = verdict["detail"].as_str() {
            assert!(detail.encode_utf16().count() <= 500);
        }
        let wrapper = &pair["wrapper"];
        if wrapper.get("reason").is_none() {
            let callers = wrapper["callers"].as_array().unwrap();
            assert!(callers.len() <= 200);
            assert!(wrapper["callers_total"].as_u64().unwrap() as usize >= callers.len());
        }
    }
    let engine = &run.output["engine"];
    assert!(engine["ms"].is_u64());
    assert!(engine["scanner_version"].is_string());
}

/// What a scan writes that is the same on every scan of one tree: the blob
/// without the fields that carry this run's time or memory.
fn stable_blobs(files: &BTreeMap<PathBuf, Vec<u8>>) -> BTreeMap<PathBuf, Value> {
    files
        .iter()
        .filter(|(path, _)| path.extension().is_some_and(|ext| ext == "json"))
        .map(|(path, bytes)| {
            let mut blob: Value = serde_json::from_slice(bytes).unwrap();
            if let Some(blob) = blob.as_object_mut() {
                for volatile in VOLATILE_BLOB_FIELDS {
                    blob.remove(*volatile);
                }
            }
            (path.clone(), blob)
        })
        .collect()
}

const VOLATILE_BLOB_FIELDS: &[&str] = &["last_updated", "memory_peaks"];

/// A3: the pass writes nothing but its output. The scan's store and tree are
/// byte-identical after it, and a scan after it writes what the scan before
/// it wrote.
#[test]
fn scan_blob_unchanged_by_matched_pass() {
    let Some(run) = run() else { return };
    assert!(
        run.store_before
            .keys()
            .any(|path| path.extension().is_some_and(|e| e == "json")),
        "the scan wrote no blob: {:?}",
        run.store_before.keys().collect::<Vec<_>>()
    );
    assert!(
        run.store_before
            .keys()
            .any(|path| path.starts_with("positions")),
        "the scan landed no rows: {:?}",
        run.store_before.keys().collect::<Vec<_>>()
    );
    assert!(
        run.store_before == run.store_after,
        "the pass changed the store"
    );
    assert!(
        run.tree_before == run.tree_after,
        "the pass changed the tree"
    );
    assert_eq!(stable_blobs(&run.store_before), stable_blobs(&run.rescan));
    assert_eq!(
        run.scan_stdout, run.rescan_stdout,
        "the scan's output moved"
    );
}
