//! carrick#2613: the fact layer's files are worked out before the file
//! reader dispatches, from the library store's first answer. Without
//! `CARRICK_FACT_FILES` they are only recorded in the run log (ticket T1);
//! with it, they are the files the reader is sent, each with the fact
//! layer's calls listed (ticket T3).
//!
//! The fixture is the library-store one, scanned as the Action scans it
//! (`CARRICK_POSITION_ROWS=1`, `CARRICK_SITE_QUESTIONS=1`, the real sidecar,
//! the store's answer from the cassette), with files added here:
//! - `src/notify.ts`, whose only network call is `relay(url)` on a labelled
//!   default export. It is not awaited, takes no literal argument and names
//!   nothing a signal looks for, so no hand-written signal raises it and
//!   today the reader never sees the file. Multi-byte characters sit before
//!   the call, so the span discovery reports is checked against the bytes;
//! - `src/slow.ts`, whose only call goes through a package the store has not
//!   answered yet;
//! - `src/serve.ts`, a route registered through a labelled server-framework
//!   export that no signal raises;
//! - `src/hook.ts`, an awaited call with a string argument (register signal
//!   C2) on a function a factory returned, a call discovery cannot follow;
//! - `src/twice.ts`, the same signal on a local helper whose walk finishes
//!   with no network call;
//! - `src/nested.ts`, the same signal on a local helper that calls a function
//!   a factory returned: the walk finds no site, but did not read everything;
//! - `src/traced.ts`, the same signal on a local helper that hands a
//!   package's function a callback that calls the labelled relay;
//! - `src/helper.ts`, an in-module request helper that builds its URL with a
//!   call and passes it to the labelled relay, called un-awaited from a
//!   client object of arrow methods, beside a helper that never sends.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), &to).unwrap();
        }
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .status()
        .expect("git failed to spawn");
    assert!(status.success(), "git {args:?} failed");
}

/// A package installed in the fixture's `node_modules`, locked to the public
/// registry, declared by `declarations`.
fn add_package(repo: &Path, name: &str, version: &str, declarations: &str) {
    let dir = repo.join("node_modules").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("package.json"),
        format!(r#"{{ "name": "{name}", "version": "{version}", "types": "index.d.ts" }}"#),
    )
    .unwrap();
    fs::write(dir.join("index.d.ts"), declarations).unwrap();
    let manifest = repo.join("package.json");
    let mut package: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&manifest).unwrap()).unwrap();
    package["dependencies"][name] = format!("^{version}").into();
    fs::write(&manifest, package.to_string()).unwrap();
    let lock = repo.join("package-lock.json");
    let mut locked: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&lock).unwrap()).unwrap();
    locked["packages"][format!("node_modules/{name}")] = serde_json::json!({
        "version": version,
        "resolved": format!("https://registry.npmjs.org/{name}/-/x-{version}.tgz"),
        "integrity": "sha512-fixture",
    });
    fs::write(&lock, locked.to_string()).unwrap();
}

/// A default export that takes one string.
const DEFAULT_SEND: &str = "declare function send(target: string): void;\nexport default send;\n";

const NOTIFY: &str = "import relay from \"@fixture/relay\"; // café, naïve, ✓\n\nexport function ping(url: string): void {\n  relay(url);\n}\n";

const SERVE: &str = "import { answer } from \"@fixture/serve\";\n\nfunction pong(): string {\n  return \"pong\";\n}\n\nanswer(\"/ping\", pong);\n";

const HOOK: &str = "function makeSender(): (path: string) => Promise<void> {\n  return async (path) => {\n    void path;\n  };\n}\n\nconst post = makeSender();\n\nexport async function hook(): Promise<void> {\n  await post(\"/hooks\");\n}\n";

const MEMBERS: &str = "export class Pusher {\n  constructor(private readonly base: string) {}\n\n  push(item: string): Promise<Response> {\n    return fetch(`${this.base}/api/v2/items/${item}`, { method: \"PUT\" });\n  }\n}\n";

const MEMBER_CALLER: &str = "import { Pusher } from \"./members\";\n\ninterface Sender {\n  push(item: string): Promise<Response>;\n}\n\nconst client: Sender = new Pusher(\"http://items.internal\");\n\nexport async function run(): Promise<void> {\n  await client.push(\"a\");\n}\n";

