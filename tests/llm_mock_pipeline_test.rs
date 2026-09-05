//! End-to-end pipeline test with fixture-driven mock LLM output.
//!
//! Drives `FileOrchestrator::analyze_files` over a realistic multi-file
//! Express service (`tests/fixtures/llm-mocked-api/`) while the agent layer
//! replays canned analyze-file responses from
//! `tests/fixtures/llm-mocked-api/__llm__/`. The canned output contains the
//! kinds of imperfections the validation pipeline exists to absorb:
//!
//! - a hallucinated endpoint whose candidate_id matches no SWC candidate
//!   (must be dropped by the candidate gate),
//! - a type hint whose import source points at the framework package
//!   (must be nulled so inference runs instead),
//! - a type symbol that exists nowhere in the file's symbol table
//!   (must be nulled),
//! - a valid type hint (must survive untouched),
//! - a data call inside a custom wrapper function (must be kept and get
//!   SWC spans), plus a cross-file mount that must resolve to full paths.

use carrick::agent_service::AgentService;
use carrick::agents::file_analyzer_agent::EmissionStyle;
use carrick::agents::file_orchestrator::FileOrchestrator;
use carrick::agents::framework_guidance_agent::{
    FrameworkGuidance, PatternExample, ProtocolGuidance,
};
use carrick::config::Config;
use carrick::framework_detector::DetectionResult;
use carrick::operation::Protocol;
use carrick::services::type_sidecar::InferKind;
use serial_test::serial;
use std::path::PathBuf;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/llm-mocked-api")
}

fn express_guidance() -> FrameworkGuidance {
    let pattern = |pattern: &str, description: &str| PatternExample {
        pattern: pattern.to_string(),
        description: description.to_string(),
        framework: "express".to_string(),
    };
    FrameworkGuidance {
        mount_patterns: vec![pattern(".use(", "Mount middleware or router")],
        endpoint_patterns: vec![
            pattern(".get(", "GET endpoint"),
            pattern(".post(", "POST endpoint"),
        ],
        middleware_patterns: vec![],
        data_fetching_patterns: vec![PatternExample {
            pattern: "fetch(".to_string(),
            description: "Fetch API call".to_string(),
            framework: "native".to_string(),
        }],
        triage_hints: String::new(),
        parsing_notes: String::new(),
    }
}

fn express_detection() -> DetectionResult {
    DetectionResult {
        frameworks: vec!["express".to_string()],
        data_fetchers: vec!["fetch".to_string()],
        messaging_clients: vec![],
        notes: String::new(),
    }
}

