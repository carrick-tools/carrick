//! Deterministic in-process event-bus contract extraction.
//!
//! A service that publishes work through an in-process emitter
//! (`bus.emit("orderPlaced", …)`) and subscribes to it somewhere else
//! (`bus.on("orderPlaced", handler)`) has a contract with two sides and a name,
//! exactly like a broker topic — the only difference is that the message never
//! leaves the process. The index holds it on the same channel a broker topic
//! uses, `OperationKey::pubsub(<event>)`: a subscription registers the handler
//! and is the contract PRODUCER, an emission sends and is the CONSUMER.
//!
//! This is the one shape the rest of the pipeline could not see. The
//! file-analyzer reports pub/sub operations for brokers, and the socket pass
//! (`crate::socket_io`) covers Socket.IO transport, but an emitter held on a
//! plain object field belongs to neither, so a question like "what subscribes
//! to this notification" had no row to answer with (carrick#676).
//!
//! The pass is structural throughout: it matches the *shape* of the call
//! (`<receiver>.on|once|addListener("literal", …)` and
//! `<anything>.emit("literal", …)`), never a library or class name, so any
//! object exposing the EventEmitter protocol resolves and no package needs to
//! be recognised. Three rules, and the receiver rule below, keep that from
//! becoming noise:
//!
//! - **Literal event names only.** A computed name has no identity to key on.
//! - **The runtime's own vocabulary is excluded** (see [`RUNTIME_EVENTS`]).
//!   These are events user code subscribes to but never emits — the runtime
//!   emits them — so they are not contracts between two pieces of this
//!   codebase, and indexing them would put a producer row on a key as generic
//!   as `error` or `data`, which any other repo's row would then match.
//! - **A site the socket pass already recorded is left alone.** Socket ops are
//!   the modeled transport contract and live on their own key; the same span
//!   must not be indexed twice on two channels.
//!
//! A fourth rule lives at the append (`append_event_bus_operations` in
//! `engine/mod.rs`) because it needs the LLM results this pass never sees: a
//! row is dropped where the file-analyzer already reported a pub/sub op for the
//! same file, topic and role. One call site is one row, and of the two the
//! model's is the richer — it carries the payload anchor that resolves the op's
//! type, which this pass does not yet extract (#688). What this pass adds is
//! the sites nothing else reports, which is the whole of the gap it was written
//! for.
//!
//! **A listener is a contract endpoint only when the object it listens on is
//! one the service receives messages from** (carrick#941, carrick#2133). A
//! stream, a line reader, a child process, a browser page or a desktop app
//! emits its own vocabulary (`line`, `pageerror`, `activate`), and a listener
//! on one reads that library's events, not a contract of this codebase. So
//! the receiver of every subscription is classified from what the file
//! states ([`ReceiverReader`]), and the row is kept when the receiver roots
//! in:
//!
//! - `this`, inside a class the file declares;
//! - an import the service's module index places inside the repo (relative,
//!   a tsconfig alias, a workspace package), through member reads, calls,
//!   `await` or `new`;
//! - a class or function the file declares;
//! - `new X()` with no arguments, `X` imported from a package or a runtime
//!   module: the repo owns that instance, as it does an emitter it builds
//!   for itself. An instance built with arguments was told where to connect
//!   or what to watch, so it is the library's;
//! - an import of a package framework detection classed as a messaging or
//!   socket client (a transport);
//! - `this.<field>`, by the field's declared type, else its initialiser, read
//!   by the rows above; a parameter, by its declared type.
//!
//! Anything else gets no row: a package or runtime module reached through a
//! call, a member or a construction with arguments, a global (`process`), an
//! unannotated parameter, a field with no declared type or initialiser, a
//! method result read off `this`. Where the file does not let the receiver be
//! placed, the pass abstains. Emissions are not classified (carrick#1523).
//!
//! What that still accepts, stated plainly. A delivery event on an in-repo
//! object that wraps a library client (a re-exported redis client's
//! `message`) reads as the repo's own, because the receiver is (carrick#1671).
//! And identity is the event name alone, as it is for every pub/sub row, so
//! two services naming an event the same thing share a key whether or not
//! they share a bus; that exposure is the one broker topics already carry.

use crate::binding_scope::{BindingKey, ident_key};
use crate::graphql_document_sites::unwrap_expression as unwrap;
use crate::operation::OperationKey;
use crate::parser::parse_file;
use crate::receiver_type::{annotated_type_ident, class_field_types};
use crate::socket_io::{RESERVED_EVENTS, SocketExtraction};
use crate::workspace_resolver::{Resolution, WorkspaceIndex};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use swc_common::errors::{ColorConfig, Handler};
use swc_common::{GLOBALS, Globals, SourceMap, sync::Lrc};
use swc_ecma_ast::{
    ArrowExpr, Callee, Class, ClassDecl, ClassExpr, ClassMember, Decl, Expr, ExprOrSpread, FnDecl,
    FnExpr, GetterProp, ImportDecl, ImportSpecifier, Lit, MemberExpr, MemberProp, MethodProp,
    Module, ModuleDecl, ModuleItem, OptChainBase, OptChainExpr, Param, ParamOrTsParamProp, Pat,
    PropName, SetterProp, Stmt, TsParamPropParam, TsType, VarDeclarator,
};
use swc_ecma_visit::{Visit, VisitWith};
use tracing::debug;

/// One side of an in-process event contract, with its source location.
#[derive(Debug, Clone)]
pub struct BusOp {
    /// Always `OperationKey::pubsub(event)` — an in-process bus is pub/sub with
    /// a shorter wire.
    pub key: OperationKey,
    /// The literal event name, kept alongside the key so the twin fold can read
    /// it without re-parsing the key.
    pub event: String,
    pub file_path: PathBuf,
    pub line: u32,
}

#[derive(Debug, Clone, Default)]
pub struct BusExtraction {
    /// `.on` / `.once` / `.addListener` — the handler side, contract producers.
    pub subscribers: Vec<BusOp>,
    /// `.emit` — the sending side, contract consumers.
    pub publishers: Vec<BusOp>,
}

