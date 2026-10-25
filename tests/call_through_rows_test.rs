//! Model rows at calls through the repository's own code (carrick#1146).
//!
//! Drives the real scanner binary on `.`, as the Action runs it, over a
//! committed copy of `tests/fixtures/call-through-client/`: four services,
//! where the requests are written in `packages/client` and reached from
//! `packages/cli` and `apps/web` through a client class, a factory, a command
//! that calls another command, and hook arguments. The model's answers are
//! replayed from the fixture's cassette, and state what a model states at
//! such sites: a path made from a method name, an invented base, a sibling's
//! route, and one correct path. Half of them carry an id that joins no
//! candidate, so those rows have no span and are placed by their line.
//!
//! Asserts on the uploaded blobs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const SERVICES: [&str; 4] = ["api", "cli", "client", "web"];

const COMMANDS: &str = "packages/cli/src/commands.ts";
const HOOKS: &str = "apps/web/src/hooks.ts";
const REQUESTS: &str = "packages/client/src/requests.ts";
const TOOLS: &str = "packages/cli/src/tools.ts";
const MAIN: &str = "packages/cli/src/main.ts";
const NESTED: &str = "packages/cli/src/nested.ts";
const PAGE: &str = "apps/web/src/page.tsx";
const BADGE: &str = "apps/web/src/badge.tsx";
const HELPERS: &str = "packages/cli/src/helpers.ts";
const PROBED: &str = "packages/cli/src/probed-client.ts";
const API_CLIENT: &str = "packages/cli/src/api-client.ts";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/call-through-client")
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "carrick-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "carrick-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
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

/// A committed copy of the fixture under `tmp`.
fn committed_copy(tmp: &Path) -> PathBuf {
    committed_copy_with(tmp, |_| {})
}

