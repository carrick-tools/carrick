//! carrick#4238 through the whole scanner, mocked ($0, no model), run as
//! `action.yml` runs it (path `.`, root service): a model route row written
//! inside the body of a handler another file registers is withdrawn, and the
//! registration stays. A row a file states for its own registration, and two
//! real registrations of one handler, stay.
//!
//! `tests/fixtures/handler-file-route-row` is one service; see its `README.md`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/handler-file-route-row")
}

/// Every index blob the scan stored.
fn blobs() -> &'static Vec<serde_json::Value> {
    static BLOBS: OnceLock<Vec<serde_json::Value>> = OnceLock::new();
    BLOBS.get_or_init(|| {
        let root = fixture();
        let cache = tempfile::tempdir().expect("temp cache dir");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
        cmd.current_dir(&root)
            .arg(".")
            .env("CARRICK_MOCK_ALL", "1")
            .env(
                "CARRICK_MOCK_FIXTURE_DIR",
                format!("{}/", root.join("__llm__").display()),
            )
            .env("CARRICK_SKIP_INTENTS", "1")
            .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
            .env("CARRICK_LOCAL_STORAGE_DIR", cache.path());
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
            "scan failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::read_dir(cache.path())
            .expect("read the storage dir")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .map(|path| {
                serde_json::from_str(&std::fs::read_to_string(path).expect("read a blob"))
                    .expect("a blob is JSON")
            })
            .collect()
    })
}

type Row = (String, String, String);

/// `(file:line, method, full path)` of every mount-graph route row stored.
fn rows() -> BTreeSet<Row> {
    let field = |row: &serde_json::Value, name: &str| {
        row[name]
            .as_str()
            .unwrap_or_else(|| panic!("row has no {name}: {row}"))
            .to_string()
    };
    blobs()
        .iter()
        .flat_map(|blob| {
            blob["mount_graph"]["endpoints"]
                .as_array()
                .into_iter()
                .flatten()
        })
        .map(|row| {
            (
                field(row, "file_location"),
                field(row, "method"),
                field(row, "full_path"),
            )
        })
        .collect()
}

fn answer_key() -> serde_json::Value {
    serde_json::from_str(
        &std::fs::read_to_string(fixture().join("expected.json")).expect("read the answer key"),
    )
    .expect("the answer key is JSON")
}

fn key_rows(name: &str) -> BTreeSet<Row> {
    answer_key()[name]
        .as_array()
        .unwrap_or_else(|| panic!("the answer key has no {name}"))
        .iter()
        .map(|row| {
            let field = |key: &str| row[key].as_str().expect("a string").to_string();
            (field("file_location"), field("method"), field("full_path"))
        })
        .collect()
}

#[test]
fn a_model_row_at_a_handler_another_file_registers_is_withdrawn() {
    assert_eq!(rows(), key_rows("routes"));
}

/// No route row, projected endpoint or type ask names a withdrawn row's
/// place. The mount graph's owner nodes are left out: the model named each
/// handler as its row's owner, and a node owns no route by itself.
#[test]
fn the_withdrawn_rows_leave_nothing_behind_in_the_blob() {
    let blob = blobs()
        .iter()
        .map(|blob| {
            let mut blob = blob.clone();
            if let Some(graph) = blob["mount_graph"].as_object_mut() {
                graph.remove("nodes");
            }
            blob.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    for place in ["src/controllers/v1.ts:4", "src/controllers/items.ts:4"] {
        assert!(
            !blob.contains(place),
            "the withdrawn row at {place} is still named in the blob"
        );
    }
}

/// Both withdrawn rows carry a response anchor in their answers, so each
/// would be asked a type. Withdrawn, neither leaves a type-manifest entry.
#[test]
fn the_withdrawn_rows_leave_no_type_ask() {
    let asked: Vec<String> = blobs()
        .iter()
        .flat_map(|blob| blob["type_manifest"].as_array().into_iter().flatten())
        .filter(|entry| {
            matches!(
                entry["file_path"].as_str(),
                Some("src/controllers/v1.ts" | "src/controllers/items.ts")
            )
        })
        .map(|entry| entry.to_string())
        .collect();
    assert!(asked.is_empty(), "type asks left behind: {asked:?}");
}
