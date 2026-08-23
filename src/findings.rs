//! Typed findings — the scanner's single model for everything a scan
//! surfaces, serialized verbatim as the `post-pr-result` wire payload
//! (schema_version 1, carrick-cloud docs/internal/pr-result-pipeline.md)
//! and rendered locally by the formatter. The types here are the schema
//! source of truth: the cloud renders the PR comment/check run from this
//! JSON and never re-parses prose.

use crate::operation::EndpointProvenance;
use serde::{Serialize, Serializer, ser::SerializeMap};

/// Wire cap on `call_sites` entries per finding. Applied when serializing
/// only — the in-memory finding keeps the full set so the terminal report
/// shows true counts.
pub const MAX_CALL_SITES: usize = 5;
/// Wire cap on `detail` length in *chars* (applied at construction).
pub const MAX_DETAIL_CHARS: usize = 400;

/// Truncate `s` to at most `max` chars, appending `"..."` when cut. Operates
/// on char boundaries — never byte-slices — replacing the old `&s[..150]`
/// truncation that panicked mid-UTF-8 sequence. The result never exceeds
/// `max` chars: when `max <= 3` leaves no room for the ellipsis the string
/// is hard-cut instead, so the ≤ max contract holds for every input.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max <= 3 {
        return s.chars().take(max).collect();
    }
    let mut out: String = s.chars().take(max - 3).collect();
    out.push_str("...");
    out
}

/// The wire view of a `call_sites` list: at most [`MAX_CALL_SITES`] entries.
fn wire_call_sites(call_sites: &[String]) -> &[String] {
    &call_sites[..call_sites.len().min(MAX_CALL_SITES)]
}

/// Fixed per finding kind (see [`Finding::severity`]); sent explicitly on the
/// wire so the cloud never re-derives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Risk,
    Gap,
    Advisory,
}

/// One repo's pinned version of a conflicting package.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PackageVersionRef {
    pub repo: String,
    pub version: String,
    pub source: String,
}

/// Dependency-conflict tier: `"major"` (semver-incompatible major spread,
/// counts as a gap) or `"unparseable"` (non-semver pins that differ as raw
/// strings, advisory).
pub mod tier {
    pub const MAJOR: &str = "major";
    pub const UNPARSEABLE: &str = "unparseable";
}

