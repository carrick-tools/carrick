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
//! - A function-valued property of an object literal that a module-scope
//!   `const` holds (`export const ordersApi = { addNote: async (id) => … }`)
//!   is a member too (carrick#1733), indexed as `<binding>.<property>`. The
//!   dotted name never meets a bare one, so it joins only a call made through
//!   that binding as another module imports it (see
//!   `FileOrchestrator::resolve_object_members`). An object the module writes
//!   through anywhere, or a property a later spread may replace, states no
//!   member.
//! - The request must sit in the member's OWN body. A request inside a
//!   callback the member builds belongs to whatever later invokes that
//!   callback, not to a site that calls the member, so a factory that
//!   assembles request handlers states no request of its own.
//! - Its body must contain exactly ONE request. Two, and a site could be
//!   reaching either, so the member is dropped.
//! - A request is recognised structurally, the same test the rest of the
//!   scanner uses: an HTTP-verb callee property, or one object-literal
//!   argument carrying at least one of `method` / `headers` / `body` / `data`.
//!   The bag may sit at any position and beside any number of other
//!   arguments — a paginating transport takes a page bag as well
//!   (carrick#675) — but two literals that each carry one of those keys leave
//!   which is the bag a guess, and the member is dropped. No client library,
//!   framework or helper name appears anywhere here.
//! - The URL is the request's sole route-shaped string or template argument,
//!   at whatever position it sits, or the path a `new URL(<literal>, base)`
//!   declared in the same function supplies to it (`url.href`, `url.toString()`
//!   or the binding itself). That second form is read through
//!   [`crate::new_url_target`], and like everywhere else the base is left
//!   alone: it is an opaque value, and only the path is asserted. A request
//!   with no URL, or with two, states no single URL and the member is dropped.
//! - The method must be a literal, EXCEPT that a request-options bag stating no
//!   method at all is a `GET`. Every fetch-shaped client defaults to `GET` when
//!   its options carry no `method`, so a member whose bag holds only headers is
//!   stating a `GET` as surely as one that spells it. A bag carrying a body is
//!   not read this way: a payload with no method is a wrapper injecting the
//!   verb, not a default. A `method` key whose value is not a literal belongs
//!   to the site, not the member.
//! - Where the URL interpolates one of the member's own parameters, that
//!   interpolation must be a whole path segment. A parameter standing for a
//!   path value (`/api/v1/things/${id}`) is a path parameter and normalises to
//!   one downstream. A parameter standing anywhere else supplies path
//!   STRUCTURE, which belongs to the caller: that is the shape
//!   [`crate::local_http_wrapper`] joins by substitution, and asserting the
//!   member's half of it alone would replace a site's literal path with a
//!   parameter name.
//! - What a call through the member sends as its body is read off the same
//!   request (carrick#1782): the bag's `body` (or `data`), or a verb call's
//!   argument after the URL. One of the member's parameters sent unchanged is
//!   the call's argument in that position, typed where the parameter is
//!   declared; anything the member builds is no body the call's arguments
//!   state. See [`crate::forwarded_body`].
//!
//! The join at the call site is by name: a candidate whose callee names
//! exactly one indexed member across the modules the file imports. A name
//! declared by two of them, or twice in one module with different requests, is
//! dropped rather than picked between.
//!
//! The modules searched are two import hops deep, nearest ring first
//! (carrick#655). A factory that constructs the client and returns it inside a
//! record (`const { client } = await getProjectClient(ref)`) is the common way
//! a consumer holds a client it never imports: the consumer imports the
//! factory's module, and the factory's module imports the client's. The
//! nearest ring that declares the name decides; a name a nearer ring declares
//! ambiguously is dropped there, never looked for further out. A receiver
//! reached off `this` (`this.options.client.list()`) joins like a local: the
//! chain is a name with no import behind it. What stays out: a receiver that
//! is itself imported from a module other than the member's — including a
//! module that constructs the client and exports the instance — because the
//! name alone cannot say that binding is the client, and the fixpoint that
//! could (`external_call_candidates`) does not reach same-repo classes.
//!
//! What the join could NOT follow is counted rather than left silent
//! (carrick#656). A listing of an operation's consumers reads as complete
//! whatever the join declined, and the two receiver shapes above are declined
//! by design, so every row the join produces for a member carries how many
//! OTHER sites in the service named that member and were not followed to it
//! (`consumers_not_resolved`). Counted in
//! [`crate::agents::file_orchestrator::FileOrchestrator::resolve_imported_members`],
//! which states which declines count and which do not.
//!
//! A name alone would be too wide, because `list` and `get` and `create` are
//! what every client calls its methods. So the receiver constrains it: where
//! the call's receiver is itself an imported binding, it must be imported from
//! the very module the member came from. A receiver that is a parameter or a
//! local — the common shape, and the one this pass exists for — carries no
//! such constraint.
//!
//! The rings stop at the repo's relative import graph. A client another
//! WORKSPACE PACKAGE publishes is imported by package name, which names no
//! module here, so the rings reach nothing for it. That half is
//! `FileOrchestrator::resolve_package_surface_members`,
//! which reads the same member index off the package's published surface and
//! constrains it with the receiver's own origin
//! ([`crate::receiver_origin`]) rather than with an import of the member's
//! module.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use swc_common::{BytePos, SourceMap, SourceMapper, Spanned, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use crate::binding_scope::ident_key;
use crate::forwarded_body::{CallBody, ParamDecl, sent_ident, type_param_names};
use crate::new_url_target::UrlBindings;
use crate::type_manifest::{is_http_method, normalize_manifest_method};
use crate::wrapper_request_shape::{
    literal_string, prop_value, request_options_argument, verb_from_callee_property,
};

