use crate::{
    agents::{file_analyzer_agent::FileAnalysisResult, framework_guidance_agent::ProtocolGuidance},
    analyzer::ApiEndpointDetails,
    app_context::AppContext,
    external_call_candidates::ExternalCallCandidate,
    framework_detector::DetectionResult,
    mount_graph::MountGraph,
    multi_agent_orchestrator::MultiAgentAnalysisResult,
    operation::OperationKey,
    packages::Packages,
    services::type_sidecar::InferKind,
    visitor::{FunctionDefinition, Mount, OwnerType},
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;
use tracing::debug;

mod mock_storage;
pub use mock_storage::MockStorage;
mod aws_storage;
pub use aws_storage::AwsStorage;
pub(crate) use aws_storage::INLINE_PAYLOAD_LIMIT_BYTES;
mod local_dir_storage;
mod tee_storage;
pub use local_dir_storage::{CACHE_DIR_ENV, ISOLATE_ENV, LocalDirStorage};
pub use tee_storage::{LAPTOP_SCAN_ENV, TeeStorage, laptop_scan_requested};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ManifestRole {
    Producer,
    Consumer,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ManifestTypeKind {
    Request,
    Response,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManifestTypeState {
    Explicit,
    Implicit,
    Unknown,
}

/// Evidence metadata for how a manifest entry was derived.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TypeEvidence {
    /// Source file path where the type was found
    pub file_path: String,
    /// Start byte offset in the source file
    pub span_start: Option<u32>,
    /// End byte offset in the source file
    pub span_end: Option<u32>,
    /// Line number in the source file
    pub line_number: u32,
    /// Kind of inference performed for this type
    pub infer_kind: InferKind,
    /// Whether the type was explicitly annotated
    pub is_explicit: bool,
    /// Current state of the type extraction
    pub type_state: ManifestTypeState,
}

/// Entry in the type manifest mapping endpoints to their type information
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TypeManifestEntry {
    /// Operation identity. Flattened so the JSON keeps the flat
    /// `protocol`/`method`/`path` fields the manifest matcher reads.
    #[serde(flatten)]
    pub key: OperationKey,
    /// Whether this is a producer or consumer
    pub role: ManifestRole,
    /// Whether this entry represents request or response
    pub type_kind: ManifestTypeKind,
    /// The type alias used in the bundled .d.ts file
    pub type_alias: String,
    /// Source file path where the type was found
    pub file_path: String,
    /// Line number in the source file
    pub line_number: u32,
    /// Whether the type was explicitly annotated
    pub is_explicit: bool,
    /// Current state of the type extraction
    pub type_state: ManifestTypeState,
    /// Evidence metadata for this entry
    pub evidence: TypeEvidence,
    /// Original declaration text as written (preserves named types for readability).
    /// Generated at CI time by the sidecar's DefinitionResolver.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_definition: Option<String>,
    /// Compiler-expanded form with all types fully inlined.
    /// Generated at CI time via ts-morph's type.getText() with NoTruncation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expanded_definition: Option<String>,
    /// The LLM-emitted type-anchor symbol for this op (e.g. `StatusResponse`),
    /// joined from the file-analyzer result by `(file_path, line_number)`. Unlike
    /// `type_alias` (the synthetic `Endpoint_<hash>_Response` name), this is the
    /// real source symbol the eval anchor metric scores against. `None` when the
    /// model emitted no anchor for this op.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_type_symbol: Option<String>,
    /// Where the entry's anchor symbol is DECLARED (carrick#649).
    ///
    /// `file_path`/`line_number` above are the site the operation was extracted
    /// at — a route file's `satisfies GetLookupResponseBody`, or the import line
    /// that brought the symbol in. That is where the type is USED, and a reader
    /// answering "where is this type defined" from it answers wrongly. This
    /// states the declaration itself, resolved from the import the file wrote
    /// and confirmed against the declaring file's own AST.
    ///
    /// `None` whenever the scanner cannot see a declaration: an anchor with no
    /// symbol, an import that resolves to no file on disk (a package, a
    /// tsconfig path the scanner does not follow), a barrel re-export, or a
    /// symbol the resolved file does not itself declare. Never a guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defined_in: Option<TypeHome>,
    /// Why this entry's type carries `any`/`unknown`, and where (carrick#376).
    ///
    /// A bare `any` in a published endpoint type answers nothing: a reader
    /// cannot tell "this field really is untyped" from "the dependency that
    /// declares it was not installed when the scan ran". Each entry names the
    /// member path and the cause the layer that produced it actually knew —
    /// the inferrer's reason for declining to read a payload, and the capture
    /// self-check's structural findings, merged and sorted by path.
    ///
    /// Empty when the type carries no top type, and empty is not a claim that
    /// it does not: a layer with no cause records `not_recorded` rather than
    /// guessing, and an entry with no type at all has nothing to walk.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub any_provenance: Vec<crate::services::type_sidecar::TypeProvenance>,
    /// The v1 side was ASKED for this alias and had no shape to give
    /// (carrick#780) — the bundle carries only Carrick's own marked
    /// `= unknown` placeholder for it.
    ///
    /// Recorded rather than re-derived, because it is the one fact that says
    /// which side answered: `type_state` used to become `Implicit` here purely
    /// because the placeholder statement read like a declaration, and an entry
    /// then published a capture answer as though v1 had produced it. The
    /// capture is still consulted for these — it is the layer that resolves
    /// what v1 could not, and a handful of real shapes come from exactly this
    /// path — but the state it earns is now the capture's own.
    ///
    /// Scan-local: every scan rebuilds the manifest and re-enriches it before
    /// anything reads this, so it is deliberately not serialized into the index
    /// blob. Publishing it (so a reader could be told which layer answered) is
    /// a blob-contract question, not a scanner-internal one.
    #[serde(skip)]
    pub v1_unresolved: bool,
}

/// The declaration site of a manifest entry's anchor symbol (carrick#649).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TypeHome {
    /// Repo-root-relative path of the file that DECLARES the symbol, in the
    /// same form the rest of the manifest's paths take.
    pub file_path: String,
    /// 1-based line of the declaration.
    pub line_number: u32,
    /// The declared symbol, named as the declaring file names it.
    pub symbol: String,
}

/// One DIRECTION of a pairing's type check: the request types compared against
/// each other, or the response types (carrick#822).
///
/// A pairing is two independent comparisons that happen to share a producer
/// and a consumer. Folding them into one verdict lost the only thing a reader
/// needs to act: which half the answer is about. On a real pair the request
/// halves proved a mismatch (`passwordHash` missing) while the response halves
/// compared nothing (the consumer's response type is `any`), and the folded row
/// stated both side by side with no way to tell them apart — a proven mismatch
/// read as "the check compared nothing".
///
/// `verdict` and `resolved` answer different questions and both are needed.
/// `verdict` is what the probe returned. `resolved` is whether it returned it
/// over two KNOWN types: the probe gates are whole-type only, so a side
/// carrying `any` three members down clears every gate and then reads
/// compatible against any counterparty shape. A `compatible` verdict with
/// `resolved: false` establishes nothing.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DirectionVerdict {
    /// What the check returned for THIS direction: `compatible`,
    /// `incompatible`, or `unverifiable` (reached and could not verify).
    pub verdict: crate::operation::TypeVerdict,
    /// The mismatch diagnostic, present iff `verdict == incompatible`. Named
    /// `reason` and not `mismatch_reason` because on this struct there is only
    /// one reason a verdict can carry, and it is this direction's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Whether `verdict` is a comparison between two KNOWN types on this
    /// direction, or the absence of one (carrick#730/#734). True only when the
    /// check's deep walk over both sides of THIS direction found no
    /// `any`/`unknown` at any depth.
    pub resolved: bool,
    /// Which side, and where in it, left this direction unresolved. Present
    /// only alongside `resolved: false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unresolved_reason: Option<String>,
}

impl Ord for DirectionVerdict {
    /// Field order only, so the structs that embed this stay sortable. NOT the
    /// verdict precedence — that is [`crate::operation::TypeVerdict::combine`]
    /// (incompatible > unverifiable > compatible), and nothing may derive a
    /// worst-wins fold from this ordering.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        fn rank(v: crate::operation::TypeVerdict) -> u8 {
            match v {
                crate::operation::TypeVerdict::Compatible => 0,
                crate::operation::TypeVerdict::Incompatible => 1,
                crate::operation::TypeVerdict::Unverifiable => 2,
            }
        }
        (
            rank(self.verdict),
            &self.reason,
            self.resolved,
            &self.unresolved_reason,
        )
            .cmp(&(
                rank(other.verdict),
                &other.reason,
                other.resolved,
                &other.unresolved_reason,
            ))
    }
}

impl PartialOrd for DirectionVerdict {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A persisted per-pair type-compatibility verdict, keyed by CANONICAL pair
/// identity (never display labels — that is the #324 fail-open trap). Emitted at
/// scan time from the cross-repo [`crate::analyzer::CrossRepoMatch`] edges this
/// repo's calls participate in as the consumer, so the cloud MCP
/// `check_compatibility` tool can surface the REAL type-compat verdict CI
/// already computes instead of a structural-matching-only answer.
///
/// Only edges the check actually REACHED are persisted, so no verdict here is
/// ever a fabricated `compatible`: a pair with no stored row is "not compared",
/// which is a different state from a pair the check reached and could not
/// verify (carrick#811). The four key fields are the exact
/// `OperationKey::canonical()` / `service_name ?? repo_name` strings the cloud
/// reconstructs from the same persisted blob it already reads, so the join is
/// byte-identical and drift-free.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CompatVerdict {
    /// Producer repo id (`service_name ?? repo_name`).
    pub producer_repo: String,
    /// Producer endpoint `OperationKey::canonical()` (e.g. `http|GET|/orders/:id`).
    pub producer_key: String,
    /// Consumer repo id (`service_name ?? repo_name`).
    pub consumer_repo: String,
    /// Consumer call `OperationKey::canonical()` (host-free, URL-normalized;
    /// equal to the persisted `DataFetchingCall::canonical_path` for every edge
    /// that yields a match).
    pub consumer_key: String,
    /// The check's answer for the REQUEST direction — the consumer's request
    /// body against the producer's declared request type.
    ///
    /// Absent when the check filed no outcome for this direction (no request
    /// type on one side, so there was nothing to compare), which is not a
    /// verdict of any kind. A row with neither direction present is never
    /// persisted: an absent row means nothing was compared, and that is the
    /// only thing absence means.
    ///
    /// Replaces the folded `verdict`/`compatible`/`mismatch_reason`/`resolved`
    /// pair-level fields (carrick#822). A blob written by a scanner that
    /// predates the split carries those instead and neither direction here; the
    /// old keys are gone from this struct, so such a row deserializes with both
    /// directions `None` and states nothing, never "compatible".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<DirectionVerdict>,
    /// The check's answer for the RESPONSE direction — the producer's declared
    /// response type against what the consumer's call site expects back. Absent
    /// on the same terms as [`CompatVerdict::request`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<DirectionVerdict>,
    /// Scanner release that produced this verdict (`CARGO_PKG_VERSION`), so a
    /// reader can see how stale the verdict is relative to the current scanner.
    pub scanner_version: String,
}

