//! A GraphQL root field written as a method decorator is a producer
//! (carrick#3043, SB1): `@Query("x")`, `@Mutation(() => T, { name: "y" })` or
//! a bare `@Query()` on a method of a resolver class, where the store labels
//! the decorator export with a `receive` op in the `decorate` form, protocol
//! `graphql`, whose method is the root operation type and whose name is the
//! first of the call's name arguments, then the decorated method's own name.
//!
//! Drives the real scanner binary on `.`, over a committed copy of
//! `tests/fixtures/graphql-decorated-resolvers/`, offline, with the
//! fixture's store answer replayed. The fixture's README is the answer key.
//!
//! Protocols: GraphQL query, mutation and subscription root fields.
//! Object-type fields (a field-resolver decorator) state no operation by
//! design; builder-call schemas, the producer's response type and federation
//! entry points are gaps.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const RESOLVERS: &str = "services/backend/src/resolvers.ts";
const PRINTED_RESOLVERS: &str = "services/printed/src/resolvers.ts";
const PRINTED_SDL: &str = "services/printed/schema.graphql";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graphql-decorated-resolvers")
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

/// One GraphQL row as the blob holds it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Row {
    side: String,
    key: String,
    file_path: String,
    source: Option<String>,
}

fn row(side: &str, key: &str, file_path: &str, source: &str) -> Row {
    Row {
        side: side.to_string(),
        key: key.to_string(),
        file_path: file_path.to_string(),
        source: Some(source.to_string()),
    }
}

/// The GraphQL rows of each service of a scan of the fixture, sorted.
fn scan() -> BTreeMap<String, Vec<Row>> {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir(), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
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
        .current_dir(&repo)
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
    let mut services = BTreeMap::new();
    for entry in std::fs::read_dir(&storage).expect("storage dir") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let blob: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read blob")).expect("parse blob");
        let mut rows = Vec::new();
        for side in ["endpoints", "calls"] {
            for entry in blob[side].as_array().expect("blob side is an array") {
                let key = &entry["key"];
                if key["protocol"].as_str() != Some("graphql") {
                    continue;
                }
                rows.push(Row {
                    side: side.to_string(),
                    key: format!(
                        "{}|{}",
                        key["kind"].as_str().unwrap(),
                        key["field"].as_str().unwrap()
                    ),
                    file_path: entry["file_path"].as_str().expect("file_path").to_string(),
                    source: entry
                        .get("resolution_source")
                        .and_then(|source| source.as_str())
                        .map(str::to_string),
                });
            }
        }
        rows.sort();
        services.insert(blob["service_name"].as_str().unwrap().to_string(), rows);
    }
    assert_eq!(
        services.keys().map(String::as_str).collect::<Vec<_>>(),
        ["backend", "consumer", "printed"],
        "every service uploads one blob"
    );
    services
}

fn scanned() -> &'static BTreeMap<String, Vec<Row>> {
    static ROWS: OnceLock<BTreeMap<String, Vec<Row>>> = OnceLock::new();
    ROWS.get_or_init(scan)
}

/// A method decorated with a labelled root-field decorator is the producer
/// of the field the call names (argument 0, then the options `name`), else
/// the method's own name, at the method's name, of the root type the op's
/// `method` says.
#[test]
fn decorated_resolver_methods_are_graphql_producers() {
    let backend = &scanned()["backend"];
    for (key, line) in [
        ("query|me", 14),
        ("mutation|renameThing", 19),
        ("query|byString", 24),
        ("subscription|events", 29),
        ("query|everything", 34),
        ("query|described", 39),
        ("query|viaOptions", 44),
        ("query|plain", 49),
    ] {
        let want = row(
            "endpoints",
            key,
            &format!("{RESOLVERS}:{line}"),
            "library_claim",
        );
        assert!(backend.contains(&want), "{want:?} in {backend:#?}");
    }
}

/// A field resolver states no operation, a name option the source does not
/// write out states none, and a parameter decorator of the same name as a
/// root-field decorator is no site: the producers are exactly the eight.
#[test]
fn a_resolve_field_or_a_parameter_decorator_states_no_operation() {
    let backend = &scanned()["backend"];
    let producers: Vec<&Row> = backend
        .iter()
        .filter(|row| row.side == "endpoints")
        .collect();
    assert_eq!(producers.len(), 8, "{producers:#?}");
    for row in &producers {
        assert_eq!(row.source.as_deref(), Some("library_claim"), "{row:?}");
        for unwanted in ["owner", "hidden", "find", "rename", "lookup", "optioned"] {
            assert!(
                !row.key.ends_with(&format!("|{unwanted}")),
                "{unwanted} is no field: {row:?}"
            );
        }
    }
}

/// A document in another service selecting a decorated field has the field's
/// key, so the two meet.
#[test]
fn a_document_selecting_a_decorated_field_pairs_with_it() {
    let services = scanned();
    let backend = &services["backend"];
    let consumer = &services["consumer"];
    for key in [
        "query|me",
        "mutation|renameThing",
        "query|byString",
        "subscription|events",
        "query|everything",
        "query|viaOptions",
    ] {
        assert!(
            backend
                .iter()
                .any(|row| row.side == "endpoints" && row.key == key),
            "no producer {key} in {backend:#?}"
        );
        assert!(
            consumer
                .iter()
                .any(|row| row.side == "calls" && row.key == key),
            "no call {key} in {consumer:#?}"
        );
    }
}

/// A field the printed schema also serves is one row: the schema's. A
/// decorated field the schema does not state is the library's.
#[test]
fn a_field_the_printed_schema_also_serves_is_written_once() {
    let printed = &scanned()["printed"];
    let producers: Vec<&Row> = printed
        .iter()
        .filter(|row| row.side == "endpoints")
        .collect();
    let printed_field = row(
        "endpoints",
        "query|printedField",
        &format!("{PRINTED_SDL}:2"),
        "graphql_schema",
    );
    let only_decorated = row(
        "endpoints",
        "query|onlyDecorated",
        &format!("{PRINTED_RESOLVERS}:10"),
        "library_claim",
    );
    assert_eq!(producers.len(), 2, "{producers:#?}");
    assert!(printed.contains(&printed_field), "{printed:#?}");
    assert!(printed.contains(&only_decorated), "{printed:#?}");
}
