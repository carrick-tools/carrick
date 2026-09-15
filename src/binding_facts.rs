//! What a file's call sites reach through an imported binding, stated as
//! facts for the analyzer (carrick#1146, carrick#1155).
//!
//! A frontend usually talks to its backend through a client object another
//! module declares, one member per endpoint:
//!
//! ```ignore
//! // lib/orders.ts
//! export const ordersApi = {
//!   get: (id: string) => sendJson("GET", `/orders/${id}`),
//! };
//! // page.tsx
//! const order = await ordersApi.get(id);
//! ```
//!
//! The site states neither the path nor the method; the member's body states
//! both, through a helper. Resolving that is following a value through
//! authored code, which is the model's job, but only if the model can see the
//! body. Before this module the analyzer was handed the first 4 KB of every
//! imported wrapper module, reached by a relative specifier only. A client
//! module is routinely several times that, so the member bodies sat past the
//! cut, and a module imported through a tsconfig alias was not offered at all.
//! The model was then asked to resolve through source it never saw, and it
//! wrote paths that exist nowhere.
//!
//! So instead of a prefix, the analyzer gets exactly what the file's own calls
//! reach, read off the AST:
//!
//! - the declaration each call site names: an exported function, a member of
//!   an exported object literal, a method of an exported class or of the class
//!   an exported instance is constructed from;
//! - one hop of helpers: the functions that declaration calls, in its own
//!   module (or off `this`), and the declaration an imported helper names in
//!   another indexed module;
//! - the declaration text of every module-level binding those read (a base
//!   constant, a config import), so an interpolation is traceable to a name.
//!
//! A site is only offered when what it reaches issues a request: the
//! declaration, or one of its helpers, contains a call the candidate scanner
//! or the same-file wrapper pass already recognised as one. That is the whole
//! test, and it is structural; no client, framework or helper name appears
//! here.
//!
//! Every such site is also a CALL SITE the analyzer is asked about
//! (`sites`), with a span of its own, so its row joins by `candidate_id`
//! rather than by line. And every literal these facts carry is recorded
//! (`literals`), because a path the model states has to be made of text that
//! was in front of it: see [`TargetEvidence`].

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use swc_common::{GLOBALS, Globals, Mark, Spanned, SyntaxContext};
use swc_ecma_ast::*;
use swc_ecma_transforms_base::resolver;
use swc_ecma_visit::{Visit, VisitMutWith, VisitWith};

use crate::swc_scanner::SWC_SPAN_BASE;

/// One declaration's text is cut here. A helper that builds a request is
/// rarely longer; one that is has already stated its URL near the top.
pub const MAX_DECLARATION_BYTES: usize = 4_000;

/// The whole material one file is handed. Declarations the file calls are
/// admitted first, then their helpers, then the bindings they read, so a cut
/// removes context before it removes the thing the site names.
pub const MAX_MATERIAL_BYTES: usize = 16_000;

/// How many module-level bindings one declaration contributes. A body that
/// reads more than this is reading a table, not building a URL.
const MAX_BINDINGS_PER_DECLARATION: usize = 8;

/// The helper name an instance declaration is recorded under.
const INSTANCE: &str = "<instance>";

/// How many calls deep helpers are followed within the declaration's own
/// module.
const MAX_SAME_MODULE_DEPTH: usize = 3;

/// A span in SWC's own numbering for a module parsed with a fresh source map
/// (the scanner's candidate spans are in the same numbering).
type SpanRange = (u32, u32);

/// What one function-like body states: its text, what it calls, what it
/// reads, and the literals it holds.
#[derive(Debug, Clone, Default)]
struct Body {
    span: SpanRange,
    text: String,
    /// Bare identifiers called (`sendJson(...)`).
    calls: BTreeSet<String>,
    /// Members called off `this` (`this.request(...)`).
    this_calls: BTreeSet<String>,
    /// Identifiers read in expression position.
    reads: BTreeSet<String>,
    /// String literal values and template quasis.
    literals: Vec<String>,
}

#[derive(Debug, Clone)]
enum DeclKind {
    /// A function declaration, or a function/arrow bound to a name.
    Function,
    /// An object literal, by member name.
    Object(BTreeMap<String, Body>),
    /// A class, by method name (static and instance alike).
    Class(BTreeMap<String, Body>),
    /// `new C(...)` of the named class.
    Instance(String),
    /// Anything else: a constant, a call's result.
    Value,
}

#[derive(Debug, Clone)]
struct Decl {
    body: Body,
    kind: DeclKind,
    /// The declaration's first line, for naming the object a member sits in.
    header: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Imported {
    Named(String),
    Default,
    Namespace,
}

#[derive(Debug, Clone)]
struct ImportFact {
    specifier: String,
    imported: Imported,
    /// The whole import statement, and where it sits.
    text: String,
    span: SpanRange,
}

/// The declarations of one module a call site can reach, read once per scan.
#[derive(Debug, Clone)]
pub struct ModuleIndex {
    display_path: String,
    decls: HashMap<String, Decl>,
    /// Exported name -> the local declaration it names.
    exports: HashMap<String, String>,
    imports: HashMap<String, ImportFact>,
    /// The module's own request calls: its HTTP candidates and its same-file
    /// wrapper sites.
    request_spans: Vec<SpanRange>,
    /// The base each request's URL opens with, as the source spells it
    /// (`env.API_URL` for `fetch(`${env.API_URL}/v1${path}`)`), where it opens
    /// with a plain binding read. See [`leading_base`].
    request_bases: HashMap<SpanRange, String>,
    /// The requests whose first argument can be a URL.
    url_requests: HashSet<SpanRange>,
}

/// A call in the importing file rooted at an imported binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallThroughSite {
    pub span: SpanRange,
    /// The imported binding the call reaches: the value itself, or the class
    /// a receiver is typed as.
    pub local: String,
    /// Member names between the binding and the call, `.call`/`.apply`
    /// already stripped.
    pub path: Vec<String>,
    /// The receiver as written, when the call goes through a parameter, local
    /// or field typed as an imported class (`client: ApiClient`) rather than
    /// through the imported binding itself.
    pub typed_receiver: Option<String>,
}

/// What one importing file's parse yields.
#[derive(Debug, Clone, Default)]
pub struct ImporterScan {
    pub imports: HashMap<String, (String, Imported)>,
    pub sites: Vec<CallThroughSite>,
}

/// The facts handed to one file.
#[derive(Debug, Clone, Default)]
pub struct ImporterFacts {
    /// Start offsets of the call sites whose declaration issues a request.
    pub sites: BTreeSet<u32>,
    /// Rendered material, one block per module, sorted by module path.
    pub material: Vec<String>,
    /// Literals the admitted declarations hold.
    pub literals: Vec<String>,
    /// The name each bare-function site calls through, by site start offset,
    /// as its declaring module spells it (carrick#1155).
    pub via: HashMap<u32, String>,
    /// The base every request a site reaches opens with, by site start
    /// offset: the module that reads it and the expression as that module
    /// spells it. Present only when all of them agree. The base is a fact of
    /// the helper's source, so the served row takes it from here and the
    /// model's spelling of it is advisory.
    pub bases: HashMap<u32, (PathBuf, String)>,
    /// Declarations cut to [`MAX_DECLARATION_BYTES`] or left out for the
    /// material budget.
    pub truncated: usize,
}

fn unwrap_expr(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(inner) => unwrap_expr(&inner.expr),
        Expr::TsAs(inner) => unwrap_expr(&inner.expr),
        Expr::TsSatisfies(inner) => unwrap_expr(&inner.expr),
        Expr::TsConstAssertion(inner) => unwrap_expr(&inner.expr),
        Expr::TsNonNull(inner) => unwrap_expr(&inner.expr),
        Expr::Await(inner) => unwrap_expr(&inner.arg),
        _ => expr,
    }
}

