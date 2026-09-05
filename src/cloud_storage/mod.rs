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
pub use local_dir_storage::{CACHE_DIR_ENV, LocalDirStorage};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ManifestRole {
    Producer,
    Consumer,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
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

/// A persisted per-pair type-compatibility verdict, keyed by CANONICAL pair
/// identity (never display labels — that is the #324 fail-open trap). Emitted at
/// scan time from the cross-repo [`crate::analyzer::CrossRepoMatch`] edges this
/// repo's calls participate in as the consumer, so the cloud MCP
/// `check_compatibility` tool can surface the REAL type-compat verdict CI
/// already computes instead of a structural-matching-only answer.
///
/// Only edges the check actually EVALUATED are persisted (`type_compatible`
/// `Some(_)`), so `compatible` is never a fabricated `true`: a pair with no
/// stored verdict is "not compared", never "compatible". The four key fields are
/// the exact `OperationKey::canonical()` / `service_name ?? repo_name` strings
/// the cloud reconstructs from the same persisted blob it already reads, so the
/// join is byte-identical and drift-free.
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
    /// `true` = the type check found the request/response types compatible,
    /// `false` = incompatible. Only evaluated edges reach here, so this is
    /// never a fabricated `true`.
    pub compatible: bool,
    /// Populated iff `!compatible`: the human-readable mismatch reason the
    /// check emitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mismatch_reason: Option<String>,
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
    /// The verdict for the SDK→producer pair, carried verbatim from the
    /// `CrossRepoMatch` the SDK's own call formed. `None` = compat was not
    /// evaluated for that edge, never "compatible".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_compatible: Option<bool>,
    /// Populated iff `type_compatible == Some(false)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mismatch_reason: Option<String>,
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UploadOutcome {
    /// The cloud already held a row for this (repo, service) carrying both this
    /// commit hash and this scanner version, so it skipped re-indexing and
    /// nothing this run computed was stored. Only ever true for `AwsStorage`;
    /// the mock and local-dir backends always write, so they report `false`.
    pub already_current: bool,
}

#[async_trait]
pub trait CloudStorage {
    async fn upload_repo_data(&self, data: &CloudRepoData) -> Result<UploadOutcome, StorageError>;

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

/// Attach per-pair type-compat verdicts to each service payload, for the cross-repo
/// edges where that service is the CONSUMER. Reads the verdicts off the
/// `CrossRepoMatch` edges `get_results` produced (compat already overlaid), and
/// keys each by the canonical pair identity the cloud reconstructs (#351/#324).
///
/// Fail-closed by construction: only edges the check actually evaluated
/// (`type_compatible.is_some()`) become a `CompatVerdict`; an unevaluated or
/// unmatched pair simply has no stored verdict, which the cloud reads as "not
/// compared", never "compatible". Multiple call sites collapsing onto one
/// producer/consumer canonical pair are deduped with incompatible-wins, so a
/// real mismatch is never masked by a sibling call site that happened to agree.
pub fn attach_compat_verdicts(
    payloads: &mut [CloudRepoData],
    matches: &[crate::analyzer::CrossRepoMatch],
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
            let Some(compatible) = m.type_compatible else {
                // The check did not evaluate this edge — persist nothing so the
                // cloud falls back to structural-matching-only (fail closed).
                continue;
            };
            let pair = (
                m.producer_repo.clone(),
                m.producer_key.clone(),
                m.consumer_repo.clone(),
                m.consumer_key.clone(),
            );
            let verdict = CompatVerdict {
                producer_repo: m.producer_repo.clone(),
                producer_key: m.producer_key.clone(),
                consumer_repo: m.consumer_repo.clone(),
                consumer_key: m.consumer_key.clone(),
                compatible,
                mismatch_reason: if compatible {
                    None
                } else {
                    m.mismatch_reason.clone()
                },
                scanner_version: scanner_version.to_string(),
            };
            by_pair
                .entry(pair)
                .and_modify(|existing| {
                    // Incompatible-wins: a mismatch on any call site to this pair
                    // is the verdict worth surfacing.
                    if existing.compatible && !verdict.compatible {
                        *existing = verdict.clone();
                    }
                })
                .or_insert(verdict);
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
        });

        let (endpoints, _calls) = mount_graph_to_api_details(&graph);
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].provenance, EndpointProvenance::Mock);

        // And it is on the serialized wire (the index blob).
        let json = serde_json::to_value(&endpoints[0]).unwrap();
        assert_eq!(json["provenance"], "mock");
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
            type_verdict: None,
            mismatch_reason: mismatch_reason.map(String::from),
            producer_provenance: Default::default(),
            relationship: carrick_match::MatchRelationship::ProducerConsumer,
        }
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

    /// A verdict round-trips through JSON keyed by canonical pair identity, and a
    /// `None` (unevaluated) edge is omitted, not serialized as compatible.
    #[test]
    fn compat_verdicts_round_trip_keyed_canonically() {
        let mut payloads = vec![empty_repo(
            "org/notification-service",
            Some("notification-service"),
        )];
        let matches = vec![
            // Evaluated + incompatible → persisted with reason.
            edge(
                "order-service",
                "http|GET|/orders/:id",
                "notification-service",
                "http|GET|/orders/:id",
                Some(false),
                Some("Order[] vs Order"),
            ),
            // Evaluated + compatible → persisted, no reason.
            edge(
                "order-service",
                "http|GET|/health",
                "notification-service",
                "http|GET|/health",
                Some(true),
                None,
            ),
            // Not evaluated → NOT persisted (fail closed).
            edge(
                "order-service",
                "http|POST|/orders",
                "notification-service",
                "http|POST|/orders",
                None,
                None,
            ),
        ];

        attach_compat_verdicts(&mut payloads, &matches);
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
        assert!(!incompat.compatible);
        assert_eq!(
            incompat.mismatch_reason.as_deref(),
            Some("Order[] vs Order")
        );
        assert_eq!(incompat.consumer_repo, "notification-service");
        assert_eq!(incompat.scanner_version, env!("CARGO_PKG_VERSION"));

        let compat = back_verdicts
            .iter()
            .find(|v| v.producer_key == "http|GET|/health")
            .unwrap();
        assert!(compat.compatible);
        assert!(compat.mismatch_reason.is_none());

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
        let matches = vec![edge(
            "order-service",
            "http|GET|/orders/:id",
            "notification-service",
            "http|GET|/orders/:id",
            Some(false),
            Some("Order[] vs Order"),
        )];
        attach_compat_verdicts(&mut payloads, &matches);
        assert!(payloads[0].compat_verdicts.is_none());
    }

    /// When two call sites hit the same producer/consumer canonical pair and
    /// disagree, the incompatible verdict wins (a real risk is never masked).
    #[test]
    fn attach_compat_verdicts_dedup_incompatible_wins() {
        let mut payloads = vec![empty_repo("org/consumer", Some("consumer"))];
        let matches = vec![
            edge(
                "producer",
                "http|GET|/x",
                "consumer",
                "http|GET|/x",
                Some(true),
                None,
            ),
            edge(
                "producer",
                "http|GET|/x",
                "consumer",
                "http|GET|/x",
                Some(false),
                Some("mismatch"),
            ),
        ];
        attach_compat_verdicts(&mut payloads, &matches);
        let verdicts = payloads[0].compat_verdicts.clone().unwrap();
        assert_eq!(verdicts.len(), 1);
        assert!(!verdicts[0].compatible);
        assert_eq!(verdicts[0].mismatch_reason.as_deref(), Some("mismatch"));
    }
}
