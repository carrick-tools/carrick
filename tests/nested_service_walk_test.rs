//! A file is indexed under one service: the deepest one whose directory holds
//! it (carrick#553).
//!
//! A service's walk used to read the directories of the services nested in
//! it, so a `.` service beside a worker under `workers/` indexed the worker's
//! routes and functions under both names and sent its files to model analysis
//! twice.
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

fn scan(repo: &Path) -> Scan {
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
    assert!(result.status.success(), "scan exited non-zero:\n{output}");

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
