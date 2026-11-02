//! A GraphQL operation that selects a field its served schema does not
//! declare breaks, whatever the two sides' types say.
//!
//! Drives the real scanner binary offline, with the type sidecar LIVE (no
//! `CARRICK_ALLOW_MISSING_TYPES`), over two fixtures that differ in one thing.
//! In both, `parcel-api` serves `schema.graphql` and its `parcel` resolver
//! returns a record that still carries `etaDays`; `parcel-web` executes
//! compiled documents whose `Parcel` operation selects `etaDays`, and whose
//! generated result type marks it optional, as generated client types mark
//! every nullable field. So the type check reads the pair compatible in both:
//! the record has the field, and the consumer does not require it.
//!
//! - `graphql-undeclared-selection/`: the schema file is the whole schema
//!   served, and it does not declare `Parcel.etaDays`. A server rejects the
//!   operation, so the pair is incompatible.
//! - `graphql-undeclared-selection-partial/`: the server adds
//!   `extend type Parcel { etaDays: Int }` from a source file when it builds
//!   the schema, so the operation is valid at runtime. The scan reads the
//!   schema file as possibly partial and the pair is unverifiable, never
//!   incompatible.
//! - `graphql-undeclared-selection-printed/`: the server builds its schema in
//!   code, and the schema the scan reads is the copy printed from it, named in
//!   `graphqlSchemas`. The code declares `etaDays` and the stale printed copy
//!   does not, so the operation is valid at runtime; a printed copy is never
//!   read as the whole schema, and the pair is unverifiable.
//!
//! - `graphql-undeclared-selection-printed-undeclared/`: the same server, with
//!   the printed copy committed in a folder the service walks and no
//!   `graphqlSchemas` entry. The scan finds the service builds its schema in
//!   code, so the copy it serves is read as possibly stale all the same
//!   (carrick#3200).
//!
//! In all four, the `Depot` operation selects only declared fields, one of them
//! through a fragment the compiled document carries, and stays compatible.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
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
            format!("{}/", repo.join("__llm__").display()),
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
        // Blobs only: a type-reach record sits beside each (carrick#3101).
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .map(|entry| {
            serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
                .expect("parse blob")
        })
        .collect()
}

fn web(blobs: &[serde_json::Value]) -> &serde_json::Value {
    blobs
        .iter()
        .find(|blob| blob["service_name"] == "parcel-web")
        .expect("no blob for parcel-web")
}

/// The GraphQL call row for root field `field` at `page`.
fn call<'a>(blobs: &'a [serde_json::Value], field: &str, page: &str) -> &'a serde_json::Value {
    web(blobs)["calls"]
        .as_array()
        .expect("calls")
        .iter()
        .find(|row| {
            row["key"]["protocol"] == "graphql"
                && row["key"]["field"] == field
                && row["file_path"]
                    .as_str()
                    .is_some_and(|path| path.starts_with(page))
        })
        .unwrap_or_else(|| panic!("no GraphQL call row for {field} at {page}"))
}

/// The stored response verdict on the consumer's pair for `key`
/// (`graphql|kind|field`).
fn verdict(blobs: &[serde_json::Value], key: &str) -> serde_json::Value {
    web(blobs)["compat_verdicts"]
        .as_array()
        .expect("compat_verdicts")
        .iter()
        .find(|verdict| verdict["consumer_key"] == key)
        .unwrap_or_else(|| panic!("no verdict for {key}"))["response"]
        .clone()
}

#[test]
fn a_field_the_whole_served_schema_does_not_declare_breaks_the_pair() {
    let blobs = scan(&fixture_dir("graphql-undeclared-selection"));

    let parcel = call(&blobs, "parcel", "apps/web/src/pages/ParcelPage.tsx");
    assert_eq!(
        parcel["undeclared_selection"],
        serde_json::json!({
            "fields": [{ "on_type": "Parcel", "field": "etaDays" }],
            "schema": "apps/api/src/schema.graphql"
        }),
        "the row names the undeclared field and the schema, with no doubt: {parcel:#}"
    );
    let response = verdict(&blobs, "graphql|query|parcel");
    assert_eq!(
        response["verdict"], "incompatible",
        "a server rejects the operation, whatever the record and the optional \
         consumer member say: {response:#}"
    );
    assert_eq!(
        response["resolved"], true,
        "a fact, not a doubt: {response:#}"
    );

    let depot = call(&blobs, "depot", "apps/web/src/pages/DepotPage.tsx");
    assert!(
        depot.get("undeclared_selection").is_none(),
        "every field it selects, the fragment's included, is declared: {depot:#}"
    );
    assert_eq!(
        verdict(&blobs, "graphql|query|depot")["verdict"],
        "compatible"
    );
}

#[test]
fn a_schema_the_server_adds_to_at_runtime_leaves_the_pair_unverifiable() {
    let blobs = scan(&fixture_dir("graphql-undeclared-selection-partial"));

    let parcel = call(&blobs, "parcel", "apps/web/src/pages/ParcelPage.tsx");
    assert_eq!(
        parcel["undeclared_selection"]["partial"], "schema_text_in_source",
        "the extension in a source file makes the schema file possibly partial: {parcel:#}"
    );
    let response = verdict(&blobs, "graphql|query|parcel");
    assert_eq!(
        response["verdict"], "unverifiable",
        "the operation is valid against the schema the server builds; never \
         incompatible: {response:#}"
    );

    assert_eq!(
        verdict(&blobs, "graphql|query|depot")["verdict"],
        "compatible"
    );
}

#[test]
fn a_printed_code_first_schema_leaves_the_pair_unverifiable() {
    let blobs = scan(&fixture_dir("graphql-undeclared-selection-printed"));

    let parcel = call(&blobs, "parcel", "apps/web/src/pages/ParcelPage.tsx");
    assert_eq!(
        parcel["undeclared_selection"],
        serde_json::json!({
            "fields": [{ "on_type": "Parcel", "field": "etaDays" }],
            "schema": "apps/api/dist/schema.graphql",
            "partial": "printed_from_code"
        }),
        "the field is still recorded, with the printed copy as the doubt: {parcel:#}"
    );
    let response = verdict(&blobs, "graphql|query|parcel");
    assert_eq!(
        response["verdict"], "unverifiable",
        "the code that builds the schema may declare the field the printed copy \
         lacks; never incompatible: {response:#}"
    );

    assert_eq!(
        verdict(&blobs, "graphql|query|depot")["verdict"],
        "compatible"
    );
}

#[test]
fn a_printed_copy_in_a_walked_folder_of_a_code_first_server_leaves_the_pair_unverifiable() {
    let blobs = scan(&fixture_dir(
        "graphql-undeclared-selection-printed-undeclared",
    ));

    let parcel = call(&blobs, "parcel", "apps/web/src/pages/ParcelPage.tsx");
    assert_eq!(
        parcel["undeclared_selection"],
        serde_json::json!({
            "fields": [{ "on_type": "Parcel", "field": "etaDays" }],
            "schema": "apps/api/src/schema.graphql",
            "partial": "printed_from_code"
        }),
        "no setting names the file, but the service builds its schema in code: {parcel:#}"
    );
    let response = verdict(&blobs, "graphql|query|parcel");
    assert_eq!(
        response["verdict"], "unverifiable",
        "a stale printed copy must not read as a break: {response:#}"
    );

    assert_eq!(
        verdict(&blobs, "graphql|query|depot")["verdict"],
        "compatible"
    );
}