/// The request one member issues, read off its body.
#[derive(Debug, Clone)]
pub struct RequestMember {
    /// Upper-case HTTP method, stated as a literal by the member's request.
    pub method: String,
    /// The URL the member builds, kept exactly as written. Everything it
    /// closes over stays an interpolation (`${this.baseUrl}/api/v2/x/${id}`),
    /// so base-URL and env-var classification downstream sees the form it
    /// always sees. A URL assembled with `new URL(path, base)` is the one
    /// exception: there the base never reaches the argument as text, so the
    /// target is the path alone, which is what a host-free call already is
    /// everywhere else in the scanner.
    pub target: String,
    /// 1-based line the member's request is written on, inside the client's own
    /// module. Provenance, not identity: it is how the row the client's own
    /// file carries for this request is found again (carrick#656), and it is
    /// deliberately outside `PartialEq` below.
    pub request_line: u32,
    /// What a call through the member sends as its body (carrick#1782): no
    /// body the call's arguments state ([`CallBody::Built`]), or the declared
    /// parameter the member sends unchanged. `None` where the member sends a
    /// parameter its declaration states no type for: the site's own payload
    /// is the body there. Outside `PartialEq` too: it says how to type a site,
    /// not which request the member makes.
    pub body: Option<CallBody>,
}

/// Two members state the SAME request when their method and their URL agree.
///
/// Written out rather than derived because `request_line` and `body` are not
/// the request: a module that declares one name twice with the same request
/// keeps it (see `MemberCollector::pop_frame`), and deriving equality over
/// either would turn every such pair into a conflict and drop a member that
/// is not ambiguous at all.
impl PartialEq for RequestMember {
    fn eq(&self, other: &Self) -> bool {
        self.method == other.method && self.target == other.target
    }
}

impl Eq for RequestMember {}

/// What the member join could NOT follow, for the member a row belongs to
/// (carrick#656).
///
/// Carried on every row the join produced for that member, and on the member's
/// own request row inside its module, so a listing of an operation's consumers
/// can say it is not complete instead of reading as though it were.
///
/// WHAT THE NUMBER IS. What the scan that wrote the row counted, and only
/// that. It is not a remainder to subtract, and it lags the source BOTH ways
/// on an incremental scan: a new site in a file that did not change is never
/// counted, and a site that was fixed or deleted stays counted on every row
/// that scan did not re-analyse, until those files change or `CACHE_VERSION`
/// moves. The rules for what counts at all are in
/// [`crate::agents::file_orchestrator::FileOrchestrator::resolve_imported_members`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnfollowedMemberSites {
    /// The member name the unfollowed sites called (`getEnvironmentVariables`).
    pub member: String,
    /// How many call sites named it in this service without resolving to it.
    /// Never zero: the field is absent instead.
    pub count: u32,
}

/// One call site's join outcome: the member's request, and the name the site
/// called it by.
///
/// The name is the key the join matched on, kept because the count of sites
/// that named the same member and were NOT followed to it is a fact about the
/// NAME (carrick#656), and by the time the rows are stamped the index the join
/// read is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMember {
    /// The member name this site called.
    pub name: String,
    /// The request that member issues.
    pub member: RequestMember,
}

/// The request members a module declares, keyed by name.
pub type RequestMemberIndex = HashMap<String, RequestMember>;

/// A member and the module that declared it, as an importing file sees it.
///
/// Generic over what a member IS, because two indexes are folded the same way:
/// the request members here, and the dispatch members of
/// [`crate::wrapper_dispatch`]. The default keeps `OwnedMember<PathBuf>`
/// reading as it always has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedMember<Id, M = RequestMember> {
    pub member: M,
    pub module: Id,
}

/// Index a module's request members.
///
/// `source_map` must be the one the module was parsed with: a template URL is
/// carried through by its source text.
pub fn collect_request_members(module: &Module, source_map: &Lrc<SourceMap>) -> RequestMemberIndex {
    let mut collector = MemberCollector {
        source_map,
        urls: UrlBindings::new(source_map),
        stack: Vec::new(),
        members: HashMap::new(),
        dropped: Vec::new(),
        constant_objects: constant_objects(module),
        class_type_params: Vec::new(),
    };
    module.visit_with(&mut collector);
    for name in collector.dropped {
        collector.members.remove(&name);
    }
    collector.members
}