fn slice(content: &str, span: swc_common::Span) -> String {
    let lo = span.lo.0.saturating_sub(SWC_SPAN_BASE) as usize;
    let hi = span.hi.0.saturating_sub(SWC_SPAN_BASE) as usize;
    content.get(lo..hi).unwrap_or_default().to_string()
}

fn prop_name(key: &PropName) -> Option<String> {
    match key {
        PropName::Ident(ident) => Some(ident.sym.to_string()),
        PropName::Str(s) => Some(s.value.to_string()),
        _ => None,
    }
}

#[derive(Default)]
struct BodyReader {
    calls: BTreeSet<String>,
    this_calls: BTreeSet<String>,
    reads: BTreeSet<String>,
    literals: Vec<String>,
}

impl Visit for BodyReader {
    fn visit_call_expr(&mut self, node: &CallExpr) {
        if let Callee::Expr(callee) = &node.callee {
            match unwrap_expr(callee) {
                Expr::Ident(ident) => {
                    self.calls.insert(ident.sym.to_string());
                }
                Expr::Member(member) => {
                    if let (Expr::This(_), MemberProp::Ident(prop)) =
                        (unwrap_expr(&member.obj), &member.prop)
                    {
                        self.this_calls.insert(prop.sym.to_string());
                    }
                }
                _ => {}
            }
        }
        node.visit_children_with(self);
    }

    fn visit_expr(&mut self, node: &Expr) {
        if let Expr::Ident(ident) = node {
            self.reads.insert(ident.sym.to_string());
        }
        node.visit_children_with(self);
    }

    fn visit_str(&mut self, node: &Str) {
        self.literals.push(node.value.to_string());
    }

    fn visit_tpl_element(&mut self, node: &TplElement) {
        self.literals.push(
            node.cooked
                .as_ref()
                .map(|cooked| cooked.to_string())
                .unwrap_or_else(|| node.raw.to_string()),
        );
    }
}

fn read_body<N: VisitWith<BodyReader> + Spanned>(content: &str, node: &N) -> Body {
    let mut reader = BodyReader::default();
    node.visit_with(&mut reader);
    let span = node.span();
    Body {
        span: (span.lo.0, span.hi.0),
        text: slice(content, span),
        calls: reader.calls,
        this_calls: reader.this_calls,
        reads: reader.reads,
        literals: reader.literals,
    }
}

fn class_members(content: &str, class: &Class) -> BTreeMap<String, Body> {
    let mut members = BTreeMap::new();
    for member in &class.body {
        match member {
            // What an instance is built with is where a client's base usually
            // is (`new Client(`${host}/v2`)` stored by the constructor).
            ClassMember::Constructor(constructor) => {
                members.insert("constructor".to_string(), read_body(content, constructor));
            }
            ClassMember::Method(method) => {
                if let Some(name) = prop_name(&method.key) {
                    members.insert(name, read_body(content, method));
                }
            }
            ClassMember::ClassProp(prop) => {
                if let (Some(name), Some(value)) = (prop_name(&prop.key), &prop.value)
                    && matches!(unwrap_expr(value), Expr::Arrow(_) | Expr::Fn(_))
                {
                    members.insert(name, read_body(content, prop));
                }
            }
            _ => {}
        }
    }
    members
}

fn object_members(content: &str, object: &ObjectLit) -> BTreeMap<String, Body> {
    let mut members = BTreeMap::new();
    for prop in &object.props {
        let PropOrSpread::Prop(prop) = prop else {
            continue;
        };
        match &**prop {
            Prop::KeyValue(kv) => {
                if let Some(name) = prop_name(&kv.key)
                    && matches!(unwrap_expr(&kv.value), Expr::Arrow(_) | Expr::Fn(_))
                {
                    members.insert(name, read_body(content, kv));
                }
            }
            Prop::Method(method) => {
                if let Some(name) = prop_name(&method.key) {
                    members.insert(name, read_body(content, method));
                }
            }
            _ => {}
        }
    }
    members
}

fn kind_of_init(content: &str, init: &Expr) -> DeclKind {
    match unwrap_expr(init) {
        Expr::Arrow(_) | Expr::Fn(_) => DeclKind::Function,
        Expr::Object(object) => DeclKind::Object(object_members(content, object)),
        Expr::Class(class) => DeclKind::Class(class_members(content, &class.class)),
        Expr::New(new) => match unwrap_expr(&new.callee) {
            Expr::Ident(ident) => DeclKind::Instance(ident.sym.to_string()),
            _ => DeclKind::Value,
        },
        _ => DeclKind::Value,
    }
}

fn header_of(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim_end();
    let mut end = line.len().min(200);
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].to_string()
}

