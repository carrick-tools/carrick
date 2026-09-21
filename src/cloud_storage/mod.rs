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
pub(crate) use aws_storage::{INLINE_PAYLOAD_LIMIT_BYTES, indexed_service_slug};
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
    /// Statements about this direction's comparison that are neither the
    /// verdict nor an unresolution (carrick#1341): an optionality gap that is
    /// legal and still a drift, and the note that the comparison was made
    /// against the serialised form.
    ///
    /// This field exists because `reason` cannot hold either of them. `reason`
    /// is present iff the verdict is `incompatible`, and both statements are
    /// true of pairs the check called COMPATIBLE — an always-sent field the
    /// receiver declares optional assigns, and the wire allowance
    /// (carrick#1340) is precisely what made a producer `Date` read as a
    /// `string` agree. Putting either in `reason` would make a compatible row
    /// read as a mismatch on the cloud side.
    ///
    /// Empty on a row with nothing to add, and `skip_serializing_if` keeps it
    /// off the wire there, so an unchanged verdict is byte-identical to what
    /// the pre-#1341 scanner wrote. A NOTE NEVER MOVES A VERDICT: readers
    /// render it as an observation beside the direction's sentence, and the
    /// verdict comes from the judge (carrick#730/#734).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
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
        // Every field is in the tuple, `notes` included: `SdkEdge` derives
        // `Ord`, so two verdicts differing only in their notes must not
        // compare `Equal` while `PartialEq` calls them different. A field
        // added here and not there breaks the Ord/Eq contract silently.
        (
            rank(self.verdict),
            &self.reason,
            self.resolved,
            &self.unresolved_reason,
            &self.notes,
        )
            .cmp(&(
                rank(other.verdict),
                &other.reason,
                other.resolved,
                &other.unresolved_reason,
                &other.notes,
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
    /// Every consumer CALL SITE this row folded, with that site's own answer
    /// (carrick#1385).
    ///
    /// The four key fields above name a canonical pair, and several call sites
    /// routinely land on one: a hook that sets a request up, the client method
    /// it calls, and the line that issues the request are three sites on one
    /// `(METHOD, path)`. The fold above is what a reader with one row and
    /// several sites to attach it to needs; it is not what a reader listing
    /// those sites needs, and quoting the folded row against each of them
    /// states the worst site's verdict about sites that never produced it.
    ///
    /// The scan already files every outcome per site — the verdict key is
    /// `(producer method, normalized producer path, (consumer file, consumer
    /// line))` — so this field retains what the fold collapses rather than
    /// computing anything new.
    ///
    /// INVARIANT, and the test `the_pair_row_is_the_fold_of_its_sites` asserts
    /// it: the pair-level `request`/`response` are the worst-wins fold of the
    /// sites listed here. A reader that ignores this field reads exactly what
    /// it read before the field existed.
    ///
    /// Empty only on a blob written before the field existed: a persisted row
    /// folded at least one site by construction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<CompatVerdictSite>,
    /// Scanner release that produced this verdict (`CARGO_PKG_VERSION`), so a
    /// reader can see how stale the verdict is relative to the current scanner.
    pub scanner_version: String,
}

/// One consumer call site's own answer, inside the pair row that folds it
/// (carrick#1385).
///
/// The directions are read exactly as [`CompatVerdict::request`] and
/// [`CompatVerdict::response`] are: absent means the check filed no outcome
/// for that half of THIS site, which is not a verdict of any kind.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CompatVerdictSite {
    /// The call site, `"<file>:<line>"`, repo-relative.
    ///
    /// Written as the canonical `(file, line)` identity the check filed the
    /// outcome under, so any column suffix an edge carried is gone and the
    /// string joins byte-for-byte against `parse_file_location` of the
    /// consumer row's own `file_location`. Repo-relative because
    /// `attach_compat_verdicts` runs after `relativize_cloud_paths`.
    pub consumer_location: String,
    /// This site's REQUEST direction. See [`CompatVerdict::request`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<DirectionVerdict>,
    /// This site's RESPONSE direction. See [`CompatVerdict::response`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<DirectionVerdict>,
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

/// What the cloud said when it took a dispatched analysis job (carrick#1229).
#[derive(Debug, Clone)]
pub struct JobSubmission {
    /// The name the job answers to afterwards, for `carrick status` and
    /// `carrick resume`.
    pub job_id: String,
    /// How many prompts the job carries, so the command that dispatched it can
    /// say what is being worked on.
    ///
    /// No estimate of how long it will take: the cloud states none, and a
    /// figure this side invented would be a promise nobody made. What a
    /// dispatched run can honestly say is that the machine does not have to
    /// stay on, and that `carrick status` answers how far it has got.
    pub analyze_rows: usize,
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
///
/// This is the capture half of the cache story, and it is NOT `CACHE_VERSION`.
/// A service's own stub is rebuilt from the AST on every scan and nothing reads
/// the previous blob's copy, so a capture change needs no analysis-cache bump.
/// What IS read back is a PEER's stored artifact: [`crate::engine::type_compat_v2`]
/// materialises the files of every other service's `capture_stub` into the
/// check workspace, and the sidecar reads each one's `carrick-manifest.json`
/// (`capture/check.ts`, `readStubAliasRecords`) to pre-gate pairs. A stale
/// artifact therefore answers today's gate with yesterday's record.
///
/// 2 (0.3.73): three changes to what an artifact says, in one release.
/// carrick#1174 rewrites absolute installed-package specifiers in the
/// declaration text and carrick#1204 scrubs any machine path the rewrite
/// missed, so a version-1 artifact can name a path that exists only on the
/// machine that captured it — materialised anywhere else, those imports
/// resolve to nothing. carrick#1165 and carrick#1164 added the record fields
/// the publish gate and the provenance labels read (`dangling_specifiers`,
/// `undeclared_names`, `unresolved_import`), and a version-1 manifest carries
/// none of them, so its aliases pre-gate exactly as they did before the gate
/// existed. Rather than judge against either, a peer still on version 1 is
/// unverifiable until it re-scans.
pub const CAPTURE_ARTIFACT_VERSION: u32 = 2;

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
            schema_binding: None,
            handler_span: endpoint.handler_span,
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
            schema_binding: None,
            // A call has no handler.
            handler_span: None,
        })
        .collect();

    (endpoints, calls)
}

