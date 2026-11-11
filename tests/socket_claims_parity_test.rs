//! The fact layer states every socket row the socket.io package-name rules
//! (the socket pass's S1 arms) state (carrick#1670 parity, part 1; D3a).
//!
//! Each fixture here is a twin of one the old pass's tests read: the same
//! sources, with the socket package's import specifier swapped for an invented
//! package (`@fixture/rt-server`, `@fixture/rt-client`) whose declarations are
//! hand-written in the vendored `node_modules`. The old pass keys on the
//! specifier `socket.io` / `socket.io-client`, so it reads nothing here: every
//! row below is the fact layer's (library claims, labelled from the store's
//! answer for the real package, mirrored in each `__llm__/library-claims`).
//! Each test asserts the keys and files its old test asserts
//! (`socket_extraction_test.rs`, `socket_class_field_test.rs`,
//! `socket_type_alias_test.rs`, `socket_namespace_test.rs`), so the old test
//! and its twin say the same thing today and the twin is the regression test
//! once the old rules go. The two cross-service corpora keep their sources and
//! `expected.json` byte-identical: the test copies their socket repos, adds the
//! stub lockfiles and the library-claims cassette from
//! `tests/fixtures/socket-claims-xrepo/`, and asserts `expected.json`'s
//! `socket_events`.
//!
//! Drives the real scanner binary on `.`, offline, over a committed copy of
//! each fixture, with the fixture's cassettes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
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

/// One socket row as the blob holds it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Row {
    service: String,
    /// `endpoints` (a listener) or `calls` (an emitter).
    side: String,
    /// `socket|<DIRECTION>|<event>`, the spelling the old tests assert.
    key: String,
    /// The file, without its line.
    file: String,
    source: Option<String>,
}

/// The socket rows of a scan of `repo` (a directory already copied and
/// committed), one scan of the whole directory as the Action runs it.
fn scan_dir(repo: &Path) -> Vec<Row> {
    let tmp = tempfile::tempdir().unwrap();
    let (storage, home, cache) = (
        tmp.path().join("storage"),
        tmp.path().join("home"),
        tmp.path().join("cache"),
    );
    for dir in [&storage, &home, &cache] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(repo)
        .env("HOME", &home)
        .env("CARRICK_LOCAL_STORAGE_DIR", &storage)
        .env("CARRICK_CACHE_DIR", &cache)
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
    let output = cmd.output().expect("spawn carrick");
    assert!(
        output.status.success(),
        "the scan failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut rows = Vec::new();
    for entry in std::fs::read_dir(&storage).expect("storage dir") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let service = path.file_stem().unwrap().to_string_lossy().to_string();
        let blob: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read blob")).expect("parse blob");
        for side in ["endpoints", "calls"] {
            for entry in blob[side].as_array().expect("blob side is an array") {
                let key = &entry["key"];
                if key["protocol"].as_str() != Some("socket") {
                    continue;
                }
                let direction = key["direction"].as_str().unwrap();
                let file_path = entry["file_path"].as_str().expect("file_path");
                rows.push(Row {
                    service: service.clone(),
                    side: side.to_string(),
                    key: format!(
                        "socket|{}|{}",
                        direction.replace("_to_", "->").to_uppercase(),
                        key["event"].as_str().unwrap()
                    ),
                    file: file_path
                        .rsplit_once(':')
                        .map_or(file_path, |(file, _)| file)
                        .to_string(),
                    source: entry
                        .get("resolution_source")
                        .and_then(|source| source.as_str())
                        .map(str::to_string),
                });
            }
        }
    }
    rows.sort();
    rows
}

