//! carrick-cloud#656: a call to an absolute third-party URL keeps its origin
//! on the match key, whether or not the host was declared.
//!
//! Deterministic end to end. The LLM is replayed from `__llm__/`, holding what
//! extraction honestly says about each call: the whole URL the call site
//! passes. What is under test is what the scanner then keys it as.
//!
//! The fixture declares nothing — no `carrick.json` at all — which is the
//! state a first scan of any repo runs in. Before this change an undeclared
//! `https://` host fell through the origin strip that exists for
//! `http://localhost:PORT` self-calls, and a working third-party call reached
//! the reader as `POST /emails`: a bare path that matches no producer and is
//! reported as a missing internal endpoint.
//!
//! See `tests/fixtures/undeclared-external-host/README.md` for the shape.

use std::process::Command;

fn calls() -> Vec<serde_json::Value> {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/undeclared-external-host");
    let mock_dir = format!("{fixture}/__llm__/");

    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(&fixture)
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_MOCK_FIXTURE_DIR", &mock_dir)
        .env("CARRICK_OUTPUT_JSON", "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env_remove("GITHUB_REPOSITORY")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("CI")
        .output()
        .expect("failed to spawn carrick binary");

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

fn call_at(calls: &[serde_json::Value], file: &str, line: i64) -> serde_json::Value {
    calls
        .iter()
        .find(|call| call["file"] == file && call["line"].as_i64() == Some(line))
        .unwrap_or_else(|| panic!("no row at {file}:{line}: {calls:#?}"))
        .clone()
}

#[test]
fn an_undeclared_third_party_host_survives_into_the_match_key() {
    let calls = calls();

    let third_party = call_at(&calls, "src/mailer.ts", 4);
    assert_eq!(third_party["method"], "POST");
    assert_eq!(
        third_party["target_url"], "https://api.example-mail.test/emails",
        "the raw target is what the call site passes"
    );
    assert_eq!(
        third_party["key"], "http|POST|https://api.example-mail.test/emails",
        "the origin IS the classification: stripped, this key reads as an \
         internal call to a producer nobody declares"
    );
}

#[test]
fn a_loopback_self_call_still_keys_on_its_bare_path() {
    let calls = calls();

    let self_call = call_at(&calls, "src/selfcall.ts", 5);
    assert_eq!(self_call["method"], "POST");
    assert_eq!(
        self_call["target_url"], "http://localhost:7100/emails",
        "the raw target is what the call site passes"
    );
    assert_eq!(
        self_call["key"], "http|POST|/emails",
        "a loopback origin names this machine and classifies nothing, so the \
         strip that lets a self-call match its own endpoint stands"
    );
}

/// The two calls state the same path and differ only in their origin, so a key
/// rule that read anything else about them could not tell them apart.
#[test]
fn the_two_keys_differ_only_because_the_origins_do() {
    let calls = calls();
    let third_party = call_at(&calls, "src/mailer.ts", 4);
    let self_call = call_at(&calls, "src/selfcall.ts", 5);

    assert_ne!(third_party["key"], self_call["key"]);
    assert!(
        third_party["target_url"]
            .as_str()
            .is_some_and(|url| url.ends_with("/emails"))
            && self_call["target_url"]
                .as_str()
                .is_some_and(|url| url.ends_with("/emails")),
        "both call sites state the same path"
    );
}
