//! End-to-end check for issue #110: the incremental scan path must populate
//! `FunctionDefinition.intent` and strip `body_source`, just like the full
//! analysis path does. Before the fix, the incremental path skipped intent
//! generation entirely, leaving DDB rows with `body_source` populated and
//! `intent` absent.

use async_trait::async_trait;
use carrick::cloud_storage::{CloudRepoData, CloudStorage, MockStorage, StorageError};
use carrick::engine::run_analysis_engine_with_sidecar;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct SharedMock(Arc<MockStorage>);

#[async_trait]
impl CloudStorage for SharedMock {
    async fn upload_repo_data(&self, data: &CloudRepoData) -> Result<(), StorageError> {
        self.0.upload_repo_data(data).await
    }
    async fn download_all_repo_data(
        &self,
    ) -> Result<(Vec<CloudRepoData>, HashMap<String, String>), StorageError> {
        self.0.download_all_repo_data().await
    }
    async fn upload_type_file(
        &self,
        repo_name: &str,
        file_name: &str,
        content: &str,
    ) -> Result<(), StorageError> {
        self.0.upload_type_file(repo_name, file_name, content).await
    }
    async fn health_check(&self) -> Result<(), StorageError> {
        self.0.health_check().await
    }
    async fn upload_logs(&self, repo: &str, log_content: &str) -> Result<(), StorageError> {
        self.0.upload_logs(repo, log_content).await
    }
    async fn post_pr_result(
        &self,
        payload: &carrick::findings::PrResultPayload,
    ) -> Result<(), StorageError> {
        self.0.post_pr_result(payload).await
    }
}

/// Plain in-memory storage with no synthetic seed-repos and direct write
/// access to the uploaded data. Used by the reuse test below to mutate
/// scan #1's stored payload between scans.
#[derive(Default, Clone)]
struct StubStorage {
    repos: Arc<Mutex<Vec<CloudRepoData>>>,
}

