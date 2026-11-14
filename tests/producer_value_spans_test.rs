//! A producer entry stores each value its type was read from
//! (carrick#4244 T3b).
//!
//! Runs the real scanner binary over `tests/fixtures/producer-value-spans/` as
//! the action does (path `.`, root service), offline, with the compiler engine
//! live. The answer key `value_spans.json` states, for each producer entry,
//! the file and text of every value the published type was built from, and
//! its `[start, end)` in UTF-16 units (the unit the compiler engine locates
//! by). `src/routes.ts` has non-ASCII text, including a 4-byte character,
//! before its values. The entry whose response type is stated by the model
//! stores no value spans. For an imported handler, every span sits inside the
//! handler the route row places (`handler_file` and `handler_span`).

use std::path::PathBuf;
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/producer-value-spans")
}

fn scan_blob() -> serde_json::Value {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(".")
        .current_dir(fixture_dir())
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/__llm__/", fixture_dir().display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1");
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
        "CARRICK_ALLOW_MISSING_TYPES",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("failed to spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let entry = std::fs::read_dir(storage.path())
        .expect("storage dir")
        .flatten()
        .find(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .expect("one blob");
    serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob")).expect("parse blob")
}

fn utf16_slice(source: &str, start: u64, end: u64) -> String {
    let units: Vec<u16> = source.encode_utf16().collect();
    String::from_utf16(&units[start as usize..end as usize]).expect("a whole slice")
}

fn answer_key() -> Vec<serde_json::Value> {
    serde_json::from_slice(
        &std::fs::read(fixture_dir().join("value_spans.json")).expect("answer key"),
    )
    .expect("parse answer key")
}

fn find_entry<'a>(
    manifest: &'a [serde_json::Value],
    row: &serde_json::Value,
) -> Option<&'a serde_json::Value> {
    manifest.iter().find(|entry| {
        entry["role"] == row["role"]
            && entry["type_kind"] == row["type_kind"]
            && entry["method"] == row["method"]
            && entry["path"] == row["name"]
    })
}

#[test]
fn producer_value_spans_fixture_stores_each_value_span() {
    let blob = scan_blob();
    let manifest = blob["type_manifest"].as_array().expect("type_manifest");
    let mut failures = Vec::new();
    for row in answer_key() {
        let Some(entry) = find_entry(manifest, &row) else {
            failures.push(format!("no entry for {row}"));
            continue;
        };
        let mut expected = Vec::new();
        for value in row["values"].as_array().expect("values") {
            let file = value["file"].as_str().unwrap();
            let source = std::fs::read_to_string(fixture_dir().join(file)).expect("source");
            let (start, end) = (
                value["start"].as_u64().unwrap(),
                value["end"].as_u64().unwrap(),
            );
            assert_eq!(
                utf16_slice(&source, start, end),
                value["text"].as_str().unwrap(),
                "key row {value}"
            );
            expected.push((file.to_string(), start, end));
        }
        let mut stored: Vec<(String, u64, u64)> = entry["evidence"]["value_spans"]
            .as_array()
            .map(|spans| {
                spans
                    .iter()
                    .map(|span| {
                        (
                            span["file_path"].as_str().unwrap_or_default().to_string(),
                            span["span_start"].as_u64().unwrap_or(u64::MAX),
                            span["span_end"].as_u64().unwrap_or(u64::MAX),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        stored.sort();
        expected.sort();
        if stored != expected {
            failures.push(format!(
                "{} {} {}: stored {stored:?}, answer key {expected:?}",
                row["type_kind"], row["method"], row["name"]
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "misplaced entries:\n{}",
        failures.join("\n")
    );
}

/// carrick#4244 R15: the values of a route whose handler is declared in
/// another file sit inside the handler the route row places.
#[test]
fn value_spans_sit_inside_the_handler_t4_places() {
    let blob = scan_blob();
    let manifest = blob["type_manifest"].as_array().expect("type_manifest");
    let row = serde_json::json!({
        "role": "producer", "type_kind": "response", "method": "GET", "name": "/imported"
    });
    let entry = find_entry(manifest, &row).expect("the imported route's response entry");
    let spans = entry["evidence"]["value_spans"]
        .as_array()
        .expect("value spans");
    assert!(!spans.is_empty(), "the imported handler's value is stored");
    let route = blob["mount_graph"]["endpoints"]
        .as_array()
        .expect("endpoints")
        .iter()
        .find(|e| e["path"] == "/imported")
        .expect("the imported route");
    let handler_file = route["handler_file"].as_str().expect("handler_file");
    let (from, to) = (
        route["handler_span"]["start_line"].as_u64().expect("start"),
        route["handler_span"]["end_line"].as_u64().expect("end"),
    );
    let source = std::fs::read_to_string(fixture_dir().join(handler_file)).expect("handler file");
    for span in spans {
        assert_eq!(span["file_path"], handler_file);
        let line_of =
            |offset: u64| utf16_slice(&source, 0, offset).matches('\n').count() as u64 + 1;
        let (a, b) = (
            line_of(span["span_start"].as_u64().unwrap()),
            line_of(span["span_end"].as_u64().unwrap()),
        );
        assert!(
            from <= a && b <= to,
            "span lines {a}-{b} outside {from}-{to}"
        );
    }
}
