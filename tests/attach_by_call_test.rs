//! The source of a same-repo module a file imports rides in that file's
//! analysis prompt only where a call in the file reaches the module
//! (carrick#1928).
//!
//! Which modules a file may carry is what it was: the wrapper modules its
//! import table reaches through relative specifiers and their re-export
//! barrels. The call rule keeps one of them only when a call site in the file
//! reaches it, and is answered by what discovery resolved: the call graph says
//! where a site lands, or, where it recorded no target, the site's callee
//! chain is rooted at a binding the file imports from the module. An import
//! that is only handed on as an argument, named in a type, rendered as a JSX
//! element, or never used attaches nothing, and a file with no candidate of
//! its own is asked about only when it holds such a call. So the rule only
//! removes: an import written through an alias or a package name attached
//! nothing before and attaches nothing now (carrick#474).
//!
//! Every test scans a small synthetic service through the engine, with the
//! model mocked, and reads the prompts the scan would have sent
//! (`CARRICK_EVAL_DUMP_DIR`). The engine is the entry on purpose: the call
//! graph and the import bindings are discovery's, and the rule reads them.
//! The shapes are the ones the language defines for calling into another
//! module: a default export, a renamed re-export through a barrel, a
//! namespace import, a method on a receiver declared as an imported class, a
//! constructor call, and a value no declaration of which is callable by name.
//!
//! Its own test binary and `#[serial]`: the dump directory is read from the
//! environment and the scan-health registry is a process-global.

use serde_json::Value;
use serial_test::serial;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const SECTION: &str = "IMPORTED HTTP WRAPPER DEFINITIONS";

/// The modules a file can reach. Each one sends a request the scanner raises
/// and exports a binding, which is what makes a module's source attachable.
///
/// One service, rooted at the tree: the root manifest names no workspace, so
/// the package under `packages/` is part of the service that imports it.
const LIBRARY: &[(&str, &str)] = &[
    (
        "package.json",
        r#"{"name":"attach-by-call","version":"1.0.0"}"#,
    ),
    (
        "tsconfig.json",
        r#"{"compilerOptions":{"baseUrl":".","jsx":"preserve","paths":{"@lib/*":["src/lib/*"]}}}"#,
    ),
    (
        "packages/http/package.json",
        r#"{"name":"@acme/http","version":"1.0.0","main":"./src/index.ts"}"#,
    ),
    (
        "packages/http/src/index.ts",
        "export function send(path: string) {\n  return fetch(`https://package.example${path}`);\n}\n",
    ),
    (
        "src/lib/request.ts",
        "export default function request(path: string) {\n  return fetch(`https://default.example${path}`);\n}\n",
    ),
    (
        "src/lib/client.ts",
        "export function post(path: string, body: unknown) {\n  return fetch(`https://client.example${path}`, { method: \"POST\", body: JSON.stringify(body) });\n}\n",
    ),
    // A barrel: it sends nothing itself and publishes `post` under another
    // name.
    (
        "src/lib/index.ts",
        "export { post as submit } from \"./client\";\n",
    ),
    (
        "src/lib/aliased.ts",
        "export function viaAlias(path: string) {\n  return fetch(`https://aliased.example${path}`);\n}\n",
    ),
    (
        "src/lib/listing.ts",
        "export function list(path: string) {\n  return fetch(`https://listing.example${path}`);\n}\n",
    ),
    (
        "src/lib/service.ts",
        "export class TokenService {\n  issue(path: string) {\n    return fetch(`https://tokens.example${path}`);\n  }\n}\n",
    ),
    // What `api` holds is what a call returned: no declaration says what
    // `api.get` is, so the call graph follows no call through it.
    (
        "src/lib/held.ts",
        "function makeClient(base: string) {\n  return { get: (path: string) => fetch(`${base}${path}`) };\n}\nexport const api = makeClient(\"https://held.example\");\n",
    ),
    // A component that sends a request when it renders.
    (
        "src/lib/panel.tsx",
        "export function Panel() {\n  fetch(\"https://panel.example/items\");\n  return null;\n}\n",
    ),
];

/// One synthetic service on disk.
struct Tree {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Tree {
    /// The library and `files`, under a fresh root.
    fn with(files: &[(&str, &str)]) -> Self {
        Self::of(LIBRARY.iter().chain(files.iter()))
    }

    fn of<'a>(files: impl IntoIterator<Item = &'a (&'a str, &'a str)>) -> Self {
        let tmp = tempfile::tempdir().expect("a temp dir");
        let root = tmp.path().canonicalize().expect("a canonical temp dir");
        for (relative, contents) in files {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
        }
        Self { _tmp: tmp, root }
    }

