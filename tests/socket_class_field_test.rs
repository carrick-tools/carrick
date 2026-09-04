//! Socket.IO contracts held on class fields (carrick#659).
//!
//! Drives the real scanner binary — offline, cassette-mocked LLM — over
//! `tests/fixtures/socket-class-field-monorepo/`, where neither side writes an
//! op on a local binding:
//!
//! - the supervisor builds its client socket in one method, parks it on
//!   `private notifications?: Socket<…>`, and emits `run:subscribe` /
//!   `run:unsubscribe` from two other methods,
//! - the gateway hands each accepted connection to a class that keeps it on a
//!   constructor parameter property and registers the listeners there.
//!
//! Every `__llm__` cassette is empty, so a row exists here only if the
//! deterministic socket pass emitted it. Pre-fix baseline: the pass resolved no
//! root for `this.<field>`, so the whole contract — both publishers and both
//! listeners — was absent, which is what let an agent read a one-publisher
//! answer as complete. This test asserts the post-fix state, so it FAILS on the
//! pre-fix scanner by construction.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/socket-class-field-monorepo")
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
fn class_field_socket_contract_is_recorded_on_both_sides() {
    let projection = scan_fixture();

    // Publisher side: both emits sit on `this.notifications`, several methods
    // away from the `io(...)` that built it.
    for event in ["run:subscribe", "run:unsubscribe"] {
        let key = format!("socket|CLIENT->SERVER|{event}");
        let call = find_op(&projection, "calls", &key, "session.ts");
        assert_eq!(
            call["protocol"].as_str(),
            Some("socket"),
            "{event}: emitter is a socket consumer"
        );
    }

    // Listener side: the per-connection socket reaches the handlers through a
    // constructor parameter property.
    for event in ["run:subscribe", "run:unsubscribe"] {
        let key = format!("socket|CLIENT->SERVER|{event}");
        find_op(&projection, "endpoints", &key, "gateway.ts");
    }

    // Server -> client leg, for the same reason on the other direction: the
    // gateway emits on its field, the supervisor listens on a local binding.
    find_op(
        &projection,
        "calls",
        "socket|SERVER->CLIENT|run:notify",
        "gateway.ts",
    );
    find_op(
        &projection,
        "endpoints",
        "socket|SERVER->CLIENT|run:notify",
        "session.ts",
    );

    // Reserved lifecycle events stay out of the contract even on field roots.
    let keys: Vec<&str> = projection["calls"]
        .as_array()
        .into_iter()
        .chain(projection["endpoints"].as_array())
        .flatten()
        .filter_map(|op| op["key"].as_str())
        .collect();
    assert!(
        !keys.iter().any(|key| key.contains("|connect")
            || key.contains("|disconnect")
            || key.ends_with("|connect")),
        "reserved lifecycle events must not become contract ops: {keys:?}"
    );
}

#[test]
fn class_field_socket_ops_match_across_the_two_services() {
    let projection = scan_fixture();

    let matches = projection["cross_repo_matches"]
        .as_array()
        .expect("projection has no `cross_repo_matches` array");
    for key in [
        "socket|CLIENT->SERVER|run:subscribe",
        "socket|CLIENT->SERVER|run:unsubscribe",
        "socket|SERVER->CLIENT|run:notify",
    ] {
        assert!(
            matches
                .iter()
                .any(|m| m["producer_key"].as_str() == Some(key)
                    && m["consumer_key"].as_str() == Some(key)),
            "{key} should match publisher to listener across the services; \
             matched keys: {:?}",
            matches
                .iter()
                .map(|m| m["producer_key"].as_str())
                .collect::<Vec<_>>()
        );
    }
}
