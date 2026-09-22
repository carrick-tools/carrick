//! What the scan says about the wait before the wait (carrick#1452).
//!
//! A user cannot tell which part of a scan is the model and which is the local
//! read, nor whether the cache is warm, so a five-minute rescan reads as "this
//! could be an hour". The file-analyzer's cache check is the one decision that
//! answers that, and this holds the sentence it states to it:
//!
//! * the counts ARE the decision — the same two numbers the run's own stats
//!   carry, not a figure derived from what came back;
//! * it is stated after the deterministic pass over the files and before the
//!   model stage, which is the only place it can be of any use;
//! * a fully warm cache states the all-analysed form AND issues no request at
//!   all, which is what "before the first request" comes to on the run a
//!   reader most wants the sentence for.
//!
//! The stream is captured by taking fd 2 for the duration: `errln!` writes to
//! `std::io::stderr()` directly — that is the point of it (carrick#1386) — so
//! nothing the test harness does to `print!` reaches these lines.

use carrick::agent_service::AgentService;
use carrick::agents::file_analyzer_agent::FileAnalysisResult;
use carrick::agents::file_orchestrator::{FileCentricAnalysisResult, FileOrchestrator};
use carrick::agents::framework_guidance_agent::{
    FrameworkGuidance, PatternExample, ProtocolGuidance,
};
use carrick::framework_detector::DetectionResult;
use carrick::operation::Protocol;
use serial_test::serial;
use std::collections::HashMap;
use std::io::{Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

/// The route the file analyzer's requests are counted under.
const ANALYZE_FILE: &str = "/analyze-file";

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/llm-mocked-api")
}

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

fn files(root: &Path) -> Vec<PathBuf> {
    vec![
        root.join("src/index.ts"),
        root.join("src/routes/users.ts"),
        root.join("src/client.ts"),
    ]
}

async fn analyze(
    root: &Path,
    cached: &HashMap<String, FileAnalysisResult>,
) -> FileCentricAnalysisResult {
    FileOrchestrator::new(AgentService::new())
        .analyze_files(
            &files(root),
            cached,
            &ProtocolGuidance::from([(Protocol::Http, guidance())]),
            &detection(),
            root,
            root,
            &[],
            &Default::default(),
            &Default::default(),
            &carrick::url_normalizer::UrlNormalizer::default_permissive(),
            &carrick::workspace_resolver::WorkspaceIndex::build_with_aliases(root, None),
            None,
        )
        .await
        .expect("the analysis ran")
}

/// Run `work` with this process's stderr pointed at a file, and answer what it
/// wrote there.
///
/// `dup` keeps the real stderr, `dup2` puts the file in its place, and the
/// original is put back before the text is read, so a failing assertion is
/// still reported on a terminal that exists.
fn capturing_stderr<T>(work: impl FnOnce() -> T) -> (T, String) {
    let mut file = tempfile::tempfile().expect("a file to capture stderr in");
    let real = unsafe { libc::dup(libc::STDERR_FILENO) };
    assert!(real >= 0, "stderr could be duplicated");
    let _ = std::io::stderr().flush();
    assert!(
        unsafe { libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO) } >= 0,
        "stderr could be redirected"
    );
    let outcome = work();
    let _ = std::io::stderr().flush();
    assert!(
        unsafe { libc::dup2(real, libc::STDERR_FILENO) } >= 0,
        "stderr could be put back"
    );
    unsafe { libc::close(real) };
    file.rewind().expect("rewind the capture");
    let mut text = String::new();
    file.read_to_string(&mut text).expect("read the capture");
    (outcome, text)
}

/// The notices the scan stated, in the order it stated them, with the line
/// each was on so an ordering assertion can name it.
fn notices(stream: &str) -> Vec<(usize, String)> {
    stream
        .lines()
        .enumerate()
        .filter_map(|(at, line)| carrick::progress::parse_notice(line).map(|text| (at, text)))
        .collect()
}

fn analyze_file_requests() -> usize {
    carrick::agent_service::request_counts()
        .get(ANALYZE_FILE)
        .copied()
        .unwrap_or(0)
}

/// The last line of the deterministic pass over the files: every file has been
/// read. The cache-check sentence belongs after it and before the model, and
/// this is the only marker in the stream that dates the boundary.
fn last_files_marker(stream: &str) -> usize {
    stream
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            carrick::progress::parse(line).is_some_and(|update| {
                matches!(update.phase, carrick::progress::Phase::Files)
                    && update.done == update.total
            })
        })
        .map(|(at, _)| at)
        .last()
        .expect("the deterministic pass ticked to its total")
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_cache_check_states_its_decision_before_the_model_is_asked() {
    let root = fixture_root();
    // SAFETY: serial test; env vars are process-global. The progress channel
    // is what carries a notice to a parent, and the mock is what keeps this
    // offline.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var(carrick::progress::PROGRESS_ENV, "1");
        std::env::remove_var(carrick::analysis_channel::ANSWERS_ENV);
    }

    // --- cold: nothing in hand, every candidate file goes to the model ------
    let runtime = tokio::runtime::Handle::current();
    let cold_started = analyze_file_requests();
    let (cold, stream) = capturing_stderr(|| {
        tokio::task::block_in_place(|| runtime.block_on(analyze(&root, &HashMap::new())))
    });

    let stated = notices(&stream);
    let (at, cold_line) = stated
        .iter()
        .find(|(_, text)| text.starts_with("Model analysis:"))
        .expect("the cache check states what it decided");
    assert_eq!(
        cold_line,
        &format!(
            "Model analysis: {} of {} files already analysed, {} new.",
            cold.stats.files_model_reused,
            cold.stats.files_model_reused + cold.stats.files_model_dispatched,
            cold.stats.files_model_dispatched
        ),
        "the counts are the decision the run recorded, not a figure beside it"
    );
    assert_eq!(
        cold.stats.files_model_reused, 0,
        "nothing was in hand on this pass"
    );
    assert!(
        cold.stats.files_model_dispatched > 0,
        "the fixture raises candidates, so there is model work to state"
    );
    assert!(
        *at > last_files_marker(&stream),
        "the sentence comes after the files are read and before the model: {stream}"
    );
    assert!(
        analyze_file_requests() > cold_started,
        "a cold pass asks the model, which is what makes the warm pass below mean something"
    );

    // --- warm: this scan's own answers handed back to it --------------------
    let warm_started = analyze_file_requests();
    let cached = cold.raw_model_results.clone();
    assert_eq!(
        cached.len(),
        cold.stats.files_model_dispatched,
        "every answered file is in the cache the next scan reads"
    );
    let (warm, stream) = capturing_stderr(|| {
        tokio::task::block_in_place(|| runtime.block_on(analyze(&root, &cached)))
    });

    assert_eq!(
        notices(&stream)
            .into_iter()
            .find(|(_, text)| text.starts_with("Model analysis:"))
            .map(|(_, text)| text),
        Some(format!(
            "Model analysis: all {} files already analysed.",
            warm.stats.files_model_reused
        )),
        "a warm cache says so, in the form that has no second number to read"
    );
    assert_eq!(
        warm.stats.files_model_dispatched, 0,
        "nothing was left for the model"
    );
    assert_eq!(
        analyze_file_requests(),
        warm_started,
        "and the sentence was stated without one request being made"
    );
}
