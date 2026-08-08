pub mod builder;

use swc_common::{FileName, SourceMap, SourceMapper, Spanned, sync::Lrc};
use swc_ecma_ast::TsTypeAnn;

use crate::{
    app_context::AppContext,
    config::Config,
    extractor::CoreExtractor,
    findings::{Finding, PackageVersionRef, tier},
    mount_graph::MountGraph,
    operation::OperationKey,
    packages::Packages,
    type_manifest::parse_file_location,
    url_normalizer::UrlNormalizer,
    utils::join_prefix_and_path,
    visitor::{FunctionDefinition, FunctionNodeType, Json, Mount, OwnerType, TypeReference},
};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::LazyLock;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tracing::{debug, warn};

// Regexes are compiled once and reused across every endpoint/type-string pass.
static ROUTE_PARAM_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r":([\w]+)").unwrap());
static IMPORT_PATH_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"import\("([^"]+)"\)\.(\w+)"#).unwrap());
static ARRAY_GENERIC_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"Array<([^>]+)>").unwrap());

// Type aliases to reduce complexity
type RouteFieldMap = HashMap<OperationKey, Json>;
/// A verified (matched) producer endpoint for the report: display label +
/// A verified producer endpoint: a display label + name (`(method, path)` for
/// HTTP, `(KIND, field)` / `(direction, event)` / `("PUBSUB", topic)` for
/// non-HTTP), the producer's provenance so the verified table can mark
/// mock/test handlers (#380), and the aggregated per-endpoint type verdict.
///
/// `type_verdict` is `None` when the matchers build the entry (they run before
/// the compat overlay) and is filled in `get_results` by joining
/// `cross_repo_matches` — see [`ApiAnalyzer::attach_verified_type_verdicts`].
#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedEndpointEntry {
    pub method: String,
    pub path: String,
    pub provenance: crate::operation::EndpointProvenance,
    pub type_verdict: Option<crate::operation::TypeVerdict>,
}

impl VerifiedEndpointEntry {
    /// Build an entry with no verdict yet — the matcher-time constructor.
    fn new(method: String, path: String, provenance: crate::operation::EndpointProvenance) -> Self {
        Self {
            method,
            path,
            provenance,
            type_verdict: None,
        }
    }
}
/// Result of `analyze_matches_with_mount_graph` and
/// `analyze_exact_key_matches`: `(findings, verified_endpoints,
/// cross_repo_matches)`.
type MatcherOutput = (
    Vec<Finding>,
    Vec<VerifiedEndpointEntry>,
    Vec<CrossRepoMatch>,
);

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum ConflictSeverity {
    Critical, // Major version differences (1.x vs 2.x)
    Warning,  // Minor version differences (1.1.x vs 1.2.x)
    Info,     // Patch version differences (1.1.1 vs 1.1.2)
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DependencyConflict {
    pub package_name: String,
    pub repos: Vec<RepoPackageInfo>,
    pub severity: ConflictSeverity,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RepoPackageInfo {
    pub repo_name: String,
    pub version: String,
    pub source_path: PathBuf,
}

/// Project a [`DependencyConflict`] into its wire finding. Tier mapping:
/// `Critical` (a semver major spread) → `major`; `Warning`/`Info` only reach
/// here for pins that didn't parse as comparable semver (see
/// `is_reportable_conflict`), so both map to `unparseable`.
fn dependency_conflict_finding(conflict: &DependencyConflict) -> Finding {
    let tier = match conflict.severity {
        ConflictSeverity::Critical => tier::MAJOR,
        ConflictSeverity::Warning | ConflictSeverity::Info => tier::UNPARSEABLE,
    };
    Finding::dependency_conflict(
        conflict.package_name.clone(),
        tier,
        conflict
            .repos
            .iter()
            .map(|repo| PackageVersionRef {
                repo: repo.repo_name.clone(),
                version: repo.version.clone(),
                source: repo.source_path.display().to_string(),
            })
            .collect(),
    )
}

/// A structured producer→consumer edge captured at the matching site. This is
/// the load-bearing cross-repo signal the eval scorer reads (contract §2): an
/// endpoint owned by one service identity matched by an outbound call from
/// another, with the type-compatibility verdict for that producer endpoint.
/// Both sides are identified by the `service_name ?? repo_name` id (#368), so
/// in a monorepo the two sides can be different services of ONE git repo and
/// still form a real edge. Same-identity pairs (producer_repo ==
/// consumer_repo) are dropped by every matcher (#397): a service exercising
/// its own contract is not a cross-service edge.
///
/// `type_compatible == None` is deliberate and load-bearing: it means compat
/// was never evaluated for this edge (e.g. the type sidecar was unavailable, so
/// the check did not run), as distinct from `Some(true)` "evaluated, compatible".
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CrossRepoMatch {
    /// Repo id of the producer endpoint (service_name ?? repo_name).
    pub producer_repo: String,
    /// `OperationKey::canonical()` of the producer endpoint (mount-resolved path).
    pub producer_key: String,
    /// Repo id of the consumer call (service_name ?? repo_name — the SAME
    /// convention as `producer_repo`, so the two sides of an edge join on one
    /// identity; #368).
    pub consumer_repo: String,
    /// `OperationKey::canonical()` of the consumer call (URL-normalized path).
    pub consumer_key: String,
    /// Source location of the consumer call (`"<file>:<line>[:<col>]"`), the join
    /// key that attributes a per-pair compat verdict to THIS consumer rather than
    /// smearing one producer's first verdict across all its consumers (#260). It
    /// shares the consumer manifest entry's source — both derive from the same
    /// call `file_location` — so after `parse_file_location` normalization the
    /// edge and the outcome's consumer location agree on `(path, line)`. Set
    /// for every edge a consumer call backs, HTTP and exact-key protocol edges
    /// alike: both constructors fill it from the call's `file_path`, and the
    /// overlay iterates all of them. `Option` only to leave room for an edge
    /// source with no call.
    pub consumer_location: Option<String>,
    /// Matcher confidence in `[0, 1]`. `1.0` for an exact normalized-key match
    /// (the only kind captured today; there is no finer score yet).
    pub match_score: f64,
    /// `None` = compat NOT evaluated for this edge; `Some(b)` = evaluated.
    pub type_compatible: Option<bool>,
    /// Three-way per-pair verdict, set by `apply_pair_outcomes` from the same
    /// v2 pair outcomes as `type_compatible` but WITHOUT the lossy collapse:
    /// `Unverifiable` stays distinct from "never evaluated" (`None`), which
    /// `type_compatible` cannot express (both map to `None` there). Downstream
    /// "Verified" bucketing reads this so a matched-but-unverifiable pair is
    /// not silently labelled compiler-compared (#260). `None` = not evaluated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_verdict: Option<crate::operation::TypeVerdict>,
    /// `Some(..)` iff `type_compatible == Some(false)`; human-readable reason.
    pub mismatch_reason: Option<String>,
    /// Whether the producer side of this edge is a real route or a mock/test
    /// handler (#380). Carried from the producer endpoint's structural
    /// classification so downstream surfaces can present a mismatch against a
    /// mock with the right amount of trust. When several producers share one
    /// exact key, route-wins (`EndpointProvenance::min`). Orthogonal to
    /// `relationship`: provenance says where the producer-side entry was
    /// written; relationship says what the pair means. A
    /// shared-external-contract edge still carries the producer side's
    /// provenance (a mock call-site is conceivable and the tags compose).
    #[serde(default)]
    pub producer_provenance: crate::operation::EndpointProvenance,
    /// What this pair means, classified by evidence kind
    /// (`carrick_match::classify_relationship`, #379). `ProducerConsumer` is
    /// the ordinary case: the producer side is a route definition. For
    /// `SharedExternalContract` the producer side's evidence was itself a
    /// call site — both sides encode the same externally-served contract, and
    /// the `producer_*`/`consumer_*` field names carry NO role meaning (they
    /// only say which side sat in the endpoint index vs the call index);
    /// renderers must not label either side producer or consumer, and
    /// type-compat verdicts are never overlaid on these edges (they would be
    /// request-vs-request comparisons mislabelled as producer-contract
    /// verdicts).
    pub relationship: carrick_match::MatchRelationship,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ApiEndpointDetails {
    // owner is Option as we store both call ands endpoints in this data structure.
    // It might make sense to split this out into its own type
    pub owner: Option<OwnerType>,
    pub key: OperationKey,
    #[allow(dead_code)]
    pub params: Vec<String>,
    // - For endpoints, `request_body` is what the server expects to receive
    // - For calls, `request_body` is what the client is sending
    // - For endpoints, `response_body` is what the server sends back
    // - For calls, `response_body` is what the client expects to receive
    pub request_body: Option<Json>,
    pub response_body: Option<Json>,
    pub handler_name: Option<String>,
    pub request_type: Option<TypeReference>,
    pub response_type: Option<TypeReference>,
    pub file_path: PathBuf,
    /// Owning repo, stamped during the cross-repo merge from
    /// `CloudRepoData::repo_name`. `None` outside cross-repo mode (single-repo
    /// data is not repo-tagged). Non-HTTP (GraphQL/socket) matching reads this to
    /// attribute a matched producer/consumer pair to its repos for a
    /// `CrossRepoMatch` edge — HTTP ops get repo identity from the repo-tagged
    /// mount graph instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_name: Option<String>,
    /// Owning service (monorepo `serviceName`), stamped during the cross-repo
    /// merge. Preferred over `repo_name` for the edge repo id (matches the
    /// cloud's `service_name ?? repo_name` convention). `None` when no
    /// `serviceName`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// For endpoints: whether the evidence is a real route or a mock/test
    /// handler, classified structurally from the source path at extraction
    /// (#380). For calls the field is meaningless and stays at its `Route`
    /// default. `default` so index blobs written before the field existed
    /// deserialize as `Route`.
    #[serde(default)]
    pub provenance: crate::operation::EndpointProvenance,
}

pub struct ApiAnalysisResult {
    pub endpoints: Vec<ApiEndpointDetails>,
    pub calls: Vec<ApiEndpointDetails>,
    /// Typed findings, in the wire shape the PR-result payload sends and the
    /// formatter renders (type/method mismatches, missing/orphaned endpoints,
    /// env-var calls, dependency conflicts).
    pub findings: Vec<Finding>,
    /// The structured dependency conflicts behind the `dependency_conflict`
    /// findings, kept alongside because the eval contract (§4) scores the full
    /// Critical/Warning/Info severity, which the wire tier collapses to
    /// major/unparseable.
    pub dependency_conflicts: Vec<DependencyConflict>,
    /// Endpoints that were successfully matched by at least one consumer
    /// call, with their method + canonical path + producer provenance (real
    /// route vs mock/test handler). Surfaced so users see what *worked* in
    /// the PR comment, not just what's broken — clean runs otherwise produce
    /// no positive signal.
    pub verified_endpoints: Vec<VerifiedEndpointEntry>,
    /// GraphQL libraries detected across all scanned repos (subset of
    /// `detected_data_fetchers`). When libraries are present but no
    /// operations were extracted, the formatter suggests committing an
    /// emitted schema (code-first schemas and Relay artifacts are not
    /// statically extractable).
    pub detected_graphql_libraries: Vec<String>,
    /// Whether any GraphQL operations (schema fields or documents) made it
    /// into the index. Gates the "no GraphQL extracted" banner.
    pub graphql_operations_indexed: bool,
    /// Structured producer→consumer edges captured at the matching site, with
    /// per-edge type-compat verdicts. Populated by `get_results`; consumed by
    /// the eval projection (it has no effect on the human Markdown report).
    pub cross_repo_matches: Vec<CrossRepoMatch>,
}

/// Return the subset of `data_fetchers` that are GraphQL libraries.
/// Comparison is case-insensitive to handle package-name casing variations.
pub fn filter_graphql_libraries(data_fetchers: &[String]) -> Vec<String> {
    // Known GraphQL client/server libraries per framework-coverage.md §4.3.
    // Match against the lowercased package name — substring or equality.
    data_fetchers
        .iter()
        .filter(|name| {
            let lower = name.to_lowercase();
            lower == "graphql"
                || lower == "graphql-request"
                || lower == "graphql-tag"
                || lower == "relay-runtime"
                || lower.starts_with("@apollo/")
                || lower.starts_with("@urql/")
                || lower == "urql"
                || lower == "apollo-client"
                || lower == "apollo-server"
        })
        .cloned()
        .collect()
}

/// Recover the verdict-join `(pseudo-method, identity)` from a canonical
/// producer key, for the protocols the type-compat check evaluates
/// (HTTP + socket + graphql + pub/sub). The v2 checker builds each
/// [`PairCheckOutcome`]'s `pseudo_method`/`identity` from the same canonical
/// key material, so the two sides agree by construction:
///
/// - HTTP (`"http|METHOD|path"`) → `("METHOD", "path")`, the HTTP join key.
/// - Socket (`"socket|DIRECTION|event"`) → `("SOCKET", "DIRECTION|event")`.
/// - GraphQL (`"graphql|KIND|field"`) → `("GRAPHQL", "KIND|field")`. The
///   `KIND` (`query`/`mutation`/`subscription`) stays lowercase here AND in
///   the outcome identity, so the two sides agree without any case folding.
/// - Pub/Sub (`"pubsub|topic"`) → `("PUBSUB", "topic")`. Unlike the other three
///   this canonical is 2-segment (topic-only; the broker is not part of
///   identity), so the third `splitn(3, '|')` field is `None`. (A topic
///   literally containing `|` would mis-split, but both sides split identically
///   so the exact-topic match still holds; topics with `|` are pathological and
///   left unguarded.)
///
/// Returns `None` for any other protocol: the check produced no verdict for it,
/// so its edge stays `None` rather than fabricating one.
fn parse_producer_key(key: &str) -> Option<(String, String)> {
    let mut parts = key.splitn(3, '|');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("http"), Some(method), Some(path)) if !method.is_empty() && !path.is_empty() => {
            Some((method.to_uppercase(), path.to_string()))
        }
        (Some("socket"), Some(direction), Some(event))
            if !direction.is_empty() && !event.is_empty() =>
        {
            Some(("SOCKET".to_string(), format!("{}|{}", direction, event)))
        }
        (Some("graphql"), Some(kind), Some(field)) if !kind.is_empty() && !field.is_empty() => {
            Some(("GRAPHQL".to_string(), format!("{}|{}", kind, field)))
        }
        (Some("pubsub"), Some(topic), None) if !topic.is_empty() => {
            Some(("PUBSUB".to_string(), topic.to_string()))
        }
        _ => None,
    }
}

/// Reconstruct a verified endpoint's DISPLAY `(method_or_label, path_or_name)`
/// from a canonical `OperationKey` string, matching [`OperationKey::display_labels`]
/// byte-for-byte so a cross-repo edge joins to its verified row by plain string
/// equality — no path normalization, because a verified entry and the edges
/// pointing at it are both built from the SAME producer key/endpoint (the HTTP
/// matcher stamps `OperationKey::http(method, full_path).canonical()` on the
/// edge and the same `(method, full_path)` on the verified row; the exact-key
/// matcher shares one key across both sides), so their canonical strings are
/// already identical.
///
/// This differs from [`parse_producer_key`] (the verdict-JOIN key, which
/// pseudo-normalizes: `("GRAPHQL", "query|field")`, `("SOCKET", "dir|event")`);
/// here we produce the human display form (`("QUERY", "field")`,
/// `("dir", "event")`) instead. Returns `None` for any unrecognized key.
fn producer_display_from_canonical(key: &str) -> Option<(String, String)> {
    let mut parts = key.splitn(3, '|');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("http"), Some(method), Some(path)) if !method.is_empty() && !path.is_empty() => {
            Some((method.to_string(), path.to_string()))
        }
        // `display_labels` uppercases the GraphQL KIND; the canonical keeps it
        // lowercase, so uppercase here to match the verified row.
        (Some("graphql"), Some(kind), Some(field)) if !kind.is_empty() && !field.is_empty() => {
            Some((kind.to_uppercase(), field.to_string()))
        }
        // The canonical's first socket segment IS `direction.label()`, the same
        // value `display_labels` uses — pass it through verbatim.
        (Some("socket"), Some(direction), Some(event))
            if !direction.is_empty() && !event.is_empty() =>
        {
            Some((direction.to_string(), event.to_string()))
        }
        (Some("pubsub"), Some(topic), None) if !topic.is_empty() => {
            Some(("PUBSUB".to_string(), topic.to_string()))
        }
        _ => None,
    }
}

/// Canonicalize a consumer source location (`"<file>:<line>[:<col>]"`) to the
/// `(path, line)` pair that joins a `CrossRepoMatch` edge to its per-pair
/// verdict (#260). Both sides feed the same `call.file_location` here: the
/// edge stores it verbatim (`path:line:col`), while the outcome carries the
/// manifest entry's `consumer_file`/`consumer_line`. Reducing both through
/// `parse_file_location` strips the divergent column/format suffix so the
/// verdict for one consumer can no longer smear onto another consumer of the
/// same producer endpoint.
fn consumer_identity(location: &str) -> (String, u32) {
    parse_file_location(location)
}

/// Strip the GitHub Actions checkout prefix (`/home/runner/work/<repo>/<repo>/`)
/// from a call-site location so PR-comment risk rows cite `server.ts:66`, not
/// the runner's absolute workspace path (#337). Anything else (local absolute
/// paths, already-relative paths) passes through unchanged.
fn strip_ci_workspace_prefix(location: &str) -> &str {
    location
        .strip_prefix("/home/runner/work/")
        .and_then(|rest| rest.split_once('/'))
        .and_then(|(_, rest)| rest.split_once('/'))
        .map(|(_, rest)| rest)
        .filter(|rest| !rest.is_empty())
        .unwrap_or(location)
}

