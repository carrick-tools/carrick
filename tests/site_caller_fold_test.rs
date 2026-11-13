//! carrick#2614: a reader row at a call of an in-repo function folds onto
//! the request that function states.
//!
//! Drives the real scanner binary offline over `tests/fixtures/caller-fold/`
//! as the Action runs it, with the type sidecar live.
//!
//! `src/client.ts` declares `login` and `logout`, each a call of a local
//! `send(path, body)` that hands its arguments to `http.post` on a package
//! the library store labels an HTTP client. Each path is decided at the
//! `send` call that writes it (`/api/login` in `login`; `/api/logout` and
//! `/api/audit` in `logout`), so the fact layer places each request there.
//! `src/page.ts` awaits `login(...)` and `logout(...)`. The reader's cassette
//! states a request at all five lines.
//!
//! Answer key (a row "stands" whatever pass states it; at the callee lines
//! the scan's own rows keep the site today):
//!
//! | line | reader states | the scan |
//! |---|---|---|
//! | `src/client.ts:12` | `POST /api/login` | a row |
//! | `src/client.ts:16`, `:17` | `POST /api/logout`, `POST /api/audit` | a row each |
//! | `src/page.ts:4` (calls `login`) | `POST /api/login` | folded: no row |
//! | `src/page.ts:9` (calls `logout`) | `DELETE /api/session` | withdrawn, saved unread: the callee states other operations and writes nothing of this one (carrick#4206) |

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
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

/// A fixture, copied and committed as a repo of its own.
fn fixture_repo(tmp: &Path, name: &str) -> PathBuf {
    let repo = tmp.join(name);
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
        &repo,
    );
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

