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
//! - **A name is read in its own scope** (carrick#1648). A constant, a local
//!   or a parameter is the binding the identifier resolves to
//!   ([`crate::binding_scope`]), so a block that declares the name again is
//!   never read as the outer value. Only a function's own top-level
//!   declarations are read; a name a nested block declares is opaque.
//! - **A request is the shape the rest of the scanner reads.** A `fetch`
//!   call, an HTTP-verb member call with one route-shaped argument, or a call
//!   carrying exactly one request-options bag (`method`/`headers`/`body`/
//!   `data`). A call the call graph resolves to a function in this service is
//!   never read as a request: it is composed instead.
//! - **A `fetch` handed in is `fetch`** (carrick#1562). A parameter whose
//!   default is the platform's `fetch` (`fetchImpl = fetch`, `{ fetchImpl =
//!   fetch } = {}`) and that the body never assigns again, and a field the
//!   field table reads as set once to the global, to such a constructor
//!   parameter, or to `options.fetch ?? fetch`, are the platform's `fetch`,
//!   whatever a caller hands in instead.
//! - **A builder's return is a value** (carrick#1562). A call to a
//!   module-scope builder (an arrow or a function whose body only returns an
//!   expression, or one held in a constant object nothing writes through) is
//!   what it returns, with the call's arguments in its parameters.
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
//!
//! A module-scope instance another module imports is the client there too
//! (carrick#1568). The import is resolved by the call graph's own walk
//! ([`crate::call_graph::ImportedBindings`]), and the instance is read where
//! it is declared, with every module's uses of it merged ([`LinkedClients`]):
//! one object, so a use that may change it anywhere changes it everywhere.
//! The receiver rules are in `docs/reference/client-semantics.md`.
//!
//! **Message roles** (carrick#1661). The same receiver core reads every call
//! through a package export or an instance of one, role-neutrally, for the
//! library-claim readers of brokers, sockets and buses ([`library_sites`]).
//! What it reads beyond the HTTP reading (module-level calls, `new` makers,
//! sub-object hops, literal arguments) is kept apart from what the summaries
//! read, so no HTTP row moves. It also reads through the service's own code
//! (carrick#1562): the instance an own factory returns, a name a caller
//! passes in, and a name an object constant, an imported constant or a
//! builder holds. Only the message roles read an own factory's instance.

// Read by the message-role row writers (carrick#1662) and the library-store
// reader (carrick#1664). Until they land, only tests call it, so the binary
// sees it as unused.
#[allow(dead_code)]
mod library_sites;

#[allow(unused_imports)]
pub use library_sites::{
    Contest, Holder, LibrarySite, LibrarySites, MakerForm, MemberUse, MemberWire, On, Selector,
    SiteArg, SiteMaker, SiteObject, SiteReceiver, library_sites, package_name,
};

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use swc_common::{SourceMap, SourceMapper, Span, Spanned, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use crate::binding_scope::{BindingKey, Declarations, ident_key, pat_key};
use crate::call_graph::{
    BindingsWanted, CallSiteTargets, ImportedBinding, ImportedBindings, PublishedBinding,
};
use crate::client_semantics::{LibrarySemantics, instance_receiver};
use crate::commonjs::{require_bound_names, require_specifier};
use crate::import_bindings::DEFAULT_EXPORT;
use crate::services::type_sidecar::{SemanticsRequestArgs, SemanticsVerbArgs};
use crate::swc_scanner::SWC_SPAN_BASE;
use crate::type_manifest::is_http_method;
use crate::visitor::SymbolKind;
use crate::wrapper_request_shape::{is_request_options, verb_from_callee_property};
use library_sites::{ClassThis, FieldWriteIr, LibrarySiteIr};

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
    /// The file whose scope `base` was read in: the module that declares the
    /// client (carrick#1568). A name in it means what that module binds.
    base_scope: Option<PathBuf>,
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
    /// The file uses the binding, or for an instance the binding of the
    /// export it was built from, other than to call through it or export it
    /// ([`BindingUse::contests_client`]). Such a use can change where its
    /// requests go (`const d = api.defaults; d.baseURL = …`), so nothing is
    /// read through it.
    contested: bool,
    /// The members the file calls through the binding (`None`: the binding
    /// itself called). An instance with a call outside its verified surface
    /// is read through nothing: the call may change its base
    /// (`api.setBaseURL("/v2")`).
    called: BTreeSet<Option<String>>,
    /// A member called by a key the source does not state.
    called_computed: bool,
    /// The same contest for a message role (carrick#1661,
    /// [`BindingUse::contests_message`]): a `new` of the export and a call
    /// through a sub-object of it contest nothing there.
    contested_message: bool,
    /// Every member called or constructed through the binding, for a
    /// message role to classify against the package's surface (carrick#1661).
    member_uses: BTreeSet<MemberUse>,
    /// For an instance: the member uses of the export's binding in the module
    /// that made it, which may configure what the maker built.
    export_uses: BTreeSet<MemberUse>,
    /// Where the binding, or for an instance the export's binding, is
    /// returned ([`BindingUse::returned_at`]). A message role reads a return
    /// as a hand-off, except the one an own factory makes of the instance it
    /// builds, which is followed to the factory's callers (carrick#1562).
    returned: BTreeSet<u32>,
}

impl ClientRef {
    /// This client, held by a binding the file uses as `used`.
    fn used_as(mut self, used: &BindingUse) -> Self {
        self.contested |= used.contests_client();
        self.called = used.called.clone();
        self.called_computed = used.called_computed;
        self.contested_message |= used.contests_message();
        self.member_uses = used.member_uses().collect();
        self.returned.extend(used.returned_at.iter().copied());
        self
    }

    /// `other`'s uses added to this client's: one maker's instance, handed
    /// the same arguments, held by two bindings an own factory returns is
    /// one maker, used as both are (carrick#1689). Before, the second
    /// binding's uses were dropped.
    fn absorb(&mut self, other: &ClientRef) {
        self.contested |= other.contested;
        self.called.extend(other.called.iter().cloned());
        self.called_computed |= other.called_computed;
        self.contested_message |= other.contested_message;
        self.member_uses.extend(other.member_uses.iter().cloned());
        self.export_uses.extend(other.export_uses.iter().cloned());
        self.returned.extend(other.returned.iter().copied());
    }
}

/// How an instance was made from a package export (carrick#1564,
/// carrick#1661): called (`export.member(…)`, `export(…)`) or constructed
/// (`new export(…)`, `new export.member(…)`), with what the maker was handed,
/// read where the instance is built.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClientInstance {
    form: MakerForm,
    /// The export's member the maker is, or `None` for the export itself.
    member: Option<String>,
    /// The one object literal the maker was handed, read as a request reads
    /// it: what an HTTP factory's base comes from. `None` unless the maker
    /// is handed exactly that.
    options: Option<ObjValue>,
    /// Every argument, as a library claim's slot reads it.
    args: Vec<SiteArg>,
    /// Where the maker call is written, in the module that declares the
    /// instance.
    site: Site,
    /// What made it: the export's maker, or a call to a function the
    /// service may declare (carrick#1562).
    made_by: MadeBy,
    /// The call is awaited where the instance is held (`await connect()`).
    awaited: bool,
}

/// What made an instance (carrick#1562).
#[derive(Debug, Clone, PartialEq, Eq)]
enum MadeBy {
    /// A maker of the package export the [`ClientRef`] names. Where the call
    /// graph resolves the maker call to a function of the service (an alias
    /// import of one), it is an own call after all.
    Export,
    /// A call to a function the service may declare: the instance is what
    /// that function returns ([`FnIr::returned`]), read once the call graph
    /// says which function it is. Its [`ClientRef`] names no package. The
    /// callee is kept as the source names it, so two writes of a field are
    /// one maker's only when they call the same binding.
    Call(OwnCallee),
}

/// The function an own call names (carrick#1562).
#[derive(Debug, Clone, PartialEq, Eq)]
enum OwnCallee {
    /// `make(…)`.
    Binding(BindingKey),
    /// `this.make(…)`, `this.#make(…)` (`None`), or `ns.a.make(…)`: the
    /// root binding and every member after it.
    Member(Option<BindingKey>, Vec<String>),
}

impl ClientInstance {
    /// The HTTP factory call this is, when it is one: `export.member({ … })`
    /// with exactly one object literal, the only maker an HTTP reading reads
    /// (carrick#1564). Every other form builds an instance only a message
    /// role reads (carrick#1661), so an HTTP call through it reads as it
    /// would through no client at all.
    ///
    /// `new export.member({ … })` needs no check of its own here: the
    /// construction contests the export's binding for HTTP
    /// ([`BindingUse::contests_client`]), and the instance inherits that.
    /// An instance an own call returns (carrick#1562) names no member and
    /// reads no options, so it is no HTTP factory's.
    fn http_factory(&self) -> Option<(&str, &ObjValue)> {
        match (&self.member, &self.options) {
            (Some(member), Some(options)) => Some((member, options)),
            _ => None,
        }
    }
}

/// A call made through a library client: on one of its members, or on the
/// client itself (`member: None`).
#[derive(Debug, Clone)]
struct CallReceiver {
    client: ClientBinding,
    member: Option<String>,
}

/// The binding a call names as its library client (carrick#1564,
/// carrick#1568). Which client it holds is settled when the summaries are
/// composed, because another module can import a module-scope instance and
/// every module's uses of it count.
#[derive(Debug, Clone)]
enum ClientBinding {
    /// A client no other module can reach: an instance a function or a class
    /// field holds. Read as the file states it.
    Own(ClientRef),
    /// A module-scope binding of this file that holds an instance.
    Module(String),
    /// A binding this file imports: the instance the module that declares it
    /// holds, when the import resolves to one, and otherwise the package
    /// client the import names directly (`None` for a relative specifier).
    Imported {
        local: String,
        package: Option<ClientRef>,
    },
}

/// Where a call is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Site {
    /// Span start in the discovery source map, which is the key
    /// [`CallSiteTargets`] is read with.
    lo: u32,
    /// Span end in the discovery source map: with `lo`, the one call
    /// [`CallSiteTargets::target_at`] names.
    hi: u32,
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
    /// The arguments as a library claim's slot reads them
    /// ([`library_sites::site_arg`]), for a callee whose library call takes
    /// a name from a parameter (carrick#1562). `None` when an argument is
    /// spread: it moves every position after it.
    site_args: Option<Vec<SiteArg>>,
    /// The line of the member the call names, or of the callee: where a
    /// library call a caller fills is placed ([`LibrarySite::line`]).
    name_line: u32,
}

/// One function body, reduced to what a request summary reads.
#[derive(Debug, Clone, Default)]
struct FnIr {
    calls: Vec<CallIr>,
    /// The calls and constructions made through a library client, for the
    /// message-role readers (carrick#1661). Kept apart from `calls`, which
    /// the summaries compose: a `new` there would make a body incomplete.
    library: Vec<LibrarySiteIr>,
    /// The writes `this.<field> = <maker>(…)` an instance member makes, for
    /// the message-role field rule (carrick#1665). Taken out by the class
    /// that reads them ([`library_sites::take_field_writes`]).
    field_writes: Vec<FieldWriteIr>,
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
    /// Every `return` the body makes (an arrow's expression body included),
    /// by where it starts, with the instances it may return when it returns
    /// one built here ([`Reader::returned_instances`]): one, or one per path
    /// for a `let` set on every path (carrick#1689). None for anything else.
    /// Settled into `returned`.
    returns: Vec<(u32, Vec<ClientRef>)>,
    /// What a call of this function returns, when it is an own factory
    /// (carrick#1562): every `return` returns an instance the call builds
    /// anew, of one of its makers, each handed its own arguments
    /// (carrick#1689).
    returned: Option<Returned>,
}

/// The instance an own factory returns (carrick#1562).
#[derive(Debug, Clone)]
struct Returned {
    /// Each maker the instance may come from (carrick#1689), as the
    /// factory's body holds it: the maker (or the own call it came from),
    /// what it was handed in the factory's scope, and the factory's own uses
    /// of the bindings that hold it. One maker's instance returned through
    /// two bindings is one maker, used as both are.
    makers: Vec<ClientRef>,
    /// Where each `return` starts ([`BindingUse::returned_at`]): a return
    /// of the holder anywhere else hands the instance on unseen.
    at: BTreeSet<u32>,
    /// The function is `async`: its call returns a promise, so the instance
    /// is reached only through `await`.
    is_async: bool,
}

impl FnIr {
    /// Settle `returns` into `returned`: an instance on every `return`, of
    /// one or more makers, each handed its own arguments (carrick#1689; one
    /// maker before). How many makers that comes to once own calls are
    /// followed is the reader's to judge ([`library_sites::MAX_MAKERS`]). A
    /// generator returns no instance to its caller.
    fn settle_returned(&mut self, is_async: bool, is_generator: bool) {
        let returns = std::mem::take(&mut self.returns);
        if is_generator || returns.is_empty() || returns.iter().any(|(_, made)| made.is_empty()) {
            return;
        }
        let mut makers: Vec<ClientRef> = Vec::new();
        for client in returns.iter().flat_map(|(_, made)| made) {
            match makers
                .iter_mut()
                .find(|known| library_sites::same_maker(known, client))
            {
                Some(known) => known.absorb(client),
                None => makers.push(client.clone()),
            }
        }
        self.returned = Some(Returned {
            makers,
            at: returns.iter().map(|(at, _)| *at).collect(),
            is_async,
        });
    }
}

/// Everything one file contributes: its functions, keyed exactly as
/// [`crate::visitor::FunctionDefinitionExtractor`] keys them, so a
/// [`CallSiteTargets`] entry names them directly.
#[derive(Debug, Default)]
pub struct FileIr {
    functions: HashMap<String, FnIr>,
    /// What this module's module-scope bindings hold, for this file's own
    /// calls and for every module that imports one (carrick#1568): each
    /// `const` a factory call built, and an anonymous `export default
    /// <factory call>` under [`DEFAULT_EXPORT`].
    module_clients: HashMap<String, ClientRef>,
    /// Every import binding the file uses, with how it uses it. What a module
    /// that declares one of them holds is changed by these uses too.
    imported: HashMap<String, BindingUse>,
    /// The modules the file loads other than through an import binding
    /// ([`module_loads`]): everything each publishes may be changed here.
    loads: BTreeSet<String>,
    /// Every specifier this file loads a module by at run time
    /// ([`value_specifiers`]).
    value_specifiers: BTreeSet<String>,
    /// The `??`/`||` defaults this module's environment reads are declared
    /// with ([`crate::env_alias::UrlBindings::env_fallbacks`]), kept for a
    /// module that holds a client: a row another module states through it
    /// describes its base with them, as this module's own rows do.
    env_fallbacks: BTreeMap<String, String>,
    /// The file names `module.exports` or `exports`. What it publishes that
    /// way is read only at module level, last write winning
    /// ([`crate::commonjs::export_assignments`]), so a write anywhere else can
    /// replace it unseen: nothing it holds is read in another module.
    names_commonjs_exports: bool,
    /// The calls written at module level, outside every function the file
    /// declares: a definition made where the module is loaded (`export const
    /// t = task({ … })`) is one. Read for the message-role readers only
    /// (carrick#1661); the summaries are composed from the functions alone.
    module_level: FnIr,
    /// Each class field that holds one maker's instance as a message role
    /// reads it (carrick#1665), keyed by its class (the class's span start)
    /// and the field. An HTTP reading reads the class's own field table.
    field_receivers: HashMap<(u32, String), ClientRef>,
    /// What each module-scope binding a name may read through holds
    /// (carrick#1562): a `const` whose initialiser is text, a constant
    /// object this module keeps ([`BindingUse::keeps_entries`]), or a
    /// builder. Read by name, for this module's names and every importer's.
    names: HashMap<String, library_sites::NameValue>,
}