const TWICE: &str = "async function double(text: string): Promise<string> {\n  return text + text;\n}\n\nexport async function twice(): Promise<string> {\n  return await double(\"/twice\");\n}\n";

const NESTED: &str = "function makeSender(): (path: string) => Promise<void> {\n  return async (path) => {\n    void path;\n  };\n}\n\nconst deliver = makeSender();\n\nasync function send(path: string): Promise<void> {\n  await deliver(path);\n}\n\nexport async function nested(): Promise<void> {\n  await send(\"/nested\");\n}\n";

const TRACED: &str = "import relay from \"@fixture/relay\";\nimport span from \"@fixture/span\";\n\nasync function traced(url: string): Promise<void> {\n  span(() => relay(url));\n}\n\nexport async function go(): Promise<void> {\n  await traced(\"/traced\");\n}\n";

const REGISTERS: &str = "import server from \"@fixture/server\";\n\nfunction health(): string {\n  return \"ok\";\n}\n\nexport function createServer() {\n  const srv = server();\n  srv.get(\"/health\", () => health());\n  return srv;\n}\n";

const REGISTERS_CALLER: &str = "import { createServer } from \"./registers\";\n\nexport function start() {\n  return createServer();\n}\n";

const REGISTERS_NAMED: &str = "import server from \"@fixture/server\";\n\nfunction health(): string {\n  return \"ok\";\n}\n\nexport function createNamedServer() {\n  const srv = server();\n  srv.get(\"/health/named\", health);\n  return srv;\n}\n";

const REGISTERS_NAMED_CALLER: &str = "import { createNamedServer } from \"./registers_named\";\n\nexport function startNamed() {\n  return createNamedServer();\n}\n";

const STATUS: &str = "import send from \"@fixture/http\";\n\nexport function readStatus(): Promise<unknown> {\n  return send(\"/api/v1/status\", { method: \"GET\" });\n}\n";

const POLL: &str = "import { readStatus } from \"./status\";\n\nexport function poll(): void {\n  void readStatus();\n}\n";

const HELPER: &str = "import relay from \"@fixture/relay\";\n\nfunction withPath(path: string): string {\n  return `/v2${path}`;\n}\n\nfunction request(path: string): void {\n  relay(withPath(path));\n}\n\nfunction shout(path: string): string {\n  return path.toUpperCase();\n}\n\nexport const items = {\n  list: () => request(\"/items\"),\n  title: () => shout(\"/items\"),\n};\n";

const UI: &str = "import { useQuery } from \"@fixture/gql\";\n\nconst ItemsDocument = \"query Items { items { id } }\";\n\nexport async function load(): Promise<unknown> {\n  useQuery(ItemsDocument, {});\n  return fetch(\"/api/items\");\n}\n";

const MIXED_LOAD: &str = "import { useQuery } from \"@fixture/gql\";\nimport send from \"@fixture/http\";\n\nconst ItemsDocument = \"query Items { items { id } }\";\n\nexport async function loadBoth(path: string): Promise<unknown> {\n  useQuery(ItemsDocument, {});\n  return send(path, { method: \"GET\" });\n}\n";

const MIXED_PAGE: &str = "import { loadBoth } from \"./mixed_load\";\n\nexport async function mixedPage(path: string): Promise<unknown> {\n  return loadBoth(path);\n}\n";

/// An answer the reader gives for one file, keyed by its stem.
fn canned_answer(repo: &Path, stem: &str, answer: serde_json::Value) {
    let dir = repo.join("__llm__/analyze-file");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(format!("{stem}.json")), answer.to_string()).unwrap();
}