/// Fold several modules' request members into one name-keyed index, also
/// returning the names dropped for being declared by more than one module, or
/// twice in one module with different requests. A caller that folds several
/// rings of modules needs to tell "not declared here" from "declared
/// ambiguously here", because only the first is a reason to look one ring
/// further out.
pub fn fold_indexes_with_conflicts<Id: Clone + PartialEq, M: Clone + PartialEq>(
    indexes: impl IntoIterator<Item = (Id, HashMap<String, M>)>,
) -> (HashMap<String, OwnedMember<Id, M>>, HashSet<String>) {
    let mut folded: HashMap<String, OwnedMember<Id, M>> = HashMap::new();
    let mut conflicting: HashSet<String> = HashSet::new();
    for (module, index) in indexes {
        for (name, member) in index {
            match folded.get(&name) {
                Some(existing) if existing.member == member && existing.module == module => {}
                Some(_) => {
                    conflicting.insert(name);
                }
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
    for name in &conflicting {
        folded.remove(name);
    }
    (folded, conflicting)
}

/// One function currently being walked. An unnamed one is a barrier: it holds
/// its own requests so they cannot be attributed to the member that built it.
struct MemberFrame {
    name: Option<String>,
    params: FrameParams,
    request: Option<RequestMember>,
    /// Two requests in one member: a site could be reaching either.
    ambiguous: bool,
}

/// A member's parameters, as its request reads them.
#[derive(Default)]
struct FrameParams {
    /// The names its plain parameters bind, for the URL rule.
    names: Vec<String>,
    /// Each parameter as a body sent through it is read (carrick#1782).
    decls: Vec<Option<ParamDecl>>,
    /// The type parameters in scope: the function's own and its class's.
    type_params: HashSet<String>,
}

impl FrameParams {
    fn of_function(function: &Function, source_map: &SourceMap, class: &HashSet<String>) -> Self {
        let mut type_params = type_param_names(function.type_params.as_deref());
        type_params.extend(class.iter().cloned());
        FrameParams {
            names: fn_param_names(&function.params),
            decls: ParamDecl::all(
                function.params.iter().map(|param| &param.pat),
                function.body.as_ref(),
                source_map,
            ),
            type_params,
        }
    }

    fn of_arrow(arrow: &ArrowExpr, source_map: &SourceMap) -> Self {
        FrameParams {
            names: arrow.params.iter().filter_map(pat_name).collect(),
            decls: ParamDecl::all(&arrow.params, Some(&*arrow.body), source_map),
            type_params: type_param_names(arrow.type_params.as_deref()),
        }
    }
}

struct MemberCollector<'a> {
    source_map: &'a Lrc<SourceMap>,
    /// The `new URL(path, base)` bindings in scope, on the same walk. A client
    /// class declares `const url` in every method it has, so the scope walk is
    /// what keeps one method's path out of the next one's request.
    urls: UrlBindings<'a>,
    /// The named members enclosing the node being visited, innermost last. A
    /// request is attributed to the innermost one, so a member's own nested
    /// closures count as the member's and a nested named function owns its own.
    stack: Vec<MemberFrame>,
    members: RequestMemberIndex,
    /// Names seen twice with different requests. Removed at the end rather
    /// than as they are found, so the second declaration cannot be kept.
    dropped: Vec<String>,
    /// The object literals whose function-valued properties are members, by
    /// the span start of the literal (see [`constant_objects`]).
    constant_objects: HashSet<BytePos>,
    /// The type parameters of the classes enclosing the node being visited,
    /// innermost last (carrick#1782).
    class_type_params: Vec<HashSet<String>>,
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
        match self.members.get_mut(&name) {
            Some(existing) if *existing == request => {
                // The same request declared twice, with the body read two
                // ways: which one a site reaches is not known, so neither
                // types it (carrick#1782).
                if existing.body != request.body {
                    existing.body = Some(CallBody::Built);
                }
            }
            Some(_) => self.dropped.push(name),
            None => {
                self.members.insert(name, request);
            }
        }
    }

    /// The type parameters of the class enclosing the node being visited.
    fn class_scope(&self) -> HashSet<String> {
        self.class_type_params.last().cloned().unwrap_or_default()
    }

    /// Walk `body` as the member named `name`, or as an anonymous barrier when
    /// `name` is `None`.
    fn walk_body<N: VisitWith<Self>>(
        &mut self,
        name: Option<String>,
        params: FrameParams,
        body: &N,
    ) {
        self.stack.push(MemberFrame {
            name,
            params,
            request: None,
            ambiguous: false,
        });
        self.urls.push();
        body.visit_with(self);
        self.urls.pop();
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
        let params = FrameParams::of_function(&node.function, self.source_map, &self.class_scope());
        self.walk_body(name, params, &node.function.body);
    }

    /// A class's type parameters are in scope for its members' parameters
    /// (carrick#1782).
    fn visit_class(&mut self, node: &Class) {
        self.class_type_params
            .push(type_param_names(node.type_params.as_deref()));
        node.visit_children_with(self);
        self.class_type_params.pop();
    }

    fn visit_private_method(&mut self, node: &PrivateMethod) {
        // A `#private` method is not reachable from another file, so it is a
        // barrier rather than a member.
        self.walk_body(None, FrameParams::default(), &node.function.body);
    }

    fn visit_constructor(&mut self, node: &Constructor) {
        // A constructor is not called by name either. It is walked all the same
        // so a `const url` it declares stays inside it rather than reaching the
        // module scope, where a method that declares none could read it.
        self.walk_body(None, FrameParams::default(), &node.body);
    }

    fn visit_fn_decl(&mut self, node: &FnDecl) {
        let params = FrameParams::of_function(&node.function, self.source_map, &HashSet::new());
        self.walk_body(
            Some(node.ident.sym.to_string()),
            params,
            &node.function.body,
        );
    }

    /// Every other function is anonymous from a caller's point of view: a
    /// callback, a method of any object literal but a module constant's, a
    /// function expression in an argument. It holds its own requests.
    fn visit_function(&mut self, node: &Function) {
        self.walk_body(None, FrameParams::default(), &node.body);
    }

    fn visit_arrow_expr(&mut self, node: &ArrowExpr) {
        self.walk_body(None, FrameParams::default(), &node.body);
    }

    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        let (Pat::Ident(binding), Some(init)) = (&node.name, &node.init) else {
            node.visit_children_with(self);
            return;
        };
        let name = binding.id.sym.to_string();
        if let Some(object) = object_literal(init)
            && self.constant_objects.contains(&object.span.lo)
        {
            self.walk_object_members(&name, object);
            return;
        }
        match &**init {
            Expr::Arrow(arrow) => {
                let params = FrameParams::of_arrow(arrow, self.source_map);
                self.walk_body(Some(name), params, &arrow.body);
            }
            Expr::Fn(fn_expr) => {
                let params =
                    FrameParams::of_function(&fn_expr.function, self.source_map, &HashSet::new());
                self.walk_body(Some(name), params, &fn_expr.function.body);
            }
            _ => {
                // `const url = new URL("/api/v2/things", this.baseUrl)`, so a
                // request reached through `url.href` states its path.
                self.urls.declare(node);
                node.visit_children_with(self);
            }
        }
    }

    fn visit_call_expr(&mut self, node: &CallExpr) {
        let request = self
            .stack
            .last()
            .and_then(|frame| self.member_request(node, &frame.params));
        if let Some(request) = request {
            let frame = self
                .stack
                .last_mut()
                .expect("frame present: a request was read in it");
            if frame.request.is_some() {
                frame.ambiguous = true;
            } else {
                frame.request = Some(request);
            }
        }
        node.visit_children_with(self);
    }
}