/// A write action that failed AFTER an attempt whose outcome nobody saw, so
/// the index may or may not carry what this run computed (carrick#1067).
///
/// The shape of the 2026-09-14 incident: `complete-upload` ran for 57 s, the
/// gateway cut the response at 30 s, the one retry was answered 400, and the
/// original handler committed the write 25 s after the cut. A bare error there
/// says "the upload failed" about a write that succeeded, which is why this is
/// a state of its own and not a message.
#[derive(Debug, Clone, PartialEq)]
pub struct UncertainWrite {
    /// The action whose response was lost: `complete-upload` or
    /// `store-metadata`.
    pub action: String,
    /// The lost attempt died on a transport error or a 5xx, so the handler
    /// behind it can still be running. Decides whether the landed-check polls
    /// or asks once: a refusal the cloud answered promptly leaves nothing to
    /// wait for.
    pub handler_may_still_run: bool,
    /// What the transport said, for the run summary.
    pub message: String,
}

#[derive(Debug)]
pub enum StorageError {
    ConnectionError(String),
    SerializationError(String),
    #[allow(dead_code)]
    NotFound(String),
    #[allow(dead_code)]
    DatabaseError(String),
    /// A write whose outcome is unknown rather than failed. The engine answers
    /// it by asking the cloud what it holds, never by ending the run.
    UncertainWrite(UncertainWrite),
}

impl StorageError {
    /// The lost write behind this error, when that is what it is.
    pub fn uncertain_write(&self) -> Option<&UncertainWrite> {
        match self {
            StorageError::UncertainWrite(write) => Some(write),
            _ => None,
        }
    }
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::ConnectionError(msg) => write!(f, "Connection error: {}", msg),
            StorageError::SerializationError(msg) => write!(f, "Serialization error: {}", msg),
            StorageError::NotFound(msg) => write!(f, "Not found: {}", msg),
            StorageError::DatabaseError(msg) => write!(f, "Database error: {}", msg),
            StorageError::UncertainWrite(write) => write!(
                f,
                "Lost the response to '{}': {}",
                write.action, write.message
            ),
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

