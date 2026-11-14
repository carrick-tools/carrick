//! The type capture reads a payload where the fact layer places it (phase 3
//! step 1, part of carrick#2474).
//!
//! Drives the real scanner binary offline over `tests/fixtures/site-payloads/`
//! with the type sidecar LIVE (no `CARRICK_ALLOW_MISSING_TYPES`, so a sidecar
//! that cannot start fails the test), as the Action runs a scan.
//!
//! `src/orders.ts:14` calls `http.post<Receipt>('/orders', order)` through a
//! package the library store labels an HTTP client whose `post` sends its
//! second argument. The reader's cassette states the call, `POST /orders`,
//! with no locator at all: no call text, no span, no payload text, no type
//! symbol (carrick-cloud#2522's shape).
//!
//! Answer key: the request alias is `Order`'s shape,
//! `{ sku: string; quantity: number }` (the body argument), and the response
//! alias names `Receipt` (the call's result).

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site-payloads")
}

/// The five environment variables that once switched the fact layer on
/// (carrick#3283). Every scan here removes them, so neither the caller's
/// environment nor CI's can mask what a scan with none of them does.
const RETIRED_SWITCHES: [&str; 5] = [
    "CARRICK_SITE_QUESTIONS",
    "CARRICK_POSITION_ROWS",
    "CARRICK_RECEIVER_TRACE",
    "CARRICK_FACT_FILES",
    "CARRICK_SITE_PAYLOADS",
];

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
        .env_remove("CARRICK_ALLOW_MISSING_TYPES");
    for name in RETIRED_SWITCHES {
        cmd.env_remove(name);
    }
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

/// The request at `src/orders.ts:14` is read at the body argument and the
/// response at the call's result, with none of the retired switches set.
fn assert_typed_at_the_slots(blob: &serde_json::Value) {
    let (request, response) = (alias(blob, "request"), alias(blob, "response"));
    // The capture prints a body inferred at an expression structurally.
    let request_type = captured_block(blob, &request);
    let response_type = captured(blob, &response);
    assert!(
        request_type.contains("sku: string") && request_type.contains("quantity: number"),
        "request: {request_type}"
    );
    assert!(
        response_type.contains("Receipt"),
        "response: {response_type}"
    );
}

/// The reader states no locator (carrick-cloud#2522's shape), so only the
/// fact layer's slots can type the request; a scan with no switch set does.
#[test]
fn a_call_the_reader_left_unlocated_is_typed_at_its_payload_slots() {
    assert_typed_at_the_slots(&scan(&fixture_dir(), &[]));
}

/// carrick#3283: no value turns the fact layer off. A scan with each
/// retired switch set to `0` is typed at the slots and writes the manifest
/// and the capture a scan with none set writes.
#[test]
fn a_switch_set_to_zero_changes_nothing() {
    let none = scan(&fixture_dir(), &[]);
    let zero: Vec<(&str, &str)> = RETIRED_SWITCHES.iter().map(|name| (*name, "0")).collect();
    let zeroed = scan(&fixture_dir(), &zero);
    assert_typed_at_the_slots(&zeroed);
    assert_eq!(none["type_manifest"], zeroed["type_manifest"]);
    assert_eq!(none["capture_stub"], zeroed["capture_stub"]);
    assert_eq!(none["mount_graph"], zeroed["mount_graph"]);
    assert_eq!(none["possible_calls"], zeroed["possible_calls"]);
}

/// carrick#2674 P3-1a: a wrapper hands its caller's options on through the
/// rest of a destructured parameter (`src/notes.ts`). The caller whose
/// literal writes no `body` is decided "no body": its request entry names
/// the root reason `no_request_body` and the capture prints `unknown`, not
/// the options literal's own type. The caller that writes a body is typed at
/// the body's value.
#[test]
fn a_caller_that_writes_no_body_through_a_wrapper_is_decided_no_body() {
    let on = scan(&fixture_dir(), &[]);
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
}

