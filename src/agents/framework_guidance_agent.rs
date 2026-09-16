use crate::{
    agent_service::AgentService, agents::schemas::AgentSchemas,
    framework_detector::DetectionResult, operation::Protocol,
    services::type_sidecar::ExtractionConfig,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use tracing::debug;

/// Guidance keyed by protocol. Each protocol with a registered LLM
/// extraction pass gets its own focused guidance (and, downstream, its own
/// analyze-file prompt) instead of diluting one prompt across protocols.
pub type ProtocolGuidance = BTreeMap<Protocol, FrameworkGuidance>;

/// Protocols that have a guidance + analyze-file prompt registered in
/// carrick-cloud. Deterministic protocols (GraphQL) never appear here; new
/// LLM-routed protocols are added together with their cloud prompts.
const LLM_ROUTED_PROTOCOLS: &[Protocol] = &[Protocol::Http];

/// A single pattern example for a specific framework
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternExample {
    /// The code pattern, e.g., "app.route('/path', subApp)"
    pub pattern: String,

    /// What this pattern represents
    pub description: String,

    /// Which framework this is for
    pub framework: String,
}

/// Flattened response format using parallel arrays (faster for structured output)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FlatPatternResponse {
    patterns: Vec<String>,
    descriptions: Vec<String>,
    frameworks: Vec<String>,
}