/// A single scan finding. Serializes as a `kind`-tagged union with an
/// explicit `severity` field (see the custom [`Serialize`] impl below).
/// `method`/`path` are protocol-agnostic display labels (HTTP verb + route,
/// GraphQL kind + field, socket direction + event); `call_sites` entries are
/// `file:line` (or bare `file`), capped at [`MAX_CALL_SITES`] on the wire.
#[derive(Clone, Debug, PartialEq)]
pub enum Finding {
    /// Producer and consumer types for one endpoint are incompatible.
    TypeMismatch {
        method: String,
        path: String,
        service: Option<String>,
        call_sites: Vec<String>,
        producer_type: String,
        consumer_type: String,
        /// Compiler error, pre-truncated to [`MAX_DETAIL_CHARS`] chars.
        detail: String,
        /// Whether the producer shape comes from a real route or a mock/test
        /// handler (#380) — a mismatch against a mock is often still real
        /// (mocks frequently encode the canonical contract) but should be
        /// presented with that caveat.
        producer_provenance: EndpointProvenance,
    },
    /// A consumer call matched a producer path but not its method. `method`
    /// is the consumer's attempt; `expected_method` is the producer's.
    MethodMismatch {
        method: String,
        path: String,
        service: Option<String>,
        call_sites: Vec<String>,
        expected_method: String,
    },
    /// A consumer call with no producer in the index.
    MissingEndpoint {
        method: String,
        path: String,
        service: Option<String>,
        call_sites: Vec<String>,
    },
    /// A producer endpoint with no consumer in the index.
    OrphanedEndpoint {
        method: String,
        path: String,
        service: Option<String>,
        /// Whether this producer is a real route or a mock/test handler
        /// (#380). Orphaned mocks are expected (most mock handlers have no
        /// scanned consumer), so surfaces can de-noise on this.
        provenance: EndpointProvenance,
    },
    /// A call whose URL is built from an env var not classified in
    /// carrick.json (`internalEnvVars` / `externalEnvVars`).
    EnvVarCall {
        method: String,
        path: String,
        env_var: String,
        call_sites: Vec<String>,
    },
    /// One package pinned to conflicting versions across repos. No
    /// method/path — this finding is not endpoint-scoped.
    DependencyConflict {
        package_name: String,
        /// [`tier::MAJOR`] or [`tier::UNPARSEABLE`].
        tier: String,
        versions: Vec<PackageVersionRef>,
    },
    /// Multiple repos encode the same external contract (#379): every side of
    /// the match is a call site, so no indexed service defines the route and
    /// producer/consumer roles do not apply. Advisory signal — "these N repos
    /// speak the same external API" — never a contract verdict.
    SharedExternalContract {
        method: String,
        path: String,
        /// Repo ids (service_name ?? repo_name) that encode this contract,
        /// sorted; always ≥2 (single-repo groups are not emitted).
        repos: Vec<String>,
        call_sites: Vec<String>,
    },
    /// Type extraction failed for one service, so every contract in it is
    /// unverified for this run (carrick#535). Not endpoint-scoped: it says
    /// the type layer is missing, which silently turns every type check into
    /// "not compared". A risk, because a green comment that omits it reads as
    /// "no type problems found" when the truth is "types were never read".
    DegradedTypes {
        /// Service this happened in (`service_name ?? repo_name`).
        service: String,
        /// Which stage lost the types: `spawn`, `init`, `resolve`, `capture`.
        stage: String,
        /// What went wrong, pre-truncated to [`MAX_DETAIL_CHARS`] chars.
        detail: String,
    },
}

impl Finding {
    pub fn type_mismatch(
        method: impl Into<String>,
        path: impl Into<String>,
        service: Option<String>,
        call_sites: Vec<String>,
        producer_type: impl Into<String>,
        consumer_type: impl Into<String>,
        detail: &str,
    ) -> Self {
        Finding::TypeMismatch {
            method: method.into(),
            path: path.into(),
            service,
            call_sites,
            producer_type: producer_type.into(),
            consumer_type: consumer_type.into(),
            detail: truncate_chars(detail, MAX_DETAIL_CHARS),
            producer_provenance: EndpointProvenance::default(),
        }
    }

    pub fn method_mismatch(
        method: impl Into<String>,
        path: impl Into<String>,
        service: Option<String>,
        call_sites: Vec<String>,
        expected_method: impl Into<String>,
    ) -> Self {
        Finding::MethodMismatch {
            method: method.into(),
            path: path.into(),
            service,
            call_sites,
            expected_method: expected_method.into(),
        }
    }

    pub fn missing_endpoint(
        method: impl Into<String>,
        path: impl Into<String>,
        service: Option<String>,
        call_sites: Vec<String>,
    ) -> Self {
        Finding::MissingEndpoint {
            method: method.into(),
            path: path.into(),
            service,
            call_sites,
        }
    }

    pub fn orphaned_endpoint(
        method: impl Into<String>,
        path: impl Into<String>,
        service: Option<String>,
    ) -> Self {
        Finding::OrphanedEndpoint {
            method: method.into(),
            path: path.into(),
            service,
            provenance: EndpointProvenance::default(),
        }
    }

    /// Attach producer-side provenance (real route vs mock/test handler) to a
    /// finding that has a producer side. Constructors default to `Route`, so
    /// existing call sites stay unchanged; the matchers chain this where the
    /// producer's classification is known. No-op for kinds without a
    /// producer-side provenance field.
    pub fn with_producer_provenance(mut self, producer: EndpointProvenance) -> Self {
        match &mut self {
            Finding::TypeMismatch {
                producer_provenance,
                ..
            } => *producer_provenance = producer,
            Finding::OrphanedEndpoint { provenance, .. } => *provenance = producer,
            _ => {}
        }
        self
    }

