//! The re-check's answer holds one item per operation place (carrick#4687).
//! ($0, no model, no cloud.)
//!
//! `check <file> --recheck --json` over a file that serves a GraphQL
//! operation listed the item twice, byte for byte, while an HTTP route in the
//! same answer was listed once. The workspace is the cross-repo corpus's
//! gateway (a schema-first GraphQL service that also serves HTTP routes) with
//! a web service that reads it.

use serde_json::Value;
use serial_test::serial;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn carrick() -> &'static str {
    env!("CARGO_BIN_EXE_carrick")
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn sidecar_built() -> bool {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/sidecar/dist/src/index.js")
        .exists()
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create the destination");
    for entry in std::fs::read_dir(from).unwrap_or_else(|e| panic!("read {}: {e}", from.display()))
    {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy a fixture file");
        }
    }
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path().join("mono");
    let corpus = fixtures().join("xrepo-corpus-1");
    copy_tree(
        &corpus.join("orders-monorepo/packages/gateway"),
        &repo.join("apps/gateway"),
    );
    copy_tree(&corpus.join("web-frontend"), &repo.join("apps/web"));
    for service in ["gateway", "web"] {
        for stale in ["carrick.json", "expected.json"] {
            let _ = std::fs::remove_file(repo.join("apps").join(service).join(stale));
        }
    }
    std::fs::write(
        repo.join("carrick.json"),
        "{ \"services\": [ { \"name\": \"gateway\", \"directory\": \"apps/gateway\" }, \
         { \"name\": \"web\", \"directory\": \"apps/web\" } ] }\n",
    )
    .expect("write carrick.json");
    git(&repo, &["init", "-q", "."]);
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=fixture@carrick.test",
            "-c",
            "user.name=fixture",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    std::fs::write(
        dir.path().join("carrick-workspace.json"),
        "{ \"repos\": [\"./mono\"] }\n",
    )
    .expect("write the workspace file");
    dir
}

/// The decorated-resolver fixture as a workspace of one repo, with its
/// stored model answers replayed: its backend serves GraphQL fields at the
/// resolver file, not at a schema file.
fn resolver_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path().join("mono");
    copy_tree(&fixtures().join("graphql-decorated-resolvers"), &repo);
    git(&repo, &["init", "-q", "."]);
    git(&repo, &["add", "-A", "-f"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=fixture@carrick.test",
            "-c",
            "user.name=fixture",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    std::fs::write(
        dir.path().join("carrick-workspace.json"),
        "{ \"repos\": [\"./mono\"] }\n",
    )
    .expect("write the workspace file");
    dir
}

fn command(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(carrick());
    command
        .args(args)
        .arg("--workspace")
        .arg(root)
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env_remove("CARRICK_CHECK_RESCAN")
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", root.join("mono/__llm__").display()),
        )
        .env("CARRICK_CHECK_BUDGET_MS", "600000")
        .env("CARRICK_RECHECK_BUDGET_MS", "600000");
    command
}

/// A copy of the index's blobs left under another repo name (a repo whose
/// folder was renamed): the same services and operations, joined twice.
fn leave_a_copy_under_another_repo_name(root: &Path) {
    let blobs = root.join(".carrick/repos");
    for entry in std::fs::read_dir(&blobs)
        .expect("the index's blobs")
        .flatten()
    {
        let mut blob: Value =
            serde_json::from_slice(&std::fs::read(entry.path()).expect("read a blob"))
                .expect("a blob is JSON");
        blob["repo_name"] = Value::from("renamed-mono");
        std::fs::write(
            blobs.join(format!("renamed-{}", entry.file_name().to_string_lossy())),
            serde_json::to_vec(&blob).expect("write a blob"),
        )
        .expect("write the copy");
    }
}

/// An edit the scan reads, so the answer is a re-scan's.
fn edit_so_the_scan_reads_it(file: &Path) {
    let mut text = std::fs::read_to_string(file).expect("the file");
    text.push_str("\nexport const edited = 1;\n");
    std::fs::write(file, text).expect("edit the file");
}

