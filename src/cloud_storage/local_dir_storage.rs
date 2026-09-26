//! On-disk `CloudStorage` for the offline cross-repo eval harness.
//!
//! `LocalDirStorage` is the storage backend that lets the eval harness run the
//! real scanner binary in two phases without ever touching the carrick cloud:
//!
//! - **Phase A (isolation):** each corpus repo is scanned in its own subprocess
//!   with `CARRICK_LOCAL_STORAGE_ISOLATE=1`, so `download_all_repo_data` returns
//!   *empty* — no real-cloud sibling data (and no other corpus repo) leaks into
//!   the run. `upload_repo_data` serialises that repo's [`CloudRepoData`] to
//!   `<dir>/<repo>.json`.
//! - **Phase B (join):** the binary runs once more without the isolate flag, so
//!   `download_all_repo_data` reads back *all* the cached repos. The engine's
//!   `build_cross_repo_analyzer` then joins them exactly as the cloud path would.
//!
//! A third read source serves the laptop `carrick index` (carrick#1490): with
//! `CARRICK_LOCAL_STORAGE_PEERS=<dir>`, `download_all_repo_data` reads the
//! blobs in `<dir>` — the siblings the build has already indexed — so the
//! repo's own scan joins against them and its upload carries the type
//! verdicts for those pairs, as a CI run's upload does after downloading its
//! siblings' index.
//!
//! The backend is chosen at binary startup purely by the presence of the
//! `CARRICK_LOCAL_STORAGE_DIR` env var (see `main.rs`). The engine never learns
//! it is in eval mode — same contract as `MockStorage`.

