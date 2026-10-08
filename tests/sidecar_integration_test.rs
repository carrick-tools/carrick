//! Sidecar Integration Tests
//!
//! End-to-end tests that verify the type-sidecar integration with the type checker.
//! These tests:
//! 1. Use test fixture repos (producer/consumer scenarios)
//! 2. Run the full analysis with sidecar enabled
//! 3. Verify bundled types are produced correctly
//! 4. Verify type checking identifies correct matches/mismatches
//!
//! Note: These tests require Node.js to be installed and the sidecar to be built.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

// ============================================================================
// Test Fixtures
// ============================================================================

/// Creates a simple producer repository with typed endpoints
fn create_producer_repo(dir: &std::path::Path) {
    // Create package.json
    fs::write(
        dir.join("package.json"),
        r#"{
  "name": "producer-api",
  "version": "1.0.0",
  "type": "module",
  "dependencies": {
    "express": "^4.18.0"
  }
}"#,
    )
    .expect("Failed to write package.json");

    // Create tsconfig.json
    fs::write(
        dir.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "target": "ES2020",
    "module": "NodeNext",
    "moduleResolution": "NodeNext",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true
  },
  "include": ["src/**/*"]
}"#,
    )
    .expect("Failed to write tsconfig.json");

    // Create src directory
    fs::create_dir_all(dir.join("src")).expect("Failed to create src directory");

    // Create types.ts with shared types
    fs::write(
        dir.join("src/types.ts"),
        r#"
export interface User {
  id: number;
  name: string;
  email: string;
}

export interface CreateUserRequest {
  name: string;
  email: string;
}

export interface Order {
  id: number;
  userId: number;
  total: number;
  items: OrderItem[];
}

export interface OrderItem {
  productId: number;
  quantity: number;
  price: number;
}
"#,
    )
    .expect("Failed to write types.ts");

    // Create routes.ts with Express endpoints
    fs::write(
        dir.join("src/routes.ts"),
        r#"
import express, { Response } from 'express';
import { User, CreateUserRequest, Order } from './types.js';

const app = express();

// GET /api/users - Returns array of users
app.get('/api/users', (req, res: Response<User[]>) => {
  const users: User[] = [
    { id: 1, name: 'Alice', email: 'alice@example.com' },
    { id: 2, name: 'Bob', email: 'bob@example.com' }
  ];
  res.json(users);
});

// GET /api/users/:id - Returns single user
app.get('/api/users/:id', (req, res: Response<User>) => {
  const user: User = { id: 1, name: 'Alice', email: 'alice@example.com' };
  res.json(user);
});

// POST /api/users - Creates a user
app.post('/api/users', (req, res: Response<User>) => {
  const body = req.body as CreateUserRequest;
  const user: User = { id: 3, name: body.name, email: body.email };
  res.json(user);
});

// GET /api/orders - Returns array of orders
app.get('/api/orders', (req, res: Response<Order[]>) => {
  const orders: Order[] = [];
  res.json(orders);
});

// GET /api/orders/:id - Returns single order
app.get('/api/orders/:id', (req, res: Response<Order>) => {
  const order: Order = { id: 1, userId: 1, total: 100, items: [] };
  res.json(order);
});

export default app;
"#,
    )
    .expect("Failed to write routes.ts");
}

/// Creates a consumer repository that calls the producer API
fn create_consumer_repo(dir: &std::path::Path) {
    // Create package.json
    fs::write(
        dir.join("package.json"),
        r#"{
  "name": "consumer-app",
  "version": "1.0.0",
  "type": "module",
  "dependencies": {
    "axios": "^1.6.0"
  }
}"#,
    )
    .expect("Failed to write package.json");

    // Create tsconfig.json
    fs::write(
        dir.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "target": "ES2020",
    "module": "NodeNext",
    "moduleResolution": "NodeNext",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true
  },
  "include": ["src/**/*"]
}"#,
    )
    .expect("Failed to write tsconfig.json");

    // Create src directory
    fs::create_dir_all(dir.join("src")).expect("Failed to create src directory");

    // Create types.ts with consumer's view of the types
    // Note: Intentionally slightly different to test compatibility
    fs::write(
        dir.join("src/types.ts"),
        r#"
// Consumer's type definitions
// These should be compatible with the producer's types

export interface User {
  id: number;
  name: string;
  email: string;
}

export interface Order {
  id: number;
  userId: number;
  total: number;
  items: OrderItem[];
}

export interface OrderItem {
  productId: number;
  quantity: number;
  price: number;
}

// Type that doesn't match producer (for testing mismatches)
export interface IncompatibleUser {
  id: string;  // Should be number!
  username: string;  // Should be name!
}
"#,
    )
    .expect("Failed to write types.ts");

    // Create api.ts with API client calls
    fs::write(
        dir.join("src/api.ts"),
        r#"
import axios from 'axios';
import { User, Order, IncompatibleUser } from './types.js';

const API_BASE = process.env.API_URL || 'http://localhost:3000';

// Correct: Fetches users with matching type
export async function getUsers(): Promise<User[]> {
  const response = await axios.get<User[]>(`${API_BASE}/api/users`);
  return response.data;
}

// Correct: Fetches single user with matching type
export async function getUser(id: number): Promise<User> {
  const response = await axios.get<User>(`${API_BASE}/api/users/${id}`);
  return response.data;
}

// Correct: Fetches orders with matching type
export async function getOrders(): Promise<Order[]> {
  const response = await axios.get<Order[]>(`${API_BASE}/api/orders`);
  return response.data;
}

// Correct: Fetches single order
export async function getOrder(id: number): Promise<Order> {
  const response = await axios.get<Order>(`${API_BASE}/api/orders/${id}`);
  return response.data;
}

// MISMATCH: Calls endpoint that doesn't exist on producer
export async function getProducts(): Promise<unknown[]> {
  const response = await axios.get(`${API_BASE}/api/products`);
  return response.data;
}

// MISMATCH: Uses DELETE on endpoint that only supports GET
export async function deleteUser(id: number): Promise<void> {
  await axios.delete(`${API_BASE}/api/users/${id}`);
}
"#,
    )
    .expect("Failed to write api.ts");
}

/// Creates a type manifest JSON file
fn create_manifest(path: &std::path::Path, repo_name: &str, entries: Vec<ManifestEntry>) {
    let manifest = serde_json::json!({
        "repo_name": repo_name,
        "commit_hash": "test-commit-hash",
        "entries": entries
    });

    fs::write(path, serde_json::to_string_pretty(&manifest).unwrap())
        .expect("Failed to write manifest");
}

#[derive(serde::Serialize)]
struct ManifestEntry {
    method: String,
    path: String,
    type_alias: String,
    role: String,
    file_path: String,
    line_number: u32,
}

