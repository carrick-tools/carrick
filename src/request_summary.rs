//! Request summaries: what each function's calls send, composed over the call
//! graph (carrick#1555).
//!
//! A call made through a client is the consumer side of most real contracts,
//! and it is written in three places at once: the constructor or field that
//! holds the URL, the line that issues the request, and the site that calls
//! the client with its own arguments.
//!
//! ```ignore
//! class ApiClient {
//!   constructor(config) { this.url = `${config.apiEndpoint}/rpc/gateway`; }
//!   getAll() { return this.cache.get(() => this.fetchAll()); }
//!   private fetchAll() {
//!     return fetch(this.url, { method: "POST", body: JSON.stringify({ action: "get-all" }) });
//!   }
//! }
//! // tools/graph.ts
//! await client.getAll();          // POST /rpc/gateway {action=get-all}
//! ```
//!
//! This pass reads all three. Every function gets a [`Summary`]: the requests
//! a call to it sends, with the URL kept as literal pieces and holes, and the
//! function's own parameters kept symbolic so each call site can fill them in
//! with its own arguments. Summaries are composed bottom-up over the call
//! graph discovery already resolves ([`crate::call_graph`]), so a receiver the
//! file DECLARES (`client: ApiClient`, `this.cache: Cache`) reaches its class
//! exactly as it does for `get_callers`. There is no second resolver and no
//! join by name.
//!
//! What it states, and nothing more:
//!
//! - **A literal is a literal.** Every piece of a URL is a literal the source
//!   writes, a parameter of the function being summarised, or an OPAQUE value
//!   the source names but this pass does not read (an environment variable, a
//!   constructor argument, an import). An opaque value may lead a URL, where it
//!   is a base, or stand for a whole path segment, where it is a path
//!   parameter. Anywhere else it supplies structure the source does not state,
//!   and no row is emitted.
//! - **A field is read where it is written.** `this.url` is the value the
//!   constructor or the field initialiser assigns, and only when nothing else
//!   in the class assigns it. A field written anywhere else is opaque.
//! - **A request is the shape the rest of the scanner reads.** A `fetch`
//!   call, an HTTP-verb member call with one route-shaped argument, or a call
//!   carrying exactly one request-options bag (`method`/`headers`/`body`/
//!   `data`). A call the call graph resolves to a function in this service is
//!   never read as a request: it is composed instead.
//! - **A callback counts only where it is invoked.** A function passed to a
//!   callee this service defines contributes its requests when that callee
//!   calls the parameter it arrives in. Passed to anything else, nothing says
//!   whether it runs, so it contributes nothing and the caller is not
//!   complete.
//! - **Nothing is only what is proven.** A summary is COMPLETE when every call
//!   in it resolved and every callee is complete. A call site whose callee is
//!   complete and sends nothing is recorded, so a model row claiming a request
//!   there can be withdrawn (`invalidateCache()`).
//!
//! Rows, per site:
//!
//! - the request's own line, when its URL is determined there;
//! - the site that fills the last hole of a request a callee leaves open (a
//!   wrapper called with its path), which is where the request is stated;
//! - a site in ANOTHER module calling a function the request's own module
//!   declares, whose request is already determined: a call through a
//!   declaration, naming the request line it reaches. A call between
//!   functions of one module adds no row, since the module's own request line
//!   already states it, and neither does a call into a module that only
//!   reaches the request through the client: that module's own call into the
//!   client is the row, and one per hop would count one request once per
//!   caller up the chain.
//!
//! One row per request reached (carrick#1555, decision 2): a call reaching two
//! requests is two rows, each with the literals its own body writes.
//!
//! **Library clients** (carrick#1564). A call through a data-fetching
//! package's client is read through that package's verified semantics
//! ([`crate::client_semantics`]): `api.post("/orders", body)` on
//! `const api = http.create({ baseURL: "/api/v1" })` sends `POST
//! /api/v1/orders` with that body. The receiver is identified from the syntax
//! alone: a binding imported from the package, or a binding or class field
//! initialised by `<that binding>.<factory>(<object literal>)` and never
//! reassigned. Nothing is inferred per site, so `new Map().get("/r")` reaches
//! no claim. A verified receiver's verb call states a row at its own site,
//! which a verb call otherwise never does; an unverified one is read exactly
//! as it would be without the semantics. Summaries are composed only once the
//! semantics are verified, which is after framework detection and the
//! service's sidecar init, so discovery hands over its inputs
//! ([`RequestSummaryInputs`]) rather than its rows.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use swc_common::{SourceMap, SourceMapper, Span, Spanned, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use crate::call_graph::CallSiteTargets;
use crate::client_semantics::{LibrarySemantics, instance_receiver};
use crate::commonjs::{require_bound_names, require_specifier};
use crate::services::type_sidecar::{SemanticsRequestArgs, SemanticsVerbArgs};
use crate::swc_scanner::SWC_SPAN_BASE;
use crate::type_manifest::is_http_method;
use crate::visitor::SymbolKind;
use crate::wrapper_request_shape::{is_request_options, verb_from_callee_property};

/// One piece of a URL or a body value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Piece {
    /// Text the source writes.
    Lit(String),
    /// The summarised function's own parameter, by position, with the name
    /// the function gives it. Filled in at each call site.
    Param(usize, String),
    /// A value the source names and this pass does not read, kept as written
    /// (`process.env.API_URL`, `config.apiEndpoint`).
    Opaque(String),
    /// A value that cannot be written as text at all (an object in a URL, a
    /// missing argument). A URL holding one states nothing.
    Unknown,
}

/// A value as far as the source states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A string, as a concatenation of pieces.
    Str(Vec<Piece>),
    /// An object literal: its plain keys, and whether anything else (a
    /// spread, a computed key) is in it too.
    Obj(ObjValue),
    /// A function expression passed as an argument, by its index among the
    /// enclosing function's nested callbacks.
    Callback(usize),
}

/// An object literal's plain keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObjValue {
    pub fields: BTreeMap<String, Value>,
    pub open: bool,
}

impl ObjValue {
    /// Put a spread in place whose possible objects are `branches`: a key it
    /// sets to the same value on every branch holds that value; a key only
    /// some branches set, or set to different values, holds nothing the
    /// source states, and the object is open. Keys no branch carries stand.
    fn spread(&mut self, branches: &[ObjValue]) {
        let keys: BTreeSet<&String> = branches
            .iter()
            .flat_map(|branch| branch.fields.keys())
            .collect();
        for key in keys {
            let first = branches[0].fields.get(key);
            let same = first.is_some()
                && branches
                    .iter()
                    .all(|branch| branch.fields.get(key) == first);
            match first {
                Some(value) if same => {
                    self.fields.insert(key.clone(), value.clone());
                }
                _ => {
                    self.fields.remove(key);
                    self.open = true;
                }
            }
        }
    }
}

impl Value {
    fn opaque(text: String) -> Self {
        Value::Str(vec![Piece::Opaque(text)])
    }

    /// This value as string pieces. An object or a function in a string
    /// position is not text the source states.
    fn pieces(&self) -> Vec<Piece> {
        match self {
            Value::Str(pieces) => pieces.clone(),
            Value::Obj(_) | Value::Callback(_) => vec![Piece::Unknown],
        }
    }
}

/// Concatenate piece lists, merging adjacent literals.
fn concat(parts: impl IntoIterator<Item = Vec<Piece>>) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    for part in parts {
        for piece in part {
            match (out.last_mut(), piece) {
                (Some(Piece::Lit(prev)), Piece::Lit(next)) => prev.push_str(&next),
                (_, Piece::Lit(next)) if next.is_empty() => {}
                (_, piece) => out.push(piece),
            }
        }
    }
    out
}

/// A request's method.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MethodValue {
    Lit(String),
    Param(usize),
    /// A key of the function's parameter at this position: an options object
    /// the caller writes (`fetch(url, init)` reads `init.method`).
    ParamKey(usize, String),
    Unknown,
}

/// How a call reads as a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestKind {
    /// The platform's own `fetch`.
    Fetch,
    /// A call carrying exactly one request-options bag.
    Bag,
    /// An HTTP-verb member call (`http.get(url)`). Whether the receiver sends
    /// a request or registers a route is a property of its type, which this
    /// pass does not read (the 2026-09-05 ruling), so a verb call states no row
    /// at its own site. Inside a function another module calls, it is what the
    /// imported-member join always read it as: the member's request.
    Verb,
    /// A call through a library client whose semantics the package's own
    /// declarations verified (carrick#1564). The receiver's type is what the
    /// verification read, so its verb calls are requests, and one states a
    /// row at its own site.
    Library,
}

/// A request's body, as far as the call writes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum BodyValue {
    #[default]
    Unstated,
    /// An object literal, written at the call.
    Fields(ObjValue),
    /// The function's own parameter, serialised as it arrives
    /// (`body: JSON.stringify(body)`): the caller writes the object.
    Param(usize),
    /// A key of the function's parameter: the options object the caller
    /// writes carries the body (`fetch(url, init)` sends `init.body`).
    ParamKey(usize, String),
}

/// A request call, as written at its site.
#[derive(Debug, Clone)]
struct RequestShape {
    kind: RequestKind,
    method: MethodValue,
    url: Vec<Piece>,
    body: BodyValue,
    /// The URL is a literal or a template written at the call itself. Such a
    /// site is read by the passes that read a site's own source, with their
    /// own base rules; a summary states its own line only for a URL it read
    /// through a binding the site does not write (a field, a constant).
    url_inline: bool,
    /// A library client's base, joined to `url` only when the row is stated
    /// (see [`join_base`]). Empty for every other request.
    base: Vec<Piece>,
    /// The library-semantics claim ids this reading used. Empty for every
    /// other request.
    semantics: BTreeSet<String>,
}

/// A client of a library package, as the source names it (carrick#1564): a
/// binding imported from the package, or one built by a factory call on such
/// a binding.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClientRef {
    /// The import specifier, exactly as written.
    package: String,
    /// `"default"` or the named export the binding was imported as.
    export: String,
    /// Set when the binding holds an instance the export's factory built.
    instance: Option<ClientInstance>,
    /// The last property of every assignment the file makes through this
    /// binding (`api.defaults.baseURL = …` gives `baseURL`). A write to the
    /// base key through the client changes where its requests go after the
    /// factory ran, so a reading through it would state the old base.
    assigned: BTreeSet<String>,
}

/// `export.factory({ ... })`: the factory member and the object literal it
/// was handed, read where the instance is built.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClientInstance {
    factory: String,
    options: ObjValue,
}

/// A call made through a library client: on one of its members, or on the
/// client itself (`member: None`).
#[derive(Debug, Clone)]
struct CallReceiver {
    client: ClientRef,
    member: Option<String>,
}

/// Where a call is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Site {
    /// Span start in the discovery source map, which is the key
    /// [`CallSiteTargets`] is read with.
    lo: u32,
    /// Span start and end within the file, in SWC's own numbering (see
    /// [`SWC_SPAN_BASE`]): the key every candidate and row carries.
    pub span_start: u32,
    pub span_end: u32,
    pub line: u32,
}

/// One call in a function body.
#[derive(Debug, Clone)]
struct CallIr {
    site: Site,
    /// The name the call is made through: the member (`getAllRepoData`) or
    /// the identifier (`fetch`). What the row displays as its client.
    callee: String,
    args: Vec<Value>,
    /// What the call sends, when it is shaped like a request. Used only if
    /// the call graph does not resolve the call to a function of this
    /// service.
    request: Option<RequestShape>,
    /// A call on a runtime global that sends nothing (`console.log`,
    /// `JSON.stringify`).
    inert: bool,
    /// The body's own parameter, when that is what is called (`produce()`).
    /// What it sends is the caller's callback's, counted where the callback
    /// is written.
    invokes_param: Option<usize>,
    /// The library client the call is made through, when the syntax names
    /// one. What it sends is read through the client's verified semantics,
    /// which are known only when the summaries are composed.
    receiver: Option<CallReceiver>,
}

/// One function body, reduced to what a request summary reads.
#[derive(Debug, Clone, Default)]
struct FnIr {
    calls: Vec<CallIr>,
    /// Parameters the body calls (`produce()`), by position.
    invoked_params: BTreeSet<usize>,
    /// Function expressions passed as call arguments, referenced from a
    /// [`Value::Callback`] argument.
    nested: Vec<FnIr>,
    /// Every other function written inside the body: an object's method, a
    /// lambda held in a local, a closure returned. Its calls state rows where
    /// they are written; what it sends belongs to whoever calls it, so it is
    /// never composed into this body's summary.
    detached: Vec<FnIr>,
    /// A declaration with no body (`declare function`, an overload, an
    /// abstract member): it states nothing about what it does, so a call to it
    /// is never proven to send nothing.
    bodyless: bool,
    /// The body does something this pass cannot follow that could send a
    /// request: constructs a class that is not the language's own, makes an
    /// optional call (`client?.load()`), calls a tag or imports a module at run
    /// time. It may still send what it is read to send; it is never proven to
    /// send only that.
    unfollowed: bool,
}

