//! carrick#4070: a Deno app that imports `npm:hono@4` with no `node_modules`
//! states its routes from the library store's answer, not from the model.
//!
//! The test runs a REAL `deno install --frozen` over the committed
//! `deno.lock` into a throwaway `DENO_DIR` (network, as the sidecar's
//! `deno-project.test.ts` does), then scans the fixture with the store's
//! answer replayed from `__llm__/library-claims/default.json`. A fake `deno`
//! cannot run the walk, so this test needs the pinned Deno on PATH (CI:
//! `.github/actions/install-deno`, via `rust-test-setup`) and fails, never
//! skips, when it is missing.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deno-npm-specifier")
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
fn a_deno_npm_specifier_states_its_routes_from_the_store_answer() {
    let work = tempfile::tempdir().unwrap();
    let deno_dir = tempfile::tempdir().unwrap();
    let root = work.path().join("app");
    copy_tree(&fixture(), &root);

    let install = Command::new("deno")
        .args([
            "install",
            "--frozen",
            "--node-modules-dir=none",
            "--entrypoint",
            "src/app.ts",
        ])
        .current_dir(&root)
        .env("DENO_DIR", deno_dir.path())
        .env("DENO_NO_UPDATE_CHECK", "1")
        .output()
        .expect("a real `deno` must be on PATH for this test");
    assert!(
        install.status.success(),
        "deno install failed:\n{}",
        String::from_utf8_lossy(&install.stderr)
    );

    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(&root)
        .arg("--verbose")
        .env("DENO_DIR", deno_dir.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", root.join("__llm__").display()),
        )
        .env("CARRICK_OUTPUT_JSON", "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env_remove("GITHUB_REPOSITORY")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("CI")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "scan failed:\n{stderr}");
    assert!(
        !stderr.contains("Unexpected token"),
        "the Deno walk failed:\n{stderr}"
    );
    let projection: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let endpoints = projection["endpoints"].as_array().unwrap();
    for (method, path) in [("GET", "/healthz"), ("POST", "/items")] {
        let rows: Vec<_> = endpoints
            .iter()
            .filter(|e| e["method"] == method && e["path"] == path)
            .collect();
        assert_eq!(rows.len(), 1, "{method} {path} stated once: {endpoints:#?}");
        assert_eq!(
            rows[0]["resolution_source"], "library_claim",
            "{method} {path} comes from the store's answer: {endpoints:#?}\n{stderr}"
        );
    }
}
