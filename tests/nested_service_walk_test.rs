//! A file is indexed under one service: the deepest one whose directory holds
//! it (carrick#553).
//!
//! A service's walk used to read the directories of the services nested in
//! it, so a `.` service beside a worker under `workers/` indexed the worker's
//! routes and functions under both names and sent its files to model analysis
//! twice.
//!
//! A checkout of its own inside a service is not that service's source
//! either (carrick#1902). A linked worktree is a whole copy of the repository
//! it sits in, and a root service read every copy as its own: each copy's
//! files were sent to model analysis and its routes and functions indexed
//! beside the real ones.
//!
//! Drives the real scanner binary offline over a tree written here, through
//! the same `carrick.json` resolution a scan uses, and reads
//! `function_definitions` from each WRITTEN BLOB: what a service's blob holds
//! is what the index says that service contains. Function definitions are
//! deterministic, so no cassette is needed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

fn write(root: &Path, file: &str, body: &str) {
    let target = root.join(file);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(target, body).unwrap();
}

/// Service name -> the functions its blob holds, and what the scan printed.
struct Scan {
    functions: BTreeMap<String, BTreeSet<String>>,
    output: String,
}

impl Scan {
    fn of(&self, service: &str) -> Vec<&str> {
        self.functions
            .get(service)
            .unwrap_or_else(|| panic!("no blob for service {service}:\n{}", self.output))
            .iter()
            .map(String::as_str)
            .collect()
    }
}

/// Run the scanner over `repo`, offline. Whether it exited zero, everything
/// it printed, and the directory its blobs were written to.
fn run(repo: &Path) -> (bool, String, tempfile::TempDir) {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let cassettes = tempfile::tempdir().expect("temp cassette dir");

    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.path().display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        // Which service a file is indexed under is the assertion; the type
        // layer is not under test.
        .env("CARRICK_ALLOW_MISSING_TYPES", "1");
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
    ] {
        cmd.env_remove(var);
    }
    let result = cmd.output().expect("failed to spawn carrick");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    (result.status.success(), output, storage)
}

fn scan(repo: &Path) -> Scan {
    let (succeeded, output, storage) = run(repo);
    assert!(succeeded, "scan exited non-zero:\n{output}");

    let mut functions = BTreeMap::new();
    for entry in std::fs::read_dir(storage.path())
        .expect("storage dir")
        .flatten()
    {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let blob: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read blob")).expect("parse blob");
        let service = blob["service_name"]
            .as_str()
            .expect("a blob names its service")
            .to_string();
        let names = blob["function_definitions"]
            .as_object()
            .expect("blob has function_definitions")
            .keys()
            .cloned()
            .collect();
        functions.insert(service, names);
    }
    Scan { functions, output }
}

/// One repository declaring every shape the rule has to answer for: a service
/// at the root, a service nested in it, one nested two levels down, an
/// `include` root that sits inside a nested service, and two services that
/// declare the same directory.
#[test]
fn each_file_is_indexed_under_the_deepest_service_that_holds_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "carrick.json",
        r#"{
          "services": [
            { "name": "storefront", "directory": ".", "include": ["workers/edge/shared"] },
            { "name": "edge", "directory": "workers/edge" },
            { "name": "edge-cron", "directory": "workers/edge/cron" },
            { "name": "reports", "directory": "tools/reports" },
            { "name": "reports-twin", "directory": "./tools/reports/" }
          ]
        }"#,
    );
    write(root, "package.json", r#"{"name":"storefront"}"#);
    write(
        root,
        "src/orders.ts",
        "export function listOrders() { return [1]; }\n",
    );
    write(
        root,
        "workers/edge/src/index.ts",
        "export function forwardRequest(request: Request) { return fetch(request); }\n",
    );
    write(
        root,
        "workers/edge/shared/headers.ts",
        "export function sharedHeaders() { return { accept: 'application/json' }; }\n",
    );
    write(
        root,
        "workers/edge/cron/tick.ts",
        "export function tick() { return Date.now(); }\n",
    );
    write(
        root,
        "tools/reports/build.ts",
        "export function buildReport() { return 'report'; }\n",
    );

    let scan = scan(root);

    // The root service: its own source and the one directory it includes on
    // purpose. Not the worker, the worker's cron job, or the reports tool.
    assert_eq!(scan.of("storefront"), ["listOrders", "sharedHeaders"]);
    // The nested service: its own tree, which holds the shared directory the
    // root also includes, and not the service nested inside it.
    assert_eq!(scan.of("edge"), ["forwardRequest", "sharedHeaders"]);
    // Two levels down: read by neither service above it.
    assert_eq!(scan.of("edge-cron"), ["tick"]);
    // One directory declared twice is read by both: neither is inside the
    // other.
    assert_eq!(scan.of("reports"), ["buildReport"]);
    assert_eq!(scan.of("reports-twin"), ["buildReport"]);
}

