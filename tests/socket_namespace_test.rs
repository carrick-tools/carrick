//! Socket.IO contracts served on a custom namespace (carrick#662).
//!
//! Drives the real scanner binary — offline, cassette-mocked LLM — over
//! `tests/fixtures/socket-namespace-monorepo/`, where the handler for an event
//! is registered on a namespace carved off a server in another function, and
//! the publishers name that namespace only in the URL path they connect with.
//!
//! Two things had to hold for the handler to be recorded. The namespace
//! binding is rooted by its declared `Namespace` type, since the server it
//! comes off is a function return the pass cannot follow. And a file that
//! carves a namespace is no longer dropped whole: the operation key has no
//! namespace component and neither side of the wire can supply one, so the ops
//! are recorded under the plain event name and meet there.
//!
//! Every `__llm__` cassette is empty, so a row exists only if the
//! deterministic socket pass emitted it. Pre-fix baseline: the fixture
//! produced the two publisher rows and nothing from the platform at all.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/socket-namespace-monorepo")
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

/// The single op for `key` in the file whose path ends with `file_suffix`.
fn find_op<'a>(
    projection: &'a serde_json::Value,
    side: &str,
    key: &str,
    file_suffix: &str,
) -> &'a serde_json::Value {
    let ops = projection[side]
        .as_array()
        .unwrap_or_else(|| panic!("projection has no `{side}` array"));
    let matched: Vec<&serde_json::Value> = ops
        .iter()
        .filter(|op| {
            op["key"].as_str() == Some(key)
                && op["file"].as_str().is_some_and(|f| {
                    Path::new(f).ends_with(file_suffix) || f.ends_with(file_suffix)
                })
        })
        .collect();
    assert_eq!(
        matched.len(),
        1,
        "expected exactly one `{side}` op with key `{key}` in `{file_suffix}`; \
         found {}; ops present: {:?}",
        matched.len(),
        ops.iter()
            .map(|o| (o["key"].as_str(), o["file"].as_str()))
            .collect::<Vec<_>>()
    );
    matched[0]
}

#[test]
fn a_namespaced_handler_is_recorded_and_meets_its_publishers() {
    let projection = scan_fixture();

    for event in ["run:subscribe", "run:unsubscribe"] {
        let key = format!("socket|CLIENT->SERVER|{event}");
        // The handler, on the namespace's per-connection socket.
        find_op(&projection, "endpoints", &key, "gateway.ts");
        // The publisher, on a client socket held in a class field.
        find_op(&projection, "calls", &key, "session.ts");
    }

    let matches = projection["cross_repo_matches"]
        .as_array()
        .expect("projection has no `cross_repo_matches` array");
    for event in ["run:subscribe", "run:unsubscribe"] {
        let key = format!("socket|CLIENT->SERVER|{event}");
        assert!(
            matches
                .iter()
                .any(|m| m["producer_key"].as_str() == Some(&key)
                    && m["consumer_key"].as_str() == Some(&key)),
            "{key} should match the namespaced handler to its publisher; \
             matched keys: {:?}",
            matches
                .iter()
                .map(|m| m["producer_key"].as_str())
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn a_default_namespace_handler_in_the_same_file_is_kept_too() {
    // The file carves a namespace AND serves the default one. Dropping the
    // whole file was what hid the namespaced handler; the default-namespace
    // handler must survive the same change rather than be traded for it.
    let projection = scan_fixture();
    find_op(
        &projection,
        "endpoints",
        "socket|CLIENT->SERVER|session:hello",
        "gateway.ts",
    );
}
