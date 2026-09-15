//! Call sites that reach an endpoint through a request wrapper declared in the
//! SAME file, with the URL passed in as an argument (carrick#588).
//!
//! A module that talks to many endpoints usually funnels them through one small
//! helper it declares itself:
//!
//! ```ignore
//! async function requestJson(base: string, path: string, token: string) {
//!   return fetch(`${base}${path}`, { headers: { Authorization: token } });
//! }
//! // …
//! const all = await requestJson(base, "/api/v1/widgets", token);
//! const one = await requestJson(base, `/api/v1/widgets/${id}`, token);
//! ```
//!
//! Neither half of that is extractable on its own. The request lives in the
//! helper, where the URL is a parameter that resolves to nothing; the endpoint
//! lives at the call site, which raises no candidate at all — its callee is a
//! local identifier rather than a client binding, and its path is not the first
//! argument. Every endpoint reached this way is invisible: not a wrong row, no
//! row.
//!
//! #369/#370 resolve exactly this indirection ACROSS files, by injecting the
//! imported wrapper's source into the analyzing prompt so the model can join the
//! site's argument onto the wrapper's base. The same-file variant needs no
//! injected context and no model judgment: the wrapper, its parameters and the
//! site's argument are all in one AST, so the join is read off it here and
//! merged into the file's extraction afterwards, the way route descriptors
//! (#234), pub/sub anchors (carrick#387) and verb-named request specs (#529)
//! already are.
//!
//! Structural throughout. What makes a function a wrapper is that its own
//! request call interpolates one of its own parameters into the URL — no client
//! library, framework or helper name appears anywhere, and a helper that builds
//! its whole URL internally is left to the existing path.
//!
//! ## Chains and bindings (carrick#1151)
//!
//! Real clients stack helpers, and hold the URL in a local binding on the way
//! down:
//!
//! ```ignore
//! function send(url: string, options: RequestInit) {
//!   return fetch(url, { ...options, headers: traceHeaders() });
//! }
//! export function callApi(method: string, endpoint: string, data?: object) {
//!   const url = `${config.API_URL}${endpoint}`;
//!   const options = { method, body: JSON.stringify(data) };
//!   return send(url, options);
//! }
//! callApi("POST", "/v1/orders", order);
//! ```
//!
//! Three structural reads make that one request:
//!
//! - **A `const` is its initializer.** Every `const` binding in the module is
//!   read by its resolver identity (name and syntax context), so a URL or an
//!   options object held in one substitutes exactly where it is referenced,
//!   through any number of `const` hops. `let`/`var` can be reassigned and are
//!   not read.
//! - **A method can arrive in an options object.** A request that spreads a
//!   parameter into its options bag (`{ ...options, headers }`) and states no
//!   `method` of its own takes the method from whatever the caller passes in
//!   that position.
//! - **A wrapper can call a wrapper.** A function whose request is a call to
//!   another same-file wrapper takes that wrapper's shape, with each parameter
//!   slot filled by what this function passes. Resolved to a fixpoint, so the
//!   depth of the chain does not matter.
//!
//! What stays unresolved is anything a `const` cannot state: a path picked by a
//! `switch`, read from a lookup table, or built by a call. Those sites raise no
//! row here, and are the model's to read.
//!
//! Scope: a wrapper is a named function declaration or a function/arrow bound to
//! a name, invoked by that name. A class method reached through a receiver
//! (`this.request("/things")`) is the same indirection through a different
//! binding shape and is not resolved here; it needs receiver resolution the way
//! the controller pass does, and is left for a follow-up rather than guessed at.

use std::collections::HashMap;

use swc_common::{SourceMap, SourceMapper, Spanned, SyntaxContext, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use crate::type_manifest::{is_http_method, normalize_manifest_method};
use crate::wrapper_request_shape::{is_request_options, literal_string, verb_from_callee_property};

/// One outbound call resolved through a request wrapper declared in the same
/// file: the site's own span and line, and the request it actually issues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWrapperCall {
    /// Start byte offset of the call expression at the SITE (not the wrapper).
    pub span_start: u32,
    /// End byte offset of the call expression at the site.
    pub span_end: u32,
    /// 1-based line of the site's call expression.
    pub line_number: usize,
    /// The wrapper the site delegates to.
    pub wrapper_name: String,
    /// The URL the wrapper builds once this site's argument is substituted in.
    /// Everything the wrapper closes over is kept verbatim (`${base}/things`),
    /// so env-var and base-URL classification downstream sees the form it
    /// always sees.
    pub target: String,
    /// Upper-case HTTP method when the wrapper (or the site) states one.
    /// `None` when the request states no method at all — downstream
    /// normalization applies its own default rather than this inventing one.
    pub method: Option<String>,
}

/// How many `const` hops a binding is followed through before giving up. A
/// real chain is two or three deep; the bound only keeps a cycle the resolver
/// did not break from looping.
const MAX_CONST_HOPS: usize = 8;

/// An object literal with more properties than this is a table, not a request
/// options bag or a URL, and is not kept in the `const` index.
const MAX_INDEXED_OBJECT_PROPS: usize = 64;

/// A binding's resolver identity: its name and syntax context.
type BindingKey = (String, SyntaxContext);

/// A part of the URL a wrapper builds.
#[derive(Debug, Clone, PartialEq, Eq)]
enum UrlPart {
    /// Text that is identical at every site: a template quasi, or an
    /// interpolation of something the wrapper closes over, kept verbatim.
    Fixed(String),
    /// The wrapper parameter at this position — the site's argument goes here.
    Param(usize),
}

