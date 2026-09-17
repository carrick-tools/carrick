//! Fixture-driven test of socket extraction for libraries that are NOT
//! Socket.IO (carrick#1281): a plain-WebSocket service and a channel-style
//! realtime client, both gated by the packages framework-detect labelled socket
//! clients, both keyed with an unknown direction.

use carrick::socket_io::scan_files;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{name}"))
}

fn clients(packages: &[&str]) -> Vec<String> {
    packages.iter().map(|pkg| pkg.to_string()).collect()
}

fn keys(ops: &[carrick::socket_io::SocketOp]) -> Vec<String> {
    let mut keys: Vec<String> = ops.iter().map(|op| op.key.canonical()).collect();
    keys.sort();
    keys
}

#[test]
fn plain_websocket_envelopes_become_unknown_direction_emitters() {
    let root = fixture("socket-ws-service");
    let files = vec![root.join("src/server.ts"), root.join("src/client.ts")];

    let extraction = scan_files(&files, &clients(&["ws"]));

    assert_eq!(
        keys(&extraction.emitters),
        vec![
            "socket|UNKNOWN|order.accepted",
            "socket|UNKNOWN|order.created",
            "socket|UNKNOWN|order.rejected",
        ],
        "the envelope discriminator is the event name; a dynamic one is skipped, \
         and the in-process EventEmitter in the same file is not a socket root"
    );

    // The receiving side of this idiom (`switch (msg.type)` inside the
    // `message` handler) is not extracted yet — carrick#1287. `connection`,
    // `message` and `open` are transport lifecycle and never become contracts,
    // so the fixture's listeners are all correctly declined.
    assert!(
        extraction.listeners.is_empty(),
        "no listener rows yet on a raw socket, got {:?}",
        keys(&extraction.listeners)
    );
}

#[test]
fn a_channel_client_resolves_through_its_method_return() {
    let root = fixture("socket-channel-client");
    let files = vec![root.join("src/orders.ts")];

    let extraction = scan_files(&files, &clients(&["realtime-channels"]));

    assert_eq!(
        keys(&extraction.listeners),
        vec![
            "socket|UNKNOWN|order.created",
            "socket|UNKNOWN|shipment.dispatched",
        ],
        "a bind with a handler is a registration; `client.subscribe(\"presence\")` \
         without one opens a channel and is not"
    );
    assert_eq!(
        keys(&extraction.emitters),
        vec!["socket|UNKNOWN|order.viewed"],
    );

    let anchored = extraction
        .listeners
        .iter()
        .find(|op| op.key.canonical() == "socket|UNKNOWN|order.created")
        .expect("order.created listener");
    assert_eq!(
        anchored.payload_type_symbol.as_deref(),
        Some("OrderCreated"),
        "the handler's parameter type anchors the payload exactly as it does for Socket.IO"
    );
}

#[test]
fn the_two_sides_of_an_unknown_direction_contract_match() {
    let ws = fixture("socket-ws-service");
    let channel = fixture("socket-channel-client");
    let extraction = scan_files(
        &[
            ws.join("src/server.ts"),
            ws.join("src/client.ts"),
            channel.join("src/orders.ts"),
        ],
        &clients(&["ws", "realtime-channels"]),
    );

    let listener_keys: std::collections::HashSet<_> =
        extraction.listeners.iter().map(|op| &op.key).collect();
    let matched: Vec<String> = extraction
        .emitters
        .iter()
        .filter(|op| listener_keys.contains(&op.key))
        .map(|op| op.key.canonical())
        .collect();
    assert_eq!(
        matched,
        vec!["socket|UNKNOWN|order.created"],
        "an unknown-direction emitter and listener meet on one key"
    );
}

#[test]
fn detection_without_the_package_extracts_nothing() {
    let root = fixture("socket-channel-client");
    let files = vec![root.join("src/orders.ts")];

    let extraction = scan_files(&files, &clients(&["kafkajs"]));

    assert!(
        extraction.is_empty(),
        "the gate is the detected package: an unlisted one produces no rows, got {:?} / {:?}",
        keys(&extraction.listeners),
        keys(&extraction.emitters)
    );
}

#[test]
fn socket_io_keeps_its_directional_key_when_it_is_also_a_detected_client() {
    let root = fixture("socket-service");
    let files = vec![root.join("src/server.ts"), root.join("src/client.ts")];

    let precise = scan_files(&files, &[]);
    let also_detected = scan_files(&files, &clients(&["socket.io", "socket.io-client"]));

    assert_eq!(
        keys(&precise.listeners),
        keys(&also_detected.listeners),
        "listing socket.io as a socket client must not replace its directional rules"
    );
    assert_eq!(keys(&precise.emitters), keys(&also_detected.emitters));
    assert!(
        keys(&also_detected.emitters)
            .iter()
            .all(|key| !key.contains("UNKNOWN")),
        "no unknown-direction rows for a library whose sides are known"
    );
}
