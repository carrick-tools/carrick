//! carrick#2488: a file Carrick cannot analyse, even split into parts, is a
//! final loss. Its service is complete, not pending: the run says what it
//! lost, writes its index with the loss recorded on it, and exits 0.
//!
//! Drives the built binary the way the GitHub Action does: from inside the
//! checkout, with the path `.`, a service at the repo root, the model mocked
//! and the index written to a local directory. The one oversized file is
//! generated here, not committed: a single function over the whole-prompt
//! line whose one item holds a call, so it can be neither sent whole nor cut
//! (`FileOrchestrator::asking`, carrick#2309).
//!
//! The upload's wire shape (the loss named as `final_losses`, never in the
//! owed list the cloud's partial rule counts) is pinned beside the storage
//! client in `src/cloud_storage/aws_storage.rs`, against the contract
//! fixture both repos read.

use carrick::agents::file_orchestrator::{FILE_TOO_LARGE_CODE, WHOLE_PROMPT_MAX_BYTES};
use std::path::Path;
use std::process::Command;

#[path = "support/real_packages.rs"]
mod real_packages;

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?}");
}

fn write(repo: &Path, file: &str, body: &str) {
    let path = repo.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// A one-service repo with a small route file and one file that is a single
/// function over the line, holding one call.
fn repo_with_one_oversized_file(root: &Path) -> std::path::PathBuf {
    let repo = root.join("routes-repo");
    std::fs::create_dir_all(&repo).unwrap();
    write(
        &repo,
        "carrick.json",
        r#"{ "services": [ { "name": "routes", "directory": "." } ] }"#,
    );
    write(
        &repo,
        "package.json",
        r#"{ "name": "routes", "private": true, "dependencies": { "express": "^4.18.0" } }"#,
    );
    write(
        &repo,
        "src/app.ts",
        "import express from \"express\";\n\
         const app = express();\n\
         app.get(\"/health\", (_req, res) => {\n  res.json({ ok: true });\n});\n\
         export default app;\n",
    );
    let filler = "x".repeat(WHOLE_PROMPT_MAX_BYTES + 10_000);
    write(
        &repo,
        "src/huge.ts",
        &format!(
            "declare const client: any;\n\
             export function sendEverything() {{\n  \
               return client.post({{ url: \"/everything/items\", body: \"{filler}\" }});\n\
             }}\n"
        ),
    );
    real_packages::install(&repo, &["express"]);
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

struct Run {
    succeeded: bool,
    output: String,
    blob: Option<serde_json::Value>,
}

/// The binary over `repo` as the Action runs it, offline.
fn scan(repo: &Path, scratch: &Path) -> Run {
    let store = scratch.join("store");
    let cache = scratch.join("cache");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    // The store's answer for the installed package; every other answer is the
    // generated mock.
    let cassettes = scratch.join("cassettes");
    real_packages::write_claims(&cassettes, &["express"]);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(repo)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_CACHE_DIR", &cache)
        .env("CARRICK_LOCAL_STORAGE_DIR", &store)
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1");
    // A runner's own values would name this repo and an OIDC token the
    // fixture cannot mint.
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_EVENT_NAME",
        "GITHUB_EVENT_PATH",
        "GITHUB_REF",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "CI",
        "CARRICK_OUTPUT_JSON",
        "CARRICK_NO_MODEL",
        "CARRICK_ALLOW_PARTIAL_ANALYSIS",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let result = cmd.output().expect("failed to spawn carrick");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let blob = std::fs::read_dir(&store)
        .expect("storage dir")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(path).ok()?).ok()
        })
        .find(|blob| blob.get("boundary").is_some());
    Run {
        succeeded: result.status.success(),
        output,
        blob,
    }
}

/// The run lost one file for good and nothing else. It exits 0, calls its
/// one service complete and names the file as one Carrick cannot analyse,
/// says nothing is pending, and the index it writes records the loss by the
/// file's repo-relative path and its code.
#[test]
fn a_file_too_large_to_analyse_is_lost_for_good_and_the_scan_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo_with_one_oversized_file(tmp.path());
    let run = scan(&repo, &tmp.path().join("action"));
    let output = &run.output;

    assert!(run.succeeded, "the scan exits 0:\n{output}");
    assert!(
        output.contains(
            "src/huge.ts: the file is too large for Carrick to analyse, even split into parts"
        ),
        "the file is named with why:\n{output}"
    );
    assert!(
        output.contains("Complete: routes (1 file(s) Carrick cannot analyse)"),
        "its service is complete, with the loss beside its name:\n{output}"
    );
    assert!(
        !output.contains("pending model analysis"),
        "nothing a re-run can mend is owed, so nothing is pending:\n{output}"
    );

    let blob = run
        .blob
        .unwrap_or_else(|| panic!("no index was written:\n{output}"));
    assert_eq!(
        blob["boundary"]["final_losses"],
        serde_json::json!({
            "total": 1,
            "reasons": [format!("src/huge.ts: {FILE_TOO_LARGE_CODE}")]
        }),
        "the index records the loss on the service:\n{:#}",
        blob["boundary"]
    );
    assert_eq!(blob["boundary"]["files_lost"]["total"], 1);
}

/// A source file the parser refuses (a closing brace with no
/// opener) is counted in `boundary.files_not_parsed` with its path and
/// `does_not_parse`, and in neither `files_lost` nor `final_losses`.
#[test]
fn a_file_that_does_not_parse_is_counted_on_the_blob_and_not_as_a_loss() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("parse-repo");
    std::fs::create_dir_all(&repo).unwrap();
    write(
        &repo,
        "carrick.json",
        r#"{ "services": [ { "name": "routes", "directory": "." } ] }"#,
    );
    write(
        &repo,
        "package.json",
        r#"{ "name": "routes", "private": true, "dependencies": { "express": "^4.18.0" } }"#,
    );
    write(
        &repo,
        "src/app.ts",
        "import express from \"express\";\nconst app = express();\n\
         app.get(\"/health\", (_req, res) => {\n  res.json({ ok: true });\n});\n\
         export default app;\n",
    );
    write(
        &repo,
        "types/broken.d.ts",
        "declare namespace Shapes {\n  interface A { x: number }\n}\n  interface B { y: string }\n}\n",
    );
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    let run = scan(&repo, &tmp.path().join("action"));
    let blob = run
        .blob
        .unwrap_or_else(|| panic!("no index was written:\n{}", run.output));
    assert_eq!(
        blob["boundary"]["files_not_parsed"],
        serde_json::json!({"total": 1, "reasons": ["types/broken.d.ts: does_not_parse"]}),
        "{:#}",
        blob["boundary"]
    );
    assert_eq!(blob["boundary"]["files_lost"]["total"], 0);
    assert_eq!(blob["boundary"]["final_losses"]["total"], 0);
}
