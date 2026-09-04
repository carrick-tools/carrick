//! Deterministic Socket.IO contract extraction.
//!
//! Socket.IO has a real operation key — event name plus message-flow
//! direction — and event names are string literals in idiomatic code, so
//! extraction is AST-based with no LLM. Listeners (`socket.on("x", ...)`)
//! are producers of the key for the direction they receive; emitters
//! (`socket.emit("x", ...)`) are consumers for the direction they send.
//! Which side of the wire a call site is on is derived from imports:
//! `socket.io-client` factories make client sockets, `new Server(...)` from
//! `socket.io` makes server roots, and the first parameter of a
//! `connection` handler is a per-connection server socket.
//!
//! The key carries the event name and the direction, and nothing else. A
//! custom namespace is therefore not part of operation identity here, and
//! cannot be: the server names it in a `.of(...)` argument that is usually a
//! variable, and the client names it in the path of the URL it connects with,
//! built in some other method. Both sides are namespace-blind, so ops are
//! recorded on a namespace exactly as they are on the default one.
//!
//! The imprecision that accepts, stated plainly: two namespaces of one server
//! handling one event name produce two producer rows on one key. That is the
//! same imprecision the model already accepts across files and services, where
//! two listeners for one event have always shared a key, so a file-level skip
//! bought nothing the key could express and only hid the ops (carrick#662).
//!
//! A long-lived socket is usually held on a class field rather than a local
//! (`private socket?: Socket<…>`; `this.socket = this.#createSocket()`), and
//! the emits that carry the contract sit in later methods, on `this.socket`
//! (carrick#659). Those fields are roots too, by two structural rules that do
//! not depend on how the field is initialized:
//! - a binding — class field, private field, constructor parameter property,
//!   `const`, or parameter — whose declared type is the `Socket` type imported
//!   from `socket.io-client` (client) or the `Socket`/`Namespace` type
//!   imported from `socket.io` (server side), and
//! - `this.<field> = <expr>` where the right-hand side is already a socket
//!   root (a factory call, `new Server(...)`, or another root binding).
//!
//! Precision over recall, per the brittleness guardrails:
//! - only string-literal event names count; dynamic names are skipped,
//! - reserved lifecycle events (`connect`, `disconnect`, ...) never become
//!   contract events,
//! - a namespace reached only through the `Server` TYPE stays invisible: the
//!   type rule admits the receivers (`Socket`, `Namespace`), while the server
//!   root is created by `new Server(...)` alone,
//! - CommonJS `require("socket.io")` bootstrapping is not traced (coverage
//!   gap, not a false positive),
//! - a field assigned the RETURN VALUE of a method that builds a socket
//!   (`this.socket = this.#createSocket()`) is a root only via one of the two
//!   rules above — method-return flow is not traced,
//! - socket identity is tracked by binding name (`this.<field>` for fields),
//!   not full scope analysis, and is flat per file: two classes in one file
//!   with same-named socket fields share a root.

use crate::operation::{OperationKey, SocketDirection};
use crate::parser::parse_file;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use swc_common::errors::{ColorConfig, Handler};
use swc_common::{GLOBALS, Globals, SourceMap, Span, Spanned, sync::Lrc};
use swc_ecma_ast::{
    AssignExpr, AssignTarget, Callee, ClassProp, Expr, ExprOrSpread, ImportDecl, ImportSpecifier,
    Lit, MemberExpr, MemberProp, ModuleExportName, NewExpr, OptChainBase, OptChainExpr, Pat,
    PrivateProp, PropName, SimpleAssignTarget, TsEntityName, TsParamProp, TsParamPropParam, TsType,
    TsTypeAnn, TsUnionOrIntersectionType, VarDeclarator,
};
use swc_ecma_visit::{Visit, VisitWith};
use tracing::debug;

/// A socket listener or emitter with its source location.
///
/// `payload_type_symbol`/`payload_type_source` carry the message payload's TS
/// type so the op can be anchored and resolved through the existing
/// SymbolRequest/sidecar bundle path (#245 Phase 1). They are populated only
/// when the payload is an explicitly-typed named reference whose declaration is
/// `import`ed (precision over recall): inline object types, generics, unions,
/// and untyped payloads stay `None` so they degrade to an honest `Unknown`
/// rather than a phantom anchor.
#[derive(Debug, Clone)]
pub struct SocketOp {
    pub key: OperationKey,
    pub file_path: PathBuf,
    pub line: u32,
    /// Bare symbol name of the payload type (e.g. `Payment`), when explicitly
    /// annotated as a named reference. `None` for inline/generic/untyped payloads.
    pub payload_type_symbol: Option<String>,
    /// Module specifier the payload type is imported from (e.g.
    /// `./types/payment`), paired with `payload_type_symbol`. `None` when the
    /// symbol is not imported (same-file or untyped).
    pub payload_type_source: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SocketExtraction {
    /// Listeners: producers of the direction they receive.
    pub listeners: Vec<SocketOp>,
    /// Emitters: consumers of the direction they send.
    pub emitters: Vec<SocketOp>,
}

impl SocketExtraction {
    pub fn is_empty(&self) -> bool {
        self.listeners.is_empty() && self.emitters.is_empty()
    }

