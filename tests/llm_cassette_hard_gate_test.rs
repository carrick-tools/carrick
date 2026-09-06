//! Layer-1 cassette hard gate.
//!
//! Replays *frozen* LLM responses (the `__llm__/` cassettes) through the full
//! scanner binary and asserts the machine-readable projection is an exact match
//! against a committed golden. Because the LLM output is frozen, any change in
//! the final output can only come from a change in scanner code — so a failure
//! here is, by construction, a scanner regression. This is the one layer that is
//! safe to *hard-gate* a merge on (the scored eval, `eval_tier_a`, is stochastic
//! and report-only).
//!
//! It runs under plain `cargo test`: fully mocked, no network, no OIDC. No
//! cassette is ever re-recorded when a prompt or model changes — the cassette is
//! a pure scanner-machinery regression net; prompt/model effects are measured
//! live in the scored eval, never here.
//!
//! If this fails after an *intentional* output change, re-record the golden:
//! ```text
//! CARRICK_MOCK_ALL=1 CARRICK_OUTPUT_JSON=1 \
//!   CARRICK_MOCK_FIXTURE_DIR=tests/fixtures/llm-mocked-api/__llm__/ \
//!   cargo run -- tests/fixtures/llm-mocked-api \
//!   > tests/fixtures/llm-mocked-api/__golden__.json
//! ```
//! The golden is portable with no masking because the scanner emits
//! repo-relative paths; the assertion below holds that property.
//!
//! Site-suffixed aliases (`_Call<id>` on a consumer, `_At<id>` on a producer)
//! are compared verbatim: `build_site_id` hashes the REPO-RELATIVE site path
//! (#355), so the ids are identical on every machine and need no masking.

use std::process::Command;

#[test]
fn cassette_hard_gate_llm_mocked_api() {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/llm-mocked-api");
    let mock_dir = format!("{fixture}/__llm__/");

    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(&fixture)
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_MOCK_FIXTURE_DIR", &mock_dir)
        .env("CARRICK_OUTPUT_JSON", "1")
        .output()
        .expect("failed to spawn carrick binary");

    assert!(
        output.status.success(),
        "scanner exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("scanner stdout was not UTF-8");
    // Every path the scanner emits is relative to the SCANNED repo root, so
    // the golden is portable across machines with no masking. Asserted rather
    // than masked: masking is what let absolute runner paths ride into the
    // uploaded index unnoticed for as long as they did. Call-site ids need no
    // masking either — they hash the repo-relative path (#355).
    assert!(
        !stdout.contains(&format!("{repo}/")),
        "scanner output leaked an absolute path anchored at the scan root:\n{stdout}"
    );

    let actual: serde_json::Value =
        serde_json::from_str(&stdout).expect("scanner output was not valid JSON");
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/llm-mocked-api/__golden__.json"))
            .expect("golden fixture was not valid JSON");

    assert_eq!(
        actual, golden,
        "Full-pipeline output drifted from the golden while the LLM input is \
         frozen — this is a scanner-code regression. If the change is \
         intentional, re-record the golden (see the module doc-comment)."
    );
}
