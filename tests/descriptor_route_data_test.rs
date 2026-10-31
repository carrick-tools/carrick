//! A table of `{ method, path }` records is routes only when it declares them
//! (carrick#3082).
//!
//! The fixture holds six files:
//!
//! - `registry.ts`: a route table whose records name their handler. A row per
//!   record.
//! - `openapi.ts`: a module-local documentation table, handler-less, every
//!   record stating its responses. A row per record.
//! - `fixture-data.js`: records of bookkeeping (a line number, a label) and a
//!   hash input. No rows.
//! - `scripts/smoke.ts`: a list of calls to make; one record carries an
//!   object, the table does not. No rows.
//! - `selftest.mjs`: an expected result built inside a test callback. No rows.
//! - `client.ts`: one `fetch`, so the model is asked about this file. It is
//!   the known positive for the model-input guard.
//!
//! Deleting the contract clause fails the data-row test; dropping the data
//! spans from the candidate filter fails the model-input guard.

use std::path::PathBuf;
use std::process::Command;

const FIXTURE: &str = "descriptor-route-data";

/// Every file of the fixture. An uncovered file falls back to the generated
/// mock, which invents rows from the prompt, so each is answered with nothing.
const FILES: [&str; 6] = [
    "registry",
    "openapi",
    "fixture-data",
    "smoke",
    "selftest",
    "client",
];

const NOTHING: &str = r#"{"mounts":[],"endpoints":[],"data_calls":[]}"#;

fn scan(verbose: bool) -> (serde_json::Value, String) {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(FIXTURE);
    let dir = tempfile::tempdir().expect("tempdir");
    let analyze = dir.path().join("analyze-file");
    std::fs::create_dir_all(&analyze).expect("create analyze-file dir");
    for stem in FILES {
        std::fs::write(analyze.join(format!("{stem}.json")), NOTHING).expect("write cassette");
    }

    let mut command = Command::new(env!("CARGO_BIN_EXE_carrick"));
    command.arg(fixture);
    if verbose {
        command.arg("--verbose");
    }
    let output = command
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", dir.path().display()),
        )
        .env("CARRICK_OUTPUT_JSON", "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env_remove("GITHUB_REPOSITORY")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("CI")
        .output()
        .expect("failed to spawn carrick binary");
    assert!(
        output.status.success(),
        "scanner exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("scanner stdout was not UTF-8");
    let projection = serde_json::from_str(&stdout).expect("scanner output was not valid JSON");
    (
        projection,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// The lines of `file` that hold a row.
fn lines_of(projection: &serde_json::Value, file: &str) -> Vec<i64> {
    let mut lines: Vec<i64> = projection["endpoints"]
        .as_array()
        .expect("the projection carries an endpoints array")
        .iter()
        .filter(|row| row["file"] == file)
        .filter_map(|row| row["line"].as_i64())
        .collect();
    lines.sort_unstable();
    lines
}

#[test]
fn a_table_that_declares_routes_keeps_a_row_per_record() {
    let (projection, _) = scan(false);
    assert_eq!(
        lines_of(&projection, "src/registry.ts"),
        vec![5, 6, 7],
        "records that name a handler, by identifier, inline or by string"
    );
    assert_eq!(
        lines_of(&projection, "src/openapi.ts"),
        vec![4, 9, 15],
        "records that state their responses, in a table no export reaches"
    );
}

#[test]
fn a_table_of_data_records_has_no_rows() {
    let (projection, _) = scan(false);
    for file in [
        "src/fixture-data.js",
        "scripts/smoke.ts",
        "src/selftest.mjs",
    ] {
        assert_eq!(
            lines_of(&projection, file),
            Vec::<i64>::new(),
            "{file} holds records, not routes"
        );
    }
}

/// A table read as data stays off the model: the file is not dispatched, so
/// the model is asked nothing it was not asked before (the known positive is
/// the file that does call `fetch`).
#[test]
fn a_table_read_as_data_adds_no_model_ask() {
    let (_, stderr) = scan(true);
    let hits: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("Mock fixture hit"))
        .collect();
    assert!(
        hits.iter().any(|line| line.contains("client.json")),
        "a dispatched file looks up its cassette: {hits:#?}"
    );
    for stem in ["fixture-data", "smoke", "selftest"] {
        assert!(
            !hits.iter().any(|line| line.contains(stem)),
            "{stem} must not reach the model: {hits:#?}"
        );
    }
}