impl FileIr {
    /// The import bindings whose declaring module the summaries read, the
    /// modules the file loads, and, when `every_specifier`, every specifier
    /// it loads a module by ([`crate::call_graph::resolve_call_edges`]'s
    /// `bindings_wanted`).
    pub fn bindings_wanted(&self, every_specifier: bool) -> BindingsWanted {
        BindingsWanted {
            locals: self.imported.keys().cloned().collect(),
            loads: self.loads.clone(),
            specifiers: if every_specifier {
                self.value_specifiers.clone()
            } else {
                BTreeSet::new()
            },
        }
    }

    /// Whether this module holds a client another module may import. Only
    /// then does a specifier that names nothing matter
    /// ([`LinkedClients`]). What an own call returns counts (carrick#1562),
    /// so most modules with a module-scope call hold one; an HTTP reading
    /// reads no such instance ([`ClientInstance::http_factory`]), so a
    /// service with no HTTP factory instance loses no HTTP row to it.
    pub fn holds_instances(&self) -> bool {
        !self.module_clients.is_empty()
    }
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
    let jsx = jsx_names(module);
    let mut uses = BindingUses::default();
    module.visit_with(&mut uses);
    uses.settle_aliases(&jsx);
    let (imports, bound_requires) = import_bindings(module);
    // Every import the file uses, however it uses it: a module that declares
    // one holds what these uses may change (carrick#1568). Kept whether or
    // not the name is declared again below, since nothing here says which
    // of its uses mean the import.
    let imported: HashMap<String, BindingUse> = imports
        .keys()
        .filter_map(|name| Some((name.clone(), uses.uses.get(name)?.clone())))
        .collect();
    let mut module_scope = ModuleScope {
        consts: HashMap::new(),
        texts: HashMap::new(),
        named: HashSet::new(),
        builders: HashMap::new(),
        receivers: HashMap::new(),
        imports: HashSet::new(),
        uses: uses.uses,
        object_consts: object_consts(module),
        subclass_fields: subclass_fields(module),
        class_this: library_sites::class_this(module),
        declared_below: redeclared_names(module),
    };
    module_scope.receivers = import_receivers(&imports)
        .into_iter()
        .map(|(name, client)| {
            let client = module_scope.with_uses(client, &name);
            (name, client)
        })
        .collect();
    // A name some function or block declares again means something else
    // there, and a client is held by name across the file, so it is no
    // client anywhere in the file.
    let redeclared = &module_scope.declared_below;
    module_scope
        .receivers
        .retain(|name, _| !redeclared.declares_name(name));
    module_scope.imports = imports
        .iter()
        .filter(|(name, import)| {
            !import.namespace && !import.reassignable && !redeclared.declares_name(name)
        })
        .map(|(name, _)| name.clone())
        .collect();
    let value_specifiers = value_specifiers(module, |name| {
        module_scope.uses.contains_key(name) || jsx.contains(name)
    });
    let mut file = FileIr {
        imported,
        value_specifiers,
        loads: module_loads(module, &bound_requires),
        names_commonjs_exports: names_commonjs_exports(module),
        ..FileIr::default()
    };
    let reader = Reader {
        source_map,
        builder_depth: std::cell::Cell::new(0),
    };

    // Module constants first, in source order, so a later one can read an
    // earlier one (`const BASE = ...; const USERS = `${BASE}/users``).
    for item in &module.body {
        if let Some(decl) = module_var_decl(item)
            && decl.kind == VarDeclKind::Const
        {
            for declarator in &decl.decls {
                let (Pat::Ident(ident), Some(init)) = (&declarator.name, &declarator.init) else {
                    continue;
                };
                let name = ident.id.sym.to_string();
                // What a name may read through it at link time
                // (carrick#1562): a builder, a constant object this module
                // keeps, or text.
                let named = library_sites::name_value(init, &module_scope).filter(|value| {
                    !matches!(value, library_sites::NameValue::Map(_))
                        || module_scope
                            .uses
                            .get(&name)
                            .is_none_or(|used| used.keeps_entries(value))
                });
                if let Some(value) = named {
                    if !matches!(value, library_sites::NameValue::Text(_)) {
                        module_scope.named.insert(ident_key(&ident.id));
                    }
                    file.names.insert(name.clone(), value);
                }
                // What a request's value may be built by: the builder
                // itself, or one an object holds that nothing writes through.
                if let Some(builder) = Builder::of(init) {
                    module_scope
                        .builders
                        .insert((ident_key(&ident.id), Vec::new()), builder);
                } else if let Expr::Object(object) =
                    crate::graphql_document_sites::unwrap_expression(init)
                    && module_scope.object_consts.contains(&name)
                    && !module_scope.written_through(&name)
                {
                    let mut found = Vec::new();
                    Builder::in_object(object, &mut Vec::new(), &mut found);
                    for (path, builder) in found {
                        module_scope
                            .builders
                            .insert((ident_key(&ident.id), path), builder);
                    }
                }
                if !matches!(&**init, Expr::Arrow(_) | Expr::Fn(_)) {
                    let (value, client, text) = {
                        let scope = Scope::module(&module_scope);
                        (
                            reader.eval(init, &scope),
                            reader.written_instance(init, &scope),
                            library_sites::text_pieces(init, &scope),
                        )
                    };
                    // Every maker form is an instance here (carrick#1661),
                    // and so is what an own call returns (carrick#1562),
                    // and a module holding one asks for every specifier
                    // ([`FileIr::holds_instances`]). An HTTP reading still
                    // reads only an HTTP factory's instance
                    // ([`ClientInstance::http_factory`]), and a service that
                    // held none before held no HTTP instance a specifier
                    // could take away.
                    if let Some(client) = client
                        && !redeclared.declares_name(&name)
                    {
                        let client = module_scope.with_uses(client, &name);
                        file.module_clients.insert(name.clone(), client.clone());
                        module_scope.receivers.insert(name, client);
                    }
                    if let Some(text) = text {
                        module_scope.texts.insert(ident_key(&ident.id), text);
                    }
                    module_scope.consts.insert(ident_key(&ident.id), value);
                }
            }
        }
        // `export default http.create({ ... })`: an instance no name in this
        // file holds, which a module importing the default reads.
        if let ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(export)) = item
            && let Some(client) =
                reader.written_instance(&export.expr, &Scope::module(&module_scope))
        {
            file.module_clients
                .insert(DEFAULT_EXPORT.to_string(), client);
        }
    }
    // A function declared at module scope whose body only returns text is a
    // builder a name may read through (carrick#1562), unless the module
    // assigns its binding again. Read once every constant is known, since a
    // declaration is hoisted.
    let mut reassigned = Reassigned::default();
    module.visit_with(&mut reassigned);
    for item in &module.body {
        if let Some(Decl::Fn(fn_decl)) = module_decl(item)
            && !reassigned.names.contains(fn_decl.ident.sym.as_ref())
            && let Some(builder) = Builder::of_function(&fn_decl.function)
        {
            if let Some(value) = library_sites::builder(&builder, &module_scope) {
                module_scope.named.insert(ident_key(&fn_decl.ident));
                file.names.insert(fn_decl.ident.sym.to_string(), value);
            }
            module_scope
                .builders
                .insert((ident_key(&fn_decl.ident), Vec::new()), builder);
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
                    &class.ident,
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
                    reader.class(ident, class, module_scope, definition_keys, &mut file);
                }
                _ => {}
            }
        }
    }
    // The calls made where the module is loaded (carrick#1661): every
    // statement and initialiser outside the functions read above. A function
    // written inside one is read where it is written, as a callback or a
    // detached function, exactly as inside a body.
    {
        let scope = Scope::module(module_scope);
        let mut module_level = FnIr::default();
        for item in &module.body {
            match item {
                ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(export)) => {
                    reader.walk(&*export.expr, &scope, &mut module_level);
                }
                // `export default function () {}`: no name keys it, so it is
                // read here, as a detached function.
                ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(ExportDefaultDecl {
                    decl:
                        DefaultDecl::Fn(FnExpr {
                            ident: None,
                            function,
                        }),
                    ..
                })) => reader.walk(&**function, &scope, &mut module_level),
                ModuleItem::ModuleDecl(ModuleDecl::TsExportAssignment(assign)) => {
                    reader.walk(&*assign.expr, &scope, &mut module_level);
                }
                _ => match module_decl(item) {
                    Some(Decl::Var(var)) => {
                        for declarator in &var.decls {
                            if let Some(init) = &declarator.init
                                && !matches!(&**init, Expr::Arrow(_) | Expr::Fn(_))
                            {
                                reader.walk(&**init, &scope, &mut module_level);
                            }
                        }
                    }
                    // A function or a class is read above.
                    Some(_) => {}
                    None => {
                        if let ModuleItem::Stmt(stmt) = item {
                            reader.walk(stmt, &scope, &mut module_level);
                        }
                    }
                },
            }
        }
        file.module_level = module_level;
    }
    // What a row stated in another module needs to say about a client's
    // base the way this module's own rows say it (carrick#1568). Any file
    // that imports a package's client may build one.
    if !module_scope.receivers.is_empty() || !file.module_clients.is_empty() {
        file.env_fallbacks = crate::env_alias::EnvAliasExtractor::build_bindings(module)
            .env_fallbacks
            .into_iter()
            .collect();
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
    /// Each module-scope `const`, by binding: a block or a function that
    /// declares the name again holds a binding of its own, which is not this
    /// one (carrick#1648).
    consts: HashMap<BindingKey, Value>,
    /// Each module-scope `const` whose initialiser is text, by binding
    /// ([`library_sites::text_pieces`]): what a library claim's name slot
    /// reads through an identifier (carrick#1661). Its text may name a
    /// constant object's entry, a builder's return or an import, read at
    /// link time (carrick#1562).
    texts: HashMap<BindingKey, Vec<library_sites::TextPiece>>,
    /// The module-scope constant objects and builders a name may read
    /// through at link time ([`FileIr::names`]), by binding (carrick#1562).
    named: HashSet<BindingKey>,
    /// Each module-scope builder a request's value may be built by
    /// (carrick#1562), by its binding and the path of entries to it inside a
    /// constant object (empty for the binding itself): a `const` arrow or
    /// function expression, a function declaration nothing assigns again,
    /// or one held in a constant object nothing writes through. Each only
    /// returns an expression ([`Reader::eval`] reads it with the call's
    /// arguments in its parameters).
    builders: HashMap<(BindingKey, Vec<String>), Builder>,
    receivers: HashMap<String, ClientRef>,
    /// Import bindings no scope below declares again, other than namespace
    /// imports: each may hold an instance the module it names declares
    /// (carrick#1568).
    imports: HashSet<String>,
    /// [`BindingUses`] over the whole file.
    uses: HashMap<String, BindingUse>,
    /// Names declared once in the file, by `const <name> = { … }`.
    object_consts: HashSet<String>,
    /// Class name -> the fields a class in this file that extends it
    /// declares or writes again.
    subclass_fields: HashMap<String, HashSet<String>>,
    /// Each class the file declares, by its binding -> what the class does
    /// with `this`, for the message-role field rule (carrick#1665).
    class_this: HashMap<BindingKey, ClassThis>,
    /// Every declaration below module scope ([`redeclared_names`]): what a
    /// function, a block or a parameter binds.
    declared_below: Declarations,
}

impl ModuleScope {
    /// `client`, contested when the file uses `binding` other than to call
    /// through it or export it (carrick#1564 re-review, R6).
    fn with_uses(&self, client: ClientRef, binding: &str) -> ClientRef {
        match self.uses.get(binding) {
            Some(used) => client.used_as(used),
            None => client,
        }
    }

    /// Whether the file writes through `name` (`name.key = …`, `delete
    /// name.key`, `name.key++`).
    fn written_through(&self, name: &str) -> bool {
        self.uses.get(name).is_some_and(|used| used.written)
    }

    /// Whether a spread of `name` puts in place exactly the keys its object
    /// literal writes (carrick#1564 re-review, R2): it is declared once, as a
    /// `const` holding that literal, and the file never writes through it,
    /// passes it to a call or aliases it.
    fn spreadable(&self, name: &str) -> bool {
        self.object_consts.contains(name)
            && self.uses.get(name).is_none_or(BindingUse::keeps_object)
    }
}

/// A function whose body only returns an expression, as a request's value
/// reads a call to it (carrick#1562): its parameters and what it returns.
struct Builder {
    params: Vec<Option<BindingKey>>,
    returned: Box<Expr>,
}

impl Builder {
    /// The builder `expr` is: an arrow, or a function expression, that only
    /// returns an expression.
    fn of(expr: &Expr) -> Option<Self> {
        match crate::graphql_document_sites::unwrap_expression(expr) {
            Expr::Arrow(arrow) => Some(Builder {
                params: arrow.params.iter().map(pat_key).collect(),
                returned: Box::new(match &*arrow.body {
                    BlockStmtOrExpr::Expr(expr) => (**expr).clone(),
                    BlockStmtOrExpr::BlockStmt(block) => only_return(&block.stmts)?.clone(),
                }),
            }),
            Expr::Fn(function) => Self::of_function(&function.function),
            _ => None,
        }
    }

    fn of_function(function: &Function) -> Option<Self> {
        Some(Builder {
            params: function
                .params
                .iter()
                .map(|param| pat_key(&param.pat))
                .collect(),
            returned: Box::new(only_return(&function.body.as_ref()?.stmts)?.clone()),
        })
    }

    /// Every builder an object literal holds, by the path of entries to it.
    fn in_object(
        object: &ObjectLit,
        path: &mut Vec<String>,
        out: &mut Vec<(Vec<String>, Builder)>,
    ) {
        for prop in &object.props {
            let PropOrSpread::Prop(prop) = prop else {
                continue;
            };
            let Prop::KeyValue(kv) = &**prop else {
                continue;
            };
            let Some(key) = prop_name(&kv.key) else {
                continue;
            };
            path.push(key);
            match crate::graphql_document_sites::unwrap_expression(&kv.value) {
                Expr::Object(inner) => Self::in_object(inner, path, out),
                value => {
                    if let Some(builder) = Self::of(value) {
                        out.push((path.clone(), builder));
                    }
                }
            }
            path.pop();
        }
    }
}

