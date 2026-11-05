//! carrick#872: the value a wrapper writes for a body-dispatching route
//! reaches the rows emitted at the sites that call the wrapper, through the
//! wrapper's request summaries (carrick#1555).
//!
//! Deterministic end to end. The LLM is replayed from `__llm__/`, and the
//! cassette states the dispatch value ONLY where the model can really see it:
//! on the client's own requests, inside `src/api-client.ts`. The consumer
//! files hold what production holds — a row at the delegating site with a
//! method and a path and no value — so an assertion here that a site's row
//! carries a value is an assertion about the scanner's join, not about the
//! model.
//!
//! The one exception is `factory.json` (carrick#2201), which states a value
//! at every site the way the model does when it guesses one from a method
//! name. The assertions on `factory.ts` measure which of those guesses the
//! scanner lets stand.
//!
//! See `tests/fixtures/wrapper-dispatch/README.md` for the shape and the
//! answer key.

use std::path::PathBuf;
use std::process::Command;

fn calls(fixture_dir: &str) -> Vec<serde_json::Value> {
    scan_calls(fixture_dir)
}

/// Whether the live sidecar can run. CI builds it before the Rust tests, so
/// a skip there would make a green tick mean nothing.
fn sidecar_available() -> bool {
    let node = Command::new("node").arg("--version").output().is_ok();
    let built = std::env::var_os("CARRICK_SIDECAR_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar"))
        .join("dist/src/index.js")
        .exists();
    assert!(
        (node && built) || std::env::var("CI").is_err(),
        "node or the built sidecar is missing in CI, so these tests would silently not run"
    );
    node && built
}

/// The scan's call rows with the compiler's position rows walked, as the
/// cloud runner scans: what a receiver a call initialised is, is the `call`
/// row's to say (carrick#2615). `None` where the sidecar cannot run.
fn calls_with_rows(fixture_dir: &str) -> Option<Vec<serde_json::Value>> {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return None;
    }
    Some(scan_calls(fixture_dir))
}

