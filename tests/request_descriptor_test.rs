//! A request a function builds and returns, with nothing in its service
//! calling it, is a candidate call row (carrick#3470).
//!
//! Drives the real scanner binary as `action.yml` does (path `.`, the root
//! `carrick.json`, offline with the fixture's cassettes) over a committed copy
//! of `tests/fixtures/request-descriptor/`, whose README is the answer key.
//! Then the local re-check of the builders' file, and `check --changed` on a
//! branch that adds it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const REQUESTS: &str = "ops-console/src/order-requests.ts";
const INVOICES: &str = "ops-console/src/invoice-client.ts";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/request-descriptor")
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "carrick-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "carrick-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// A committed copy of the fixture under `tmp`, at `repo`.
fn committed_copy(tmp: &Path) -> PathBuf {
    let repo = tmp.join("repo");
    copy_dir(&fixture_dir(), &repo);
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A", "-f"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

/// The scanner on `.` from inside `repo`, offline, with the fixture's
/// cassettes.
fn scan_command(repo: &Path, storage: &Path, home: &Path, cache: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(repo)
        .env("HOME", home)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage)
        .env("CARRICK_CACHE_DIR", cache)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_NO_UPDATE_CHECK", "1")
        .env(
            "CARRICK_SIDECAR_DIR",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar"),
        );
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_EVENT_PATH",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        "CARRICK_OUTPUT_JSON",
        "CARRICK_NO_MODEL",
        "CARRICK_ALLOW_MISSING_TYPES",
        "CARRICK_LOCAL_STORAGE_ISOLATE",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

type Blobs = BTreeMap<String, serde_json::Value>;

fn read_blobs(storage: &Path) -> Blobs {
    std::fs::read_dir(storage)
        .expect("storage dir")
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let blob: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
            Some((blob["service_name"].as_str()?.to_string(), blob))
        })
        .collect()
}

/// The fixture scanned once for the whole binary.
fn scanned() -> &'static Blobs {
    static SCAN: OnceLock<Blobs> = OnceLock::new();
    SCAN.get_or_init(|| {
        let tmp = tempfile::tempdir().expect("temp dir");
        let repo = committed_copy(tmp.path());
        let storage = tmp.path().join("store");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let output = scan_command(&repo, &storage, &home, &tmp.path().join("cache"))
            .output()
            .expect("spawn carrick");
        assert!(
            output.status.success(),
            "the scan failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        read_blobs(&storage)
    })
}

