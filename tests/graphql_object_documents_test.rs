//! A declaration holding GraphQL text in a property is a document.
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/graphql-object-documents/`: `users-api` serves a
//! schema-first GraphQL API, and `users-web` imports four generated
//! declarations of the shape `{ id, op, query: "<document text>" }` through a
//! `export *` barrel and passes three of them, as bare identifiers, to a local
//! function. The fourth (`adminStats`) is passed nowhere.
//!
//! Answer key, read by hand from the fixture:
//!
//! | operation | executed at | row |
//! |---|---|---|
//! | `currentUser` | `Profile.ts:6` | one row at the call |
//! | `users` | `Profile.ts:11` | one row at the call |
//! | `updateUser` | `Profile.ts:14` | one row at the call |
//! | `adminStats` | nowhere | stays at `users.graphql:14` |

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn consumer_rows() -> BTreeSet<(String, String)> {
    let repo =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graphql-object-documents");
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(&repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
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

    let mut rows = BTreeSet::new();
    for entry in std::fs::read_dir(storage.path())
        .expect("storage dir")
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
    {
        let blob: serde_json::Value =
            serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
                .expect("parse blob");
        if blob["service_name"] != "users-web" {
            continue;
        }
        for row in blob["calls"].as_array().expect("calls section") {
            let key = &row["key"];
            if key["protocol"] != "graphql" {
                continue;
            }
            rows.insert((
                format!(
                    "{}|{}",
                    key["kind"].as_str().unwrap_or_default(),
                    key["field"].as_str().unwrap_or_default()
                ),
                row["file_path"].as_str().unwrap_or_default().to_string(),
            ));
        }
    }
    rows
}

#[test]
fn a_bare_argument_naming_an_object_document_is_placed_at_its_call() {
    let expected: BTreeSet<(String, String)> = [
        ("query|currentUser", "apps/web/src/components/Profile.ts:6"),
        ("query|users", "apps/web/src/components/Profile.ts:11"),
        (
            "mutation|updateUser",
            "apps/web/src/components/Profile.ts:14",
        ),
        ("query|adminStats", "apps/web/src/graphql/users.graphql:14"),
    ]
    .iter()
    .map(|(key, file)| (key.to_string(), file.to_string()))
    .collect();
    assert_eq!(consumer_rows(), expected);
}
