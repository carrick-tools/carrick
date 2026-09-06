//! Multi-Agent Orchestrator using AST-Gated File-Centric Analysis.
//!
//! This orchestrator implements the new analysis workflow:
//! 1. Framework Detection - Identify frameworks and data fetchers
//! 2. Framework Guidance Generation - Get patterns for detected frameworks
//! 3. AST-Gated File Analysis - Use SWC Scanner as gatekeeper, then LLM for relevant files
//! 4. Mount Graph Construction - Build from file analysis results
//!
//! The AST-Gated approach:
//! - Skips files with no API patterns (zero LLM cost)
//! - Sends full file context for better alias resolution
//! - Passes AST-detected candidate hints for 100% recall

use crate::{
    agent_service::AgentService,
    agents::{
        file_analyzer_agent::FileAnalysisResult,
        file_orchestrator::{FileCentricAnalysisResult, FileOrchestrator, ProcessingStats},
        framework_guidance_agent::{FrameworkGuidanceAgent, ProtocolGuidance},
    },
    framework_detector::{DetectionResult, FrameworkDetector},
    mount_graph::MountGraph,
    packages::Packages,
    url_normalizer::UrlNormalizer,
    visitor::ImportedSymbol,
};
use std::collections::HashMap;
use swc_common::{SourceMap, sync::Lrc};
use tracing::debug;

/// Complete analysis result from the multi-agent workflow
#[derive(Debug)]
pub struct MultiAgentAnalysisResult {
    #[allow(dead_code)]
    pub framework_detection: DetectionResult,
    #[allow(dead_code)]
    pub framework_guidance: ProtocolGuidance,
    pub mount_graph: MountGraph,
    /// File-centric analysis results (replaces old AnalysisResults)
    pub file_results: HashMap<String, FileAnalysisResult>,
    /// The model's raw per-file answers, for the incremental cache. See
    /// [`FileCentricAnalysisResult::raw_model_results`].
    pub raw_model_results: HashMap<String, FileAnalysisResult>,
    /// Processing statistics
    #[allow(dead_code)]
    pub stats: ProcessingStats,
}

/// Orchestrates the complete multi-agent workflow using AST-Gated File-Centric analysis.
///
/// This orchestrator:
/// 1. Detects frameworks from package.json and imports
/// 2. Generates framework-specific patterns via LLM
/// 3. Uses SWC Scanner as gatekeeper to skip irrelevant files (zero cost)
/// 4. Sends relevant files to LLM with full context + candidate hints
/// 5. Builds MountGraph from aggregated results
pub struct MultiAgentOrchestrator {
    agent_service: AgentService,
    #[allow(dead_code)]
    source_map: Lrc<SourceMap>,
}

impl MultiAgentOrchestrator {
    pub fn new(source_map: Lrc<SourceMap>) -> Self {
        Self {
            agent_service: AgentService::new(),
            source_map,
        }
    }

