//! The site fold writes the graph the cross-service pass reads
//! (carrick#2795).
//!
//! Drives the real scanner binary on `.`, over a committed copy of
//! `tests/fixtures/call-through-result-label/`, with the switches every cloud
//! scan runs (site questions, position rows, fact files, receiver trace and
//! site payloads). There the CLI's calls into the client's wrappers are stated
//! at the caller by the site fold, and the client's own requests are site rows
//! too. Before the fix those fact rows sat in the blob's `calls` and nowhere
//! in `mount_graph.data_calls`, while the model's rows they replaced stayed
//! in the graph: the cross-service pass, the cloud's graph readers and the
//! local analyzer read a graph that did not hold what `calls` states.
//!
//! The pinned literals at the end are what the scan states today and must
//! keep stating: mirroring the facts moves no `calls` row and no type.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const COMMANDS: &str = "packages/cli/src/commands.ts";
const USERS: &str = "packages/client/src/users.ts";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/call-through-result-label")
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "carrick-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "carrick-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
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
    let repo = tmp.join("workspace");
    copy_dir(&fixture_dir(), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

/// The scanner on `.` from inside `repo`, offline, with the fixture's
/// cassettes, the store at `storage` and the home at `home`, with the
/// switches a cloud scan runs.
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
        )
        .env("CARRICK_SITE_QUESTIONS", "1")
        .env("CARRICK_POSITION_ROWS", "1")
        .env("CARRICK_FACT_FILES", "1")
        .env("CARRICK_RECEIVER_TRACE", "1")
        .env("CARRICK_SITE_PAYLOADS", "1");
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

/// One scan's printed output.
fn run(mut cmd: Command) -> String {
    let output = cmd.output().expect("spawn carrick");
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "the scan failed:\n{printed}");
    printed
}

type Blobs = BTreeMap<String, serde_json::Value>;

fn read_blobs(storage: &Path) -> Blobs {
    let blobs: Blobs = std::fs::read_dir(storage)
        .expect("storage dir")
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let mut blob: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
            // Peaks are measured, so they differ run to run (carrick#3083).
            blob.as_object_mut()?.remove("memory_peaks");
            Some((blob["service_name"].as_str()?.to_string(), blob))
        })
        .collect();
    assert_eq!(
        blobs.keys().map(String::as_str).collect::<Vec<_>>(),
        ["api", "cli", "client"],
        "every service uploads one blob"
    );
    blobs
}

/// The run log a scan under `home` kept.
fn run_log(home: &Path) -> String {
    std::fs::read_dir(home.join(".carrick/logs/runs"))
        .expect("the scan keeps a run log")
        .filter_map(|entry| std::fs::read_to_string(entry.ok()?.path()).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The fixture scanned once for the whole binary: the blobs and the run log.
fn scanned() -> &'static (Blobs, String) {
    static SCAN: OnceLock<(Blobs, String)> = OnceLock::new();
    SCAN.get_or_init(|| {
        let tmp = tempfile::tempdir().expect("temp dir");
        let repo = committed_copy(tmp.path());
        let storage = tmp.path().join("store");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        run(scan_command(
            &repo,
            &storage,
            &home,
            &tmp.path().join("cache"),
        ));
        (read_blobs(&storage), run_log(&home))
    })
}

/// A graph row: `(file:line, METHOD, path, source, role)`.
type GraphRow = (String, String, String, String, Option<String>);

fn graph_rows(blob: &serde_json::Value) -> Vec<GraphRow> {
    blob["mount_graph"]["data_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|call| {
            (
                call["file_location"].as_str().unwrap().to_string(),
                call["method"].as_str().unwrap().to_uppercase(),
                call["canonical_path"].as_str().unwrap().to_string(),
                call["resolution_source"].as_str().unwrap_or("").to_string(),
                call["role"].as_str().map(str::to_string),
            )
        })
        .collect()
}

/// The graph rows at `file`, by line.
fn graph_at(blob: &serde_json::Value, file: &str, line: u32) -> Vec<GraphRow> {
    let place = format!("{file}:{line}");
    graph_rows(blob)
        .into_iter()
        .filter(|row| row.0 == place)
        .collect()
}

