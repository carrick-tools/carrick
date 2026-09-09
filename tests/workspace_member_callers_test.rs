//! Call edges into a sibling workspace package (carrick#776).
//!
//! Drives the real scanner binary — offline, cassette-mocked LLM — over
//! `tests/fixtures/workspace-member-callers/`, where the one scanned service
//! calls members on a client class another package publishes under a manifest
//! subpath. Asserts on the UPLOADED BLOB rather than the projection, because
//! the edges live in `function_definitions[].calls` and that is what the
//! cross-service caller join inverts.
//!
//! Pre-fix baseline: `call_graph` resolved neither half of these sites — a
//! receiver bound to an instance resolved to nothing, and a non-relative
//! specifier resolved to nothing — so both positive assertions FAIL on the
//! pre-fix scanner by construction. The four negative sites are the answer key
//! for what must still resolve to nothing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

/// One resolved edge, as the blob records it.
#[derive(Debug, PartialEq, Eq)]
struct Edge {
    callee: String,
    callee_file: String,
    call_site_line: u64,
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workspace-member-callers")
}

/// Scan the fixture offline and return every function's resolved call edges,
/// keyed by definition key.
fn scan_edges() -> HashMap<String, Vec<Edge>> {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let cassettes = fixture_dir().join("__llm__");
    assert!(cassettes.exists(), "fixture cassette dir missing");

    let bin = PathBuf::from(env!("CARGO_BIN_EXE_carrick"));
    let mut cmd = Command::new(&bin);
    cmd.arg(fixture_dir())
        // The blob is only written when the scan believes it should upload,
        // and `CARRICK_OUTPUT_JSON` suppresses that — so this harness reads
        // the local storage dir instead of the eval projection.
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1");
    // Same ambient-CI stripping as the other fixture harnesses: keeps repo
    // identity tied to the scanned dir and the upload decision deterministic.
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
        "fixture scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut blobs = std::fs::read_dir(storage.path())
        .expect("storage dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    blobs.sort();
    assert_eq!(blobs.len(), 1, "expected one uploaded blob, got {blobs:?}");

    let blob: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&blobs[0]).expect("read blob")).expect("parse blob");
    blob["function_definitions"]
        .as_object()
        .expect("blob has no function_definitions")
        .iter()
        .map(|(key, def)| {
            let edges = def["calls"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|call| Edge {
                    callee: call["name"].as_str().unwrap_or_default().to_string(),
                    callee_file: call["file_path"].as_str().unwrap_or_default().to_string(),
                    call_site_line: call["call_site_line"].as_u64().unwrap_or_default(),
                })
                .collect();
            (key.clone(), edges)
        })
        .collect()
}

#[test]
fn a_typed_receiver_records_an_edge_into_the_sibling_package() {
    let edges = scan_edges();
    assert_eq!(
        edges.get("readRun").map(Vec::as_slice),
        Some(
            [Edge {
                callee: "RunClient.subscribeToRun".to_string(),
                callee_file: "packages/core/src/v2/client/index.ts".to_string(),
                call_site_line: 10,
            }]
            .as_slice()
        ),
        "all edges were {edges:?}"
    );
}

#[test]
fn a_constructed_receiver_records_an_edge_into_the_sibling_package() {
    let edges = scan_edges();
    assert_eq!(
        edges.get("readStream").map(Vec::as_slice),
        Some(
            [Edge {
                callee: "RunClient.fetchStream".to_string(),
                callee_file: "packages/core/src/v2/client/index.ts".to_string(),
                call_site_line: 16,
            }]
            .as_slice()
        ),
        "all edges were {edges:?}"
    );
}

/// `this.field.member()`, with the field declared by a constructor parameter
/// property (carrick#782). Dropped at COLLECTION before the fix — a two-level
/// chain never became a `CalleeRef` — so this assertion fails on the pre-fix
/// scanner however the receiver is declared.
#[test]
fn a_this_field_receiver_records_an_edge_into_the_sibling_package() {
    let edges = scan_edges();
    assert_eq!(
        edges
            .get("RunMetadataManager.readStreamThroughField")
            .map(Vec::as_slice),
        Some(
            [Edge {
                callee: "RunClient.fetchStream".to_string(),
                callee_file: "packages/core/src/v2/client/index.ts".to_string(),
                call_site_line: 45,
            }]
            .as_slice()
        ),
        "all edges were {edges:?}"
    );
}

/// A receiver whose ORIGIN is a workspace package, whose class the file never
/// names (carrick#781). One class in the package's published surface declares
/// the member, so the join answers; before the fix `resolve_member` had
/// nothing left to try and returned nothing.
#[test]
fn an_origin_receiver_records_an_edge_when_one_class_declares_the_member() {
    let edges = scan_edges();
    // The manager itself is an exported object literal, so
    // `runClientManager.clientOrThrow` is an indexed definition and the call to
    // it is an edge of its own since carrick#830. What this test is about is
    // the member called on the receiver that call returns.
    assert_eq!(
        edges.get("readRunByOrigin").map(Vec::as_slice),
        Some(
            [
                Edge {
                    callee: "RunClient.subscribeToRun".to_string(),
                    callee_file: "packages/core/src/v2/client/index.ts".to_string(),
                    call_site_line: 65,
                },
                Edge {
                    callee: "runClientManager.clientOrThrow".to_string(),
                    callee_file: "packages/core/src/v2/manager/index.ts".to_string(),
                    call_site_line: 64,
                }
            ]
            .as_slice()
        ),
        "all edges were {edges:?}"
    );
}

#[test]
fn a_receiver_the_file_does_not_declare_records_nothing() {
    let edges = scan_edges();
    for caller in [
        "readUnbound",
        "readVendor",
        "readAmbiguous",
        "inner",
        // The field is initialised with `new RunClient()` but never
        // annotated, so the class body declares nothing about it.
        "UntypedManager.readStreamUntyped",
        "nested",
    ] {
        assert_eq!(
            edges.get(caller).map(Vec::as_slice),
            Some([].as_slice()),
            "{caller} must record no edge; all edges were {edges:?}"
        );
    }

    // Two callers whose RECEIVER answers nothing, and whose only edge is the
    // manager member that produced it — an exported object literal's member,
    // indexed since carrick#830. Asserted exactly, so the receiver's silence is
    // still what is being read.
    for caller in [
        // The origin is a workspace package, but two classes on its published
        // surface declare `fetchStream`, so the join is ambiguous and drops
        // rather than picking one (carrick#781).
        ("readStreamByOrigin", 70),
        // A nested parameter shadows the origin, so neither the enclosing
        // function nor the arrow itself may answer for it.
        ("readContestedByOrigin", 79),
    ] {
        let (name, line) = caller;
        assert_eq!(
            edges.get(name).map(Vec::as_slice),
            Some(
                [Edge {
                    callee: "runClientManager.clientOrThrow".to_string(),
                    callee_file: "packages/core/src/v2/manager/index.ts".to_string(),
                    call_site_line: line,
                }]
                .as_slice()
            ),
            "{name} must record nothing for its receiver; all edges were {edges:?}"
        );
    }
}
