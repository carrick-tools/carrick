//! Functions served one per folder (`supabase/functions/<name>/index.ts`)
//! reach the index as one fact row each, `ALL /functions/v1/<name>`, stated
//! by the routing convention the platform's config file selects. The model's
//! guess for the same file folds onto that row and is never a second one. An
//! entry file whose handler is not located (an imported handler, no serving
//! call) still states its row, with no handler, and is saved as an unread
//! site.
//!
//! Drives the real scanner binary on `.`, offline, over a committed copy of
//! `tests/fixtures/hosted-functions` (its answer key is the README there),
//! once with its `carrick.json` (the folder is its own service) and once
//! without (one service at the root).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "carrick-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "carrick-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// Every blob the scan of the fixture writes, with `carrick.json` kept or
/// removed.
fn scan_blobs(with_config: bool) -> Vec<serde_json::Value> {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("workspace");
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hosted-functions"),
        &repo,
    );
    if !with_config {
        std::fs::remove_file(repo.join("carrick.json")).unwrap();
    }
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let (storage, home, cache) = (
        tmp.path().join("storage"),
        tmp.path().join("home"),
        tmp.path().join("cache"),
    );
    for dir in [&storage, &home, &cache] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(&repo)
        .env("HOME", &home)
        .env("CARRICK_LOCAL_STORAGE_DIR", &storage)
        .env("CARRICK_CACHE_DIR", &cache)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_NO_UPDATE_CHECK", "1")
        .env(
            "CARRICK_SIDECAR_DIR",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar"),
        );
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_EVENT_PATH",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        "CARRICK_OUTPUT_JSON",
        "CARRICK_NO_MODEL",
        "CARRICK_ALLOW_MISSING_TYPES",
        "CARRICK_LOCAL_STORAGE_ISOLATE",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("spawn carrick");
    assert!(
        output.status.success(),
        "the scan failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read_dir(&storage)
        .expect("storage dir")
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| {
            serde_json::from_slice(&std::fs::read(path).expect("read blob")).expect("parse blob")
        })
        .collect()
}

/// (`file:line`, method, path, resolution source) of every endpoint row in `blobs` under
/// `supabase/functions`.
fn function_rows(blobs: &[serde_json::Value]) -> BTreeSet<(String, String, String, String)> {
    blobs
        .iter()
        .flat_map(|blob| blob["endpoints"].as_array().into_iter().flatten())
        .filter_map(|row| {
            let file = row["file_path"].as_str()?;
            let at = file.find("supabase/functions/")?;
            Some((
                file[at..].to_string(),
                row["key"]["method"].as_str()?.to_string(),
                row["key"]["path"].as_str()?.to_string(),
                row["resolution_source"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            ))
        })
        .collect()
}

/// (file, slot, reason) of every unread PRODUCER site under
/// `supabase/functions`. The calls inside the functions (a `serve` imported
/// by URL, an untyped `req.json`) are unread sites of their own, as they are
/// without the convention, and are not what this test is about.
fn function_unread(blobs: &[serde_json::Value]) -> BTreeSet<(String, String, String)> {
    blobs
        .iter()
        .flat_map(|blob| blob["unread_sites"].as_array().into_iter().flatten())
        .filter(|site| site["role"] == "producer")
        .filter_map(|site| {
            let file = site["file"].as_str()?;
            let at = file.find("supabase/functions/")?;
            Some((
                file[at..].to_string(),
                site["slot"].as_str()?.to_string(),
                site["reason"].as_str()?.to_string(),
            ))
        })
        .collect()
}

#[test]
fn each_folder_function_is_one_fact_row_at_its_real_address() {
    // The row's place is `file:line`, the line its handler is declared on.
    let expected: BTreeSet<(String, String, String, String)> = [
        ("price-quote", 8, "/functions/v1/price-quote"),
        ("team-digest", 1, "/functions/v1/team-digest"),
        ("purge-sessions", 3, "/functions/v1/purge-sessions"),
        // Handler not located: the route is still the folder's.
        ("send-invite", 4, "/functions/v1/send-invite"),
        ("report-export", 1, "/functions/v1/report-export"),
    ]
    .into_iter()
    .map(|(folder, line, path)| {
        (
            format!("supabase/functions/{folder}/index.ts:{line}"),
            "ALL".to_string(),
            path.to_string(),
            "file_based_route".to_string(),
        )
    })
    .collect();
    let unread_expected: BTreeSet<(String, String, String)> = ["send-invite", "report-export"]
        .into_iter()
        .map(|folder| {
            (
                format!("supabase/functions/{folder}/index.ts"),
                "handler".to_string(),
                "handler_not_located".to_string(),
            )
        })
        .collect();
    for with_config in [true, false] {
        let blobs = scan_blobs(with_config);
        assert_eq!(
            function_rows(&blobs),
            expected,
            "carrick.json kept: {with_config}"
        );
        assert_eq!(
            function_unread(&blobs),
            unread_expected,
            "carrick.json kept: {with_config}"
        );
    }
}
