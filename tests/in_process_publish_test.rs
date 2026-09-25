//! Pub/sub rows whose call goes into an in-process wrapper (carrick#1513).
//!
//! Drives the real scanner binary, offline, over
//! `tests/fixtures/in-process-publish/`, with the model's answers replayed
//! from `__llm__/`. The answer key reports EVERY `publish`/`subscribe` call
//! as a pub/sub operation, as the file-analyzer does when it sees only the
//! caller's file. What the scan keeps is decided by what each call reaches.
//! The fixture's README lists the sites and why each one goes or stays.

use std::collections::BTreeSet;
use std::path::PathBuf;
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
}

fn scan() -> Scan {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let cassettes = fixture_dir().join("__llm__");

    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(fixture_dir())
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        // The assertion is on which rows exist; the type layer is not under test.
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
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "fixture scan exited non-zero:\n{stderr}"
    );

    let mut blobs = std::fs::read_dir(storage.path())
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
    Scan { rows, anchors }
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
        call("shipment.sent", &format!("{SHIPPING}:13")),
        // A global called with arguments: `fetch` as much as anything.
        call("shipment.sent", &format!("{SHIPPING}:14")),
        // A constructed field the class reassigns: whatever was attached last
        // is what the call reaches.
        call("shipment.sent", &format!("{SHIPPING}:15")),
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
