//! The laptop scan's storage: upload to the cloud, and keep a copy.
//!
//! `carrick index` builds the local read model by scanning each repo into a
//! cache directory and joining the blobs back. `carrick refresh` does that
//! facts-only, because nothing local could ask the model. A laptop scan
//! can, and the index it produces is the one the cloud stores — so writing it
//! to the cache directory at the same time removes the upload-then-download
//! round trip entirely, and the `.carrick` index is built from the run that
//! produced it rather than from a later read of the cloud
//! (carrick-cloud `docs/internal/reference/laptop-scan-seam.md` §8.3).
//!
//! It also makes §2.4 cost the laptop nothing: a dirty run deliberately sends
//! no `file_results` to the cloud, and the local copy is written from the same
//! payload, so the next local scan reads back what this one computed.
//!
//! Cross-repo reads come from the LOCAL side, so the per-repo phase stays
//! isolated exactly as it is today and the join phase reads every blob the
//! run wrote. Everything else — opening the run, the capability flags, the
//! logs — is the cloud's.

use async_trait::async_trait;
use std::collections::HashMap;

use crate::cloud_storage::{
    AwsStorage, CloudRepoData, CloudStorage, LocalDirStorage, RunContext, RunStart, StorageError,
    UploadOutcome,
};

/// Set to `1` alongside `CARRICK_LOCAL_STORAGE_DIR` to make a local-index scan
/// a laptop scan: model analysis through the cloud, and an upload.
///
/// The indexer's handoff to the scan subprocess it spawns, not a user-facing
/// knob — `carrick index` is what turns it on. Deliberately off for the other
/// command: `carrick refresh` runs from a session-start hook, and a hook that
/// spends real money every time an editor opens is not a feature.
pub const LAPTOP_SCAN_ENV: &str = "CARRICK_LAPTOP_SCAN";

/// Whether this process was asked to run as a laptop scan.
pub fn laptop_scan_requested() -> bool {
    std::env::var(LAPTOP_SCAN_ENV).as_deref() == Ok("1")
}

pub struct TeeStorage {
    cloud: AwsStorage,
    local: LocalDirStorage,
}

impl TeeStorage {
    pub fn from_env(force_reindex: bool) -> Result<Self, StorageError> {
        Ok(Self {
            cloud: AwsStorage::new(force_reindex)?,
            local: LocalDirStorage::from_env()?,
        })
    }
}

#[async_trait]
impl CloudStorage for TeeStorage {
    /// Local first, then the cloud, and the cloud's error is propagated.
    ///
    /// Order matters: the analysis has already been paid for by the time
    /// either write happens, and the local copy is the one thing that survives
    /// a refusal. A `409 partial_refused` still fails the run — the cloud's
    /// index is not written and the user is told — but the developer keeps the
    /// index they just paid for, which is the whole point of writing it here.
    async fn upload_repo_data(
        &self,
        data: &CloudRepoData,
        final_in_run: bool,
    ) -> Result<UploadOutcome, StorageError> {
        self.local.upload_repo_data(data, final_in_run).await?;
        self.cloud.upload_repo_data(data, final_in_run).await
    }

    async fn begin_run(&self, run: &RunContext) -> Result<RunStart, StorageError> {
        self.cloud.begin_run(run).await
    }

    /// The local copy only: the cloud already serves this generation, and the
    /// local read model is built from this run's blobs, so a service the run
    /// held back would otherwise vanish from it.
    fn keep_served_generation(&self, data: &CloudRepoData) {
        if let Err(error) = self.local.write_cache_file(data) {
            tracing::warn!("Could not keep the served generation locally: {error}");
        }
    }

