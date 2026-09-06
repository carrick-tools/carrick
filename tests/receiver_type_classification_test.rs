//! carrick#695 end to end: a bare `x.verb("/lit", arg)` site classified by the
//! type of its RECEIVER, through the real type sidecar.
//!
//! The two sites in the fixture are written identically — same verb, same route
//! literal, same "a path and one more argument" shape — and differ only in what
//! the receiver IS. That is the point of the ticket: no rule about the call's
//! shape can separate them (ruling, 2026-09-05), and the compiler can. One
//! receiver is an instance of a package the framework detection named a server
//! framework, the other of one it named a data fetcher, so the first site is a
//! route and the second is a request.
//!
//! The dependency declarations are written at test time rather than checked in:
//! the whole answer depends on them being resolvable, and a checkout with no
//! installed dependencies resolves neither — which the second test asserts,
//! because that is the shape CI scans today.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use carrick::agent_service::AgentService;
use carrick::agents::file_analyzer_agent::ResolutionSource;
use carrick::agents::file_orchestrator::FileOrchestrator;
use carrick::agents::framework_guidance_agent::{
    FrameworkGuidance, PatternExample, ProtocolGuidance,
};
use carrick::framework_detector::DetectionResult;
use carrick::operation::Protocol;
use carrick::services::type_sidecar::TypeSidecar;
use serial_test::serial;
use tempfile::TempDir;

const APP_TS: &str = r#"import { createServer } from "server-fw";
import { createClient } from "http-fetcher";

const app = createServer();
const api = createClient();

app.get("/widgets", (req, res) => res.send("ok"));
api.get("/widgets", { retries: 2 });
"#;

const SERVER_DTS: &str = r#"export declare class Server {
  get(path: string, handler: (req: unknown, res: { send(body: string): void }) => void): void;
}
export declare function createServer(): Server;
"#;

const CLIENT_DTS: &str = r#"export declare class HttpClient {
  get<T = unknown>(path: string, options?: { retries?: number }): Promise<T>;
}
export declare function createClient(): HttpClient;
"#;

fn write_package(root: &Path, name: &str, dts: &str) {
    let dir = root.join("node_modules").join(name);
    fs::create_dir_all(&dir).expect("package dir");
    fs::write(
        dir.join("package.json"),
        format!(r#"{{"name":"{name}","version":"1.0.0","types":"index.d.ts"}}"#),
    )
    .expect("package.json");
    fs::write(dir.join("index.d.ts"), dts).expect("index.d.ts");
}

/// The fixture repo. `installed` writes the two declaration packages; without
/// them both receivers resolve to `any`, which is the CI checkout's shape.
fn write_repo(root: &Path, installed: bool) {
    fs::create_dir_all(root.join("src")).expect("src dir");
    fs::write(
        root.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "strict": true,
    "rootDir": "src",
    "module": "esnext",
    "moduleResolution": "bundler",
    "target": "es2022",
    "lib": ["es2022"],
    "skipLibCheck": true
  },
  "include": ["src"]
}"#,
    )
    .expect("tsconfig");
    fs::write(root.join("src/app.ts"), APP_TS).expect("app.ts");
    if installed {
        write_package(root, "server-fw", SERVER_DTS);
        write_package(root, "http-fetcher", CLIENT_DTS);
    }
}

fn detection() -> DetectionResult {
    DetectionResult {
        frameworks: vec!["server-fw".to_string()],
        data_fetchers: vec!["http-fetcher".to_string()],
        messaging_clients: vec![],
        notes: String::new(),
    }
}

fn guidance() -> ProtocolGuidance {
    ProtocolGuidance::from([(
        Protocol::Http,
        FrameworkGuidance {
            mount_patterns: vec![],
            endpoint_patterns: vec![PatternExample {
                pattern: ".get(".to_string(),
                description: "GET endpoint".to_string(),
                framework: "server-fw".to_string(),
            }],
            middleware_patterns: vec![],
            data_fetching_patterns: vec![],
            triage_hints: String::new(),
            parsing_notes: String::new(),
        },
    )])
}

fn is_node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn sidecar_path() -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    path.exists().then_some(path)
}

/// Rows the deterministic layer states for the fixture, with a real sidecar up.
/// `None` when the sidecar is not available to run.
struct Rows {
    /// Rows still carrying `ReceiverType` after the model join.
    endpoints: Vec<(String, String)>,
    calls: Vec<(String, String)>,
    /// What the DETERMINISTIC layer emitted, before the model join: the
    /// producer side keeps the model's richer row when the model also saw the
    /// route (the rule every structural route already lives by), so this is
    /// where a route the receiver's type stated is counted.
    emitted_by_receiver_type: usize,
    classified_endpoints: usize,
    classified_calls: usize,
    unresolved: usize,
}