/// A consumer call that reaches a producer endpoint through a published npm
/// client rather than over a URL the consumer writes (carrick#466, cloud#370).
///
/// The consumer's own source contains no route at all — it writes
/// `ledger.payments.create(...)` — so the direct HTTP matcher can never see
/// the pair. This edge is that relationship recorded explicitly: the candidate
/// call site, the SDK member it lands on, and the producer endpoint the SDK's
/// own outbound call at that member already matched. It is deliberately its
/// own relationship and never folded into `endpoints`/`calls`: projecting SDK
/// traffic into HTTP matching is the known false-positive class the
/// [`crate::external_call_candidates`] design exists to avoid.
///
/// Stored on the CONSUMER's blob, keyed by the same canonical identities as
/// [`CompatVerdict`] (`OperationKey::canonical()` and `service_name ??
/// repo_name`), so the cloud joins it to the producer endpoint by exact match.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SdkEdge {
    /// Consumer service id (`service_name ?? repo_name`).
    pub consumer_repo: String,
    /// `"file:line"` of the consumer's call, repo-relative.
    pub consumer_location: String,
    /// The npm package the client was imported from, exactly as the
    /// `package.json` names it.
    pub package: String,
    /// Which export of the package the receiver came from: `default`, or the
    /// named export. `None` for a namespace import, which never produces an
    /// edge (it resolves to no single member).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_symbol: Option<String>,
    /// The callee as written at the consumer's call site: `ledger.payments.create`.
    pub callee: String,
    /// Service id of the repo publishing `package`.
    pub sdk_repo: String,
    /// The member path inside the SDK, without the root binding:
    /// `payments.create`.
    pub sdk_member: String,
    /// `"file:line"` of that member in the SDK repo, repo-relative to it.
    pub sdk_location: String,
    /// Producer service id (`service_name ?? repo_name`).
    pub producer_repo: String,
    /// Producer endpoint `OperationKey::canonical()` (e.g.
    /// `http|POST|/v1/payments`), byte-identical to the producer endpoint's own
    /// key so the cloud de-orphans by exact match.
    pub producer_key: String,
    /// The REQUEST direction of the SDK→producer pair, carried verbatim from
    /// the `CrossRepoMatch` the SDK's own call formed — or, when the SDK repo
    /// is not a current service this run, from the [`CompatVerdict`] the SDK
    /// repo's own scan persisted for the same canonical pair.
    ///
    /// `None` = nothing was compared for this direction, never "compatible"
    /// (#324). That includes an edge whose verdict came off a peer blob written
    /// before the per-direction split: those rows state no direction at all,
    /// and nothing is what this edge then says until the peer rescans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<DirectionVerdict>,
    /// The RESPONSE direction of the same pair, on the same terms as
    /// [`SdkEdge::request`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<DirectionVerdict>,
    /// Scanner release that produced this edge (`CARGO_PKG_VERSION`).
    pub scanner_version: String,
}

/// SDK-mediated calls the join could not turn into an edge, aggregated per
/// package and reason.
///
/// Recorded rather than dropped so the surface can say "this package is
/// called, and here is why nothing is known about where those calls land"
/// instead of silently showing nothing. `reason` is one of
/// `no_sdk_repo_in_project`, `member_not_found`, `receiver_unresolved`,
/// `no_matching_producer`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SdkUnresolved {
    pub package: String,
    pub count: usize,
    pub reason: String,
}

/// One file the model was asked about and did not answer for.
///
/// Sent with the write actions so the cloud can apply its first-scan partial
/// rule: a service with no hosted rows accepts the partial index and echoes
/// this list back, and a service that already has rows refuses it. `reason` is
/// [`crate::agent_service::AgentCallError::code`] verbatim.
///
/// A file a budget refused is never in this list (carrick#555): nothing failed
/// there, so there is nothing for the cloud to decide about.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct UnanalysedFile {
    pub path: String,
    pub reason: String,
}

/// What the engine knows about this run before it opens it.
pub struct RunContext {
    /// `owner/repo` from the git remote, when this clone has one. The laptop
    /// gate keys on it; the CI path derives repo identity from the signed
    /// OIDC claims instead and never reads this.
    pub repo_full_name: Option<String>,
    /// HEAD's commit.
    pub commit: String,
    /// The working tree carries changes HEAD does not describe.
    pub dirty: bool,
}

/// What opening the run told the scanner.
#[derive(Debug, Default)]
pub struct RunStart {
    /// The cloud's "candidates not refreshed since …" sentence, or `None`.
    /// Printed before the scan starts, so a capped run says so at the top
    /// rather than surprising the user at the end.
    pub allowance_sentence: Option<String>,
    /// Services of this repo that already have hosted rows, or `None` when
    /// this is not a laptop run and the question was never asked. An empty
    /// list means every service is a first index, which is what lets the
    /// scanner skip its own lost-file abort and let the cloud decide (§4).
    pub indexed_services: Option<Vec<String>>,
}

impl RunStart {
    /// Whether this run is a laptop scan.
    ///
    /// Read off `indexed_services` rather than a second flag, because only
    /// `start-scan` answers that question and only the laptop path asks it —
    /// two fields could disagree, and this one cannot.
    pub fn is_laptop(&self) -> bool {
        self.indexed_services.is_some()
    }
}

