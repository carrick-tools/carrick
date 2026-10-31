//! carrick#2227: `--changed-only` analyses the services whose inputs changed
//! since their stored index, and carries the rest.
//!
//! The workspace is written by this test: three services in carrick.json
//! (`apps/api`, `apps/web`, `apps/worker`), a workspace package two of them
//! import by name, a library one reaches through a tsconfig `paths` alias, a
//! shared base tsconfig, and an ambient declaration directory `web`'s tsconfig
//! includes. `web` calls `api`'s route with a typed client, so a change to
//! `api`'s response flips `web`'s type verdict.
//!
//! Commit A is scanned on the offline harness (`CARRICK_LOCAL_STORAGE_DIR`,
//! cassettes as the model, no cloud, no spend). Each case is one commit from
//! A, scanned from a copy of that store with `--changed-only`; the store is
//! what the run wrote.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const REPO: &str = "fixture/mono";

const FILES: &[(&str, &str)] = &[
    (
        "package.json",
        r#"{ "name": "changed-only-fixture", "private": true, "workspaces": ["apps/*", "packages/*"] }
"#,
    ),
    ("package-lock.json", "{ \"lockfileVersion\": 3 }\n"),
    (
        "carrick.json",
        r#"{
  "services": [
    { "name": "api", "directory": "apps/api" },
    { "name": "web", "directory": "apps/web" },
    { "name": "worker", "directory": "apps/worker" }
  ]
}
"#,
    ),
    (
        "tsconfig.base.json",
        r#"{
  "compilerOptions": {
    "strict": true,
    "target": "es2022",
    "module": "esnext",
    "moduleResolution": "bundler",
    "skipLibCheck": true
  }
}
"#,
    ),
    ("README.md", "A workspace for the changed-only test.\n"),
    ("types/global.d.ts", "declare const BUILD_ID: string;\n"),
    (
        "packages/shared/package.json",
        r#"{ "name": "@fixture/shared", "version": "0.0.0", "main": "src/index.ts" }
"#,
    ),
    (
        "packages/shared/src/index.ts",
        "export { label } from \"./x\";\n",
    ),
    (
        "packages/shared/src/x.ts",
        "export function label(n: number): string {\n  return \"n\" + n;\n}\n",
    ),
    (
        "libs/util/index.ts",
        "export function twice(n: number): number {\n  return n * 2;\n}\n",
    ),
    (
        "apps/api/package.json",
        r#"{ "name": "fixture-api", "private": true, "dependencies": { "@fixture/shared": "*" } }
"#,
    ),
    (
        "apps/api/tsconfig.json",
        r#"{ "extends": "../../tsconfig.base.json", "include": ["src"] }
"#,
    ),
    (
        "apps/api/src/http-server.ts",
        r#"export interface Reply {
  json(body: unknown): void;
}
export interface Server {
  post(path: string, handler: (request: unknown, reply: Reply) => void): void;
}
export function createServer(): Server {
  return { post() {} };
}
"#,
    ),
    (
        "apps/api/src/types.ts",
        "export interface Order {\n  id: string;\n}\n",
    ),
    (
        "apps/api/src/model/index.ts",
        "export function describeOrder(order: { id: string }): string {\n  return order.id;\n}\n",
    ),
    ("apps/api/src/server.ts", SERVER_AT_A),
    (
        "apps/web/package.json",
        r#"{ "name": "fixture-web", "private": true }
"#,
    ),
    (
        "apps/web/tsconfig.json",
        r#"{ "extends": "../../tsconfig.base.json", "include": ["src", "../../types"] }
"#,
    ),
    (
        "apps/web/src/http-client.ts",
        r#"export interface ClientInstance {
  post<T = any>(url: string): Promise<T>;
}
export function create(options?: { baseURL?: string }): ClientInstance {
  throw new Error(`no client for ${options?.baseURL}`);
}
"#,
    ),
    (
        "apps/web/src/checkout.ts",
        r#"import { create } from "./http-client";
import type { Order } from "../../api/src/types";
import { describeOrder } from "../../api/src/model";

const api = create({ baseURL: "http://localhost:3000" });

export async function typedCheckout(order: Order): Promise<number> {
  const result = await api.post<{ x: number }>("/checkout");
  return result.x + describeOrder(order).length + BUILD_ID.length;
}
"#,
    ),
    (
        "apps/worker/package.json",
        r#"{ "name": "fixture-worker", "private": true, "dependencies": { "@fixture/shared": "*" } }
"#,
    ),
    (
        "apps/worker/tsconfig.json",
        r#"{
  "extends": "../../tsconfig.base.json",
  "compilerOptions": { "baseUrl": ".", "paths": { "@lib/*": ["../../libs/*"] } },
  "include": ["src"]
}
"#,
    ),
    ("apps/worker/src/job.ts", JOB_AT_A),
];

