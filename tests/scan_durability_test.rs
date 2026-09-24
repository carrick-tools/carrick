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
//!   facts-only with no cached detection. Against a cloud that reads
//!   `pending_services` the last write closes the scan and names service 2;
//!   against one that does not, no write closes it and a fail marker does
//!   (carrick-cloud#892);
//! - a detection a budget refused is pending too, so a first index whose
//!   ceiling is spent stays open for a raised ceiling to govern its re-run;
//! - a FILE a budget refused is not a loss, and the next scan asks about it
//!   again while its unchanged siblings replay (carrick#1413);
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
    /// Whether this stands for a cloud that answered
    /// `accepts_pending_services` at `start-scan`.
    accepts_pending: bool,
    /// Every list the engine asked the final write to carry.
    pending_named: Arc<Mutex<Vec<Vec<String>>>>,
}

impl StubStorage {
    fn laptop(indexed: &[&str]) -> Self {
        Self {
            indexed: Some(indexed.iter().map(|s| s.to_string()).collect()),
            ..Self::default()
        }
    }

    /// A laptop run against a cloud that reads `pending_services`.
    fn laptop_on_current_cloud(indexed: &[&str]) -> Self {
        Self {
            accepts_pending: true,
            ..Self::laptop(indexed)
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
    fn name_pending_on_final_write(&self, pending_services: &[String]) -> bool {
        if !self.accepts_pending {
            return false;
        }
        self.pending_named
            .lock()
            .unwrap()
            .push(pending_services.to_vec());
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
        std::env::remove_var(carrick::retry_budget::BUDGET_ENV);
        std::env::remove_var("CARRICK_MOCK_FIXTURE_DIR");
        std::env::remove_var(carrick::scan_health::ALLOW_PARTIAL_ENV);
        std::env::remove_var("GITHUB_EVENT_NAME");
        std::env::remove_var("GITHUB_REF");
    }
}

fn requests_to(route: &str) -> usize {
    carrick::agent_service::request_counts()
        .get(route)
        .copied()
        .unwrap_or(0)
}

fn detect_requests() -> usize {
    requests_to("/framework-detect")
}

fn analyze_file_requests() -> usize {
    requests_to("/analyze-file")
}

/// Stands in for whatever the cloud refuses with. Deliberately not a copy of
/// any sentence the cloud ships: the point of quoting `error.message` is that
/// the scanner holds no opinion about the words, so a test that asserted the
/// cloud's current wording would be asserting the opposite.
const REFUSAL_SENTENCE: &str =
    "A budget refused this scan. It finished with facts only; inference resumes later.";

/// Matches the one `general` guidance request each service makes, and none of
/// the pattern or extraction-config requests beside it.
const GENERAL_GUIDANCE: &str = "\"task\":\"general\"";

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

/// A failure that outlasts the retry, against a cloud deployed before
/// `accepts_pending_services`: the run still exits cleanly, services 1 and 3
/// land complete, service 2 lands facts-only as a first index with no cached
/// detection, no write closes the scan (that cloud would stamp the first index
/// complete on it), and the fail marker names service 2. The rescan then asks
/// for service 2's detection only.
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
        "against this cloud a run with a pending service must not close its scan with a write"
    );
    assert!(storage.pending_named.lock().unwrap().is_empty());
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

/// The same failure against a cloud that reads `pending_services`: every
/// service lands, the last write closes the scan and names service 2, and no
/// fail marker is sent, so the run gets its receipt and last-scan row and the
/// founder is told it finished (carrick-cloud#892). The complete rescan closes
/// with its last write and names nothing.
#[tokio::test]
#[serial]
async fn a_pending_service_rides_the_final_write_to_a_cloud_that_reads_it() {
    offline_env();
    let tmp = tempfile::tempdir().unwrap();
    let repo = three_service_repo(tmp.path());
    let storage = StubStorage::laptop_on_current_cloud(&[]);

    carrick::agent_service::inject_mock_failure("/framework-detect", BETA_ONLY_DEPENDENCY, 2);
    run_analysis_engine_with_sidecar(storage.clone(), repo.to_str().unwrap(), None, false)
        .await
        .expect("a laptop run with a pending service exits 0");

    assert_eq!(storage.uploaded_services(), ["alpha", "beta", "gamma"]);
    let uploads = storage.uploads();
    assert!(
        uploads.last().unwrap().final_in_run,
        "the last write closes the scan"
    );
    assert_eq!(
        uploads.iter().filter(|u| u.final_in_run).count(),
        1,
        "only the last write is final"
    );
    assert_eq!(
        *storage.pending_named.lock().unwrap(),
        [vec!["beta".to_string()]],
        "the final write names the pending service"
    );
    assert!(
        storage.scan_failed.lock().unwrap().is_empty(),
        "a finished run is not reported failed"
    );

    let rescan = StubStorage {
        indexed: Some(vec!["alpha".into(), "beta".into(), "gamma".into()]),
        ..storage.clone()
    };
    let already = rescan.uploads().len();
    run_analysis_engine_with_sidecar(rescan.clone(), repo.to_str().unwrap(), None, false)
        .await
        .expect("the rescan succeeds");
    assert!(rescan.uploads()[already..].last().unwrap().final_in_run);
    assert_eq!(
        rescan.pending_named.lock().unwrap().len(),
        1,
        "a complete rescan names nothing"
    );
    assert!(rescan.scan_failed.lock().unwrap().is_empty());
}

/// A detection the budget refused is pending like any other: the first index
/// stays open, so a per-workspace ceiling raised after the refusal still
/// governs the re-run, and an operator kill switch during a first index does
/// not end it. Against a cloud that reads the list, the last write closes the
/// scan and names the service; no fail marker is sent.
#[tokio::test]
#[serial]
async fn a_budget_refusal_keeps_the_first_index_open() {
    offline_env();
    let tmp = tempfile::tempdir().unwrap();
    let repo = three_service_repo(tmp.path());
    let storage = StubStorage::laptop_on_current_cloud(&[]);

    carrick::agent_service::inject_mock_budget_refusal(
        "/framework-detect",
        BETA_ONLY_DEPENDENCY,
        1,
        REFUSAL_SENTENCE,
    );
    let before = detect_requests();
    run_analysis_engine_with_sidecar(storage.clone(), repo.to_str().unwrap(), None, false)
        .await
        .expect("a budget refusal does not fail a laptop run");

    // alpha, beta (refused), gamma: a refusal is not retried in the same run.
    assert_eq!(detect_requests() - before, 3);
    assert_eq!(storage.uploaded_services(), ["alpha", "beta", "gamma"]);
    let beta = storage.latest("beta").unwrap();
    assert!(
        beta.cached_detection.is_none(),
        "the refused service is analysed facts-only"
    );
    assert!(storage.uploads().last().unwrap().final_in_run);
    assert_eq!(
        *storage.pending_named.lock().unwrap(),
        [vec!["beta".to_string()]]
    );
    assert!(storage.scan_failed.lock().unwrap().is_empty());
}

/// Only beta's `server.ts` holds this, so it is the needle that refuses one
/// file's analysis and no other.
const BETA_ONLY_ROUTE: &str = "/beta/status";

/// A file a budget refused is asked about again on the next scan, and the run
/// says so in the cloud's own words.
///
/// The refusal is the one case where a file the scan dispatched comes back
/// with nothing and the scan is still right: nothing failed, so the file is
/// not lost, the service is not held back, and the only thing owed is the
/// question itself. Nothing chained two scans to prove the question is
/// re-asked — it is true by construction (a refused call records no answer, so
/// the next scan has nothing to replay), and construction is exactly what a
/// later change can quietly alter.
#[tokio::test]
#[serial]
async fn a_refused_file_is_asked_about_again_on_the_next_scan() {
    offline_env();
    let tmp = tempfile::tempdir().unwrap();
    let repo = three_service_repo(tmp.path());
    let storage = StubStorage::laptop_on_current_cloud(&[]);

    // Scan 1: beta's one file is refused, alpha's and gamma's are answered.
    carrick::agent_service::inject_mock_budget_refusal(
        "/analyze-file",
        BETA_ONLY_ROUTE,
        1,
        REFUSAL_SENTENCE,
    );
    let before = analyze_file_requests();
    run_analysis_engine_with_sidecar(storage.clone(), repo.to_str().unwrap(), None, false)
        .await
        .expect("a refused file does not fail the run");
    let first_scan_requests = analyze_file_requests() - before;
    assert_eq!(
        first_scan_requests, 3,
        "one file per service is dispatched on a first index"
    );
    assert_eq!(storage.uploaded_services(), ["alpha", "beta", "gamma"]);

    // The run's own sentence about it is the cloud's, once.
    let line = carrick::scan_health::not_refreshed_line().expect("the run says a file was refused");
    assert!(line.contains(REFUSAL_SENTENCE), "{line}");
    assert_eq!(
        line.matches(REFUSAL_SENTENCE).count(),
        1,
        "one sentence per run: {line}"
    );

    // A refusal is not a loss: the service lands, closes the scan, is not
    // named pending, and its boundary counts no lost file.
    assert!(storage.uploads().last().unwrap().final_in_run);
    assert!(
        storage.pending_named.lock().unwrap().is_empty(),
        "a refused FILE leaves no service pending, so the cloud is never asked \
         to hold a first index open for one"
    );
    assert!(storage.scan_failed.lock().unwrap().is_empty());
    // Counted in the boundary, in the scanner's own words. Until carrick#1419
    // gives a refusal a bucket of its own, `files_lost` is the only one a file
    // with no model answer has, and a boundary that counted nothing here would
    // say this service's index is complete.
    let beta = storage.latest("beta").unwrap();
    let files_lost = &beta.boundary.as_ref().unwrap().files_lost;
    assert_eq!(
        files_lost.total, 1,
        "a refused file is counted, not hidden: {files_lost:?}"
    );
    assert!(
        files_lost
            .reasons
            .iter()
            .any(|reason| reason.contains("server.ts") && reason.contains("not sent to the model")),
        "the reason names the file and says it was not sent: {files_lost:?}"
    );
    assert!(
        !files_lost
            .reasons
            .iter()
            .any(|reason| reason.contains(REFUSAL_SENTENCE)),
        "the cloud's sentence belongs to the run's single line, not to one row \
         per refused file: {files_lost:?}"
    );

    // The mechanism the re-ask rests on: no answer was recorded for the
    // refused file, while its siblings' answers were.
    let refused_answers = beta.file_results.clone().unwrap_or_default();
    assert!(
        !refused_answers
            .keys()
            .any(|path| path.ends_with("server.ts")),
        "a refused file records no answer: {:?}",
        refused_answers.keys().collect::<Vec<_>>()
    );
    let answered = storage
        .latest("alpha")
        .unwrap()
        .file_results
        .unwrap_or_default();
    assert!(
        answered.keys().any(|path| path.ends_with("server.ts")),
        "an answered file records its answer: {:?}",
        answered.keys().collect::<Vec<_>>()
    );

    // Scan 2: same commit, nothing edited, every service indexed. alpha and
    // gamma replay their cached answers and cost no request; beta's refused
    // file is the only one asked about again.
    let rescan = StubStorage {
        indexed: Some(vec!["alpha".into(), "beta".into(), "gamma".into()]),
        ..storage.clone()
    };
    let before = analyze_file_requests();
    run_analysis_engine_with_sidecar(rescan.clone(), repo.to_str().unwrap(), None, false)
        .await
        .expect("the rescan succeeds");
    assert_eq!(
        analyze_file_requests() - before,
        1,
        "the refused file is asked about again, and only it"
    );
    let beta = rescan.latest("beta").unwrap();
    assert!(
        beta.file_results
            .unwrap_or_default()
            .keys()
            .any(|path| path.ends_with("server.ts")),
        "the second scan records the answer the first was refused"
    );
    assert!(rescan.scan_failed.lock().unwrap().is_empty());
}

/// Every limit a Carrick Cloud prompt lambda enforces, by `details.reason`
/// (carrick-cloud#401). The same file `agent_service`'s wire test reads.
fn cloud_limits() -> Vec<String> {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/cloud-limit-refusals/refusals.json")).unwrap();
    let reasons: Vec<String> = fixture["refusals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["reason"].as_str().unwrap().to_string())
        .collect();
    assert!(reasons.len() >= 9, "the fixture lists every limit");
    reasons
}

/// A refusal as every prompt lambda sends one: `llm_disabled`, not retriable,
/// the limit named in `details.reason`, the sentence in `message`.
///
/// Every limit carries [`REFUSAL_SENTENCE`]: the run keeps the first refusal
/// sentence it meets in a process-global, and the other tests in this binary
/// read that line.
fn refusal_envelope(reason: &str) -> String {
    serde_json::json!({
        "success": false,
        "error": {
            "code": "llm_disabled",
            "message": REFUSAL_SENTENCE,
            "retriable": false,
            "details": { "reason": reason, "requestId": "r" },
        },
    })
    .to_string()
}

/// Service names in the uploads after the first `from`, sorted.
fn landed_since(storage: &StubStorage, from: usize) -> Vec<String> {
    let mut landed: Vec<String> = storage.uploads()[from..]
        .iter()
        .filter_map(|u| u.data.service_name.clone())
        .collect();
    landed.sort();
    landed
}

/// Hitting a limit never stops a scan (carrick-cloud#401). For every limit
/// the cloud enforces, a CI scan of a repo that already has an index, whose
/// one file of beta the limit refuses, lands all three services, beta
/// included, and exits 0. It is the strictest case: had the refusal counted
/// as a lost file, beta would be held back and the run would fail.
#[tokio::test]
#[serial]
async fn on_ci_every_limit_on_a_file_still_uploads_every_service() {
    for reason in cloud_limits() {
        offline_env();
        let tmp = tempfile::tempdir().unwrap();
        let repo = three_service_repo(tmp.path());
        let storage = StubStorage::default();
        run_analysis_engine_with_sidecar(storage.clone(), repo.to_str().unwrap(), None, false)
            .await
            .expect("a clean CI scan");
        let first = storage.uploads().len();

        carrick::agent_service::inject_mock_envelope(
            "/analyze-file",
            BETA_ONLY_ROUTE,
            1,
            &refusal_envelope(&reason),
        );
        run_analysis_engine_with_sidecar(storage.clone(), repo.to_str().unwrap(), None, true)
            .await
            .unwrap_or_else(|e| panic!("{reason}: a limit failed the CI run: {e}"));

        assert_eq!(
            landed_since(&storage, first),
            ["alpha", "beta", "gamma"],
            "{reason}: every service lands"
        );
        // The refusal reached the file, and was read as a refusal: beta's own
        // upload says its file was not sent, which a lost file does not.
        let beta = storage.latest("beta").unwrap();
        let files_lost = &beta.boundary.as_ref().unwrap().files_lost;
        assert!(
            files_lost
                .reasons
                .iter()
                .any(|r| r.contains("server.ts") && r.contains("not sent to the model")),
            "{reason}: {files_lost:?}"
        );
    }
}

/// The same for a limit that refuses a service's framework detection, which
/// defers the whole service to facts-only. On CI it fails no run: a first
/// index lands every service, beta facts-only; once beta has an index, beta
/// is held back so its index stays whole rather than thinner, the other two
/// land, and the run still exits 0.
#[tokio::test]
#[serial]
async fn on_ci_a_limit_on_detection_fails_no_run() {
    for reason in cloud_limits() {
        offline_env();
        let tmp = tempfile::tempdir().unwrap();
        let repo = three_service_repo(tmp.path());
        let storage = StubStorage::default();

        carrick::agent_service::inject_mock_envelope(
            "/framework-detect",
            BETA_ONLY_DEPENDENCY,
            1,
            &refusal_envelope(&reason),
        );
        run_analysis_engine_with_sidecar(storage.clone(), repo.to_str().unwrap(), None, false)
            .await
            .unwrap_or_else(|e| panic!("{reason}: a limit failed a first index: {e}"));
        assert_eq!(
            storage.uploaded_services(),
            ["alpha", "beta", "gamma"],
            "{reason}"
        );
        assert!(
            storage.latest("beta").unwrap().cached_detection.is_none(),
            "{reason}: the refused service is analysed facts-only"
        );

        let first = storage.uploads().len();
        carrick::agent_service::inject_mock_envelope(
            "/framework-detect",
            BETA_ONLY_DEPENDENCY,
            1,
            &refusal_envelope(&reason),
        );
        run_analysis_engine_with_sidecar(storage.clone(), repo.to_str().unwrap(), None, true)
            .await
            .unwrap_or_else(|e| panic!("{reason}: a limit failed a CI rescan: {e}"));
        assert_eq!(
            landed_since(&storage, first),
            ["alpha", "gamma"],
            "{reason}: beta's index is kept, not thinned"
        );
    }
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

/// A spent run-wide retry budget skips the in-run retry (carrick#1126): one
/// detection failure that the retry would have absorbed leaves the service
/// pending instead, and the run finishes without asking again.
#[tokio::test]
#[serial]
async fn a_spent_retry_budget_leaves_the_owed_service_pending_without_a_retry() {
    offline_env();
    // SAFETY: `#[serial]`, as in `offline_env`.
    unsafe {
        std::env::set_var(carrick::retry_budget::BUDGET_ENV, "0");
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = three_service_repo(tmp.path());
    let storage = StubStorage::laptop(&[]);

    carrick::agent_service::inject_mock_failure("/framework-detect", BETA_ONLY_DEPENDENCY, 1);
    let before = detect_requests();
    let outcome =
        run_analysis_engine_with_sidecar(storage.clone(), repo.to_str().unwrap(), None, false)
            .await;
    // The budget knob is reset before any assertion can leave it set.
    offline_env();
    outcome.expect("a laptop run with a pending service exits 0");

    assert_eq!(
        detect_requests() - before,
        3,
        "no in-run retry once the budget is spent"
    );
    assert_eq!(storage.uploaded_services(), ["alpha", "beta", "gamma"]);
    let beta = storage.latest("beta").unwrap();
    assert!(beta.cached_detection.is_none() && beta.cached_guidance.is_none());
    assert!(storage.uploads().iter().all(|u| !u.final_in_run));
    let markers = storage.scan_failed.lock().unwrap().clone();
    assert_eq!(markers.len(), 1, "{markers:?}");
    assert!(markers[0].1.contains("beta"), "{markers:?}");
}

/// A guidance failure keeps the detection that answered (carrick#1126). Alpha's
/// guidance fails and the budget skips the in-run retry, so alpha lands
/// pending with its detection cached and no guidance; the rescan asks alpha's
/// guidance again and nobody's detection.
#[tokio::test]
#[serial]
async fn a_guidance_failure_keeps_the_detection_and_the_rescan_asks_guidance_only() {
    offline_env();
    // SAFETY: `#[serial]`, as in `offline_env`.
    unsafe {
        std::env::set_var(carrick::retry_budget::BUDGET_ENV, "0");
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = three_service_repo(tmp.path());
    let storage = StubStorage::laptop(&[]);

    // Alpha is analysed first, so its general guidance request is the first
    // one this matches.
    carrick::agent_service::inject_mock_failure("/framework-guidance", GENERAL_GUIDANCE, 1);
    let outcome =
        run_analysis_engine_with_sidecar(storage.clone(), repo.to_str().unwrap(), None, false)
            .await;
    offline_env();
    outcome.expect("a laptop run with a pending service exits 0");

    let alpha = storage.latest("alpha").unwrap();
    assert!(
        alpha.cached_detection.is_some(),
        "the detection that answered is kept"
    );
    assert!(
        alpha.cached_guidance.is_none(),
        "the guidance that failed is still owed"
    );
    assert!(
        alpha.file_results.as_ref().is_none_or(|r| r.is_empty()),
        "a guidance-deferred service is analysed facts-only"
    );
    for service in ["beta", "gamma"] {
        let data = storage.latest(service).unwrap();
        assert!(data.cached_guidance.is_some(), "{service} is complete");
    }
    let markers = storage.scan_failed.lock().unwrap().clone();
    assert_eq!(markers.len(), 1, "{markers:?}");
    assert!(markers[0].1.contains("alpha"), "{markers:?}");
    assert!(markers[0].1.contains("framework guidance"), "{markers:?}");

    let rescan = StubStorage {
        indexed: Some(vec!["alpha".into(), "beta".into(), "gamma".into()]),
        ..storage.clone()
    };
    let already = storage.uploads().len();
    let detect_before = detect_requests();
    let guidance_before = requests_to("/framework-guidance");
    run_analysis_engine_with_sidecar(rescan.clone(), repo.to_str().unwrap(), None, false)
        .await
        .expect("the rescan succeeds");
    assert_eq!(
        detect_requests() - detect_before,
        0,
        "no detection is asked again"
    );
    // One service's guidance: four pattern categories and the general
    // request. Its extraction config answered the first time and is replayed.
    assert_eq!(
        requests_to("/framework-guidance") - guidance_before,
        5,
        "alpha's guidance, and nothing else, is asked again"
    );
    let alpha = rescan.latest("alpha").unwrap();
    assert!(alpha.cached_detection.is_some() && alpha.cached_guidance.is_some());
    assert!(
        rescan.uploads()[already..].last().unwrap().final_in_run,
        "a complete rescan closes its scan"
    );
}

/// The in-run retry of a guidance failure asks guidance only: three detection
/// requests in all, and every service lands complete (carrick#1126).
#[tokio::test]
#[serial]
async fn the_in_run_retry_of_a_guidance_failure_does_not_ask_detection_again() {
    offline_env();
    let tmp = tempfile::tempdir().unwrap();
    let repo = three_service_repo(tmp.path());
    let storage = StubStorage::laptop(&[]);

    carrick::agent_service::inject_mock_failure("/framework-guidance", GENERAL_GUIDANCE, 1);
    let before = detect_requests();
    run_analysis_engine_with_sidecar(storage.clone(), repo.to_str().unwrap(), None, false)
        .await
        .expect("a recovered guidance failure must not fail the run");

    assert_eq!(detect_requests() - before, 3);
    for service in ["alpha", "beta", "gamma"] {
        let data = storage.latest(service).unwrap();
        assert!(
            data.cached_detection.is_some() && data.cached_guidance.is_some(),
            "{service} is complete"
        );
    }
    assert!(storage.uploads().last().unwrap().final_in_run);
    assert!(storage.scan_failed.lock().unwrap().is_empty());
}