    fn merge(&mut self, other: SocketExtraction) {
        self.listeners.extend(other.listeners);
        self.emitters.extend(other.emitters);
    }
}

/// Socket.IO lifecycle/reserved events that are not application contract
/// events.
const RESERVED_EVENTS: &[&str] = &[
    "connection",
    "connect",
    "connect_error",
    "disconnect",
    "disconnecting",
    "error",
    "reconnect",
    "reconnect_attempt",
    "reconnect_error",
    "reconnect_failed",
    "ping",
    "pong",
    "newListener",
    "removeListener",
];

/// Extract Socket.IO operations from the service's TS/JS files.
pub fn scan_files(service_files: &[PathBuf]) -> SocketExtraction {
    let mut extraction = SocketExtraction::default();
    for file in service_files {
        let is_script = file
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| matches!(ext, "ts" | "tsx" | "js" | "jsx"));
        if !is_script {
            continue;
        }
        extraction.merge(extract_from_ts_file(file));
    }
    debug!(
        listeners = extraction.listeners.len(),
        emitters = extraction.emitters.len(),
        "Socket.IO extraction complete"
    );
    extraction
}

fn extract_from_ts_file(file_path: &Path) -> SocketExtraction {
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));

    let globals = Globals::new();
    GLOBALS.set(&globals, || {
        let Some(module) = parse_file(file_path, &cm, &handler) else {
            return SocketExtraction::default();
        };

        // Pass A: collect socket-rooted binding names. Run to fixpoint (a
        // connection-handler socket needs the server root known first);
        // two iterations cover every realistic nesting.
        let mut roots = SocketRoots::default();
        loop {
            let before = roots.size();
            let mut collector = RootCollector { roots: &mut roots };
            module.visit_with(&mut collector);
            if roots.size() == before {
                break;
            }
        }

        if roots.size() == 0 {
            return SocketExtraction::default();
        }

        // Pass B: collect ops on socket-rooted identifiers.
        let mut ops = OpCollector {
            cm: cm.clone(),
            file_path,
            roots: &roots,
            extraction: SocketExtraction::default(),
        };
        module.visit_with(&mut ops);
        ops.extraction
    })
}

/// Which side of the wire a socket-rooted binding sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SocketKind {
    Client,
    Server,
}

#[derive(Default)]
struct SocketRoots {
    /// Local names of `socket.io-client` factories (`io`, `connect`, ...).
    client_factories: HashSet<String>,
    /// Local names of the `socket.io` `Server` class.
    server_classes: HashSet<String>,
    /// Local names of the `Socket` TYPE imported from `socket.io-client`.
    /// A binding declared with it holds a client socket however it was
    /// initialized (carrick#659).
    client_socket_types: HashSet<String>,
    /// Local names of the `Socket` TYPE imported from `socket.io` — the
    /// per-connection server socket, not the server root.
    server_socket_types: HashSet<String>,
    /// Bindings holding client sockets (`const s = io(url)`). Class fields are
    /// keyed `this.<name>` / `this.#<name>`.
    client_sockets: HashSet<String>,
    /// Bindings holding server roots (`const io = new Server(...)`) or
    /// per-connection sockets (`io.on("connection", (socket) => ...)`).
    server_sockets: HashSet<String>,
    /// Imported type symbols → their module specifier. Drives payload-anchor
    /// resolution (#245): an emitted/received payload typed as an imported
    /// named reference gets a `(symbol, source)` pair the SymbolRequest path
    /// can bundle. Same-file types are absent here and resolve with `None`
    /// source.
    type_imports: HashMap<String, String>,
    /// Binding name → payload type symbol, from `const x: T = …` declarators
    /// and typed function parameters. Lets `socket.emit("e", payment)` recover
    /// `Payment` from the `payment` binding's annotation. File-level and flat
    /// (binding shadowing is ignored — a precision tradeoff consistent with the
    /// module's other guardrails); only simple named references are recorded,
    /// so generics/unions/inline object types never produce an anchor.
    binding_types: HashMap<String, String>,
}

impl SocketRoots {
    fn size(&self) -> usize {
        self.client_factories.len()
            + self.server_classes.len()
            + self.client_socket_types.len()
            + self.server_socket_types.len()
            + self.client_sockets.len()
            + self.server_sockets.len()
            + self.type_imports.len()
            + self.binding_types.len()
    }

    fn record(&mut self, key: String, kind: SocketKind) {
        match kind {
            SocketKind::Client => self.client_sockets.insert(key),
            SocketKind::Server => self.server_sockets.insert(key),
        };
    }

    fn kind_of(&self, key: &str) -> Option<SocketKind> {
        if self.client_sockets.contains(key) {
            Some(SocketKind::Client)
        } else if self.server_sockets.contains(key) {
            Some(SocketKind::Server)
        } else {
            None
        }
    }

    /// Socket kind a declared type annotation implies, or `None` when the type
    /// is not the socket.io `Socket` type. `Socket<ServerToClient,
    /// ClientToServer>` and `Socket | undefined` both resolve — a generic
    /// parameterization and an optional field are still that socket.
    fn kind_of_type_ann(&self, type_ann: &TsTypeAnn) -> Option<SocketKind> {
        self.kind_of_type(&type_ann.type_ann)
    }

    fn kind_of_type(&self, ty: &TsType) -> Option<SocketKind> {
        match ty {
            TsType::TsTypeRef(type_ref) => match &type_ref.type_name {
                TsEntityName::Ident(ident) => {
                    let name = ident.sym.as_ref();
                    if self.client_socket_types.contains(name) {
                        Some(SocketKind::Client)
                    } else if self.server_socket_types.contains(name) {
                        Some(SocketKind::Server)
                    } else {
                        None
                    }
                }
                TsEntityName::TsQualifiedName(_) => None,
            },
            TsType::TsUnionOrIntersectionType(TsUnionOrIntersectionType::TsUnionType(union)) => {
                union
                    .types
                    .iter()
                    .find_map(|member| self.kind_of_type(member))
            }
            TsType::TsParenthesizedType(paren) => self.kind_of_type(&paren.type_ann),
            _ => None,
        }
    }

