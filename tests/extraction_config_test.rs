//! Tests for the framework-guidance `extraction_config` task: the cloud
//! generates machinery-unwrap rules from the repo's detected stack and
//! dependency list, and the scanner parses them into the `ExtractionConfig`
//! it forwards to the sidecar's infer action.
//!
//! Uses the mock-LLM fixture harness (`CARRICK_MOCK_FIXTURE_DIR`) with canned
//! rules for a non-axios client (`got`) and a workspace-internal wrapper
//! (`ApiEnvelope`), mirroring the cloud prompt's two rule shapes:
//! property-path unwrapping gated on origin globs, and generic-index
//! unwrapping for a local envelope type.

use carrick::agent_service::AgentService;
use carrick::agents::framework_guidance_agent::FrameworkGuidanceAgent;
use carrick::framework_detector::DetectionResult;
use serial_test::serial;
use std::path::PathBuf;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extraction-config/__llm__")
}

#[tokio::test]
#[serial]
async fn fetch_extraction_config_parses_canned_rules() {
    // SAFETY: serial test; env vars are process-global.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var(
            "CARRICK_MOCK_FIXTURE_DIR",
            fixture_dir().to_string_lossy().to_string(),
        );
    }

    let agent = FrameworkGuidanceAgent::new(AgentService::new());
    let detection = DetectionResult {
        frameworks: vec!["fastify".to_string()],
        data_fetchers: vec!["got".to_string()],
        messaging_clients: vec![],
        socket_clients: vec![],
        notes: String::new(),
    };
    let dependencies = vec!["fastify".to_string(), "got".to_string()];

    let config = agent
        .fetch_extraction_config(&detection, &dependencies)
        .await
        .expect("canned extraction config should parse");

    // SAFETY: cleanup of env vars set above.
    unsafe {
        std::env::remove_var("CARRICK_MOCK_ALL");
        std::env::remove_var("CARRICK_MOCK_FIXTURE_DIR");
    }

    assert_eq!(config.rules.len(), 2);

    // got: machinery type unwrapped via property path, gated on origin globs.
    let got_rule = &config.rules[0];
    assert_eq!(got_rule.wrapper_symbols, vec!["Response"]);
    assert!(got_rule.machinery_indicators.contains(&"body".to_string()));
    assert_eq!(got_rule.origin_module_globs.len(), 3);
    assert_eq!(got_rule.payload_generic_index, None);
    assert_eq!(got_rule.payload_property_path, vec!["body"]);

    // Workspace-internal wrapper: distinctive symbol name, no origin globs
    // (origin globs resolve against node_modules and would gate the rule
    // off for local types), generic-index unwrapping.
    let envelope_rule = &config.rules[1];
    assert_eq!(envelope_rule.wrapper_symbols, vec!["ApiEnvelope"]);
    assert!(envelope_rule.origin_module_globs.is_empty());
    assert_eq!(envelope_rule.payload_generic_index, Some(0));
    assert!(envelope_rule.payload_property_path.is_empty());

    // The sidecar's zod validator rejects null payloadGenericIndex — absent
    // values must be omitted from the serialized rule, not sent as null.
    let wire = serde_json::to_value(got_rule).unwrap();
    assert!(
        wire.get("payloadGenericIndex").is_none(),
        "absent generic index must be omitted on the wire, got: {}",
        wire
    );
    assert_eq!(wire["payloadPropertyPath"][0], "body");
}