/// Why one service's type extraction failed. Structured rather than a
/// prose string so the finding, the terminal report, and the index all read
/// the same two fields.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TypeDegradation {
    /// Which stage lost the types: `spawn`, `init`, `resolve`, `capture`.
    pub stage: String,
    /// The underlying error, verbatim.
    pub detail: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CloudRepoData {
    pub repo_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>, // Service name from carrick.json for cross-repo resolution
    pub endpoints: Vec<ApiEndpointDetails>,
    pub calls: Vec<ApiEndpointDetails>,
    pub mounts: Vec<Mount>,
    pub apps: HashMap<String, AppContext>,
    pub imported_handlers: Vec<(String, String, String, String)>,
    pub function_definitions: HashMap<String, FunctionDefinition>,
    pub config_json: Option<String>,
    pub package_json: Option<String>,
    pub packages: Option<Packages>, // Structured package data for dependency analysis
    pub last_updated: DateTime<Utc>,
    pub commit_hash: String,
    /// The working tree carried changes `commit_hash` does not describe.
    ///
    /// In the blob rather than on the envelope because it QUALIFIES
    /// `commit_hash`, which is already here: the blob's sentence is "this is
    /// the index at 4f2a1c9", and for a dirty tree that sentence is false
    /// without this field — every later reader of `previous_data` would
    /// inherit the false version. Absent on a CI scan and on every blob
    /// written before the field existed, both of which mean "clean".
    ///
    /// Wire contract: carrick-cloud
    /// `docs/internal/reference/laptop-scan-seam.md` §2.2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mount_graph: Option<MountGraph>, // Mount graph for framework-agnostic analysis
    /// Bundled TypeScript type definitions (.d.ts content)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundled_types: Option<String>,
    /// Type manifest mapping endpoints/calls to their type aliases
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_manifest: Option<Vec<TypeManifestEntry>>,
    /// Cached per-file LLM analysis results for incremental re-analysis
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_results: Option<HashMap<String, FileAnalysisResult>>,
    /// Cached framework detection result
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_detection: Option<DetectionResult>,
    /// Cached per-protocol framework guidance
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_guidance: Option<ProtocolGuidance>,
    /// Cached machinery-unwrap rules from the extraction_config task.
    /// Reusable under the same `package_json_hash` gate as detection/guidance
    /// (its inputs are the detected stack + dependency names).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_extraction_config: Option<crate::services::type_sidecar::ExtractionConfig>,
    /// Hash of package.json content — if it matches, cached detection/guidance are reusable
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_json_hash: Option<String>,
    /// Cache format version — discard cached data if mismatched
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_version: Option<u32>,
    /// Set when this service's type extraction FAILED outright: the sidecar
    /// was missing, never became ready, died mid-run, or the resolution /
    /// capture call errored. `None` means types were resolved (possibly with
    /// per-symbol failures, which are not a degradation of the whole
    /// service). Drives the loud `degraded_types` finding and `has_types`
    /// on the PR-result payload (carrick#535) — before it existed, a dead
    /// sidecar cost the run every type verdict and said so in one log line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub types_degraded: Option<TypeDegradation>,
    /// Why type extraction was skipped or failed for this service, if it was.
    /// `None` means types were resolved normally. Set so the index records
    /// that this service's data is degraded (endpoints without types) instead
    /// of that being indistinguishable from "endpoints have no types".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_extraction_status: Option<String>,
    /// Per-pair type-compat verdicts for cross-repo edges where THIS
    /// repo's calls are the consumer, keyed by canonical pair identity (#351).
    /// Additive and optional: blobs scanned before this field carry `None`, and
    /// the MCP `check_compatibility` tool falls back to structural-matching-only
    /// (fail closed) for any pair without a stored verdict. Populated after
    /// cross-repo type checking by `attach_compat_verdicts`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat_verdicts: Option<Vec<CompatVerdict>>,
    /// v2 capture stub package for this service ("tsc as the serializer"):
    /// the compiler-emitted declaration tree + pinned deps, captured at scan
    /// time and re-assembled into the synthetic check workspace at cross-repo
    /// time. Inline file map for now; WP5 replaces the transport with
    /// content-addressed S3 tarballs + descriptors. A peer with `None` (or a
    /// mismatched `artifact_version`) is treated as having no surface: its
    /// pairs verdict unverifiable with a re-scan reason, never compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_stub: Option<CaptureStubArtifact>,
    /// Deterministic, AST-only outbound call candidates for this service
    /// (carrick#510). A parallel data channel, not part of the operation
    /// index: these rows are never projected into `endpoints` or `calls`, and
    /// nothing in matching or type compatibility reads them. The downstream
    /// consumer is an egress-inventory surface that enumerates where a service
    /// talks to something it does not own.
    ///
    /// Additive and optional: blobs scanned before the field existed carry
    /// `None`, which reads as "this scan predates the channel", not as "this
    /// service makes no external calls".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_call_candidates: Option<Vec<ExternalCallCandidate>>,
    /// The callable surface THIS repo publishes as an npm package
    /// (carrick#466): every member path its entry module exports, anchored to
    /// the source span implementing it. Computed deterministically by
    /// [`crate::sdk_surface`], and read only by the SDK-edge join — nothing in
    /// matching or type compatibility touches it.
    ///
    /// `None` on a repo that publishes no resolvable TypeScript entry, and on
    /// every blob written before the field existed; the join treats the second
    /// case as "member not found" and logs it, because a peer that predates
    /// the channel cannot be told from one with no surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdk_surface: Option<Vec<crate::sdk_surface::SdkMember>>,
    /// SDK-mediated consumer edges where THIS repo is the consumer, computed
    /// at cross-repo time by [`crate::sdk_edges`]. Same storage convention as
    /// `compat_verdicts`: consumer-side only, canonical keys, additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdk_edges: Option<Vec<SdkEdge>>,
    /// Why this repo's remaining SDK calls produced no edge, per package and
    /// reason. Kept beside `sdk_edges` so "no edges" is distinguishable from
    /// "no SDK calls".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdk_unresolved: Option<Vec<SdkUnresolved>>,
    /// The scanner release (`CARGO_PKG_VERSION`) that produced this upload.
    /// The cloud treats a stored row as current only when the commit hash AND
    /// the `scanner_version` match, so a scanner release re-indexes an
    /// unchanged repo once instead of being short-circuited forever by the hash
    /// alone. Without it, a release that fixes extraction could never refresh a
    /// repo whose code had not moved: the run printed "Uploaded" and the index
    /// kept the rows the previous scanner wrote.
    ///
    /// Additive and optional: blobs written before the field existed carry
    /// `None`. Set on every production upload; test fixtures leave it `None`,
    /// and it is deliberately absent from the placeholder blob the download
    /// path builds for an adjacent repo with no metadata — that row is someone
    /// else's scan, not this scanner's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scanner_version: Option<String>,
    /// What this service's scan could not classify (carrick#705): the files it
    /// lost, the calls it could not place, the operations it has no type for,
    /// each with its reasons capped and its exact total kept.
    ///
    /// Additive and optional. A blob written before the field existed carries
    /// `None`, which reads as "this scanner does not state its boundary" — the
    /// one thing it must never read as is "this scan had no boundary".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary: Option<crate::boundary::ServiceBoundary>,
    /// Handlers in this service that switch on a request field, with the
    /// values each answers (carrick#831).
    ///
    /// A fact about the HANDLER, kept beside the operations rather than inside
    /// them, because a handler states it whether or not it declares a route.
    /// For a routed handler the same fact is also on each operation row as its
    /// own `dispatch` case; for a ROUTELESS one — an API-gateway lambda, where
    /// the route lives in infrastructure and not in the source — this array is
    /// the only place the nine operations behind it are stated at all, and no
    /// operation row is invented for them. Promotion to rows is the
    /// `operations` block in `carrick.json`.
    ///
    /// Nothing in matching reads it. Additive and optional: a blob written
    /// before the field existed carries `None`, which reads as "this scanner
    /// does not state dispatch tables", never as "this service has none".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_tables: Option<Vec<crate::dispatch::DispatchTable>>,
}

/// Version of the v2 capture stub artifact schema. Bumped on incompatible
/// changes; the check phase treats a peer scanned with a different version as
/// having no surface (its pairs are unverifiable with a re-scan reason).
pub const CAPTURE_ARTIFACT_VERSION: u32 = 1;

/// The v2 capture stub package as it travels between scan time and check
/// time: a types-only npm package (package.json + tsconfig.snapshot.json +
/// carrick-manifest.json + types/ declaration tree) flattened to a file map.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CaptureStubArtifact {
    pub artifact_version: u32,
    /// `@carrick/<sanitized-service>` package name the stub was emitted as.
    pub package_name: String,
    pub ts_version: String,
    /// True when the source repo had no node_modules at capture time.
    pub bare_checkout: bool,
    /// Stub-relative path -> file content. BTreeMap for stable serialization.
    pub files: std::collections::BTreeMap<String, String>,
}

impl CaptureStubArtifact {
    /// Read a capture stub directory (as produced by `capture_v2`) into the
    /// wire artifact.
    pub fn from_stub_dir(
        stub_dir: &std::path::Path,
        package_name: &str,
        ts_version: &str,
        bare_checkout: bool,
    ) -> std::io::Result<Self> {
        let mut files = std::collections::BTreeMap::new();
        collect_stub_files(stub_dir, stub_dir, &mut files)?;
        Ok(Self {
            artifact_version: CAPTURE_ARTIFACT_VERSION,
            package_name: package_name.to_string(),
            ts_version: ts_version.to_string(),
            bare_checkout,
            files,
        })
    }

    /// Write the artifact back out as a stub directory under `dest`.
    pub fn materialize(&self, dest: &std::path::Path) -> std::io::Result<()> {
        for (rel, content) in &self.files {
            let path = dest.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, content)?;
        }
        Ok(())
    }
}

fn collect_stub_files(
    root: &std::path::Path,
    dir: &std::path::Path,
    files: &mut std::collections::BTreeMap<String, String>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            // node_modules only appears transiently (self-check symlink); it
            // must never ride the artifact.
            if entry.file_name() == "node_modules" {
                continue;
            }
            collect_stub_files(root, &path, files)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| std::io::Error::other(e.to_string()))?
                .to_string_lossy()
                .replace('\\', "/");
            files.insert(rel, std::fs::read_to_string(&path)?);
        }
    }
    Ok(())
}

impl CloudRepoData {
    /// Create CloudRepoData directly from multi-agent analysis results
    /// This bypasses the legacy Analyzer adapter layer
    pub fn from_multi_agent_results(
        repo_name: String,
        repo_path: &str,
        analysis_result: &MultiAgentAnalysisResult,
        config_json: Option<String>,
        package_json: Option<String>,
        packages: Option<Packages>,
        function_definitions: HashMap<String, FunctionDefinition>,
    ) -> Self {
        // Extract service_name from config_json if present
        let service_name = config_json.as_ref().and_then(|json| {
            serde_json::from_str::<serde_json::Value>(json)
                .ok()
                .and_then(|v| {
                    v.get("serviceName")
                        .and_then(|s| s.as_str())
                        .map(String::from)
                })
        });
        let mount_graph = &analysis_result.mount_graph;

        // Project endpoints + consumer calls through the shared helper so the
        // consumer key is the pre-computed `canonical_path` (identical to the
        // manifest join key).
        let (endpoints, calls) = mount_graph_to_api_details(mount_graph);

        // Convert MountEdges to Mount
        let mounts: Vec<Mount> = mount_graph
            .get_mounts()
            .iter()
            .map(|mount| Mount {
                parent: OwnerType::App(mount.parent.clone()),
                child: OwnerType::Router(mount.child.clone()),
                prefix: mount.path_prefix.clone(),
            })
            .collect();

        debug!(
            endpoints = endpoints.len(),
            calls = calls.len(),
            mounts = mounts.len(),
            function_definitions = function_definitions.len(),
            "Created CloudRepoData directly from multi-agent results"
        );

        Self {
            repo_name,
            service_name,
            endpoints,
            calls,
            mounts,
            apps: HashMap::new(),
            imported_handlers: vec![],
            function_definitions,
            config_json,
            package_json,
            packages,
            last_updated: Utc::now(),
            commit_hash: get_current_commit_hash(repo_path),
            dirty: None,
            mount_graph: Some(mount_graph.clone()), // Store mount graph for cross-repo analysis
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
            // Stamp the release that produced this blob so the cloud can tell
            // "same commit, same scanner" (skip) from "same commit, newer
            // scanner" (re-index).
            scanner_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            // Collected once the blob is complete and its paths are relative;
            // see `crate::boundary::ServiceBoundary::collect`.
            boundary: None,
            dispatch_tables: None,
        }
    }
}

/// Project a `MountGraph`'s endpoints and consumer calls into the
/// `ApiEndpointDetails` shape shared by the cloud index. Returns
/// `(endpoints, calls)`.
///
/// This is the single place both cloud projections
/// (`CloudRepoData::from_multi_agent_results` and the engine's
/// `build_cloud_data_from_mount_graph`) key their operations, so a producer
/// endpoint keys on `full_path` and a consumer call keys on the pre-computed
/// `canonical_path` — the same key the type manifest joins on.
pub fn mount_graph_to_api_details(
    mount_graph: &MountGraph,
) -> (Vec<ApiEndpointDetails>, Vec<ApiEndpointDetails>) {
    let endpoints: Vec<ApiEndpointDetails> = mount_graph
        .get_resolved_endpoints()
        .iter()
        .map(|endpoint| ApiEndpointDetails {
            owner: Some(OwnerType::App(endpoint.owner.clone())),
            key: OperationKey::http(&endpoint.method, endpoint.full_path.clone()),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: endpoint.handler.clone(),
            request_type: None,
            response_type: None,
            file_path: PathBuf::from(&endpoint.file_location),
            repo_name: None,
            service_name: None,
            provenance: endpoint.provenance,
            resolution_source: endpoint.resolution_source,
            view_module: endpoint.view_module,
            // The case this operation answers (carrick#831). Part of the
            // operation's identity, so it travels onto the index row: without
            // it, nine operations behind one route are one row on the wire.
            dispatch: endpoint.dispatch.clone(),
        })
        .collect();

    let calls: Vec<ApiEndpointDetails> = mount_graph
        .get_data_calls()
        .iter()
        .map(|call| ApiEndpointDetails {
            owner: None,
            key: OperationKey::http(&call.method, call.canonical_path.clone()),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: Some(call.client.clone()),
            request_type: None,
            response_type: None,
            file_path: PathBuf::from(&call.file_location),
            repo_name: None,
            service_name: None,
            // Provenance is producer-side metadata; calls keep the default.
            provenance: Default::default(),
            resolution_source: call.resolution_source,
            // Likewise: a view module is a property of a route's module.
            view_module: false,
            // The value this call sends for the field its target dispatches
            // on (carrick#831): the consumer half of the same identity.
            dispatch: call.dispatch.clone(),
        })
        .collect();

    (endpoints, calls)
}

