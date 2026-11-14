//! Where the reader read a value is stored on its manifest entry
//! (carrick#4244 T1).
//!
//! Runs the real scanner binary over `tests/fixtures/site-places/` as the
//! action does (path `.`, root service), offline, with the compiler engine
//! live. The answer key `places.json` states, for each entry, the file and the
//! text of the node the entry's type was read from; the test finds that text
//! in the file, counts its `[start, end)` position in UTF-16 units (the unit
//! the compiler engine locates by), and checks the stored place equals it.
//! `src/client.ts` has a 4-byte character (two UTF-16 units) and two
//! 3-byte characters before its values, so a place counted in bytes or in
//! characters is wrong.

use std::path::PathBuf;
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site-places")
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

/// The UTF-16 code units `[start, end)` of `source`, as text.
fn utf16_slice(source: &str, start: u64, end: u64) -> String {
    let units: Vec<u16> = source.encode_utf16().collect();
    String::from_utf16(&units[start as usize..end as usize]).expect("a whole slice")
}

#[test]
fn site_places_fixture_stores_each_value_span() {
    let blob = scan_blob();
    let manifest = blob["type_manifest"].as_array().expect("type_manifest");
    let key: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture_dir().join("places.json")).expect("answer key"),
    )
    .expect("parse answer key");
    let mut failures = Vec::new();
    for row in key.as_array().expect("answer key rows") {
        let field = |name: &str| row[name].as_str().unwrap_or_default();
        let entry = manifest.iter().find(|entry| {
            entry["role"] == field("role")
                && entry["type_kind"] == field("type_kind")
                && entry["protocol"] == field("protocol")
                && (row["method"].is_null()
                    || entry["method"].as_str().unwrap_or_default() == field("method"))
                && entry["path"] == field("path")
        });
        let Some(entry) = entry else {
            failures.push(format!("no entry for {row}"));
            continue;
        };
        let source = std::fs::read_to_string(fixture_dir().join(field("file"))).expect("source");
        // The key's numbers were counted by hand; this proves they name the text.
        let (start, end) = (row["start"].as_u64().unwrap(), row["end"].as_u64().unwrap());
        assert_eq!(
            utf16_slice(&source, start, end),
            field("text"),
            "key row {row}"
        );
        let stored = (
            entry["evidence"]["span_start"].as_u64(),
            entry["evidence"]["span_end"].as_u64(),
        );
        if stored != (Some(start), Some(end)) {
            failures.push(format!(
                "{} {} {} {}: stored {stored:?}, answer key {:?}",
                field("role"),
                field("type_kind"),
                field("method"),
                field("path"),
                (start, end)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "misplaced entries:\n{}",
        failures.join("\n")
    );
}