/// The expression a body of exactly one `return <expr>;` returns.
fn only_return(stmts: &[Stmt]) -> Option<&Expr> {
    match stmts {
        [Stmt::Return(ReturnStmt { arg: Some(arg), .. })] => Some(arg),
        _ => None,
    }
}

/// How one binding is used beyond its own declaration: an identifier, or a
/// field of `this` keyed `this.<field>` ([`binding_key`]).
#[derive(Debug, Default, Clone)]
struct BindingUse {
    /// The members called through it (`None`: the binding itself called).
    called: BTreeSet<Option<String>>,
    /// A member called through it by a key the source does not state
    /// (`api[name](…)`).
    called_computed: bool,
    /// The root of a member assignment, update or `delete` target
    /// (`api.defaults.baseURL = …`, `o.x++`).
    written: bool,
    /// The object of a member read that is not called (`api.defaults`).
    member_read: bool,
    /// Spread into an object or array literal.
    spread: bool,
    /// Anything else: an argument, an initialiser, a property value, a
    /// returned value, an array element, an operand, a test. An operand of
    /// `instanceof`, `typeof` or a comparison is not: it reads the value and
    /// keeps nothing of it (carrick#1568).
    other: bool,
    /// Exported by name (`export { api }`, `export default api`,
    /// `module.exports.api = api`). Not a use that changes anything; it says
    /// another module may import the binding (carrick#1568).
    exported: bool,
    /// Read as an operand of `instanceof`, `typeof` or a comparison: a value
    /// use, which keeps an import at run time, that changes nothing.
    read: bool,
    /// Tested for truth: the whole test of an `if`, a loop or a conditional,
    /// or the operand of `!` (`if (!this.client)`). A message role reads it
    /// as [`Self::read`] (carrick#1665); before, it was the operand it is to
    /// an HTTP reading, which still contests on it.
    tested: bool,
    /// Calls through a sub-object of it (`client.tasks.trigger(…)`, every hop
    /// a plain name) and constructions of it or of a member (`new Queue(…)`,
    /// `new lib.Worker(…)`), as the message roles read them (carrick#1661).
    /// Before, each was the member read or the operand it is to an HTTP
    /// reading, which still contests on it ([`Self::contests_client`]).
    library_calls: BTreeSet<MemberUse>,
    /// Where it is returned: a `return <binding>` statement, or an arrow
    /// whose expression body is the binding, by the span start of the
    /// returned expression's statement (discovery numbering). A message
    /// role follows a return an own factory makes to the factory's callers
    /// (carrick#1562, [`FnIr::returned`]) and takes the binding away for any
    /// other ([`ClientRef::returned`]); before, it was the operand it is to
    /// an HTTP reading, which still contests on it.
    returned_at: BTreeSet<u32>,
    /// The member chains read off it as values, every hop a plain name
    /// (`TOPICS.orders` is `["orders"]`): among the [`Self::member_read`]s,
    /// the entries of a constant object a name may read (carrick#1562,
    /// [`Self::keeps_entries`]).
    entry_reads: BTreeSet<Vec<String>>,
}

impl BindingUse {
    /// A client binding used for anything but calls through it (and being
    /// exported) could have its base changed by code this pass does not
    /// read, so nothing is read through it.
    ///
    /// A call through a sub-object and a construction contest exactly as
    /// the member read and the operand they were recorded as before
    /// carrick#1661, a truthiness test as the operand it was before
    /// carrick#1665, and a return as the operand it was before carrick#1562.
    fn contests_client(&self) -> bool {
        self.written
            || self.member_read
            || self.spread
            || self.other
            || self.tested
            || !self.library_calls.is_empty()
            || !self.returned_at.is_empty()
    }

    /// The same for a message role (carrick#1661): a hand-off, a write, a
    /// member read that is not called, a spread, or a member called by a key
    /// the source does not state. Constructing the binding is what a `new`
    /// maker does, and a call through a sub-object is a call; both are among
    /// [`Self::member_uses`], which the caller classifies. A truthiness test
    /// keeps nothing of the binding. A return is the reader's to judge
    /// ([`BindingUse::returned_at`]): followed to an own factory's callers,
    /// or a hand-off.
    fn contests_message(&self) -> bool {
        self.written || self.member_read || self.spread || self.other || self.called_computed
    }

    /// Every use of `other` added to these (one class's uses of a field and
    /// a related class's, carrick#1665).
    fn merge(&mut self, other: &BindingUse) {
        self.called.extend(other.called.iter().cloned());
        self.called_computed |= other.called_computed;
        self.written |= other.written;
        self.member_read |= other.member_read;
        self.spread |= other.spread;
        self.other |= other.other;
        self.exported |= other.exported;
        self.read |= other.read;
        self.tested |= other.tested;
        self.library_calls
            .extend(other.library_calls.iter().cloned());
        self.returned_at.extend(other.returned_at.iter().copied());
        self.entry_reads.extend(other.entry_reads.iter().cloned());
    }

    /// Whether a constant object holding `entries` holds them still, for a
    /// name read through one (carrick#1562): nothing writes through it,
    /// hands it on, aliases, spreads (a copy shares its inner objects),
    /// returns or constructs it, or calls a member by a key the source does
    /// not state; and every entry read off it as a value is text or a
    /// builder, never an inner object a holder could change. Reading an
    /// entry, testing it and calling a builder through it keep it.
    fn keeps_entries(&self, entries: &library_sites::NameValue) -> bool {
        !self.written
            && !self.other
            && !self.spread
            && !self.called_computed
            && self.returned_at.is_empty()
            && !self
                .library_calls
                .iter()
                .any(|used| used.form == MakerForm::New)
            && self
                .entry_reads
                .iter()
                .all(|path| !matches!(entries.entry(path), Some(library_sites::NameValue::Map(_))))
    }

    /// Every member called or constructed through the binding.
    fn member_uses(&self) -> impl Iterator<Item = MemberUse> + '_ {
        self.called
            .iter()
            .map(|member| MemberUse {
                form: MakerForm::Call,
                path: Vec::new(),
                member: member.clone(),
            })
            .chain(self.library_calls.iter().cloned())
    }

    /// An object constant never written through, passed to a call or
    /// aliased holds exactly the keys its literal writes. Constructing the
    /// binding itself was an operand use before carrick#1661, a truthiness
    /// test before carrick#1665 and a return before carrick#1562; each counts
    /// as one here.
    fn keeps_object(&self) -> bool {
        !self.written
            && !self.other
            && !self.tested
            && self.returned_at.is_empty()
            && !self.library_calls.contains(&MemberUse {
                form: MakerForm::New,
                path: Vec::new(),
                member: None,
            })
    }
}

/// Every binding's uses over a subtree ([`BindingUse`]). Calls through a
/// binding (`api.get(…)`, `api(…)`) and `export default api` record nothing;
/// a bare reassignment is left to the declaration rules.
#[derive(Default)]
struct BindingUses {
    uses: HashMap<String, BindingUse>,
    /// `import alias = root.a.b` declarations, settled after the walk.
    aliases: Vec<EntityAlias>,
}

/// `import alias = root.a.b` (`export import` when `exported`).
struct EntityAlias {
    alias: String,
    root: String,
    exported: bool,
}

impl BindingUses {
    fn mark(&mut self, key: String, record: impl FnOnce(&mut BindingUse)) {
        record(self.uses.entry(key).or_default());
    }

    /// `member` is read and not called: its chain's root is read.
    fn read_member(&mut self, member: &MemberExpr) {
        if let MemberProp::Computed(key) = &member.prop {
            key.expr.visit_with(self);
        }
        let obj = crate::graphql_document_sites::unwrap_expression(&member.obj);
        if let Some(key) = binding_key(obj) {
            self.mark(key, |used| used.member_read = true);
        } else if let Some(inner) = as_member(obj) {
            self.read_member(inner);
        } else {
            obj.visit_with(self);
        }
    }

    /// `member` is assigned: its chain's root is written through. A field of
    /// `this` assigned itself is the field's own write, which the class's
    /// field table reads.
    fn write_member(&mut self, member: &MemberExpr) {
        if let MemberProp::Computed(key) = &member.prop {
            key.expr.visit_with(self);
        }
        if matches!(&*member.obj, Expr::This(_)) {
            return;
        }
        let obj = crate::graphql_document_sites::unwrap_expression(&member.obj);
        if let Some(key) = binding_key(obj) {
            self.mark(key, |used| used.written = true);
        } else if let Some(inner) = as_member(obj) {
            self.write_member(inner);
        } else {
            obj.visit_with(self);
        }
    }

    /// A call's callee: a binding called, or the direct receiver of the
    /// member called, records nothing; a deeper receiver is read.
    fn callee(&mut self, callee: &Expr) {
        let callee = crate::graphql_document_sites::unwrap_expression(callee);
        if let Some(key) = binding_key(callee) {
            self.mark(key, |used| {
                used.called.insert(None);
            });
            return;
        }
        let Some(member) = as_member(callee) else {
            callee.visit_with(self);
            return;
        };
        if let MemberProp::Computed(key) = &member.prop {
            key.expr.visit_with(self);
        }
        let obj = crate::graphql_document_sites::unwrap_expression(&member.obj);
        if let Some(key) = binding_key(obj) {
            let called = called_member(&member.prop);
            self.mark(key, |used| match called {
                Some(name) => {
                    used.called.insert(Some(name));
                }
                None => used.called_computed = true,
            });
            return;
        }
        // `client.tasks.trigger(…)`: a call through a sub-object, every hop a
        // plain name (carrick#1661).
        if let Some(called) = called_member(&member.prop)
            && let Some((key, path)) = member_path(obj)
        {
            self.mark(key, |used| {
                used.library_calls.insert(MemberUse {
                    form: MakerForm::Call,
                    path,
                    member: Some(called),
                });
            });
            return;
        }
        match as_member(obj) {
            Some(inner) => self.read_member(inner),
            None => obj.visit_with(self),
        }
    }

    /// `new Client(…)`, `new lib.Worker(…)`, `new lib.a.Worker(…)`: the
    /// binding is constructed (carrick#1661). Any other callee is read as
    /// before.
    fn constructed(&mut self, callee: &Expr) {
        let callee = crate::graphql_document_sites::unwrap_expression(callee);
        if let Some(key) = binding_key(callee) {
            self.mark(key, |used| {
                used.library_calls.insert(MemberUse {
                    form: MakerForm::New,
                    path: Vec::new(),
                    member: None,
                });
            });
            return;
        }
        if let Some(member) = as_member(callee)
            && let MemberProp::Ident(name) = &member.prop
        {
            let obj = crate::graphql_document_sites::unwrap_expression(&member.obj);
            let held = binding_key(obj)
                .map(|key| (key, Vec::new()))
                .or_else(|| member_path(obj));
            if let Some((key, path)) = held {
                self.mark(key, |used| {
                    used.library_calls.insert(MemberUse {
                        form: MakerForm::New,
                        path,
                        member: Some(name.sym.to_string()),
                    });
                });
                return;
            }
        }
        callee.visit_with(self);
    }
}

/// The binding a member chain is read off, through any hops, plain,
/// computed or optional.
fn chain_root(expr: &Expr) -> Option<String> {
    let mut at = crate::graphql_document_sites::unwrap_expression(expr);
    loop {
        if let Some(key) = binding_key(at) {
            return Some(key);
        }
        at = crate::graphql_document_sites::unwrap_expression(&as_member(at)?.obj);
    }
}

/// `root.a.b`, with every hop a plain name and `root` a binding
/// ([`binding_key`]): the root's key and the hops in order.
fn member_path(expr: &Expr) -> Option<(String, Vec<String>)> {
    let mut path: Vec<String> = Vec::new();
    let mut at = crate::graphql_document_sites::unwrap_expression(expr);
    loop {
        if let Some(key) = binding_key(at) {
            if path.is_empty() {
                return None;
            }
            path.reverse();
            return Some((key, path));
        }
        let member = as_member(at)?;
        let MemberProp::Ident(name) = &member.prop else {
            return None;
        };
        path.push(name.sym.to_string());
        at = crate::graphql_document_sites::unwrap_expression(&member.obj);
    }
}

impl Visit for BindingUses {
    // A type is no use of a value: a type literal's key (`{ api: T }`) and
    // a type query (`typeof api` in a type) read nothing at run time
    // (carrick#1568).
    fn visit_ts_type(&mut self, _: &TsType) {}

    fn visit_ts_interface_body(&mut self, _: &TsInterfaceBody) {}

    // A class's `implements` and an interface's `extends` name types, which
    // the compiler erases.
    fn visit_ts_expr_with_type_args(&mut self, _: &TsExprWithTypeArgs) {}

    // `import client = lib.api` names `lib` in no expression. Whether that is
    // a use depends on whether `client` is used as a value, which is known
    // only once the whole file is read ([`BindingUses::settle_aliases`]).
    // `import type x = …` binds a type, which no value use can reach.
    fn visit_ts_import_equals_decl(&mut self, decl: &TsImportEqualsDecl) {
        if let TsModuleRef::TsEntityName(entity) = &decl.module_ref {
            let mut root = entity;
            while let TsEntityName::TsQualifiedName(qualified) = root {
                root = &qualified.left;
            }
            if let TsEntityName::Ident(ident) = root {
                self.aliases.push(EntityAlias {
                    alias: decl.id.sym.to_string(),
                    root: ident.sym.to_string(),
                    exported: decl.is_export,
                });
            }
        }
    }

    fn visit_expr(&mut self, expr: &Expr) {
        if let Some(key) = binding_key(expr) {
            self.mark(key, |used| used.other = true);
            return;
        }
        if let Expr::Member(member) = expr {
            // `TOPICS.orders` as a value: an entry read (carrick#1562). One
            // by a key the source does not state may read any entry, the
            // object itself as far as a name is concerned.
            match member_path(expr) {
                Some((key, path)) => self.mark(key, |used| {
                    used.entry_reads.insert(path);
                }),
                None => {
                    if let Some(key) = chain_root(expr) {
                        self.mark(key, |used| {
                            used.entry_reads.insert(Vec::new());
                        });
                    }
                }
            }
            self.read_member(member);
            return;
        }
        expr.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        match &call.callee {
            Callee::Expr(callee) => self.callee(callee),
            other => other.visit_with(self),
        }
        call.args.visit_with(self);
    }

    fn visit_new_expr(&mut self, new: &NewExpr) {
        self.constructed(&new.callee);
        new.args.visit_with(self);
    }

