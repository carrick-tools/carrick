//! A wrapper whose request line leaves the path open to its callers, called
//! where a module loads (carrick#4121). The fact layer states each such call
//! as a row of its own; once every call holding the request line open has a
//! fact row that stands, the model's row at the request line is the same
//! request stated again, and it goes. A call the fact layer does not decide,
//! a call in a constructor or a callback, and two such calls on one line
//! keep the model's row.
//!
//! Drives the real scanner binary on `.`, as the Action runs it, over a
//! committed copy of `tests/fixtures/wrapper-request-line-module/repo` (one
//! root service), offline, with the fixture's store answer replayed as the
//! library store's and one model answer per wrapper file.
//!
//! Protocols: HTTP only. A socket emit or a topic publish through a wrapper
//! whose event or topic is open is read by the fact layer, not here: a gap.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wrapper-request-line-module")
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

/// A wrapper called only where a module loads (`sendWarm`): its one call
/// states the request as a fact row, and the request line states no row.
#[test]
fn a_wrapper_called_only_at_module_level_states_the_request_at_its_call() {
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
        "a request line restates its module-level callers' requests:\n{}\n{rows:#?}",
        restated.join("\n")
    );
    for (at, want) in key()["caller_rows"].as_object().unwrap() {
        let stated: Vec<String> = rows
            .get(at)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, source)| source != "model")
            .map(|(text, _)| text)
            .collect();
        assert_eq!(stated, [want.as_str().unwrap()], "{at}: {rows:#?}");
    }
}

/// A module-level call whose argument the fact layer cannot decide makes a
/// lead, not a fact row: the request line keeps the model's row.
#[test]
fn an_undecided_module_level_call_keeps_the_request_line() {
    keeps("src/net/arg.ts:4");
}

/// A wrapper also called from a constructor has a caller no row states.
#[test]
fn a_call_in_a_constructor_keeps_the_request_line() {
    keeps("src/net/boot.ts:4");
}

/// A call in a callback written where a module loads is not one the scan
/// reads as a module-level call.
#[test]
fn a_call_in_a_module_level_callback_keeps_the_request_line() {
    keeps("src/net/timer.ts:4");
}

/// Two module-level calls of two wrappers on one line: no one row there is
/// either request's.
#[test]
fn two_module_level_calls_on_one_line_keep_their_request_lines() {
    keeps("src/net/left.ts:4");
    keeps("src/net/right.ts:4");
}