/// Everything one file contributes: its functions, keyed exactly as
/// [`crate::visitor::FunctionDefinitionExtractor`] keys them, so a
/// [`CallSiteTargets`] entry names them directly.
#[derive(Debug, Default)]
pub struct FileIr {
    functions: HashMap<String, FnIr>,
}

/// Runtime globals whose calls send nothing, by receiver. Not client
/// libraries: the language's own objects, which no request goes through.
const INERT_GLOBALS: &[&str] = &[
    "console", "JSON", "Math", "Object", "Array", "Number", "String", "Boolean", "Date", "Symbol",
    "Reflect",
];

/// Read one parsed file. `definition_keys` is the extractor's key set for the
/// same file, used to name a static member the way the extractor does when a
/// static and an instance member share a name.
pub fn extract_file_ir(
    module: &Module,
    source_map: &Lrc<SourceMap>,
    definition_keys: &HashSet<String>,
) -> FileIr {
    let mut writes = MemberWrites::default();
    module.visit_with(&mut writes);
    let mut module_scope = ModuleScope {
        consts: HashMap::new(),
        receivers: HashMap::new(),
        member_writes: writes.by_root,
        subclass_fields: subclass_fields(module),
    };
    module_scope.receivers = import_receivers(module)
        .into_iter()
        .map(|(name, client)| {
            let client = module_scope.with_writes(client, &name);
            (name, client)
        })
        .collect();
    // A name some function or block declares again means something else
    // there, and nothing here tracks which scope a use sits in, so it is no
    // client anywhere in the file.
    let redeclared = redeclared_names(module);
    module_scope
        .receivers
        .retain(|name, _| !redeclared.contains(name));
    let mut file = FileIr::default();
    let reader = Reader { source_map };

    // Module constants first, in source order, so a later one can read an
    // earlier one (`const BASE = ...; const USERS = `${BASE}/users``).
    for item in &module.body {
        if let Some(decl) = module_var_decl(item)
            && decl.kind == VarDeclKind::Const
        {
            for declarator in &decl.decls {
                if let (Pat::Ident(ident), Some(init)) = (&declarator.name, &declarator.init)
                    && !matches!(&**init, Expr::Arrow(_) | Expr::Fn(_))
                {
                    let name = ident.id.sym.to_string();
                    let (value, client) = {
                        let scope = Scope::module(&module_scope);
                        (reader.eval(init, &scope), reader.factory_call(init, &scope))
                    };
                    if let Some(client) = client
                        && !redeclared.contains(&name)
                    {
                        let client = module_scope.with_writes(client, &name);
                        module_scope.receivers.insert(name.clone(), client);
                    }
                    module_scope.consts.insert(name, value);
                }
            }
        }
    }
    let module_scope = &module_scope;
    let none = Captured::default();

    for item in &module.body {
        match module_decl(item) {
            Some(Decl::Fn(fn_decl)) => {
                let ir = reader.function(&fn_decl.function, None, module_scope, &none);
                file.functions.insert(fn_decl.ident.sym.to_string(), ir);
            }
            Some(Decl::Var(var)) => {
                for declarator in &var.decls {
                    let (Pat::Ident(ident), Some(init)) = (&declarator.name, &declarator.init)
                    else {
                        continue;
                    };
                    let ir = match &**init {
                        Expr::Arrow(arrow) => reader.arrow(arrow, None, module_scope, &none),
                        Expr::Fn(fn_expr) => {
                            reader.function(&fn_expr.function, None, module_scope, &none)
                        }
                        _ => continue,
                    };
                    file.functions.insert(ident.id.sym.to_string(), ir);
                }
            }
            Some(Decl::Class(class)) => {
                reader.class(
                    class.ident.sym.as_ref(),
                    &class.class,
                    module_scope,
                    definition_keys,
                    &mut file,
                );
            }
            _ => {}
        }
        // `export default function name() {}` and `export default class Name
        // {}`: named, so the extractor keys them by that name.
        if let ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(export)) = item {
            match &export.decl {
                DefaultDecl::Fn(FnExpr {
                    ident: Some(ident),
                    function,
                }) => {
                    let ir = reader.function(function, None, module_scope, &none);
                    file.functions.insert(ident.sym.to_string(), ir);
                }
                DefaultDecl::Class(ClassExpr {
                    ident: Some(ident),
                    class,
                }) => {
                    reader.class(
                        ident.sym.as_ref(),
                        class,
                        module_scope,
                        definition_keys,
                        &mut file,
                    );
                }
                _ => {}
            }
        }
    }
    file
}

fn module_decl(item: &ModuleItem) -> Option<&Decl> {
    match item {
        ModuleItem::Stmt(Stmt::Decl(decl)) => Some(decl),
        ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => Some(&export.decl),
        _ => None,
    }
}

fn module_var_decl(item: &ModuleItem) -> Option<&VarDecl> {
    match module_decl(item)? {
        Decl::Var(var) => Some(var),
        _ => None,
    }
}

/// What module scope holds for every function in the file: its constants,
/// and the bindings that name a library client (carrick#1564).
#[derive(Default)]
struct ModuleScope {
    consts: HashMap<String, Value>,
    receivers: HashMap<String, ClientRef>,
    /// [`MemberWrites`] over the whole file.
    member_writes: HashMap<String, BTreeSet<String>>,
    /// Class name -> the fields a class in this file that extends it
    /// declares or writes again.
    subclass_fields: HashMap<String, HashSet<String>>,
}

impl ModuleScope {
    /// `client`, with the properties the file assigns through `binding`.
    fn with_writes(&self, mut client: ClientRef, binding: &str) -> ClientRef {
        client.assigned = self.member_writes.get(binding).cloned().unwrap_or_default();
        client
    }
}

/// The last property of every member assignment in a subtree, keyed by the
/// binding the member chain starts at: `api` for `api.defaults.baseURL = …`,
/// `this.api` for `this.api.defaults.baseURL = …`.
#[derive(Default)]
struct MemberWrites {
    by_root: HashMap<String, BTreeSet<String>>,
}

impl Visit for MemberWrites {
    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        if let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = &assign.left
            && let Some(last) = written_key(&member.prop)
            && let Some(root) = member_root(&member.obj)
        {
            self.by_root.entry(root).or_default().insert(last);
        }
        assign.visit_children_with(self);
    }
}

/// The property a member assignment writes, where the source names it.
fn written_key(prop: &MemberProp) -> Option<String> {
    match prop {
        MemberProp::Ident(ident) => Some(ident.sym.to_string()),
        MemberProp::PrivateName(private) => Some(format!("#{}", private.name)),
        MemberProp::Computed(computed) => match &*computed.expr {
            Expr::Lit(Lit::Str(key)) => Some(key.value.to_string()),
            _ => None,
        },
    }
}

/// The binding a member chain starts at: an identifier, or a field of
/// `this`.
fn member_root(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Paren(e) => member_root(&e.expr),
        Expr::TsAs(e) => member_root(&e.expr),
        Expr::TsNonNull(e) => member_root(&e.expr),
        Expr::TsSatisfies(e) => member_root(&e.expr),
        Expr::TsTypeAssertion(e) => member_root(&e.expr),
        Expr::Ident(ident) => Some(ident.sym.to_string()),
        Expr::Member(member) => match member.obj.unwrap_parens() {
            Expr::This(_) => written_key(&member.prop).map(|field| format!("this.{field}")),
            _ => member_root(&member.obj),
        },
        _ => None,
    }
}

/// Every class this file declares by name, with the class it extends (when
/// that is a name) and the fields it declares or writes.
fn class_fields_by_name(module: &Module) -> HashMap<String, (Option<String>, HashSet<String>)> {
    #[derive(Default)]
    struct Classes {
        found: HashMap<String, (Option<String>, HashSet<String>)>,
    }
    impl Classes {
        fn record(&mut self, name: String, class: &Class) {
            let superclass = match class.super_class.as_deref().map(Expr::unwrap_parens) {
                Some(Expr::Ident(ident)) => Some(ident.sym.to_string()),
                _ => None,
            };
            let mut fields = HashSet::new();
            let mut writes = ThisWrites::default();
            for member in &class.body {
                match member {
                    ClassMember::ClassProp(prop) if !prop.is_static => {
                        if let Some(name) = prop_name(&prop.key) {
                            fields.insert(name);
                        }
                    }
                    ClassMember::PrivateProp(prop) if !prop.is_static => {
                        fields.insert(format!("#{}", prop.key.name));
                    }
                    ClassMember::Constructor(ctor) => {
                        for param in &ctor.params {
                            if let ParamOrTsParamProp::TsParamProp(prop) = param
                                && let TsParamPropParam::Ident(ident) = &prop.param
                            {
                                fields.insert(ident.id.sym.to_string());
                            }
                        }
                    }
                    _ => {}
                }
                member.visit_with(&mut writes);
            }
            fields.extend(writes.fields);
            self.found.insert(name, (superclass, fields));
        }
    }
    impl Visit for Classes {
        fn visit_class_decl(&mut self, decl: &ClassDecl) {
            self.record(decl.ident.sym.to_string(), &decl.class);
            decl.visit_children_with(self);
        }
        fn visit_class_expr(&mut self, expr: &ClassExpr) {
            if let Some(ident) = &expr.ident {
                self.record(ident.sym.to_string(), &expr.class);
            }
            expr.visit_children_with(self);
        }
    }
    let mut classes = Classes::default();
    module.visit_with(&mut classes);
    classes.found
}

/// Class name -> every field a class extending it, directly or through
/// others in this file, declares or writes. Such a field holds the
/// subclass's value on a subclass instance, so the base class's own
/// methods cannot read it as the base class writes it.
fn subclass_fields(module: &Module) -> HashMap<String, HashSet<String>> {
    let classes = class_fields_by_name(module);
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    for (name, (superclass, fields)) in &classes {
        let mut seen: HashSet<&str> = HashSet::from([name.as_str()]);
        let mut ancestor = superclass.as_deref();
        while let Some(base) = ancestor {
            if !seen.insert(base) {
                break;
            }
            out.entry(base.to_string())
                .or_default()
                .extend(fields.iter().cloned());
            ancestor = classes.get(base).and_then(|(sup, _)| sup.as_deref());
        }
    }
    out
}

/// A class's field table: the value each field holds, and the fields that
/// hold a library client's instance.
#[derive(Default)]
struct ClassFields {
    values: HashMap<String, Value>,
    receivers: HashMap<String, ClientRef>,
}

/// What a function written inside another can read of it.
#[derive(Default)]
struct Captured {
    values: HashMap<String, Value>,
    receivers: HashMap<String, ClientRef>,
}

/// Every module-scope binding a package specifier introduces, with the export
/// it names (carrick#1564): `import x from "p"` and `const x = require("p")`
/// name `default`; `import { a as x } from "p"`, `const { a: x } =
/// require("p")` and `const x = require("p").a` name `a`. A namespace import
/// names the module rather than an export, and a relative specifier names no
/// package, so neither is a client.
fn import_receivers(module: &Module) -> HashMap<String, ClientRef> {
    const DEFAULT: &str = "default";
    let mut receivers = HashMap::new();
    let mut add = |local: String, package: String, export: String| {
        if !package.starts_with('.') && !package.starts_with('/') {
            receivers.insert(
                local,
                ClientRef {
                    package,
                    export,
                    instance: None,
                    assigned: BTreeSet::new(),
                },
            );
        }
    };
    for item in &module.body {
        match item {
            ModuleItem::ModuleDecl(ModuleDecl::Import(import)) if !import.type_only => {
                let package = import.src.value.to_string();
                for specifier in &import.specifiers {
                    match specifier {
                        ImportSpecifier::Default(default) => {
                            add(
                                default.local.sym.to_string(),
                                package.clone(),
                                DEFAULT.to_string(),
                            );
                        }
                        ImportSpecifier::Named(named) if !named.is_type_only => {
                            let export = match &named.imported {
                                Some(ModuleExportName::Ident(ident)) => ident.sym.to_string(),
                                Some(ModuleExportName::Str(name)) => name.value.to_string(),
                                None => named.local.sym.to_string(),
                            };
                            add(named.local.sym.to_string(), package.clone(), export);
                        }
                        _ => {}
                    }
                }
            }
            ModuleItem::ModuleDecl(ModuleDecl::TsImportEquals(decl)) if !decl.is_type_only => {
                if let TsModuleRef::TsExternalModuleRef(external) = &decl.module_ref {
                    add(
                        decl.id.sym.to_string(),
                        external.expr.value.to_string(),
                        DEFAULT.to_string(),
                    );
                }
            }
            ModuleItem::Stmt(Stmt::Decl(Decl::Var(var))) if var.kind == VarDeclKind::Const => {
                for declarator in &var.decls {
                    let Some(init) = declarator.init.as_deref() else {
                        continue;
                    };
                    if let Expr::Member(member) = init
                        && let Some(package) = require_specifier(&member.obj)
                        && let MemberProp::Ident(property) = &member.prop
                        && let Pat::Ident(local) = &declarator.name
                    {
                        add(local.id.sym.to_string(), package, property.sym.to_string());
                        continue;
                    }
                    let Some(package) = require_specifier(init) else {
                        continue;
                    };
                    for bound in require_bound_names(&declarator.name) {
                        let export = match bound.kind {
                            SymbolKind::Namespace => DEFAULT.to_string(),
                            SymbolKind::Named | SymbolKind::Default => bound.imported,
                        };
                        add(bound.local.id.sym.to_string(), package.clone(), export);
                    }
                }
            }
            _ => {}
        }
    }
    receivers
}

