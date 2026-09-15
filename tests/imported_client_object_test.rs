//! carrick#1146, carrick#1151 (residual), carrick#1155: what a call site
//! reaches through an imported client object, and what extraction may say
//! about it.
//!
//! Deterministic end to end. The LLM is replayed from `__llm__/`, and the
//! cassettes stand in for the model's answers, including one invented path.
//! What is under test is everything around the model: which call sites are
//! offered, what the analyzer is handed for them, which model rows survive the
//! evidence gate, and the name each row is written through.
//!
//! See `tests/fixtures/imported-client-object/README.md` for the shape.

use std::path::PathBuf;
use std::process::Command;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/imported-client-object")
}

struct Scan {
    calls: Vec<serde_json::Value>,
    /// The user message each analyzed file was sent, by repo-relative path.
    messages: Vec<(String, String)>,
}

fn scan() -> Scan {
    let fixture = fixture();
    let mock_dir = fixture.join("__llm__");
    let dump = tempfile::tempdir().expect("tempdir");
    let storage = tempfile::tempdir().expect("tempdir");

    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(&fixture)
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_MOCK_FIXTURE_DIR", &mock_dir)
        .env("CARRICK_OUTPUT_JSON", "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_EVAL_DUMP_DIR", dump.path())
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
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

    let root = fixture.canonicalize().expect("fixture exists");
    let mut messages = Vec::new();
    for entry in std::fs::read_dir(dump.path()).expect("dump dir") {
        let artifact: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(entry.expect("entry").path()).expect("artifact"),
        )
        .expect("artifact is JSON");
        let path = PathBuf::from(artifact["file_path"].as_str().expect("file_path"));
        let relative = path
            .canonicalize()
            .ok()
            .and_then(|p| {
                p.strip_prefix(&root)
                    .ok()
                    .map(|p| p.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| path.to_string_lossy().to_string());
        messages.push((
            relative,
            artifact["request_user_message"]
                .as_str()
                .expect("request_user_message")
                .to_string(),
        ));
    }
    messages.sort();

    Scan {
        calls: projection["calls"]
            .as_array()
            .expect("projection carries a calls array")
            .clone(),
        messages,
    }
}

fn message<'a>(scan: &'a Scan, file: &str) -> &'a str {
    scan.messages
        .iter()
        .find(|(path, _)| path == file)
        .map(|(_, message)| message.as_str())
        .unwrap_or_else(|| {
            panic!(
                "{file} was not sent to the analyzer; sent: {:?}",
                scan.messages.iter().map(|(p, _)| p).collect::<Vec<_>>()
            )
        })
}

#[test]
fn an_object_member_called_through_an_alias_import_is_offered_with_its_body() {
    let scan = scan();
    // The page raises no candidate by any name heuristic (`shelves.rename`
    // is neither an API-ish receiver nor an HTTP verb); it is analyzed only
    // because what it calls issues a request.
    let page = message(&scan, "src/pages/shelf-page.tsx");
    for site in [
        "Line 5 (span 169-217) shelves.rename",
        "Line 9 (span 284-308) shelves.archive",
    ] {
        assert!(page.contains(site), "{site} not offered:\n{page}");
    }

    let facts = page
        .split("### IMPORTED HTTP WRAPPER DEFINITIONS")
        .nth(1)
        .and_then(|rest| rest.split("### FILE CONTENT").next())
        .expect("the page is handed the declarations it calls");
    // The member bodies sit past the first 4 KB of their module, which is all
    // the analyzer used to be handed, and behind a tsconfig alias, which it
    // used not to follow at all.
    assert!(
        facts.contains("`/v2/shelves/${shelfId}/label`"),
        "the called member's body:\n{facts}"
    );
    assert!(facts.contains("`/v2/shelves/${shelfId}/archive`"));
    assert!(
        facts.contains("${settings.gatewayUrl}${path}"),
        "the helper the member calls, from its own module:\n{facts}"
    );
    assert!(facts.contains("import { settings } from \"./settings\";"));
    assert!(
        facts.contains("gatewayUrl: process.env.SHELF_API_URL"),
        "the declaration of the binding the helper reads, one hop further:\n{facts}"
    );
    // Only what the page reaches: not the members it never calls, and not the
    // module's type declarations.
    assert!(!facts.contains("\"/v2/shelves\")"), "{facts}");
    assert!(!facts.contains("interface CountSession"), "{facts}");
    let member_module = facts.find("--- module: src/lib/shelves.ts ---").unwrap();
    let helper_module = facts.find("--- module: src/lib/http.ts ---").unwrap();
    assert!(
        member_module < helper_module,
        "what the file calls comes first:\n{facts}"
    );
}

