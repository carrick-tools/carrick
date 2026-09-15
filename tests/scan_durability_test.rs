//! A scan survives one model call the cloud never answered (2026-09-15).
//!
//! The incident: a seven-service first index finished four services, then one
//! framework-detection call came back `model_error` after its retries, the
//! `?` on it ended the run, and nothing uploaded. These tests drive the real
//! engine over a three-service repo with the model mocked, fail service 2's
//! detection on purpose, and assert what the run does instead:
//!
//! - the run finishes, and services 1 and 3 land either way;
//! - one failure is absorbed by the in-run retry, and everything lands;
//! - a failure that outlasts the retry leaves service 2 pending: uploaded
//!   facts-only with no cached detection, and the laptop scan closed with a
//!   fail marker rather than its last write (carrick-cloud#892);
//! - the rescan asks for service 2's detection and nobody else's.
//!
//! Its own test binary, `#[serial]`, and every count read as a delta: the
//! scan-health registry, the request counters and the injected failures are
//! process-globals.

use async_trait::async_trait;
use carrick::cloud_storage::{
    CloudRepoData, CloudStorage, RunContext, RunStart, StorageError, UploadOutcome,
};
use carrick::engine::run_analysis_engine_with_sidecar;
use serial_test::serial;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// One upload, as the engine made it.
#[derive(Clone)]
struct Upload {
    data: CloudRepoData,
    final_in_run: bool,
}

/// In-memory storage that keeps every upload, the latest per service as the
/// previous generation, and every fail marker. `indexed` is what `start-scan`
/// answers; `Some` makes the run a laptop run.
#[derive(Default, Clone)]
struct StubStorage {
    uploads: Arc<Mutex<Vec<Upload>>>,
    scan_failed: Arc<Mutex<Vec<(String, String)>>>,
    kept: Arc<Mutex<Vec<String>>>,
    indexed: Option<Vec<String>>,
}

impl StubStorage {
    fn laptop(indexed: &[&str]) -> Self {
        Self {
            indexed: Some(indexed.iter().map(|s| s.to_string()).collect()),
            ..Self::default()
        }
    }

    fn uploads(&self) -> Vec<Upload> {
        self.uploads.lock().unwrap().clone()
    }

    fn latest(&self, service: &str) -> Option<CloudRepoData> {
        self.uploads()
            .into_iter()
            .rev()
            .find(|u| u.data.service_name.as_deref() == Some(service))
            .map(|u| u.data)
    }

    fn uploaded_services(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .uploads()
            .into_iter()
            .filter_map(|u| u.data.service_name)
            .collect();
        names.sort();
        names
    }
}

