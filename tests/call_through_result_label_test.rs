//! Call-through reads the library store's result label (carrick#2820, part
//! of the result-shape chain on carrick-cloud#2639).
//!
//! Drives the real scanner binary on `.`, over a committed copy of
//! `tests/fixtures/call-through-result-label/`: a client package whose
//! wrappers read an invented library client's result in the shapes a wrapper
//! can take, a CLI that calls them, and an API that serves the routes. The
//! store's answer is the fixture's cassette: the library's `get` is labelled
//! with the member `data`, its `send` with the whole value, and its `ping`
//! with nothing.
//!
//! The blobs state what the pass read: the member each wrapper reads, the
//! span of the call it returns, and the label the sidecar verified at each
//! library call. Whether the type captured at a caller is kept is the pass's
//! `settle`, which reads the mount graph; the rows site questions state sit
//! in the blob's `calls` and not in that graph (carrick#2795), so the kept
//! and dropped types are asserted in `src/call_through.rs`' unit tests over
//! the same facts, not here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const CLIENT_FILE: &str = "packages/client/src/users.ts";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/call-through-result-label")
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "carrick-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "carrick-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
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

/// Every key named `result` removed, at any depth.
fn without_results(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.remove("result");
            map.values_mut().for_each(without_results);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(without_results),
        _ => {}
    }
}

/// What one scan of the fixture left: each service's blob and the run log.
struct Scan {
    blobs: BTreeMap<String, serde_json::Value>,
    log: String,
}

/// The fixture scanned as the Action scans it with the fact layer on,
/// offline. `labelled: false` strips every `result` from the store's answer.
fn scan(labelled: bool) -> Scan {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir(), &repo);
    if !labelled {
        let cassette = repo.join("__llm__/library-claims/default.json");
        let mut answer: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&cassette).unwrap()).unwrap();
        without_results(&mut answer);
        std::fs::write(&cassette, serde_json::to_vec_pretty(&answer).unwrap()).unwrap();
    }
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let storage = tmp.path().join("storage");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&storage).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(&repo)
        .env("HOME", &home)
        .env("CARRICK_LOCAL_STORAGE_DIR", &storage)
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", tmp.path().join("cache"))
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env(
            "CARRICK_SIDECAR_DIR",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar"),
        )
        .env("CARRICK_POSITION_ROWS", "1")
        .env("CARRICK_SITE_QUESTIONS", "1");
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
        "CARRICK_FACT_FILES",
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
    let blobs: BTreeMap<String, serde_json::Value> = std::fs::read_dir(&storage)
        .unwrap()
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let blob: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
            Some((blob["service_name"].as_str()?.to_string(), blob))
        })
        .collect();
    assert_eq!(
        blobs.keys().map(String::as_str).collect::<Vec<_>>(),
        ["api", "cli", "client"],
        "every service uploads one blob"
    );
    let log = std::fs::read_dir(home.join(".carrick/logs/runs"))
        .expect("the scan keeps a run log")
        .filter_map(|entry| std::fs::read_to_string(entry.ok()?.path()).ok())
        .collect::<Vec<_>>()
        .join("\n");
    Scan { blobs, log }
}

fn labelled() -> &'static Scan {
    static SCAN: OnceLock<Scan> = OnceLock::new();
    SCAN.get_or_init(|| scan(true))
}

fn unlabelled() -> &'static Scan {
    static SCAN: OnceLock<Scan> = OnceLock::new();
    SCAN.get_or_init(|| scan(false))
}

