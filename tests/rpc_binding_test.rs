//! Procedures named by the variable that holds them (contract amendment 13,
//! `name: { bound: "binding" }`, carrick#4909 and carrick#4919): each
//! definition states one producer row, keyed by its procedure, served at a
//! path the code never writes (`""`), at the line its maker call starts on,
//! with the method that call writes. A model row of its method at that line
//! is replaced, not kept beside it. The answer key is
//! `tests/fixtures/rpc-binding/README.md`.
//!
//! Drives the real scanner binary on `.`, as the Action runs it, offline,
//! with the fixture's store answer replayed as the library store's. Needs the
//! built compiler engine (the verifier).
//!
//! Protocols: RPC procedures over HTTP transport. GraphQL, sockets, pub/sub
//! and queues are untouched.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const FIXTURE: &str = "rpc-binding";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(FIXTURE)
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

/// Every blob one scan stored, and each service's fallbacks line (the
/// counts the scan states about itself), by service name.
struct Scan {
    blobs: Vec<serde_json::Value>,
    fallbacks: BTreeMap<String, serde_json::Value>,
}

/// The fixture's store answer `cassette`, as JSON.
fn cassette(name: &str) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(fixture_dir().join(name)).unwrap()).unwrap()
}

/// `runs` scans of one committed copy of the fixture, sharing one store and
/// cache, with `answer` replayed as the store's answer and the fixture's
/// reader answers as the model's: every scan after the first takes the
/// incremental path.
fn scans(answer: &serde_json::Value, runs: usize) -> Vec<Scan> {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir().join("repo"), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let cassettes = tmp.path().join("cassettes");
    std::fs::create_dir_all(cassettes.join("library-claims")).unwrap();
    std::fs::write(
        cassettes.join("library-claims/default.json"),
        serde_json::to_vec(answer).unwrap(),
    )
    .unwrap();
    copy_dir(
        &fixture_dir().join("__llm__/analyze-file"),
        &cassettes.join("analyze-file"),
    );
    let storage = tmp.path().join("storage");
    std::fs::create_dir_all(&storage).unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(&repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", &storage)
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", tmp.path().join("cache"))
        .env("HOME", &home)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        // The fallbacks lines, stated to a parent that reads them.
        .env("CARRICK_PROGRESS", "1");
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
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let mut out = Vec::new();
    for run in 0..runs {
        // A later scan reads the store back, so it finds the first's blob.
        if run == 1 {
            cmd.env_remove("CARRICK_LOCAL_STORAGE_ISOLATE");
        }
        let output = cmd.output().expect("spawn carrick");
        assert!(
            output.status.success(),
            "scan failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let mut blobs = Vec::new();
        for entry in std::fs::read_dir(&storage).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "json") {
                blobs.push(serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap());
            }
        }
        assert!(!blobs.is_empty(), "the scan stored a blob");
        // The runner keeps the last line each service states.
        let mut fallbacks = BTreeMap::new();
        for line in String::from_utf8_lossy(&output.stderr).lines() {
            let Some(payload) = line.trim_start().strip_prefix("@carrick-fallbacks ") else {
                continue;
            };
            let line: serde_json::Value =
                serde_json::from_str(payload).expect("a fallbacks line is JSON");
            if let Some(service) = line["service"].as_str() {
                fallbacks.insert(service.to_string(), line.clone());
            }
        }
        out.push(Scan { blobs, fallbacks });
    }
    // Each later scan took the incremental path: its run log says so.
    let incremental = std::fs::read_dir(home.join(".carrick/logs/runs"))
        .map(|logs| {
            logs.filter(|log| {
                std::fs::read_to_string(log.as_ref().unwrap().path())
                    .unwrap_or_default()
                    .contains("Incremental analysis complete")
            })
            .count()
        })
        .unwrap_or_default();
    assert!(incremental >= runs - 1, "a re-scan took the cold path");
    out
}