/// Run git in `dir` as a fixture author, clear of whatever git environment
/// and configuration the test process inherited: a pre-commit hook exports
/// `GIT_DIR` for the repository it runs in, and a developer's own config can
/// sign commits or run hooks of its own.
fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.test",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A repository with one commit: a service at its root holding one function,
/// and the folder a coding agent keeps its worktrees in, ignored.
fn committed_repository(root: &Path) {
    write(
        root,
        "carrick.json",
        r#"{ "services": [{ "name": "storefront", "directory": "." }] }"#,
    );
    write(root, "package.json", r#"{"name":"storefront"}"#);
    write(
        root,
        "src/orders.ts",
        "export function listOrders() { return [1]; }\n",
    );
    write(root, ".gitignore", ".agent/worktrees/\n");
    git(root, &["init", "-q"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "one service"]);
}

/// carrick#1902, the measured case, with the worktree made by git itself: a
/// service at the repository root and a linked worktree of the repository in
/// a folder the repository ignores. The worktree is a copy of every file, at
/// whatever commit its branch is on, and it is scanned zero times.
#[test]
fn a_root_service_is_scanned_once_beside_a_worktree_copy_of_itself() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    committed_repository(root);
    git(
        root,
        &[
            "worktree",
            "add",
            "-q",
            ".agent/worktrees/task",
            "-b",
            "task",
        ],
    );
    // The copy has moved on from the commit the root is at.
    write(
        root,
        ".agent/worktrees/task/src/orders.ts",
        "export function listOrders() { return [1]; }\n\
         export function onlyInTheWorktree() { return [2]; }\n",
    );

    let scan = scan(root);

    assert_eq!(scan.of("storefront"), ["listOrders"]);
    assert!(
        scan.output.contains("Discovered 1 file(s) under "),
        "the copy is not among the files the walk found:\n{}",
        scan.output
    );
    assert!(
        scan.output.contains(
            "Left out 1 folder(s) with their own .git: .agent/worktrees/task. Name one under \
             \"include\" in carrick.json to scan it."
        ),
        "the scan says what it left out and how to bring it back:\n{}",
        scan.output
    );
}

/// The other two checkouts a repository can hold, made by git itself. A
/// submodule is declared by the repository and pinned by its commit, so it is
/// read as before. A clone nobody declared is left out, until the config
/// names it.
#[test]
fn a_declared_submodule_is_read_and_an_undeclared_clone_is_read_when_named() {
    let library = tempfile::tempdir().unwrap();
    write(
        library.path(),
        "client.ts",
        "export function sharedClient() { return fetch('/health'); }\n",
    );
    git(library.path(), &["init", "-q"]);
    git(library.path(), &["add", "-A"]);
    git(library.path(), &["commit", "-q", "-m", "shared client"]);
    let library_url = library.path().to_str().unwrap();

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    committed_repository(root);
    git(
        root,
        &["submodule", "add", "-q", library_url, "libs/shared"],
    );
    git(root, &["clone", "-q", library_url, "scratch/clone"]);

    let scan_before = scan(root);
    assert_eq!(
        scan_before.of("storefront"),
        ["listOrders", "sharedClient"],
        "the submodule's source is the repository's, the clone's is not:\n{}",
        scan_before.output
    );
    assert!(
        scan_before
            .output
            .contains("Left out 1 folder(s) with their own .git: scratch/clone."),
        "{}",
        scan_before.output
    );

    // Named under `include`, the clone is a root of the walk and is read.
    write(
        root,
        "scratch/clone/extra.ts",
        "export function onlyInTheClone() { return 3; }\n",
    );
    write(
        root,
        "carrick.json",
        r#"{ "services": [{ "name": "storefront", "directory": ".", "include": ["scratch/clone"] }] }"#,
    );
    let scan_named = scan(root);
    // Both checkouts of the library are read now, so its one function is
    // defined twice and each definition is keyed by its file.
    assert_eq!(
        scan_named.of("storefront"),
        [
            "listOrders",
            "onlyInTheClone",
            "sharedClient@libs/shared/client.ts",
            "sharedClient@scratch/clone/client.ts"
        ]
    );
    assert!(
        !scan_named.output.contains("Left out "),
        "nothing is left out once the config names it:\n{}",
        scan_named.output
    );
}