/// The result label stored for the library call on `file:line`, with its span.
fn label_at(
    blob: &serde_json::Value,
    file: &str,
    line: u64,
) -> Option<(serde_json::Value, serde_json::Value)> {
    let matching: Vec<&serde_json::Value> = blob["call_through"]["request_results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["file"] == file && entry["line"] == line)
        .collect();
    match matching.as_slice() {
        [] => None,
        [only] => Some((only["result"].clone(), only["span"].clone())),
        more => panic!("one labelled call on {file}:{line}: {more:?}"),
    }
}

/// The return shape stored for the function opening on `file:line`.
fn shape_of(blob: &serde_json::Value, file: &str, line: u64) -> Option<serde_json::Value> {
    blob["call_through"]["return_shapes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["definition"]["file"] == file && entry["definition"]["line"] == line)
        .map(|entry| entry["shape"].clone())
}

/// The line the wrapper `name` of the client file opens on.
fn opens_on(name: &str) -> u64 {
    let source = std::fs::read_to_string(fixture_dir().join(CLIENT_FILE)).unwrap();
    source
        .lines()
        .position(|line| {
            line.starts_with(&format!("export async function {name}("))
                || line.starts_with(&format!("export function {name}("))
        })
        .unwrap_or_else(|| panic!("{name} is in the fixture")) as u64
        + 1
}

/// The line the first `needle` of a wrapper's body is on.
fn call_line(name: &str, needle: &str) -> u64 {
    let source = std::fs::read_to_string(fixture_dir().join(CLIENT_FILE)).unwrap();
    let start = opens_on(name) as usize - 1;
    source
        .lines()
        .enumerate()
        .skip(start)
        .find(|(_, line)| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle} is in {name}"))
        .0 as u64
        + 1
}

fn member(name: &str) -> serde_json::Value {
    serde_json::json!({ "member": [name] })
}

/// The labels the sidecar verified are stored with the line and the span of
/// the library call they belong to, in both services that walk to it.
#[test]
fn the_verified_labels_are_stored_at_the_library_calls_they_belong_to() {
    let scan = labelled();
    let client = &scan.blobs["client"];
    for (wrapper, needle, label) in [
        ("loadUser", "api.get<User>", member("data")),
        ("loadStatus", "api.get<User>", member("data")),
        (
            "createUser",
            "api.send<User>",
            serde_json::json!({ "whole": true }),
        ),
        ("loadPage", "api.get<UserPage>", member("data")),
        ("loadThen", "api.get<User>", member("data")),
    ] {
        let at = call_line(wrapper, needle);
        let (stored, _) = label_at(client, CLIENT_FILE, at)
            .unwrap_or_else(|| panic!("{wrapper}: {CLIENT_FILE}:{at} holds a label"));
        assert_eq!(stored, label, "{wrapper}");
    }
    // The call the store labels with nothing holds none.
    let ping = call_line("pingUsers", "api.ping");
    assert_eq!(label_at(client, CLIENT_FILE, ping), None);
    // Nothing is stored for a service that reaches no library call.
    assert!(scan.blobs["api"]["call_through"]["request_results"].is_null());
    // The CLI walks into the client's wrappers and finds the same calls.
    let cli = &scan.blobs["cli"];
    let at = call_line("loadUser", "api.get<User>");
    assert_eq!(
        label_at(cli, CLIENT_FILE, at).map(|(label, _)| label),
        Some(member("data"))
    );
}

/// Each wrapper's return shape names the member it reads and the span of the
/// call it returns; that span is the labelled call's own where the wrapper
/// returns that call, and another call's where the labelled call is only
/// inside it.
#[test]
fn a_wrappers_return_shape_names_its_member_and_the_call_it_returns() {
    let client = &labelled().blobs["client"];
    let stored = |wrapper: &str| {
        shape_of(client, CLIENT_FILE, opens_on(wrapper))
            .unwrap_or_else(|| panic!("{wrapper} has a return shape"))
    };
    let leaf_span = |wrapper: &str, needle: &str| {
        label_at(client, CLIENT_FILE, call_line(wrapper, needle))
            .unwrap_or_else(|| panic!("{wrapper} has a labelled call"))
            .1
    };

    // `reply.data`: the member, over the labelled call itself.
    let load = stored("loadUser");
    assert_eq!(load["kind"], "member");
    assert_eq!(load["member"], "data");
    assert_eq!(load["base"], leaf_span("loadUser", "api.get<User>"));

    // `.status`: another member of the same result.
    let status = stored("loadStatus");
    assert_eq!(status["kind"], "member");
    assert_eq!(status["member"], "status");
    assert_eq!(status["base"], leaf_span("loadStatus", "api.get<User>"));

    // The labelled call returned whole.
    let create = stored("createUser");
    assert_eq!(create["kind"], "value");
    assert_eq!(create["base"], leaf_span("createUser", "api.send<User>"));

    // `reply.data.items`: two reads, so no shape at all.
    assert_eq!(shape_of(client, CLIENT_FILE, opens_on("loadPage")), None);

    // `toModel((await get()).data)` and `Object.freeze(await send())`: the
    // call returned is another call, which only holds the labelled one.
    for (wrapper, needle) in [
        ("loadModel", "api.get<User>"),
        ("loadFrozen", "api.send<User>"),
    ] {
        let shape = stored(wrapper);
        assert_eq!(shape["kind"], "value", "{wrapper}");
        assert_ne!(shape["base"], leaf_span(wrapper, needle), "{wrapper}");
    }

    // `.then(..)` and `.catch(..)`: the value is run through a callback.
    for wrapper in ["loadThen", "loadCaught"] {
        assert_eq!(stored(wrapper)["derived"], true, "{wrapper}");
    }
    for wrapper in ["loadUser", "loadStatus", "createUser"] {
        assert!(stored(wrapper).get("derived").is_none(), "{wrapper}");
    }

    // `(await ping()).ok`: a member read over a call no label names.
    let ping = stored("pingUsers");
    assert_eq!(ping["member"], "ok");
}

/// The result checks are the only ask this adds, one per labelled claim a
/// call reaches; a store that labels nothing asks nothing, writes no label
/// and leaves the rows, the calls and the captured types as they were.
#[test]
fn a_store_without_result_labels_asks_nothing_and_changes_no_row_or_type() {
    let with = labelled();
    let without = unlabelled();
    // Two checks in the client (`get`, `send`), and the CLI's three: those
    // two and the hook, over every labelled call each walk reaches.
    for line in [
        "result shapes: verified results checks=2 not_reached=0 results=8",
        "result shapes: verified results checks=3 not_reached=0 results=9",
    ] {
        assert!(with.log.contains(line), "`{line}` in the run log");
    }
    assert!(
        !without.log.contains("result shapes: verified results"),
        "no label, no ask"
    );
    for service in ["api", "cli", "client"] {
        let (a, b) = (&with.blobs[service], &without.blobs[service]);
        assert!(b["call_through"]["request_results"].is_null(), "{service}");
        // The same rows and the same captured types: reading a label asks the
        // sidecar for no type.
        assert_eq!(a["calls"], b["calls"], "{service}: calls");
        assert_eq!(
            a["type_manifest"].as_array().map(Vec::len),
            b["type_manifest"].as_array().map(Vec::len),
            "{service}: type entries"
        );
        // The return shapes read are the same, label or no label.
        assert_eq!(
            a["call_through"]["return_shapes"], b["call_through"]["return_shapes"],
            "{service}: return shapes"
        );
    }
}