/// The fixture, committed, with `relay` and `answer` labelled by the store
/// and `slow` left pending.
fn repo(tmp: &Path) -> PathBuf {
    let repo = tmp.join("relay-service");
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library-store"),
        &repo,
    );
    fs::remove_file(repo.join("README.md")).ok();
    add_package(&repo, "@fixture/relay", "1.0.0", DEFAULT_SEND);
    add_package(&repo, "@fixture/slow", "1.0.0", DEFAULT_SEND);
    add_package(
        &repo,
        "@fixture/serve",
        "1.0.0",
        "export declare function answer(path: string, handler: () => string): void;\n",
    );
    fs::write(repo.join("src/notify.ts"), NOTIFY).unwrap();
    fs::write(
        repo.join("src/slow.ts"),
        "import slow from \"@fixture/slow\";\n\nexport function later(target: string): void {\n  slow(target);\n}\n",
    )
    .unwrap();
    fs::write(repo.join("src/serve.ts"), SERVE).unwrap();
    fs::write(repo.join("src/hook.ts"), HOOK).unwrap();
    fs::write(repo.join("src/members.ts"), MEMBERS).unwrap();
    fs::write(repo.join("src/member_caller.ts"), MEMBER_CALLER).unwrap();
    fs::write(repo.join("src/twice.ts"), TWICE).unwrap();
    fs::write(repo.join("src/nested.ts"), NESTED).unwrap();
    add_package(
        &repo,
        "@fixture/span",
        "1.0.0",
        "declare function span<T>(work: () => T): T;\nexport default span;\n",
    );
    fs::write(repo.join("src/traced.ts"), TRACED).unwrap();
    add_package(
        &repo,
        "@fixture/server",
        "1.0.0",
        "export interface App {\n  get(path: string, handler: () => string): void;\n}\ndeclare function server(): App;\nexport default server;\n",
    );
    add_package(
        &repo,
        "@fixture/http",
        "1.0.0",
        "declare function send(target: string, init?: { method?: string }): Promise<unknown>;\nexport default send;\n",
    );
    add_package(
        &repo,
        "@fixture/gql",
        "1.0.0",
        "export declare function useQuery(document: string, variables: object): unknown;\n",
    );
    fs::write(repo.join("src/ui.ts"), UI).unwrap();
    fs::write(repo.join("src/mixed_load.ts"), MIXED_LOAD).unwrap();
    fs::write(repo.join("src/mixed_page.ts"), MIXED_PAGE).unwrap();
    // The call into `loadBoth` reaches a GraphQL client and an HTTP client at
    // once; the reader's row there is a real request.
    canned_answer(
        &repo,
        "mixed_page",
        serde_json::json!({
            "mounts": [], "endpoints": [],
            "data_calls": [{
                "candidate_id": "@line:4", "line_number": 4, "target": "/api/mixed",
                "method": "GET", "pattern_matched": "loadBoth(",
                "call_expression_text": null, "call_expression_line": null,
                "payload_expression_text": null, "payload_expression_line": null,
                "primary_type_symbol": null, "type_import_source": null
            }]
        }),
    );
    // The reader answers a `POST /graphql` row for the hook call, which no
    // signal lists, under the span id discovery gave the site (a `@line:N`
    // id is rewritten only for a listed candidate).
    canned_answer(
        &repo,
        "ui",
        serde_json::json!({
            "mounts": [], "endpoints": [],
            "data_calls": [{
                "candidate_id": span_id(UI, "useQuery(ItemsDocument, {})"),
                "line_number": 6, "target": "/graphql",
                "method": "POST", "pattern_matched": "useQuery(",
                "call_expression_text": null, "call_expression_line": null,
                "payload_expression_text": null, "payload_expression_line": null,
                "primary_type_symbol": null, "type_import_source": null
            }, {
                "candidate_id": "@line:7", "line_number": 7, "target": "/api/items",
                "method": "GET", "pattern_matched": "fetch(",
                "call_expression_text": null, "call_expression_line": null,
                "payload_expression_text": null, "payload_expression_line": null,
                "primary_type_symbol": null, "type_import_source": null
            }]
        }),
    );
    fs::write(repo.join("src/registers.ts"), REGISTERS).unwrap();
    fs::write(repo.join("src/registers_caller.ts"), REGISTERS_CALLER).unwrap();
    fs::write(repo.join("src/registers_named.ts"), REGISTERS_NAMED).unwrap();
    fs::write(
        repo.join("src/registers_named_caller.ts"),
        REGISTERS_NAMED_CALLER,
    )
    .unwrap();
    fs::write(repo.join("src/status.ts"), STATUS).unwrap();
    fs::write(repo.join("src/poll.ts"), POLL).unwrap();
    fs::write(repo.join("src/helper.ts"), HELPER).unwrap();
    canned_answer(
        &repo,
        "notify",
        serde_json::json!({
            "mounts": [], "endpoints": [],
            "data_calls": [{
                "candidate_id": "@line:4", "line_number": 4, "target": "/relayed",
                "method": "POST", "pattern_matched": "relay(",
                "call_expression_text": null, "call_expression_line": null,
                "payload_expression_text": null, "payload_expression_line": null,
                "primary_type_symbol": null, "type_import_source": null
            }]
        }),
    );
    canned_answer(
        &repo,
        "serve",
        serde_json::json!({
            "mounts": [], "data_calls": [],
            "endpoints": [{
                "candidate_id": "@line:7", "line_number": 7, "owner_node": "answer",
                "method": "GET", "path": "/ping", "handler_name": "pong",
                "pattern_matched": "answer(",
                "payload_expression_text": null, "payload_expression_line": null,
                "response_expression_text": null, "response_expression_line": null,
                "primary_type_symbol": null, "type_import_source": null
            }]
        }),
    );
    let cassette = repo.join("__llm__/library-claims/default.json");
    let mut answer: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&cassette).unwrap()).unwrap();
    let claims = answer["library_claims"].as_array_mut().unwrap();
    claims.push(serde_json::json!({
        "package": "@fixture/relay", "version": "1.0.0", "status": "answered",
        "surface_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "exports": [{
            "specifier": "@fixture/relay", "export": "default", "role": "http_client",
            "side": null, "classifier": "fixture-classifier", "makes": [], "scopes": [],
            "ops": [], "reserved": [], "calls": [], "patterns": []
        }]
    }));
    claims.push(serde_json::json!({
        "package": "@fixture/serve", "version": "1.0.0", "status": "answered",
        "surface_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "exports": [{
            "specifier": "@fixture/serve", "export": "answer", "role": "server_framework",
            "side": "server", "classifier": "fixture-classifier", "makes": [], "scopes": [],
            "ops": [], "reserved": [], "calls": [], "patterns": []
        }]
    }));
    claims.push(serde_json::json!({
        "package": "@fixture/span", "version": "1.0.0", "status": "answered",
        "surface_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "exports": [{
            "specifier": "@fixture/span", "export": "default", "role": "none",
            "side": null, "classifier": "fixture-classifier", "makes": [], "scopes": [],
            "ops": [], "reserved": [], "calls": [], "patterns": []
        }]
    }));
    for (package, role, side) in [
        ("@fixture/server", "server_framework", Some("server")),
        ("@fixture/http", "http_client", None),
    ] {
        claims.push(serde_json::json!({
            "package": package, "version": "1.0.0", "status": "answered",
            "surface_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "exports": [{
                "specifier": package, "export": "default", "role": role,
                "side": side, "classifier": "fixture-classifier", "makes": [], "scopes": [],
                "ops": [], "reserved": [], "calls": [], "patterns": []
            }]
        }));
    }
    claims.push(serde_json::json!({
        "package": "@fixture/gql", "version": "1.0.0", "status": "answered",
        "surface_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "exports": [{
            "specifier": "@fixture/gql", "export": "useQuery", "role": "graphql_client",
            "side": null, "classifier": "fixture-classifier", "makes": [], "scopes": [],
            "ops": [], "reserved": [], "calls": [], "patterns": []
        }]
    }));
    claims.push(serde_json::json!({
        "package": "@fixture/slow", "version": "1.0.0", "status": "pending",
        "reason": "lister_unavailable"
    }));
    fs::write(&cassette, answer.to_string()).unwrap();
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