    /// Whether THIS run's write of this payload reached the stored index — the
    /// question a [`StorageError::UncertainWrite`] leaves open (carrick#1067).
    ///
    /// A read, and a cheap one: the engine asks it only after a write it did
    /// not see the outcome of, and answers `true` by treating that write as
    /// delivered. So it must be exact, and the commit alone is not exact
    /// enough: a `--no-cache` run, or a second scan of a dirty tree, rewrites
    /// a row that ALREADY carries this commit, and a row the previous
    /// generation left there would confirm a write that never happened.
    /// `written_after` is when this run's attempt began, and a row that has
    /// not moved since is not this run's.
    ///
    /// `false` means "this run's index is not what the cloud holds for this
    /// service", which includes every case the check could not settle, so an
    /// unconfirmed write is reported rather than assumed.
    ///
    /// Required rather than defaulted: an `async_trait` default body forces
    /// `Self: Sync` on every generic caller (carrick#956), and each backend
    /// knows its own store.
    async fn index_landed(
        &self,
        data: &CloudRepoData,
        written_after: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, StorageError>;

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

    /// Whether this cloud takes a whole scan's prompts as one job.
    ///
    /// Asked after the run is open, because it is `start-scan` that answers
    /// it. Not async, so the default puts no `Sync` bound on generic callers
    /// (carrick#956).
    fn accepts_analysis_job(&self) -> bool {
        false
    }

    /// Hand the cloud every prompt this run built, instead of asking them one
    /// at a time and waiting (carrick#1229).
    ///
    /// `None` means this backend, or this cloud, does not take analysis jobs —
    /// in which case the run scans synchronously, which is what it did before
    /// this existed. So a scanner that can dispatch in front of a cloud that
    /// cannot is not a broken install, it is an ordinary scan.
    async fn submit_analysis_job(
        &self,
        _bundle: &crate::analysis_job::JobBundle,
    ) -> Result<Option<JobSubmission>, StorageError> {
        Ok(None)
    }

    /// Keep `data`, the generation the index already serves for a service this
    /// run held back, wherever this backend builds a read model from the
    /// run's own writes.
    ///
    /// Only the laptop's storage has such a place: `carrick index` builds the
    /// `.carrick` index from the blobs the scan wrote, and a held-back service
    /// writes none. Everywhere else the index itself is the read model and
    /// already holds it, so the default does nothing. Not async, so the
    /// default body puts no `Sync` bound on generic callers (carrick#956).
    fn keep_served_generation(&self, _data: &CloudRepoData) {}

    /// Tell this backend that the run sent at least one file to the analyzer,
    /// so its write actions must replace the stored generation rather than be
    /// short-circuited on the commit hash (carrick#1306).
    ///
    /// The cloud dedupes a write on (commit, scanner version), and neither
    /// moves when a user prepares the checkout the way the pre-flight refusal
    /// told them to: the generated output is gitignored, so the tree is clean
    /// by git's measure and `--no-cache` was not passed. The scan then
    /// re-analyses, reports better numbers locally, and the index every agent
    /// reads is unchanged. A run that analysed a file knows its answers are
    /// not the stored generation's, and this is it saying so.
    ///
    /// A property of the RUN: called once, before any write action, and every
    /// write action of the run carries the flag afterwards. Not async, so the
    /// default body puts no `Sync` bound on generic callers (carrick#956).
    /// The default does nothing, because only a backend that talks to the
    /// freshness guard has anything to say to it — but a backend that WRAPS
    /// one must forward this or the wrapped cloud never hears it.
    fn note_analyzed_files(&self) {}

    /// Ask the run's final write (the one [`Self::upload_repo_data`] gets with
    /// `final_in_run`) to name the services this run leaves pending, and say
    /// whether it will.
    ///
    /// A laptop scan's final write closes the scan, and the cloud stamps the
    /// repo's first index complete on it. A run that still owes some services
    /// their model analysis must not be stamped, or the re-run that fills them
    /// in is metered under the monthly pool instead of the first-index ceiling
    /// (carrick-cloud#892). `true` means this backend will carry
    /// `pending_services` on that write and the cloud said at `start-scan` it
    /// reads them. `false` means the caller must not mark any write final and
    /// closes the scan with [`Self::report_scan_failed`] instead, which frees
    /// the slot and stamps nothing.
    ///
    /// The default is `false`: only a laptop run against a cloud that answered
    /// `accepts_pending_services` has a first index to keep open. Not async,
    /// so the default puts no `Sync` bound on generic callers (carrick#956).
    fn name_pending_on_final_write(&self, _pending_services: &[String]) -> bool {
        false
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
    /// True everywhere. It was false on the laptop path, where the log was a
    /// 0644 file naming the developer's machine and logging every dependency
    /// at debug — so the one run whose log anyone needed, a first index that
    /// died at minute 66, left the cloud nothing to read (carrick#1063). The
    /// file is now filtered to this crate's own debug lines, and what leaves
    /// the machine is redacted first ([`crate::logging::Redaction`]).
    fn uploads_run_logs(&self) -> bool {
        true
    }

    /// Say that this run died, and in which stage, before it could upload.
    ///
    /// The other half of carrick#1063: a run that ends badly leaves a scan
    /// slot the cloud can only time out, and no row anywhere says what the
    /// run had been doing. `stage` is one of
    /// [`crate::scan_stage::Stage`]'s tokens and `reason` is the error's own
    /// words, redacted and truncated.
    ///
    /// Best-effort by contract, and it takes no `Result` for that reason:
    /// there is nothing a caller could do with a failure to report a failure,
    /// and the run's exit code is the analysis's, never this call's. A default
    /// no-op, because only the laptop path has a slot to mark.
    async fn report_scan_failed(&self, _stage: &str, _reason: &str) {}

    /// Say that this run died before `start-scan` opened a scan (carrick#1096).
    ///
    /// The half [`CloudStorage::report_scan_failed`] cannot cover: a laptop
    /// run that stops at a missing runtime, a service manifest it cannot
    /// read, or a repository `start-scan` never accepted holds no slot, so
    /// there is nothing to mark and the cloud saw nothing at all. This event
    /// claims no slot and counts against no allowance. `repo` is `owner/repo`
    /// when the checkout's origin names one; `stage` and `reason` are shaped
    /// exactly as the fail marker's are.
    ///
    /// Best-effort for the same reasons, and a default no-op for the same
    /// reason: only the laptop credential has anyone to tell.
    async fn report_preflight_failed(&self, _repo: Option<&str>, _stage: &str, _reason: &str) {}

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
        notes: outcome.notes.clone(),
    }
}

/// Worst-wins rank of one direction: the verdict first, then whether it
/// compared two KNOWN types (carrick#839).
///
/// The verdict half is exactly [`crate::operation::TypeVerdict::combine`]'s
/// precedence (incompatible > unverifiable > compatible) written as an order,
/// so a strict increase here is the same replacement decision the fold made
/// before this function existed.
///
/// The resolution half is the tie-break it was missing. Two call sites on one
/// pair and one direction, both `compatible`, one that compared two known
/// types and one that compared over an `any`, used to store whichever arrived
/// first — deterministic, because `matches` is sorted, but not honest: the row
/// could say the check compared two known types while a sibling call site on
/// the same contract compared nothing. An unresolved sibling degrades the
/// stored direction instead, which is the rule
/// [`crate::analyzer::PairDirections::from_outcomes`] already applies WITHIN a
/// direction, and the precedence the cloud's own reader already ranks rows by.
fn direction_rank(direction: &DirectionVerdict) -> (u8, u8) {
    use crate::operation::TypeVerdict;
    let verdict = match direction.verdict {
        TypeVerdict::Compatible => 0,
        TypeVerdict::Unverifiable => 1,
        TypeVerdict::Incompatible => 2,
    };
    (verdict, u8::from(!direction.resolved))
}

/// Fold an incoming direction into a stored one, worst-wins by
/// [`direction_rank`]. A direction only one side states is kept as stated: the
/// other call site compared nothing there, and nothing never overrides an
/// answer.
///
/// `notes` survives the fold either way (carrick#1341), on the same rule
/// [`crate::analyzer::PairDirections::from_outcomes`] uses: worst-wins picks
/// which VERDICT this direction is, and an observation is not a candidate for
/// that. Both call sites really did compare, so both their observations stay,
/// deduped and sorted so the stored bytes do not depend on fold order.
fn merge_direction(stored: &mut Option<DirectionVerdict>, incoming: &Option<DirectionVerdict>) {
    let Some(incoming) = incoming else { return };
    match stored {
        None => *stored = Some(incoming.clone()),
        Some(existing) => {
            let mut notes = std::mem::take(&mut existing.notes);
            notes.extend(incoming.notes.iter().cloned());
            if direction_rank(incoming) > direction_rank(existing) {
                *existing = incoming.clone();
            }
            notes.sort();
            notes.dedup();
            existing.notes = notes;
        }
    }
}

/// Fold an incoming site into the list a pair row carries, by call-site
/// identity (carrick#1385).
///
/// Two edges at one location is a duplicate, not two sites, and it folds
/// through exactly the same [`merge_direction`] the pair row does — otherwise
/// the pair row would stop being the fold of the sites it lists.
fn merge_site(sites: &mut Vec<CompatVerdictSite>, incoming: CompatVerdictSite) {
    if let Some(existing) = sites
        .iter_mut()
        .find(|s| s.consumer_location == incoming.consumer_location)
    {
        merge_direction(&mut existing.request, &incoming.request);
        merge_direction(&mut existing.response, &incoming.response);
        return;
    }
    sites.push(incoming);
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
/// request half, and vice versa. Within a direction it ranks on the verdict
/// AND on whether the site compared two known types (carrick#839), so a
/// `compatible` produced over an `any` no longer masks a sibling's.
///
/// What the dedup collapses is retained beside it: each row lists the call
/// sites it folded and their own answers (`sites`, carrick#1385), so a reader
/// listing an operation's consumers can say what the check found AT a site
/// instead of quoting the worst site's verdict against all of them.
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
            // The call site this edge's outcome was filed for. Every edge that
            // filed one has a location — `verdict_key_of_edge` returns `None`
            // without it, and an edge with no key files nothing — so this is
            // the same condition as the `dirs.is_empty()` check above, stated
            // where the site needs it. Reduced to the canonical `(file, line)`
            // the check keyed on, so a column suffix cannot split one site in
            // two and the string joins against the consumer row's own
            // `file_location`.
            let Some(location) = m.consumer_location.as_deref() else {
                continue;
            };
            let (file, line) = crate::type_manifest::parse_file_location(location);
            let pair = (
                m.producer_repo.clone(),
                m.producer_key.clone(),
                m.consumer_repo.clone(),
                m.consumer_key.clone(),
            );
            let site = CompatVerdictSite {
                consumer_location: format!("{file}:{line}"),
                request: dirs.request.as_ref().map(direction_verdict),
                response: dirs.response.as_ref().map(direction_verdict),
            };
            let row = CompatVerdict {
                producer_repo: m.producer_repo.clone(),
                producer_key: m.producer_key.clone(),
                consumer_repo: m.consumer_repo.clone(),
                consumer_key: m.consumer_key.clone(),
                request: dirs.request.as_ref().map(direction_verdict),
                response: dirs.response.as_ref().map(direction_verdict),
                sites: vec![site.clone()],
                scanner_version: scanner_version.to_string(),
            };
            by_pair
                .entry(pair)
                .and_modify(|existing| {
                    // The site keeps its OWN answer; only the pair-level
                    // fields below fold. The two are written from one `dirs`,
                    // and the same `merge_direction` runs over both, which is
                    // what makes the pair row the fold of this list.
                    merge_site(&mut existing.sites, site.clone());
                    // Worst-wins on the same canonical pair, PER DIRECTION, by
                    // the one precedence the scanner states
                    // ([`direction_rank`]): a direction is replaced only when
                    // the incoming one ranks strictly worse than the stored
                    // one, so two sites that tie on verdict AND on resolution
                    // keep the first — and `matches` is sorted, so that is
                    // deterministic. Folding the two directions together here
                    // would put back exactly the conflation carrick#822
                    // removed.
                    merge_direction(&mut existing.request, &row.request);
                    merge_direction(&mut existing.response, &row.response);
                })
                .or_insert(row);
        }

