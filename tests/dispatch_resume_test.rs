//! Dispatch and resume, end to end and offline (carrick#1229).
//!
//! The two halves are one seam and the test drives both of them over the same
//! fixture: the scan builds its prompts and hands them over, and a second scan
//! is given answers named by the ids the first produced and joins them without
//! asking the model anything.
//!
//! What it pins:
//!
//! * a row's `id` is `sha256(body)` — the bytes after the guidance prefix, the
//!   same slice the cloud's analysis cache hashes for its key. The digest in
//!   `analysis_job`'s own test was computed by the cloud's `analysis_cache.js`
//!   over a shared fixture; this proves the scanner's real prompts are named
//!   the same way;
//! * the guidance block is carried ONCE for every row that names it, which is
//!   the whole of the bundle's size argument;
//! * a collected answer reaches the index — an endpoint no mock and no
//!   deterministic pass would ever state appears in the joined rows, so it can
//!   only have come from the bundle;
//! * an answer whose body has changed since the hand-off is not replayed. That
//!   is the join being content-addressed rather than path-keyed, which is what
//!   lets a resume run on a dirty tree, at a later commit, and on a machine
//!   that never ran the dispatch.

use carrick::agent_service::AgentService;
use carrick::agents::file_orchestrator::FileOrchestrator;
use carrick::agents::framework_guidance_agent::{
    FrameworkGuidance, PatternExample, ProtocolGuidance,
};
use carrick::analysis_channel;
use carrick::analysis_job::{ANSWERS_SCHEMA, body_id};
use carrick::framework_detector::DetectionResult;
use carrick::operation::Protocol;
use serial_test::serial;
use std::io::Write;
use std::path::{Path, PathBuf};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/llm-mocked-api")
}

/// Guidance with an id, which is what a dispatch requires: without one the
/// cloud keys the whole message and the block this bundle carries once would
/// be paid for once per file.
fn guidance() -> FrameworkGuidance {
    FrameworkGuidance {
        mount_patterns: vec![PatternExample {
            pattern: ".use(".to_string(),
            description: "Mount middleware or router".to_string(),
            framework: "express".to_string(),
        }],
        endpoint_patterns: vec![PatternExample {
            pattern: ".get(".to_string(),
            description: "GET endpoint".to_string(),
            framework: "express".to_string(),
        }],
        middleware_patterns: vec![],
        data_fetching_patterns: vec![PatternExample {
            pattern: "fetch(".to_string(),
            description: "Fetch API call".to_string(),
            framework: "native".to_string(),
        }],
        triage_hints: String::new(),
        parsing_notes: String::new(),
        guidance_key: Some("a".repeat(64)),
    }
}

fn detection() -> DetectionResult {
    DetectionResult {
        frameworks: vec!["express".to_string()],
        data_fetchers: vec!["fetch".to_string()],
        messaging_clients: vec![],
        notes: String::new(),
    }
}

fn files(root: &Path) -> Vec<PathBuf> {
    vec![
        root.join("src/index.ts"),
        root.join("src/routes/users.ts"),
        root.join("src/client.ts"),
    ]
}

async fn analyze(root: &Path) -> carrick::agents::file_orchestrator::FileCentricAnalysisResult {
    FileOrchestrator::new(AgentService::new())
        .analyze_files(
            &files(root),
            &std::collections::HashMap::new(),
            &ProtocolGuidance::from([(Protocol::Http, guidance())]),
            &detection(),
            root,
            root,
            &[],
            &Default::default(),
            &Default::default(),
            &carrick::url_normalizer::UrlNormalizer::default_permissive(),
            None,
        )
        .await
        .expect("the analysis ran")
}

/// One answer bundle, gzipped as the cloud sends it.
fn answer_bundle(answers: &[(String, String)], into: &Path) -> PathBuf {
    let mut text = format!(
        "{}\n",
        serde_json::json!({"schema": ANSWERS_SCHEMA, "complete": true})
    );
    for (id, body) in answers {
        text.push_str(&format!(
            "{}\n",
            serde_json::json!({"id": id, "text": body, "cached": true})
        ));
    }
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(text.as_bytes()).expect("gzip");
    let path = into.join("answers.ndjson.gz");
    std::fs::write(&path, encoder.finish().expect("gzip")).expect("write the answers");
    path
}

/// An answer no deterministic pass and no mock would produce, so a row
/// carrying it can only have come from the bundle.
fn collected_answer(line: usize, candidate_id: &str) -> String {
    serde_json::json!({
        "mounts": [],
        "endpoints": [{
            "candidate_id": candidate_id,
            "line_number": line,
            "owner_node": "app",
            "method": "GET",
            "path": "/only-from-the-collected-answer",
            "handler_name": "anonymous",
            "pattern_matched": ".get(",
            "payload_expression_text": null,
            "payload_expression_line": null,
            "response_expression_text": null,
            "response_expression_line": null,
            "primary_type_symbol": null,
            "type_import_source": null
        }],
        "data_calls": []
    })
    .to_string()
}

