use crate::analyzer::Analyzer;
use crate::cloud_storage::CloudRepoData;
use crate::config::Config;
use swc_common::{SourceMap, sync::Lrc};

pub struct AnalyzerBuilder {
    config: Config,
    cm: Lrc<SourceMap>,
    skip_type_resolution: bool,
}

impl AnalyzerBuilder {
    pub fn new_for_cross_repo(config: Config, cm: Lrc<SourceMap>) -> Self {
        Self {
            config,
            cm,
            skip_type_resolution: true,
        }
    }

    /// Build analyzer from CloudRepoData (used by build_cross_repo_analyzer)
    pub async fn build_from_repo_data(
        &self,
        all_repo_data: Vec<CloudRepoData>,
    ) -> Result<Analyzer, Box<dyn std::error::Error>> {
        let mut analyzer = Analyzer::new(self.config.clone());

        // Populate analyzer with data from all repos
        for repo_data in all_repo_data {
            // Stamp repo identity onto every endpoint/call so the non-HTTP
            // matcher (`analyze_exact_key_matches`) can attribute a matched
            // GraphQL/socket producer↔consumer pair to its repos. HTTP ops also
            // resolve identity from the repo-tagged mount graph, so stamping is
            // redundant-but-harmless there; the non-HTTP path has no other source.
            let repo_name = repo_data.repo_name.clone();
            let service_name = repo_data.service_name.clone();
            analyzer
                .endpoints
                .extend(repo_data.endpoints.into_iter().map(|mut e| {
                    e.repo_name = Some(repo_name.clone());
                    e.service_name = service_name.clone();
                    e
                }));
            analyzer
                .calls
                .extend(repo_data.calls.into_iter().map(|mut c| {
                    c.repo_name = Some(repo_name.clone());
                    c.service_name = service_name.clone();
                    c
                }));
            analyzer.mounts.extend(repo_data.mounts);
            analyzer.apps.extend(repo_data.apps);
            analyzer
                .imported_handlers
                .extend(repo_data.imported_handlers);
            // Keys are unique within a repo (`call_graph::merge_definitions`
            // re-keys same-named definitions per file, #582) but not across
            // repos, so a `foo` defined in two repos still collapses here.
            // This map is never uploaded and feeds only handler-field
            // resolution, so the cross-repo case is left as it is rather than
            // given a second key shape.
            analyzer
                .function_definitions
                .extend(repo_data.function_definitions);
        }

        self.finalize_analyzer(analyzer).await
    }

    /// Common analyzer finalization steps (eliminates duplication)
    async fn finalize_analyzer(
        &self,
        mut analyzer: Analyzer,
    ) -> Result<Analyzer, Box<dyn std::error::Error>> {
        // Skip path resolution in cross-repo mode - paths are already resolved
        if !self.skip_type_resolution {
            // Resolve endpoint paths and types
            let endpoints = analyzer.resolve_all_endpoint_paths(
                &analyzer.endpoints.clone(),
                &analyzer.mounts.clone(),
                &analyzer.apps.clone(),
            );
            analyzer.endpoints = endpoints;
        }

        // Build the router
        analyzer.build_endpoint_router();

        // Resolve imported handler route fields
        let (response_fields, request_fields) = analyzer.resolve_imported_handler_route_fields(
            &analyzer.imported_handlers.clone(),
            &analyzer.function_definitions.clone(),
        );

        // Update endpoints and resolve types (skip type resolution in cross-repo mode)
        analyzer.update_endpoints_with_resolved_fields(response_fields, request_fields);

        if !self.skip_type_resolution {
            analyzer.resolve_types_for_endpoints(self.cm.clone());
        }

        Ok(analyzer)
    }
}
