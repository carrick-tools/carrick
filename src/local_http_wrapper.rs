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
//! Scope: a wrapper is a named function declaration or a function/arrow bound to
//! a name, invoked by that name. A class method reached through a receiver
//! (`this.request("/things")`) is the same indirection through a different
//! binding shape and is not resolved here; it needs receiver resolution the way
//! the controller pass does, and is left for a follow-up rather than guessed at.

use swc_common::{SourceMap, SourceMapper, Spanned, SyntaxContext, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use crate::type_manifest::{is_http_method, normalize_manifest_method};
use crate::wrapper_request_shape::{
    literal_string, prop_value, request_options_argument, verb_from_callee_property,
};

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
    /// The request states no method. Not an assertion of GET — the value is
    /// left unset and the existing consumer normalization decides.
    Unstated,
}

/// A request wrapper declared in this file whose URL carries one of its own
/// parameters.
#[derive(Debug, Clone)]
struct WrapperDef {
    name: String,
    /// The binding's syntax context, so a shadowing declaration elsewhere in
    /// the file cannot be mistaken for a call to this one. Meaningful only
    /// after the resolver pass; where it has not run every context compares
    /// equal and this degrades to a name match.
    ctxt: SyntaxContext,
    url: Vec<UrlPart>,
    method: MethodSource,
}

/// One named function currently being walked.
struct FnFrame {
    name: String,
    ctxt: SyntaxContext,
    params: Vec<Option<String>>,
    resolved: Option<(Vec<UrlPart>, MethodSource)>,
    /// Two parameterized requests in one helper: a site could be reaching
    /// either, so nothing about it is asserted.
    ambiguous: bool,
}

/// Every same-file wrapper call site in `module`, in source order.
///
/// `source_map` must be the one the module was parsed with: interpolations the
/// wrapper closes over are carried through by their source text.
pub fn collect_local_wrapper_calls(
    module: &Module,
    source_map: &Lrc<SourceMap>,
) -> Vec<LocalWrapperCall> {
    let mut wrappers = WrapperCollector {
        source_map,
        stack: Vec::new(),
        wrappers: Vec::new(),
    };
    module.visit_with(&mut wrappers);
    if wrappers.wrappers.is_empty() {
        return Vec::new();
    }

    let mut sites = SiteCollector {
        source_map,
        wrappers: wrappers.wrappers,
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

/// Finds the file's request wrappers: named functions whose own request call
/// interpolates one of their parameters into the URL.
struct WrapperCollector<'a> {
    source_map: &'a Lrc<SourceMap>,
    /// The named functions enclosing the node being visited, innermost last.
    /// A request is attributed to the innermost one, so a helper's own nested
    /// closures count as the helper's and a nested named function owns its own.
    stack: Vec<FnFrame>,
    wrappers: Vec<WrapperDef>,
}

impl WrapperCollector<'_> {
    fn push_frame(&mut self, ident: &Ident, params: Vec<Option<String>>) {
        self.stack.push(FnFrame {
            name: ident.sym.to_string(),
            ctxt: ident.ctxt,
            params,
            resolved: None,
            ambiguous: false,
        });
    }

    fn pop_frame(&mut self) {
        let Some(frame) = self.stack.pop() else {
            return;
        };
        if frame.ambiguous {
            return;
        }
        let Some((url, method)) = frame.resolved else {
            return;
        };
        self.wrappers.push(WrapperDef {
            name: frame.name,
            ctxt: frame.ctxt,
            url,
            method,
        });
    }

    /// The URL parts and method source of `call`, when it is a request whose
    /// URL carries one of `params`.
    fn request_shape(
        &self,
        call: &CallExpr,
        params: &[Option<String>],
    ) -> Option<(Vec<UrlPart>, MethodSource)> {
        let verb = verb_from_callee_property(callee_property(call).as_deref());
        let options = request_options_argument(call);
        // Not request-shaped: no HTTP-verb callee and no request-options bag.
        // The same structural test the cross-file wrapper pass uses.
        if verb.is_none() && options.is_none() {
            return None;
        }

        let url_arg = call.args.first().filter(|arg| arg.spread.is_none())?;
        let url = url_parts(&url_arg.expr, params, self.source_map)?;

        let method = match options {
            Some((_, obj)) => match prop_value(obj, "method") {
                Some(Some(value)) => match literal_string(value) {
                    Some(literal) => {
                        let normalized = normalize_manifest_method(&literal);
                        if !is_http_method(&normalized) {
                            return None;
                        }
                        MethodSource::Fixed(normalized)
                    }
                    // A `method` key that is not a literal is the
                    // parameterized wrapper: only the site knows the method,
                    // so bind it to the site the way the URL is bound.
                    None => MethodSource::Param(param_index(value, params)?),
                },
                // Shorthand `{ method }` — the value is the binding of that
                // name, so it is a parameter or nothing this can read.
                Some(None) => MethodSource::Param(named_param_index("method", params)?),
                None => match verb {
                    Some(verb) => MethodSource::Fixed(verb),
                    None => MethodSource::Unstated,
                },
            },
            None => MethodSource::Fixed(verb?),
        };
        Some((url, method))
    }
}

