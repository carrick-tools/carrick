//! The fact layer's files for a tree, handed to an in-process file
//! orchestrator as a scan hands them (carrick#3711 R1). A test that calls
//! `FileOrchestrator::analyze_files` itself gets no fact files unless it
//! passes these through `with_fact_files`, so no store answer reaches it.
//!
//! The walk is the real sidecar's, and the store is answered from the tree's
//! own `__llm__/library-claims/default.json` under `CARRICK_MOCK_ALL`.

#![allow(dead_code)]

use carrick::agents::file_orchestrator::FactFileSource;
use carrick::request_summary::fact_files::FactFiles;
use carrick::swc_scanner::{ScanResult, SwcScanner};
use std::path::{Path, PathBuf};

/// An installed route-table package for trees a test writes itself
/// (carrick#3711 R2): `makeTable()` builds a table whose `get` and `post`
/// register routes, and whose `route(path)` returns the verbs registered on
/// one path. With its lockfile entry and its store answer under `__llm__`.
pub fn install_table_package(root: &Path) {
    let files = [
        (
            "node_modules/fixture-table/package.json",
            r#"{ "name": "fixture-table", "version": "1.0.0", "types": "index.d.ts" }"#,
        ),
        ("node_modules/fixture-table/index.d.ts", TABLE_TYPES),
        (
            "package.json",
            r#"{ "name": "table-service", "version": "1.0.0", "private": true, "dependencies": { "fixture-table": "^1.0.0" } }"#,
        ),
        ("package-lock.json", TABLE_LOCK),
        (
            "tsconfig.json",
            r#"{ "compilerOptions": { "target": "es2022", "module": "commonjs", "moduleResolution": "node", "strict": true, "skipLibCheck": true, "types": [] }, "include": ["src/**/*"] }"#,
        ),
        ("__llm__/library-claims/default.json", TABLE_CLAIMS),
    ];
    for (relative, text) in files {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

const TABLE_TYPES: &str = "export interface Request {\n  path: string;\n}\n\
export interface Reply {\n  send(body: unknown): void;\n}\n\
export type Handler = (request: Request, reply: Reply) => unknown;\n\
/** What `Table.route` returns: the verbs registered on one path. */\n\
export interface Route {\n  get(...handlers: Handler[]): this;\n  post(...handlers: Handler[]): this;\n}\n\
export interface Table {\n  get(path: string, ...handlers: Handler[]): this;\n  post(path: string, ...handlers: Handler[]): this;\n  route(path: string): Route;\n}\n\
declare function makeTable(): Table;\nexport default makeTable;\n";

const TABLE_LOCK: &str = r#"{
  "name": "table-service",
  "version": "1.0.0",
  "lockfileVersion": 3,
  "requires": true,
  "packages": {
    "": { "name": "table-service", "version": "1.0.0", "dependencies": { "fixture-table": "^1.0.0" } },
    "node_modules/fixture-table": {
      "version": "1.0.0",
      "resolved": "https://registry.npmjs.org/fixture-table/-/fixture-table-1.0.0.tgz"
    }
  }
}
"#;

const TABLE_CLAIMS: &str = r#"{
  "library_claims": [
    {
      "package": "fixture-table",
      "version": "1.0.0",
      "status": "answered",
      "exports": [
        {
          "specifier": "fixture-table",
          "export": "default",
          "role": "server_framework",
          "side": null,
          "makes": [{ "form": "call", "member": null, "picker": "fixture-picker" }],
          "scopes": [
            { "member": "route", "of": "instance:()", "name": { "arg": 0 }, "picker": "fixture-picker" }
          ],
          "ops": [
            { "op": "receive", "member": "get", "of": "instance:()", "name": { "arg": 0 }, "handler": { "arg": 1 }, "method": "GET", "picker": "fixture-picker" },
            { "op": "receive", "member": "post", "of": "instance:()", "name": { "arg": 0 }, "handler": { "arg": 1 }, "method": "POST", "picker": "fixture-picker" },
            { "op": "receive", "member": "get", "of": "instance:()>scope:route", "name": { "bound": "scope" }, "handler": { "arg": 0 }, "method": "GET", "picker": "fixture-picker" },
            { "op": "receive", "member": "post", "of": "instance:()>scope:route", "name": { "bound": "scope" }, "handler": { "arg": 0 }, "method": "POST", "picker": "fixture-picker" }
          ],
          "reserved": [],
          "calls": [],
          "patterns": []
        }
      ]
    }
  ]
}
"#;

/// The built sidecar. CI builds it before the Rust tests; without it these
/// tests prove nothing, so its absence fails them rather than skipping.
pub fn sidecar_script() -> PathBuf {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    assert!(
        script.exists(),
        "the fact layer needs the built sidecar at {}",
        script.display()
    );
    script
}

/// The fact files of `repo`'s first service, its store answered from
/// `repo/__llm__`. Sets `CARRICK_MOCK_ALL` and `CARRICK_MOCK_FIXTURE_DIR`
/// (callers run serially) and leaves `CARRICK_MOCK_FIXTURE_DIR` unset after.
pub async fn walked(repo: &Path) -> FactFiles {
    // SAFETY: the callers' tests run one at a time; env vars are
    // process-global.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var("CARRICK_MOCK_FIXTURE_DIR", repo.join("__llm__"));
    }
    let rows = tempfile::tempdir().expect("a rows dir");
    let facts =
        carrick::engine::fact_seam::fact_files_of_tree(repo, &sidecar_script(), rows.path())
            .await
            .unwrap_or_else(|| panic!("{} asks the store nothing", repo.display()));
    unsafe {
        std::env::remove_var("CARRICK_MOCK_FIXTURE_DIR");
    }
    facts
}

/// [`walked`] from synchronous code, or from inside a test's runtime: on a
/// thread and a runtime of its own.
pub fn walked_blocking(repo: &Path) -> FactFiles {
    let repo = repo.to_path_buf();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(walked(&repo))
    })
    .join()
    .expect("the walk ran")
}

/// `facts` as the engine sends them: already arrived.
pub fn source(facts: &FactFiles) -> FactFileSource {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    sender
        .send(Some(facts.clone()))
        .expect("the receiver is held");
    FactFileSource::later(receiver)
}

/// The fact layer's listed calls in `file` (relative to `repo`), as the
/// orchestrator lists them for the reader (`scan_sites`), with their ids.
pub fn listed_sites(facts: &FactFiles, repo: &Path, file: &str) -> ScanResult {
    let path = repo.join(file);
    let content = std::fs::read_to_string(&path).expect("the file reads");
    SwcScanner::new().scan_sites(&path, &content, &facts.listed_in(file))
}

/// The id of the fact layer's listed call in `file` (relative to `repo`)
/// that starts on `line`, as the model would echo it.
pub fn listed_candidate_at(
    facts: &FactFiles,
    repo: &Path,
    file: &str,
    line: usize,
) -> Option<String> {
    listed_sites(facts, repo, file)
        .candidates
        .into_iter()
        .find(|candidate| candidate.line_number == line)
        .map(|candidate| candidate.candidate_id)
}
