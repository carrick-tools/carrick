//! GraphQL consumer rows sit at the calls that execute a document
//! (carrick#1157).
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/graphql-document-call-sites/`: `orders-api` serves a
//! schema-first GraphQL API from `src/schema.graphql`, and `shop-web` writes
//! its documents in `src/graphql/orders.graphql`, compiled into
//! `src/generated/documents.ts` as graphql-js AST literals. Two components
//! pass those declarations to hooks, one through a relative import and one
//! through the `@/` path alias. The checkout page also makes a REST call on an
//! internal base.
//!
//! The cassettes answer the REST call and the REST route; every GraphQL row
//! below is deterministic.
//!
//! Answer key, read by hand from the fixture:
//!
//! | operation | executed at | row |
//! |---|---|---|
//! | `Orders` (`orders`, aliased `shippingZones`) | `CheckoutPage.tsx:6` | two rows at the hook |
//! | `PlaceOrder` (`placeOrder`) | `CheckoutPage.tsx:7`, `RetryButton.tsx:5` | one row at each hook |
//! | `Carriers` (`carriers`) | nowhere | stays at `orders.graphql:17` |
//!
//! The REST call in the checkout page is still a call: the page is not a
//! document file, so the transport fold does not read it as a GraphQL POST.
//!
//! The checkout page's cassette also locates the `placeOrder` result type
//! (`PlacedOrderFragment`, declared in the generated module). The model answers
//! for the file it reads, the page, so that answer joins the row placed at the
//! page's hook, never a row in the document's own module (carrick#1728). The
//! type is the field's payload, not the whole operation result: a consumer row
//! is keyed by its root field, the level the call-site generic is unwrapped to
//! (`resolve_request_type_arg` in `src/graphql.rs`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graphql-document-call-sites")
}

struct Scan {
    blobs: Vec<(String, serde_json::Value)>,
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

    /// `(key, file:line)` for every row of `protocol` in `section`, the key as
    /// `kind|field` for GraphQL and `METHOD path` for HTTP.
    fn rows(&self, service: &str, section: &str, protocol: &str) -> BTreeSet<(String, String)> {
        self.blob(service)[section]
            .as_array()
            .unwrap_or_else(|| panic!("{service} blob has no {section}"))
            .iter()
            .filter(|row| row["key"]["protocol"] == protocol)
            .map(|row| {
                let key = &row["key"];
                let key = if protocol == "graphql" {
                    format!(
                        "{}|{}",
                        key["kind"].as_str().unwrap_or_default(),
                        key["field"].as_str().unwrap_or_default()
                    )
                } else {
                    format!(
                        "{} {}",
                        key["method"].as_str().unwrap_or_default(),
                        key["path"].as_str().unwrap_or_default()
                    )
                };
                (
                    key,
                    row["file_path"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }
}

/// Whether the scan runs the type sidecar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Types {
    /// Rows are under test, not the type layer.
    Optional,
    /// The type layer is under test: a sidecar that cannot start fails the
    /// scan instead of reading as a missing type.
    Required,
}

fn scan(repo: &Path, types: Types) -> Scan {
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
        .env("CARRICK_SKIP_INTENTS", "1");
    match types {
        Types::Optional => {
            cmd.env("CARRICK_ALLOW_MISSING_TYPES", "1");
        }
        Types::Required => {
            cmd.env_remove("CARRICK_ALLOW_MISSING_TYPES");
        }
    }
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
    Scan { blobs }
}

fn rows(items: &[(&str, &str)]) -> BTreeSet<(String, String)> {
    items
        .iter()
        .map(|(key, file)| (key.to_string(), file.to_string()))
        .collect()
}

#[test]
fn consumer_rows_sit_at_the_calls_that_execute_compiled_documents() {
    let scan = scan(&fixture_dir(), Types::Optional);

    assert_eq!(
        scan.rows("shop-web", "calls", "graphql"),
        rows(&[
            ("query|orders", "apps/web/src/pages/CheckoutPage.tsx:6"),
            (
                "query|shippingZones",
                "apps/web/src/pages/CheckoutPage.tsx:6"
            ),
            (
                "mutation|placeOrder",
                "apps/web/src/pages/CheckoutPage.tsx:7"
            ),
            (
                "mutation|placeOrder",
                "apps/web/src/components/RetryButton.tsx:5"
            ),
            ("query|carriers", "apps/web/src/graphql/orders.graphql:17"),
        ]),
        "an executed operation is indexed at each call that executes it; one no call \
         executes stays in its document file"
    );
    // A row at a call is attributed like the document it came from.
    let bindings: BTreeSet<String> = scan.blob("shop-web")["calls"]
        .as_array()
        .expect("calls")
        .iter()
        .filter(|row| row["key"]["protocol"] == "graphql")
        .map(|row| row["schema_binding"].to_string())
        .collect();
    assert_eq!(bindings, BTreeSet::from(["\"served\"".to_string()]));
    assert_eq!(
        scan.rows("shop-web", "calls", "http"),
        rows(&[(
            "GET /orders/export",
            "apps/web/src/pages/CheckoutPage.tsx:10"
        )]),
        "the REST call on a page that executes documents is not folded away"
    );
    assert_eq!(
        scan.rows("orders-api", "endpoints", "graphql"),
        rows(&[
            ("query|orders", "apps/api/src/schema.graphql:2"),
            ("query|shippingZones", "apps/api/src/schema.graphql:3"),
            ("query|carriers", "apps/api/src/schema.graphql:4"),
            ("mutation|placeOrder", "apps/api/src/schema.graphql:8"),
        ])
    );
}

/// `(type_state, primary_type_symbol, expanded_definition)` of the GraphQL
/// consumer response entry for `key` (`kind|field`) at `file:line` in
/// `service`'s type manifest.
fn consumer_response_type(
    scan: &Scan,
    service: &str,
    key: &str,
    site: (&str, u64),
) -> (String, Option<String>, Option<String>) {
    let manifest = scan.blob(service)["type_manifest"]
        .as_array()
        .expect("type_manifest");
    let entry = manifest
        .iter()
        .find(|entry| {
            entry["protocol"] == "graphql"
                && entry["role"] == "consumer"
                && entry["type_kind"] == "response"
                && format!(
                    "{}|{}",
                    entry["kind"].as_str().unwrap_or_default(),
                    entry["field"].as_str().unwrap_or_default()
                ) == key
                && entry["file_path"] == site.0
                && entry["line_number"] == site.1
        })
        .unwrap_or_else(|| panic!("no consumer response entry for {key} at {site:?}"));
    let text = |field: &str| entry[field].as_str().map(str::to_string);
    (
        text("type_state").unwrap_or_default(),
        text("primary_type_symbol"),
        text("expanded_definition"),
    )
}

/// carrick#1728: the model locates a result type for the file it reads, which
/// is the file that executes the document. The row it describes is placed at
/// that file's call only once the documents are read, so the located type
/// must join after the placement, or it finds the document's own module and
/// nothing there.
#[test]
fn a_type_located_where_a_document_is_executed_types_the_row_at_the_call() {
    let scan = scan(&fixture_dir(), Types::Required);

    let (state, symbol, definition) = consumer_response_type(
        &scan,
        "shop-web",
        "mutation|placeOrder",
        ("apps/web/src/pages/CheckoutPage.tsx", 7),
    );
    assert_eq!(
        symbol.as_deref(),
        Some("PlacedOrderFragment"),
        "the type located in the page anchors the row at the page's hook"
    );
    assert_ne!(state, "unknown", "the located type resolves");
    let definition = definition.unwrap_or_default();
    assert!(
        definition.contains("id: string") && !definition.contains("placeOrder"),
        "the located type is the field's payload: {definition}"
    );

    // The page locates nothing for the query it also executes.
    let (state, symbol, _) = consumer_response_type(
        &scan,
        "shop-web",
        "query|orders",
        ("apps/web/src/pages/CheckoutPage.tsx", 6),
    );
    assert_eq!(
        (state.as_str(), symbol),
        ("unknown", None),
        "a row with no located type stays unknown"
    );
}