/// Where the request's method comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MethodSource {
    /// Stated as a literal by the wrapper itself.
    Fixed(String),
    /// The wrapper parameterizes it, so the SITE's argument is the method.
    Param(usize),
    /// The `method` of the options object the SITE passes at this position
    /// (carrick#1151): the wrapper spreads that parameter into its own options
    /// and states no method over it.
    OptionsParam(usize),
    /// The request states no method. Not an assertion of GET — the value is
    /// left unset and the existing consumer normalization decides.
    Unstated,
}

/// The request a wrapper issues, in terms of its own parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shape {
    url: Vec<UrlPart>,
    method: MethodSource,
}

/// One argument a function passes to a call, read in that function's terms:
/// as URL text, as a method, and as an options object. Whichever reading the
/// callee's shape asks for is the one used.
#[derive(Debug, Clone)]
struct ArgForm {
    url: Option<Vec<UrlPart>>,
    method: Option<MethodSource>,
    options_method: Option<MethodSource>,
}

/// One call inside a named function that may be the request it issues.
#[derive(Debug, Clone)]
struct RequestUse {
    /// The call's own request shape, when it is request-shaped where it stands
    /// (an HTTP-verb callee or a request-options bag) and its URL carries a
    /// parameter.
    direct: Option<Shape>,
    /// The call's callee and arguments, when the callee is a plain identifier:
    /// a call to another same-file wrapper takes that wrapper's shape.
    delegate: Option<(BindingKey, Vec<ArgForm>)>,
}

/// One named function and the calls it makes.
#[derive(Debug, Clone)]
struct FnRecord {
    name: String,
    key: BindingKey,
    uses: Vec<RequestUse>,
}

/// Every same-file wrapper call site in `module`, in source order.
///
/// `source_map` must be the one the module was parsed with: interpolations the
/// wrapper closes over are carried through by their source text.
pub fn collect_local_wrapper_calls(
    module: &Module,
    source_map: &Lrc<SourceMap>,
) -> Vec<LocalWrapperCall> {
    let consts = collect_consts(module);
    let mut functions = FnCollector {
        reader: Reader {
            source_map,
            consts: &consts,
        },
        stack: Vec::new(),
        records: Vec::new(),
    };
    module.visit_with(&mut functions);
    let wrappers = resolve_wrappers(functions.records);
    if wrappers.is_empty() {
        return Vec::new();
    }

    let mut sites = SiteCollector {
        reader: Reader {
            source_map,
            consts: &consts,
        },
        wrappers,
        calls: Vec::new(),
    };
    module.visit_with(&mut sites);

    let mut calls = sites.calls;
    // Emit in source order so a scan of the same file always produces the same
    // rows.
    calls.sort_by_key(|call| (call.span_start, call.span_end));
    calls.dedup_by_key(|call| (call.span_start, call.span_end));
    calls
}

/// Every `const` binding in the module whose initializer could state a URL, a
/// method or an options object, by resolver identity. A key declared twice
/// (possible only where the resolver has not run) states nothing and is
/// dropped.
fn collect_consts(module: &Module) -> HashMap<BindingKey, Expr> {
    #[derive(Default)]
    struct ConstCollector {
        consts: HashMap<BindingKey, Option<Expr>>,
    }
    impl Visit for ConstCollector {
        fn visit_var_decl(&mut self, node: &VarDecl) {
            if node.kind == VarDeclKind::Const {
                for decl in &node.decls {
                    let (Pat::Ident(binding), Some(init)) = (&decl.name, decl.init.as_deref())
                    else {
                        continue;
                    };
                    if !indexable_initializer(init) {
                        continue;
                    }
                    let key = (binding.id.sym.to_string(), binding.id.ctxt);
                    self.consts
                        .entry(key)
                        .and_modify(|held| *held = None)
                        .or_insert_with(|| Some(init.clone()));
                }
            }
            node.visit_children_with(self);
        }
    }

    let mut collector = ConstCollector::default();
    module.visit_with(&mut collector);
    collector
        .consts
        .into_iter()
        .filter_map(|(key, init)| Some((key, init?)))
        .collect()
}

/// Whether a `const` initializer is one of the shapes this module reads: a
/// string, a template, a concatenation, another binding, or a small object.
fn indexable_initializer(expr: &Expr) -> bool {
    match unwrap(expr) {
        Expr::Lit(Lit::Str(_)) | Expr::Tpl(_) | Expr::Ident(_) | Expr::Bin(_) => true,
        Expr::Object(obj) => obj.props.len() <= MAX_INDEXED_OBJECT_PROPS,
        _ => false,
    }
}

/// Strip wrappers that do not change a value: parentheses and type assertions.
fn unwrap(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(inner) => unwrap(&inner.expr),
        Expr::TsAs(inner) => unwrap(&inner.expr),
        Expr::TsNonNull(inner) => unwrap(&inner.expr),
        Expr::TsConstAssertion(inner) => unwrap(&inner.expr),
        Expr::TsSatisfies(inner) => unwrap(&inner.expr),
        Expr::TsTypeAssertion(inner) => unwrap(&inner.expr),
        _ => expr,
    }
}

/// A piece of text an expression evaluates to, as this module reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    /// Text. `literal` is true when the source writes it as literal text (a
    /// string or a template quasi, directly or through a `const`), false for an
    /// interpolation carried through verbatim.
    Text { text: String, literal: bool },
    /// The enclosing function's parameter at this position.
    Param(usize),
}

/// Reads expressions against the module's `const` index.
struct Reader<'a> {
    source_map: &'a Lrc<SourceMap>,
    consts: &'a HashMap<BindingKey, Expr>,
}