    pub fn env_var_call(
        method: impl Into<String>,
        path: impl Into<String>,
        env_var: impl Into<String>,
        call_sites: Vec<String>,
    ) -> Self {
        Finding::EnvVarCall {
            method: method.into(),
            path: path.into(),
            env_var: env_var.into(),
            call_sites,
        }
    }

    pub fn dependency_conflict(
        package_name: impl Into<String>,
        tier: impl Into<String>,
        versions: Vec<PackageVersionRef>,
    ) -> Self {
        Finding::DependencyConflict {
            package_name: package_name.into(),
            tier: tier.into(),
            versions,
        }
    }

    pub fn shared_external_contract(
        method: impl Into<String>,
        path: impl Into<String>,
        repos: Vec<String>,
        call_sites: Vec<String>,
    ) -> Self {
        Finding::SharedExternalContract {
            method: method.into(),
            path: path.into(),
            repos,
            call_sites,
        }
    }

    /// Type extraction failed for `service` at `stage`.
    pub fn degraded_types(
        service: impl Into<String>,
        stage: impl Into<String>,
        detail: &str,
    ) -> Self {
        Finding::DegradedTypes {
            service: service.into(),
            stage: stage.into(),
            detail: truncate_chars(detail, MAX_DETAIL_CHARS),
        }
    }

    /// The wire `kind` tag.
    pub fn kind(&self) -> &'static str {
        match self {
            Finding::TypeMismatch { .. } => "type_mismatch",
            Finding::MethodMismatch { .. } => "method_mismatch",
            Finding::MissingEndpoint { .. } => "missing_endpoint",
            Finding::OrphanedEndpoint { .. } => "orphaned_endpoint",
            Finding::EnvVarCall { .. } => "env_var_call",
            Finding::DependencyConflict { .. } => "dependency_conflict",
            Finding::SharedExternalContract { .. } => "shared_external_contract",
            Finding::DegradedTypes { .. } => "degraded_types",
        }
    }

    /// Severity is a function of the kind (plus tier for dependency
    /// conflicts), per the wire contract's table — never stored, so it can't
    /// drift from the kind.
    pub fn severity(&self) -> Severity {
        match self {
            Finding::TypeMismatch { .. }
            | Finding::MethodMismatch { .. }
            | Finding::DegradedTypes { .. } => Severity::Risk,
            Finding::MissingEndpoint { .. } | Finding::OrphanedEndpoint { .. } => Severity::Gap,
            Finding::EnvVarCall { .. } | Finding::SharedExternalContract { .. } => {
                Severity::Advisory
            }
            Finding::DependencyConflict { tier, .. } => {
                if tier == tier::MAJOR {
                    Severity::Gap
                } else {
                    Severity::Advisory
                }
            }
        }
    }
}

