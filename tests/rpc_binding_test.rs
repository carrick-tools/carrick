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
    /// What the scan printed: the report a person reads.
    stdout: String,
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
    scans_with(answer, runs, false)
}

/// [`scans`]; with `json`, one scan that prints the analysis as JSON
/// (`CARRICK_OUTPUT_JSON`, the eval projection) and stores no blob.
fn scans_with(answer: &serde_json::Value, runs: usize, json: bool) -> Vec<Scan> {
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
    if json {
        cmd.env("CARRICK_OUTPUT_JSON", "1");
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
        assert!(json || !blobs.is_empty(), "the scan stored a blob");
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
        out.push(Scan {
            blobs,
            fallbacks,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        });
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

/// The unread sites `scan` saves for `service` with `reason`, as
/// `file:line reason`.
fn unread_sites(scan: &Scan, service: &str, reason: &str) -> Vec<String> {
    let mut sites = Vec::new();
    for blob in &scan.blobs {
        if blob["service_name"].as_str() != Some(service) {
            continue;
        }
        for site in blob["unread_sites"].as_array().cloned().unwrap_or_default() {
            if site["reason"].as_str() == Some(reason) {
                sites.push(format!(
                    "{}:{} {reason}",
                    site["file"].as_str().unwrap_or_default(),
                    site["line"]
                ));
            }
        }
    }
    sites.sort();
    sites
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

/// The type entries `scan` states for `service`'s producer rows, as
/// `line Kind METHOD "path" => definition` (the expanded definition, or `-`
/// where none was published).
fn producer_types(scan: &Scan, service: &str) -> Vec<String> {
    let mut entries = Vec::new();
    for blob in &scan.blobs {
        if blob["service_name"].as_str() != Some(service) {
            continue;
        }
        for entry in blob["type_manifest"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            if entry["role"].as_str() != Some("producer") {
                continue;
            }
            entries.push(format!(
                "{} {} {} {:?} => {}",
                entry["line_number"],
                entry["type_kind"].as_str().unwrap_or_default(),
                entry["method"].as_str().unwrap_or_default(),
                entry["path"].as_str().unwrap_or_default(),
                entry["expanded_definition"]
                    .as_str()
                    .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
                    .unwrap_or_else(|| "-".to_string()),
            ));
        }
    }
    entries.sort();
    entries
}

/// carrick#4909 T1: each procedure row carries its request and response
/// types, read off the variable's own call signature, on the cold scan and
/// the incremental one alike. The request is what a caller sends (the
/// label's `payload` key, `data`, of parameter 0: the validator's input),
/// whatever the method; the response is the awaited return. The model's
/// `POST /saveNote` row was replaced, and no entry of its stands.
#[test]
fn each_procedure_row_is_typed_from_its_call_signature() {
    for (run, scan) in declared().iter().enumerate() {
        let entries = producer_types(scan, "declared");
        assert_eq!(
            entries,
            vec![
                r#"10 request GET "" => undefined"#.to_string(),
                r#"10 response GET "" => number"#.to_string(),
                r#"3 request GET "" => undefined"#.to_string(),
                r#"3 response GET "" => { id: number; }[]"#.to_string(),
                r#"5 request POST "" => { body: string; }"#.to_string(),
                r#"5 response POST "" => { ok: boolean; }"#.to_string(),
            ],
            "scan {run}: {entries:#?}"
        );
    }
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
/// no row is written there, and the definition is saved as an unread site
/// with the instance walk's reason (`unread`). The definitions written
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
    // The definitions' own file (its callers' saves are the next test's).
    let saved: Vec<String> = unread_sites(scan, "declared", "unread")
        .into_iter()
        .filter(|site| site.starts_with(&format!("{NOTES}:")))
        .collect();
    assert_eq!(saved, vec![format!("{NOTES}:5 unread")], "{saved:#?}");
    // With the pass-through claims nothing there is unread.
    assert_eq!(
        unread_sites(&declared()[0], "declared", "unread"),
        Vec::<String>::new()
    );
}

/// Two definitions one declaration holds (an object of them under
/// `grouped`) would share one key: neither states a row, and each is saved
/// as an unread site with `procedure_router_ambiguous`.
#[test]
fn two_definitions_under_one_declaration_are_saved_ambiguous() {
    let scan = &declared()[0];
    let rows = producer_rows(scan, "declared");
    assert!(!rows.iter().any(|row| row.contains("grouped")), "{rows:#?}");
    let grouped = "services/declared/src/grouped.ts";
    assert_eq!(
        unread_sites(scan, "declared", "procedure_router_ambiguous"),
        vec![
            format!("{grouped}:4 procedure_router_ambiguous"),
            format!("{grouped}:5 procedure_router_ambiguous"),
        ]
    );
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

/// The call rows `scan` states for `service`, as `file:line METHOD "path"
/// source => key @ file#binding`.
fn call_rows(scan: &Scan, service: &str) -> Vec<String> {
    let mut rows = Vec::new();
    for blob in &scan.blobs {
        if blob["service_name"].as_str() != Some(service) {
            continue;
        }
        for row in blob["mount_graph"]["data_calls"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let procedure = &row["procedure"];
            rows.push(format!(
                "{} {} {:?} {} => {} @ {}#{}",
                row["file_location"].as_str().unwrap_or_default(),
                row["method"].as_str().unwrap_or_default(),
                row["target_url"].as_str().unwrap_or_default(),
                row["resolution_source"].as_str().unwrap_or_default(),
                procedure["path"].as_str().unwrap_or("-"),
                procedure["router"]["file"].as_str().unwrap_or_default(),
                procedure["router"]["binding"].as_str().unwrap_or_default(),
            ));
        }
    }
    rows.sort();
    rows
}

/// The unread sites `scan` saves for `service` in `file` (a path suffix),
/// as `file:line reason`.
fn unread_in(scan: &Scan, service: &str, file: &str) -> Vec<String> {
    let mut sites = Vec::new();
    for blob in &scan.blobs {
        if blob["service_name"].as_str() != Some(service) {
            continue;
        }
        for site in blob["unread_sites"].as_array().cloned().unwrap_or_default() {
            let at = site["file"].as_str().unwrap_or_default();
            if at.ends_with(file) {
                sites.push(format!("{at}:{} {}", site["line"], site["reason"]));
            }
        }
    }
    sites.sort();
    sites
}

/// The answer key's consumer rows: each call of a procedure's variable,
/// directly or through the wrapper's instance (called, or handed on and
/// called), states one request row keyed by that procedure's own key, on
/// the method its definition states, at the call. Nothing else is a row.
#[test]
fn each_call_of_a_procedure_states_one_request_row() {
    let rows = call_rows(&declared()[0], "declared");
    let at = |file: &str, line: u32, method: &str, name: &str| {
        format!(
            r#"services/declared/src/{file}:{line} {method} "" request_summary => {name} @ {NOTES}#"#
        )
    };
    assert_eq!(
        rows,
        vec![
            at("direct.ts", 4, "GET", "listNotes"),
            // Called inside a callback the alias is also handed beside.
            at("handed.ts", 18, "POST", "saveNote"),
            at("handed.ts", 8, "POST", "saveNote"),
            at("page.tsx", 6, "POST", "saveNote"),
        ],
        "{rows:#?}"
    );
}

/// Each call pairs with its procedure in the one service (a pair whose
/// two ends are one service, plan D12), and the procedure nothing calls
/// (`unused`) is the one endpoint without a consumer.
#[test]
fn the_calls_pair_with_their_procedures_in_one_service() {
    let scan = scans_with(&cassette("library-claims-declared.json"), 1, true).remove(0);
    let printed = &scan.stdout;
    let analysis: serde_json::Value =
        serde_json::from_str(&printed[printed.find('{').expect("the JSON analysis")..])
            .expect("the analysis is JSON");
    let mut pairs: Vec<String> = analysis["cross_repo_matches"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|pair| pair["consumer_repo"] == "declared" || pair["producer_repo"] == "declared")
        .map(|pair| {
            format!(
                "{} {} <- {} {}",
                pair["producer_repo"].as_str().unwrap_or_default(),
                pair["producer_key"].as_str().unwrap_or_default(),
                pair["consumer_repo"].as_str().unwrap_or_default(),
                pair["consumer_key"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            "declared http|GET| <- declared http|GET|".to_string(),
            "declared http|POST| <- declared http|POST|".to_string(),
            "declared http|POST| <- declared http|POST|".to_string(),
            "declared http|POST| <- declared http|POST|".to_string(),
        ],
        "one pair per call: direct.ts, handed.ts twice, page.tsx"
    );
    let report = &declared()[0].stdout;
    let unconsumed: Vec<&str> = report
        .lines()
        .filter(|line| line.ends_with("| `declared` |"))
        .collect();
    assert_eq!(unconsumed, vec!["| `GET` | `` | `declared` |"], "{report}");
}

/// The wrapper's instance handed on and never called here (`value.ts`):
/// nothing says where the procedure is called, so the maker call is saved
/// with that reason. A maker call whose instance is called states no site
/// of its own (the row is at the call), and a plain local function
/// (`control.ts`) states nothing at all.
#[test]
fn a_wrapper_never_called_here_is_saved_and_a_plain_function_states_nothing() {
    let scan = &declared()[0];
    assert_eq!(
        unread_in(scan, "declared", "src/value.ts"),
        vec!["services/declared/src/value.ts:9 \"detached_unplaced\"".to_string()]
    );
    assert_eq!(
        unread_in(scan, "declared", "src/page.tsx"),
        Vec::<String>::new()
    );
    assert_eq!(
        unread_in(scan, "declared", "src/handed.ts"),
        Vec::<String>::new()
    );
    assert_eq!(
        unread_in(scan, "declared", "src/direct.ts"),
        Vec::<String>::new()
    );
    assert_eq!(
        unread_in(scan, "declared", "src/control.ts"),
        Vec::<String>::new()
    );
    assert!(
        !call_rows(scan, "declared")
            .iter()
            .any(|row| row.contains("control.ts")),
        "{:#?}",
        call_rows(scan, "declared")
    );
}

/// A call of a procedure whose definition states no method this scan reads
/// (`saveNote` without the pass-through claims) states no row on a guessed
/// method: each such call is saved `unread`, and the procedure whose method
/// is read (`listNotes`) keeps its call's row.
#[test]
fn a_call_of_a_procedure_with_an_unread_method_is_saved_not_guessed() {
    let scan = unchained();
    assert_eq!(
        call_rows(scan, "declared"),
        vec![format!(
            r#"services/declared/src/direct.ts:4 GET "" request_summary => listNotes @ {NOTES}#"#
        )]
    );
    let saved: Vec<String> = unread_sites(scan, "declared", "unread")
        .into_iter()
        .filter(|site| !site.starts_with(&format!("{NOTES}:")))
        .collect();
    assert_eq!(
        saved,
        vec![
            "services/declared/src/handed.ts:18 unread".to_string(),
            "services/declared/src/handed.ts:8 unread".to_string(),
            "services/declared/src/page.tsx:6 unread".to_string(),
        ]
    );
}

/// A service whose procedure set is empty (the re-exported maker, whose
/// definitions state nothing until carrick#4882): a wrapper's call of a
/// function declared in the repository that is no procedure of the set is
/// a plain call in place: no row, and no site saved or asked by this pass.
/// The saves left are the other readers' own, as before this pass: the
/// callback reader's `missed_inside` at the call written inside the
/// function handed to `memo`, and the value reader's `value_callee` at the
/// direct call. A row at those lines replaces each in the declared service.
#[test]
fn a_wrapper_of_no_procedure_of_the_set_states_nothing() {
    let scan = scans(&cassette("library-claims.json"), 1).remove(0);
    assert_eq!(call_rows(&scan, "reexport"), Vec::<String>::new());
    let mut saved = Vec::new();
    for file in [
        "src/page.tsx",
        "src/handed.ts",
        "src/value.ts",
        "src/direct.ts",
    ] {
        saved.extend(unread_in(&scan, "reexport", file));
    }
    assert_eq!(
        saved,
        vec![
            "services/reexport/src/handed.ts:18 \"missed_inside\"".to_string(),
            "services/reexport/src/direct.ts:4 \"value_callee\"".to_string(),
        ]
    );
}

/// The consumer rows ride no cache either: an incremental re-scan states
/// the same ones.
#[test]
fn an_incremental_rescan_states_the_same_calls() {
    let scans = declared();
    assert_eq!(
        call_rows(&scans[1], "declared"),
        call_rows(&scans[0], "declared")
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