/// The items `check <file> --recheck --json` answers for `file`, counted by
/// the place each stands at.
fn recheck_items(root: &Path, file: &str) -> BTreeMap<String, usize> {
    let out = command(root, &["check", file, "--recheck", "--json"])
        .output()
        .expect("spawn check --recheck");
    let json: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("not JSON: {e}\n{}", String::from_utf8_lossy(&out.stdout)));
    let mut found: BTreeMap<String, usize> = BTreeMap::new();
    let mut walk = vec![&json];
    while let Some(value) = walk.pop() {
        match value {
            Value::Array(values) => walk.extend(values),
            Value::Object(map) => {
                if let (Some(kind), Some(path)) = (map.get("kind"), map.get("path")) {
                    let place = format!(
                        "{} {} {} line {} dispatch {}",
                        kind,
                        map.get("method").unwrap_or(&Value::Null),
                        path,
                        map.get("line").unwrap_or(&Value::Null),
                        map.get("dispatch").unwrap_or(&Value::Null),
                    );
                    *found.entry(place).or_default() += 1;
                }
                walk.extend(map.values());
            }
            _ => {}
        }
    }
    found
}

#[test]
#[serial]
fn recheck_lists_a_graphql_operation_once_and_an_http_route_once() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let dir = workspace();
    let root = dir.path();
    let indexed = command(root, &["refresh"]).output().expect("refresh");
    assert!(
        indexed.status.success(),
        "refresh: {}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    leave_a_copy_under_another_repo_name(root);
    for file in [
        "mono/apps/gateway/src/orders.resolver.ts",
        "mono/apps/gateway/src/users.controller.ts",
        "mono/apps/gateway/src/schema.graphql",
    ] {
        edit_so_the_scan_reads_it(&root.join(file));
        let items = recheck_items(root, file);
        eprintln!("{file}: {items:#?}");
        let doubled: Vec<_> = items.iter().filter(|(_, count)| **count > 1).collect();
        assert!(
            doubled.is_empty(),
            "{file}: an item is listed more than once: {doubled:#?}"
        );
    }
}

/// A resolver file: each decorated root field is one item at its method.
#[test]
#[serial]
fn recheck_lists_each_resolver_field_once() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let dir = resolver_workspace();
    let root = dir.path();
    let indexed = command(root, &["refresh"]).output().expect("refresh");
    assert!(
        indexed.status.success(),
        "refresh: {}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    leave_a_copy_under_another_repo_name(root);
    edit_so_the_scan_reads_it(&root.join("mono/services/backend/src/resolvers.ts"));
    let items = recheck_items(root, "mono/services/backend/src/resolvers.ts");
    eprintln!("{items:#?}");
    assert!(
        items.keys().any(|place| place.contains("\"QUERY\" \"me\"")),
        "the resolver's field is an item: {items:#?}"
    );
    let doubled: Vec<_> = items.iter().filter(|(_, count)| **count > 1).collect();
    assert!(doubled.is_empty(), "listed more than once: {doubled:#?}");
}

/// The two copies differ: an edit above the resolvers moves every field one
/// line down in the re-scanned copy, and the copy kept under the other repo
/// name still holds the lines the index had. The answer is the re-scanned
/// copy's, once each, and the kept copy's places do not appear beside them.
#[test]
#[serial]
fn recheck_answers_from_the_rescanned_copy_when_the_copies_differ() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let dir = resolver_workspace();
    let root = dir.path();
    let indexed = command(root, &["refresh"]).output().expect("refresh");
    assert!(
        indexed.status.success(),
        "refresh: {}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    leave_a_copy_under_another_repo_name(root);
    let resolvers = root.join("mono/services/backend/src/resolvers.ts");
    let text = std::fs::read_to_string(&resolvers).expect("the resolver file");
    std::fs::write(&resolvers, format!("export const edited = 1;\n{text}"))
        .expect("edit the resolver file");
    let items = recheck_items(root, "mono/services/backend/src/resolvers.ts");
    eprintln!("{items:#?}");
    let places: Vec<&str> = items.keys().map(String::as_str).collect();
    assert_eq!(
        places,
        [
            "\"route\" \"MUTATION\" \"renameThing\" line 20 dispatch null",
            "\"route\" \"QUERY\" \"byString\" line 25 dispatch null",
            "\"route\" \"QUERY\" \"described\" line 40 dispatch null",
            "\"route\" \"QUERY\" \"everything\" line 35 dispatch null",
            "\"route\" \"QUERY\" \"me\" line 15 dispatch null",
            "\"route\" \"QUERY\" \"plain\" line 50 dispatch null",
            "\"route\" \"QUERY\" \"viaOptions\" line 45 dispatch null",
            "\"route\" \"SUBSCRIPTION\" \"events\" line 30 dispatch null",
        ],
        "the re-scanned copy's places, each once"
    );
    assert!(items.values().all(|count| *count == 1), "{items:#?}");
}
