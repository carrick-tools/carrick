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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use swc_common::{SourceMap, SourceMapper, Span, Spanned, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use crate::call_graph::CallSiteTargets;
use crate::swc_scanner::SWC_SPAN_BASE;
use crate::type_manifest::is_http_method;
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
    let mut consts: HashMap<String, Value> = HashMap::new();
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
                    let scope = Scope::module(&consts);
                    let value = reader.eval(init, &scope);
                    consts.insert(ident.id.sym.to_string(), value);
                }
            }
        }
    }

    for item in &module.body {
        match module_decl(item) {
            Some(Decl::Fn(fn_decl)) => {
                let ir = reader.function(&fn_decl.function, None, &consts, &HashMap::new());
                file.functions.insert(fn_decl.ident.sym.to_string(), ir);
            }
            Some(Decl::Var(var)) => {
                for declarator in &var.decls {
                    let (Pat::Ident(ident), Some(init)) = (&declarator.name, &declarator.init)
                    else {
                        continue;
                    };
                    let ir = match &**init {
                        Expr::Arrow(arrow) => reader.arrow(arrow, None, &consts, &HashMap::new()),
                        Expr::Fn(fn_expr) => {
                            reader.function(&fn_expr.function, None, &consts, &HashMap::new())
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
                    &consts,
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
                    let ir = reader.function(function, None, &consts, &HashMap::new());
                    file.functions.insert(ident.sym.to_string(), ir);
                }
                DefaultDecl::Class(ClassExpr {
                    ident: Some(ident),
                    class,
                }) => {
                    reader.class(
                        ident.sym.as_ref(),
                        class,
                        &consts,
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

/// What an expression can read: the function's parameters and constants, the
/// enclosing class's fields, and the module's constants.
struct Scope<'a> {
    params: Vec<Option<String>>,
    locals: HashMap<String, Value>,
    fields: Option<&'a HashMap<String, Value>>,
    consts: &'a HashMap<String, Value>,
}

impl<'a> Scope<'a> {
    fn module(consts: &'a HashMap<String, Value>) -> Self {
        Self {
            params: Vec::new(),
            locals: HashMap::new(),
            fields: None,
            consts,
        }
    }

    fn param_index(&self, name: &str) -> Option<usize> {
        self.params.iter().position(|p| p.as_deref() == Some(name))
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
        consts: &HashMap<String, Value>,
        definition_keys: &HashSet<String>,
        file: &mut FileIr,
    ) {
        let fields = self.class_fields(class, consts);
        let key = |member: &str, is_static: bool| {
            let plain = format!("{name}.{member}");
            let statik = format!("{name}.static.{member}");
            if is_static && definition_keys.contains(&statik) {
                statik
            } else {
                plain
            }
        };
        for member in &class.body {
            match member {
                ClassMember::Method(method) if !matches!(method.kind, MethodKind::Setter) => {
                    let Some(member_name) = prop_name(&method.key) else {
                        continue;
                    };
                    let ir =
                        self.function(&method.function, Some(&fields), consts, &HashMap::new());
                    file.functions
                        .insert(key(&member_name, method.is_static), ir);
                }
                ClassMember::PrivateMethod(method)
                    if !matches!(method.kind, MethodKind::Setter) =>
                {
                    let member_name = format!("#{}", method.key.name);
                    let ir =
                        self.function(&method.function, Some(&fields), consts, &HashMap::new());
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
                            self.arrow(arrow, Some(&fields), consts, &HashMap::new())
                        }
                        Expr::Fn(fn_expr) => {
                            self.function(&fn_expr.function, Some(&fields), consts, &HashMap::new())
                        }
                        _ => continue,
                    };
                    file.functions.insert(key(&member_name, prop.is_static), ir);
                }
                _ => {}
            }
        }
    }

    /// Each field the class assigns exactly once, in its constructor or its
    /// initialiser, and nowhere else. A constructor parameter read into a field
    /// is opaque: nothing here follows `new ApiClient(config)` back to the
    /// value it was built with.
    fn class_fields(
        &self,
        class: &Class,
        consts: &HashMap<String, Value>,
    ) -> HashMap<String, Value> {
        let mut fields: HashMap<String, Value> = HashMap::new();
        let mut contested: HashSet<String> = HashSet::new();
        let mut assign = |name: String, value: Value, fields: &mut HashMap<String, Value>| {
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
                        let value = self.eval(init, &Scope::module(consts));
                        assign(name, value, &mut fields);
                    }
                }
                ClassMember::PrivateProp(prop) if !prop.is_static => {
                    if let Some(init) = &prop.value {
                        let value = self.eval(init, &Scope::module(consts));
                        assign(format!("#{}", prop.key.name), value, &mut fields);
                    }
                }
                ClassMember::Constructor(ctor) => {
                    let mut scope = Scope::module(consts);
                    for param in &ctor.params {
                        match param {
                            ParamOrTsParamProp::TsParamProp(prop) => {
                                if let TsParamPropParam::Ident(ident) = &prop.param {
                                    let name = ident.id.sym.to_string();
                                    assign(name.clone(), Value::opaque(name), &mut fields);
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
                        if let Some((name, value)) = this_assignment(stmt) {
                            let value = self.eval(value, &scope);
                            assign(name, value, &mut fields);
                        }
                    }
                }
                _ => {}
            }
        }

        // A field written outside the constructor holds whatever the last
        // writer put there.
        let mut writes = ThisWrites::default();
        for member in &class.body {
            match member {
                ClassMember::Method(method) => method.function.visit_with(&mut writes),
                ClassMember::PrivateMethod(method) => method.function.visit_with(&mut writes),
                ClassMember::ClassProp(prop) => prop.value.visit_with(&mut writes),
                _ => {}
            }
        }
        contested.extend(writes.fields);
        for name in contested {
            fields.insert(name.clone(), Value::opaque(format!("this.{name}")));
        }
        // A field holding only a value the class is handed (`this.baseUrl =
        // baseUrl`, a parameter property) is written the way the request
        // writes it; one the class BUILDS is read through.
        fields
            .into_iter()
            .map(|(name, value)| {
                let value = bound(format!("this.{name}"), value);
                (name, value)
            })
            .collect()
    }

    fn function(
        &self,
        function: &Function,
        fields: Option<&HashMap<String, Value>>,
        consts: &HashMap<String, Value>,
        captured: &HashMap<String, Value>,
    ) -> FnIr {
        let params = function.params.iter().map(|p| pat_name(&p.pat)).collect();
        let mut scope = Scope {
            params,
            locals: captured.clone(),
            fields,
            consts,
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
        fields: Option<&HashMap<String, Value>>,
        consts: &HashMap<String, Value>,
        captured: &HashMap<String, Value>,
    ) -> FnIr {
        let params = arrow.params.iter().map(pat_name).collect();
        let mut scope = Scope {
            params,
            locals: captured.clone(),
            fields,
            consts,
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
    fn body(&self, stmts: &[Stmt], scope: &mut Scope<'_>, ir: &mut FnIr) {
        let mut reassigned = Reassigned::default();
        for stmt in stmts {
            stmt.visit_with(&mut reassigned);
        }
        for stmt in stmts {
            if let Stmt::Decl(Decl::Var(var)) = stmt {
                for declarator in &var.decls {
                    if let Some(init) = &declarator.init {
                        self.walk(init, scope, ir);
                    }
                    if let (Pat::Ident(ident), Some(init)) = (&declarator.name, &declarator.init) {
                        let name = ident.id.sym.to_string();
                        let value = if reassigned.names.contains(&name) {
                            Value::opaque(name.clone())
                        } else {
                            bound(name.clone(), self.eval(init, scope))
                        };
                        scope.locals.insert(name, value);
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
                if let Some(value) = scope.locals.get(name).or_else(|| scope.consts.get(name)) {
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
                if let Some(value) = scope.fields.and_then(|fields| fields.get(&name)) {
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

    fn object(&self, obj: &ObjectLit, scope: &Scope<'_>) -> ObjValue {
        let mut value = ObjValue::default();
        for prop in &obj.props {
            let PropOrSpread::Prop(prop) = prop else {
                value.open = true;
                continue;
            };
            match &**prop {
                Prop::KeyValue(kv) => match prop_name(&kv.key) {
                    Some(key) => {
                        value.fields.insert(key, self.eval(&kv.value, scope));
                    }
                    None => value.open = true,
                },
                Prop::Shorthand(ident) => {
                    let expr = Expr::Ident(ident.clone());
                    value
                        .fields
                        .insert(ident.sym.to_string(), self.eval(&expr, scope));
                }
                _ => value.open = true,
            }
        }
        value
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
            RequestKind::Bag => None,
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
                // method is a verb someone else supplies.
                None if obj.fields.contains_key("body") || obj.fields.contains_key("data") => {
                    MethodValue::Unknown
                }
                None => MethodValue::Lit("GET".to_string()),
            },
            None => match &verb {
                Some(verb) => MethodValue::Lit(verb.clone()),
                None => MethodValue::Lit("GET".to_string()),
            },
        };

        let body_of = |value: &Value| match value {
            Value::Obj(body) => BodyValue::Fields(body.clone()),
            Value::Str(pieces) => match pieces.as_slice() {
                [Piece::Param(index, _)] => BodyValue::Param(*index),
                _ => BodyValue::Unstated,
            },
            Value::Callback(_) => BodyValue::Unstated,
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
        })
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
                        self.scope.consts,
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
                        self.scope.consts,
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

        let request = if call.args.iter().any(|arg| arg.spread.is_some()) {
            None
        } else {
            self.reader.request_shape(call, &args, self.scope)
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
        });
    }

    // A function written in the body and not handed to a call: its calls
    // state rows where they are written, and what it sends is not this
    // body's.
    fn visit_function(&mut self, function: &Function) {
        let detached = self
            .reader
            .function(function, None, self.scope.consts, &self.captured());
        self.ir.detached.push(detached);
    }

    fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
        let detached = self.reader.arrow(
            arrow,
            self.scope.fields,
            self.scope.consts,
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
    /// parameters as opaque names (the callback is not called with them).
    fn captured(&self) -> HashMap<String, Value> {
        let mut captured = self.scope.locals.clone();
        for name in self.scope.params.iter().flatten() {
            captured.insert(name.clone(), Value::opaque(name.clone()));
        }
        captured
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
        })
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
                    if let Some(request) = &call.request {
                        summary
                            .effects
                            .insert(Effect::from_shape(request, file, call.site.line));
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

/// Compose every function's summary and state the rows it supports.
pub fn summarize(files: &HashMap<PathBuf, FileIr>, sites: &CallSiteTargets) -> RequestSummaryIndex {
    let mut composer = Composer {
        files,
        sites,
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
                if let Some(request) = &call.request
                    && request.kind != RequestKind::Verb
                    && !request.url_inline
                {
                    let effect = Effect::from_shape(request, file, call.site.line);
                    if !effect.has_params() {
                        match row(&effect, call, None, true) {
                            Some(row) => rows.push(row),
                            None => index.undetermined += 1,
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
    let target = render_target(&effect.url)?;
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

    #[test]
    fn concat_merges_adjacent_literals() {
        assert_eq!(
            concat([vec![lit("/a")], vec![lit("/b"), opaque("x")], vec![lit("")]]),
            vec![lit("/a/b"), opaque("x")]
        );
    }
}