/// One scan's run log, its stderr and the user message of every prompt the
/// reader was sent, by the file each one is for.
struct Scan {
    log: String,
    stderr: String,
    prompts: BTreeMap<String, String>,
    /// The scan's consumer rows (`CARRICK_OUTPUT_JSON`).
    calls: Vec<serde_json::Value>,
}

impl Scan {
    /// The prompt the reader was sent for `file`, if any.
    fn prompt(&self, file: &str) -> Option<&str> {
        self.prompts
            .iter()
            .find(|(path, _)| path.ends_with(file))
            .map(|(_, prompt)| prompt.as_str())
    }

    /// The ids the reader's prompt for `file` lists.
    fn listed(&self, file: &str) -> Vec<String> {
        self.prompt(file)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim_start().strip_prefix("- Candidate "))
            .filter_map(|rest| rest.split_once(": Line ").map(|(id, _)| id.to_string()))
            .collect()
    }

    fn log_has(&self, needles: &[&str]) -> bool {
        self.log
            .lines()
            .any(|line| needles.iter().all(|needle| line.contains(needle)))
    }

    /// The service's fallbacks line, as the parent reading the scan gets it.
    fn fallbacks(&self) -> serde_json::Value {
        let line = self
            .stderr
            .lines()
            .find_map(|line| line.trim_start().strip_prefix("@carrick-fallbacks "))
            .expect("the scan states its fallbacks");
        serde_json::from_str(line).unwrap()
    }
}

