//! Deterministic socket contract extraction.
//!
//! A socket event has a real operation key — event name plus message-flow
//! direction — and event names are string literals in idiomatic code, so
//! extraction is AST-based with no LLM. Listeners (`socket.on("x", ...)`)
//! are producers of the key for the direction they receive; emitters
//! (`socket.emit("x", ...)`) are consumers for the direction they send.
//!
//! Two rules admit a socket root, and the difference between them is the whole
//! shape of this module (carrick#1281):
//!
//! - **Socket.IO**, whose imports say which side of the wire a binding is on:
//!   `socket.io-client` factories make client sockets, `new Server(...)` from
//!   `socket.io` makes server roots, and the first parameter of a `connection`
//!   handler is a per-connection server socket. The direction is derived from
//!   the side, and the key is precise.
//! - **Any package the framework-detect step labels a socket client**
//!   (`DetectionResult::socket_clients` — `ws`, channel clients, hub clients),
//!   whose imports say nothing about sides because the same symbol is used on
//!   both. A binding constructed from one of those imports, declared with a
//!   type imported from one, or returned by a method OF one, is a socket root
//!   of unknown direction, and its ops are keyed `SocketDirection::Unknown`.
//!   An unknown-direction listener and emitter share that key, so the two
//!   sides of one contract still match; they cannot match a Socket.IO row for
//!   the same event name, which is correct — they are not the same transport.
//!
//! Nothing here reads a package NAME except `socket.io` / `socket.io-client`,
//! whose two extra rules buy the direction. Every other library reaches this
//! pass through the detector's list, so no library is special-cased and none
//! has to be recognised in Rust.
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
//!   imported from `socket.io` (server side), or a type alias for one of those
//!   (`type SupervisorSocket = Socket<…>`, which a file holding several
//!   sockets declares once and then uses on every field). The alias may be
//!   declared in the file or imported from a relative sibling, which is
//!   followed ONE hop through the binding resolver, re-export chains included;
//!   the declaring module is then read with these same module-local rules, so
//!   an alias of an imported alias stops there (carrick#670), and
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
//! - a socket taken out of a container (`this.sockets.get(id).emit(…)`) is not
//!   a root: the container's value type is never consulted (carrick#670),
//! - a socket reached as a member off an IMPORTED VALUE
//!   (`gateway.workerNamespace.emit(…)`) is not a root: only the alias hop
//!   above crosses a module boundary, and it carries a type, not a value
//!   (carrick#670),
//! - a type alias imported by a PACKAGE specifier is not followed: reaching it
//!   needs the sidecar's module resolution, as it does everywhere else,
//! - socket identity is tracked by binding name (`this.<field>` for fields),
//!   not full scope analysis, and is flat per file: two classes in one file
//!   with same-named socket fields share a root.
//!
//! What the unknown-direction half adds, and what it accepts:
//! - the call vocabulary widens to the protocol's own words — registration is
//!   `on`/`once`/`addListener`/`bind`/`subscribe` AND a function-valued second
//!   argument, sending is `emit`/`send`/`publish`/`trigger`. The callback
//!   requirement is what tells `channel.subscribe("orders", handler)` (a
//!   registration) from `client.subscribe("orders")` (a channel handle), which
//!   no name list could,
//! - a method-return IS traced, for unknown roots only: a channel client hands
//!   its channel back from a call (`const channel = client.subscribe("orders")`)
//!   and there is no other way to reach it. Socket.IO keeps its no-method-return
//!   rule, where a typed field is the idiom instead,
//! - the runtime's own event vocabulary is excluded as well as Socket.IO's
//!   reserved names ([`crate::event_emitter::RUNTIME_EVENTS`]), because a
//!   `ws` socket's `message`/`close`/`open` are the transport's lifecycle, not
//!   a contract, and a row on a name that generic would match any unrelated
//!   row sharing it,
//! - a handler argument is what makes a registration, and a bare identifier
//!   counts as one, so an options bag passed where a callback would go
//!   (`room.subscribe("orders", opts)`) reads as a registration. The
//!   alternative — accepting only a literal function expression — loses every
//!   `bind("evt", handleEvent)`, which is the commoner spelling,
//! - one shape reads a payload rather than an argument: `send(JSON.stringify({
//!   type: "x", … }))`, the plain-WebSocket idiom, where the event name is a
//!   discriminator field of the sent object. `type` and `event` are accepted as
//!   the discriminator key, and only `send` is read this way. That is a
//!   convention, not a structure, and it is the only place in this module where
//!   one is used; it earns its place because on a raw socket there is no other
//!   literal to key on,
//! - the RECEIVING side of that idiom is read on the same convention
//!   (carrick#1287). A raw socket delivers every message through one transport
//!   event ([`TRANSPORT_RECEIVE_EVENTS`]), so the application's event names are
//!   not in the registration call at all: they are the literals the handler
//!   compares the parsed payload's discriminator against. Inside such a
//!   handler, on a binding taken from `JSON.parse(...)`, three spellings of
//!   that one comparison each yield a listener on `socket|UNKNOWN|<literal>` —
//!   `switch (msg.type) { case "x": }`, `msg.type === "x"`, and a table
//!   indexed by the discriminator (`handlers[msg.type]`) whose literal keys
//!   name the events and whose handler parameters type the payloads. The
//!   registration itself stays declined, because `message` is the transport's
//!   name and not a contract,
//! - the receive side's guards are what keep that convention honest: the
//!   handler must be written AT the registration (a handler passed by name is
//!   a cross-function hop this pass does not take), the discriminator must be
//!   read off a binding initialized from `JSON.parse(...)` — so the same
//!   `switch` in a helper elsewhere in the file records nothing — the key must
//!   be one of [`ENVELOPE_EVENT_KEYS`], the literal must be written at the
//!   comparison, and a negated comparison (`msg.type !== "x"`) is not read:
//!   it names an event the branch does NOT handle. A dispatch table must be an
//!   object literal bound to a file-local binding or written inline, on the
//!   flat per-file binding rule the rest of the module uses,
//! - three receive-side spellings are known and not read, each a coverage gap
//!   rather than a judgement: a destructured discriminator
//!   (`const { type } = JSON.parse(raw)`) has no member expression to key on,
//!   `addEventListener("message", …)` is not in the registration vocabulary
//!   above, and a table held on a class field (`this.handlers[msg.type]`) is
//!   outside the binding map. They are tracked in carrick#1298,
//! - an unknown-direction listener and emitter for one event in ONE service
//!   match each other, because the key cannot express that they are the same
//!   side. Socket.IO's directional key does not have this exposure.