impl BusExtraction {
    pub fn is_empty(&self) -> bool {
        self.subscribers.is_empty() && self.publishers.is_empty()
    }

    fn merge(&mut self, other: BusExtraction) {
        self.subscribers.extend(other.subscribers);
        self.publishers.extend(other.publishers);
    }
}

/// Methods that register a handler. `prependListener` and its `once` variant
/// are the same registration with a different queue position, so they count.
const SUBSCRIBE_METHODS: &[&str] = &[
    "on",
    "once",
    "addListener",
    "prependListener",
    "prependOnceListener",
];

/// The method that sends.
const PUBLISH_METHOD: &str = "emit";

/// The runtime's own reserved vocabulary: events a Node process, stream,
/// socket, or child process emits at code that subscribes to them. User code is
/// on one side of these only, so they are lifecycle, not a contract between two
/// parts of a codebase — and their names are generic enough (`error`, `data`,
/// `close`) that a row on one would match any unrelated row sharing the name.
///
/// This is an exclusion vocabulary, not a library list: nothing here names a
/// package, and a package's own event names are not enumerated anywhere. The
/// Socket.IO lifecycle names are unioned in from
/// [`crate::socket_io::RESERVED_EVENTS`], because the socket pass DECLINES
/// those sites (they are reserved there too) and so leaves no claim on the span
/// for this pass to see.
///
/// The socket pass reads this list back for its unknown-direction roots
/// (`crate::socket_io::is_transport_event`): a raw socket's `message`/`close`
/// is the same runtime vocabulary reaching user code down a different pipe.
pub(crate) const RUNTIME_EVENTS: &[&str] = &[
    // Process lifecycle and signals.
    "SIGBREAK",
    "SIGHUP",
    "SIGINT",
    "SIGQUIT",
    "SIGTERM",
    "SIGUSR1",
    "SIGUSR2",
    "SIGWINCH",
    "beforeExit",
    "exit",
    "rejectionHandled",
    "uncaughtException",
    "unhandledRejection",
    "warning",
    // Streams, sockets, servers.
    "aborted",
    "clientError",
    "continue",
    "data",
    "drain",
    "end",
    "finish",
    "listening",
    "lookup",
    "open",
    "pause",
    "pipe",
    "readable",
    "ready",
    "request",
    "response",
    "resume",
    "secureConnection",
    "timeout",
    "unpipe",
    "upgrade",
    // Child processes and workers.
    "close",
    "message",
    "messageerror",
    "online",
    "spawn",
];

/// Whether an event name is the runtime's rather than the codebase's.
fn is_reserved(event: &str) -> bool {
    RUNTIME_EVENTS.contains(&event) || RESERVED_EVENTS.contains(&event)
}

/// Extract in-process event-bus operations from the service's TS/JS files.
///
/// `sockets` is the deterministic Socket.IO extraction for the SAME file set,
/// already run: every span it recorded is claimed and skipped here, so one call
/// site never produces both a `socket|…` and a `pubsub|…` row.
///
/// `modules` is the SERVICE's module index (its own tsconfig `paths`
/// included), which places a receiver's import inside or outside the repo;
/// `transports` is every package framework detection classed as a messaging or
/// socket client. Both serve the receiver rule (module doc).
pub fn scan_files(
    service_files: &[PathBuf],
    sockets: &SocketExtraction,
    modules: &WorkspaceIndex,
    transports: &[String],
) -> BusExtraction {
    let claimed = claimed_spans(sockets);
    let mut extraction = BusExtraction::default();
    for file in service_files {
        let is_script = file
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(crate::file_finder::is_source_extension);
        if !is_script {
            continue;
        }
        extraction.merge(extract_from_ts_file(file, &claimed, modules, transports));
    }
    debug!(
        subscribers = extraction.subscribers.len(),
        publishers = extraction.publishers.len(),
        "In-process event bus extraction complete"
    );
    extraction
}

/// Spans the socket pass already recorded, as (file, line, event). Keyed on the
/// event as well as the span so two calls sharing a line — `bus.on("a", () =>
/// socket.emit("b", x))` — are told apart.
///
/// Both passes place an op on the line of its method name, not the line its
/// receiver chain starts on (carrick#1626), so a chained socket call written
/// across lines is still the span this pass skips. A change to where either
/// pass puts its line is a change to this join.
fn claimed_spans(sockets: &SocketExtraction) -> HashSet<(PathBuf, u32, String)> {
    sockets
        .listeners
        .iter()
        .chain(sockets.emitters.iter())
        .filter_map(|op| {
            op.key
                .socket_event()
                .map(|event| (op.file_path.clone(), op.line, event.to_string()))
        })
        .collect()
}

fn extract_from_ts_file(
    file_path: &Path,
    claimed: &HashSet<(PathBuf, u32, String)>,
    modules: &WorkspaceIndex,
    transports: &[String],
) -> BusExtraction {
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));

    let globals = Globals::new();
    GLOBALS.set(&globals, || {
        let Some(module) = parse_file(file_path, &cm, &handler) else {
            return BusExtraction::default();
        };
        let mut collector = BusCollector {
            cm: cm.clone(),
            file_path,
            claimed,
            classes: Vec::new(),
            pending: Vec::new(),
            extraction: BusExtraction::default(),
        };
        module.visit_with(&mut collector);
        let BusCollector {
            pending,
            mut extraction,
            ..
        } = collector;
        if pending.is_empty() {
            return extraction;
        }
        // The file's bindings are read only once a subscription needs them.
        let reader = ReceiverReader::new(&module, file_path, modules, transports);
        for subscription in pending {
            if reader.keeps(&subscription.receiver, subscription.class.as_deref()) {
                extraction.subscribers.push(subscription.op);
            } else {
                debug!(
                    event = %subscription.op.event,
                    file = %file_path.display(),
                    line = subscription.op.line,
                    "listener on an object this repo does not own; not a contract (carrick#941)"
                );
            }
        }
        extraction
    })
}

/// A subscription whose receiver is still to be classified.
struct PendingSubscription {
    op: BusOp,
    /// The object `.on(…)` is called on.
    receiver: Box<Expr>,
    /// The fields of the class the call sits in, when `this` there is an
    /// instance of a class the file declares.
    class: Option<Rc<ClassFields>>,
}

