//! Producer rows stated by a type: the tables, views and functions a hosted
//! database's generated schema type declares reach the index as one `ALL` row
//! each, `/rest/v1/<key>` and `/rest/v1/rpc/<key>`, source `type_resource`,
//! at the key's own line, with `resource {schema, name, kind}` and no type
//! entries. The type is type argument 0 of the served package's make call;
//! the layout is selected by the platform's config file.
//!
//! Drives the real scanner binary on `.`, offline, over a committed copy of
//! `tests/fixtures/hosted-tables-producer` (its answer key is the README
//! there), with `library-claims.json` replayed as the store's answer.

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

/// The fixture as one scan reads it.
#[derive(Clone, Copy)]
struct Tree<'a> {
    /// `supabase/config.toml` kept.
    marker: bool,
    /// Files written into the copy before the commit.
    extra: &'a [(&'a str, &'a str)],
}

/// What one scan wrote: its blobs and its run log.
struct Scanned {
    blobs: Vec<serde_json::Value>,
    log: String,
}

/// One scan of `tree`.
fn scan(tree: Tree<'_>) -> Scanned {
    scans_of(tree, 1).remove(0)
}

/// [`scan`], returned as a one-element list.
fn scans(tree: Tree<'_>) -> Vec<Scanned> {
    scans_of(tree, 1)
}

/// `runs` scans of one committed copy of `tree`, sharing one store and
/// cache: every scan after the first takes the incremental path.
fn scans_of(tree: Tree<'_>, runs: usize) -> Vec<Scanned> {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("workspace");
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hosted-tables-producer"),
        &repo,
    );
    if !tree.marker {
        std::fs::remove_file(repo.join("supabase/config.toml")).unwrap();
    }
    std::fs::create_dir_all(repo.join("__llm__/library-claims")).unwrap();
    std::fs::copy(
        repo.join("library-claims.json"),
        repo.join("__llm__/library-claims/default.json"),
    )
    .unwrap();
    for (file, text) in tree.extra {
        let path = repo.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
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
    let logs = home.join(".carrick/logs/runs");
    let mut out = Vec::with_capacity(runs);
    for run in 0..runs {
        // The first scan finds no earlier blob; a later one reads the store
        // back and takes the incremental path.
        if run == 0 {
            cmd.env("CARRICK_LOCAL_STORAGE_ISOLATE", "1");
        } else {
            cmd.env_remove("CARRICK_LOCAL_STORAGE_ISOLATE");
        }
        let before: BTreeSet<PathBuf> = run_logs(&logs);
        let output = cmd.output().expect("spawn carrick");
        assert!(
            output.status.success(),
            "the scan failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let log = run_logs(&logs)
            .difference(&before)
            .map(|path| std::fs::read_to_string(path).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        let blobs = std::fs::read_dir(&storage)
            .expect("storage dir")
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .map(|path| {
                serde_json::from_slice(&std::fs::read(path).expect("read blob"))
                    .expect("parse blob")
            })
            .collect();
        out.push(Scanned { blobs, log });
    }
    out
}

/// The run logs under `dir`.
fn run_logs(dir: &Path) -> BTreeSet<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
        .unwrap_or_default()
}

/// (`file:line`, method, path, `resource` as `schema.name:kind`) of every
/// `type_resource` row in `blobs`.
fn type_rows(blobs: &[serde_json::Value]) -> BTreeSet<(String, String, String, String)> {
    blobs
        .iter()
        .flat_map(|blob| blob["endpoints"].as_array().into_iter().flatten())
        .filter(|row| row["resolution_source"] == "type_resource")
        .map(|row| {
            let resource = &row["resource"];
            (
                row["file_path"].as_str().unwrap().to_string(),
                row["key"]["method"].as_str().unwrap().to_string(),
                row["key"]["path"].as_str().unwrap().to_string(),
                format!(
                    "{}.{}:{}",
                    resource["schema"].as_str().unwrap_or_default(),
                    resource["name"].as_str().unwrap_or_default(),
                    resource["kind"].as_str().unwrap_or_default()
                ),
            )
        })
        .collect()
}

const MARKED: Tree<'static> = Tree {
    marker: true,
    extra: &[],
};

fn expected() -> BTreeSet<(String, String, String, String)> {
    [
        (4, "/rest/v1/accounts", "public.accounts:table"),
        (5, "/rest/v1/notes", "public.notes:table"),
        (6, "/rest/v1/unused_table", "public.unused_table:table"),
        (10, "/rest/v1/rpc/bump_count", "public.bump_count:function"),
    ]
    .into_iter()
    .map(|(line, path, resource)| {
        (
            format!("src/types.ts:{line}"),
            "ALL".to_string(),
            path.to_string(),
            resource.to_string(),
        )
    })
    .collect()
}

#[test]
fn each_key_of_the_schema_type_is_one_row_with_its_resource() {
    // Two makes of the same type state each key once; the make with no type
    // argument states none.
    let scanned = scan(MARKED);
    assert_eq!(type_rows(&scanned.blobs), expected());
}

#[test]
fn the_rows_carry_no_type_entries() {
    let blobs = scan(MARKED).blobs;
    for blob in &blobs {
        let manifest = blob["type_manifest"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            manifest
                .iter()
                .all(|entry| !entry.to_string().contains("/rest/v1")),
            "a manifest entry for a type-resource row: {manifest:?}"
        );
        assert!(
            !blob["bundled_types"]
                .as_str()
                .unwrap_or_default()
                .contains("rest_v1"),
            "a bundled type for a type-resource row"
        );
        for row in blob["endpoints"].as_array().into_iter().flatten() {
            if row["resolution_source"] == "type_resource" {
                assert!(row["request_type"].is_null() && row["response_type"].is_null());
            }
        }
    }
}

#[test]
fn without_the_marker_no_file_states_a_row() {
    let scanned = scan(Tree {
        marker: false,
        ..MARKED
    });
    assert!(type_rows(&scanned.blobs).is_empty());
}

#[test]
fn a_second_scan_states_the_same_rows() {
    let runs = scans_of(MARKED, 2);
    assert!(
        runs[1].log.contains("Incremental analysis complete"),
        "the second scan took the incremental path"
    );
    for (run, scanned) in runs.iter().enumerate() {
        assert_eq!(type_rows(&scanned.blobs), expected(), "scan {run}");
    }
}