/// A blob's HTTP call rows in `file`: `(line, METHOD, path, source)`.
fn http_calls_in(blob: &serde_json::Value, file: &str) -> Vec<(u32, String, String, String)> {
    let mut rows: Vec<(u32, String, String, String)> = blob["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|call| call["key"]["protocol"] == "http")
        .filter_map(|call| {
            let (at, line) = call["file_path"].as_str()?.rsplit_once(':')?;
            (at == file).then(|| {
                (
                    line.parse().unwrap(),
                    call["key"]["method"].as_str().unwrap().to_uppercase(),
                    call["key"]["path"].as_str().unwrap().to_string(),
                    call["resolution_source"].as_str().unwrap_or("").to_string(),
                )
            })
        })
        .collect();
    rows.sort();
    rows
}

fn row(line: u32, method: &str, path: &str, source: &str) -> (u32, String, String, String) {
    (
        line,
        method.to_string(),
        path.to_string(),
        source.to_string(),
    )
}

#[test]
fn a_returned_request_in_an_uncalled_builder_is_a_candidate_call_row() {
    let blobs = scanned();
    let console = blobs
        .get("ops-console")
        .expect("ops-console uploads a blob");
    assert_eq!(
        http_calls_in(console, REQUESTS),
        [
            row(7, "GET", "/api/v2/orders/:orderId", "request_descriptor"),
            row(15, "POST", "/api/v2/orders", "request_descriptor"),
            row(24, "GET", "/api/v1/orders/:orderId", "request_descriptor"),
        ],
        "the answer key's rows, and no row for the shapes that are no request"
    );
    // The graph the cross-service pass and the cloud read holds them too.
    let graph: Vec<(String, String, String)> = console["mount_graph"]["data_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|call| call["resolution_source"] == "request_descriptor")
        .map(|call| {
            (
                call["file_location"].as_str().unwrap().to_string(),
                call["method"].as_str().unwrap().to_uppercase(),
                call["canonical_path"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(graph.len(), 3, "the graph's descriptor rows: {graph:?}");
}

/// A builder its service calls is its caller's request: the file's rows are
/// the model's row at the `fetch`, as before the pass, and no descriptor.
#[test]
fn a_builder_its_service_calls_states_no_descriptor() {
    let console = &scanned()["ops-console"];
    assert_eq!(
        http_calls_in(console, INVOICES),
        [row(15, "GET", "/api/v2/invoices/:invoiceId", "model")],
    );
}

/// The POST builder's body is typed where it is written, as a request body
/// (`JSON.stringify(draft)` reads as `draft`); nothing is typed as its
/// response, since the literal is no call.
#[test]
fn the_built_body_is_typed_and_no_response_is() {
    let console = &scanned()["ops-console"];
    let alias = |kind: &str| -> Option<String> {
        console["type_manifest"]
            .as_array()?
            .iter()
            .find(|entry| {
                entry["file_path"] == REQUESTS
                    && entry["line_number"] == 15
                    && entry["evidence"]["infer_kind"] == kind
            })
            .and_then(|entry| entry["type_alias"].as_str().map(str::to_string))
    };
    let bundled = console["bundled_types"].as_str().unwrap_or_default();
    let request = alias("request_body").expect("a request entry at the POST builder");
    assert!(
        bundled.contains(&format!(
            "export type {request} = {{ sku: string; quantity: number; }};"
        )),
        "the body's type:\n{bundled}"
    );
    let response = alias("call_result").expect("the pair's response entry");
    assert!(
        bundled.contains(&format!("export type {response} = unknown;")),
        "no response is typed at the literal:\n{bundled}"
    );
}

/// A workspace of one repo, the fixture committed in it, with `held` (a
/// repo-relative file) left out of the commit and handed back.
fn workspace(held: Option<&str>) -> (tempfile::TempDir, Option<Vec<u8>>) {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path().join("repo");
    copy_dir(&fixture_dir(), &repo);
    let kept = held.map(|file| {
        let path = repo.join(file);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        bytes
    });
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A", "-f"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    std::fs::write(
        dir.path().join("carrick-workspace.json"),
        "{ \"repos\": [\"./repo\"] }\n",
    )
    .unwrap();
    (dir, kept)
}

/// A local command in `dir`, offline, as a signed-in machine runs it.
fn local(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_carrick"))
        .args(args)
        .current_dir(dir)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", dir.join(".test-credentials"))
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_NO_UPDATE_CHECK", "1")
        .env(
            "CARRICK_SIDECAR_DIR",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar"),
        )
        .output()
        .expect("spawn carrick")
}

/// The re-check of the builders' file lists each descriptor as a candidate
/// item, with the route that serves it, or none.
#[test]
fn the_re_check_lists_each_descriptor_as_a_candidate_with_its_route() {
    let (dir, _) = workspace(None);
    let refreshed = local(dir.path(), &["refresh", "--workspace", "."]);
    assert!(refreshed.status.success(), "refresh failed");
    let checked = local(
        dir.path(),
        &[
            "check",
            &format!("repo/{REQUESTS}"),
            "--workspace",
            ".",
            "--recheck",
            "--json",
        ],
    );
    assert!(checked.status.success(), "check --recheck failed");
    let answer: serde_json::Value =
        serde_json::from_slice(&checked.stdout).expect("check --json is JSON");
    let items: Vec<(u64, String, String, String, Vec<String>)> = answer["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["kind"] == "call")
        .map(|item| {
            (
                item["line"].as_u64().unwrap(),
                item["method"].as_str().unwrap().to_string(),
                item["source"].as_str().unwrap().to_string(),
                item["resolution_source"].as_str().unwrap().to_string(),
                item["counterparts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| c["service"].as_str().unwrap().to_string())
                    .collect(),
            )
        })
        .collect();
    let item = |line: u64, method: &str, served: &[&str]| {
        (
            line,
            method.to_string(),
            "candidate".to_string(),
            "request_descriptor".to_string(),
            served.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        items,
        [
            item(7, "GET", &["orders-api"]),
            item(15, "POST", &["orders-api"]),
            item(24, "GET", &[]),
        ]
    );
}

/// `check --changed` on a branch that adds the builders fails on none of
/// them and exits 0: the one that reaches no route is counted on the summary
/// line as a built request and listed with `--all` as not failing.
#[test]
fn check_changed_on_a_branch_adding_builders_fails_on_none() {
    let (dir, held) = workspace(Some(REQUESTS));
    assert!(
        local(dir.path(), &["refresh", "--workspace", "."])
            .status
            .success()
    );
    let repo = dir.path().join("repo");
    git(&repo, &["checkout", "-q", "-b", "add-builders"]);
    std::fs::write(repo.join(REQUESTS), held.unwrap()).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "add builders"]);
    let plain = local(&repo, &["check", "--changed"]);
    let text = String::from_utf8_lossy(&plain.stdout).into_owned();
    assert!(
        text.contains("0 incompatible, 0 reach no route, plus 1 built request not seen sent"),
        "the summary counts the unmatched built request as its own kind:\n{text}"
    );
    // Not 1: no problem fails. Not 2 (carrick#3012): the matched pairs hold
    // no response type on the descriptor's side, but a matched method and
    // path is the check a built request can have.
    assert_eq!(plain.status.code(), Some(0), "{text}");
    let all = local(&repo, &["check", "--changed", "--all"]);
    let listed = String::from_utf8_lossy(&all.stdout).into_owned();
    assert!(
        listed.lines().any(
            |line| line.starts_with(&format!("repo/{REQUESTS}:24  no route"))
                && line.ends_with("does not fail]")
        ),
        "the unmatched candidate is listed and does not fail:\n{listed}"
    );
}

/// The fixture's route whose only matched callers are built requests.
const LOOKUP_ROUTE: &str = "orders-api/app/routes/api.v2.orders.$orderId.ts";

/// `check --changed` on `repo` after a branch that commits `edits`, run once
/// the workspace is refreshed on main.
fn check_changed_after(dir: &Path, edits: &[(&str, &str)]) -> std::process::Output {
    let repo = dir.join("repo");
    git(&repo, &["checkout", "-q", "-b", "edit-route"]);
    for (file, text) in edits {
        std::fs::write(repo.join(file), text).unwrap();
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "edit route"]);
    local(&repo, &["check", "--changed"])
}

/// `check --changed` on a branch that edits only a route whose matched
/// callers are all built requests exits 0 (carrick#3532): a matched method
/// and path is the check such a pair can have, whichever side changed.
#[test]
fn check_changed_on_a_route_whose_callers_are_built_requests_exits_0() {
    let (dir, _) = workspace(None);
    assert!(
        local(dir.path(), &["refresh", "--workspace", "."])
            .status
            .success()
    );
    let edited = "// GET /api/v2/orders/:orderId, now with its currency.\n\n\
export type Order = {\n  id: string;\n  total: number;\n  currency: string;\n};\n\n\
export async function loader({ params }: { params: { orderId: string } }): Promise<Order> {\n  \
return { id: String(params.orderId), total: 0, currency: \"EUR\" };\n}\n";
    let checked = check_changed_after(dir.path(), &[(LOOKUP_ROUTE, edited)]);
    let text = String::from_utf8_lossy(&checked.stdout).into_owned();
    assert_eq!(checked.status.code(), Some(0), "{text}");
}

/// A route whose matched caller is a call the code sends, with no verdict
/// the type check could reach, still makes the run exit 2: only built
/// requests stand in for a type check.
#[test]
fn check_changed_on_a_route_with_an_unjudged_sent_call_still_exits_2() {
    const INVOICE_ROUTE: &str = "orders-api/app/routes/api.v2.invoices.$invoiceId.ts";
    let route = |marker: &str| {
        format!(
            "// GET /api/v2/invoices/:invoiceId{marker}\n\n\
export async function loader({{ params }}: {{ params: {{ invoiceId: string }} }}): Promise<any> {{\n  \
return {{ id: String(params.invoiceId) }};\n}}\n"
        )
    };
    let (dir, _) = workspace(None);
    let repo = dir.path().join("repo");
    std::fs::write(repo.join(INVOICE_ROUTE), route("")).unwrap();
    // A call the code sends, through a wrapper the local check reads with
    // no model: a fact row, matched to the route above, never judged.
    std::fs::write(
        repo.join("ops-console/src/send.ts"),
        "export async function send<T>(url: string, options: { method: string }): Promise<T> {\n  \
const response = await fetch(url, options);\n  return (await response.json()) as T;\n}\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("ops-console/src/invoice-reader.ts"),
        "import { send } from \"./send.js\";\n\n\
export type Invoice = { id: string };\n\n\
export class InvoiceReader {\n  constructor(private readonly baseUrl: string) {}\n\n  \
readInvoice(invoiceId: string): Promise<Invoice> {\n    \
return send<Invoice>(`${this.baseUrl}/api/v2/invoices/${invoiceId}`, { method: \"GET\" });\n  }\n}\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "invoice route and reader"]);
    assert!(
        local(dir.path(), &["refresh", "--workspace", "."])
            .status
            .success()
    );
    let checked = check_changed_after(dir.path(), &[(INVOICE_ROUTE, &route(", edited."))]);
    let text = String::from_utf8_lossy(&checked.stdout).into_owned();
    assert!(
        text.contains("checked 1 route and 0 calls")
            && text.contains("1 not type-checked")
            && !text.contains("built request"),
        "the route's one pair is unjudged and its call is no built request:\n{text}"
    );
    assert_eq!(checked.status.code(), Some(2), "{text}");
}