struct BusCollector<'a> {
    cm: Lrc<SourceMap>,
    file_path: &'a Path,
    claimed: &'a HashSet<(PathBuf, u32, String)>,
    /// What `this` is at the current point of the walk: a class's fields, or
    /// `None` inside a function that rebinds `this` to something else.
    classes: Vec<Option<Rc<ClassFields>>>,
    pending: Vec<PendingSubscription>,
    extraction: BusExtraction,
}

impl BusCollector<'_> {
    /// Walk `node` with `this` bound to `class` (or to nothing the file
    /// declares).
    fn with_this<N: VisitWith<Self> + ?Sized>(&mut self, class: Option<Rc<ClassFields>>, node: &N) {
        self.classes.push(class);
        node.visit_children_with(self);
        self.classes.pop();
    }
}

impl Visit for BusCollector<'_> {
    fn visit_class(&mut self, class: &Class) {
        self.with_this(Some(Rc::new(ClassFields::of(class))), class);
    }

    // A `function` and an object-literal method bind `this` to their caller,
    // never to an enclosing class. An arrow inherits it, so it has no arm.
    fn visit_fn_decl(&mut self, node: &FnDecl) {
        self.with_this(None, node);
    }

    fn visit_fn_expr(&mut self, node: &FnExpr) {
        self.with_this(None, node);
    }

    fn visit_method_prop(&mut self, node: &MethodProp) {
        self.with_this(None, node);
    }

    fn visit_getter_prop(&mut self, node: &GetterProp) {
        self.with_this(None, node);
    }

    fn visit_setter_prop(&mut self, node: &SetterProp) {
        self.with_this(None, node);
    }

    fn visit_call_expr(&mut self, node: &swc_ecma_ast::CallExpr) {
        if let Callee::Expr(callee) = &node.callee
            && let Expr::Member(member) = &**callee
        {
            self.record_member_call(member, &node.args);
        }
        node.visit_children_with(self);
    }

    fn visit_opt_chain_expr(&mut self, node: &OptChainExpr) {
        // `bus?.on("x", …)` is an optional call, not a `CallExpr`, so it needs
        // its own arm or the op is silently lost.
        if let OptChainBase::Call(call) = &*node.base {
            match &*call.callee {
                Expr::Member(member) => self.record_member_call(member, &call.args),
                Expr::OptChain(inner) => {
                    if let OptChainBase::Member(member) = &*inner.base {
                        self.record_member_call(member, &call.args);
                    }
                }
                _ => {}
            }
        }
        node.visit_children_with(self);
    }
}

impl BusCollector<'_> {
    /// Record the bus op a `<receiver>.<method>("event", …)` call site carries,
    /// if any. An emission is recorded as it stands; a subscription waits for
    /// its receiver to be classified (module doc).
    fn record_member_call(&mut self, member: &MemberExpr, args: &[ExprOrSpread]) {
        let Some(prop) = member.prop.as_ident() else {
            return;
        };
        let method = prop.sym.as_ref();
        let is_subscribe = SUBSCRIBE_METHODS.contains(&method);
        let is_publish = method == PUBLISH_METHOD;
        if !is_subscribe && !is_publish {
            return;
        }
        // A registration takes a handler; an emission usually carries a
        // payload. Neither is required to have one, but a literal event name
        // is: without it there is no key.
        let Some(first) = args.first() else {
            return;
        };
        let Expr::Lit(Lit::Str(event)) = &*first.expr else {
            return;
        };
        let event = event.value.to_string_lossy().into_owned();
        if is_reserved(&event) {
            return;
        }
        // The method name's line, as the socket pass places its ops
        // (`claimed_spans`), not the line the receiver chain starts on.
        let line = self.cm.lookup_char_pos(prop.span.lo).line as u32;
        if self
            .claimed
            .contains(&(self.file_path.to_path_buf(), line, event.clone()))
        {
            debug!(
                event = %event,
                file = %self.file_path.display(),
                line,
                "event-bus site already recorded as a socket op; leaving it there"
            );
            return;
        }
        let op = BusOp {
            key: OperationKey::pubsub(event.clone()),
            event,
            file_path: self.file_path.to_path_buf(),
            line,
        };
        if is_subscribe {
            self.pending.push(PendingSubscription {
                op,
                receiver: member.obj.clone(),
                class: self.classes.last().cloned().flatten(),
            });
        } else {
            self.extraction.publishers.push(op);
        }
    }
}

/// What one class declares about its instance fields: the type written for
/// each ([`class_field_types`]) and the value it is initialised with.
#[derive(Default)]
struct ClassFields {
    types: crate::receiver_type::ReceiverTypes,
    /// Field -> its declared initialiser. `None` marks a field declared twice.
    initialisers: HashMap<String, Option<Box<Expr>>>,
}

impl ClassFields {
    fn of(class: &Class) -> Self {
        let mut initialisers: HashMap<String, Option<Box<Expr>>> = HashMap::new();
        let mut record = |name: String, value: Option<&Expr>| {
            let Some(value) = value else {
                return;
            };
            initialisers
                .entry(name)
                .and_modify(|existing| *existing = None)
                .or_insert_with(|| Some(Box::new(value.clone())));
        };
        for member in &class.body {
            match member {
                ClassMember::ClassProp(prop) if !prop.is_static => {
                    if let PropName::Ident(key) = &prop.key {
                        record(key.sym.to_string(), prop.value.as_deref());
                    }
                }
                ClassMember::PrivateProp(prop) if !prop.is_static => {
                    record(format!("#{}", prop.key.name), prop.value.as_deref());
                }
                // `constructor(private bus = new EventEmitter())`.
                ClassMember::Constructor(constructor) => {
                    for param in &constructor.params {
                        if let ParamOrTsParamProp::TsParamProp(prop) = param
                            && let TsParamPropParam::Assign(assign) = &prop.param
                            && let Pat::Ident(ident) = &*assign.left
                        {
                            record(ident.id.sym.to_string(), Some(&*assign.right));
                        }
                    }
                }
                _ => {}
            }
        }
        Self {
            types: class_field_types(class),
            initialisers,
        }
    }
}