/// Collapse any dynamic path segment (`:id`, `{id}`, `[id]`) to `:param` so the
/// compat verdict join is param-NAME-agnostic. The cross-repo edge's
/// `producer_key` keeps the source param name (`/orders/:id`), while the
/// outcome's `identity` is built from the normalized manifest
/// (`/orders/:param`). Without collapsing BOTH sides, the join misses on every
/// parameterized route and the edge falls back to the optimistic `Some(true)`
/// default — the historical cause of compat being pinned regardless of the
/// actual verdicts.
fn normalize_compat_path(path: &str) -> String {
    path.split('/')
        .map(|seg| {
            let is_param = seg.starts_with(':')
                || (seg.starts_with('{') && seg.ends_with('}'))
                || (seg.starts_with('[') && seg.ends_with(']'));
            if is_param { ":param" } else { seg }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Map the structured v2 pair outcomes onto each cross-repo edge's
/// `type_compatible`, keyed per consumer (#260) and param-name-agnostic on
/// the path ([`normalize_compat_path`]). Pure over `(outcomes, matches)` so
/// the verdict join is unit-testable without running the checker.
///
/// Verdict precedence per edge key (multiple type_kinds collapse):
/// incompatible > unverifiable/gate-caught > compatible. An edge whose key
/// produced NO pair outcome stays `None` — compat was never evaluated for
/// it, and the old optimistic `Some(true)` default is gone: `Some(true)` now
/// requires an explicit compatible verdict.
pub(crate) fn apply_pair_outcomes(outcomes: &[PairCheckOutcome], matches: &mut [CrossRepoMatch]) {
    // The per-pair verdict key: producer `(METHOD, normalized identity)` plus
    // the consumer call-site identity `(file, line)`.
    type VerdictKey = (String, String, (String, u32));

    let mut incompatible: HashMap<VerdictKey, String> = HashMap::new();
    let mut unverifiable: HashSet<VerdictKey> = HashSet::new();
    let mut compatible: HashSet<VerdictKey> = HashSet::new();
    for outcome in outcomes {
        let line = if outcome.consumer_line == 0 {
            1
        } else {
            outcome.consumer_line
        };
        let key = (
            outcome.pseudo_method.clone(),
            normalize_compat_path(&outcome.identity),
            (outcome.consumer_file.clone(), line),
        );
        match outcome.bucket {
            crate::services::type_sidecar::VerdictBucket::Incompatible => {
                // Multiple type_kinds for one pair collapse to the first
                // reason in outcome order (sorted by pair_key upstream).
                incompatible.entry(key).or_insert_with(|| {
                    outcome
                        .diagnostic
                        .clone()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| {
                            "producer and consumer types are incompatible".to_string()
                        })
                });
            }
            crate::services::type_sidecar::VerdictBucket::Unverifiable
            | crate::services::type_sidecar::VerdictBucket::GateCaughtBakedAny => {
                unverifiable.insert(key);
            }
            crate::services::type_sidecar::VerdictBucket::Compatible => {
                compatible.insert(key);
            }
        }
    }

    for edge in matches.iter_mut() {
        // A shared-external-contract edge has no producer contract to verify:
        // both sides are call sites, so any comparison keyed on the same
        // (method, path, consumer) would be request-vs-request. These edges
        // are verdict-exempt: `type_compatible` stays `None` (#379), and the
        // reason is cleared with it — `mismatch_reason` is only ever present
        // alongside `type_compatible == Some(false)`.
        if edge.relationship != carrick_match::MatchRelationship::ProducerConsumer {
            edge.type_compatible = None;
            edge.type_verdict = None;
            edge.mismatch_reason = None;
            continue;
        }
        // Recover the join key from the producer_key: HTTP, socket, graphql,
        // and pubsub all join here. Any other protocol was never checked, so
        // its verdict is genuinely unknown — leave it `None` rather than
        // fabricate `Some(true)` (#260, part 2).
        let Some((method, path)) = parse_producer_key(&edge.producer_key) else {
            edge.type_compatible = None;
            edge.type_verdict = None;
            continue;
        };
        // Without a consumer identity the pair can't be matched to its own
        // verdict, and asserting `Some(true)` would risk re-smearing — leave it
        // `None` (compat undetermined for this edge).
        let Some(consumer) = edge.consumer_location.as_deref().map(consumer_identity) else {
            edge.type_compatible = None;
            edge.type_verdict = None;
            continue;
        };
        let key = (method, normalize_compat_path(&path), consumer);
        // `type_verdict` carries the three-way result WITHOUT the `Option<bool>`
        // collapse: `Unverifiable` stays distinct from "never evaluated"
        // (`None`), which is exactly what the honest "Verified" buckets need.
        if let Some(reason) = incompatible.get(&key) {
            edge.type_compatible = Some(false);
            edge.type_verdict = Some(crate::operation::TypeVerdict::Incompatible);
            edge.mismatch_reason = Some(reason.clone());
        } else if unverifiable.contains(&key) {
            // Matched but unverifiable — compat undetermined (`type_compatible`
            // NOT `Some(true)`), but the pair WAS reached, so the verdict is a
            // real `Unverifiable`, not absent.
            edge.type_compatible = None;
            edge.type_verdict = Some(crate::operation::TypeVerdict::Unverifiable);
        } else if compatible.contains(&key) {
            edge.type_compatible = Some(true);
            edge.type_verdict = Some(crate::operation::TypeVerdict::Compatible);
        } else {
            // No pair outcome reached this edge: compat was not evaluated
            // for it. `None`, never a fabricated `Some(true)`.
            edge.type_compatible = None;
            edge.type_verdict = None;
        }
    }
}

/// Sort cross-repo edges into a deterministic order and drop exact duplicates,
/// keyed on the
/// `(producer_repo, producer_key, consumer_repo, consumer_key, consumer_location)`
/// identity tuple. The HTTP matcher and the non-HTTP matcher capture edges in
/// non-deterministic iteration order, and `get_results` re-runs this over the
/// combined set so every consumer (PR comment, dashboard, eval projection,
/// cassette gate) sees a stable order. `consumer_location` is part of the
/// identity so two distinct call sites in one consumer repo against the same
/// producer endpoint stay separate edges (each carries its own verdict).
fn sort_dedup_cross_repo_matches(matches: &mut Vec<CrossRepoMatch>) {
    // Relationship is part of edge identity: one repo can index BOTH a real
    // route definition and a call-site-evidence entry on the same key, and
    // the resulting producer/consumer and shared-external-contract edges are
    // distinct facts. Ordered as a stable tiebreaker (ProducerConsumer <
    // SharedExternalContract via the discriminant), never collapsed.
    let relationship_rank = |m: &CrossRepoMatch| match m.relationship {
        carrick_match::MatchRelationship::ProducerConsumer => 0u8,
        carrick_match::MatchRelationship::SharedExternalContract => 1u8,
    };
    matches.sort_by(|a, b| {
        (
            &a.producer_repo,
            &a.producer_key,
            &a.consumer_repo,
            &a.consumer_key,
            &a.consumer_location,
            relationship_rank(a),
        )
            .cmp(&(
                &b.producer_repo,
                &b.producer_key,
                &b.consumer_repo,
                &b.consumer_key,
                &b.consumer_location,
                relationship_rank(b),
            ))
    });
    matches.dedup_by(|a, b| {
        a.producer_repo == b.producer_repo
            && a.producer_key == b.producer_key
            && a.consumer_repo == b.consumer_repo
            && a.consumer_key == b.consumer_key
            && a.consumer_location == b.consumer_location
            && a.relationship == b.relationship
    });
}

/// Sort verified endpoints into a deterministic order and collapse same
/// `(method, path)` rows. Sorting orders by `(method, path, provenance)`, so a
/// `Route` entry sorts before a `Mock` one (`Route < Mock`) and `dedup_by`
/// keeps the first of each run — a key backed by BOTH a real route and a mock
/// reads as a route (#380). `type_verdict` is not yet set at any call site
/// (the matchers build entries verdict-free; the join runs later), so it plays
/// no part in the ordering or the dedup key.
fn sort_dedup_verified(verified: &mut Vec<VerifiedEndpointEntry>) {
    verified.sort_by(|a, b| {
        (&a.method, &a.path, a.provenance).cmp(&(&b.method, &b.path, b.provenance))
    });
    verified.dedup_by(|a, b| a.method == b.method && a.path == b.path);
}

/// Aggregate the per-consumer type verdicts on the cross-repo edges onto each
/// verified producer endpoint. A verified row is per producer `(method, path)`;
/// a producer can have several consumer edges, each with its own verdict, so
/// they collapse by [`TypeVerdict::combine`] (honesty-first: `Compatible` only
/// when EVERY evaluated pair is compatible). Edges with no evaluated verdict
/// (`type_verdict == None` — compat never ran for that pair) contribute
/// nothing, so a row with zero evaluated pairs stays `None` ("types not
/// compared"), never a fabricated compatible.
///
/// The join is exact string equality between the edge's reconstructed producer
/// display ([`producer_display_from_canonical`]) and the verified row's
/// `(method, path)`: both derive from the same producer key, HTTP and non-HTTP
/// alike, so parameterized routes (`/orders/:id`) join without normalization.
fn attach_verified_type_verdicts(
    verified: &mut [VerifiedEndpointEntry],
    cross_repo_matches: &[CrossRepoMatch],
) {
    use std::collections::HashMap;
    let mut by_endpoint: HashMap<(String, String), crate::operation::TypeVerdict> = HashMap::new();
    for edge in cross_repo_matches {
        let Some(verdict) = edge.type_verdict else {
            continue;
        };
        let Some(display) = producer_display_from_canonical(&edge.producer_key) else {
            continue;
        };
        by_endpoint
            .entry(display)
            .and_modify(|agg| *agg = agg.combine(verdict))
            .or_insert(verdict);
    }
    for entry in verified.iter_mut() {
        entry.type_verdict = by_endpoint
            .get(&(entry.method.clone(), entry.path.clone()))
            .copied();
    }
}

/// Order-preserving dedup of byte-identical findings rows, run once at the
/// `get_results` aggregation point so every renderer (PR comment, terminal
/// report, eval projection) sees each row once. A duplicated producer manifest
/// entry (same-key alias collision, #334) made ts_check emit the same mismatch
/// once per duplicate, which rendered as identical rows in the PR comment.
/// Full-struct equality only: findings differing in any field (call site,
/// detail, types) are legitimately distinct and kept. The `kept.contains`
/// check inside the loop makes this O(n^2), which is fine at findings scale
/// (tens of rows); `Finding` is not `Hash`, so a set-based pass isn't worth
/// the ceremony here.
fn dedup_findings(findings: &mut Vec<Finding>) {
    let mut kept: Vec<Finding> = Vec::with_capacity(findings.len());
    for finding in findings.drain(..) {
        if !kept.contains(&finding) {
            kept.push(finding);
        }
    }
    *findings = kept;
}

/// Accept only values whose *shape* is an extractable outgoing-call route, as
/// produced by the file-analyzer LLM's `target` field (see that lambda's
/// system prompt): an absolute path (`/users`), a full URL (`http(s)://…`), or
/// an env-var base form (`${VAR}/path`, `${process.env.VAR}/…`). Template
/// params like `${id}` are legal *inside* a path and are preserved.
///
/// Everything else — bare identifiers (`query`, `DynamoDB`, `CarrickApiKeys`),
/// member/call expressions (`res.json()`, `params.service`), SDK operation
/// tokens (`Service:Op`, `Service.Op`), and literals (`null`, `new`, `.`,
/// `unknown`) — is rejected. Pure string-shape logic: it names no framework,
/// client, or SDK. The shape-blind residue (a bare `${TABLE_NAME}` that is
/// really a datastore resource, not a base URL) is handled on the prompt side.
/// Collapse an inline fallback inside a template interpolation to the bare
/// reference: `${A ?? <expr>}` / `${A || <expr>}` -> `${A}` (equivalently
/// `${process.env.A ?? <expr>}` and dotted alias forms like
/// `${config.baseUrl || <expr>}`). Applies to every interpolation in the
/// target, so a mid-path `${id ?? 0}` also collapses to the plain `${id}`
/// param form.
///
/// The file-analyzer LLM sometimes renders a call target verbatim with its
/// source-level fallback (`${AUDIT_WEBHOOK_URL ?? "http://localhost:3099"}
/// /audit/events`, carrick#399); the whitespace inside the interpolation then
/// fails [`is_valid_route_shape`] and the call is silently dropped. The
/// env-var NAME is the classification signal (carrick#294) and the fallback
/// expression is noise, so both observed renderings of the same call site
/// normalize to the identical canonical key.
///
/// Deliberately tight: the collapse happens only when the left-hand side of a
/// top-level `??`/`||` (outside quotes, brackets, and parens) is a clean
/// reference (ASCII alphanumerics, `_`, `$`, `.`). Anything else — call
/// expressions, concatenation, plain whitespace junk — is left verbatim so
/// [`is_valid_route_shape`] still rejects it.
///
/// Returns `Some(normalized)` when at least one interpolation was collapsed,
/// `None` when the target has no collapsible fallback.
pub fn normalize_env_fallback_target(target: &str) -> Option<String> {
    let mut out = String::with_capacity(target.len());
    let mut rest = target;
    let mut changed = false;
    while let Some(start) = rest.find("${") {
        let (before, after) = rest.split_at(start);
        out.push_str(before);
        let inner = &after[2..];
        let Some(end) = interpolation_end(inner) else {
            // Unterminated interpolation: keep the tail verbatim.
            out.push_str(after);
            rest = "";
            break;
        };
        let content = &inner[..end];
        match fallback_reference(content) {
            Some(reference) => {
                out.push_str("${");
                out.push_str(reference);
                out.push('}');
                changed = true;
            }
            None => out.push_str(&after[..end + 3]),
        }
        rest = &inner[end + 1..];
    }
    out.push_str(rest);
    changed.then_some(out)
}

/// Byte index of the `}` closing an interpolation whose `${` was already
/// consumed. Brace-depth, quote, and escape aware, so a `}` inside a quoted
/// fallback string (including behind an escaped quote, `"\"}"`) or a nested
/// `${...}` inside a backtick template does not terminate the scan early.
fn interpolation_end(s: &str) -> Option<usize> {
    let mut depth = 1usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' | '`' => quote = Some(c),
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
    }
    None
}

/// If the interpolation content is `<reference> ?? <expr>` or
/// `<reference> || <expr>` with the operator at the top level, return the
/// trimmed reference. `None` means "not a collapsible fallback" — the caller
/// keeps the content verbatim.
fn fallback_reference(content: &str) -> Option<&str> {
    let op = top_level_fallback_op(content)?;
    let reference = content[..op].trim();
    let expr = content[op + 2..].trim();
    (!reference.is_empty()
        && !expr.is_empty()
        && reference
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '.')))
    .then_some(reference)
}

/// Byte index of the first `??` or `||` outside quotes and outside any
/// bracket/paren/brace nesting. Escape aware inside quotes, so an escaped
/// quote never ends the string early and a `??`/`||` inside a string literal
/// never counts. Single `?`/`|` (ternary, optional chaining, bitwise-or)
/// never match.
fn top_level_fallback_op(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    for i in 0..bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if b == b'\\' {
                    escaped = true;
                } else if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' | b'`' => quote = Some(b),
                b'{' | b'(' | b'[' => depth += 1,
                b'}' | b')' | b']' => depth = depth.saturating_sub(1),
                b'?' | b'|' if depth == 0 && bytes.get(i + 1) == Some(&b) => {
                    // Match only the START of a `??`/`||` run.
                    if i > 0 && bytes[i - 1] == b {
                        continue;
                    }
                    return Some(i);
                }
                _ => {}
            },
        }
    }
    None
}

pub fn is_valid_route_shape(route: &str) -> bool {
    let route = route.trim();
    if route.is_empty() {
        return false;
    }
    // No leftover JavaScript-source markers that prove the value is an
    // unresolved expression (`a || b`, a call/group) rather than a route.
    let is_clean = |s: &str| {
        !s.contains("||")
            && !s.contains('(')
            && !s.contains(')')
            && !s.chars().any(|c| c.is_whitespace())
    };

    // Explicit `ENV_VAR:NAME:/path` form (the analyzer's canonical env-var
    // route; see `is_env_var_base_url` / `extract_env_var_name`).
    if let Some(rest) = route.strip_prefix("ENV_VAR:") {
        let mut parts = rest.splitn(2, ':');
        let name = parts.next().unwrap_or("");
        return match parts.next() {
            Some(path) => {
                !name.is_empty()
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && path.starts_with('/')
                    && is_clean(path)
                    && path.trim_start_matches('/') != name
            }
            None => false,
        };
    }
    // Env-var base form: `${VAR}/path`, `${process.env.VAR}/…`, bare `${VAR}`.
    if let Some(rest) = route.strip_prefix("${") {
        return rest.contains('}') && is_clean(route);
    }
    // Full URL.
    if route.starts_with("http://") || route.starts_with("https://") {
        return is_clean(route);
    }
    // Absolute path (may carry `${id}` / `:id` template params).
    if route.starts_with('/') {
        return is_clean(route);
    }
    // Bare identifier, member/call expression, `Service:Op`, `#…`, literal.
    false
}

pub struct Analyzer {
    // <Route, http_method, handler_name, source>
    pub imported_handlers: Vec<(String, String, String, String)>,
    pub function_definitions: HashMap<String, FunctionDefinition>,
    pub endpoints: Vec<ApiEndpointDetails>,
    pub calls: Vec<ApiEndpointDetails>,
    pub mounts: Vec<Mount>,
    pub apps: HashMap<String, AppContext>,
    config: Config,
    endpoint_router: Option<matchit::Router<Vec<(String, String)>>>,
    all_repo_packages: HashMap<String, Packages>, // repo_name -> packages
    detected_data_fetchers: Vec<String>,
    mount_graph: Option<MountGraph>, // Mount graph for framework-agnostic analysis
    /// Structured v2 check outcomes, one per matched manifest pair, set by
    /// the engine after `check_v2` runs. `None` = type compat was NOT
    /// evaluated this run (load-bearing: every edge stays `None`, and the
    /// scorer must never read absent compat data as "compatible").
    pair_outcomes: Option<Vec<PairCheckOutcome>>,
    /// Merged manifest entries across every participating repo, for the
    /// alias -> display-name map the findings projection uses.
    type_manifests: Vec<crate::cloud_storage::TypeManifestEntry>,
}

/// One structured type-compat outcome for a matched (producer, consumer,
/// type_kind) manifest pair — the v2 replacement for ts_check's label-keyed
/// verdict JSON. Carries the full join identity, so the overlay matches an
/// edge by `(pseudo_method, normalized identity, consumer location)` without
/// re-parsing any human-readable string.
#[derive(Clone, Debug)]
pub struct PairCheckOutcome {
    /// Stable pair key (`<producer_service>/<producer_alias>~<consumer_service>/<consumer_alias>`).
    pub pair_key: String,
    /// `GET`/`POST`/... for HTTP; `SOCKET`/`GRAPHQL`/`PUBSUB` otherwise —
    /// exactly what `parse_producer_key` recovers from an edge.
    pub pseudo_method: String,
    /// Producer path (HTTP) or exact operation-key tail (other protocols).
    pub identity: String,
    /// Consumer call-site file, as recorded on the manifest entry.
    pub consumer_file: String,
    pub consumer_line: u32,
    /// Kept on the outcome for the WP4 structured-findings swap and the
    /// integration tests; the edge join collapses kinds deliberately.
    #[allow(dead_code)]
    pub type_kind: crate::cloud_storage::ManifestTypeKind,
    pub bucket: crate::services::type_sidecar::VerdictBucket,
    /// For gate buckets: which side and which gate fired.
    #[allow(dead_code)]
    pub gate: Option<String>,
    /// Scrubbed compiler diagnostic or synthesized reason.
    pub diagnostic: Option<String>,
    pub producer_alias: String,
    pub consumer_alias: String,
    #[allow(dead_code)]
    pub producer_service: String,
    #[allow(dead_code)]
    pub consumer_service: String,
}

impl CoreExtractor for Analyzer {}

impl Analyzer {
    pub fn new(config: Config) -> Self {
        Analyzer {
            imported_handlers: Vec::new(),
            function_definitions: HashMap::new(),
            endpoints: Vec::new(),
            calls: Vec::new(),
            mounts: Vec::new(),
            apps: HashMap::new(),
            config,
            endpoint_router: None,
            all_repo_packages: HashMap::new(),
            detected_data_fetchers: Vec::new(),
            mount_graph: None,
            pair_outcomes: None,
            type_manifests: Vec::new(),
        }
    }

    /// Set the mount graph for framework-agnostic analysis
    pub fn set_mount_graph(&mut self, mount_graph: MountGraph) {
        self.mount_graph = Some(mount_graph);
    }

    /// Store the structured v2 check outcomes for this run. Not calling this
    /// leaves compat unevaluated: every edge keeps `type_compatible: None`.
    pub fn set_pair_outcomes(&mut self, outcomes: Vec<PairCheckOutcome>) {
        self.pair_outcomes = Some(outcomes);
    }

    /// Store the merged manifest entries (all repos) for display-name mapping.
    pub fn set_type_manifests(&mut self, entries: Vec<crate::cloud_storage::TypeManifestEntry>) {
        self.type_manifests = entries;
    }

    pub fn add_repo_packages(&mut self, repo_name: String, packages: Packages) {
        self.all_repo_packages.insert(repo_name, packages);
    }

    pub fn analyze_dependencies(&self) -> Vec<DependencyConflict> {
        self.find_dependency_conflicts()
    }

    fn find_dependency_conflicts(&self) -> Vec<DependencyConflict> {
        let mut package_versions: HashMap<String, Vec<RepoPackageInfo>> = HashMap::new();

        // Collect all packages from all repositories
        for (repo_name, packages) in &self.all_repo_packages {
            for (package_name, package_info) in packages.get_dependencies() {
                let repo_package_info = RepoPackageInfo {
                    repo_name: repo_name.clone(),
                    version: package_info.version.clone(),
                    source_path: package_info.source_path.clone(),
                };

                package_versions
                    .entry(package_name.clone())
                    .or_default()
                    .push(repo_package_info);
            }
        }

        // Find packages with conflicting versions
        let mut conflicts = Vec::new();
        for (package_name, repo_infos) in package_versions {
            if repo_infos.len() > 1 {
                // Check if all versions are the same
                let first_version = &repo_infos[0].version;
                let has_conflicts = repo_infos.iter().any(|info| info.version != *first_version);

                if has_conflicts && Self::is_reportable_conflict(&repo_infos) {
                    let severity = Self::determine_conflict_severity(&repo_infos);
                    conflicts.push(DependencyConflict {
                        package_name,
                        repos: repo_infos,
                        severity,
                    });
                }
            }
        }

        conflicts
    }

    /// A cross-service version difference is only worth reporting when the
    /// versions are semver-INCOMPATIBLE — i.e. they span more than one MAJOR
    /// version (`zod` 3.x vs 4.x). Differences confined to minor/patch within a
    /// single major (`typescript` 5.3 vs 5.4) are semver-compatible by
    /// construction and would be false positives: they don't cause the
    /// cross-service type/runtime breakage this report exists to surface, and on
    /// real org-wide installs they are pervasive noise that buries the genuine
    /// major-version conflicts. Versions that don't parse as semver fall back to
    /// "report it" — `has_conflicts` already established the raw strings differ,
    /// and a genuinely divergent non-semver pin (`workspace:*` vs a tag, a git
    /// URL) must never be silently dropped.
    fn is_reportable_conflict(repo_infos: &[RepoPackageInfo]) -> bool {
        use semver::Version;

        let parsed: Vec<Version> = repo_infos
            .iter()
            .filter_map(|info| Version::parse(&info.version).ok())
            .collect();

        // At least one version is not valid semver: report conservatively.
        if parsed.len() != repo_infos.len() {
            return true;
        }

        let first_major = parsed[0].major;
        parsed.iter().any(|v| v.major != first_major)
    }

    fn determine_conflict_severity(repo_infos: &[RepoPackageInfo]) -> ConflictSeverity {
        use semver::Version;

        let mut versions = Vec::new();
        for info in repo_infos {
            if let Ok(version) = Version::parse(&info.version) {
                versions.push(version);
            }
        }

        if versions.len() < 2 {
            return ConflictSeverity::Info;
        }

        // Check for major version differences
        let first_major = versions[0].major;
        if versions.iter().any(|v| v.major != first_major) {
            return ConflictSeverity::Critical;
        }

        // Check for minor version differences
        let first_minor = versions[0].minor;
        if versions.iter().any(|v| v.minor != first_minor) {
            return ConflictSeverity::Warning;
        }

        // Only patch differences remain
        ConflictSeverity::Info
    }

    fn byte_offset_to_utf16_offset(source: &str, byte_offset: usize) -> usize {
        source[..byte_offset].encode_utf16().count()
    }

    /// Normalize route by removing ENV_VAR prefixes and extracting the actual path
    fn normalize_route_for_type_name(route: &str) -> String {
        if route.contains("ENV_VAR:") {
            // Extract the actual path from ENV_VAR constructs
            // "ENV_VAR:COMMENT_SERVICE_URL:/api/comments" -> "/api/comments"
            let segments: Vec<&str> = route.split("ENV_VAR:").collect();
            let mut clean_path = String::new();

            // Add the part before any ENV_VAR marker
            clean_path.push_str(segments[0]);

            // Process each segment with an ENV_VAR marker
            for segment in segments.iter().skip(1) {
                let subparts: Vec<&str> = segment.splitn(2, ':').collect();
                if subparts.len() == 2 {
                    clean_path.push_str(subparts[1]);
                }
            }

            clean_path
        } else {
            route.to_string()
        }
    }

    /// Generate common type alias name for producer/consumer comparison
    /// This creates matching names that can be compared via ts-morph
    pub fn generate_common_type_alias_name(
        route: &str,
        method: &str,
        is_request_type: bool,
        is_consumer: bool,
    ) -> String {
        let suffix = if is_request_type {
            "Request"
        } else {
            "Response"
        };
        let role = if is_consumer { "Consumer" } else { "Producer" };
        let method_pascal = Self::method_to_pascal_case(method);

        // Normalize the route to handle env vars consistently
        let normalized_route = Self::normalize_route_for_type_name(route);
        let sanitized_route = Self::sanitize_route_for_dynamic_paths(&normalized_route);

        format!("{}{}{}{}", method_pascal, sanitized_route, suffix, role)
    }

    /// Helper method to convert HTTP method to PascalCase
    fn method_to_pascal_case(method: &str) -> String {
        if method.is_empty() {
            "UnknownMethod".to_string()
        } else {
            let lowercase_method = method.to_lowercase();
            let mut m = lowercase_method.chars();
            match m.next() {
                None => "UnknownMethod".to_string(),
                Some(f) => f.to_uppercase().collect::<String>() + m.as_str(),
            }
        }
    }

    fn sanitize_route_for_dynamic_paths(route: &str) -> String {
        // Strip query parameters first
        let route_without_query = if let Some(query_idx) = route.find('?') {
            &route[..query_idx]
        } else {
            route
        };

        route_without_query
            .split('/')
            .filter(|segment| !segment.is_empty()) // Remove empty segments
            .map(|segment| {
                if let Some(param_name) = segment.strip_prefix(':') {
                    // Convert :id -> ById, :userId -> ByUserId, :eventId -> ByEventId
                    format!("By{}", Self::to_pascal_case(param_name))
                } else if segment.starts_with("${") && segment.ends_with('}') {
                    // Handle template literal syntax: ${userId} -> ByUserid
                    // Extract the variable name from ${varName} or ${process.env.VAR}
                    let inner = &segment[2..segment.len() - 1]; // Remove ${ and }
                    // If it contains a dot (like process.env.VAR), take the last part
                    let param_name = inner.rsplit('.').next().unwrap_or(inner);
                    format!("By{}", Self::to_pascal_case(param_name))
                } else {
                    // Convert regular segments to PascalCase
                    Self::to_pascal_case(segment)
                }
            })
            .collect::<Vec<String>>()
            .join("")
    }

    fn to_pascal_case(input: &str) -> String {
        if input.is_empty() {
            return String::new();
        }

        let mut result = String::new();
        let mut capitalize_next = true;

        for ch in input.chars() {
            if ch.is_alphanumeric() {
                if capitalize_next {
                    result.push(ch.to_uppercase().next().unwrap_or(ch));
                    capitalize_next = false;
                } else {
                    result.push(ch.to_lowercase().next().unwrap_or(ch));
                }
            } else {
                // Non-alphanumeric characters trigger capitalization of next char
                capitalize_next = true;
            }
        }

        result
    }

    /// Extract environment variable name from a route
    /// Examples:
    /// - "ENV_VAR:API_URL:/users" -> "API_URL"
    /// - "${process.env.SERVICE_URL}/orders" -> "SERVICE_URL"
    /// - "${API_BASE}/users" -> "API_BASE"
    /// - "unknown" -> "UNKNOWN_API"
    fn extract_env_var_name(route: &str) -> String {
        // Handle ENV_VAR:NAME:/path format
        if route.starts_with("ENV_VAR:") {
            let parts: Vec<&str> = route.splitn(3, ':').collect();
            if parts.len() >= 2 {
                return parts[1].to_string();
            }
        }

        // Handle ${process.env.VAR} or ${VAR} patterns
        if let Some(start) = route.find("${")
            && let Some(end) = route[start..].find('}')
        {
            let inner = &route[start + 2..start + end];
            // Handle process.env.VAR -> VAR
            if let Some(last_dot) = inner.rfind('.') {
                return inner[last_dot + 1..].to_string();
            }
            return inner.to_string();
        }

        // Handle process.env.VAR patterns (without ${})
        if let Some(idx) = route.find("process.env.") {
            let after = &route[idx + 12..];
            let end = after
                .find(|c: char| !c.is_alphanumeric() && c != '_')
                .unwrap_or(after.len());
            if end > 0 {
                return after[..end].to_string();
            }
        }

        // Handle start-of-string variable (e.g. API_URL + "/path")
        if let Some(first_char) = route.chars().next()
            && first_char.is_uppercase()
        {
            let end = route
                .find(|c: char| !c.is_alphanumeric() && c != '_')
                .unwrap_or(route.len());
            if end > 0 {
                return route[..end].to_string();
            }
        }

        "UNKNOWN_API".to_string()
    }

