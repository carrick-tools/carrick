//! Call sites that reach an endpoint through a request member declared in
//! ANOTHER same-repo module, where the member states its whole request itself
//! (carrick#588, wrong-method/wrong-version class).
//!
//! A service client is usually one class with one method per endpoint:
//!
//! ```ignore
//! // apiClient.ts
//! class ApiClient {
//!   createArtifactUrl(name: string) {
//!     const encoded = encodeURIComponent(name);
//!     return send(Schema, `${this.baseUrl}/api/v2/artifacts/${encoded}`, { method: "PUT" });
//!   }
//! }
//! // consumer.ts
//! const handle = await client.createArtifactUrl(filename);
//! ```
//!
//! The site carries no path and no method. Its own file states neither, so
//! everything downstream has to come from the member's body, and until this
//! pass nothing deterministic read it:
//!
//! - The member's own request goes through a helper the module imports by a
//!   relative specifier, with the URL at an argument position other than the
//!   first, so the candidate scanner raises nothing for it and the client
//!   module's real requests are invisible.
//! - [`crate::wrapper_request_shape`] folds a method per MODULE, so a client
//!   with one verb per method collapses to no method at all.
//! - [`crate::local_http_wrapper`] resolves the same indirection within one
//!   file, and only for a wrapper the site passes its path INTO.
//!
//! With nothing supplied, the site's method and path are left to extraction,
//! which has only the consumer file to read them off. That is how a `PUT` to
//! `/api/v2/...` and a `GET` to `/api/v1/...` were both recorded against a
//! path literal that appears in the consumer file only inside an error
//! message, one of them with the wrong verb and one with the wrong version.
//!
//! What this pass reads is deliberately narrow, and narrow in the direction of
//! saying nothing rather than guessing:
//!
//! - A member is a class method, a named function declaration, or a
//!   function/arrow bound to a name. It is indexed by that name.
//! - The request must sit in the member's OWN body. A request inside a
//!   callback the member builds belongs to whatever later invokes that
//!   callback, not to a site that calls the member, so a factory that
//!   assembles request handlers states no request of its own.
//! - Its body must contain exactly ONE request. Two, and a site could be
//!   reaching either, so the member is dropped.
//! - A request is recognised structurally, the same test the rest of the
//!   scanner uses: an HTTP-verb callee property, or a sole object-literal
//!   argument carrying at least one of `method` / `headers` / `body` / `data`.
//!   No client library, framework or helper name appears anywhere here.
//! - The URL is the request's sole route-shaped string or template argument,
//!   at whatever position it sits. A request with none, or with two, states no
//!   single URL and the member is dropped.
//! - The method must be a literal. A parameterised method belongs to the site,
//!   not the member.
//! - Where the URL interpolates one of the member's own parameters, that
//!   interpolation must be a whole path segment. A parameter standing for a
//!   path value (`/api/v1/things/${id}`) is a path parameter and normalises to
//!   one downstream. A parameter standing anywhere else supplies path
//!   STRUCTURE, which belongs to the caller: that is the shape
//!   [`crate::local_http_wrapper`] joins by substitution, and asserting the
//!   member's half of it alone would replace a site's literal path with a
//!   parameter name.
//!
//! The join at the call site is by name: a candidate whose callee names
//! exactly one indexed member across the modules the file imports. A name
//! declared by two of them, or twice in one module with different requests, is
//! dropped rather than picked between.
//!
//! A name alone would be too wide, because `list` and `get` and `create` are
//! what every client calls its methods. So the receiver constrains it: where
//! the call's receiver is itself an imported binding, it must be imported from
//! the very module the member came from. A receiver that is a parameter or a
//! local — the common shape, and the one this pass exists for — carries no
//! such constraint, and a receiver imported from a package matches no module
//! and so joins to nothing.

use std::collections::HashMap;

use swc_common::{SourceMap, SourceMapper, Spanned, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use crate::type_manifest::{is_http_method, normalize_manifest_method};
use crate::wrapper_request_shape::{
    is_request_options, literal_string, prop_value, sole_object_literal, verb_from_callee_property,
};

/// The request one member issues, read off its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestMember {
    /// Upper-case HTTP method, stated as a literal by the member's request.
    pub method: String,
    /// The URL the member builds, kept exactly as written. Everything it
    /// closes over stays an interpolation (`${this.baseUrl}/api/v2/x/${id}`),
    /// so base-URL and env-var classification downstream sees the form it
    /// always sees.
    pub target: String,
}