impl ModuleIndex {
    /// Read the declarations of `content`. `request_spans` are the module's
    /// own request calls in the scanner's span numbering. `None` when the
    /// module does not parse.
    pub fn build(
        path: &Path,
        display_path: &str,
        content: &str,
        requests: Vec<(SpanRange, Option<String>)>,
    ) -> Option<Self> {
        let (_, module) = crate::swc_scanner::parse_standalone_module(path, content)?;
        let request_spans: Vec<SpanRange> = requests.iter().map(|(span, _)| *span).collect();
        let mut request_bases: HashMap<SpanRange, String> = requests
            .into_iter()
            .filter_map(|(span, base)| Some((span, base?)))
            .collect();
        let mut reader = BaseReader {
            content,
            requests: request_spans.iter().copied().collect(),
            templates: HashMap::new(),
            calls: Vec::new(),
        };
        module.visit_with(&mut reader);
        // Which requests state a URL at all, and the base of those that open
        // with one. A request given a base by the caller (a same-file wrapper
        // site, whose resolved target is known) is one of them.
        let mut url_requests: HashSet<SpanRange> = request_bases.keys().copied().collect();
        for (span, argument) in reader.calls {
            url_requests.insert(span);
            if request_bases.contains_key(&span) {
                continue;
            }
            let base = match argument {
                UrlArgument::Base(base) => Some(base),
                UrlArgument::Binding(name) => reader.templates.get(&name).cloned().flatten(),
                UrlArgument::Opaque => None,
            };
            if let Some(base) = base {
                request_bases.insert(span, base);
            }
        }
        let mut index = ModuleIndex {
            display_path: display_path.to_string(),
            decls: HashMap::new(),
            exports: HashMap::new(),
            imports: HashMap::new(),
            request_spans,
            request_bases,
            url_requests,
        };
        for item in &module.body {
            let item_text = slice(content, item.span());
            match item {
                ModuleItem::ModuleDecl(ModuleDecl::Import(import)) => {
                    if import.type_only {
                        continue;
                    }
                    for spec in &import.specifiers {
                        let (local, imported) = match spec {
                            ImportSpecifier::Named(named) if !named.is_type_only => (
                                named.local.sym.to_string(),
                                Imported::Named(match &named.imported {
                                    Some(ModuleExportName::Ident(ident)) => ident.sym.to_string(),
                                    Some(ModuleExportName::Str(s)) => s.value.to_string(),
                                    None => named.local.sym.to_string(),
                                }),
                            ),
                            ImportSpecifier::Default(default) => {
                                (default.local.sym.to_string(), Imported::Default)
                            }
                            ImportSpecifier::Namespace(ns) => {
                                (ns.local.sym.to_string(), Imported::Namespace)
                            }
                            _ => continue,
                        };
                        index.imports.insert(
                            local,
                            ImportFact {
                                specifier: import.src.value.to_string(),
                                imported,
                                text: item_text.clone(),
                                span: (import.span.lo.0, import.span.hi.0),
                            },
                        );
                    }
                }
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => {
                    for name in index.add_decl(content, &export.decl, &item_text) {
                        index.exports.insert(name.clone(), name);
                    }
                }
                ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(export)) => {
                    let (name, kind, body) = match &export.decl {
                        DefaultDecl::Fn(f) => (
                            f.ident.as_ref().map(|i| i.sym.to_string()),
                            DeclKind::Function,
                            read_body(content, &*f.function),
                        ),
                        DefaultDecl::Class(c) => (
                            c.ident.as_ref().map(|i| i.sym.to_string()),
                            DeclKind::Class(class_members(content, &c.class)),
                            read_body(content, &*c.class),
                        ),
                        DefaultDecl::TsInterfaceDecl(_) => continue,
                    };
                    let local = name.unwrap_or_else(|| "default".to_string());
                    index.decls.insert(
                        local.clone(),
                        Decl {
                            body: Body {
                                text: item_text.clone(),
                                ..body
                            },
                            kind,
                            header: header_of(&item_text),
                        },
                    );
                    index.exports.insert("default".to_string(), local);
                }
                ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(export)) => {
                    match unwrap_expr(&export.expr) {
                        Expr::Ident(ident) => {
                            index
                                .exports
                                .insert("default".to_string(), ident.sym.to_string());
                        }
                        expr => {
                            let mut body = read_body(content, expr);
                            body.text = item_text.clone();
                            index.decls.insert(
                                "default".to_string(),
                                Decl {
                                    body,
                                    kind: kind_of_init(content, expr),
                                    header: header_of(&item_text),
                                },
                            );
                            index
                                .exports
                                .insert("default".to_string(), "default".to_string());
                        }
                    }
                }
                ModuleItem::ModuleDecl(ModuleDecl::ExportNamed(named)) if named.src.is_none() => {
                    for spec in &named.specifiers {
                        let ExportSpecifier::Named(spec) = spec else {
                            continue;
                        };
                        let orig = match &spec.orig {
                            ModuleExportName::Ident(ident) => ident.sym.to_string(),
                            ModuleExportName::Str(s) => s.value.to_string(),
                        };
                        let exported = match &spec.exported {
                            Some(ModuleExportName::Ident(ident)) => ident.sym.to_string(),
                            Some(ModuleExportName::Str(s)) => s.value.to_string(),
                            None => orig.clone(),
                        };
                        index.exports.insert(exported, orig);
                    }
                }
                ModuleItem::Stmt(Stmt::Decl(decl)) => {
                    index.add_decl(content, decl, &item_text);
                }
                _ => {}
            }
        }
        Some(index)
    }

    /// Record a declaration statement, returning the names it binds.
    fn add_decl(
        &mut self,
        content: &str,
        decl: &swc_ecma_ast::Decl,
        item_text: &str,
    ) -> Vec<String> {
        match decl {
            swc_ecma_ast::Decl::Fn(f) => {
                let name = f.ident.sym.to_string();
                let mut body = read_body(content, &*f.function);
                body.text = item_text.to_string();
                self.decls.insert(
                    name.clone(),
                    Decl {
                        body,
                        kind: DeclKind::Function,
                        header: header_of(item_text),
                    },
                );
                vec![name]
            }
            swc_ecma_ast::Decl::Class(c) => {
                let name = c.ident.sym.to_string();
                let mut body = read_body(content, &*c.class);
                body.text = item_text.to_string();
                self.decls.insert(
                    name.clone(),
                    Decl {
                        body,
                        kind: DeclKind::Class(class_members(content, &c.class)),
                        header: header_of(item_text),
                    },
                );
                vec![name]
            }
            swc_ecma_ast::Decl::Var(var) => {
                let mut names = Vec::new();
                for declarator in &var.decls {
                    let Pat::Ident(binding) = &declarator.name else {
                        continue;
                    };
                    let name = binding.id.sym.to_string();
                    let kind = declarator
                        .init
                        .as_deref()
                        .map(|init| kind_of_init(content, init))
                        .unwrap_or(DeclKind::Value);
                    // One declarator of a multi-declarator statement is read
                    // on its own span, so a sibling's text is not attributed
                    // to it; a single one keeps the statement (and `export`).
                    let mut body = read_body(content, declarator);
                    if var.decls.len() == 1 {
                        body.text = item_text.to_string();
                    }
                    let header = header_of(&body.text);
                    self.decls.insert(name.clone(), Decl { body, kind, header });
                    names.push(name);
                }
                names
            }
            _ => Vec::new(),
        }
    }

    fn reaches_request(&self, body: &Body) -> bool {
        self.request_spans
            .iter()
            .any(|(start, end)| body.span.0 <= *start && *end <= body.span.1)
    }

    /// The bases of the requests inside `body`, one entry per request (`None`
    /// for a request whose base is not a plain binding read).
    fn bases_within(&self, body: &Body) -> Vec<Option<&str>> {
        self.request_spans
            .iter()
            .filter(|span| self.url_requests.contains(*span))
            .filter(|(start, end)| body.span.0 <= *start && *end <= body.span.1)
            .map(|span| self.request_bases.get(span).map(String::as_str))
            .collect()
    }

    /// The body an exported name and member path reach in this module, with
    /// the members beside it (for `this.x()` helpers) and the header of the
    /// object or class it sits in.
    fn member(&self, export: &str, path: &[String]) -> Option<Member<'_>> {
        let local = self.exports.get(export)?;
        let decl = self.decls.get(local)?;
        match (path, &decl.kind) {
            ([], DeclKind::Function) => Some(Member {
                body: &decl.body,
                siblings: None,
                header: None,
                function_name: Some(local.as_str()),
                construction: None,
            }),
            ([member], DeclKind::Object(members)) | ([member], DeclKind::Class(members)) => {
                Some(Member {
                    body: members.get(member)?,
                    siblings: Some(members),
                    header: Some(decl.header.as_str()),
                    function_name: None,
                    construction: None,
                })
            }
            ([member], DeclKind::Instance(class)) => {
                let class_decl = self.decls.get(class)?;
                let DeclKind::Class(members) = &class_decl.kind else {
                    return None;
                };
                Some(Member {
                    body: members.get(member)?,
                    siblings: Some(members),
                    header: Some(class_decl.header.as_str()),
                    function_name: None,
                    construction: Some(&decl.body),
                })
            }
            // A member of a value this module builds by a call (a configured
            // client) is declared by whatever built it, which is not in this
            // module: the value's own declaration is what states its base.
            ([_, ..], DeclKind::Value) => Some(Member {
                body: &decl.body,
                siblings: None,
                header: None,
                function_name: None,
                construction: None,
            }),
            _ => None,
        }
    }
}

/// The base a URL opens with, when it opens with a plain read of a binding
/// or of the environment: `${env.API_URL}/v1${path}` -> `env.API_URL`. A read
/// off `this`, a call, or a template that opens with text states no base a
/// file other than this one could resolve.
pub fn leading_base(target: &str) -> Option<String> {
    let rest = target.trim().strip_prefix("${")?;
    let end = rest.find('}')?;
    let base = rest[..end].trim();
    let plain = !base.is_empty()
        && !base.starts_with("this.")
        && base
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '.');
    plain.then(|| base.to_string())
}

enum UrlArgument {
    /// A template opening with a plain binding read.
    Base(String),
    /// A binding, which may hold such a template.
    Binding(String),
    /// A string, concatenation or other read that opens with no base a
    /// consumer could resolve.
    Opaque,
}

/// Reads, for each request call, the base its URL argument opens with.
struct BaseReader<'a> {
    content: &'a str,
    requests: HashSet<SpanRange>,
    /// `const url = `${base}...`` declarations by name. A name declared twice
    /// maps to `None`.
    templates: HashMap<String, Option<String>>,
    calls: Vec<(SpanRange, UrlArgument)>,
}

impl BaseReader<'_> {
    fn template_base(&self, expr: &Expr) -> Option<String> {
        let Expr::Tpl(tpl) = unwrap_expr(expr) else {
            return None;
        };
        let first = tpl.quasis.first()?;
        if !first.raw.is_empty() {
            return None;
        }
        let base = tpl.exprs.first()?;
        leading_base(&format!("${{{}}}", slice(self.content, base.span())))
    }
}