/// Hand-rolled so `severity` is emitted alongside the `kind` tag without
/// being a stored field (a derived internally-tagged enum can't add a
/// computed sibling field).
impl Serialize for Finding {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("kind", self.kind())?;
        map.serialize_entry("severity", &self.severity())?;
        match self {
            Finding::TypeMismatch {
                method,
                path,
                service,
                call_sites,
                producer_type,
                consumer_type,
                detail,
                producer_provenance,
            } => {
                map.serialize_entry("method", method)?;
                map.serialize_entry("path", path)?;
                map.serialize_entry("service", service)?;
                map.serialize_entry("call_sites", wire_call_sites(call_sites))?;
                map.serialize_entry("producer_type", producer_type)?;
                map.serialize_entry("consumer_type", consumer_type)?;
                map.serialize_entry("detail", detail)?;
                map.serialize_entry("producer_provenance", producer_provenance)?;
            }
            Finding::MethodMismatch {
                method,
                path,
                service,
                call_sites,
                expected_method,
            } => {
                map.serialize_entry("method", method)?;
                map.serialize_entry("path", path)?;
                map.serialize_entry("service", service)?;
                map.serialize_entry("call_sites", wire_call_sites(call_sites))?;
                map.serialize_entry("expected_method", expected_method)?;
            }
            Finding::MissingEndpoint {
                method,
                path,
                service,
                call_sites,
            } => {
                map.serialize_entry("method", method)?;
                map.serialize_entry("path", path)?;
                map.serialize_entry("service", service)?;
                map.serialize_entry("call_sites", wire_call_sites(call_sites))?;
            }
            Finding::OrphanedEndpoint {
                method,
                path,
                service,
                provenance,
            } => {
                map.serialize_entry("method", method)?;
                map.serialize_entry("path", path)?;
                map.serialize_entry("service", service)?;
                map.serialize_entry("provenance", provenance)?;
            }
            Finding::EnvVarCall {
                method,
                path,
                env_var,
                call_sites,
            } => {
                map.serialize_entry("method", method)?;
                map.serialize_entry("path", path)?;
                map.serialize_entry("env_var", env_var)?;
                map.serialize_entry("call_sites", wire_call_sites(call_sites))?;
            }
            Finding::DependencyConflict {
                package_name,
                tier,
                versions,
            } => {
                map.serialize_entry("package_name", package_name)?;
                map.serialize_entry("tier", tier)?;
                map.serialize_entry("versions", versions)?;
            }
            Finding::SharedExternalContract {
                method,
                path,
                repos,
                call_sites,
            } => {
                map.serialize_entry("method", method)?;
                map.serialize_entry("path", path)?;
                map.serialize_entry("repos", repos)?;
                map.serialize_entry("call_sites", wire_call_sites(call_sites))?;
            }
            Finding::DegradedTypes {
                service,
                stage,
                detail,
            } => {
                map.serialize_entry("service", service)?;
                map.serialize_entry("stage", stage)?;
                map.serialize_entry("detail", detail)?;
            }
        }
        map.end()
    }
}

/// An endpoint reference in the PR delta. `service` is emitted as explicit
/// `null` when unattributed (the wire shows it, unlike `verified` entries).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EndpointRef {
    pub method: String,
    pub path: String,
    pub service: Option<String>,
}

/// What this PR changed relative to the repo's last-indexed (main) state.
/// `None` at the payload level outside a PR diff baseline.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PrDelta {
    pub new_endpoints: Vec<EndpointRef>,
    pub removed_endpoints: Vec<EndpointRef>,
}

impl PrDelta {
    pub fn is_empty(&self) -> bool {
        self.new_endpoints.is_empty() && self.removed_endpoints.is_empty()
    }
}

/// Shape of the project being analyzed: a lone repo, a monorepo (multiple
/// services declared in one carrick.json), or a poly-repo project (peer repos
/// indexed alongside this one). The formatter frames findings with it; the
/// cloud applies the same baseline gating when rendering the PR comment.
#[derive(Clone, Debug, Serialize)]
pub struct Topology {
    /// This repo's name, used to title single-repo comments.
    pub repo_name: String,
    /// Services declared for THIS repo. More than one means a monorepo.
    pub local_service_count: usize,
    /// Other repos indexed for the project (peers).
    pub peer_repo_count: usize,
}

/// Index size for the scanned project.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ScanStats {
    pub endpoints: usize,
    pub calls: usize,
}

