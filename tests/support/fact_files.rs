//! The fact layer's files for a tree, handed to an in-process file
//! orchestrator as a scan hands them (carrick#3711 R1). A test that calls
//! `FileOrchestrator::analyze_files` itself gets no fact files unless it
//! passes these through `with_fact_files`, so no store answer reaches it.
//!
//! The walk is the real sidecar's, and the store is answered from the tree's
//! own `__llm__/library-claims/default.json` under `CARRICK_MOCK_ALL`.

#![allow(dead_code)]

use carrick::agents::file_orchestrator::FactFileSource;
use carrick::request_summary::fact_files::FactFiles;
use carrick::swc_scanner::{ScanResult, SwcScanner};
use std::path::{Path, PathBuf};

/// The built sidecar. CI builds it before the Rust tests; without it these
/// tests prove nothing, so its absence fails them rather than skipping.
pub fn sidecar_script() -> PathBuf {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    assert!(
        script.exists(),
        "the fact layer needs the built sidecar at {}",
        script.display()
    );
    script
}

/// The fact files of `repo`'s first service, its store answered from
/// `repo/__llm__`. Sets `CARRICK_MOCK_ALL` and `CARRICK_MOCK_FIXTURE_DIR`
/// (callers run serially) and leaves `CARRICK_MOCK_FIXTURE_DIR` unset after.
pub async fn walked(repo: &Path) -> FactFiles {
    // SAFETY: the callers' tests run one at a time; env vars are
    // process-global.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var("CARRICK_MOCK_FIXTURE_DIR", repo.join("__llm__"));
    }
    let rows = tempfile::tempdir().expect("a rows dir");
    let facts =
        carrick::engine::fact_seam::fact_files_of_tree(repo, &sidecar_script(), rows.path())
            .await
            .unwrap_or_else(|| panic!("{} asks the store nothing", repo.display()));
    unsafe {
        std::env::remove_var("CARRICK_MOCK_FIXTURE_DIR");
    }
    facts
}

/// [`walked`] from synchronous code, or from inside a test's runtime: on a
/// thread and a runtime of its own.
pub fn walked_blocking(repo: &Path) -> FactFiles {
    let repo = repo.to_path_buf();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(walked(&repo))
    })
    .join()
    .expect("the walk ran")
}

/// `facts` as the engine sends them: already arrived.
pub fn source(facts: &FactFiles) -> FactFileSource {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    sender
        .send(Some(facts.clone()))
        .expect("the receiver is held");
    FactFileSource::later(receiver)
}

/// The fact layer's listed calls in `file` (relative to `repo`), as the
/// orchestrator lists them for the reader (`scan_sites`), with their ids.
pub fn listed_sites(facts: &FactFiles, repo: &Path, file: &str) -> ScanResult {
    let path = repo.join(file);
    let content = std::fs::read_to_string(&path).expect("the file reads");
    SwcScanner::new().scan_sites(&path, &content, &facts.listed_in(file))
}

/// The id of the fact layer's listed call in `file` (relative to `repo`)
/// that starts on `line`, as the model would echo it.
pub fn listed_candidate_at(
    facts: &FactFiles,
    repo: &Path,
    file: &str,
    line: usize,
) -> Option<String> {
    listed_sites(facts, repo, file)
        .candidates
        .into_iter()
        .find(|candidate| candidate.line_number == line)
        .map(|candidate| candidate.candidate_id)
}