/// The producer rows `blobs` state in the service `service`'s mount graph,
/// as `file:line METHOD "full_path" "path" source owner => key @ file#binding`.
fn producer_rows(scan: &Scan, service: &str) -> Vec<String> {
    let mut rows = Vec::new();
    for blob in &scan.blobs {
        if blob["service_name"].as_str() != Some(service) {
            continue;
        }
        for row in blob["mount_graph"]["endpoints"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let procedure = match row.get("procedure") {
                Some(procedure) => {
                    assert!(
                        !procedure["router"]["repo"]
                            .as_str()
                            .unwrap_or_default()
                            .is_empty(),
                        "a procedure names the repo of its router: {row}"
                    );
                    format!(
                        "{} @ {}#{}",
                        procedure["path"].as_str().unwrap_or_default(),
                        procedure["router"]["file"].as_str().unwrap_or_default(),
                        procedure["router"]["binding"].as_str().unwrap_or_default(),
                    )
                }
                None => "-".to_string(),
            };
            rows.push(format!(
                "{} {} {:?} {:?} {} {} => {}",
                row["file_location"].as_str().unwrap_or_default(),
                row["method"].as_str().unwrap_or_default(),
                row["full_path"].as_str().unwrap_or_default(),
                row["path"].as_str().unwrap_or_default(),
                row["resolution_source"].as_str().unwrap_or_default(),
                row["owner"].as_str().unwrap_or_default(),
                procedure,
            ));
        }
    }
    rows.sort();
    rows
}

/// The flat operation rows `scan` states for `service`, as `file:line
/// METHOD "path" => key`.
fn flat_rows(scan: &Scan, service: &str) -> Vec<String> {
    let mut rows = Vec::new();
    for blob in &scan.blobs {
        if blob["service_name"].as_str() != Some(service) {
            continue;
        }
        for row in blob["endpoints"].as_array().cloned().unwrap_or_default() {
            rows.push(format!(
                "{} {} {:?} => {}",
                row["file_path"].as_str().unwrap_or_default(),
                row["key"]["method"].as_str().unwrap_or_default(),
                row["key"]["path"].as_str().unwrap_or_default(),
                row["procedure"]["path"].as_str().unwrap_or("-"),
            ));
        }
    }
    rows.sort();
    rows
}

/// The count `key` of `service`'s fallbacks line in `scan`, 0 when unstated.
fn count(scan: &Scan, service: &str, key: &str) -> u64 {
    scan.fallbacks
        .get(service)
        .and_then(|line| line[key].as_u64())
        .unwrap_or(0)
}

/// The declared service's two scans (the second incremental), with the
/// store answering the package that declares the maker.
fn declared() -> &'static Vec<Scan> {
    static SCANS: OnceLock<Vec<Scan>> = OnceLock::new();
    SCANS.get_or_init(|| scans(&cassette("library-claims-declared.json"), 2))
}

/// The declared service's scan with no pass-through claim on the builder's
/// members (`use`, `validate`): the instance walk stops at a chained call,
/// so the maker call's method is unread there.
fn unchained() -> &'static Scan {
    static SCAN: OnceLock<Scan> = OnceLock::new();
    SCAN.get_or_init(|| {
        let mut answer = cassette("library-claims-declared.json");
        for package in answer["library_claims"].as_array_mut().unwrap() {
            for export in package["exports"].as_array_mut().unwrap() {
                export["scopes"] = serde_json::json!([]);
            }
        }
        scans(&answer, 1).remove(0)
    })
}

const NOTES: &str = "services/declared/src/notes.functions.ts";