    /// Run the complete multi-agent analysis workflow using AST-Gated File-Centric approach.
    ///
    /// ## Workflow:
    /// 1. **Framework Detection** - Identify frameworks from packages and imports
    /// 2. **Framework Guidance** - Generate patterns for detected frameworks
    /// 3. **AST-Gated Analysis** - For each file:
    ///    - Run SWC Scanner to find candidates
    ///    - If no candidates → SKIP (zero LLM cost)
    ///    - If candidates exist → Send full file + patterns + hints to LLM
    /// 4. **Graph Construction** - Build MountGraph from all file results
    #[allow(clippy::too_many_arguments)]
    pub async fn run_complete_analysis(
        &self,
        files: Vec<std::path::PathBuf>,
        packages: &Packages,
        imported_symbols: &HashMap<String, ImportedSymbol>,
        // Root for file-based route derivation: the SERVICE directory when
        // carrick.json declares one, else the repo root (see
        // `engine::service_scan_root`).
        service_root: &str,
        // The repository root, for the workspace package map (carrick#666).
        repo_root: &std::path::Path,
        graphql_producer_hints: &crate::graphql::GraphqlProducerHints,
        graphql_consumer_hints: &crate::graphql::GraphqlConsumerHints,
        normalizer: &UrlNormalizer,
        // The warm type sidecar for this service, when one is up: the
        // deterministic layer asks it what a bare `x.verb("/lit", arg)` site's
        // receiver is (carrick#695).
        sidecar: Option<&crate::services::type_sidecar::TypeSidecar>,
    ) -> Result<MultiAgentAnalysisResult, Box<dyn std::error::Error>> {
        debug!("Starting AST-Gated File-Centric analysis...");

        // Stage 0: Framework Detection
        //
        // Both model stages below are skipped in local mode (carrick#708):
        // detection and guidance are prompt inputs, and a run with no model to
        // prompt states an empty inventory rather than an invented one. The
        // deterministic layer reads the declared dependencies for the routing
        // conventions it needs, so the facts a file states outright survive
        // the skip.
        debug!("=== Stage 0: Framework Detection ===");
        let framework_detection = if crate::local_mode::no_model() {
            debug!("Local mode: skipping framework detection (no model)");
            DetectionResult::default()
        } else {
            let framework_detector = FrameworkDetector::new(self.agent_service.clone());
            framework_detector
                .detect_frameworks_and_libraries(packages, imported_symbols)
                .await?
        };

        debug!("Detected frameworks: {:?}", framework_detection.frameworks);
        debug!(
            "Detected data fetchers: {:?}",
            framework_detection.data_fetchers
        );

        // Stage 1: Framework Guidance Generation
        debug!("=== Stage 1: Framework Guidance Generation ===");
        let framework_guidance = if crate::local_mode::no_model() {
            debug!("Local mode: skipping framework guidance (no model)");
            crate::local_mode::offline_guidance()
        } else {
            let framework_guidance_agent = FrameworkGuidanceAgent::new(self.agent_service.clone());
            framework_guidance_agent
                .generate_for_active_protocols(&framework_detection)
                .await?
        };

        for (protocol, guidance) in &framework_guidance {
            debug!(
                "Generated {:?} guidance with {} mount patterns, {} endpoint patterns, {} data fetching patterns",
                protocol,
                guidance.mount_patterns.len(),
                guidance.endpoint_patterns.len(),
                guidance.data_fetching_patterns.len()
            );
        }

        // Stage 2: AST-Gated File-Centric Analysis
        debug!("=== Stage 2: AST-Gated File-Centric Analysis ===");
        let file_orchestrator = FileOrchestrator::new(self.agent_service.clone());
        let file_centric_result = file_orchestrator
            .analyze_files(
                &files,
                // A full analysis holds no previous answer: every file goes to
                // the model.
                &HashMap::new(),
                &framework_guidance,
                &framework_detection,
                std::path::Path::new(service_root),
                repo_root,
                &packages.declared_dependency_names(),
                graphql_producer_hints,
                graphql_consumer_hints,
                normalizer,
                sidecar,
            )
            .await?;

        self.print_analysis_summary(&file_centric_result);

        // Stage 3: Mount Graph is already built by FileOrchestrator
        debug!("=== Stage 3: Mount Graph Summary ===");
        let mount_graph = &file_centric_result.mount_graph;
        debug!("Built mount graph:");
        debug!("  - {} nodes", mount_graph.get_nodes().len());
        debug!("  - {} mounts", mount_graph.get_mounts().len());
        debug!(
            "  - {} endpoints",
            mount_graph.get_resolved_endpoints().len()
        );
        debug!("  - {} data calls", mount_graph.get_data_calls().len());

        Ok(MultiAgentAnalysisResult {
            framework_detection,
            framework_guidance,
            mount_graph: file_centric_result.mount_graph,
            file_results: file_centric_result.file_results,
            raw_model_results: file_centric_result.raw_model_results,
            stats: file_centric_result.stats,
        })
    }

    fn print_analysis_summary(&self, result: &FileCentricAnalysisResult) {
        debug!("=== ANALYSIS SUMMARY ===");
        debug!("File Processing:");
        debug!("  - Files processed: {}", result.stats.files_processed);
        debug!(
            "  - Files sent to the model: {}",
            result.stats.files_model_dispatched
        );
        debug!("  - Files skipped (total): {}", result.stats.files_skipped);
        debug!(
            "  - Zero-cost skips (no API patterns): {}",
            result.stats.files_skipped_no_candidates
        );

        debug!("Extracted Items:");
        debug!("  - Total mounts: {}", result.stats.total_mounts);
        debug!("  - Total endpoints: {}", result.stats.total_endpoints);
        debug!("  - Total data calls: {}", result.stats.total_data_calls);

        if !result.stats.errors.is_empty() {
            debug!("Errors ({}):", result.stats.errors.len());
            for error in &result.stats.errors {
                debug!("  - {}", error);
            }
        }
    }
}