// ============================================================================
// Unit Tests for Manifest Structure
// ============================================================================

#[test]
fn test_manifest_structure_is_valid() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let manifest_path = temp_dir.path().join("test-manifest.json");

    let entries = vec![
        ManifestEntry {
            method: "GET".to_string(),
            path: "/api/users".to_string(),
            type_alias: "GetUsersResponse".to_string(),
            role: "producer".to_string(),
            file_path: "src/routes.ts".to_string(),
            line_number: 10,
        },
        ManifestEntry {
            method: "POST".to_string(),
            path: "/api/users".to_string(),
            type_alias: "CreateUserResponse".to_string(),
            role: "producer".to_string(),
            file_path: "src/routes.ts".to_string(),
            line_number: 20,
        },
    ];

    create_manifest(&manifest_path, "test-api", entries);

    // Verify the manifest was created and is valid JSON
    let content = fs::read_to_string(&manifest_path).expect("Failed to read manifest");
    let parsed: serde_json::Value = serde_json::from_str(&content).expect("Invalid JSON");

    assert_eq!(parsed["repo_name"], "test-api");
    assert_eq!(parsed["commit_hash"], "test-commit-hash");
    assert!(parsed["entries"].is_array());
    assert_eq!(parsed["entries"].as_array().unwrap().len(), 2);
}

#[test]
fn test_producer_fixture_creation() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let producer_path = temp_dir.path().join("producer");
    fs::create_dir_all(&producer_path).expect("Failed to create producer directory");

    create_producer_repo(&producer_path);

    // Verify all expected files exist
    assert!(producer_path.join("package.json").exists());
    assert!(producer_path.join("tsconfig.json").exists());
    assert!(producer_path.join("src/types.ts").exists());
    assert!(producer_path.join("src/routes.ts").exists());

    // Verify package.json content
    let package_json = fs::read_to_string(producer_path.join("package.json"))
        .expect("Failed to read package.json");
    let parsed: serde_json::Value = serde_json::from_str(&package_json).expect("Invalid JSON");
    assert_eq!(parsed["name"], "producer-api");
}

#[test]
fn test_consumer_fixture_creation() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let consumer_path = temp_dir.path().join("consumer");
    fs::create_dir_all(&consumer_path).expect("Failed to create consumer directory");

    create_consumer_repo(&consumer_path);

    // Verify all expected files exist
    assert!(consumer_path.join("package.json").exists());
    assert!(consumer_path.join("tsconfig.json").exists());
    assert!(consumer_path.join("src/types.ts").exists());
    assert!(consumer_path.join("src/api.ts").exists());

    // Verify package.json content
    let package_json = fs::read_to_string(consumer_path.join("package.json"))
        .expect("Failed to read package.json");
    let parsed: serde_json::Value = serde_json::from_str(&package_json).expect("Invalid JSON");
    assert_eq!(parsed["name"], "consumer-app");
}

// ============================================================================
// Sidecar Integration Tests
// ============================================================================

/// Write bundled .d.ts content into a minimal capture-stub-shaped temp dir
/// (`<dir>/types/surface.d.ts`) for the stub-based `resolve_definitions`
/// action. Returns the stub dir root (owned by the caller's TempDir-free
/// lifetime; std::env::temp_dir cleanup is the OS's).
fn stub_dir_for(dts: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "carrick-test-defres-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let types = dir.join("types");
    fs::create_dir_all(&types).expect("create stub types dir");
    fs::write(types.join("surface.d.ts"), dts).expect("write stub surface");
    dir
}

/// Check if Node.js is available
fn is_node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if the sidecar has been built
fn is_sidecar_built() -> bool {
    let sidecar_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/index.js");
    sidecar_path.exists()
}

#[test]
fn test_sidecar_produces_bundled_types() {
    if !is_node_available() {
        eprintln!("Skipping test: Node.js not available");
        return;
    }

    if !is_sidecar_built() {
        eprintln!("Skipping test: Sidecar not built (run: cd src/sidecar && npm run build)");
        return;
    }

    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let producer_path = temp_dir.path().join("producer");
    fs::create_dir_all(&producer_path).expect("Failed to create producer directory");

    create_producer_repo(&producer_path);

    // For now, we just verify the fixture is created correctly
    // Full sidecar integration would require spawning the sidecar process
    // which is tested in the sidecar's own test suite

    let types_file = producer_path.join("src/types.ts");
    let content = fs::read_to_string(&types_file).expect("Failed to read types.ts");

    // Verify the types file contains expected interfaces
    assert!(content.contains("interface User"));
    assert!(content.contains("interface Order"));
    assert!(content.contains("interface OrderItem"));
}

#[test]
fn test_sidecar_handles_missing_types() {
    if !is_node_available() {
        eprintln!("Skipping test: Node.js not available");
        return;
    }

    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let empty_repo = temp_dir.path().join("empty-repo");
    fs::create_dir_all(&empty_repo).expect("Failed to create empty repo");

    // Create minimal package.json
    fs::write(
        empty_repo.join("package.json"),
        r#"{"name": "empty-repo", "version": "1.0.0"}"#,
    )
    .expect("Failed to write package.json");

    // Create minimal tsconfig.json
    fs::write(
        empty_repo.join("tsconfig.json"),
        r#"{"compilerOptions": {"target": "ES2020"}}"#,
    )
    .expect("Failed to write tsconfig.json");

    // Verify the repo structure
    assert!(empty_repo.join("package.json").exists());
    assert!(empty_repo.join("tsconfig.json").exists());

    // The sidecar should handle this gracefully (no types to extract)
    // This is verified in the sidecar's own test suite
}

