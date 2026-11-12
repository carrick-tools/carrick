//! A Deno app that a pnpm workspace glob holds, with its own `deno.json` and
//! no `package.json`, is scanned as a service and its routes are stated
//! (carrick#3969). Before the fix the app was in no derived service and its
//! route was a silent miss. The fixture is invented.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deno-under-workspace-glob")
}

/// A `deno` on PATH that answers only `--version`, so the scan's runtime
/// check passes without a Deno install on the test machine.
#[cfg(unix)]
fn fake_deno(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let deno = bin.join("deno");
    std::fs::write(&deno, "#!/bin/sh\necho 'deno 2.9.4 (stable)'\n").unwrap();
    std::fs::set_permissions(&deno, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

#[cfg(unix)]
#[test]
fn a_deno_app_under_a_workspace_glob_states_its_route() {
    let tools = tempfile::tempdir().unwrap();
    let bin = fake_deno(tools.path());
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let root = fixture();
    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(&root)
        .env("PATH", path)
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
    let projection: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let endpoints = projection["endpoints"].as_array().unwrap();
    let stated: Vec<_> = endpoints
        .iter()
        .filter(|row| {
            row["file"] == "apps/edge/hello/src/app.ts"
                && row["line"] == 3
                && row["method"] == "GET"
                && row["path"] == "/healthz"
        })
        .collect();
    assert_eq!(
        stated.len(),
        1,
        "the Deno app's route is stated once: {endpoints:#?}\n{stderr}"
    );
}