#[test]
fn a_model_row_is_kept_only_when_its_path_is_written_somewhere_it_read() {
    let scan = scan();
    let mut page_rows: Vec<(i64, String, String)> = scan
        .calls
        .iter()
        .filter(|call| call["file"] == "src/pages/shelf-page.tsx")
        .map(|call| {
            (
                call["line"].as_i64().expect("line"),
                call["method"].as_str().expect("method").to_string(),
                call["target_url"].as_str().expect("target").to_string(),
            )
        })
        .collect();
    page_rows.sort();
    assert_eq!(
        page_rows,
        vec![(
            5,
            "PATCH".to_string(),
            "${process.env.SHELF_API_URL}/v2/shelves/${shelfId}/label".to_string()
        )],
        "the archive row names `/shelf/` and `/archived`, which nothing the model \
         read contains, so it is dropped: {:#?}",
        scan.calls
    );
}

/// The base is a fact of the helper's source, so every row through `sendJson`
/// is served the base `sendJson` reads, resolved the way `sendJson`'s own
/// module resolves it, whatever the model spelled (the stock cassette writes
/// `${gatewayUrl}`, a name that exists nowhere).
#[test]
fn a_row_through_an_imported_helper_is_served_the_helpers_base() {
    let scan = scan();
    let mut rows: Vec<(String, i64, String, String)> = scan
        .calls
        .iter()
        .filter(|call| call["file"] != "src/lib/ledger.ts")
        .map(|call| {
            (
                call["file"].as_str().expect("file").to_string(),
                call["line"].as_i64().expect("line"),
                call["base"]["env_var"]
                    .as_str()
                    .unwrap_or("<none>")
                    .to_string(),
                call["target_url"].as_str().expect("target").to_string(),
            )
        })
        .collect();
    rows.sort();
    assert_eq!(
        rows,
        vec![
            (
                "src/lib/shelves.ts".to_string(),
                163,
                "SHELF_API_URL".to_string(),
                "${process.env.SHELF_API_URL}/v2/shelves/${shelfId}/label".to_string()
            ),
            (
                "src/pages/shelf-page.tsx".to_string(),
                5,
                "SHELF_API_URL".to_string(),
                "${process.env.SHELF_API_URL}/v2/shelves/${shelfId}/label".to_string()
            ),
            (
                "src/pages/stock.ts".to_string(),
                4,
                "SHELF_API_URL".to_string(),
                "${process.env.SHELF_API_URL}/v2/stock".to_string()
            ),
        ],
        "{:#?}",
        scan.calls
    );
}

#[test]
fn every_row_through_a_project_helper_names_that_helper() {
    let scan = scan();
    let mut rows: Vec<(String, i64, String, String)> = scan
        .calls
        .iter()
        .map(|call| {
            (
                call["file"].as_str().expect("file").to_string(),
                call["line"].as_i64().expect("line"),
                call["handler"].as_str().expect("handler").to_string(),
                call["resolution_source"]
                    .as_str()
                    .expect("source")
                    .to_string(),
            )
        })
        .collect();
    rows.sort();
    assert_eq!(
        rows,
        vec![
            // The residual same-file site (carrick#1151): the path is picked
            // by a switch, so the wrapper pass offers the site to the model
            // rather than resolving it, and the row carries a span of its own.
            (
                "src/lib/ledger.ts".to_string(),
                16,
                "ledgerRequest".to_string(),
                "model".to_string()
            ),
            (
                "src/lib/ledger.ts".to_string(),
                20,
                "ledgerRequest".to_string(),
                "same_file_wrapper".to_string()
            ),
            // Extraction wrote `sendJson` here and `sendJson(` below; both
            // are written through the same project function.
            (
                "src/lib/shelves.ts".to_string(),
                163,
                "sendJson".to_string(),
                "model".to_string()
            ),
            // A member call names no bare function the index can check, so
            // what extraction wrote stays.
            (
                "src/pages/shelf-page.tsx".to_string(),
                5,
                "shelves.rename".to_string(),
                "model".to_string()
            ),
            (
                "src/pages/stock.ts".to_string(),
                4,
                "sendJson".to_string(),
                "model".to_string()
            ),
        ],
        "{:#?}",
        scan.calls
    );
}

#[test]
fn a_same_file_site_the_fixpoint_cannot_state_is_offered() {
    let scan = scan();
    let ledger = message(&scan, "src/lib/ledger.ts");
    assert!(
        ledger.contains("Line 16 (span 348-380) ledgerRequest [fn: loadEntries]"),
        "{ledger}"
    );
    // A site the fixpoint resolved is a row already, not an offer.
    assert!(!ledger.contains("Line 20 ("), "{ledger}");
}