const SERVER_AT_A: &str = r#"import { createServer } from "./http-server";
import { label } from "@fixture/shared";
import type { Order } from "./types";

interface CheckoutResult {
  x: number;
}

const app = createServer();

app.post("/checkout", (request, reply) => {
  const result: CheckoutResult = { x: label(1).length };
  reply.json(result);
});

export type { Order };
"#;

const JOB_AT_A: &str = r#"import { label } from "@fixture/shared";
import { twice } from "@lib/util";

export function run(): string {
  return label(twice(2));
}
"#;

/// The model's answers, keyed by file stem as the mock reads them.
const CASSETTES: &[(&str, &str)] = &[
    (
        "analyze-file/server.json",
        r#"{
  "mounts": [],
  "endpoints": [
    {
      "candidate_id": "@line:11",
      "line_number": 11,
      "owner_node": "app",
      "method": "POST",
      "path": "/checkout",
      "handler_name": "anonymous",
      "pattern_matched": "app.post(",
      "payload_expression_text": null,
      "payload_expression_line": null,
      "response_expression_text": "reply.json(result)",
      "response_expression_line": 13,
      "primary_type_symbol": "CheckoutResult",
      "type_import_source": null
    }
  ],
  "data_calls": []
}"#,
    ),
    (
        "analyze-file/checkout.json",
        r#"{
  "mounts": [],
  "endpoints": [],
  "data_calls": [
    {
      "candidate_id": "@line:8",
      "line_number": 8,
      "target": "/checkout",
      "method": "POST",
      "pattern_matched": "api.post(",
      "call_expression_text": "api.post<{ x: number }>(\"/checkout\")",
      "call_expression_line": 8,
      "payload_expression_text": null,
      "payload_expression_line": null,
      "primary_type_symbol": null,
      "type_import_source": null
    }
  ]
}"#,
    ),
];

const EMPTY_ANSWER: &str = r#"{ "mounts": [], "endpoints": [], "data_calls": [] }"#;

fn run_git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git failed to spawn");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// What one scan printed and how it exited.
struct Scan {
    output: String,
    code: Option<i32>,
    /// Every line of the run's own debug log (`~/.carrick/logs`).
    log: String,
}

impl Scan {
    /// The services this scan's scope lines name as analysed.
    fn analysed(&self) -> BTreeSet<String> {
        self.scoped("analysed")
    }

    fn carried(&self) -> BTreeSet<String> {
        self.scoped("carried")
    }

    fn scoped(&self, how: &str) -> BTreeSet<String> {
        let prefix = "scope: ";
        self.output
            .lines()
            .filter_map(|line| {
                let rest = &line[line.find(prefix)? + prefix.len()..];
                let (service, verdict) = rest.split_once(' ')?;
                (verdict == how || verdict.starts_with(&format!("{how} (")))
                    .then(|| service.to_string())
            })
            .collect()
    }
}