/// The request members a module declares, keyed by name.
pub type RequestMemberIndex = HashMap<String, RequestMember>;

/// A member and the module that declared it, as an importing file sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedMember<Id> {
    pub member: RequestMember,
    pub module: Id,
}

/// Index a module's request members.
///
/// `source_map` must be the one the module was parsed with: a template URL is
/// carried through by its source text.
pub fn collect_request_members(module: &Module, source_map: &Lrc<SourceMap>) -> RequestMemberIndex {
    let mut collector = MemberCollector {
        source_map,
        stack: Vec::new(),
        members: HashMap::new(),
        dropped: Vec::new(),
    };
    module.visit_with(&mut collector);
    for name in collector.dropped {
        collector.members.remove(&name);
    }
    collector.members
}

/// Fold several modules' indexes into the one an importing file resolves
/// against, keeping which module each name came from. A name two modules both
/// declare is dropped unless they agree, because nothing at the call site says
/// which was meant.
pub fn fold_indexes<Id: Clone + PartialEq>(
    indexes: impl IntoIterator<Item = (Id, RequestMemberIndex)>,
) -> HashMap<String, OwnedMember<Id>> {
    let mut folded: HashMap<String, OwnedMember<Id>> = HashMap::new();
    let mut conflicting: Vec<String> = Vec::new();
    for (module, index) in indexes {
        for (name, member) in index {
            match folded.get(&name) {
                Some(existing) if existing.member == member && existing.module == module => {}
                Some(_) => conflicting.push(name),
                None => {
                    folded.insert(
                        name,
                        OwnedMember {
                            member,
                            module: module.clone(),
                        },
                    );
                }
            }
        }
    }
    for name in conflicting {
        folded.remove(&name);
    }
    folded
}

/// One function currently being walked. An unnamed one is a barrier: it holds
/// its own requests so they cannot be attributed to the member that built it.
struct MemberFrame {
    name: Option<String>,
    params: Vec<String>,
    request: Option<RequestMember>,
    /// Two requests in one member: a site could be reaching either.
    ambiguous: bool,
}

struct MemberCollector<'a> {
    source_map: &'a Lrc<SourceMap>,
    /// The named members enclosing the node being visited, innermost last. A
    /// request is attributed to the innermost one, so a member's own nested
    /// closures count as the member's and a nested named function owns its own.
    stack: Vec<MemberFrame>,
    members: RequestMemberIndex,
    /// Names seen twice with different requests. Removed at the end rather
    /// than as they are found, so the second declaration cannot be kept.
    dropped: Vec<String>,
}

impl MemberCollector<'_> {
    fn pop_frame(&mut self) {
        let Some(frame) = self.stack.pop() else {
            return;
        };
        let Some(name) = frame.name else {
            return;
        };
        if frame.ambiguous {
            self.dropped.push(name);
            return;
        }
        let Some(request) = frame.request else {
            return;
        };
        match self.members.get(&name) {
            Some(existing) if *existing == request => {}
            Some(_) => self.dropped.push(name),
            None => {
                self.members.insert(name, request);
            }
        }
    }

    /// Walk `body` as the member named `name`, or as an anonymous barrier when
    /// `name` is `None`.
    fn walk_body<N: VisitWith<Self>>(
        &mut self,
        name: Option<String>,
        params: Vec<String>,
        body: &N,
    ) {
        self.stack.push(MemberFrame {
            name,
            params,
            request: None,
            ambiguous: false,
        });
        body.visit_with(self);
        self.pop_frame();
    }
}

