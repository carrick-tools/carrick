//! `carrick derive --workspace <root>` and a scan of `<root>` name the same
//! services (carrick#1858).
//!
//! The Action's installer asks the command which services to prepare
//! (`scripts/install-scanned-deps.sh`), and the scan derives its own from the
//! same root with `service_derivation::resolve`. The command first decides
//! which REPOSITORIES the folder holds, and at the root of one git repository
//! whose packages sit in directories below it, it answered "a folder of
//! repositories": each package directory was a repository with one service,
//! and the scan's services were never asked for.
//!
//! Each case asks both and compares the service roots, resolved the way the
//! installer resolves them: a service's `directory` against its repository's
//! path.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn write(root: &Path, file: &str, body: &str) {
    let target = root.join(file);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(target, body).unwrap();
}

fn package(root: &Path, directory: &str, name: &str) {
    write(
        root,
        &format!("{directory}/package.json"),
        &format!(r#"{{"name":"{name}"}}"#),
    );
    write(
        root,
        &format!("{directory}/src/index.ts"),
        "export const port = 3000;\n",
    );
}

/// The service roots the installer is told about.
fn asked_of_the_command(root: &Path) -> BTreeSet<PathBuf> {
    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .args(["derive", "--workspace"])
        .arg(root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let derived: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    derived["repos"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|repo| {
            let path = PathBuf::from(repo["path"].as_str().unwrap());
            repo["services"]
                .as_array()
                .unwrap()
                .iter()
                .map(move |service| match service["directory"].as_str() {
                    Some(directory) => path.join(directory),
                    None => path.clone(),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The service roots a scan of `root` reads.
fn asked_of_the_scan(root: &Path) -> BTreeSet<PathBuf> {
    carrick::service_derivation::resolve(root)
        .unwrap()
        .services
        .iter()
        .map(|service| match service.directory.as_deref() {
            Some(directory) => root.join(directory),
            None => root.to_path_buf(),
        })
        .collect()
}

/// One git repository, a root manifest that declares no workspace, and
/// package directories one and two levels down.
#[test]
fn a_git_repository_with_package_directories_is_one_repository_to_both() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap().join("shop");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    write(&root, "package.json", r#"{"private":true}"#);
    package(&root, "web", "web");
    package(&root, "api", "api");
    package(&root, "tools/sync", "sync");

    assert_eq!(asked_of_the_command(&root), asked_of_the_scan(&root));
}

/// The shapes that already agreed still do: a declared workspace, an explicit
/// `carrick.json`, and a single package.
#[test]
fn a_declared_workspace_an_explicit_config_and_a_single_package_agree() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();

    let workspace = base.join("workspace");
    std::fs::create_dir_all(workspace.join(".git")).unwrap();
    write(
        &workspace,
        "package.json",
        r#"{"private":true,"workspaces":["apps/*"]}"#,
    );
    package(&workspace, "apps/web", "web");
    package(&workspace, "apps/api", "api");
    assert_eq!(
        asked_of_the_command(&workspace),
        asked_of_the_scan(&workspace)
    );

    let declared = base.join("declared");
    std::fs::create_dir_all(declared.join(".git")).unwrap();
    package(&declared, "web", "web");
    package(&declared, "api", "api");
    write(
        &declared,
        "carrick.json",
        r#"{"services":[{"name":"web","directory":"web"},{"name":"api","directory":"api"}]}"#,
    );
    assert_eq!(
        asked_of_the_command(&declared),
        asked_of_the_scan(&declared)
    );

    let single = base.join("single");
    std::fs::create_dir_all(single.join(".git")).unwrap();
    package(&single, ".", "single");
    assert_eq!(asked_of_the_command(&single), asked_of_the_scan(&single));
}
