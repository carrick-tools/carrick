//! Socket.IO contracts whose socket is declared through a type alias
//! (carrick#670).
//!
//! Drives the real scanner binary — offline, cassette-mocked LLM — over
//! `tests/fixtures/socket-type-alias-monorepo/`, where each side names its
//! socket type once (`type SupervisorSocket = Socket<…>`) and then declares
//! every field with the alias. The declared-type rule from #659 admitted the
//! imported names literally, so before the alias was resolved neither side
//! produced a row at all.
//!
//! One file goes further and never names the socket type at all: it imports
//! the alias its sibling declares, which the pass follows one hop through the
//! binding resolver (carrick#670, second half).
//!
//! Every `__llm__` cassette is empty, so a row exists only if the
//! deterministic socket pass emitted it. These tests FAIL on main by
//! construction.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/socket-type-alias-monorepo")
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
fn alias_declared_sockets_are_recorded_on_both_sides() {
    let projection = scan_fixture();

    for event in ["run:start", "run:stop"] {
        let key = format!("socket|CLIENT->SERVER|{event}");
        // Publisher: a field declared with the client-socket alias.
        find_op(&projection, "calls", &key, "controller.ts");
        // Handler: a constructor parameter property declared with the
        // server-socket alias.
        find_op(&projection, "endpoints", &key, "workloadServer.ts");
    }
}

#[test]
fn alias_declared_sockets_meet_on_one_key() {
    let projection = scan_fixture();

    let matches = projection["cross_repo_matches"]
        .as_array()
        .expect("projection has no `cross_repo_matches` array");
    for event in ["run:start", "run:stop"] {
        let key = format!("socket|CLIENT->SERVER|{event}");
        assert!(
            matches
                .iter()
                .any(|m| m["producer_key"].as_str() == Some(&key)
                    && m["consumer_key"].as_str() == Some(&key)),
            "{key} should match the handler to its publisher; matched keys: {:?}",
            matches
                .iter()
                .map(|m| m["producer_key"].as_str())
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn an_imported_alias_is_followed_to_its_declaring_module() {
    // `notifier.ts` imports the alias from `controller.ts` and never names the
    // socket type, so this row exists only if the import hop ran.
    let projection = scan_fixture();

    find_op(
        &projection,
        "endpoints",
        "socket|SERVER->CLIENT|run:notify",
        "notifier.ts",
    );
    find_op(
        &projection,
        "calls",
        "socket|SERVER->CLIENT|run:notify",
        "workloadServer.ts",
    );

    let matches = projection["cross_repo_matches"]
        .as_array()
        .expect("projection has no `cross_repo_matches` array");
    let key = "socket|SERVER->CLIENT|run:notify";
    assert!(
        matches
            .iter()
            .any(|m| m["producer_key"].as_str() == Some(key)
                && m["consumer_key"].as_str() == Some(key)),
        "{key} should match the alias-importing listener to its emitter; matched keys: {:?}",
        matches
            .iter()
            .map(|m| m["producer_key"].as_str())
            .collect::<Vec<_>>()
    );
}