use crate::cloud_storage::{
    CloudRepoData, CloudStorage, JobSubmission, StorageError, UploadOutcome,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::debug;

/// Env var holding the cache directory. Its presence at startup also selects
/// this backend over `MockStorage`/`AwsStorage`.
pub const CACHE_DIR_ENV: &str = "CARRICK_LOCAL_STORAGE_DIR";
/// When set to `1`, `download_all_repo_data` returns empty (Phase A isolation).
pub const ISOLATE_ENV: &str = "CARRICK_LOCAL_STORAGE_ISOLATE";
/// A directory of sibling blobs for `download_all_repo_data` to read instead
/// of the cache dir. Ignored when [`ISOLATE_ENV`] is set: isolation is the
/// eval harness's guarantee and nothing may widen it.
pub const PEERS_ENV: &str = "CARRICK_LOCAL_STORAGE_PEERS";
/// A path for `post_pr_result` to write the PR result to, so an offline replay
/// of a PR run can read what the cloud would have been sent (the `on_main`
/// split, carrick-cloud#1408). Unset, the result is dropped as before.
pub const PR_RESULT_OUT_ENV: &str = "CARRICK_PR_RESULT_OUT";

/// Where `download_all_repo_data` reads the cross-repo set from.
#[derive(Debug, Clone, PartialEq)]
pub enum CrossRepoReads {
    /// Nothing: the eval harness's Phase A, and the free `carrick refresh`.
    Isolated,
    /// Every blob in the cache dir: the join.
    CacheDir,
    /// Every blob in this directory: a laptop scan reading the siblings its
    /// build has already indexed (carrick#1490).
    Peers(PathBuf),
}

impl CrossRepoReads {
    fn from_env() -> Self {
        if std::env::var(ISOLATE_ENV).as_deref() == Ok("1") {
            return Self::Isolated;
        }
        match std::env::var_os(PEERS_ENV) {
            Some(dir) if !dir.is_empty() => Self::Peers(PathBuf::from(dir)),
            _ => Self::CacheDir,
        }
    }
}

pub struct LocalDirStorage {
    cache_dir: PathBuf,
    reads: CrossRepoReads,
}

impl LocalDirStorage {
    /// Construct from the `CARRICK_LOCAL_STORAGE_DIR` / `CARRICK_LOCAL_STORAGE_ISOLATE`
    /// / `CARRICK_LOCAL_STORAGE_PEERS` env vars. Creates the cache dir if it
    /// does not exist.
    pub fn from_env() -> Result<Self, StorageError> {
        let cache_dir = std::env::var(CACHE_DIR_ENV)
            .map_err(|_| StorageError::ConnectionError(format!("{CACHE_DIR_ENV} is not set")))?;
        Self::new(PathBuf::from(cache_dir), CrossRepoReads::from_env())
    }

    pub fn new(cache_dir: PathBuf, reads: CrossRepoReads) -> Result<Self, StorageError> {
        std::fs::create_dir_all(&cache_dir).map_err(|e| {
            StorageError::ConnectionError(format!(
                "Failed to create local storage dir {}: {e}",
                cache_dir.display()
            ))
        })?;
        Ok(Self { cache_dir, reads })
    }

    /// Sanitize a `(repo, service)` pair into a single-segment file stem. The
    /// service is part of the key so a multi-service repo writes one file per
    /// service instead of clobbering itself down to a single repo file; the
    /// sanitisation also stops a repo/service id containing a path separator
    /// from escaping the cache dir.
    fn cache_path(&self, repo_name: &str, service_name: Option<&str>) -> PathBuf {
        let key = match service_name {
            Some(svc) if !svc.is_empty() => format!("{repo_name}__{svc}"),
            _ => repo_name.to_string(),
        };
        let safe: String = key
            .chars()
            .map(|c| if c == '/' || c == '\\' { '_' } else { c })
            .collect();
        self.cache_dir.join(format!("{safe}.json"))
    }

    /// Write `data` to its cache file.
    pub(crate) fn write_cache_file(&self, data: &CloudRepoData) -> Result<(), StorageError> {
        let path = self.cache_path(&data.repo_name, data.service_name.as_deref());
        debug!(
            "LOCAL: Uploading repo data for {} (service: {:?}) -> {}",
            data.repo_name,
            data.service_name,
            path.display()
        );
        let json = serde_json::to_string_pretty(data)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;
        std::fs::write(&path, json).map_err(|e| {
            StorageError::ConnectionError(format!("Failed to write {}: {e}", path.display()))
        })
    }
}

#[async_trait]
impl CloudStorage for LocalDirStorage {
    /// An offline run takes every job it is handed: there is no deployment to
    /// wait for, and the harness's whole interest is in the bundle itself.
    fn accepts_analysis_job(&self) -> bool {
        true
    }

    /// Write the dispatched job where an offline run can read it back.
    ///
    /// The eval harness and the dispatch tests need the bundle itself — the
    /// rows, their ids, what the header carries once — and nothing about it is
    /// a property of the cloud. So this backend takes the job, names it after
    /// the repo, and answers as the cloud would (carrick#1229).
    async fn submit_analysis_job(
        &self,
        bundle: &crate::analysis_job::JobBundle,
    ) -> Result<Option<JobSubmission>, StorageError> {
        let bytes = bundle.encode().map_err(StorageError::SerializationError)?;
        let path = self
            .cache_path(&bundle.header.repo, None)
            .with_extension("analysis-job.ndjson.gz");
        std::fs::write(&path, &bytes).map_err(|e| {
            StorageError::ConnectionError(format!("could not write {}: {e}", path.display()))
        })?;
        debug!(
            "Wrote a {} row analysis job to {}",
            bundle.analyze.len(),
            path.display()
        );
        Ok(Some(JobSubmission {
            job_id: crate::analysis_job::digest(&bytes).0[..12].to_string(),
            analyze_rows: bundle.analyze.len(),
        }))
    }

    async fn upload_repo_data(
        &self,
        data: &CloudRepoData,
        _final_in_run: bool,
    ) -> Result<UploadOutcome, StorageError> {
        self.write_cache_file(data)?;
        // The cache file is rewritten unconditionally — there is no freshness
        // check to short-circuit on — and nothing here is metered, so there is
        // no spend to report either.
        Ok(UploadOutcome::default())
    }

    /// Read the cache file back and compare the commit it carries.
    ///
    /// A local write fails outright or not at all — there is no lost response
    /// to arbitrate here and nothing that could have written this file but
    /// this run — so `written_after` has nothing to separate and the commit is
    /// the whole question. An unreadable or unparseable file is "not at this
    /// commit" (carrick#1067).
    async fn index_landed(
        &self,
        data: &CloudRepoData,
        _written_after: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, StorageError> {
        let path = self.cache_path(&data.repo_name, data.service_name.as_deref());
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Ok(false);
        };
        Ok(serde_json::from_str::<CloudRepoData>(&content)
            .is_ok_and(|stored| stored.commit_hash == data.commit_hash))
    }

    // Cache files are keyed by (repo, service), so each service of a
    // multi-service repo persists to its own file without clobbering — same
    // property as MockStorage.
    fn supports_multi_service(&self) -> bool {
        true
    }

    // Writes to a local file, so no request-size wall applies and the caches
    // are kept whatever the payload weighs.
    fn stages_oversized_payloads(&self) -> bool {
        true
    }

    async fn download_all_repo_data(
        &self,
    ) -> Result<(Vec<CloudRepoData>, HashMap<String, String>), StorageError> {
        // Phase A: upload-only. Returning empty is the load-bearing isolation —
        // without it the real cloud (or a sibling corpus repo) would inject data
        // into the per-repo scan and break Tier-A fidelity.
        let dir = match &self.reads {
            CrossRepoReads::Isolated => {
                debug!("LOCAL: isolate mode — returning empty cross-repo set");
                return Ok((Vec::new(), HashMap::new()));
            }
            CrossRepoReads::CacheDir => &self.cache_dir,
            CrossRepoReads::Peers(dir) => dir,
        };

        let mut repos = Vec::new();
        let entries = std::fs::read_dir(dir).map_err(|e| {
            StorageError::ConnectionError(format!(
                "Failed to read local storage dir {}: {e}",
                dir.display()
            ))
        })?;
        // Collect + sort paths so the joined order is deterministic across runs.
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
            .collect();
        paths.sort();
        for path in paths {
            let content = std::fs::read_to_string(&path).map_err(|e| {
                StorageError::ConnectionError(format!("Failed to read {}: {e}", path.display()))
            })?;
            let data: CloudRepoData = serde_json::from_str(&content).map_err(|e| {
                StorageError::SerializationError(format!("Failed to parse {}: {e}", path.display()))
            })?;
            repos.push(data);
        }

        // S3 URL map is unused offline; supply a stable local marker per repo so
        // any consumer expecting a key per repo still finds one.
        let urls = repos
            .iter()
            .map(|r| (r.repo_name.clone(), format!("file://{}", r.repo_name)))
            .collect();

        debug!("LOCAL: Downloaded {} cached repos", repos.len());
        Ok((repos, urls))
    }

    async fn health_check(&self) -> Result<(), StorageError> {
        debug!("LOCAL: Health check passed");
        Ok(())
    }

    async fn upload_logs(&self, repo: &str, _log_content: &str) -> Result<(), StorageError> {
        debug!("LOCAL: Skipping log upload for {}", repo);
        Ok(())
    }

    /// Nothing to ship: this backend is the offline harness and the local
    /// join, and neither has a cloud to send a log to. Saying so here stops the
    /// engine at its gate, where it used to call the no-op above and then log
    /// "Uploaded run logs to S3" for a run that sent nothing, which read as
    /// the join having uploaded the laptop's log.
    fn uploads_run_logs(&self) -> bool {
        false
    }

    async fn upload_type_file(
        &self,
        repo_name: &str,
        file_name: &str,
        _content: &str,
    ) -> Result<(), StorageError> {
        debug!(
            "LOCAL: Skipping type-file upload for {} / {}",
            repo_name, file_name
        );
        Ok(())
    }

    async fn post_pr_result(
        &self,
        payload: &crate::findings::PrResultPayload,
    ) -> Result<(), StorageError> {
        let Some(out) = std::env::var_os(PR_RESULT_OUT_ENV).filter(|out| !out.is_empty()) else {
            debug!(
                "LOCAL: Skipping PR result for {} (PR #{})",
                payload.repo, payload.pr_number
            );
            return Ok(());
        };
        let json = serde_json::to_string_pretty(payload)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;
        std::fs::write(&out, json).map_err(|e| {
            StorageError::ConnectionError(format!(
                "could not write the PR result to {}: {e}",
                PathBuf::from(&out).display()
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_service_cache_paths_do_not_clobber() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            LocalDirStorage::new(dir.path().to_path_buf(), CrossRepoReads::CacheDir).unwrap();

        // Two services in the SAME repo must land in distinct files (the bug:
        // keying by repo_name alone clobbered orders-pkg with gateway).
        let orders = store.cache_path("orders-monorepo", Some("orders-pkg"));
        let gateway = store.cache_path("orders-monorepo", Some("gateway"));
        assert_ne!(orders, gateway);

        // A single-service repo (no service name) keeps the bare repo file name.
        assert_eq!(
            store.cache_path("payments-svc", None),
            dir.path().join("payments-svc.json")
        );

        // Path separators in either component are neutralised (no dir escape).
        assert_eq!(
            store.cache_path("a/b", Some("c\\d")),
            dir.path().join("a_b__c_d.json")
        );
    }

    /// The local join runs after every laptop scan, in the same log file. When
    /// it claimed a run log it never sent, that line was the only "Uploaded
    /// run logs" a reader of the laptop's log could find.
    #[test]
    fn local_storage_does_not_claim_to_ship_run_logs() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            LocalDirStorage::new(dir.path().to_path_buf(), CrossRepoReads::CacheDir).unwrap();
        assert!(!store.uploads_run_logs());
    }

    /// carrick#1490: a laptop scan reads its siblings from the peers directory
    /// and never from its own cache dir, which holds only what it wrote.
    #[tokio::test]
    async fn peers_reads_the_peers_directory_and_not_the_cache_dir() {
        let cache = tempfile::tempdir().unwrap();
        let peers = tempfile::tempdir().unwrap();
        let sibling = serde_json::json!({
            "repo_name": "producer",
            "endpoints": [], "calls": [], "mounts": [], "apps": {},
            "imported_handlers": [], "function_definitions": {},
            "last_updated": "2026-09-25T00:00:00Z",
            "commit_hash": "4f2a1c9"
        });
        std::fs::write(peers.path().join("local-0-0.json"), sibling.to_string()).unwrap();
        std::fs::write(cache.path().join("own.json"), "{ not json }").unwrap();

        let store = LocalDirStorage::new(
            cache.path().to_path_buf(),
            CrossRepoReads::Peers(peers.path().to_path_buf()),
        )
        .unwrap();
        let (repos, _) = store.download_all_repo_data().await.unwrap();
        assert_eq!(
            repos
                .iter()
                .map(|r| r.repo_name.as_str())
                .collect::<Vec<_>>(),
            ["producer"]
        );

        let isolated =
            LocalDirStorage::new(cache.path().to_path_buf(), CrossRepoReads::Isolated).unwrap();
        assert!(
            isolated
                .download_all_repo_data()
                .await
                .unwrap()
                .0
                .is_empty()
        );
    }
}
