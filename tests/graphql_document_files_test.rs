//! A GraphQL document file whose compiled declaration its own service holds
//! writes no consumer row: the calls that execute the declaration are the
//! consumers, and a document nothing executes is consumed by nobody.
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/graphql-document-files/`: `catalog-api` serves a
//! schema-first GraphQL API; `documents` holds `operations.graphql` and the
//! module generated from it (declarations of the shape `{ id, op, query }`);
//! `catalog-web` sends one of those declarations through a client.
//!
//! Answer key, read by hand from the fixture:
//!
//! | operation | executed at | consumer rows |
//! |---|---|---|
//! | `items` | `apps/web/src/list.ts:5`, and `:9` behind `&&` | one each, in `catalog-web`; none in `documents` |
//! | `renameItem` | nowhere | none |
//! | `adminStats` | nowhere | none (the producer field stays) |

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

/// `(service, kind|field, file:line)` of every GraphQL consumer row, and the
/// `kind|field` of every GraphQL producer row, from one scan of the fixture.
/// Consumer rows `(service, kind|field, file:line)` and producer `kind|field`s.
type Scan = (BTreeSet<(String, String, String)>, BTreeSet<String>);

fn scan() -> &'static Scan {
    static SCAN: OnceLock<Scan> = OnceLock::new();
    SCAN.get_or_init(|| {
        let repo =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graphql-document-files");
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

        let mut consumers = BTreeSet::new();
        let mut producers = BTreeSet::new();
        for entry in std::fs::read_dir(storage.path())
            .expect("storage dir")
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        {
            let blob: serde_json::Value =
                serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
                    .expect("parse blob");
            let service = blob["service_name"].as_str().unwrap_or_default();
            let name = |key: &serde_json::Value| {
                format!(
                    "{}|{}",
                    key["kind"].as_str().unwrap_or_default(),
                    key["field"].as_str().unwrap_or_default()
                )
            };
            for row in blob["calls"].as_array().expect("calls section") {
                if row["key"]["protocol"] == "graphql" {
                    consumers.insert((
                        service.to_string(),
                        name(&row["key"]),
                        row["file_path"].as_str().unwrap_or_default().to_string(),
                    ));
                }
            }
            for row in blob["endpoints"].as_array().expect("endpoints section") {
                if row["key"]["protocol"] == "graphql" {
                    producers.insert(name(&row["key"]));
                }
            }
        }
        (consumers, producers)
    })
}

#[test]
fn a_document_file_whose_compiled_declaration_its_service_holds_writes_no_row() {
    let (consumers, _) = scan();
    let documents: Vec<_> = consumers
        .iter()
        .filter(|(service, _, _)| service == "documents")
        .collect();
    assert!(documents.is_empty(), "{documents:?}");
    assert!(
        consumers.contains(&(
            "catalog-web".to_string(),
            "query|items".to_string(),
            "apps/web/src/list.ts:5".to_string()
        )),
        "{consumers:?}"
    );
}

#[test]
fn a_document_nothing_executes_is_no_consumer() {
    let (consumers, producers) = scan();
    let dead: Vec<_> = consumers
        .iter()
        .filter(|(_, key, _)| key == "query|adminStats" || key == "mutation|renameItem")
        .collect();
    assert!(dead.is_empty(), "{dead:?}");
    assert!(producers.contains("query|adminStats"), "{producers:?}");
}

#[test]
fn a_document_passed_behind_a_short_circuit_and_is_a_consumer_at_the_call() {
    let (consumers, _) = scan();
    assert!(
        consumers.contains(&(
            "catalog-web".to_string(),
            "query|items".to_string(),
            "apps/web/src/list.ts:9".to_string()
        )),
        "{consumers:?}"
    );
}