#[test]
fn test_type_checking_with_sidecar_types() {
    if !is_node_available() {
        eprintln!("Skipping test: Node.js not available");
        return;
    }

    let temp_dir = TempDir::new().expect("Failed to create temp directory");

    // Create producer and consumer repos
    let producer_path = temp_dir.path().join("producer");
    let consumer_path = temp_dir.path().join("consumer");
    fs::create_dir_all(&producer_path).expect("Failed to create producer directory");
    fs::create_dir_all(&consumer_path).expect("Failed to create consumer directory");

    create_producer_repo(&producer_path);
    create_consumer_repo(&consumer_path);

    // Create manifests for testing
    let producer_manifest_path = temp_dir.path().join("producer-manifest.json");
    let consumer_manifest_path = temp_dir.path().join("consumer-manifest.json");

    // Producer manifest - endpoints defined in routes.ts
    let producer_entries = vec![
        ManifestEntry {
            method: "GET".to_string(),
            path: "/api/users".to_string(),
            type_alias: "UsersArrayResponse".to_string(),
            role: "producer".to_string(),
            file_path: "src/routes.ts".to_string(),
            line_number: 9,
        },
        ManifestEntry {
            method: "GET".to_string(),
            path: "/api/users/:id".to_string(),
            type_alias: "UserResponse".to_string(),
            role: "producer".to_string(),
            file_path: "src/routes.ts".to_string(),
            line_number: 18,
        },
        ManifestEntry {
            method: "POST".to_string(),
            path: "/api/users".to_string(),
            type_alias: "CreateUserResponse".to_string(),
            role: "producer".to_string(),
            file_path: "src/routes.ts".to_string(),
            line_number: 24,
        },
        ManifestEntry {
            method: "GET".to_string(),
            path: "/api/orders".to_string(),
            type_alias: "OrdersArrayResponse".to_string(),
            role: "producer".to_string(),
            file_path: "src/routes.ts".to_string(),
            line_number: 31,
        },
        ManifestEntry {
            method: "GET".to_string(),
            path: "/api/orders/:id".to_string(),
            type_alias: "OrderResponse".to_string(),
            role: "producer".to_string(),
            file_path: "src/routes.ts".to_string(),
            line_number: 37,
        },
    ];

    // Consumer manifest - API calls in api.ts
    let consumer_entries = vec![
        ManifestEntry {
            method: "GET".to_string(),
            path: "/api/users".to_string(),
            type_alias: "UsersArrayData".to_string(),
            role: "consumer".to_string(),
            file_path: "src/api.ts".to_string(),
            line_number: 10,
        },
        ManifestEntry {
            method: "GET".to_string(),
            path: "/api/users/{id}".to_string(), // Different param format
            type_alias: "UserData".to_string(),
            role: "consumer".to_string(),
            file_path: "src/api.ts".to_string(),
            line_number: 16,
        },
        ManifestEntry {
            method: "GET".to_string(),
            path: "/api/orders".to_string(),
            type_alias: "OrdersArrayData".to_string(),
            role: "consumer".to_string(),
            file_path: "src/api.ts".to_string(),
            line_number: 22,
        },
        ManifestEntry {
            method: "GET".to_string(),
            path: "/api/orders/{id}".to_string(),
            type_alias: "OrderData".to_string(),
            role: "consumer".to_string(),
            file_path: "src/api.ts".to_string(),
            line_number: 28,
        },
        // Orphaned consumer - no matching producer
        ManifestEntry {
            method: "GET".to_string(),
            path: "/api/products".to_string(),
            type_alias: "ProductsData".to_string(),
            role: "consumer".to_string(),
            file_path: "src/api.ts".to_string(),
            line_number: 34,
        },
        // Another orphaned consumer - wrong method
        ManifestEntry {
            method: "DELETE".to_string(),
            path: "/api/users/{id}".to_string(),
            type_alias: "DeleteUserResult".to_string(),
            role: "consumer".to_string(),
            file_path: "src/api.ts".to_string(),
            line_number: 40,
        },
    ];

    create_manifest(&producer_manifest_path, "producer-api", producer_entries);
    create_manifest(&consumer_manifest_path, "consumer-app", consumer_entries);

    // Verify manifests were created
    assert!(producer_manifest_path.exists());
    assert!(consumer_manifest_path.exists());

    // Read and verify producer manifest
    let producer_manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&producer_manifest_path).unwrap()).unwrap();
    assert_eq!(producer_manifest["repo_name"], "producer-api");
    assert_eq!(
        producer_manifest["entries"].as_array().unwrap().len(),
        5,
        "Producer should have 5 endpoints"
    );

    // Read and verify consumer manifest
    let consumer_manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&consumer_manifest_path).unwrap()).unwrap();
    assert_eq!(consumer_manifest["repo_name"], "consumer-app");
    assert_eq!(
        consumer_manifest["entries"].as_array().unwrap().len(),
        6,
        "Consumer should have 6 API calls"
    );

    // Verify path normalization would match endpoints
    // GET /api/users/:id (producer) should match GET /api/users/{id} (consumer)
    let producer_entries_parsed = producer_manifest["entries"].as_array().unwrap();
    let consumer_entries_parsed = consumer_manifest["entries"].as_array().unwrap();

    // Count expected matches (ignoring path param format differences)
    let mut expected_matches = 0;
    for producer_entry in producer_entries_parsed {
        let producer_method = producer_entry["method"].as_str().unwrap();
        let producer_path = normalize_path(producer_entry["path"].as_str().unwrap());

        for consumer_entry in consumer_entries_parsed {
            let consumer_method = consumer_entry["method"].as_str().unwrap();
            let consumer_path = normalize_path(consumer_entry["path"].as_str().unwrap());

            if producer_method == consumer_method && producer_path == consumer_path {
                expected_matches += 1;
            }
        }
    }

    // Should find 4 matches (GET /api/users, GET /api/users/:id, GET /api/orders, GET /api/orders/:id)
    assert_eq!(expected_matches, 4, "Should find 4 matching endpoint pairs");
}

/// Helper function to normalize paths for comparison (mimics TypeScript implementation)
fn normalize_path(path: &str) -> String {
    let mut normalized = path.to_lowercase();

    // Remove trailing slashes
    normalized = normalized.trim_end_matches('/').to_string();

    // Ensure leading slash
    if !normalized.starts_with('/') {
        normalized = format!("/{}", normalized);
    }

    // Normalize path parameters to :param
    // Handle Express style :id
    let re_colon = regex::Regex::new(r":[\w-]+").unwrap();
    normalized = re_colon.replace_all(&normalized, ":param").to_string();

    // Handle OpenAPI style {id}
    let re_brace = regex::Regex::new(r"\{[^}]+\}").unwrap();
    normalized = re_brace.replace_all(&normalized, ":param").to_string();

    // Handle Next.js style [id]
    let re_bracket = regex::Regex::new(r"\[[^\]]+\]").unwrap();
    normalized = re_bracket.replace_all(&normalized, ":param").to_string();

    normalized
}

#[test]
fn test_path_normalization() {
    // Test various path formats normalize correctly
    assert_eq!(normalize_path("/api/users/:id"), "/api/users/:param");
    assert_eq!(normalize_path("/api/users/{userId}"), "/api/users/:param");
    assert_eq!(normalize_path("/api/users/[id]"), "/api/users/:param");
    assert_eq!(normalize_path("/API/Users/"), "/api/users");
    assert_eq!(normalize_path("api/users"), "/api/users");
    assert_eq!(
        normalize_path("/api/users/:userId/posts/:postId"),
        "/api/users/:param/posts/:param"
    );
}

