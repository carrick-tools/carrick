//! carrick#2100 through the whole scanner, mocked ($0, no model).
//!
//! `tests/fixtures/unwidened-budget`: `api` answers `GET /job` with an
//! unannotated `{ status: "ready", attempts: 1 }`, which the compiler widens
//! to `status: string`; `web` expects `status: "ready"`. The sidecar's
//! unwidened reading recovers the literal, and the pair is compatible.
//!
//! When that reading is not made (past the sidecar's work cap, carrick#2629;
//! forced here with `CARRICK_UNWIDENED_READINGS_CAP=0`), the pair must read
//! unverifiable, never incompatible from the widened type, and the scan must
//! count the reading it did not make on the producer's health.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unwidened-budget")
}

fn sidecar_built() -> bool {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/sidecar/dist/src/index.js")
        .exists()
}

/// What one scan stored and logged.
struct Scan {
    log: String,
    storage: tempfile::TempDir,
}

/// Scan `root` with the cassettes in its `__llm__` and `env` set, as the
/// Action runs the binary, storing each service's index blob locally.
fn scan(root: &Path, env: &[(&str, &str)]) -> Scan {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(root)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", root.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env_remove("CARRICK_UNWIDENED_READINGS_CAP");
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
    cmd.envs(env.iter().copied());
    let output = cmd.output().expect("failed to spawn carrick");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "scan failed:\n{stderr}");
    Scan {
        log: format!("{stdout}\n{stderr}"),
        storage,
    }
}

/// The index blob the scan stored for `service`.
fn blob(scan: &Scan, service: &str) -> serde_json::Value {
    let path = scan
        .storage
        .path()
        .join(format!("unwidened-budget__{service}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the scan stored no blob for {service}: {e}"));
    serde_json::from_str(&text).expect("the blob is JSON")
}

/// The verdict the consumer's blob stores on the `GET /job` response.
fn job_response_verdict(scan: &Scan) -> serde_json::Value {
    let web = blob(scan, "web");
    let rows: Vec<&serde_json::Value> = web["compat_verdicts"]
        .as_array()
        .expect("the call is type-checked, so a verdict row is stored")
        .iter()
        .filter(|row| row["producer_key"].as_str() == Some("http|GET|/job"))
        .collect();
    assert_eq!(rows.len(), 1, "one verdict row for the call: {rows:#?}");
    rows[0]["response"].clone()
}

/// The `unwidened_reading` count on the producer's stored boundary, and the
/// `unwidened_out_of_budget` mark on its response entry.
fn producer_health(scan: &Scan) -> (Option<u64>, Option<bool>) {
    let api = blob(scan, "api");
    let counted = api["boundary"]["time_limits_run_out"]["unwidened_reading"].as_u64();
    let marked = api["type_manifest"]
        .as_array()
        .expect("the producer's manifest")
        .iter()
        .find(|e| e["role"] == "producer" && e["type_kind"] == "response")
        .map(|e| e["unwidened_out_of_budget"].as_bool().unwrap_or(false));
    (counted, marked)
}

#[test]
fn a_reading_that_ran_out_of_budget_leaves_the_pair_unverifiable() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let spent = scan(&fixture_dir(), &[("CARRICK_UNWIDENED_READINGS_CAP", "0")]);
    let response = job_response_verdict(&spent);
    assert_eq!(
        response["verdict"], "unverifiable",
        "a mismatch judged on the widened type is not published: {response:#}"
    );
    assert!(
        response["unresolved_reason"]
            .as_str()
            .is_some_and(|why| why.contains("reading them ran out of time")),
        "{response:#}"
    );
    assert!(
        spent.log.lines().any(|l| l.contains("Types not verified:")
            && l.contains("GET /job response")
            && l.contains("reading them ran out of time")),
        "the log says why the pair is not verified:\n{}",
        spent.log
    );
    assert_eq!(
        producer_health(&spent),
        (Some(1), Some(true)),
        "the producer's health counts the reading it did not make"
    );
}

#[test]
fn a_reading_made_in_time_reads_the_pair_compatible() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let read = scan(&fixture_dir(), &[]);
    let response = job_response_verdict(&read);
    assert_eq!(response["verdict"], "compatible", "{response:#}");
    assert_eq!(producer_health(&read), (None, Some(false)));
}
