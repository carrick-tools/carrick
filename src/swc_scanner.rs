//! Lightweight SWC Scanner - AST Gatekeeper for file-centric analysis.
//!
//! This module implements the first stage of the AST-Gated architecture:
//! scan files using SWC to find potential API call sites BEFORE sending
//! to the LLM. If no candidates are found, the file is skipped entirely
//! (Cost: $0).
//!
//! The scanner is intentionally broad - it's better to have false positives
//! (which the LLM will filter out) than false negatives (which would cause
//! missed API patterns).
//!
//! Note: Type extraction is now handled by the TypeSidecar (src/sidecar).
//! The legacy TypePositionFinder and related code has been removed as part
//! of the compiler sidecar architecture migration.

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use swc_common::{
    SourceMap, SourceMapper, Spanned,
    errors::{ColorConfig, Handler},
    sync::Lrc,
};
use swc_ecma_ast::*;
use swc_ecma_parser::{EsSyntax, TsSyntax};
use swc_ecma_visit::{Visit, VisitWith};

use crate::local_http_wrapper::{LocalWrapperCall, collect_local_wrapper_calls};
use crate::operation::{Protocol, PubsubRole};
use crate::parser::parse_file;
use crate::type_manifest::is_http_method;
use crate::wrapper_request_shape::{RequestShapeSignal, call_request_shape};

/// A candidate API call site detected by the SWC scanner.
/// This is passed as a "hint" to the LLM to ensure 100% recall.
#[derive(Debug, Clone, Serialize)]
pub struct CandidateTarget {
    /// Protocol family this call site belongs to. Routes the candidate to
    /// that protocol's analyze-file prompt (or skips it when no prompt is
    /// registered). Not serialized: the JSON candidate context the HTTP
    /// prompt receives stays exactly as before.
    #[serde(skip)]
    pub protocol: Protocol,
    /// Stable identifier for this call site within the file
    pub candidate_id: String,
    /// Start byte offset of the call expression
    pub span_start: u32,
    /// End byte offset of the call expression
    pub span_end: u32,
    /// 1-based line number where the call was detected
    pub line_number: usize,
    /// The callee object (e.g., "app", "router", "fetch")
    pub callee_object: String,
    /// The callee property/method (e.g., "get", "post", "use")
    pub callee_property: Option<String>,
    /// Name of the enclosing function (if any)
    pub enclosing_function: Option<String>,
    /// First-argument snippet (e.g., URL/path literal/template). For a
    /// request-spec call (see [`CandidateTarget::request_spec`]) this is the
    /// quoted `url`/`path` literal read off the object, not the object's own
    /// source text — the raw snippet of a multi-line config object is just
    /// `{`, which anchors nothing.
    pub path_snippet: Option<String>,
    /// A snippet of the code at this location
    pub code_snippet: String,
    /// Method and URL read structurally off a single object-literal argument
    /// that carries both (`client({ method: "post", url: "/api/v1/login" })`).
    /// Present only for that shape. Not serialized: the JSON candidate
    /// context the HTTP prompt receives stays exactly as before, and these
    /// facts are used to overrule the model's answer after it replies (#537).
    #[serde(skip)]
    pub request_spec: Option<RequestSpec>,
    /// What this call site says about the request shape of the module it lives
    /// in — the literal HTTP method and body presence, when they are readable
    /// off the AST (carrick-cloud#386). Read only when this module is another
    /// file's imported HTTP wrapper, to resolve the METHOD of the delegating
    /// site the same way #369/#370 resolves its target. Where `request_spec`
    /// reads a call that declares its whole request as data, this reads the
    /// options bag of a call whose URL is an expression — the shape a request
    /// wrapper is built out of. Not serialized, for the same reason.
    #[serde(skip)]
    pub request_shape: RequestShapeSignal,
}

/// The HTTP method and URL a call declares as data on one object-literal
/// argument, rather than positionally. Both are string literals; the URL is
/// route-shaped. Structural: the shape is the signal, no client library is
/// named anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestSpec {
    /// The HTTP verb, upper-cased. Read from the object's `method` literal, or
    /// from the invoked member name when the object carries only a `url` (see
    /// `method_from_callee`).
    pub method: String,
    /// The `url` (or `path`) literal, with OpenAPI-style `{param}` segments
    /// rewritten to the router spelling `:param` (see
    /// [`normalize_path_params`]).
    pub url: String,
    /// True when the verb came from the invoked member name
    /// (`client.post({ url: "/v1/things" })`) rather than a `method` property.
    ///
    /// That form is unambiguously a REQUEST: the verb is the operation being
    /// performed, and `url` is the request-side spelling of the target (a
    /// declarative route registration spells it `path` and carries a handler).
    /// The consumer backfill emits only this form deterministically; a
    /// `{ method, url }` object alone can equally be a producer's route
    /// descriptor, so it stays an anchor for the analyzer's answer.
    pub method_from_callee: bool,
}

/// Rewrite OpenAPI-style path parameters (`/v1/sessions/{sessionId}/release`)
/// to the router spelling the rest of Carrick keys on (`/v1/sessions/:sessionId/release`),
/// so a call written in the OpenAPI spelling joins the producer route that
/// declares the same path.
///
/// Whole segments only: `${BASE}` and `foo{bar}baz` are left alone, so a
/// target that interpolates an env-var base survives untouched.
pub fn normalize_path_params(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for (i, segment) in path.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        let inner = segment
            .strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))
            .map(str::trim)
            .filter(|inner| !inner.is_empty() && !inner.contains(['{', '}']));
        match inner {
            Some(inner) => {
                out.push(':');
                out.push_str(inner);
            }
            None => out.push_str(segment),
        }
    }
    out
}

impl CandidateTarget {
    /// Format as a hint string for the LLM prompt
    pub fn format_hint(&self) -> String {
        let callee = match &self.callee_property {
            Some(prop) => format!("{}.{}", self.callee_object, prop),
            None => self.callee_object.clone(),
        };
        let func = self
            .enclosing_function
            .as_deref()
            .unwrap_or("unknown_function");
        let path = self.path_snippet.as_deref().unwrap_or("<path unavailable>");

        format!(
            "- Candidate {}: Line {} (span {}-{}) {} [fn: {}] [path: {}] - `{}`",
            self.candidate_id,
            self.line_number,
            self.span_start,
            self.span_end,
            callee,
            func,
            path,
            self.code_snippet
        )
    }
}

/// Result of scanning a file for API candidates
#[derive(Debug)]
pub struct ScanResult {
    /// List of candidate API call sites
    pub candidates: Vec<CandidateTarget>,
    /// True when the file could not be parsed at all. Callers must surface
    /// this: a parse failure excludes the whole file from the index, which is
    /// very different from a healthy file with no API candidates.
    pub parse_failed: bool,
    /// Module-specifier strings of every `import ... from '<source>'` in the
    /// file (e.g. `"nats"`, `"@nats-io/nats-core"`, `"./local"`). Collected from
    /// the same parse that produces candidates so the orchestrator can decide,
    /// without re-parsing, whether a zero-candidate file imports a recognized
    /// messaging-client package and should be force-analyzed (pub/sub Part B).
    pub import_sources: Vec<String>,
    /// Pub/sub operations whose identity is fully derivable from the AST
    /// (carrick#387). Merged into the file's LLM extraction after the analyzer
    /// pass so an extraction-recall flake cannot lose the operation — the same
    /// authoritative-structural-facts contract as file-based route endpoints.
    /// Empty whenever Signal 7's messaging-client gates are off.
    pub pubsub_anchor_ops: Vec<PubsubAnchorOp>,
    /// Outbound calls that reach their endpoint through a request wrapper
    /// declared in THIS file, with the path passed in as an argument
    /// (carrick#588). The site raises no candidate of its own and the wrapper's
    /// own URL resolves to nothing, so the join is read off the AST and merged
    /// into the file's extraction afterwards. See `crate::local_http_wrapper`.
    pub local_wrapper_calls: Vec<LocalWrapperCall>,
}

/// A pub/sub operation asserted deterministically from the AST (carrick#387):
/// a statement/initializer-position member call literally named `publish` or
/// `subscribe` whose ONLY argument resolves to a literal topic string (inline
/// literal, top-level const-string reference, or a template literal whose every
/// interpolation is such a reference). Payload-less by construction — a call
/// with a payload argument stays on the LLM path so the type-capture judgment
/// (locators, envelope unwrapping) is never preempted. The measured gap this
/// closes: the file-analyzer sometimes omits no-payload template-literal-topic
/// ops entirely (4/20 passes on the messenger-template-topic-nopayload
/// fixture), and with no payload there is no judgment left for the LLM to add —
/// the topic, role, and line are structural facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubsubAnchorOp {
    /// The fully resolved literal topic string (e.g. "PollController:pollingStarted").
    pub topic: String,
    /// publish -> Publisher, subscribe -> Subscriber (the protocol vocabulary
    /// is the only method-name shape the anchor accepts).
    pub role: PubsubRole,
    /// 1-based line number of the call site.
    pub line_number: usize,
    /// First parameter of an inline handler function passed alongside the
    /// topic (`subscribe("topic", (msg) => …)`, carrick#402 shape c) — the one
    /// shape where the handler param IS the decoded payload binding, so the
    /// backfill records it as a FunctionParam payload locator. Recorded only
    /// when the param is a simple identifier. `None` for every other anchor
    /// shape: the single-arg call has no handler, and the options-object /
    /// constructor-worker shapes (kafkajs `eachMessage`, BullMQ `Job`) pass an
    /// ENVELOPE to the handler, where a deterministic param locator would
    /// replace an honest Unknown with a wrong type.
    pub handler_param: Option<String>,
    /// 1-based line of that parameter (the handler may start on a later line
    /// than the call the op is keyed on).
    pub handler_param_line: Option<usize>,
}

/// A value exported from a module. Used by file-based routing to recover the
/// HTTP method of an app-router handler (`export async function GET(...)`),
/// which is structural information the call-site scanner does not capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedHandler {
    /// The exported binding name (`GET`, `POST`, …), or `"default"` for a
    /// default export.
    pub name: String,
    /// 1-based line number of the export.
    pub line_number: usize,
    /// Start byte offset of the exported declaration.
    pub span_start: u32,
    /// End byte offset of the exported declaration.
    pub span_end: u32,
    /// HTTP-method literals the handler body compares the request method
    /// against (carrick#601). Empty when the handler reads no method guard.
    /// A route module that exports one generic handler and narrows the method
    /// inside the body serves only the guarded verbs, so the convention's
    /// default verb for that export would be an endpoint nothing serves.
    pub method_guards: Vec<String>,
}

/// A route declared as data in a registry array
/// (`{ method: 'GET', path: '/health', handler: healthCheckHandler }`). The
/// HTTP method, path, and handler owner are all structural facts — no call site
/// the candidate scanner can see — so they are emitted as a deterministic
/// endpoint instead of being routed through the LLM (#234). Only descriptors
/// whose method *and* path are string literals are reported; dynamic-handler
/// cases stay on the recall-boost candidate path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDescriptorEndpoint {
    /// The HTTP method literal (`GET`, `POST`, …), verbatim from the object.
    pub method: String,
    /// The route path literal (`/gateway/health`), verbatim from the object.
    pub path: String,
    /// The handler identifier (`healthCheckHandler`) — the route's real owner.
    /// `None` when the handler is absent or not a bare identifier.
    pub handler: Option<String>,
    /// 1-based line number of the descriptor object literal.
    pub line_number: usize,
    /// Start byte offset of the descriptor object literal.
    pub span_start: u32,
    /// End byte offset of the descriptor object literal.
    pub span_end: u32,
}

/// A route bound to an imported handler in a route table
/// (`router('/widget', widget)`), #580.
///
/// Half a route: the path is here, the method and handler are in the module
/// `binding` was imported from. See
/// [`SwcScanner::controller_route_bindings`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerRouteBinding {
    /// The route path literal (`/widget/:id`), verbatim from the call.
    pub path: String,
    /// Local name of the LAST argument — the handler, whatever middleware
    /// precedes it.
    pub binding: String,
    /// Module specifier `binding` was imported from, to be resolved through
    /// the module graph.
    pub import_source: String,
    /// 1-based line number of the binding call.
    pub line_number: usize,
    /// Start byte offset of the binding call.
    pub span_start: u32,
    /// End byte offset of the binding call.
    pub span_end: u32,
}

/// A controller-class method that answers an HTTP method (#580).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerMethod {
    /// The method's own name (`get`, `exportCsv`) — the route's handler.
    pub name: String,
    /// The HTTP method it answers, uppercased.
    pub http_method: String,
    /// 1-based line number of the method, in the controller's own file.
    pub line_number: usize,
    /// Start byte offset of the method.
    pub span_start: u32,
    /// End byte offset of the method.
    pub span_end: u32,
}

/// The controller class a module default-exports (#580). Only the methods that
/// answer an HTTP method are carried; a class with none is not a controller and
/// contributes no routes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerClass {
    /// The class name — the owner of every route bound to this controller.
    pub name: String,
    pub methods: Vec<ControllerMethod>,
}

/// Lightweight SWC-based scanner for detecting potential API patterns.
///
/// This scanner looks for method call expressions that match common
/// API patterns across frameworks. It's intentionally broad to avoid
/// missing any potential API calls.
pub struct SwcScanner {
    source_map: Lrc<SourceMap>,
}

impl Default for SwcScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl SwcScanner {
    pub fn new() -> Self {
        Self {
            source_map: Lrc::new(SourceMap::default()),
        }
    }

    /// Scan a file for potential API call sites.
    ///
    /// Returns a ScanResult with candidates and whether the file should be analyzed.
    /// If no candidates are found, the file can be skipped.
    #[allow(dead_code)]
    pub fn scan_file(
        &self,
        file_path: &Path,
        data_fetchers: &[String],
        messaging_clients: &[String],
    ) -> ScanResult {
        let handler = Handler::with_tty_emitter(
            ColorConfig::Never,
            true,
            false,
            Some(self.source_map.clone()),
        );

        let module = match parse_file(file_path, &self.source_map, &handler) {
            Some(m) => m,
            None => {
                return ScanResult {
                    candidates: Vec::new(),
                    parse_failed: true,
                    import_sources: Vec::new(),
                    pubsub_anchor_ops: Vec::new(),
                    local_wrapper_calls: Vec::new(),
                };
            }
        };

        let import_sources = collect_import_sources(&module);
        let imports_messaging_client =
            file_imports_messaging_client(&import_sources, messaging_clients);
        let repo_has_messaging_clients = !messaging_clients.is_empty();
        // Const-string topic bindings are only needed for the gated Signal 7, so
        // skip the pre-pass entirely when both gate tiers are off.
        let const_string_values = if imports_messaging_client || repo_has_messaging_clients {
            collect_const_string_values(&module)
        } else {
            HashMap::new()
        };
        let local_wrapper_calls = collect_local_wrapper_calls(&module, &self.source_map);
        let mut visitor = CandidateVisitor::new(
            self.source_map.clone(),
            package_import_locals(&module, data_fetchers),
            imports_messaging_client,
            const_string_values,
            repo_has_messaging_clients,
            package_import_locals(&module, messaging_clients),
        );
        module.visit_with(&mut visitor);

        ScanResult {
            candidates: visitor.candidates,
            parse_failed: false,
            import_sources,
            pubsub_anchor_ops: visitor.pubsub_anchor_ops,
            local_wrapper_calls,
        }
    }

    /// Scan file content directly (useful for testing or when content is already loaded).
    ///
    /// Creates a fresh SourceMap for each call to ensure per-file byte offsets.
    /// Previously, reusing `self.source_map` caused cumulative offset accumulation
    /// when scanning multiple files, breaking span-based type inference in the sidecar.
    pub fn scan_content(
        &self,
        file_path: &Path,
        content: &str,
        data_fetchers: &[String],
        messaging_clients: &[String],
    ) -> ScanResult {
        use swc_common::{FileName, GLOBALS, Globals, Mark};
        use swc_ecma_parser::{Parser, StringInput, Syntax, lexer::Lexer};
        use swc_ecma_transforms_base::resolver;
        use swc_ecma_visit::VisitMutWith;

        // Determine syntax based on file extension. Decorators must be enabled
        // so NestJS-style `@Controller('users')` / `@Get(':id')` parse into
        // `Decorator` nodes that the visitor can traverse.
        let (syntax, is_typescript) = if let Some(ext) = file_path.extension() {
            match ext.to_string_lossy().as_ref() {
                "ts" => (
                    Syntax::Typescript(TsSyntax {
                        decorators: true,
                        ..Default::default()
                    }),
                    true,
                ),
                "tsx" => (
                    Syntax::Typescript(TsSyntax {
                        tsx: true,
                        decorators: true,
                        ..Default::default()
                    }),
                    true,
                ),
                "jsx" => (
                    Syntax::Es(EsSyntax {
                        jsx: true,
                        ..Default::default()
                    }),
                    false,
                ),
                _ => (Syntax::Es(Default::default()), false),
            }
        } else {
            (Syntax::Es(Default::default()), false)
        };

        // Create a fresh SourceMap for each file to ensure per-file byte offsets.
        // SWC's SourceMap maintains cumulative offsets across new_source_file() calls,
        // so reusing a single map across files would shift all spans by the total size
        // of previously scanned files.
        let file_source_map: Lrc<SourceMap> = Default::default();
        let source_file = file_source_map.new_source_file(
            Lrc::new(FileName::Real(file_path.to_path_buf())),
            content.to_string(),
        );

        let lexer = Lexer::new(
            syntax,
            Default::default(),
            StringInput::from(&*source_file),
            None,
        );
        let mut parser = Parser::new_from(lexer);

        let mut module = match parser.parse_module() {
            Ok(m) => m,
            Err(_) => {
                return ScanResult {
                    candidates: Vec::new(),
                    parse_failed: true,
                    import_sources: Vec::new(),
                    pubsub_anchor_ops: Vec::new(),
                    local_wrapper_calls: Vec::new(),
                };
            }
        };

        // Apply resolver for proper scope handling
        GLOBALS.set(&Globals::new(), || {
            let unresolved_mark = Mark::new();
            let top_level_mark = Mark::new();
            let mut pass = resolver(unresolved_mark, top_level_mark, is_typescript);
            module.visit_mut_with(&mut pass);
        });

        let import_sources = collect_import_sources(&module);
        let imports_messaging_client =
            file_imports_messaging_client(&import_sources, messaging_clients);
        let repo_has_messaging_clients = !messaging_clients.is_empty();
        // Const-string topic bindings are only needed for the gated Signal 7, so
        // skip the pre-pass entirely when both gate tiers are off.
        let const_string_values = if imports_messaging_client || repo_has_messaging_clients {
            collect_const_string_values(&module)
        } else {
            HashMap::new()
        };
        let local_wrapper_calls = collect_local_wrapper_calls(&module, &file_source_map);
        let mut visitor = CandidateVisitor::new(
            file_source_map,
            package_import_locals(&module, data_fetchers),
            imports_messaging_client,
            const_string_values,
            repo_has_messaging_clients,
            package_import_locals(&module, messaging_clients),
        );
        module.visit_with(&mut visitor);

        ScanResult {
            candidates: visitor.candidates,
            parse_failed: false,
            import_sources,
            pubsub_anchor_ops: visitor.pubsub_anchor_ops,
            local_wrapper_calls,
        }
    }

    /// Extract the top-level exported bindings of a module.
    ///
    /// This powers file-based routing: an app-router endpoint declares its HTTP
    /// method as the *name* of an exported handler (`export function GET`), which
    /// never appears as a call site, so the candidate scanner alone cannot see
    /// it. Returns one [`ExportedHandler`] per exported binding; `export default`
    /// is reported with the name `"default"`.
    pub fn exported_handlers(&self, file_path: &Path, content: &str) -> Vec<ExportedHandler> {
        use swc_common::{FileName, Spanned};
        use swc_ecma_parser::{Parser, StringInput, Syntax, lexer::Lexer};

        let syntax = match file_path.extension().and_then(|e| e.to_str()) {
            Some("ts") => Syntax::Typescript(TsSyntax {
                decorators: true,
                ..Default::default()
            }),
            Some("tsx") => Syntax::Typescript(TsSyntax {
                tsx: true,
                decorators: true,
                ..Default::default()
            }),
            Some("jsx") => Syntax::Es(EsSyntax {
                jsx: true,
                ..Default::default()
            }),
            _ => Syntax::Es(Default::default()),
        };

        let sm: Lrc<SourceMap> = Default::default();
        let source_file = sm.new_source_file(
            Lrc::new(FileName::Real(file_path.to_path_buf())),
            content.to_string(),
        );
        let lexer = Lexer::new(
            syntax,
            Default::default(),
            StringInput::from(&*source_file),
            None,
        );
        let mut parser = Parser::new_from(lexer);
        let module = match parser.parse_module() {
            Ok(m) => m,
            Err(_) => return Vec::new(),
        };

        // Method guards are read per top-level *binding*, exported or not, so a
        // handler declared above an `export { ... }` list reads its guard the
        // same way an inline `export function` does.
        let mut guards_by_binding: HashMap<String, Vec<String>> = HashMap::new();
        for item in &module.body {
            let decl = match item {
                ModuleItem::Stmt(Stmt::Decl(d)) => d,
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(e)) => &e.decl,
                _ => continue,
            };
            match decl {
                Decl::Fn(f) => {
                    guards_by_binding
                        .insert(f.ident.sym.to_string(), collect_method_guards(&*f.function));
                }
                Decl::Var(var) => {
                    for d in &var.decls {
                        let (Pat::Ident(ident), Some(init)) = (&d.name, &d.init) else {
                            continue;
                        };
                        guards_by_binding
                            .insert(ident.id.sym.to_string(), collect_method_guards(&**init));
                    }
                }
                _ => {}
            }
        }