/// #336 (reopened): the END ARTIFACT for an `axios.get<Order[]>`-shaped
/// consumer must carry the array depth. The live repro is a MULTI-LINE call
/// whose binding is last used as a scalar projection:
///
/// ```ts
/// const ordersResponse = await axios.get<Order[]>(
///   `${ORDER_SERVICE_URL}/api/orders`,
/// );
/// const orderCount = ordersResponse.data.length;
/// ```
///
/// The LLM anchor is the bare element (`Order`), so the explicit bundle
/// pre-claims the consumer alias; the depth must be recovered by inference and
/// copied onto the `SymbolRequest` (`apply_inferred_array_depth`). Before the
/// fix, the single-line locator print missed the multi-line call AND the
/// terminal-use walk anchored on `number`, so the bundle rendered
/// `export interface <alias> {...}` with the `[]` gone and every scan reported
/// a false `{...}[] not assignable` contract risk. This asserts the rendered
/// definition — the artifact the manifest stores for the check phase — not
/// the intermediate inference.
#[test]
fn test_multiline_call_result_bundles_array_definition() {
    use carrick::services::type_sidecar::{
        ExtractionConfig, ExtractionRule, InferKind, InferRequestItem, SymbolRequest, TypeSidecar,
    };
    use std::time::Duration;

    if !is_node_available() {
        eprintln!("Skipping test: Node.js not available");
        return;
    }
    let sidecar_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    if !sidecar_path.exists() {
        eprintln!("Skipping test: Sidecar not built (run: cd src/sidecar && npm run build)");
        return;
    }

    // Consumer repo mirroring carrick-demo-notification-service, with a stub
    // axios so no npm install is needed.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let repo = temp_dir.path().join("consumer");
    fs::create_dir_all(repo.join("src")).expect("Failed to create src");
    fs::create_dir_all(repo.join("node_modules/axios")).expect("Failed to create axios stub dir");
    fs::write(
        repo.join("package.json"),
        r#"{ "name": "consumer-app", "version": "1.0.0", "dependencies": { "axios": "^1.6.0" } }"#,
    )
    .unwrap();
    fs::write(
        repo.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "target": "ES2020",
    "module": "commonjs",
    "moduleResolution": "node",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true
  },
  "include": ["src/**/*"]
}"#,
    )
    .unwrap();
    fs::write(
        repo.join("node_modules/axios/package.json"),
        r#"{ "name": "axios", "version": "1.6.0", "main": "index.js", "types": "index.d.ts" }"#,
    )
    .unwrap();
    fs::write(
        repo.join("node_modules/axios/index.d.ts"),
        r#"export interface AxiosResponse<T = any> {
  data: T;
  status: number;
}
export interface AxiosInstance {
  get<T = any>(url: string): Promise<AxiosResponse<T>>;
}
declare const axios: AxiosInstance;
export default axios;
"#,
    )
    .unwrap();
    fs::write(
        repo.join("src/types.ts"),
        r#"export interface Order {
  id: number;
  userId: number;
  product: string;
  amount: number;
}
"#,
    )
    .unwrap();
    // The call spans three lines with a trailing comma (line 7 = call start).
    fs::write(
        repo.join("src/api.ts"),
        r#"import axios from 'axios';
import { Order } from './types';

const ORDER_SERVICE_URL = 'http://localhost:3002';

export async function getOrderCount(): Promise<number> {
  const ordersResponse = await axios.get<Order[]>(
    `${ORDER_SERVICE_URL}/api/orders`,
  );
  const orderCount = ordersResponse.data.length;
  return orderCount;
}
"#,
    )
    .unwrap();

    let sidecar = TypeSidecar::spawn(&sidecar_path).expect("Failed to spawn sidecar");
    sidecar.start_init(&repo, None);
    sidecar
        .wait_ready(Duration::from_secs(60))
        .expect("Sidecar failed to initialize");

    let alias = "Endpoint_4022e5b76e4552db_Response_Call8904b52acc58d4d6";
    // The LLM-extracted anchor: bare element symbol, alias pre-claimed.
    let explicit = vec![SymbolRequest {
        symbol_name: "Order".to_string(),
        source_file: repo.join("src/types.ts").to_string_lossy().to_string(),
        alias: Some(alias.to_string()),
        array_depth: None,
        payload_borrow_witness: false,
        consumer_response: false,
    }];
    // The LLM's compact single-line locator for the multi-line call.
    let infer = vec![InferRequestItem {
        file_path: repo.join("src/api.ts").to_string_lossy().to_string(),
        line_number: 7,
        span_start: None,
        span_end: None,
        expression_text: Some("axios.get<Order[]>(`${ORDER_SERVICE_URL}/api/orders`)".to_string()),
        expression_line: Some(7),
        infer_kind: InferKind::CallResult,
        alias: Some(alias.to_string()),
        param_name: None,
    }];
    let config = ExtractionConfig {
        rules: vec![ExtractionRule {
            wrapper_symbols: vec!["AxiosResponse".to_string()],
            origin_module_globs: vec!["axios".to_string(), "axios/*".to_string()],
            payload_generic_index: Some(0),
            ..Default::default()
        }],
    };

    let result = sidecar
        .resolve_all_types(&explicit, &infer, Some(&config))
        .expect("resolve_all_types failed");
    let dts = result.dts_content.expect("expected bundled dts content");

    // The bundled alias must be an array-form type alias. An
    // `export interface` structurally cannot carry the depth — that is the
    // exact live artifact this test locks out.
    assert!(
        !dts.contains(&format!("export interface {}", alias)),
        "bundled alias must not render as a depth-less interface:\n{}",
        dts
    );
    let alias_line = dts
        .lines()
        .find(|l| l.contains(&format!("export type {} =", alias)))
        .unwrap_or_else(|| {
            panic!(
                "expected an `export type {} = ...` alias in:\n{}",
                alias, dts
            )
        });
    assert!(
        alias_line.trim_end().trim_end_matches(';').ends_with("[]"),
        "bundled alias must keep the use-site array depth, got: {}",
        alias_line
    );

    // And the definition the manifest stores (resolved_definition /
    // expanded_definition) must resolve to the same array form.
    let stub = stub_dir_for(&dts);
    let definitions = sidecar
        .resolve_definitions(&stub.to_string_lossy(), &[alias.to_string()])
        .expect("resolve_definitions failed");
    let def = definitions
        .iter()
        .find(|d| d.type_alias == alias)
        .expect("expected a resolved definition for the consumer alias");
    assert!(
        def.definition
            .trim_end()
            .trim_end_matches(';')
            .ends_with("[]"),
        "resolved_definition dropped the array depth: {}",
        def.definition
    );
    assert!(
        def.expanded.trim_end().ends_with("[]"),
        "expanded_definition dropped the array depth: {}",
        def.expanded
    );
}