impl MemberCollector<'_> {
    /// Walk a module constant's object literal (carrick#1733): each
    /// function-valued property is the member `<binding>.<property>`. A
    /// property a later spread may replace, a key written twice, and every
    /// other property state no member; a function inside one is walked as
    /// before and holds its own requests.
    fn walk_object_members(&mut self, binding: &str, object: &ObjectLit) {
        let last_spread = object
            .props
            .iter()
            .rposition(|prop| matches!(prop, PropOrSpread::Spread(_)));
        let mut keys: HashSet<String> = HashSet::new();
        for (position, prop) in object.props.iter().enumerate() {
            let PropOrSpread::Prop(prop) = prop else {
                prop.visit_with(self);
                continue;
            };
            let key = match &**prop {
                Prop::KeyValue(kv) => prop_name_text(&kv.key),
                Prop::Method(method) => prop_name_text(&method.key),
                Prop::Shorthand(ident) => Some(ident.sym.to_string()),
                Prop::Getter(getter) => prop_name_text(&getter.key),
                Prop::Setter(setter) => prop_name_text(&setter.key),
                Prop::Assign(_) => None,
            };
            let name = key.map(|key| {
                let name = format!("{binding}.{key}");
                if !keys.insert(key) {
                    self.dropped.push(name.clone());
                }
                name
            });
            let settled = last_spread.is_none_or(|spread| position > spread);
            match (&**prop, name) {
                (Prop::KeyValue(kv), Some(name)) if settled => match unwrap_parens(&kv.value) {
                    Expr::Arrow(arrow) => {
                        let params = FrameParams::of_arrow(arrow, self.source_map);
                        self.walk_body(Some(name), params, &arrow.body);
                    }
                    Expr::Fn(fn_expr) => {
                        let params = FrameParams::of_function(
                            &fn_expr.function,
                            self.source_map,
                            &HashSet::new(),
                        );
                        self.walk_body(Some(name), params, &fn_expr.function.body);
                    }
                    _ => prop.visit_with(self),
                },
                (Prop::Method(method), Some(name)) if settled => {
                    let params = FrameParams::of_function(
                        &method.function,
                        self.source_map,
                        &HashSet::new(),
                    );
                    self.walk_body(Some(name), params, &method.function.body);
                }
                _ => prop.visit_with(self),
            }
        }
    }

    /// The request `call` issues, when it states its whole URL and its method
    /// itself and neither depends on `params`.
    fn member_request(&self, call: &CallExpr, params: &FrameParams) -> Option<RequestMember> {
        let verb = verb_from_callee_property(callee_property(call).as_deref());
        let options = request_options_argument(call);
        if verb.is_none() && options.is_none() {
            return None;
        }
        let verb_call = verb.is_some();

        let (target_at, target) = self.sole_target_argument(call)?;
        // A URL whose STRUCTURE comes from the member's caller is the shape
        // `local_http_wrapper` joins by substitution. Asserting the member's
        // half alone would replace the site's literal with a parameter name.
        if parameter_supplies_path_structure(&target, &params.names) {
            return None;
        }

        let method = match options {
            Some((_, obj)) => match prop_value(obj, "method") {
                // A `method` key that is not a literal belongs to the site.
                Some(value) => normalize_http_method(&literal_string(value?)?)?,
                None => match verb {
                    Some(verb) => verb,
                    None => default_method(obj)?,
                },
            },
            None => verb?,
        };

        Some(RequestMember {
            method,
            target,
            request_line: u32::try_from(self.source_map.lookup_char_pos(call.span().lo).line)
                .unwrap_or(0),
            body: sent_body(call, options, verb_call, target_at, params),
        })
    }

    /// The call's single URL argument, with its position: a route-shaped
    /// string or template as written, or the path a `new URL(<literal>,
    /// base)` in scope supplies to it. `None` when the call has none, or more
    /// than one, or when the argument sits behind a spread.
    fn sole_target_argument(&self, call: &CallExpr) -> Option<(usize, String)> {
        let mut found: Option<(usize, String)> = None;
        for (position, arg) in call.args.iter().enumerate() {
            if arg.spread.is_some() {
                continue;
            }
            let stated = argument_text(&arg.expr, self.source_map)
                .filter(|text| is_route_shaped(text))
                .or_else(|| self.urls.stated_path(&arg.expr));
            let Some(text) = stated else {
                continue;
            };
            if found.is_some() {
                return None;
            }
            found = Some((position, text));
        }
        found
    }
}

