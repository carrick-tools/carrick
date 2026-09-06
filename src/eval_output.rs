//! Machine-readable scanner output for the eval harness.
//!
//! When `CARRICK_OUTPUT_JSON` is set, the engine emits an [`EvalProjection`]
//! instead of the human-readable Markdown report. This is a *dedicated* shape,
//! deliberately decoupled from the internal analyzer types, so the eval scoring
//! contract stays stable across refactors of `ApiEndpointDetails` and friends.
//! The cross-repo eval (slice S1) scores endpoint/call set accuracy, cross-repo
//! producer→consumer matches, per-op type resolution, and dependency conflicts
//! from this projection.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::analyzer::{
    ApiAnalysisResult, ApiEndpointDetails, ConflictSeverity,
    CrossRepoMatch as AnalyzerCrossRepoMatch, DependencyConflict,
};
use crate::cloud_storage::{ManifestRole, ManifestTypeKind, TypeManifestEntry};
use crate::operation::OperationKey;

/// The full eval projection of a single scan: the producer endpoints, the
/// consumer calls, the cross-repo edges between them, and the dependency
/// conflicts the scanner extracted.
#[derive(Debug, Serialize, Deserialize)]
pub struct EvalProjection {
    pub endpoints: Vec<EvalOp>,
    pub calls: Vec<EvalOp>,
    /// Structured producer→consumer edges (contract §2). The scorer keys
    /// cross-repo match P/R/F1 and the compat-verdict metric off these.
    pub cross_repo_matches: Vec<CrossRepoMatch>,
    /// Dependency version conflicts across the scanned repos (contract §4).
    pub dependency_conflicts: Vec<EvalDependencyConflict>,
}

/// One extracted operation (endpoint or call), flattened for scoring.
#[derive(Debug, Serialize, Deserialize)]
pub struct EvalOp {
    /// Stable identity from `OperationKey::canonical()`, e.g. `http|GET|/users`.
    pub key: String,
    /// `"http" | "graphql" | "socket"`.
    pub protocol: String,
    /// HTTP method, when this is an HTTP operation.
    pub method: Option<String>,
    /// HTTP path, GraphQL field, or socket event.
    pub path: Option<String>,
    pub handler: Option<String>,
    pub request_type: Option<String>,
    pub response_type: Option<String>,
    /// Consumer ops only: the raw call target as written in source
    /// (`http://comment-service/api/comments?userId=${id}`). `path` above is
    /// the canonical MATCH key and is host-free for literal-origin URLs, so
    /// this is the only field that still carries the host. The Tier-A
    /// scorer's `host_contains` labels assert against it. `None` for
    /// producer ops and for calls with no mount-graph raw target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_url: Option<String>,
    /// Consumer ops only: how the call's base resolves (carrick#649), the same
    /// value the persisted row carries. `path` is the route and `target_url` is
    /// the expression; this is the only field that says where the base goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<crate::call_base::CallBaseResolution>,
    /// Consumer ops only: what the member join could not follow for the client
    /// method this call reaches (carrick#656), the same value the persisted row
    /// carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumers_not_resolved: Option<crate::imported_request_member::UnfollowedMemberSites>,
    /// Which layer stated this row, and for a deterministic row which pass
    /// (carrick#660), exactly as the persisted operation carries it. `None`
    /// for an op that does not come from the HTTP emit/join phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_source: Option<crate::agents::file_analyzer_agent::ResolutionSource>,
    pub file: String,
    pub line: u32,
    // --- Type-manifest fields (contract §3), joined by OperationKey ---
    /// The type alias used in the bundled `.d.ts` file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_alias: Option<String>,
    /// `ManifestTypeState` in String form: `"Explicit"` / `"Implicit"` / `"Unknown"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_state: Option<String>,
    /// Original declaration text as written (sidecar `resolved_definition`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_definition: Option<String>,
    /// Compiler-expanded form with all types inlined (sidecar `expanded_definition`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded_definition: Option<String>,
    /// Whether the type was explicitly annotated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_explicit: Option<bool>,
    /// The LLM-emitted type anchor (contract §3, §5 row 5).
    ///
    /// The real anchor (`primary_type_symbol` on the file-analyzer result) is
    /// threaded onto the type manifest at build time, joined by `(file_path,
    /// line_number)`, so this carries the real source symbol (`StatusResponse`)
    /// rather than the hashed `type_alias`. Falls back to `type_alias` only when
    /// no real anchor was extracted for the op.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_type_symbol: Option<String>,
    /// Where `primary_type_symbol` is DECLARED (carrick#649), from the manifest
    /// entry. `file`/`line` on this op are the operation's own site — a route's
    /// annotation, or an import line — so this is the only field that answers
    /// "where is this type defined".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defined_in: Option<crate::cloud_storage::TypeHome>,
}

/// A structured producer→consumer edge (contract §2 FROZEN field set).
///
/// `producer_repo` and `consumer_repo` both carry the `service_name ??
/// repo_name` id — one convention on BOTH sides (#368). In a plain repo that
/// is the repo name; in a monorepo with carrick.json `services[]` it is the
/// service name, on the producer AND the consumer, so a join on edge identity
/// never sees a service on one side and a repo on the other.
#[derive(Debug, Serialize, Deserialize)]
pub struct CrossRepoMatch {
    pub producer_repo: String,
    pub producer_key: String,
    pub consumer_repo: String,
    pub consumer_key: String,
    pub match_score: f64,
    /// `None` = compat NOT evaluated for this edge; `Some(b)` = evaluated.
    /// Omitted from JSON when `None` so the scorer never reads absent compat
    /// data as a verdict (the silently-skipped-check trap, contract §7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_compatible: Option<bool>,
    /// `Some(..)` iff `type_compatible == Some(false)`; omitted otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mismatch_reason: Option<String>,
    /// Additive (#379): `"producer_consumer"` or `"shared_external_contract"`
    /// (`carrick_match::MatchRelationship`'s serde wire values). For
    /// shared-external-contract edges the `producer_*`/`consumer_*` names
    /// carry no roles — both sides are call-site encodings of the same
    /// external contract. Defaults to producer/consumer so projections
    /// recorded before this field existed keep their meaning.
    #[serde(default = "default_relationship")]
    pub relationship: String,
}

/// The pre-#379 implied relationship of every edge.
fn default_relationship() -> String {
    "producer_consumer".to_string()
}

/// A dependency version conflict (contract §4). `versions` is sorted ascending
/// for stable comparison against the answer key.
#[derive(Debug, Serialize, Deserialize)]
pub struct EvalDependencyConflict {
    pub package: String,
    pub versions: Vec<String>,
    pub severity: String,
}

