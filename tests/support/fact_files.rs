//! The fact layer's files for a tree, handed to an in-process file
//! orchestrator as a scan hands them (carrick#3711 R1). A test that calls
//! `FileOrchestrator::analyze_files` itself gets no fact files unless it
//! passes these through `with_fact_files`, so no store answer reaches it.
//!
//! The walk is the real sidecar's, and the store is answered from the tree's
//! own `__llm__/library-claims/default.json` under `CARRICK_MOCK_ALL`.

#![allow(dead_code)]

#[path = "real_packages.rs"]
mod real_packages;

use carrick::agents::file_orchestrator::FactFileSource;
use carrick::request_summary::fact_files::FactFiles;
use carrick::swc_scanner::{ScanResult, SwcScanner};
use std::path::{Path, PathBuf};

/// A package invented for trees a test writes itself (carrick#3711 R2): its
/// declarations, and the store's answer for it (one `library_claims` entry).
pub struct Invented {
    pub name: &'static str,
    pub types: &'static str,
    pub claims: &'static str,
}

/// The installed route-table package: `makeTable()` builds a `Table` whose
/// `get` and `post` register routes, and whose `route(path)` returns the
/// verbs registered on one path.
pub const TABLE: Invented = Invented {
    name: "fixture-table",
    types: TABLE_TYPES,
    claims: TABLE_CLAIMS,
};

/// The installed request-client package: `makeClient()` builds a `Client`
/// whose `get(url)` sends a request, and `makeConfigClient()` a
/// `ConfigClient` whose `get({ url })` and `post({ url, body })` do.
pub const HTTP: Invented = Invented {
    name: "fixture-http",
    types: HTTP_TYPES,
    claims: HTTP_CLAIMS,
};

/// The installed plugin server `inline-registration-scope` declares: an `App`
/// whose `register(plugin, { prefix })` and `group(prefix, fn)` run a
/// callback under a prefix, and whose `get` and `post` register routes. Read
/// from that fixture, so there is one copy.
pub const PLUGIN: Invented = Invented {
    name: "fixture-plugin-server",
    types: include_str!(
        "../fixtures/inline-registration-scope/node_modules/fixture-plugin-server/index.d.ts"
    ),
    claims: include_str!(
        "../fixtures/inline-registration-scope/__llm__/library-claims/default.json"
    ),
};

/// [`TABLE`] installed in `root`, with its lockfile entry and its store answer
/// under `__llm__`.
pub fn install_table_package(root: &Path) {
    install_invented(root, &[&TABLE]);
}

/// Each of `packages` installed in `root`: its `node_modules` entry, the
/// manifest's dependency and the lockfile's public entry, a tsconfig, and
/// their store answers under `__llm__`.
pub fn install_invented(root: &Path, packages: &[&Invented]) {
    let dependencies: serde_json::Map<String, serde_json::Value> = packages
        .iter()
        .map(|package| (package.name.to_string(), serde_json::json!("^1.0.0")))
        .collect();
    let mut locked = serde_json::Map::new();
    locked.insert(
        String::new(),
        serde_json::json!({ "name": "table-service", "version": "1.0.0", "dependencies": dependencies }),
    );
    for package in packages {
        let name = package.name;
        locked.insert(
            format!("node_modules/{name}"),
            serde_json::json!({
                "version": "1.0.0",
                "resolved": format!("https://registry.npmjs.org/{name}/-/{name}-1.0.0.tgz"),
            }),
        );
        write(
            root,
            &format!("node_modules/{name}/package.json"),
            &serde_json::json!({ "name": name, "version": "1.0.0", "types": "index.d.ts" })
                .to_string(),
        );
        write(
            root,
            &format!("node_modules/{name}/index.d.ts"),
            package.types,
        );
    }
    write(
        root,
        "package.json",
        &serde_json::json!({ "name": "table-service", "version": "1.0.0", "private": true, "dependencies": dependencies })
            .to_string(),
    );
    write(
        root,
        "package-lock.json",
        &serde_json::to_string_pretty(&serde_json::json!({
            "name": "table-service",
            "version": "1.0.0",
            "lockfileVersion": 3,
            "requires": true,
            "packages": locked,
        }))
        .unwrap(),
    );
    write(
        root,
        "tsconfig.json",
        r#"{ "compilerOptions": { "target": "es2022", "module": "commonjs", "moduleResolution": "node", "strict": true, "skipLibCheck": true, "types": [] }, "include": ["src/**/*"] }"#,
    );
    // An answer is one `library_claims` entry, or a whole cassette of them.
    let claims: Vec<serde_json::Value> = packages
        .iter()
        .flat_map(|package| {
            let answer: serde_json::Value =
                serde_json::from_str(package.claims).expect("an invented answer");
            match answer.get("library_claims") {
                Some(serde_json::Value::Array(entries)) => entries.clone(),
                _ => vec![answer],
            }
        })
        .collect();
    write(
        root,
        "__llm__/library-claims/default.json",
        &serde_json::json!({ "library_claims": claims }).to_string(),
    );
}