/// Where a receiver's value comes from, as the file states it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// This repo's own code: an in-repo import, a class or function the file
    /// declares, `this` in a declared class.
    Repo,
    /// A package detection classed as a messaging or socket client.
    Transport,
    /// `new X()` with no arguments, `X` from a package or a runtime module:
    /// an instance the repo built for itself and told nothing about.
    OwnedInstance,
    /// A package's or the runtime's object, a global, or anything the file
    /// does not let the pass place.
    Outside,
}

impl Origin {
    /// Whether a listener on a value of this origin is a contract endpoint.
    fn keeps(self) -> bool {
        self != Origin::Outside
    }

    /// The origin of a value read off this one (a member or a call result).
    /// The repo's code and a transport stay what they are; anything a
    /// package's object hands out is that package's.
    fn carried(self) -> Origin {
        match self {
            Origin::Repo | Origin::Transport => self,
            Origin::OwnedInstance | Origin::Outside => Origin::Outside,
        }
    }
}

/// What one binding is, keyed by its resolver identity.
enum Binding {
    /// An imported binding (or a `require`), with its specifier.
    Import(String),
    /// A function or class the file declares.
    Declared,
    /// A `const`/`let`/`var`: its annotation's type name and its initialiser.
    Local {
        annotation: Option<String>,
        init: Option<Box<Expr>>,
    },
    /// A parameter, with its annotation's type name.
    Param { annotation: Option<String> },
    /// Declared more than once: nothing is stated about it.
    Contested,
}

/// How deep a receiver is followed through local bindings and fields before
/// the pass abstains: enough for any hand-written chain, and a stop for a
/// binding that reads itself.
const MAX_RECEIVER_HOPS: usize = 8;

/// Classifies the receiver of a subscription in one file (module doc).
struct ReceiverReader<'a> {
    file: &'a Path,
    modules: &'a WorkspaceIndex,
    transports: &'a [String],
    bindings: HashMap<BindingKey, Binding>,
    /// Names a type annotation can refer to: imports and the classes,
    /// interfaces, type aliases and enums the file declares.
    type_names: HashMap<String, Binding>,
}

impl<'a> ReceiverReader<'a> {
    fn new(
        module: &Module,
        file: &'a Path,
        modules: &'a WorkspaceIndex,
        transports: &'a [String],
    ) -> Self {
        let mut collector = BindingCollector::default();
        module.visit_with(&mut collector);
        // Type names are read at module scope only: a type a function body
        // declares is not what a field or parameter elsewhere names.
        for item in &module.body {
            let declared = match item {
                ModuleItem::Stmt(Stmt::Decl(decl)) => Some(decl),
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => Some(&export.decl),
                _ => None,
            };
            let name = match declared {
                Some(Decl::Class(class)) => Some(&class.ident),
                Some(Decl::TsInterface(interface)) => Some(&interface.id),
                Some(Decl::TsTypeAlias(alias)) => Some(&alias.id),
                Some(Decl::TsEnum(enumeration)) => Some(&enumeration.id),
                _ => None,
            };
            if let Some(name) = name {
                record(
                    &mut collector.type_names,
                    name.sym.to_string(),
                    Binding::Declared,
                );
            }
        }
        Self {
            file,
            modules,
            transports,
            bindings: collector.bindings,
            type_names: collector.type_names,
        }
    }

    fn keeps(&self, receiver: &Expr, class: Option<&ClassFields>) -> bool {
        self.value_origin(receiver, class, 0).keeps()
    }

    fn value_origin(&self, expr: &Expr, class: Option<&ClassFields>, depth: usize) -> Origin {
        if depth > MAX_RECEIVER_HOPS {
            return Origin::Outside;
        }
        match expr {
            Expr::Paren(inner) => self.value_origin(&inner.expr, class, depth),
            Expr::Await(inner) => self.value_origin(&inner.arg, class, depth),
            Expr::TsAs(inner) => self.value_origin(&inner.expr, class, depth),
            Expr::TsNonNull(inner) => self.value_origin(&inner.expr, class, depth),
            Expr::TsSatisfies(inner) => self.value_origin(&inner.expr, class, depth),
            Expr::TsConstAssertion(inner) => self.value_origin(&inner.expr, class, depth),
            Expr::TsTypeAssertion(inner) => self.value_origin(&inner.expr, class, depth),
            Expr::This(_) if class.is_some() => Origin::Repo,
            Expr::Ident(ident) => self.binding_origin(ident, class, depth + 1),
            Expr::Member(member) => self.member_origin(member, class, depth),
            Expr::Call(call) => match &call.callee {
                Callee::Expr(callee) => self.call_origin(callee, &call.args, class, depth),
                _ => Origin::Outside,
            },
            Expr::OptChain(chain) => match &*chain.base {
                OptChainBase::Member(member) => self.member_origin(member, class, depth),
                OptChainBase::Call(call) => {
                    self.call_origin(&call.callee, &call.args, class, depth)
                }
            },
            Expr::New(new) => {
                let constructed = self.value_origin(&new.callee, class, depth);
                if constructed.keeps() {
                    return constructed;
                }
                let no_arguments = new.args.as_ref().is_none_or(|args| args.is_empty());
                if no_arguments && self.is_imported_name(&new.callee) {
                    Origin::OwnedInstance
                } else {
                    Origin::Outside
                }
            }
            _ => Origin::Outside,
        }
    }

    /// `this.<field>` reads the field; any other member read carries its
    /// object's origin.
    fn member_origin(
        &self,
        member: &MemberExpr,
        class: Option<&ClassFields>,
        depth: usize,
    ) -> Origin {
        if let (Expr::This(_), Some(fields)) = (unwrap(&member.obj), class) {
            let name = match &member.prop {
                MemberProp::Ident(prop) => prop.sym.to_string(),
                MemberProp::PrivateName(private) => format!("#{}", private.name),
                MemberProp::Computed(_) => return Origin::Outside,
            };
            return self.field_origin(fields, &name, depth + 1);
        }
        self.value_origin(&member.obj, class, depth).carried()
    }

