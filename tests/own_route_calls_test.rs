//! carrick#1926 / carrick#1944 through the whole scanner, mocked ($0, no
//! model): a call to a route of the calling service is an index row, marked
//! `own_route`, and its pair is an edge.
//!
//! Two fixtures, each with its answer key in `expected.json`:
//!
//! - `tests/fixtures/own-route-calls` is one service whose pages, components
//!   and helper module call the routes it defines. The model answers in
//!   `__llm__/` state every call correctly. The scan used to delete each row a
//!   route of the service matched, which left 1 of this fixture's 36.
//! - `tests/fixtures/own-route-sibling-serves` is two services: the caller has
//!   a catch-all route of its own and a sibling defines the concrete route.
//!   The mark is the caller's statement about itself, and the edge goes to
//!   whoever serves the call on the most literal segments.
//!
//! See each fixture's `README.md` for the shape.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn expected(name: &str) -> serde_json::Value {
    let path = fixture(name).join("expected.json");
    serde_json::from_str(&std::fs::read_to_string(&path).expect("read the answer key"))
        .expect("the answer key is JSON")
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
fn projection(name: &str) -> serde_json::Value {
    let cache = tempfile::tempdir().expect("temp cache dir");
    let output = carrick(&fixture(name))
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
}

fn one_service() -> &'static serde_json::Value {
    static SCAN: OnceLock<serde_json::Value> = OnceLock::new();
    SCAN.get_or_init(|| projection("own-route-calls"))
}

fn two_services() -> &'static serde_json::Value {
    static SCAN: OnceLock<serde_json::Value> = OnceLock::new();
    SCAN.get_or_init(|| projection("own-route-sibling-serves"))
}

/// `(file, line, method, path, own_route)` for every call row. A row with no
/// `own_route` key reads false, which is how the key is written.
type CallRow = (String, u64, String, String, bool);

fn call_rows(rows: &serde_json::Value, own_route_absent_is_false: bool) -> BTreeSet<CallRow> {
    rows.as_array()
        .expect("a calls array")
        .iter()
        .map(|row| {
            let own_route = match row.get("own_route") {
                Some(value) => value.as_bool().expect("own_route is a boolean"),
                None => {
                    assert!(own_route_absent_is_false, "the answer key states every row");
                    false
                }
            };
            (
                row["file"].as_str().expect("file").to_string(),
                row["line"].as_u64().expect("line"),
                row["method"].as_str().expect("method").to_string(),
                row["path"].as_str().expect("path").to_string(),
                own_route,
            )
        })
        .collect()
}

/// `(producer_repo, producer_key, consumer_repo, consumer_key)` per edge.
type Edge = (String, String, String, String);

fn edges(matches: &serde_json::Value) -> Vec<Edge> {
    let field = |edge: &serde_json::Value, name: &str| {
        edge[name]
            .as_str()
            .unwrap_or_else(|| panic!("edge has no {name}: {edge}"))
            .to_string()
    };
    let mut edges: Vec<Edge> = matches
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
        .collect();
    edges.sort();
    edges
}

#[test]
fn every_stated_call_is_a_row_and_a_call_to_an_own_route_is_marked() {
    let scan = one_service();
    let key = expected("own-route-calls");

    let rows = call_rows(&scan["calls"], true);
    let expected_rows = call_rows(&key["calls"], false);
    assert_eq!(
        rows, expected_rows,
        "the rows are the answer key's, each with its mark"
    );
    assert_eq!(rows.len(), 36);
    assert_eq!(
        rows.iter().filter(|row| row.4).count(),
        35,
        "every call a route of this service serves is marked"
    );

    // The one unmarked row is written against an undeclared base, which
    // matches no route: the scan cannot say where it goes.
    let unmarked: Vec<&CallRow> = rows.iter().filter(|row| !row.4).collect();
    assert_eq!(unmarked.len(), 1);
    assert_eq!(unmarked[0].0, "app/dashboard/page.tsx");
    assert_eq!(unmarked[0].1, 22);

    let endpoints: BTreeSet<(String, String)> = scan["endpoints"]
        .as_array()
        .expect("an endpoints array")
        .iter()
        .map(|row| {
            (
                row["method"].as_str().expect("method").to_string(),
                row["path"].as_str().expect("path").to_string(),
            )
        })
        .collect();
    let expected_endpoints: BTreeSet<(String, String)> = key["endpoints"]
        .as_array()
        .expect("an endpoints array")
        .iter()
        .map(|row| {
            (
                row["method"].as_str().expect("method").to_string(),
                row["path"].as_str().expect("path").to_string(),
            )
        })
        .collect();
    assert_eq!(
        endpoints, expected_endpoints,
        "keeping the calls moves no route"
    );
}