    /// Scan the service and return the prompt of every file the scan asked
    /// the model about, keyed by the path the prompt names the file by.
    async fn prompts(&self) -> BTreeMap<String, String> {
        offline();
        let dump = tempfile::tempdir().unwrap();
        dump_into(Some(dump.path()));
        let scanned = carrick::engine::run_analysis_engine_with_sidecar(
            carrick::cloud_storage::MockStorage::new(),
            self.root.to_str().unwrap(),
            None,
            true,
        )
        .await;
        dump_into(None);
        scanned.expect("a mocked scan");

        let mut prompts = BTreeMap::new();
        for entry in std::fs::read_dir(dump.path()).expect("the dump dir") {
            let path = entry.unwrap().path();
            let payload: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            prompts.insert(
                payload["file_path"].as_str().unwrap().to_string(),
                payload["request_user_message"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }
        prompts
    }
}

fn offline() {
    // SAFETY: every test in this binary is `#[serial]`, so no other thread
    // reads the environment while these are set.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var("CARRICK_SKIP_INTENTS", "1");
        std::env::remove_var("CARRICK_MOCK_FIXTURE_DIR");
        std::env::remove_var("CARRICK_EVAL_DUMP_DIR");
    }
    // The registry files a loss under the service being scanned and keeps it
    // for the life of the process.
    carrick::scan_health::enter_service(None);
    carrick::scan_health::forget_service_losses(None);
}

fn dump_into(dir: Option<&Path>) {
    // SAFETY: `#[serial]`, as in `offline`.
    unsafe {
        match dir {
            Some(dir) => std::env::set_var("CARRICK_EVAL_DUMP_DIR", dir),
            None => std::env::remove_var("CARRICK_EVAL_DUMP_DIR"),
        }
    }
}

/// The body of the `### <name>` section of a prompt, or nothing when the
/// prompt has no such section.
fn section<'a>(prompt: &'a str, name: &str) -> &'a str {
    let header = format!("\n### {name}");
    let Some(start) = prompt.find(&header) else {
        return "";
    };
    let body = &prompt[start + 1..];
    let body = &body[body.find('\n').map_or(body.len(), |i| i + 1)..];
    &body[..body.find("\n### ").map_or(body.len(), |i| i + 1)]
}

/// The modules a prompt's imported-source section holds, in order.
fn attached(prompt: &str) -> Vec<&str> {
    section(prompt, SECTION)
        .lines()
        .filter_map(|line| line.strip_prefix("--- wrapper module: "))
        .map(|rest| rest.trim_end_matches(" ---"))
        .collect()
}

/// How many candidates a prompt lists.
fn candidates(prompt: &str) -> usize {
    section(prompt, "CANDIDATE CONTEXT")
        .lines()
        .filter(|line| line.starts_with("{\"candidate_id\""))
        .count()
}

/// The prompt of `file`, which the scan must have asked about.
fn prompt_of<'a>(prompts: &'a BTreeMap<String, String>, file: &str) -> &'a str {
    prompts.get(file).unwrap_or_else(|| {
        panic!(
            "{file} was not asked about; asked: {:?}",
            prompts.keys().collect::<Vec<_>>()
        )
    })
}

/// A request the file sends itself, so the scan asks about the file whatever
/// it imports.
const OWN: &str = "export const own = () => fetch(\"https://own.example/own\");\n";

#[tokio::test]
#[serial]
async fn a_call_keeps_the_module_it_reaches_however_the_import_is_written() {
    let tree = Tree::with(&[
        // A default export, called by the name this file gives it.
        (
            "src/default_export.ts",
            "import send from \"./lib/request\";\nexport const load = (path: string) => send(path);\n",
        ),
        // A re-export under another name, through a barrel.
        (
            "src/barrel_rename.ts",
            "import { submit } from \"./lib\";\nexport const save = (path: string, body: unknown) => submit(path, body);\n",
        ),
        // A member called off a namespace import.
        (
            "src/namespace_member.ts",
            "import * as listing from \"./lib/listing\";\nexport const load = (path: string) => listing.list(path);\n",
        ),
    ]);
    let prompts = tree.prompts().await;
    for (file, module) in [
        ("src/default_export.ts", "src/lib/request.ts"),
        ("src/barrel_rename.ts", "src/lib/client.ts"),
        ("src/namespace_member.ts", "src/lib/listing.ts"),
    ] {
        assert_eq!(attached(prompt_of(&prompts, file)), [module], "{file}");
    }
}

/// Only the call graph answers this one: the call is made on a parameter the
/// file declares to be the imported class, and the method is that class's.
/// The import itself is the root of no call and is constructed nowhere.
#[tokio::test]
#[serial]
async fn a_method_on_a_receiver_declared_as_an_imported_class_reaches_the_class_module() {
    let tree = Tree::with(&[(
        "src/declared_receiver.ts",
        "import { TokenService } from \"./lib/service\";\nexport const issue = (service: TokenService, path: string) => service.issue(path);\n",
    )]);
    let prompts = tree.prompts().await;
    assert_eq!(
        attached(prompt_of(&prompts, "src/declared_receiver.ts")),
        ["src/lib/service.ts"]
    );
}