/// An endpoint at least one consumer call successfully matched.
#[derive(Clone, Debug, Serialize)]
pub struct VerifiedEndpoint {
    pub method: String,
    pub path: String,
    /// Real route vs mock/test handler (#380); route-wins when several
    /// producers share the (method, path) key.
    pub provenance: EndpointProvenance,
    /// Aggregated per-endpoint type verdict (#455). Drives the cloud
    /// PR-comment renderer's honest "Verified" buckets: `Some(Compatible)` →
    /// "Type-checked", `Some(Unverifiable)` → "Types not verifiable", anything
    /// else (absent, or `Incompatible` — already a loud finding above) →
    /// "Types not compared". Omitted from the wire when absent, so an older
    /// payload and a not-type-checked pair both read as "not compared".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_verdict: Option<crate::operation::TypeVerdict>,
}

/// GraphQL coverage signal: libraries detected vs operations actually
/// indexed. Drives the "commit an emitted schema" banner.
#[derive(Clone, Debug, Serialize)]
pub struct GraphqlStatus {
    pub libraries: Vec<String>,
    pub operations_indexed: bool,
}

/// The `post-pr-result` payload. The transport layer (aws_storage) adds the
/// `action` and `schema_version` envelope fields; everything else on the wire
/// is exactly this struct.
#[derive(Clone, Debug, Serialize)]
pub struct PrResultPayload {
    /// Self-reported repo name — the cloud ignores it (identity comes from
    /// the OIDC token) but it aids log correlation.
    pub repo: String,
    pub pr_number: u64,
    /// `pull_request.head.sha` from GITHUB_EVENT_PATH; `null` if unavailable.
    pub head_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub topology: Topology,
    pub stats: ScanStats,
    pub findings: Vec<Finding>,
    pub delta: Option<PrDelta>,
    pub verified: Vec<VerifiedEndpoint>,
    pub graphql: GraphqlStatus,
    /// Whether every scanned service resolved its types this run. `false`
    /// when any service degraded (see the `degraded_types` findings), so a
    /// surface reading the payload can say "types were not read" instead of
    /// inferring it from an absent type verdict.
    pub has_types: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truncate_chars_is_char_boundary_safe() {
        // Multi-byte input: 'é' (2 bytes), '☕' (3 bytes). The old byte-sliced
        // truncation panicked on exactly this shape.
        let s = "café☕".repeat(100); // 500 chars, 900 bytes
        let out = truncate_chars(&s, 400);
        assert_eq!(out.chars().count(), 400);
        assert!(out.ends_with("..."));
        // Truncation point is a char boundary: the kept prefix is intact.
        assert!(s.starts_with(out.trim_end_matches("...")));

        // At or under the cap: returned unchanged, no ellipsis.
        assert_eq!(truncate_chars("café☕", 5), "café☕");
        assert_eq!(truncate_chars("café☕", 400), "café☕");

        // ASCII over the cap.
        assert_eq!(truncate_chars("abcdefgh", 6), "abc...");
    }

    #[test]
    fn truncate_chars_never_exceeds_max_even_for_tiny_max() {
        // max <= 3 leaves no room for the ellipsis: hard-cut instead. The
        // ≤ max contract must hold for every input (pub fn, release builds).
        assert_eq!(truncate_chars("abcdef", 3), "abc");
        assert_eq!(truncate_chars("abcdef", 2), "ab");
        assert_eq!(truncate_chars("abcdef", 0), "");
        assert_eq!(truncate_chars("café☕x", 2), "ca");
        for max in 0..6 {
            assert!(truncate_chars("abcdefgh", max).chars().count() <= max);
        }
        // Short input with tiny max: unchanged.
        assert_eq!(truncate_chars("ab", 2), "ab");
    }

    #[test]
    fn call_sites_capped_on_the_wire_only() {
        // The in-memory finding keeps every call site (the terminal report
        // shows true counts); only the serialized wire form is capped.
        let sites: Vec<String> = (0..9).map(|i| format!("src/a.ts:{i}")).collect();
        let finding = Finding::missing_endpoint("GET", "/x", None, sites);
        let Finding::MissingEndpoint { call_sites, .. } = &finding else {
            panic!("wrong variant");
        };
        assert_eq!(call_sites.len(), 9);

        let v = serde_json::to_value(&finding).unwrap();
        let wire_sites = v["call_sites"].as_array().unwrap();
        assert_eq!(wire_sites.len(), MAX_CALL_SITES);
        // The first five in order, not an arbitrary subset.
        assert_eq!(wire_sites[0], "src/a.ts:0");
        assert_eq!(wire_sites[4], "src/a.ts:4");
    }