#[async_trait]
impl CloudStorage for StubStorage {
    async fn upload_repo_data(
        &self,
        data: &CloudRepoData,
        final_in_run: bool,
    ) -> Result<UploadOutcome, StorageError> {
        self.uploads.lock().unwrap().push(Upload {
            data: data.clone(),
            final_in_run,
        });
        Ok(UploadOutcome::default())
    }
    async fn index_landed(
        &self,
        _data: &CloudRepoData,
        _written_after: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, StorageError> {
        Ok(true)
    }
    async fn begin_run(&self, _run: &RunContext) -> Result<RunStart, StorageError> {
        Ok(RunStart {
            allowance_sentence: None,
            indexed_services: self.indexed.clone(),
        })
    }
    fn supports_multi_service(&self) -> bool {
        true
    }
    fn keep_served_generation(&self, data: &CloudRepoData) {
        self.kept
            .lock()
            .unwrap()
            .push(data.service_name.clone().unwrap_or_default());
    }
    async fn download_all_repo_data(
        &self,
    ) -> Result<(Vec<CloudRepoData>, HashMap<String, String>), StorageError> {
        // The latest generation of each service, as the index serves it.
        let mut latest: Vec<CloudRepoData> = Vec::new();
        for upload in self.uploads() {
            latest.retain(|d| {
                d.repo_name != upload.data.repo_name || d.service_name != upload.data.service_name
            });
            latest.push(upload.data);
        }
        Ok((latest, HashMap::new()))
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
    fn uploads_run_logs(&self) -> bool {
        false
    }
    async fn report_scan_failed(&self, stage: &str, reason: &str) {
        self.scan_failed
            .lock()
            .unwrap()
            .push((stage.to_string(), reason.to_string()));
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

/// Only service 2's manifest names this, so it is the needle that fails
/// service 2's detection call and no other.
const BETA_ONLY_DEPENDENCY: &str = "beta-only-dependency";

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// A committed three-service repo: alpha, beta, gamma, each an HTTP server
/// with one route and one outbound call, so each has files the model is asked
/// about.
fn three_service_repo(tmp: &Path) -> PathBuf {
    let repo = tmp.join("monorepo");
    write(
        &repo,
        "carrick.json",
        r#"{
  "services": [
    { "name": "alpha", "directory": "services/alpha" },
    { "name": "beta", "directory": "services/beta" },
    { "name": "gamma", "directory": "services/gamma" }
  ]
}"#,
    );
    write(
        &repo,
        "package.json",
        r#"{ "name": "monorepo", "private": true }"#,
    );
    for (service, extra) in [
        ("alpha", ""),
        ("beta", ",\n    \"beta-only-dependency\": \"1.0.0\""),
        ("gamma", ""),
    ] {
        write(
            &repo,
            &format!("services/{service}/package.json"),
            &format!(
                "{{\n  \"name\": \"{service}\",\n  \"dependencies\": {{\n    \"express\": \"4.18.2\"{extra}\n  }}\n}}\n"
            ),
        );
        write(
            &repo,
            &format!("services/{service}/src/server.ts"),
            &format!(
                "import express from \"express\";\n\n\
                 const app = express();\n\n\
                 app.get(\"/{service}/status\", async (_req, res) => {{\n  \
                 const upstream = await fetch(\"http://localhost:9000/api/health\");\n  \
                 res.json({{ ok: upstream.ok }});\n}});\n\n\
                 app.listen(3000);\n"
            ),
        );
    }
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

fn offline_env() {
    // SAFETY: every test in this binary is `#[serial]`, so no other thread
    // reads the environment while these are set.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var("CARRICK_SKIP_INTENTS", "1");
        std::env::set_var(carrick::engine::durability::RETRY_DELAY_ENV, "0");
        std::env::remove_var("CARRICK_MOCK_FIXTURE_DIR");
        std::env::remove_var(carrick::scan_health::ALLOW_PARTIAL_ENV);
        std::env::remove_var("GITHUB_EVENT_NAME");
        std::env::remove_var("GITHUB_REF");
    }
}

fn detect_requests() -> usize {
    carrick::agent_service::request_counts()
        .get("/framework-detect")
        .copied()
        .unwrap_or(0)
}

/// One exhausted detection call is absorbed by the retry at the end of the
/// run: every service lands, service 2 with its detection cached, and the
/// laptop scan is closed by its last write as usual.
#[tokio::test]
#[serial]
async fn a_detection_failure_the_in_run_retry_recovers_lands_every_service() {
    offline_env();
    let tmp = tempfile::tempdir().unwrap();
    let repo = three_service_repo(tmp.path());
    let storage = StubStorage::laptop(&[]);

    carrick::agent_service::inject_mock_failure("/framework-detect", BETA_ONLY_DEPENDENCY, 1);
    let before = detect_requests();
    run_analysis_engine_with_sidecar(storage.clone(), repo.to_str().unwrap(), None, false)
        .await
        .expect("a recovered detection failure must not fail the run");

    // alpha, beta (failed), gamma, then beta again in the retry.
    assert_eq!(detect_requests() - before, 4);
    assert_eq!(storage.uploaded_services(), ["alpha", "beta", "gamma"]);
    let beta = storage.latest("beta").unwrap();
    assert!(
        beta.cached_detection.is_some(),
        "the retry's detection is cached"
    );
    let uploads = storage.uploads();
    assert!(
        uploads.last().unwrap().final_in_run,
        "the last write closes the scan"
    );
    assert!(storage.scan_failed.lock().unwrap().is_empty());
}

/// A failure that outlasts the retry: the run still exits cleanly, services 1
/// and 3 land complete, service 2 lands facts-only as a first index with no
/// cached detection, no write closes the scan, and the fail marker names
/// service 2. The rescan then asks for service 2's detection only.
#[tokio::test]
#[serial]
async fn a_detection_failure_that_outlasts_the_retry_leaves_only_that_service_pending() {
    offline_env();
    let tmp = tempfile::tempdir().unwrap();
    let repo = three_service_repo(tmp.path());
    let storage = StubStorage::laptop(&[]);

    carrick::agent_service::inject_mock_failure("/framework-detect", BETA_ONLY_DEPENDENCY, 2);
    run_analysis_engine_with_sidecar(storage.clone(), repo.to_str().unwrap(), None, false)
        .await
        .expect("a laptop run with a pending service exits 0");

    assert_eq!(storage.uploaded_services(), ["alpha", "beta", "gamma"]);
    for service in ["alpha", "gamma"] {
        let data = storage.latest(service).unwrap();
        assert!(data.cached_detection.is_some(), "{service} is complete");
        assert!(data.cached_guidance.is_some(), "{service} is complete");
    }
    let beta = storage.latest("beta").unwrap();
    assert!(
        beta.cached_detection.is_none() && beta.cached_guidance.is_none(),
        "a pending service caches no detection, so the next scan asks for it"
    );
    assert!(
        beta.file_results.as_ref().is_none_or(|r| r.is_empty()),
        "no file of a deferred service holds a model answer"
    );
    assert!(
        beta.boundary.is_some(),
        "the facts-only analysis still lands its structural payload"
    );
    assert!(
        storage.uploads().iter().all(|u| !u.final_in_run),
        "a run with a pending service must not close its scan with a write"
    );
    let markers = storage.scan_failed.lock().unwrap().clone();
    assert_eq!(markers.len(), 1, "{markers:?}");
    assert!(markers[0].1.contains("beta"), "{markers:?}");
    assert!(markers[0].1.contains("framework detection"), "{markers:?}");

    // The rescan. Every service now has a row; only beta has no detection.
    let rescan = StubStorage {
        indexed: Some(vec!["alpha".into(), "beta".into(), "gamma".into()]),
        ..storage.clone()
    };
    let already = storage.uploads().len();
    let before = detect_requests();
    run_analysis_engine_with_sidecar(rescan.clone(), repo.to_str().unwrap(), None, false)
        .await
        .expect("the rescan succeeds");
    assert_eq!(
        detect_requests() - before,
        1,
        "the rescan asks for beta's detection and replays alpha's and gamma's"
    );
    let beta = rescan.latest("beta").unwrap();
    assert!(beta.cached_detection.is_some());
    let fresh = &rescan.uploads()[already..];
    assert!(
        fresh.last().unwrap().final_in_run,
        "a complete rescan closes its scan"
    );
    assert!(
        rescan.scan_failed.lock().unwrap().len() == 1,
        "no new marker"
    );
}

/// On CI the same failure lands services 1 and 3 and ends non-zero naming
/// service 2, because the exit code is the only place CI can show it. A
/// service that already has an index is held back rather than thinned.
#[tokio::test]
#[serial]
async fn on_ci_a_pending_service_with_an_index_is_held_back_and_named() {
    offline_env();
    let tmp = tempfile::tempdir().unwrap();
    let repo = three_service_repo(tmp.path());
    let storage = StubStorage::default();

    // First, a clean scan: every service gets an index.
    run_analysis_engine_with_sidecar(storage.clone(), repo.to_str().unwrap(), None, false)
        .await
        .expect("a clean CI scan");
    let first = storage.uploads().len();
    assert_eq!(first, 3);

    // Then a scan whose beta detection never answers. --no-cache forces
    // detection to be asked again for every service.
    carrick::agent_service::inject_mock_failure("/framework-detect", BETA_ONLY_DEPENDENCY, 2);
    let error =
        run_analysis_engine_with_sidecar(storage.clone(), repo.to_str().unwrap(), None, true)
            .await
            .expect_err("CI says a service is pending");
    let message = error.to_string();
    assert!(message.contains("beta"), "{message}");
    let mut landed: Vec<String> = storage.uploads()[first..]
        .iter()
        .filter_map(|u| u.data.service_name.clone())
        .collect();
    landed.sort();
    assert_eq!(
        landed,
        ["alpha", "gamma"],
        "beta's index is kept, not thinned"
    );
}

/// The laptop half of holding a service back: a rescan whose detection for a
/// service that already has an index never answers keeps that index, keeps
/// its served generation for the local read model, lands the other two, and
/// still exits 0 with the scan closed by the fail marker.
#[tokio::test]
#[serial]
async fn on_a_laptop_a_held_back_service_keeps_its_served_generation() {
    offline_env();
    let tmp = tempfile::tempdir().unwrap();
    let repo = three_service_repo(tmp.path());
    let first = StubStorage::laptop(&[]);
    run_analysis_engine_with_sidecar(first.clone(), repo.to_str().unwrap(), None, false)
        .await
        .expect("a clean first index");
    let landed = first.uploads().len();

    let rescan = StubStorage {
        indexed: Some(vec!["alpha".into(), "beta".into(), "gamma".into()]),
        ..first.clone()
    };
    carrick::agent_service::inject_mock_failure("/framework-detect", BETA_ONLY_DEPENDENCY, 2);
    run_analysis_engine_with_sidecar(rescan.clone(), repo.to_str().unwrap(), None, true)
        .await
        .expect("a laptop run with a held-back service exits 0");

    let mut fresh: Vec<String> = rescan.uploads()[landed..]
        .iter()
        .filter_map(|u| u.data.service_name.clone())
        .collect();
    fresh.sort();
    assert_eq!(
        fresh,
        ["alpha", "gamma"],
        "beta's index is kept, not thinned"
    );
    assert_eq!(*rescan.kept.lock().unwrap(), ["beta"]);
    assert!(rescan.uploads()[landed..].iter().all(|u| !u.final_in_run));
    let markers = rescan.scan_failed.lock().unwrap().clone();
    assert_eq!(markers.len(), 1, "{markers:?}");
    assert!(markers[0].1.contains("beta"), "{markers:?}");
}
