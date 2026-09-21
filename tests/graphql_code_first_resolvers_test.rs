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
//! | `parcels` | `parcels/queries.ts:7` | `parcels/queries.ts:7` |
//! | `parcel` | `parcels/queries.ts:11` | `parcels/queries.ts:11` |
//! | `heaviestWeight` | `parcels/queries.ts:17` | `parcels/queries.ts:17` |
//! | `recentParcels` | `parcels/queries.ts:22` | `parcels/queries.ts:22` |
//! | `parcelCount` | `parcels/queries.ts:26` | `parcels/queries.ts:26` |
//! | `archivedParcels` | `parcels/queries.ts:29` | `parcels/queries.ts:29` |
//! | `dispatchParcel` | `parcels/mutations.ts:5` | `parcels/mutations.ts:5` |
//! | `recallParcel` | nowhere admitted | `dist/schema.graphql:3` |
//! | `retireParcel` | nowhere admitted | `dist/schema.graphql:4` |
//!
//! Without the package in detection no file is admitted for its schema
//! fields, and every field is served at its printed schema line.

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

/// `(kind|field, file:line)` for every GraphQL endpoint row of `parcels-api`.
fn scan_graphql_endpoints(cassettes: &Path) -> BTreeSet<(String, String)> {
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
        .next()
        .expect("one blob");
    let blob: serde_json::Value =
        serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
            .expect("parse blob");
    assert_eq!(blob["service_name"], "parcels-api");
    blob["endpoints"]
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
            ("query|parcels", "apps/api/src/graphql/parcels/queries.ts:7"),
            ("query|parcel", "apps/api/src/graphql/parcels/queries.ts:11"),
            (
                "query|heaviestWeight",
                "apps/api/src/graphql/parcels/queries.ts:17"
            ),
            (
                "query|recentParcels",
                "apps/api/src/graphql/parcels/queries.ts:22"
            ),
            (
                "query|parcelCount",
                "apps/api/src/graphql/parcels/queries.ts:26"
            ),
            (
                "query|archivedParcels",
                "apps/api/src/graphql/parcels/queries.ts:29"
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

#[test]
fn without_the_builder_package_in_detection_no_file_is_admitted_for_its_schema() {
    let cassettes = tempfile::tempdir().expect("temp cassette dir");
    copy_tree(&fixture_dir().join("__llm__"), cassettes.path());
    let detection = cassettes
        .path()
        .join("framework-detect/framework-detect.json");
    let mut answer: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&detection).expect("read detection"))
            .expect("parse detection");
    answer["data_fetchers"] = serde_json::json!([]);
    std::fs::write(&detection, serde_json::to_vec(&answer).unwrap()).expect("write detection");

    let printed = "apps/api/dist/schema.graphql";
    assert_eq!(
        scan_graphql_endpoints(cassettes.path()),
        rows(&[
            ("query|archivedParcels", &format!("{printed}:13")),
            ("query|health", &format!("{printed}:14")),
            ("query|heaviestWeight", &format!("{printed}:15")),
            ("query|parcel", &format!("{printed}:16")),
            ("query|parcelCount", &format!("{printed}:17")),
            ("query|parcels", &format!("{printed}:18")),
            ("query|recentParcels", &format!("{printed}:19")),
            ("mutation|dispatchParcel", &format!("{printed}:2")),
            ("mutation|recallParcel", &format!("{printed}:3")),
            ("mutation|retireParcel", &format!("{printed}:4")),
        ])
    );
}