#[async_trait]
impl CloudStorage for StubStorage {
    async fn upload_repo_data(&self, data: &CloudRepoData) -> Result<(), StorageError> {
        self.repos.lock().unwrap().push(data.clone());
        Ok(())
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

fn run_git(dir: &std::path::Path, args: &[&str]) {
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

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
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

#[tokio::test]
async fn incremental_path_populates_intent() {
    // Mock all lambda + storage interactions.
    // SAFETY: tests in this binary share env, but no other test in this file
    // depends on these vars.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        // The engine refuses to upload from a pull_request run or a
        // non-main ref (anti-pollution guard). This scan targets a temp repo
        // and a mock store, so the runner's GitHub context must not apply —
        // in CI on a PR it made scan #1 upload nothing.
        std::env::remove_var("GITHUB_EVENT_NAME");
        std::env::remove_var("GITHUB_REF");
    }

    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/koa-api");
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo_path = tmp.path().join("koa");
    copy_dir(&fixture, &repo_path);

    run_git(&repo_path, &["init", "-q"]);
    run_git(&repo_path, &["add", "-A"]);
    run_git(&repo_path, &["commit", "-q", "-m", "init"]);

    let storage = SharedMock(Arc::new(MockStorage::new()));

    // Scan #1 — full path. Populates cache fields so scan #2 takes the
    // incremental branch.
    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #1 failed");

    // Make a trivial change so HEAD differs from the previous commit_hash —
    // git diff in the incremental path needs a non-empty diff target.
    std::fs::write(repo_path.join("noop.ts"), "// touch\n").unwrap();
    run_git(&repo_path, &["add", "-A"]);
    run_git(&repo_path, &["commit", "-q", "-m", "noop"]);

    // Scan #2 — should take the incremental branch.
    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #2 failed");

    // Inspect the most-recently uploaded payload for this repo.
    let (uploaded, _) = storage
        .0
        .download_all_repo_data()
        .await
        .expect("download failed");

    // download_all_repo_data adds synthetic seed repos when total <= 1, so
    // filter for our actual repo by package.json signature.
    let our_repo = uploaded
        .iter()
        .rfind(|r| {
            r.package_json
                .as_ref()
                .map(|j| j.contains("koa"))
                .unwrap_or(false)
        })
        .expect("no koa repo upload found");

    // After the fix, the latest (incremental) upload must have intents
    // populated and body_source stripped, same as the full-path upload would.
    let with_intent = our_repo
        .function_definitions
        .values()
        .filter(|d| d.intent.is_some())
        .count();
    let with_body = our_repo
        .function_definitions
        .values()
        .filter(|d| d.body_source.is_some())
        .count();

    assert!(
        with_intent > 0,
        "incremental upload should populate intent on at least one function (got {} of {})",
        with_intent,
        our_repo.function_definitions.len()
    );
    assert_eq!(
        with_body, 0,
        "incremental upload should strip body_source on all functions (still set on {})",
        with_body
    );
}

/// Verifies the reuse optimization: in incremental mode, intents from
/// `previous_data.function_definitions` are carried forward for functions
/// whose source file did not change, so /generate-intent is not called
/// again for them.
///
/// Method: between scans, mutate scan #1's stored upload to set a sentinel
/// intent on every function. Scan #2 runs incrementally with that mutated
/// payload as `previous_data`. The mock /generate-intent returns
/// "Mock intent: function does something." — distinct from the sentinel.
/// If the sentinel survives to scan #2's upload, reuse worked.
#[tokio::test]
async fn incremental_path_reuses_intents_from_previous_scan() {
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        // The engine refuses to upload from a pull_request run or a
        // non-main ref (anti-pollution guard). This scan targets a temp repo
        // and a mock store, so the runner's GitHub context must not apply —
        // in CI on a PR it made scan #1 upload nothing.
        std::env::remove_var("GITHUB_EVENT_NAME");
        std::env::remove_var("GITHUB_REF");
    }

    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/koa-api");
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo_path = tmp.path().join("koa");
    copy_dir(&fixture, &repo_path);

    run_git(&repo_path, &["init", "-q"]);
    run_git(&repo_path, &["add", "-A"]);
    run_git(&repo_path, &["commit", "-q", "-m", "init"]);

    let storage = StubStorage::default();
    const SENTINEL: &str = "SENTINEL_REUSED_INTENT";

    // Scan #1 — full path. Populates cache fields used by scan #2.
    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #1 failed");

    // Inject sentinel intents onto scan #1's stored payload. Scan #2 will
    // pick this up as `previous_data`.
    {
        let mut repos = storage.repos.lock().unwrap();
        let prev = repos
            .iter_mut()
            .rfind(|r| {
                r.package_json
                    .as_ref()
                    .map(|j| j.contains("koa"))
                    .unwrap_or(false)
            })
            .expect("no prior upload to mutate");
        assert!(
            !prev.function_definitions.is_empty(),
            "scan #1 should have produced function definitions"
        );
        for def in prev.function_definitions.values_mut() {
            def.intent = Some(SENTINEL.to_string());
        }
    }

    // Trivial change in a *different* file so server.ts (where the koa
    // handlers live) stays unchanged — its functions should be reused.
    std::fs::write(repo_path.join("noop.ts"), "// touch\n").unwrap();
    run_git(&repo_path, &["add", "-A"]);
    run_git(&repo_path, &["commit", "-q", "-m", "noop"]);

    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #2 failed");

    let repos = storage.repos.lock().unwrap();
    let scan2 = repos
        .iter()
        .rfind(|r| {
            r.package_json
                .as_ref()
                .map(|j| j.contains("koa"))
                .unwrap_or(false)
        })
        .expect("no scan #2 upload found");

    let sentinel_count = scan2
        .function_definitions
        .values()
        .filter(|d| d.intent.as_deref() == Some(SENTINEL))
        .count();

    assert!(
        sentinel_count > 0,
        "scan #2 should reuse at least one sentinel intent from prev (got {} of {} fns; intents: {:?})",
        sentinel_count,
        scan2.function_definitions.len(),
        scan2
            .function_definitions
            .values()
            .map(|d| d.intent.clone())
            .collect::<Vec<_>>()
    );
}
