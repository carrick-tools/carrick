//! The bytes a dispatched job's answers are named by, pinned to a
//! checked-in list (carrick#1332).
//!
//! A dispatched row's `id` is `sha256(body)` and the cloud hands the answer
//! back under it. So the prompt body is a NAME as well as a question: a
//! release that moves one byte of it between the `dispatch` and the `resume`
//! renames every question the job answered: the stored answers match nothing
//! the resume rebuilds, so every file is analysed from the start and the whole
//! job takes as long as it did the first time (carrick#1248). Nothing failed
//! when that happened — the rule lived in prose.
//!
//! This is the test that fails instead. It builds a real bundle over a fixed
//! fixture, offline, and asserts the ids against `tests/golden/bundle-row-ids.json`.
//!
//! **A deliberate prompt change updates the golden file in the same PR, and
//! the PR body says that every in-flight job will miss and run again from the
//! start.** That is the whole point: the time is not avoidable, only visible.
//!
//! What is pinned and why:
//!
//! * **`id`** — the answer's name. A change here orphans a dispatched job.
//! * **`schema_sha`** — the response schema the row names. Not part of `id`,
//!   so it does not orphan a job's answers, but the cloud's analysis cache
//!   keys the schema too (canonicalised, in the file-analyzer's cache module),
//!   so a change there re-analyses every file of every repo already indexed.
//! * **the path each id belongs to** — so a failure names the file whose
//!   prompt moved rather than printing two lists of hex.
//!
//! What is deliberately NOT pinned: the sha of the guidance block. The block
//! is carried verbatim in the header and sits BEFORE the hashed body, and
//! `analysis_job`'s module doc states the consequence — a release that
//! rewords the guidance renderer must not reject the answers the cache would
//! have served. `a_reworded_guidance_block_leaves_every_id_alone` asserts that
//! directly.
//!
//! Offline: `CARRICK_MOCK_ALL=1`, no cloud, no model.
//!
//! Reference: `docs/reference/dispatch-resume.md`.

use carrick::agent_service::AgentService;
use carrick::agents::file_orchestrator::FileOrchestrator;
use carrick::agents::framework_guidance_agent::{
    FrameworkGuidance, PatternExample, ProtocolGuidance,
};
use carrick::analysis_channel;
use carrick::analysis_job::{AnalyzeRow, body_id};
use carrick::framework_detector::DetectionResult;
use carrick::operation::Protocol;
use serial_test::serial;
use std::path::{Path, PathBuf};

/// The fixture the golden is computed over. Small, checked in, and scanned
/// with the model mocked, so the list below is reproducible on any machine
/// for free.
const FIXTURE: &str = "tests/fixtures/llm-mocked-api";

const GOLDEN: &str = include_str!("golden/bundle-row-ids.json");

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)
}

/// Fixed guidance, so the golden depends on the prompt BUILDER and not on what
/// a guidance answer happened to say. It carries an id, which is what a
/// dispatch requires: without one the cloud keys the whole message and the
/// bundle would be refused.
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
        socket_clients: vec![],
        notes: String::new(),
    }
}

/// The files the golden covers. Named rather than walked, so adding a file to
/// the fixture for some other test cannot silently change what this pins.
fn files(root: &Path) -> Vec<PathBuf> {
    vec![
        root.join("src/index.ts"),
        root.join("src/routes/users.ts"),
        root.join("src/client.ts"),
    ]
}

/// Run one dispatch over the fixture and take what it collected.
async fn dispatch(guidance: FrameworkGuidance) -> Vec<AnalyzeRow> {
    let root = fixture_root();
    analysis_channel::begin_dispatch();
    FileOrchestrator::new(AgentService::new())
        .analyze_files(
            &files(&root),
            &std::collections::HashMap::new(),
            &ProtocolGuidance::from([(Protocol::Http, guidance)]),
            &detection(),
            &root,
            &root,
            &[],
            &Default::default(),
            &Default::default(),
            &carrick::url_normalizer::UrlNormalizer::default_permissive(),
            None,
        )
        .await
        .expect("the analysis ran");
    let collected = analysis_channel::take().expect("the run was dispatching");
    assert!(
        collected.degraded.is_empty(),
        "the dispatch was refused, so there is no bundle to pin: {:?}",
        collected.degraded
    );
    collected.rows
}

