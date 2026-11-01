use crate::cloud_storage::{
    CloudRepoData, CloudStorage, StorageError, TypeReachObject, TypeReachRecord, UploadOutcome,
    decode_type_reach, type_reach,
};
use crate::packages::{PackageJson, Packages};
use async_trait::async_trait;
use chrono::Utc;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;
use tracing::debug;

/// `(repo, stage, reason)`, as one pre-scan failure was reported.
type PreflightFailure = (Option<String>, String, String);

/// `(repo, service, commit)`: where a stored type-reach object sits, beside
/// the blob of the same key, as the cloud keys it under the blob's prefix.
type ReachKey = (String, Option<String>, String);

fn reach_key(data: &CloudRepoData) -> ReachKey {
    (
        data.repo_name.clone(),
        data.service_name.clone(),
        data.commit_hash.clone(),
    )
}

pub struct MockStorage {
    data: Mutex<Vec<CloudRepoData>>,
    type_files: Mutex<HashMap<String, String>>,
    /// Every `(stage, reason)` this storage was asked to mark, so a test can
    /// read back what a failing run reported (carrick#1063).
    scan_failures: Mutex<Vec<(String, String)>>,
    /// Every `(repo, stage, reason)` a run that never opened a scan reported
    /// (carrick#1096).
    preflight_failures: Mutex<Vec<PreflightFailure>>,
    /// Every run log this storage was handed, as the redaction left it.
    logs: Mutex<Vec<String>>,
    /// The type-reach objects written beside the blobs (carrick#3101).
    type_reach: Mutex<HashMap<ReachKey, Vec<u8>>>,
}

impl Default for MockStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl MockStorage {
    pub fn new() -> Self {
        Self {
            data: Mutex::new(Vec::new()),
            type_files: Mutex::new(HashMap::new()),
            scan_failures: Mutex::new(Vec::new()),
            preflight_failures: Mutex::new(Vec::new()),
            logs: Mutex::new(Vec::new()),
            type_reach: Mutex::new(HashMap::new()),
        }
    }

    /// The failures reported against this storage, in order.
    ///
    /// Read by tests only, the library's and the binary's. Not `cfg(test)`:
    /// the binary's unit tests link the library built without it.
    pub fn scan_failures(&self) -> Vec<(String, String)> {
        self.scan_failures.lock().unwrap().clone()
    }

    /// The pre-scan failures reported against this storage, in order.
    pub fn preflight_failures(&self) -> Vec<PreflightFailure> {
        self.preflight_failures.lock().unwrap().clone()
    }

    /// The run logs this storage was handed, in order, as the redaction left
    /// them.
    pub fn uploaded_logs(&self) -> Vec<String> {
        self.logs.lock().unwrap().clone()
    }
}

#[async_trait]
impl CloudStorage for MockStorage {
    async fn upload_repo_data(
        &self,
        data: &CloudRepoData,
        _final_in_run: bool,
    ) -> Result<UploadOutcome, StorageError> {
        debug!(
            "MOCK: Uploading repo data for repo: {} (service: {:?})",
            data.repo_name, data.service_name
        );
        self.data.lock().unwrap().push(data.clone());
        // The mock always stores what it is given — it has no freshness check
        // to short-circuit on, and no meter behind it to report a spend from.
        Ok(UploadOutcome::default())
    }

    /// Writes the object, then stamps the blob with what it wrote, so the
    /// blob names only an object this store holds (carrick#3101).
    async fn upload_repo_data_with_reach(
        &self,
        data: &CloudRepoData,
        type_reach: Option<&TypeReachObject>,
        final_in_run: bool,
    ) -> Result<UploadOutcome, StorageError> {
        let Some(object) = type_reach else {
            return self.upload_repo_data(data, final_in_run).await;
        };
        self.type_reach
            .lock()
            .unwrap()
            .insert(reach_key(data), object.gz().to_vec());
        let mut stamped = data.clone();
        stamped.type_reach = Some(object.reference().clone());
        self.upload_repo_data(&stamped, final_in_run).await
    }

    async fn type_reach_records(
        &self,
        stored: &[CloudRepoData],
    ) -> HashMap<String, TypeReachRecord> {
        let objects = self.type_reach.lock().unwrap();
        stored
            .iter()
            .filter_map(|data| {
                let reference = data.type_reach.as_ref()?;
                let record = decode_type_reach(objects.get(&reach_key(data))?, reference)?;
                Some((type_reach::service_id(data).to_string(), record))
            })
            .collect()
    }

