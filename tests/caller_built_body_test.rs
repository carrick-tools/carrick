//! A caller of an in-repo method that builds its own request body takes the
//! body it sends (carrick#4260).
//!
//! Drives the real scanner binary offline over `tests/fixtures/caller-built-body/`
//! with the compiler engine LIVE, as the Action runs a scan. The fixture's
//! method sends through the platform `fetch`, so the fact layer decides each
//! send at the method; that site yields to the row the request summaries
//! state at the caller, and its body slot types the row, read through the
//! caller's own call. Answer key: `tests/fixtures/caller-built-body/README.md`.
//! Protocols: HTTP request bodies only.

use std::path::PathBuf;
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/caller-built-body")
}

/// The blob one offline scan of the fixture writes.
fn scan() -> serde_json::Value {
    let repo = fixture_dir();
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(&repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env_remove("CARRICK_ALLOW_MISSING_TYPES");
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
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("failed to spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut blobs: Vec<serde_json::Value> = std::fs::read_dir(storage.path())
        .expect("storage dir")
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .map(|entry| {
            serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
                .expect("parse blob")
        })
        .collect();
    assert_eq!(blobs.len(), 1, "one service, one blob");
    blobs.remove(0)
}

/// The consumer request entries at `src/caller.ts:line`.
fn requests_at(blob: &serde_json::Value, line: u64) -> Vec<&serde_json::Value> {
    blob["type_manifest"]
        .as_array()
        .expect("type_manifest")
        .iter()
        .filter(|entry| {
            entry["role"] == "consumer"
                && entry["type_kind"] == "request"
                && entry["file_path"] == "src/caller.ts"
                && entry["line_number"] == line
        })
        .collect()
}

/// What the capture stub says the request at `line` is, across lines.
fn request_type(blob: &serde_json::Value, line: u64) -> String {
    let entries = requests_at(blob, line);
    assert_eq!(
        entries.len(),
        1,
        "one request entry at :{line}: {entries:#?}"
    );
    let alias = entries[0]["type_alias"].as_str().expect("alias");
    let head = format!("export type {alias} = ");
    let files = blob["capture_stub"]["files"]
        .as_object()
        .expect("a capture stub: the compiler engine ran");
    for text in files.values().filter_map(|text| text.as_str()) {
        if let Some(start) = text.find(&head) {
            let rest = &text[start + head.len()..];
            let end = rest.find("\nexport ").unwrap_or(rest.len());
            return rest[..end].split_whitespace().collect::<Vec<_>>().join(" ");
        }
    }
    panic!("no `{head}` in the stub: {files:#?}");
}

#[test]
fn a_caller_of_a_method_that_builds_its_body_takes_the_body_it_sends() {
    let blob = scan();
    assert_eq!(
        request_type(&blob, 5),
        "{ widget: { label: string; size: number; }; source: \"cli\"; };"
    );
}

#[test]
fn a_generic_method_s_built_body_is_read_as_its_caller_instantiates_it() {
    let blob = scan();
    let text = request_type(&blob, 8);
    assert_eq!(text, "{ item: { title: string; }; };");
    assert!(!text.contains("item: T"), "{text}");
}

#[test]
fn a_caller_of_a_method_that_builds_an_inline_body_takes_it() {
    let blob = scan();
    assert_eq!(request_type(&blob, 9), "{ name: string; };");
}

#[test]
fn a_caller_of_a_method_that_sends_its_argument_keeps_its_type() {
    let blob = scan();
    assert_eq!(request_type(&blob, 11), "{ label: string; size: number; };");
}

#[test]
fn a_caller_of_a_method_that_sends_no_body_states_no_request() {
    let blob = scan();
    assert!(
        requests_at(&blob, 10).is_empty(),
        "{:#?}",
        requests_at(&blob, 10)
    );
}