impl Visit for BaseReader<'_> {
    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        if let (Pat::Ident(binding), Some(init)) = (&node.name, &node.init) {
            let base = self.template_base(init);
            if base.is_some() {
                let name = binding.id.sym.to_string();
                let entry = self.templates.entry(name).or_insert(base.clone());
                if *entry != base {
                    *entry = None;
                }
            }
        }
        node.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, node: &CallExpr) {
        let span = (node.span.lo.0, node.span.hi.0);
        if self.requests.contains(&span)
            && let Some(first) = node.args.first()
            && first.spread.is_none()
        {
            // Only an argument that can be a URL makes the call a URL
            // request here; `res.json()` and `p.catch(() => null)` are
            // candidates the scanner raised that issue nothing of their own.
            let argument = match unwrap_expr(&first.expr) {
                expr if self.template_base(expr).is_some() => {
                    self.template_base(expr).map(UrlArgument::Base)
                }
                Expr::Ident(ident) => Some(UrlArgument::Binding(ident.sym.to_string())),
                Expr::Tpl(_) | Expr::Lit(Lit::Str(_)) | Expr::Bin(_) | Expr::Member(_) => {
                    Some(UrlArgument::Opaque)
                }
                _ => None,
            };
            if let Some(argument) = argument {
                self.calls.push((span, argument));
            }
        }
        node.visit_children_with(self);
    }
}

/// What an exported name and member path reach in one module.
struct Member<'a> {
    body: &'a Body,
    /// The object or class the body sits in, for `this.x()` helpers.
    siblings: Option<&'a BTreeMap<String, Body>>,
    /// The first line of that object or class.
    header: Option<&'a str>,
    /// The declared name, when the body is a bare function.
    function_name: Option<&'a str>,
    /// The instance declaration, when the call goes through `new C(...)`.
    construction: Option<&'a Body>,
}

struct SiteReader<'a> {
    imports: HashMap<(String, SyntaxContext), (String, Imported)>,
    /// Bindings annotated with an imported type, by resolver identity, to the
    /// type's local name.
    typed_bindings: HashMap<(String, SyntaxContext), String>,
    /// Class fields annotated with an imported type, by field name. A name two
    /// classes in the file annotate differently states nothing.
    typed_fields: HashMap<String, Option<String>>,
    sites: &'a mut Vec<CallThroughSite>,
}

/// The imported type an annotation names, when it is a plain reference.
fn annotated_type(
    annotation: Option<&TsTypeAnn>,
    type_imports: &HashSet<String>,
) -> Option<String> {
    let TsType::TsTypeRef(reference) = &*annotation?.type_ann else {
        return None;
    };
    let TsEntityName::Ident(name) = &reference.type_name else {
        return None;
    };
    let name = name.sym.to_string();
    type_imports.contains(&name).then_some(name)
}

/// Records which bindings and fields carry an imported type.
struct TypedBindingReader<'a> {
    type_imports: &'a HashSet<String>,
    bindings: HashMap<(String, SyntaxContext), String>,
    fields: HashMap<String, Option<String>>,
}

impl TypedBindingReader<'_> {
    fn field(&mut self, name: String, ty: Option<String>) {
        let Some(ty) = ty else {
            return;
        };
        match self.fields.get(&name) {
            Some(Some(existing)) if *existing != ty => {
                self.fields.insert(name, None);
            }
            Some(_) => {}
            None => {
                self.fields.insert(name, Some(ty));
            }
        }
    }
}

impl Visit for TypedBindingReader<'_> {
    fn visit_binding_ident(&mut self, node: &BindingIdent) {
        if let Some(ty) = annotated_type(node.type_ann.as_deref(), self.type_imports) {
            self.bindings
                .insert((node.id.sym.to_string(), node.id.ctxt), ty);
        }
        node.visit_children_with(self);
    }

    fn visit_class_prop(&mut self, node: &ClassProp) {
        if let Some(name) = prop_name(&node.key) {
            let ty = annotated_type(node.type_ann.as_deref(), self.type_imports);
            self.field(name, ty);
        }
        node.visit_children_with(self);
    }

    fn visit_ts_param_prop(&mut self, node: &TsParamProp) {
        if let TsParamPropParam::Ident(binding) = &node.param {
            let ty = annotated_type(binding.type_ann.as_deref(), self.type_imports);
            self.field(binding.id.sym.to_string(), ty);
        }
        node.visit_children_with(self);
    }
}

impl Visit for SiteReader<'_> {
    fn visit_call_expr(&mut self, node: &CallExpr) {
        if let Callee::Expr(callee) = &node.callee {
            let mut path: Vec<String> = Vec::new();
            let mut expr = unwrap_expr(callee);
            // The root the chain ends at: an identifier, or `this.<field>`.
            let root = loop {
                match expr {
                    Expr::Ident(ident) => break Some((Some(ident), None)),
                    Expr::Member(member) => {
                        let name = match &member.prop {
                            MemberProp::Ident(prop) => prop.sym.to_string(),
                            MemberProp::Computed(computed) => match unwrap_expr(&computed.expr) {
                                Expr::Lit(Lit::Str(s)) => s.value.to_string(),
                                _ => break None,
                            },
                            MemberProp::PrivateName(_) => break None,
                        };
                        if matches!(unwrap_expr(&member.obj), Expr::This(_)) {
                            break Some((None, Some(name)));
                        }
                        path.push(name);
                        expr = unwrap_expr(&member.obj);
                    }
                    _ => break None,
                }
            };
            path.reverse();
            if matches!(path.last().map(String::as_str), Some("call" | "apply")) {
                path.pop();
            }
            let span = (node.span.lo.0, node.span.hi.0);
            match root {
                Some((Some(ident), None))
                    if self
                        .imports
                        .contains_key(&(ident.sym.to_string(), ident.ctxt)) =>
                {
                    self.sites.push(CallThroughSite {
                        span,
                        local: ident.sym.to_string(),
                        path,
                        typed_receiver: None,
                    });
                }
                Some((Some(ident), None)) if path.len() == 1 => {
                    if let Some(ty) = self
                        .typed_bindings
                        .get(&(ident.sym.to_string(), ident.ctxt))
                    {
                        self.sites.push(CallThroughSite {
                            span,
                            local: ty.clone(),
                            path,
                            typed_receiver: Some(ident.sym.to_string()),
                        });
                    }
                }
                Some((None, Some(field))) if path.len() == 1 => {
                    if let Some(Some(ty)) = self.typed_fields.get(&field) {
                        self.sites.push(CallThroughSite {
                            span,
                            local: ty.clone(),
                            path,
                            typed_receiver: Some(format!("this.{field}")),
                        });
                    }
                }
                _ => {}
            }
        }
        node.visit_children_with(self);
    }
}

/// Every call in `content` rooted at an imported value binding, or at a
/// receiver typed as an imported class, in source order. `None` when the file
/// does not parse.
pub fn scan_importer(path: &Path, content: &str) -> Option<ImporterScan> {
    let (_, mut module) = crate::swc_scanner::parse_standalone_module(path, content)?;
    let (_, is_typescript) = crate::parser::syntax_for_path(path);
    GLOBALS.set(&Globals::new(), || {
        let unresolved = Mark::new();
        let top_level = Mark::new();
        module.visit_mut_with(&mut resolver(unresolved, top_level, is_typescript));
    });
    let mut scan = ImporterScan::default();
    let mut keyed: HashMap<(String, SyntaxContext), (String, Imported)> = HashMap::new();
    let mut type_imports: HashSet<String> = HashSet::new();
    for item in &module.body {
        let ModuleItem::ModuleDecl(ModuleDecl::Import(import)) = item else {
            continue;
        };
        for spec in &import.specifiers {
            let (local, imported, type_only) = match spec {
                ImportSpecifier::Named(named) => (
                    &named.local,
                    Imported::Named(match &named.imported {
                        Some(ModuleExportName::Ident(ident)) => ident.sym.to_string(),
                        Some(ModuleExportName::Str(s)) => s.value.to_string(),
                        None => named.local.sym.to_string(),
                    }),
                    import.type_only || named.is_type_only,
                ),
                ImportSpecifier::Default(default) => {
                    (&default.local, Imported::Default, import.type_only)
                }
                ImportSpecifier::Namespace(ns) => {
                    (&ns.local, Imported::Namespace, import.type_only)
                }
            };
            let entry = (import.src.value.to_string(), imported);
            scan.imports.insert(local.sym.to_string(), entry.clone());
            // Any import can name a class a receiver is typed as; only a value
            // import is a binding a call can go through directly.
            type_imports.insert(local.sym.to_string());
            if !type_only {
                keyed.insert((local.sym.to_string(), local.ctxt), entry);
            }
        }
    }
    if scan.imports.is_empty() {
        return Some(scan);
    }
    let mut typed = TypedBindingReader {
        type_imports: &type_imports,
        bindings: HashMap::new(),
        fields: HashMap::new(),
    };
    module.visit_with(&mut typed);
    let mut sites = Vec::new();
    module.visit_with(&mut SiteReader {
        imports: keyed,
        typed_bindings: typed.bindings,
        typed_fields: typed.fields,
        sites: &mut sites,
    });
    sites.sort_by_key(|site| site.span);
    sites.dedup_by_key(|site| site.span);
    scan.sites = sites;
    Some(scan)
}