/// Every name declared below module scope: in a function (its parameters
/// included), a class, or a block.
fn redeclared_names(module: &Module) -> HashSet<String> {
    let mut names = DeclaredNames::default();
    for item in &module.body {
        match item {
            ModuleItem::ModuleDecl(ModuleDecl::Import(_) | ModuleDecl::TsImportEquals(_)) => {}
            _ => match module_decl(item) {
                // The binding a module-scope declaration introduces is the
                // module's own; only what is written inside it declares
                // anything further down.
                Some(Decl::Var(var)) => {
                    for declarator in &var.decls {
                        declarator.init.visit_with(&mut names);
                    }
                }
                Some(Decl::Fn(fn_decl)) => fn_decl.function.visit_with(&mut names),
                Some(Decl::Class(class)) => class.class.visit_with(&mut names),
                _ => item.visit_with(&mut names),
            },
        }
    }
    names.names
}

/// Every binding a subtree declares.
#[derive(Default)]
struct DeclaredNames {
    names: HashSet<String>,
    /// How many times each is declared, for [`Reader::body`].
    counts: HashMap<String, usize>,
}

impl DeclaredNames {
    fn declare(&mut self, name: String) {
        *self.counts.entry(name.clone()).or_default() += 1;
        self.names.insert(name);
    }
}

impl Visit for DeclaredNames {
    fn visit_binding_ident(&mut self, ident: &BindingIdent) {
        self.declare(ident.id.sym.to_string());
    }

    fn visit_fn_decl(&mut self, decl: &FnDecl) {
        self.declare(decl.ident.sym.to_string());
        decl.visit_children_with(self);
    }

    fn visit_class_decl(&mut self, decl: &ClassDecl) {
        self.declare(decl.ident.sym.to_string());
        decl.visit_children_with(self);
    }

    // A parameter named in a type (`(api: Api) => void`) binds nothing.
    fn visit_ts_type(&mut self, _: &TsType) {}

    fn visit_ts_interface_body(&mut self, _: &TsInterfaceBody) {}
}

/// What an expression can read: the function's parameters and constants, the
/// enclosing class's fields, and the module's constants.
struct Scope<'a> {
    params: Vec<Option<String>>,
    locals: HashMap<String, Value>,
    /// Locals holding a library client's instance (carrick#1564).
    local_receivers: HashMap<String, ClientRef>,
    fields: Option<&'a ClassFields>,
    module: &'a ModuleScope,
}

impl<'a> Scope<'a> {
    fn module(module: &'a ModuleScope) -> Self {
        Self {
            params: Vec::new(),
            locals: HashMap::new(),
            local_receivers: HashMap::new(),
            fields: None,
            module,
        }
    }

    fn param_index(&self, name: &str) -> Option<usize> {
        self.params.iter().position(|p| p.as_deref() == Some(name))
    }

    /// The library client `name` holds here, if it holds one. A parameter or
    /// a local of the same name is not the module's binding.
    fn receiver(&self, name: &str) -> Option<&ClientRef> {
        if self.param_index(name).is_some() {
            return None;
        }
        if let Some(client) = self.local_receivers.get(name) {
            return Some(client);
        }
        if self.locals.contains_key(name) {
            return None;
        }
        self.module.receivers.get(name)
    }
}

struct Reader<'a> {
    source_map: &'a Lrc<SourceMap>,
}