impl Visit for MemberCollector<'_> {
    fn visit_class_method(&mut self, node: &ClassMethod) {
        // Getters and setters are not called with arguments and are not how a
        // client spells an endpoint.
        let name = match node.kind {
            MethodKind::Method => prop_name_text(&node.key),
            _ => None,
        };
        self.walk_body(
            name,
            fn_param_names(&node.function.params),
            &node.function.body,
        );
    }

    fn visit_private_method(&mut self, node: &PrivateMethod) {
        // A `#private` method is not reachable from another file, so it is a
        // barrier rather than a member.
        self.walk_body(None, Vec::new(), &node.function.body);
    }

    fn visit_fn_decl(&mut self, node: &FnDecl) {
        self.walk_body(
            Some(node.ident.sym.to_string()),
            fn_param_names(&node.function.params),
            &node.function.body,
        );
    }

    /// Every other function is anonymous from a caller's point of view: a
    /// callback, an object-literal method, a function expression in an
    /// argument. It holds its own requests.
    fn visit_function(&mut self, node: &Function) {
        self.walk_body(None, Vec::new(), &node.body);
    }

    fn visit_arrow_expr(&mut self, node: &ArrowExpr) {
        self.walk_body(None, Vec::new(), &node.body);
    }

    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        let (Pat::Ident(binding), Some(init)) = (&node.name, &node.init) else {
            node.visit_children_with(self);
            return;
        };
        let name = binding.id.sym.to_string();
        match &**init {
            Expr::Arrow(arrow) => {
                let params = arrow.params.iter().filter_map(pat_name).collect();
                self.walk_body(Some(name), params, &arrow.body);
            }
            Expr::Fn(fn_expr) => {
                let params = fn_param_names(&fn_expr.function.params);
                self.walk_body(Some(name), params, &fn_expr.function.body);
            }
            _ => node.visit_children_with(self),
        }
    }

    fn visit_call_expr(&mut self, node: &CallExpr) {
        if let Some(frame) = self.stack.last() {
            let params = frame.params.clone();
            if let Some(request) = self.member_request(node, &params) {
                let frame = self
                    .stack
                    .last_mut()
                    .expect("frame present: checked immediately above");
                if frame.request.is_some() {
                    frame.ambiguous = true;
                } else {
                    frame.request = Some(request);
                }
            }
        }
        node.visit_children_with(self);
    }
}

impl MemberCollector<'_> {
    /// The request `call` issues, when it states its whole URL and its method
    /// itself and neither depends on `params`.
    fn member_request(&self, call: &CallExpr, params: &[String]) -> Option<RequestMember> {
        let verb = verb_from_callee_property(callee_property(call).as_deref());
        let options = sole_object_literal(call).filter(|(_, obj)| is_request_options(obj));
        if verb.is_none() && options.is_none() {
            return None;
        }

        let target = sole_route_shaped_argument(call, self.source_map)?;
        // A URL whose STRUCTURE comes from the member's caller is the shape
        // `local_http_wrapper` joins by substitution. Asserting the member's
        // half alone would replace the site's literal with a parameter name.
        if parameter_supplies_path_structure(&target, params) {
            return None;
        }

        let method = match options {
            Some((_, obj)) => match prop_value(obj, "method") {
                // A `method` key that is not a literal belongs to the site.
                Some(value) => normalize_http_method(&literal_string(value?)?)?,
                None => verb?,
            },
            None => verb?,
        };

        Some(RequestMember { method, target })
    }
}

/// The call's single route-shaped string or template argument, as written.
/// `None` when it has none, or more than one, or when the argument sits behind
/// a spread.
fn sole_route_shaped_argument(call: &CallExpr, source_map: &Lrc<SourceMap>) -> Option<String> {
    let mut found: Option<String> = None;
    for arg in &call.args {
        if arg.spread.is_some() {
            continue;
        }
        let Some(text) = argument_text(&arg.expr, source_map) else {
            continue;
        };
        if !is_route_shaped(&text) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(text);
    }
    found
}

/// The text a string or template argument contributes: a literal's value, or a
/// template's source with its interpolations intact.
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

/// Whether a URL is shaped like something a consumer requests AND states a
/// path segment of its own. An absolute path, a full URL, or an interpolated
/// base with a literal path behind it. A helper's `${base}${path}` states no
/// segment and fails here, which is the point: its path belongs to its caller.
fn is_route_shaped(target: &str) -> bool {
    let trimmed = target.trim();
    let shaped = trimmed.starts_with('/')
        || trimmed.starts_with("${")
        || trimmed.starts_with("http://")
        || trimmed.starts_with("https://");
    shaped && states_a_literal_segment(trimmed)
}