/// The answer key's three producer rows: one per definition, keyed by the
/// variable that holds it and its file, router binding `""` at the module's
/// top level, at the line its maker call starts on, with the method that
/// call writes (the label's default where it writes none). No other row.
#[test]
fn each_definition_states_one_procedure_row() {
    let rows = producer_rows(&declared()[0], "declared");
    assert_eq!(
        rows,
        vec![
            format!(r#"{NOTES}:10 GET "" "" library_claim unused => unused @ {NOTES}#"#),
            format!(r#"{NOTES}:3 GET "" "" library_claim listNotes => listNotes @ {NOTES}#"#),
            format!(r#"{NOTES}:5 POST "" "" library_claim saveNote => saveNote @ {NOTES}#"#),
        ],
        "{rows:#?}"
    );
}

/// The index's flat rows hold the same three procedures: none folds into
/// another for sharing a method and the empty path.
#[test]
fn the_flat_rows_hold_each_procedure() {
    let rows = flat_rows(&declared()[0], "declared");
    assert_eq!(
        rows,
        vec![
            format!(r#"{NOTES}:10 GET "" => unused"#),
            format!(r#"{NOTES}:3 GET "" => listNotes"#),
            format!(r#"{NOTES}:5 POST "" => saveNote"#),
        ],
        "{rows:#?}"
    );
}

/// The model's `POST /saveNote` at the line `saveNote`'s maker call starts
/// on is replaced by the procedure's row (the trace counts one replace),
/// never kept beside it. The control scan, where the trace states no row
/// there, shows the model row joined at that line.
#[test]
fn a_model_row_at_the_definition_is_replaced_not_duplicated() {
    let scan = &declared()[0];
    let rows = producer_rows(scan, "declared");
    let at_save: Vec<&String> = rows
        .iter()
        .filter(|row| row.starts_with(&format!("{NOTES}:5 ")))
        .collect();
    assert_eq!(at_save.len(), 1, "{rows:#?}");
    assert!(
        !rows.iter().any(|row| row.contains("/saveNote")),
        "{rows:#?}"
    );
    assert_eq!(count(scan, "declared", "routes_trace_replaced"), 1);
    assert!(
        producer_rows(unchained(), "declared").contains(&format!(
            r#"{NOTES}:5 POST "/saveNote" "/saveNote" model saveNote => -"#
        )),
        "{:#?}",
        producer_rows(unchained(), "declared")
    );
}

/// Without a pass-through claim on the builder's chained members the maker
/// call is not reached from `saveNote`'s op call, so its method is unread:
/// no row is written there and the site is counted. The definitions written
/// directly on the maker's result keep theirs.
#[test]
fn a_chained_definition_reads_its_method_only_through_pass_through_members() {
    let scan = unchained();
    assert_eq!(
        producer_rows(scan, "declared"),
        vec![
            format!(r#"{NOTES}:10 GET "" "" library_claim unused => unused @ {NOTES}#"#),
            format!(r#"{NOTES}:3 GET "" "" library_claim listNotes => listNotes @ {NOTES}#"#),
            format!(r#"{NOTES}:5 POST "/saveNote" "/saveNote" model saveNote => -"#),
        ]
    );
    assert_eq!(
        count(scan, "declared", "routes_untraced_method_unstated"),
        1
    );
}

/// Two definitions one declaration holds (an object of them under
/// `grouped`) would share one key: neither states a row.
#[test]
fn two_definitions_under_one_declaration_state_no_row() {
    let rows = producer_rows(&declared()[0], "declared");
    assert!(!rows.iter().any(|row| row.contains("grouped")), "{rows:#?}");
}

/// A definition no model row stands at (`unused`, no caller) is written,
/// not refused for stating no path.
#[test]
fn a_definition_with_no_model_row_is_written() {
    let rows = producer_rows(&declared()[0], "declared");
    assert!(
        rows.iter()
            .any(|row| row.starts_with(&format!("{NOTES}:10 GET \"\" "))),
        "{rows:#?}"
    );
}

/// The rows ride no cache: an incremental re-scan states the same rows
/// from the cached model answers (step 0c).
#[test]
fn an_incremental_rescan_states_the_same_rows() {
    let scans = declared();
    assert_eq!(
        producer_rows(&scans[1], "declared"),
        producer_rows(&scans[0], "declared")
    );
}

/// The maker re-exported from a package the app does not declare (the
/// fixture as the plan lays it out): the store answers the package the app
/// imports, and the call's declaration is in its dependency, so no claim is
/// asked until carrick#4882 (carrick#4878) lands. Then it states the
/// declared service's rows.
#[test]
#[ignore = "needs carrick#4882: a re-exported declaration's claims are not asked"]
fn a_re_exported_maker_states_the_same_rows() {
    let scans = scans(&cassette("library-claims.json"), 1);
    let notes = "services/reexport/src/notes.functions.ts";
    assert_eq!(
        producer_rows(&scans[0], "reexport"),
        vec![
            format!(r#"{notes}:10 GET "" "" library_claim unused => unused @ {notes}#"#),
            format!(r#"{notes}:3 GET "" "" library_claim listNotes => listNotes @ {notes}#"#),
            format!(r#"{notes}:5 POST "" "" library_claim saveNote => saveNote @ {notes}#"#),
        ]
    );
}