    /// Check if a route represents an environment variable base URL.
    ///
    /// Returns true for:
    /// - "ENV_VAR:API_URL:/users" (explicit ENV_VAR format)
    /// - "${process.env.API_URL}/users" (process.env pattern at start)
    /// - "${API_BASE_URL}/users" (UPPER_CASE var at start)
    /// - "${authHost}/oauth/token" (ANY leading variable followed by a path)
    ///
    /// Returns false for:
    /// - "/users/${userId}" (path parameter, not base URL)
    /// - "/api/${version}/data" (path parameter in middle)
    /// - "${userId}" (whole-URL opaque variable — no path to classify)
    fn is_env_var_base_url(route: &str) -> bool {
        // Check for explicit ENV_VAR: prefix format
        if route.starts_with("ENV_VAR:") {
            return true;
        }

        // Check for process.env pattern
        if route.contains("process.env.") {
            return true;
        }

        // Check for ${...} at the START of the route (not in the middle)
        if route.starts_with("${")
            && let Some(end) = route.find('}')
        {
            let var_name = &route[2..end];
            // If it contains a dot (like process.env.X) or is UPPER_CASE, it's an env var
            if var_name.contains('.')
                || var_name
                    .chars()
                    .all(|c| c.is_uppercase() || c == '_' || c.is_ascii_digit())
            {
                return true;
            }
            // #378: casing is not identity. ANY leading variable followed by
            // a path remainder is structurally a base URL — the call cannot
            // be located without knowing it, so it must be classified
            // (internalEnvVars/externalEnvVars) before it may match;
            // previously the matcher silently stripped lowercase bases and
            // paired third-party calls with in-org producers that happened to
            // share a generic path.
            //
            // #467 narrowed the persisted consumer key: `consumer_call_path`
            // now keeps only an ENV-VAR-SHAPED undeclared base
            // (`process.env.*` / `SCREAMING_SNAKE`) verbatim and strips an
            // injected one (`${this.apiUrl}/…`), because no `internalEnvVars`
            // entry could ever classify a class property. This gate is
            // deliberately left as-is: it runs on the ALREADY-canonicalized
            // key, so an injected base no longer reaches it as a `${var}`
            // prefix, and the branch still guards any other route shape that
            // does.
            if route[end + 1..].starts_with('/') {
                return true;
            }
        }

        // Check for start-of-string variables (e.g. API_URL + "/path")
        // If it starts with an uppercase letter and is not a path (doesn't start with /),
        // we treat it as a potential environment variable or constant base URL.
        if let Some(first_char) = route.chars().next()
            && first_char.is_uppercase()
        {
            // Extract the first identifier
            let end = route
                .find(|c: char| !c.is_alphanumeric() && c != '_')
                .unwrap_or(route.len());

            // If the identifier is non-empty and looks like a constant (mostly uppercase/digits/underscore)
            // we treat it as an env var.
            // We verify it's at least 2 chars to avoid single letters being treated as vars excessively
            if end >= 2 {
                let ident = &route[..end];
                if ident
                    .chars()
                    .all(|c| c.is_uppercase() || c == '_' || c.is_ascii_digit())
                {
                    return true;
                }
            }
        }

        false
    }

    /// Helper to process a TsTypeAnn and produce a TypeReference.
    /// This function encapsulates the logic to find the correct span,
    /// calculate the UTF-16 offset, and build the TypeReference struct.
    pub fn create_type_reference_from_swc(
        type_ann_swc: &TsTypeAnn,
        cm: &Lrc<SourceMap>,
        func_def_file_path: &Path,
        alias: String,
    ) -> Option<TypeReference> {
        let type_ref_span = match &*type_ann_swc.type_ann {
            swc_ecma_ast::TsType::TsTypeRef(type_ref) => type_ref.span,
            _ => type_ann_swc.span, // fallback
        };

        let loc = cm.lookup_char_pos(type_ref_span.lo);
        let file_start_bytepos = loc.file.start_pos;
        if type_ref_span.lo < file_start_bytepos {
            warn!(
                "Span `lo` ({:?}) is before its supposed file's start_pos ({:?}) for file {:?}. This indicates a SourceMap or span issue.",
                type_ref_span.lo, file_start_bytepos, loc.file.name
            );
            return None; // Or handle as an error appropriately
        }
        let file_relative_byte_offset_u32 = (type_ref_span.lo - file_start_bytepos).0;

        let actual_span_file_path = match &*loc.file.name {
            FileName::Real(pathbuf) => pathbuf.clone(), // Clone to own PathBuf
            other => {
                warn!(
                    "Span found in a non-real file: {:?}. Cannot process.",
                    other
                );
                return None;
            }
        };

        let file_content = match std::fs::read_to_string(&actual_span_file_path) {
            Ok(content) => content,
            Err(e) => {
                warn!(
                    "Failed to read file {:?} for offset calculation: {}. Skipping.",
                    actual_span_file_path, e
                );
                return None;
            }
        };

        let utf16_offset = Self::byte_offset_to_utf16_offset(
            &file_content,
            file_relative_byte_offset_u32 as usize,
        );

        let composite_type_string = cm
            .span_to_snippet(type_ann_swc.type_ann.span())
            .unwrap_or_else(|_| "UnknownType".to_string());

        Some(TypeReference {
            file_path: func_def_file_path.to_path_buf(), // Use the function's file path
            type_ann: Some(Box::new(*type_ann_swc.type_ann.clone())), // Store the SWC AST node
            start_position: utf16_offset,
            composite_type_string,
            alias,
        })
    }

    pub fn resolve_types_for_endpoints(&mut self, cm: Lrc<SourceMap>) -> &mut Self {
        let mut request_types_map = HashMap::new();
        let mut response_types_map = HashMap::new();
        let mut seen = HashSet::new();

        // Routers that are mounted on routers can cause duplicate endpoints
        // Lets fix this through dedupe rather than editing the mounting
        self.endpoints.retain(|endpoint| {
            let key = (endpoint.key.clone(), endpoint.handler_name.clone());
            // returns true or false if the value in the set already exists
            seen.insert(key)
        });

        for endpoint in &self.endpoints {
            let Some((method, route)) = endpoint.key.as_http() else {
                continue;
            };
            if let Some(handler_name) = &endpoint.handler_name
                && let Some(func_def) = self.function_definitions.get(handler_name)
                && func_def.arguments.len() >= 2
            {
                // Process Request Type (argument 0)
                if let Some(req_type_ann_swc) = &func_def.arguments[0].type_ann {
                    let alias = Self::generate_common_type_alias_name(
                        route, method, true,  // is_request_type
                        false, // is_consumer = false (endpoints are producers)
                    );
                    if let Some(type_ref) = Self::create_type_reference_from_swc(
                        req_type_ann_swc,
                        &cm,
                        &func_def.file_path,
                        alias,
                    ) {
                        request_types_map.insert(endpoint.key.clone(), type_ref);
                    }
                }

                // Process Response Type (argument 1)
                if let Some(res_type_ann_swc) = &func_def.arguments[1].type_ann {
                    let alias = Self::generate_common_type_alias_name(
                        route, method, false, // is_request_type = false
                        false, // is_consumer = false (endpoints are producers)
                    );
                    if let Some(type_ref) = Self::create_type_reference_from_swc(
                        res_type_ann_swc,
                        &cm,
                        &func_def.file_path,
                        alias,
                    ) {
                        response_types_map.insert(endpoint.key.clone(), type_ref);
                    }
                }
            }
        }

        // Update all endpoints with the resolved types
        for endpoint in &mut self.endpoints {
            if let Some(req_type) = request_types_map.get(&endpoint.key) {
                endpoint.request_type = Some(req_type.clone());
            }
            if let Some(resp_type) = response_types_map.get(&endpoint.key) {
                endpoint.response_type = Some(resp_type.clone());
            }
        }
        self
    }

    // This function analyzes the function definitions and returns a HashMap of route fields.
    pub fn resolve_imported_handler_route_fields(
        &self,
        imported_handlers: &[(String, String, String, String)],
        function_definitions: &HashMap<String, FunctionDefinition>,
    ) -> (RouteFieldMap, RouteFieldMap) {
        let mut response_fields = HashMap::new();
        let mut request_fields = HashMap::new();

        for (route, method, handler_name, _) in imported_handlers {
            if let Some(func_def) = function_definitions.get(handler_name) {
                // Extract response fields from the handler function
                let resp_json = match &func_def.node_type {
                    FunctionNodeType::ArrowFunction(arrow) => self.extract_fields_from_arrow(arrow),
                    FunctionNodeType::FunctionDeclaration(decl) => {
                        self.extract_fields_from_function_decl(decl)
                    }
                    FunctionNodeType::FunctionExpression(expr) => {
                        self.extract_fields_from_function_expr(expr)
                    }
                    FunctionNodeType::Placeholder => {
                        // In CI mode, AST is not available, skip field extraction
                        Json::Null
                    }
                };

                // Extract request body fields from the handler function
                let req_json = match &func_def.node_type {
                    FunctionNodeType::ArrowFunction(arrow) => {
                        if let swc_ecma_ast::BlockStmtOrExpr::BlockStmt(block) = &*arrow.body {
                            self.extract_req_body_fields(block)
                        } else {
                            None
                        }
                    }
                    FunctionNodeType::FunctionDeclaration(decl) => {
                        if let Some(body) = &decl.function.body {
                            self.extract_req_body_fields(body)
                        } else {
                            None
                        }
                    }
                    FunctionNodeType::FunctionExpression(expr) => {
                        if let Some(body) = &expr.function.body {
                            self.extract_req_body_fields(body)
                        } else {
                            None
                        }
                    }
                    FunctionNodeType::Placeholder => {
                        // In CI mode, AST is not available, skip request body extraction
                        None
                    }
                };

                // Store with composite key
                let key = OperationKey::http(method, route.clone());
                response_fields.insert(key.clone(), resp_json);
                if let Some(req) = req_json {
                    request_fields.insert(key, req);
                }
            }
        }

        (response_fields, request_fields)
    }

    // We know endpoints will exist for each imported handler
    pub fn update_endpoints_with_resolved_fields(
        &mut self,
        response_fields: RouteFieldMap,
        request_fields: RouteFieldMap,
    ) -> &mut Self {
        for endpoint in &mut self.endpoints {
            if let Some(response) = response_fields.get(&endpoint.key) {
                endpoint.response_body = Some(response.clone());
            }
            if let Some(request) = request_fields.get(&endpoint.key) {
                endpoint.request_body = Some(request.clone());
            }
        }

        self
    }

    /// Framework-agnostic analysis using mount graph.
    /// Returns `(findings, verified_endpoints, cross_repo_matches)` — the
    /// second element captures (method, path) of every endpoint that at least
    /// one consumer call successfully matched (positive signal for the PR
    /// comment), and the third captures the structured producer→consumer
    /// edges (consumed only by the eval projection).
    fn analyze_matches_with_mount_graph(&self, mount_graph: &MountGraph) -> MatcherOutput {
        /// A wrong-verb call group awaiting resolution. Producers hold
        /// `(EXPECTED_METHOD, suppression key, declared full_path)` for every
        /// exact-path producer; resolution is deferred to after the call loop
        /// because "prefer an unverified producer" needs the complete
        /// verified set.
        #[derive(Default)]
        struct MismatchCandidate {
            call_sites: BTreeSet<String>,
            producers: Vec<(String, String, String)>,
        }

        // Grouped accumulators, BTree-keyed so same-target call sites collapse
        // into one finding and the emitted order is deterministic.
        // (METHOD, path) → call sites.
        let mut missing: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
        // (METHOD, consumer path) → wrong-verb candidate.
        let mut mismatch_candidates: BTreeMap<(String, String), MismatchCandidate> =
            BTreeMap::new();
        // (env_var, METHOD, path) → call sites.
        let mut env_var_calls: BTreeMap<(String, String, String), BTreeSet<String>> =
            BTreeMap::new();
        // The single producer each method-mismatch finding names: suppressed
        // from the orphan list so a wrong-verb call reports once, as a risk —
        // not as a missing+orphaned pair. Its exact-path siblings keep their
        // own verified/orphaned classification.
        let mut method_mismatched_producers: HashSet<String> = HashSet::new();
        // Structured producer→consumer edges for the eval projection.
        let mut cross_repo_matches: Vec<CrossRepoMatch> = Vec::new();
        // Shared-external-contract groups (#379): (METHOD, path) → (repo ids,
        // call sites) for matches whose "producer" side is itself call-site
        // evidence — no one in the pair defines the route, so the repos merely
        // encode the same externally-served contract. BTree-keyed for
        // deterministic emission; reported as one group finding per contract,
        // with no producer/consumer roles.
        type SharedGroup = (BTreeSet<String>, BTreeSet<String>);
        let mut shared_contract_groups: BTreeMap<(String, String), SharedGroup> = BTreeMap::new();

        // Consumer id lookup: a call's `(METHOD, canonical_path, file_location)`
        // → owning `service_name ?? repo_name` id — the SAME id convention the
        // producer side resolves through `endpoint.service_name ??
        // endpoint.repo_name`, so both sides of an edge join on one identity
        // (#368; a repo-only consumer id left monorepo rows asymmetric).
        // `merge_from_repos` tags each merged data call with its repo and
        // service; `self.calls` carry only `(key, file_path)`, so this
        // re-attaches the identity at the matching site. Keyed on `canonical_path` (NOT
        // the raw `target_url`) because that is exactly the path the matcher looks
        // up with — `self.calls[].key` is `OperationKey::http(method,
        // canonical_path)`, so `build_cross_repo_match`'s `target` is the bare
        // canonical path. Keying on the raw `${ENV}/path` here would never join
        // (they diverge whenever the host base is stripped). Keyed on the full
        // triple because two calls in one file can share a canonical path.
        let consumer_repo_by_call: HashMap<(String, String, String), String> = mount_graph
            .get_data_calls()
            .iter()
            .filter_map(|c| {
                c.service_name.as_ref().or(c.repo_name.as_ref()).map(|id| {
                    (
                        (
                            c.method.to_uppercase(),
                            c.canonical_path.clone(),
                            c.file_location.clone(),
                        ),
                        id.clone(),
                    )
                })
            })
            .collect();

        // Track which endpoints have been matched
        let mut matched_endpoints: HashSet<String> = HashSet::new();

        // Deduplicate calls
        let mut unique_calls = Vec::new();
        let mut seen_calls = HashSet::new();
        for call in &self.calls {
            // Drop HTTP calls whose target is not a real outgoing-call shape.
            // The file-analyzer LLM sometimes emits SDK ops, bare identifiers,
            // or member expressions as a call target (e.g. `DynamoDB:PutItem`,
            // `res.json()`); those never match a producer and would otherwise
            // flood the report as "missing endpoints" / env-var suggestions.
            // Non-HTTP operations (GraphQL/Socket) are keyed exactly and handled
            // by their own matchers, so this route-shape gate only applies to HTTP.
            if let Some((_, target)) = call.key.as_http()
                && !is_valid_route_shape(target)
            {
                debug!("Skipping call with non-route value: {}", call.key);
                continue;
            }
            let key = format!("{}:{}", call.key.canonical(), call.file_path.display());
            if seen_calls.insert(key) {
                unique_calls.push(call);
            }
        }

        // Create URL normalizer once for all calls
        let normalizer = UrlNormalizer::new(&self.config);

        // For each call, try to find matching endpoint using mount graph.
        // This is the HTTP matcher: non-HTTP operations are dispatched to
        // their own matchers and skipped here.
        for call in &unique_calls {
            let Some((method, target)) = call.key.as_http() else {
                continue;
            };
            let call_site = call.file_path.display().to_string();

            // Env-var base URLs (framework-agnostic; smarter detection avoids
            // false positives on path parameters) look up by their canonical
            // `ENV_VAR:` route; everything else looks up the raw target.
            let (lookup_url, miss_path);
            let is_env_route = Self::is_env_var_base_url(target);
            if is_env_route {
                let env_var_name = Self::extract_env_var_name(target);
                let normalized_path = normalizer.extract_path(target);
                let canonical_env_var_route =
                    format!("ENV_VAR:{}:{}", env_var_name, normalized_path);

                if self.config.is_external_call(&canonical_env_var_route) {
                    continue;
                }
                if !self.config.is_internal_call(&canonical_env_var_route) {
                    env_var_calls
                        .entry((env_var_name, method.to_string(), normalized_path))
                        .or_default()
                        .insert(call_site);
                    continue;
                }
                lookup_url = canonical_env_var_route;
                miss_path = normalized_path;
            } else {
                lookup_url = target.to_string();
                miss_path = normalizer.extract_path(target);
            }

            match mount_graph.find_matching_endpoints_with_normalizer(
                &lookup_url,
                method,
                &normalizer,
            ) {
                None => {
                    // URL was identified as external - skip it
                }
                Some(matching_endpoints) if !matching_endpoints.is_empty() => {
                    // The env-var edge keys on the extracted path (identical
                    // to the pre-typed matcher); the plain edge re-normalizes
                    // the raw target.
                    let normalized_path = if is_env_route {
                        miss_path
                    } else {
                        normalizer.normalize(&lookup_url).path
                    };
                    for endpoint in matching_endpoints {
                        // #381: a pairing with zero literal agreement — a
                        // wildcard-only producer (`GET /*`) absorbing an
                        // arbitrary call — carries no signal. The call is
                        // ROUTED (so it is not a missing endpoint) but not
                        // MATCHED: no verified mark, no cross-repo edge.
                        // Non-zero pairs are already the most specific
                        // available (the mount graph filters to maximal
                        // agreement).
                        if carrick_match::match_agreement(&endpoint.full_path, &normalized_path)
                            .unwrap_or(0)
                            == 0
                        {
                            continue;
                        }
                        let key = format!("{}:{}", endpoint.method, endpoint.full_path);
                        matched_endpoints.insert(key);
                        let Some(edge) = Self::build_cross_repo_match(
                            call,
                            method,
                            target,
                            &normalized_path,
                            endpoint,
                            &consumer_repo_by_call,
                        ) else {
                            continue;
                        };
                        match edge.relationship {
                            carrick_match::MatchRelationship::ProducerConsumer => {
                                // #397: a service calling its own endpoint
                                // (through an env-var base; localhost
                                // self-calls are dropped at extraction) is
                                // real behaviour but not a cross-service
                                // contract edge. Drop the self-pair on repo
                                // identity alone, the same structural rule
                                // `analyze_exact_key_matches` applies. The
                                // endpoint was already marked matched above,
                                // so it still surfaces as a verified
                                // endpoint — only the degenerate self-edge
                                // is dropped.
                                if edge.producer_repo != edge.consumer_repo {
                                    cross_repo_matches.push(edge);
                                }
                            }
                            carrick_match::MatchRelationship::SharedExternalContract => {
                                // Both sides are call-site encodings of the
                                // same external contract (#379): record group
                                // membership for the report. The edge's
                                // producer_/consumer_ fields carry no roles
                                // (see `CrossRepoMatch::relationship`); a
                                // same-repo pair (a repo re-matching its own
                                // double-extracted site) still counts toward
                                // the group but emits no self edge.
                                let (repos, sites) = shared_contract_groups
                                    .entry((method.to_string(), normalized_path.clone()))
                                    .or_default();
                                repos.insert(edge.producer_repo.clone());
                                repos.insert(edge.consumer_repo.clone());
                                sites.insert(call_site.clone());
                                // The endpoint-side entry is ITSELF a call
                                // site (that is what made the pair shared) —
                                // its source location is a group member too,
                                // so the report shows where every encoding
                                // lives, including the double-extracted one.
                                sites.insert(endpoint.file_location.clone());
                                if edge.producer_repo != edge.consumer_repo {
                                    cross_repo_matches.push(edge);
                                }
                            }
                        }
                    }
                }
                Some(_) => {
                    // No producer under this method — retry with an EXACT
                    // path match (param-name-agnostic, never wildcarding a
                    // param against a concrete segment) ignoring the method,
                    // to tell a wrong verb on a declared route (a contract
                    // risk) apart from a genuinely missing endpoint (a
                    // connectivity gap). A wildcard-only collision — e.g.
                    // `POST /users/:id` while `GET /users/list` is missing —
                    // stays a missing endpoint.
                    // Only route-definition evidence can back "the producer
                    // expects METHOD": a call-site-evidence entry (#379) is
                    // not a producer, so naming it here would fabricate the
                    // same role the shared-external-contract classification
                    // exists to remove.
                    let mut path_matches = mount_graph
                        .find_exact_path_matches_any_method(&lookup_url, &normalizer)
                        .unwrap_or_default();
                    path_matches.retain(|endpoint| {
                        endpoint.evidence == carrick_match::MatchEvidence::RouteDefinition
                    });
                    if path_matches.is_empty() {
                        missing
                            .entry((method.to_string(), miss_path))
                            .or_default()
                            .insert(call_site);
                    } else {
                        let candidate = mismatch_candidates
                            .entry((method.to_string(), miss_path))
                            .or_default();
                        candidate.call_sites.insert(call_site);
                        for endpoint in path_matches {
                            candidate.producers.push((
                                endpoint.method.to_uppercase(),
                                format!("{}:{}", endpoint.method, endpoint.full_path),
                                endpoint.full_path.clone(),
                            ));
                        }
                    }
                }
            }
        }

        // Resolve wrong-verb candidates now that the verified set is
        // complete: prefer a producer no consumer matched — the wrong verb
        // most plausibly aims at it, and it would otherwise double-report as
        // an orphan — falling back to the first in sorted order when every
        // exact-path producer is verified. Only the chosen producer is
        // suppressed from the orphan list. Keyed by the producer's DECLARED
        // path so N consumer spellings of one route collapse into one risk.
        let mut method_mismatches: BTreeMap<(String, String, String), BTreeSet<String>> =
            BTreeMap::new();
        for ((method, _consumer_path), mut candidate) in mismatch_candidates {
            candidate.producers.sort();
            candidate.producers.dedup();
            let Some((expected, producer_key, declared_path)) = candidate
                .producers
                .iter()
                .find(|(_, key, _)| !matched_endpoints.contains(key))
                .or_else(|| candidate.producers.first())
                .cloned()
            else {
                continue;
            };
            method_mismatched_producers.insert(producer_key);
            method_mismatches
                .entry((method, declared_path, expected))
                .or_default()
                .extend(candidate.call_sites);
        }

        // Findings order: risks (method mismatches) first, then gaps, then
        // advisories — mirrors the report's section order.
        let mut findings: Vec<Finding> = Vec::new();
        for ((method, path, expected), sites) in method_mismatches {
            findings.push(Finding::method_mismatch(
                method,
                path,
                None,
                sites.into_iter().collect(),
                expected,
            ));
        }
        for ((method, path), sites) in missing {
            findings.push(Finding::missing_endpoint(
                method,
                path,
                None,
                sites.into_iter().collect(),
            ));
        }

        // Find orphaned endpoints (not matched by any call), and capture
        // verified matches as (method, path, provenance) tuples for the
        // formatter.
        let mut verified: Vec<VerifiedEndpointEntry> = Vec::new();
        for endpoint in mount_graph.get_resolved_endpoints() {
            // Call-site-evidence entries are not producers (#379): matched or
            // not, they are neither "verified endpoints" (nothing was served)
            // nor "orphaned endpoints" (nothing was defined). Their matches
            // surface as shared-external-contract groups instead.
            if endpoint.evidence == carrick_match::MatchEvidence::CallSite {
                continue;
            }
            let key = format!("{}:{}", endpoint.method, endpoint.full_path);
            if matched_endpoints.contains(&key) {
                verified.push(VerifiedEndpointEntry::new(
                    endpoint.method.clone(),
                    endpoint.full_path.clone(),
                    endpoint.provenance,
                ));
            } else if !method_mismatched_producers.contains(&key)
                && !carrick_match::is_catch_all_path(&endpoint.full_path)
                && carrick_match::path_literal_specificity(&endpoint.full_path) > 0
            {
                // A producer already surfaced in a method-mismatch risk is
                // neither verified nor re-reported as orphaned. Nor is a
                // catch-all mount (`/*`, `/api/**`) or a route with no
                // literal segment at all (`/:slug`): those absorb calls by
                // design (#381), so "no consumer matched" is not a meaningful
                // observation — they are routing infrastructure, not an
                // unconsumed contract.
                findings.push(
                    Finding::orphaned_endpoint(
                        endpoint.method.clone(),
                        endpoint.full_path.clone(),
                        // Prefer the monorepo service name, falling back to the repo
                        // (matches the cloud's service_name ?? repo_name convention).
                        endpoint
                            .service_name
                            .clone()
                            .or_else(|| endpoint.repo_name.clone()),
                    )
                    .with_producer_provenance(endpoint.provenance),
                );
            }
        }
        for ((env_var, method, path), sites) in env_var_calls {
            findings.push(Finding::env_var_call(
                method,
                path,
                env_var,
                sites.into_iter().collect(),
            ));
        }
        // Shared-external-contract groups (#379): only groups spanning ≥2
        // repos are reported — "N repos encode the same external contract" is
        // signal; a single repo's own encoding (every SDK operation would
        // qualify) is noise, and unattributed single-repo runs collect no repo
        // ids at all.
        for ((method, path), (repos, sites)) in shared_contract_groups {
            if repos.len() < 2 {
                continue;
            }
            findings.push(Finding::shared_external_contract(
                method,
                path,
                repos.into_iter().collect(),
                sites.into_iter().collect(),
            ));
        }
        sort_dedup_verified(&mut verified);

        // Deterministic order for the projection (mirrors verified.sort()): the
        // matcher iterates calls/endpoints in a non-deterministic order, so sort
        // and dedup the captured edges on their identity tuple. The non-HTTP
        // edges added later in `get_results` are re-sorted there over the
        // combined set, so this is the HTTP-only first pass.
        sort_dedup_cross_repo_matches(&mut cross_repo_matches);

        (findings, verified, cross_repo_matches)
    }