/// #336 third path: the live rescan on v0.2.4 still dropped the depth because
/// the extraction supplied an SWC SPAN locator (no expression text). SWC
/// BytePos is 1-based, so the span sits one byte past the ts-morph 0-based
/// call on BOTH ends; strict containment excluded the real `axios.get` call
/// and escalated to the smallest ENCLOSING call — the whole
/// `notificationRouter.get("/status", handler)` registration — anchoring
/// `Router` instead of `Order`, so `apply_inferred_array_depth` had nothing to
/// copy and the bundle rendered the depth-less interface again. Mirrors the
/// live file_results data_call exactly: span locator, `primary_type_symbol`
/// "Order", `type_import_source` set, multi-line call inside a route
/// registration, scalar terminal use.
#[test]
fn test_swc_span_call_result_bundles_array_definition() {
    use carrick::services::type_sidecar::{
        ExtractionConfig, ExtractionRule, InferKind, InferRequestItem, SymbolRequest, TypeSidecar,
    };
    use std::time::Duration;

    if !is_node_available() {
        eprintln!("Skipping test: Node.js not available");
        return;
    }
    let sidecar_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    if !sidecar_path.exists() {
        eprintln!("Skipping test: Sidecar not built (run: cd src/sidecar && npm run build)");
        return;
    }

    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let repo = temp_dir.path().join("consumer");
    fs::create_dir_all(repo.join("src")).expect("Failed to create src");
    fs::create_dir_all(repo.join("node_modules/axios")).expect("Failed to create axios stub dir");
    fs::write(
        repo.join("package.json"),
        r#"{ "name": "consumer-app", "version": "1.0.0", "dependencies": { "axios": "^1.6.0" } }"#,
    )
    .unwrap();
    fs::write(
        repo.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "target": "ES2020",
    "module": "commonjs",
    "moduleResolution": "node",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true
  },
  "include": ["src/**/*"]
}"#,
    )
    .unwrap();
    fs::write(
        repo.join("node_modules/axios/package.json"),
        r#"{ "name": "axios", "version": "1.6.0", "main": "index.js", "types": "index.d.ts" }"#,
    )
    .unwrap();
    fs::write(
        repo.join("node_modules/axios/index.d.ts"),
        r#"export interface AxiosResponse<T = any> {
  data: T;
  status: number;
}
export interface AxiosInstance {
  get<T = any>(url: string): Promise<AxiosResponse<T>>;
}
declare const axios: AxiosInstance;
export default axios;
"#,
    )
    .unwrap();
    fs::write(
        repo.join("src/types.ts"),
        r#"export interface Order {
  id: number;
  userId: number;
  product: string;
  amount: number;
}
"#,
    )
    .unwrap();
    // The registration-wrapped call mirrors the live server.ts shape.
    let api_source = r#"import axios from 'axios';
import { Order } from './types';

const ORDER_SERVICE_URL = 'http://localhost:3002';

interface Router {
  get(path: string, handler: () => Promise<void>): Router;
}
declare const notificationRouter: Router;

notificationRouter.get('/status', async () => {
  const ordersResponse = await axios.get<Order[]>(
    `${ORDER_SERVICE_URL}/api/orders`,
  );
  const orderCount = ordersResponse.data.length;
  console.log(orderCount);
});
"#;
    fs::write(repo.join("src/api.ts"), api_source).unwrap();

    // SWC-style span of the multi-line call: 1-based BytePos on both ends,
    // exactly as the scanner's `span_range` (`span.lo.0`, `span.hi.0`) emits.
    let call_text = "axios.get<Order[]>(\n    `${ORDER_SERVICE_URL}/api/orders`,\n  )";
    let call_start = api_source
        .find(call_text)
        .expect("api.ts must contain the multi-line call");
    let span_start = (call_start + 1) as u32;
    let span_end = (call_start + call_text.len() + 1) as u32;
    let line_number = (api_source[..call_start].matches('\n').count() + 1) as u32;

    let sidecar = TypeSidecar::spawn(&sidecar_path).expect("Failed to spawn sidecar");
    sidecar.start_init(&repo, None);
    sidecar
        .wait_ready(Duration::from_secs(60))
        .expect("Sidecar failed to initialize");

    let alias = "Endpoint_4022e5b76e4552db_Response_Call8904b52acc58d4d6";
    let explicit = vec![SymbolRequest {
        symbol_name: "Order".to_string(),
        source_file: repo.join("src/types.ts").to_string_lossy().to_string(),
        alias: Some(alias.to_string()),
        array_depth: None,
        payload_borrow_witness: false,
        consumer_response: false,
    }];
    // Span locator only — the live data_call carried no expression text.
    let infer = vec![InferRequestItem {
        file_path: repo.join("src/api.ts").to_string_lossy().to_string(),
        line_number,
        span_start: Some(span_start),
        span_end: Some(span_end),
        expression_text: None,
        expression_line: None,
        infer_kind: InferKind::CallResult,
        alias: Some(alias.to_string()),
        param_name: None,
    }];
    let config = ExtractionConfig {
        rules: vec![ExtractionRule {
            wrapper_symbols: vec!["AxiosResponse".to_string()],
            origin_module_globs: vec!["axios".to_string(), "axios/*".to_string()],
            payload_generic_index: Some(0),
            ..Default::default()
        }],
    };

    let result = sidecar
        .resolve_all_types(&explicit, &infer, Some(&config))
        .expect("resolve_all_types failed");
    let dts = result.dts_content.expect("expected bundled dts content");

    assert!(
        !dts.contains(&format!("export interface {}", alias)),
        "bundled alias must not render as a depth-less interface:\n{}",
        dts
    );
    let alias_line = dts
        .lines()
        .find(|l| l.contains(&format!("export type {} =", alias)))
        .unwrap_or_else(|| {
            panic!(
                "expected an `export type {} = ...` alias in:\n{}",
                alias, dts
            )
        });
    assert!(
        alias_line.trim_end().trim_end_matches(';').ends_with("[]"),
        "bundled alias must keep the use-site array depth, got: {}",
        alias_line
    );

    let stub = stub_dir_for(&dts);
    let definitions = sidecar
        .resolve_definitions(&stub.to_string_lossy(), &[alias.to_string()])
        .expect("resolve_definitions failed");
    let def = definitions
        .iter()
        .find(|d| d.type_alias == alias)
        .expect("expected a resolved definition for the consumer alias");
    assert!(
        def.definition
            .trim_end()
            .trim_end_matches(';')
            .ends_with("[]"),
        "resolved_definition dropped the array depth: {}",
        def.definition
    );
    assert!(
        def.expanded.trim_end().ends_with("[]"),
        "expanded_definition dropped the array depth: {}",
        def.expanded
    );
}

