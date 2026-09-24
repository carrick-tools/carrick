//! carrick#1491 through the whole scanner, mocked ($0, no model).
//!
//! `tests/fixtures/retype-http-client` is a two-service monorepo: `api`
//! answers `POST /checkout` with a `CheckoutResult`, and `web` calls it twice
//! through a generic client instance, once with a type argument and once
//! without, reading `response.data.x` after each. Neither call publishes a
//! comparable type, so before the retype check both pairs went unverified and
//! a read of a field the producer does not return went unflagged.
//!
//! The scan must flag both reads when the producer returns `{ y }`, and flag
//! neither when it returns `{ x }`. It must also say, in its log, why each
//! pair it could not verify is unverified.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/retype-http-client")
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create the destination");
    for entry in std::fs::read_dir(from).expect("read the fixture") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy a fixture file");
        }
    }
}

/// Scan `root` with the fixture's cassettes; returns the projection and the
/// scanner's combined log.
fn scan(root: &Path) -> (serde_json::Value, String) {
    let cache = tempfile::tempdir().expect("temp cache dir");
    let cassettes = fixture_dir().join("__llm__");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
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
fn a_read_of_a_field_the_producer_does_not_return_is_flagged() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let (projection, log) = scan(&fixture_dir());

    let matches = checkout_matches(&projection);
    assert_eq!(matches.len(), 2, "both calls match the route: {matches:#?}");
    let mut reasons: Vec<&str> = matches
        .iter()
        .map(|m| {
            assert_eq!(m["type_compatible"], false, "{m:#}");
            m["mismatch_reason"].as_str().unwrap_or_default()
        })
        .collect();
    reasons.sort();
    // The typed call reads at line 7, the untyped one at line 12.
    for (reason, line) in reasons.iter().zip([12, 7]) {
        assert!(
            reason.contains(&format!("web/src/checkout.ts:{line}:"))
                && reason.contains("Property 'x' does not exist on type '{ y: number; }'"),
            "the read is named at its line: {reason}"
        );
    }

    // The request half of each pair compared nothing (the route reads no
    // body), and the log says so per pair, with the reason.
    for line in [6, 11] {
        assert!(
            log.contains(&format!(
                "Types not verified: POST /checkout request (web/src/checkout.ts:{line} in web \
                 against api):"
            )),
            "no reason line for the call at line {line}:\n{log}"
        );
    }
}

#[test]
fn a_read_of_a_field_the_producer_returns_is_not_flagged() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("retype-http-client");
    copy_tree(&fixture_dir(), &root);
    let routes = root.join("api/src/routes.ts");
    let source = std::fs::read_to_string(&routes).expect("read routes");
    let renamed = source
        .replace("y: number;", "x: number;")
        .replace("{ y: 1 }", "{ x: 1 }");
    assert_ne!(source, renamed, "the fixture's producer shape moved");
    std::fs::write(&routes, renamed).expect("write routes");

    let (projection, log) = scan(&root);
    let matches = checkout_matches(&projection);
    assert_eq!(matches.len(), 2, "{matches:#?}");
    for m in matches {
        assert_ne!(m["type_compatible"], false, "nothing to flag: {m:#}");
        assert!(m["mismatch_reason"].is_null(), "{m:#}");
    }
    assert!(
        !log.contains("Types not verified: POST /checkout response"),
        "the response half of both pairs is now a fact:\n{log}"
    );
}