impl Reader<'_> {
    fn text(&self, span: Span) -> String {
        self.source_map
            .span_to_snippet(span)
            .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    }

    fn site(&self, span: Span) -> Site {
        let local = self.source_map.lookup_byte_offset(span.lo).pos.0;
        let local_hi = self.source_map.lookup_byte_offset(span.hi).pos.0;
        Site {
            lo: span.lo.0,
            span_start: local + SWC_SPAN_BASE,
            span_end: local_hi + SWC_SPAN_BASE,
            line: self.source_map.lookup_char_pos(span.lo).line as u32,
        }
    }

    /// A class: its field table, then each member keyed as the extractor
    /// keys it.
    fn class(
        &self,
        name: &str,
        class: &Class,
        module: &ModuleScope,
        definition_keys: &HashSet<String>,
        file: &mut FileIr,
    ) {
        let redeclared = module.subclass_fields.get(name);
        let fields = self.class_fields(class, module, redeclared);
        let none = Captured::default();
        let key = |member: &str, is_static: bool| {
            let plain = format!("{name}.{member}");
            let statik = format!("{name}.static.{member}");
            if is_static && definition_keys.contains(&statik) {
                statik
            } else {
                plain
            }
        };
        // In a static member `this` is the class, not an instance: the
        // instance field table says nothing about what it holds.
        let table = |is_static: bool| (!is_static).then_some(&fields);
        for member in &class.body {
            match member {
                ClassMember::Method(method) if !matches!(method.kind, MethodKind::Setter) => {
                    let Some(member_name) = prop_name(&method.key) else {
                        continue;
                    };
                    let ir =
                        self.function(&method.function, table(method.is_static), module, &none);
                    file.functions
                        .insert(key(&member_name, method.is_static), ir);
                }
                ClassMember::PrivateMethod(method)
                    if !matches!(method.kind, MethodKind::Setter) =>
                {
                    let member_name = format!("#{}", method.key.name);
                    let ir =
                        self.function(&method.function, table(method.is_static), module, &none);
                    file.functions
                        .insert(key(&member_name, method.is_static), ir);
                }
                ClassMember::ClassProp(prop) => {
                    let (Some(member_name), Some(init)) = (prop_name(&prop.key), &prop.value)
                    else {
                        continue;
                    };
                    let ir = match &**init {
                        Expr::Arrow(arrow) => {
                            self.arrow(arrow, table(prop.is_static), module, &none)
                        }
                        Expr::Fn(fn_expr) => {
                            self.function(&fn_expr.function, table(prop.is_static), module, &none)
                        }
                        _ => continue,
                    };
                    file.functions.insert(key(&member_name, prop.is_static), ir);
                }
                _ => {}
            }
        }
    }

    /// Each field the class assigns exactly once, in its constructor's own
    /// statements or its initialiser, and nowhere else. A constructor
    /// parameter read into a field is opaque: nothing here follows `new
    /// ApiClient(config)` back to the value it was built with.
    ///
    /// Anywhere else is anywhere: a method, a branch or a closure inside the
    /// constructor, and a class in this file that extends this one and
    /// declares or writes the field again (`redeclared`), since on its
    /// instances the base class's methods read the subclass's value.
    ///
    /// A field assigned that way from a library client's factory call holds
    /// that client's instance (carrick#1564).
    fn class_fields(
        &self,
        class: &Class,
        module: &ModuleScope,
        redeclared: Option<&HashSet<String>>,
    ) -> ClassFields {
        let mut fields: HashMap<String, Value> = HashMap::new();
        let mut receivers: HashMap<String, ClientRef> = HashMap::new();
        let mut contested: HashSet<String> = HashSet::new();
        let mut nested_writes: HashSet<String> = HashSet::new();
        let mut assign = |name: String,
                          value: Value,
                          client: Option<ClientRef>,
                          fields: &mut HashMap<String, Value>| {
            if let Some(client) = client {
                receivers.insert(name.clone(), client);
            }
            if fields.insert(name.clone(), value).is_some() {
                contested.insert(name);
            }
        };

        for member in &class.body {
            match member {
                ClassMember::ClassProp(prop) if !prop.is_static => {
                    if let (Some(name), Some(init)) = (prop_name(&prop.key), &prop.value)
                        && !matches!(&**init, Expr::Arrow(_) | Expr::Fn(_))
                    {
                        let scope = Scope::module(module);
                        let value = self.eval(init, &scope);
                        let client = self.factory_call(init, &scope);
                        assign(name, value, client, &mut fields);
                    }
                }
                ClassMember::PrivateProp(prop) if !prop.is_static => {
                    if let Some(init) = &prop.value {
                        let scope = Scope::module(module);
                        let value = self.eval(init, &scope);
                        let client = self.factory_call(init, &scope);
                        assign(format!("#{}", prop.key.name), value, client, &mut fields);
                    }
                }
                ClassMember::Constructor(ctor) => {
                    let mut scope = Scope::module(module);
                    for param in &ctor.params {
                        match param {
                            ParamOrTsParamProp::TsParamProp(prop) => {
                                if let TsParamPropParam::Ident(ident) = &prop.param {
                                    let name = ident.id.sym.to_string();
                                    assign(name.clone(), Value::opaque(name), None, &mut fields);
                                }
                            }
                            ParamOrTsParamProp::Param(_) => {}
                        }
                    }
                    let Some(body) = &ctor.body else {
                        continue;
                    };
                    // Constructor parameters stay opaque: they are read by
                    // name, not as positions a call site could fill.
                    //
                    // Only a statement of the constructor's own body is a
                    // write the field table reads; one inside a branch, a
                    // loop or a closure may or may not run, and is contested
                    // like a write from a method.
                    let mut nested = ThisWrites::default();
                    for stmt in &body.stmts {
                        if let Stmt::Decl(Decl::Var(var)) = stmt {
                            for declarator in &var.decls {
                                if let (Pat::Ident(ident), Some(init)) =
                                    (&declarator.name, &declarator.init)
                                {
                                    let value = self.eval(init, &scope);
                                    scope.locals.insert(ident.id.sym.to_string(), value);
                                }
                            }
                        }
                        match this_assignment(stmt) {
                            Some((name, value)) => {
                                value.visit_with(&mut nested);
                                let client = self.factory_call(value, &scope);
                                let value = self.eval(value, &scope);
                                assign(name, value, client, &mut fields);
                            }
                            None => stmt.visit_with(&mut nested),
                        }
                    }
                    nested_writes.extend(nested.fields);
                }
                _ => {}
            }
        }
        contested.extend(nested_writes);

        // A field written outside the constructor holds whatever the last
        // writer put there.
        let mut writes = ThisWrites::default();
        for member in &class.body {
            match member {
                ClassMember::Method(method) => method.function.visit_with(&mut writes),
                ClassMember::PrivateMethod(method) => method.function.visit_with(&mut writes),
                ClassMember::ClassProp(prop) => prop.value.visit_with(&mut writes),
                ClassMember::PrivateProp(prop) => prop.value.visit_with(&mut writes),
                ClassMember::StaticBlock(block) => block.visit_with(&mut writes),
                _ => {}
            }
        }
        contested.extend(writes.fields);
        contested.extend(redeclared.into_iter().flatten().cloned());
        for name in contested {
            receivers.remove(&name);
            fields.insert(name.clone(), Value::opaque(format!("this.{name}")));
        }
        let receivers = receivers
            .into_iter()
            .map(|(name, client)| {
                let client = module.with_writes(client, &format!("this.{name}"));
                (name, client)
            })
            .collect();
        // A field holding only a value the class is handed (`this.baseUrl =
        // baseUrl`, a parameter property) is written the way the request
        // writes it; one the class BUILDS is read through.
        let values = fields
            .into_iter()
            .map(|(name, value)| {
                let value = bound(format!("this.{name}"), value);
                (name, value)
            })
            .collect();
        ClassFields { values, receivers }
    }

    fn function(
        &self,
        function: &Function,
        fields: Option<&ClassFields>,
        module: &ModuleScope,
        captured: &Captured,
    ) -> FnIr {
        let params = function.params.iter().map(|p| pat_name(&p.pat)).collect();
        let mut scope = Scope {
            params,
            locals: captured.values.clone(),
            local_receivers: inherited_receivers(captured, function),
            fields,
            module,
        };
        let mut ir = FnIr::default();
        match &function.body {
            Some(body) => self.body(&body.stmts, &mut scope, &mut ir),
            None => ir.bodyless = true,
        }
        ir
    }

    fn arrow(
        &self,
        arrow: &ArrowExpr,
        fields: Option<&ClassFields>,
        module: &ModuleScope,
        captured: &Captured,
    ) -> FnIr {
        let params = arrow.params.iter().map(pat_name).collect();
        let mut scope = Scope {
            params,
            locals: captured.values.clone(),
            local_receivers: inherited_receivers(captured, arrow),
            fields,
            module,
        };
        let mut ir = FnIr::default();
        match &*arrow.body {
            BlockStmtOrExpr::BlockStmt(block) => self.body(&block.stmts, &mut scope, &mut ir),
            BlockStmtOrExpr::Expr(expr) => self.walk(expr, &scope, &mut ir),
        }
        ir
    }

    /// A function body, statement by statement, so a constant is readable
    /// by the statements after it. A binding assigned again anywhere in the
    /// body is opaque everywhere.
    ///
    /// A constant built by a library client's factory call holds that
    /// client's instance (carrick#1564), unless the body declares its name a
    /// second time somewhere, where nothing here says which one a use means.
    fn body(&self, stmts: &[Stmt], scope: &mut Scope<'_>, ir: &mut FnIr) {
        let mut reassigned = Reassigned::default();
        for stmt in stmts {
            stmt.visit_with(&mut reassigned);
        }
        let mut declared: Option<DeclaredNames> = None;
        for stmt in stmts {
            if let Stmt::Decl(Decl::Var(var)) = stmt {
                for declarator in &var.decls {
                    if let Some(init) = &declarator.init {
                        self.walk(init, scope, ir);
                    }
                    if let (Pat::Ident(ident), Some(init)) = (&declarator.name, &declarator.init) {
                        let name = ident.id.sym.to_string();
                        let client = if reassigned.names.contains(&name) {
                            None
                        } else {
                            self.factory_call(init, scope)
                        };
                        let value = if reassigned.names.contains(&name) {
                            Value::opaque(name.clone())
                        } else {
                            bound(name.clone(), self.eval(init, scope))
                        };
                        scope.locals.insert(name.clone(), value);
                        scope.local_receivers.remove(&name);
                        if let Some(client) = client {
                            let declared = declared.get_or_insert_with(|| {
                                let mut names = DeclaredNames::default();
                                for stmt in stmts {
                                    stmt.visit_with(&mut names);
                                }
                                names
                            });
                            if declared.counts.get(&name) == Some(&1) {
                                let client = scope.module.with_writes(client, &name);
                                scope.local_receivers.insert(name, client);
                            }
                        }
                    }
                }
                continue;
            }
            self.walk(stmt, scope, ir);
        }
    }

    /// Every call under `node`, stopping at nested functions: a function
    /// expression passed to a call is read as that call's callback, and any
    /// other nested function belongs to whoever calls it.
    fn walk<N>(&self, node: &N, scope: &Scope<'_>, ir: &mut FnIr)
    where
        N: for<'r, 's, 'i> VisitWith<CallWalker<'r, 's, 'i>>,
    {
        let mut walker = CallWalker {
            reader: self,
            scope,
            ir,
        };
        node.visit_with(&mut walker);
    }

    /// The value of an expression, as far as the source states it.
    fn eval(&self, expr: &Expr, scope: &Scope<'_>) -> Value {
        match expr {
            Expr::Lit(Lit::Str(s)) => Value::Str(vec![Piece::Lit(s.value.to_string())]),
            Expr::Lit(Lit::Num(n)) => Value::Str(vec![Piece::Lit(n.value.to_string())]),
            Expr::Tpl(tpl) => Value::Str(self.template(tpl, scope)),
            Expr::Bin(bin) if bin.op == BinaryOp::Add => {
                let left = self.eval(&bin.left, scope);
                let right = self.eval(&bin.right, scope);
                match (&left, &right) {
                    (Value::Str(_), Value::Str(_)) => {
                        Value::Str(concat([left.pieces(), right.pieces()]))
                    }
                    _ => Value::opaque(self.text(bin.span)),
                }
            }
            // `process.env.URL ?? "http://localhost:3000"`: the left side is
            // the value the deployment supplies; the fallback is a default,
            // not the target.
            Expr::Bin(bin)
                if matches!(bin.op, BinaryOp::NullishCoalescing | BinaryOp::LogicalOr) =>
            {
                match self.eval(&bin.left, scope) {
                    left @ Value::Str(_) => left,
                    _ => Value::opaque(self.text(bin.span)),
                }
            }
            Expr::Ident(ident) => {
                let name = ident.sym.as_ref();
                if let Some(index) = scope.param_index(name) {
                    return Value::Str(vec![Piece::Param(index, name.to_string())]);
                }
                if let Some(value) = scope
                    .locals
                    .get(name)
                    .or_else(|| scope.module.consts.get(name))
                {
                    return value.clone();
                }
                Value::opaque(name.to_string())
            }
            Expr::Member(member) => self.member(member, scope),
            Expr::Object(obj) => Value::Obj(self.object(obj, scope)),
            Expr::Call(call) => {
                // `JSON.stringify(body)` is the body it serialises.
                if is_member_call(call, "JSON", "stringify")
                    && let Some(arg) = call.args.first()
                    && arg.spread.is_none()
                {
                    return self.eval(&arg.expr, scope);
                }
                // `url.toString()` is the URL.
                if let Callee::Expr(callee) = &call.callee
                    && let Expr::Member(member) = &**callee
                    && member_prop(member).as_deref() == Some("toString")
                    && call.args.is_empty()
                {
                    let inner = self.eval(&member.obj, scope);
                    if matches!(inner, Value::Str(_)) {
                        return inner;
                    }
                }
                Value::opaque(self.text(call.span))
            }
            Expr::New(new) => self.new_url(new, scope),
            Expr::Paren(paren) => self.eval(&paren.expr, scope),
            Expr::TsAs(e) => self.eval(&e.expr, scope),
            Expr::TsNonNull(e) => self.eval(&e.expr, scope),
            Expr::TsConstAssertion(e) => self.eval(&e.expr, scope),
            Expr::TsSatisfies(e) => self.eval(&e.expr, scope),
            Expr::TsTypeAssertion(e) => self.eval(&e.expr, scope),
            Expr::Await(e) => self.eval(&e.arg, scope),
            _ => Value::opaque(self.text(expr.span())),
        }
    }

    fn template(&self, tpl: &Tpl, scope: &Scope<'_>) -> Vec<Piece> {
        let mut parts: Vec<Vec<Piece>> = Vec::new();
        for (index, quasi) in tpl.quasis.iter().enumerate() {
            let text = quasi
                .cooked
                .as_ref()
                .map(|cooked| cooked.to_string())
                .unwrap_or_else(|| quasi.raw.to_string());
            parts.push(vec![Piece::Lit(text)]);
            if let Some(expr) = tpl.exprs.get(index) {
                parts.push(self.eval(expr, scope).pieces());
            }
        }
        concat(parts)
    }

    fn member(&self, member: &MemberExpr, scope: &Scope<'_>) -> Value {
        // `this.field`, read through the class's field table.
        if let Expr::This(_) = &*member.obj {
            let name = match &member.prop {
                MemberProp::Ident(ident) => Some(ident.sym.to_string()),
                MemberProp::PrivateName(private) => Some(format!("#{}", private.name)),
                MemberProp::Computed(_) => None,
            };
            if let Some(name) = name {
                if let Some(value) = scope.fields.and_then(|fields| fields.values.get(&name)) {
                    return value.clone();
                }
                return Value::opaque(format!("this.{name}"));
            }
        }
        // `url.href` on a URL value is the URL.
        if member_prop(member).as_deref() == Some("href") {
            let inner = self.eval(&member.obj, scope);
            if matches!(inner, Value::Str(_)) {
                return inner;
            }
        }
        // A key of an object the source writes as a literal.
        if let Some(key) = member_prop(member)
            && let Value::Obj(obj) = self.eval(&member.obj, scope)
            && let Some(value) = obj.fields.get(&key)
        {
            return value.clone();
        }
        Value::opaque(self.text(member.span))
    }

    /// `new URL(path, base)`: the base is opaque and leads, the path follows.
    fn new_url(&self, new: &NewExpr, scope: &Scope<'_>) -> Value {
        let is_url = matches!(&*new.callee, Expr::Ident(ident) if ident.sym == *"URL");
        let args = new.args.as_deref().unwrap_or_default();
        if !is_url || args.iter().any(|arg| arg.spread.is_some()) {
            return Value::opaque(self.text(new.span));
        }
        match args {
            [path] => self.eval(&path.expr, scope),
            // Only the path is asserted, as everywhere a `new URL` is read
            // (carrick#610): the base is an opaque value the URL resolves
            // against, and a path starting with `/` replaces whatever path it
            // carries.
            [path, _base] => {
                let path = self.eval(&path.expr, scope).pieces();
                if !matches!(path.first(), Some(Piece::Lit(text)) if text.starts_with('/')) {
                    return Value::opaque(self.text(new.span));
                }
                Value::Str(path)
            }
            _ => Value::opaque(self.text(new.span)),
        }
    }

    /// An object literal's keys, in source order, so a later entry overwrites
    /// an earlier one exactly as the runtime does.
    ///
    /// A spread, or a computed key, can overwrite what came before it, so a
    /// key written earlier is kept only where the source says what the later
    /// entry holds. A spread whose possible keys the source states (an object
    /// literal, or a condition choosing between them) overwrites exactly those
    /// keys; any other spread, and any computed key, may overwrite every one,
    /// and the object is open. A key written after a spread is known.
    fn object(&self, obj: &ObjectLit, scope: &Scope<'_>) -> ObjValue {
        let mut value = ObjValue::default();
        for prop in &obj.props {
            let prop = match prop {
                PropOrSpread::Spread(spread) => {
                    match self.spread_branches(&spread.expr, scope) {
                        Some(branches) => value.spread(&branches),
                        None => {
                            value.fields.clear();
                            value.open = true;
                        }
                    }
                    continue;
                }
                PropOrSpread::Prop(prop) => prop,
            };
            match &**prop {
                Prop::KeyValue(kv) => match prop_name(&kv.key) {
                    Some(key) => {
                        value.fields.insert(key, self.eval(&kv.value, scope));
                    }
                    None => {
                        value.fields.clear();
                        value.open = true;
                    }
                },
                Prop::Shorthand(ident) => {
                    let expr = Expr::Ident(ident.clone());
                    value
                        .fields
                        .insert(ident.sym.to_string(), self.eval(&expr, scope));
                }
                // A method, a getter or a setter: its key holds no value this
                // pass reads.
                other => {
                    match other {
                        Prop::Method(MethodProp { key, .. })
                        | Prop::Getter(GetterProp { key, .. })
                        | Prop::Setter(SetterProp { key, .. }) => match prop_name(key) {
                            Some(name) => {
                                value.fields.remove(&name);
                            }
                            None => value.fields.clear(),
                        },
                        _ => {}
                    }
                    value.open = true;
                }
            }
        }
        value
    }

    /// The objects a spread may put in place, when the source states every
    /// key each one can carry: an object literal, a `cond ? {…} : {…}`
    /// choice, `cond && {…}`, or a constant holding one. `None` for anything
    /// else, which may carry any key.
    fn spread_branches(&self, expr: &Expr, scope: &Scope<'_>) -> Option<Vec<ObjValue>> {
        let known = |value: ObjValue| (!value.open).then_some(vec![value]);
        match expr {
            Expr::Paren(paren) => self.spread_branches(&paren.expr, scope),
            Expr::TsAs(e) => self.spread_branches(&e.expr, scope),
            Expr::TsSatisfies(e) => self.spread_branches(&e.expr, scope),
            Expr::TsNonNull(e) => self.spread_branches(&e.expr, scope),
            Expr::Object(obj) => known(self.object(obj, scope)),
            Expr::Cond(cond) => {
                let mut branches = self.spread_branches(&cond.cons, scope)?;
                branches.extend(self.spread_branches(&cond.alt, scope)?);
                Some(branches)
            }
            // Spreading a falsy left side adds nothing.
            Expr::Bin(bin) if bin.op == BinaryOp::LogicalAnd => {
                let mut branches = self.spread_branches(&bin.right, scope)?;
                branches.push(ObjValue::default());
                Some(branches)
            }
            other => match self.eval(other, scope) {
                Value::Obj(value) => known(value),
                _ => None,
            },
        }
    }

    /// Whether `call` is shaped like a request, and what it sends if so.
    fn request_shape(
        &self,
        call: &CallExpr,
        args: &[Value],
        scope: &Scope<'_>,
    ) -> Option<RequestShape> {
        let Callee::Expr(callee) = &call.callee else {
            return None;
        };
        let is_fetch = match &**callee {
            // Only the global: a parameter or a local named `fetch` is
            // someone's own function.
            Expr::Ident(ident) => {
                ident.sym == *"fetch"
                    && scope.param_index("fetch").is_none()
                    && !scope.locals.contains_key("fetch")
            }
            Expr::Member(member) => {
                member_prop(member).as_deref() == Some("fetch")
                    && matches!(&*member.obj, Expr::Ident(obj)
                        if obj.sym == *"window" || obj.sym == *"globalThis")
            }
            _ => false,
        };
        let verb = match &**callee {
            Expr::Member(member) => verb_from_callee_property(member_prop(member).as_deref())
                .filter(|_| !self.is_builtin_collection(&member.obj, scope)),
            _ => None,
        };

        let mut bag: Option<(usize, &ObjValue)> = None;
        for (index, arg) in call.args.iter().enumerate() {
            if let Expr::Object(obj) = &*arg.expr
                && is_request_options(obj)
            {
                if bag.is_some() {
                    return None;
                }
                if let Some(Value::Obj(value)) = args.get(index) {
                    bag = Some((index, value));
                }
            }
        }
        let kind = if is_fetch {
            RequestKind::Fetch
        } else if bag.is_some() {
            RequestKind::Bag
        } else if verb.is_some() {
            // A route registration hands over its handler; a request does not.
            if args.iter().any(|arg| matches!(arg, Value::Callback(_))) {
                return None;
            }
            RequestKind::Verb
        } else {
            return None;
        };

        let url_expr = match kind {
            RequestKind::Fetch | RequestKind::Verb => call.args.first().map(|arg| &*arg.expr),
            // A library reading is built from the client's claims
            // ([`library_shape`]), never from the call's written shape.
            RequestKind::Bag | RequestKind::Library => None,
        };
        let url_inline = url_expr.is_some_and(|expr| {
            matches!(
                expr.unwrap_parens(),
                Expr::Lit(Lit::Str(_)) | Expr::Tpl(_) | Expr::Bin(_)
            )
        });
        let url = match kind {
            RequestKind::Fetch => args.first()?.pieces(),
            RequestKind::Verb => match args.first()? {
                Value::Str(pieces) if is_route_shaped(pieces) => pieces.clone(),
                _ => return None,
            },
            RequestKind::Bag => match bag.and_then(|(_, obj)| obj.fields.get("url")) {
                Some(Value::Str(url)) => url.clone(),
                _ => {
                    let mut routes = args
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| bag.is_none_or(|(bag_index, _)| bag_index != *index))
                        .filter_map(|(_, value)| match value {
                            Value::Str(pieces) if is_route_shaped(pieces) => Some(pieces.clone()),
                            _ => None,
                        });
                    let url = routes.next()?;
                    if routes.next().is_some() {
                        return None;
                    }
                    url
                }
            },
            RequestKind::Library => return None,
        };

        // `fetch(url, init)` with `init` the function's own parameter: the
        // caller writes the options, and with them the verb and the body.
        let options_param = match (kind, bag, args.get(1)) {
            (RequestKind::Fetch, None, Some(Value::Str(pieces))) => match pieces.as_slice() {
                [Piece::Param(index, _)] => Some(*index),
                _ => None,
            },
            _ => None,
        };

        let method = match bag.map(|(_, obj)| obj) {
            _ if options_param.is_some() => {
                MethodValue::ParamKey(options_param.unwrap_or_default(), "method".to_string())
            }
            Some(obj) => match obj.fields.get("method") {
                Some(Value::Str(pieces)) => method_of(pieces),
                Some(_) => MethodValue::Unknown,
                None if verb.is_some() => MethodValue::Lit(verb.clone().unwrap_or_default()),
                // A bag stating no method and carrying no body is a GET, the
                // default every fetch-shaped client applies. A body with no
                // method is a verb someone else supplies, and so may be a
                // method a spread or a computed key puts in the bag.
                None if obj.open
                    || obj.fields.contains_key("body")
                    || obj.fields.contains_key("data") =>
                {
                    MethodValue::Unknown
                }
                None => MethodValue::Lit("GET".to_string()),
            },
            None => match &verb {
                Some(verb) => MethodValue::Lit(verb.clone()),
                // `fetch(url, { ...init })`: the options are an object whose
                // method a spread may set.
                None if kind == RequestKind::Fetch
                    && matches!(args.get(1), Some(Value::Obj(options)) if options.open) =>
                {
                    MethodValue::Unknown
                }
                None => MethodValue::Lit("GET".to_string()),
            },
        };

        let body = match bag.map(|(_, obj)| obj) {
            _ if options_param.is_some() => {
                BodyValue::ParamKey(options_param.unwrap_or_default(), "body".to_string())
            }
            Some(obj) => obj
                .fields
                .get("body")
                .or_else(|| obj.fields.get("data"))
                .map(body_of)
                .unwrap_or_default(),
            // `client.post(url, body)`: the argument after the URL.
            None if kind == RequestKind::Verb => args.get(1).map(body_of).unwrap_or_default(),
            None => BodyValue::Unstated,
        };

        Some(RequestShape {
            kind,
            method,
            url,
            body,
            // A bag's URL is inline unless it reached the call through a
            // binding; its URL argument is one of several, so only a whole
            // literal argument counts.
            url_inline: url_inline
                || (kind == RequestKind::Bag
                    && call.args.iter().any(|arg| {
                        matches!(
                            arg.expr.unwrap_parens(),
                            Expr::Lit(Lit::Str(_)) | Expr::Tpl(_)
                        )
                    })),
            base: Vec::new(),
            semantics: BTreeSet::new(),
        })
    }

    /// `<client>.<factory>({ ... })`, where `<client>` is a binding imported
    /// from a package (carrick#1564): the instance it builds, with the object
    /// literal it was handed. Anything else — an instance's own factory call,
    /// options that are not written as one literal — builds nothing this pass
    /// reads.
    fn factory_call(&self, expr: &Expr, scope: &Scope<'_>) -> Option<ClientRef> {
        let expr = match expr {
            Expr::TsAs(e) => &*e.expr,
            Expr::TsNonNull(e) => &*e.expr,
            Expr::TsSatisfies(e) => &*e.expr,
            other => other,
        };
        let Expr::Call(call) = expr.unwrap_parens() else {
            return None;
        };
        let Callee::Expr(callee) = &call.callee else {
            return None;
        };
        let Expr::Member(member) = &**callee else {
            return None;
        };
        let Expr::Ident(binding) = &*member.obj else {
            return None;
        };
        let factory = member_prop(member)?;
        let client = scope.receiver(binding.sym.as_ref())?;
        if client.instance.is_some() {
            return None;
        }
        let [options] = call.args.as_slice() else {
            return None;
        };
        let Expr::Object(options_literal) = &*options.expr else {
            return None;
        };
        if options.spread.is_some() {
            return None;
        }
        Some(ClientRef {
            package: client.package.clone(),
            export: client.export.clone(),
            instance: Some(ClientInstance {
                factory,
                options: self.object(options_literal, scope),
            }),
            // Filled in by the caller, which knows the binding it goes to.
            assigned: BTreeSet::new(),
        })
    }

    /// The library client a call is made through, when the syntax names one
    /// (carrick#1564): `client.member(...)`, `client(...)`, and the same on a
    /// class field (`this.api.member(...)`, `this.api(...)`).
    fn call_receiver(&self, callee: &Expr, scope: &Scope<'_>) -> Option<CallReceiver> {
        match callee.unwrap_parens() {
            Expr::Ident(ident) => Some(CallReceiver {
                client: scope.receiver(ident.sym.as_ref())?.clone(),
                member: None,
            }),
            Expr::Member(member) => {
                if let Expr::This(_) = &*member.obj {
                    return Some(CallReceiver {
                        client: field_receiver(member, scope)?.clone(),
                        member: None,
                    });
                }
                let member_name = member_prop(member)?;
                let client = match member.obj.unwrap_parens() {
                    Expr::Ident(ident) => scope.receiver(ident.sym.as_ref())?,
                    Expr::Member(field) if matches!(&*field.obj, Expr::This(_)) => {
                        field_receiver(field, scope)?
                    }
                    _ => return None,
                };
                Some(CallReceiver {
                    client: client.clone(),
                    member: Some(member_name),
                })
            }
            _ => None,
        }
    }

    /// A receiver the source builds as one of the language's own keyed
    /// collections, whose `get` and `delete` send nothing.
    fn is_builtin_collection(&self, receiver: &Expr, scope: &Scope<'_>) -> bool {
        const COLLECTIONS: &[&str] = &[
            "Map",
            "Set",
            "WeakMap",
            "URLSearchParams",
            "Headers",
            "FormData",
        ];
        let Value::Str(pieces) = self.eval(receiver, scope) else {
            return false;
        };
        let [Piece::Opaque(text)] = pieces.as_slice() else {
            return false;
        };
        let Some(constructed) = text.strip_prefix("new ") else {
            return false;
        };
        // `new Map()` and `new Map<string, User>()` alike.
        let name = constructed
            .split(['(', '<'])
            .next()
            .unwrap_or_default()
            .trim();
        COLLECTIONS.contains(&name)
    }
}

