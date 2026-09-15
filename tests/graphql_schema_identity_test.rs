//! GraphQL documents are attributed to the schema they are written against
//! (carrick#1134).
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/graphql-schema-identity/`, copied into a fresh git
//! repository so "tracked" means what it means on a real checkout. Two
//! printed schemas sit in a tooling build folder that no service directory
//! covers:
//!
//! - `catalog.graphql`, which `catalog-api` names in `graphqlSchemas`, so it is
//!   served;
//! - `ledger.graphql`, which nothing declares: a copy of an API someone else
//!   serves.
//!
//! Both define a `viewer` query. `storefront` sends documents against each,
//! one mixing the two, and two tagged-template documents that select only
//! `viewer`, sent through a declared internal and a declared external base.
//!
//! The cassettes answer the two transport calls and the one HTTP route; every
//! GraphQL row below is deterministic.
//!
//! Answer key, read by hand from the fixture:
//!
//! | document | fields it shares with a schema | identity | rows |
//! |---|---|---|---|
//! | `catalog.gql` | `products`, `addProduct` (`retiredListing` is in neither) | served | all three |
//! | `ledger.gql` | `balance`, `statements`, `transferFunds` | external | none |
//! | `overview.gql` | `products` (catalog), `balance` (ledger) | unresolved | none |
//! | `viewer.gql` | `viewer` (both), no transport | unresolved | none |
//! | `nested.gql` | `product` (it also selects `legacySku`, which `Product` no longer has) | served | one |
//! | `retired.gql` | none (`discontinuedQuery` is in no schema) | no local schema | one |
//! | `account.ts` | `viewer` (both), base `CATALOG_URL` internal | served | one |
//! | `holder.ts` | `viewer` (both), base `LEDGER_URL` external | external | none |

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graphql-schema-identity")
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

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed");
}

/// The fixture as a git repository with every file staged. An extra schema
/// written after staging stays untracked: it defines `retiredListing`, so if
/// untracked files counted as identities, `catalog.gql` would span two
/// schemas and lose its rows.
fn tracked_copy() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp fixture dir");
    copy_tree(&fixture_dir(), dir.path());
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["add", "-A"]);
    std::fs::write(
        dir.path().join("tooling/graphql/dist/local.graphql"),
        "type Query {\n  retiredListing: Listing\n}\n\ntype Listing {\n  id: ID!\n}\n",
    )
    .expect("write untracked schema");
    dir
}

struct Scan {
    blobs: Vec<(String, serde_json::Value)>,
    stdout: String,
}

impl Scan {
    fn blob(&self, service: &str) -> &serde_json::Value {
        &self
            .blobs
            .iter()
            .find(|(name, _)| name == service)
            .unwrap_or_else(|| panic!("no blob for {service}"))
            .1
    }

    /// `(key, file)` for every row of `protocol` in `section` of a service,
    /// the key as `kind|field` for GraphQL and `METHOD path` for HTTP.
    fn rows(&self, service: &str, section: &str, protocol: &str) -> BTreeSet<(String, String)> {
        self.blob(service)[section]
            .as_array()
            .unwrap_or_else(|| panic!("{service} blob has no {section}"))
            .iter()
            .filter(|row| row["key"]["protocol"] == protocol)
            .map(|row| {
                let file = row["file_path"].as_str().unwrap_or_default();
                let file = file.split(':').next().unwrap_or_default().to_string();
                let key = &row["key"];
                let key = if protocol == "graphql" {
                    format!(
                        "{}|{}",
                        key["kind"].as_str().unwrap_or_default(),
                        key["field"].as_str().unwrap_or_default()
                    )
                } else {
                    key.to_string()
                };
                (key, file)
            })
            .collect()
    }
}

fn scan(repo: &Path) -> Scan {
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
        .env("CARRICK_ALLOW_MISSING_TYPES", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
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
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
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
    Scan { blobs, stdout }
}

fn rows(items: &[(&str, &str)]) -> BTreeSet<(String, String)> {
    items
        .iter()
        .map(|(key, file)| (key.to_string(), file.to_string()))
        .collect()
}

#[test]
fn documents_are_indexed_as_calls_only_when_their_schema_is_served_here() {
    let repo = tracked_copy();
    let scan = scan(repo.path());

    let catalog = "tooling/graphql/dist/catalog.graphql";
    assert_eq!(
        scan.rows("catalog-api", "endpoints", "graphql"),
        rows(&[
            ("mutation|addProduct", catalog),
            ("query|product", catalog),
            ("query|products", catalog),
            ("query|viewer", catalog),
        ]),
        "the declared schema's root fields are catalog-api's producers"
    );

    let catalog_doc = "apps/web/src/graphql/catalog.gql";
    assert_eq!(
        scan.rows("storefront", "calls", "graphql"),
        rows(&[
            ("mutation|addProduct", catalog_doc),
            ("query|products", catalog_doc),
            // In no schema at all: still a call, so a field the server removed
            // reads as a missing operation.
            ("query|retiredListing", catalog_doc),
            // A nested field the schema dropped does not change which schema
            // the document is written against.
            ("query|product", "apps/web/src/graphql/nested.gql"),
            // A root field no schema holds: its server may be elsewhere, so it
            // stays a call and reads as a missing operation.
            (
                "query|discontinuedQuery",
                "apps/web/src/graphql/retired.gql"
            ),
            ("query|viewer", "apps/web/src/account.ts"),
        ]),
        "only documents attributed to the served schema, or read through an internal base, \
         are calls:\n{}",
        scan.stdout
    );

    // Neither transport comes back as an HTTP call. Here both calls name their
    // document binding, so the #361 repair rewrites them to the operation key
    // and they never reach the graph; the fold-before-drop ordering for a
    // transport that does reach it is pinned by the engine unit test
    // `settle_drops_external_documents_and_still_folds_their_transport`.
    let http_calls = scan.rows("storefront", "calls", "http");
    assert!(
        !http_calls
            .iter()
            .any(|(_, file)| file.ends_with("account.ts") || file.ends_with("holder.ts")),
        "transport calls of document files are folded: {http_calls:?}"
    );

    let notices: Vec<&str> = scan
        .stdout
        .lines()
        .filter(|line| line.contains("Service 'storefront'"))
        .map(str::trim)
        .collect();
    assert_eq!(
        notices,
        vec![
            "Service 'storefront': 4 GraphQL document operation(s) are written against \
             'tooling/graphql/dist/ledger.graphql', which no service in this repository serves, \
             so they are read as calls to an external API and not indexed. If a service here \
             serves that schema, name the file in its `graphqlSchemas`.",
            "Service 'storefront': 3 GraphQL document operation(s) are not indexed because no \
             single schema holds all their fields, or both a served and an external schema do \
             and the call's base URL does not say which.",
        ],
        "{}",
        scan.stdout
    );
}