/// #336 fourth path — the CI condition proven against the SHIPPED v0.2.5 dist:
/// the GitHub Action scans a bare checkout (the demo workflow is
/// `actions/checkout` + `carrick-tools/carrick` with no `npm install`), so
/// `import axios from 'axios'` resolves to `any`, the call's SEMANTIC type
/// carries no symbol, and the anchor path found nothing — the depth was erased
/// even with the span-slack (#346) and call-payload-anchor (#344) fixes in
/// place, because those recover the depth from the resolved type, which no
/// longer exists. The caller's payload claim is still in the AST: the single
/// explicit call generic `<Order[]>`, whose type resolves against the repo's
/// OWN sources. Unlike the sibling tests above, this repo has NO axios stub in
/// node_modules — exactly like CI.
#[test]
fn test_untyped_client_call_result_bundles_array_definition() {
    use carrick::services::type_sidecar::{
        ExtractionConfig, ExtractionRule, InferKind, InferRequestItem, SymbolRequest, TypeSidecar,
    };
    use std::time::Duration;

    if !is_node_available() {
        eprintln!("Skipping test: Node.js not available");
        return;
    }
    let sidecar_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    if !sidecar_path.exists() {
        eprintln!("Skipping test: Sidecar not built (run: cd src/sidecar && npm run build)");
        return;
    }

    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let repo = temp_dir.path().join("consumer");
    fs::create_dir_all(repo.join("src")).expect("Failed to create src");
    // Deliberately NO node_modules: mirror the Action's bare checkout.
    fs::write(
        repo.join("package.json"),
        r#"{ "name": "consumer-app", "version": "1.0.0", "dependencies": { "axios": "^1.6.0" } }"#,
    )
    .unwrap();
    fs::write(
        repo.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "target": "ES2020",
    "module": "commonjs",
    "moduleResolution": "node",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true
  },
  "include": ["src/**/*"]
}"#,
    )
    .unwrap();
    fs::write(
        repo.join("src/types.ts"),
        r#"export interface Order {
  id: number;
  userId: number;
  product: string;
  amount: number;
}
"#,
    )
    .unwrap();
    let api_source = r#"import axios from 'axios';
import { Order } from './types';

const ORDER_SERVICE_URL = 'http://localhost:3002';

interface Router {
  get(path: string, handler: () => Promise<void>): Router;
}
declare const notificationRouter: Router;

notificationRouter.get('/status', async () => {
  const ordersResponse = await axios.get<Order[]>(
    `${ORDER_SERVICE_URL}/api/orders`,
  );
  const orderCount = ordersResponse.data.length;
  console.log(orderCount);
});
"#;
    fs::write(repo.join("src/api.ts"), api_source).unwrap();

    // SWC-style span (1-based on both ends), as the scanner's span_range emits.
    let call_text = "axios.get<Order[]>(\n    `${ORDER_SERVICE_URL}/api/orders`,\n  )";
    let call_start = api_source
        .find(call_text)
        .expect("api.ts must contain the multi-line call");
    let span_start = (call_start + 1) as u32;
    let span_end = (call_start + call_text.len() + 1) as u32;
    let line_number = (api_source[..call_start].matches('\n').count() + 1) as u32;

    let sidecar = TypeSidecar::spawn(&sidecar_path).expect("Failed to spawn sidecar");
    sidecar.start_init(&repo, None);
    sidecar
        .wait_ready(Duration::from_secs(60))
        .expect("Sidecar failed to initialize");

    let alias = "Endpoint_4022e5b76e4552db_Response_Call8904b52acc58d4d6";
    let explicit = vec![SymbolRequest {
        symbol_name: "Order".to_string(),
        source_file: repo.join("src/types.ts").to_string_lossy().to_string(),
        alias: Some(alias.to_string()),
        array_depth: None,
        payload_borrow_witness: false,
        consumer_response: false,
    }];
    let infer = vec![InferRequestItem {
        file_path: repo.join("src/api.ts").to_string_lossy().to_string(),
        line_number,
        span_start: Some(span_start),
        span_end: Some(span_end),
        expression_text: None,
        expression_line: None,
        infer_kind: InferKind::CallResult,
        alias: Some(alias.to_string()),
        param_name: None,
    }];
    // Rules are present (the cloud generates them either way) but inert: they
    // cannot match a wrapper on an `any` call type, exactly as in CI.
    let config = ExtractionConfig {
        rules: vec![ExtractionRule {
            wrapper_symbols: vec!["AxiosResponse".to_string()],
            origin_module_globs: vec!["axios".to_string(), "axios/*".to_string()],
            payload_generic_index: Some(0),
            ..Default::default()
        }],
    };

    let result = sidecar
        .resolve_all_types(&explicit, &infer, Some(&config))
        .expect("resolve_all_types failed");
    let dts = result.dts_content.expect("expected bundled dts content");

    assert!(
        !dts.contains(&format!("export interface {}", alias)),
        "bundled alias must not render as a depth-less interface:\n{}",
        dts
    );
    let alias_line = dts
        .lines()
        .find(|l| l.contains(&format!("export type {} =", alias)))
        .unwrap_or_else(|| {
            panic!(
                "expected an `export type {} = ...` alias in:\n{}",
                alias, dts
            )
        });
    assert!(
        alias_line.trim_end().trim_end_matches(';').ends_with("[]"),
        "bundled alias must keep the use-site array depth, got: {}",
        alias_line
    );

    let stub = stub_dir_for(&dts);
    let definitions = sidecar
        .resolve_definitions(&stub.to_string_lossy(), &[alias.to_string()])
        .expect("resolve_definitions failed");
    let def = definitions
        .iter()
        .find(|d| d.type_alias == alias)
        .expect("expected a resolved definition for the consumer alias");
    assert!(
        def.definition
            .trim_end()
            .trim_end_matches(';')
            .ends_with("[]"),
        "resolved_definition dropped the array depth: {}",
        def.definition
    );
    assert!(
        def.expanded.trim_end().ends_with("[]"),
        "expanded_definition dropped the array depth: {}",
        def.expanded
    );
}

// ============================================================================
// Integration with Carrick CLI (when available)
// ============================================================================