/// The value a binding holds, as a URL writes it.
///
/// A binding the source BUILDS — a literal path around a base — is read
/// through, which is the point of reading it. One holding only a value the
/// source does not state (`encodeURIComponent(name)`, a constructor argument)
/// says nothing its own name does not, and is written by that name, the way
/// the request itself writes it. An environment read and a construction keep
/// their text: the first names where the value comes from, the second what the
/// binding is.
fn bound(name: String, value: Value) -> Value {
    let Value::Str(pieces) = &value else {
        return value;
    };
    if pieces.iter().any(|piece| matches!(piece, Piece::Lit(_))) {
        return value;
    }
    match pieces.as_slice() {
        [Piece::Param(..)] => value,
        [Piece::Opaque(text)]
            if text.starts_with("new ")
                || text.starts_with("process.env.")
                || text.starts_with("import.meta.env.")
                || text.starts_with("Deno.env.") =>
        {
            value
        }
        _ => Value::opaque(name),
    }
}

/// The body a value states, where it stands for one.
fn body_of(value: &Value) -> BodyValue {
    match value {
        Value::Obj(body) => BodyValue::Fields(body.clone()),
        Value::Str(pieces) => match pieces.as_slice() {
            [Piece::Param(index, _)] => BodyValue::Param(*index),
            _ => BodyValue::Unstated,
        },
        Value::Callback(_) => BodyValue::Unstated,
    }
}

/// The library client a class field holds (`this.api`), when it holds one.
fn field_receiver<'s>(member: &MemberExpr, scope: &'s Scope<'_>) -> Option<&'s ClientRef> {
    let name = match &member.prop {
        MemberProp::Ident(ident) => ident.sym.to_string(),
        MemberProp::PrivateName(private) => format!("#{}", private.name),
        MemberProp::Computed(_) => return None,
    };
    scope.fields?.receivers.get(&name)
}

/// The library instances a function written inside another can read: the
/// enclosing function's, less any name it declares again itself.
fn inherited_receivers<N>(captured: &Captured, node: &N) -> HashMap<String, ClientRef>
where
    N: VisitWith<DeclaredNames>,
{
    if captured.receivers.is_empty() {
        return HashMap::new();
    }
    let mut declared = DeclaredNames::default();
    node.visit_with(&mut declared);
    captured
        .receivers
        .iter()
        .filter(|(name, _)| !declared.names.contains(*name))
        .map(|(name, client)| (name.clone(), client.clone()))
        .collect()
}

fn method_of(pieces: &[Piece]) -> MethodValue {
    match pieces {
        [Piece::Lit(method)] if is_http_method(method) => MethodValue::Lit(method.to_uppercase()),
        [Piece::Param(index, _)] => MethodValue::Param(*index),
        _ => MethodValue::Unknown,
    }
}

/// Whether a URL value says anything route-like: a literal piece holding a
/// `/`, or a value this pass read out of a binding that does.
fn is_route_shaped(pieces: &[Piece]) -> bool {
    pieces.iter().any(|piece| match piece {
        Piece::Lit(text) => text.contains('/'),
        _ => false,
    })
}

struct CallWalker<'r, 's, 'i> {
    reader: &'r Reader<'r>,
    scope: &'s Scope<'s>,
    ir: &'i mut FnIr,
}