/// A scan pointed at a folder that holds repositories and is not one: every
/// source file sits in a checkout of its own. The scan stops, and the error
/// names the checkouts rather than sending the reader to a config that is
/// not the problem.
#[test]
fn a_folder_of_checkouts_is_refused_with_the_checkouts_named() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for repo in ["billing", "catalogue"] {
        write(
            root,
            &format!("{repo}/src/index.ts"),
            "export function handler() { return 1; }\n",
        );
        std::fs::create_dir_all(root.join(repo).join(".git")).unwrap();
    }

    let (succeeded, output, _storage) = run(root);

    assert!(!succeeded, "a scan of no source is refused:\n{output}");
    assert!(
        output.contains(
            "Left out 2 folder(s) with their own .git: billing, catalogue. Scan one of them, or \
             name it under \"include\" in carrick.json."
        ),
        "{output}"
    );
}

/// Every walk of a repository's tree stops at the same checkout
/// ([`carrick::file_finder::git_boundary`]). One tree, asked of each walk: a
/// workspace with a copy of itself in a plain folder, so the dot folders some
/// of these walks skip on their own account decide nothing here.
#[test]
fn every_walk_of_the_tree_leaves_the_same_checkout_out() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    write(
        root,
        "package.json",
        r#"{"name":"shop","private":true,"workspaces":["**/packages/*"]}"#,
    );
    let package = |at: &str, source: &str| {
        write(
            root,
            &format!("{at}/packages/shared/package.json"),
            r#"{"name":"@shop/shared","main":"./index.ts"}"#,
        );
        write(root, &format!("{at}/packages/shared/index.ts"), source);
    };
    package(".", "export function price() { return 1; }\n");
    write(root, "schema.graphql", "type Query { orders: [String] }\n");
    // The copy: an older state of the same tree, in a folder that sorts
    // before the real package, with a package and a field only it holds.
    let copy = "agent-trees/task";
    write(root, &format!("{copy}/.git"), "gitdir: /elsewhere\n");
    package(copy, "export function price() { return 0; }\n");
    write(
        root,
        &format!("{copy}/packages/retired/package.json"),
        r#"{"name":"@shop/retired"}"#,
    );
    write(
        root,
        &format!("{copy}/packages/retired/index.ts"),
        "export const retired = true;\n",
    );
    write(
        root,
        &format!("{copy}/schema.graphql"),
        "type Query { orders: [String] retiredReport: String }\n",
    );

    let in_the_copy = |path: &Path| path.starts_with(root.join("agent-trees"));

    // The source walk, which the pre-flight and the external-call inventory
    // read their files through.
    let (files, _) = carrick::file_finder::find_files(
        root.to_str().unwrap(),
        &carrick::packages::MANIFEST_SKIP_DIRS,
    );
    assert!(
        !files.is_empty() && !files.iter().any(|file| in_the_copy(file)),
        "{files:?}"
    );

    // The manifest walks: which package names are this repository's, and
    // which directory an import of one resolves to.
    let names = carrick::packages::collect_internal_package_names(root);
    assert!(
        names.contains("@shop/shared") && !names.contains("@shop/retired"),
        "{names:?}"
    );
    assert_eq!(
        carrick::workspace_resolver::WorkspaceIndex::build(root)
            .resolve(Path::new("apps/web/checkout.ts"), "@shop/shared"),
        carrick::workspace_resolver::Resolution::Internal(PathBuf::from(
            "packages/shared/index.ts"
        )),
        "an import of a workspace package resolves into the repository, not into a copy of it"
    );

    // The workspace-member walk: the copy's packages are no services.
    let derived = carrick::service_derivation::resolve(root).unwrap();
    let directories: Vec<Option<&str>> = derived
        .services
        .iter()
        .map(|service| service.directory.as_deref())
        .collect();
    assert_eq!(directories, [Some("packages/shared")]);

    // The GraphQL walk: a schema in the copy is not one this service serves.
    let served: Vec<String> = carrick::graphql::scan_repo(&[root.to_path_buf()], &[], &[])
        .producers
        .iter()
        .map(|operation| operation.key.canonical())
        .collect();
    assert_eq!(served, ["graphql|query|orders"]);
}