impl Reader<'_> {
    /// The `const` initializer `ident` names, when it names one.
    fn const_init(&self, ident: &Ident) -> Option<&Expr> {
        self.consts.get(&(ident.sym.to_string(), ident.ctxt))
    }

    /// The pieces of text `expr` evaluates to, with `params` read as
    /// parameter slots. `None` when the expression is not a string this can
    /// state (a call, a member access at the top level, an unknown binding).
    fn pieces(
        &self,
        expr: &Expr,
        params: &[Option<BindingKey>],
        hops: usize,
    ) -> Option<Vec<Piece>> {
        match unwrap(expr) {
            Expr::Lit(Lit::Str(literal)) => Some(vec![Piece::Text {
                text: literal.value.to_string(),
                literal: !literal.value.is_empty(),
            }]),
            Expr::Ident(ident) => {
                if let Some(index) = param_position(ident, params) {
                    return Some(vec![Piece::Param(index)]);
                }
                if hops >= MAX_CONST_HOPS {
                    return None;
                }
                self.pieces(self.const_init(ident)?, params, hops + 1)
            }
            Expr::Tpl(tpl) => {
                let mut pieces = Vec::new();
                for (index, quasi) in tpl.quasis.iter().enumerate() {
                    let text = quasi.raw.to_string();
                    if !text.is_empty() {
                        pieces.push(Piece::Text {
                            text,
                            literal: true,
                        });
                    }
                    let Some(interpolated) = tpl.exprs.get(index) else {
                        continue;
                    };
                    pieces.extend(self.interpolation(interpolated, params, hops)?);
                }
                Some(pieces)
            }
            // `base + path`: each side is read the way a template reads one
            // interpolation, so an opaque side is carried verbatim.
            Expr::Bin(bin) if bin.op == BinaryOp::Add => {
                let mut pieces = self.interpolation(&bin.left, params, hops)?;
                pieces.extend(self.interpolation(&bin.right, params, hops)?);
                Some(pieces)
            }
            // `cond ? "/v1/things?x=1" : "/v1/things"`: two spellings of one
            // route. Read when both branches state the same text up to the
            // query string, as that text; any other conditional states two
            // requests and is not read.
            Expr::Cond(cond) => {
                let consequent = self.pieces(&cond.cons, params, hops)?;
                let alternate = self.pieces(&cond.alt, params, hops)?;
                let route = |pieces: &[Piece]| -> Vec<Piece> {
                    let mut route = Vec::new();
                    for piece in pieces {
                        match piece {
                            Piece::Text { text, literal } if *literal => match text.find('?') {
                                Some(query) => {
                                    if query > 0 {
                                        route.push(Piece::Text {
                                            text: text[..query].to_string(),
                                            literal: true,
                                        });
                                    }
                                    return route;
                                }
                                None => route.push(piece.clone()),
                            },
                            other => route.push(other.clone()),
                        }
                    }
                    route
                };
                let consequent_route = route(&consequent);
                (merge_text(&consequent_route) == merge_text(&route(&alternate)))
                    .then_some(consequent_route)
            }
            _ => None,
        }
    }

    /// One interpolated value: its pieces when it resolves, else its own source
    /// text carried verbatim as `${…}`.
    fn interpolation(
        &self,
        expr: &Expr,
        params: &[Option<BindingKey>],
        hops: usize,
    ) -> Option<Vec<Piece>> {
        if let Expr::Ident(_) | Expr::Lit(Lit::Str(_)) | Expr::Tpl(_) = unwrap(expr)
            && let Some(pieces) = self.pieces(expr, params, hops)
        {
            return Some(pieces);
        }
        let snippet = self.source_map.span_to_snippet(expr.span()).ok()?;
        Some(vec![Piece::Text {
            text: format!("${{{}}}", snippet.trim()),
            literal: false,
        }])
    }

    /// `expr` read as an HTTP method: a literal verb, a parameter, or a
    /// `const` holding either.
    fn method(
        &self,
        expr: &Expr,
        params: &[Option<BindingKey>],
        hops: usize,
    ) -> Option<MethodSource> {
        if let Some(literal) = literal_string(unwrap(expr)) {
            let normalized = normalize_manifest_method(&literal);
            return is_http_method(&normalized).then_some(MethodSource::Fixed(normalized));
        }
        let Expr::Ident(ident) = unwrap(expr) else {
            return None;
        };
        if let Some(index) = param_position(ident, params) {
            return Some(MethodSource::Param(index));
        }
        if hops >= MAX_CONST_HOPS {
            return None;
        }
        self.method(self.const_init(ident)?, params, hops + 1)
    }

    /// `expr` read as a request options object, for the method it carries: an
    /// object literal, a parameter holding one, or a `const` holding one.
    fn options_method(
        &self,
        expr: &Expr,
        params: &[Option<BindingKey>],
        hops: usize,
    ) -> Option<MethodSource> {
        match unwrap(expr) {
            Expr::Object(obj) => self.object_method(obj, params, hops),
            Expr::Ident(ident) => {
                if let Some(index) = param_position(ident, params) {
                    return Some(MethodSource::OptionsParam(index));
                }
                if hops >= MAX_CONST_HOPS {
                    return None;
                }
                self.options_method(self.const_init(ident)?, params, hops + 1)
            }
            _ => None,
        }
    }

    /// The method an options object literal states, read in property order so
    /// a `method` written after a spread overrides it and a spread written
    /// after a `method` may replace it. `None` when a property that could
    /// carry the method cannot be read.
    fn object_method(
        &self,
        obj: &ObjectLit,
        params: &[Option<BindingKey>],
        hops: usize,
    ) -> Option<MethodSource> {
        let mut method = MethodSource::Unstated;
        for prop in &obj.props {
            match prop {
                // A spread this can read replaces the method; one it cannot
                // read is left out, as a request states no method through it
                // that this could name.
                PropOrSpread::Spread(spread) => {
                    if let Some(spread_method) = self.options_method(&spread.expr, params, hops) {
                        method = spread_method;
                    }
                }
                PropOrSpread::Prop(prop) => match &**prop {
                    Prop::KeyValue(kv) if prop_name_is(&kv.key, "method") => {
                        method = self.method(&kv.value, params, hops)?;
                    }
                    Prop::Shorthand(ident) if ident.sym.as_ref() == "method" => {
                        method = self.method(&Expr::Ident(ident.clone()), params, hops)?;
                    }
                    _ => {}
                },
            }
        }
        Some(method)
    }

    /// The request-options bag among a call's arguments, read through `const`
    /// bindings: the one argument that is (or names) an object literal
    /// carrying a request-options key.
    fn options_bag<'e>(&'e self, call: &'e CallExpr) -> Option<&'e ObjectLit> {
        let mut found: Option<&ObjectLit> = None;
        for arg in &call.args {
            if arg.spread.is_some() {
                continue;
            }
            let mut expr = unwrap(&arg.expr);
            for _ in 0..MAX_CONST_HOPS {
                let Expr::Ident(ident) = expr else {
                    break;
                };
                match self.const_init(ident) {
                    Some(init) => expr = unwrap(init),
                    None => break,
                }
            }
            if let Expr::Object(obj) = expr
                && is_request_options(obj)
            {
                if found.is_some() {
                    return None;
                }
                found = Some(obj);
            }
        }
        found
    }

    /// The request `call` issues where it stands, when it is request-shaped
    /// and its URL carries one of `params`.
    fn direct_shape(&self, call: &CallExpr, params: &[Option<BindingKey>]) -> Option<Shape> {
        let verb = verb_from_callee_property(callee_property(call).as_deref());
        let options = self.options_bag(call);
        // Not request-shaped: no HTTP-verb callee and no request-options bag.
        // The same structural test the cross-file wrapper pass uses.
        if verb.is_none() && options.is_none() {
            return None;
        }

        let url_arg = call.args.first().filter(|arg| arg.spread.is_none())?;
        let url = url_parts(self.pieces(&url_arg.expr, params, 0)?);
        if !url.iter().any(|part| matches!(part, UrlPart::Param(_))) {
            return None;
        }

        let method = match options.map(|obj| self.object_method(obj, params, 0)) {
            Some(Some(MethodSource::Unstated)) | None => match verb {
                Some(verb) => MethodSource::Fixed(verb),
                None => MethodSource::Unstated,
            },
            Some(Some(method)) => method,
            Some(None) => return None,
        };
        Some(Shape { url, method })
    }

    /// One argument read in its function's terms.
    fn arg_form(&self, expr: &Expr, params: &[Option<BindingKey>]) -> ArgForm {
        ArgForm {
            url: self.pieces(expr, params, 0).map(url_parts),
            method: self.method(expr, params, 0),
            options_method: self.options_method(expr, params, 0),
        }
    }
}