fn scan_calls(fixture_dir: &str) -> Vec<serde_json::Value> {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/{fixture_dir}");
    let mock_dir = format!("{fixture}/__llm__/");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(&fixture)
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_MOCK_FIXTURE_DIR", &mock_dir)
        .env("CARRICK_OUTPUT_JSON", "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env_remove("GITHUB_REPOSITORY")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("CI");
    let output = cmd.output().expect("failed to spawn carrick binary");

    assert!(
        output.status.success(),
        "scanner exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("scanner stdout was not UTF-8");
    let projection: serde_json::Value =
        serde_json::from_str(&stdout).expect("scanner output was not valid JSON");
    projection["calls"]
        .as_array()
        .expect("projection carries a calls array")
        .clone()
}

/// The one call row a file states at `line`.
fn call_at<'a>(calls: &'a [serde_json::Value], file: &str, line: i64) -> &'a serde_json::Value {
    let matches: Vec<&serde_json::Value> = calls
        .iter()
        .filter(|call| {
            call["line"].as_i64() == Some(line)
                && call["file"].as_str().is_some_and(|f| f.ends_with(file))
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one call row at {file}:{line}, got {matches:#?}"
    );
    matches[0]
}

fn dispatch_value(call: &serde_json::Value) -> Option<&str> {
    call["dispatch"]["value"].as_str()
}

#[test]
fn a_site_calling_a_member_that_issues_the_request_carries_its_value() {
    let calls = calls("wrapper-dispatch");
    let site = call_at(&calls, "search.ts", 4);
    assert_eq!(
        dispatch_value(site),
        Some("search-by-intent"),
        "the value is written in `searchByIntent`'s own body, one file away"
    );
    assert_eq!(site["dispatch"]["field"].as_str(), Some("action"));
    assert_eq!(site["dispatch"]["location"].as_str(), Some("body"));
    assert_eq!(site["method"].as_str(), Some("POST"));
}

#[test]
fn a_site_calling_a_member_that_delegates_through_a_callback_carries_its_value() {
    let calls = calls("wrapper-dispatch");
    assert_eq!(
        dispatch_value(call_at(&calls, "graph.ts", 4)),
        Some("get-cross-repo-data"),
        "`getAllRepoData` reaches its request through the cache's callback"
    );
}

#[test]
fn a_site_two_delegations_from_the_request_carries_its_value() {
    let calls = calls("wrapper-dispatch");
    assert_eq!(
        dispatch_value(call_at(&calls, "services.ts", 4)),
        Some("get-cross-repo-data"),
        "`findService` -> `getAllRepoData` -> `fetchCrossRepoData`"
    );
}

#[test]
fn a_site_calling_a_member_that_issues_two_requests_states_one_row_per_request() {
    // `refreshEverything` writes two actions, one per request. The site sends
    // both, so it states both, each with the value its own body writes
    // (carrick#1555, decision 2): neither is a guess about which one it asks
    // for.
    let calls = calls("wrapper-dispatch");
    let mut values: Vec<Option<&str>> = calls
        .iter()
        .filter(|call| {
            call["line"].as_i64() == Some(4)
                && call["file"]
                    .as_str()
                    .is_some_and(|f| f.ends_with("refresh.ts"))
        })
        .map(dispatch_value)
        .collect();
    values.sort();
    assert_eq!(
        values,
        vec![Some("invalidate-cache"), Some("rebuild-index")]
    );
}

/// carrick#2201: `factory.ts` holds the client a function-typed factory
/// returns. Its cassette states a value at every site, as the model does when
/// it guesses one from a method name. What `make` returns is the compiler's
/// to say, so these scan with the position rows walked (carrick#2615).
#[test]
fn a_guessed_site_value_takes_the_wrapper_s_value() {
    let Some(calls) = calls_with_rows("wrapper-dispatch") else {
        return;
    };
    assert_eq!(
        dispatch_value(call_at(&calls, "factory.ts", 7)),
        Some("get-cross-repo-data"),
        "`get-all-repo-data` is written nowhere in the file; the wrapper's value replaces it"
    );
}

#[test]
fn a_guessed_site_value_on_a_member_that_issues_two_requests_becomes_one_row_per_request() {
    // The same answer as `refresh.ts:4`: the guessed `refresh-everything` is
    // gone and the site states both requests the member sends.
    let Some(calls) = calls_with_rows("wrapper-dispatch") else {
        return;
    };
    let mut values: Vec<Option<&str>> = calls
        .iter()
        .filter(|call| {
            call["line"].as_i64() == Some(8)
                && call["file"]
                    .as_str()
                    .is_some_and(|f| f.ends_with("factory.ts"))
        })
        .map(dispatch_value)
        .collect();
    values.sort();
    assert_eq!(
        values,
        vec![Some("invalidate-cache"), Some("rebuild-index")]
    );
}

#[test]
fn a_site_value_its_file_writes_is_kept() {
    let Some(calls) = calls_with_rows("wrapper-dispatch") else {
        return;
    };
    assert_eq!(
        dispatch_value(call_at(&calls, "factory.ts", 9)),
        Some("purge-cache"),
        "the site writes `purge-cache` as the argument `send` posts"
    );
}

#[test]
fn a_same_file_wrapper_site_carries_the_value_the_wrapper_writes() {
    let calls = calls("wrapper-dispatch");
    let site = call_at(&calls, "local.ts", 11);
    assert_eq!(
        dispatch_value(site),
        Some("store-metadata"),
        "the value is at the wrapper's request four lines up"
    );
    assert_eq!(
        site["resolution_source"].as_str(),
        Some("request_summary"),
        "the row is the source's: the wrapper's summary states it where the \
         site fills the path the wrapper leaves open (carrick#1555)"
    );
}