/// The root reasons the consumer request entry at `file:line` states.
fn root_reasons(blob: &serde_json::Value, file: &str, line: u64) -> Vec<String> {
    entry_at(blob, "request", file, line)["any_provenance"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["path"] == "")
                .filter_map(|row| row["reason"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// carrick#4165: `tests/fixtures/site-payloads-absent-body/`. A call that
/// writes nothing at the label's body argument, no call up to the row's
/// writing one either, is decided "no body" by that fact, whatever its verb.
/// The store labels `@fixture/http`'s `post` with body argument 1, `remove`
/// with argument 1's `data` key, and `purge` with no body slot.
///
/// | line | call | the request |
/// |---|---|---|
/// | 7 | `http.post("/orders/ping")` | `no_request_body` |
/// | 10 | `bump("1")`, whose `http.post` writes no body | `no_request_body` |
/// | 13 | `send(...)`, whose body parameter has a default | not decided |
/// | 15 | `http.post(..., ...rest)` | not decided |
/// | 17 | `http.remove("/orders/1")` | `no_request_body` |
/// | 19 | `http.remove(..., { headers: {} })` | `no_request_body` (the literal) |
/// | 21 | `http.purge(...)`, no body slot | `not_located` |
/// | 23 | `http.post(..., null)` | typed `null` |
/// | 25 | `http.post("/orders", order)` | typed `Order` |
/// | 28 | `either(...)`, one branch writes a body, one none | not decided |
#[test]
fn a_call_that_writes_no_body_argument_is_decided_no_body() {
    const FILE: &str = "src/orders.ts";
    let repo =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site-payloads-absent-body");
    let on = scan(&repo, &[]);
    let decided = vec!["no_request_body".to_string()];
    for line in [7, 10, 17, 19] {
        assert_eq!(root_reasons(&on, FILE, line), decided, "{FILE}:{line}");
        let alias = alias_at(&on, "request", FILE, line);
        let text = captured(&on, &alias);
        assert_eq!(
            text.trim_end_matches(';'),
            "unknown",
            "{FILE}:{line} {text}"
        );
    }
    for line in [13, 15, 28] {
        let reasons = root_reasons(&on, FILE, line);
        assert!(!reasons.contains(&decided[0]), "{FILE}:{line} {reasons:?}");
    }
    assert_eq!(
        root_reasons(&on, FILE, 21),
        vec!["not_located".to_string()],
        "{FILE}:21"
    );
    let null = captured(&on, &alias_at(&on, "request", FILE, 23));
    assert_eq!(null.trim_end_matches(';'), "null", "{null}");
    let order = captured_block(&on, &alias_at(&on, "request", FILE, 25));
    assert!(order.contains("sku: string"), "{order}");
}

/// carrick#4324: a wrapper that writes the body onto the options its caller
/// hands it sends a body the caller's literal does not show. Lines 33 and 38 of
/// `tests/fixtures/site-payloads-absent-body/src/orders.ts`
/// (`stamp(..., { headers: {} })`, whose `stamp` sets `init.data`) is not
/// decided "no body".
#[test]
fn a_wrapper_that_writes_onto_the_options_it_is_handed_is_not_decided_no_body() {
    let repo =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site-payloads-absent-body");
    let on = scan(&repo, &[]);
    // Line 38 is the same through a helper the wrapper hands the options to.
    for line in [33, 38] {
        let reasons = root_reasons(&on, "src/orders.ts", line);
        assert_eq!(
            reasons,
            vec!["not_located".to_string()],
            "{line} {reasons:?}"
        );
    }
}

/// carrick#4294: a call below the deciding call that writes a closed options
/// literal lacking the body key states the request body absent, as the
/// deciding call's own literal does. `tests/fixtures/site-payloads-absent-body/`
/// `src/orders.ts`:
///
/// | line | call | the request |
/// |---|---|---|
/// | 42 | `drop(...)`, whose `http.remove` writes `{ headers: {} }` | `no_request_body` |
/// | 45 | `makeRemover(...)`, the same inside a returned function | `no_request_body` |
/// | 49 | `dropWith(...)`, whose literal spreads the caller's options | not decided |
/// | 57 | `dropBuilt(...)`, whose options are a local a branch may extend | not decided |
/// | 61 | `stampVia(...)`, the literal handed to a hop that writes the body | not decided |
#[test]
fn a_closed_literal_below_the_deciding_call_is_decided_no_body() {
    const FILE: &str = "src/orders.ts";
    let repo =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site-payloads-absent-body");
    let on = scan(&repo, &[]);
    let decided = "no_request_body".to_string();
    for line in [42, 45] {
        assert_eq!(
            root_reasons(&on, FILE, line),
            vec![decided.clone()],
            "{FILE}:{line}"
        );
    }
    for line in [49, 57, 61] {
        let reasons = root_reasons(&on, FILE, line);
        assert!(!reasons.contains(&decided), "{FILE}:{line} {reasons:?}");
    }
}

/// carrick#2472 C3: a pub/sub payload a slot types is read there, not at the
/// symbol the reader named.
///
/// `tests/fixtures/site-payloads-pubsub/`: a bus the library store labels
/// (`publish` sends its second argument), whose declarations do not verify the
/// publish, so each call stands as a site row (as in `site_row_span_test`).
/// The reader's cassette names, for `src/orders.ts:4`, a symbol the file does
/// not import (`OrderCreated`, from `src/types.ts`, an unrelated shape) and an
/// envelope copy of the call as the payload locator, which the envelope guard
/// treats as no locator and so does not demote the symbol to inference.
/// `src/relay.ts` is the same shape through a function the store does not
/// label: no site, no slot.
///
/// | alias | the scan |
/// |---|---|
/// | `orders.created` (slotted, borrowed symbol) | the envelope's own shape, not `OrderCreated` |
/// | `orders.paid` (slotted, no symbol) | the envelope's own shape |
/// | `orders.shipped` (no slot, borrowed symbol) | `OrderShipped` |
#[test]
fn a_borrowed_pubsub_symbol_gives_way_to_the_slot_that_types_the_alias() {
    let repo =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site-payloads-pubsub");
    let on = scan(&repo, &[]);
    let type_at = |blob: &serde_json::Value, file: &str, line: u64| {
        let alias = alias_at(blob, "response", file, line);
        captured_block(blob, &alias)
    };

    let created_on = type_at(&on, "src/orders.ts", 4);
    assert!(
        created_on.contains("quantity: number") && !created_on.contains("OrderCreated"),
        "on: {created_on}"
    );

    // A slotted alias with no symbol is typed at the slot.
    let paid = type_at(&on, "src/orders.ts", 5);
    assert!(paid.contains("sku: string"), "paid: {paid}");

    // No slot: the guard decides, as before.
    let shipped = type_at(&on, "src/relay.ts", 9);
    assert!(shipped.contains("OrderShipped"), "shipped: {shipped}");
}

/// Every pub/sub entry of `role` at `file:line`.
fn pubsub_entries<'a>(
    blob: &'a serde_json::Value,
    role: &str,
    file: &str,
    line: u64,
) -> Vec<&'a serde_json::Value> {
    blob["type_manifest"]
        .as_array()
        .expect("type_manifest")
        .iter()
        .filter(|entry| {
            entry["role"] == role && entry["file_path"] == file && entry["line_number"] == line
        })
        .collect()
}

/// carrick#3089: `tests/fixtures/site-payloads-pubsub-verified/`, a bus the
/// library store labels (`publish` sends argument 1, `subscribe` hands
/// argument 1) whose declarations VERIFY both, so each call is a library
/// row, not a site row. The reader's cassette states only the `jobs.named`
/// publish, naming `Job`.
///
/// | call | entry | the type |
/// |---|---|---|
/// | `jobs.created` publish, `:6` | one consumer, site-id'd | the payload's shape |
/// | `jobs.done` subscribe, `:10` | one producer | the handler parameter's shape |
/// | `jobs.named` publish, `:17` | one consumer, site-id'd | `Job` |
#[test]
fn a_verified_library_pubsub_call_is_anchored_and_typed_at_its_slot() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/site-payloads-pubsub-verified");
    let on = scan(&repo, &[]);
    let manifest = &on["type_manifest"];

    let one = |blob: &serde_json::Value, role: &str, line: u64| -> String {
        let entries = pubsub_entries(blob, role, "src/jobs.ts", line);
        assert_eq!(
            entries.len(),
            1,
            "one {role} entry at src/jobs.ts:{line}: {manifest:#}"
        );
        entries[0]["type_alias"].as_str().unwrap().to_string()
    };

    let created = one(&on, "consumer", 6);
    assert!(
        created.contains("_Call"),
        "a publisher is site-id'd: {created}"
    );
    let created_type = captured_block(&on, &created);
    assert!(
        created_type.contains("jobId: string") && created_type.contains("priority: number"),
        "created: {created_type}"
    );

    let done = one(&on, "producer", 10);
    let done_type = captured_block(&on, &done);
    assert!(
        done_type.contains("jobId: string") && done_type.contains("ok: boolean"),
        "done: {done_type}"
    );

    let named = one(&on, "consumer", 17);
    let named_type = captured_block(&on, &named);
    assert!(
        named_type.contains("Job") || named_type.contains("owner: string"),
        "named: {named_type}"
    );

    let aliases: Vec<&str> = manifest
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["type_alias"].as_str())
        .collect();
    let unique: std::collections::BTreeSet<&&str> = aliases.iter().collect();
    assert_eq!(unique.len(), aliases.len(), "no alias twice: {aliases:#?}");
}