/// Every line under `home/.carrick/logs`.
fn read_logs(home: &Path) -> String {
    let mut log = String::new();
    let mut dirs = vec![home.join(".carrick/logs")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                log.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    log
}

/// A workspace with its scan cassettes and the store a scan writes to.
struct Workspace {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    cassettes: PathBuf,
    store: PathBuf,
}

impl Workspace {
    fn scan(&self, flags: &[&str]) -> Scan {
        self.scan_into(&self.store, flags)
    }

    fn scan_into(&self, store: &Path, flags: &[&str]) -> Scan {
        self.scan_with(store, flags, &[])
    }

    fn scan_with(&self, store: &Path, flags: &[&str], env: &[(&str, &str)]) -> Scan {
        let home = self._tmp.path().join("home");
        let _ = std::fs::remove_dir_all(&home);
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
        cmd.envs(env.iter().copied());
        cmd.env("HOME", &home);
        cmd.arg(&self.repo)
            .args(["--repo", REPO])
            .args(flags)
            .env("CARRICK_MOCK_ALL", "1")
            .env(
                "CARRICK_MOCK_FIXTURE_DIR",
                format!("{}/", self.cassettes.display()),
            )
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
            "CARRICK_LOCAL_STORAGE_ISOLATE",
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
        ] {
            cmd.env_remove(var);
        }
        let result = cmd.output().expect("failed to spawn carrick");
        Scan {
            output: format!(
                "{}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            ),
            code: result.status.code(),
            log: read_logs(&home),
        }
    }

    /// Each stored blob, by service, as its raw JSON.
    fn blobs(&self) -> BTreeMap<String, serde_json::Value> {
        blobs_in(&self.store)
    }

    fn commit(&self, message: &str) -> String {
        run_git(&self.repo, &["add", "-A"]);
        run_git(&self.repo, &["commit", "-q", "-m", message]);
        run_git(&self.repo, &["rev-parse", "HEAD"])
    }

    fn edit(&self, relative: &str, contents: &str) {
        write(&self.repo, relative, contents);
    }

    fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.repo.join(relative)).unwrap()
    }
}

fn blobs_in(store: &Path) -> BTreeMap<String, serde_json::Value> {
    std::fs::read_dir(store)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension()? == "json").then_some(path)
        })
        .map(|path| {
            let blob: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            (blob["service_name"].as_str().unwrap().to_string(), blob)
        })
        .collect()
}

/// The repository and store at commit A, scanned once for every case.
fn at_a() -> &'static Path {
    static AT_A: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &AT_A
        .get_or_init(|| {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("a");
            let repo = root.join("mono");
            for (path, contents) in FILES {
                write(&repo, path, contents);
            }
            let cassettes = root.join("llm");
            for (path, contents) in CASSETTES {
                write(&cassettes, path, contents);
            }
            for stem in [
                "http-server",
                "http-client",
                "types",
                "index",
                "x",
                "job",
                "global",
            ] {
                write(
                    &cassettes,
                    &format!("analyze-file/{stem}.json"),
                    EMPTY_ANSWER,
                );
            }
            run_git(&repo, &["init", "-q", "-b", "main"]);
            let workspace = Workspace {
                _tmp: tempfile::tempdir().unwrap(),
                repo,
                cassettes,
                store: root.join("store"),
            };
            std::fs::create_dir_all(&workspace.store).unwrap();
            workspace.commit("A");
            let first = workspace.scan(&[]);
            assert_eq!(
                first.code,
                Some(0),
                "the scan at A failed:\n{}",
                first.output
            );
            assert_eq!(
                workspace.blobs().keys().cloned().collect::<Vec<_>>(),
                ["api", "web", "worker"],
                "{}",
                first.output
            );
            (tmp, root)
        })
        .1
}

/// A copy of the repository and store at A.
fn case() -> Workspace {
    let tmp = tempfile::tempdir().unwrap();
    copy_dir(at_a(), tmp.path());
    let repo = tmp.path().join("mono");
    // A copy rewrites every mtime: refresh the index so the tree reads clean.
    run_git(&repo, &["update-index", "-q", "--refresh"]);
    Workspace {
        repo,
        cassettes: tmp.path().join("llm"),
        store: tmp.path().join("store"),
        _tmp: tmp,
    }
}

fn names(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| name.to_string()).collect()
}