/// Whether the URL carries a `/` outside its interpolations, i.e. a path
/// segment the member itself wrote down.
fn states_a_literal_segment(target: &str) -> bool {
    let mut depth = 0usize;
    let bytes = target.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if depth == 0 && bytes[i] == b'$' && bytes.get(i + 1) == Some(&b'{') {
            depth = 1;
            i += 2;
            continue;
        }
        if depth > 0 {
            match bytes[i] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
            i += 1;
            continue;
        }
        // A scheme's `//` is not a path segment.
        if bytes[i] == b'/' && bytes.get(i + 1) != Some(&b'/') && i > 0 && bytes[i - 1] != b'/' {
            return true;
        }
        if bytes[i] == b'/' && i == 0 {
            return true;
        }
        i += 1;
    }
    false
}

/// Whether any interpolation naming one of the member's parameters sits
/// somewhere other than a whole path segment.
///
/// `/api/v1/things/${id}` is a path parameter: the caller supplies a value and
/// the normaliser turns it into `:id`. `${base}${path}` and `${base}${prefix}/x`
/// are not: the caller supplies segments, and the member alone states no path.
/// A parameter used inside a larger expression (`${encodeURIComponent(name)}`)
/// is read the same way, because the value still comes from the caller.
fn parameter_supplies_path_structure(target: &str, params: &[String]) -> bool {
    if params.is_empty() {
        return false;
    }
    let mut offset = 0usize;
    while let Some(open) = target[offset..].find("${") {
        let open = offset + open;
        let Some(close) = target[open + 2..].find('}') else {
            return false;
        };
        let close = open + 2 + close;
        let inner = &target[open + 2..close];
        if params.iter().any(|param| expression_names(inner, param)) {
            let preceded = target[..open].ends_with('/');
            let followed = match target[close + 1..].chars().next() {
                None | Some('/') | Some('?') | Some('#') => true,
                Some(_) => false,
            };
            if !preceded || !followed {
                return true;
            }
        }
        offset = close + 1;
    }
    false
}

/// Whether `expr` uses `name` as a whole identifier.
fn expression_names(expr: &str, name: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut rest = expr;
    while let Some(at) = rest.find(name) {
        let before_ok = at == 0 || !rest[..at].chars().next_back().is_some_and(is_word);
        let after = &rest[at + name.len()..];
        let after_ok = !after.chars().next().is_some_and(is_word);
        if before_ok && after_ok {
            return true;
        }
        rest = &rest[at + name.len()..];
    }
    false
}

