//! File-centric analyzer agent for high-speed static analysis using Gemini 3.0 Flash.
//!
//! NOTE: This module is part of a refactoring effort. The public API will be integrated
//! with the orchestrator in subsequent commits.
#![allow(dead_code)]
//!
//! This agent implements a "one-shot" analysis approach where an entire file is sent
//! to the LLM along with framework-specific patterns. The LLM returns structured
//! findings that can be directly deserialized into graph structures.
//!
//! Key features:
//! - Framework agnostic: All detection logic derives from injected patterns
//! - Alias resolution: Import sources are tracked for cross-file linking
//! - Flat schema: Avoids recursion errors and ensures deterministic parsing

use crate::{
    agent_service::AgentService,
    agents::{framework_guidance_agent::FrameworkGuidance, schemas::AgentSchemas},
    visitor::{ImportedSymbol, SymbolKind},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, trace, warn};

/// Result of analyzing a single mount relationship
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountResult {
    pub line_number: i32,
    pub parent_node: String,
    pub child_node: String,
    pub mount_path: String,
    pub import_source: Option<String>,
    pub pattern_matched: String,
}

/// How an endpoint handler emits its response payload. Classified by the
/// analyze-file model (the cloud prompt teaches the semantics; this enum and
/// the response schema define the wire values). Drives which inference kind
/// `collect_type_requests` asks the sidecar for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmissionStyle {
    /// The payload is the argument of a send call — `res.json(users)`,
    /// `reply.send(users)`, and also `return c.json(users)` /
    /// `return NextResponse.json(users)` (the payload is the argument,
    /// never the framework Response wrapper).
    ImperativeSend,
    /// The handler's return value IS the payload (Fastify return-style,
    /// `return users`). The response contract is the handler's return type.
    ReturnValue,
    /// No recoverable payload expression: zero-arg sends (`res.json()`),
    /// streams/buffers handed to send calls, or payloads written by helpers
    /// invisible at the call site (`renderUsers(res)`).
    NoPayload,
}

impl EmissionStyle {
    /// Parse a model-emitted style string leniently: case-insensitive, with
    /// `_`/space separators tolerated. Returns `None` for anything off-enum —
    /// one junk value must not fail deserialization of the whole file (every
    /// other endpoint field is similarly absorbed rather than rejected).
    fn parse_lenient(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        match normalized.as_str() {
            "imperative-send" => Some(EmissionStyle::ImperativeSend),
            "return-value" => Some(EmissionStyle::ReturnValue),
            "no-payload" => Some(EmissionStyle::NoPayload),
            _ => None,
        }
    }
}

fn deserialize_emission_style<'de, D>(deserializer: D) -> Result<Option<EmissionStyle>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Deserialize as a raw JSON value, not Option<String>: a non-string value
    // (number, bool, object) from the model must degrade to None like any
    // other junk, not fail the whole file's parse.
    let raw: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    Ok(raw
        .as_ref()
        .and_then(serde_json::Value::as_str)
        .and_then(EmissionStyle::parse_lenient))
}

fn deserialize_call_kind<'de, D>(
    deserializer: D,
) -> Result<Option<crate::operation::CallKind>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Absorb off-enum / non-string call_kind values to None instead of failing
    // the whole file's parse (mirrors emission_style; supports fail-closed).
    let raw: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    Ok(raw
        .as_ref()
        .and_then(serde_json::Value::as_str)
        .and_then(crate::operation::CallKind::parse_lenient))
}

fn deserialize_pubsub_role<'de, D>(
    deserializer: D,
) -> Result<Option<crate::operation::PubsubRole>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Absorb off-enum / non-string role values to None instead of failing the
    // whole file's parse (mirrors call_kind). A role of None means the op can't
    // be placed on either side, so engine ingestion drops it with a debug log.
    let raw: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    Ok(raw
        .as_ref()
        .and_then(serde_json::Value::as_str)
        .and_then(crate::operation::PubsubRole::parse_lenient))
}

/// Which layer stated a row, and — for a deterministic row — which pass.
///
/// Every HTTP candidate is resolved before the model is called, and the row
/// the deterministic layer emits for it is the substrate the model's answer is
/// joined onto (see `FileOrchestrator::emit_resolved_rows` and
/// `FileOrchestrator::join_model_rows`). This field records who stated the
/// row, for the cache, the scan log and the tests.
///
/// It also rides the wire (carrick#660): the mount-graph row and the uploaded
/// operation both carry it, so a reader of the index can tell a row the source
/// states outright from one that is the model's own reading. Absence there
/// means the scan did not state it, never that the row is the model's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionSource {
    /// A verb-named request spec: `client.post({ url: "/v1/things" })` (#529).
    RequestSpec,
    /// A request wrapper declared in the same file, called with the path as an
    /// argument (carrick#588).
    SameFileWrapper,
    /// A request member of an imported same-repo module (carrick#588/#623).
    ImportedMember,
    /// A binding holding the WHOLE request URL, read from an environment
    /// variable (carrick#572/#632).
    WholeUrlEnv,
    /// The path a `new URL(path, base)` states for the call's target
    /// (carrick#610).
    NewUrl,
    /// A route declared by file location (Next.js app router, etc.).
    FileBasedRoute,
    /// A route declared as data: `{ method, path, handler }` (#234).
    DescriptorRoute,
    /// A route table binding a literal path to an imported controller (#580).
    ClassController,
    /// The compiler's answer for what the RECEIVER of a bare
    /// `x.verb("/lit", arg)` site IS (carrick#695): an instance of a detected
    /// server framework registers a route, an instance of a detected data
    /// fetcher issues a request. The path and verb are the site's own
    /// literals; only the ROLE comes from the type.
    ReceiverType,
    /// The candidate's own route-shaped first-argument literal. The one source
    /// that states no row of its own: a bare `x.verb("/lit", arg)` carries no
    /// role, so whether it registers a route or requests one is the model's to
    /// decide. It overrules the path of the row the model does return (#332).
    InlineLiteral,
    /// The row is the model's, with no deterministic twin at its span.
    Model,
}

/// Result of analyzing a single endpoint definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointResult {
    pub candidate_id: String,
    pub line_number: i32,
    pub owner_node: String,
    pub method: String,
    pub path: String,
    pub handler_name: String,
    pub pattern_matched: String,
    /// Start byte offset of the endpoint definition call expression (from SWC via apply_candidate_map)
    #[serde(default)]
    pub call_expression_span_start: Option<u32>,
    /// End byte offset of the endpoint definition call expression (from SWC via apply_candidate_map)
    #[serde(default)]
    pub call_expression_span_end: Option<u32>,
    /// Verbatim code text of the request payload expression (from Gemini)
    pub payload_expression_text: Option<String>,
    /// Line number where the payload expression starts (from Gemini)
    pub payload_expression_line: Option<i32>,
    /// Verbatim code text of the response payload subexpression (from Gemini).
    /// This is the value whose type we want — e.g., `users` in `res.json(users)`,
    /// `ctx.body = users`, `h.response(users)`, `return users`, `reply.send(users)`,
    /// `c.json(users)`. Framework-agnostic; set to null for payload-less handlers
    /// (redirects, 204s, streaming).
    pub response_expression_text: Option<String>,
    /// Line number where the payload expression starts (from Gemini)
    pub response_expression_line: Option<i32>,
    /// How the handler emits its response payload (from Gemini). `None` when
    /// the field is missing or carries an off-enum string (the lenient
    /// deserializer absorbs model junk instead of failing the whole file);
    /// treated as `ImperativeSend` downstream.
    #[serde(default, deserialize_with = "deserialize_emission_style")]
    pub emission_style: Option<EmissionStyle>,
    /// The primary type symbol name without wrappers (e.g., "User" from "Response<User[]>")
    pub primary_type_symbol: Option<String>,
    /// Import path where the type is defined (e.g., "./types/user"), null if inline or same file
    pub type_import_source: Option<String>,
    /// Which layer stated this row. Never from the model — set by the
    /// emit/join pass. See [`ResolutionSource`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_source: Option<ResolutionSource>,
    /// Whether the module serving this route ALSO renders a view: it exports a
    /// handler (which is why the route exists) and a default export (which is
    /// the component). A fact read off the module's export list, never from
    /// the model, and set only by the file-based route pass — every other
    /// emitter leaves it `false`.
    ///
    /// It exists because route-ness is the exported handler and not the file
    /// extension (carrick#704): once a page module's read handler is
    /// recognised as the GET it is, a reader needs to tell a page's own data
    /// endpoint from an API surface a sibling service is expected to call.
    /// `skip_deserializing` is what makes "never from the model" true rather
    /// than merely intended: `file_results` holds the model's raw answers, so
    /// this field is on a shape the model's JSON is parsed into, and a
    /// hallucinated key would otherwise be believed on a model-only row. The
    /// value is recomputed from the module's exports on every scan, cold or
    /// incremental, so nothing needs to round-trip. Skipped when false on the
    /// way out for the same reason `resolution_source` is: `file_results` is
    /// the bulk of the blob and the first thing the size limit drops.
    #[serde(skip_deserializing, skip_serializing_if = "std::ops::Not::not")]
    pub view_module: bool,
}

/// Result of analyzing a single data-fetching call
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataCallResult {
    pub candidate_id: String,
    pub line_number: i32,
    pub target: String,
    pub method: Option<String>,
    /// LLM classification of the call target (internal_http / external_http /
    /// sdk / unresolved). `None` when the model omitted it or emitted an off-enum
    /// value (lenient, like emission_style); downstream gating treats that as
    /// unclassified. See `crate::operation::CallKind`.
    #[serde(default, deserialize_with = "deserialize_call_kind")]
    pub call_kind: Option<crate::operation::CallKind>,
    pub pattern_matched: String,
    /// Start byte offset of the data call expression (from SWC via apply_candidate_map)
    #[serde(default)]
    pub call_expression_span_start: Option<u32>,
    /// End byte offset of the data call expression (from SWC via apply_candidate_map)
    #[serde(default)]
    pub call_expression_span_end: Option<u32>,
    /// Verbatim code text of the call expression itself (from Gemini)
    #[serde(default)]
    pub call_expression_text: Option<String>,
    /// Line number where the call expression starts (from Gemini)
    #[serde(default)]
    pub call_expression_line: Option<i32>,
    /// Verbatim code text of the request payload expression (from Gemini)
    pub payload_expression_text: Option<String>,
    /// Line number where the payload expression starts (from Gemini)
    pub payload_expression_line: Option<i32>,
    /// The primary type symbol name without wrappers (e.g., "User" from "Promise<User>")
    pub primary_type_symbol: Option<String>,
    /// Import path where the type is defined (e.g., "./types/user"), null if inline or same file
    pub type_import_source: Option<String>,
    /// The concrete URL the SOURCE states this request falls back to when the
    /// environment supplies nothing, for a target whose whole origin is an
    /// environment variable with a LOOPBACK fallback
    /// (`process.env.SUPPORT_URL ?? "http://localhost:3939/api/ask"`).
    ///
    /// Never from the model — set by `merge_whole_url_env_calls` from the
    /// binding's own AST, alongside `target`. `target` states the deployed
    /// request (the env var supplies the origin) and is what the row displays;
    /// this states the default, and it is what the canonical key is computed
    /// from, because a loopback origin is a structural prefix to strip rather
    /// than an opaque base to keep verbatim. See
    /// [`crate::env_alias::whole_url_local_default`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loopback_default_url: Option<String>,
    /// How this call's BASE resolves (carrick#649): the expression it was
    /// written against, whether that expression reads the environment, and the
    /// default the source states for it.
    ///
    /// Never from the model — stamped by `stamp_call_bases` from the file's own
    /// AST once every base-resolution pass has run, so it reads the SAME target
    /// the row persists. Absent when the target's base is not one the scanner
    /// reads (a literal absolute origin, an unreadable base slot). See
    /// [`crate::call_base::CallBaseResolution`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<crate::call_base::CallBaseResolution>,
    /// What the member join could not follow for the client method this row
    /// reaches (carrick#656): the method's name, and how many call sites named
    /// it in this service without resolving to it.
    ///
    /// Never from the model — stamped by `stamp_unfollowed_member_sites` once
    /// every file's join outcome is in, on the rows the join produced for that
    /// member and on the member's own request row. Absent whenever the count is
    /// zero, which is also what a scan that saw no such site records: absence
    /// says nothing was counted, never that the consumer list is complete.
    ///
    /// Cached with the rest of the row, so on an incremental scan it states
    /// what the last scan to re-analyse these files counted. That lags the
    /// source in both directions — an uncounted new site, or a fixed one still
    /// counted — until those files change or `CACHE_VERSION` moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumers_not_resolved: Option<crate::imported_request_member::UnfollowedMemberSites>,
    /// Which layer stated this row. Never from the model — set by the
    /// emit/join pass. See [`ResolutionSource`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_source: Option<ResolutionSource>,
}