use crate::import_bindings::BindingResolver;
use crate::operation::{OperationKey, SocketDirection};
use crate::parser::parse_file;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use swc_common::errors::{ColorConfig, Handler};
use swc_common::{GLOBALS, Globals, SourceMap, Span, Spanned, sync::Lrc};
use swc_ecma_ast::{
    AssignExpr, AssignTarget, BinExpr, BinaryOp, Callee, ClassProp, Expr, ExprOrSpread, Function,
    ImportDecl, ImportSpecifier, Lit, MemberExpr, MemberProp, Module, ModuleExportName, NewExpr,
    ObjectLit, OptChainBase, OptChainExpr, Pat, PrivateProp, Prop, PropName, PropOrSpread,
    SimpleAssignTarget, SwitchStmt, TsEntityName, TsParamProp, TsParamPropParam, TsType,
    TsTypeAliasDecl, TsTypeAnn, TsUnionOrIntersectionType, VarDeclarator,
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
/// events. Shared with `crate::event_emitter`, which declines the same names:
/// a site this pass leaves alone because the event is reserved must not be
/// picked up there as an in-process bus contract instead (carrick#676).
pub(crate) const RESERVED_EVENTS: &[&str] = &[
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

/// Handler-registration vocabulary for unknown-direction roots. This is the
/// protocol's own words, not a library list — the same footing
/// `crate::event_emitter::SUBSCRIBE_METHODS` stands on. A call only counts as a
/// registration when it ALSO passes a handler (see
/// [`OpCollector::handler_argument`]), which is what separates
/// `channel.subscribe("orders", handler)` from `client.subscribe("orders")`.
const UNKNOWN_SUBSCRIBE_METHODS: &[&str] = &["on", "once", "addListener", "bind", "subscribe"];

/// Sending vocabulary for unknown-direction roots, with the event name as the
/// first argument.
const UNKNOWN_PUBLISH_METHODS: &[&str] = &["emit", "send", "publish", "trigger"];

/// Object keys accepted as the event discriminator of a serialized envelope
/// (`send(JSON.stringify({ type: "order.created", … }))`). A convention rather
/// than a structure, and deliberately the only one in this module: a raw socket
/// carries no other literal to key on.
const ENVELOPE_EVENT_KEYS: &[&str] = &["type", "event"];

/// Transport events that DELIVER an application message to user code, and so
/// carry an envelope whose discriminator names the real event (carrick#1287).
///
/// One name, and the reason it is one: a raw socket is message-framed, so each
/// `message` is exactly one envelope. The byte-stream spelling (`data`) is not
/// here — a chunk is not a message, and a discriminator read off an unframed
/// chunk would be keyed on whatever happened to arrive in it.
///
/// These names are also in [`crate::event_emitter::RUNTIME_EVENTS`], so the
/// registration itself is still declined as transport lifecycle; what this list
/// adds is the walk INTO the handler.
const TRANSPORT_RECEIVE_EVENTS: &[&str] = &["message"];

/// Extract socket operations from the service's TS/JS files.
///
/// `socket_clients` is `DetectionResult::socket_clients` — the packages the
/// framework-detect step labelled socket/realtime clients. Socket.IO's rules do
/// not consult it; it is the gate for every other library, so an empty slice
/// (local mode, a repo whose detection predates the field) leaves this pass
/// extracting exactly what it extracted before.
pub fn scan_files(service_files: &[PathBuf], socket_clients: &[String]) -> SocketExtraction {
    let mut extraction = SocketExtraction::default();
    // One resolver for the whole service: it caches each module's export
    // table, and a module that declares a shared socket type is read by every
    // file that imports it (carrick#670).
    let mut resolver = AliasResolver::default();
    for file in service_files {
        let is_script = file
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| matches!(ext, "ts" | "tsx" | "js" | "jsx"));
        if !is_script {
            continue;
        }
        extraction.merge(extract_from_ts_file(file, &mut resolver, socket_clients));
    }
    debug!(
        listeners = extraction.listeners.len(),
        emitters = extraction.emitters.len(),
        "Socket extraction complete"
    );
    extraction
}

fn extract_from_ts_file(
    file_path: &Path,
    resolver: &mut AliasResolver,
    socket_clients: &[String],
) -> SocketExtraction {
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));

    let globals = Globals::new();
    GLOBALS.set(&globals, || {
        let Some(module) = parse_file(file_path, &cm, &handler) else {
            return SocketExtraction::default();
        };

        // Pass A: collect socket-rooted binding names, to fixpoint.
        let mut roots = SocketRoots::for_clients(socket_clients);
        collect_roots(&module, &mut roots);

        // Pass A2 (carrick#670): a field whose declared type is a socket alias
        // IMPORTED from a sibling module — the module-local rules cannot see
        // the declaration, so follow the import one hop and re-collect.
        if resolve_imported_aliases(&module, file_path, &mut roots, resolver) {
            collect_roots(&module, &mut roots);
        }

        if roots.size() == 0 {
            return SocketExtraction::default();
        }

        // Pass B0 (carrick#1287): the file's dispatch tables, so a delivery
        // handler that indexes one by the envelope discriminator can be read
        // back to the events its keys name.
        let mut tables = TableCollector {
            tables: HashMap::new(),
            cm: cm.clone(),
        };
        module.visit_with(&mut tables);

        // Pass B: collect ops on socket-rooted identifiers.
        let mut ops = OpCollector {
            cm: cm.clone(),
            file_path,
            roots: &roots,
            tables: &tables.tables,
            extraction: SocketExtraction::default(),
        };
        module.visit_with(&mut ops);
        ops.extraction
    })
}

/// Cross-module resolution of a socket type alias, with the two caches that
/// keep it cheap: the binding resolver's export tables, and one verdict per
/// (declaring module, exported name) — a shared socket type is imported by
/// many files and must be read once (carrick#670).
#[derive(Default)]
struct AliasResolver {
    bindings: BindingResolver,
    verdicts: HashMap<(PathBuf, String), Option<SocketKind>>,
}

impl AliasResolver {
    /// What the type `exported_name` of `specifier`, as imported by
    /// `importer`, resolves to — `None` when it is not a socket type, when the
    /// specifier is not relative, or when the declaration cannot be reached.
    ///
    /// ONE import hop. Re-export hops on the way to the declaring module are
    /// followed by the binding resolver, but if the declaring module's alias is
    /// ITSELF an imported alias it stops there: the declaring module is read
    /// with the module-local rules only.
    fn kind_of_imported_type(
        &mut self,
        importer: &Path,
        specifier: &str,
        exported_name: &str,
        socket_clients: &[String],
    ) -> Option<SocketKind> {
        if !specifier.starts_with('.') {
            return None;
        }
        let declaring = self
            .bindings
            .resolve_type(importer, specifier, exported_name)?;
        let key = (declaring.clone(), exported_name.to_string());
        if let Some(verdict) = self.verdicts.get(&key) {
            return *verdict;
        }
        let verdict = declaring_module_kind(&declaring, exported_name, socket_clients);
        self.verdicts.insert(key, verdict);
        verdict
    }
}

/// Read the module that declares the alias with the module-local rules and ask
/// what its own collector made of the name.
fn declaring_module_kind(
    file: &Path,
    exported_name: &str,
    socket_clients: &[String],
) -> Option<SocketKind> {
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
    let globals = Globals::new();
    GLOBALS.set(&globals, || {
        let module = parse_file(file, &cm, &handler)?;
        let mut roots = SocketRoots::for_clients(socket_clients);
        collect_roots(&module, &mut roots);
        if roots.client_socket_types.contains(exported_name) {
            Some(SocketKind::Client)
        } else if roots.server_socket_types.contains(exported_name) {
            Some(SocketKind::Server)
        } else if roots.unknown_socket_types.contains(exported_name) {
            Some(SocketKind::Unknown)
        } else {
            None
        }
    })
}

/// Run the root collector to fixpoint: a connection-handler socket needs the
/// server root known first, and an alias may be declared after its use.
fn collect_roots(module: &Module, roots: &mut SocketRoots) {
    loop {
        let before = roots.size();
        let mut collector = RootCollector { roots };
        module.visit_with(&mut collector);
        if roots.size() == before {
            break;
        }
    }
}

