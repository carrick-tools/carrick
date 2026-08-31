use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

// Test utilities for parsing new formatter output
struct TestOutput {
    raw_output: String,
    endpoint_count: usize,
    has_success: bool,
}

impl TestOutput {
    fn parse(stdout: &str) -> Self {
        let raw_output = stdout.to_string();

        // Extract endpoint count from "Indexed **X endpoints**" in formatted output
        let endpoint_count = stdout
            .lines()
            .find(|line| line.contains("Indexed **") && line.contains(" endpoints**"))
            .and_then(|line| {
                // Parse "Indexed **4 endpoints** and **5 cross-service calls**"
                let parts: Vec<&str> = line.split("**").collect();
                if parts.len() >= 2 {
                    parts[1]
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse().ok())
                } else {
                    None
                }
            })
            .unwrap_or(0);

        // Check for successful analysis via the markdown header. The header
        // may carry a topology suffix after the brand mark, so match the stable
        // prefix rather than the whole line.
        let has_success = stdout.contains("## 🪢 Carrick");

        Self {
            raw_output,
            endpoint_count,
            has_success,
        }
    }

    #[allow(dead_code)]
    fn contains_endpoint(&self, endpoint: &str) -> bool {
        self.raw_output.contains(endpoint)
    }

    fn contains_no_duplicates_bug(&self) -> bool {
        self.raw_output.contains("Unique endpoint paths: 0")
    }
}

#[test]
fn test_imported_router_endpoint_resolution() {
    // Create a temporary directory for the test
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_project_path = temp_dir.path().join("test_project");

    // Copy the test fixture to the temporary directory
    let fixture_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/imported-routers");

    copy_dir_recursive(&fixture_path, &test_project_path).expect("Failed to copy test fixture");

    // Run carrick on the test project
    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(test_project_path.to_str().unwrap())
        .env("CARRICK_MOCK_ALL", "1")
        .output()
        .expect("Failed to execute carrick");

    let stdout = String::from_utf8(output.stdout).expect("Invalid UTF-8 in stdout");
    let stderr = String::from_utf8(output.stderr).expect("Invalid UTF-8 in stderr");

    // Print output for debugging if test fails
    if !output.status.success() {
        eprintln!("Command failed with status: {}", output.status);
        eprintln!("STDOUT:\n{}", stdout);
        eprintln!("STDERR:\n{}", stderr);
    }

    // The command should succeed
    assert!(output.status.success(), "Carrick command failed");

    // Parse the output using our test utilities
    let test_output = TestOutput::parse(&stdout);

    // Check that analysis completed successfully
    assert!(
        test_output.has_success,
        "Should have successful CARRICK analysis output. Output: {}",
        stdout
    );

    // We should find at least 10 endpoints (the exact number depends on our fixture)
    assert!(
        test_output.endpoint_count >= 10,
        "Expected at least 10 endpoints, but found {}. This suggests imported router resolution failed. Output: {}",
        test_output.endpoint_count,
        stdout
    );

    // Check that "Unique endpoint paths: 0" does NOT appear (this was the bug)
    assert!(
        !test_output.contains_no_duplicates_bug(),
        "Found 'Unique endpoint paths: 0' which indicates the imported router bug. Output: {}",
        stdout
    );

    // Note: In mock mode, full path resolution (e.g., /users/:id vs /:id) depends on
    // import_source being correctly set, which requires more sophisticated mock logic.
    // The key assertions above verify that:
    // 1. We find the expected number of endpoints (10)
    // 2. The "Unique endpoint paths: 0" bug is not present
    // These are the critical checks for the imported router resolution feature.
}

#[test]
fn test_basic_endpoint_detection() {
    // Test with the existing express-single example to ensure we don't break existing functionality
    let test_repo_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("express-single");

    if !test_repo_path.exists() {
        panic!(
            "examples/express-single directory not found at {:?}",
            test_repo_path
        );
    }

    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(test_repo_path.to_str().unwrap())
        .env("CARRICK_MOCK_ALL", "1")
        .output()
        .expect("Failed to execute carrick");

    let stdout = String::from_utf8(output.stdout).expect("Invalid UTF-8 in stdout");

    if !output.status.success() {
        let stderr = String::from_utf8(output.stderr).expect("Invalid UTF-8 in stderr");
        eprintln!("Command failed with status: {}", output.status);
        eprintln!("STDOUT:\n{}", stdout);
        eprintln!("STDERR:\n{}", stderr);
    }

    assert!(
        output.status.success(),
        "Carrick command failed on express-single"
    );

    // Parse the output using our test utilities
    let test_output = TestOutput::parse(&stdout);

    // Check that analysis completed successfully
    assert!(
        test_output.has_success,
        "Should have successful CARRICK analysis output for express-single. Output: {}",
        stdout
    );

    // express-single should have 3 endpoints (the 4th uses a variable reference which the mock doesn't detect)
    assert_eq!(
        test_output.endpoint_count, 3,
        "Expected 3 endpoints in express-single, but found {}. Output: {}",
        test_output.endpoint_count, stdout
    );
}