    /// A call's result: `require("x")` is the module `x`; anything else
    /// carries its callee's origin. A method read off `this` is not a field,
    /// so its result is not placed.
    fn call_origin(
        &self,
        callee: &Expr,
        args: &[ExprOrSpread],
        class: Option<&ClassFields>,
        depth: usize,
    ) -> Origin {
        if let Expr::Ident(ident) = callee
            && ident.sym == *"require"
            && !self.bindings.contains_key(&ident_key(ident))
            && let Some(first) = args.first()
            && let Expr::Lit(Lit::Str(specifier)) = &*first.expr
        {
            return self.import_origin(&specifier.value.to_string_lossy());
        }
        // A registration returns the emitter it registered on, so
        // `bus.on("a", f).on("b", g)` listens on `bus` twice.
        if let Expr::Member(member) = unwrap(callee)
            && let MemberProp::Ident(method) = &member.prop
            && SUBSCRIBE_METHODS.contains(&method.sym.as_ref())
        {
            return self.value_origin(&member.obj, class, depth);
        }
        self.value_origin(callee, class, depth).carried()
    }

    fn binding_origin(
        &self,
        ident: &swc_ecma_ast::Ident,
        class: Option<&ClassFields>,
        depth: usize,
    ) -> Origin {
        match self.bindings.get(&ident_key(ident)) {
            Some(Binding::Import(specifier)) => self.import_origin(specifier),
            Some(Binding::Declared) => Origin::Repo,
            Some(Binding::Local { annotation, init }) => {
                let declared = annotation
                    .as_deref()
                    .map_or(Origin::Outside, |name| self.type_origin(name));
                if declared.keeps() {
                    return declared;
                }
                init.as_deref().map_or(Origin::Outside, |init| {
                    self.value_origin(init, class, depth)
                })
            }
            Some(Binding::Param { annotation }) => annotation
                .as_deref()
                .map_or(Origin::Outside, |name| self.type_origin(name)),
            Some(Binding::Contested) | None => Origin::Outside,
        }
    }

    /// A field by its declared type, else by its initialiser.
    fn field_origin(&self, fields: &ClassFields, name: &str, depth: usize) -> Origin {
        let declared = fields
            .types
            .get(name)
            .map_or(Origin::Outside, |type_name| self.type_origin(type_name));
        if declared.keeps() {
            return declared;
        }
        match fields.initialisers.get(name) {
            Some(Some(init)) => self.value_origin(init, Some(fields), depth),
            _ => Origin::Outside,
        }
    }

    /// What a type name written in an annotation refers to.
    fn type_origin(&self, name: &str) -> Origin {
        match self.type_names.get(name) {
            Some(Binding::Import(specifier)) => self.import_origin(specifier),
            Some(Binding::Declared) => Origin::Repo,
            _ => Origin::Outside,
        }
    }

    fn import_origin(&self, specifier: &str) -> Origin {
        if crate::in_process_pubsub::is_transport_import(
            self.modules,
            self.transports,
            self.file,
            specifier,
        ) {
            return Origin::Transport;
        }
        match self.modules.resolve(self.file, specifier) {
            Resolution::Internal(_) => Origin::Repo,
            _ => Origin::Outside,
        }
    }

    /// Whether a constructor names an import, directly or as a member of one
    /// (`EventEmitter`, `events.EventEmitter`).
    fn is_imported_name(&self, callee: &Expr) -> bool {
        match unwrap(callee) {
            Expr::Ident(ident) => matches!(
                self.bindings.get(&ident_key(ident)),
                Some(Binding::Import(_))
            ),
            Expr::Member(member) if member.prop.is_ident() => self.is_imported_name(&member.obj),
            _ => false,
        }
    }
}

/// Insert a binding; a second declaration of the same key contests it.
fn record<K: std::hash::Hash + Eq>(table: &mut HashMap<K, Binding>, key: K, binding: Binding) {
    table
        .entry(key)
        .and_modify(|existing| *existing = Binding::Contested)
        .or_insert(binding);
}

/// The annotation's type name, when it is a plain type reference.
fn annotation_of(pat: &Pat) -> Option<String> {
    match pat {
        Pat::Ident(ident) => ident
            .type_ann
            .as_deref()
            .and_then(annotated_type_ident)
            .map(|name| name.sym.to_string()),
        Pat::Assign(assign) => annotation_of(&assign.left),
        _ => None,
    }
}

/// Every binding a module declares, by resolver identity.
#[derive(Default)]
struct BindingCollector {
    bindings: HashMap<BindingKey, Binding>,
    type_names: HashMap<String, Binding>,
}

impl BindingCollector {
    fn param(&mut self, pat: &Pat) {
        if let Some(key) = crate::binding_scope::pat_key(pat) {
            let annotation = annotation_of(pat);
            record(&mut self.bindings, key, Binding::Param { annotation });
        }
    }
}

impl Visit for BindingCollector {
    fn visit_import_decl(&mut self, import: &ImportDecl) {
        let specifier = import.src.value.to_string_lossy().into_owned();
        for spec in &import.specifiers {
            let local = match spec {
                ImportSpecifier::Named(named) => &named.local,
                ImportSpecifier::Default(default) => &default.local,
                ImportSpecifier::Namespace(namespace) => &namespace.local,
            };
            record(
                &mut self.bindings,
                ident_key(local),
                Binding::Import(specifier.clone()),
            );
            record(
                &mut self.type_names,
                local.sym.to_string(),
                Binding::Import(specifier.clone()),
            );
        }
    }

    fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
        if let Pat::Ident(ident) = &declarator.name {
            let annotation = annotation_of(&declarator.name);
            record(
                &mut self.bindings,
                ident_key(&ident.id),
                Binding::Local {
                    annotation,
                    init: declarator.init.clone(),
                },
            );
        }
        declarator.visit_children_with(self);
    }

    fn visit_param(&mut self, param: &Param) {
        self.param(&param.pat);
        param.visit_children_with(self);
    }

    fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
        for pat in &arrow.params {
            self.param(pat);
        }
        arrow.visit_children_with(self);
    }

    fn visit_ts_param_prop(&mut self, prop: &swc_ecma_ast::TsParamProp) {
        let pat = match &prop.param {
            TsParamPropParam::Ident(ident) => Pat::Ident(ident.clone()),
            TsParamPropParam::Assign(assign) => Pat::Assign(assign.clone()),
        };
        self.param(&pat);
        prop.visit_children_with(self);
    }

    fn visit_fn_decl(&mut self, decl: &FnDecl) {
        record(
            &mut self.bindings,
            ident_key(&decl.ident),
            Binding::Declared,
        );
        decl.visit_children_with(self);
    }

    fn visit_class_decl(&mut self, decl: &ClassDecl) {
        record(
            &mut self.bindings,
            ident_key(&decl.ident),
            Binding::Declared,
        );
        decl.visit_children_with(self);
    }

    fn visit_fn_expr(&mut self, expr: &FnExpr) {
        if let Some(ident) = &expr.ident {
            record(&mut self.bindings, ident_key(ident), Binding::Declared);
        }
        expr.visit_children_with(self);
    }

    fn visit_class_expr(&mut self, expr: &ClassExpr) {
        if let Some(ident) = &expr.ident {
            record(&mut self.bindings, ident_key(ident), Binding::Declared);
        }
        expr.visit_children_with(self);
    }

    // A parameter named in a type (`(bus: Bus) => void`) binds nothing.
    fn visit_ts_type(&mut self, _: &TsType) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// The packages the test repo's manifest declares, so each resolves as
    /// an external dependency rather than as nothing.
    const MANIFEST: &str = r#"{
      "name": "fixture",
      "dependencies": {
        "@acme/broker": "1.0.0",
        "@acme/fs-watch": "1.0.0",
        "@acme/browser": "1.0.0",
        "@acme/desktop": "1.0.0",
        "@acme/db": "1.0.0",
        "socket.io-client": "4.0.0"
      }
    }"#;

    /// The packages detection classes as transports in these tests.
    fn transports() -> Vec<String> {
        vec!["@acme/broker".to_string(), "socket.io-client".to_string()]
    }

    /// A test repo: a manifest, the files named, and the module index over
    /// it. `tsconfig`, when given, is written at the root and governs the
    /// service as its `carrick.json` entry would name it.
    struct Repo {
        dir: TempDir,
        modules: WorkspaceIndex,
    }

    impl Repo {
        fn new(files: &[(&str, &str)], tsconfig: Option<&str>) -> Self {
            let dir = TempDir::new().unwrap();
            fs::write(dir.path().join("package.json"), MANIFEST).unwrap();
            for (path, source) in files {
                let file = dir.path().join(path);
                fs::create_dir_all(file.parent().unwrap()).unwrap();
                fs::write(file, source).unwrap();
            }
            let modules = match tsconfig {
                Some(config) => {
                    fs::write(dir.path().join("tsconfig.json"), config).unwrap();
                    WorkspaceIndex::build_with_aliases(
                        dir.path(),
                        Some((Path::new(""), Path::new("tsconfig.json"))),
                    )
                }
                None => WorkspaceIndex::build_with_aliases(dir.path(), None),
            };
            Self { dir, modules }
        }

        fn path(&self, file: &str) -> PathBuf {
            self.dir.path().join(file)
        }

        /// Scan `file` alone, with `sockets` claimed.
        fn scan_with(&self, file: &str, sockets: &SocketExtraction) -> BusExtraction {
            scan_files(&[self.path(file)], sockets, &self.modules, &transports())
        }

        fn scan(&self, file: &str) -> BusExtraction {
            self.scan_with(file, &SocketExtraction::default())
        }
    }

    /// Scan one file's source, alone in a repo, with no socket ops claimed.
    fn scan(source: &str) -> BusExtraction {
        Repo::new(&[("src/bus.ts", source)], None).scan("src/bus.ts")
    }

    /// The events `source`'s listeners keep, with the support files written
    /// beside it.
    fn kept(source: &str, support: &[(&str, &str)]) -> Vec<String> {
        let mut files = vec![("src/listener.ts", source)];
        files.extend_from_slice(support);
        let found = Repo::new(&files, None).scan("src/listener.ts");
        found.subscribers.into_iter().map(|op| op.event).collect()
    }

    /// An in-repo module holding a bus, for the cases that import one.
    const BUS_MODULE: (&str, &str) = (
        "src/bus.ts",
        r#"import { EventEmitter } from "node:events";
export const bus = new EventEmitter();
"#,
    );

    /// The declared bus the property tests below listen on.
    const DECLARED_BUS: &str = r#"import { EventEmitter } from "node:events";