    /// What the in-memory store holds, answered from the store itself: an
    /// upload that was recorded is at the commit it was recorded with, and
    /// only this process ever recorded it, so there is no earlier generation
    /// for `written_after` to exclude (carrick#1067).
    async fn index_landed(
        &self,
        data: &CloudRepoData,
        _written_after: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, StorageError> {
        let stored = self.data.lock().unwrap();
        Ok(stored.iter().any(|entry| {
            entry.repo_name == data.repo_name
                && entry.service_name == data.service_name
                && entry.commit_hash == data.commit_hash
        }))
    }

    // In-memory store keyed per uploaded entry — safe to hold many services
    // per repo, so multi-service fan-out can be exercised under CARRICK_MOCK_ALL.
    fn supports_multi_service(&self) -> bool {
        true
    }

    // Nothing crosses a wire, so no request-size wall applies and payloads are
    // recorded whole — mock runs see the same caches a staged upload keeps.
    fn stages_oversized_payloads(&self) -> bool {
        true
    }

    async fn post_pr_result(
        &self,
        payload: &crate::findings::PrResultPayload,
    ) -> Result<(), StorageError> {
        debug!(
            "MOCK: post_pr_result for {} (PR #{})",
            payload.repo, payload.pr_number
        );
        Ok(())
    }

    async fn upload_type_file(
        &self,
        repo_name: &str,
        file_name: &str,
        content: &str,
    ) -> Result<(), StorageError> {
        debug!(
            "MOCK: Uploading type file for repo: {}, file: {}",
            repo_name, file_name
        );
        let key = format!("{}:{}", repo_name, file_name);
        let mut type_files = self.type_files.lock().unwrap();
        type_files.insert(key, content.to_string());
        Ok(())
    }

    async fn download_all_repo_data(
        &self,
    ) -> Result<(Vec<CloudRepoData>, HashMap<String, String>), StorageError> {
        debug!("MOCK: Downloading all repo data");
        // Drop the lock as soon as we have a clone — concurrent
        // upload_repo_data calls would otherwise block on us while we
        // build the synthetic mock repos and S3-URL map below.
        let mut result = self.data.lock().unwrap().clone();

        // Add some mock repos to simulate cross-repo scenario
        if result.len() <= 1 {
            // Create mock packages for testing dependency conflicts
            let mock_packages_a = Self::create_mock_packages_repo_a();
            let mock_packages_b = Self::create_mock_packages_repo_b();

            // Create mock repos for testing
            let mock_repos = vec![
                CloudRepoData {
                    repo_name: "repo-a".to_string(),
                    service_name: None,
                    endpoints: vec![],
                    calls: vec![],
                    mounts: vec![],
                    apps: HashMap::new(),
                    imported_handlers: vec![],
                    function_definitions: HashMap::new(),
                    config_json: None,
                    package_json: None,
                    packages: Some(mock_packages_a),
                    last_updated: Utc::now(),
                    commit_hash: "abc123".to_string(),
                    dirty: None,
                    mount_graph: None,
                    bundled_types: None,
                    type_manifest: None,
                    file_results: None,
                    cached_detection: None,
                    cached_guidance: None,
                    cached_extraction_config: None,
                    package_json_hash: None,
                    cache_version: None,
                    type_extraction_status: None,
                    types_degraded: None,
                    compat_verdicts: None,
                    capture_stub: None,
                    external_call_candidates: None,
                    sdk_surface: None,
                    sdk_edges: None,
                    sdk_unresolved: None,
                    scanner_version: None,
                    scanner_build: None,
                    boundary: None,
                    dispatch_tables: None,
                    possible_calls: None,
                    possible_calls_omitted: None,
                    call_through: None,
                    route_boundaries: None,
                    memory_peaks: None,
                    type_reach: None,
                    pair_outcomes: None,
                },
                CloudRepoData {
                    repo_name: "repo-b".to_string(),
                    service_name: None,
                    endpoints: vec![],
                    calls: vec![],
                    mounts: vec![],
                    apps: HashMap::new(),
                    imported_handlers: vec![],
                    function_definitions: HashMap::new(),
                    config_json: None,
                    package_json: None,
                    packages: Some(mock_packages_b),
                    last_updated: Utc::now(),
                    commit_hash: "def456".to_string(),
                    dirty: None,
                    mount_graph: None,
                    bundled_types: None,
                    type_manifest: None,
                    file_results: None,
                    cached_detection: None,
                    cached_guidance: None,
                    cached_extraction_config: None,
                    package_json_hash: None,
                    cache_version: None,
                    type_extraction_status: None,
                    types_degraded: None,
                    compat_verdicts: None,
                    capture_stub: None,
                    external_call_candidates: None,
                    sdk_surface: None,
                    sdk_edges: None,
                    sdk_unresolved: None,
                    scanner_version: None,
                    scanner_build: None,
                    boundary: None,
                    dispatch_tables: None,
                    possible_calls: None,
                    possible_calls_omitted: None,
                    call_through: None,
                    route_boundaries: None,
                    memory_peaks: None,
                    type_reach: None,
                    pair_outcomes: None,
                },
            ];
            result.extend(mock_repos);
        }

        // Create mock S3 URLs
        let mut mock_s3_urls = HashMap::new();
        for repo_data in &result {
            mock_s3_urls.insert(
                repo_data.repo_name.clone(),
                format!("https://mock-s3.com/{}", repo_data.repo_name),
            );
        }

        debug!("MOCK: Found {} repos", result.len());
        Ok((result, mock_s3_urls))
    }

    async fn health_check(&self) -> Result<(), StorageError> {
        debug!("MOCK: Health check passed");
        Ok(())
    }

    async fn upload_logs(&self, repo: &str, log_content: &str) -> Result<(), StorageError> {
        debug!("MOCK: Recording a run log for {}", repo);
        self.logs.lock().unwrap().push(log_content.to_string());
        Ok(())
    }

    async fn report_scan_failed(&self, stage: &str, reason: &str) {
        debug!("MOCK: Recording scan failure in {}", stage);
        self.scan_failures
            .lock()
            .unwrap()
            .push((stage.to_string(), reason.to_string()));
    }

    async fn report_preflight_failed(&self, repo: Option<&str>, stage: &str, reason: &str) {
        debug!("MOCK: Recording pre-scan failure in {}", stage);
        self.preflight_failures.lock().unwrap().push((
            repo.map(str::to_string),
            stage.to_string(),
            reason.to_string(),
        ));
    }
}

impl MockStorage {
    fn create_mock_packages_repo_a() -> Packages {
        let mut dependencies = BTreeMap::new();
        dependencies.insert("express".to_string(), "5.0.0".to_string()); // Major version difference (critical)
        dependencies.insert("react".to_string(), "18.3.0".to_string()); // Minor version difference (warning)
        dependencies.insert("lodash".to_string(), "4.17.22".to_string()); // Patch version difference (info)

        let package_json = PackageJson {
            name: Some("repo-a".to_string()),
            version: Some("1.0.0".to_string()),
            dependencies,
            dev_dependencies: BTreeMap::new(),
            peer_dependencies: BTreeMap::new(),
            optional_dependencies: BTreeMap::new(),
            resolutions: BTreeMap::new(),
        };

        Packages::new(vec![PathBuf::from("repo-a/package.json")]).unwrap_or_else(|_| {
            // Create manually if file doesn't exist
            let mut packages = Packages::default();
            packages.package_jsons.push(package_json);
            packages
                .source_paths
                .push(PathBuf::from("repo-a/package.json"));
            packages.resolve_dependencies();
            packages
        })
    }

    fn create_mock_packages_repo_b() -> Packages {
        let mut dependencies = BTreeMap::new();
        dependencies.insert("express".to_string(), "4.18.0".to_string()); // Major version difference (critical)
        dependencies.insert("react".to_string(), "18.2.0".to_string()); // Minor version difference (warning)
        dependencies.insert("lodash".to_string(), "4.17.21".to_string()); // Patch version difference (info)
        dependencies.insert("axios".to_string(), "1.3.0".to_string()); // Only in repo-b

        let package_json = PackageJson {
            name: Some("repo-b".to_string()),
            version: Some("1.0.0".to_string()),
            dependencies,
            dev_dependencies: BTreeMap::new(),
            peer_dependencies: BTreeMap::new(),
            optional_dependencies: BTreeMap::new(),
            resolutions: BTreeMap::new(),
        };

        Packages::new(vec![PathBuf::from("repo-b/package.json")]).unwrap_or_else(|_| {
            // Create manually if file doesn't exist
            let mut packages = Packages::default();
            packages.package_jsons.push(package_json);
            packages
                .source_paths
                .push(PathBuf::from("repo-b/package.json"));
            packages.resolve_dependencies();
            packages
        })
    }
}