    /// Socket kind of an expression that is being bound to a name: a client
    /// factory call, a `new Server(...)`, or a reference to a binding already
    /// known to be a root. Nothing else — a method call that happens to return
    /// a socket is not traced (see the module docs).
    fn kind_of_init(&self, expr: &Expr) -> Option<SocketKind> {
        match expr {
            Expr::Call(call) => match &call.callee {
                Callee::Expr(callee) => match &**callee {
                    Expr::Ident(factory)
                        if self.client_factories.contains(factory.sym.as_ref()) =>
                    {
                        Some(SocketKind::Client)
                    }
                    _ => None,
                },
                _ => None,
            },
            Expr::New(NewExpr { callee, .. }) => match &**callee {
                Expr::Ident(class) if self.server_classes.contains(class.sym.as_ref()) => {
                    Some(SocketKind::Server)
                }
                _ => None,
            },
            Expr::Ident(ident) => self.kind_of(ident.sym.as_ref()),
            Expr::Member(member) => member_root(member).and_then(|root| self.kind_of(&root)),
            Expr::Paren(paren) => self.kind_of_init(&paren.expr),
            Expr::Await(awaited) => self.kind_of_init(&awaited.arg),
            Expr::TsNonNull(non_null) => self.kind_of_init(&non_null.expr),
            Expr::TsAs(as_expr) => self.kind_of_init(&as_expr.expr),
            _ => None,
        }
    }

    fn direction_for(&self, root: &str, is_listener: bool) -> Option<SocketDirection> {
        if self.client_sockets.contains(root) {
            // A client listens to server→client messages and emits
            // client→server messages.
            Some(if is_listener {
                SocketDirection::ServerToClient
            } else {
                SocketDirection::ClientToServer
            })
        } else if self.server_sockets.contains(root) {
            Some(if is_listener {
                SocketDirection::ClientToServer
            } else {
                SocketDirection::ServerToClient
            })
        } else {
            None
        }
    }
}

struct RootCollector<'a> {
    roots: &'a mut SocketRoots,
}