/// A scan with the site questions on or off, and the fact layer choosing the
/// reader's files or not.
fn run_scan(site_questions: bool, fact_files: bool) -> Scan {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let storage = tmp.path().join("storage");
    let home = tmp.path().join("home");
    let dump = tmp.path().join("dump");
    fs::create_dir_all(&storage).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&dump).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(&repo)
        .env("HOME", &home)
        .env("CARRICK_LOCAL_STORAGE_DIR", &storage)
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", tmp.path().join("cache"))
        .env("CARRICK_EVAL_DUMP_DIR", &dump)
        .env("CARRICK_PROGRESS", "1")
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env(
            "CARRICK_SIDECAR_DIR",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar"),
        )
        .env("CARRICK_OUTPUT_JSON", "1")
        .env("CARRICK_POSITION_ROWS", "1");
    if site_questions {
        cmd.env("CARRICK_SITE_QUESTIONS", "1");
    } else {
        cmd.env_remove("CARRICK_SITE_QUESTIONS");
    }
    if fact_files {
        cmd.env("CARRICK_FACT_FILES", "1");
    } else {
        cmd.env_remove("CARRICK_FACT_FILES");
    }
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        "CARRICK_NO_MODEL",
        "CARRICK_ALLOW_MISSING_TYPES",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("spawn carrick");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "scan exited non-zero:\n{stderr}");
    let log = fs::read_dir(home.join(".carrick/logs/runs"))
        .expect("the scan keeps a run log")
        .filter_map(|entry| fs::read_to_string(entry.ok()?.path()).ok())
        .collect::<Vec<_>>()
        .join("\n");
    let marker = "### FILE CONTENT (Path: ";
    let prompts = fs::read_dir(&dump)
        .expect("the dump dir is readable")
        .filter_map(|entry| {
            let dumped: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(entry.ok()?.path()).ok()?).ok()?;
            let message = dumped["request_user_message"].as_str()?.to_string();
            let start = message.find(marker)? + marker.len();
            let path = message[start..].split(')').next()?.to_string();
            Some((path, message))
        })
        .collect();
    let projection: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("the scan prints its projection");
    let calls = projection["calls"].as_array().cloned().unwrap_or_default();
    Scan {
        log,
        stderr,
        prompts,
        calls,
    }
}

/// The scan with the fact layer recording its files only.
fn recording() -> &'static Scan {
    static SCAN: OnceLock<Scan> = OnceLock::new();
    SCAN.get_or_init(|| run_scan(true, false))
}

/// The scan with the fact layer choosing the reader's files.
fn choosing() -> &'static Scan {
    static SCAN: OnceLock<Scan> = OnceLock::new();
    SCAN.get_or_init(|| run_scan(true, true))
}

fn fact_lines(log: &str) -> Vec<&str> {
    log.lines()
        .filter(|line| line.contains("fact files:"))
        .collect()
}

/// `span:<lo>-<hi>` of the first `call` in `source`, in SWC's numbering
/// (byte offsets from 1).
fn span_id(source: &str, call: &str) -> String {
    let start = source.find(call).expect("the call is in the source") as u32 + 1;
    format!("span:{}-{}", start, start + call.len() as u32)
}

