use crate::url_normalizer::UrlNormalizer;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Represents a node in the mount graph (router or app)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GraphNode {
    pub name: String,
    pub node_type: NodeType,
    pub creation_site: Option<String>,
    pub file_location: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeType {
    Root,      // Never mounted by other nodes (top-level applications)
    Mountable, // Can be mounted by other nodes (routers, sub-apps)
    Unknown,   // Insufficient information to classify
}

/// Represents a mount relationship between nodes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountEdge {
    pub parent: String,
    pub child: String,
    pub path_prefix: String,
    pub middleware_stack: Vec<String>,
}

/// Represents an HTTP endpoint with its full computed path
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedEndpoint {
    pub method: String,
    pub path: String,
    pub full_path: String,
    pub handler: Option<String>,
    pub owner: String,
    pub file_location: String,
    pub middleware_chain: Vec<String>,
    /// Optional repo identifier for cross-repo matching
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_name: Option<String>,
    /// Optional service identifier (monorepo carrick.json serviceName), tagged
    /// during cross-repo merge so findings can name the owning service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// Where this endpoint's evidence comes from — a real route or a mock/test
    /// handler — classified structurally from the source path when the graph
    /// is built (`file_finder::endpoint_provenance`). `default` so graphs
    /// serialized before the field existed read as `Route`. Orthogonal to
    /// `evidence`: provenance describes WHERE the entry was written (route vs
    /// mock tree), evidence describes WHAT KIND of expression backs it (a
    /// definition vs a client call) — a mock call-site carries both tags.
    #[serde(default)]
    pub provenance: crate::operation::EndpointProvenance,
    /// What kind of source evidence backs this entry (#379). Endpoints are
    /// route definitions by construction, but extraction can fabricate one
    /// from a client call expression (the same site double-extracted as an
    /// endpoint AND a data call — see
    /// `FileOrchestrator::classify_endpoint_evidence`). A `CallSite` entry is
    /// not a producer: a consumer call matching it is classified as a
    /// shared-external-contract pair, not producer/consumer. Defaults to
    /// `RouteDefinition` so index data recorded before this field existed
    /// keeps its meaning.
    #[serde(default)]
    pub evidence: carrick_match::MatchEvidence,
}

/// Represents a data-fetching call with its target
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataFetchingCall {
    pub method: String,
    pub target_url: String,
    /// Canonical key path for this consumer call, computed once via
    /// `UrlNormalizer::consumer_call_path`. Every downstream consumer (the cloud
    /// projections, the type manifest, the type-request collector) keys on THIS
    /// field so the projection key and the manifest key are byte-identical.
    pub canonical_path: String,
    pub client: String,
    pub file_location: String,
    /// Semantic kind carried from extraction; `None` until the file-analyzer
    /// prompt emits it. Drives compat gating in a later stage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_kind: Option<crate::operation::CallKind>,
    /// Owning repo, tagged during cross-repo merge (`merge_from_repos`) so a
    /// matched consumer can be attributed to its repo for cross-repo edges.
    /// `None` until merge (single-repo graphs don't carry it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_name: Option<String>,
    /// Owning service (monorepo carrick.json serviceName), tagged during
    /// cross-repo merge exactly like `repo_name`. Carried so a matched
    /// consumer resolves to the same `service_name ?? repo_name` id the
    /// producer side uses — without it, monorepo match rows named the
    /// producer by service but the consumer by repo (#368).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// Hostname of an absolute-URL target, retained from normalisation.
    ///
    /// `UrlNormalizer` computes the host on its way to a comparable path and
    /// then discards it, so the only record of WHERE a call goes used to be the
    /// raw `target_url`. Retained verbatim and without judgement: the field
    /// says a host was written at this call site, not that the host is external
    /// (classification stays with `internalDomains`/`externalDomains` and is
    /// unchanged by this field existing).
    ///
    /// `None` for relative paths, env-var bases, and template bases — those
    /// have no literal hostname to retain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// 1-based line of the call site: the typed counterpart of the line packed
    /// into `file_location` as `"{file}:{line}"` text. `None` when extraction
    /// reported a line that is not a positive integer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// The complete mount and endpoint graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountGraph {
    pub nodes: HashMap<String, GraphNode>,
    pub mounts: Vec<MountEdge>,
    pub endpoints: Vec<ResolvedEndpoint>,
    pub data_calls: Vec<DataFetchingCall>,
}