        payload.compat_verdicts = if by_pair.is_empty() {
            None
        } else {
            Some(
                by_pair
                    .into_values()
                    .map(|mut row| {
                        // By location, so the stored bytes do not depend on
                        // the order the edges arrived in.
                        row.sites.sort();
                        row
                    })
                    .collect(),
            )
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
        // the wrong repo's commit hash. Mirrors `git_state::unchanged_since`.
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
                socket_clients: vec![],
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
            handler_span: None,
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
            handler_span: None,
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
            handler_span: None,
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
            role: None,
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
            handler_span: None,
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
            role: None,
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
            handler_span: None,
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
            role: None,
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
        // The manifest entry production fills these from states the file and
        // the line separately, so a test edge whose location packs a line (or
        // a line and a column) has to be split the same way — the edge side of
        // the key runs the location through `parse_file_location`, and a
        // verbatim `consumer_file` would silently file the outcome under a key
        // no edge can recover.
        let (consumer_file, consumer_line) = crate::type_manifest::parse_file_location(
            e.consumer_location.as_deref().unwrap_or("src/client.ts"),
        );
        crate::analyzer::PairCheckOutcome {
            pair_key: format!("{}~{}", e.producer_key, e.consumer_key),
            pseudo_method: method.to_uppercase(),
            identity: path.to_string(),
            consumer_file,
            consumer_line,
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
            // Set per test with struct-update syntax where a note is the
            // thing under test; every other fixture states none.
            notes: Vec::new(),
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

    /// The wire spelling of `notes` and the two statements it exists to carry
    /// (carrick#1341). A COMPATIBLE direction carries the note, and it reaches
    /// the blob under the key `notes` as a list of strings.
    ///
    /// This is the test the ticket is about: before the field, both statements
    /// were true of a compatible row and had nowhere to go, because `reason`
    /// is present iff the verdict is `incompatible`.
    #[test]
    fn a_compatible_direction_carries_its_notes_to_the_blob() {
        let wire_note = "The producer's type is compared in the form JSON puts \
             on the wire: a value with a toJSON() method (a Date, for example) \
             travels as what it serialises to.";
        let gap_note = "'createdBy' is always sent by the producer and optional \
             on the consumer.";
        let e = edge(
            "order-service",
            "http|GET|/orders/:id",
            "payments-svc",
            "http|GET|/orders/:id",
            None,
            None,
        );
        let outcomes = vec![crate::analyzer::PairCheckOutcome {
            notes: vec![wire_note.to_string(), gap_note.to_string()],
            ..compatible_outcome(&e, ManifestTypeKind::Response)
        }];
        let mut payloads = vec![empty_repo("org/payments-svc", Some("payments-svc"))];
        attach_compat_verdicts(&mut payloads, &[e], &directions(&outcomes));

        let response = payloads[0].compat_verdicts.as_ref().unwrap()[0]
            .response
            .as_ref()
            .unwrap();
        assert_eq!(
            response.verdict,
            crate::operation::TypeVerdict::Compatible,
            "a note must never move the verdict: the judge decides it"
        );
        assert_eq!(response.reason, None, "a note is not a mismatch reason");

        let wire = serde_json::to_value(response).unwrap();
        assert_eq!(
            wire["notes"],
            serde_json::json!([gap_note, wire_note]),
            "spelled `notes`, a list of strings, sorted"
        );
        assert_eq!(
            wire["verdict"], "compatible",
            "the serialised verdict is untouched by the note"
        );
    }

    /// A direction with nothing to observe writes NO `notes` key at all, so a
    /// row whose verdict has not changed is byte-identical to what the
    /// pre-#1341 scanner wrote. Without `skip_serializing_if` this would put
    /// `"notes":[]` on every stored verdict in every blob.
    #[test]
    fn a_direction_with_no_notes_writes_no_notes_key() {
        let e = edge(
            "order-service",
            "http|GET|/orders/:id",
            "payments-svc",
            "http|GET|/orders/:id",
            None,
            None,
        );
        let outcomes = vec![compatible_outcome(&e, ManifestTypeKind::Response)];
        let mut payloads = vec![empty_repo("org/payments-svc", Some("payments-svc"))];
        attach_compat_verdicts(&mut payloads, &[e], &directions(&outcomes));

        let response = payloads[0].compat_verdicts.as_ref().unwrap()[0]
            .response
            .clone()
            .unwrap();
        assert!(response.notes.is_empty());
        let wire = serde_json::to_string(&response).unwrap();
        assert!(
            !wire.contains("notes"),
            "an empty list must not reach the wire; got: {wire}"
        );
        assert_eq!(
            wire, r#"{"verdict":"compatible","resolved":true}"#,
            "byte-identical to the pre-#1341 spelling"
        );
    }

    /// A stored direction written before the field existed reads as NO notes,
    /// and states nothing (carrick#1341). The peer-blob path makes this
    /// reachable in production: an `SdkEdge` takes its directions off a peer's
    /// stored `CompatVerdict`, which may have been written by any older
    /// scanner.
    #[test]
    fn a_stored_direction_without_notes_reads_as_none() {
        let old: DirectionVerdict = serde_json::from_value(serde_json::json!({
            "verdict": "incompatible",
            "reason": "Property 'passwordHash' is missing",
            "resolved": true
        }))
        .expect("a pre-#1341 direction still deserializes");
        assert!(
            old.notes.is_empty(),
            "absence is no notes, never a fabricated one"
        );

        // And the same row inside a whole stored verdict, which is the shape
        // the peer-blob path actually reads back.
        let stored: CompatVerdict = serde_json::from_value(serde_json::json!({
            "producer_repo": "order-service",
            "producer_key": "http|GET|/orders/:id",
            "consumer_repo": "payments-svc",
            "consumer_key": "http|GET|/orders/:id",
            "response": { "verdict": "compatible", "resolved": true },
            "scanner_version": "0.3.82"
        }))
        .expect("a pre-#1341 stored verdict still deserializes");
        assert!(stored.response.unwrap().notes.is_empty());
    }

    /// Several call sites collapsing onto one direction keep EVERY
    /// observation, deduped and sorted, even when worst-wins picks a different
    /// call site's verdict (carrick#1341). A note is a statement about a
    /// comparison that happened, not a candidate for the verdict fold, so the
    /// losing call site's note must survive.
    #[test]
    fn collapsing_call_sites_union_their_notes() {
        let note_a = "'a' is always sent by the producer and optional on the consumer.";
        let note_b = "'b' is always sent by the producer and optional on the consumer.";
        let e = edge(
            "order-service",
            "http|GET|/orders/:id",
            "payments-svc",
            "http|GET|/orders/:id",
            None,
            None,
        );
        let outcomes = vec![
            crate::analyzer::PairCheckOutcome {
                notes: vec![note_b.to_string(), note_a.to_string()],
                ..compatible_outcome(&e, ManifestTypeKind::Response)
            },
            crate::analyzer::PairCheckOutcome {
                notes: vec![note_a.to_string()],
                ..incompatible_outcome(&e, ManifestTypeKind::Response, "shapes disagree")
            },
        ];
        let mut payloads = vec![empty_repo("org/payments-svc", Some("payments-svc"))];
        attach_compat_verdicts(&mut payloads, &[e], &directions(&outcomes));

        let response = payloads[0].compat_verdicts.as_ref().unwrap()[0]
            .response
            .clone()
            .unwrap();
        assert_eq!(
            response.verdict,
            crate::operation::TypeVerdict::Incompatible,
            "worst-wins still decides the verdict"
        );
        assert_eq!(
            response.notes,
            vec![note_a.to_string(), note_b.to_string()],
            "both call sites' observations survive, deduped and sorted"
        );
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

    /// `calls[].schema_binding` on the wire (carrick#1134): the two values in
    /// snake case on GraphQL call rows, and no key at all on a row that has
    /// none, so HTTP rows and every endpoint keep their exact shape.
    #[test]
    fn schema_binding_rides_graphql_call_rows_only() {
        use crate::graphql::SchemaBinding;
        use crate::operation::{GraphqlOperationKind, OperationKey};
        let row = |key: OperationKey, binding: Option<SchemaBinding>| ApiEndpointDetails {
            owner: None,
            key,
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: None,
            response_type: None,
            file_path: std::path::PathBuf::from("src/doc.graphql:2"),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            resolution_source: None,
            view_module: false,
            dispatch: None,
            schema_binding: binding,
            handler_span: None,
        };
        let mut data = empty_repo("org/web", Some("web"));
        data.calls = vec![
            row(
                OperationKey::graphql(GraphqlOperationKind::Query, "products"),
                Some(SchemaBinding::Served),
            ),
            row(
                OperationKey::graphql(GraphqlOperationKind::Mutation, "retire"),
                Some(SchemaBinding::NoLocalSchema),
            ),
            row(OperationKey::http("GET", "/health".to_string()), None),
        ];
        data.endpoints = vec![row(OperationKey::http("GET", "/health".to_string()), None)];

        let wire = serde_json::to_value(&data).unwrap();
        assert_eq!(wire["calls"][0]["schema_binding"], "served");
        assert_eq!(wire["calls"][1]["schema_binding"], "no_local_schema");
        assert!(wire["calls"][2].get("schema_binding").is_none());
        assert!(wire["endpoints"][0].get("schema_binding").is_none());

        let back: CloudRepoData = serde_json::from_value(wire).unwrap();
        assert_eq!(back.calls[0].schema_binding, Some(SchemaBinding::Served));
        assert_eq!(
            back.calls[1].schema_binding,
            Some(SchemaBinding::NoLocalSchema)
        );
        assert_eq!(back.calls[2].schema_binding, None);
    }

    /// A blob from a scanner that predates `schema_binding` reads with the
    /// field absent on its GraphQL call rows.
    #[test]
    fn cloud_repo_data_without_schema_binding_deserializes_to_none() {
        let json = r#"{
            "repo_name": "org/web",
            "endpoints": [],
            "calls": [{
                "owner": null,
                "key": {"protocol": "graphql", "kind": "query", "field": "products"},
                "params": [],
                "request_body": null,
                "response_body": null,
                "handler_name": null,
                "request_type": null,
                "response_type": null,
                "file_path": "src/graphql/catalog.gql:2"
            }],
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
        let data: CloudRepoData = serde_json::from_str(json).expect("an older blob still reads");
        assert!(data.calls[0].schema_binding.is_none());
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

    /// Two EDGES landing on one canonical pair union their notes too
    /// (carrick#1341), which is a different code path from two outcomes on one
    /// edge: this one runs through [`merge_direction`], where worst-wins
    /// REPLACES the whole stored struct. The losing edge's observation has to
    /// survive that replacement, because its comparison happened as much as
    /// the winner's did.
    #[test]
    fn merging_two_edges_unions_their_notes() {
        let winner_note = "'b' is always sent by the producer and optional on the consumer.";
        let loser_note = "'a' is always sent by the producer and optional on the consumer.";
        let mut payloads = vec![empty_repo("org/consumer", Some("consumer"))];
        let agrees = edge_at("producer", "http|GET|/x", "consumer", "http|GET|/x", "a.ts");
        let breaks = edge_at("producer", "http|GET|/x", "consumer", "http|GET|/x", "b.ts");
        let dirs = directions(&[
            // The COMPATIBLE edge, which worst-wins discards, is the one whose
            // note must not be discarded with it.
            crate::analyzer::PairCheckOutcome {
                notes: vec![loser_note.to_string()],
                ..compatible_outcome(&agrees, ManifestTypeKind::Request)
            },
            crate::analyzer::PairCheckOutcome {
                notes: vec![winner_note.to_string()],
                ..incompatible_outcome(&breaks, ManifestTypeKind::Request, "mismatch")
            },
        ]);
        attach_compat_verdicts(&mut payloads, &[agrees, breaks], &dirs);

        let verdicts = payloads[0].compat_verdicts.clone().unwrap();
        assert_eq!(verdicts.len(), 1);
        let request = verdicts[0].request.as_ref().unwrap();
        assert_eq!(
            request.verdict,
            crate::operation::TypeVerdict::Incompatible,
            "worst-wins still decides the verdict across edges"
        );
        assert_eq!(
            request.notes,
            vec![loser_note.to_string(), winner_note.to_string()],
            "the discarded edge's observation survives the replacement, sorted"
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

    /// A `compatible` outcome that compared over an `any` (carrick#839).
    fn compatible_but_unresolved_outcome(
        e: &CrossRepoMatch,
        type_kind: ManifestTypeKind,
        reason: &str,
    ) -> crate::analyzer::PairCheckOutcome {
        outcome(
            e,
            type_kind,
            crate::services::type_sidecar::VerdictBucket::Compatible,
            None,
            false,
            Some(reason),
        )
    }

    /// The pair-level fold, recomputed from the sites a row lists.
    fn fold_of_sites(
        sites: &[CompatVerdictSite],
    ) -> (Option<DirectionVerdict>, Option<DirectionVerdict>) {
        let mut request = None;
        let mut response = None;
        for site in sites {
            merge_direction(&mut request, &site.request);
            merge_direction(&mut response, &site.response);
        }
        (request, response)
    }

    /// carrick#839: two call sites on one pair and one direction, both
    /// `compatible`, one that compared two known types and one that compared
    /// over an `any`. The stored direction is the unresolved one WHICHEVER
    /// order they arrive in — before the resolution tie-break the fold ranked
    /// on the verdict alone, so the row kept whichever came first and could
    /// say the check compared two known types while a sibling call site on the
    /// same contract compared nothing.
    #[test]
    fn a_resolved_site_never_masks_an_unresolved_sibling() {
        for reversed in [false, true] {
            let mut payloads = vec![empty_repo("org/consumer", Some("consumer"))];
            let known = edge_at("producer", "http|GET|/x", "consumer", "http|GET|/x", "a.ts");
            let blind = edge_at("producer", "http|GET|/x", "consumer", "http|GET|/x", "b.ts");
            let dirs = directions(&[
                compatible_outcome(&known, ManifestTypeKind::Response),
                compatible_but_unresolved_outcome(
                    &blind,
                    ManifestTypeKind::Response,
                    "consumer side carries `any` at `data`",
                ),
            ]);
            let mut edges = vec![known, blind];
            if reversed {
                edges.reverse();
            }
            attach_compat_verdicts(&mut payloads, &edges, &dirs);

            let verdicts = payloads[0].compat_verdicts.clone().unwrap();
            assert_eq!(verdicts.len(), 1);
            let response = verdicts[0].response.as_ref().unwrap();
            assert_eq!(
                response.verdict,
                crate::operation::TypeVerdict::Compatible,
                "the verdict axis is untouched: both sites answered compatible"
            );
            assert!(
                !response.resolved,
                "arrival order {reversed}: an unresolved sibling degrades the \
                 stored direction instead of being dropped"
            );
            assert_eq!(
                response.unresolved_reason.as_deref(),
                Some("consumer side carries `any` at `data`"),
                "the unresolution travels with the verdict it qualifies"
            );
        }
    }

    /// carrick#1385: the row lists every call site it folded, each with its own
    /// answer, so a reader listing an operation's consumers can say what the
    /// check found AT a site. The sites here disagree, which is the whole
    /// reported shape: one operation-level `incompatible` was quoted against
    /// four call sites, one of which agreed with the producer.
    #[test]
    fn a_verdict_row_lists_each_call_sites_own_answer() {
        let mut payloads = vec![empty_repo("org/consumer", Some("consumer"))];
        let agrees = edge_at(
            "producer",
            "http|GET|/x",
            "consumer",
            "http|GET|/x",
            "src/lib/client.ts:124",
        );
        let breaks = edge_at(
            "producer",
            "http|GET|/x",
            "consumer",
            "http|GET|/x",
            "src/hooks/use-thing.ts:17:9",
        );
        let dirs = directions(&[
            compatible_outcome(&agrees, ManifestTypeKind::Response),
            incompatible_outcome(&breaks, ManifestTypeKind::Response, "not assignable"),
        ]);
        attach_compat_verdicts(&mut payloads, &[agrees, breaks], &dirs);

        let verdicts = payloads[0].compat_verdicts.clone().unwrap();
        assert_eq!(verdicts.len(), 1, "still one row per canonical pair");
        let sites = &verdicts[0].sites;
        assert_eq!(sites.len(), 2, "both call sites are listed");

        // Sorted by location, and the column suffix the edge carried is gone:
        // a site's identity is the `(file, line)` the check keyed on, which is
        // what the consumer row's own `file_location` reduces to.
        assert_eq!(sites[0].consumer_location, "src/hooks/use-thing.ts:17");
        assert_eq!(sites[1].consumer_location, "src/lib/client.ts:124");

        let broken = sites[0].response.as_ref().expect("the site that broke");
        assert_eq!(broken.verdict, crate::operation::TypeVerdict::Incompatible);
        assert_eq!(broken.reason.as_deref(), Some("not assignable"));

        let fine = sites[1].response.as_ref().expect("the site that agreed");
        assert_eq!(
            fine.verdict,
            crate::operation::TypeVerdict::Compatible,
            "the site that agreed keeps its own answer instead of inheriting \
             the worst site's"
        );
        assert!(fine.reason.is_none());
    }

    /// THE PIN: the pair-level directions are the worst-wins fold of the sites
    /// the row lists. A reader that ignores `sites` reads exactly what it read
    /// before the field existed, and a reader that reads them can recover how
    /// the fold got its answer. Asserted over sites that disagree on BOTH
    /// directions in opposite senses, so neither half can pass by accident.
    #[test]
    fn the_pair_row_is_the_fold_of_its_sites() {
        let mut payloads = vec![empty_repo("org/consumer", Some("consumer"))];
        let first = edge_at(
            "producer",
            "http|POST|/x",
            "consumer",
            "http|POST|/x",
            "src/a.ts:3",
        );
        let second = edge_at(
            "producer",
            "http|POST|/x",
            "consumer",
            "http|POST|/x",
            "src/b.ts:9",
        );
        let dirs = directions(&[
            incompatible_outcome(&first, ManifestTypeKind::Request, "request mismatch"),
            compatible_outcome(&first, ManifestTypeKind::Response),
            compatible_outcome(&second, ManifestTypeKind::Request),
            unverifiable_outcome(
                &second,
                ManifestTypeKind::Response,
                "producer side carries `any`",
            ),
        ]);
        attach_compat_verdicts(&mut payloads, &[first, second], &dirs);

        let row = payloads[0].compat_verdicts.clone().unwrap().remove(0);
        assert_eq!(row.sites.len(), 2);
        let (request, response) = fold_of_sites(&row.sites);
        assert_eq!(
            row.request, request,
            "the stored request direction is the fold of the sites' request halves"
        );
        assert_eq!(
            row.response, response,
            "the stored response direction is the fold of the sites' response halves"
        );
        // Stated outright as well, so the pin cannot pass by both sides being
        // empty or by both being computed the same wrong way.
        assert_eq!(
            row.request.as_ref().unwrap().verdict,
            crate::operation::TypeVerdict::Incompatible
        );
        assert_eq!(
            row.response.as_ref().unwrap().verdict,
            crate::operation::TypeVerdict::Unverifiable
        );
    }

    /// The wire spelling, and the two things a blob written before the field
    /// existed must still do (carrick#1385).
    #[test]
    fn sites_ride_the_wire_and_an_older_row_reads_without_them() {
        let mut payloads = vec![empty_repo("org/consumer", Some("consumer"))];
        let e = edge_at(
            "producer",
            "http|GET|/x",
            "consumer",
            "http|GET|/x",
            "src/a.ts:3",
        );
        let dirs = directions(&[compatible_outcome(&e, ManifestTypeKind::Response)]);
        attach_compat_verdicts(&mut payloads, &[e], &dirs);

        let json = serde_json::to_value(&payloads[0].compat_verdicts).unwrap();
        assert_eq!(
            json[0]["sites"],
            serde_json::json!([{
                "consumer_location": "src/a.ts:3",
                "response": { "verdict": "compatible", "resolved": true },
            }]),
            "the site list reaches the blob under `sites`, with the site's own \
             directions spelled exactly as the pair-level ones are"
        );

        // A row the field never touched: absent from the wire, not `null`, so
        // a row with nothing to list is byte-identical to a pre-#1385 one.
        let empty = CompatVerdict {
            producer_repo: "p".to_string(),
            producer_key: "http|GET|/x".to_string(),
            consumer_repo: "c".to_string(),
            consumer_key: "http|GET|/x".to_string(),
            request: None,
            response: None,
            sites: Vec::new(),
            scanner_version: "0.0.0-test".to_string(),
        };
        let text = serde_json::to_string(&empty).unwrap();
        assert!(
            !text.contains("sites"),
            "an empty site list must be omitted, not serialized as []"
        );

        // And a stored row written before the field existed still reads, with
        // the pair-level directions it has always carried.
        let older: CompatVerdict = serde_json::from_str(
            r#"{"producer_repo":"p","producer_key":"http|GET|/x","consumer_repo":"c",
                "consumer_key":"http|GET|/x",
                "response":{"verdict":"compatible","resolved":true},
                "scanner_version":"0.3.48"}"#,
        )
        .expect("a blob written before `sites` existed still deserializes");
        assert!(older.sites.is_empty());
        assert_eq!(
            older.response.as_ref().unwrap().verdict,
            crate::operation::TypeVerdict::Compatible
        );
    }
}
