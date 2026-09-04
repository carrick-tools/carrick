//! Client members published by another workspace package (carrick#666).
//!
//! Drives the real scanner binary — offline, cassette-mocked LLM — over
//! `tests/fixtures/workspace-package-client/`, where the one scanned service
//! never imports the client it calls. It imports a factory from a sibling
//! package BY PACKAGE NAME, asks it for a client, parks it on a local, and
//! calls a member. The route each site reaches is stated only in the other
//! package's source, which is not in the service's own file list.
//!
//! Every `__llm__` cassette is empty, so a row exists here only if a
//! deterministic pass emitted it. Pre-fix baseline: the member join followed
//! relative specifiers only, so it never reached a sibling package at all and
//! the whole surface produced no consumer row — which is how a package with
//! roughly thirty real calls recorded one. This test asserts the post-fix
//! state, so it FAILS on the pre-fix scanner by construction.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workspace-package-client")
}

/// Same ambient-CI stripping as `xrepo_harness_test.rs`: keeps repo identity
/// tied to the scanned fixture dir and `should_upload_data()` deterministic.
fn strip_ci_env(cmd: &mut Command) -> &mut Command {
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

/// One isolated offline scan of the fixture; returns the parsed projection.
fn scan_fixture() -> serde_json::Value {
    let cache = tempfile::tempdir().expect("temp cache dir");
    let cassettes = fixture_dir().join("__llm__");
    assert!(cassettes.exists(), "fixture cassette dir missing");

    let bin = PathBuf::from(env!("CARGO_BIN_EXE_carrick"));
    let mut cmd = Command::new(&bin);
    cmd.arg(fixture_dir())
        .env("CARRICK_LOCAL_STORAGE_DIR", cache.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_OUTPUT_JSON", "1");
    strip_ci_env(&mut cmd);
    let output = cmd.output().expect("failed to spawn carrick");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "fixture scan exited non-zero:\n{stderr}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON in scanner stdout:\n{stdout}"));
    serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("projection parse failed: {e}\n{stdout}"))
}

/// Every consumer row the scan produced, as `(method, path, file, line)`.
fn calls(projection: &serde_json::Value) -> Vec<(String, String, String, i64)> {
    projection["calls"]
        .as_array()
        .expect("projection has no `calls` array")
        .iter()
        .map(|call| {
            (
                call["method"].as_str().unwrap_or_default().to_string(),
                call["path"].as_str().unwrap_or_default().to_string(),
                call["file"].as_str().unwrap_or_default().to_string(),
                call["line"].as_i64().unwrap_or_default(),
            )
        })
        .collect()
}

fn rows_in(
    calls: &[(String, String, String, i64)],
    file_suffix: &str,
) -> Vec<(String, String, i64)> {
    let mut rows: Vec<(String, String, i64)> = calls
        .iter()
        .filter(|(_, _, file, _)| {
            Path::new(file).ends_with(file_suffix) || file.ends_with(file_suffix)
        })
        .map(|(method, path, _, line)| (method.clone(), path.clone(), *line))
        .collect();
    rows.sort();
    rows
}

#[test]
fn a_member_published_by_a_sibling_package_resolves_at_its_call_site() {
    let projection = scan_fixture();
    let calls = calls(&projection);

    // `coreClient.retrieveWidget(id)`, where `coreClient` came out of a factory
    // imported from `@fixture/core/v2` and the route is written in
    // `packages/core/src/v2/client/index.ts`.
    assert!(
        calls.iter().any(|(method, path, file, line)| {
            method == "GET"
                && path == "/api/v2/widgets/:encoded"
                && file.ends_with("widgets.ts")
                && *line == 9
        }),
        "the core package's route is missing; rows were {calls:?}"
    );

    // The receiver decides which package answers: the same member name reached
    // through the other package's factory is the other package's route.
    assert!(
        calls.iter().any(|(method, path, file, line)| {
            method == "GET"
                && path == "/api/other/widgets/:encoded"
                && file.ends_with("widgets.ts")
                && *line == 17
        }),
        "the other package's route is missing or was answered by the core package; \
         rows were {calls:?}"
    );

    // A chained call is one outbound request, not one per link.
    assert!(
        calls.iter().any(|(method, path, file, line)| {
            method == "POST"
                && path == "/api/v2/widgets/:encoded/archive"
                && file.ends_with("widgets.ts")
                && *line == 38
        }),
        "the archive route is missing; rows were {calls:?}"
    );
}

#[test]
fn a_receiver_that_states_no_package_joins_to_nothing() {
    let projection = scan_fixture();
    let rows = rows_in(&calls(&projection), "widgets.ts");

    // Three sites resolve and no more: the unbound receiver at line 24, the
    // name both surfaces declare differently at line 31, and the second link of
    // the chain at line 38 all resolve to nothing.
    let mut expected = vec![
        ("GET".to_string(), "/api/v2/widgets/:encoded".to_string(), 9),
        (
            "GET".to_string(),
            "/api/other/widgets/:encoded".to_string(),
            17,
        ),
        (
            "POST".to_string(),
            "/api/v2/widgets/:encoded/archive".to_string(),
            38,
        ),
    ];
    expected.sort();
    assert_eq!(rows, expected, "unexpected rows for widgets.ts");
}

#[test]
fn a_local_name_bound_from_two_packages_joins_to_neither() {
    let projection = scan_fixture();
    let rows = rows_in(&calls(&projection), "ambiguous.ts");
    assert!(
        rows.is_empty(),
        "a receiver name bound from two packages must resolve to neither; got {rows:?}"
    );
}