/// A GraphQL resolver the file-analyzer found: the schema field it answers and
/// the resolver function's declared return type (resolved downstream into the
/// producer-side response contract). Mirrors the `graphql_operations` array in
/// `AgentSchemas::file_analysis_schema`. The `kind` wire values (query /
/// mutation / subscription) come from `crate::operation::GraphqlOperationKind`,
/// the single source of truth shared with the operation graph; the schema enum
/// is kept in lockstep by `graphql_operations_schema_enum_matches_serde_wire_values`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphqlOperation {
    /// The GraphQL root operation this resolver implements.
    pub kind: crate::operation::GraphqlOperationKind,
    /// The schema field name this resolver answers (e.g., "order").
    pub field: String,
    /// Name of the resolver function (e.g., "resolveOrder"). `None` only when no
    /// function in this file resolves the field (the #248 type-locate fallback,
    /// which carries `backing_type_symbol` instead).
    #[serde(default)]
    pub resolver_function: Option<String>,
    /// Line number where the resolver function is defined. `None` whenever
    /// `resolver_function` is `None`.
    #[serde(default)]
    pub resolver_line: Option<i32>,
    /// The primary return type symbol without wrappers (e.g. "ApiResponse" from
    /// "Promise<ApiResponse>"). `None` for untyped or inline-object resolver
    /// returns. Describes the RESOLVER's return type only — the resolver-less
    /// fallback uses `backing_type_symbol`.
    pub primary_type_symbol: Option<String>,
    /// Import path where `primary_type_symbol` is defined (e.g., "./types/order"),
    /// null if inline or same file. Null whenever `primary_type_symbol` is null.
    pub type_import_source: Option<String>,
    /// #248 type-locate fallback: the co-located TS type describing a
    /// resolver-less field's response shape (e.g. "Order" for `orders: [Order!]!`
    /// with no `resolveOrders`). `None` whenever a resolver was linked — kept
    /// separate from `primary_type_symbol` so the resolver-linking path stays
    /// undisturbed.
    #[serde(default)]
    pub backing_type_symbol: Option<String>,
    /// Import path where `backing_type_symbol` is defined, or `None` if declared
    /// in this file. `None` whenever `backing_type_symbol` is `None`.
    #[serde(default)]
    pub backing_type_source: Option<String>,
}

/// A co-located consumer result type the file-analyzer located for a GraphQL
/// document with no explicit call-site generic (#268 — the consumer mirror of
/// the producer `backing_type_symbol` fallback, #248). Mirrors the
/// `graphql_consumer_locates` array in `AgentSchemas::file_analysis_schema`.
///
/// Kept as an entirely separate top-level array from `graphql_operations`
/// (producer-only) rather than overloading a shared field — the 186cb27
/// lesson: cramming two purposes onto one field made the model null out an
/// unrelated signal (a resolver-backed op's `resolver_function`) as a side
/// effect of teaching the resolver-less case. Isolation here is structural:
/// there is no field this array could collide with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphqlConsumerLocate {
    /// The GraphQL operation kind of the document being located.
    pub kind: crate::operation::GraphqlOperationKind,
    /// The document's top-level field name (e.g., "order", "orderUpdated") —
    /// matches the consumer op this entry locates a type for.
    pub field: String,
    /// The co-located TS type bound to the document's RESULT shape (e.g.
    /// "OrderUpdate"). Never a request/variables type.
    pub result_type_symbol: String,
    /// Import path where `result_type_symbol` is defined (e.g.,
    /// "./types/order"), or `None` if declared in this file.
    pub result_type_source: Option<String>,
}

/// A pub/sub operation the file-analyzer found: the topic it targets and which
/// side (subscriber = producer, publisher = consumer) the code sits on. Mirrors
/// the `pubsub_operations` array in `AgentSchemas::file_analysis_schema`. The
/// `role` wire values (subscriber / publisher) come from
/// `crate::operation::PubsubRole`, the single source of truth; the schema enum
/// is kept in lockstep by `pubsub_operations_schema_enum_matches_serde_wire_values`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PubsubOperation {
    /// The exact topic/channel name (literal string).
    pub topic: String,
    /// Which side of the topic this op sits on. `None` when the model omitted it
    /// or emitted an off-enum value (lenient, like call_kind); such an op can't
    /// be placed on either side and is dropped during engine ingestion.
    #[serde(default, deserialize_with = "deserialize_pubsub_role")]
    pub role: Option<crate::operation::PubsubRole>,
    /// Line number where the operation appears.
    pub line_number: i32,
    /// The primary payload type symbol without wrappers (e.g., "PageViewEvent").
    /// `None` for untyped or inline-object payloads.
    #[serde(default)]
    pub primary_type_symbol: Option<String>,
    /// Import path where the type is defined, null if inline or same file. Null
    /// whenever `primary_type_symbol` is null.
    #[serde(default)]
    pub type_import_source: Option<String>,
    /// Diagnostic only: the pub/sub library/transport if evident (e.g., "redis").
    /// Not part of the operation's identity.
    #[serde(default)]
    pub broker: Option<String>,
    /// Verbatim text of the value holding the decoded payload at this site: a
    /// publisher's payload expression, or a subscriber handler's payload
    /// parameter/binding. The locator for the sidecar's location-based
    /// inference when `primary_type_symbol` is null (wrapper patterns whose
    /// payload type is carried by a generic binding, never a bare identifier).
    #[serde(default)]
    pub payload_expression_text: Option<String>,
    /// Line where the payload expression starts. Null whenever
    /// `payload_expression_text` is null.
    #[serde(default)]
    pub payload_expression_line: Option<i32>,
}

/// Complete analysis result for a single file
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileAnalysisResult {
    pub mounts: Vec<MountResult>,
    pub endpoints: Vec<EndpointResult>,
    pub data_calls: Vec<DataCallResult>,
    /// GraphQL resolvers found in the file. Optional on the wire (a model may
    /// omit it for non-GraphQL files), so default to empty rather than failing
    /// the whole file's parse.
    #[serde(default)]
    pub graphql_operations: Vec<GraphqlOperation>,
    /// Pub/sub operations found in the file. Optional on the wire (a model may
    /// omit it for files with no pub/sub), so default to empty rather than
    /// failing the whole file's parse.
    #[serde(default)]
    pub pubsub_operations: Vec<PubsubOperation>,
    /// Co-located consumer result types the file-analyzer located for GraphQL
    /// documents with no explicit call-site generic (#268). Optional on the
    /// wire (a model may omit it for files with no such documents), so default
    /// to empty rather than failing the whole file's parse.
    #[serde(default)]
    pub graphql_consumer_locates: Vec<GraphqlConsumerLocate>,
}

/// Agent that performs file-centric analysis using framework-agnostic patterns.
///
/// This agent sends the full content of a file to the LLM along with patterns
/// derived from framework guidance. The LLM acts purely as a pattern matcher
/// and alias resolver.
pub struct FileAnalyzerAgent {
    agent_service: AgentService,
}

impl FileAnalyzerAgent {
    pub fn new(agent_service: AgentService) -> Self {
        Self { agent_service }
    }

    /// Analyze a single file with the given framework patterns.
    ///
    /// # Arguments
    /// * `file_path` - Path to the file being analyzed (for context)
    /// * `file_content` - Full content of the file
    /// * `guidance` - Framework-specific patterns to use for matching
    ///
    /// # Returns
    /// A `FileAnalysisResult` containing all detected mounts, endpoints, and data calls.
    /// Analyze a single file with the given framework patterns (legacy method without candidates).
    ///
    /// This method is kept for backward compatibility. Prefer `analyze_file_with_candidates`
    /// for the AST-gated architecture.
    pub async fn analyze_file(
        &self,
        file_path: &str,
        file_content: &str,
        guidance: &FrameworkGuidance,
    ) -> Result<FileAnalysisResult, Box<dyn std::error::Error>> {
        // Delegate to the new method with empty candidates
        self.analyze_file_with_candidates(
            file_path,
            file_content,
            guidance,
            &[],
            &[],
            &HashMap::new(),
            &[],
            &[],
            &[],
        )
        .await
    }