/// A client class constructed into a field and called through the field: the
/// call graph follows no call on an untyped field, and the constructor call
/// is what says the file uses the module. The same import named only in a
/// type calls nothing.
#[tokio::test]
#[serial]
async fn a_class_constructed_into_a_field_keeps_its_module() {
    let tree = Tree::with(&[
        (
            "src/field_client.ts",
            "import { TokenService } from \"./lib/service\";\nexport class Tokens {\n  private service;\n  constructor() {\n    this.service = new TokenService();\n  }\n  issue(path: string) {\n    return this.service.issue(path);\n  }\n}\n",
        ),
        (
            "src/typed_only.ts",
            "import { TokenService } from \"./lib/service\";\nexport function describe(service: TokenService | undefined): string {\n  return typeof service;\n}\n",
        ),
    ]);
    let prompts = tree.prompts().await;
    assert_eq!(
        attached(prompt_of(&prompts, "src/field_client.ts")),
        ["src/lib/service.ts"]
    );
    assert!(
        !prompts.contains_key("src/typed_only.ts"),
        "a class named only in a type is called by nothing and the file is not asked about"
    );
}

/// Where the call graph follows no call, the binding the call is made
/// through says which module it comes from.
#[tokio::test]
#[serial]
async fn a_call_the_call_graph_does_not_follow_is_rooted_at_its_import() {
    let tree = Tree::with(&[
        // A member of what a factory returned.
        (
            "src/held_client.ts",
            "import { api } from \"./lib/held\";\nexport const load = (path: string) => api.get(path);\n",
        ),
        // The function's own `call` and `apply`.
        (
            "src/call_and_apply.ts",
            "import request from \"./lib/request\";\nexport const load = (path: string) => request.call(null, path);\nexport const again = (path: string) => request.apply(null, [path]);\n",
        ),
    ]);
    let prompts = tree.prompts().await;
    assert_eq!(
        attached(prompt_of(&prompts, "src/held_client.ts")),
        ["src/lib/held.ts"]
    );
    assert_eq!(
        attached(prompt_of(&prompts, "src/call_and_apply.ts")),
        ["src/lib/request.ts"]
    );
}

/// Each of these files sends a request of its own, so each is asked about,
/// and none of them makes a call through what it imports.
#[tokio::test]
#[serial]
async fn an_import_no_call_is_made_through_attaches_nothing() {
    let argument = format!(
        "import {{ post }} from \"./lib/client\";\ndeclare function keep(handler: unknown): void;\nkeep(post);\n{OWN}"
    );
    let type_only = format!(
        "import type {{ TokenService }} from \"./lib/service\";\nexport function describe(service: TokenService): string {{\n  return typeof service;\n}}\n{OWN}"
    );
    let never_called = format!("import {{ post }} from \"./lib/client\";\n{OWN}");
    // Rendering a component hands it to the JSX factory: no call of this
    // file's sends the component's request.
    let rendered = format!(
        "import {{ Panel }} from \"./lib/panel\";\nexport const Page = () => <Panel />;\n{OWN}"
    );
    let tree = Tree::with(&[
        ("src/argument_only.ts", argument.as_str()),
        ("src/type_only.ts", type_only.as_str()),
        ("src/never_called.ts", never_called.as_str()),
        ("src/rendered.tsx", rendered.as_str()),
    ]);
    let prompts = tree.prompts().await;
    for file in [
        "src/argument_only.ts",
        "src/type_only.ts",
        "src/never_called.ts",
        "src/rendered.tsx",
    ] {
        let prompt = prompt_of(&prompts, file);
        assert!(
            candidates(prompt) > 0,
            "{file} raises a candidate of its own"
        );
        assert!(
            !prompt.contains(&format!("### {SECTION}")),
            "{file} carries no imported source"
        );
    }
}