#[tokio::test]
#[serial]
async fn a_dispatched_scan_names_every_prompt_by_its_body_and_a_resume_joins_the_answers_back() {
    let root = fixture_root();
    let temp = tempfile::tempdir().expect("temp dir");
    // SAFETY: serial test; env vars are process-global.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::remove_var(analysis_channel::ANSWERS_ENV);
    }

    // --- dispatch -----------------------------------------------------------
    analysis_channel::begin_dispatch();
    let dispatched = analyze(&root).await;
    let collected = analysis_channel::take().expect("the run was dispatching");

    assert!(
        collected.degraded.is_empty(),
        "guidance with an id can be carried: {:?}",
        collected.degraded
    );
    assert!(
        collected.rows.len() >= 2,
        "every file that would have been dispatched built a prompt, got {}",
        collected.rows.len()
    );
    assert!(
        dispatched.file_results.is_empty() && dispatched.raw_model_results.is_empty(),
        "a dispatched run states no rows: its product is the bundle"
    );

    for row in &collected.rows {
        assert_eq!(
            row.id,
            body_id(&row.body),
            "the id names the body bytes and nothing else"
        );
        assert!(
            !row.body.contains("Mount middleware or router"),
            "the guidance block is carried once in the header, not in every row"
        );
        assert_eq!(row.guidance_key, "a".repeat(64));
    }
    assert_eq!(
        collected.guidance.len(),
        1,
        "one block for every row that names it"
    );
    assert_eq!(
        collected.schemas.len(),
        1,
        "one response schema, carried once"
    );
    let block = collected
        .guidance
        .values()
        .next()
        .expect("the block is in the header");
    assert!(
        block.contains("Mount middleware or router"),
        "and it is the rendered block, verbatim"
    );

    // --- resume -------------------------------------------------------------
    // One collected answer, for the file whose rows the assertions below read.
    // The candidate id comes from the prompt itself, because a row the model
    // invents at a span no candidate holds is dropped by the candidate gate —
    // as it should be, collected or not.
    let users = collected
        .rows
        .iter()
        .find(|row| {
            row.body
                .contains("### FILE CONTENT (Path: src/routes/users.ts)")
        })
        .expect("users.ts built a prompt");
    let (line, candidate_id) = first_candidate(&users.body);
    let answers = answer_bundle(
        &[(users.id.clone(), collected_answer(line, &candidate_id))],
        temp.path(),
    );
    // SAFETY: serial test.
    unsafe { std::env::set_var(analysis_channel::ANSWERS_ENV, &answers) };

    let resumed = analyze(&root).await;
    // Read off the RAW model answers rather than the joined rows: this is the
    // seam under test — the answer entering phase 3 as the model's answer —
    // and reading it here keeps the assertion clear of the precedence rules
    // that decide what the join then does with it.
    let raw = resumed
        .raw_model_results
        .iter()
        .find(|(path, _)| path.ends_with("users.ts"))
        .map(|(_, rows)| rows)
        .expect("users.ts has a model answer");
    assert!(
        raw.endpoints
            .iter()
            .any(|endpoint| endpoint.path == "/only-from-the-collected-answer"),
        "the collected answer is this file's model answer; got {:?}",
        raw.endpoints.iter().map(|e| &e.path).collect::<Vec<_>>()
    );
    assert_eq!(
        resumed.stats.files_model_dispatched,
        collected.rows.len() - 1,
        "every file but the one with a collected answer went to the model"
    );

    // --- the join is content, not a path ------------------------------------
    // The same answer under the id of a body one byte different. It names
    // nothing this scan built, so nothing is replayed from it — which is the
    // property that lets a resume run on an edited tree without inheriting an
    // answer to a question the file no longer asks.
    let stale_dir = temp.path().join("stale");
    std::fs::create_dir_all(&stale_dir).expect("temp dir");
    let stale = answer_bundle(
        &[(
            body_id(&format!("{} ", users.body)),
            collected_answer(line, &candidate_id),
        )],
        &stale_dir,
    );
    // SAFETY: serial test.
    unsafe { std::env::set_var(analysis_channel::ANSWERS_ENV, &stale) };
    let missed = analyze(&root).await;
    let raw = missed
        .raw_model_results
        .iter()
        .find(|(path, _)| path.ends_with("users.ts"))
        .map(|(_, rows)| rows)
        .expect("users.ts has a model answer");
    assert!(
        !raw.endpoints
            .iter()
            .any(|endpoint| endpoint.path == "/only-from-the-collected-answer"),
        "an answer for a body this scan did not build is not this scan's answer"
    );
    assert_eq!(
        missed.stats.files_model_dispatched,
        collected.rows.len(),
        "so every file was analysed here"
    );

    // SAFETY: serial test; leave the process as it was found.
    unsafe {
        std::env::remove_var(analysis_channel::ANSWERS_ENV);
        std::env::remove_var("CARRICK_MOCK_ALL");
    }
}

/// The first candidate the prompt names, as `(line, candidate_id)`.
///
/// The prompt lists them as `- Candidate <id>: Line N (span a-b) …`, which is
/// the contract between the prompt and the model's `candidate_id`. A collected
/// answer has to speak the same language or the candidate gate drops it — as
/// it drops any row at a span no candidate holds, collected or not.
fn first_candidate(body: &str) -> (usize, String) {
    for line in body.lines() {
        let Some(rest) = line.trim().strip_prefix("- Candidate ") else {
            continue;
        };
        // The id itself contains colons (`span:98-121`), so the separator is
        // the whole `: Line ` between the id and the line number.
        let Some((id, tail)) = rest.split_once(": Line ") else {
            continue;
        };
        let Some(number) = tail
            .split_whitespace()
            .next()
            .and_then(|word| word.parse::<usize>().ok())
        else {
            continue;
        };
        return (number, id.to_string());
    }
    panic!("the prompt named no candidate:\n{body}");
}