    /// Build a [`CrossRepoMatch`] from a matched consumer call + producer
    /// endpoint. Both sides carry the `service_name ?? repo_name` id (#368).
    /// Returns `None` only when the consumer cannot be attributed (no
    /// `service_name`/`repo_name` tag in the merged graph for this call) — an
    /// edge without both ids is not useful to the scorer.
    ///
    /// `match_score` is `1.0`: every edge captured here is an exact
    /// normalized-key match (there is no finer scorer yet). `type_compatible`
    /// is left `None` here; `get_results` overlays the per-endpoint compat
    /// verdict after type checking has (or has not) run.
    fn build_cross_repo_match(
        call: &ApiEndpointDetails,
        method: &str,
        target: &str,
        normalized_consumer_path: &str,
        endpoint: &crate::mount_graph::ResolvedEndpoint,
        consumer_repo_by_call: &HashMap<(String, String, String), String>,
    ) -> Option<CrossRepoMatch> {
        let producer_repo = endpoint
            .service_name
            .clone()
            .or_else(|| endpoint.repo_name.clone())?;
        let lookup_key = (
            method.to_uppercase(),
            target.to_string(),
            call.file_path.display().to_string(),
        );
        let consumer_repo = consumer_repo_by_call.get(&lookup_key).cloned()?;

        let producer_key = OperationKey::http(&endpoint.method, endpoint.full_path.clone());
        let consumer_key = OperationKey::http(method, normalized_consumer_path.to_string());

        Some(CrossRepoMatch {
            producer_repo,
            producer_key: producer_key.canonical(),
            consumer_repo,
            consumer_key: consumer_key.canonical(),
            // Classified by evidence kind (#379): the consumer side is always
            // a call site here; the producer side is whatever evidence backs
            // the matched endpoint entry.
            relationship: carrick_match::classify_relationship(
                endpoint.evidence,
                carrick_match::MatchEvidence::CallSite,
            ),
            // The consumer call's source location — the per-pair join key for the
            // compat verdict (#260). Shares the consumer manifest entry's source
            // (both come from this call's `file_location`), so the overlay can
            // attribute the outcome's consumer location to THIS edge.
            consumer_location: Some(call.file_path.display().to_string()),
            match_score: 1.0,
            type_compatible: None,
            type_verdict: None,
            mismatch_reason: None,
            producer_provenance: endpoint.provenance,
        })
    }

    /// Match consumers against producers of a protocol whose operations have
    /// exact key identity (GraphQL fields, socket events) — no URL or mount
    /// hierarchy to normalize. Returns `(findings, verified,
    /// cross_repo_matches)`.
    ///
    /// If no producer of the protocol is indexed anywhere, consumers are
    /// skipped silently: the producing service may simply not be scanned,
    /// and guessing would create false "missing endpoint" noise. Unconsumed
    /// producers are reported as orphans, the same soft signal REST orphans
    /// get.
    fn analyze_exact_key_matches(&self, protocol: crate::operation::Protocol) -> MatcherOutput {
        let producer_keys: HashSet<&OperationKey> = self
            .endpoints
            .iter()
            .filter(|endpoint| endpoint.key.protocol() == protocol)
            .map(|endpoint| &endpoint.key)
            .collect();
        if producer_keys.is_empty() {
            return (Vec::new(), Vec::new(), Vec::new());
        }

        // Producer repo ids (service_name ?? repo_name) per canonical key, so a
        // matched consumer can be attributed for a `CrossRepoMatch`. A key with
        // no repo identity yields no edge (the same guard the HTTP path applies).
        // Multiple producers can legitimately share one exact key — two services
        // exposing the same GraphQL field, or several listeners for one socket
        // event — and exact-key matching has no URL to disambiguate them. So
        // collect ALL distinct producer repos (a `BTreeMap` for deterministic
        // order) and emit one edge per producer↔consumer pair, rather than
        // arbitrarily keeping the first by iteration order. Each repo carries
        // the producer's provenance; when one repo has several producers on the
        // same key, route-wins (`EndpointProvenance::min`, #380).
        let mut producer_repos_by_key: HashMap<
            String,
            std::collections::BTreeMap<String, crate::operation::EndpointProvenance>,
        > = HashMap::new();
        for endpoint in &self.endpoints {
            if endpoint.key.protocol() != protocol {
                continue;
            }
            if let Some(repo) = endpoint
                .service_name
                .clone()
                .or_else(|| endpoint.repo_name.clone())
            {
                producer_repos_by_key
                    .entry(endpoint.key.canonical())
                    .or_default()
                    .entry(repo)
                    .and_modify(|provenance| *provenance = (*provenance).min(endpoint.provenance))
                    .or_insert(endpoint.provenance);
            }
        }

        // (label, name) → call sites, BTree-keyed so same-op call sites
        // collapse into one finding in deterministic order.
        let mut missing: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
        let mut cross_repo_matches: Vec<CrossRepoMatch> = Vec::new();
        let mut matched: HashSet<&OperationKey> = HashSet::new();
        let mut seen_calls = HashSet::new();
        for call in &self.calls {
            if call.key.protocol() != protocol {
                continue;
            }
            let dedup = format!("{}:{}", call.key.canonical(), call.file_path.display());
            if !seen_calls.insert(dedup) {
                continue;
            }
            if producer_keys.contains(&call.key) {
                matched.insert(&call.key);
                // Emit the cross-repo edge. Exact-key protocols share one key on
                // both sides, so producer_key == consumer_key. For sockets the
                // producer is the listener (an endpoint) and the consumer is the
                // emitter (a call); this attribution follows directly from which
                // side the op sits on. `type_compatible` is left `None` —
                // `overlay_compat_verdicts` fills it in if compat ran.
                let consumer_repo = call.service_name.clone().or_else(|| call.repo_name.clone());
                let canonical = call.key.canonical();
                if let (Some(producer_repos), Some(consumer_repo)) =
                    (producer_repos_by_key.get(&canonical), consumer_repo)
                {
                    for (producer_repo, producer_provenance) in producer_repos {
                        // Same-repo publisher↔subscriber (or listener↔emitter) is
                        // an intra-repo self-loop, not a cross-repo contract edge.
                        // Drop it structurally on repo identity alone (never on
                        // topic-name or library patterns) so a dead-letter retry
                        // loop or any in-process fan-out doesn't surface as a
                        // self-match. A key that OTHER repos also participate on
                        // still emits its genuine cross-repo edges — only the
                        // producer==consumer pair is skipped.
                        if *producer_repo == consumer_repo {
                            continue;
                        }
                        cross_repo_matches.push(CrossRepoMatch {
                            producer_repo: producer_repo.clone(),
                            producer_key: canonical.clone(),
                            consumer_repo: consumer_repo.clone(),
                            consumer_key: canonical.clone(),
                            // Exact-key producers are definition-side entries
                            // (SDL root fields, socket listeners, pub/sub
                            // subscribers), so the pair is a real
                            // producer/consumer edge.
                            relationship: carrick_match::classify_relationship(
                                carrick_match::MatchEvidence::RouteDefinition,
                                carrick_match::MatchEvidence::CallSite,
                            ),
                            // Exact-key protocols (GraphQL/socket) ARE
                            // type-checked, so this consumer location feeds the
                            // compat overlay (`apply_pair_outcomes`) and also
                            // keeps the dedup identity precise.
                            consumer_location: Some(call.file_path.display().to_string()),
                            match_score: 1.0,
                            type_compatible: None,
                            type_verdict: None,
                            mismatch_reason: None,
                            producer_provenance: *producer_provenance,
                        });
                    }
                }
            } else {
                let (label, name) = call.key.display_labels();
                missing
                    .entry((label, name))
                    .or_default()
                    .insert(call.file_path.display().to_string());
            }
        }

        let mut findings: Vec<Finding> = missing
            .into_iter()
            .map(|((label, name), sites)| {
                Finding::missing_endpoint(label, name, None, sites.into_iter().collect())
            })
            .collect();
        let mut verified = Vec::new();
        let mut seen_producers = HashSet::new();
        for endpoint in &self.endpoints {
            if endpoint.key.protocol() != protocol {
                continue;
            }
            if !seen_producers.insert(endpoint.key.canonical()) {
                continue;
            }
            let (label, name) = endpoint.key.display_labels();
            if matched.contains(&endpoint.key) {
                verified.push(VerifiedEndpointEntry::new(label, name, endpoint.provenance));
            } else {
                // GraphQL/socket producers are not repo-tagged at this layer, so
                // the owning service is unknown.
                findings.push(
                    Finding::orphaned_endpoint(label, name, None)
                        .with_producer_provenance(endpoint.provenance),
                );
            }
        }
        sort_dedup_verified(&mut verified);

        (findings, verified, cross_repo_matches)
    }

    pub fn compute_full_paths_for_endpoint(
        endpoint: &ApiEndpointDetails,
        mounts: &[Mount],
        _apps: &std::collections::HashMap<String, AppContext>,
    ) -> Vec<String> {
        let mut results = Vec::new();

        // Defensive: skip endpoints with no owner
        let mut owner = match &endpoint.owner {
            Some(owner) => owner.clone(),
            None => return results,
        };

        // Mount-prefix resolution only applies to HTTP routes
        let Some((_, route)) = endpoint.key.as_http() else {
            return results;
        };
        let mut path = route.to_string();
        let mut visited = std::collections::HashSet::new();

        // Walk up the mount chain, prepending prefixes
        loop {
            // Prevent cycles
            if !visited.insert(owner.clone()) {
                break;
            }

            // Find the mount where this owner is the child
            if let Some(mount) = mounts.iter().find(|m| m.child == owner) {
                // Prepend the prefix
                path = join_prefix_and_path(&mount.prefix, &path);
                // Move up to the parent
                owner = mount.parent.clone();
                // If the parent is an app, we're done
                if let OwnerType::App(_) = owner {
                    results.push(path.clone());
                    break;
                }
            } else {
                // If owner is an app, just push the path
                if let OwnerType::App(_) = owner {
                    results.push(path.clone());
                }
                // No more parents, stop
                break;
            }
        }

        results
    }

    pub fn resolve_all_endpoint_paths(
        &self,
        endpoints: &[ApiEndpointDetails],
        mounts: &[Mount],
        apps: &std::collections::HashMap<String, AppContext>,
    ) -> Vec<ApiEndpointDetails> {
        let mut new_endpoints = Vec::new();
        for endpoint in endpoints {
            let Some((method, _)) = endpoint.key.as_http() else {
                new_endpoints.push(endpoint.clone());
                continue;
            };
            let method = method.to_string();
            let full_paths = Self::compute_full_paths_for_endpoint(endpoint, mounts, apps);
            for path in full_paths {
                let mut ep = endpoint.clone();
                ep.key = OperationKey::http(&method, path);
                new_endpoints.push(ep);
            }
        }
        new_endpoints
    }

    fn normalize_route_params(&self, route: &str) -> String {
        // Replace all parameter placeholders with a consistent name.
        ROUTE_PARAM_RE.replace_all(route, "{param}").to_string()
    }

    pub fn build_endpoint_router(&mut self) {
        let mut router = matchit::Router::new();

        // Use a HashMap to collect all endpoints by path before inserting into router
        let mut path_to_endpoints: HashMap<String, Vec<(String, String)>> = HashMap::new();

        for endpoint in &self.endpoints {
            let Some((method, route)) = endpoint.key.as_http() else {
                continue;
            };
            let normalized_route = self.normalize_route_params(route);

            path_to_endpoints
                .entry(normalized_route)
                .or_default()
                .push((route.to_string(), method.to_string()));
        }

        debug!("Unique endpoint paths: {}", path_to_endpoints.len());

        // Now insert each unique path once, with a collection of route-method pairs
        for (path, route_methods) in path_to_endpoints {
            if let Err(e) = router.insert(&path, route_methods) {
                warn!("Could not add route to router: {}", e);
            }
        }

        self.endpoint_router = Some(router);
    }

    pub fn get_results(&self) -> ApiAnalysisResult {
        // Framework-agnostic analysis using mount graph (required)
        let mount_graph = self.mount_graph.as_ref()
            .expect("Mount graph must be set before calling get_results(). This is a framework-agnostic requirement.");

        let (matcher_findings, mut verified_endpoints, mut cross_repo_matches) =
            self.analyze_matches_with_mount_graph(mount_graph);
        // Findings order mirrors the report: contract risks first (type
        // mismatches, then the matchers' method mismatches), then gaps, then
        // advisories.
        let mut findings = self.get_type_mismatch_findings();
        findings.extend(matcher_findings);
        for protocol in [
            crate::operation::Protocol::Graphql,
            crate::operation::Protocol::Websocket,
            crate::operation::Protocol::Pubsub,
        ] {
            let (protocol_findings, protocol_verified, protocol_cross_repo_matches) =
                self.analyze_exact_key_matches(protocol);
            findings.extend(protocol_findings);
            verified_endpoints.extend(protocol_verified);
            cross_repo_matches.extend(protocol_cross_repo_matches);
        }
        // Route-wins per (method, path) over the combined HTTP + non-HTTP set.
        sort_dedup_verified(&mut verified_endpoints);
        // Re-sort/dedup over the combined HTTP + non-HTTP edge set so the final
        // ordering is stable regardless of which matcher produced an edge.
        sort_dedup_cross_repo_matches(&mut cross_repo_matches);
        // Sorted by package so the findings (and the eval projection's source
        // data) don't inherit HashMap iteration order.
        let mut dependency_conflicts = self.analyze_dependencies();
        dependency_conflicts.sort_by(|a, b| a.package_name.cmp(&b.package_name));
        findings.extend(dependency_conflicts.iter().map(dependency_conflict_finding));
        // Collapse byte-identical rows (#334) after every source has appended;
        // order-preserving, so the risks-then-gaps-then-advisories report
        // ordering above survives.
        dedup_findings(&mut findings);

        // Overlay the per-pair type-compat verdict onto the captured edges. The
        // verdict is keyed by the producer's (METHOD, full_path) AND the consumer's
        // source location, so each (producer, consumer) edge gets its own verdict
        // rather than sharing the producer's first verdict across all consumers
        // (#260).
        //
        // `type_compatible` stays `None` when compat was not evaluated (no
        // sidecar, so no pair outcomes were computed). This `None` is
        // load-bearing: the scorer must never read absent compat data as
        // "compatible".
        self.overlay_compat_verdicts(&mut cross_repo_matches);

        // Join the per-pair verdicts back onto each verified producer endpoint,
        // so the "Verified" surfaces (PR comment #455, terminal report #456) can
        // bucket rows honestly instead of blanket-claiming compiler comparison.
        // Runs AFTER the overlay so the edges carry their verdicts.
        attach_verified_type_verdicts(&mut verified_endpoints, &cross_repo_matches);

        let detected_graphql_libraries = filter_graphql_libraries(&self.detected_data_fetchers);
        let graphql_operations_indexed = self
            .endpoints
            .iter()
            .chain(self.calls.iter())
            .any(|details| details.key.protocol() == crate::operation::Protocol::Graphql);

        // Canonical ordering: the analyzer collects endpoints/calls in a
        // non-deterministic order (HashMap iteration + concurrent file joins
        // upstream), so two scans of the same repo can emit the same set in a
        // different sequence. Sort here, at the single aggregation point, so
        // *every* consumer (PR comment, dashboard upload, eval projection, the
        // cassette hard gate) sees a stable order. Keyed on the canonical
        // operation key then the `<file>:<line>` location to fully disambiguate
        // same-key operations. Mirrors the adjacent `verified_endpoints.sort()`.
        // The key allocates (canonical() + owned path string), so use
        // sort_by_cached_key: it computes each element's key once, not once per
        // comparison.
        let sort_key = |d: &ApiEndpointDetails| {
            (
                d.key.canonical(),
                d.file_path.to_string_lossy().into_owned(),
            )
        };
        let mut endpoints = self.endpoints.clone();
        let mut calls = self.calls.clone();
        endpoints.sort_by_cached_key(&sort_key);
        calls.sort_by_cached_key(&sort_key);

        ApiAnalysisResult {
            endpoints,
            calls,
            findings,
            dependency_conflicts,
            verified_endpoints,
            detected_graphql_libraries,
            graphql_operations_indexed,
            cross_repo_matches,
        }
    }