/// A committed copy of the fixture under `tmp`, `edit`ed before the commit.
fn committed_copy_with(tmp: &Path, edit: impl FnOnce(&Path)) -> PathBuf {
    let repo = tmp.join("workspace");
    copy_dir(&fixture_dir(), &repo);
    edit(&repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

/// The scanner run on `.` from inside `repo`, offline, with the fixture's
/// cassette and a store at `storage`.
fn scan_command(repo: &Path, storage: &Path) -> Command {
    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(".")
        .current_dir(repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1");
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
        "CARRICK_LOCAL_STORAGE_ISOLATE",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

fn run(mut cmd: Command) {
    let output = cmd.output().expect("failed to spawn carrick");
    assert!(
        output.status.success(),
        "fixture scan exited non-zero:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Every service's blob, keyed by service name.
type Blobs = BTreeMap<String, serde_json::Value>;

fn read_blobs(storage: &Path) -> BTreeMap<String, (PathBuf, serde_json::Value)> {
    std::fs::read_dir(storage)
        .expect("storage dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let blob: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
            let service = blob["service_name"].as_str()?.to_string();
            Some((service, (path, blob)))
        })
        .collect()
}

/// Scan a committed copy of the fixture offline, once for the whole binary.
fn blobs() -> &'static Blobs {
    &scan().0
}

/// The line the pass logs at debug about what it settled, read from the run
/// log the scan keeps under its home directory.
fn settled_line() -> &'static str {
    &scan().1
}

fn scan() -> &'static (Blobs, String) {
    static SCAN: OnceLock<(Blobs, String)> = OnceLock::new();
    SCAN.get_or_init(|| {
        let tmp = tempfile::tempdir().expect("temp dir");
        let repo = committed_copy(tmp.path());
        let storage = tmp.path().join("storage");
        std::fs::create_dir_all(&storage).unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut cmd = scan_command(&repo, &storage);
        cmd.env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
            .env("CARRICK_CACHE_DIR", tmp.path().join("cache"))
            .env("HOME", &home);
        run(cmd);
        let settled = std::fs::read_dir(home.join(".carrick/logs/runs"))
            .expect("the scan keeps a run log")
            .filter_map(|entry| std::fs::read_to_string(entry.ok()?.path()).ok())
            .flat_map(|log| {
                log.lines()
                    .filter(|line| line.contains("Call-through rows settled"))
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .next()
            .expect("the pass logs what it settled");
        let blobs: Blobs = read_blobs(&storage)
            .into_iter()
            .map(|(service, (_, blob))| (service, blob))
            .collect();
        assert_eq!(
            blobs.keys().map(String::as_str).collect::<Vec<_>>(),
            SERVICES,
            "every service uploads one blob"
        );
        (blobs, settled)
    })
}

/// The response verdicts stored for the call at `file:line`, each with the
/// producer operation it was judged against.
fn response_verdicts_at(file: &str, line: u32) -> Vec<(String, String)> {
    let site = format!("{file}:{line}");
    blobs()
        .values()
        .flat_map(|blob| {
            blob["compat_verdicts"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .flat_map(|pair| {
            let producer = text(&pair["producer_key"]).unwrap_or_default();
            pair["sites"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|at| at["consumer_location"] == site.as_str())
                .filter_map(|at| text(&at["response"]["verdict"]))
                .map(|verdict| (producer.clone(), verdict))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn text(value: &serde_json::Value) -> Option<String> {
    value.as_str().map(str::to_string)
}

/// One call row of a blob's mount graph.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    method: String,
    target: String,
    path: String,
    source: String,
    role: Option<String>,
    reaches: Option<String>,
}

fn data_calls(service: &str) -> &'static Vec<serde_json::Value> {
    blobs()[service]["mount_graph"]["data_calls"]
        .as_array()
        .expect("the blob holds a mount graph")
}

/// The one call row of `service` at `file:line`.
fn row_at(service: &str, file: &str, line: u32) -> Row {
    let rows = rows_in(blobs(), service, file, line);
    assert_eq!(rows.len(), 1, "one row at {file}:{line}, got {rows:?}");
    rows.into_iter().next().unwrap()
}

/// Every call row of `service` at `file:line` in `blobs`, sorted by path.
fn rows_in(blobs: &Blobs, service: &str, file: &str, line: u32) -> Vec<Row> {
    let site = format!("{file}:{line}");
    let mut rows: Vec<Row> = blobs[service]["mount_graph"]["data_calls"]
        .as_array()
        .expect("the blob holds a mount graph")
        .iter()
        .filter(|call| call["file_location"] == site.as_str())
        .map(|call| Row {
            method: text(&call["method"]).unwrap_or_default(),
            target: text(&call["target_url"]).unwrap_or_default(),
            path: text(&call["canonical_path"]).unwrap_or_default(),
            source: text(&call["resolution_source"]).unwrap_or_default(),
            role: text(&call["role"]),
            reaches: text(&call["reaches_request"]),
        })
        .collect();
    rows.sort_by(|a, b| (&a.path, &a.method).cmp(&(&b.path, &b.method)));
    rows
}

/// A row that took the operation of the request written at `REQUESTS:line`.
fn corrected(method: &str, path: &str, request_line: u32) -> Row {
    Row {
        method: method.to_string(),
        target: path.to_string(),
        path: path.to_string(),
        source: "request_summary".to_string(),
        role: Some("wrapper_call".to_string()),
        reaches: Some(format!("{REQUESTS}:{request_line}")),
    }
}

/// A row left exactly as the model stated it.
fn as_stated(method: &str, target: &str, path: &str) -> Row {
    Row {
        method: method.to_string(),
        target: target.to_string(),
        path: path.to_string(),
        source: "model".to_string(),
        role: None,
        reaches: None,
    }
}

/// The consumer type entries the manifest holds at `file:line`.
fn consumer_entries_at(service: &str, file: &str, line: u32) -> Vec<serde_json::Value> {
    blobs()[service]["type_manifest"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| {
            entry["role"] == "consumer"
                && entry["file_path"] == file
                && entry["line_number"] == line
        })
        .collect()
}

#[test]
fn a_method_call_on_a_factory_bound_client_takes_the_request_the_method_reaches() {
    // The model made the path from the method's name, at a row with no span.
    assert_eq!(
        row_at("cli", COMMANDS, 9),
        corrected("GET", "/api/v1/runs/:id", 5)
    );
    // The model stated the call twice, once as a sibling's route and once
    // under the method's name. Both rows reach one request, and one row is
    // kept.
    assert_eq!(
        row_at("cli", COMMANDS, 21),
        corrected("GET", "/api/v1/runs", 8)
    );
}

#[test]
fn a_command_that_calls_another_command_takes_the_request_and_loses_the_invented_base() {
    assert_eq!(
        row_at("cli", COMMANDS, 15),
        corrected("GET", "/api/v1/runs/:id", 5)
    );
    for service in SERVICES {
        for call in data_calls(service) {
            assert!(
                !call["target_url"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("API_URL"),
                "no row keeps the base the model invented: {call}"
            );
        }
    }
}

#[test]
fn a_row_the_model_stated_correctly_stays_the_models_and_keeps_its_type_anchor() {
    assert_eq!(
        row_at("cli", COMMANDS, 28),
        Row {
            method: "GET".to_string(),
            target: "/api/v1/status".to_string(),
            path: "/api/v1/status".to_string(),
            source: "model".to_string(),
            role: Some("wrapper_call".to_string()),
            reaches: Some(format!("{REQUESTS}:23")),
        }
    );
    let entries = consumer_entries_at("cli", COMMANDS, 28);
    assert!(
        entries
            .iter()
            .any(|entry| entry["primary_type_symbol"] == "Status"),
        "the type the model anchored at the call is kept: {entries:?}"
    );
}

/// The corrected row's own call is the call that reaches the request, and the
/// function it calls hands back the request's parsed body unchanged, so the
/// type anchored at it is what the caller reads of that request: it is kept
/// under the reached operation and judged against that operation's producer.
#[test]
fn a_corrected_row_keeps_its_type_and_is_judged_against_the_reached_operation() {
    assert_eq!(
        row_at("cli", COMMANDS, 145),
        Row {
            method: "GET".to_string(),
            target: "${process.env.RUNS_ORIGIN}/api/v1/runs/${id}".to_string(),
            path: "/api/v1/runs/:id".to_string(),
            source: "request_summary".to_string(),
            role: Some("wrapper_call".to_string()),
            reaches: Some(format!("{REQUESTS}:45")),
        }
    );
    let entries = consumer_entries_at("cli", COMMANDS, 145);
    assert!(
        entries.iter().any(|entry| {
            entry["type_kind"] == "response"
                && entry["method"] == "GET"
                && entry["path"] == "/api/v1/runs/:id"
                && entry["primary_type_symbol"] == "Run"
        }),
        "the response entry is keyed to the reached operation: {entries:#?}"
    );
    assert!(
        entries.iter().all(|entry| entry["path"] != "/read"),
        "no entry keeps the path the model made up: {entries:#?}"
    );
    assert_eq!(
        response_verdicts_at(COMMANDS, 145),
        [(
            "http|GET|/api/v1/runs/:id".to_string(),
            "compatible".to_string()
        )]
    );
}

/// The caller's type is a stale copy: it requires a field the API no longer
/// sends. The model's row named a path nothing serves, so nothing was
/// compared; the corrected row is compared and the break is found.
#[test]
fn a_stale_type_at_a_corrected_row_is_found() {
    assert_eq!(
        response_verdicts_at(COMMANDS, 157),
        [(
            "http|GET|/api/v1/runs/:id".to_string(),
            "incompatible".to_string()
        )]
    );
}

/// carrick#1601: a call of a function that does not hand back its request's
/// parsed body unchanged is worth that function's own result. The function
/// called wraps the body in an envelope (`readRunStamped`), or the client
/// method called returns a member of what a transport returns (`Client.send`
/// reads `.data` of a request spec's value, whose shape nothing here
/// states). The row still takes the operation, and the type anchored there
/// goes: judged against the producer, it would read as a break that is not
/// there.
#[test]
fn a_corrected_row_whose_call_returns_another_shape_carries_no_type() {
    assert_eq!(
        row_at("cli", COMMANDS, 151).reaches,
        Some(format!("{REQUESTS}:51"))
    );
    for line in [9, 46, 151] {
        let entries = consumer_entries_at("cli", COMMANDS, line);
        assert!(
            entries.is_empty(),
            "no type is carried from a call that returns another shape ({COMMANDS}:{line}): {entries:#?}"
        );
        assert_eq!(response_verdicts_at(COMMANDS, line), []);
    }
}

/// Every method of the client makes a request of its own before the one it
/// hands back, and drops its value: a probe. The request the call reaches is
/// the one whose value the method returns, whether the probe is a request a
/// fact states (line 64) or one only the model reads (line 164).
#[test]
fn a_call_whose_client_drops_a_probe_takes_the_request_it_hands_back() {
    for line in [64, 164] {
        assert_eq!(
            row_at("cli", COMMANDS, line),
            corrected("GET", "/api/v1/runs/:id", 5),
            "{COMMANDS}:{line}"
        );
    }
}

/// The function called asks for a token and hands it to the request it
/// returns. The token is that request's input, not what the call answers
/// with, so the call reaches the run.
#[test]
fn a_token_handed_to_the_returned_request_is_not_the_request_reached() {
    assert_eq!(
        row_at("cli", COMMANDS, 176),
        corrected("GET", "/api/v1/runs/:id", 5)
    );
}

/// The function called asks for a token, a request a fact states, and then
/// makes a `fetch` only the model reads. The token is not the one operation
/// the call reaches, so the row stays as the model stated it, and is counted.
#[test]
fn a_call_that_also_reaches_a_request_no_fact_states_stays_as_the_model_stated_it() {
    assert_eq!(
        row_at("cli", COMMANDS, 139),
        as_stated("GET", "/tokened", "/tokened")
    );
    assert!(
        settled_line().contains("2 reach a request only the model states"),
        "{}",
        settled_line()
    );
}

/// The request is reached through a callback or a spread call, so the row's
/// call is the hook's and the type anchored there is the hook's result, not
/// what the request returns. Those entries go, and are counted.
#[test]
fn a_type_anchored_at_a_call_that_is_not_the_requests_is_dropped_and_counted() {
    for (file, line) in [(HOOKS, 9), (HOOKS, 14), (HOOKS, 19)] {
        let entries = consumer_entries_at("web", file, line);
        assert!(
            entries.is_empty(),
            "no type is carried from a hook's call ({file}:{line}): {entries:#?}"
        );
        assert_eq!(response_verdicts_at(file, line), []);
    }
    assert!(
        settled_line().contains("(12 lost the types anchored at a call that is not the request's,"),
        "{}",
        settled_line()
    );
}

/// At a call through, the arguments are the called function's parameters and
/// not a request body, so no corrected row carries a request entry, and the
/// rows that kept a response type are counted as having lost the request's.
#[test]
fn a_corrected_row_never_carries_a_request_entry() {
    for (service, file, line) in [
        ("cli", COMMANDS, 9),
        ("cli", COMMANDS, 15),
        ("cli", COMMANDS, 21),
        ("cli", COMMANDS, 46),
        ("cli", COMMANDS, 101),
        ("cli", COMMANDS, 145),
        ("cli", COMMANDS, 151),
        ("cli", COMMANDS, 157),
        ("web", HOOKS, 9),
        ("web", HOOKS, 14),
        ("web", HOOKS, 19),
    ] {
        let requests: Vec<serde_json::Value> = consumer_entries_at(service, file, line)
            .into_iter()
            .filter(|entry| entry["type_kind"] == "request")
            .collect();
        assert!(
            requests.is_empty(),
            "no request type is checked at a call through ({file}:{line}): {requests:#?}"
        );
    }
    // Lines 145 and 157 of `commands.ts` keep their response entry.
    assert!(
        settled_line().contains("2 kept the response type and lost the request type)"),
        "{}",
        settled_line()
    );
}

#[test]
fn a_hook_call_takes_the_request_its_arguments_reach() {
    // The result of a call into the client, spread into the hook's options.
    assert_eq!(
        row_at("web", HOOKS, 9),
        corrected("GET", "/api/v1/runs/:id", 5)
    );
    // The one callback among the hook's options.
    assert_eq!(
        row_at("web", HOOKS, 14),
        corrected("GET", "/api/v1/runs", 8)
    );
    // A callback handed to one of the app's own functions, which makes no
    // request itself.
    assert_eq!(
        row_at("web", HOOKS, 19),
        corrected("POST", "/api/v1/audit", 17)
    );
}

/// `save(id, await loadToken())`: both functions reach a request, and the row
/// is the call to `save`. A plain argument's value is never read.
#[test]
fn the_calls_own_callee_decides_before_any_argument() {
    assert_eq!(
        row_at("cli", COMMANDS, 46),
        corrected("PUT", "/api/v1/runs/:id", 11)
    );
}

#[test]
fn a_row_whose_call_proves_no_one_operation_stays_as_the_model_stated_it() {
    let mut changed = Vec::new();
    for (service, file, line, stated) in [
        // A helper that calls two client methods reaches two operations.
        (
            "cli",
            COMMANDS,
            57,
            as_stated("GET", "/api/v1/run-and-list", "/api/v1/run-and-list"),
        ),
        // Nothing resolves the receiver.
        (
            "cli",
            COMMANDS,
            70,
            as_stated("GET", "/api/v1/injected/${id}", "/api/v1/injected/:id"),
        ),
        // The function called writes its URL from its parameters alone, so
        // no row states its request.
        ("cli", COMMANDS, 76, as_stated("GET", "/reply", "/reply")),
        // The site writes the route itself.
        ("cli", COMMANDS, 82, as_stated("GET", "/orders", "/orders")),
        // The site also writes a request of its own.
        (
            "cli",
            COMMANDS,
            88,
            as_stated("GET", "/api/v1/notified/${id}", "/api/v1/notified/:id"),
        ),
        // Only the model states the request the function called makes.
        (
            "cli",
            COMMANDS,
            94,
            as_stated("GET", "/api/v1/pings", "/api/v1/pings"),
        ),
        // The site writes the route, and the function it calls reaches one
        // request of its own (a token). The row is the one the scanner reads
        // off the site's own source, which this pass never touches.
        (
            "cli",
            COMMANDS,
            113,
            Row {
                method: "GET".to_string(),
                target: "${base}/api/v1/widgets".to_string(),
                path: "/api/v1/widgets".to_string(),
                source: "same_file_wrapper".to_string(),
                role: Some("network_request".to_string()),
                reaches: None,
            },
        ),
        // The site makes a request itself, inside a call to a function that
        // reaches one request of its own.
        (
            "cli",
            COMMANDS,
            127,
            as_stated("POST", "/api/v1/audited", "/api/v1/audited"),
        ),
        // Two callbacks: one through a client nothing resolves, one that
        // reaches a request.
        (
            "web",
            HOOKS,
            24,
            as_stated("POST", "/vendor/send", "/vendor/send"),
        ),
    ] {
        let row = row_at(service, file, line);
        if row != stated {
            changed.push((format!("{file}:{line}"), row));
        }
    }
    assert!(
        changed.is_empty(),
        "each of these must stay as the model stated it: {changed:#?}"
    );
}

#[test]
fn every_request_row_is_unchanged() {
    let mut requests: Vec<(String, String, String)> = data_calls("client")
        .iter()
        .map(|call| {
            (
                text(&call["file_location"]).unwrap(),
                format!(
                    "{} {}",
                    call["method"].as_str().unwrap(),
                    call["canonical_path"].as_str().unwrap()
                ),
                text(&call["resolution_source"]).unwrap(),
            )
        })
        .collect();
    let spec = |line: u32, operation: &str| {
        (
            format!("{REQUESTS}:{line}"),
            operation.to_string(),
            "request_spec".to_string(),
        )
    };
    requests.sort_by_key(|(site, _, _)| {
        site.rsplit_once(':')
            .and_then(|(_, line)| line.parse::<u32>().ok())
    });
    assert_eq!(
        requests,
        [
            spec(5, "GET /api/v1/runs/:id"),
            spec(8, "GET /api/v1/runs"),
            spec(11, "PUT /api/v1/runs/:id"),
            spec(14, "GET /api/v1/token"),
            spec(17, "POST /api/v1/audit"),
            spec(20, "GET /health"),
            spec(23, "GET /api/v1/status"),
            (
                format!("{REQUESTS}:27"),
                "GET /api/v1/ping".to_string(),
                "model".to_string()
            ),
            spec(39, "POST /api/v1/runs/:id/archive"),
            (
                format!("{REQUESTS}:45"),
                "GET /api/v1/runs/:id".to_string(),
                "env_base_path".to_string()
            ),
            (
                format!("{REQUESTS}:51"),
                "GET /api/v1/runs/:id".to_string(),
                "env_base_path".to_string()
            ),
        ]
    );
}

/// A changed row is changed in the mount graph and in the `calls` projection
/// of it, which is what the cross-service check and the upload read.
#[test]
fn the_calls_projection_agrees_with_the_mount_graph() {
    for service in SERVICES {
        let mut graph: Vec<(String, String, String, String)> = data_calls(service)
            .iter()
            .map(|call| {
                (
                    text(&call["file_location"]).unwrap(),
                    text(&call["method"]).unwrap(),
                    text(&call["canonical_path"]).unwrap(),
                    text(&call["resolution_source"]).unwrap_or_default(),
                )
            })
            .collect();
        let mut projected: Vec<(String, String, String, String)> = blobs()[service]["calls"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|call| call["key"]["protocol"] == "http" || call["key"]["method"].is_string())
            .map(|call| {
                (
                    text(&call["file_path"]).unwrap(),
                    text(&call["key"]["method"]).unwrap(),
                    text(&call["key"]["path"]).unwrap(),
                    text(&call["resolution_source"]).unwrap_or_default(),
                )
            })
            .collect();
        graph.sort();
        projected.sort();
        assert_eq!(graph, projected, "service {service}");
    }
}

/// The boundary's counts are read off the settled rows: a corrected row is
/// not the model's alone, and no type is missing at it.
#[test]
fn the_boundary_counts_the_settled_rows() {
    let boundary = &blobs()["cli"]["boundary"];
    // Eleven rows in `commands.ts` the model alone states, and the one at
    // `nested.ts:7`.
    assert_eq!(boundary["model_only_rows"], 12, "{boundary}");
    let untyped: Vec<String> = boundary["calls_without_expected_type"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(text)
        .collect();
    // Lines 145 and 157 carry a resolved type. Line 15 never held a type
    // entry (the model's base kept its key off every route), so no type is
    // asked for there.
    for line in [15, 145, 157] {
        let site = format!("({COMMANDS}:{line})");
        assert!(
            !untyped.iter().any(|reason| reason.ends_with(&site)),
            "a corrected row is not a call missing its type: {untyped:?}"
        );
    }
}

/// A call into the client class the model stated no row at states the request
/// it reaches from the source alone.
#[test]
fn a_call_into_the_client_the_model_left_unstated_states_the_request_it_reaches() {
    for (line, method, path, request_line) in [
        (34, "GET", "/api/v1/token", 14),
        (40, "PUT", "/api/v1/runs/:id", 11),
        (121, "POST", "/api/v1/audit", 17),
    ] {
        assert_eq!(
            row_at("cli", COMMANDS, line),
            corrected(method, path, request_line),
            "{COMMANDS}:{line}"
        );
    }
    // Two calls on one line state a row each.
    assert_eq!(
        rows_in(blobs(), "cli", COMMANDS, 53),
        [
            corrected("GET", "/api/v1/runs", 8),
            corrected("GET", "/api/v1/runs/:id", 5),
        ]
    );
    // Without the compiler's rows nothing types the table's parameter.
    assert_eq!(rows_in(blobs(), "cli", TOOLS, 9), []);
}

/// The fixture with the model's answers for the CLI's commands and the web's
/// hooks emptied, scanned once with the compiler's rows: every row at a call
/// through the client class is then the source's alone.
fn unanswered() -> &'static Blobs {
    static SCAN: OnceLock<Blobs> = OnceLock::new();
    SCAN.get_or_init(|| {
        let tmp = tempfile::tempdir().expect("temp dir");
        let repo = committed_copy_with(tmp.path(), |repo| {
            let empty = serde_json::json!({
                "mounts": [], "endpoints": [], "data_calls": [], "pubsub_operations": []
            });
            for answer in ["commands.json", "hooks.json"] {
                std::fs::write(
                    repo.join("__llm__/analyze-file").join(answer),
                    serde_json::to_string_pretty(&empty).unwrap(),
                )
                .unwrap();
            }
        });
        let storage = tmp.path().join("storage");
        std::fs::create_dir_all(&storage).unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut cmd = scan_command(&repo, &storage);
        cmd.env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
            .env("CARRICK_CACHE_DIR", tmp.path().join("cache"))
            .env("CARRICK_POSITION_ROWS", "1")
            .env("HOME", &home);
        run(cmd);
        read_blobs(&storage)
            .into_iter()
            .map(|(service, (_, blob))| (service, blob))
            .collect()
    })
}

#[test]
fn every_call_into_the_client_states_its_request_with_no_model_row() {
    let blobs = unanswered();
    for (file, line, method, path, request_line) in [
        (COMMANDS, 9, "GET", "/api/v1/runs/:id", 5),
        (COMMANDS, 21, "GET", "/api/v1/runs", 8),
        (COMMANDS, 28, "GET", "/api/v1/status", 23),
        (COMMANDS, 34, "GET", "/api/v1/token", 14),
        (COMMANDS, 40, "PUT", "/api/v1/runs/:id", 11),
        // The client's own probe is a side request.
        (COMMANDS, 64, "GET", "/api/v1/runs/:id", 5),
        (COMMANDS, 101, "POST", "/api/v1/runs/:id/archive", 39),
        (COMMANDS, 121, "POST", "/api/v1/audit", 17),
        (COMMANDS, 164, "GET", "/api/v1/runs/:id", 5),
        // Only the compiler types the parameter: this row needs the
        // compiler's rows, which the scan walks with the sidecar's build
        // (`src/sidecar/dist`).
        (TOOLS, 9, "GET", "/api/v1/runs/:id", 5),
    ] {
        assert_eq!(
            rows_in(blobs, "cli", file, line),
            [corrected(method, path, request_line)],
            "{file}:{line}"
        );
    }
    assert_eq!(
        rows_in(blobs, "cli", COMMANDS, 53),
        [
            corrected("GET", "/api/v1/runs", 8),
            corrected("GET", "/api/v1/runs/:id", 5),
        ]
    );
}

#[test]
fn no_row_is_stated_a_hop_above_the_client_or_where_no_function_crosses_for_the_call() {
    let blobs = unanswered();
    let mut stated = Vec::new();
    for (service, file, line) in [
        // A command of another file, which calls the client itself: the
        // command's own call is the row.
        ("cli", MAIN, 5),
        // A command of the same file, and a helper of the same file.
        ("cli", COMMANDS, 15),
        ("cli", COMMANDS, 46),
        ("cli", COMMANDS, 57),
        // Nothing resolves the receiver.
        ("cli", COMMANDS, 70),
    ] {
        stated.extend(
            rows_in(blobs, service, file, line)
                .into_iter()
                .filter(|row| row.role.as_deref() == Some("wrapper_call"))
                .map(|row| (format!("{file}:{line}"), row)),
        );
    }
    assert!(stated.is_empty(), "no call-through row here: {stated:#?}");
}

/// A call straight into a function another service declares states the
/// request that function reaches in the service that writes it
/// (carrick#2773), at the call, where no row sits.
#[test]
fn a_call_straight_into_another_services_function_states_the_request_it_reaches() {
    let blobs = unanswered();
    for (service, file, line, method, path, request_line) in [
        // A factory's options spread into a hook: the factory hands its
        // request back inside the object, unrun.
        ("web", HOOKS, 9, "GET", "/api/v1/runs/:id", 5),
        // A callback handed to the hook, and to one of the web's own
        // functions.
        ("web", HOOKS, 14, "GET", "/api/v1/runs", 8),
        ("web", HOOKS, 19, "POST", "/api/v1/audit", 17),
        ("web", HOOKS, 24, "POST", "/api/v1/audit", 17),
        ("cli", COMMANDS, 145, "GET", "/api/v1/runs/:id", 45),
        ("cli", COMMANDS, 151, "GET", "/api/v1/runs/:id", 51),
        ("cli", COMMANDS, 157, "GET", "/api/v1/runs/:id", 45),
        // `authorizedRun` is called from its own file, so it is no wrapper.
        ("cli", COMMANDS, 171, "GET", "/api/v1/runs/:id", 5),
    ] {
        let mut row = corrected(method, path, request_line);
        // The two requests written on a declared environment base keep it in
        // their target.
        if request_line == 45 || request_line == 51 {
            row.target = "${process.env.RUNS_ORIGIN}/api/v1/runs/${id}".to_string();
        }
        assert_eq!(rows_in(blobs, service, file, line), [row], "{file}:{line}");
    }
    // The request is written from the parameters, or only the model states it.
    for line in [76, 82, 94] {
        assert_eq!(
            rows_in(blobs, "cli", COMMANDS, line),
            [],
            "{COMMANDS}:{line}"
        );
    }
}

/// A boundary call stated whether or not the model stated a row: the first
/// scan and the unanswered one alike.
#[test]
fn a_boundary_call_in_a_function_no_other_call_reaches_one_request_through_states_its_row() {
    for blobs in [blobs(), unanswered()] {
        // `loadPair` reaches two operations, so it is no wrapper.
        assert_eq!(
            rows_in(blobs, "cli", HELPERS, 6),
            [
                corrected("GET", "/api/v1/runs", 8),
                corrected("GET", "/api/v1/runs/:id", 5),
            ]
        );
        // `statusReader` hands its reader back unrun: it is no wrapper.
        assert_eq!(
            rows_in(blobs, "cli", HELPERS, 27),
            [corrected("GET", "/api/v1/status", 23)]
        );
        // The probe in the private `invoke`, which no other file calls.
        assert_eq!(
            rows_in(blobs, "cli", PROBED, 14),
            [corrected("GET", "/health", 20)]
        );
    }
}

/// A helper only its own file calls gets no row beside the one its caller
/// holds: the model's wrong route at the caller is corrected to the request,
/// and the call inside the helper states nothing more.
#[test]
fn a_boundary_call_in_a_same_file_helper_whose_caller_holds_the_row_states_nothing() {
    assert_eq!(
        rows_in(blobs(), "cli", COMMANDS, 186),
        [corrected("GET", "/api/v1/runs/:id", 5)]
    );
    assert_eq!(rows_in(blobs(), "cli", COMMANDS, 182), []);
    // With no row at the caller, the helper's call is the site.
    assert_eq!(
        rows_in(unanswered(), "cli", COMMANDS, 182),
        [corrected("GET", "/api/v1/runs/:id", 5)]
    );
}

/// A boundary call inside a function whose callers state the row states
/// nothing: the callers are the sites, one per module that calls in.
#[test]
fn a_boundary_call_inside_a_wrapper_states_nothing() {
    for blobs in [blobs(), unanswered()] {
        for (service, file, lines) in [
            ("cli", API_CLIENT, vec![18, 22, 27, 32, 36, 40, 44]),
            ("cli", PROBED, vec![10, 35]),
            ("cli", HELPERS, vec![16, 21]),
            ("web", BADGE, vec![5]),
        ] {
            for line in lines {
                assert_eq!(rows_in(blobs, service, file, line), [], "{file}:{line}");
            }
        }
    }
}

/// A call written inside another call whose row states the same request is
/// that row's call: one call, one row.
#[test]
fn a_call_inside_a_call_whose_row_states_its_request_states_no_second_row() {
    // The model states `GET /api/v1/status` at `Promise.all(` on line 7; the
    // call into the client is on line 8, inside it.
    assert_eq!(
        rows_in(blobs(), "cli", NESTED, 7),
        [as_stated("GET", "/api/v1/status", "/api/v1/status")]
    );
    assert_eq!(rows_in(blobs(), "cli", NESTED, 8), []);
}

/// Two calls on one line that reach one operation state one row.
#[test]
fn two_calls_of_one_operation_on_one_line_state_one_row() {
    assert_eq!(
        rows_in(blobs(), "cli", NESTED, 16),
        [corrected("GET", "/api/v1/status", 23)]
    );
}

/// None where the function called reaches two operations, requests a route
/// its own service writes, or only builds a function that requests.
#[test]
fn no_leaf_row_where_the_callee_reaches_two_operations_its_own_request_or_hands_back_a_reader() {
    for (line, why) in [
        (21, "`loadPair` asks for one run and for every run"),
        (27, "`localPing` reaches the CLI's own `GET /api/v1/ping`"),
        (33, "`statusReader` hands back `read` unrun"),
    ] {
        let stated: Vec<Row> = rows_in(blobs(), "cli", NESTED, line)
            .into_iter()
            .filter(|row| row.role.as_deref() == Some("wrapper_call"))
            .collect();
        assert_eq!(stated, [], "{NESTED}:{line}: {why}");
    }
}

/// A function written inside the callee that the callee itself calls is
/// what the callee runs.
#[test]
fn a_call_into_a_function_that_runs_a_reader_it_writes_states_its_request() {
    assert_eq!(
        rows_in(blobs(), "cli", NESTED, 39),
        [corrected("GET", "/api/v1/status", 23)]
    );
}

/// A JSX element renders a component; it is no call the source writes. The
/// same component called as a function states the request it reaches.
#[test]
fn a_jsx_element_states_no_row_and_a_call_of_the_component_does() {
    assert_eq!(rows_in(blobs(), "web", PAGE, 5), []);
    assert_eq!(
        rows_in(blobs(), "web", PAGE, 10),
        [corrected("GET", "/api/v1/runs", 8)]
    );
}

/// The version of the scanner that wrote main's stored copy before this pass
/// existed.
const EARLIER_SCANNER: &str = "0.0.1";

/// Put back, in a stored blob, the rows the model stated at the sites this
/// pass settles, under the version of an earlier scanner: the copy of main an
/// index holds when the scanner that wrote it did not settle them.
fn unsettle(blob: &mut serde_json::Value, stated: &[(&str, u32, &str, &str, &str)]) {
    blob["scanner_version"] = EARLIER_SCANNER.into();
    for (file, line, method, target, path) in stated {
        let site = format!("{file}:{line}");
        let call = blob["mount_graph"]["data_calls"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|call| call["file_location"] == site.as_str())
            .unwrap_or_else(|| panic!("no row at {site}"));
        let object = call.as_object_mut().unwrap();
        object.insert("method".into(), (*method).into());
        object.insert("target_url".into(), (*target).into());
        object.insert("canonical_path".into(), (*path).into());
        object.insert("resolution_source".into(), "model".into());
        object.remove("role");
        object.remove("reaches_request");
        let projected = blob["calls"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|call| call["file_path"] == site.as_str())
            .unwrap_or_else(|| panic!("no projected row at {site}"));
        projected["key"]["method"] = (*method).into();
        projected["key"]["path"] = (*path).into();
        projected["resolution_source"] = "model".into();
    }
}

/// Take out of a stored blob every row at the sites `at`, in the graph and in
/// its projection.
fn drop_rows(blob: &mut serde_json::Value, at: &[(&str, u32)]) {
    let sites: Vec<String> = at
        .iter()
        .map(|(file, line)| format!("{file}:{line}"))
        .collect();
    let before = blob["mount_graph"]["data_calls"].as_array().unwrap().len();
    blob["mount_graph"]["data_calls"]
        .as_array_mut()
        .unwrap()
        .retain(|call| {
            !sites
                .iter()
                .any(|site| call["file_location"] == site.as_str())
        });
    blob["calls"]
        .as_array_mut()
        .unwrap()
        .retain(|call| !sites.iter().any(|site| call["file_path"] == site.as_str()));
    assert!(
        blob["mount_graph"]["data_calls"].as_array().unwrap().len() < before,
        "a row stood at {sites:?}"
    );
}

/// A pull request scan compares its rows with the copy of main the index
/// holds. Where that copy was written before this pass existed, the pass
/// alone must not make the scan report anything as the pull request's doing.
#[test]
fn a_pull_request_scan_reports_nothing_as_introduced_against_a_main_copy_without_the_pass() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = committed_copy(tmp.path());
    let storage = tmp.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();

    // Main, then its stored copy as an older scanner would have written it.
    run(scan_command(&repo, &storage));
    let mut stored = read_blobs(&storage);
    assert_eq!(stored.len(), SERVICES.len(), "main is indexed");
    let (path, mut cli) = stored.remove("cli").unwrap();
    unsettle(
        &mut cli,
        &[
            (COMMANDS, 9, "GET", "/getRun", "/getRun"),
            (
                COMMANDS,
                15,
                "GET",
                "${API_URL}/api/v1/runs/${id}",
                "${API_URL}/api/v1/runs/${id}",
            ),
            (COMMANDS, 21, "GET", "/api/v1/token", "/api/v1/token"),
            (COMMANDS, 28, "GET", "/api/v1/status", "/api/v1/status"),
            (COMMANDS, 46, "POST", "/save", "/save"),
            (COMMANDS, 101, "POST", "/archive", "/archive"),
        ],
    );
    // The rows the pass states where the model stated none were not there.
    drop_rows(
        &mut cli,
        &[
            (COMMANDS, 34),
            (COMMANDS, 40),
            (COMMANDS, 53),
            (COMMANDS, 121),
            (NESTED, 16),
            (NESTED, 39),
            (COMMANDS, 171),
            (HELPERS, 6),
            (HELPERS, 27),
            (PROBED, 14),
        ],
    );
    std::fs::write(&path, serde_json::to_string(&cli).unwrap()).unwrap();
    let (path, mut web) = stored.remove("web").unwrap();
    unsettle(
        &mut web,
        &[
            (HOOKS, 9, "GET", "/v1/runs/${id}", "/v1/runs/:id"),
            (HOOKS, 14, "GET", "/api/runs", "/api/runs"),
            (HOOKS, 19, "POST", "/audit", "/audit"),
        ],
    );
    drop_rows(&mut web, &[(PAGE, 10)]);
    std::fs::write(&path, serde_json::to_string(&web).unwrap()).unwrap();
    for (_, (path, mut blob)) in stored {
        unsettle(&mut blob, &[]);
        std::fs::write(&path, serde_json::to_string(&blob).unwrap()).unwrap();
    }

    // The pull request edits one file that holds no call.
    let spinner = repo.join("apps/web/src/spinner.ts");
    let mut source = std::fs::read_to_string(&spinner).unwrap();
    source.push_str("\nexport const SPINNER_CLASS = \"busy\";\n");
    std::fs::write(&spinner, source).unwrap();
    run_git(&repo, &["commit", "-qam", "pr: name the class"]);

    let out = tmp.path().join("pr-result.json");
    let mut cmd = scan_command(&repo, &storage);
    cmd.env("CARRICK_PR_RESULT_OUT", &out)
        .env("GITHUB_EVENT_NAME", "pull_request")
        .env("GITHUB_REF", "refs/pull/7/merge");
    run(cmd);
    let result: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).expect("the PR run posts a result"))
            .unwrap();

    let findings = result["findings"].as_array().unwrap();
    let introduced: Vec<&serde_json::Value> = findings
        .iter()
        .filter(|finding| finding["on_main"] == false)
        .collect();
    assert!(
        introduced.is_empty(),
        "the pass alone introduces nothing: {introduced:#?}"
    );
    // The corrected call at `commands.ts:101` reaches a request the API
    // serves under another verb: a finding main's stored copy does not hold
    // at that site, and one the run must still not call the pull request's.
    let mismatch = findings
        .iter()
        .find(|finding| {
            finding["kind"] == "method_mismatch"
                && finding["call_sites"]
                    .as_array()
                    .is_some_and(|sites| sites.contains(&format!("{COMMANDS}:101").into()))
        })
        .unwrap_or_else(|| panic!("the corrected call is checked: {findings:#?}"));
    // Main's copy reads the files the pull request left alone differently,
    // so the run says it cannot tell (carrick#1530).
    assert_eq!(
        mismatch["on_main_unknown"], "main_index_other_scanner",
        "{mismatch:#}"
    );
    let delta = &result["delta"];
    assert!(
        delta.is_null()
            || (delta["new_endpoints"].as_array().is_none_or(Vec::is_empty)
                && delta["removed_endpoints"]
                    .as_array()
                    .is_none_or(Vec::is_empty)),
        "no route is added or removed: {delta:#}"
    );
}

/// The scanner on `repo` with the store at `storage` and `flags`, reading
/// the services the store holds, with a home of its own: what it printed.
fn rescan(repo: &Path, storage: &Path, home: &Path, flags: &[&str]) -> String {
    let _ = std::fs::remove_dir_all(home);
    std::fs::create_dir_all(home).unwrap();
    let mut cmd = scan_command(repo, storage);
    cmd.args(flags)
        .env("HOME", home)
        .env("CARRICK_NO_UPDATE_CHECK", "1");
    let output = cmd.output().expect("failed to spawn carrick");
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "the scan failed:\n{printed}");
    printed
}

/// The services a `--changed-only` scan printed as `how` (`analysed`,
/// `carried`).
fn scoped(output: &str, how: &str) -> Vec<String> {
    let mut services: Vec<String> = output
        .lines()
        .filter_map(|line| {
            let rest = &line[line.find("scope: ")? + "scope: ".len()..];
            let (service, verdict) = rest.split_once(' ')?;
            (verdict == how || verdict.starts_with(&format!("{how} (")))
                .then(|| service.to_string())
        })
        .collect();
    services.sort();
    services
}

/// The fixture scanned at A into `<tmp>/store`, then commit B, which edits
/// only `packages/cli`: no other service reads it, so `--changed-only`
/// analyses `cli` and carries the rest, among them `client`, where the
/// requests `cli`'s rows reach are written.
fn scanned_then_cli_edited(tmp: &Path) -> (PathBuf, PathBuf) {
    let repo = committed_copy(tmp);
    let storage = tmp.join("store");
    std::fs::create_dir_all(&storage).unwrap();
    rescan(&repo, &storage, &tmp.join("home"), &[]);
    let types = repo.join("packages/cli/src/types.ts");
    let mut source = std::fs::read_to_string(&types).unwrap();
    source.push_str("\n/** A run's id, as a command takes it. */\nexport type RunId = string;\n");
    std::fs::write(&types, source).unwrap();
    run_git(&repo, &["commit", "-qam", "B: name the id type"]);
    (repo, storage)
}

/// carrick#2378: a service `--changed-only` carries states what the
/// call-through pass reads of it (the definitions that hand back a request's
/// parsed body, the calls whose value is not handed back, its own call-through
/// rows), so the rows of the service analysed beside it settle exactly as a
/// full scan settles them.
#[test]
fn a_changed_only_scan_settles_the_rows_a_full_scan_settles() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (repo, storage) = scanned_then_cli_edited(tmp.path());
    let full_storage = tmp.path().join("full-store");
    copy_dir(&storage, &full_storage);

    let scoped_scan = rescan(
        &repo,
        &storage,
        &tmp.path().join("home"),
        &["--changed-only"],
    );
    assert_eq!(scoped(&scoped_scan, "analysed"), ["cli"], "{scoped_scan}");
    assert_eq!(
        scoped(&scoped_scan, "carried"),
        ["api", "client", "web"],
        "{scoped_scan}"
    );
    // The pass's own counts are not compared: a full scan also settles the
    // rows of the services the scoped one carries.
    rescan(&repo, &full_storage, &tmp.path().join("home"), &[]);

    let pinned = |mut blob: serde_json::Value| {
        blob["last_updated"] = serde_json::Value::Null;
        serde_json::to_string_pretty(&blob).unwrap()
    };
    let scoped_blobs = read_blobs(&storage);
    let full_blobs = read_blobs(&full_storage);
    assert_eq!(
        pinned(scoped_blobs["cli"].1.clone()),
        pinned(full_blobs["cli"].1.clone()),
        "the analysed service's blob"
    );
}

/// A stored blob that states no call-through facts (one written before
/// carrick#2378) is carried and read as a service with none, as every
/// carried service was read before: the scan completes, and the carried blob
/// is not given facts it was not stored with.
#[test]
fn a_carried_blob_without_call_through_facts_reads_as_one_with_none() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (repo, storage) = scanned_then_cli_edited(tmp.path());
    let (client_path, mut client) = read_blobs(&storage).remove("client").unwrap();
    assert!(
        client.get("call_through").is_some(),
        "the client states its facts: {:?}",
        client
            .as_object()
            .map(|blob| blob.keys().collect::<Vec<_>>())
    );
    client.as_object_mut().unwrap().remove("call_through");
    std::fs::write(&client_path, serde_json::to_string(&client).unwrap()).unwrap();

    let scan = rescan(
        &repo,
        &storage,
        &tmp.path().join("home"),
        &["--changed-only"],
    );
    assert_eq!(scoped(&scan, "carried"), ["api", "client", "web"], "{scan}");
    let carried: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&client_path).unwrap()).unwrap();
    assert!(
        carried.get("call_through").is_none(),
        "a carried blob states no facts it was not stored with"
    );
}