    fn visit_opt_chain_expr(&mut self, chain: &OptChainExpr) {
        match &*chain.base {
            OptChainBase::Call(call) => {
                self.callee(&call.callee);
                call.args.visit_with(self);
            }
            OptChainBase::Member(member) => {
                // Read as the whole object, as a computed read is.
                if let Some(key) = chain_root(&Expr::Member(member.clone())) {
                    self.mark(key, |used| {
                        used.entry_reads.insert(Vec::new());
                    });
                }
                self.read_member(member);
            }
        }
    }

    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        match &assign.left {
            AssignTarget::Simple(SimpleAssignTarget::Ident(_)) => {}
            AssignTarget::Simple(SimpleAssignTarget::Member(member)) => self.write_member(member),
            other => other.visit_with(self),
        }
        // `module.exports = api`, `exports.api = api`: an export.
        if is_commonjs_export(&assign.left)
            && let Some(key) = binding_key(crate::graphql_document_sites::unwrap_expression(
                &assign.right,
            ))
        {
            self.mark(key, |used| used.exported = true);
            return;
        }
        assign.right.visit_with(self);
    }

    fn visit_update_expr(&mut self, update: &UpdateExpr) {
        match as_member(crate::graphql_document_sites::unwrap_expression(
            &update.arg,
        )) {
            Some(member) => self.write_member(member),
            None if binding_key(&update.arg).is_some() => {}
            None => update.arg.visit_with(self),
        }
    }

    fn visit_unary_expr(&mut self, unary: &UnaryExpr) {
        if unary.op == UnaryOp::Delete
            && let Some(member) =
                as_member(crate::graphql_document_sites::unwrap_expression(&unary.arg))
        {
            self.write_member(member);
            return;
        }
        if unary.op == UnaryOp::TypeOf {
            self.compared(&unary.arg);
            return;
        }
        if unary.op == UnaryOp::Bang {
            self.tested(&unary.arg);
            return;
        }
        unary.arg.visit_with(self);
    }

    fn visit_if_stmt(&mut self, stmt: &IfStmt) {
        self.tested(&stmt.test);
        stmt.cons.visit_with(self);
        stmt.alt.visit_with(self);
    }

    fn visit_while_stmt(&mut self, stmt: &WhileStmt) {
        self.tested(&stmt.test);
        stmt.body.visit_with(self);
    }

    fn visit_do_while_stmt(&mut self, stmt: &DoWhileStmt) {
        stmt.body.visit_with(self);
        self.tested(&stmt.test);
    }

    fn visit_for_stmt(&mut self, stmt: &ForStmt) {
        stmt.init.visit_with(self);
        if let Some(test) = &stmt.test {
            self.tested(test);
        }
        stmt.update.visit_with(self);
        stmt.body.visit_with(self);
    }

    fn visit_cond_expr(&mut self, cond: &CondExpr) {
        self.tested(&cond.test);
        cond.cons.visit_with(self);
        cond.alt.visit_with(self);
    }

    /// `e instanceof http.HttpError`, `api === other`: each operand is read
    /// and nothing of it is kept (carrick#1568).
    fn visit_bin_expr(&mut self, bin: &BinExpr) {
        if matches!(
            bin.op,
            BinaryOp::InstanceOf
                | BinaryOp::EqEq
                | BinaryOp::NotEq
                | BinaryOp::EqEqEq
                | BinaryOp::NotEqEq
                | BinaryOp::Lt
                | BinaryOp::LtEq
                | BinaryOp::Gt
                | BinaryOp::GtEq
        ) {
            self.compared(&bin.left);
            self.compared(&bin.right);
            return;
        }
        bin.visit_children_with(self);
    }

    /// `export { api }`, `export { api as client }`: the binding is published,
    /// not used. A re-export from another module names none of this file's.
    fn visit_named_export(&mut self, export: &NamedExport) {
        if export.src.is_some() || export.type_only {
            return;
        }
        for specifier in &export.specifiers {
            if let ExportSpecifier::Named(named) = specifier
                && !named.is_type_only
                && let ModuleExportName::Ident(ident) = &named.orig
            {
                self.mark(ident.sym.to_string(), |used| used.exported = true);
            }
        }
    }

    /// A member in a destructuring target (`({ a: o.x } = src)`) is written.
    fn visit_pat(&mut self, pat: &Pat) {
        if let Pat::Expr(expr) = pat
            && let Some(member) = as_member(crate::graphql_document_sites::unwrap_expression(expr))
        {
            self.write_member(member);
            return;
        }
        pat.visit_children_with(self);
    }

    fn visit_object_lit(&mut self, object: &ObjectLit) {
        for prop in &object.props {
            match prop {
                PropOrSpread::Spread(spread) => self.spread(&spread.expr),
                PropOrSpread::Prop(prop) => prop.visit_with(self),
            }
        }
    }

    fn visit_array_lit(&mut self, array: &ArrayLit) {
        for element in array.elems.iter().flatten() {
            match element.spread {
                Some(_) => self.spread(&element.expr),
                None => element.expr.visit_with(self),
            }
        }
    }

    fn visit_prop(&mut self, prop: &Prop) {
        if let Prop::Shorthand(ident) = prop {
            self.mark(ident.sym.to_string(), |used| used.other = true);
            return;
        }
        prop.visit_children_with(self);
    }

    fn visit_export_default_expr(&mut self, export: &ExportDefaultExpr) {
        match binding_key(crate::graphql_document_sites::unwrap_expression(
            &export.expr,
        )) {
            Some(key) => self.mark(key, |used| used.exported = true),
            None => export.expr.visit_with(self),
        }
    }

    /// `return client`: the binding is returned ([`BindingUse::returned_at`]).
    fn visit_return_stmt(&mut self, ret: &ReturnStmt) {
        let returned = ret
            .arg
            .as_deref()
            .and_then(|arg| binding_key(crate::graphql_document_sites::unwrap_expression(arg)));
        match returned {
            Some(key) => self.mark(key, |used| {
                used.returned_at.insert(ret.span.lo.0);
            }),
            None => ret.visit_children_with(self),
        }
    }

    /// `() => client`: an arrow whose expression body is a binding returns
    /// it, as `return client` does.
    fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
        let BlockStmtOrExpr::Expr(body) = &*arrow.body else {
            arrow.visit_children_with(self);
            return;
        };
        match binding_key(crate::graphql_document_sites::unwrap_expression(body)) {
            Some(key) => {
                arrow.params.visit_with(self);
                self.mark(key, |used| {
                    used.returned_at.insert(body.span().lo.0);
                });
            }
            None => arrow.visit_children_with(self),
        }
    }
}

impl BindingUses {
    /// An `import alias = root.a.b` whose alias is exported or used as a value
    /// (`uses`, or a JSX tag in `jsx`) hands `root` on: what it reaches may be
    /// changed through the alias (carrick#1568). One used only as a type is
    /// erased, and names nothing. Settled to a fixed point, so an alias of an
    /// alias reaches the first root.
    fn settle_aliases(&mut self, jsx: &HashSet<String>) {
        let mut settled: HashSet<usize> = HashSet::new();
        loop {
            let ready: Vec<usize> = self
                .aliases
                .iter()
                .enumerate()
                .filter(|(index, alias)| {
                    !settled.contains(index)
                        && (alias.exported
                            || self.uses.contains_key(&alias.alias)
                            || jsx.contains(&alias.alias))
                })
                .map(|(index, _)| index)
                .collect();
            if ready.is_empty() {
                return;
            }
            for index in ready {
                settled.insert(index);
                let root = self.aliases[index].root.clone();
                self.mark(root, |used| used.other = true);
            }
        }
    }

    fn spread(&mut self, expr: &Expr) {
        match binding_key(crate::graphql_document_sites::unwrap_expression(expr)) {
            Some(key) => self.mark(key, |used| used.spread = true),
            None => expr.visit_with(self),
        }
    }

    /// A test for truth (carrick#1665): a binding that is the whole test is
    /// [`BindingUse::tested`]; anything else is visited as usual.
    fn tested(&mut self, expr: &Expr) {
        match binding_key(crate::graphql_document_sites::unwrap_expression(expr)) {
            Some(key) => self.mark(key, |used| used.tested = true),
            None => expr.visit_with(self),
        }
    }

    /// An operand whose value is read and not kept: a binding, or a member
    /// chain rooted at one (`(http as any).HttpError`), records no use of
    /// the binding. A computed key in the chain is still visited, and any
    /// other operand is visited as usual.
    fn compared(&mut self, expr: &Expr) {
        let expr = crate::graphql_document_sites::unwrap_expression(expr);
        if let Some(key) = binding_key(expr) {
            self.mark(key, |used| used.read = true);
            return;
        }
        let Some(mut member) = as_member(expr) else {
            expr.visit_with(self);
            return;
        };
        loop {
            if let MemberProp::Computed(key) = &member.prop {
                key.expr.visit_with(self);
            }
            let obj = crate::graphql_document_sites::unwrap_expression(&member.obj);
            if let Some(key) = binding_key(obj) {
                self.mark(key, |used| used.read = true);
                return;
            }
            match as_member(obj) {
                Some(inner) => member = inner,
                None => {
                    obj.visit_with(self);
                    return;
                }
            }
        }
    }
}

/// The key a binding is tracked by: an identifier's name, or `this.<field>`.
fn binding_key(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(ident) => Some(ident.sym.to_string()),
        Expr::Member(member) if matches!(&*member.obj, Expr::This(_)) => match &member.prop {
            MemberProp::Ident(ident) => Some(format!("this.{}", ident.sym)),
            MemberProp::PrivateName(private) => Some(format!("this.#{}", private.name)),
            MemberProp::Computed(_) => None,
        },
        _ => None,
    }
}

/// The member a call names: an identifier, a private name, or a string key.
fn called_member(prop: &MemberProp) -> Option<String> {
    match prop {
        MemberProp::Ident(ident) => Some(ident.sym.to_string()),
        MemberProp::PrivateName(private) => Some(format!("#{}", private.name)),
        MemberProp::Computed(computed) => match &*computed.expr {
            Expr::Lit(Lit::Str(key)) => Some(key.value.to_string()),
            _ => None,
        },
    }
}

/// A member access, plain or optional (`a.b`, `a?.b`).
fn as_member(expr: &Expr) -> Option<&MemberExpr> {
    match expr {
        Expr::Member(member) => Some(member),
        Expr::OptChain(chain) => match &*chain.base {
            OptChainBase::Member(member) => Some(member),
            OptChainBase::Call(_) => None,
        },
        _ => None,
    }
}

/// `module.exports`, or a property of it or of `exports`.
fn is_commonjs_export(target: &AssignTarget) -> bool {
    let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = target else {
        return false;
    };
    let is_module_exports = |expr: &Expr| {
        matches!(expr, Expr::Member(inner)
            if matches!(&*inner.obj, Expr::Ident(obj) if obj.sym == *"module")
                && member_prop(inner).as_deref() == Some("exports"))
    };
    (matches!(&*member.obj, Expr::Ident(obj) if obj.sym == *"module")
        && member_prop(member).as_deref() == Some("exports"))
        || is_module_exports(&member.obj)
        || matches!(&*member.obj, Expr::Ident(obj) if obj.sym == *"exports")
}

/// Every name declared exactly once in the module, by `const <name> = { … }`.
fn object_consts(module: &Module) -> HashSet<String> {
    #[derive(Default)]
    struct ObjectConsts {
        found: HashSet<String>,
    }
    impl Visit for ObjectConsts {
        fn visit_var_decl(&mut self, decl: &VarDecl) {
            if decl.kind == VarDeclKind::Const {
                for declarator in &decl.decls {
                    if let (Pat::Ident(ident), Some(init)) = (&declarator.name, &declarator.init)
                        && matches!(
                            crate::graphql_document_sites::unwrap_expression(init),
                            Expr::Object(_)
                        )
                    {
                        self.found.insert(ident.id.sym.to_string());
                    }
                }
            }
            decl.visit_children_with(self);
        }
    }
    let mut consts = ObjectConsts::default();
    module.visit_with(&mut consts);
    let declared = Declarations::of(module);
    consts
        .found
        .into_iter()
        .filter(|name| declared.name_count(name) == 1)
        .collect()
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
    /// The class's span start: what a message-role field site names its
    /// class by ([`FileIr::field_receivers`]).
    class: u32,
    values: HashMap<String, Value>,
    receivers: HashMap<String, ClientRef>,
    /// Fields that hold the platform's `fetch` unless the class is handed
    /// another (carrick#1562): set once, where the field table reads a
    /// write, to the global or to a constructor parameter whose default it
    /// is ([`platform_fetch`]).
    fetches: HashSet<String>,
}

/// What a function written inside another can read of it, by binding.
#[derive(Default)]
struct Captured {
    values: HashMap<BindingKey, Value>,
    /// Literal text only: a hole is a parameter of the enclosing function,
    /// which the function written inside it is not called with.
    texts: HashMap<BindingKey, Vec<library_sites::TextPiece>>,
    receivers: HashMap<BindingKey, ClientRef>,
    /// [`Scope::fetches`].
    fetches: HashSet<BindingKey>,
}

/// One module-scope binding an import introduces.
#[derive(Debug, Clone)]
struct ImportBinding {
    /// The specifier, exactly as written.
    specifier: String,
    /// `"default"` or the named export the binding was imported as.
    export: String,
    /// `import * as ns`: the binding names the module, not an export.
    namespace: bool,
    /// Bound by `let` or `var`, so the file may assign it something else:
    /// its uses count against what it was bound to, and no call through it is
    /// read as that (carrick#1568).
    reassignable: bool,
}

