use carrick::cloud_storage::{CloudRepoData, CloudStorage, MockStorage};
use carrick::packages::{PackageJson, Packages};
use chrono::Utc;
use std::collections::HashMap;
use std::path::PathBuf;

/// Helper function to create test CloudRepoData
fn create_test_repo_data(repo_name: &str, commit_hash: &str) -> CloudRepoData {
    let mut deps = HashMap::new();
    deps.insert("express".to_string(), "4.18.0".to_string());

    let package_json = PackageJson {
        name: Some(repo_name.to_string()),
        version: Some("1.0.0".to_string()),
        dependencies: deps,
        dev_dependencies: HashMap::new(),
        peer_dependencies: HashMap::new(),
        optional_dependencies: HashMap::new(),
        resolutions: HashMap::new(),
    };

    let mut packages = Packages::default();
    packages.package_jsons.push(package_json);
    packages.source_paths.push(PathBuf::from("package.json"));
    packages.resolve_dependencies();

    CloudRepoData {
        repo_name: repo_name.to_string(),
        service_name: None,
        endpoints: vec![],
        calls: vec![],
        mounts: vec![],
        apps: HashMap::new(),
        imported_handlers: vec![],
        function_definitions: HashMap::new(),
        config_json: None,
        package_json: Some(r#"{"name":"test","version":"1.0.0"}"#.to_string()),
        packages: Some(packages),
        last_updated: Utc::now(),
        commit_hash: commit_hash.to_string(),
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
    }
}

#[tokio::test]
async fn test_upload_and_download_single_repo() {
    // Given: a MockStorage instance
    let storage = MockStorage::new();

    // When: we upload repo data
    let repo_data = create_test_repo_data("express-single", "abc123");
    storage
        .upload_repo_data(&repo_data)
        .await
        .expect("Upload should succeed");

    // Then: we can download it back
    let (downloaded, _s3_urls) = storage
        .download_all_repo_data()
        .await
        .expect("Download should succeed");

    // Verify we got the repo back (plus any mock repos that might be added)
    assert!(
        downloaded.iter().any(|r| r.repo_name == "express-single"),
        "Should find uploaded repo in downloaded data"
    );

    let found_repo = downloaded
        .iter()
        .find(|r| r.repo_name == "express-single")
        .expect("Should find express-single");

    assert_eq!(found_repo.commit_hash, "abc123");
    assert!(found_repo.packages.is_some());
}

#[tokio::test]
async fn test_upload_multiple_repos() {
    // Given: a MockStorage instance
    let storage = MockStorage::new();

    // When: we upload multiple repos
    let repo1 = create_test_repo_data("repo-1", "hash1");
    let repo2 = create_test_repo_data("repo-2", "hash2");
    let repo3 = create_test_repo_data("repo-3", "hash3");

    storage
        .upload_repo_data(&repo1)
        .await
        .expect("Upload repo1 should succeed");
    storage
        .upload_repo_data(&repo2)
        .await
        .expect("Upload repo2 should succeed");
    storage
        .upload_repo_data(&repo3)
        .await
        .expect("Upload repo3 should succeed");

    // Then: we can download all of them
    let (downloaded, _s3_urls) = storage
        .download_all_repo_data()
        .await
        .expect("Download should succeed");

    // Should have at least our 3 repos (mock might add more)
    let our_repos: Vec<_> = downloaded
        .iter()
        .filter(|r| r.repo_name.starts_with("repo-"))
        .collect();

    assert!(
        our_repos.len() >= 3,
        "Should have at least 3 uploaded repos, found {}",
        our_repos.len()
    );

    // Verify each repo
    assert!(our_repos.iter().any(|r| r.repo_name == "repo-1"));
    assert!(our_repos.iter().any(|r| r.repo_name == "repo-2"));
    assert!(our_repos.iter().any(|r| r.repo_name == "repo-3"));
}

#[tokio::test]
async fn test_health_check_succeeds() {
    // Given: a MockStorage instance
    let storage = MockStorage::new();

    // When: we perform a health check
    let result = storage.health_check().await;

    // Then: it should succeed
    assert!(result.is_ok(), "Health check should succeed");
}

#[tokio::test]
async fn test_upload_type_file() {
    // Given: a MockStorage instance
    let storage = MockStorage::new();

    // When: we upload a type file
    let type_content = "export interface User { id: string; name: string; }";
    let result = storage
        .upload_type_file("express-single", "types.ts", type_content)
        .await;

    // Then: it should succeed
    assert!(result.is_ok(), "Type file upload should succeed");
}

#[tokio::test]
async fn test_concurrent_uploads() {
    // Given: a MockStorage instance
    let storage = std::sync::Arc::new(MockStorage::new());

    // When: we upload repos concurrently
    let mut handles = vec![];

    for i in 0..5 {
        let storage_clone = storage.clone();
        let handle = tokio::spawn(async move {
            let repo =
                create_test_repo_data(&format!("concurrent-repo-{}", i), &format!("hash{}", i));
            storage_clone
                .upload_repo_data(&repo)
                .await
                .expect("Concurrent upload should succeed");
        });
        handles.push(handle);
    }

    // Wait for all uploads to complete
    for handle in handles {
        handle.await.expect("Task should complete");
    }

    // Then: all repos should be present
    let (downloaded, _) = storage
        .download_all_repo_data()
        .await
        .expect("Download should succeed");

    let concurrent_repos: Vec<_> = downloaded
        .iter()
        .filter(|r| r.repo_name.starts_with("concurrent-repo-"))
        .collect();

    assert_eq!(
        concurrent_repos.len(),
        5,
        "Should have all 5 concurrent repos"
    );
}

#[tokio::test]
async fn test_empty_storage_returns_mock_data() {
    // Given: a fresh MockStorage instance with nothing uploaded
    let storage = MockStorage::new();

    // When: we download cross-repo data
    let (downloaded, _) = storage
        .download_all_repo_data()
        .await
        .expect("Download should succeed for empty storage");

    // Then: MockStorage seeds two synthetic repos when fewer than 2
    // entries exist, so cross-repo analysis has data to work with.
    assert_eq!(downloaded.len(), 2);
}

#[tokio::test]
async fn test_update_existing_repo() {
    // Given: a MockStorage with an uploaded repo
    let storage = MockStorage::new();

    let repo_v1 = create_test_repo_data("versioned-repo", "hash1");
    storage
        .upload_repo_data(&repo_v1)
        .await
        .expect("Upload v1 should succeed");

    // When: we upload the same repo with a different commit hash
    let repo_v2 = create_test_repo_data("versioned-repo", "hash2");
    storage
        .upload_repo_data(&repo_v2)
        .await
        .expect("Upload v2 should succeed");

    // Then: both versions exist (MockStorage appends, doesn't replace)
    let (downloaded, _) = storage
        .download_all_repo_data()
        .await
        .expect("Download should succeed");

    let versions: Vec<_> = downloaded
        .iter()
        .filter(|r| r.repo_name == "versioned-repo")
        .collect();

    // MockStorage currently appends, so we'll have 2 versions
    assert!(
        !versions.is_empty(),
        "Should have at least one version of the repo"
    );
}

#[tokio::test]
async fn test_packages_preserved_in_upload_download_cycle() {
    // Given: repo data with packages (using create_test_repo_data which properly constructs packages)
    let storage = MockStorage::new();

    let repo_data = create_test_repo_data("package-test-repo", "xyz789");

    // When: we upload and download
    storage
        .upload_repo_data(&repo_data)
        .await
        .expect("Upload should succeed");

    let (downloaded, _) = storage
        .download_all_repo_data()
        .await
        .expect("Download should succeed");

    // Then: packages should be preserved
    let found = downloaded
        .iter()
        .find(|r| r.repo_name == "package-test-repo")
        .expect("Should find uploaded repo");

    assert!(found.packages.is_some(), "Packages should be preserved");
    let packages = found.packages.as_ref().unwrap();

    // Check that package structure is preserved
    assert!(
        !packages.package_jsons.is_empty(),
        "Package JSONs should be preserved"
    );
    assert!(
        !packages.source_paths.is_empty(),
        "Source paths should be preserved"
    );

    // Verify dependencies are in merged_dependencies after resolve
    if !packages.merged_dependencies.is_empty() {
        assert!(
            packages.merged_dependencies.contains_key("express"),
            "Should preserve express dependency"
        );
    }
}

/// The upload payload is exactly `serde_json::to_string(&CloudRepoData)`
/// (`cloud_storage/aws_storage.rs`), so serialising one built from a real
/// fixture parse is the end-to-end check that `tokens` reaches the cloud.
/// `eval_output::EvalProjection` carries no function rows, so this is the only
/// path the field travels.
#[test]
fn uploaded_json_carries_tokens_on_a_function_row() {
    use carrick::parser::parse_file;
    use carrick::visitor::FunctionDefinitionExtractor;
    use swc_common::SourceMap;
    use swc_common::errors::{ColorConfig, Handler};
    use swc_common::sync::Lrc;
    use swc_ecma_visit::VisitWith;

    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/class-controller-api/src/controllers/report.ts");
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
    let module = parse_file(&fixture, &cm, &handler).expect("fixture parses");
    let mut extractor = FunctionDefinitionExtractor::new(fixture.clone(), cm.clone());
    module.visit_with(&mut extractor);
    extractor.finalize_exports();

    let mut data = create_test_repo_data("acme/reports", "abc123");
    data.function_definitions = extractor.function_definitions;

    let json: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&data).expect("serialize")).expect("parse");
    let row = &json["function_definitions"]["ReportController.exportCsv"];
    let tokens: Vec<&str> = row["tokens"]
        .as_array()
        .expect("tokens present on the uploaded row")
        .iter()
        .map(|t| t.as_str().expect("token is a string"))
        .collect();
    assert_eq!(tokens[0], "ctx", "parameter name leads the uploaded tokens");
    for expected in ["response", "buildRows", "text/csv"] {
        assert!(
            tokens.contains(&expected),
            "{expected} missing from uploaded tokens: {tokens:?}"
        );
    }
}
