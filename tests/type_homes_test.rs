//! A type entry records where its type is declared, whichever protocol it
//! belongs to (carrick#2148, H2).
//!
//! Drives the real scanner binary offline over `tests/fixtures/type-homes/`
//! with the compiler engine LIVE (no `CARRICK_ALLOW_MISSING_TYPES`), as the
//! Action scans. One pass stores the home the engine reported for the answer
//! that published each entry's text.
//!
//! Answer key, read by hand from the fixture (`src/models/orders.ts` declares
//! the types; `src/models/index.ts` is a barrel over it):
//!
//! | row | its type | home |
//! |---|---|---|
//! | `orders-api.ts:6` producer response | `Order`, imported through the barrel | `models/orders.ts:1` |
//! | `orders-api.ts:11` producer request | `CreateOrder`, the declared body | `models/orders.ts:7` |
//! | `client.ts:5` consumer request | `NewItem`, a typed parameter | `models/orders.ts:12` |
//! | `client.ts:9` consumer response | `Page<Order>`, cast at the read | `models/orders.ts:16` |
//! | `client.ts:14` consumer response | `KitResponse<Order>`, a client package's machinery | none |
//! | `orders.ts:4` pub/sub publisher | `OrderCreated` | `types.ts:1` |
//! | `relay.ts:9` pub/sub publisher | `OrderShipped` | `types.ts:5` |
//! | `tickets.ts:7` socket handler | `TicketUpdate` | `models/orders.ts:21` |
//! | `schema.graphql:7` GraphQL response | `Parcel`, the field's SDL type | `models/orders.ts:26` |
//!
//! A shape nobody named has no home: the `orders.paid` payload, an object
//! literal.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// The blob one offline scan of `repo` writes.
fn scan(repo: &Path) -> serde_json::Value {
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
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env_remove("CARRICK_ALLOW_MISSING_TYPES");
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
    let mut blobs: Vec<serde_json::Value> = std::fs::read_dir(storage.path())
        .expect("storage dir")
        .flatten()
        // Blobs only: a type-reach record sits beside each (carrick#3101).
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .map(|entry| {
            serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
                .expect("parse blob")
        })
        .collect();
    assert_eq!(blobs.len(), 1, "one service, one blob");
    blobs.remove(0)
}

/// `(file, line, role, kind)` -> the entry's `defined_in` as `(file, line, symbol)`.
type Home = Option<(String, u64, String)>;

/// The one `role` `kind` entry at `file:line`.
fn entry_at<'a>(
    blob: &'a serde_json::Value,
    file: &str,
    line: u64,
    role: &str,
    kind: &str,
) -> &'a serde_json::Value {
    let entries: Vec<&serde_json::Value> = blob["type_manifest"]
        .as_array()
        .expect("type_manifest")
        .iter()
        .filter(|entry| {
            entry["role"] == role
                && entry["type_kind"] == kind
                && entry["file_path"] == file
                && entry["line_number"] == line
        })
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "one {role} {kind} entry at {file}:{line}: {entries:#?}"
    );
    entries[0]
}

fn home_of(blob: &serde_json::Value, file: &str, line: u64, role: &str, kind: &str) -> Home {
    entry_at(blob, file, line, role, kind)
        .get("defined_in")
        .filter(|home| !home.is_null())
        .map(|home| {
            (
                home["file_path"].as_str().unwrap_or_default().to_string(),
                home["line_number"].as_u64().unwrap_or_default(),
                home["symbol"].as_str().unwrap_or_default().to_string(),
            )
        })
}

fn at(file: &str, line: u64, symbol: &str) -> Home {
    Some((file.to_string(), line, symbol.to_string()))
}

#[test]
fn type_homes_fixture_records_every_named_home() {
    let blob = scan(&fixture_dir("type-homes"));
    let cases: [(&str, u64, &str, &str, Home); 9] = [
        (
            "src/orders-api.ts",
            6,
            "producer",
            "response",
            at("src/models/orders.ts", 1, "Order"),
        ),
        (
            "src/orders-api.ts",
            11,
            "producer",
            "request",
            at("src/models/orders.ts", 7, "CreateOrder"),
        ),
        (
            "src/client.ts",
            5,
            "consumer",
            "request",
            at("src/models/orders.ts", 12, "NewItem"),
        ),
        (
            "src/client.ts",
            9,
            "consumer",
            "response",
            at("src/models/orders.ts", 16, "Page"),
        ),
        ("src/client.ts", 14, "consumer", "response", None),
        (
            "src/orders.ts",
            4,
            "consumer",
            "response",
            at("src/types.ts", 1, "OrderCreated"),
        ),
        (
            "src/relay.ts",
            9,
            "consumer",
            "response",
            at("src/types.ts", 5, "OrderShipped"),
        ),
        (
            "src/tickets.ts",
            7,
            "producer",
            "response",
            at("src/models/orders.ts", 21, "TicketUpdate"),
        ),
        (
            "schema.graphql",
            7,
            "producer",
            "response",
            at("src/models/orders.ts", 26, "Parcel"),
        ),
    ];
    for (file, line, role, kind, want) in cases {
        assert_eq!(
            home_of(&blob, file, line, role, kind),
            want,
            "{role} {kind} {file}:{line}"
        );
    }
}

/// carrick#2148 H3: a request validated by a schema records the schema's
/// declaration as its home, marked as a schema, over
/// `tests/fixtures/schema-homes/`.
///
/// | row | its contract | home |
/// |---|---|---|
/// | `tickets.ts:7` producer request | a validator middleware handed `NewTicket` | `schemas.ts:4`, schema |
/// | `tickets.ts:11` producer request | the body handed to `NewTicket.safeParse` | `schemas.ts:4`, schema |
/// | `tickets.ts:16` producer request | a schema written inline | none |
#[test]
fn schema_homes_fixture_records_the_schema_declaration() {
    let blob = scan(&fixture_dir("schema-homes"));
    let schema_home = |line: u64| {
        let entry = entry_at(&blob, "src/tickets.ts", line, "producer", "request");
        (
            entry["primary_type_symbol"].clone(),
            entry["defined_in"].clone(),
        )
    };
    let named = serde_json::json!({
        "file_path": "src/schemas.ts",
        "line_number": 4,
        "symbol": "NewTicket",
        "kind": "schema",
    });
    for line in [7, 11] {
        assert_eq!(
            schema_home(line),
            (serde_json::Value::Null, named.clone()),
            "producer request src/tickets.ts:{line}"
        );
    }
    assert_eq!(
        home_of(&blob, "src/tickets.ts", 16, "producer", "request"),
        None,
        "an inline schema has no home"
    );
}
