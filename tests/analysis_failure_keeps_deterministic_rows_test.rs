//! A file the model failed on keeps the rows determinism already stated.
//!
//! The deterministic layer resolves a file's candidates BEFORE the analyzer is
//! called, so by the time a call fails those rows are already in hand and cost
//! nothing. Discarding them with the failure loses index rows to a transient
//! 503 that the source states outright.
//!
//! The run verdict is unchanged: the file is still counted as unanalysed, the
//! run still reports the loss, and the file is still absent from the cache so
//! the next scan retries the model. Only the rows survive.
//!
//! This test lives in its own binary because a lost file is recorded in the
//! process-global [`carrick::scan_health`] registry, which has no reset: a
//! second test in the same process would inherit the loss and see its own run
//! aborted before upload.

use async_trait::async_trait;
use carrick::cloud_storage::{CloudRepoData, CloudStorage, StorageError, UploadOutcome};
use carrick::engine::run_analysis_engine_with_sidecar;
use serial_test::serial;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Default, Clone)]
struct StubStorage {
    repos: Arc<Mutex<Vec<CloudRepoData>>>,
}

#[async_trait]
impl CloudStorage for StubStorage {
    async fn upload_repo_data(&self, data: &CloudRepoData) -> Result<UploadOutcome, StorageError> {
        self.repos.lock().unwrap().push(data.clone());
        Ok(UploadOutcome::default())
    }
    async fn download_all_repo_data(
        &self,
    ) -> Result<(Vec<CloudRepoData>, HashMap<String, String>), StorageError> {
        Ok((self.repos.lock().unwrap().clone(), HashMap::new()))
    }
    async fn upload_type_file(
        &self,
        _repo_name: &str,
        _file_name: &str,
        _content: &str,
    ) -> Result<(), StorageError> {
        Ok(())
    }
    async fn health_check(&self) -> Result<(), StorageError> {
        Ok(())
    }
    async fn upload_logs(&self, _repo: &str, _log_content: &str) -> Result<(), StorageError> {
        Ok(())
    }
    async fn post_pr_result(
        &self,
        _payload: &carrick::findings::PrResultPayload,
    ) -> Result<(), StorageError> {
        Ok(())
    }
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

/// A copy of the fixture whose cassette for ONE file is not JSON, so the
/// analyzer call for that file fails the way a gateway error does: the
/// response cannot be parsed into an answer, the retries are spent, and the
/// orchestrator's fold takes its failure arm.
fn fixture_with_a_failing_file(tmp: &Path, failing_stem: &str) -> (PathBuf, PathBuf) {
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/env-var-whole-url");
    let repo_path = tmp.join("service");
    copy_dir(&fixture, &repo_path);
    let cassette = repo_path.join("__llm__");
    std::fs::write(
        cassette
            .join("analyze-file")
            .join(format!("{failing_stem}.json")),
        "the analyzer did not answer with JSON",
    )
    .unwrap();
    (repo_path, cassette)
}

fn mock_env(cassette: &Path) {
    // SAFETY: the single test in this binary is `#[serial]`, so no other
    // thread is reading the environment while these are set.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassette.display()),
        );
        std::env::set_var("CARRICK_SKIP_INTENTS", "1");
        // The loss below is deliberate, so the run is allowed to finish and
        // upload. Without this the engine aborts before the upload — which is
        // the behaviour this test leaves untouched.
        std::env::set_var(carrick::scan_health::ALLOW_PARTIAL_ENV, "1");
        std::env::remove_var("GITHUB_EVENT_NAME");
        std::env::remove_var("GITHUB_REF");
    }
}

/// `src/helpdesk.ts` holds two calls the source resolves on its own: the
/// whole-URL `fetch(url)` at line 7, whose only path is inside the env-var
/// fallback, and the base-plus-path call at line 20. Neither needs the model.
#[tokio::test]
#[serial]
async fn a_failed_model_call_keeps_the_files_deterministic_rows() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (repo_path, cassette) = fixture_with_a_failing_file(tmp.path(), "helpdesk");
    mock_env(&cassette);

    let storage = StubStorage::default();
    let lost_before = carrick::scan_health::lost_file_count();

    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("the scan must finish: the loss is allowed by the partial-analysis opt-in");

    let uploaded = storage
        .repos
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("the scan uploaded nothing");

    // The failure is still a failure: counted, named, and reported.
    assert_eq!(
        carrick::scan_health::lost_file_count() - lost_before,
        1,
        "the file the model failed on must still be counted as unanalysed"
    );
    let summary = carrick::scan_health::summary_line().expect("a lost file must reach the summary");
    assert!(
        summary.contains("helpdesk.ts"),
        "the run must name the file it lost: {summary}"
    );

    // ... and the rows the source stated on its own are still in the index.
    let helpdesk_calls: Vec<String> = uploaded
        .calls
        .iter()
        .filter(|call| call.file_path.to_string_lossy().contains("helpdesk.ts"))
        .map(|call| call.key.to_string())
        .collect();
    assert!(
        helpdesk_calls.iter().any(|key| key.contains("/api/answer")),
        "the whole-URL call the source resolves must survive the model's failure: \
         {helpdesk_calls:#?}"
    );
    // The file's OTHER call used to be the boundary: the deterministic layer
    // resolved its base but stated no row of its own, so a model failure lost
    // it. carrick#733 made an env-backed base plus a literal path a row, and
    // the assertion says so rather than pinning the gap it left.
    assert!(
        helpdesk_calls
            .iter()
            .any(|key| key.contains("/api/v1/items")),
        "the base-plus-path call the source states must survive the model's failure too: \
         {helpdesk_calls:#?}"
    );

    // The cache must not record the silence as an answer, or the next scan
    // would replay it instead of retrying the model.
    let cached: Vec<String> = uploaded
        .file_results
        .as_ref()
        .expect("the scan must populate the cache")
        .keys()
        .cloned()
        .collect();
    assert!(
        !cached.iter().any(|path| path.ends_with("helpdesk.ts")),
        "a file whose model call failed must stay out of the cache: {cached:#?}"
    );
}