/// The services whose blob this scan rewrote, with the commit each names.
fn written(
    before: &BTreeMap<String, serde_json::Value>,
    after: &BTreeMap<String, serde_json::Value>,
) -> BTreeMap<String, String> {
    after
        .iter()
        .filter(|(service, blob)| before.get(*service) != Some(blob))
        .map(|(service, blob)| {
            (
                service.clone(),
                blob["commit_hash"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// One commit from A, scanned with `--changed-only`: the services analysed
/// and the blobs written.
fn changed_only_after(
    edit: impl FnOnce(&Workspace),
) -> (Workspace, Scan, BTreeMap<String, String>, String) {
    let workspace = case();
    edit(&workspace);
    let head = workspace.commit("B");
    let before = workspace.blobs();
    let scan = workspace.scan(&["--changed-only"]);
    assert_eq!(scan.code, Some(0), "{}", scan.output);
    let written = written(&before, &workspace.blobs());
    (workspace, scan, written, head)
}

fn assert_analysed(scan: &Scan, expected: &[&str]) {
    assert_eq!(scan.analysed(), names(expected), "{}", scan.output);
    let carried: BTreeSet<String> = names(&["api", "web", "worker"])
        .difference(&names(expected))
        .cloned()
        .collect();
    assert_eq!(scan.carried(), carried, "{}", scan.output);
}

fn verdicts(blob: &serde_json::Value) -> String {
    blob["compat_verdicts"].to_string()
}

/// Case 1: a change to `api`'s handler analyses `api` alone. `web` is
/// carried, and because its verdict against `api` changed, it is written at
/// the new commit with the new verdict. `worker` is not written.
#[test]
fn a_producer_change_refreshes_the_carried_consumer_verdict() {
    let (workspace, scan, written, head) = changed_only_after(|ws| {
        let server = ws
            .read("apps/api/src/server.ts")
            .replace("  x: number;", "  y: number;")
            .replace("{ x: label(1).length }", "{ y: label(1).length }");
        ws.edit("apps/api/src/server.ts", &server);
    });
    assert_analysed(&scan, &["api"]);
    assert_eq!(
        written,
        BTreeMap::from([
            ("api".to_string(), head.clone()),
            ("web".to_string(), head.clone())
        ]),
        "{}",
        scan.output
    );
    let at_a = blobs_in(&at_a().join("store"));
    let web = &workspace.blobs()["web"];
    assert!(
        verdicts(&at_a["web"]).contains("\"compatible\""),
        "at A, web's call agrees with api: {}",
        verdicts(&at_a["web"])
    );
    assert!(
        verdicts(web).contains("\"incompatible\""),
        "web's verdict is refreshed: {}",
        verdicts(web)
    );
    assert_eq!(web["boundary"]["commit_hash"], head.as_str());
    assert_eq!(web["boundary"]["files_attempted"], 0);
}

/// Case 2: a file `web` imports by a relative path across service roots.
#[test]
fn a_relative_import_across_roots_is_an_input() {
    let (_, scan, written, _) = changed_only_after(|ws| {
        ws.edit(
            "apps/api/src/types.ts",
            "export interface Order {\n  id: string;\n  total?: number;\n}\n",
        );
    });
    assert_analysed(&scan, &["api", "web"]);
    assert!(!written.contains_key("worker"), "{written:?}");
}

/// Case 3: a workspace package imported by name.
#[test]
fn a_workspace_package_is_an_input_of_its_importers() {
    let (_, scan, _, _) = changed_only_after(|ws| {
        ws.edit(
            "packages/shared/src/x.ts",
            "export function label(n: number): string {\n  return \"#\" + n;\n}\n",
        );
    });
    assert_analysed(&scan, &["api", "worker"]);
}

/// Case 4: a library reached through a tsconfig `paths` alias.
#[test]
fn an_aliased_library_is_an_input() {
    let (_, scan, _, _) = changed_only_after(|ws| {
        ws.edit(
            "libs/util/index.ts",
            "export function twice(n: number): number {\n  return n + n;\n}\n",
        );
    });
    assert_analysed(&scan, &["worker"]);
}

/// Case 5: an ambient declaration directory the tsconfig includes.
#[test]
fn a_tsconfig_include_is_an_input() {
    let (_, scan, _, _) = changed_only_after(|ws| {
        ws.edit(
            "types/global.d.ts",
            "declare const BUILD_ID: string;\ndeclare const REGION: string;\n",
        );
    });
    assert_analysed(&scan, &["web"]);
}

/// Case 6: a file added beside the directory an import resolves through
/// takes its place, so `web` is analysed (and `api`, whose root it is in).
/// Deleting it again moves the import back to the directory: the deleted
/// file is in no closure on the tree being scanned, so only the directory
/// the import is looked up in can say so.
#[test]
fn a_file_that_shadows_a_resolution_is_an_input_when_added_or_deleted() {
    let (workspace, scan, _, _) = changed_only_after(|ws| {
        ws.edit(
            "apps/api/src/model.ts",
            "export function describeOrder(order: { id: string }): string {\n  return `#${order.id}`;\n}\n",
        );
    });
    assert_analysed(&scan, &["api", "web"]);

    std::fs::remove_file(workspace.repo.join("apps/api/src/model.ts")).unwrap();
    workspace.commit("C");
    let scan = workspace.scan(&["--changed-only"]);
    assert_eq!(scan.code, Some(0), "{}", scan.output);
    assert_analysed(&scan, &["api", "web"]);
}

/// Cases 7, 8 and 9: the shared tsconfig, the lockfile and carrick.json.
#[test]
fn shared_config_lockfile_and_carrick_json_analyse_every_service() {
    for (path, append) in [
        ("tsconfig.base.json", "\n"),
        ("package-lock.json", "\n"),
        ("carrick.json", "\n"),
    ] {
        let (_, scan, _, _) = changed_only_after(|ws| {
            let contents = ws.read(path) + append;
            ws.edit(path, &contents);
        });
        assert_analysed(&scan, &["api", "web", "worker"]);
    }
}

/// Case 10: a change no service reads writes nothing and closes the scan.
#[test]
fn an_unread_change_writes_nothing_and_closes_the_scan() {
    let (_, scan, written, _) = changed_only_after(|ws| {
        ws.edit("README.md", "Changed.\n");
    });
    assert_analysed(&scan, &[]);
    assert!(written.is_empty(), "{written:?}\n{}", scan.output);
    assert_eq!(
        scan.output
            .matches("close-scan (reason: no_change)")
            .count(),
        1,
        "{}",
        scan.output
    );
}

/// Cases 11 and 12: a stored commit the clone lacks, and a stored blob from
/// another scanner version, analyse that service.
#[test]
fn a_stored_blob_that_cannot_vouch_analyses_its_service() {
    for (field, value) in [
        ("commit_hash", serde_json::json!("f".repeat(40))),
        ("scanner_version", serde_json::json!("0.0.1")),
    ] {
        let workspace = case();
        let path = std::fs::read_dir(&workspace.store)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.to_string_lossy().ends_with("worker.json"))
            .unwrap();
        let mut blob: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        blob[field] = value;
        std::fs::write(&path, serde_json::to_string_pretty(&blob).unwrap()).unwrap();
        workspace.edit("README.md", "Changed.\n");
        workspace.commit("B");
        let scan = workspace.scan(&["--changed-only"]);
        assert_eq!(scan.code, Some(0), "{}", scan.output);
        assert_analysed(&scan, &["worker"]);
    }
}

/// The cassette path the mock reads the extraction config from.
const EXTRACTION_CONFIG: &str = "framework-guidance/extraction_config.json";

/// An answer with no rule list: the extraction config fails to arrive.
const NO_RULE_LIST: &str = "{}\n";

fn blob_path(store: &Path, service: &str) -> PathBuf {
    std::fs::read_dir(store)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension().is_some_and(|ext| ext == "json")
                && serde_json::from_str::<serde_json::Value>(
                    &std::fs::read_to_string(path).unwrap(),
                )
                .is_ok_and(|blob| blob["service_name"] == service)
        })
        .unwrap()
}

/// `api`'s blob as a scan at A writes it when the extraction config does not
/// arrive: its types resolved with machinery wrappers left wrapped.
fn api_blob_without_its_config() -> &'static str {
    static BLOB: OnceLock<String> = OnceLock::new();
    BLOB.get_or_init(|| {
        let workspace = case();
        write(&workspace.cassettes, EXTRACTION_CONFIG, NO_RULE_LIST);
        let store = workspace
            .repo
            .parent()
            .unwrap()
            .join("store-without-config");
        std::fs::create_dir_all(&store).unwrap();
        let scan = workspace.scan_into(&store, &[]);
        assert_eq!(scan.code, Some(0), "{}", scan.output);
        let blob = std::fs::read_to_string(blob_path(&store, "api")).unwrap();
        assert!(
            blob.contains("machinery unwrapping disabled this run"),
            "the scan records that the config did not arrive:\n{}",
            scan.output
        );
        blob
    })
}

/// The store at A, except that `api`'s extraction config did not arrive.
fn case_where_api_lacks_its_config() -> Workspace {
    let workspace = case();
    std::fs::write(
        blob_path(&workspace.store, "api"),
        api_blob_without_its_config(),
    )
    .unwrap();
    workspace
}

fn touch_web(workspace: &Workspace) {
    let checkout = workspace.read("apps/web/src/checkout.ts");
    workspace.edit(
        "apps/web/src/checkout.ts",
        &format!("{checkout}// A change to web alone.\n"),
    );
}

/// A person reading the scan is not told why a service was analysed again
/// when the reason is a gap in its stored index; the debug log is.
fn assert_reason_only_in_the_log(scan: &Scan, service: &str) {
    assert!(
        !scan.output.contains("stored_incomplete")
            && !scan.output.contains("without its extraction config"),
        "{}",
        scan.output
    );
    assert!(
        scan.output
            .contains(&format!("scope: {service} analysed\n")),
        "the scope line stays, without its reason: {}",
        scan.output
    );
    assert!(
        scan.log.contains(&format!(
            "scope: {service} analysed (stored_incomplete: the stored index was written without its extraction config)"
        )),
        "{}",
        scan.log
    );
}

/// carrick#2301: a stored blob whose extraction config did not arrive owes
/// that work. A change to `web` alone analyses `api` too, the config is asked
/// for again, and `api`'s types are captured with it. `worker`, whose blob
/// records no such problem, is carried.
#[test]
fn a_service_stored_without_its_extraction_config_is_analysed_again() {
    let workspace = case_where_api_lacks_its_config();
    touch_web(&workspace);
    let head = workspace.commit("B");
    let scan = workspace.scan(&["--changed-only"]);
    assert_eq!(scan.code, Some(0), "{}", scan.output);
    assert_analysed(&scan, &["api", "web"]);
    assert_reason_only_in_the_log(&scan, "api");
    let blobs = workspace.blobs();
    assert_eq!(blobs["api"]["commit_hash"], head.as_str());
    assert_eq!(
        blobs["api"]["type_extraction_status"],
        serde_json::Value::Null,
        "the config arrived, so the blob records no problem: {}",
        scan.output
    );
    assert_eq!(
        blobs["api"]["boundary"]["type_extraction_status"],
        serde_json::Value::Null
    );
    assert!(
        verdicts(&blobs["web"]).contains("\"compatible\""),
        "web's verdict is read from api's types as captured this run: {}",
        verdicts(&blobs["web"])
    );
}

/// carrick#2301: when the config fails to arrive again the blob keeps saying
/// so, and the next scan asks once more. `worker` is carried throughout.
#[test]
fn an_extraction_config_that_fails_again_is_asked_for_on_the_next_scan() {
    let workspace = case_where_api_lacks_its_config();
    write(&workspace.cassettes, EXTRACTION_CONFIG, NO_RULE_LIST);
    touch_web(&workspace);
    workspace.commit("B");
    let scan = workspace.scan(&["--changed-only"]);
    assert_eq!(scan.code, Some(0), "{}", scan.output);
    assert_analysed(&scan, &["api", "web"]);
    let status = &workspace.blobs()["api"]["type_extraction_status"];
    assert!(
        status
            .as_str()
            .is_some_and(|status| status.contains("machinery unwrapping disabled this run")),
        "{status}: {}",
        scan.output
    );

    workspace.edit("README.md", "Changed.\n");
    workspace.commit("C");
    let scan = workspace.scan(&["--changed-only"]);
    assert_eq!(scan.code, Some(0), "{}", scan.output);
    // `web` was analysed at B with the config its blob kept from A, so it
    // asked for none, recorded no problem, and is carried here.
    assert_analysed(&scan, &["api"]);
    assert_reason_only_in_the_log(&scan, "api");
}

/// carrick#2301, what must not change: a status recorded beside an extraction
/// config (a capture that produced no stub) is nothing an analysis repairs,
/// so its service is carried.
#[test]
fn a_stored_status_beside_an_extraction_config_is_carried() {
    let workspace = case();
    let path = blob_path(&workspace.store, "worker");
    let mut blob: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        blob["cached_extraction_config"].is_object(),
        "worker's blob keeps the config it was analysed with"
    );
    blob["type_extraction_status"] =
        serde_json::json!("v2 type capture degraded: no stub package was produced");
    std::fs::write(&path, serde_json::to_string_pretty(&blob).unwrap()).unwrap();
    touch_web(&workspace);
    workspace.commit("B");
    let scan = workspace.scan(&["--changed-only"]);
    assert_eq!(scan.code, Some(0), "{}", scan.output);
    assert_analysed(&scan, &["web"]);
}