/// How the facts reader reaches other modules: which module paths a specifier
/// names from the module that writes it (the module itself first, then any
/// modules a re-export barrel stands for), and the declarations of one.
pub trait ModuleResolver {
    fn modules(&self, from: &Path, specifier: &str) -> Vec<PathBuf>;
    /// A module whose declarations can issue a request.
    fn index(&self, module: &Path) -> Option<Rc<ModuleIndex>>;
    /// Any module's declarations, read only for the binding a handed
    /// declaration reads (a config object), whether or not it issues anything.
    fn declarations(&self, module: &Path) -> Option<Rc<ModuleIndex>>;
}

#[derive(Debug)]
struct Entry {
    priority: u8,
    module: PathBuf,
    display: String,
    span: SpanRange,
    label: String,
    text: String,
    literals: Vec<String>,
}

/// A body found in some module, with that module's index kept alive.
struct Found {
    module: PathBuf,
    index: Rc<ModuleIndex>,
    body: Body,
}

/// The first module `specifier` names from `from` that declares `export`
/// with `path` under it.
/// A member found in some module, owned so the index can be borrowed again.
struct FoundMember {
    found: Found,
    siblings: Option<BTreeMap<String, Body>>,
    header: Option<String>,
    function_name: Option<String>,
    construction: Option<Body>,
}

fn find_member(
    resolver: &dyn ModuleResolver,
    from: &Path,
    specifier: &str,
    export: &str,
    path: &[String],
) -> Option<FoundMember> {
    resolver
        .modules(from, specifier)
        .into_iter()
        .find_map(|module| {
            let index = resolver.index(&module)?;
            let member = index.member(export, path)?;
            let (body, siblings, header, function_name, construction) = (
                member.body.clone(),
                member.siblings.cloned(),
                member.header.map(str::to_string),
                member.function_name.map(str::to_string),
                member.construction.cloned(),
            );
            Some(FoundMember {
                found: Found {
                    module,
                    index,
                    body,
                },
                siblings,
                header,
                function_name,
                construction,
            })
        })
}

