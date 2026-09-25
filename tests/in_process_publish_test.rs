//! Pub/sub rows whose call goes into an in-process wrapper (carrick#1513).
//!
//! Drives the real scanner binary, offline, over
//! `tests/fixtures/in-process-publish/`, with the model's answers replayed
//! from `__llm__/`. The answer key reports EVERY `publish`/`subscribe` call
//! as a pub/sub operation, as the file-analyzer does when it sees only the
//! caller's file. What the scan keeps is decided by what each call reaches.
//! The fixture's README lists the sites and why each one goes or stays.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/in-process-publish")
}

/// One pub/sub row as the blob holds it: side, topic, `file:line`.
type Row = (String, String, String);

struct Scan {
    rows: BTreeSet<Row>,
    /// Pub/sub type-manifest anchors: (role, topic, `file:line`).
    anchors: BTreeSet<Row>,
    blob: serde_json::Value,
    stderr: String,
}

/// One isolated first scan of the fixture.
fn scan() -> Scan {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    scan_in(storage.path(), cache.path())
}

/// A scan that stores its blob in `storage` and reads the previous one back
/// from there, so a second call takes the incremental path.
fn scan_in(storage: &Path, cache: &Path) -> Scan {
    let cassettes = fixture_dir().join("__llm__");

    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(fixture_dir())
        .env("CARRICK_LOCAL_STORAGE_DIR", storage)
        .env("CARRICK_CACHE_DIR", cache)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        // The row tests hold without the sidecar; the one test that needs it
        // says so when it is missing.
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
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "fixture scan exited non-zero:\n{stderr}"
    );

    let mut blobs = std::fs::read_dir(storage)
        .expect("storage dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    blobs.sort();
    assert_eq!(blobs.len(), 1, "expected one written blob, got {blobs:?}");
    let blob: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&blobs[0]).expect("read blob")).expect("parse blob");

    let mut rows = BTreeSet::new();
    for side in ["endpoints", "calls"] {
        for row in blob[side].as_array().expect("blob side is an array") {
            if row["key"]["protocol"] == "pubsub" {
                rows.insert((
                    side.to_string(),
                    row["key"]["topic"].as_str().expect("topic").to_string(),
                    row["file_path"].as_str().expect("file_path").to_string(),
                ));
            }
        }
    }
    let mut anchors = BTreeSet::new();
    for entry in blob["type_manifest"].as_array().into_iter().flatten() {
        if entry["protocol"] == "pubsub" {
            anchors.insert((
                entry["role"].as_str().expect("role").to_string(),
                entry["topic"].as_str().expect("topic").to_string(),
                format!(
                    "{}:{}",
                    entry["file_path"].as_str().expect("file_path"),
                    entry["line_number"]
                ),
            ));
        }
    }
    Scan {
        rows,
        anchors,
        blob,
        stderr,
    }
}

/// Every generated type alias (`Endpoint_<hash>_Response...`) written in `text`.
fn aliases_in(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|token| token.starts_with("Endpoint_") && token.contains("_Response"))
        .map(str::to_owned)
        .collect()
}

fn call(topic: &str, site: &str) -> Row {
    ("calls".to_string(), topic.to_string(), site.to_string())
}

fn endpoint(topic: &str, site: &str) -> Row {
    ("endpoints".to_string(), topic.to_string(), site.to_string())
}

const ORDERS: &str = "src/orders/orders.service.ts";
const LISTENER: &str = "src/inventory/inventory.listener.ts";
const SHIPPING: &str = "src/shipping/shipping.service.ts";