/// Normalise and validate an HTTP method literal.
fn normalize_http_method(literal: &str) -> Option<String> {
    let normalized = normalize_manifest_method(literal);
    is_http_method(&normalized).then_some(normalized)
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

/// A property key's name, when it is a plain one.
fn prop_name_text(key: &PropName) -> Option<String> {
    match key {
        PropName::Ident(ident) => Some(ident.sym.to_string()),
        PropName::Str(s) => Some(s.value.to_string()),
        _ => None,
    }
}

/// The names a function's parameters bind. Destructured and rest parameters
/// bind no single name.
fn fn_param_names(params: &[Param]) -> Vec<String> {
    params
        .iter()
        .filter_map(|param| pat_name(&param.pat))
        .collect()
}

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
    use swc_common::{FileName, GLOBALS, Globals, Mark, errors::Handler};
    use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};
    use swc_ecma_transforms_base::resolver;
    use swc_ecma_visit::VisitMutWith;

    fn index(content: &str) -> RequestMemberIndex {
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
        let _handler: Option<Handler> = None;
        GLOBALS.set(&Globals::new(), || {
            let unresolved = Mark::new();
            let top_level = Mark::new();
            module.visit_mut_with(&mut resolver(unresolved, top_level, true));
        });
        collect_request_members(&module, &source_map)
    }

    #[test]
    fn reads_method_and_versioned_path_off_a_class_method() {
        let members = index(
            r#"
            class ApiClient {
              createArtifactUrl(name: string) {
                const encoded = encodeURIComponent(name);
                return send(Schema, `${this.baseUrl}/api/v2/artifacts/${encoded}`, {
                  method: "PUT",
                  headers: this.headers(),
                }, merge(this.defaults));
              }
              readArtifactUrl(name: string) {
                const encoded = encodeURIComponent(name);
                return send(Schema, `${this.baseUrl}/api/v1/artifacts/${encoded}`, {
                  method: "GET",
                  headers: this.headers(),
                });
              }
            }
            "#,
        );
        assert_eq!(
            members.get("createArtifactUrl"),
            Some(&RequestMember {
                method: "PUT".to_string(),
                target: "${this.baseUrl}/api/v2/artifacts/${encoded}".to_string(),
            }),
            "the URL is at argument 1 and the method is a literal in the options bag"
        );
        assert_eq!(
            members.get("readArtifactUrl"),
            Some(&RequestMember {
                method: "GET".to_string(),
                target: "${this.baseUrl}/api/v1/artifacts/${encoded}".to_string(),
            }),
            "sibling methods keep their own verb and their own version"
        );
    }

    #[test]
    fn skips_a_member_whose_path_comes_from_its_caller() {
        let members = index(
            r#"
            export async function apiGet(origin: string, path: string, token: string) {
              return fetch(`${origin}${path}`, { headers: { Authorization: token } });
            }
            export async function apiResource(origin: string, resource: string) {
              return fetch(`${origin}/api/v1/${resource}`, { method: "GET" });
            }
            "#,
        );
        assert!(
            members.is_empty(),
            "a helper the caller passes its path into states no path of its own; got {members:?}"
        );
    }

    #[test]
    fn keeps_a_parameter_that_is_a_whole_path_segment() {
        let members = index(
            r#"
            class C {
              getThing(id: string) {
                return fetch(`${this.base}/api/v1/things/${id}`, { method: "GET" });
              }
              oddBase(prefix: string) {
                return fetch(`${this.base}${prefix}/things`, { method: "GET" });
              }
            }
            "#,
        );
        assert_eq!(
            members.get("getThing").map(|m| m.target.as_str()),
            Some("${this.base}/api/v1/things/${id}"),
            "a path parameter is a value the normaliser turns into `:id`"
        );
        assert!(
            !members.contains_key("oddBase"),
            "a parameter outside a path segment supplies structure the caller owns"
        );
    }

    #[test]
    fn skips_a_member_with_two_requests_or_a_parameterised_method() {
        let members = index(
            r#"
            class C {
              twoWays(id: string) {
                if (id) return fetch(`${this.base}/api/a/${id}`, { method: "GET" });
                return fetch(`${this.base}/api/b`, { method: "POST" });
              }
              anyVerb(verb: string) {
                return fetch(`${this.base}/api/c`, { method: verb });
              }
            }
            "#,
        );
        assert!(
            !members.contains_key("twoWays"),
            "two requests in one member: a site could be reaching either"
        );
        assert!(
            !members.contains_key("anyVerb"),
            "a parameterised method belongs to the site, not the member"
        );
    }

    #[test]
    fn a_verb_named_callee_states_the_method() {
        let members = index(
            r#"
            class C {
              listThings() {
                return this.http.get(`${this.base}/api/v1/things`);
              }
            }
            "#,
        );
        assert_eq!(
            members.get("listThings"),
            Some(&RequestMember {
                method: "GET".to_string(),
                target: "${this.base}/api/v1/things".to_string(),
            })
        );
    }

    #[test]
    fn folding_drops_a_name_two_modules_disagree_on() {
        let a =
            index(r#"class A { go() { return fetch(`${this.b}/api/a`, { method: "GET" }); } }"#);
        let b =
            index(r#"class B { go() { return fetch(`${this.b}/api/b`, { method: "GET" }); } }"#);
        assert!(
            fold_indexes([("a", a.clone()), ("b", b)]).is_empty(),
            "nothing at the call site says which module's `go` was meant"
        );
        let folded = fold_indexes([("a", a.clone()), ("a", a)]);
        assert_eq!(
            folded.len(),
            1,
            "the same module reached twice is not a conflict"
        );
        assert_eq!(folded["go"].module, "a", "the owning module is kept");
    }
}