impl Visit for WrapperCollector<'_> {
    fn visit_fn_decl(&mut self, node: &FnDecl) {
        let params = fn_params(&node.function.params);
        self.push_frame(&node.ident, params);
        node.visit_children_with(self);
        self.pop_frame();
    }

    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        let framed = match (&node.name, node.init.as_deref()) {
            (Pat::Ident(ident), Some(Expr::Arrow(arrow))) => {
                self.push_frame(&ident.id, arrow.params.iter().map(pat_name).collect());
                true
            }
            (Pat::Ident(ident), Some(Expr::Fn(fn_expr))) => {
                self.push_frame(&ident.id, fn_params(&fn_expr.function.params));
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
        let params = match self.stack.last() {
            Some(frame) if !frame.params.is_empty() => frame.params.clone(),
            _ => Vec::new(),
        };
        if !params.is_empty()
            && let Some(shape) = self.request_shape(node, &params)
            && let Some(frame) = self.stack.last_mut()
        {
            if frame.resolved.is_some() {
                frame.ambiguous = true;
            } else {
                frame.resolved = Some(shape);
            }
        }
        node.visit_children_with(self);
    }
}

/// Finds the call sites that delegate to one of the collected wrappers.
struct SiteCollector<'a> {
    source_map: &'a Lrc<SourceMap>,
    wrappers: Vec<WrapperDef>,
    calls: Vec<LocalWrapperCall>,
}

impl Visit for SiteCollector<'_> {
    fn visit_call_expr(&mut self, node: &CallExpr) {
        if let Callee::Expr(callee) = &node.callee
            && let Expr::Ident(ident) = &**callee
            && let Some(wrapper) = self
                .wrappers
                .iter()
                .find(|wrapper| wrapper.name == ident.sym.as_ref() && wrapper.ctxt == ident.ctxt)
            && let Some(call) = resolve_site(wrapper, node, self.source_map)
        {
            self.calls.push(call);
        }
        node.visit_children_with(self);
    }
}