    /// Analyze a single file with AST-detected candidate targets.
    ///
    /// This is the primary method for the AST-Gated architecture:
    /// 1. SWC Scanner has already found candidate lines
    /// 2. We send Full File + Patterns + Candidate Hints to the LLM
    /// 3. The LLM uses candidates as "Focused Targets" to ensure 100% recall
    ///
    /// # Arguments
    /// * `file_path` - Path to the file being analyzed (for context)
    /// * `file_content` - Full content of the file
    /// * `guidance` - Framework-specific patterns to use for matching
    /// * `candidate_hints` - AST-detected candidate lines (formatted hints from SWC Scanner)
    /// * `candidate_contexts` - Structured candidate details (JSON strings)
    ///
    /// # Returns
    /// A `FileAnalysisResult` containing all detected mounts, endpoints, and data calls.
    /// Eval-harness diagnostics (off in normal runs). When `CARRICK_EVAL_DUMP_DIR`
    /// is set, write the file-analyzer's input (`request_user_message` — which
    /// embeds the framework guidance + candidates the model received) and its
    /// `raw_response` to `<dir>/<file>.attemptN.json`. This makes prompt-hardening
    /// evidence-driven: we read what the model actually emitted (owner_node,
    /// mount_path, whether the endpoint was extracted at all) rather than guess.
    /// `retry_reason` is `None` on attempt 1 and names the trigger on attempt 2
    /// (currently always `"suspicious_fields"`), so attempt pairs are
    /// distinguishable in the dumps and harness runs can measure how often the
    /// retry fires and what it changes.
    fn dump_eval_artifact(
        file_path: &str,
        attempt: u8,
        user_message: &str,
        response: &str,
        retry_reason: Option<&str>,
    ) {
        let Ok(dir) = std::env::var("CARRICK_EVAL_DUMP_DIR") else {
            return;
        };
        let dir = std::path::Path::new(&dir);
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let stem = file_path.replace(['/', '\\'], "_");
        let payload = serde_json::json!({
            "file_path": file_path,
            "attempt": attempt,
            "retry_reason": retry_reason,
            "request_user_message": user_message,
            "raw_response": response,
        });
        if let Ok(s) = serde_json::to_string_pretty(&payload) {
            let _ = std::fs::write(dir.join(format!("{stem}.attempt{attempt}.json")), s);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn analyze_file_with_candidates(
        &self,
        file_path: &str,
        file_content: &str,
        guidance: &FrameworkGuidance,
        candidate_hints: &[String],
        candidate_contexts: &[String],
        imported_symbols: &HashMap<String, ImportedSymbol>,
        graphql_producer_hints: &[String],
        graphql_consumer_hints: &[String],
        wrapper_context: &[String],
    ) -> Result<FileAnalysisResult, Box<dyn std::error::Error>> {
        // Skip empty files
        if file_content.trim().is_empty() {
            return Ok(FileAnalysisResult::default());
        }

        let user_message = self.build_user_message_with_candidates(
            file_path,
            file_content,
            guidance,
            candidate_hints,
            candidate_contexts,
            imported_symbols,
            graphql_producer_hints,
            graphql_consumer_hints,
            wrapper_context,
        );

        debug!("=== FILE ANALYZER AGENT (AST-GATED) ===");
        debug!("Analyzing file: {}", file_path);
        debug!(
            "File size: {} chars, {} lines",
            file_content.len(),
            file_content.lines().count()
        );
        debug!("Candidate targets: {}", candidate_hints.len());

        let schema = AgentSchemas::file_analysis_schema();
        let response = self
            .agent_service
            .analyze_with_lambda("/analyze-file", &user_message, Some(schema.clone()))
            .await?;

        // Raw bodies at trace only — debug logs are persisted and uploaded,
        // and the response quotes source snippets from the scanned repo (#61).
        trace!("=== RAW FILE ANALYSIS RESPONSE ===");
        trace!("{}", response);
        trace!("=== END RAW RESPONSE ===");
        debug!("File analysis response: {} chars", response.len());

        // Eval diagnostics: when CARRICK_EVAL_DUMP_DIR is set (the eval harness's
        // capture mode), persist the analyzer's input (the guidance/candidates it
        // received) and raw output so prompt-hardening can be driven by what the
        // model actually emitted, not by guesswork. Off unless the env is set.
        Self::dump_eval_artifact(file_path, 1, &user_message, &response, None);

        let mut result: FileAnalysisResult = serde_json::from_str(&response).map_err(|e| {
            format!(
                "Failed to parse file analysis response: {}. Raw response: {}",
                e, response
            )
        })?;

        // Sanitize LLM response: Gemini sometimes returns "+null" as a string instead of null
        let needs_retry = Self::sanitize_result(&mut result);
        let initial_result = result.clone();
        if needs_retry {
            warn!("[FileAnalyzerAgent] Suspicious fields detected in LLM output; retrying once");
            let response = self
                .agent_service
                .analyze_with_lambda("/analyze-file", &user_message, Some(schema))
                .await?;

            trace!("=== RAW FILE ANALYSIS RESPONSE ===");
            trace!("{}", response);
            trace!("=== END RAW RESPONSE ===");
            debug!("File analysis retry response: {} chars", response.len());
            Self::dump_eval_artifact(
                file_path,
                2,
                &user_message,
                &response,
                Some("suspicious_fields"),
            );

            let mut retry_result: FileAnalysisResult =
                serde_json::from_str(&response).map_err(|e| {
                    format!(
                        "Failed to parse file analysis response: {}. Raw response: {}",
                        e, response
                    )
                })?;

            Self::sanitize_result(&mut retry_result);
            let chosen = Self::choose_best_result(initial_result, retry_result);

            debug!(
                "File analysis complete: {} mounts, {} endpoints, {} data_calls",
                chosen.mounts.len(),
                chosen.endpoints.len(),
                chosen.data_calls.len()
            );

            return Ok(chosen);
        }

        debug!(
            "File analysis complete: {} mounts, {} endpoints, {} data_calls",
            result.mounts.len(),
            result.endpoints.len(),
            result.data_calls.len()
        );

        Ok(result)
    }

    /// Count every top-level result section, so the best-of selection sees a
    /// difference in ANY of them. The previous version only counted the three
    /// HTTP sections, so a retry that lost graphql/pubsub/consumer-locate
    /// findings could still tie on the HTTP counts and be selected, silently
    /// dropping those findings.
    fn result_score(result: &FileAnalysisResult) -> usize {
        result.mounts.len()
            + result.endpoints.len()
            + result.data_calls.len()
            + result.graphql_operations.len()
            + result.pubsub_operations.len()
            + result.graphql_consumer_locates.len()
    }

    fn choose_best_result(
        initial: FileAnalysisResult,
        retry: FileAnalysisResult,
    ) -> FileAnalysisResult {
        let initial_score = Self::result_score(&initial);
        let retry_score = Self::result_score(&retry);

        if retry_score >= initial_score {
            retry
        } else {
            warn!("[FileAnalyzerAgent] Retry produced fewer findings; keeping original result");
            initial
        }
    }

    /// Sanitize the LLM response to fix common issues like "+null" strings
    fn sanitize_result(result: &mut FileAnalysisResult) -> bool {
        // Helper to check if a string represents null. Covers the placeholder /
        // null-like literals the model intermittently emits ("null", "+null",
        // "-null", "NULL", "undefined", "-", "").
        fn is_null_string(s: &str) -> bool {
            s == "+null"
                || s == "-null"
                || s == "null"
                || s == "NULL"
                || s == "undefined"
                || s == "-"
                || s.is_empty()
        }

        fn normalize_optional_string(value: &mut Option<String>) {
            let Some(current) = value.as_deref() else {
                return;
            };
            let trimmed = current.trim();
            if is_null_string(trimmed) {
                *value = None;
            } else if trimmed != current {
                *value = Some(trimmed.to_string());
            }
        }

        /// Strip balanced ENCLOSING parenthesis pairs from a locator string:
        /// `(decision)` → `decision`, `(({ payload }))` → `{ payload }`.
        /// Only strips when the leading `(` matches the trailing `)` around the
        /// WHOLE string — `(a).b(c)` is untouched (its first paren closes
        /// mid-string). Pure syntax normalization for the model's habit of
        /// copying an arrow-function parameter with its parameter-list parens.
        fn strip_enclosing_parens(text: &str) -> &str {
            let mut current = text.trim();
            loop {
                let inner = current
                    .strip_prefix('(')
                    .and_then(|rest| rest.strip_suffix(')'));
                let Some(inner) = inner else {
                    return current;
                };
                // The stripped pair must enclose the whole string: scan the
                // candidate inner text and reject if depth ever goes negative
                // (that means the leading paren closed before the end).
                let mut depth = 0i32;
                let balanced = inner.chars().all(|ch| {
                    match ch {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    depth >= 0
                }) && depth == 0;
                if !balanced {
                    return current;
                }
                current = inner.trim();
            }
        }

        fn is_suspicious_import_source(value: &str) -> bool {
            if value.contains("primary_type_symbol") || value.contains("type_import_source") {
                return true;
            }

            value.chars().any(|ch| {
                ch.is_whitespace()
                    || ch == '"'
                    || ch == '\''
                    || ch == '`'
                    || ch == '{'
                    || ch == '}'
                    || ch == '('
                    || ch == ')'
                    || ch == ':'
                    || ch == ','
                    || ch == '['
                    || ch == ']'
            })
        }

        fn is_invalid_relative_source(value: &str) -> bool {
            value.starts_with('.')
                && !value.starts_with("./")
                && !value.starts_with("../")
                && !value.starts_with(".\\")
                && !value.starts_with("..\\")
        }

        fn is_valid_identifier(value: &str) -> bool {
            let mut chars = value.chars();
            let Some(first) = chars.next() else {
                return false;
            };
            if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
                return false;
            }
            chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
        }

        fn normalize_import_source(value: &mut Option<String>) -> bool {
            let mut suspicious = false;
            normalize_optional_string(value);
            if let Some(source) = value.as_deref() {
                if source.starts_with("node:") {
                    *value = None;
                } else if is_suspicious_import_source(source) || is_invalid_relative_source(source)
                {
                    *value = None;
                    suspicious = true;
                }
            }
            suspicious
        }

        let mut needs_retry = false;

        // Sanitize endpoints
        for endpoint in &mut result.endpoints {
            // Pairing invariant: no-payload means "no recoverable payload
            // expression". When the model contradicts itself and ships a
            // usable response expression anyway, keep the recoverable
            // contract — downgrade to imperative-send instead of discarding
            // the expression on the strength of a contradicted claim.
            if endpoint.emission_style == Some(EmissionStyle::NoPayload)
                && endpoint
                    .response_expression_text
                    .as_deref()
                    .is_some_and(|text| !is_null_string(text.trim()))
            {
                debug!(
                    "Endpoint {} {} claims no-payload but carries response expression {:?}; \
                     treating as imperative-send",
                    endpoint.method, endpoint.path, endpoint.response_expression_text
                );
                endpoint.emission_style = Some(EmissionStyle::ImperativeSend);
            }
            normalize_optional_string(&mut endpoint.primary_type_symbol);
            if normalize_import_source(&mut endpoint.type_import_source) {
                needs_retry = true;
            }
            if let Some(ref symbol) = endpoint.primary_type_symbol
                && !is_valid_identifier(symbol)
            {
                endpoint.primary_type_symbol = None;
            }
        }

        // Sanitize data calls
        for data_call in &mut result.data_calls {
            // Fix method field
            if let Some(ref mut s) = data_call.method {
                let trimmed = s.trim();
                if is_null_string(trimmed) {
                    data_call.method = None;
                } else if trimmed != s.as_str() {
                    *s = trimmed.to_string();
                }
            }
            normalize_optional_string(&mut data_call.primary_type_symbol);
            if normalize_import_source(&mut data_call.type_import_source) {
                needs_retry = true;
            }
            if let Some(ref symbol) = data_call.primary_type_symbol
                && !is_valid_identifier(symbol)
            {
                data_call.primary_type_symbol = None;
            }
        }

        // Sanitize pub/sub operations (#283): trim/normalize the topic, payload
        // type symbol, and its import source the same way data_calls are handled.
        result.pubsub_operations.retain_mut(|op| {
            let trimmed_topic = op.topic.trim();
            // Drop the op when the topic is a null-like placeholder the model
            // sometimes emits ("null", "+null", "-null", "undefined", "", "-"):
            // a topicless pub/sub op has no identity and can't be placed on
            // either side, so it must not survive into the engine.
            if is_null_string(trimmed_topic) {
                return false;
            }
            if trimmed_topic != op.topic.as_str() {
                op.topic = trimmed_topic.to_string();
            }
            normalize_optional_string(&mut op.primary_type_symbol);
            if normalize_import_source(&mut op.type_import_source) {
                needs_retry = true;
            }
            if let Some(ref symbol) = op.primary_type_symbol
                && !is_valid_identifier(symbol)
            {
                op.primary_type_symbol = None;
            }
            normalize_optional_string(&mut op.broker);
            // Payload locator hygiene: a blank/null-like text is no locator,
            // and a non-positive line is not a valid 1-based anchor. The line
            // is a PRECISION aid, not a prerequisite — the sidecar's text
            // search runs file-wide without one — so a valid text with a
            // missing/invalid line is kept, and the infer collector
            // (`collect_pubsub_infer_requests`) anchors the search to the
            // operation's own line instead (the payload expression starts at
            // the publish call; the handler starts at the subscribe call). A
            // line WITHOUT a text is meaningless, so that direction nulls.
            normalize_optional_string(&mut op.payload_expression_text);
            // The model frequently copies a subscriber handler's payload
            // parameter WITH its arrow-parameter-list parens (`(decision)`,
            // `({ time, run })`) — measured 12/20 on the wrapper-relay harness
            // fixture. Enclosing parens are pure syntax, never part of the
            // binding or expression the sidecar must locate, so strip balanced
            // outer pairs deterministically here rather than fighting the
            // model's copy granularity in the schema description.
            if let Some(text) = op.payload_expression_text.take() {
                let stripped = strip_enclosing_parens(&text);
                op.payload_expression_text = if stripped.is_empty() {
                    None
                } else {
                    Some(stripped.to_string())
                };
            }
            if op.payload_expression_line.is_some_and(|line| line <= 0) {
                op.payload_expression_line = None;
            }
            if op.payload_expression_text.is_none() {
                op.payload_expression_line = None;
            }
            true
        });

        needs_retry
    }

    // The system prompt for the Carrick Static Analysis Engine lives in
    // the private carrick-cloud file-analyzer lambda
    // (lambdas/file-analyzer/system_prompt.txt). This orchestrator only
    // builds the structured user_message and POSTs to /analyze-file via
    // AgentService::analyze_with_lambda.

    /// Build the dynamic user message with patterns and file content (legacy).
    #[allow(dead_code)]
    fn build_user_message(
        &self,
        file_path: &str,
        file_content: &str,
        guidance: &FrameworkGuidance,
    ) -> String {
        self.build_user_message_with_candidates(
            file_path,
            file_content,
            guidance,
            &[],
            &[],
            &HashMap::new(),
            &[],
            &[],
            &[],
        )
    }

    /// Build the dynamic user message with patterns, file content, and candidate targets.
    #[allow(clippy::too_many_arguments)]
    fn build_user_message_with_candidates(
        &self,
        file_path: &str,
        file_content: &str,
        guidance: &FrameworkGuidance,
        candidate_hints: &[String],
        candidate_contexts: &[String],
        imported_symbols: &HashMap<String, ImportedSymbol>,
        graphql_producer_hints: &[String],
        graphql_consumer_hints: &[String],
        wrapper_context: &[String],
    ) -> String {
        let mount_patterns = self.format_patterns(&guidance.mount_patterns);
        let endpoint_patterns = self.format_patterns(&guidance.endpoint_patterns);
        let data_patterns = self.format_patterns(&guidance.data_fetching_patterns);

        // Format candidate targets section
        let candidates_section = if candidate_hints.is_empty() {
            "No specific candidates provided - analyze the entire file for patterns.".to_string()
        } else {
            format!(
                "The following lines triggered the AST parser. Analyze these specific locations:\n{}",
                candidate_hints.join("\n")
            )
        };

        let candidate_contexts_section = if candidate_contexts.is_empty() {
            "No structured candidate contexts provided.".to_string()
        } else {
            format!(
                "Structured candidate contexts (JSON, one per candidate):\n{}",
                candidate_contexts.join("\n")
            )
        };

        let imports_section = Self::format_import_table(imported_symbols);

        // GraphQL producer context (Stage B2): the SDL root fields this service
        // exposes. Repo-global (one list per scan, identical across files), so it
        // sits in the cacheable front block alongside the guidance. When the
        // service has no SDL producers this is empty and the section is omitted,
        // leaving every non-GraphQL prompt byte-identical to before.
        //
        // The section string carries its OWN leading `\n` (the blank line that
        // separates it from the triage hints above) and its own trailing `\n`. The
        // template therefore interpolates it ADJACENT to the triage-hints
        // placeholder with no surrounding newline of its own — so an empty section
        // contributes exactly zero bytes and the non-GraphQL message is byte-for-byte
        // the pre-feature prompt, preserving the Vertex implicit prefix-cache hit.
        let graphql_producers_section = if graphql_producer_hints.is_empty() {
            String::new()
        } else {
            format!(
                "\n### GRAPHQL SCHEMA PRODUCERS (from this repo's SDL)\n\
                 The fields below are this service's GraphQL schema root fields. If a function in \
                 this file resolves one of them, emit a `graphql_operations` entry linking the \
                 resolver function to its `kind`/`field` and its return type. A function that \
                 RETURNS a field's value is its resolver even when the return is wrapped (Promise, \
                 a response envelope, an async iterator) — always link it. ONLY for a listed field \
                 that NO function in this file returns, emit an entry with just `kind`/`field` and \
                 set `backing_type_symbol` (+ `backing_type_source`) to a co-located type that \
                 declares the field's response shape (leave `resolver_function` null; unwrap \
                 list/`[]` wrappers to the bare element type, e.g. `Order` for `[Order!]!`).\n{}\n",
                graphql_producer_hints
                    .iter()
                    .map(|line| format!("- {}", line))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };

        // GraphQL consumer context (#268 — the consumer mirror of the producer
        // section above): document consumers the deterministic pass
        // (`TaggedTplVisitor::capture_request_call`) could NOT anchor because
        // there was no explicit call-site generic. Repo-global and stable
        // across every file in the scan, same caching rationale as
        // `graphql_producers_section` — an empty section contributes zero
        // bytes so a non-GraphQL (or fully-anchored) prompt stays byte-for-byte
        // identical to before.
        let graphql_consumers_section = if graphql_consumer_hints.is_empty() {
            String::new()
        } else {
            format!(
                "\n### GRAPHQL DOCUMENT CONSUMERS WITH NO EXPLICIT RESULT TYPE\n\
                 The operations below are executable GraphQL documents (`gql`/`graphql` tagged \
                 templates) this repo consumes whose result type has NO explicit call-site generic \
                 (e.g. `client.request<T>(DOC)`) anywhere in the codebase. For each one listed below \
                 that belongs to the file being analyzed, if a type declared or imported in THIS file \
                 describes the operation's RESULT shape, emit a `graphql_consumer_locates` entry with \
                 its `kind`/`field` and set `result_type_symbol` (+ `result_type_source`) to that \
                 type. NEVER use a request/variables type — only the type describing the data the \
                 operation returns. If you are not confident which type is co-located, omit the entry \
                 entirely rather than guessing.\n{}\n",
                graphql_consumer_hints
                    .iter()
                    .map(|line| format!("- {}", line))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };

        // Imported HTTP wrapper definitions (#369 — cross-file wrapper-site
        // resolution): same-repo modules this file imports whose exported
        // functions perform the actual HTTP request. Without their source the
        // model cannot know a call like `apiRequest("GET", "/things")` is an
        // outbound HTTP call, or what base path the wrapper prepends. Empty
        // for files with no such imports, contributing zero bytes so every
        // existing prompt stays byte-for-byte identical (cache-safe).
        let wrapper_context_section = if wrapper_context.is_empty() {
            String::new()
        } else {
            format!(
                "\n### IMPORTED HTTP WRAPPER DEFINITIONS (same-repo modules imported by this file)\n\
                 The source below defines HTTP request wrapper function(s) that live in OTHER \
                 files of this repo and are IMPORTED by the file being analyzed. A call in THIS \
                 file to one of these imported wrappers (directly, or via `.call(...)`/`.apply(...)`) \
                 is a real outbound HTTP data call happening at that call site. For each such call \
                 site, emit a data_call whose method and target are RESOLVED through the wrapper. \
                 The target MUST be the full request URL the wrapper builds: the wrapper's base \
                 (keep any `${{...}}` host/config interpolation verbatim) + the wrapper's path \
                 prefix + the site's path argument. Example: the wrapper's options carry baseURL \
                 `${{cfg.host}}/api/v1` and the site passes \"/things\" -> target \
                 `${{cfg.host}}/api/v1/things`. Emitting only the site's argument (\"/things\") \
                 or dropping the wrapper's path prefix is WRONG — always prepend the wrapper's \
                 full base. Take the method from the site's arguments when the wrapper \
                 parameterizes it. Report the site's own line numbers and call_expression_text. \
                 Do NOT report the wrapper's internal request line here — it belongs to the \
                 wrapper's own file.\n{}\n",
                wrapper_context.join("\n")
            )
        };

        // Web postMessage channel guidance (#372): emitted ONLY when this
        // file's candidates include a `.postMessage(...)` send or an
        // `addEventListener('message', …)` listener (the Signal-8 shapes).
        // The pubsub schema descriptions define topics as first-argument
        // strings; postMessage carries its topic inside the envelope, so the
        // mapping is taught here instead of in the (globally shared) schema
        // text — description edits there measurably perturb unrelated
        // messenger/broker extraction on flash-lite. Files without
        // postMessage candidates contribute zero bytes, so every existing
        // prompt stays byte-for-byte identical (cache-safe).
        let has_postmessage_candidates = candidate_hints
            .iter()
            .any(|h| h.contains(".postMessage [fn:") || h.contains(".addEventListener [fn:"));
        let postmessage_section = if !has_postmessage_candidates {
            String::new()
        } else {
            "\n### WEB POSTMESSAGE CHANNEL (candidates above include postMessage / 'message'-listener sites)\n\
             The browser postMessage channel is a pub/sub surface; extract pubsub_operations from \
             these candidate sites. A `.postMessage(...)` call whose message object carries a string \
             literal under an `action` or `type` key PUBLISHES that string as the topic \
             (`parent.postMessage({ action: 'doc-ready', data }, '*')` -> one operation, role \
             `publisher`, topic 'doc-ready'). A listener registered with \
             `addEventListener('message', handler)` SUBSCRIBES: each string-literal dispatch case \
             over that envelope key inside the handler (`case 'doc-ready':`) is one operation, role \
             `subscriber`, with that case's string as the topic — follow the handler function \
             reference to its body, and emit one operation per case. A postMessage call whose \
             message has no literal `action`/`type` key is a raw data transfer: emit nothing for \
             it. The DOM event name 'message' is never itself a topic — a catch-all listener whose \
             handler does not dispatch on envelope keys yields NO operations. For \
             `primary_type_symbol` use the payload's named type when evident (e.g. the declared \
             type of the callback receiving `event.data.data`), else null; `broker` is null.\n"
                .to_string()
        };

        // Add line-number prefixes to file content so Gemini can read line numbers directly
        let mut numbered_content =
            String::with_capacity(file_content.len() + file_content.lines().count() * 7);
        for (i, line) in file_content.lines().enumerate() {
            use std::fmt::Write;
            if i > 0 {
                numbered_content.push('\n');
            }
            let _ = write!(numbered_content, "{:4}| {}", i + 1, line);
        }

        format!(
            r#"### ACTIVE PATTERNS (Derived from Framework Guidance)
{{
  "mount_patterns": [
    {}
  ],
  "endpoint_patterns": [
    {}
  ],
  "data_fetching_patterns": [
    {}
  ]
}}

### FRAMEWORK-SPECIFIC HINTS
{}{}{}
### FRAMEWORK-SPECIFIC PARSING NOTES
These notes are generated per-scan by the framework guidance layer and describe how to correctly extract endpoints, mounts, owners, and prefixes for the exact framework(s) detected in this repo. Read them carefully — they override any generic rule in the system prompt when they conflict.
{}

### CANDIDATE TARGETS (AST-Detected Hints)
{}

### CANDIDATE CONTEXT (Structured JSON)
{}  // Use these JSON blobs to decide method/path/consumer vs non-consumer. If missing path/method, set them to null.

### IMPORT TABLE (Do not hallucinate sources)
{}
{}{}
### FILE CONTENT (Path: {})
Lines are prefixed with line numbers. Use these numbers for *_expression_line fields.
```
{}
```

Analyze this file and return a JSON object with:
- "mounts": array of mount relationships found
- "endpoints": array of HTTP endpoint definitions found
- "data_calls": array of data fetching calls found
- "pubsub_operations": array of pub/sub publish/subscribe operations found

For each mount, include: line_number, parent_node, child_node, mount_path, import_source (null if local), pattern_matched

For each endpoint, include: candidate_id, line_number, owner_node, method, path, handler_name, pattern_matched,
response_expression_text, response_expression_line, payload_expression_text, payload_expression_line,
primary_type_symbol, type_import_source
  - Echo candidate_id from the candidate context
  - MUST emit response_expression_text: copy the EXACT payload subexpression (e.g., "users" from res.json(users), ctx.body = users, h.response(users), or return users). Emit null for payload-less handlers.
  - MUST emit response_expression_line: read the line number from the prefix in the source code (line where the payload subexpression appears)
  - For payload_expression_text: copy the EXACT expression for the request payload (e.g., "req.body")
  - For payload_expression_line: read the line number from the prefix

For each data_call, include: candidate_id, line_number, target, method (null if unknown), pattern_matched,
call_expression_text, call_expression_line, payload_expression_text, payload_expression_line,
primary_type_symbol, type_import_source
  - Echo candidate_id from the candidate context
  - MUST emit call_expression_text: copy the EXACT text of the fetch/axios/HTTP call (e.g., 'fetch("/api/users")')
  - MUST emit call_expression_line: read the line number from the prefix
  - For payload_expression_text: copy the EXACT payload argument text if detected; emit null when the call sends no payload
  - For payload_expression_line: read the line number from the prefix; null whenever payload_expression_text is null

For each pubsub_operation, include: topic, role, line_number, primary_type_symbol, type_import_source, broker, payload_expression_text, payload_expression_line
  - topic: the literal topic/channel/subject string (resolve a named const like TOPIC to its string literal)
  - role: "publisher" if the code SENDS a payload to a topic, "subscriber" if it REGISTERS a handler for a topic
  - line_number: read the line number from the prefix in the source code
  - primary_type_symbol: the DECODED application payload type (unwrap envelope/transport wrappers and wire types down to the inner named type); null if untyped
  - type_import_source: import path where primary_type_symbol is defined (from the import table), or null if local; null whenever primary_type_symbol is null
  - broker: the messaging library/transport if evident (e.g. "kafka"), else null
  - COMPLETENESS: the pub/sub candidates in CANDIDATE TARGETS are an exhaustive checklist. For EVERY candidate whose pattern is a publish/subscribe call, either emit a pubsub_operations entry (resolving same-file consts and `${{name}}:event` template topics to their literal string) or skip it ONLY because it is a request/response action invocation (e.g. .call / registerActionHandler / registerMethodActionHandlers) or its topic cannot be resolved to a literal. Never omit one because the file is long or the call site is late in the file.

Return ONLY the JSON object, no explanations."#,
            // Section order is load-bearing for Vertex implicit prompt caching.
            // The guidance blocks (patterns + triage hints + parsing notes) are
            // byte-identical across every file in a scan (one FrameworkGuidance is
            // fetched once and reused), so keeping them as a contiguous front block
            // — before any per-file content (candidates, imports, source) — lets the
            // cacheable request prefix extend past the systemInstruction to cover
            // them too. Per-file content stays last so it never breaks the prefix.
            // Do not interleave stable and per-file sections.
            mount_patterns,
            endpoint_patterns,
            data_patterns,
            guidance.triage_hints,
            graphql_producers_section,
            graphql_consumers_section,
            guidance.parsing_notes,
            candidates_section,
            candidate_contexts_section,
            imports_section,
            wrapper_context_section,
            postmessage_section,
            file_path,
            numbered_content
        )
    }

    /// Format pattern examples as JSON array items.
    fn format_patterns(
        &self,
        patterns: &[crate::agents::framework_guidance_agent::PatternExample],
    ) -> String {
        if patterns.is_empty() {
            return "// No patterns provided".to_string();
        }

        patterns
            .iter()
            .map(|p| {
                format!(
                    r#"    // {} ({}): {}
    "{}""#,
                    p.framework, p.description, p.pattern, p.pattern
                )
            })
            .collect::<Vec<_>>()
            .join(",\n")
    }

    /// Format the AST-derived imports grouped by source module with kind
    /// annotations. This is Move 3 (§9.3) in framework-coverage.md: richer
    /// per-file grounding so the LLM reads symbols against real imports
    /// rather than a pattern list.
    ///
    /// Format:
    /// ```text
    /// Imports resolved from the AST (grouped by source):
    ///   - From '@nestjs/common': Get, Post, Controller [named]
    ///   - From 'koa': Koa [default]
    ///   - From 'express': express [namespace]
    ///   - From './user.service': UserService [named]
    /// ```
    fn format_import_table(imported_symbols: &HashMap<String, ImportedSymbol>) -> String {
        if imported_symbols.is_empty() {
            return "No imports detected by AST; treat all symbols as local unless resolved in code.".to_string();
        }

        // Group by (source, kind). Named imports can batch per source;
        // default/namespace are one-per-source in practice but we handle it
        // uniformly for deterministic output.
        use std::collections::BTreeMap;
        let mut groups: BTreeMap<(String, &'static str), Vec<(String, String)>> = BTreeMap::new();
        for symbol in imported_symbols.values() {
            let kind_label = match symbol.kind {
                SymbolKind::Named => "named",
                SymbolKind::Default => "default",
                SymbolKind::Namespace => "namespace",
            };
            groups
                .entry((symbol.source.clone(), kind_label))
                .or_default()
                .push((symbol.local_name.clone(), symbol.imported_name.clone()));
        }

        // Stable per-group ordering by local name.
        for entries in groups.values_mut() {
            entries.sort();
        }

        let lines: Vec<String> = groups
            .iter()
            .map(|((source, kind_label), entries)| {
                let pretty: Vec<String> = entries
                    .iter()
                    .map(|(local, imported)| {
                        if local == imported {
                            local.clone()
                        } else {
                            // `import { Foo as Bar }` → Bar (as Foo)
                            format!("{local} (as {imported})")
                        }
                    })
                    .collect();
                format!(
                    "  - From '{source}': {names} [{kind_label}]",
                    names = pretty.join(", "),
                )
            })
            .collect();

        format!(
            "Imports resolved from the AST (grouped by source):\n{}\n\n\
             Use this table to interpret candidates. An identifier used in this file that appears above is the imported symbol from that module; an identifier NOT listed is either local to this file or globally available. Do NOT invent sources.",
            lines.join("\n"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::framework_guidance_agent::PatternExample;
    use std::collections::HashMap;

    fn endpoint_json(extra_fields: &str) -> String {
        format!(
            r#"{{
                "candidate_id": "span:1-2",
                "line_number": 5,
                "owner_node": "app",
                "method": "GET",
                "path": "/users",
                "handler_name": "anonymous",
                "pattern_matched": ".get(",
                "payload_expression_text": null,
                "payload_expression_line": null,
                "response_expression_text": null,
                "response_expression_line": null,
                "primary_type_symbol": null,
                "type_import_source": null{}
            }}"#,
            extra_fields
        )
    }

    fn data_call_json(extra_fields: &str) -> String {
        format!(
            r#"{{
                "candidate_id": "span:1-2",
                "line_number": 5,
                "target": "/api/users",
                "method": "GET",
                "pattern_matched": "fetch(",
                "payload_expression_text": null,
                "payload_expression_line": null,
                "primary_type_symbol": null,
                "type_import_source": null{}
            }}"#,
            extra_fields
        )
    }

    #[test]
    fn call_kind_absorbs_model_junk_instead_of_failing_the_file() {
        // An off-enum call_kind must degrade to None, not fail the whole file's
        // parse (which would drop every data call in the file); this is the
        // fail-closed behavior the field is meant to provide.
        for junk in ["internal-http", "INTERNAL_HTTP", "datastore", "", "???"] {
            let json = data_call_json(&format!(r#", "call_kind": "{}""#, junk));
            let call: DataCallResult = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("junk {:?} failed the parse: {}", junk, e));
            assert_eq!(
                call.call_kind,
                crate::operation::CallKind::parse_lenient(junk),
                "junk {:?}",
                junk
            );
        }
        // Non-string JSON values degrade to None the same way.
        for junk in ["0", "false", "{}", "[\"sdk\"]"] {
            let json = data_call_json(&format!(r#", "call_kind": {}"#, junk));
            let call: DataCallResult = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("non-string {:?} failed the parse: {}", junk, e));
            assert_eq!(call.call_kind, None, "non-string {:?}", junk);
        }
        // Valid wire values still parse.
        let json = data_call_json(r#", "call_kind": "sdk""#);
        let call: DataCallResult = serde_json::from_str(&json).unwrap();
        assert_eq!(call.call_kind, Some(crate::operation::CallKind::Sdk));
    }

    #[test]
    fn emission_style_deserializes_wire_values() {
        for (wire, expected) in [
            ("imperative-send", EmissionStyle::ImperativeSend),
            ("return-value", EmissionStyle::ReturnValue),
            ("no-payload", EmissionStyle::NoPayload),
        ] {
            let json = endpoint_json(&format!(r#", "emission_style": "{}""#, wire));
            let endpoint: EndpointResult = serde_json::from_str(&json).unwrap();
            assert_eq!(endpoint.emission_style, Some(expected), "wire {}", wire);
        }
    }

    #[test]
    fn emission_style_absorbs_model_junk_instead_of_failing_the_file() {
        // One off-enum string must not fail deserialization of the whole
        // FileAnalysisResult (which would drop every endpoint in the file).
        for junk in ["return_value", "Imperative-Send", "NO PAYLOAD", "", "???"] {
            let json = endpoint_json(&format!(r#", "emission_style": "{}""#, junk));
            let endpoint: EndpointResult = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("junk {:?} failed the parse: {}", junk, e));
            let expected = EmissionStyle::parse_lenient(junk);
            assert_eq!(endpoint.emission_style, expected, "junk {:?}", junk);
        }
        // Non-string JSON values degrade to None the same way.
        for junk in ["0", "false", "{}", "[\"return-value\"]"] {
            let json = endpoint_json(&format!(r#", "emission_style": {}"#, junk));
            let endpoint: EndpointResult = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("non-string {:?} failed the parse: {}", junk, e));
            assert_eq!(endpoint.emission_style, None, "non-string {:?}", junk);
        }
        // Lenient parsing still recovers obvious separator/case variants.
        assert_eq!(
            EmissionStyle::parse_lenient("return_value"),
            Some(EmissionStyle::ReturnValue)
        );
        assert_eq!(EmissionStyle::parse_lenient("???"), None);

        // Missing and null both map to None.
        for json in [
            endpoint_json(""),
            endpoint_json(r#", "emission_style": null"#),
        ] {
            let endpoint: EndpointResult = serde_json::from_str(&json).unwrap();
            assert_eq!(endpoint.emission_style, None);
        }
    }

    #[test]
    fn sanitize_downgrades_contradictory_no_payload_to_imperative_send() {
        let json = endpoint_json(r#", "emission_style": "no-payload""#);
        let mut endpoint: EndpointResult = serde_json::from_str(&json).unwrap();
        endpoint.response_expression_text = Some("users".to_string());
        endpoint.response_expression_line = Some(6);
        let mut result = FileAnalysisResult {
            graphql_consumer_locates: vec![],
            mounts: vec![],
            endpoints: vec![endpoint],
            data_calls: vec![],
            graphql_operations: vec![],
            pubsub_operations: vec![],
        };

        FileAnalyzerAgent::sanitize_result(&mut result);

        assert_eq!(
            result.endpoints[0].emission_style,
            Some(EmissionStyle::ImperativeSend),
            "a contradicted no-payload claim must not discard a recoverable contract"
        );

        // An honest no-payload claim (null expression) is left alone.
        let json = endpoint_json(r#", "emission_style": "no-payload""#);
        let endpoint: EndpointResult = serde_json::from_str(&json).unwrap();
        let mut result = FileAnalysisResult {
            graphql_consumer_locates: vec![],
            mounts: vec![],
            endpoints: vec![endpoint],
            data_calls: vec![],
            graphql_operations: vec![],
            pubsub_operations: vec![],
        };
        FileAnalyzerAgent::sanitize_result(&mut result);
        assert_eq!(
            result.endpoints[0].emission_style,
            Some(EmissionStyle::NoPayload)
        );
    }

    fn create_test_guidance() -> FrameworkGuidance {
        FrameworkGuidance {
            mount_patterns: vec![
                PatternExample {
                    pattern: ".use(".to_string(),
                    description: "Mount middleware or router".to_string(),
                    framework: "express".to_string(),
                },
                PatternExample {
                    pattern: ".register(".to_string(),
                    description: "Register plugin or router".to_string(),
                    framework: "fastify".to_string(),
                },
            ],
            endpoint_patterns: vec![
                PatternExample {
                    pattern: ".get(".to_string(),
                    description: "GET endpoint".to_string(),
                    framework: "express".to_string(),
                },
                PatternExample {
                    pattern: ".post(".to_string(),
                    description: "POST endpoint".to_string(),
                    framework: "express".to_string(),
                },
            ],
            middleware_patterns: vec![],
            data_fetching_patterns: vec![
                PatternExample {
                    pattern: "fetch(".to_string(),
                    description: "Fetch API call".to_string(),
                    framework: "native".to_string(),
                },
                PatternExample {
                    pattern: "axios.".to_string(),
                    description: "Axios HTTP call".to_string(),
                    framework: "axios".to_string(),
                },
            ],
            triage_hints: "Look for router.use() for mounts, router.get/post/etc for endpoints"
                .to_string(),
            parsing_notes: "Express uses chained methods".to_string(),
        }
    }

    #[test]
    fn test_file_analysis_result_default() {
        let result = FileAnalysisResult::default();
        assert!(result.mounts.is_empty());
        assert!(result.endpoints.is_empty());
        assert!(result.data_calls.is_empty());
    }

    #[test]
    fn test_mount_result_serialization() {
        let mount = MountResult {
            line_number: 10,
            parent_node: "app".to_string(),
            child_node: "userRouter".to_string(),
            mount_path: "/users".to_string(),
            import_source: Some("./routes/users".to_string()),
            pattern_matched: ".use(".to_string(),
        };

        let json = serde_json::to_string(&mount).unwrap();
        assert!(json.contains("parent_node"));
        assert!(json.contains("import_source"));
        assert!(json.contains("./routes/users"));

        let deserialized: MountResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.parent_node, "app");
        assert_eq!(
            deserialized.import_source,
            Some("./routes/users".to_string())
        );
    }

    #[test]
    fn test_endpoint_result_serialization() {
        let endpoint = EndpointResult {
            view_module: false,
            candidate_id: "span:100-140".to_string(),
            line_number: 15,
            owner_node: "router".to_string(),
            method: "GET".to_string(),
            path: "/users/:id".to_string(),
            handler_name: "getUserById".to_string(),
            pattern_matched: ".get(".to_string(),
            call_expression_span_start: None,
            call_expression_span_end: None,
            payload_expression_text: None,
            payload_expression_line: None,
            response_expression_text: None,
            response_expression_line: None,
            emission_style: None,
            primary_type_symbol: Some("User".to_string()),
            type_import_source: Some("./types/user".to_string()),
            resolution_source: None,
        };

        let json = serde_json::to_string(&endpoint).unwrap();
        assert!(json.contains("owner_node"));
        assert!(json.contains("GET"));

        let deserialized: EndpointResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.method, "GET");
        assert_eq!(deserialized.handler_name, "getUserById");
    }

    #[test]
    fn test_data_call_result_serialization() {
        let data_call = DataCallResult {
            call_kind: None,
            candidate_id: "span:200-260".to_string(),
            line_number: 25,
            target: "https://api.example.com/data".to_string(),
            method: Some("POST".to_string()),
            pattern_matched: "fetch(".to_string(),
            call_expression_span_start: None,
            call_expression_span_end: None,
            call_expression_text: None,
            call_expression_line: None,
            payload_expression_text: None,
            payload_expression_line: None,
            primary_type_symbol: Some("Comment".to_string()),
            type_import_source: None,

            loopback_default_url: None,
            base: None,
            consumers_not_resolved: None,
            resolution_source: None,
        };

        let json = serde_json::to_string(&data_call).unwrap();
        assert!(json.contains("target"));
        assert!(json.contains("POST"));

        let deserialized: DataCallResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.target, "https://api.example.com/data");
        assert_eq!(deserialized.method, Some("POST".to_string()));
    }

    #[test]
    fn test_sanitize_result_filters_placeholders() {
        let mut result = FileAnalysisResult {
            graphql_consumer_locates: vec![],
            mounts: vec![],
            endpoints: vec![EndpointResult {
                view_module: false,
                candidate_id: "span:10-50".to_string(),
                line_number: 1,
                owner_node: "app".to_string(),
                method: "GET".to_string(),
                path: "/test".to_string(),
                handler_name: "handler".to_string(),
                pattern_matched: ".get(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                payload_expression_text: None,
                payload_expression_line: None,
                response_expression_text: None,
                response_expression_line: None,
                emission_style: None,
                primary_type_symbol: Some("-".to_string()),
                type_import_source: Some(".repo-a_types.ts".to_string()),
                resolution_source: None,
            }],
            data_calls: vec![DataCallResult {
                call_kind: None,
                candidate_id: "span:60-120".to_string(),
                line_number: 2,
                target: "https://example.com".to_string(),
                method: Some("  POST  ".to_string()),
                pattern_matched: "fetch(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                call_expression_text: None,
                call_expression_line: None,
                payload_expression_text: None,
                payload_expression_line: None,
                primary_type_symbol: Some("NULL".to_string()),
                type_import_source: Some("bad import (oops)".to_string()),

                loopback_default_url: None,
                base: None,
                consumers_not_resolved: None,
                resolution_source: None,
            }],
            graphql_operations: vec![],
            pubsub_operations: vec![],
        };

        let needs_retry = FileAnalyzerAgent::sanitize_result(&mut result);
        assert!(needs_retry);

        let endpoint = &result.endpoints[0];
        assert!(endpoint.primary_type_symbol.is_none());
        assert!(endpoint.type_import_source.is_none());

        let data_call = &result.data_calls[0];
        assert_eq!(data_call.method, Some("POST".to_string()));
        assert!(data_call.primary_type_symbol.is_none());
        assert!(data_call.type_import_source.is_none());
    }

    #[test]
    fn sanitize_normalizes_pubsub_payload_locators() {
        let op = |text: Option<&str>, line: Option<i32>| PubsubOperation {
            topic: "itemArchived".to_string(),
            role: Some(crate::operation::PubsubRole::Subscriber),
            line_number: 13,
            primary_type_symbol: None,
            type_import_source: None,
            broker: None,
            payload_expression_text: text.map(String::from),
            payload_expression_line: line,
        };
        let mut result = FileAnalysisResult {
            pubsub_operations: vec![
                // Arrow-param-list parens stripped (the measured 12/20 shape).
                op(Some("(decision)"), Some(36)),
                op(Some("({ time, item })"), Some(13)),
                // Non-enclosing parens untouched.
                op(Some("(a).b(c)"), Some(1)),
                // Null-like text nulls the pair; line-only never survives alone.
                op(Some("null"), Some(5)),
                op(None, Some(7)),
                // Non-positive line dropped, text kept.
                op(Some("payload"), Some(0)),
            ],
            ..Default::default()
        };
        FileAnalyzerAgent::sanitize_result(&mut result);
        let texts: Vec<Option<&str>> = result
            .pubsub_operations
            .iter()
            .map(|o| o.payload_expression_text.as_deref())
            .collect();
        let lines: Vec<Option<i32>> = result
            .pubsub_operations
            .iter()
            .map(|o| o.payload_expression_line)
            .collect();
        assert_eq!(
            texts,
            vec![
                Some("decision"),
                Some("{ time, item }"),
                Some("(a).b(c)"),
                None,
                None,
                Some("payload"),
            ]
        );
        assert_eq!(lines, vec![Some(36), Some(13), Some(1), None, None, None]);
    }

    #[test]
    fn sanitize_drops_pubsub_ops_with_null_like_topic() {
        let pubsub_op = |topic: &str| PubsubOperation {
            topic: topic.to_string(),
            role: Some(crate::operation::PubsubRole::Publisher),
            line_number: 1,
            primary_type_symbol: None,
            type_import_source: None,
            broker: None,
            payload_expression_text: None,
            payload_expression_line: None,
        };

        let mut result = FileAnalysisResult {
            pubsub_operations: vec![
                pubsub_op("null"),
                pubsub_op("+null"),
                pubsub_op("-null"),
                pubsub_op("undefined"),
                pubsub_op(""),
                pubsub_op("  -  "),
                pubsub_op("  orders.created  "),
            ],
            ..Default::default()
        };

        FileAnalyzerAgent::sanitize_result(&mut result);

        // Only the real topic survives, and it is trimmed.
        assert_eq!(result.pubsub_operations.len(), 1);
        assert_eq!(result.pubsub_operations[0].topic, "orders.created");
    }

    #[test]
    fn test_file_analysis_result_serialization() {
        let result = FileAnalysisResult {
            graphql_consumer_locates: vec![],
            mounts: vec![MountResult {
                line_number: 5,
                parent_node: "app".to_string(),
                child_node: "apiRouter".to_string(),
                mount_path: "/api".to_string(),
                import_source: Some("./api".to_string()),
                pattern_matched: ".use(".to_string(),
            }],
            endpoints: vec![EndpointResult {
                view_module: false,
                candidate_id: "span:80-120".to_string(),
                line_number: 10,
                owner_node: "router".to_string(),
                method: "GET".to_string(),
                path: "/health".to_string(),
                handler_name: "healthCheck".to_string(),
                pattern_matched: ".get(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                payload_expression_text: None,
                payload_expression_line: None,
                response_expression_text: None,
                response_expression_line: None,
                emission_style: None,
                primary_type_symbol: None,
                type_import_source: None,
                resolution_source: None,
            }],
            data_calls: vec![],
            graphql_operations: vec![],
            pubsub_operations: vec![],
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("mounts"));
        assert!(json.contains("endpoints"));
        assert!(json.contains("data_calls"));

        let deserialized: FileAnalysisResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.mounts.len(), 1);
        assert_eq!(deserialized.endpoints.len(), 1);
        assert!(deserialized.data_calls.is_empty());
    }

    #[test]
    fn graphql_operations_deserialize_from_model_shape() {
        // The exact wire shape the file-analyzer emits for a GraphQL resolver.
        let json = r#"{
            "mounts": [],
            "endpoints": [],
            "data_calls": [],
            "graphql_operations": [
                {
                    "kind": "query",
                    "field": "order",
                    "resolver_function": "resolveOrder",
                    "resolver_line": 38,
                    "primary_type_symbol": "ApiResponse",
                    "type_import_source": null
                }
            ]
        }"#;

        let result: FileAnalysisResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.graphql_operations.len(), 1);
        let op = &result.graphql_operations[0];
        assert_eq!(op.kind, crate::operation::GraphqlOperationKind::Query);
        assert_eq!(op.field, "order");
        assert_eq!(op.resolver_function.as_deref(), Some("resolveOrder"));
        assert_eq!(op.resolver_line, Some(38));
        assert_eq!(op.primary_type_symbol.as_deref(), Some("ApiResponse"));
        assert_eq!(op.type_import_source, None);
    }

    #[test]
    fn graphql_operations_deserialize_resolverless_type_locate() {
        // #248: a resolver-less field the LLM locates by its backing type omits
        // resolver_function/resolver_line entirely and carries the type in the
        // dedicated backing_type_symbol. #[serde(default)] must yield None for the
        // missing resolver locators rather than failing the parse.
        let json = r#"{
            "mounts": [],
            "endpoints": [],
            "data_calls": [],
            "graphql_operations": [
                {
                    "kind": "query",
                    "field": "orders",
                    "backing_type_symbol": "Order",
                    "backing_type_source": null
                }
            ]
        }"#;

        let result: FileAnalysisResult = serde_json::from_str(json).unwrap();
        let op = &result.graphql_operations[0];
        assert_eq!(op.field, "orders");
        assert_eq!(op.resolver_function, None);
        assert_eq!(op.resolver_line, None);
        assert_eq!(op.primary_type_symbol, None);
        assert_eq!(op.backing_type_symbol.as_deref(), Some("Order"));
    }

    #[test]
    fn graphql_operations_default_empty_when_omitted() {
        // A non-GraphQL file omits graphql_operations entirely; #[serde(default)]
        // must yield an empty vec rather than failing the whole file's parse.
        let json = r#"{ "mounts": [], "endpoints": [], "data_calls": [] }"#;
        let result: FileAnalysisResult = serde_json::from_str(json).unwrap();
        assert!(result.graphql_operations.is_empty());

        // mutation / subscription wire values round-trip too.
        let json = r#"{
            "mounts": [], "endpoints": [], "data_calls": [],
            "graphql_operations": [
                { "kind": "mutation", "field": "createOrder", "resolver_function": "createOrder", "resolver_line": 7, "primary_type_symbol": null, "type_import_source": null },
                { "kind": "subscription", "field": "orderUpdated", "resolver_function": "orderUpdated", "resolver_line": 9, "primary_type_symbol": null, "type_import_source": null }
            ]
        }"#;
        let result: FileAnalysisResult = serde_json::from_str(json).unwrap();
        assert_eq!(
            result.graphql_operations[0].kind,
            crate::operation::GraphqlOperationKind::Mutation
        );
        assert_eq!(
            result.graphql_operations[1].kind,
            crate::operation::GraphqlOperationKind::Subscription
        );
    }

    #[test]
    fn pubsub_operations_deserialize_from_model_shape() {
        // The exact wire shape the file-analyzer emits for a pub/sub operation.
        let json = r#"{
            "mounts": [],
            "endpoints": [],
            "data_calls": [],
            "pubsub_operations": [
                {
                    "topic": "metrics.page_view",
                    "role": "subscriber",
                    "line_number": 5,
                    "primary_type_symbol": "PageViewEvent",
                    "type_import_source": "./types/events",
                    "broker": "redis"
                },
                {
                    "topic": "metrics.page_view",
                    "role": "publisher",
                    "line_number": 7,
                    "primary_type_symbol": null,
                    "type_import_source": null,
                    "broker": null
                }
            ]
        }"#;

        let result: FileAnalysisResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.pubsub_operations.len(), 2);
        let sub = &result.pubsub_operations[0];
        assert_eq!(sub.topic, "metrics.page_view");
        assert_eq!(sub.role, Some(crate::operation::PubsubRole::Subscriber));
        assert_eq!(sub.line_number, 5);
        assert_eq!(sub.primary_type_symbol.as_deref(), Some("PageViewEvent"));
        assert_eq!(sub.type_import_source.as_deref(), Some("./types/events"));
        assert_eq!(sub.broker.as_deref(), Some("redis"));
        assert_eq!(
            result.pubsub_operations[1].role,
            Some(crate::operation::PubsubRole::Publisher)
        );
    }

    #[test]
    fn pubsub_operations_default_empty_and_role_absorbs_junk() {
        // A file with no pub/sub omits the array entirely; #[serde(default)]
        // yields an empty vec rather than failing the whole file's parse.
        let json = r#"{ "mounts": [], "endpoints": [], "data_calls": [] }"#;
        let result: FileAnalysisResult = serde_json::from_str(json).unwrap();
        assert!(result.pubsub_operations.is_empty());

        // An off-enum / non-string role degrades to None (lenient, like call_kind)
        // instead of failing the parse; the optional type/broker slots default.
        for junk in [
            r#""SUBSCRIBE""#,
            r#""listener""#,
            r#""""#,
            "0",
            "false",
            "{}",
        ] {
            let json = format!(
                r#"{{ "mounts": [], "endpoints": [], "data_calls": [],
                    "pubsub_operations": [
                        {{ "topic": "t", "role": {}, "line_number": 1 }}
                    ] }}"#,
                junk
            );
            let result: FileAnalysisResult = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("junk role {:?} failed the parse: {}", junk, e));
            assert_eq!(result.pubsub_operations[0].role, None, "junk {:?}", junk);
            assert_eq!(result.pubsub_operations[0].primary_type_symbol, None);
            assert_eq!(result.pubsub_operations[0].broker, None);
        }
    }

    #[test]
    fn test_choose_best_result_prefers_richer_output() {
        let initial = FileAnalysisResult {
            graphql_consumer_locates: vec![],
            mounts: vec![MountResult {
                line_number: 1,
                parent_node: "app".to_string(),
                child_node: "router".to_string(),
                mount_path: "/api".to_string(),
                import_source: None,
                pattern_matched: "app.use".to_string(),
            }],
            endpoints: vec![EndpointResult {
                view_module: false,
                candidate_id: "span:130-180".to_string(),
                line_number: 2,
                owner_node: "router".to_string(),
                method: "GET".to_string(),
                path: "/users".to_string(),
                handler_name: "handler".to_string(),
                pattern_matched: ".get(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                payload_expression_text: None,
                payload_expression_line: None,
                response_expression_text: None,
                response_expression_line: None,
                emission_style: None,
                primary_type_symbol: None,
                type_import_source: None,
                resolution_source: None,
            }],
            data_calls: vec![DataCallResult {
                call_kind: None,
                candidate_id: "span:190-240".to_string(),
                line_number: 3,
                target: "/users".to_string(),
                method: Some("GET".to_string()),
                pattern_matched: "fetch(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                call_expression_text: None,
                call_expression_line: None,
                payload_expression_text: None,
                payload_expression_line: None,
                primary_type_symbol: None,
                type_import_source: None,

                loopback_default_url: None,
                base: None,
                consumers_not_resolved: None,
                resolution_source: None,
            }],
            graphql_operations: vec![],
            pubsub_operations: vec![],
        };

        let retry = FileAnalysisResult {
            graphql_consumer_locates: vec![],
            mounts: vec![],
            endpoints: vec![],
            data_calls: vec![],
            graphql_operations: vec![],
            pubsub_operations: vec![],
        };

        let chosen = FileAnalyzerAgent::choose_best_result(initial.clone(), retry);
        assert_eq!(chosen.mounts.len(), initial.mounts.len());
        assert_eq!(chosen.endpoints.len(), initial.endpoints.len());
        assert_eq!(chosen.data_calls.len(), initial.data_calls.len());
    }

    /// A resolver linkage as the model emits it for a gateway resolver file.
    fn test_graphql_op(field: &str) -> GraphqlOperation {
        serde_json::from_str(&format!(
            r#"{{"kind":"query","field":"{field}","resolver_function":"resolve","resolver_line":38,"primary_type_symbol":null,"type_import_source":null}}"#
        ))
        .unwrap()
    }

    fn test_pubsub_op(topic: &str) -> PubsubOperation {
        serde_json::from_str(&format!(
            r#"{{"topic":"{topic}","role":"publisher","line_number":1}}"#
        ))
        .unwrap()
    }

    fn test_data_call(target: &str) -> DataCallResult {
        DataCallResult {
            call_kind: None,
            candidate_id: "span:190-240".to_string(),
            line_number: 3,
            target: target.to_string(),
            method: Some("GET".to_string()),
            pattern_matched: "fetch(".to_string(),
            call_expression_span_start: None,
            call_expression_span_end: None,
            call_expression_text: None,
            call_expression_line: None,
            payload_expression_text: None,
            payload_expression_line: None,
            primary_type_symbol: None,
            type_import_source: None,
            loopback_default_url: None,
            base: None,
            consumers_not_resolved: None,
            resolution_source: None,
        }
    }

    #[test]
    fn test_result_score_counts_all_six_sections() {
        let consumer_locate: GraphqlConsumerLocate = serde_json::from_str(
            r#"{"kind":"query","field":"order","result_type_symbol":"Order","result_type_source":null}"#,
        )
        .unwrap();
        let result = FileAnalysisResult {
            data_calls: vec![test_data_call("/users")],
            graphql_operations: vec![test_graphql_op("order"), test_graphql_op("orders")],
            pubsub_operations: vec![test_pubsub_op("jobs.created")],
            graphql_consumer_locates: vec![consumer_locate],
            ..Default::default()
        };
        // 1 data call + 2 graphql ops + 1 pubsub op + 1 consumer locate.
        assert_eq!(FileAnalyzerAgent::result_score(&result), 5);
    }

    #[test]
    fn test_choose_best_result_counts_non_http_sections() {
        // The retry ties on the three HTTP sections but lost the graphql and
        // pubsub findings the original had. Before result_score counted all
        // six sections, this tie selected the retry and silently dropped them.
        let initial = FileAnalysisResult {
            data_calls: vec![test_data_call("/users")],
            graphql_operations: vec![test_graphql_op("order")],
            pubsub_operations: vec![test_pubsub_op("jobs.created")],
            ..Default::default()
        };
        let retry = FileAnalysisResult {
            data_calls: vec![test_data_call("/users")],
            ..Default::default()
        };
        let chosen = FileAnalyzerAgent::choose_best_result(initial, retry);
        assert_eq!(chosen.graphql_operations.len(), 1);
        assert_eq!(chosen.pubsub_operations.len(), 1);
    }

    #[test]
    fn test_choose_best_result_retry_recovering_graphql_section_wins() {
        // The retry recovered graphql findings the original lacked; the fixed
        // score sees the difference and selects the retry.
        let initial = FileAnalysisResult {
            data_calls: vec![test_data_call("/users")],
            ..Default::default()
        };
        let retry = FileAnalysisResult {
            data_calls: vec![test_data_call("/users")],
            graphql_operations: vec![test_graphql_op("order")],
            ..Default::default()
        };
        let chosen = FileAnalyzerAgent::choose_best_result(initial, retry);
        assert_eq!(chosen.graphql_operations.len(), 1);
    }

    #[test]
    fn test_format_patterns_empty() {
        let agent = FileAnalyzerAgent::new(AgentService::new());
        let result = agent.format_patterns(&[]);
        assert_eq!(result, "// No patterns provided");
    }

    #[test]
    fn test_format_patterns_with_items() {
        let agent = FileAnalyzerAgent::new(AgentService::new());
        let patterns = vec![
            PatternExample {
                pattern: ".get(".to_string(),
                description: "GET endpoint".to_string(),
                framework: "express".to_string(),
            },
            PatternExample {
                pattern: ".post(".to_string(),
                description: "POST endpoint".to_string(),
                framework: "express".to_string(),
            },
        ];

        let result = agent.format_patterns(&patterns);
        assert!(result.contains(".get("));
        assert!(result.contains(".post("));
        assert!(result.contains("express"));
        assert!(result.contains("GET endpoint"));
    }

    #[test]
    fn test_build_user_message() {
        let agent = FileAnalyzerAgent::new(AgentService::new());
        let guidance = create_test_guidance();
        let file_content = r#"
import express from 'express';
const app = express();
app.get('/health', (req, res) => res.json({ status: 'ok' }));
"#;

        let message = agent.build_user_message("test.ts", file_content, &guidance);

        assert!(message.contains("ACTIVE PATTERNS"));
        assert!(message.contains("mount_patterns"));
        assert!(message.contains("endpoint_patterns"));
        assert!(message.contains("data_fetching_patterns"));
        assert!(message.contains("test.ts"));
        assert!(message.contains("express"));
    }

    #[test]
    fn build_user_message_instructs_pubsub_operations() {
        // The response schema requires `pubsub_operations`, so the message must
        // also teach it: with no instruction (and nothing in the system prompt)
        // the lite model omitted the array in 9/12 harness runs on the
        // corpus-2 Kafka subscriber, silently dropping every pub/sub op in the
        // file. Instruction and schema were each independently sufficient in
        // the paired harness experiment (12/12 emission); both ship together.
        let agent = FileAnalyzerAgent::new(AgentService::new());
        let guidance = create_test_guidance();

        let message = agent.build_user_message("test.ts", "const x = 1;", &guidance);

        assert!(message.contains(
            "- \"pubsub_operations\": array of pub/sub publish/subscribe operations found"
        ));
        assert!(message.contains("For each pubsub_operation, include: topic, role, line_number"));
        assert!(
            message.contains("resolve a named const like TOPIC to its string literal"),
            "topic instruction must teach const-reference resolution"
        );
    }

    fn named(local: &str, source: &str) -> ImportedSymbol {
        ImportedSymbol {
            local_name: local.to_string(),
            imported_name: local.to_string(),
            source: source.to_string(),
            kind: SymbolKind::Named,
        }
    }

    fn default_import(local: &str, source: &str) -> ImportedSymbol {
        ImportedSymbol {
            local_name: local.to_string(),
            imported_name: local.to_string(),
            source: source.to_string(),
            kind: SymbolKind::Default,
        }
    }

    fn namespace_import(local: &str, source: &str) -> ImportedSymbol {
        ImportedSymbol {
            local_name: local.to_string(),
            imported_name: local.to_string(),
            source: source.to_string(),
            kind: SymbolKind::Namespace,
        }
    }

    /// #369: the wrapper-context section appears exactly when context is
    /// supplied, and an empty context leaves the message byte-identical to
    /// the pre-#369 shape (the cache-safety contract of every optional
    /// section).
    #[test]
    fn test_wrapper_context_section_present_iff_supplied() {
        let agent = FileAnalyzerAgent::new(AgentService::new());
        let guidance = create_test_guidance();
        let file_content = r#"import { apiRequest } from "./GenericFunctions";
const r = await apiRequest.call(this, "GET", "/webhooks", {});"#;

        let without = agent.build_user_message_with_candidates(
            "node.ts",
            file_content,
            &guidance,
            &[],
            &[],
            &HashMap::new(),
            &[],
            &[],
            &[],
        );
        assert!(!without.contains("IMPORTED HTTP WRAPPER DEFINITIONS"));

        let wrapper = vec![
            "--- wrapper module: nodes/GenericFunctions.ts ---\nexport async function apiRequest(method, resource) { /* baseURL: `${credentials.host}/api/v1` */ }".to_string(),
        ];
        let with = agent.build_user_message_with_candidates(
            "node.ts",
            file_content,
            &guidance,
            &[],
            &[],
            &HashMap::new(),
            &[],
            &[],
            &wrapper,
        );
        assert!(with.contains("### IMPORTED HTTP WRAPPER DEFINITIONS"));
        assert!(with.contains("wrapper module: nodes/GenericFunctions.ts"));
        // The section sits in the per-file zone: after the import table,
        // before the file content (cache-prefix invariant).
        let sec = with.find("### IMPORTED HTTP WRAPPER DEFINITIONS").unwrap();
        assert!(with.find("### IMPORT TABLE").unwrap() < sec);
        assert!(sec < with.find("### FILE CONTENT").unwrap());
    }

    /// #372: the postMessage-channel section appears exactly when a Signal-8
    /// candidate (`.postMessage` / `.addEventListener` on a 'message' event)
    /// is among the hints; any other candidate mix leaves the message
    /// byte-identical to the pre-#372 shape (cache-safety contract).
    #[test]
    fn test_postmessage_section_present_iff_candidates_match() {
        let agent = FileAnalyzerAgent::new(AgentService::new());
        let guidance = create_test_guidance();
        let file_content = r#"window.parent.postMessage({ action: 'done' }, '*');"#;

        let http_only = vec![
            "- Candidate span:0-10: Line 1 (span 0-10) fetch [fn: <module>] [path: '/x'] - `fetch('/x')`"
                .to_string(),
        ];
        let without = agent.build_user_message_with_candidates(
            "page.tsx",
            file_content,
            &guidance,
            &http_only,
            &[],
            &HashMap::new(),
            &[],
            &[],
            &[],
        );
        assert!(!without.contains("WEB POSTMESSAGE CHANNEL"));

        let pm = vec![
            "- Candidate span:0-10: Line 1 (span 0-10) window.postMessage [fn: notify] [path: <path unavailable>] - `window.parent.postMessage(`"
                .to_string(),
        ];
        let with = agent.build_user_message_with_candidates(
            "page.tsx",
            file_content,
            &guidance,
            &pm,
            &[],
            &HashMap::new(),
            &[],
            &[],
            &[],
        );
        assert!(with.contains("### WEB POSTMESSAGE CHANNEL"));
        // Listener-shape hints gate it too.
        let listener = vec![
            "- Candidate span:0-10: Line 1 (span 0-10) window.addEventListener [fn: mount] [path: <path unavailable>] - `window.addEventListener('message', handleMessage);`"
                .to_string(),
        ];
        let with_listener = agent.build_user_message_with_candidates(
            "sdk.ts",
            file_content,
            &guidance,
            &listener,
            &[],
            &HashMap::new(),
            &[],
            &[],
            &[],
        );
        assert!(with_listener.contains("### WEB POSTMESSAGE CHANNEL"));
        // Per-file zone: after the import table, before the file content.
        let sec = with.find("### WEB POSTMESSAGE CHANNEL").unwrap();
        assert!(with.find("### IMPORT TABLE").unwrap() < sec);
        assert!(sec < with.find("### FILE CONTENT").unwrap());
    }

    #[test]
    fn test_build_user_message_includes_import_table_and_candidates() {
        let agent = FileAnalyzerAgent::new(AgentService::new());
        let guidance = create_test_guidance();
        let file_content = r#"
import { User } from './types';
const data = await fetch('/api/users').then(resp => resp.json());
"#;

        let candidates = vec!["- Line 3: fetch(...) - `fetch('/api/users')`".to_string()];
        let candidate_contexts: Vec<String> =
            vec![r#"{"line":3,"callee":"fetch","path":"/api/users","fn":"getData"}"#.to_string()];
        let mut imported = HashMap::new();
        imported.insert("User".to_string(), named("User", "./types"));
        imported.insert("useUsers".to_string(), named("useUsers", "../hooks"));

        let message = agent.build_user_message_with_candidates(
            "test.ts",
            file_content,
            &guidance,
            &candidates,
            &candidate_contexts,
            &imported,
            &[],
            &[],
            &[],
        );

        assert!(message.contains("IMPORT TABLE"));
        // Grouped-by-source format: "From './types': User [named]".
        assert!(
            message.contains("From './types': User [named]"),
            "expected grouped import line, got: {message}"
        );
        assert!(message.contains("CANDIDATE TARGETS"));
        assert!(message.contains("Line 3"));
        assert!(message.contains("CANDIDATE CONTEXT"));
        assert!(message.contains("/api/users"));

        // Cache-prefix invariant (Vertex implicit caching): the per-scan-stable
        // guidance blocks must precede every per-file block, so the cacheable
        // request prefix can extend across them. A regression here silently
        // strands triage hints / parsing notes after per-file content, killing
        // the within-scan prefix cache. See the format! comment.
        // Key off the structural `### ` headers, not bare phrases: a phrase like
        // "IMPORT TABLE" can occur inside guidance text or file source, which would
        // make `find` match the wrong offset as the guidance evolves.
        let pos = |needle: &str| {
            message
                .find(needle)
                .unwrap_or_else(|| panic!("section `{needle}` missing from message:\n{message}"))
        };
        let last_stable = pos("### FRAMEWORK-SPECIFIC PARSING NOTES");
        for per_file in [
            "### CANDIDATE TARGETS",
            "### CANDIDATE CONTEXT",
            "### IMPORT TABLE",
            "### FILE CONTENT",
        ] {
            assert!(
                last_stable < pos(per_file),
                "stable guidance must precede per-file `{per_file}` for prefix caching"
            );
        }
    }

    #[test]
    fn build_user_message_includes_graphql_producers_when_hints_present() {
        let agent = FileAnalyzerAgent::new(AgentService::new());
        let guidance = create_test_guidance();
        let file_content = "export function resolveOrder() { return {}; }\n";
        let hints = vec![
            "query order: Order".to_string(),
            "mutation refundOrder: Order!".to_string(),
        ];

        let message = agent.build_user_message_with_candidates(
            "resolvers.ts",
            file_content,
            &guidance,
            &[],
            &[],
            &HashMap::new(),
            &hints,
            &[],
            &[],
        );

        assert!(
            message.contains("### GRAPHQL SCHEMA PRODUCERS (from this repo's SDL)"),
            "expected GraphQL producers section, got:\n{message}"
        );
        assert!(
            message.contains("- query order: Order"),
            "expected formatted producer field line, got:\n{message}"
        );
        assert!(
            message.contains("- mutation refundOrder: Order!"),
            "expected second producer field line, got:\n{message}"
        );
        assert!(
            message.contains("graphql_operations"),
            "expected pointer to the graphql_operations output channel, got:\n{message}"
        );

        // The producer block is repo-global (stable across files), so it must sit
        // in the cacheable front block, before any per-file section.
        let pos = |needle: &str| {
            message
                .find(needle)
                .unwrap_or_else(|| panic!("section `{needle}` missing:\n{message}"))
        };
        assert!(
            pos("### GRAPHQL SCHEMA PRODUCERS (from this repo's SDL)") < pos("### FILE CONTENT"),
            "stable GraphQL producer block must precede per-file content for prefix caching"
        );
    }

    #[test]
    fn build_user_message_omits_graphql_producers_when_hints_empty() {
        let agent = FileAnalyzerAgent::new(AgentService::new());
        let guidance = create_test_guidance();
        let file_content = "const x = 1;\n";

        let message = agent.build_user_message_with_candidates(
            "plain.ts",
            file_content,
            &guidance,
            &[],
            &[],
            &HashMap::new(),
            &[],
            &[],
            &[],
        );

        assert!(
            !message.contains("GRAPHQL SCHEMA PRODUCERS"),
            "GraphQL producers section must be absent when no hints are passed, got:\n{message}"
        );

        // Byte-identity guard (Vertex implicit prefix caching): an empty hint list
        // must contribute ZERO bytes to the assembled message — i.e. the prompt is
        // byte-for-byte what the pre-feature template produced. The pre-feature form
        // joined the triage hints directly to the PARSING NOTES header with a SINGLE
        // newline. A stray blank line here (the `{}\n{}` template bug Copilot flagged)
        // would shift the cacheable prefix and tank the within-scan cache hit rate.
        let triage_hints = &guidance.triage_hints;
        let pre_feature_join = format!(
            "### FRAMEWORK-SPECIFIC HINTS\n{triage_hints}\n### FRAMEWORK-SPECIFIC PARSING NOTES"
        );
        assert!(
            message.contains(&pre_feature_join),
            "empty hints must render the pre-feature single-newline join, got:\n{message}"
        );
        // And explicitly: no doubled blank line where the section would have gone.
        let doubled_join = format!(
            "### FRAMEWORK-SPECIFIC HINTS\n{triage_hints}\n\n### FRAMEWORK-SPECIFIC PARSING NOTES"
        );
        assert!(
            !message.contains(&doubled_join),
            "empty hints must NOT leave a doubled blank line before PARSING NOTES, got:\n{message}"
        );
    }

    /// #268: the consumer mirror of
    /// `build_user_message_includes_graphql_producers_when_hints_present` — a
    /// non-empty `graphql_consumer_hints` list renders the consumer section,
    /// formats each hint line, points at the `graphql_consumer_locates` output
    /// channel, and (being repo-global/stable) sits in the cacheable front
    /// block before any per-file content.
    #[test]
    fn build_user_message_includes_graphql_consumers_when_hints_present() {
        let agent = FileAnalyzerAgent::new(AgentService::new());
        let guidance = create_test_guidance();
        let file_content = "export interface OrderUpdate { id: string }\n";
        let hints = vec!["subscription|orderUpdated @ lib/graphql.ts".to_string()];

        let message = agent.build_user_message_with_candidates(
            "lib/graphql.ts",
            file_content,
            &guidance,
            &[],
            &[],
            &HashMap::new(),
            &[],
            &hints,
            &[],
        );

        assert!(
            message.contains("### GRAPHQL DOCUMENT CONSUMERS WITH NO EXPLICIT RESULT TYPE"),
            "expected GraphQL consumers section, got:\n{message}"
        );
        assert!(
            message.contains("- subscription|orderUpdated @ lib/graphql.ts"),
            "expected formatted consumer hint line, got:\n{message}"
        );
        assert!(
            message.contains("graphql_consumer_locates"),
            "expected pointer to the graphql_consumer_locates output channel, got:\n{message}"
        );

        let pos = |needle: &str| {
            message
                .find(needle)
                .unwrap_or_else(|| panic!("section `{needle}` missing:\n{message}"))
        };
        assert!(
            pos("### GRAPHQL DOCUMENT CONSUMERS WITH NO EXPLICIT RESULT TYPE")
                < pos("### FILE CONTENT"),
            "stable GraphQL consumer block must precede per-file content for prefix caching"
        );
    }

    /// #268 mirror of `build_user_message_omits_graphql_producers_when_hints_empty`:
    /// an empty `graphql_consumer_hints` list must omit the section entirely and
    /// contribute zero bytes, preserving the pre-feature byte-identical prompt.
    #[test]
    fn build_user_message_omits_graphql_consumers_when_hints_empty() {
        let agent = FileAnalyzerAgent::new(AgentService::new());
        let guidance = create_test_guidance();
        let file_content = "const x = 1;\n";

        let message = agent.build_user_message_with_candidates(
            "plain.ts",
            file_content,
            &guidance,
            &[],
            &[],
            &HashMap::new(),
            &[],
            &[],
            &[],
        );

        assert!(
            !message.contains("GRAPHQL DOCUMENT CONSUMERS"),
            "GraphQL consumers section must be absent when no hints are passed, got:\n{message}"
        );

        let triage_hints = &guidance.triage_hints;
        let pre_feature_join = format!(
            "### FRAMEWORK-SPECIFIC HINTS\n{triage_hints}\n### FRAMEWORK-SPECIFIC PARSING NOTES"
        );
        assert!(
            message.contains(&pre_feature_join),
            "empty hints must render the pre-feature single-newline join, got:\n{message}"
        );
    }

    #[test]
    fn test_format_import_table_groups_by_source_and_kind() {
        // Named imports from the same module batch together (NestJS style).
        let mut imports = HashMap::new();
        imports.insert("Get".to_string(), named("Get", "@nestjs/common"));
        imports.insert("Post".to_string(), named("Post", "@nestjs/common"));
        imports.insert(
            "Controller".to_string(),
            named("Controller", "@nestjs/common"),
        );
        imports.insert("Koa".to_string(), default_import("Koa", "koa"));
        imports.insert(
            "express".to_string(),
            namespace_import("express", "express"),
        );

        let out = FileAnalyzerAgent::format_import_table(&imports);
        // @nestjs/common should list all three named imports on one line, sorted.
        assert!(
            out.contains("From '@nestjs/common': Controller, Get, Post [named]"),
            "named imports should group & sort: {out}"
        );
        // Default & namespace annotated explicitly.
        assert!(
            out.contains("From 'koa': Koa [default]"),
            "default import annotation: {out}"
        );
        assert!(
            out.contains("From 'express': express [namespace]"),
            "namespace import annotation: {out}"
        );
    }

    #[test]
    fn test_format_import_table_aliased_named_import() {
        // `import { Foo as Bar } from 'mod'` — show both.
        let mut imports = HashMap::new();
        imports.insert(
            "Bar".to_string(),
            ImportedSymbol {
                local_name: "Bar".to_string(),
                imported_name: "Foo".to_string(),
                source: "mod".to_string(),
                kind: SymbolKind::Named,
            },
        );
        let out = FileAnalyzerAgent::format_import_table(&imports);
        assert!(
            out.contains("From 'mod': Bar (as Foo) [named]"),
            "aliased import should show `local (as imported)`: {out}"
        );
    }

    #[test]
    fn test_format_import_table_empty() {
        let imports = HashMap::new();
        let out = FileAnalyzerAgent::format_import_table(&imports);
        assert!(out.contains("No imports detected"));
    }

    // test_system_message_is_framework_agnostic was deleted: the system
    // prompt now lives in carrick-cloud/lambdas/file-analyzer/system_prompt.txt
    // and is no longer accessible from this Rust crate.
}
