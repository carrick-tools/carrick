//! carrick#4186 through the whole scanner, mocked ($0, no model): a catch-all
//! route whose wildcard covers a literal segment of the call keeps its edge,
//! and the type check judges nothing there. A concrete route's call in the
//! same file keeps its real verdict.
//!
//! `tests/fixtures/catch-all-type-pair` is two services; see its `README.md`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const WORKER: &str = "worker/src/jobs.ts";

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/catch-all-type-pair")
}

/// The scan's stdout projection and every index blob it stored.
struct Scan {
    projection: serde_json::Value,
    blobs: Vec<serde_json::Value>,
}

fn carrick(root: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(root)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", root.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1");
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
    cmd
}

fn scan() -> &'static Scan {
    static SCAN: OnceLock<Scan> = OnceLock::new();
    SCAN.get_or_init(|| {
        // The stdout projection (edges) and the stored blobs (verdicts) come
        // from two scans: `CARRICK_OUTPUT_JSON` does not store the verdicts.
        let projection_cache = tempfile::tempdir().expect("temp cache dir");
        let output = carrick(&fixture())
            .env("CARRICK_LOCAL_STORAGE_DIR", projection_cache.path())
            .env("CARRICK_OUTPUT_JSON", "1")
            .output()
            .expect("failed to spawn carrick");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "scan failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let start = stdout
            .find("\n{")
            .map(|i| i + 1)
            .or_else(|| stdout.starts_with('{').then_some(0))
            .unwrap_or_else(|| panic!("no JSON in scanner stdout:\n{stdout}"));
        let projection = serde_json::from_str(&stdout[start..])
            .unwrap_or_else(|e| panic!("projection parse failed: {e}\n{stdout}"));

        let cache = tempfile::tempdir().expect("temp cache dir");
        let output = carrick(&fixture())
            .env("CARRICK_LOCAL_STORAGE_DIR", cache.path())
            .output()
            .expect("failed to spawn carrick");
        assert!(
            output.status.success(),
            "scan failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let blobs = std::fs::read_dir(cache.path())
            .expect("read the storage dir")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .map(|path| {
                serde_json::from_str(&std::fs::read_to_string(path).expect("read a blob"))
                    .expect("a blob is JSON")
            })
            .collect();
        Scan { projection, blobs }
    })
}

type Edge = (String, String, String, String);

fn edges(matches: &serde_json::Value) -> BTreeSet<Edge> {
    let field = |edge: &serde_json::Value, name: &str| {
        edge[name]
            .as_str()
            .unwrap_or_else(|| panic!("edge has no {name}: {edge}"))
            .to_string()
    };
    matches
        .as_array()
        .expect("a matches array")
        .iter()
        .map(|edge| {
            (
                field(edge, "producer_repo"),
                field(edge, "producer_key"),
                field(edge, "consumer_repo"),
                field(edge, "consumer_key"),
            )
        })
        .collect()
}

/// `(half, verdict)` stored for the worker call at `line`.
fn verdicts_at(line: u64) -> BTreeSet<(String, String)> {
    let site = format!("{WORKER}:{line}");
    let mut found = BTreeSet::new();
    for blob in &scan().blobs {
        for pair in blob["compat_verdicts"].as_array().into_iter().flatten() {
            for at in pair["sites"].as_array().into_iter().flatten() {
                if at["consumer_location"] != site.as_str() {
                    continue;
                }
                for half in ["request", "response"] {
                    if let Some(verdict) = at[half]["verdict"].as_str() {
                        found.insert((half.to_string(), verdict.to_string()));
                    }
                }
            }
        }
    }
    found
}

#[test]
fn a_catch_all_covering_a_call_literal_keeps_its_edge_and_gets_no_verdict() {
    let key: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture().join("expected.json")).expect("read the answer key"),
    )
    .expect("the answer key is JSON");
    assert_eq!(
        edges(&scan().projection["cross_repo_matches"]),
        edges(&key["matches"]),
        "both calls keep their edge, the catch-all's included"
    );

    // The call only the catch-all serves: nothing is compared.
    assert_eq!(
        verdicts_at(15),
        BTreeSet::new(),
        "a catch-all covering `/v1/jobs/close` is not the type answer"
    );

    // The control: a real break against the concrete route is still found.
    let open = verdicts_at(21);
    assert!(
        open.contains(&("request".to_string(), "incompatible".to_string())),
        "the concrete route's call keeps its decided verdict: {open:?}"
    );
}