impl Visit for CallWalker<'_, '_, '_> {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        // `produce()`: the body invokes a parameter.
        let invokes_param = match &call.callee {
            Callee::Expr(callee) => match &**callee {
                Expr::Ident(ident) => self.scope.param_index(ident.sym.as_ref()),
                _ => None,
            },
            _ => None,
        };
        if let Some(index) = invokes_param {
            self.ir.invoked_params.insert(index);
        }

        if matches!(call.callee, Callee::Import(_)) {
            self.ir.unfollowed = true;
        }
        let mut args: Vec<Value> = Vec::with_capacity(call.args.len());
        for arg in &call.args {
            let value = match &*arg.expr {
                Expr::Arrow(arrow) => {
                    let nested = self.reader.arrow(
                        arrow,
                        self.scope.fields,
                        self.scope.module,
                        &self.captured(),
                    );
                    self.ir.nested.push(nested);
                    Value::Callback(self.ir.nested.len() - 1)
                }
                Expr::Fn(fn_expr) => {
                    // A `function` expression binds its own `this`.
                    let nested = self.reader.function(
                        &fn_expr.function,
                        None,
                        self.scope.module,
                        &self.captured(),
                    );
                    self.ir.nested.push(nested);
                    Value::Callback(self.ir.nested.len() - 1)
                }
                expr => {
                    expr.visit_with(self);
                    self.reader.eval(expr, self.scope)
                }
            };
            args.push(value);
        }
        if let Callee::Expr(callee) = &call.callee {
            callee.visit_with(self);
        }

        let spread = call.args.iter().any(|arg| arg.spread.is_some());
        let request = if spread {
            None
        } else {
            self.reader.request_shape(call, &args, self.scope)
        };
        // A spread argument moves every position after it, and the claims
        // say what a position means.
        let receiver = match &call.callee {
            Callee::Expr(callee) if !spread => self.reader.call_receiver(callee, self.scope),
            _ => None,
        };
        let inert = match &call.callee {
            Callee::Expr(callee) => match &**callee {
                Expr::Member(member) => matches!(&*member.obj, Expr::Ident(obj)
                    if INERT_GLOBALS.contains(&obj.sym.as_ref())),
                _ => false,
            },
            _ => false,
        };
        let callee = match &call.callee {
            Callee::Expr(callee) => match &**callee {
                Expr::Ident(ident) => ident.sym.to_string(),
                Expr::Member(member) => member_prop(member).unwrap_or_default(),
                _ => String::new(),
            },
            _ => String::new(),
        };
        self.ir.calls.push(CallIr {
            site: self.reader.site(call.span),
            callee,
            args,
            request,
            inert,
            invokes_param,
            receiver,
        });
    }

    // A function written in the body and not handed to a call: its calls
    // state rows where they are written, and what it sends is not this
    // body's.
    fn visit_function(&mut self, function: &Function) {
        let detached = self
            .reader
            .function(function, None, self.scope.module, &self.captured());
        self.ir.detached.push(detached);
    }

    fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
        let detached = self.reader.arrow(
            arrow,
            self.scope.fields,
            self.scope.module,
            &self.captured(),
        );
        self.ir.detached.push(detached);
    }

    fn visit_class(&mut self, _: &Class) {}

    fn visit_new_expr(&mut self, new: &NewExpr) {
        // The language's own constructors run no code of this service's.
        const BUILTINS: &[&str] = &[
            "Map",
            "Set",
            "WeakMap",
            "WeakSet",
            "URL",
            "URLSearchParams",
            "Headers",
            "FormData",
            "Date",
            "Error",
            "TypeError",
            "RangeError",
            "Promise",
            "RegExp",
            "Array",
            "Object",
            "AbortController",
            "TextEncoder",
            "TextDecoder",
            "Blob",
        ];
        let builtin = matches!(&*new.callee, Expr::Ident(ident)
            if BUILTINS.contains(&ident.sym.as_ref()));
        if !builtin {
            self.ir.unfollowed = true;
        }
        new.visit_children_with(self);
    }

    fn visit_opt_call(&mut self, call: &OptCall) {
        self.ir.unfollowed = true;
        call.visit_children_with(self);
    }

    fn visit_tagged_tpl(&mut self, tagged: &TaggedTpl) {
        self.ir.unfollowed = true;
        tagged.visit_children_with(self);
    }
}

impl CallWalker<'_, '_, '_> {
    /// What a callback written here can read: this body's constants, and its
    /// parameters as opaque names (the callback is not called with them), and
    /// the library instances this body holds.
    fn captured(&self) -> Captured {
        let mut values = self.scope.locals.clone();
        for name in self.scope.params.iter().flatten() {
            values.insert(name.clone(), Value::opaque(name.clone()));
        }
        Captured {
            values,
            receivers: self.scope.local_receivers.clone(),
        }
    }
}

/// Names assigned after their declaration, anywhere in a body.
#[derive(Default)]
struct Reassigned {
    names: HashSet<String>,
}

impl Visit for Reassigned {
    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        if let AssignTarget::Simple(SimpleAssignTarget::Ident(ident)) = &assign.left {
            self.names.insert(ident.id.sym.to_string());
        }
        assign.visit_children_with(self);
    }

    fn visit_update_expr(&mut self, update: &UpdateExpr) {
        if let Expr::Ident(ident) = &*update.arg {
            self.names.insert(ident.sym.to_string());
        }
        update.visit_children_with(self);
    }
}

/// `this.x` fields written anywhere a visitor is walked.
#[derive(Default)]
struct ThisWrites {
    fields: HashSet<String>,
}

impl Visit for ThisWrites {
    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        if let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = &assign.left
            && let Expr::This(_) = &*member.obj
        {
            match &member.prop {
                MemberProp::Ident(ident) => {
                    self.fields.insert(ident.sym.to_string());
                }
                MemberProp::PrivateName(private) => {
                    self.fields.insert(format!("#{}", private.name));
                }
                MemberProp::Computed(_) => {}
            }
        }
        assign.visit_children_with(self);
    }
}

/// `this.x = value;` as a statement.
fn this_assignment(stmt: &Stmt) -> Option<(String, &Expr)> {
    let Stmt::Expr(expr_stmt) = stmt else {
        return None;
    };
    let Expr::Assign(assign) = &*expr_stmt.expr else {
        return None;
    };
    if assign.op != AssignOp::Assign {
        return None;
    }
    let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = &assign.left else {
        return None;
    };
    if !matches!(&*member.obj, Expr::This(_)) {
        return None;
    }
    let name = match &member.prop {
        MemberProp::Ident(ident) => ident.sym.to_string(),
        MemberProp::PrivateName(private) => format!("#{}", private.name),
        MemberProp::Computed(_) => return None,
    };
    Some((name, &assign.right))
}

fn pat_name(pat: &Pat) -> Option<String> {
    match pat {
        Pat::Ident(ident) => Some(ident.id.sym.to_string()),
        Pat::Assign(assign) => pat_name(&assign.left),
        _ => None,
    }
}

fn prop_name(key: &PropName) -> Option<String> {
    match key {
        PropName::Ident(ident) => Some(ident.sym.to_string()),
        PropName::Str(s) => Some(s.value.to_string()),
        _ => None,
    }
}

fn member_prop(member: &MemberExpr) -> Option<String> {
    match &member.prop {
        MemberProp::Ident(ident) => Some(ident.sym.to_string()),
        _ => None,
    }
}

fn is_member_call(call: &CallExpr, object: &str, property: &str) -> bool {
    let Callee::Expr(callee) = &call.callee else {
        return false;
    };
    let Expr::Member(member) = &**callee else {
        return false;
    };
    matches!(&*member.obj, Expr::Ident(obj) if obj.sym == *object)
        && member_prop(member).as_deref() == Some(property)
}

// ---------------------------------------------------------------------------
// Composition
// ---------------------------------------------------------------------------

/// Where a request is written.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct RequestSite {
    file: PathBuf,
    line: u32,
}

/// One request a call to a function sends.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Effect {
    site: RequestSite,
    method: MethodValue,
    url: Vec<Piece>,
    /// The body's plain keys whose values are strings.
    body: BTreeMap<String, Vec<Piece>>,
    /// The function's own parameter the body is, or the key of it that holds
    /// the body, when the caller writes it.
    body_param: Option<(usize, Option<String>)>,
    /// A verb call: a request only when reached from another function.
    verb: bool,
    /// A library client's base, kept apart from `url` until the row is
    /// stated, so a path a caller fills in is joined as it is finally
    /// written (carrick#1564).
    base: Vec<Piece>,
    /// The library-semantics claim ids the request was read through.
    semantics: BTreeSet<String>,
}

impl Effect {
    fn from_shape(shape: &RequestShape, file: &Path, line: u32) -> Self {
        let (body, body_param) = match &shape.body {
            BodyValue::Fields(obj) => (string_fields(obj), None),
            BodyValue::Param(index) => (BTreeMap::new(), Some((*index, None))),
            BodyValue::ParamKey(index, key) => (BTreeMap::new(), Some((*index, Some(key.clone())))),
            BodyValue::Unstated => (BTreeMap::new(), None),
        };
        Effect {
            site: RequestSite {
                file: file.to_path_buf(),
                line,
            },
            method: shape.method.clone(),
            url: shape.url.clone(),
            body,
            body_param,
            verb: shape.kind == RequestKind::Verb,
            base: shape.base.clone(),
            semantics: shape.semantics.clone(),
        }
    }

    /// Whether any piece is still one of the summarised function's parameters.
    /// Whether the effect still waits on a caller: its method is a parameter,
    /// or a parameter supplies its URL's structure. A parameter standing for a
    /// whole path segment is a path parameter, not a hole: the request is
    /// stated where it is written, whatever value the segment takes.
    fn has_params(&self) -> bool {
        matches!(
            self.method,
            MethodValue::Param(_) | MethodValue::ParamKey(..)
        ) || self.url.iter().enumerate().any(|(position, piece)| {
            matches!(piece, Piece::Param(..)) && !is_whole_segment(&self.url, position)
        }) || self
            .base
            .iter()
            .any(|piece| matches!(piece, Piece::Param(..)))
    }

    /// This effect as seen from a call passing `args`.
    fn instantiate(&self, args: &[Value]) -> Effect {
        let fill = |pieces: &[Piece]| -> Vec<Piece> {
            concat(
                pieces
                    .iter()
                    .enumerate()
                    .map(|(position, piece)| match piece {
                        // A path parameter stays one, under the callee's own
                        // name: the caller's argument is its value, not the
                        // route's.
                        Piece::Param(_, name) if is_whole_segment(pieces, position) => {
                            vec![Piece::Opaque(name.clone())]
                        }
                        Piece::Param(index, name) => match args.get(*index) {
                            Some(Value::Str(arg)) => match arg.as_slice() {
                                // An opaque value standing for a path segment is a
                                // path parameter, and keeps the name the callee gives
                                // it. Leading the URL it is the base, and the caller's
                                // own expression is what says where the request goes.
                                [Piece::Opaque(_)] if position > 0 => {
                                    vec![Piece::Opaque(name.clone())]
                                }
                                _ => arg.clone(),
                            },
                            Some(other) => other.pieces(),
                            None => vec![Piece::Unknown],
                        },
                        other => vec![other.clone()],
                    }),
            )
        };
        let method = match &self.method {
            MethodValue::Param(index) => match args.get(*index) {
                Some(Value::Str(pieces)) => method_of(pieces),
                _ => MethodValue::Unknown,
            },
            MethodValue::ParamKey(index, key) => match args.get(*index) {
                Some(Value::Obj(obj)) => match obj.fields.get(key) {
                    Some(Value::Str(pieces)) => method_of(pieces),
                    Some(_) => MethodValue::Unknown,
                    // Options stating no method and no body: the GET every
                    // fetch-shaped client defaults to.
                    None if obj.fields.contains_key("body") || obj.open => MethodValue::Unknown,
                    None => MethodValue::Lit("GET".to_string()),
                },
                Some(Value::Str(pieces)) => match pieces.as_slice() {
                    [Piece::Param(outer, _)] => MethodValue::ParamKey(*outer, key.clone()),
                    _ => MethodValue::Unknown,
                },
                _ => MethodValue::Unknown,
            },
            other => other.clone(),
        };
        let (body, body_param) = match &self.body_param {
            Some((index, key)) => match (args.get(*index), key) {
                (Some(Value::Obj(obj)), None) => (string_fields(obj), None),
                (Some(Value::Obj(obj)), Some(key)) => match obj.fields.get(key) {
                    Some(Value::Obj(body)) => (string_fields(body), None),
                    Some(Value::Str(pieces)) => match pieces.as_slice() {
                        [Piece::Param(outer, _)] => (BTreeMap::new(), Some((*outer, None))),
                        _ => (BTreeMap::new(), None),
                    },
                    _ => (BTreeMap::new(), None),
                },
                (Some(Value::Str(pieces)), key) => match pieces.as_slice() {
                    [Piece::Param(outer, _)] => (BTreeMap::new(), Some((*outer, key.clone()))),
                    _ => (BTreeMap::new(), None),
                },
                _ => (BTreeMap::new(), None),
            },
            None => (
                self.body
                    .iter()
                    .map(|(key, pieces)| (key.clone(), fill(pieces)))
                    .collect(),
                None,
            ),
        };
        Effect {
            site: self.site.clone(),
            method,
            url: fill(&self.url),
            body,
            body_param,
            verb: self.verb,
            base: fill(&self.base),
            semantics: self.semantics.clone(),
        }
    }

    /// This effect as a call at `site` in `file` sends it. Where the call
    /// fills the last hole the callee left open, the call is where the request
    /// is stated, and it is the request line everything further up reaches.
    fn at_call(&self, args: &[Value], file: &Path, site: Site) -> Effect {
        let mut instantiated = self.instantiate(args);
        if self.has_params() && !instantiated.has_params() {
            instantiated.site = RequestSite {
                file: file.to_path_buf(),
                line: site.line,
            };
        }
        instantiated
    }

    /// This effect with every parameter left unknown: a callback's requests,
    /// whose arguments the callee supplies.
    fn detached(&self) -> Effect {
        self.instantiate(&[])
    }
}