/// Whether a property key names `name`.
fn prop_name_is(key: &PropName, name: &str) -> bool {
    match key {
        PropName::Ident(ident) => ident.sym.as_ref() == name,
        PropName::Str(literal) => literal.value.as_ref() == name,
        _ => false,
    }
}

/// The position of the parameter `ident` refers to, matched by resolver
/// identity so a nested closure's own parameter of the same name is not it.
fn param_position(ident: &Ident, params: &[Option<BindingKey>]) -> Option<usize> {
    params.iter().position(|param| {
        param
            .as_ref()
            .is_some_and(|(name, ctxt)| name == ident.sym.as_ref() && *ctxt == ident.ctxt)
    })
}

/// Pieces as a wrapper's URL parts, adjacent text merged so two spellings of
/// one URL compare equal.
fn url_parts(pieces: Vec<Piece>) -> Vec<UrlPart> {
    let mut parts: Vec<UrlPart> = Vec::new();
    for piece in pieces {
        match piece {
            Piece::Text { text, .. } => match parts.last_mut() {
                Some(UrlPart::Fixed(held)) => held.push_str(&text),
                _ => parts.push(UrlPart::Fixed(text)),
            },
            Piece::Param(index) => parts.push(UrlPart::Param(index)),
        }
    }
    parts
}

/// Pieces as plain text, for comparing two spellings of one route.
fn merge_text(pieces: &[Piece]) -> Vec<UrlPart> {
    url_parts(pieces.to_vec())
}

/// Collects every named function and the calls it makes.
struct FnCollector<'a> {
    reader: Reader<'a>,
    /// The named functions enclosing the node being visited, innermost last,
    /// as (record, parameters). A call is attributed to the innermost one, so
    /// a helper's own nested closures count as the helper's and a nested named
    /// function owns its own.
    stack: Vec<(FnRecord, Vec<Option<BindingKey>>)>,
    records: Vec<FnRecord>,
}

impl FnCollector<'_> {
    fn push_frame(&mut self, ident: &Ident, params: Vec<Option<BindingKey>>) {
        self.stack.push((
            FnRecord {
                name: ident.sym.to_string(),
                key: (ident.sym.to_string(), ident.ctxt),
                uses: Vec::new(),
            },
            params,
        ));
    }

    fn pop_frame(&mut self) {
        if let Some((record, _)) = self.stack.pop()
            && !record.uses.is_empty()
        {
            self.records.push(record);
        }
    }
}

