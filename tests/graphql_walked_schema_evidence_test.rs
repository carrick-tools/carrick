//! A schema file under a service's directory is served only when the service
//! shows it serves a schema (carrick#1189).
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/graphql-walked-vendor-schema/`, with no `graphqlSchemas`
//! anywhere, so every schema is found by a service's own walk:
//!
//! - `ledger-api` walks `src/schema.graphql` and serves an HTTP route;
//! - `directory-api` walks `src/schema.graphql`, serves no route, and the
//!   model links a resolver to its field;
//! - `wallet-web` has a copy of a third-party ledger API's schema in
//!   `src/vendor/`, a document against it and a document against
//!   `ledger-api`'s `accounts`;
//! - `ops-console`, settled after `wallet-web`, has a document against the
//!   vendor schema;
//! - `storefront-gateway` serves an ordinary REST route and has a copy of a
//!   third-party payouts API's schema in `src/vendor/` with a document against
//!   it. Any HTTP route still counts as evidence, so the copy is served: a
//!   known limit, pinned here until a GraphQL-server signal replaces the route
//!   leg (carrick#1213).
//!
//! The cassettes answer the route and the resolver; every other row is
//! deterministic.
//!
//! Answer key, read by hand from the fixture:
//!
//! | service | producers | calls |
//! |---|---|---|
//! | `ledger-api` | `accounts` (route evidence) | none |
//! | `directory-api` | `people` (resolver evidence) | none |
//! | `wallet-web` | none: the vendor copy is not served | `accounts` only; `balance` is a call to an external API |
//! | `ops-console` | none | none: `statements` is a call to an external API |
//! | `storefront-gateway` | `payouts` (route evidence, the known limit) | `payouts` |

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graphql-walked-vendor-schema")
}

struct Scan {
    blobs: Vec<(String, serde_json::Value)>,
    stdout: String,
}

impl Scan {
    fn graphql_rows(&self, service: &str, section: &str) -> BTreeSet<(String, String)> {
        let blob = &self
            .blobs
            .iter()
            .find(|(name, _)| name == service)
            .unwrap_or_else(|| panic!("no blob for {service}"))
            .1;
        blob[section]
            .as_array()
            .unwrap_or_else(|| panic!("{service} blob has no {section}"))
            .iter()
            .filter(|row| row["key"]["protocol"] == "graphql")
            .map(|row| {
                let file = row["file_path"].as_str().unwrap_or_default();
                (
                    format!(
                        "{}|{}",
                        row["key"]["kind"].as_str().unwrap_or_default(),
                        row["key"]["field"].as_str().unwrap_or_default()
                    ),
                    file.split(':').next().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }
}

fn scan() -> Scan {
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
        // Rows and report lines are under test, not the type layer.
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

    let mut blobs = Vec::new();
    for entry in std::fs::read_dir(storage.path())
        .expect("storage dir")
        .flatten()
    {
        let blob: serde_json::Value =
            serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
                .expect("parse blob");
        let service = blob["service_name"]
            .as_str()
            .expect("blob names its service")
            .to_string();
        blobs.push((service, blob));
    }
    Scan {
        blobs,
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
    }
}

fn rows(items: &[(&str, &str)]) -> BTreeSet<(String, String)> {
    items
        .iter()
        .map(|(key, file)| (key.to_string(), file.to_string()))
        .collect()
}

#[test]
fn a_vendor_schema_in_a_client_directory_is_not_that_clients_producers() {
    let scan = scan();

    assert_eq!(
        scan.graphql_rows("ledger-api", "endpoints"),
        rows(&[("query|accounts", "apps/api/src/schema.graphql")]),
        "a service that serves routes serves the schema its walk finds"
    );
    assert_eq!(
        scan.graphql_rows("directory-api", "endpoints"),
        rows(&[("query|people", "apps/directory/src/schema.graphql")]),
        "a resolver joined to a field is evidence without any route"
    );
    assert!(
        scan.graphql_rows("wallet-web", "endpoints").is_empty(),
        "the client serves none of the vendor copy in its tree"
    );
    assert_eq!(
        scan.graphql_rows("wallet-web", "calls"),
        rows(&[("query|accounts", "apps/web/src/graphql/accounts.gql")]),
        "the document against the vendor copy is a call to an external API"
    );
    assert!(
        scan.graphql_rows("ops-console", "calls").is_empty(),
        "a later service reads the vendor copy as external too"
    );
    // Known limit (carrick#1213): a backend-for-frontend's REST route is
    // evidence, so the vendor copy in its tree is served and its document
    // reads as a call into this repository. Flip both to empty/external when
    // the route leg is replaced by a GraphQL-server signal.
    assert_eq!(
        scan.graphql_rows("storefront-gateway", "endpoints"),
        rows(&[(
            "query|payouts",
            "apps/gateway/src/vendor/payouts-schema.graphql"
        )]),
        "today any HTTP route serves the schema a service's walk finds"
    );
    assert_eq!(
        scan.graphql_rows("storefront-gateway", "calls"),
        rows(&[("query|payouts", "apps/gateway/src/graphql/payouts.gql")]),
    );
    assert!(
        scan.stdout.contains(
            "Service 'wallet-web': 2 GraphQL schema field(s) in \
             'apps/web/src/vendor/ledger-schema.graphql' are not indexed as operations it serves."
        ),
        "the report names the file it stopped reading as served:\n{}",
        scan.stdout
    );
}