/// An object literal's string-valued keys.
fn string_fields(obj: &ObjValue) -> BTreeMap<String, Vec<Piece>> {
    obj.fields
        .iter()
        .filter_map(|(key, value)| match value {
            Value::Str(pieces) => Some((key.clone(), pieces.clone())),
            _ => None,
        })
        .collect()
}

/// What a call to one function sends.
#[derive(Debug, Clone, Default)]
struct Summary {
    effects: BTreeSet<Effect>,
    /// Every call in the body resolved, every callee is complete, and every
    /// callback went to a callee that says what it does with it.
    complete: bool,
    /// Parameters the function invokes.
    invokes: BTreeSet<usize>,
}

/// A row this pass states at one call site.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SummaryRow {
    /// The name the call is made through.
    pub callee: String,
    pub span_start: u32,
    pub span_end: u32,
    pub line: u32,
    pub method: String,
    pub target: String,
    /// The request body's plain keys whose values are literals.
    pub body_literals: BTreeMap<String, String>,
    /// For a call through a declaration in another module: the request line
    /// it reaches, as `"<file>:<line>"`.
    pub reaches_request: Option<String>,
    /// The request primitive's own line (`fetch(...)` itself), where the
    /// passes that read a site's own source state it too and are kept.
    pub own_site: bool,
    /// The library-semantics claim ids the row was read through
    /// (carrick#1564), sorted. Empty on every other row.
    pub library_semantics: Vec<String>,
}

/// What the summaries are composed from, as discovery leaves it: every
/// file's functions and the call sites the call graph resolved. Held until
/// the service's library semantics are verified, which needs detection and
/// the service's sidecar (carrick#1564).
#[derive(Debug, Default)]
pub struct RequestSummaryInputs {
    pub files: HashMap<PathBuf, FileIr>,
    pub sites: CallSiteTargets,
}

/// Everything the pass states, per file (keyed by the file as walked).
#[derive(Debug, Default)]
pub struct RequestSummaryIndex {
    rows: HashMap<PathBuf, BTreeMap<u32, Vec<SummaryRow>>>,
    silent: HashMap<PathBuf, BTreeSet<u32>>,
    /// Requests this pass reached whose URL it could not state.
    pub undetermined: usize,
}

impl RequestSummaryIndex {
    /// Rows read through library semantics (carrick#1564).
    pub fn library_row_count(&self) -> usize {
        self.rows
            .values()
            .flat_map(|sites| sites.values().flatten())
            .filter(|row| !row.library_semantics.is_empty())
            .count()
    }

    /// The rows at each call site of `file`, keyed by span start.
    pub fn rows(&self, file: &Path) -> Option<&BTreeMap<u32, Vec<SummaryRow>>> {
        self.rows.get(file)
    }

    /// Call sites of `file` whose callee provably sends nothing, by span
    /// start.
    pub fn silent(&self, file: &Path) -> Option<&BTreeSet<u32>> {
        self.silent.get(file)
    }

    pub fn row_count(&self) -> usize {
        self.rows
            .values()
            .flat_map(|sites| sites.values())
            .map(Vec::len)
            .sum()
    }

    pub fn silent_count(&self) -> usize {
        self.silent.values().map(BTreeSet::len).sum()
    }
}

struct Composer<'a> {
    files: &'a HashMap<PathBuf, FileIr>,
    sites: &'a CallSiteTargets,
    semantics: &'a LibrarySemantics,
    memo: HashMap<(PathBuf, String), Summary>,
    in_progress: HashSet<(PathBuf, String)>,
}

