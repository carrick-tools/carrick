//! Cassette hard gate for a route the file layout states (carrick#1400).
//!
//! Every other file-layout fixture runs on an empty model answer, so no test
//! ran a model row through `join_model_rows` for a route with no registration
//! call. That is where carrick#1395 lived: the model has to echo a candidate
//! id, and every id such a file offers is a call its handler makes.
//!
//! `tests/fixtures/file-route-cassette/__llm__/` is one real analyzer run,
//! recorded by `scripts/record-cassette.sh` and never edited by hand. The run
//! replays it through the full binary with no network, then asserts two
//! things:
//!
//! - the invariants the join owes a file route, whatever the model said: one
//!   row per route, at the export's own line, stated by the file layout;
//! - the whole projection against the committed `__golden__.json`. The
//!   cassette is frozen, so any drift is a scanner change.
//!
//! If the golden fails after an intentional output change, re-record the
//! golden alone:
//! ```text
//! CARRICK_MOCK_ALL=1 CARRICK_OUTPUT_JSON=1 \
//!   CARRICK_MOCK_FIXTURE_DIR=tests/fixtures/file-route-cassette/__llm__/ \
//!   cargo run -- tests/fixtures/file-route-cassette \
//!   > tests/fixtures/file-route-cassette/__golden__.json
//! ```

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// The routes the fixture's layout states, with the line each is exported on.
const ROUTES: &[(&str, &str, &str, u64)] = &[
    ("GET", "/api/notes", "src/pages/api/notes.ts", 8),
    ("POST", "/api/notes", "src/pages/api/notes.ts", 13),
    ("PUT", "/api/notes/:id", "src/pages/api/notes/[id].ts", 6),
];

#[test]
fn cassette_hard_gate_file_route() {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = Path::new(repo).join("tests/fixtures/file-route-cassette");
    let cassette = fixture.join("__llm__");
    let golden_path = fixture.join("__golden__.json");
    assert!(
        cassette.join("analyze-file").is_dir() && golden_path.is_file(),
        "the cassette is not recorded yet: run `scripts/record-cassette.sh \
         tests/fixtures/file-route-cassette` (one real analyzer run, owner-approved \
         spend) and commit `__llm__/` and `__golden__.json`"
    );

    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(&fixture)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassette.display()),
        )
        .env("CARRICK_OUTPUT_JSON", "1")
        .output()
        .expect("failed to spawn carrick binary");
    assert!(
        output.status.success(),
        "scanner exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("scanner stdout was not UTF-8");
    assert!(
        !stdout.contains(&format!("{repo}/")),
        "scanner output leaked an absolute path anchored at the scan root:\n{stdout}"
    );
    let actual: serde_json::Value =
        serde_json::from_str(&stdout).expect("scanner output was not valid JSON");

    // One row per route, at the export, stated by the layout. A row the model
    // anchored on a body read or an outbound call would sit on a line inside
    // the handler; a row the join failed to fold would make a second one.
    let endpoints = actual["endpoints"]
        .as_array()
        .expect("the projection lists endpoints");
    let mut rows: BTreeMap<(String, String), Vec<&serde_json::Value>> = BTreeMap::new();
    for row in endpoints {
        let key = (
            row["method"].as_str().unwrap_or_default().to_string(),
            row["path"].as_str().unwrap_or_default().to_string(),
        );
        rows.entry(key).or_default().push(row);
    }
    assert_eq!(
        rows.len(),
        ROUTES.len(),
        "the layout states exactly {} routes, got {:?}",
        ROUTES.len(),
        rows.keys().collect::<Vec<_>>()
    );
    for (method, path, file, line) in ROUTES {
        let found = rows
            .get(&(method.to_string(), path.to_string()))
            .unwrap_or_else(|| panic!("no row for {method} {path}"));
        assert_eq!(found.len(), 1, "{method} {path} has {} rows", found.len());
        let row = found[0];
        assert_eq!(row["file"], *file, "{method} {path}");
        assert_eq!(
            row["line"], *line,
            "{method} {path} must sit on its export, not on a call inside it"
        );
        assert_eq!(
            row["resolution_source"], "file_based_route",
            "{method} {path}"
        );
    }

    let golden: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&golden_path).expect("golden is readable"))
            .expect("golden fixture was not valid JSON");
    assert_eq!(
        actual, golden,
        "Full-pipeline output drifted from the golden while the model answer is \
         frozen, so the drift is a scanner change."
    );
}

/// Scan the fixture with the storage settings `scripts/record-cassette.sh`
/// records under, the analyzer mocked, and return what landed in the storage
/// directory. Every upload path writes that directory (TeeStorage writes its
/// local copy before the cloud one), so an empty directory is a run that
/// uploaded nothing.
fn storage_after_scan(output_json: bool) -> Vec<String> {
    let repo = env!("CARGO_MANIFEST_DIR");
    let store = tempfile::tempdir().expect("temp dir");
    let mut command = Command::new(env!("CARGO_BIN_EXE_carrick"));
    command
        .arg(Path::new(repo).join("tests/fixtures/file-route-cassette"))
        .arg("--no-cache")
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_LOCAL_STORAGE_DIR", store.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_SKIP_UPLOAD", "1")
        .env_remove("CARRICK_OUTPUT_JSON");
    if output_json {
        command.env("CARRICK_OUTPUT_JSON", "1");
    }
    let output = command.output().expect("failed to spawn carrick binary");
    assert!(
        output.status.success(),
        "scanner exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut written: Vec<String> = std::fs::read_dir(store.path())
        .expect("read the storage dir")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    written.sort();
    written
}

/// The recording run must upload nothing: `CARRICK_OUTPUT_JSON` ends the run
/// at the JSON projection, before any upload.
#[test]
fn the_recording_settings_upload_nothing() {
    let written = storage_after_scan(true);
    assert!(written.is_empty(), "the recording run wrote {written:?}");
}

/// The control that makes the assertion above mean something: without
/// `CARRICK_OUTPUT_JSON` the same run writes its index into the directory.
#[test]
fn the_upload_tripwire_sees_an_upload() {
    let written = storage_after_scan(false);
    assert!(
        written.iter().any(|name| name.ends_with(".json")),
        "a run that uploads must leave its index in the storage dir, got {written:?}"
    );
}
