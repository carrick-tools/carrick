use crate::{agent_service::AgentService, packages::Packages, visitor::ImportedSymbol};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, trace};

/// Result of framework and library detection
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DetectionResult {
    pub frameworks: Vec<String>,
    pub data_fetchers: Vec<String>,
    /// Packages used as pub/sub / message-queue clients (Kafka/NATS/Redis-pubsub/...),
    /// enumerated by the framework-detect LLM. Populated only after the carrick-cloud
    /// /framework-detect prompt+schema deploy; empty until then (see
    /// corpus-2-pubsub-DEPLOY-SPEC-messaging-clients.md). Used to force-analyze pub/sub-only
    /// files that produce no SWC candidates.
    #[serde(default)]
    pub messaging_clients: Vec<String>,
    pub notes: String,
}

/// Input data for LLM-based framework detection
#[derive(Debug, Serialize)]
struct FrameworkDetectionInput {
    package_json: PackageJsonSummary,
    imports: Vec<String>,
}

/// Simplified package.json summary for LLM analysis
#[derive(Debug, Serialize)]
struct PackageJsonSummary {
    dependencies: HashMap<String, String>,
    dev_dependencies: HashMap<String, String>,
}

/// Framework detector that combines package.json analysis with LLM classification
pub struct FrameworkDetector {
    agent_service: AgentService,
}

impl FrameworkDetector {
    pub fn new(agent_service: AgentService) -> Self {
        Self { agent_service }
    }

    /// Main detection function that combines package.json and import analysis
    pub async fn detect_frameworks_and_libraries(
        &self,
        packages: &Packages,
        imported_symbols: &HashMap<String, ImportedSymbol>,
    ) -> Result<DetectionResult, Box<dyn std::error::Error>> {
        // Extract package.json data
        let package_summary = self.extract_package_summary(packages);

        // Extract import statements
        let import_statements = self.extract_import_statements(imported_symbols);

        // Prepare input for LLM
        let input = FrameworkDetectionInput {
            package_json: package_summary,
            imports: import_statements,
        };

        // Call LLM for classification
        let result = self.classify_with_llm(input).await?;

        Ok(result)
    }

    /// Extract relevant package.json information
    fn extract_package_summary(&self, packages: &Packages) -> PackageJsonSummary {
        let mut all_dependencies = HashMap::new();
        let mut all_dev_dependencies = HashMap::new();

        for package_json in &packages.package_jsons {
            // Merge dependencies
            for (name, version) in &package_json.dependencies {
                all_dependencies.insert(name.clone(), version.clone());
            }

            // Merge dev dependencies
            for (name, version) in &package_json.dev_dependencies {
                all_dev_dependencies.insert(name.clone(), version.clone());
            }
        }

        PackageJsonSummary {
            dependencies: all_dependencies,
            dev_dependencies: all_dev_dependencies,
        }
    }

    /// Convert imported symbols to import statement strings for LLM analysis
    fn extract_import_statements(
        &self,
        imported_symbols: &HashMap<String, ImportedSymbol>,
    ) -> Vec<String> {
        let mut import_statements = Vec::new();
        let mut source_to_symbols: HashMap<String, Vec<&ImportedSymbol>> = HashMap::new();

        // Group symbols by source
        for symbol in imported_symbols.values() {
            source_to_symbols
                .entry(symbol.source.clone())
                .or_default()
                .push(symbol);
        }

        // Convert to import statement strings
        for (source, symbols) in source_to_symbols {
            let mut statement = String::new();

            let default_imports: Vec<_> = symbols
                .iter()
                .filter(|s| matches!(s.kind, crate::visitor::SymbolKind::Default))
                .collect();

            let named_imports: Vec<_> = symbols
                .iter()
                .filter(|s| matches!(s.kind, crate::visitor::SymbolKind::Named))
                .collect();

            let namespace_imports: Vec<_> = symbols
                .iter()
                .filter(|s| matches!(s.kind, crate::visitor::SymbolKind::Namespace))
                .collect();

            if !default_imports.is_empty() {
                statement.push_str(&format!(
                    "import {} from '{}';",
                    default_imports[0].local_name, source
                ));
            } else if !named_imports.is_empty() {
                let named_list: Vec<_> = named_imports
                    .iter()
                    .map(|s| s.local_name.as_str())
                    .collect();
                statement.push_str(&format!(
                    "import {{ {} }} from '{}';",
                    named_list.join(", "),
                    source
                ));
            } else if !namespace_imports.is_empty() {
                statement.push_str(&format!(
                    "import * as {} from '{}';",
                    namespace_imports[0].local_name, source
                ));
            }

            if !statement.is_empty() {
                import_statements.push(statement);
            }
        }

        import_statements
    }

    /// Use the carrick-cloud /framework-detect lambda to classify frameworks
    /// and data-fetching libraries. The Rust side just sends the structured
    /// input (package.json summary + imports list); the prompt body lives
    /// at carrick-cloud/lambdas/framework-detect/index.js.
    async fn classify_with_llm(
        &self,
        input: FrameworkDetectionInput,
    ) -> Result<DetectionResult, Box<dyn std::error::Error>> {
        let body = serde_json::json!({
            "package_json": input.package_json,
            "imports": input.imports,
        });

        let response = self
            .agent_service
            .post_to_lambda("/framework-detect", &body, "framework-detect")
            .await?;

        // Full response bodies go to trace: debug logs are persisted to
        // ~/.carrick/logs and uploaded, and the body can quote source from
        // the scanned repo (#61).
        trace!("Framework Detection LLM Response:");
        trace!("{}", response);
        trace!("--- End of Response ---");
        debug!("Framework detection response: {} chars", response.len());

        // Lambda returns Gemini's raw text — same JSON-extraction step.
        let json_str = self.extract_json_from_response(&response)?;

        let detection_result: DetectionResult = serde_json::from_str(&json_str).map_err(|e| {
            format!(
                "Failed to parse LLM response as JSON: {}. Response was: {}",
                e, json_str
            )
        })?;

        Ok(detection_result)
    }

    /// Extract JSON from LLM response that may contain extra text
    fn extract_json_from_response(
        &self,
        response: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let response = response.trim();

        // If response is pure JSON, return it
        if response.starts_with('{') && response.ends_with('}') {
            return Ok(response.to_string());
        }

        // Find JSON object boundaries
        let mut brace_count = 0;
        let mut start_idx = None;
        let mut end_idx = None;

        for (i, ch) in response.char_indices() {
            match ch {
                '{' => {
                    if start_idx.is_none() {
                        start_idx = Some(i);
                    }
                    brace_count += 1;
                }
                '}' => {
                    brace_count -= 1;
                    if brace_count == 0 && start_idx.is_some() {
                        end_idx = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }

        if let (Some(start), Some(end)) = (start_idx, end_idx) {
            Ok(response[start..=end].to_string())
        } else {
            // Fallback: try to find JSON-like patterns
            if let Some(start) = response.find('{') {
                if let Some(end) = response.rfind('}') {
                    Ok(response[start..=end].to_string())
                } else {
                    Err("Could not find valid JSON in LLM response".into())
                }
            } else {
                Err("No JSON object found in LLM response".into())
            }
        }
    }
}