impl EvalProjection {
    /// Build the projection from analyzer results plus the merged type manifest.
    /// The manifest is joined per op by `OperationKey`; deps come straight from
    /// `result.dependency_conflicts`; matches from `result.cross_repo_matches`;
    /// `raw_targets` (from [`ApiAnalyzer::call_raw_targets`], keyed
    /// `(canonical key, file location)`) attaches each consumer op's raw source
    /// target so host-asserting eval labels can still see a literal origin the
    /// canonical key deliberately strips.
    pub fn from_results(
        result: &ApiAnalysisResult,
        type_manifest: &[TypeManifestEntry],
        raw_targets: &HashMap<(String, String), String>,
        call_bases: &HashMap<(String, String), crate::call_base::CallBaseResolution>,
        call_unfollowed_members: &HashMap<
            (String, String),
            crate::imported_request_member::UnfollowedMemberSites,
        >,
    ) -> Self {
        let manifest_index = ManifestIndex::build(type_manifest);
        // Canonical ordering so the projection is deterministic across runs:
        // the matcher's edge order and analyze_dependencies' conflict order are
        // both unstable, which would otherwise flake the cassette hard gate.
        let mut cross_repo_matches: Vec<CrossRepoMatch> = result
            .cross_repo_matches
            .iter()
            .map(CrossRepoMatch::from_analyzer)
            .collect();
        cross_repo_matches.sort_by(|a, b| {
            (
                &a.producer_repo,
                &a.producer_key,
                &a.consumer_repo,
                &a.consumer_key,
            )
                .cmp(&(
                    &b.producer_repo,
                    &b.producer_key,
                    &b.consumer_repo,
                    &b.consumer_key,
                ))
        });
        let mut dependency_conflicts: Vec<EvalDependencyConflict> = result
            .dependency_conflicts
            .iter()
            .map(EvalDependencyConflict::from_conflict)
            .collect();
        dependency_conflicts.sort_by(|a, b| a.package.cmp(&b.package));
        // Intra-repo pub/sub (and any exact-key) self-loops must not surface in
        // the cross-repo projection. A topic that ONE repo both produces and
        // consumes, with no other repo on the key, is a self-edge — not a
        // contract edge (the matcher already drops the self-EDGE; this keeps the
        // producer/consumer OPS out of the endpoint/call sets too, so e.g. a
        // dead-letter retry loop can't inflate them). Keyed on repo identity +
        // canonical key alone — never on topic-name or library patterns — so it
        // behaves identically for Kafka/Redis/NATS/RabbitMQ and socket events. A
        // key that any OTHER repo also touches (fan-in/fan-out), or that a single
        // repo only produces OR only consumes (an orphan), is left untouched.
        let self_loops = Self::intra_repo_self_loops(result);
        // Drop an op only when its OWN repo attribution matches the self-loop
        // repo for its key. An op with no attribution is undecidable (it might
        // belong to a different, untagged repo) and is kept — mirroring the
        // skip in `intra_repo_self_loops`.
        let is_self_loop_op = |d: &&ApiEndpointDetails| {
            self_loops.get(&d.key.canonical()).is_some_and(|loop_repo| {
                d.service_name.as_deref().or(d.repo_name.as_deref()) == Some(loop_repo)
            })
        };
        Self {
            // `endpoints` are producers and `calls` are consumers; the role
            // selects which manifest slot each op joins to. A shared canonical
            // key (e.g. an HTTP path produced in one repo and consumed in
            // another) carries one entry per role, so without this split the two
            // ops would collapse into a single slot and clobber each other's
            // anchor / resolved-definition / type-state (the projection-collapse
            // finding, #207).
            endpoints: result
                .endpoints
                .iter()
                .filter(|d| !is_self_loop_op(d))
                .map(|d| EvalOp::from_details(d, &manifest_index, ManifestRole::Producer))
                .collect(),
            calls: result
                .calls
                .iter()
                .filter(|d| !is_self_loop_op(d))
                .map(|d| {
                    let mut op = EvalOp::from_details(d, &manifest_index, ManifestRole::Consumer);
                    let site = (
                        d.key.canonical(),
                        d.file_path.to_string_lossy().into_owned(),
                    );
                    op.target_url = raw_targets.get(&site).cloned();
                    op.base = call_bases.get(&site).cloned();
                    op.consumers_not_resolved = call_unfollowed_members.get(&site).cloned();
                    op
                })
                .collect(),
            cross_repo_matches,
            dependency_conflicts,
        }
    }

    /// Pure intra-repo self-loops among exact-key (non-HTTP) operations:
    /// canonical key → the single repo that both produces AND consumes it, with
    /// no other repo on the key. The projection filters out the ops of that repo
    /// on that key.
    ///
    /// HTTP ops are excluded — they get repo provenance from the mount graph, not
    /// from `repo_name`; localhost self-calls are already dropped at extraction,
    /// and the HTTP matcher drops self-pair EDGES on repo identity (#397) while
    /// keeping the ops visible as endpoint/call. A non-HTTP op with no repo
    /// attribution is skipped (self-loop is
    /// undecidable without both repo ids), on both sides: it neither votes here
    /// nor gets dropped by the filter.
    fn intra_repo_self_loops(result: &ApiAnalysisResult) -> HashMap<String, String> {
        use std::collections::HashSet;
        let mut producer_repos: HashMap<String, HashSet<&str>> = HashMap::new();
        let mut consumer_repos: HashMap<String, HashSet<&str>> = HashMap::new();
        for (ops, repos) in [
            (&result.endpoints, &mut producer_repos),
            (&result.calls, &mut consumer_repos),
        ] {
            for d in ops {
                if matches!(d.key.protocol(), crate::operation::Protocol::Http) {
                    continue;
                }
                if let Some(repo) = d.service_name.as_deref().or(d.repo_name.as_deref()) {
                    repos.entry(d.key.canonical()).or_default().insert(repo);
                }
            }
        }
        let mut self_loops = HashMap::new();
        for (key, producers) in &producer_repos {
            let Some(consumers) = consumer_repos.get(key) else {
                continue; // orphan producer — surfaces as an endpoint, not a self-loop
            };
            let repos: HashSet<&str> = producers.iter().chain(consumers.iter()).copied().collect();
            if repos.len() == 1 {
                let repo = repos.iter().next().expect("len checked above");
                self_loops.insert(key.clone(), repo.to_string());
            }
        }
        self_loops
    }
}

