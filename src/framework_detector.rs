use crate::{
    agent_service::AgentService,
    packages::Packages,
    visitor::{ImportedSymbol, SymbolKind},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
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

/// Input data for LLM-based framework detection.
///
/// This struct IS the `/framework-detect` request body — it is handed to
/// `post_to_lambda` and serialized there, so nothing re-assembles the body
/// alongside it.
///
/// Every field is an ordered collection on purpose. Two scans of an unchanged
/// checkout must produce byte-identical bytes here (carrick#954): the cloud
/// caches the answer by the hash of this body (carrick-cloud#770), the
/// guidance derived from the answer is embedded in every analyze-file prompt,
/// and a body that drifts therefore re-pays the whole scan for nothing. Never
/// build a field of this struct from a `HashMap`.
#[derive(Debug, Serialize)]
struct FrameworkDetectionInput {
    package_json: PackageJsonSummary,
    imports: Vec<String>,
}

/// Simplified package.json summary for LLM analysis. `BTreeMap` so the JSON
/// object keys come out in one order (see [`FrameworkDetectionInput`]).
#[derive(Debug, Serialize)]
struct PackageJsonSummary {
    dependencies: BTreeMap<String, String>,
    dev_dependencies: BTreeMap<String, String>,
}

/// Framework detector that combines package.json analysis with LLM classification
pub struct FrameworkDetector {
    agent_service: AgentService,
}

impl FrameworkDetector {
    pub fn new(agent_service: AgentService) -> Self {
        Self { agent_service }
    }

    /// Main detection function that combines package.json and import analysis.
    ///
    /// `imports` is the service's whole import sample: every distinct
    /// `(local_name, imported_name, source, kind)` fact its files state,
    /// deduplicated and ordered. It is deliberately NOT the local-name-keyed
    /// symbol map the rest of the pipeline carries — that map collapses two
    /// files importing different things under one local name down to whichever
    /// file was parsed last, so the sample it yields varies between runs of an
    /// unchanged checkout (carrick#954).
    pub async fn detect_frameworks_and_libraries(
        &self,
        packages: &Packages,
        imports: &BTreeSet<ImportedSymbol>,
    ) -> Result<DetectionResult, Box<dyn std::error::Error>> {
        let result = self
            .classify_with_llm(build_detection_input(packages, imports))
            .await?;

        Ok(result)
    }

    /// Use the carrick-cloud /framework-detect lambda to classify frameworks
    /// and data-fetching libraries. The Rust side just sends the structured
    /// input (package.json summary + imports list); the prompt body lives
    /// at carrick-cloud/lambdas/framework-detect/index.js.
    async fn classify_with_llm(
        &self,
        input: FrameworkDetectionInput,
    ) -> Result<DetectionResult, Box<dyn std::error::Error>> {
        // The input struct is posted as-is: no second assembly of the body,
        // so the bytes the cloud hashes are exactly the bytes
        // `build_detection_input` produced and the determinism test asserts.
        let response = self
            .agent_service
            .post_to_lambda("/framework-detect", &input, "framework-detect")
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

/// Build the `/framework-detect` request body from a service's manifests and
/// its import sample. Pure and total: the same inputs always produce the same
/// bytes, whatever order the files were parsed in (carrick#954).
fn build_detection_input(
    packages: &Packages,
    imports: &BTreeSet<ImportedSymbol>,
) -> FrameworkDetectionInput {
    FrameworkDetectionInput {
        package_json: extract_package_summary(packages),
        imports: extract_import_statements(imports),
    }
}

/// Extract relevant package.json information.
fn extract_package_summary(packages: &Packages) -> PackageJsonSummary {
    let mut all_dependencies = BTreeMap::new();
    let mut all_dev_dependencies = BTreeMap::new();

    for package_json in &packages.package_jsons {
        for (name, version) in &package_json.dependencies {
            all_dependencies.insert(name.clone(), version.clone());
        }

        for (name, version) in &package_json.dev_dependencies {
            all_dev_dependencies.insert(name.clone(), version.clone());
        }
    }

    PackageJsonSummary {
        dependencies: all_dependencies,
        dev_dependencies: all_dev_dependencies,
    }
}

/// Render the import sample as import statements for LLM analysis: one
/// statement per source, sources in lexicographic order, local names sorted
/// and deduplicated within a source.
///
/// All ordering is decided here, never inherited from the caller, and every
/// source the service imports from appears — including the ones whose only
/// local name is also used for a different module elsewhere in the service.
///
/// There is no cap on the list: if one is ever added it goes AFTER this
/// function's ordering, or the sample starts varying again (carrick#954).
fn extract_import_statements(imports: &BTreeSet<ImportedSymbol>) -> Vec<String> {
    let mut by_source: BTreeMap<&str, Vec<&ImportedSymbol>> = BTreeMap::new();
    for symbol in imports {
        by_source
            .entry(symbol.source.as_str())
            .or_default()
            .push(symbol);
    }

    let mut import_statements = Vec::new();
    for (source, symbols) in by_source {
        let locals_of = |kind: &SymbolKind| -> Vec<&str> {
            let mut names: Vec<&str> = symbols
                .iter()
                .filter(|s| s.kind == *kind)
                .map(|s| s.local_name.as_str())
                .collect();
            names.sort_unstable();
            names.dedup();
            names
        };

        // One form per source, defaults first: a module reached under several
        // forms across the service contributes the same single line however
        // its files are ordered.
        let default_imports = locals_of(&SymbolKind::Default);
        let named_imports = locals_of(&SymbolKind::Named);
        let namespace_imports = locals_of(&SymbolKind::Namespace);

        let statement = if let Some(name) = default_imports.first() {
            format!("import {} from '{}';", name, source)
        } else if !named_imports.is_empty() {
            format!(
                "import {{ {} }} from '{}';",
                named_imports.join(", "),
                source
            )
        } else if let Some(name) = namespace_imports.first() {
            format!("import * as {} from '{}';", name, source)
        } else {
            continue;
        };

        import_statements.push(statement);
    }

    import_statements
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visitor::ImportSymbolExtractor;
    use std::path::{Path, PathBuf};
    use swc_common::{
        SourceMap,
        errors::{ColorConfig, Handler},
        sync::Lrc,
    };
    use swc_ecma_visit::VisitWith;

    /// A service whose files import different modules under the same local
    /// name (`router`, `logger`, `helpers`), import the same module twice,
    /// rename on import, and reach one module as both a default and a named
    /// import. Every one of those is a way the sample used to depend on which
    /// file was parsed last.
    const FIXTURE_FILES: [(&str, &str); 3] = [
        (
            "a.ts",
            "import express from 'express';\n\
             import { router } from './api/router';\n\
             import { logger } from './log/app';\n\
             import * as helpers from './util/helpers';\n\
             import shared from './shared/client';\n\
             import { query } from './db';\n",
        ),
        (
            "b.ts",
            "import express from 'express';\n\
             import router from 'koa-router';\n\
             import { logger } from './log/worker';\n\
             import * as helpers from './other/helpers';\n",
        ),
        (
            "c.ts",
            "import { createClient as client } from 'redis';\n\
             import { shared } from './shared/client';\n\
             import { pool } from './db';\n",
        ),
    ];

    const MANIFEST: &str = r#"{
      "name": "sample-service",
      "dependencies": { "redis": "^4.6.0", "express": "^4.19.2", "koa-router": "^12.0.1" },
      "devDependencies": { "typescript": "^5.4.5", "vitest": "^1.6.0" }
    }"#;

    fn write_fixture(root: &Path) -> Vec<PathBuf> {
        std::fs::write(root.join("package.json"), MANIFEST).expect("manifest");
        FIXTURE_FILES
            .iter()
            .map(|(name, source)| {
                let path = root.join(name);
                std::fs::write(&path, source).expect("fixture file");
                path
            })
            .collect()
    }

    /// Extract import facts exactly as `engine::discover_files_and_symbols`
    /// does — the production parse path, one `ImportSymbolExtractor` per file,
    /// every symbol folded into the service-wide sample.
    fn import_facts(files: &[PathBuf]) -> BTreeSet<ImportedSymbol> {
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Auto, true, false, Some(cm.clone()));

        let mut facts = BTreeSet::new();
        for file in files {
            let module = crate::parser::parse_file(file, &cm, &handler).expect("fixture parses");
            let mut extractor = ImportSymbolExtractor::new();
            module.visit_with(&mut extractor);
            facts.extend(extractor.imported_symbols.into_values());
        }
        facts
    }

    /// The bytes `post_to_lambda` puts on the wire for this input.
    fn wire_body(input: &FrameworkDetectionInput) -> String {
        serde_json::to_string(input).expect("detection input serializes")
    }

    #[test]
    fn framework_detect_body_is_identical_across_walk_orders() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut files = write_fixture(dir.path());

        // Both arms load their own `Packages` and build their own maps, so a
        // body that depended on hash iteration order would differ here.
        let packages = Packages::new(vec![dir.path().join("package.json")]).expect("packages");
        let forward = wire_body(&build_detection_input(&packages, &import_facts(&files)));

        files.reverse();
        let packages = Packages::new(vec![dir.path().join("package.json")]).expect("packages");
        let reversed = wire_body(&build_detection_input(&packages, &import_facts(&files)));

        assert_eq!(
            forward, reversed,
            "the framework-detect body must not depend on the order files were parsed in"
        );
    }

    #[test]
    fn import_sample_keeps_every_source_when_local_names_collide() {
        let dir = tempfile::tempdir().expect("temp dir");
        let files = write_fixture(dir.path());
        let packages = Packages::new(vec![dir.path().join("package.json")]).expect("packages");

        let input = build_detection_input(&packages, &import_facts(&files));

        // Sources in lexicographic order; both halves of every local-name
        // collision present; `express` imported by two files stated once.
        assert_eq!(
            input.imports,
            vec![
                "import { router } from './api/router';",
                "import { pool, query } from './db';",
                "import { logger } from './log/app';",
                "import { logger } from './log/worker';",
                "import * as helpers from './other/helpers';",
                "import shared from './shared/client';",
                "import * as helpers from './util/helpers';",
                "import express from 'express';",
                "import router from 'koa-router';",
                "import { client } from 'redis';",
            ]
        );

        assert_eq!(
            input.package_json.dependencies.keys().collect::<Vec<_>>(),
            vec!["express", "koa-router", "redis"]
        );
    }
}