/// The facts for one importing file.
pub fn importer_facts(
    importer: &Path,
    scan: &ImporterScan,
    resolver: &dyn ModuleResolver,
) -> ImporterFacts {
    let mut facts = ImporterFacts::default();
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen: HashSet<(PathBuf, SpanRange)> = HashSet::new();
    let mut push = |entries: &mut Vec<Entry>, entry: Entry| {
        if seen.insert((entry.module.clone(), entry.span)) {
            entries.push(entry);
        }
    };

    for site in &scan.sites {
        let Some((specifier, imported)) = scan.imports.get(&site.local) else {
            continue;
        };
        let (export, path): (String, &[String]) = match imported {
            Imported::Named(name) => (name.clone(), &site.path[..]),
            Imported::Default => ("default".to_string(), &site.path[..]),
            Imported::Namespace => match site.path.split_first() {
                Some((first, rest)) => (first.clone(), rest),
                None => continue,
            },
        };
        let Some(FoundMember {
            found: target,
            siblings,
            header,
            function_name,
            construction,
        }) = find_member(resolver, importer, specifier, &export, path)
        else {
            continue;
        };

        // Helpers: the same-module declarations and `this` members the
        // declaration calls, followed within its module up to
        // `MAX_SAME_MODULE_DEPTH` calls deep (a client method that delegates
        // to a sibling that delegates to the one issuing the request is the
        // ordinary class shape), and one hop into another module for a helper
        // any of them imports.
        let mut helpers: Vec<(Found, String)> = Vec::new();
        let mut visited: HashSet<SpanRange> = HashSet::from([target.body.span]);
        let mut frontier: Vec<Body> = vec![target.body.clone()];
        for _ in 0..MAX_SAME_MODULE_DEPTH {
            let mut next: Vec<Body> = Vec::new();
            for body in &frontier {
                for name in &body.calls {
                    if let Some(decl) = target.index.decls.get(name) {
                        if visited.insert(decl.body.span) {
                            next.push(decl.body.clone());
                            helpers.push((
                                Found {
                                    module: target.module.clone(),
                                    index: Rc::clone(&target.index),
                                    body: decl.body.clone(),
                                },
                                name.clone(),
                            ));
                        }
                    } else if let Some(import) = target.index.imports.get(name) {
                        let export = match &import.imported {
                            Imported::Named(name) => name.clone(),
                            Imported::Default => "default".to_string(),
                            Imported::Namespace => continue,
                        };
                        if let Some(helper) =
                            find_member(resolver, &target.module, &import.specifier, &export, &[])
                            && visited.insert(helper.found.body.span)
                        {
                            helpers.push((helper.found, name.clone()));
                        }
                    }
                }
                if let Some(siblings) = &siblings {
                    for name in &body.this_calls {
                        if let Some(sibling) = siblings.get(name)
                            && visited.insert(sibling.span)
                        {
                            next.push(sibling.clone());
                            helpers.push((
                                Found {
                                    module: target.module.clone(),
                                    index: Rc::clone(&target.index),
                                    body: sibling.clone(),
                                },
                                format!("this.{name}"),
                            ));
                        }
                    }
                }
            }
            frontier = next;
        }
        // A call through an instance is also handed how the instance is built:
        // the constructor it runs, and the declaration that constructs it when
        // that is in view.
        if (construction.is_some() || site.typed_receiver.is_some())
            && let Some(constructor) = siblings.as_ref().and_then(|s| s.get("constructor"))
            && visited.insert(constructor.span)
        {
            helpers.push((
                Found {
                    module: target.module.clone(),
                    index: Rc::clone(&target.index),
                    body: constructor.clone(),
                },
                "this.constructor".to_string(),
            ));
        }
        if let Some(construction) = construction {
            helpers.push((
                Found {
                    module: target.module.clone(),
                    index: Rc::clone(&target.index),
                    body: construction,
                },
                INSTANCE.to_string(),
            ));
        }

        let reaches = target.index.reaches_request(&target.body)
            || helpers
                .iter()
                .any(|(helper, _)| helper.index.reaches_request(&helper.body));
        if !reaches {
            continue;
        }
        facts.sites.insert(site.span.0);
        if let Some(name) = function_name {
            facts.via.insert(site.span.0, name);
        }
        let mut bases: Vec<(PathBuf, Option<&str>)> = target
            .index
            .bases_within(&target.body)
            .into_iter()
            .map(|base| (target.module.clone(), base))
            .collect();
        for (helper, _) in &helpers {
            bases.extend(
                helper
                    .index
                    .bases_within(&helper.body)
                    .into_iter()
                    .map(|base| (helper.module.clone(), base)),
            );
        }
        if let Some((module, Some(base))) = bases.first()
            && bases
                .iter()
                .all(|(other_module, other)| other_module == module && *other == Some(*base))
        {
            facts
                .bases
                .insert(site.span.0, (module.clone(), base.to_string()));
        }

        let receiver = site
            .typed_receiver
            .as_deref()
            .unwrap_or(site.local.as_str());
        let called_as = std::iter::once(receiver)
            .chain(site.path.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(".");
        // Several calls reaching one declaration (a client value called as
        // `lab.get` and `lab.post`) are named on its one entry, so the label
        // does not read as though only the first of them went through it.
        if let Some(existing) = entries.iter_mut().find(|entry| {
            entry.priority == 0 && entry.module == target.module && entry.span == target.body.span
        }) {
            let (names, declared) = match existing.label.split_once("; declared in: ") {
                Some((names, header)) => (names.to_string(), format!("; declared in: {header}")),
                None => (existing.label.clone(), String::new()),
            };
            let already = names
                .trim_start_matches("called by this file as ")
                .split(", ")
                .any(|name| name == called_as);
            if !already {
                existing.label = format!("{names}, {called_as}{declared}");
            }
        }
        let label = match header {
            Some(header) => format!("called by this file as {called_as}; declared in: {header}"),
            None => format!("called by this file as {called_as}"),
        };
        push(
            &mut entries,
            Entry {
                priority: 0,
                module: target.module.clone(),
                display: target.index.display_path.clone(),
                span: target.body.span,
                label,
                text: target.body.text.clone(),
                literals: target.body.literals.clone(),
            },
        );

        let mut bindings: Vec<(&Found, &str)> = Vec::new();
        let mut helper_names: HashSet<&str> = HashSet::new();
        for (helper, name) in &helpers {
            helper_names.insert(name.as_str());
            push(
                &mut entries,
                Entry {
                    priority: 1,
                    module: helper.module.clone(),
                    display: helper.index.display_path.clone(),
                    span: helper.body.span,
                    label: if name == INSTANCE {
                        "the instance this file's calls go through".to_string()
                    } else if name == "this.constructor" {
                        "the constructor that instance runs".to_string()
                    } else {
                        format!("helper a declaration in this section calls: {name}")
                    },
                    text: helper.body.text.clone(),
                    literals: helper.body.literals.clone(),
                },
            );
            for read in &helper.body.reads {
                bindings.push((helper, read.as_str()));
            }
        }
        for read in &target.body.reads {
            bindings.push((&target, read.as_str()));
        }
        let mut admitted = 0;
        for (owner, name) in bindings {
            if admitted >= MAX_BINDINGS_PER_DECLARATION || helper_names.contains(name) {
                continue;
            }
            let entry = if let Some(decl) = owner.index.decls.get(name) {
                if decl.body.span == owner.body.span
                    || !matches!(decl.kind, DeclKind::Value | DeclKind::Object(_))
                {
                    continue;
                }
                Entry {
                    priority: 2,
                    module: owner.module.clone(),
                    display: owner.index.display_path.clone(),
                    span: decl.body.span,
                    label: format!("binding a declaration in this section reads: {name}"),
                    text: decl.body.text.clone(),
                    literals: decl.body.literals.clone(),
                }
            } else if let Some(import) = owner.index.imports.get(name) {
                // One hop further for an imported binding: what the module it
                // comes from declares it as, so an interpolation of it
                // (`${env.API_URL}`) is traceable to its source.
                let export = match &import.imported {
                    Imported::Named(export) => Some(export.clone()),
                    Imported::Default => Some("default".to_string()),
                    Imported::Namespace => None,
                };
                if let Some(export) = export
                    && let Some((module, declared)) = resolver
                        .modules(&owner.module, &import.specifier)
                        .into_iter()
                        .find_map(|module| {
                            let index = resolver.declarations(&module)?;
                            let local = index.exports.get(&export)?;
                            let decl = index.decls.get(local)?;
                            matches!(decl.kind, DeclKind::Value | DeclKind::Object(_)).then(|| {
                                (
                                    module.clone(),
                                    (index.display_path.clone(), decl.body.clone()),
                                )
                            })
                        })
                {
                    let (display, body) = declared;
                    push(
                        &mut entries,
                        Entry {
                            priority: 2,
                            module,
                            display,
                            span: body.span,
                            label: format!(
                                "declaration of {name}, which a declaration in this section imports"
                            ),
                            text: body.text,
                            literals: body.literals,
                        },
                    );
                }
                Entry {
                    priority: 2,
                    module: owner.module.clone(),
                    display: owner.index.display_path.clone(),
                    // An import is one statement: key it by where it sits.
                    span: import.span,
                    label: format!("binding a declaration in this section reads: {name}"),
                    text: import.text.clone(),
                    literals: Vec::new(),
                }
            } else {
                continue;
            };
            admitted += 1;
            push(&mut entries, entry);
        }
    }

    // Admit by priority under the budget, then render grouped by module in
    // source order, so the same inputs always yield the same bytes.
    entries.sort_by(|a, b| (a.priority, &a.module, a.span).cmp(&(b.priority, &b.module, b.span)));
    let mut used = 0;
    let mut by_module: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
    for mut entry in entries {
        if entry.text.len() > MAX_DECLARATION_BYTES {
            let mut end = MAX_DECLARATION_BYTES;
            while end > 0 && !entry.text.is_char_boundary(end) {
                end -= 1;
            }
            entry.text.truncate(end);
            entry.text.push_str("\n// (truncated)");
            facts.truncated += 1;
        }
        let cost = entry.text.len() + entry.label.len() + 4;
        if used + cost > MAX_MATERIAL_BYTES {
            facts.truncated += 1;
            continue;
        }
        used += cost;
        by_module
            .entry(entry.display.clone())
            .or_default()
            .push(entry);
    }
    // The module a site's declaration sits in comes before the modules that
    // only supply its helpers, so the reader meets what the file calls first.
    let mut blocks: Vec<(u8, String, Vec<Entry>)> = by_module
        .into_iter()
        .map(|(display, entries)| {
            let first = entries
                .iter()
                .map(|entry| entry.priority)
                .min()
                .unwrap_or(u8::MAX);
            (first, display, entries)
        })
        .collect();
    blocks.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    for (_, display, mut module_entries) in blocks {
        module_entries.sort_by_key(|entry| entry.span);
        // One import statement binds several names; state it once.
        module_entries.dedup_by_key(|entry| entry.span);
        let mut block = format!("--- module: {display} ---\n");
        for entry in module_entries {
            block.push_str("// ");
            block.push_str(&entry.label);
            block.push('\n');
            block.push_str(&entry.text);
            block.push('\n');
            facts.literals.extend(entry.literals);
        }
        facts.material.push(block);
    }
    facts
}

/// The literal text a model-stated target has to be made of (carrick#1146,
/// Design 1 (c)).
///
/// A path segment or host the model writes into a data call's target is
/// evidence-backed when it is part of some string literal or template quasi
/// in the analyzed file, or in the declarations the analyzer was handed for
/// it. A segment that is neither was not in front of the model: it was
/// invented, and the row is dropped rather than indexed as a route that exists
/// nowhere.
///
/// Segments, not whole paths: a wrapper's base prefix and a site's argument
/// are two literals the model is told to join, so the joined path is never
/// one literal. Interpolations (`${...}`), path parameters (`:id`, `{id}`) and
/// a query string are not literal claims and are not checked.
#[derive(Debug, Default)]
pub struct TargetEvidence {
    segments: HashSet<String>,
    literals: Vec<String>,
}

impl TargetEvidence {
    pub fn new<'a>(literals: impl IntoIterator<Item = &'a str>) -> Self {
        let mut evidence = TargetEvidence::default();
        for literal in literals {
            for segment in literal.split(['/', '?', '&', '#', '=']) {
                let segment = segment.trim();
                if !segment.is_empty() {
                    evidence.segments.insert(segment.to_string());
                }
            }
            evidence.literals.push(literal.to_string());
        }
        evidence
    }

    /// Every literal string and template quasi in `content`. Empty when the
    /// file does not parse.
    pub fn file_literals(path: &Path, content: &str) -> Vec<String> {
        let Some((_, module)) = crate::swc_scanner::parse_standalone_module(path, content) else {
            return Vec::new();
        };
        let mut reader = BodyReader::default();
        module.visit_with(&mut reader);
        reader.literals
    }

    /// The first part of `target` that no literal states, or `None` when
    /// every literal claim it makes is backed.
    pub fn unbacked_part(&self, target: &str) -> Option<String> {
        let stripped = strip_interpolations(target.trim());
        let mut rest = stripped.as_str();
        if let Some((scheme, after)) = rest.split_once("://")
            && !scheme.is_empty()
            && scheme.chars().all(|c| c.is_ascii_alphabetic())
        {
            let host_end = after.find('/').unwrap_or(after.len());
            let host = &after[..host_end];
            if !host.is_empty()
                && !host.contains('\u{0}')
                && !self.literals.iter().any(|literal| literal.contains(host))
            {
                return Some(host.to_string());
            }
            rest = &after[host_end..];
        }
        let path = rest.split(['?', '#']).next().unwrap_or_default();
        for segment in path.split('/') {
            let segment = segment.trim();
            if segment.is_empty()
                || segment.contains('\u{0}')
                || segment.starts_with(':')
                || (segment.starts_with('{') && segment.ends_with('}'))
                || segment == "*"
                || segment == "**"
            {
                continue;
            }
            if !self.segments.contains(segment) {
                return Some(segment.to_string());
            }
        }
        None
    }
}