/// The blob one offline scan of `repo` stores.
fn scan(repo: &Path) -> serde_json::Value {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env_remove("CARRICK_ALLOW_MISSING_TYPES");
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
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("failed to spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut blobs: Vec<serde_json::Value> = fs::read_dir(storage.path())
        .expect("storage dir")
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .map(|entry| {
            serde_json::from_slice(&fs::read(entry.path()).expect("read blob")).expect("parse blob")
        })
        .collect();
    assert_eq!(blobs.len(), 1, "one service, one blob");
    blobs.remove(0)
}

/// Every call row as `(file:line, METHOD path, resolution source)`, sorted.
fn calls(blob: &serde_json::Value) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = blob["calls"]
        .as_array()
        .expect("calls")
        .iter()
        .map(|row| {
            (
                row["file_path"].as_str().unwrap_or_default().to_string(),
                format!(
                    "{} {}",
                    row["key"]["method"].as_str().unwrap_or_default(),
                    row["key"]["path"].as_str().unwrap_or_default()
                ),
                row["resolution_source"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    out.sort();
    out
}

fn at<'r>(rows: &'r [(String, String, String)], place: &str) -> Vec<&'r (String, String, String)> {
    rows.iter().filter(|row| row.0 == place).collect()
}

#[test]
fn a_reader_row_at_a_call_of_a_function_that_states_the_request_folds_onto_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture_repo(tmp.path(), "caller-fold");

    // The reader's cassette states a row at each of these five lines, the
    // caller's copy of `/api/login` among them.
    let places = [
        "src/client.ts:12",
        "src/client.ts:16",
        "src/client.ts:17",
        "src/page.ts:4",
        "src/page.ts:9",
    ];
    let on = calls(&scan(&repo));
    // The request stands once, at the call that decides its path.
    let login = at(&on, "src/client.ts:12");
    assert_eq!(login.len(), 1, "{on:#?}");
    assert_eq!(login[0].1, "POST /api/login", "{on:#?}");
    assert_ne!(login[0].2, "model", "{on:#?}");
    // The caller's copy of the same request is folded onto it.
    assert!(at(&on, "src/page.ts:4").is_empty(), "{on:#?}");
    // A caller whose reader row names another operation than any its callee
    // states: the callee reaches two requests and writes nothing of
    // `/api/session`, so the reader's row is withdrawn and the site saved
    // unread (carrick#4206).
    assert!(at(&on, "src/page.ts:9").is_empty(), "{on:#?}");
    assert_eq!(at(&on, "src/client.ts:16").len(), 1, "{on:#?}");
    assert_eq!(at(&on, "src/client.ts:17").len(), 1, "{on:#?}");
    assert_eq!(on.len(), places.len() - 2, "{on:#?}");
}

/// Every graph call row's `file:line`: the rows the cross-service pass reads.
fn graph_calls(blob: &serde_json::Value) -> Vec<String> {
    let mut out: Vec<String> = blob["mount_graph"]["data_calls"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    row["file_location"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// carrick#2704 (RULED): a reader row at a call of a function in the same
/// service folds onto the fact the function's request is, whatever key the
/// reader gave it.
///
/// `tests/fixtures/caller-fold-disagree/`: `src/client.ts` declares four
/// functions, each writing its request inside a function it hands a
/// tracing package (`span(name, () => http.get(…))`), so no discovered
/// chain runs from their callers: `listItems` (`GET /api/items`),
/// `replaceItem` (`PUT /api/items/${id}`), `saveItem` (`POST /api/items`)
/// and `fetchPath(path)`, whose path is its caller's parameter. In
/// `src/screens.ts` a hook calls `listItems` from a query package's
/// callback, and three functions call the others. The reader's cassette
/// names each caller's request wrongly: `GET /api/list`, `PATCH`, `POST
/// /api/save`, and `GET /api/all`. Its `tag` row at `src/screens.ts:25`
/// echoes an id no candidate lists (`unlisted:tag`): the scan lists only the
/// `listItems()` site on that line, so the mock's `@line:25` placeholder
/// would name that call (`agent_service::substitute_candidate_placeholders`)
/// and state the `tag` row as a second reading of `listItems()`, which the
/// call-through pass then corrects to `GET /api/items`.
///
/// | line | reader states | the scan |
/// |---|---|---|
/// | `src/client.ts:9`, `:13`, `:17` | the request | the fact row |
/// | `src/screens.ts:5` (hook, `listItems`) | `GET /api/list` | folded: no row |
/// | `src/screens.ts:9` (`replaceItem`) | `PATCH /api/items/:id` | folded: no row |
/// | `src/screens.ts:13` (`saveItem`) | `POST /api/save` | folded: no row |
/// | `src/screens.ts:17` (`fetchPath`) | `GET /api/all` | withdrawn, saved unread: no fact states the request and the callee writes nothing of it (carrick#4206) |
/// | `src/screens.ts:25` (`listItems` and `tag` on one line) | `GET /api/list`, `GET /api/tag` | the `tag` row stands |
#[test]
fn a_reader_row_that_disagrees_with_the_fact_its_callee_states_folds_onto_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture_repo(tmp.path(), "caller-fold-disagree");

    let blob = scan(&repo);
    let on = calls(&blob);
    // Each request stands once, at the call that writes it, as a fact.
    for (place, operation) in [
        ("src/client.ts:9", "GET /api/items"),
        ("src/client.ts:13", "PUT /api/items/:id"),
        ("src/client.ts:17", "POST /api/items"),
    ] {
        let rows = at(&on, place);
        assert_eq!(rows.len(), 1, "{place}: {on:#?}");
        assert_eq!(rows[0].1, operation, "{place}: {on:#?}");
        assert_ne!(rows[0].2, "model", "{place}: {on:#?}");
    }
    // The callers' misreadings of those requests are gone, from the calls
    // and from the graph the cross-service pass reads.
    let graph = graph_calls(&blob);
    for place in ["src/screens.ts:5", "src/screens.ts:9", "src/screens.ts:13"] {
        assert!(at(&on, place).is_empty(), "{place}: {on:#?}");
        assert!(!graph.contains(&place.to_string()), "{place}: {graph:#?}");
    }
    // A caller whose request no fact states: `fetchPath(path)` is handed its
    // path by a parameter and writes nothing of `/api/all`, so the reader's
    // row is withdrawn and the site saved unread (carrick#4206).
    assert!(at(&on, "src/screens.ts:17").is_empty(), "{on:#?}");
    // A caller line that holds a second call (`src/screens.ts:25` calls
    // `listItems` and the unrelated `tag`) is not the callee's request alone:
    // the reader's row for the other call stands.
    let shared = at(&on, "src/screens.ts:25");
    assert!(shared.iter().any(|row| row.1 == "GET /api/tag"), "{on:#?}");
}

/// carrick#2793: a request written in a member of an exported object literal
/// folds its callers' rows exactly as one written in a function declaration
/// does.
///
/// `tests/fixtures/caller-fold-object/`: `src/client.ts` exports `itemsApi`,
/// an object literal whose members each write their request inside a
/// function handed to a tracing package: `replace` is an arrow
/// (`PUT /api/items/${id}`), `save` a method (`POST /api/items`). The call
/// graph keys them `itemsApi.replace` and `itemsApi.save`. `src/screens.ts`
/// calls each; the reader's cassette names each caller's request wrongly.
///
/// | line | reader states | the scan |
/// |---|---|---|
/// | `src/client.ts:10`, `:13` | the request | the fact row |
/// | `src/screens.ts:4` (`itemsApi.replace`) | `PATCH /api/items/:id` | folded: no row |
/// | `src/screens.ts:8` (`itemsApi.save`) | `POST /api/save` | folded: no row |
#[test]
fn a_reader_row_at_a_call_of_an_object_literal_member_folds_onto_its_fact() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture_repo(tmp.path(), "caller-fold-object");

    let blob = scan(&repo);
    let on = calls(&blob);
    // Each request stands once, at the call that writes it, as a fact.
    for (place, operation) in [
        ("src/client.ts:10", "PUT /api/items/:id"),
        ("src/client.ts:13", "POST /api/items"),
    ] {
        let rows = at(&on, place);
        assert_eq!(rows.len(), 1, "{place}: {on:#?}");
        assert_eq!(rows[0].1, operation, "{place}: {on:#?}");
        assert_ne!(rows[0].2, "model", "{place}: {on:#?}");
    }
    // The callers' misreadings are gone, from the calls and from the graph
    // the cross-service pass reads.
    let graph = graph_calls(&blob);
    for place in ["src/screens.ts:4", "src/screens.ts:8"] {
        assert!(at(&on, place).is_empty(), "{place}: {on:#?}");
        assert!(!graph.contains(&place.to_string()), "{place}: {graph:#?}");
    }
    assert_eq!(on.len(), 2, "{on:#?}");
}