/// The manifest fields an op carries, selected from the request/response
/// manifest entries for a single operation key at the op's own call/handler
/// site.
#[derive(Default)]
struct ManifestFields {
    type_alias: Option<String>,
    type_state: Option<String>,
    resolved_definition: Option<String>,
    expanded_definition: Option<String>,
    is_explicit: Option<bool>,
    /// Real LLM type-anchor symbol, threaded onto the manifest entry at build
    /// time. Distinct from `type_alias` (the synthetic hashed
    /// `Endpoint_<hash>_Response` name); the anchor metric scores against this.
    primary_type_symbol: Option<String>,
    /// Where the anchor symbol is DECLARED (carrick#649), as opposed to
    /// `file`/`line` on the op, which are where it is used.
    defined_in: Option<crate::cloud_storage::TypeHome>,
}

/// One manifest entry, kept whole (kind + site discriminators intact) so the
/// per-op join can pick the right entry instead of collapsing them.
struct ManifestRecord {
    type_kind: ManifestTypeKind,
    file_path: String,
    line_number: u32,
    type_alias: String,
    type_state: String,
    resolved_definition: Option<String>,
    expanded_definition: Option<String>,
    is_explicit: bool,
    primary_type_symbol: Option<String>,
    defined_in: Option<crate::cloud_storage::TypeHome>,
}

impl ManifestRecord {
    fn has_definition(&self) -> bool {
        self.resolved_definition.is_some() || self.expanded_definition.is_some()
    }
}

/// Lookup from `(canonical operation key, role)` → the manifest entries for
/// that op-and-role.
///
/// Keying on role as well as the canonical key is load-bearing (#207): a single
/// canonical key (e.g. `http|GET|/orders/:param`) can be both a producer in one
/// repo and a consumer in another. Keying by canonical alone collapsed the two
/// roles' entries into one slot, so the last writer's anchor /
/// resolved-definition / type-state clobbered the other role's.
///
/// Within a `(key, role)` slot the entries are NOT interchangeable either, on
/// two axes the previous first-definition-wins collapse got wrong:
/// - **kind**: an op has up to a request and a response entry. The eval's
///   per-op resolved-type labels are the op's response contract, so a request
///   entry that happened to resolve first must not displace the response
///   (`POST /payments` / `POST /track` surfaced their request shapes).
/// - **site**: fan-in — several call sites on the same key (two repos
///   publishing one pub/sub topic, #290) — carries one entry per site. All
///   sites shared whichever entry won, so one publisher's payload masked the
///   other's. Each op joins the records at its own file/line.
struct ManifestIndex {
    by_key: HashMap<(String, ManifestRole), Vec<ManifestRecord>>,
}

impl ManifestIndex {
    fn build(entries: &[TypeManifestEntry]) -> Self {
        let mut by_key: HashMap<(String, ManifestRole), Vec<ManifestRecord>> = HashMap::new();
        for entry in entries {
            by_key
                .entry((entry.key.canonical(), entry.role))
                .or_default()
                .push(ManifestRecord {
                    type_kind: entry.type_kind,
                    file_path: entry.file_path.clone(),
                    line_number: entry.line_number,
                    type_alias: entry.type_alias.clone(),
                    type_state: manifest_type_state_string(entry.type_state),
                    resolved_definition: entry.resolved_definition.clone(),
                    expanded_definition: entry.expanded_definition.clone(),
                    is_explicit: entry.is_explicit,
                    primary_type_symbol: entry.primary_type_symbol.clone(),
                    defined_in: entry.defined_in.clone(),
                });
        }
        Self { by_key }
    }

    /// Join one op to its manifest fields.
    ///
    /// 1. **Site filter**: keep the records in the op's own file (suffix-aligned
    ///    so a repo-relative manifest path matches an absolute op path). When
    ///    nothing matches — single-site slots, or a path convention drift — all
    ///    records stay in play, which is exactly the pre-site-aware behavior.
    /// 2. **Kind preference**: the op-level fields carry the RESPONSE entry
    ///    (the contract the per-op eval labels are written against), falling
    ///    back to a request entry only when no response entry resolved a
    ///    definition, so request-only ops (`POST /billing/charge`) still
    ///    surface their one known type.
    /// 3. **Line proximity**: same-kind records left after (1)+(2) are same-file
    ///    fan-in; the record nearest the op's line is the op's own.
    fn get(
        &self,
        key: &str,
        role: ManifestRole,
        op_file: &str,
        op_line: u32,
    ) -> Option<ManifestFields> {
        let records = self.by_key.get(&(key.to_string(), role))?;
        let site: Vec<&ManifestRecord> = {
            let in_file: Vec<&ManifestRecord> = records
                .iter()
                .filter(|r| same_source_file(&r.file_path, op_file))
                .collect();
            if in_file.is_empty() {
                records.iter().collect()
            } else {
                in_file
            }
        };
        let nearest = |kind: ManifestTypeKind, need_definition: bool| {
            site.iter()
                .filter(|r| r.type_kind == kind && (!need_definition || r.has_definition()))
                .min_by_key(|r| r.line_number.abs_diff(op_line))
                .copied()
        };
        let definition_record = nearest(ManifestTypeKind::Response, true)
            .or_else(|| nearest(ManifestTypeKind::Request, true))
            .or_else(|| {
                site.iter()
                    .min_by_key(|r| r.line_number.abs_diff(op_line))
                    .copied()
            })?;
        // The anchor symbol is independent of the definition-richness pick: a
        // response entry that wins on definition but carries no symbol must not
        // erase a request entry's symbol.
        let primary_type_symbol = definition_record
            .primary_type_symbol
            .clone()
            .or_else(|| site.iter().find_map(|r| r.primary_type_symbol.clone()));
        // Same rule as the anchor above, for the same reason: the declaration
        // site belongs to whichever entry carries the symbol.
        let defined_in = definition_record
            .defined_in
            .clone()
            .or_else(|| site.iter().find_map(|r| r.defined_in.clone()));
        Some(ManifestFields {
            defined_in,
            type_alias: Some(definition_record.type_alias.clone()),
            type_state: Some(definition_record.type_state.clone()),
            resolved_definition: definition_record.resolved_definition.clone(),
            expanded_definition: definition_record.expanded_definition.clone(),
            is_explicit: Some(definition_record.is_explicit),
            primary_type_symbol,
        })
    }
}

/// Whether two source paths name the same file across the repo-relative vs
/// absolute conventions the manifest and op details mix: equal, or one is a
/// path-component-aligned suffix of the other. Both separator styles count as
/// a component boundary so a Windows-style absolute path still aligns against
/// a repo-relative one.
fn same_source_file(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    long.ends_with(short) && matches!(long.as_bytes()[long.len() - short.len() - 1], b'/' | b'\\')
}

