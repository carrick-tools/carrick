//! The compiler engine's build stamp (carrick#4133).
//!
//! The Node writer and the Rust checker must hash the same inputs to the same
//! value, a stale engine in a source checkout is refused, and an engine that
//! reached the scanner by the customer paths (`CARRICK_SIDECAR_DIR`, next to
//! the binary) is never refused. Needs `node` on PATH.

use carrick::engine_stamp::{self, EngineOrigin, StampVerdict};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// An engine folder as a checkout holds it: inputs, a test the stamp ignores,
/// and a build output with no stamp yet.
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    for (rel, body) in [
        ("package.json", "{\"name\":\"engine\"}\n"),
        ("package-lock.json", "{\"lockfileVersion\":3}\n"),
        ("tsconfig.json", "{}\n"),
        ("tsconfig.build.json", "{\"extends\":\"./tsconfig.json\"}\n"),
        ("src/index.ts", "export const a = 1;\n"),
        ("src/nested/deep.ts", "export const b = 2;\n"),
        ("src/zeta.ts", "export const c = 3;\n"),
        ("test/index.test.ts", "// ignored by the stamp\n"),
        ("dist/src/index.js", "// built\n"),
    ] {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    (dir, root)
}

fn entry(root: &Path) -> PathBuf {
    root.join("dist/src/index.js")
}

/// Run the build's stamp writer on `root`, as `npm run build` does.
fn write_stamp(root: &Path) {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/scripts/write-engine-stamp.mjs");
    let status = Command::new("node")
        .arg(script)
        .arg(root)
        .status()
        .expect("node must be on PATH");
    assert!(status.success(), "the stamp writer failed");
}

#[test]
fn node_writer_and_rust_checker_hash_the_same_inputs() {
    let (_dir, root) = fixture();
    write_stamp(&root);
    let written = engine_stamp::read_stamp(&entry(&root)).expect("the writer wrote a stamp");
    assert_eq!(written, engine_stamp::hash_inputs(&root).unwrap());
    assert_eq!(written.len(), 64);
}

#[test]
fn a_test_edit_keeps_the_stamp_and_a_source_edit_changes_it() {
    let (_dir, root) = fixture();
    let before = engine_stamp::hash_inputs(&root).unwrap();
    fs::write(root.join("test/index.test.ts"), "// edited\n").unwrap();
    assert_eq!(before, engine_stamp::hash_inputs(&root).unwrap());
    fs::write(root.join("src/zeta.ts"), "export const c = 4;\n").unwrap();
    assert_ne!(before, engine_stamp::hash_inputs(&root).unwrap());
}

#[test]
fn a_source_checkout_accepts_a_current_build_even_after_a_test_edit() {
    let (_dir, root) = fixture();
    write_stamp(&root);
    fs::write(root.join("test/index.test.ts"), "// edited\n").unwrap();
    assert!(matches!(
        engine_stamp::check(&entry(&root), EngineOrigin::SourceCheckout),
        StampVerdict::Ok { .. }
    ));
}

#[test]
fn a_source_checkout_refuses_a_build_older_than_its_sources() {
    let (_dir, root) = fixture();
    write_stamp(&root);
    fs::write(root.join("src/zeta.ts"), "export const c = 4;\n").unwrap();
    match engine_stamp::check(&entry(&root), EngineOrigin::SourceCheckout) {
        StampVerdict::Refuse(message) => {
            assert!(message.contains("npm run build"), "{message}");
            assert!(message.contains("CARRICK_SIDECAR_DIR"), "{message}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_source_checkout_refuses_a_build_with_no_stamp() {
    let (_dir, root) = fixture();
    match engine_stamp::check(&entry(&root), EngineOrigin::SourceCheckout) {
        StampVerdict::Refuse(message) => {
            assert!(message.contains("no build stamp"), "{message}");
            assert!(message.contains("npm run build"), "{message}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_source_checkout_without_sources_beside_the_build_is_not_refused() {
    let (_dir, root) = fixture();
    write_stamp(&root);
    fs::remove_dir_all(root.join("src")).unwrap();
    assert!(matches!(
        engine_stamp::check(&entry(&root), EngineOrigin::SourceCheckout),
        StampVerdict::Ok { .. }
    ));
}

#[test]
fn the_customer_paths_never_refuse() {
    for origin in [EngineOrigin::EnvVar, EngineOrigin::NextToBinary] {
        // Sources that no longer match the stamp: still fine.
        let (_dir, root) = fixture();
        write_stamp(&root);
        fs::write(root.join("src/zeta.ts"), "export const c = 4;\n").unwrap();
        assert!(
            matches!(
                engine_stamp::check(&entry(&root), origin),
                StampVerdict::Ok { .. }
            ),
            "{origin:?} refused a stale stamp"
        );

        // No stamp at all: a warning, not a refusal.
        let (_dir, root) = fixture();
        match engine_stamp::check(&entry(&root), origin) {
            StampVerdict::Warn(message) => assert!(message.contains("no build stamp"), "{message}"),
            other => panic!("{origin:?}: expected a warning, got {other:?}"),
        }

        // No sources next to the build (an install): fine.
        let (_dir, root) = fixture();
        write_stamp(&root);
        fs::remove_dir_all(root.join("src")).unwrap();
        assert!(matches!(
            engine_stamp::check(&entry(&root), origin),
            StampVerdict::Ok { .. }
        ));
    }
}

/// The build stamp is a file the build writes into dist/src, which both
/// packagings copy whole.
#[test]
fn the_build_script_runs_the_stamp_writer() {
    let package =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/package.json"))
            .unwrap();
    assert!(package.contains("scripts/write-engine-stamp.mjs"));
}