        let mut out = Vec::new();
        let mut push = |name: String, span: swc_common::Span, method_guards: Vec<String>| {
            out.push(ExportedHandler {
                name,
                line_number: sm.lookup_char_pos(span.lo).line,
                span_start: span.lo.0,
                span_end: span.hi.0,
                method_guards,
            });
        };
        let guards_of = |binding: &str| -> Vec<String> {
            guards_by_binding.get(binding).cloned().unwrap_or_default()
        };

        for item in &module.body {
            let ModuleItem::ModuleDecl(decl) = item else {
                continue;
            };
            match decl {
                // `export function GET() {}`, `export const POST = ...`, `export class X {}`
                ModuleDecl::ExportDecl(export) => match &export.decl {
                    Decl::Fn(f) => {
                        let name = f.ident.sym.to_string();
                        let guards = guards_of(&name);
                        push(name, export.span(), guards);
                    }
                    Decl::Class(c) => {
                        let name = c.ident.sym.to_string();
                        push(name, export.span(), Vec::new());
                    }
                    Decl::Var(var) => {
                        for d in &var.decls {
                            if let Pat::Ident(ident) = &d.name {
                                let name = ident.id.sym.to_string();
                                let guards = guards_of(&name);
                                push(name, export.span(), guards);
                            }
                        }
                    }
                    _ => {}
                },
                // `export { GET, POST as handler }`
                ModuleDecl::ExportNamed(named) => {
                    for spec in &named.specifiers {
                        if let ExportSpecifier::Named(n) = spec {
                            // Prefer the exported alias if present (`as handler`).
                            let name = match n.exported.as_ref().unwrap_or(&n.orig) {
                                ModuleExportName::Ident(id) => id.sym.to_string(),
                                ModuleExportName::Str(s) => s.value.to_string(),
                            };
                            // The guard lives on the *local* binding the
                            // specifier renames, not on the exported alias.
                            let guards = match &n.orig {
                                ModuleExportName::Ident(id) => guards_of(id.sym.as_ref()),
                                ModuleExportName::Str(s) => guards_of(s.value.as_ref()),
                            };
                            push(name, n.span(), guards);
                        }
                    }
                }
                // `export default function () {}` / `export default expr`
                ModuleDecl::ExportDefaultDecl(d) => {
                    let guards = collect_method_guards(&d.decl);
                    push("default".to_string(), d.span(), guards);
                }
                ModuleDecl::ExportDefaultExpr(e) => {
                    let guards = collect_method_guards(&*e.expr);
                    push("default".to_string(), e.span(), guards);
                }
                _ => {}
            }
        }

        out
    }

    /// Extract route-descriptor endpoints declared as data in a registry array
    /// (`{ method: 'GET', path: '/health', handler: healthCheckHandler }`).
    ///
    /// This powers deterministic route-descriptor extraction (#234): the method,
    /// path, and handler owner are all structural facts with no call site the
    /// candidate scanner can see, and the file-analyzer prompt only matches
    /// framework-call patterns — so the orchestrator builds the endpoint from
    /// these facts directly, bypassing the LLM. Only descriptors whose method
    /// *and* path are string literals are returned; the rest stay on the
    /// recall-boost candidate path.
    pub fn route_descriptor_endpoints(
        &self,
        file_path: &Path,
        content: &str,
    ) -> Vec<RouteDescriptorEndpoint> {
        let Some((sm, module)) = parse_standalone_module(file_path, content) else {
            return Vec::new();
        };

        let mut visitor = RouteDescriptorVisitor {
            source_map: sm,
            endpoints: Vec::new(),
        };
        module.visit_with(&mut visitor);
        visitor.endpoints
    }

    /// Collect every `router('/path', …, controller)` binding in `content`
    /// (#580 part b).
    ///
    /// A class-controller service keeps its paths in one route table and its
    /// handlers in controller modules that never name their own path, so
    /// single-file analysis can see neither half of a route. This is the
    /// route-table half: the literal path, and the local binding the path was
    /// bound to, with the module specifier that binding was imported from so
    /// the caller can follow it.
    ///
    /// The shape is recognised structurally, with no framework names involved:
    ///
    /// * the callee is a *bare identifier this file imported* — a route binder
    ///   is a callable another module supplies, not a method on an object and
    ///   not a local closure;
    /// * the first argument is a string literal that is a producer path (see
    ///   [`is_producer_route_path`]);
    /// * the last argument is a bare identifier this file imported, which is
    ///   the handler even when middleware sits in front of it
    ///   (`router('/token', errorHandler, token)`).
    ///
    /// Recognising the *shape* is deliberately not the whole gate: what makes
    /// this a route is that the binding resolves to a module default-exporting
    /// a controller class (see
    /// [`default_export_controller_class`](Self::default_export_controller_class)),
    /// which is the caller's step. A call that merely looks like this but
    /// binds something else emits nothing.
    pub fn controller_route_bindings(
        &self,
        file_path: &Path,
        content: &str,
    ) -> Vec<ControllerRouteBinding> {
        let Some((sm, module)) = parse_standalone_module(file_path, content) else {
            return Vec::new();
        };

        let mut visitor = ControllerRouteVisitor {
            source_map: sm,
            imports: collect_import_locals(&module),
            bindings: Vec::new(),
        };
        module.visit_with(&mut visitor);
        visitor.bindings
    }

    /// The controller class `content` default-exports, with the methods that
    /// answer an HTTP method (#580 part b).
    ///
    /// The controller half of a class-controller route. Three default-export
    /// shapes reach a class, and nothing else does:
    ///
    /// * `export default class Foo {}` — the class itself;
    /// * `export default new Foo()` — an instance of a class declared here;
    /// * `export default foo` where `foo` is a local binding for either of the
    ///   above (`const foo = new Foo(); export default foo;`).
    ///
    /// A default export that is a function, an object, an instance of a class
    /// from another module, or an anonymous class returns `None`: without a
    /// class declared in this module there is no method list to enumerate and
    /// no name to own the routes.
    ///
    /// A method answers an HTTP method when its NAME is an HTTP verb
    /// (`get`, `post`, …) or when it carries a decorator whose single argument
    /// is a string literal naming one (`@method('GET')`). The two tests are
    /// deliberately asymmetric: a name is weak evidence, so only the seven
    /// verbs a handler is realistically named after count, while an explicit
    /// literal is a declaration and is taken at face value for any method
    /// [`crate::type_manifest::is_http_method`] accepts. Reading the literal
    /// rather than the decorator's name is what keeps this framework-agnostic
    /// AND rejects `@accept('text/csv')`, whose literal is a content type.
    pub fn default_export_controller_class(
        &self,
        file_path: &Path,
        content: &str,
    ) -> Option<ControllerClass> {
        let (sm, module) = parse_standalone_module(file_path, content)?;
        let (name, class) = default_exported_class(&module)?;
        let methods = class
            .body
            .iter()
            .filter_map(|member| match member {
                ClassMember::Method(method) => controller_method(method, &sm),
                _ => None,
            })
            .collect();
        Some(ControllerClass { name, methods })
    }
}

/// Read the HTTP-method guard a handler body applies to the incoming request
/// (carrick#601).
///
/// Structural and framework-agnostic: the guard is recognized as *the handler
/// comparing the request's method against a literal*, never as any framework's
/// API. Two spellings are read, which is what the shape reduces to in any
/// stack:
///
/// * a comparison (`===`/`!==`/`==`/`!=`) between something's `.method` and an
///   HTTP-method string literal, in either operand order;
/// * a `switch` whose discriminant is that `.method` and whose cases are those
///   literals.
///
/// A local binding initialized from a `.method` member (`const m = req.method`,
/// `const { method } = request`) counts as the same expression, because a
/// handler that destructures first is doing the same narrowing.
///
/// Deliberately NOT required: that the non-matching branch rejects. Detecting
/// "rejects" means enumerating throw/`405`/early-return spellings across
/// frameworks, which is the brittleness this module exists to avoid. The cost
/// is that a handler which merely *branches* on the method reads as guarded on
/// the methods it branches on — which is still nearer the truth than the
/// convention's single default verb.
fn collect_method_guards<N>(node: &N) -> Vec<String>
where
    N: VisitWith<MethodGuardVisitor> + ?Sized,
{
    // Two passes so a comparison is read the same whether it precedes or
    // follows the binding it compares against.
    let mut visitor = MethodGuardVisitor {
        aliases: HashSet::new(),
        collecting_aliases: true,
        methods: Vec::new(),
    };
    node.visit_with(&mut visitor);
    visitor.collecting_aliases = false;
    node.visit_with(&mut visitor);
    visitor.methods
}

/// Collector behind [`collect_method_guards`].
struct MethodGuardVisitor {
    /// Local bindings initialized from a request's `.method`.
    aliases: HashSet<String>,
    /// First pass (bindings) rather than second pass (comparisons).
    collecting_aliases: bool,
    /// HTTP-method literals found, in source order, deduplicated.
    methods: Vec<String>,
}

/// `true` when the expression is a `.method` member access on anything.
fn is_method_member(expr: &Expr) -> bool {
    match unwrap_expr(expr) {
        Expr::Member(m) => matches!(&m.prop, MemberProp::Ident(i) if i.sym.as_ref() == "method"),
        _ => false,
    }
}

/// The HTTP-method literal an expression denotes, uppercased.
fn method_literal(expr: &Expr) -> Option<String> {
    let text = match unwrap_expr(expr) {
        Expr::Lit(Lit::Str(s)) => s.value.to_string(),
        // A no-substitution template literal is the same literal.
        Expr::Tpl(t) if t.exprs.is_empty() && t.quasis.len() == 1 => t.quasis[0].raw.to_string(),
        _ => return None,
    };
    is_http_method(&text).then(|| text.trim().to_uppercase())
}

impl MethodGuardVisitor {
    /// `true` when the expression denotes the request's method, either
    /// directly or through a local binding taken from it.
    fn denotes_method(&self, expr: &Expr) -> bool {
        if is_method_member(expr) {
            return true;
        }
        match unwrap_expr(expr) {
            Expr::Ident(id) => self.aliases.contains(id.sym.as_ref()),
            _ => false,
        }
    }

    fn record(&mut self, method: String) {
        if !self.methods.contains(&method) {
            self.methods.push(method);
        }
    }
}

