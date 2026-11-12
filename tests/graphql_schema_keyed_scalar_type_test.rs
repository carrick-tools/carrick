//! Placing a root field from the service's schema moves where the field is
//! served, never what type it serves (carrick#1295).
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/graphql-schema-keyed-scalar-type/` with the type sidecar
//! LIVE. `shipping-api` builds its schema in code and prints it to
//! `dist/schema.graphql`. Its `scheduleShipment` field is defined in
//! `src/graphql/shipments.ts`, whose resolver returns a record with a `Date`
//! property; the schema serves that property as the `DateTime` scalar, which
//! `kit.ts` serialises to an ISO string. No model answer names a resolver for
//! the field. `shipping-web` executes the mutation and reads `scheduledDate`
//! as a `string`, which is what the wire carries.
//!
//! Answer key, read by hand from the fixture:
//!
//! | row | served at | producer type anchor | verdict |
//! |---|---|---|---|
//! | `mutation scheduleShipment` | `shipments.ts:13` (the schema-keyed reader) | the schema line, as with no reader | unverifiable, as with no reader |
//!
//! The resolver's in-process return type is not the field's wire type: read
//! as the producer type, its `Date` can judge the client's `string`
//! incompatible, a break the wire does not have. Whether a GraphQL producer
//! type may ever come from the resolver, through the scalar's wire mapping,
//! is carrick#1875, not this placement. The schema alone gives this field no
//! type the capture can read, so the verdict stays unverifiable, as it is
//! with no reader. Read from the code at the field's property (the stub's
//! `deterministic-infer` anchor), the capture's JSON wire transform turns the
//! `Date` into a string and the verdict moves to compatible here; on a
//! resolver whose `Date` it does not transform it moves to incompatible.

use std::path::PathBuf;
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/graphql-schema-keyed-scalar-type")
}

/// Every service's blob from one offline scan of the fixture.
fn scan() -> Vec<serde_json::Value> {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(fixture_dir())
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
        // Blobs only: a type-reach record sits beside each (carrick#3101).
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
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

fn relative(file: &str) -> String {
    let root = fixture_dir().display().to_string();
    file.strip_prefix(&root)
        .unwrap_or(file)
        .trim_start_matches('/')
        .to_string()
}

/// `file:line` of the producer row for `mutation scheduleShipment`.
fn served_at(blobs: &[serde_json::Value]) -> String {
    let row = blob(blobs, "shipping-api")["endpoints"]
        .as_array()
        .expect("endpoints")
        .iter()
        .find(|row| {
            row["key"]["protocol"] == "graphql" && row["key"]["field"] == "scheduleShipment"
        })
        .expect("a producer row for scheduleShipment");
    // A row's `file_path` carries its line.
    relative(row["file_path"].as_str().unwrap_or_default())
}

/// The files the producer's response type entries for `scheduleShipment`
/// are anchored in.
fn producer_type_files(blobs: &[serde_json::Value]) -> Vec<String> {
    blob(blobs, "shipping-api")["type_manifest"]
        .as_array()
        .expect("type_manifest")
        .iter()
        .filter(|entry| {
            entry["protocol"] == "graphql"
                && entry["role"] == "producer"
                && entry["type_kind"] == "response"
                && entry["field"] == "scheduleShipment"
        })
        .map(|entry| relative(entry["file_path"].as_str().unwrap_or_default()))
        .collect()
}

/// How the type capture anchored the producer's response type for
/// `scheduleShipment`: the `anchor_origin` its stub manifest records for
/// the alias, `None` when the stub holds no row for it.
fn producer_anchor_origin(blobs: &[serde_json::Value]) -> Option<String> {
    let api = blob(blobs, "shipping-api");
    let alias = api["type_manifest"]
        .as_array()
        .expect("type_manifest")
        .iter()
        .find(|entry| {
            entry["protocol"] == "graphql"
                && entry["role"] == "producer"
                && entry["type_kind"] == "response"
                && entry["field"] == "scheduleShipment"
        })
        .expect("a producer response entry")["type_alias"]
        .clone();
    let manifest: serde_json::Value = serde_json::from_str(
        api["capture_stub"]["files"]["carrick-manifest.json"]
            .as_str()
            .expect("the stub's manifest"),
    )
    .expect("parse the stub's manifest");
    manifest["aliases"]
        .as_array()
        .expect("aliases")
        .iter()
        .find(|row| row["alias"] == alias)
        .and_then(|row| row["anchor_origin"].as_str())
        .map(str::to_string)
}

/// The stored response verdict on the client's pair for the mutation.
fn verdict(blobs: &[serde_json::Value]) -> serde_json::Value {
    blob(blobs, "shipping-web")["compat_verdicts"]
        .as_array()
        .expect("compat_verdicts")
        .iter()
        .find(|verdict| verdict["consumer_key"] == "graphql|mutation|scheduleShipment")
        .unwrap_or_else(|| panic!("no verdict for scheduleShipment"))["response"]
        .clone()
}

#[test]
fn a_schema_keyed_field_keeps_its_schema_type_anchor() {
    let blobs = scan();

    // Where: the schema-keyed reader serves the field at its property.
    assert_eq!(served_at(&blobs), "apps/api/src/graphql/shipments.ts:13");

    // What: the producer type is anchored where it was before the reader
    // placed the field, not at the resolver's in-process return type.
    let files = producer_type_files(&blobs);
    assert!(
        !files.is_empty(),
        "the field keeps a producer response entry"
    );
    assert!(
        files
            .iter()
            .all(|file| file == "apps/api/dist/schema.graphql"),
        "the producer type stays at the schema: {files:?}"
    );
    assert_ne!(
        producer_anchor_origin(&blobs).as_deref(),
        Some("deterministic-infer"),
        "the producer type is not inferred from the code at the field's property"
    );
    let response = verdict(&blobs);
    assert_ne!(
        response["verdict"], "incompatible",
        "a DateTime the server sends as a string is not a break: {response}"
    );
    assert_eq!(
        response["verdict"], "unverifiable",
        "the verdict is the one the schema alone gives: {response}"
    );
}
