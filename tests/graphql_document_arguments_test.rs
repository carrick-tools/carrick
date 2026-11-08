//! The calls that execute a GraphQL document are read through the value
//! resolver (carrick#3504 S9): a document handed under an object key, through
//! a wrapper's parameter, or imported from another workspace package through
//! an `export *` barrel is placed at the call that sends it.
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/graphql-document-arguments/`: `users-api` serves a
//! schema-first GraphQL API, and `users-web` hands generated documents to a
//! client it declares. The fixture commits the web app's `node_modules` (a
//! copy of `packages/documents`, as an install leaves it): the compiler
//! resolves a package import only through it.
//!
//! Answer key, read by hand from the fixture:
//!
//! | operation | executed at | row |
//! |---|---|---|
//! | `users` | `w-key.ts:5`, `useQuery({ query: ListUsersDocument })` | one row at the call |
//! | `updateUser` | `w-mutation.ts:5`, `useMutation({ mutation: updateUserMutation })` | one row at the call |
//! | `currentUser` | `w-param.ts:9`, `run(currentUserQuery)`; `run` hands its parameter to `gql({ query: doc })` on line 5 | one row at the caller, none at line 5 |
//! | `userById` | `w-package.ts:5`, imported from `@fixture/documents` | one row at the call |
//!
//! The HTTP twins these calls drop are covered in the reader's unit tests
//! (`graphql_document_sites.rs`): no call here is an HTTP candidate, so the
//! model is asked nothing.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

/// `(kind|field, file:line)` of every GraphQL consumer row of `users-web`,
/// from one scan of the fixture.
fn rows() -> &'static BTreeSet<(String, String)> {
    static SCAN: OnceLock<BTreeSet<(String, String)>> = OnceLock::new();
    SCAN.get_or_init(|| {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/graphql-document-arguments");
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

        let mut graphql = BTreeSet::new();
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
                graphql.insert((
                    format!(
                        "{}|{}",
                        key["kind"].as_str().unwrap_or_default(),
                        key["field"].as_str().unwrap_or_default()
                    ),
                    row["file_path"].as_str().unwrap_or_default().to_string(),
                ));
            }
        }
        graphql
    })
}

fn row(key: &str, at: &str) -> (String, String) {
    (key.to_string(), at.to_string())
}

#[test]
fn an_object_key_argument_naming_a_document_is_placed_at_its_call() {
    let rows = rows();
    assert!(
        rows.contains(&row("query|users", "apps/web/src/w-key.ts:5")),
        "{rows:?}"
    );
    assert!(
        rows.contains(&row("mutation|updateUser", "apps/web/src/w-mutation.ts:5")),
        "{rows:?}"
    );
}

#[test]
fn a_document_passed_through_a_wrapper_parameter_is_placed_at_the_wrappers_caller() {
    let rows = rows();
    let current_user: Vec<&(String, String)> = rows
        .iter()
        .filter(|(key, _)| key == "query|currentUser")
        .collect();
    assert_eq!(
        current_user,
        vec![&row("query|currentUser", "apps/web/src/w-param.ts:9")],
        "placed at the caller that passes the document, not inside the wrapper"
    );
}

#[test]
fn a_document_a_workspace_package_reexports_is_placed_at_its_call() {
    let rows = rows();
    assert!(
        rows.contains(&row("query|userById", "apps/web/src/w-package.ts:5")),
        "{rows:?}"
    );
}