/// Case 13 (carrick#2826): an import nothing accounts for analyses its
/// service only when a changed path is one it could name. A run that changed
/// nothing it could name carries the service; a run that adds the file a
/// relative import was looking for analyses it.
#[test]
fn an_unresolved_import_analyses_its_service_only_when_it_could_name_a_change() {
    let workspace = case();
    workspace.edit(
        "apps/worker/src/job.ts",
        &format!(
            "import \"undeclared-module\";\nimport \"../../../build/shared/log\";\n{JOB_AT_A}"
        ),
    );
    workspace.commit("B");
    let scan = workspace.scan(&["--changed-only"]);
    assert_analysed(&scan, &["worker"]);
    workspace.edit("README.md", "Changed.\n");
    workspace.commit("C");
    let scan = workspace.scan(&["--changed-only"]);
    assert_analysed(&scan, &[]);
    workspace.edit("build/shared/log.ts", "export const log = 1;\n");
    workspace.commit("D");
    let scan = workspace.scan(&["--changed-only"]);
    assert_analysed(&scan, &["worker"]);
}

/// carrick#2826 review: a workspace package whose entry is not built (its
/// `main` names a `dist/` file nobody generated) is an import the resolver
/// cannot follow, and a change anywhere in that package still analyses every
/// service importing it: the package's whole directory is an input of its
/// importers, whatever its entry resolves to.
#[test]
fn an_unbuilt_workspace_package_is_still_an_input_of_its_importers() {
    let workspace = case();
    // The entry names a build output that is not there, and no source sits
    // where a resolver would fall back to: `@fixture/shared` resolves to
    // nothing on this tree.
    workspace.edit(
        "packages/shared/package.json",
        "{ \"name\": \"@fixture/shared\", \"version\": \"0.0.0\", \"main\": \"dist/index.js\" }\n",
    );
    for file in ["packages/shared/src/index.ts", "packages/shared/src/x.ts"] {
        std::fs::remove_file(workspace.repo.join(file)).expect("remove a source file");
    }
    workspace.edit(
        "packages/shared/lib/x.ts",
        "export function label(n: number): string {\n  return \"n\" + n;\n}\n",
    );
    workspace.commit("B");
    let scan = workspace.scan(&["--changed-only"]);
    assert_eq!(scan.code, Some(0), "{}", scan.output);
    workspace.edit(
        "packages/shared/lib/x.ts",
        "export function label(n: number): string {\n  return \"#\" + n;\n}\n",
    );
    workspace.commit("C");
    let scan = workspace.scan(&["--changed-only"]);
    assert_analysed(&scan, &["api", "worker"]);
}

