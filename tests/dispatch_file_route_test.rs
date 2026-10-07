//! carrick#2048 through the whole scanner, mocked ($0, no model): a file-based
//! route whose handler dispatches on a body field keeps one producer row per
//! case, and each call that sends a case links to it.
//!
//! See `tests/fixtures/dispatch-file-route/README.md` for the shape. The three
//! cases echo three different kinds of id, so one scan takes every path the
//! join has for a row the file layout already states.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dispatch-file-route")
}

/// The scanner over `root`, replaying the fixture's own model answers.
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

/// The scan's JSON projection: endpoints, calls and edges.
fn scan() -> &'static serde_json::Value {
    static SCAN: OnceLock<serde_json::Value> = OnceLock::new();
    SCAN.get_or_init(|| {
        let cache = tempfile::tempdir().expect("temp cache dir");
        let output = carrick(&fixture())
            .env("CARRICK_LOCAL_STORAGE_DIR", cache.path())
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
        serde_json::from_str(&stdout[start..])
            .unwrap_or_else(|e| panic!("projection parse failed: {e}\n{stdout}"))
    })
}

/// `(method, path, dispatch value)` for every row of `section`.
fn rows(section: &str) -> BTreeSet<(String, String, Option<String>)> {
    scan()[section]
        .as_array()
        .unwrap_or_else(|| panic!("a {section} array"))
        .iter()
        .map(|row| {
            (
                row["method"].as_str().expect("method").to_string(),
                row["path"].as_str().expect("path").to_string(),
                row["dispatch"]["value"].as_str().map(str::to_string),
            )
        })
        .collect()
}

fn row(method: &str, path: &str, case: Option<&str>) -> (String, String, Option<String>) {
    (
        method.to_string(),
        path.to_string(),
        case.map(str::to_string),
    )
}

const ROUTE: &str = "/api/orders/:orderId";

/// The `POST` route is one row per case the handler answers, each stated by
/// the file layout, and no row without a case is left beside them to answer
/// every call whatever it sends.
#[test]
fn a_file_route_that_dispatches_on_its_body_keeps_every_case() {
    assert_eq!(
        rows("endpoints"),
        BTreeSet::from([
            row("GET", ROUTE, None),
            row("POST", ROUTE, Some("cancel")),
            row("POST", ROUTE, Some("confirm")),
            row("POST", ROUTE, Some("refund")),
        ]),
    );
    for endpoint in scan()["endpoints"].as_array().expect("endpoints") {
        assert_eq!(
            endpoint["resolution_source"], "file_based_route",
            "every case is a case of the route the layout states: {endpoint}"
        );
        assert_eq!(endpoint["file"], "app/api/orders/[orderId]/route.ts");
    }
}

/// Each call sends one case, and each links to the row for it: one edge per
/// call. A pair is drawn only where the call's value is the case's
/// (`carrick_match::dispatch_verdict`), and the route has no row without a
/// case, so a `POST` edge per call is each call reaching its own case.
#[test]
fn each_call_links_to_the_case_it_sends() {
    let calls = rows("calls");
    assert_eq!(
        calls,
        BTreeSet::from([
            row("GET", ROUTE, None),
            row("POST", ROUTE, Some("cancel")),
            row("POST", ROUTE, Some("confirm")),
            row("POST", ROUTE, Some("refund")),
        ]),
        "each call states the case it sends"
    );

    let edges = scan()["cross_repo_matches"]
        .as_array()
        .expect("a cross_repo_matches array");
    let producers: Vec<&str> = edges
        .iter()
        .map(|edge| edge["producer_key"].as_str().expect("producer_key"))
        .collect();
    assert_eq!(
        producers
            .iter()
            .filter(|key| **key == "http|POST|/api/orders/:orderId")
            .count(),
        3,
        "each POST call links to a case: {edges:#?}"
    );
    assert_eq!(edges.len(), 4, "one edge per call: {edges:#?}");
}