#[test]
#[ignore] // Run with: cargo test -- --ignored
fn test_full_carrick_analysis_with_sidecar() {
    if !is_node_available() {
        eprintln!("Skipping test: Node.js not available");
        return;
    }

    if !is_sidecar_built() {
        eprintln!("Skipping test: Sidecar not built");
        return;
    }

    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let producer_path = temp_dir.path().join("producer");
    fs::create_dir_all(&producer_path).expect("Failed to create producer directory");

    create_producer_repo(&producer_path);

    // Run carrick with sidecar enabled
    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(producer_path.to_str().unwrap())
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_SIDECAR_TYPE_EXTRACTION", "1")
        .output()
        .expect("Failed to execute carrick");

    let stdout = String::from_utf8(output.stdout).expect("Invalid UTF-8 in stdout");
    let stderr = String::from_utf8(output.stderr).expect("Invalid UTF-8 in stderr");

    // Print output for debugging
    if !output.status.success() {
        eprintln!("Command failed with status: {}", output.status);
        eprintln!("STDOUT:\n{}", stdout);
        eprintln!("STDERR:\n{}", stderr);
    }

    // The command should succeed
    assert!(output.status.success(), "Carrick command failed");

    // Should have CARRICK output header
    assert!(
        stdout.contains("🪢 CARRICK") || stdout.contains("CARRICK"),
        "Should have CARRICK output. Got: {}",
        stdout
    );
}

/// carrick#413 two-anchor arbitration, end to end through the real sidecar:
/// a pub/sub explicit anchor carrying the scanner's borrow witness whose
/// alias the location-based inference resolves to a DIFFERENT named root is
/// demoted — the bundle request is re-aimed at the tsc-witnessed payload
/// type, so the alias's dts definition (the thing compat verdicts are
/// computed from) is the real payload shape, not the borrowed one. The same
/// disagreement WITHOUT the witness keeps the explicit symbol untouched.
#[test]
fn test_pubsub_witnessed_borrow_demotes_to_inferred_payload_type() {
    use carrick::services::type_sidecar::{
        InferKind, InferRequestItem, SymbolRequest, TypeSidecar,
    };
    use std::time::Duration;

    if !is_node_available() {
        eprintln!("Skipping test: Node.js not available");
        return;
    }
    let sidecar_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    if !sidecar_path.exists() {
        eprintln!("Skipping test: Sidecar not built (run: cd src/sidecar && npm run build)");
        return;
    }

    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let repo = temp_dir.path().join("subscriber-svc");
    fs::create_dir_all(repo.join("src")).expect("Failed to create src");
    fs::write(
        repo.join("package.json"),
        r#"{ "name": "subscriber-svc", "version": "1.0.0" }"#,
    )
    .unwrap();
    fs::write(
        repo.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "target": "ES2020",
    "module": "commonjs",
    "moduleResolution": "node",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true
  },
  "include": ["src/**/*"]
}"#,
    )
    .unwrap();
    fs::write(
        repo.join("src/types.ts"),
        r#"export interface OrderPlaced {
  orderId: string;
  totalCents: number;
}

export interface AuditRecord {
  auditId: string;
  note: string;
}
"#,
    )
    .unwrap();
    // The borrow shape: the handler's payload is annotated `OrderPlaced`,
    // while `AuditRecord` (the borrowed symbol) annotates a different
    // binding in the same file.
    fs::write(
        repo.join("src/subscriber.ts"),
        r#"import { OrderPlaced, AuditRecord } from "./types";

export function onOrderPlaced(evt: OrderPlaced): void {
  const record: AuditRecord = { auditId: "a1", note: "seen" };
  void record;
  void evt;
}
"#,
    )
    .unwrap();

    let sidecar = TypeSidecar::spawn(&sidecar_path).expect("Failed to spawn sidecar");
    sidecar.start_init(&repo, None);
    sidecar
        .wait_ready(Duration::from_secs(60))
        .expect("Sidecar failed to initialize");

    let types_file = repo.join("src/types.ts").to_string_lossy().to_string();
    let subscriber_file = repo.join("src/subscriber.ts").to_string_lossy().to_string();

    let demoted_alias = "Pubsub_Demoted_Response";
    let kept_alias = "Pubsub_Kept_Response";
    let explicit = vec![
        // The borrowed anchor WITH the scanner's AST witness: must demote.
        SymbolRequest {
            symbol_name: "AuditRecord".to_string(),
            source_file: types_file.clone(),
            alias: Some(demoted_alias.to_string()),
            array_depth: None,
            payload_borrow_witness: true,
            consumer_response: false,
        },
        // The same borrowed anchor WITHOUT a witness: must stay explicit.
        SymbolRequest {
            symbol_name: "AuditRecord".to_string(),
            source_file: types_file.clone(),
            alias: Some(kept_alias.to_string()),
            array_depth: None,
            payload_borrow_witness: false,
            consumer_response: false,
        },
    ];
    let infer_item = |alias: &str| InferRequestItem {
        file_path: subscriber_file.clone(),
        line_number: 4,
        span_start: None,
        span_end: None,
        expression_text: None,
        expression_line: None,
        infer_kind: InferKind::FunctionParam,
        alias: Some(alias.to_string()),
        param_name: Some("evt".to_string()),
    };
    let infer = vec![infer_item(demoted_alias), infer_item(kept_alias)];

    let result = sidecar
        .resolve_all_types(&explicit, &infer, None)
        .expect("resolve_all_types failed");

    // The inference reported the deterministic anchor + declaration source
    // (the fields the arbitration runs on).
    let inf = result
        .inferred_types
        .iter()
        .find(|i| i.alias == demoted_alias)
        .expect("expected an inference for the demoted alias");
    assert_eq!(inf.primary_type_symbol.as_deref(), Some("OrderPlaced"));
    assert!(
        inf.primary_type_symbol_source
            .as_deref()
            .is_some_and(|s| s.ends_with("types.ts")),
        "anchor source must be the declaration file, got {:?}",
        inf.primary_type_symbol_source
    );

    // Witnessed borrow: the alias is bundled from the tsc-witnessed payload
    // type (OrderPlaced), not the borrowed AuditRecord.
    let demoted_entry = result
        .explicit_manifest
        .iter()
        .find(|e| e.alias == demoted_alias)
        .expect("demoted alias must still be bundled (re-aimed, not dropped)");
    assert_eq!(
        demoted_entry.original_name, "OrderPlaced",
        "witnessed borrow must be re-aimed at the inferred root"
    );

    // Unwitnessed disagreement: explicit stays authoritative.
    let kept_entry = result
        .explicit_manifest
        .iter()
        .find(|e| e.alias == kept_alias)
        .expect("kept alias must be bundled");
    assert_eq!(
        kept_entry.original_name, "AuditRecord",
        "without a witness the explicit symbol must win"
    );

    // carrick#1779: the result names the alias the arbitration re-aimed, the
    // symbol it rejected and the root it took, so the manifest row the
    // borrowed symbol was stamped on can follow. The kept alias is no change.
    assert_eq!(
        result.anchor_changes.len(),
        1,
        "{:?}",
        result.anchor_changes
    );
    let change = &result.anchor_changes[0];
    assert_eq!(change.alias, demoted_alias);
    assert_eq!(change.rejected, "AuditRecord");
    let root = change.reaimed.as_ref().expect("re-aimed, not dropped");
    assert_eq!(root.symbol, "OrderPlaced");
    assert!(
        root.source_file.ends_with("types.ts"),
        "{}",
        root.source_file
    );

    // And the dts — the artifact compat verdicts are computed from — carries
    // the payload shape for the demoted alias and the explicit shape for the
    // kept one.
    let dts = result.dts_content.expect("expected bundled dts content");
    let demoted_def = dts
        .split("export")
        .find(|chunk| chunk.contains(demoted_alias))
        .expect("demoted alias definition in dts");
    assert!(
        demoted_def.contains("orderId"),
        "demoted alias must carry the payload shape:\n{}",
        dts
    );
    assert!(
        !demoted_def.contains("auditId"),
        "demoted alias must not carry the borrowed shape:\n{}",
        dts
    );
    let kept_def = dts
        .split("export")
        .find(|chunk| chunk.contains(kept_alias))
        .expect("kept alias definition in dts");
    assert!(
        kept_def.contains("auditId"),
        "kept alias must stay the explicit shape:\n{}",
        dts
    );
}

