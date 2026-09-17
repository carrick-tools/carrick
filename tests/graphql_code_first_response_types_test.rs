//! A code-first GraphQL field serves its resolver's return type as the
//! operation's response type (carrick#1256).
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/graphql-code-first-resolvers/` with the type sidecar LIVE
//! (no `CARRICK_ALLOW_MISSING_TYPES`, so a sidecar that fails to start fails
//! the test instead of reading as a missing type). The model cassettes place
//! each root field at its FIELD line; the resolver function sits inside the
//! field's config object, one to four lines further down. The scanner reads
//! that function's span from the AST and the sidecar resolves its return.
//!
//! Every site sits below multi-byte prose (`// Colis — lecture`), so a span
//! sent in byte units would miss the function it names (carrick#805).
//!
//! Answer key, read by hand from the fixture:
//!
//! | field | resolver | response type |
//! |---|---|---|
//! | `health` (`kit.ts:19`) | `() => 'ok'` on the field line | `string` |
//! | `parcels` (`queries.ts:6`) | `() => listParcels()` two lines down | `Parcel[]` |
//! | `parcel` (`queries.ts:10`) | arrow four lines down | `Parcel` (the sidecar renders an optional payload as the payload) |
//! | `heaviestWeight` (`queries.ts:16`) | `resolve` beside a `validate` function | `number` |
//! | `dispatchParcel` (`mutations.ts:5`) | arrow three lines down | `Parcel` |
//! | `recentParcels` (`queries.ts:21`) | an identifier, not a literal | none: no anchor is sent |
//! | `recallParcel`, `retireParcel` | no resolver located | none |
//!
//! Without the span, a bare line anchor binds `parcels` and `dispatchParcel`
//! to the builder callback one line above (the whole fields object), and
//! `parcel` and `recentParcels` to the PREVIOUS field's resolver two lines
//! above: confidently wrong types, which is what this test rules out.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graphql-code-first-resolvers")
}

/// `kind|field` -> `(type_state, expanded_definition)` for every GraphQL
/// producer RESPONSE entry of `parcels-api`'s type manifest (a field with
/// arguments also carries a request entry, which is not under test).
fn scan_graphql_response_types() -> BTreeMap<String, (String, Option<String>)> {
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
            format!("{}/__llm__/", fixture_dir().display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1");
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
        // The type layer is under test: a sidecar that cannot start must
        // fail the scan, not pass as "types unavailable".
        "CARRICK_ALLOW_MISSING_TYPES",
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
    blob["type_manifest"]
        .as_array()
        .expect("type_manifest")
        .iter()
        .filter(|entry| {
            entry["protocol"] == "graphql"
                && entry["role"] == "producer"
                && entry["type_kind"] == "response"
        })
        .map(|entry| {
            (
                format!(
                    "{}|{}",
                    entry["kind"].as_str().unwrap_or_default(),
                    entry["field"].as_str().unwrap_or_default()
                ),
                (
                    entry["type_state"].as_str().unwrap_or_default().to_string(),
                    entry["expanded_definition"].as_str().map(str::to_string),
                ),
            )
        })
        .collect()
}

const PARCEL: &str = "{ id: string; weightGrams: number; }";

#[test]
fn a_code_first_field_serves_its_resolver_return_type() {
    let types = scan_graphql_response_types();
    let typed = |field: &str| -> (String, Option<String>) {
        types
            .get(field)
            .unwrap_or_else(|| panic!("no manifest entry for {field}: {types:?}"))
            .clone()
    };

    assert_eq!(
        typed("query|health"),
        ("implicit".to_string(), Some("string".to_string())),
        "a resolver on the field line resolves as before"
    );
    assert_eq!(
        typed("query|parcels"),
        ("implicit".to_string(), Some(format!("{PARCEL}[]"))),
        "the resolver two lines below the field, not the builder callback above it"
    );
    assert_eq!(
        typed("query|parcel"),
        ("implicit".to_string(), Some(PARCEL.to_string())),
        "the field's own resolver (one parcel), not the previous field's (a list)"
    );
    assert_eq!(
        typed("query|heaviestWeight"),
        ("implicit".to_string(), Some("number".to_string())),
        "`resolve` is the resolver when another property is a function too; \
         a line anchor would have taken the previous field's parcel"
    );
    assert_eq!(
        typed("mutation|dispatchParcel"),
        ("implicit".to_string(), Some(PARCEL.to_string())),
        "a mutation's resolver three lines below its field"
    );

    // A resolver named rather than written: no anchor is sent, so the entry
    // stays unknown instead of carrying the neighbouring field's type.
    assert_eq!(
        typed("query|recentParcels"),
        ("unknown".to_string(), None),
        "an identifier resolver is left unresolved, never bound to a neighbour"
    );
    // Fields no admitted file resolves keep their printed-schema anchor only.
    for field in ["mutation|recallParcel", "mutation|retireParcel"] {
        assert_eq!(typed(field), ("unknown".to_string(), None), "{field}");
    }
}
