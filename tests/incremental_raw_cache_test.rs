//! The incremental cache holds the model's RAW answer per file, not the joined
//! result (Phase A of the scanner consolidation).
//!
//! Before this split, an unchanged file replayed the rows a previous scan's
//! deterministic layer had already folded in, so every resolver improvement
//! needed a `CACHE_VERSION` bump and a full re-analysis of every indexed repo.
//! Now the deterministic layer runs on every scan over every discovered file
//! and only the model's reply is cached, so a scanner improvement reaches an
//! indexed repo on the next push with zero model calls.
//!
//! Both tests drive the real engine over a copy of the `env-var-whole-url`
//! fixture with its own cassette replayed for the model, so the model's answer
//! is fixed and any difference between the two scans is the scanner's.
//!
//! They are `#[serial]` and read [`carrick::scan_health::attempted_count`],
//! which is a process-global: two of these running at once would see each
//! other's dispatch counts.

use async_trait::async_trait;
use carrick::cloud_storage::{CloudRepoData, CloudStorage, StorageError, UploadOutcome};
use carrick::engine::run_analysis_engine_with_sidecar;
use serial_test::serial;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// In-memory storage with no synthetic seed repos and direct access to what
/// was uploaded, so a test can read scan #1's payload back and mutate it
/// before scan #2 picks it up as `previous_data`.
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

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .status()
        .expect("git failed to spawn");
    assert!(status.success(), "git {:?} failed", args);
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

/// A committed copy of the fixture, plus the cassette directory to replay the
/// model from. The copy is committed so the incremental branch has a previous
/// commit to diff against; nothing else is committed afterwards, so HEAD stays
/// equal to scan #1's `commit_hash` and `git diff` reports no changed file.
fn committed_fixture(tmp: &Path) -> (PathBuf, PathBuf) {
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/env-var-whole-url");
    let repo_path = tmp.join("service");
    copy_dir(&fixture, &repo_path);
    run_git(&repo_path, &["init", "-q"]);
    run_git(&repo_path, &["add", "-A"]);
    run_git(&repo_path, &["commit", "-q", "-m", "init"]);
    let cassette = repo_path.join("__llm__");
    (repo_path, cassette)
}

fn mock_env(cassette: &Path) {
    // SAFETY: both tests in this binary are `#[serial]`, so no other thread is
    // reading the environment while these are set.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassette.display()),
        );
        std::env::set_var("CARRICK_SKIP_INTENTS", "1");
        // The engine refuses to upload from a pull_request run or a non-main
        // ref. This scan targets a temp repo and a stub store, so the runner's
        // GitHub context must not apply.
        std::env::remove_var("GITHUB_EVENT_NAME");
        std::env::remove_var("GITHUB_REF");
    }
}

fn latest_upload(storage: &StubStorage) -> CloudRepoData {
    storage
        .repos
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("a scan uploaded nothing")
}

/// Canonical, order-free view of a projection array, so a HashMap iteration
/// order cannot read as a difference between two scans.
fn canonical(rows: &[carrick::analyzer::ApiEndpointDetails]) -> Vec<String> {
    let mut out: Vec<String> = rows
        .iter()
        .map(|row| serde_json::to_string(row).expect("row serializes"))
        .collect();
    out.sort();
    out
}

/// Every data-call and endpoint row the cache holds, as
/// `(file, line, target-or-path, resolution_source)`.
fn cached_rows(data: &CloudRepoData) -> Vec<(String, i32, String, String)> {
    let mut rows = Vec::new();
    let results = data
        .file_results
        .as_ref()
        .expect("a scan must populate the incremental cache");
    for (path, result) in results {
        for call in &result.data_calls {
            rows.push((
                path.clone(),
                call.line_number,
                call.target.clone(),
                format!("{:?}", call.resolution_source),
            ));
        }
        for endpoint in &result.endpoints {
            rows.push((
                path.clone(),
                endpoint.line_number,
                endpoint.path.clone(),
                format!("{:?}", endpoint.resolution_source),
            ));
        }
    }
    rows.sort();
    rows
}

