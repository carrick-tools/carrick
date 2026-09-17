//! Fixture-driven test of Socket.IO contract extraction over real server and
//! client files: import-anchored socket identification, direction
//! assignment, reserved-event filtering, and dynamic-name skipping.

use carrick::socket_io::scan_files;
use std::path::PathBuf;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/socket-service")
}

#[test]
fn scan_files_extracts_directional_socket_contract() {
    let root = fixture_root();
    let files = vec![root.join("src/server.ts"), root.join("src/client.ts")];

    let extraction = scan_files(&files, &[]);

    let mut listeners: Vec<String> = extraction
        .listeners
        .iter()
        .map(|op| op.key.canonical())
        .collect();
    listeners.sort();
    assert_eq!(
        listeners,
        vec![
            // server listens for client->server messages
            "socket|CLIENT->SERVER|chat:message",
            "socket|CLIENT->SERVER|typing",
            // client listens for server->client broadcasts
            "socket|SERVER->CLIENT|chat:broadcast",
        ],
        "listeners carry the direction of messages they receive"
    );

    let mut emitters: Vec<String> = extraction
        .emitters
        .iter()
        .map(|op| op.key.canonical())
        .collect();
    emitters.sort();
    assert_eq!(
        emitters,
        vec![
            "socket|CLIENT->SERVER|chat:message",
            "socket|CLIENT->SERVER|presence:ping",
            "socket|SERVER->CLIENT|chat:broadcast",
            "socket|SERVER->CLIENT|user:left",
            "socket|SERVER->CLIENT|user:typing",
        ],
        "emitters carry the direction of messages they send; dynamic names are skipped"
    );

    // Server listeners and client emitters meet on the same key.
    let listener_keys: std::collections::HashSet<_> =
        extraction.listeners.iter().map(|op| &op.key).collect();
    let matched = extraction
        .emitters
        .iter()
        .filter(|op| listener_keys.contains(&op.key))
        .count();
    assert_eq!(
        matched, 2,
        "chat:message (client->server) and chat:broadcast (server->client) should match"
    );
}