/// The HTTP rows of `calls`: `(file:line, METHOD, path)`.
fn http_calls(blob: &serde_json::Value) -> BTreeSet<(String, String, String)> {
    blob["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|call| call["key"]["protocol"] == "http")
        .map(|call| {
            (
                call["file_path"].as_str().unwrap().to_string(),
                call["key"]["method"].as_str().unwrap().to_uppercase(),
                call["key"]["path"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn http_graph(blob: &serde_json::Value) -> BTreeSet<(String, String, String)> {
    graph_rows(blob)
        .into_iter()
        .map(|row| (row.0, row.1, row.2))
        .collect()
}

/// Asserts 1 to 3 on the blobs of one scan.
fn the_graph_holds_what_calls_states(blobs: &Blobs, scan: &str) {
    let cli = &blobs["cli"];
    // 1. The CLI's four calls into wrappers whose request is not a row of the
    // service: the fact at the caller, a call through, and no model row.
    for (line, method, path) in [
        (23, "POST", "/users"),
        (35, "GET", "/ping"),
        (53, "POST", "/users/:id/frozen"),
        (65, "POST", "/users/:id/caught"),
    ] {
        assert_eq!(
            graph_at(cli, COMMANDS, line),
            [(
                format!("{COMMANDS}:{line}"),
                method.to_string(),
                path.to_string(),
                "request_summary".to_string(),
                Some("wrapper_call".to_string()),
            )],
            "{scan}: the CLI's graph at {COMMANDS}:{line}"
        );
    }
    // 2. The client's nine requests, none a call through.
    let client_rows: Vec<(u32, GraphRow)> = graph_rows(&blobs["client"])
        .into_iter()
        .map(|row| (row.0.rsplit_once(':').unwrap().1.parse().unwrap(), row))
        .collect();
    assert_eq!(
        client_rows
            .iter()
            .map(|(line, _)| *line)
            .collect::<Vec<_>>(),
        [17, 23, 28, 33, 38, 44, 49, 54, 59],
        "{scan}: the client's graph rows: {client_rows:?}"
    );
    for (line, row) in &client_rows {
        assert_eq!(row.0, format!("{USERS}:{line}"), "{scan}");
        assert_eq!(row.3, "request_summary", "{scan}: {row:?}");
        assert_eq!(row.4, None, "{scan}: a request is no call through: {row:?}");
    }
    // 3. Every service: the graph and `calls` state the same HTTP rows, and
    // no service's graph names a file of another.
    for (service, blob) in blobs {
        assert_eq!(
            http_graph(blob),
            http_calls(blob),
            "{scan}: {service}'s graph and calls"
        );
    }
    for (service, directory) in [("cli", "packages/cli/"), ("client", "packages/client/")] {
        for row in graph_rows(&blobs[service]) {
            assert!(
                row.0.starts_with(directory),
                "{scan}: {service}'s graph row {row:?} sits in another service's files"
            );
        }
    }
}

#[test]
fn the_graph_holds_the_fact_rows_the_site_fold_stated() {
    the_graph_holds_what_calls_states(&scanned().0, "a full scan");
}

/// The model's four rows at the CLI's calls were replaced by facts, so the
/// cross-service pass skips and counts them instead of reading them as calls
/// that reach no request.
#[test]
fn call_through_counts_the_rows_a_fact_replaced() {
    let log = &scanned().1;
    assert!(
        log.contains("left alone: 0 reach no request row, 0 reach several operations, 0 reach a request only the model states (carrick#1146); 4 skipped because a fact site row replaced the model's row (carrick#2795)"),
        "the settle summary in the run log:\n{}",
        log.lines()
            .filter(|line| line.contains("Call-through rows settled"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    // Mirroring a fact asks the store and the compiler nothing new.
    for line in [
        "result shapes: verified results checks=2 not_reached=0 results=8",
        "result shapes: verified results checks=3 not_reached=0 results=9",
    ] {
        assert!(log.contains(line), "`{line}` in the run log");
    }
}

/// Mirroring the facts moves no row of `calls` and no type: what the scan
/// states at the CLI's ten calls, and the response type kept at each.
#[test]
fn the_calls_and_the_types_do_not_move() {
    let cli = &scanned().0["cli"];
    let calls: Vec<(String, String, String, String)> = cli["calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|call| {
            (
                call["file_path"].as_str().unwrap().to_string(),
                call["key"]["method"].as_str().unwrap().to_string(),
                call["key"]["path"].as_str().unwrap().to_string(),
                call["resolution_source"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let expected: Vec<(String, String, String, String)> = [
        (17, "GET", "/users/:id", "imported_member"),
        (29, "GET", "/users/:id/status", "imported_member"),
        (41, "GET", "/users/:id/page", "imported_member"),
        (47, "GET", "/users/:id/model", "imported_member"),
        (59, "GET", "/users/:id/then", "imported_member"),
        (74, "GET", "/users/:id", "imported_member"),
        (23, "POST", "/users", "request_summary"),
        (35, "GET", "/ping", "request_summary"),
        (53, "POST", "/users/:id/frozen", "request_summary"),
        (65, "POST", "/users/:id/caught", "request_summary"),
    ]
    .into_iter()
    .map(|(line, method, path, source)| {
        (
            format!("{COMMANDS}:{line}"),
            method.to_string(),
            path.to_string(),
            source.to_string(),
        )
    })
    .collect();
    assert_eq!(calls, expected);

    let mut responses: Vec<(u64, String)> = cli["type_manifest"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["role"] == "consumer" && entry["type_kind"] == "response")
        .map(|entry| {
            assert_eq!(entry["file_path"], COMMANDS);
            (
                entry["line_number"].as_u64().unwrap(),
                entry["expanded_definition"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    responses.sort();
    let user = "{ id: string; }";
    let want: Vec<(u64, String)> = [
        (17, user),
        (23, user),
        (29, "number"),
        (35, "number"),
        (41, "{ id: string; }[]"),
        (47, user),
        (53, user),
        (59, user),
        (65, "null | { id: string; }"),
        (74, user),
    ]
    .into_iter()
    .map(|(line, definition)| (line, definition.to_string()))
    .collect();
    assert_eq!(responses, want);
}

/// The fixture scanned at A into `<tmp>/store`, then commit B, which edits
/// only `packages/cli`: no other service reads it, so `--changed-only`
/// analyses `cli` and carries `client` and `api`.
fn scanned_then_cli_edited(tmp: &Path) -> (PathBuf, PathBuf) {
    let repo = committed_copy(tmp);
    let storage = tmp.join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let home = tmp.join("home");
    std::fs::create_dir_all(&home).unwrap();
    run(scan_command(&repo, &storage, &home, &tmp.join("cache")));
    let types = repo.join("packages/cli/src/types.ts");
    let mut source = std::fs::read_to_string(&types).unwrap();
    source.push_str("\n/** A run's id, as a command takes it. */\nexport type RunId = string;\n");
    std::fs::write(&types, source).unwrap();
    run_git(&repo, &["commit", "-qam", "B: name the id type"]);
    (repo, storage)
}

/// An incremental scan and a `--changed-only` scan write the same graph a full
/// scan does: the fold runs on every path that writes a blob.
#[test]
fn the_graph_holds_the_facts_on_a_rescan_and_a_changed_only_scan() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (repo, storage) = scanned_then_cli_edited(tmp.path());
    let plain_storage = tmp.path().join("plain-store");
    copy_dir(&storage, &plain_storage);

    let rescan = |storage: &Path, flags: &[&str]| {
        let home = tmp.path().join("rescan-home");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let mut cmd = scan_command(&repo, storage, &home, &tmp.path().join("cache"));
        cmd.args(flags);
        run(cmd)
    };
    let scoped = rescan(&storage, &["--changed-only"]);
    assert!(
        scoped.contains("scope: cli analysed"),
        "the CLI is analysed and the rest carried:\n{scoped}"
    );
    rescan(&plain_storage, &[]);

    let scoped_blobs = read_blobs(&storage);
    let plain_blobs = read_blobs(&plain_storage);
    the_graph_holds_what_calls_states(&scoped_blobs, "a changed-only scan");
    the_graph_holds_what_calls_states(&plain_blobs, "a plain rescan");
    let pinned = |blob: &serde_json::Value| {
        let mut blob = blob.clone();
        blob["last_updated"] = serde_json::Value::Null;
        serde_json::to_string_pretty(&blob).unwrap()
    };
    assert_eq!(
        pinned(&scoped_blobs["cli"]),
        pinned(&plain_blobs["cli"]),
        "the analysed service's blob"
    );
}