    #[test]
    fn producer_provenance_defaults_to_route_and_serializes() {
        // Constructors default to `route`; the wire always carries the field
        // so the cloud renderer never has to infer it (#380).
        let mismatch = Finding::type_mismatch("GET", "/x", None, vec![], "A", "B", "boom");
        let v = serde_json::to_value(&mismatch).unwrap();
        assert_eq!(v["producer_provenance"], json!("route"));

        let mismatch = mismatch.with_producer_provenance(EndpointProvenance::Mock);
        let v = serde_json::to_value(&mismatch).unwrap();
        assert_eq!(v["producer_provenance"], json!("mock"));

        let orphan = Finding::orphaned_endpoint("GET", "/x", None)
            .with_producer_provenance(EndpointProvenance::Mock);
        let v = serde_json::to_value(&orphan).unwrap();
        assert_eq!(v["provenance"], json!("mock"));

        // No-op on kinds without a producer-side field.
        let missing = Finding::missing_endpoint("GET", "/x", None, vec![])
            .with_producer_provenance(EndpointProvenance::Mock);
        assert_eq!(
            missing,
            Finding::missing_endpoint("GET", "/x", None, vec![])
        );
    }

    #[test]
    fn detail_truncated_at_construction() {
        let long = "☕".repeat(500);
        let finding = Finding::type_mismatch("GET", "/x", None, vec![], "A", "B", &long);
        let Finding::TypeMismatch { detail, .. } = &finding else {
            panic!("wrong variant");
        };
        assert_eq!(detail.chars().count(), MAX_DETAIL_CHARS);
        assert!(detail.ends_with("..."));
    }

    /// A dead sidecar costs the run every type verdict, so the loss has to
    /// reach the comment as a finding the renderer can show. The cloud
    /// buckets an unrecognised kind by its self-reported `severity` and
    /// renders `detail`, so a scanner ahead of the cloud still surfaces it
    /// (carrick#535).
    #[test]
    fn degraded_types_serializes_as_a_risk_with_detail() {
        let finding =
            Finding::degraded_types("api", "capture", "Sidecar process died unexpectedly");
        let v = serde_json::to_value(&finding).unwrap();
        assert_eq!(v["kind"], json!("degraded_types"));
        assert_eq!(v["severity"], json!("risk"));
        assert_eq!(v["service"], json!("api"));
        assert_eq!(v["stage"], json!("capture"));
        assert_eq!(v["detail"], json!("Sidecar process died unexpectedly"));

        let long = "e".repeat(900);
        let Finding::DegradedTypes { detail, .. } =
            Finding::degraded_types("api", "resolve", &long)
        else {
            panic!("wrong variant");
        };
        assert_eq!(detail.chars().count(), MAX_DETAIL_CHARS);
    }