impl Visit for FnCollector<'_> {
    fn visit_fn_decl(&mut self, node: &FnDecl) {
        let params = node
            .function
            .params
            .iter()
            .map(|param| pat_key(&param.pat))
            .collect();
        self.push_frame(&node.ident, params);
        node.visit_children_with(self);
        self.pop_frame();
    }

    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        let framed = match (&node.name, node.init.as_deref().map(unwrap)) {
            (Pat::Ident(ident), Some(Expr::Arrow(arrow))) => {
                self.push_frame(&ident.id, arrow.params.iter().map(pat_key).collect());
                true
            }
            (Pat::Ident(ident), Some(Expr::Fn(fn_expr))) => {
                let params = fn_expr
                    .function
                    .params
                    .iter()
                    .map(|param| pat_key(&param.pat))
                    .collect();
                self.push_frame(&ident.id, params);
                true
            }
            _ => false,
        };
        node.visit_children_with(self);
        if framed {
            self.pop_frame();
        }
    }

    fn visit_call_expr(&mut self, node: &CallExpr) {
        if let Some((_, params)) = self.stack.last()
            && params.iter().any(Option::is_some)
        {
            let direct = self.reader.direct_shape(node, params);
            let delegate = match &node.callee {
                Callee::Expr(callee) => match unwrap(callee) {
                    Expr::Ident(ident) => Some((
                        (ident.sym.to_string(), ident.ctxt),
                        node.args
                            .iter()
                            .map(|arg| match arg.spread {
                                Some(_) => ArgForm {
                                    url: None,
                                    method: None,
                                    options_method: None,
                                },
                                None => self.reader.arg_form(&arg.expr, params),
                            })
                            .collect(),
                    )),
                    _ => None,
                },
                _ => None,
            };
            if (direct.is_some() || delegate.is_some())
                && let Some((record, _)) = self.stack.last_mut()
            {
                record.uses.push(RequestUse { direct, delegate });
            }
        }
        node.visit_children_with(self);
    }
}

/// The wrappers the file declares, resolved to a fixpoint: a function whose
/// requests all come to ONE shape carrying one of its parameters is a
/// wrapper, and a call to a wrapper is a request of that wrapper's shape.
///
/// Recomputed from scratch each round against the previous round's answer,
/// so the result does not depend on declaration order, and bounded by the
/// number of functions, which is the longest chain there can be.
fn resolve_wrappers(records: Vec<FnRecord>) -> HashMap<BindingKey, (String, Shape)> {
    // A key two records share (possible only where the resolver has not run)
    // names two functions; neither is readable by name.
    let mut counts: HashMap<&BindingKey, usize> = HashMap::new();
    for record in &records {
        *counts.entry(&record.key).or_default() += 1;
    }
    let records: Vec<&FnRecord> = records
        .iter()
        .filter(|record| counts.get(&record.key) == Some(&1))
        .collect();

    let mut resolved: HashMap<BindingKey, (String, Shape)> = HashMap::new();
    for _ in 0..=records.len() {
        let mut next: HashMap<BindingKey, (String, Shape)> = HashMap::new();
        for record in &records {
            let mut shapes: Vec<Shape> = Vec::new();
            for request in &record.uses {
                let delegated = request.delegate.as_ref().and_then(|(callee, args)| {
                    if *callee == record.key {
                        return None;
                    }
                    let (_, shape) = resolved.get(callee)?;
                    compose(shape, args)
                });
                let Some(shape) = delegated.or_else(|| request.direct.clone()) else {
                    continue;
                };
                if shape
                    .url
                    .iter()
                    .any(|part| matches!(part, UrlPart::Param(_)))
                    && !shapes.contains(&shape)
                {
                    shapes.push(shape);
                }
            }
            // Two different parameterized requests in one helper: a site could
            // be reaching either, so nothing about it is asserted. The same
            // request made twice (a retry) is one shape.
            if let [shape] = shapes.as_slice() {
                next.insert(record.key.clone(), (record.name.clone(), shape.clone()));
            }
        }
        if next == resolved {
            break;
        }
        resolved = next;
    }
    resolved
}

/// A wrapper's shape with each parameter slot filled by what the caller
/// passes, in the caller's own terms. `None` when a slot the shape reads is
/// filled by something that cannot be read.
fn compose(shape: &Shape, args: &[ArgForm]) -> Option<Shape> {
    let mut pieces = Vec::new();
    for part in &shape.url {
        match part {
            UrlPart::Fixed(text) => pieces.push(UrlPart::Fixed(text.clone())),
            UrlPart::Param(index) => pieces.extend(args.get(*index)?.url.clone()?),
        }
    }
    let mut url: Vec<UrlPart> = Vec::new();
    for part in pieces {
        match (url.last_mut(), part) {
            (Some(UrlPart::Fixed(held)), UrlPart::Fixed(text)) => held.push_str(&text),
            (_, part) => url.push(part),
        }
    }
    let method = match &shape.method {
        MethodSource::Fixed(method) => MethodSource::Fixed(method.clone()),
        MethodSource::Unstated => MethodSource::Unstated,
        MethodSource::Param(index) => args.get(*index)?.method.clone()?,
        // No options argument at all: the request states no method.
        MethodSource::OptionsParam(index) => match args.get(*index) {
            Some(arg) => arg.options_method.clone()?,
            None => MethodSource::Unstated,
        },
    };
    Some(Shape { url, method })
}

/// Finds the call sites that delegate to one of the resolved wrappers.
struct SiteCollector<'a> {
    reader: Reader<'a>,
    wrappers: HashMap<BindingKey, (String, Shape)>,
    calls: Vec<LocalWrapperCall>,
}