/// The services `--changed-only` analyses write the bytes a scan without it
/// writes for them.
#[test]
fn an_analysed_service_writes_what_a_full_scan_writes() {
    let workspace = case();
    workspace.edit(
        "apps/api/src/types.ts",
        "export interface Order {\n  id: string;\n  total?: number;\n}\n",
    );
    workspace.commit("B");
    let full_store = workspace.repo.parent().unwrap().join("full-store");
    copy_dir(&workspace.store, &full_store);
    let scoped = workspace.scan(&["--changed-only"]);
    assert_eq!(scoped.code, Some(0), "{}", scoped.output);
    let full = workspace.scan_into(&full_store, &[]);
    assert_eq!(full.code, Some(0), "{}", full.output);
    let pinned = |mut blob: serde_json::Value| {
        blob["last_updated"] = serde_json::Value::Null;
        serde_json::to_string_pretty(&blob).unwrap()
    };
    let scoped = workspace.blobs();
    let full = blobs_in(&full_store);
    for service in ["api", "web"] {
        assert_eq!(
            pinned(scoped[service].clone()),
            pinned(full[service].clone()),
            "{service}"
        );
    }
}

/// carrick#1066 with carrick#2227: services analysed side by side carry the
/// rest exactly as one at a time does. The carried service is never handed
/// to a worker (its stored blob is untouched and no worker names it), and
/// the two analysed beside each other write the bytes they write one at a
/// time.
#[test]
fn analysed_side_by_side_a_carried_service_is_never_analysed() {
    let workspace = case();
    workspace.edit(
        "apps/api/src/types.ts",
        "export interface Order {\n  id: string;\n  total?: number;\n}\n",
    );
    workspace.commit("B");
    let before = workspace.blobs();
    let one_at_a_time_store = workspace.repo.parent().unwrap().join("one-at-a-time");
    copy_dir(&workspace.store, &one_at_a_time_store);
    let side_by_side = workspace.scan_with(
        &workspace.store,
        &["--changed-only"],
        &[("CARRICK_SERVICE_CONCURRENCY", "4"), ("RUST_LOG", "info")],
    );
    assert_eq!(side_by_side.code, Some(0), "{}", side_by_side.output);
    assert_analysed(&side_by_side, &["api", "web"]);
    let analysing: Vec<&str> = side_by_side
        .output
        .lines()
        .filter(|line| line.contains("Analyzing service "))
        .collect();
    assert_eq!(analysing.len(), 2, "{}", side_by_side.output);
    assert!(
        analysing.iter().all(|line| !line.contains("worker")),
        "{}",
        side_by_side.output
    );
    // The speed lines a runner log is read for: the width and why, the walk
    // lane, and each analysed service's sidecar peak.
    let said = |needle: &str| {
        side_by_side
            .output
            .lines()
            .filter(|line| line.contains(needle))
            .count()
    };
    assert_eq!(
        said("Analysing services 2 at a time: "),
        1,
        "{}",
        side_by_side.output
    );
    assert_eq!(said("Walk lane off: "), 1, "{}", side_by_side.output);
    assert_eq!(said("sidecar peak "), 2, "{}", side_by_side.output);
    let one_at_a_time = workspace.scan_with(
        &one_at_a_time_store,
        &["--changed-only"],
        &[("CARRICK_SERVICE_CONCURRENCY", "1")],
    );
    assert_eq!(one_at_a_time.code, Some(0), "{}", one_at_a_time.output);
    let side_by_side = workspace.blobs();
    assert_eq!(side_by_side["worker"], before["worker"]);
    let pinned = |mut blob: serde_json::Value| {
        blob["last_updated"] = serde_json::Value::Null;
        serde_json::to_string_pretty(&blob).unwrap()
    };
    let one_at_a_time = blobs_in(&one_at_a_time_store);
    for service in ["api", "web", "worker"] {
        assert_eq!(
            pinned(side_by_side[service].clone()),
            pinned(one_at_a_time[service].clone()),
            "{service}"
        );
    }
}

/// Without the flag, a change no service reads analyses every service, as
/// every scan before carrick#2227 did.
#[test]
fn without_the_flag_every_service_is_analysed() {
    let workspace = case();
    workspace.edit("README.md", "Changed.\n");
    let head = workspace.commit("B");
    let scan = workspace.scan(&[]);
    assert_eq!(scan.code, Some(0), "{}", scan.output);
    assert!(scan.analysed().is_empty() && scan.carried().is_empty());
    for (service, blob) in workspace.blobs() {
        assert_eq!(blob["commit_hash"], head.as_str(), "{service}");
    }
}

/// `--changed-only` beside `--no-cache` is a usage error.
#[test]
fn changed_only_with_no_cache_is_a_usage_error() {
    let workspace = case();
    let scan = workspace.scan(&["--changed-only", "--no-cache"]);
    assert_eq!(scan.code, Some(2), "{}", scan.output);
    assert!(
        scan.output.contains(
            "carrick: --changed-only cannot be combined with --no-cache, which re-analyses every service."
        ),
        "{}",
        scan.output
    );
}