#[tokio::test]
#[serial]
async fn mock_llm_output_flows_through_validation_and_mount_graph() {
    let root = fixture_root();
    // SAFETY: serial test; env vars are process-global.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var(
            "CARRICK_MOCK_FIXTURE_DIR",
            root.join("__llm__").to_string_lossy().to_string(),
        );
    }

    let files = vec![
        root.join("src/index.ts"),
        root.join("src/routes/users.ts"),
        root.join("src/client.ts"),
        root.join("src/types.ts"),
        root.join("src/socket.ts"),
        root.join("src/reports.ts"),
    ];

    let orchestrator = FileOrchestrator::new(AgentService::new());
    let result = orchestrator
        .analyze_files(
            &files,
            &std::collections::HashMap::new(),
            &ProtocolGuidance::from([(Protocol::Http, express_guidance())]),
            &express_detection(),
            &root,
            &root,
            &[],
            &Default::default(),
            &Default::default(),
            &carrick::url_normalizer::UrlNormalizer::default_permissive(),
        )
        .await
        .expect("analysis should succeed");

    // SAFETY: cleanup of env vars set above.
    unsafe {
        std::env::remove_var("CARRICK_MOCK_ALL");
        std::env::remove_var("CARRICK_MOCK_FIXTURE_DIR");
    }

    // types.ts has no call-site candidates and must be skipped before the LLM.
    assert_eq!(
        result.stats.files_skipped_no_candidates, 1,
        "types.ts should be skipped by the SWC gatekeeper"
    );

    // socket.ts only contains WebSocket/EventSource constructors — a protocol
    // with no registered analyze-file prompt. It must be routed away from the
    // HTTP prompt, not analyzed by it.
    assert_eq!(
        result.stats.files_skipped_unrouted_protocol, 1,
        "socket.ts should be skipped as unrouted-protocol, not sent to the HTTP prompt"
    );
    let socket_result = result
        .file_results
        .iter()
        .find(|(path, _)| path.ends_with("socket.ts"))
        .map(|(_, r)| r)
        .expect("socket.ts should have an (empty) cached result");
    assert!(
        socket_result.endpoints.is_empty() && socket_result.data_calls.is_empty(),
        "no HTTP facts may be invented for a socket-only file"
    );

    let users_result = result
        .file_results
        .iter()
        .find(|(path, _)| path.ends_with("users.ts"))
        .map(|(_, r)| r)
        .expect("users.ts should have a result");

    // The hallucinated DELETE endpoint (candidate_id with no SWC match) is gone.
    assert_eq!(
        users_result.endpoints.len(),
        2,
        "expected the hallucinated endpoint to be dropped, got: {:?}",
        users_result
            .endpoints
            .iter()
            .map(|e| (&e.method, &e.path))
            .collect::<Vec<_>>()
    );
    assert!(
        !users_result.endpoints.iter().any(|e| e.method == "DELETE"),
        "hallucinated DELETE endpoint must not survive the candidate gate"
    );

    // Surviving endpoints carry SWC spans for span-based type inference.
    for endpoint in &users_result.endpoints {
        assert!(
            endpoint.call_expression_span_start.is_some()
                && endpoint.call_expression_span_end.is_some(),
            "gated endpoints must carry SWC spans: {:?}",
            (&endpoint.method, &endpoint.path)
        );
    }

    // GET /: the valid type hint (User from '../types') survives.
    let get_endpoint = users_result
        .endpoints
        .iter()
        .find(|e| e.method == "GET")
        .expect("GET endpoint should survive");
    assert_eq!(get_endpoint.primary_type_symbol.as_deref(), Some("User"));
    assert_eq!(get_endpoint.type_import_source.as_deref(), Some("../types"));

    // POST /: the framework-package import source is scrubbed so the
    // sidecar infers from the payload expression instead.
    let post_endpoint = users_result
        .endpoints
        .iter()
        .find(|e| e.method == "POST")
        .expect("POST endpoint should survive");
    assert_eq!(
        post_endpoint.type_import_source, None,
        "framework package must not survive as a type import source"
    );
    assert_eq!(
        post_endpoint.primary_type_symbol, None,
        "a symbol whose claimed source does not match the import table must be nulled"
    );
    assert_eq!(
        post_endpoint.response_expression_text.as_deref(),
        Some("created"),
        "expression locators must survive type-hint scrubbing"
    );
    assert_eq!(
        post_endpoint.emission_style,
        Some(EmissionStyle::ImperativeSend),
        "emission_style must survive validation and candidate gating"
    );

    // reports.ts: emission_style routes type inference. The return-value
    // endpoint asks for the handler's return type (line-anchored
    // FunctionReturn); the no-payload endpoint asks for nothing at all,
    // leaving its manifest entry honestly unknown.
    let reports_result = result
        .file_results
        .iter()
        .find(|(path, _)| path.ends_with("reports.ts"))
        .map(|(_, r)| r)
        .expect("reports.ts should have a result");
    assert_eq!(reports_result.endpoints.len(), 2);
    let summary = reports_result
        .endpoints
        .iter()
        .find(|e| e.path == "/summary")
        .expect("/summary endpoint should survive");
    assert_eq!(summary.emission_style, Some(EmissionStyle::ReturnValue));
    let export = reports_result
        .endpoints
        .iter()
        .find(|e| e.path == "/export")
        .expect("/export endpoint should survive");
    assert_eq!(export.emission_style, Some(EmissionStyle::NoPayload));

    let (_explicit, infer, _inline) = orchestrator.collect_type_requests(
        &result.file_results,
        &root.to_string_lossy(),
        &result.mount_graph,
        &Config::default(),
    );
    let reports_items: Vec<_> = infer
        .iter()
        .filter(|item| item.file_path.ends_with("reports.ts"))
        .collect();
    assert_eq!(
        reports_items.len(),
        1,
        "only the return-value endpoint may request inference, got: {:?}",
        reports_items
    );
    let summary_item = reports_items[0];
    assert_eq!(summary_item.line_number, summary.line_number as u32);
    assert_eq!(summary_item.infer_kind, InferKind::FunctionReturn);
    assert_eq!(
        summary_item.expression_text.as_deref(),
        Some("buildSummary()"),
        "return-value inference must locate the handler via the payload expression"
    );
    assert!(
        summary_item.span_start.is_none() && summary_item.span_end.is_none(),
        "the registration-call span must not be sent for return-value endpoints: {:?}",
        summary_item
    );

    // index.ts: the bogus symbol (defined nowhere in the file) is nulled.
    let index_result = result
        .file_results
        .iter()
        .find(|(path, _)| path.ends_with("index.ts"))
        .map(|(_, r)| r)
        .expect("index.ts should have a result");
    let health = index_result
        .endpoints
        .iter()
        .find(|e| e.path == "/health")
        .expect("/health endpoint should survive");
    assert_eq!(
        health.primary_type_symbol, None,
        "a symbol absent from the symbol table must be nulled"
    );

    // client.ts: the data call inside the custom wrapper is kept, with spans.
    let client_result = result
        .file_results
        .iter()
        .find(|(path, _)| path.ends_with("client.ts"))
        .map(|(_, r)| r)
        .expect("client.ts should have a result");
    assert_eq!(client_result.data_calls.len(), 1);
    let data_call = &client_result.data_calls[0];
    assert!(data_call.target.contains("orders.internal"));
    assert!(
        data_call.call_expression_span_start.is_some(),
        "data call matched to an SWC candidate must carry spans"
    );
    assert_eq!(data_call.primary_type_symbol.as_deref(), Some("Order"));

    // Mount graph: endpoints from the mounted router resolve under the
    // mount prefix; app-level endpoints resolve at the root.
    let endpoints = result.mount_graph.get_resolved_endpoints();
    let full_paths: Vec<(String, String)> = endpoints
        .iter()
        .map(|e| (e.method.clone(), e.full_path.clone()))
        .collect();
    assert!(
        full_paths
            .iter()
            .any(|(m, p)| m.eq_ignore_ascii_case("get") && p == "/api/users"),
        "GET / on the mounted router should resolve to /api/users, got: {:?}",
        full_paths
    );
    assert!(
        full_paths
            .iter()
            .any(|(m, p)| m.eq_ignore_ascii_case("post") && p == "/api/users"),
        "POST / on the mounted router should resolve to /api/users, got: {:?}",
        full_paths
    );
    assert!(
        full_paths
            .iter()
            .any(|(m, p)| m.eq_ignore_ascii_case("get") && p == "/health"),
        "app-level GET /health should resolve at the root, got: {:?}",
        full_paths
    );
}