    /// The CLOUD side, unlike the cross-repo read below. The question is only
    /// ever asked about a cloud write whose response was lost, and the local
    /// copy — written first, and successfully, or this would not be running —
    /// would answer `true` to every one of them (carrick#1067).
    async fn index_landed(
        &self,
        data: &CloudRepoData,
        written_after: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, StorageError> {
        self.cloud.index_landed(data, written_after).await
    }

    fn supports_multi_service(&self) -> bool {
        self.cloud.supports_multi_service()
    }

    fn stages_oversized_payloads(&self) -> bool {
        self.cloud.stages_oversized_payloads()
    }

    /// The local side, so the per-repo phase stays isolated and the join phase
    /// reads back every blob this run wrote. The sibling data a laptop needs
    /// arrives as `previous_data`, which the indexer hands in from the hosted
    /// snapshot rather than from a cross-repo download.
    async fn download_all_repo_data(
        &self,
    ) -> Result<(Vec<CloudRepoData>, HashMap<String, String>), StorageError> {
        self.local.download_all_repo_data().await
    }

    async fn upload_type_file(
        &self,
        repo_name: &str,
        file_name: &str,
        content: &str,
    ) -> Result<(), StorageError> {
        self.cloud
            .upload_type_file(repo_name, file_name, content)
            .await
    }

    async fn health_check(&self) -> Result<(), StorageError> {
        self.cloud.health_check().await
    }

    async fn upload_logs(&self, repo: &str, log_content: &str) -> Result<(), StorageError> {
        self.cloud.upload_logs(repo, log_content).await
    }

    fn uploads_run_logs(&self) -> bool {
        self.cloud.uploads_run_logs()
    }

    /// The marker goes to the cloud, which is the only side of the tee that
    /// holds a scan slot; the local cache has nothing to record it against.
    async fn report_scan_failed(&self, stage: &str, reason: &str) {
        self.cloud.report_scan_failed(stage, reason).await;
    }

    async fn report_preflight_failed(&self, repo: Option<&str>, stage: &str, reason: &str) {
        self.cloud
            .report_preflight_failed(repo, stage, reason)
            .await;
    }

    async fn post_pr_result(
        &self,
        payload: &crate::findings::PrResultPayload,
    ) -> Result<(), StorageError> {
        self.cloud.post_pr_result(payload).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::CloudAuth;

    fn blob() -> CloudRepoData {
        serde_json::from_value(serde_json::json!({
            "repo_name": "api",
            "endpoints": [], "calls": [], "mounts": [], "apps": {},
            "imported_handlers": [], "function_definitions": {},
            "last_updated": "2026-09-11T00:00:00Z",
            "commit_hash": "4f2a1c9"
        }))
        .unwrap()
    }

    fn tee(
        responses: Vec<(u16, String)>,
        cache: &std::path::Path,
    ) -> (TeeStorage, std::thread::JoinHandle<Vec<String>>) {
        let (base, server) = crate::agent_service::tests::stub_server(responses);
        let cloud = AwsStorage::for_test(
            &format!("{base}/types/check-or-upload"),
            CloudAuth::Bearer("carrick_sk_live_test".to_string()),
            false,
        );
        let local = LocalDirStorage::new(cache.to_path_buf(), true).unwrap();
        (TeeStorage { cloud, local }, server)
    }

    fn check_ok() -> (u16, String) {
        (
            200,
            serde_json::json!({
                "exists": true, "s3Url": "s3://bucket/api.json",
                "uploadUrl": null, "hash": "4f2a1c9"
            })
            .to_string(),
        )
    }

    /// One run, two destinations: the index the laptop paid for lands in the
    /// cache directory the read model is built from, and in the cloud. That is
    /// what removes the upload-then-download round trip (§8.3).
    #[tokio::test]
    async fn an_uploaded_payload_is_also_written_to_the_cache_directory() {
        let dir = tempfile::tempdir().unwrap();
        let (storage, server) = tee(
            vec![
                check_ok(),
                (200, serde_json::json!({ "success": true }).to_string()),
            ],
            dir.path(),
        );

        storage.upload_repo_data(&blob(), true).await.unwrap();

        assert_eq!(server.join().unwrap().len(), 2, "the cloud was written to");
        let written = dir.path().join("api.json");
        assert!(written.exists(), "the local copy was not written");
        let local: CloudRepoData =
            serde_json::from_slice(&std::fs::read(&written).unwrap()).unwrap();
        assert_eq!(local.repo_name, "api");
        assert_eq!(local.commit_hash, "4f2a1c9");
    }

    /// The analysis has already been paid for by the time either write
    /// happens, so a cloud refusal still fails the run — the user is told, and
    /// the cloud's index is not written — but the developer keeps the index
    /// they just bought.
    #[tokio::test]
    async fn a_refused_upload_still_leaves_the_local_index_behind() {
        let dir = tempfile::tempdir().unwrap();
        let (storage, server) = tee(
            vec![
                check_ok(),
                (
                    409,
                    serde_json::json!({
                        "error": "This service already has an index.",
                        "code": "partial_refused"
                    })
                    .to_string(),
                ),
            ],
            dir.path(),
        );

        let message = storage
            .upload_repo_data(&blob(), true)
            .await
            .unwrap_err()
            .to_string();
        assert!(message.contains("partial_refused"), "{message}");
        assert!(
            dir.path().join("api.json").exists(),
            "a refusal must not take the paid-for local index with it"
        );
        server.join().unwrap();
    }

    /// The cross-repo read is the local one, so phase 1 stays isolated exactly
    /// as it is on a facts-only pass and no sibling's data reaches a repo's own
    /// scan.
    #[tokio::test]
    async fn cross_repo_reads_come_from_the_isolated_local_side() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sibling.json"), "{ not json }").unwrap();
        let (storage, server) = tee(vec![], dir.path());
        let (repos, urls) = storage.download_all_repo_data().await.unwrap();
        assert!(repos.is_empty(), "isolation was not honoured");
        assert!(urls.is_empty());
        server.join().unwrap();
    }

    /// carrick#1022, over a real checkout: a clone that landed in a folder
    /// which is not the repository's name uploads under the repository's name,
    /// and keeps the folder's name on the copy the local join reads.
    ///
    /// The reproduction is the whole point — `git clone <url>` names the folder
    /// after the repo, so this only bites a clone given a target name or a
    /// renamed directory, and it bites it after the analysis is paid for.
    #[tokio::test]
    async fn a_clone_in_a_differently_named_folder_uploads_under_the_repo_name() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("ws");
        std::fs::create_dir(&checkout).unwrap();
        git(&checkout, &["init"]);
        git(
            &checkout,
            &["remote", "add", "origin", "https://github.com/acme/api.git"],
        );

        // The two names the scan has for one repository, derived the way the
        // engine derives them. The blob's is the directory: the indexer strips
        // `GITHUB_REPOSITORY` from the child scan, so `get_repository_name`
        // falls back to it. Read here through `file_name` rather than that
        // helper because CI sets `GITHUB_REPOSITORY` for the test process
        // itself, and it would win over the fixture.
        let remote = crate::git_state::remote_name(&checkout);
        assert_eq!(remote.as_deref(), Some("acme/api"));
        let folder = checkout.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(folder, "ws");

        let cache = dir.path().join("cache");
        let (storage, server) = tee(
            vec![
                (
                    200,
                    serde_json::json!({
                        "schema": "carrick.start-scan/0",
                        "scan_id": "scan_01J",
                        "project_id": "proj_1",
                        "project_slug": "acme",
                        "indexed_services": [],
                        "multi_service": true
                    })
                    .to_string(),
                ),
                check_ok(),
                (200, serde_json::json!({ "success": true }).to_string()),
            ],
            &cache,
        );

        storage
            .begin_run(&RunContext {
                repo_full_name: remote,
                commit: "4f2a1c9".to_string(),
                dirty: false,
            })
            .await
            .unwrap();
        let mut payload = blob();
        payload.repo_name = folder;
        storage.upload_repo_data(&payload, true).await.unwrap();

        let requests = server.join().unwrap();
        let write = body_of(&requests[2]);
        assert_eq!(body_of(&requests[1])["repo"], "api");
        assert_eq!(write["repo"], "api", "{write}");
        assert_eq!(write["cloudRepoData"]["repo_name"], "api", "{write}");

        // And the local half is untouched: every cached blob is keyed on the
        // directory label, and a scan that renamed its own copy would be
        // invisible to the join that reads it back.
        let cached: CloudRepoData =
            serde_json::from_slice(&std::fs::read(cache.join("ws.json")).unwrap()).unwrap();
        assert_eq!(cached.repo_name, "ws");
        assert!(
            !cache.join("api.json").exists(),
            "the local copy was renamed with the upload"
        );
    }

    fn git(repo: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("git is on PATH");
        assert!(
            status.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    fn body_of(request: &str) -> serde_json::Value {
        let (_, body) = request
            .split_once("\r\n\r\n")
            .expect("a request with a body");
        serde_json::from_str(body).expect("a JSON body")
    }

    /// The flag is the indexer's handoff and nothing else: unset, or set to
    /// anything but `1`, is a facts-only pass.
    #[test]
    fn the_laptop_flag_is_exactly_one() {
        assert_eq!(LAPTOP_SCAN_ENV, "CARRICK_LAPTOP_SCAN");
    }
}