/// What a call through the member sends as its body (carrick#1782): the
/// options bag's `body` (or `data`) where it carries one, or for a verb call
/// the argument after the URL.
///
/// - One of the member's parameters, unchanged: read from its declaration
///   ([`ParamDecl::forwarded`]), which leaves `None` where the declaration
///   states no type.
/// - The member's options parameter spread into the bag with no body of its
///   own after it: the caller writes the body, so `None` (the site's own
///   payload).
/// - Anything else, including no body at all: [`CallBody::Built`].
fn sent_body(
    call: &CallExpr,
    options: Option<(usize, &ObjectLit)>,
    verb: bool,
    target_at: usize,
    params: &FrameParams,
) -> Option<CallBody> {
    let param = |ident: &Ident| {
        let key = ident_key(ident);
        params.decls.iter().flatten().find(|decl| decl.key == key)
    };
    let sent: Option<&Ident> = match options.map(|(_, bag)| bag_body(bag)) {
        Some(BagBody::Sent(expr)) => sent_ident(expr),
        Some(BagBody::Named(ident)) => Some(ident),
        Some(BagBody::Spread(spread)) => {
            return match sent_ident(spread).and_then(param) {
                Some(_) => None,
                None => Some(CallBody::Built),
            };
        }
        Some(BagBody::Absent) | None if verb => call
            .args
            .get(target_at + 1)
            .filter(|arg| arg.spread.is_none())
            .filter(|_| options.is_none_or(|(at, _)| at != target_at + 1))
            .and_then(|arg| sent_ident(&arg.expr)),
        Some(BagBody::Absent) | None => None,
    };
    match sent.and_then(param) {
        Some(decl) => decl.forwarded(&params.type_params),
        None => Some(CallBody::Built),
    }
}

/// The body a request-options bag carries.
enum BagBody<'a> {
    /// The last `body` or `data` key, with nothing spread after it.
    Sent(&'a Expr),
    /// The same, written shorthand (`{ body }`).
    Named(&'a Ident),
    /// A spread with no `body` or `data` key after it, which may carry one.
    Spread(&'a Expr),
    /// Neither.
    Absent,
}

fn bag_body(bag: &ObjectLit) -> BagBody<'_> {
    let is_body = |name: Option<String>| matches!(name.as_deref(), Some("body" | "data"));
    let mut found = BagBody::Absent;
    for prop in &bag.props {
        match prop {
            PropOrSpread::Spread(spread) => found = BagBody::Spread(&spread.expr),
            PropOrSpread::Prop(prop) => match &**prop {
                Prop::KeyValue(kv) if is_body(prop_name_text(&kv.key)) => {
                    found = BagBody::Sent(&kv.value);
                }
                Prop::Shorthand(ident) if is_body(Some(ident.sym.to_string())) => {
                    found = BagBody::Named(ident);
                }
                _ => {}
            },
        }
    }
    found
}

/// The method a request-options bag that names none states.
///
/// `GET`, which is what every fetch-shaped client sends when its options carry
/// no `method`: a member whose bag holds only headers is stating a `GET` as
/// surely as one that spells it, and fifteen of one measured client's forty-one
/// request members are written that way. Reaching this point already means the
/// call states a URL and carries a request-options object, so the shape is
/// settled rather than guessed at.
///
/// A bag carrying a payload is the exception, and states nothing: a body with
/// no method is a wrapper injecting the verb, not a library default.
fn default_method(options: &ObjectLit) -> Option<String> {
    let carries_payload =
        prop_value(options, "body").is_some() || prop_value(options, "data").is_some();
    (!carries_payload).then(|| "GET".to_string())
}

/// The object literals a module-scope `const` holds and nothing in the module
/// writes through (carrick#1733), by the literal's span start. Their
/// function-valued properties are what a caller in another module reaches as
/// `<binding>.<property>`. A module that assigns to a property of the binding,
/// deletes one, or hands the binding to a built-in that writes to its first
/// argument (`Object.assign(api, …)`) can replace any member, so that object
/// states none. A write through a name a nested scope declares again counts as
/// well: dropping a member is the safe side.
fn constant_objects(module: &Module) -> HashSet<BytePos> {
    let mut declared: Vec<(String, BytePos)> = Vec::new();
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
        for declarator in &var.decls {
            if let (Pat::Ident(binding), Some(init)) = (&declarator.name, &declarator.init)
                && let Some(object) = object_literal(init)
            {
                declared.push((binding.id.sym.to_string(), object.span.lo));
            }
        }
    }
    if declared.is_empty() {
        return HashSet::new();
    }
    let mut writes = WrittenThrough::default();
    module.visit_with(&mut writes);
    declared
        .into_iter()
        .filter(|(name, _)| !writes.names.contains(name))
        .map(|(_, start)| start)
        .collect()
}

/// The object literal an initialiser is, through parentheses and the type
/// assertions that leave the value alone (`as const`, `satisfies T`, `as T`).
fn object_literal(expr: &Expr) -> Option<&ObjectLit> {
    match expr {
        Expr::Object(object) => Some(object),
        Expr::Paren(paren) => object_literal(&paren.expr),
        Expr::TsConstAssertion(assertion) => object_literal(&assertion.expr),
        Expr::TsSatisfies(satisfies) => object_literal(&satisfies.expr),
        Expr::TsAs(as_expr) => object_literal(&as_expr.expr),
        _ => None,
    }
}

fn unwrap_parens(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(paren) => unwrap_parens(&paren.expr),
        other => other,
    }
}

/// The names a module writes through: the root binding of every member
/// expression it assigns to, updates or deletes, and the first argument of
/// every `Object.assign` / `Object.defineProperty` / `Object.defineProperties`
/// / `Object.setPrototypeOf` call.
#[derive(Default)]
struct WrittenThrough {
    names: HashSet<String>,
}