impl MountGraph {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            mounts: Vec::new(),
            endpoints: Vec::new(),
            data_calls: Vec::new(),
        }
    }

    /// Compute the full path by walking up the mount graph.
    ///
    /// The live path resolution now runs in `FileOrchestrator::resolve_endpoint_paths`;
    /// this graph-walking implementation is retained for its mount-chain regression
    /// tests (nested / broken / disconnected router chains) and their coverage of
    /// `find_mount_for_child`'s alias handling.
    #[allow(dead_code)]
    pub fn compute_full_path(&self, owner: &str, path: &str) -> String {
        let mut full_path = path.to_string();
        let mut current_node = owner;
        let mut visited = HashSet::new();

        // Walk up the mount chain
        while let Some(mount) = self.find_mount_for_child(current_node) {
            if visited.contains(current_node) {
                break; // Prevent cycles
            }
            visited.insert(current_node);

            full_path = self.join_paths(&mount.path_prefix, &full_path);
            current_node = &mount.parent;

            // Stop if we reach a root node (top-level application)
            if let Some(node) = self.nodes.get(current_node)
                && matches!(node.node_type, NodeType::Root)
            {
                break;
            }
        }

        self.normalize_path(&full_path)
    }

    fn find_mount_for_child(&self, child: &str) -> Option<&MountEdge> {
        // First, try to find an exact match by child name
        if let Some(mount) = self.mounts.iter().find(|mount| mount.child == child) {
            return Some(mount);
        }

        // If no exact match, try to find a mount where the child node is an alias
        // of the current node. This handles cases where the same router is referred
        // to by different names:
        //   - routes/users.ts defines `const router = express.Router()`
        //   - server.ts imports it as `import userRouter from './routes/users'`
        //   - Mount is `app.use('/users', userRouter)` but endpoint owner is `router`
        //
        // Two nodes are considered aliases if they have the SAME file location
        // (same file AND same line/column). This distinguishes:
        //   - Aliases: `router` and `apiRouter` both at `api-router.ts:1:0` (same variable, different names)
        //   - Different routers: `router` at `server.ts:5:0` and `apiRouter` at `server.ts:27:0`
        let current_node_location = self.nodes.get(child).map(|n| &n.file_location);

        if let Some(location) = current_node_location {
            // Find all nodes that share this EXACT file location (they are aliases of the same router)
            // We use exact match (including line/column) to avoid treating different routers
            // in the same file as aliases
            let alias_names: Vec<&String> = self
                .nodes
                .iter()
                .filter(|(name, node)| *name != child && node.file_location == *location)
                .map(|(name, _)| name)
                .collect();

            // Try to find a mount where the child is one of these aliases
            for alias in alias_names {
                if let Some(mount) = self.mounts.iter().find(|mount| &mount.child == alias) {
                    return Some(mount);
                }
            }
        }

        None
    }

    fn join_paths(&self, prefix: &str, path: &str) -> String {
        let normalized_prefix = prefix.trim_end_matches('/');
        let normalized_path = path.trim_start_matches('/');

        if normalized_prefix.is_empty() {
            format!("/{}", normalized_path)
        } else if normalized_path.is_empty() {
            normalized_prefix.to_string()
        } else {
            // Idempotent guard (see FileOrchestrator::join_paths): don't re-apply a
            // prefix the path already carries — avoids `/api/v1/api/v1/status` when
            // a constructor-carried prefix is baked into the path and also emitted
            // as the mount prefix. Segment-boundary match. Framework-agnostic.
            let pfx = if normalized_prefix.starts_with('/') {
                normalized_prefix.to_string()
            } else {
                format!("/{}", normalized_prefix)
            };
            let full = format!("/{}", normalized_path);
            match full.strip_prefix(&pfx) {
                // Already prefixed (exact, or at a segment boundary) — don't double it.
                Some(rest) if rest.is_empty() || rest.starts_with('/') => full,
                _ => format!("{}/{}", normalized_prefix, normalized_path),
            }
        }
    }

    fn normalize_path(&self, path: &str) -> String {
        if path.is_empty() || path == "/" {
            "/".to_string()
        } else {
            // Remove duplicate slashes and ensure single leading slash
            let clean_path = path
                .split('/')
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
                .join("/");
            format!("/{}", clean_path)
        }
    }

    /// Get all endpoints with their resolved full paths
    pub fn get_resolved_endpoints(&self) -> &[ResolvedEndpoint] {
        &self.endpoints
    }

    /// Get all data-fetching calls
    pub fn get_data_calls(&self) -> &[DataFetchingCall] {
        &self.data_calls
    }

    /// Get mount relationships
    pub fn get_mounts(&self) -> &[MountEdge] {
        &self.mounts
    }

    /// Get all nodes in the graph
    pub fn get_nodes(&self) -> &HashMap<String, GraphNode> {
        &self.nodes
    }

    /// Find all endpoints that match a given path pattern
    ///
    /// This is the basic matching method that does not perform URL normalization.
    /// For cross-service matching with full URLs, use `find_matching_endpoints_normalized`.
    pub fn find_matching_endpoints(&self, path: &str, method: &str) -> Vec<&ResolvedEndpoint> {
        self.endpoints
            .iter()
            .filter(|endpoint| {
                endpoint.method.eq_ignore_ascii_case(method)
                    && self.paths_match(&endpoint.full_path, path)
            })
            .collect()
    }

    /// Find all endpoints that match a given URL pattern with URL normalization
    ///
    /// This method handles real-world URL patterns:
    /// - Full URLs: `https://user-service.internal/users/123` → matches `/users/:id`
    /// - Env var patterns: `ENV_VAR:SERVICE_URL:/users/123` → matches `/users/:id`
    /// - Template literals: `${API_URL}/users/${id}` → matches `/users/:id`
    /// - Query strings are stripped: `/users?page=1` → matches `/users`
    ///
    /// Returns `None` if the URL is identified as external (should be skipped).
    /// Returns `Some(vec)` with matching endpoints (may be empty if no match found).
    ///
    /// A call pairs with the MOST SPECIFIC producers available (#381): among
    /// all routing matches, only the candidates with maximal literal
    /// agreement are returned. A catch-all mount (`/api/**`, agreement 1)
    /// therefore never shadows a concrete route (`/api/v1/chat/new`,
    /// agreement 4) declared elsewhere in the org; it is returned only when
    /// nothing more specific matches. The relative selection is
    /// data-derived — no absolute specificity threshold.
    pub fn find_matching_endpoints_with_normalizer(
        &self,
        url: &str,
        method: &str,
        normalizer: &UrlNormalizer,
    ) -> Option<Vec<&ResolvedEndpoint>> {
        let normalized = normalizer.normalize(url);

        // Skip external calls and calls whose URL was a single opaque variable
        // we couldn't resolve (e.g. `${url}`) — there's no path to match, and
        // reporting them produces bogus "missing endpoint" noise.
        if normalized.is_external || normalized.is_unresolved {
            return None;
        }

        let scored: Vec<(&ResolvedEndpoint, u32)> = self
            .endpoints
            .iter()
            .filter(|endpoint| endpoint.method.eq_ignore_ascii_case(method))
            .filter_map(|endpoint| {
                carrick_match::match_agreement(&endpoint.full_path, &normalized.path)
                    .map(|agreement| (endpoint, agreement))
            })
            .collect();
        let best = scored.iter().map(|(_, a)| *a).max().unwrap_or(0);

        Some(
            scored
                .into_iter()
                .filter(|(_, agreement)| *agreement == best)
                .map(|(endpoint, _)| endpoint)
                .collect(),
        )
    }

    /// Exact-path lookup ignoring the HTTP method. Used after a
    /// method-filtered lookup came back empty, to distinguish "no producer
    /// declares this path" (missing endpoint) from "a producer declares
    /// exactly this path under a different verb" (method mismatch — a
    /// contract risk, not a connectivity gap).
    ///
    /// Unlike [`find_matching_endpoints_with_normalizer`] this does NOT
    /// wildcard params against concrete segments: `POST /users/:id` must not
    /// turn a missing `GET /users/list` into a "wrong verb" risk. Segments
    /// match literally or param-to-param only (param names are not identity).
    pub fn find_exact_path_matches_any_method(
        &self,
        url: &str,
        normalizer: &UrlNormalizer,
    ) -> Option<Vec<&ResolvedEndpoint>> {
        let normalized = normalizer.normalize(url);
        if normalized.is_external || normalized.is_unresolved {
            return None;
        }
        Some(
            self.endpoints
                .iter()
                .filter(|endpoint| {
                    Self::paths_equal_modulo_param_names(&endpoint.full_path, &normalized.path)
                })
                .collect(),
        )
    }

    /// Whether two paths name the same declared route once param NAMES are
    /// ignored: same segment count, and each segment pair is literally equal
    /// or a param on BOTH sides (`:id` vs `:param`). Trailing `?` optional
    /// markers are ignored for the comparison.
    fn paths_equal_modulo_param_names(a: &str, b: &str) -> bool {
        let a_segments: Vec<&str> = a.split('/').collect();
        let b_segments: Vec<&str> = b.split('/').collect();
        a_segments.len() == b_segments.len()
            && a_segments.iter().zip(&b_segments).all(|(a_seg, b_seg)| {
                let (a_seg, b_seg) = (a_seg.trim_end_matches('?'), b_seg.trim_end_matches('?'));
                a_seg == b_seg || (Self::is_param_segment(a_seg) && Self::is_param_segment(b_seg))
            })
    }

    // Path-matching semantics live in the `carrick-match` crate (the single
    // source of truth, also compiled to wasm for Node.js consumers). The
    // methods below are thin delegates kept for call-site ergonomics.

    fn paths_match(&self, endpoint_path: &str, call_path: &str) -> bool {
        carrick_match::paths_match(endpoint_path, call_path)
    }

    /// Route parameter placeholder check. See [`carrick_match::is_param_segment`];
    /// it is also the param definition `canonical_path_has_literal_segment`
    /// (#307) reuses, so the noise gate and the matcher agree on what a
    /// placeholder is.
    pub(crate) fn is_param_segment(seg: &str) -> bool {
        carrick_match::is_param_segment(seg)
    }

    /// Merge mount graphs from multiple repos for cross-repo analysis
    /// This is framework-agnostic - it merges the behavior-based analysis results
    pub fn merge_from_repos(all_repo_data: &[crate::cloud_storage::CloudRepoData]) -> Self {
        let mut merged = MountGraph::new();
        let mut seen_endpoints: HashSet<String> = HashSet::new();
        let mut seen_data_calls: HashSet<String> = HashSet::new();
        let mut seen_mounts: HashSet<String> = HashSet::new();

        for repo_data in all_repo_data {
            if let Some(ref mount_graph) = repo_data.mount_graph {
                // Merge nodes (deduplicate by name, prefer first occurrence)
                for (node_name, node) in &mount_graph.nodes {
                    merged
                        .nodes
                        .entry(node_name.clone())
                        .or_insert_with(|| node.clone());
                }

                // Merge endpoints, deduplicating by repo + service + method +
                // full_path. Keying on BOTH repo and service means neither two
                // monorepo services that share a route (e.g. a common `/health`),
                // nor two repos that happen to declare the same `serviceName`,
                // collapse into one endpoint and lose an orphan finding.
                for endpoint in &mount_graph.endpoints {
                    let key = format!(
                        "{}:{}:{}:{}",
                        repo_data.repo_name,
                        repo_data.service_name.as_deref().unwrap_or(""),
                        endpoint.method,
                        endpoint.full_path
                    );
                    if seen_endpoints.insert(key) {
                        let mut tagged_endpoint = endpoint.clone();
                        // Tag endpoint with its owning repo and (monorepo) service
                        // so cross-repo findings can name where it lives.
                        tagged_endpoint.repo_name = Some(repo_data.repo_name.clone());
                        tagged_endpoint.service_name = repo_data.service_name.clone();
                        merged.endpoints.push(tagged_endpoint);
                    }
                }

                // Merge data calls (deduplicate by method + target_url + file_location).
                // Tag each consumer with its owning repo AND (monorepo) service
                // (symmetric with the endpoint tagging above) so a matched call
                // resolves to the same `service_name ?? repo_name` id producers
                // carry when cross-repo edges are captured (#368).
                for call in &mount_graph.data_calls {
                    let key = format!("{}:{}:{}", call.method, call.target_url, call.file_location);
                    if seen_data_calls.insert(key) {
                        let mut tagged_call = call.clone();
                        tagged_call.repo_name = Some(repo_data.repo_name.clone());
                        tagged_call.service_name = repo_data.service_name.clone();
                        merged.data_calls.push(tagged_call);
                    }
                }

                // Merge mounts (deduplicate by parent + child + prefix)
                for mount in &mount_graph.mounts {
                    let key = format!("{}:{}:{}", mount.parent, mount.child, mount.path_prefix);
                    if seen_mounts.insert(key) {
                        merged.mounts.push(mount.clone());
                    }
                }
            }
        }

        merged
    }
}

