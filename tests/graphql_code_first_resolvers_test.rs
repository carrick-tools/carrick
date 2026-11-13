//! A GraphQL schema built in code is served at its resolvers (carrick#1157).
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/graphql-code-first-resolvers/`: `parcels-api` builds its
//! schema with calls on one builder value, created from a package in
//! `src/graphql/kit.ts`. Two field modules export nothing, raise no HTTP
//! candidate, and import the builder, one relatively with a `.ts` extension and
//! one through the `@/` path alias. The printed schema sits in `dist/`, named
//! in `graphqlSchemas`. Two files are not schema modules:
//! `src/lib/retirement.ts` calls, at module scope, through a value from a
//! package detection does not name, and `src/lib/courier.ts` calls a detected
//! SDK client inside a request function.
//!
//! The framework-detect cassette names the builder's package and the SDK on
//! the `data_fetchers` channel. The analyze-file cassettes answer as the model
//! would for each file it is asked about, including a resolver claim in each of
//! the two files that are not schema modules: a claim lands only if its file is
//! wrongly admitted.
//!
//! Answer key, read by hand from the fixture:
//!
//! | field | resolved at | served at |
//! |---|---|---|
//! | `health` | `kit.ts:19` | `kit.ts:19` |
//! | `parcels` | `parcels/queries.ts:8` | `parcels/queries.ts:8` |
//! | `parcel` | `parcels/queries.ts:12` | `parcels/queries.ts:12` |
//! | `heaviestWeight` | `parcels/queries.ts:18` | `parcels/queries.ts:18` |
//! | `recentParcels` | `parcels/queries.ts:23` | `parcels/queries.ts:23` |
//! | `parcelCount` | `parcels/queries.ts:27` | `parcels/queries.ts:27` |
//! | `archivedParcels` | `parcels/queries.ts:30` | `parcels/queries.ts:30` |
//! | `pendingParcels` | `parcels/queries.ts:34` | `parcels/queries.ts:34` |
//! | `dispatchParcel` | `parcels/mutations.ts:5` | `parcels/mutations.ts:5` |
//! | `recallParcel` | nowhere admitted | `dist/schema.graphql:3` |
//! | `retireParcel` | nowhere admitted | `dist/schema.graphql:4` |
//!
//! Without the package in detection no file is admitted for its schema
//! fields, and a model answer can leave a field out. Either way the fields
//! are served where they are resolved: the printed schema says which root
//! type each fields object belongs to (carrick#1295).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graphql-code-first-resolvers")
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create dir");
    for entry in std::fs::read_dir(from).expect("read dir").flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// The index blob of `parcels-api`, scanned offline against `cassettes`.
fn scan_blob(cassettes: &Path) -> serde_json::Value {
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
            format!("{}/", cassettes.display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        // Rows are under test, not the type layer.
        .env("CARRICK_ALLOW_MISSING_TYPES", "1");
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

    let entry = std::fs::read_dir(storage.path())
        .expect("storage dir")
        .flatten()
        // Blobs only: a type-reach record sits beside each (carrick#3101).
        .find(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .expect("one blob");
    let blob: serde_json::Value =
        serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
            .expect("parse blob");
    assert_eq!(blob["service_name"], "parcels-api");
    blob
}

/// `(kind|field, file:line)` for every GraphQL endpoint row of `parcels-api`.
fn scan_graphql_endpoints(cassettes: &Path) -> BTreeSet<(String, String)> {
    scan_blob(cassettes)["endpoints"]
        .as_array()
        .expect("endpoints")
        .iter()
        .filter(|row| row["key"]["protocol"] == "graphql")
        .map(|row| {
            (
                format!(
                    "{}|{}",
                    row["key"]["kind"].as_str().unwrap_or_default(),
                    row["key"]["field"].as_str().unwrap_or_default()
                ),
                row["file_path"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

fn rows(items: &[(&str, &str)]) -> BTreeSet<(String, String)> {
    items
        .iter()
        .map(|(key, file)| (key.to_string(), file.to_string()))
        .collect()
}

#[test]
fn a_code_first_schema_is_served_where_its_fields_are_resolved() {
    assert_eq!(
        scan_graphql_endpoints(&fixture_dir().join("__llm__")),
        rows(&[
            ("query|health", "apps/api/src/graphql/kit.ts:19"),
            ("query|parcels", "apps/api/src/graphql/parcels/queries.ts:8"),
            ("query|parcel", "apps/api/src/graphql/parcels/queries.ts:12"),
            (
                "query|heaviestWeight",
                "apps/api/src/graphql/parcels/queries.ts:18"
            ),
            (
                "query|recentParcels",
                "apps/api/src/graphql/parcels/queries.ts:23"
            ),
            (
                "query|parcelCount",
                "apps/api/src/graphql/parcels/queries.ts:27"
            ),
            (
                "query|archivedParcels",
                "apps/api/src/graphql/parcels/queries.ts:30"
            ),
            (
                "query|pendingParcels",
                "apps/api/src/graphql/parcels/queries.ts:34"
            ),
            (
                "mutation|dispatchParcel",
                "apps/api/src/graphql/parcels/mutations.ts:5"
            ),
            ("mutation|recallParcel", "apps/api/dist/schema.graphql:3"),
            ("mutation|retireParcel", "apps/api/dist/schema.graphql:4"),
        ]),
        "field modules that call through the detected builder are admitted and their \
         resolvers located; a file calling through an undetected package, or calling a \
         detected client inside a function, is not"
    );
}

/// The rows of the answer key above.
fn answer_key() -> BTreeSet<(String, String)> {
    rows(&[
        ("query|health", "apps/api/src/graphql/kit.ts:19"),
        ("query|parcels", "apps/api/src/graphql/parcels/queries.ts:8"),
        ("query|parcel", "apps/api/src/graphql/parcels/queries.ts:12"),
        (
            "query|heaviestWeight",
            "apps/api/src/graphql/parcels/queries.ts:18",
        ),
        (
            "query|recentParcels",
            "apps/api/src/graphql/parcels/queries.ts:23",
        ),
        (
            "query|parcelCount",
            "apps/api/src/graphql/parcels/queries.ts:27",
        ),
        (
            "query|archivedParcels",
            "apps/api/src/graphql/parcels/queries.ts:30",
        ),
        (
            "query|pendingParcels",
            "apps/api/src/graphql/parcels/queries.ts:34",
        ),
        (
            "mutation|dispatchParcel",
            "apps/api/src/graphql/parcels/mutations.ts:5",
        ),
        ("mutation|recallParcel", "apps/api/dist/schema.graphql:3"),
        ("mutation|retireParcel", "apps/api/dist/schema.graphql:4"),
    ])
}

/// The fixture's cassettes, copied to a temp dir and edited by `edit`.
fn edited_cassettes(edit: impl FnOnce(&Path)) -> tempfile::TempDir {
    let cassettes = tempfile::tempdir().expect("temp cassette dir");
    copy_tree(&fixture_dir().join("__llm__"), cassettes.path());
    edit(cassettes.path());
    cassettes
}

fn edit_json(file: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let mut answer: serde_json::Value =
        serde_json::from_slice(&std::fs::read(file).expect("read cassette"))
            .expect("parse cassette");
    edit(&mut answer);
    std::fs::write(file, serde_json::to_vec(&answer).unwrap()).expect("write cassette");
}

/// Without the builder's package in detection, no field module is admitted
/// to the model for its schema (carrick#1157's gate). The fields are still
/// located: the printed schema says which root type each field object
/// belongs to, and the module builds it at module scope (carrick#1295).
#[test]
fn without_the_builder_package_in_detection_fields_are_still_located_from_the_schema() {
    let cassettes = edited_cassettes(|dir| {
        edit_json(
            &dir.join("framework-detect/framework-detect.json"),
            |answer| {
                answer["data_fetchers"] = serde_json::json!([]);
            },
        );
    });
    assert_eq!(scan_graphql_endpoints(cassettes.path()), answer_key());
}

/// A field module the model is asked about, answering with no field list,
/// still serves its fields where they are resolved (carrick#1295).
#[test]
fn a_field_the_model_leaves_out_is_located_from_the_schema() {
    let cassettes = edited_cassettes(|dir| {
        edit_json(&dir.join("analyze-file/queries.json"), |answer| {
            answer["graphql_operations"] = serde_json::json!([]);
        });
    });
    assert_eq!(scan_graphql_endpoints(cassettes.path()), answer_key());
}

/// A root field no reader places is saved as an unread site on the line its
/// row sits on, with the reason (carrick#3989). The two fields the answer
/// key serves at the printed schema are the ones no resolver was found for;
/// every other field is served at its resolver and saves nothing. The rows
/// themselves do not move.
#[test]
fn a_field_no_reader_places_is_saved_with_a_reason() {
    let blob = scan_blob(&fixture_dir().join("__llm__"));
    let sites = blob["unread_sites"]
        .as_array()
        .expect("the blob has the unread_sites key");
    let handler: BTreeSet<(String, u64, String, String)> = sites
        .iter()
        .filter(|site| site["slot"] == "handler")
        .map(|site| {
            (
                site["file"].as_str().unwrap_or_default().to_string(),
                site["line"].as_u64().unwrap_or_default(),
                site["reason"].as_str().unwrap_or_default().to_string(),
                site["role"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let at = |line: u64| {
        (
            "apps/api/dist/schema.graphql".to_string(),
            line,
            "handler_not_located".to_string(),
            "producer".to_string(),
        )
    };
    assert_eq!(handler, BTreeSet::from([at(3), at(4)]));
    assert_eq!(
        blob["unread_sites_by_reason"]["handler_not_located"], 2,
        "the count per reason names both"
    );
    assert_eq!(
        scan_graphql_endpoints(&fixture_dir().join("__llm__")),
        answer_key(),
        "saving the sites moves no row"
    );
}