/// A file that raises no candidate is asked about only when a call in it
/// reaches a module it imports. Importing one is not enough.
#[tokio::test]
#[serial]
async fn a_file_with_no_candidate_is_asked_about_only_when_a_call_in_it_reaches_a_module() {
    let tree = Tree::with(&[
        (
            "src/calls.ts",
            "import { post } from \"./lib/client\";\nexport const save = (path: string, body: unknown) => post(path, body);\n",
        ),
        (
            "src/imports_only.ts",
            "import { post } from \"./lib/client\";\nexport const unused = 1;\n",
        ),
        (
            "src/hands_on.ts",
            "import { post } from \"./lib/client\";\ndeclare function keep(handler: unknown): void;\nkeep(post);\n",
        ),
        (
            "src/names_a_type.ts",
            "import type { TokenService } from \"./lib/service\";\nexport function describe(service: TokenService): string {\n  return typeof service;\n}\n",
        ),
        (
            "src/renders.tsx",
            "import { Panel } from \"./lib/panel\";\nexport const Page = () => <Panel />;\n",
        ),
    ]);
    let prompts = tree.prompts().await;

    let calls = prompt_of(&prompts, "src/calls.ts");
    assert_eq!(candidates(calls), 0, "the file raises no candidate");
    assert_eq!(attached(calls), ["src/lib/client.ts"]);

    for file in [
        "src/imports_only.ts",
        "src/hands_on.ts",
        "src/names_a_type.ts",
        "src/renders.tsx",
    ] {
        assert!(
            !prompts.contains_key(file),
            "{file} holds no call into a module and is not asked about"
        );
    }
}

/// The rule only removes. A module imported through a config alias or by a
/// package's name was never attached, because the pass reads the import
/// table through relative specifiers, and a call into it attaches nothing
/// now either (carrick#474).
#[tokio::test]
#[serial]
async fn an_import_written_through_an_alias_or_a_package_name_attaches_nothing() {
    let alias = "import { viaAlias } from \"@lib/aliased\";\nexport const load = (path: string) => viaAlias(path);\n";
    let package =
        "import { send } from \"@acme/http\";\nexport const load = (path: string) => send(path);\n";
    let alias_with_own = format!("{alias}{OWN}");
    let package_with_own = format!("{package}{OWN}");
    let tree = Tree::with(&[
        ("src/paths_alias.ts", alias),
        ("src/workspace_package.ts", package),
        ("src/paths_alias_own.ts", alias_with_own.as_str()),
        ("src/workspace_package_own.ts", package_with_own.as_str()),
    ]);
    let prompts = tree.prompts().await;
    for file in ["src/paths_alias.ts", "src/workspace_package.ts"] {
        assert!(
            !prompts.contains_key(file),
            "{file} raises no candidate and is not asked about"
        );
    }
    for file in ["src/paths_alias_own.ts", "src/workspace_package_own.ts"] {
        assert!(
            !prompt_of(&prompts, file).contains(&format!("### {SECTION}")),
            "{file} carries no imported source"
        );
    }
}

/// The section is a function of the tree: a second scan renders the same
/// prompts, and the order a file writes its imports in changes nothing in
/// what is attached or in what order.
#[tokio::test]
#[serial]
async fn the_attached_source_is_the_same_bytes_on_a_second_scan_and_under_reordered_imports() {
    let calls = "export const all = (path: string) => [send(path), list(path), post(path, {})];\n";
    let forward = format!(
        "import send from \"./lib/request\";\nimport {{ list }} from \"./lib/listing\";\nimport {{ post }} from \"./lib/client\";\n{calls}"
    );
    let backward = format!(
        "import {{ post }} from \"./lib/client\";\nimport {{ list }} from \"./lib/listing\";\nimport send from \"./lib/request\";\n{calls}"
    );
    let tree = Tree::with(&[
        ("src/forward.ts", forward.as_str()),
        ("src/backward.ts", backward.as_str()),
    ]);
    let first = tree.prompts().await;
    let second = tree.prompts().await;
    assert_eq!(first, second, "two scans of one tree");

    let modules = [
        "src/lib/client.ts",
        "src/lib/listing.ts",
        "src/lib/request.ts",
    ];
    let forward = prompt_of(&first, "src/forward.ts");
    let backward = prompt_of(&first, "src/backward.ts");
    assert_eq!(attached(forward), modules, "in sorted order");
    assert!(!section(forward, SECTION).is_empty());
    assert_eq!(
        section(forward, SECTION),
        section(backward, SECTION),
        "the same bytes whichever order the imports are written in"
    );
}

/// A file that imports no attachable module is asked exactly what it would be
/// asked in a service that holds none.
#[tokio::test]
#[serial]
async fn a_file_that_imports_no_attachable_module_keeps_its_prompt() {
    let standalone = (
        "src/standalone.ts",
        "export const own = (id: string) => fetch(`https://own.example/items/${id}`);\n",
    );
    let beside_the_library = Tree::with(&[standalone]).prompts().await;
    let alone = Tree::of(&[LIBRARY[0], LIBRARY[1], standalone])
        .prompts()
        .await;
    let prompt = prompt_of(&beside_the_library, "src/standalone.ts");
    assert!(!prompt.contains(&format!("### {SECTION}")));
    assert_eq!(prompt, prompt_of(&alone, "src/standalone.ts"));
}
