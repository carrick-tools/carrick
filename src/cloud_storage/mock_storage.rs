use crate::cloud_storage::{CloudRepoData, CloudStorage, StorageError, UploadOutcome};
use crate::packages::{PackageJson, Packages};
use async_trait::async_trait;
use chrono::Utc;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tracing::debug;

pub struct MockStorage {
    data: Mutex<Vec<CloudRepoData>>,
    type_files: Mutex<HashMap<String, String>>,
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
        }
    }
}

#[async_trait]
impl CloudStorage for MockStorage {
    async fn upload_repo_data(&self, data: &CloudRepoData) -> Result<UploadOutcome, StorageError> {
        debug!(
            "MOCK: Uploading repo data for repo: {} (service: {:?})",
            data.repo_name, data.service_name
        );
        self.data.lock().unwrap().push(data.clone());
        // The mock always stores what it is given — it has no freshness check
        // to short-circuit on.
        Ok(UploadOutcome {
            already_current: false,
        })
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
                    boundary: None,
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
                },
                CloudRepoData {
                    boundary: None,
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

    async fn upload_logs(&self, repo: &str, _log_content: &str) -> Result<(), StorageError> {
        debug!("MOCK: Skipping log upload for {}", repo);
        Ok(())
    }
}

impl MockStorage {
    fn create_mock_packages_repo_a() -> Packages {
        let mut dependencies = HashMap::new();
        dependencies.insert("express".to_string(), "5.0.0".to_string()); // Major version difference (critical)
        dependencies.insert("react".to_string(), "18.3.0".to_string()); // Minor version difference (warning)
        dependencies.insert("lodash".to_string(), "4.17.22".to_string()); // Patch version difference (info)

        let package_json = PackageJson {
            name: Some("repo-a".to_string()),
            version: Some("1.0.0".to_string()),
            dependencies,
            dev_dependencies: HashMap::new(),
            peer_dependencies: HashMap::new(),
            optional_dependencies: HashMap::new(),
            resolutions: HashMap::new(),
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
        let mut dependencies = HashMap::new();
        dependencies.insert("express".to_string(), "4.18.0".to_string()); // Major version difference (critical)
        dependencies.insert("react".to_string(), "18.2.0".to_string()); // Minor version difference (warning)
        dependencies.insert("lodash".to_string(), "4.17.21".to_string()); // Patch version difference (info)
        dependencies.insert("axios".to_string(), "1.3.0".to_string()); // Only in repo-b

        let package_json = PackageJson {
            name: Some("repo-b".to_string()),
            version: Some("1.0.0".to_string()),
            dependencies,
            dev_dependencies: HashMap::new(),
            peer_dependencies: HashMap::new(),
            optional_dependencies: HashMap::new(),
            resolutions: HashMap::new(),
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