/// `target` with every `${...}` replaced by a NUL placeholder, so a segment
/// or host that holds one is recognisable as not a literal claim.
fn strip_interpolations(target: &str) -> String {
    let mut out = String::with_capacity(target.len());
    let mut chars = target.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'{') {
            chars.next();
            let mut depth = 1;
            for inner in chars.by_ref() {
                match inner {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            out.push('\u{0}');
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(HashMap<String, PathBuf>, HashMap<PathBuf, Rc<ModuleIndex>>);

    impl ModuleResolver for Fixed {
        fn modules(&self, _from: &Path, specifier: &str) -> Vec<PathBuf> {
            self.0.get(specifier).cloned().into_iter().collect()
        }
        fn index(&self, module: &Path) -> Option<Rc<ModuleIndex>> {
            self.1.get(module).cloned()
        }
        fn declarations(&self, module: &Path) -> Option<Rc<ModuleIndex>> {
            self.1.get(module).cloned()
        }
    }

    /// Spans of every call in `content` whose source text starts with one of
    /// `prefixes`: a stand-in for the candidate scanner's request spans.
    fn spans_of(content: &str, prefixes: &[&str]) -> Vec<SpanRange> {
        let (_, module) =
            crate::swc_scanner::parse_standalone_module(Path::new("m.ts"), content).unwrap();
        struct Calls<'a>(&'a str, &'a [&'a str], Vec<SpanRange>);
        impl Visit for Calls<'_> {
            fn visit_call_expr(&mut self, node: &CallExpr) {
                let text = slice(self.0, node.span);
                if self.1.iter().any(|p| text.starts_with(p)) {
                    self.2.push((node.span.lo.0, node.span.hi.0));
                }
                node.visit_children_with(self);
            }
        }
        let mut calls = Calls(content, prefixes, Vec::new());
        module.visit_with(&mut calls);
        calls.2
    }

    fn unbased(spans: Vec<SpanRange>) -> Vec<(SpanRange, Option<String>)> {
        spans.into_iter().map(|span| (span, None)).collect()
    }

    const CLIENT: &str = r#"import { settings } from "./settings";

const PREFIX = "/catalog/v3";

export async function sendJson(verb: string, path: string, body?: object) {
  const url = `${settings.gatewayUrl}${PREFIX}${path}`;
  const res = await fetch(url, { method: verb, body: JSON.stringify(body) });
  await res.json();
  return res.json();
}

export function formatLabel(label: string) {
  return label.toUpperCase();
}

