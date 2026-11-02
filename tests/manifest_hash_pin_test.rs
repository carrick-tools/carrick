//! carrick#2314: the manifest hash a scan stores is the key its detection,
//! guidance and extraction config are replayed on. A scan that computes
//! another hash for the same tree asks for all three again, for every
//! service, so the hash for each way of naming the root is pinned here.
//! A `package.json` that states a top-level `version` hashes without it
//! (carrick#2568); the manifests pinned here state none, so they still hash
//! from their bytes.
//!
//! The expected values are what the released 0.3.114 binary wrote for this
//! tree (darwin-arm64 build, sha256
//! `5ad93f8db8b6d017118c030f0a072b7436a5397d3abc3a869de06e1b35ec77c8`), read from the
//! `package_json_hash` of the blobs it stored. Each is
//! `sha256(key NUL manifest bytes NUL)`, and the key is named beside it.
//!
//! A value here changes only with a plan for the blobs already stored under
//! it: every repository indexed with that spelling asks its setup again on
//! the first scan after the change.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

const FILES: &[(&str, &str)] = &[
    (
        "package.json",
        "{ \"name\": \"probe\", \"private\": true }\n",
    ),
    (
        "carrick.json",
        "{ \"services\": [ { \"name\": \"api\", \"directory\": \"apps/api\" }, { \"name\": \"web\", \"directory\": \"apps/web\" } ] }\n",
    ),
    (
        "apps/api/package.json",
        "{ \"name\": \"probe-api\", \"private\": true }\n",
    ),
    (
        "apps/web/package.json",
        "{ \"name\": \"probe-web\", \"private\": true }\n",
    ),
    ("apps/api/src/a.ts", "export const a = 1;\n"),
    ("apps/web/src/b.ts", "export const b = 1;\n"),
];

/// The root named `.`, as the GitHub Action names it. Keys
/// `./apps/api/package.json` and `./apps/web/package.json`.
const AS_DOT: [&str; 2] = [
    "7b5372c59e60dfb68c83ec655232e17031057add848491f61148ae4e60c590cb",
    "49bd64147dbbd931a43ec79f04548137a17a7127b8e1887ee07f760505d2d108",
];

/// The root named `tree` from its parent directory. Keys
/// `tree/apps/api/package.json` and `tree/apps/web/package.json`.
const AS_RELATIVE: [&str; 2] = [
    "8a01df50d738443babe62a85d76b7e13327e771407868fe8631dbbde45fc03f1",
    "64ee4413efca3482a5fe4976b291bc2b7db998d384193bdff44a50602972eeb8",
];

/// The root named by an absolute path with no symlink in it. Keys
/// `apps/api/package.json` and `apps/web/package.json`.
const AS_ABSOLUTE: [&str; 2] = [
    "0a328e9f4bb8d315004c6c0058b8f97d207edb8c6f9de13cf1e10d3eede9ce54",
    "edaf17f4ee2b900bd4d7e2d354473b1770ef1094c82a4ab1ef324d5354a228e1",
];

/// The root named `link`, a symlink to the tree, from its parent directory.
/// Keys `link/apps/api/package.json` and `link/apps/web/package.json`.
#[cfg(unix)]
const AS_RELATIVE_LINK: [&str; 2] = [
    "ed86de68f07003211adf3fa48192ececbc76a76db2ed9e968ef04a144c27c816",
    "c29d9df9caedf6be47d0fd9fd3d628c563aaeb0415ac6b02a8aafce1779f709d",
];

/// Scan the tree from `cwd` with the root named `root`, into a store of its
/// own, and return each service's stored manifest hash as `[api, web]`.
fn stored_hashes(cwd: &Path, root: &str, store: &Path) -> [String; 2] {
    std::fs::create_dir_all(store).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.current_dir(cwd)
        .arg(root)
        .args(["--repo", "fixture/probe"])
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_ALLOW_UNPREPARED", "1")
        .env("CARRICK_NO_UPDATE_CHECK", "1")
        .env("CARRICK_LOCAL_STORAGE_DIR", store);
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_EVENT_NAME",
        "GITHUB_REF",
        "GITHUB_ACTIONS",
        "GITHUB_SHA",
        "CI",
        "CARRICK_OUTPUT_JSON",
        "CARRICK_MOCK_FIXTURE_DIR",
        "CARRICK_LOCAL_STORAGE_ISOLATE",
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
    assert_eq!(result.status.code(), Some(0), "root {root:?}:\n{output}");
    let by_service: BTreeMap<String, String> = std::fs::read_dir(store)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension()? == "json").then_some(path)
        })
        .map(|path| {
            let blob: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            (
                blob["service_name"].as_str().unwrap().to_string(),
                blob["package_json_hash"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        by_service.keys().collect::<Vec<_>>(),
        ["api", "web"],
        "root {root:?}:\n{output}"
    );
    [by_service["api"].clone(), by_service["web"].clone()]
}

/// The tree under a directory that is its own canonical path, so an absolute
/// root has no symlink in it unless the test puts one there (a macOS temp
/// directory sits behind one).
fn tree() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().canonicalize().unwrap();
    for (path, contents) in FILES {
        let file = parent.join("tree").join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, contents).unwrap();
    }
    (tmp, parent)
}

/// carrick#2314: a root named `.`, by a relative path, or by an absolute
/// path stores the hash 0.3.114 stored for that spelling.
#[test]
fn each_spelling_of_the_root_stores_the_hash_the_release_stored() {
    let (_tmp, parent) = tree();
    let root = parent.join("tree");
    assert_eq!(
        stored_hashes(&root, ".", &parent.join("store-dot")),
        AS_DOT,
        "the root named `.`"
    );
    assert_eq!(
        stored_hashes(&parent, "tree", &parent.join("store-relative")),
        AS_RELATIVE,
        "the root named by a relative path"
    );
    assert_eq!(
        stored_hashes(
            &parent,
            root.to_str().unwrap(),
            &parent.join("store-absolute")
        ),
        AS_ABSOLUTE,
        "the root named by an absolute path"
    );
}

/// A symlink on the way to the root. Named by a relative path, the key is
/// the path as written, which is what 0.3.114 stored. Named by an absolute
/// path, 0.3.114 keyed each manifest by that absolute path, so the hash
/// differed with the directory the checkout sat in; carrick#2302 made it the
/// hash of the tree itself, and that stays.
#[cfg(unix)]
#[test]
fn a_symlinked_root_stores_a_hash_that_does_not_name_the_machine() {
    let (_tmp, parent) = tree();
    std::os::unix::fs::symlink(parent.join("tree"), parent.join("link")).unwrap();
    assert_eq!(
        stored_hashes(&parent, "link", &parent.join("store-relative-link")),
        AS_RELATIVE_LINK,
        "the root named by a relative path through a symlink"
    );
    assert_eq!(
        stored_hashes(
            &parent,
            parent.join("link").to_str().unwrap(),
            &parent.join("store-absolute-link")
        ),
        AS_ABSOLUTE,
        "the root named by an absolute path through a symlink"
    );
}