/// Admit any imported socket type alias this file needs, and report whether
/// anything was admitted.
///
/// Gated on the file actually writing a socket-shaped call whose receiver is
/// still unrooted, and then on that receiver having a declared type name: a
/// file with nothing to gain resolves nothing, so the cost lands only where
/// the answer can change.
fn resolve_imported_aliases(
    module: &Module,
    file_path: &Path,
    roots: &mut SocketRoots,
    resolver: &mut AliasResolver,
) -> bool {
    let mut wanted = UnrootedCallRoots {
        roots,
        keys: HashSet::new(),
    };
    module.visit_with(&mut wanted);
    let keys = wanted.keys;
    if keys.is_empty() {
        return false;
    }

    let mut admitted = false;
    for key in keys {
        let Some(type_name) = roots.declared_type_names.get(&key).cloned() else {
            continue;
        };
        if roots.client_socket_types.contains(&type_name)
            || roots.server_socket_types.contains(&type_name)
            || roots.unknown_socket_types.contains(&type_name)
        {
            continue;
        }
        let Some((specifier, exported)) = roots.imported_types.get(&type_name).cloned() else {
            continue;
        };
        let socket_clients = roots.socket_clients.clone();
        let Some(kind) =
            resolver.kind_of_imported_type(file_path, &specifier, &exported, &socket_clients)
        else {
            continue;
        };
        match kind {
            SocketKind::Client => roots.client_socket_types.insert(type_name),
            SocketKind::Server => roots.server_socket_types.insert(type_name),
            SocketKind::Unknown => roots.unknown_socket_types.insert(type_name),
        };
        admitted = true;
    }
    admitted
}

/// Receivers of a socket-shaped call (`.on`/`.once`/`.emit` with a literal
/// event) that no rule has rooted yet.
struct UnrootedCallRoots<'a> {
    roots: &'a SocketRoots,
    keys: HashSet<String>,
}

impl UnrootedCallRoots<'_> {
    fn note(&mut self, member: &MemberExpr, args: &[ExprOrSpread]) {
        if let Some(prop) = member.prop.as_ident()
            && (matches!(prop.sym.as_ref(), "on" | "once" | "emit")
                || UNKNOWN_SUBSCRIBE_METHODS.contains(&prop.sym.as_ref())
                || UNKNOWN_PUBLISH_METHODS.contains(&prop.sym.as_ref()))
            && args
                .first()
                .is_some_and(|arg| matches!(&*arg.expr, Expr::Lit(Lit::Str(_))))
            && let Some(root) = chain_root(&member.obj)
            && self.roots.kind_of(&root).is_none()
        {
            self.keys.insert(root);
        }
    }
}

impl Visit for UnrootedCallRoots<'_> {
    fn visit_call_expr(&mut self, node: &swc_ecma_ast::CallExpr) {
        if let Callee::Expr(callee) = &node.callee
            && let Expr::Member(member) = &**callee
        {
            self.note(member, &node.args);
        }
        node.visit_children_with(self);
    }

    fn visit_opt_chain_expr(&mut self, node: &OptChainExpr) {
        if let OptChainBase::Call(call) = &*node.base {
            match &*call.callee {
                Expr::Member(member) => self.note(member, &call.args),
                Expr::OptChain(inner) => {
                    if let OptChainBase::Member(member) = &*inner.base {
                        self.note(member, &call.args);
                    }
                }
                _ => {}
            }
        }
        node.visit_children_with(self);
    }
}

/// Which side of the wire a socket-rooted binding sits on — or `Unknown`, for a
/// library whose imports do not distinguish the sides (carrick#1281).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SocketKind {
    Client,
    Server,
    Unknown,
}

#[derive(Default)]
struct SocketRoots {
    /// `DetectionResult::socket_clients`: the packages whose value and type
    /// imports admit an unknown-direction socket root. Empty → this file's
    /// unknown-direction rules never fire.
    socket_clients: Vec<String>,
    /// Local names of `socket.io-client` factories (`io`, `connect`, ...).
    client_factories: HashSet<String>,
    /// Local names of the `socket.io` `Server` class.
    server_classes: HashSet<String>,
    /// Local names of the `Socket` TYPE imported from `socket.io-client`.
    /// A binding declared with it holds a client socket however it was
    /// initialized (carrick#659).
    client_socket_types: HashSet<String>,
    /// Local type name → (module specifier, name inside that module) for every
    /// named type-position import from a relative specifier. Feeds the
    /// imported-alias hop (carrick#670).
    imported_types: HashMap<String, (String, String)>,
    /// Binding key (`socket`, `this.socket`) → the simple type name it was
    /// declared with, when that name is not an admitted socket type. The
    /// imported-alias hop reads this to know WHICH name to follow.
    declared_type_names: HashMap<String, String>,
    /// Local names of the `Socket` TYPE imported from `socket.io` — the
    /// per-connection server socket, not the server root.
    server_socket_types: HashSet<String>,
    /// Local names imported as VALUES from a `socket_clients` package: the
    /// constructors and factories that make an unknown-direction socket
    /// (`new WebSocket(url)`, `createClient(...)`). Both call and `new`
    /// positions read this set, because a package publishes one or the other
    /// and nothing structural says which.
    unknown_constructors: HashSet<String>,
    /// Local names imported from a `socket_clients` package used in TYPE
    /// position (`private conn?: WebSocket`). Same names as
    /// `unknown_constructors` — an import specifier is admitted to both, since
    /// a value import and a type import are told apart by use, not by shape.
    unknown_socket_types: HashSet<String>,
    /// Bindings holding client sockets (`const s = io(url)`). Class fields are
    /// keyed `this.<name>` / `this.#<name>`.
    client_sockets: HashSet<String>,
    /// Bindings holding server roots (`const io = new Server(...)`) or
    /// per-connection sockets (`io.on("connection", (socket) => ...)`).
    server_sockets: HashSet<String>,
    /// Bindings holding a socket from a `socket_clients` package, whose
    /// direction no rule can derive.
    unknown_sockets: HashSet<String>,
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
    /// Empty roots that will admit unknown-direction sockets for
    /// `socket_clients`.
    fn for_clients(socket_clients: &[String]) -> Self {
        Self {
            socket_clients: socket_clients.to_vec(),
            ..Self::default()
        }
    }

    /// Is this import specifier one of the detected socket-client packages?
    /// Exact or `<entry>/` prefix — the same convention
    /// `file_imports_messaging_client` uses, so a package gates the same way
    /// everywhere.
    fn is_socket_client(&self, specifier: &str) -> bool {
        self.socket_clients
            .iter()
            .any(|pkg| specifier == pkg || specifier.starts_with(&format!("{}/", pkg)))
    }

    fn size(&self) -> usize {
        self.client_factories.len()
            + self.server_classes.len()
            + self.client_socket_types.len()
            + self.server_socket_types.len()
            + self.client_sockets.len()
            + self.server_sockets.len()
            + self.unknown_constructors.len()
            + self.unknown_socket_types.len()
            + self.unknown_sockets.len()
            + self.type_imports.len()
            + self.binding_types.len()
    }

    fn record(&mut self, key: String, kind: SocketKind) {
        match kind {
            SocketKind::Client => self.client_sockets.insert(key),
            SocketKind::Server => self.server_sockets.insert(key),
            SocketKind::Unknown => self.unknown_sockets.insert(key),
        };
    }