impl Visit for RootCollector<'_> {
    fn visit_import_decl(&mut self, node: &ImportDecl) {
        let source = node.src.value.as_ref();
        // Record every named import's local name → module specifier so a
        // socket payload typed as an imported symbol (`import type { Payment }
        // from "./types"`) can be anchored. Default/namespace imports are
        // skipped: payload type references are named, and a default import's
        // local name is not the exported declaration the bundler resolves by.
        for specifier in &node.specifiers {
            if let ImportSpecifier::Named(named) = specifier {
                self.roots
                    .type_imports
                    .insert(named.local.sym.to_string(), source.to_string());
            }
        }
        if source != "socket.io" && source != "socket.io-client" {
            return;
        }
        for specifier in &node.specifiers {
            match specifier {
                ImportSpecifier::Default(default) if source == "socket.io-client" => {
                    self.roots
                        .client_factories
                        .insert(default.local.sym.to_string());
                }
                ImportSpecifier::Named(named) => {
                    let imported = named
                        .imported
                        .as_ref()
                        .map(|name| match name {
                            ModuleExportName::Ident(ident) => ident.sym.to_string(),
                            ModuleExportName::Str(s) => s.value.to_string(),
                        })
                        .unwrap_or_else(|| named.local.sym.to_string());
                    match (source, imported.as_str()) {
                        ("socket.io-client", "io" | "connect" | "default") => {
                            self.roots
                                .client_factories
                                .insert(named.local.sym.to_string());
                        }
                        ("socket.io", "Server") => {
                            self.roots
                                .server_classes
                                .insert(named.local.sym.to_string());
                        }
                        // The socket TYPE on either side. A binding declared
                        // with it is a socket root regardless of how it was
                        // initialized (carrick#659).
                        ("socket.io-client", "Socket") => {
                            self.roots
                                .client_socket_types
                                .insert(named.local.sym.to_string());
                        }
                        // `Namespace` is the other server-side receiver: it
                        // broadcasts server->client and hands per-connection
                        // sockets to its `connection` handler, exactly as the
                        // default namespace does (carrick#662).
                        ("socket.io", "Socket" | "Namespace") => {
                            self.roots
                                .server_socket_types
                                .insert(named.local.sym.to_string());
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        // const socket = io(url) — client socket; const io = new Server(...) —
        // server root.
        if let Pat::Ident(binding) = &node.name
            && let Some(init) = node.init.as_deref()
            && let Some(kind) = self.roots.kind_of_init(init)
        {
            self.roots.record(binding.id.sym.to_string(), kind);
        }
        node.visit_children_with(self);
    }

    fn visit_class_prop(&mut self, node: &ClassProp) {
        // `private socket?: Socket<…>` / `socket = io(url)` on a class body:
        // the field is the root every later `this.socket.emit(…)` reads
        // (carrick#659).
        if let PropName::Ident(name) = &node.key {
            let key = format!("this.{}", name.sym);
            let kind = node
                .type_ann
                .as_deref()
                .and_then(|type_ann| self.roots.kind_of_type_ann(type_ann))
                .or_else(|| {
                    node.value
                        .as_deref()
                        .and_then(|value| self.roots.kind_of_init(value))
                });
            if let Some(kind) = kind {
                self.roots.record(key, kind);
            }
        }
        node.visit_children_with(self);
    }

    fn visit_private_prop(&mut self, node: &PrivateProp) {
        // Same rule for `#socket`, which is a distinct AST node.
        let key = format!("this.#{}", node.key.name);
        let kind = node
            .type_ann
            .as_deref()
            .and_then(|type_ann| self.roots.kind_of_type_ann(type_ann))
            .or_else(|| {
                node.value
                    .as_deref()
                    .and_then(|value| self.roots.kind_of_init(value))
            });
        if let Some(kind) = kind {
            self.roots.record(key, kind);
        }
        node.visit_children_with(self);
    }

    fn visit_ts_param_prop(&mut self, node: &TsParamProp) {
        // `constructor(private readonly socket: Socket)` declares the field and
        // the parameter in one breath, so the same declared-type rule has to
        // reach it or every later `this.socket.on(…)` is invisible.
        if let TsParamPropParam::Ident(ident) = &node.param
            && let Some(type_ann) = ident.type_ann.as_deref()
            && let Some(kind) = self.roots.kind_of_type_ann(type_ann)
        {
            self.roots.record(format!("this.{}", ident.id.sym), kind);
        }
        node.visit_children_with(self);
    }

    fn visit_assign_expr(&mut self, node: &AssignExpr) {
        // `this.socket = io(url)` / `this.socket = socket` — the untyped route
        // to the same field root. The fixpoint loop lets the right-hand side
        // become known after this statement is first visited.
        if let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = &node.left
            && let Some(key) = member_root(member)
            && key.starts_with("this.")
            && let Some(kind) = self.roots.kind_of_init(&node.right)
        {
            self.roots.record(key, kind);
        }
        node.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, node: &swc_ecma_ast::CallExpr) {
        // io.on("connection", (socket) => ...) — the handler's first param
        // is a per-connection server socket.
        if let Callee::Expr(callee) = &node.callee
            && let Expr::Member(member) = &**callee
            && member
                .prop
                .as_ident()
                .is_some_and(|prop| prop.sym.as_ref() == "on")
            && let Some(receiver) = member_root(member)
            && self.roots.server_sockets.contains(&receiver)
            && let Some(first) = node.args.first()
            && matches!(&*first.expr, Expr::Lit(Lit::Str(event)) if matches!(event.value.as_ref(), "connection" | "connect"))
            && let Some(handler) = node.args.get(1)
        {
            let param = match &*handler.expr {
                Expr::Arrow(arrow) => arrow.params.first().and_then(|p| p.as_ident()),
                Expr::Fn(func) => func.function.params.first().and_then(|p| p.pat.as_ident()),
                _ => None,
            };
            if let Some(param) = param {
                self.roots.server_sockets.insert(param.id.sym.to_string());
            }
        }
        node.visit_children_with(self);
    }

    fn visit_pat(&mut self, node: &Pat) {
        // Record `const payment: Payment` / `(payment: Payment) => …` style
        // typed bindings so an emitted payload identifier can recover its
        // type symbol. Only simple named references count (see
        // `named_type_symbol`); anything else leaves the binding unanchored.
        if let Pat::Ident(ident) = node
            && let Some(type_ann) = ident.type_ann.as_ref()
        {
            if let Some(symbol) = named_type_symbol(type_ann) {
                self.roots
                    .binding_types
                    .insert(ident.id.sym.to_string(), symbol);
            }
            // A local or parameter declared with the socket type is a root by
            // the same rule as a class field (carrick#659): `(socket: Socket)
            // => …` on the server, `const socket: Socket<…> = connect()` on
            // the client.
            if let Some(kind) = self.roots.kind_of_type_ann(type_ann) {
                self.roots.record(ident.id.sym.to_string(), kind);
            }
        }
        node.visit_children_with(self);
    }
}

/// Bare symbol name of a simple named type annotation (`Payment` from
/// `: Payment`), or `None` for anything that is not a single unqualified type
/// reference. Precision over recall: generics (`Foo<T>`), unions, intersections,
/// inline object types, qualified names (`ns.Type`), and primitives are all
/// rejected so the socket anchor only fires when there is one resolvable symbol.
fn named_type_symbol(type_ann: &TsTypeAnn) -> Option<String> {
    match &*type_ann.type_ann {
        TsType::TsTypeRef(type_ref) if type_ref.type_params.is_none() => {
            match &type_ref.type_name {
                TsEntityName::Ident(ident) => {
                    let name = ident.sym.to_string();
                    // Reject TS built-in/primitive references that happen to parse
                    // as a type ref so they never become a bundle target.
                    if is_builtin_type(&name) {
                        None
                    } else {
                        Some(name)
                    }
                }
                TsEntityName::TsQualifiedName(_) => None,
            }
        }
        _ => None,
    }
}

/// Lowercase/well-known TS types that must never be treated as a resolvable
/// payload anchor.
fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "any"
            | "unknown"
            | "never"
            | "void"
            | "object"
            | "string"
            | "number"
            | "boolean"
            | "bigint"
            | "symbol"
            | "undefined"
            | "null"
            | "Array"
            | "Promise"
            | "Record"
            | "Map"
            | "Set"
            | "Date"
            // Capitalized global wrapper / utility types: a payload annotated
            // with one of these is a TS/lib global, not a user type, so it must
            // not become a SymbolRequest (the sidecar would try to bundle the
            // global declaration — noisy and useless).
            | "Object"
            | "String"
            | "Number"
            | "Boolean"
            | "Symbol"
            | "BigInt"
            | "Function"
            | "RegExp"
            | "Error"
    )
}

struct OpCollector<'a> {
    cm: Lrc<SourceMap>,
    file_path: &'a Path,
    roots: &'a SocketRoots,
    extraction: SocketExtraction,
}

/// Walk a callee chain (`io.to("room").emit`, `socket.broadcast.emit`) back
/// to the name of its root binding. A chain rooted on `this` yields the field
/// key (`this.socket`, `this.#socket`) the class-field rules record under.
fn chain_root(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(ident) => Some(ident.sym.to_string()),
        Expr::Member(member) => member_root(member),
        Expr::Call(call) => match &call.callee {
            Callee::Expr(callee) => chain_root(callee),
            _ => None,
        },
        Expr::OptChain(opt) => match &*opt.base {
            OptChainBase::Member(member) => member_root(member),
            OptChainBase::Call(call) => chain_root(&call.callee),
        },
        Expr::Paren(paren) => chain_root(&paren.expr),
        Expr::Await(awaited) => chain_root(&awaited.arg),
        Expr::TsNonNull(non_null) => chain_root(&non_null.expr),
        Expr::TsAs(as_expr) => chain_root(&as_expr.expr),
        _ => None,
    }
}