/// `ManifestTypeState` → its String form, matching the manifest's serde naming
/// (`"Explicit"` / `"Implicit"` / `"Unknown"`).
fn manifest_type_state_string(state: crate::cloud_storage::ManifestTypeState) -> String {
    use crate::cloud_storage::ManifestTypeState::*;
    match state {
        Explicit => "Explicit",
        Implicit => "Implicit",
        Unknown => "Unknown",
    }
    .to_string()
}

impl EvalOp {
    /// `role` selects the manifest slot this op joins to: a producer (endpoint)
    /// reads the Producer slot, a consumer (call) the Consumer slot, so a shared
    /// canonical key keeps each side's anchor / resolved-definition / type-state
    /// distinct (#207).
    fn from_details(d: &ApiEndpointDetails, manifest: &ManifestIndex, role: ManifestRole) -> Self {
        let (protocol, method, path) = project_key(&d.key);
        let (file, line) = split_location(&d.file_path);
        let canonical = d.key.canonical();
        let fields = manifest.get(&canonical, role, &file, line);
        EvalOp {
            key: canonical,
            protocol,
            method,
            path,
            // Consumer-only, attached by the caller from the mount graph, for
            // the same reason `target_url` is: the details type carries
            // neither.
            base: None,
            consumers_not_resolved: None,
            // Carried by the details type itself (carrick#660), on both sides.
            resolution_source: d.resolution_source,
            handler: d.handler_name.clone(),
            request_type: d
                .request_type
                .as_ref()
                .map(|t| t.composite_type_string.clone()),
            response_type: d
                .response_type
                .as_ref()
                .map(|t| t.composite_type_string.clone()),
            target_url: None,
            file,
            line,
            type_alias: fields.as_ref().and_then(|f| f.type_alias.clone()),
            type_state: fields.as_ref().and_then(|f| f.type_state.clone()),
            resolved_definition: fields.as_ref().and_then(|f| f.resolved_definition.clone()),
            expanded_definition: fields.as_ref().and_then(|f| f.expanded_definition.clone()),
            is_explicit: fields.as_ref().and_then(|f| f.is_explicit),
            // The real LLM type anchor (#233): the symbol threaded onto the
            // manifest entry. NO fallback to the hashed `type_alias` — an op whose
            // response is an inline/anonymous type (e.g. `GET /users/recent`
            // returning a bare `{ count; ids }`) has no named symbol, so its anchor
            // is genuinely `None`. Substituting the synthetic `Endpoint_<hash>`
            // alias there fabricates an anchor the source never declared and mis-
            // scores against an expected `None`.
            primary_type_symbol: fields.as_ref().and_then(|f| f.primary_type_symbol.clone()),
            // Where that anchor is DECLARED (carrick#649). `file`/`line` above
            // are the operation's site; a reader answering "where is this type
            // defined" from them answers wrongly.
            defined_in: fields.and_then(|f| f.defined_in.clone()),
        }
    }
}

impl CrossRepoMatch {
    fn from_analyzer(m: &AnalyzerCrossRepoMatch) -> Self {
        CrossRepoMatch {
            producer_repo: m.producer_repo.clone(),
            producer_key: m.producer_key.clone(),
            consumer_repo: m.consumer_repo.clone(),
            consumer_key: m.consumer_key.clone(),
            match_score: m.match_score,
            type_compatible: m.type_compatible,
            mismatch_reason: m.mismatch_reason.clone(),
            relationship: match m.relationship {
                carrick_match::MatchRelationship::ProducerConsumer => "producer_consumer",
                carrick_match::MatchRelationship::SharedExternalContract => {
                    "shared_external_contract"
                }
            }
            .to_string(),
        }
    }
}

impl EvalDependencyConflict {
    fn from_conflict(c: &DependencyConflict) -> Self {
        let mut versions: Vec<String> = c.repos.iter().map(|r| r.version.clone()).collect();
        versions.sort();
        versions.dedup();
        EvalDependencyConflict {
            package: c.package_name.clone(),
            versions,
            severity: conflict_severity_string(&c.severity),
        }
    }
}

/// Render `ConflictSeverity` to a stable lowercase string. Mirrors the enum's
/// variant identity (`critical` / `warning` / `info`) so the eval answer key
/// compares against a stable token rather than display prose.
fn conflict_severity_string(severity: &ConflictSeverity) -> String {
    match severity {
        ConflictSeverity::Critical => "critical",
        ConflictSeverity::Warning => "warning",
        ConflictSeverity::Info => "info",
    }
    .to_string()
}

/// `(protocol, method, path)` projected from the operation key. For non-HTTP
/// protocols `method` is `None` and `path` carries the field / event name.
fn project_key(key: &OperationKey) -> (String, Option<String>, Option<String>) {
    match key {
        OperationKey::Http { method, path } => {
            ("http".to_string(), Some(method.clone()), Some(path.clone()))
        }
        OperationKey::Graphql { field, .. } => ("graphql".to_string(), None, Some(field.clone())),
        OperationKey::Socket { event, .. } => ("socket".to_string(), None, Some(event.clone())),
        OperationKey::Pubsub { topic } => ("pubsub".to_string(), None, Some(topic.clone())),
    }
}