/// Every module-scope binding an import introduces, with the export it names:
/// `import x from "m"` and `const x = require("m")` name `default`; `import {
/// a as x } from "m"`, `const { a: x } = require("m")` and `const x =
/// require("m").a` name `a`; `import * as x` names the module. A `require`
/// bound by any declaration kind counts, exported or not, as it does for the
/// call graph ([`crate::commonjs::require_bindings`]).
///
/// Also returns where each `require` call these bindings come from starts, so
/// the module loads a file makes elsewhere ([`module_loads`]) leave them out.
fn import_bindings(module: &Module) -> (HashMap<String, ImportBinding>, HashSet<u32>) {
    let mut bindings = HashMap::new();
    let mut require_calls = HashSet::new();
    let mut add =
        |local: String, specifier: String, export: String, namespace: bool, reassignable: bool| {
            bindings.insert(
                local,
                ImportBinding {
                    specifier,
                    export,
                    namespace,
                    reassignable,
                },
            );
        };
    for item in &module.body {
        match item {
            ModuleItem::ModuleDecl(ModuleDecl::Import(import)) if !import.type_only => {
                let specifier = import.src.value.to_string();
                for import_specifier in &import.specifiers {
                    match import_specifier {
                        ImportSpecifier::Default(default) => {
                            add(
                                default.local.sym.to_string(),
                                specifier.clone(),
                                DEFAULT_EXPORT.to_string(),
                                false,
                                false,
                            );
                        }
                        ImportSpecifier::Named(named) if !named.is_type_only => {
                            let export = match &named.imported {
                                Some(ModuleExportName::Ident(ident)) => ident.sym.to_string(),
                                Some(ModuleExportName::Str(name)) => name.value.to_string(),
                                None => named.local.sym.to_string(),
                            };
                            add(
                                named.local.sym.to_string(),
                                specifier.clone(),
                                export,
                                false,
                                false,
                            );
                        }
                        ImportSpecifier::Namespace(namespace) => {
                            add(
                                namespace.local.sym.to_string(),
                                specifier.clone(),
                                DEFAULT_EXPORT.to_string(),
                                true,
                                false,
                            );
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
                        DEFAULT_EXPORT.to_string(),
                        false,
                        false,
                    );
                }
            }
            _ => {
                let Some(var) = module_var_decl(item) else {
                    continue;
                };
                let reassignable = var.kind != VarDeclKind::Const;
                for declarator in &var.decls {
                    let Some(init) = declarator.init.as_deref() else {
                        continue;
                    };
                    if let Expr::Member(member) = init
                        && let Some(specifier) = require_specifier(&member.obj)
                        && let MemberProp::Ident(property) = &member.prop
                        && let Pat::Ident(local) = &declarator.name
                    {
                        require_calls.insert(member.obj.span().lo.0);
                        add(
                            local.id.sym.to_string(),
                            specifier,
                            property.sym.to_string(),
                            false,
                            reassignable,
                        );
                        continue;
                    }
                    let Some(specifier) = require_specifier(init) else {
                        continue;
                    };
                    require_calls.insert(init.span().lo.0);
                    for bound in require_bound_names(&declarator.name) {
                        let export = match bound.kind {
                            SymbolKind::Namespace => DEFAULT_EXPORT.to_string(),
                            SymbolKind::Named | SymbolKind::Default => bound.imported,
                        };
                        add(
                            bound.local.id.sym.to_string(),
                            specifier.clone(),
                            export,
                            false,
                            reassignable,
                        );
                    }
                }
            }
        }
    }
    (bindings, require_calls)
}

/// The modules a file loads other than through a module-scope import
/// binding, by specifier (carrick#1568): `import("./m")` and `require("./m")`
/// anywhere, and any other call handed a relative specifier as its first
/// argument (a `require` made by a factory, `req("./m")`). What such a load
/// does with the module is not followed, so everything the module publishes
/// counts as used in every way.
///
/// A specifier the source computes (`import(name)`, a template with a hole)
/// names no module and is not here.
fn module_loads(module: &Module, bound_requires: &HashSet<u32>) -> BTreeSet<String> {
    struct Loads<'a> {
        bound_requires: &'a HashSet<u32>,
        found: BTreeSet<String>,
    }
    impl Visit for Loads<'_> {
        fn visit_call_expr(&mut self, call: &CallExpr) {
            let specifier = call
                .args
                .first()
                .filter(|arg| arg.spread.is_none())
                .and_then(|arg| literal_specifier(&arg.expr));
            if let Some(specifier) = specifier {
                let require = matches!(&call.callee, Callee::Expr(callee)
                    if matches!(&**callee, Expr::Ident(ident) if ident.sym == *"require"));
                let bound = require && self.bound_requires.contains(&call.span.lo.0);
                let relative = specifier.starts_with("./") || specifier.starts_with("../");
                if !bound && (matches!(call.callee, Callee::Import(_)) || require || relative) {
                    self.found.insert(specifier);
                }
            }
            call.visit_children_with(self);
        }
    }
    let mut loads = Loads {
        bound_requires,
        found: BTreeSet::new(),
    };
    module.visit_with(&mut loads);
    loads.found
}

/// Every specifier `module` loads a module by at run time (carrick#1568): a
/// static import, a side-effect import, a re-export (`export { x } from`,
/// `export * from`, `export * as ns from`), `import x = require()`, and
/// `import()` or `require` with a literal specifier anywhere.
///
/// Only what TypeScript erases is left out: an import none of whose bindings
/// is used as a value (`used`), which the compiler drops (`import type` and
/// `{ type x }` among them), and `export type`, or a re-export whose every
/// specifier is `type`.
fn value_specifiers(module: &Module, used: impl Fn(&str) -> bool) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for item in &module.body {
        let ModuleItem::ModuleDecl(decl) = item else {
            continue;
        };
        match decl {
            // A binding `import type` or `{ type x }` brings can only be used
            // as a type, so the one rule covers it.
            ModuleDecl::Import(import) => {
                let kept = import.specifiers.is_empty()
                    || import.specifiers.iter().any(|specifier| match specifier {
                        ImportSpecifier::Named(named) => used(named.local.sym.as_ref()),
                        ImportSpecifier::Default(default) => used(default.local.sym.as_ref()),
                        ImportSpecifier::Namespace(namespace) => {
                            used(namespace.local.sym.as_ref())
                        }
                    });
                if kept {
                    found.insert(import.src.value.to_string());
                }
            }
            ModuleDecl::ExportAll(export) if !export.type_only => {
                found.insert(export.src.value.to_string());
            }
            ModuleDecl::ExportNamed(export) if !export.type_only => {
                if let Some(src) = &export.src
                    && (export.specifiers.is_empty()
                        || export.specifiers.iter().any(|specifier| {
                            !matches!(specifier, ExportSpecifier::Named(named) if named.is_type_only)
                        }))
                {
                    found.insert(src.value.to_string());
                }
            }
            ModuleDecl::TsImportEquals(decl) if !decl.is_type_only => {
                if let TsModuleRef::TsExternalModuleRef(external) = &decl.module_ref
                    && used(decl.id.sym.as_ref())
                {
                    found.insert(external.expr.value.to_string());
                }
            }
            _ => {}
        }
    }
    struct Loads {
        found: BTreeSet<String>,
    }
    impl Visit for Loads {
        fn visit_call_expr(&mut self, call: &CallExpr) {
            let loads = match &call.callee {
                Callee::Import(_) => true,
                Callee::Expr(callee) => {
                    matches!(&**callee, Expr::Ident(ident) if ident.sym == *"require")
                }
                Callee::Super(_) => false,
            };
            if loads
                && let Some(specifier) = call
                    .args
                    .first()
                    .filter(|arg| arg.spread.is_none())
                    .and_then(|arg| literal_specifier(&arg.expr))
            {
                self.found.insert(specifier);
            }
            call.visit_children_with(self);
        }
    }
    let mut loads = Loads { found };
    module.visit_with(&mut loads);
    loads.found
}

/// Every name a JSX element uses as its tag (`<Button />`, the `ui` of
/// `<ui.Button />`): a use of the binding as a value that no expression
/// states.
fn jsx_names(module: &Module) -> HashSet<String> {
    #[derive(Default)]
    struct Names {
        found: HashSet<String>,
    }
    impl Visit for Names {
        fn visit_jsx_element_name(&mut self, name: &JSXElementName) {
            match name {
                JSXElementName::Ident(ident) => {
                    self.found.insert(ident.sym.to_string());
                }
                JSXElementName::JSXMemberExpr(member) => {
                    let mut object = &member.obj;
                    while let JSXObject::JSXMemberExpr(inner) = object {
                        object = &inner.obj;
                    }
                    if let JSXObject::Ident(ident) = object {
                        self.found.insert(ident.sym.to_string());
                    }
                }
                JSXElementName::JSXNamespacedName(_) => {}
            }
        }
    }
    let mut names = Names::default();
    module.visit_with(&mut names);
    names.found
}

/// A string literal, or a template with no hole in it.
fn literal_specifier(expr: &Expr) -> Option<String> {
    match crate::graphql_document_sites::unwrap_expression(expr) {
        Expr::Lit(Lit::Str(literal)) => Some(literal.value.to_string()),
        Expr::Tpl(tpl) if tpl.exprs.is_empty() => tpl.quasis.first().map(|quasi| {
            quasi
                .cooked
                .as_ref()
                .map(|cooked| cooked.to_string())
                .unwrap_or_else(|| quasi.raw.to_string())
        }),
        _ => None,
    }
}

/// The imports that name a package's export (carrick#1564). A namespace import
/// names the module rather than an export, and a relative specifier names no
/// package, so neither is a client of one.
fn import_receivers(imports: &HashMap<String, ImportBinding>) -> HashMap<String, ClientRef> {
    imports
        .iter()
        .filter(|(_, import)| {
            !import.namespace
                && !import.specifier.starts_with('.')
                && !import.specifier.starts_with('/')
        })
        .map(|(local, import)| {
            (
                local.clone(),
                ClientRef {
                    package: import.specifier.clone(),
                    export: import.export.clone(),
                    instance: None,
                    contested: false,
                    called: BTreeSet::new(),
                    called_computed: false,
                    contested_message: false,
                    member_uses: BTreeSet::new(),
                    export_uses: BTreeSet::new(),
                    returned: BTreeSet::new(),
                },
            )
        })
        .collect()
}

/// Whether the file names `module.exports` or `exports` anywhere.
fn names_commonjs_exports(module: &Module) -> bool {
    #[derive(Default)]
    struct Names {
        found: bool,
    }
    impl Visit for Names {
        fn visit_expr(&mut self, expr: &Expr) {
            match expr {
                Expr::Ident(ident) if ident.sym == *"exports" => self.found = true,
                Expr::Member(member)
                    if matches!(&*member.obj, Expr::Ident(obj) if obj.sym == *"module")
                        && member_prop(member).as_deref() == Some("exports") =>
                {
                    self.found = true;
                }
                _ => expr.visit_children_with(self),
            }
        }
    }
    let mut names = Names::default();
    module.visit_with(&mut names);
    names.found
}

/// Every declaration below module scope: in a function (its parameters
/// included), a class, or a block.
fn redeclared_names(module: &Module) -> Declarations {
    let mut names = Declarations::default();
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
    names
}

/// What an expression can read: the function's parameters and constants, the
/// enclosing class's fields, and the module's constants, each by the binding
/// an identifier resolves to (carrick#1648).
struct Scope<'a> {
    params: Vec<Option<BindingKey>>,
    locals: HashMap<BindingKey, Value>,
    /// The locals whose initialiser is text and that nothing assigns again
    /// ([`library_sites::text_pieces`], carrick#1661), the enclosing
    /// function's included. A local of this function may hold one of its
    /// parameters (carrick#1562); one of the enclosing function's never
    /// does.
    texts: HashMap<BindingKey, Vec<library_sites::TextPiece>>,
    /// Locals holding a library client's instance (carrick#1564).
    local_receivers: HashMap<BindingKey, ClientRef>,
    /// `let` locals the body sets, on every path to each `return` of them,
    /// to an instance of one of these makers (carrick#1689,
    /// [`Reader::let_makers`]). Read only where they are returned: no call
    /// is read through one, for any role.
    let_makers: HashMap<BindingKey, Vec<ClientRef>>,
    /// Parameters, the enclosing function's included, that hold the
    /// platform's `fetch` unless a caller hands another (`fetchImpl =
    /// fetch`, `{ fetchImpl = fetch } = {}`), and that the body never
    /// assigns again: a call through one is a `fetch` (carrick#1562).
    fetches: HashSet<BindingKey>,
    fields: Option<&'a ClassFields>,
    module: &'a ModuleScope,
}

impl<'a> Scope<'a> {
    fn module(module: &'a ModuleScope) -> Self {
        Self {
            params: Vec::new(),
            locals: HashMap::new(),
            texts: HashMap::new(),
            local_receivers: HashMap::new(),
            let_makers: HashMap::new(),
            fetches: HashSet::new(),
            fields: None,
            module,
        }
    }

    /// The position of the parameter `ident` names, when it names one.
    fn param_index(&self, ident: &Ident) -> Option<usize> {
        let key = ident_key(ident);
        self.params.iter().position(|p| p.as_ref() == Some(&key))
    }

    /// The library client `ident` holds here, if it holds one. A parameter or
    /// a local of the same name is not the module's binding.
    fn receiver(&self, ident: &Ident) -> Option<&ClientRef> {
        if self.param_index(ident).is_some() {
            return None;
        }
        let key = ident_key(ident);
        if let Some(client) = self.local_receivers.get(&key) {
            return Some(client);
        }
        if self.locals.contains_key(&key) {
            return None;
        }
        self.module.receivers.get(ident.sym.as_ref())
    }

    /// The binding a call through `ident` names as its client, by the same
    /// scope rules as [`receiver`](Self::receiver), with an import of another
    /// module read as whatever that module declares (carrick#1568).
    fn call_binding(&self, ident: &Ident) -> Option<ClientBinding> {
        if self.param_index(ident).is_some() {
            return None;
        }
        let key = ident_key(ident);
        if let Some(client) = self.local_receivers.get(&key) {
            return Some(ClientBinding::Own(client.clone()));
        }
        if self.locals.contains_key(&key) {
            return None;
        }
        let name = ident.sym.as_ref();
        match self.module.receivers.get(name) {
            Some(client) if client.instance.is_some() => {
                Some(ClientBinding::Module(name.to_string()))
            }
            package => self
                .module
                .imports
                .contains(name)
                .then(|| ClientBinding::Imported {
                    local: name.to_string(),
                    package: package.cloned(),
                }),
        }
    }
}