/// The cache is the model's raw reply: no row in it may carry a deterministic
/// provenance, because the deterministic layer emits its rows AFTER the cache
/// is read and re-emits them on every scan.
fn assert_no_deterministic_rows(data: &CloudRepoData, label: &str) {
    let deterministic: Vec<_> = cached_rows(data)
        .into_iter()
        .filter(|(_, _, _, source)| !matches!(source.as_str(), "None" | "Some(Model)"))
        .collect();
    assert!(
        deterministic.is_empty(),
        "{label}: the cache holds rows the deterministic layer stated, which it re-states on \
         every scan: {deterministic:#?}"
    );
}

/// The whole split, end to end: a second scan of an unchanged tree calls the
/// model zero times, produces exactly the projection the first scan did, and
/// caches only what the model said — the deterministic rows are recomputed.
#[tokio::test]
#[serial]
async fn an_unchanged_scan_replays_the_cached_model_answer_and_re_emits_the_rest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (repo_path, cassette) = committed_fixture(tmp.path());
    mock_env(&cassette);

    let storage = StubStorage::default();

    let before_scan_one = carrick::scan_health::attempted_count();
    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #1 failed");
    let scan_one = latest_upload(&storage);
    let dispatched_one = carrick::scan_health::attempted_count() - before_scan_one;
    assert!(
        dispatched_one > 0,
        "scan #1 is the cold scan: it must dispatch files to the model"
    );

    // No commit between the scans, so HEAD is still scan #1's `commit_hash`
    // and every file is unchanged.
    let before_scan_two = carrick::scan_health::attempted_count();
    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #2 failed");
    let scan_two = latest_upload(&storage);
    let dispatched_two = carrick::scan_health::attempted_count() - before_scan_two;

    assert_eq!(
        dispatched_two, 0,
        "an unchanged tree must reach the model zero times (scan #1 dispatched {dispatched_one})"
    );

    // The deterministic layer re-ran over every file and the cached model
    // answer joined onto it, so the projection is the cold scan's.
    assert_eq!(
        canonical(&scan_two.calls),
        canonical(&scan_one.calls),
        "the incremental scan's calls differ from the cold scan's"
    );
    assert_eq!(
        canonical(&scan_two.endpoints),
        canonical(&scan_one.endpoints),
        "the incremental scan's endpoints differ from the cold scan's"
    );

    assert_no_deterministic_rows(&scan_one, "scan #1");
    assert_no_deterministic_rows(&scan_two, "scan #2");

    // The named case: the model reports the binding's NAME as the target of
    // the whole-URL call (`fetch(url)`), and the source states the URL. The
    // cache must hold the model's words; the projection must hold the
    // source's.
    let cached = cached_rows(&scan_two);
    assert!(
        cached
            .iter()
            .any(|(file, line, target, _)| file.ends_with("helpdesk.ts")
                && *line == 7
                && target == "url"),
        "the cache must hold the model's own target for src/helpdesk.ts:7: {cached:#?}"
    );
    assert!(
        !cached
            .iter()
            .any(|(_, _, target, _)| target == "${process.env.HELPDESK_URL}/api/answer"),
        "the cache must not hold the target the deterministic layer resolved: {cached:#?}"
    );
    assert!(
        scan_two
            .calls
            .iter()
            .any(|call| call.key.to_string().contains("/api/answer")),
        "the projection must still carry the resolved whole-URL call: {:#?}",
        scan_two.calls
    );
}

/// A cache written by the previous format is refused, and the scan that
/// refuses it re-reads every file from the model. The version is what carries
/// a change of join rule (or of prompt or schema) to already-indexed repos.
#[tokio::test]
#[serial]
async fn a_previous_format_cache_is_refused_and_the_scan_goes_back_to_the_model() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (repo_path, cassette) = committed_fixture(tmp.path());
    mock_env(&cassette);

    let storage = StubStorage::default();

    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #1 failed");

    // Age scan #1's payload to the format that held JOINED results.
    {
        let mut repos = storage.repos.lock().unwrap();
        let prev = repos.last_mut().expect("no prior upload to mutate");
        assert!(
            prev.file_results.is_some(),
            "scan #1 must have populated the cache"
        );
        prev.cache_version = Some(20);
    }

    let before_scan_two = carrick::scan_health::attempted_count();
    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #2 failed");
    let dispatched_two = carrick::scan_health::attempted_count() - before_scan_two;

    assert!(
        dispatched_two > 0,
        "a v20 cache holds joined rows this version's join would fold in again: it must be \
         refused and every file re-read from the model"
    );
}