impl FlatPatternResponse {
    /// Convert parallel arrays back to Vec<PatternExample>
    fn into_pattern_examples(self) -> Vec<PatternExample> {
        self.patterns
            .into_iter()
            .zip(self.descriptions)
            .zip(self.frameworks)
            .map(|((pattern, description), framework)| PatternExample {
                pattern,
                description,
                framework,
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeneralGuidanceResponse {
    triage_hints: String,
    parsing_notes: String,
}

/// Framework-specific guidance for downstream agents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameworkGuidance {
    /// Patterns for router/sub-app mounting
    pub mount_patterns: Vec<PatternExample>,

    /// Patterns for HTTP endpoint definitions
    pub endpoint_patterns: Vec<PatternExample>,

    /// Patterns for middleware registration
    pub middleware_patterns: Vec<PatternExample>,

    /// Patterns for outbound HTTP calls
    pub data_fetching_patterns: Vec<PatternExample>,

    /// Free-form hints for the triage agent
    pub triage_hints: String,

    /// Framework-specific notes that may affect parsing
    pub parsing_notes: String,

    /// One id for the five guidance answers above, folded from the
    /// `guidance_key` each `/framework-guidance` response carried
    /// (carrick-cloud#871). The analyze-file request sends it so the cloud's
    /// analysis cache can key on the guidance's identity instead of its text,
    /// which is what stops a regenerated guidance block from re-analysing
    /// every file in the repo.
    ///
    /// `None` whenever any of the five answers came back without a key — an
    /// offline run, or a cloud with the guidance cache switched off. A partial
    /// id would name guidance it does not fully describe, so there is no
    /// halfway state: the analyzer falls back to the whole-message key.
    ///
    /// Skipped when absent, and defaulted on read, because this struct is
    /// persisted in the index blob (`CloudRepoData::cached_guidance`) and a
    /// blob written before this field must still deserialise. Such a blob is
    /// not replayed: the engine asks for guidance again rather than scan under
    /// an id-less answer forever, because the gate that guards the replay does
    /// not move on an ordinary scan (carrick#1224, `guidance_is_keyed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance_key: Option<String>,
}

/// Fold the per-answer guidance keys into the single id the analyze-file
/// request carries. Order is the fixed argument order, not a sort: these are
/// five distinct answers, and which one is which is part of what the id means.
/// Length-delimited so concatenation is unambiguous.
///
/// Returns `None` if ANY answer lacked a key (see [`FrameworkGuidance`]).
fn compose_guidance_key(keys: [Option<&str>; 5]) -> Option<String> {
    let mut hasher = Sha256::new();
    for key in keys {
        let key = key?;
        hasher.update((key.len() as u64).to_le_bytes());
        hasher.update(key.as_bytes());
    }
    Some(format!("{:x}", hasher.finalize()))
}

/// Agent that generates framework-specific patterns and guidance
/// for downstream agents to use in their prompts.
///
/// This agent is fully LLM-driven - it asks the LLM to provide
/// patterns for whatever frameworks are detected, without any
/// hardcoded framework knowledge.
pub struct FrameworkGuidanceAgent {
    agent_service: AgentService,
}

impl FrameworkGuidanceAgent {
    pub fn new(agent_service: AgentService) -> Self {
        Self { agent_service }
    }

    /// Generate framework-specific guidance based on detected frameworks.
    /// Uses parallel calls to the /framework-guidance lambda for each category.
    /// All prompt construction lives lambda-side now (see
    /// carrick-cloud/lambdas/framework-guidance/prompts.js).
    /// Generate guidance for every protocol with a registered LLM pass.
    /// Detection's library inventory selects which protocols are active;
    /// today HTTP is the only routed protocol, so the map has one entry.
    /// When the websocket prompt lands, socket libraries in the inventory
    /// activate a second, independently prompted entry.
    pub async fn generate_for_active_protocols(
        &self,
        framework_detection: &DetectionResult,
    ) -> Result<ProtocolGuidance, Box<dyn std::error::Error>> {
        let mut guidance = ProtocolGuidance::new();
        for protocol in LLM_ROUTED_PROTOCOLS {
            guidance.insert(
                *protocol,
                self.generate_guidance(framework_detection, *protocol)
                    .await?,
            );
        }
        Ok(guidance)
    }

    pub async fn generate_guidance(
        &self,
        framework_detection: &DetectionResult,
        protocol: Protocol,
    ) -> Result<FrameworkGuidance, Box<dyn std::error::Error>> {
        debug!("=== FRAMEWORK GUIDANCE AGENT DEBUG ===");
        debug!(
            "Generating {:?} guidance for frameworks: {:?}",
            protocol, framework_detection.frameworks
        );
        debug!("Data fetchers: {:?}", framework_detection.data_fetchers);

        // Execute calls in parallel for speed (flattened schema makes this fast enough)
        debug!("  Fetching all patterns in parallel...");
        let mount_task = self.fetch_patterns("mount", framework_detection, protocol);
        let endpoint_task = self.fetch_patterns("endpoint", framework_detection, protocol);
        let middleware_task = self.fetch_patterns("middleware", framework_detection, protocol);
        let fetching_task = self.fetch_patterns("data_fetching", framework_detection, protocol);
        let general_task = self.fetch_general_guidance(framework_detection, protocol);

        // Wait for all tasks to complete
        let (
            mount_patterns,
            endpoint_patterns,
            middleware_patterns,
            data_fetching_patterns,
            general_guidance,
        ) = tokio::try_join!(
            mount_task,
            endpoint_task,
            middleware_task,
            fetching_task,
            general_task
        )?;

        let guidance_key = compose_guidance_key([
            mount_patterns.guidance_key.as_deref(),
            endpoint_patterns.guidance_key.as_deref(),
            middleware_patterns.guidance_key.as_deref(),
            data_fetching_patterns.guidance_key.as_deref(),
            general_guidance.guidance_key.as_deref(),
        ]);

        let guidance = FrameworkGuidance {
            mount_patterns: mount_patterns.value,
            endpoint_patterns: endpoint_patterns.value,
            middleware_patterns: middleware_patterns.value,
            data_fetching_patterns: data_fetching_patterns.value,
            triage_hints: general_guidance.value.triage_hints,
            parsing_notes: general_guidance.value.parsing_notes,
            guidance_key,
        };

        debug!("Generated guidance with:");
        debug!("  - {} mount patterns", guidance.mount_patterns.len());
        debug!("  - {} endpoint patterns", guidance.endpoint_patterns.len());
        debug!(
            "  - {} middleware patterns",
            guidance.middleware_patterns.len()
        );
        debug!(
            "  - {} data fetching patterns",
            guidance.data_fetching_patterns.len()
        );

        Ok(guidance)
    }

    /// Common /framework-guidance request body: task + protocol + the
    /// detection inventory + the response schema the lambda forwards to the
    /// model. Task-specific fields are added by the caller.
    fn guidance_request_body(
        task: &str,
        framework_detection: &DetectionResult,
        protocol: Protocol,
        schema: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "task": task,
            "protocol": protocol,
            "frameworks": framework_detection.frameworks,
            "data_fetchers": framework_detection.data_fetchers,
            "response_schema": schema,
        })
    }

    /// Fetch agent-generated machinery-unwrap rules for the repo's HTTP
    /// clients (the `extraction_config` task). `dependencies` is the cleaned
    /// list of merged package.json dependency names — the cloud prompt uses
    /// it to ground rules in packages the repo actually uses.
    pub async fn fetch_extraction_config(
        &self,
        framework_detection: &DetectionResult,
        dependencies: &[String],
    ) -> Result<ExtractionConfig, Box<dyn std::error::Error>> {
        let mut body = Self::guidance_request_body(
            "extraction_config",
            framework_detection,
            Protocol::Http,
            AgentSchemas::extraction_config_schema(),
        );
        body["dependencies"] = serde_json::json!(dependencies);

        let response = self
            .agent_service
            .post_to_lambda("/framework-guidance", &body, "extraction_config")
            .await?;

        // Per-rule tolerant parsing: one malformed rule (a float index, a
        // mistyped field) must not throw away every valid rule in the
        // response. Only a response without a `rules` array fails outright.
        #[derive(Deserialize)]
        struct RawConfig {
            rules: Vec<serde_json::Value>,
        }
        let raw: RawConfig = serde_json::from_str(&response).map_err(|e| {
            format!(
                "Failed to parse extraction config: {}. Raw response: {}",
                e, response
            )
        })?;
        let total = raw.rules.len();
        let rules: Vec<crate::services::type_sidecar::ExtractionRule> = raw
            .rules
            .into_iter()
            .filter_map(|rule| match serde_json::from_value(rule.clone()) {
                Ok(parsed) => Some(parsed),
                Err(e) => {
                    debug!("Dropping malformed extraction rule ({}): {}", e, rule);
                    None
                }
            })
            .collect();
        if rules.len() < total {
            debug!(
                "Extraction config: kept {}/{} rules after validation",
                rules.len(),
                total
            );
        }

        Ok(ExtractionConfig { rules })
    }

    async fn fetch_patterns(
        &self,
        category: &str,
        framework_detection: &DetectionResult,
        protocol: Protocol,
    ) -> Result<Keyed<Vec<PatternExample>>, Box<dyn std::error::Error>> {
        let mut body = Self::guidance_request_body(
            "patterns",
            framework_detection,
            protocol,
            AgentSchemas::pattern_list_schema(),
        );
        body["category"] = serde_json::json!(category);

        let outcome = self
            .agent_service
            .post_to_lambda_keyed("/framework-guidance", &body, category)
            .await?;

        let parsed: FlatPatternResponse = serde_json::from_str(&outcome.text).map_err(|e| {
            format!(
                "Failed to parse {} patterns: {}. Raw response: {}",
                category, e, outcome.text
            )
        })?;

        Ok(Keyed {
            value: parsed.into_pattern_examples(),
            guidance_key: outcome.guidance_key,
        })
    }

    async fn fetch_general_guidance(
        &self,
        framework_detection: &DetectionResult,
        protocol: Protocol,
    ) -> Result<Keyed<GeneralGuidanceResponse>, Box<dyn std::error::Error>> {
        let body = Self::guidance_request_body(
            "general",
            framework_detection,
            protocol,
            AgentSchemas::general_guidance_schema(),
        );

        let outcome = self
            .agent_service
            .post_to_lambda_keyed("/framework-guidance", &body, "general")
            .await?;

        let parsed: GeneralGuidanceResponse = serde_json::from_str(&outcome.text).map_err(|e| {
            format!(
                "Failed to parse general guidance: {}. Raw response: {}",
                e, outcome.text
            )
        })?;

        Ok(Keyed {
            value: parsed,
            guidance_key: outcome.guidance_key,
        })
    }
}

/// A parsed guidance answer beside the id of the stored entry it came from.
/// The id never reaches a prompt: it exists so the analyze-file request can
/// name the guidance it embedded (carrick-cloud#871).
struct Keyed<T> {
    value: T,
    guidance_key: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pattern_example_serialization() {
        let pattern = PatternExample {
            pattern: "app.get('/test', handler)".to_string(),
            description: "Test endpoint".to_string(),
            framework: "someframework".to_string(),
        };

        let json = serde_json::to_string(&pattern).unwrap();
        assert!(json.contains("pattern"));
        assert!(json.contains("description"));
        assert!(json.contains("framework"));

        let deserialized: PatternExample = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.pattern, pattern.pattern);
        assert_eq!(deserialized.description, pattern.description);
        assert_eq!(deserialized.framework, pattern.framework);
    }

