//! carrick-cloud#1369: a PR run marks each finding with whether main already
//! had it, so the PR check fails only on what the PR introduced.
//!
//! Two repos from `tests/fixtures/local-mode-workspace`: a flat-route
//! producer (`GET /api/v1/widgets/:widgetId`) and a client consumer whose
//! verb is edited to make a wrong-verb finding. Every row is deterministic,
//! `CARRICK_MOCK_ALL` keeps the run offline, and the storage is in memory.

use async_trait::async_trait;
use carrick::cloud_storage::{CloudRepoData, CloudStorage, StorageError, UploadOutcome};
use carrick::engine::run_analysis_engine_with_sidecar;
use carrick::findings::PrResultPayload;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// In-memory index plus every PR result the run posted.
#[derive(Default, Clone)]
struct Store {
    repos: Arc<Mutex<Vec<CloudRepoData>>>,
    pr_results: Arc<Mutex<Vec<PrResultPayload>>>,
}

#[async_trait]
impl CloudStorage for Store {
    async fn upload_repo_data(
        &self,
        data: &CloudRepoData,
        _final_in_run: bool,
    ) -> Result<UploadOutcome, StorageError> {
        let mut repos = self.repos.lock().unwrap();
        repos.retain(|stored| {
            !(stored.repo_name == data.repo_name && stored.service_name == data.service_name)
        });
        repos.push(data.clone());
        Ok(UploadOutcome::default())
    }
    async fn index_landed(
        &self,
        data: &CloudRepoData,
        _written_after: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, StorageError> {
        Ok(self.repos.lock().unwrap().iter().any(|stored| {
            stored.repo_name == data.repo_name
                && stored.service_name == data.service_name
                && stored.commit_hash == data.commit_hash
        }))
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
    async fn post_pr_result(&self, payload: &PrResultPayload) -> Result<(), StorageError> {
        self.pr_results.lock().unwrap().push(payload.clone());
        Ok(())
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
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

fn fixture_repo(root: &Path, name: &str) -> PathBuf {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/local-mode-workspace")
        .join(name);
    let repo = root.join(name);
    copy_dir(&src, &repo);
    git(&repo, &["init", "-q"]);
    commit(&repo, "init");
    repo
}

fn commit(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "--allow-empty", "-m", message]);
}

/// Rewrite the consumer's verb, with `padding` comment lines above the call
/// to move its line the way an unrelated edit would.
fn set_consumer_verb(repo: &Path, verb: &str, padding: usize) {
    let client = repo.join("src/client.ts");
    let source = std::fs::read_to_string(&client).unwrap();
    let body = source
        .lines()
        .filter(|line| !line.starts_with("// pad"))
        .collect::<Vec<_>>()
        .join("\n");
    let body = body
        .replace("method: \"GET\"", &format!("method: \"{verb}\""))
        .replace("method: \"PUT\"", &format!("method: \"{verb}\""));
    let pad: String = (0..padding).map(|i| format!("// pad {i}\n")).collect();
    std::fs::write(&client, format!("{pad}{body}\n")).unwrap();
}

async fn scan(store: &Store, repo: &Path) {
    run_analysis_engine_with_sidecar(store.clone(), repo.to_str().unwrap(), None, false)
        .await
        .expect("scan failed");
}

/// Scan `repo` as PR #7 and return the wrong-verb finding it posted.
async fn pr_scan_wrong_verb(store: &Store, repo: &Path) -> serde_json::Value {
    // SAFETY: the one test in this binary sets these, sequentially.
    unsafe {
        std::env::set_var("GITHUB_REF", "refs/pull/7/merge");
        std::env::set_var("GITHUB_EVENT_NAME", "pull_request");
    }
    scan(store, repo).await;
    unsafe {
        std::env::remove_var("GITHUB_REF");
        std::env::remove_var("GITHUB_EVENT_NAME");
    }
    let payload = store
        .pr_results
        .lock()
        .unwrap()
        .pop()
        .expect("a PR run posts its result");
    let json = serde_json::to_value(&payload).unwrap();
    json["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["kind"] == "method_mismatch" && f["method"] == "PUT")
        .cloned()
        .unwrap_or_else(|| panic!("no PUT method_mismatch in {:#}", json["findings"]))
}

#[tokio::test]
async fn a_pr_run_marks_each_finding_with_whether_main_had_it() {
    // SAFETY: the only test in this binary.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::remove_var("GITHUB_REF");
        std::env::remove_var("GITHUB_EVENT_NAME");
        // The repo name comes from GITHUB_REPOSITORY before the directory, so
        // on a runner both fixtures would be one repo named for this one.
        std::env::remove_var("GITHUB_REPOSITORY");
    }
    let tmp = tempfile::tempdir().unwrap();
    let producer = fixture_repo(tmp.path(), "catalog-web");
    let consumer = fixture_repo(tmp.path(), "inventory-svc");

    // No prior index of the consumer: the field is absent, so the cloud
    // keeps today's behaviour.
    let store = Store::default();
    scan(&store, &producer).await;
    set_consumer_verb(&consumer, "PUT", 0);
    commit(&consumer, "put");
    let finding = pr_scan_wrong_verb(&store, &consumer).await;
    assert!(
        finding.get("on_main").is_none(),
        "no baseline, no field: {finding:#}"
    );

    // Main calls with the right verb; the PR introduces the wrong one.
    set_consumer_verb(&consumer, "GET", 0);
    commit(&consumer, "main: get");
    scan(&store, &consumer).await;
    set_consumer_verb(&consumer, "PUT", 0);
    commit(&consumer, "pr: put");
    let finding = pr_scan_wrong_verb(&store, &consumer).await;
    assert_eq!(finding["on_main"], serde_json::json!(false), "{finding:#}");

    // Main already calls with the wrong verb. The PR only edits the file
    // above the call, so the line moves and the pairing does not.
    scan(&store, &consumer).await;
    set_consumer_verb(&consumer, "PUT", 3);
    commit(&consumer, "pr: unrelated edit above the call");
    let finding = pr_scan_wrong_verb(&store, &consumer).await;
    assert_eq!(finding["on_main"], serde_json::json!(true), "{finding:#}");
}
