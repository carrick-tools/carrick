//! A function that returns a name given only a value that names itself must
//! not abort the scan (carrick-cloud#3983: a stack overflow, exit 134).
//!
//! The folder is a small repo of browser scripts; one holds the function. The
//! overflow is a process abort, so only a run of the binary can prove the
//! exit: a unit test aborts its own binary instead of failing.

use std::process::Command;

#[test]
fn a_script_with_a_self_named_return_scans_to_exit_zero() {
    let repo = tempfile::tempdir().expect("temp repo dir");
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let home = tempfile::tempdir().expect("temp home dir");
    let scripts = repo.path().join("js");
    std::fs::create_dir_all(&scripts).unwrap();
    std::fs::write(
        scripts.join("old-library.js"),
        "function f(){ var x = x || null; return x; }\n\
         function g(){ var y = y ? y : null; return y; }\n",
    )
    .unwrap();
    std::fs::write(scripts.join("site.js"), "function ready() { return 1; }\n").unwrap();
    std::fs::write(
        repo.path().join("index.php"),
        "<?php echo '<script src=\"js/site.js\"></script>'; ?>\n",
    )
    .unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(repo.path())
        .env("HOME", home.path())
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_SKIP_INTENTS", "1");
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
        "CARRICK_NO_MODEL",
        "CARRICK_ALLOW_MISSING_TYPES",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("failed to spawn carrick");
    assert!(
        output.status.success(),
        "scan exited {:?}:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}
