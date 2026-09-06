//! The cross-repo join, as the local indexer consumes it.
//!
//! The join itself is the engine's, unchanged: `carrick index` runs the same
//! cross-repo phase the offline eval harness does (every cached blob
//! downloaded, one analyzer built over all of them, the v2 type check run
//! against the sidecar), and this module is only how that run hands its
//! result back — the merged operations with their locations, and the edges
//! between them with their verdicts.
//!
//! It is deliberately NOT [`crate::eval_output::EvalProjection`]. That shape
//! is the eval scorer's contract, read by the other repo, and it carries no
//! service attribution, no consumer location and no type verdict — the three
//! things a local reader answers with. Extending a cross-repo contract to
//! serve a local file would be the wrong direction for both.
//!
//! Written by the join subprocess and read once by the indexer, so it is a
//! private hand-off with no compatibility surface. The contract a reader sees
//! is `docs/local-mode-output.md`.

use serde::{Deserialize, Serialize};

use crate::agents::file_analyzer_agent::ResolutionSource;
use crate::analyzer::{ApiAnalysisResult, ApiEndpointDetails};
use crate::operation::TypeVerdict;

/// Which side of a contract an operation sits on.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A route, resolver, subscriber or handler this service serves.
    Producer,
    /// A call this service makes.
    Consumer,
}

/// One merged operation, with the service that owns it and where it is
/// written.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JoinedOperation {
    pub role: Role,
    /// `service_name ?? repo_name` — the one identity both sides of an edge
    /// use.
    pub service: String,
    /// `OperationKey::canonical()`, the identity an edge names.
    pub key: String,
    /// HTTP method, GraphQL kind, or socket direction.
    pub method: String,
    /// Route path, GraphQL field, socket event, or pub/sub topic.
    pub path: String,
    /// Repo-relative file, and the line when the row recorded one.
    pub file: String,
    pub line: Option<u32>,
    pub resolution_source: Option<ResolutionSource>,
    pub handler: Option<String>,
}

/// One producer/consumer edge, with whatever the type check concluded about
/// it.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JoinedMatch {
    pub producer_service: String,
    pub producer_key: String,
    pub consumer_service: String,
    pub consumer_key: String,
    /// Where the consumer's call is written, when the edge carries it.
    pub consumer_file: Option<String>,
    pub consumer_line: Option<u32>,
    /// `producer_consumer`, or `shared_external_contract` for a pair where
    /// neither side serves the other.
    pub relationship: String,
    /// `None` = the check never evaluated this pair, which is not the same as
    /// "compatible".
    pub type_verdict: Option<TypeVerdict>,
    pub mismatch_reason: Option<String>,
}

/// A contract problem the join found, projected down to the two kinds a local
/// verdict can state. Everything else the report renders is a finding about a
/// service, not about a row a reader is editing.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JoinedFinding {
    /// `type_mismatch` or `method_mismatch`.
    pub kind: String,
    pub service: Option<String>,
    pub method: String,
    pub path: String,
    /// `"file:line"` of each consumer call site the finding covers.
    pub call_sites: Vec<String>,
    pub detail: String,
    /// The finding's own `verdict_state` (carrick#727), carried rather than
    /// re-derived: it is the same three words the local contract states, and
    /// one producer of a word is the whole point of carrick#731. `None` on a
    /// finding that does not state one.
    pub verdict_state: Option<String>,
}

/// Everything the join learned, in the shape the indexer reads it.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LocalJoin {
    pub scanner_version: String,
    pub operations: Vec<JoinedOperation>,
    pub matches: Vec<JoinedMatch>,
    pub findings: Vec<JoinedFinding>,
}