struct Reader<'a> {
    source_map: &'a Lrc<SourceMap>,
    /// How many builder calls deep [`Reader::eval`] is reading
    /// (carrick#1562): a builder that calls itself stops at the cap.
    builder_depth: std::cell::Cell<usize>,
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
            hi: span.hi.0,
            span_start: local + SWC_SPAN_BASE,
            span_end: local_hi + SWC_SPAN_BASE,
            line: self.source_map.lookup_char_pos(span.lo).line as u32,
        }
    }

    /// A class: its field table, then each member keyed as the extractor
    /// keys it.
    fn class(
        &self,
        ident: &Ident,
        class: &Class,
        module: &ModuleScope,
        definition_keys: &HashSet<String>,
        file: &mut FileIr,
    ) {
        let name = ident.sym.as_ref();
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
        // Every maker write to a field, for the message-role field rule
        // (carrick#1665): the initialisers' and the constructor's here, each
        // member's as it is read.
        let mut field_writes = self.initialiser_and_constructor_writes(class, &fields, module);
        for member in &class.body {
            match member {
                ClassMember::Method(method) if !matches!(method.kind, MethodKind::Setter) => {
                    let Some(member_name) = prop_name(&method.key) else {
                        continue;
                    };
                    let mut ir =
                        self.function(&method.function, table(method.is_static), module, &none);
                    library_sites::take_field_writes(&mut ir, &mut field_writes);
                    file.functions
                        .insert(key(&member_name, method.is_static), ir);
                }
                ClassMember::PrivateMethod(method)
                    if !matches!(method.kind, MethodKind::Setter) =>
                {
                    let member_name = format!("#{}", method.key.name);
                    let mut ir =
                        self.function(&method.function, table(method.is_static), module, &none);
                    library_sites::take_field_writes(&mut ir, &mut field_writes);
                    file.functions
                        .insert(key(&member_name, method.is_static), ir);
                }
                ClassMember::ClassProp(prop) => {
                    let (Some(member_name), Some(init)) = (prop_name(&prop.key), &prop.value)
                    else {
                        continue;
                    };
                    let mut ir = match &**init {
                        Expr::Arrow(arrow) => {
                            self.arrow(arrow, table(prop.is_static), module, &none)
                        }
                        Expr::Fn(fn_expr) => {
                            self.function(&fn_expr.function, table(prop.is_static), module, &none)
                        }
                        _ => continue,
                    };
                    library_sites::take_field_writes(&mut ir, &mut field_writes);
                    file.functions.insert(key(&member_name, prop.is_static), ir);
                }
                _ => {}
            }
        }
        let receivers =
            library_sites::field_receivers(&ident_key(ident), &module.class_this, field_writes);
        for (field, client) in receivers {
            file.field_receivers.insert((fields.class, field), client);
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
        // Fields set to the platform's `fetch`, or to a constructor parameter
        // whose default it is (carrick#1562).
        let mut fetches: HashSet<String> = HashSet::new();
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
                        if platform_fetch(init, module) {
                            fetches.insert(name.clone());
                        }
                        let scope = Scope::module(module);
                        let value = self.eval(init, &scope);
                        let client = self.factory_call(init, &scope);
                        assign(name, value, client, &mut fields);
                    }
                }
                ClassMember::PrivateProp(prop) if !prop.is_static => {
                    if let Some(init) = &prop.value {
                        let name = format!("#{}", prop.key.name);
                        if platform_fetch(init, module) {
                            fetches.insert(name.clone());
                        }
                        let scope = Scope::module(module);
                        let value = self.eval(init, &scope);
                        let client = self.factory_call(init, &scope);
                        assign(name, value, client, &mut fields);
                    }
                }
                ClassMember::Constructor(ctor) => {
                    let mut scope = Scope::module(module);
                    let mut fetch_params: HashSet<BindingKey> = HashSet::new();
                    for param in &ctor.params {
                        match param {
                            ParamOrTsParamProp::TsParamProp(prop) => match &prop.param {
                                TsParamPropParam::Ident(ident) => {
                                    let name = ident.id.sym.to_string();
                                    assign(name.clone(), Value::opaque(name), None, &mut fields);
                                }
                                // `private fetchImpl = fetch`.
                                TsParamPropParam::Assign(param) => {
                                    if let Pat::Ident(ident) = &*param.left
                                        && platform_fetch(&param.right, module)
                                    {
                                        fetches.insert(ident.id.sym.to_string());
                                    }
                                }
                            },
                            ParamOrTsParamProp::Param(param) => {
                                fetch_bindings(&param.pat, module, &mut fetch_params);
                            }
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
                                    scope.locals.insert(ident_key(&ident.id), value);
                                }
                            }
                        }
                        match this_assignment(stmt) {
                            Some((name, value)) => {
                                value.visit_with(&mut nested);
                                let handed = matches!(
                                    crate::graphql_document_sites::unwrap_expression(value),
                                    Expr::Ident(ident) if fetch_params.contains(&ident_key(ident))
                                );
                                if handed || platform_fetch(value, module) {
                                    fetches.insert(name.clone());
                                }
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
        fetches.retain(|name| !contested.contains(name));
        for name in contested {
            receivers.remove(&name);
            fields.insert(name.clone(), Value::opaque(format!("this.{name}")));
        }
        let receivers = receivers
            .into_iter()
            .map(|(name, client)| {
                let client = module.with_uses(client, &format!("this.{name}"));
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
        ClassFields {
            class: class.span.lo.0,
            values,
            receivers,
            fetches,
        }
    }

    fn function(
        &self,
        function: &Function,
        fields: Option<&ClassFields>,
        module: &ModuleScope,
        captured: &Captured,
    ) -> FnIr {
        let params = function.params.iter().map(|p| pat_key(&p.pat)).collect();
        let mut fetches = captured.fetches.clone();
        for param in &function.params {
            fetch_bindings(&param.pat, module, &mut fetches);
        }
        let mut scope = Scope {
            params,
            locals: captured.values.clone(),
            texts: captured.texts.clone(),
            local_receivers: inherited_receivers(captured, function),
            let_makers: HashMap::new(),
            fetches,
            fields,
            module,
        };
        let mut ir = FnIr::default();
        match &function.body {
            Some(body) => self.body(&body.stmts, &mut scope, &mut ir),
            None => ir.bodyless = true,
        }
        ir.settle_returned(function.is_async, function.is_generator);
        ir
    }

    fn arrow(
        &self,
        arrow: &ArrowExpr,
        fields: Option<&ClassFields>,
        module: &ModuleScope,
        captured: &Captured,
    ) -> FnIr {
        let params = arrow.params.iter().map(pat_key).collect();
        let mut fetches = captured.fetches.clone();
        for param in &arrow.params {
            fetch_bindings(param, module, &mut fetches);
        }
        let mut scope = Scope {
            params,
            locals: captured.values.clone(),
            texts: captured.texts.clone(),
            local_receivers: inherited_receivers(captured, arrow),
            let_makers: HashMap::new(),
            fetches,
            fields,
            module,
        };
        let mut ir = FnIr::default();
        match &*arrow.body {
            BlockStmtOrExpr::BlockStmt(block) => self.body(&block.stmts, &mut scope, &mut ir),
            // The expression body is what the arrow returns, where it starts
            // ([`BindingUses::visit_arrow_expr`] keys it the same way).
            BlockStmtOrExpr::Expr(expr) => {
                let returned = self.returned_instances(expr, &scope);
                ir.returns.push((expr.span().lo.0, returned));
                self.walk(expr, &scope, &mut ir);
            }
        }
        ir.settle_returned(arrow.is_async, arrow.is_generator);
        ir
    }

    /// A function body, statement by statement, so a constant is readable
    /// by the statements after it. A binding assigned again anywhere in the
    /// body is opaque everywhere, and so is one the body declares twice (a
    /// `var` written again inside a block is the same binding, carrick#1648).
    ///
    /// A constant built by a library client's factory call holds that
    /// client's instance (carrick#1564), unless the body declares its name a
    /// second time somewhere, where nothing here says which one a use means.
    fn body(&self, stmts: &[Stmt], scope: &mut Scope<'_>, ir: &mut FnIr) {
        let mut reassigned = Reassigned::default();
        let mut declared = Declarations::default();
        for stmt in stmts {
            stmt.visit_with(&mut reassigned);
            stmt.visit_with(&mut declared);
        }
        // A parameter the body assigns again may hold anything.
        scope
            .fetches
            .retain(|(name, _)| !reassigned.names.contains(name));
        for (index, stmt) in stmts.iter().enumerate() {
            if let Stmt::Decl(Decl::Var(var)) = stmt {
                for declarator in &var.decls {
                    if let Some(init) = &declarator.init {
                        self.walk(init, scope, ir);
                    }
                    // A `let` set again on every path to one of a few makers'
                    // instances, read where it is returned (carrick#1689).
                    // One never set again is a local receiver, below. Its
                    // uses are kept by name, so it is read only where the
                    // body declares the name once; a `var` declared twice
                    // is set by each declaration too, and a destructuring or
                    // a `for (x of …)` head declares it again.
                    if let Pat::Ident(ident) = &declarator.name
                        && reassigned.names.contains(ident.id.sym.as_ref())
                        && reassigned.declared_once(&declared, ident.id.sym.as_ref())
                        && let Some(makers) = self.let_makers(
                            stmts,
                            &stmts[index + 1..],
                            &ident.id,
                            declarator.init.as_deref(),
                            scope,
                        )
                    {
                        let name = ident.id.sym.as_ref();
                        let makers = makers
                            .into_iter()
                            .map(|client| scope.module.with_uses(client, name))
                            .collect();
                        scope.let_makers.insert(ident_key(&ident.id), makers);
                    }
                    if let (Pat::Ident(ident), Some(init)) = (&declarator.name, &declarator.init) {
                        let name = ident.id.sym.to_string();
                        let key = ident_key(&ident.id);
                        let unsettled =
                            reassigned.names.contains(&name) || declared.binding_count(&key) > 1;
                        // A package maker's instance, or what an own call
                        // returns (carrick#1562): an HTTP reading reads only
                        // the first ([`ClientInstance::http_factory`]).
                        let client = if unsettled {
                            None
                        } else {
                            self.written_instance(init, scope)
                        };
                        let value = if unsettled {
                            Value::opaque(name.clone())
                        } else {
                            bound(name.clone(), self.eval(init, scope))
                        };
                        if !unsettled && let Some(pieces) = library_sites::text_pieces(init, scope)
                        {
                            scope.texts.insert(key.clone(), pieces);
                        }
                        scope.locals.insert(key.clone(), value);
                        scope.local_receivers.remove(&key);
                        if let Some(client) = client
                            && declared.name_count(&name) == 1
                        {
                            let client = scope.module.with_uses(client, &name);
                            scope.local_receivers.insert(key, client);
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
            // The binding the identifier resolves to, never another of the
            // same name (carrick#1648): a block that declares the name again
            // holds a binding this pass does not read, so it is opaque.
            Expr::Ident(ident) => {
                let name = ident.sym.as_ref();
                if let Some(index) = scope.param_index(ident) {
                    return Value::Str(vec![Piece::Param(index, name.to_string())]);
                }
                let key = ident_key(ident);
                if let Some(value) = scope
                    .locals
                    .get(&key)
                    .or_else(|| scope.module.consts.get(&key))
                {
                    // An object the file writes through holds keys its
                    // literal does not state (carrick#1564, third review).
                    if matches!(value, Value::Obj(_)) && scope.module.written_through(name) {
                        return Value::Obj(ObjValue {
                            fields: BTreeMap::new(),
                            open: true,
                        });
                    }
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
                // `usersPath(id)`, `ENDPOINTS.users.byId(id)`: what the
                // builder returns, with the call's arguments in its
                // parameters (carrick#1562).
                if let Some(value) = self.builder_call(call, scope) {
                    return value;
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

    /// A call to a module-scope builder ([`ModuleScope::builders`]), read
    /// as what it returns with each parameter holding what the call passes
    /// (carrick#1562). The callee is the builder's binding, read by its
    /// scope, or a path of plain entries into a constant object. A spread
    /// argument moves every position, so such a call reads as no builder's;
    /// a missing argument holds nothing the source states.
    fn builder_call(&self, call: &CallExpr, scope: &Scope<'_>) -> Option<Value> {
        const MAX_BUILDER_DEPTH: usize = 8;
        let Callee::Expr(callee) = &call.callee else {
            return None;
        };
        let (root, path) = match crate::graphql_document_sites::unwrap_expression(callee) {
            Expr::Ident(ident) => (ident, Vec::new()),
            Expr::Member(member) => library_sites::named_path(member)?,
            _ => return None,
        };
        if scope.param_index(root).is_some() {
            return None;
        }
        let builder = scope.module.builders.get(&(ident_key(root), path))?;
        let depth = self.builder_depth.get();
        if depth >= MAX_BUILDER_DEPTH || call.args.iter().any(|arg| arg.spread.is_some()) {
            return None;
        }
        let mut inner = Scope::module(scope.module);
        for (index, param) in builder.params.iter().enumerate() {
            let Some(param) = param else {
                continue;
            };
            let value = match call.args.get(index) {
                Some(arg) => self.eval(&arg.expr, scope),
                None => Value::Str(vec![Piece::Unknown]),
            };
            inner.locals.insert(param.clone(), value);
        }
        self.builder_depth.set(depth + 1);
        let value = self.eval(&builder.returned, &inner);
        self.builder_depth.set(depth);
        Some(value)
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
    /// choice, `cond && {…}`, or a constant [`ModuleScope::spreadable`] says
    /// holds one. `None` for anything
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
            // Only a constant holding one object literal that nothing else
            // can reach (carrick#1564 re-review, R2). Any other value may
            // hold keys the source does not state here.
            Expr::Ident(ident) if scope.module.spreadable(ident.sym.as_ref()) => {
                match self.eval(expr, scope) {
                    Value::Obj(value) => known(value),
                    _ => None,
                }
            }
            _ => None,
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
            // Only the global: a parameter, a local or a block's own binding
            // named `fetch` is someone's own function (carrick#1648). A
            // parameter that holds the global unless a caller hands another,
            // and a field set to one, are the global too (carrick#1562).
            Expr::Ident(ident) => {
                (ident.sym == *"fetch"
                    && scope.module.declared_below.binding_count(&ident_key(ident)) == 0)
                    || scope.fetches.contains(&ident_key(ident))
            }
            Expr::Member(member) => {
                (member_prop(member).as_deref() == Some("fetch")
                    && matches!(&*member.obj, Expr::Ident(obj)
                        if obj.sym == *"window" || obj.sym == *"globalThis"))
                    || (matches!(&*member.obj, Expr::This(_))
                        && this_field(member).is_some_and(|field| {
                            scope
                                .fields
                                .is_some_and(|fields| fields.fetches.contains(&field))
                        }))
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

        // The options the request carries: the bag, or the object `fetch` is
        // handed, whether or not its own keys are written at the call
        // (`fetch(url, { ...INIT })` sends INIT's method).
        let options = bag.map(|(_, obj)| obj).or(match (kind, args.get(1)) {
            (RequestKind::Fetch, Some(Value::Obj(options))) => Some(options),
            _ => None,
        });
        let method = match options {
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
                None => MethodValue::Lit("GET".to_string()),
            },
        };

        let body = match options {
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
            base_scope: None,
            semantics: BTreeSet::new(),
        })
    }

    /// A maker call on a binding imported from a package: `<client>.<member>(…)`
    /// or `<client>(…)`, or the same constructed with `new` (carrick#1564,
    /// carrick#1661). The instance it builds, with what it was handed.
    /// Anything else — an instance's own maker call, an argument spread that
    /// moves every position — builds nothing this pass reads.
    ///
    /// Which of these is a maker at all is the package's claims' to say, read
    /// once the summaries or the library sites are composed: an HTTP reading
    /// reads only `<client>.<member>({ … })` ([`ClientInstance::http_factory`]).
    fn factory_call(&self, expr: &Expr, scope: &Scope<'_>) -> Option<ClientRef> {
        let expr = match expr {
            Expr::TsAs(e) => &*e.expr,
            Expr::TsNonNull(e) => &*e.expr,
            Expr::TsSatisfies(e) => &*e.expr,
            other => other,
        };
        let expr = expr.unwrap_parens();
        let (form, callee, args): (MakerForm, &Expr, &[ExprOrSpread]) = match expr {
            Expr::Call(call) => match &call.callee {
                Callee::Expr(callee) => (MakerForm::Call, &**callee, call.args.as_slice()),
                _ => return None,
            },
            Expr::New(new) => (
                MakerForm::New,
                &*new.callee,
                new.args.as_deref().unwrap_or_default(),
            ),
            _ => return None,
        };
        let (binding, member) = match callee {
            Expr::Ident(binding) => (binding, None),
            Expr::Member(member) => match &*member.obj {
                Expr::Ident(binding) => (binding, Some(member_prop(member)?)),
                _ => return None,
            },
            _ => return None,
        };
        let client = scope.receiver(binding)?;
        if client.instance.is_some() {
            return None;
        }
        if args.iter().any(|arg| arg.spread.is_some()) {
            return None;
        }
        // An HTTP factory's options: exactly one object literal, as before.
        let options = match args {
            [options] => match crate::graphql_document_sites::unwrap_expression(&options.expr) {
                Expr::Object(literal) => Some(self.object(literal, scope)),
                _ => None,
            },
            _ => None,
        };
        Some(ClientRef {
            package: client.package.clone(),
            export: client.export.clone(),
            instance: Some(ClientInstance {
                form,
                member,
                options,
                args: args
                    .iter()
                    .map(|arg| library_sites::site_arg(&arg.expr, scope))
                    .collect(),
                site: self.site(expr.span()),
                made_by: MadeBy::Export,
                awaited: false,
            }),
            // A write through the export before the factory ran reaches the
            // instance; the caller adds the instance binding's own uses.
            contested: client.contested,
            called: BTreeSet::new(),
            called_computed: false,
            contested_message: client.contested_message,
            member_uses: BTreeSet::new(),
            export_uses: client.member_uses.clone(),
            returned: client.returned.clone(),
        })
    }

    /// The library client a call is made through, when the syntax names one
    /// (carrick#1564): `client.member(...)`, `client(...)`, and the same on a
    /// class field (`this.api.member(...)`, `this.api(...)`).
    fn call_receiver(&self, callee: &Expr, scope: &Scope<'_>) -> Option<CallReceiver> {
        match callee.unwrap_parens() {
            Expr::Ident(ident) => Some(CallReceiver {
                client: scope.call_binding(ident)?,
                member: None,
            }),
            Expr::Member(member) => {
                if let Expr::This(_) = &*member.obj {
                    return Some(CallReceiver {
                        client: ClientBinding::Own(field_receiver(member, scope)?.clone()),
                        member: None,
                    });
                }
                let member_name = member_prop(member)?;
                let client = match member.obj.unwrap_parens() {
                    Expr::Ident(ident) => scope.call_binding(ident)?,
                    Expr::Member(field) if matches!(&*field.obj, Expr::This(_)) => {
                        ClientBinding::Own(field_receiver(field, scope)?.clone())
                    }
                    _ => return None,
                };
                Some(CallReceiver {
                    client,
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
    scope.fields?.receivers.get(&this_field(member)?)
}

/// The field a member of `this` names (`this.api`, `this.#api`). A computed
/// key names none.
fn this_field(member: &MemberExpr) -> Option<String> {
    match &member.prop {
        MemberProp::Ident(ident) => Some(ident.sym.to_string()),
        MemberProp::PrivateName(private) => Some(format!("#{}", private.name)),
        MemberProp::Computed(_) => None,
    }
}

/// Every binding `pat` introduces whose default is the platform's `fetch`
/// ([`platform_fetch`]): `fetchImpl = fetch`, `{ fetchImpl = fetch }`,
/// `{ fetch: impl = fetch } = {}` (carrick#1562).
fn fetch_bindings(pat: &Pat, module: &ModuleScope, out: &mut HashSet<BindingKey>) {
    match pat {
        Pat::Assign(assign) => match &*assign.left {
            Pat::Ident(ident) if platform_fetch(&assign.right, module) => {
                out.insert(ident_key(&ident.id));
            }
            other => fetch_bindings(other, module, out),
        },
        Pat::Object(object) => {
            for prop in &object.props {
                match prop {
                    ObjectPatProp::Assign(assign)
                        if assign
                            .value
                            .as_deref()
                            .is_some_and(|value| platform_fetch(value, module)) =>
                    {
                        out.insert(ident_key(&assign.key.id));
                    }
                    ObjectPatProp::KeyValue(kv) => fetch_bindings(&kv.value, module, out),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// Whether `expr` is the platform's `fetch`, as a default or a fallback
/// names it (carrick#1562): the global (no binding of the module or below
/// it is named `fetch`), `globalThis.fetch` or `window.fetch`, either bound
/// to the global object, or a `??`/`||` falling back to one.
fn platform_fetch(expr: &Expr, module: &ModuleScope) -> bool {
    match crate::graphql_document_sites::unwrap_expression(expr) {
        Expr::Ident(ident) => {
            ident.sym == *"fetch" && module.declared_below.binding_count(&ident_key(ident)) == 0
        }
        Expr::Member(member) => {
            member_prop(member).as_deref() == Some("fetch")
                && matches!(&*member.obj, Expr::Ident(obj)
                    if obj.sym == *"window" || obj.sym == *"globalThis")
        }
        Expr::Call(call) => match &call.callee {
            Callee::Expr(callee) => {
                match crate::graphql_document_sites::unwrap_expression(callee) {
                    Expr::Member(bind) if member_prop(bind).as_deref() == Some("bind") => {
                        platform_fetch(&bind.obj, module)
                    }
                    _ => false,
                }
            }
            _ => false,
        },
        Expr::Bin(bin) if matches!(bin.op, BinaryOp::NullishCoalescing | BinaryOp::LogicalOr) => {
            platform_fetch(&bin.right, module)
        }
        _ => false,
    }
}

/// The library instances a function written inside another can read: the
/// enclosing function's, less any name it declares again itself.
fn inherited_receivers<N>(captured: &Captured, node: &N) -> HashMap<BindingKey, ClientRef>
where
    N: VisitWith<Declarations>,
{
    if captured.receivers.is_empty() {
        return HashMap::new();
    }
    let declared = Declarations::of(node);
    captured
        .receivers
        .iter()
        .filter(|((name, _), _)| !declared.declares_name(name))
        .map(|(key, client)| (key.clone(), client.clone()))
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
        // `produce()`: the body invokes a parameter. One that holds the
        // platform's `fetch` unless a caller hands another is a `fetch`
        // (carrick#1562).
        let invokes_param = match &call.callee {
            Callee::Expr(callee) => match &**callee {
                Expr::Ident(ident) if !self.scope.fetches.contains(&ident_key(ident)) => {
                    self.scope.param_index(ident)
                }
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
        // The same call as a message role reads it (carrick#1661), kept apart
        // from what the summaries compose.
        if let Callee::Expr(callee) = &call.callee
            && let Some(site) =
                self.reader
                    .library_site(call.span, MakerForm::Call, callee, &call.args, self.scope)
        {
            self.ir.library.push(site);
        }
        let callee = match &call.callee {
            Callee::Expr(callee) => match &**callee {
                Expr::Ident(ident) => ident.sym.to_string(),
                Expr::Member(member) => member_prop(member).unwrap_or_default(),
                _ => String::new(),
            },
            _ => String::new(),
        };
        let name_line = match &call.callee {
            Callee::Expr(callee) => {
                match crate::graphql_document_sites::unwrap_expression(callee) {
                    Expr::Member(member) => self.reader.line(member.prop.span()),
                    other => self.reader.line(other.span()),
                }
            }
            _ => self.reader.line(call.span),
        };
        let site_args = (!spread).then(|| {
            call.args
                .iter()
                .map(|arg| library_sites::site_arg(&arg.expr, self.scope))
                .collect()
        });
        self.ir.calls.push(CallIr {
            site: self.reader.site(call.span),
            callee,
            args,
            request,
            inert,
            invokes_param,
            receiver,
            site_args,
            name_line,
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
        // A construction through a library client (`new Queue("emails")`):
        // a maker, or a definition, as a message role reads it
        // (carrick#1661). Never a summary's call.
        if let Some(site) = self.reader.library_site(
            new.span,
            MakerForm::New,
            &new.callee,
            new.args.as_deref().unwrap_or_default(),
            self.scope,
        ) {
            self.ir.library.push(site);
        }
        new.visit_children_with(self);
    }

    // `return queue`: what the body returns, for the own-factory rule
    // (carrick#1562). Never a summary's.
    fn visit_return_stmt(&mut self, ret: &ReturnStmt) {
        let returned = ret
            .arg
            .as_deref()
            .map(|arg| self.reader.returned_instances(arg, self.scope))
            .unwrap_or_default();
        self.ir.returns.push((ret.span.lo.0, returned));
        ret.visit_children_with(self);
    }

    // `this.queue = new Queue("emails")`: a maker write, as the message-role
    // field rule reads it (carrick#1665). Never a summary's.
    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        if let Some(write) = self.reader.field_write(assign, self.scope) {
            self.ir.field_writes.push(write);
        }
        assign.visit_children_with(self);
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
        for key in self.scope.params.iter().flatten() {
            values.insert(key.clone(), Value::opaque(key.0.clone()));
        }
        Captured {
            values,
            texts: self
                .scope
                .texts
                .iter()
                .filter(|(_, pieces)| {
                    !pieces
                        .iter()
                        .any(|piece| matches!(piece, library_sites::TextPiece::Param(_)))
                })
                .map(|(key, pieces)| (key.clone(), pieces.clone()))
                .collect(),
            receivers: self.scope.local_receivers.clone(),
            fetches: self.scope.fetches.clone(),
        }
    }
}

/// Names assigned after their declaration, anywhere in a body.
#[derive(Default)]
struct Reassigned {
    names: HashSet<String>,
    /// How many assignments name each binding as their whole target
    /// (`x = …`, `x += …`): [`Declarations`] counts each such target among a
    /// name's declarations, since it is a binding identifier too.
    targets: HashMap<String, usize>,
}

impl Reassigned {
    /// Whether the body declares `name` once, whatever it assigns to it
    /// (carrick#1689).
    fn declared_once(&self, declared: &Declarations, name: &str) -> bool {
        declared.name_count(name) == 1 + self.targets.get(name).copied().unwrap_or(0)
    }
}

impl Visit for Reassigned {
    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        if let AssignTarget::Simple(SimpleAssignTarget::Ident(ident)) = &assign.left {
            self.names.insert(ident.id.sym.to_string());
            *self.targets.entry(ident.id.sym.to_string()).or_default() += 1;
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
    /// The file whose scope `base` was read in ([`RequestShape::base_scope`]).
    base_scope: Option<PathBuf>,
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
            base_scope: shape.base_scope.clone(),
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
            base_scope: self.base_scope.clone(),
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
    /// For a row whose base was read in another module's scope (carrick#1568):
    /// that module's environment-read defaults, which describe the base as
    /// that module's own rows describe it. The file the row sits in has
    /// defaults of its own that say nothing about this base.
    pub base_fallbacks: Option<BTreeMap<String, String>>,
}

/// What the summaries are composed from, as discovery leaves it: every
/// file's functions, the call sites the call graph resolved, and the module
/// each import the files read names ([`FileIr::bindings_wanted`]). Held until
/// the service's library semantics are verified, which needs detection and
/// the service's sidecar (carrick#1564).
#[derive(Debug, Default)]
pub struct RequestSummaryInputs {
    pub files: HashMap<PathBuf, FileIr>,
    pub sites: CallSiteTargets,
    pub bindings: ImportedBindings,
}

/// How many modules re-exporting a binding they import are followed to the
/// one that declares it, beyond the hops the call graph's resolver already
/// takes. The resolver's own cap, for the same reason.
const MAX_REEXPORT_HOPS: usize = 8;

/// Every module-scope client of the service, keyed by (file as walked,
/// binding), with every module's uses of it merged in, and the imports that
/// read one (carrick#1568).
///
/// One instance is one object wherever it is imported, so a use that may
/// change its base in any module is a use of it in every module: the merge
/// is what makes the declaring module's own calls, and every importer's, read
/// nothing once any module writes through it, hands it on, calls a member its
/// verified surface does not name, or loads its module some other way
/// (`import("./api")`, a `require` inside a function).
///
/// Where a use cannot be followed to what it names, because the resolver
/// stopped at a cap, the use may be of any instance: no import reads through
/// one anywhere in the service. The declaring modules still read their own
/// calls, as they did before an import was followed at all.
#[derive(Debug, Default)]
struct LinkedClients {
    clients: HashMap<(PathBuf, String), ClientRef>,
    /// (importer, local) -> the client it reads, for an import that reaches
    /// one only through modules that publish nothing through `exports`.
    imports: HashMap<(PathBuf, String), (PathBuf, String)>,
    /// Some use could not be followed to what it names.
    unfollowable: bool,
}

/// What an import, followed through the modules that re-export it, reaches.
enum Declared {
    /// A module-scope client, and whether every module on the way publishes
    /// nothing through `exports` ([`FileIr::names_commonjs_exports`]), which
    /// is when another module may read through it.
    Client((PathBuf, String), bool),
    /// A whole module, which a namespace re-exported by name stands for.
    Module(Vec<PublishedBinding>),
    /// Something that is not a client.
    Nothing,
    /// A cap, or a hop to a module nothing resolves, stopped the walk.
    Unfollowable,
}

impl LinkedClients {
    fn link(files: &HashMap<PathBuf, FileIr>, bindings: &ImportedBindings) -> Self {
        let mut linked = LinkedClients::default();
        for (file, ir) in files {
            for (name, client) in &ir.module_clients {
                linked
                    .clients
                    .insert((file.clone(), name.clone()), client.clone());
            }
        }
        for (file, ir) in files {
            for (local, used) in &ir.imported {
                match bindings.get(file, local) {
                    Some(ImportedBinding::Binding { file: at, name }) => {
                        match declared_client(files, bindings, at, name) {
                            Declared::Client(held, readable) => {
                                linked.merge(&held, used);
                                if readable {
                                    linked.imports.insert((file.clone(), local.clone()), held);
                                }
                            }
                            Declared::Module(published) => {
                                linked.merge_namespace(files, bindings, &published, used);
                            }
                            Declared::Nothing => {}
                            Declared::Unfollowable => linked.unfollowable = true,
                        }
                    }
                    Some(ImportedBinding::Module(published)) => {
                        linked.merge_namespace(files, bindings, published, used);
                    }
                    Some(ImportedBinding::Unfollowable | ImportedBinding::Unresolved) => {
                        linked.unfollowable = true;
                    }
                    None => {}
                }
            }
            // A module loaded some other way: nothing says what is done with
            // it, so every client it publishes is taken as changed.
            for specifier in &ir.loads {
                match bindings.load(file, specifier) {
                    Some(ImportedBinding::Module(published)) => {
                        linked.contest_published(files, bindings, published);
                    }
                    Some(ImportedBinding::Unfollowable) => linked.unfollowable = true,
                    // A load by `import()` or `require` of a specifier that
                    // names nothing is among the file's unresolved
                    // specifiers below; any other call handed one names a
                    // file or a directory, not a module.
                    Some(ImportedBinding::Unresolved | ImportedBinding::Binding { .. }) | None => {}
                }
            }
        }
        // A module some file loads by a specifier that names nothing the
        // scan can find may be any module of the service, and may do
        // anything to what it publishes, however the file then uses it.
        linked.unfollowable |= !bindings.unresolved_in().is_empty();
        linked
    }

    /// Merge one module's uses of a binding into the client it names.
    fn merge(&mut self, held: &(PathBuf, String), used: &BindingUse) {
        if let Some(client) = self.clients.get_mut(held) {
            client.contested |= used.contests_client();
            client.called.extend(used.called.iter().cloned());
            client.called_computed |= used.called_computed;
            client.contested_message |= used.contests_message();
            client.member_uses.extend(used.member_uses());
            client.returned.extend(used.returned_at.iter().copied());
        }
    }

    /// A namespace reads each binding its module publishes as a member: used
    /// any way but calling one of them directly, it may reach any of them.
    fn merge_namespace(
        &mut self,
        files: &HashMap<PathBuf, FileIr>,
        bindings: &ImportedBindings,
        published: &[PublishedBinding],
        used: &BindingUse,
    ) {
        // A call through a member of the namespace (`lib.api.send(…)`) is a
        // use of the namespace other than calling a binding it publishes, for
        // a message role as for HTTP (carrick#1661).
        let contests = used.contests_client() || used.called_computed;
        for binding in published {
            match declared_client(files, bindings, &binding.file, &binding.name) {
                Declared::Client(held, _) => {
                    if let Some(client) = self.clients.get_mut(&held) {
                        client.contested |= contests;
                        client.contested_message |= contests;
                        if used.called.contains(&Some(binding.published.clone())) {
                            client.called.insert(None);
                            client.member_uses.insert(MemberUse {
                                form: MakerForm::Call,
                                path: Vec::new(),
                                member: None,
                            });
                        }
                    }
                }
                Declared::Module(inner) => {
                    if contests {
                        self.contest_published(files, bindings, &inner);
                    }
                }
                Declared::Nothing => {}
                Declared::Unfollowable => self.unfollowable = true,
            }
        }
    }

    /// Every client `published` reaches, taken as changed.
    fn contest_published(
        &mut self,
        files: &HashMap<PathBuf, FileIr>,
        bindings: &ImportedBindings,
        published: &[PublishedBinding],
    ) {
        let contested = BindingUse {
            other: true,
            ..BindingUse::default()
        };
        self.merge_namespace(files, bindings, published, &contested);
    }

    /// The client a call's receiver binding holds, with every module's uses,
    /// and the file whose scope its options were read in.
    fn client_in_scope<'c>(
        &'c self,
        file: &'c Path,
        binding: &'c ClientBinding,
    ) -> Option<(&'c ClientRef, &'c Path)> {
        match binding {
            ClientBinding::Own(client) => Some((client, file)),
            ClientBinding::Module(name) => self
                .clients
                .get(&(file.to_path_buf(), name.clone()))
                .map(|client| (client, file)),
            ClientBinding::Imported { local, package } => {
                let held = (!self.unfollowable)
                    .then(|| self.imports.get(&(file.to_path_buf(), local.clone())))
                    .flatten();
                match held {
                    Some(held) => self
                        .clients
                        .get(held)
                        .map(|client| (client, held.0.as_path())),
                    None => package.as_ref().map(|client| (client, file)),
                }
            }
        }
    }
}

/// What `name` in `file` reaches, following a module that re-exports a
/// binding it imports (`import { api } from "./api"; export { api }`) to the
/// module that declares it.
fn declared_client(
    files: &HashMap<PathBuf, FileIr>,
    bindings: &ImportedBindings,
    file: &Path,
    name: &str,
) -> Declared {
    let mut at = (file.to_path_buf(), name.to_string());
    let mut readable = true;
    for _ in 0..=MAX_REEXPORT_HOPS {
        let Some(ir) = files.get(&at.0) else {
            return Declared::Nothing;
        };
        readable &= !ir.names_commonjs_exports;
        if ir.module_clients.contains_key(&at.1) {
            return Declared::Client(at, readable);
        }
        if !ir.imported.contains_key(&at.1) {
            return Declared::Nothing;
        }
        match bindings.get(&at.0, &at.1) {
            Some(ImportedBinding::Binding { file, name }) => at = (file.clone(), name.clone()),
            Some(ImportedBinding::Module(published)) => return Declared::Module(published.clone()),
            Some(ImportedBinding::Unfollowable | ImportedBinding::Unresolved) => {
                return Declared::Unfollowable;
            }
            None => return Declared::Nothing,
        }
    }
    Declared::Unfollowable
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
    clients: LinkedClients,
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
                    if let Some(request) = shape_of(call, file, self.semantics, &self.clients) {
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
        clients: LinkedClients::link(files, &inputs.bindings),
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
                    match row(&instantiated, file, composer.files, call, reaches, false) {
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
                if let Some(request) = shape_of(call, file, composer.semantics, &composer.clients) {
                    let library = request.kind == RequestKind::Library;
                    if library || (request.kind != RequestKind::Verb && !request.url_inline) {
                        let effect = Effect::from_shape(&request, file, call.site.line);
                        if !effect.has_params() {
                            match row(&effect, file, composer.files, call, None, !library) {
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
    file: &Path,
    files: &HashMap<PathBuf, FileIr>,
    call: &CallIr,
    reaches_request: Option<String>,
    own_site: bool,
) -> Option<SummaryRow> {
    let site = call.site;
    let MethodValue::Lit(method) = &effect.method else {
        return None;
    };
    if !base_reads_the_same_in(effect, file) {
        return None;
    }
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
        base_fallbacks: effect
            .base_scope
            .as_ref()
            .filter(|scope| scope.as_path() != file)
            .map(|scope| {
                files
                    .get(scope)
                    .map(|ir| ir.env_fallbacks.clone())
                    .unwrap_or_default()
            }),
    })
}

/// Whether a row stated in `file` states the base `effect` was read with
/// (carrick#1568).
///
/// A row's target names an opaque base by the expression that holds it
/// (`${config.apiUrl}`), and the passes after this one read that name in the
/// file the row sits in. A base read in another module's scope, a client that
/// module declares, would then be read as whatever the stating file binds to
/// that name, or as nothing. So such a row is stated only where every piece
/// of its base means the same in every file: text the source writes, or an
/// environment read.
fn base_reads_the_same_in(effect: &Effect, file: &Path) -> bool {
    match &effect.base_scope {
        Some(scope) if scope != file => effect.base.iter().all(|piece| match piece {
            Piece::Lit(_) => true,
            Piece::Opaque(text) => reads_the_environment(text),
            Piece::Param(..) | Piece::Unknown => false,
        }),
        _ => true,
    }
}

/// Whether `text` is exactly an environment read, in one of the spellings the
/// runtimes give it (as [`crate::env_alias`] reads them): `process.env.NAME`,
/// `import.meta.env.NAME`, either with a string index, or `Deno.env.get("NAME")`.
fn reads_the_environment(text: &str) -> bool {
    let name = |name: &str| {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    };
    let quoted = |inner: &str| {
        [('"', '"'), ('\'', '\'')].iter().any(|(open, close)| {
            inner
                .strip_prefix(*open)
                .and_then(|rest| rest.strip_suffix(*close))
                .is_some_and(name)
        })
    };
    ["process.env", "import.meta.env"].iter().any(|object| {
        text.strip_prefix(object).is_some_and(|rest| {
            rest.strip_prefix('.').is_some_and(name)
                || rest
                    .strip_prefix('[')
                    .and_then(|rest| rest.strip_suffix(']'))
                    .is_some_and(quoted)
        })
    }) || text
        .strip_prefix("Deno.env.get(")
        .and_then(|rest| rest.strip_suffix(')'))
        .is_some_and(quoted)
}

/// What a call sends when the call graph resolves it to nothing of this
/// service: through a verified library client, what the client's claims say
/// (carrick#1564); otherwise the shape the call is written in.
fn shape_of<'c>(
    call: &'c CallIr,
    file: &Path,
    semantics: &LibrarySemantics,
    clients: &LinkedClients,
) -> Option<Cow<'c, RequestShape>> {
    call.receiver
        .as_ref()
        .and_then(|receiver| {
            let (client, scope) = clients.client_in_scope(file, &receiver.client)?;
            let mut shape =
                library_shape(client, receiver.member.as_deref(), &call.args, semantics)?;
            if !shape.base.is_empty() {
                shape.base_scope = Some(scope.to_path_buf());
            }
            Some(shape)
        })
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
/// the file uses the client other than to call through it
/// ([`ClientRef::contested`]), the factory's options are open and do not
/// name the base key after everything that could overwrite it, or an
/// options or config object handed to the call is open or names a base key
/// itself. Every base key detection claims for the client counts, verified
/// or not.
fn library_shape(
    client: &ClientRef,
    member: Option<&str>,
    args: &[Value],
    semantics: &LibrarySemantics,
) -> Option<RequestShape> {
    if semantics.is_empty() {
        return None;
    }
    if client.contested {
        return None;
    }
    let base_keys = semantics.claimed_base_keys(&client.package, &client.export);
    let mut used: BTreeSet<String> = BTreeSet::new();
    let (surface, base) = match &client.instance {
        None => (
            semantics.surface(&client.package, &client.export, "export")?,
            Vec::new(),
        ),
        Some(instance) => {
            // Only `export.member({ … })` is an HTTP factory's instance; any
            // other maker's reads as no client (carrick#1661).
            let (factory_member, options) = instance.http_factory()?;
            let factory = semantics.factory(&client.package, &client.export, factory_member)?;
            let surface = semantics.surface(
                &client.package,
                &client.export,
                &instance_receiver(factory_member),
            )?;
            if client.called_computed
                || client
                    .called
                    .iter()
                    .any(|member| !surface.names(member.as_deref()))
            {
                return None;
            }
            // The base is the literal or opaque value the options hold, or
            // none when they do not name the key at all. A key still in the
            // options was written after every entry that could overwrite it
            // ([`Reader::object`] drops the rest), so it stands even in open
            // options; a key missing from open options may be in the part the
            // source does not state.
            let base = match options.fields.get(&factory.base_url_key) {
                Some(Value::Str(pieces)) => pieces.clone(),
                Some(_) => return None,
                None if options.open => return None,
                None => Vec::new(),
            };
            used.insert(factory.claim_id.clone());
            (surface, base)
        }
    };

    let verb = member.and_then(|member| surface.verb(member));
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
            let request = surface.requests(member).find(|request| {
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
        base_scope: None,
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

    /// carrick#1661 split two uses out of the member read and the operand
    /// they were: a call through a sub-object and a construction. A message
    /// role reads both as calls; an HTTP client is contested by each exactly
    /// as before, and an object constant that is constructed still holds no
    /// known keys.
    #[test]
    fn a_construction_or_a_sub_object_call_still_contests_an_http_client() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("input.ts");
        std::fs::write(
            &path,
            "new Bare();\n\
             new lib.Worker();\n\
             new lib2.jobs.Worker();\n\
             api.tasks.trigger(\"x\");\n\
             this.client.tasks.trigger(\"y\");\n",
        )
        .expect("write file");
        let cm: Lrc<SourceMap> = Default::default();
        let handler = swc_common::errors::Handler::with_tty_emitter(
            swc_common::errors::ColorConfig::Never,
            true,
            false,
            Some(cm.clone()),
        );
        let module = crate::parser::parse_file(&path, &cm, &handler).expect("parsed module");
        let mut uses = BindingUses::default();
        module.visit_with(&mut uses);
        for name in ["Bare", "lib", "lib2", "api", "this.client"] {
            let used = &uses.uses[name];
            assert!(used.contests_client(), "{name} contests an HTTP client");
            assert!(
                !used.contests_message(),
                "{name} is a call to a message role"
            );
            assert!(!used.library_calls.is_empty(), "{name}");
        }
        assert!(!uses.uses["Bare"].keeps_object());
        assert!(uses.uses["lib"].keeps_object());
    }

    /// carrick#1665 split a truthiness test out of the operand it was: a
    /// message role reads `if (!this.client)` as reading the binding and
    /// keeping nothing of it, and an HTTP client and an object constant are
    /// contested by it exactly as before.
    #[test]
    fn a_truthiness_test_still_contests_an_http_client() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("input.ts");
        std::fs::write(
            &path,
            "if (api) {}\n\
             if (!this.client) {}\n\
             while (lib) {}\n\
             do {} while (bus);\n\
             for (; queue; ) {}\n\
             const picked = opts ? 1 : 2;\n\
             const both = !!pair;\n",
        )
        .expect("write file");
        let cm: Lrc<SourceMap> = Default::default();
        let handler = swc_common::errors::Handler::with_tty_emitter(
            swc_common::errors::ColorConfig::Never,
            true,
            false,
            Some(cm.clone()),
        );
        let module = crate::parser::parse_file(&path, &cm, &handler).expect("parsed module");
        let mut uses = BindingUses::default();
        module.visit_with(&mut uses);
        for name in ["api", "this.client", "lib", "bus", "queue", "opts", "pair"] {
            let used = &uses.uses[name];
            assert!(used.tested, "{name} is tested");
            assert!(used.contests_client(), "{name} contests an HTTP client");
            assert!(
                !used.contests_message(),
                "{name} is no use to a message role"
            );
            assert!(
                !used.keeps_object(),
                "{name} is no object constant's key set"
            );
        }
    }

    /// carrick#1562 split a returned binding out of the operand it was: a
    /// message role follows an own factory's `return client` to its callers,
    /// and an HTTP client and an object constant are contested by it exactly
    /// as before, an arrow's expression body included.
    #[test]
    fn a_returned_client_still_contests_an_http_client() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("input.ts");
        std::fs::write(
            &path,
            "function a() { return api; }\n\
             function b() { return (this.client as Client); }\n\
             const c = () => opts;\n\
             const d = () => (lib!);\n",
        )
        .expect("write file");
        let cm: Lrc<SourceMap> = Default::default();
        let handler = swc_common::errors::Handler::with_tty_emitter(
            swc_common::errors::ColorConfig::Never,
            true,
            false,
            Some(cm.clone()),
        );
        let module = crate::parser::parse_file(&path, &cm, &handler).expect("parsed module");
        let mut uses = BindingUses::default();
        module.visit_with(&mut uses);
        for name in ["api", "this.client", "opts", "lib"] {
            let used = &uses.uses[name];
            assert_eq!(used.returned_at.len(), 1, "{name} is returned once");
            assert!(used.contests_client(), "{name} contests an HTTP client");
            assert!(
                !used.contests_message(),
                "{name} is followed, not handed off, for a message role"
            );
            assert!(
                !used.keeps_object(),
                "{name} is no object constant's key set"
            );
        }
        let mut merged = BindingUse::default();
        merged.merge(&uses.uses["api"]);
        assert_eq!(merged.returned_at, uses.uses["api"].returned_at);
    }
}
