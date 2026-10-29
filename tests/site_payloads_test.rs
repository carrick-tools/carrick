//! The type capture reads a payload where the fact layer places it (phase 3
//! step 1, part of carrick#2474).
//!
//! Drives the real scanner binary offline over `tests/fixtures/site-payloads/`
//! with the type sidecar LIVE (no `CARRICK_ALLOW_MISSING_TYPES`, so a sidecar
//! that cannot start fails the test) and the compiler's position rows on
//! (`CARRICK_POSITION_ROWS=1`), as the Action runs a scan with them.
//!
//! `src/orders.ts:14` calls `http.post<Receipt>('/orders', order)` through a
//! package the library store labels an HTTP client whose `post` sends its
//! second argument. The reader's cassette states the call, `POST /orders`,
//! with no locator at all: no call text, no span, no payload text, no type
//! symbol (carrick-cloud#2522's shape).
//!
//! Answer key:
//!
//! | switch | request alias | response alias |
//! |---|---|---|
//! | off | `unknown` (a manifest placeholder) | names `Receipt` (read at the call's line) |
//! | `CARRICK_SITE_PAYLOADS=1` | `Order`'s shape, `{ sku: string; quantity: number }` (the body argument) | names `Receipt` (the call's result) |

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site-payloads")
}

/// The blob one offline scan of `repo` writes, with `env` set.
fn scan(repo: &Path, env: &[(&str, &str)]) -> serde_json::Value {
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
        .env("CARRICK_POSITION_ROWS", "1")
        .env_remove("CARRICK_ALLOW_MISSING_TYPES")
        .env_remove("CARRICK_SITE_PAYLOADS")
        .env_remove("CARRICK_SITE_QUESTIONS");
    for (name, value) in env {
        cmd.env(name, value);
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
    let mut blobs: Vec<serde_json::Value> = std::fs::read_dir(storage.path())
        .expect("storage dir")
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .map(|entry| {
            serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
                .expect("parse blob")
        })
        .collect();
    assert_eq!(blobs.len(), 1, "one service, one blob");
    blobs.remove(0)
}

/// The consumer `kind` entry at `file:line`.
fn entry_at<'a>(
    blob: &'a serde_json::Value,
    kind: &str,
    file: &str,
    line: u64,
) -> &'a serde_json::Value {
    let entries: Vec<&serde_json::Value> = blob["type_manifest"]
        .as_array()
        .expect("type_manifest")
        .iter()
        .filter(|entry| {
            entry["role"] == "consumer"
                && entry["type_kind"] == kind
                && entry["file_path"] == file
                && entry["line_number"] == line
        })
        .collect();
    assert_eq!(entries.len(), 1, "one consumer {kind} entry: {entries:#?}");
    entries[0]
}

/// The alias of the consumer `kind` entry at `src/orders.ts:14`.
fn alias(blob: &serde_json::Value, kind: &str) -> String {
    alias_at(blob, kind, "src/orders.ts", 14)
}

fn alias_at(blob: &serde_json::Value, kind: &str, file: &str, line: u64) -> String {
    entry_at(blob, kind, file, line)["type_alias"]
        .as_str()
        .unwrap()
        .to_string()
}

/// What the capture stub says `alias` is: the right-hand side of its
/// `export type` line.
fn captured(blob: &serde_json::Value, alias: &str) -> String {
    let files = blob["capture_stub"]["files"]
        .as_object()
        .expect("a capture stub: the sidecar ran");
    let head = format!("export type {alias} = ");
    files
        .values()
        .filter_map(|text| text.as_str())
        .flat_map(str::lines)
        .find_map(|line| line.trim().strip_prefix(&head).map(str::to_string))
        .unwrap_or_else(|| panic!("no `{head}` line in the stub: {files:#?}"))
}

/// The whole declaration of `alias` in the stub's surface, across lines.
fn captured_block(blob: &serde_json::Value, alias: &str) -> String {
    let surface = blob["capture_stub"]["files"]["types/surface.d.ts"]
        .as_str()
        .expect("a capture surface: the sidecar ran");
    let head = format!("export type {alias} = ");
    let start = surface
        .find(&head)
        .unwrap_or_else(|| panic!("no `{head}` in the surface: {surface}"));
    let rest = &surface[start + head.len()..];
    rest[..rest.find("\nexport ").unwrap_or(rest.len())].to_string()
}

#[test]
fn a_call_the_reader_left_unlocated_is_typed_at_its_payload_slots() {
    let off = scan(&fixture_dir(), &[]);
    let on = scan(&fixture_dir(), &[("CARRICK_SITE_PAYLOADS", "1")]);

    // The known negative: the reader states no locator, so off, the request
    // is a placeholder. The response is read at the call's line already
    // (a line-only request finds the call there), and must stay so.
    let (request, response) = (alias(&off, "request"), alias(&off, "response"));
    assert_eq!(captured(&off, &request).trim_end_matches(';'), "unknown");
    assert!(
        captured(&off, &response).contains("Receipt"),
        "response off: {}",
        captured(&off, &response)
    );

    // On, the request is read at the body argument and the response at the
    // call's result.
    assert_eq!(alias(&on, "request"), request, "the aliases do not move");
    // The capture prints a body inferred at an expression structurally.
    let request_type = captured_block(&on, &request);
    let response_type = captured(&on, &response);
    assert!(
        request_type.contains("sku: string") && request_type.contains("quantity: number"),
        "request: {request_type}"
    );
    assert!(
        response_type.contains("Receipt"),
        "response: {response_type}"
    );
}