    /// Snapshot of the wire shape: kind tags, severity strings, tier values,
    /// and field names must match docs/internal/pr-result-pipeline.md
    /// (schema_version 1) in carrick-cloud exactly.
    #[test]
    fn payload_serializes_to_wire_contract_shape() {
        let payload = PrResultPayload {
            repo: "api-server".to_string(),
            pr_number: 123,
            head_sha: None,
            run_id: Some("9876543210".to_string()),
            topology: Topology {
                repo_name: "api-server".to_string(),
                local_service_count: 1,
                peer_repo_count: 2,
            },
            stats: ScanStats {
                endpoints: 42,
                calls: 17,
            },
            findings: vec![
                Finding::type_mismatch(
                    "GET",
                    "/api/users",
                    None,
                    vec!["web/src/client.ts:12".to_string()],
                    "UserResponse",
                    "User[]",
                    "Property 'role' is missing",
                ),
                Finding::method_mismatch(
                    "GET",
                    "/api/orders",
                    None,
                    vec!["web/src/orders.ts:4".to_string()],
                    "POST",
                ),
                Finding::missing_endpoint(
                    "DELETE",
                    "/api/sessions",
                    None,
                    vec!["web/src/auth.ts:9".to_string()],
                ),
                Finding::orphaned_endpoint("GET", "/legacy/ping", Some("billing".to_string())),
                Finding::env_var_call(
                    "GET",
                    "/orders",
                    "ORDER_SERVICE_URL",
                    vec!["src/orders.ts:3".to_string()],
                ),
                Finding::dependency_conflict(
                    "zod",
                    tier::MAJOR,
                    vec![PackageVersionRef {
                        repo: "billing".to_string(),
                        version: "4.0.0".to_string(),
                        source: "package.json".to_string(),
                    }],
                ),
                Finding::dependency_conflict("typescript", tier::UNPARSEABLE, vec![]),
                Finding::shared_external_contract(
                    "POST",
                    "/v2/widgets",
                    vec!["repo-alpha".to_string(), "repo-beta".to_string()],
                    vec!["src/widgets-client.ts:33".to_string()],
                ),
            ],
            delta: Some(PrDelta {
                new_endpoints: vec![EndpointRef {
                    method: "GET".to_string(),
                    path: "/x".to_string(),
                    service: None,
                }],
                removed_endpoints: vec![EndpointRef {
                    method: "GET".to_string(),
                    path: "/y".to_string(),
                    service: None,
                }],
            }),
            verified: vec![VerifiedEndpoint {
                method: "GET".to_string(),
                path: "/api/users".to_string(),
                provenance: Default::default(),
                type_verdict: None,
            }],
            graphql: GraphqlStatus {
                libraries: vec![],
                operations_indexed: false,
            },
            has_types: true,
        };

        let v = serde_json::to_value(&payload).unwrap();

        assert_eq!(v["repo"], "api-server");
        assert_eq!(v["pr_number"], 123);
        assert_eq!(v["head_sha"], serde_json::Value::Null);
        assert_eq!(v["run_id"], "9876543210");
        assert_eq!(
            v["topology"],
            json!({ "repo_name": "api-server", "local_service_count": 1, "peer_repo_count": 2 })
        );
        assert_eq!(v["stats"], json!({ "endpoints": 42, "calls": 17 }));
        assert_eq!(
            v["delta"],
            json!({
                "new_endpoints": [{ "method": "GET", "path": "/x", "service": null }],
                "removed_endpoints": [{ "method": "GET", "path": "/y", "service": null }],
            })
        );
        assert_eq!(
            v["verified"],
            json!([{ "method": "GET", "path": "/api/users", "provenance": "route" }])
        );
        assert_eq!(
            v["graphql"],
            json!({ "libraries": [], "operations_indexed": false })
        );

        let findings = v["findings"].as_array().unwrap();
        assert_eq!(
            findings[0],
            json!({
                "kind": "type_mismatch",
                "severity": "risk",
                "method": "GET",
                "path": "/api/users",
                "service": null,
                "call_sites": ["web/src/client.ts:12"],
                "producer_type": "UserResponse",
                "consumer_type": "User[]",
                "detail": "Property 'role' is missing",
                "producer_provenance": "route",
            })
        );
        assert_eq!(
            findings[1],
            json!({
                "kind": "method_mismatch",
                "severity": "risk",
                "method": "GET",
                "path": "/api/orders",
                "service": null,
                "call_sites": ["web/src/orders.ts:4"],
                "expected_method": "POST",
            })
        );
        assert_eq!(
            findings[2],
            json!({
                "kind": "missing_endpoint",
                "severity": "gap",
                "method": "DELETE",
                "path": "/api/sessions",
                "service": null,
                "call_sites": ["web/src/auth.ts:9"],
            })
        );
        assert_eq!(
            findings[3],
            json!({
                "kind": "orphaned_endpoint",
                "severity": "gap",
                "method": "GET",
                "path": "/legacy/ping",
                "service": "billing",
                "provenance": "route",
            })
        );
        assert_eq!(
            findings[4],
            json!({
                "kind": "env_var_call",
                "severity": "advisory",
                "method": "GET",
                "path": "/orders",
                "env_var": "ORDER_SERVICE_URL",
                "call_sites": ["src/orders.ts:3"],
            })
        );
        assert_eq!(
            findings[5],
            json!({
                "kind": "dependency_conflict",
                "severity": "gap",
                "package_name": "zod",
                "tier": "major",
                "versions": [{ "repo": "billing", "version": "4.0.0", "source": "package.json" }],
            })
        );
        // Unparseable tier downgrades the severity to advisory.
        assert_eq!(findings[6]["severity"], "advisory");
        assert_eq!(findings[6]["tier"], "unparseable");
        // Shared external contract (#379): role-free — repos, never a
        // producer/consumer or service attribution.
        assert_eq!(
            findings[7],
            json!({
                "kind": "shared_external_contract",
                "severity": "advisory",
                "method": "POST",
                "path": "/v2/widgets",
                "repos": ["repo-alpha", "repo-beta"],
                "call_sites": ["src/widgets-client.ts:33"],
            })
        );
    }

