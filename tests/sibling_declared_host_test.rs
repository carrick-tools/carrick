//! carrick#3441 (slice A1): what the scanner's own cross-repo join does with a
//! call to an absolute host that a SIBLING service declares internal, and with
//! one nobody declares. Two-phase, offline, the real binary, the model mocked
//! from each repo's `__llm__/` (the shape of `tests/xrepo_harness_test.rs`).
//!
//! Fixture: `tests/fixtures/sibling-declared-host/` (see its README).

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Deserialize)]
struct Projection {
    #[serde(default)]
    calls: Vec<Op>,
    #[serde(default)]
    cross_repo_matches: Vec<Edge>,
}

#[derive(Debug, Deserialize)]
struct Op {
    key: String,
}

#[derive(Debug, Deserialize)]
struct Edge {
    producer_repo: String,
    producer_key: String,
    consumer_repo: String,
    consumer_key: String,
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sibling-declared-host")
}

fn scan(repo: &Path, cache: &Path, join: bool) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", cache)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1");
    if join {
        cmd.env("CARRICK_OUTPUT_JSON", "1")
            .env_remove("CARRICK_LOCAL_STORAGE_ISOLATE");
    } else {
        cmd.env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
            .env_remove("CARRICK_OUTPUT_JSON");
    }
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
    let out = cmd.output().expect("spawn carrick");
    assert!(
        out.status.success(),
        "scan of {} failed:\n{}",
        repo.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn joined() -> Projection {
    let cache = tempfile::tempdir().expect("temp cache dir");
    for repo in ["ledger-api", "tool-cli"] {
        scan(&root().join(repo), cache.path(), false);
    }
    let out = scan(&root().join("tool-cli"), cache.path(), true);
    serde_json::from_slice(&out.stdout).expect("the join prints an EvalProjection")
}

/// Edges into `ledger-api` from `tool-cli`, as `(producer_key, consumer_key)`.
fn edges(p: &Projection) -> Vec<(String, String)> {
    let mut v: Vec<_> = p
        .cross_repo_matches
        .iter()
        .filter(|e| e.producer_repo == "ledger-api" && e.consumer_repo == "tool-cli")
        .map(|e| (e.producer_key.clone(), e.consumer_key.clone()))
        .collect();
    v.sort();
    v
}

#[test]
fn the_join_pairs_calls_through_a_host_a_sibling_declares_internal() {
    let p = joined();
    let calls: Vec<_> = p.calls.iter().map(|c| c.key.as_str()).collect();
    // Calls keep their host on the key; the pairing is by path.
    assert!(calls.contains(&"http|POST|https://api.ledger.example/jobs/run"));
    let e = edges(&p);
    assert!(
        e.iter().any(|(prod, _)| prod == "http|POST|/jobs/run"),
        "POST /jobs/run unpaired: {e:?}"
    );
    assert!(
        e.iter().any(|(prod, _)| prod == "http|GET|/jobs/status"),
        "GET /jobs/status unpaired: {e:?}"
    );
}

/// A call to a host NOBODY declares internal is a third party
/// (carrick-cloud#656, carrick#3468): `mailer` has no `internalDomains` entry
/// anywhere, so its `POST /v1/send` does not meet `ledger-api`'s route of the
/// same path. The call stays on the key with its host. The ledger edges are
/// asserted in the same test, so a change that stops all absolute-host
/// pairing cannot pass it.
#[test]
fn the_join_never_pairs_a_host_nobody_declares() {
    let p = joined();
    let e = edges(&p);
    assert!(
        !e.iter().any(|(prod, _)| prod == "http|POST|/v1/send"),
        "an undeclared host was paired by path: {e:?}"
    );
    let calls: Vec<_> = p.calls.iter().map(|c| c.key.as_str()).collect();
    assert!(
        calls.contains(&"http|POST|https://api.mailer.example/v1/send"),
        "the third-party call left the key: {calls:?}"
    );
    for want in ["http|POST|/jobs/run", "http|GET|/jobs/status"] {
        assert!(
            e.iter().any(|(prod, _)| prod == want),
            "{want} unpaired: {e:?}"
        );
    }
}
