//! A file sent to the model keeps the rows the fact layer reads when the
//! model's answer omits them (the replay of a PR sends an empty answer for
//! each edited file).
//!
//! Drives the real scanner binary on `.` over a committed copy of
//! `tests/fixtures/pr-empty-answer/repo`, offline, once with an answer that
//! states each literal route and once with an answer that states nothing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pr-empty-answer")
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

const NOTHING: &str = r#"{"mounts":[],"endpoints":[],"data_calls":[]}"#;

fn endpoint(id: &str, line: i64, owner: &str, path: &str) -> String {
    format!(
        r#"{{"candidate_id":"{id}","line_number":{line},
        "owner_node":"{owner}","method":"GET","path":"{path}","handler_name":"anonymous",
        "pattern_matched":"{owner}.get(","payload_expression_text":null,
        "payload_expression_line":null,"response_expression_text":"\"ok\"",
        "response_expression_line":{line},"emission_style":"imperative-send",
        "primary_type_symbol":null,"type_import_source":null}}"#
    )
}

fn stating(line: i64, owner: &str, path: &str) -> String {
    answer(&[endpoint(&format!("@line:{line}"), line, owner, path)], "")
}

fn answer(endpoints: &[String], data_calls: &str) -> String {
    format!(
        r#"{{"mounts":[],"endpoints":[{}],"data_calls":[{data_calls}]}}"#,
        endpoints.join(",")
    )
}

/// The service's routes, each as `file:line METHOD path source`.
fn routes(answers: &[(&str, String)]) -> BTreeSet<String> {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir().join("repo"), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let storage = tmp.path().join("storage");
    let home = tmp.path().join("home");
    let cassettes = tmp.path().join("cassettes");
    std::fs::create_dir_all(&storage).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(cassettes.join("library-claims")).unwrap();
    std::fs::create_dir_all(cassettes.join("analyze-file")).unwrap();
    std::fs::copy(
        fixture_dir().join("library-claims.json"),
        cassettes.join("library-claims/default.json"),
    )
    .unwrap();
    for (stem, body) in answers {
        std::fs::write(cassettes.join(format!("analyze-file/{stem}.json")), body).unwrap();
    }

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
        .env("CARRICK_ALLOW_UNPREPARED", "1");
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
    let output = cmd.output().expect("spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let blobs: Vec<serde_json::Value> = std::fs::read_dir(&storage)
        .expect("the store reads")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| serde_json::from_slice(&std::fs::read(path).ok()?).ok())
        .collect();
    assert_eq!(blobs.len(), 1, "the root service uploads one blob");
    blobs[0]["mount_graph"]["endpoints"]
        .as_array()
        .expect("endpoints")
        .iter()
        .map(|row| {
            format!(
                "{} {} {} {}",
                row["file_location"].as_str().unwrap_or_default(),
                row["method"].as_str().unwrap_or_default(),
                row["full_path"].as_str().unwrap_or_default(),
                row["resolution_source"].as_str().unwrap_or_default(),
            )
        })
        .collect()
}

/// The fact layer reads `app.get("/widgets")` and `server.get("/gadgets")`;
/// an answer that omits them must not take the rows away, and the rows keep
/// a source that is not the model's.
#[test]
fn a_sent_file_keeps_the_rows_a_deterministic_pass_reads() {
    let stated = routes(&[
        ("app", stating(10, "app", "/widgets")),
        ("typed", stating(7, "server", "/gadgets")),
    ]);
    let omitted = routes(&[("app", NOTHING.to_string()), ("typed", NOTHING.to_string())]);
    eprintln!("stated: {stated:#?}\nomitted: {omitted:#?}");
    for row in ["src/app.ts:10 GET /widgets", "src/typed.ts:7 GET /gadgets"] {
        assert!(
            omitted
                .iter()
                .any(|r| r.starts_with(row) && !r.ends_with("llm")),
            "the omitting answer lost {row}: {omitted:#?}"
        );
    }
}

/// The same file also registers routes in a loop over a tuple array and holds
/// a data call the model is asked about. The answer that states the loop
/// routes and the call adds rows; the answer that omits them still leaves the
/// rows the fact layer reads, and its loop routes cost no row of a wrong kind.
#[test]
fn a_sent_file_with_a_loop_and_a_data_call_keeps_its_deterministic_rows() {
    let ledger = r#"{"candidate_id":"@line:27","line_number":27,"target":"https://${host}/ledger","method":"GET","pattern_matched":"fetch(","call_expression_text":"fetch(`https://${host}/ledger`)","call_expression_line":27,"payload_expression_text":null,"payload_expression_line":null}"#;
    let stated_app = answer(
        &[
            endpoint("@line:10", 10, "app", "/widgets"),
            endpoint("@line:22:a", 22, "app", "/orders"),
            endpoint("@line:22:b", 22, "app", "/invoices"),
        ],
        ledger,
    );
    let stated = routes(&[
        ("app", stated_app),
        ("typed", stating(7, "server", "/gadgets")),
    ]);
    let omitted = routes(&[("app", NOTHING.to_string()), ("typed", NOTHING.to_string())]);
    eprintln!("stated: {stated:#?}\nomitted: {omitted:#?}");
    for row in ["src/app.ts:10 GET /widgets", "src/typed.ts:7 GET /gadgets"] {
        assert!(
            stated.iter().any(|r| r.starts_with(row)),
            "the stating answer lost {row}: {stated:#?}"
        );
        assert!(
            omitted
                .iter()
                .any(|r| r.starts_with(row) && !r.ends_with(" model")),
            "the omitting answer lost {row}: {omitted:#?}"
        );
    }
}