impl Default for MountGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::Config;

    /// The retained fields are additive on the wire: a call with nothing to
    /// retain serialises to exactly the keys it always had, so a payload
    /// written by this scanner is byte-identical to one written before the
    /// fields existed, and an older payload still deserialises.
    #[test]
    fn retained_fields_are_absent_when_empty() {
        let call = DataFetchingCall {
            method: "GET".to_string(),
            target_url: "/api/orders".to_string(),
            canonical_path: "/api/orders".to_string(),
            client: "fetch(".to_string(),
            file_location: "src/orders.ts:4".to_string(),
            call_kind: None,
            repo_name: None,
            service_name: None,
            host: None,
            line: None,
        };
        assert_eq!(
            serde_json::to_value(&call).unwrap(),
            serde_json::json!({
                "method": "GET",
                "target_url": "/api/orders",
                "canonical_path": "/api/orders",
                "client": "fetch(",
                "file_location": "src/orders.ts:4"
            })
        );

        let older: DataFetchingCall = serde_json::from_str(
            r#"{"method":"GET","target_url":"/api/orders","canonical_path":"/api/orders",
                "client":"fetch(","file_location":"src/orders.ts:4"}"#,
        )
        .expect("a payload written before the fields existed still reads");
        assert_eq!(older.host, None);
        assert_eq!(older.line, None);

        let retained = DataFetchingCall {
            host: Some("api.vendor.test".to_string()),
            line: Some(4),
            ..call
        };
        let json = serde_json::to_value(&retained).unwrap();
        assert_eq!(json["host"], serde_json::json!("api.vendor.test"));
        assert_eq!(json["line"], serde_json::json!(4));
    }

    #[test]
    fn test_url_normalized_matching_full_url() {
        // Test that full URLs are normalized and matched against endpoint paths
        let mut graph = MountGraph::new();

        // Add an endpoint
        graph.endpoints.push(ResolvedEndpoint {
            method: "GET".to_string(),
            path: "/users/:id".to_string(),
            full_path: "/users/:id".to_string(),
            handler: Some("getUser".to_string()),
            owner: "userRouter".to_string(),
            file_location: "routes/users.js:10:1".to_string(),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
        });

        // Create config with internal domain
        let config = Config {
            internal_domains: ["user-service.internal".to_string()].into_iter().collect(),
            external_domains: ["api.stripe.com".to_string()].into_iter().collect(),
            ..Default::default()
        };

        // Test: Full internal URL should match
        let normalizer = UrlNormalizer::new(&config);
        let _result = graph.find_matching_endpoints_with_normalizer(
            "https://user-service.internal/users/123",
            "GET",
            &normalizer,
        );
    }

    /// Test for exact 3-level nesting structure from CI logs (repo-b)
    ///
    /// Structure:
    /// - app (Root) mounts apiRouter at /api
    /// - apiRouter (Mountable) mounts v1Router at /v1
    /// - v1Router (Mountable) owns POST /chat
    ///
    /// Expected: `compute_full_path("v1Router", "/chat")` returns `/api/v1/chat`
    #[test]
    fn test_nested_router_three_levels() {
        // Set up the graph manually to reproduce the exact structure from the logs
        let mut graph = MountGraph::new();

        // Add nodes
        graph.nodes.insert(
            "app".to_string(),
            GraphNode {
                name: "app".to_string(),
                node_type: NodeType::Root,
                creation_site: None,
                file_location: "repo-b_server.ts:1:0".to_string(),
            },
        );
        graph.nodes.insert(
            "apiRouter".to_string(),
            GraphNode {
                name: "apiRouter".to_string(),
                node_type: NodeType::Mountable,
                creation_site: None,
                file_location: "api-router.ts:1:0".to_string(),
            },
        );
        graph.nodes.insert(
            "v1Router".to_string(),
            GraphNode {
                name: "v1Router".to_string(),
                node_type: NodeType::Mountable,
                creation_site: None,
                file_location: "v1-router.ts:1:0".to_string(),
            },
        );

        // Add mount edges: app -> apiRouter at /api
        graph.mounts.push(MountEdge {
            parent: "app".to_string(),
            child: "apiRouter".to_string(),
            path_prefix: "/api".to_string(),
            middleware_stack: Vec::new(),
        });

        // Add mount edges: apiRouter -> v1Router at /v1
        graph.mounts.push(MountEdge {
            parent: "apiRouter".to_string(),
            child: "v1Router".to_string(),
            path_prefix: "/v1".to_string(),
            middleware_stack: Vec::new(),
        });

        // Test compute_full_path for the endpoint POST /chat owned by v1Router
        let full_path = graph.compute_full_path("v1Router", "/chat");

        println!("Computed full path: {}", full_path);
        println!("Nodes: {:?}", graph.nodes.keys().collect::<Vec<_>>());
        println!("Mounts: {:?}", graph.mounts);

        // The bug would cause this to return "/api/chat" (missing /v1)
        // or "/v1/chat" (missing /api)
        assert_eq!(
            full_path, "/api/v1/chat",
            "Three-level nested router path should include all mount prefixes: expected /api/v1/chat but got {}",
            full_path
        );
    }

    /// Test for broken mount chain where parent node names don't match across files.
    ///
    /// This reproduces the exact bug from CI logs where:
    /// - In server.ts: `app.use('/api', router)` - 'router' is imported from api-router.ts
    /// - In api-router.ts: `router.use('/v1', v1Router)` - 'router' is the local variable name
    ///
    /// The mount edges created are:
    /// - app -> router at /api (using imported name in server.ts)
    /// - router -> v1Router at /v1 (using local name in api-router.ts)
    ///
    /// BUG: If the import name resolution fails, 'router' in server.ts might not be
    /// recognized as the same as 'router' in api-router.ts, breaking the chain.
    #[test]
    fn test_nested_router_broken_chain_with_local_variable_names() {
        // This test simulates what happens when the parent of a mount uses a local
        // variable name that wasn't properly resolved to its imported name
        let mut graph = MountGraph::new();

        // Add nodes - note that "router" appears as a separate node from "apiRouter"
        graph.nodes.insert(
            "app".to_string(),
            GraphNode {
                name: "app".to_string(),
                node_type: NodeType::Root,
                creation_site: None,
                file_location: "repo-b_server.ts:1:0".to_string(),
            },
        );
        // This node represents "router" as imported in server.ts
        graph.nodes.insert(
            "router".to_string(),
            GraphNode {
                name: "router".to_string(),
                node_type: NodeType::Mountable,
                creation_site: None,
                file_location: "api-router.ts:1:0".to_string(),
            },
        );
        graph.nodes.insert(
            "v1Router".to_string(),
            GraphNode {
                name: "v1Router".to_string(),
                node_type: NodeType::Mountable,
                creation_site: None,
                file_location: "v1-router.ts:1:0".to_string(),
            },
        );

        // Mount edges as they would be created from the logs
        // app mounts router (the imported name) at /api
        graph.mounts.push(MountEdge {
            parent: "app".to_string(),
            child: "router".to_string(),
            path_prefix: "/api".to_string(),
            middleware_stack: Vec::new(),
        });

        // router (local name in api-router.ts) mounts v1Router at /v1
        graph.mounts.push(MountEdge {
            parent: "router".to_string(),
            child: "v1Router".to_string(),
            path_prefix: "/v1".to_string(),
            middleware_stack: Vec::new(),
        });

        // Test compute_full_path for the endpoint POST /chat owned by v1Router
        let full_path = graph.compute_full_path("v1Router", "/chat");

        println!("Broken chain test - Computed full path: {}", full_path);
        println!("Mounts: {:?}", graph.mounts);

        // When mounts are properly connected (router -> router), this should work
        assert_eq!(
            full_path, "/api/v1/chat",
            "Even with local variable names, path should resolve correctly when mount chain is connected"
        );
    }

    /// Test for the actual bug: when mount parent names are inconsistent
    /// This reproduces the ACTUAL bug where the second mount's parent doesn't match
    /// the first mount's child due to import name resolution failure
    #[test]
    fn test_nested_router_disconnected_chain_bug() {
        let mut graph = MountGraph::new();

        // Add nodes
        graph.nodes.insert(
            "app".to_string(),
            GraphNode {
                name: "app".to_string(),
                node_type: NodeType::Root,
                creation_site: None,
                file_location: "repo-b_server.ts:1:0".to_string(),
            },
        );
        // "apiRouter" is the imported name in server.ts
        graph.nodes.insert(
            "apiRouter".to_string(),
            GraphNode {
                name: "apiRouter".to_string(),
                node_type: NodeType::Mountable,
                creation_site: None,
                file_location: "api-router.ts:1:0".to_string(),
            },
        );
        // "router" is the local name in api-router.ts (DIFFERENT from "apiRouter")
        graph.nodes.insert(
            "router".to_string(),
            GraphNode {
                name: "router".to_string(),
                node_type: NodeType::Mountable,
                creation_site: None,
                file_location: "api-router.ts:1:0".to_string(),
            },
        );
        graph.nodes.insert(
            "v1Router".to_string(),
            GraphNode {
                name: "v1Router".to_string(),
                node_type: NodeType::Mountable,
                creation_site: None,
                file_location: "v1-router.ts:1:0".to_string(),
            },
        );

        // BUG REPRODUCTION: Mount edges with DISCONNECTED names
        // app mounts apiRouter (the imported name) at /api
        graph.mounts.push(MountEdge {
            parent: "app".to_string(),
            child: "apiRouter".to_string(), // <-- imported name
            path_prefix: "/api".to_string(),
            middleware_stack: Vec::new(),
        });

        // router (local name, NOT "apiRouter") mounts v1Router at /v1
        // This creates a BROKEN chain because "router" != "apiRouter"
        graph.mounts.push(MountEdge {
            parent: "router".to_string(), // <-- local name, doesn't match "apiRouter"!
            child: "v1Router".to_string(),
            path_prefix: "/v1".to_string(),
            middleware_stack: Vec::new(),
        });

        // Test compute_full_path for the endpoint POST /chat owned by v1Router
        let full_path = graph.compute_full_path("v1Router", "/chat");

        println!(
            "Disconnected chain bug test - Computed full path: {}",
            full_path
        );
        println!("Mounts: {:?}", graph.mounts);

        // BUG: This will return "/v1/chat" because the chain is broken at "router"
        // The traversal finds v1Router -> router at /v1, but then can't find
        // a mount where router is the child (because the mount has "apiRouter" as child)

        // For now, this test documents the current (buggy) behavior
        // After the fix, this should return "/api/v1/chat"

        // Current buggy behavior: returns /v1/chat (missing /api)
        // We assert what SHOULD happen after fix:
        assert_eq!(
            full_path, "/api/v1/chat",
            "Disconnected chain should still resolve to /api/v1/chat after fix. Got: {}",
            full_path
        );
    }

    #[test]
    fn test_url_normalized_matching_env_var_pattern() {
        let mut graph = MountGraph::new();

        graph.endpoints.push(ResolvedEndpoint {
            method: "POST".to_string(),
            path: "/orders".to_string(),
            full_path: "/orders".to_string(),
            handler: Some("createOrder".to_string()),
            owner: "orderRouter".to_string(),
            file_location: "routes/orders.js:15:1".to_string(),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
        });

        let config = Config {
            internal_env_vars: ["ORDER_SERVICE_URL".to_string()].into_iter().collect(),
            external_env_vars: ["STRIPE_API".to_string()].into_iter().collect(),
            ..Default::default()
        };

        // Test: Internal env var pattern should match
        let normalizer = UrlNormalizer::new(&config);
        let result = graph.find_matching_endpoints_with_normalizer(
            "ENV_VAR:ORDER_SERVICE_URL:/orders",
            "POST",
            &normalizer,
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 1);

        // Test: External env var pattern should return None
        let result = graph.find_matching_endpoints_with_normalizer(
            "ENV_VAR:STRIPE_API:/v1/charges",
            "POST",
            &normalizer,
        );
        assert!(result.is_none());
    }

    #[test]
    fn injected_base_consumer_key_matches_producer_endpoint() {
        // #467: an internal HTTP client whose base URL is dependency-injected
        // onto a class property (`this.apiUrl = opts.apiUrl`) is the ordinary
        // shape of a sibling-service client in a TS monorepo. Its canonical key
        // must reduce to the bare route so it joins the sibling's producer
        // endpoint; keeping `${this.apiUrl}` on the key made the join impossible.
        let mut graph = MountGraph::new();
        graph.endpoints.push(ResolvedEndpoint {
            method: "GET".to_string(),
            path: "/api/v1/widgets/:id".to_string(),
            full_path: "/api/v1/widgets/:id".to_string(),
            handler: Some("getWidget".to_string()),
            owner: "widgetRouter".to_string(),
            file_location: "services/widget-plane/src/routes.ts:12:1".to_string(),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
        });

        // No env vars declared: the injected base cannot be classified through
        // `internalEnvVars`, which is exactly the point.
        let normalizer = UrlNormalizer::new(&Config::default());
        let canonical = normalizer.consumer_call_path("${this.someBase}/api/v1/widgets/${id}");
        assert_eq!(canonical, "/api/v1/widgets/:id");
        assert!(UrlNormalizer::canonical_path_has_literal_segment(
            &canonical
        ));

        let result = graph.find_matching_endpoints_with_normalizer(&canonical, "GET", &normalizer);
        let matched = result.expect("injected-base consumer key must reach the matcher");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].full_path, "/api/v1/widgets/:id");
    }

    #[test]
    fn test_url_normalized_matching_template_literal() {
        let mut graph = MountGraph::new();

        graph.endpoints.push(ResolvedEndpoint {
            method: "GET".to_string(),
            path: "/users/:userId/orders/:orderId".to_string(),
            full_path: "/users/:userId/orders/:orderId".to_string(),
            handler: Some("getUserOrder".to_string()),
            owner: "orderRouter".to_string(),
            file_location: "routes/orders.js:20:1".to_string(),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
        });

        let config = Config {
            internal_env_vars: ["API_URL".to_string()].into_iter().collect(),
            ..Default::default()
        };

        // Test: Template literal should be normalized and matched
        let normalizer = UrlNormalizer::new(&config);
        let result = graph.find_matching_endpoints_with_normalizer(
            "${API_URL}/users/${userId}/orders/${orderId}",
            "GET",
            &normalizer,
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 1);
    }

    #[test]
    fn test_url_normalized_matching_query_string_stripped() {
        let mut graph = MountGraph::new();

        graph.endpoints.push(ResolvedEndpoint {
            method: "GET".to_string(),
            path: "/users".to_string(),
            full_path: "/users".to_string(),
            handler: Some("listUsers".to_string()),
            owner: "userRouter".to_string(),
            file_location: "routes/users.js:5:1".to_string(),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
        });

        let config = Config::default();

        // Test: Query string should be stripped before matching
        let normalizer = UrlNormalizer::new(&config);
        let result = graph.find_matching_endpoints_with_normalizer(
            "/users?page=1&limit=10",
            "GET",
            &normalizer,
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 1);
    }

    /// #381: the matcher pairs a call with the MOST specific producers only.
    /// A catch-all mount is returned when nothing better matches, and never
    /// alongside (or instead of) a concrete route for the same call.
    #[test]
    fn test_matching_returns_only_most_specific_producers() {
        let mut graph = MountGraph::new();
        for full_path in ["/api/**", "/api/v1/chat/new"] {
            graph.endpoints.push(ResolvedEndpoint {
                method: "GET".to_string(),
                path: full_path.to_string(),
                full_path: full_path.to_string(),
                handler: None,
                owner: "app".to_string(),
                file_location: "server.ts:1".to_string(),
                middleware_chain: vec![],
                repo_name: None,
                service_name: None,
                provenance: Default::default(),
                evidence: carrick_match::MatchEvidence::RouteDefinition,
            });
        }
        let normalizer = UrlNormalizer::default_permissive();

        // The concrete route out-ranks the catch-all for its own path.
        let result = graph
            .find_matching_endpoints_with_normalizer("/api/v1/chat/new", "GET", &normalizer)
            .expect("internal path");
        assert_eq!(
            result
                .iter()
                .map(|e| e.full_path.as_str())
                .collect::<Vec<_>>(),
            vec!["/api/v1/chat/new"],
            "the catch-all must not pair when a more specific producer matches"
        );

        // With no better candidate, the catch-all is still the router truth.
        let result = graph
            .find_matching_endpoints_with_normalizer("/api/v2/other", "GET", &normalizer)
            .expect("internal path");
        assert_eq!(
            result
                .iter()
                .map(|e| e.full_path.as_str())
                .collect::<Vec<_>>(),
            vec!["/api/**"]
        );
    }

    // The path-matching unit tests (optional segments, symmetric params, param
    // syntaxes, wildcards) moved to crates/carrick-match with the functions
    // they exercise. CI runs them via `cargo test -p carrick-match`.

    fn cloud_repo_with_health(
        repo: &str,
        service: Option<&str>,
    ) -> crate::cloud_storage::CloudRepoData {
        let mut mg = MountGraph::new();
        mg.endpoints.push(ResolvedEndpoint {
            method: "GET".to_string(),
            path: "/health".to_string(),
            full_path: "/health".to_string(),
            handler: None,
            owner: repo.to_string(),
            file_location: format!("{}/health.ts:1", repo),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
        });
        crate::cloud_storage::CloudRepoData {
            repo_name: repo.to_string(),
            service_name: service.map(|s| s.to_string()),
            endpoints: vec![],
            calls: vec![],
            mounts: vec![],
            apps: std::collections::HashMap::new(),
            imported_handlers: vec![],
            function_definitions: std::collections::HashMap::new(),
            config_json: None,
            package_json: None,
            packages: None,
            last_updated: chrono::Utc::now(),
            commit_hash: "test".to_string(),
            mount_graph: Some(mg),
            bundled_types: None,
            type_manifest: None,
            file_results: None,
            cached_detection: None,
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: None,
            cache_version: None,
            type_extraction_status: None,
            types_degraded: None,
            compat_verdicts: None,
            capture_stub: None,
            external_call_candidates: None,
            sdk_surface: None,
            sdk_edges: None,
            sdk_unresolved: None,
        }
    }

    #[test]
    fn test_merge_keeps_same_route_across_monorepo_services() {
        // Two services in the same repo both exposing GET /health must survive
        // the merge as distinct, service-tagged endpoints rather than collapse
        // into one (which would hide an orphan finding).
        let repos = vec![
            cloud_repo_with_health("platform", Some("auth")),
            cloud_repo_with_health("platform", Some("billing")),
        ];
        let merged = MountGraph::merge_from_repos(&repos);
        let health: Vec<_> = merged
            .endpoints
            .iter()
            .filter(|e| e.full_path == "/health")
            .collect();
        assert_eq!(health.len(), 2, "both services' /health must be kept");
        let services: std::collections::HashSet<_> = health
            .iter()
            .filter_map(|e| e.service_name.as_deref())
            .collect();
        assert!(services.contains("auth"));
        assert!(services.contains("billing"));
    }

    #[test]
    fn test_merge_keeps_same_route_across_repos_with_shared_service_name() {
        // Two distinct repos that happen to declare the same serviceName must
        // not collapse — repo identity is part of the dedup key.
        let repos = vec![
            cloud_repo_with_health("repo-a", Some("api")),
            cloud_repo_with_health("repo-b", Some("api")),
        ];
        let merged = MountGraph::merge_from_repos(&repos);
        let health = merged
            .endpoints
            .iter()
            .filter(|e| e.full_path == "/health")
            .count();
        assert_eq!(health, 2, "endpoints from different repos must be kept");
    }

    #[test]
    fn test_merge_tags_data_calls_with_repo_and_service() {
        // Consumer calls must be tagged with BOTH repo and service, so the
        // matcher resolves them to the same `service_name ?? repo_name` id
        // producers carry (#368) — repo-only tagging left match rows naming
        // the producer by service but the consumer by repo.
        let mut repo = cloud_repo_with_health("platform", Some("web"));
        repo.mount_graph
            .as_mut()
            .unwrap()
            .data_calls
            .push(DataFetchingCall {
                method: "GET".to_string(),
                target_url: "/api/orders".to_string(),
                canonical_path: "/api/orders".to_string(),
                client: "fetch".to_string(),
                file_location: "apps/web/src/orders.ts:4".to_string(),
                call_kind: None,
                repo_name: None,
                service_name: None,
                host: None,
                line: None,
            });
        let merged = MountGraph::merge_from_repos(&[repo]);
        assert_eq!(merged.data_calls.len(), 1);
        assert_eq!(merged.data_calls[0].repo_name.as_deref(), Some("platform"));
        assert_eq!(merged.data_calls[0].service_name.as_deref(), Some("web"));
    }
}