impl WrittenThrough {
    fn member(&mut self, member: &MemberExpr) {
        if let Some(root) = member_root(&member.obj) {
            self.names.insert(root);
        }
    }
}

impl Visit for WrittenThrough {
    fn visit_assign_expr(&mut self, node: &AssignExpr) {
        if let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = &node.left {
            self.member(member);
        }
        node.visit_children_with(self);
    }

    fn visit_update_expr(&mut self, node: &UpdateExpr) {
        if let Expr::Member(member) = &*node.arg {
            self.member(member);
        }
        node.visit_children_with(self);
    }

    fn visit_unary_expr(&mut self, node: &UnaryExpr) {
        if node.op == UnaryOp::Delete
            && let Expr::Member(member) = &*node.arg
        {
            self.member(member);
        }
        node.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, node: &CallExpr) {
        if let Callee::Expr(callee) = &node.callee
            && let Expr::Member(member) = &**callee
            && matches!(&*member.obj, Expr::Ident(object) if object.sym == *"Object")
            && let MemberProp::Ident(prop) = &member.prop
            && matches!(
                prop.sym.as_ref(),
                "assign" | "defineProperty" | "defineProperties" | "setPrototypeOf"
            )
            && let Some(target) = node.args.first()
            && let Some(root) = member_root(&target.expr)
        {
            self.names.insert(root);
        }
        node.visit_children_with(self);
    }
}

