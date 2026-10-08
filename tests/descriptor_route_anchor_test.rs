//! A route declared as data is never typed by its own descriptor
//! (carrick#2094).
//!
//! A route descriptor (`{ method, path, … }`, a direct element of an array)
//! states a method and a path and nothing that sends a body. Its row's span is
//! the object literal itself, so a locator that reads that span publishes the
//! descriptor (`{ method: string; path: string; … }`) as the route's response
//! contract, and every consumer paired onto the route is judged against it.
//!
//! The fixture holds the two shapes that declare routes as data:
//!
//! - `docs.ts`, a documentation array. It names no handler and its 2xx schema
//!   is a reference by name, which this layer cannot resolve. The route's
//!   response is unknown.
//! - `registry.ts`, a registry array whose descriptors name their handler.
//!   The type layer follows a handler that is an inline function or an
//!   identifier (declared here or imported) and reads what it returns. A
//!   handler read off a member is not followed, and that row states no type
//!   rather than the descriptor.
//!
//! Deleting the descriptor gate in `collect_type_requests` fails the
//! documentation and member-handler tests; widening it to every descriptor
//! fails the registry tests.

use std::path::PathBuf;
use std::process::Command;

const FIXTURE: &str = "descriptor-route-anchor";

/// Every file of the fixture. An uncovered file falls back to the generated
/// mock, which invents rows from the prompt, so each is answered with nothing:
/// the rows under test are the deterministic descriptor rows.
const FILES: [&str; 3] = ["docs", "handlers", "registry"];

const NOTHING: &str = r#"{"mounts":[],"endpoints":[],"data_calls":[]}"#;

fn scan() -> serde_json::Value {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(FIXTURE);
    let dir = tempfile::tempdir().expect("tempdir");
    let analyze = dir.path().join("analyze-file");
    std::fs::create_dir_all(&analyze).expect("create analyze-file dir");
    for stem in FILES {
        std::fs::write(analyze.join(format!("{stem}.json")), NOTHING).expect("write cassette");
    }

    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(fixture)
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
    serde_json::from_str(&stdout).expect("scanner output was not valid JSON")
}

/// The one row the run states at `file:line`.
fn row_at<'a>(projection: &'a serde_json::Value, file: &str, line: i64) -> &'a serde_json::Value {
    let endpoints = projection["endpoints"]
        .as_array()
        .expect("the projection carries an endpoints array");
    let found: Vec<&serde_json::Value> = endpoints
        .iter()
        .filter(|row| row["file"] == file && row["line"].as_i64() == Some(line))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "expected exactly one row at {file}:{line}, got {found:#?} out of {endpoints:#?}"
    );
    found[0]
}

/// The response type the run states for the row, expanded, or `<none>`.
fn response_of(row: &serde_json::Value) -> String {
    row["expanded_definition"]
        .as_str()
        .or_else(|| row["resolved_definition"].as_str())
        .unwrap_or("<none>")
        .to_string()
}

/// The row states no type: none at all, or the `unknown` placeholder.
fn assert_untyped(row: &serde_json::Value, why: &str) {
    let response = response_of(row);
    assert!(
        !response.contains("method"),
        "{why}: the descriptor literal was published as the response: {response}"
    );
    assert!(
        response == "<none>" || response == "unknown",
        "{why}: expected no type, got {response} on {row:#?}"
    );
}

#[test]
fn a_documentation_descriptor_publishes_no_type() {
    let projection = scan();
    let row = row_at(&projection, "src/docs.ts", 4);
    assert_eq!(row["method"], "GET");
    assert_eq!(row["path"], "/status");
    assert_untyped(
        row,
        "a documentation descriptor names no handler and its schema is a reference",
    );
}

#[test]
fn a_registry_descriptor_is_typed_at_its_handler() {
    let projection = scan();
    const ITEM: &str = "{ id: string; name: string; }";
    assert_eq!(
        response_of(row_at(&projection, "src/registry.ts", 21)),
        ITEM,
        "a declared function handler serves what it returns"
    );
    assert_eq!(
        response_of(row_at(&projection, "src/registry.ts", 22)),
        format!("{ITEM}[]"),
        "an arrow bound to a const serves what it returns"
    );
    assert_eq!(
        response_of(row_at(&projection, "src/registry.ts", 23)),
        "{ removed: boolean; }",
        "an imported handler serves what it returns"
    );
    assert_eq!(
        response_of(row_at(&projection, "src/registry.ts", 24)),
        ITEM,
        "an inline handler serves what it returns"
    );
}

#[test]
fn a_registry_descriptor_with_a_member_handler_publishes_no_type() {
    let projection = scan();
    let row = row_at(&projection, "src/registry.ts", 25);
    assert_eq!(row["path"], "/items/count");
    assert_untyped(
        row,
        "a handler read off a member is not followed, and the descriptor is no answer",
    );
}