impl MethodGuardVisitor {
    /// Record a local binding taken from a request's `.method`, so a later
    /// comparison against that binding reads as a method comparison.
    fn record_method_alias(&mut self, node: &VarDeclarator) {
        let Some(init) = &node.init else {
            return;
        };
        match &node.name {
            // `const m = req.method`
            Pat::Ident(id) if is_method_member(init) => {
                self.aliases.insert(id.id.sym.to_string());
            }
            // `const { method } = request` / `const { method: verb } = request`
            Pat::Object(obj) => {
                for prop in &obj.props {
                    match prop {
                        ObjectPatProp::Assign(a) if a.key.sym.as_ref() == "method" => {
                            self.aliases.insert(a.key.sym.to_string());
                        }
                        ObjectPatProp::KeyValue(kv) => {
                            let key_is_method = matches!(
                                &kv.key,
                                PropName::Ident(i) if i.sym.as_ref() == "method"
                            );
                            if let (true, Pat::Ident(id)) = (key_is_method, &*kv.value) {
                                self.aliases.insert(id.id.sym.to_string());
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

impl Visit for MethodGuardVisitor {
    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        if self.collecting_aliases {
            self.record_method_alias(node);
        }
        node.visit_children_with(self);
    }

    fn visit_bin_expr(&mut self, node: &BinExpr) {
        if !self.collecting_aliases
            && matches!(
                node.op,
                BinaryOp::EqEq | BinaryOp::EqEqEq | BinaryOp::NotEq | BinaryOp::NotEqEq
            )
        {
            let pair = if self.denotes_method(&node.left) {
                method_literal(&node.right)
            } else if self.denotes_method(&node.right) {
                method_literal(&node.left)
            } else {
                None
            };
            if let Some(method) = pair {
                self.record(method);
            }
        }
        node.visit_children_with(self);
    }

    fn visit_switch_stmt(&mut self, node: &SwitchStmt) {
        if !self.collecting_aliases && self.denotes_method(&node.discriminant) {
            for case in &node.cases {
                if let Some(method) = case.test.as_deref().and_then(method_literal) {
                    self.record(method);
                }
            }
        }
        node.visit_children_with(self);
    }
}

/// Parse `content` as a module with its OWN source map, so a caller that only
/// needs an AST (and the line numbers that go with it) is not entangled with
/// the scanner's own map. Returns `None` when the file does not parse; every
/// deterministic pass built on this treats a parse failure as "no facts here",
/// which is what the scanner's candidate pass does too.
fn parse_standalone_module(file_path: &Path, content: &str) -> Option<(Lrc<SourceMap>, Module)> {
    use swc_common::FileName;
    use swc_ecma_parser::{Parser, StringInput, Syntax, lexer::Lexer};

    let syntax = match file_path.extension().and_then(|e| e.to_str()) {
        Some("ts") => Syntax::Typescript(TsSyntax {
            decorators: true,
            ..Default::default()
        }),
        Some("tsx") => Syntax::Typescript(TsSyntax {
            tsx: true,
            decorators: true,
            ..Default::default()
        }),
        Some("jsx") => Syntax::Es(EsSyntax {
            jsx: true,
            ..Default::default()
        }),
        _ => Syntax::Es(Default::default()),
    };

    let sm: Lrc<SourceMap> = Default::default();
    let source_file = sm.new_source_file(
        Lrc::new(FileName::Real(file_path.to_path_buf())),
        content.to_string(),
    );
    let lexer = Lexer::new(
        syntax,
        Default::default(),
        StringInput::from(&*source_file),
        None,
    );
    let mut parser = Parser::new_from(lexer);
    parser.parse_module().ok().map(|module| (sm, module))
}

/// Local binding name -> the module specifier it was imported from, for every
/// value import in `module`. Type-only imports bind nothing at runtime, so a
/// route can never be bound to one.
fn collect_import_locals(module: &Module) -> HashMap<String, String> {
    let mut imports = HashMap::new();
    for item in &module.body {
        let ModuleItem::ModuleDecl(ModuleDecl::Import(decl)) = item else {
            continue;
        };
        if decl.type_only {
            continue;
        }
        let specifier = decl.src.value.to_string();
        for spec in &decl.specifiers {
            let local = match spec {
                ImportSpecifier::Default(s) => s.local.sym.to_string(),
                ImportSpecifier::Named(s) if !s.is_type_only => s.local.sym.to_string(),
                // `import * as ns from` binds a namespace OBJECT, which is not
                // a callable a route can be bound to.
                _ => continue,
            };
            imports.insert(local, specifier.clone());
        }
    }
    imports
}

/// The class a module default-exports, as `(name, class)`, following a local
/// binding and a `new` expression to the declaration in the SAME module. See
/// [`SwcScanner::default_export_controller_class`] for the shapes covered.
fn default_exported_class(module: &Module) -> Option<(String, &Class)> {
    let mut classes: HashMap<String, &ClassDecl> = HashMap::new();
    let mut locals: HashMap<String, &Expr> = HashMap::new();
    let mut default_export: Option<&Expr> = None;

    for item in &module.body {
        let decl = match item {
            ModuleItem::Stmt(Stmt::Decl(decl)) => Some(decl),
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => Some(&export.decl),
            // `export default class Foo {}` — the class is the export. An
            // anonymous default class has no name to own the routes it would
            // serve, so it is not a controller here.
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(export)) => {
                if let DefaultDecl::Class(class) = &export.decl
                    && let Some(ident) = &class.ident
                {
                    return Some((ident.sym.to_string(), &class.class));
                }
                None
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(export)) => {
                default_export = Some(&export.expr);
                None
            }
            _ => None,
        };
        match decl {
            Some(Decl::Class(class)) => {
                classes.insert(class.ident.sym.to_string(), class);
            }
            Some(Decl::Var(var)) => {
                for declarator in &var.decls {
                    if let Pat::Ident(ident) = &declarator.name
                        && let Some(init) = &declarator.init
                    {
                        locals.insert(ident.id.sym.to_string(), init);
                    }
                }
            }
            _ => {}
        }
    }

    // Depth cap: a default export chained through more local bindings than
    // this is indistinguishable from a mis-resolution, and each hop is a
    // guess about a value we cannot evaluate.
    const MAX_BINDING_HOPS: usize = 4;
    let mut expr = default_export?;
    for _ in 0..MAX_BINDING_HOPS {
        match unwrap_expr(expr) {
            // `export default new Foo()`
            Expr::New(new_expr) => {
                let Expr::Ident(ident) = unwrap_expr(&new_expr.callee) else {
                    return None;
                };
                let class = classes.get(&ident.sym.to_string())?;
                return Some((class.ident.sym.to_string(), &class.class));
            }
            // `export default Foo` (the class) or `export default foo` (a
            // local binding for one).
            Expr::Ident(ident) => {
                let name = ident.sym.to_string();
                if let Some(class) = classes.get(&name) {
                    return Some((class.ident.sym.to_string(), &class.class));
                }
                expr = locals.get(&name)?;
            }
            _ => return None,
        }
    }
    None
}

/// Strip the wrappers that do not change which value an expression names, so
/// `export default (new Foo() as Controller)` reads like `new Foo()`.
fn unwrap_expr(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(inner) => unwrap_expr(&inner.expr),
        Expr::TsAs(inner) => unwrap_expr(&inner.expr),
        Expr::TsNonNull(inner) => unwrap_expr(&inner.expr),
        Expr::TsSatisfies(inner) => unwrap_expr(&inner.expr),
        other => other,
    }
}

/// The HTTP method a controller method answers, or `None` when it answers
/// none — a helper, not a route. See
/// [`SwcScanner::default_export_controller_class`] for the rule.
fn controller_method(
    method: &ClassMethod,
    source_map: &Lrc<SourceMap>,
) -> Option<ControllerMethod> {
    // Constructors, getters and setters are not request handlers.
    if method.kind != MethodKind::Method {
        return None;
    }
    let name = match &method.key {
        PropName::Ident(id) => id.sym.to_string(),
        PropName::Str(s) => s.value.to_string(),
        _ => return None,
    };
    let http_method = declared_http_method(&method.function.decorators).or_else(|| {
        VERB_NAMED_METHODS
            .contains(&name.as_str())
            .then(|| name.to_uppercase())
    })?;
    let span = method.span;
    Some(ControllerMethod {
        name,
        http_method,
        // The method's NAME, not its span: a decorated method's span opens at
        // the first decorator, which would report the route a line or two
        // above the handler a reader is being sent to.
        line_number: source_map.lookup_char_pos(method.key.span().lo).line,
        span_start: span.lo.0,
        span_end: span.hi.0,
    })
}

/// The HTTP method a decorator declares outright: a call with exactly one
/// argument, a plain string literal, naming an HTTP method. One argument is
/// required so a multi-argument decorator (`@roles('GET', 'admin')`) cannot
/// contribute a method by accident.
fn declared_http_method(decorators: &[Decorator]) -> Option<String> {
    decorators.iter().find_map(|decorator| {
        let Expr::Call(call) = &*decorator.expr else {
            return None;
        };
        let [arg] = call.args.as_slice() else {
            return None;
        };
        if arg.spread.is_some() {
            return None;
        }
        let Expr::Lit(Lit::Str(literal)) = &*arg.expr else {
            return None;
        };
        let value = literal.value.to_string();
        crate::type_manifest::is_http_method(&value).then(|| value.trim().to_uppercase())
    })
}

/// Method names that are strong enough evidence on their own to make a
/// controller method a route. Deliberately the seven verbs a handler is
/// realistically named after — not every method
/// [`crate::type_manifest::is_http_method`] accepts, because a class is far
/// more likely to have a `connect` or `trace` helper than to serve one.
const VERB_NAMED_METHODS: &[&str] = &["get", "post", "put", "patch", "delete", "head", "options"];

/// Collects `router('/path', …, controller)` bindings. See
/// [`SwcScanner::controller_route_bindings`] for the shape and why it is gated
/// on imports rather than on any framework name.
struct ControllerRouteVisitor {
    source_map: Lrc<SourceMap>,
    /// Local binding name -> module specifier, for this file's value imports.
    imports: HashMap<String, String>,
    bindings: Vec<ControllerRouteBinding>,
}

impl Visit for ControllerRouteVisitor {
    fn visit_call_expr(&mut self, node: &CallExpr) {
        node.visit_children_with(self);

        let Callee::Expr(callee) = &node.callee else {
            return;
        };
        // The binder is a callable another module supplies.
        let Expr::Ident(callee_ident) = unwrap_expr(callee) else {
            return;
        };
        if !self.imports.contains_key(&callee_ident.sym.to_string()) {
            return;
        }
        // `binder(path, handler)` at minimum; middleware may sit between them.
        let ([first, .., last], false) = (
            node.args.as_slice(),
            node.args.iter().any(|arg| arg.spread.is_some()),
        ) else {
            return;
        };
        let Expr::Lit(Lit::Str(path)) = &*first.expr else {
            return;
        };
        let path = path.value.to_string();
        if !is_producer_route_path(&path) {
            return;
        }
        let Expr::Ident(handler) = unwrap_expr(&last.expr) else {
            return;
        };
        let binding = handler.sym.to_string();
        let Some(import_source) = self.imports.get(&binding).cloned() else {
            return;
        };
        let span = node.span;
        self.bindings.push(ControllerRouteBinding {
            path,
            binding,
            import_source,
            line_number: self.source_map.lookup_char_pos(span.lo).line,
            span_start: span.lo.0,
            span_end: span.hi.0,
        });
    }
}

/// Whether `path` can be the path of a PRODUCER — a route this service serves.
///
/// A served route is a path on this origin, so it is absolute: it starts with
/// `/`. A template literal counts when its *static head* does
/// (`` `/orders/${id}` ``), which is what the leading-backtick strip covers.
///
/// The contrast with [`RouteDescriptorVisitor::is_route_shaped_path`] is the
/// whole point of having two predicates, and it is directional (#580). A
/// CONSUMER names someone else's origin, so a full `http(s)://` URL is a
/// legitimate outbound target. A producer never serves one: a string that is
/// a full URL under a server-side route call is a schema `$id`, a redirect
/// target, or a validator argument — never the path the route is mounted at.
/// Bare tokens are rejected on the same reasoning (`GET` is a method literal,
/// `text/csv` a content type; neither is a path).
pub fn is_producer_route_path(path: &str) -> bool {
    let trimmed = path.trim();
    trimmed
        .strip_prefix('`')
        .unwrap_or(trimmed)
        .starts_with('/')
}

/// Collects deterministic route descriptors (`{ method, path, handler }` with
/// literal method + path) for the no-LLM emission path (#234). The shape guard
/// is shared with the recall-boost candidate via
/// [`CandidateVisitor::route_descriptor`], but the deterministic gate is
/// strictly narrower (#241): a descriptor is emitted only when it is a *direct
/// element of an array literal* (a routes registry, not a standalone config
/// object) and its path is *producer-route-shaped* (leading `/`, not a bare
/// token like `some-message` and not a full URL, #580). Anything failing this
/// gate is left for the LLM extraction path; only genuine route registries are
/// authoritative.
struct RouteDescriptorVisitor {
    source_map: Lrc<SourceMap>,
    endpoints: Vec<RouteDescriptorEndpoint>,
}

impl RouteDescriptorVisitor {
    /// A path is route-shaped when it is an absolute path (`/widgets`) or an
    /// http(s) URL. This rejects bare tokens (`some-message`), RPC method names,
    /// and other non-route strings that happen to sit under a `path` key.
    ///
    /// CONSUMER-side only. A producer's path must additionally be absolute —
    /// see [`is_producer_route_path`], which owns that direction.
    fn is_route_shaped_path(path: &str) -> bool {
        let trimmed = path.trim();
        trimmed.starts_with('/')
            || trimmed.starts_with("http://")
            || trimmed.starts_with("https://")
    }

    /// Emit a deterministic endpoint for `node` when it carries a literal
    /// method + a route-shaped literal path. Used only for object literals that
    /// are direct elements of an array literal (the registry context, #241).
    fn try_emit(&mut self, node: &ObjectLit) {
        let Some(descriptor) = CandidateVisitor::route_descriptor(node) else {
            return;
        };
        // The deterministic path requires literal method *and* path; a
        // descriptor missing either keeps only its recall-boost candidate.
        let (Some(method), Some(path)) = (descriptor.method, descriptor.path) else {
            return;
        };
        // #241: reject non-route paths (bare tokens, RPC method names) so a
        // config object that merely carries `method`/`path` keys is not
        // fabricated as an endpoint. #580: this is the PRODUCER side, so a
        // full URL is rejected too — it is never the path a route is served
        // at.
        if !is_producer_route_path(&path) {
            return;
        }
        let span = node.span;
        self.endpoints.push(RouteDescriptorEndpoint {
            method,
            path,
            handler: descriptor.handler,
            line_number: self.source_map.lookup_char_pos(span.lo).line,
            span_start: span.lo.0,
            span_end: span.hi.0,
        });
    }
}

impl Visit for RouteDescriptorVisitor {
    fn visit_array_lit(&mut self, node: &ArrayLit) {
        // #241: only object literals that are *direct elements* of an array
        // (a routes registry) qualify for deterministic emission. A standalone
        // config object — e.g. an axios `{ method, path, headers }` options bag
        // — never reaches `try_emit`, so it falls through to the LLM path.
        for element in node.elems.iter().flatten() {
            if let Expr::Object(obj) = &*element.expr {
                self.try_emit(obj);
            }
        }
        node.visit_children_with(self);
    }
}

/// The salient parts of a route-descriptor object literal
/// (`{ method, path, handler }`): the HTTP method literal (when it is a string
/// literal), the path literal snippet (when present) and the handler identifier
/// (when it is a bare identifier reference).
struct RouteDescriptor {
    method: Option<String>,
    path: Option<String>,
    handler: Option<String>,
}

/// Visitor that collects potential API call sites.
struct CandidateVisitor {
    candidates: Vec<CandidateTarget>,
    source_map: Lrc<SourceMap>,
    function_stack: Vec<String>,
    /// Local binding names imported from a known network/data-fetching package
    /// (e.g. `axios` from `import axios from 'axios'`). Calls rooted at one of
    /// these are emitted as candidates regardless of method name, so bespoke
    /// client wrappers (`client.users.list()`) are not missed.
    network_import_locals: HashSet<String>,
    /// Span ranges already emitted, so the broadened signals below don't push
    /// the same call site twice (candidate ids are span-based).
    seen_spans: HashSet<(u32, u32)>,
    /// Depth of enclosing `await` expressions. An awaited call with a string
    /// argument is a strong network-call signal even when the callee name is
    /// unknown.
    await_depth: usize,
    /// True when this file imports a package the cloud /framework-detect step
    /// flagged as a messaging client (NATS, Redis, Kafka, …). This is the gate
    /// for Signal 7 (pub/sub call-site surfacing): the publish/subscribe shape
    /// (`obj.method("topic", payload)`) is indistinguishable from
    /// `socket.emit('x')` / `logger.info('x')`, so surfacing it unconditionally
    /// broke the socket-skip invariant and risked corpus-1. socket.io is *not* a
    /// messaging client, so socket files never gate in and the signal stays inert
    /// there. Empty `messaging_clients` → always false → Signal 7 never fires.
    file_imports_messaging_client: bool,
    /// Top-level `const <id> = "<literal>"` bindings (name -> literal value),
    /// so a pub/sub call whose topic is referenced by name (`const SUBJECT =
    /// "user.registered"; … nc.publish(SUBJECT, …)`) still counts as having a
    /// string-literal topic for the Signal 7 first-arg check, and so the
    /// anchor-op path (carrick#387) can resolve const-ref and template-literal
    /// topics to their literal strings. Only string-literal initializers are
    /// recorded; this is a recall booster, not a full constant-folder.
    const_string_values: HashMap<String, String>,
    /// True while visiting a call expression that sits directly in a
    /// statement-expression (`nc.publish(SUBJECT, payload);`) or a variable
    /// initializer (`const sub = nc.subscribe("topic");`). Signal 7 only fires
    /// in these two positions — the fire-and-forget publish/subscribe shapes —
    /// so call sites nested inside other expressions are not surfaced.
    in_pubsub_call_position: bool,
    /// True when the REPO's framework detection found any messaging client,
    /// regardless of this file's imports. Second tier of the Signal 7 gate:
    /// a file that receives its messaging client by constructor injection or
    /// inheritance (`this.messenger` from a base class) has no gating import,
    /// so the call SHAPE gates instead — but only for member calls literally
    /// named `publish`/`subscribe`, the protocol vocabulary itself, so
    /// `logger.info('msg')` / `socket.emit('evt')` stay inert (carrick#317).
    repo_has_messaging_clients: bool,
    /// Deterministically asserted pub/sub operations (carrick#387): the
    /// payload-less publish/subscribe sites whose topic resolves to a literal
    /// string. Collected alongside the Signal 7 candidates (same gates, same
    /// position rule) and merged into the file's extraction after the LLM pass.
    pubsub_anchor_ops: Vec<PubsubAnchorOp>,
    /// Local binding names imported from a package the framework-detect step
    /// flagged as a messaging client (`Worker` from `import { Worker } from
    /// 'bullmq'`). Gate for the NewExpr subscriber anchor (carrick#402 shape
    /// b): `new X("literal", fn)` anchors only when `X` is one of these
    /// bindings — resolution to a detected messaging-client IMPORT, not merely
    /// any file-level import, which is what keeps `new CronJob("0 * * * *",
    /// fn)` from becoming a phantom subscriber. Empty when the repo detected
    /// no messaging clients.
    messaging_import_locals: HashSet<String>,
}

impl CandidateVisitor {
    fn new(
        source_map: Lrc<SourceMap>,
        network_import_locals: HashSet<String>,
        file_imports_messaging_client: bool,
        const_string_values: HashMap<String, String>,
        repo_has_messaging_clients: bool,
        messaging_import_locals: HashSet<String>,
    ) -> Self {
        Self {
            candidates: Vec::new(),
            source_map,
            function_stack: Vec::new(),
            network_import_locals,
            seen_spans: HashSet::new(),
            await_depth: 0,
            file_imports_messaging_client,
            const_string_values,
            in_pubsub_call_position: false,
            repo_has_messaging_clients,
            pubsub_anchor_ops: Vec::new(),
            messaging_import_locals,
        }
    }

    /// Check if an identifier looks like an API-related object
    fn is_potential_api_object(&self, name: &str) -> bool {
        // Common API object patterns (framework-agnostic)
        let api_objects = [
            // Generic router/app patterns
            "app",
            "router",
            "server",
            "api",
            "route",
            "routes",
            // HTTP client patterns
            "fetch",
            "axios",
            "http",
            "https",
            "request",
            "client",
            "response",
            "res",
            "resp",
            // Common variations
            "apiRouter",
            "appRouter",
            "mainRouter",
            "authRouter",
            "userRouter",
            "v1Router",
            "v2Router",
        ];

        // Check exact matches
        if api_objects.contains(&name) {
            return true;
        }

        // Check if name ends with common API suffixes
        let lower = name.to_lowercase();
        lower.ends_with("router")
            || lower.ends_with("route")
            || lower.ends_with("routes")
            || lower.ends_with("app")
            || lower.ends_with("server")
            || lower.ends_with("api")
            || lower.ends_with("client")
            || lower.ends_with("handler")
            || lower.ends_with("controller")
    }

    /// Check if a method name looks like an API method
    fn is_potential_api_method(&self, name: &str) -> bool {
        let api_methods = [
            // HTTP methods
            "get",
            "post",
            "put",
            "delete",
            "patch",
            "head",
            "options",
            "all",
            // Mounting/middleware
            "use",
            "mount",
            "register",
            "plugin",
            "route",
            // Data fetching
            "fetch",
            "json",
            "text",
            "blob",
            "send",
            "request",
            // Common framework patterns
            "listen",
            "handle",
            "handler",
            "middleware",
            "define",
        ];

        api_methods.contains(&name.to_lowercase().as_str())
    }

    /// Check if this is a call to a global network primitive (`fetch(...)`).
    /// Other primitives (`WebSocket`, `EventSource`, `XMLHttpRequest`) are
    /// constructed with `new` and handled in `visit_new_expr`.
    fn is_global_network_call(&self, callee: &Callee) -> bool {
        if let Callee::Expr(expr) = callee
            && let Expr::Ident(ident) = &**expr
        {
            return matches!(ident.sym.as_ref(), "fetch");
        }
        false
    }

    /// Is this a `navigator.sendBeacon(url, ...)` call? This is a web-platform
    /// data-transmitting primitive (a fire-and-forget HTTP POST), the same
    /// family as `fetch`/`XMLHttpRequest`. Matching the syntactic shape
    /// `navigator.sendBeacon(...)` keeps the scanner free of any third-party
    /// client allowlist. This is shape-based, not resolution-based: it keys off
    /// a receiver named `navigator`, which a local could shadow, so it does not
    /// prove the actual browser built-in is being called.
    fn is_navigator_send_beacon(callee: &Callee) -> bool {
        let Callee::Expr(expr) = callee else {
            return false;
        };
        let Expr::Member(member) = &**expr else {
            return false;
        };
        let MemberProp::Ident(prop) = &member.prop else {
            return false;
        };
        prop.sym.as_ref() == "sendBeacon"
            && matches!(&*member.obj, Expr::Ident(obj) if obj.sym.as_ref() == "navigator")
    }

    /// Root identifier of a callee expression, e.g. `client` in
    /// `client.users.list()` or `client(...)`.
    fn callee_root_ident(expr: &Expr) -> Option<String> {
        match expr {
            Expr::Ident(ident) => Some(ident.sym.to_string()),
            Expr::Member(member) => Self::callee_root_ident(&member.obj),
            Expr::Call(call) => match &call.callee {
                Callee::Expr(e) => Self::callee_root_ident(e),
                _ => None,
            },
            _ => None,
        }
    }

    /// Does the first argument look like a URL (has a network scheme)? This is a
    /// low-noise structural signal that catches bespoke clients without naming
    /// them, e.g. `httpClient('https://api.example.com/users')`.
    fn first_arg_has_url_scheme(call: &CallExpr) -> bool {
        let Some(arg) = call.args.first() else {
            return false;
        };
        let starts_with_scheme = |s: &str| {
            let s = s.trim_start();
            s.starts_with("http://")
                || s.starts_with("https://")
                || s.starts_with("ws://")
                || s.starts_with("wss://")
                || s.starts_with("//")
        };
        match &*arg.expr {
            Expr::Lit(Lit::Str(s)) => starts_with_scheme(s.value.as_ref()),
            Expr::Tpl(tpl) => tpl
                .quasis
                .first()
                .map(|q| starts_with_scheme(q.raw.as_ref()))
                .unwrap_or(false),
            _ => false,
        }
    }

    /// Is the first argument a string or template literal? Combined with an
    /// enclosing `await`, this flags awaited calls like `await load('/data')`.
    fn first_arg_is_stringish(call: &CallExpr) -> bool {
        matches!(
            call.args.first().map(|a| &*a.expr),
            Some(Expr::Lit(Lit::Str(_))) | Some(Expr::Tpl(_))
        )
    }

    /// Signal-7 topic check: is the first argument a string/template literal, or
    /// a bare identifier bound to a top-level `const <id> = "<literal>"`? The
    /// const-ref case (`const SUBJECT = "user.registered"; nc.publish(SUBJECT,
    /// …)`) is the publisher idiom that an inline-literal-only check would miss.
    fn first_arg_is_stringish_or_const_string(&self, call: &CallExpr) -> bool {
        match call.args.first().map(|a| &*a.expr) {
            Some(Expr::Lit(Lit::Str(_))) | Some(Expr::Tpl(_)) => true,
            Some(Expr::Ident(ident)) => self.const_string_values.contains_key(ident.sym.as_ref()),
            _ => false,
        }
    }

    /// Resolve a topic expression to its literal string, or `None` when it is
    /// not deterministically resolvable (carrick#387 anchor-op path). Handles
    /// exactly the shapes the const-string pre-pass supports: an inline string
    /// literal, a reference to a top-level `const <id> = "<literal>"`, and a
    /// template literal whose every interpolation is such a reference
    /// (`` `${name}:pollingStarted` `` with `export const name = '...'`).
    /// Anything else — call results, member expressions, parameters — returns
    /// `None` and the site stays on the LLM path.
    fn resolve_topic_string(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Lit(Lit::Str(s)) => Some(s.value.to_string()),
            Expr::Ident(ident) => self.const_string_values.get(ident.sym.as_ref()).cloned(),
            Expr::Tpl(tpl) => {
                let mut resolved = String::new();
                for (i, quasi) in tpl.quasis.iter().enumerate() {
                    match &quasi.cooked {
                        Some(cooked) => resolved.push_str(cooked),
                        // A quasi with no cooked value contains an invalid
                        // escape — not a resolvable literal.
                        None => return None,
                    }
                    if let Some(interp) = tpl.exprs.get(i) {
                        let Expr::Ident(ident) = &**interp else {
                            return None;
                        };
                        let value = self.const_string_values.get(ident.sym.as_ref())?;
                        resolved.push_str(value);
                    }
                }
                Some(resolved)
            }
            _ => None,
        }
    }

    /// Does this expression occupy a Signal-7 position when it forms a whole
    /// statement expression or variable initializer? Directly a call or a
    /// constructor (`nc.publish(SUBJECT, payload);`, `const w = new
    /// Worker("q", fn)`), or either under a single `await` (`await
    /// consumer.subscribe({ topic });`) — the idiom every promise-returning
    /// subscribe API forces, which the bare-call check missed (carrick#402).
    fn is_pubsub_positioned_expr(expr: &Expr) -> bool {
        match expr {
            Expr::Call(_) | Expr::New(_) => true,
            Expr::Await(awaited) => matches!(&*awaited.arg, Expr::Call(_) | Expr::New(_)),
            _ => false,
        }
    }

    /// The first argument as an object literal (`subscribe({ topic: 'x',
    /// fromBeginning: false })`), or `None` when absent, spread, or any other
    /// shape.
    fn first_arg_object_literal(call: &CallExpr) -> Option<&ObjectLit> {
        let arg = call.args.first()?;
        if arg.spread.is_some() {
            return None;
        }
        match &*arg.expr {
            Expr::Object(obj) => Some(obj),
            _ => None,
        }
    }

    /// Topic values carried by a publish/subscribe options object (carrick#402
    /// shape a): a `topic: <t>` property or a `topics: [<t>, …]` array, where
    /// each `<t>` resolves via [`Self::resolve_topic_string`]. Returns `Some`
    /// when the object carries a protocol-vocabulary topic key at all — the
    /// shape that qualifies the site as a candidate — with the vec holding
    /// only the deterministically resolvable values (used for anchor ops; may
    /// be empty when e.g. the topic is a parameter). Sibling properties
    /// (`fromBeginning`, `groupId`, …) are deliberately tolerated: the key
    /// vocabulary is the signal, not the whole object shape.
    fn object_literal_topic_values(&self, obj: &ObjectLit) -> Option<Vec<String>> {
        let mut has_topic_key = false;
        let mut topics = Vec::new();
        for prop in &obj.props {
            let PropOrSpread::Prop(prop) = prop else {
                continue;
            };
            match &**prop {
                Prop::KeyValue(kv) => {
                    let name = match &kv.key {
                        PropName::Ident(id) => id.sym.to_string(),
                        PropName::Str(s) => s.value.to_string(),
                        _ => continue,
                    };
                    match name.as_str() {
                        "topic" => {
                            has_topic_key = true;
                            if let Some(topic) = self.resolve_topic_string(&kv.value) {
                                topics.push(topic);
                            }
                        }
                        "topics" => {
                            has_topic_key = true;
                            if let Expr::Array(arr) = &*kv.value {
                                for elem in arr.elems.iter().flatten() {
                                    if elem.spread.is_none()
                                        && let Some(topic) = self.resolve_topic_string(&elem.expr)
                                    {
                                        topics.push(topic);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                // Shorthand `{ topic }` references a local binding; resolvable
                // exactly when it is a recorded top-level const string.
                Prop::Shorthand(id) => {
                    if matches!(id.sym.as_ref(), "topic" | "topics") {
                        has_topic_key = true;
                        if let Some(value) = self.const_string_values.get(id.sym.as_ref()) {
                            topics.push(value.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        has_topic_key.then_some(topics)
    }

    /// First parameter of an inline handler function expression, when it is a
    /// simple identifier (`(msg) => …`, `function (msg) { … }`, incl. a typed
    /// `(msg: T)`). `None` for non-function expressions, param-less handlers,
    /// and destructured params — a deterministic locator is only recorded for
    /// the binding shape the sidecar matches by name.
    fn handler_first_ident_param(expr: &Expr) -> Option<(String, swc_common::Span)> {
        let pat = match expr {
            Expr::Arrow(arrow) => arrow.params.first(),
            Expr::Fn(fn_expr) => fn_expr.function.params.first().map(|p| &p.pat),
            _ => None,
        }?;
        match pat {
            Pat::Ident(binding) => Some((binding.id.sym.to_string(), binding.id.span)),
            _ => None,
        }
    }

    /// Emit a candidate for `call`, deduplicating by span so the multiple
    /// broadened signals never double-count one call site.
    fn push_candidate(
        &mut self,
        call: &CallExpr,
        callee_object: String,
        callee_property: Option<String>,
    ) {
        let (span_start, span_end) = self.span_range(call.span);
        if !self.seen_spans.insert((span_start, span_end)) {
            return;
        }
        let line_number = self.get_line_number(call.span);
        let candidate_id = self.candidate_id(span_start, span_end);
        let code_snippet = self.get_code_snippet(call.span);
        // A request-spec call carries its URL as a property, not positionally.
        // Its raw first-arg snippet is the opening brace of the object, so the
        // literal has to come off the AST or the candidate anchors nothing.
        let request_spec = Self::call_request_spec(call);
        let path_snippet = match &request_spec {
            Some(spec) => Some(format!("'{}'", spec.url)),
            None => self.extract_first_arg_snippet(call),
        };
        // What this site says about its own module's request shape, for when
        // another file imports this module as its HTTP wrapper
        // (carrick-cloud#386).
        let request_shape = call_request_shape(call, callee_property.as_deref());

        self.candidates.push(CandidateTarget {
            protocol: Protocol::Http,
            candidate_id,
            span_start,
            span_end,
            line_number,
            callee_object,
            callee_property,
            enclosing_function: self.current_function(),
            path_snippet,
            code_snippet,
            request_spec,
            request_shape,
        });
    }

    /// Emit a candidate from a raw span (for nodes that are not call
    /// expressions, e.g. `new WebSocket(...)` or a route-descriptor object
    /// literal). Deduplicates by span like [`push_candidate`].
    #[allow(clippy::too_many_arguments)]
    fn push_span_candidate(
        &mut self,
        span: swc_common::Span,
        protocol: Protocol,
        callee_object: String,
        callee_property: Option<String>,
        path_snippet: Option<String>,
    ) {
        let (span_start, span_end) = self.span_range(span);
        if !self.seen_spans.insert((span_start, span_end)) {
            return;
        }
        let line_number = self.get_line_number(span);
        let candidate_id = self.candidate_id(span_start, span_end);
        let code_snippet = self.get_code_snippet(span);
        self.candidates.push(CandidateTarget {
            protocol,
            candidate_id,
            span_start,
            span_end,
            line_number,
            callee_object,
            callee_property,
            enclosing_function: self.current_function(),
            path_snippet,
            code_snippet,
            request_spec: None,
            // Not a call expression, so there are no request arguments to read.
            request_shape: RequestShapeSignal::NotARequest,
        });
    }

    /// Extract a code snippet for the given span
    fn get_code_snippet(&self, span: swc_common::Span) -> String {
        self.source_map
            .span_to_snippet(span)
            .unwrap_or_else(|_| "<snippet unavailable>".to_string())
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect::<String>()
    }

    /// Get line number from span
    fn get_line_number(&self, span: swc_common::Span) -> usize {
        self.source_map.lookup_char_pos(span.lo).line
    }

    fn span_range(&self, span: swc_common::Span) -> (u32, u32) {
        (span.lo.0, span.hi.0)
    }

    fn candidate_id(&self, span_start: u32, span_end: u32) -> String {
        format!("span:{}-{}", span_start, span_end)
    }

    fn current_function(&self) -> Option<String> {
        self.function_stack.last().cloned()
    }

    fn extract_first_arg_snippet(&self, call: &CallExpr) -> Option<String> {
        let arg = call.args.first()?;
        self.source_map
            .span_to_snippet(arg.expr.span())
            .ok()
            .map(|s| s.lines().next().unwrap_or("").to_string())
            .map(|s| s.chars().take(120).collect())
    }

    /// The request spec of a call that declares its method and URL as data on
    /// its first argument (`client({ method: "post", url: "/api/v1/login" })`,
    /// `axios({ ... })`, `client.request({ ... })`), or that names its method
    /// as the member it invokes and carries the URL on that object
    /// (`client.post({ url: "/v1/things" })`, see
    /// [`Self::verb_call_request_spec`]). `None` for every other call shape.
    ///
    /// This is the config-object form of an outbound HTTP call, and nothing in
    /// the candidate layer could see it before (#537): no argument is a
    /// string, so the URL-scheme and stringish-argument signals never fire;
    /// the callee is frequently a bare binding (a client passed in as a
    /// parameter), so the member-name heuristics never fire either; and the
    /// object's raw first-line snippet is `{`. The call was invisible, and the
    /// path was whatever the model guessed.
    ///
    /// Purely structural — the property names on the object are the whole
    /// signal, and no client library is named. A producer route declared the
    /// same way (`server.route({ method, url, handler })`) satisfies it too,
    /// which is correct: the candidate carries the route literal either way
    /// and the analyzer decides which side of the contract it is on.
    fn call_request_spec(call: &CallExpr) -> Option<RequestSpec> {
        let arg = call.args.first()?;
        if arg.spread.is_some() {
            return None;
        }
        let Expr::Object(obj) = &*arg.expr else {
            return None;
        };
        if let Some(spec) = Self::request_spec(obj) {
            return Some(spec);
        }
        // The verb-named form: the method is the member being invoked and the
        // object carries only the URL (#529).
        let Callee::Expr(callee) = &call.callee else {
            return None;
        };
        let verb = Self::callee_member_prop(callee)?;
        Self::verb_call_request_spec(&verb, obj)
    }

    /// The request spec of `client.post({ url: "/v1/things" })` — a verb-named
    /// method on a receiver, handed one object literal that carries the URL.
    /// This is how generated OpenAPI clients issue every operation, and until
    /// #529 nothing saw them: the object is the only argument, so no string
    /// argument exists for the URL-scheme or stringish signals to read, and
    /// the receiver is routinely an expression (`(options?.client ?? client)`)
    /// rather than a name the receiver heuristics recognise.
    ///
    /// Three structural guards keep this off producer-side route
    /// registrations, which are the one other thing that puts a route literal
    /// on an object argument of a verb-named method:
    ///
    /// - the URL key must be `url`, the request-side spelling — a declarative
    ///   route spells it `path` (and #241 pins that a `{ method, path }`
    ///   object is never a deterministic route by itself);
    /// - the object may carry no `handler` property and no function value —
    ///   both are the registration side declaring what answers the route,
    ///   which a request has no use for;
    /// - the URL must be route-shaped, so an options bag whose `url` holds a
    ///   bare token is not a request.
    ///
    /// Structural throughout: the shape is the whole signal and no client
    /// library, generator, or framework is named.
    fn verb_call_request_spec(verb: &str, obj: &ObjectLit) -> Option<RequestSpec> {
        if !crate::type_manifest::is_http_method(verb) {
            return None;
        }

        let mut url = None;
        for prop in &obj.props {
            // A spread (`{ ...options, url: "/x" }`) carries no key to read and
            // no handler to fear; the generated clients all pass one.
            let PropOrSpread::Prop(prop) = prop else {
                continue;
            };
            let Prop::KeyValue(kv) = &**prop else {
                continue;
            };
            if matches!(&*kv.value, Expr::Arrow(_) | Expr::Fn(_)) {
                return None;
            }
            let key = match &kv.key {
                PropName::Ident(id) => id.sym.to_string(),
                PropName::Str(s) => s.value.to_string(),
                _ => continue,
            };
            if key == "handler" {
                return None;
            }
            if key == "url"
                && let Expr::Lit(Lit::Str(value)) = &*kv.value
            {
                url = Some(value.value.to_string());
            }
        }

        let url = url?;
        if !RouteDescriptorVisitor::is_route_shaped_path(&url) {
            return None;
        }
        Some(RequestSpec {
            method: verb.trim().to_uppercase(),
            url: normalize_path_params(&url),
            method_from_callee: true,
        })
    }

    /// The `{ method, url }` pair of an object literal, when both are string
    /// literals, the method is an HTTP verb, and the URL is route-shaped
    /// (leading `/` or an http(s) URL). `path` is accepted as the URL key for
    /// the clients that spell it that way.
    ///
    /// The three guards are what keep this off ordinary config objects: an
    /// options bag with a `method` naming an RPC operation, or a `url` holding
    /// a bare token, is not a request. Deliberately separate from
    /// [`Self::route_descriptor`], which owns the producer-side registry shape
    /// and must not start recognising `url` (#241 pins that a standalone
    /// `{ method, path }` object is never a deterministic endpoint).
    fn request_spec(node: &ObjectLit) -> Option<RequestSpec> {
        let mut method = None;
        let mut url = None;

        for prop in &node.props {
            let PropOrSpread::Prop(prop) = prop else {
                continue;
            };
            let Prop::KeyValue(kv) = &**prop else {
                continue;
            };
            let key = match &kv.key {
                PropName::Ident(id) => id.sym.to_string(),
                PropName::Str(s) => s.value.to_string(),
                _ => continue,
            };
            let Expr::Lit(Lit::Str(value)) = &*kv.value else {
                continue;
            };
            match key.as_str() {
                "method" => method = Some(value.value.to_string()),
                // `url` wins over `path` when a config carries both.
                "url" => url = Some(value.value.to_string()),
                "path" if url.is_none() => url = Some(value.value.to_string()),
                _ => {}
            }
        }

        let (method, url) = (method?, url?);
        if !crate::type_manifest::is_http_method(&method) {
            return None;
        }
        if !RouteDescriptorVisitor::is_route_shaped_path(&url) {
            return None;
        }
        Some(RequestSpec {
            method: method.trim().to_uppercase(),
            url: normalize_path_params(&url),
            method_from_callee: false,
        })
    }

    /// Inspect an object literal for the route-descriptor shape
    /// (`{ method, path, handler }`). Returns the path literal snippet and the
    /// handler identifier when the object carries *both* a `method` and a
    /// `path` property; otherwise `None`. Only string-keyed (ident or string)
    /// properties are considered, so spread/computed config objects don't
    /// accidentally match.
    fn route_descriptor(node: &ObjectLit) -> Option<RouteDescriptor> {
        let key_name = |key: &PropName| -> Option<String> {
            match key {
                PropName::Ident(id) => Some(id.sym.to_string()),
                PropName::Str(s) => Some(s.value.to_string()),
                _ => None,
            }
        };

        let mut has_method = false;
        let mut has_path = false;
        let mut method = None;
        let mut path = None;
        let mut handler = None;

        for prop in &node.props {
            let PropOrSpread::Prop(prop) = prop else {
                continue;
            };
            let Prop::KeyValue(kv) = &**prop else {
                continue;
            };
            let Some(name) = key_name(&kv.key) else {
                continue;
            };
            match name.as_str() {
                "method" => {
                    has_method = true;
                    // Keep the method literal so the route can be emitted
                    // deterministically (#234). A non-literal method (computed
                    // expr) still satisfies the shape guard but yields no
                    // deterministic emission — only the recall-boost candidate.
                    if let Expr::Lit(Lit::Str(s)) = &*kv.value {
                        method = Some(s.value.to_string());
                    }
                }
                "path" => {
                    has_path = true;
                    if let Expr::Lit(Lit::Str(s)) = &*kv.value {
                        path = Some(s.value.to_string());
                    }
                }
                "handler" => {
                    if let Expr::Ident(id) = &*kv.value {
                        handler = Some(id.sym.to_string());
                    }
                }
                _ => {}
            }
        }

        (has_method && has_path).then_some(RouteDescriptor {
            method,
            path,
            handler,
        })
    }

    /// Property name of a member-expression callee (`axios.post(...)` ->
    /// "post", `w['post'](...)` -> "post"). `None` for non-member callees and
    /// non-literal computed properties. Used by the structural signals (URL
    /// scheme, awaited stringish call) so the candidate hint carries the full
    /// `object.property` callee even when the package is absent from the
    /// LLM-detected `data_fetchers` list — without this, the same call site's
    /// hint flips between `axios.post` and bare `axios` depending on a per-run
    /// LLM detection output, which is exactly the kind of prompt variance the
    /// candidate layer exists to prevent.
    fn callee_member_prop(expr: &Expr) -> Option<String> {
        let Expr::Member(member) = expr else {
            return None;
        };
        match &member.prop {
            MemberProp::Ident(id) => Some(id.sym.to_string()),
            MemberProp::Computed(c) => match &*c.expr {
                Expr::Lit(Lit::Str(s)) => Some(s.value.to_string()),
                _ => None,
            },
            MemberProp::PrivateName(_) => None,
        }
    }

    /// Extract callee object name from expression
    fn extract_callee_object(expr: &Expr) -> Option<String> {
        match expr {
            Expr::Ident(ident) => Some(ident.sym.to_string()),
            Expr::Member(member) => Self::extract_callee_object(&member.obj),
            Expr::Call(call) => {
                // Handle chained calls like createApp().get()
                if let Callee::Expr(callee_expr) = &call.callee {
                    Self::extract_callee_object(callee_expr)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

impl Visit for CandidateVisitor {
    fn visit_fn_decl(&mut self, node: &FnDecl) {
        let name = Some(node.ident.sym.to_string());
        self.function_stack.push(name.clone().unwrap());
        node.visit_children_with(self);
        self.function_stack.pop();
    }

    fn visit_fn_expr(&mut self, node: &FnExpr) {
        if let Some(ident) = &node.ident {
            self.function_stack.push(ident.sym.to_string());
        }
        node.visit_children_with(self);
        if node.ident.is_some() {
            self.function_stack.pop();
        }
    }

    fn visit_arrow_expr(&mut self, node: &ArrowExpr) {
        self.function_stack.push("<arrow>".to_string());
        node.visit_children_with(self);
        self.function_stack.pop();
    }

    fn visit_class_method(&mut self, node: &ClassMethod) {
        if let Some(name) = match &node.key {
            PropName::Ident(id) => Some(id.sym.to_string()),
            PropName::Str(s) => Some(s.value.to_string()),
            _ => None,
        } {
            self.function_stack.push(name);
            node.visit_children_with(self);
            self.function_stack.pop();
        } else {
            node.visit_children_with(self);
        }
    }

    fn visit_method_prop(&mut self, node: &MethodProp) {
        if let Some(name) = match &node.key {
            PropName::Ident(id) => Some(id.sym.to_string()),
            PropName::Str(s) => Some(s.value.to_string()),
            _ => None,
        } {
            self.function_stack.push(name);
            node.visit_children_with(self);
            self.function_stack.pop();
        } else {
            node.visit_children_with(self);
        }
    }

    fn visit_decorator(&mut self, node: &Decorator) {
        // Emit a candidate for any decorator call expression. This is the
        // framework-agnostic path for class-method routing (NestJS) — the
        // scanner stays free of framework names; the LLM classifies the
        // decorator by its identifier via the Import Table.
        if let Expr::Call(call) = &*node.expr
            && let Callee::Expr(callee_expr) = &call.callee
            && let Some(name) = Self::extract_callee_object(callee_expr)
        {
            self.push_candidate(call, name, None);
        }
        node.visit_children_with(self);
    }

    fn visit_await_expr(&mut self, node: &AwaitExpr) {
        self.await_depth += 1;
        node.visit_children_with(self);
        self.await_depth -= 1;
    }

    fn visit_expr_stmt(&mut self, node: &ExprStmt) {
        // A statement-expression that *is* a call (`nc.publish(SUBJECT, payload);`)
        // is one of the two Signal-7 positions. Mark it so the call expr it wraps
        // is eligible; `visit_call_expr` clears the flag before descending so
        // nested calls don't inherit the position.
        if Self::is_pubsub_positioned_expr(&node.expr) {
            self.in_pubsub_call_position = true;
        }
        node.visit_children_with(self);
        self.in_pubsub_call_position = false;
    }

    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        // A variable initializer that *is* a call (`const sub = nc.subscribe("topic");`)
        // is the other Signal-7 position. Same flag-clearing discipline as
        // `visit_expr_stmt`.
        if node
            .init
            .as_deref()
            .is_some_and(Self::is_pubsub_positioned_expr)
        {
            self.in_pubsub_call_position = true;
        }
        node.visit_children_with(self);
        self.in_pubsub_call_position = false;
    }

    fn visit_new_expr(&mut self, node: &NewExpr) {
        // Snapshot-and-clear the Signal-7 position flag exactly like
        // `visit_call_expr`: the constructor-worker anchor below only fires
        // when the NewExpr itself is the statement expression or variable
        // initializer, and calls nested inside its arguments must not inherit
        // the position.
        let in_pubsub_call_position = self.in_pubsub_call_position;
        self.in_pubsub_call_position = false;

        // Constructor-registered subscriber (carrick#402 shape b): `new
        // X("literal", fn, …)` — the BullMQ `new Worker(queueName, handler)`
        // idiom, where the queue name is the contract topic. GATED on the
        // constructor identifier being an import binding resolved from a
        // framework-detect `messaging_clients` package — NOT merely any
        // file-level import — which is what keeps `new CronJob("0 * * * *",
        // fn)` from becoming a phantom subscriber. The second argument must be
        // an inline function (a reference identifier could be an options
        // object). Payload-less by policy: the handler receives a job ENVELOPE
        // (`Job`), so a deterministic param locator would replace an honest
        // Unknown with a wrong type.
        if in_pubsub_call_position
            && let Expr::Ident(ident) = &*node.callee
            && self.messaging_import_locals.contains(ident.sym.as_ref())
            && let Some(args) = node.args.as_ref()
            && args.len() >= 2
            && args[0].spread.is_none()
            && args[1].spread.is_none()
            && matches!(&*args[1].expr, Expr::Arrow(_) | Expr::Fn(_))
            && let Some(topic) = self.resolve_topic_string(&args[0].expr)
        {
            let path_snippet = self
                .source_map
                .span_to_snippet(args[0].expr.span())
                .ok()
                .map(|s| s.lines().next().unwrap_or("").chars().take(120).collect());
            self.push_span_candidate(
                node.span,
                // Same routing as the Signal 7 call-site candidates: the
                // pub/sub guidance lives in the HTTP analyze-file prompt, so
                // this must reach it rather than be set aside as unrouted.
                Protocol::Http,
                ident.sym.to_string(),
                None,
                path_snippet,
            );
            self.pubsub_anchor_ops.push(PubsubAnchorOp {
                topic,
                role: PubsubRole::Subscriber,
                line_number: self.get_line_number(node.span),
                handler_param: None,
                handler_param_line: None,
            });
        }

        // Network primitives constructed with `new`: `new WebSocket(url)`,
        // `new EventSource(url)`, `new XMLHttpRequest()`. Emitting these as
        // candidates keeps files using them from being skipped by the gate.
        if let Expr::Ident(ident) = &*node.callee
            && matches!(
                ident.sym.as_ref(),
                "WebSocket" | "EventSource" | "XMLHttpRequest"
            )
        {
            let path_snippet = node
                .args
                .as_ref()
                .and_then(|args| args.first())
                .and_then(|a| self.source_map.span_to_snippet(a.expr.span()).ok())
                .map(|s| s.lines().next().unwrap_or("").chars().take(120).collect());
            // XMLHttpRequest is an HTTP client; WebSocket and EventSource
            // belong to the socket family (SSE rides the socket model) and
            // must not reach the HTTP prompt.
            let protocol = if ident.sym.as_ref() == "XMLHttpRequest" {
                Protocol::Http
            } else {
                Protocol::Websocket
            };
            self.push_span_candidate(
                node.span,
                protocol,
                ident.sym.to_string(),
                None,
                path_snippet,
            );
        }
        node.visit_children_with(self);
    }

    fn visit_object_lit(&mut self, node: &ObjectLit) {
        // Signal 6: route-descriptor object literals — a declarative routing
        // shape where the method, path, and handler are *data*, not a method
        // call (`{ method: 'GET', path: '/health', handler: healthCheckHandler }`,
        // typically collected in a `routeRegistry`-style array and registered
        // in a loop). None of the call-site signals fire on such a file, so the
        // gate would skip it and the endpoint would be missed entirely.
        //
        // The shape guard requires *both* a `method` and a `path` property to
        // avoid flagging ordinary config objects. The candidate is keyed on the
        // `handler` identifier when present so the hint points the LLM at the
        // real owner (the handler fn), not the HTTP method string — the
        // owner-fabrication trap.
        //
        // When the method and path are both string literals the route is now
        // emitted deterministically by the orchestrator (`route_descriptor_endpoints`,
        // #234), bypassing the LLM. This candidate stays as a recall booster for
        // the dynamic-handler cases the deterministic path can't own (e.g. a
        // computed method/path, or a handler that isn't a bare identifier): the
        // gate still keeps the file and the LLM classifies it.
        if let Some(descriptor) = Self::route_descriptor(node) {
            let path_snippet = descriptor.path.map(|p| format!("'{}'", p));
            self.push_span_candidate(
                node.span,
                Protocol::Http,
                descriptor
                    .handler
                    .unwrap_or_else(|| "<route-descriptor>".to_string()),
                None,
                path_snippet,
            );
        }
        node.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        // Snapshot the Signal-7 position flag set by the enclosing
        // `visit_expr_stmt` / `visit_var_declarator`, then clear it so calls
        // nested inside this call's arguments/callee don't inherit the position.
        let in_pubsub_call_position = self.in_pubsub_call_position;
        self.in_pubsub_call_position = false;

        // Signal 1: global fetch primitive.
        if self.is_global_network_call(&call.callee) {
            self.push_candidate(call, "fetch".to_string(), None);
        }

        // Signal 1b: `navigator.sendBeacon(url, ...)` — a web-platform HTTP POST
        // primitive. Its first argument is the URL, so the existing
        // `push_candidate` (which records the first-arg path snippet and tags
        // Protocol::Http) routes it through the HTTP prompt, where the method is
        // inferred as POST. Recognized by structural shape, no client allowlist.
        if Self::is_navigator_send_beacon(&call.callee) {
            self.push_candidate(
                call,
                "navigator".to_string(),
                Some("sendBeacon".to_string()),
            );
        }

        // Signal 2: call rooted at an identifier imported from a known
        // network/data-fetching package (covers wrappers regardless of method
        // name), or direct invocation of such an import (`client(url)`).
        if let Callee::Expr(callee_expr) = &call.callee
            && let Some(root) = Self::callee_root_ident(callee_expr)
            && self.network_import_locals.contains(&root)
        {
            // Same member-property extraction as Signals 3/4 (incl. computed
            // string properties like `client["post"](url)`), so every signal
            // that can emit this span first labels it identically.
            let property = Self::callee_member_prop(callee_expr);
            self.push_candidate(call, root, property);
        }

        // Signal 2b: the call declares its method and URL as data on a single
        // object-literal argument (`client({ method: "post", url: "/x" })`).
        // None of the other call-site signals can see this shape — see
        // `call_request_spec` — so without this one the call raises no
        // candidate at all and the file can be skipped before the analyzer
        // ever reads it (#537).
        if Self::call_request_spec(call).is_some() {
            let (obj, prop) = match &call.callee {
                Callee::Expr(e) => (
                    Self::extract_callee_object(e).unwrap_or_else(|| "<request-spec>".to_string()),
                    Self::callee_member_prop(e),
                ),
                _ => ("<request-spec>".to_string(), None),
            };
            self.push_candidate(call, obj, prop);
        }

        // Signal 3: first argument is a URL with a network scheme.
        if Self::first_arg_has_url_scheme(call) {
            let (obj, prop) = match &call.callee {
                Callee::Expr(e) => (
                    Self::extract_callee_object(e).unwrap_or_else(|| "<url-call>".to_string()),
                    Self::callee_member_prop(e),
                ),
                _ => ("<url-call>".to_string(), None),
            };
            self.push_candidate(call, obj, prop);
        }

        // Signal 4: awaited call with a string/template argument.
        if self.await_depth > 0 && Self::first_arg_is_stringish(call) {
            let (obj, prop) = match &call.callee {
                Callee::Expr(e) => (
                    Self::extract_callee_object(e).unwrap_or_else(|| "<awaited-call>".to_string()),
                    Self::callee_member_prop(e),
                ),
                _ => ("<awaited-call>".to_string(), None),
            };
            self.push_candidate(call, obj, prop);
        }

        // Signal 5 (existing): method calls matching the API name heuristics.
        if let Callee::Expr(callee_expr) = &call.callee
            && let Expr::Member(member) = &**callee_expr
        {
            let method_name = match &member.prop {
                MemberProp::Ident(ident) => Some(ident.sym.to_string()),
                MemberProp::Computed(computed) => {
                    if let Expr::Lit(Lit::Str(s)) = &*computed.expr {
                        Some(s.value.to_string())
                    } else {
                        None
                    }
                }
                MemberProp::PrivateName(_) => None,
            };

            if let Some(method) = method_name {
                let obj_name = Self::extract_callee_object(&member.obj);

                let is_api_call = match &obj_name {
                    Some(name) => {
                        self.is_potential_api_object(name) || self.is_potential_api_method(&method)
                    }
                    None => self.is_potential_api_method(&method),
                };

                if is_api_call {
                    self.push_candidate(
                        call,
                        obj_name.unwrap_or_else(|| "<chain>".to_string()),
                        Some(method),
                    );
                }
            }
        }

        // Signal 8 (UNGATED): web-platform cross-context messaging. postMessage
        // sends a message envelope to another browsing context (parent window,
        // iframe contentWindow, worker, message port, broadcast channel);
        // addEventListener('message', …) registers the receiving side. Like
        // `navigator.sendBeacon` (Signal 1b) these are web-platform primitives
        // with no package import to gate on, so they are recognized purely by
        // shape: the `postMessage` property name, and the literal 'message'
        // event name on the listener side. Unlike Signal 7 the topic is NOT the
        // first argument — it is a string literal on the envelope's
        // `action`/`type` property (send side) or in the handler's dispatch
        // cases (receive side) — so topic extraction is LLM-side; the shape
        // only routes the file. Topicless transfers (`worker.postMessage(buf)`)
        // surface as candidates too and are rejected there.
        if let Callee::Expr(callee_expr) = &call.callee {
            // The callee name comes from a member property (`window.parent
            // .postMessage`, incl. computed string form `w['postMessage']`) or
            // a bare identifier (`postMessage(...)` / `addEventListener(
            // 'message', ...)` in worker/global scope, where the receiver is
            // the implicit global).
            let (receiver, prop_name): (Option<&Expr>, Option<String>) = match &**callee_expr {
                Expr::Member(member) => {
                    let name = match &member.prop {
                        MemberProp::Ident(id) => Some(id.sym.to_string()),
                        MemberProp::Computed(c) => match &*c.expr {
                            Expr::Lit(Lit::Str(s)) => Some(s.value.to_string()),
                            _ => None,
                        },
                        MemberProp::PrivateName(_) => None,
                    };
                    (Some(&*member.obj), name)
                }
                Expr::Ident(id) => (None, Some(id.sym.to_string())),
                _ => (None, None),
            };
            // Receiver must not be a string/number literal (a `"str".method()`
            // chain is never a messaging context).
            let receiver_ok = !matches!(
                receiver,
                Some(Expr::Lit(Lit::Str(_))) | Some(Expr::Lit(Lit::Num(_)))
            );
            if receiver_ok && let Some(prop_name) = prop_name {
                let is_post_message = prop_name == "postMessage";
                let is_message_listener = prop_name == "addEventListener"
                    && matches!(
                        call.args.first().map(|a| &*a.expr),
                        Some(Expr::Lit(Lit::Str(s))) if s.value.as_ref() == "message"
                    );
                if is_post_message || is_message_listener {
                    let obj_name = receiver
                        .and_then(Self::extract_callee_object)
                        .unwrap_or_else(|| "<global>".to_string());
                    self.push_candidate(call, obj_name, Some(prop_name));
                }
            }
        }

        // Signal 7 (GATED): fire-and-forget pub/sub call sites. The
        // publish/subscribe shape (`nc.publish(SUBJECT, payload);`,
        // `const sub = nc.subscribe("topic")`) is a member call with a
        // string/const-string topic as its first argument, but unlike an HTTP
        // call it is not awaited and the method name is library-specific
        // (publish/subscribe/emit/produce/…), so the other signals miss it
        // inconsistently. Surfacing is TWO-TIER (carrick#317): tier 1 — the
        // file imports a messaging-client package
        // (`file_imports_messaging_client`), any member-call shape qualifies;
        // tier 2 — no gating import but the repo detected messaging clients
        // (`repo_has_messaging_clients`, the injected/inherited-client case),
        // then only calls literally named publish/subscribe qualify. The shape
        // is identical to `socket.emit('x')` and `logger.info('x')`; the
        // gating is what keeps this from firing on socket.io / logging files
        // (socket.io is not a messaging client, and tier 2's method-name
        // constraint excludes emit/info), so it has zero socket-skip /
        // corpus-1 collateral. With empty `messaging_clients` both tiers are
        // off and this branch is inert.
        if (self.file_imports_messaging_client || self.repo_has_messaging_clients)
            && in_pubsub_call_position
            && let Callee::Expr(callee_expr) = &call.callee
            && let Expr::Member(member) = &**callee_expr
            // Receiver must not be a string/number literal — that would be a
            // `"str".method()` / `(123).method()` chain, not a pub/sub client.
            && !matches!(&*member.obj, Expr::Lit(Lit::Str(_)) | Expr::Lit(Lit::Num(_)))
        {
            let method = match &member.prop {
                MemberProp::Ident(id) => Some(id.sym.to_string()),
                MemberProp::Computed(c) => match &*c.expr {
                    Expr::Lit(Lit::Str(s)) => Some(s.value.to_string()),
                    _ => None,
                },
                MemberProp::PrivateName(_) => None,
            };
            let role = match method.as_deref() {
                Some("publish") => Some(PubsubRole::Publisher),
                Some("subscribe") => Some(PubsubRole::Subscriber),
                _ => None,
            };
            // First-argument shapes that qualify the site: the direct
            // stringish/const-string topic (unchanged), or — on calls literally
            // named publish/subscribe only — an options object carrying a
            // protocol-vocabulary `topic`/`topics` key, the kafkajs
            // `subscribe({ topic, fromBeginning })` idiom (carrick#402 shape a).
            // Restricting the object shape to the vocabulary-named methods
            // keeps `client.run({ eachMessage })` / arbitrary `configure({...})`
            // config calls inert.
            let stringish_topic = self.first_arg_is_stringish_or_const_string(call);
            let object_topics = if role.is_some() {
                Self::first_arg_object_literal(call)
                    .and_then(|obj| self.object_literal_topic_values(obj))
            } else {
                None
            };
            // Two-tier gate (carrick#317). Tier 1: the file imports a detected
            // messaging client — any member-call shape qualifies (unchanged).
            // Tier 2: the file has NO gating import (injected/inherited client,
            // e.g. `this.messenger` provided by a base class) but the repo has
            // detected messaging clients — then the method NAME must be the
            // pub/sub protocol vocabulary itself (`publish`/`subscribe`), which
            // keeps `logger.info('msg')` / `socket.emit('evt')` / RxJS
            // `.subscribe(fn)` (function arg, already excluded by the
            // first-argument shape checks) inert.
            let gates_in = self.file_imports_messaging_client || role.is_some();
            if (stringish_topic || object_topics.is_some()) && gates_in {
                let obj_name = Self::extract_callee_object(&member.obj)
                    .unwrap_or_else(|| "<pubsub>".to_string());

                // Anchor-op path (carrick#387 + #402), a strict subset of the
                // sites gated in above: only calls literally named
                // publish/subscribe (the protocol vocabulary — stricter than
                // tier 1's any-method candidate) whose topic resolves to a
                // literal are structural facts. Assert them deterministically
                // so an LLM extraction omission cannot lose them;
                // payload-carrying calls stay LLM-owned (locator judgment,
                // envelope unwrapping).
                if let Some(role) = role {
                    let line_number = self.get_line_number(call.span);
                    if let Some(topics) = &object_topics {
                        // Options-object shape (#402 a): every resolvable
                        // topic anchors, payload-less — the message handler is
                        // registered elsewhere (`run({ eachMessage })`) and
                        // receives an envelope, so there is no deterministic
                        // payload locator to record. SUBSCRIBER role only
                        // (Copilot review on #409): a `publish({ topic, ... })`
                        // options object may carry the payload as a sibling
                        // property, so a payload-less publisher assertion could
                        // put a typeless op on the wire where the LLM's locator
                        // judgment should own the site. Publisher object shapes
                        // keep the candidate below but stay LLM-owned.
                        if role == PubsubRole::Subscriber && call.args.len() == 1 {
                            for topic in topics {
                                self.pubsub_anchor_ops.push(PubsubAnchorOp {
                                    topic: topic.clone(),
                                    role,
                                    line_number,
                                    handler_param: None,
                                    handler_param_line: None,
                                });
                            }
                        }
                    } else if call.args.len() == 1
                        && call.args[0].spread.is_none()
                        && let Some(topic) = self.resolve_topic_string(&call.args[0].expr)
                    {
                        // Payload-less single-arg call (#387, unchanged).
                        self.pubsub_anchor_ops.push(PubsubAnchorOp {
                            topic,
                            role,
                            line_number,
                            handler_param: None,
                            handler_param_line: None,
                        });
                    } else if role == PubsubRole::Subscriber
                        && call.args.len() == 2
                        && call.args.iter().all(|a| a.spread.is_none())
                        && matches!(&*call.args[1].expr, Expr::Arrow(_) | Expr::Fn(_))
                        && let Some(topic) = self.resolve_topic_string(&call.args[0].expr)
                    {
                        // Two-arg `subscribe("topic", handler)` (#402 c): the
                        // inline function second argument is a handler, not a
                        // payload, so the op is still structurally certain.
                        // Its first param — when a simple identifier — is the
                        // decoded-payload binding, recorded as a FunctionParam
                        // locator for the sidecar. A function REFERENCE second
                        // arg stays LLM-owned: an identifier could be an
                        // options object.
                        let handler = Self::handler_first_ident_param(&call.args[1].expr);
                        self.pubsub_anchor_ops.push(PubsubAnchorOp {
                            topic,
                            role,
                            line_number,
                            handler_param: handler.as_ref().map(|(name, _)| name.clone()),
                            handler_param_line: handler.map(|(_, span)| self.get_line_number(span)),
                        });
                    }
                }

                self.push_candidate(call, obj_name, method);
            }
        }

        // Continue visiting child nodes
        call.visit_children_with(self);
    }
}

/// Collect the module-specifier string of every `import ... from '<source>'`
/// declaration in the module (e.g. `"nats"`, `"@nats-io/nats-core"`). Used by
/// the file-orchestrator to force-analyze zero-candidate files that import a
/// recognized messaging-client package (pub/sub Part B).
fn collect_import_sources(module: &Module) -> Vec<String> {
    module
        .body
        .iter()
        .filter_map(|item| match item {
            ModuleItem::ModuleDecl(ModuleDecl::Import(import)) => {
                Some(import.src.value.to_string())
            }
            _ => None,
        })
        .collect()
}

/// Does any of this module's import specifiers match a `messaging_clients`
/// entry? Gate for the pub/sub call-site Signal 7.
///
/// An import source matches when it is exactly the entry (`"nats"` matches
/// `"nats"`) or a subpath under it (`"nats"` matches `"nats/foo"`). Matching is
/// strictly exact-or-`"<entry>/"`-prefix, so `"nats"` does NOT match
/// `"@nats-io/nats-core"` — a scoped client gates only when that scoped name
/// (e.g. `"@nats-io/nats-core"`, or `"@nats-io"` as a `"@nats-io/"` prefix) is
/// itself a `messaging_clients` entry.
/// This is the same matching convention as
/// `FileOrchestrator::imports_messaging_client` and the data-fetcher
/// import-recall check, kept in sync deliberately so a package gates the same
/// way everywhere without a hardcoded list. Empty `messaging_clients` → false.
fn file_imports_messaging_client(import_sources: &[String], messaging_clients: &[String]) -> bool {
    if messaging_clients.is_empty() {
        return false;
    }
    import_sources.iter().any(|src| {
        messaging_clients
            .iter()
            .any(|pkg| src == pkg || src.starts_with(&format!("{}/", pkg)))
    })
}

/// Collect top-level `const <id> = "<string-literal>"` bindings (name ->
/// literal value) so a pub/sub call whose topic is a const reference (`const
/// SUBJECT = "user.registered"; nc.publish(SUBJECT, …)`) still counts as having
/// a string-literal topic for Signal 7, and so the anchor-op path (carrick#387)
/// can resolve const-ref and template-literal topics to their literal strings.
/// Only module-body `const` declarators with an identifier pattern and a bare
/// string-literal initializer are recorded — this is a targeted recall booster,
/// not a general constant-folder, so template literals, member exprs, and
/// nested scopes are intentionally ignored.
fn collect_const_string_values(module: &Module) -> HashMap<String, String> {
    let mut bindings = HashMap::new();
    for item in &module.body {
        let var = match item {
            ModuleItem::Stmt(Stmt::Decl(Decl::Var(var))) => var,
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => match &export.decl {
                Decl::Var(var) => var,
                _ => continue,
            },
            _ => continue,
        };
        if var.kind != VarDeclKind::Const {
            continue;
        }
        for decl in &var.decls {
            let Pat::Ident(ident) = &decl.name else {
                continue;
            };
            if let Some(init) = &decl.init
                && let Expr::Lit(Lit::Str(value)) = &**init
            {
                bindings.insert(ident.id.sym.to_string(), value.value.to_string());
            }
        }
    }
    bindings
}

/// Collect the local binding names introduced by imports from any of the
/// listed packages, covering default, named (incl. aliases), and namespace
/// imports. Matched exactly or as a scope/subpath prefix
/// (`pkg`, `@scope/pkg`, `pkg/sub`).
///
/// The package list comes from framework detection — the LLM decides which of
/// the repo's dependencies are data-fetching libraries / messaging clients —
/// so the scanner carries no hardcoded package list. Called with
/// `data_fetchers` for the network-call Signal 2 and with `messaging_clients`
/// for the NewExpr subscriber gate (carrick#402). This is a recall booster for
/// the gatekeeper, not an authoritative classification: the LLM still decides
/// what each call is. Matching is the same exact-or-`"<pkg>/"`-prefix
/// convention as `file_imports_messaging_client`.
fn package_import_locals(module: &Module, packages: &[String]) -> HashSet<String> {
    let is_listed = |src: &str| {
        packages
            .iter()
            .any(|pkg| src == pkg || src.starts_with(&format!("{}/", pkg)))
    };
    let mut locals = HashSet::new();
    for item in &module.body {
        let ModuleItem::ModuleDecl(ModuleDecl::Import(import)) = item else {
            continue;
        };
        if !is_listed(import.src.value.as_ref()) {
            continue;
        }
        for spec in &import.specifiers {
            match spec {
                ImportSpecifier::Default(d) => {
                    locals.insert(d.local.sym.to_string());
                }
                ImportSpecifier::Named(n) => {
                    locals.insert(n.local.sym.to_string());
                }
                ImportSpecifier::Namespace(ns) => {
                    locals.insert(ns.local.sym.to_string());
                }
            }
        }
    }
    locals
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scan_test_content(content: &str) -> ScanResult {
        scan_test_content_with_fetchers(content, &[])
    }

    fn scan_test_content_with_fetchers(content: &str, data_fetchers: &[String]) -> ScanResult {
        let scanner = SwcScanner::new();
        let path = PathBuf::from("test.ts");
        scanner.scan_content(&path, content, data_fetchers, &[])
    }

    fn handler_names(content: &str) -> Vec<String> {
        let scanner = SwcScanner::new();
        let mut names: Vec<String> = scanner
            .exported_handlers(&PathBuf::from("route.ts"), content)
            .into_iter()
            .map(|h| h.name)
            .collect();
        names.sort();
        names
    }

    #[test]
    fn scan_content_flags_parse_failures() {
        let result = scan_test_content("function broken( {{{");
        assert!(result.parse_failed);
        assert!(result.candidates.is_empty());

        let healthy = scan_test_content("const x = 1;");
        assert!(!healthy.parse_failed);
        assert!(healthy.candidates.is_empty());
    }

    #[test]
    fn candidate_hint_is_stable_across_data_fetcher_detection() {
        // carrick#359: the candidate for a member call must carry the same
        // `object.property` callee whether or not the package appears in the
        // LLM-detected data_fetchers list. Before this guard, `axios.post`
        // (Signal 2, import-gated) degraded to bare `axios` with a null
        // property (Signal 4, awaited-stringish) whenever a scan's detect
        // output omitted axios — a per-run LLM output mutating the candidate
        // hint the file-analyzer prompt presents.
        let content = r#"
import axios from "axios";
export async function record(event: unknown): Promise<void> {
  await axios.post(`${process.env.HOOK_URL ?? "http://localhost:3099"}/audit/events`, event);
}
"#;

        let with_fetcher = scan_test_content_with_fetchers(content, &["axios".to_string()]);
        let without_fetcher = scan_test_content(content);

        let pick = |r: &ScanResult| {
            let c = r
                .candidates
                .iter()
                .find(|c| c.callee_object == "axios")
                .expect("axios candidate must be emitted")
                .clone();
            (
                c.candidate_id.clone(),
                c.callee_object.clone(),
                c.callee_property.clone(),
                c.path_snippet.clone(),
            )
        };
        let a = pick(&with_fetcher);
        let b = pick(&without_fetcher);
        assert_eq!(a, b, "candidate must not depend on data_fetchers");
        assert_eq!(a.2.as_deref(), Some("post"));
    }

    #[test]
    fn imported_fetcher_computed_property_call_carries_property() {
        // Signal 2 (import-gated) must extract computed string properties the
        // same way Signals 3/4 do, so whichever signal emits a span first
        // labels it identically.
        let content = r#"
import client from "client-lib";
export async function load() {
  await client["post"]("/things", {});
}
"#;
        let result = scan_test_content_with_fetchers(content, &["client-lib".to_string()]);
        let c = result
            .candidates
            .iter()
            .find(|c| c.callee_object == "client")
            .expect("imported-fetcher candidate must be emitted");
        assert_eq!(c.callee_property.as_deref(), Some("post"));
    }

    #[test]
    fn url_scheme_candidate_carries_member_property() {
        // Signal 3 (URL-scheme first arg) on a member callee that is neither a
        // known API object nor a detected data-fetcher import: the hint should
        // still name the full callee, not just the root object.
        let content = r#"
export function ping(myTransport: { fire(u: string): void }) {
  myTransport.fire("https://example.com/ping");
}
"#;
        let result = scan_test_content(content);
        let c = result
            .candidates
            .iter()
            .find(|c| c.callee_object == "myTransport")
            .expect("url-scheme candidate must be emitted");
        assert_eq!(c.callee_property.as_deref(), Some("fire"));
    }

    /// The method guards read off one named export.
    fn handler_guards(content: &str, export: &str) -> Vec<String> {
        let scanner = SwcScanner::new();
        scanner
            .exported_handlers(&PathBuf::from("route.ts"), content)
            .into_iter()
            .find(|h| h.name == export)
            .unwrap_or_else(|| panic!("export {export} not found"))
            .method_guards
    }

    // --- Method guards (carrick#601) ---

    #[test]
    fn method_guard_read_from_a_negated_comparison() {
        let content = r#"
export async function action({ request }: { request: Request }) {
  if (request.method !== "PUT") {
    throw new Response("Method Not Allowed", { status: 405 });
  }
  return Response.json({});
}
"#;
        assert_eq!(handler_guards(content, "action"), vec!["PUT"]);
    }

    #[test]
    fn method_guard_read_through_a_destructured_local() {
        let content = r#"
export async function action({ request }: { request: Request }) {
  const { method } = request;
  if (method !== "GET") throw new Response(null, { status: 405 });
  return Response.json({});
}
"#;
        assert_eq!(handler_guards(content, "action"), vec!["GET"]);
    }

    #[test]
    fn method_guard_read_through_a_renamed_local() {
        let content = r#"
export async function action({ request }: { request: Request }) {
  const verb = request.method;
  if (verb === "DELETE") return Response.json({});
  throw new Response(null, { status: 405 });
}
"#;
        assert_eq!(handler_guards(content, "action"), vec!["DELETE"]);
    }

    #[test]
    fn method_guard_read_from_a_switch() {
        let content = r#"
export async function action({ request }: { request: Request }) {
  switch (request.method) {
    case "PUT":
      return Response.json({});
    case "DELETE":
      return new Response(null, { status: 204 });
    default:
      throw new Response(null, { status: 405 });
  }
}
"#;
        assert_eq!(handler_guards(content, "action"), vec!["PUT", "DELETE"]);
    }

    #[test]
    fn method_guard_read_from_a_call_expression_export() {
        // The handler is the result of a factory call, so the guard lives in
        // the callback the factory receives.
        let content = r#"
export const action = withAuth(async ({ request }) => {
  if (request.method !== "PATCH") throw new Response(null, { status: 405 });
  return Response.json({});
});
"#;
        assert_eq!(handler_guards(content, "action"), vec!["PATCH"]);
    }

    #[test]
    fn method_guard_read_through_an_export_list() {
        // The guard lives on the local binding the specifier renames.
        let content = r#"
async function writeHandler({ request }: { request: Request }) {
  if (request.method !== "PUT") throw new Response(null, { status: 405 });
  return Response.json({});
}
export { writeHandler as action };
"#;
        assert_eq!(handler_guards(content, "action"), vec!["PUT"]);
    }

    #[test]
    fn no_method_guard_when_the_handler_does_not_compare_the_method() {
        let content = r#"
export async function action({ request }: { request: Request }) {
  const body = await request.json();
  if (body.kind !== "PUT") throw new Error("bad kind");
  return Response.json(body);
}
"#;
        assert!(handler_guards(content, "action").is_empty());
    }

    #[test]
    fn a_non_method_literal_is_not_a_guard() {
        // Only HTTP methods narrow a route; a comparison against anything else
        // leaves the handler unguarded.
        let content = r#"
export async function action({ request }: { request: Request }) {
  if (request.method !== "QUERY") throw new Response(null, { status: 405 });
  return Response.json({});
}
"#;
        assert!(handler_guards(content, "action").is_empty());
    }

    #[test]
    fn method_guards_are_per_export_not_per_module() {
        // A read export and a write export in one module must not borrow each
        // other's guard.
        let content = r#"
export async function loader({ request }: { request: Request }) {
  return Response.json({});
}
export async function action({ request }: { request: Request }) {
  if (request.method !== "PUT") throw new Response(null, { status: 405 });
  return Response.json({});
}
"#;
        assert!(handler_guards(content, "loader").is_empty());
        assert_eq!(handler_guards(content, "action"), vec!["PUT"]);
    }

    #[test]
    fn exported_handlers_finds_app_router_methods() {
        let content = r#"
export async function GET(req: Request) { return Response.json({}); }
export function POST() {}
const helper = 1;
function notExported() {}
"#;
        assert_eq!(handler_names(content), vec!["GET", "POST"]);
    }

    #[test]
    fn exported_handlers_finds_const_and_named_and_default() {
        let content = r#"
export const PUT = async () => {};
function handlePatch() {}
export { handlePatch as PATCH };
export default function handler() {}
"#;
        assert_eq!(handler_names(content), vec!["PATCH", "PUT", "default"]);
    }

    #[test]
    fn exported_handlers_empty_when_no_exports() {
        let content = "const x = 1; function f() {}";
        assert!(handler_names(content).is_empty());
    }

    #[test]
    fn detects_imported_client_wrapper_calls() {
        // `sdk`/`doThing` match none of the name heuristics; only the
        // import-based signal catches this, and only because detection flagged
        // `got` as a data fetcher (no hardcoded package list in the scanner).
        let content = r#"
import sdk from 'got';
async function run() { return sdk.doThing(); }
"#;
        let fetchers = vec!["got".to_string()];
        assert!(
            !scan_test_content_with_fetchers(content, &fetchers)
                .candidates
                .is_empty()
        );
        // Without detection flagging the package, the wrapper call is invisible
        // to the import signal (the other signals don't apply here either).
        assert!(scan_test_content(content).candidates.is_empty());
    }

    #[test]
    fn detects_url_scheme_first_arg() {
        let content = r#"function run() { return notanapi('https://api.example.com/users'); }"#;
        assert!(!scan_test_content(content).candidates.is_empty());
    }

    #[test]
    fn detects_new_network_primitives() {
        let content =
            r#"function run() { const ws = new WebSocket('wss://example.com'); return ws; }"#;
        assert!(!scan_test_content(content).candidates.is_empty());
    }

    #[test]
    fn detects_navigator_send_beacon_relative_url() {
        // `navigator.sendBeacon('/collect', payload)` is a web-platform HTTP
        // POST primitive. None of the name heuristics match `navigator` or
        // `sendBeacon`, and a relative `/collect` has no URL scheme, so only the
        // dedicated shape signal keeps this file from being skipped by the gate.
        let content = r#"function track() { const ok = navigator.sendBeacon('/collect', payload); return ok; }"#;
        let result = scan_test_content(content);
        let beacon = result.candidates.iter().find(|c| {
            c.callee_object == "navigator" && c.callee_property.as_deref() == Some("sendBeacon")
        });
        assert!(
            beacon.is_some(),
            "expected a navigator.sendBeacon candidate, got {:?}",
            result
                .candidates
                .iter()
                .map(|c| (&c.callee_object, &c.callee_property))
                .collect::<Vec<_>>()
        );
        let beacon = beacon.unwrap();
        assert_eq!(beacon.callee_property.as_deref(), Some("sendBeacon"));
        assert_eq!(beacon.protocol, Protocol::Http);
        assert_eq!(beacon.path_snippet.as_deref(), Some("'/collect'"));
    }

    #[test]
    fn detects_navigator_send_beacon_absolute_url() {
        let content = r#"function track() {
    navigator.sendBeacon('https://metrics.example.com/collect', JSON.stringify(data));
}"#;
        let result = scan_test_content(content);
        assert!(
            result
                .candidates
                .iter()
                .any(|c| c.callee_object == "navigator"
                    && c.callee_property.as_deref() == Some("sendBeacon")),
            "expected a navigator.sendBeacon candidate for an absolute URL"
        );
    }

    #[test]
    fn ignores_unrelated_send_beacon_member() {
        // A `sendBeacon` method on some other object is NOT the web-platform
        // primitive; the shape guard requires the `navigator` receiver.
        let content = r#"function f() { return tracker.sendBeacon('/x'); }"#;
        let result = scan_test_content(content);
        assert!(
            !result
                .candidates
                .iter()
                .any(|c| c.callee_object == "navigator"
                    && c.callee_property.as_deref() == Some("sendBeacon")),
            "non-navigator.sendBeacon must not be tagged as the navigator primitive"
        );
    }

    #[test]
    fn detects_window_parent_post_message() {
        // `window.parent.postMessage({action: 'x'}, '*')` is the web-platform
        // cross-context messaging send primitive. No name heuristic matches
        // `window.parent` or `postMessage`, the first arg is an object literal
        // (no URL scheme, not stringish), and the file has no messaging-client
        // import — only the Signal 8 shape surfaces it.
        let content = r#"function notify() {
    window.parent.postMessage({ action: 'document-completed', data: null }, '*');
}"#;
        let result = scan_test_content(content);
        assert!(
            result.candidates.iter().any(|c| {
                c.callee_object == "window" && c.callee_property.as_deref() == Some("postMessage")
            }),
            "expected a window.parent.postMessage candidate, got {:?}",
            result
                .candidates
                .iter()
                .map(|c| (&c.callee_object, &c.callee_property))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn detects_arbitrary_receiver_post_message() {
        // The receiver is arbitrary (iframe.contentWindow, worker, port) —
        // the property name is the signal; the LLM rejects topicless
        // transfers downstream.
        let content = r#"function send(worker) { worker.postMessage({ lines }); }"#;
        let result = scan_test_content(content);
        assert!(
            result
                .candidates
                .iter()
                .any(|c| c.callee_property.as_deref() == Some("postMessage")),
            "expected a postMessage candidate for a non-window receiver"
        );
    }

    #[test]
    fn detects_message_event_listener() {
        // `window.addEventListener('message', handler)` is the receiving side
        // of the postMessage channel. The literal 'message' first argument is
        // required — see ignores_non_message_event_listener.
        let content = r#"function mount(handler) { window.addEventListener('message', handler); }"#;
        let result = scan_test_content(content);
        assert!(
            result.candidates.iter().any(|c| {
                c.callee_object == "window"
                    && c.callee_property.as_deref() == Some("addEventListener")
            }),
            "expected a window.addEventListener('message') candidate"
        );
    }

    #[test]
    fn detects_bare_global_post_message_and_listener() {
        // Worker/global scope uses the implicit global: `postMessage(...)` and
        // `addEventListener('message', ...)` with no receiver at all.
        let content = r#"addEventListener('message', (event) => {
    postMessage({ type: 'result', data: event.data });
});"#;
        let result = scan_test_content(content);
        assert!(
            result
                .candidates
                .iter()
                .any(|c| c.callee_property.as_deref() == Some("addEventListener")
                    && c.callee_object == "<global>"),
            "expected a bare addEventListener('message') candidate"
        );
        assert!(
            result
                .candidates
                .iter()
                .any(|c| c.callee_property.as_deref() == Some("postMessage")
                    && c.callee_object == "<global>"),
            "expected a bare postMessage candidate"
        );
    }

    #[test]
    fn ignores_non_message_event_listener() {
        // addEventListener with any other event name is generic DOM wiring,
        // not the messaging channel.
        let content = r#"function mount(el, onClick) {
    el.addEventListener('click', onClick);
    document.addEventListener('keydown', onClick);
}"#;
        let result = scan_test_content(content);
        assert!(
            !result
                .candidates
                .iter()
                .any(|c| c.callee_property.as_deref() == Some("addEventListener")),
            "non-'message' addEventListener must not produce candidates"
        );
    }

    #[test]
    fn detects_awaited_stringish_call() {
        let content = r#"async function run() { return await loadData('/data.json'); }"#;
        assert!(!scan_test_content(content).candidates.is_empty());
    }

    #[test]
    fn ignores_non_network_code() {
        let content = r#"
function run() {
    console.log('hello');
    const x = compute(1, 2);
    return x;
}
"#;
        assert!(scan_test_content(content).candidates.is_empty());
    }

    #[test]
    fn dedupes_candidate_spans_across_signals() {
        // `await axios.get('https://x.com/y')` matches the import-local,
        // url-scheme, awaited-stringish, and name heuristics simultaneously,
        // but the single call site must yield exactly one candidate.
        let content = r#"
import axios from 'axios';
async function run() { return await axios.get('https://x.com/y'); }
"#;
        let result = scan_test_content(content);
        assert_eq!(result.candidates.len(), 1);
    }

    #[test]
    fn exported_handlers_reports_line_numbers() {
        let content = "\n\nexport function GET() {}\n";
        let scanner = SwcScanner::new();
        let handlers = scanner.exported_handlers(&PathBuf::from("route.ts"), content);
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].name, "GET");
        assert_eq!(handlers[0].line_number, 3);
    }

    #[test]
    fn detects_route_descriptor_object_literal() {
        // The gateway owner-fabrication trap (#227): a raw-handler block where
        // the route is declarative *data* in a registry array, not a method
        // call. No call-site signal fires, so without the object-literal signal
        // the whole file is skipped and `GET /gateway/health` is missed.
        let content = r#"
export const healthCheckHandler = async (_req: unknown, _res: unknown) => {
  return { ok: true, ts: Date.now() };
};

const routeRegistry = [
  { method: 'GET', path: '/gateway/health', handler: healthCheckHandler },
];

export { routeRegistry };
"#;
        let result = scan_test_content(content);
        let descriptor = result
            .candidates
            .iter()
            .find(|c| c.path_snippet.as_deref() == Some("'/gateway/health'"));
        assert!(
            descriptor.is_some(),
            "expected a route-descriptor candidate for the registry object, got {:?}",
            result
                .candidates
                .iter()
                .map(|c| (&c.callee_object, &c.path_snippet))
                .collect::<Vec<_>>()
        );
        let descriptor = descriptor.unwrap();
        assert_eq!(descriptor.protocol, Protocol::Http);
        // The candidate must be keyed on the real handler fn, never the HTTP
        // method string — the owner-fabrication bait.
        assert_eq!(descriptor.callee_object, "healthCheckHandler");
        assert_ne!(descriptor.callee_object, "GET");
    }

    #[test]
    fn route_descriptor_without_handler_still_flagged() {
        // `method` + `path` is enough for the gate to keep the file; a missing
        // or non-identifier handler falls back to a sentinel so the LLM still
        // sees and classifies the route.
        let content = r#"
const routes = [
  { method: 'POST', path: '/widgets' },
];
export { routes };
"#;
        let result = scan_test_content(content);
        let descriptor = result
            .candidates
            .iter()
            .find(|c| c.path_snippet.as_deref() == Some("'/widgets'"));
        assert!(
            descriptor.is_some(),
            "expected a route-descriptor candidate"
        );
        assert_eq!(descriptor.unwrap().callee_object, "<route-descriptor>");
    }

    #[test]
    fn plain_config_object_is_not_a_route_descriptor() {
        // An object with only one of the two required keys (or neither) is
        // ordinary config and must not be flagged, or the gate would light up
        // on every options bag in the codebase.
        let only_method = scan_test_content(r#"const a = { method: 'GET' };"#);
        let only_path = scan_test_content(r#"const b = { path: '/x' };"#);
        let neither = scan_test_content(r#"const c = { timeout: 5000, retries: 3 };"#);
        assert!(only_method.candidates.is_empty());
        assert!(only_path.candidates.is_empty());
        assert!(neither.candidates.is_empty());
    }

    #[test]
    fn route_descriptor_endpoints_extracts_method_path_handler() {
        // #234: the route declared as data carries the full method/path/handler
        // structurally, so it is emitted deterministically (no LLM). The owner is
        // the handler identifier `healthCheckHandler`, never the method literal
        // "GET" (the owner-fabrication trap).
        let content = r#"
export const healthCheckHandler = async (_req: unknown, _res: unknown) => {
  return { ok: true, ts: Date.now() };
};

const routeRegistry = [
  { method: 'GET', path: '/gateway/health', handler: healthCheckHandler },
];

export { routeRegistry };
"#;
        let scanner = SwcScanner::new();
        let endpoints =
            scanner.route_descriptor_endpoints(&PathBuf::from("health.handler.ts"), content);
        assert_eq!(endpoints.len(), 1, "expected one route descriptor endpoint");
        let ep = &endpoints[0];
        assert_eq!(ep.method, "GET");
        assert_eq!(ep.path, "/gateway/health");
        assert_eq!(ep.handler.as_deref(), Some("healthCheckHandler"));
        assert_ne!(ep.handler.as_deref(), Some("GET"));
        assert!(ep.span_end > ep.span_start);
    }

    #[test]
    fn route_descriptor_endpoints_skips_dynamic_method_or_path() {
        // A computed method/path can't be emitted deterministically — it stays on
        // the recall-boost candidate path, so no deterministic endpoint is built.
        let dynamic = r#"
const verb = 'GET';
const routes = [
  { method: verb, path: '/widgets', handler: listWidgets },
];
export { routes };
"#;
        let scanner = SwcScanner::new();
        let endpoints = scanner.route_descriptor_endpoints(&PathBuf::from("routes.ts"), dynamic);
        assert!(
            endpoints.is_empty(),
            "non-literal method must not yield a deterministic endpoint, got {endpoints:?}"
        );
    }

    #[test]
    fn route_descriptor_endpoints_allows_missing_handler() {
        // Literal method + path with no (or non-identifier) handler still emits a
        // deterministic endpoint; the owner is left unresolved for the caller.
        let content = r#"
const routes = [
  { method: 'POST', path: '/widgets' },
];
export { routes };
"#;
        let scanner = SwcScanner::new();
        let endpoints = scanner.route_descriptor_endpoints(&PathBuf::from("routes.ts"), content);
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].method, "POST");
        assert_eq!(endpoints[0].path, "/widgets");
        assert_eq!(endpoints[0].handler, None);
    }

    #[test]
    fn standalone_two_key_config_object_is_not_a_deterministic_endpoint() {
        // #241 (the real gap): a *standalone* config object that happens to carry
        // string-literal `method` + `path` keys — an axios-style request spec — is
        // NOT a route registry. It must not be emitted as a deterministic endpoint
        // (which would also suppress the LLM that classifies the file correctly).
        // The one-key case was already covered; this is the two-key misfire.
        let axios_config = r#"
const response = await client({
  method: 'GET',
  path: '/data',
  headers: { 'x-api-key': key },
});
"#;
        let scanner = SwcScanner::new();
        let endpoints =
            scanner.route_descriptor_endpoints(&PathBuf::from("client.ts"), axios_config);
        assert!(
            endpoints.is_empty(),
            "standalone {{ method, path, headers }} config must not be a route descriptor, got {endpoints:?}"
        );

        // The recall-boost candidate still fires (the object has the shape), so
        // `http_candidates` stays non-empty and the file is NOT suppressed: it
        // falls through to the LLM extraction path, which is the whole point.
        let result = scan_test_content(axios_config);
        assert!(
            !result.candidates.is_empty(),
            "the LLM fall-through candidate must survive so the file is not skipped"
        );
    }

    #[test]
    fn registry_descriptor_with_non_route_path_is_not_a_deterministic_endpoint() {
        // #241: even inside an array, a `path` that is a bare token (`some-message`)
        // — an RPC channel name, message key, etc. — is not route-shaped, so it must
        // not be fabricated as a `GET some-message` endpoint. It falls through.
        let content = r#"
const handlers = [
  { method: 'GET', path: 'some-message', handler: onMessage },
];
export { handlers };
"#;
        let scanner = SwcScanner::new();
        let endpoints = scanner.route_descriptor_endpoints(&PathBuf::from("handlers.ts"), content);
        assert!(
            endpoints.is_empty(),
            "a non-route path (bare token) must not yield a deterministic endpoint, got {endpoints:?}"
        );
    }

    #[test]
    fn registry_descriptor_with_url_path_is_not_a_producer_endpoint() {
        // #580 (revises #241): an http(s) URL names someone else's origin, so
        // it can be a CONSUMER target but never the path a producer serves its
        // route at. In a registry it is a webhook target being called out to, a
        // schema `$id`, or a redirect — not a route this service answers.
        let content = r#"
const routes = [
  { method: 'POST', path: 'https://api.example.com/webhook', handler: onHook },
];
export { routes };
"#;
        let scanner = SwcScanner::new();
        let endpoints = scanner.route_descriptor_endpoints(&PathBuf::from("routes.ts"), content);
        assert!(
            endpoints.is_empty(),
            "an absolute URL must not be emitted as a producer route path, got {endpoints:?}"
        );
    }

    #[test]
    fn producer_route_path_requires_a_leading_slash() {
        // #580: the three shapes the OAuth2-server scan mis-emitted as paths.
        assert!(!is_producer_route_path("GET"), "a method literal");
        assert!(!is_producer_route_path("text/csv"), "a content type");
        assert!(
            !is_producer_route_path("https://example.invalid/schemas/x.json"),
            "a schema $id URL"
        );
        assert!(!is_producer_route_path("http://example.invalid/x"));
        assert!(!is_producer_route_path(""));

        // Absolute paths, including a template literal whose static head is
        // absolute, are producer paths.
        assert!(is_producer_route_path("/"));
        assert!(is_producer_route_path("/users/:id"));
        assert!(is_producer_route_path("  /users  "), "surrounding space");
        assert!(is_producer_route_path("`/orders/${id}`"));
        assert!(
            !is_producer_route_path("`https://x.invalid/${id}`"),
            "a template whose static head is a URL is still not a producer path"
        );
    }

    #[test]
    fn consumer_request_spec_still_accepts_an_absolute_url() {
        // #580 guard: splitting the producer predicate must not narrow the
        // CONSUMER side, where a full URL is the ordinary outbound target.
        let content = r#"
const response = await client.post({
  url: 'https://api.example.com/v1/things',
  body: payload,
});
"#;
        let result = scan_test_content(content);
        let spec = result
            .candidates
            .iter()
            .find_map(|c| c.request_spec.as_ref())
            .expect("an absolute-URL request spec must still be captured");
        assert_eq!(spec.method, "POST");
        assert_eq!(spec.url, "https://api.example.com/v1/things");
    }

    #[test]
    fn controller_route_bindings_take_the_last_argument() {
        // #580 part b: the handler is the LAST argument, whatever middleware
        // sits in front of it. Taking the first identifier argument would
        // attribute `/token` to the error handler — and fail silently, because
        // a middleware module has no controller class to enumerate.
        let content = r#"
import { router } from './framework';
import errorHandler from './middleware/error-handler';
import token from './controllers/token';
import widget from './controllers/widget';

export default [
  router('/widget', widget),
  router('/token', errorHandler, token),
];
"#;
        let scanner = SwcScanner::new();
        let bindings = scanner.controller_route_bindings(&PathBuf::from("routes.ts"), content);
        let bound: Vec<(&str, &str, &str)> = bindings
            .iter()
            .map(|b| {
                (
                    b.path.as_str(),
                    b.binding.as_str(),
                    b.import_source.as_str(),
                )
            })
            .collect();
        assert_eq!(
            bound,
            vec![
                ("/widget", "widget", "./controllers/widget"),
                ("/token", "token", "./controllers/token"),
            ]
        );
    }

    #[test]
    fn controller_route_bindings_reject_non_route_shapes() {
        // Each call below fails exactly one leg of the gate. None is a route.
        let content = r#"
import { router } from './framework';
import widget from './controllers/widget';

const local = (path, handler) => handler;

export default [
  // path is not absolute
  router('widget', widget),
  // handler is not an imported binding
  router('/inline', (ctx) => ctx),
  // the binder is a local closure, not an imported callable
  local('/local', widget),
  // the binder is a method on an object
  suite.describe('/described', widget),
  // one argument only: nothing is bound
  router('/lonely'),
];
"#;
        let scanner = SwcScanner::new();
        assert_eq!(
            scanner.controller_route_bindings(&PathBuf::from("routes.ts"), content),
            Vec::new()
        );
    }

    #[test]
    fn default_export_controller_class_follows_every_class_shape() {
        // #580 part b: the three default-export shapes that reach a class
        // declared in the same module.
        let scanner = SwcScanner::new();
        let class_of = |content: &str| {
            scanner
                .default_export_controller_class(&PathBuf::from("controller.ts"), content)
                .map(|c| c.name)
        };

        assert_eq!(
            class_of("export default class Health { get(ctx) {} }"),
            Some("Health".to_string()),
            "the class itself"
        );
        assert_eq!(
            class_of("class Widget { get(ctx) {} }\nexport default new Widget();"),
            Some("Widget".to_string()),
            "an instance of a local class"
        );
        assert_eq!(
            class_of(
                "class Session { get(ctx) {} }\nconst session = new Session();\nexport default session;"
            ),
            Some("Session".to_string()),
            "an instance through a local binding"
        );
        assert_eq!(
            class_of("class Widget { get(ctx) {} }\nexport default Widget;"),
            Some("Widget".to_string()),
            "the class through a local binding"
        );

        // Everything else emits nothing: there is no method list to enumerate
        // and no class name to own the routes.
        assert_eq!(
            class_of(
                "const errorHandler = async (ctx, next) => next();\nexport default errorHandler;"
            ),
            None,
            "a middleware function"
        );
        assert_eq!(
            class_of("import Widget from './widget';\nexport default new Widget();"),
            None,
            "an instance of a class from another module"
        );
        assert_eq!(
            class_of("export default class { get(ctx) {} }"),
            None,
            "an anonymous class has no name to own a route"
        );
        assert_eq!(class_of("export const widget = 1;"), None, "no default");
    }

    #[test]
    fn controller_methods_are_verb_named_or_verb_decorated() {
        // #580 part b: `@method('GET')` declares the method of a handler that
        // is not verb-named; `@accept('text/csv')` declares a content type and
        // must contribute nothing; a plain helper is not a route at all.
        let content = r#"
import { Controller, accept, method } from '../framework';

class ReportController extends Controller {
  get(ctx) {}

  @method('GET')
  @accept('text/csv')
  exportCsv(ctx) {}

  buildRows() { return []; }

  @accept('text/csv')
  renderCsv(ctx) {}

  @roles('GET', 'admin')
  audit(ctx) {}

  constructor() { super(); }
}

export default new ReportController();
"#;
        let scanner = SwcScanner::new();
        let class = scanner
            .default_export_controller_class(&PathBuf::from("report.ts"), content)
            .expect("a default-exported controller class");
        assert_eq!(class.name, "ReportController");
        let methods: Vec<(&str, &str)> = class
            .methods
            .iter()
            .map(|m| (m.name.as_str(), m.http_method.as_str()))
            .collect();
        assert_eq!(
            methods,
            vec![("get", "GET"), ("exportCsv", "GET")],
            "only a verb-named method and a verb-decorated one are routes"
        );
        // The decorated method is reported at its own line, not at the first
        // decorator's, so the index points at the handler.
        assert_eq!(class.methods[1].line_number, 9);
    }

    /// #537: the config-object call form. The client is a bare binding (here a
    /// destructured parameter, the shape that makes every callee-name
    /// heuristic useless), no argument is a string, and the object spans
    /// several lines — so before this signal the call raised no candidate at
    /// all and its path was left to the model, which produced a wildcard.
    #[test]
    fn config_object_call_yields_a_candidate_carrying_method_and_url() {
        let content = r#"
export const login = async ({ clientId, apiClient }) => {
  const response = await apiClient({
    method: "post",
    url: "/api/v1/auth/universal-auth/login",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    data: loginData
  });
  return response.data.accessToken;
};
"#;
        let result = scan_test_content(content);
        let candidate = result
            .candidates
            .iter()
            .find(|c| c.callee_object == "apiClient")
            .unwrap_or_else(|| {
                panic!(
                    "config-object call must raise a candidate: {:#?}",
                    result.candidates
                )
            });

        let spec = candidate
            .request_spec
            .as_ref()
            .expect("the candidate must carry the structural request spec");
        assert_eq!(spec.method, "POST");
        assert_eq!(spec.url, "/api/v1/auth/universal-auth/login");
        // The hint must show the URL, not the object's opening brace.
        assert_eq!(
            candidate.path_snippet.as_deref(),
            Some("'/api/v1/auth/universal-auth/login'")
        );
    }

    /// The same shape written on one line, called on a member and on a plain
    /// module binding. Structural, so the callee spelling is irrelevant.
    #[test]
    fn config_object_call_is_recognised_whatever_the_callee_is() {
        let content = r#"
import axios from "axios";
export async function run() {
  await axios({ method: "GET", url: "/api/v1/status" });
  await client.request({ method: 'delete', url: '/api/v1/sessions/current' });
}
"#;
        let result = scan_test_content(content);
        let mut seen: Vec<(String, String)> = result
            .candidates
            .iter()
            .filter_map(|c| {
                c.request_spec
                    .as_ref()
                    .map(|s| (s.method.clone(), s.url.clone()))
            })
            .collect();
        seen.sort();
        assert_eq!(
            seen,
            vec![
                ("DELETE".to_string(), "/api/v1/sessions/current".to_string()),
                ("GET".to_string(), "/api/v1/status".to_string()),
            ]
        );
    }

    /// The three guards that keep this off ordinary options bags: a method
    /// that is not an HTTP verb, a URL that is not route-shaped, and a
    /// non-literal value for either.
    #[test]
    fn non_request_config_objects_carry_no_spec() {
        let cases = [
            // `method` names an RPC operation, not an HTTP verb.
            r#"await rpc({ method: "workspace.list", url: "/rpc" });"#,
            // `url` is a bare token, not a route.
            r#"await send({ method: "post", url: "queue-name" });"#,
            // Method is computed.
            r#"await client({ method: verb, url: "/api/v1/things" });"#,
            // URL is a template literal, so no unambiguous literal to anchor.
            r#"await client({ method: "post", url: `${base}/things` });"#,
            // No URL key at all.
            r#"await client({ method: "post", body: payload });"#,
        ];
        for case in cases {
            let content = format!("export async function run() {{ {case} }}");
            let result = scan_test_content(&content);
            assert!(
                result.candidates.iter().all(|c| c.request_spec.is_none()),
                "must carry no request spec: {case}"
            );
        }
    }

    /// A producer route declared the same way (`server.route({ method, url })`,
    /// the config-object route registration form) is recognised by the same
    /// structural reader, so its path literal anchors the candidate too —
    /// including the SPA-fallback wildcard, which must be read verbatim rather
    /// than guessed at.
    #[test]
    fn config_object_route_registration_anchors_its_path_literal() {
        let content = r#"
export function register(server) {
  server.route({
    method: "GET",
    url: "/*",
    handler: (request, reply) => reply.send(indexHtml)
  });
}
"#;
        let result = scan_test_content(content);
        let candidate = result
            .candidates
            .iter()
            .find(|c| c.request_spec.is_some())
            .unwrap_or_else(|| {
                panic!(
                    "route registration must raise a candidate: {:#?}",
                    result.candidates
                )
            });
        let spec = candidate.request_spec.as_ref().expect("checked above");
        assert_eq!((spec.method.as_str(), spec.url.as_str()), ("GET", "/*"));
        assert_eq!(candidate.path_snippet.as_deref(), Some("'/*'"));
    }

    /// #529: a generated OpenAPI client issues every operation as a verb-named
    /// method handed one object that carries the URL, on a receiver that is an
    /// expression rather than a name. No argument is a string and the callee
    /// heuristics have nothing to read, so before this the call raised no
    /// candidate carrying its path and the operation never reached the index.
    #[test]
    fn verb_named_object_call_yields_a_candidate_carrying_method_and_url() {
        let content = r#"
export const releaseSession = <ThrowOnError extends boolean = false>(options?: Options) => {
  return (options?.client ?? client).post<ReleaseResponse, ReleaseError, ThrowOnError>({
    ...options,
    url: "/v1/sessions/{sessionId}/release",
  });
};
"#;
        let result = scan_test_content(content);
        let candidate = result
            .candidates
            .iter()
            .find(|c| c.request_spec.is_some())
            .unwrap_or_else(|| {
                panic!(
                    "verb-named object call must raise a candidate: {:#?}",
                    result.candidates
                )
            });

        let spec = candidate.request_spec.as_ref().expect("checked above");
        assert_eq!(spec.method, "POST");
        // OpenAPI `{param}` placeholders arrive in the router spelling so the
        // call joins the route the producer declares.
        assert_eq!(spec.url, "/v1/sessions/:sessionId/release");
        assert!(spec.method_from_callee);
        // The hint must show the URL, not the object's opening brace.
        assert_eq!(
            candidate.path_snippet.as_deref(),
            Some("'/v1/sessions/:sessionId/release'")
        );
    }

    /// The same shape on a plainly named receiver, with and without the
    /// leading spread. Structural, so the callee spelling is irrelevant.
    #[test]
    fn verb_named_object_call_is_recognised_whatever_the_callee_is() {
        let content = r#"
export async function run() {
  await client.get({ ...options, url: "/v1/sessions" });
  await sdk.http.delete({ url: "/v1/sessions/{sessionId}" });
}
"#;
        let result = scan_test_content(content);
        let mut seen: Vec<(String, String)> = result
            .candidates
            .iter()
            .filter_map(|c| {
                c.request_spec
                    .as_ref()
                    .map(|s| (s.method.clone(), s.url.clone()))
            })
            .collect();
        seen.sort();
        assert_eq!(
            seen,
            vec![
                ("DELETE".to_string(), "/v1/sessions/:sessionId".to_string()),
                ("GET".to_string(), "/v1/sessions".to_string()),
            ]
        );
    }

    /// The guards that keep the verb-named form off route registrations and
    /// ordinary options bags.
    #[test]
    fn non_request_verb_named_object_calls_carry_no_spec() {
        let cases = [
            // `path`, not `url`: the declarative route-registration spelling.
            r#"await router.get({ path: "/v1/things", handler: onThings });"#,
            // Carries a handler, so it registers a route rather than issuing one.
            r#"await api.get({ url: "/v1/things", handler: onThings });"#,
            r#"await api.get({ url: "/v1/things", onError: (e) => log(e) });"#,
            // The member is not an HTTP verb.
            r#"await queue.send({ url: "/v1/things" });"#,
            // `url` is a bare token, not a route.
            r#"await client.post({ url: "queue-name" });"#,
            // Shorthand property: no literal to read.
            r#"await client.post({ url });"#,
            // Template literal, so no unambiguous literal to anchor.
            r#"await client.post({ url: `${base}/things` });"#,
        ];
        for case in cases {
            let content = format!("export async function run() {{ {case} }}");
            let result = scan_test_content(&content);
            assert!(
                result.candidates.iter().all(|c| c.request_spec.is_none()),
                "must carry no request spec: {case}"
            );
        }
    }

    #[test]
    fn normalize_path_params_rewrites_whole_segments_only() {
        assert_eq!(
            normalize_path_params("/v1/sessions/{sessionId}/release"),
            "/v1/sessions/:sessionId/release"
        );
        // Already in the router spelling, and idempotent.
        assert_eq!(
            normalize_path_params("/v1/sessions/:id"),
            "/v1/sessions/:id"
        );
        // An interpolated base is not a path param.
        assert_eq!(
            normalize_path_params("${API_URL}/v1/sessions/{id}"),
            "${API_URL}/v1/sessions/:id"
        );
        // Partial-segment braces are left alone.
        assert_eq!(normalize_path_params("/v1/a{b}c"), "/v1/a{b}c");
        assert_eq!(normalize_path_params("/v1/{}"), "/v1/{}");
    }

    #[test]
    fn test_detects_express_style_endpoints() {
        let content = r#"
import express from 'express';
const app = express();

app.get('/users', getUsers);
app.post('/users', createUser);
router.delete('/users/:id', deleteUser);
"#;

        let result = scan_test_content(content);
        assert!(!result.candidates.is_empty());
        assert!(result.candidates.len() >= 3);

        // Should detect app.get, app.post, router.delete
        let methods: Vec<_> = result
            .candidates
            .iter()
            .filter_map(|c| c.callee_property.as_ref())
            .collect();
        assert!(methods.contains(&&"get".to_string()));
        assert!(methods.contains(&&"post".to_string()));
        assert!(methods.contains(&&"delete".to_string()));
    }

    #[test]
    fn test_detects_fetch_calls() {
        let content = r#"
async function getData() {
    const response = await fetch('/api/data');
    const data = await response.json();
    return data;
}
"#;

        let result = scan_test_content(content);
        assert!(!result.candidates.is_empty());

        // Should detect fetch and response.json
        let has_fetch = result.candidates.iter().any(|c| c.callee_object == "fetch");
        let has_json = result.candidates.iter().any(|c| {
            c.callee_property
                .as_ref()
                .map(|p| p == "json")
                .unwrap_or(false)
        });

        assert!(has_fetch, "Should detect global fetch call");
        assert!(has_json, "Should detect response.json() call");
    }

    #[test]
    fn test_candidate_spans_and_ids() {
        let content = "fetch('/api/users');";
        let result = scan_test_content(content);
        assert!(!result.candidates.is_empty());
        assert!(!result.candidates.is_empty());

        let candidate = &result.candidates[0];
        assert!(candidate.span_start < candidate.span_end);
        assert_eq!(
            candidate.candidate_id,
            format!("span:{}-{}", candidate.span_start, candidate.span_end)
        );
    }

    #[test]
    fn test_detects_router_mounts() {
        let content = r#"
import userRouter from './routes/users';
import authRouter from './routes/auth';

app.use('/api/users', userRouter);
app.use('/api/auth', authRouter);
router.use('/v1', v1Router);
"#;

        let result = scan_test_content(content);
        assert!(!result.candidates.is_empty());

        // Should detect all .use() calls
        let use_calls: Vec<_> = result
            .candidates
            .iter()
            .filter(|c| {
                c.callee_property
                    .as_ref()
                    .map(|p| p == "use")
                    .unwrap_or(false)
            })
            .collect();

        assert!(use_calls.len() >= 3, "Should detect all router mounts");
    }

    #[test]
    fn test_skips_irrelevant_files() {
        let content = r#"
// A utility file with no API patterns
export function formatDate(date: Date): string {
    return date.toISOString();
}

export function calculateSum(numbers: number[]): number {
    return numbers.reduce((a, b) => a + b, 0);
}

const arr = [1, 2, 3];
arr.map(x => x * 2);
arr.filter(x => x > 1);
console.log('test');
"#;

        let result = scan_test_content(content);
        // This should have few or no candidates (map, filter, reduce, log are not API patterns)
        assert!(
            result.candidates.len() <= 1,
            "Utility files should have minimal candidates"
        );
    }

    #[test]
    fn test_detects_axios_calls() {
        let content = r#"
import axios from 'axios';

async function fetchUser(id: string) {
    const response = await axios.get(`/users/${id}`);
    return response.data;
}
"#;

        let result = scan_test_content(content);
        assert!(!result.candidates.is_empty());

        let has_axios = result.candidates.iter().any(|c| c.callee_object == "axios");
        assert!(has_axios, "Should detect axios calls");
    }

    #[test]
    fn test_candidate_format_hint() {
        let candidate = CandidateTarget {
            protocol: Protocol::Http,
            candidate_id: "span:100-140".to_string(),
            span_start: 100,
            span_end: 140,
            line_number: 15,
            callee_object: "app".to_string(),
            callee_property: Some("get".to_string()),
            enclosing_function: Some("handler".to_string()),
            path_snippet: Some("'/users'".to_string()),
            code_snippet: "app.get('/users', handler)".to_string(),
            request_spec: None,
            request_shape: RequestShapeSignal::NotARequest,
        };

        let hint = candidate.format_hint();
        assert!(hint.contains("Line 15"));
        assert!(hint.contains("span:100-140"));
        assert!(hint.contains("app.get"));
        assert!(hint.contains("handler"));
        assert!(hint.contains("[path: '/users']"));
        assert!(hint.contains("app.get('/users', handler)"));
    }

    #[test]
    fn test_detects_chained_calls() {
        let content = r#"
createRouter()
    .get('/health', healthCheck)
    .post('/data', handleData);
"#;

        let result = scan_test_content(content);
        assert!(!result.candidates.is_empty());

        // Should detect the HTTP methods even in chained form
        let methods: Vec<_> = result
            .candidates
            .iter()
            .filter_map(|c| c.callee_property.as_ref())
            .collect();
        assert!(methods.contains(&&"get".to_string()));
        assert!(methods.contains(&&"post".to_string()));
    }

    #[test]
    fn test_scan_content_per_file_offsets_no_accumulation() {
        // Regression test: verify that scanning multiple files with the same SwcScanner
        // produces per-file byte offsets (not cumulative offsets).
        let scanner = SwcScanner::new();

        let file_a_content = "fetch('/api/a');";
        let file_b_content = "fetch('/api/b');";

        let result_a = scanner.scan_content(&PathBuf::from("a.ts"), file_a_content, &[], &[]);
        let result_b = scanner.scan_content(&PathBuf::from("b.ts"), file_b_content, &[], &[]);

        assert!(
            !result_a.candidates.is_empty(),
            "file a should have candidates"
        );
        assert!(
            !result_b.candidates.is_empty(),
            "file b should have candidates"
        );

        let span_a = (
            result_a.candidates[0].span_start,
            result_a.candidates[0].span_end,
        );
        let span_b = (
            result_b.candidates[0].span_start,
            result_b.candidates[0].span_end,
        );

        // Both files have the same content structure, so spans should be identical
        // (both start at offset 0-based within their own file).
        assert_eq!(
            span_a, span_b,
            "Spans should be identical for identically-structured files (per-file offsets). \
             Got a={:?}, b={:?}. If b is offset, SourceMap accumulation bug is present.",
            span_a, span_b
        );

        // Spans should be within the file size
        assert!(
            (span_b.1 as usize) <= file_b_content.len() + 1,
            "span_end {} should not exceed file size {}",
            span_b.1,
            file_b_content.len()
        );
    }

    #[test]
    fn test_detects_decorator_calls_for_nestjs_style() {
        // Regression for the gap verified in the carrick-cloud repo's docs/internal/framework-coverage.md §2.3:
        // prior to Move 2, decorator calls produced zero candidates because the
        // visitor only fired on member calls. After widening the scanner, a
        // @Controller('users') class with @Get/@Post/@Get(':id') methods must
        // produce non-zero candidates — the LLM decides which are routing
        // decorators via the Import Table.
        let content = r#"
import { Controller, Get, Post } from '@nestjs/common';

@Controller('users')
export class UsersController {
  @Get()
  findAll() { return []; }

  @Get(':id')
  findOne() { return null; }

  @Post()
  create() { return { id: 1 }; }
}
"#;

        let result = scan_test_content(content);
        assert!(
            !result.candidates.is_empty(),
            "NestJS controller should analyze"
        );

        // At least four decorator candidates (one Controller + three method decorators).
        let decorator_candidates: Vec<_> = result
            .candidates
            .iter()
            .filter(|c| {
                matches!(
                    c.callee_object.as_str(),
                    "Controller" | "Get" | "Post" | "Put" | "Patch" | "Delete"
                )
            })
            .collect();
        assert!(
            decorator_candidates.len() >= 4,
            "expected >=4 decorator candidates, got {}: {:?}",
            decorator_candidates.len(),
            decorator_candidates
                .iter()
                .map(|c| &c.callee_object)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_detects_custom_router_names() {
        let content = r#"
const userRouter = createRouter();
const authRouter = createRouter();
const apiHandler = createHandler();

userRouter.get('/profile', getProfile);
authRouter.post('/login', login);
apiHandler.route('/data', handleData);
"#;

        let result = scan_test_content(content);
        assert!(!result.candidates.is_empty());

        // Should detect calls on userRouter, authRouter, apiHandler
        assert!(
            result.candidates.len() >= 3,
            "Should detect custom-named router calls"
        );
    }

    /// Gating proof for the pub/sub call-site Signal 7. The critical property is
    /// that the publish/subscribe shape is surfaced ONLY in files that import a
    /// messaging-client package, and that the shape-identical `socket.emit(...)`
    /// is never surfaced (socket.io is not a messaging client), so the gate has
    /// zero socket-skip / corpus-1 collateral.
    #[test]
    fn pubsub_candidate_surfacing_is_gated_by_messaging_client_import() {
        use std::fs;

        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");

        // --- Real NATS publisher fixture: `nc.publish(SUBJECT, ...)` with a
        //     const-string topic (`const SUBJECT = "user.registered"`). ---
        let publisher_path = fixtures.join("xrepo-corpus-2/analytics-worker/src/nats/publisher.ts");
        let publisher_src = fs::read_to_string(&publisher_path)
            .unwrap_or_else(|e| panic!("read {}: {}", publisher_path.display(), e));
        let publisher_scanner = SwcScanner::new();

        // Gated in (messaging_clients=["nats"]): the publish call is surfaced,
        // proving const-string topic resolution works (`SUBJECT` -> literal).
        let gated_in = publisher_scanner.scan_content(
            &publisher_path,
            &publisher_src,
            &[],
            &["nats".to_string()],
        );
        assert!(
            gated_in
                .candidates
                .iter()
                .any(|c| c.callee_property.as_deref() == Some("publish")),
            "gated in (messaging_clients=[nats]): nc.publish(SUBJECT,…) must be surfaced, got {:?}",
            gated_in.candidates
        );

        // Gated out (messaging_clients=[]): the file is inert — zero candidates.
        let gated_out = publisher_scanner.scan_content(&publisher_path, &publisher_src, &[], &[]);
        assert!(
            gated_out.candidates.is_empty(),
            "gated out (messaging_clients=[]): publisher file must surface 0 candidates, got {:?}",
            gated_out.candidates
        );

        // --- Real NATS subscriber fixture: `const sub = nc.subscribe("user.registered")`
        //     (variable initializer position, inline string literal). ---
        let subscriber_path =
            fixtures.join("xrepo-corpus-2/notifications-svc/src/nats/subscriber.ts");
        let subscriber_src = fs::read_to_string(&subscriber_path)
            .unwrap_or_else(|e| panic!("read {}: {}", subscriber_path.display(), e));
        let subscriber_scanner = SwcScanner::new();

        let sub_gated_in = subscriber_scanner.scan_content(
            &subscriber_path,
            &subscriber_src,
            &[],
            &["nats".to_string()],
        );
        assert!(
            sub_gated_in
                .candidates
                .iter()
                .any(|c| c.callee_property.as_deref() == Some("subscribe")),
            "gated in: const sub = nc.subscribe(\"…\") must be surfaced, got {:?}",
            sub_gated_in.candidates
        );

        let sub_gated_out =
            subscriber_scanner.scan_content(&subscriber_path, &subscriber_src, &[], &[]);
        assert!(
            sub_gated_out.candidates.is_empty(),
            "gated out: subscriber file must surface 0 candidates, got {:?}",
            sub_gated_out.candidates
        );

        // --- Socket gate exclusion: socket.io is NOT a messaging client, so even
        //     with messaging_clients=["nats"] set, `socket.emit('x', d)` (the
        //     shape-identical publish look-alike) must NOT be surfaced by
        //     Signal 7. This is the socket-skip invariant the ungated version
        //     broke. ---
        let socket_src = r#"
import { Server } from 'socket.io';
const io = new Server();
io.on('connection', (socket) => {
  socket.emit('payment:settled', { id: 1 });
});
"#;
        let socket_path = PathBuf::from("realtime.ts");
        let socket_scanner = SwcScanner::new();
        let socket_scan =
            socket_scanner.scan_content(&socket_path, socket_src, &[], &["nats".to_string()]);
        assert!(
            !socket_scan
                .candidates
                .iter()
                .any(|c| c.callee_property.as_deref() == Some("emit")),
            "socket.emit must NOT be surfaced — socket.io is not a messaging client, so the gate \
             excludes socket files even when messaging_clients=[nats], got {:?}",
            socket_scan.candidates
        );
    }

    /// carrick#317: a file that receives its messaging client by constructor
    /// injection or inheritance (`this.messenger` from a base class) has no
    /// gating import. Tier 2 of the Signal 7 gate surfaces its call sites by
    /// SHAPE — member calls literally named publish/subscribe with a stringish
    /// topic — while logger/socket/emit shapes stay inert.
    #[test]
    fn pubsub_shape_gate_surfaces_injected_messenger_without_import() {
        let src = r#"
import type { Messenger } from './types';

declare const logger: { info: (m: string) => void };
declare const socket: { emit: (e: string, p: unknown) => void };

export class NetworkMonitor {
  constructor(private readonly messenger: Messenger) {}

  notifyDegraded(url: string): void {
    this.messenger.publish('NetworkController:rpcEndpointDegraded', { url });
  }

  watch(handler: (payload: unknown) => void): void {
    this.messenger.subscribe('NetworkController:networkDidChange', handler);
  }

  log(): void {
    logger.info('a plain log line');
    socket.emit('user:connected', { id: 1 });
  }
}
"#;
        let scanner = SwcScanner::new();

        // Repo-level clients detected; this file imports none of them.
        let result = scanner.scan_content(
            &PathBuf::from("network-monitor.ts"),
            src,
            &[],
            &["@metamask/messenger".to_string()],
        );
        let props: Vec<&str> = result
            .candidates
            .iter()
            .filter_map(|c| c.callee_property.as_deref())
            .collect();
        assert!(
            props.contains(&"publish"),
            "shape gate must surface the injected messenger publish, got {props:?}"
        );
        assert!(
            props.contains(&"subscribe"),
            "shape gate must surface the injected messenger subscribe, got {props:?}"
        );
        assert!(
            !props.contains(&"info") && !props.contains(&"emit"),
            "logger.info / socket.emit must stay inert under the shape gate, got {props:?}"
        );

        // Empty messaging_clients: both tiers off, file fully inert.
        let inert = scanner.scan_content(&PathBuf::from("network-monitor.ts"), src, &[], &[]);
        assert!(
            inert.candidates.is_empty(),
            "with no detected messaging clients the file must surface 0 candidates, got {:?}",
            inert.candidates
        );
    }

    /// carrick#387: a payload-less publish/subscribe with a deterministically
    /// resolvable topic is a structural fact — emit it as an anchor op so an
    /// LLM extraction omission cannot lose the operation. The template-literal
    /// case (`` this.messenger.publish(`${name}:started`) `` with
    /// `export const name = '...'`) is the measured 4/20 recall gap.
    #[test]
    fn pubsub_anchor_ops_resolve_template_and_literal_topics() {
        use crate::operation::PubsubRole;

        let src = r#"
export const name = 'PollController';
const CHANNEL = 'jobs.retry';
export class PollController {
  start() {
    this.messenger.publish(`${name}:pollingStarted`);
  }
  stop() {
    this.messenger.publish('PollController:stopped');
  }
  listen() {
    const sub = this.messenger.subscribe(CHANNEL);
  }
}
"#;
        let scanner = SwcScanner::new();
        let result = scanner.scan_content(
            &PathBuf::from("poll.ts"),
            src,
            &[],
            &["fakebus".to_string()],
        );
        let ops = &result.pubsub_anchor_ops;
        assert_eq!(ops.len(), 3, "expected 3 anchor ops, got {ops:?}");
        assert!(
            ops.iter()
                .any(|op| op.topic == "PollController:pollingStarted"
                    && op.role == PubsubRole::Publisher
                    && op.line_number == 6),
            "template topic with exported same-file const must resolve, got {ops:?}"
        );
        assert!(
            ops.iter().any(|op| op.topic == "PollController:stopped"
                && op.role == PubsubRole::Publisher
                && op.line_number == 9),
            "inline string-literal topic must resolve, got {ops:?}"
        );
        assert!(
            ops.iter().any(|op| op.topic == "jobs.retry"
                && op.role == PubsubRole::Subscriber
                && op.line_number == 12),
            "const-ref topic in initializer position must resolve, got {ops:?}"
        );
    }

    /// The anchor path only asserts what is structurally certain: gated off with
    /// no detected messaging clients; never for payload-carrying calls (those
    /// stay LLM-owned so the type-capture path is undisturbed); never for
    /// unresolvable or non-publish/subscribe-named calls; never in nested
    /// (non-statement/initializer) positions.
    #[test]
    fn pubsub_anchor_ops_stay_inert_outside_the_guarded_shape() {
        let src = r#"
export const name = 'PollController';
import { bus } from 'fakebus';
function localTopic(): string { return 'x'; }
export function run(payload: object, dynamic: string) {
  bus.publish('with.payload', payload);
  bus.publish(`${dynamic}:started`);
  bus.emit('not.protocol.vocab');
  wrap(bus.publish('nested.position'));
  bus.publish(localTopic());
}
"#;
        let scanner = SwcScanner::new();
        let gated_in = scanner.scan_content(
            &PathBuf::from("guarded.ts"),
            src,
            &[],
            &["fakebus".to_string()],
        );
        assert!(
            gated_in.pubsub_anchor_ops.is_empty(),
            "payload-carrying / unresolvable / nested / non-vocab calls must not anchor, got {:?}",
            gated_in.pubsub_anchor_ops
        );

        // Gate off entirely: no messaging clients detected.
        let no_payload_src = r#"
import { bus } from 'fakebus';
bus.publish('plain.topic');
"#;
        let gated_out =
            scanner.scan_content(&PathBuf::from("guarded.ts"), no_payload_src, &[], &[]);
        assert!(
            gated_out.pubsub_anchor_ops.is_empty(),
            "empty messaging_clients must keep the anchor path inert, got {:?}",
            gated_out.pubsub_anchor_ops
        );
    }

    /// carrick#402 shape a, on the exact corpus-2 file that flaked (kafkajs
    /// `await consumer.subscribe({ topic: TOPIC, fromBeginning: false })` with
    /// a const-ref topic): the site must surface as a candidate AND emit a
    /// payload-less subscriber anchor op, so an LLM extraction miss is
    /// backfilled deterministically. With no detected messaging clients the
    /// file stays fully inert.
    #[test]
    fn kafkajs_object_literal_subscribe_is_candidate_and_anchor() {
        use crate::operation::PubsubRole;
        use std::fs;

        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/xrepo-corpus-2/notifications-svc/src/kafka/consumer.ts");
        let src =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
        let scanner = SwcScanner::new();

        let gated_in = scanner.scan_content(&path, &src, &[], &["kafkajs".to_string()]);
        assert!(
            gated_in
                .candidates
                .iter()
                .any(|c| c.callee_property.as_deref() == Some("subscribe")),
            "awaited consumer.subscribe({{ topic }}) must surface as a candidate, got {:?}",
            gated_in.candidates
        );
        let ops = &gated_in.pubsub_anchor_ops;
        assert_eq!(
            ops.len(),
            1,
            "expected exactly the subscribe anchor, got {ops:?}"
        );
        let op = &ops[0];
        assert_eq!(op.topic, "order.placed");
        assert_eq!(op.role, PubsubRole::Subscriber);
        assert_eq!(
            (op.handler_param.as_deref(), op.handler_param_line),
            (None, None),
            "options-object subscribe registers its handler elsewhere; the anchor must be payload-less"
        );

        let gated_out = scanner.scan_content(&path, &src, &[], &[]);
        assert!(
            gated_out.pubsub_anchor_ops.is_empty(),
            "empty messaging_clients must keep the kafkajs file anchor-inert, got {:?}",
            gated_out.pubsub_anchor_ops
        );
    }

    /// carrick#402 shape a: `subscribe({ topics: ['a','b'] })` anchors every
    /// resolvable topic; a vocabulary key on a NON-vocabulary method
    /// (`configure({ topic })`), the handler-registration object
    /// (`run({ eachMessage })`), and a `publish({ topic, ... })` options
    /// object (whose sibling property may be the payload — Copilot review on
    /// #409) stay anchor-inert. The publish object shape still surfaces as a
    /// candidate so the LLM owns the site.
    #[test]
    fn object_literal_topics_array_anchors_each_topic() {
        use crate::operation::PubsubRole;

        let src = r#"
import { Kafka } from 'fakekafka';
const consumer = new Kafka({ brokers: [] }).consumer({ groupId: 'g' });
declare const bus: { publish: (opts: object) => void };
export async function start(): Promise<void> {
  await consumer.subscribe({ topics: ['order.created', 'order.cancelled'], fromBeginning: true });
  await consumer.configure({ topic: 'not.a.subscription' });
  await consumer.run({ eachMessage: async () => {} });
  bus.publish({ topic: 'order.audited', payload: { id: 1 } });
}
"#;
        let scanner = SwcScanner::new();
        let result = scanner.scan_content(
            &PathBuf::from("consumer.ts"),
            src,
            &[],
            &["fakekafka".to_string()],
        );
        let ops = &result.pubsub_anchor_ops;
        assert_eq!(
            ops.len(),
            2,
            "expected one anchor per topics[] entry and nothing else, got {ops:?}"
        );
        for topic in ["order.created", "order.cancelled"] {
            assert!(
                ops.iter()
                    .any(|op| op.topic == topic && op.role == PubsubRole::Subscriber),
                "topics[] entry {topic} must anchor, got {ops:?}"
            );
        }
        assert!(
            result
                .candidates
                .iter()
                .any(|c| c.callee_property.as_deref() == Some("publish")),
            "publish({{ topic }}) must still surface as a candidate, got {:?}",
            result.candidates
        );
    }

    /// carrick#402 shape b, on the exact corpus-3 file that flaked (BullMQ
    /// `export const dispatchWorker = new Worker("shipments.dispatch", async
    /// (job) => {...})`): the constructor site must surface as a candidate AND
    /// emit a payload-less subscriber anchor, gated on `Worker` being an
    /// import binding from a detected messaging-client package.
    #[test]
    fn bullmq_new_worker_is_candidate_and_anchor() {
        use crate::operation::PubsubRole;
        use std::fs;

        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/xrepo-corpus-3/fulfillment-worker/src/worker.ts");
        let src =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
        let scanner = SwcScanner::new();

        let gated_in = scanner.scan_content(&path, &src, &[], &["bullmq".to_string()]);
        assert!(
            gated_in
                .candidates
                .iter()
                .any(|c| c.callee_object == "Worker"),
            "new Worker(queue, handler) must surface as a candidate, got {:?}",
            gated_in.candidates
        );
        assert!(
            gated_in.pubsub_anchor_ops.iter().any(|op| {
                op.topic == "shipments.dispatch"
                    && op.role == PubsubRole::Subscriber
                    && op.handler_param.is_none()
            }),
            "new Worker must emit a payload-less subscriber anchor (the handler \
             param is a Job envelope, never a payload locator), got {:?}",
            gated_in.pubsub_anchor_ops
        );

        let gated_out = scanner.scan_content(&path, &src, &[], &[]);
        assert!(
            gated_out.pubsub_anchor_ops.is_empty(),
            "empty messaging_clients must keep the worker file anchor-inert, got {:?}",
            gated_out.pubsub_anchor_ops
        );
    }

    /// carrick#402 shape b negative: the NewExpr gate is IMPORT-BINDING
    /// resolution, not method shape and not any file-level import. A
    /// `new CronJob("literal", fn)` whose binding comes from a package the
    /// detect step did NOT flag as a messaging client must not anchor — even
    /// when the repo has detected messaging clients, and even when the same
    /// file also imports one.
    #[test]
    fn new_expr_gate_rejects_non_messaging_constructors() {
        let src = r#"
import { Worker } from 'bullmq';
import { CronJob } from 'cron';
export const job = new CronJob('0 * * * *', () => {});
export const unused = Worker;
"#;
        let scanner = SwcScanner::new();
        let result = scanner.scan_content(
            &PathBuf::from("scheduler.ts"),
            src,
            &[],
            &["bullmq".to_string()],
        );
        assert!(
            result.pubsub_anchor_ops.is_empty(),
            "new CronJob('lit', fn) must NOT anchor: cron is not a detected \
             messaging client, got {:?}",
            result.pubsub_anchor_ops
        );
        assert!(
            !result
                .candidates
                .iter()
                .any(|c| c.callee_object == "CronJob"),
            "CronJob must not surface as a pub/sub candidate, got {:?}",
            result.candidates
        );
    }

    /// carrick#402 shape c: two-arg `subscribe("topic", handler)` with an
    /// INLINE function anchors and records the handler's first param (simple
    /// identifier only) as the FunctionParam payload locator; a destructured
    /// param anchors payload-less; a function REFERENCE second arg (could be
    /// an options object) does not anchor at all.
    #[test]
    fn two_arg_subscribe_records_inline_handler_param() {
        use crate::operation::PubsubRole;

        let src = r#"
import { bus } from 'fakebus';
declare function handlerRef(msg: unknown): void;
bus.subscribe('user.created', (msg) => { console.log(msg); });
bus.subscribe('user.updated', function (payload: { id: string }) { void payload; });
bus.subscribe('user.enriched', ({ data }) => { void data; });
bus.subscribe('user.deleted', handlerRef);
"#;
        let scanner = SwcScanner::new();
        let result = scanner.scan_content(
            &PathBuf::from("subscriber.ts"),
            src,
            &[],
            &["fakebus".to_string()],
        );
        let ops = &result.pubsub_anchor_ops;
        assert_eq!(
            ops.len(),
            3,
            "expected the three inline-handler anchors, got {ops:?}"
        );
        assert!(
            ops.iter().any(|op| op.topic == "user.created"
                && op.role == PubsubRole::Subscriber
                && op.handler_param.as_deref() == Some("msg")
                && op.handler_param_line == Some(4)),
            "arrow handler's ident param must be recorded, got {ops:?}"
        );
        assert!(
            ops.iter()
                .any(|op| op.topic == "user.updated"
                    && op.handler_param.as_deref() == Some("payload")),
            "function-expression handler's typed ident param must be recorded, got {ops:?}"
        );
        assert!(
            ops.iter()
                .any(|op| op.topic == "user.enriched" && op.handler_param.is_none()),
            "destructured handler param must anchor payload-less, got {ops:?}"
        );
        assert!(
            !ops.iter().any(|op| op.topic == "user.deleted"),
            "a function-reference second arg must not anchor, got {ops:?}"
        );
    }
}
