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
//! Each compiled declaration is asserted to a document type whose first
//! argument is its operation's result type, which declares one property per
//! root field under its response key. A row at a call serves that property's
//! type (carrick#1761): the field's payload, not the whole operation result,
//! because a consumer row is keyed by its root field, the level the call-site
//! generic is unwrapped to (`resolve_request_type_arg` in `src/graphql.rs`).
//! The checkout page's cassette also locates the `placeOrder` result type;
//! the declaration wins over it. A located type joining the row at the call
//! (carrick#1728) is covered where the declaration states no result type, in
//! `graphql_declared_result_types_test.rs`.

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

/// carrick#1761: a generated document declaration states its operation's
/// result type in its own type (`as unknown as DocumentNode<OrdersQuery, ...>`),
/// and that type declares one property per root field, under the field's
/// response key. The row at the call that executes the document serves that
/// property's type: the field's payload, never the operation wrapper. The page
/// locates nothing for this query, so only the declaration can type it.
#[test]
fn a_row_at_a_call_that_executes_a_typed_document_serves_its_fields_declared_type() {
    let scan = scan(&fixture_dir(), Types::Required);

    // Both calls that execute `PlaceOrderDocument` read the field's declared
    // type; the page's located type no longer decides it.
    for site in [
        ("apps/web/src/pages/CheckoutPage.tsx", 7),
        ("apps/web/src/components/RetryButton.tsx", 5),
    ] {
        let (state, _, definition) =
            consumer_response_type(&scan, "shop-web", "mutation|placeOrder", site);
        let definition = definition.unwrap_or_default();
        assert_ne!(state, "unknown", "{site:?}: {definition}");
        assert!(
            definition.contains("id: string") && !definition.contains("placeOrder"),
            "{site:?} serves the field's payload: {definition}"
        );
    }

    let (state, _, definition) = consumer_response_type(
        &scan,
        "shop-web",
        "query|orders",
        ("apps/web/src/pages/CheckoutPage.tsx", 6),
    );
    let definition = definition.unwrap_or_default();
    assert_ne!(
        state, "unknown",
        "the declared result type types the row: {definition}"
    );
    assert!(
        definition.contains("id: string")
            && !definition.contains("orders")
            && !definition.contains("code"),
        "the row serves the `orders` property, not the operation result: {definition}"
    );

    // An aliased root field is a property under its alias.
    let (state, _, definition) = consumer_response_type(
        &scan,
        "shop-web",
        "query|shippingZones",
        ("apps/web/src/pages/CheckoutPage.tsx", 6),
    );
    let definition = definition.unwrap_or_default();
    assert_ne!(
        state, "unknown",
        "the declared result type types the aliased row: {definition}"
    );
    assert!(
        definition.contains("code: string")
            && !definition.contains("zones")
            && !definition.contains("id: string"),
        "the aliased row serves the `zones` property: {definition}"
    );
}