    /// #455: a verified entry's `type_verdict` serializes to the exact
    /// lowercase strings the cloud PR-comment renderer keys its honest buckets
    /// on (`"compatible"` / `"unverifiable"` / `"incompatible"`), and is
    /// OMITTED entirely when absent so an older payload and a not-type-checked
    /// pair both read as "Types not compared".
    #[test]
    fn verified_endpoint_type_verdict_wire_strings() {
        use crate::operation::TypeVerdict;

        let with = |v: Option<TypeVerdict>| {
            serde_json::to_value(VerifiedEndpoint {
                method: "GET".to_string(),
                path: "/api/users".to_string(),
                provenance: Default::default(),
                type_verdict: v,
            })
            .unwrap()
        };

        assert_eq!(
            with(Some(TypeVerdict::Compatible))["type_verdict"],
            "compatible"
        );
        assert_eq!(
            with(Some(TypeVerdict::Unverifiable))["type_verdict"],
            "unverifiable"
        );
        assert_eq!(
            with(Some(TypeVerdict::Incompatible))["type_verdict"],
            "incompatible"
        );
        // Absent verdict: the key must not appear at all.
        let none = with(None);
        assert!(
            none.get("type_verdict").is_none(),
            "an absent verdict must be omitted from the wire, got: {none}"
        );
    }

    /// An empty `run_id` is omitted, not sent as `""` — the cloud validates
    /// it against `/^\d+$/` and would drop the whole field anyway.
    #[test]
    fn run_id_omitted_when_absent() {
        let payload = PrResultPayload {
            repo: "r".to_string(),
            pr_number: 1,
            head_sha: Some("a".repeat(40)),
            run_id: None,
            topology: Topology {
                repo_name: "r".to_string(),
                local_service_count: 1,
                peer_repo_count: 0,
            },
            stats: ScanStats {
                endpoints: 0,
                calls: 0,
            },
            findings: vec![],
            delta: None,
            verified: vec![],
            graphql: GraphqlStatus {
                libraries: vec![],
                operations_indexed: false,
            },
            has_types: true,
        };
        let v = serde_json::to_value(&payload).unwrap();
        assert!(v.get("run_id").is_none());
        assert_eq!(v["head_sha"], "a".repeat(40));
        assert_eq!(v["delta"], serde_json::Value::Null);
    }
}