#[test]
fn only_calls_that_reach_a_transport_or_a_counterpart_stay_pubsub_rows() {
    let scan = scan();

    // Kept. Each stays for a different reason, so dropping every row
    // cannot pass this test.
    let kept = [
        // The wrapper's module imports the package detection lists as the
        // messaging client.
        call("order.placed", &format!("{ORDERS}:24")),
        // The wrapper sends through a transport it was handed, which proves
        // nothing about where it goes.
        call("order.cancelled", &format!("{ORDERS}:31")),
        // A call on the package client itself resolves to no repo function.
        call("order.archived", &format!("{ORDERS}:32")),
        // In-process, but the service subscribes to the same topic: the pair
        // is a contract, as an emitter's `emit`/`on` pair is.
        call("inventory.reserved", &format!("{ORDERS}:26")),
        endpoint("inventory.reserved", &format!("{LISTENER}:9")),
        // A field constructed from a runtime global: the globals include
        // sockets.
        call("shipment.sent", &format!("{SHIPPING}:15")),
        // A global called with arguments (`fetch` as much as anything), next
        // to a push onto the class's own list.
        call("shipment.sent", &format!("{SHIPPING}:16")),
        // A constructed field the class reassigns: whatever was attached last
        // is what the call reaches.
        call("shipment.sent", &format!("{SHIPPING}:17")),
        // No call at all, only a write to a handed-in object: nothing shows
        // the value staying here.
        call("shipment.sent", &format!("{SHIPPING}:18")),
    ];
    for row in &kept {
        assert!(
            scan.rows.contains(row),
            "expected {row:?} to stay a pub/sub row; rows: {:#?}",
            scan.rows
        );
    }

    // Withdrawn: in-process with nothing on the other side of the topic.
    let withdrawn = [
        // A field constructed from a declared dependency.
        call("order.placed", &format!("{ORDERS}:23")),
        // A second call to the same wrapper from the same function: the call
        // graph keeps the first site only.
        call("order.totals_changed", &format!("{ORDERS}:25")),
        // A field constructed from the repo's own listener-list class.
        call("order.cancelled", &format!("{ORDERS}:30")),
        // A module-scope instance behind a plain function.
        call("order.noted", &format!("{ORDERS}:33")),
        // The subscribing side, on a line where a callback is its own
        // definition.
        endpoint("stock.checked", &format!("{LISTENER}:10")),
    ];
    for row in &withdrawn {
        assert!(
            !scan.rows.contains(row),
            "expected {row:?} to be withdrawn as an in-process call; rows: {:#?}",
            scan.rows
        );
    }

    assert_eq!(
        scan.rows.len(),
        kept.len(),
        "unexpected pub/sub rows: {:#?}",
        scan.rows
    );
}

#[test]
fn a_withdrawn_row_leaves_no_type_anchor() {
    let scan = scan();
    let sites: BTreeSet<&str> = scan
        .anchors
        .iter()
        .map(|(_, _, site)| site.as_str())
        .collect();
    for site in [
        format!("{ORDERS}:23"),
        format!("{ORDERS}:25"),
        format!("{ORDERS}:30"),
        format!("{ORDERS}:33"),
        format!("{LISTENER}:10"),
    ] {
        assert!(
            !sites.contains(site.as_str()),
            "a withdrawn row kept its manifest anchor at {site}: {:#?}",
            scan.anchors
        );
    }
    assert_eq!(
        scan.anchors.len(),
        scan.rows.len(),
        "every kept row has one anchor: anchors {:#?}, rows {:#?}",
        scan.anchors,
        scan.rows
    );
}

/// A withdrawn row asks the sidecar for nothing: every type the scan resolved
/// and every declaration its stub carries belongs to a manifest entry. Needs
/// the sidecar built (`src/sidecar`), as CI builds it before this step.
#[test]
fn a_withdrawn_row_resolves_no_type() {
    let scan = scan();
    let manifest: BTreeSet<String> = scan.blob["type_manifest"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["type_alias"].as_str().map(str::to_owned))
        .collect();
    for field in ["bundled_types", "capture_stub"] {
        let value = &scan.blob[field];
        assert!(
            !value.is_null(),
            "the blob has no {field}: the sidecar did not run, so this test checks nothing"
        );
        let resolved = aliases_in(&value.to_string());
        let orphans: Vec<&String> = resolved.difference(&manifest).collect();
        assert!(
            orphans.is_empty(),
            "{field} names aliases no manifest entry holds (a withdrawn row's type was resolved): {orphans:?}"
        );
    }
}

/// The second scan of an unchanged tree reuses the model's cached answers and
/// builds its rows on the incremental path, which withdraws the same rows.
#[test]
fn a_rescan_withdraws_the_same_rows() {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let first = scan_in(storage.path(), cache.path());
    let second = scan_in(storage.path(), cache.path());
    assert!(
        second.stderr.contains("already analysed"),
        "the second scan did not reuse the cached answers, so it did not take the incremental path:\n{}",
        second.stderr
    );
    assert!(
        !second
            .rows
            .contains(&call("order.placed", &format!("{ORDERS}:23"))),
        "the incremental path kept an in-process row: {:#?}",
        second.rows
    );
    assert_eq!(first.rows, second.rows);
    assert_eq!(first.anchors, second.anchors);
}