#[test]
fn files_only_the_fact_layer_finds_are_logged_as_fact_only() {
    let scan = recording();
    let lines = fact_lines(&scan.log);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("fact-only src/notify.ts (site true")),
        "the file whose only call is a labelled relay is fact-only: {lines:#?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("fact-only src/slow.ts (site false, lead false, unanswered-package true)")
        }),
        "a call into a package the store has not answered selects its file: {lines:#?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.contains("fact files: ")
                && line.contains("read today")
                && line.contains("fact-only")
        }),
        "the per-service summary line: {lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("packages unanswered at dispatch 1")),
        "{lines:#?}"
    );
    for signal in ["C2", "C3"] {
        assert!(
            lines
                .iter()
                .any(|line| line.contains(&format!("{signal} candidates: at a site"))),
            "{signal} is counted: {lines:#?}"
        );
    }
    // Record-only: the reader was sent neither file.
    for file in ["src/notify.ts", "src/slow.ts"] {
        assert!(
            !scan.log.lines().any(|line| {
                line.contains(file)
                    && (line.contains("Analyzing:") || line.contains("Force-analyzing"))
            }),
            "the reader's file set did not move for {file}"
        );
    }
    assert!(
        scan.log_has(&["Skipped (no API patterns)", "src/notify.ts"]),
        "today's gate still skips the file"
    );
}

#[test]
fn with_the_site_questions_off_nothing_is_recorded_and_the_scan_runs() {
    let scan = run_scan(false, false);
    assert!(
        fact_lines(&scan.log).is_empty(),
        "no fact-layer record without the site questions: {:#?}",
        fact_lines(&scan.log)
    );
}

/// With the switch on and no fact files to choose by (the site questions
/// off), selection fails closed: every candidate stays, and every file a
/// candidate selects is read and says so.
#[test]
fn with_the_switch_on_and_no_fact_files_every_candidate_stays() {
    let scan = run_scan(false, true);
    for file in ["src/hook.ts", "src/twice.ts", "src/nested.ts"] {
        assert!(scan.prompt(file).is_some(), "{file} is read");
        assert!(
            scan.log_has(&["Analyzing: ", file]),
            "{file} is logged as read"
        );
    }
    assert!(!scan.log_has(&["Fact-layer file"]));
}

/// T3 test 2, with the span check: the fact layer sends the reader the file
/// no signal raised a candidate in, with its call listed at the span the
/// bytes give, past multi-byte characters, and the reader's row there joins.
#[test]
fn a_fact_file_is_read_with_its_call_listed_and_the_readers_row_joins() {
    let scan = choosing();
    assert!(
        scan.log_has(&[
            "Fact-layer file (no HTTP candidates):",
            "src/notify.ts",
            "[1 site(s)]"
        ]),
        "{}",
        scan.log
    );
    assert_eq!(
        scan.listed("src/notify.ts"),
        vec![span_id(NOTIFY, "relay(url)")],
        "listed at the call's own bytes"
    );
    assert!(
        !scan.log_has(&["had no matching SWC candidate", "notify.ts"]),
        "the reader's row joined its listed call"
    );
    assert!(
        recording().prompt("src/notify.ts").is_none(),
        "without the switch the file is not read"
    );
}

/// T3 test 3: a call into a package the store has not answered still sends
/// its file to the reader, so selection fails closed.
#[test]
fn a_call_into_an_unanswered_package_sends_its_file() {
    let scan = choosing();
    assert!(
        scan.log_has(&["Fact-layer file (no HTTP candidates):", "src/slow.ts"]),
        "{}",
        scan.log
    );
    assert!(scan.prompt("src/slow.ts").is_some());
}

/// T3 test 4: the reader's route at a fact site no signal raised is kept:
/// the site is listed, so its id names a listed call (carrick#2382).
#[test]
fn a_readers_route_at_a_fact_site_survives_the_unlisted_id_drop() {
    let scan = choosing();
    assert_eq!(
        scan.listed("src/serve.ts"),
        vec![span_id(SERVE, "answer(\"/ping\", pong)")]
    );
    assert!(
        scan.log_has(&["Model endpoints dropped, their id names no listed site: 0"]),
        "{}",
        scan.log
    );
    assert!(recording().prompt("src/serve.ts").is_none());
}