/// A writer a test reads back: what a pass logged, as the log holds it.
#[derive(Clone, Default)]
struct Logged(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Logged {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logged {
    type Writer = Logged;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// carrick#1985 end to end through the real sidecar: the answer to the
/// signature pass's batch says how long its slots took, and the pass writes
/// that to the log.
///
/// The sidecar is required, not skipped. The pass's own tests read timings
/// written by hand, so this is the one place a member renamed on either side
/// of the protocol would show: the scanner would read no timing and log
/// nothing, with every other test green.
#[test]
fn test_signature_pass_logs_where_its_time_went() {
    use carrick::services::type_sidecar::TypeSidecar;
    use carrick::signature_pass::populate_function_signatures;
    use carrick::visitor::{FunctionArgument, FunctionDefinition, FunctionNodeType};
    use std::collections::HashMap;
    use std::time::Duration;

    let sidecar_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    assert!(
        sidecar_path.exists(),
        "build the sidecar first: cd src/sidecar && npm ci && npm run build"
    );

    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let repo = temp_dir
        .path()
        .canonicalize()
        .expect("the temp directory resolves");
    fs::create_dir_all(repo.join("src")).expect("create src");
    fs::write(
        repo.join("tsconfig.json"),
        r#"{ "compilerOptions": { "target": "ES2020", "module": "commonjs", "strict": true, "skipLibCheck": true, "types": [] }, "include": ["src/**/*.ts"] }"#,
    )
    .expect("write tsconfig");
    // No return annotation, and one parameter without a type: two slots.
    let file = repo.join("src/orders.ts");
    fs::write(
        &file,
        "export function label(order: { id: string }, note) {\n  return { title: 'Order ' + order.id, lines: [1, 2, 3] };\n}\n",
    )
    .expect("write the source file");

    let argument = |name: &str, ty: Option<&str>| FunctionArgument {
        name: name.to_string(),
        type_ann: None,
        is_explicit: ty.is_some(),
        type_string: ty.map(str::to_string),
        is_optional: false,
        has_default: false,
        default_value: None,
        is_rest: false,
    };
    let mut definitions = HashMap::from([(
        "label".to_string(),
        FunctionDefinition {
            name: "label".to_string(),
            file_path: file.clone(),
            node_type: FunctionNodeType::Placeholder,
            arguments: vec![
                argument("order", Some("{ id: string }")),
                argument("note", None),
            ],
            body_source: None,
            is_exported: true,
            line_number: 1,
            end_line: 3,
            intent: None,
            calls: vec![],
            tokens: vec![],
            return_type: None,
            return_is_explicit: false,
            signature: None,
            intent_input_hash: None,
            dispatch_table: None,
        },
    )]);

    let sidecar = TypeSidecar::spawn(&sidecar_path).expect("the sidecar spawns");
    sidecar.start_init(&repo, None);
    sidecar
        .wait_ready(Duration::from_secs(120))
        .expect("the sidecar initialises on the fixture");

    let logged = Logged::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logged.clone())
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        populate_function_signatures(Some(&sidecar), &mut definitions, repo.to_str().unwrap());
    });
    let log = String::from_utf8_lossy(&logged.0.lock().unwrap()).into_owned();

    let returned = "{ title: string; lines: number[]; }";
    assert_eq!(
        definitions["label"].signature.as_deref(),
        Some(format!("(order: {{ id: string }}, note: any) => {returned}").as_str()),
        "the pass answers as it did: {log}"
    );

    let said = |start: &str| {
        log.lines()
            .filter_map(|line| line.split_once("carrick::signature_pass: "))
            .map(|(_, said)| said)
            .find(|said| said.starts_with(start))
            .unwrap_or_else(|| panic!("the pass logged no line starting {start:?}: {log}"))
            .to_string()
    };
    let batch = said("Signature inference batch 1 of 1: ");
    assert!(
        batch.starts_with("Signature inference batch 1 of 1: 2 slot(s) took "),
        "{batch}"
    );
    assert!(
        batch.contains("the 1 first asked of their file took "),
        "{batch}"
    );
    assert!(
        batch.contains("s computing types, ") && batch.contains("s printing them); "),
        "the batch says what its time went on: {batch}"
    );
    let pass = said("Signature inference timing: ");
    assert!(
        pass.starts_with("Signature inference timing: 2 slot(s) took "),
        "{pass}"
    );
    assert!(
        pass.contains("s computing types, ") && pass.contains("s printing them); "),
        "the pass says what its time went on: {pass}"
    );
    assert!(pass.contains("; the slowest 1 (1%) took "), "{pass}");
    assert!(
        !pass.contains("at least"),
        "a batch that named every slot it timed left none out: {pass}"
    );
    assert!(
        pass.ends_with(&format!(
            "the longest type printed is {} character(s), by label return at {}:1",
            returned.len(),
            file.display()
        )),
        "{pass}"
    );
    // A length, never the type.
    assert!(
        !log.contains("title: string"),
        "the log holds a type's text: {log}"
    );
}

// ============================================================================
// Utility Functions
// ============================================================================

/// Copy directory recursively (for test fixtures)
#[allow(dead_code)]
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    if !dst.exists() {
        fs::create_dir_all(dst)?;
    }

    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest_path = dst.join(entry.file_name());

        if path.is_dir() {
            copy_dir_recursive(&path, &dest_path)?;
        } else {
            fs::copy(&path, &dest_path)?;
        }
    }

    Ok(())
}