#[derive(Debug)]
pub enum StorageError {
    ConnectionError(String),
    SerializationError(String),
    #[allow(dead_code)]
    NotFound(String),
    #[allow(dead_code)]
    DatabaseError(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::ConnectionError(msg) => write!(f, "Connection error: {}", msg),
            StorageError::SerializationError(msg) => write!(f, "Serialization error: {}", msg),
            StorageError::NotFound(msg) => write!(f, "Not found: {}", msg),
            StorageError::DatabaseError(msg) => write!(f, "Database error: {}", msg),
        }
    }
}

impl Error for StorageError {}

/// What the cloud did with one uploaded payload.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UploadOutcome {
    /// The cloud already held a row for this (repo, service) carrying both this
    /// commit hash and this scanner version, so it skipped re-indexing and
    /// nothing this run computed was stored. Only ever true for `AwsStorage`;
    /// the mock and local-dir backends always write, so they report `false`.
    pub already_current: bool,
    /// What this scan cost, on the last write action of a laptop run and
    /// nowhere else (carrick#995). `None` on every CI upload, on every write
    /// before the last one, and on a cloud that does not send it.
    pub scan_spend: Option<crate::scan_spend::ScanSpend>,
}

#[async_trait]
pub trait CloudStorage {
    /// Upload one service's payload.
    ///
    /// `final_in_run` marks the last write action of the whole run. A
    /// multi-service repo sends N of these, and only the last releases the
    /// cloud's in-flight scan slot — releasing on the first would leave the
    /// rest of the run unprotected. The engine materialises every payload
    /// before it uploads any, so it is the only place that knows which is
    /// last (carrick-cloud `docs/internal/reference/laptop-scan-seam.md`
    /// §2.2).
    async fn upload_repo_data(
        &self,
        data: &CloudRepoData,
        final_in_run: bool,
    ) -> Result<UploadOutcome, StorageError>;

    /// Open the run: prove connectivity, and on the laptop path claim the
    /// scan slot and resolve the project before a single model call is paid
    /// for.
    ///
    /// Replaces the health-check probe on that path rather than joining it.
    /// The probe is a `check-or-upload` with `repo: "health"`, which a
    /// workspace-scoped credential cannot reach, and the engine `?`s it — so
    /// the first thing a laptop run did was fail. `start-scan` proves the
    /// same things and more (C9).
    ///
    /// The default is today's probe, which is what every non-cloud backend
    /// and the whole CI path keep doing.
    async fn begin_run(&self, _run: &RunContext) -> Result<RunStart, StorageError> {
        self.health_check().await.map(|()| RunStart::default())
    }

    /// Whether this backend can store more than one service per git repo
    /// without collision. The production index keys on
    /// (workspace, project, repo) only — no service discriminator — so real
    /// uploads of multiple services from one repo would overwrite each other.
    /// Mock storage records uploads in memory and is safe, so it overrides
    /// this. Gates multi-service index upload in the engine.
    fn supports_multi_service(&self) -> bool {
        false
    }

    /// Whether this backend can carry a `CloudRepoData` that exceeds
    /// [`INLINE_PAYLOAD_LIMIT_BYTES`] without dropping anything from it.
    ///
    /// `AwsStorage` PUTs oversized payloads to a presigned staging object
    /// (carrick#486), which has no request-size wall, so the incremental
    /// caches survive at any size. The engine's payload-size guard reads this:
    /// when it is false the caches are dropped to fit the request under the
    /// wall, and the next scan re-analyzes every file (carrick#536).
    fn stages_oversized_payloads(&self) -> bool {
        false
    }

    async fn download_all_repo_data(
        &self,
    ) -> Result<(Vec<CloudRepoData>, HashMap<String, String>), StorageError>;
    #[allow(dead_code)]
    async fn upload_type_file(
        &self,
        repo_name: &str,
        file_name: &str,
        content: &str,
    ) -> Result<(), StorageError>;
    async fn health_check(&self) -> Result<(), StorageError>;
    async fn upload_logs(&self, repo: &str, log_content: &str) -> Result<(), StorageError>;

    /// Whether this run's debug log is shipped to the cloud at all.
    ///
    /// True everywhere it always was. False on the laptop path, where
    /// `upload-logs` is not an action the credential may take and the log
    /// names the developer's own machine.
    fn uploads_run_logs(&self) -> bool {
        true
    }

    /// Relay a PR run's structured findings to the cloud, which renders and
    /// posts (and updates in place on later pushes) a single GitHub App
    /// comment + check run on the PR. Only called on `pull_request` runs —
    /// index data is deliberately not uploaded there, so this is the one
    /// signal a PR run sends.
    ///
    /// The payload's `run_id` lets the cloud re-run this exact workflow run
    /// when a sibling repo's main changes (see carrick-cloud
    /// docs/internal/fanout-pr-rerun.md), and `head_sha` anchors the check
    /// run. Wire shape: docs/internal/pr-result-pipeline.md in carrick-cloud.
    async fn post_pr_result(
        &self,
        payload: &crate::findings::PrResultPayload,
    ) -> Result<(), StorageError>;
}

/// The wire form of one direction's outcome. The two structs are deliberately
/// separate: [`crate::analyzer::PairDirectionOutcome`] is scan-internal and may
/// gain fields no reader is promised, this one is the contract.
pub(crate) fn direction_verdict(
    outcome: &crate::analyzer::PairDirectionOutcome,
) -> DirectionVerdict {
    DirectionVerdict {
        verdict: outcome.verdict,
        reason: outcome.reason.clone(),
        resolved: outcome.resolved,
        unresolved_reason: outcome.unresolved_reason.clone(),
    }
}

/// Fold an incoming direction into a stored one, worst-wins by
/// [`crate::operation::TypeVerdict::combine`]. A direction only one side states
/// is kept as stated: the other call site compared nothing there, and nothing
/// never overrides an answer.
fn merge_direction(stored: &mut Option<DirectionVerdict>, incoming: &Option<DirectionVerdict>) {
    let Some(incoming) = incoming else { return };
    match stored {
        None => *stored = Some(incoming.clone()),
        Some(existing) => {
            if existing.verdict.combine(incoming.verdict) != existing.verdict {
                *existing = incoming.clone();
            }
        }
    }
}

/// Attach per-pair type-compat verdicts to each service payload, for the cross-repo
/// edges where that service is the CONSUMER. Reads the verdicts off the
/// `CrossRepoMatch` edges `get_results` produced (compat already overlaid), and
/// keys each by the canonical pair identity the cloud reconstructs (#351/#324).
///
/// Fail-closed by construction: only edges the check actually REACHED
/// (`type_verdict.is_some()`) become a `CompatVerdict`; an unreached or
/// unmatched pair simply has no stored verdict, which the cloud reads as "not
/// compared", never "compatible". A pair the check reached and could not
/// verify IS stored, as `verdict: unverifiable` with the reason that says why
/// (carrick#811) — before that, the two states were the same absence, and 74
/// of 82 judged edges on a real service pair reached the cloud as silence.
/// Multiple call sites collapsing onto one producer/consumer canonical pair
/// are deduped worst-wins via [`TypeVerdict::combine`] (incompatible >
/// unverifiable > compatible), so a real mismatch is never masked by a sibling
/// call site that happened to agree, and a pair that compared nothing is never
/// upgraded by one that did. That dedup runs PER DIRECTION (carrick#822): a
/// sibling call site that resolved the response half cannot upgrade this row's
/// request half, and vice versa.
pub fn attach_compat_verdicts(
    payloads: &mut [CloudRepoData],
    matches: &[crate::analyzer::CrossRepoMatch],
    directions: &crate::analyzer::PairDirections,
) {
    let scanner_version = env!("CARGO_PKG_VERSION");
    for payload in payloads.iter_mut() {
        let service_id = payload
            .service_name
            .clone()
            .unwrap_or_else(|| payload.repo_name.clone());

        // Dedup by canonical pair key; incompatible wins if call sites disagree.
        let mut by_pair: std::collections::BTreeMap<
            (String, String, String, String),
            CompatVerdict,
        > = std::collections::BTreeMap::new();

        for m in matches {
            if m.consumer_repo != service_id {
                continue;
            }
            // The directions this run filed for the edge, joined by the same
            // key the per-edge overlay used, so a reader never has to pair a
            // verdict with its resolution itself.
            let dirs = directions.for_edge(m);
            if dirs.is_empty() {
                // No pair outcome reached this edge — persist nothing, so an
                // absent row means "not compared" and nothing else (fail
                // closed).
                continue;
            }
            let pair = (
                m.producer_repo.clone(),
                m.producer_key.clone(),
                m.consumer_repo.clone(),
                m.consumer_key.clone(),
            );
            let row = CompatVerdict {
                producer_repo: m.producer_repo.clone(),
                producer_key: m.producer_key.clone(),
                consumer_repo: m.consumer_repo.clone(),
                consumer_key: m.consumer_key.clone(),
                request: dirs.request.as_ref().map(direction_verdict),
                response: dirs.response.as_ref().map(direction_verdict),
                scanner_version: scanner_version.to_string(),
            };
            by_pair
                .entry(pair)
                .and_modify(|existing| {
                    // Worst-wins on the same canonical pair, PER DIRECTION, by
                    // the one precedence the scanner states
                    // (`TypeVerdict::combine`): a direction is replaced only
                    // when the incoming verdict is worse than the stored one,
                    // so equal verdicts keep the first — and `matches` is
                    // sorted, so that is deterministic. Folding the two
                    // directions together here would put back exactly the
                    // conflation carrick#822 removed.
                    merge_direction(&mut existing.request, &row.request);
                    merge_direction(&mut existing.response, &row.response);
                })
                .or_insert(row);
        }

        payload.compat_verdicts = if by_pair.is_empty() {
            None
        } else {
            Some(by_pair.into_values().collect())
        };
    }
}