/// T3 test 5 (D4): a register-signal candidate at a call discovery cannot
/// follow is kept under its id and selects its file, and is counted as the
/// old path's selection fallback; one at a call whose walk finished is
/// dropped, and its file is no longer read.
#[test]
fn a_register_candidate_stays_only_where_discovery_cannot_follow_its_call() {
    let (off, on) = (recording(), choosing());
    let hook = span_id(HOOK, "post(\"/hooks\")");
    assert_eq!(off.listed("src/hook.ts"), vec![hook.clone()]);
    assert_eq!(on.listed("src/hook.ts"), vec![hook], "kept under its id");
    assert_eq!(
        off.listed("src/twice.ts"),
        vec![span_id(TWICE, "double(\"/twice\")")]
    );
    assert!(
        on.prompt("src/twice.ts").is_none(),
        "dropped at a finished walk"
    );
    assert!(on.log_has(&["Skipped (no API patterns)", "src/twice.ts"]));
    // A walk that found no site but passed a call it could not follow on the
    // way has not read everything: the candidate stays.
    let nested = span_id(NESTED, "send(\"/nested\")");
    assert_eq!(off.listed("src/nested.ts"), vec![nested.clone()]);
    assert_eq!(on.listed("src/nested.ts"), vec![nested]);
    // So has one that hands a package's function a callback that reaches a
    // labelled export: the walk never entered it from this call.
    let traced = span_id(TRACED, "traced(\"/traced\")");
    assert!(off.listed("src/traced.ts").contains(&traced));
    assert!(
        on.listed("src/traced.ts").contains(&traced),
        "{:?}",
        on.listed("src/traced.ts")
    );
    let fallbacks = on.fallbacks();
    assert!(
        fallbacks["fallback_selection_C2"].as_u64().unwrap_or(0) >= 1,
        "{fallbacks}"
    );
    assert!(
        off.fallbacks().get("fallback_selection_C2").is_none(),
        "without the switch the line is as it was"
    );
}

/// F0 (carrick#2440): a candidate kept at a call discovery cannot follow
/// feeds the imported-member join as it does with the switch off. The call
/// `client.push("a")` goes through an interface, so discovery cannot follow
/// it and the register candidate is kept; the member another module declares
/// states the verb and the path.
#[test]
fn a_kept_candidate_reaches_the_imported_member_join() {
    let (off, on) = (recording(), choosing());
    for (scan, name) in [(off, "off"), (on, "on")] {
        let row = scan
            .calls
            .iter()
            .find(|call| {
                call["file"]
                    .as_str()
                    .is_some_and(|f| f.ends_with("member_caller.ts"))
                    && call["line"].as_i64() == Some(10)
            })
            .unwrap_or_else(|| panic!("{name}: no row at the call: {:#?}", scan.calls));
        assert_eq!(row["method"], "PUT", "{name}: {row}");
        assert_eq!(row["path"], "/api/v2/items/:item", "{name}: {row}");
    }
}

/// carrick#2745: a route registration has a request's shape
/// (`srv.get("/health", handler)`); the store's role for the export tells
/// them apart. The callee registers a route, so no call row is stated at its
/// caller; a callee that sends a request keeps its row.
#[test]
fn a_callee_that_registers_a_route_states_no_call_at_its_caller() {
    let on = choosing();
    let at = |scan: &Scan, file: &str, line: i64| -> Vec<serde_json::Value> {
        scan.calls
            .iter()
            .filter(|call| {
                call["file"].as_str().is_some_and(|f| f.ends_with(file))
                    && call["line"].as_i64() == Some(line)
            })
            .cloned()
            .collect()
    };
    let registered = at(on, "registers_caller.ts", 4);
    assert!(
        registered.is_empty(),
        "a route registration is not a request: {registered:#?}"
    );
    let polled = at(on, "poll.ts", 4);
    assert_eq!(
        polled.len(),
        1,
        "the control keeps its row: {:#?}",
        on.calls
    );
    assert_eq!(polled[0]["method"], "GET", "{}", polled[0]);
    assert_eq!(polled[0]["path"], "/api/v1/status", "{}", polled[0]);
    assert!(
        on.log_has(&["is a server-framework call", "registers.ts:9"]),
        "the drop is logged: {}",
        on.log
    );
    assert!(
        !recording().log_has(&["is a server-framework call"]),
        "the off arm does not run the filter"
    );
}