    /// The id names five specific answers. Two guidance sets that used the
    /// same five entries in different roles are not the same guidance, so they
    /// must not share an analysis cache entry.
    #[test]
    fn the_composed_guidance_id_depends_on_which_answer_is_which() {
        let a = compose_guidance_key([Some("1"), Some("2"), Some("3"), Some("4"), Some("5")]);
        let swapped = compose_guidance_key([Some("2"), Some("1"), Some("3"), Some("4"), Some("5")]);
        assert!(a.is_some());
        assert_ne!(a, swapped);
        assert_eq!(
            a,
            compose_guidance_key([Some("1"), Some("2"), Some("3"), Some("4"), Some("5")])
        );
        // Length-delimited, so a boundary shift is not a collision.
        assert_ne!(
            compose_guidance_key([Some("ab"), Some("c"), Some("d"), Some("e"), Some("f")]),
            compose_guidance_key([Some("a"), Some("bc"), Some("d"), Some("e"), Some("f")])
        );
    }

    /// A partial id would name guidance it does not fully describe, and the
    /// cloud would serve one variant's answer for another's prompt.
    #[test]
    fn one_missing_answer_leaves_no_guidance_id_at_all() {
        for i in 0..5 {
            let mut keys = [Some("k"); 5];
            keys[i] = None;
            assert_eq!(compose_guidance_key(keys), None, "slot {i}");
        }
    }

