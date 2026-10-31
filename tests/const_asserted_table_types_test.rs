//! A call inside a const-asserted table keeps the type the compiler infers
//! for it (carrick#2590).
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/const-asserted-tool-table/` with the type sidecar LIVE (no
//! `CARRICK_ALLOW_MISSING_TYPES`). The fixture is a table of tools written as
//! an array literal closed by `as const`; each tool's handler is an arrow
//! that returns a client method's result. No line states the response type:
//! the client method's return type is inferred from the object it builds.
//!
//! `as const` states no type. Before the fix the type inference read the
//! const assertion around the whole table as the call's stated type and
//! printed its type node, `const`, as the response. Nothing declares a type
//! named `const`, so capture decayed the alias to `unknown` and the side
//! reached the index untyped.

use std::path::PathBuf;
use std::process::Command;

const ALIAS_FILE: &str = "src/tools.ts";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/const-asserted-tool-table")
}

/// The service's blob after one offline scan of the fixture.
fn scan_blob() -> serde_json::Value {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(fixture_dir())
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
        // Blobs only: a type-reach record sits beside each (carrick#3101).
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .next()
        .expect("one blob");
    let blob: serde_json::Value =
        serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
            .expect("parse blob");
    assert_eq!(blob["service_name"], "tool-table");
    blob
}

#[test]
fn a_call_in_a_const_asserted_table_keeps_its_inferred_response_type() {
    let blob = scan_blob();

    let entry = blob["type_manifest"]
        .as_array()
        .expect("type_manifest")
        .iter()
        .find(|e| {
            e["role"] == "consumer"
                && e["type_kind"] == "response"
                && e["file_path"]
                    .as_str()
                    .unwrap_or_default()
                    .ends_with(ALIAS_FILE)
        })
        .unwrap_or_else(|| {
            panic!(
                "no consumer response entry in {ALIAS_FILE}: {}",
                serde_json::to_string_pretty(&blob["type_manifest"]).unwrap()
            )
        });
    let alias = entry["type_alias"]
        .as_str()
        .expect("type_alias")
        .to_string();

    // The bundle carries the type the compiler infers for the call, not the
    // `const` of the assertion around the table.
    let bundled = blob["bundled_types"].as_str().expect("bundled_types");
    let bundled_line = bundled
        .lines()
        .find(|l| l.contains(&format!("type {alias} =")))
        .unwrap_or_else(|| panic!("no bundled line for {alias}:\n{bundled}"));
    assert!(
        !bundled_line.trim_end().ends_with("= const;"),
        "the const assertion was read as the call's type: {bundled_line}"
    );
    for member in [
        "installEnabled: boolean",
        "projectEnabled: boolean",
        "evaluator: string",
    ] {
        assert!(
            bundled_line.contains(member),
            "bundled type lost `{member}`: {bundled_line}"
        );
    }

    // Capture keeps it: the alias is not decayed to a top type.
    let files = &blob["capture_stub"]["files"];
    let manifest: serde_json::Value = serde_json::from_str(
        files["carrick-manifest.json"]
            .as_str()
            .expect("capture manifest"),
    )
    .expect("parse capture manifest");
    let captured = manifest["aliases"]
        .as_array()
        .expect("aliases")
        .iter()
        .find(|a| a["alias"] == alias.as_str())
        .unwrap_or_else(|| panic!("no capture row for {alias}: {manifest}"));
    assert_eq!(
        captured["self_check"], "ok",
        "capture decayed the call's type: {captured}"
    );
    let surface = files["types/surface.d.ts"].as_str().expect("surface stub");
    // The stub prints an object alias across lines: read up to the next
    // declaration.
    let start = surface
        .find(&format!("type {alias} ="))
        .unwrap_or_else(|| panic!("no stub declaration for {alias}:\n{surface}"));
    let declaration = &surface[start..];
    let declaration = declaration
        .find("\nexport ")
        .map_or(declaration, |end| &declaration[..end]);
    let declaration = declaration.split_whitespace().collect::<Vec<_>>().join(" ");
    for member in [
        "installEnabled: boolean",
        "projectEnabled: boolean",
        "evaluator: string",
    ] {
        assert!(
            declaration.contains(member),
            "stub alias lost `{member}`: {declaration}"
        );
    }
}