impl Visit for SiteCollector<'_> {
    fn visit_call_expr(&mut self, node: &CallExpr) {
        if let Callee::Expr(callee) = &node.callee
            && let Expr::Ident(ident) = unwrap(callee)
            && let Some((name, shape)) = self.wrappers.get(&(ident.sym.to_string(), ident.ctxt))
            && let Some(call) = resolve_site(&self.reader, name, shape, node)
        {
            self.calls.push(call);
        }
        node.visit_children_with(self);
    }
}

/// Substitute this site's arguments into the wrapper's URL and method.
fn resolve_site(
    reader: &Reader<'_>,
    wrapper_name: &str,
    shape: &Shape,
    call: &CallExpr,
) -> Option<LocalWrapperCall> {
    let mut target = String::new();
    // At least one parameter slot must be filled with literal text the site
    // states (directly or through a `const`). A slot filled only by variables
    // tells us nothing the wrapper's own line did not already say — which is
    // also what keeps one wrapper delegating to another from reading as a
    // site.
    let mut states_a_literal = false;
    for part in &shape.url {
        match part {
            UrlPart::Fixed(text) => target.push_str(text),
            UrlPart::Param(index) => {
                let arg = call.args.get(*index).filter(|arg| arg.spread.is_none())?;
                match reader.pieces(&arg.expr, &[], 0) {
                    Some(pieces) => {
                        for piece in pieces {
                            if let Piece::Text { text, literal } = piece {
                                states_a_literal |= literal;
                                target.push_str(&text);
                            }
                        }
                    }
                    // A slot the site fills with an expression — the base URL
                    // it holds in a variable, most often. Carried through as an
                    // interpolation of that expression, which is exactly what
                    // the wrapper's own request line reads as.
                    None => {
                        let snippet = reader.source_map.span_to_snippet(arg.expr.span()).ok()?;
                        target.push_str(&format!("${{{}}}", snippet.trim()));
                    }
                }
            }
        }
    }
    // Consumer-side route shape: an absolute path, a full URL, or a base the
    // wrapper interpolates in front of one, and a separator somewhere in it.
    // A helper that merely takes a string and passes an options bag
    // (`send(level, message, { headers })`) fails this and asserts nothing.
    if !states_a_literal || !target.contains('/') || !is_route_shaped(&target) {
        return None;
    }

    let method = match &shape.method {
        MethodSource::Fixed(method) => Some(method.clone()),
        MethodSource::Param(index) => {
            let arg = call.args.get(*index).filter(|arg| arg.spread.is_none())?;
            match reader.method(&arg.expr, &[], 0)? {
                MethodSource::Fixed(method) => Some(method),
                _ => return None,
            }
        }
        MethodSource::OptionsParam(index) => match call.args.get(*index) {
            None => None,
            Some(arg) if arg.spread.is_some() => return None,
            Some(arg) => match reader.options_method(&arg.expr, &[], 0)? {
                MethodSource::Fixed(method) => Some(method),
                MethodSource::Unstated => None,
                _ => return None,
            },
        },
        MethodSource::Unstated => None,
    };

    Some(LocalWrapperCall {
        span_start: call.span.lo.0,
        span_end: call.span.hi.0,
        line_number: reader.source_map.lookup_char_pos(call.span.lo).line,
        wrapper_name: wrapper_name.to_string(),
        target,
        method,
    })
}

/// Whether a resolved target is shaped like something a consumer requests: an
/// absolute path, a full URL, or an interpolated base with the rest behind it.
/// The consumer direction of the route-shape test the route-descriptor pass
/// applies — a bare token is a message name or a log level, never a target.
fn is_route_shaped(target: &str) -> bool {
    let trimmed = target.trim();
    trimmed.starts_with('/')
        || trimmed.starts_with("${")
        || trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
}

/// The property a call was made through (`post` in `client.post(…)`).
fn callee_property(call: &CallExpr) -> Option<String> {
    let Callee::Expr(callee) = &call.callee else {
        return None;
    };
    let Expr::Member(member) = &**callee else {
        return None;
    };
    match &member.prop {
        MemberProp::Ident(ident) => Some(ident.sym.to_string()),
        MemberProp::Computed(computed) => match &*computed.expr {
            Expr::Lit(Lit::Str(literal)) => Some(literal.value.to_string()),
            _ => None,
        },
        MemberProp::PrivateName(_) => None,
    }
}