#[test]
fn with_the_switch_at_anything_but_one_the_blob_is_unchanged() {
    let off = scan(&fixture_dir(), &[]);
    let not_one = scan(&fixture_dir(), &[("CARRICK_SITE_PAYLOADS", "true")]);
    let (request, response) = (alias(&not_one, "request"), alias(&not_one, "response"));
    assert_eq!(captured(&not_one, &request), captured(&off, &request));
    assert_eq!(captured(&not_one, &response), captured(&off, &response));
    assert_eq!(off["type_manifest"], not_one["type_manifest"]);
    assert_eq!(off["capture_stub"], not_one["capture_stub"]);
}

/// carrick#2674 P3-1a: a wrapper hands its caller's options on through the
/// rest of a destructured parameter (`src/notes.ts`). Switch on, the caller
/// whose literal writes no `body` is decided "no body": its request entry
/// names the root reason `no_request_body` and the capture prints `unknown`,
/// not the options literal's own type. The caller that writes a body is typed
/// at the body's value. Switch off, neither is read.
#[test]
fn a_caller_that_writes_no_body_through_a_wrapper_is_decided_no_body() {
    // The global `fetch` is a decided site only with the site questions on,
    // as the Action runs them.
    let off = scan(&fixture_dir(), &[("CARRICK_SITE_QUESTIONS", "1")]);
    let on = scan(
        &fixture_dir(),
        &[
            ("CARRICK_SITE_QUESTIONS", "1"),
            ("CARRICK_SITE_PAYLOADS", "1"),
        ],
    );
    let (note, ping) = (14_u64, 18_u64);

    let ping_entry = entry_at(&on, "request", "src/notes.ts", ping);
    let reasons: Vec<&str> = ping_entry["any_provenance"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["path"] == "")
                .filter_map(|row| row["reason"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(reasons, vec!["no_request_body"], "{ping_entry:#?}");
    let ping_type = captured(&on, ping_entry["type_alias"].as_str().unwrap());
    assert_eq!(ping_type.trim_end_matches(';'), "unknown", "{ping_type}");

    let note_alias = alias_at(&on, "request", "src/notes.ts", note);
    let note_type = captured_block(&on, &note_alias);
    assert!(note_type.contains("text: string"), "request: {note_type}");

    // Off, the options literal's own type is not read as a decision.
    let off_entry = entry_at(&off, "request", "src/notes.ts", ping);
    assert!(
        off_entry["any_provenance"]
            .as_array()
            .is_none_or(|rows| rows.iter().all(|row| row["reason"] != "no_request_body")),
        "{off_entry:#?}"
    );
}

/// carrick#2472 C3: a pub/sub payload a slot types is read there, not at the
/// symbol the reader named.
///
/// `tests/fixtures/site-payloads-pubsub/`: a bus the library store labels
/// (`publish` sends its second argument), whose declarations do not verify the
/// publish, so each call stands as a site row (as in `site_row_span_test`).
/// The reader's cassette names, for `src/orders.ts:4`, a symbol the file does
/// not import (`OrderCreated`, from `src/types.ts`, an unrelated shape) and an
/// envelope copy of the call as the payload locator. No guard drops that
/// locator (carrick#2472 C6a), so with no slot the failing symbol is demoted
/// to inference of the envelope copy.
/// `src/relay.ts` is the same shape through a function the store does not
/// label: no site, no slot.
///
/// | alias | switch off | `CARRICK_SITE_PAYLOADS=1` |
/// |---|---|---|
/// | `orders.created` (slotted, borrowed symbol) | the envelope copy's shape | the slot's shape (same text) |
/// | `orders.paid` (slotted, no symbol) | the slot's shape | the same |
/// | `orders.shipped` (no slot, borrowed symbol) | the envelope copy's shape | the same |
#[test]
fn a_borrowed_pubsub_symbol_gives_way_to_the_slot_that_types_the_alias() {
    let repo =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site-payloads-pubsub");
    let off = scan(&repo, &[("CARRICK_SITE_QUESTIONS", "1")]);
    let on = scan(
        &repo,
        &[
            ("CARRICK_SITE_QUESTIONS", "1"),
            ("CARRICK_SITE_PAYLOADS", "1"),
        ],
    );
    let type_at = |blob: &serde_json::Value, file: &str, line: u64| {
        let alias = alias_at(blob, "response", file, line);
        captured_block(blob, &alias)
    };

    // The borrowed symbol is read neither way: the slot types it on, and
    // demotion to the locator's inference does off.
    for blob in [&off, &on] {
        let created = type_at(blob, "src/orders.ts", 4);
        assert!(
            created.contains("quantity: number") && !created.contains("OrderCreated"),
            "created: {created}"
        );
    }

    // A slotted alias with no symbol is typed at the slot either way.
    for blob in [&off, &on] {
        let paid = type_at(blob, "src/orders.ts", 5);
        assert!(paid.contains("sku: string"), "paid: {paid}");
    }

    // No slot: the failing symbol is demoted to the locator's own shape.
    for blob in [&off, &on] {
        let shipped = type_at(blob, "src/relay.ts", 9);
        assert!(
            shipped.contains("topic: string") && !shipped.contains("OrderShipped"),
            "shipped: {shipped}"
        );
    }
}
