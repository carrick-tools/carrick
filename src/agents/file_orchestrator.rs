//! File-centric orchestrator for processing files using the FileAnalyzerAgent.
//!
//! NOTE: This module is part of a refactoring effort. The public API will be integrated
//! with the main orchestrator in subsequent commits.
#![allow(dead_code)]
//!
//! This orchestrator implements the AST-Gated File-Centric architecture:
//! 1. **Gatekeeper:** Run SWC Scanner to find potential API call sites
//! 2. **Check Relevance:** If no candidates found → SKIP file (Cost: $0)
//! 3. **Context:** Send Full File + Patterns + Candidate Targets to Gemini
//! 4. **Direct Build:** Deserialize JSON response directly into MountGraph structs
//!
//! This approach:
//! - Skips files with no API patterns (zero LLM cost)
//! - Utilizes Gemini's large context window for better alias resolution
//! - Passes AST-detected lines as "Candidate Targets" to ensure 100% recall
//! - Produces deterministic results through strict schema enforcement

use crate::{
    agent_service::AgentService,
    agents::{
        file_analyzer_agent::{
            DataCallResult, EmissionStyle, EndpointResult, FileAnalysisResult, FileAnalyzerAgent,
            MountResult, PubsubOperation,
        },
        framework_guidance_agent::ProtocolGuidance,
    },
    call_base::resolve_call_base,
    cloud_storage::{ManifestRole, ManifestTypeKind},
    config::Config,
    env_alias::{
        EnvAliasExtractor, EnvAliasMap, EnvFallbackMap, EnvSchemaIndex, LiteralBaseMap,
        WholeUrlFallbackMap, exported_env_aliases, merge_imported_env_aliases, module_env_schema,
        resolve_target_env_alias, resolve_target_literal_base, resolve_whole_url_target,
        whole_url_local_default,
    },
    file_based_router::{MethodSource, RoutingConvention, builtin_conventions, derive_route},
    framework_detector::DetectionResult,
    import_bindings::BindingResolver,
    imported_request_member::{
        RequestMember, RequestMemberIndex, ResolvedMember, UnfollowedMemberSites,
        collect_request_members, fold_indexes_with_conflicts,
    },
    local_http_wrapper::LocalWrapperCall,
    mount_graph::{DataFetchingCall, GraphNode, MountEdge, MountGraph, NodeType, ResolvedEndpoint},
    operation::{OperationKey, Protocol},
    parser::parse_file,
    services::type_sidecar::{
        ExtractionConfig, InferKind, InferRequestItem, SymbolRequest, TypeResolutionResult,
        TypeSidecar,
    },
    swc_scanner::{
        CandidateTarget, ControllerRouteBinding, PubsubAnchorOp, RouteDescriptorEndpoint,
        SwcScanner, collect_import_sources, is_producer_route_path, normalize_path_params,
    },
    type_manifest::{
        build_call_site_id, build_manifest_type_alias, build_manifest_type_alias_with_call_id,
        is_http_method, normalize_manifest_method, parse_file_location,
    },
    url_normalizer::UrlNormalizer,
    visitor::{ImportSymbolExtractor, ImportedSymbol, SymbolKind, TypeSymbolExtractor},
    wrapper_request_shape::{self, RequestShapeSignal, WrapperRequestShape},
};
use futures::stream::StreamExt;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use swc_common::{
    SourceMap,
    errors::{ColorConfig, Handler},
    sync::Lrc,
};
use swc_ecma_ast::{
    BinExpr, BinaryOp, BindingIdent, ExportSpecifier, Expr, Lit, ModuleDecl, ModuleItem, Pat, Str,
    Tpl, TsEntityName, TsType, VarDeclarator,
};
use swc_ecma_visit::{Visit, VisitWith};
use tracing::{debug, warn};

/// Complete result of file-centric analysis
#[derive(Debug)]
pub struct FileCentricAnalysisResult {
    /// Per-file analysis results
    pub file_results: HashMap<String, FileAnalysisResult>,
    /// Aggregated mount graph
    pub mount_graph: MountGraph,
    /// Processing statistics
    pub stats: ProcessingStats,
    /// Bundled type definitions (if sidecar was used)
    pub bundled_types: Option<String>,
    /// Type resolution result from sidecar
    pub type_resolution: Option<TypeResolutionResult>,
}

/// Statistics about the file-centric analysis
#[derive(Debug, Default)]
pub struct ProcessingStats {
    pub files_processed: usize,
    pub files_skipped: usize,
    /// Files skipped because SWC found no API candidates (zero-cost skips)
    pub files_skipped_no_candidates: usize,
    /// Files excluded because they could not be parsed. Counted separately
    /// from zero-cost skips: a parse failure silently removes the file's
    /// endpoints from the index. A subset of `files_skipped`.
    pub files_parse_failed: usize,
    /// Files the analyzer was asked about and never answered for (the call
    /// failed after its retries were spent). A subset of `files_skipped`, and
    /// unlike a parse failure it is not a repeatable limitation of the scanner
    /// — it is index loss this run caused. See [`crate::scan_health`].
    pub files_analysis_failed: usize,
    /// Files whose only candidates belong to protocols without a registered
    /// analyze-file prompt (e.g. raw WebSocket constructors). Skipped instead
    /// of being fed to the HTTP prompt, which couldn't classify them.
    pub files_skipped_unrouted_protocol: usize,
    pub total_mounts: usize,
    pub total_endpoints: usize,
    /// Endpoints derived structurally from file-based routing conventions
    /// (Next.js app router, etc.) rather than from a call-site scan. A subset
    /// of `total_endpoints`.
    pub file_based_endpoints: usize,
    /// Endpoints derived deterministically from route-descriptor data
    /// (`{ method, path, handler }` in a registry array) rather than from the
    /// file-analyzer LLM. A subset of `total_endpoints`. See #234.
    pub route_descriptor_endpoints: usize,
    /// Endpoints derived deterministically by joining a route table's bound
    /// path to the controller class the binding resolves to, across files. A
    /// subset of `total_endpoints`. See #580.
    pub class_controller_endpoints: usize,
    /// Pub/sub operations asserted deterministically from the AST and merged in
    /// because the file-analyzer's extraction omitted them (carrick#387). The
    /// anchors themselves are computed for every gated file; only the ones the
    /// LLM missed are counted here.
    pub pubsub_anchor_backfills: usize,
    /// LLM-emitted pub/sub operations dropped because their topic has no
    /// literal witness (string literal or template-literal shape) in the
    /// analyzed file's source (carrick#311). The analyzer occasionally invents
    /// a topic from a wrapper-function NAME (`publishStatusChanged` ->
    /// `status.changed`); the real op lives in the file that holds the literal.
    pub pubsub_phantom_topic_drops: usize,
    /// Outbound calls asserted deterministically from a verb-named request
    /// spec (`client.post({ url: "/v1/things" })`) and merged in because the
    /// file-analyzer's extraction omitted them (#529). A subset of
    /// `total_data_calls`.
    pub request_spec_call_backfills: usize,
    /// Outbound calls resolved through a request wrapper declared in the same
    /// file and merged in because their call sites raise no candidate for the
    /// file-analyzer to answer (carrick#588). A subset of `total_data_calls`.
    pub local_wrapper_call_backfills: usize,
    /// Data calls whose method and target were read off the imported member
    /// they call, rather than left to extraction to infer from the consumer
    /// file (carrick#588). A subset of `total_data_calls`.
    pub imported_member_resolutions: usize,
    /// Outbound calls asserted from the imported member they call and merged
    /// in because extraction returned no row for their site at all
    /// (carrick#623). A subset of `total_data_calls`.
    pub imported_member_backfills: usize,
    /// Outbound calls whose whole URL is read from an environment variable and
    /// merged in because extraction returned no row for their site at all
    /// (carrick#632). A subset of `total_data_calls`.
    pub whole_url_env_backfills: usize,
    /// Whole-URL env-var calls extraction DID answer, whose row was corrected
    /// to what the binding's own AST states (carrick#632). A subset of
    /// `total_data_calls`.
    pub whole_url_env_corrections: usize,
    /// Wrapper-resolved data calls whose HTTP method was corrected from what
    /// extraction gave them to the method their wrapper module hardcodes
    /// (carrick-cloud#386). A subset of `total_data_calls`.
    pub wrapper_method_propagations: usize,
    pub total_data_calls: usize,
    pub errors: Vec<String>,
}

/// A same-repo module that itself performs HTTP and exports a binding — the
/// shape of a request wrapper another file imports (#369/#370).
struct WrapperModule {
    /// The module's source, injected into the importing file's prompt so its
    /// delegating call sites can be emitted with a resolved target.
    snippet: String,
    /// The method (and body presence) every request in the module agrees on,
    /// read off the AST. `None` when the module parameterizes its method, its
    /// requests disagree, or none is readable. See `crate::wrapper_request_shape`.
    request_shape: Option<WrapperRequestShape>,
}

/// Owner assigned to endpoints declared by file location (file-based routing).
/// These routes have no mount chain — their derived path is already absolute —
/// so the owner is a sentinel that matches no mount during path resolution.
const FILE_BASED_ROUTE_OWNER: &str = "__file_based_route__";

/// Sentinel owner for a route-descriptor endpoint whose handler is absent or not
/// a bare identifier. Like `FILE_BASED_ROUTE_OWNER`, it matches no mount during
/// path resolution, so the descriptor's already-absolute path is used as-is.
const ROUTE_DESCRIPTOR_OWNER: &str = "__route_descriptor__";

/// `pattern_matched` tag for endpoints emitted deterministically from
/// route-descriptor data (#234).
const ROUTE_DESCRIPTOR_PATTERN: &str = "route-descriptor";

/// `pattern_matched` tag for endpoints emitted deterministically from a route
/// table that binds a path to an imported controller instance (#580).
const CLASS_CONTROLLER_PATTERN: &str = "class-controller-route";

type EndpointLookup = HashMap<(String, u32), Vec<(String, String)>>;
type DataCallLookup = HashMap<(String, u32), Vec<(String, String, String)>>;

#[derive(Debug, Default)]
struct SymbolTable {
    local_types: HashSet<String>,
    imported_symbols: HashMap<String, ImportedSymbol>,
}

/// Everything one parse of a source file yields for the passes that follow.
///
/// A struct rather than a tuple because two of the fields are the same type —
/// `EnvAliasMap` and `WholeUrlFallbackMap` are both `HashMap<String, String>` — and
/// a positional swap between them would compile and be wrong everywhere at once.
#[derive(Default)]
struct FileSymbols {
    table: SymbolTable,
    /// Local bindings that alias a `process.env` variable (#218).
    env_aliases: EnvAliasMap,
    /// Paths stated by the `??` fallback of a binding holding a whole request
    /// URL (carrick#572).
    whole_url_fallbacks: WholeUrlFallbackMap,
    /// Every `??`/`||` string-literal default an env read in this file was
    /// declared with, ungated (carrick#649). Read for what it says about a
    /// call's base, never to resolve a target.
    env_fallbacks: EnvFallbackMap,
    /// Absolute URL literals the file declares as bases (carrick#627).
    literal_bases: LiteralBaseMap,
    /// The file's own request members, read for the files that import it
    /// (carrick#588).
    request_members: RequestMemberIndex,
}

/// Where one request member is declared (carrick#656).
///
/// Keyed by member NAME across the files a scan analyses, so a name two
/// modules declare is dropped rather than attributed to either. Holds enough
/// to find the member's own request row again once the analysis is in: the
/// `file_results` key of its module, and the line the request sits on.
struct MemberHome {
    path_str: String,
    request_line: u32,
}

/// Reduce a TS type annotation to its primary symbol, stripping the same
/// wrappers `primary_type_symbol` strips (`Promise<User[]>` -> `User`). Arrays
/// and the `Promise`/`Array`/`ReadonlyArray` container generics unwrap to their
/// element; a qualified name (`ns.Foo`) or a builtin has no borrowable symbol.
/// Used to compare a request payload's declared type against the model's
/// emitted response symbol.
fn base_type_symbol(ty: &TsType) -> Option<String> {
    match ty {
        TsType::TsArrayType(arr) => base_type_symbol(&arr.elem_type),
        TsType::TsParenthesizedType(inner) => base_type_symbol(&inner.type_ann),
        TsType::TsTypeRef(type_ref) => {
            let name = match &type_ref.type_name {
                TsEntityName::Ident(id) => id.sym.to_string(),
                TsEntityName::TsQualifiedName(_) => return None,
            };
            if matches!(name.as_str(), "Promise" | "Array" | "ReadonlyArray") {
                return type_ref
                    .type_params
                    .as_ref()
                    .and_then(|p| p.params.first())
                    .and_then(|p| base_type_symbol(p));
            }
            Some(name)
        }
        _ => None,
    }
}

/// Is `expr` a call, or an await/paren/cast wrapping one? Identifies a binding
/// whose value comes FROM a call, so its type annotation is response-side
/// evidence (`const r: AuditEvent = await axios.post(...)`).
fn expr_is_call_like(expr: &Expr) -> bool {
    match expr {
        Expr::Await(a) => expr_is_call_like(&a.arg),
        Expr::Paren(p) => expr_is_call_like(&p.expr),
        Expr::TsAs(a) => expr_is_call_like(&a.expr),
        Expr::TsNonNull(a) => expr_is_call_like(&a.expr),
        Expr::Call(_) => true,
        _ => false,
    }
}

/// The payload expression text as a bare identifier (`event`), or `None` when
/// it is anything else (`{ ... }`, `event.data`, `build()`) — the only shape
/// whose declared type we can resolve from the binding table.
fn payload_bare_ident(text: &str) -> Option<&str> {
    let t = text.trim();
    let mut chars = t.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$');
    if first_ok && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') {
        Some(t)
    } else {
        None
    }
}

/// Does the verbatim call text carry an explicit type-argument list (before the
/// call's argument parens) naming `symbol`? Detects a response generic like
/// `axios.post<AuditEvent>(...)` without re-parsing.
fn call_text_has_type_generic(call_text: &str, symbol: &str) -> bool {
    let head_end = call_text.find('(').unwrap_or(call_text.len());
    let head = &call_text[..head_end];
    match head.find('<') {
        Some(lt) => contains_word(&head[lt..], symbol),
        None => false,
    }
}

/// A data-call target shaped like a transport endpoint (an env-var base, a
/// `process.env` read, an absolute URL, or the analyzer's canonical
/// `ENV_VAR:` form) rather than an operation identity. Superset of the
/// `fold_graphql_transport_calls` shape test, including its quote/backtick
/// trim: the model sometimes emits the target with its source quoting intact
/// (`"https://…/graphql"`, `` `${GQL_URL}/graphql` ``), which must not let the
/// URL escape the prefix checks.
fn is_transport_shaped_target(target: &str) -> bool {
    let t = target.trim().trim_matches(['`', '"', '\'']);
    t.contains("${")
        || t.contains("process.env.")
        || t.starts_with("http://")
        || t.starts_with("https://")
        || t.starts_with("ENV_VAR:")
}

/// Whole-word substring test: `word` appears in `haystack` bounded by
/// non-identifier characters, so `TICKET_QUERY` matches the call text but does
/// not match inside `TICKET_QUERY_V2`.
fn contains_word(haystack: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let bytes = haystack.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let mut search_from = 0;
    while let Some(rel) = haystack[search_from..].find(word) {
        let start = search_from + rel;
        let end = start + word.len();
        let before_ok = start == 0 || !is_ident(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_ident(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        search_from = start + 1;
        if search_from >= bytes.len() {
            break;
        }
    }
    false
}

/// Collects every named type reference appearing at ANY depth inside a TS type
/// (`Response<AuditEvent>` yields `Response` AND `AuditEvent`;
/// `Promise<Wrapper<AuditEvent>[]>` yields all three). Structural and
/// name-agnostic — no wrapper allowlist — so an annotated result binding counts
/// as response evidence for a symbol regardless of which framework envelope
/// wraps it. A qualified name (`api.AuditEvent`) contributes its rightmost
/// ident, matching the shape of an emitted `primary_type_symbol`.
struct TypeRefIdentCollector<'a> {
    syms: &'a mut HashSet<String>,
}

impl Visit for TypeRefIdentCollector<'_> {
    fn visit_ts_type_ref(&mut self, n: &swc_ecma_ast::TsTypeRef) {
        let mut name = &n.type_name;
        loop {
            match name {
                TsEntityName::Ident(id) => {
                    self.syms.insert(id.sym.to_string());
                    break;
                }
                TsEntityName::TsQualifiedName(q) => {
                    self.syms.insert(q.right.sym.to_string());
                    name = &q.left;
                }
            }
        }
        n.visit_children_with(self);
    }
}

/// Collects the AST evidence `suppress_borrowed_request_types` needs from one
/// file in a single walk: every binding's declared primary type (params and
/// typed `const`/`let`), and every type symbol MENTIONED in the annotation of a
/// call-initialized binding (response-side evidence).
#[derive(Default)]
struct BindingTypeCollector {
    /// binding identifier -> its declared primary type symbol.
    binding_types: HashMap<String, String>,
    /// All named type refs (any nesting depth) appearing in the annotation of a
    /// call-initialized binding: `const r: Response<AuditEvent> = await
    /// axios.post(...)` contributes `Response` and `AuditEvent`. Membership is
    /// response-side evidence for that symbol — deliberately mention-based, not
    /// primary-symbol equality, so a framework envelope never hides a real
    /// response annotation (and never via a hardcoded wrapper-name list).
    call_annotated_syms: HashSet<String>,
}

impl Visit for BindingTypeCollector {
    fn visit_binding_ident(&mut self, n: &BindingIdent) {
        if let Some(type_ann) = &n.type_ann
            && let Some(sym) = base_type_symbol(&type_ann.type_ann)
        {
            self.binding_types.insert(n.id.sym.to_string(), sym);
        }
        n.visit_children_with(self);
    }

    fn visit_var_declarator(&mut self, n: &VarDeclarator) {
        if let Pat::Ident(binding) = &n.name
            && let (Some(type_ann), Some(init)) = (&binding.type_ann, n.init.as_deref())
            && expr_is_call_like(init)
        {
            type_ann.type_ann.visit_with(&mut TypeRefIdentCollector {
                syms: &mut self.call_annotated_syms,
            });
        }
        n.visit_children_with(self);
    }
}

/// Collects the per-binding annotation evidence the pub/sub borrow witness
/// checks an explicit `primary_type_symbol` against (#413): for every
/// annotated binding in the file (params and typed `const`/`let`), the set of
/// type symbols MENTIONED at any nesting depth in that binding's annotation.
///
/// Mention-based on purpose, mirroring `call_annotated_syms` in the HTTP
/// sibling: `envelope: Envelope<OrderPlaced>` credits BOTH `Envelope` and
/// `OrderPlaced` to `envelope`, so an explicit anchor that correctly names
/// the INNER contract type of a wrapped payload is never witnessed as a
/// borrow by its own wrapper annotation (the corpus-2 `order.placed`
/// publisher is exactly this shape). Primary-symbol equality would misfire
/// there; membership cannot.
#[derive(Default)]
struct AnnotationMentionsCollector {
    /// binding identifier -> every type symbol mentioned in its annotation.
    /// An annotation mentioning no named type (`payload: string`) yields an
    /// empty set — the binding still counts as annotated.
    mentions_by_binding: HashMap<String, HashSet<String>>,
}

impl Visit for AnnotationMentionsCollector {
    fn visit_binding_ident(&mut self, n: &BindingIdent) {
        if let Some(type_ann) = &n.type_ann {
            let syms = self
                .mentions_by_binding
                .entry(n.id.sym.to_string())
                .or_default();
            type_ann
                .type_ann
                .visit_with(&mut TypeRefIdentCollector { syms });
        }
        n.visit_children_with(self);
    }
}

/// Deterministic borrow witness for a pub/sub explicit anchor (#413): AST
/// evidence that `symbol` structurally cannot be the type of the payload the
/// op's locator names. The pub/sub analogue of
/// `suppress_borrowed_request_types`' binding-type check.
///
/// Fires only when BOTH hold:
/// 1. The payload binding itself is annotated, and that annotation never
///    mentions `symbol` at any depth — the file's own contract for the
///    payload contradicts the emitted anchor. An unannotated payload proves
///    nothing and never witnesses.
/// 2. `symbol` IS mentioned in the annotation of a DIFFERENT binding in the
///    file — the place the model plausibly borrowed it from.
///
/// A qualified symbol (`api.AuditEvent`) is compared by its rightmost ident,
/// matching what `TypeRefIdentCollector` records. No witness means no
/// demotion downstream (`demote_witnessed_borrowed_anchors`), so every
/// failure mode here — unparseable file, destructured payload, missing
/// annotation — fails closed to the current explicit-anchor behavior.
fn pubsub_payload_borrow_witness(
    mentions_by_binding: &HashMap<String, HashSet<String>>,
    payload_ident: &str,
    symbol: &str,
) -> bool {
    let leaf = symbol.rsplit('.').next().unwrap_or(symbol);
    let Some(payload_mentions) = mentions_by_binding.get(payload_ident) else {
        return false;
    };
    if payload_mentions.contains(leaf) {
        return false;
    }
    mentions_by_binding
        .iter()
        .any(|(binding, syms)| binding != payload_ident && syms.contains(leaf))
}

/// Collects the textual witnesses `suppress_phantom_pubsub_topics` checks an
/// LLM-emitted pub/sub topic against (carrick#311): every string-literal value
/// in the file, plus the static-part shape of every composed string (template
/// literal or `+` concatenation chain). A topic is witnessed when it equals a
/// literal (inline topics and same-file const-ref topics) or fits a composed
/// shape's static parts in order (topics like `` `${this.name}:stateChange` ``
/// -> `PollController:stateChange`, which the analyzer resolves from context
/// the AST pre-pass cannot). Witness collection is deliberately lenient — any
/// literal anywhere in the file counts — because the guard is a precision
/// tool: it only needs to reject topics with NO textual basis at all, the
/// invented-from-a-function-name class.
#[derive(Default)]
struct PubsubTopicWitnessCollector {
    /// Every string-literal value in the file, including zero-interpolation
    /// template literals.
    literals: HashSet<String>,
    /// The static parts, in order, of every composed string in the file: the
    /// cooked quasis of a template literal with at least one interpolation,
    /// and the literal operands of a `+` chain containing at least one string
    /// literal (dynamic pieces become the gaps between parts).
    template_patterns: Vec<Vec<String>>,
}

impl PubsubTopicWitnessCollector {
    /// Record a composed-string pattern, unless every static part is empty
    /// (`` `${x}` ``, `a + b`): a fully dynamic composition provides no
    /// textual evidence FOR any particular topic, and recording it would make
    /// every topic vacuously witnessed, defeating the guard (Copilot review
    /// on #395).
    fn push_pattern(&mut self, quasis: Vec<String>) {
        if quasis.iter().any(|q| !q.is_empty()) {
            self.template_patterns.push(quasis);
        }
    }

    fn witnessed(&self, topic: &str) -> bool {
        self.literals.contains(topic)
            || self
                .template_patterns
                .iter()
                .any(|quasis| template_pattern_matches(quasis, topic))
    }
}

impl Visit for PubsubTopicWitnessCollector {
    fn visit_str(&mut self, s: &Str) {
        self.literals.insert(s.value.to_string());
    }

    fn visit_tpl(&mut self, tpl: &Tpl) {
        // Interpolations can contain further literals (`${flag ? 'a' : 'b'}`).
        tpl.visit_children_with(self);
        let cooked: Option<Vec<String>> = tpl
            .quasis
            .iter()
            .map(|q| q.cooked.as_ref().map(|c| c.to_string()))
            .collect();
        // A quasi with no cooked value contains an invalid escape; such a
        // template is not a usable witness.
        let Some(quasis) = cooked else {
            return;
        };
        if tpl.exprs.is_empty() {
            self.literals.insert(quasis.concat());
        } else {
            self.push_pattern(quasis);
        }
    }

    fn visit_bin_expr(&mut self, n: &BinExpr) {
        // Operands can contain further literals and composed strings.
        n.visit_children_with(self);
        if n.op != BinaryOp::Add {
            return;
        }
        // A `+` chain composes a string the same way a template literal does:
        // `'orders.' + kind + '.changed'` witnesses `orders.<anything>.changed`.
        // Record it as the equivalent static-part pattern. Chains with no
        // string-literal operand (numeric arithmetic) are not string witnesses.
        let mut parts: Vec<Option<String>> = Vec::new();
        flatten_concat_parts(&n.left, &mut parts);
        flatten_concat_parts(&n.right, &mut parts);
        if !parts.iter().any(Option::is_some) {
            return;
        }
        let mut quasis = vec![String::new()];
        for part in parts {
            match part {
                Some(text) => quasis
                    .last_mut()
                    .expect("quasis starts non-empty")
                    .push_str(&text),
                None => quasis.push(String::new()),
            }
        }
        // Inner sub-chains are visited again by the traversal and yield
        // strictly more lenient sub-patterns; the redundancy is harmless.
        self.push_pattern(quasis);
    }
}

/// Flatten a `+` expression tree into its ordered operands for the concat
/// witness: a string-literal operand contributes its value, anything dynamic
/// contributes a gap (`None`).
fn flatten_concat_parts(expr: &Expr, parts: &mut Vec<Option<String>>) {
    match expr {
        Expr::Bin(bin) if bin.op == BinaryOp::Add => {
            flatten_concat_parts(&bin.left, parts);
            flatten_concat_parts(&bin.right, parts);
        }
        Expr::Paren(paren) => flatten_concat_parts(&paren.expr, parts),
        Expr::Lit(Lit::Str(s)) => parts.push(Some(s.value.to_string())),
        _ => parts.push(None),
    }
}

/// Glob-style match of a topic against a template literal's static parts:
/// `quasis` are the cooked strings between interpolations, so the topic fits
/// when it starts with the first quasi, ends with the last, and contains the
/// middle ones in order — each interpolation matching any (possibly empty)
/// text. Middles are consumed greedily left to right, which is exact for the
/// non-overlapping shapes topic templates take.
fn template_pattern_matches(quasis: &[String], topic: &str) -> bool {
    let Some((first, rest)) = quasis.split_first() else {
        return false;
    };
    let Some(after_prefix) = topic.strip_prefix(first.as_str()) else {
        return false;
    };
    let Some((last, middle)) = rest.split_last() else {
        // A single quasi means no interpolations: exact match only (already
        // covered by the literal set, kept for correctness).
        return after_prefix.is_empty();
    };
    let mut remaining = after_prefix;
    for quasi in middle {
        match remaining.find(quasi.as_str()) {
            Some(idx) => remaining = &remaining[idx + quasi.len()..],
            None => return false,
        }
    }
    remaining.ends_with(last.as_str())
}

/// A mount-site binding, resolved back to the file that defines it.
///
/// `child_node` is the name the MOUNTING file registered (`sessionsRoutes`);
/// `local_name` is what the defining module calls it (`routes`), when its
/// export table names it at all.
#[derive(Debug, Clone)]
struct MountBinding {
    child_node: String,
    local_name: Option<String>,
}

/// Orchestrates file-centric analysis using the FileAnalyzerAgent.
///
/// This orchestrator implements the AST-Gated architecture:
/// 1. **Gatekeeper:** Use SWC Scanner to find potential API call sites
/// 2. **Check Relevance:** If no candidates → skip file (zero cost)
/// 3. **Context:** Send Full File + Patterns + Candidate Targets to Gemini
/// 4. **Build:** Deserialize response directly into MountGraph
pub struct FileOrchestrator {
    file_analyzer: FileAnalyzerAgent,
    swc_scanner: SwcScanner,
}

impl FileOrchestrator {
    pub fn new(agent_service: AgentService) -> Self {
        Self {
            file_analyzer: FileAnalyzerAgent::new(agent_service),
            swc_scanner: SwcScanner::new(),
        }
    }

    /// Run AST-gated file-centric analysis on all provided files.
    ///
    /// **AST-Gated Architecture:**
    /// 1. Run SWC Scanner on each file to find potential API call sites
    /// 2. If no candidates found → SKIP file (zero LLM cost)
    /// 3. If candidates exist → Send Full File + Patterns + Candidate Hints to Gemini
    /// 4. Merge results into MountGraph
    ///
    /// # Arguments
    /// * `files` - List of file paths to analyze
    /// * `guidance` - Framework-specific patterns for detection
    /// * `framework_detection` - Framework detection results (used for type scrubbing)
    ///
    /// # Returns
    /// A `FileCentricAnalysisResult` containing per-file results and aggregated graph.
    #[allow(clippy::too_many_arguments)]
    pub async fn analyze_files(
        &self,
        files: &[PathBuf],
        guidance: &ProtocolGuidance,
        framework_detection: &DetectionResult,
        // Root for file-based route derivation: the SERVICE directory when
        // carrick.json declares one, else the repo root. Convention root globs
        // (`app`, `src/app`, …) are matched against paths relative to THIS.
        service_root: &Path,
        // Package names the service's own package.json declares. Second input to
        // the routing-convention bootstrap, so a file-routed service is
        // recognized from its manifest even when framework detection names only
        // the HTTP server it runs behind.
        dependency_names: &[String],
        graphql_producer_hints: &crate::graphql::GraphqlProducerHints,
        graphql_consumer_hints: &crate::graphql::GraphqlConsumerHints,
        normalizer: &UrlNormalizer,
    ) -> Result<FileCentricAnalysisResult, Box<dyn std::error::Error>> {
        debug!("=== AST-GATED FILE-CENTRIC ORCHESTRATOR ===");
        debug!("Processing {} files with SWC gatekeeper", files.len());

        // Per-protocol prompt routing: each protocol with a registered LLM
        // pass analyzes only its own candidates, so prompts stay focused.
        // HTTP is the only routed protocol today.
        let guidance = guidance
            .get(&Protocol::Http)
            .ok_or("missing HTTP guidance: guidance map must contain the http protocol")?;

        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        let mut stats = ProcessingStats::default();
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Auto, true, false, Some(cm.clone()));

        // Routing conventions for file-based routes (Next.js app/pages router,
        // etc.). Empty when neither the detected frameworks nor the declared
        // dependencies name a convention-bearing framework, in which case the
        // file-based pass below is a no-op.
        let conventions = builtin_conventions(&framework_detection.frameworks, dependency_names);
        debug!(
            "File-based routing conventions active: {:?}",
            conventions.iter().map(|c| &c.name).collect::<Vec<_>>()
        );

        // A file that passed the SWC gatekeeper and is ready for the (expensive) LLM call.
        // The CPU-bound preprocessing (read, scan, symbol table) is done serially up front;
        // the LLM calls themselves are then dispatched concurrently.
        struct PendingFile {
            path_str: String,
            content: String,
            candidate_hints: Vec<String>,
            candidate_contexts: Vec<String>,
            candidate_map: HashMap<String, CandidateTarget>,
            symbol_table: SymbolTable,
            /// `local const -> process.env name` bindings (e.g.
            /// `ORDERS_BASE -> ORDERS_SERVICE_URL`). Used after the LLM pass to
            /// rewrite call targets that interpolate an env-var base URL aliased
            /// through a local const, so the real env-var name reaches
            /// classification and cross-repo matching. See `crate::env_alias`.
            env_alias_map: EnvAliasMap,
            /// Paths stated by the `??` fallback of a binding that holds a
            /// WHOLE request URL read from an environment variable
            /// (carrick#572). Per-file: the shape is a local const passed
            /// straight to a request, never an imported config property.
            whole_url_fallbacks: WholeUrlFallbackMap,
            /// Every `??`/`||` string-literal default an env read in this file
            /// was declared with (carrick#649). Per-file for the same reason
            /// `whole_url_fallbacks` is, and read by the base-stamping pass
            /// only — it resolves no target and changes no key.
            env_fallbacks: EnvFallbackMap,
            /// Absolute URL literals the file declares as bases, for a target
            /// that interpolates one as its leading segment (carrick#627).
            /// Per-file for the same reason as `whole_url_fallbacks`: the shape is
            /// a module-level const of this file, read at its own call sites.
            literal_bases: LiteralBaseMap,
            /// Endpoints derived from file-based routing conventions, merged in
            /// after the LLM pass. Empty for non-route files.
            route_endpoints: Vec<EndpointResult>,
            /// Endpoints derived deterministically from route-descriptor data
            /// (`{ method, path, handler }`), merged in after the LLM pass. The
            /// LLM ignores route-as-data, so these are the authoritative source
            /// for such endpoints. Empty for files with no route descriptors.
            descriptor_endpoints: Vec<EndpointResult>,
            /// Repo-global GraphQL producer hint lines (Stage B2), injected into
            /// the user message so the model can link resolver functions in this
            /// file to schema fields. Identical for every file; cloned per-pending
            /// so the concurrent dispatch closure owns its copy. Empty for repos
            /// with no SDL producers.
            graphql_producer_hints: Vec<String>,
            /// Repo-global GraphQL consumer hint lines (#268), injected into the
            /// user message so the model can locate a co-located result type for
            /// a document with no explicit call-site generic. Identical for every
            /// file; cloned per-pending so the concurrent dispatch closure owns
            /// its copy. Empty for repos with no unanchored GraphQL consumers.
            graphql_consumer_hints: Vec<String>,
            /// Source of same-repo HTTP wrapper modules this file imports
            /// (#369 — cross-file wrapper-site resolution). Injected into the
            /// user message so call sites of an imported wrapper can be emitted
            /// as resolved data calls. Empty for files with no such imports.
            wrapper_context: Vec<String>,
            /// The request shape every wrapper module behind `wrapper_context`
            /// agrees on (carrick-cloud#386): the literal HTTP method, and
            /// whether the request carries a body. `None` — the common case —
            /// whenever the wrappers parameterize the method, disagree, or this
            /// file imports none. Propagated onto the resolved call sites after
            /// the LLM pass, because the delegating site itself carries no
            /// method and would otherwise default to GET.
            wrapper_request_shape: Option<WrapperRequestShape>,
            /// Pub/sub operations asserted deterministically from the AST
            /// (carrick#387), merged in after the LLM pass so an extraction
            /// omission cannot lose them. Empty when Signal 7's gates are off.
            pubsub_anchor_ops: Vec<PubsubAnchorOp>,
            /// Outbound calls resolved through a request wrapper declared in
            /// this same file (carrick#588), merged in after the LLM pass.
            /// Their sites raise no candidate, so without this the endpoints
            /// they reach are absent from the index entirely.
            local_wrapper_calls: Vec<LocalWrapperCall>,
            /// This file's OWN request members, keyed by name (carrick#588).
            /// Read for the files that import it, never for itself.
            request_members: RequestMemberIndex,
            /// Call sites whose callee names a request member of an imported
            /// same-repo module, keyed by the site's call-expression start
            /// offset (carrick#588). The member states the whole request, so
            /// its method and URL are applied to the site after the LLM pass.
            /// Empty for files that import no such module.
            resolved_members: HashMap<u32, ResolvedMember>,
            /// Call sites in this file that named an indexed request member
            /// and did NOT resolve to it (carrick#656), as (span, member
            /// name). Aggregated per member once every file's join is in, and
            /// stamped onto the rows that member DID produce, so a consumer
            /// listing can state what the join could not follow.
            unresolved_member_sites: Vec<(u32, String)>,
        }

        /// A zero-candidate file whose skip decision is deferred until the
        /// repo's wrapper modules are known (#369): if it imports a same-repo
        /// module that performs HTTP, it is rescued into the LLM pass with
        /// that wrapper's source as context; otherwise it is skipped exactly
        /// as before. Content is deliberately NOT retained — most files in a
        /// repo land here, so holding their bodies would spike peak memory to
        /// roughly the repo's source size; the rare rescued file is re-read.
        struct DeferredZeroCandidate {
            path_str: String,
            file_path: PathBuf,
            import_sources: Vec<String>,
        }

        // PHASE 1 (serial, CPU-bound): run the SWC gatekeeper on every file and build the
        // work list of files that actually need an LLM call. Zero-cost skips are recorded here.
        let mut pending: Vec<PendingFile> = Vec::new();
        let mut deferred_zero_candidates: Vec<DeferredZeroCandidate> = Vec::new();
        // Route tables that bind a path to an imported handler (#580 part b).
        // Collected here, where the file's content is already in hand, and
        // resolved after the whole pass: the endpoints belong to the CONTROLLER
        // modules, whose own results are not final until then.
        let mut controller_route_bindings: Vec<(PathBuf, Vec<ControllerRouteBinding>)> = Vec::new();
        // carrick#656, filled per file and read once every file is in.
        // `member_deficits` counts, per member name, the sites that named it
        // and did not resolve to it; `resolved_member_rows` says which spans in
        // which file the join DID resolve, and to which member, so the count
        // can be stamped on exactly those rows.
        let mut member_deficits: HashMap<String, u32> = HashMap::new();
        let mut resolved_member_rows: HashMap<String, HashMap<u32, String>> = HashMap::new();
        // What the repo's validation schemas declare about the environment
        // (carrick#649), folded across every parseable file. Repo-wide because
        // an environment variable is process-global and the file declaring the
        // schema is almost never the file making the call — and because such a
        // file usually raises no HTTP candidate at all, so the per-file maps
        // the analyzed files carry would never see it.
        let mut env_schema = EnvSchemaIndex::default();
        for file_path in files {
            let path_str = file_path.to_string_lossy().to_string();

            // Read file content
            let content = match std::fs::read_to_string(file_path) {
                Ok(c) => c,
                Err(e) => {
                    stats
                        .errors
                        .push(format!("Failed to read {}: {}", path_str, e));
                    stats.files_skipped += 1;
                    continue;
                }
            };

            // Skip empty files
            if content.trim().is_empty() {
                debug!("Skipping empty file: {}", path_str);
                stats.files_skipped += 1;
                continue;
            }

            // STEP 1: Run SWC Scanner (Gatekeeper). Pass the LLM-detected
            // data-fetching packages so import-based recall uses detection's
            // decision rather than a hardcoded package list.
            let scan_result = self.swc_scanner.scan_content(
                file_path,
                &content,
                &framework_detection.data_fetchers,
                &framework_detection.messaging_clients,
            );

            // A parse failure excludes the whole file (and any endpoints in
            // it) from the index — surface it instead of letting it look like
            // a healthy file with no API patterns.
            if scan_result.parse_failed {
                warn!(
                    "Failed to parse {} — file excluded from analysis; any endpoints in it \
                     will be missing from the index",
                    path_str
                );
                stats.errors.push(format!("Parse failure: {}", path_str));
                stats.files_skipped += 1;
                stats.files_parse_failed += 1;
                // Store empty result so incremental cache knows this file was processed
                file_results.insert(path_str, FileAnalysisResult::default());
                continue;
            }

            // Environment-variable declarations (carrick#649). Collected here,
            // before every skip branch below, because the file that declares
            // the env schema is typically a config module with no route and no
            // call — exactly the file the candidate-less skip drops.
            //
            // Pre-filtered on the source containing a `.optional()` or
            // `.default(` call, which any such declaration must, so the extra
            // parse is paid only by files that could carry one. Same shape as
            // the controller-route pre-check below.
            if (content.contains(".optional()") || content.contains(".default("))
                && let Some(module) = parse_file(file_path, &cm, &handler)
            {
                env_schema.merge_module(module_env_schema(&module));
            }

            // Pub/sub Part B: a pub/sub-only file (e.g. NATS `nc.publish(...)`)
            // produces zero SWC candidates, so the zero-candidate skip below would
            // drop it before the file-analyzer ever sees its publish/subscribe
            // idioms. Force-analyze it when it imports a package the cloud
            // /framework-detect step flagged as a messaging client. INERT today:
            // `messaging_clients` is empty until that cloud prompt+schema deploys,
            // so this never fires and current behavior is unchanged. socket.io is
            // not a messaging client, so socket files are never caught here.
            let imports_messaging_client = Self::imports_messaging_client(
                &scan_result.import_sources,
                &framework_detection.messaging_clients,
            );

            // Protocol dispatch: the HTTP analyze-file prompt only ever sees
            // HTTP candidates. Candidates of protocols without a registered
            // prompt (raw WebSocket/EventSource constructors today) are set
            // aside; a file with only those is skipped, not sent to a prompt
            // that has no instructions for them.
            let (http_candidates, unrouted_candidates): (Vec<_>, Vec<_>) = scan_result
                .candidates
                .into_iter()
                .partition(|candidate| candidate.protocol == Protocol::Http);

            // File-based routing: routes declared by file location (Next.js app
            // router, etc.) have no call-site candidate — the endpoint *is* the
            // exported handler declaration. The path comes from the layout and the
            // methods from exported handler names; both are invisible to a
            // call-site scan, so they are derived deterministically here.
            let route_endpoints = if conventions.is_empty() {
                Vec::new()
            } else {
                let rel_path = file_path.strip_prefix(service_root).unwrap_or(file_path);
                Self::file_based_endpoints(
                    &self.swc_scanner,
                    rel_path,
                    file_path,
                    &content,
                    &conventions,
                )
            };

            // Route-descriptor routes: a route declared as data
            // (`{ method, path, handler }` in a registry array) is fully
            // structural — method, path, and handler owner are literals the
            // file-analyzer prompt ignores (it only matches framework-call
            // patterns). Emit it deterministically (#234) instead of relying on
            // the LLM. The recall-boost candidate the scanner also raised for
            // such an object is redundant once the route is owned here, so it is
            // dropped from `http_candidates` below (matched by span): a file whose
            // only candidates are deterministically-owned descriptors then skips
            // the LLM entirely, like a file-based route.
            let descriptor_endpoints =
                Self::route_descriptor_endpoints(&self.swc_scanner, file_path, &content);
            let descriptor_spans: HashSet<(u32, u32)> = descriptor_endpoints
                .iter()
                .filter_map(|e| Some((e.call_expression_span_start?, e.call_expression_span_end?)))
                .collect();
            let http_candidates: Vec<_> = http_candidates
                .into_iter()
                .filter(|c| !descriptor_spans.contains(&(c.span_start, c.span_end)))
                .collect();

            // Class-controller routes (#580 part b): a route table binding a
            // literal path to an imported handler. Collected before any of the
            // skip branches below, because a route table is usually a file with
            // no candidates of its own — the `router(...)` calls are plain
            // function calls, not framework method calls. Resolution needs
            // every file's result, so it happens after the pass; only the
            // bindings are held here, not the content.
            //
            // Pre-filtered on the source containing a string literal that
            // opens with `/`, which every such binding must (the path is the
            // first argument and it is a plain string literal). A necessary
            // condition, checked without parsing, so the extra parse is paid
            // only by files that could possibly carry a route table.
            if content.contains("\"/") || content.contains("'/") {
                let bindings = self
                    .swc_scanner
                    .controller_route_bindings(file_path, &content);
                if !bindings.is_empty() {
                    controller_route_bindings.push((file_path.clone(), bindings));
                }
            }

            // GraphQL resolver routing (Stage B2): a resolver file is loose
            // exported functions with no HTTP route candidate, so the
            // candidate-less skip below would drop it before the file-analyzer
            // ever sees it — and without the SDL producer context it couldn't
            // link a resolver to its field anyway. Rescue it from the skip when
            // ALL of: this repo has SDL producers, the file is co-located with
            // the schema (under an SDL scan root), and it has at least one
            // exported binding (resolver-shaped). Scoped this tightly so only
            // schema-adjacent resolver files reach the LLM, not every
            // exported-function file in the repo. The injected GRAPHQL SCHEMA
            // PRODUCERS section gives the model the field list to link against.
            let is_graphql_resolver_file = !graphql_producer_hints.is_empty()
                && graphql_producer_hints.file_within_scan_roots(file_path)
                // Cheap `export` substring pre-check before the expensive
                // `exported_handlers` SWC reparse: `scan_content` (Step 1) already
                // parsed this file, and a resolver file must contain at least one
                // `export`. `&&` short-circuits, so the reparse runs only when the
                // keyword is present — the common no-exports file avoids a second
                // parse entirely. (Reusing the Step-1 parse directly would mean
                // threading exported-handler data through `ScanResult`, which it
                // does not currently carry; the substring guard is the cheap win.)
                && content.contains("export")
                && !self
                    .swc_scanner
                    .exported_handlers(file_path, &content)
                    .is_empty();

            // GraphQL consumer routing (#268): a file that only co-locates a
            // GraphQL document's result type (no `fetch`/`axios`/etc. HTTP call
            // shape) raises no HTTP candidate, so the candidate-less skip below
            // would drop it before the file-analyzer ever sees it. Rescue it
            // when this exact file was recorded in `graphql_consumer_hints` —
            // simpler than the producer's scan-root containment check: a
            // consumer's located type has no fixed directory to scope to, so
            // exact path membership is both correct and cheap (no re-parse).
            let is_graphql_consumer_file = !graphql_consumer_hints.is_empty()
                && graphql_consumer_hints.file_has_hint(file_path);

            // STEP 2: Check Relevance - if there are no candidates for a routed
            // protocol, SKIP the (expensive) LLM call. File-based route and
            // route-descriptor endpoints are still recorded: they're derived
            // structurally and need no LLM.
            if http_candidates.is_empty() {
                let structural_endpoints: Vec<EndpointResult> = route_endpoints
                    .iter()
                    .cloned()
                    .chain(descriptor_endpoints.iter().cloned())
                    .collect();
                if !structural_endpoints.is_empty() {
                    debug!(
                        "Structural route(s) (no call-site candidates): {} [{} file-based, {} route-descriptor]",
                        path_str,
                        route_endpoints.len(),
                        descriptor_endpoints.len()
                    );
                    stats.total_endpoints += structural_endpoints.len();
                    stats.file_based_endpoints += route_endpoints.len();
                    stats.route_descriptor_endpoints += descriptor_endpoints.len();
                    file_results.insert(
                        path_str,
                        FileAnalysisResult {
                            endpoints: structural_endpoints,
                            ..Default::default()
                        },
                    );
                    continue;
                } else if is_graphql_resolver_file {
                    // Fall through to the LLM pass with empty HTTP candidates:
                    // the file-analyzer reads the producer-field context and the
                    // file content to emit `graphql_operations`.
                    debug!(
                        "Routed GraphQL resolver file (no HTTP candidates): {}",
                        path_str
                    );
                } else if is_graphql_consumer_file {
                    // Fall through to the LLM pass with empty HTTP candidates:
                    // the file-analyzer reads the consumer-hint context and the
                    // file content to emit `graphql_consumer_locates` (#268).
                    debug!(
                        "Routed GraphQL consumer file (no HTTP candidates): {}",
                        path_str
                    );
                } else if imports_messaging_client {
                    // Pub/sub Part B: this file imports a cloud-detected
                    // messaging-client package but raised no HTTP candidate (its
                    // publish/subscribe calls are invisible to the call-site
                    // scanner). Fall through to the file-analyzer so its pub/sub
                    // idiom-teaching can extract the operations. INERT until the
                    // cloud /framework-detect step populates `messaging_clients`.
                    debug!(
                        "Force-analyzing messaging-client file (no HTTP candidates): {}",
                        path_str
                    );
                } else if unrouted_candidates.is_empty() {
                    // Defer the skip decision (#369): if this file imports a
                    // same-repo module that performs HTTP (a request wrapper),
                    // it is rescued into the LLM pass after the wrapper map is
                    // built below; otherwise it is skipped exactly as before.
                    deferred_zero_candidates.push(DeferredZeroCandidate {
                        path_str,
                        file_path: file_path.clone(),
                        import_sources: scan_result.import_sources,
                    });
                    continue;
                } else {
                    debug!(
                        "Skipped (only unrouted-protocol candidates): {} [{} candidate(s)]",
                        path_str,
                        unrouted_candidates.len()
                    );
                    stats.files_skipped += 1;
                    stats.files_skipped_unrouted_protocol += 1;
                    file_results.insert(path_str, FileAnalysisResult::default());
                    continue;
                }
            }

            debug!(
                "Analyzing: {} [{} HTTP candidate(s), {} unrouted]",
                path_str,
                http_candidates.len(),
                unrouted_candidates.len()
            );

            // STEP 3: Prepare Candidate Targets as hints for the LLM
            let candidate_hints: Vec<String> =
                http_candidates.iter().map(|c| c.format_hint()).collect();
            let candidate_contexts: Vec<String> = http_candidates
                .iter()
                .map(|c| serde_json::to_string(c).unwrap_or_default())
                .collect();
            let candidate_map: HashMap<String, CandidateTarget> = http_candidates
                .iter()
                .map(|candidate| (candidate.candidate_id.clone(), candidate.clone()))
                .collect();

            let symbols = Self::extract_symbol_table(file_path, &cm, &handler);

            pending.push(PendingFile {
                path_str,
                content,
                candidate_hints,
                candidate_contexts,
                candidate_map,
                symbol_table: symbols.table,
                env_alias_map: symbols.env_aliases,
                whole_url_fallbacks: symbols.whole_url_fallbacks,
                env_fallbacks: symbols.env_fallbacks,
                literal_bases: symbols.literal_bases,
                route_endpoints,
                descriptor_endpoints,
                graphql_producer_hints: graphql_producer_hints.lines.clone(),
                graphql_consumer_hints: graphql_consumer_hints.lines.clone(),
                wrapper_context: Vec::new(),
                wrapper_request_shape: None,
                pubsub_anchor_ops: scan_result.pubsub_anchor_ops,
                local_wrapper_calls: scan_result.local_wrapper_calls,
                request_members: symbols.request_members,
                resolved_members: HashMap::new(),
                unresolved_member_sites: Vec::new(),
            });
        }

        // PHASE 1b (#369 — cross-file wrapper-site resolution). Wrapper map:
        // same-repo modules that themselves perform HTTP (≥1 HTTP candidate)
        // AND export at least one binding — the shape of a request wrapper
        // another file would import. Keyed by canonical file path. The whole
        // (size-capped) file is the context snippet: wrappers are small client
        // modules, and slicing exact function spans buys little over the cap.
        const WRAPPER_SNIPPET_MAX: usize = 4_000;
        let mut wrapper_map: HashMap<PathBuf, WrapperModule> = HashMap::new();
        for pf in &pending {
            // Rescued files (graphql/messaging fall-throughs) carry no HTTP
            // candidates and are not wrapper material.
            if pf.candidate_hints.is_empty() {
                continue;
            }
            let path = Path::new(&pf.path_str);
            if self
                .swc_scanner
                .exported_handlers(path, &pf.content)
                .is_empty()
            {
                continue; // nothing importable
            }
            let Ok(canonical) = path.canonicalize() else {
                continue;
            };
            let mut snippet = format!("--- wrapper module: {} ---\n", pf.path_str);
            if pf.content.len() > WRAPPER_SNIPPET_MAX {
                let mut end = WRAPPER_SNIPPET_MAX;
                while end > 0 && !pf.content.is_char_boundary(end) {
                    end -= 1;
                }
                snippet.push_str(&pf.content[..end]);
                snippet.push_str("\n// (truncated)");
            } else {
                snippet.push_str(&pf.content);
            }
            // The method the module's own request calls fix, read off the AST
            // candidates the gatekeeper already raised for it
            // (carrick-cloud#386). `None` whenever the module parameterizes its
            // method or its requests disagree, in which case the delegating
            // site keeps whatever extraction gave it.
            let request_shape = wrapper_request_shape::fold_module(
                pf.candidate_map.values().map(|c| &c.request_shape),
            );
            wrapper_map.insert(
                canonical,
                WrapperModule {
                    snippet,
                    request_shape,
                },
            );
        }

        // Attach wrapper context to files that import a wrapper module via a
        // RELATIVE specifier (v1 scope: `./`/`../` only — tsconfig path
        // aliases would need sidecar-grade resolution).
        // Re-export specifiers per module, memoized across every importer in the
        // repo (#472): the follow only runs on a `wrapper_map` miss, which is the
        // common case, so without this a scan would re-parse the same barrels
        // once per importing file.
        // The request members a module declares, keyed by canonical path
        // (carrick#588), filled on demand as importers ask for it. Deliberately
        // NOT the `wrapper_map` set: that one is gated on the module raising an
        // HTTP candidate, and a client class whose requests go through a helper
        // raises none, which is exactly the module a consumer needs read for
        // it. Seeded from the files already parsed for their symbol tables, so
        // the on-demand parse only runs for a module no analyzed file was.
        let mut member_cache: HashMap<PathBuf, RequestMemberIndex> = HashMap::new();
        for pf in &pending {
            if let Ok(canonical) = Path::new(&pf.path_str).canonicalize() {
                member_cache.insert(canonical, pf.request_members.clone());
            }
        }

        let mut reexport_cache: HashMap<PathBuf, Vec<String>> = HashMap::new();
        // The same-repo modules each module imports, resolved, memoized across
        // every importer that reaches it: the second ring of the member join
        // below reads it once per module, not once per consumer.
        let mut import_cache: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        {
            for pf in &mut pending {
                let importer = Path::new(&pf.path_str).to_path_buf();
                let self_canon = importer.canonicalize().ok();
                let mut seen: HashSet<PathBuf> = HashSet::new();
                let mut matched: Vec<PathBuf> = Vec::new();
                // Where each imported local name comes from, so a member's
                // receiver can be checked against the module that declared it.
                // `None` is a package import: a real binding, no same-repo
                // module behind it.
                let mut import_owners: HashMap<String, Option<PathBuf>> = HashMap::new();
                for (local_name, symbol) in &pf.symbol_table.imported_symbols {
                    import_owners.insert(
                        local_name.clone(),
                        Self::resolve_relative_import(&importer, &symbol.source),
                    );
                }
                for symbol in pf.symbol_table.imported_symbols.values() {
                    let Some(resolved) = Self::resolve_relative_import(&importer, &symbol.source)
                    else {
                        continue;
                    };
                    if self_canon.as_ref() == Some(&resolved) || !seen.insert(resolved.clone()) {
                        continue;
                    }
                    if wrapper_map.is_empty() {
                        continue;
                    }
                    matched.extend(Self::wrapper_modules_behind(
                        &resolved,
                        self_canon.as_ref(),
                        &wrapper_map,
                        &mut reexport_cache,
                        &cm,
                        &handler,
                    ));
                }
                // `imported_symbols` is a HashMap, so sort before materializing
                // the prompt context: identical inputs must yield identical
                // wrapper context.
                matched.sort();
                matched.dedup();
                pf.wrapper_request_shape = wrapper_request_shape::fold_wrappers(
                    matched
                        .iter()
                        .filter_map(|path| wrapper_map.get(path))
                        .map(|module| module.request_shape.as_ref()),
                );
                pf.wrapper_context = matched
                    .iter()
                    .filter_map(|path| wrapper_map.get(path).map(|m| m.snippet.clone()))
                    .collect();
                // Members reached directly by a relative import, plus those
                // behind a re-export barrel the wrapper pass already followed,
                // and then one hop further: the modules those import. A
                // factory that constructs the client and hands it back inside
                // a record is the common shape (carrick#655): the consumer
                // imports the factory's module and never the client's, so the
                // client is two hops away and the nearer ring declares no
                // member for the site. Only a file with candidates can have a
                // site to resolve, so a rescued or candidate-less file
                // triggers no parse.
                if !pf.candidate_map.is_empty() {
                    let nearest: BTreeSet<PathBuf> =
                        seen.iter().chain(matched.iter()).cloned().collect();
                    let mut further: BTreeSet<PathBuf> = BTreeSet::new();
                    // What each module the file imports imports in turn. The
                    // second ring is built out of it, and so is the test that
                    // decides whether a declined receiver is a client this
                    // module could be holding (carrick#656).
                    let mut receiver_imports: HashMap<PathBuf, BTreeSet<PathBuf>> = HashMap::new();
                    for path in &nearest {
                        let targets = import_cache
                            .entry(path.clone())
                            .or_insert_with(|| Self::relative_import_targets(path, &cm, &handler));
                        receiver_imports.insert(path.clone(), targets.iter().cloned().collect());
                        for target in targets.iter() {
                            if self_canon.as_ref() == Some(target) || nearest.contains(target) {
                                continue;
                            }
                            further.insert(target.clone());
                        }
                    }
                    let mut rings: Vec<Vec<(PathBuf, RequestMemberIndex)>> = Vec::new();
                    for ring in [&nearest, &further] {
                        let mut indexes: Vec<(PathBuf, RequestMemberIndex)> = Vec::new();
                        for path in ring {
                            if !member_cache.contains_key(path) {
                                let index = parse_file(path, &cm, &handler)
                                    .map(|module| collect_request_members(&module, &cm))
                                    .unwrap_or_default();
                                member_cache.insert(path.clone(), index);
                            }
                            if let Some(index) = member_cache.get(path) {
                                indexes.push((path.clone(), index.clone()));
                            }
                        }
                        rings.push(indexes);
                    }
                    // The join's outcome, and the sites it declined
                    // (carrick#656) — both come back from the walk, because
                    // the module a declined site named is only known there.
                    (pf.resolved_members, pf.unresolved_member_sites) =
                        Self::resolve_imported_members(
                            &pf.candidate_map,
                            rings,
                            &import_owners,
                            &receiver_imports,
                        );
                }
            }
        }

        // Rescue or finalize the deferred zero-candidate skips: a file that
        // imports a wrapper module is force-analyzed with the wrapper's source
        // as context (its call sites are the repo's real outbound calls);
        // everything else is skipped exactly as before this pass existed.
        for deferred in deferred_zero_candidates {
            let mut seen: HashSet<PathBuf> = HashSet::new();
            let mut matched: Vec<PathBuf> = Vec::new();
            if !wrapper_map.is_empty() {
                let self_canon = deferred.file_path.canonicalize().ok();
                for spec in &deferred.import_sources {
                    let Some(resolved) = Self::resolve_relative_import(&deferred.file_path, spec)
                    else {
                        continue;
                    };
                    if self_canon.as_ref() == Some(&resolved) || !seen.insert(resolved.clone()) {
                        continue;
                    }
                    matched.extend(Self::wrapper_modules_behind(
                        &resolved,
                        self_canon.as_ref(),
                        &wrapper_map,
                        &mut reexport_cache,
                        &cm,
                        &handler,
                    ));
                }
                matched.sort();
                matched.dedup();
            }
            let rescued_shape = wrapper_request_shape::fold_wrappers(
                matched
                    .iter()
                    .filter_map(|path| wrapper_map.get(path))
                    .map(|module| module.request_shape.as_ref()),
            );
            let ctx: Vec<String> = matched
                .iter()
                .filter_map(|path| wrapper_map.get(path).map(|m| m.snippet.clone()))
                .collect();
            if ctx.is_empty() {
                debug!(
                    "Skipped (no API patterns): {} [0 candidates]",
                    deferred.path_str
                );
                stats.files_skipped += 1;
                stats.files_skipped_no_candidates += 1;
                // Store empty result so incremental cache knows this file was processed
                file_results.insert(deferred.path_str, FileAnalysisResult::default());
                continue;
            }
            debug!(
                "Force-analyzing wrapper-importing file (no HTTP candidates): {}",
                deferred.path_str
            );
            // Re-read the rescued file: content was not retained at defer time
            // (memory), and the file was readable moments ago in this pass.
            let Ok(content) = std::fs::read_to_string(&deferred.file_path) else {
                warn!(
                    "Wrapper-importing file became unreadable, skipping: {}",
                    deferred.path_str
                );
                stats.files_skipped += 1;
                file_results.insert(deferred.path_str, FileAnalysisResult::default());
                continue;
            };
            let symbols = Self::extract_symbol_table(&deferred.file_path, &cm, &handler);
            pending.push(PendingFile {
                path_str: deferred.path_str,
                content,
                candidate_hints: Vec::new(),
                candidate_contexts: Vec::new(),
                candidate_map: HashMap::new(),
                symbol_table: symbols.table,
                env_alias_map: symbols.env_aliases,
                whole_url_fallbacks: symbols.whole_url_fallbacks,
                env_fallbacks: symbols.env_fallbacks,
                literal_bases: symbols.literal_bases,
                route_endpoints: Vec::new(),
                descriptor_endpoints: Vec::new(),
                graphql_producer_hints: graphql_producer_hints.lines.clone(),
                graphql_consumer_hints: graphql_consumer_hints.lines.clone(),
                wrapper_context: ctx,
                wrapper_request_shape: rescued_shape,
                // Rescued zero-candidate files by definition raised no Signal 7
                // candidate, so they can carry no anchor ops either.
                pubsub_anchor_ops: Vec::new(),
                // Nor any same-file wrapper: a file with no HTTP candidate
                // issues no request of its own for one to be built out of.
                local_wrapper_calls: Vec::new(),
                // A rescued file raised no candidate of its own, so it is not
                // read as anybody's wrapper and nothing joins onto it either.
                request_members: RequestMemberIndex::default(),
                resolved_members: HashMap::new(),
                unresolved_member_sites: Vec::new(),
            });
        }

        // Where every request member the scan read is declared (carrick#656).
        // Built here, after the rescue loop, because a client whose requests go
        // through a helper raises no candidate of its own and reaches `pending`
        // only as a rescued file — and its members are in `member_cache`, read
        // on demand for the consumers that import it, never on its own
        // `PendingFile`.
        let member_homes = {
            let analysed: HashMap<PathBuf, String> = pending
                .iter()
                .filter_map(|pf| {
                    Path::new(&pf.path_str)
                        .canonicalize()
                        .ok()
                        .map(|canonical| (canonical, pf.path_str.clone()))
                })
                .collect();
            Self::member_homes(&member_cache, &analysed)
        };

        // PHASE 1c (#218 — cross-file env-alias resolution). A consumer that
        // builds its URLs from an imported config-object property
        // (`config.catalogUrl` ← `process.env.CATALOG_URL` in another file)
        // has an empty per-file alias map, so the declared env-var name was
        // never recovered and the base stayed verbatim in the call key. Follow
        // each file's import graph one hop (relative specifiers, same scope as
        // the #369 wrapper pass) and fold the imported modules' exported env
        // aliases into the importer's map. Parsed modules are memoized per
        // canonical path, so each config module is parsed once per scan.
        {
            let mut module_exports_cache: HashMap<PathBuf, EnvAliasMap> = HashMap::new();
            for pf in &mut pending {
                let importer = PathBuf::from(&pf.path_str);
                Self::merge_cross_file_env_aliases(
                    &mut pf.env_alias_map,
                    &importer,
                    &pf.symbol_table.imported_symbols,
                    &mut module_exports_cache,
                    &cm,
                    &handler,
                );
            }
        }

        // PHASE 2 (concurrent, I/O-bound): dispatch the LLM calls. `AgentService` owns a
        // semaphore (CARRICK_CONCURRENCY_LIMIT, default 20) that enforces the real rate cap,
        // so we eagerly buffer up to that many in-flight requests. Completion order does not
        // affect the result: stats are counts and `file_results` is a map, so the aggregate
        // is deterministic regardless of which call finishes first.
        let concurrency = std::env::var("CARRICK_CONCURRENCY_LIMIT")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(20)
            .max(1);

        // STEP 4: Call the file analyzer with Full File + Patterns + Candidate Targets +
        // richer AST-derived import table (Move 3, §9.3 of framework-coverage.md).
        // Every file dispatched here is one the index expects an answer for, so
        // the count is registered before the calls go out and each failure is
        // registered as it happens (#461).
        crate::scan_health::record_files_attempted(pending.len());
        let analyzed: Vec<(PendingFile, Result<FileAnalysisResult, String>)> =
            futures::stream::iter(pending.into_iter().map(|pf| async move {
                let result = self
                    .file_analyzer
                    .analyze_file_with_candidates(
                        &pf.path_str,
                        &pf.content,
                        guidance,
                        &pf.candidate_hints,
                        &pf.candidate_contexts,
                        &pf.symbol_table.imported_symbols,
                        &pf.graphql_producer_hints,
                        &pf.graphql_consumer_hints,
                        &pf.wrapper_context,
                    )
                    .await
                    .map_err(|e| {
                        // Recorded here, where the error is still typed: this
                        // file has no analysis in the index, and the run must
                        // not call itself a success. A call the quota breaker
                        // aborted is excluded — it was never attempted, and
                        // the breaker already fails the run on its own terms.
                        let quota_abort = e
                            .downcast_ref::<crate::agent_service::AgentCallError>()
                            .is_some_and(|err| err.is_quota_abort());
                        if !quota_abort {
                            crate::scan_health::record_unanalysed_file(
                                &pf.path_str,
                                &crate::scan_health::analysis_failure_reason(e.as_ref()),
                            );
                        }
                        e.to_string()
                    });
                (pf, result)
            }))
            .buffer_unordered(concurrency)
            .collect()
            .await;

        // PHASE 3 (serial): fold the per-file results into the aggregate.
        for (pf, result) in analyzed {
            match result {
                Ok(result) => {
                    // Note: Type positions are now resolved by the TypeSidecar (src/sidecar)
                    // using the compiler-based approach instead of position-based extraction.

                    let mut adjusted = result;
                    Self::apply_candidate_map(&mut adjusted, &pf.candidate_map, &pf.path_str);
                    // Read the method and target of a site that calls an
                    // imported module's request member off that member
                    // (carrick#588). Runs first among the post-extraction
                    // passes: it states both facts outright, where the ones
                    // below correct or backfill one of them.
                    stats.imported_member_resolutions +=
                        Self::apply_imported_members(&mut adjusted, &pf.resolved_members);
                    // Then emit the resolved sites extraction returned no row
                    // for at all (carrick#623). Immediately after the rewrite
                    // above, so the coverage test below reads the rows
                    // extraction produced and nothing a later pass appends.
                    stats.imported_member_backfills += Self::merge_imported_member_calls(
                        &mut adjusted,
                        &pf.resolved_members,
                        &pf.candidate_map,
                    );
                    // Carry the wrapper's own request shape onto the sites that
                    // delegate to it (carrick-cloud#386). Runs immediately after
                    // `apply_candidate_map` because that is what stamps the span
                    // this reads to tell a delegating site from a real client
                    // call, and before every downstream method reader.
                    stats.wrapper_method_propagations += Self::propagate_wrapper_request_shape(
                        &mut adjusted,
                        pf.wrapper_request_shape.as_ref(),
                    );
                    // Merge the outbound calls that reach their endpoint
                    // through a request wrapper declared in this same file
                    // (carrick#588). Their sites raise no candidate, so
                    // extraction was never asked about them and without this
                    // the endpoints they reach are absent from the index
                    // entirely. Merged HERE, before the fold below, so these
                    // rows go through the same normalization the analyzer's
                    // do: a wrapper that interpolates an env-var base emits
                    // `${ALIAS}/things`, and alias resolution is what turns
                    // that into the `process.env` name matching keys on.
                    stats.local_wrapper_call_backfills +=
                        Self::merge_local_wrapper_calls(&mut adjusted, pf.local_wrapper_calls);
                    // Collapse inline env-var fallbacks the model rendered
                    // verbatim (`${A ?? "http://localhost"}/p` -> `${A}/p`,
                    // carrick#399) BEFORE alias resolution: a local alias with
                    // a rendered fallback must become the bare `${alias}` for
                    // the alias-map lookup to rewrite it. This fold is the
                    // single entry point for LLM data calls, so every
                    // downstream reader (route-shape gate, canonical call key,
                    // env-var classification, uploads) sees one normalized
                    // form.
                    Self::normalize_fallback_targets(&mut adjusted);
                    Self::resolve_target_bases(
                        &mut adjusted,
                        &pf.env_alias_map,
                        &pf.whole_url_fallbacks,
                        &pf.literal_bases,
                    );
                    // Then emit the whole-URL sites that resolution had no row
                    // to rewrite (carrick#632). Immediately after the rewrite
                    // above, so both sides of the coverage test are in the
                    // resolved `${process.env.NAME}/path` form and nothing a
                    // later pass appends is read as covering a site.
                    //
                    // The general rule these three backfills share: a pass
                    // that fully resolves a candidate must EMIT, not only
                    // patch. Patching is enough while extraction is wrong
                    // about a site; it does nothing at all when extraction is
                    // silent about it, which is the more common failure and
                    // the one that leaves the endpoint absent from the index
                    // rather than merely mis-stated.
                    let (backfilled, corrected) = Self::merge_whole_url_env_calls(
                        &mut adjusted,
                        &pf.candidate_map,
                        &pf.env_alias_map,
                        &pf.whole_url_fallbacks,
                    );
                    stats.whole_url_env_backfills += backfilled;
                    stats.whole_url_env_corrections += corrected;
                    Self::validate_type_hints(&mut adjusted, &pf.symbol_table);
                    Self::normalize_unusable_types(&mut adjusted, &framework_detection.frameworks);

                    // Deterministic extraction-flake guards (#361): drop a
                    // data-call response symbol that borrows the request type,
                    // and repair a graphql-over-HTTP target reported as the
                    // transport URL. Both re-parse the file, so both are gated
                    // on a candidate data call being present.
                    let file_path = Path::new(&pf.path_str);
                    Self::suppress_borrowed_request_types(&mut adjusted, file_path);
                    Self::rewrite_graphql_document_targets(&mut adjusted, file_path);

                    // Read how each call's base resolves (carrick#649). LAST
                    // among the passes that touch a target, because it must
                    // read the SAME target the row persists: an env-var base
                    // rewritten to `${process.env.NAME}` above is the spelling
                    // this reports, and a graphql target rewritten just above
                    // is the one this reads. Stamps `base` and nothing else —
                    // no target, no method, no key.
                    Self::stamp_call_bases(
                        &mut adjusted,
                        &pf.env_alias_map,
                        &pf.env_fallbacks,
                        &env_schema,
                    );

                    // Canonicalize LLM-emitted endpoint paths to colon-style params
                    // (`/w/[slug]` -> `/w/:slug`) so they dedupe against the file-based
                    // router's structural entries instead of both surviving and flipping
                    // form between non-deterministic scans.
                    Self::canonicalize_endpoint_paths(&mut adjusted);

                    // Merge file-based route endpoints the LLM pass didn't already
                    // produce. The structural (method, path) facts are authoritative.
                    stats.file_based_endpoints +=
                        Self::merge_file_based_endpoints(&mut adjusted, pf.route_endpoints);

                    // Merge route-descriptor endpoints (`{ method, path, handler }`
                    // data) the LLM pass didn't produce — it ignores route-as-data,
                    // so these are emitted deterministically and are authoritative
                    // for such routes (#234).
                    stats.route_descriptor_endpoints +=
                        Self::merge_file_based_endpoints(&mut adjusted, pf.descriptor_endpoints);

                    // Drop LLM-emitted pub/sub ops whose topic has no literal
                    // witness in the file's source (carrick#311): the analyzer
                    // occasionally invents a topic from a wrapper-function
                    // NAME (`publishStatusChanged` -> `status.changed`). Must
                    // run before the anchor merge below so deterministic
                    // anchor ops are never candidates for the drop.
                    stats.pubsub_phantom_topic_drops +=
                        Self::suppress_phantom_pubsub_topics(&mut adjusted, file_path);

                    // Merge deterministically-anchored pub/sub operations the
                    // LLM pass didn't already produce (carrick#387). A
                    // payload-less publish/subscribe with a resolvable literal
                    // topic is a structural fact — an extraction omission must
                    // not lose the operation.
                    stats.pubsub_anchor_backfills +=
                        Self::merge_pubsub_anchor_ops(&mut adjusted, pf.pubsub_anchor_ops);

                    // Merge the outbound calls a verb-named request spec states
                    // outright (`client.post({ url: "/v1/things" })`) and the
                    // extraction did not report (#529). Runs after
                    // `apply_candidate_map` so the coverage check reads the
                    // model's calls already re-anchored to their spec.
                    stats.request_spec_call_backfills +=
                        Self::merge_request_spec_calls(&mut adjusted, &pf.candidate_map);

                    // carrick#656: what the join could not follow in THIS
                    // file, less the sites that turned out to be route
                    // definitions. A route registration whose verb happens to
                    // name a request member (`app.get` against a client's
                    // `get`) is a producer, not a consumer that went
                    // unfollowed — the same exclusion `merge_imported_member_calls`
                    // applies before it emits a row.
                    for (span, name) in &pf.unresolved_member_sites {
                        if adjusted
                            .endpoints
                            .iter()
                            .any(|endpoint| endpoint.call_expression_span_start == Some(*span))
                        {
                            continue;
                        }
                        *member_deficits.entry(name.clone()).or_insert(0) += 1;
                    }
                    if !pf.resolved_members.is_empty() {
                        resolved_member_rows.insert(
                            pf.path_str.clone(),
                            pf.resolved_members
                                .iter()
                                .map(|(span, resolved)| (*span, resolved.name.clone()))
                                .collect(),
                        );
                    }

                    stats.total_mounts += adjusted.mounts.len();
                    stats.total_endpoints += adjusted.endpoints.len();
                    stats.total_data_calls += adjusted.data_calls.len();
                    stats.files_processed += 1;
                    file_results.insert(pf.path_str, adjusted);
                }
                Err(e) => {
                    // The file is absent from `file_results`, so its endpoints
                    // and calls are absent from the index. Warn rather than
                    // collect quietly: `stats.errors` is only ever printed at
                    // debug, which is how this loss stayed invisible (#461).
                    // The run-level verdict is in `scan_health`.
                    warn!("Failed to analyze {}: {}", pf.path_str, e);
                    stats
                        .errors
                        .push(format!("Failed to analyze {}: {}", pf.path_str, e));
                    stats.files_skipped += 1;
                    stats.files_analysis_failed += 1;
                }
            }
        }

        // PHASE 4 (#580 part b): join each route table's paths to the
        // controller classes they bind. Cross-file, so it runs once every
        // file's own result is in — a controller's rows must survive the pass
        // that analyses the controller file. Sorted by (route table, line,
        // path) because `file_results` is a HashMap and the bindings' order
        // decides which of two identical rows is kept.
        controller_route_bindings.sort_by(|(a_file, _), (b_file, _)| a_file.cmp(b_file));
        let mut resolver = BindingResolver::new();
        let mut class_controller_rows: Vec<(PathBuf, EndpointResult)> = Vec::new();
        for (router_file, bindings) in &mut controller_route_bindings {
            bindings.sort_by(|a, b| {
                a.line_number
                    .cmp(&b.line_number)
                    .then_with(|| a.path.cmp(&b.path))
            });
            // `BindingResolver` resolves relative to the importer and answers
            // with canonical paths; a non-canonical importer resolves to a
            // path that matches no analysed file (`/var` vs `/private/var`).
            let Ok(canonical) = router_file.canonicalize() else {
                continue;
            };
            class_controller_rows.extend(Self::class_controller_endpoints(
                &self.swc_scanner,
                &mut resolver,
                &canonical,
                bindings,
            ));
        }
        let class_controller_added =
            Self::merge_class_controller_endpoints(&mut file_results, files, class_controller_rows);
        stats.class_controller_endpoints += class_controller_added;
        stats.total_endpoints += class_controller_added;

        debug!("\n=== FILE PROCESSING COMPLETE ===");
        debug!("  - Files processed (LLM calls): {}", stats.files_processed);
        debug!("  - Files skipped (total): {}", stats.files_skipped);
        debug!(
            "  - Zero-cost skips (no API patterns): {}",
            stats.files_skipped_no_candidates
        );
        if stats.files_parse_failed > 0 {
            warn!(
                "{} file(s) failed to parse and are excluded from the index",
                stats.files_parse_failed
            );
        }
        debug!("  - Total mounts: {}", stats.total_mounts);
        debug!("  - Total endpoints: {}", stats.total_endpoints);
        debug!(
            "  - File-based route endpoints: {}",
            stats.file_based_endpoints
        );
        debug!(
            "  - Route-descriptor endpoints: {}",
            stats.route_descriptor_endpoints
        );
        debug!(
            "  - Class-controller endpoints: {}",
            stats.class_controller_endpoints
        );
        debug!("  - Total data calls: {}", stats.total_data_calls);
        debug!(
            "  - Request-spec call backfills: {}",
            stats.request_spec_call_backfills
        );
        debug!(
            "  - Same-file wrapper call backfills: {}",
            stats.local_wrapper_call_backfills
        );
        debug!(
            "  - Imported-member resolutions: {}",
            stats.imported_member_resolutions
        );
        debug!(
            "  - Imported-member call backfills: {}",
            stats.imported_member_backfills
        );
        debug!(
            "  - Whole-URL env-var call backfills: {}",
            stats.whole_url_env_backfills
        );
        debug!(
            "  - Whole-URL env-var call corrections: {}",
            stats.whole_url_env_corrections
        );
        debug!(
            "  - Wrapper method propagations: {}",
            stats.wrapper_method_propagations
        );

        // PHASE 5 (carrick#656): state what the member join could not follow.
        // Cross-file, so it runs once every file's own result is in: the sites
        // a member lost are in the consumer files, and the rows that carry the
        // number are wherever that member resolved.
        let stamped = Self::stamp_unfollowed_member_sites(
            &mut file_results,
            &member_deficits,
            &resolved_member_rows,
            &member_homes,
        );
        if stamped > 0 {
            debug!("  - Rows stating unfollowed member call sites: {stamped}");
        }

        // STEP 5: Build aggregated mount graph from all file results
        let mount_graph =
            self.build_mount_graph(&file_results, normalizer, service_root, Path::new(""));

        Ok(FileCentricAnalysisResult {
            file_results,
            mount_graph,
            stats,
            bundled_types: None,
            type_resolution: None,
        })
    }

    /// Collect type requests from analysis results for sidecar processing.
    ///
    /// Returns two vectors:
    /// - `SymbolRequest`: For entries WITH explicit type annotations (primary_type_symbol + type_import_source)
    /// - `InferRequestItem`: For entries WITHOUT explicit type annotations (need inference)
    ///
    /// # Arguments
    /// * `file_results` - Analysis results keyed by file path
    /// * `repo_path` - Path to the repository root (used to convert relative paths to absolute)
    /// * `mount_graph` - Resolved mount graph for canonical method/path aliases
    /// * `config` - Config used for URL normalization
    pub fn collect_type_requests(
        &self,
        file_results: &HashMap<String, FileAnalysisResult>,
        repo_path: &str,
        mount_graph: &MountGraph,
        config: &Config,
    ) -> (
        Vec<SymbolRequest>,
        Vec<InferRequestItem>,
        Vec<(String, String)>,
    ) {
        // Convert repo_path to absolute for path resolution
        let repo_root = std::path::Path::new(repo_path);
        let repo_root_absolute = if repo_root.is_absolute() {
            repo_root.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(repo_root))
                .unwrap_or_else(|_| repo_root.to_path_buf())
                .canonicalize()
                .unwrap_or_else(|_| repo_root.to_path_buf())
        };

        let normalizer = UrlNormalizer::new(config);
        let mut explicit_requests: Vec<SymbolRequest> = Vec::new();
        let mut explicit_seen: HashSet<String> = HashSet::new();
        let mut infer_requests: Vec<InferRequestItem> = Vec::new();
        let mut endpoint_lookup: EndpointLookup = HashMap::new();
        let mut data_call_lookup: DataCallLookup = HashMap::new();
        let mut inline_aliases: Vec<(String, String)> = Vec::new();
        let should_infer_request_body = |method: &str| {
            matches!(
                method,
                "POST" | "PUT" | "PATCH" | "DELETE" | "ALL" | "UNKNOWN"
            )
        };
        let mut push_explicit =
            |symbol_name: String, source_file: String, alias: Option<String>| {
                let key = format!(
                    "{}|{}|{}",
                    source_file,
                    symbol_name,
                    alias.as_deref().unwrap_or("")
                );
                if explicit_seen.insert(key) {
                    explicit_requests.push(SymbolRequest {
                        symbol_name,
                        source_file,
                        alias,
                        array_depth: None,
                        payload_borrow_witness: false,
                    });
                }
            };
        /// Locator for type inference: either SWC byte-offset spans or Gemini expression text + line
        enum InferLocator<'a> {
            Span {
                span_start: Option<u32>,
                span_end: Option<u32>,
            },
            Text {
                expression_text: Option<&'a str>,
                expression_line: Option<i32>,
            },
            /// Locate purely by line number (no span, no text). Used for
            /// file-based route handlers, where the only reliable anchor is the
            /// handler's declaration line and the sidecar resolves the function
            /// via `findFunctionByLine`.
            Line,
        }

        let mut push_infer = |file_path: &str,
                              line_number: u32,
                              infer_kind: InferKind,
                              alias: String,
                              locator: InferLocator<'_>| {
            match locator {
                InferLocator::Span {
                    span_start,
                    span_end,
                } => {
                    let (Some(start), Some(end)) = (span_start, span_end) else {
                        return false;
                    };
                    infer_requests.push(InferRequestItem {
                        file_path: file_path.to_string(),
                        line_number,
                        infer_kind,
                        span_start: Some(start),
                        span_end: Some(end),
                        expression_text: None,
                        expression_line: None,
                        alias: Some(alias),
                        param_name: None,
                    });
                    true
                }
                InferLocator::Text {
                    expression_text,
                    expression_line,
                } => {
                    let Some(text) = expression_text else {
                        return false;
                    };
                    if text.is_empty() {
                        return false;
                    }
                    infer_requests.push(InferRequestItem {
                        file_path: file_path.to_string(),
                        line_number,
                        infer_kind,
                        span_start: None,
                        span_end: None,
                        expression_text: Some(text.to_string()),
                        expression_line: expression_line
                            .map(|l| if l > 0 { l as u32 } else { line_number }),
                        alias: Some(alias),
                        param_name: None,
                    });
                    true
                }
                InferLocator::Line => {
                    infer_requests.push(InferRequestItem {
                        file_path: file_path.to_string(),
                        line_number,
                        infer_kind,
                        span_start: None,
                        span_end: None,
                        expression_text: None,
                        expression_line: None,
                        alias: Some(alias),
                        param_name: None,
                    });
                    true
                }
            }
        };

        for endpoint in mount_graph.get_resolved_endpoints() {
            let (file_path, line_number) = parse_file_location(&endpoint.file_location);
            let method = normalize_manifest_method(&endpoint.method);
            endpoint_lookup
                .entry((file_path, line_number))
                .or_default()
                .push((method, endpoint.full_path.clone()));
        }

        for data_call in mount_graph.get_data_calls() {
            if !normalizer.is_probable_url(&data_call.target_url) {
                continue;
            }
            let (file_path, line_number) = parse_file_location(&data_call.file_location);
            let Some(method) = Self::normalize_consumer_method(Some(&data_call.method)) else {
                continue;
            };
            // Canonical path computed once at mount-graph build time; keep the
            // manifest join key identical to the projection key for this call.
            let path = data_call.canonical_path.clone();
            let call_id = build_call_site_id(
                &file_path,
                line_number,
                &OperationKey::http(&method, path.clone()),
                repo_path,
            );
            data_call_lookup
                .entry((file_path, line_number))
                .or_default()
                .push((method, path, call_id));
        }

        for (file_path, result) in file_results {
            // Convert file_path to absolute path relative to repo root
            let file_path_absolute = Self::to_absolute_path(file_path, &repo_root_absolute);

            // Process endpoints
            for endpoint in &result.endpoints {
                let line_number = if endpoint.line_number <= 0 {
                    1
                } else {
                    endpoint.line_number as u32
                };
                let lookup_key = (file_path.clone(), line_number);
                let method_fallback = normalize_manifest_method(&endpoint.method);
                let (method, path) = endpoint_lookup
                    .get(&lookup_key)
                    .and_then(|entries| {
                        if entries.len() == 1 {
                            return Some(entries[0].clone());
                        }
                        entries
                            .iter()
                            .find(|(entry_method, entry_path)| {
                                entry_method == &method_fallback
                                    && (entry_path == &endpoint.path
                                        || entry_path.ends_with(&endpoint.path))
                            })
                            .or_else(|| {
                                entries
                                    .iter()
                                    .find(|(entry_method, _)| entry_method == &method_fallback)
                            })
                            .cloned()
                    })
                    .unwrap_or_else(|| (method_fallback.clone(), endpoint.path.clone()));
                if !is_http_method(&method) || !path.starts_with('/') {
                    continue;
                }
                let key = OperationKey::http(&method, path.clone());
                let response_alias = build_manifest_type_alias(
                    &key,
                    ManifestRole::Producer,
                    ManifestTypeKind::Response,
                );
                let request_alias = build_manifest_type_alias(
                    &key,
                    ManifestRole::Producer,
                    ManifestTypeKind::Request,
                );

                // no-payload endpoints have no recoverable response contract:
                // skip the explicit-symbol bundling as well as inference below,
                // so the manifest entry stays honestly `unknown` (with its
                // evidence) instead of publishing a phantom contract from a
                // type hint the handler never sends.
                let no_payload = endpoint.emission_style == Some(EmissionStyle::NoPayload);

                if !no_payload {
                    if let (Some(symbol), Some(import_source)) =
                        (&endpoint.primary_type_symbol, &endpoint.type_import_source)
                    {
                        // Explicit type with import source - bundle it
                        push_explicit(
                            symbol.clone(),
                            Self::resolve_import_path(&file_path_absolute, import_source),
                            Some(response_alias.clone()),
                        );
                    } else if endpoint.primary_type_symbol.is_some()
                        && endpoint.type_import_source.is_none()
                    {
                        // Type symbol exists but no import - it might be in the same file
                        if let Some(ref symbol) = endpoint.primary_type_symbol {
                            push_explicit(
                                symbol.clone(),
                                file_path_absolute.clone(),
                                Some(response_alias.clone()),
                            );
                        }
                    } else if endpoint.type_import_source.is_some()
                        && endpoint.primary_type_symbol.is_none()
                    {
                        warn!(
                            "[FileOrchestrator] Endpoint at {}:{} has import source {:?} but no symbol; relying on inference",
                            file_path, line_number, endpoint.type_import_source
                        );
                    }
                }

                // File-based routes (Next.js app router, etc.) have no call-site
                // payload expression: the handler's return type *is* the response
                // contract (e.g., `export async function GET(): Promise<Response>` or `Promise<NextResponse<User[]>>`, or an
                // inferred `return new Response(...)`). Their stored span points at
                // the whole handler declaration, which the response-body locators
                // would misread as the payload — so request a `FunctionReturn`
                // anchored on the handler line instead, which the sidecar resolves
                // via `findFunctionByLine` and Promise-unwraps. Request-body
                // inference is skipped: a Next.js request body isn't recoverable
                // from the signature.
                if endpoint.owner_node == FILE_BASED_ROUTE_OWNER {
                    // Structurally derived endpoints never carry an
                    // emission_style today, but the no-payload gate must hold
                    // here too if that ever changes — a no-payload claim means
                    // the manifest stays honestly unknown, with no inference.
                    if !no_payload {
                        // The Line locator is infallible, so no inline-alias
                        // fallback is needed here.
                        push_infer(
                            &file_path_absolute,
                            line_number,
                            InferKind::FunctionReturn,
                            response_alias.clone(),
                            InferLocator::Line,
                        );
                    }
                    continue;
                }

                // Route response inference by the model's emission_style
                // classification. `None` (field omitted — e.g. cached
                // pre-emission-style analysis) falls back to imperative-send,
                // which is the historical behavior.
                match endpoint.emission_style {
                    // The handler's return value IS the payload: ask for the
                    // handler's return type. Prefer the text locator — the
                    // sidecar resolves the expression's *containing* function,
                    // which finds the exact handler even when it's a named
                    // reference declared far from the registration line. Fall
                    // back to the registration line (correct for inline
                    // handlers, whose function starts on that line) when the
                    // model gave no expression.
                    Some(EmissionStyle::ReturnValue) => {
                        let _ = push_infer(
                            &file_path_absolute,
                            line_number,
                            InferKind::FunctionReturn,
                            response_alias.clone(),
                            InferLocator::Text {
                                expression_text: endpoint.response_expression_text.as_deref(),
                                expression_line: endpoint.response_expression_line,
                            },
                        ) || push_infer(
                            &file_path_absolute,
                            line_number,
                            InferKind::FunctionReturn,
                            response_alias.clone(),
                            InferLocator::Line,
                        );
                    }
                    // No recoverable payload expression (zero-arg sends,
                    // streams, helper-written payloads): skip inference. The
                    // manifest entry keeps `unknown` with its evidence —
                    // honest, instead of inferring from the wrong node.
                    Some(EmissionStyle::NoPayload) => {}
                    Some(EmissionStyle::ImperativeSend) | None => {
                        let response_inferred = push_infer(
                            &file_path_absolute,
                            line_number,
                            InferKind::ResponseBody,
                            response_alias.clone(),
                            InferLocator::Text {
                                expression_text: endpoint.response_expression_text.as_deref(),
                                expression_line: endpoint.response_expression_line,
                            },
                        ) || push_infer(
                            &file_path_absolute,
                            line_number,
                            InferKind::ResponseBody,
                            response_alias.clone(),
                            InferLocator::Span {
                                span_start: endpoint.call_expression_span_start,
                                span_end: endpoint.call_expression_span_end,
                            },
                        );
                        if !response_inferred
                            && let Some(symbol) = endpoint.primary_type_symbol.as_ref()
                        {
                            inline_aliases.push((response_alias.clone(), symbol.clone()));
                        }
                    }
                }

                if should_infer_request_body(&method) {
                    let _ = push_infer(
                        &file_path_absolute,
                        line_number,
                        InferKind::RequestBody,
                        request_alias.clone(),
                        InferLocator::Text {
                            expression_text: endpoint.payload_expression_text.as_deref(),
                            expression_line: endpoint.payload_expression_line,
                        },
                    ) || push_infer(
                        &file_path_absolute,
                        line_number,
                        InferKind::RequestBody,
                        request_alias.clone(),
                        InferLocator::Span {
                            span_start: endpoint.call_expression_span_start,
                            span_end: endpoint.call_expression_span_end,
                        },
                    );
                }
            }

            // Process data calls
            for data_call in &result.data_calls {
                let line_number = if data_call.line_number <= 0 {
                    1
                } else {
                    data_call.line_number as u32
                };
                if !normalizer.is_probable_url(&data_call.target) {
                    continue;
                }
                let lookup_key = (file_path.clone(), line_number);
                let Some(method_fallback) =
                    Self::normalize_consumer_method(data_call.method.as_deref())
                else {
                    continue;
                };
                // Same canonicalization the mount-graph loop above and the cloud
                // projection use, so this fallback path keys identically.
                let target_path = Self::canonical_call_path(&normalizer, data_call);
                let (method, path, call_id) = data_call_lookup
                    .get(&lookup_key)
                    .and_then(|entries| {
                        if entries.len() == 1 {
                            return Some(entries[0].clone());
                        }
                        entries
                            .iter()
                            .find(|(entry_method, entry_path, _)| {
                                entry_method == &method_fallback && entry_path == &target_path
                            })
                            .or_else(|| {
                                entries
                                    .iter()
                                    .find(|(entry_method, _, _)| entry_method == &method_fallback)
                            })
                            .cloned()
                    })
                    .unwrap_or_else(|| {
                        (
                            method_fallback.clone(),
                            target_path.clone(),
                            build_call_site_id(
                                file_path,
                                line_number,
                                &OperationKey::http(&method_fallback, target_path.clone()),
                                repo_path,
                            ),
                        )
                    });
                let key = OperationKey::http(&method, path.clone());
                let response_alias = build_manifest_type_alias_with_call_id(
                    &key,
                    ManifestRole::Consumer,
                    ManifestTypeKind::Response,
                    Some(&call_id),
                );
                let request_alias = build_manifest_type_alias_with_call_id(
                    &key,
                    ManifestRole::Consumer,
                    ManifestTypeKind::Request,
                    Some(&call_id),
                );

                if let (Some(symbol), Some(import_source)) = (
                    &data_call.primary_type_symbol,
                    &data_call.type_import_source,
                ) {
                    // Explicit type with import source - bundle it
                    push_explicit(
                        symbol.clone(),
                        Self::resolve_import_path(&file_path_absolute, import_source),
                        Some(response_alias.clone()),
                    );
                } else if data_call.primary_type_symbol.is_some()
                    && data_call.type_import_source.is_none()
                {
                    // Type symbol exists but no import - it might be in the same file
                    if let Some(ref symbol) = data_call.primary_type_symbol {
                        push_explicit(
                            symbol.clone(),
                            file_path_absolute.clone(),
                            Some(response_alias.clone()),
                        );
                    }
                } else if data_call.type_import_source.is_some()
                    && data_call.primary_type_symbol.is_none()
                {
                    warn!(
                        "[FileOrchestrator] Data call at {}:{} has import source {:?} but no symbol; relying on inference",
                        file_path, line_number, data_call.type_import_source
                    );
                }

                let call_inferred = push_infer(
                    &file_path_absolute,
                    line_number,
                    InferKind::CallResult,
                    response_alias.clone(),
                    InferLocator::Text {
                        expression_text: data_call.call_expression_text.as_deref(),
                        expression_line: data_call.call_expression_line,
                    },
                ) || push_infer(
                    &file_path_absolute,
                    line_number,
                    InferKind::CallResult,
                    response_alias.clone(),
                    InferLocator::Span {
                        span_start: data_call.call_expression_span_start,
                        span_end: data_call.call_expression_span_end,
                    },
                );
                if !call_inferred && let Some(symbol) = data_call.primary_type_symbol.as_ref() {
                    inline_aliases.push((response_alias.clone(), symbol.clone()));
                }

                if should_infer_request_body(&method) {
                    push_infer(
                        &file_path_absolute,
                        line_number,
                        InferKind::RequestBody,
                        request_alias.clone(),
                        InferLocator::Text {
                            expression_text: data_call.payload_expression_text.as_deref(),
                            expression_line: data_call.payload_expression_line,
                        },
                    );
                }
            }
        }

        debug!(
            "[FileOrchestrator] Collected {} explicit type requests, {} inference requests, {} inline aliases",
            explicit_requests.len(),
            infer_requests.len(),
            inline_aliases.len()
        );

        (explicit_requests, infer_requests, inline_aliases)
    }

    /// Build `SymbolRequest`s for Socket.IO payload anchors (#245 Phase 1).
    ///
    /// Sibling to `collect_type_requests`: it routes the deterministically
    /// captured socket payload type through the *same* sidecar bundle path the
    /// HTTP explicit-symbol case uses. Listeners are producers, emitters are
    /// consumers; each resolves to the Response-kind alias.
    ///
    /// The alias MUST be `build_manifest_type_alias(&op.key, role, Response)` —
    /// byte-identical to the alias `append_protocol_manifest_entry` stamped on
    /// the manifest entry — or the resolved `.d.ts` never joins back and the
    /// entry stays `Unknown`. This contract is guarded by a unit test.
    ///
    /// Only ops whose extractor captured a `payload_type_symbol` produce a
    /// request; an absent source means the symbol is declared in the emitting
    /// file, so it is resolved against that file's absolute path.
    pub fn collect_socket_type_requests(
        &self,
        sockets: &crate::socket_io::SocketExtraction,
        repo_path: &str,
    ) -> Vec<SymbolRequest> {
        let repo_root = std::path::Path::new(repo_path);
        let repo_root_absolute = if repo_root.is_absolute() {
            repo_root.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(repo_root))
                .unwrap_or_else(|_| repo_root.to_path_buf())
                .canonicalize()
                .unwrap_or_else(|_| repo_root.to_path_buf())
        };

        let mut requests: Vec<SymbolRequest> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut push = |op: &crate::socket_io::SocketOp, role: ManifestRole| {
            let Some(symbol) = op.payload_type_symbol.as_ref() else {
                return;
            };
            let file_abs =
                Self::to_absolute_path(&op.file_path.to_string_lossy(), &repo_root_absolute);
            let source_file = match op.payload_type_source.as_ref() {
                Some(import_source) => Self::resolve_import_path(&file_abs, import_source),
                // No import → same-file declaration: resolve against the file.
                None => file_abs,
            };
            let alias = build_manifest_type_alias(&op.key, role, ManifestTypeKind::Response);
            let dedup_key = format!("{}|{}|{}", source_file, symbol, alias);
            if seen.insert(dedup_key) {
                requests.push(SymbolRequest {
                    symbol_name: symbol.clone(),
                    source_file,
                    alias: Some(alias),
                    array_depth: None,
                    payload_borrow_witness: false,
                });
            }
        };
        for op in &sockets.listeners {
            push(op, ManifestRole::Producer);
        }
        for op in &sockets.emitters {
            push(op, ManifestRole::Consumer);
        }
        debug!(
            "[FileOrchestrator] Collected {} socket payload type requests",
            requests.len()
        );
        requests
    }

    /// Build `SymbolRequest`s for pub/sub decoded-payload anchors (#corpus-2
    /// resolution dim).
    ///
    /// Sibling of `collect_socket_type_requests`, but reads the LLM-sourced
    /// `pubsub_operations` out of `file_results` rather than the deterministic
    /// `ProtocolExtractions` struct — pub/sub ops never go through the
    /// deterministic protocol extractors, so this walks the exact same source
    /// `append_pubsub_manifest_entries` walks. Each op carrying a
    /// `primary_type_symbol` routes that decoded-payload type through the *same*
    /// sidecar bundle path the Socket.IO and HTTP explicit-symbol cases use, so
    /// the sidecar expands it into the entry's `resolved_type`. Subscribers are
    /// producers, publishers are consumers; each resolves to the Response-kind
    /// alias.
    ///
    /// The alias MUST be byte-identical to the one `add_protocol_manifest_entry`
    /// stamps on the manifest entry in `append_pubsub_manifest_entries` — or the
    /// resolved `.d.ts` never joins back and the entry stays `Unknown`. Producers
    /// (subscribers) use the plain `build_manifest_type_alias(&key, role,
    /// Response)`; consumers (publishers) append a `build_call_site_id(path, line,
    /// &key)` suffix so fan-in publishers on one topic don't collide on a single
    /// alias (see `append_pubsub_manifest_entries`). Both contracts are guarded by
    /// unit tests.
    ///
    /// Only ops whose extractor captured a `primary_type_symbol` produce a
    /// request; a `None` symbol (untyped or inline-object payload) emits nothing,
    /// exactly like socket. A roleless op anchors nothing and is skipped (it was
    /// already dropped from `cloud_data` and has no manifest entry to join to).
    /// An absent `type_import_source` means the symbol is declared in the op's
    /// own file, so it resolves against that file's absolute path.
    ///
    /// Each request additionally carries the deterministic borrow witness
    /// (#413, `pubsub_payload_borrow_witness`): AST evidence that the emitted
    /// symbol structurally cannot be the type of the payload the op's locator
    /// names. The witness alone changes nothing — it only licenses
    /// `demote_witnessed_borrowed_anchors` to prefer the locator's
    /// tsc-resolved root over the explicit symbol when the two disagree.
    pub fn collect_pubsub_type_requests(
        &self,
        file_results: &HashMap<String, FileAnalysisResult>,
        repo_path: &str,
    ) -> Vec<SymbolRequest> {
        use crate::operation::{OperationKey, PubsubRole};

        let repo_root = std::path::Path::new(repo_path);
        let repo_root_absolute = if repo_root.is_absolute() {
            repo_root.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(repo_root))
                .unwrap_or_else(|_| repo_root.to_path_buf())
                .canonicalize()
                .unwrap_or_else(|_| repo_root.to_path_buf())
        };

        let mut requests: Vec<SymbolRequest> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        // `file_results` is a HashMap, whose iteration order is non-deterministic.
        // Walk the keys in sorted order so the emitted `SymbolRequest` sequence is
        // stable across runs (the scanner's output determinism depends on it).
        let mut paths: Vec<&String> = file_results.keys().collect();
        paths.sort();
        for path in paths {
            let result = &file_results[path];
            let file_abs = Self::to_absolute_path(path, &repo_root_absolute);
            // Lazy per-file annotation-mention evidence for the borrow
            // witness (#413): parsed at most once per file, and only when an
            // op actually carries both anchors (an explicit symbol AND a
            // payload locator the infer collector will accept).
            let mut annotation_mentions: Option<Option<AnnotationMentionsCollector>> = None;
            for op in &result.pubsub_operations {
                // Mirror the manifest-side role mapping in
                // `append_pubsub_manifest_entries`: subscriber = producer,
                // publisher = consumer; a roleless op anchors nothing.
                let role = match op.role {
                    Some(PubsubRole::Subscriber) => ManifestRole::Producer,
                    Some(PubsubRole::Publisher) => ManifestRole::Consumer,
                    None => continue,
                };
                let Some(symbol) = op.primary_type_symbol.as_ref() else {
                    continue;
                };
                let source_file = match op.type_import_source.as_ref() {
                    Some(import_source) => Self::resolve_import_path(&file_abs, import_source),
                    // No import → same-file declaration: resolve against the file.
                    None => file_abs.clone(),
                };
                // Borrow witness (#413): computed only against locators the
                // sibling infer collector will actually resolve — a bare
                // identifier that does not contain the op's own topic (the
                // envelope-copy guard) — because arbitration in
                // `demote_witnessed_borrowed_anchors` needs the second
                // anchor's inference to exist for the same alias.
                let payload_borrow_witness = op
                    .payload_expression_text
                    .as_deref()
                    .filter(|text| !text.contains(op.topic.as_str()))
                    .and_then(payload_bare_ident)
                    .is_some_and(|payload_ident| {
                        let collector = annotation_mentions.get_or_insert_with(|| {
                            let cm: Lrc<SourceMap> = Default::default();
                            let handler = Handler::with_tty_emitter(
                                ColorConfig::Never,
                                false,
                                false,
                                Some(cm.clone()),
                            );
                            parse_file(std::path::Path::new(&file_abs), &cm, &handler).map(
                                |module| {
                                    let mut mentions = AnnotationMentionsCollector::default();
                                    module.visit_with(&mut mentions);
                                    mentions
                                },
                            )
                        });
                        collector.as_ref().is_some_and(|mentions| {
                            pubsub_payload_borrow_witness(
                                &mentions.mentions_by_binding,
                                payload_ident,
                                symbol,
                            )
                        })
                    });
                let key = OperationKey::pubsub(op.topic.clone());
                // Mirror the manifest side (`append_pubsub_manifest_entries`):
                // publishers (consumers) disambiguate by call site so fan-in
                // publishers don't collide on one alias; subscribers (producers)
                // stay plain. `build_call_site_id` MUST see the same (path, line,
                // key, repo_root) the manifest side passes — `path` is the raw
                // `file_results` key on both sides and the id relativizes it
                // against `repo_root` internally (#355) — or the alias diverges
                // and the resolution enrich-join silently drops the resolved
                // payload type.
                let alias = match role {
                    ManifestRole::Consumer => {
                        // Same >= 1 clamp as the manifest side (see
                        // `append_pubsub_manifest_entries`) so the call_id matches.
                        let line = u32::try_from(op.line_number).unwrap_or(0).max(1);
                        let call_id = build_call_site_id(path, line, &key, repo_path);
                        build_manifest_type_alias_with_call_id(
                            &key,
                            role,
                            ManifestTypeKind::Response,
                            Some(&call_id),
                        )
                    }
                    ManifestRole::Producer => {
                        build_manifest_type_alias(&key, role, ManifestTypeKind::Response)
                    }
                };
                let dedup_key = format!("{}|{}|{}", source_file, symbol, alias);
                if seen.insert(dedup_key) {
                    requests.push(SymbolRequest {
                        symbol_name: symbol.clone(),
                        source_file,
                        alias: Some(alias),
                        array_depth: None,
                        payload_borrow_witness,
                    });
                }
            }
        }
        debug!(
            "[FileOrchestrator] Collected {} pub/sub payload type requests",
            requests.len()
        );
        requests
    }

    /// Build `InferRequestItem`s for pub/sub payloads whose type is NOT a bare
    /// named symbol — the wrapper patterns where the payload type lives in a
    /// generic binding (a topic→payload type map on a bus, a schema catalog's
    /// `infer`, a handle factory's declaration-site type argument) and is
    /// therefore invisible to the `primary_type_symbol` bundle path.
    ///
    /// Sibling of `collect_pubsub_type_requests`, mirroring the division the
    /// HTTP family already uses: the LLM supplies a LOCATOR
    /// (`payload_expression_text` + `payload_expression_line`), and the
    /// sidecar's location-based inference resolves the type deterministically
    /// with tsc — which has already instantiated the wrapper's generics at the
    /// site. No library or method-name matching is involved anywhere.
    ///
    /// Routing is by role, not by syntax: a publisher's locator is a value
    /// EXPRESSION (the payload argument / `payload:` property initializer), so
    /// it takes `InferKind::Expression`; a subscriber's locator is the handler
    /// parameter or destructured binding holding the payload, so it takes
    /// `InferKind::FunctionParam` (the sidecar matches the param by name,
    /// whole binding pattern, or binding element — see `inferFunctionParam`).
    ///
    /// Two-anchor co-emission (#413, replacing the former #268-style isolation
    /// guard): an op that already carries a `primary_type_symbol` still emits
    /// an infer request when it has a usable payload locator. The explicit
    /// bundle remains authoritative for the alias — a same-alias inference
    /// never shadows a bundled definition in `resolve_all_types` — EXCEPT
    /// when the scanner attached a deterministic borrow witness to the
    /// explicit request and the inferred payload root disagrees with the
    /// emitted symbol, in which case `demote_witnessed_borrowed_anchors`
    /// drops the explicit anchor and the inference becomes the alias's
    /// definition (the wrong-symbol borrow class, measured 13/20 on the
    /// honest c1 decoy fixture).
    ///
    /// The alias MUST be byte-identical to the one
    /// `append_pubsub_manifest_entries` stamped on the manifest entry — same
    /// key, role, kind, and (for publishers) the same `build_call_site_id` over
    /// the same raw `file_results` path and >=1-clamped line — or the
    /// enrich-join silently fails to flip `Unknown` → `Implicit`. Guarded by a
    /// unit test alongside the symbol-path alias test.
    pub fn collect_pubsub_infer_requests(
        &self,
        file_results: &HashMap<String, FileAnalysisResult>,
        repo_path: &str,
    ) -> Vec<InferRequestItem> {
        use crate::operation::{OperationKey, PubsubRole};

        let repo_root = std::path::Path::new(repo_path);
        let repo_root_absolute = if repo_root.is_absolute() {
            repo_root.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(repo_root))
                .unwrap_or_else(|_| repo_root.to_path_buf())
                .canonicalize()
                .unwrap_or_else(|_| repo_root.to_path_buf())
        };

        let mut requests: Vec<InferRequestItem> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        // Sorted walk for output determinism, same as the symbol collector.
        let mut paths: Vec<&String> = file_results.keys().collect();
        paths.sort();
        for path in paths {
            let result = &file_results[path];
            for op in &result.pubsub_operations {
                let role = match op.role {
                    Some(PubsubRole::Subscriber) => ManifestRole::Producer,
                    Some(PubsubRole::Publisher) => ManifestRole::Consumer,
                    None => continue,
                };
                // No isolation guard for named anchors (#413): an op that
                // carries a `primary_type_symbol` ALSO emits an infer request
                // when it has a usable payload locator, so the sidecar
                // resolves both anchors and `demote_witnessed_borrowed_anchors`
                // can arbitrate a witnessed wrong symbol against the
                // tsc-resolved payload root. Without a borrow witness the
                // explicit bundle still wins unconditionally — the combine
                // step in `resolve_all_types` never lets a same-alias
                // inference shadow a bundled alias.
                let Some(text) = op.payload_expression_text.as_ref() else {
                    continue;
                };
                // Envelope guard (measured 10/20 on the wrapper-dispatch harness
                // fixture): the model sometimes copies the whole options/envelope
                // object or call instead of the payload value. When the locator
                // text contains the op's own extracted topic literal, it
                // demonstrably includes the ROUTING key — that text is the
                // envelope or the call, never the decoded payload. Resolving it
                // would put the envelope's type on the manifest and feed false
                // compat verdicts; dropping it keeps the op Unknown, which is
                // recoverable. Structural on purpose: keyed on this op's topic
                // string alone, no property-name or library conventions.
                if text.contains(op.topic.as_str()) {
                    debug!(
                        topic = %op.topic,
                        file = %path,
                        "pub/sub payload locator contains the topic literal; \
                         treating as envelope copy and leaving the op unanchored"
                    );
                    continue;
                }
                let line = u32::try_from(op.line_number).unwrap_or(0).max(1);
                let key = OperationKey::pubsub(op.topic.clone());
                let alias = match role {
                    ManifestRole::Consumer => {
                        let call_id = build_call_site_id(path, line, &key, repo_path);
                        build_manifest_type_alias_with_call_id(
                            &key,
                            role,
                            ManifestTypeKind::Response,
                            Some(&call_id),
                        )
                    }
                    ManifestRole::Producer => {
                        build_manifest_type_alias(&key, role, ManifestTypeKind::Response)
                    }
                };
                let file_abs = Self::to_absolute_path(path, &repo_root_absolute);
                // The sidecar's text search anchors on the expression's own
                // line when the model reported one, falling back to the
                // operation line.
                let expression_line = op
                    .payload_expression_line
                    .and_then(|l| u32::try_from(l).ok())
                    .filter(|&l| l > 0);
                let dedup_key = format!("{}|{}|{}", file_abs, text, alias);
                if !seen.insert(dedup_key) {
                    continue;
                }
                let request = match role {
                    ManifestRole::Consumer => InferRequestItem {
                        file_path: file_abs,
                        line_number: line,
                        span_start: None,
                        span_end: None,
                        expression_text: Some(text.clone()),
                        // Anchor the text search even when the model omitted the
                        // expression's own line: the payload is an argument of
                        // the publish call at the op's line, so it STARTS within
                        // the sidecar's +/-5-line window of it. An unanchored
                        // search scans the whole file and, for identical text at
                        // multiple sites, has no proximity tie-break — a wrong
                        // occurrence resolves a confidently wrong type, whereas
                        // an anchored miss degrades to Unknown (recoverable).
                        expression_line: expression_line.or(Some(line)),
                        infer_kind: InferKind::Expression,
                        alias: Some(alias),
                        param_name: None,
                    },
                    ManifestRole::Producer => InferRequestItem {
                        // Anchor the containing-function search on the payload
                        // binding's own line when present (the handler may start
                        // lines away from the subscribe call the op is keyed on).
                        file_path: file_abs,
                        line_number: expression_line.unwrap_or(line),
                        span_start: None,
                        span_end: None,
                        // No expression_text: `resolveContainingFunction` treats
                        // a failed text match as terminal, while the line-only
                        // fallback tolerates ±2 lines — strictly more robust for
                        // a locator that names a binding, not an expression.
                        expression_text: None,
                        expression_line: None,
                        infer_kind: InferKind::FunctionParam,
                        alias: Some(alias),
                        param_name: Some(text.clone()),
                    },
                };
                requests.push(request);
            }
        }
        debug!(
            "[FileOrchestrator] Collected {} pub/sub payload infer requests",
            requests.len()
        );
        requests
    }

    /// Build `SymbolRequest`s for GraphQL consumer result-type anchors (#248
    /// consumer side).
    ///
    /// Near-copy of `collect_socket_type_requests`: it routes the deterministic
    /// consumer anchor — the TS result type bound at the `client.request<T>(DOC)`
    /// call site (`GraphqlOp::payload_type_symbol`) — through the *same* sidecar
    /// bundle path the HTTP explicit-symbol and socket cases use. Only consumers
    /// carry a `payload_type_symbol` (SDL producers anchor on their SDL type
    /// expression, not a bundled TS symbol), so this is consumer-only.
    ///
    /// When `payload_type_symbol` is absent, this falls back to
    /// `GraphqlOp::consumer_located_type_symbol` (#268): the co-located result
    /// type the file-analyzer located for a document with no explicit call-site
    /// generic. Same `SymbolRequest` shape, same alias — the sidecar bundle path
    /// can't tell the two anchors apart, and doesn't need to. The engine merge's
    /// isolation guard already guarantees an op never carries both.
    ///
    /// The alias MUST be `build_manifest_type_alias(&op.key, Consumer, Response)`
    /// — byte-identical to the alias `add_protocol_manifest_entry` stamped on the
    /// manifest entry in `append_protocol_manifest_entries` — or the resolved
    /// `.d.ts` never joins back and the entry stays `Unknown`. This contract is
    /// guarded by a unit test.
    ///
    /// An absent source means the symbol is declared in the consuming file, so it
    /// is resolved against that file's absolute path (same-file fallback). For
    /// the #268 fallback, "the consuming file" is `op.file_path` itself — unlike
    /// the producer type-locate (which has a separate `resolver_file` the
    /// backing type may differ from), the consumer join is scoped per-file, so
    /// the located type always resolves against the same file the document
    /// lives in.
    pub fn collect_graphql_type_requests(
        &self,
        graphql: &crate::graphql::GraphqlExtraction,
        repo_path: &str,
    ) -> Vec<SymbolRequest> {
        let repo_root = std::path::Path::new(repo_path);
        let repo_root_absolute = if repo_root.is_absolute() {
            repo_root.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(repo_root))
                .unwrap_or_else(|_| repo_root.to_path_buf())
                .canonicalize()
                .unwrap_or_else(|_| repo_root.to_path_buf())
        };

        let mut requests: Vec<SymbolRequest> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for op in &graphql.consumers {
            // #268: the deterministic call-site anchor (`payload_type_symbol`,
            // an explicit `client.request<T>(DOC)` generic) is the
            // higher-fidelity signal and always wins when present. Fall back to
            // the file-analyzer's located co-located type
            // (`consumer_located_type_symbol`) only when the deterministic pass
            // found nothing to anchor on — the engine merge's isolation guard
            // already guarantees the two are mutually exclusive per op, so this
            // fallback never silently shadows a real call-site type.
            let (symbol, source) = if let Some(symbol) = op.payload_type_symbol.as_ref() {
                (symbol, op.payload_type_source.as_ref())
            } else if let Some(symbol) = op.consumer_located_type_symbol.as_ref() {
                (symbol, op.consumer_located_type_source.as_ref())
            } else {
                continue;
            };
            let file_abs =
                Self::to_absolute_path(&op.file_path.to_string_lossy(), &repo_root_absolute);
            let source_file = match source {
                Some(import_source) => Self::resolve_import_path(&file_abs, import_source),
                // No import → same-file declaration: resolve against the file.
                None => file_abs,
            };
            let alias = build_manifest_type_alias(
                &op.key,
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
            );
            let dedup_key = format!("{}|{}|{}", source_file, symbol, alias);
            if seen.insert(dedup_key) {
                requests.push(SymbolRequest {
                    symbol_name: symbol.clone(),
                    source_file,
                    alias: Some(alias),
                    array_depth: None,
                    payload_borrow_witness: false,
                });
            }
        }
        let consumer_count = requests.len();

        // #248: SDL producer fields with no resolver but a co-located backing
        // type. Unlike the resolver path (a `FunctionReturn` infer whose concrete
        // return carries wrappers), this bundles the located element type and
        // wraps it in the SDL list depth (`Order` + `[Order!]!` → `Order[]`). The
        // symbol resolves against the file the entry came from (`resolver_file`),
        // following `response_type_source` when the type is imported.
        for op in &graphql.producers {
            let Some(symbol) = op.response_type_symbol.as_ref() else {
                continue;
            };
            let Some(entry_file) = op.resolver_file.as_ref() else {
                continue;
            };
            let file_abs =
                Self::to_absolute_path(&entry_file.to_string_lossy(), &repo_root_absolute);
            let source_file = match op.response_type_source.as_ref() {
                Some(import_source) => Self::resolve_import_path(&file_abs, import_source),
                None => file_abs,
            };
            let alias = build_manifest_type_alias(
                &op.key,
                ManifestRole::Producer,
                ManifestTypeKind::Response,
            );
            // SDL list depth carries the array-ness; the bundled element type
            // carries the shape. `0` (a non-list field) bundles as-is.
            let array_depth = op
                .primary_type_symbol
                .as_deref()
                .map(crate::graphql::graphql_list_depth)
                .filter(|&d| d > 0);
            let dedup_key = format!("{}|{}|{}", source_file, symbol, alias);
            if seen.insert(dedup_key) {
                requests.push(SymbolRequest {
                    symbol_name: symbol.clone(),
                    source_file,
                    alias: Some(alias),
                    array_depth,
                    payload_borrow_witness: false,
                });
            }
        }
        debug!(
            "[FileOrchestrator] Collected {} graphql type requests ({} consumer, {} producer type-locate)",
            requests.len(),
            consumer_count,
            requests.len() - consumer_count,
        );
        requests
    }

    /// Build `FunctionReturn` infer requests for GraphQL SDL producers whose
    /// resolver location was joined in from the file-analyzer (`graphql_operations`,
    /// Stage B1).
    ///
    /// Producers do NOT use the `SymbolRequest`/bundle path the consumer/socket
    /// anchors use: bundling the SDL anchor symbol (`ApiResponse`) would emit the
    /// still-generic wrapper, not the producer's real response contract. The
    /// producer's contract is the resolver function's RETURN type expanded
    /// (`Promise<ApiResponse<Order>>` → `{ data: …, errors }`), so this points an
    /// `InferKind::FunctionReturn` at the resolver's file/line — exactly the
    /// file-based-route handler path. The sidecar resolves the fn return,
    /// Promise/async-iterator-unwraps it, and structurally expands it.
    ///
    /// The alias MUST be `build_manifest_type_alias(&op.key, Producer, Response)`
    /// — byte-identical to the alias `add_protocol_manifest_entry` stamped on the
    /// producer manifest entry — or the inferred type never joins back and the
    /// entry stays `Unknown`. This is the load-bearing join, guarded by a unit
    /// test exactly as the consumer side is.
    ///
    /// Only producers with BOTH `resolver_file` and `resolver_line` set produce a
    /// request; an SDL producer with no matched LLM op stays inferred-from-nothing
    /// (it keeps its SDL anchor, but no expanded response contract).
    pub fn collect_graphql_producer_infer_requests(
        &self,
        graphql: &crate::graphql::GraphqlExtraction,
        repo_path: &str,
    ) -> Vec<InferRequestItem> {
        let repo_root = std::path::Path::new(repo_path);
        let repo_root_absolute = if repo_root.is_absolute() {
            repo_root.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(repo_root))
                .unwrap_or_else(|_| repo_root.to_path_buf())
                .canonicalize()
                .unwrap_or_else(|_| repo_root.to_path_buf())
        };

        let mut requests: Vec<InferRequestItem> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for op in &graphql.producers {
            let (Some(resolver_file), Some(resolver_line)) =
                (op.resolver_file.as_ref(), op.resolver_line)
            else {
                continue;
            };
            let file_abs =
                Self::to_absolute_path(&resolver_file.to_string_lossy(), &repo_root_absolute);
            let alias = build_manifest_type_alias(
                &op.key,
                ManifestRole::Producer,
                ManifestTypeKind::Response,
            );
            let dedup_key = format!("{}|{}|{}", file_abs, resolver_line, alias);
            if seen.insert(dedup_key) {
                requests.push(InferRequestItem {
                    file_path: file_abs,
                    line_number: resolver_line,
                    span_start: None,
                    span_end: None,
                    expression_text: None,
                    expression_line: None,
                    infer_kind: InferKind::FunctionReturn,
                    alias: Some(alias),
                    param_name: None,
                });
            }
        }
        debug!(
            "[FileOrchestrator] Collected {} graphql producer infer requests",
            requests.len()
        );
        requests
    }

    /// Parse a file once and extract both the symbol table and the env-var
    /// alias map (`local const -> process.env name`). Sharing the parse keeps
    /// the per-file CPU cost flat — both passes are cheap AST walks.
    /// Pub/sub Part B: does this file import a package the cloud
    /// /framework-detect step flagged as a messaging client?
    ///
    /// Resolve a RELATIVE import specifier (`./x`, `../y/z`) from `importer`
    /// to the canonical path of an existing file, trying the TypeScript
    /// resolution order for extension-less specifiers (#369). Non-relative
    /// specifiers (packages, tsconfig aliases) return `None` — alias
    /// resolution needs the sidecar's tsconfig knowledge and is out of scope
    /// here.
    pub(crate) fn resolve_relative_import(importer: &Path, spec: &str) -> Option<PathBuf> {
        if !(spec.starts_with("./") || spec.starts_with("../")) {
            return None;
        }
        let base = importer.parent()?.join(spec);
        // NodeNext/ESM: `./helper.js` names the emitted JS but the source on
        // disk is `helper.ts`. Reuse the one substitution table
        // (`ts_sibling_candidates`, carrick#148) that `canonicalize_or_probe`
        // already resolves through, so the two resolvers cannot drift again
        // (#468). Same early return as `canonicalize_or_probe`: for a
        // JS-family specifier the TS sources are the whole candidate set, with
        // the literal JS last for genuinely JS-only modules.
        if let Some(siblings) = base.to_str().and_then(Self::ts_sibling_candidates) {
            return siblings
                .into_iter()
                .map(PathBuf::from)
                .find_map(|c| c.is_file().then(|| c.canonicalize().ok())?);
        }
        let mut candidates: Vec<PathBuf> = Vec::new();
        if base.extension().is_some() {
            candidates.push(base.clone());
        }
        for ext in ["ts", "tsx", "js", "mjs", "cjs"] {
            let mut with_ext = base.as_os_str().to_owned();
            with_ext.push(".");
            with_ext.push(ext);
            candidates.push(PathBuf::from(with_ext));
        }
        for index in [
            "index.ts",
            "index.tsx",
            "index.js",
            "index.mjs",
            "index.cjs",
        ] {
            candidates.push(base.join(index));
        }
        candidates
            .into_iter()
            .find_map(|c| c.is_file().then(|| c.canonicalize().ok())?)
    }

    /// Collect the module specifiers a file RE-EXPORTS from — `export * from
    /// "./x.js"`, `export * as ns from "./x.js"`, `export { a } from "./x.js"`.
    ///
    /// Ordinary imports are deliberately NOT collected: a module that merely
    /// imports a wrapper is not a stand-in for it, only a module that
    /// re-publishes its bindings is. Type-only re-exports (`export type { T }
    /// from …`, `export type * from …`) cannot carry a runtime fetch helper and
    /// are skipped. Purely structural — no name or path heuristics (#472).
    fn reexport_sources(file: &Path, cm: &Lrc<SourceMap>, handler: &Handler) -> Vec<String> {
        let Some(module) = parse_file(file, cm, handler) else {
            return Vec::new();
        };
        let mut sources: Vec<String> = Vec::new();
        for item in &module.body {
            match item {
                ModuleItem::ModuleDecl(ModuleDecl::ExportAll(export)) => {
                    if export.type_only {
                        continue;
                    }
                    sources.push(export.src.value.to_string());
                }
                ModuleItem::ModuleDecl(ModuleDecl::ExportNamed(export)) => {
                    let Some(src) = &export.src else {
                        continue; // `export { a }` — local, not a re-export
                    };
                    if export.type_only {
                        continue;
                    }
                    // `export { type A, type B } from …` carries no value binding.
                    let has_value_specifier = export.specifiers.iter().any(|spec| match spec {
                        ExportSpecifier::Named(named) => !named.is_type_only,
                        _ => true,
                    });
                    if !has_value_specifier {
                        continue;
                    }
                    sources.push(src.value.to_string());
                }
                _ => {}
            }
        }
        sources
    }

    /// How many re-export hops beyond the directly-resolved module the wrapper
    /// search follows (#472). `barrel -> impl` is one hop. Bounded because a
    /// deep chain is indistinguishable from a mis-resolution, and each hop
    /// costs a file parse.
    const WRAPPER_REEXPORT_MAX_HOPS: usize = 3;
    /// Hard cap on modules visited per import while following re-exports, so a
    /// wide barrel fan-out (`export *` × N, each re-exporting further) cannot
    /// turn one import into an unbounded parse storm.
    const WRAPPER_REEXPORT_MAX_VISITS: usize = 32;
    /// How many wrapper modules a single re-export chain may stand for before it
    /// is rejected outright (#472). A helper barrel fronts one or two modules
    /// that actually perform HTTP; a package's public-surface `index.ts` fronts
    /// dozens, and tells you nothing about which helper the importer uses.
    /// Rejecting rather than truncating keeps the decision structural — there is
    /// no principled way to pick N of 20 — and bounds the wrapper context a
    /// single import can add.
    const WRAPPER_REEXPORT_MAX_HITS: usize = 4;

    /// The wrapper modules a resolved import target stands for (#472).
    ///
    /// Normally that is the target itself. But with NodeNext specifier
    /// resolution fixed (#469), a wrapper import commonly resolves to a
    /// RE-EXPORT BARREL — a module whose whole body is `export * from "./…"`.
    /// A barrel raises zero HTTP candidates, so it is never in `wrapper_map`
    /// (built at the candidate gate), and the #369/#370 rescue stopped one hop
    /// short of the module that actually defines the fetch helper.
    ///
    /// So: on a miss, follow re-export declarations breadth-first up to
    /// `WRAPPER_REEXPORT_MAX_HOPS` hops and return every wrapper module reached,
    /// i.e. treat the re-exporting chain as aliases of the defining module.
    /// Visited canonical paths are tracked, so a barrel that re-exports itself
    /// (or a cycle of barrels) terminates. Behaviour is byte-identical to before
    /// whenever the resolved target is already in `wrapper_map`: the follow only
    /// runs on a miss.
    ///
    /// A chain that stands for more than `WRAPPER_REEXPORT_MAX_HITS` wrapper
    /// modules is rejected whole: that is a package's public-surface barrel, not
    /// an alias for one helper, and attaching its whole HTTP surface as "the
    /// wrapper this file uses" is noise.
    ///
    /// Results are sorted by canonical path so wrapper context is deterministic
    /// regardless of import-table iteration order.
    /// The same-repo modules `module_path` imports by a relative specifier,
    /// resolved to source files, sorted and deduplicated. Type-only imports
    /// count: a consumer that holds the client in a typed field imports it
    /// that way, and the member join reads the member off the module however
    /// the binding was declared.
    fn relative_import_targets(
        module_path: &Path,
        cm: &Lrc<SourceMap>,
        handler: &Handler,
    ) -> Vec<PathBuf> {
        let Some(module) = parse_file(module_path, cm, handler) else {
            return Vec::new();
        };
        let mut targets: Vec<PathBuf> = collect_import_sources(&module)
            .iter()
            .filter_map(|spec| Self::resolve_relative_import(module_path, spec))
            .collect();
        targets.sort();
        targets.dedup();
        targets
    }

    fn wrapper_modules_behind(
        resolved: &Path,
        self_canon: Option<&PathBuf>,
        wrapper_map: &HashMap<PathBuf, WrapperModule>,
        reexport_cache: &mut HashMap<PathBuf, Vec<String>>,
        cm: &Lrc<SourceMap>,
        handler: &Handler,
    ) -> Vec<PathBuf> {
        if wrapper_map.contains_key(resolved) {
            return vec![resolved.to_path_buf()];
        }
        let mut hits: Vec<PathBuf> = Vec::new();
        let mut visited: HashSet<PathBuf> = HashSet::new();
        visited.insert(resolved.to_path_buf());
        if let Some(importer) = self_canon {
            visited.insert(importer.clone());
        }
        let mut frontier: Vec<PathBuf> = vec![resolved.to_path_buf()];
        let mut budget = Self::WRAPPER_REEXPORT_MAX_VISITS;
        for _ in 0..Self::WRAPPER_REEXPORT_MAX_HOPS {
            if frontier.is_empty() || budget == 0 {
                break;
            }
            let mut next: Vec<PathBuf> = Vec::new();
            for module in &frontier {
                // Borrowed, not cloned: nothing in the loop body touches the
                // cache again, so the memoized specifiers are read in place.
                let specs: &[String] = reexport_cache
                    .entry(module.clone())
                    .or_insert_with(|| Self::reexport_sources(module, cm, handler));
                for spec in specs {
                    if budget == 0 {
                        break;
                    }
                    let Some(target) = Self::resolve_relative_import(module, spec) else {
                        continue;
                    };
                    if !visited.insert(target.clone()) {
                        continue;
                    }
                    budget -= 1;
                    if wrapper_map.contains_key(&target) {
                        hits.push(target);
                    } else {
                        next.push(target);
                    }
                }
            }
            frontier = next;
        }
        // Too wide to stand for one helper — or too wide to have been measured
        // at all, because the visit budget ran out before the chain was fully
        // walked. Both are the same verdict: this is a module surface, not a
        // wrapper alias.
        if hits.len() > Self::WRAPPER_REEXPORT_MAX_HITS || budget == 0 {
            return Vec::new();
        }
        hits.sort();
        hits
    }

    /// An import source matches a `messaging_clients` entry when it is exactly
    /// the entry (`"nats"`) or a subpath/scoped specifier under it
    /// (`"@nats-io/nats-core"` matches `"@nats-io/nats-core"`; `"nats/foo"`
    /// matches `"nats"`). Same matching convention as the data-fetcher
    /// import-recall check, so it generalizes to any package without a hardcoded
    /// list. INERT today: `messaging_clients` is empty until the cloud deploys,
    /// so this always returns `false` and skip behavior is unchanged.
    fn imports_messaging_client(import_sources: &[String], messaging_clients: &[String]) -> bool {
        if messaging_clients.is_empty() {
            return false;
        }
        import_sources.iter().any(|src| {
            messaging_clients
                .iter()
                .any(|pkg| src == pkg || src.starts_with(&format!("{}/", pkg)))
        })
    }

    fn extract_symbol_table(
        file_path: &Path,
        cm: &Lrc<SourceMap>,
        handler: &Handler,
    ) -> FileSymbols {
        let Some(module) = parse_file(file_path, cm, handler) else {
            return FileSymbols::default();
        };

        let mut import_extractor = ImportSymbolExtractor::new();
        module.visit_with(&mut import_extractor);

        let mut type_extractor = TypeSymbolExtractor::new();
        module.visit_with(&mut type_extractor);

        let bindings = EnvAliasExtractor::build_bindings(&module);
        // Read on this parse rather than a second one: every file is a
        // candidate wrapper for some other file, and a client module whose
        // requests go through a helper raises no HTTP candidate of its own, so
        // the wrapper map's candidate gate is the wrong filter for this.
        let request_members = collect_request_members(&module, cm);

        FileSymbols {
            table: SymbolTable {
                local_types: type_extractor.type_symbols,
                imported_symbols: import_extractor.imported_symbols,
            },
            env_aliases: bindings.aliases,
            whole_url_fallbacks: bindings.whole_url_fallbacks,
            env_fallbacks: bindings.env_fallbacks,
            literal_bases: bindings.literal_bases,
            request_members,
        }
    }

    /// Strip a trailing TypeScript/JavaScript source-file extension from a module
    /// specifier, returning `Some(stripped)` only when one was present. Import
    /// specifiers CAN legitimately carry extensions (NodeNext/bundler ESM writes
    /// `./foo.js`), so this alone never decides anything: the caller rewrites
    /// only when the extension-less form matches the AST import table, i.e. when
    /// the source demonstrably imported without the extension and the suffix was
    /// added by the model.
    fn strip_source_file_extension(spec: &str) -> Option<&str> {
        for ext in [
            ".d.ts", ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs",
        ] {
            if let Some(stripped) = spec.strip_suffix(ext) {
                return Some(stripped);
            }
        }
        None
    }

    /// Normalize a spurious source-file extension the lite model sometimes
    /// appends to `type_import_source` (e.g. `../types/events.ts`). We rewrite
    /// ONLY when the extension-less form matches the symbol's import-table entry,
    /// so a specifier that legitimately lacks an extension — a scoped package
    /// like `@metamask/network-controller`, a same-file type — is left untouched.
    /// Deterministic and framework-agnostic: the AST import table is the source
    /// of truth, never a hard-coded library list.
    fn normalize_import_extension(
        primary: &Option<String>,
        source: &mut Option<String>,
        symbol_table: &SymbolTable,
    ) {
        let (Some(symbol), Some(src)) = (primary.as_deref(), source.as_deref()) else {
            return;
        };
        let Some(stripped) = Self::strip_source_file_extension(src) else {
            return;
        };
        let root = symbol
            .split_once('.')
            .map(|(root, _)| root)
            .unwrap_or(symbol);
        if let Some(imported) = symbol_table.imported_symbols.get(root)
            && imported.source.as_str() == stripped
        {
            *source = Some(stripped.to_string());
        }
    }

    /// Suppress a `data_call.primary_type_symbol` that borrows the REQUEST
    /// body's type (flake pattern 2, #361).
    ///
    /// `primary_type_symbol` on a data call names the call's RESULT type. When
    /// the result is un-annotated but the request payload has a named type
    /// (`axios.post(url, event)` with `event: AuditEvent`), the lite model
    /// intermittently emits the request type (`AuditEvent`) into that
    /// response-only slot. This corrupts the consumer's type contract with a
    /// symbol the call never produces.
    ///
    /// The fix is deterministic and evidence-gated, mirroring
    /// `normalize_import_extension`: we suppress ONLY when the emitted symbol is
    /// demonstrably the request payload's declared type AND there is no
    /// response-side type evidence. A shared request/response type that is
    /// legitimately annotated — a call generic (`axios.post<AuditEvent>(...)`)
    /// or an annotated result binding (`const r: AuditEvent = await ...`) — is
    /// left untouched. When the payload is not a bare identifier (an object
    /// literal, a member expression, a call) we cannot resolve its type from the
    /// AST, so we never touch the row. We normalize against evidence, never
    /// guess; the worst case is a missed suppression, never a nulled real type.
    fn suppress_borrowed_request_types(result: &mut FileAnalysisResult, file_path: &Path) {
        // Gate: only parse when a data call has both an emitted symbol and a
        // bare-identifier payload (the only shape whose type we can resolve).
        let needs_parse = result.data_calls.iter().any(|dc| {
            dc.primary_type_symbol.is_some()
                && dc
                    .payload_expression_text
                    .as_deref()
                    .and_then(payload_bare_ident)
                    .is_some()
        });
        if !needs_parse {
            return;
        }

        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
        let Some(module) = parse_file(file_path, &cm, &handler) else {
            return;
        };
        let mut collector = BindingTypeCollector::default();
        module.visit_with(&mut collector);

        for dc in &mut result.data_calls {
            let Some(symbol) = dc.primary_type_symbol.as_deref() else {
                continue;
            };
            let Some(payload_ident) = dc
                .payload_expression_text
                .as_deref()
                .and_then(payload_bare_ident)
            else {
                continue;
            };
            // The emitted (response) symbol must BE the request payload's
            // declared type for this to be a borrow.
            if collector
                .binding_types
                .get(payload_ident)
                .map(String::as_str)
                != Some(symbol)
            {
                continue;
            }
            // Response-side evidence that the type is legitimate, not borrowed:
            // an explicit call generic, or any call-initialized binding in the
            // file whose annotation MENTIONS this symbol at any depth
            // (`const r: Response<AuditEvent> = await ...` counts for
            // `AuditEvent` — envelope wrappers never hide real evidence).
            let has_response_evidence = dc
                .call_expression_text
                .as_deref()
                .is_some_and(|text| call_text_has_type_generic(text, symbol))
                || collector.call_annotated_syms.contains(symbol);
            if has_response_evidence {
                continue;
            }
            // Borrow with no response evidence: drop it so the type falls to
            // Unknown / sidecar inference instead of the wrong request type.
            dc.primary_type_symbol = None;
            dc.type_import_source = None;
        }
    }

    /// Repair a graphql-over-HTTP `data_call.target` that the model reported as
    /// the shared transport URL instead of the operation identity (flake
    /// pattern 3, #361).
    ///
    /// A `client.request(TICKET_QUERY, ...)` call whose client points at a
    /// shared endpoint (`${SUPPORT_GQL_URL}/graphql`) intermittently yields the
    /// URL as `target` rather than the invoked operation. GraphQL matches on
    /// operation identity, not the connection URL, so a URL target is a dead
    /// key. The document dispatched at the call site is deterministically
    /// derivable — `graphql::document_operation_keys` maps the document binding
    /// (`TICKET_QUERY`) to its canonical operation key (`graphql|query|ticket`),
    /// exactly the form the operation matcher joins on — so we rewrite the
    /// target to it.
    ///
    /// Reuses #310's site-matching principle (document identity), not a URL
    /// heuristic: we rewrite only a transport-shaped target whose verbatim call
    /// text names exactly one tracked gql document. If zero or several documents
    /// match, the operation can't be derived unambiguously and the row is left
    /// untouched (no guessing). The rewritten `graphql|…` target is not a valid
    /// route shape, so — like the transport call the #310 fold already drops —
    /// it never leaks into the HTTP graph; the real consumer op comes from the
    /// deterministic GraphQL scan.
    fn rewrite_graphql_document_targets(result: &mut FileAnalysisResult, file_path: &Path) {
        // Gate: only parse when a data call has a transport-shaped target and
        // verbatim call text to match a document binding against.
        let needs_parse = result
            .data_calls
            .iter()
            .any(|dc| is_transport_shaped_target(&dc.target) && dc.call_expression_text.is_some());
        if !needs_parse {
            return;
        }

        let keys = crate::graphql::document_operation_keys(file_path);
        if keys.is_empty() {
            return;
        }

        for dc in &mut result.data_calls {
            if !is_transport_shaped_target(&dc.target) {
                continue;
            }
            let Some(call_text) = dc.call_expression_text.as_deref() else {
                continue;
            };
            // Exactly one tracked document named in the call text — otherwise
            // the operation is ambiguous and we do not guess.
            let mut matched = keys
                .iter()
                .filter(|(doc, _)| contains_word(call_text, doc))
                .map(|(_, key)| key);
            if let Some(key) = matched.next()
                && matched.next().is_none()
                && dc.target != *key
            {
                dc.target = key.clone();
            }
        }
    }

    /// Drop a `pubsub_operations` entry whose topic has no textual witness in
    /// the analyzed file's source (carrick#311).
    ///
    /// The file-analyzer occasionally invents a topic from a wrapper-function
    /// NAME: `worker.ts` calls `publishStatusChanged(evt)` (imported from
    /// `./status.publisher`) and the model emits a phantom
    /// `pubsub|status.changed` op even though no such string exists anywhere in
    /// the file. The real operation is extracted from the file that holds the
    /// literal, so the phantom is pure precision noise. Enforcing the existing
    /// only-literal-topics extraction contract (`append_pubsub_operations` doc)
    /// deterministically: a kept topic must equal a string literal in the file
    /// or fit the static parts of a composed string (template literal or `+`
    /// concatenation; see [`PubsubTopicWitnessCollector`]). Runs BEFORE
    /// `merge_pubsub_anchor_ops`, so deterministic anchor ops (carrick#387) are
    /// never candidates for the drop. An unparseable file yields no witness
    /// evidence either way, so everything is kept (fail open, like the other
    /// re-parse guards). Returns the number of ops dropped.
    fn suppress_phantom_pubsub_topics(result: &mut FileAnalysisResult, file_path: &Path) -> usize {
        // Gate: only re-parse when there is a pub/sub op to check.
        if result.pubsub_operations.is_empty() {
            return 0;
        }

        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
        let Some(module) = parse_file(file_path, &cm, &handler) else {
            return 0;
        };
        let mut witnesses = PubsubTopicWitnessCollector::default();
        module.visit_with(&mut witnesses);

        let before = result.pubsub_operations.len();
        result.pubsub_operations.retain(|op| {
            let witnessed = witnesses.witnessed(&op.topic);
            if !witnessed {
                debug!(
                    topic = %op.topic,
                    file = %file_path.display(),
                    "pub/sub topic has no literal witness in the file; dropping phantom op"
                );
            }
            witnessed
        });
        before - result.pubsub_operations.len()
    }

    fn validate_type_hints(result: &mut FileAnalysisResult, symbol_table: &SymbolTable) {
        let validate = |primary: &mut Option<String>, source: &mut Option<String>| {
            // Normalize a spurious `.ts`/`.js` suffix first so a hint that only
            // differs from its import by the extension is kept (not nulled below).
            Self::normalize_import_extension(primary, source, symbol_table);

            let Some(symbol) = primary.as_ref() else {
                *source = None;
                return;
            };

            let (root, has_member) = symbol
                .split_once('.')
                .map(|(root, _)| (root, true))
                .unwrap_or((symbol.as_str(), false));

            if symbol_table.local_types.contains(root) {
                if source.is_none() && !has_member {
                    return;
                }
            } else if let Some(imported) = symbol_table.imported_symbols.get(root) {
                let source_matches = source
                    .as_deref()
                    .map(|value| value == imported.source.as_str())
                    .unwrap_or(false);
                let namespace_ok = if imported.kind == SymbolKind::Namespace {
                    has_member
                } else {
                    !has_member
                };
                if source_matches && namespace_ok {
                    return;
                }
            }

            *primary = None;
            *source = None;
        };

        for endpoint in &mut result.endpoints {
            validate(
                &mut endpoint.primary_type_symbol,
                &mut endpoint.type_import_source,
            );
        }

        for data_call in &mut result.data_calls {
            validate(
                &mut data_call.primary_type_symbol,
                &mut data_call.type_import_source,
            );
        }

        // Pub/sub parity for the reject check above, with two deliberate
        // deviations from the HTTP closure (which stays byte-identical):
        //
        // 1. AST source-overwrite (pub/sub ONLY): when the symbol root passes
        //    the AST check, the import table is the source of truth for
        //    `type_import_source`, so we overwrite it instead of rejecting on
        //    a source mismatch. The HTTP side must NOT get this overwrite: it
        //    would turn null-to-infer into keep-explicit and could rescue
        //    borrowed symbols, which needs a decoy-fixture A/B first.
        //
        // 2. Guarded demote: a failing symbol is demoted (symbol AND source
        //    both to null) ONLY when the op carries a payload locator that the
        //    envelope guard in `collect_pubsub_infer_requests` will accept
        //    (text present and not containing the op's own topic), so the
        //    demoted op is guaranteed to fall through to location-based
        //    inference rather than being dropped. Otherwise the suspect
        //    symbol is kept as the recall floor. This targets the measured
        //    wrong-symbol borrow class (13/20 on the c1 decoy prompt): a
        //    present symbol takes the explicit path and is never rescued by
        //    inference, so a confidently wrong resolve was previously
        //    unrecoverable.
        for op in &mut result.pubsub_operations {
            Self::normalize_import_extension(
                &op.primary_type_symbol,
                &mut op.type_import_source,
                symbol_table,
            );

            let Some(symbol) = op.primary_type_symbol.as_ref() else {
                // Hygiene parity with the HTTP closure and the schema
                // invariant (source is null whenever the symbol is null): a
                // source with no symbol anchors nothing, so clear it rather
                // than leave an inconsistent pair.
                op.type_import_source = None;
                continue;
            };

            let (root, has_member) = symbol
                .split_once('.')
                .map(|(root, _)| (root, true))
                .unwrap_or((symbol.as_str(), false));

            if symbol_table.local_types.contains(root) {
                if !has_member {
                    // Locally declared: by definition there is no import
                    // source. Overwrite whatever the model said.
                    op.type_import_source = None;
                    continue;
                }
            } else if let Some(imported) = symbol_table.imported_symbols.get(root) {
                let namespace_ok = if imported.kind == SymbolKind::Namespace {
                    has_member
                } else {
                    !has_member
                };
                if namespace_ok {
                    op.type_import_source = Some(imported.source.clone());
                    continue;
                }
            }

            let demote_survives_envelope_guard = op
                .payload_expression_text
                .as_deref()
                .is_some_and(|text| !text.contains(op.topic.as_str()));
            if demote_survives_envelope_guard {
                debug!(
                    topic = %op.topic,
                    symbol = %symbol,
                    "pub/sub type symbol failed the AST check and a usable \
                     payload locator exists; demoting to location-based inference"
                );
                op.primary_type_symbol = None;
                op.type_import_source = None;
            }
        }
    }

    /// Normalize unusable type hints from the LLM so we can force inference instead of padding unknowns.
    ///
    /// Checks BOTH `type_import_source` AND `primary_type_symbol` against all detected frameworks.
    /// This prevents the LLM from using framework namespace types (e.g., `express`, `fastify`)
    /// as payload types, which would resolve to the framework's root namespace instead of actual data.
    fn normalize_unusable_types(result: &mut FileAnalysisResult, frameworks: &[String]) {
        let scrub = |primary: &mut Option<String>, source: &mut Option<String>| {
            // Check type_import_source against ALL detected frameworks
            if let Some(src) = source.as_deref()
                && frameworks.iter().any(|f| f == src)
            {
                *primary = None;
                *source = None;
                return;
            }
            // Check primary_type_symbol: if it matches a framework package name
            // (the default import), it's a framework namespace, not a payload type
            if let Some(sym) = primary.as_deref() {
                let sym_lower = sym.to_lowercase();
                if frameworks.iter().any(|f| f.to_lowercase() == sym_lower) {
                    *primary = None;
                    *source = None;
                }
            }
        };

        for endpoint in &mut result.endpoints {
            scrub(
                &mut endpoint.primary_type_symbol,
                &mut endpoint.type_import_source,
            );
        }
        for call in &mut result.data_calls {
            scrub(&mut call.primary_type_symbol, &mut call.type_import_source);
        }
    }

    /// Derive endpoints for a file whose route is declared by its location in
    /// the project layout (file-based routing) rather than by a call expression
    /// the SWC gatekeeper can see. `derive_route` supplies the path from the
    /// filesystem; the exported handler extractor supplies the HTTP methods and
    /// declaration spans. Neither is recoverable from a call-site scan, so these
    /// endpoints are built deterministically.
    ///
    /// Payload/response *symbol* fields are left empty here: the structural
    /// facts (method and path) are owned at synthesis time, while the response
    /// type is recovered downstream in `collect_type_requests`, which asks the
    /// sidecar for the handler's (Promise-unwrapped) return type — the response
    /// contract for a file-based route.
    ///
    /// `pub` + `#[doc(hidden)]`: this is exposed only so the end-to-end fixture
    /// test (`tests/file_based_routing_test.rs`) can drive the real synthesis
    /// path. It is not part of the supported crate API.
    #[doc(hidden)]
    pub fn file_based_endpoints(
        scanner: &SwcScanner,
        rel_path: &Path,
        file_path: &Path,
        content: &str,
        conventions: &[RoutingConvention],
    ) -> Vec<EndpointResult> {
        let Some(route) = derive_route(rel_path, conventions) else {
            return Vec::new();
        };

        match route.method_source {
            // App-router style: one exported function per HTTP method. The export
            // name *is* the method (GET/POST/...), and its declaration span lets
            // the sidecar locate the handler body later.
            MethodSource::ExportName => scanner
                .exported_handlers(file_path, content)
                .into_iter()
                .flat_map(|h| {
                    // An export named for a method *is* that method; a
                    // convention whose route modules name their handlers for a
                    // role instead (a read export, a write export) maps those
                    // names itself, and a method guard inside the body narrows
                    // that default to the verbs actually served (carrick#601).
                    // Either way the endpoint is anchored on the export's own
                    // span, so an exported binding initialized from a call — a
                    // handler built by a route-builder factory rather than
                    // declared as a function — anchors exactly like a function
                    // declaration does (carrick#473). The anchor stays the
                    // export span, never the guard line: the sidecar's
                    // return-type lookup is keyed on it.
                    let methods = route.http_methods_for_export(
                        &h.name,
                        &h.method_guards,
                        &h.declared_methods,
                    );
                    methods
                        .into_iter()
                        .map(|method| EndpointResult {
                            candidate_id: format!("file-route:{}:{}", method, h.span_start),
                            line_number: h.line_number as i32,
                            owner_node: FILE_BASED_ROUTE_OWNER.to_string(),
                            method,
                            path: route.path.clone(),
                            handler_name: h.name.clone(),
                            pattern_matched: route.convention.clone(),
                            call_expression_span_start: Some(h.span_start),
                            call_expression_span_end: Some(h.span_end),
                            payload_expression_text: None,
                            payload_expression_line: None,
                            response_expression_text: None,
                            response_expression_line: None,
                            emission_style: None,
                            primary_type_symbol: None,
                            type_import_source: None,
                        })
                        .collect::<Vec<_>>()
                })
                .collect(),
            // Pages-router style: a single default export serves every method. The
            // concrete method set isn't recoverable from the layout alone, so we
            // leave these to a follow-up rather than emit an endpoint with an
            // unknown method (which the mount graph would drop anyway).
            MethodSource::DefaultExport => Vec::new(),
        }
    }

    /// Build deterministic endpoints for routes declared as data
    /// (`{ method: 'GET', path: '/health', handler: healthCheckHandler }` in a
    /// registry array). The method, path, and handler owner are all structural
    /// facts the file-analyzer prompt ignores (it only matches framework-call
    /// patterns), so they are emitted directly instead of through the LLM (#234).
    ///
    /// The owner is the handler identifier (`healthCheckHandler`), never the
    /// HTTP-method literal — the owner-fabrication trap (#227). The descriptor
    /// path is already absolute and carries no mount chain, so (like file-based
    /// routes) the owner resolves to no mount prefix and the path is used as-is.
    /// Descriptors with no resolvable handler fall back to a sentinel owner.
    fn route_descriptor_endpoints(
        scanner: &SwcScanner,
        file_path: &Path,
        content: &str,
    ) -> Vec<EndpointResult> {
        scanner
            .route_descriptor_endpoints(file_path, content)
            .into_iter()
            .filter(|d| is_http_method(&d.method))
            .map(|d: RouteDescriptorEndpoint| {
                let method = d.method.to_uppercase();
                let handler = d
                    .handler
                    .unwrap_or_else(|| ROUTE_DESCRIPTOR_OWNER.to_string());
                EndpointResult {
                    candidate_id: format!("route-descriptor:{}:{}", method, d.span_start),
                    line_number: d.line_number as i32,
                    owner_node: handler.clone(),
                    method,
                    path: d.path,
                    handler_name: handler,
                    pattern_matched: ROUTE_DESCRIPTOR_PATTERN.to_string(),
                    call_expression_span_start: Some(d.span_start),
                    call_expression_span_end: Some(d.span_end),
                    payload_expression_text: None,
                    payload_expression_line: None,
                    response_expression_text: None,
                    response_expression_line: None,
                    emission_style: None,
                    primary_type_symbol: None,
                    type_import_source: None,
                }
            })
            .collect()
    }

    /// Build the endpoints a route table declares by binding a path to an
    /// imported controller instance (#580 part b).
    ///
    /// A class-controller service splits every route across two files: the
    /// route table holds the path (`router('/widget', widget)`) and the
    /// controller module holds the method and the handler, naming its own path
    /// nowhere. Neither file states a route on its own, so the single-file
    /// analyzer — LLM or scanner — cannot see one, and the routes are simply
    /// absent from the index. Joining the two halves is the same cross-file job
    /// `resolve_mount_bindings` already does for an imported router, through
    /// the same [`BindingResolver`], so it is done the same way: resolve the
    /// bound identifier to the module that DECLARES it, never to whatever the
    /// importing file happened to call it.
    ///
    /// Returns `(controller file, endpoint)` pairs. The endpoint belongs to the
    /// controller's file, not the route table's: that is where the handler is,
    /// and it is what a reader following the index needs to open. Emitted with
    /// a sentinel-free owner — the class name — which matches no mount, so the
    /// bound path is used as-is (like a file-based or descriptor route).
    ///
    /// Silent on anything it cannot resolve structurally: a non-relative
    /// specifier, a module that default-exports something other than a class
    /// declared in it, or a class with no method answering an HTTP method.
    fn class_controller_endpoints(
        scanner: &SwcScanner,
        resolver: &mut BindingResolver,
        // The route table, canonicalized: `BindingResolver` resolves relative
        // to the importer and returns canonical paths, and on macOS a
        // non-canonical importer resolves to a path that matches nothing
        // (`/var` vs `/private/var`).
        router_file: &Path,
        bindings: &[ControllerRouteBinding],
    ) -> Vec<(PathBuf, EndpointResult)> {
        let mut endpoints = Vec::new();
        for binding in bindings {
            let Some(resolved) =
                resolver.resolve(router_file, &binding.import_source, &binding.binding)
            else {
                continue;
            };
            let Ok(content) = std::fs::read_to_string(&resolved.file) else {
                continue;
            };
            let Some(controller) =
                scanner.default_export_controller_class(&resolved.file, &content)
            else {
                debug!(
                    "Route {} binds {} at {}:{}, but its module default-exports no controller \
                     class: no endpoint emitted",
                    binding.path,
                    binding.binding,
                    resolved.file.display(),
                    binding.line_number
                );
                continue;
            };
            for method in controller.methods {
                endpoints.push((
                    resolved.file.clone(),
                    EndpointResult {
                        candidate_id: format!(
                            "class-controller:{}:{}",
                            method.http_method, method.span_start
                        ),
                        line_number: i32::try_from(method.line_number).unwrap_or(0),
                        owner_node: controller.name.clone(),
                        method: method.http_method,
                        path: binding.path.clone(),
                        handler_name: method.name,
                        pattern_matched: CLASS_CONTROLLER_PATTERN.to_string(),
                        call_expression_span_start: Some(method.span_start),
                        call_expression_span_end: Some(method.span_end),
                        payload_expression_text: None,
                        payload_expression_line: None,
                        response_expression_text: None,
                        response_expression_line: None,
                        emission_style: None,
                        primary_type_symbol: None,
                        type_import_source: None,
                    },
                ));
            }
        }
        endpoints
    }

    /// Merge the class-controller endpoints of a whole pass into `file_results`,
    /// keyed by the controller module each one belongs to (#580 part b).
    ///
    /// Runs after every file's own result is in, so a controller's rows are
    /// never overwritten by the pass that analyses that controller file. The
    /// `file_results` key for a controller is the key its own file was analysed
    /// under — resolved canonically, because a key and a resolved import can
    /// name the same file by different paths — so a controller that also
    /// carries call-site endpoints keeps one entry, not two.
    ///
    /// Returns the number of endpoints added.
    fn merge_class_controller_endpoints(
        file_results: &mut HashMap<String, FileAnalysisResult>,
        // The files this pass scanned, so a controller module that produced no
        // result of its own is still keyed the way every other file is.
        scanned_files: &[PathBuf],
        endpoints: Vec<(PathBuf, EndpointResult)>,
    ) -> usize {
        let mut key_by_canonical: HashMap<PathBuf, String> = HashMap::new();
        for file in scanned_files {
            if let Ok(canonical) = file.canonicalize() {
                key_by_canonical.insert(canonical, file.to_string_lossy().to_string());
            }
        }

        let mut added = 0;
        for (controller_file, endpoint) in endpoints {
            let key = key_by_canonical
                .get(&controller_file)
                .cloned()
                .unwrap_or_else(|| controller_file.to_string_lossy().to_string());
            let result = file_results.entry(key).or_default();
            // Deduped on method + path + line, not method + path: one class
            // legitimately serves the same method at several paths, and the
            // line is what distinguishes this row's handler from another
            // extraction of the same route. A row already at this exact
            // position is the same fact twice.
            let duplicate = result.endpoints.iter().any(|existing| {
                existing.method.eq_ignore_ascii_case(&endpoint.method)
                    && existing.path == endpoint.path
                    && existing.line_number == endpoint.line_number
            });
            if !duplicate {
                result.endpoints.push(endpoint);
                added += 1;
            }
        }
        added
    }

    /// Append structurally derived endpoints (file-based routes and
    /// route-descriptor data) the LLM pass didn't already produce (matched by
    /// method + path), keeping the deterministic entries. Returns the number
    /// actually added.
    fn merge_file_based_endpoints(
        result: &mut FileAnalysisResult,
        route_endpoints: Vec<EndpointResult>,
    ) -> usize {
        let mut added = 0;
        for ep in route_endpoints {
            let duplicate = result
                .endpoints
                .iter()
                .any(|e| e.method.eq_ignore_ascii_case(&ep.method) && e.path == ep.path);
            if !duplicate {
                result.endpoints.push(ep);
                added += 1;
            }
        }
        added
    }

    /// Merge deterministically-anchored pub/sub operations (carrick#387) into a
    /// file's extraction, adding only the ops the LLM pass missed. An anchor is
    /// considered covered — and skipped — exactly when the extraction already
    /// carries its contribution: an op with the same (topic, role) pair, at any
    /// line (the matching join is topic-keyed; one op per side suffices). Line
    /// numbers deliberately play no part in coverage: a line can carry several
    /// pub/sub ops, and an extracted op that shares the anchor's line but not
    /// its topic (e.g. a template the model kept verbatim) does not put the
    /// resolved topic on the wire — the anchor still must (Copilot review on
    /// #389). Backfilled ops carry no type judgment: `primary_type_symbol` /
    /// `broker` stay `None`. The two-arg `subscribe("topic", handler)` shape
    /// (carrick#402 c) additionally recorded its inline handler's first param,
    /// which lands here as the FunctionParam payload locator
    /// (`payload_expression_text`/`_line`) that
    /// `collect_pubsub_infer_requests` routes through the sidecar; every other
    /// anchor shape is payload-less and keeps all judgment fields `None` —
    /// pure topic recall.
    fn merge_pubsub_anchor_ops(
        result: &mut FileAnalysisResult,
        anchor_ops: Vec<PubsubAnchorOp>,
    ) -> usize {
        let mut added = 0;
        for anchor in anchor_ops {
            let line = i32::try_from(anchor.line_number).unwrap_or(i32::MAX);
            let covered = result
                .pubsub_operations
                .iter()
                .any(|op| op.topic == anchor.topic && op.role == Some(anchor.role));
            if covered {
                continue;
            }
            debug!(
                "Backfilling pub/sub op the extraction missed: {} ({:?}) at line {}",
                anchor.topic, anchor.role, line
            );
            result.pubsub_operations.push(PubsubOperation {
                topic: anchor.topic,
                role: Some(anchor.role),
                line_number: line,
                primary_type_symbol: None,
                type_import_source: None,
                broker: None,
                payload_expression_text: anchor.handler_param,
                payload_expression_line: anchor
                    .handler_param_line
                    .and_then(|l| i32::try_from(l).ok()),
            });
            added += 1;
        }
        added
    }

    /// Canonicalize a route path to colon-style params so structurally identical
    /// endpoints from the LLM pass (`/w/[slug]`) and the file-based router
    /// (`/w/:slug`) dedupe to one entry instead of both surviving and flipping
    /// form between non-deterministic scans. `[id]` -> `:id`, `[...rest]` -> `**`;
    /// `:id`, `*`, and literal segments are left unchanged (idempotent).
    /// Segment whitespace is trimmed and a whitespace-only path collapses to `/`:
    /// the LLM emits root routes as `"/ "` and the space otherwise survives into
    /// `full_path`, breaking matching both ways (#332).
    fn canonicalize_route_path(path: &str) -> String {
        let mut out = String::with_capacity(path.len());
        for (i, seg) in path.split('/').enumerate() {
            if i > 0 {
                out.push('/');
            }
            let seg = seg.trim();
            // Catch-all (`[...rest]`) and optional catch-all (`[[...rest]]`) both
            // map to the router's `**`; ordinary dynamic segments (`[id]`, `[[id]]`)
            // map to `:id`.
            let is_catch_all = (seg.starts_with("[...") && seg.ends_with(']'))
                || (seg.starts_with("[[...") && seg.ends_with("]]"));
            if is_catch_all {
                out.push_str("**");
            } else if seg.len() > 2 && seg.starts_with('[') && seg.ends_with(']') {
                // trim() mirrors the router's sanitize_param so whitespace-jittered
                // LLM output (`[ slug ]`) still dedupes against the router's `:slug`.
                let inner = seg.trim_matches(|c| c == '[' || c == ']').replace('.', "");
                out.push(':');
                out.push_str(inner.trim());
            } else {
                out.push_str(seg);
            }
        }
        // Empty or whitespace-only input canonicalizes to the root route.
        if out.chars().all(|c| c == '/') {
            return "/".to_string();
        }
        out
    }

    /// Rewrite every LLM-emitted endpoint path to the canonical colon form before
    /// the file-based merge, so the structural router entries dedupe against them.
    fn canonicalize_endpoint_paths(result: &mut FileAnalysisResult) {
        for ep in &mut result.endpoints {
            let canon = Self::canonicalize_route_path(&ep.path);
            if canon != ep.path {
                ep.path = canon;
            }
        }
        // Mount prefixes come from the same LLM pass and feed join_paths the
        // same way; whitespace jitter there would poison every child full_path.
        // Plain trim only: an empty mount path (`app.use(middleware)`) is
        // meaningful and must not collapse to `/`.
        for mount in &mut result.mounts {
            let trimmed = mount.mount_path.trim();
            if trimmed != mount.mount_path {
                mount.mount_path = trimmed.to_string();
            }
        }
    }

    fn apply_candidate_map(
        result: &mut FileAnalysisResult,
        candidate_map: &HashMap<String, CandidateTarget>,
        file_path: &str,
    ) {
        // Endpoints: keep filter_map (endpoints without candidates are unreliable),
        // but a drop means an endpoint the LLM reported vanishes from the index —
        // log which, so silent loss is at least diagnosable.
        let mut dropped_endpoints: Vec<String> = Vec::new();
        result.endpoints = result
            .endpoints
            .drain(..)
            .filter_map(|mut endpoint| {
                let Some(candidate) = candidate_map.get(&endpoint.candidate_id) else {
                    dropped_endpoints.push(format!(
                        "{} {} (candidate_id '{}')",
                        endpoint.method, endpoint.path, endpoint.candidate_id
                    ));
                    return None;
                };
                endpoint.line_number = candidate.line_number as i32;
                endpoint.call_expression_span_start = Some(candidate.span_start);
                endpoint.call_expression_span_end = Some(candidate.span_end);
                Self::reanchor_endpoint_path(&mut endpoint, candidate, file_path);
                Some(endpoint)
            })
            .collect();

        if !dropped_endpoints.is_empty() {
            warn!(
                "[FileOrchestrator] {} endpoint(s) in {} dropped — {}: {}",
                dropped_endpoints.len(),
                file_path,
                Self::drop_reason(candidate_map.len()),
                dropped_endpoints.join(", ")
            );
        }

        // Data calls: preserve even without candidate match (inline aliases still work)
        let mut dropped_count = 0;
        result.data_calls = result
            .data_calls
            .drain(..)
            .map(|mut data_call| {
                if let Some(candidate) = candidate_map.get(&data_call.candidate_id) {
                    data_call.line_number = candidate.line_number as i32;
                    data_call.call_expression_span_start = Some(candidate.span_start);
                    data_call.call_expression_span_end = Some(candidate.span_end);
                    Self::reanchor_data_call(&mut data_call, candidate, file_path);
                } else {
                    dropped_count += 1;
                }
                data_call
            })
            .collect();

        if dropped_count > 0 {
            warn!(
                "[FileOrchestrator] {} data call(s) in {} had no matching SWC candidate ({}, spans unavailable)",
                dropped_count,
                file_path,
                Self::drop_reason(candidate_map.len())
            );
        }
    }

    /// Why a reported operation failed the candidate join, phrased so the two
    /// causes are not confusable in a scan log.
    ///
    /// A file reaches the analyzer by one of two routes. Normally it raised
    /// HTTP candidates and they are offered to the analyzer as hints, so a
    /// `candidate_id` that is absent from the map really is an id the analyzer
    /// invented. But a file can also be *force-analyzed* with no HTTP
    /// candidates at all — the GraphQL resolver/consumer fall-throughs, the
    /// messaging-client fall-through, and the #369 wrapper rescue all do this,
    /// and the last one alone routes ~30% of the analyzed files on a large
    /// monorepo. For those the map is empty by construction, so EVERY reported
    /// operation is dropped and none of them says anything about analyzer
    /// accuracy.
    ///
    /// The count is of HTTP candidates specifically, since that is what
    /// `candidate_map` is built from: a force-analyzed GraphQL or messaging
    /// file may well have raised unrouted candidates of another protocol, so
    /// "no SWC candidates" would be the wrong claim.
    ///
    /// Reporting both as "no matching SWC candidate" made a force-analyzed
    /// file look like a wholesale extraction failure. The offered count is the
    /// only fact that separates them, so it goes in the message.
    fn drop_reason(http_candidates_offered: usize) -> String {
        if http_candidates_offered == 0 {
            "file was force-analyzed with no HTTP candidates offered, so nothing could join"
                .to_string()
        } else {
            format!(
                "candidate_id matched none of the {} HTTP candidate(s) offered for this file",
                http_candidates_offered
            )
        }
    }

    /// Re-anchor an LLM-emitted endpoint path to the registration call's
    /// first-arg string literal when the two disagree (#332). For root routes
    /// the LLM emits whitespace junk (`"/ "`) or copies a sibling route's path
    /// (`"/:id"`); the literal at the candidate the endpoint already points at
    /// is deterministic ground truth. An emitted path that merely EXTENDS the
    /// literal at a segment boundary (`/api/v1/status` vs `/status`) is kept:
    /// that is a constructor-carried prefix baked into the path, not a
    /// mis-copy (join_paths' idempotent guard depends on it surviving).
    fn reanchor_endpoint_path(
        endpoint: &mut EndpointResult,
        candidate: &CandidateTarget,
        file_path: &str,
    ) {
        let Some(literal) = Self::route_literal_from_snippet(candidate.path_snippet.as_deref())
        else {
            return;
        };
        let canon_literal = Self::canonicalize_route_path(&literal);
        let canon_path = Self::canonicalize_route_path(&endpoint.path);
        if canon_literal == canon_path {
            return;
        }
        // Baked-prefix escape hatch. A root literal never qualifies: every
        // path trivially "ends with" `/`, and the observed mis-copies are
        // exactly root routes.
        if canon_literal != "/"
            && let Some(rest) = canon_path.strip_suffix(&canon_literal)
            && (rest.is_empty() || rest.ends_with('/') || canon_literal.starts_with('/'))
        {
            return;
        }
        warn!(
            "[FileOrchestrator] Re-anchored endpoint path '{}' to registration literal '{}' ({}:{})",
            endpoint.path, canon_literal, file_path, candidate.line_number
        );
        endpoint.path = canon_literal;
    }

    /// Overrule the model's `target` and `method` with the structural request
    /// spec, for the calls that carry one (#537).
    ///
    /// `client({ method: "post", url: "/api/v1/login" })` states its method
    /// and its path as data. Both are AST facts, so neither is the model's to
    /// decide, and when the model has to guess at this shape it guesses badly:
    /// the observed failure was a wildcard path on every such call, which then
    /// matched a producer's SPA fallback.
    ///
    /// The one thing the model may know better is the BASE. A client built
    /// with a configured `baseURL` gives targets like `${API_URL}/api/v1/login`
    /// — the same path with the host the normalizer needs for internal/external
    /// classification in front of it. So a target that already ends with the
    /// spec's URL at a segment boundary is kept as-is; anything else is
    /// replaced. This is the same baked-prefix rule
    /// [`Self::reanchor_endpoint_path`] applies on the producer side.
    fn reanchor_data_call(
        data_call: &mut DataCallResult,
        candidate: &CandidateTarget,
        file_path: &str,
    ) {
        let Some(spec) = candidate.request_spec.as_ref() else {
            Self::reanchor_new_url_call(data_call, candidate, file_path);
            return;
        };

        if data_call.method.as_deref().map(str::trim) != Some(spec.method.as_str()) {
            data_call.method = Some(spec.method.clone());
        }

        // Both sides in the router spelling before they are compared: the spec
        // url already is, and a model that copied an OpenAPI-style `{param}`
        // out of the source is stating the same path (#529).
        let target = normalize_path_params(data_call.target.trim());
        if Self::target_carries_url(&target, &spec.url) {
            if target != data_call.target {
                data_call.target = target;
            }
            return;
        }

        warn!(
            "[FileOrchestrator] Re-anchored data call target '{}' to request-spec url '{}' ({}:{})",
            data_call.target, spec.url, file_path, candidate.line_number
        );
        data_call.target = spec.url.clone();
    }

    /// Anchor a call whose target is built as `new URL(path, base)` to the
    /// path that constructor states (carrick#610).
    ///
    /// The value the request receives is a binding, or a `.href` off one, so
    /// the site itself is not route-shaped and extraction has nothing to read
    /// the path off. What it wrote down is therefore either unusable (the
    /// binding name, dropped downstream for want of a route shape) or lifted
    /// from somewhere else in the file, which is how a client calling `v2` came
    /// to be recorded calling `v1`.
    ///
    /// Only the path is asserted. The base stays opaque, so the call matches by
    /// route path like every other host-free call, and a target that already
    /// carries the path behind a base it read for itself is left alone.
    fn reanchor_new_url_call(
        data_call: &mut DataCallResult,
        candidate: &CandidateTarget,
        file_path: &str,
    ) {
        let Some(path) = candidate.new_url_path.as_deref() else {
            return;
        };
        let target = normalize_path_params(data_call.target.trim());
        if Self::target_carries_url(&target, path) {
            if target != data_call.target {
                data_call.target = target;
            }
            return;
        }

        warn!(
            "[FileOrchestrator] Re-anchored data call target '{}' to the path its URL constructor states, '{}' ({}:{})",
            data_call.target, path, file_path, candidate.line_number
        );
        data_call.target = path.to_string();
    }

    /// Does `target` already state `url` — either as the whole target, or as
    /// its tail behind a base (`${API_URL}/v1/things`)? The tail must start at
    /// a segment boundary, so `/things` does not "carry" `/other-things`.
    /// Both arguments are expected in the router param spelling.
    fn target_carries_url(target: &str, url: &str) -> bool {
        match target.trim().strip_suffix(url) {
            Some(prefix) => prefix.is_empty() || !prefix.ends_with('/'),
            None => false,
        }
    }

    /// Emit the outbound calls a verb-named request spec states outright, for
    /// the call sites the file analyzer returned nothing for (#529).
    ///
    /// `client.post({ url: "/v1/sessions/{sessionId}/release" })` is how a
    /// generated OpenAPI client issues an operation, and the method and path
    /// are both AST facts there — the same standing as a route descriptor's
    /// `{ method, path }`, which is merged deterministically by #234. When
    /// extraction misses such a file (a generated client is hundreds of
    /// near-identical wrappers, and the analyzer routinely returns none of
    /// them), the consumer side of every one of those operations is lost and
    /// the producer's endpoints are reported orphaned — the index asserting no
    /// consumer where one exists.
    ///
    /// Only `method_from_callee` specs qualify; see [`RequestSpec`] for why
    /// the `{ method, url }` object form stays the analyzer's to classify.
    /// A spec whose site the analyzer DID answer is left alone, and so is one
    /// whose (method, url) another call in the file already carries — a target
    /// behind a base URL included, since the base is the one thing the
    /// analyzer knows and this backfill does not.
    fn merge_request_spec_calls(
        result: &mut FileAnalysisResult,
        candidate_map: &HashMap<String, CandidateTarget>,
    ) -> usize {
        let mut specs: Vec<&CandidateTarget> = candidate_map
            .values()
            .filter(|candidate| {
                candidate
                    .request_spec
                    .as_ref()
                    .is_some_and(|spec| spec.method_from_callee)
            })
            .collect();
        // The map iterates in hash order; emit in source order so a scan of the
        // same file always produces the same rows.
        specs.sort_by_key(|candidate| candidate.span_start);

        let mut added = 0;
        for candidate in specs {
            let Some(spec) = candidate.request_spec.as_ref() else {
                continue;
            };
            let covered = result.data_calls.iter().any(|data_call| {
                data_call.candidate_id == candidate.candidate_id
                    || (data_call
                        .method
                        .as_deref()
                        .map(|method| method.trim().to_uppercase())
                        .as_deref()
                        == Some(spec.method.as_str())
                        && Self::target_carries_url(
                            &normalize_path_params(&data_call.target),
                            &spec.url,
                        ))
            });
            if covered {
                continue;
            }
            debug!(
                "Backfilling outbound call the extraction missed: {} {} at line {}",
                spec.method, spec.url, candidate.line_number
            );
            result.data_calls.push(DataCallResult {
                candidate_id: candidate.candidate_id.clone(),
                line_number: i32::try_from(candidate.line_number).unwrap_or(i32::MAX),
                target: spec.url.clone(),
                method: Some(spec.method.clone()),
                // Classification is judgment, not an AST fact: the URL is a
                // bare path with no host to classify from.
                call_kind: None,
                pattern_matched: if candidate.callee_object.starts_with('<') {
                    // The receiver is an expression, not a name (the
                    // `(options?.client ?? client)` shape).
                    "http-client".to_string()
                } else {
                    candidate.callee_object.clone()
                },
                // The span is what the type sidecar anchors on, and it is also
                // what marks this call candidate-backed downstream.
                call_expression_span_start: Some(candidate.span_start),
                call_expression_span_end: Some(candidate.span_end),
                call_expression_text: None,
                call_expression_line: Some(
                    i32::try_from(candidate.line_number).unwrap_or(i32::MAX),
                ),
                payload_expression_text: None,
                payload_expression_line: None,
                primary_type_symbol: None,
                type_import_source: None,

                loopback_default_url: None,
                base: None,
                consumers_not_resolved: None,
            });
            added += 1;
        }
        added
    }

    /// Emit the outbound calls that reach their endpoint through a request
    /// wrapper declared in the SAME file, with the path passed in as an
    /// argument (carrick#588).
    ///
    /// `requestJson(base, "/api/v1/widgets", token)` delegating to a local
    /// `requestJson(base, path, token)` that does the `fetch` is one HTTP call
    /// to `/api/v1/widgets`, and every part of that is an AST fact. Neither
    /// half is extractable alone: the wrapper's own request interpolates a
    /// parameter, so its URL resolves to nothing, and the site raises no
    /// candidate at all (its callee is a local identifier and its path is not
    /// the first argument), so the analyzer is never asked about it. The
    /// endpoints reached this way are therefore absent from the index — not
    /// wrong rows, no rows — which is what makes this a deterministic emission
    /// rather than a prompt problem. #369/#370 resolve the same indirection
    /// across files, where the wrapper's source has to be injected before the
    /// model can join it; same-file needs no injection and no judgment.
    ///
    /// A site extraction DID answer is left alone: same span, same line, or a
    /// call already carrying the same path with a compatible method.
    fn merge_local_wrapper_calls(
        result: &mut FileAnalysisResult,
        wrapper_calls: Vec<LocalWrapperCall>,
    ) -> usize {
        // Only rows extraction produced can cover a site. Reading the vector
        // as it grows would let the first backfill of a line suppress its
        // siblings — two wrapper sites on one line (a `Promise.all` of them,
        // say) are two calls.
        let extracted = result.data_calls.len();
        let mut added = 0;
        for call in wrapper_calls {
            let line = i32::try_from(call.line_number).unwrap_or(i32::MAX);
            let ours = normalize_path_params(&call.target);
            let covered = result.data_calls[..extracted].iter().any(|data_call| {
                if data_call.call_expression_span_start == Some(call.span_start)
                    || data_call.line_number == line
                    || data_call.call_expression_line == Some(line)
                {
                    return true;
                }
                let method_agrees = match (data_call.method.as_deref(), call.method.as_deref()) {
                    (Some(theirs), Some(mine)) => theirs.trim().eq_ignore_ascii_case(mine),
                    // An unstated method on either side cannot separate them.
                    _ => true,
                };
                let theirs = normalize_path_params(&data_call.target);
                // An empty target states no path, so it carries nothing: the
                // suffix test would otherwise read it as covering everything.
                !theirs.trim().is_empty()
                    && method_agrees
                    && (Self::target_carries_url(&theirs, &ours)
                        || Self::target_carries_url(&ours, &theirs))
            });
            if covered {
                continue;
            }
            debug!(
                "Backfilling same-file wrapper call the extraction was never offered: {} {} at line {}",
                call.method.as_deref().unwrap_or("<unstated>"),
                call.target,
                call.line_number
            );
            result.data_calls.push(DataCallResult {
                candidate_id: format!("local-wrapper:{}-{}", call.span_start, call.span_end),
                line_number: line,
                target: call.target,
                method: call.method,
                // Classification is judgment, not an AST fact: the target is
                // the wrapper's own URL expression, with no host to classify
                // from beyond what the wrapper closes over.
                call_kind: None,
                pattern_matched: call.wrapper_name,
                // The span is the SITE's, which is what the type sidecar
                // anchors on and what marks this call candidate-backed
                // downstream.
                call_expression_span_start: Some(call.span_start),
                call_expression_span_end: Some(call.span_end),
                call_expression_text: None,
                call_expression_line: Some(line),
                payload_expression_text: None,
                payload_expression_line: None,
                primary_type_symbol: None,
                type_import_source: None,

                loopback_default_url: None,
                base: None,
                consumers_not_resolved: None,
            });
            added += 1;
        }
        added
    }

    /// Extract the route path from a candidate's first-arg source snippet when
    /// it is a plain single- or double-quoted string literal. Template
    /// literals, arrays, escaped strings, and truncated snippets return None:
    /// only an unambiguous literal may override the LLM-emitted path.
    fn route_literal_from_snippet(snippet: Option<&str>) -> Option<String> {
        let s = snippet?.trim();
        let quote = s.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        if s.len() < 2 || !s.ends_with(quote) {
            return None;
        }
        let inner = &s[1..s.len() - 1];
        if inner.contains(quote) || inner.contains('\\') {
            return None;
        }
        let inner = inner.trim();
        if inner.is_empty() {
            return None;
        }
        Some(inner.to_string())
    }

    /// Rewrite data-call targets that interpolate an env-var base URL aliased
    /// through a local const so the real `process.env` name reaches downstream
    /// classification and cross-repo matching (#218).
    ///
    /// The file analyzer emits the target verbatim — e.g.
    /// `${ORDERS_BASE}/orders/${id}` for
    /// `const ORDERS_BASE = process.env.ORDERS_SERVICE_URL ?? "...";`. Without
    /// this pass, `Config::is_internal_call` and the cross-repo matcher key on
    /// the local const `ORDERS_BASE` rather than `ORDERS_SERVICE_URL`, so the
    /// edge never forms. Rewriting the leading `${ALIAS}` to
    /// `${process.env.NAME}` funnels the call back through the existing
    /// direct-`process.env` handling instead of duplicating env-var parsing.
    /// Fold env aliases exported by same-repo modules `importer` imports into
    /// its alias map (#218 cross-file scope): resolve each RELATIVE import
    /// specifier to a file, parse it (memoized in `module_exports_cache` by
    /// canonical path), project its aliases onto its export names, and merge
    /// them under the importer's local names. One hop only — a config module
    /// that itself re-exports another module's aliases is a documented
    /// limitation, not followed. Package and tsconfig-alias specifiers resolve
    /// to nothing, exactly as in the #369 wrapper pass.
    ///
    /// Pub because the corpus-3 regression test drives this exact seam over
    /// the committed fixture files.
    pub fn merge_cross_file_env_aliases(
        env_alias_map: &mut EnvAliasMap,
        importer: &Path,
        imported_symbols: &HashMap<String, ImportedSymbol>,
        module_exports_cache: &mut HashMap<PathBuf, EnvAliasMap>,
        cm: &Lrc<SourceMap>,
        handler: &Handler,
    ) {
        merge_imported_env_aliases(env_alias_map, imported_symbols, |spec| {
            let resolved = Self::resolve_relative_import(importer, spec)?;
            Some(
                module_exports_cache
                    .entry(resolved.clone())
                    .or_insert_with(|| {
                        parse_file(&resolved, cm, handler)
                            .map(|module| exported_env_aliases(&module))
                            .unwrap_or_default()
                    })
                    .clone(),
            )
        });
    }

    /// Collapse inline env-var fallbacks in data-call targets (carrick#399):
    /// `${A ?? <expr>}` / `${A || <expr>}` -> `${A}`. The model intermittently
    /// (~1/3 of scans, guidance-correlated) copies the template literal
    /// verbatim including its fallback; the whitespace inside the
    /// interpolation then fails `is_valid_route_shape` and the call is
    /// silently dropped — the residual #359 recall flake. The env-var NAME is
    /// the classification signal (#294); the fallback expression is noise.
    /// Pure string normalization, no re-parse; see
    /// [`crate::analyzer::normalize_env_fallback_target`] for the exact rules
    /// (non-fallback expressions stay verbatim and stay rejected).
    fn normalize_fallback_targets(result: &mut FileAnalysisResult) {
        for data_call in &mut result.data_calls {
            if let Some(normalized) =
                crate::analyzer::normalize_env_fallback_target(&data_call.target)
            {
                debug!(
                    "Collapsed inline fallback in data-call target: {:?} -> {:?}",
                    data_call.target, normalized
                );
                data_call.target = normalized;
            }
        }
    }

    /// Carry an imported wrapper's fixed request shape onto the call sites that
    /// delegate to it (carrick-cloud#386).
    ///
    /// Cross-file wrapper-site resolution (#369/#370) resolves the TARGET of a
    /// delegating call because the wrapper's source is in the prompt. The METHOD
    /// is not resolvable that way when the wrapper hardcodes it: the delegating
    /// site's own arguments carry no method, so extraction emits none and
    /// `normalize_consumer_method` falls back to `GET`. A POST-only client then
    /// appears in the index as a set of GETs.
    ///
    /// A delegating site is identified exactly as the wrapper-echo suppression
    /// in `build_mount_graph` identifies it: no `call_expression_span_start`,
    /// i.e. the deterministic scanner raised no HTTP candidate there, because
    /// `this.client.load()` is not a client call it recognizes. Using the same
    /// discriminator is deliberate — a record this pass rewrites is exactly a
    /// record that pass may collapse, so the two can never disagree about which
    /// row is the wrapper's echo.
    ///
    /// A wrapper that sends no body at all takes the site's payload anchor with
    /// it: a non-GET method turns on request-body inference downstream
    /// (`should_infer_request_body`), and a request with no body must not
    /// acquire a request type from whatever expression extraction pointed at.
    /// Only a definite `Some(false)` does this; an unreadable argument list
    /// leaves the anchor alone.
    /// Join a file's candidate call sites onto the request members of the
    /// same-repo modules it imports (carrick#588).
    ///
    /// A site like `client.createUpload(name)` states neither a path nor a
    /// method. Its own file states neither either, so extraction has only the
    /// file's other text to read them off, and what it reads off is whatever
    /// happens to look like a path — a name, a comment, a literal inside an
    /// error message. The member it calls states both, and
    /// `crate::imported_request_member` has already read them.
    ///
    /// The join is by name: the candidate's callee property
    /// (`client.createArtifactUrl`), or its callee object when the call is a
    /// bare identifier. A name no imported module declares, or one two of them
    /// declare differently, resolves to nothing.
    ///
    /// A name alone would be too wide — `list`, `get` and `create` are what
    /// every client calls its methods — so the receiver constrains it.
    /// `import_owners` maps each of the file's imported local names to the
    /// same-repo module it resolves to, or to `None` for a package import.
    /// Where the call's receiver is one of those names it must have come from
    /// the very module the member did. A receiver that is a parameter or a
    /// local carries no such constraint, which is the shape this pass exists
    /// for; a receiver imported from a package matches no module and joins to
    /// nothing.
    /// Join each candidate's callee name onto a request member, nearest ring
    /// first.
    ///
    /// `rings` are the module sets in order of distance from the consumer:
    /// the modules it imports (through barrels), then the modules those
    /// import. The nearest ring that declares the name decides: a name a
    /// nearer ring declares ambiguously is dropped there, never looked for
    /// further out, and a name a nearer ring declares once is taken from it
    /// even when a further ring declares it too. A receiver that is itself an
    /// imported binding must have been imported from the member's own module,
    /// whichever ring that module is in; a receiver that is a parameter, a
    /// local or a `this` chain carries no such constraint (see the module docs
    /// of [`crate::imported_request_member`]).
    ///
    /// Returns the sites it DECLINED as well (carrick#656), so a row can state
    /// what the join could not follow instead of reading as complete. A
    /// decline is counted only where the receiver's own module imports the
    /// member's: that is the shape the join gives up on by design — a client
    /// constructed in one module and exported as an instance, where the name
    /// alone cannot say the binding is the client. A receiver imported from a
    /// module that never imports the client's is a different function that
    /// happens to share a name, and counting it would send a reader after a
    /// call site that does not exist. `receiver_imports` supplies that
    /// relation: each module the file imports, and the same-repo modules IT
    /// imports.
    fn resolve_imported_members(
        candidate_map: &HashMap<String, CandidateTarget>,
        rings: Vec<Vec<(PathBuf, RequestMemberIndex)>>,
        import_owners: &HashMap<String, Option<PathBuf>>,
        receiver_imports: &HashMap<PathBuf, BTreeSet<PathBuf>>,
    ) -> (HashMap<u32, ResolvedMember>, Vec<(u32, String)>) {
        let rings: Vec<_> = rings
            .into_iter()
            .map(fold_indexes_with_conflicts)
            .filter(|(members, conflicting)| !members.is_empty() || !conflicting.is_empty())
            .collect();
        if rings.is_empty() {
            return (HashMap::new(), Vec::new());
        }
        let mut resolved = HashMap::new();
        let mut declined: Vec<(u32, String)> = Vec::new();
        for candidate in candidate_map.values() {
            let name = Self::member_call_name(candidate);
            let mut owned = None;
            for (members, conflicting) in &rings {
                if conflicting.contains(name) {
                    break;
                }
                if let Some(member) = members.get(name) {
                    owned = Some(member);
                    break;
                }
            }
            let Some(owned) = owned else {
                continue;
            };
            if let Some(receiver_source) = import_owners.get(&candidate.callee_object)
                && receiver_source.as_ref() != Some(&owned.module)
            {
                if receiver_source.as_ref().is_some_and(|source| {
                    receiver_imports
                        .get(source)
                        .is_some_and(|imports| imports.contains(&owned.module))
                }) {
                    declined.push((candidate.span_start, name.to_string()));
                }
                continue;
            }
            resolved.insert(
                candidate.span_start,
                ResolvedMember {
                    name: name.to_string(),
                    member: owned.member.clone(),
                },
            );
        }
        // `candidate_map` iterates in hash order and a count computed from
        // this is persisted.
        declined.sort();
        (resolved, declined)
    }

    /// The member name a candidate call site names: its callee property
    /// (`client.createArtifactUrl`), or its callee object when the call is a
    /// bare identifier. The same name the join keys on, written once so the
    /// join and the count of what the join missed can never read a site's name
    /// two different ways.
    fn member_call_name(candidate: &CandidateTarget) -> &str {
        candidate
            .callee_property
            .as_deref()
            .unwrap_or(&candidate.callee_object)
    }

    /// Where each request member the scan read is declared, keyed by name
    /// (carrick#656).
    ///
    /// `member_cache` holds every module a member was read for, canonical path
    /// to index; `analysed` maps those paths to the `file_results` key of the
    /// files this scan analysed. A member in a module nothing analysed has no
    /// row to carry anything and gets no home, but it still makes its name
    /// ambiguous: a name two modules declare is dropped rather than attributed
    /// to either, because a site naming it belongs to neither more than the
    /// other and a count that guessed between them would be worth less than no
    /// count at all.
    fn member_homes(
        member_cache: &HashMap<PathBuf, RequestMemberIndex>,
        analysed: &HashMap<PathBuf, String>,
    ) -> HashMap<String, MemberHome> {
        let mut homes: HashMap<String, MemberHome> = HashMap::new();
        let mut declared: HashMap<String, usize> = HashMap::new();
        for (module, index) in member_cache {
            for (name, member) in index {
                *declared.entry(name.clone()).or_insert(0) += 1;
                if let Some(path_str) = analysed.get(module) {
                    homes.insert(
                        name.clone(),
                        MemberHome {
                            path_str: path_str.clone(),
                            request_line: member.request_line,
                        },
                    );
                }
            }
        }
        homes.retain(|name, _| declared.get(name) == Some(&1));
        homes
    }

    /// Apply the resolved members to the sites that called them.
    ///
    /// This OVERWRITES the method and target extraction gave the site, which
    /// nothing else in this pass does. It is warranted because the two are not
    /// evidence of the same quality: the member's request is a literal in the
    /// source, and the site's own file contains no statement of either. The
    /// same reasoning already licenses `reanchor_data_call` to overrule a
    /// target from a request spec read off the AST.
    ///
    /// Runs immediately after `apply_candidate_map`, which is what stamps the
    /// span this joins on, and before every downstream reader of method or
    /// target.
    fn apply_imported_members(
        result: &mut FileAnalysisResult,
        resolved: &HashMap<u32, ResolvedMember>,
    ) -> usize {
        if resolved.is_empty() {
            return 0;
        }
        let mut applied = 0;
        for data_call in &mut result.data_calls {
            let Some(span) = data_call.call_expression_span_start else {
                continue;
            };
            let Some(member) = resolved.get(&span).map(|resolved| &resolved.member) else {
                continue;
            };
            let method_agrees = data_call
                .method
                .as_deref()
                .map(normalize_manifest_method)
                .is_some_and(|method| method == member.method);
            if method_agrees && data_call.target == member.target {
                continue;
            }
            debug!(
                "Resolving call site through its imported member: {} {} (was {} {})",
                member.method,
                member.target,
                data_call.method.as_deref().unwrap_or("<unstated>"),
                data_call.target,
            );
            data_call.method = Some(member.method.clone());
            data_call.target = member.target.clone();
            applied += 1;
        }
        applied
    }

    /// Emit the resolved members whose call sites extraction returned no row
    /// for at all (carrick#623).
    ///
    /// `apply_imported_members` above only REWRITES a row that already exists,
    /// so a resolved member is silently dropped whenever the analyzer answered
    /// nothing for its site. That is the common case, not the rare one: a bare
    /// `client.createUpload(name)` states no path and no verb, and a consumer
    /// file that contains no path-shaped text anywhere gives the model nothing
    /// to answer with, so it answers with nothing. The endpoint the site
    /// reaches is then absent from the index entirely, and the producer is
    /// reported orphaned.
    ///
    /// The member's method and URL are both literals in the source, and the
    /// site's span is an AST fact, so the row is asserted rather than inferred
    /// — the same standing as `merge_local_wrapper_calls`, whose coverage test
    /// and row shape this mirrors.
    fn merge_imported_member_calls(
        result: &mut FileAnalysisResult,
        resolved: &HashMap<u32, ResolvedMember>,
        candidate_map: &HashMap<String, CandidateTarget>,
    ) -> usize {
        if resolved.is_empty() {
            return 0;
        }
        // Both maps iterate in hash order; emit in source order so a scan of
        // the same file always produces the same rows.
        //
        // A request member is an HTTP request, so only an HTTP candidate can
        // be a site that reaches one. The name join is by member name with no
        // receiver constraint where the receiver is a local, which is the
        // shape this pass exists for, so a `client.publish(topic, payload)`
        // would otherwise join to an imported member called `publish`.
        // Rewriting a row could never act on that (a pub/sub site has no HTTP
        // row at its span); emitting one could.
        let mut sites: Vec<(&CandidateTarget, &RequestMember)> = candidate_map
            .values()
            .filter(|candidate| candidate.protocol == Protocol::Http)
            .filter_map(|candidate| {
                resolved
                    .get(&candidate.span_start)
                    .map(|resolved| (candidate, &resolved.member))
            })
            .collect();
        sites.sort_by_key(|(candidate, _)| candidate.span_start);

        // Only rows extraction produced can cover a site. Reading the vector
        // as it grows would let the first backfill of a line suppress its
        // siblings — two resolved sites on one line are two calls.
        let extracted = result.data_calls.len();
        let mut added = 0;
        for (candidate, member) in sites {
            // A site extraction answered as an ENDPOINT is answered. One
            // candidate map holds both, and a route definition whose verb
            // happens to name an imported request member (`app.get(...)`
            // against a client's `get`) resolves like any other bare call:
            // the receiver is a local, so no import constrains the join.
            // Rewriting could never reach it, because there is no data-call
            // row at its span to rewrite; emitting one would invent a
            // consumer for a route the file DEFINES.
            if result
                .endpoints
                .iter()
                .any(|endpoint| endpoint.call_expression_span_start == Some(candidate.span_start))
            {
                continue;
            }
            let line = i32::try_from(candidate.line_number).unwrap_or(i32::MAX);
            let ours = normalize_path_params(&member.target);
            let covered = result.data_calls[..extracted].iter().any(|data_call| {
                if data_call.call_expression_span_start == Some(candidate.span_start)
                    || data_call.line_number == line
                    || data_call.call_expression_line == Some(line)
                {
                    return true;
                }
                let method_agrees = match data_call.method.as_deref() {
                    Some(theirs) => theirs.trim().eq_ignore_ascii_case(&member.method),
                    // An unstated method cannot separate them.
                    None => true,
                };
                let theirs = normalize_path_params(&data_call.target);
                // An empty target states no path, so it carries nothing: the
                // suffix test would otherwise read it as covering everything.
                !theirs.trim().is_empty()
                    && method_agrees
                    && (Self::target_carries_url(&theirs, &ours)
                        || Self::target_carries_url(&ours, &theirs))
            });
            if covered {
                continue;
            }
            debug!(
                "Backfilling imported-member call the extraction returned no row for: {} {} at line {}",
                member.method, member.target, candidate.line_number
            );
            result.data_calls.push(DataCallResult {
                candidate_id: format!(
                    "imported-member:{}-{}",
                    candidate.span_start, candidate.span_end
                ),
                line_number: line,
                target: member.target.clone(),
                method: Some(member.method.clone()),
                // Classification is judgment, not an AST fact: the target is
                // the member's own URL expression, with no host to classify
                // from beyond what the member closes over.
                call_kind: None,
                pattern_matched: candidate
                    .callee_property
                    .clone()
                    .unwrap_or_else(|| candidate.callee_object.clone()),
                // The span is what the type sidecar anchors on, and it is also
                // what marks this call candidate-backed downstream.
                call_expression_span_start: Some(candidate.span_start),
                call_expression_span_end: Some(candidate.span_end),
                call_expression_text: None,
                call_expression_line: Some(line),
                payload_expression_text: None,
                payload_expression_line: None,
                primary_type_symbol: None,
                type_import_source: None,

                loopback_default_url: None,
                base: None,
                consumers_not_resolved: None,
            });
            added += 1;
        }
        added
    }

    /// Stamp what the member join could not follow onto the rows that member
    /// DID produce (carrick#656).
    ///
    /// A consumer listing reads as complete whatever the join declined, and
    /// the shapes it declines are declined by design — a client imported as an
    /// already-constructed instance, a receiver whose only statement of type is
    /// a parameter annotation. `unresolved_member_sites` counted those sites;
    /// this is where the number reaches a persisted row, because only here is
    /// every file's contribution in.
    ///
    /// Two kinds of row carry it, and both are rows the same member is behind:
    ///
    /// 1. Every row at a site the join resolved to that member. These are the
    ///    consumer rows an operation lists, so the count travels with the list
    ///    it qualifies.
    /// 2. The member's own request row, inside the client module, found by the
    ///    line its request is written on. It is the row an operation lists when
    ///    NO site resolved, which is exactly the state the count describes;
    ///    without it a member the join followed nowhere would say nothing at
    ///    all. It is stamped only when a row exists there: a client whose
    ///    request goes through a helper raises no candidate, so whether
    ///    extraction answered for that line is not something this pass decides.
    ///
    /// Rewrites nothing else, and never writes a zero: the field is absent
    /// unless a site was really counted.
    fn stamp_unfollowed_member_sites(
        file_results: &mut HashMap<String, FileAnalysisResult>,
        deficits: &HashMap<String, u32>,
        resolved_rows: &HashMap<String, HashMap<u32, String>>,
        homes: &HashMap<String, MemberHome>,
    ) -> usize {
        if deficits.is_empty() {
            return 0;
        }
        let mut stamped = 0;
        // 1. The rows the join produced for the member, wherever they are.
        for (path, spans) in resolved_rows {
            let Some(result) = file_results.get_mut(path) else {
                continue;
            };
            for data_call in &mut result.data_calls {
                let Some(span) = data_call.call_expression_span_start else {
                    continue;
                };
                let Some(member) = spans.get(&span) else {
                    continue;
                };
                let Some(count) = deficits.get(member).copied().filter(|count| *count > 0) else {
                    continue;
                };
                data_call.consumers_not_resolved = Some(UnfollowedMemberSites {
                    member: member.clone(),
                    count,
                });
                stamped += 1;
            }
        }
        // 2. The member's own request row in its module. Sorted so a file
        //    holding two such members is stamped in one order, not in whatever
        //    order the map iterates.
        let mut counted: Vec<(&String, &u32)> =
            deficits.iter().filter(|(_, count)| **count > 0).collect();
        counted.sort();
        for (member, count) in counted {
            let Some(home) = homes.get(member) else {
                continue;
            };
            let Some(result) = file_results.get_mut(&home.path_str) else {
                continue;
            };
            let Ok(line) = i32::try_from(home.request_line) else {
                continue;
            };
            for data_call in &mut result.data_calls {
                if data_call.consumers_not_resolved.is_some() {
                    continue;
                }
                if data_call.line_number != line && data_call.call_expression_line != Some(line) {
                    continue;
                }
                data_call.consumers_not_resolved = Some(UnfollowedMemberSites {
                    member: member.clone(),
                    count: *count,
                });
                stamped += 1;
            }
        }
        stamped
    }

    /// Emit the whole-URL environment-variable calls whose sites extraction
    /// returned no row for at all (carrick#632).
    ///
    /// `resolve_env_var_aliases` above only REWRITES a row that already
    /// exists, so the resolution #604 added is lost whenever the analyzer
    /// answered nothing for the site. That happens wherever the request is
    /// buried — an arrow function that is a property of an object literal
    /// handed to a factory call, say — because the site states no path and the
    /// file may state none anywhere outside the fallback literal, so there is
    /// nothing for the model to answer with. The call is then absent from the
    /// index entirely: not a matched edge, not an unmatched call, not an
    /// egress candidate.
    ///
    /// Every part of the row is a literal in the source. The binding's env var
    /// and the path inside its `??` fallback are what
    /// [`resolve_whole_url_target`] reads, the method is the literal in the
    /// call's own options bag, and the span is an AST fact — the same standing
    /// as `merge_local_wrapper_calls` and `merge_imported_member_calls`, whose
    /// coverage test and row shape this mirrors.
    ///
    /// Two narrowings keep it to what the source states outright:
    ///
    /// - Only a call the AST reads as a REQUEST with a literal method
    ///   qualifies. A bare binding is passed to plenty of things that are not
    ///   requests (`new URL(url)`, a logger), and joining on the binding name
    ///   alone would invent a call at every one of them.
    /// - A request whose method cannot be read as a literal
    ///   (`fetch(url, { headers })`, or a parameterized `{ method }`) is left
    ///   alone rather than emitted with a guessed verb. That shape stays
    ///   extraction's to answer.
    ///
    /// The row extraction produced for the site is CORRECTED rather than left
    /// alone. Emitting only where extraction was silent is what #633 shipped,
    /// and it left the live case unfixed: the model does answer this shape
    /// often enough, paraphrasing the binding as `${SUPPORT_URL}/api/ask`, and
    /// that spelling is not the one the rest of the pipeline resolves env vars
    /// through, nor does it carry the fallback the canonical key needs. Same
    /// span is identity — `apply_candidate_map` stamps the candidate's own
    /// span on the row answered for it — so the row is this call and every
    /// field the AST states outright belongs on it. A row on the same line
    /// with a DIFFERENT span is a different call and still covers the site.
    fn merge_whole_url_env_calls(
        result: &mut FileAnalysisResult,
        candidate_map: &HashMap<String, CandidateTarget>,
        aliases: &EnvAliasMap,
        paths: &WholeUrlFallbackMap,
    ) -> (usize, usize) {
        if paths.is_empty() {
            return (0, 0);
        }
        // The map iterates in hash order; emit in source order so a scan of
        // the same file always produces the same rows.
        let mut sites: Vec<(&CandidateTarget, String, String, Option<String>)> = candidate_map
            .values()
            .filter(|candidate| candidate.protocol == Protocol::Http)
            .filter_map(|candidate| {
                // The method has to be the call's own literal: see above.
                let RequestShapeSignal::Known(shape) = &candidate.request_shape else {
                    return None;
                };
                let snippet = candidate.path_snippet.as_deref()?;
                let target = resolve_whole_url_target(snippet, aliases, paths)?;
                let local_default = whole_url_local_default(snippet, aliases, paths);
                Some((candidate, shape.method.clone(), target, local_default))
            })
            .collect();
        sites.sort_by_key(|(candidate, _, _, _)| candidate.span_start);

        // Only rows extraction produced can cover a site. Reading the vector
        // as it grows would let the first backfill of a line suppress its
        // siblings — two resolved sites on one line are two calls.
        let extracted = result.data_calls.len();
        let mut added = 0;
        let mut corrected = 0;
        for (candidate, method, target, local_default) in sites {
            // A site extraction answered as an ENDPOINT is answered. One
            // candidate map holds route registrations as well as calls, and a
            // route mounted at a whole URL read from an environment variable
            // is a route the file DEFINES, not a call it makes.
            if result
                .endpoints
                .iter()
                .any(|endpoint| endpoint.call_expression_span_start == Some(candidate.span_start))
            {
                continue;
            }
            let line = i32::try_from(candidate.line_number).unwrap_or(i32::MAX);
            let ours = normalize_path_params(&target);
            // The row extraction answered for THIS call, if it answered one.
            // Either the candidate's own span, or a row on the line that is
            // anchored to no span at all — a second request on the line would
            // have raised its own candidate and carried its own span, so an
            // unanchored row on it is this one.
            let mine = result.data_calls[..extracted].iter().position(|data_call| {
                data_call.call_expression_span_start == Some(candidate.span_start)
                    || (data_call.call_expression_span_start.is_none()
                        && (data_call.line_number == line
                            || data_call.call_expression_line == Some(line)))
            });
            if let Some(index) = mine {
                corrected += Self::correct_whole_url_row(
                    &mut result.data_calls[index],
                    candidate,
                    &method,
                    target,
                    local_default,
                );
                continue;
            }
            let covered = result.data_calls[..extracted].iter().any(|data_call| {
                if data_call.line_number == line || data_call.call_expression_line == Some(line) {
                    return true;
                }
                let method_agrees = match data_call.method.as_deref() {
                    Some(theirs) => theirs.trim().eq_ignore_ascii_case(&method),
                    // An unstated method cannot separate them.
                    None => true,
                };
                let theirs = normalize_path_params(&data_call.target);
                // An empty target states no path, so it carries nothing: the
                // suffix test would otherwise read it as covering everything.
                !theirs.trim().is_empty()
                    && method_agrees
                    && (Self::target_carries_url(&theirs, &ours)
                        || Self::target_carries_url(&ours, &theirs))
            });
            if covered {
                continue;
            }
            debug!(
                "Backfilling whole-URL env-var call the extraction returned no row for: {} {} at line {}",
                method, target, candidate.line_number
            );
            result.data_calls.push(DataCallResult {
                candidate_id: format!(
                    "whole-url-env:{}-{}",
                    candidate.span_start, candidate.span_end
                ),
                line_number: line,
                target,
                method: Some(method),
                // Classification is judgment, not an AST fact: the origin is
                // whatever the environment supplies, and the fallback's host
                // says nothing about the deployed one.
                call_kind: None,
                pattern_matched: candidate
                    .callee_property
                    .clone()
                    .unwrap_or_else(|| candidate.callee_object.clone()),
                // The span is what the type sidecar anchors on, and it is also
                // what marks this call candidate-backed downstream.
                call_expression_span_start: Some(candidate.span_start),
                call_expression_span_end: Some(candidate.span_end),
                call_expression_text: None,
                call_expression_line: Some(line),
                payload_expression_text: None,
                payload_expression_line: None,
                primary_type_symbol: None,
                type_import_source: None,
                loopback_default_url: local_default,
                base: None,
                consumers_not_resolved: None,
            });
            added += 1;
        }
        (added, corrected)
    }

    /// The path a data call is KEYED on — the one canonicalization every reader
    /// of the key shares (the mount-graph loop, the type-request join, the
    /// uploaded projection).
    ///
    /// Normally that is `consumer_call_path` over the target the row states.
    /// A whole-URL env-var call is keyed on the LOOPBACK default the source
    /// states for it instead (`loopback_default_url`), because the target's
    /// origin is an env var and an undeclared env-var base is kept verbatim,
    /// which would leave `${process.env.SUPPORT_URL}` standing in the key as a
    /// leading path segment. The call would then be absent from the operation
    /// it requests: `fetch("http://localhost:3939/api/ask")` keys `/api/ask`
    /// and putting an env-var override in front of the same literal would drop
    /// it out of the index. The target itself is untouched, so the row still
    /// displays the deployed origin and still reads as an env-var call for the
    /// `internalEnvVars` suggestion.
    fn canonical_call_path(normalizer: &UrlNormalizer, data_call: &DataCallResult) -> String {
        let url = data_call
            .loopback_default_url
            .as_deref()
            .unwrap_or(&data_call.target);
        normalizer.consumer_call_path(url)
    }

    /// Put on the row extraction answered for a whole-URL env-var call what the
    /// binding's own AST states: the resolved target, the method the call's
    /// options bag spells out, and the loopback default the canonical key is
    /// computed from. Returns 1 when anything changed.
    ///
    /// The target is overwritten rather than merged. This rule fires only where
    /// the call site states no path of its own, so every path in an extracted
    /// row for it came from reading the same fallback literal — deterministically
    /// here, by paraphrase there.
    fn correct_whole_url_row(
        data_call: &mut DataCallResult,
        candidate: &CandidateTarget,
        method: &str,
        target: String,
        local_default: Option<String>,
    ) -> usize {
        let mut changed = false;
        if data_call.target != target {
            debug!(
                "Correcting whole-URL env-var call target at line {}: {} -> {}",
                candidate.line_number, data_call.target, target
            );
            data_call.target = target;
            changed = true;
        }
        if data_call.loopback_default_url != local_default {
            data_call.loopback_default_url = local_default;
            changed = true;
        }
        // Only where extraction stated none: a stated verb is the model reading
        // the same options bag, and disagreeing with it is a separate question
        // from resolving the URL.
        if data_call.method.as_deref().is_none_or(str::is_empty) {
            data_call.method = Some(method.to_string());
            changed = true;
        }
        if data_call.call_expression_span_start.is_none() {
            data_call.call_expression_span_start = Some(candidate.span_start);
            data_call.call_expression_span_end = Some(candidate.span_end);
            changed = true;
        }
        usize::from(changed)
    }

    fn propagate_wrapper_request_shape(
        result: &mut FileAnalysisResult,
        shape: Option<&WrapperRequestShape>,
    ) -> usize {
        let Some(shape) = shape else {
            return 0;
        };
        let mut propagated = 0;
        for data_call in &mut result.data_calls {
            // Extracted at its own client call site, not through the wrapper —
            // its method is the one the scanner saw.
            if data_call.call_expression_span_start.is_some() {
                continue;
            }
            let existing = data_call
                .method
                .as_deref()
                .map(normalize_manifest_method)
                .unwrap_or_default();
            if existing != shape.method {
                data_call.method = Some(shape.method.clone());
                propagated += 1;
            }
            if shape.has_body == Some(false) {
                data_call.payload_expression_text = None;
                data_call.payload_expression_line = None;
            }
        }
        propagated
    }

    /// Record how each call's base resolves (carrick#649).
    ///
    /// Reads the target the row will persist and states what the AST says
    /// about its base slot: the expression as written, whether it reads the
    /// environment, the literal default beside it, and what a validation
    /// schema in the scanned files declares about the variable. Writes only
    /// `base` — the target, the method and the canonical key are untouched, so
    /// this cannot move a call into or out of an operation.
    ///
    /// A row already carrying a base is left alone: nothing upstream sets one
    /// today, and if something starts to, the pass that read the call site is
    /// the better witness than this re-reading of its target.
    fn stamp_call_bases(
        result: &mut FileAnalysisResult,
        aliases: &EnvAliasMap,
        env_fallbacks: &EnvFallbackMap,
        env_schema: &EnvSchemaIndex,
    ) {
        for data_call in &mut result.data_calls {
            if data_call.base.is_some() {
                continue;
            }
            data_call.base =
                resolve_call_base(&data_call.target, aliases, env_fallbacks, env_schema);
        }
    }

    /// Resolve the binding a call target names in its base slot: the
    /// `process.env` variable behind it (#218), the whole URL it holds
    /// (carrick#572), or the absolute URL literal it was declared with
    /// (carrick#627).
    fn resolve_target_bases(
        result: &mut FileAnalysisResult,
        env_alias_map: &EnvAliasMap,
        whole_url_fallbacks: &WholeUrlFallbackMap,
        literal_bases: &LiteralBaseMap,
    ) {
        if env_alias_map.is_empty() && whole_url_fallbacks.is_empty() && literal_bases.is_empty() {
            return;
        }
        for data_call in &mut result.data_calls {
            // The whole-URL rule first (carrick#572): it fires only on a target
            // that states nothing but the binding, which the leading-`${}`
            // rewrite would leave without a path and every downstream gate
            // would then drop.
            if let Some(resolved) =
                resolve_whole_url_target(&data_call.target, env_alias_map, whole_url_fallbacks)
            {
                data_call.target = resolved;
                continue;
            }
            if let Some(resolved) = resolve_target_env_alias(&data_call.target, env_alias_map) {
                data_call.target = resolved;
                continue;
            }
            // A base the file declares as a literal (carrick#627). Last,
            // because the two rules above read a binding backed by the
            // environment and this one reads a binding backed by nothing but
            // its own initializer; a name cannot be both.
            if let Some(resolved) = resolve_target_literal_base(&data_call.target, literal_bases) {
                data_call.target = resolved;
            }
        }
    }

    /// Resolve types using the TypeSidecar.
    ///
    /// This method collects type requests from the analysis results and sends them
    /// to the sidecar for bundling (explicit) and inference (implicit).
    ///
    /// # Arguments
    /// * `sidecar` - The TypeSidecar instance for type resolution
    /// * `file_results` - Analysis results keyed by file path
    /// * `repo_path` - Path to the repository root (used to convert relative paths to absolute)
    /// * `extraction_config` - Agent-generated machinery-unwrap rules
    /// * `mount_graph` - Resolved mount graph for canonical method/path aliases
    /// * `config` - Config used for URL normalization
    /// * `extra_explicit` - Deterministically-collected explicit symbol
    ///   requests for non-HTTP protocols (socket payload anchors, #245)
    /// * `extra_infer` - Deterministically-collected `FunctionReturn` infer
    ///   requests for non-HTTP protocols (GraphQL producer resolver returns,
    ///   Stage B1). Unlike `extra_explicit`, these go through the infer path so
    ///   the resolver return is expanded, not bundled as the generic SDL anchor.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_types_with_sidecar(
        &self,
        sidecar: &TypeSidecar,
        file_results: &HashMap<String, FileAnalysisResult>,
        repo_path: &str,
        extraction_config: Option<&ExtractionConfig>,
        mount_graph: &MountGraph,
        config: &Config,
        extra_explicit: &[SymbolRequest],
        extra_infer: &[InferRequestItem],
    ) -> Result<TypeResolutionResult, Box<dyn std::error::Error>> {
        let (mut explicit, mut infer, inline_aliases) =
            self.collect_type_requests(file_results, repo_path, mount_graph, config);

        // Deterministically-collected explicit requests for non-HTTP protocols
        // (today: Socket.IO payload anchors, #245). They use the same
        // `SymbolRequest` shape and bundle path as the HTTP explicit case; the
        // alias each carries matches its manifest entry so the enrich-join lands.
        explicit.extend_from_slice(extra_explicit);

        // Deterministically-collected infer requests for non-HTTP protocols
        // (today: GraphQL producer resolver returns, Stage B1). A producer's real
        // response contract is the resolver's RETURN type expanded, so it takes
        // the `FunctionReturn` infer path (mirroring file-based routes), NOT the
        // bundle path — bundling the SDL anchor would emit the generic wrapper.
        // The alias each carries matches its manifest entry so the join lands.
        infer.extend_from_slice(extra_infer);

        debug!(
            "[FileOrchestrator] Resolving types: {} explicit ({} from non-HTTP protocols), {} inferred ({} from non-HTTP protocols)",
            explicit.len(),
            extra_explicit.len(),
            infer.len(),
            extra_infer.len()
        );

        let result = sidecar
            .resolve_all_types(&explicit, &infer, extraction_config)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

        let result = self.append_inline_aliases(result, inline_aliases);

        // Log results
        debug!(
            "[FileOrchestrator] Type resolution complete: {} manifest entries, {} inferred types, {} failures",
            result.explicit_manifest.len(),
            result.inferred_types.len(),
            result.symbol_failures.len()
        );

        // Cap warn-level error emissions the same way per-symbol failures are
        // capped below: a TS-loose codebase can produce dozens of multi-line
        // type-resolution errors that would otherwise dominate the 5 MB log
        // tail and evict the genuinely-novel diagnostics. Spillover stays at
        // debug — visible with --verbose / in the file log.
        const ERROR_WARN_CAP: usize = 10;
        let total_errors = result.errors.len();
        if total_errors > 0 {
            let cap = ERROR_WARN_CAP.min(total_errors);
            warn!(
                "[FileOrchestrator] Type resolution warnings: {:?}",
                &result.errors[..cap]
            );
            if total_errors > cap {
                warn!(
                    shown = cap,
                    suppressed = total_errors - cap,
                    "[FileOrchestrator] Additional type resolution warnings (run with --verbose to see all)"
                );
                debug!(
                    "[FileOrchestrator] Suppressed type resolution warnings: {:?}",
                    &result.errors[cap..]
                );
            }
        }

        // Per-symbol failures carry the actual diagnostic detail (which symbol,
        // which file, why). Cap warn-level emissions so a TS-loose codebase
        // with hundreds of unresolvable types doesn't dominate the 5 MB log
        // tail and evict the actually-novel diagnostic in a failed run.
        // Spillover stays at debug — visible with --verbose or in the file
        // log, but doesn't push noise into uploaded artifacts.
        const SYMBOL_FAILURE_WARN_CAP: usize = 20;
        let total = result.symbol_failures.len();
        let cap = SYMBOL_FAILURE_WARN_CAP.min(total);
        for failure in &result.symbol_failures[..cap] {
            warn!(
                symbol = %failure.symbol_name,
                source_file = %failure.source_file,
                reason = %failure.reason,
                "[FileOrchestrator] Symbol failed to resolve"
            );
        }
        if total > cap {
            warn!(
                shown = cap,
                suppressed = total - cap,
                "[FileOrchestrator] Additional symbol failures (run with --verbose to see all)"
            );
            for failure in &result.symbol_failures[cap..] {
                debug!(
                    symbol = %failure.symbol_name,
                    source_file = %failure.source_file,
                    reason = %failure.reason,
                    "[FileOrchestrator] Symbol failed to resolve"
                );
            }
        }

        Ok(result)
    }

    fn append_inline_aliases(
        &self,
        mut result: TypeResolutionResult,
        inline_aliases: Vec<(String, String)>,
    ) -> TypeResolutionResult {
        if inline_aliases.is_empty() {
            return result;
        }

        let mut combined = result.dts_content.take().unwrap_or_default();
        let mut seen = HashSet::new();

        for (alias, type_string) in inline_aliases {
            if !seen.insert(alias.clone()) {
                continue;
            }
            if Self::dts_defines_alias(&combined, &alias) {
                if Self::replace_unknown_alias(&mut combined, &alias, &type_string) {
                    continue;
                }
                continue;
            }
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str("export type ");
            combined.push_str(&alias);
            combined.push_str(" = ");
            combined.push_str(type_string.trim().trim_end_matches(';'));
            combined.push_str(";\n");
        }

        if !combined.is_empty() {
            result.dts_content = Some(combined);
        }

        result
    }

    /// Convert a file path to an absolute path.
    ///
    /// If the path is already absolute, returns it as-is.
    /// Otherwise, resolves it relative to the repo root and canonicalizes.
    fn to_absolute_path(file_path: &str, repo_root_absolute: &std::path::Path) -> String {
        use std::path::Path;

        let path = Path::new(file_path);
        if path.is_absolute() {
            return file_path.to_string();
        }

        // Resolve relative to current directory (which should be where cargo run was executed)
        let resolved = std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf());

        // Canonicalize to resolve .. and . components
        resolved
            .canonicalize()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| {
                // If canonicalize fails, try joining with repo root
                repo_root_absolute.join(path).to_string_lossy().to_string()
            })
    }

    /// Resolve an import path relative to a file.
    ///
    /// Converts relative import paths like "./types/user" to absolute paths.
    /// Bare specifiers (e.g. `types/user`) are also resolved against the
    /// nearest `tsconfig.json#compilerOptions.baseUrl` so TypeScript's
    /// classic non-relative resolution works — consistent with `tsc` behaviour
    /// when `baseUrl` is set. If neither relative nor baseUrl resolution
    /// finds a real file, the original specifier is returned unchanged so
    /// node_modules packages like `react` still pass through.
    pub(crate) fn resolve_import_path(current_file: &str, import_source: &str) -> String {
        use std::path::Path;

        let current_dir = Path::new(current_file).parent().unwrap_or(Path::new(""));

        if import_source.starts_with('.') {
            // Relative import — join against the file's own directory.
            let resolved = current_dir.join(import_source);
            let resolved_str = resolved.to_string_lossy().to_string();
            return Self::canonicalize_or_probe(&resolved_str).unwrap_or_else(|| {
                // Nothing matched on disk. Preserve pre-2026-05 behaviour so
                // downstream mount linking still sees a plausible path. If
                // the import already ends in a TS-family extension, return
                // the resolved path as-is (avoid `.ts.ts` double-extension);
                // otherwise append `.ts` as a default.
                if Self::has_ts_extension(&resolved_str) {
                    resolved_str
                } else {
                    let fallback = format!("{}.ts", resolved_str);
                    Path::new(&fallback)
                        .canonicalize()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or(fallback)
                }
            });
        }

        // Bare specifier — try tsconfig `paths` mappings first (in tsc they
        // take precedence over plain baseUrl lookup). This is how a workspace
        // shared-types package (`@meridian/contracts` mapped to
        // `../contracts/src/index.ts`) resolves to a real source file.
        if let Some(found) = Self::resolve_via_tsconfig_paths(current_dir, import_source) {
            return found;
        }

        // Then only attempt baseUrl resolution if a tsconfig in
        // the file's ancestry sets `compilerOptions.baseUrl` *explicitly*.
        // `tsc` only enables non-relative module resolution against baseUrl
        // when it's set; defaulting to "." here would shadow real
        // node_modules packages. Falling through returns the source
        // unchanged so package imports (`react`, `axios`) still flow through.
        if let Some((tsconfig_dir, base_url)) = Self::find_tsconfig_base_url(current_dir)
            && let Some(found) = Self::canonicalize_or_probe(
                tsconfig_dir
                    .join(&base_url)
                    .join(import_source)
                    .to_string_lossy()
                    .as_ref(),
            )
        {
            return found;
        }

        import_source.to_string()
    }

    /// Returns true if `path` ends in a TypeScript-family source extension.
    /// True when the path already carries a JS/TS module extension, so it must
    /// be probed as-is and never get a `.ts` appended. Covers the NodeNext
    /// families (`.mts`/`.cts`/`.mjs`/`.cjs`) too — `.d.mts`/`.d.cts` match via
    /// their `.mts`/`.cts` suffix — so a literal `foo.mts` import resolves
    /// exactly instead of probing a nonsensical `foo.mts.ts`.
    fn has_ts_extension(path: &str) -> bool {
        path.ends_with(".ts")
            || path.ends_with(".tsx")
            || path.ends_with(".mts")
            || path.ends_with(".cts")
            || path.ends_with(".js")
            || path.ends_with(".jsx")
            || path.ends_with(".mjs")
            || path.ends_with(".cjs")
    }

    /// TypeScript's NodeNext/ESM module resolution rewrites a relative
    /// import's output-JS extension back to the input-TS extension: an
    /// `import ... from './types.js'` written for a `types.ts` source
    /// resolves to `types.ts` (then `.tsx`/`.d.ts`), and only falls back to a
    /// real emitted `types.js` when no TS sibling exists. Return that ordered
    /// candidate list for a JS-family specifier so the TS source wins, mirroring
    /// tsc (carrick#148). `None` for specifiers without a rewritable JS
    /// extension — those keep their existing probe behaviour.
    fn ts_sibling_candidates(base: &str) -> Option<Vec<String>> {
        let (stem, ts_exts): (&str, &[&str]) = if let Some(stem) = base.strip_suffix(".js") {
            (stem, &[".ts", ".tsx", ".d.ts"])
        } else if let Some(stem) = base.strip_suffix(".jsx") {
            (stem, &[".tsx", ".ts", ".d.ts"])
        } else if let Some(stem) = base.strip_suffix(".mjs") {
            (stem, &[".mts", ".d.mts"])
        } else if let Some(stem) = base.strip_suffix(".cjs") {
            (stem, &[".cts", ".d.cts"])
        } else {
            return None;
        };
        let mut candidates: Vec<String> =
            ts_exts.iter().map(|ext| format!("{stem}{ext}")).collect();
        // The literal emitted JS is the last resort — only when no TS source
        // sits beside it (a genuine `.js`-only module).
        candidates.push(base.to_string());
        Some(candidates)
    }

    /// Probe a path on disk and return a canonicalized absolute string if
    /// it (or one of the standard `.ts/.tsx/.js/.jsx`/`index.*` candidates)
    /// exists. Returns `None` when nothing matches; callers decide on a
    /// fallback. A JS-family specifier resolves to its TS sibling first
    /// (tsc's ESM `.js`→`.ts` rewrite, carrick#148). A TS-family specifier
    /// is probed exactly — extension-swapping a `.ts` isn't TS resolver
    /// behaviour and would mask import bugs.
    fn canonicalize_or_probe(base: &str) -> Option<String> {
        use std::path::Path;

        let probe = |candidate: &str| -> Option<String> {
            Path::new(candidate).exists().then(|| {
                Path::new(candidate)
                    .canonicalize()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| candidate.to_string())
            })
        };

        // ESM/NodeNext: `./types.js` resolves to the `types.ts` source first.
        if let Some(candidates) = Self::ts_sibling_candidates(base) {
            return candidates.iter().find_map(|c| probe(c));
        }

        if Self::has_ts_extension(base) {
            return probe(base);
        }

        let candidates = [
            format!("{}.ts", base),
            format!("{}.tsx", base),
            format!("{}.js", base),
            format!("{}.jsx", base),
            format!("{}/index.ts", base),
            format!("{}/index.tsx", base),
            format!("{}/index.js", base),
            format!("{}/index.jsx", base),
        ];

        candidates.iter().find_map(|c| probe(c))
    }

    /// Walk up from `start_dir` looking for `tsconfig.json`. Return its
    /// directory and the resolved `compilerOptions.baseUrl` only if the
    /// option is *explicitly set* — matches `tsc` behaviour, which only
    /// enables baseUrl-based non-relative resolution when configured.
    /// Returns `None` for tsconfigs that omit baseUrl (or for repos with
    /// no tsconfig at all). Path aliases (`compilerOptions.paths`) and
    /// `extends` inheritance are out of scope here.
    /// Resolve a bare import specifier through `compilerOptions.paths`
    /// mappings of tsconfigs in the file's ancestry (nearest first). Mapping
    /// targets resolve against `baseUrl` when set (tsc's rule), else the
    /// tsconfig's own directory (TS 4.1+ paths-without-baseUrl). Supports the
    /// spec's single-`*` wildcard. Only a target that probes to a real file
    /// wins — a dangling mapping cannot eat a real package import. Walking
    /// past a paths-less tsconfig keeps `extends`-style layouts working; the
    /// probe gate makes that safe.
    fn resolve_via_tsconfig_paths(
        start_dir: &std::path::Path,
        import_source: &str,
    ) -> Option<String> {
        let mut dir = Some(start_dir);
        while let Some(d) = dir {
            let tsconfig = d.join("tsconfig.json");
            if tsconfig.is_file()
                && let Ok(text) = std::fs::read_to_string(&tsconfig)
                && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
                && let Some(co) = json.get("compilerOptions")
                && let Some(paths) = co.get("paths").and_then(|p| p.as_object())
            {
                let base = co.get("baseUrl").and_then(|v| v.as_str()).unwrap_or(".");
                for (pattern, targets) in paths {
                    let Some(targets) = targets.as_array() else {
                        continue;
                    };
                    // At most one `*` per pattern (the tsconfig spec); the
                    // matched substring substitutes into each target's `*`.
                    let substitution: Option<String> = match pattern.matches('*').count() {
                        0 => (pattern == import_source).then(String::new),
                        1 => {
                            let (prefix, suffix) = pattern.split_once('*').unwrap();
                            (import_source.len() >= prefix.len() + suffix.len()
                                && import_source.starts_with(prefix)
                                && import_source.ends_with(suffix))
                            .then(|| {
                                import_source[prefix.len()..import_source.len() - suffix.len()]
                                    .to_string()
                            })
                        }
                        _ => None,
                    };
                    let Some(substitution) = substitution else {
                        continue;
                    };
                    for target in targets {
                        let Some(target) = target.as_str() else {
                            continue;
                        };
                        let candidate = target.replacen('*', &substitution, 1);
                        if let Some(found) = Self::canonicalize_or_probe(
                            d.join(base).join(candidate).to_string_lossy().as_ref(),
                        ) {
                            return Some(found);
                        }
                    }
                }
            }
            dir = d.parent();
        }
        None
    }

    fn find_tsconfig_base_url(start_dir: &std::path::Path) -> Option<(std::path::PathBuf, String)> {
        let mut dir = Some(start_dir);
        while let Some(d) = dir {
            let tsconfig = d.join("tsconfig.json");
            if tsconfig.is_file()
                && let Ok(text) = std::fs::read_to_string(&tsconfig)
                && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
                && let Some(base_url) = json
                    .get("compilerOptions")
                    .and_then(|c| c.get("baseUrl"))
                    .and_then(|v| v.as_str())
            {
                return Some((d.to_path_buf(), base_url.to_string()));
            }
            dir = d.parent();
        }
        None
    }

    fn dts_defines_alias(content: &str, alias: &str) -> bool {
        let escaped = regex::escape(alias);
        let pattern = format!(r"\b(type|interface|class|enum|namespace)\s+{}\b", escaped);
        match regex::Regex::new(&pattern) {
            Ok(re) => re.is_match(content),
            Err(_) => false,
        }
    }

    fn replace_unknown_alias(content: &mut String, alias: &str, type_string: &str) -> bool {
        let escaped = regex::escape(alias);
        let pattern = format!(r"export\s+type\s+{}\s*=\s*unknown\s*;", escaped);
        let Ok(re) = regex::Regex::new(&pattern) else {
            return false;
        };
        if !re.is_match(content) {
            return false;
        }
        let replacement = format!(
            "export type {} = {};",
            alias,
            type_string.trim().trim_end_matches(';')
        );
        *content = re.replace(content, replacement).to_string();
        true
    }

    fn normalize_consumer_method(method: Option<&str>) -> Option<String> {
        let raw = method.unwrap_or("").trim();
        if raw.is_empty() || raw.eq_ignore_ascii_case("unknown") {
            return Some("GET".to_string());
        }
        let normalized = normalize_manifest_method(raw);
        if is_http_method(&normalized) {
            Some(normalized)
        } else {
            None
        }
    }

    /// Build a MountGraph from aggregated file analysis results.
    ///
    /// This implements the key insight from the refactoring plan:
    /// The `import_source` field from each mount result is the key to cross-file resolution.
    pub fn build_mount_graph(
        &self,
        file_results: &HashMap<String, FileAnalysisResult>,
        normalizer: &UrlNormalizer,
        // Root the `file_results` keys are resolved against when classifying
        // endpoint provenance (real route vs mock/test handler): the service
        // scan root when keys are as-scanned paths, or `Path::new("")` when
        // the keys are already repo-relative (the engine normalizes them
        // before rebuilding the graph).
        scan_root: &Path,
        // Directory the `file_results` keys are relative to ON DISK, so an
        // imported binding can be resolved back to the module that defines
        // it. `Path::new("")` when the keys already resolve as written
        // (absolute, or relative to the working directory); the repo root
        // when the engine has normalized them to repo-relative paths.
        file_root: &Path,
    ) -> MountGraph {
        let mut graph = MountGraph::new();

        // Which mount-site binding stands for which analysed file, resolved
        // through the module graph — (file, exported name), never a bare name.
        // This is what attributes a mounted plugin's routes to the binding it
        // was registered under; everything below it is fallback.
        let owner_bindings = Self::resolve_mount_bindings(file_results, file_root);

        // Fallback name map for mounts the module graph cannot reach (tsconfig
        // path aliases, workspace packages, files not on disk): normalized
        // import source -> every child node mounted from it. A specifier that
        // fronts SEVERAL distinct children is a barrel, and the name alone
        // cannot say which module an endpoint belongs to, so the set is kept
        // whole and the ambiguity refused rather than silently resolved to
        // whichever mount was seen last.
        let mut import_map: HashMap<String, BTreeSet<String>> = HashMap::new();

        // First pass: collect all nodes and build import mappings
        for (file_path, result) in file_results {
            // Add nodes from endpoints
            for endpoint in &result.endpoints {
                let node_key = format!("{}:{}", file_path, endpoint.owner_node);
                if !graph.nodes.contains_key(&node_key) {
                    graph.nodes.insert(
                        endpoint.owner_node.clone(),
                        GraphNode {
                            name: endpoint.owner_node.clone(),
                            node_type: NodeType::Unknown,
                            creation_site: None,
                            file_location: format!("{}:{}", file_path, endpoint.line_number),
                        },
                    );
                }
            }

            // Add nodes and import mappings from mounts
            for mount in &result.mounts {
                // Add parent node
                if !graph.nodes.contains_key(&mount.parent_node) {
                    graph.nodes.insert(
                        mount.parent_node.clone(),
                        GraphNode {
                            name: mount.parent_node.clone(),
                            node_type: NodeType::Unknown,
                            creation_site: None,
                            file_location: format!("{}:{}", file_path, mount.line_number),
                        },
                    );
                }

                // Add child node
                if !graph.nodes.contains_key(&mount.child_node) {
                    graph.nodes.insert(
                        mount.child_node.clone(),
                        GraphNode {
                            name: mount.child_node.clone(),
                            node_type: NodeType::Unknown,
                            creation_site: None,
                            file_location: format!("{}:{}", file_path, mount.line_number),
                        },
                    );
                }

                // Track import source for cross-file resolution
                if let Some(import_source) = &mount.import_source {
                    // Normalize the import source
                    let normalized = Self::normalize_import_source(import_source);
                    import_map
                        .entry(normalized)
                        .or_default()
                        .insert(mount.child_node.clone());
                }
            }
        }

        // Second pass: build mount edges with resolved names
        for (file_path, result) in file_results {
            // Children mounted in THIS file. A parent that is one of them is a
            // router this file created or imported and then mounted itself, so
            // it is already a node in the graph and must not be rewritten.
            let local_children: HashSet<&str> = result
                .mounts
                .iter()
                .map(|mount| mount.child_node.as_str())
                .collect();
            for mount in &result.mounts {
                // A mount declared inside a mounted plugin names its parent as
                // the framework instance the plugin was handed (`server`,
                // `fastify`) — the same name every other module uses, and one
                // with no edge to the binding this module was registered
                // under. Resolved through the same file-first identity as an
                // endpoint's owner (#517), the chain reaches the root and the
                // ancestor prefixes survive; left as-is, everything above this
                // module's own prefix is lost (carrick#535).
                let parent = if local_children.contains(mount.parent_node.as_str()) {
                    mount.parent_node.clone()
                } else {
                    Self::resolve_endpoint_owner(
                        &owner_bindings,
                        &import_map,
                        &mount.parent_node,
                        file_path,
                    )
                };
                graph.mounts.push(MountEdge {
                    parent,
                    child: mount.child_node.clone(),
                    path_prefix: mount.mount_path.clone(),
                    middleware_stack: Vec::new(),
                });

                // Store import mapping for later endpoint resolution
                if let Some(import_source) = &mount.import_source {
                    let normalized = Self::normalize_import_source(import_source);
                    graph.nodes.insert(
                        format!("__import_map__::{}", normalized),
                        GraphNode {
                            name: mount.child_node.clone(),
                            node_type: NodeType::Unknown,
                            creation_site: None,
                            file_location: file_path.clone(),
                        },
                    );
                }
            }
        }

        // Third pass: infer node types based on mount behavior
        self.infer_node_types(&mut graph);

        // Fourth pass: add endpoints with resolved owners
        for (file_path, result) in file_results {
            for endpoint in &result.endpoints {
                let method = endpoint.method.trim().to_uppercase();
                if !is_http_method(&method) {
                    continue; // Skip non-HTTP methods (e.g., "use", empty)
                }

                // #580: a producer path is absolute. Every candidate call the
                // scanner raises is a *possible* route, and the analyzer will
                // answer with the literal it found there — so a decorator
                // argument (`@method('GET')`, `@accept('text/csv')`) or a
                // schema `$id` URL passed to a validator can arrive here
                // wearing the shape of an endpoint. None of them is a path
                // this service serves. Dropped at the point rows enter the
                // graph, which is the single choke point both callers of
                // `build_mount_graph` (the live pass and the engine's cached
                // rebuild) go through.
                //
                // Only the endpoint is dropped, not the owner node the first
                // pass registered for it: that node may also own real routes,
                // and it is inert without an endpoint attached.
                if !is_producer_route_path(&endpoint.path) {
                    debug!(
                        "Dropping endpoint {} {:?} at {}:{}: a served route's path starts with \
                         '/', so this literal is not one (decorator argument, content type, or \
                         absolute URL)",
                        method, endpoint.path, file_path, endpoint.line_number
                    );
                    continue;
                }

                // Try to resolve the owner using import information
                let resolved_owner = Self::resolve_endpoint_owner(
                    &owner_bindings,
                    &import_map,
                    &endpoint.owner_node,
                    file_path,
                );

                graph.endpoints.push(ResolvedEndpoint {
                    method,
                    path: endpoint.path.clone(),
                    full_path: endpoint.path.clone(), // Will be resolved later
                    handler: Some(endpoint.handler_name.clone()),
                    owner: resolved_owner,
                    file_location: format!("{}:{}", file_path, endpoint.line_number),
                    middleware_chain: Vec::new(),
                    repo_name: None,
                    service_name: None,
                    // Structural classification from the source path: mock
                    // trees keep producing endpoints, tagged as such (#380).
                    provenance: crate::file_finder::endpoint_provenance(
                        Path::new(file_path),
                        scan_root,
                    ),
                    // Assumed a route definition here; reclassified to
                    // `CallSite` by `classify_endpoint_evidence` when the same
                    // source site was also extracted as a data call (#379).
                    evidence: carrick_match::MatchEvidence::RouteDefinition,
                });
            }
        }

        // Fifth pass: add data calls.
        //
        // Collected before anything is committed to the graph so the
        // wrapper-echo suppression below can see the whole service's calls.
        // The bool is "candidate-backed": `apply_candidate_map` stamps a span
        // onto a data call only when the model's `candidate_id` joined a real
        // SWC HTTP candidate, i.e. only when the deterministic scanner saw an
        // HTTP client call at that source location.
        let mut collected: Vec<(DataFetchingCall, bool)> = Vec::new();
        for (file_path, result) in file_results {
            for data_call in &result.data_calls {
                let Some(method) = Self::normalize_consumer_method(data_call.method.as_deref())
                else {
                    continue;
                };
                // Drop calls whose target is not a real outgoing-call route
                // (SDK ops, bare identifiers, member expressions). Filtering at
                // the producer keeps the uploaded cross-repo index clean, not
                // just the local report.
                if !crate::analyzer::is_valid_route_shape(&data_call.target) {
                    debug!(
                        "Skipping data call with non-route target: {} ({})",
                        data_call.target, file_path
                    );
                    continue;
                }
                // Drop calls whose canonical path has no literal segment left
                // to match on (`${baseUrl}${path}`, a bare `${GQL_URL}`): they
                // can never join a producer key, so they are pure index noise
                // (#307) — typically a wrapper's internal fetch, whose resolved
                // call-site emissions are extracted separately and do match.
                let canonical_path = Self::canonical_call_path(normalizer, data_call);
                if !UrlNormalizer::canonical_path_has_literal_segment(&canonical_path) {
                    debug!(
                        "Skipping data call with no literal path segment: {} ({})",
                        data_call.target, file_path
                    );
                    continue;
                }
                collected.push((
                    DataFetchingCall {
                        method,
                        target_url: data_call.target.clone(),
                        canonical_path,
                        client: data_call.pattern_matched.clone(),
                        file_location: format!("{}:{}", file_path, data_call.line_number),
                        call_kind: data_call.call_kind,
                        repo_name: None,
                        service_name: None,
                        // Retention only. Normalisation computes the host and
                        // the extractor reports the line; both were previously
                        // thrown away (the line survived as packed text inside
                        // `file_location`). Neither field is read by matching.
                        host: normalizer.absolute_host(&data_call.target),
                        line: u32::try_from(data_call.line_number).ok().filter(|l| *l > 0),
                        // How the base resolves (carrick#649), read off the
                        // same target this row states. Retention only, like
                        // `host` and `line`: nothing in matching reads it.
                        base: data_call.base.clone(),
                        // What the member join could not follow for this row's
                        // client method (carrick#656), carried through so the
                        // index can state it beside the consumer list.
                        consumers_not_resolved: data_call.consumers_not_resolved.clone(),
                    },
                    data_call.call_expression_span_start.is_some(),
                ));
            }
        }

        // Suppress wrapper-resolution echoes (#369/#370 follow-up).
        //
        // A file that imports a module which itself performs HTTP is analyzed
        // with that module's source as wrapper context, and the model is asked
        // to emit the outbound call RESOLVED THROUGH the wrapper at the
        // delegating call site. That is the only record of the call when the
        // wrapper's own request URL is too templated to index (`${base}${path}`
        // is dropped a few lines above by the literal-segment gate) — which is
        // the case #370 was built for.
        //
        // It is a DUPLICATE when the wrapper's own request URL is already
        // concrete: the same physical outbound request is then extracted twice,
        // once at the wrapper's real client call and once at the delegating
        // site, under one (method, canonical path) but two file locations — so
        // nothing downstream collapses it and the reported call count is one
        // too high. The delegating site is distinguishable structurally: it has
        // no SWC HTTP candidate behind it, because the delegation
        // (`this.svc.fetchOrders()`, `helper.load()`) is not a client call the
        // scanner recognizes. No framework knowledge involved.
        //
        // So: drop a candidate-less call whose (method, canonical path) is
        // already carried by a candidate-backed call in this service. The
        // candidate-backed record wins — it is the real client call site and
        // the only one with spans for the type sidecar. When every record for a
        // key is candidate-less (the #370 case above), none is dropped.
        //
        // Known limitation: `swc_scanner` DOES raise a candidate for a plain
        // imported-function wrapper call (`getOrders()` from `./client`), so
        // when that wrapper's own URL is concrete both records are
        // candidate-backed and this rule does not separate them. Covered here
        // is the delegation-through-a-value shape (injected dependency, method
        // call, object property), which raises no candidate at the site.
        let anchored: HashSet<(String, String)> = collected
            .iter()
            .filter(|(_, candidate_backed)| *candidate_backed)
            .map(|(call, _)| (call.method.clone(), call.canonical_path.clone()))
            .collect();
        for (call, candidate_backed) in collected {
            if !candidate_backed
                && anchored.contains(&(call.method.clone(), call.canonical_path.clone()))
            {
                debug!(
                    "Suppressing wrapper-resolution echo of a call already extracted at its client call site: {} {} ({})",
                    call.method, call.canonical_path, call.file_location
                );
                continue;
            }
            graph.data_calls.push(call);
        }

        // Sixth pass: resolve full paths for endpoints
        self.resolve_endpoint_paths(&mut graph);

        // Evidence classification (#379): an "endpoint" whose exact source
        // site was ALSO extracted as a data call is a client call expression
        // the extraction double-classified (an integration/SDK operation
        // definition), not a route this service defines. Mark it as call-site
        // evidence so the cross-repo matcher reports a pair against it as a
        // shared external contract instead of fabricating a producer role.
        // Must run BEFORE the self-call suppression below: suppression deletes
        // the twin data call (the fabricated endpoint matches it), destroying
        // the evidence.
        Self::classify_endpoint_evidence(&mut graph);

        // Seventh pass: suppress self-calls. A data call whose (method, canonical
        // path) matches one of THIS service's own resolved endpoints is the
        // service hitting its own HTTP surface (e.g. a cron/reindex job fetching
        // `http://localhost:PORT/warehouses/:id/stock/:sku`), not a cross-repo
        // dependency. Emitting it would (a) inject a spurious self producer↔
        // consumer edge and (b) leak an operation the service already exposes as
        // a producer. The mount graph is built per service, so a call matching an
        // endpoint in the SAME graph is necessarily intra-service; a genuine
        // cross-service-same-repo call lives in a different service's graph and is
        // untouched. Runs after path resolution so the endpoint `full_path`s are
        // final, and matches param-aware (`find_matching_endpoints`) so a
        // canonicalized `/warehouses/:wid/...` still matches a declared
        // `/warehouses/:warehouseId/...`. This is only reachable once the literal
        // origin is stripped from the call key (see `consumer_call_path`); a raw
        // `http://host:port/...` key matches no endpoint and would evade it.
        let keep_call: Vec<bool> = graph
            .data_calls
            .iter()
            .map(|call| {
                // Two guards on what counts as a self-call, composed:
                // - A zero-agreement routing match (#381) — this service's own
                //   catch-all fallback (`GET /*`) absorbing the call — is not
                //   evidence of a self-call: it would suppress every one of
                //   the service's real outgoing calls. Only a producer that
                //   shares literal path signal with the call counts.
                // - Only route-definition evidence counts (#379): a call whose
                //   only match is a call-site-evidence endpoint (its own
                //   double-extracted twin) is a call to an EXTERNAL contract,
                //   not the service hitting its own surface — keep it so the
                //   shared-external-contract group can form cross-repo.
                let is_self_call = graph
                    .find_matching_endpoints(&call.canonical_path, &call.method)
                    .iter()
                    .any(|endpoint| {
                        endpoint.evidence == carrick_match::MatchEvidence::RouteDefinition
                            && carrick_match::match_agreement(
                                &endpoint.full_path,
                                &call.canonical_path,
                            )
                            .unwrap_or(0)
                                > 0
                    });
                if is_self_call {
                    debug!(
                        "Suppressing self-call to own endpoint: {} {} ({})",
                        call.method, call.canonical_path, call.file_location
                    );
                }
                !is_self_call
            })
            .collect();
        // `keep_call` was derived element-for-element from `graph.data_calls`
        // just above, so the iterator running dry mid-retain can only mean that
        // invariant was broken — fail loudly rather than silently keeping calls.
        let mut keep_iter = keep_call.into_iter();
        graph.data_calls.retain(|_| {
            keep_iter
                .next()
                .expect("keep_call must be exactly as long as graph.data_calls")
        });

        graph
    }

    fn normalize_import_source(source: &str) -> String {
        source
            .trim_start_matches("./")
            .trim_start_matches("../")
            .trim_end_matches(".ts")
            .trim_end_matches(".js")
            .trim_end_matches(".tsx")
            .trim_end_matches(".jsx")
            .to_string()
    }

    /// Infer node types based on mount behavior.
    fn infer_node_types(&self, graph: &mut MountGraph) {
        // Nodes that are mounted by others are Mountable
        let mounted_children: std::collections::HashSet<_> =
            graph.mounts.iter().map(|m| m.child.clone()).collect();

        // Nodes that mount others are potential Roots
        let mounting_parents: std::collections::HashSet<_> =
            graph.mounts.iter().map(|m| m.parent.clone()).collect();

        for (name, node) in graph.nodes.iter_mut() {
            if name.starts_with("__import_map__") {
                continue;
            }

            if mounted_children.contains(name) {
                node.node_type = NodeType::Mountable;
            } else if mounting_parents.contains(name) && !mounted_children.contains(name) {
                node.node_type = NodeType::Root;
            }
        }
    }

    /// Resolve every mount's imported child back to the file that DEFINES it,
    /// keyed by the `file_results` key of that file.
    ///
    /// This is the identity the old name-based resolution lost. A barrel gives
    /// four mounts the same import specifier (`./routes.js`) and each module
    /// typically names its own plugin identically (`const routes = ...; export
    /// default routes`), so neither the specifier nor the local symbol
    /// distinguishes them — only the module a binding was re-exported FROM
    /// does. `BindingResolver` walks `export { default as X } from`,
    /// `export { a as b } from`, and `export * from` hops to find it, with no
    /// framework knowledge involved: an Express router, a Fastify plugin, and
    /// anything else the mount graph follows resolve through the same path.
    ///
    /// Bindings are deduped by `child_node` so the same module mounted under
    /// several prefixes stays ONE binding (the alias fan-out in
    /// `resolve_endpoint_paths` handles the prefixes, #373).
    ///
    /// Keys are resolved against `file_root`, which is `Path::new("")` when
    /// they already resolve as written and the repo root when the engine has
    /// normalized them to repo-relative paths.
    ///
    /// Returns an empty map when nothing resolves — non-relative specifiers,
    /// or `file_results` keys that are not paths on disk (in-memory tests, a
    /// graph rebuilt away from the checkout). Callers fall back to the name
    /// map there.
    fn resolve_mount_bindings(
        file_results: &HashMap<String, FileAnalysisResult>,
        file_root: &Path,
    ) -> HashMap<String, Vec<MountBinding>> {
        // Canonical path -> the `file_results` key naming that file. Both
        // sides are canonicalized: the resolver returns canonical paths, and a
        // key can reach the same file through a relative path or a symlinked
        // parent (`/var` -> `/private/var` on macOS).
        let mut key_by_canonical: HashMap<PathBuf, &str> = HashMap::new();
        for key in file_results.keys() {
            if let Ok(canonical) = file_root.join(key).canonicalize() {
                key_by_canonical.insert(canonical, key.as_str());
            }
        }
        if key_by_canonical.is_empty() {
            return HashMap::new();
        }

        // `file_results` is a HashMap: sort the mount sites so the bindings
        // (and any tie-break between them) are identical run to run.
        let mut mount_sites: Vec<(&str, &MountResult)> = file_results
            .iter()
            .flat_map(|(file_path, result)| {
                result
                    .mounts
                    .iter()
                    .map(move |mount| (file_path.as_str(), mount))
            })
            .collect();
        mount_sites.sort_by(|(a_file, a), (b_file, b)| {
            a_file
                .cmp(b_file)
                .then(a.line_number.cmp(&b.line_number))
                .then(a.child_node.cmp(&b.child_node))
        });

        let mut resolver = BindingResolver::new();
        let mut bindings: HashMap<String, Vec<MountBinding>> = HashMap::new();
        for (file_path, mount) in mount_sites {
            let Some(import_source) = &mount.import_source else {
                continue; // locally defined child: no module to resolve
            };
            let Ok(importer) = file_root.join(file_path).canonicalize() else {
                continue;
            };
            let Some(resolved) = resolver.resolve(&importer, import_source, &mount.child_node)
            else {
                continue;
            };
            let Some(target_key) = key_by_canonical.get(&resolved.file) else {
                continue; // defining module was not analysed (no endpoints in it)
            };
            let entry = bindings.entry((*target_key).to_string()).or_default();
            if entry
                .iter()
                .any(|binding| binding.child_node == mount.child_node)
            {
                continue;
            }
            entry.push(MountBinding {
                child_node: mount.child_node.clone(),
                local_name: resolved.local_name,
            });
        }
        bindings
    }

    /// Resolve the node an endpoint belongs to, so its path can be joined to
    /// the prefix its module was mounted under.
    ///
    /// Identity is file-first: when exactly one mounted binding resolves to
    /// the file an endpoint was extracted from, that binding owns it. The
    /// endpoint's own `owner_node` cannot carry that decision — it is the
    /// variable the route was attached to, which for a plugin is the
    /// framework instance handed to it (`server`, `fastify`), a name every
    /// module in a repo shares.
    ///
    /// `local_name` only breaks ties, in the rarer case where one file
    /// defines several mounted routers. When it cannot break the tie, the
    /// original owner is kept: an unmounted-looking endpoint is a visible
    /// gap, a confidently wrong prefix is not.
    fn resolve_endpoint_owner(
        owner_bindings: &HashMap<String, Vec<MountBinding>>,
        import_map: &HashMap<String, BTreeSet<String>>,
        owner_name: &str,
        file_path: &str,
    ) -> String {
        if let Some(bindings) = owner_bindings.get(file_path) {
            if let [only] = bindings.as_slice() {
                return only.child_node.clone();
            }
            if let Some(binding) = bindings
                .iter()
                .find(|binding| binding.local_name.as_deref() == Some(owner_name))
            {
                return binding.child_node.clone();
            }
            // Several bindings resolved to this file and none of them names
            // this owner: refuse to guess.
            return owner_name.to_string();
        }

        // Fallback: match the file against the import specifiers seen at mount
        // sites. Substring matching, so it is only ever a hint — `./routes.js`
        // matches every path containing "routes". Take the most specific
        // pattern that matches, and use it only when the mounts agree on a
        // single child; a specifier fronting several children is a barrel this
        // path cannot see through.
        let file_parts: Vec<&str> = file_path.split('/').collect();
        let mut best_len = 0usize;
        let mut candidates: BTreeSet<&str> = BTreeSet::new();
        for (pattern, children) in import_map {
            let matches =
                file_path.contains(pattern) || file_parts.iter().any(|part| part.contains(pattern));
            if !matches || pattern.len() < best_len {
                continue;
            }
            if pattern.len() > best_len {
                best_len = pattern.len();
                candidates.clear();
            }
            candidates.extend(children.iter().map(String::as_str));
        }
        match candidates.iter().copied().collect::<Vec<_>>().as_slice() {
            [only] => (*only).to_string(),
            _ => owner_name.to_string(),
        }
    }

    /// Every mounted node's full prefix chains, composed from the root down.
    ///
    /// `mounts` is a name-keyed edge list, so a node can have several parents
    /// (the same router name mounted in more than one file) and a chain can
    /// in principle loop; both are handled by walking each edge with a
    /// path-local visited set and capping the fan-out. Chains are sorted so
    /// the emitted paths do not depend on `HashMap` iteration order.
    fn mount_prefix_chains(
        mounts: &[crate::mount_graph::MountEdge],
    ) -> HashMap<String, Vec<String>> {
        // child -> [(parent, prefix)], deduplicated.
        let mut by_child: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
        for mount in mounts {
            let edges = by_child.entry(mount.child.as_str()).or_default();
            let edge = (mount.parent.as_str(), mount.path_prefix.as_str());
            if !edges.contains(&edge) {
                edges.push(edge);
            }
        }

        // `graph.mounts` is built by iterating a `HashMap` of file results, so
        // the edge order arriving here is not stable. Sorted before the walk:
        // the fan-out cap truncates, and an unsorted cap would keep a
        // different subset of chains run to run — inside uploaded index data.
        for edges in by_child.values_mut() {
            edges.sort();
        }

        let mut resolved: HashMap<String, Vec<String>> = HashMap::new();
        for child in by_child.keys() {
            let mut visiting: HashSet<&str> = HashSet::new();
            let mut chains = Self::chains_for(child, &by_child, &mut visiting);
            chains.sort();
            chains.dedup();
            resolved.insert((*child).to_string(), chains);
        }
        resolved
    }

    /// How many prefix chains one node may resolve to. Alias fan-out is
    /// normally one or two; a name-keyed graph in a large repo can multiply
    /// them, and past this point the extra chains are noise, not routes.
    const MAX_PREFIX_CHAINS: usize = 8;

    fn chains_for<'a>(
        node: &'a str,
        by_child: &HashMap<&'a str, Vec<(&'a str, &'a str)>>,
        visiting: &mut HashSet<&'a str>,
    ) -> Vec<String> {
        let Some(edges) = by_child.get(node) else {
            return Vec::new();
        };
        if !visiting.insert(node) {
            return Vec::new(); // cycle: this node is already on the walk
        }
        let mut chains: Vec<String> = Vec::new();
        for (parent, prefix) in edges {
            // An edge back onto a node already on this walk is a cycle. Drop
            // the edge rather than the node: composing through it would
            // invent a prefix the route never carries, and a mount point
            // whose own ancestry is unresolvable is still the top of this
            // chain.
            if visiting.contains(parent) {
                continue;
            }
            let parent_chains = Self::chains_for(parent, by_child, visiting);
            if parent_chains.is_empty() {
                chains.push(Self::join_paths(prefix, ""));
            } else {
                for parent_chain in parent_chains {
                    chains.push(Self::join_paths(&parent_chain, prefix));
                }
            }
            if chains.len() >= Self::MAX_PREFIX_CHAINS {
                debug!(
                    "Mount chain fan-out for '{}' hit the {} chain cap; extra chains dropped",
                    node,
                    Self::MAX_PREFIX_CHAINS
                );
                chains.truncate(Self::MAX_PREFIX_CHAINS);
                break;
            }
        }
        visiting.remove(node);
        chains
    }

    /// Resolve full paths for endpoints by traversing the mount graph.
    fn resolve_endpoint_paths(&self, graph: &mut MountGraph) {
        // Build owner -> full mount path prefixes. A router's own mount
        // prefix is only the last hop: routers are routinely registered on a
        // parent that is itself registered under a prefix
        // (`app.register(v1, { prefix: "/api/v1" })` with
        // `v1.register(orders, { prefix: "/orders" })`), and taking the last
        // hop alone dropped every ancestor — an endpoint declared at `/` in a
        // router mounted three levels down was indexed as `/`, colliding with
        // every other router's `/` (carrick#535). Each chain is walked to the
        // root and composed.
        //
        // A child mounted under several prefixes (path aliases, e.g. the same
        // sub-router mounted at both `/api/v2` and `/api/v2-beta`) serves its
        // routes under EACH of them, so every chain is kept separately (#373).
        // Concatenating them into one string produced junk keys like
        // `/api/v2/api/v2-beta/x` and silently dropped every alias's endpoint
        // set but the fused one.
        let owner_prefixes = Self::mount_prefix_chains(&graph.mounts);

        // Apply prefixes to endpoints, fanning an endpoint out once per
        // distinct alias prefix. Endpoints whose owner is not mounted keep
        // their path as the full path.
        let mut resolved = Vec::with_capacity(graph.endpoints.len());
        for endpoint in graph.endpoints.drain(..) {
            match owner_prefixes.get(&endpoint.owner) {
                Some(prefixes) => {
                    let mut seen_full_paths: HashSet<String> = HashSet::new();
                    for prefix in prefixes {
                        let full_path = Self::join_paths(prefix, &endpoint.path);
                        // Distinct prefixes can still join to the same full
                        // path (the idempotent guard in `join_paths` skips a
                        // prefix the path already carries); emit each full
                        // path once.
                        if seen_full_paths.insert(full_path.clone()) {
                            let mut fanned = endpoint.clone();
                            fanned.full_path = full_path;
                            resolved.push(fanned);
                        }
                    }
                }
                None => resolved.push(endpoint),
            }
        }
        graph.endpoints = resolved;
    }

    /// Reclassify endpoints whose evidence is actually a client call
    /// expression (#379).
    ///
    /// An endpoint is call-site evidence when a data call in the SAME graph
    /// shares its exact source site (`file:line`), its method, and a matching
    /// path — the extraction emitted one call expression as BOTH an endpoint
    /// and a data call (e.g. an integration platform's request-descriptor
    /// `perform` operation). The triple gate is deliberately strict, purely
    /// structural, and framework-agnostic: a genuine route definition never
    /// shares its exact line, verb, AND path with an outbound call. Known
    /// limitation (logged at debug level via the reclassification message
    /// only): a fabricated endpoint whose twin call was emitted at a
    /// different line is not caught and keeps route-definition evidence.
    ///
    /// Runs after `resolve_endpoint_paths` (so `full_path` is final) and
    /// before self-call suppression (which deletes the twin call).
    fn classify_endpoint_evidence(graph: &mut MountGraph) {
        for endpoint in &mut graph.endpoints {
            let twinned = graph.data_calls.iter().any(|call| {
                call.file_location == endpoint.file_location
                    && call.method.eq_ignore_ascii_case(&endpoint.method)
                    && carrick_match::paths_match(&endpoint.full_path, &call.canonical_path)
            });
            if twinned {
                debug!(
                    "Endpoint {} {} at {} is a double-extracted call expression; \
                     reclassifying as call-site evidence",
                    endpoint.method, endpoint.full_path, endpoint.file_location
                );
                endpoint.evidence = carrick_match::MatchEvidence::CallSite;
            }
        }
    }

    fn join_paths(prefix: &str, path: &str) -> String {
        let trimmed_prefix = prefix.trim_end_matches('/');
        let trimmed_path = path.trim_start_matches('/');

        if trimmed_prefix.is_empty() {
            if trimmed_path.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", trimmed_path)
            }
        } else {
            // Every emitted path must be absolute. A mount prefix reaches here
            // exactly as extracted (only trimmed), so a slashless one would
            // otherwise emit a slashless full path, which the type-manifest
            // writer drops on its `starts_with('/')` guard.
            let pfx = if trimmed_prefix.starts_with('/') {
                trimmed_prefix.to_string()
            } else {
                format!("/{}", trimmed_prefix)
            };
            if trimmed_path.is_empty() {
                return pfx;
            }
            // Idempotent guard: if the endpoint path already carries this prefix,
            // don't apply it twice. This happens when a constructor-carried prefix
            // is baked into the endpoint path AND also (redundantly) emitted as the
            // mount's path_prefix — without the guard that doubled to
            // `/api/v1/api/v1/status`. Match on a segment boundary so `/api` does
            // not spuriously swallow `/apixyz`. Framework-agnostic.
            let full = format!("/{}", trimmed_path);
            match full.strip_prefix(&pfx) {
                // Already prefixed (exact, or at a segment boundary) — don't double it.
                Some(rest) if rest.is_empty() || rest.starts_with('/') => full,
                _ => format!("{}/{}", pfx, trimmed_path),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::file_analyzer_agent::{DataCallResult, EndpointResult, MountResult};

    /// #369: relative import specifiers resolve through the TS extension
    /// order to an existing file; package and alias specifiers resolve to
    /// nothing (alias resolution is sidecar territory).
    #[test]
    fn resolve_relative_import_extension_order_and_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("nodes")).unwrap();
        std::fs::create_dir_all(root.join("lib/client")).unwrap();
        std::fs::write(root.join("nodes/GenericFunctions.ts"), "export {}").unwrap();
        std::fs::write(root.join("lib/client/index.ts"), "export {}").unwrap();
        let importer = root.join("nodes/Survey.node.ts");
        std::fs::write(&importer, "import {} from './GenericFunctions';").unwrap();

        // Extension-less sibling specifier → .ts file.
        let resolved = FileOrchestrator::resolve_relative_import(&importer, "./GenericFunctions")
            .expect("sibling .ts should resolve");
        assert!(resolved.ends_with("nodes/GenericFunctions.ts"));

        // Directory specifier → index.ts.
        let resolved = FileOrchestrator::resolve_relative_import(&importer, "../lib/client")
            .expect("directory index should resolve");
        assert!(resolved.ends_with("lib/client/index.ts"));

        // Package + alias specifiers are out of scope.
        assert!(FileOrchestrator::resolve_relative_import(&importer, "n8n-workflow").is_none());
        assert!(FileOrchestrator::resolve_relative_import(&importer, "@/lib/client").is_none());
        // Nonexistent target resolves to nothing.
        assert!(FileOrchestrator::resolve_relative_import(&importer, "./missing").is_none());
    }

    /// #468: under `moduleResolution: nodenext` a relative import names the
    /// EMITTED JS (`./helper.js`) while the source on disk is `helper.ts`.
    /// `resolve_relative_import` must apply the same `.js`→`.ts` rewrite the
    /// scanner already does in `ts_sibling_candidates`/`canonicalize_or_probe`
    /// (carrick#148), otherwise #369/#370 wrapper resolution and cross-file env
    /// aliasing are inert on every NodeNext repo.
    #[test]
    fn resolve_relative_import_rewrites_js_specifier_to_ts_source() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let src = root.join("src");
        // Only the TS-family sources exist on disk — exactly what a NodeNext
        // repo looks like before `tsc` has emitted anything.
        std::fs::write(src.join("helper.ts"), "export {}").unwrap();
        std::fs::write(src.join("widget.tsx"), "export {}").unwrap();
        std::fs::write(src.join("typed.d.ts"), "export {}").unwrap();
        std::fs::write(src.join("modern.mts"), "export {}").unwrap();
        std::fs::write(src.join("legacyCjs.cts"), "export {}").unwrap();
        // A genuine JS-only module: no TS sibling, so the literal file wins.
        std::fs::write(src.join("vendored.js"), "module.exports = {}").unwrap();
        let importer = src.join("caller.ts");
        std::fs::write(&importer, "import {} from './helper.js';").unwrap();

        let cases = [
            ("./helper.js", "src/helper.ts"),
            ("./widget.js", "src/widget.tsx"),
            ("./typed.js", "src/typed.d.ts"),
            ("./modern.mjs", "src/modern.mts"),
            ("./legacyCjs.cjs", "src/legacyCjs.cts"),
            // Regression guard for the probe-order change: a real emitted/
            // vendored `.js` with no TS sibling still resolves to itself.
            ("./vendored.js", "src/vendored.js"),
        ];
        for (spec, expected) in cases {
            let resolved = FileOrchestrator::resolve_relative_import(&importer, spec)
                .unwrap_or_else(|| panic!("{spec} should resolve to {expected}"));
            assert!(
                resolved.ends_with(expected),
                "{spec} resolved to {resolved:?}, expected it to end with {expected}"
            );
        }

        // A parent-relative `.js` specifier resolves the same way (the live
        // shape is `../../helper.js`).
        let nested = root.join("src/nodes/deep.ts");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "import {} from '../helper.js';").unwrap();
        let resolved = FileOrchestrator::resolve_relative_import(&nested, "../helper.js")
            .expect("../helper.js should resolve to the .ts source");
        assert!(resolved.ends_with("src/helper.ts"));

        // TS-family specifiers are probed exactly — no extension swapping, and
        // a dangling one still resolves to nothing.
        let resolved = FileOrchestrator::resolve_relative_import(&importer, "./helper.ts")
            .expect("explicit .ts specifier should resolve");
        assert!(resolved.ends_with("src/helper.ts"));
        assert!(FileOrchestrator::resolve_relative_import(&importer, "./missing.js").is_none());
    }

    /// #472 helpers: build a throwaway source map + handler, and a
    /// `wrapper_map` whose keys are the canonical paths of `wrappers`.
    fn reexport_test_env() -> (Lrc<SourceMap>, Handler) {
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
        (cm, handler)
    }

    fn wrapper_map_of(wrappers: &[PathBuf]) -> HashMap<PathBuf, WrapperModule> {
        wrappers
            .iter()
            .map(|p| {
                (
                    p.canonicalize().expect("wrapper must exist"),
                    WrapperModule {
                        snippet: format!("--- wrapper module: {} ---\n", p.display()),
                        request_shape: None,
                    },
                )
            })
            .collect()
    }

    /// #472: `reexport_sources` collects ONLY re-export declarations. An
    /// ordinary import is not a pass-through (a module that imports a wrapper
    /// does not re-publish it), and a type-only re-export cannot carry a
    /// runtime fetch helper.
    #[test]
    fn reexport_sources_collects_only_value_re_exports() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("barrel.ts");
        std::fs::write(
            &file,
            r#"
import { unrelated } from "./notAReExport.js";
export * from "./starred.js";
export * as ns from "./namespaced.js";
export { helper } from "./named.js";
export { helper as aliased, other } from "./mixed.js";
export type { Options } from "./typesOnly.js";
export type * from "./starTypesOnly.js";
export { type OnlyAType } from "./inlineTypesOnly.js";
export { local };
const local = 1;
"#,
        )
        .unwrap();
        let (cm, handler) = reexport_test_env();
        let sources = FileOrchestrator::reexport_sources(&file, &cm, &handler);
        assert_eq!(
            sources,
            vec![
                "./starred.js",
                "./namespaced.js",
                "./named.js",
                "./mixed.js",
            ],
            "only value re-exports are pass-throughs"
        );
    }

    /// #472: the core defect. A wrapper import that resolves to a re-export
    /// barrel found nothing, because a barrel raises zero HTTP candidates and
    /// so is never in `wrapper_map`. Following `export * from` / named
    /// re-exports reaches the module that actually defines the helper.
    ///
    /// Cases, all on one fixture tree: single barrel, chained barrels, a named
    /// re-export, a self-referencing cycle, a chain one hop past the cap, and
    /// the unchanged direct-hit path.
    #[test]
    fn wrapper_modules_behind_follows_re_export_barrels() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();

        // The defining module: the only one that would raise HTTP candidates.
        let impl_path = src.join("fetching.ts");
        std::fs::write(
            &impl_path,
            "export async function typedFetch(u: string) { return fetch(u); }",
        )
        .unwrap();

        // One hop: barrel -> impl.
        std::fs::write(src.join("barrel.ts"), r#"export * from "./fetching.js";"#).unwrap();
        // Named re-export instead of a star.
        std::fs::write(
            src.join("named.ts"),
            r#"export { typedFetch } from "./fetching.js";"#,
        )
        .unwrap();
        // Two hops: outer -> inner -> impl.
        std::fs::write(src.join("outer.ts"), r#"export * from "./inner.js";"#).unwrap();
        std::fs::write(src.join("inner.ts"), r#"export * from "./fetching.js";"#).unwrap();
        // Cycle: ringA <-> ringB, reaching nothing.
        std::fs::write(src.join("ringA.ts"), r#"export * from "./ringB.js";"#).unwrap();
        std::fs::write(src.join("ringB.ts"), r#"export * from "./ringA.js";"#).unwrap();
        // Self-referencing barrel.
        std::fs::write(
            src.join("ouroboros.ts"),
            r#"export * from "./ouroboros.js";"#,
        )
        .unwrap();
        // Four hops: one past WRAPPER_REEXPORT_MAX_HOPS (3).
        std::fs::write(src.join("deep1.ts"), r#"export * from "./deep2.js";"#).unwrap();
        std::fs::write(src.join("deep2.ts"), r#"export * from "./deep3.js";"#).unwrap();
        std::fs::write(src.join("deep3.ts"), r#"export * from "./deep4.js";"#).unwrap();
        std::fs::write(src.join("deep4.ts"), r#"export * from "./fetching.js";"#).unwrap();
        // Exactly three hops: the last chain that must still resolve.
        std::fs::write(src.join("edge1.ts"), r#"export * from "./edge2.js";"#).unwrap();
        std::fs::write(src.join("edge2.ts"), r#"export * from "./edge3.js";"#).unwrap();
        std::fs::write(src.join("edge3.ts"), r#"export * from "./fetching.js";"#).unwrap();
        // A module that merely IMPORTS the wrapper is not a pass-through.
        std::fs::write(
            src.join("importsOnly.ts"),
            r#"import { typedFetch } from "./fetching.js"; export const x = typedFetch;"#,
        )
        .unwrap();

        let (cm, handler) = reexport_test_env();
        let wrapper_map = wrapper_map_of(std::slice::from_ref(&impl_path));
        let canonical_impl = impl_path.canonicalize().unwrap();

        let behind = |module: &str| {
            let resolved = src.join(module).canonicalize().expect("module must exist");
            let mut cache: HashMap<PathBuf, Vec<String>> = HashMap::new();
            FileOrchestrator::wrapper_modules_behind(
                &resolved,
                None,
                &wrapper_map,
                &mut cache,
                &cm,
                &handler,
            )
        };

        // Direct hit: unchanged behaviour, no following.
        assert_eq!(behind("fetching.ts"), vec![canonical_impl.clone()]);
        // Single barrel — the live shape, and what fails on main.
        assert_eq!(
            behind("barrel.ts"),
            vec![canonical_impl.clone()],
            "`export * from` barrel must resolve to the defining module"
        );
        // Named re-export.
        assert_eq!(behind("named.ts"), vec![canonical_impl.clone()]);
        // Chained barrels.
        assert_eq!(behind("outer.ts"), vec![canonical_impl.clone()]);
        // Exactly at the cap.
        assert_eq!(behind("edge1.ts"), vec![canonical_impl.clone()]);

        // Cycles terminate and find nothing.
        assert!(behind("ringA.ts").is_empty(), "barrel cycle must terminate");
        assert!(
            behind("ouroboros.ts").is_empty(),
            "self-referencing barrel must terminate"
        );
        // One hop past the cap is not followed.
        assert!(
            behind("deep1.ts").is_empty(),
            "chain longer than WRAPPER_REEXPORT_MAX_HOPS must not resolve"
        );
        // Ordinary imports are never pass-throughs.
        assert!(behind("importsOnly.ts").is_empty());
    }

    /// #472: a barrel that fans out to several modules yields every wrapper
    /// behind it, sorted by canonical path so the prompt context does not
    /// depend on `imported_symbols` HashMap iteration order. The re-export
    /// cache is shared, so each module is parsed once.
    #[test]
    fn wrapper_modules_behind_is_sorted_and_memoized() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let a = src.join("aFetch.ts");
        let b = src.join("bFetch.ts");
        std::fs::write(&a, "export function a(u: string) { return fetch(u); }").unwrap();
        std::fs::write(&b, "export function b(u: string) { return fetch(u); }").unwrap();
        std::fs::write(src.join("plain.ts"), "export const plain = 1;").unwrap();
        // Declaration order is b, plain, a — the result must come back sorted.
        std::fs::write(
            src.join("wide.ts"),
            r#"export * from "./bFetch.js";
export * from "./plain.js";
export * from "./aFetch.js";"#,
        )
        .unwrap();

        let (cm, handler) = reexport_test_env();
        let wrapper_map = wrapper_map_of(&[a.clone(), b.clone()]);
        let mut cache: HashMap<PathBuf, Vec<String>> = HashMap::new();
        let resolved = src.join("wide.ts").canonicalize().unwrap();
        let hits = FileOrchestrator::wrapper_modules_behind(
            &resolved,
            None,
            &wrapper_map,
            &mut cache,
            &cm,
            &handler,
        );
        let mut expected = vec![a.canonicalize().unwrap(), b.canonicalize().unwrap()];
        expected.sort();
        assert_eq!(hits, expected);

        // `plain.ts` is not a wrapper, so it was expanded — proving the cache
        // holds every module whose re-exports were read, not just the barrel.
        assert!(cache.contains_key(&resolved));
        assert!(cache.contains_key(&src.join("plain.ts").canonicalize().unwrap()));

        // A second call reuses the cache: mutate the barrel on disk and the
        // memoized specifiers still drive the result.
        std::fs::write(src.join("wide.ts"), "export const nothing = 1;").unwrap();
        let hits_again = FileOrchestrator::wrapper_modules_behind(
            &resolved,
            None,
            &wrapper_map,
            &mut cache,
            &cm,
            &handler,
        );
        assert_eq!(hits_again, expected, "re-export sources must be memoized");
    }

    /// #472: a package's public-surface `index.ts` re-exports its whole HTTP
    /// surface. It is not an alias for one helper, so it stands for nothing —
    /// otherwise every file importing the package barrel would carry dozens of
    /// wrapper snippets into its prompt. Measured on a real monorepo: a helper
    /// barrel fronts one HTTP module, the package barrel fronts 22.
    #[test]
    fn wrapper_modules_behind_rejects_a_wide_package_barrel() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();

        let mut wrappers: Vec<PathBuf> = Vec::new();
        let mut narrow = String::new();
        let mut wide = String::new();
        for i in 0..(FileOrchestrator::WRAPPER_REEXPORT_MAX_HITS + 1) {
            let path = src.join(format!("client{i}.ts"));
            std::fs::write(
                &path,
                format!("export function c{i}(u: string) {{ return fetch(u); }}"),
            )
            .unwrap();
            wrappers.push(path);
            wide.push_str(&format!("export * from \"./client{i}.js\";\n"));
            if i < FileOrchestrator::WRAPPER_REEXPORT_MAX_HITS {
                narrow.push_str(&format!("export * from \"./client{i}.js\";\n"));
            }
        }
        std::fs::write(src.join("wideBarrel.ts"), &wide).unwrap();
        std::fs::write(src.join("narrowBarrel.ts"), &narrow).unwrap();

        let (cm, handler) = reexport_test_env();
        let wrapper_map = wrapper_map_of(&wrappers);
        let mut cache: HashMap<PathBuf, Vec<String>> = HashMap::new();

        let wide_hits = FileOrchestrator::wrapper_modules_behind(
            &src.join("wideBarrel.ts").canonicalize().unwrap(),
            None,
            &wrapper_map,
            &mut cache,
            &cm,
            &handler,
        );
        assert!(
            wide_hits.is_empty(),
            "a barrel over the whole HTTP surface stands for no single wrapper"
        );

        // Exactly at the limit still resolves, so the rejection is a cliff at a
        // stated threshold rather than a silent squeeze.
        let narrow_hits = FileOrchestrator::wrapper_modules_behind(
            &src.join("narrowBarrel.ts").canonicalize().unwrap(),
            None,
            &wrapper_map,
            &mut cache,
            &cm,
            &handler,
        );
        // A barrel wide enough to exhaust the visit budget is rejected too, even
        // though only a couple of its targets are wrappers: the budget ran out
        // before its width could be measured, so it cannot be judged narrow.
        let mut sparse = String::new();
        for i in 0..(FileOrchestrator::WRAPPER_REEXPORT_MAX_VISITS + 1) {
            let path = src.join(format!("plain{i}.ts"));
            std::fs::write(&path, format!("export const p{i} = {i};")).unwrap();
            sparse.push_str(&format!("export * from \"./plain{i}.js\";\n"));
        }
        sparse.push_str("export * from \"./client0.js\";\n");
        std::fs::write(src.join("sparseBarrel.ts"), &sparse).unwrap();
        let sparse_hits = FileOrchestrator::wrapper_modules_behind(
            &src.join("sparseBarrel.ts").canonicalize().unwrap(),
            None,
            &wrapper_map,
            &mut cache,
            &cm,
            &handler,
        );
        assert!(
            sparse_hits.is_empty(),
            "an unmeasurably wide barrel must not sneak under the hit limit"
        );

        assert_eq!(
            narrow_hits.len(),
            FileOrchestrator::WRAPPER_REEXPORT_MAX_HITS
        );
    }

    /// #472: the importer itself is excluded from the follow, so a barrel that
    /// re-exports back to the importing file cannot make a file its own
    /// wrapper context.
    #[test]
    fn wrapper_modules_behind_excludes_the_importer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let consumer = src.join("consumer.ts");
        std::fs::write(
            &consumer,
            "export function go(u: string) { return fetch(u); }",
        )
        .unwrap();
        std::fs::write(src.join("loop.ts"), r#"export * from "./consumer.js";"#).unwrap();

        let (cm, handler) = reexport_test_env();
        // The importer is itself wrapper material (it raised candidates).
        let wrapper_map = wrapper_map_of(std::slice::from_ref(&consumer));
        let mut cache: HashMap<PathBuf, Vec<String>> = HashMap::new();
        let self_canon = consumer.canonicalize().unwrap();
        let resolved = src.join("loop.ts").canonicalize().unwrap();
        let hits = FileOrchestrator::wrapper_modules_behind(
            &resolved,
            Some(&self_canon),
            &wrapper_map,
            &mut cache,
            &cm,
            &handler,
        );
        assert!(hits.is_empty(), "a file must not become its own wrapper");
    }

    /// Pub/sub Part B: a NATS pub/sub-only file produces ZERO SWC candidates,
    /// so the orchestrator's zero-candidate skip would drop it before the
    /// file-analyzer. The skip is bypassed iff `imports_messaging_client` is
    /// true. This test drives that exact decision through a real `scan_content`
    /// to prove (a) the import sources ARE available at the skip point, and
    /// (b) the force-analyze verdict, including inertness under the empty
    /// (live default) `messaging_clients`.
    #[test]
    fn messaging_client_import_forces_analysis_of_zero_candidate_file() {
        use crate::swc_scanner::SwcScanner;
        use std::path::PathBuf;

        let scanner = SwcScanner::new();

        // A pub/sub-only NATS file: imports "nats", only calls publish/subscribe.
        let nats_src = r#"
            import { connect } from 'nats';
            const SUBJECT = 'orders.created';
            const nc = await connect();
            nc.publish(SUBJECT, JSON.stringify({ id: 1 }));
            nc.subscribe(SUBJECT);
        "#;
        let nats_scan = scanner.scan_content(&PathBuf::from("orders_pub.ts"), nats_src, &[], &[]);
        // Precondition for the whole fix: this file has NO HTTP/data candidates,
        // so it would hit the zero-candidate skip without Part B.
        assert!(
            nats_scan.candidates.is_empty(),
            "expected zero candidates for a pub/sub-only file, got {:?}",
            nats_scan.candidates
        );
        // The import source must be visible at the skip point.
        assert!(
            nats_scan.import_sources.iter().any(|s| s == "nats"),
            "import_sources should contain 'nats', got {:?}",
            nats_scan.import_sources
        );

        // 1) With the cloud-detected messaging_clients=["nats"], the NATS file is
        //    force-analyzed (NOT skipped).
        assert!(
            FileOrchestrator::imports_messaging_client(
                &nats_scan.import_sources,
                &["nats".to_string()],
            ),
            "a nats-importing file with messaging_clients=[nats] must be force-analyzed"
        );

        // 2) Inertness: with the LIVE default messaging_clients=[] (cloud not yet
        //    deployed), the SAME file is NOT force-analyzed -> it is skipped,
        //    exactly as today. This is the no-behavior-change guarantee.
        assert!(
            !FileOrchestrator::imports_messaging_client(&nats_scan.import_sources, &[]),
            "empty messaging_clients (live default) must leave the file skippable (inert)"
        );

        // 3) No collateral: a file importing only an unrelated package is skipped
        //    even when messaging_clients=["nats"].
        let lodash_scan = scanner.scan_content(
            &PathBuf::from("util.ts"),
            "import _ from 'lodash';\n",
            &[],
            &["nats".to_string()],
        );
        assert!(
            !FileOrchestrator::imports_messaging_client(
                &lodash_scan.import_sources,
                &["nats".to_string()],
            ),
            "a lodash-only file must not be force-analyzed by messaging_clients=[nats]"
        );

        // Scoped/subpath specifiers match their package entry (e.g. a NATS file
        // importing the scoped client under a messaging_clients entry).
        assert!(
            FileOrchestrator::imports_messaging_client(
                &["@nats-io/nats-core".to_string()],
                &["@nats-io/nats-core".to_string()],
            ),
            "scoped messaging-client import must match its entry"
        );
        assert!(
            FileOrchestrator::imports_messaging_client(
                &["ioredis/built/Redis".to_string()],
                &["ioredis".to_string()],
            ),
            "subpath import must match its package entry"
        );
    }

    #[test]
    fn join_paths_does_not_double_a_baked_prefix() {
        // The double-prefix bug: a constructor-carried prefix baked into the
        // endpoint path AND also emitted as the mount prefix must resolve once.
        assert_eq!(
            FileOrchestrator::join_paths("/api/v1", "/api/v1/status"),
            "/api/v1/status"
        );
        // Exact match (prefix == path) also collapses to one.
        assert_eq!(
            FileOrchestrator::join_paths("/api/v1", "/api/v1"),
            "/api/v1"
        );
        // Normal mount-site prefix (path has no prefix) still applies.
        assert_eq!(
            FileOrchestrator::join_paths("/api/v1", "/status"),
            "/api/v1/status"
        );
        // No false positive: a shared textual prefix that is NOT a segment
        // boundary must still be joined.
        assert_eq!(
            FileOrchestrator::join_paths("/api", "/apixyz"),
            "/api/apixyz"
        );
        // Empty prefix passes the path through.
        assert_eq!(FileOrchestrator::join_paths("", "/users"), "/users");
    }

    /// A mount prefix that arrives without a leading slash must still produce
    /// an absolute full path. Nothing between the extraction pass and
    /// `resolve_endpoint_paths` guarantees the slash (the mount path is only
    /// trimmed), and a slashless full path is dropped outright by the
    /// `starts_with('/')` guards in the type-manifest writer, so the endpoint
    /// gets no SymbolRequest and no bundled types.
    #[test]
    fn join_paths_normalizes_a_slashless_prefix() {
        // Nested path under a slashless prefix: the join must not leak the raw
        // prefix through (pre-fix this emitted `field/document/field/create-many`).
        assert_eq!(
            FileOrchestrator::join_paths("field", "/document/field/create-many"),
            "/field/document/field/create-many"
        );
        // Root route on a slashless-prefixed router (`router.get('/')`).
        assert_eq!(FileOrchestrator::join_paths("field", "/"), "/field");
        // Empty endpoint path is the same branch.
        assert_eq!(FileOrchestrator::join_paths("field", ""), "/field");
        // The idempotent guard still holds when the prefix has no leading slash.
        assert_eq!(
            FileOrchestrator::join_paths("api/v1", "/api/v1/status"),
            "/api/v1/status"
        );
        // A slashless prefix with a trailing slash must not double the separator.
        assert_eq!(
            FileOrchestrator::join_paths("field/", "/create"),
            "/field/create"
        );
        assert_eq!(FileOrchestrator::join_paths("field/", "/"), "/field");
    }

    /// Regression: `tsconfig.json` with `"baseUrl": "."` makes
    /// `import { X } from "types/user"` resolve to `<repo>/types/user.ts`.
    /// Pre-fix this hit the early `if !import_source.starts_with('.')` return
    /// and dropped through to the sidecar with a literal `types/user`, which
    /// then failed `fs.existsSync` and emitted "Source file not found".
    #[test]
    fn test_resolve_import_path_uses_tsconfig_baseurl_for_bare_specifier() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(
            repo.path().join("tsconfig.json"),
            r#"{ "compilerOptions": { "baseUrl": "." } }"#,
        )
        .unwrap();
        std::fs::create_dir_all(repo.path().join("types")).unwrap();
        std::fs::write(
            repo.path().join("types/user.ts"),
            "export interface User { id: number }",
        )
        .unwrap();
        let server = repo.path().join("server.ts");
        std::fs::write(&server, "// stub").unwrap();

        let resolved =
            FileOrchestrator::resolve_import_path(server.to_string_lossy().as_ref(), "types/user");

        let expected = repo.path().join("types/user.ts").canonicalize().unwrap();
        assert_eq!(
            std::path::Path::new(&resolved).canonicalize().unwrap(),
            expected,
            "bare specifier should resolve via baseUrl, not fall through"
        );
    }

    /// Bare specifiers that aren't on disk (real node_modules packages like
    /// `react`) must still pass through unchanged so downstream code can
    /// distinguish package imports from missing local files.
    #[test]
    fn test_resolve_import_path_preserves_unresolvable_bare_specifier() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(
            repo.path().join("tsconfig.json"),
            r#"{ "compilerOptions": { "baseUrl": "." } }"#,
        )
        .unwrap();
        let server = repo.path().join("server.ts");
        std::fs::write(&server, "// stub").unwrap();

        let resolved =
            FileOrchestrator::resolve_import_path(server.to_string_lossy().as_ref(), "react");

        assert_eq!(resolved, "react");
    }

    /// Corpus-3 regression (`@meridian/contracts`): a workspace shared-types
    /// package imported through a tsconfig `paths` mapping must resolve to the
    /// mapped file. Pre-fix only `baseUrl` was consulted, so the specifier
    /// fell through unchanged and the sidecar bundler reported "Could not
    /// extract type definition" for every symbol it carried.
    #[test]
    fn test_resolve_import_path_uses_tsconfig_paths_mapping() {
        let repo = tempfile::tempdir().unwrap();
        // Mirror the monorepo layout: packages/catalog-api consumes
        // packages/contracts via a paths alias.
        std::fs::create_dir_all(repo.path().join("packages/catalog-api/src")).unwrap();
        std::fs::create_dir_all(repo.path().join("packages/contracts/src")).unwrap();
        std::fs::write(
            repo.path().join("packages/catalog-api/tsconfig.json"),
            r#"{ "compilerOptions": { "baseUrl": ".", "paths": { "@meridian/contracts": ["../contracts/src/index.ts"] } } }"#,
        )
        .unwrap();
        std::fs::write(
            repo.path().join("packages/contracts/src/index.ts"),
            "export interface Product { id: string }",
        )
        .unwrap();
        let routes = repo
            .path()
            .join("packages/catalog-api/src/products.routes.ts");
        std::fs::write(&routes, "// stub").unwrap();

        let resolved = FileOrchestrator::resolve_import_path(
            routes.to_string_lossy().as_ref(),
            "@meridian/contracts",
        );

        let expected = repo
            .path()
            .join("packages/contracts/src/index.ts")
            .canonicalize()
            .unwrap();
        assert_eq!(
            std::path::Path::new(&resolved).canonicalize().unwrap(),
            expected,
            "bare specifier should resolve via the tsconfig paths mapping"
        );
    }

    /// The spec's single-`*` wildcard form (`"@app/*": ["src/*"]`).
    #[test]
    fn test_resolve_import_path_uses_tsconfig_paths_wildcard() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("src/models")).unwrap();
        std::fs::write(
            repo.path().join("tsconfig.json"),
            r#"{ "compilerOptions": { "paths": { "@app/*": ["src/*"] } } }"#,
        )
        .unwrap();
        std::fs::write(
            repo.path().join("src/models/user.ts"),
            "export interface User { id: number }",
        )
        .unwrap();
        let server = repo.path().join("server.ts");
        std::fs::write(&server, "// stub").unwrap();

        let resolved = FileOrchestrator::resolve_import_path(
            server.to_string_lossy().as_ref(),
            "@app/models/user",
        );

        let expected = repo
            .path()
            .join("src/models/user.ts")
            .canonicalize()
            .unwrap();
        assert_eq!(
            std::path::Path::new(&resolved).canonicalize().unwrap(),
            expected,
            "wildcard paths mapping should resolve, baseUrl defaulting to the tsconfig dir"
        );
    }

    /// A paths mapping whose target does not exist must not eat the
    /// specifier — real package imports still pass through.
    #[test]
    fn test_resolve_import_path_paths_mapping_falls_through_when_target_missing() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(
            repo.path().join("tsconfig.json"),
            r#"{ "compilerOptions": { "paths": { "react": ["vendored/react.ts"] } } }"#,
        )
        .unwrap();
        let server = repo.path().join("server.ts");
        std::fs::write(&server, "// stub").unwrap();

        let resolved =
            FileOrchestrator::resolve_import_path(server.to_string_lossy().as_ref(), "react");

        assert_eq!(resolved, "react");
    }

    /// `tsc` only enables baseUrl-based non-relative resolution when the
    /// option is explicitly set. A tsconfig without `baseUrl` must not
    /// shadow real package imports — bare specifiers should pass through.
    #[test]
    fn test_resolve_import_path_skips_baseurl_when_not_set() {
        let repo = tempfile::tempdir().unwrap();
        // tsconfig WITHOUT baseUrl
        std::fs::write(
            repo.path().join("tsconfig.json"),
            r#"{ "compilerOptions": { "strict": true } }"#,
        )
        .unwrap();
        // A file at types/user.ts that *would* resolve if we defaulted
        // baseUrl to "." — must NOT be picked up here.
        std::fs::create_dir_all(repo.path().join("types")).unwrap();
        std::fs::write(
            repo.path().join("types/user.ts"),
            "export interface User { id: number }",
        )
        .unwrap();
        let server = repo.path().join("server.ts");
        std::fs::write(&server, "// stub").unwrap();

        let resolved =
            FileOrchestrator::resolve_import_path(server.to_string_lossy().as_ref(), "types/user");

        assert_eq!(
            resolved, "types/user",
            "without explicit baseUrl, bare specifiers must pass through unchanged",
        );
    }

    /// Pre-fix, a relative import like `./foo.ts` whose target couldn't be
    /// canonicalized (broken symlink, absent file, permissions) fell through
    /// to a `.ts.ts` double-extension fallback because the wrapper helper
    /// returned `None` for already-extension paths and the outer code
    /// blindly appended `.ts`.
    #[test]
    fn test_resolve_import_path_no_double_extension_for_missing_relative() {
        let repo = tempfile::tempdir().unwrap();
        let server = repo.path().join("server.ts");
        std::fs::write(&server, "// stub").unwrap();

        let resolved = FileOrchestrator::resolve_import_path(
            server.to_string_lossy().as_ref(),
            "./missing.ts",
        );

        assert!(
            !resolved.ends_with(".ts.ts"),
            "relative import with extension must not get .ts appended on miss; got `{}`",
            resolved
        );
        assert!(
            resolved.ends_with(".ts"),
            "should still surface a single-`.ts` path; got `{}`",
            resolved
        );
    }

    /// Relative imports continue to resolve against the importing file's
    /// directory, not against tsconfig.baseUrl.
    #[test]
    fn test_resolve_import_path_relative_imports_unaffected() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("src/types")).unwrap();
        std::fs::write(
            repo.path().join("src/types/order.ts"),
            "export interface Order {}",
        )
        .unwrap();
        let server = repo.path().join("src/server.ts");
        std::fs::write(&server, "// stub").unwrap();

        let resolved = FileOrchestrator::resolve_import_path(
            server.to_string_lossy().as_ref(),
            "./types/order",
        );

        let expected = repo
            .path()
            .join("src/types/order.ts")
            .canonicalize()
            .unwrap();
        assert_eq!(
            std::path::Path::new(&resolved).canonicalize().unwrap(),
            expected,
        );
    }

    /// carrick#148: NodeNext/ESM writes a relative import as `./types.js`
    /// even when the source is `types.ts`. tsc resolves the `.js` specifier
    /// to the `.ts` sibling; our resolver must too, or the `.js` path is
    /// carried into the sidecar bundle where it `fs.existsSync`-fails and
    /// logs "Source file not found" (losing the type).
    #[test]
    fn test_resolve_import_path_js_specifier_resolves_to_ts_sibling() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        std::fs::write(
            repo.path().join("src/types.ts"),
            "export interface SearchByIntentResponse { hits: number }",
        )
        .unwrap();
        let server = repo.path().join("src/index.ts");
        std::fs::write(&server, "// stub").unwrap();

        let resolved =
            FileOrchestrator::resolve_import_path(server.to_string_lossy().as_ref(), "./types.js");

        let resolved_path = std::path::Path::new(&resolved);
        assert!(
            resolved_path.is_file(),
            "`.js` specifier should resolve to the on-disk `.ts` sibling, got `{resolved}`",
        );
        let expected = repo.path().join("src/types.ts").canonicalize().unwrap();
        assert_eq!(
            resolved_path.canonicalize().unwrap(),
            expected,
            "`./types.js` must resolve to `types.ts`, not a non-existent `types.js`",
        );
    }

    /// The `.ts` sibling must win even when a real emitted `types.js` also
    /// sits next to it — tsc prefers the TypeScript source over the JS output.
    #[test]
    fn test_resolve_import_path_ts_wins_over_js_sibling() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        std::fs::write(
            repo.path().join("src/types.ts"),
            "export interface SearchByIntentResponse { hits: number }",
        )
        .unwrap();
        // Decoy emitted output next to the source.
        std::fs::write(repo.path().join("src/types.js"), "module.exports = {};").unwrap();
        let server = repo.path().join("src/index.ts");
        std::fs::write(&server, "// stub").unwrap();

        let resolved =
            FileOrchestrator::resolve_import_path(server.to_string_lossy().as_ref(), "./types.js");

        let expected = repo.path().join("src/types.ts").canonicalize().unwrap();
        assert_eq!(
            std::path::Path::new(&resolved).canonicalize().unwrap(),
            expected,
            "`.ts` sibling must win over an emitted `.js` decoy",
        );
    }

    /// A literal NodeNext TS-family extension (`.mts`/`.cts`) must be probed
    /// exactly, not treated as extensionless and probed as `foo.mts.ts`
    /// (Copilot review on #148).
    #[test]
    fn test_resolve_import_path_mts_specifier_resolves_exactly() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        std::fs::write(
            repo.path().join("src/types.mts"),
            "export interface SearchByIntentResponse { hits: number }",
        )
        .unwrap();
        let server = repo.path().join("src/index.ts");
        std::fs::write(&server, "// stub").unwrap();

        let resolved =
            FileOrchestrator::resolve_import_path(server.to_string_lossy().as_ref(), "./types.mts");

        let expected = repo.path().join("src/types.mts").canonicalize().unwrap();
        assert_eq!(
            std::path::Path::new(&resolved).canonicalize().unwrap(),
            expected,
            "`./types.mts` must resolve to `types.mts`, not a nonsensical `types.mts.ts`",
        );
    }

    #[test]
    fn test_normalize_import_source() {
        assert_eq!(
            FileOrchestrator::normalize_import_source("./routes/users"),
            "routes/users"
        );
        assert_eq!(
            FileOrchestrator::normalize_import_source("../api/index.ts"),
            "api/index"
        );
        assert_eq!(
            FileOrchestrator::normalize_import_source("./auth.js"),
            "auth"
        );
        assert_eq!(
            FileOrchestrator::normalize_import_source("components/Header.tsx"),
            "components/Header"
        );
    }

    #[test]
    fn test_build_mount_graph_from_single_file() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mut file_results = HashMap::new();
        file_results.insert(
            "src/app.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![MountResult {
                    line_number: 10,
                    parent_node: "app".to_string(),
                    child_node: "userRouter".to_string(),
                    mount_path: "/users".to_string(),
                    import_source: Some("./routes/users".to_string()),
                    pattern_matched: ".use(".to_string(),
                }],
                endpoints: vec![EndpointResult {
                    candidate_id: "span:100-140".to_string(),
                    line_number: 5,
                    owner_node: "app".to_string(),
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
                }],
                data_calls: vec![],
                graphql_operations: vec![],
                pubsub_operations: vec![],
            },
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );

        assert_eq!(graph.mounts.len(), 1);
        assert_eq!(graph.endpoints.len(), 1);
        assert_eq!(graph.mounts[0].parent, "app");
        assert_eq!(graph.mounts[0].child, "userRouter");
        assert_eq!(graph.mounts[0].path_prefix, "/users");
    }

    /// #580: an analyzer row whose path is not absolute is not a route this
    /// service serves. The reported shapes are a decorator argument
    /// (`@method('GET')` -> `GET`, `@accept('text/csv')` -> `text/csv`) and a
    /// JSON-schema `$id` passed to a validator, all of which reached the
    /// endpoint list on a real scan. The real `GET /health` in the same file
    /// must survive, so this is a drop and not a whole-file suppression.
    #[test]
    fn build_mount_graph_drops_endpoint_rows_whose_path_is_not_absolute() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let endpoint = |line_number: i32, method: &str, path: &str| EndpointResult {
            candidate_id: format!("span:{line_number}"),
            line_number,
            owner_node: "SettingsController".to_string(),
            method: method.to_string(),
            path: path.to_string(),
            handler_name: "get".to_string(),
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
        };

        let mut file_results = HashMap::new();
        file_results.insert(
            "src/settings/controller.ts".to_string(),
            FileAnalysisResult {
                endpoints: vec![
                    endpoint(10, "GET", "GET"),
                    endpoint(22, "GET", "text/csv"),
                    endpoint(15, "POST", "https://example.invalid/schemas/start.json"),
                    endpoint(30, "GET", "/settings"),
                ],
                ..Default::default()
            },
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );

        let paths: Vec<&str> = graph.endpoints.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["/settings"],
            "only the absolute path is a served route"
        );
        assert!(
            graph.endpoints.iter().all(|e| e.path.starts_with('/')),
            "no endpoint row may carry a non-absolute path"
        );
    }

    fn class_controller_fixture_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/class-controller-api")
    }

    /// Every file the class-controller fixture would be scanned with, in the
    /// same shape `analyze_files` receives them (paths as found on disk).
    fn class_controller_fixture_files() -> Vec<PathBuf> {
        let src = class_controller_fixture_root().join("src");
        let mut files = vec![src.join("routes.ts"), src.join("framework.ts")];
        for entry in std::fs::read_dir(src.join("controllers")).expect("controllers dir") {
            files.push(entry.expect("dir entry").path());
        }
        files.push(src.join("middleware/error-handler.ts"));
        files.sort();
        files
    }

    /// Resolve the fixture's route table to its endpoints, the way a scan
    /// does: scan the table for bindings, then join each to the controller
    /// module it names.
    fn class_controller_fixture_endpoints() -> Vec<(PathBuf, EndpointResult)> {
        let routes = class_controller_fixture_root()
            .join("src/routes.ts")
            .canonicalize()
            .expect("the fixture route table exists");
        let content = std::fs::read_to_string(&routes).expect("route table is readable");
        let scanner = SwcScanner::new();
        let bindings = scanner.controller_route_bindings(&routes, &content);
        assert_eq!(
            bindings.len(),
            8,
            "the fixture binds eight paths, one behind middleware: {bindings:?}"
        );
        let mut resolver = BindingResolver::new();
        FileOrchestrator::class_controller_endpoints(&scanner, &mut resolver, &routes, &bindings)
    }

    /// #580 part b: the recall half. A route table binds a literal path to an
    /// imported controller instance and the controller module never names its
    /// own path, so neither file states a route and single-file analysis finds
    /// none. Joined across the two, every bound path appears with the method,
    /// owner and handler the controller declares, located at the controller's
    /// own file and line.
    #[test]
    fn class_controller_endpoints_join_every_bound_path_to_its_controller() {
        let controllers = class_controller_fixture_root()
            .join("src/controllers")
            .canonicalize()
            .expect("the fixture controllers directory exists");

        let mut routes: Vec<(String, String, String, String, String)> =
            class_controller_fixture_endpoints()
                .into_iter()
                .map(|(file, endpoint)| {
                    let located = file
                        .strip_prefix(&controllers)
                        .expect("every route resolves to a controller module")
                        .display()
                        .to_string();
                    (
                        endpoint.method,
                        endpoint.path,
                        endpoint.owner_node,
                        endpoint.handler_name,
                        format!("{}:{}", located, endpoint.line_number),
                    )
                })
                .collect();
        routes.sort();

        let expected: Vec<(String, String, String, String, String)> = [
            (
                "DELETE",
                "/session",
                "SessionController",
                "delete",
                "session.ts:8",
            ),
            (
                "DELETE",
                "/widget/:id",
                "WidgetItemController",
                "delete",
                "widget-item.ts:12",
            ),
            ("GET", "/", "RootController", "get", "root.ts:4"),
            ("GET", "/health", "HealthController", "get", "health.ts:5"),
            (
                "GET",
                "/profile",
                "ProfileController",
                "get",
                "profile.ts:4",
            ),
            // Not verb-named: the method comes from the `@method('GET')`
            // literal, and the row is located at the handler, not at the
            // decorator above it.
            (
                "GET",
                "/report",
                "ReportController",
                "exportCsv",
                "report.ts:7",
            ),
            (
                "GET",
                "/session",
                "SessionController",
                "get",
                "session.ts:4",
            ),
            ("GET", "/widget", "WidgetController", "get", "widget.ts:4"),
            (
                "GET",
                "/widget/:id",
                "WidgetItemController",
                "get",
                "widget-item.ts:4",
            ),
            (
                "PATCH",
                "/profile",
                "ProfileController",
                "patch",
                "profile.ts:8",
            ),
            // Middleware sits in front of the controller; the handler is the
            // LAST argument.
            ("POST", "/token", "TokenController", "post", "token.ts:4"),
            ("POST", "/widget", "WidgetController", "post", "widget.ts:8"),
            (
                "PUT",
                "/widget/:id",
                "WidgetItemController",
                "put",
                "widget-item.ts:8",
            ),
        ]
        .into_iter()
        .map(|(method, path, owner, handler, location)| {
            (
                method.to_string(),
                path.to_string(),
                owner.to_string(),
                handler.to_string(),
                location.to_string(),
            )
        })
        .collect();

        assert_eq!(routes, expected);
    }

    /// #580: the fixture's negatives. A schema `$id` URL passed to a
    /// validator, a `@accept('text/csv')` content type, and a helper method
    /// that is neither verb-named nor verb-decorated all sit inside
    /// controllers that DO serve routes — so each has to be rejected
    /// individually, not by suppressing the file.
    #[test]
    fn class_controller_endpoints_emit_nothing_for_non_routes() {
        let endpoints = class_controller_fixture_endpoints();

        assert!(
            endpoints.iter().all(|(_, e)| e.path.starts_with('/')),
            "no row may carry a path that is not absolute: {:?}",
            endpoints
                .iter()
                .map(|(_, e)| &e.path)
                .collect::<Vec<&String>>()
        );
        let handlers: Vec<&str> = endpoints
            .iter()
            .map(|(_, e)| e.handler_name.as_str())
            .collect();
        assert!(
            !handlers.contains(&"buildRows"),
            "a method that is neither verb-named nor verb-decorated is not a route"
        );
        assert!(
            !endpoints
                .iter()
                .any(|(file, _)| file.ends_with("error-handler.ts")),
            "the middleware in front of /token must own no route"
        );
    }

    /// #580 part b end to end: the emitted rows survive the mount graph — the
    /// absolute-path gate part (a) added, and owner resolution, which must not
    /// rewrite a controller class into some other node.
    #[test]
    fn build_mount_graph_keeps_every_class_controller_route() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);
        let files = class_controller_fixture_files();

        let mut file_results = HashMap::new();
        let added = FileOrchestrator::merge_class_controller_endpoints(
            &mut file_results,
            &files,
            class_controller_fixture_endpoints(),
        );
        assert_eq!(added, 13, "eight bound paths, thirteen handler methods");

        // Each controller's rows land under the key its own file was scanned
        // with, so a controller that also carries call-site endpoints keeps
        // one entry rather than two.
        let widget = class_controller_fixture_root()
            .join("src/controllers/widget.ts")
            .to_string_lossy()
            .to_string();
        assert_eq!(
            file_results
                .get(&widget)
                .map(|r| r.endpoints.len())
                .unwrap_or_default(),
            2,
            "WidgetController serves GET and POST at /widget"
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );
        assert_eq!(graph.endpoints.len(), 13);
        assert!(
            graph.endpoints.iter().all(|e| e.path.starts_with('/')),
            "part (a)'s gate must pass every real route through"
        );

        let mut paths: Vec<&str> = graph
            .endpoints
            .iter()
            .map(|e| e.full_path.as_str())
            .collect();
        paths.sort();
        paths.dedup();
        assert_eq!(
            paths,
            vec![
                "/",
                "/health",
                "/profile",
                "/report",
                "/session",
                "/token",
                "/widget",
                "/widget/:id",
            ],
            "the bound path is used as-is: a controller owns no mount prefix"
        );

        let token = graph
            .endpoints
            .iter()
            .find(|e| e.full_path == "/token")
            .expect("the middleware-fronted route resolves to its controller");
        assert_eq!(token.method, "POST");
        assert_eq!(token.owner, "TokenController");
        assert_eq!(token.handler.as_deref(), Some("post"));
        assert!(
            token.file_location.ends_with("controllers/token.ts:4"),
            "located at the handler, not at the route table: {}",
            token.file_location
        );
    }

    /// #373: a sub-router mounted under multiple path aliases serves its routes
    /// under EACH alias. Reproduces the reported Hono shape
    /// (`app.route('/api/v2', downloadRoute); app.route('/api/v2-beta', downloadRoute)`):
    /// the two mount prefixes must fan out into one endpoint per alias, not
    /// concatenate into a junk key like `/api/v2/api/v2-beta/download`.
    #[test]
    fn test_build_mount_graph_fans_out_mount_path_aliases() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mount = |line_number: i32, mount_path: &str| MountResult {
            line_number,
            parent_node: "app".to_string(),
            child_node: "downloadRoute".to_string(),
            mount_path: mount_path.to_string(),
            import_source: Some("./routes/download".to_string()),
            pattern_matched: ".route(".to_string(),
        };

        let mut file_results = HashMap::new();
        file_results.insert(
            "src/app.ts".to_string(),
            FileAnalysisResult {
                mounts: vec![mount(10, "/api/v2"), mount(11, "/api/v2-beta")],
                ..Default::default()
            },
        );
        file_results.insert(
            "src/routes/download.ts".to_string(),
            FileAnalysisResult {
                endpoints: vec![EndpointResult {
                    candidate_id: "span:100-140".to_string(),
                    line_number: 5,
                    owner_node: "downloadRoute".to_string(),
                    method: "GET".to_string(),
                    path: "/download".to_string(),
                    handler_name: "getDownload".to_string(),
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
                }],
                ..Default::default()
            },
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );

        let mut full_paths: Vec<&str> = graph
            .endpoints
            .iter()
            .map(|e| e.full_path.as_str())
            .collect();
        full_paths.sort_unstable();
        assert_eq!(
            full_paths,
            vec!["/api/v2-beta/download", "/api/v2/download"],
            "each mount alias must yield its own endpoint"
        );
    }

    #[test]
    fn test_build_mount_graph_tags_mock_tree_endpoints_with_provenance() {
        use crate::operation::EndpointProvenance;

        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let endpoint = |path: &str| EndpointResult {
            candidate_id: "span:100-140".to_string(),
            line_number: 5,
            owner_node: "http".to_string(),
            method: "GET".to_string(),
            path: path.to_string(),
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
        };
        let file_result = |path: &str| FileAnalysisResult {
            endpoints: vec![endpoint(path)],
            ..Default::default()
        };

        let mut file_results = HashMap::new();
        // MSW-style mock-service-worker handler tree: extracted, but tagged.
        file_results.insert(
            "src/mocks/handlers.ts".to_string(),
            file_result("/api/widgets"),
        );
        // A real route registration in product source.
        file_results.insert("src/routes/widgets.ts".to_string(), file_result("/widgets"));

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );

        assert_eq!(graph.endpoints.len(), 2);
        let by_file = |needle: &str| {
            graph
                .endpoints
                .iter()
                .find(|e| e.file_location.starts_with(needle))
                .unwrap_or_else(|| panic!("no endpoint from {needle}"))
        };
        assert_eq!(
            by_file("src/mocks/").provenance,
            EndpointProvenance::Mock,
            "a mock-tree handler must be extracted with provenance=mock"
        );
        assert_eq!(
            by_file("src/routes/").provenance,
            EndpointProvenance::Route,
            "a real route must be extracted with provenance=route"
        );
    }

    #[test]
    fn test_build_mount_graph_provenance_is_scan_root_relative() {
        use crate::operation::EndpointProvenance;

        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mut file_results = HashMap::new();
        file_results.insert(
            "tests/fixtures/mocks/service-a/src/index.ts".to_string(),
            FileAnalysisResult {
                endpoints: vec![EndpointResult {
                    candidate_id: "span:1-2".to_string(),
                    line_number: 1,
                    owner_node: "app".to_string(),
                    method: "GET".to_string(),
                    path: "/health".to_string(),
                    handler_name: "health".to_string(),
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
                }],
                ..Default::default()
            },
        );

        // A scan rooted inside a mock/test prefix (eval fixtures) must not tag
        // everything under it as mock: only segments below the root count.
        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new("tests/fixtures/mocks/service-a"),
            Path::new(""),
        );

        assert_eq!(graph.endpoints.len(), 1);
        assert_eq!(graph.endpoints[0].provenance, EndpointProvenance::Route);
    }

    #[test]
    fn test_join_paths_avoids_double_slashes() {
        assert_eq!(FileOrchestrator::join_paths("/", "/users"), "/users");
        assert_eq!(FileOrchestrator::join_paths("/api", "/users"), "/api/users");
        assert_eq!(
            FileOrchestrator::join_paths("/api/", "/users"),
            "/api/users"
        );
        assert_eq!(FileOrchestrator::join_paths("", "/users"), "/users");
        assert_eq!(FileOrchestrator::join_paths("/api", "/"), "/api");
    }

    #[test]
    fn test_build_mount_graph_with_data_calls() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mut file_results = HashMap::new();
        file_results.insert(
            "src/service.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![],
                data_calls: vec![DataCallResult {
                    call_kind: None,
                    candidate_id: "span:200-260".to_string(),
                    line_number: 15,
                    target: "https://api.example.com/data".to_string(),
                    method: Some("POST".to_string()),
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
                }],
                graphql_operations: vec![],
                pubsub_operations: vec![],
            },
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );

        assert_eq!(graph.data_calls.len(), 1);
        assert_eq!(
            graph.data_calls[0].target_url,
            "https://api.example.com/data"
        );
        assert_eq!(graph.data_calls[0].method, "POST");
    }

    /// The host normalisation computes and the line extraction reports are both
    /// retained on the call, and `file_location` keeps the exact text it
    /// always had.
    ///
    /// The host is retained WITHOUT classification: `api.vendor.test` is
    /// declared external here and `orders.internal.test` internal, and both
    /// keep their host. What the declarations decide is matching, which this
    /// retention does not touch.
    #[test]
    fn data_calls_retain_the_host_and_a_typed_line() {
        let orchestrator = FileOrchestrator::new(AgentService::new());
        let config = crate::config::Config {
            internal_domains: ["orders.internal.test".to_string()].into_iter().collect(),
            external_domains: ["api.vendor.test".to_string()].into_iter().collect(),
            external_env_vars: ["BILLING_API".to_string()].into_iter().collect(),
            ..Default::default()
        };

        let mut file_results = HashMap::new();
        file_results.insert(
            "src/service.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![],
                data_calls: vec![
                    call_with_span(12, "https://api.vendor.test/v1/charges", Some(200)),
                    call_with_span(18, "https://orders.internal.test/orders", Some(210)),
                    call_with_span(24, "${process.env.BILLING_API}/invoices", Some(220)),
                    call_with_span(31, "/api/local", Some(230)),
                ],
                graphql_operations: vec![],
                pubsub_operations: vec![],
            },
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::new(&config),
            Path::new(""),
            Path::new(""),
        );

        let mut retained: Vec<(Option<&str>, Option<u32>, &str)> = graph
            .data_calls
            .iter()
            .map(|call| (call.host.as_deref(), call.line, call.file_location.as_str()))
            .collect();
        retained.sort();
        assert_eq!(
            retained,
            vec![
                (None, Some(24), "src/service.ts:24"),
                (None, Some(31), "src/service.ts:31"),
                (Some("api.vendor.test"), Some(12), "src/service.ts:12"),
                (Some("orders.internal.test"), Some(18), "src/service.ts:18"),
            ],
            "an env-var base and a relative path have no literal host to retain"
        );
    }

    /// A call to a DECLARED external domain keeps its origin on
    /// `canonical_path` — the key `mount_graph_to_api_details` uploads and the
    /// matcher looks up. Stripping it there is what made a declared third-party
    /// call arrive at matching as a bare `/user`, indistinguishable from an
    /// internal call and reported as a missing endpoint.
    #[test]
    fn declared_external_absolute_url_keeps_its_host_on_the_match_key() {
        let orchestrator = FileOrchestrator::new(AgentService::new());
        let config = crate::config::Config {
            external_domains: ["https://api.vendor.test".to_string()]
                .into_iter()
                .collect(),
            ..Default::default()
        };

        let mut file_results = HashMap::new();
        file_results.insert(
            "src/service.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![],
                data_calls: vec![
                    call_with_span(12, "https://api.vendor.test/v1/charges", Some(200)),
                    call_with_span(18, "https://orders.undeclared.test/orders", Some(210)),
                ],
                graphql_operations: vec![],
                pubsub_operations: vec![],
            },
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::new(&config),
            Path::new(""),
            Path::new(""),
        );

        let mut keys: Vec<&str> = graph
            .data_calls
            .iter()
            .map(|call| call.canonical_path.as_str())
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["/orders", "https://api.vendor.test/v1/charges"],
            "a declared-external host survives on the key; an undeclared origin \
             is still stripped so a self-call can match"
        );
    }

    /// A data call as the fifth pass sees it. `span` carries the
    /// `apply_candidate_map` stamp: `Some` when the model's `candidate_id`
    /// joined a real SWC HTTP candidate, `None` when the model reported an
    /// outbound call at a location the deterministic scanner saw no client
    /// call at (the wrapper-resolved delegating site).
    fn call_with_span(line: i32, target: &str, span: Option<u32>) -> DataCallResult {
        DataCallResult {
            call_kind: None,
            candidate_id: "span:200-260".to_string(),
            line_number: line,
            target: target.to_string(),
            method: Some("GET".to_string()),
            pattern_matched: "axios.get(".to_string(),
            call_expression_span_start: span,
            call_expression_span_end: span.map(|s| s + 40),
            call_expression_text: None,
            call_expression_line: None,
            payload_expression_text: None,
            payload_expression_line: None,
            primary_type_symbol: None,
            type_import_source: None,
            loopback_default_url: None,
            base: None,
            consumers_not_resolved: None,
        }
    }

    /// A data call with no method at all — what extraction emits for a site
    /// that only delegates (`this.client.load(id)`), because the site's own
    /// arguments carry no method.
    fn methodless_call(line: i32, target: &str, span: Option<u32>) -> DataCallResult {
        DataCallResult {
            method: None,
            pattern_matched: "client.load(".to_string(),
            ..call_with_span(line, target, span)
        }
    }

    /// carrick-cloud#386: a wrapper that hardcodes `method: "POST"` inside its
    /// own request tells the delegating site nothing, so the site's method is
    /// empty and `normalize_consumer_method` turns it into a GET. The wrapper's
    /// method is a structural fact, so it is carried to the site instead.
    ///
    /// The wrapper's OWN request call is untouched: it is candidate-backed, so
    /// its method is the one the scanner saw at a real client call.
    #[test]
    fn a_wrapper_hardcoded_method_reaches_the_delegating_site() {
        let mut result = result_with_data_calls(vec![
            methodless_call(115, "${CATALOG_API_URL}/catalog/sync", Some(400)),
            methodless_call(23, "/catalog/sync", None),
        ]);
        let shape = WrapperRequestShape {
            method: "POST".to_string(),
            has_body: Some(true),
        };

        let propagated =
            FileOrchestrator::propagate_wrapper_request_shape(&mut result, Some(&shape));

        assert_eq!(propagated, 1, "only the delegating site is rewritten");
        assert_eq!(
            result.data_calls[0].method, None,
            "the wrapper's own candidate-backed request keeps what extraction gave it"
        );
        assert_eq!(result.data_calls[1].method.as_deref(), Some("POST"));
    }

    /// With no wrapper behind the file — or a wrapper that parameterizes its
    /// method — nothing is rewritten. This is the majority of files, and the
    /// case where the delegating site's own argument IS the method.
    #[test]
    fn an_unknown_wrapper_shape_rewrites_nothing() {
        let mut result = result_with_data_calls(vec![methodless_call(23, "/catalog/sync", None)]);
        assert_eq!(
            FileOrchestrator::propagate_wrapper_request_shape(&mut result, None),
            0
        );
        assert_eq!(result.data_calls[0].method, None);
    }

    /// The method flip turns on request-body inference downstream
    /// (`should_infer_request_body`), so a wrapper that demonstrably sends no
    /// body must take the site's payload anchor with it — otherwise the site
    /// acquires a request type for a request that has no request body.
    #[test]
    fn a_bodyless_wrapper_clears_the_sites_payload_anchor() {
        let mut call = methodless_call(23, "/catalog/things/42", None);
        call.payload_expression_text = Some("{ id }".to_string());
        call.payload_expression_line = Some(23);
        let mut result = result_with_data_calls(vec![call]);

        FileOrchestrator::propagate_wrapper_request_shape(
            &mut result,
            Some(&WrapperRequestShape {
                method: "DELETE".to_string(),
                has_body: Some(false),
            }),
        );

        assert_eq!(result.data_calls[0].method.as_deref(), Some("DELETE"));
        assert_eq!(result.data_calls[0].payload_expression_text, None);
        assert_eq!(result.data_calls[0].payload_expression_line, None);
    }

    /// A wrapper whose body presence cannot be read leaves the anchor alone: a
    /// positional payload the site itself passes is a real request body.
    #[test]
    fn an_unreadable_body_presence_leaves_the_payload_anchor_alone() {
        let mut call = methodless_call(23, "/catalog/things", None);
        call.payload_expression_text = Some("payload".to_string());
        call.payload_expression_line = Some(23);
        let mut result = result_with_data_calls(vec![call]);

        FileOrchestrator::propagate_wrapper_request_shape(
            &mut result,
            Some(&WrapperRequestShape {
                method: "POST".to_string(),
                has_body: None,
            }),
        );

        assert_eq!(
            result.data_calls[0].payload_expression_text.as_deref(),
            Some("payload")
        );
    }

    /// carrick-cloud#386, second half: a wrapper-resolved site and the
    /// wrapper's own templated request are ONE outbound operation, and must
    /// key as one.
    ///
    /// They agree on the path already — the wrapper's `${CATALOG_API_URL}` base
    /// is a declared-internal env var, so `consumer_call_path` strips it to the
    /// bare route the resolved site reports. What kept them apart was the
    /// method: the delegating site defaulted to GET against the wrapper's POST,
    /// so the existing wrapper-echo suppression never saw one key. With the
    /// method propagated they collapse, and the candidate-backed record — the
    /// real client call, the only one with spans for the type sidecar — wins.
    #[test]
    fn a_resolved_wrapper_site_and_its_template_call_key_as_one_operation() {
        let orchestrator = FileOrchestrator::new(AgentService::new());
        let config = crate::config::Config {
            internal_env_vars: ["CATALOG_API_URL".to_string()].into_iter().collect(),
            ..Default::default()
        };

        let post = |line: i32, target: &str, span: Option<u32>| DataCallResult {
            method: Some("POST".to_string()),
            ..call_with_span(line, target, span)
        };

        let mut file_results = HashMap::new();
        file_results.insert(
            "src/catalog-client.ts".to_string(),
            result_with_data_calls(vec![post(
                115,
                "${CATALOG_API_URL}/catalog/sync",
                Some(400),
            )]),
        );
        file_results.insert(
            "src/tools/sync-catalog.ts".to_string(),
            result_with_data_calls(vec![post(23, "/catalog/sync", None)]),
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::new(&config),
            Path::new(""),
            Path::new(""),
        );

        assert_eq!(
            graph.data_calls.len(),
            1,
            "one physical request must key as one operation, got {:?}",
            graph
                .data_calls
                .iter()
                .map(|c| (&c.method, &c.canonical_path, &c.file_location))
                .collect::<Vec<_>>()
        );
        assert_eq!(graph.data_calls[0].method, "POST");
        assert_eq!(graph.data_calls[0].canonical_path, "/catalog/sync");
        assert_eq!(
            graph.data_calls[0].file_location, "src/catalog-client.ts:115",
            "the candidate-backed client call is the record that survives"
        );
    }

    /// The counterfactual for the test above: with the delegating site still
    /// recorded as a GET, the two records carry different keys and both
    /// survive. This is the state carrick-cloud#386 reported.
    #[test]
    fn a_delegating_site_left_as_get_does_not_collapse() {
        let orchestrator = FileOrchestrator::new(AgentService::new());
        let config = crate::config::Config {
            internal_env_vars: ["CATALOG_API_URL".to_string()].into_iter().collect(),
            ..Default::default()
        };

        let mut file_results = HashMap::new();
        file_results.insert(
            "src/catalog-client.ts".to_string(),
            result_with_data_calls(vec![DataCallResult {
                method: Some("POST".to_string()),
                ..call_with_span(115, "${CATALOG_API_URL}/catalog/sync", Some(400))
            }]),
        );
        file_results.insert(
            "src/tools/sync-catalog.ts".to_string(),
            result_with_data_calls(vec![call_with_span(23, "/catalog/sync", None)]),
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::new(&config),
            Path::new(""),
            Path::new(""),
        );

        assert_eq!(graph.data_calls.len(), 2);
    }

    /// Reproduction of the cross-service call double-count.
    ///
    /// One physical outbound request: a class-based service module holds the
    /// only `axios.get(`${ORDER_SERVICE_URL}/api/orders`)` in the repo, and a
    /// controller in another file delegates to it. The controller is analyzed
    /// with the service as wrapper context (#369/#370), so the model resolves
    /// the call through the wrapper and emits it a second time at the
    /// delegating site — with no SWC candidate behind it, because
    /// `this.usersService.fetchOrdersForUser(...)` is not a client call the
    /// scanner recognizes.
    ///
    /// Both records carry the same (method, canonical path) but different file
    /// locations, so nothing downstream collapses them: `ApiAnalysisResult`
    /// counts calls raw, and the PR comment reported one call too many.
    /// Structural, not framework-specific — any delegation to a same-repo
    /// module whose own request URL is concrete produces it.
    #[test]
    fn test_build_mount_graph_drops_wrapper_echo_of_an_extracted_call_site() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let target = "${ORDER_SERVICE_URL}/api/orders";
        let mut file_results = HashMap::new();
        // The real client call site: candidate-backed.
        file_results.insert(
            "src/users/users.service.ts".to_string(),
            result_with_data_calls(vec![call_with_span(27, target, Some(640))]),
        );
        // The delegating site, resolved through wrapper context: no candidate.
        file_results.insert(
            "src/users/users.controller.ts".to_string(),
            result_with_data_calls(vec![call_with_span(50, target, None)]),
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );

        assert_eq!(
            graph.data_calls.len(),
            1,
            "one physical call site must yield one call, got {:?}",
            graph
                .data_calls
                .iter()
                .map(|c| c.file_location.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            graph.data_calls[0].file_location, "src/users/users.service.ts:27",
            "the surviving record must be the real client call site, not the delegation"
        );
    }

    /// The suppression must be a no-op for the case #370 was built for: a
    /// wrapper whose own request URL is fully templated (`${base}${path}`) is
    /// dropped by the literal-segment gate, so the delegating site's resolved
    /// emission is the ONLY record of the call and must survive even though it
    /// has no SWC candidate behind it.
    #[test]
    fn test_build_mount_graph_keeps_wrapper_resolved_call_with_no_extracted_twin() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mut file_results = HashMap::new();
        file_results.insert(
            "src/http/client.ts".to_string(),
            result_with_data_calls(vec![call_with_span(8, "${API_BASE}${path}", Some(120))]),
        );
        file_results.insert(
            "src/orders/orders.repository.ts".to_string(),
            result_with_data_calls(vec![call_with_span(33, "${API_BASE}/api/orders", None)]),
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );

        assert_eq!(graph.data_calls.len(), 1);
        assert_eq!(
            graph.data_calls[0].file_location,
            "src/orders/orders.repository.ts:33"
        );
    }

    /// Two genuinely distinct client call sites to the same operation are two
    /// consumers, not a duplicate: both are candidate-backed, so neither is an
    /// echo and both survive. Guards the fix against collapsing into a blanket
    /// repo-wide dedup on (method, canonical path).
    #[test]
    fn test_build_mount_graph_keeps_two_real_call_sites_to_the_same_operation() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let target = "${ORDER_SERVICE_URL}/api/orders";
        let mut file_results = HashMap::new();
        file_results.insert(
            "src/reports/nightly.ts".to_string(),
            result_with_data_calls(vec![call_with_span(11, target, Some(300))]),
        );
        file_results.insert(
            "src/orders/sync.ts".to_string(),
            result_with_data_calls(vec![call_with_span(74, target, Some(900))]),
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );

        assert_eq!(graph.data_calls.len(), 2);
    }

    /// Pre-fix-failing case for carrick#399 (eval run 29677844107): the model
    /// rendered the audit call target verbatim with its inline fallback, the
    /// whitespace inside the interpolation failed `is_valid_route_shape`, and
    /// the fifth-pass gate silently dropped the call. After the fold-time
    /// collapse, the call survives with the same canonical key the clean
    /// `${AUDIT_WEBHOOK_URL}/audit/events` rendering produces.
    #[test]
    fn test_fallback_target_survives_graph_build_with_clean_canonical_key() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let make_result = |target: &str| FileAnalysisResult {
            data_calls: vec![DataCallResult {
                call_kind: None,
                candidate_id: "span:1002-1107".to_string(),
                line_number: 42,
                target: target.to_string(),
                method: Some("POST".to_string()),
                pattern_matched: "axios.post(".to_string(),
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
            }],
            ..Default::default()
        };

        let config = Config {
            internal_env_vars: ["AUDIT_WEBHOOK_URL"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            ..Config::default()
        };
        let normalizer = UrlNormalizer::new(&config);

        let build = |result: FileAnalysisResult| {
            let mut file_results = HashMap::new();
            file_results.insert("lib/audit.ts".to_string(), result);
            orchestrator.build_mount_graph(&file_results, &normalizer, Path::new(""), Path::new(""))
        };

        // The verbatim rendering, run through the fold-time normalization the
        // orchestrator applies to every LLM result.
        let mut verbatim =
            make_result(r#"${AUDIT_WEBHOOK_URL ?? "http://localhost:3099"}/audit/events"#);
        FileOrchestrator::normalize_fallback_targets(&mut verbatim);
        let graph = build(verbatim);
        assert_eq!(
            graph.data_calls.len(),
            1,
            "normalized fallback target must survive the route-shape gate"
        );
        assert_eq!(
            graph.data_calls[0].target_url,
            "${AUDIT_WEBHOOK_URL}/audit/events"
        );
        assert_eq!(graph.data_calls[0].canonical_path, "/audit/events");

        // The clean rendering keys identically (fallback vs clean is the only
        // difference between the hit and miss eval runs).
        let clean = build(make_result("${AUDIT_WEBHOOK_URL}/audit/events"));
        assert_eq!(
            clean.data_calls[0].canonical_path,
            graph.data_calls[0].canonical_path
        );
        assert_eq!(
            clean.data_calls[0].target_url,
            graph.data_calls[0].target_url
        );

        // A non-fallback whitespace target stays dropped: normalization does
        // not touch it and the guard still rejects it.
        let mut junk = make_result("${base + path}/audit/events");
        FileOrchestrator::normalize_fallback_targets(&mut junk);
        let graph = build(junk);
        assert!(graph.data_calls.is_empty());
    }

    /// A data call to the service's OWN endpoint (same mount graph) is a
    /// self-call — the service hitting its own HTTP surface — and must be
    /// dropped so it neither leaks as an indexed consumer op nor forms a
    /// spurious self producer↔consumer edge. The literal `http://localhost:PORT`
    /// origin is stripped to a bare param path (`consumer_call_path`), which then
    /// matches the declared endpoint param-aware (`:wid` ≡ `:warehouseId`). A
    /// call to a DIFFERENT path survives. Fails before the origin-strip +
    /// suppression pass: both calls' keys keep the raw `http://host:port/...`
    /// origin, so nothing matches the endpoint and both survive (len == 2).
    #[test]
    fn test_build_mount_graph_suppresses_self_calls() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mk_call = |id: &str, target: &str| DataCallResult {
            call_kind: None,
            candidate_id: id.to_string(),
            line_number: 7,
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
        };

        let mut file_results = HashMap::new();
        file_results.insert(
            "src/app.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![EndpointResult {
                    candidate_id: "span:1-40".to_string(),
                    line_number: 5,
                    owner_node: "app".to_string(),
                    method: "GET".to_string(),
                    path: "/warehouses/:warehouseId/stock/:sku".to_string(),
                    handler_name: "getStock".to_string(),
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
                }],
                data_calls: vec![
                    // Self-call to the service's own endpoint over localhost.
                    mk_call(
                        "span:100-160",
                        "http://localhost:4002/warehouses/${wid}/stock/${sku}",
                    ),
                    // Genuine outbound call to a different path — survives.
                    mk_call("span:200-260", "http://localhost:9000/catalog/${id}"),
                ],
                graphql_operations: vec![],
                pubsub_operations: vec![],
            },
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );

        let call_paths: Vec<&str> = graph
            .data_calls
            .iter()
            .map(|c| c.canonical_path.as_str())
            .collect();
        assert_eq!(
            graph.data_calls.len(),
            1,
            "self-call must be suppressed, surviving calls: {call_paths:?}"
        );
        assert_eq!(graph.data_calls[0].canonical_path, "/catalog/:id");
    }

    /// #379: an "endpoint" whose exact source site (file:line), method, and
    /// path were ALSO extracted as a data call is a double-extracted client
    /// call expression, not a route definition. It must be reclassified to
    /// call-site evidence, and its twin call must SURVIVE self-call
    /// suppression (the call targets an external contract, not the service's
    /// own surface). A genuine route definition in the same file keeps
    /// route-definition evidence, and a call matching THAT one is still
    /// suppressed as a self-call.
    #[test]
    fn test_build_mount_graph_reclassifies_double_extracted_call_as_call_site_evidence() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mk_endpoint = |line: u32, method: &str, path: &str| EndpointResult {
            candidate_id: format!("span:{line}"),
            line_number: line as i32,
            owner_node: "app".to_string(),
            method: method.to_string(),
            path: path.to_string(),
            handler_name: "handler".to_string(),
            pattern_matched: ".request(".to_string(),
            call_expression_span_start: None,
            call_expression_span_end: None,
            payload_expression_text: None,
            payload_expression_line: None,
            response_expression_text: None,
            response_expression_line: None,
            emission_style: None,
            primary_type_symbol: None,
            type_import_source: None,
        };
        let mk_call = |line: u32, method: &str, target: &str| DataCallResult {
            call_kind: None,
            candidate_id: format!("call:{line}"),
            line_number: line as i32,
            target: target.to_string(),
            method: Some(method.to_string()),
            pattern_matched: "request(".to_string(),
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
        };

        let mut file_results = HashMap::new();
        file_results.insert(
            "operations/create-widget.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![
                    // Double-extracted client call: same line, verb, and path
                    // as the data call below → call-site evidence.
                    mk_endpoint(14, "POST", "/v2/widgets"),
                    // Genuine route definition at a different site.
                    mk_endpoint(40, "GET", "/health"),
                ],
                data_calls: vec![
                    mk_call(14, "POST", "/v2/widgets"),
                    // Self-call to the genuine route: still suppressed.
                    mk_call(52, "GET", "/health"),
                ],
                graphql_operations: vec![],
                pubsub_operations: vec![],
            },
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );

        let evidence_of = |method: &str| {
            graph
                .endpoints
                .iter()
                .find(|e| e.method == method)
                .expect("endpoint present")
                .evidence
        };
        assert_eq!(
            evidence_of("POST"),
            carrick_match::MatchEvidence::CallSite,
            "double-extracted call expression must be call-site evidence"
        );
        assert_eq!(
            evidence_of("GET"),
            carrick_match::MatchEvidence::RouteDefinition,
            "a genuine route definition keeps route-definition evidence"
        );

        // The twin call survives (external contract encoding); the genuine
        // self-call is still suppressed.
        let surviving: Vec<&str> = graph
            .data_calls
            .iter()
            .map(|c| c.canonical_path.as_str())
            .collect();
        assert_eq!(
            surviving,
            vec!["/v2/widgets"],
            "twin call must survive; self-call to the real route must not"
        );
    }

    /// #307 (class 1): a wrapper-internal call whose canonical path is nothing
    /// but template interpolations (`${baseUrl}${path}`, a bare `${GQL_URL}`)
    /// can never match a producer key and must not enter the graph; a call
    /// with any literal segment stays.
    #[test]
    fn test_build_mount_graph_drops_calls_with_no_literal_path_segment() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mk_call = |id: &str, target: &str, method: &str| DataCallResult {
            call_kind: None,
            candidate_id: id.to_string(),
            line_number: 6,
            target: target.to_string(),
            method: Some(method.to_string()),
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
        };
        let mut file_results = HashMap::new();
        file_results.insert(
            "src/lib/apiClient.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![],
                data_calls: vec![
                    mk_call("c1", "${baseUrl}${path}", "GET"),
                    mk_call("c2", "${NEXT_PUBLIC_GATEWAY_GQL_URL}", "POST"),
                    mk_call("c3", "/orders/${orderId}/timeline", "GET"),
                ],
                graphql_operations: vec![],
                pubsub_operations: vec![],
            },
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );

        let targets: Vec<&str> = graph
            .data_calls
            .iter()
            .map(|c| c.target_url.as_str())
            .collect();
        assert_eq!(
            targets,
            vec!["/orders/${orderId}/timeline"],
            "fully-templated targets must be dropped, literal-segment targets kept"
        );
    }

    #[test]
    fn test_collect_type_requests_skips_non_url_data_calls() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mut file_results = HashMap::new();
        file_results.insert(
            "src/service.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![],
                data_calls: vec![
                    DataCallResult {
                        call_kind: None,
                        candidate_id: "span:300-340".to_string(),
                        line_number: 12,
                        target: "ordersResp".to_string(),
                        method: Some("GET".to_string()),
                        pattern_matched: "resp.json()".to_string(),
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
                    },
                    DataCallResult {
                        call_kind: None,
                        candidate_id: "span:350-400".to_string(),
                        line_number: 15,
                        target: "https://api.example.com/data".to_string(),
                        method: Some("GET".to_string()),
                        pattern_matched: "fetch(".to_string(),
                        call_expression_span_start: Some(350),
                        call_expression_span_end: Some(400),
                        call_expression_text: None,
                        call_expression_line: None,
                        payload_expression_text: None,
                        payload_expression_line: None,
                        primary_type_symbol: None,
                        type_import_source: None,

                        loopback_default_url: None,
                        base: None,
                        consumers_not_resolved: None,
                    },
                ],
                graphql_operations: vec![],
                pubsub_operations: vec![],
            },
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );
        let config = Config::default();
        let (_explicit, infer, _inline) =
            orchestrator.collect_type_requests(&file_results, ".", &graph, &config);

        assert_eq!(infer.len(), 1);
    }

    #[test]
    fn test_collect_type_requests_skips_non_http_methods() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mut file_results = HashMap::new();
        file_results.insert(
            "src/service.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![],
                data_calls: vec![DataCallResult {
                    call_kind: None,
                    candidate_id: "span:410-460".to_string(),
                    line_number: 12,
                    target: "https://api.example.com/data".to_string(),
                    method: Some(".json()".to_string()),
                    pattern_matched: "resp.json()".to_string(),
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
                }],
                graphql_operations: vec![],
                pubsub_operations: vec![],
            },
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );
        let config = Config::default();
        let (explicit, infer, inline) =
            orchestrator.collect_type_requests(&file_results, ".", &graph, &config);

        assert!(explicit.is_empty());
        assert!(infer.is_empty());
        assert!(inline.is_empty());
    }

    #[test]
    fn test_collect_type_requests_assigns_call_ids() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mut file_results = HashMap::new();
        file_results.insert(
            "src/service.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![],
                data_calls: vec![
                    DataCallResult {
                        call_kind: None,
                        candidate_id: "span:470-520".to_string(),
                        line_number: 10,
                        target: "https://api.example.com/orders".to_string(),
                        method: Some("GET".to_string()),
                        pattern_matched: "fetch(".to_string(),
                        call_expression_span_start: Some(470),
                        call_expression_span_end: Some(520),
                        call_expression_text: None,
                        call_expression_line: None,
                        payload_expression_text: None,
                        payload_expression_line: None,
                        primary_type_symbol: None,
                        type_import_source: None,

                        loopback_default_url: None,
                        base: None,
                        consumers_not_resolved: None,
                    },
                    DataCallResult {
                        call_kind: None,
                        candidate_id: "span:530-580".to_string(),
                        line_number: 20,
                        target: "https://api.example.com/orders".to_string(),
                        method: Some("GET".to_string()),
                        pattern_matched: "fetch(".to_string(),
                        call_expression_span_start: Some(530),
                        call_expression_span_end: Some(580),
                        call_expression_text: None,
                        call_expression_line: None,
                        payload_expression_text: None,
                        payload_expression_line: None,
                        primary_type_symbol: None,
                        type_import_source: None,

                        loopback_default_url: None,
                        base: None,
                        consumers_not_resolved: None,
                    },
                ],
                graphql_operations: vec![],
                pubsub_operations: vec![],
            },
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );
        let config = Config::default();
        let (_explicit, infer, _inline) =
            orchestrator.collect_type_requests(&file_results, ".", &graph, &config);

        let mut aliases: Vec<String> = infer.into_iter().filter_map(|item| item.alias).collect();
        aliases.sort();

        assert_eq!(aliases.len(), 2);
        assert!(aliases[0].contains("_Call"));
        assert!(aliases[1].contains("_Call"));
        assert_ne!(aliases[0], aliases[1]);
    }

    #[test]
    fn test_collect_type_requests_file_based_route_uses_function_return() {
        // A file-based route endpoint (sentinel owner) carries a handler span but
        // no call-site payload expression. Its response type must be requested as
        // a line-anchored FunctionReturn (the handler's return type), NOT a
        // span/text ResponseBody — which would misread the function declaration.
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mut file_results = HashMap::new();
        file_results.insert(
            "app/users/route.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![EndpointResult {
                    candidate_id: "file-route:GET:42".to_string(),
                    line_number: 7,
                    owner_node: FILE_BASED_ROUTE_OWNER.to_string(),
                    method: "GET".to_string(),
                    path: "/users".to_string(),
                    handler_name: "GET".to_string(),
                    pattern_matched: "nextjs-app".to_string(),
                    // Span points at the whole handler declaration — the landmine
                    // the old code would have fed to the response-body locator.
                    call_expression_span_start: Some(42),
                    call_expression_span_end: Some(300),
                    payload_expression_text: None,
                    payload_expression_line: None,
                    response_expression_text: None,
                    response_expression_line: None,
                    emission_style: None,
                    primary_type_symbol: None,
                    type_import_source: None,
                }],
                data_calls: vec![],
                graphql_operations: vec![],
                pubsub_operations: vec![],
            },
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );
        let config = Config::default();
        let (_explicit, infer, _inline) =
            orchestrator.collect_type_requests(&file_results, ".", &graph, &config);

        // Exactly one inference: the response. No request-body inference for a
        // file-based GET (and none even for POST — not recoverable from the sig).
        assert_eq!(infer.len(), 1);
        let item = &infer[0];
        assert_eq!(item.infer_kind, InferKind::FunctionReturn);
        assert_eq!(item.line_number, 7);
        // Line-only locator: no span, no text — so the sidecar uses findFunctionByLine
        // and can't misresolve the declaration span as a payload.
        assert!(item.span_start.is_none());
        assert!(item.span_end.is_none());
        assert!(item.expression_text.is_none());
        let alias = item.alias.as_deref().unwrap_or_default();
        assert!(alias.contains("Response"), "alias was {alias}");
    }

    /// Build a call-site endpoint with the given emission style. Carries both
    /// a response expression and SWC spans so the test proves the routing
    /// decision comes from `emission_style`, not from locator availability.
    fn endpoint_with_emission_style(
        method: &str,
        path: &str,
        emission_style: Option<EmissionStyle>,
    ) -> EndpointResult {
        EndpointResult {
            candidate_id: "span:100-200".to_string(),
            line_number: 12,
            owner_node: "app".to_string(),
            method: method.to_string(),
            path: path.to_string(),
            handler_name: "anonymous".to_string(),
            pattern_matched: ".get(".to_string(),
            call_expression_span_start: Some(100),
            call_expression_span_end: Some(200),
            payload_expression_text: None,
            payload_expression_line: None,
            response_expression_text: Some("users".to_string()),
            response_expression_line: Some(13),
            emission_style,
            primary_type_symbol: None,
            type_import_source: None,
        }
    }

    type CollectedRequests = (
        Vec<SymbolRequest>,
        Vec<InferRequestItem>,
        Vec<(String, String)>,
    );

    fn collect_for_endpoint(endpoint: EndpointResult) -> CollectedRequests {
        let orchestrator = FileOrchestrator::new(AgentService::new());
        let mut file_results = HashMap::new();
        file_results.insert(
            "src/server.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![endpoint],
                data_calls: vec![],
                graphql_operations: vec![],
                pubsub_operations: vec![],
            },
        );
        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );
        let config = Config::default();
        orchestrator.collect_type_requests(&file_results, ".", &graph, &config)
    }

    fn infer_for_endpoint(endpoint: EndpointResult) -> Vec<InferRequestItem> {
        collect_for_endpoint(endpoint).1
    }

    #[test]
    fn test_collect_type_requests_return_value_uses_function_return_via_text() {
        // Fastify return-style: the handler's return value IS the payload.
        // With a response expression available, the request must be a
        // text-located FunctionReturn — the sidecar resolves the expression's
        // containing function, which finds the exact handler even when it's a
        // named reference declared far from the registration line.
        let infer = infer_for_endpoint(endpoint_with_emission_style(
            "GET",
            "/users",
            Some(EmissionStyle::ReturnValue),
        ));

        assert_eq!(infer.len(), 1);
        let item = &infer[0];
        assert_eq!(item.infer_kind, InferKind::FunctionReturn);
        assert_eq!(item.line_number, 12);
        assert_eq!(item.expression_text.as_deref(), Some("users"));
        assert_eq!(item.expression_line, Some(13));
        assert!(
            item.span_start.is_none() && item.span_end.is_none(),
            "the call-expression span must not be sent — the registration call \
             contains the handler, so span resolution would bind the wrong function: {:?}",
            item
        );
    }

    #[test]
    fn test_collect_type_requests_return_value_falls_back_to_line_anchor() {
        // Pairing-invariant violation (return-value but no expression): fall
        // back to anchoring on the registration line, which is correct for
        // inline handlers (their function starts on that line).
        let mut endpoint =
            endpoint_with_emission_style("GET", "/users", Some(EmissionStyle::ReturnValue));
        endpoint.response_expression_text = None;
        endpoint.response_expression_line = None;

        let infer = infer_for_endpoint(endpoint);
        assert_eq!(infer.len(), 1);
        let item = &infer[0];
        assert_eq!(item.infer_kind, InferKind::FunctionReturn);
        assert_eq!(item.line_number, 12);
        assert!(
            item.span_start.is_none() && item.span_end.is_none() && item.expression_text.is_none(),
            "fallback must be line-anchored only: {:?}",
            item
        );
    }

    #[test]
    fn test_collect_type_requests_no_payload_skips_response_inference() {
        // Zero-arg sends / helper-written payloads: no recoverable payload
        // expression exists, so no inference is requested at all — the
        // manifest entry stays honestly `unknown` with its evidence. The
        // spans on the endpoint are the landmine: without the emission_style
        // gate the span fallback would infer from the whole `app.get(...)`
        // call expression.
        let mut endpoint =
            endpoint_with_emission_style("GET", "/export", Some(EmissionStyle::NoPayload));
        // Pairing invariant: no-payload ⇒ response expression is null.
        endpoint.response_expression_text = None;
        endpoint.response_expression_line = None;

        let infer = infer_for_endpoint(endpoint);
        assert!(
            infer.is_empty(),
            "no-payload endpoints must not request response inference: {:?}",
            infer
        );
    }

    #[test]
    fn test_collect_type_requests_no_payload_skips_explicit_symbol_too() {
        // A no-payload endpoint may still carry a (validated) type hint the
        // model picked up from imports — but the handler never sends it, so
        // bundling it would publish a phantom response contract. Both the
        // explicit-symbol path and the inline-alias fallback must be gated,
        // not just inference.
        let mut endpoint =
            endpoint_with_emission_style("GET", "/export", Some(EmissionStyle::NoPayload));
        endpoint.response_expression_text = None;
        endpoint.response_expression_line = None;
        endpoint.primary_type_symbol = Some("User".to_string());
        endpoint.type_import_source = Some("./types".to_string());

        let (explicit, infer, inline) = collect_for_endpoint(endpoint);
        assert!(
            explicit.is_empty(),
            "no-payload endpoints must not bundle explicit response symbols: {:?}",
            explicit
        );
        assert!(infer.is_empty(), "got: {:?}", infer);
        assert!(inline.is_empty(), "got: {:?}", inline);
    }

    #[test]
    fn test_collect_type_requests_no_payload_keeps_request_body_inference() {
        // The classification is about the RESPONSE payload; request-body
        // inference for mutating methods is unaffected.
        let mut endpoint =
            endpoint_with_emission_style("POST", "/orders", Some(EmissionStyle::NoPayload));
        endpoint.response_expression_text = None;
        endpoint.response_expression_line = None;
        endpoint.payload_expression_text = Some("req.body".to_string());
        endpoint.payload_expression_line = Some(13);

        let infer = infer_for_endpoint(endpoint);
        assert_eq!(infer.len(), 1, "got: {:?}", infer);
        assert_eq!(infer[0].infer_kind, InferKind::RequestBody);
        assert_eq!(infer[0].expression_text.as_deref(), Some("req.body"));
    }

    #[test]
    fn test_collect_type_requests_imperative_send_matches_legacy_default() {
        // Explicit imperative-send and an absent emission_style (cached
        // pre-emission-style analysis) must produce the identical request:
        // text-located ResponseBody.
        for style in [Some(EmissionStyle::ImperativeSend), None] {
            let infer = infer_for_endpoint(endpoint_with_emission_style("GET", "/users", style));
            assert_eq!(infer.len(), 1, "style {:?} got: {:?}", style, infer);
            let item = &infer[0];
            assert_eq!(item.infer_kind, InferKind::ResponseBody);
            assert_eq!(item.expression_text.as_deref(), Some("users"));
            assert_eq!(item.expression_line, Some(13));
        }
    }

    #[test]
    fn test_validate_type_hints_rejects_invalid_symbols() {
        let mut result = FileAnalysisResult {
            graphql_consumer_locates: vec![],
            mounts: vec![],
            endpoints: vec![
                EndpointResult {
                    candidate_id: "span:590-650".to_string(),
                    line_number: 10,
                    owner_node: "app".to_string(),
                    method: "GET".to_string(),
                    path: "/users".to_string(),
                    handler_name: "handler".to_string(),
                    pattern_matched: "app.get".to_string(),
                    call_expression_span_start: None,
                    call_expression_span_end: None,
                    payload_expression_text: None,
                    payload_expression_line: None,
                    response_expression_text: None,
                    response_expression_line: None,
                    emission_style: None,
                    primary_type_symbol: Some("User".to_string()),
                    type_import_source: Some("react".to_string()),
                },
                EndpointResult {
                    candidate_id: "span:700-740".to_string(),
                    line_number: 12,
                    owner_node: "app".to_string(),
                    method: "GET".to_string(),
                    path: "/models".to_string(),
                    handler_name: "handler".to_string(),
                    pattern_matched: "app.get".to_string(),
                    call_expression_span_start: None,
                    call_expression_span_end: None,
                    payload_expression_text: None,
                    payload_expression_line: None,
                    response_expression_text: None,
                    response_expression_line: None,
                    emission_style: None,
                    primary_type_symbol: Some("Models.User".to_string()),
                    type_import_source: Some("./models".to_string()),
                },
            ],
            data_calls: vec![DataCallResult {
                call_kind: None,
                candidate_id: "span:660-700".to_string(),
                line_number: 12,
                target: "/users".to_string(),
                method: Some("GET".to_string()),
                pattern_matched: "fetch(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                call_expression_text: None,
                call_expression_line: None,
                payload_expression_text: None,
                payload_expression_line: None,
                primary_type_symbol: Some("LocalType".to_string()),
                type_import_source: None,

                loopback_default_url: None,
                base: None,
                consumers_not_resolved: None,
            }],
            graphql_operations: vec![],
            pubsub_operations: vec![],
        };

        let mut imported_symbols = HashMap::new();
        imported_symbols.insert(
            "User".to_string(),
            ImportedSymbol {
                local_name: "User".to_string(),
                imported_name: "User".to_string(),
                source: "./repo-a_types".to_string(),
                kind: SymbolKind::Named,
            },
        );
        imported_symbols.insert(
            "Models".to_string(),
            ImportedSymbol {
                local_name: "Models".to_string(),
                imported_name: "Models".to_string(),
                source: "./models".to_string(),
                kind: SymbolKind::Namespace,
            },
        );

        let symbol_table = SymbolTable {
            local_types: HashSet::from(["LocalType".to_string()]),
            imported_symbols,
        };

        FileOrchestrator::validate_type_hints(&mut result, &symbol_table);

        let invalid_endpoint = &result.endpoints[0];
        assert!(invalid_endpoint.primary_type_symbol.is_none());
        assert!(invalid_endpoint.type_import_source.is_none());

        let namespace_endpoint = &result.endpoints[1];
        assert_eq!(
            namespace_endpoint.primary_type_symbol.as_deref(),
            Some("Models.User")
        );
        assert_eq!(
            namespace_endpoint.type_import_source.as_deref(),
            Some("./models")
        );

        let data_call = &result.data_calls[0];
        assert_eq!(data_call.primary_type_symbol.as_deref(), Some("LocalType"));
        assert!(data_call.type_import_source.is_none());
    }

    #[test]
    fn test_validate_type_hints_strips_spurious_source_extension() {
        use crate::agents::file_analyzer_agent::PubsubOperation;
        use crate::operation::PubsubRole;

        let mut result = FileAnalysisResult {
            graphql_consumer_locates: vec![],
            mounts: vec![],
            endpoints: vec![EndpointResult {
                candidate_id: "span:1-2".to_string(),
                line_number: 10,
                owner_node: "app".to_string(),
                method: "GET".to_string(),
                path: "/orders".to_string(),
                handler_name: "handler".to_string(),
                pattern_matched: "app.get".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                payload_expression_text: None,
                payload_expression_line: None,
                response_expression_text: None,
                response_expression_line: None,
                emission_style: None,
                // `.ts` appended by the model; the extension-less form is imported.
                primary_type_symbol: Some("OrderPlacedEvent".to_string()),
                type_import_source: Some("../types/events.ts".to_string()),
            }],
            data_calls: vec![DataCallResult {
                call_kind: None,
                candidate_id: "span:3-4".to_string(),
                line_number: 12,
                target: "/x".to_string(),
                method: Some("GET".to_string()),
                pattern_matched: "fetch(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                call_expression_text: None,
                call_expression_line: None,
                payload_expression_text: None,
                payload_expression_line: None,
                primary_type_symbol: Some("OrderPlacedEvent".to_string()),
                type_import_source: Some("../types/events.tsx".to_string()),

                loopback_default_url: None,
                base: None,
                consumers_not_resolved: None,
            }],
            graphql_operations: vec![],
            pubsub_operations: vec![
                // (a) spurious `.ts` that matches the import table -> stripped + kept.
                PubsubOperation {
                    topic: "order.placed".to_string(),
                    role: Some(PubsubRole::Subscriber),
                    line_number: 20,
                    primary_type_symbol: Some("OrderPlacedEvent".to_string()),
                    type_import_source: Some("../types/events.ts".to_string()),
                    broker: None,
                    payload_expression_text: None,
                    payload_expression_line: None,
                },
                // (b) scoped-package source (no extension) -> untouched (canary safety).
                PubsubOperation {
                    topic: "AccountsController:selectedAccountChange".to_string(),
                    role: Some(PubsubRole::Subscriber),
                    line_number: 30,
                    primary_type_symbol: Some("InternalAccount".to_string()),
                    type_import_source: Some("@metamask/keyring-internal-api".to_string()),
                    broker: None,
                    payload_expression_text: None,
                    payload_expression_line: None,
                },
                // (c) `.ts` whose stripped form does not match the import-table
                //     entry -> extension normalization leaves it alone, then the
                //     pub/sub AST source-overwrite corrects it to the import
                //     table's source (the symbol itself passes the check).
                PubsubOperation {
                    topic: "other".to_string(),
                    role: Some(PubsubRole::Publisher),
                    line_number: 40,
                    primary_type_symbol: Some("OrderPlacedEvent".to_string()),
                    type_import_source: Some("../wrong/path.ts".to_string()),
                    broker: None,
                    payload_expression_text: None,
                    payload_expression_line: None,
                },
            ],
        };

        let mut imported_symbols = HashMap::new();
        imported_symbols.insert(
            "OrderPlacedEvent".to_string(),
            ImportedSymbol {
                local_name: "OrderPlacedEvent".to_string(),
                imported_name: "OrderPlacedEvent".to_string(),
                source: "../types/events".to_string(),
                kind: SymbolKind::Named,
            },
        );
        imported_symbols.insert(
            "InternalAccount".to_string(),
            ImportedSymbol {
                local_name: "InternalAccount".to_string(),
                imported_name: "InternalAccount".to_string(),
                source: "@metamask/keyring-internal-api".to_string(),
                kind: SymbolKind::Named,
            },
        );

        let symbol_table = SymbolTable {
            local_types: HashSet::new(),
            imported_symbols,
        };

        FileOrchestrator::validate_type_hints(&mut result, &symbol_table);

        // endpoint + data_call: extension stripped, hint kept (not nulled).
        assert_eq!(
            result.endpoints[0].primary_type_symbol.as_deref(),
            Some("OrderPlacedEvent")
        );
        assert_eq!(
            result.endpoints[0].type_import_source.as_deref(),
            Some("../types/events")
        );
        assert_eq!(
            result.data_calls[0].type_import_source.as_deref(),
            Some("../types/events")
        );

        // pubsub (a) stripped; (b) scoped package untouched; (c) no match, unchanged.
        assert_eq!(
            result.pubsub_operations[0].type_import_source.as_deref(),
            Some("../types/events")
        );
        assert_eq!(
            result.pubsub_operations[1].type_import_source.as_deref(),
            Some("@metamask/keyring-internal-api")
        );
        assert_eq!(
            result.pubsub_operations[2].primary_type_symbol.as_deref(),
            Some("OrderPlacedEvent")
        );
        assert_eq!(
            result.pubsub_operations[2].type_import_source.as_deref(),
            Some("../types/events")
        );
    }

    fn pubsub_op(
        topic: &str,
        role: crate::operation::PubsubRole,
        symbol: Option<&str>,
        source: Option<&str>,
        locator: Option<&str>,
    ) -> crate::agents::file_analyzer_agent::PubsubOperation {
        crate::agents::file_analyzer_agent::PubsubOperation {
            topic: topic.to_string(),
            role: Some(role),
            line_number: 20,
            primary_type_symbol: symbol.map(str::to_string),
            type_import_source: source.map(str::to_string),
            broker: None,
            payload_expression_text: locator.map(str::to_string),
            payload_expression_line: locator.map(|_| 21),
        }
    }

    fn pubsub_only_result(
        ops: Vec<crate::agents::file_analyzer_agent::PubsubOperation>,
    ) -> FileAnalysisResult {
        FileAnalysisResult {
            graphql_consumer_locates: vec![],
            mounts: vec![],
            endpoints: vec![],
            data_calls: vec![],
            graphql_operations: vec![],
            pubsub_operations: ops,
        }
    }

    /// (a) A pub/sub symbol that fails the AST check while a usable
    /// (non-envelope) payload locator exists is demoted, and the demoted op
    /// then flows through `collect_pubsub_infer_requests` to an
    /// `InferRequestItem` — the end-to-end rescue path for the wrong-symbol
    /// borrow class.
    #[test]
    fn test_validate_pubsub_type_hints_demotes_wrong_symbol_into_infer_path() {
        use crate::operation::PubsubRole;

        let mut result = pubsub_only_result(vec![
            pubsub_op(
                "order.placed",
                PubsubRole::Publisher,
                Some("BorrowedDecoy"),
                Some("./decoys"),
                Some("payload"),
            ),
            // Model emitted a source with no symbol: cleared for hygiene
            // (schema invariant: source is null whenever the symbol is null).
            pubsub_op(
                "order.cancelled",
                PubsubRole::Publisher,
                None,
                Some("./ghost"),
                None,
            ),
        ]);
        let symbol_table = SymbolTable {
            local_types: HashSet::new(),
            imported_symbols: HashMap::new(),
        };

        FileOrchestrator::validate_type_hints(&mut result, &symbol_table);

        let op = &result.pubsub_operations[0];
        assert!(op.primary_type_symbol.is_none(), "symbol must be demoted");
        assert!(op.type_import_source.is_none(), "source must be demoted");
        assert!(
            result.pubsub_operations[1].type_import_source.is_none(),
            "a dangling source with no symbol must be cleared"
        );

        // The demoted op must fall through to the infer path.
        let orchestrator = FileOrchestrator::new(AgentService::new());
        let mut file_results = HashMap::new();
        file_results.insert("src/pub.ts".to_string(), result);
        let requests = orchestrator.collect_pubsub_infer_requests(&file_results, ".");
        assert_eq!(requests.len(), 1, "demoted op must emit an infer request");
        let item = &requests[0];
        assert_eq!(item.infer_kind, InferKind::Expression);
        assert_eq!(item.expression_text.as_deref(), Some("payload"));
        assert_eq!(item.expression_line, Some(21));
    }

    /// (b) A symbol naming a class or enum declared in the same file passes
    /// the AST check and is KEPT — through the real `extract_symbol_table`
    /// path, so this pins the `TypeSymbolExtractor` class/enum extension the
    /// demote depends on.
    #[test]
    fn test_validate_pubsub_type_hints_keeps_local_class_and_enum_symbols() {
        use crate::operation::PubsubRole;

        let tmp = tempfile::tempdir().expect("tempdir");
        let file_path = tmp.path().join("events.ts");
        std::fs::write(
            &file_path,
            "export class OrderPlacedEvent { id: string; }\n\
             export enum OrderStatus { Placed, Shipped }\n",
        )
        .expect("write file");
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, true, false, Some(cm.clone()));
        let symbol_table = FileOrchestrator::extract_symbol_table(&file_path, &cm, &handler).table;
        assert!(
            symbol_table.local_types.contains("OrderPlacedEvent"),
            "class declarations must be collected as local types"
        );
        assert!(
            symbol_table.local_types.contains("OrderStatus"),
            "enum declarations must be collected as local types"
        );

        let mut result = pubsub_only_result(vec![
            pubsub_op(
                "order.placed",
                PubsubRole::Publisher,
                Some("OrderPlacedEvent"),
                // Model hallucinated an import source for a local class: the
                // AST overwrite corrects it to None instead of demoting.
                Some("./events"),
                Some("event"),
            ),
            pubsub_op(
                "order.status",
                PubsubRole::Subscriber,
                Some("OrderStatus"),
                None,
                Some("status"),
            ),
        ]);

        FileOrchestrator::validate_type_hints(&mut result, &symbol_table);

        assert_eq!(
            result.pubsub_operations[0].primary_type_symbol.as_deref(),
            Some("OrderPlacedEvent"),
            "local class symbol must be kept"
        );
        assert!(
            result.pubsub_operations[0].type_import_source.is_none(),
            "local class symbol's source must be overwritten to None"
        );
        assert_eq!(
            result.pubsub_operations[1].primary_type_symbol.as_deref(),
            Some("OrderStatus"),
            "local enum symbol must be kept"
        );
        assert!(result.pubsub_operations[1].type_import_source.is_none());
    }

    /// (c) A failing symbol whose locator contains the op's own topic (the
    /// envelope case `collect_pubsub_infer_requests` would drop) is KEPT, not
    /// demoted: demoting would strand the op with no anchor at all. Same for
    /// an op with no locator (the recall floor).
    #[test]
    fn test_validate_pubsub_type_hints_keeps_symbol_when_demote_target_unusable() {
        use crate::operation::PubsubRole;

        let mut result = pubsub_only_result(vec![
            pubsub_op(
                "order.placed",
                PubsubRole::Publisher,
                Some("WrongSymbol"),
                Some("./somewhere"),
                // Envelope copy: contains the topic literal, so the infer
                // path's envelope guard would drop it.
                Some("{ topic: 'order.placed', data }"),
            ),
            pubsub_op(
                "order.shipped",
                PubsubRole::Subscriber,
                Some("AnotherWrongSymbol"),
                None,
                None,
            ),
        ]);
        let symbol_table = SymbolTable {
            local_types: HashSet::new(),
            imported_symbols: HashMap::new(),
        };

        FileOrchestrator::validate_type_hints(&mut result, &symbol_table);

        assert_eq!(
            result.pubsub_operations[0].primary_type_symbol.as_deref(),
            Some("WrongSymbol"),
            "envelope-locator op must keep its symbol"
        );
        assert_eq!(
            result.pubsub_operations[0].type_import_source.as_deref(),
            Some("./somewhere")
        );
        assert_eq!(
            result.pubsub_operations[1].primary_type_symbol.as_deref(),
            Some("AnotherWrongSymbol"),
            "locator-less op must keep its symbol as the recall floor"
        );
    }

    /// (d) Regression pin: HTTP ops must NOT inherit the pub/sub AST
    /// source-overwrite. An imported symbol with a mismatched source is still
    /// nulled (null-to-infer), never rescued by rewriting the source; a local
    /// type with a claimed import source is still nulled.
    #[test]
    fn test_validate_type_hints_http_behaviour_unchanged_no_source_overwrite() {
        let mut result = FileAnalysisResult {
            graphql_consumer_locates: vec![],
            mounts: vec![],
            endpoints: vec![EndpointResult {
                candidate_id: "span:1-2".to_string(),
                line_number: 10,
                owner_node: "app".to_string(),
                method: "GET".to_string(),
                path: "/users".to_string(),
                handler_name: "handler".to_string(),
                pattern_matched: "app.get".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                payload_expression_text: None,
                payload_expression_line: None,
                response_expression_text: None,
                response_expression_line: None,
                emission_style: None,
                // Imported symbol, wrong source: HTTP must null, not rewrite.
                primary_type_symbol: Some("User".to_string()),
                type_import_source: Some("./wrong".to_string()),
            }],
            data_calls: vec![DataCallResult {
                call_kind: None,
                candidate_id: "span:3-4".to_string(),
                line_number: 12,
                target: "/users".to_string(),
                method: Some("GET".to_string()),
                pattern_matched: "fetch(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                call_expression_text: None,
                call_expression_line: None,
                payload_expression_text: None,
                payload_expression_line: None,
                // Local type with a claimed import source: HTTP must null.
                primary_type_symbol: Some("LocalType".to_string()),
                type_import_source: Some("./local".to_string()),

                loopback_default_url: None,
                base: None,
                consumers_not_resolved: None,
            }],
            graphql_operations: vec![],
            pubsub_operations: vec![],
        };

        let mut imported_symbols = HashMap::new();
        imported_symbols.insert(
            "User".to_string(),
            ImportedSymbol {
                local_name: "User".to_string(),
                imported_name: "User".to_string(),
                source: "./types".to_string(),
                kind: SymbolKind::Named,
            },
        );
        let symbol_table = SymbolTable {
            local_types: HashSet::from(["LocalType".to_string()]),
            imported_symbols,
        };

        FileOrchestrator::validate_type_hints(&mut result, &symbol_table);

        assert!(
            result.endpoints[0].primary_type_symbol.is_none(),
            "HTTP imported symbol with mismatched source must still be nulled"
        );
        assert!(result.endpoints[0].type_import_source.is_none());
        assert!(
            result.data_calls[0].primary_type_symbol.is_none(),
            "HTTP local type with claimed import source must still be nulled"
        );
        assert!(result.data_calls[0].type_import_source.is_none());
    }

    #[test]
    fn test_build_mount_graph_cross_file_resolution() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mut file_results = HashMap::new();

        // Main app file that imports and mounts user router
        file_results.insert(
            "src/app.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![MountResult {
                    line_number: 10,
                    parent_node: "app".to_string(),
                    child_node: "userRouter".to_string(),
                    mount_path: "/api/users".to_string(),
                    import_source: Some("./routes/users".to_string()),
                    pattern_matched: ".use(".to_string(),
                }],
                endpoints: vec![],
                data_calls: vec![],
                graphql_operations: vec![],
                pubsub_operations: vec![],
            },
        );

        // User routes file with endpoints
        file_results.insert(
            "src/routes/users.ts".to_string(),
            FileAnalysisResult {
                graphql_consumer_locates: vec![],
                mounts: vec![],
                endpoints: vec![
                    EndpointResult {
                        candidate_id: "span:710-740".to_string(),
                        line_number: 5,
                        owner_node: "router".to_string(),
                        method: "GET".to_string(),
                        path: "/".to_string(),
                        handler_name: "listUsers".to_string(),
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
                    },
                    EndpointResult {
                        candidate_id: "span:750-780".to_string(),
                        line_number: 10,
                        owner_node: "router".to_string(),
                        method: "POST".to_string(),
                        path: "/".to_string(),
                        handler_name: "createUser".to_string(),
                        pattern_matched: ".post(".to_string(),
                        call_expression_span_start: None,
                        call_expression_span_end: None,
                        payload_expression_text: None,
                        payload_expression_line: None,
                        response_expression_text: None,
                        response_expression_line: None,
                        emission_style: None,
                        primary_type_symbol: None,
                        type_import_source: None,
                    },
                ],
                data_calls: vec![],
                graphql_operations: vec![],
                pubsub_operations: vec![],
            },
        );

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );

        // Should have the mount and both endpoints
        assert_eq!(graph.mounts.len(), 1);
        assert_eq!(graph.endpoints.len(), 2);

        // Verify the import mapping was created
        let has_import_map = graph
            .nodes
            .keys()
            .any(|k| k.starts_with("__import_map__::"));
        assert!(has_import_map, "Should have import mapping node");
    }

    /// Root of the on-disk module graph both barrel tests resolve through.
    /// Endpoint extraction is LLM-side, so the `FileAnalysisResult`s below are
    /// authored by hand to mirror what the analyzer emits for these files; the
    /// FIXTURE is what the deterministic import/mount resolution reads.
    fn barrel_fixture_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/barrel-reexport-mounts")
    }

    fn barrel_fixture(relative: &str) -> String {
        barrel_fixture_root()
            .join(relative)
            .to_string_lossy()
            .into_owned()
    }

    /// One route, attached to the plugin's own framework instance. Every
    /// module in the fixture uses the SAME owner name (`server`) — that
    /// collision is the point: the owner name cannot identify the module.
    fn plugin_endpoint(method: &str, path: &str) -> EndpointResult {
        EndpointResult {
            candidate_id: format!("span:{method}:{path}"),
            line_number: 4,
            owner_node: "server".to_string(),
            method: method.to_string(),
            path: path.to_string(),
            handler_name: "anonymous".to_string(),
            pattern_matched: format!(".{}(", method.to_lowercase()),
            call_expression_span_start: None,
            call_expression_span_end: None,
            payload_expression_text: None,
            payload_expression_line: None,
            response_expression_text: None,
            response_expression_line: None,
            emission_style: None,
            primary_type_symbol: None,
            type_import_source: None,
        }
    }

    fn register_mount(line_number: i32, child: &str, prefix: &str, source: &str) -> MountResult {
        MountResult {
            line_number,
            parent_node: "fastify".to_string(),
            child_node: child.to_string(),
            mount_path: prefix.to_string(),
            import_source: Some(source.to_string()),
            pattern_matched: ".register(".to_string(),
        }
    }

    /// (method, full_path, owner) for every endpoint, sorted.
    fn resolved_routes(graph: &MountGraph) -> Vec<(String, String, String)> {
        let mut routes: Vec<(String, String, String)> = graph
            .endpoints
            .iter()
            .map(|e| (e.method.clone(), e.full_path.clone(), e.owner.clone()))
            .collect();
        routes.sort();
        routes
    }

    fn nested_fixture_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nested-plugin-mounts")
    }

    /// A mount declared inside a plugin: the parent is the framework instance
    /// the plugin was handed, which every module in the fixture calls
    /// `server`.
    fn plugin_mount(line_number: i32, child: &str, prefix: &str, source: &str) -> MountResult {
        MountResult {
            line_number,
            parent_node: "server".to_string(),
            child_node: child.to_string(),
            mount_path: prefix.to_string(),
            import_source: Some(source.to_string()),
            pattern_matched: ".register(".to_string(),
        }
    }

    /// Routers registered two levels down: the root mounts an API plugin at
    /// `/api/v1`, and that plugin mounts two leaf routers. Both leaf routers
    /// declare the same relative paths (`/` and `/:id`), so applying only the
    /// leaf's own mount prefix left them colliding on `POST /` and `GET /:id`
    /// — the shape that made a real 2.9k-file service drop ~30 endpoints as
    /// duplicate manifest aliases (carrick#535).
    #[test]
    fn test_build_mount_graph_composes_the_whole_mount_chain() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mut file_results = HashMap::new();
        file_results.insert(
            "src/server.ts".to_string(),
            FileAnalysisResult {
                mounts: vec![MountResult {
                    line_number: 6,
                    parent_node: "server".to_string(),
                    child_node: "registerApiRoutes".to_string(),
                    mount_path: "/api/v1".to_string(),
                    import_source: Some("./api/index.js".to_string()),
                    pattern_matched: ".register(".to_string(),
                }],
                ..Default::default()
            },
        );
        file_results.insert(
            "src/api/index.ts".to_string(),
            FileAnalysisResult {
                mounts: vec![
                    plugin_mount(
                        8,
                        "registerCatalogRouter",
                        "/catalog",
                        "./catalog-router.js",
                    ),
                    plugin_mount(
                        9,
                        "registerInventoryRouter",
                        "/inventory",
                        "./inventory-router.js",
                    ),
                ],
                ..Default::default()
            },
        );
        for module in ["catalog", "inventory"] {
            file_results.insert(
                format!("src/api/{module}-router.ts"),
                FileAnalysisResult {
                    endpoints: vec![plugin_endpoint("POST", "/"), plugin_endpoint("GET", "/:id")],
                    ..Default::default()
                },
            );
        }

        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            &nested_fixture_root(),
        );

        assert_eq!(
            resolved_routes(&graph),
            vec![
                (
                    "GET".to_string(),
                    "/api/v1/catalog/:id".to_string(),
                    "registerCatalogRouter".to_string()
                ),
                (
                    "GET".to_string(),
                    "/api/v1/inventory/:id".to_string(),
                    "registerInventoryRouter".to_string()
                ),
                (
                    "POST".to_string(),
                    "/api/v1/catalog".to_string(),
                    "registerCatalogRouter".to_string()
                ),
                (
                    "POST".to_string(),
                    "/api/v1/inventory".to_string(),
                    "registerInventoryRouter".to_string()
                ),
            ],
            "every prefix in the chain must be applied, so the two routers' \
             identical relative paths stay distinct"
        );
    }

    /// The chain walk must not double-apply a prefix a router is mounted
    /// under twice by different names, and must keep alias fan-out (#373):
    /// one child mounted under two prefixes still serves both.
    #[test]
    fn test_mount_prefix_chains_fan_out_and_terminate() {
        let mounts = vec![
            MountEdge {
                parent: "app".to_string(),
                child: "api".to_string(),
                path_prefix: "/api".to_string(),
                middleware_stack: Vec::new(),
            },
            MountEdge {
                parent: "app".to_string(),
                child: "api".to_string(),
                path_prefix: "/api-beta".to_string(),
                middleware_stack: Vec::new(),
            },
            MountEdge {
                parent: "api".to_string(),
                child: "orders".to_string(),
                path_prefix: "/orders".to_string(),
                middleware_stack: Vec::new(),
            },
            // Cycle: `app` mounted back under its own descendant. The walk
            // must terminate rather than compose forever.
            MountEdge {
                parent: "orders".to_string(),
                child: "app".to_string(),
                path_prefix: "/loop".to_string(),
                middleware_stack: Vec::new(),
            },
        ];

        let chains = FileOrchestrator::mount_prefix_chains(&mounts);
        assert_eq!(
            chains.get("orders"),
            Some(&vec![
                "/api-beta/orders".to_string(),
                "/api/orders".to_string()
            ]),
            "a child mounted under two aliases serves its routes under both"
        );
        assert!(
            chains.get("app").is_some_and(|c| c.len() <= 8),
            "the cycle must terminate inside the fan-out cap"
        );
    }

    /// Four plugins registered through ONE barrel: same import specifier for
    /// all four, and three of the four modules name their plugin `routes`.
    /// Resolving by name collapsed every route onto the last binding
    /// (`/v1/logs/<path>`, owner `logsRoutes`); resolving by (file, exported
    /// name) keeps each module's identity.
    #[test]
    fn test_build_mount_graph_resolves_barrel_reexported_plugins_to_their_own_module() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mut file_results = HashMap::new();
        file_results.insert(
            "src/plugin.ts".to_string(),
            FileAnalysisResult {
                mounts: vec![
                    register_mount(10, "actionsRoutes", "/v1", "./routes.js"),
                    register_mount(11, "sessionsRoutes", "/v1", "./routes.js"),
                    register_mount(12, "filesRoutes", "/v1", "./routes.js"),
                    register_mount(13, "logsRoutes", "/v1/logs", "./routes.js"),
                ],
                ..Default::default()
            },
        );
        for (module, path) in [
            ("actions", "/actions"),
            ("sessions", "/sessions"),
            ("files", "/files"),
        ] {
            file_results.insert(
                format!("src/modules/{module}/{module}.routes.ts"),
                FileAnalysisResult {
                    endpoints: vec![plugin_endpoint("POST", path)],
                    ..Default::default()
                },
            );
        }
        file_results.insert(
            "src/modules/logs/logs.routes.ts".to_string(),
            FileAnalysisResult {
                endpoints: vec![plugin_endpoint("POST", "/entries")],
                ..Default::default()
            },
        );

        // Repo-relative keys with the root passed separately: the shape the
        // engine rebuilds a graph in, and the one that has to reach the
        // module files on disk to resolve the barrel.
        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            &barrel_fixture_root(),
        );

        assert_eq!(
            resolved_routes(&graph),
            vec![
                (
                    "POST".to_string(),
                    "/v1/actions".to_string(),
                    "actionsRoutes".to_string()
                ),
                (
                    "POST".to_string(),
                    "/v1/files".to_string(),
                    "filesRoutes".to_string()
                ),
                (
                    "POST".to_string(),
                    "/v1/logs/entries".to_string(),
                    "logsRoutes".to_string()
                ),
                (
                    "POST".to_string(),
                    "/v1/sessions".to_string(),
                    "sessionsRoutes".to_string()
                ),
            ],
            "each barrel-re-exported plugin must keep its own module's routes and prefix"
        );
    }

    /// The same collision without a barrel: two modules, both `export default
    /// routes`, imported directly and registered under different prefixes.
    #[test]
    fn test_build_mount_graph_resolves_direct_default_imports_to_their_own_module() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mut file_results = HashMap::new();
        file_results.insert(
            barrel_fixture("src/direct-plugin.ts"),
            FileAnalysisResult {
                mounts: vec![
                    register_mount(
                        8,
                        "ordersRoutes",
                        "/v1/orders",
                        "./modules/orders/orders.routes.js",
                    ),
                    register_mount(
                        9,
                        "reportsRoutes",
                        "/v1/reports",
                        "./modules/reports/reports.routes.js",
                    ),
                ],
                ..Default::default()
            },
        );
        file_results.insert(
            barrel_fixture("src/modules/orders/orders.routes.ts"),
            FileAnalysisResult {
                endpoints: vec![plugin_endpoint("GET", "/pending")],
                ..Default::default()
            },
        );
        file_results.insert(
            barrel_fixture("src/modules/reports/reports.routes.ts"),
            FileAnalysisResult {
                endpoints: vec![plugin_endpoint("GET", "/daily")],
                ..Default::default()
            },
        );

        // As-scanned absolute keys, the shape `analyze_files` builds in.
        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            &barrel_fixture_root(),
            Path::new(""),
        );

        assert_eq!(
            resolved_routes(&graph),
            vec![
                (
                    "GET".to_string(),
                    "/v1/orders/pending".to_string(),
                    "ordersRoutes".to_string()
                ),
                (
                    "GET".to_string(),
                    "/v1/reports/daily".to_string(),
                    "reportsRoutes".to_string()
                ),
            ],
            "directly imported default exports must not collapse onto one module either"
        );
    }

    #[test]
    fn test_infer_node_types() {
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mut graph = MountGraph::new();

        // Add nodes
        graph.nodes.insert(
            "app".to_string(),
            GraphNode {
                name: "app".to_string(),
                node_type: NodeType::Unknown,
                creation_site: None,
                file_location: "app.ts:1".to_string(),
            },
        );
        graph.nodes.insert(
            "userRouter".to_string(),
            GraphNode {
                name: "userRouter".to_string(),
                node_type: NodeType::Unknown,
                creation_site: None,
                file_location: "routes/users.ts:1".to_string(),
            },
        );

        // Add mount: app mounts userRouter
        graph.mounts.push(MountEdge {
            parent: "app".to_string(),
            child: "userRouter".to_string(),
            path_prefix: "/users".to_string(),
            middleware_stack: vec![],
        });

        orchestrator.infer_node_types(&mut graph);

        // app should be Root (mounts others, not mounted)
        assert_eq!(graph.nodes.get("app").unwrap().node_type, NodeType::Root);
        // userRouter should be Mountable (is mounted)
        assert_eq!(
            graph.nodes.get("userRouter").unwrap().node_type,
            NodeType::Mountable
        );
    }

    #[test]
    fn test_processing_stats_default() {
        let stats = ProcessingStats::default();
        assert_eq!(stats.files_processed, 0);
        assert_eq!(stats.files_skipped, 0);
        assert_eq!(stats.total_mounts, 0);
        assert_eq!(stats.total_endpoints, 0);
        assert_eq!(stats.total_data_calls, 0);
        assert_eq!(stats.file_based_endpoints, 0);
        assert!(stats.errors.is_empty());
    }

    fn next_conventions() -> Vec<RoutingConvention> {
        builtin_conventions(&["Next.js".to_string()], &[])
    }

    #[test]
    fn test_file_based_endpoints_app_router_method_per_export() {
        let scanner = SwcScanner::new();
        let content = r#"
export async function GET() { return Response.json([]); }
export async function POST(req: Request) { return Response.json({}); }
export const runtime = "edge";
"#;
        let mut endpoints = FileOrchestrator::file_based_endpoints(
            &scanner,
            Path::new("app/users/route.ts"),
            Path::new("app/users/route.ts"),
            content,
            &next_conventions(),
        );
        endpoints.sort_by(|a, b| a.method.cmp(&b.method));

        // GET + POST become endpoints; `runtime` is not an HTTP method.
        assert_eq!(endpoints.len(), 2, "expected GET and POST only");
        assert_eq!(endpoints[0].method, "GET");
        assert_eq!(endpoints[1].method, "POST");
        for ep in &endpoints {
            assert_eq!(ep.path, "/users");
            assert_eq!(ep.owner_node, FILE_BASED_ROUTE_OWNER);
            assert_eq!(ep.pattern_matched, "nextjs-app");
            assert!(ep.call_expression_span_start.is_some());
            assert!(ep.call_expression_span_end.is_some());
            // Type enrichment is deferred to the LLM/sidecar pass.
            assert!(ep.response_expression_text.is_none());
        }
    }

    #[test]
    fn test_file_based_endpoints_dynamic_segment() {
        let scanner = SwcScanner::new();
        let content = "export async function GET() {}\n";
        let endpoints = FileOrchestrator::file_based_endpoints(
            &scanner,
            Path::new("app/users/[id]/route.ts"),
            Path::new("app/users/[id]/route.ts"),
            content,
            &next_conventions(),
        );
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].method, "GET");
        assert_eq!(endpoints[0].path, "/users/:id");
    }

    #[test]
    fn test_file_based_endpoints_astro_filename_with_export_methods() {
        // Astro is the FileName + ExportName combination: the path comes from
        // the filename (like pages-router) but methods come from named exports
        // (like app-router). Both `export function` and `export const` forms
        // must be recognized.
        let scanner = SwcScanner::new();
        let content = r#"
export async function GET() { return new Response("[]"); }
export const POST = async (ctx) => new Response("{}");
export const prerender = false;
"#;
        let mut endpoints = FileOrchestrator::file_based_endpoints(
            &scanner,
            Path::new("src/pages/api/users.ts"),
            Path::new("src/pages/api/users.ts"),
            content,
            &builtin_conventions(&["Astro".to_string()], &[]),
        );
        endpoints.sort_by(|a, b| a.method.cmp(&b.method));

        // GET + POST become endpoints; `prerender` is not an HTTP method.
        assert_eq!(endpoints.len(), 2, "expected GET and POST only");
        assert_eq!(endpoints[0].method, "GET");
        assert_eq!(endpoints[1].method, "POST");
        for ep in &endpoints {
            assert_eq!(ep.path, "/api/users");
            assert_eq!(ep.owner_node, FILE_BASED_ROUTE_OWNER);
            assert_eq!(ep.pattern_matched, "astro");
        }
    }

    #[test]
    fn test_file_based_endpoints_astro_dynamic_segment() {
        let scanner = SwcScanner::new();
        let content = "export function GET() {}\n";
        let endpoints = FileOrchestrator::file_based_endpoints(
            &scanner,
            Path::new("src/pages/posts/[id].ts"),
            Path::new("src/pages/posts/[id].ts"),
            content,
            &builtin_conventions(&["Astro".to_string()], &[]),
        );
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].method, "GET");
        assert_eq!(endpoints[0].path, "/posts/:id");
    }

    #[test]
    fn test_file_based_endpoints_pages_router_default_export_deferred() {
        // Pages-router default export serves every method; the method set isn't
        // recoverable from the layout, so no endpoint is synthesized (yet).
        let scanner = SwcScanner::new();
        let content = "export default function handler(req, res) {}\n";
        let endpoints = FileOrchestrator::file_based_endpoints(
            &scanner,
            Path::new("pages/api/users.ts"),
            Path::new("pages/api/users.ts"),
            content,
            &next_conventions(),
        );
        assert!(endpoints.is_empty());
    }

    #[test]
    fn test_file_based_endpoints_non_route_file() {
        let scanner = SwcScanner::new();
        let content = "export async function GET() {}\n";
        let endpoints = FileOrchestrator::file_based_endpoints(
            &scanner,
            Path::new("src/services/users.ts"),
            Path::new("src/services/users.ts"),
            content,
            &next_conventions(),
        );
        assert!(
            endpoints.is_empty(),
            "non-route files should yield no file-based endpoints"
        );
    }

    fn flat_conventions() -> Vec<RoutingConvention> {
        builtin_conventions(&["remix".to_string()], &[])
    }

    #[test]
    fn test_file_based_endpoints_flat_route_factory_export() {
        // carrick#473: the route module's handler is the *result of a call*
        // (a route-builder factory), not a function declaration. It raises no
        // call-site candidate, so the endpoint has to be anchored on the
        // export itself — exactly as a declared handler would be.
        let scanner = SwcScanner::new();
        let content = r#"
import { makeRoute } from "~/lib/routeBuilders.server";
export const action = makeRoute({ body: WidgetSchema }, async ({ body }) => {
  return json({ ok: true });
});
"#;
        let endpoints = FileOrchestrator::file_based_endpoints(
            &scanner,
            Path::new("app/routes/api.v1.widgets.$widgetId.activate.ts"),
            Path::new("app/routes/api.v1.widgets.$widgetId.activate.ts"),
            content,
            &flat_conventions(),
        );
        assert_eq!(endpoints.len(), 1, "expected one endpoint for the action");
        let ep = &endpoints[0];
        assert_eq!(ep.method, "POST");
        assert_eq!(ep.path, "/api/v1/widgets/:widgetId/activate");
        assert_eq!(ep.handler_name, "action");
        assert_eq!(ep.owner_node, FILE_BASED_ROUTE_OWNER);
        assert_eq!(ep.pattern_matched, "remix-flat");
        // Span-anchored on the export, so the normal candidate join applies.
        assert!(ep.call_expression_span_start.is_some());
        assert!(ep.call_expression_span_end.is_some());
    }

    #[test]
    fn test_file_based_endpoints_flat_route_loader_is_get() {
        let scanner = SwcScanner::new();
        let content = r#"
export const loader = makeRoute({}, async () => json([]));
"#;
        let endpoints = FileOrchestrator::file_based_endpoints(
            &scanner,
            Path::new("app/routes/api.v1.widgets.$widgetId.ts"),
            Path::new("app/routes/api.v1.widgets.$widgetId.ts"),
            content,
            &flat_conventions(),
        );
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].method, "GET");
        assert_eq!(endpoints[0].path, "/api/v1/widgets/:widgetId");
    }

    #[test]
    fn test_file_based_endpoints_flat_route_declared_and_called_forms_agree() {
        // The declared-function form and the factory-call form of the same
        // route module must produce the same endpoint: the discriminator is
        // the export name, never the shape of its initializer.
        let scanner = SwcScanner::new();
        let rel = Path::new("app/routes/api.v1.widgets.$widgetId.activate.ts");
        let declared = FileOrchestrator::file_based_endpoints(
            &scanner,
            rel,
            rel,
            "export async function action({ request }) { return json({}); }\n",
            &flat_conventions(),
        );
        let called = FileOrchestrator::file_based_endpoints(
            &scanner,
            rel,
            rel,
            "export const action = makeRoute({}, async () => json({}));\n",
            &flat_conventions(),
        );
        assert_eq!(declared.len(), 1);
        assert_eq!(called.len(), 1);
        assert_eq!(declared[0].method, called[0].method);
        assert_eq!(declared[0].path, called[0].path);
    }

    #[test]
    fn test_file_based_endpoints_flat_route_non_handler_exports_ignored() {
        // A route module also exports helpers and config. Only the exports the
        // convention names as handlers become endpoints.
        let scanner = SwcScanner::new();
        let content = r#"
export const config = makeConfig({ runtime: "node" });
export const schema = buildSchema({});
export function serializeWidget(w) { return w; }
"#;
        let endpoints = FileOrchestrator::file_based_endpoints(
            &scanner,
            Path::new("app/routes/api.v1.widgets.ts"),
            Path::new("app/routes/api.v1.widgets.ts"),
            content,
            &flat_conventions(),
        );
        assert!(
            endpoints.is_empty(),
            "non-handler exports must not become endpoints, got {endpoints:?}"
        );
    }

    #[test]
    fn test_file_based_endpoints_flat_route_gate_is_the_route_plane() {
        // The same call-expression export outside the route plane yields
        // nothing: the recall fix is scoped to files a convention claims.
        let scanner = SwcScanner::new();
        let content = "export const action = makeRoute({}, async () => json({}));\n";
        for rel in [
            // Not under the route root.
            "app/services/widgets.server.ts",
            "src/lib/api.v1.widgets.$widgetId.activate.ts",
            // Under the route root, but the UI page plane, not an API surface.
            "app/routes/api.v1.widgets.$widgetId.activate.tsx",
        ] {
            let endpoints = FileOrchestrator::file_based_endpoints(
                &scanner,
                Path::new(rel),
                Path::new(rel),
                content,
                &flat_conventions(),
            );
            assert!(
                endpoints.is_empty(),
                "{rel} is not a route module; expected no endpoints, got {endpoints:?}"
            );
        }
    }

    #[test]
    fn test_file_based_endpoints_no_conventions_is_noop() {
        let scanner = SwcScanner::new();
        let content = "export async function GET() {}\n";
        // No convention-bearing framework detected → empty conventions.
        let endpoints = FileOrchestrator::file_based_endpoints(
            &scanner,
            Path::new("app/users/route.ts"),
            Path::new("app/users/route.ts"),
            content,
            &builtin_conventions(&["express".to_string()], &[]),
        );
        assert!(endpoints.is_empty());
    }

    #[test]
    fn route_descriptor_endpoint_owner_is_handler_not_method() {
        // #234: a route declared as data is emitted deterministically with the
        // handler identifier as owner — never the HTTP-method literal "GET"
        // (the owner-fabrication trap, #227). Drives the real fixture.
        let scanner = SwcScanner::new();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "tests/fixtures/xrepo-corpus-1/orders-monorepo/packages/gateway/src/health.handler.ts",
        );
        let content = std::fs::read_to_string(&fixture).expect("fixture must exist");

        let endpoints = FileOrchestrator::route_descriptor_endpoints(&scanner, &fixture, &content);

        assert_eq!(
            endpoints.len(),
            1,
            "expected exactly one route-descriptor endpoint, got {endpoints:?}"
        );
        let ep = &endpoints[0];
        assert_eq!(ep.method, "GET");
        assert_eq!(ep.path, "/gateway/health");
        assert_eq!(
            ep.owner_node, "healthCheckHandler",
            "owner must be the handler ident, not the method literal"
        );
        assert_ne!(ep.owner_node, "GET");
        assert_eq!(ep.handler_name, "healthCheckHandler");
        assert_eq!(ep.pattern_matched, ROUTE_DESCRIPTOR_PATTERN);
        assert!(ep.call_expression_span_start.is_some());
        assert!(ep.call_expression_span_end.is_some());
    }

    #[test]
    fn route_descriptor_endpoint_missing_handler_uses_sentinel_owner() {
        let scanner = SwcScanner::new();
        let content = r#"
const routes = [
  { method: 'POST', path: '/widgets' },
];
export { routes };
"#;
        let endpoints =
            FileOrchestrator::route_descriptor_endpoints(&scanner, Path::new("routes.ts"), content);
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].method, "POST");
        assert_eq!(endpoints[0].path, "/widgets");
        assert_eq!(endpoints[0].owner_node, ROUTE_DESCRIPTOR_OWNER);
    }

    fn synthetic_endpoint(method: &str, path: &str) -> EndpointResult {
        EndpointResult {
            candidate_id: format!("file-route:{}:0", method),
            line_number: 1,
            owner_node: FILE_BASED_ROUTE_OWNER.to_string(),
            method: method.to_string(),
            path: path.to_string(),
            handler_name: method.to_string(),
            pattern_matched: "nextjs-app".to_string(),
            call_expression_span_start: Some(0),
            call_expression_span_end: Some(1),
            payload_expression_text: None,
            payload_expression_line: None,
            response_expression_text: None,
            response_expression_line: None,
            emission_style: None,
            primary_type_symbol: None,
            type_import_source: None,
        }
    }

    #[test]
    fn test_merge_file_based_endpoints_dedups_by_method_and_path() {
        let mut result = FileAnalysisResult {
            // The LLM pass already produced GET /users (e.g. via a Response.json
            // candidate). The structural entry for it must not be duplicated.
            endpoints: vec![synthetic_endpoint("GET", "/users")],
            ..Default::default()
        };

        let added = FileOrchestrator::merge_file_based_endpoints(
            &mut result,
            vec![
                synthetic_endpoint("get", "/users"), // duplicate (case-insensitive method)
                synthetic_endpoint("POST", "/users"), // new method, same path
            ],
        );

        assert_eq!(added, 1);
        assert_eq!(result.endpoints.len(), 2);
        assert!(
            result
                .endpoints
                .iter()
                .any(|e| e.method == "POST" && e.path == "/users")
        );
    }

    #[test]
    fn test_canonicalize_route_path_brackets_to_colons() {
        // Astro/Next bracket params -> colon form; catch-alls -> **; colon and
        // literal segments are untouched (idempotent).
        assert_eq!(
            FileOrchestrator::canonicalize_route_path("/w/[slug]/projects/new"),
            "/w/:slug/projects/new"
        );
        assert_eq!(
            FileOrchestrator::canonicalize_route_path("/w/[slug]/p/[projSlug]/keys/new"),
            "/w/:slug/p/:projSlug/keys/new"
        );
        assert_eq!(
            FileOrchestrator::canonicalize_route_path("/files/[...path]"),
            "/files/**"
        );
        // Next.js optional catch-all must also reach `**`, matching the router,
        // not a malformed `:[slug]`.
        assert_eq!(
            FileOrchestrator::canonicalize_route_path("/blog/[[...slug]]"),
            "/blog/**"
        );
        // Whitespace-jittered brackets must still dedupe against the router's
        // trimmed colon form (Copilot review).
        assert_eq!(
            FileOrchestrator::canonicalize_route_path("/w/[ slug ]/x"),
            "/w/:slug/x"
        );
        assert_eq!(
            FileOrchestrator::canonicalize_route_path("/w/[[ id ]]/x"),
            "/w/:id/x"
        );
        assert_eq!(
            FileOrchestrator::canonicalize_route_path("/w/:slug/invite"),
            "/w/:slug/invite"
        );
    }

    #[test]
    fn test_canonicalize_collapses_bracket_and_colon_duplicate() {
        // The LLM emitted the bracket form; the file-based router emits the colon
        // form. After canonicalization they are the same path, so the structural
        // entry dedupes instead of producing a second, form-flipped endpoint.
        let mut result = FileAnalysisResult {
            endpoints: vec![synthetic_endpoint("POST", "/w/[slug]/projects/new")],
            ..Default::default()
        };
        FileOrchestrator::canonicalize_endpoint_paths(&mut result);
        assert_eq!(result.endpoints[0].path, "/w/:slug/projects/new");

        let added = FileOrchestrator::merge_file_based_endpoints(
            &mut result,
            vec![synthetic_endpoint("POST", "/w/:slug/projects/new")],
        );
        assert_eq!(
            added, 0,
            "colon-form route should dedupe against the canonicalized LLM path"
        );
        assert_eq!(result.endpoints.len(), 1);
    }

    #[test]
    fn test_canonicalize_route_path_trims_llm_whitespace() {
        // The file-analyzer occasionally emits whitespace-jittered root routes
        // ("/ "); the space must not survive into full_path where it breaks
        // matching both ways (#332).
        assert_eq!(FileOrchestrator::canonicalize_route_path("/ "), "/");
        assert_eq!(
            FileOrchestrator::canonicalize_route_path("/users "),
            "/users"
        );
        assert_eq!(
            FileOrchestrator::canonicalize_route_path(" /users"),
            "/users"
        );
        // A whitespace-only or empty path is the root route.
        assert_eq!(FileOrchestrator::canonicalize_route_path(" "), "/");
        assert_eq!(FileOrchestrator::canonicalize_route_path(""), "/");
    }

    #[test]
    fn test_root_route_with_whitespace_joins_to_mount_prefix() {
        // A root route the LLM emitted as "/ " on a router mounted at
        // /api/users must resolve to /api/users, not "/api/users/ " (#332).
        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let mut result = FileAnalysisResult {
            mounts: vec![MountResult {
                line_number: 10,
                parent_node: "app".to_string(),
                child_node: "userRouter".to_string(),
                mount_path: "/api/users".to_string(),
                import_source: Some("./routes/users".to_string()),
                pattern_matched: ".use(".to_string(),
            }],
            endpoints: vec![{
                let mut ep = synthetic_endpoint("GET", "/ ");
                ep.owner_node = "userRouter".to_string();
                ep
            }],
            ..Default::default()
        };

        // Ingestion order mirrors analyze_files: canonicalize, then graph build.
        FileOrchestrator::canonicalize_endpoint_paths(&mut result);

        let mut file_results = HashMap::new();
        file_results.insert("src/app.ts".to_string(), result);
        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );

        assert_eq!(graph.endpoints.len(), 1);
        assert_eq!(graph.endpoints[0].full_path, "/api/users");
    }

    fn candidate_with_snippet(id: &str, snippet: Option<&str>) -> CandidateTarget {
        CandidateTarget {
            protocol: crate::operation::Protocol::Http,
            candidate_id: id.to_string(),
            span_start: 100,
            span_end: 140,
            line_number: 12,
            callee_object: "router".to_string(),
            callee_property: Some("get".to_string()),
            enclosing_function: None,
            path_snippet: snippet.map(|s| s.to_string()),
            code_snippet: "router.get(...)".to_string(),
            request_spec: None,
            new_url_path: None,
            request_shape: crate::wrapper_request_shape::RequestShapeSignal::NotARequest,
        }
    }

    fn endpoint_with_candidate(path: &str, candidate_id: &str) -> EndpointResult {
        let mut ep = synthetic_endpoint("GET", path);
        ep.candidate_id = candidate_id.to_string();
        ep
    }

    #[test]
    fn test_apply_candidate_map_reanchors_path_to_registration_literal() {
        // #332: for a root route the LLM copied the sibling route's path
        // ("/:id"). The first-arg string literal at the candidate the endpoint
        // already points at is deterministic ground truth, so it wins.
        let mut result = FileAnalysisResult {
            endpoints: vec![endpoint_with_candidate("/:id", "c1")],
            ..Default::default()
        };
        let mut candidate_map = HashMap::new();
        candidate_map.insert(
            "c1".to_string(),
            candidate_with_snippet("c1", Some("\"/\"")),
        );
        FileOrchestrator::apply_candidate_map(&mut result, &candidate_map, "routes/orders.ts");
        assert_eq!(result.endpoints[0].path, "/");
    }

    #[test]
    fn test_apply_candidate_map_keeps_path_that_extends_the_literal() {
        // A constructor-carried prefix baked into the emitted path extends the
        // registration literal at a segment boundary. That is not a mis-copy;
        // it must survive (join_paths' idempotent guard depends on it).
        let mut result = FileAnalysisResult {
            endpoints: vec![endpoint_with_candidate("/api/v1/status", "c1")],
            ..Default::default()
        };
        let mut candidate_map = HashMap::new();
        candidate_map.insert(
            "c1".to_string(),
            candidate_with_snippet("c1", Some("'/status'")),
        );
        FileOrchestrator::apply_candidate_map(&mut result, &candidate_map, "src/app.ts");
        assert_eq!(result.endpoints[0].path, "/api/v1/status");
    }

    #[test]
    fn test_apply_candidate_map_ignores_non_literal_snippets() {
        // Template literals, expressions, and absent snippets are ambiguous;
        // only a plain quoted literal may override the emitted path.
        let mut result = FileAnalysisResult {
            endpoints: vec![
                endpoint_with_candidate("/x/:y", "c1"),
                endpoint_with_candidate("/a", "c2"),
            ],
            ..Default::default()
        };
        let mut candidate_map = HashMap::new();
        candidate_map.insert(
            "c1".to_string(),
            candidate_with_snippet("c1", Some("`/x/${y}`")),
        );
        candidate_map.insert("c2".to_string(), candidate_with_snippet("c2", None));
        FileOrchestrator::apply_candidate_map(&mut result, &candidate_map, "src/app.ts");
        assert_eq!(result.endpoints[0].path, "/x/:y");
        assert_eq!(result.endpoints[1].path, "/a");
    }

    /// A force-analyzed file (GraphQL fall-through, messaging fall-through, or
    /// the #369 wrapper rescue) carries an empty candidate map by construction,
    /// so every reported operation is dropped for a reason that has nothing to
    /// do with the analyzer inventing an id. The two causes must not share a
    /// message: reading "no matching SWC candidate" against a force-analyzed
    /// file reads as a wholesale extraction failure, which is what sent one
    /// investigation after a phantom regression.
    #[test]
    fn test_drop_reason_separates_no_candidates_from_id_mismatch() {
        let forced = FileOrchestrator::drop_reason(0);
        assert!(
            forced.contains("force-analyzed"),
            "an empty map must be reported as force-analysis, got: {forced}"
        );
        // The map is built from HTTP candidates only, so an empty map must not
        // claim the scanner saw nothing: a force-analyzed GraphQL or messaging
        // file can still have raised unrouted candidates of another protocol.
        assert!(
            forced.contains("HTTP"),
            "the empty-map message must scope its claim to HTTP candidates, got: {forced}"
        );

        let mismatch = FileOrchestrator::drop_reason(19);
        assert!(
            mismatch.contains("19"),
            "the offered count is the fact that distinguishes the causes, got: {mismatch}"
        );
        assert!(
            !mismatch.contains("force-analyzed"),
            "a real id mismatch must not claim force-analysis, got: {mismatch}"
        );
    }

    /// Diagnostics only: dropping an endpoint whose candidate_id matches no
    /// offered candidate is the existing contract and stays exactly as it was,
    /// on both routes into the analyzer.
    #[test]
    fn test_apply_candidate_map_drops_unjoinable_endpoints_on_both_routes() {
        // Route 1: candidates were offered, one id is unknown.
        let mut result = FileAnalysisResult {
            endpoints: vec![
                endpoint_with_candidate("/known", "c1"),
                endpoint_with_candidate("/invented", "c-nope"),
            ],
            ..Default::default()
        };
        let mut candidate_map = HashMap::new();
        candidate_map.insert("c1".to_string(), candidate_with_snippet("c1", None));
        FileOrchestrator::apply_candidate_map(&mut result, &candidate_map, "src/app.ts");
        assert_eq!(result.endpoints.len(), 1);
        assert_eq!(result.endpoints[0].path, "/known");

        // Route 2: force-analyzed, no candidates offered at all.
        let mut forced = FileAnalysisResult {
            endpoints: vec![endpoint_with_candidate("/a", "c1")],
            ..Default::default()
        };
        FileOrchestrator::apply_candidate_map(&mut forced, &HashMap::new(), "src/forced.ts");
        assert!(forced.endpoints.is_empty());
    }

    fn request_spec_candidate(id: &str, method: &str, url: &str) -> CandidateTarget {
        let mut candidate = candidate_with_snippet(id, Some(&format!("'{url}'")));
        candidate.callee_object = "apiClient".to_string();
        candidate.callee_property = None;
        candidate.request_spec = Some(crate::swc_scanner::RequestSpec {
            method: method.to_string(),
            url: url.to_string(),
            method_from_callee: false,
        });
        candidate
    }

    /// The #529 form: the verb is the member being invoked, so the spec is one
    /// the backfill may emit on its own.
    fn verb_call_candidate(id: &str, method: &str, url: &str) -> CandidateTarget {
        let mut candidate = request_spec_candidate(id, method, url);
        candidate.callee_object = "client".to_string();
        candidate.callee_property = Some(method.to_ascii_lowercase());
        if let Some(spec) = candidate.request_spec.as_mut() {
            spec.method_from_callee = true;
        }
        candidate
    }

    fn data_call_with(candidate_id: &str, target: &str, method: Option<&str>) -> DataCallResult {
        DataCallResult {
            call_kind: None,
            candidate_id: candidate_id.to_string(),
            line_number: 1,
            target: target.to_string(),
            method: method.map(|m| m.to_string()),
            pattern_matched: "client(".to_string(),
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
        }
    }

    /// #537: method and URL written as data on the call's own argument are AST
    /// facts, so the model's answer never stands against them. The wildcard is
    /// the observed failure — seven such calls all reported as `/*`, which then
    /// matched a producer's SPA fallback.
    #[test]
    fn request_spec_overrules_the_models_target_and_method() {
        let mut result = FileAnalysisResult {
            data_calls: vec![
                data_call_with("c1", "/*", Some("POST")),
                data_call_with("c2", "", None),
            ],
            ..Default::default()
        };
        let mut candidate_map = HashMap::new();
        candidate_map.insert(
            "c1".to_string(),
            request_spec_candidate("c1", "POST", "/api/v1/auth/universal-auth/login"),
        );
        candidate_map.insert(
            "c2".to_string(),
            request_spec_candidate("c2", "DELETE", "/api/v1/sessions/current"),
        );

        FileOrchestrator::apply_candidate_map(&mut result, &candidate_map, "src/client.ts");

        assert_eq!(
            result.data_calls[0].target,
            "/api/v1/auth/universal-auth/login"
        );
        assert_eq!(result.data_calls[0].method.as_deref(), Some("POST"));
        assert_eq!(result.data_calls[1].target, "/api/v1/sessions/current");
        assert_eq!(result.data_calls[1].method.as_deref(), Some("DELETE"));
    }

    /// The base is the one thing the model may legitimately know better: a
    /// client built with a configured `baseURL` yields the same path behind a
    /// host the normalizer needs for internal/external classification. A target
    /// that already ends with the structural literal at a segment boundary is
    /// therefore kept.
    #[test]
    fn request_spec_keeps_a_target_that_already_carries_its_url() {
        let mut result = FileAnalysisResult {
            data_calls: vec![
                data_call_with("c1", "${API_URL}/api/v1/status", Some("GET")),
                data_call_with("c2", "/api/v1/status", Some("GET")),
            ],
            ..Default::default()
        };
        let mut candidate_map = HashMap::new();
        candidate_map.insert(
            "c1".to_string(),
            request_spec_candidate("c1", "GET", "/api/v1/status"),
        );
        candidate_map.insert(
            "c2".to_string(),
            request_spec_candidate("c2", "GET", "/api/v1/status"),
        );

        FileOrchestrator::apply_candidate_map(&mut result, &candidate_map, "src/client.ts");

        assert_eq!(result.data_calls[0].target, "${API_URL}/api/v1/status");
        assert_eq!(result.data_calls[1].target, "/api/v1/status");
    }

    /// A candidate without a request spec — every positional call shape — is
    /// untouched, so the model stays the authority where it always was.
    #[test]
    fn data_calls_without_a_request_spec_are_left_alone() {
        let mut result = FileAnalysisResult {
            data_calls: vec![data_call_with("c1", "${API_URL}/things", Some("GET"))],
            ..Default::default()
        };
        let mut candidate_map = HashMap::new();
        candidate_map.insert("c1".to_string(), candidate_with_snippet("c1", Some("'/x'")));

        FileOrchestrator::apply_candidate_map(&mut result, &candidate_map, "src/client.ts");

        assert_eq!(result.data_calls[0].target, "${API_URL}/things");
        assert_eq!(result.data_calls[0].method.as_deref(), Some("GET"));
    }

    /// #529: a generated OpenAPI client is hundreds of near-identical
    /// wrappers, and extraction routinely returns none of them. The method and
    /// the path are AST facts at every one of those sites, so the operations
    /// are emitted deterministically rather than lost — otherwise the producer
    /// reports its endpoints orphaned while a consumer in the index calls them.
    #[test]
    fn merge_request_spec_calls_backfills_operations_extraction_missed() {
        let mut result = FileAnalysisResult::default();
        let mut candidate_map = HashMap::new();
        let mut release = verb_call_candidate("c1", "POST", "/v1/sessions/:sessionId/release");
        release.span_start = 400;
        let mut list = verb_call_candidate("c2", "GET", "/v1/sessions");
        list.span_start = 100;
        candidate_map.insert("c1".to_string(), release);
        candidate_map.insert("c2".to_string(), list);

        let added = FileOrchestrator::merge_request_spec_calls(&mut result, &candidate_map);

        assert_eq!(added, 2);
        // Source order, not hash order: the same file must produce the same
        // rows on every scan.
        let emitted: Vec<(&str, Option<&str>)> = result
            .data_calls
            .iter()
            .map(|call| (call.target.as_str(), call.method.as_deref()))
            .collect();
        assert_eq!(
            emitted,
            vec![
                ("/v1/sessions", Some("GET")),
                ("/v1/sessions/:sessionId/release", Some("POST")),
            ]
        );
        // The span makes the backfilled call candidate-backed downstream and is
        // what the type sidecar anchors on.
        assert_eq!(result.data_calls[0].call_expression_span_start, Some(100));
        assert_eq!(result.data_calls[0].call_expression_span_end, Some(140));
        assert_eq!(result.data_calls[0].line_number, 12);
    }

    /// The backfill never duplicates an operation the analyzer did report —
    /// neither at the same site, nor as the same (method, path) behind the base
    /// URL the analyzer resolved and this pass cannot see.
    #[test]
    fn merge_request_spec_calls_skips_operations_already_extracted() {
        let mut result = FileAnalysisResult {
            data_calls: vec![
                data_call_with("c1", "/v1/sessions/:sessionId/release", Some("POST")),
                data_call_with("other", "${API_URL}/v1/sessions", Some("GET")),
            ],
            ..Default::default()
        };
        let mut candidate_map = HashMap::new();
        candidate_map.insert(
            "c1".to_string(),
            verb_call_candidate("c1", "POST", "/v1/sessions/:sessionId/release"),
        );
        // Same operation, reported against another candidate id with the base
        // in front of it.
        candidate_map.insert(
            "c2".to_string(),
            verb_call_candidate("c2", "GET", "/v1/sessions"),
        );
        // A different operation on the same path prefix must still be emitted.
        candidate_map.insert(
            "c3".to_string(),
            verb_call_candidate("c3", "DELETE", "/v1/sessions"),
        );

        let added = FileOrchestrator::merge_request_spec_calls(&mut result, &candidate_map);

        assert_eq!(added, 1);
        assert_eq!(result.data_calls.len(), 3);
        assert_eq!(result.data_calls[2].target, "/v1/sessions");
        assert_eq!(result.data_calls[2].method.as_deref(), Some("DELETE"));
    }

    /// A `{ method, url }` object states a request and a route registration
    /// identically, so only the verb-named form — where the verb is the
    /// operation being performed — is emitted without the analyzer. The config
    /// form keeps its #537 anchoring role and nothing more.
    #[test]
    fn merge_request_spec_calls_ignores_config_object_specs() {
        let mut result = FileAnalysisResult::default();
        let mut candidate_map = HashMap::new();
        candidate_map.insert(
            "c1".to_string(),
            request_spec_candidate("c1", "GET", "/v1/health"),
        );

        let added = FileOrchestrator::merge_request_spec_calls(&mut result, &candidate_map);

        assert_eq!(added, 0);
        assert!(result.data_calls.is_empty());
    }

    fn wrapper_call(
        line: usize,
        span_start: u32,
        target: &str,
        method: Option<&str>,
    ) -> LocalWrapperCall {
        LocalWrapperCall {
            span_start,
            span_end: span_start + 40,
            line_number: line,
            wrapper_name: "requestJson".to_string(),
            target: target.to_string(),
            method: method.map(|method| method.to_string()),
        }
    }

    /// carrick#588: a site that delegates to a same-file request wrapper raises
    /// no candidate, so extraction is never asked about it and the endpoint it
    /// reaches is absent from the index. The join is an AST fact and is merged
    /// in after the pass.
    #[test]
    fn merge_local_wrapper_calls_backfills_sites_never_offered_to_extraction() {
        let mut result = FileAnalysisResult::default();
        let calls = vec![
            wrapper_call(31, 100, "${base}/api/v1/widgets", None),
            wrapper_call(37, 300, "${base}/api/v1/widgets/${id}", Some("DELETE")),
        ];

        let added = FileOrchestrator::merge_local_wrapper_calls(&mut result, calls);

        assert_eq!(added, 2);
        let emitted: Vec<(&str, Option<&str>)> = result
            .data_calls
            .iter()
            .map(|call| (call.target.as_str(), call.method.as_deref()))
            .collect();
        assert_eq!(
            emitted,
            vec![
                ("${base}/api/v1/widgets", None),
                ("${base}/api/v1/widgets/${id}", Some("DELETE")),
            ],
            "the wrapper's base is kept verbatim, and an unstated method is left \
             unset rather than asserted as GET"
        );
        // The span is the SITE's: it anchors the type sidecar and marks the call
        // candidate-backed downstream.
        assert_eq!(result.data_calls[0].call_expression_span_start, Some(100));
        assert_eq!(result.data_calls[0].call_expression_span_end, Some(140));
        assert_eq!(result.data_calls[0].line_number, 31);
        assert_eq!(result.data_calls[0].pattern_matched, "requestJson");
    }

    /// carrick#588 finding 6: a package that reaches its siblings through one
    /// helper passes each path as an argument, and some of those paths carry a
    /// query string the caller builds (`?${params.toString()}`). The site
    /// resolves, but the parens the built query leaves in the target used to
    /// fail the route-shape gate in `build_mount_graph`, so those calls were
    /// dropped between the backfill and the graph and the endpoints they reach
    /// had no consumer in the index.
    #[test]
    fn wrapper_calls_with_a_built_query_string_reach_the_graph() {
        let orchestrator = FileOrchestrator::new(AgentService::new());
        let mut result = FileAnalysisResult::default();
        let calls = vec![
            wrapper_call(31, 100, "${base}/api/v1/widgets", None),
            wrapper_call(35, 300, "${base}/api/v1/widgets?${params.toString()}", None),
            wrapper_call(
                39,
                500,
                "${base}/api/v1/widgets/${id}/history?since=${encodeURIComponent(at)}",
                None,
            ),
        ];

        let added = FileOrchestrator::merge_local_wrapper_calls(&mut result, calls);
        assert_eq!(added, 3, "every site is backfilled");

        let mut file_results = HashMap::new();
        file_results.insert("src/tools.ts".to_string(), result);
        let graph = orchestrator.build_mount_graph(
            &file_results,
            &UrlNormalizer::default_permissive(),
            Path::new(""),
            Path::new(""),
        );

        let mut reached: Vec<(&str, &str)> = graph
            .data_calls
            .iter()
            .map(|call| (call.method.as_str(), call.canonical_path.as_str()))
            .collect();
        reached.sort_unstable();
        assert_eq!(
            reached,
            vec![
                ("GET", "/api/v1/widgets"),
                ("GET", "/api/v1/widgets"),
                ("GET", "/api/v1/widgets/:id/history"),
            ],
            "the query string is not part of the route and must not drop the call"
        );
    }

    /// Two sites on one source line (a `Promise.all` of them) are two calls:
    /// the first backfill must not suppress its siblings.
    #[test]
    fn merge_local_wrapper_calls_keeps_siblings_on_one_line() {
        let mut result = FileAnalysisResult::default();
        let calls = vec![
            wrapper_call(88, 100, "${base}/api/v1/widgets", None),
            wrapper_call(88, 200, "${base}/api/v1/gadgets", None),
        ];

        let added = FileOrchestrator::merge_local_wrapper_calls(&mut result, calls);

        assert_eq!(added, 2);
        assert_eq!(result.data_calls[1].target, "${base}/api/v1/gadgets");
    }

    /// A site extraction DID answer keeps the analyzer's row: same span, same
    /// line, or the same path already carried behind a base.
    #[test]
    fn merge_local_wrapper_calls_skips_sites_extraction_already_answered() {
        let mut by_span = data_call_with("c1", "/api/v1/widgets", Some("GET"));
        by_span.line_number = 90;
        by_span.call_expression_span_start = Some(100);
        let mut by_line = data_call_with("c2", "/unrelated", Some("GET"));
        by_line.line_number = 37;
        let mut by_path = data_call_with("c3", "${base}/api/v1/things", Some("POST"));
        by_path.line_number = 91;

        let mut result = FileAnalysisResult {
            data_calls: vec![by_span, by_line, by_path],
            ..Default::default()
        };
        let calls = vec![
            wrapper_call(31, 100, "${base}/api/v1/widgets", None),
            wrapper_call(37, 300, "${base}/api/v1/widgets/${id}", Some("DELETE")),
            wrapper_call(44, 500, "${base}/api/v1/things", Some("POST")),
            wrapper_call(51, 700, "${base}/api/v1/things", Some("DELETE")),
        ];

        let added = FileOrchestrator::merge_local_wrapper_calls(&mut result, calls);

        assert_eq!(added, 1, "only the site nothing already covers is emitted");
        assert_eq!(result.data_calls[3].target, "${base}/api/v1/things");
        assert_eq!(result.data_calls[3].method.as_deref(), Some("DELETE"));
    }

    fn resolved_member(span: u32, method: &str, target: &str) -> HashMap<u32, ResolvedMember> {
        HashMap::from([(
            span,
            ResolvedMember {
                name: "member".to_string(),
                member: ring_member(method, target),
            },
        )])
    }

    /// A member as the ring assertions compare it. `request_line` is
    /// provenance and outside `RequestMember`'s `PartialEq`, so the value here
    /// is never read.
    fn ring_member(method: &str, target: &str) -> RequestMember {
        RequestMember {
            method: method.to_string(),
            target: target.to_string(),
            request_line: 0,
        }
    }

    /// The join outcome the ring assertions compare against: the member, under
    /// the name the site called it by.
    fn ring_outcome(name: &str, method: &str, target: &str) -> ResolvedMember {
        ResolvedMember {
            name: name.to_string(),
            member: ring_member(method, target),
        }
    }

    fn ring(entries: &[(&str, &str, &str, &str)]) -> Vec<(PathBuf, RequestMemberIndex)> {
        let mut by_module: Vec<(PathBuf, RequestMemberIndex)> = Vec::new();
        for (module, name, method, target) in entries {
            let path = PathBuf::from(module);
            let index = match by_module.iter_mut().find(|(p, _)| *p == path) {
                Some((_, index)) => index,
                None => {
                    by_module.push((path, RequestMemberIndex::new()));
                    &mut by_module.last_mut().unwrap().1
                }
            };
            index.insert(name.to_string(), ring_member(method, target));
        }
        by_module
    }

    fn site(callee_object: &str, callee_property: &str) -> HashMap<String, CandidateTarget> {
        let mut candidate = candidate_with_snippet("c1", None);
        candidate.callee_object = callee_object.to_string();
        candidate.callee_property = Some(callee_property.to_string());
        HashMap::from([("c1".to_string(), candidate)])
    }

    /// carrick#655: a member the consumer's own imports do not declare is
    /// looked for one ring further out, where the factory's module imported
    /// the client.
    #[test]
    fn resolve_imported_members_reads_the_second_ring_when_the_first_declares_nothing() {
        let rings = vec![
            ring(&[]),
            ring(&[(
                "client.ts",
                "listThings",
                "GET",
                "${this.base}/api/v1/things",
            )]),
        ];
        let (resolved, _declined) = FileOrchestrator::resolve_imported_members(
            &site("projectClient", "listThings"),
            rings,
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(
            resolved.get(&100),
            Some(&ring_outcome(
                "listThings",
                "GET",
                "${this.base}/api/v1/things"
            ))
        );
    }

    /// The nearest ring that declares the name decides, even when a further
    /// ring declares it too.
    #[test]
    fn resolve_imported_members_takes_the_nearest_ring_that_declares_the_name() {
        let rings = vec![
            ring(&[("near.ts", "listThings", "GET", "${this.base}/api/v2/things")]),
            ring(&[("far.ts", "listThings", "GET", "${this.base}/api/v1/things")]),
        ];
        let (resolved, _declined) = FileOrchestrator::resolve_imported_members(
            &site("client", "listThings"),
            rings,
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(
            resolved.get(&100),
            Some(&ring_outcome(
                "listThings",
                "GET",
                "${this.base}/api/v2/things"
            ))
        );
    }

    /// A name a nearer ring declares ambiguously is dropped there. It is not
    /// looked for further out: the ambiguity is the answer.
    #[test]
    fn resolve_imported_members_stops_at_a_ring_that_declares_the_name_ambiguously() {
        let rings = vec![
            ring(&[
                ("a.ts", "listThings", "GET", "${this.base}/api/a"),
                ("b.ts", "listThings", "GET", "${this.base}/api/b"),
            ]),
            ring(&[("far.ts", "listThings", "GET", "${this.base}/api/v1/things")]),
        ];
        let (resolved, _declined) = FileOrchestrator::resolve_imported_members(
            &site("client", "listThings"),
            rings,
            &HashMap::new(),
            &HashMap::new(),
        );
        assert!(resolved.is_empty(), "{resolved:?}");
    }

    /// The receiver constraint applies whichever ring the member is in: a
    /// receiver imported from a module other than the member's joins nothing.
    #[test]
    fn resolve_imported_members_keeps_the_receiver_constraint_across_rings() {
        let rings = vec![
            ring(&[]),
            ring(&[(
                "client.ts",
                "listThings",
                "GET",
                "${this.base}/api/v1/things",
            )]),
        ];
        let import_owners =
            HashMap::from([("legacy".to_string(), Some(PathBuf::from("legacy.ts")))]);
        let (resolved, _declined) = FileOrchestrator::resolve_imported_members(
            &site("legacy", "listThings"),
            rings,
            &import_owners,
            &HashMap::new(),
        );
        assert!(resolved.is_empty(), "{resolved:?}");
    }

    /// carrick#656: a receiver imported from a module that CONSTRUCTS the
    /// client and exports the instance is a site the join gives up on by
    /// design, so it is counted and a row can say the list is one short.
    #[test]
    fn resolve_imported_members_counts_a_declined_receiver_that_holds_the_client() {
        let rings = vec![
            ring(&[]),
            ring(&[(
                "client.ts",
                "listThings",
                "GET",
                "${this.base}/api/v1/things",
            )]),
        ];
        let import_owners =
            HashMap::from([("client".to_string(), Some(PathBuf::from("instance.ts")))]);
        let receiver_imports = HashMap::from([(
            PathBuf::from("instance.ts"),
            BTreeSet::from([PathBuf::from("client.ts")]),
        )]);
        let (resolved, declined) = FileOrchestrator::resolve_imported_members(
            &site("client", "listThings"),
            rings,
            &import_owners,
            &receiver_imports,
        );
        assert!(
            resolved.is_empty(),
            "the join still declines it: {resolved:?}"
        );
        assert_eq!(declined, vec![(100, "listThings".to_string())]);
    }

    /// A receiver imported from a module that never imports the client's is a
    /// different function that shares a name. Counting it would send a reader
    /// after a call site of this member that does not exist.
    #[test]
    fn resolve_imported_members_does_not_count_a_name_collision() {
        let rings = vec![
            ring(&[]),
            ring(&[(
                "client.ts",
                "createArtifactUrl",
                "PUT",
                "${this.base}/api/v2/artifacts",
            )]),
        ];
        let import_owners =
            HashMap::from([("apiClient".to_string(), Some(PathBuf::from("legacy.ts")))]);
        let receiver_imports = HashMap::from([(
            PathBuf::from("legacy.ts"),
            BTreeSet::from([PathBuf::from("strings.ts")]),
        )]);
        let (resolved, declined) = FileOrchestrator::resolve_imported_members(
            &site("apiClient", "createArtifactUrl"),
            rings,
            &import_owners,
            &receiver_imports,
        );
        assert!(resolved.is_empty(), "{resolved:?}");
        assert!(declined.is_empty(), "{declined:?}");
    }

    /// A member two modules declare is attributed to neither: a site naming it
    /// belongs to one no more than the other.
    #[test]
    fn member_homes_drops_a_name_two_modules_declare() {
        let member_cache = HashMap::from([
            (
                PathBuf::from("/repo/client.ts"),
                RequestMemberIndex::from([(
                    "listThings".to_string(),
                    ring_member("GET", "${this.base}/api/v1/things"),
                )]),
            ),
            (
                PathBuf::from("/repo/other.ts"),
                RequestMemberIndex::from([
                    (
                        "listThings".to_string(),
                        ring_member("GET", "${this.base}/api/v2/things"),
                    ),
                    (
                        "readThing".to_string(),
                        ring_member("GET", "${this.base}/api/v1/things/${id}"),
                    ),
                ]),
            ),
        ]);
        let analysed = HashMap::from([
            (PathBuf::from("/repo/client.ts"), "client.ts".to_string()),
            (PathBuf::from("/repo/other.ts"), "other.ts".to_string()),
        ]);

        let homes = FileOrchestrator::member_homes(&member_cache, &analysed);

        assert!(!homes.contains_key("listThings"), "declared twice");
        assert_eq!(
            homes.get("readThing").map(|home| home.path_str.as_str()),
            Some("other.ts")
        );
    }

    /// The member's own request row carries the count too (carrick#656): it is
    /// the row an operation lists when NO site resolved, which is exactly the
    /// state the count describes. Found by the line the request is written on,
    /// because a client whose request goes through a helper raises no
    /// candidate and its row carries no span.
    #[test]
    fn stamp_unfollowed_member_sites_marks_the_member_s_own_row() {
        let mut client = FileAnalysisResult::default();
        client
            .data_calls
            .push(call_with_span(18, "${this.baseUrl}/api/v1/things", None));
        let mut consumer = FileAnalysisResult::default();
        consumer.data_calls.push(call_with_span(
            4,
            "${this.baseUrl}/api/v1/things",
            Some(100),
        ));
        let mut file_results = HashMap::from([
            ("client.ts".to_string(), client),
            ("consumer.ts".to_string(), consumer),
        ]);

        let stamped = FileOrchestrator::stamp_unfollowed_member_sites(
            &mut file_results,
            &HashMap::from([("listThings".to_string(), 2)]),
            &HashMap::from([(
                "consumer.ts".to_string(),
                HashMap::from([(100, "listThings".to_string())]),
            )]),
            &HashMap::from([(
                "listThings".to_string(),
                MemberHome {
                    path_str: "client.ts".to_string(),
                    request_line: 18,
                },
            )]),
        );

        assert_eq!(stamped, 2, "the resolved site's row and the member's own");
        let expected = Some(UnfollowedMemberSites {
            member: "listThings".to_string(),
            count: 2,
        });
        assert_eq!(
            file_results["consumer.ts"].data_calls[0].consumers_not_resolved,
            expected
        );
        assert_eq!(
            file_results["client.ts"].data_calls[0].consumers_not_resolved,
            expected
        );
    }

    /// A member nothing was lost for writes nothing: an absent field says the
    /// scan counted nothing, and a zero would read as a completeness claim the
    /// count cannot make.
    #[test]
    fn stamp_unfollowed_member_sites_never_writes_a_zero() {
        let mut consumer = FileAnalysisResult::default();
        consumer.data_calls.push(call_with_span(
            4,
            "${this.baseUrl}/api/v1/things",
            Some(100),
        ));
        let mut file_results = HashMap::from([("consumer.ts".to_string(), consumer)]);

        let stamped = FileOrchestrator::stamp_unfollowed_member_sites(
            &mut file_results,
            &HashMap::from([("listThings".to_string(), 0)]),
            &HashMap::from([(
                "consumer.ts".to_string(),
                HashMap::from([(100, "listThings".to_string())]),
            )]),
            &HashMap::new(),
        );

        assert_eq!(stamped, 0);
        assert!(
            file_results["consumer.ts"].data_calls[0]
                .consumers_not_resolved
                .is_none()
        );
    }

    /// carrick#623: `apply_imported_members` can only rewrite a row that
    /// already exists, so a resolved member whose site extraction answered
    /// nothing for was dropped and the endpoint it reaches was absent from the
    /// index entirely.
    #[test]
    fn merge_imported_member_calls_emits_a_site_extraction_returned_no_row_for() {
        let mut result = FileAnalysisResult::default();
        let mut candidate_map = HashMap::new();
        candidate_map.insert("c1".to_string(), candidate_with_snippet("c1", None));
        let resolved = resolved_member(100, "PUT", "${this.baseUrl}/api/v2/things/${id}");

        let added =
            FileOrchestrator::merge_imported_member_calls(&mut result, &resolved, &candidate_map);

        assert_eq!(added, 1);
        assert_eq!(
            result.data_calls[0].target, "${this.baseUrl}/api/v2/things/${id}",
            "everything the member closes over is kept verbatim"
        );
        assert_eq!(result.data_calls[0].method.as_deref(), Some("PUT"));
        assert_eq!(result.data_calls[0].candidate_id, "imported-member:100-140");
        // The span is the SITE's: it anchors the type sidecar and marks the
        // call candidate-backed downstream.
        assert_eq!(result.data_calls[0].call_expression_span_start, Some(100));
        assert_eq!(result.data_calls[0].call_expression_span_end, Some(140));
        assert_eq!(result.data_calls[0].line_number, 12);
    }

    /// A site extraction DID answer keeps the analyzer's row (which
    /// `apply_imported_members` has already corrected by this point): same
    /// span, same line, or the same path already carried behind a base.
    #[test]
    fn merge_imported_member_calls_skips_sites_extraction_already_answered() {
        let mut answered = data_call_with("c1", "${base}/api/v2/things/${id}", Some("PUT"));
        answered.call_expression_span_start = Some(100);
        let mut result = FileAnalysisResult {
            data_calls: vec![answered],
            ..Default::default()
        };
        let mut candidate_map = HashMap::new();
        candidate_map.insert("c1".to_string(), candidate_with_snippet("c1", None));
        let resolved = resolved_member(100, "PUT", "${this.baseUrl}/api/v2/things/${id}");

        let added =
            FileOrchestrator::merge_imported_member_calls(&mut result, &resolved, &candidate_map);

        assert_eq!(added, 0);
        assert_eq!(result.data_calls.len(), 1);
    }

    /// One candidate map holds route registrations as well as calls, and the
    /// name join is unconstrained where the receiver is a local, so a route
    /// whose verb names an imported request member (`app.get(...)` against a
    /// client's `get`) resolves like any bare call. Rewriting could never
    /// reach it; emitting would invent a consumer for a route the file
    /// DEFINES.
    #[test]
    fn merge_imported_member_calls_leaves_a_route_registration_alone() {
        let mut endpoint = endpoint_with_candidate("/things", "c1");
        endpoint.call_expression_span_start = Some(100);
        let mut result = FileAnalysisResult {
            endpoints: vec![endpoint],
            ..Default::default()
        };
        let mut candidate_map = HashMap::new();
        candidate_map.insert("c1".to_string(), candidate_with_snippet("c1", None));
        let resolved = resolved_member(100, "GET", "${this.baseUrl}/api/v2/things/${id}");

        let added =
            FileOrchestrator::merge_imported_member_calls(&mut result, &resolved, &candidate_map);

        assert_eq!(added, 0);
        assert!(result.data_calls.is_empty());
    }

    /// A request member is an HTTP request, so only an HTTP candidate can be a
    /// site that reaches one. A `client.publish(topic, payload)` sharing a
    /// name with an imported `publish` member joins to nothing.
    #[test]
    fn merge_imported_member_calls_ignores_a_non_http_candidate() {
        let mut result = FileAnalysisResult::default();
        let mut candidate = candidate_with_snippet("c1", None);
        candidate.protocol = crate::operation::Protocol::Pubsub;
        candidate.callee_property = Some("publish".to_string());
        let mut candidate_map = HashMap::new();
        candidate_map.insert("c1".to_string(), candidate);
        let resolved = resolved_member(100, "POST", "${this.baseUrl}/api/v2/events");

        let added =
            FileOrchestrator::merge_imported_member_calls(&mut result, &resolved, &candidate_map);

        assert_eq!(added, 0);
        assert!(result.data_calls.is_empty());
    }

    /// A site that passes a whole-URL env-var binding straight to a request,
    /// with the method stated as a literal in its own options bag.
    fn whole_url_site(binding: &str, method: &str) -> HashMap<String, CandidateTarget> {
        let mut candidate = candidate_with_snippet("c1", Some(binding));
        candidate.callee_object = "fetch".to_string();
        candidate.callee_property = None;
        candidate.request_shape = RequestShapeSignal::Known(WrapperRequestShape {
            method: method.to_string(),
            has_body: Some(true),
        });
        HashMap::from([("c1".to_string(), candidate)])
    }

    fn whole_url_maps(
        binding: &str,
        env_name: &str,
        fallback: &str,
    ) -> (EnvAliasMap, WholeUrlFallbackMap) {
        (
            EnvAliasMap::from([(binding.to_string(), env_name.to_string())]),
            WholeUrlFallbackMap::from([
                (binding.to_string(), fallback.to_string()),
                (env_name.to_string(), fallback.to_string()),
            ]),
        )
    }

    /// carrick#632: `resolve_env_var_aliases` can only rewrite a row that
    /// already exists, so a whole-URL env-var call whose site extraction
    /// answered nothing for was dropped and the endpoint it reaches was absent
    /// from the index entirely.
    #[test]
    fn merge_whole_url_env_calls_emits_a_site_extraction_returned_no_row_for() {
        let mut result = FileAnalysisResult::default();
        let candidate_map = whole_url_site("askUrl", "POST");
        let (aliases, paths) =
            whole_url_maps("askUrl", "SERVICE_ASK_URL", "http://localhost:3939/api/ask");

        let (added, corrected) = FileOrchestrator::merge_whole_url_env_calls(
            &mut result,
            &candidate_map,
            &aliases,
            &paths,
        );

        assert_eq!((added, corrected), (1, 0));
        assert_eq!(
            result.data_calls[0].target, "${process.env.SERVICE_ASK_URL}/api/ask",
            "the env var supplies the origin and the fallback literal the path"
        );
        assert_eq!(
            result.data_calls[0].loopback_default_url.as_deref(),
            Some("http://localhost:3939/api/ask"),
            "the loopback default is what the canonical key is computed from"
        );
        assert_eq!(result.data_calls[0].method.as_deref(), Some("POST"));
        assert_eq!(result.data_calls[0].candidate_id, "whole-url-env:100-140");
        // The span is the SITE's: it anchors the type sidecar and marks the
        // call candidate-backed downstream.
        assert_eq!(result.data_calls[0].call_expression_span_start, Some(100));
        assert_eq!(result.data_calls[0].call_expression_span_end, Some(140));
        assert_eq!(result.data_calls[0].line_number, 12);
        assert_eq!(result.data_calls[0].pattern_matched, "fetch");
    }

    /// carrick#632 (live shape): extraction DOES answer this site often enough,
    /// paraphrasing the binding as `${SERVICE_ASK_URL}/api/ask`. #633 read that
    /// row as covering the site and emitted nothing, so the resolved target
    /// never reached the index and the call stayed keyed on an env-var origin.
    /// The row is this call — it carries the candidate's own span — so it is
    /// corrected rather than left.
    #[test]
    fn merge_whole_url_env_calls_corrects_the_row_extraction_answered_for_the_site() {
        let mut answered = data_call_with("c1", "${SERVICE_ASK_URL}/api/ask", Some("POST"));
        answered.call_expression_span_start = Some(100);
        let mut result = FileAnalysisResult {
            data_calls: vec![answered],
            ..Default::default()
        };
        let candidate_map = whole_url_site("askUrl", "POST");
        let (aliases, paths) =
            whole_url_maps("askUrl", "SERVICE_ASK_URL", "http://localhost:3939/api/ask");

        let (added, corrected) = FileOrchestrator::merge_whole_url_env_calls(
            &mut result,
            &candidate_map,
            &aliases,
            &paths,
        );

        assert_eq!((added, corrected), (0, 1), "one call, one row");
        assert_eq!(result.data_calls.len(), 1);
        assert_eq!(
            result.data_calls[0].target, "${process.env.SERVICE_ASK_URL}/api/ask",
            "the spelling the rest of the pipeline resolves env vars through"
        );
        assert_eq!(
            result.data_calls[0].loopback_default_url.as_deref(),
            Some("http://localhost:3939/api/ask")
        );
    }

    /// A row already carrying everything the AST states is left exactly as it
    /// is: correcting is not rewriting for its own sake.
    #[test]
    fn merge_whole_url_env_calls_leaves_an_already_correct_row_alone() {
        let mut answered =
            data_call_with("c1", "${process.env.SERVICE_ASK_URL}/api/ask", Some("POST"));
        answered.call_expression_span_start = Some(100);
        answered.loopback_default_url = Some("http://localhost:3939/api/ask".to_string());
        let mut result = FileAnalysisResult {
            data_calls: vec![answered],
            ..Default::default()
        };
        let candidate_map = whole_url_site("askUrl", "POST");
        let (aliases, paths) =
            whole_url_maps("askUrl", "SERVICE_ASK_URL", "http://localhost:3939/api/ask");

        let (added, corrected) = FileOrchestrator::merge_whole_url_env_calls(
            &mut result,
            &candidate_map,
            &aliases,
            &paths,
        );

        assert_eq!((added, corrected), (0, 0));
        assert_eq!(result.data_calls.len(), 1);
    }

    /// A row on the same line with a span of its OWN is a different call — a
    /// second request on the line raises its own candidate — so it covers the
    /// site and neither corrects it nor suppresses the emission for it.
    #[test]
    fn merge_whole_url_env_calls_leaves_a_different_call_on_the_same_line_alone() {
        let mut sibling = data_call_with("c2", "${process.env.OTHER_URL}/api/other", Some("GET"));
        sibling.call_expression_span_start = Some(200);
        sibling.line_number = 12;
        let mut result = FileAnalysisResult {
            data_calls: vec![sibling],
            ..Default::default()
        };
        let candidate_map = whole_url_site("askUrl", "POST");
        let (aliases, paths) =
            whole_url_maps("askUrl", "SERVICE_ASK_URL", "http://localhost:3939/api/ask");

        let (added, corrected) = FileOrchestrator::merge_whole_url_env_calls(
            &mut result,
            &candidate_map,
            &aliases,
            &paths,
        );

        assert_eq!((added, corrected), (0, 0));
        assert_eq!(
            result.data_calls[0].target, "${process.env.OTHER_URL}/api/other",
            "the sibling call is untouched"
        );
    }

    /// A fallback on a third-party origin states nothing about this machine, so
    /// the call keeps the verbatim env-var key an undeclared base gets.
    #[test]
    fn merge_whole_url_env_calls_keys_a_third_party_fallback_verbatim() {
        let mut result = FileAnalysisResult::default();
        let candidate_map = whole_url_site("askUrl", "POST");
        let (aliases, paths) = whole_url_maps(
            "askUrl",
            "SERVICE_ASK_URL",
            "https://api.example.com/v1/ask",
        );

        let (added, corrected) = FileOrchestrator::merge_whole_url_env_calls(
            &mut result,
            &candidate_map,
            &aliases,
            &paths,
        );

        assert_eq!((added, corrected), (1, 0));
        assert_eq!(
            result.data_calls[0].target, "${process.env.SERVICE_ASK_URL}/v1/ask",
            "the path still comes from the fallback"
        );
        assert_eq!(
            result.data_calls[0].loopback_default_url, None,
            "no loopback default, so nothing to key on but the env var"
        );
    }

    /// The binding is passed to plenty of things that are not requests
    /// (`new URL(url)`, a logger). Joining on the binding name alone would
    /// invent a call at every one of them, so a candidate the AST does not
    /// read as a request is left alone.
    #[test]
    fn merge_whole_url_env_calls_ignores_a_site_that_is_not_a_request() {
        let mut result = FileAnalysisResult::default();
        let mut candidate_map = whole_url_site("askUrl", "POST");
        candidate_map
            .get_mut("c1")
            .expect("candidate")
            .request_shape = RequestShapeSignal::NotARequest;
        let (aliases, paths) =
            whole_url_maps("askUrl", "SERVICE_ASK_URL", "http://localhost:3939/api/ask");

        let (added, corrected) = FileOrchestrator::merge_whole_url_env_calls(
            &mut result,
            &candidate_map,
            &aliases,
            &paths,
        );

        assert_eq!((added, corrected), (0, 0));
        assert!(result.data_calls.is_empty());
    }

    /// A request whose method is not a literal (`fetch(url, { headers })`, or
    /// a parameterized `{ method }`) would be emitted with a guessed verb.
    /// That shape stays extraction's to answer.
    #[test]
    fn merge_whole_url_env_calls_ignores_a_site_with_no_readable_method() {
        let mut result = FileAnalysisResult::default();
        let mut candidate_map = whole_url_site("askUrl", "POST");
        candidate_map
            .get_mut("c1")
            .expect("candidate")
            .request_shape = RequestShapeSignal::Unreadable;
        let (aliases, paths) =
            whole_url_maps("askUrl", "SERVICE_ASK_URL", "http://localhost:3939/api/ask");

        let (added, corrected) = FileOrchestrator::merge_whole_url_env_calls(
            &mut result,
            &candidate_map,
            &aliases,
            &paths,
        );

        assert_eq!((added, corrected), (0, 0));
        assert!(result.data_calls.is_empty());
    }

    /// One candidate map holds route registrations as well as calls. A route
    /// mounted at a whole URL the environment supplies is a route the file
    /// DEFINES, not a call it makes.
    #[test]
    fn merge_whole_url_env_calls_leaves_a_route_registration_alone() {
        let mut endpoint = endpoint_with_candidate("/api/ask", "c1");
        endpoint.call_expression_span_start = Some(100);
        let mut result = FileAnalysisResult {
            endpoints: vec![endpoint],
            ..Default::default()
        };
        let candidate_map = whole_url_site("askUrl", "POST");
        let (aliases, paths) =
            whole_url_maps("askUrl", "SERVICE_ASK_URL", "http://localhost:3939/api/ask");

        let (added, corrected) = FileOrchestrator::merge_whole_url_env_calls(
            &mut result,
            &candidate_map,
            &aliases,
            &paths,
        );

        assert_eq!((added, corrected), (0, 0));
        assert!(result.data_calls.is_empty());
    }

    /// A target that states a path of its own is the base-plus-path shape,
    /// which already resolves. The whole-URL rule must not fire on it, or the
    /// path at the call site would be replaced by the fallback's.
    #[test]
    fn merge_whole_url_env_calls_leaves_a_base_plus_path_site_alone() {
        let mut result = FileAnalysisResult::default();
        let candidate_map = whole_url_site("`${askUrl}/api/other`", "POST");
        let (aliases, paths) =
            whole_url_maps("askUrl", "SERVICE_ASK_URL", "http://localhost:3939/api/ask");

        let (added, corrected) = FileOrchestrator::merge_whole_url_env_calls(
            &mut result,
            &candidate_map,
            &aliases,
            &paths,
        );

        assert_eq!((added, corrected), (0, 0));
        assert!(result.data_calls.is_empty());
    }

    /// An OpenAPI-style path the model copied verbatim states the same route as
    /// the spec, so it is normalized in place rather than stripped of the base.
    #[test]
    fn request_spec_normalizes_a_target_written_with_openapi_params() {
        let mut result = FileAnalysisResult {
            data_calls: vec![data_call_with(
                "c1",
                "${API_URL}/v1/sessions/{sessionId}/release",
                Some("POST"),
            )],
            ..Default::default()
        };
        let mut candidate_map = HashMap::new();
        candidate_map.insert(
            "c1".to_string(),
            verb_call_candidate("c1", "POST", "/v1/sessions/:sessionId/release"),
        );

        FileOrchestrator::apply_candidate_map(&mut result, &candidate_map, "src/client.ts");

        assert_eq!(
            result.data_calls[0].target,
            "${API_URL}/v1/sessions/:sessionId/release"
        );
    }

    /// `collect_pubsub_type_requests` walks a `HashMap<String, _>`, whose
    /// iteration order is non-deterministic. The scanner's output determinism
    /// depends on the emitted `SymbolRequest` order being stable, so this asserts
    /// that several ops spread across multiple files come back in the same order
    /// every call (we walk the file keys sorted).
    #[test]
    fn collect_pubsub_type_requests_is_deterministic() {
        use crate::agents::file_analyzer_agent::PubsubOperation;
        use crate::operation::PubsubRole;

        let agent_service = AgentService::new();
        let orchestrator = FileOrchestrator::new(agent_service);

        let pubsub_op = |topic: &str, role: PubsubRole, symbol: &str| PubsubOperation {
            topic: topic.to_string(),
            role: Some(role),
            line_number: 1,
            primary_type_symbol: Some(symbol.to_string()),
            type_import_source: None,
            broker: None,
            payload_expression_text: None,
            payload_expression_line: None,
        };

        // Three files, each contributing a typed op. The HashMap insertion order
        // intentionally differs from the sorted key order.
        let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();
        file_results.insert(
            "src/zeta.ts".to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![pubsub_op(
                    "orders.created",
                    PubsubRole::Publisher,
                    "OrderCreated",
                )],
                ..Default::default()
            },
        );
        file_results.insert(
            "src/alpha.ts".to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![pubsub_op(
                    "users.signedup",
                    PubsubRole::Subscriber,
                    "UserSignedUp",
                )],
                ..Default::default()
            },
        );
        file_results.insert(
            "src/mid.ts".to_string(),
            FileAnalysisResult {
                pubsub_operations: vec![pubsub_op(
                    "page.viewed",
                    PubsubRole::Subscriber,
                    "PageViewEvent",
                )],
                ..Default::default()
            },
        );

        // SymbolRequest has no PartialEq, so compare by its identifying fields.
        let order = |reqs: &[SymbolRequest]| -> Vec<(String, String, Option<String>)> {
            reqs.iter()
                .map(|r| {
                    (
                        r.symbol_name.clone(),
                        r.source_file.clone(),
                        r.alias.clone(),
                    )
                })
                .collect()
        };

        let first = orchestrator.collect_pubsub_type_requests(&file_results, ".");
        let second = orchestrator.collect_pubsub_type_requests(&file_results, ".");

        assert_eq!(first.len(), 3, "every typed op should anchor a request");
        assert_eq!(
            order(&first),
            order(&second),
            "collect_pubsub_type_requests must emit a stable SymbolRequest order"
        );
        // Order follows the sorted file keys: alpha < mid < zeta.
        let symbols: Vec<&str> = first.iter().map(|r| r.symbol_name.as_str()).collect();
        assert_eq!(
            symbols,
            vec!["UserSignedUp", "PageViewEvent", "OrderCreated"]
        );
    }

    // ---- #413 pub/sub borrow witness ----

    /// One file exercising every witness verdict: `evt: OrderPlaced` is the
    /// annotated payload, `record: AuditRecord` is the borrow source, and
    /// `envelope: Envelope<OrderPlaced>` is the wrapper whose annotation
    /// MENTIONS the inner contract type.
    fn write_witness_fixture(dir: &std::path::Path) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/pub.ts"),
            r#"
import type { OrderPlaced, Envelope, AuditRecord } from "./types";

export function handleOrder(evt: OrderPlaced): void {
  void evt;
}

export function auditIt(record: AuditRecord): void {
  void record;
}

export function publishWrapped(order: OrderPlaced): void {
  const envelope: Envelope<OrderPlaced> = { v: 1, data: order };
  void envelope;
}
"#,
        )
        .unwrap();
    }

    fn witness_requests(
        dir: &std::path::Path,
        ops: Vec<crate::agents::file_analyzer_agent::PubsubOperation>,
    ) -> Vec<crate::services::type_sidecar::SymbolRequest> {
        let mut file_results = HashMap::new();
        file_results.insert("src/pub.ts".to_string(), pubsub_only_result(ops));
        let orchestrator = FileOrchestrator::new(AgentService::new());
        orchestrator.collect_pubsub_type_requests(&file_results, dir.to_str().unwrap())
    }

    /// The witness fires exactly when the payload binding's annotation
    /// contradicts the emitted symbol AND the symbol is anchored to a
    /// different binding — and stays off in every agreement, wrapper-mention,
    /// unannotated, non-bare-locator, and envelope-copy shape.
    #[test]
    fn test_pubsub_borrow_witness_verdicts() {
        use crate::operation::PubsubRole;

        let dir = tempfile::tempdir().unwrap();
        write_witness_fixture(dir.path());

        let sub = |topic: &str, symbol: &str, locator: Option<&str>| {
            pubsub_op(topic, PubsubRole::Subscriber, Some(symbol), None, locator)
        };
        let requests = witness_requests(
            dir.path(),
            vec![
                // Borrow: payload `evt: OrderPlaced` never mentions
                // AuditRecord, and AuditRecord annotates `record`.
                sub("t.borrow", "AuditRecord", Some("evt")),
                // Agreement: the symbol IS the payload annotation.
                sub("t.agree", "OrderPlaced", Some("evt")),
                // Wrapper mention: `envelope: Envelope<OrderPlaced>` mentions
                // the inner contract type at depth — never a borrow, even
                // though the primary symbol of the annotation is `Envelope`.
                sub("t.wrapped", "OrderPlaced", Some("envelope")),
                // Unannotated/unknown payload binding: proves nothing.
                sub("t.unannotated", "AuditRecord", Some("mystery")),
                // Symbol anchored to no other binding: no borrow source.
                sub("t.ghost", "GhostType", Some("evt")),
                // Non-bare-ident locator: the infer path can't be attributed
                // to a binding, so no witness.
                sub("t.encoded", "AuditRecord", Some("jc.encode(evt)")),
                // Envelope-copy locator (contains the topic): the infer
                // collector drops it, so there is no second anchor to
                // arbitrate against.
                sub(
                    "t.copy",
                    "AuditRecord",
                    Some("{ topic: \"t.copy\", data: evt }"),
                ),
                // No locator at all.
                sub("t.nolocator", "AuditRecord", None),
            ],
        );

        // Every op carries a symbol, so all eight anchor a request, in op
        // order (a single file's ops walk in vec order).
        let witnesses: Vec<bool> = requests.iter().map(|r| r.payload_borrow_witness).collect();
        assert_eq!(
            witnesses,
            vec![
                true,  // t.borrow: witnessed borrow
                false, // t.agree: agreement
                false, // t.wrapped: wrapper annotation mentions the symbol
                false, // t.unannotated: payload binding not annotated
                false, // t.ghost: symbol anchored to no other binding
                false, // t.encoded: non-bare-ident locator
                false, // t.copy: envelope-copy locator (contains the topic)
                false, // t.nolocator: no locator at all
            ],
            "requests: {requests:?}"
        );
    }

    /// An unparseable (or absent) file yields no annotation evidence, so the
    /// witness fails closed to `false` — never blocking the explicit path.
    #[test]
    fn test_pubsub_borrow_witness_fails_closed_without_a_parseable_file() {
        use crate::operation::PubsubRole;

        let dir = tempfile::tempdir().unwrap();
        // No src/pub.ts on disk at all.
        let requests = witness_requests(
            dir.path(),
            vec![pubsub_op(
                "t.borrow",
                PubsubRole::Subscriber,
                Some("AuditRecord"),
                None,
                Some("evt"),
            )],
        );
        assert_eq!(requests.len(), 1);
        assert!(!requests[0].payload_borrow_witness);
    }

    /// Pure-map witness check: qualified symbols compare by rightmost ident.
    #[test]
    fn test_pubsub_borrow_witness_qualified_symbol_leaf() {
        let mut mentions: HashMap<String, HashSet<String>> = HashMap::new();
        mentions.insert(
            "evt".to_string(),
            ["OrderPlaced".to_string()].into_iter().collect(),
        );
        mentions.insert(
            "record".to_string(),
            ["AuditRecord".to_string()].into_iter().collect(),
        );
        assert!(pubsub_payload_borrow_witness(
            &mentions,
            "evt",
            "api.AuditRecord"
        ));
        assert!(!pubsub_payload_borrow_witness(
            &mentions,
            "evt",
            "api.OrderPlaced"
        ));
    }

    // ---- #361 deterministic extraction-flake guards ----

    /// Minimal `DataCallResult` builder for the guard tests.
    fn guard_data_call(
        line_number: i32,
        target: &str,
        call_expression_text: Option<&str>,
        payload_expression_text: Option<&str>,
        primary_type_symbol: Option<&str>,
        type_import_source: Option<&str>,
    ) -> DataCallResult {
        DataCallResult {
            call_kind: None,
            candidate_id: format!("span:{line_number}"),
            line_number,
            target: target.to_string(),
            method: Some("POST".to_string()),
            pattern_matched: "call".to_string(),
            call_expression_span_start: None,
            call_expression_span_end: None,
            call_expression_text: call_expression_text.map(str::to_string),
            call_expression_line: Some(line_number),
            payload_expression_text: payload_expression_text.map(str::to_string),
            payload_expression_line: Some(line_number),
            primary_type_symbol: primary_type_symbol.map(str::to_string),
            type_import_source: type_import_source.map(str::to_string),
            loopback_default_url: None,
            base: None,
            consumers_not_resolved: None,
        }
    }

    fn result_with_data_calls(data_calls: Vec<DataCallResult>) -> FileAnalysisResult {
        FileAnalysisResult {
            graphql_consumer_locates: vec![],
            mounts: vec![],
            endpoints: vec![],
            data_calls,
            graphql_operations: vec![],
            pubsub_operations: vec![],
        }
    }

    #[test]
    fn test_suppress_borrowed_request_type_nulls_request_only_symbol() {
        // `event: AuditEvent` is the REQUEST payload; the call has no response
        // annotation, so `AuditEvent` in the response slot is a borrow. A second
        // call carries an explicit `<AuditEvent>` generic — a real response
        // annotation that must survive.
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("audit.ts");
        std::fs::write(
            &file_path,
            r#"
import axios from "axios";
export interface AuditEvent { paymentId: string; }
export async function recordAuditEvent(event: AuditEvent): Promise<void> {
  await axios.post("http://localhost:3099/audit/events", event);
  await axios.post<AuditEvent>("http://localhost:3099/audit/echo", event);
}
"#,
        )
        .unwrap();

        let mut result = result_with_data_calls(vec![
            // Borrow: no response evidence -> must be nulled.
            guard_data_call(
                5,
                "/audit/events",
                Some(r#"axios.post("http://localhost:3099/audit/events", event)"#),
                Some("event"),
                Some("AuditEvent"),
                Some("./types"),
            ),
            // Explicit call generic -> a real response annotation -> kept.
            guard_data_call(
                6,
                "/audit/echo",
                Some(r#"axios.post<AuditEvent>("http://localhost:3099/audit/echo", event)"#),
                Some("event"),
                Some("AuditEvent"),
                Some("./types"),
            ),
        ]);

        FileOrchestrator::suppress_borrowed_request_types(&mut result, &file_path);

        // Borrow suppressed (both symbol and its import source).
        assert_eq!(result.data_calls[0].primary_type_symbol, None);
        assert_eq!(result.data_calls[0].type_import_source, None);
        // Explicitly annotated response kept.
        assert_eq!(
            result.data_calls[1].primary_type_symbol.as_deref(),
            Some("AuditEvent")
        );
        assert_eq!(
            result.data_calls[1].type_import_source.as_deref(),
            Some("./types")
        );
    }

    #[test]
    fn test_suppress_borrowed_request_type_keeps_annotated_result_binding() {
        // A file where the SAME type is legitimately annotated on a
        // call-initialized binding (`const echoed: AuditEvent = await ...`):
        // that is response-side evidence, so the shared type must be kept.
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("echo.ts");
        std::fs::write(
            &file_path,
            r#"
import axios from "axios";
export interface AuditEvent { paymentId: string; }
export async function echoAudit(event: AuditEvent): Promise<AuditEvent> {
  const echoed: AuditEvent = await axios.post("http://localhost:3099/audit/echo", event);
  return echoed;
}
"#,
        )
        .unwrap();

        let mut result = result_with_data_calls(vec![guard_data_call(
            5,
            "/audit/echo",
            Some(r#"axios.post("http://localhost:3099/audit/echo", event)"#),
            Some("event"),
            Some("AuditEvent"),
            Some("./types"),
        )]);

        FileOrchestrator::suppress_borrowed_request_types(&mut result, &file_path);

        assert_eq!(
            result.data_calls[0].primary_type_symbol.as_deref(),
            Some("AuditEvent"),
            "annotated result binding is response evidence; type must not be nulled"
        );
    }

    #[test]
    fn test_suppress_borrowed_request_type_keeps_wrapped_result_binding() {
        // The result binding's annotation wraps the shared type in a generic
        // envelope (`const r: Response<AuditEvent> = await ...`). Evidence is
        // mention-based (any depth, no wrapper-name allowlist), so the symbol
        // must be kept exactly like the bare-annotation case.
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("wrapped.ts");
        std::fs::write(
            &file_path,
            r#"
import axios from "axios";
export interface AuditEvent { paymentId: string; }
export interface Response<T> { data: T; status: number; }
export async function echoAudit(event: AuditEvent): Promise<AuditEvent> {
  const r: Response<AuditEvent> = await axios.post("http://localhost:3099/audit/echo", event);
  return r.data;
}
"#,
        )
        .unwrap();

        let mut result = result_with_data_calls(vec![guard_data_call(
            6,
            "/audit/echo",
            Some(r#"axios.post("http://localhost:3099/audit/echo", event)"#),
            Some("event"),
            Some("AuditEvent"),
            Some("./types"),
        )]);

        FileOrchestrator::suppress_borrowed_request_types(&mut result, &file_path);

        assert_eq!(
            result.data_calls[0].primary_type_symbol.as_deref(),
            Some("AuditEvent"),
            "a generic-wrapped result annotation is response evidence; type must not be nulled"
        );
    }

    #[test]
    fn test_suppress_borrowed_request_type_ignores_non_identifier_payload() {
        // An object-literal payload has no resolvable binding type, so the row
        // is untouched even though the symbol is set (we never guess).
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("obj.ts");
        std::fs::write(
            &file_path,
            r#"
import axios from "axios";
export interface AuditEvent { paymentId: string; }
export async function send(): Promise<void> {
  await axios.post("http://localhost:3099/audit/events", { paymentId: "1" });
}
"#,
        )
        .unwrap();

        let mut result = result_with_data_calls(vec![guard_data_call(
            5,
            "/audit/events",
            Some(r#"axios.post("http://localhost:3099/audit/events", { paymentId: "1" })"#),
            Some(r#"{ paymentId: "1" }"#),
            Some("AuditEvent"),
            Some("./types"),
        )]);

        FileOrchestrator::suppress_borrowed_request_types(&mut result, &file_path);

        assert_eq!(
            result.data_calls[0].primary_type_symbol.as_deref(),
            Some("AuditEvent")
        );
    }

    #[test]
    fn test_rewrite_graphql_document_target_from_transport_url() {
        // `client.request(TICKET_QUERY, ...)` over a shared endpoint: the model
        // reported the transport URL as target. The document identity resolves
        // to the exact canonical operation key the matcher joins on.
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("support.ts");
        std::fs::write(
            &file_path,
            r#"
import { gql } from "graphql-tag";
import { GraphQLClient } from "graphql-request";
const client = new GraphQLClient(process.env.SUPPORT_GQL_URL ?? "http://localhost:4005/graphql");
const TICKET_QUERY = gql`
  query ticket($id: ID!) {
    ticket(id: $id) {
      id
      subject
      status
    }
  }
`;
export async function loadTicket(id: string) {
  const data = await client.request(TICKET_QUERY, { id });
  return data;
}
"#,
        )
        .unwrap();

        let mut result = result_with_data_calls(vec![
            // Transport-URL target dispatching a known gql document -> rewritten.
            guard_data_call(
                16,
                "`${SUPPORT_GQL_URL}/graphql`",
                Some("client.request(TICKET_QUERY, { id })"),
                None,
                None,
                None,
            ),
            // A real HTTP call in the same file naming no gql document -> untouched.
            guard_data_call(
                20,
                "`${API_BASE}/orders`",
                Some("fetch(`${API_BASE}/orders`)"),
                None,
                None,
                None,
            ),
        ]);

        FileOrchestrator::rewrite_graphql_document_targets(&mut result, &file_path);

        assert_eq!(
            result.data_calls[0].target, "graphql|query|ticket",
            "URL target must be rewritten to the exact canonical operation key"
        );
        assert_eq!(
            result.data_calls[1].target, "`${API_BASE}/orders`",
            "a non-graphql transport call must be left untouched"
        );
    }

    #[test]
    fn test_rewrite_graphql_document_target_trims_quoted_transport_url() {
        // The model sometimes emits the target with its source quoting intact
        // (`"https://…/graphql"`, backticked template). The quote/backtick trim
        // (mirroring fold_graphql_transport_calls) must not let a quoted URL
        // escape the transport-shape check.
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("support.ts");
        std::fs::write(
            &file_path,
            r#"
import { gql } from "graphql-tag";
import { GraphQLClient } from "graphql-request";
const client = new GraphQLClient("https://support.example.com/graphql");
const TICKET_QUERY = gql`
  query ticket($id: ID!) {
    ticket(id: $id) {
      id
    }
  }
`;
export async function loadTicket(id: string) {
  return client.request(TICKET_QUERY, { id });
}
"#,
        )
        .unwrap();

        let mut result = result_with_data_calls(vec![
            // Double-quoted absolute URL -> still transport-shaped -> rewritten.
            guard_data_call(
                13,
                r#""https://support.example.com/graphql""#,
                Some("client.request(TICKET_QUERY, { id })"),
                None,
                None,
                None,
            ),
            // Backticked absolute URL (no `${}`) -> same.
            guard_data_call(
                13,
                "`https://support.example.com/graphql`",
                Some("client.request(TICKET_QUERY, { id })"),
                None,
                None,
                None,
            ),
        ]);

        FileOrchestrator::rewrite_graphql_document_targets(&mut result, &file_path);

        assert_eq!(
            result.data_calls[0].target, "graphql|query|ticket",
            "a double-quoted transport URL must be rewritten"
        );
        assert_eq!(
            result.data_calls[1].target, "graphql|query|ticket",
            "a backticked transport URL must be rewritten"
        );
    }

    #[test]
    fn test_document_operation_keys_exposes_canonical_form() {
        // Guards the exact key form pattern 3 rewrites to against drift.
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("gql.ts");
        std::fs::write(
            &file_path,
            r#"
import { gql } from "graphql-tag";
const ESCALATE_MUTATION = gql`
  mutation escalateTicket($id: ID!, $reason: String!) {
    escalateTicket(id: $id, reason: $reason) {
      ticketId
    }
  }
`;
"#,
        )
        .unwrap();

        let keys = crate::graphql::document_operation_keys(&file_path);
        assert_eq!(
            keys.get("ESCALATE_MUTATION").map(String::as_str),
            Some("graphql|mutation|escalateTicket")
        );
    }

    /// carrick#387: a deterministically-anchored payload-less pub/sub op the
    /// LLM extraction missed is backfilled with all judgment fields `None`;
    /// an anchor is covered — and skipped — exactly when the extraction
    /// already carries its (topic, role) contribution. Line numbers play no
    /// part in coverage: a line is not an operation identity.
    #[test]
    fn merge_pubsub_anchor_ops_backfills_only_missed_ops() {
        use crate::agents::file_analyzer_agent::PubsubOperation;
        use crate::operation::PubsubRole;
        use crate::swc_scanner::PubsubAnchorOp;

        let llm_op = |topic: &str, line: i32| PubsubOperation {
            topic: topic.to_string(),
            role: Some(PubsubRole::Publisher),
            line_number: line,
            primary_type_symbol: None,
            type_import_source: None,
            broker: None,
            payload_expression_text: None,
            payload_expression_line: None,
        };

        let mut result = FileAnalysisResult {
            pubsub_operations: vec![
                // Covers anchor (a): identical (topic, role) at the same site.
                llm_op("PollController:pollingStarted", 182),
                // Covers anchor (b) by (topic, role) at a different line — the
                // topic is already on the publisher side, one op per side is
                // all the matching join needs.
                llm_op("PollController:stateChange", 90),
            ],
            ..Default::default()
        };

        let anchors = vec![
            // (a) same (topic, role) as an extracted op -> skipped.
            PubsubAnchorOp {
                topic: "PollController:pollingStarted".to_string(),
                role: PubsubRole::Publisher,
                line_number: 182,
                handler_param: None,
                handler_param_line: None,
            },
            // (b) same (topic, role) as an extracted op at another line -> skipped.
            PubsubAnchorOp {
                topic: "PollController:stateChange".to_string(),
                role: PubsubRole::Publisher,
                line_number: 95,
                handler_param: None,
                handler_param_line: None,
            },
            // (c) genuinely missed -> backfilled.
            PubsubAnchorOp {
                topic: "PollController:pollingStopped".to_string(),
                role: PubsubRole::Publisher,
                line_number: 201,
                handler_param: None,
                handler_param_line: None,
            },
        ];

        let added = FileOrchestrator::merge_pubsub_anchor_ops(&mut result, anchors);
        assert_eq!(added, 1, "only the missed anchor must be backfilled");
        assert_eq!(result.pubsub_operations.len(), 3);

        let backfilled = result
            .pubsub_operations
            .iter()
            .find(|op| op.topic == "PollController:pollingStopped")
            .expect("missed anchor must be present after the merge");
        assert_eq!(backfilled.role, Some(PubsubRole::Publisher));
        assert_eq!(backfilled.line_number, 201);
        assert_eq!(backfilled.primary_type_symbol, None);
        assert_eq!(backfilled.type_import_source, None);
        assert_eq!(backfilled.broker, None);
        assert_eq!(backfilled.payload_expression_text, None);
        assert_eq!(backfilled.payload_expression_line, None);
    }

    /// Copilot review on #389: an extracted op on the same LINE must not mask
    /// a missing (topic, role) contribution — a line can carry several pub/sub
    /// ops (`bus.publish('a'); bus.subscribe('b')` minified onto one line), and
    /// the extraction may have produced only one of them, or one with a
    /// different topic entirely (e.g. a template kept verbatim). Coverage is
    /// keyed on (topic, role) alone.
    #[test]
    fn merge_pubsub_anchor_ops_same_line_does_not_mask_missing_ops() {
        use crate::agents::file_analyzer_agent::PubsubOperation;
        use crate::operation::PubsubRole;
        use crate::swc_scanner::PubsubAnchorOp;

        let mut result = FileAnalysisResult {
            pubsub_operations: vec![PubsubOperation {
                topic: "jobs.retry".to_string(),
                role: Some(PubsubRole::Publisher),
                line_number: 7,
                primary_type_symbol: None,
                type_import_source: None,
                broker: None,
                payload_expression_text: None,
                payload_expression_line: None,
            }],
            ..Default::default()
        };

        let anchors = vec![
            // Publisher+subscriber share line 7; only the publisher was
            // extracted -> the subscriber must backfill despite the shared line.
            PubsubAnchorOp {
                topic: "jobs.retry".to_string(),
                role: PubsubRole::Publisher,
                line_number: 7,
                handler_param: None,
                handler_param_line: None,
            },
            PubsubAnchorOp {
                topic: "jobs.completed".to_string(),
                role: PubsubRole::Subscriber,
                line_number: 7,
                handler_param: None,
                handler_param_line: None,
            },
            // Same line, same role, DIFFERENT topic (the extraction kept a
            // template verbatim, say) -> the resolved-topic anchor must
            // backfill; the resolved literal is the joinable key.
            PubsubAnchorOp {
                topic: "jobs.failed".to_string(),
                role: PubsubRole::Publisher,
                line_number: 7,
                handler_param: None,
                handler_param_line: None,
            },
        ];

        let added = FileOrchestrator::merge_pubsub_anchor_ops(&mut result, anchors);
        assert_eq!(
            added, 2,
            "line-sharing must not mask the subscriber or the different-topic anchor"
        );
        assert!(
            result
                .pubsub_operations
                .iter()
                .any(|op| op.topic == "jobs.completed"
                    && op.role == Some(PubsubRole::Subscriber)
                    && op.line_number == 7),
            "subscriber sharing the publisher's line must be backfilled"
        );
        assert!(
            result
                .pubsub_operations
                .iter()
                .any(|op| op.topic == "jobs.failed" && op.role == Some(PubsubRole::Publisher)),
            "different-topic anchor on the same line must be backfilled"
        );
    }

    /// Same topic on the OTHER side is not coverage: a publisher extraction
    /// does not cover a subscriber anchor for the same topic.
    #[test]
    fn merge_pubsub_anchor_ops_role_disambiguates_coverage() {
        use crate::agents::file_analyzer_agent::PubsubOperation;
        use crate::operation::PubsubRole;
        use crate::swc_scanner::PubsubAnchorOp;

        let mut result = FileAnalysisResult {
            pubsub_operations: vec![PubsubOperation {
                topic: "jobs.retry".to_string(),
                role: Some(PubsubRole::Publisher),
                line_number: 10,
                primary_type_symbol: None,
                type_import_source: None,
                broker: None,
                payload_expression_text: None,
                payload_expression_line: None,
            }],
            ..Default::default()
        };

        let added = FileOrchestrator::merge_pubsub_anchor_ops(
            &mut result,
            vec![PubsubAnchorOp {
                topic: "jobs.retry".to_string(),
                role: PubsubRole::Subscriber,
                line_number: 25,
                handler_param: None,
                handler_param_line: None,
            }],
        );
        assert_eq!(added, 1);
        assert!(
            result
                .pubsub_operations
                .iter()
                .any(|op| op.topic == "jobs.retry" && op.role == Some(PubsubRole::Subscriber)),
            "subscriber anchor must be backfilled alongside the publisher extraction"
        );
    }

    /// carrick#402 shape c: an anchor from the two-arg
    /// `subscribe("topic", (msg) => …)` form carries its inline handler's
    /// first param, and the backfill must land it as the FunctionParam payload
    /// locator (`payload_expression_text`/`_line`) so
    /// `collect_pubsub_infer_requests` routes it through the sidecar. Type
    /// judgment fields stay `None`.
    #[test]
    fn merge_pubsub_anchor_ops_carries_handler_param_locator() {
        use crate::operation::PubsubRole;
        use crate::swc_scanner::PubsubAnchorOp;

        let mut result = FileAnalysisResult::default();
        let added = FileOrchestrator::merge_pubsub_anchor_ops(
            &mut result,
            vec![PubsubAnchorOp {
                topic: "user.created".to_string(),
                role: PubsubRole::Subscriber,
                line_number: 12,
                handler_param: Some("msg".to_string()),
                handler_param_line: Some(12),
            }],
        );
        assert_eq!(added, 1);
        let op = &result.pubsub_operations[0];
        assert_eq!(op.payload_expression_text.as_deref(), Some("msg"));
        assert_eq!(op.payload_expression_line, Some(12));
        assert_eq!(op.primary_type_symbol, None);
        assert_eq!(op.broker, None);
    }

    /// Test-local pub/sub op constructor for the phantom-topic guard tests.
    fn phantom_guard_op(
        topic: &str,
        line: i32,
    ) -> crate::agents::file_analyzer_agent::PubsubOperation {
        crate::agents::file_analyzer_agent::PubsubOperation {
            topic: topic.to_string(),
            role: Some(crate::operation::PubsubRole::Publisher),
            line_number: line,
            primary_type_symbol: None,
            type_import_source: None,
            broker: None,
            payload_expression_text: None,
            payload_expression_line: None,
        }
    }

    /// carrick#311 reproduction: `worker.ts` calls `publishStatusChanged(evt)`
    /// (a wrapper imported from `./status.publisher`) and contains no topic
    /// literal at all, but the analyzer emits a `status.changed` op derived
    /// from the function NAME. No literal witness -> the phantom is dropped.
    #[test]
    fn suppress_phantom_pubsub_topics_drops_wrapper_name_derived_topic() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("worker.ts");
        std::fs::write(
            &file_path,
            r#"
import { publishStatusChanged } from "./status.publisher";
import type { OrderEvent } from "./types";

export async function processOrder(evt: OrderEvent): Promise<void> {
  await publishStatusChanged(evt);
}
"#,
        )
        .unwrap();

        let mut result = FileAnalysisResult {
            pubsub_operations: vec![phantom_guard_op("status.changed", 6)],
            ..Default::default()
        };

        let dropped = FileOrchestrator::suppress_phantom_pubsub_topics(&mut result, &file_path);
        assert_eq!(dropped, 1, "the name-derived phantom topic must be dropped");
        assert!(
            result.pubsub_operations.is_empty(),
            "no op may survive in a file with no topic witness, got {:?}",
            result.pubsub_operations
        );
    }

    /// Every literal-witnessed topic form survives the guard: an inline
    /// string-literal topic, a same-file const-ref topic (the literal lives in
    /// the const initializer), and a template-composed topic the analyzer
    /// resolved from context the AST pre-pass cannot reach (a class-property
    /// interpolation, the MetaMask messenger shape). A phantom mixed into the
    /// same file is still dropped.
    #[test]
    fn suppress_phantom_pubsub_topics_keeps_literal_witnessed_topics() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("controller.ts");
        std::fs::write(
            &file_path,
            r#"
import { bus } from "somebus";

const SUBJECT = "user.registered";

export class PollController {
  private name = "PollController";

  run(payload: object, state: object, kind: string): void {
    bus.publish("orders.created", payload);
    bus.publish(SUBJECT, payload);
    bus.publish(`${this.name}:stateChange`, state);
    bus.publish("orders." + kind + ".changed", payload);
  }
}
"#,
        )
        .unwrap();

        let mut result = FileAnalysisResult {
            pubsub_operations: vec![
                phantom_guard_op("orders.created", 10),
                phantom_guard_op("user.registered", 11),
                phantom_guard_op("PollController:stateChange", 12),
                phantom_guard_op("orders.priority.changed", 13),
                // Invented from nothing in this file -> must be dropped.
                phantom_guard_op("status.changed", 12),
            ],
            ..Default::default()
        };

        let dropped = FileOrchestrator::suppress_phantom_pubsub_topics(&mut result, &file_path);
        assert_eq!(dropped, 1, "only the unwitnessed topic may be dropped");
        let topics: Vec<&str> = result
            .pubsub_operations
            .iter()
            .map(|op| op.topic.as_str())
            .collect();
        assert_eq!(
            topics,
            vec![
                "orders.created",
                "user.registered",
                "PollController:stateChange",
                "orders.priority.changed"
            ],
            "inline, const-ref, template-composed, and concat-composed topics must all survive"
        );
    }

    /// Copilot review on #395: a fully dynamic composition (`` `${x}` ``,
    /// `a + b`) has no static parts and must not act as a match-anything
    /// witness — a phantom topic in a file containing one is still dropped.
    #[test]
    fn suppress_phantom_pubsub_topics_ignores_fully_dynamic_compositions() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("dynamic.ts");
        std::fs::write(
            &file_path,
            r#"
import { publishStatusChanged } from "./status.publisher";

export function run(evt: object, label: string, tail: string): void {
  const rendered = `${label}`;
  const joined = label + tail;
  console.log(rendered, joined);
  publishStatusChanged(evt);
}
"#,
        )
        .unwrap();

        let mut result = FileAnalysisResult {
            pubsub_operations: vec![phantom_guard_op("status.changed", 8)],
            ..Default::default()
        };

        let dropped = FileOrchestrator::suppress_phantom_pubsub_topics(&mut result, &file_path);
        assert_eq!(
            dropped, 1,
            "a fully dynamic template or concat must not witness the phantom"
        );
        assert!(result.pubsub_operations.is_empty());
    }

    /// The template witness anchors on the static parts: a composed topic must
    /// start with the first quasi, contain the middles in order, and end with
    /// the last quasi. A topic that merely shares no shape with any template
    /// is not witnessed.
    #[test]
    fn template_pattern_matches_anchors_static_parts() {
        let leading_interp = vec![String::new(), ":stateChange".to_string()];
        assert!(template_pattern_matches(
            &leading_interp,
            "PollController:stateChange"
        ));
        assert!(!template_pattern_matches(&leading_interp, "status.changed"));

        let trailing_interp = vec!["orders.".to_string(), String::new()];
        assert!(template_pattern_matches(&trailing_interp, "orders.created"));
        assert!(
            !template_pattern_matches(&trailing_interp, "prefix.orders.created"),
            "the first quasi anchors as a prefix"
        );

        let middle_interp = vec![
            "jobs.".to_string(),
            ".retry.".to_string(),
            ".done".to_string(),
        ];
        assert!(template_pattern_matches(
            &middle_interp,
            "jobs.email.retry.3.done"
        ));
        assert!(
            !template_pattern_matches(&middle_interp, "jobs.email.retry.3"),
            "the last quasi anchors as a suffix"
        );
    }
}