/// carrick#4245: `tests/fixtures/site-payloads-pubsub-loop/`, a publisher and
/// a subscriber each registered in a loop over a list of topics
/// (`src/publish.ts` over a same-file list, `src/listen.ts` over an imported
/// one, both `as const`). The reader's cassettes state no pub/sub row, and the
/// site questions answer only the kind (`topic`): each element is a row, and
/// the publisher's rows pair with the subscriber's.
#[test]
fn a_loop_over_a_stated_list_of_topics_writes_a_row_per_element_that_pairs() {
    let repo =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site-payloads-pubsub-loop");
    let on = scan(&repo, &[]);
    // The topics each row of `array` in `file` names.
    let topics_in = |array: &str, file: &str| -> Vec<String> {
        let mut topics: Vec<String> = on[array]
            .as_array()
            .unwrap_or_else(|| panic!("a {array} array"))
            .iter()
            .filter(|row| {
                row["file_path"]
                    .as_str()
                    .is_some_and(|at| at.starts_with(file))
                    && row["key"]["protocol"] == "pubsub"
            })
            .filter_map(|row| row["key"]["topic"].as_str().map(str::to_string))
            .collect();
        topics.sort();
        topics
    };
    let elements = vec!["feed.created".to_string(), "feed.updated".to_string()];
    assert_eq!(topics_in("calls", "src/publish.ts"), elements, "{on:#}");
    assert_eq!(topics_in("endpoints", "src/listen.ts"), elements, "{on:#}");
}