export const shelvesApi = {
  list: () => sendJson("GET", "/shelves"),
  rename: (shelfId: string, name: string) =>
    sendJson("PATCH", `/shelves/${shelfId}/label`, { name }),
  describe: (shelfId: string) => formatLabel(shelfId),
};
"#;

    fn client_index() -> ModuleIndex {
        let spans = spans_of(CLIENT, &["fetch(", "res.json("]);
        ModuleIndex::build(
            Path::new("client.ts"),
            "src/client.ts",
            CLIENT,
            unbased(spans),
        )
        .unwrap()
    }

    fn facts_for(consumer: &str) -> (ImporterScan, ImporterFacts) {
        let scan = scan_importer(Path::new("page.tsx"), consumer).unwrap();
        let settings =
            "export const settings = {\n  gatewayUrl: process.env.SHELF_GATEWAY ?? \"\",\n};\n";
        let settings_index = ModuleIndex::build(
            Path::new("settings.ts"),
            "src/settings.ts",
            settings,
            Vec::new(),
        )
        .unwrap();
        let resolver = Fixed(
            HashMap::from([
                ("./client".to_string(), PathBuf::from("/repo/src/client.ts")),
                (
                    "./settings".to_string(),
                    PathBuf::from("/repo/src/settings.ts"),
                ),
            ]),
            HashMap::from([
                (
                    PathBuf::from("/repo/src/client.ts"),
                    Rc::new(client_index()),
                ),
                (
                    PathBuf::from("/repo/src/settings.ts"),
                    Rc::new(settings_index),
                ),
            ]),
        );
        let facts = importer_facts(Path::new("page.tsx"), &scan, &resolver);
        (scan, facts)
    }

    #[test]
    fn a_member_call_is_handed_the_member_its_helper_and_the_bindings_they_read() {
        let consumer = r#"import { shelvesApi } from "./client";
export async function load(id: string) {
  await shelvesApi.rename(id, "new");
}
"#;
        let (scan, facts) = facts_for(consumer);
        assert_eq!(scan.sites.len(), 1);
        assert_eq!(
            facts.sites.len(),
            1,
            "the member reaches a request through its helper"
        );
        let material = facts.material.join("\n");
        assert!(material.contains("--- module: src/client.ts ---"));
        assert!(material.contains(
            "called by this file as shelvesApi.rename; declared in: export const shelvesApi = {"
        ));
        assert!(
            material.contains("`/shelves/${shelfId}/label`"),
            "{material}"
        );
        assert!(material.contains("helper a declaration in this section calls: sendJson"));
        assert!(material.contains("${settings.gatewayUrl}${PREFIX}${path}"));
        assert!(material.contains("binding a declaration in this section reads: PREFIX"));
        assert!(material.contains("import { settings } from \"./settings\";"));
        assert!(
            material.contains("gatewayUrl: process.env.SHELF_GATEWAY"),
            "the imported binding's own declaration, one hop: {material}"
        );
        // Only what the site reaches: not the sibling members.
        assert!(!material.contains("\"/shelves\")"), "{material}");
        assert!(facts.via.is_empty(), "a member call names no bare function");
        // `sendJson` builds its URL in a const opening with the settings read,
        // and `res.json()` beside it states no URL, so the base is settled.
        let (module, base) = facts.bases.values().next().expect("the site's base");
        assert_eq!(module, &PathBuf::from("/repo/src/client.ts"));
        assert_eq!(base, "settings.gatewayUrl");
    }

    #[test]
    fn a_member_that_reaches_no_request_is_not_offered() {
        let consumer = r#"import { shelvesApi } from "./client";
const label = shelvesApi.describe("a");
"#;
        let (_, facts) = facts_for(consumer);
        assert!(facts.sites.is_empty());
        assert!(facts.material.is_empty());
    }

    #[test]
    fn a_bare_helper_call_is_offered_with_its_declaring_name() {
        let consumer = r#"import { sendJson as send } from "./client";
send.call(null, "DELETE", "/shelves/1");
"#;
        let (scan, facts) = facts_for(consumer);
        let site = scan.sites[0].span.0;
        assert!(facts.sites.contains(&site));
        assert_eq!(facts.via.get(&site).map(String::as_str), Some("sendJson"));
    }

    #[test]
    fn a_shadowed_local_is_not_a_site() {
        let consumer = r#"import { shelvesApi } from "./client";
function local(shelvesApi: { list(): void }) {
  shelvesApi.list();
}
"#;
        let (scan, _) = facts_for(consumer);
        assert!(scan.sites.is_empty(), "{:?}", scan.sites);
    }

    #[test]
    fn an_instance_member_reaches_the_class_method() {
        let module = r#"class Gateway {
  constructor(private readonly base: string) {}
  async archive(id: string) {
    return fetch(`${this.base}/archive/${id}`, { method: "POST" });
  }
}
export const gateway = new Gateway(`${process.env.VAULT_URL}/v4`);
"#;
        let spans = spans_of(module, &["fetch("]);
        let index =
            ModuleIndex::build(Path::new("g.ts"), "src/g.ts", module, unbased(spans)).unwrap();
        let scan = scan_importer(
            Path::new("p.ts"),
            "import { gateway } from \"./g\";\ngateway.archive(\"x\");\n",
        )
        .unwrap();
        let resolver = Fixed(
            HashMap::from([("./g".to_string(), PathBuf::from("/g.ts"))]),
            HashMap::from([(PathBuf::from("/g.ts"), Rc::new(index))]),
        );
        let facts = importer_facts(Path::new("p.ts"), &scan, &resolver);
        assert_eq!(facts.sites.len(), 1);
        assert!(facts.material[0].contains("`${this.base}/archive/${id}`"));
        assert!(
            facts.material[0]
                .contains("export const gateway = new Gateway(`${process.env.VAULT_URL}/v4`);"),
            "{}",
            facts.material[0]
        );
        assert!(facts.material[0].contains("constructor(private readonly base: string) {}"));
    }

    #[test]
    fn a_member_call_on_an_exported_value_is_handed_the_value_and_the_binding_it_reads() {
        let module = r#"import axios from "axios";

export const labBaseUrl = process.env.PUBLIC_LAB_URL || "http://localhost:4600";

const lab = axios.create({
  baseURL: labBaseUrl,
  timeout: 8000,
});

export const unrelated = Object.freeze(["a"]);

export default lab;
"#;
        let spans = spans_of(module, &["axios.create("]);
        let index =
            ModuleIndex::build(Path::new("lab.ts"), "src/lab.ts", module, unbased(spans)).unwrap();
        let scan = scan_importer(
            Path::new("q.ts"),
            "import lab from \"./lab\";\nimport { unrelated } from \"./lab\";\nlab.get(\"/samples\");\nlab.post(\"/samples\");\nlab.get(\"/panels\");\nunrelated.includes(\"a\");\n",
        )
        .unwrap();
        let resolver = Fixed(
            HashMap::from([("./lab".to_string(), PathBuf::from("/lab.ts"))]),
            HashMap::from([(PathBuf::from("/lab.ts"), Rc::new(index))]),
        );
        let facts = importer_facts(Path::new("q.ts"), &scan, &resolver);
        assert_eq!(
            facts.sites.len(),
            3,
            "only the value whose declaration issues a request: {:?}",
            scan.sites
        );
        let material = facts.material.join("\n");
        assert!(
            material.contains("called by this file as lab.get, lab.post\n"),
            "{material}"
        );
        assert!(
            material.contains("const lab = axios.create({"),
            "{material}"
        );
        assert!(
            material.contains("binding a declaration in this section reads: labBaseUrl"),
            "{material}"
        );
        assert!(
            material.contains("process.env.PUBLIC_LAB_URL"),
            "{material}"
        );
        assert!(!material.contains("Object.freeze"), "{material}");
        assert!(
            facts.bases.is_empty(),
            "a client built from an options object states no URL base: {:?}",
            facts.bases
        );
    }

    #[test]
    fn a_receiver_typed_as_an_imported_class_reaches_its_method_through_siblings() {
        let module = r#"export class Registry {
  private readonly endpoint: string;
  constructor(root: string) {
    this.endpoint = `${root}/rpc/registry`;
  }
  private async post(body: object) {
    return fetch(this.endpoint, { method: "POST", body: JSON.stringify(body) });
  }
  private async load() {
    return this.post({ action: "list" });
  }
  async find(name: string) {
    const all = await this.load();
    return all;
  }
}
"#;
        let spans = spans_of(module, &["fetch("]);
        let index =
            ModuleIndex::build(Path::new("r.ts"), "src/r.ts", module, unbased(spans)).unwrap();
        let scan = scan_importer(
            Path::new("p.ts"),
            "import type { Registry } from \"./r\";\nexport async function f(registry: Registry) {\n  return registry.find(\"x\");\n}\n",
        )
        .unwrap();
        assert_eq!(scan.sites.len(), 1, "{:?}", scan.sites);
        assert_eq!(scan.sites[0].typed_receiver.as_deref(), Some("registry"));
        let resolver = Fixed(
            HashMap::from([("./r".to_string(), PathBuf::from("/r.ts"))]),
            HashMap::from([(PathBuf::from("/r.ts"), Rc::new(index))]),
        );
        let facts = importer_facts(Path::new("p.ts"), &scan, &resolver);
        assert_eq!(
            facts.sites.len(),
            1,
            "find reaches the request two siblings down"
        );
        let material = &facts.material[0];
        assert!(
            material.contains("called by this file as registry.find"),
            "{material}"
        );
        assert!(material.contains("helper a declaration in this section calls: this.load"));
        assert!(material.contains("helper a declaration in this section calls: this.post"));
        assert!(material.contains("the constructor that instance runs"));
        assert!(material.contains("`${root}/rpc/registry`"));
    }

    #[test]
    fn leading_base_reads_only_a_plain_binding() {
        assert_eq!(
            leading_base("${env.API_URL}/v1/x").as_deref(),
            Some("env.API_URL")
        );
        assert_eq!(
            leading_base("${import.meta.env.VITE_URL}/x").as_deref(),
            Some("import.meta.env.VITE_URL")
        );
        assert_eq!(leading_base("${this.baseUrl}/x"), None);
        assert_eq!(leading_base("${cfg()}/x"), None);
        assert_eq!(leading_base("/v1/x"), None);
    }

    #[test]
    fn evidence_backs_joined_segments_and_rejects_invented_ones() {
        let evidence = TargetEvidence::new([
            "/catalog/v3",
            "/shelves/",
            "/label",
            "https://edge.example.test",
        ]);
        assert_eq!(
            evidence.unbacked_part("${settings.gatewayUrl}/catalog/v3/shelves/:shelfId/label"),
            None
        );
        assert_eq!(
            evidence.unbacked_part("/catalog/v3/shelves/${id}/label?x=${y}"),
            None
        );
        assert_eq!(
            evidence.unbacked_part("/catalog/v3/shelf/:id"),
            Some("shelf".to_string())
        );
        assert_eq!(
            evidence.unbacked_part("https://edge.example.test/shelves"),
            None
        );
        assert_eq!(
            evidence.unbacked_part("https://api.example.com/graphql"),
            Some("api.example.com".to_string())
        );
        assert_eq!(evidence.unbacked_part("${base}${path}"), None);
        assert_eq!(evidence.unbacked_part("/shelves/v${n}"), None);
    }

    #[test]
    fn file_literals_reads_strings_and_template_quasis() {
        let literals = TargetEvidence::file_literals(
            Path::new("a.ts"),
            "const a = `/x/${id}/y`; fetch('/z');",
        );
        assert!(literals.contains(&"/x/".to_string()));
        assert!(literals.contains(&"/y".to_string()));
        assert!(literals.contains(&"/z".to_string()));
    }
}