#[test]
fn each_marked_call_is_an_edge_whose_two_ends_are_the_service() {
    let scan = one_service();
    let edges = edges(&scan["cross_repo_matches"]);

    assert_eq!(edges.len(), 35, "one edge per marked call: {edges:#?}");
    for (producer, _, consumer, _) in &edges {
        assert_eq!(
            producer, consumer,
            "the pair's two ends are one service, which is its only mark"
        );
    }

    // Every route a call reaches is some edge's producer. Only the route
    // nothing calls is left without one: `GET /api/settings` is fetched
    // through an undeclared base alone.
    let produced: BTreeSet<&str> = edges.iter().map(|edge| edge.1.as_str()).collect();
    let uncalled: Vec<String> = expected("own-route-calls")["endpoints"]
        .as_array()
        .expect("an endpoints array")
        .iter()
        .map(|row| {
            format!(
                "http|{}|{}",
                row["method"].as_str().expect("method"),
                row["path"].as_str().expect("path")
            )
        })
        .filter(|key| !produced.contains(key.as_str()))
        .collect();
    assert_eq!(uncalled, vec!["http|GET|/api/settings".to_string()]);
}

/// The index blob itself, which is what the cloud reads: the mark is on the
/// mount-graph row, spelled `"own_route": true`, and absent from a row no own
/// route serves. A marked call is type-checked against its own route
/// (carrick#1945): its verdict row names the service twice, and lists the
/// call's site with a response half, and a request half where the route's
/// method takes a body.
#[test]
fn the_stored_index_carries_the_mark_and_a_verdict_for_each_marked_call() {
    let cache = tempfile::tempdir().expect("temp cache dir");
    let output = carrick(&fixture("own-route-calls"))
        .env("CARRICK_LOCAL_STORAGE_DIR", cache.path())
        .output()
        .expect("failed to spawn carrick");
    assert!(
        output.status.success(),
        "scan failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let blob_path = std::fs::read_dir(cache.path())
        .expect("read the storage dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .expect("the scan stored one index blob");
    let blob: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&blob_path).expect("read the blob"))
            .expect("the blob is JSON");

    let rows = blob["mount_graph"]["data_calls"]
        .as_array()
        .expect("mount_graph.data_calls");
    assert_eq!(rows.len(), 36);
    let marked = rows
        .iter()
        .filter(|row| row["own_route"] == serde_json::json!(true))
        .count();
    assert_eq!(marked, 35);
    let unmarked: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|row| row.get("own_route").is_none())
        .collect();
    assert_eq!(
        unmarked.len(),
        1,
        "a row no own route serves carries no key, never `false`"
    );
    assert_eq!(
        unmarked[0]["target_url"],
        "${process.env.APP_URL}/api/settings"
    );
    assert_eq!(
        blob["calls"].as_array().expect("calls").len(),
        36,
        "the projected call rows are the same rows"
    );

    // Each verdict row names the one service at both ends, and no field marks
    // it as such.
    let verdicts = blob["compat_verdicts"]
        .as_array()
        .expect("a marked call is type-checked, so verdict rows are stored");
    for row in verdicts {
        assert_eq!(row["producer_repo"], row["consumer_repo"], "{row}");
    }
    // The halves each call site got, by `file:line`.
    let mut halves: std::collections::BTreeMap<String, (bool, bool)> = Default::default();
    for row in verdicts {
        for site in row["sites"].as_array().expect("sites") {
            let entry = halves
                .entry(
                    site["consumer_location"]
                        .as_str()
                        .expect("location")
                        .to_string(),
                )
                .or_default();
            entry.0 |= site.get("request").is_some();
            entry.1 |= site.get("response").is_some();
        }
    }
    for row in call_rows(&expected("own-route-calls")["calls"], false) {
        let (file, line, method, _, own_route) = &row;
        let site = format!("{file}:{line}");
        let (request, response) = halves.get(&site).copied().unwrap_or_default();
        if !own_route {
            assert!(!request && !response, "no route serves {row:?}");
            continue;
        }
        // A caller of `listItems()`, which returns a member of the body: the
        // row states what the function returns, which is not the response
        // (carrick#1601), so it has no response entry to pair.
        let restated = matches!(
            site.as_str(),
            "components/ItemPicker.tsx:10" | "components/ItemPickerAlias.tsx:10"
        );
        assert_eq!(response, !restated, "a response half at {row:?}");
        if *method != "GET" {
            assert!(request, "a body method's call has a request half: {row:?}");
        }
    }

    // A call to an own route with no resolved expected type is a shortfall
    // like any other call's, and nothing counts it apart any more.
    let boundary = &blob["boundary"];
    assert!(
        boundary.get("own_route_calls_not_compared").is_none(),
        "{boundary:#}"
    );
    let printed = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!printed.contains("types not compared yet"), "{printed}");
}

#[test]
fn a_call_a_sibling_serves_better_is_marked_and_is_the_sibling_s_edge() {
    let scan = two_services();
    let key = expected("own-route-sibling-serves");

    assert_eq!(
        call_rows(&scan["calls"], true),
        call_rows(&key["calls"], false),
        "both calls are rows, and the caller's catch-all marks both"
    );

    let expected_edges = edges(&key["matches"]);
    assert_eq!(
        edges(&scan["cross_repo_matches"]),
        expected_edges,
        "the concrete route's service is the producer of the call it serves; \
         the catch-all keeps the call only it serves"
    );
    assert_eq!(
        expected_edges
            .iter()
            .filter(|(producer, _, consumer, _)| producer == consumer)
            .count(),
        1,
        "two marked rows, one same-service edge: the mark is not the pair"
    );
}