/// Substitute this site's arguments into the wrapper's URL and method.
fn resolve_site(
    wrapper: &WrapperDef,
    call: &CallExpr,
    source_map: &Lrc<SourceMap>,
) -> Option<LocalWrapperCall> {
    let mut target = String::new();
    // At least one parameter slot must be filled by a literal the site states.
    // A wrapper whose every slot is filled by a variable tells us nothing the
    // wrapper's own line did not already say.
    let mut states_a_literal = false;
    for part in &wrapper.url {
        match part {
            UrlPart::Fixed(text) => target.push_str(text),
            UrlPart::Param(index) => {
                let arg = call.args.get(*index).filter(|arg| arg.spread.is_none())?;
                match argument_text(&arg.expr, source_map) {
                    Some(text) => {
                        states_a_literal = true;
                        target.push_str(&text);
                    }
                    // A slot the site fills with an expression — the base URL
                    // it holds in a variable, most often. Carried through as an
                    // interpolation of that expression, which is exactly what
                    // the wrapper's own request line reads as.
                    None => {
                        let snippet = source_map.span_to_snippet(arg.expr.span()).ok()?;
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

    let method = match &wrapper.method {
        MethodSource::Fixed(method) => Some(method.clone()),
        MethodSource::Param(index) => {
            let arg = call.args.get(*index).filter(|arg| arg.spread.is_none())?;
            let normalized = normalize_manifest_method(&literal_string(&arg.expr)?);
            if !is_http_method(&normalized) {
                return None;
            }
            Some(normalized)
        }
        MethodSource::Unstated => None,
    };

    Some(LocalWrapperCall {
        span_start: call.span.lo.0,
        span_end: call.span.hi.0,
        line_number: source_map.lookup_char_pos(call.span.lo).line,
        wrapper_name: wrapper.name.clone(),
        target,
        method,
    })
}

/// The URL a request builds, as fixed text and parameter slots, or `None` when
/// it carries none of the enclosing function's parameters (a wrapper that
/// builds its whole URL internally is already extractable where it stands).
fn url_parts(
    expr: &Expr,
    params: &[Option<String>],
    source_map: &Lrc<SourceMap>,
) -> Option<Vec<UrlPart>> {
    match expr {
        Expr::Paren(paren) => url_parts(&paren.expr, params, source_map),
        Expr::TsAs(as_expr) => url_parts(&as_expr.expr, params, source_map),
        Expr::TsNonNull(non_null) => url_parts(&non_null.expr, params, source_map),
        // The whole URL is the parameter: `fetch(path, …)`.
        Expr::Ident(_) => Some(vec![UrlPart::Param(param_index(expr, params)?)]),
        Expr::Tpl(tpl) => {
            let mut parts = Vec::new();
            let mut carries_param = false;
            for (index, quasi) in tpl.quasis.iter().enumerate() {
                let text = quasi
                    .cooked
                    .as_ref()
                    .map(|cooked| cooked.to_string())
                    .unwrap_or_else(|| quasi.raw.to_string());
                if !text.is_empty() {
                    parts.push(UrlPart::Fixed(text));
                }
                let Some(interpolated) = tpl.exprs.get(index) else {
                    continue;
                };
                match param_index(interpolated, params) {
                    Some(param) => {
                        parts.push(UrlPart::Param(param));
                        carries_param = true;
                    }
                    // Anything the wrapper closes over is carried through
                    // verbatim, exactly as the analyzer emits it from the
                    // wrapper's own line.
                    None => {
                        let snippet = source_map.span_to_snippet(interpolated.span()).ok()?;
                        parts.push(UrlPart::Fixed(format!("${{{}}}", snippet.trim())));
                    }
                }
            }
            if !carries_param {
                return None;
            }
            Some(parts)
        }
        _ => None,
    }
}

/// The position of the parameter `expr` names, when it names one.
fn param_index(expr: &Expr, params: &[Option<String>]) -> Option<usize> {
    match expr {
        Expr::Paren(paren) => param_index(&paren.expr, params),
        Expr::TsAs(as_expr) => param_index(&as_expr.expr, params),
        Expr::TsNonNull(non_null) => param_index(&non_null.expr, params),
        Expr::Ident(ident) => named_param_index(ident.sym.as_ref(), params),
        _ => None,
    }
}

fn named_param_index(name: &str, params: &[Option<String>]) -> Option<usize> {
    params
        .iter()
        .position(|param| param.as_deref() == Some(name))
}

/// The text a site's argument contributes to the URL: a string literal's value,
/// or a template's source with its interpolations intact.
fn argument_text(expr: &Expr, source_map: &Lrc<SourceMap>) -> Option<String> {
    match expr {
        Expr::Paren(paren) => argument_text(&paren.expr, source_map),
        Expr::TsAs(as_expr) => argument_text(&as_expr.expr, source_map),
        Expr::TsNonNull(non_null) => argument_text(&non_null.expr, source_map),
        Expr::Lit(Lit::Str(literal)) => Some(literal.value.to_string()),
        Expr::Tpl(_) => {
            let snippet = source_map.span_to_snippet(expr.span()).ok()?;
            let trimmed = snippet.trim();
            let inner = trimmed
                .strip_prefix('`')
                .and_then(|rest| rest.strip_suffix('`'))?;
            Some(inner.to_string())
        }
        _ => None,
    }
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

/// The names a function's parameters bind, by position.
fn fn_params(params: &[Param]) -> Vec<Option<String>> {
    params.iter().map(|param| pat_name(&param.pat)).collect()
}

/// The name a parameter binds, when it is a plain identifier. Destructured and
/// rest parameters bind no single name and hold a position no argument can be
/// read from.
fn pat_name(pat: &Pat) -> Option<String> {
    match pat {
        Pat::Ident(ident) => Some(ident.id.sym.to_string()),
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
}
