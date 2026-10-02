//! A response type imported through an alias the repo's own config declares
//! is served as the endpoint's type (carrick#1416).
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/alias-shaped-type-imports/` with the type sidecar LIVE (no
//! `CARRICK_ALLOW_MISSING_TYPES`, so a sidecar that fails to start fails the
//! test instead of reading as a missing type).
//!
//! Every route's response type is imported by a specifier no relative-path
//! join can answer, each declared in a different shape of config, and each
//! shape is one the resolver this replaced could not read at all:
//!
//! | route | specifier | declared in |
//! |---|---|---|
//! | `GET /shipments/:id` | `@shipping/shipment.ts` | a Deno import map (`src/shipping/deno.json`) |
//! | `GET /invoices/:number` | `@invoicing/contracts/invoice` | a tsconfig that only `extends` the one with `paths` |
//! | `GET /receipts/:reference` | `~receipts/receipt` | a tsconfig written as JSONC, with comments and a trailing comma |
//!
//! Before carrick#1416 each specifier reached the sidecar unresolved, as
//! itself, and every one of these rows carried no type.
//!
//! One client file casts responses to types imported the same way: one whose
//! declaration has a member typed `unknown`, and one whose `unknown` member is
//! followed by a member typed through a module that was never generated
//! (carrick#1752).
//!
//! Two shapes this fixture deliberately does NOT use, because each would
//! answer a different question than the one above:
//!
//! - a `#name` specifier, which Node resolves through the importer's own
//!   `package.json` `imports` and never through a tsconfig `paths` entry, so
//!   writing one in `paths` tests the wrong rule;
//! - a `tsconfig` named for the service in `carrick.json`, which governs
//!   every file under the service directory IN PLACE OF the nearest config
//!   (carrick#1104) and would shadow the three configs under test.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/alias-shaped-type-imports")
}

/// The service's blob after one offline scan of the fixture.
fn scan_blob() -> serde_json::Value {
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
    assert_eq!(blob["service_name"], "alias-api");
    blob
}

/// `METHOD path` -> `(type_state, expanded_definition)` for every HTTP
/// RESPONSE entry of the service's type manifest on the `role` side.
fn response_types(
    blob: &serde_json::Value,
    role: &str,
) -> BTreeMap<String, (String, Option<String>)> {
    blob["type_manifest"]
        .as_array()
        .expect("type_manifest")
        .iter()
        .filter(|entry| {
            entry["role"] == role
                && entry["type_kind"] == "response"
                && entry["protocol"] != "graphql"
        })
        .map(|entry| {
            (
                format!(
                    "{} {}",
                    entry["method"].as_str().unwrap_or_default(),
                    entry["path"].as_str().unwrap_or_default()
                ),
                (
                    entry["type_state"].as_str().unwrap_or_default().to_string(),
                    entry["expanded_definition"].as_str().map(str::to_string),
                ),
            )
        })
        .collect()
}

#[test]
fn a_response_type_behind_each_alias_shape_is_served() {
    let types = response_types(&scan_blob(), "producer");
    let typed = |route: &str| -> (String, Option<String>) {
        types
            .get(route)
            .unwrap_or_else(|| panic!("no manifest entry for {route}: {types:?}"))
            .clone()
    };

    assert_eq!(
        typed("GET /shipments/:id"),
        (
            "explicit".to_string(),
            Some("{ id: string; carrier: string; }".to_string())
        ),
        "a Deno import map's prefix mapping names the file the type is in"
    );
    assert_eq!(
        typed("GET /invoices/:number"),
        (
            "explicit".to_string(),
            Some("{ number: string; cents: number; }".to_string())
        ),
        "a `paths` mapping reached only through an `extends` chain still resolves"
    );
    assert_eq!(
        typed("GET /receipts/:reference"),
        (
            "explicit".to_string(),
            Some("{ reference: string; paid: boolean; }".to_string())
        ),
        "a tsconfig with comments and a trailing comma is read as JSONC, not refused"
    );
}

/// carrick#1752: a client casts `response.json()` to a declared type, imported
/// through the same kind of alias, that has one member the declaration itself
/// types `unknown`. The row is the typed contract it states, with one open
/// field, not an unknown row: it stays `explicit` and serves every member.
///
/// From the release that first resolved these aliases (carrick#1416) until
/// this one, the capture read the real declaration, found the `unknown`, and
/// the whole row read `unknown` although its definition held the full shape.
#[test]
fn a_cast_response_with_one_open_member_stays_explicit() {
    let blob = scan_blob();
    let types = response_types(&blob, "consumer");
    let (state, expanded) = types
        .get("GET /statements/:number")
        .unwrap_or_else(|| panic!("no consumer response entry: {types:?}"))
        .clone();
    assert_eq!(
        (state.as_str(), expanded.as_deref()),
        (
            "explicit",
            Some("{ number: string; cents: number; memo: unknown; }")
        ),
        "the cast states the contract; `memo` is its one open field"
    );

    // The same cast to a type whose open member comes first and whose next
    // member is typed through a module the checkout does not have. The
    // capture repairs that member to `unknown` too (carrick#1397), and that
    // one is not the author's: the row stays `unknown`, still served.
    let (state, expanded) = types
        .get("GET /ledgers/:number")
        .unwrap_or_else(|| panic!("no consumer response entry: {types:?}"))
        .clone();
    assert_eq!(
        (state.as_str(), expanded.as_deref()),
        (
            "unknown",
            Some("{ number: string; memo: unknown; row: unknown; }")
        ),
        "an import that did not resolve is not an open field the source declared"
    );
}