/// The repo-relative path a row's body names itself with — the header the
/// prompt writes for the file it is about. Used only to make a failure
/// readable; the id is what is pinned.
fn path_of(row: &AnalyzeRow) -> String {
    const HEADER: &str = "### FILE CONTENT (Path: ";
    row.body
        .split_once(HEADER)
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(path, _)| path.to_string())
        .unwrap_or_else(|| panic!("a prompt body that names no file:\n{}", row.body))
}

/// The rows, in the shape the golden file holds them.
fn rendered(rows: &[AnalyzeRow]) -> serde_json::Value {
    serde_json::json!({
        "fixture": FIXTURE,
        "guidance_key": "a".repeat(64),
        "rows": rows
            .iter()
            .map(|row| serde_json::json!({
                "path": path_of(row),
                "id": row.id,
                "schema_sha": row.schema_sha,
            }))
            .collect::<Vec<_>>(),
    })
}

/// Every prompt this fixture dispatches, named the way its answer will come
/// back.
///
/// A failure here is one of two things, and the PR decides which:
///
/// * an accident — a prompt template, a candidate renderer, an import table or
///   the response schema moved without anyone meaning it to. Revert it.
/// * a deliberate change — then paste the printed list into
///   `tests/golden/bundle-row-ids.json` in the same PR and say in the PR body
///   that every in-flight job will miss and run again from the start.
#[tokio::test]
#[serial]
async fn every_dispatched_row_is_named_by_the_bytes_the_golden_file_pins() {
    // SAFETY: serial test; env vars are process-global.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::remove_var(analysis_channel::ANSWERS_ENV);
    }
    let rows = dispatch(guidance()).await;
    // SAFETY: serial test; leave the process as it was found.
    unsafe { std::env::remove_var("CARRICK_MOCK_ALL") };

    let mut expected: serde_json::Value =
        serde_json::from_str(GOLDEN).expect("the golden file is JSON");
    // The file carries its own instructions for whoever opens it; they are not
    // part of what is pinned.
    expected
        .as_object_mut()
        .expect("the golden file is an object")
        .remove("note");
    let actual = rendered(&rows);

    assert_eq!(
        actual,
        expected,
        "the bytes a dispatched job's answers are named by have moved. Every job in \
         flight now misses on every row and is analysed again from the start.\n\nIf that \
         is deliberate, \
         this is the list to check in, and the PR body has to say so:\n{}\n",
        serde_json::to_string_pretty(&actual).expect("render the actual list")
    );

    // The golden is only worth what it covers: a list of nothing passes.
    assert_eq!(
        rows.len(),
        3,
        "the fixture dispatched a different set of files than the golden was built over"
    );
    for row in &rows {
        assert_eq!(
            row.id,
            body_id(&row.body),
            "the id names the body and nothing else"
        );
    }
}

/// The guidance block is not key material, and a release that rewords the
/// guidance renderer must not orphan a dispatched job.
///
/// The inverse of the test above, and the reason the block's own hash is NOT
/// in the golden file: `analysis_job`'s module doc makes this a stated
/// property of the split, so it needs an assertion rather than a paragraph.
#[tokio::test]
#[serial]
async fn a_reworded_guidance_block_leaves_every_id_alone() {
    // SAFETY: serial test.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::remove_var(analysis_channel::ANSWERS_ENV);
    }
    let plain = dispatch(guidance()).await;

    let mut reworded = guidance();
    reworded.triage_hints = "Quite a lot of freshly generated wording, in the block.".to_string();
    reworded.parsing_notes = "And some more of it, so the block is a different length.".to_string();
    let after = dispatch(reworded).await;
    // SAFETY: serial test.
    unsafe { std::env::remove_var("CARRICK_MOCK_ALL") };

    assert_eq!(
        plain.iter().map(|row| &row.id).collect::<Vec<_>>(),
        after.iter().map(|row| &row.id).collect::<Vec<_>>(),
        "guidance that regenerated into different words renamed every prompt; a resume \
         would re-analyse the whole job"
    );
}