/// The type the stub captures for `alias`, its whitespace collapsed and its
/// closing `;` dropped.
fn captured_flat(blob: &serde_json::Value, alias: &str) -> String {
    captured_block(blob, alias)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(';')
        .to_string()
}

/// carrick#4319 T1: `tests/fixtures/site-maker-response/`. Where the
/// deciding call only makes the function that sends (`makeGate({ base })`
/// returns a closure that calls `http.remove`), the site row's response is
/// read at the labelled call: what the request answers, never the closure.
/// Where the deciding call runs the send, it stays read there. The answer
/// key is the fixture's README.
#[test]
fn a_maker_decided_site_reads_its_response_at_the_labelled_call() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site-maker-response");
    let on = scan(&repo, &[]);
    let response =
        |file: &str, line: u64| captured_flat(&on, &alias_at(&on, "response", file, line));

    // Maker sites: read at the labelled call.
    for (file, line, wanted) in [
        ("src/gate.ts", 7, "{ ok: boolean; }"),
        ("src/poster.ts", 7, "{ id: string; }"),
        ("src/twice.ts", 6, "{ up: boolean; }"),
        ("src/twice.ts", 7, "{ up: boolean; }"),
        ("src/partial.ts", 6, "{ swept: number; }"),
        ("src/handed.ts", 6, "{ flushed: boolean; }"),
        ("src/pinger.ts", 6, "{ up: boolean; }"),
        ("src/pinger.ts", 7, "{ up: boolean; }"),
    ] {
        assert_eq!(response(file, line), wanted, "response at {file}:{line}");
    }
    // The maker's own type parameter is open at the labelled call: a decided
    // abstain, not the closure.
    let typed = entry_at(&on, "response", "src/typed-maker.ts", 6);
    assert_eq!(
        captured_flat(&on, typed["type_alias"].as_str().unwrap()),
        "unknown",
        "{typed:#?}"
    );
    let reasons: Vec<&str> = typed["any_provenance"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row["reason"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(reasons, vec!["bound_by_callers"], "{typed:#?}");
    // The request slot does not move.
    let order = captured_flat(&on, &alias_at(&on, "request", "src/poster.ts", 7));
    assert_eq!(order, "{ sku: string; qty: number; }");
    // A key with an environment base has no entry, before and after.
    assert!(
        on["type_manifest"]
            .as_array()
            .expect("type_manifest")
            .iter()
            .all(|entry| !(entry["file_path"] == "src/env-maker.ts" && entry["line_number"] == 6)),
        "no entry at src/env-maker.ts:6"
    );

    // Wrappers that run the send, and the labelled call itself: unchanged.
    for (file, line, wanted) in [
        ("src/generic.ts", 6, "{ rows: number; }"),
        ("src/local-run.ts", 7, "{ done: boolean; }"),
        ("src/retried.ts", 8, "{ n: number; }"),
        ("src/direct.ts", 3, "{ ok: boolean; }"),
    ] {
        assert_eq!(response(file, line), wanted, "response at {file}:{line}");
    }
}

/// What the blob holds at `file:line` of the model's row: its call rows,
/// its graph rows and its consumer manifest entries.
fn model_row_at(blob: &serde_json::Value, file: &str, line: u64) -> Vec<String> {
    let place = format!("{file}:{line}");
    let list = |value: &serde_json::Value| value.as_array().cloned().unwrap_or_default();
    let mut found: Vec<String> = list(&blob["calls"])
        .iter()
        .filter(|row| row["file_path"] == place.as_str() && row["resolution_source"] == "model")
        .map(|row| format!("calls {}", row["key"]))
        .collect();
    found.extend(
        list(&blob["mount_graph"]["data_calls"])
            .iter()
            .filter(|row| row["file_location"] == place.as_str())
            .map(|row| format!("graph {} {}", row["method"], row["target_url"])),
    );
    found.extend(
        list(&blob["type_manifest"])
            .iter()
            .filter(|entry| {
                entry["role"] == "consumer"
                    && entry["file_path"] == file
                    && entry["line_number"] == line
            })
            .map(|entry| format!("manifest {}", entry["type_alias"])),
    );
    found
}

/// carrick#4319 T2: `tests/fixtures/site-maker-response/`. Where every
/// deciding call of a labelled request only makes the function that sends,
/// each holds the request's fact row, and that row reads the response at
/// the labelled call (T1), the model's row at the labelled call is the same
/// request stated again: it goes from the calls, the graph and the manifest.
/// A maker with a caller left open (a lead), one handed on as a value, a
/// key with an environment base, and every wrapper that runs the send keep
/// the model's row. No type is lost: each retired row's type is read at
/// the site row. The answer key is the fixture's README.
#[test]
fn a_model_row_at_a_maker_sites_labelled_call_is_withdrawn() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site-maker-response");
    let on = scan(&repo, &[]);

    for (file, line) in [
        ("src/gate.ts", 5),
        ("src/poster.ts", 5),
        ("src/twice.ts", 4),
        ("src/typed-maker.ts", 4),
    ] {
        let left = model_row_at(&on, file, line);
        assert!(
            left.is_empty(),
            "the model's row at {file}:{line} stays: {left:#?}"
        );
    }
    for (file, line) in [
        ("src/partial.ts", 4),
        ("src/handed.ts", 4),
        ("src/env-maker.ts", 4),
        ("src/generic.ts", 4),
        ("src/local-run.ts", 4),
        ("src/retried.ts", 5),
        ("src/pinger.ts", 4),
    ] {
        let kept = model_row_at(&on, file, line);
        assert!(
            kept.iter().any(|row| row.starts_with("calls ")),
            "the model's row at {file}:{line} goes: {kept:#?}"
        );
    }

    // No type is lost: the site rows read what the retired rows read.
    let response =
        |file: &str, line: u64| captured_flat(&on, &alias_at(&on, "response", file, line));
    for (file, line, wanted) in [
        ("src/gate.ts", 7, "{ ok: boolean; }"),
        ("src/poster.ts", 7, "{ id: string; }"),
        ("src/twice.ts", 6, "{ up: boolean; }"),
        ("src/twice.ts", 7, "{ up: boolean; }"),
    ] {
        assert_eq!(response(file, line), wanted, "response at {file}:{line}");
    }
    let order = captured_flat(&on, &alias_at(&on, "request", "src/poster.ts", 7));
    assert_eq!(order, "{ sku: string; qty: number; }");
    // The environment-based maker's model row keeps the only type it has.
    assert_eq!(
        response("src/env-maker.ts", 4),
        "{ trimmed: number; }",
        "response at src/env-maker.ts:4"
    );
}