/// Root binding name of a member expression: the field key when the object is
/// `this`, otherwise the root of whatever the object is rooted on.
fn member_root(member: &MemberExpr) -> Option<String> {
    if matches!(&*member.obj, Expr::This(_)) {
        return this_field_key(&member.prop);
    }
    chain_root(&member.obj)
}

/// `this.socket` -> `"this.socket"`, `this.#socket` -> `"this.#socket"`.
/// Computed access (`this[name]`) has no static key.
fn this_field_key(prop: &MemberProp) -> Option<String> {
    match prop {
        MemberProp::Ident(ident) => Some(format!("this.{}", ident.sym)),
        MemberProp::PrivateName(private) => Some(format!("this.#{}", private.name)),
        MemberProp::Computed(_) => None,
    }
}

impl Visit for OpCollector<'_> {
    fn visit_call_expr(&mut self, node: &swc_ecma_ast::CallExpr) {
        if let Callee::Expr(callee) = &node.callee
            && let Expr::Member(member) = &**callee
        {
            self.record_member_call(member, &node.args, node.span());
        }
        node.visit_children_with(self);
    }

    fn visit_opt_chain_expr(&mut self, node: &OptChainExpr) {
        // `this.socket?.emit("x", …)` is an optional call, not a `CallExpr`,
        // so it needs its own arm or the op is silently lost.
        if let OptChainBase::Call(call) = &*node.base {
            match &*call.callee {
                Expr::Member(member) => self.record_member_call(member, &call.args, node.span()),
                Expr::OptChain(inner) => {
                    if let OptChainBase::Member(member) = &*inner.base {
                        self.record_member_call(member, &call.args, node.span());
                    }
                }
                _ => {}
            }
        }
        node.visit_children_with(self);
    }
}

