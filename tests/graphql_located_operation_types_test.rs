//! A GraphQL consumer row whose located result type is the result of the
//! row's whole operation serves the field's payload, not the operation
//! wrapper around it (carrick#1760).
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/graphql-located-operation-types/` with the type sidecar
//! LIVE (no `CARRICK_ALLOW_MISSING_TYPES`, so a sidecar that cannot start
//! fails the test instead of reading as a missing type). `billing-api` serves
//! a schema-first API whose resolvers state their return types. `billing-web`
//! executes documents whose declarations state no result type, so every
//! consumer type comes from the model's locate:
//!
//! - `InvoicePage.tsx` executes a compiled document selecting two root
//!   fields, `invoice` and `settings`. Its cassette locates
//!   `InvoicePageQuery` for both: the OPERATION's result
//!   (`{ __typename: 'Query', invoice?: ..., settings: ... }`).
//! - `CustomerPage.tsx` sends a `gql` template written in the page, and its
//!   cassette locates `CustomerData`, an interface the page declares with
//!   one property, the operation's one root field.
//! - `FolderPage.tsx` executes a document selecting `folder`, whose payload
//!   carries its parent folder under the same name. Its cassette locates
//!   `FolderView`, the field's payload.
//!
//! Answer key, read by hand from the fixture:
//!
//! | row | located | served type | verdict |
//! |---|---|---|---|
//! | `query invoice` (`InvoicePage.tsx:6`) | `InvoicePageQuery`, the operation's result | its `invoice` property: `{ __typename: 'Invoice', id, total } \| null` | compatible with the resolver's `Invoice \| null` |
//! | `query settings` (`InvoicePage.tsx:6`) | `InvoicePageQuery`, the operation's result | its `settings` property: `{ __typename: 'Settings', prefix }` | compatible with `Settings` |
//! | `query customer` (`CustomerPage.tsx`) | `CustomerData`, the operation's result | its `customer` property: `{ id, name } \| null` | compatible with `Customer \| null` |
//! | `query folder` (`FolderPage.tsx:6`) | `FolderView`, the field's payload | `FolderView` whole, its parent `folder` inside it | none to judge: no resolver |
//!
//! Served whole, the operation's result would read incompatible against each
//! resolver's field-level return, which is what the verdicts rule out.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graphql-located-operation-types")
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
/// response entry for `key` (`kind|field`) in `file`.
fn consumer_type(
    blobs: &[serde_json::Value],
    key: &str,
    file: &str,
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
                && entry["file_path"] == file
        })
        .unwrap_or_else(|| panic!("no consumer response entry for {key} in {file}"));
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

/// No served text may be the operation's result: the `Query` root type
/// named, or one row's root field a member of another's type.
fn assert_field_level(definition: &str, root_fields: &[&str]) {
    assert!(
        !definition.contains("'Query'") && !definition.contains("\"Query\""),
        "the row serves the field's payload, not the operation's result: {definition}"
    );
    for field in root_fields {
        assert!(
            !definition.contains(&format!("{field}:"))
                && !definition.contains(&format!("{field}?:")),
            "the operation's `{field}` property is not in the row's type: {definition}"
        );
    }
}

#[test]
fn a_located_operation_result_serves_each_rows_field() {
    let blobs = scan(&fixture_dir());
    let page = "apps/web/src/pages/InvoicePage.tsx";

    let (state, symbol, definition) = consumer_type(&blobs, "query|invoice", page);
    assert_ne!(state, "unknown", "the field's type resolves: {definition}");
    assert_eq!(
        symbol, None,
        "the operation's result is not the row's type, so the row names no symbol for it"
    );
    assert_field_level(&definition, &["invoice", "settings"]);
    assert!(
        definition.contains("\"Invoice\"")
            && definition.contains("total: number")
            && definition.contains("null"),
        "the row serves the `invoice` property, nullable as written: {definition}"
    );
    assert_eq!(
        verdict(&blobs, "graphql|query|invoice"),
        "compatible",
        "the field-level consumer type agrees with the resolver's return"
    );

    let (state, symbol, definition) = consumer_type(&blobs, "query|settings", page);
    assert_ne!(state, "unknown", "the field's type resolves: {definition}");
    assert_eq!(symbol, None);
    assert_field_level(&definition, &["invoice", "settings"]);
    assert!(
        definition.contains("\"Settings\"") && definition.contains("prefix: string"),
        "the row serves the `settings` property: {definition}"
    );
    assert_eq!(verdict(&blobs, "graphql|query|settings"), "compatible");
}

/// An interface the page declares, with one property for the operation's
/// one root field, is that operation's result too.
#[test]
fn a_located_interface_for_the_operation_serves_the_field() {
    let blobs = scan(&fixture_dir());

    let (state, symbol, definition) = consumer_type(
        &blobs,
        "query|customer",
        "apps/web/src/pages/CustomerPage.tsx",
    );
    assert_ne!(state, "unknown", "the field's type resolves: {definition}");
    assert_eq!(symbol, None);
    assert_field_level(&definition, &["customer"]);
    assert!(
        definition.contains("name: string") && definition.contains("null"),
        "the row serves the `customer` property: {definition}"
    );
    assert_eq!(verdict(&blobs, "graphql|query|customer"), "compatible");
}

/// A located payload type is served as it is, though it carries a member
/// named like the field: its own fields are not root fields.
#[test]
fn a_located_payload_type_is_served_whole() {
    let blobs = scan(&fixture_dir());

    let (state, symbol, definition) =
        consumer_type(&blobs, "query|folder", "apps/web/src/pages/FolderPage.tsx");
    assert_eq!(symbol.as_deref(), Some("FolderView"));
    assert_ne!(state, "unknown", "the located type resolves: {definition}");
    assert!(
        definition.contains("name: string") && definition.contains("folder"),
        "the row serves the payload, its parent folder inside it: {definition}"
    );
}