async fn deterministic_rows(installed: bool) -> Option<Rows> {
    if !is_node_available() {
        eprintln!("Skipping test: Node.js not available");
        return None;
    }
    let Some(sidecar_path) = sidecar_path() else {
        eprintln!("Skipping test: sidecar not built (run: cd src/sidecar && npm run build)");
        return None;
    };

    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("api");
    write_repo(&root, installed);

    let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
    sidecar.start_init(&root, None);
    sidecar
        .wait_ready(Duration::from_secs(60))
        .expect("sidecar init");

    // The empty-cassette instrument: the mock analyzer replays a fixture
    // directory, and an empty one means the model returns nothing. What
    // remains is exactly what the deterministic layer states on its own —
    // which is the claim under test. (With a model answer at these spans the
    // producer join keeps the model's richer row, as it does for every other
    // structural route; the classification still happened, but the row would
    // no longer carry its source.)
    let empty_cassette = temp.path().join("__llm__");
    fs::create_dir_all(&empty_cassette).expect("cassette dir");
    // SAFETY: serial test; env vars are process-global.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var(
            "CARRICK_MOCK_FIXTURE_DIR",
            empty_cassette.to_string_lossy().to_string(),
        );
    }
    let orchestrator = FileOrchestrator::new(AgentService::new());
    let result = orchestrator
        .analyze_files(
            &[root.join("src/app.ts")],
            &std::collections::HashMap::new(),
            &guidance(),
            &detection(),
            &root,
            &root,
            &[],
            &Default::default(),
            &Default::default(),
            &carrick::url_normalizer::UrlNormalizer::default_permissive(),
            Some(&sidecar),
        )
        .await
        .expect("analysis should succeed");
    unsafe {
        std::env::remove_var("CARRICK_MOCK_ALL");
        std::env::remove_var("CARRICK_MOCK_FIXTURE_DIR");
    }

    let mut endpoints = Vec::new();
    let mut calls = Vec::new();
    for file_result in result.file_results.values() {
        for endpoint in &file_result.endpoints {
            if endpoint.resolution_source == Some(ResolutionSource::ReceiverType) {
                endpoints.push((endpoint.method.clone(), endpoint.path.clone()));
            }
        }
        for call in &file_result.data_calls {
            if call.resolution_source == Some(ResolutionSource::ReceiverType) {
                calls.push((call.method.clone().unwrap_or_default(), call.target.clone()));
            }
        }
    }
    Some(Rows {
        endpoints,
        calls,
        emitted_by_receiver_type: result
            .stats
            .deterministic_rows_emitted
            .get(&ResolutionSource::ReceiverType)
            .copied()
            .unwrap_or(0),
        classified_endpoints: result.stats.receiver_classified_endpoints,
        classified_calls: result.stats.receiver_classified_calls,
        unresolved: result.stats.receiver_unresolved,
    })
}

#[tokio::test]
#[serial]
async fn two_identically_shaped_sites_split_on_what_their_receivers_are() {
    let Some(rows) = deterministic_rows(true).await else {
        return;
    };

    assert_eq!(
        rows.classified_endpoints, 1,
        "the server-typed receiver states a route"
    );
    assert_eq!(
        rows.classified_calls, 1,
        "the client-typed receiver states a request"
    );
    assert_eq!(rows.unresolved, 0, "both receivers resolved");
    assert_eq!(
        rows.emitted_by_receiver_type, 2,
        "both rows were emitted by the deterministic layer"
    );
    // The consumer side keeps the deterministic row and folds the model's
    // answer onto it, so the request row still names its source.
    assert_eq!(
        rows.calls,
        vec![("GET".to_string(), "/widgets".to_string())]
    );
}

#[tokio::test]
#[serial]
async fn a_checkout_with_no_installed_dependencies_states_neither() {
    let Some(rows) = deterministic_rows(false).await else {
        return;
    };

    assert_eq!(rows.classified_endpoints, 0);
    assert_eq!(rows.classified_calls, 0);
    assert_eq!(
        rows.unresolved, 2,
        "both receivers were asked about and neither resolved"
    );
    assert_eq!(rows.emitted_by_receiver_type, 0);
    assert!(
        rows.endpoints.is_empty() && rows.calls.is_empty(),
        "no row claims a role it could not read: {:?} {:?}",
        rows.endpoints,
        rows.calls
    );
}