/// The binding a parameter introduces, when it is a plain identifier.
/// Destructured and rest parameters bind no single name and hold a position no
/// argument can be read from.
fn pat_key(pat: &Pat) -> Option<BindingKey> {
    match pat {
        Pat::Ident(ident) => Some((ident.id.sym.to_string(), ident.id.ctxt)),
        // `path = "/default"`: the binding is the left side.
        Pat::Assign(assign) => pat_key(&assign.left),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Parse `content` the way the scanner does (resolver included, so
    /// shadowed bindings carry distinct syntax contexts) and collect its
    /// same-file wrapper call sites.
    fn collect(content: &str) -> Vec<LocalWrapperCall> {
        use swc_common::{FileName, GLOBALS, Globals, Mark};
        use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};
        use swc_ecma_transforms_base::resolver;
        use swc_ecma_visit::VisitMutWith;

        let source_map: Lrc<SourceMap> = Default::default();
        let source_file = source_map.new_source_file(
            Lrc::new(FileName::Real(PathBuf::from("client.ts"))),
            content.to_string(),
        );
        let lexer = Lexer::new(
            Syntax::Typescript(TsSyntax {
                decorators: true,
                ..Default::default()
            }),
            Default::default(),
            StringInput::from(&*source_file),
            None,
        );
        let mut module = Parser::new_from(lexer)
            .parse_module()
            .expect("fixture parses");
        GLOBALS.set(&Globals::new(), || {
            let unresolved = Mark::new();
            let top_level = Mark::new();
            module.visit_mut_with(&mut resolver(unresolved, top_level, true));
        });
        collect_local_wrapper_calls(&module, &source_map)
    }

    fn targets(calls: &[LocalWrapperCall]) -> Vec<(Option<&str>, &str)> {
        calls
            .iter()
            .map(|call| (call.method.as_deref(), call.target.as_str()))
            .collect()
    }

    /// The defect shape (carrick#588): the path is an ARGUMENT at the site and
    /// the request that uses it lives in a same-file helper, so the site raises
    /// no candidate and the helper's own URL resolves to nothing.
    #[test]
    fn resolves_path_argument_through_a_same_file_helper() {
        let calls = collect(
            r#"
async function requestJson(base: string, path: string, token: string) {
  const res = await fetch(`${base}${path}`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  return res.json();
}

export function buildTools(base: string, token: string) {
  return {
    listWidgets: () => requestJson(base, "/api/v1/widgets", token),
    getWidget: (id: string) => requestJson(base, `/api/v1/widgets/${id}`, token),
  };
}
"#,
        );

        assert_eq!(
            targets(&calls),
            vec![
                (None, "${base}/api/v1/widgets"),
                (None, "${base}/api/v1/widgets/${id}"),
            ],
            "both sites must resolve, keeping the helper's base verbatim"
        );
        assert!(
            calls.iter().all(|call| call.wrapper_name == "requestJson"),
            "sites must be attributed to the helper they delegate to"
        );
        assert!(
            calls[0].span_start < calls[1].span_start,
            "spans must be the SITE's, in source order"
        );
    }

    /// A site whose path argument carries a query string built from an
    /// expression resolves like any other: the query is not part of the route
    /// and is truncated downstream (carrick#588 finding 6).
    #[test]
    fn resolves_a_path_argument_carrying_a_built_query_string() {
        let calls = collect(
            r#"
async function requestJson(base: string, path: string, token: string) {
  const res = await fetch(`${base}${path}`, { headers: { Authorization: token } });
  return res.json();
}

export function buildTools(base: string, token: string) {
  return {
    listWidgets: (params: URLSearchParams) =>
      requestJson(base, `/api/v1/widgets?${params.toString()}`, token),
    widgetHistory: (id: string, at: string) =>
      requestJson(base, `/api/v1/widgets/${id}/history?since=${encodeURIComponent(at)}`, token),
  };
}
"#,
        );

        assert_eq!(
            targets(&calls),
            vec![
                (None, "${base}/api/v1/widgets?${params.toString()}"),
                (
                    None,
                    "${base}/api/v1/widgets/${id}/history?since=${encodeURIComponent(at)}"
                ),
            ],
            "the site's argument is kept verbatim, query string included"
        );
    }

    /// The whole URL is the parameter, and the helper states its method.
    #[test]
    fn resolves_a_bare_path_parameter_and_a_stated_method() {
        let calls = collect(
            r#"
function send(path, payload) {
  return client.post(path, payload, { headers: authHeaders() });
}
send("/v2/orders", order);
"#,
        );
        assert_eq!(targets(&calls), vec![(Some("POST"), "/v2/orders")]);
    }

    /// A helper that parameterizes its method takes it from the site, so a
    /// POST-only surface is never recorded as a GET.
    #[test]
    fn takes_a_parameterized_method_from_the_call_site() {
        let calls = collect(
            r#"
function call(method: string, path: string, body?: unknown) {
  return fetch(`${host}${path}`, { method, body: JSON.stringify(body) });
}
call("PUT", "/v1/settings", next);
call(verb, "/v1/ignored", next);
"#,
        );
        assert_eq!(
            targets(&calls),
            vec![(Some("PUT"), "${host}/v1/settings")],
            "the literal-verb site resolves; the variable-verb site asserts nothing"
        );
    }

    /// Nothing is asserted about helpers that are not requests, sites that pass
    /// a non-literal path, helpers whose URL is fully internal, or a name that
    /// a different binding shadows.
    #[test]
    fn stays_inert_outside_the_shape() {
        assert!(
            collect(
                r#"
function translate(key: string) { return dictionary[key]; }
translate("/some/key");
"#
            )
            .is_empty(),
            "a helper that issues no request is not a wrapper"
        );

        assert!(
            collect(
                r#"
function requestJson(base: string, path: string) {
  return fetch(`${base}${path}`, { headers: {} });
}
requestJson(base, buildPath(id));
"#
            )
            .is_empty(),
            "a computed path argument resolves to nothing and must not be guessed"
        );

        assert!(
            collect(
                r#"
function loadAll(token: string) {
  return fetch(`${host}/v1/all`, { headers: { Authorization: token } });
}
loadAll(token);
"#
            )
            .is_empty(),
            "a helper whose URL carries no parameter is already extractable where it stands"
        );

        assert!(
            collect(
                r#"
function requestJson(base: string, path: string) {
  return fetch(`${base}${path}`, { headers: {} });
}
function outer() {
  const requestJson = (a: string, b: string) => `${a}${b}`;
  requestJson(base, "/v1/not-a-call");
}
"#
            )
            .is_empty(),
            "a shadowing binding of the same name must not be read as the wrapper"
        );

        assert!(
            collect(
                r#"
function emit(level: string, message: string) {
  transport.send(`${level}: ${message}`, { headers: base });
}
emit("warn", "/tmp/file went missing");
"#
            )
            .is_empty(),
            "a helper whose joined string is not route-shaped asserts nothing"
        );

        assert!(
            collect(
                r#"
function twoWays(path: string, body: unknown) {
  if (body) return fetch(`${host}${path}`, { method: "POST", body });
  return fetch(`${host}${path}`, { method: "DELETE" });
}
twoWays("/v1/things", body);
"#
            )
            .is_empty(),
            "two parameterized requests in one helper leave the site ambiguous"
        );
    }

    /// carrick#1151: the URL held in a `const`, the method carried in an
    /// options object the wrapper spreads, and a wrapper that calls a second
    /// wrapper. Each alone left the site with no row.
    #[test]
    fn resolves_a_two_hop_chain_with_the_url_and_options_in_consts() {
        let calls = collect(
            r#"
function send(url: string, options: RequestInit, label: string) {
  return tracer.span(label, async () => {
    const response = await fetch(url, { ...options, headers: traceHeaders() });
    return response;
  });
}

export async function callApi(method: string, endpoint: string, data?: object) {
  const url = `${config.apiUrl}${endpoint}`;
  const init: RequestInit = {
    method,
    headers: { "Content-Type": "application/json" },
    body: data ? JSON.stringify(data) : undefined,
  };
  const response = await send(url, init, endpoint);
  return response.json();
}

export const ordersApi = {
  list: () => callApi("GET", "/v1/orders"),
  get: (orderId: string) => callApi("GET", `/v1/orders/${orderId}`),
  cancel: (orderId: string) => callApi("PATCH", `/v1/orders/${orderId}/cancel`, {}),
};
"#,
        );
        assert_eq!(
            targets(&calls),
            vec![
                (Some("GET"), "${config.apiUrl}/v1/orders"),
                (Some("GET"), "${config.apiUrl}/v1/orders/${orderId}"),
                (
                    Some("PATCH"),
                    "${config.apiUrl}/v1/orders/${orderId}/cancel"
                ),
            ],
            "one row per site, method from the site, base kept verbatim: {calls:#?}"
        );
        assert!(
            calls.iter().all(|call| call.wrapper_name == "callApi"),
            "each site is attributed to the wrapper it calls, not the one beneath it"
        );
    }

    /// A retry makes the same request twice. One shape, not an ambiguity.
    #[test]
    fn a_retried_request_through_a_second_hop_is_one_shape() {
        let calls = collect(
            r#"
function send(url: string, options: RequestInit) {
  return fetch(url, { ...options, headers: authHeaders() });
}

async function sendWithRetry(url: string, options: RequestInit) {
  const first = await send(url, options);
  if (first.status !== 401) return first;
  const retried = { ...options, headers: { Authorization: await refresh() } };
  return send(url, retried);
}

export function callAuthed(method: string, endpoint: string) {
  const url = `${base}${endpoint}`;
  const options = { method, credentials: "include" };
  return report(() => sendWithRetry(url, options), endpoint);
}

callAuthed("DELETE", "/v1/sessions/current");
"#,
        );
        assert_eq!(
            targets(&calls),
            vec![(Some("DELETE"), "${base}/v1/sessions/current")],
            "{calls:#?}"
        );
    }

    /// A site whose own `const`s state the URL and the options bag is a site,
    /// two `const` hops deep.
    #[test]
    fn a_site_that_holds_its_url_and_options_in_consts_resolves() {
        let calls = collect(
            r#"
function send(url: string, options: RequestInit) {
  return fetch(url, { ...options, headers: authHeaders() });
}

export async function download(fileId: string) {
  const endpoint = `/v1/files/${fileId}/content`;
  const url = `${base}${endpoint}`;
  const options: RequestInit = { method: "GET", credentials: "include" };
  const response = await send(url, options);
  return response.blob();
}
"#,
        );
        assert_eq!(
            targets(&calls),
            vec![(Some("GET"), "${base}/v1/files/${fileId}/content")],
            "{calls:#?}"
        );
    }

    /// Two spellings of one route in a conditional argument are that route.
    #[test]
    fn a_conditional_argument_naming_one_route_resolves_to_it() {
        let calls = collect(
            r#"
function call(method: string, path: string) {
  return fetch(`${host}${path}`, { method });
}
call("POST", q.length > 0 ? `/v1/items?${q}` : "/v1/items");
call("GET", admin ? "/v1/admin/items" : "/v1/items");
"#,
        );
        assert_eq!(
            targets(&calls),
            vec![(Some("POST"), "${host}/v1/items")],
            "a conditional choosing between two routes states neither: {calls:#?}"
        );
    }

    /// A wrapper's delegation to the wrapper beneath it passes variables, not
    /// a route, and must not itself read as a site.
    #[test]
    fn a_delegation_between_wrappers_is_not_a_site() {
        let calls = collect(
            r#"
function send(url: string, options: RequestInit) {
  return fetch(url, { ...options, headers: authHeaders() });
}
export function callApi(method: string, endpoint: string) {
  const url = `${config.apiUrl}${endpoint}`;
  return send(url, { method });
}
"#,
        );
        assert!(calls.is_empty(), "{calls:#?}");
    }

    /// A `let` can be reassigned before the request, so it is not read, and a
    /// method the site passes through a variable is not guessed.
    #[test]
    fn reassignable_bindings_and_variable_methods_are_not_read() {
        let calls = collect(
            r#"
function send(url: string, options: RequestInit) {
  return fetch(url, { ...options, headers: authHeaders() });
}
export function callApi(method: string, endpoint: string) {
  const url = `${base}${endpoint}`;
  return send(url, { method });
}
let path = "/v1/first";
path = "/v1/second";
callApi("GET", path);
callApi(verb, "/v1/things");
const options = { method: pickVerb() };
send("/v1/other", options);
"#,
        );
        assert!(calls.is_empty(), "{calls:#?}");
    }
}