/// A registration whose handler is a named function has a request's shape
/// too (the summaries read `srv.get("/x", handler)` as `GET /x`). With the
/// fact layer choosing, the store's role for the export withdraws the row at
/// every caller; the off arm keeps it.
#[test]
fn a_callee_that_registers_a_named_handler_states_no_call_at_its_caller() {
    let at = |scan: &Scan| -> Vec<serde_json::Value> {
        scan.calls
            .iter()
            .filter(|call| {
                call["file"]
                    .as_str()
                    .is_some_and(|f| f.ends_with("registers_named_caller.ts"))
                    && call["line"].as_i64() == Some(4)
            })
            .cloned()
            .collect()
    };
    let on = at(choosing());
    assert!(
        on.is_empty(),
        "a route registration is not a request: {on:#?}"
    );
    assert!(
        choosing().log_has(&["the store labels a server framework"]),
        "the drop is logged: {}",
        choosing().log
    );
    let off = at(recording());
    assert_eq!(off.len(), 1, "the off arm is unchanged: {off:#?}");
    assert_eq!(off[0]["path"], "/health/named", "{}", off[0]);
}

/// The fact layer lists a call into an in-module request helper that reaches
/// a labelled export, awaited or not, and never one into a helper that sends
/// nothing. With the switch off the file is not read: no signal raises it.
#[test]
fn an_unawaited_call_into_a_request_helper_is_a_fact_site() {
    let on = choosing();
    assert_eq!(
        on.listed("src/helper.ts"),
        vec![
            span_id(HELPER, "relay(withPath(path))"),
            span_id(HELPER, "request(\"/items\")")
        ],
        "the helper's own send and the call into it, never the call into `shout`: {}",
        on.log
    );
    assert!(
        recording().prompt("src/helper.ts").is_none(),
        "without the switch the file is not read"
    );
}

/// carrick#2763: a call into an export the store labels a GraphQL client
/// executes an operation. No HTTP candidate is listed there, so the reader's
/// row for it joins none and has no span; it is withdrawn by the span its id
/// names. The fetch beside it keeps its row.
#[test]
fn a_row_at_a_call_into_a_graphql_client_is_withdrawn() {
    let on = choosing();
    let at = |scan: &Scan, line: i64| -> Vec<serde_json::Value> {
        scan.calls
            .iter()
            .filter(|call| {
                call["file"].as_str().is_some_and(|f| f.ends_with("ui.ts"))
                    && call["line"].as_i64() == Some(line)
            })
            .cloned()
            .collect()
    };
    let fetched = at(on, 7);
    assert_eq!(
        fetched.len(),
        1,
        "the control keeps its row: {:#?}",
        on.calls
    );
    assert_eq!(fetched[0]["path"], "/api/items", "{}", fetched[0]);
    assert!(
        !on.calls.iter().any(|call| call["path"] == "/graphql"),
        "no HTTP row at a GraphQL client call: {:#?}",
        on.calls
    );
    assert!(
        on.log_has(&["Withdrawing HTTP row", "/graphql", "GraphQL client"]),
        "the withdrawal is logged: {}",
        on.log
    );
}

/// A call can reach a GraphQL client and an HTTP client at once (a helper that
/// runs a hook and a fetch). It makes a real request, so the reader's row there
/// is kept and no GraphQL withdrawal is logged for it.
#[test]
fn a_row_at_a_call_with_a_graphql_end_and_an_http_end_is_kept() {
    let on = choosing();
    let kept: Vec<_> = on
        .calls
        .iter()
        .filter(|call| call["path"] == "/api/mixed")
        .collect();
    assert_eq!(kept.len(), 1, "the HTTP row is kept: {:#?}", on.calls);
    assert!(
        !on.log_has(&["Withdrawing HTTP row", "/api/mixed"]),
        "no withdrawal at a call with an HTTP end: {}",
        on.log
    );
}