/// The real-name stand-ins `installed` (`tests/fixtures/real-packages`) in
/// `root`, with a manifest listing them when it has none, and the store's
/// answer for `answered` under `root/__llm__`.
pub fn install_real(root: &Path, installed: &[&str], answered: &[&str]) {
    if !root.join("package.json").exists() {
        let dependencies: serde_json::Map<String, serde_json::Value> = installed
            .iter()
            .map(|name| (name.to_string(), serde_json::json!("*")))
            .collect();
        std::fs::write(
            root.join("package.json"),
            serde_json::json!({ "name": "fixture", "private": true, "dependencies": dependencies })
                .to_string(),
        )
        .unwrap();
    }
    real_packages::install(root, installed);
    real_packages::write_claims(&root.join("__llm__"), answered);
}

fn write(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

const TABLE_TYPES: &str = "export interface Request {\n  path: string;\n}\n\
export interface Reply {\n  send(body: unknown): void;\n}\n\
export type Handler = (request: Request, reply: Reply) => unknown;\n\
/** What `Table.route` returns: the verbs registered on one path. */\n\
export interface Route {\n  get(...handlers: Handler[]): this;\n  post(...handlers: Handler[]): this;\n}\n\
export interface Table {\n  get(path: string, ...handlers: Handler[]): this;\n  post(path: string, ...handlers: Handler[]): this;\n  route(path: string): Route;\n}\n\
declare function makeTable(): Table;\nexport default makeTable;\n";

const TABLE_CLAIMS: &str = r#"{
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
"#;

const HTTP_TYPES: &str = "export interface Client {\n  get(url: string): Promise<unknown>;\n}\n\
export interface RequestConfig {\n  url: string;\n  params?: unknown;\n  body?: unknown;\n}\n\
export interface ConfigClient {\n  get(config: RequestConfig): Promise<unknown>;\n  post(config: RequestConfig): Promise<unknown>;\n}\n\
declare function makeClient(): Client;\nexport default makeClient;\n\
export declare function makeConfigClient(): ConfigClient;\n";

const HTTP_CLAIMS: &str = r#"{
      "package": "fixture-http",
      "version": "1.0.0",
      "status": "answered",
      "exports": [
        {
          "specifier": "fixture-http",
          "export": "default",
          "role": "http_client",
          "side": "client",
          "makes": [{ "form": "call", "member": null, "picker": "fixture-picker" }],
          "scopes": [],
          "ops": [
            { "op": "request", "member": "get", "of": "instance:()", "name": { "arg": 0 }, "method_default": "GET", "picker": "fixture-picker" }
          ],
          "reserved": [],
          "calls": [],
          "patterns": []
        },
        {
          "specifier": "fixture-http",
          "export": "makeConfigClient",
          "role": "http_client",
          "side": "client",
          "makes": [{ "form": "call", "member": null, "picker": "fixture-picker" }],
          "scopes": [],
          "ops": [
            { "op": "request", "member": "get", "of": "instance:()", "name": { "arg": 0, "key": "url" }, "method_default": "GET", "picker": "fixture-picker" },
            { "op": "request", "member": "post", "of": "instance:()", "name": { "arg": 0, "key": "url" }, "method_default": "POST", "picker": "fixture-picker" }
          ],
          "reserved": [],
          "calls": [],
          "patterns": []
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
/// `repo/__llm__`. Sets `CARRICK_MOCK_ALL`, and `CARRICK_MOCK_FIXTURE_DIR`
/// for the walk only (callers run serially).
pub async fn walked(repo: &Path) -> FactFiles {
    // SAFETY: the callers' tests run one at a time; env vars are
    // process-global. The caller's own cassette dir is put back after.
    let before = std::env::var_os("CARRICK_MOCK_FIXTURE_DIR");
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
        match before {
            Some(dir) => std::env::set_var("CARRICK_MOCK_FIXTURE_DIR", dir),
            None => std::env::remove_var("CARRICK_MOCK_FIXTURE_DIR"),
        }
    }
    facts
}

/// The fact files of the committed tree at `fixture`, walked on a copy with
/// the real-name stand-ins `installed` (`tests/fixtures/real-packages`) and
/// the store's answer for `answered` (stand-ins, plus answers for packages a
/// scan never installs, such as `lib-dom-fetch`). The tree itself is left
/// alone: its other tests pin it as it is. The files are keyed by path
/// relative to the tree, so they apply to the committed tree unchanged.
pub fn walked_copy(fixture: &Path, installed: &[&str], answered: &[&str]) -> FactFiles {
    let dir = tempfile::tempdir().expect("a copy dir");
    let copy = dir.path().canonicalize().unwrap().join("repo");
    copy_tree(fixture, &copy);
    install_real(&copy, installed, answered);
    walked_blocking(&copy)
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
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