pub fn get_current_commit_hash(repo_path: &str) -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_path)
        // Clear inherited git env so the repo is discovered from repo_path, not
        // an ambient GIT_DIR / GIT_WORK_TREE (e.g. a pre-commit hook or the eval
        // harness subprocess running inside a worktree) — otherwise this records
        // the wrong repo's commit hash. Mirrors get_changed_files in the engine.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::type_sidecar::InferKind;

    /// The cloud compares the stored row's scanner version against this field,
    /// so it has to survive the wire under exactly that name — and stay absent
    /// (not `null`) when unset, so a blob written by an older scanner still
    /// deserializes into `None` rather than failing the whole download.
    #[test]
    fn scanner_version_rides_the_wire_and_is_omitted_when_unset() {
        let mut repo = empty_repo("orders-service", None);
        repo.scanner_version = Some("1.2.3".to_string());

        let json = serde_json::to_value(&repo).unwrap();
        assert_eq!(json["scanner_version"], "1.2.3");

        let back: CloudRepoData = serde_json::from_str(&serde_json::to_string(&repo).unwrap())
            .expect("a stamped blob round-trips");
        assert_eq!(back.scanner_version.as_deref(), Some("1.2.3"));

        // Unset: the key is absent from the payload, which is exactly the
        // shape of every blob the cloud stored before this field existed.
        let unstamped = empty_repo("orders-service", None);
        let json = serde_json::to_string(&unstamped).unwrap();
        assert!(
            !json.contains("scanner_version"),
            "an unset scanner_version must be omitted, not serialized as null"
        );
        let back: CloudRepoData =
            serde_json::from_str(&json).expect("old cloud data (no field) still deserializes");
        assert!(back.scanner_version.is_none());
    }

    /// The full-analysis upload path stamps the running release. Without this
    /// the cloud can never tell "same commit, newer scanner" from "same commit,
    /// same scanner" and a release would never re-index an unchanged repo.
    #[test]
    fn from_multi_agent_results_stamps_the_running_scanner_version() {
        let analysis_result = MultiAgentAnalysisResult {
            framework_detection: DetectionResult {
                frameworks: vec![],
                data_fetchers: vec![],
                messaging_clients: vec![],
                notes: String::new(),
            },
            framework_guidance: ProtocolGuidance::new(),
            mount_graph: MountGraph::new(),
            file_results: HashMap::new(),
            raw_model_results: HashMap::new(),
            stats: Default::default(),
        };

        let data = CloudRepoData::from_multi_agent_results(
            "orders-service".to_string(),
            ".",
            &analysis_result,
            None,
            None,
            None,
            HashMap::new(),
        );

        assert_eq!(
            data.scanner_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }

    /// The flattened key is the wire contract with the manifest matcher:
    /// entries must serialize with flat `protocol`/`method`/`path` fields,
    /// and round-trip back into the tagged key.
    #[test]
    fn manifest_entry_serializes_flat_protocol_fields() {
        let key = OperationKey::http("GET", "/api/users/:id");
        let entry = TypeManifestEntry {
            key: key.clone(),
            role: ManifestRole::Producer,
            type_kind: ManifestTypeKind::Response,
            type_alias: "Endpoint_abc_Response".to_string(),
            file_path: "src/routes.ts".to_string(),
            line_number: 12,
            is_explicit: false,
            type_state: ManifestTypeState::Unknown,
            evidence: TypeEvidence {
                file_path: "src/routes.ts".to_string(),
                span_start: None,
                span_end: None,
                line_number: 12,
                infer_kind: InferKind::ResponseBody,
                is_explicit: false,
                type_state: ManifestTypeState::Unknown,
            },
            resolved_definition: None,
            expanded_definition: None,
            primary_type_symbol: None,
            defined_in: None,
            any_provenance: Vec::new(),
            v1_unresolved: false,
        };

        let json: serde_json::Value = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["protocol"], "http");
        assert_eq!(json["method"], "GET");
        assert_eq!(json["path"], "/api/users/:id");

        let back: TypeManifestEntry = serde_json::from_value(json).unwrap();
        assert_eq!(back.key, key);
    }

    /// The uploaded index (extraction output) must carry each endpoint's
    /// provenance so downstream matching/rendering can tell a mock producer
    /// from a real route (#380). Calls keep the `Route` default.
    #[test]
    fn mount_graph_projection_carries_endpoint_provenance() {
        use crate::mount_graph::ResolvedEndpoint;
        use crate::operation::EndpointProvenance;

        let mut graph = MountGraph::new();
        graph.endpoints.push(ResolvedEndpoint {
            view_module: false,
            method: "GET".to_string(),
            path: "/api/widgets".to_string(),
            full_path: "/api/widgets".to_string(),
            handler: Some("handler".to_string()),
            owner: "http".to_string(),
            file_location: "src/mocks/handlers.ts:5".to_string(),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: EndpointProvenance::Mock,
            evidence: carrick_match::MatchEvidence::RouteDefinition,
            resolution_source: None,
            dispatch: None,
        });

        let (endpoints, _calls) = mount_graph_to_api_details(&graph);
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].provenance, EndpointProvenance::Mock);

        // And it is on the serialized wire (the index blob).
        let json = serde_json::to_value(&endpoints[0]).unwrap();
        assert_eq!(json["provenance"], "mock");
    }

    /// The uploaded index carries which layer stated each row (carrick#660):
    /// the projection copies it off the mount-graph row on both sides, and a
    /// row that predates the field reads as "not stated", never as the model's.
    #[test]
    fn mount_graph_projection_carries_the_resolution_source() {
        use crate::agents::file_analyzer_agent::ResolutionSource;
        use crate::mount_graph::{DataFetchingCall, ResolvedEndpoint};

        let mut graph = MountGraph::new();
        graph.endpoints.push(ResolvedEndpoint {
            method: "GET".to_string(),
            path: "/api/widgets".to_string(),
            full_path: "/api/widgets".to_string(),
            handler: Some("loader".to_string()),
            owner: "http".to_string(),
            file_location: "app/routes/api.widgets.ts:4".to_string(),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
            resolution_source: Some(ResolutionSource::FileBasedRoute),
            view_module: false,
            dispatch: None,
        });
        // A route whose module also renders a view (carrick#704).
        graph.endpoints.push(ResolvedEndpoint {
            method: "GET".to_string(),
            path: "/admin".to_string(),
            full_path: "/admin".to_string(),
            handler: Some("loader".to_string()),
            owner: "http".to_string(),
            file_location: "app/routes/admin._index.tsx:7".to_string(),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
            resolution_source: Some(ResolutionSource::FileBasedRoute),
            view_module: true,
            dispatch: None,
        });
        graph.data_calls.push(DataFetchingCall {
            method: "POST".to_string(),
            target_url: "${process.env.GATEWAY_URL}/v1/quote".to_string(),
            canonical_path: "/v1/quote".to_string(),
            client: "gatewayClient".to_string(),
            file_location: "src/ledger.ts:31".to_string(),
            call_kind: None,
            repo_name: None,
            service_name: None,
            host: None,
            line: Some(31),
            base: None,
            consumers_not_resolved: None,
            resolution_source: Some(ResolutionSource::WholeUrlEnv),
            dispatch: None,
        });

        let (endpoints, calls) = mount_graph_to_api_details(&graph);
        assert_eq!(
            endpoints[0].resolution_source,
            Some(ResolutionSource::FileBasedRoute)
        );
        assert_eq!(
            calls[0].resolution_source,
            Some(ResolutionSource::WholeUrlEnv)
        );

        // And on the serialized wire, under the snake_case enum spelling.
        assert_eq!(
            serde_json::to_value(&endpoints[0]).unwrap()["resolution_source"],
            "file_based_route"
        );
        assert_eq!(
            serde_json::to_value(&calls[0]).unwrap()["resolution_source"],
            "whole_url_env"
        );
    }

    /// carrick#831: the dispatch case survives the one projection both cloud
    /// paths go through, on the producer side and the consumer side, and
    /// reaches the wire only where a row has one.
    #[test]
    fn mount_graph_projection_carries_the_dispatch_case() {
        use crate::dispatch::{Dispatch, DispatchLocation};
        use crate::mount_graph::{DataFetchingCall, ResolvedEndpoint};

        let case = |value: &str| Dispatch {
            location: DispatchLocation::Body,
            field: "action".to_string(),
            value: value.to_string(),
        };
        let mut graph = MountGraph::new();
        let route = |dispatch: Option<Dispatch>| ResolvedEndpoint {
            method: "POST".to_string(),
            path: "/types/check-or-upload".to_string(),
            full_path: "/types/check-or-upload".to_string(),
            handler: Some("handler".to_string()),
            owner: "http".to_string(),
            file_location: "lambdas/check-or-upload/index.ts:0".to_string(),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
            resolution_source: None,
            view_module: false,
            dispatch,
        };
        graph.endpoints.push(route(Some(case("search-by-intent"))));
        graph.endpoints.push(route(None));
        graph.data_calls.push(DataFetchingCall {
            method: "POST".to_string(),
            target_url: "${API}/types/check-or-upload".to_string(),
            canonical_path: "/types/check-or-upload".to_string(),
            client: "fetch".to_string(),
            file_location: "lambdas/mcp-server/src/api-client.ts:115".to_string(),
            call_kind: None,
            repo_name: None,
            service_name: None,
            host: None,
            line: Some(115),
            base: None,
            consumers_not_resolved: None,
            resolution_source: None,
            dispatch: Some(case("search-by-intent")),
        });

        let (endpoints, calls) = mount_graph_to_api_details(&graph);
        assert_eq!(endpoints[0].dispatch, Some(case("search-by-intent")));
        assert_eq!(endpoints[1].dispatch, None, "a plain route states none");
        assert_eq!(calls[0].dispatch, Some(case("search-by-intent")));

        // On the wire: the three fields verbatim, and no key at all on a row
        // without a case — the majority of rows pay nothing for this field.
        let producer = serde_json::to_value(&endpoints[0]).unwrap();
        assert_eq!(producer["dispatch"]["location"], "body");
        assert_eq!(producer["dispatch"]["field"], "action");
        assert_eq!(producer["dispatch"]["value"], "search-by-intent");
        assert!(
            serde_json::to_value(&endpoints[1]).unwrap()["dispatch"].is_null(),
            "a plain route writes no dispatch key"
        );
        assert_eq!(
            serde_json::to_value(&calls[0]).unwrap()["dispatch"]["value"],
            "search-by-intent"
        );
    }

    /// carrick#831: a blob written before the field existed reads exactly as
    /// it did — every dispatch-shaped field absent, and absence meaning a
    /// plain route rather than an unknown one.
    #[test]
    fn a_blob_without_dispatch_reads_as_it_always_did() {
        let old_blob = serde_json::json!({
            "repo_name": "old",
            "endpoints": [{
                "owner": null,
                "key": { "protocol": "http", "method": "GET", "path": "/things" },
                "params": [],
                "request_body": null,
                "response_body": null,
                "handler_name": "list",
                "request_type": null,
                "response_type": null,
                "file_path": "src/routes.ts:4"
            }],
            "calls": [],
            "mounts": [],
            "apps": {},
            "imported_handlers": [],
            "function_definitions": {},
            "last_updated": "2026-01-01T00:00:00Z",
            "commit_hash": "abc123"
        });

        let data: CloudRepoData =
            serde_json::from_value(old_blob).expect("an old blob still deserializes");
        assert_eq!(data.endpoints[0].dispatch, None);
        assert_eq!(
            data.dispatch_tables, None,
            "absence says this scanner stated no tables, never that there are none"
        );

        // And the same blob with an explicit null, which is what a hand-rolled
        // or partially-migrated writer produces.
        let with_nulls = serde_json::json!({
            "repo_name": "old",
            "endpoints": [],
            "calls": [],
            "mounts": [],
            "apps": {},
            "imported_handlers": [],
            "function_definitions": {},
            "last_updated": "2026-01-01T00:00:00Z",
            "commit_hash": "abc123",
            "dispatch_tables": null
        });
        let data: CloudRepoData =
            serde_json::from_value(with_nulls).expect("an explicit null deserializes too");
        assert_eq!(data.dispatch_tables, None);
    }

    /// carrick#704: `view_module` survives the one projection both cloud paths
    /// go through, and reaches the wire only where it is true. The cloud reads
    /// the mount-graph row for HTTP and this top-level copy as the fallback, so
    /// both halves are asserted.
    #[test]
    fn mount_graph_projection_carries_the_view_module_marker() {
        use crate::agents::file_analyzer_agent::ResolutionSource;
        use crate::mount_graph::{DataFetchingCall, ResolvedEndpoint};

        let mut graph = MountGraph::new();
        let row = |path: &str, file: &str, view_module: bool| ResolvedEndpoint {
            method: "GET".to_string(),
            path: path.to_string(),
            full_path: path.to_string(),
            handler: Some("loader".to_string()),
            owner: "http".to_string(),
            file_location: file.to_string(),
            middleware_chain: vec![],
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            evidence: carrick_match::MatchEvidence::RouteDefinition,
            resolution_source: Some(ResolutionSource::FileBasedRoute),
            view_module,
            dispatch: None,
        };
        graph
            .endpoints
            .push(row("/admin", "app/routes/admin._index.tsx:7", true));
        graph.endpoints.push(row(
            "/resources/things",
            "app/routes/resources.things.tsx:9",
            false,
        ));
        graph.data_calls.push(DataFetchingCall {
            method: "GET".to_string(),
            target_url: "/internal/audit".to_string(),
            canonical_path: "/internal/audit".to_string(),
            client: "fetch".to_string(),
            file_location: "app/routes/settings.builds.tsx:19".to_string(),
            call_kind: None,
            repo_name: None,
            service_name: None,
            host: None,
            line: Some(19),
            base: None,
            consumers_not_resolved: None,
            resolution_source: None,
            dispatch: None,
        });

        let (endpoints, calls) = mount_graph_to_api_details(&graph);
        assert!(endpoints[0].view_module, "the view module keeps its marker");
        assert!(!endpoints[1].view_module, "a resource route is not one");
        assert!(!calls[0].view_module, "a call is never a view module");

        // On the wire: present only where it is true, so the field costs
        // nothing on the rows that are the majority.
        let wire = serde_json::to_value(&endpoints[0]).unwrap();
        assert_eq!(wire["view_module"], true);
        assert!(
            serde_json::to_value(&endpoints[1])
                .unwrap()
                .get("view_module")
                .is_none(),
            "a false marker is absent, not written onto every row"
        );
        assert!(
            serde_json::to_value(&graph.endpoints[0])
                .unwrap()
                .get("view_module")
                .is_some_and(|v| v == true),
            "and the mount-graph row the cloud actually reads carries it too"
        );
    }

    use crate::analyzer::CrossRepoMatch;

    fn empty_repo(repo_name: &str, service_name: Option<&str>) -> CloudRepoData {
        CloudRepoData {
            repo_name: repo_name.to_string(),
            service_name: service_name.map(String::from),
            endpoints: vec![],
            calls: vec![],
            mounts: vec![],
            apps: HashMap::new(),
            imported_handlers: vec![],
            function_definitions: HashMap::new(),
            config_json: None,
            package_json: None,
            packages: None,
            last_updated: Utc::now(),
            commit_hash: "deadbeef".to_string(),
            dirty: None,
            mount_graph: None,
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
            scanner_version: None,
            boundary: None,
            dispatch_tables: None,
        }
    }

    fn edge(
        producer_repo: &str,
        producer_key: &str,
        consumer_repo: &str,
        consumer_key: &str,
        type_compatible: Option<bool>,
        mismatch_reason: Option<&str>,
    ) -> CrossRepoMatch {
        CrossRepoMatch {
            producer_repo: producer_repo.to_string(),
            producer_key: producer_key.to_string(),
            consumer_repo: consumer_repo.to_string(),
            consumer_key: consumer_key.to_string(),
            consumer_location: Some("src/client.ts".to_string()),
            match_score: 1.0,
            type_compatible,
            // The overlay sets both halves together, so a test edge that
            // states one states the other. Since carrick#822 the stored row is
            // built from the check outcomes, not from these fields: use
            // `directions(&[...])` to state what the check found.
            type_verdict: type_compatible.map(|c| {
                if c {
                    crate::operation::TypeVerdict::Compatible
                } else {
                    crate::operation::TypeVerdict::Incompatible
                }
            }),
            mismatch_reason: mismatch_reason.map(String::from),
            producer_provenance: Default::default(),
            relationship: carrick_match::MatchRelationship::ProducerConsumer,
        }
    }

    /// The same edge at a different call site. Two call sites on one canonical
    /// producer/consumer pair is the shape the per-pair dedup exists for, and
    /// they are only distinct edges when their locations differ.
    fn edge_at(
        producer_repo: &str,
        producer_key: &str,
        consumer_repo: &str,
        consumer_key: &str,
        location: &str,
    ) -> CrossRepoMatch {
        let mut e = edge(
            producer_repo,
            producer_key,
            consumer_repo,
            consumer_key,
            None,
            None,
        );
        e.consumer_location = Some(location.to_string());
        e
    }

    /// One check outcome for one direction of one edge, keyed exactly as the v2
    /// checker keys it: the producer key's method and path, and the edge's own
    /// call-site file. Production feeds `attach_compat_verdicts` from these; a
    /// test states the edge and its outcomes in one place rather than setting a
    /// verdict on the edge and hoping the two agree.
    #[allow(clippy::too_many_arguments)]
    fn outcome(
        e: &CrossRepoMatch,
        type_kind: ManifestTypeKind,
        bucket: crate::services::type_sidecar::VerdictBucket,
        diagnostic: Option<&str>,
        resolved: bool,
        unresolved_reason: Option<&str>,
    ) -> crate::analyzer::PairCheckOutcome {
        let rest = e.producer_key.split_once('|').expect("http key").1;
        let (method, path) = rest.split_once('|').expect("http key");
        crate::analyzer::PairCheckOutcome {
            pair_key: format!("{}~{}", e.producer_key, e.consumer_key),
            pseudo_method: method.to_uppercase(),
            identity: path.to_string(),
            consumer_file: e
                .consumer_location
                .clone()
                .unwrap_or_else(|| "src/client.ts".to_string()),
            consumer_line: 1,
            type_kind,
            bucket,
            gate: None,
            diagnostic: diagnostic.map(String::from),
            producer_alias: "P".to_string(),
            consumer_alias: "C".to_string(),
            producer_service: e.producer_repo.clone(),
            consumer_service: e.consumer_repo.clone(),
            resolved,
            unresolved_reason: unresolved_reason.map(String::from),
        }
    }

    /// A compatible outcome that compared two known types.
    fn compatible_outcome(
        e: &CrossRepoMatch,
        type_kind: ManifestTypeKind,
    ) -> crate::analyzer::PairCheckOutcome {
        outcome(
            e,
            type_kind,
            crate::services::type_sidecar::VerdictBucket::Compatible,
            None,
            true,
            None,
        )
    }

    /// An incompatible outcome carrying its diagnostic.
    fn incompatible_outcome(
        e: &CrossRepoMatch,
        type_kind: ManifestTypeKind,
        diagnostic: &str,
    ) -> crate::analyzer::PairCheckOutcome {
        outcome(
            e,
            type_kind,
            crate::services::type_sidecar::VerdictBucket::Incompatible,
            Some(diagnostic),
            true,
            None,
        )
    }

    /// A gate-caught outcome: the check reached this direction and could not
    /// verify it, and says which side carried the `any`.
    fn unverifiable_outcome(
        e: &CrossRepoMatch,
        type_kind: ManifestTypeKind,
        reason: &str,
    ) -> crate::analyzer::PairCheckOutcome {
        outcome(
            e,
            type_kind,
            crate::services::type_sidecar::VerdictBucket::GateCaughtBakedAny,
            None,
            false,
            Some(reason),
        )
    }

    fn directions(
        outcomes: &[crate::analyzer::PairCheckOutcome],
    ) -> crate::analyzer::PairDirections {
        crate::analyzer::PairDirections::from_outcomes(outcomes)
    }

    /// carrick#822, the row this ticket is about: the request halves proved a
    /// mismatch while the response halves compared nothing. The blob states
    /// both, each on its own direction, so a reader can act on the mismatch
    /// without the unresolved response taking the whole row down with it.
    #[test]
    fn each_direction_carries_its_own_verdict_and_resolution() {
        let broken = edge(
            "order-service",
            "http|POST|/users/managers",
            "notification-service",
            "http|POST|/users/managers",
            None,
            None,
        );
        let clean = edge(
            "order-service",
            "http|GET|/health",
            "notification-service",
            "http|GET|/health",
            None,
            None,
        );
        // Checked by nobody this run: no direction, so no row at all.
        let unchecked = edge(
            "order-service",
            "http|GET|/unchecked",
            "notification-service",
            "http|GET|/unchecked",
            None,
            None,
        );
        let dirs = directions(&[
            incompatible_outcome(
                &broken,
                ManifestTypeKind::Request,
                "Property 'passwordHash' is missing",
            ),
            unverifiable_outcome(
                &broken,
                ManifestTypeKind::Response,
                "the consumer type carries `any` at `<0>`",
            ),
            compatible_outcome(&clean, ManifestTypeKind::Request),
            compatible_outcome(&clean, ManifestTypeKind::Response),
        ]);

        let mut payloads = vec![empty_repo(
            "org/notification-service",
            Some("notification-service"),
        )];
        let matches = vec![broken, clean, unchecked];
        attach_compat_verdicts(&mut payloads, &matches, &dirs);

        // Round-trip: the directions are the wire, not just the struct.
        let json = serde_json::to_string(&payloads[0]).unwrap();
        let back: CloudRepoData = serde_json::from_str(&json).unwrap();
        let verdicts = back.compat_verdicts.unwrap();
        let by_key = |key: &str| {
            verdicts
                .iter()
                .find(|v| v.producer_key == key)
                .unwrap_or_else(|| panic!("no verdict for {key}"))
                .clone()
        };

        let split = by_key("http|POST|/users/managers");
        let request = split.request.expect("the request direction is stated");
        assert_eq!(request.verdict, crate::operation::TypeVerdict::Incompatible);
        assert_eq!(
            request.reason.as_deref(),
            Some("Property 'passwordHash' is missing"),
            "the proven mismatch keeps its diagnostic"
        );
        assert!(request.resolved, "the request halves were both known types");
        let response = split.response.expect("the response direction is stated");
        assert_eq!(
            response.verdict,
            crate::operation::TypeVerdict::Unverifiable
        );
        assert!(!response.resolved);
        assert_eq!(
            response.unresolved_reason.as_deref(),
            Some("the consumer type carries `any` at `<0>`"),
            "and the unresolved side names itself, on its own half"
        );

        let clean_row = by_key("http|GET|/health");
        assert_eq!(
            clean_row.request.as_ref().map(|d| d.verdict),
            Some(crate::operation::TypeVerdict::Compatible)
        );
        assert!(clean_row.response.as_ref().is_some_and(|d| d.resolved));

        // A direction the check never reached is omitted from the wire rather
        // than sent as `null` or as a claim that nothing resolved.
        assert!(
            !verdicts
                .iter()
                .any(|v| v.producer_key == "http|GET|/unchecked"),
            "an unchecked pair has no row"
        );
        let raw: serde_json::Value = serde_json::from_str(&json).unwrap();
        let clean_raw = raw["compat_verdicts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["producer_key"] == "http|GET|/health")
            .unwrap();
        assert_eq!(clean_raw["request"]["verdict"], "compatible");
        assert_eq!(clean_raw["request"]["resolved"], true);
        assert!(
            clean_raw["request"].get("reason").is_none(),
            "a compatible direction carries no diagnostic: {clean_raw}"
        );
        assert!(
            clean_raw["request"].get("unresolved_reason").is_none(),
            "nor an unresolution: {clean_raw}"
        );
        for gone in ["verdict", "compatible", "mismatch_reason", "resolved"] {
            assert!(
                clean_raw.get(gone).is_none(),
                "the folded field `{gone}` is replaced, not kept beside: {clean_raw}"
            );
        }
    }

    /// A verdict written before the per-direction split (carrick#822) still
    /// deserializes, and states NOTHING: its folded fields are not on this
    /// struct, and there is no direction to reconstruct them into — the row
    /// never said which half its verdict was about. Nothing is the honest read,
    /// and it is never "compatible" (#324).
    #[test]
    fn a_folded_verdict_from_an_older_scan_states_no_direction() {
        let older = serde_json::json!({
            "producer_repo": "order-service",
            "producer_key": "http|GET|/orders/:id",
            "consumer_repo": "notification-service",
            "consumer_key": "http|GET|/orders/:id",
            "verdict": "incompatible",
            "compatible": false,
            "mismatch_reason": "Order[] vs Order",
            "resolved": false,
            "unresolved_reason": "the consumer type carries `any` at `<0>`",
            "scanner_version": "0.3.48"
        });
        let verdict: CompatVerdict = serde_json::from_value(older).unwrap();
        assert_eq!(verdict.request, None);
        assert_eq!(verdict.response, None);
        // The identity survives, so the row still joins and still says which
        // scanner wrote it.
        assert_eq!(verdict.producer_key, "http|GET|/orders/:id");
        assert_eq!(verdict.scanner_version, "0.3.48");
    }

    /// An old blob that predates `compat_verdicts` deserializes with the field
    /// defaulted to `None` — additive and backwards compatible.
    #[test]
    fn cloud_repo_data_without_compat_verdicts_deserializes_to_none() {
        let json = r#"{
            "repo_name": "org/api",
            "endpoints": [],
            "calls": [],
            "mounts": [],
            "apps": {},
            "imported_handlers": [],
            "function_definitions": {},
            "config_json": null,
            "package_json": null,
            "packages": null,
            "last_updated": "2026-01-01T00:00:00Z",
            "commit_hash": "abc123"
        }"#;
        let data: CloudRepoData = serde_json::from_str(json).unwrap();
        assert!(data.compat_verdicts.is_none());
    }

    /// A blob from a scanner that predates `resolution_source` (v20/v21)
    /// deserializes with the field absent on every row it carries — the
    /// operations, and the mount-graph rows behind them. Absence is the
    /// scanner never having stated it, which is why nothing here defaults to
    /// `Model`.
    #[test]
    fn cloud_repo_data_without_resolution_source_deserializes_to_none() {
        let json = r#"{
            "repo_name": "org/api",
            "endpoints": [{
                "owner": {"App": "http"},
                "key": {"protocol": "http", "method": "GET", "path": "/api/widgets"},
                "params": [],
                "request_body": null,
                "response_body": null,
                "handler_name": "loader",
                "request_type": null,
                "response_type": null,
                "file_path": "app/routes/api.widgets.ts:4"
            }],
            "calls": [],
            "mounts": [],
            "apps": {},
            "imported_handlers": [],
            "function_definitions": {},
            "config_json": null,
            "package_json": null,
            "packages": null,
            "last_updated": "2026-01-01T00:00:00Z",
            "commit_hash": "abc123",
            "mount_graph": {
                "nodes": {},
                "mounts": [],
                "endpoints": [{
                    "method": "GET",
                    "path": "/api/widgets",
                    "full_path": "/api/widgets",
                    "handler": "loader",
                    "owner": "http",
                    "file_location": "app/routes/api.widgets.ts:4",
                    "middleware_chain": []
                }],
                "data_calls": [{
                    "method": "POST",
                    "target_url": "/v1/quote",
                    "canonical_path": "/v1/quote",
                    "client": "gatewayClient",
                    "file_location": "src/ledger.ts:31"
                }]
            }
        }"#;
        let data: CloudRepoData = serde_json::from_str(json).expect("an older blob still reads");
        assert!(data.endpoints[0].resolution_source.is_none());
        let graph = data.mount_graph.expect("the blob carries its mount graph");
        assert!(graph.endpoints[0].resolution_source.is_none());
        assert!(graph.data_calls[0].resolution_source.is_none());

        // carrick#704's field the same way: a blob with no `view_module` key
        // reads as "not a view module" on the operation and on the mount-graph
        // row behind it, never as missing data.
        assert!(!data.endpoints[0].view_module);
        assert!(!graph.endpoints[0].view_module);
    }

    /// The stored rows carry the canonical pair identity the cloud
    /// reconstructs, and survive the wire round trip.
    #[test]
    fn compat_verdicts_round_trip_keyed_canonically() {
        let mut payloads = vec![empty_repo(
            "org/notification-service",
            Some("notification-service"),
        )];
        let broken = edge(
            "order-service",
            "http|GET|/orders/:id",
            "notification-service",
            "http|GET|/orders/:id",
            None,
            None,
        );
        let clean = edge(
            "order-service",
            "http|GET|/health",
            "notification-service",
            "http|GET|/health",
            None,
            None,
        );
        let unchecked = edge(
            "order-service",
            "http|POST|/orders",
            "notification-service",
            "http|POST|/orders",
            None,
            None,
        );
        let dirs = directions(&[
            incompatible_outcome(&broken, ManifestTypeKind::Response, "Order[] vs Order"),
            compatible_outcome(&clean, ManifestTypeKind::Response),
        ]);
        let matches = vec![broken, clean, unchecked];

        attach_compat_verdicts(&mut payloads, &matches, &dirs);
        let verdicts = payloads[0]
            .compat_verdicts
            .clone()
            .expect("verdicts present");
        assert_eq!(verdicts.len(), 2, "only evaluated edges are persisted");

        // Round-trips through the wire.
        let json = serde_json::to_string(&payloads[0]).unwrap();
        let back: CloudRepoData = serde_json::from_str(&json).unwrap();
        let back_verdicts = back.compat_verdicts.unwrap();
        let incompat = back_verdicts
            .iter()
            .find(|v| v.producer_key == "http|GET|/orders/:id")
            .unwrap();
        let response = incompat.response.as_ref().expect("response direction");
        assert_eq!(
            response.verdict,
            crate::operation::TypeVerdict::Incompatible
        );
        assert_eq!(response.reason.as_deref(), Some("Order[] vs Order"));
        assert!(
            incompat.request.is_none(),
            "no request outcome was filed, so no request direction is claimed"
        );
        assert_eq!(incompat.consumer_repo, "notification-service");
        assert_eq!(incompat.scanner_version, env!("CARGO_PKG_VERSION"));

        let compat = back_verdicts
            .iter()
            .find(|v| v.producer_key == "http|GET|/health")
            .unwrap();
        let compat_response = compat.response.as_ref().expect("response direction");
        assert_eq!(
            compat_response.verdict,
            crate::operation::TypeVerdict::Compatible
        );
        assert!(compat_response.reason.is_none());

        // The unevaluated POST /orders pair is absent — the cloud reads its
        // absence as "not compared", never "compatible".
        assert!(
            !back_verdicts
                .iter()
                .any(|v| v.producer_key == "http|POST|/orders"),
            "unevaluated edge must not be persisted"
        );
    }

    /// Verdicts are attributed CONSUMER-side: an edge whose consumer is a
    /// different repo is not stored on this payload.
    #[test]
    fn attach_compat_verdicts_only_stores_consumer_side_edges() {
        let mut payloads = vec![empty_repo("org/order-service", Some("order-service"))];
        // order-service is the PRODUCER here, notification-service the consumer:
        // this verdict belongs on notification-service's blob, not order-service's.
        let e = edge(
            "order-service",
            "http|GET|/orders/:id",
            "notification-service",
            "http|GET|/orders/:id",
            None,
            None,
        );
        let dirs = directions(&[incompatible_outcome(
            &e,
            ManifestTypeKind::Response,
            "Order[] vs Order",
        )]);
        attach_compat_verdicts(&mut payloads, &[e], &dirs);
        assert!(payloads[0].compat_verdicts.is_none());
    }

    /// When two call sites hit the same producer/consumer canonical pair and
    /// disagree, the incompatible verdict wins on that DIRECTION (a real risk
    /// is never masked), and the other direction is untouched by it.
    #[test]
    fn attach_compat_verdicts_dedup_incompatible_wins() {
        let mut payloads = vec![empty_repo("org/consumer", Some("consumer"))];
        let agrees = edge_at("producer", "http|GET|/x", "consumer", "http|GET|/x", "a.ts");
        let breaks = edge_at("producer", "http|GET|/x", "consumer", "http|GET|/x", "b.ts");
        let dirs = directions(&[
            compatible_outcome(&agrees, ManifestTypeKind::Request),
            compatible_outcome(&agrees, ManifestTypeKind::Response),
            incompatible_outcome(&breaks, ManifestTypeKind::Request, "mismatch"),
            compatible_outcome(&breaks, ManifestTypeKind::Response),
        ]);
        attach_compat_verdicts(&mut payloads, &[agrees, breaks], &dirs);
        let verdicts = payloads[0].compat_verdicts.clone().unwrap();
        assert_eq!(verdicts.len(), 1);
        let request = verdicts[0].request.as_ref().unwrap();
        assert_eq!(request.verdict, crate::operation::TypeVerdict::Incompatible);
        assert_eq!(request.reason.as_deref(), Some("mismatch"));
        assert_eq!(
            verdicts[0].response.as_ref().unwrap().verdict,
            crate::operation::TypeVerdict::Compatible,
            "the request-side break does not smear onto the response half"
        );
    }

    /// carrick#811: a direction the check reached and could not verify is
    /// PERSISTED, as the third state — not dropped into the same silence as a
    /// pair nobody checked.
    #[test]
    fn an_unverifiable_direction_is_persisted_as_the_third_state() {
        let mut payloads = vec![empty_repo("org/consumer", Some("consumer"))];
        let e = edge(
            "producer",
            "http|GET|/orders/:id",
            "consumer",
            "http|GET|/orders/:id",
            None,
            None,
        );
        // The judge's own flag and sentence, joined to the same pair.
        let dirs = directions(&[unverifiable_outcome(
            &e,
            ManifestTypeKind::Response,
            "the producer type carries 'any' at 'tenant'",
        )]);
        attach_compat_verdicts(&mut payloads, &[e], &dirs);

        let verdicts = payloads[0].compat_verdicts.clone().unwrap();
        assert_eq!(verdicts.len(), 1, "the reached pair is stored");
        let response = verdicts[0].response.as_ref().unwrap();
        assert_eq!(
            response.verdict,
            crate::operation::TypeVerdict::Unverifiable
        );
        assert_eq!(response.reason, None);
        assert!(!response.resolved);
        assert_eq!(
            response.unresolved_reason.as_deref(),
            Some("the producer type carries 'any' at 'tenant'")
        );
        assert!(
            verdicts[0].request.is_none(),
            "the request half was never reached and claims nothing"
        );
    }

    /// The wire, read as the cloud reads it: each direction's `verdict`
    /// serializes to the exact three strings the mcp-server types declare, and
    /// a direction that compared nothing carries no diagnostic to mistake for
    /// one.
    #[test]
    fn each_direction_rides_the_wire_as_the_three_declared_strings() {
        let mut payloads = vec![empty_repo("org/consumer", Some("consumer"))];
        let a = edge(
            "producer",
            "http|GET|/a",
            "consumer",
            "http|GET|/a",
            None,
            None,
        );
        let b = edge(
            "producer",
            "http|GET|/b",
            "consumer",
            "http|GET|/b",
            None,
            None,
        );
        let c = edge(
            "producer",
            "http|GET|/c",
            "consumer",
            "http|GET|/c",
            None,
            None,
        );
        let dirs = directions(&[
            compatible_outcome(&a, ManifestTypeKind::Request),
            incompatible_outcome(&b, ManifestTypeKind::Request, "A vs B"),
            unverifiable_outcome(&c, ManifestTypeKind::Request, "a side carries `any`"),
        ]);
        attach_compat_verdicts(&mut payloads, &[a, b, c], &dirs);

        let raw: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&payloads[0]).unwrap()).unwrap();
        let row = |key: &str| -> serde_json::Value {
            raw["compat_verdicts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["producer_key"] == key)
                .unwrap()
                .clone()
        };
        assert_eq!(row("http|GET|/a")["request"]["verdict"], "compatible");
        assert_eq!(row("http|GET|/b")["request"]["verdict"], "incompatible");
        assert_eq!(row("http|GET|/b")["request"]["reason"], "A vs B");

        let unverifiable = row("http|GET|/c");
        assert_eq!(unverifiable["request"]["verdict"], "unverifiable");
        assert_eq!(unverifiable["request"]["resolved"], false);
        assert!(
            unverifiable["request"].get("reason").is_none(),
            "a direction that compared nothing has no mismatch to state: {unverifiable}"
        );
        assert!(
            unverifiable.get("response").is_none(),
            "and the unreached half is absent from the wire: {unverifiable}"
        );
    }

    /// An edge no pair outcome reached carries no direction, and absence from
    /// `compat_verdicts` therefore means exactly one thing: not compared.
    #[test]
    fn an_unreached_edge_is_still_absent() {
        let mut payloads = vec![empty_repo("org/consumer", Some("consumer"))];
        let matches = vec![edge(
            "producer",
            "http|GET|/x",
            "consumer",
            "http|GET|/x",
            None,
            None,
        )];
        attach_compat_verdicts(&mut payloads, &matches, &Default::default());
        assert!(payloads[0].compat_verdicts.is_none());
    }

    /// Worst-wins across call sites spans all three states, per direction: a
    /// direction that compared nothing is never upgraded by a sibling call site
    /// that agreed.
    #[test]
    fn attach_compat_verdicts_dedup_unverifiable_beats_compatible() {
        let mut payloads = vec![empty_repo("org/consumer", Some("consumer"))];
        let agrees = edge_at("producer", "http|GET|/x", "consumer", "http|GET|/x", "a.ts");
        let blind = edge_at("producer", "http|GET|/x", "consumer", "http|GET|/x", "b.ts");
        let dirs = directions(&[
            compatible_outcome(&agrees, ManifestTypeKind::Request),
            unverifiable_outcome(&blind, ManifestTypeKind::Request, "a side carries `any`"),
        ]);
        attach_compat_verdicts(&mut payloads, &[agrees, blind], &dirs);
        let verdicts = payloads[0].compat_verdicts.clone().unwrap();
        assert_eq!(verdicts.len(), 1);
        assert_eq!(
            verdicts[0].request.as_ref().unwrap().verdict,
            crate::operation::TypeVerdict::Unverifiable
        );
    }

    /// ...and an incompatible verdict still wins over an unverifiable one,
    /// whichever order the call sites arrive in.
    #[test]
    fn attach_compat_verdicts_dedup_incompatible_beats_unverifiable() {
        let mut payloads = vec![empty_repo("org/consumer", Some("consumer"))];
        let blind = edge_at("producer", "http|GET|/x", "consumer", "http|GET|/x", "a.ts");
        let breaks = edge_at("producer", "http|GET|/x", "consumer", "http|GET|/x", "b.ts");
        let dirs = directions(&[
            unverifiable_outcome(&blind, ManifestTypeKind::Request, "a side carries `any`"),
            incompatible_outcome(&breaks, ManifestTypeKind::Request, "mismatch"),
        ]);
        attach_compat_verdicts(&mut payloads, &[blind, breaks], &dirs);
        let verdicts = payloads[0].compat_verdicts.clone().unwrap();
        assert_eq!(verdicts.len(), 1);
        let request = verdicts[0].request.as_ref().unwrap();
        assert_eq!(request.verdict, crate::operation::TypeVerdict::Incompatible);
        assert_eq!(request.reason.as_deref(), Some("mismatch"));
    }
}