    /// Raw call targets for the eval projection, keyed by the same identity a
    /// consumer `ApiEndpointDetails` carries: `(canonical key, file location)`.
    /// The canonical key path is host-free by design (`consumer_call_path`
    /// strips a literal origin so the match key can join a producer route), so
    /// the raw source target (`http://comment-service/api/comments?...`) is the
    /// only place the host survives. The Tier-A scorer's `host_contains`
    /// labels assert against it; nothing in the product path reads this.
    pub fn call_raw_targets(&self) -> HashMap<(String, String), String> {
        self.mount_graph
            .as_ref()
            .map(|g| {
                g.get_data_calls()
                    .iter()
                    .map(|c| {
                        (
                            (
                                OperationKey::http(&c.method, c.canonical_path.clone()).canonical(),
                                c.file_location.clone(),
                            ),
                            c.target_url.clone(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Overlay the type-compatibility verdict onto each captured cross-repo
    /// edge, keyed by the producer's `(METHOD, identity)` AND the consumer's
    /// source location — so each `(producer, consumer)` pair gets ITS OWN
    /// verdict (#260).
    ///
    /// When no pair outcomes were stored (the engine never ran `check_v2` —
    /// sidecar unavailable, or the run failed before checking), every edge
    /// keeps `type_compatible: None` (load-bearing — see [`CrossRepoMatch`]).
    fn overlay_compat_verdicts(&self, matches: &mut [CrossRepoMatch]) {
        let Some(outcomes) = self.pair_outcomes.as_ref() else {
            // Compat was not evaluated for this run — leave every edge `None`.
            return;
        };
        apply_pair_outcomes(outcomes, matches);
    }

    /// Alias -> human display name (`GET /users → Response`) from the merged
    /// manifests, for scrubbing synthetic `Endpoint_<hash>` names out of
    /// user-facing type/error strings.
    fn build_display_name_map(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for entry in &self.type_manifests {
            let type_kind = match entry.type_kind {
                crate::cloud_storage::ManifestTypeKind::Request => "request",
                crate::cloud_storage::ManifestTypeKind::Response => "response",
            };
            let display = crate::type_manifest::build_display_name(&entry.key, type_kind);
            map.insert(entry.type_alias.clone(), display);
        }
        map
    }

    /// Project the incompatible v2 pair outcomes into typed
    /// [`Finding::TypeMismatch`]s. Returns empty when compat was not
    /// evaluated for this run. The producer/consumer type labels come from
    /// the manifest entries (real anchor symbol, else the expanded
    /// definition, else the display name); the detail is the scrubbed
    /// compiler diagnostic.
    fn get_type_mismatch_findings(&self) -> Vec<Finding> {
        let Some(outcomes) = self.pair_outcomes.as_ref() else {
            return Vec::new();
        };
        let display_names = self.build_display_name_map();
        let manifest_by_alias: HashMap<&str, &crate::cloud_storage::TypeManifestEntry> = self
            .type_manifests
            .iter()
            .map(|e| (e.type_alias.as_str(), e))
            .collect();
        let type_label = |alias: &str| -> String {
            if let Some(entry) = manifest_by_alias.get(alias) {
                if let Some(symbol) = entry.primary_type_symbol.as_deref() {
                    return symbol.to_string();
                }
                if let Some(expanded) = entry.expanded_definition.as_deref() {
                    return expanded.to_string();
                }
            }
            display_names
                .get(alias)
                .cloned()
                .unwrap_or_else(|| alias.to_string())
        };

        outcomes
            .iter()
            .filter(|o| o.bucket == crate::services::type_sidecar::VerdictBucket::Incompatible)
            .map(|outcome| {
                let method = outcome.pseudo_method.clone();
                let path = outcome.identity.clone();
                // The consumer call site is the actionable location — it's
                // where the broken read/write happens. In CI the manifest
                // carries the runner's absolute checkout path; strip it so
                // the risk row cites the repo-relative file.
                let location = format!("{}:{}", outcome.consumer_file, outcome.consumer_line);
                let call_sites = vec![strip_ci_workspace_prefix(&location).to_string()];
                let producer_provenance = self.producer_provenance_for(&method, &path);
                let detail = outcome
                    .diagnostic
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "producer and consumer types are incompatible".to_string());
                Finding::type_mismatch(
                    method,
                    path,
                    None,
                    call_sites,
                    self.clean_type_string(&type_label(&outcome.producer_alias), &display_names),
                    self.clean_type_string(&type_label(&outcome.consumer_alias), &display_names),
                    &self.clean_error_message(&detail, &display_names),
                )
                .with_producer_provenance(producer_provenance)
            })
            .collect()
    }

    /// Provenance of the producer behind a compat verdict, joined against
    /// the mount graph's resolved endpoints by `(METHOD, path)` — the same
    /// param-name-agnostic path normalization the compat overlay uses. When
    /// several producers share the key, route-wins (`.min()`); an unmatched
    /// key (non-HTTP labels, no mount graph) conservatively reads as `Route`.
    fn producer_provenance_for(
        &self,
        method: &str,
        path: &str,
    ) -> crate::operation::EndpointProvenance {
        let Some(mount_graph) = self.mount_graph.as_ref() else {
            return Default::default();
        };
        let want = normalize_compat_path(path);
        mount_graph
            .get_resolved_endpoints()
            .iter()
            .filter(|endpoint| {
                endpoint.method.eq_ignore_ascii_case(method)
                    && normalize_compat_path(&endpoint.full_path) == want
            })
            .map(|endpoint| endpoint.provenance)
            .min()
            .unwrap_or_default()
    }

    fn clean_type_string(&self, type_str: &str, display_names: &HashMap<String, String>) -> String {
        // Remove absolute paths from import statements, keeping only the relative part
        let mut cleaned = IMPORT_PATH_RE
            .replace_all(type_str, |caps: &regex::Captures| {
                let type_name = &caps[2];
                // Replace hash-based type aliases with display names
                if let Some(display) = display_names.get(type_name) {
                    return display.clone();
                }
                let path = &caps[1];
                // Extract just the filename without path for readability
                if let Some(filename) = path.split('/').next_back() {
                    format!("{}.{}", filename, type_name)
                } else {
                    format!("{}.{}", path, type_name)
                }
            })
            .to_string();

        // Also replace standalone hash-based type aliases (not inside import())
        for (alias, display) in display_names {
            if cleaned.contains(alias.as_str()) {
                cleaned = cleaned.replace(alias.as_str(), display);
            }
        }

        // Simplify Array<T> to T[]
        cleaned = ARRAY_GENERIC_RE.replace_all(&cleaned, "$1[]").to_string();

        cleaned
    }

    fn clean_error_message(&self, error: &str, display_names: &HashMap<String, String>) -> String {
        let mut cleaned = error
            .replace("Type '", "")
            .replace(
                "' is missing the following properties from type '",
                " missing properties from ",
            )
            .replace("': ", ": ")
            .replace("' is not assignable to type '", " not assignable to ")
            .replace("'.", "");

        // A diagnostic ending mid-sentence ("... type 'Y'") has no trailing
        // period, so the "'." replacement above never fires on the closing
        // quote; drop the unbalanced closer here.
        if let Some(stripped) = cleaned.strip_suffix('\'') {
            cleaned = stripped.to_string();
        }

        // Replace hash-based type aliases in error messages
        for (alias, display) in display_names {
            if cleaned.contains(alias.as_str()) {
                cleaned = cleaned.replace(alias.as_str(), display);
            }
        }

        cleaned
    }

    /// Extract repository prefix from endpoint owner information
    /// Note: Currently unused but kept for future multi-repo scenarios where
    /// owner names might contain repo prefixes (format: "repo_prefix:name")
    #[allow(dead_code)]
    pub fn extract_repo_prefix_from_owner(&self, owner: &Option<OwnerType>) -> String {
        if let Some(owner) = owner {
            match owner {
                OwnerType::App(name) | OwnerType::Router(name) => {
                    // Extract repo prefix from owner name (format: "repo_prefix:name")
                    name.split(':').next().unwrap_or("default").to_string()
                }
            }
        } else {
            "default".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for #334: a duplicated producer manifest entry made ts_check
    /// emit the same mismatch once per duplicate, which reached the PR comment
    /// as byte-identical rows. Identical rows collapse at the aggregation
    /// point; findings differing in any field (call site, detail, types) are
    /// legitimately distinct and must survive.
    #[test]
    fn dedup_findings_collapses_identical_rows_only() {
        let mismatch = || {
            Finding::type_mismatch(
                "GET",
                "/api/orders/:id",
                None,
                vec!["src/user-routes.ts:12".to_string()],
                "Order[]",
                "OrderWithUser",
                "Property 'user' is missing",
            )
        };
        let other = Finding::missing_endpoint(
            "GET",
            "/api/orders/:id",
            None,
            vec!["src/user-routes.ts:30".to_string()],
        );
        let mut findings = vec![mismatch(), other.clone(), mismatch()];

        dedup_findings(&mut findings);

        assert_eq!(findings, vec![mismatch(), other]);
    }

    /// Cross-service dependency conflicts are reported only when semver-
    /// INCOMPATIBLE (a major-version spread). `zod` 3.22.0 vs 4.0.0 is a real
    /// conflict (Critical); `typescript` 5.3.0 vs 5.4.0 is a compatible same-major
    /// drift and must be suppressed — it was the false positive that pinned the
    /// xrepo-corpus-1 dependency F1 to 0.667 (precision 0.5). Non-semver pins that
    /// differ as raw strings are reported conservatively.
    #[test]
    fn dependency_conflict_reported_only_when_major_incompatible() {
        fn infos(versions: &[&str]) -> Vec<RepoPackageInfo> {
            versions
                .iter()
                .enumerate()
                .map(|(i, v)| RepoPackageInfo {
                    repo_name: format!("repo-{i}"),
                    version: (*v).to_string(),
                    source_path: PathBuf::from("package.json"),
                })
                .collect()
        }

        // zod 3.x vs 4.x — major spread → reported, Critical.
        let zod = infos(&["3.22.0", "4.0.0"]);
        assert!(Analyzer::is_reportable_conflict(&zod));
        assert!(matches!(
            Analyzer::determine_conflict_severity(&zod),
            ConflictSeverity::Critical
        ));

        // typescript 5.3 vs 5.4 — same-major minor drift → suppressed.
        assert!(!Analyzer::is_reportable_conflict(&infos(&[
            "5.3.0", "5.4.0"
        ])));
        // patch-only drift → also suppressed.
        assert!(!Analyzer::is_reportable_conflict(&infos(&[
            "1.1.1", "1.1.2"
        ])));
        // three same-major versions → suppressed.
        assert!(!Analyzer::is_reportable_conflict(&infos(&[
            "18.2.0", "18.3.0", "18.3.1"
        ])));

        // A non-semver pin that differs as a raw string → reported conservatively
        // (has_conflicts upstream already established the strings differ).
        assert!(Analyzer::is_reportable_conflict(&infos(&[
            "workspace:*",
            "1.0.0"
        ])));
    }

    #[test]
    fn route_shape_drops_non_route_values() {
        // Exact values the file-analyzer LLM mis-emitted as call targets on the
        // carrick-cloud run (SDK ops, bare identifiers, member/call expressions,
        // literals, leaked `||`).
        let dropped = [
            "DynamoDB:PutItem",
            "DynamoDB:Query",
            "DynamoDB.TransactWriteItems",
            "DynamoDBClient",
            "DynamoDB",
            "GetCommand",
            "QueryCommand",
            "new",
            "null",
            ".",
            "unknown",
            "query",
            "request",
            "request.formData()",
            "res.json()",
            ".json()",
            "result.response.text()",
            "ordersResp",
            "listRes",
            "serviceName",
            "params.service",
            "getAllRepoData",
            "search_by_intent",
            "scaffold",
            "CarrickApiKeys",
            "user#${auth.user_id}",
            "${API_KEYS_TABLE}||CarrickApiKeys",
            "",
        ];
        for route in dropped {
            assert!(
                !is_valid_route_shape(route),
                "expected route to be dropped: {route:?}"
            );
        }
    }

    #[test]
    fn route_shape_keeps_real_routes() {
        let kept = [
            "/mcp",
            "/getAllRepoData",
            "/findService",
            "/users/${userId}",
            "/api/orders/:id",
            "${GITHUB_API}/repos/:owner/:repo",
            "${RESEND_ENDPOINT}/",
            "${lambdaUrl}",
            "${process.env.API_BASE}/users",
            "https://api.github.com/repos/owner/repo",
            "http://localhost:3000/health",
        ];
        for route in kept {
            assert!(
                is_valid_route_shape(route),
                "expected route to be kept: {route:?}"
            );
        }
    }

    /// carrick#399: the file-analyzer sometimes renders a call target verbatim
    /// with its inline fallback; the collapse must yield the exact clean form
    /// so both renderings of the same call site produce one canonical key.
    #[test]
    fn normalize_env_fallback_collapses_inline_fallbacks() {
        let cases = [
            // The exact verbatim rendering from the #399 evidence
            // (eval run 29677844107).
            (
                r#"${AUDIT_WEBHOOK_URL ?? "http://localhost:3099"}/audit/events"#,
                "${AUDIT_WEBHOOK_URL}/audit/events",
            ),
            ("${A || 'x'}/p", "${A}/p"),
            (
                r#"${process.env.AUDIT_WEBHOOK_URL ?? "http://localhost:3099"}/audit/events"#,
                "${process.env.AUDIT_WEBHOOK_URL}/audit/events",
            ),
            // Dotted config-alias form (#218 shape) with a fallback.
            (
                r#"${config.catalogUrl ?? "http://localhost:4001"}/api/products"#,
                "${config.catalogUrl}/api/products",
            ),
            // Fallback expression carrying a nested interpolation inside a
            // backtick template: the inner `${PORT}` must not end the scan.
            ("${BASE ?? `http://localhost:${PORT}`}/p", "${BASE}/p"),
            // Quoted `}` and `||` inside the fallback string are inert.
            (r#"${BASE ?? "a||b}c"}/p"#, "${BASE}/p"),
            // Escaped quote inside the fallback string does not end the
            // string early, so the `}` and `||` behind it stay inert.
            (r#"${BASE ?? "it\"s}fine"}/p"#, "${BASE}/p"),
            (r#"${BASE ?? "\" || junk"}/p"#, "${BASE}/p"),
            // Backtick-wrapped template with a mid-path param: only the
            // fallback interpolation collapses, the `${id}` param survives.
            (
                "`${ORDERS_BASE || 'http://localhost:3001'}/orders/${id}`",
                "`${ORDERS_BASE}/orders/${id}`",
            ),
            // Mid-path param fallback collapses to the plain param form.
            ("/orders/${id ?? 0}", "/orders/${id}"),
            // Chained fallback keeps the first reference.
            ("${A ?? B ?? C}/p", "${A}/p"),
        ];
        for (raw, want) in cases {
            assert_eq!(
                normalize_env_fallback_target(raw).as_deref(),
                Some(want),
                "for {raw:?}"
            );
        }
        // The collapsed forms are valid route shapes; the verbatim forms are
        // not (that rejection was the #359 recall flake).
        assert!(!is_valid_route_shape(
            r#"${AUDIT_WEBHOOK_URL ?? "http://localhost:3099"}/audit/events"#
        ));
        assert!(is_valid_route_shape("${AUDIT_WEBHOOK_URL}/audit/events"));
        assert!(is_valid_route_shape("${A}/p"));
        assert!(is_valid_route_shape(
            "${process.env.AUDIT_WEBHOOK_URL}/audit/events"
        ));
    }

    /// The whitespace guard in `is_valid_route_shape` exists for a reason:
    /// only clean-reference fallbacks become valid. Everything else stays
    /// verbatim and stays rejected.
    #[test]
    fn normalize_env_fallback_leaves_non_fallback_expressions_rejected() {
        let untouched_and_rejected = [
            // Plain whitespace junk: no fallback operator at all.
            "${a b}/p",
            // Call-expression left-hand side is not an env reference.
            r#"${getUrl() ?? "x"}/p"#,
            // Concatenation is not a fallback.
            "${base + path}/p",
            // Optional chaining on the left-hand side is not a clean reference.
            "${cfg?.url ?? 'x'}/p",
            // Fallback with an empty right-hand side is not collapsible.
            "${A ??}/p",
            // Operator OUTSIDE the interpolation (historical dropped case).
            "${API_KEYS_TABLE}||CarrickApiKeys",
        ];
        for target in untouched_and_rejected {
            assert_eq!(
                normalize_env_fallback_target(target),
                None,
                "expected no normalization for {target:?}"
            );
            assert!(
                !is_valid_route_shape(target),
                "expected route to stay dropped: {target:?}"
            );
        }
        // Already-clean targets are untouched (no-op, not Some(identical)).
        for target in ["${AUDIT_WEBHOOK_URL}/audit/events", "/plain/path"] {
            assert_eq!(normalize_env_fallback_target(target), None);
        }
    }

    /// Acceptance from carrick#399: feeding the run-29677844107 verbatim target
    /// through the consumer-call path yields canonical `/audit/events` with
    /// env-var base `AUDIT_WEBHOOK_URL` — byte-identical to the clean
    /// `${AUDIT_WEBHOOK_URL}/audit/events` rendering's key.
    #[test]
    fn normalized_fallback_target_produces_the_clean_canonical_key() {
        let config = crate::config::Config {
            internal_env_vars: ["AUDIT_WEBHOOK_URL"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            ..crate::config::Config::default()
        };
        let normalizer = crate::url_normalizer::UrlNormalizer::new(&config);

        let verbatim = r#"${AUDIT_WEBHOOK_URL ?? "http://localhost:3099"}/audit/events"#;
        let clean = "${AUDIT_WEBHOOK_URL}/audit/events";

        let normalized = normalize_env_fallback_target(verbatim).expect("collapses");
        assert!(is_valid_route_shape(&normalized));
        assert_eq!(
            normalizer.consumer_call_path(&normalized),
            normalizer.consumer_call_path(clean)
        );
        assert_eq!(normalizer.consumer_call_path(&normalized), "/audit/events");
        assert_eq!(
            Analyzer::extract_env_var_name(&normalized),
            "AUDIT_WEBHOOK_URL"
        );
    }

    #[test]
    fn test_filter_graphql_libraries() {
        let data_fetchers = vec![
            "axios".to_string(),
            "graphql-request".to_string(),
            "@apollo/client".to_string(),
            "urql".to_string(),
            "got".to_string(),
            "node-fetch".to_string(),
            "@urql/core".to_string(),
            "relay-runtime".to_string(),
        ];
        let mut found = filter_graphql_libraries(&data_fetchers);
        found.sort();
        assert_eq!(
            found,
            vec![
                "@apollo/client".to_string(),
                "@urql/core".to_string(),
                "graphql-request".to_string(),
                "relay-runtime".to_string(),
                "urql".to_string(),
            ]
        );
    }

    #[test]
    fn test_filter_graphql_libraries_empty_when_rest_only() {
        let data_fetchers = vec!["axios".to_string(), "fetch".to_string(), "got".to_string()];
        let found = filter_graphql_libraries(&data_fetchers);
        assert!(found.is_empty());
    }

    #[test]
    fn test_sanitize_route_colon_params() {
        // Standard :param style path parameters
        assert_eq!(
            Analyzer::sanitize_route_for_dynamic_paths("/users/:id"),
            "UsersById"
        );
        assert_eq!(
            Analyzer::sanitize_route_for_dynamic_paths("/users/:userId/comments"),
            "UsersByUseridComments"
        );
        assert_eq!(
            Analyzer::sanitize_route_for_dynamic_paths("/api/:id/comments/:commentId"),
            "ApiByIdCommentsByCommentid"
        );
    }

    #[test]
    fn test_sanitize_route_template_literal_params() {
        // Template literal ${param} style path parameters
        assert_eq!(
            Analyzer::sanitize_route_for_dynamic_paths("/users/${userId}"),
            "UsersByUserid"
        );
        assert_eq!(
            Analyzer::sanitize_route_for_dynamic_paths("/users/${userId}/comments"),
            "UsersByUseridComments"
        );
        assert_eq!(
            Analyzer::sanitize_route_for_dynamic_paths("/api/${postId}/comments/${commentId}"),
            "ApiByPostidCommentsByCommentid"
        );
    }

    #[test]
    fn test_sanitize_route_template_literal_with_dot_notation() {
        // Template literals with process.env or object property access
        // Should use the last part (the actual variable name)
        assert_eq!(
            Analyzer::sanitize_route_for_dynamic_paths("/orders/${process.env.ORDER_ID}"),
            "OrdersByOrderId"
        );
    }

    #[test]
    fn test_sanitize_route_mixed_params() {
        // Mix of :param and ${param} styles (unlikely but should work)
        assert_eq!(
            Analyzer::sanitize_route_for_dynamic_paths("/users/:id/posts/${postId}"),
            "UsersByIdPostsByPostid"
        );
    }

    #[test]
    fn test_sanitize_route_no_params() {
        // Paths without any parameters
        assert_eq!(
            Analyzer::sanitize_route_for_dynamic_paths("/api/users"),
            "ApiUsers"
        );
        assert_eq!(
            Analyzer::sanitize_route_for_dynamic_paths("/health"),
            "Health"
        );
    }

    #[test]
    fn test_sanitize_route_root_path() {
        assert_eq!(Analyzer::sanitize_route_for_dynamic_paths("/"), "");
    }

    #[test]
    fn test_sanitize_route_empty_segments() {
        // Should handle double slashes gracefully
        assert_eq!(
            Analyzer::sanitize_route_for_dynamic_paths("/api//users"),
            "ApiUsers"
        );
    }

    #[test]
    fn test_sanitize_route_strips_query_params() {
        // Query parameters should be stripped before processing
        assert_eq!(
            Analyzer::sanitize_route_for_dynamic_paths("/orders?userId=123"),
            "Orders"
        );
        assert_eq!(
            Analyzer::sanitize_route_for_dynamic_paths("/users/:id?include=posts"),
            "UsersById"
        );
        assert_eq!(
            Analyzer::sanitize_route_for_dynamic_paths("/api/data?page=1&limit=10"),
            "ApiData"
        );
        assert_eq!(
            Analyzer::sanitize_route_for_dynamic_paths("/orders?userId=:userId"),
            "Orders"
        );
    }

    #[test]
    fn test_to_pascal_case() {
        assert_eq!(Analyzer::to_pascal_case("userId"), "Userid");
        assert_eq!(Analyzer::to_pascal_case("user_id"), "UserId");
        assert_eq!(Analyzer::to_pascal_case("user-id"), "UserId");
        assert_eq!(Analyzer::to_pascal_case("USER"), "User");
        assert_eq!(Analyzer::to_pascal_case(""), "");
    }

    #[test]
    fn test_extract_env_var_name() {
        // ENV_VAR:NAME:/path format
        assert_eq!(
            Analyzer::extract_env_var_name("ENV_VAR:API_URL:/users"),
            "API_URL"
        );
        assert_eq!(
            Analyzer::extract_env_var_name("ENV_VAR:ORDER_SERVICE_URL:/orders"),
            "ORDER_SERVICE_URL"
        );

        // ${process.env.VAR} format
        assert_eq!(
            Analyzer::extract_env_var_name("${process.env.SERVICE_URL}/orders"),
            "SERVICE_URL"
        );
        assert_eq!(
            Analyzer::extract_env_var_name("${process.env.API_BASE}/users/123"),
            "API_BASE"
        );

        // ${VAR} format (without process.env)
        assert_eq!(
            Analyzer::extract_env_var_name("${BASE_URL}/orders"),
            "BASE_URL"
        );

        // process.env.VAR without ${}
        assert_eq!(
            Analyzer::extract_env_var_name("process.env.MY_API_URL + \"/data\""),
            "MY_API_URL"
        );

        // Unknown/fallback
        assert_eq!(Analyzer::extract_env_var_name("unknown"), "UNKNOWN_API");
        assert_eq!(Analyzer::extract_env_var_name("/users"), "UNKNOWN_API");
    }

    #[test]
    fn test_is_env_var_base_url() {
        // Should return true for env var base URLs
        assert!(Analyzer::is_env_var_base_url("ENV_VAR:API_URL:/users"));
        assert!(Analyzer::is_env_var_base_url(
            "ENV_VAR:ORDER_SERVICE_URL:/orders"
        ));
        assert!(Analyzer::is_env_var_base_url(
            "${process.env.API_URL}/users"
        ));
        assert!(Analyzer::is_env_var_base_url(
            "${process.env.SERVICE_URL}/orders"
        ));
        assert!(Analyzer::is_env_var_base_url("${API_BASE_URL}/users"));
        assert!(Analyzer::is_env_var_base_url("${ORDER_SERVICE}/orders"));
        assert!(Analyzer::is_env_var_base_url(
            "process.env.API_URL + \"/data\""
        ));

        // Should return false for path parameters (not base URL env vars)
        assert!(!Analyzer::is_env_var_base_url("/users/${userId}"));
        assert!(!Analyzer::is_env_var_base_url("/api/${version}/data"));
        assert!(!Analyzer::is_env_var_base_url("/orders/${orderId}/items"));
        assert!(!Analyzer::is_env_var_base_url("/users/:id"));
        assert!(!Analyzer::is_env_var_base_url("/api/users"));

        // Edge cases
        assert!(!Analyzer::is_env_var_base_url("${userId}")); // whole-URL opaque var, no path to classify
        // #378: a leading variable followed by a path is a base regardless of
        // casing — it must be classified before the path may match anything.
        assert!(Analyzer::is_env_var_base_url("${camelCase}/path"));
        assert!(Analyzer::is_env_var_base_url("${authHost}/oauth/token"));
        assert!(Analyzer::is_env_var_base_url("${API_V2}/users")); // UPPER_CASE with digit
    }
    fn graphql_details(key: OperationKey, file: &str) -> ApiEndpointDetails {
        ApiEndpointDetails {
            owner: None,
            key,
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: None,
            response_type: None,
            file_path: PathBuf::from(file),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
        }
    }

    /// Like [`graphql_details`] but stamps repo identity (as the cross-repo
    /// merge does), so the exact-key matcher can attribute an edge to repos.
    fn op_details_in_repo(key: OperationKey, file: &str, repo: &str) -> ApiEndpointDetails {
        ApiEndpointDetails {
            repo_name: Some(repo.to_string()),
            ..graphql_details(key, file)
        }
    }

    #[test]
    fn test_graphql_matching_verified_missing_and_orphaned() {
        use crate::operation::GraphqlOperationKind;
        let mut analyzer = Analyzer::new(Config::default());

        analyzer.endpoints.push(graphql_details(
            OperationKey::graphql(GraphqlOperationKind::Query, "user"),
            "schema.graphql:3",
        ));
        analyzer.endpoints.push(graphql_details(
            OperationKey::graphql(GraphqlOperationKind::Mutation, "createUser"),
            "schema.graphql:8",
        ));
        analyzer.calls.push(graphql_details(
            OperationKey::graphql(GraphqlOperationKind::Query, "user"),
            "client.ts:12",
        ));
        analyzer.calls.push(graphql_details(
            OperationKey::graphql(GraphqlOperationKind::Query, "orders"),
            "client.ts:20",
        ));

        let (findings, verified, _edges) =
            analyzer.analyze_exact_key_matches(crate::operation::Protocol::Graphql);

        assert_eq!(
            verified,
            vec![VerifiedEndpointEntry::new(
                "QUERY".to_string(),
                "user".to_string(),
                crate::operation::EndpointProvenance::Route
            )]
        );
        assert_eq!(
            findings,
            vec![
                // Protocol-agnostic labels: the GraphQL op kind is the
                // "method", the field the "path"; the call site travels typed.
                Finding::missing_endpoint("QUERY", "orders", None, vec!["client.ts:20".into()]),
                // GraphQL orphans are not repo-tagged at this layer.
                Finding::orphaned_endpoint("MUTATION", "createUser", None),
            ]
        );
    }

    #[test]
    fn test_graphql_consumers_silent_without_indexed_producers() {
        use crate::operation::GraphqlOperationKind;
        let mut analyzer = Analyzer::new(Config::default());

        // A consumer document but no GraphQL schema indexed anywhere: the
        // producing service may simply not be scanned — stay silent.
        analyzer.calls.push(graphql_details(
            OperationKey::graphql(GraphqlOperationKind::Query, "user"),
            "client.ts:12",
        ));

        let (findings, verified, edges) =
            analyzer.analyze_exact_key_matches(crate::operation::Protocol::Graphql);
        assert!(findings.is_empty());
        assert!(verified.is_empty());
        assert!(edges.is_empty());
    }

    #[test]
    fn test_socket_matching_is_direction_aware() {
        use crate::operation::SocketDirection;
        let mut analyzer = Analyzer::new(Config::default());

        // Server side: listens for chat:message, emits chat:broadcast.
        analyzer.endpoints.push(graphql_details(
            OperationKey::socket("chat:message", SocketDirection::ClientToServer),
            "server.ts:10",
        ));
        analyzer.calls.push(graphql_details(
            OperationKey::socket("chat:broadcast", SocketDirection::ServerToClient),
            "server.ts:11",
        ));
        // Client side: emits chat:message, listens for chat:broadcast,
        // and emits one event nobody handles.
        analyzer.calls.push(graphql_details(
            OperationKey::socket("chat:message", SocketDirection::ClientToServer),
            "client.ts:5",
        ));
        analyzer.endpoints.push(graphql_details(
            OperationKey::socket("chat:broadcast", SocketDirection::ServerToClient),
            "client.ts:6",
        ));
        analyzer.calls.push(graphql_details(
            OperationKey::socket("typing", SocketDirection::ClientToServer),
            "client.ts:9",
        ));

        let (findings, verified, _edges) =
            analyzer.analyze_exact_key_matches(crate::operation::Protocol::Websocket);

        assert_eq!(
            verified,
            vec![
                VerifiedEndpointEntry::new(
                    "CLIENT->SERVER".to_string(),
                    "chat:message".to_string(),
                    crate::operation::EndpointProvenance::Route
                ),
                VerifiedEndpointEntry::new(
                    "SERVER->CLIENT".to_string(),
                    "chat:broadcast".to_string(),
                    crate::operation::EndpointProvenance::Route
                ),
            ]
        );
        assert_eq!(
            findings,
            vec![Finding::missing_endpoint(
                "CLIENT->SERVER",
                "typing",
                None,
                vec!["client.ts:9".into()],
            )]
        );
    }

    #[test]
    fn test_exact_key_matches_emit_cross_repo_edges() {
        use crate::operation::{GraphqlOperationKind, SocketDirection};
        let mut analyzer = Analyzer::new(Config::default());

        // GraphQL: producer schema field in `gateway`, consumer document field
        // in `web-frontend`. Same operation key on both sides.
        analyzer.endpoints.push(op_details_in_repo(
            OperationKey::graphql(GraphqlOperationKind::Query, "order"),
            "schema.graphql:3",
            "gateway",
        ));
        analyzer.calls.push(op_details_in_repo(
            OperationKey::graphql(GraphqlOperationKind::Query, "order"),
            "web/lib/graphql.ts:5",
            "web-frontend",
        ));
        // Socket: the producer is the LISTENER (an endpoint) in `web-frontend`;
        // the consumer is the EMITTER (a call) in `payments-svc`. The event flows
        // payments-svc → web-frontend, but the contract producer is the listener.
        analyzer.endpoints.push(op_details_in_repo(
            OperationKey::socket("payment:settled", SocketDirection::ServerToClient),
            "web/lib/realtime.ts:8",
            "web-frontend",
        ));
        analyzer.calls.push(op_details_in_repo(
            OperationKey::socket("payment:settled", SocketDirection::ServerToClient),
            "payments/realtime/server.ts:9",
            "payments-svc",
        ));

        let (_, _, gql_edges) =
            analyzer.analyze_exact_key_matches(crate::operation::Protocol::Graphql);
        assert_eq!(gql_edges.len(), 1, "one graphql edge expected");
        let e = &gql_edges[0];
        assert_eq!(e.producer_repo, "gateway");
        assert_eq!(e.consumer_repo, "web-frontend");
        assert_eq!(e.producer_key, "graphql|query|order");
        assert_eq!(e.consumer_key, "graphql|query|order");
        assert_eq!(e.match_score, 1.0);
        // Compat is filled in later by overlay_compat_verdicts, not here.
        assert_eq!(e.type_compatible, None);

        let (_, _, socket_edges) =
            analyzer.analyze_exact_key_matches(crate::operation::Protocol::Websocket);
        assert_eq!(socket_edges.len(), 1, "one socket edge expected");
        let s = &socket_edges[0];
        // Direction-aware: listener repo is the producer, emitter repo the consumer.
        assert_eq!(s.producer_repo, "web-frontend");
        assert_eq!(s.consumer_repo, "payments-svc");
        assert_eq!(s.producer_key, "socket|SERVER->CLIENT|payment:settled");
        assert_eq!(s.consumer_key, "socket|SERVER->CLIENT|payment:settled");
    }

    #[test]
    fn pubsub_redis_exact_topic_emits_cross_repo_edge() {
        // Corpus-2 edge #4: web-dashboard SUBSCRIBES Redis `metrics.page_view`
        // (the contract producer / endpoint) while analytics-worker PUBLISHES it
        // (the consumer / call). Identity is the topic alone, so the subscriber
        // and publisher keys are equal and match exactly across the two repos.
        let mut analyzer = Analyzer::new(Config::default());

        analyzer.endpoints.push(op_details_in_repo(
            OperationKey::pubsub("metrics.page_view"),
            "web/lib/realtime.ts:5",
            "web-dashboard",
        ));
        analyzer.calls.push(op_details_in_repo(
            OperationKey::pubsub("metrics.page_view"),
            "analytics/redis/publisher.ts:7",
            "analytics-worker",
        ));

        let (_, _, edges) = analyzer.analyze_exact_key_matches(crate::operation::Protocol::Pubsub);
        assert_eq!(edges.len(), 1, "one pub/sub edge expected");
        let e = &edges[0];
        assert_eq!(e.producer_repo, "web-dashboard");
        assert_eq!(e.consumer_repo, "analytics-worker");
        assert_eq!(e.producer_key, "pubsub|metrics.page_view");
        assert_eq!(e.consumer_key, "pubsub|metrics.page_view");
        assert_eq!(e.match_score, 1.0);
        // Compat is filled in later by overlay_compat_verdicts, not here; pub/sub
        // compat machinery is deferred, so it stays None.
        assert_eq!(e.type_compatible, None);
    }

    #[test]
    fn pubsub_intra_repo_self_loop_emits_no_edge() {
        // Corpus-2 `_must_not_emit`: orders-engine BOTH subscribes (producer /
        // endpoint) and publishes (consumer / call) the internal `__dlq.retry`
        // dead-letter topic. Same repo on both sides with no other participant is
        // an intra-repo self-loop, not a cross-repo contract — no edge.
        let mut analyzer = Analyzer::new(Config::default());

        analyzer.endpoints.push(op_details_in_repo(
            OperationKey::pubsub("__dlq.retry"),
            "orders-engine/src/kafka/dlq.ts:26",
            "orders-engine",
        ));
        analyzer.calls.push(op_details_in_repo(
            OperationKey::pubsub("__dlq.retry"),
            "orders-engine/src/kafka/dlq.ts:19",
            "orders-engine",
        ));

        let (_, _, edges) = analyzer.analyze_exact_key_matches(crate::operation::Protocol::Pubsub);
        assert!(
            edges.is_empty(),
            "intra-repo self-loop must not emit a cross-repo edge, got {edges:?}"
        );
    }

    #[test]
    fn pubsub_self_edge_dropped_but_cross_edges_survive() {
        // Fan-in must not regress: two repos subscribe `order.placed`
        // (producers), and orders-engine ALSO publishes it (consumer). Only the
        // orders-engine→orders-engine self-edge is dropped; the genuine
        // orders-engine(publisher) → notifications-svc(subscriber) cross edge
        // survives.
        let mut analyzer = Analyzer::new(Config::default());

        analyzer.endpoints.push(op_details_in_repo(
            OperationKey::pubsub("order.placed"),
            "notifications-svc/src/consume.ts:4",
            "notifications-svc",
        ));
        analyzer.endpoints.push(op_details_in_repo(
            OperationKey::pubsub("order.placed"),
            "orders-engine/src/consume.ts:4",
            "orders-engine",
        ));
        analyzer.calls.push(op_details_in_repo(
            OperationKey::pubsub("order.placed"),
            "orders-engine/src/producer.ts:7",
            "orders-engine",
        ));

        let (_, _, edges) = analyzer.analyze_exact_key_matches(crate::operation::Protocol::Pubsub);
        assert_eq!(
            edges.len(),
            1,
            "self-edge dropped, cross edge kept; got {edges:?}"
        );
        assert_eq!(edges[0].producer_repo, "notifications-svc");
        assert_eq!(edges[0].consumer_repo, "orders-engine");
    }

    #[test]
    fn test_exact_key_matches_emit_edge_per_producer_repo() {
        use crate::operation::GraphqlOperationKind;
        let mut analyzer = Analyzer::new(Config::default());

        // Two services expose the same GraphQL field; exact-key matching cannot
        // disambiguate by URL, so a consumer of `order` gets an edge to each.
        analyzer.endpoints.push(op_details_in_repo(
            OperationKey::graphql(GraphqlOperationKind::Query, "order"),
            "gateway/schema.graphql:3",
            "gateway",
        ));
        analyzer.endpoints.push(op_details_in_repo(
            OperationKey::graphql(GraphqlOperationKind::Query, "order"),
            "legacy/schema.graphql:3",
            "legacy-gateway",
        ));
        analyzer.calls.push(op_details_in_repo(
            OperationKey::graphql(GraphqlOperationKind::Query, "order"),
            "web/lib/graphql.ts:5",
            "web-frontend",
        ));

        let (_, _, edges) = analyzer.analyze_exact_key_matches(crate::operation::Protocol::Graphql);
        assert_eq!(edges.len(), 2, "one edge per producer repo expected");
        let producer_repos: std::collections::BTreeSet<&str> =
            edges.iter().map(|e| e.producer_repo.as_str()).collect();
        assert_eq!(
            producer_repos,
            ["gateway", "legacy-gateway"].into_iter().collect()
        );
        assert!(edges.iter().all(|e| e.consumer_repo == "web-frontend"));
    }

    #[test]
    fn test_graphql_calls_do_not_hit_http_matcher() {
        use crate::operation::GraphqlOperationKind;
        let mut analyzer = Analyzer::new(Config::default());

        analyzer.calls.push(graphql_details(
            OperationKey::graphql(GraphqlOperationKind::Query, "user"),
            "client.ts:12",
        ));

        let mount_graph = MountGraph::new();
        let (findings, verified, _cross_repo_matches) =
            analyzer.analyze_matches_with_mount_graph(&mount_graph);
        assert!(findings.is_empty());
        assert!(verified.is_empty());
    }

    #[test]
    fn test_analyze_matches_with_mount_graph_env_vars() {
        // Setup config with internal env vars
        let config = Config {
            internal_env_vars: ["API_URL".to_string()].into_iter().collect(),
            ..Config::default()
        };

        // Create analyzer with dummy source map (not used for this analysis)
        let mut analyzer = Analyzer::new(config);

        // Add calls that use env vars
        // 1. Valid internal call (should match if endpoint exists, or report missing)
        analyzer.calls.push(ApiEndpointDetails {
            owner: None,
            key: OperationKey::http("GET", "ENV_VAR:API_URL:/users"),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: None,
            response_type: None,
            file_path: PathBuf::from("test.ts"),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
        });

        // 2. Unclassified env var (not in internal/external list)
        analyzer.calls.push(ApiEndpointDetails {
            owner: None,
            key: OperationKey::http("GET", "ENV_VAR:UNKNOWN_VAR:/posts"),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: None,
            response_type: None,
            file_path: PathBuf::from("test.ts"),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
        });

        // 3. Process.env pattern (should be detected as env var)
        analyzer.calls.push(ApiEndpointDetails {
            owner: None,
            key: OperationKey::http("GET", "${process.env.OTHER_VAR}/comments"),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: None,
            response_type: None,
            file_path: PathBuf::from("test.ts"),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
        });

        // 4. Raw code pattern with UPPERCASE var (common in legacy code)
        // e.g. LEGACY_API_URL + "/users"
        analyzer.calls.push(ApiEndpointDetails {
            owner: None,
            key: OperationKey::http("GET", "LEGACY_API_URL + \"/users\""),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: None,
            response_type: None,
            file_path: PathBuf::from("test.ts"),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
        });

        let mount_graph = MountGraph::new(); // Empty graph

        // Run analysis
        let (findings, _verified, _cross_repo_matches) =
            analyzer.analyze_matches_with_mount_graph(&mount_graph);

        // Check results
        // 1. Valid internal call surfaces as a missing endpoint (graph is
        // empty), carrying the normalized path and the typed call site.
        assert!(findings.iter().any(|f| matches!(
            f,
            Finding::MissingEndpoint { method, path, call_sites, .. }
                if method == "GET" && path == "/users" && call_sites == &vec!["test.ts".to_string()]
        )));

        // 2. Unclassified var becomes an env-var finding.
        assert!(findings.iter().any(|f| matches!(
            f,
            Finding::EnvVarCall { env_var, path, .. } if env_var == "UNKNOWN_VAR" && path == "/posts"
        )));

        // 3. Process.env var becomes an env-var finding too.
        assert!(findings.iter().any(|f| matches!(
            f,
            Finding::EnvVarCall { env_var, .. } if env_var == "OTHER_VAR"
        )));

        // 4. Raw, unresolved `LEGACY_API_URL + "/users"` expressions are now
        // dropped by is_valid_route_shape: the file-analyzer contract requires
        // composed URLs to be normalized to `${VAR}/path`, so a raw JS
        // expression here is unreliable. This is the same tightening that stops
        // bare uppercase identifiers (`CarrickApiKeys`, `DynamoDB`) from being
        // mis-reported as env-var calls.
        assert!(!findings.iter().any(|f| matches!(
            f,
            Finding::EnvVarCall { env_var, .. } if env_var == "LEGACY_API_URL"
        )));
    }

    /// A consumer call whose path exists in the index under a different verb
    /// must surface exactly once, as a method-mismatch risk — not as a
    /// missing-endpoint + orphaned-endpoint pair.
    #[test]
    fn test_wrong_verb_call_surfaces_as_method_mismatch() {
        use crate::mount_graph::ResolvedEndpoint;

        let mut analyzer = Analyzer::new(Config::default());

        analyzer.calls.push(ApiEndpointDetails {
            owner: None,
            key: OperationKey::http("GET", "/api/orders"),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: None,
            response_type: None,
            file_path: PathBuf::from("client.ts:12"),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
        });

        let mut mount_graph = MountGraph::new();
        mount_graph.endpoints.push(ResolvedEndpoint {
            method: "POST".to_string(),
            path: "/api/orders".to_string(),
            full_path: "/api/orders".to_string(),
            handler: None,
            owner: "app".to_string(),
            file_location: "server.ts:10".to_string(),
            middleware_chain: vec![],
            repo_name: Some("api".to_string()),
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
        });

        let (findings, verified, _edges) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        assert_eq!(
            findings,
            vec![Finding::method_mismatch(
                "GET",
                "/api/orders",
                None,
                vec!["client.ts:12".into()],
                "POST",
            )],
            "a wrong-verb call must surface once, as a risk"
        );
        // The producer is neither verified nor orphaned.
        assert!(verified.is_empty());
    }

    /// A call whose path matches nothing at all (under any verb) is still a
    /// plain missing endpoint, and the unrelated producer stays orphaned.
    #[test]
    fn test_unmatched_path_still_reports_missing_and_orphaned() {
        use crate::mount_graph::ResolvedEndpoint;

        let mut analyzer = Analyzer::new(Config::default());

        analyzer.calls.push(ApiEndpointDetails {
            owner: None,
            key: OperationKey::http("GET", "/api/missing"),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: None,
            response_type: None,
            file_path: PathBuf::from("client.ts:3"),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
        });

        let mut mount_graph = MountGraph::new();
        mount_graph.endpoints.push(ResolvedEndpoint {
            method: "POST".to_string(),
            path: "/api/orders".to_string(),
            full_path: "/api/orders".to_string(),
            handler: None,
            owner: "app".to_string(),
            file_location: "server.ts:10".to_string(),
            middleware_chain: vec![],
            repo_name: Some("api".to_string()),
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
        });

        let (findings, _, _) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        assert_eq!(
            findings,
            vec![
                Finding::missing_endpoint("GET", "/api/missing", None, vec!["client.ts:3".into()]),
                Finding::orphaned_endpoint("POST", "/api/orders", Some("api".to_string())),
            ]
        );
    }

    /// A matched wire whose producer is a mock/test handler must expose that
    /// provenance on the edge, the verified entry, and — when unmatched — the
    /// orphan finding (#380).
    #[test]
    fn test_matched_wire_exposes_mock_producer_provenance() {
        use crate::mount_graph::{DataFetchingCall, ResolvedEndpoint};
        use crate::operation::EndpointProvenance;

        let mut analyzer = Analyzer::new(Config::default());

        analyzer.calls.push(ApiEndpointDetails {
            owner: None,
            key: OperationKey::http("GET", "/api/widgets"),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: None,
            response_type: None,
            file_path: PathBuf::from("client.ts:12"),
            repo_name: Some("consumer-repo".to_string()),
            service_name: None,
            provenance: Default::default(),
        });

        let mut mount_graph = MountGraph::new();
        // The matched producer: an MSW-style handler under a mock tree.
        mount_graph.endpoints.push(ResolvedEndpoint {
            method: "GET".to_string(),
            path: "/api/widgets".to_string(),
            full_path: "/api/widgets".to_string(),
            handler: None,
            owner: "http".to_string(),
            file_location: "src/mocks/handlers.ts:5".to_string(),
            middleware_chain: vec![],
            repo_name: Some("producer-repo".to_string()),
            service_name: None,
            provenance: EndpointProvenance::Mock,
            evidence: carrick_match::MatchEvidence::RouteDefinition,
        });
        // An unmatched mock producer: must orphan WITH the mock tag.
        mount_graph.endpoints.push(ResolvedEndpoint {
            method: "POST".to_string(),
            path: "/api/widgets".to_string(),
            full_path: "/api/widgets".to_string(),
            handler: None,
            owner: "http".to_string(),
            file_location: "src/mocks/handlers.ts:9".to_string(),
            middleware_chain: vec![],
            repo_name: Some("producer-repo".to_string()),
            service_name: None,
            provenance: EndpointProvenance::Mock,
            evidence: carrick_match::MatchEvidence::RouteDefinition,
        });
        mount_graph.data_calls.push(DataFetchingCall {
            method: "GET".to_string(),
            target_url: "/api/widgets".to_string(),
            canonical_path: "/api/widgets".to_string(),
            client: "fetch".to_string(),
            file_location: "client.ts:12".to_string(),
            call_kind: None,
            repo_name: Some("consumer-repo".to_string()),
            service_name: None,
        });

        let (findings, verified, edges) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        assert_eq!(edges.len(), 1, "one matched wire expected, got {edges:?}");
        assert_eq!(edges[0].producer_repo, "producer-repo");
        assert_eq!(
            edges[0].producer_provenance,
            EndpointProvenance::Mock,
            "the matched wire must expose the producer's mock provenance"
        );
        assert_eq!(
            verified,
            vec![VerifiedEndpointEntry::new(
                "GET".to_string(),
                "/api/widgets".to_string(),
                EndpointProvenance::Mock
            )],
            "the verified entry must carry the mock tag"
        );
        assert_eq!(
            findings,
            vec![
                Finding::orphaned_endpoint("POST", "/api/widgets", Some("producer-repo".into()))
                    .with_producer_provenance(EndpointProvenance::Mock)
            ],
            "the orphaned mock must carry the mock tag"
        );
    }

    /// Exact-key protocols thread producer provenance onto the edge the same
    /// way the HTTP matcher does (#380).
    #[test]
    fn test_exact_key_edge_carries_producer_provenance() {
        use crate::operation::EndpointProvenance;

        let mut analyzer = Analyzer::new(Config::default());

        let mut producer = op_details_in_repo(
            OperationKey::pubsub("orders.created"),
            "svc/src/mocks/bus.ts:4",
            "producer-repo",
        );
        producer.provenance = EndpointProvenance::Mock;
        analyzer.endpoints.push(producer);
        analyzer.calls.push(op_details_in_repo(
            OperationKey::pubsub("orders.created"),
            "worker/src/publish.ts:9",
            "consumer-repo",
        ));

        let (_, verified, edges) =
            analyzer.analyze_exact_key_matches(crate::operation::Protocol::Pubsub);
        assert_eq!(edges.len(), 1, "one pub/sub edge expected, got {edges:?}");
        assert_eq!(edges[0].producer_provenance, EndpointProvenance::Mock);
        assert_eq!(
            verified,
            vec![VerifiedEndpointEntry::new(
                "PUBSUB".to_string(),
                "orders.created".to_string(),
                EndpointProvenance::Mock
            )]
        );
    }

    /// #379: when the producer-side entry is call-site evidence (a client
    /// call double-extracted as an endpoint), a match against another repo's
    /// identical call is a shared-external-contract pair: the edge carries
    /// the relationship, the report gets one role-free group finding naming
    /// both repos, and no producer role is fabricated (the entry is neither
    /// verified nor orphaned, and there is no missing-endpoint gap).
    #[test]
    fn test_call_site_evidence_pair_reports_shared_external_contract() {
        use crate::mount_graph::{DataFetchingCall, ResolvedEndpoint};

        let mut analyzer = Analyzer::new(Config::default());

        // repo-alpha: a client call expression the extraction double-emitted;
        // reclassified to call-site evidence at scan time.
        let mut mount_graph = MountGraph::new();
        mount_graph.endpoints.push(ResolvedEndpoint {
            method: "POST".to_string(),
            path: "/v2/widgets".to_string(),
            full_path: "/v2/widgets".to_string(),
            handler: None,
            owner: "app".to_string(),
            file_location: "operations/create-widget.ts:14".to_string(),
            middleware_chain: vec![],
            repo_name: Some("repo-alpha".to_string()),
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::CallSite,
        });
        // repo-beta: the identical call to the same external endpoint.
        mount_graph.data_calls.push(DataFetchingCall {
            method: "POST".to_string(),
            target_url: "https://api.vendor.example/v2/widgets".to_string(),
            canonical_path: "/v2/widgets".to_string(),
            client: "fetch".to_string(),
            file_location: "src/widgets-client.ts:33".to_string(),
            call_kind: None,
            repo_name: Some("repo-beta".to_string()),
            service_name: None,
        });
        analyzer
            .calls
            .push(http_call("POST", "/v2/widgets", "src/widgets-client.ts:33"));

        let (findings, verified, edges) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        // One edge, classified — with NO producer role asserted beyond the
        // side names (see CrossRepoMatch::relationship).
        assert_eq!(edges.len(), 1, "edges: {edges:?}");
        assert_eq!(
            edges[0].relationship,
            carrick_match::MatchRelationship::SharedExternalContract
        );
        assert_eq!(edges[0].producer_repo, "repo-alpha");
        assert_eq!(edges[0].consumer_repo, "repo-beta");

        // One role-free group finding; no gaps, no fabricated producer. The
        // call sites name BOTH encodings — repo-beta's call AND repo-alpha's
        // double-extracted site — so the report shows where every encoding
        // of the contract lives.
        assert_eq!(
            findings,
            vec![Finding::shared_external_contract(
                "POST",
                "/v2/widgets",
                vec!["repo-alpha".to_string(), "repo-beta".to_string()],
                vec![
                    "operations/create-widget.ts:14".to_string(),
                    "src/widgets-client.ts:33".to_string(),
                ],
            )],
            "the pair must surface once, as a shared-external-contract group"
        );
        assert!(
            verified.is_empty(),
            "a call-site-evidence entry is never a verified producer"
        );
    }

    /// #379 control: a real route definition matched by another repo's call
    /// stays a plain producer/consumer edge — verified endpoint (with its
    /// provenance, #380), no shared group, no behaviour change.
    #[test]
    fn test_route_definition_pair_stays_producer_consumer() {
        use crate::mount_graph::{DataFetchingCall, ResolvedEndpoint};

        let mut analyzer = Analyzer::new(Config::default());

        let mut mount_graph = MountGraph::new();
        mount_graph.endpoints.push(ResolvedEndpoint {
            method: "POST".to_string(),
            path: "/v2/widgets".to_string(),
            full_path: "/v2/widgets".to_string(),
            handler: Some("createWidget".to_string()),
            owner: "app".to_string(),
            file_location: "src/server.ts:14".to_string(),
            middleware_chain: vec![],
            repo_name: Some("repo-alpha".to_string()),
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
        });
        mount_graph.data_calls.push(DataFetchingCall {
            method: "POST".to_string(),
            target_url: "/v2/widgets".to_string(),
            canonical_path: "/v2/widgets".to_string(),
            client: "fetch".to_string(),
            file_location: "src/widgets-client.ts:33".to_string(),
            call_kind: None,
            repo_name: Some("repo-beta".to_string()),
            service_name: None,
        });
        analyzer
            .calls
            .push(http_call("POST", "/v2/widgets", "src/widgets-client.ts:33"));

        let (findings, verified, edges) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].relationship,
            carrick_match::MatchRelationship::ProducerConsumer
        );
        assert!(findings.is_empty(), "findings: {findings:?}");
        assert_eq!(
            verified,
            vec![VerifiedEndpointEntry::new(
                "POST".to_string(),
                "/v2/widgets".to_string(),
                crate::operation::EndpointProvenance::Route
            )]
        );
    }

    /// #379: a repo re-matching its OWN call-site-evidence entry (the twin
    /// call kept by scan-time suppression) is not signal — no self edge, and
    /// a group confined to one repo is not reported.
    #[test]
    fn test_same_repo_call_site_pair_emits_no_edge_or_group() {
        use crate::mount_graph::{DataFetchingCall, ResolvedEndpoint};

        let mut analyzer = Analyzer::new(Config::default());

        let mut mount_graph = MountGraph::new();
        mount_graph.endpoints.push(ResolvedEndpoint {
            method: "POST".to_string(),
            path: "/v2/widgets".to_string(),
            full_path: "/v2/widgets".to_string(),
            handler: None,
            owner: "app".to_string(),
            file_location: "operations/create-widget.ts:14".to_string(),
            middleware_chain: vec![],
            repo_name: Some("repo-alpha".to_string()),
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::CallSite,
        });
        mount_graph.data_calls.push(DataFetchingCall {
            method: "POST".to_string(),
            target_url: "/v2/widgets".to_string(),
            canonical_path: "/v2/widgets".to_string(),
            client: "request".to_string(),
            file_location: "operations/create-widget.ts:14".to_string(),
            call_kind: None,
            repo_name: Some("repo-alpha".to_string()),
            service_name: None,
        });
        analyzer.calls.push(http_call(
            "POST",
            "/v2/widgets",
            "operations/create-widget.ts:14",
        ));

        let (findings, verified, edges) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        assert!(edges.is_empty(), "no self edge: {edges:?}");
        assert!(
            findings.is_empty(),
            "single-repo group is noise: {findings:?}"
        );
        assert!(verified.is_empty());
    }

    /// Bare HTTP consumer call for the mount-graph matcher tests.
    fn http_call(method: &str, path: &str, file: &str) -> ApiEndpointDetails {
        graphql_details(OperationKey::http(method, path.to_string()), file)
    }

    /// Producer endpoint in the mount graph, repo-tagged `"api"`.
    fn resolved(method: &str, full_path: &str) -> crate::mount_graph::ResolvedEndpoint {
        crate::mount_graph::ResolvedEndpoint {
            method: method.to_string(),
            path: full_path.to_string(),
            full_path: full_path.to_string(),
            handler: None,
            owner: "app".to_string(),
            file_location: "server.ts:10".to_string(),
            middleware_chain: vec![],
            repo_name: Some("api".to_string()),
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
        }
    }

    /// The wrong-verb retry must require an EXACT declared path, never a
    /// param wildcard: `POST /users/:id` wildcard-matches `/users/list`, but
    /// a missing `GET /users/list` is a connectivity gap, not a "call uses
    /// GET but the producer expects POST" risk (which always headlines and
    /// would fail the cloud check run even for a no-baseline repo).
    #[test]
    fn test_wildcard_only_collision_stays_missing_endpoint() {
        let mut analyzer = Analyzer::new(Config::default());

        analyzer
            .calls
            .push(http_call("GET", "/users/list", "client.ts:4"));

        let mut mount_graph = MountGraph::new();
        mount_graph.endpoints.push(resolved("POST", "/users/:id"));

        let (findings, verified, _) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        assert_eq!(
            findings,
            vec![
                Finding::missing_endpoint("GET", "/users/list", None, vec!["client.ts:4".into()]),
                Finding::orphaned_endpoint("POST", "/users/:id", Some("api".to_string())),
            ],
            "a wildcard-only path collision must not become a method mismatch"
        );
        assert!(verified.is_empty());
    }

    /// Param NAMES are not identity: a consumer path normalized to a
    /// different param name (`/orders/:oid` vs the declared `/orders/:id`)
    /// still counts as the same declared route, and the finding reports the
    /// producer's declared spelling.
    #[test]
    fn test_method_mismatch_matches_params_by_position_not_name() {
        let mut analyzer = Analyzer::new(Config::default());

        analyzer
            .calls
            .push(http_call("GET", "/orders/:oid", "client.ts:7"));

        let mut mount_graph = MountGraph::new();
        mount_graph.endpoints.push(resolved("POST", "/orders/:id"));

        let (findings, _, _) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        assert_eq!(
            findings,
            vec![Finding::method_mismatch(
                "GET",
                "/orders/:id",
                None,
                vec!["client.ts:7".into()],
                "POST",
            )]
        );
    }

    /// When several verbs exist at the mismatched path, name the UNVERIFIED
    /// producer (the wrong verb most plausibly aims at it) and suppress only
    /// that one from the orphan list — every sibling keeps its own
    /// verified/orphaned classification.
    #[test]
    fn test_method_mismatch_prefers_unverified_producer_and_keeps_siblings() {
        let mut analyzer = Analyzer::new(Config::default());

        // GET /a is genuinely consumed; PUT /a is the wrong verb; GET /b is
        // an unrelated real orphan that must survive.
        analyzer.calls.push(http_call("GET", "/a", "client.ts:1"));
        analyzer.calls.push(http_call("PUT", "/a", "client.ts:2"));

        let mut mount_graph = MountGraph::new();
        mount_graph.endpoints.push(resolved("GET", "/a"));
        mount_graph.endpoints.push(resolved("POST", "/a"));
        mount_graph.endpoints.push(resolved("GET", "/b"));

        let (findings, verified, _) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        assert_eq!(
            findings,
            vec![
                // Expected method is the unverified POST, not the verified GET.
                Finding::method_mismatch("PUT", "/a", None, vec!["client.ts:2".into()], "POST"),
                // GET /b keeps its orphan classification; POST /a is
                // suppressed (it is the producer the risk names).
                Finding::orphaned_endpoint("GET", "/b", Some("api".to_string())),
            ]
        );
        assert_eq!(
            verified,
            vec![VerifiedEndpointEntry::new(
                "GET".to_string(),
                "/a".to_string(),
                crate::operation::EndpointProvenance::Route
            )]
        );
    }

    /// When every exact-path producer is verified, fall back to the first in
    /// sorted order; a verified producer never sat in the orphan list, so
    /// nothing is hidden.
    #[test]
    fn test_method_mismatch_falls_back_to_sorted_verified_producer() {
        let mut analyzer = Analyzer::new(Config::default());

        analyzer.calls.push(http_call("GET", "/a", "client.ts:1"));
        analyzer.calls.push(http_call("PUT", "/a", "client.ts:2"));

        let mut mount_graph = MountGraph::new();
        mount_graph.endpoints.push(resolved("GET", "/a"));

        let (findings, verified, _) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        assert_eq!(
            findings,
            vec![Finding::method_mismatch(
                "PUT",
                "/a",
                None,
                vec!["client.ts:2".into()],
                "GET",
            )]
        );
        assert_eq!(
            verified,
            vec![VerifiedEndpointEntry::new(
                "GET".to_string(),
                "/a".to_string(),
                crate::operation::EndpointProvenance::Route
            )]
        );
    }

    // -----------------------------------------------------------------------
    // overlay_compat_verdicts — the verdict-attachment step, re-pointed at
    // the structured v2 pair outcomes (WP3). These tests are deterministic:
    // they craft `PairCheckOutcome`s and assert how they land on the edges.
    // No sidecar, no tsc, no LLM — just the Rust join logic. The join
    // semantics they pin (#226/#260, param normalization, per-consumer
    // keying, unverifiable-stays-None) are unchanged from the ts_check era;
    // what changed is the input shape (structured outcomes, no label
    // parsing) and that `Some(true)` now requires an EXPLICIT compatible
    // verdict instead of falling out of an optimistic default.
    // -----------------------------------------------------------------------

    use crate::services::type_sidecar::VerdictBucket;

    /// Minimal analyzer carrying the given structured pair outcomes.
    fn analyzer_with_outcomes(outcomes: Vec<PairCheckOutcome>) -> Analyzer {
        let mut analyzer = Analyzer::new(Config::default());
        analyzer.set_pair_outcomes(outcomes);
        analyzer
    }

    /// Build one structured pair outcome. `consumer_location` is
    /// `"<file>:<line>[:<col>]"`, reduced through `parse_file_location`
    /// exactly like the manifest side does.
    fn outcome(
        pseudo_method: &str,
        identity: &str,
        consumer_location: &str,
        bucket: VerdictBucket,
        diagnostic: Option<&str>,
    ) -> PairCheckOutcome {
        let (consumer_file, consumer_line) = parse_file_location(consumer_location);
        PairCheckOutcome {
            pair_key: format!("p/{identity}~c/{consumer_location}#{pseudo_method}"),
            pseudo_method: pseudo_method.to_string(),
            identity: identity.to_string(),
            consumer_file,
            consumer_line,
            type_kind: crate::cloud_storage::ManifestTypeKind::Response,
            bucket,
            gate: None,
            diagnostic: diagnostic.map(str::to_string),
            producer_alias: "Producer_Alias".to_string(),
            consumer_alias: "Consumer_Alias".to_string(),
            producer_service: "producer-svc".to_string(),
            consumer_service: "consumer-svc".to_string(),
        }
    }

    /// A `payments-svc` consumer edge against `producer_key`, with a default
    /// consumer call site. The overlay keys per-consumer, so outcome
    /// fixtures must carry a consumer location matching this one.
    const PAYMENTS_CONSUMER_LOC: &str = "payments-svc/src/client.ts:12:1";

    fn edge(producer_key: &str) -> CrossRepoMatch {
        edge_at(producer_key, "payments-svc", PAYMENTS_CONSUMER_LOC)
    }

    /// Build an edge with an explicit consumer repo + source location, so a test
    /// can model two consumers of one producer with distinct call sites.
    fn edge_at(producer_key: &str, consumer_repo: &str, consumer_location: &str) -> CrossRepoMatch {
        CrossRepoMatch {
            producer_repo: "orders-monorepo".to_string(),
            producer_key: producer_key.to_string(),
            consumer_repo: consumer_repo.to_string(),
            consumer_key: producer_key.to_string(),
            consumer_location: Some(consumer_location.to_string()),
            match_score: 1.0,
            type_compatible: None,
            type_verdict: None,
            mismatch_reason: None,
            producer_provenance: Default::default(),
            relationship: carrick_match::MatchRelationship::ProducerConsumer,
        }
    }

    /// With outcomes stored, every covered edge gets a verdict: the pair in
    /// the incompatible bucket → `Some(false)` + reason; an explicitly
    /// compatible pair → `Some(true)`.
    #[test]
    fn overlay_compat_verdicts_attaches_from_outcomes() {
        let analyzer = analyzer_with_outcomes(vec![
            outcome(
                "GET",
                "/orders/:id",
                "payments-svc/src/client.ts:12",
                VerdictBucket::Incompatible,
                Some("id: number is not assignable to string"),
            ),
            outcome(
                "POST",
                "/payments",
                "payments-svc/src/client.ts:12",
                VerdictBucket::Compatible,
                None,
            ),
        ]);

        let mut matches = vec![edge("http|GET|/orders/:id"), edge("http|POST|/payments")];
        analyzer.overlay_compat_verdicts(&mut matches);

        assert_eq!(
            matches[0].type_compatible,
            Some(false),
            "the incompatible pair's edge is incompatible"
        );
        assert_eq!(
            matches[0].mismatch_reason.as_deref(),
            Some("id: number is not assignable to string"),
        );
        assert_eq!(
            matches[1].type_compatible,
            Some(true),
            "an explicitly compatible pair's edge is compatible"
        );
        assert!(matches[1].mismatch_reason.is_none());
    }

    /// #455/#456: `apply_pair_outcomes` sets the three-way `type_verdict`
    /// WITHOUT the `Option<bool>` collapse. The load-bearing case is
    /// `Unverifiable`: the edge keeps `type_compatible == None` (it is NOT
    /// compatible) yet carries `Some(Unverifiable)`, so a matched-but-
    /// unverifiable pair is separable from a never-evaluated one (`None`).
    #[test]
    fn apply_pair_outcomes_sets_three_way_type_verdict() {
        use crate::operation::TypeVerdict;
        let outcomes = vec![
            outcome(
                "GET",
                "/orders/:id",
                PAYMENTS_CONSUMER_LOC,
                VerdictBucket::Compatible,
                None,
            ),
            outcome(
                "POST",
                "/refunds",
                PAYMENTS_CONSUMER_LOC,
                VerdictBucket::Unverifiable,
                None,
            ),
            outcome(
                "PUT",
                "/widgets",
                PAYMENTS_CONSUMER_LOC,
                VerdictBucket::Incompatible,
                Some("boom"),
            ),
        ];
        let mut matches = vec![
            edge("http|GET|/orders/:id"),
            edge("http|POST|/refunds"),
            edge("http|PUT|/widgets"),
            edge("http|DELETE|/never-checked"),
        ];
        apply_pair_outcomes(&outcomes, &mut matches);

        assert_eq!(matches[0].type_verdict, Some(TypeVerdict::Compatible));
        assert_eq!(matches[1].type_verdict, Some(TypeVerdict::Unverifiable));
        assert_eq!(
            matches[1].type_compatible, None,
            "an unverifiable pair is NOT compatible, but its verdict is present"
        );
        assert_eq!(matches[2].type_verdict, Some(TypeVerdict::Incompatible));
        assert_eq!(
            matches[3].type_verdict, None,
            "an edge no outcome reached stays verdict-absent"
        );
    }

    /// The reconstruction used by the verified-endpoint verdict join must equal
    /// [`OperationKey::display_labels`] byte-for-byte for every protocol —
    /// otherwise the join silently misses and every row falls to "not
    /// compared" (the exact bug #456 targets). Guards trap #1 (join drift),
    /// including the param-name-carrying HTTP route.
    #[test]
    fn producer_display_from_canonical_matches_display_labels() {
        use crate::operation::{GraphqlOperationKind, OperationKey, SocketDirection};
        let keys = vec![
            OperationKey::http("get", "/orders/:id"),
            OperationKey::http("POST", "/payments"),
            OperationKey::graphql(GraphqlOperationKind::Query, "getUser"),
            OperationKey::graphql(GraphqlOperationKind::Mutation, "createOrder"),
            OperationKey::socket("chat:message", SocketDirection::ClientToServer),
            OperationKey::socket("chat:message", SocketDirection::ServerToClient),
            OperationKey::pubsub("orders.created"),
        ];
        for key in keys {
            let canonical = key.canonical();
            assert_eq!(
                producer_display_from_canonical(&canonical),
                Some(key.display_labels()),
                "canonical {canonical} must reconstruct display_labels exactly"
            );
        }
    }

    /// The verdict join + per-endpoint aggregation (#455/#456): edges carry
    /// per-consumer verdicts; a verified row aggregates all its consumers'
    /// verdicts honesty-first. Covers a param HTTP route with two consumers
    /// (compatible + unverifiable → the row degrades to unverifiable), a
    /// non-HTTP (GraphQL) producer joined by reconstructed display, and a
    /// verified row with no evaluated edge (stays `None`).
    #[test]
    fn attach_verified_type_verdicts_joins_and_aggregates() {
        use crate::operation::{EndpointProvenance, TypeVerdict};

        let mut compat = edge_at("http|GET|/orders/:id", "a", "a/x.ts:1");
        compat.type_verdict = Some(TypeVerdict::Compatible);
        let mut unver = edge_at("http|GET|/orders/:id", "b", "b/y.ts:2");
        unver.type_verdict = Some(TypeVerdict::Unverifiable);
        let mut gql = edge_at("graphql|query|getUser", "c", "c/z.ts:3");
        gql.type_verdict = Some(TypeVerdict::Compatible);
        // An edge whose producer was never type-checked contributes nothing.
        let unevaluated = edge_at("http|POST|/unmatched", "d", "d/w.ts:4");
        let edges = vec![compat, unver, gql, unevaluated];

        let mut verified = vec![
            VerifiedEndpointEntry::new(
                "GET".into(),
                "/orders/:id".into(),
                EndpointProvenance::Route,
            ),
            VerifiedEndpointEntry::new("QUERY".into(), "getUser".into(), EndpointProvenance::Route),
            VerifiedEndpointEntry::new(
                "POST".into(),
                "/unmatched".into(),
                EndpointProvenance::Route,
            ),
        ];
        attach_verified_type_verdicts(&mut verified, &edges);

        assert_eq!(
            verified[0].type_verdict,
            Some(TypeVerdict::Unverifiable),
            "compatible + unverifiable consumers aggregate to unverifiable"
        );
        assert_eq!(
            verified[1].type_verdict,
            Some(TypeVerdict::Compatible),
            "a GraphQL producer joins by reconstructed display and is compatible"
        );
        assert_eq!(
            verified[2].type_verdict, None,
            "a verified row with no evaluated edge stays verdict-absent"
        );
    }

    /// #379: a shared-external-contract edge is verdict-exempt. Both sides
    /// are call sites, so any comparison on the same key would be
    /// request-vs-request — the guard keeps the verdict `None` even when a
    /// compatible outcome exists for the same join key.
    #[test]
    fn apply_pair_outcomes_leaves_shared_external_contract_edges_unevaluated() {
        let outcomes = vec![outcome(
            "POST",
            "/v2/widgets",
            PAYMENTS_CONSUMER_LOC,
            VerdictBucket::Compatible,
            None,
        )];

        let mut shared = edge("http|POST|/v2/widgets");
        shared.relationship = carrick_match::MatchRelationship::SharedExternalContract;
        // A stale reason (e.g. from an earlier overlay pass) must be cleared
        // along with the verdict: `mismatch_reason` is only ever present
        // alongside `type_compatible == Some(false)`.
        shared.mismatch_reason = Some("stale request-vs-request reason".to_string());
        let mut matches = vec![shared, edge("http|POST|/v2/widgets")];
        apply_pair_outcomes(&outcomes, &mut matches);

        assert_eq!(
            matches[0].type_compatible, None,
            "shared-external-contract edges are verdict-exempt"
        );
        assert_eq!(
            matches[0].type_verdict, None,
            "a verdict-exempt edge carries no three-way verdict either"
        );
        assert_eq!(
            matches[0].mismatch_reason, None,
            "a verdict-exempt edge carries no mismatch reason"
        );
        assert_eq!(
            matches[1].type_compatible,
            Some(true),
            "the producer/consumer edge on the same key still gets its verdict"
        );
        assert_eq!(
            matches[1].type_verdict,
            Some(crate::operation::TypeVerdict::Compatible),
            "the producer/consumer edge on the same key gets its three-way verdict"
        );
    }

    /// PR-comment polish (#337): the risk row's call site must not leak the CI
    /// runner workspace prefix, and the assignability detail must not end with
    /// an unbalanced quote (`clean_error_message` on the raw compiler text).
    #[test]
    fn type_mismatch_finding_strips_runner_prefix_and_trailing_quote() {
        let analyzer = analyzer_with_outcomes(vec![outcome(
            "GET",
            "/api/notifications/status",
            "/home/runner/work/user-service/user-service/server.ts:66",
            VerdictBucket::Incompatible,
            Some("Type 'NotificationStatus' is not assignable to type 'StatusView'"),
        )]);

        let findings = analyzer.get_type_mismatch_findings();
        assert_eq!(findings.len(), 1);
        let Finding::TypeMismatch {
            call_sites, detail, ..
        } = &findings[0]
        else {
            panic!("expected a TypeMismatch finding, got {:?}", findings[0]);
        };
        assert_eq!(call_sites, &vec!["server.ts:66".to_string()]);
        assert_eq!(detail, "NotificationStatus not assignable to StatusView");
    }

    /// A consumer location outside the GitHub Actions workspace passes through
    /// untouched; the prefix strip must not eat local or repo-relative paths.
    #[test]
    fn strip_ci_workspace_prefix_leaves_non_runner_paths_alone() {
        assert_eq!(
            strip_ci_workspace_prefix("payments-svc/src/client.ts:12"),
            "payments-svc/src/client.ts:12"
        );
        assert_eq!(
            strip_ci_workspace_prefix("/home/runner/work/repo/repo/src/api.ts:7"),
            "src/api.ts:7"
        );
        // Degenerate: nothing after the checkout dir → keep the original.
        assert_eq!(
            strip_ci_workspace_prefix("/home/runner/work/repo/repo"),
            "/home/runner/work/repo/repo"
        );
    }

    /// No outcomes stored → compat was not evaluated → every edge keeps
    /// `None` (the load-bearing absent verdict, NOT a fake `Some(true)`).
    #[test]
    fn overlay_compat_verdicts_leaves_none_when_outcomes_absent() {
        let analyzer = Analyzer::new(Config::default());

        let mut matches = vec![edge("http|GET|/orders/:id")];
        analyzer.overlay_compat_verdicts(&mut matches);

        assert_eq!(
            matches[0].type_compatible, None,
            "no outcomes → compat not evaluated → verdict stays None (never fake true)"
        );
    }

    /// An edge whose pair was matched but could NOT be verified (a side
    /// decayed, or the probe gates fired) stays `None` (unverifiable) rather
    /// than compatible — asserting a compatibility the checker never
    /// established would mask a real shape mismatch (#235). The baked-any
    /// gate bucket lands identically.
    #[test]
    fn overlay_compat_verdicts_leaves_unverifiable_edge_none() {
        let analyzer = analyzer_with_outcomes(vec![
            outcome(
                "GET",
                "/orders/:id",
                "payments-svc/src/client.ts:12",
                VerdictBucket::Unverifiable,
                Some("consumer type resolves to unknown"),
            ),
            outcome(
                "POST",
                "/payments",
                "payments-svc/src/client.ts:12",
                VerdictBucket::Compatible,
                None,
            ),
        ]);

        let mut matches = vec![edge("http|GET|/orders/:id"), edge("http|POST|/payments")];
        analyzer.overlay_compat_verdicts(&mut matches);

        assert_eq!(
            matches[0].type_compatible, None,
            "an unverifiable edge stays None, never a fake Some(true)"
        );
        assert!(matches[0].mismatch_reason.is_none());
        assert_eq!(
            matches[1].type_compatible,
            Some(true),
            "a genuinely-checked compatible edge is compatible"
        );
    }

    /// The gate-caught baked-any bucket (a side decayed to `any` and the
    /// probe gate fired) maps to `None` exactly like unverifiable: the pair
    /// was NOT verified, and `any` must never read as compatible.
    #[test]
    fn overlay_compat_verdicts_gate_caught_baked_any_edge_none() {
        let analyzer = analyzer_with_outcomes(vec![outcome(
            "GET",
            "/orders/:id",
            "payments-svc/src/client.ts:12",
            VerdictBucket::GateCaughtBakedAny,
            Some("producer side resolved to any (baked at capture time)"),
        )]);

        let mut matches = vec![edge("http|GET|/orders/:id")];
        analyzer.overlay_compat_verdicts(&mut matches);

        assert_eq!(
            matches[0].type_compatible, None,
            "a gate-caught baked-any edge stays None — any is never compatible"
        );
        assert!(matches[0].mismatch_reason.is_none());
    }

    /// The `graphql|subscription|orderUpdated` false-positive class at the
    /// verdict-join layer: an unresolved consumer makes the pair
    /// unverifiable, and that must pin the edge to `None` — NOT a
    /// compatible-by-absence. With explicit verdicts the absence default is
    /// gone entirely, but the unverifiable pin is still the load-bearing
    /// half.
    #[test]
    fn apply_pair_outcomes_graphql_unverifiable_edge_none() {
        let outcomes = vec![
            outcome(
                "GRAPHQL",
                "subscription|orderUpdated",
                "web-frontend/lib/graphql.ts:84",
                VerdictBucket::Unverifiable,
                Some("consumer type was not resolved at capture time"),
            ),
            outcome(
                "GRAPHQL",
                "query|order",
                "web-frontend/lib/graphql.ts:76",
                VerdictBucket::Compatible,
                None,
            ),
        ];
        let mut matches = vec![
            edge_at(
                "graphql|subscription|orderUpdated",
                "web-frontend",
                "web-frontend/lib/graphql.ts:84",
            ),
            edge_at(
                "graphql|query|order",
                "web-frontend",
                "web-frontend/lib/graphql.ts:76",
            ),
        ];
        apply_pair_outcomes(&outcomes, &mut matches);

        assert_eq!(
            matches[0].type_compatible, None,
            "an unresolved graphql consumer makes the edge unverifiable (None), \
             never a fake Some(true)"
        );
        assert!(matches[0].mismatch_reason.is_none());
        assert_eq!(
            matches[1].type_compatible,
            Some(true),
            "the resolved graphql edge with an explicit compatible verdict reads Some(true)"
        );
    }

    /// THE live compat=1/6 regression class. The pair outcome may carry the
    /// producer path in normalized form (`/orders/:param`) while the edge's
    /// `producer_key` keeps the SOURCE param name (`/orders/:id`). The join
    /// must collapse both to `:param`; otherwise the incompatible verdict
    /// misses the edge.
    #[test]
    fn apply_pair_outcomes_joins_across_param_name_normalization() {
        let outcomes = vec![
            outcome(
                "GET",
                "/orders/:param",
                "web-frontend/lib/api.ts:36",
                VerdictBucket::Incompatible,
                Some("Order is not assignable to OrderView"),
            ),
            outcome(
                "GET",
                "/orders/:param",
                "payments-svc/clients/orders.client.ts:13",
                VerdictBucket::Compatible,
                None,
            ),
            outcome(
                "POST",
                "/payments",
                "web-frontend/lib/api.ts:48",
                VerdictBucket::Unverifiable,
                None,
            ),
            outcome(
                "GRAPHQL",
                "query|order",
                "web-frontend/lib/graphql.ts:50",
                VerdictBucket::Compatible,
                None,
            ),
        ];
        let mut matches = vec![
            edge_at(
                "http|GET|/orders/:id",
                "web-frontend",
                "web-frontend/lib/api.ts:36",
            ),
            edge_at(
                "http|GET|/orders/:id",
                "payments-svc",
                "payments-svc/clients/orders.client.ts:13",
            ),
            edge_at(
                "http|POST|/payments",
                "web-frontend",
                "web-frontend/lib/api.ts:48",
            ),
            edge_at(
                "graphql|query|order",
                "web-frontend",
                "web-frontend/lib/graphql.ts:50",
            ),
        ];

        apply_pair_outcomes(&outcomes, &mut matches);

        assert_eq!(
            matches[0].type_compatible,
            Some(false),
            "the web consumer's incompatible verdict must attach despite :id vs :param"
        );
        assert_eq!(
            matches[1].type_compatible,
            Some(true),
            "the payments consumer of the same producer keeps its own compatible verdict"
        );
        assert_eq!(
            matches[2].type_compatible, None,
            "POST /payments is unverifiable → None, never a fake compatible"
        );
        assert_eq!(
            matches[3].type_compatible,
            Some(true),
            "a graphql edge with an explicit compatible verdict reads Some(true)"
        );
    }

    #[test]
    fn normalize_compat_path_collapses_param_syntaxes() {
        assert_eq!(normalize_compat_path("/orders/:id"), "/orders/:param");
        assert_eq!(normalize_compat_path("/orders/{id}"), "/orders/:param");
        assert_eq!(normalize_compat_path("/orders/[id]"), "/orders/:param");
        assert_eq!(normalize_compat_path("/payments"), "/payments");
        assert_eq!(normalize_compat_path("/"), "/");
    }

    /// THE #260 regression. One producer endpoint (`GET /orders/:id`) with TWO
    /// consumers of differing compatibility. Each pair outcome carries its own
    /// consumer identity, so the verdicts must be distinct and correctly
    /// attributed to each consumer's edge — never smeared.
    #[test]
    fn overlay_compat_verdicts_keys_per_consumer_no_smear() {
        let analyzer = analyzer_with_outcomes(vec![
            outcome(
                "GET",
                "/orders/:id",
                "web-frontend/src/orders.ts:42:3",
                VerdictBucket::Incompatible,
                Some("Order.id: number is not assignable to OrderView.id: string"),
            ),
            outcome(
                "GET",
                "/orders/:id",
                "payments-svc/src/orders-client.ts:18:5",
                VerdictBucket::Compatible,
                None,
            ),
        ]);

        // Two edges into the SAME producer endpoint, distinguished only by their
        // consumer identity (repo + call-site location).
        let payments = edge_at(
            "http|GET|/orders/:id",
            "payments-svc",
            "payments-svc/src/orders-client.ts:18:5",
        );
        let web = edge_at(
            "http|GET|/orders/:id",
            "web-frontend",
            "web-frontend/src/orders.ts:42",
        );
        let mut matches = vec![payments, web];
        analyzer.overlay_compat_verdicts(&mut matches);

        // payments-svc: compatible, from its own explicit verdict.
        assert_eq!(
            matches[0].consumer_repo, "payments-svc",
            "fixture ordering sanity"
        );
        assert_eq!(
            matches[0].type_compatible,
            Some(true),
            "the compatible consumer keeps its true verdict"
        );
        assert!(matches[0].mismatch_reason.is_none());

        // web-frontend: incompatible — and crucially NOT smeared with payments'
        // compatible verdict (the #260 false-negative).
        assert_eq!(matches[1].consumer_repo, "web-frontend");
        assert_eq!(
            matches[1].type_compatible,
            Some(false),
            "the incompatible consumer's edge must read Some(false), not the \
             smeared Some(true) — this is the #260 collapse"
        );
        assert_eq!(
            matches[1].mismatch_reason.as_deref(),
            Some("Order.id: number is not assignable to OrderView.id: string"),
        );
    }

    /// A GraphQL edge joins its explicit compatible verdict just like HTTP.
    #[test]
    fn overlay_compat_verdicts_graphql_edge_joins_compatible() {
        let analyzer = analyzer_with_outcomes(vec![outcome(
            "GRAPHQL",
            "query|order",
            "web-frontend/src/query.ts:7",
            VerdictBucket::Compatible,
            None,
        )]);

        let mut matches = vec![edge_at(
            "graphql|query|order",
            "web-frontend",
            "web-frontend/src/query.ts:7",
        )];
        analyzer.overlay_compat_verdicts(&mut matches);

        assert_eq!(
            matches[0].type_compatible,
            Some(true),
            "a GraphQL edge with an explicit compatible verdict reads Some(true)"
        );
        assert!(matches[0].mismatch_reason.is_none());
    }

    /// The socket cross-repo join. A `socket|DIRECTION|event` edge joins its
    /// outcome by the `("SOCKET", "<DIRECTION>|<event>")` identity that
    /// `parse_producer_key` recovers from the edge — `Some(false)` + reason
    /// on a mismatch, `Some(true)` on an explicit compatible.
    #[test]
    fn apply_pair_outcomes_joins_socket_edge() {
        let outcomes = vec![
            outcome(
                "SOCKET",
                "CLIENT->SERVER|chat:bad",
                "client/src/chat.ts:20",
                VerdictBucket::Incompatible,
                Some("Sent type is not assignable to listener type"),
            ),
            outcome(
                "SOCKET",
                "SERVER->CLIENT|payment:settled",
                "payments-svc/realtime/server.ts:27",
                VerdictBucket::Compatible,
                None,
            ),
        ];

        let mut matches = vec![
            edge_at(
                "socket|SERVER->CLIENT|payment:settled",
                "payments-svc",
                "payments-svc/realtime/server.ts:27",
            ),
            edge_at(
                "socket|CLIENT->SERVER|chat:bad",
                "chat-svc",
                "client/src/chat.ts:20:5",
            ),
        ];
        apply_pair_outcomes(&outcomes, &mut matches);

        assert_eq!(
            matches[0].type_compatible,
            Some(true),
            "the compatible socket edge reads Some(true) from its explicit verdict"
        );
        assert!(matches[0].mismatch_reason.is_none());

        assert_eq!(
            matches[1].type_compatible,
            Some(false),
            "the socket edge with an incompatible outcome reads Some(false)"
        );
        assert_eq!(
            matches[1].mismatch_reason.as_deref(),
            Some("Sent type is not assignable to listener type"),
        );
    }

    /// `parse_producer_key` recovers the `(pseudo-method, identity)` join pair
    /// from a graphql canonical key. The KIND stays lowercase on both the key
    /// and the v2 pair identity, so the two sides agree with no case folding.
    #[test]
    fn parse_producer_key_recovers_graphql_pair() {
        assert_eq!(
            parse_producer_key("graphql|query|order"),
            Some(("GRAPHQL".to_string(), "query|order".to_string())),
        );
        assert_eq!(
            parse_producer_key("graphql|subscription|orderUpdated"),
            Some((
                "GRAPHQL".to_string(),
                "subscription|orderUpdated".to_string()
            )),
        );
        // Malformed (missing field) → no join key, edge stays None.
        assert_eq!(parse_producer_key("graphql|query|"), None);
        assert_eq!(parse_producer_key("graphql|query"), None);
    }

    /// The graphql cross-repo join: incompatible direction. The outcome's
    /// `("GRAPHQL", "<kind>|<field>")` identity joins the edge's canonical
    /// key, landing `Some(false)` + reason.
    #[test]
    fn apply_pair_outcomes_joins_graphql_edge() {
        let outcomes = vec![
            outcome(
                "GRAPHQL",
                "subscription|orderUpdated",
                "web-frontend/lib/graphql.ts:80",
                VerdictBucket::Incompatible,
                Some("note?: optional producer field is not assignable to required consumer field"),
            ),
            outcome(
                "GRAPHQL",
                "query|order",
                "web-frontend/lib/graphql.ts:76",
                VerdictBucket::Compatible,
                None,
            ),
        ];

        let mut matches = vec![
            edge_at(
                "graphql|query|order",
                "web-frontend",
                "web-frontend/lib/graphql.ts:76",
            ),
            edge_at(
                "graphql|subscription|orderUpdated",
                "web-frontend",
                "web-frontend/lib/graphql.ts:80:5",
            ),
        ];
        apply_pair_outcomes(&outcomes, &mut matches);

        assert_eq!(
            matches[0].type_compatible,
            Some(true),
            "the compatible graphql edge reads Some(true) from its explicit verdict"
        );
        assert!(matches[0].mismatch_reason.is_none());

        assert_eq!(
            matches[1].type_compatible,
            Some(false),
            "the graphql edge with an incompatible outcome reads Some(false)"
        );
        assert_eq!(
            matches[1].mismatch_reason.as_deref(),
            Some("note?: optional producer field is not assignable to required consumer field"),
        );
    }

    /// Pub/sub canonical keys are 2-segment (`pubsub|<topic>`, broker excluded
    /// from identity), so the third `splitn(3, '|')` field is `None`.
    /// `parse_producer_key` must still recover `("PUBSUB", "<topic>")`. A
    /// topic carries no path params, so `normalize_compat_path` leaves it
    /// unchanged — guarding a future param-collapse refactor from silently
    /// breaking the pub/sub join.
    #[test]
    fn parse_producer_key_recovers_pubsub_pair() {
        assert_eq!(
            parse_producer_key("pubsub|order.placed"),
            Some(("PUBSUB".to_string(), "order.placed".to_string())),
        );
        // Empty topic → no join key, edge stays None.
        assert_eq!(parse_producer_key("pubsub|"), None);
        // A topic with dots/no slashes is unaffected by the HTTP path-param
        // collapse, so the two sides of the join still agree.
        assert_eq!(normalize_compat_path("order.placed"), "order.placed");
    }

    /// The pub/sub cross-repo join. The outcome's `("PUBSUB", "<topic>")`
    /// identity joins the edge's canonical key — `Some(true)` on an explicit
    /// compatible, `Some(false)` + reason on a reported mismatch.
    #[test]
    fn apply_pair_outcomes_joins_pubsub_edge() {
        let outcomes = vec![
            outcome(
                "PUBSUB",
                "order.placed",
                "orders-svc/src/publisher.ts:42",
                VerdictBucket::Incompatible,
                Some("Type 'WideOrder' is not assignable to type 'StrictOrder'"),
            ),
            outcome(
                "PUBSUB",
                "metrics.page_view",
                "analytics-svc/src/track.ts:10",
                VerdictBucket::Compatible,
                None,
            ),
        ];

        let mut matches = vec![
            edge_at(
                "pubsub|metrics.page_view",
                "analytics-svc",
                "analytics-svc/src/track.ts:10",
            ),
            edge_at(
                "pubsub|order.placed",
                "orders-svc",
                "orders-svc/src/publisher.ts:42",
            ),
        ];
        apply_pair_outcomes(&outcomes, &mut matches);

        assert_eq!(
            matches[0].type_compatible,
            Some(true),
            "the compatible pub/sub edge reads Some(true) from its explicit verdict"
        );
        assert!(matches[0].mismatch_reason.is_none());

        assert_eq!(
            matches[1].type_compatible,
            Some(false),
            "the pub/sub edge with an incompatible outcome reads Some(false)"
        );
        assert_eq!(
            matches[1].mismatch_reason.as_deref(),
            Some("Type 'WideOrder' is not assignable to type 'StrictOrder'"),
        );
    }

    /// When the check itself failed, the engine degrades every probing pair
    /// to unverifiable with the failure as the reason — the edge reads
    /// `None`, never compatible.
    #[test]
    fn overlay_compat_verdicts_treats_check_failure_as_unverifiable() {
        let analyzer = analyzer_with_outcomes(vec![outcome(
            "POST",
            "/payments",
            PAYMENTS_CONSUMER_LOC,
            VerdictBucket::Unverifiable,
            Some("type check did not run: v2 check failed: pnpm missing"),
        )]);

        let mut matches = vec![edge("http|POST|/payments")];
        analyzer.overlay_compat_verdicts(&mut matches);

        assert_eq!(
            matches[0].type_compatible, None,
            "a failed check is not a verdict — edges stay None"
        );
    }

    // ---- #378/#381: path-specificity gate on cross-repo pairing ----

    /// Repo-tagged producer endpoint for the specificity-gate tests.
    fn resolved_in(
        method: &str,
        full_path: &str,
        repo: &str,
    ) -> crate::mount_graph::ResolvedEndpoint {
        crate::mount_graph::ResolvedEndpoint {
            repo_name: Some(repo.to_string()),
            ..resolved(method, full_path)
        }
    }

    /// Consumer-side data call, repo-tagged as the cross-repo merge does, so
    /// `build_cross_repo_match` can attribute the consumer repo. The
    /// `(METHOD, canonical_path, file_location)` triple must mirror the
    /// `analyzer.calls` entry it corresponds to.
    fn data_call_in(
        method: &str,
        canonical_path: &str,
        file: &str,
        repo: &str,
    ) -> crate::mount_graph::DataFetchingCall {
        crate::mount_graph::DataFetchingCall {
            method: method.to_string(),
            target_url: canonical_path.to_string(),
            canonical_path: canonical_path.to_string(),
            client: "fetch".to_string(),
            file_location: file.to_string(),
            call_kind: None,
            repo_name: Some(repo.to_string()),
            service_name: None,
        }
    }

    /// Convention pin (#368): both sides of a cross-repo match row carry the
    /// `service_name ?? repo_name` id. In a monorepo (carrick.json
    /// `services[]`) producer AND consumer are named by service; a repo-only
    /// consumer id left rows asymmetric (`web -> <repo>`), breaking every
    /// join on edge identity — the eval scorer's owner attribution and the
    /// cloud's `attach_compat_verdicts` consumer filter included.
    #[test]
    fn match_row_ids_are_service_qualified_on_both_sides() {
        let mut analyzer = Analyzer::new(Config::default());

        analyzer.calls.push(http_call(
            "POST",
            "/api/v2/client/:workspaceId/user",
            "apps/web/src/client.ts:12",
        ));

        let mut mount_graph = MountGraph::new();
        // Producer: the `api` service of monorepo `acme-mono`.
        let mut producer = resolved_in("POST", "/api/v2/client/:workspaceId/user", "acme-mono");
        producer.service_name = Some("api".to_string());
        mount_graph.endpoints.push(producer);
        // Consumer: the `web` service of the same monorepo.
        let mut consumer = data_call_in(
            "POST",
            "/api/v2/client/:workspaceId/user",
            "apps/web/src/client.ts:12",
            "acme-mono",
        );
        consumer.service_name = Some("web".to_string());
        mount_graph.data_calls.push(consumer);

        let (_, _, edges) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        assert_eq!(edges.len(), 1, "expected one edge, got {edges:?}");
        assert_eq!(
            (
                edges[0].producer_repo.as_str(),
                edges[0].consumer_repo.as_str()
            ),
            ("api", "web"),
            "both sides must carry the service-qualified id"
        );
    }

    /// KILL (#381): a wildcard-only producer (`GET /*`, the SPA fallback
    /// shape) routes every call but corroborates none of them. The pairing
    /// must not produce a cross-repo edge, must not mark the fallback
    /// verified, must not turn the absorbed call into a missing endpoint, and
    /// must not report the fallback as orphaned.
    #[test]
    fn catch_all_only_producer_absorbs_but_never_pairs() {
        let mut analyzer = Analyzer::new(Config::default());

        analyzer.calls.push(http_call(
            "GET",
            "/internal/metrics/export",
            "metrics-svc/src/client.ts:9",
        ));

        let mut mount_graph = MountGraph::new();
        mount_graph
            .endpoints
            .push(resolved_in("GET", "/*", "site-shell"));
        mount_graph.data_calls.push(data_call_in(
            "GET",
            "/internal/metrics/export",
            "metrics-svc/src/client.ts:9",
            "metrics-svc",
        ));

        let (findings, verified, edges) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        assert!(
            edges.is_empty(),
            "a wildcard-only producer must not pair with arbitrary calls, got {edges:?}"
        );
        assert!(
            verified.is_empty(),
            "no-signal absorption is not verification, got {verified:?}"
        );
        assert!(
            findings.is_empty(),
            "absorbed call is not missing, and a catch-all is never orphaned; got {findings:?}"
        );
    }

    /// KILL (#381): a catch-all producer must not pair with a call that has a
    /// more specific in-org producer available — the call pairs with the
    /// maximal-agreement producer only.
    #[test]
    fn catch_all_never_outranks_specific_producer() {
        let mut analyzer = Analyzer::new(Config::default());

        analyzer.calls.push(http_call(
            "GET",
            "/api/v1/chat/new",
            "web-client/src/chat.ts:14",
        ));

        let mut mount_graph = MountGraph::new();
        mount_graph
            .endpoints
            .push(resolved_in("GET", "/api/**", "gateway"));
        mount_graph
            .endpoints
            .push(resolved_in("GET", "/api/v1/chat/new", "chat-svc"));
        mount_graph.data_calls.push(data_call_in(
            "GET",
            "/api/v1/chat/new",
            "web-client/src/chat.ts:14",
            "web-client",
        ));

        let (findings, verified, edges) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        assert_eq!(
            edges.len(),
            1,
            "only the most specific producer pairs, got {edges:?}"
        );
        assert_eq!(edges[0].producer_repo, "chat-svc");
        assert_eq!(edges[0].consumer_repo, "web-client");
        assert_eq!(
            verified,
            vec![VerifiedEndpointEntry::new(
                "GET".to_string(),
                "/api/v1/chat/new".to_string(),
                crate::operation::EndpointProvenance::Route
            )],
            "the catch-all is not verified by a call it lost"
        );
        assert!(
            findings.is_empty(),
            "the out-ranked catch-all is a mount, not an orphan; got {findings:?}"
        );
    }

    /// KILL (#378): a consumer call whose base is an UNDECLARED template
    /// variable (`${authHost}/oauth/token`) must not silently strip the base
    /// and pair with an in-org producer that happens to serve the same
    /// generic path. The call surfaces as an env-var advisory (declare the
    /// base in carrick.json) and the producer keeps its honest orphan state.
    #[test]
    fn unknown_template_base_is_advisory_not_match() {
        let mut analyzer = Analyzer::new(Config::default());

        analyzer.calls.push(http_call(
            "POST",
            "${authHost}/oauth/token",
            "billing-svc/src/auth.ts:21",
        ));

        let mut mount_graph = MountGraph::new();
        mount_graph
            .endpoints
            .push(resolved_in("POST", "/oauth/token", "auth-shim"));
        mount_graph.data_calls.push(data_call_in(
            "POST",
            "${authHost}/oauth/token",
            "billing-svc/src/auth.ts:21",
            "billing-svc",
        ));

        let (findings, verified, edges) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        assert!(
            edges.is_empty(),
            "an undeclared base must never pair across repos, got {edges:?}"
        );
        assert!(verified.is_empty());
        assert_eq!(
            findings,
            vec![
                Finding::orphaned_endpoint("POST", "/oauth/token", Some("auth-shim".to_string())),
                Finding::env_var_call(
                    "POST",
                    "/oauth/token",
                    "authHost",
                    vec!["billing-svc/src/auth.ts:21".into()],
                ),
            ],
            "the call is an advisory to classify the base, not a match or a missing endpoint"
        );
    }

    /// KEEP (#378): the SAME generic path pairs fine once the base is
    /// declared internal — corroboration comes from the declared base, not
    /// from the path's shape. No path is ever blanket-excluded.
    #[test]
    fn declared_internal_base_still_pairs_generic_path() {
        let config = Config {
            internal_env_vars: ["AUTH_HOST".to_string()].into_iter().collect(),
            ..Config::default()
        };
        let mut analyzer = Analyzer::new(config);

        analyzer.calls.push(http_call(
            "POST",
            "ENV_VAR:AUTH_HOST:/oauth/token",
            "billing-svc/src/auth.ts:21",
        ));

        let mut mount_graph = MountGraph::new();
        mount_graph
            .endpoints
            .push(resolved_in("POST", "/oauth/token", "auth-shim"));
        mount_graph.data_calls.push(data_call_in(
            "POST",
            "ENV_VAR:AUTH_HOST:/oauth/token",
            "billing-svc/src/auth.ts:21",
            "billing-svc",
        ));

        let (findings, verified, edges) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        assert_eq!(
            edges.len(),
            1,
            "declared base corroborates the pair, got {edges:?}"
        );
        assert_eq!(edges[0].producer_repo, "auth-shim");
        assert_eq!(edges[0].consumer_repo, "billing-svc");
        assert_eq!(edges[0].consumer_key, "http|POST|/oauth/token");
        assert_eq!(
            verified,
            vec![VerifiedEndpointEntry::new(
                "POST".to_string(),
                "/oauth/token".to_string(),
                crate::operation::EndpointProvenance::Route
            )]
        );
        assert!(findings.is_empty(), "got {findings:?}");
    }

    /// KILL (#397): a service calling its OWN endpoint through an env-var base
    /// (localhost self-calls are dropped at extraction, so this is the shape
    /// that reaches the matcher) must not emit a producer==consumer self-pair
    /// edge — the same structural rule `analyze_exact_key_matches` applies.
    /// The endpoint itself stays visible: it is verified by the self-call,
    /// not orphaned. Pins the monorepo shape from the issue, where both
    /// sides carry the same `service_name ?? repo_name` id (#368).
    #[test]
    fn http_self_pair_emits_no_edge_but_endpoint_stays_visible() {
        let config = Config {
            internal_env_vars: ["API_URL".to_string()].into_iter().collect(),
            ..Config::default()
        };
        let mut analyzer = Analyzer::new(config);

        analyzer.calls.push(http_call(
            "GET",
            "ENV_VAR:API_URL:/api/self-status",
            "apps/web/src/status.ts:7",
        ));

        let mut mount_graph = MountGraph::new();
        // Producer: the `web` service of monorepo `acme-mono`.
        let mut producer = resolved_in("GET", "/api/self-status", "acme-mono");
        producer.service_name = Some("web".to_string());
        mount_graph.endpoints.push(producer);
        // Consumer: the SAME `web` service calling its own endpoint.
        let mut consumer = data_call_in(
            "GET",
            "ENV_VAR:API_URL:/api/self-status",
            "apps/web/src/status.ts:7",
            "acme-mono",
        );
        consumer.service_name = Some("web".to_string());
        mount_graph.data_calls.push(consumer);

        let (findings, verified, edges) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        assert!(
            edges.is_empty(),
            "a producer==consumer pair is not a cross-repo edge, got {edges:?}"
        );
        assert_eq!(
            verified,
            vec![VerifiedEndpointEntry::new(
                "GET".to_string(),
                "/api/self-status".to_string(),
                crate::operation::EndpointProvenance::Route
            )],
            "the endpoint stays visible as a verified endpoint"
        );
        assert!(
            findings.is_empty(),
            "matched means neither missing nor orphaned, got {findings:?}"
        );
    }

    /// KEEP: literal paths keep pairing on their own signal — a
    /// single-segment `/nodes` as much as a deep `/api/v1/chat/new`, and a
    /// param route against a concrete caller value. No specificity threshold
    /// is applied to a pairing that literal segments corroborate.
    #[test]
    fn literal_and_param_paths_keep_pairing() {
        let mut analyzer = Analyzer::new(Config::default());

        analyzer
            .calls
            .push(http_call("GET", "/nodes", "web-client/src/graph.ts:3"));
        analyzer.calls.push(http_call(
            "GET",
            "/api/v1/chat/new",
            "web-client/src/chat.ts:14",
        ));
        analyzer
            .calls
            .push(http_call("GET", "/users/123", "web-client/src/user.ts:8"));

        let mut mount_graph = MountGraph::new();
        mount_graph
            .endpoints
            .push(resolved_in("GET", "/nodes", "graph-svc"));
        mount_graph
            .endpoints
            .push(resolved_in("GET", "/api/v1/chat/new", "chat-svc"));
        mount_graph
            .endpoints
            .push(resolved_in("GET", "/users/:id", "user-svc"));
        for (path, file) in [
            ("/nodes", "web-client/src/graph.ts:3"),
            ("/api/v1/chat/new", "web-client/src/chat.ts:14"),
            ("/users/123", "web-client/src/user.ts:8"),
        ] {
            mount_graph
                .data_calls
                .push(data_call_in("GET", path, file, "web-client"));
        }

        let (findings, verified, edges) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        let producer_repos: std::collections::BTreeSet<&str> =
            edges.iter().map(|e| e.producer_repo.as_str()).collect();
        assert_eq!(
            producer_repos,
            ["chat-svc", "graph-svc", "user-svc"].into_iter().collect(),
            "got {edges:?}"
        );
        assert_eq!(verified.len(), 3);
        assert!(findings.is_empty(), "got {findings:?}");
    }

    /// KEEP: a catch-all with a literal mount prefix (the shape file-based
    /// routing synthesizes for a Next.js `[...slug]` route) still pairs when
    /// it is the most specific producer available — the literal prefix is the
    /// corroborating signal.
    #[test]
    fn sole_prefixed_catch_all_still_pairs() {
        let mut analyzer = Analyzer::new(Config::default());

        analyzer.calls.push(http_call(
            "GET",
            "/files/report/pdf",
            "web-client/src/files.ts:5",
        ));

        let mut mount_graph = MountGraph::new();
        mount_graph
            .endpoints
            .push(resolved_in("GET", "/files/**", "files-svc"));
        mount_graph.data_calls.push(data_call_in(
            "GET",
            "/files/report/pdf",
            "web-client/src/files.ts:5",
            "web-client",
        ));

        let (findings, verified, edges) = analyzer.analyze_matches_with_mount_graph(&mount_graph);

        assert_eq!(edges.len(), 1, "got {edges:?}");
        assert_eq!(edges[0].producer_repo, "files-svc");
        assert_eq!(
            verified,
            vec![VerifiedEndpointEntry::new(
                "GET".to_string(),
                "/files/**".to_string(),
                crate::operation::EndpointProvenance::Route
            )]
        );
        assert!(findings.is_empty(), "got {findings:?}");
    }
}
