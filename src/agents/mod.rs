pub mod asked_ahead;
pub mod file_analyzer_agent;
pub mod file_orchestrator;
pub mod framework_guidance_agent;
pub mod schemas;
pub mod source_parts;

// File-centric analysis types (new architecture)
// These are re-exported for external use
#[allow(unused_imports)]
pub use file_analyzer_agent::FileAnalyzerAgent;
#[allow(unused_imports)]
pub use file_orchestrator::{FileCentricAnalysisResult, FileOrchestrator, ProcessingStats};
#[allow(unused_imports)]
pub use framework_guidance_agent::{FrameworkGuidance, FrameworkGuidanceAgent};