const bus = new EventEmitter();
"#;

    fn events(ops: &[BusOp]) -> Vec<&str> {
        ops.iter().map(|op| op.event.as_str()).collect()
    }

    /// The registration methods all produce a producer row, on the pub/sub key.
    #[test]
    fn every_registration_form_is_a_subscriber() {
        let found = scan(&format!(
            r#"{DECLARED_BUS}
            bus.on("orderPlaced", handleOrder);
            bus.once("orderShipped", handleShipped);
            bus.addListener("orderRefunded", handleRefund);
            bus.prependListener("orderArchived", handleArchive);
            "#
        ));
        assert_eq!(
            events(&found.subscribers),
            vec![
                "orderPlaced",
                "orderShipped",
                "orderRefunded",
                "orderArchived"
            ]
        );
        assert!(found.publishers.is_empty());
        assert_eq!(
            found.subscribers[0].key.canonical(),
            "pubsub|orderPlaced",
            "an in-process bus row lives on the pub/sub channel"
        );
    }

    /// `.emit` is the consumer side, and carries the line it was written on.
    #[test]
    fn an_emission_is_a_publisher_at_its_own_line() {
        let found = scan(
            r#"
            function ship() {
              bus.emit("orderShipped", { id });
            }
            "#,
        );
        assert!(found.subscribers.is_empty());
        assert_eq!(events(&found.publishers), vec!["orderShipped"]);
        assert_eq!(found.publishers[0].line, 3);
    }

    /// The motivating shape (carrick#676): the bus is a field on some other
    /// object, so the receiver is a chain rather than a bare name. An
    /// emission is recorded whatever its receiver is.
    #[test]
    fn a_bus_reached_through_a_member_chain_resolves() {
        let found = Repo::new(
            &[
                (
                    "src/listener.ts",
                    r#"
            import { engine } from "./engine";
            engine.eventBus.on("workerNotification", onNotification);
            this.deps.bus?.emit("workerNotification", payload);
            "#,
                ),
                ("src/engine.ts", "export const engine = makeEngine();\n"),
            ],
            None,
        )
        .scan("src/listener.ts");
        assert_eq!(events(&found.subscribers), vec!["workerNotification"]);
        assert_eq!(events(&found.publishers), vec!["workerNotification"]);
    }

    /// No literal, no key: a computed event name has no identity to index.
    #[test]
    fn a_computed_event_name_is_not_indexed() {
        let found = scan(&format!(
            r#"{DECLARED_BUS}
            bus.on(eventName, handler);
            bus.emit(`${{prefix}}.created`, payload);
            bus.on();
            "#
        ));
        assert!(found.is_empty(), "{found:?}");
    }

    /// The runtime's own vocabulary is lifecycle, not a contract.
    #[test]
    fn runtime_lifecycle_events_are_not_contracts() {
        // Every receiver is one the repo owns, so the name alone decides.
        let found = scan(&format!(
            r#"{DECLARED_BUS}
            bus.on("SIGTERM", shutdown);
            bus.on("data", chunk => buffer.push(chunk));
            bus.on("error", fail);
            bus.on("close", done);
            bus.on("disconnect", reconnect);
            "#
        ));
        assert!(found.is_empty(), "{found:?}");
    }

    /// A method that is not the emitter protocol is not a bus call, however
    /// literal its first argument.
    #[test]
    fn other_methods_are_not_bus_calls() {
        let found = scan(&format!(
            r#"{DECLARED_BUS}
            bus.join("orders", "orders.id");
            bus.get("/orders", listOrders);
            bus.addEventListener("click", onClick);
            "#
        ));
        assert!(found.is_empty(), "{found:?}");
    }

    /// carrick#1626: a registration or emission written across lines sits on
    /// the line of its method name, not the line its receiver chain starts on.
    #[test]
    fn a_chained_op_sits_on_its_method_line() {
        let found = scan(
            r#"import { EventEmitter } from "node:events";
const queueEvents = new EventEmitter();
queueEvents
  .on("jobDone", onDone)
  .on("jobStalled", onStalled);
bus
  .emit("jobRetried", { id: 1 });
"#,
        );
        let lines = |ops: &[BusOp]| -> Vec<(String, u32)> {
            ops.iter().map(|op| (op.event.clone(), op.line)).collect()
        };
        let mut subscribers = lines(&found.subscribers);
        subscribers.sort();
        assert_eq!(
            subscribers,
            vec![("jobDone".to_string(), 4), ("jobStalled".to_string(), 5)]
        );
        assert_eq!(
            lines(&found.publishers),
            vec![("jobRetried".to_string(), 7)]
        );
    }

    /// The socket pass and this one place a chained call on the same line, so
    /// the span the socket pass claims is still the one this pass skips: a
    /// chained Socket.IO listener is one socket row, never a pub/sub row too.
    #[test]
    fn a_chained_socket_listener_stays_claimed() {
        let source = r#"
import { io } from "socket.io-client";
const client = io("https://example.test");
client
  .on("orderPlaced", handleOrder);
"#;
        let repo = Repo::new(&[("src/client.ts", source)], None);

        let sockets = crate::socket_io::scan_files(&[repo.path("src/client.ts")], &[]);
        let socket_lines: Vec<u32> = sockets.listeners.iter().map(|op| op.line).collect();
        assert_eq!(socket_lines, vec![5], "the socket row is on `.on(`");

        // The receiver is a transport, which the receiver rule keeps: only
        // the claim stops it.
        let found = repo.scan_with("src/client.ts", &sockets);
        assert!(
            found.is_empty(),
            "the socket-claimed span must not also become a pub/sub row: {found:?}"
        );
        assert_eq!(
            events(&repo.scan("src/client.ts").subscribers),
            vec!["orderPlaced"],
            "unclaimed, the same listener is kept"
        );
    }

    /// A span the socket pass recorded stays on the socket channel: one call
    /// site, one row. The socket extraction here is the real one, produced by
    /// running that pass over the same file.
    #[test]
    fn a_socket_claimed_span_is_not_indexed_twice() {
        let source = r#"
            import { io } from "socket.io-client";
            import { EventEmitter } from "node:events";
            const bus = new EventEmitter();
            const client = io("https://example.test");
            client.on("orderPlaced", handleOrder);
            bus.on("orderArchived", handleArchive);
        "#;
        let repo = Repo::new(&[("src/client.ts", source)], None);

        let sockets = crate::socket_io::scan_files(&[repo.path("src/client.ts")], &[]);
        assert_eq!(
            sockets.listeners.len(),
            1,
            "fixture must produce the socket op this test folds against"
        );

        let found = repo.scan_with("src/client.ts", &sockets);
        assert_eq!(
            events(&found.subscribers),
            vec!["orderArchived"],
            "the socket-claimed span must not also become a pub/sub row"
        );
    }

    // ---- The receiver rule (carrick#941, carrick#2133). Each case is kept
    // or dropped for exactly one reason.

    #[test]
    fn an_emitter_built_with_no_arguments_is_the_repos() {
        let source = r#"import { EventEmitter } from "node:events";
const bus = new EventEmitter();
bus.on("cartUpdated", h);
"#;
        assert_eq!(kept(source, &[]), vec!["cartUpdated"]);
    }

    #[test]
    fn a_bus_imported_from_the_repo_is_the_repos() {
        let source = r#"import { bus } from "./bus";
bus.on("orderPlaced", h);
"#;
        assert_eq!(kept(source, &[BUS_MODULE]), vec!["orderPlaced"]);
    }

    #[test]
    fn a_member_chain_on_an_in_repo_import_is_the_repos() {
        let source = r#"import { engine } from "./engine";
engine.events.on("jobFinished", h);
"#;
        let engine = ("src/engine.ts", "export const engine = makeEngine();\n");
        assert_eq!(kept(source, &[engine]), vec!["jobFinished"]);
    }

    #[test]
    fn a_bus_imported_through_the_services_alias_is_the_repos() {
        let tsconfig =
            r#"{ "compilerOptions": { "baseUrl": ".", "paths": { "~/*": ["./src/*"] } } }"#;
        let repo = Repo::new(
            &[
                (
                    "src/listener.ts",
                    "import { bus } from \"~/events/bus\";\nbus.on(\"orderPlaced\", h);\n",
                ),
                ("src/events/bus.ts", BUS_MODULE.1),
            ],
            Some(tsconfig),
        );
        assert_eq!(
            events(&repo.scan("src/listener.ts").subscribers),
            vec!["orderPlaced"]
        );
    }

    #[test]
    fn this_inside_a_declared_class_is_the_repos() {
        let source = r#"import { EventEmitter } from "node:events";
class Feed extends EventEmitter {
  start() {
    this.on("tick", h);
  }
}
"#;
        assert_eq!(kept(source, &[]), vec!["tick"]);
    }

    #[test]
    fn a_function_the_file_declares_is_the_repos() {
        let source = r#"function makeBus() {
  return { on(event: string, handler: () => void) {} };
}
makeBus().on("stockLow", h);
"#;
        assert_eq!(kept(source, &[]), vec!["stockLow"]);
    }

    #[test]
    fn a_field_typed_by_an_in_repo_class_is_the_repos() {
        let source = r#"import { Feed } from "./feed";
class Poller {
  constructor(private readonly feed: Feed) {}
  start() {
    this.feed.on("batchReady", h);
  }
}
"#;
        let feed = ("src/feed.ts", "export class Feed {}\n");
        assert_eq!(kept(source, &[feed]), vec!["batchReady"]);
    }

    #[test]
    fn a_parameter_typed_in_repo_is_the_repos() {
        let source = r#"import type { OrderBus } from "./order-bus";
function wire(source: OrderBus) {
  source.on("orderShipped", h);
}
"#;
        let order_bus = ("src/order-bus.ts", "export interface OrderBus {}\n");
        assert_eq!(kept(source, &[order_bus]), vec!["orderShipped"]);
    }

    #[test]
    fn a_transport_client_is_kept_whatever_it_was_built_with() {
        let source = r#"import { Subscriber } from "@acme/broker";
const sub = new Subscriber(process.env.BROKER_URL);
sub.on("invoicePaid", h);
"#;
        assert_eq!(kept(source, &[]), vec!["invoicePaid"]);
    }

    #[test]
    fn a_line_reader_from_a_runtime_module_is_not_a_contract() {
        let source = r#"import * as readline from "node:readline";
const reader = readline.createInterface({ input: process.stdin });
reader.on("line", handle);
"#;
        assert!(kept(source, &[]).is_empty());
    }

    #[test]
    fn a_listener_chained_on_a_runtime_call_is_not_a_contract() {
        let source = r#"import { createInterface } from "node:readline";
createInterface({ input: child.stdout }).on("line", h);
"#;
        assert!(kept(source, &[]).is_empty());
    }

    #[test]
    fn a_required_runtime_module_is_not_a_contract() {
        let source = r#"const readline = require("readline");
readline.createInterface({ input: process.stdin }).on("line", h);
"#;
        assert!(kept(source, &[]).is_empty());
    }

    #[test]
    fn a_package_instance_built_with_arguments_is_the_packages() {
        let source = r#"import { Watcher } from "@acme/fs-watch";
const w = new Watcher("./src");
w.on("changed", h);
"#;
        assert!(kept(source, &[]).is_empty());
    }

    #[test]
    fn a_page_from_a_package_call_is_the_packages() {
        let source = r#"import { launch } from "@acme/browser";
async function run() {
  const page = await (await launch()).newPage();
  page.on("pageerror", h);
}
"#;
        assert!(kept(source, &[]).is_empty());
    }

    #[test]
    fn a_package_export_is_the_packages() {
        let source = r#"import { app } from "@acme/desktop";
app.on("activate", show);
"#;
        assert!(kept(source, &[]).is_empty());
    }

    #[test]
    fn a_global_is_not_the_repos() {
        assert!(kept("process.stdout.on(\"resize\", h);\n", &[]).is_empty());
    }

    #[test]
    fn an_unannotated_parameter_is_not_placed() {
        let source = r#"function wire(source) {
  source.on("tick", h);
}
"#;
        assert!(kept(source, &[]).is_empty());
    }

    #[test]
    fn a_field_typed_by_a_package_is_the_packages() {
        let source = r#"import { Connection } from "@acme/db";
class Tailer {
  private conn: Connection;
  start() {
    this.conn.on("rowStream", h);
  }
}
"#;
        assert!(kept(source, &[]).is_empty());
    }

    #[test]
    fn a_method_result_read_off_this_is_not_placed() {
        let source = r#"import { EventEmitter } from "node:events";
class Feed extends EventEmitter {
  start() {
    this.channel().on("tick", h);
  }
}
"#;
        assert!(kept(source, &[]).is_empty());
    }

    #[test]
    fn a_field_initialised_with_an_owned_emitter_is_the_repos() {
        let source = r#"import { EventEmitter } from "node:events";
class Feed {
  private readonly events = new EventEmitter();
  start() {
    this.events.on("tick", h);
  }
}
"#;
        assert_eq!(kept(source, &[]), vec!["tick"]);
    }

    #[test]
    fn a_shadowed_bus_is_not_the_import() {
        let source = r#"import { bus } from "./bus";
import { createInterface } from "node:readline";
function read() {
  const bus = createInterface({ input: process.stdin });
  bus.on("line", h);
}
bus.on("orderPlaced", h);
"#;
        assert_eq!(kept(source, &[BUS_MODULE]), vec!["orderPlaced"]);
    }
}