/// A committed copy of `tests/fixtures/<name>` (plus the overlay of the
/// corpora), in a fresh temp dir; scanned as one repo.
fn scan_fixture(name: &str) -> Vec<Row> {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("workspace");
    copy_dir(&fixtures().join(name), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    scan_dir(&repo)
}

/// A cross-service corpus repo: the corpus's copy of `<corpus>/<repo>`, with
/// the overlay's files (lockfile, cassettes) added, scanned as its own repo.
fn scan_corpus_repo(corpus: &str, repo_name: &str) -> Vec<Row> {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join(repo_name);
    copy_dir(
        &fixtures().join(format!("xrepo-{corpus}")).join(repo_name),
        &repo,
    );
    let overlay = fixtures()
        .join("socket-claims-xrepo")
        .join(corpus)
        .join(repo_name);
    copy_dir(&overlay, &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let mut rows = scan_dir(&repo);
    // One repo, one service: the service is the repo.
    for row in &mut rows {
        row.service = repo_name.to_string();
    }
    rows
}

fn has(rows: &[Row], side: &str, key: &str, file_suffix: &str) -> bool {
    rows.iter()
        .any(|row| row.side == side && row.key == key && row.file.ends_with(file_suffix))
}

/// A call and an endpoint of `key` in two different services.
fn pairs(rows: &[Row], key: &str) -> bool {
    rows.iter().any(|call| {
        call.side == "calls"
            && call.key == key
            && rows.iter().any(|endpoint| {
                endpoint.side == "endpoints"
                    && endpoint.key == key
                    && endpoint.service != call.service
            })
    })
}

/// The two rows the fact layer states where the old rules state none (see
/// `the_fact_layer_states_two_rows_the_old_rules_skip`): a client's lifecycle
/// `connect` on a socket made by a call-form factory, and an event whose name is
/// a template over a runtime value.
fn is_extra(row: &Row) -> bool {
    row.key.ends_with("|connect") || row.key.contains("${")
}

fn keys(rows: &[Row], side: &str) -> Vec<String> {
    let mut keys: Vec<String> = rows
        .iter()
        .filter(|row| row.side == side && !is_extra(row))
        .map(|row| row.key.clone())
        .collect();
    keys.sort();
    keys
}

/// Every socket row of an invented-package fixture is the fact layer's: the
/// socket pass reads no import of an invented package.
fn assert_all_fact_rows(rows: &[Row]) {
    for row in rows {
        assert_eq!(
            row.source.as_deref(),
            Some("library_claim"),
            "{row:?} is a fact-layer row: {rows:#?}"
        );
    }
}

/// `socket_extraction_test.rs`: the directional contract of a socket server and
/// client, import-anchored, reserved events filtered, dynamic names skipped.
#[test]
fn a_socket_service_states_its_directional_contract() {
    let rows = scan_fixture("socket-service-claims");
    assert_all_fact_rows(&rows);
    assert_eq!(
        keys(&rows, "endpoints"),
        vec![
            // server listens for client->server messages
            "socket|CLIENT->SERVER|chat:message",
            "socket|CLIENT->SERVER|typing",
            // client listens for server->client broadcasts
            "socket|SERVER->CLIENT|chat:broadcast",
        ],
        "listeners carry the direction of messages they receive: {rows:#?}"
    );
    assert_eq!(
        keys(&rows, "calls"),
        vec![
            "socket|CLIENT->SERVER|chat:message",
            "socket|CLIENT->SERVER|presence:ping",
            "socket|SERVER->CLIENT|chat:broadcast",
            "socket|SERVER->CLIENT|user:left",
            "socket|SERVER->CLIENT|user:typing",
        ],
        "emitters carry the direction of messages they send; dynamic names are skipped: {rows:#?}"
    );
}

/// Two rows the old rules do not state and the fact layer does (both read on
/// the first run of this file against main; D3 does not lose them, it keeps
/// them): `connect` on a client socket made by `io(url)` at module level, where
/// the label reserves it (the old rules drop `RESERVED_EVENTS`), and an emit
/// whose name is a template over a runtime value (the old rules skip dynamic
/// names). Pinned so a fix flips this test; neither row is a lost row.
#[test]
fn the_fact_layer_states_two_rows_the_old_rules_skip() {
    let rows = scan_fixture("socket-service-claims");
    assert!(
        has(
            &rows,
            "endpoints",
            "socket|SERVER->CLIENT|connect",
            "client.ts"
        ),
        "{rows:#?}"
    );
    assert!(
        has(
            &rows,
            "calls",
            "socket|CLIENT->SERVER|metrics:${process.env.SHARD}",
            "client.ts"
        ),
        "{rows:#?}"
    );
}

/// `socket_class_field_test.rs`: a client socket parked on a class field, a
/// per-connection socket on a constructor parameter property.
#[test]
fn a_socket_held_on_a_class_field_is_stated_on_both_sides() {
    let rows = scan_fixture("socket-class-field-claims");
    assert_all_fact_rows(&rows);
    for event in ["run:subscribe", "run:unsubscribe"] {
        let key = format!("socket|CLIENT->SERVER|{event}");
        assert!(has(&rows, "calls", &key, "session.ts"), "{key}: {rows:#?}");
        assert!(
            has(&rows, "endpoints", &key, "gateway.ts"),
            "{key}: {rows:#?}"
        );
        assert!(pairs(&rows, &key), "{key} meets across services: {rows:#?}");
    }
    let notify = "socket|SERVER->CLIENT|run:notify";
    assert!(has(&rows, "calls", notify, "gateway.ts"), "{rows:#?}");
    assert!(has(&rows, "endpoints", notify, "session.ts"), "{rows:#?}");
    assert!(pairs(&rows, notify), "{rows:#?}");
    assert!(
        !rows
            .iter()
            .any(|row| row.key.ends_with("|connect") || row.key.ends_with("|disconnect")),
        "reserved lifecycle events stay out of the contract: {rows:#?}"
    );
}

/// `socket_type_alias_test.rs`: sockets declared through a type alias of the
/// package's socket type, one of them imported from the declaring module.
#[test]
fn a_socket_declared_through_a_type_alias_is_stated() {
    let rows = scan_fixture("socket-type-alias-claims");
    assert_all_fact_rows(&rows);
    for event in ["run:start", "run:stop"] {
        let key = format!("socket|CLIENT->SERVER|{event}");
        assert!(
            has(&rows, "calls", &key, "controller.ts"),
            "{key}: {rows:#?}"
        );
        assert!(
            has(&rows, "endpoints", &key, "workloadServer.ts"),
            "{key}: {rows:#?}"
        );
        assert!(pairs(&rows, &key), "{key} meets: {rows:#?}");
    }
    // `notifier.ts` imports the alias and never names the socket type.
    let notify = "socket|SERVER->CLIENT|run:notify";
    assert!(has(&rows, "endpoints", notify, "notifier.ts"), "{rows:#?}");
    assert!(
        has(&rows, "calls", notify, "workloadServer.ts"),
        "{rows:#?}"
    );
    assert!(pairs(&rows, notify), "{rows:#?}");
}

/// `socket_namespace_test.rs`: a handler registered on a namespace carved off
/// the server, beside a default-namespace handler in the same file.
#[test]
fn a_namespaced_handler_is_stated_and_meets_its_publishers() {
    let rows = scan_fixture("socket-namespace-claims");
    assert_all_fact_rows(&rows);
    for event in ["run:subscribe", "run:unsubscribe"] {
        let key = format!("socket|CLIENT->SERVER|{event}");
        assert!(
            has(&rows, "endpoints", &key, "gateway.ts"),
            "{key}: {rows:#?}"
        );
        assert!(has(&rows, "calls", &key, "session.ts"), "{key}: {rows:#?}");
        assert!(pairs(&rows, &key), "{key} meets: {rows:#?}");
    }
    assert!(
        has(
            &rows,
            "endpoints",
            "socket|CLIENT->SERVER|session:hello",
            "gateway.ts"
        ),
        "the default-namespace handler is kept too: {rows:#?}"
    );
}

/// The `socket_events` the corpus's `expected.json` files hold for `repos`,
/// stated by the fact layer, and each pair across the two repos.
fn assert_corpus_socket_events(corpus: &str, repos: [&str; 2]) {
    let mut rows = Vec::new();
    let mut expected: Vec<(String, String, String)> = Vec::new();
    for repo in repos {
        rows.extend(scan_corpus_repo(corpus, repo));
        let answer: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                fixtures()
                    .join(format!("xrepo-{corpus}"))
                    .join(repo)
                    .join("expected.json"),
            )
            .expect("read expected.json"),
        )
        .expect("parse expected.json");
        for event in answer["socket_events"].as_array().expect("socket_events") {
            let side = match event["role"].as_str().unwrap() {
                "producer" => "endpoints",
                "consumer" => "calls",
                other => panic!("unknown role {other}"),
            };
            expected.push((
                side.to_string(),
                event["key"].as_str().unwrap().to_string(),
                event["source"].as_str().unwrap().to_string(),
            ));
        }
    }
    assert!(!expected.is_empty(), "the corpus answers socket events");
    for (side, key, source) in &expected {
        assert!(
            has(&rows, side, key, source),
            "{side} {key} in {source}: {rows:#?}"
        );
    }
    let keys: BTreeSet<&String> = expected.iter().map(|(_, key, _)| key).collect();
    for key in keys {
        assert!(pairs(&rows, key), "{key} meets across the repos: {rows:#?}");
    }
}

/// `xrepo-corpus-1`: a socket server emit and a client listener.
#[test]
fn corpus_one_states_its_socket_events() {
    assert_corpus_socket_events("corpus-1", ["payments-svc", "web-frontend"]);
}

/// `xrepo-corpus-3`: a socket server listener and a client emitter.
#[test]
fn corpus_three_states_its_socket_events() {
    assert_corpus_socket_events("corpus-3", ["support-desk", "storefront-web"]);
}