impl OpCollector<'_> {
    /// Record the socket op a `<root>.<method>(...)` call site carries, if any.
    /// Shared by plain and optional calls so both spellings resolve.
    fn record_member_call(&mut self, member: &MemberExpr, args: &[ExprOrSpread], span: Span) {
        let Some(prop) = member.prop.as_ident() else {
            return;
        };
        let Some(root_name) = chain_root(&member.obj) else {
            return;
        };
        let is_socket_root = self.roots.kind_of(&root_name).is_some();

        let is_listener = matches!(prop.sym.as_ref(), "on" | "once");
        let is_emitter = prop.sym.as_ref() == "emit";
        if is_socket_root
            && (is_listener || is_emitter)
            && let Some(first) = args.first()
            && let Expr::Lit(Lit::Str(event)) = &*first.expr
            && !RESERVED_EVENTS.contains(&event.value.as_ref())
            && let Some(direction) = self.roots.direction_for(&root_name, is_listener)
        {
            let payload_symbol = if is_listener {
                // Listener: the handler's first parameter is the received
                // payload; read its type annotation directly.
                Self::listener_payload_symbol(args)
            } else {
                // Emitter: the second argument is the sent payload; recover
                // its symbol from the binding's annotation.
                self.emitter_payload_symbol(args)
            };
            let (payload_type_symbol, payload_type_source) = match payload_symbol {
                Some(symbol) => {
                    let source = self.roots.type_imports.get(&symbol).cloned();
                    (Some(symbol), source)
                }
                None => (None, None),
            };
            let op = SocketOp {
                key: OperationKey::socket(event.value.to_string(), direction),
                file_path: self.file_path.to_path_buf(),
                line: self.cm.lookup_char_pos(span.lo).line as u32,
                payload_type_symbol,
                payload_type_source,
            };
            if is_listener {
                self.extraction.listeners.push(op);
            } else {
                self.extraction.emitters.push(op);
            }
        }
    }

    /// Payload type symbol of a listener call's handler — the type annotation
    /// on the handler's first parameter (`socket.on("e", (p: Payment) => …)`).
    fn listener_payload_symbol(args: &[ExprOrSpread]) -> Option<String> {
        let handler = args.get(1)?;
        let first_param: Option<&Pat> = match &*handler.expr {
            Expr::Arrow(arrow) => arrow.params.first(),
            Expr::Fn(func) => func.function.params.first().map(|p| &p.pat),
            _ => None,
        };
        match first_param? {
            Pat::Ident(ident) => ident.type_ann.as_deref().and_then(named_type_symbol),
            _ => None,
        }
    }

    /// Payload type symbol of an emitter call — the second argument's binding
    /// type (`socket.emit("e", payment)` where `payment: Payment`). Only a bare
    /// identifier argument resolves; inline literals/expressions stay
    /// unanchored.
    fn emitter_payload_symbol(&self, args: &[ExprOrSpread]) -> Option<String> {
        let payload = args.get(1)?;
        match &*payload.expr {
            Expr::Ident(ident) => self.roots.binding_types.get(ident.sym.as_ref()).cloned(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> SocketExtraction {
        let dir = std::env::temp_dir().join(format!(
            "carrick-socket-test-{}-{:016x}",
            std::process::id(),
            {
                // unique-enough per test input to avoid tempdir collisions
                let mut hash: u64 = 0xcbf29ce484222325;
                for byte in source.as_bytes() {
                    hash ^= u64::from(*byte);
                    hash = hash.wrapping_mul(0x100000001b3);
                }
                hash
            }
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("file.ts");
        std::fs::write(&file, source).unwrap();
        let result = extract_from_ts_file(&file);
        std::fs::remove_dir_all(&dir).ok();
        result
    }

    fn keys(ops: &[SocketOp]) -> Vec<String> {
        let mut keys: Vec<String> = ops.iter().map(|op| op.key.canonical()).collect();
        keys.sort();
        keys
    }

    #[test]
    fn server_listeners_and_emitters() {
        let result = extract(
            r#"
import { Server } from "socket.io";
const io = new Server(httpServer);
io.on("connection", (socket) => {
  socket.on("chat:message", (msg) => { io.emit("chat:broadcast", msg); });
  socket.emit("welcome", { ok: true });
  socket.broadcast.emit("user:joined", socket.id);
  io.to("room").emit("room:update", {});
  socket.on("disconnect", () => {});
});
"#,
        );
        assert_eq!(
            keys(&result.listeners),
            vec!["socket|CLIENT->SERVER|chat:message"],
            "server listener is a producer of client->server"
        );
        assert_eq!(
            keys(&result.emitters),
            vec![
                "socket|SERVER->CLIENT|chat:broadcast",
                "socket|SERVER->CLIENT|room:update",
                "socket|SERVER->CLIENT|user:joined",
                "socket|SERVER->CLIENT|welcome",
            ],
            "server emits (incl. broadcast/to chains) are consumers of server->client"
        );
    }

    #[test]
    fn client_listeners_and_emitters() {
        let result = extract(
            r#"
import { io } from "socket.io-client";
const socket = io("https://chat.internal");
socket.on("chat:broadcast", (msg) => console.log(msg));
socket.emit("chat:message", "hello");
socket.on("connect", () => {});
"#,
        );
        assert_eq!(
            keys(&result.listeners),
            vec!["socket|SERVER->CLIENT|chat:broadcast"]
        );
        assert_eq!(
            keys(&result.emitters),
            vec!["socket|CLIENT->SERVER|chat:message"]
        );
    }

    #[test]
    fn unrelated_on_calls_are_ignored() {
        let result = extract(
            r#"
import { Server } from "socket.io";
const io = new Server(httpServer);
process.on("exit", () => {});
emitter.on("data", () => {});
emitter.emit("data", 1);
"#,
        );
        assert!(result.is_empty(), "non-socket .on/.emit must not match");
    }

    #[test]
    fn dynamic_event_names_are_skipped() {
        let result = extract(
            r#"
import { io } from "socket.io-client";
const socket = io(url);
socket.emit(EVENTS.USER_CREATED, payload);
socket.on(`chat:${kind}`, handler);
"#,
        );
        assert!(result.is_empty(), "only literal event names count");
    }

    #[test]
    fn namespace_files_are_recorded_under_the_plain_event_key() {
        // A file that carves a namespace off its server used to be dropped
        // whole. The key has no namespace component and neither side of the
        // wire can supply one, so the ops are recorded and the namespace is
        // simply not part of their identity (carrick#662).
        //
        // The accepted imprecision: two namespaces of one server handling one
        // event name produce two producer rows on one key, the same
        // imprecision the model already accepts across files and services.
        let result = extract(
            r#"
import type { Namespace } from "socket.io";
import { Server } from "socket.io";
const io = new Server(httpServer);
const chat: Namespace = io.of("/chat");
chat.on("connection", (socket) => {
  socket.on("chat:message", handler);
});
io.on("connection", (socket) => {
  socket.on("chat:message", handler);
  socket.on("presence:ping", handler);
});
"#,
        );
        assert_eq!(
            keys(&result.listeners),
            vec![
                // One event name handled on two namespaces: two producer rows,
                // one key. This is the imprecision above, asserted rather than
                // avoided.
                "socket|CLIENT->SERVER|chat:message",
                "socket|CLIENT->SERVER|chat:message",
                "socket|CLIENT->SERVER|presence:ping",
            ],
            "ops on a namespace and on the default namespace are both recorded"
        );
    }

    #[test]
    fn namespace_typed_binding_is_a_server_root() {
        // The namespace is carved off a server the pass cannot see (a function
        // return here), so its declared type is the only thing that roots it,
        // and the connection handler's socket follows from that.
        let result = extract(
            r#"
import type { Namespace, Socket } from "socket.io";
import { Server } from "socket.io";

function createWorkerNamespace({ io, namespace }: { io: Server; namespace: string }) {
  const worker: Namespace<ClientToServer, ServerToClient> = io.of(namespace);

  worker.on("connection", async (socket) => {
    socket.on("run:subscribe", async ({ runIds }) => {});
    socket.on("disconnect", () => {});
  });

  worker.emit("run:notify", { version: "1" });

  return worker;
}
"#,
        );
        assert_eq!(
            keys(&result.listeners),
            vec!["socket|CLIENT->SERVER|run:subscribe"]
        );
        assert_eq!(
            keys(&result.emitters),
            vec!["socket|SERVER->CLIENT|run:notify"],
            "a namespace broadcast is a server->client consumer"
        );
    }

    #[test]
    fn files_without_socket_io_imports_are_ignored() {
        let result = extract(
            r#"
const socket = connectSomething();
socket.on("chat:message", handler);
socket.emit("chat:message", "hi");
"#,
        );
        assert!(result.is_empty());
    }

    fn find(ops: &[SocketOp], canonical: &str) -> SocketOp {
        ops.iter()
            .find(|op| op.key.canonical() == canonical)
            .unwrap_or_else(|| panic!("missing op {canonical} in {ops:?}"))
            .clone()
    }

    #[test]
    fn typed_emitter_payload_captures_symbol_and_source() {
        // `socket.emit("payment:settled", payment)` where `payment: Payment`
        // and `Payment` is imported — the corpus's resolvable case.
        let result = extract(
            r#"
import { io } from "socket.io-client";
import type { Payment } from "./types/payment";
const socket = io("https://payments.internal");
const settle = (payment: Payment) => {
  socket.emit("payment:settled", payment);
};
"#,
        );
        let op = find(&result.emitters, "socket|CLIENT->SERVER|payment:settled");
        assert_eq!(op.payload_type_symbol.as_deref(), Some("Payment"));
        assert_eq!(op.payload_type_source.as_deref(), Some("./types/payment"));
    }

    #[test]
    fn typed_listener_payload_captures_handler_param_type() {
        // server `io.on("connection", socket => socket.on("event", (p: Payment) => …))`
        let result = extract(
            r#"
import { Server } from "socket.io";
import type { Payment } from "./types/payment";
const io = new Server(httpServer);
io.on("connection", (socket) => {
  socket.on("payment:received", (payment: Payment) => { void payment; });
});
"#,
        );
        let op = find(&result.listeners, "socket|CLIENT->SERVER|payment:received");
        assert_eq!(op.payload_type_symbol.as_deref(), Some("Payment"));
        assert_eq!(op.payload_type_source.as_deref(), Some("./types/payment"));
    }

    #[test]
    fn same_file_typed_payload_has_symbol_but_no_source() {
        // Payload type declared in the same file — symbol resolves, but there is
        // no import source (the SymbolRequest path resolves it against the
        // emitting file).
        let result = extract(
            r#"
import { io } from "socket.io-client";
interface Payment { id: string }
const socket = io("https://payments.internal");
const settle = (payment: Payment) => {
  socket.emit("payment:settled", payment);
};
"#,
        );
        let op = find(&result.emitters, "socket|CLIENT->SERVER|payment:settled");
        assert_eq!(op.payload_type_symbol.as_deref(), Some("Payment"));
        assert_eq!(op.payload_type_source, None);
    }

    #[test]
    fn untyped_and_inline_payloads_have_no_symbol() {
        let result = extract(
            r#"
import { io } from "socket.io-client";
import type { Payment } from "./types/payment";
const socket = io("https://chat.internal");
socket.emit("chat:message", "hello");
socket.emit("chat:object", { ok: true });
socket.on("chat:broadcast", (msg) => console.log(msg));
const settle = (payment: Payment[]) => { socket.emit("chat:array", payment); };
"#,
        );
        for canonical in [
            "socket|CLIENT->SERVER|chat:message",
            "socket|CLIENT->SERVER|chat:object",
            "socket|CLIENT->SERVER|chat:array",
        ] {
            let op = find(&result.emitters, canonical);
            assert_eq!(
                op.payload_type_symbol, None,
                "{canonical} should be unanchored"
            );
            assert_eq!(op.payload_type_source, None);
        }
        let listener = find(&result.listeners, "socket|SERVER->CLIENT|chat:broadcast");
        assert_eq!(listener.payload_type_symbol, None);
    }

    #[test]
    fn capitalized_global_payload_types_are_not_anchored() {
        // Global wrapper/utility types (Object, String, Function, …) are TS/lib
        // globals, not user types — annotating a payload with one must NOT create
        // a SymbolRequest (the sidecar would try to bundle the global). Copilot
        // review of #245 Phase 1.
        let result = extract(
            r#"
import { io } from "socket.io-client";
const socket = io("https://chat.internal");
const a = (p: Object) => { socket.emit("e:object", p); };
const b = (p: String) => { socket.emit("e:string", p); };
socket.on("e:fn", (p: Function) => p());
"#,
        );
        for canonical in [
            "socket|CLIENT->SERVER|e:object",
            "socket|CLIENT->SERVER|e:string",
        ] {
            let op = find(&result.emitters, canonical);
            assert_eq!(
                op.payload_type_symbol, None,
                "{canonical} (global type) must not be anchored"
            );
        }
        let listener = find(&result.listeners, "socket|SERVER->CLIENT|e:fn");
        assert_eq!(listener.payload_type_symbol, None);
    }

    #[test]
    fn typed_client_field_emits_are_recorded() {
        // carrick#659: the socket is built in one method, parked on a class
        // field, and the contract emits happen in later methods on
        // `this.<field>`. The field's declared type is what makes it a root —
        // the assignment goes through a method return, which is not traced.
        let result = extract(
            r#"
import type { Socket } from "socket.io-client";
import { io } from "socket.io-client";

class Supervisor {
  private notifications?: Socket<ServerToClient, ClientToServer>;

  private createSocket() {
    const socket = io(this.url);
    socket.on("run:notify", (msg) => this.handle(msg));
    return socket;
  }

  start() {
    this.notifications = this.createSocket();
  }

  subscribe(runIds: string[]) {
    this.notifications.emit("run:subscribe", { version: "1", runIds });
  }

  unsubscribe(runIds: string[]) {
    this.notifications.emit("run:unsubscribe", { version: "1", runIds });
  }
}
"#,
        );
        assert_eq!(
            keys(&result.emitters),
            vec![
                "socket|CLIENT->SERVER|run:subscribe",
                "socket|CLIENT->SERVER|run:unsubscribe",
            ],
            "emits on a typed client socket field are client->server consumers"
        );
        assert_eq!(
            keys(&result.listeners),
            vec!["socket|SERVER->CLIENT|run:notify"],
            "the local-binding listener still resolves"
        );
    }

    #[test]
    fn typed_server_socket_param_and_field_are_roots() {
        // The `socket.io` `Socket` type is the per-connection server socket, so
        // its listeners produce client->server and its emits consume
        // server->client — the mirror of the client field.
        let result = extract(
            r#"
import type { Socket } from "socket.io";

class Connection {
  private socket: Socket;

  constructor(socket: Socket) {
    this.socket = socket;
    socket.on("worker:ready", (msg) => this.ack(msg));
  }

  push() {
    this.socket.emit("worker:task", { id: 1 });
  }
}
"#,
        );
        assert_eq!(
            keys(&result.listeners),
            vec!["socket|CLIENT->SERVER|worker:ready"]
        );
        assert_eq!(
            keys(&result.emitters),
            vec!["socket|SERVER->CLIENT|worker:task"]
        );
    }

    #[test]
    fn constructor_parameter_property_is_a_field_root() {
        // `constructor(private readonly socket: Socket)` declares the field
        // and the parameter at once — a distinct AST node from a class prop.
        let result = extract(
            r#"
import type { Socket } from "socket.io";

class WorkerConnection {
  constructor(private readonly socket: Socket) {}

  register() {
    this.socket.on("worker:ready", (msg) => this.ack(msg));
    this.socket.emit("worker:task", { id: 1 });
  }
}
"#,
        );
        assert_eq!(
            keys(&result.listeners),
            vec!["socket|CLIENT->SERVER|worker:ready"]
        );
        assert_eq!(
            keys(&result.emitters),
            vec!["socket|SERVER->CLIENT|worker:task"]
        );
    }

    #[test]
    fn untyped_field_assigned_a_factory_call_is_a_root() {
        // No type annotation: the field becomes a root through the assignment
        // rule instead, including the private-field spelling.
        let result = extract(
            r#"
import { io } from "socket.io-client";

class Client {
  #socket;
  socket;

  connect() {
    this.#socket = io(this.url);
    this.socket = io(this.url);
  }

  send() {
    this.#socket.emit("private:ping", {});
    this.socket.emit("public:ping", {});
  }
}
"#,
        );
        assert_eq!(
            keys(&result.emitters),
            vec![
                "socket|CLIENT->SERVER|private:ping",
                "socket|CLIENT->SERVER|public:ping",
            ]
        );
    }

    #[test]
    fn optional_chained_field_calls_are_recorded() {
        // `this.socket?.emit(...)` is an optional call, a different AST node
        // from a plain call; it must not silently drop the op.
        let result = extract(
            r#"
import type { Socket } from "socket.io-client";

class Client {
  private socket?: Socket;

  send() {
    this.socket?.emit("run:subscribe", { version: "1" });
    this.socket?.on("run:notify", (msg) => this.handle(msg));
  }
}
"#,
        );
        assert_eq!(
            keys(&result.emitters),
            vec!["socket|CLIENT->SERVER|run:subscribe"]
        );
        assert_eq!(
            keys(&result.listeners),
            vec!["socket|SERVER->CLIENT|run:notify"]
        );
    }

    #[test]
    fn untyped_unassigned_fields_are_not_roots() {
        // A field that is neither declared with the socket type nor assigned a
        // socket root stays invisible — no phantom ops from `this.bus.emit`.
        let result = extract(
            r#"
import { io } from "socket.io-client";
const probe = io(url);

class Client {
  private bus = new EventEmitter();
  private socketish;

  send() {
    this.bus.emit("domain:event", {});
    this.socketish.emit("domain:other", {});
  }
}
"#,
        );
        assert!(
            result.is_empty(),
            "only socket-rooted fields produce ops, got {:?}",
            keys(&result.emitters)
        );
    }

    #[test]
    fn field_type_from_an_unrelated_module_is_not_a_root() {
        // The type name alone means nothing: `Socket` must come from a
        // socket.io module for the field to be a root.
        let result = extract(
            r#"
import type { Socket } from "net";
import { io } from "socket.io-client";
const probe = io(url);

class Client {
  private socket?: Socket;

  send() {
    this.socket.emit("domain:event", {});
  }
}
"#,
        );
        assert!(
            result.is_empty(),
            "a same-named foreign type is not a socket"
        );
    }
}
