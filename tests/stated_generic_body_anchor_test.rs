//! A consumer row whose source states the body it reads as an instantiation
//! of a generic is anchored at the outer named type (carrick#1817).
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/stated-generic-body/` with the type sidecar LIVE (no
//! `CARRICK_ALLOW_MISSING_TYPES`, so a sidecar that cannot start fails the
//! test instead of reading as a missing anchor). `src/client.ts` makes nine
//! `fetch` calls and states each body at its read, by a cast or by the
//! annotation of the declaration the read initializes. `src/shapes.ts`
//! declares what the statements name. The cassette answers one data call per
//! `fetch`, and its `primary_type_symbol` is what varies: the element inside
//! the statement, the generic itself, or nothing.
//!
//! Answer key, read by hand from the fixture. The home is the declaration in
//! `src/shapes.ts`:
//!
//! | call | the read is stated as | the model named | anchor | home |
//! |---|---|---|---|---|
//! | `client.ts:5` | `Promise<Envelope<Order[]>>` | `Order` | `Envelope` | line 13 |
//! | `client.ts:11` | `Envelope<Member>` | nothing | `Envelope` | line 13 |
//! | `client.ts:17` | `Envelope<Member[]>` | `Envelope` | `Envelope` | line 13 |
//! | `client.ts:24` | `Promise<OrderEnvelope>` | `Order` | `OrderEnvelope` | line 19 |
//! | `client.ts:30` | `Listing` | nothing | `Listing` | line 22 |
//! | `client.ts:36` | `Listing<Member>` | `Member` | `Listing` | line 22 |
//! | `client.ts:42` | `Envelope<Order[]> \| Envelope<Member[]>` | `Order` | none | none |
//! | `client.ts:48` | `Envelope<Order> \| null` | nothing | `Envelope` | line 13 |
//! | `client.ts:54` | `Record<string, Order>` | `Order` | none | none |
//!
//! The anchor is the name the source writes outermost, once `Promise` and
//! `| null` are seen through: the generic for an instantiation, the alias
//! for an alias of one, and the same name for a generic written with or
//! without its defaulted argument. A type argument is never the anchor,
//! whichever name the model picked. A union of two instantiations states no
//! single name, and a generic the repo does not declare has no home to
//! state, so those rows keep no anchor.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stated-generic-body")
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
            format!("{}/", fixture_dir().join("__llm__").display()),
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
        .map(|entry| {
            serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
                .expect("parse blob")
        })
        .collect();
    assert_eq!(blobs.len(), 1, "one service, one blob");
    blobs.remove(0)
}

/// `(primary_type_symbol, defined_in as (file, line, symbol))` of the
/// consumer response entry for the call at `src/client.ts:line`.
fn anchor(blob: &serde_json::Value, line: u64) -> (Option<String>, Option<(String, u64, String)>) {
    let entries: Vec<&serde_json::Value> = blob["type_manifest"]
        .as_array()
        .expect("type_manifest")
        .iter()
        .filter(|entry| {
            entry["role"] == "consumer"
                && entry["type_kind"] == "response"
                && entry["file_path"] == "src/client.ts"
                && entry["line_number"] == line
        })
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "one consumer response entry at src/client.ts:{line}: {entries:#?}"
    );
    let entry = entries[0];
    let home = entry.get("defined_in").filter(|home| !home.is_null());
    (
        entry["primary_type_symbol"].as_str().map(str::to_string),
        home.map(|home| {
            (
                home["file_path"].as_str().unwrap_or_default().to_string(),
                home["line_number"].as_u64().unwrap_or_default(),
                home["symbol"].as_str().unwrap_or_default().to_string(),
            )
        }),
    )
}

/// An anchor at `symbol`, declared at `src/shapes.ts:line`.
fn at(symbol: &str, line: u64) -> (Option<String>, Option<(String, u64, String)>) {
    (
        Some(symbol.to_string()),
        Some(("src/shapes.ts".to_string(), line, symbol.to_string())),
    )
}

#[test]
fn a_stated_generic_body_is_anchored_at_its_outer_named_type() {
    let blob = scan(&fixture_dir());

    // An instantiation is anchored at the generic, with the generic's own
    // declaration as its home: behind `Promise`, cast after the await,
    // annotated on a declaration, and through `| null`. The model's pick
    // does not move it: the element inside, nothing, or the generic itself.
    for line in [5, 11, 17, 48] {
        assert_eq!(anchor(&blob, line), at("Envelope", 13), "client.ts:{line}");
    }

    // An alias of an instantiation is the name the source states, so the row
    // is anchored at the alias, not at the generic behind it.
    assert_eq!(anchor(&blob, 24), at("OrderEnvelope", 19));

    // A generic with a defaulted argument is one anchor, written bare or
    // with the argument.
    assert_eq!(anchor(&blob, 30), at("Listing", 22));
    assert_eq!(anchor(&blob, 36), at("Listing", 22));
}

#[test]
fn a_statement_with_no_single_repo_declared_name_anchors_nothing() {
    let blob = scan(&fixture_dir());

    // A union of two instantiations states no single name. The model's
    // `Order` is inside both members and is not the body either.
    assert_eq!(anchor(&blob, 42), (None, None));

    // `Record` is the compiler's, so there is no home in the repo to state,
    // and its argument is not the body.
    assert_eq!(anchor(&blob, 54), (None, None));
}
