//! A scan of an unprepared checkout is refused before it starts (carrick#1254).
//!
//! The unit tests in `src/preflight.rs` cover what counts as unprepared. These
//! run the binary, which is the only place the two things that matter are
//! true at once: the refusal happens before the sidecar, the model and the
//! upload, and it is on the way out of the process rather than in a log.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

/// A repo with one service, one route, and whatever config the case needs.
fn repo(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("temp dir");
    write(
        dir.path(),
        "carrick.json",
        r#"{"serviceName":"api","internalEnvVars":[],"externalEnvVars":[]}"#,
    );
    write(
        dir.path(),
        "src/app.ts",
        "import express from 'express';\nconst app = express();\napp.get('/health', (_req, res) => res.json({ ok: true }));\n",
    );
    for (path, contents) in files {
        write(dir.path(), path, contents);
    }
    dir
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    fs::write(path, contents).expect("write");
}

fn scan(root: &Path, allow: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_carrick"));
    command
        .arg(root.to_str().expect("utf-8 path"))
        .env("CARRICK_MOCK_ALL", "1");
    if allow {
        command.arg(carrick::preflight::ALLOW_FLAG);
    }
    command.output().expect("carrick ran")
}

fn said(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Dependencies the lockfile states and the tree has not installed. The
/// refusal names the service and the command, because the lockfile says which
/// package manager locked them.
#[test]
fn a_scan_is_refused_when_the_dependencies_are_not_installed() {
    let repo = repo(&[
        (
            "package.json",
            r#"{"name":"api","dependencies":{"express":"4"}}"#,
        ),
        ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
    ]);
    let output = scan(repo.path(), false);
    let said = said(&output);
    assert!(
        !output.status.success(),
        "the scan proceeded on an unprepared tree: {said}"
    );
    assert!(
        said.contains("`api`: dependencies are not installed"),
        "the refusal names the service: {said}"
    );
    assert!(
        said.contains("Run `pnpm install`"),
        "and the command the lockfile names: {said}"
    );
}

/// A config mapping whose target directory is not on the checkout — the
/// generated client nobody generated — that the service imports through. No
/// command is invented for it.
///
/// Beside it, a second mapping that is equally missing and that nothing
/// imports: the stale entry a deleted package leaves behind. It is not a
/// reason to refuse and it must not crowd out the one that is
/// (carrick#1301).
#[test]
fn a_scan_is_refused_when_a_mapping_names_a_directory_that_is_not_there() {
    let repo = repo(&[
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"paths":{
                 "@db/*":["./src/generated/db/*"],
                 "@parser/*":["./packages/parser/src/*"]
               }}}"#,
        ),
        (
            "src/db.ts",
            "import { client } from '@db/client';\nexport const db = client;\n",
        ),
    ]);
    let output = scan(repo.path(), false);
    let said = said(&output);
    assert!(
        !output.status.success(),
        "the scan proceeded with a mapping pointing at nothing: {said}"
    );
    assert!(
        said.contains("maps `@db/*` to src/generated/db"),
        "the refusal names the mapping and the missing directory: {said}"
    );
    assert!(
        !said.contains("maps `@parser/*` to packages/parser/src, and that directory"),
        "a mapping nothing imports through cannot make a type `any`, so it is not refused: {said}"
    );
    assert!(
        said.contains("nothing this service imports resolves through it"),
        "it is said once, as the line it is — not as a reason to stop: {said}"
    );
    assert!(
        !said.contains("Run `"),
        "nothing in the config says what fills it, so no command is guessed: {said}"
    );
}

/// The way a CI job that deliberately checks out without installing keeps
/// scanning: one named flag, and the same tree goes through.
#[test]
fn the_override_scans_the_same_tree() {
    let repo = repo(&[
        (
            "package.json",
            r#"{"name":"api","dependencies":{"express":"4"}}"#,
        ),
        ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"paths":{"@db/*":["./src/generated/db/*"]}}}"#,
        ),
    ]);
    let refused = said(&scan(repo.path(), false));
    assert!(
        refused.contains("this checkout is not prepared"),
        "the same tree without the flag is the refusal this is the way past: {refused}"
    );

    let output = scan(repo.path(), true);
    let said = said(&output);
    assert!(
        !said.contains("this checkout is not prepared"),
        "the flag is the way past the refusal: {said}"
    );
    assert!(
        output.status.success(),
        "and the scan it allows is an ordinary one: {said}"
    );
}