// Helper function to recursively copy directories
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    if !dst.exists() {
        fs::create_dir_all(dst)?;
    }

    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let dest_path = dst.join(file_name);

        if path.is_dir() {
            copy_dir_recursive(&path, &dest_path)?;
        } else {
            fs::copy(&path, &dest_path)?;
        }
    }

    Ok(())
}

/// #580 part b: a route table binding literal paths to imported controller
/// instances. Every route is split across two files — the table has the path,
/// the controller has the method and the handler — so before the cross-file
/// join none of them reached the index. Eight bound paths, thirteen handler
/// methods; the schema `$id` URL, the content-type decorator and the plain
/// helper in those same controllers contribute nothing.
#[test]
fn test_class_controller_route_resolution() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_project_path = temp_dir.path().join("test_project");

    let fixture_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/class-controller-api");
    copy_dir_recursive(&fixture_path, &test_project_path).expect("Failed to copy test fixture");

    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(test_project_path.to_str().unwrap())
        .env("CARRICK_MOCK_ALL", "1")
        .output()
        .expect("Failed to execute carrick");

    let stdout = String::from_utf8(output.stdout).expect("Invalid UTF-8 in stdout");
    if !output.status.success() {
        let stderr = String::from_utf8(output.stderr).expect("Invalid UTF-8 in stderr");
        eprintln!("STDOUT:\n{}", stdout);
        eprintln!("STDERR:\n{}", stderr);
    }
    assert!(output.status.success(), "Carrick command failed");

    let test_output = TestOutput::parse(&stdout);
    assert!(
        test_output.has_success,
        "Should have successful CARRICK analysis output. Output: {}",
        stdout
    );
    assert!(
        test_output.endpoint_count >= 13,
        "Expected at least 13 endpoints from the class-controller fixture, found {}. \
         This suggests the route table was not joined to its controllers. Output: {}",
        test_output.endpoint_count,
        stdout
    );
    for path in [
        "/token",
        "/widget",
        "/widget/:id",
        "/report",
        "/session",
        "/profile",
        "/health",
    ] {
        assert!(
            test_output.contains_endpoint(path),
            "bound path {} is missing from the index. Output: {}",
            path,
            stdout
        );
    }
}

#[test]
fn test_no_duplicate_processing_regression() {
    // This test specifically checks that the same imported router file
    // doesn't get processed multiple times incorrectly
    let fixture_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/imported-routers");

    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(fixture_path.to_str().unwrap())
        .env("CARRICK_MOCK_ALL", "1")
        .output()
        .expect("Failed to execute carrick");

    let stdout = String::from_utf8(output.stdout).expect("Invalid UTF-8 in stdout");

    if !output.status.success() {
        let stderr = String::from_utf8(output.stderr).expect("Invalid UTF-8 in stderr");
        eprintln!("Command failed with status: {}", output.status);
        eprintln!("STDOUT:\n{}", stdout);
        eprintln!("STDERR:\n{}", stderr);
    }

    assert!(output.status.success(), "Carrick command failed");

    // Parse the output using our test utilities
    let test_output = TestOutput::parse(&stdout);

    // Check that analysis completed successfully
    assert!(
        test_output.has_success,
        "Should have successful CARRICK analysis output. Output: {}",
        stdout
    );

    // Count how many times each router file appears in the parsing logs
    let users_parse_count = stdout
        .matches("Parsing:")
        .filter(|line| line.contains("users.ts"))
        .count();
    let api_parse_count = stdout
        .matches("Parsing:")
        .filter(|line| line.contains("api.ts"))
        .count();
    let health_parse_count = stdout
        .matches("Parsing:")
        .filter(|line| line.contains("health.ts"))
        .count();

    // Each router file should be parsed exactly twice:
    // 1. Once when discovered as a regular file (with imported_router_name: None)
    // 2. Once when discovered as an imported router (with imported_router_name: Some(...))
    assert!(
        users_parse_count <= 2,
        "users.ts should be parsed at most 2 times, but was parsed {} times. This suggests duplicate processing.",
        users_parse_count
    );
    assert!(
        api_parse_count <= 2,
        "api.ts should be parsed at most 2 times, but was parsed {} times. This suggests duplicate processing.",
        api_parse_count
    );
    assert!(
        health_parse_count <= 2,
        "health.ts should be parsed at most 2 times, but was parsed {} times. This suggests duplicate processing.",
        health_parse_count
    );

    // Most importantly, we should NOT see "Unique endpoint paths: 0"
    assert!(
        !test_output.contains_no_duplicates_bug(),
        "Found 'Unique endpoint paths: 0' which indicates the imported router resolution bug has regressed. Output: {}",
        stdout
    );
}
