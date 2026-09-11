//! The laptop scan's storage: upload to the cloud, and keep a copy.
//!
//! `carrick index` builds the local read model by scanning each repo into a
//! cache directory and joining the blobs back. That has always been a
//! facts-only pass, because nothing local could ask the model. A laptop scan
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
/// knob — `carrick index --infer` is what turns it on. Deliberately opt-in:
/// `carrick refresh` runs from a session-start hook, and a hook that spends
/// real money every time an editor opens is not a feature.
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

    /// The flag is the indexer's handoff and nothing else: unset, or set to
    /// anything but `1`, is a facts-only pass.
    #[test]
    fn the_laptop_flag_is_exactly_one() {
        assert_eq!(LAPTOP_SCAN_ENV, "CARRICK_LAPTOP_SCAN");
    }
}