impl LocalJoin {
    /// Project a finished cross-repo analysis.
    pub fn from_results(results: &ApiAnalysisResult) -> Self {
        let mut operations: Vec<JoinedOperation> = results
            .endpoints
            .iter()
            .map(|op| project(op, Role::Producer))
            .chain(results.calls.iter().map(|op| project(op, Role::Consumer)))
            .collect();
        // The analyzer's collections are built from maps, so their order is
        // not the source's. Sorting here is what makes two runs over an
        // unchanged tree write byte-identical files.
        operations.sort_by(|a, b| {
            (&a.service, &a.file, a.line, &a.key).cmp(&(&b.service, &b.file, b.line, &b.key))
        });

        let mut matches: Vec<JoinedMatch> = results
            .cross_repo_matches
            .iter()
            .map(|edge| {
                let (consumer_file, consumer_line) = edge
                    .consumer_location
                    .as_deref()
                    .map(super::split_location)
                    .map_or((None, None), |(file, line)| (Some(file), line));
                JoinedMatch {
                    producer_service: edge.producer_repo.clone(),
                    producer_key: edge.producer_key.clone(),
                    consumer_service: edge.consumer_repo.clone(),
                    consumer_key: edge.consumer_key.clone(),
                    consumer_file,
                    consumer_line,
                    relationship: serde_json::to_value(edge.relationship)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_string))
                        .unwrap_or_else(|| "producer_consumer".to_string()),
                    type_verdict: edge.type_verdict,
                    mismatch_reason: edge.mismatch_reason.clone(),
                }
            })
            .collect();
        matches.sort_by(|a, b| {
            (
                &a.producer_service,
                &a.producer_key,
                &a.consumer_service,
                &a.consumer_key,
                &a.consumer_file,
                a.consumer_line,
            )
                .cmp(&(
                    &b.producer_service,
                    &b.producer_key,
                    &b.consumer_service,
                    &b.consumer_key,
                    &b.consumer_file,
                    b.consumer_line,
                ))
        });

        let mut findings: Vec<JoinedFinding> = results
            .findings
            .iter()
            .filter_map(project_finding)
            .collect();
        findings.sort_by(|a, b| {
            (&a.kind, &a.service, &a.method, &a.path, &a.call_sites).cmp(&(
                &b.kind,
                &b.service,
                &b.method,
                &b.path,
                &b.call_sites,
            ))
        });

        Self {
            scanner_version: env!("CARGO_PKG_VERSION").to_string(),
            operations,
            matches,
            findings,
        }
    }

    /// Write the join where the indexer asked for it, creating the parent
    /// directory. Called from the engine at the end of a cross-repo run.
    pub fn write(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string(self).map_err(|e| {
            std::io::Error::other(format!("failed to serialize the local join: {e}"))
        })?;
        std::fs::write(path, json)
    }
}

/// The two findings a row-level verdict can state. A missing endpoint, an
/// orphan, a dependency conflict and the rest are about a service or a
/// project, and a reader editing one file is not the place to raise them.
fn project_finding(finding: &crate::findings::Finding) -> Option<JoinedFinding> {
    match finding {
        crate::findings::Finding::TypeMismatch {
            method,
            path,
            service,
            call_sites,
            detail,
            verdict_state,
            ..
        } => Some(JoinedFinding {
            kind: "type_mismatch".to_string(),
            service: service.clone(),
            method: method.clone(),
            path: path.clone(),
            call_sites: call_sites.clone(),
            detail: detail.clone(),
            verdict_state: wire_verdict_state(*verdict_state),
        }),
        crate::findings::Finding::MethodMismatch {
            method,
            path,
            service,
            call_sites,
            expected_method,
            verdict_state,
            ..
        } => Some(JoinedFinding {
            kind: "method_mismatch".to_string(),
            service: service.clone(),
            method: method.clone(),
            path: path.clone(),
            verdict_state: wire_verdict_state(*verdict_state),
            call_sites: call_sites.clone(),
            detail: format!(
                "this call uses {method} and the producer serves {expected_method} at {path}"
            ),
        }),
        _ => None,
    }
}

/// The finding's verdict state in the spelling both contracts print.
fn wire_verdict_state(state: Option<crate::findings::VerdictState>) -> Option<String> {
    let state = state?;
    serde_json::to_value(state)
        .ok()?
        .as_str()
        .map(str::to_string)
}

/// One merged operation, projected. `file_path` carries the location in
/// `file:line[:col]` form, which is why it is split rather than read.
fn project(op: &ApiEndpointDetails, role: Role) -> JoinedOperation {
    let (method, path) = op.key.display_labels();
    let (file, line) = super::split_location(&op.file_path.to_string_lossy());
    JoinedOperation {
        role,
        service: op
            .service_name
            .clone()
            .or_else(|| op.repo_name.clone())
            .unwrap_or_default(),
        key: op.key.canonical(),
        method,
        path,
        file,
        line,
        resolution_source: op.resolution_source,
        handler: op.handler_name.clone(),
    }
}