/// The identifier a member chain starts from (`api` in `api.tasks.list`).
fn member_root(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(ident) => Some(ident.sym.to_string()),
        Expr::Member(member) => member_root(&member.obj),
        Expr::Paren(paren) => member_root(&paren.expr),
        _ => None,
    }
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

    /// A member as the assertions compare it. `request_line` and `body` are
    /// outside `PartialEq`, so the values here are never read.
    fn member(method: &str, target: &str) -> RequestMember {
        RequestMember {
            method: method.to_string(),
            target: target.to_string(),
            request_line: 0,
            body: None,
        }
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
            Some(&member(
                "PUT",
                "${this.baseUrl}/api/v2/artifacts/${encoded}"
            )),
            "the URL is at argument 1 and the method is a literal in the options bag"
        );
        assert_eq!(
            members.get("readArtifactUrl"),
            Some(&member(
                "GET",
                "${this.baseUrl}/api/v1/artifacts/${encoded}"
            )),
            "sibling methods keep their own verb and their own version"
        );
    }

    /// carrick#656: the member records the line its request is written on, so
    /// the row the client's own file carries for it can be found again.
    #[test]
    fn records_the_line_its_request_is_written_on() {
        let members = index(
            "class ApiClient {\n  listThings() {\n    return send(Schema, `${this.base}/api/v1/things`, {\n      method: \"GET\",\n    });\n  }\n}\n",
        );
        assert_eq!(
            members.get("listThings").map(|member| member.request_line),
            Some(3),
            "the request's own line, not the member's or the file's"
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
            Some(&member("GET", "${this.base}/api/v1/things"))
        );
    }

    #[test]
    fn a_url_built_with_the_url_constructor_states_the_member_s_path() {
        let members = index(
            r#"
            class C {
              describeSession() {
                const url = new URL("/api/v2/session", this.apiURL);
                return send(Schema, url.href, { headers: this.headers() });
              }
              listThings(after?: string) {
                const url = new URL("/api/v2/things", this.apiURL);
                if (after) {
                  url.searchParams.append("after", after);
                }
                return send(Schema, url.toString(), { headers: this.headers() });
              }
            }
            "#,
        );
        assert_eq!(
            members.get("describeSession").map(|m| m.target.as_str()),
            Some("/api/v2/session"),
            "the path is stated by the `new URL` the request reads through `.href`"
        );
        assert_eq!(
            members.get("listThings").map(|m| m.target.as_str()),
            Some("/api/v2/things"),
            "one method's `url` must not be read as the next one's"
        );
    }

    #[test]
    fn a_request_options_bag_that_states_no_method_is_a_get() {
        let members = index(
            r#"
            class C {
              readThing(id: string) {
                return send(Schema, `${this.base}/api/v1/things/${id}`, {
                  headers: this.headers(),
                });
              }
            }
            "#,
        );
        assert_eq!(
            members.get("readThing"),
            Some(&member("GET", "${this.base}/api/v1/things/${id}")),
            "a bag holding only headers states the method every client defaults to"
        );
    }

    #[test]
    fn a_bag_carrying_a_payload_and_no_method_states_nothing() {
        let members = index(
            r#"
            class C {
              writeThing(payload: unknown) {
                return send(Schema, `${this.base}/api/v1/things`, {
                  headers: this.headers(),
                  body: JSON.stringify(payload),
                });
              }
            }
            "#,
        );
        assert!(
            !members.contains_key("writeThing"),
            "a body with no method is a wrapper injecting the verb, not a default GET"
        );
    }

    /// carrick#675: the member's request goes through a PAGINATING transport,
    /// which takes a page bag as well as a request-options bag.
    ///
    /// Two object literals in one call used to read as no request at all, so a
    /// client's list endpoints had no member and no consumer row, while the
    /// same client's unpaginated methods resolved theirs.
    #[test]
    fn a_page_bag_beside_the_options_bag_is_still_the_members_request() {
        let members = index(
            r#"
            class C {
              listThings(page?: number) {
                return sendPage(
                  Schema,
                  `${this.base}/api/v1/things`,
                  { page, limit: 20 },
                  { method: "GET", headers: this.headers() }
                );
              }
            }
            "#,
        );
        assert_eq!(
            members.get("listThings"),
            Some(&member("GET", "${this.base}/api/v1/things")),
            "a literal carrying none of the request keys cannot make the bag ambiguous"
        );
    }

    /// Two literals that BOTH carry a request key state nothing: which one
    /// configures the request is a guess, and guessing sets the method and the
    /// path for every site that calls the member.
    #[test]
    fn two_request_options_literals_state_no_single_request() {
        let members = index(
            r#"
            class C {
              writeThing(payload: unknown) {
                return send(
                  Schema,
                  `${this.base}/api/v1/things`,
                  { body: payload },
                  { method: "POST", headers: this.headers() }
                );
              }
            }
            "#,
        );
        assert!(
            !members.contains_key("writeThing"),
            "two bags that each carry a request key leave which is which a guess"
        );
    }

    #[test]
    fn two_urls_in_one_request_state_no_single_target() {
        let members = index(
            r#"
            class C {
              copyThing() {
                const url = new URL("/api/v2/things", this.base);
                return send(Schema, url.href, "/api/v2/other", { headers: {} });
              }
            }
            "#,
        );
        assert!(
            !members.contains_key("copyThing"),
            "a built URL beside a route-shaped literal is two URLs, not one"
        );
    }

    #[test]
    fn folding_drops_a_name_two_modules_disagree_on() {
        let a =
            index(r#"class A { go() { return fetch(`${this.b}/api/a`, { method: "GET" }); } }"#);
        let b =
            index(r#"class B { go() { return fetch(`${this.b}/api/b`, { method: "GET" }); } }"#);
        assert!(
            fold_indexes_with_conflicts([("a", a.clone()), ("b", b)])
                .0
                .is_empty(),
            "nothing at the call site says which module's `go` was meant"
        );
        let (folded, conflicting) = fold_indexes_with_conflicts([("a", a.clone()), ("a", a)]);
        assert!(
            conflicting.is_empty(),
            "the same module reached twice is not a conflict"
        );
        assert_eq!(
            folded.len(),
            1,
            "the same module reached twice is not a conflict"
        );
        assert_eq!(folded["go"].module, "a", "the owning module is kept");
    }

    /// carrick#655: the conflicting names come back by name, so a caller
    /// folding several rings can stop at a ring that declares the name
    /// ambiguously instead of reading it as "not declared here".
    #[test]
    fn a_conflicting_name_is_reported_not_merely_dropped() {
        let a =
            index(r#"class A { go() { return fetch(`${this.b}/api/a`, { method: "GET" }); } }"#);
        let b =
            index(r#"class B { go() { return fetch(`${this.b}/api/b`, { method: "GET" }); } }"#);
        let (folded, conflicting) = fold_indexes_with_conflicts([("a", a), ("b", b)]);
        assert!(folded.is_empty());
        assert!(conflicting.contains("go"));
    }

    /// carrick#1733: a client written as an object constant states one member
    /// per function-valued property, under `<binding>.<property>`, through a
    /// helper the module imports, in every function form a property takes.
    #[test]
    fn an_object_constant_s_function_properties_are_members_under_dotted_names() {
        let members = index(
            r#"
            import { apiFetch } from "./http";
            export const ordersApi = {
              addNote: async (orderId: string, body: string) => {
                const form = new FormData();
                form.append("body", body);
                const response = await apiFetch(`/v1/orders/${orderId}/notes`, {
                  method: "POST",
                  body: form,
                });
                return response.json();
              },
              async setStatus(orderId: string, status: string) {
                return apiFetch(`/v1/orders/${orderId}/status`, {
                  method: "PUT",
                  body: JSON.stringify({ status }),
                });
              },
              remove: async function (orderId: string) {
                return apiFetch(`/v1/orders/${orderId}`, { method: "DELETE" });
              },
              list: async () => apiFetch("/v1/orders"),
            } as const;
            "#,
        );
        assert_eq!(
            members.get("ordersApi.addNote"),
            Some(&member("POST", "/v1/orders/${orderId}/notes"))
        );
        assert_eq!(
            members.get("ordersApi.setStatus"),
            Some(&member("PUT", "/v1/orders/${orderId}/status")),
            "a method shorthand is a member as well"
        );
        assert_eq!(
            members.get("ordersApi.remove"),
            Some(&member("DELETE", "/v1/orders/${orderId}")),
            "and a function expression"
        );
        assert!(
            !members.contains_key("ordersApi.list"),
            "a call with no options bag and no verb states no request"
        );
        assert!(
            !members.contains_key("addNote") && !members.contains_key("ordersApi"),
            "the property never answers to its bare name, nor the object to its own: {members:?}"
        );
    }

    /// carrick#1733, the negatives: every shape where the property a caller
    /// reaches may not be the function written there states no member.
    #[test]
    fn an_object_the_module_may_change_states_no_member() {
        let members = index(
            r#"
            export const written = {
              send: (id: string) => fetch(`/v1/a/${id}`, { method: "POST" }),
            };
            written.send = (id: string) => fetch(`/v1/b/${id}`, { method: "POST" });

            export const assigned = {
              send: (id: string) => fetch(`/v1/c/${id}`, { method: "POST" }),
            };
            Object.assign(assigned, overrides);

            export const deleted = {
              send: (id: string) => fetch(`/v1/d/${id}`, { method: "POST" }),
            };
            delete deleted.send;

            export const spread = {
              send: (id: string) => fetch(`/v1/e/${id}`, { method: "POST" }),
              ...overrides,
              keep: (id: string) => fetch(`/v1/f/${id}`, { method: "POST" }),
            };

            export const twice = {
              send: (id: string) => fetch(`/v1/g/${id}`, { method: "POST" }),
              send: (id: string) => fetch(`/v1/g/${id}`, { method: "POST" }),
            };

            export let reassignable = {
              send: (id: string) => fetch(`/v1/h/${id}`, { method: "POST" }),
            };

            export function factory() {
              const local = {
                send: (id: string) => fetch(`/v1/i/${id}`, { method: "POST" }),
              };
              return local;
            }
            "#,
        );
        for name in [
            "written.send",
            "assigned.send",
            "deleted.send",
            "spread.send",
            "twice.send",
            "reassignable.send",
            "local.send",
        ] {
            assert!(
                !members.contains_key(name),
                "{name} states no member: {members:?}"
            );
        }
        assert_eq!(
            members.get("spread.keep"),
            Some(&member("POST", "/v1/f/${id}")),
            "a property after the last spread is the object's own"
        );
    }

    /// What each member's body reading says, as a short label.
    fn body_of(members: &RequestMemberIndex, name: &str) -> String {
        match &members
            .get(name)
            .unwrap_or_else(|| panic!("{name} is a member: {members:?}"))
            .body
        {
            None => "site".to_string(),
            Some(CallBody::Built) => "none".to_string(),
            Some(CallBody::Param(param)) => {
                assert_eq!(param.file, "client.ts");
                format!("declared at {}", param.line)
            }
        }
    }

    /// carrick#1782: a call through a member hands it its parameters, so what
    /// the call's body is follows from what the member sends. A parameter
    /// sent unchanged (serialised, shorthand, or after the URL of a verb
    /// call) whose declaration states a type is read at that declaration. A
    /// body the member builds, no body, a parameter assigned again, a spread
    /// a later key or a non-parameter may fill: no body at the site. A
    /// declaration that states no type (none, or the class's type parameter)
    /// and an options parameter the caller writes the body into: the site's
    /// own payload.
    #[test]
    fn a_member_states_what_body_a_call_through_it_sends() {
        let members = index(
            r#"type Body = { name: string };
class Client<P> {
  create(body: Body) {
    return send(`/v1/things`, { method: "POST", body: JSON.stringify(body) });
  }
  wrap(name: string) {
    return send(`/v1/wrap`, { method: "POST", body: JSON.stringify({ name }) });
  }
  shorthand(body: Body) {
    return send(`/v1/short`, { method: "POST", body });
  }
  generic(body: P) {
    return send(`/v1/generic`, { method: "POST", body: JSON.stringify(body) });
  }
  untyped(body) {
    return send(`/v1/untyped`, { method: "POST", body: JSON.stringify(body) });
  }
  read(id: string) {
    return send(`/v1/things/${id}`, { method: "GET" });
  }
  stamped(body: Body) {
    body = { ...body };
    return send(`/v1/stamped`, { method: "POST", body: JSON.stringify(body) });
  }
  verb(body: Body) {
    return this.http.post(`/v1/verb`, body, { headers: {} });
  }
  options(init: RequestInit) {
    return send(`/v1/options`, { ...init, method: "POST" });
  }
  spreadLocal(body: Body) {
    const defaults = {};
    return send(`/v1/local`, { ...defaults, method: "POST" });
  }
  later(body: Body) {
    return send(`/v1/later`, { method: "POST", body: JSON.stringify(body), ...this.defaults });
  }
}
export const api = {
  update: async (
    id: string,
    params: { status: "a" | "b" },
  ) => send(`/v1/things/${id}`, { method: "PUT", body: JSON.stringify(params) }),
};
"#,
        );
        let read: Vec<(&str, String)> = [
            "create",
            "wrap",
            "shorthand",
            "generic",
            "untyped",
            "read",
            "stamped",
            "verb",
            "options",
            "spreadLocal",
            "later",
            "api.update",
        ]
        .into_iter()
        .map(|name| (name, body_of(&members, name)))
        .collect();
        assert_eq!(
            read,
            vec![
                ("create", "declared at 3".to_string()),
                ("wrap", "none".to_string()),
                ("shorthand", "declared at 9".to_string()),
                ("generic", "site".to_string()),
                ("untyped", "site".to_string()),
                ("read", "none".to_string()),
                ("stamped", "none".to_string()),
                ("verb", "declared at 25".to_string()),
                ("options", "site".to_string()),
                ("spreadLocal", "none".to_string()),
                ("later", "none".to_string()),
                ("api.update", "declared at 42".to_string()),
            ]
        );
    }

    /// carrick#1782: one name declared twice with the same request but its
    /// body read two ways keeps the member, and neither reading types a site.
    #[test]
    fn a_member_declared_twice_with_two_body_readings_states_no_body() {
        let members = index(
            r#"type Body = { name: string };
class A {
  save(body: Body) {
    return send(`/v1/save`, { method: "POST", body: JSON.stringify(body) });
  }
}
class B {
  save(name: string) {
    return send(`/v1/save`, { method: "POST", body: JSON.stringify({ name }) });
  }
}
class C {
  load(body: Body) {
    return send(`/v1/load`, { method: "POST", body: JSON.stringify(body) });
  }
}
class D {
  load(body: Body) {
    return send(`/v1/load`, { method: "POST", body: JSON.stringify(body) });
  }
}
"#,
        );
        assert_eq!(body_of(&members, "save"), "none");
        assert_eq!(
            body_of(&members, "load"),
            "none",
            "two declarations are two parameters: which one a site reaches is not known"
        );
    }
}
