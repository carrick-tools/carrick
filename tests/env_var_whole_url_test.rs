//! carrick#572: a request whose whole URL is read from an environment variable
//! is recorded, instead of vanishing for want of a route shape.
//!
//! Deterministic end to end. The LLM is replayed from `__llm__/`, and the
//! cassette holds what extraction can honestly say about `fetch(url, …)`: the
//! binding, and nothing else. A bare identifier is not route-shaped, so before
//! this change the call was dropped before it became a row of any kind — not a
//! matched edge, not an unmatched call, not an egress candidate — and the
//! assertion below fails on the pre-fix scanner.
//!
//! See `tests/fixtures/env-var-whole-url/README.md` for the shape.

use std::process::Command;

fn calls() -> Vec<serde_json::Value> {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/env-var-whole-url");
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
fn a_whole_url_read_from_an_env_var_is_recorded() {
    let calls = calls();
    assert_eq!(calls.len(), 8, "one row per call site: {calls:#?}");

    let ask = call_at(&calls, "src/helpdesk.ts", 7);
    assert_eq!(ask["method"], "POST");
    assert_eq!(
        ask["target_url"], "${process.env.HELPDESK_URL}/api/answer",
        "the env var supplies the origin and the fallback literal supplies the path"
    );
    assert_eq!(
        ask["path"], "/api/answer",
        "the loopback default states the origin, so the key is the route it requests"
    );

    // The base-plus-path shape that already resolved must be untouched: the
    // whole-URL rule fires only where the target states no path of its own.
    let items = call_at(&calls, "src/helpdesk.ts", 20);
    assert_eq!(items["method"], "GET");
    assert_eq!(
        items["target_url"],
        "${process.env.CATALOG_URL}/api/v1/items"
    );
    assert_eq!(
        items["path"], "${process.env.CATALOG_URL}/api/v1/items",
        "an undeclared env-var BASE is still kept verbatim: the whole-URL rule \
         reads the fallback, and this target states its own path"
    );
}

/// carrick#632: the same URL at a site the extraction returns no row for. The
/// resolution above is a rewrite, so it had nothing to act on and the call was
/// absent from the index entirely, not merely wrong.
#[test]
fn a_whole_url_site_the_extraction_answered_nothing_for_is_recorded() {
    let calls = calls();

    let ask = call_at(&calls, "src/toolset.ts", 12);
    assert_eq!(
        ask["method"], "POST",
        "the method is the literal in the call's own options bag"
    );
    assert_eq!(
        ask["target_url"], "${process.env.SERVICE_ASK_URL}/api/ask",
        "the env var supplies the origin and the fallback literal supplies the path"
    );
    assert_eq!(ask["path"], "/api/ask");
}

/// carrick#632 (the live shape): the same call at a site extraction DID answer,
/// paraphrasing the binding as the bare env-var name. #633 read that row as
/// covering the site and emitted nothing, so the call stayed keyed on an
/// env-var origin and `get_operation(POST /api/ask)` could not find it.
#[test]
fn a_whole_url_site_the_extraction_answered_wrongly_is_corrected() {
    let calls = calls();

    let ask = call_at(&calls, "src/answered.ts", 11);
    assert_eq!(ask["method"], "POST");
    assert_eq!(
        ask["target_url"], "${process.env.SUPPORT_ASK_URL}/api/ask",
        "the analyzer's paraphrase is corrected to what the binding states"
    );
    assert_eq!(
        ask["path"], "/api/ask",
        "the call is findable under the route it requests, the way the same \
         fetch against the fallback literal already was"
    );
}

/// carrick#649 gap 1: every persisted row says how its base RESOLVES.
///
/// The rows below have the same shape as each other on `path` and `target_url`
/// and different truths about where they go, which is the whole reason the
/// field exists. Nothing here may change a `path`: the assertions above are the
/// regression check that the canonical key is untouched.
#[test]
fn every_call_states_how_its_base_resolves() {
    let calls = calls();

    // An environment variable with a LOOPBACK default: the source says the
    // unconfigured request stays on this machine.
    let ask = call_at(&calls, "src/helpdesk.ts", 7);
    assert_eq!(
        ask["base"],
        serde_json::json!({
            "written": "${process.env.HELPDESK_URL}",
            "kind": "env",
            "env_var": "HELPDESK_URL",
            "fallback": "http://localhost:7100/api/answer",
            "fallback_is_loopback": true,
        })
    );

    // The same shape whose fallback carries no path at all — the whole-URL map
    // drops it, and the base still states it.
    let items = call_at(&calls, "src/helpdesk.ts", 20);
    assert_eq!(
        items["base"],
        serde_json::json!({
            "written": "${process.env.CATALOG_URL}",
            "kind": "env",
            "env_var": "CATALOG_URL",
            "fallback": "http://localhost:4001",
            "fallback_is_loopback": true,
        })
    );

    // An environment variable whose default names a THIRD PARTY, and which the
    // repo's schema declares with a default — so it is never absent.
    let quote = call_at(&calls, "src/ledger.ts", 31);
    assert_eq!(
        quote["base"],
        serde_json::json!({
            "written": "${process.env.GATEWAY_URL}",
            "kind": "env",
            "env_var": "GATEWAY_URL",
            "fallback": "https://api.example.com/v1/quote",
            "fallback_is_loopback": false,
            "declared_optional": false,
        }),
        "the `??` literal is what the call falls back to; the schema's own \
         default is the second place the source could have said it"
    );

    // An environment variable the repo's schema declares OPTIONAL with no
    // default, in a file that makes no call of its own.
    let knowledge = call_at(&calls, "src/ledger.ts", 40);
    assert_eq!(
        knowledge["base"],
        serde_json::json!({
            "written": "${process.env.KNOWLEDGE_URL}",
            "kind": "env",
            "env_var": "KNOWLEDGE_URL",
            "fallback_is_loopback": false,
            "declared_optional": true,
        }),
        "the declaration is a repo-wide fact, read from a config file with no \
         route and no call"
    );

    // A base handed in as an option: the expression is all the scanner sees.
    let lookup = call_at(&calls, "src/ledger.ts", 12);
    assert_eq!(
        lookup["base"],
        serde_json::json!({
            "written": "${this.opts.lookupUrl}",
            "kind": "injected",
            "fallback_is_loopback": false,
        })
    );

    // A bare relative path states no base at all.
    let entries = call_at(&calls, "src/ledger.ts", 22);
    assert_eq!(
        entries["base"],
        serde_json::json!({
            "written": "",
            "kind": "relative",
            "fallback_is_loopback": false,
        })
    );
}

/// carrick#649 gap 2: a manifest anchor states where the type is DECLARED, not
/// only where the operation was extracted.
///
/// `LedgerEntry` is imported at the call site, so the op's own `file`/`line`
/// point at the `fetch`. Answering "where is this type defined" from those is
/// how an agent gets it wrong.
#[test]
fn an_imported_anchor_states_its_declaring_file_and_line() {
    let calls = calls();

    let lookup = call_at(&calls, "src/ledger.ts", 12);
    assert_eq!(lookup["primary_type_symbol"], "LedgerEntry");
    assert_eq!(lookup["file"], "src/ledger.ts");
    assert_eq!(lookup["line"], 12);
    assert_eq!(
        lookup["defined_in"],
        serde_json::json!({
            "file_path": "src/types.ts",
            "line_number": 4,
            "symbol": "LedgerEntry",
        }),
        "the import the file wrote, resolved and confirmed against the \
         declaring file's own AST"
    );

    // A call with no anchor symbol has no declaration to state, and says so by
    // omitting the field rather than pointing at its own site.
    let entries = call_at(&calls, "src/ledger.ts", 22);
    assert_eq!(entries["defined_in"], serde_json::Value::Null);
}
