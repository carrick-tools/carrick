//! A GraphQL consumer row at a call that executes a typed document serves the
//! field's result type its declaration states (carrick#1761).
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/graphql-declared-result-types/` with the type sidecar LIVE
//! (no `CARRICK_ALLOW_MISSING_TYPES`, so a sidecar that cannot start fails the
//! test instead of reading as a missing type). `billing-api` serves a
//! schema-first API whose resolvers state their return types. `billing-web`
//! executes documents compiled into `src/generated/graphql.ts`, a module other
//! than the pages that execute them. Each compiled declaration is asserted to
//! a document type whose first argument is the operation's result type
//! (`{ __typename: 'Query', invoice?: { ... } | null }`), except `NoteDocument`,
//! asserted to the document type with no arguments.
//!
//! The pages' cassettes answer one REST call each and locate two result
//! types: `InvoiceQuery` for `invoice`, the OPERATION's result (a wrapper
//! around the field), and `NoteView` for `note`, which the declaration does
//! not type.
//!
//! Answer key, read by hand from the fixture:
//!
//! | row | anchor | served type | verdict |
//! |---|---|---|---|
//! | `query invoice` (`InvoicePage.tsx:6`) | the declaration (the locate is ignored) | the `invoice` property: `{ __typename: 'Invoice', id, total, dueAt? } \| null` | compatible with the resolver's `Invoice \| null` |
//! | `mutation sendInvoice` (`InvoicePage.tsx:7`) | the declaration, no locate | `{ __typename: 'Invoice', id: string }` | compatible with `Invoice` |
//! | `query note` (`NotePage.tsx:6`) | the located `NoteView` (carrick#1728) | `{ id: string; body: string }` | none to judge: no resolver |
//!
//! An operation-level type under the field-keyed `invoice` row would read
//! incompatible against the resolver's field-level return (carrick#1760),
//! which is what the verdicts rule out.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graphql-declared-result-types")
}

/// Every service's blob from one offline scan of `repo`.
fn scan(repo: &Path) -> Vec<serde_json::Value> {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", fixture_dir().join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env_remove("CARRICK_ALLOW_MISSING_TYPES");
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
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read_dir(storage.path())
        .expect("storage dir")
        .flatten()
        .map(|entry| {
            serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
                .expect("parse blob")
        })
        .collect()
}

fn blob<'a>(blobs: &'a [serde_json::Value], service: &str) -> &'a serde_json::Value {
    blobs
        .iter()
        .find(|blob| blob["service_name"] == service)
        .unwrap_or_else(|| panic!("no blob for {service}"))
}

/// `(type_state, primary_type_symbol, expanded_definition)` of the consumer
/// response entry for `key` (`kind|field`) at `file:line`.
fn consumer_type(
    blobs: &[serde_json::Value],
    key: &str,
    site: (&str, u64),
) -> (String, Option<String>, String) {
    let entry = blob(blobs, "billing-web")["type_manifest"]
        .as_array()
        .expect("type_manifest")
        .iter()
        .find(|entry| {
            entry["protocol"] == "graphql"
                && entry["role"] == "consumer"
                && entry["type_kind"] == "response"
                && format!(
                    "{}|{}",
                    entry["kind"].as_str().unwrap_or_default(),
                    entry["field"].as_str().unwrap_or_default()
                ) == key
                && entry["file_path"] == site.0
                && entry["line_number"] == site.1
        })
        .unwrap_or_else(|| panic!("no consumer response entry for {key} at {site:?}"));
    (
        entry["type_state"].as_str().unwrap_or_default().to_string(),
        entry["primary_type_symbol"].as_str().map(str::to_string),
        entry["expanded_definition"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

/// The stored response verdict on the consumer's pair for `key`
/// (`graphql|kind|field`).
fn verdict(blobs: &[serde_json::Value], key: &str) -> String {
    blob(blobs, "billing-web")["compat_verdicts"]
        .as_array()
        .expect("compat_verdicts")
        .iter()
        .find(|verdict| verdict["consumer_key"] == key)
        .unwrap_or_else(|| panic!("no verdict for {key}"))["response"]["verdict"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn a_typed_documents_declaration_types_the_rows_at_its_calls_at_field_level() {
    let blobs = scan(&fixture_dir());

    // The declaration types the row, and wins over the operation-level type
    // the page's cassette locates.
    let (state, symbol, definition) = consumer_type(
        &blobs,
        "query|invoice",
        ("apps/web/src/pages/InvoicePage.tsx", 6),
    );
    assert_ne!(state, "unknown", "the declared type resolves: {definition}");
    assert_ne!(
        symbol.as_deref(),
        Some("InvoiceQuery"),
        "the ignored locate does not name the row's type either"
    );
    assert!(
        definition.contains("total: number")
            && definition.contains("null")
            && !definition.contains("invoice")
            && !definition.contains("'Query'")
            && !definition.contains("\"Query\""),
        "the row serves the field's payload, nullable as declared, not the operation result: \
         {definition}"
    );
    assert_eq!(
        verdict(&blobs, "graphql|query|invoice"),
        "compatible",
        "the field-level consumer type agrees with the resolver's return"
    );

    // No locate at all: only the declaration types this row.
    let (state, _, definition) = consumer_type(
        &blobs,
        "mutation|sendInvoice",
        ("apps/web/src/pages/InvoicePage.tsx", 7),
    );
    assert_ne!(state, "unknown", "the declared type resolves: {definition}");
    assert!(
        definition.contains("id: string") && !definition.contains("sendInvoice"),
        "the row serves the field's payload: {definition}"
    );
    assert_eq!(
        verdict(&blobs, "graphql|mutation|sendInvoice"),
        "compatible"
    );
}

/// carrick#1728: the model locates a result type for the file it reads, the
/// file that executes the document. A declaration that states no result type
/// leaves the row to that locate, which joins at the call.
#[test]
fn a_type_located_where_an_untyped_document_is_executed_types_the_row_at_the_call() {
    let blobs = scan(&fixture_dir());

    let (state, symbol, definition) =
        consumer_type(&blobs, "query|note", ("apps/web/src/pages/NotePage.tsx", 6));
    assert_eq!(
        symbol.as_deref(),
        Some("NoteView"),
        "the type located in the page anchors the row at the page's hook"
    );
    assert_ne!(state, "unknown", "the located type resolves");
    assert!(
        definition.contains("body: string"),
        "the located type is served: {definition}"
    );
}
