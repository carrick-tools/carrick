//! carrick#1990: `exclude` in carrick.json leaves paths out of the scan.
//!
//! Drives the real scanner binary over `tests/fixtures/exclude-patterns/` with
//! the model off and reads the blob it writes. The fixture's README is the
//! answer key.

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

struct Scan {
    blob: serde_json::Value,
    output: String,
}

fn scan() -> &'static Scan {
    static SCAN: OnceLock<Scan> = OnceLock::new();
    SCAN.get_or_init(|| {
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/exclude-patterns");
        let storage = tempfile::tempdir().expect("storage dir");
        let cache = tempfile::tempdir().expect("cache dir");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
        cmd.arg(&fixture)
            .env("CARRICK_MOCK_ALL", "1")
            .env("CARRICK_NO_MODEL", "1")
            .env("CARRICK_SKIP_INTENTS", "1")
            .env("CARRICK_CACHE_DIR", cache.path())
            .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
            .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1");
        for var in [
            "GITHUB_REPOSITORY",
            "GITHUB_ACTIONS",
            "CI",
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
        ] {
            cmd.env_remove(var);
        }
        let result = cmd.output().expect("failed to spawn carrick");
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.status.success(), "scan exited non-zero:\n{output}");
        let blob = std::fs::read_dir(storage.path())
            .expect("storage dir")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .map(|path| {
                serde_json::from_slice::<serde_json::Value>(&std::fs::read(path).expect("blob"))
                    .expect("blob is JSON")
            })
            .find(|blob| blob.get("function_definitions").is_some())
            .unwrap_or_else(|| panic!("no blob written:\n{output}"));
        Scan { blob, output }
    })
}

fn keys(side: &str) -> Vec<String> {
    let mut keys: Vec<String> = scan().blob[side]
        .as_array()
        .unwrap_or_else(|| panic!("blob has {side}"))
        .iter()
        .map(|row| {
            format!(
                "{} {}",
                row["key"]["method"].as_str().unwrap_or_default(),
                row["key"]["path"].as_str().unwrap_or_default()
            )
        })
        .collect();
    keys.sort();
    keys
}

#[test]
fn an_excluded_file_states_no_function_route_or_call() {
    let functions = scan().blob["function_definitions"]
        .as_object()
        .expect("blob has function_definitions");
    let mut names: Vec<&str> = functions.keys().map(String::as_str).collect();
    names.sort();
    assert_eq!(
        names,
        ["GET", "firstOrder", "loadOrders"],
        "{}",
        scan().output
    );
    assert_eq!(keys("endpoints"), ["GET /api/orders"]);
    assert_eq!(keys("calls"), ["GET /api/orders"]);
}

#[test]
fn a_file_importing_an_excluded_module_is_still_typed() {
    assert_eq!(
        scan().blob["function_definitions"]["firstOrder"]["signature"].as_str(),
        Some("(rows: OrderRow[]) => OrderRow"),
        "{}",
        scan().output
    );
}

#[test]
fn the_scan_says_what_it_excluded_and_the_blob_states_the_patterns() {
    assert!(
        scan()
            .output
            .contains("Left out 4 file(s) matching the 3 exclude pattern(s) in carrick.json."),
        "{}",
        scan().output
    );
    let config: serde_json::Value = serde_json::from_str(
        scan().blob["config_json"]
            .as_str()
            .expect("blob has config_json"),
    )
    .expect("config_json is JSON");
    assert_eq!(
        config["exclude"],
        serde_json::json!(["scripts/", "app/api/legacy/", "**/*.scratch.ts"])
    );
}