/// `file_path` is stored as `"<file>:<line>"` for deterministic output. Split it
/// back into a path and a best-effort line number (0 when absent/unparseable).
/// `file`/`line` are informational only — scoring keys off method + path.
fn split_location(p: &Path) -> (String, u32) {
    let s = p.to_string_lossy();
    match s.rsplit_once(':') {
        Some((file, line)) if !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()) => {
            (file.to_string(), line.parse().unwrap_or(0))
        }
        _ => (s.into_owned(), 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::{
        ConflictSeverity, CrossRepoMatch as AnalyzerCrossRepoMatch, DependencyConflict,
        RepoPackageInfo,
    };
    use crate::cloud_storage::{
        ManifestRole, ManifestTypeKind, ManifestTypeState, TypeEvidence, TypeManifestEntry,
    };
    use crate::services::type_sidecar::InferKind;
    use serde_json::Value;
    use std::path::PathBuf;

    fn manifest_entry(
        key: OperationKey,
        role: ManifestRole,
        type_state: ManifestTypeState,
        is_explicit: bool,
        resolved: Option<&str>,
        primary_type_symbol: Option<&str>,
    ) -> TypeManifestEntry {
        manifest_entry_at(
            key,
            role,
            ManifestTypeKind::Response,
            "Endpoint_abc_Response",
            "src/orders.ts",
            12,
            type_state,
            is_explicit,
            resolved,
            primary_type_symbol,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn manifest_entry_at(
        key: OperationKey,
        role: ManifestRole,
        type_kind: ManifestTypeKind,
        type_alias: &str,
        file_path: &str,
        line_number: u32,
        type_state: ManifestTypeState,
        is_explicit: bool,
        resolved: Option<&str>,
        primary_type_symbol: Option<&str>,
    ) -> TypeManifestEntry {
        TypeManifestEntry {
            key,
            role,
            type_kind,
            type_alias: type_alias.to_string(),
            file_path: file_path.to_string(),
            line_number,
            is_explicit,
            type_state,
            evidence: TypeEvidence {
                file_path: file_path.to_string(),
                span_start: None,
                span_end: None,
                line_number,
                infer_kind: match type_kind {
                    ManifestTypeKind::Request => InferKind::RequestBody,
                    ManifestTypeKind::Response => InferKind::ResponseBody,
                },
                is_explicit,
                type_state,
            },
            resolved_definition: resolved.map(String::from),
            expanded_definition: None,
            primary_type_symbol: primary_type_symbol.map(String::from),
            defined_in: None,
        }
    }

    fn endpoint(method: &str, path: &str, file_line: &str) -> ApiEndpointDetails {
        ApiEndpointDetails {
            view_module: false,
            owner: None,
            key: OperationKey::http(method, path.to_string()),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: Some("getOrder".to_string()),
            request_type: None,
            response_type: None,
            file_path: PathBuf::from(file_line),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            resolution_source: None,
        }
    }

    fn pubsub_op(topic: &str, file_line: &str, repo: &str) -> ApiEndpointDetails {
        ApiEndpointDetails {
            view_module: false,
            owner: None,
            key: OperationKey::pubsub(topic),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: None,
            response_type: None,
            file_path: PathBuf::from(file_line),
            repo_name: Some(repo.to_string()),
            service_name: None,
            provenance: Default::default(),
            resolution_source: None,
        }
    }

    /// Intra-repo pub/sub self-loop (corpus-2 `_must_not_emit`): a topic one repo
    /// both publishes and subscribes, with no other repo on it, must NOT surface
    /// as an endpoint or a call in the projection. A fan-in topic (distinct
    /// producer/consumer repos) and an orphan producer (no consumer) must survive.
    #[test]
    fn intra_repo_pubsub_self_loop_dropped_from_projection() {
        let result = ApiAnalysisResult {
            // subscribers are producers (endpoints); publishers are consumers (calls)
            endpoints: vec![
                // self-loop: orders-engine subscribes __dlq.retry
                pubsub_op(
                    "__dlq.retry",
                    "orders-engine/src/kafka/dlq.ts:26",
                    "orders-engine",
                ),
                // fan-in: billing-svc subscribes order.placed
                pubsub_op(
                    "order.placed",
                    "billing-svc/src/consume.ts:4",
                    "billing-svc",
                ),
                // orphan producer: nobody publishes user.registered
                pubsub_op(
                    "user.registered",
                    "orders-engine/src/consume.ts:9",
                    "orders-engine",
                ),
            ],
            calls: vec![
                // self-loop: orders-engine publishes __dlq.retry
                pubsub_op(
                    "__dlq.retry",
                    "orders-engine/src/kafka/dlq.ts:19",
                    "orders-engine",
                ),
                // fan-in: orders-engine publishes order.placed (other repo consumes)
                pubsub_op(
                    "order.placed",
                    "orders-engine/src/producer.ts:7",
                    "orders-engine",
                ),
            ],
            findings: vec![],
            dependency_conflicts: vec![],
            verified_endpoints: vec![],
            sdk_edges: Vec::new(),
            sdk_unresolved: Vec::new(),
            detected_graphql_libraries: vec![],
            graphql_operations_indexed: false,
            cross_repo_matches: vec![],
        };

        let projection = EvalProjection::from_results(
            &result,
            &[],
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        let endpoint_keys: Vec<&str> = projection
            .endpoints
            .iter()
            .map(|o| o.key.as_str())
            .collect();
        let call_keys: Vec<&str> = projection.calls.iter().map(|o| o.key.as_str()).collect();

        // Self-loop dropped from BOTH sets.
        assert!(
            !endpoint_keys.contains(&"pubsub|__dlq.retry"),
            "self-loop endpoint must be dropped, got {endpoint_keys:?}"
        );
        assert!(
            !call_keys.contains(&"pubsub|__dlq.retry"),
            "self-loop call must be dropped, got {call_keys:?}"
        );
        // Fan-in (distinct repos) survives.
        assert!(endpoint_keys.contains(&"pubsub|order.placed"));
        assert!(call_keys.contains(&"pubsub|order.placed"));
        // Orphan producer (no consumer) survives.
        assert!(endpoint_keys.contains(&"pubsub|user.registered"));
    }

    /// An op WITHOUT repo attribution on a self-loop key must survive: with no
    /// repo id it is undecidable whether it belongs to the self-looping repo or
    /// to another (untagged) participant, so only the attributed self-loop ops
    /// are dropped.
    #[test]
    fn unattributed_op_on_self_loop_key_survives() {
        let result = ApiAnalysisResult {
            endpoints: vec![
                // attributed self-loop producer: dropped
                pubsub_op(
                    "__dlq.retry",
                    "orders-engine/src/kafka/dlq.ts:26",
                    "orders-engine",
                ),
                // UNATTRIBUTED producer on the same key: kept (undecidable)
                ApiEndpointDetails {
                    repo_name: None,
                    ..pubsub_op("__dlq.retry", "unknown/src/consume.ts:3", "ignored")
                },
            ],
            calls: vec![
                // attributed self-loop consumer: dropped
                pubsub_op(
                    "__dlq.retry",
                    "orders-engine/src/kafka/dlq.ts:19",
                    "orders-engine",
                ),
            ],
            findings: vec![],
            dependency_conflicts: vec![],
            verified_endpoints: vec![],
            sdk_edges: Vec::new(),
            sdk_unresolved: Vec::new(),
            detected_graphql_libraries: vec![],
            graphql_operations_indexed: false,
            cross_repo_matches: vec![],
        };

        let projection = EvalProjection::from_results(
            &result,
            &[],
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        let surviving: Vec<(&str, &str)> = projection
            .endpoints
            .iter()
            .map(|o| (o.key.as_str(), o.file.as_str()))
            .collect();
        assert_eq!(
            surviving,
            vec![("pubsub|__dlq.retry", "unknown/src/consume.ts")],
            "only the unattributed op survives; attributed self-loop ops are dropped"
        );
        assert!(
            projection.calls.is_empty(),
            "attributed self-loop call must be dropped, got {:?}",
            projection.calls.iter().map(|o| &o.key).collect::<Vec<_>>()
        );
    }

    /// Build a projection with a cross-repo match, manifest-joined op fields,
    /// and a dependency conflict, then assert the serialized JSON shape.
    #[test]
    fn projection_serializes_full_cross_repo_shape() {
        let producer_key = OperationKey::http("GET", "/orders/:param");
        let endpoints = vec![endpoint("GET", "/orders/:param", "src/orders.ts:12")];

        let matches = vec![
            // Compat evaluated, compatible.
            AnalyzerCrossRepoMatch {
                producer_repo: "orders-monorepo".to_string(),
                producer_key: producer_key.canonical(),
                consumer_repo: "payments-svc".to_string(),
                consumer_key: "http|GET|/orders/:param".to_string(),
                consumer_location: Some("payments-svc/src/orders-client.ts:18".to_string()),
                match_score: 1.0,
                type_compatible: Some(true),
                type_verdict: None,
                mismatch_reason: None,
                producer_provenance: Default::default(),
                relationship: carrick_match::MatchRelationship::ProducerConsumer,
            },
            // Compat evaluated, incompatible.
            AnalyzerCrossRepoMatch {
                producer_repo: "orders-monorepo".to_string(),
                producer_key: producer_key.canonical(),
                consumer_repo: "web-frontend".to_string(),
                consumer_key: "http|GET|/orders/:param".to_string(),
                consumer_location: Some("web-frontend/src/orders.ts:42".to_string()),
                match_score: 1.0,
                type_compatible: Some(false),
                type_verdict: None,
                mismatch_reason: Some("id: number vs string".to_string()),
                producer_provenance: Default::default(),
                relationship: carrick_match::MatchRelationship::ProducerConsumer,
            },
            // Compat NOT evaluated (the load-bearing None).
            AnalyzerCrossRepoMatch {
                producer_repo: "orders-monorepo".to_string(),
                producer_key: "http|POST|/payments".to_string(),
                consumer_repo: "web-frontend".to_string(),
                consumer_key: "http|POST|/payments".to_string(),
                consumer_location: Some("web-frontend/src/payments.ts:7".to_string()),
                match_score: 1.0,
                type_compatible: None,
                type_verdict: None,
                mismatch_reason: None,
                producer_provenance: Default::default(),
                relationship: carrick_match::MatchRelationship::ProducerConsumer,
            },
        ];

        let deps = vec![DependencyConflict {
            package_name: "zod".to_string(),
            // Deliberately out of order to prove the projection sorts versions.
            repos: vec![
                RepoPackageInfo {
                    repo_name: "web-frontend".to_string(),
                    version: "3.23.0".to_string(),
                    source_path: PathBuf::from("web-frontend/package.json"),
                },
                RepoPackageInfo {
                    repo_name: "payments-svc".to_string(),
                    version: "3.22.0".to_string(),
                    source_path: PathBuf::from("payments-svc/package.json"),
                },
            ],
            severity: ConflictSeverity::Warning,
        }];

        let result = ApiAnalysisResult {
            endpoints,
            calls: vec![],
            findings: vec![],
            dependency_conflicts: deps,
            verified_endpoints: vec![],
            sdk_edges: Vec::new(),
            sdk_unresolved: Vec::new(),
            detected_graphql_libraries: vec![],
            graphql_operations_indexed: false,
            cross_repo_matches: matches,
        };

        let manifest = vec![manifest_entry(
            producer_key,
            ManifestRole::Producer,
            ManifestTypeState::Explicit,
            true,
            Some("{ id: number; amountCents: number }"),
            Some("OrderResponse"),
        )];

        let projection = EvalProjection::from_results(
            &result,
            &manifest,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        let json: Value =
            serde_json::from_str(&serde_json::to_string(&projection).unwrap()).unwrap();

        // --- top-level keys present, snake_case ---
        assert!(json.get("cross_repo_matches").is_some());
        assert!(json.get("dependency_conflicts").is_some());

        // --- cross_repo_matches shape ---
        let cms = json["cross_repo_matches"].as_array().unwrap();
        assert_eq!(cms.len(), 3);

        let compatible = &cms[0];
        assert_eq!(compatible["producer_repo"], "orders-monorepo");
        assert_eq!(compatible["producer_key"], "http|GET|/orders/:param");
        assert_eq!(compatible["consumer_repo"], "payments-svc");
        assert_eq!(compatible["match_score"], 1.0);
        assert_eq!(compatible["type_compatible"], true);
        // Additive #379 field: the evidence-kind classification travels.
        assert_eq!(compatible["relationship"], "producer_consumer");
        // mismatch_reason omitted when None.
        assert!(compatible.get("mismatch_reason").is_none());

        let incompatible = &cms[1];
        assert_eq!(incompatible["type_compatible"], false);
        assert_eq!(incompatible["mismatch_reason"], "id: number vs string");

        // type_compatible: None must be OMITTED, not serialized as null.
        let not_evaluated = &cms[2];
        assert!(
            not_evaluated.get("type_compatible").is_none(),
            "type_compatible: None must be omitted from JSON, got {:?}",
            not_evaluated
        );
        assert!(not_evaluated.get("mismatch_reason").is_none());

        // --- manifest-joined op fields ---
        let op = &json["endpoints"].as_array().unwrap()[0];
        assert_eq!(op["key"], "http|GET|/orders/:param");
        assert_eq!(op["type_state"], "Explicit");
        assert_eq!(op["is_explicit"], true);
        assert_eq!(
            op["resolved_definition"],
            "{ id: number; amountCents: number }"
        );
        assert_eq!(op["type_alias"], "Endpoint_abc_Response");
        // The real LLM anchor surfaces — NOT the hashed type_alias (#233).
        assert_eq!(op["primary_type_symbol"], "OrderResponse");
        // expanded_definition was None → omitted.
        assert!(op.get("expanded_definition").is_none());

        // --- dependency_conflicts shape: versions sorted ascending ---
        let dc = &json["dependency_conflicts"].as_array().unwrap()[0];
        assert_eq!(dc["package"], "zod");
        assert_eq!(
            dc["versions"].as_array().unwrap(),
            &vec![Value::from("3.22.0"), Value::from("3.23.0")]
        );
        assert_eq!(dc["severity"], "warning");
    }

    /// An op with no manifest entry must omit every manifest field (omit-if-none).
    #[test]
    fn op_without_manifest_omits_type_fields() {
        let result = ApiAnalysisResult {
            endpoints: vec![endpoint("GET", "/api/v1/status", "src/status.ts:3")],
            calls: vec![],
            findings: vec![],
            dependency_conflicts: vec![],
            verified_endpoints: vec![],
            sdk_edges: Vec::new(),
            sdk_unresolved: Vec::new(),
            detected_graphql_libraries: vec![],
            graphql_operations_indexed: false,
            cross_repo_matches: vec![],
        };

        let projection = EvalProjection::from_results(
            &result,
            &[],
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        let json: Value =
            serde_json::from_str(&serde_json::to_string(&projection).unwrap()).unwrap();
        let op = &json["endpoints"].as_array().unwrap()[0];

        for field in [
            "type_alias",
            "type_state",
            "resolved_definition",
            "expanded_definition",
            "is_explicit",
            "primary_type_symbol",
        ] {
            assert!(
                op.get(field).is_none(),
                "{} must be omitted when there is no manifest entry, got {:?}",
                field,
                op
            );
        }
        // Empty match/dep arrays still serialize as present empty arrays.
        assert_eq!(json["cross_repo_matches"].as_array().unwrap().len(), 0);
        assert_eq!(json["dependency_conflicts"].as_array().unwrap().len(), 0);
    }

    /// #245 Phase 1: a manifest entry keyed by a *socket* OperationKey joins to
    /// its op in the projection and surfaces `primary_type_symbol`/`type_state`
    /// on the EvalOp — proving the projection join is protocol-agnostic, not
    /// HTTP-only. (Plan test #4.)
    #[test]
    fn socket_keyed_manifest_entry_surfaces_on_eval_op() {
        use crate::operation::SocketDirection;

        let socket_key = OperationKey::socket("payment:settled", SocketDirection::ServerToClient);
        // A socket emitter lands on `calls` (consumer side).
        let call = ApiEndpointDetails {
            view_module: false,
            owner: None,
            key: socket_key.clone(),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: None,
            response_type: None,
            file_path: PathBuf::from("src/socket.ts:12"),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            resolution_source: None,
        };

        let result = ApiAnalysisResult {
            endpoints: vec![],
            calls: vec![call],
            findings: vec![],
            dependency_conflicts: vec![],
            verified_endpoints: vec![],
            sdk_edges: Vec::new(),
            sdk_unresolved: Vec::new(),
            detected_graphql_libraries: vec![],
            graphql_operations_indexed: false,
            cross_repo_matches: vec![],
        };

        // A socket emitter is a consumer, so its manifest entry carries the
        // Consumer role and the consumer EvalOp (on `calls`) must join the
        // Consumer slot (#207).
        let manifest = vec![manifest_entry(
            socket_key,
            ManifestRole::Consumer,
            ManifestTypeState::Explicit,
            true,
            Some("{ id: string; amountCents: number }"),
            Some("Payment"),
        )];

        let projection = EvalProjection::from_results(
            &result,
            &manifest,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        let json: Value =
            serde_json::from_str(&serde_json::to_string(&projection).unwrap()).unwrap();
        let op = &json["calls"].as_array().unwrap()[0];

        assert_eq!(op["key"], "socket|SERVER->CLIENT|payment:settled");
        assert_eq!(op["protocol"], "socket");
        // method is null for non-HTTP (the field is always present); path
        // carries the event name.
        assert_eq!(op["method"], Value::Null);
        assert_eq!(op["path"], "payment:settled");
        assert_eq!(op["type_state"], "Explicit");
        assert_eq!(op["primary_type_symbol"], "Payment");
        assert_eq!(
            op["resolved_definition"],
            "{ id: string; amountCents: number }"
        );
    }

    /// #207 de-collapse: when a producer endpoint and a consumer call share ONE
    /// canonical key (e.g. `http|GET|/orders/:param` produced by an orders pkg
    /// AND consumed by a web frontend), each must surface ITS OWN manifest
    /// fields — the producer's expected `Order` anchor on the endpoint, the
    /// consumer's `OrderView` anchor on the call — not a single collapsed value.
    /// Keying the manifest index by `(canonical, role)` is what keeps them apart;
    /// before the fix the second-written entry clobbered the first's slot.
    #[test]
    fn shared_key_producer_and_consumer_keep_own_manifest_fields() {
        let shared_key = OperationKey::http("GET", "/orders/:param");

        let result = ApiAnalysisResult {
            // The endpoint (producer side) and the call (consumer side) carry the
            // SAME canonical key, the collapse trap.
            endpoints: vec![endpoint("GET", "/orders/:param", "src/orders.ts:12")],
            calls: vec![ApiEndpointDetails {
                view_module: false,
                owner: None,
                key: shared_key.clone(),
                params: vec![],
                request_body: None,
                response_body: None,
                handler_name: Some("fetchOrder".to_string()),
                request_type: None,
                response_type: None,
                file_path: PathBuf::from("web/src/orders.ts:7"),
                repo_name: None,
                service_name: None,
                provenance: Default::default(),
                resolution_source: None,
            }],
            findings: vec![],
            dependency_conflicts: vec![],
            verified_endpoints: vec![],
            sdk_edges: Vec::new(),
            sdk_unresolved: Vec::new(),
            detected_graphql_libraries: vec![],
            graphql_operations_indexed: false,
            cross_repo_matches: vec![],
        };

        // Two manifest entries on the SAME canonical key, distinguished only by
        // role, each with a distinct anchor + resolved definition + type_state.
        let manifest = vec![
            manifest_entry(
                shared_key.clone(),
                ManifestRole::Producer,
                ManifestTypeState::Explicit,
                true,
                Some("{ id: number; amountCents: number }"),
                Some("Order"),
            ),
            manifest_entry(
                shared_key,
                ManifestRole::Consumer,
                ManifestTypeState::Implicit,
                false,
                Some("{ id: number }"),
                Some("OrderView"),
            ),
        ];

        let projection = EvalProjection::from_results(
            &result,
            &manifest,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        let json: Value =
            serde_json::from_str(&serde_json::to_string(&projection).unwrap()).unwrap();

        let endpoint_op = &json["endpoints"].as_array().unwrap()[0];
        let call_op = &json["calls"].as_array().unwrap()[0];

        // Same canonical key on both sides — the collapse trap.
        assert_eq!(endpoint_op["key"], "http|GET|/orders/:param");
        assert_eq!(call_op["key"], "http|GET|/orders/:param");

        // The producer endpoint surfaces the PRODUCER slot's fields.
        assert_eq!(endpoint_op["primary_type_symbol"], "Order");
        assert_eq!(
            endpoint_op["resolved_definition"],
            "{ id: number; amountCents: number }"
        );
        assert_eq!(endpoint_op["type_state"], "Explicit");
        assert_eq!(endpoint_op["is_explicit"], true);

        // The consumer call surfaces the CONSUMER slot's fields — proving the two
        // roles did NOT collapse into one slot and clobber each other.
        assert_eq!(call_op["primary_type_symbol"], "OrderView");
        assert_eq!(call_op["resolved_definition"], "{ id: number }");
        assert_eq!(call_op["type_state"], "Implicit");
        assert_eq!(call_op["is_explicit"], false);
    }

    /// The per-op eval labels are written against the op's RESPONSE contract,
    /// so when an op carries both a request and a response manifest entry, the
    /// response entry's fields must surface — even when the request entry
    /// resolved first. Before the kind-aware join, first-definition-wins let the
    /// request shape displace the response (`POST /payments` / `POST /track`
    /// surfaced `{ orderId; amountCents }` where the eval expected `Payment`).
    #[test]
    fn response_entry_wins_over_request_entry_for_op_fields() {
        let key = OperationKey::http("POST", "/payments");
        let result = ApiAnalysisResult {
            endpoints: vec![endpoint("POST", "/payments", "src/payments.ts:30")],
            calls: vec![],
            findings: vec![],
            dependency_conflicts: vec![],
            verified_endpoints: vec![],
            sdk_edges: Vec::new(),
            sdk_unresolved: Vec::new(),
            detected_graphql_libraries: vec![],
            graphql_operations_indexed: false,
            cross_repo_matches: vec![],
        };
        // Request entry FIRST (the manifest order that used to win).
        let manifest = vec![
            manifest_entry_at(
                key.clone(),
                ManifestRole::Producer,
                ManifestTypeKind::Request,
                "Endpoint_req_Request",
                "src/payments.ts",
                30,
                ManifestTypeState::Explicit,
                true,
                Some("{ orderId: number; amountCents: number }"),
                None,
            ),
            manifest_entry_at(
                key,
                ManifestRole::Producer,
                ManifestTypeKind::Response,
                "Endpoint_res_Response",
                "src/payments.ts",
                35,
                ManifestTypeState::Explicit,
                true,
                Some("{ id: string; orderId: number; status: string }"),
                Some("Payment"),
            ),
        ];

        let projection = EvalProjection::from_results(
            &result,
            &manifest,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        let json: Value =
            serde_json::from_str(&serde_json::to_string(&projection).unwrap()).unwrap();
        let op = &json["endpoints"].as_array().unwrap()[0];

        assert_eq!(op["type_alias"], "Endpoint_res_Response");
        assert_eq!(
            op["resolved_definition"],
            "{ id: string; orderId: number; status: string }"
        );
        assert_eq!(op["primary_type_symbol"], "Payment");
    }

    /// An op whose only resolved entry is request-kind (`POST /billing/charge`,
    /// a fire-and-forget consumer with no read response) must keep surfacing
    /// that request definition — the response preference is a preference, not a
    /// filter.
    #[test]
    fn request_only_op_still_surfaces_request_definition() {
        let key = OperationKey::http("POST", "/billing/charge");
        let call = ApiEndpointDetails {
            view_module: false,
            owner: None,
            key: key.clone(),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: None,
            response_type: None,
            file_path: PathBuf::from("src/billing.client.ts:19"),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            resolution_source: None,
        };
        let result = ApiAnalysisResult {
            endpoints: vec![],
            calls: vec![call],
            findings: vec![],
            dependency_conflicts: vec![],
            verified_endpoints: vec![],
            sdk_edges: Vec::new(),
            sdk_unresolved: Vec::new(),
            detected_graphql_libraries: vec![],
            graphql_operations_indexed: false,
            cross_repo_matches: vec![],
        };
        let manifest = vec![manifest_entry_at(
            key,
            ManifestRole::Consumer,
            ManifestTypeKind::Request,
            "Endpoint_req_Request_Callabc",
            "src/billing.client.ts",
            19,
            ManifestTypeState::Implicit,
            false,
            Some("{ paymentId: string; amountCents: number }"),
            None,
        )];

        let projection = EvalProjection::from_results(
            &result,
            &manifest,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        let json: Value =
            serde_json::from_str(&serde_json::to_string(&projection).unwrap()).unwrap();
        let op = &json["calls"].as_array().unwrap()[0];

        assert_eq!(
            op["resolved_definition"],
            "{ paymentId: string; amountCents: number }"
        );
        assert_eq!(op["type_state"], "Implicit");
    }

    /// Fan-in (#290/#291): two consumer call sites on ONE canonical key — two
    /// repos publishing the same pub/sub topic with deliberately different
    /// payloads — carry one manifest entry per site. Each call op must join the
    /// records at ITS OWN file, not whichever site's entry happened to be
    /// indexed first (which masked orders-engine's nested payload behind
    /// billing's flat one and mis-scored its resolution). The manifest path is
    /// repo-relative while the op path is absolute, mirroring the live mixed
    /// conventions, so this also pins the suffix-aligned site match.
    #[test]
    fn fan_in_consumer_calls_join_their_own_site_records() {
        let key = OperationKey::pubsub("order.placed");
        let call_at = |file_line: &str| ApiEndpointDetails {
            view_module: false,
            owner: None,
            key: key.clone(),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: None,
            response_type: None,
            file_path: PathBuf::from(file_line),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            resolution_source: None,
        };
        let result = ApiAnalysisResult {
            endpoints: vec![],
            calls: vec![
                call_at("/scan/billing-svc/src/kafka/producer.ts:15"),
                call_at("/scan/orders-engine/src/kafka/producer.ts:22"),
            ],
            findings: vec![],
            dependency_conflicts: vec![],
            verified_endpoints: vec![],
            sdk_edges: Vec::new(),
            sdk_unresolved: Vec::new(),
            detected_graphql_libraries: vec![],
            graphql_operations_indexed: false,
            cross_repo_matches: vec![],
        };
        let manifest = vec![
            manifest_entry_at(
                key.clone(),
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                "Endpoint_x_Response_Callbilling",
                "billing-svc/src/kafka/producer.ts",
                15,
                ManifestTypeState::Explicit,
                true,
                Some("{ id: string; total: number }"),
                Some("OrderPlaced"),
            ),
            manifest_entry_at(
                key,
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                "Endpoint_x_Response_Callorders",
                "orders-engine/src/kafka/producer.ts",
                22,
                ManifestTypeState::Explicit,
                true,
                Some("{ id: string; total: { amountCents: number } }"),
                Some("OrderPlaced"),
            ),
        ];

        let projection = EvalProjection::from_results(
            &result,
            &manifest,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        let json: Value =
            serde_json::from_str(&serde_json::to_string(&projection).unwrap()).unwrap();
        let calls = json["calls"].as_array().unwrap();

        assert_eq!(calls[0]["type_alias"], "Endpoint_x_Response_Callbilling");
        assert_eq!(
            calls[0]["resolved_definition"],
            "{ id: string; total: number }"
        );
        assert_eq!(calls[1]["type_alias"], "Endpoint_x_Response_Callorders");
        assert_eq!(
            calls[1]["resolved_definition"],
            "{ id: string; total: { amountCents: number } }"
        );
    }
}