    /// `cached_guidance` rides in the index blob, so a blob written before the
    /// id existed has to keep loading — and a guidance with no id must not add
    /// a null field to the blobs we write.
    #[test]
    fn a_blob_written_before_the_guidance_id_still_loads() {
        let old_blob = r#"{
            "mount_patterns": [],
            "endpoint_patterns": [],
            "middleware_patterns": [],
            "data_fetching_patterns": [],
            "triage_hints": "hints",
            "parsing_notes": "notes"
        }"#;
        let parsed: FrameworkGuidance = serde_json::from_str(old_blob).unwrap();
        assert_eq!(parsed.guidance_key, None);
        assert_eq!(parsed.triage_hints, "hints");

        let json = serde_json::to_string(&parsed).unwrap();
        assert!(
            !json.contains("guidance_key"),
            "an absent id must not be written into the blob: {json}"
        );

        let keyed = FrameworkGuidance {
            guidance_key: Some("abc".to_string()),
            ..parsed
        };
        let json = serde_json::to_string(&keyed).unwrap();
        assert!(json.contains("\"guidance_key\":\"abc\""));
        let round_tripped: FrameworkGuidance = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped.guidance_key.as_deref(), Some("abc"));
    }

    #[test]
    fn test_framework_guidance_serialization() {
        let guidance = FrameworkGuidance {
            mount_patterns: vec![PatternExample {
                pattern: "test".to_string(),
                description: "test".to_string(),
                framework: "test".to_string(),
            }],
            endpoint_patterns: vec![],
            middleware_patterns: vec![],
            data_fetching_patterns: vec![],
            triage_hints: "some hints".to_string(),
            parsing_notes: "some notes".to_string(),
            guidance_key: None,
        };

        let json = serde_json::to_string(&guidance).unwrap();
        assert!(json.contains("mount_patterns"));
        assert!(json.contains("endpoint_patterns"));
        assert!(json.contains("triage_hints"));

        let deserialized: FrameworkGuidance = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.mount_patterns.len(),
            guidance.mount_patterns.len()
        );
    }

    // build_context_string tests removed: context-string formatting now
    // lives lambda-side (carrick-cloud/lambdas/framework-guidance/prompts.js
    // → buildContextString). Rust only forwards the frameworks +
    // data_fetchers arrays.
}
