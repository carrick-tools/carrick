//! carrick#2591 through the whole scanner, mocked ($0, no model).
//!
//! `tests/fixtures/open-member-any`: `api` answers `POST /checkout` with a
//! `Checkout { id: string; meta: any }`. `web` reads `id` as a number (a
//! closed member: a real mismatch) and `meta.x` (the member the author
//! declared `any`: open, never a mismatch). Before the fix the producer's
//! whole side read unknown, so neither read was judged.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/open-member-any")
}

/// Scan `root` with the cassettes in its `__llm__`; returns the projection
/// and the scanner's combined log.
fn scan(root: &Path) -> (serde_json::Value, String) {
    scan_with(root, &[])
}

fn scan_with(root: &Path, env: &[(&str, &str)]) -> (serde_json::Value, String) {
    let cache = tempfile::tempdir().expect("temp cache dir");
    let cassettes = root.join("__llm__");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.envs(env.iter().copied());
    cmd.arg(root)
        .env("CARRICK_LOCAL_STORAGE_DIR", cache.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.display()),
        )
        .env("CARRICK_OUTPUT_JSON", "1");
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
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "scan failed:\n{stderr}");
    let start = stdout
        .find("\n{")
        .map(|i| i + 1)
        .or_else(|| stdout.starts_with('{').then_some(0))
        .unwrap_or_else(|| panic!("no JSON in scanner stdout:\n{stdout}"));
    let projection = serde_json::from_str(&stdout[start..])
        .unwrap_or_else(|e| panic!("projection parse failed: {e}\n{stdout}"));
    (projection, format!("{stdout}\n{stderr}"))
}

fn checkout_matches(projection: &serde_json::Value) -> Vec<&serde_json::Value> {
    projection["cross_repo_matches"]
        .as_array()
        .expect("projection has no `cross_repo_matches` array")
        .iter()
        .filter(|m| m["producer_key"].as_str() == Some("http|POST|/checkout"))
        .collect()
}

fn sidecar_built() -> bool {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/sidecar/dist/src/index.js")
        .exists()
}

#[test]
fn a_closed_member_is_judged_beside_a_declared_open_one() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let (projection, log) = scan(&fixture_dir());
    let matches = checkout_matches(&projection);
    assert_eq!(matches.len(), 2, "both calls match the route: {matches:#?}");
    let flagged: Vec<&&serde_json::Value> = matches
        .iter()
        .filter(|m| m["type_compatible"] == false)
        .collect();
    assert_eq!(
        flagged.len(),
        1,
        "only the read of `id` is flagged:\n{matches:#?}\n{log}"
    );
    let reason = flagged[0]["mismatch_reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("'id' is string on the producer and number on the consumer"),
        "the flagged read is `id`, and the open member is not named: {reason}"
    );
    assert!(!reason.contains("meta"), "{reason}");
    let open = matches
        .iter()
        .find(|m| m["mismatch_reason"].is_null())
        .expect("the read of the open member is not a mismatch");
    assert_ne!(open["type_compatible"], false, "{open:#}");
    // Both responses were judged: neither is logged as unverified.
    assert!(
        !log.contains("Types not verified: POST /checkout response"),
        "the producer's open member leaves both pairs judged:\n{log}"
    );
}

/// The producer's side is served as the type it states, with `meta` the one
/// open field, and not as `unknown` (carrick#2591).
#[test]
fn a_side_with_one_declared_any_member_is_served_as_its_shape() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
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
        "CARRICK_OUTPUT_JSON",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("failed to spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let served: Vec<(String, Option<String>)> = std::fs::read_dir(storage.path())
        .expect("storage dir")
        .flatten()
        // Blobs only: a type-reach record sits beside each (carrick#3101).
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| {
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(entry.path()).ok()?).ok()
        })
        .filter(|blob| blob["service_name"] == "api")
        .flat_map(|blob| {
            blob["type_manifest"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter(|entry| entry["role"] == "producer" && entry["type_kind"] == "response")
        .map(|entry| {
            (
                entry["type_state"].as_str().unwrap_or_default().to_string(),
                entry["expanded_definition"].as_str().map(str::to_string),
            )
        })
        .collect();
    assert_eq!(served.len(), 1, "one producer response entry: {served:?}");
    assert_ne!(served[0].0, "unknown", "served as its shape: {served:?}");
    let shape = served[0].1.as_deref().unwrap_or_default();
    assert!(
        shape.contains("id: string") && shape.contains("meta: any"),
        "the resolved shape is served, with `meta` open: {shape}"
    );
}