    fn kind_of(&self, key: &str) -> Option<SocketKind> {
        if self.client_sockets.contains(key) {
            Some(SocketKind::Client)
        } else if self.server_sockets.contains(key) {
            Some(SocketKind::Server)
        } else if self.unknown_sockets.contains(key) {
            Some(SocketKind::Unknown)
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
                    } else if self.unknown_socket_types.contains(name) {
                        Some(SocketKind::Unknown)
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

    /// The simple type NAME a declared type carries, when it is one named
    /// reference (`X`, `X<A, B>`, `X | undefined`). The alias hop needs the
    /// name of a type it could not resolve locally.
    fn simple_type_name(ty: &TsType) -> Option<String> {
        match ty {
            TsType::TsTypeRef(type_ref) => match &type_ref.type_name {
                TsEntityName::Ident(ident) => Some(ident.sym.to_string()),
                TsEntityName::TsQualifiedName(_) => None,
            },
            TsType::TsUnionOrIntersectionType(TsUnionOrIntersectionType::TsUnionType(union)) => {
                union
                    .types
                    .iter()
                    .find_map(|member| Self::simple_type_name(member))
            }
            TsType::TsParenthesizedType(paren) => Self::simple_type_name(&paren.type_ann),
            _ => None,
        }
    }

    /// Remember what a binding was declared with when the type is not (yet) an
    /// admitted socket type, so the imported-alias hop knows which name to
    /// follow for that binding.
    fn note_declared_type(&mut self, key: &str, type_ann: &TsTypeAnn) {
        if let Some(name) = Self::simple_type_name(&type_ann.type_ann) {
            self.declared_type_names.insert(key.to_string(), name);
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
                    Expr::Ident(factory)
                        if self.unknown_constructors.contains(factory.sym.as_ref()) =>
                    {
                        Some(SocketKind::Unknown)
                    }
                    // The method-return hop, for unknown roots only: a channel
                    // client hands the object that carries the events back from
                    // a call (`const channel = client.subscribe("orders")`), so
                    // refusing to trace it would make the whole library
                    // invisible. A Socket.IO root never reaches this arm.
                    Expr::Member(member) => {
                        match member_root(member).map(|root| self.kind_of(&root)) {
                            Some(Some(SocketKind::Unknown)) => Some(SocketKind::Unknown),
                            _ => None,
                        }
                    }
                    _ => None,
                },
                _ => None,
            },
            Expr::New(NewExpr { callee, .. }) => match &**callee {
                Expr::Ident(class) if self.server_classes.contains(class.sym.as_ref()) => {
                    Some(SocketKind::Server)
                }
                Expr::Ident(class) if self.unknown_constructors.contains(class.sym.as_ref()) => {
                    Some(SocketKind::Unknown)
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
        } else if self.unknown_sockets.contains(root) {
            // Nothing in the library said which side this binding is on, so
            // neither does the key. Both roles land on one direction, which is
            // what lets the two sides of the contract still meet.
            Some(SocketDirection::Unknown)
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
                // Same binding, keyed for the alias hop: the name the DECLARING
                // module publishes, which a renaming import changes.
                let exported = named
                    .imported
                    .as_ref()
                    .map(|name| match name {
                        ModuleExportName::Ident(ident) => ident.sym.to_string(),
                        ModuleExportName::Str(s) => s.value.to_string(),
                    })
                    .unwrap_or_else(|| named.local.sym.to_string());
                self.roots
                    .imported_types
                    .insert(named.local.sym.to_string(), (source.to_string(), exported));
            }
        }
        if source != "socket.io" && source != "socket.io-client" {
            // Any import from a package the detector labelled a socket client
            // admits its local names as unknown-direction roots. The specifier
            // rule above wins: a detection that also lists `socket.io` leaves
            // Socket.IO's precise directional rules in force, never replaced by
            // the general ones.
            if self.roots.is_socket_client(source) {
                for specifier in &node.specifiers {
                    let local = match specifier {
                        ImportSpecifier::Default(default) => default.local.sym.to_string(),
                        ImportSpecifier::Named(named) => named.local.sym.to_string(),
                        // `import * as x from "ws"` binds a namespace, not a
                        // socket: `x.WebSocket` is the constructor and member
                        // roots are not traced here (see the module docs).
                        ImportSpecifier::Namespace(_) => continue,
                    };
                    self.roots.unknown_constructors.insert(local.clone());
                    self.roots.unknown_socket_types.insert(local);
                }
            }
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

    fn visit_ts_type_alias_decl(&mut self, node: &TsTypeAliasDecl) {
        // `type SupervisorSocket = Socket<ServerToClient, ClientToServer>` —
        // a file that names its socket type once and then declares every field
        // with the alias (carrick#670). The alias IS the admitted type, so it
        // joins the same set and every rule that reads a declared type sees it.
        // The fixpoint loop resolves declaration order and a chain of aliases.
        if let Some(kind) = self.roots.kind_of_type(&node.type_ann) {
            let name = node.id.sym.to_string();
            match kind {
                SocketKind::Client => self.roots.client_socket_types.insert(name),
                SocketKind::Server => self.roots.server_socket_types.insert(name),
                SocketKind::Unknown => self.roots.unknown_socket_types.insert(name),
            };
        }
        node.visit_children_with(self);
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
            match kind {
                Some(kind) => self.roots.record(key, kind),
                None => {
                    if let Some(type_ann) = node.type_ann.as_deref() {
                        self.roots.note_declared_type(&key, type_ann);
                    }
                }
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
        match kind {
            Some(kind) => self.roots.record(key, kind),
            None => {
                if let Some(type_ann) = node.type_ann.as_deref() {
                    self.roots.note_declared_type(&key, type_ann);
                }
            }
        }
        node.visit_children_with(self);
    }

    fn visit_ts_param_prop(&mut self, node: &TsParamProp) {
        // `constructor(private readonly socket: Socket)` declares the field and
        // the parameter in one breath, so the same declared-type rule has to
        // reach it or every later `this.socket.on(…)` is invisible.
        if let TsParamPropParam::Ident(ident) = &node.param
            && let Some(type_ann) = ident.type_ann.as_deref()
        {
            let key = format!("this.{}", ident.id.sym);
            match self.roots.kind_of_type_ann(type_ann) {
                Some(kind) => self.roots.record(key, kind),
                None => self.roots.note_declared_type(&key, type_ann),
            }
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
            match self.roots.kind_of_type_ann(type_ann) {
                Some(kind) => self.roots.record(ident.id.sym.to_string(), kind),
                None => self
                    .roots
                    .note_declared_type(ident.id.sym.as_ref(), type_ann),
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
    /// File-local dispatch tables, for the receive side of the envelope idiom
    /// (carrick#1287).
    tables: &'a HashMap<String, Vec<TableEntry>>,
    extraction: SocketExtraction,
}

/// Is this event name the transport's own vocabulary rather than the
/// codebase's? The union of Socket.IO's reserved names and the runtime's
/// (`message`, `close`, `open`, …): a raw socket's lifecycle events are emitted
/// AT user code by the transport, so they are not a contract between two parts
/// of a codebase, and they are generic enough that a row on one would match any
/// unrelated row sharing the name.
fn is_transport_event(event: &str) -> bool {
    RESERVED_EVENTS.contains(&event) || crate::event_emitter::RUNTIME_EVENTS.contains(&event)
}

/// The event name a serialized envelope carries: the string-literal value of a
/// `type`/`event` key in `JSON.stringify({ … })`, or in an object literal sent
/// directly. `None` for a dynamic name, a spread, or any other argument — the
/// same literal-only rule the rest of the pass applies.
fn envelope_event_name(expr: &Expr) -> Option<String> {
    let object = match expr {
        Expr::Object(object) => object,
        Expr::Call(call) => {
            let Callee::Expr(callee) = &call.callee else {
                return None;
            };
            let Expr::Member(member) = &**callee else {
                return None;
            };
            if !is_json_method(member, "stringify") {
                return None;
            }
            match call.args.first().map(|arg| &*arg.expr) {
                Some(Expr::Object(object)) => object,
                _ => return None,
            }
        }
        Expr::Paren(paren) => return envelope_event_name(&paren.expr),
        Expr::TsAs(as_expr) => return envelope_event_name(&as_expr.expr),
        _ => return None,
    };
    object.props.iter().find_map(|prop| {
        let swc_ecma_ast::PropOrSpread::Prop(prop) = prop else {
            return None;
        };
        let swc_ecma_ast::Prop::KeyValue(key_value) = &**prop else {
            return None;
        };
        let key = match &key_value.key {
            PropName::Ident(ident) => ident.sym.to_string(),
            PropName::Str(name) => name.value.to_string(),
            _ => return None,
        };
        if !ENVELOPE_EVENT_KEYS.contains(&key.as_str()) {
            return None;
        }
        match &*key_value.value {
            Expr::Lit(Lit::Str(value)) => Some(value.value.to_string()),
            _ => None,
        }
    })
}

/// `JSON.<method>` as a callee: the serializer on the way out and the parser on
/// the way back in are the two ends of one convention, read the same way.
fn is_json_method(member: &MemberExpr, method: &str) -> bool {
    matches!(&*member.obj, Expr::Ident(ident) if ident.sym.as_ref() == "JSON")
        && member
            .prop
            .as_ident()
            .is_some_and(|prop| prop.sym.as_ref() == method)
}

/// Strip the wrappers that carry an expression through unchanged, so a cast or
/// a parenthesis never hides a shape this pass reads.
fn unwrap_expr(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(paren) => unwrap_expr(&paren.expr),
        Expr::TsAs(as_expr) => unwrap_expr(&as_expr.expr),
        Expr::TsSatisfies(satisfies) => unwrap_expr(&satisfies.expr),
        Expr::TsNonNull(non_null) => unwrap_expr(&non_null.expr),
        Expr::Await(awaited) => unwrap_expr(&awaited.arg),
        other => other,
    }
}

/// Is this expression `JSON.parse(...)`? The argument is not read: what it
/// establishes is that the binding holds a decoded envelope, not what was
/// decoded.
fn is_json_parse(expr: &Expr) -> bool {
    let Expr::Call(call) = unwrap_expr(expr) else {
        return false;
    };
    let Callee::Expr(callee) = &call.callee else {
        return false;
    };
    matches!(&**callee, Expr::Member(member) if is_json_method(member, "parse"))
}

/// Is this expression the discriminator of one of `bindings` — `msg.type`,
/// `payload.event`, `msg?.type` — using the same key convention the sending
/// side keys on ([`ENVELOPE_EVENT_KEYS`])?
fn is_envelope_discriminator(expr: &Expr, bindings: &HashSet<String>) -> bool {
    let member = match unwrap_expr(expr) {
        Expr::Member(member) => member,
        // `msg?.type` is the same read with a guard on it.
        Expr::OptChain(opt) => match &*opt.base {
            OptChainBase::Member(member) => member,
            OptChainBase::Call(_) => return false,
        },
        _ => return false,
    };
    let Some(prop) = member.prop.as_ident() else {
        return false;
    };
    if !ENVELOPE_EVENT_KEYS.contains(&prop.sym.as_ref()) {
        return false;
    }
    matches!(unwrap_expr(&member.obj), Expr::Ident(ident) if bindings.contains(ident.sym.as_ref()))
}

/// The string a literal expression carries, if it is one.
fn string_literal(expr: &Expr) -> Option<String> {
    match unwrap_expr(expr) {
        Expr::Lit(Lit::Str(value)) => Some(value.value.to_string()),
        _ => None,
    }
}

/// The static name of an object key (`{ "order.created": … }`, `{ ping: … }`).
/// A computed or numeric key names no event.
fn prop_name_literal(key: &PropName) -> Option<String> {
    match key {
        PropName::Ident(ident) => Some(ident.sym.to_string()),
        PropName::Str(name) => Some(name.value.to_string()),
        _ => None,
    }
}

/// Payload type symbol of a handler's first parameter, which is where a
/// dispatch table carries the payload type the switch/comparison spellings do
/// not have.
fn first_param_symbol(param: Option<&Pat>) -> Option<String> {
    match param? {
        Pat::Ident(ident) => ident.type_ann.as_deref().and_then(named_type_symbol),
        _ => None,
    }
}

/// One entry of a dispatch table: an event name written as a key, the line the
/// handler is defined on, and the payload type its parameter declares.
#[derive(Debug, Clone)]
struct TableEntry {
    event: String,
    line: u32,
    payload_symbol: Option<String>,
}

/// The entries an object literal contributes when it is dispatched on: every
/// key whose value is callable. A non-callable value (`{ retries: 3 }`) names
/// nothing, which is what keeps a settings object indexed by a discriminator
/// from becoming a row per setting.
fn table_entries(object: &ObjectLit, cm: &SourceMap) -> Vec<TableEntry> {
    object
        .props
        .iter()
        .filter_map(|prop| {
            let PropOrSpread::Prop(prop) = prop else {
                return None;
            };
            let (event, payload_symbol, span) = match &**prop {
                Prop::KeyValue(key_value) => {
                    let event = prop_name_literal(&key_value.key)?;
                    let symbol = match unwrap_expr(&key_value.value) {
                        Expr::Arrow(arrow) => first_param_symbol(arrow.params.first()),
                        Expr::Fn(function) => function_first_param_symbol(&function.function),
                        // A handler referenced by name is callable; its
                        // parameter type is a hop away and stays unanchored.
                        Expr::Ident(_) => None,
                        _ => return None,
                    };
                    (event, symbol, key_value.key.span())
                }
                Prop::Method(method) => (
                    prop_name_literal(&method.key)?,
                    function_first_param_symbol(&method.function),
                    method.key.span(),
                ),
                // `{ handleOrder }`: the key names the event, the binding is
                // the handler.
                Prop::Shorthand(ident) => (ident.sym.to_string(), None, ident.span),
                _ => return None,
            };
            Some(TableEntry {
                event,
                line: cm.lookup_char_pos(span.lo).line as u32,
                payload_symbol,
            })
        })
        .collect()
}

fn function_first_param_symbol(function: &Function) -> Option<String> {
    first_param_symbol(function.params.first().map(|param| &param.pat))
}

/// Every file-local `const handlers = { … }`, keyed by binding name, so a
/// dispatch on `handlers[msg.type]` can be read back. Flat per file, the same
/// identity rule socket roots use.
struct TableCollector {
    tables: HashMap<String, Vec<TableEntry>>,
    cm: Lrc<SourceMap>,
}

impl Visit for TableCollector {
    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        if let Pat::Ident(ident) = &node.name
            && let Some(init) = &node.init
            && let Expr::Object(object) = unwrap_expr(init)
        {
            let entries = table_entries(object, &self.cm);
            if !entries.is_empty() {
                self.tables.insert(ident.id.sym.to_string(), entries);
            }
        }
        node.visit_children_with(self);
    }
}

/// Bindings inside a delivery handler that hold a decoded envelope
/// (`const msg = JSON.parse(raw)`). Only a discriminator read off one of these
/// is read as an event name: without it, any `x.type === "y"` in the handler
/// would be one.
#[derive(Default)]
struct EnvelopeBindings {
    names: HashSet<String>,
}

impl Visit for EnvelopeBindings {
    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        if let Pat::Ident(ident) = &node.name
            && let Some(init) = &node.init
            && is_json_parse(init)
        {
            self.names.insert(ident.id.sym.to_string());
        }
        node.visit_children_with(self);
    }
}

/// The three spellings of one comparison, collected from a delivery handler's
/// body. The same shape `crate::swc_scanner`'s method-guard visitor reads for
/// HTTP verbs: a discriminant, and the literals written against it.
struct DispatchCollector<'a> {
    bindings: &'a HashSet<String>,
    tables: &'a HashMap<String, Vec<TableEntry>>,
    cm: Lrc<SourceMap>,
    found: Vec<TableEntry>,
    seen: HashSet<String>,
}

impl DispatchCollector<'_> {
    /// Record one event, once. A handler dispatches on an event once however
    /// many spellings name it, so the first site wins and a repeat is dropped
    /// rather than doubling the key.
    fn record(&mut self, event: String, span: Span, payload_symbol: Option<String>) {
        if is_transport_event(&event) || !self.seen.insert(event.clone()) {
            return;
        }
        let line = self.cm.lookup_char_pos(span.lo).line as u32;
        self.found.push(TableEntry {
            event,
            line,
            payload_symbol,
        });
    }

    fn record_entry(&mut self, entry: TableEntry) {
        if is_transport_event(&entry.event) || !self.seen.insert(entry.event.clone()) {
            return;
        }
        self.found.push(entry);
    }
}

impl Visit for DispatchCollector<'_> {
    fn visit_switch_stmt(&mut self, node: &SwitchStmt) {
        if is_envelope_discriminator(&node.discriminant, self.bindings) {
            for case in &node.cases {
                if let Some(test) = case.test.as_deref()
                    && let Some(event) = string_literal(test)
                {
                    self.record(event, case.span, None);
                }
            }
        }
        node.visit_children_with(self);
    }

    fn visit_bin_expr(&mut self, node: &BinExpr) {
        // Equality only: `msg.type !== "x"` names the event this branch does
        // NOT handle, and reading it would record a listener for it.
        if matches!(node.op, BinaryOp::EqEq | BinaryOp::EqEqEq) {
            let literal = if is_envelope_discriminator(&node.left, self.bindings) {
                string_literal(&node.right)
            } else if is_envelope_discriminator(&node.right, self.bindings) {
                string_literal(&node.left)
            } else {
                None
            };
            if let Some(event) = literal {
                self.record(event, node.span, None);
            }
        }
        node.visit_children_with(self);
    }

    fn visit_member_expr(&mut self, node: &MemberExpr) {
        if let MemberProp::Computed(computed) = &node.prop
            && is_envelope_discriminator(&computed.expr, self.bindings)
        {
            let tables = self.tables;
            let cm = self.cm.clone();
            let entries = match unwrap_expr(&node.obj) {
                Expr::Ident(ident) => tables.get(ident.sym.as_ref()).cloned(),
                Expr::Object(object) => Some(table_entries(object, &cm)),
                _ => None,
            };
            for entry in entries.into_iter().flatten() {
                self.record_entry(entry);
            }
        }
        node.visit_children_with(self);
    }
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
        let Some(kind) = self.roots.kind_of(&root_name) else {
            return;
        };
        let method = prop.sym.as_ref();

        // The vocabulary a root answers to. Socket.IO's two words are the
        // directional half; an unknown-direction root reads the protocol
        // vocabulary, because its library's word for "register a handler" is
        // whatever that library chose.
        let (is_listener, is_emitter) = match kind {
            SocketKind::Client | SocketKind::Server => {
                (matches!(method, "on" | "once"), method == "emit")
            }
            SocketKind::Unknown => (
                UNKNOWN_SUBSCRIBE_METHODS.contains(&method)
                    && Self::handler_argument(args).is_some(),
                UNKNOWN_PUBLISH_METHODS.contains(&method),
            ),
        };

        // `send(JSON.stringify({ type: "x", … }))`: the raw-WebSocket idiom,
        // where the event name is a field of the payload rather than an
        // argument. `send` only — a library whose sending method takes the
        // event name as an argument has one there, and reading an object it was
        // handed instead would key on whatever field happened to be called
        // `type`.
        if kind == SocketKind::Unknown
            && method == "send"
            && !args
                .first()
                .is_some_and(|arg| matches!(&*arg.expr, Expr::Lit(Lit::Str(_))))
        {
            if let Some(event) = args.first().and_then(|arg| envelope_event_name(&arg.expr))
                && !is_transport_event(&event)
            {
                self.extraction.emitters.push(SocketOp {
                    key: OperationKey::socket(event, SocketDirection::Unknown),
                    file_path: self.file_path.to_path_buf(),
                    line: self.cm.lookup_char_pos(span.lo).line as u32,
                    payload_type_symbol: None,
                    payload_type_source: None,
                });
            }
            return;
        }

        // The RECEIVING side of that idiom (carrick#1287): one transport event
        // delivers every message, so the contract names are inside the handler,
        // not in this call. The registration itself still records nothing — the
        // rows are the literals the handler discriminates on.
        if kind == SocketKind::Unknown
            && UNKNOWN_SUBSCRIBE_METHODS.contains(&method)
            && let Some(first) = args.first()
            && let Expr::Lit(Lit::Str(event)) = &*first.expr
            && TRANSPORT_RECEIVE_EVENTS.contains(&event.value.as_ref())
        {
            if let Some(handler) = Self::handler_argument(args)
                && let Some(direction) = self.roots.direction_for(&root_name, true)
            {
                self.record_envelope_listeners(handler, direction);
            }
            return;
        }

        let reserved = |event: &str| match kind {
            // An unknown-direction root's transport lifecycle (`message`,
            // `close`, `open`) is not a contract either, and its names are
            // generic enough to match anything.
            SocketKind::Unknown => is_transport_event(event),
            _ => RESERVED_EVENTS.contains(&event),
        };

        if (is_listener || is_emitter)
            && let Some(first) = args.first()
            && let Expr::Lit(Lit::Str(event)) = &*first.expr
            && !reserved(event.value.as_ref())
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

    /// Listener rows for every event a delivery handler dispatches on
    /// (carrick#1287). The handler must be written at the registration: a
    /// handler passed by name is a cross-function hop this pass does not take,
    /// the same rule that keeps a socket out of a container invisible.
    fn record_envelope_listeners(&mut self, handler: &Expr, direction: SocketDirection) {
        if !matches!(unwrap_expr(handler), Expr::Arrow(_) | Expr::Fn(_)) {
            return;
        }
        let mut bindings = EnvelopeBindings::default();
        handler.visit_with(&mut bindings);
        if bindings.names.is_empty() {
            return;
        }
        let mut dispatch = DispatchCollector {
            bindings: &bindings.names,
            tables: self.tables,
            cm: self.cm.clone(),
            found: Vec::new(),
            seen: HashSet::new(),
        };
        handler.visit_with(&mut dispatch);
        for entry in dispatch.found {
            let (payload_type_symbol, payload_type_source) = match entry.payload_symbol {
                Some(symbol) => {
                    let source = self.roots.type_imports.get(&symbol).cloned();
                    (Some(symbol), source)
                }
                None => (None, None),
            };
            self.extraction.listeners.push(SocketOp {
                key: OperationKey::socket(entry.event, direction),
                file_path: self.file_path.to_path_buf(),
                line: entry.line,
                payload_type_symbol,
                payload_type_source,
            });
        }
    }

    /// The handler a registration call passes, when it passes one. A function
    /// expression or a reference to one; anything else (an options bag, a
    /// missing argument) is not a registration, which is how
    /// `client.subscribe("orders")` — a call that RETURNS a channel — is told
    /// from `channel.subscribe("orders", handler)`.
    fn handler_argument(args: &[ExprOrSpread]) -> Option<&Expr> {
        let candidate = args.get(1)?;
        match &*candidate.expr {
            expr @ (Expr::Arrow(_) | Expr::Fn(_) | Expr::Ident(_)) => Some(expr),
            _ => None,
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
        first_param_symbol(first_param)
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
        extract_with_clients(source, &[])
    }

    /// Extract with a detected socket-client list, for the unknown-direction
    /// half of the pass (carrick#1281).
    fn extract_with_clients(source: &str, socket_clients: &[&str]) -> SocketExtraction {
        let socket_clients: Vec<String> =
            socket_clients.iter().map(|pkg| pkg.to_string()).collect();
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
        let result = extract_from_ts_file(&file, &mut AliasResolver::default(), &socket_clients);
        std::fs::remove_dir_all(&dir).ok();
        result
    }

    /// Write a small module graph and extract from `main.ts`, so a test can
    /// exercise the one-hop alias import (carrick#670).
    fn extract_graph(files: &[(&str, &str)]) -> SocketExtraction {
        let dir = std::env::temp_dir().join(format!(
            "carrick-socket-graph-{}-{:016x}",
            std::process::id(),
            {
                let mut hash: u64 = 0xcbf29ce484222325;
                for (name, source) in files {
                    for byte in name.as_bytes().iter().chain(source.as_bytes()) {
                        hash ^= u64::from(*byte);
                        hash = hash.wrapping_mul(0x100000001b3);
                    }
                }
                hash
            }
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, source) in files {
            std::fs::write(dir.join(name), source).unwrap();
        }
        let result = extract_from_ts_file(&dir.join("main.ts"), &mut AliasResolver::default(), &[]);
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
    fn same_file_socket_type_alias_is_a_root_type() {
        // carrick#670: a file that holds several sockets names the type once
        // and then declares every field with the alias. The alias is the
        // admitted type, so fields declared with it are roots.
        let result = extract(
            r#"
import type { Socket } from "socket.io-client";

export type SupervisorSocket = Socket<ServerToClient, ClientToServer>;

class Controller {
  private socket: SupervisorSocket;

  constructor(socket: SupervisorSocket) {
    this.socket = socket;
  }

  start() {
    this.socket.emit("run:start", { version: "1" });
    this.socket.on("run:notify", (msg) => this.handle(msg));
  }

  stop() {
    this.socket.emit("run:stop", { version: "1" });
  }
}
"#,
        );
        assert_eq!(
            keys(&result.emitters),
            vec![
                "socket|CLIENT->SERVER|run:start",
                "socket|CLIENT->SERVER|run:stop",
            ]
        );
        assert_eq!(
            keys(&result.listeners),
            vec!["socket|SERVER->CLIENT|run:notify"]
        );
    }

    #[test]
    fn a_chain_of_socket_type_aliases_resolves() {
        // The alias is resolved by the same fixpoint the other roots use, so
        // declaration order and an alias of an alias both work.
        let result = extract(
            r#"
import type { Socket } from "socket.io";

class Connection {
  private socket: WorkerSocket;

  register() {
    this.socket.on("worker:ready", (msg) => this.ack(msg));
  }
}

type WorkerSocket = ConnectedSocket;
type ConnectedSocket = Socket<ClientToServer, ServerToClient>;
"#,
        );
        assert_eq!(
            keys(&result.listeners),
            vec!["socket|CLIENT->SERVER|worker:ready"],
            "an alias declared after use, and an alias of an alias, both resolve"
        );
    }

    #[test]
    fn an_alias_of_an_unrelated_type_is_not_a_root_type() {
        // The alias only counts because its right-hand side is the socket
        // type; a same-named alias of anything else stays inert.
        let result = extract(
            r#"
import type { Socket } from "socket.io-client";
import { io } from "socket.io-client";
const probe = io(url);

type SupervisorSocket = EventEmitter;

class Controller {
  private socket: SupervisorSocket;

  start() {
    this.socket.emit("run:start", {});
  }
}
"#,
        );
        assert!(
            result.is_empty(),
            "an alias of a foreign type is not a socket, got {:?}",
            keys(&result.emitters)
        );
    }

    #[test]
    fn an_imported_socket_type_alias_is_followed_one_hop() {
        // carrick#670: the module that owns the socket declares the alias and
        // its siblings import it, so the importing file never names the socket
        // type at all.
        let result = extract_graph(&[
            (
                "controller.ts",
                r#"
import type { Socket } from "socket.io-client";
export type SupervisorSocket = Socket<ServerToClient, ClientToServer>;
"#,
            ),
            (
                "main.ts",
                r#"
import type { SupervisorSocket } from "./controller.js";

export class RunNotifier {
  private socket: SupervisorSocket;

  constructor(opts: { supervisorSocket: SupervisorSocket }) {
    this.socket = opts.supervisorSocket;
  }

  start() {
    this.socket.on("run:notify", async ({ run }) => this.handle(run));
  }
}
"#,
            ),
        ]);
        assert_eq!(
            keys(&result.listeners),
            vec!["socket|SERVER->CLIENT|run:notify"]
        );
    }

    #[test]
    fn an_imported_alias_is_followed_through_a_re_export() {
        // The importing file names a barrel, and the barrel republishes the
        // type from the module that declares it.
        let result = extract_graph(&[
            (
                "socketTypes.ts",
                r#"
import type { Socket } from "socket.io";
export type WorkloadSocket = Socket<ClientToServer, ServerToClient>;
"#,
            ),
            (
                "index.ts",
                r#"
export type { WorkloadSocket } from "./socketTypes.js";
"#,
            ),
            (
                "main.ts",
                r#"
import type { WorkloadSocket } from "./index.js";

class Connection {
  constructor(private readonly socket: WorkloadSocket) {}

  register() {
    this.socket.on("run:start", ({ runId }) => this.start(runId));
    this.socket.emit("run:notify", { version: "1" });
  }
}
"#,
            ),
        ]);
        assert_eq!(
            keys(&result.listeners),
            vec!["socket|CLIENT->SERVER|run:start"]
        );
        assert_eq!(
            keys(&result.emitters),
            vec!["socket|SERVER->CLIENT|run:notify"]
        );
    }

    #[test]
    fn an_imported_alias_of_an_unrelated_type_is_not_followed() {
        // The hop reads the declaring module with the same rules; an alias of
        // something that is not a socket resolves to nothing.
        let result = extract_graph(&[
            (
                "controller.ts",
                r#"
import type { EventEmitter } from "node:events";
export type SupervisorSocket = EventEmitter;
"#,
            ),
            (
                "main.ts",
                r#"
import type { SupervisorSocket } from "./controller.js";

class RunNotifier {
  private socket: SupervisorSocket;

  start() {
    this.socket.on("run:notify", (msg) => this.handle(msg));
  }
}
"#,
            ),
        ]);
        assert!(
            result.is_empty(),
            "an imported alias of a foreign type is not a socket, got {:?}",
            keys(&result.listeners)
        );
    }

    #[test]
    fn a_package_specifier_alias_is_not_followed() {
        // Only relative specifiers are followed; a package import would need
        // the sidecar's module resolution.
        let result = extract_graph(&[(
            "main.ts",
            r#"
import type { SupervisorSocket } from "@acme/contracts";

class RunNotifier {
  private socket: SupervisorSocket;

  start() {
    this.socket.on("run:notify", (msg) => this.handle(msg));
  }
}
"#,
        )]);
        assert!(result.is_empty());
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

    #[test]
    fn an_unknown_root_declines_the_transport_vocabulary() {
        let result = extract_with_clients(
            r#"
import { Connection } from "wire-transport";
const link = new Connection(url);
link.on("message", handleFrame);
link.on("close", teardown);
link.on("order.settled", handleSettled);
"#,
            &["wire-transport"],
        );
        assert_eq!(
            keys(&result.listeners),
            vec!["socket|UNKNOWN|order.settled"],
            "the runtime's own events are the transport's, not the codebase's"
        );
    }

    #[test]
    fn a_registration_needs_a_handler_and_a_send_needs_a_name() {
        let result = extract_with_clients(
            r#"
import { Hub } from "wire-transport";
const hub = new Hub(url);
const room = hub.join("lobby");
room.subscribe("seat.taken", onSeatTaken);
room.subscribe("seat.freed");
room.publish("seat.released", seat);
room.publish(topicFromConfig, seat);
"#,
            &["wire-transport"],
        );
        assert_eq!(keys(&result.listeners), vec!["socket|UNKNOWN|seat.taken"]);
        assert_eq!(keys(&result.emitters), vec!["socket|UNKNOWN|seat.released"]);
    }

    #[test]
    fn only_send_reads_the_event_out_of_an_envelope() {
        let result = extract_with_clients(
            r#"
import { Connection } from "wire-transport";
const link = new Connection(url);
link.send(JSON.stringify({ type: "order.packed", parcel }));
link.trigger({ event: "order.labelled", parcel });
"#,
            &["wire-transport"],
        );
        assert_eq!(
            keys(&result.emitters),
            vec!["socket|UNKNOWN|order.packed"],
            "a sending method that takes the event as an argument is not read as an envelope"
        );
    }

    #[test]
    fn a_delivery_handler_discriminating_on_the_envelope_yields_listeners() {
        let result = extract_with_clients(
            r#"
import { Connection } from "wire-transport";
const link = new Connection(url);
link.on("message", (raw) => {
  const msg = JSON.parse(raw);
  switch (msg.type) {
    case "order.packed":
      pack(msg);
      break;
    case "order.labelled":
      label(msg);
      break;
  }
  if (msg.type === "order.shipped") {
    ship(msg);
  }
  if ("order.returned" === msg.type) {
    unpack(msg);
  }
});
"#,
            &["wire-transport"],
        );
        assert_eq!(
            keys(&result.listeners),
            vec![
                "socket|UNKNOWN|order.labelled",
                "socket|UNKNOWN|order.packed",
                "socket|UNKNOWN|order.returned",
                "socket|UNKNOWN|order.shipped",
            ],
            "a case and a comparison are two spellings of one discriminator read, \
             either way round"
        );
        assert!(
            keys(&result.emitters).is_empty(),
            "the registration itself is still transport lifecycle"
        );
    }

    #[test]
    fn a_dispatch_table_names_the_events_and_types_their_payloads() {
        let result = extract_with_clients(
            r#"
import { Connection } from "wire-transport";
import type { Parcel } from "./types/parcel";
const link = new Connection(url);
const handlers = {
  "order.packed": (parcel: Parcel) => pack(parcel),
  "order.labelled": function (parcel: Parcel) { label(parcel); },
  "order.returned"(parcel: Parcel) { unpack(parcel); },
  "order.shipped": onShipped,
  orderAudited,
  retries: 3,
};
link.on("message", (raw) => {
  const envelope = JSON.parse(raw);
  handlers[envelope?.event]?.(envelope.payload);
});
"#,
            &["wire-transport"],
        );
        assert_eq!(
            keys(&result.listeners),
            vec![
                "socket|UNKNOWN|order.labelled",
                "socket|UNKNOWN|order.packed",
                "socket|UNKNOWN|order.returned",
                "socket|UNKNOWN|order.shipped",
                "socket|UNKNOWN|orderAudited",
            ],
            "every callable key of the table the handler dispatches on is an event, \
             written as an arrow, a function, a method or a shorthand binding, and \
             read through an optional discriminator; a number is not a handler"
        );
        let packed = find(&result.listeners, "socket|UNKNOWN|order.packed");
        assert_eq!(packed.payload_type_symbol.as_deref(), Some("Parcel"));
        assert_eq!(
            packed.payload_type_source.as_deref(),
            Some("./types/parcel"),
            "the handler's parameter anchors the payload through the same import map \
             a Socket.IO listener uses"
        );
        let shipped = find(&result.listeners, "socket|UNKNOWN|order.shipped");
        assert_eq!(
            shipped.payload_type_symbol, None,
            "a handler referenced by name keeps its parameter type a hop away"
        );
    }

    #[test]
    fn the_discriminator_must_come_from_the_parsed_payload() {
        let result = extract_with_clients(
            r#"
import { Connection } from "wire-transport";
const link = new Connection(url);
const route = { "order.packed": pack };
link.on("message", (frame) => {
  if (frame.type === "order.packed") {
    pack(frame);
  }
  if (settings.type === "order.labelled") {
    label(frame);
  }
  route[frame.type]?.(frame);
});
function replay(raw) {
  const msg = JSON.parse(raw);
  switch (msg.type) {
    case "order.replayed":
      replayOrder(msg);
  }
}
"#,
            &["wire-transport"],
        );
        assert!(
            result.listeners.is_empty(),
            "an undecoded handler argument is not an envelope, and the same switch \
             outside a delivery handler has no transport to belong to, got {:?}",
            keys(&result.listeners)
        );
    }

    #[test]
    fn the_receive_side_declines_what_it_cannot_read() {
        let result = extract_with_clients(
            r#"
import { Connection } from "wire-transport";
const PACKED = "order.packed";
const link = new Connection(url);
link.on("message", onFrame);
link.on("message", (raw) => {
  const msg = JSON.parse(raw);
  switch (msg.type) {
    case PACKED:
      pack(msg);
      break;
    case "order.labelled":
      label(msg);
      break;
  }
  if (msg.type !== "order.shipped") {
    return;
  }
  if (msg.status === "pending") {
    hold(msg);
  }
  if (msg.type === "message") {
    ignore(msg);
  }
});
"#,
            &["wire-transport"],
        );
        assert_eq!(
            keys(&result.listeners),
            vec!["socket|UNKNOWN|order.labelled"],
            "a handler passed by name is not followed, a case on a binding has no \
             literal, `!==` names what the branch does not handle, `status` is not \
             the discriminator key, and the transport's own name is never a contract"
        );
    }

    #[test]
    fn one_event_named_twice_in_a_handler_is_one_row() {
        let result = extract_with_clients(
            r#"
import { Connection } from "wire-transport";
const link = new Connection(url);
link.on("message", (raw) => {
  const msg = JSON.parse(raw);
  if (msg.type === "order.packed") {
    pack(msg);
  }
  switch (msg.type) {
    case "order.packed":
      packAgain(msg);
  }
});
"#,
            &["wire-transport"],
        );
        assert_eq!(
            keys(&result.listeners),
            vec!["socket|UNKNOWN|order.packed"],
            "a handler dispatches on an event once however many spellings name it"
        );
    }

    #[test]
    fn a_socket_io_root_does_not_read_its_message_handler() {
        let result = extract(
            r#"
import { io } from "socket.io-client";
const socket = io("https://example.test");
socket.on("message", (raw) => {
  const msg = JSON.parse(raw);
  switch (msg.type) {
    case "order.packed":
      pack(msg);
  }
});
"#,
        );
        assert_eq!(
            keys(&result.listeners),
            vec!["socket|SERVER->CLIENT|message"],
            "a library whose sides are known names its event in the call — that name, \
             whatever it is — and the envelope inside the handler is not read: doing \
             both would key one site twice"
        );
    }

    #[test]
    fn a_non_socket_object_in_a_gated_file_is_not_a_socket_root() {
        // The gate is the package a binding CAME FROM, not the file's imports:
        // an in-process bus beside a socket keeps its own channel, so its rows
        // still match a subscriber in a file that imports nothing.
        let result = extract_with_clients(
            r#"
import { Connection } from "wire-transport";
import { EventEmitter } from "node:events";
const link = new Connection(url);
const bus = new EventEmitter();
bus.emit("order.audited", entry);
link.emit("order.shipped", parcel);
"#,
            &["wire-transport"],
        );
        assert_eq!(keys(&result.emitters), vec!["socket|UNKNOWN|order.shipped"]);
    }
}