impl Composer<'_> {
    fn summary(&mut self, file: &Path, key: &str) -> Option<Summary> {
        let id = (file.to_path_buf(), key.to_string());
        if let Some(summary) = self.memo.get(&id) {
            return Some(summary.clone());
        }
        let ir = self.files.get(file)?.functions.get(key)?;
        if self.in_progress.contains(&id) {
            // Recursion: what the cycle sends is not known yet, and a
            // function on it is never complete.
            return Some(Summary::default());
        }
        self.in_progress.insert(id.clone());
        let summary = self.compose(file, ir);
        self.in_progress.remove(&id);
        self.memo.insert(id, summary.clone());
        Some(summary)
    }

    /// The callee a call resolves to, when it is a function this pass read.
    fn callee(&mut self, file: &Path, call: &CallIr) -> Option<(PathBuf, Summary)> {
        let (target_file, key) = self.sites.target(file, call.site.lo)?.clone();
        let summary = self.summary(&target_file, &key)?;
        Some((target_file, summary))
    }

    fn compose(&mut self, file: &Path, ir: &FnIr) -> Summary {
        let mut summary = Summary {
            effects: BTreeSet::new(),
            complete: !ir.bodyless && !ir.unfollowed,
            invokes: ir.invoked_params.clone(),
        };
        for call in &ir.calls {
            if call.invokes_param.is_some() {
                continue;
            }
            match self.callee(file, call) {
                Some((_, callee)) => {
                    summary.complete &= callee.complete;
                    for effect in &callee.effects {
                        summary
                            .effects
                            .insert(effect.at_call(&call.args, file, call.site));
                    }
                    for (index, arg) in call.args.iter().enumerate() {
                        match arg {
                            Value::Callback(nested) if callee.invokes.contains(&index) => {
                                let callback = self.compose(file, &ir.nested[*nested]);
                                summary.complete &= callback.complete;
                                summary
                                    .effects
                                    .extend(callback.effects.iter().map(Effect::detached));
                            }
                            Value::Str(pieces) if callee.invokes.contains(&index) => {
                                if let [Piece::Param(param, _)] = pieces.as_slice() {
                                    summary.invokes.insert(*param);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                None => {
                    if let Some(request) = shape_of(call, self.semantics) {
                        summary
                            .effects
                            .insert(Effect::from_shape(&request, file, call.site.line));
                    } else if !call.inert {
                        summary.complete = false;
                    }
                    // A callback handed to a callee this pass cannot read may
                    // or may not run.
                    for arg in &call.args {
                        if let Value::Callback(nested) = arg {
                            let callback = self.compose(file, &ir.nested[*nested]);
                            if !callback.effects.is_empty() || !callback.complete {
                                summary.complete = false;
                            }
                        }
                    }
                }
            }
        }
        summary
    }
}

/// Compose every function's summary and state the rows it supports, reading
/// calls through library clients with the semantics the service verified
/// (carrick#1564; empty where it verified none).
pub fn summarize(
    inputs: &RequestSummaryInputs,
    semantics: &LibrarySemantics,
) -> RequestSummaryIndex {
    let files = &inputs.files;
    let mut composer = Composer {
        files,
        sites: &inputs.sites,
        semantics,
        memo: HashMap::new(),
        in_progress: HashSet::new(),
    };
    let mut index = RequestSummaryIndex::default();

    let mut paths: Vec<&PathBuf> = files.keys().collect();
    paths.sort();
    for path in paths {
        let mut keys: Vec<&String> = files[path].functions.keys().collect();
        keys.sort();
        for key in keys {
            let ir = &files[path].functions[key];
            emit(&mut composer, path, ir, &mut index);
        }
    }
    for sites in index.rows.values_mut() {
        for rows in sites.values_mut() {
            rows.sort();
            rows.dedup();
        }
    }
    index
}

fn emit(composer: &mut Composer<'_>, file: &Path, ir: &FnIr, index: &mut RequestSummaryIndex) {
    for call in &ir.calls {
        if call.invokes_param.is_some() {
            continue;
        }
        let mut rows: Vec<SummaryRow> = Vec::new();
        match composer.callee(file, call) {
            Some((callee_file, callee)) => {
                let mut sends = false;
                for effect in &callee.effects {
                    sends = true;
                    let instantiated = effect.instantiate(&call.args);
                    if instantiated.has_params() {
                        // Still open: a caller further up fills it.
                        continue;
                    }
                    let reaches = if effect.has_params() {
                        // The last hole is filled here: this site states the
                        // request. Not for a verb call, whose role nothing
                        // here states: a helper handed a path could as well be
                        // registering a route.
                        if effect.verb {
                            continue;
                        }
                        None
                    } else if effect.site.file != file && callee_file == effect.site.file {
                        // A call into the module that states the request: the
                        // call through its declaration. A call into any other
                        // module reaches the request through a function that
                        // module declares, and that module's own call into
                        // the client is the row; stating one at every hop up
                        // the chain would count one request once per caller.
                        Some(format!(
                            "{}:{}",
                            effect.site.file.display(),
                            effect.site.line
                        ))
                    } else {
                        // A hop inside one module: its own request line
                        // already states it.
                        continue;
                    };
                    match row(&instantiated, call, reaches, false) {
                        Some(row) => rows.push(row),
                        None => index.undetermined += 1,
                    }
                }
                for (arg_index, arg) in call.args.iter().enumerate() {
                    if let Value::Callback(nested) = arg
                        && callee.invokes.contains(&arg_index)
                    {
                        let callback = composer.compose(file, &ir.nested[*nested]);
                        sends |= !callback.effects.is_empty();
                    }
                }
                if callee.complete && !sends {
                    index
                        .silent
                        .entry(file.to_path_buf())
                        .or_default()
                        .insert(call.site.span_start);
                }
            }
            None => {
                // A call through a verified library client states its own
                // row, and claims the site as a summary does anywhere else:
                // the passes that read the site's own source cannot read the
                // client's base, so their reading is not the one to keep.
                if let Some(request) = shape_of(call, composer.semantics) {
                    let library = request.kind == RequestKind::Library;
                    if library || (request.kind != RequestKind::Verb && !request.url_inline) {
                        let effect = Effect::from_shape(&request, file, call.site.line);
                        if !effect.has_params() {
                            match row(&effect, call, None, !library) {
                                Some(row) => rows.push(row),
                                None => index.undetermined += 1,
                            }
                        }
                    }
                }
            }
        }
        if !rows.is_empty() {
            index
                .rows
                .entry(file.to_path_buf())
                .or_default()
                .entry(call.site.span_start)
                .or_default()
                .extend(rows);
        }
    }
    for nested in ir.nested.iter().chain(&ir.detached) {
        emit(composer, file, nested, index);
    }
}

/// The row an effect supports at `site`, when its URL and method are both
/// stated.
fn row(
    effect: &Effect,
    call: &CallIr,
    reaches_request: Option<String>,
    own_site: bool,
) -> Option<SummaryRow> {
    let site = call.site;
    let MethodValue::Lit(method) = &effect.method else {
        return None;
    };
    let target = render_target(&join_base(&effect.base, &effect.url))?;
    let body_literals = effect
        .body
        .iter()
        .filter_map(|(key, pieces)| match pieces.as_slice() {
            [Piece::Lit(value)] => Some((key.clone(), value.clone())),
            _ => None,
        })
        .collect();
    Some(SummaryRow {
        callee: call.callee.clone(),
        span_start: site.span_start,
        span_end: site.span_end,
        line: site.line,
        method: method.clone(),
        target,
        body_literals,
        reaches_request,
        own_site,
        library_semantics: effect.semantics.iter().cloned().collect(),
    })
}

/// What a call sends when the call graph resolves it to nothing of this
/// service: through a verified library client, what the client's claims say
/// (carrick#1564); otherwise the shape the call is written in.
fn shape_of<'c>(call: &'c CallIr, semantics: &LibrarySemantics) -> Option<Cow<'c, RequestShape>> {
    call.receiver
        .as_ref()
        .and_then(|receiver| library_shape(receiver, &call.args, semantics))
        .map(Cow::Owned)
        .or_else(|| call.request.as_ref().map(Cow::Borrowed))
}

/// The request a call through a library client sends, read through the
/// claims its package's declarations verified (carrick#1564).
///
/// `None` whenever those claims do not state both the URL and the method:
/// the receiver or its factory did not verify, or a request call writes no
/// literal method (a library's default is never assumed). The call then keeps
/// the reading it has without the semantics. A body is read only through a
/// verified body claim.
///
/// `None` too wherever the source may set a base the reading would not see:
/// the file assigns a base key through the client (`api.defaults.baseURL =
/// …`), the factory's options are open to a spread, or an options or config
/// object handed to the call is open or names a base key itself. Every base
/// key detection claims for the client counts, verified or not.
fn library_shape(
    receiver: &CallReceiver,
    args: &[Value],
    semantics: &LibrarySemantics,
) -> Option<RequestShape> {
    if semantics.is_empty() {
        return None;
    }
    let client = &receiver.client;
    let base_keys = semantics.claimed_base_keys(&client.package, &client.export);
    if base_keys.iter().any(|key| client.assigned.contains(*key)) {
        return None;
    }
    let mut used: BTreeSet<String> = BTreeSet::new();
    let (surface, base) = match &client.instance {
        None => (
            semantics.surface(&client.package, &client.export, "export")?,
            Vec::new(),
        ),
        Some(instance) => {
            let factory = semantics.factory(&client.package, &client.export, &instance.factory)?;
            let surface = semantics.surface(
                &client.package,
                &client.export,
                &instance_receiver(&instance.factory),
            )?;
            if instance.options.open {
                return None;
            }
            // The base is the literal or opaque value the options hold, or
            // none when they do not name the key at all.
            let base = match instance.options.fields.get(&factory.base_url_key) {
                Some(Value::Str(pieces)) => pieces.clone(),
                Some(_) => return None,
                None => Vec::new(),
            };
            used.insert(factory.claim_id.clone());
            (surface, base)
        }
    };

    let verb = receiver
        .member
        .as_deref()
        .and_then(|member| surface.verb(member));
    // The data a `(path, body)` verb sends is not options: a spread in it,
    // or a field that happens to share a base key's name, says nothing
    // about where the request goes.
    let body_argument = verb
        .and_then(|verb| verb.body.as_ref())
        .filter(|claim| claim.args == SemanticsVerbArgs::PathBody)
        .map(|_| 1);
    for (index, arg) in args.iter().enumerate() {
        if Some(index) == body_argument {
            continue;
        }
        if let Value::Obj(object) = arg
            && (object.open || base_keys.iter().any(|key| object.fields.contains_key(*key)))
        {
            return None;
        }
    }

    let (method, url, body, body_claim) = match verb {
        Some(verb) => {
            let Some(Value::Str(url)) = args.first() else {
                return None;
            };
            used.insert(verb.claim_id.clone());
            let (body, body_claim) = match &verb.body {
                Some(claim) => {
                    let body = match (claim.args, args.get(1), &claim.body_key) {
                        (SemanticsVerbArgs::PathBody, Some(value), _) => body_of(value),
                        (SemanticsVerbArgs::PathOptions, Some(Value::Obj(options)), Some(key)) => {
                            options.fields.get(key).map(body_of).unwrap_or_default()
                        }
                        _ => BodyValue::Unstated,
                    };
                    (body, Some(claim.claim_id.clone()))
                }
                None => (BodyValue::Unstated, None),
            };
            (
                MethodValue::Lit(verb.method.clone()),
                url.clone(),
                body,
                body_claim,
            )
        }
        None => {
            let request = surface
                .requests(receiver.member.as_deref())
                .find(|request| {
                    matches!(
                        (request.args, args.first()),
                        (SemanticsRequestArgs::Config, Some(Value::Obj(_)))
                            | (SemanticsRequestArgs::PathOptions, Some(Value::Str(_)))
                    )
                })?;
            let (url, options) = match (request.args, args.first(), args.get(1)) {
                (SemanticsRequestArgs::Config, Some(Value::Obj(config)), _) => {
                    match config.fields.get(request.url_key.as_deref()?) {
                        Some(Value::Str(url)) => (url.clone(), config),
                        _ => return None,
                    }
                }
                (
                    SemanticsRequestArgs::PathOptions,
                    Some(Value::Str(url)),
                    Some(Value::Obj(options)),
                ) => (url.clone(), options),
                _ => return None,
            };
            let method = match options.fields.get(&request.method_key) {
                Some(Value::Str(pieces)) => method_of(pieces),
                _ => MethodValue::Unknown,
            };
            if method == MethodValue::Unknown {
                return None;
            }
            used.insert(request.claim_id.clone());
            let (body, body_claim) = match &request.body {
                Some((claim_id, key)) => (
                    options.fields.get(key).map(body_of).unwrap_or_default(),
                    Some(claim_id.clone()),
                ),
                None => (BodyValue::Unstated, None),
            };
            (method, url, body, body_claim)
        }
    };
    if body != BodyValue::Unstated
        && let Some(claim_id) = body_claim
    {
        used.insert(claim_id);
    }

    Some(RequestShape {
        kind: RequestKind::Library,
        method,
        url,
        body,
        url_inline: false,
        base,
        semantics: used,
    })
}

/// A library client's base and a call's path, as one URL (carrick#1564).
///
/// Joined with exactly one slash between them, and a path that is itself an
/// absolute URL ignores the base, as a client resolving against a base does.
/// A path that does not start with text the source writes says nothing about
/// the slash between the two, so the join states no URL. An empty base is no
/// base, and a base holding a query or a fragment states no URL: the path
/// would land after it.
fn join_base(base: &[Piece], path: &[Piece]) -> Vec<Piece> {
    let base = concat([base.to_vec()]);
    if base.is_empty() {
        return path.to_vec();
    }
    if base
        .iter()
        .any(|piece| matches!(piece, Piece::Lit(text) if text.contains(['?', '#'])))
    {
        return vec![Piece::Unknown];
    }
    let Some(Piece::Lit(first)) = path.first() else {
        return vec![Piece::Unknown];
    };
    if is_absolute_url(first) {
        return path.to_vec();
    }
    let mut path = path.to_vec();
    let base_slash = matches!(base.last(), Some(Piece::Lit(text)) if text.ends_with('/'));
    let path_slash = first.starts_with('/');
    let separator = match (base_slash, path_slash) {
        (true, true) => {
            if let Some(Piece::Lit(text)) = path.first_mut() {
                text.remove(0);
            }
            Vec::new()
        }
        (false, false) if !first.is_empty() => vec![Piece::Lit("/".to_string())],
        _ => Vec::new(),
    };
    concat([base.to_vec(), separator, path])
}

/// `scheme://…` or a protocol-relative `//…`.
fn is_absolute_url(text: &str) -> bool {
    text.starts_with("//")
        || text.split_once("://").is_some_and(|(scheme, _)| {
            !scheme.is_empty()
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        })
}

/// A URL written the way the scanner's other rows write one: literal text,
/// with each opaque value as `${expr}`.
///
/// `None` unless the URL states a route: an optional opaque base, then a path
/// starting with `/` that holds at least one literal segment, where every
/// other opaque value stands for a whole segment.
fn render_target(url: &[Piece]) -> Option<String> {
    if url.iter().enumerate().any(|(position, piece)| match piece {
        Piece::Unknown => true,
        Piece::Param(..) => !is_whole_segment(url, position),
        _ => false,
    }) {
        return None;
    }
    let (base, path) = match url.first()? {
        Piece::Opaque(base) => (Some(base.as_str()), &url[1..]),
        _ => (None, url),
    };
    match path.first()? {
        Piece::Lit(text) if text.starts_with('/') => {}
        _ => return None,
    }
    let mut rendered = String::new();
    let mut literal_segment = false;
    for (position, piece) in path.iter().enumerate() {
        match piece {
            Piece::Lit(text) => {
                // A literal segment: text between slashes that is not only a
                // query or a fragment.
                let route = text.split(['?', '#']).next().unwrap_or("");
                literal_segment |= route.split('/').any(|segment| !segment.is_empty());
                rendered.push_str(text);
            }
            Piece::Opaque(name) | Piece::Param(_, name) => {
                if !is_whole_segment(path, position) {
                    return None;
                }
                rendered.push_str(&format!("${{{name}}}"));
            }
            Piece::Unknown => return None,
        }
    }
    if !literal_segment {
        return None;
    }
    Some(match base {
        Some(base) => format!("${{{base}}}{rendered}"),
        None => rendered,
    })
}

/// Whether the piece at `position` stands for a whole path segment: it
/// follows a literal ending in `/`, and ends at the next `/`, `?`, `#` or the
/// end of the URL.
fn is_whole_segment(url: &[Piece], position: usize) -> bool {
    let after_slash = position > 0
        && matches!(url.get(position - 1), Some(Piece::Lit(prev)) if prev.ends_with('/'));
    let segment_ends = match url.get(position + 1) {
        None => true,
        Some(Piece::Lit(next)) => next.starts_with(['/', '?', '#']),
        Some(_) => false,
    };
    after_slash && segment_ends
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(text: &str) -> Piece {
        Piece::Lit(text.to_string())
    }

    fn opaque(text: &str) -> Piece {
        Piece::Opaque(text.to_string())
    }

    #[test]
    fn a_route_behind_an_opaque_base_renders_with_the_base_leading() {
        assert_eq!(
            render_target(&[opaque("config.apiEndpoint"), lit("/rpc/gateway")]).as_deref(),
            Some("${config.apiEndpoint}/rpc/gateway")
        );
    }

    #[test]
    fn an_opaque_whole_segment_is_a_path_parameter() {
        assert_eq!(
            render_target(&[lit("/users/"), opaque("id"), lit("/orders")]).as_deref(),
            Some("/users/${id}/orders")
        );
    }

    #[test]
    fn an_opaque_value_inside_a_segment_states_no_route() {
        assert_eq!(
            render_target(&[lit("/api/v"), opaque("version"), lit("/x")]),
            None
        );
    }

    #[test]
    fn a_url_that_is_only_a_value_states_no_route() {
        assert_eq!(render_target(&[opaque("data.staged_url")]), None);
        assert_eq!(render_target(&[opaque("base"), lit("/")]), None);
    }

    #[test]
    fn a_parameter_left_unfilled_states_no_route() {
        assert_eq!(
            render_target(&[Piece::Param(0, "base".to_string()), lit("/users")]),
            None
        );
        assert_eq!(
            render_target(&[lit("/users-"), Piece::Param(0, "id".to_string())]),
            None
        );
    }

    #[test]
    fn a_parameter_standing_for_a_whole_segment_is_a_path_parameter() {
        assert_eq!(
            render_target(&[lit("/users/"), Piece::Param(0, "id".to_string())]).as_deref(),
            Some("/users/${id}")
        );
    }

    /// carrick#1564: a client's base and a call's path meet at exactly one
    /// slash, and a path that is a URL of its own ignores the base.
    #[test]
    fn a_base_and_a_path_join_with_exactly_one_slash() {
        let joined = |base: &[Piece], path: &[Piece]| render_target(&join_base(base, path));
        assert_eq!(
            joined(&[lit("/api")], &[lit("/users")]).as_deref(),
            Some("/api/users")
        );
        assert_eq!(
            joined(&[lit("/api/")], &[lit("/users")]).as_deref(),
            Some("/api/users")
        );
        assert_eq!(
            joined(&[lit("/api/")], &[lit("users")]).as_deref(),
            Some("/api/users")
        );
        assert_eq!(
            joined(&[lit("/api")], &[lit("users")]).as_deref(),
            Some("/api/users")
        );
        assert_eq!(
            joined(&[opaque("process.env.API")], &[lit("users/"), opaque("id")]).as_deref(),
            Some("${process.env.API}/users/${id}")
        );
        // No base: the path is the URL.
        assert_eq!(joined(&[], &[lit("/users")]).as_deref(), Some("/users"));
        // A URL of its own ignores the base.
        assert_eq!(
            join_base(&[lit("/api")], &[lit("https://other.example/x")]),
            vec![lit("https://other.example/x")]
        );
        assert_eq!(
            join_base(&[lit("/api")], &[lit("//cdn.example/x")]),
            vec![lit("//cdn.example/x")]
        );
        // A path that does not start with text says nothing about the slash.
        assert_eq!(joined(&[lit("/api")], &[opaque("path")]), None);
        // An empty base is no base: a path with no slash of its own is not
        // a route.
        assert_eq!(joined(&[lit("")], &[lit("users")]), None);
        assert_eq!(
            joined(&[lit("")], &[lit("/users")]).as_deref(),
            Some("/users")
        );
        // A base holding a query or a fragment would put the path after it.
        assert_eq!(joined(&[lit("/api?v=1")], &[lit("/users")]), None);
        assert_eq!(joined(&[lit("/api#top")], &[lit("/users")]), None);
    }

    /// A spread overwrites the keys it may carry, and only those: every key
    /// when the source does not state them, the literal's own keys when it
    /// does, and a key only some branches set becomes unknown.
    #[test]
    fn a_spread_overwrites_exactly_the_keys_it_may_carry() {
        let mut value = ObjValue::default();
        value
            .fields
            .insert("action".to_string(), Value::Str(vec![lit("sync")]));
        value
            .fields
            .insert("limit".to_string(), Value::Str(vec![lit("5")]));
        value.spread(&[
            ObjValue {
                fields: BTreeMap::from([("limit".to_string(), Value::Str(vec![lit("1")]))]),
                open: false,
            },
            ObjValue::default(),
        ]);
        assert_eq!(
            value.fields.keys().collect::<Vec<_>>(),
            vec!["action"],
            "the key one branch sets is unknown, the rest stand"
        );
        assert!(value.open);

        let mut same = ObjValue::default();
        same.spread(&[
            ObjValue {
                fields: BTreeMap::from([("op".to_string(), Value::Str(vec![lit("x")]))]),
                open: false,
            },
            ObjValue {
                fields: BTreeMap::from([("op".to_string(), Value::Str(vec![lit("x")]))]),
                open: false,
            },
        ]);
        assert_eq!(same.fields.get("op"), Some(&Value::Str(vec![lit("x")])));
        assert!(!same.open, "every branch states the same value");
    }

    #[test]
    fn concat_merges_adjacent_literals() {
        assert_eq!(
            concat([vec![lit("/a")], vec![lit("/b"), opaque("x")], vec![lit("")]]),
            vec![lit("/a/b"), opaque("x")]
        );
    }
}
