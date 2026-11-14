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
//! - **A response's init is not a request's** (carrick#1986). A bag holding
//!   only `headers` (and `status`, `statusText`) is what a response is built
//!   with too: `return redirect(path, { headers })` sends nothing. Read off
//!   such a bag alone, a call is a request only where the source waits on
//!   its result (`await`, `.then`) or the callee is a function the
//!   repository defines; anywhere else it states nothing
//!   ([`Composer::request_of`]).
//! - **A `fetch` handed in is `fetch`** (carrick#1562). A parameter whose
//!   default is the platform's `fetch` (`fetchImpl = fetch`, `{ fetchImpl =
//!   fetch } = {}`) and that the body never assigns again, and a field the
//!   field table reads as set once to the global, to such a constructor
//!   parameter, or to `options.fetch ?? fetch`, are the platform's `fetch`,
//!   whatever a caller hands in instead.
//! - **A declared method is every verb it names** (carrick#2049). A method
//!   that is a parameter typed as a closed set of HTTP verbs (`method:
//!   "POST" | "DELETE"`, or an alias the file declares:
//!   [`crate::declared_verbs`]) is sent as one of them, so the request's own
//!   line states a row per verb, even where its URL is written at the call.
//!   Which verbs: those its calls write, when every call writes one; every
//!   verb the declaration allows when a call hands it a value nothing here
//!   reads, or nothing calls it. A caller that writes a verb states that one
//!   at its own line as well. A parameter typed `string`, an optional one,
//!   one the body assigns again, one typed by an import and one of a
//!   function written inside another (whose callers are not resolved here)
//!   states no verb.
//! - **A conditional's branches are requests** (carrick#2050). A URL a test
//!   chooses (`kind === "draft" ? `drafts/${id}` : `posts/${id}``, held in a
//!   local or written in the URL) is one row per branch. A branch nothing
//!   here read, beside one that writes a path, may span any number of
//!   segments, so no placeholder stands for it ([`alternatives`]).
//! - **One test is one choice** (carrick#2051). A method and a URL a test
//!   chooses together (`fetch(existing ? `/notes/${id}` : "/notes", { method:
//!   existing ? "PATCH" : "POST" })`, or the same through locals) pair branch
//!   with branch: `PATCH /notes/:id` and `POST /notes`. Two different tests
//!   pair nothing, and no row is stated.
//! - **A builder's return is a value** (carrick#1562). A call to a
//!   module-scope builder (an arrow or a function whose body only returns an
//!   expression, or one held in a constant object nothing writes through) is
//!   what it returns, with the call's arguments in its parameters. Several
//!   returns that end in one written path, after values the source does not
//!   write, read as a hole for those values with the path after it
//!   (carrick#2536). Returns that write different
//!   paths, and a conditional whose branches do, read as no one path and
//!   state no row (carrick#2541).
//! - **A key of an object parameter is a parameter** (carrick#1950). A
//!   wrapper that takes its request as one object (`send({ url, body })`)
//!   reads each key as the caller writes it: through a destructured
//!   parameter (`{ url }`, `{ url: target }`), through the parameter itself
//!   (`options.url`), or unpacked in the body (`const { url } = options`).
//!   The object literal a caller passes fills the key in, and a caller that
//!   passes its own parameter on leaves it to its own caller. Nothing is read
//!   through a binding the function assigns again, a key with a default, a
//!   `??`/`||` fallback on one, or a key the caller's object does not state
//!   (it is missing, or a spread or a computed key after it may overwrite
//!   it). A key waits on a caller only where the request states no route
//!   without it (the whole URL, a path glued after a base). Where it does
//!   (a base before a literal path, a whole path segment), the request is
//!   stated at its own line with the key under the name the function reads
//!   it by, as it was before keys were read ([`Effect::settle_keys`]).
//! - **A built query says nothing about the route** (carrick#1950). A call
//!   to a function of the same class (`this.query(ref)`) or the same module
//!   (`pageQuery(cursor)`) whose every `return` is the empty string or text
//!   starting with a literal `?` yields nothing or a query. It ends the path:
//!   a value before it is a whole segment, and the row's target leaves it
//!   out. The function must not be `async`, must end in a `return`, and must
//!   not be one a subclass in the file overrides or the class assigns over.
//!   Any other value glued after a segment still states no row.
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

// What a conditional leaves a URL or a method able to read as (carrick#2050,
// carrick#2051).
mod alternatives;
use alternatives::{Alternation, Alternatives, Verdict, WayGuard};

// The requests a function builds and returns for a caller to send
// (carrick#3470): stated as candidate rows where nothing in the service
// calls the function.
pub mod descriptors;

// What each function compares a request value against, and where that value
// comes from (carrick#2386): the receive side of the same IR. Only tests and
// the offline census read it so far, so the binary sees it as unused.
#[allow(dead_code)]
mod guards;
use guards::{GuardIr, Subject, Tested, WrittenIn};
// `new URL(input, base)` by the URL standard: the one reader both the value
// reader and the guards call (carrick#4124).
mod new_url;

// Where a discovered call's name is decided (carrick#2433), and the record
// each decided site carries (carrick#2435): record-only, so only tests and
// the offline census read them yet.
#[allow(dead_code)]
pub mod decided_sites;
// The files the fact layer would send the file reader (carrick#2613): record-only.
pub mod fact_files;
// The one list of reasons a site was not read (carrick#3688).
pub mod stop;
pub use stop::Stop;
#[cfg(test)]
mod site_census;
#[allow(dead_code)]
pub mod site_record;

// What each import binding reaches, read from the compiler's per-position
// rows when the run walks them (carrick#2413 slice B).
mod row_links;

// The definition each call reaches, read from the compiler's per-position
// rows when the run walks them (carrick#2413 slice D).
mod row_calls;
pub use row_calls::Unfollowed;
pub(crate) use row_calls::{CallIntoFile, Reach as CallReach};

// What binding the rows costs, and the budget that bounds it (carrick#2564).
mod rows_join;
pub use rows_join::ROWS_JOIN_BUDGET;

// Every call that reaches a labelled library export, at any depth, read
// from the rows (carrick#2431). Record-only: only tests and the offline
// census read it so far, so the binary sees it as unused.
#[allow(dead_code)]
pub(crate) mod discovery;
/// Method decorators that call an in-repo decorator factory
/// (carrick#3688 C1): `src/request_summary/own_decorators.rs`.
pub(crate) mod own_decorators;

// The implementations of an interface member a call goes through, asked of
// the analysis sidecar and read by discovery (carrick#2432 slice B).
pub(crate) mod implementations;

// The routes a hand-written dispatch states (carrick#2474 R4a): record-only,
// logged by a scan with the receiver trace on and dropped.
pub(crate) mod dispatch;

// The value resolver (carrick#3504): what a value may hold, lowered where
// it is written and followed through callers, defaults, constructions and
// returns. Discovery's value walk reads callees through it (carrick#2432).
// Design: carrick-cloud docs/internal/decisions/2026-11-07-one-value-resolver.md
mod resolve;
pub(crate) use resolve::{DocumentEnd, DocumentLookup, document_calls};
use resolve::{Holds, ValueAt};

// What each construction of a class passes, across the service, for the
// fields its constructor sets from a parameter (carrick#2452 slice 2).
mod constructions;
use constructions::{ClassOrigin, FieldLink};

// One fixture service walked with or without rows, for the tests of what
// the summaries read through them.
#[cfg(test)]
pub(crate) mod fixture_walk;

// What the compiler's per-position rows say at each lookup the summaries
// make by name today (carrick#2413): an offline census.
#[cfg(test)]
mod rows_census;

#[allow(unused_imports)]
pub use guards::{
    Branch, CallChain, CallFunction, CallName, CallStart, ChainEnd, ChainSite, Conjoined, Constant,
    GuardChain, GuardPosition, GuardShape, HoleReason, Hop, HopArg, LibraryReceiver, NameFrom,
    NamedCall, Root, Within, compared_methods, guard_chains, named_calls,
};

#[allow(unused_imports)]
pub use library_sites::{
    CallerChain, Contest, Holder, LibrarySite, LibrarySites, MakerForm, MemberUse, MemberWire,
    NameFallback, NamePlace, On, PlacedName, RegistrySpecifier, Selector, SiteArg, SiteMaker,
    SiteNames, SiteObject, SiteReceiver, SlotName, library_sites, package_name, package_spelling,
    registry_specifier,
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
use crate::forwarded_body::{CallBody, ParamDecl, type_param_names};
use crate::import_bindings::DEFAULT_EXPORT;
use crate::services::type_sidecar::{SemanticsRequestArgs, SemanticsVerbArgs};
use crate::swc_scanner::SWC_SPAN_BASE;
use crate::type_manifest::is_http_method;
use crate::visitor::SymbolKind;
use crate::wrapper_request_shape::{
    could_be_response_init, is_request_options, verb_from_callee_property,
};
use library_sites::{ClassThis, FieldWriteIr, LibrarySiteIr};

/// One piece of a URL or a body value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Piece {
    /// Text the source writes.
    Lit(String),
    /// The summarised function's own parameter, by position, with the name
    /// the function gives it. Filled in at each call site.
    Param(usize, String),
    /// One key of the summarised function's own parameter (carrick#1950): the
    /// parameter's position, the key, and the text the function reads it by
    /// (`url` for `{ url }`, `options.url`). Filled in at each call site from
    /// the object the caller writes at that position.
    ParamKey(usize, String, String),
    /// [`Piece::ParamKey`] with what it holds where the caller's object
    /// leaves the key out (carrick#3608): the key's default, or the value a
    /// caller wrote under it before spreading its own options (`{ version:
    /// 'v2', ...options }`). Built only by the caller walk
    /// (`decided_sites::filled_with`), which fills it at the next call up;
    /// left open it reads as the parameter it names.
    ParamKeyOr(usize, String, String, Vec<Piece>),
    /// A value the source names and this pass does not read, kept as written
    /// (`process.env.API_URL`, `config.apiEndpoint`), with where it comes
    /// from as the receive side reads it (carrick#2406): an environment
    /// read, an import, a global, or the hole reading stopped at. The text
    /// alone decides how a target renders.
    ///
    /// `link` names the module-scope binding the value reads through when it
    /// is one a call graph can follow (an imported constant, an entry of an
    /// imported constant object, a call of an imported builder with text
    /// arguments: carrick#2447). Once every module is read, text the link
    /// resolves to takes the piece's place ([`RequestSummaryInputs::linked`]);
    /// a link that resolves to nothing changes nothing. A binding whose text
    /// the names cannot read hands the importer what its own module reads it
    /// as: its text, or one written path after the piece (carrick#2540).
    ///
    /// A field the class's constructor sets from a parameter (`this.base`)
    /// links to that field instead: it takes the value every construction
    /// in the service passes there ([`FieldLink`], carrick#2452).
    Opaque {
        text: String,
        end: Root,
        link: Option<Box<Link>>,
    },
    /// A query one of the service's own functions builds (carrick#1950):
    /// nothing, or text that starts with `?`. It ends the path and states
    /// nothing about the route, so it is read only as the last piece of a URL.
    Query,
    /// A value that cannot be written as text at all (an object in a URL, a
    /// missing argument). A URL holding one states nothing.
    Unknown,
    /// A parameter of a function this one is written inside, whole or by one
    /// of its keys (carrick#1949): how many functions out, and the piece that
    /// function reads it as ([`Piece::Param`] or [`Piece::ParamKey`]). A
    /// callback or a closure is never called with the enclosing function's
    /// arguments, so only a call of the enclosing function fills it in, once
    /// the effect is lifted there ([`Effect::lifted`]).
    Outer(u8, Box<Piece>),
}

/// What an opaque piece reads through, resolved once every module is read
/// ([`RequestSummaryInputs::linked`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Link {
    /// A module-scope binding (carrick#2447).
    Named(library_sites::NamedRef),
    /// A field of a class the file declares (carrick#2452).
    Field(FieldLink),
    /// The parameters the arguments of a call the text reader cannot read
    /// hand it (`signed(`${ROOT}${path}`)`, carrick#4022): the hole is still
    /// one a caller chooses, so the request is open until a caller fills
    /// them ([`Piece::refilled`]), and closed it is the plain hole again.
    /// Nothing resolves it to text.
    Through(Vec<Piece>),
}

impl Piece {
    /// Whether the piece is a parameter a caller may fill in: the summarised
    /// function's own, whole or by one of its keys, or an enclosing
    /// function's.
    fn is_parameter(&self) -> bool {
        matches!(
            self,
            Piece::Param(..) | Piece::ParamKey(..) | Piece::ParamKeyOr(..) | Piece::Outer(..)
        )
    }

    /// Whether a caller still chooses what the piece holds, for a name or a
    /// URL: a parameter, or a hole of an unread call open on one
    /// ([`Link::Through`], carrick#4022). Only the readers of a request's
    /// name ask this; a receiver handed such a hole is no parameter.
    fn waits_on_caller(&self) -> bool {
        self.is_parameter()
            || self
                .through()
                .is_some_and(|inner| inner.iter().any(Piece::waits_on_caller))
    }

    /// The parameters a hole made of an unread call holds ([`Link::Through`]).
    fn through(&self) -> Option<&[Piece]> {
        match self {
            Piece::Opaque {
                link: Some(link), ..
            } => match &**link {
                Link::Through(inner) => Some(inner),
                _ => None,
            },
            _ => None,
        }
    }

    /// A hole made of an unread call ([`Link::Through`]) with its parameters
    /// replaced by `fill(inner)`: still open while one is left, else the
    /// plain hole with its own text and end. `None` for any other piece.
    fn refilled(&self, fill: &dyn Fn(&[Piece]) -> Vec<Piece>) -> Option<Piece> {
        let inner = self.through()?;
        let Piece::Opaque { text, end, .. } = self else {
            return None;
        };
        let inner = fill(inner);
        let open = inner.iter().any(Piece::waits_on_caller);
        Some(Piece::Opaque {
            text: text.clone(),
            end: end.clone(),
            link: open.then(|| Box::new(Link::Through(inner))),
        })
    }

    /// The text a parameter is read by where it is written.
    fn parameter_name(&self) -> Option<&str> {
        match self {
            Piece::Param(_, name)
            | Piece::ParamKey(_, _, name)
            | Piece::ParamKeyOr(_, _, name, _) => Some(name),
            Piece::Outer(_, inner) => inner.parameter_name(),
            _ => None,
        }
    }

    /// The piece as a function written inside this one reads it: one more
    /// function out (carrick#1949).
    fn enclosed(&self) -> Piece {
        match self {
            Piece::Param(..) | Piece::ParamKey(..) | Piece::ParamKeyOr(..) => {
                Piece::Outer(1, Box::new(self.clone()))
            }
            Piece::Outer(depth, inner) => Piece::Outer(depth.saturating_add(1), inner.clone()),
            other => other
                .refilled(&|inner| inner.iter().map(Piece::enclosed).collect())
                .unwrap_or_else(|| other.clone()),
        }
    }

    /// The piece as the function one out reads it: an enclosing function's
    /// parameter becomes its own (carrick#1949).
    fn lifted(&self) -> Piece {
        match self {
            Piece::Outer(1, inner) => (**inner).clone(),
            Piece::Outer(depth, inner) => Piece::Outer(depth - 1, inner.clone()),
            other => other
                .refilled(&|inner| inner.iter().map(Piece::lifted).collect())
                .unwrap_or_else(|| other.clone()),
        }
    }
}

/// `pieces` with each parameter piece `param` answers replaced by its
/// answer, and the holes of unread calls in them refilled the same way
/// ([`Piece::refilled`], carrick#4022).
fn substituted(pieces: &[Piece], param: &dyn Fn(&Piece) -> Option<Vec<Piece>>) -> Vec<Piece> {
    concat(pieces.iter().map(|piece| {
        param(piece).unwrap_or_else(|| {
            vec![
                piece
                    .refilled(&|inner| substituted(inner, param))
                    .unwrap_or_else(|| piece.clone()),
            ]
        })
    }))
}

/// Where a value renamed into one opaque piece comes from (carrick#2406): a
/// value of one piece keeps that piece's end, a key of a parameter ends at
/// the parameter it is read from, and anything else at the expression
/// reading stopped at.
fn end_of(pieces: &[Piece]) -> Root {
    // A value that reads as several written paths keeps that end through a
    // rename (carrick#2541).
    if pieces.iter().any(|piece| {
        matches!(
            piece,
            Piece::Opaque {
                end: Root::Hole(HoleReason::SeveralPaths),
                ..
            }
        )
    }) {
        return Root::Hole(HoleReason::SeveralPaths);
    }
    match pieces {
        [Piece::Lit(text)] => Root::Literal(text.clone()),
        [Piece::Opaque { end, .. }] => end.clone(),
        [Piece::Param(index, name)] => Root::Param(*index, name.clone()),
        [Piece::ParamKey(index, ..) | Piece::ParamKeyOr(index, ..)] => {
            Root::Param(*index, String::new())
        }
        [Piece::Outer(depth, inner)] => match &**inner {
            Piece::Param(index, name) => Root::Outer(*depth, *index, name.clone()),
            Piece::ParamKey(index, ..) | Piece::ParamKeyOr(index, ..) => {
                Root::Outer(*depth, *index, String::new())
            }
            _ => Root::Hole(HoleReason::UnreadExpression),
        },
        _ => Root::Hole(HoleReason::UnreadExpression),
    }
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
    /// A spread of the function's own parameter, and the keys written before
    /// it (carrick#2411): what a caller's argument there puts in place is
    /// read where the call is, so those keys stand unless it sets them.
    /// `fields` and `open` read as if the spread could hold anything: only
    /// the body a request sends reads this.
    pub under_param: Option<(usize, BTreeMap<String, Value>)>,
    /// A spread of the rest of a destructured parameter (`const { path,
    /// ...rest } = config`, carrick#2519), as the first thing the object
    /// holds: the parameter's position and the keys the destructuring takes
    /// out, with how many functions out the parameter is (0: this one's;
    /// a closure reads its enclosing function's, carrick#1949). Only a
    /// decided site's option keys (its method, and its payload slot's key,
    /// carrick#2674) read this; the body a request sends does not, so no body
    /// row moves.
    pub under_rest: Option<(u8, usize, BTreeSet<String>)>,
    /// The keys whose value depends on which spread branch runs (carrick#2474
    /// R5e), or that a getter or setter writes (its value is not stated):
    /// `open` is also set, for the readers that only ask whether anything
    /// beyond `fields` may be in the object.
    pub unsettled: BTreeSet<String>,
    /// The keys the literal writes as a method (carrick#2741): a function,
    /// not a value any reader of `fields` takes. `open` is also set.
    pub functions: BTreeSet<String>,
    /// Something the source does not state may set any key not in `fields`: a
    /// spread of a value, or a computed key. A method, getter or setter with a
    /// plain key sets only that key.
    pub wide: bool,
}

/// What an object states about one key ([`ObjValue::key`]).
#[derive(Debug, PartialEq, Eq)]
pub enum KeyState<'a> {
    /// The source states the key's value.
    Known(&'a Value),
    /// No spread or computed key can set the key: it is not there.
    Absent,
    /// The literal writes the key as a method: a function, not a value.
    Function,
    /// A spread or a computed key may set the key, or a getter or setter
    /// writes it; the source does not say to what.
    Unread,
}

impl ObjValue {
    /// What this object states about `key`. Only a prefix slot and a payload
    /// slot's key read this (carrick#2474 R5e, carrick#2674); body and name
    /// readers keep `open`.
    pub fn key(&self, key: &str) -> KeyState<'_> {
        match self.fields.get(key) {
            Some(value) => KeyState::Known(value),
            None if self.functions.contains(key) => KeyState::Function,
            None if self.wide || self.unsettled.contains(key) => KeyState::Unread,
            None => KeyState::Absent,
        }
    }

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
                    self.functions.remove(key);
                    self.unsettled.insert(key.clone());
                    self.open = true;
                }
            }
        }
    }
}

impl Value {
    fn opaque(text: String, end: Root) -> Self {
        Value::Str(vec![Piece::Opaque {
            text,
            end,
            link: None,
        }])
    }

    /// An opaque value that ends at the hole `reason` names.
    fn hole(text: String, reason: HoleReason) -> Self {
        Self::opaque(text, Root::Hole(reason))
    }

    /// This value as string pieces. An object or a function in a string
    /// position is not text the source states.
    fn pieces(&self) -> Vec<Piece> {
        match self {
            Value::Str(pieces) => pieces.clone(),
            Value::Obj(_) | Value::Callback(_) => vec![Piece::Unknown],
        }
    }

    /// This value as a function written inside the one that holds it reads
    /// it: every parameter piece one function further out (carrick#1949).
    fn enclosed(&self) -> Value {
        match self {
            Value::Str(pieces) => Value::Str(pieces.iter().map(Piece::enclosed).collect()),
            Value::Obj(obj) => Value::Obj(ObjValue {
                fields: obj
                    .fields
                    .iter()
                    .map(|(key, value)| (key.clone(), value.enclosed()))
                    .collect(),
                open: obj.open,
                unsettled: obj.unsettled.clone(),
                functions: obj.functions.clone(),
                wide: obj.wide,
                // A closure's object reads as it always did.
                under_param: None,
                under_rest: None,
            }),
            Value::Callback(index) => Value::Callback(*index),
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
    /// A key of an enclosing function's parameter (carrick#1949): how many
    /// functions out, its position there, and the key. `fetch(url, init)` in
    /// a closure, with `init` the enclosing function's parameter. Becomes
    /// [`MethodValue::ParamKey`] once lifted into that function.
    Outer(u8, usize, String),
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
    /// The call reads as a request only on an options bag a response's init
    /// could equally be (`redirect(path, { headers })`, carrick#1986): no
    /// `fetch`, no verb, no key only a request carries. Such a call is a
    /// request only where something else the source states says so
    /// ([`Composer::request_of`]).
    init_only: bool,
    /// The verbs the source declares the method parameter can be, when the
    /// method is a parameter whose annotation is a closed set of them
    /// (carrick#2049). Empty for every other request.
    declared_methods: Vec<String>,
    /// How a conditional in the URL or the method splits the request
    /// (carrick#2050, carrick#2051).
    alternation: Alternation,
}

impl RequestShape {
    /// Whether the request is stated at its own line though its URL is
    /// written at the call. The source says it is more than one request: the
    /// method is a parameter declared as a closed set of verbs
    /// (carrick#2049), or a conditional chooses the URL or the method (carrick#2050,
    /// carrick#2051). That
    /// is one row per request, a count the
    /// single row of a model cannot hold. Only a URL that starts with its
    /// path is stated here: one that leads with a base is read by the passes
    /// that settle a base, and keeps their row.
    fn fans_out_inline(&self) -> bool {
        let path_first =
            |url: &[Piece]| matches!(url.first(), Some(Piece::Lit(text)) if text.starts_with('/'));
        match &self.alternation {
            // A way a caller's test decides is the caller's to state.
            Alternation::Rows(rows, _) if rows.len() > 1 => rows
                .iter()
                .all(|(_, url, guards)| path_first(url) && guards.is_empty()),
            Alternation::Rows(..) | Alternation::Single | Alternation::Nothing => {
                !self.declared_methods.is_empty() && path_first(&self.url)
            }
        }
    }
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
    /// The module-scope `let` that holds the instance, when a getter built
    /// it there on first use and returns that one object to every caller
    /// (carrick#1790, [`Reader::lazy_clients`]). Only the
    /// instance a `return` of such a `let` reads carries it.
    shared: Option<String>,
}

impl ClientRef {
    /// This client, held by a binding the file uses as `used`.
    fn used_as(self, used: &BindingUse) -> Self {
        self.used_apart(used, used)
    }

    /// This client, held by a binding the file uses as `own`, among the
    /// uses of every binding of its name, `by_name` (carrick#3547 S2). The
    /// HTTP reading's fields read `by_name`, as the old path always has
    /// (R4); the message roles' fields read `own`.
    fn used_apart(mut self, by_name: &BindingUse, own: &BindingUse) -> Self {
        self.contested |= by_name.contests_client();
        self.called = by_name.called.keys().cloned().collect();
        self.called_computed = by_name.called_computed;
        self.contested_message |= own.contests_message();
        self.member_uses = own.member_uses().collect();
        self.returned.extend(own.returned_at.iter().copied());
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
    /// The source waits on what the call returns: it is awaited, or handed
    /// to `.then`, `.catch` or `.finally` (carrick#1986).
    waited: bool,
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
    /// The position of the first spread argument (carrick#2662): a decided
    /// site reads the arguments before it as written, and every position
    /// from it on as unread. `None` without a spread.
    spread_from: Option<usize>,
    /// The line of the member the call names, or of the callee: where a
    /// library call a caller fills is placed ([`LibrarySite::line`]).
    name_line: u32,
    /// A JSX element rendering a component, read as a call of it with its
    /// attributes as one props object (carrick#1949): what the element's
    /// props fill in of the component's requests, and nothing else
    /// ([`Composer::compose`]).
    jsx: bool,
    /// Each argument as a guard reads where its value comes from
    /// (carrick#2386): `None` for a function written in place, and for
    /// every position from a spread on.
    arg_subjects: Vec<Option<Subject>>,
    /// The callee, read the same way: the parameter, import or global a
    /// function handed to this call is handed to.
    callee_subject: Option<Subject>,
    /// The callee as written.
    callee_text: String,
    /// What the callee may hold, when it is a value rather than a name the
    /// rows' call target reads (carrick#2432): a parameter, a key of one, a
    /// local holding one, each side of a fallback. Read by discovery alone.
    callee_values: Vec<ValueAt>,
    /// The member the callee reads, `object.key`, when the object is no
    /// parameter alone ([`ValueAt::KeyOf`], carrick#2448). Read by
    /// discovery where the rows name no definition for the call.
    callee_key: Option<ValueAt>,
    /// What each argument may hold, by position (carrick#2432): the values a
    /// parameter of the callee is handed. A spread is an empty entry, and
    /// the last ([`values`]). Read by discovery alone.
    arg_values: Vec<Vec<ValueAt>>,
    /// Each argument as the text it states ([`resolve::args_text`],
    /// carrick#3504 B10-1a): the terms a caller hands a parameter, read by
    /// position through [`Self::passed_texts`]. `None` at the first spread,
    /// which ends the list; empty for a JSX element, whose props are no
    /// positional argument. Read by the text reader's caller mode (B10-1b).
    arg_texts: Vec<Option<ValueAt>>,
    /// What each argument after the first spread may hold, a spread an
    /// empty entry ([`Reader::rest_values`]): a member that merges every
    /// argument from a slot on reads them past a spread (carrick#3538).
    /// Read by the procedures walk alone.
    rest_values: Vec<Vec<ValueAt>>,
    /// Each argument's span before the first spread, read past parentheses,
    /// type assertions and an `await` ([`arg_spans`]): which expression is
    /// written in place at a position. Read by the procedures walk alone
    /// (carrick#3538).
    arg_spans: Vec<(u32, u32)>,
    /// The call is what an `if`, a ternary or a `&&` tests: the branch its
    /// result guards.
    tested: Option<Tested>,
    /// Each name of a function this module declares that an argument hands
    /// on, by position, with the keys down to it inside the argument, as
    /// the syntax reads it. Read only by [`decided_sites::Calls`]'s build,
    /// where the compiler's rows give no read at that argument and key
    /// (carrick#4176): every other reader reads that one record.
    handed_names: Vec<(usize, Vec<String>, String)>,
    /// The text each argument before any spread states, or a field of an
    /// object-literal argument does, as the value flow reads names
    /// (carrick#2386): what a call that names an operation names. Read by
    /// the chains alone.
    names: Vec<guards::ArgName>,
    /// Each argument as a spread of the callee's parameter puts it in place
    /// (carrick#2411): empty for an element.
    spread_args: Vec<SpreadArg>,
    /// Each object the callee's member chain is read on, innermost first,
    /// as its span (discovery numbering) and whether it is a member access
    /// rather than a name (`this.child` then nothing for
    /// `this.child.send()`): where a compiler row names the receiver's type
    /// (carrick#2431), and the key a member access reads as written
    /// ([`discovery::receiver_spans`]). Read by discovery alone.
    receiver_spans: Vec<(u32, u32, bool, Option<String>)>,
    /// The object the callee's member is read on, whatever it is written
    /// as: its span (discovery numbering) and form. `None` when the callee
    /// is no member access. Read by the receiver trace alone (carrick#2474).
    receiver_object: Option<(u32, u32, discovery::ObjectForm)>,
    /// What the receiver trace walks an instance from (carrick#2474 R2a).
    /// Read by the trace alone.
    instance: InstanceIr,
    /// A `new C(…)` or a `super(…)`: the call runs the constructor of the
    /// class its row names (carrick#3527, [`FileIr::constructor_at`]).
    constructs: bool,
    /// `f.call(t, a)` or `f.apply(t, [a])`: the call runs `f` with the
    /// arguments shifted past `this`, once the compiler's row names the
    /// member as the language library's ([`LanguageCall::decided`],
    /// carrick#3504 S4d). `None` for any other callee.
    language_call: Option<Box<LanguageCall>>,
    /// The arguments a `bind` the call's callee was read through bound ahead
    /// of the call's own (carrick#3504 S4d-b). Empty as written; set on the
    /// copy the decided step reads a discovery site's chain through
    /// ([`Self::with_bound`]).
    bound: Vec<Value>,
    /// The tests of the body's parameters every path to the call takes, as
    /// a caller decides them (carrick#4093, [`Reader::path_guards`]): a
    /// request stated through the call is sent only where each goes the
    /// path's way ([`Effect::path_guards`]).
    path_guards: Vec<WayGuard>,
}

/// A call written as a function's `call` or `apply` member: what the
/// function may hold and the arguments it is handed (carrick#3504 S4d).
/// Read as a call of the function only where [`Self::decided`] is set:
/// a user object's own `call` method is a member call like any other.
#[derive(Debug, Clone)]
struct LanguageCall {
    /// What the function the member is read on may hold.
    callee: Vec<ValueAt>,
    /// `apply`'s arguments, the elements of the array written as its
    /// second argument; `None` for `call`, whose arguments are the call's
    /// own from position 1.
    applied: Option<Applied>,
    /// The compiler's row at the call names the language library's `call`
    /// or `apply` ([`RequestSummaryInputs::read_language_calls`]).
    decided: bool,
}

/// What `f.apply(t, [a, b])` hands `f`, by position, as [`CallIr::args`]
/// and [`CallIr::arg_values`] hold a call's own: the array literal's
/// elements, every position from a spread or a hole on unread. Anything
/// but an array literal hands nothing the scan reads.
#[derive(Debug, Clone, Default)]
struct Applied {
    args: Vec<Value>,
    /// The first position no element states (a spread, a hole, or `0` for
    /// no array literal): it and every one after it are unread.
    open_from: Option<usize>,
    /// What each element may hold; an empty entry from `open_from` on.
    arg_values: Vec<Vec<ValueAt>>,
    /// What each element states as text; `None` at `open_from`, where the
    /// list ends, and for an array literal that is no literal.
    arg_texts: Vec<Option<ValueAt>>,
}

impl CallIr {
    /// The argument the function this call runs receives at `index`, read
    /// the way the call writes it: shifted past `this` for a decided
    /// `call`, an element for a decided `apply`. `None` where no argument
    /// is written there or a spread before it moves it.
    fn passed_arg(&self, index: usize) -> Option<&Value> {
        // Bound arguments fill the leading parameters, the call's own the
        // rest (carrick#3504 S4d-b).
        if let Some(bound) = self.bound.get(index) {
            return Some(bound);
        }
        let index = index.saturating_sub(self.bound.len());
        match self.language_call.as_deref() {
            Some(LanguageCall {
                decided: true,
                applied: Some(applied),
                ..
            }) => match applied.open_from {
                Some(from) if index >= from => None,
                _ => applied.args.get(index),
            },
            Some(LanguageCall {
                decided: true,
                applied: None,
                ..
            }) => self.own_arg(index + 1),
            _ => self.own_arg(index),
        }
    }

    /// Whether the arguments of this `f.bind(this, a, …)` call after `this`
    /// are all written, as text no parameter of the function that writes the
    /// call changes: no spread, no function, no object. Those are the
    /// arguments the call bound ahead of the calls of its result
    /// (carrick#3504 S4d-b); anything else is not read.
    fn args_are_closed_text(&self) -> bool {
        self.site_args.is_some()
            && self.args.len() > 1
            && self.args[1..].iter().all(|arg| match arg {
                Value::Str(pieces) => pieces.iter().all(|piece| !piece.is_parameter()),
                _ => false,
            })
    }

    /// This call as one whose callee was read through `bind`: the `bind`
    /// call's arguments after `this` fill the parameters ahead of its own
    /// ([`Self::passed_arg`]). Only for a `bind` that
    /// [`Self::args_are_closed_text`] holds for.
    ///
    /// The bound copy keeps the call's own [`Self::path_guards`].
    fn with_bound(&self, bind: &CallIr) -> CallIr {
        CallIr {
            bound: bind.args.iter().skip(1).cloned().collect(),
            ..self.clone()
        }
    }

    /// The argument this call writes at `index`, unless a spread before it
    /// moves every position. With the receiver trace on, a spread leaves
    /// the arguments before it as written ([`CallIr::spread_from`]).
    fn own_arg(&self, index: usize) -> Option<&Value> {
        match (&self.site_args, self.spread_from) {
            (Some(_), _) => self.args.get(index),
            (None, Some(from)) if index < from => self.args.get(index),
            _ => None,
        }
    }

    /// Whether the call leaves position `index` out: it writes fewer
    /// arguments, and no spread may fill it. The callee's default is what
    /// it holds there (carrick#3504 S2b-2).
    fn omits(&self, index: usize) -> bool {
        self.args.len() <= index && self.spread_from.is_none()
    }

    /// What each argument the function this call runs receives may hold,
    /// by position, as [`CallIr::arg_values`] states a call's own, shifted
    /// as [`Self::passed_arg`] shifts them.
    fn passed_values(&self) -> &[Vec<ValueAt>] {
        self.passed_by_position(
            &self.arg_values,
            |applied| applied.arg_values.as_slice(),
            Vec::is_empty,
        )
    }

    /// What each argument the function this call runs receives states as
    /// text, by position, as [`CallIr::arg_texts`] states a call's own and
    /// shifted as [`Self::passed_values`] shifts them (carrick#3504 B10-1a).
    /// `None` is a spread, which ends the list.
    fn passed_texts(&self) -> &[Option<ValueAt>] {
        self.passed_by_position(
            &self.arg_texts,
            |applied| applied.arg_texts.as_slice(),
            Option::is_none,
        )
    }

    /// The text the function this call runs receives at `index`, as
    /// [`Self::passed_arg`] reads the value there: past the arguments a
    /// `bind` filled, shifted past `this` for a decided `call`, an element
    /// of a decided `apply`. `None` where nothing is written there, a spread
    /// moves it, or a `bind` filled it (its text is no term of this call).
    fn passed_text(&self, index: usize) -> Option<&ValueAt> {
        let index = index.checked_sub(self.bound.len())?;
        let texts = self.passed_texts();
        if texts.iter().take(index + 1).any(Option::is_none) {
            return None;
        }
        texts.get(index)?.as_ref()
    }

    /// One per-argument list of this call, `own`, as the function a decided
    /// `call` or `apply` runs receives it: an `apply`'s elements, or a
    /// `call`'s own past `this`. `spread` says an entry is a spread, which
    /// moves every position from it on.
    fn passed_by_position<'s, T>(
        &'s self,
        own: &'s [T],
        applied: impl Fn(&'s Applied) -> &'s [T],
        spread: impl Fn(&T) -> bool,
    ) -> &'s [T] {
        match self.language_call.as_deref() {
            Some(LanguageCall {
                decided: true,
                applied: Some(from_array),
                ..
            }) => applied(from_array),
            Some(LanguageCall {
                decided: true,
                applied: None,
                ..
            }) => match own.first() {
                // `f.call(...args)`: the spread moves every position.
                Some(this) if spread(this) => &own[..1],
                _ => own.get(1..).unwrap_or_default(),
            },
            _ => own,
        }
    }

    /// The function a decided `call` or `apply` runs, as what it may hold.
    fn language_callee(&self) -> Option<&[ValueAt]> {
        self.language_call
            .as_deref()
            .filter(|language| language.decided)
            .map(|language| language.callee.as_slice())
    }
}

/// What one argument holds, whole, and what each plain key of an object
/// literal written there holds (`{ app }`), as the value resolver lowers
/// them ([`Reader::value_of`], carrick#3504 S3): the receiver trace's walk
/// ends where a value names an instance.
#[derive(Debug, Clone, Default)]
struct ArgHeld {
    whole: Vec<ValueAt>,
    keys: BTreeMap<String, Vec<ValueAt>>,
    /// The argument's span (discovery numbering), where no spread comes
    /// before it: where a compiler row names what a name there declares
    /// (carrick#2474 R2b).
    span: Option<(u32, u32)>,
    /// Every way the value reads where the source states more than one
    /// (carrick#2635).
    alts: ArgAlts,
}

/// The values an argument may take, one list of pieces per way it reads,
/// where it holds a default (`opts.prefix ?? '/api'`, `a || b`, a defaulted
/// parameter) or a conditional (carrick#2635): the whole argument, and each
/// plain key of an object literal written there. A value with one reading
/// is not here. Read by the receiver trace alone.
#[derive(Debug, Clone, Default)]
struct ArgAlts {
    whole: Vec<Way>,
    keys: BTreeMap<String, Vec<Way>>,
}

/// One way a value reads (carrick#2635).
#[derive(Debug, Clone)]
struct Way {
    pieces: Vec<Piece>,
    /// A default: it applies only where the value written before it is
    /// missing (a caller omits it, or none is known), so it is stated only
    /// then.
    fallback: bool,
}

impl Way {
    fn stated(pieces: Vec<Piece>) -> Self {
        Way {
            pieces,
            fallback: false,
        }
    }

    fn defaulted(ways: Vec<Way>) -> impl Iterator<Item = Way> {
        ways.into_iter().map(|way| Way {
            fallback: true,
            ..way
        })
    }
}

/// What the receiver trace reads at one call (carrick#2474 R2a): the
/// receiver object's value as the body reads it (a parameter, a key of
/// one, an enclosing function's), what the receiver holds as the value
/// resolver lowers it (carrick#3504 S3), and each argument's.
#[derive(Debug, Clone, Default)]
struct InstanceIr {
    receiver_value: Option<Value>,
    receiver: Vec<ValueAt>,
    args: Vec<ArgHeld>,
    /// The argument slots whose text [`RequestSummaryInputs::linked`] read
    /// through a module-scope constant, piece by piece as they read before
    /// the read made them one text (address chain S1): where a prefix is
    /// read from, the trace stores each such piece with the module that
    /// declares it. Read by the trace alone.
    folds: Vec<SlotFold>,
}

/// One argument slot (an argument, or a key of an object-literal one) whose
/// text holds a piece read through a module-scope constant
/// ([`InstanceIr::folds`]).
#[derive(Debug, Clone)]
struct SlotFold {
    arg: usize,
    key: Option<String>,
    pieces: Vec<FoldPiece>,
}

/// A piece of a [`SlotFold`]: text written at the call, or text read
/// through a constant, with the module that declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FoldPiece {
    Lit(String),
    Folded { text: String, from: PathBuf },
}

/// What a call's argument puts in place of the callee's spread of that
/// parameter in a body it sends ([`ObjValue::under_param`], carrick#2411).
#[derive(Debug, Clone, PartialEq, Eq)]
enum SpreadArg {
    /// The caller's own parameter, by position: its callers fill the spread.
    Param(usize),
    /// The objects it may be, every key of each stated
    /// ([`Reader::spread_branches`]).
    Keys(Vec<ObjValue>),
    /// Anything else, which may carry any key.
    Unread,
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
    /// Each write `this.<field> = value` an instance member makes, with
    /// what the value may hold: the class's field reader takes them out
    /// (carrick#3504 S4c, [`library_sites::take_field_values`]).
    field_values: Vec<resolve::FieldWrite>,
    /// Each plain assignment of a name written directly in this body, with
    /// what the value may hold read in its scope: a module binding's writes
    /// the value resolver reads (carrick#3504 S1).
    binding_writes: Vec<resolve::BindingWrite>,
    /// Each `let` or `var` this body declares once and writes again, with
    /// its initialiser read in its scope: the binding table takes it under
    /// this function's key (carrick#3504 S1b).
    local_bindings: Vec<resolve::LocalBinding>,
    /// Parameters the body calls (`produce()`), by position.
    invoked_params: BTreeSet<usize>,
    /// The decorators on this method that call a name no library export
    /// answers to, written as the call `@Name(…)`
    /// ([`own_decorators::OwnDecorator`], carrick#3688 C1).
    own_decorators: Vec<own_decorators::OwnDecorator>,
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
    /// What the function returns when that is one call's value read one way
    /// ([`crate::return_flow::return_shapes`], carrick#1601, carrick#3823):
    /// the one reader of whether it hands back its request's parsed body.
    /// `None` for anything else, or nothing returned.
    return_shape: Option<crate::return_flow::ReturnShape>,
    /// Each parameter by position, as a body sent through it is read
    /// (carrick#1782): where its name is written, whether its declaration
    /// states a type, and whether the body assigns it again. `None` for a
    /// destructured or rest parameter.
    params: Vec<Option<ParamDecl>>,
    /// The type parameters in scope at the declaration, the function's own
    /// and, for a class member, the class's: a parameter typed through one is
    /// typed by whatever the caller passes.
    type_params: HashSet<String>,
    /// Where the function node is written: the key that names a member of an
    /// exported object, whose function is read as a detached one
    /// (carrick#2386).
    site: Option<Site>,
    /// What the body compares a request value against (carrick#2386).
    guards: Vec<GuardIr>,
    /// For a function written as a call's argument, or inside one: the call,
    /// the argument and the keys down to it.
    written_in: Option<WrittenIn>,
    /// Whether each `return` (an arrow's expression body included) hands
    /// back a test: a comparison, a negation, a logical expression, a call
    /// or a boolean.
    returns_tests: Vec<bool>,
    /// Each `new X(…)` of a constructor that is not the language's own, read
    /// as a call is, for the chains alone (carrick#2386): the send side never
    /// reads it, so a construction composes into no summary.
    constructions: Vec<CallIr>,
    /// Each optional call (`socket?.emit(…)`), read as a call is, for
    /// discovery and the rows' call targets alone (carrick#2431): the
    /// summaries compose none, so the body stays [`Self::unfollowed`].
    optional_calls: Vec<CallIr>,
    /// Each default a parameter, or a key of a destructured one, is given:
    /// its position, the keys down to it and what it may hold, read in the
    /// function's own scope (carrick#2432). Read by discovery alone.
    param_defaults: Vec<(usize, Vec<String>, Vec<ValueAt>)>,
    /// The same defaults as the text they state, for the caller walk to fill
    /// an omitted argument with (carrick#3504 S2b-2), with the keys down to
    /// each (empty for a whole parameter's).
    param_default_texts: Vec<(usize, Vec<String>, ValueAt)>,
    /// Every plain name the parameters bind, where each is declared
    /// ([`resolve::param_bindings`] keys the file's parameter table from
    /// these, carrick#3504 B10-1a).
    param_names: Vec<resolve::ParamName>,
    /// Each key of a parameter with a default whose text the function's own
    /// scope reads (`{ version = 'v1' }`, `const { method = 'POST' } =
    /// options || {}`), by position and key: what a request the function
    /// sends reads where a caller's object leaves the key out
    /// ([`Effect::key_defaults`], carrick#3608).
    key_defaults: BTreeMap<(usize, String), Vec<Piece>>,
    /// Each function written as a parameter's default, read with this
    /// function's own captures (carrick#2813). A value that is one reaches it
    /// here, by where it starts. Discovery's value walk enters it; no site
    /// walk does, so the call inside it is no lead of its own.
    param_functions: Vec<FnIr>,
    /// What each `return` (an arrow's expression body included) may hand
    /// back, for a callee an own factory returns (carrick#2432). Read by
    /// discovery alone.
    returned_values: Vec<ValueAt>,
    /// Each request this body builds and returns for a caller to send
    /// (carrick#3470, [`descriptors`]). Read by that pass alone.
    built_requests: Vec<descriptors::BuiltRequest>,
    /// A class constructor's body, keyed `<Class>.constructor` in
    /// [`FileIr::functions`] (carrick#3527): read by discovery and the
    /// decided sites, which reach it from a `new` or a `super(...)` through
    /// [`FileIr::constructor_keys`]. The old path's summaries and the
    /// construction reader skip it.
    constructor: bool,
}

impl FnIr {
    /// The calls discovery and the rows' call targets read in this body:
    /// its calls, its constructions and its optional calls, each with
    /// whether it is a construction.
    fn read_calls(&self) -> impl Iterator<Item = (&CallIr, bool)> {
        self.calls
            .iter()
            .map(|call| (call, false))
            .chain(self.constructions.iter().map(|call| (call, true)))
            .chain(self.optional_calls.iter().map(|call| (call, false)))
    }
}

/// `expr` with its types, parentheses and `await` taken off.
fn unwrap_value(expr: &Expr) -> &Expr {
    match crate::graphql_document_sites::unwrap_expression(expr) {
        Expr::Await(awaited) => unwrap_value(&awaited.arg),
        other => other,
    }
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
    /// `effect` as this body sends it (carrick#1782): a body sent through a
    /// parameter the body assigns again is not what the caller passed, so
    /// the effect states no body a caller writes.
    fn settle_forward(&self, mut effect: Effect) -> Effect {
        if let Some((index, None)) = effect.body_param
            && self
                .params
                .get(index)
                .and_then(Option::as_ref)
                .is_some_and(ParamDecl::assigned_again)
        {
            effect.body_param = None;
        }
        effect
    }

    /// What the body is at a call of this function that sends `effect`, an
    /// effect of this function's own summary (carrick#1782).
    ///
    /// The function writes the body itself or sends none: the call's
    /// arguments are not the body. It sends one of its parameters unchanged:
    /// the argument in that position is, typed from the parameter's
    /// declaration where that states a type. It forwards an options object
    /// the caller writes the body into: the call's own payload is the body,
    /// as on a request's own line.
    fn call_body(&self, effect: &Effect) -> Option<CallBody> {
        match &effect.body_param {
            None => Some(CallBody::Built),
            Some((_, Some(_))) => None,
            Some((index, None)) => self
                .params
                .get(*index)?
                .as_ref()?
                .forwarded(&self.type_params),
        }
    }

    /// Settle `returns` into `returned`: an instance on every `return`, of
    /// one or more makers, each handed its own arguments (carrick#1689; one
    /// maker before). How many makers that comes to once own calls are
    /// followed is the reader's to judge ([`library_sites::MAX_MAKERS`]). A
    /// generator returns no instance to its caller, and a function that
    /// returns a lazily built client on one path (carrick#1790) and anything
    /// else on another returns no one instance.
    fn settle_returned(&mut self, is_async: bool, is_generator: bool) {
        let returns = std::mem::take(&mut self.returns);
        if is_generator || returns.is_empty() || returns.iter().any(|(_, made)| made.is_empty()) {
            return;
        }
        let mut shared = returns
            .iter()
            .flat_map(|(_, made)| made)
            .map(|client| &client.shared);
        if let Some(first) = shared.next()
            && shared.any(|other| other != first)
        {
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
    /// What each module-scope binding left out of `module_clients` because
    /// a scope below declares its name again holds (carrick#2474 R2b): read
    /// only by the receiver trace, through the declaration the compiler
    /// names at a use, which tells the two bindings apart.
    /// Published or not: a published one stays dropped for library sites
    /// (carrick#3559 R3).
    shadowed_clients: HashMap<String, ClientRef>,
    /// The dropped module-scope clients a library site may read at a use that
    /// names no declaration below ([`Scope::site_binding`], carrick#3559),
    /// with the uses of their own binding. Linked beside `module_clients`,
    /// which the old path reads and this never joins.
    dropped_clients: HashMap<String, ClientRef>,
    /// Each module-scope `const` whose initialiser is only a name
    /// (`export const alias = web`), by binding, with that name: read only
    /// by the receiver trace, which follows an import of `alias` to the
    /// instance `web` holds ([`Self::module_instance`], carrick#2474 R3).
    module_aliases: HashMap<String, String>,
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
    /// The functions written as a default of a class constructor's parameter,
    /// which no function definition holds (carrick#2813).
    constructor_defaults: Vec<FnIr>,
    /// Each class field that holds one maker's instance as a message role
    /// reads it (carrick#1665), keyed by its class (the class's span start)
    /// and the field. An HTTP reading reads the class's own field table.
    field_receivers: HashMap<(u32, String), ClientRef>,
    /// Each getter an instance member of a class declares, by the class's span
    /// start and the member's name: the key of its function in
    /// [`Self::functions`] (carrick#3489 W4).
    class_getters: HashMap<(u32, String), String>,
    /// What each module-scope binding a name may read through holds
    /// (carrick#1562): a `const` whose initialiser is text, a constant
    /// object this module keeps ([`BindingUse::keeps_entries`]), or a
    /// builder. Read by name, for this module's names and every importer's.
    names: HashMap<String, library_sites::NameValue>,
    /// Each function declared at module scope that returns the client a
    /// getter builds into a module `let` on first use (carrick#1790), by
    /// name, with how this module uses its binding: what accounts for every
    /// call of the getter (carrick#1798, [`library_sites`]'s
    /// `share_lazy_clients`). A class member that returns one is no binding
    /// a call names, so it is not here.
    getters: HashMap<String, OwnGetter>,
    /// Each module-scope binding a plain name declares, by the 1-based line
    /// its name is declared on and the name, as a compiler row names it:
    /// its initialiser and every write (the value resolver's binding table,
    /// carrick#3504 S1).
    module_bindings: HashMap<(u32, String), resolve::ModuleBinding>,
    /// Each name a parameter of any function of the file binds, by the
    /// 1-based line its name is declared on and the name, as
    /// [`Self::module_bindings`] is keyed: its function, position and keys
    /// (carrick#3504 B10-1a). A table of its own, never read by a value
    /// reader; the text reader reads a parameter through its callers from it.
    param_bindings: HashMap<(u32, String), resolve::ParamAt>,
    /// The pieces [`Reader::eval`] read each expression as while it read a
    /// call's arguments (a call's, an optional call's, a construction's and
    /// an `apply`'s elements) and the initialiser of each local a body
    /// declares, by the expression's span (discovery numbering, as
    /// [`CallIr::arg_texts`] spans its terms; carrick#3504 B10-2a1). An
    /// expression read as no text has none. Linked as every other list of
    /// pieces is ([`Self::visit_pieces`]). No reader reads it yet: it is how
    /// a part of a name the resolver reads is spelled as the summaries spell
    /// it.
    arg_pieces: HashMap<(u32, u32), Vec<Piece>>,
    /// Each import (by its local name) this module only reads a member of
    /// and never changes ([`resolve::module_bindings`], carrick#2448).
    kept_imports: HashSet<String>,
    /// Where each name of a binding this module writes through starts
    /// (`api.defaults.baseURL = u` names `api`), from the one write census
    /// ([`resolve::module_bindings`], carrick#3504 S5b).
    written_through_at: HashSet<(u32, u32)>,
    /// The write census's verdict on every binding of the module, for a
    /// local the binding table does not hold ([`resolve::PlacedWrites`],
    /// carrick#3504 S7b-1).
    placed_writes: resolve::PlacedWrites,
    /// Where this file starts in the discovery source map
    /// ([`Self::swc_start`]).
    source_start: u32,
    /// Each definition the extractor keyed in this file, with where its
    /// function node starts and whether the module exports it
    /// ([`Self::place_definitions`], carrick#2386).
    definitions: HashMap<String, (Option<u32>, bool)>,
    /// Each expression-position use of an import binding the file uses, by
    /// name, as its span (discovery numbering): where a compiler row says
    /// which declaration the use reaches (carrick#2413). Name-keyed as
    /// [`Self::imported`] is, so a name declared again below lands here too,
    /// and its uses then name more than one declaration.
    import_uses: HashMap<String, Vec<(u32, u32)>>,
    /// Each module-scope binding [`Self::names`], [`Self::module_clients`]
    /// and [`Self::getters`] may hold, by the name the compiler gives its
    /// symbol, with the 1-based line its declaration starts on and the name
    /// those tables key it by ([`module_declarations`]): where a compiler
    /// row's declaration joins this file (carrick#2413 slice B).
    declared_at: HashMap<String, (u32, String)>,
    /// Each module-scope function's span (`span_start`, `span_end`, in the
    /// file's own SWC numbering), by binding: where a decorator factory's
    /// body is ([`own_decorators`], carrick#3688 C1).
    function_spans: HashMap<String, (u32, u32)>,
    /// The file loads a module by a specifier it computes (`require(name)`,
    /// `import(name)`), which may be any module of the service
    /// ([`module_loads`]).
    computed_load: bool,
    /// The declarations this file holds that no definition the extractor
    /// keys opens at: an overload's signatures, an interface's members
    /// (carrick#2413 slice D).
    declaration_lines: row_calls::DeclarationLines,
    /// Each line a definition the extractor keyed opens on, or the compiler
    /// starts it on, with the definitions there ([`Self::place_definitions`]):
    /// where a compiler row's declaration joins a definition (carrick#2413
    /// slice D).
    definition_lines: HashMap<u32, Vec<String>>,
    /// Each import or `require` binding the file holds as bound, by its
    /// local name: what the module publishes it as and the specifier, the
    /// names the library store keys an export by (carrick#2431). A binding
    /// assigned again or declared again below is left out.
    bindings: HashMap<String, ImportBinding>,
    /// What this module's own reading makes of each module `const` that
    /// [`Self::names`] states nothing for, read uncalled: what an importer
    /// reads where the text link fails (carrick#2540).
    held: HashMap<String, Held>,
    /// The same for each module builder with no [`Self::names`] entry, read
    /// called (carrick#2540).
    held_returns: HashMap<String, Held>,
    /// Each module-scope class's fields its constructor sets from a
    /// parameter, by the class's span start (carrick#2452).
    class_origins: HashMap<u32, ClassOrigin>,
    /// Every `new` the file writes, wherever it is, and every class it
    /// extends ([`constructions::NewsAndBases`], carrick#2452).
    news: constructions::NewsAndBases,
    /// Each line a class with a constructor body is declared on (its
    /// decorators, the class and its name) and the constructor's own line,
    /// with the class's name and the key of its constructor in
    /// [`Self::functions`]: where a compiler `call` row for `new C(…)` or
    /// `super(…)`, which names the class, joins the constructor it runs
    /// (carrick#3527, [`Self::constructor_at`]).
    constructor_keys: HashMap<u32, Vec<(String, String)>>,
    /// Each function-local binding [`Scope::held`] holds, by its
    /// initialiser's span (discovery numbering, the initialiser read past
    /// an `await`): a use of the name is read as the initialiser's values,
    /// so these say which binding a value the procedures walk reaches is
    /// held in (carrick#3538 M1). Read by that walk alone.
    held_inits: HashMap<(u32, u32), HeldInit>,
    /// FALLBACK (carrick#3794, deleted when block-scoped and nested-function
    /// locals are in the one binding table): each plain declarator written
    /// in a nested block, by the line its name is declared on and the name,
    /// which [`Self::module_bindings`] does not hold. Read by the
    /// procedures walk alone, and each read is counted.
    block_bindings: HashMap<(u32, String), BlockBinding>,
    /// FALLBACK (carrick#3794): what each named function of the file returns
    /// (a declaration, or a declarator whose initialiser is a function), by
    /// the line its name is declared on and the name: for a function no
    /// definition key joins. Read by the procedures walk alone, counted.
    block_returns: HashMap<(u32, String), Vec<ValueAt>>,
}

/// Text a link resolves to, and what a binding hands an importer where it
/// does not, each by the importing file and the binding it names.
type LinkedReads = (
    HashMap<(PathBuf, library_sites::NamedRef), String>,
    HashMap<(PathBuf, library_sites::NamedRef), Held>,
    // The module that declares each linked binding (address chain S1).
    HashMap<(PathBuf, library_sites::NamedRef), PathBuf>,
);

/// What a module-scope binding hands an importer where its text cannot be
/// read by name (carrick#2540): the declaring module's own reading of it.
/// The importer reads the binding as the declaring module does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Held {
    /// Every value is the one written text.
    Text(String),
    /// Values the source does not write, then the one written path every
    /// value ends in ([`shared_written_tail`]).
    Tail(String),
    /// The value reads as more than one written path (carrick#2541): no
    /// importer keys a row on it.
    SeveralPaths,
}

/// Every expression-position identifier among `names`, by name, with its
/// span: the positions a compiler row is written at. Type positions hold
/// none.
struct ExpressionIdents<'a> {
    names: &'a HashMap<String, BindingUse>,
    spans: HashMap<String, Vec<(u32, u32)>>,
}

impl Visit for ExpressionIdents<'_> {
    fn visit_ts_type(&mut self, _: &TsType) {}
    fn visit_ts_interface_body(&mut self, _: &TsInterfaceBody) {}
    fn visit_ts_expr_with_type_args(&mut self, _: &TsExprWithTypeArgs) {}

    fn visit_expr(&mut self, expr: &Expr) {
        if let Expr::Ident(ident) = expr
            && self.names.contains_key(ident.sym.as_ref())
        {
            self.spans
                .entry(ident.sym.to_string())
                .or_default()
                .push((ident.span.lo.0, ident.span.hi.0));
        }
        expr.visit_children_with(self);
    }
}

/// A getter of a lazily built client as the module that declares it uses its
/// binding (carrick#1798).
#[derive(Debug, Clone, Default)]
struct OwnGetter {
    uses: BindingUse,
    /// The module publishes the binding: `export function`, `export {}`,
    /// `export default` or a CommonJS export. A module the scan cannot
    /// follow may then call it.
    published: bool,
}

impl FileIr {
    /// The instance the module-scope binding `name` holds, through each
    /// `const` that only names another binding ([`Self::module_aliases`]):
    /// `export const alias = web` holds what `web` holds (carrick#2474 R3).
    fn module_instance(&self, name: &str) -> Option<&ClientRef> {
        let mut name = name;
        let mut seen: Vec<&str> = Vec::new();
        loop {
            if let Some(client) = self
                .module_clients
                .get(name)
                .or_else(|| self.shadowed_clients.get(name))
            {
                return Some(client);
            }
            if seen.contains(&name) {
                return None;
            }
            seen.push(name);
            name = self.module_aliases.get(name)?;
        }
    }

    /// A span start this file's [`BindingUse`]s record (discovery
    /// numbering) in SWC numbering, as [`Site::span_start`] is.
    pub(crate) fn swc_start(&self, lo: u32) -> u32 {
        lo.saturating_sub(self.source_start) + SWC_SPAN_BASE
    }

    /// A span start in SWC numbering (a fresh parse of this file's source,
    /// as the records' reader makes) in discovery numbering: the inverse of
    /// [`Self::swc_start`].
    pub(crate) fn discovery_start(&self, lo: u32) -> u32 {
        lo.saturating_sub(SWC_SPAN_BASE) + self.source_start
    }

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

    /// Each import binding the file uses, with the span of every
    /// expression-position use of it in SWC numbering ([`Self::swc_start`]):
    /// where a compiler row names the declaration it reaches (carrick#2413).
    pub fn import_uses(&self) -> impl Iterator<Item = (&str, Vec<(u32, u32)>)> + '_ {
        self.import_uses.iter().map(|(name, spans)| {
            (
                name.as_str(),
                spans
                    .iter()
                    .map(|(lo, hi)| (self.swc_start(*lo), self.swc_start(*hi)))
                    .collect(),
            )
        })
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

/// The module-scope reading of one file's source, for a reader that has the
/// text and no scan around it (carrick#2740: a file route's compared
/// methods). `None` when the file does not parse. The empty key set names
/// class static members only, which that reader does not look up.
pub fn file_ir_of(file_path: &Path, content: &str) -> Option<FileIr> {
    let (source_map, module) = crate::swc_scanner::parse_standalone_module(file_path, content)?;
    Some(extract_file_ir(&module, &source_map, &HashSet::new()))
}

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
    // Names the module assigns again: a function declaration one of them
    // names may be replaced by the time it is called.
    let mut reassigned = Reassigned::default();
    module.visit_with(&mut reassigned);
    let classes = classes_by_name(module);
    let mut module_scope = ModuleScope {
        consts: HashMap::new(),
        texts: HashMap::new(),
        named: HashSet::new(),
        builders: HashMap::new(),
        // Read before any constant or body that may call one: a declaration
        // is hoisted.
        query_builders: module_query_builders(module, &reassigned),
        receivers: HashMap::new(),
        lazy_clients: HashMap::new(),
        dropped: HashMap::new(),
        destructured: HashSet::new(),
        lets: module
            .body
            .iter()
            .filter_map(module_var_decl)
            .filter(|var| var.kind != VarDeclKind::Const)
            .flat_map(|var| &var.decls)
            .filter_map(|declarator| match &declarator.name {
                Pat::Ident(ident) => Some(ident_key(&ident.id)),
                _ => None,
            })
            .collect(),
        imports: HashSet::new(),
        uses: uses.uses,
        binding_uses: uses.bindings,
        unbound_uses: uses.unbound,
        object_consts: object_consts(module),
        object_lits: HashMap::new(),
        subclass_fields: subclass_members(&classes, |class| &class.fields),
        subclass_methods: subclass_members(&classes, |class| &class.methods),
        class_this: library_sites::class_this(module),
        constructions: HashMap::new(),
        declared_below: redeclared_names(module),
        verb_aliases: crate::declared_verbs::TypeAliases::of(module),
        own_collections: {
            let declared = Declarations::of(module);
            COLLECTIONS
                .iter()
                .copied()
                .filter(|name| declared.declares_name(name))
                .collect()
        },
        guards: guards::ModuleGuards::default(),
    };
    let package_clients = import_receivers(&imports);
    module_scope.receivers = package_clients
        .iter()
        .map(|(name, client)| {
            let client = module_scope.with_uses(client.clone(), name, None);
            (name.clone(), client)
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
    // The imports that table leaves out, for library sites: each is read at
    // a use that names no declaration below, with the uses of its own
    // binding (carrick#3559).
    let dropped_imports: Vec<(String, Dropped)> = imports
        .iter()
        .filter(|(name, import)| {
            !import.namespace && !import.reassignable && redeclared.declares_name(name)
        })
        .map(|(name, import)| {
            let package = package_clients
                .get(name)
                .map(|client| module_scope.with_uses(client.clone(), name, Some(&import.key)));
            let site = ClientBinding::Imported {
                local: name.clone(),
                package,
            };
            (
                name.clone(),
                Dropped {
                    site: Some(site),
                    import: true,
                },
            )
        })
        .collect();
    module_scope.dropped.extend(dropped_imports);
    let value_specifiers = value_specifiers(module, |name| {
        module_scope.uses.contains_key(name) || jsx.contains(name)
    });
    let mut idents = ExpressionIdents {
        names: &imported,
        spans: HashMap::new(),
    };
    module.visit_with(&mut idents);
    let import_uses = idents.spans;
    let (loads, computed_load) = module_loads(module, &bound_requires);
    let bindings = imports
        .iter()
        .filter(|(name, import)| {
            !import.reassignable && !module_scope.declared_below.declares_name(name)
        })
        .map(|(name, import)| (name.clone(), import.clone()))
        .collect();
    let mut file = FileIr {
        bindings,
        import_uses,
        imported,
        value_specifiers,
        loads,
        computed_load,
        declared_at: module_declarations(module, source_map),
        declaration_lines: row_calls::DeclarationLines::of(module, source_map),
        news: constructions::NewsAndBases::of(module),
        names_commonjs_exports: names_commonjs_exports(module),
        source_start: source_map
            .try_lookup_byte_offset(module.span.lo)
            .map_or(0, |found| found.sf.start_pos.0),
        ..FileIr::default()
    };
    let reader = Reader {
        source_map,
        builder_depth: std::cell::Cell::new(0),
        returned_objects: crate::return_flow::returned_objects(module),
        return_shapes: crate::return_flow::return_shapes(module)
            .into_iter()
            .filter_map(|(node, shape)| Some(((node.lo.0, node.hi.0), shape?)))
            .collect(),
        recording_depth: std::cell::Cell::new(0),
        arg_pieces: std::cell::RefCell::new(HashMap::new()),
        held_inits: Default::default(),
        block_bindings: Default::default(),
        function_returns: Default::default(),
        way_tests: Default::default(),
    };
    module_scope.guards = guards::ModuleGuards::of(module, &imports, definition_keys, &reader);

    // A function declared at module scope whose body only returns text is a
    // builder (carrick#1562), unless the module assigns its binding again.
    // A declaration is hoisted, so every constant below reads it
    // (carrick#2536).
    for item in &module.body {
        if let Some(Decl::Fn(fn_decl)) = module_decl(item)
            && !reassigned.names.contains(fn_decl.ident.sym.as_ref())
            && let Some(builder) = Builder::of_function(&fn_decl.function)
        {
            module_scope
                .builders
                .insert((ident_key(&fn_decl.ident), Vec::new()), builder);
        }
    }
    // Module constants first, in source order, so a later one can read an
    // earlier one (`const BASE = ...; const USERS = `${BASE}/users``).
    for item in &module.body {
        // `export const …` publishes the bindings it declares.
        let declared_exported = matches!(item, ModuleItem::ModuleDecl(_));
        if let Some(decl) = module_var_decl(item)
            && decl.kind == VarDeclKind::Const
        {
            for declarator in &decl.decls {
                if let (Pat::Object(pattern), Some(init)) = (&declarator.name, &declarator.init) {
                    // `const { key: alias } = object`: each alias holds what
                    // `const alias = <the key's value>` would
                    // (carrick#3489 W2).
                    let found: Vec<(String, BindingKey, Option<ClientRef>, Option<String>)> = {
                        let scope = Scope::module(&module_scope);
                        destructured_keys(pattern, init, &module_scope)
                            .into_iter()
                            .map(|(alias, value)| {
                                (
                                    alias.0.to_string(),
                                    alias.clone(),
                                    reader.written_instance(value, &scope),
                                    match crate::graphql_document_sites::unwrap_expression(value) {
                                        Expr::Ident(target) => Some(target.sym.to_string()),
                                        _ => None,
                                    },
                                )
                            })
                            .collect()
                    };
                    for (name, key, client, target) in found {
                        module_scope.destructured.insert(name.clone());
                        match (client, target) {
                            (Some(client), _) => {
                                if redeclared.declares_name(&name) {
                                    let by_name =
                                        module_scope.with_uses(client.clone(), &name, None);
                                    let own = module_scope.with_uses(client, &name, Some(&key));
                                    let published =
                                        declared_exported || module_scope.published(&name);
                                    record_dropped(
                                        &mut module_scope.dropped,
                                        &mut file,
                                        name,
                                        by_name,
                                        own,
                                        published,
                                    );
                                } else {
                                    let client = module_scope.with_uses(client, &name, None);
                                    file.module_clients.insert(name.clone(), client.clone());
                                    module_scope.receivers.insert(name, client);
                                }
                            }
                            (None, Some(target)) => {
                                file.module_aliases.insert(name, target);
                            }
                            (None, None) => {}
                        }
                    }
                    continue;
                }
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
                    module_scope
                        .object_lits
                        .insert(ident_key(&ident.id), object.clone());
                }
                if !matches!(
                    crate::graphql_document_sites::unwrap_expression(init),
                    Expr::Arrow(_) | Expr::Fn(_)
                ) {
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
                    if client.is_none()
                        && let Expr::Ident(target) =
                            crate::graphql_document_sites::unwrap_expression(init)
                    {
                        file.module_aliases
                            .insert(name.clone(), target.sym.to_string());
                    }
                    if let Some(client) = client {
                        if redeclared.declares_name(&name) {
                            let by_name = module_scope.with_uses(client.clone(), &name, None);
                            let own =
                                module_scope.with_uses(client, &name, Some(&ident_key(&ident.id)));
                            let published = declared_exported || module_scope.published(&name);
                            record_dropped(
                                &mut module_scope.dropped,
                                &mut file,
                                name,
                                by_name,
                                own,
                                published,
                            );
                        } else {
                            let client = module_scope.with_uses(client, &name, None);
                            file.module_clients.insert(name.clone(), client.clone());
                            module_scope.receivers.insert(name, client);
                        }
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
    // A name may read through such a builder too. Read once every constant is
    // known, since the text it builds may name one.
    for item in &module.body {
        if let Some(Decl::Fn(fn_decl)) = module_decl(item)
            && !reassigned.names.contains(fn_decl.ident.sym.as_ref())
            && let Some(builder) = Builder::of_function(&fn_decl.function)
            && let Some(value) = library_sites::builder(&builder, &module_scope)
        {
            module_scope.named.insert(ident_key(&fn_decl.ident));
            file.names.insert(fn_decl.ident.sym.to_string(), value);
        }
    }
    // What an importer reads where `names` states nothing (carrick#2540): a
    // module constant, and a module builder called, as this module reads them.
    for item in &module.body {
        let Some(decl) = module_var_decl(item) else {
            continue;
        };
        if decl.kind != VarDeclKind::Const {
            continue;
        }
        for declarator in &decl.decls {
            let (Pat::Ident(ident), Some(init)) = (&declarator.name, &declarator.init) else {
                continue;
            };
            let name = ident.id.sym.as_ref();
            if matches!(
                crate::graphql_document_sites::unwrap_expression(init),
                Expr::Arrow(_) | Expr::Fn(_)
            ) || file.names.contains_key(name)
            {
                continue;
            }
            if let Some(value) = module_scope.consts.get(&ident_key(&ident.id))
                && let Some(held) = held_by(std::slice::from_ref(value))
            {
                file.held.insert(name.to_string(), held);
            }
        }
    }
    for ((key, path), builder) in &module_scope.builders {
        if !path.is_empty()
            || file.names.contains_key(&key.0)
            || builder.params.iter().any(Option::is_none)
        {
            continue;
        }
        // Each parameter reads as itself: a prefix the caller hands in is no
        // value the module states.
        let mut inner = Scope::module(&module_scope);
        for (index, param) in builder.params.iter().enumerate() {
            if let Some(param) = param {
                inner.locals.insert(
                    param.clone(),
                    Value::Str(vec![Piece::Param(index, param.0.clone())]),
                );
            }
        }
        let values: Vec<Value> = builder
            .returned
            .iter()
            .map(|returned| reader.eval(returned, &inner))
            .collect();
        if let Some(held) = held_by(&values) {
            file.held_returns.insert(key.0.clone(), held);
        }
    }
    // A client a getter builds into a module `let` on first use
    // (carrick#1790), read where a function returns it, once the module's
    // constants are known.
    module_scope.lazy_clients = reader.lazy_clients(module, &module_scope, &reassigned);
    module_scope.constructions = reader.constructions(module, &module_scope, &jsx, &reassigned);
    let module_scope = &module_scope;
    let none = Captured::default();

    // Each function a module-scope binding names, and whether the statement
    // that declares it publishes it (carrick#1798).
    let mut bound: HashMap<String, bool> = HashMap::new();
    // The span starts of the object members read as functions above, which
    // module scope does not read again (carrick#3356).
    let mut lifted_members: HashSet<u32> = HashSet::new();
    for item in &module.body {
        let declared_exported = matches!(item, ModuleItem::ModuleDecl(_));
        match module_decl(item) {
            Some(Decl::Fn(fn_decl)) => {
                let ir = reader.function(&fn_decl.function, None, module_scope, &none);
                let name = fn_decl.ident.sym.to_string();
                bound.insert(name.clone(), declared_exported);
                let at = reader.site(fn_decl.function.span);
                file.function_spans
                    .insert(name.clone(), (at.span_start, at.span_end));
                file.functions.insert(name, ir);
            }
            Some(Decl::Var(var)) => {
                for declarator in &var.decls {
                    let (Pat::Ident(ident), Some(init)) = (&declarator.name, &declarator.init)
                    else {
                        continue;
                    };
                    // A wrapper that only states a type (`(async () => {})
                    // satisfies ActionFunction`, `as`, `!`, parentheses) still
                    // binds the function itself (carrick#2740).
                    let span;
                    let ir = match crate::graphql_document_sites::unwrap_expression(init) {
                        Expr::Arrow(arrow) => {
                            span = arrow.span;
                            reader.arrow(arrow, None, module_scope, &none)
                        }
                        Expr::Fn(fn_expr) => {
                            span = fn_expr.function.span;
                            reader.function(&fn_expr.function, None, module_scope, &none)
                        }
                        // The functions a constant object holds (carrick#3356).
                        Expr::Object(object) => {
                            let name = ident.id.sym.as_ref();
                            if var.kind != VarDeclKind::Const
                                || !module_scope.object_consts.contains(name)
                                || module_scope.written_through(name)
                            {
                                continue;
                            }
                            lift_object_members(
                                &reader,
                                name,
                                object,
                                module_scope,
                                definition_keys,
                                &mut lifted_members,
                                &mut file,
                            );
                            continue;
                        }
                        // `const Api = class {…}` is the class `Api`; a named
                        // expression keeps its own name.
                        Expr::Class(class) => {
                            let own = class.ident.as_ref().unwrap_or(&ident.id);
                            reader.class(
                                own,
                                &class.class,
                                module_scope,
                                definition_keys,
                                &mut file,
                            );
                            continue;
                        }
                        _ => continue,
                    };
                    let name = ident.id.sym.to_string();
                    bound.insert(name.clone(), declared_exported);
                    let at = reader.site(span);
                    file.function_spans
                        .insert(name.clone(), (at.span_start, at.span_end));
                    file.functions.insert(name, ir);
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
                    let name = ident.sym.to_string();
                    bound.insert(name.clone(), true);
                    file.functions.insert(name, ir);
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
        // `export default { async fetch() {} }`: the extractor keys its
        // members `default.<member>` (carrick#830). No binding names the
        // object, so nothing writes through it (carrick#3370).
        if let ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(export)) = item
            && let Expr::Object(object) =
                crate::graphql_document_sites::unwrap_expression(&export.expr)
        {
            lift_object_members(
                &reader,
                "default",
                object,
                module_scope,
                definition_keys,
                &mut lifted_members,
                &mut file,
            );
        }
    }
    // A getter of a lazily built client (carrick#1790), with how this module
    // uses its binding, so that every call of it can be accounted for
    // (carrick#1798).
    for (name, declared_exported) in bound {
        let hands_out_shared = file
            .functions
            .get(&name)
            .and_then(|function| function.returned.as_ref())
            .is_some_and(|returned| returned.makers.iter().any(|made| made.shared.is_some()));
        if hands_out_shared {
            let uses = module_scope.uses.get(&name).cloned().unwrap_or_default();
            let published = declared_exported || uses.exported;
            file.getters.insert(name, OwnGetter { uses, published });
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
                    match crate::graphql_document_sites::unwrap_expression(&export.expr) {
                        Expr::Object(object) if !lifted_members.is_empty() => {
                            walk_object_except(
                                &reader,
                                object,
                                &lifted_members,
                                &scope,
                                &mut module_level,
                            );
                        }
                        _ => reader.walk(&*export.expr, &scope, &mut module_level),
                    }
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
                            let Some(init) = &declarator.init else {
                                continue;
                            };
                            match crate::graphql_document_sites::unwrap_expression(init) {
                                Expr::Arrow(_) | Expr::Fn(_) => {}
                                Expr::Object(object) if !lifted_members.is_empty() => {
                                    walk_object_except(
                                        &reader,
                                        object,
                                        &lifted_members,
                                        &scope,
                                        &mut module_level,
                                    );
                                }
                                _ => reader.walk(&**init, &scope, &mut module_level),
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
        (
            file.module_bindings,
            file.kept_imports,
            file.written_through_at,
            file.placed_writes,
        ) = resolve::module_bindings(
            module,
            source_map,
            &reader,
            &scope,
            &file.functions,
            &file.module_level,
            &file.constructor_defaults,
        );
        file.param_bindings = resolve::param_bindings(
            &file.functions,
            &file.module_level,
            &file.constructor_defaults,
            &file.module_bindings,
            &file.placed_writes,
        );
        // A request read through a local written more than once keeps its
        // ways only where the census places every write of it (carrick#3679).
        let placed = &file.placed_writes;
        for body in file
            .functions
            .values_mut()
            .chain(std::iter::once(&mut file.module_level))
        {
            body.visit_calls(&mut |call| {
                if let Some(request) = &mut call.request {
                    request.alternation.census_checked(placed);
                }
            });
        }
    }
    file.arg_pieces = reader.arg_pieces.take();
    file.held_inits = reader.held_inits.take();
    file.block_bindings = reader.block_bindings.take();
    // FALLBACK (carrick#3794): each named function's returns, by its name's
    // line and its name.
    let returns = reader.function_returns.take();
    file.block_returns = named_functions(module, source_map)
        .into_iter()
        .filter_map(|(start, at)| Some((at, returns.get(&start)?.clone())))
        .collect();
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

/// Each argument's span (discovery numbering) before the first spread, read
/// past parentheses, type assertions and an `await` ([`CallIr::arg_spans`]).
fn arg_spans(args: &[ExprOrSpread]) -> Vec<(u32, u32)> {
    args.iter()
        .take_while(|arg| arg.spread.is_none())
        .map(|arg| {
            let span = unwrap_value(&arg.expr).span();
            (span.lo.0, span.hi.0)
        })
        .collect()
}

/// FALLBACK (carrick#3794): every function declaration, and every plain
/// declarator whose initialiser is a function, at any depth, by where its
/// function node starts, with the line its name is declared on and the name.
fn named_functions(module: &Module, source_map: &Lrc<SourceMap>) -> HashMap<u32, (u32, String)> {
    struct Find<'s> {
        source_map: &'s Lrc<SourceMap>,
        found: HashMap<u32, (u32, String)>,
    }
    impl Find<'_> {
        fn line(&self, span: Span) -> u32 {
            self.source_map.lookup_char_pos(span.lo).line as u32
        }
    }
    impl Visit for Find<'_> {
        fn visit_fn_decl(&mut self, decl: &FnDecl) {
            let at = (self.line(decl.ident.span), decl.ident.sym.to_string());
            self.found.insert(decl.function.span.lo.0, at);
            decl.visit_children_with(self);
        }
        fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
            if let Pat::Ident(binding) = &declarator.name {
                let start = match declarator.init.as_deref().map(unwrap_value) {
                    Some(Expr::Arrow(arrow)) => Some(arrow.span.lo.0),
                    Some(Expr::Fn(function)) => Some(function.function.span.lo.0),
                    _ => None,
                };
                if let Some(start) = start {
                    let at = (self.line(binding.id.span), binding.id.sym.to_string());
                    self.found.insert(start, at);
                }
            }
            declarator.visit_children_with(self);
        }
    }
    let mut find = Find {
        source_map,
        found: HashMap::new(),
    };
    module.visit_with(&mut find);
    find.found
}

/// A function an object literal holds as one of its entries.
#[derive(Clone, Copy)]
enum ObjectMember<'o> {
    Function(&'o Function),
    Arrow(&'o ArrowExpr),
}

impl ObjectMember<'_> {
    fn span(&self) -> Span {
        match self {
            ObjectMember::Function(function) => function.span,
            ObjectMember::Arrow(arrow) => arrow.span,
        }
    }
}

/// Each function `object` holds, keyed `owner.member` (`owner.v1.member` one
/// object down) the way the function extractor keys an exported object's
/// members (`FunctionDefinitionExtractor::collect_exported_object_members`
/// in `visitor.rs`, carrick#830), to the same depth: a method, or an entry
/// whose value is an arrow or a function expression (carrick#3356). An entry
/// naming a function declared elsewhere is that function already.
fn object_members<'o>(
    owner: &str,
    object: &'o ObjectLit,
    depth: usize,
    out: &mut Vec<(String, ObjectMember<'o>)>,
) {
    const MAX_DEPTH: usize = 3;
    if depth > MAX_DEPTH {
        return;
    }
    for prop in &object.props {
        let PropOrSpread::Prop(prop) = prop else {
            continue;
        };
        match &**prop {
            Prop::Method(method) => {
                if let Some(key) = prop_name(&method.key) {
                    out.push((
                        format!("{owner}.{key}"),
                        ObjectMember::Function(&method.function),
                    ));
                }
            }
            Prop::KeyValue(kv) => {
                let Some(key) = prop_name(&kv.key) else {
                    continue;
                };
                let name = format!("{owner}.{key}");
                match &*kv.value {
                    Expr::Arrow(arrow) => out.push((name, ObjectMember::Arrow(arrow))),
                    Expr::Fn(fn_expr) => {
                        out.push((name, ObjectMember::Function(&fn_expr.function)))
                    }
                    Expr::Object(nested) => object_members(&name, nested, depth + 1, out),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// Read each function `object` holds that the extractor keys (under
/// `owner`) as a function of the module under that key, and note its span
/// so module scope does not read its body again (carrick#3356, #3370).
#[allow(clippy::too_many_arguments)]
fn lift_object_members(
    reader: &Reader<'_>,
    owner: &str,
    object: &ObjectLit,
    module_scope: &ModuleScope,
    definition_keys: &HashSet<String>,
    lifted: &mut HashSet<u32>,
    file: &mut FileIr,
) {
    let none = Captured::default();
    let mut members = Vec::new();
    object_members(owner, object, 0, &mut members);
    for (key, member) in members {
        if !definition_keys.contains(&key) {
            continue;
        }
        let ir = match member {
            ObjectMember::Function(function) => {
                reader.function(function, None, module_scope, &none)
            }
            ObjectMember::Arrow(arrow) => reader.arrow(arrow, None, module_scope, &none),
        };
        lifted.insert(member.span().lo.0);
        file.functions.insert(key, ir);
    }
}

/// Read the calls `object` makes where the module is loaded, as a walk of
/// the whole literal would, less the bodies of the members read as functions
/// of the module (`lifted`, by span start): each is read once, as its own
/// function (carrick#3356).
fn walk_object_except(
    reader: &Reader<'_>,
    object: &ObjectLit,
    lifted: &HashSet<u32>,
    scope: &Scope<'_>,
    ir: &mut FnIr,
) {
    for prop in &object.props {
        let prop = match prop {
            PropOrSpread::Spread(spread) => {
                reader.walk(&*spread.expr, scope, ir);
                continue;
            }
            PropOrSpread::Prop(prop) => &**prop,
        };
        match prop {
            Prop::Method(method) if lifted.contains(&method.function.span.lo.0) => {}
            Prop::KeyValue(kv) => {
                let value = &*kv.value;
                let kept = match value {
                    Expr::Arrow(arrow) => !lifted.contains(&arrow.span.lo.0),
                    Expr::Fn(fn_expr) => !lifted.contains(&fn_expr.function.span.lo.0),
                    Expr::Object(_) => false,
                    _ => true,
                };
                if kept {
                    reader.walk(prop, scope, ir);
                    continue;
                }
                if let PropName::Computed(key) = &kv.key {
                    reader.walk(&*key.expr, scope, ir);
                }
                if let Expr::Object(nested) = value {
                    walk_object_except(reader, nested, lifted, scope, ir);
                }
            }
            _ => reader.walk(prop, scope, ir),
        }
    }
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

/// Each module-scope binding a file's names, clients and getters may be
/// keyed by ([`FileIr::declared_at`]), by the name the compiler gives its
/// symbol: the line its declaration starts on, as the compiler's `getStart`
/// reads it, and the name the file's tables key it by.
///
/// A declarator starts at its name (`export const X` starts at `X`); a
/// function declaration and an `export default` start at their first
/// keyword or modifier. A default export's symbol is `default`: a named
/// function's table key is its name, an expression's is
/// [`DEFAULT_EXPORT`], and `export default name` keys it by `name`. The
/// first declaration of a name wins, as an overload's first signature
/// does.
fn module_declarations(
    module: &Module,
    source_map: &Lrc<SourceMap>,
) -> HashMap<String, (u32, String)> {
    let line = |at: swc_common::BytePos| source_map.lookup_char_pos(at).line as u32;
    let mut declared: HashMap<String, (u32, String)> = HashMap::new();
    let mut add = |symbol: &str, at: swc_common::BytePos, key: &str| {
        declared
            .entry(symbol.to_string())
            .or_insert_with(|| (line(at), key.to_string()));
    };
    for item in &module.body {
        if let Some(var) = module_var_decl(item) {
            for declarator in &var.decls {
                if let Pat::Ident(ident) = &declarator.name {
                    add(&ident.id.sym, ident.id.span.lo, &ident.id.sym);
                }
            }
        }
        if let Some(Decl::Fn(fn_decl)) = module_decl(item) {
            add(&fn_decl.ident.sym, item.span().lo, &fn_decl.ident.sym);
        }
        match item {
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(export)) => {
                if let DefaultDecl::Fn(FnExpr {
                    ident: Some(ident), ..
                }) = &export.decl
                {
                    add(DEFAULT_EXPORT, export.span.lo, &ident.sym);
                    add(&ident.sym, export.span.lo, &ident.sym);
                }
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(export)) => {
                let key = match crate::graphql_document_sites::unwrap_expression(&export.expr) {
                    Expr::Ident(ident) => ident.sym.to_string(),
                    _ => DEFAULT_EXPORT.to_string(),
                };
                add(DEFAULT_EXPORT, export.span.lo, &key);
            }
            _ => {}
        }
    }
    declared
}

/// A module-scope binding a scope below declares the name of again
/// (carrick#3559).
struct Dropped {
    /// What a library site reads it as at a use that names no declaration
    /// below ([`Scope::site_binding`]). `None`: a `const` the module
    /// publishes, whose importers' uses do not reach a dropped client.
    site: Option<ClientBinding>,
    /// An import, not a `const` or a destructured name.
    import: bool,
}

/// Record a `const` or destructured name that holds an instance and that a
/// scope below declares again: for the receiver trace (`file.shadowed_clients`,
/// read as before) and for library sites (`dropped`, `file.dropped_clients`),
/// unless the module publishes it (carrick#3559).
fn record_dropped(
    dropped: &mut HashMap<String, Dropped>,
    file: &mut FileIr,
    name: String,
    by_name: ClientRef,
    own: ClientRef,
    published: bool,
) {
    let site = (!published && own.instance.is_some()).then(|| ClientBinding::Module(name.clone()));
    if site.is_some() {
        file.dropped_clients.insert(name.clone(), own);
    }
    dropped.insert(
        name.clone(),
        Dropped {
            site,
            import: false,
        },
    );
    file.shadowed_clients.insert(name, by_name);
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
    /// Each module-scope function a call of which yields nothing or a query
    /// ([`function_builds_query`], carrick#1950), by binding: a function
    /// declaration nothing assigns again, or a `const` arrow or function
    /// expression.
    query_builders: HashSet<BindingKey>,
    receivers: HashMap<String, ClientRef>,
    /// Each module-scope `let` a getter builds a client into on first use,
    /// with the instance it holds (carrick#1790,
    /// [`Reader::lazy_clients`]). Read only where a function returns it: no
    /// call is read through the `let`, for any role.
    lazy_clients: HashMap<BindingKey, ClientRef>,
    /// Every module-scope binding the by-name tables drop because a scope
    /// below declares the name again, by name: the `const`s that hold an
    /// instance ([`FileIr::shadowed_clients`]) and the imports a client may
    /// be read through (carrick#3559).
    dropped: HashMap<String, Dropped>,
    /// Each module-scope `const` a destructuring of a module object literal
    /// binds ([`destructured_keys`], carrick#3489 W2): a module name whose
    /// instance [`FileIr::module_instance`] follows.
    destructured: HashSet<String>,
    /// Each module-scope `let` or `var` a plain name declares: the instance
    /// it holds is each value written to it, read through the binding table
    /// (carrick#3489 W5, carrick#3504 S1).
    lets: HashSet<BindingKey>,
    /// Import bindings no scope below declares again, other than namespace
    /// imports: each may hold an instance the module it names declares
    /// (carrick#1568).
    imports: HashSet<String>,
    /// [`BindingUses`] over the whole file, by name.
    uses: HashMap<String, BindingUse>,
    /// The same uses by the binding each identifier names
    /// ([`BindingUses::bindings`], carrick#3547 S2).
    binding_uses: HashMap<BindingKey, BindingUse>,
    /// The uses recorded with no binding of their own, by name
    /// ([`BindingUses::unbound`]).
    unbound_uses: HashMap<String, BindingUse>,
    /// Names declared once in the file, by `const <name> = { … }`.
    object_consts: HashSet<String>,
    /// The literal of each module-scope `const` object the file never writes
    /// through, by binding: what a serialised name states
    /// ([`library_sites::serialised_operand`], carrick#2453).
    object_lits: HashMap<BindingKey, ObjectLit>,
    /// Class name -> the fields a class in this file that extends it
    /// declares or writes again.
    subclass_fields: HashMap<String, HashSet<String>>,
    /// Class name -> the methods a class in this file that extends it
    /// declares: on one of its instances, a call through `this` in the base
    /// class reaches the subclass's method (carrick#1950).
    subclass_methods: HashMap<String, HashSet<String>>,
    /// Each class the file declares, by its binding -> what the class does
    /// with `this`, for the message-role field rule (carrick#1665).
    class_this: HashMap<BindingKey, ClassThis>,
    /// Class name -> the value every construction of it in this file passes
    /// at each constructor position, for a class only this file can
    /// construct ([`Reader::constructions`], carrick#2452). `None`: the
    /// constructions disagree there, or one leaves the position out.
    constructions: HashMap<String, Vec<Option<Value>>>,
    /// Every declaration below module scope ([`redeclared_names`]): what a
    /// function, a block or a parameter binds.
    declared_below: Declarations,
    /// The type aliases the file declares that name a closed set of HTTP
    /// verbs (carrick#2049).
    verb_aliases: crate::declared_verbs::TypeAliases,
    /// The names of [`COLLECTIONS`] the file binds itself, anywhere: a
    /// declaration or an import. `new Map()` there builds the file's own
    /// `Map`, which is no language collection (carrick#2281).
    own_collections: HashSet<&'static str>,
    /// What a guard reads a name through at module scope (carrick#2386).
    guards: guards::ModuleGuards,
}

impl ModuleScope {
    /// `client`, contested when the file uses `name` other than to call
    /// through it or export it (carrick#1564 re-review, R6). Given the
    /// `binding` that holds it, a message role reads that binding's own
    /// uses, and an HTTP reading still every use of the name
    /// ([`ClientRef::used_apart`], carrick#3547 S2).
    fn with_uses(&self, client: ClientRef, name: &str, binding: Option<&BindingKey>) -> ClientRef {
        let Some(by_name) = self.uses.get(name) else {
            return client;
        };
        match binding {
            Some(key) => client.used_apart(by_name, &self.uses_of(key)),
            None => client.used_as(by_name),
        }
    }

    /// The uses of the binding `key`, with every use of its name recorded
    /// with no binding of its own (an alias root), so that one fails closed
    /// (carrick#3547 S2).
    fn uses_of(&self, key: &BindingKey) -> BindingUse {
        let mut used = self.binding_uses.get(key).cloned().unwrap_or_default();
        if let Some(unbound) = self.unbound_uses.get(&key.0) {
            used.merge(unbound);
        }
        used
    }

    /// Whether the module exports `name` by name (`export { name }`,
    /// `export default name`, `module.exports = name`).
    fn published(&self, name: &str) -> bool {
        self.uses.get(name).is_some_and(|used| used.exported)
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
    /// Each different expression a `return` writes, in source order; never
    /// empty. [`Reader::builder_call`] reads them together (carrick#2536).
    returned: Vec<Expr>,
}

impl Builder {
    /// The builder `expr` is: an arrow, or a function expression, that only
    /// returns an expression.
    fn of(expr: &Expr) -> Option<Self> {
        match crate::graphql_document_sites::unwrap_expression(expr) {
            Expr::Arrow(arrow) => Some(Builder {
                params: arrow.params.iter().map(pat_key).collect(),
                returned: match &*arrow.body {
                    ArrowFunctionBody::Expr(expr) => vec![(**expr).clone()],
                    ArrowFunctionBody::FunctionBody(block) => returned_values(&block.stmts)?,
                },
            }),
            Expr::Fn(function) => Self::of_function(&function.function),
            _ => None,
        }
    }

    fn of_function(function: &Function) -> Option<Self> {
        if function.is_generator {
            return None;
        }
        Some(Builder {
            params: function
                .params
                .iter()
                .map(|param| pat_key(&param.pat))
                .collect(),
            returned: returned_values(&function.body.as_ref()?.stmts)?,
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

/// Each different expression a `return` of a function body writes, in source
/// order (carrick#2447, carrick#2536), with each local `const` it names replaced by its
/// initialiser, so the builder reads it as `return root + "/v1"` for
/// `const root = "/api"; return root + "/v1"`.
///
/// The body may hold only `const name = <expr>` declarations, `return <expr>`,
/// `throw`, blocks and `if` / `else`, and every path through it must end in a
/// `return` or a `throw`. Any other statement (a loop, a `let`, a call that
/// may change what a later line reads) states no value, and nor does a body
/// with no `return`. One call statement is let through: a method call on a
/// body-local `const` (`query.set(k, v)`), when every return reads that
/// local only after the `?` that starts the query (see [`only_in_query`]). The
/// query is never part of the route (carrick#1950), so the call changes
/// nothing the route reads. The caller reads several different returns together
/// ([`shared_written_tail`]). A local is replaced by its
/// binding ([`ident_key`]), never by name, and a `const` a block or a branch
/// declares is read only inside it, so a block that declares the name again
/// keeps its own and the lines after the block read the outer one.
fn returned_values(stmts: &[Stmt]) -> Option<Vec<Expr>> {
    fn ends(stmts: &[Stmt]) -> bool {
        stmts.iter().any(|stmt| match stmt {
            Stmt::Return(_) | Stmt::Throw(_) => true,
            Stmt::Block(block) => ends(&block.stmts),
            Stmt::If(branch) => branch.alt.as_ref().is_some_and(|alt| {
                ends(std::slice::from_ref(&branch.cons)) && ends(std::slice::from_ref(alt))
            }),
            _ => false,
        })
    }

    /// Replaces each local `const`'s identifier by its initialiser, except
    /// the locals a call statement acted on (`.1`): those stay names until
    /// the return has been checked.
    struct Inline<'a>(&'a HashMap<BindingKey, Expr>, &'a HashSet<BindingKey>);
    impl swc_ecma_visit::VisitMut for Inline<'_> {
        fn visit_mut_expr(&mut self, expr: &mut Expr) {
            if let Expr::Ident(ident) = expr {
                let key = ident_key(ident);
                if !self.1.contains(&key)
                    && let Some(init) = self.0.get(&key)
                {
                    *expr = init.clone();
                    return;
                }
            }
            swc_ecma_visit::VisitMutWith::visit_mut_children_with(expr, self);
        }
    }

    fn walk(
        stmts: &[Stmt],
        locals: &mut HashMap<BindingKey, Expr>,
        touched: &mut HashSet<BindingKey>,
        returns: &mut Vec<Expr>,
    ) -> Option<()> {
        for stmt in stmts {
            match stmt {
                Stmt::Decl(Decl::Var(var)) if var.kind == VarDeclKind::Const => {
                    for declarator in &var.decls {
                        let (Pat::Ident(name), Some(init)) = (&declarator.name, &declarator.init)
                        else {
                            return None;
                        };
                        let mut init = (**init).clone();
                        swc_ecma_visit::VisitMutWith::visit_mut_with(
                            &mut init,
                            &mut Inline(locals, touched),
                        );
                        locals.insert(ident_key(&name.id), init);
                    }
                }
                Stmt::Return(ReturnStmt { arg: Some(arg), .. }) => {
                    let mut value = (**arg).clone();
                    swc_ecma_visit::VisitMutWith::visit_mut_with(
                        &mut value,
                        &mut Inline(locals, touched),
                    );
                    if !touched.is_empty() {
                        if !only_in_query(&value, touched) {
                            return None;
                        }
                        swc_ecma_visit::VisitMutWith::visit_mut_with(
                            &mut value,
                            &mut Inline(locals, &HashSet::new()),
                        );
                    }
                    returns.push(value);
                }
                Stmt::Throw(_) => {}
                // `query.set(k, v)` on a body-local `const`: the local may
                // change, so it is read only where the return puts it after
                // the `?` (checked at each return).
                Stmt::Expr(ExprStmt { expr, .. }) => {
                    let Expr::Call(CallExpr {
                        callee: Callee::Expr(callee),
                        ..
                    }) = &**expr
                    else {
                        return None;
                    };
                    let Expr::Member(MemberExpr { obj, .. }) =
                        crate::graphql_document_sites::unwrap_expression(callee)
                    else {
                        return None;
                    };
                    let Expr::Ident(receiver) = &**obj else {
                        return None;
                    };
                    let key = ident_key(receiver);
                    if !locals.contains_key(&key) {
                        return None;
                    }
                    touched.insert(key);
                }
                // A block's or a branch's `const` is visible only inside it:
                // each walks its own copy of the locals declared so far.
                Stmt::Block(block) => walk(&block.stmts, &mut locals.clone(), touched, returns)?,
                Stmt::If(branch) => {
                    walk(
                        std::slice::from_ref(&branch.cons),
                        &mut locals.clone(),
                        touched,
                        returns,
                    )?;
                    if let Some(alt) = &branch.alt {
                        walk(
                            std::slice::from_ref(alt),
                            &mut locals.clone(),
                            touched,
                            returns,
                        )?;
                    }
                }
                _ => return None,
            }
        }
        Some(())
    }

    if !ends(stmts) {
        return None;
    }
    let mut returns = Vec::new();
    walk(
        stmts,
        &mut HashMap::new(),
        &mut HashSet::new(),
        &mut returns,
    )?;
    let mut distinct: Vec<Expr> = Vec::new();
    for value in returns {
        if !distinct
            .iter()
            .any(|seen| swc_common::EqIgnoreSpan::eq_ignore_span(seen, &value))
        {
            distinct.push(value);
        }
    }
    (!distinct.is_empty()).then_some(distinct)
}

/// Whether every mention of a local in `touched` in a returned value sits
/// after a literal `?`: in a template after a quasi that holds one, or in a
/// `+` chain after a string that does. Anything else (the local in the path,
/// or a shape this does not read) is `false`, so the builder stands down.
fn only_in_query(value: &Expr, touched: &HashSet<BindingKey>) -> bool {
    struct Mentions<'a>(&'a HashSet<BindingKey>, bool);
    impl swc_ecma_visit::Visit for Mentions<'_> {
        fn visit_ident(&mut self, ident: &Ident) {
            self.1 |= self.0.contains(&ident_key(ident));
        }
    }
    fn mentions(expr: &Expr, touched: &HashSet<BindingKey>) -> bool {
        let mut found = Mentions(touched, false);
        swc_ecma_visit::VisitWith::visit_with(expr, &mut found);
        found.1
    }
    fn clean(expr: &Expr, touched: &HashSet<BindingKey>, in_query: &mut bool) -> bool {
        match crate::graphql_document_sites::unwrap_expression(expr) {
            Expr::Lit(Lit::Str(text)) => {
                *in_query |= text.value.to_string_lossy().contains('?');
                true
            }
            Expr::Tpl(template) => {
                for (index, quasi) in template.quasis.iter().enumerate() {
                    *in_query |= quasi.raw.contains('?');
                    if let Some(part) = template.exprs.get(index)
                        && !clean(part, touched, in_query)
                    {
                        return false;
                    }
                }
                true
            }
            Expr::Bin(BinExpr {
                op: BinaryOp::Add,
                left,
                right,
                ..
            }) => clean(left, touched, in_query) && clean(right, touched, in_query),
            other => *in_query || !mentions(other, touched),
        }
    }
    clean(value, touched, &mut false)
}

/// The written path every value ends in, when what comes before it is only
/// values the source does not write (carrick#2536): `readMount() + "/svc"`
/// and `"/svc"` share `/svc`. The tails must be byte-equal, not merely
/// share a suffix (`/v1/api` and `/v2/api` share none, and `/admin/api` with
/// `/api` would fold written text into the hole). The tail starts with `/`,
/// holds a non-empty segment and no `?` or `#`, and every earlier piece of
/// every value is an unread value.
fn shared_written_tail(values: &[Value]) -> Option<String> {
    let mut tail: Option<&str> = None;
    for value in values {
        let Value::Str(pieces) = value else {
            return None;
        };
        let (Piece::Lit(last), before) = pieces.split_last()? else {
            return None;
        };
        if !before
            .iter()
            .all(|piece| matches!(piece, Piece::Opaque { .. }))
        {
            return None;
        }
        if tail.is_some_and(|tail| tail != last) {
            return None;
        }
        tail = Some(last);
    }
    let tail = tail?;
    (tail.starts_with('/')
        && !tail.contains(['?', '#'])
        && tail.split('/').any(|segment| !segment.is_empty()))
    .then(|| tail.to_string())
}

/// What a binding that holds one of `values` hands an importer (carrick#2540,
/// the #2536 rule read where an importer cannot see the value): the one
/// written text every value is, or the one written path every value ends in
/// after values the source does not write. A value that reads through another
/// module's binding is never a tail: that binding may write path text the
/// tail would hide. A value that reads as several written paths is that
/// (carrick#2541).
fn held_by(values: &[Value]) -> Option<Held> {
    let first = values.first()?;
    if values.iter().any(holds_several_paths) {
        return Some(Held::SeveralPaths);
    }
    if let Value::Str(pieces) = first
        && let [Piece::Lit(text)] = pieces.as_slice()
        && values.iter().all(|value| value == first)
    {
        return Some(Held::Text(text.clone()));
    }
    let linked = |value: &Value| {
        matches!(value, Value::Str(pieces) if pieces
            .iter()
            .any(|piece| piece.through().is_none()
                && matches!(piece, Piece::Opaque { link: Some(_), .. })))
    };
    if !values.iter().any(linked)
        && let Some(tail) = shared_written_tail(values)
    {
        return Some(Held::Tail(tail));
    }
    several_values(values.to_vec(), String::new())
        .filter(holds_several_paths)
        .map(|_| Held::SeveralPaths)
}

/// Whether `value` writes a path: a literal that holds a `/` with a
/// non-empty segment before any `?` or `#`, outside the origin
/// (`scheme://host[:port]` or `//host`) a value may lead with (carrick#2541).
/// An origin is a base, not a path.
fn writes_a_path_segment(value: &Value) -> bool {
    let Value::Str(pieces) = value else {
        return false;
    };
    for (position, piece) in pieces.iter().enumerate() {
        let Piece::Lit(text) = piece else {
            continue;
        };
        let text = if position == 0 && is_absolute_url(text) {
            let rest = text.split_once("//").map_or("", |(_, rest)| rest);
            rest.find('/').map_or("", |at| &rest[at..])
        } else {
            text.as_str()
        };
        let route = text.split(['?', '#']).next().unwrap_or("");
        if route.contains('/') && route.split('/').any(|segment| !segment.is_empty()) {
            return true;
        }
        if route.len() != text.len() {
            return false;
        }
    }
    false
}

/// A builder's returns that are each the caller's own parameter `index`,
/// whole (`init ?? {}`) or spread first into an object (`{ ...init, keepalive:
/// true }`, [`ObjValue::under_param`] with nothing written before it): the
/// caller's options, with each key any return writes after the spread
/// unread. Such a key is held before the spread, as an unknown, and
/// unsettled, so the option-key readers (a decided site's method and payload
/// slot, [`Effect::read_key`]) refuse it, and every other key is
/// the one the caller's caller writes (carrick#1561 C11). `None`
/// where the returns are all one value (that value is the answer), and
/// unless at least one return spreads the parameter and every return is one
/// of those two forms of the same parameter.
fn passed_options(values: &[Value]) -> Option<Value> {
    if values.iter().all(|value| *value == values[0]) {
        return None;
    }
    let mut index: Option<usize> = None;
    let mut written: BTreeSet<String> = BTreeSet::new();
    let mut spread = false;
    for value in values {
        let position = match value {
            Value::Str(pieces) => match pieces.as_slice() {
                [Piece::Param(position, _)] => *position,
                _ => return None,
            },
            Value::Obj(object) => match &object.under_param {
                Some((position, before))
                    if before.is_empty()
                        && object.under_rest.is_none()
                        && object.functions.is_empty() =>
                {
                    spread = true;
                    written.extend(object.fields.keys().cloned());
                    written.extend(object.unsettled.iter().cloned());
                    *position
                }
                _ => return None,
            },
            Value::Callback(_) => return None,
        };
        if index.is_some_and(|index| index != position) {
            return None;
        }
        index = Some(position);
    }
    let index = index.filter(|_| spread)?;
    let before = written
        .iter()
        .map(|key| (key.clone(), Value::Str(vec![Piece::Unknown])))
        .collect();
    Some(Value::Obj(ObjValue {
        fields: BTreeMap::new(),
        open: true,
        under_param: Some((index, before)),
        under_rest: None,
        unsettled: written,
        functions: BTreeSet::new(),
        wide: true,
    }))
}

/// A value that is one of `values`: a builder's returns, a conditional's
/// branches (carrick#2536, carrick#2541).
///
/// 1. Every value equal: that value.
/// 2. [`shared_written_tail`]: the unread value, then the one path they share.
/// 3. Some value writes a path, or already reads as several: the hole
///    [`HoleReason::SeveralPaths`], which no row is keyed on.
/// 4. Otherwise `None`: the caller's hole, as before.
fn several_values(values: Vec<Value>, text: String) -> Option<Value> {
    if values.iter().all(|value| *value == values[0]) {
        return values.into_iter().next();
    }
    if let Some(tail) = shared_written_tail(&values) {
        return Some(Value::Str(vec![
            Piece::Opaque {
                text,
                end: Root::Hole(HoleReason::UnreadExpression),
                link: None,
            },
            Piece::Lit(tail),
        ]));
    }
    values
        .iter()
        .any(|value| writes_a_path_segment(value) || holds_several_paths(value))
        .then(|| Value::hole(text, HoleReason::SeveralPaths))
}

/// Whether `value` holds a piece that reads as several written paths.
fn holds_several_paths(value: &Value) -> bool {
    matches!(value, Value::Str(pieces) if pieces.iter().any(|piece| matches!(
        piece,
        Piece::Opaque { end: Root::Hole(HoleReason::SeveralPaths), .. }
    )))
}

/// How one binding is used beyond its own declaration: an identifier, or a
/// field of `this` keyed `this.<field>` ([`binding_key`]).
#[derive(Debug, Default, Clone)]
struct BindingUse {
    /// The members called through it (`None`: the binding itself called),
    /// each with the span start of every call (discovery numbering, as
    /// [`Self::returned_at`]). Where a getter's calls are, so that every one
    /// can be accounted for (carrick#1798).
    called: BTreeMap<Option<String>, BTreeSet<u32>>,
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
    /// The span start of each use that sets [`Self::other`] (discovery
    /// numbering, as [`Self::returned_at`]; a use with no span of its own
    /// records none). A constant handed to one call is read where that call
    /// is ([`Self::handed_only_to`], carrick#2451).
    other_at: BTreeSet<u32>,
    /// Exported by name (`export { api }`, `export default api`,
    /// `module.exports.api = api`). Not a use that changes anything; it says
    /// another module may import the binding (carrick#1568).
    exported: bool,
    /// Read as an operand of `instanceof`, `typeof` or a comparison: a value
    /// use, which keeps an import at run time, that changes nothing.
    read: bool,
    /// Tested for truth: in a test position ([`BindingUses::tested`]), the
    /// whole test of an `if`, a loop or a conditional, the operand of `!`
    /// (`if (!this.client)`), or an operand of a `&&` or `||` in a test
    /// position (carrick#1690). A message role reads it as [`Self::read`]
    /// (carrick#1665); before, it was the operand it is to an HTTP reading,
    /// which still contests on it.
    tested: bool,
    /// The object of a member read whose value is only tested
    /// (carrick#1690): a member chain read off the binding in a test
    /// position (`if (socket.recovered || retrying)`). A message role reads
    /// it as [`Self::read`]: a read changes no name, prefix or base. Before,
    /// it was the [`Self::member_read`] it is to an HTTP reading, which still
    /// contests on it.
    member_tested: bool,
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
    /// carrick#1665, a return as the operand it was before carrick#1562, and
    /// a member read only tested as the member read it was before
    /// carrick#1690.
    fn contests_client(&self) -> bool {
        self.written
            || self.member_read
            || self.member_tested
            || self.spread
            || self.other
            || self.tested
            || !self.library_calls.is_empty()
            || !self.returned_at.is_empty()
    }

    /// The same for a message role (carrick#1661): a hand-off, a write, a
    /// member read used as a value, a spread, or a member called by a key
    /// the source does not state. Constructing the binding is what a `new`
    /// maker does, and a call through a sub-object is a call; both are among
    /// [`Self::member_uses`], which the caller classifies. A truthiness test
    /// of the binding or of a member read off it keeps nothing of the
    /// binding (carrick#1665, carrick#1690). A return is the reader's to
    /// judge ([`BindingUse::returned_at`]): followed to an own factory's
    /// callers, or a hand-off.
    fn contests_message(&self) -> bool {
        self.written || self.member_read || self.spread || self.other || self.called_computed
    }

    /// Whether the binding is used only as the callee of `new`
    /// (carrick#2452): not called, read, handed on, stored, spread, returned,
    /// tested, extended or published. A type position is no use
    /// ([`BindingUses`]).
    fn only_constructed(&self) -> bool {
        !(self.written
            || self.member_read
            || self.member_tested
            || self.spread
            || self.other
            || self.tested
            || self.exported
            || self.called_computed)
            && self.called.is_empty()
            && self.returned_at.is_empty()
            && self.entry_reads.is_empty()
            && self.library_calls.iter().all(|made| {
                made.form == MakerForm::New && made.path.is_empty() && made.member.is_none()
            })
    }

    /// Every use of `other` added to these (one class's uses of a field and
    /// a related class's, carrick#1665).
    fn merge(&mut self, other: &BindingUse) {
        for (member, at) in &other.called {
            self.called
                .entry(member.clone())
                .or_default()
                .extend(at.iter().copied());
        }
        self.called_computed |= other.called_computed;
        self.written |= other.written;
        self.member_read |= other.member_read;
        self.spread |= other.spread;
        self.other |= other.other;
        self.other_at.extend(other.other_at.iter().copied());
        self.exported |= other.exported;
        self.read |= other.read;
        self.tested |= other.tested;
        self.member_tested |= other.member_tested;
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

    /// Whether this use may change an object the binding holds, or hand it
    /// to code that may: written through, handed on (an argument, an
    /// initialiser, any value use [`Self::other`] counts), returned, spread,
    /// or called by a key the source does not state. Calling it, calling a
    /// member, reading, testing and exporting it change nothing
    /// (carrick#2413 slice B): the use of an import whose declaration no
    /// row gives that contests every exported object and instance
    /// ([`row_links::RowLinks::open`]).
    fn may_change(&self) -> bool {
        self.written
            || self.other
            || self.spread
            || self.called_computed
            || !self.returned_at.is_empty()
    }

    /// Every member called or constructed through the binding.
    fn member_uses(&self) -> impl Iterator<Item = MemberUse> + '_ {
        self.called
            .keys()
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
        !self.other && self.keeps_object_apart_from_other()
    }

    /// [`Self::keeps_object`] but for [`Self::other`], which it leaves to the
    /// caller.
    fn keeps_object_apart_from_other(&self) -> bool {
        !self.written
            && !self.tested
            && self.returned_at.is_empty()
            && !self.library_calls.contains(&MemberUse {
                form: MakerForm::New,
                path: Vec::new(),
                member: None,
            })
    }
}

impl BindingUse {
    /// An object constant that keeps exactly the keys its literal writes and
    /// is handed to one call, the one whose argument starts at `at`: its
    /// only use that sets [`Self::other`] (carrick#2451). A use with no span
    /// of its own records none, so it is never this one.
    fn handed_only_to(&self, at: u32) -> bool {
        self.other_at.len() == 1
            && self.other_at.contains(&at)
            && self.keeps_object_apart_from_other()
    }
}

/// Every binding's uses over a subtree ([`BindingUse`]). Calls through a
/// binding (`api.get(…)`, `api(…)`) and `export default api` record nothing;
/// a bare reassignment is left to the declaration rules.
///
/// Every use is recorded by name in [`Self::uses`], and beside it by the
/// binding its identifier names in [`Self::bindings`], or in
/// [`Self::unbound`] when it names none (carrick#3547 S2), as
/// [`crate::binding_scope::Declarations`] keeps names and bindings.
#[derive(Default)]
struct BindingUses {
    uses: HashMap<String, BindingUse>,
    /// The same uses by the binding each identifier names. A file parsed
    /// without the resolver gives every binding of a name one key, which
    /// is the name's uses.
    bindings: HashMap<BindingKey, BindingUse>,
    /// The uses with no binding of their own: a field of `this` and the
    /// root of an `import alias = root.a.b`. Every binding of the name
    /// reads them ([`ModuleScope::uses_of`]).
    unbound: HashMap<String, BindingUse>,
    /// `import alias = root.a.b` declarations, settled after the walk.
    aliases: Vec<EntityAlias>,
}

/// The key a use is recorded under ([`binding_key`]): the name, and the
/// binding an identifier names. A field of `this` names none.
struct UseKey {
    name: String,
    binding: Option<BindingKey>,
}

impl UseKey {
    /// The binding `ident` names.
    fn of(ident: &Ident) -> Self {
        Self {
            name: ident.sym.to_string(),
            binding: Some(ident_key(ident)),
        }
    }

    /// `name`, with no binding of its own.
    fn unbound(name: String) -> Self {
        Self {
            name,
            binding: None,
        }
    }
}

/// `import alias = root.a.b` (`export import` when `exported`).
struct EntityAlias {
    alias: String,
    root: String,
    exported: bool,
    /// The span start of the declaration.
    at: u32,
}

impl BindingUses {
    /// Records one use under `key`'s name and under its binding, or with
    /// no binding. [`BindingUse::merge`] adds every field `record` sets.
    fn mark(&mut self, key: UseKey, record: impl FnOnce(&mut BindingUse)) {
        let mut used = BindingUse::default();
        record(&mut used);
        match key.binding {
            Some(binding) => self.bindings.entry(binding).or_default().merge(&used),
            None => self
                .unbound
                .entry(key.name.clone())
                .or_default()
                .merge(&used),
        }
        self.uses.entry(key.name).or_default().merge(&used);
    }

    /// `key` is used as a value, at `at`.
    fn mark_other(&mut self, key: UseKey, at: u32) {
        self.mark(key, |used| {
            used.other = true;
            used.other_at.insert(at);
        });
    }

    /// `member` is read and not called: its chain's root is read.
    fn read_member(&mut self, member: &MemberExpr) {
        self.read_member_as(member, |used| used.member_read = true);
    }

    /// `member` is read, and its chain's root, when a binding, is recorded
    /// by `record`. A computed key is visited, and a root that is no binding
    /// is visited as usual.
    fn read_member_as(&mut self, member: &MemberExpr, record: fn(&mut BindingUse)) {
        if let MemberProp::Computed(key) = &member.prop {
            key.expr.visit_with(self);
        }
        let obj = crate::graphql_document_sites::unwrap_expression(&member.obj);
        if let Some(key) = binding_key(obj) {
            self.mark(key, record);
        } else if let Some(inner) = as_member(obj) {
            self.read_member_as(inner, record);
        } else {
            obj.visit_with(self);
        }
    }

    /// `TOPICS.orders` read off its binding: an entry read (carrick#1562).
    /// One by a key the source does not state, or through an optional
    /// chain, may read any entry: the object itself as far as a name is
    /// concerned.
    fn entry_read(&mut self, expr: &Expr) {
        let path = match expr {
            Expr::Member(_) => member_path(expr),
            _ => None,
        };
        match path {
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

    /// A call's callee, for the call that starts at `at`: a binding called,
    /// or the direct receiver of the member called, records the call where
    /// it is ([`BindingUse::called`]); a deeper receiver is read.
    fn callee(&mut self, callee: &Expr, at: u32) {
        let callee = crate::graphql_document_sites::unwrap_expression(callee);
        if let Some(key) = binding_key(callee) {
            self.mark(key, |used| {
                used.called.entry(None).or_default().insert(at);
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
                    used.called.entry(Some(name)).or_default().insert(at);
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
    /// binding is constructed (carrick#1661), or, in the decorate `form`,
    /// called as a decorator (`@lib.on(…)`, carrick#3043). Any other callee
    /// is read as before.
    fn constructed(&mut self, callee: &Expr, form: MakerForm) {
        let callee = crate::graphql_document_sites::unwrap_expression(callee);
        if let Some(key) = binding_key(callee) {
            self.mark(key, |used| {
                used.library_calls.insert(MemberUse {
                    form,
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
                        form,
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
fn chain_root(expr: &Expr) -> Option<UseKey> {
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
fn member_path(expr: &Expr) -> Option<(UseKey, Vec<String>)> {
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
                    at: decl.span.lo.0,
                });
            }
        }
    }

    fn visit_expr(&mut self, expr: &Expr) {
        if let Some(key) = binding_key(expr) {
            self.mark_other(key, expr.span().lo.0);
            return;
        }
        if let Expr::Member(member) = expr {
            // `TOPICS.orders` as a value: an entry read (carrick#1562).
            self.entry_read(expr);
            self.read_member(member);
            return;
        }
        expr.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        match &call.callee {
            Callee::Expr(callee) => self.callee(callee, call.span.lo.0),
            other => other.visit_with(self),
        }
        call.args.visit_with(self);
    }

    fn visit_new_expr(&mut self, new: &NewExpr) {
        self.constructed(&new.callee, MakerForm::New);
        new.args.visit_with(self);
    }

    /// `@m(…)`: the binding called is used as a decorator (carrick#3043),
    /// which the caller classifies as it does a construction.
    fn visit_decorator(&mut self, decorator: &Decorator) {
        match crate::graphql_document_sites::unwrap_expression(&decorator.expr) {
            Expr::Call(CallExpr {
                callee: Callee::Expr(callee),
                args,
                ..
            }) => {
                self.constructed(callee, MakerForm::Decorate);
                args.visit_with(self);
            }
            _ => decorator.visit_children_with(self),
        }
    }

    fn visit_opt_chain_expr(&mut self, chain: &OptChainExpr) {
        match &*chain.base {
            OptChainBase::Call(call) => {
                self.callee(&call.callee, call.span.lo.0);
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
                self.mark(UseKey::of(ident), |used| used.exported = true);
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
            self.mark_other(UseKey::of(ident), ident.span.lo.0);
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
        let ArrowFunctionBody::Expr(body) = &*arrow.body else {
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
                let at = self.aliases[index].at;
                // The root is a name the declaration's entity names, with
                // no binding read here: every binding of it is contested.
                self.mark_other(UseKey::unbound(root), at);
            }
        }
    }

    fn spread(&mut self, expr: &Expr) {
        match binding_key(crate::graphql_document_sites::unwrap_expression(expr)) {
            Some(key) => self.mark(key, |used| used.spread = true),
            None => expr.visit_with(self),
        }
    }

    /// An expression in a test position, whose value is only tested for
    /// truth: the whole test of an `if`, a loop or a conditional, the operand
    /// of `!`, and each operand of a `&&` or `||` in a test position
    /// (carrick#1690). A binding there is [`BindingUse::tested`]
    /// (carrick#1665), and a member chain read off one, through any hops, is
    /// [`BindingUse::member_tested`], its entry read as a value's is.
    /// Anything else is visited as usual. A comparison or `typeof` operand is
    /// [`Self::compared`].
    fn tested(&mut self, expr: &Expr) {
        let expr = crate::graphql_document_sites::unwrap_expression(expr);
        if let Some(key) = binding_key(expr) {
            self.mark(key, |used| used.tested = true);
            return;
        }
        if let Expr::Bin(bin) = expr
            && matches!(bin.op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr)
        {
            self.tested(&bin.left);
            self.tested(&bin.right);
            return;
        }
        if let Some(member) = as_member(expr) {
            self.entry_read(expr);
            self.read_member_as(member, |used| used.member_tested = true);
            return;
        }
        expr.visit_with(self);
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

/// The key a binding is tracked by: an identifier's name and binding, or
/// `this.<field>`, which names no binding.
fn binding_key(expr: &Expr) -> Option<UseKey> {
    match expr {
        Expr::Ident(ident) => Some(UseKey::of(ident)),
        Expr::Member(member) if matches!(&*member.obj, Expr::This(_)) => match &member.prop {
            MemberProp::Ident(ident) => Some(UseKey::unbound(format!("this.{}", ident.sym))),
            MemberProp::PrivateName(private) => {
                Some(UseKey::unbound(format!("this.#{}", private.name)))
            }
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
            Expr::Lit(Lit::Str(key)) => Some(key.value.to_string_lossy().into_owned()),
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

/// One class a file declares by name.
#[derive(Default)]
struct DeclaredClass {
    /// The class it extends, when that is a name.
    superclass: Option<String>,
    /// The fields it declares or writes.
    fields: HashSet<String>,
    /// The instance methods and accessors it declares (carrick#1950).
    methods: HashSet<String>,
}

/// Every class this file declares by name.
fn classes_by_name(module: &Module) -> HashMap<String, DeclaredClass> {
    #[derive(Default)]
    struct Classes {
        found: HashMap<String, DeclaredClass>,
    }
    impl Classes {
        fn record(&mut self, name: String, class: &Class) {
            let superclass = match class.super_class.as_deref().map(Expr::unwrap_parens) {
                Some(Expr::Ident(ident)) => Some(ident.sym.to_string()),
                _ => None,
            };
            let mut fields = HashSet::new();
            let mut methods = HashSet::new();
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
                    ClassMember::Method(method) if !method.is_static => {
                        if let Some(name) = prop_name(&method.key) {
                            methods.insert(name);
                        }
                    }
                    _ => {}
                }
                member.visit_with(&mut writes);
            }
            fields.extend(writes.fields);
            self.found.insert(
                name,
                DeclaredClass {
                    superclass,
                    fields,
                    methods,
                },
            );
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

/// Class name -> every member (`members` picks which: its fields, or its
/// methods) that a class extending it, directly or through others in this
/// file, declares or writes. Such a member is the subclass's on a subclass
/// instance, so the base class's own methods cannot read it as the base
/// class writes it.
fn subclass_members(
    classes: &HashMap<String, DeclaredClass>,
    members: impl Fn(&DeclaredClass) -> &HashSet<String>,
) -> HashMap<String, HashSet<String>> {
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    for (name, class) in classes {
        let mut seen: HashSet<&str> = HashSet::from([name.as_str()]);
        let mut ancestor = class.superclass.as_deref();
        while let Some(base) = ancestor {
            if !seen.insert(base) {
                break;
            }
            out.entry(base.to_string())
                .or_default()
                .extend(members(class).iter().cloned());
            ancestor = classes
                .get(base)
                .and_then(|class| class.superclass.as_deref());
        }
    }
    out
}

/// The instance methods of `class` a call of which, through `this`, yields
/// nothing or a query ([`ClassFields::query_builders`], carrick#1950).
///
/// `fields` is the class's own table, which holds every name the class
/// assigns through `this` or declares as a field: a method one of them names
/// may have been replaced. `overridden` is what a class in this file that
/// extends this one declares as a method: on its instances the call reaches
/// that method instead.
fn class_query_builders(
    class: &Class,
    fields: &ClassFields,
    overridden: Option<&HashSet<String>>,
) -> HashSet<String> {
    // Each method name, with whether every declaration of it that has a body
    // builds a query, and how many have one (overloads have none).
    let mut bodies: HashMap<String, (bool, usize)> = HashMap::new();
    for member in &class.body {
        let (name, function, plain) = match member {
            ClassMember::Method(method) if !method.is_static => (
                prop_name(&method.key),
                &method.function,
                method.kind == MethodKind::Method,
            ),
            ClassMember::PrivateMethod(method) if !method.is_static => (
                Some(format!("#{}", method.key.name)),
                &method.function,
                method.kind == MethodKind::Method,
            ),
            _ => continue,
        };
        let Some(name) = name else {
            continue;
        };
        if function.body.is_none() {
            continue;
        }
        let entry = bodies.entry(name).or_insert((true, 0));
        entry.0 &= plain && function_builds_query(function);
        entry.1 += 1;
    }
    bodies
        .into_iter()
        .filter(|(name, (builds, count))| {
            *builds
                && *count == 1
                && !fields.values.contains_key(name)
                && overridden.is_none_or(|methods| !methods.contains(name))
        })
        .map(|(name, _)| name)
        .collect()
}

/// The module-scope functions a call of which yields nothing or a query
/// ([`ModuleScope::query_builders`], carrick#1950).
fn module_query_builders(module: &Module, reassigned: &Reassigned) -> HashSet<BindingKey> {
    let mut builders = HashSet::new();
    for item in &module.body {
        match module_decl(item) {
            Some(Decl::Fn(declared))
                if !reassigned.names.contains(declared.ident.sym.as_ref())
                    && function_builds_query(&declared.function) =>
            {
                builders.insert(ident_key(&declared.ident));
            }
            Some(Decl::Var(var)) if var.kind == VarDeclKind::Const => {
                for declarator in &var.decls {
                    let (Pat::Ident(ident), Some(init)) = (&declarator.name, &declarator.init)
                    else {
                        continue;
                    };
                    let builds = match crate::graphql_document_sites::unwrap_expression(init) {
                        Expr::Arrow(arrow) => arrow_builds_query(arrow),
                        Expr::Fn(function) => function_builds_query(&function.function),
                        _ => false,
                    };
                    if builds {
                        builders.insert(ident_key(&ident.id));
                    }
                }
            }
            _ => {}
        }
    }
    builders
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
    /// The instance methods a call of which, through `this`, yields nothing
    /// or a query ([`function_builds_query`], carrick#1950): declared once
    /// with a body, never assigned over as a field, and declared again by no
    /// class in this file that extends this one.
    query_builders: HashSet<String>,
    /// The fields the constructor sets from a parameter, and its
    /// parameters' defaults ([`Reader::ctor_origins`], carrick#2452): read
    /// once every module is, through the class's constructions.
    origins: HashMap<String, constructions::FieldOrigin>,
    ctor_defaults: Vec<(usize, Vec<String>, Vec<ValueAt>)>,
}

/// What a function written inside another can read of it, by binding.
#[derive(Default)]
struct Captured {
    values: HashMap<BindingKey, Value>,
    /// The enclosing functions' plain parameters whose keys a body may read
    /// as the caller wrote them ([`Scope::key_objects`]), with how many
    /// functions out each is and its position there (carrick#1949).
    objects: HashMap<BindingKey, (u8, usize)>,
    /// [`Scope::rest_of`], each one function further out.
    rests: HashMap<BindingKey, (u8, usize, BTreeSet<String>)>,
    /// Literal text only: a hole is a parameter of the enclosing function,
    /// which the function written inside it is not called with.
    texts: HashMap<BindingKey, Vec<library_sites::TextPiece>>,
    receivers: HashMap<BindingKey, ClientRef>,
    /// [`Scope::fetches`].
    fetches: HashSet<BindingKey>,
    /// [`Scope::subjects`], each parameter of the enclosing functions read
    /// as an enclosing one ([`Subject::enclosed`]).
    subjects: HashMap<BindingKey, Subject>,
    /// [`Scope::held`], each parameter one function further out.
    held: HashMap<BindingKey, Vec<ValueAt>>,
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
    /// The binding the local identifier names ([`ident_key`]).
    key: BindingKey,
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
        |local: &Ident, specifier: String, export: String, namespace: bool, reassignable: bool| {
            bindings.insert(
                local.sym.to_string(),
                ImportBinding {
                    specifier,
                    export,
                    namespace,
                    reassignable,
                    key: ident_key(local),
                },
            );
        };
    for item in &module.body {
        match item {
            ModuleItem::ModuleDecl(ModuleDecl::Import(import)) if !import.type_only => {
                let specifier = import.src.value.to_string_lossy().into_owned();
                for import_specifier in &import.specifiers {
                    match import_specifier {
                        ImportSpecifier::Default(default) => {
                            add(
                                &default.local,
                                specifier.clone(),
                                DEFAULT_EXPORT.to_string(),
                                false,
                                false,
                            );
                        }
                        ImportSpecifier::Named(named) if !named.is_type_only => {
                            let export = match &named.imported {
                                Some(ModuleExportName::Ident(ident)) => ident.sym.to_string(),
                                Some(ModuleExportName::Str(name)) => {
                                    name.value.to_string_lossy().into_owned()
                                }
                                None => named.local.sym.to_string(),
                            };
                            add(&named.local, specifier.clone(), export, false, false);
                        }
                        ImportSpecifier::Namespace(namespace) => {
                            add(
                                &namespace.local,
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
                        &decl.id,
                        external.expr.value.to_string_lossy().into_owned(),
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
                            &local.id,
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
                            &bound.local.id,
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
/// names no module and is not here: the second value says whether the file
/// makes such a load with `import()` or `require` (carrick#2413 slice B).
fn module_loads(module: &Module, bound_requires: &HashSet<u32>) -> (BTreeSet<String>, bool) {
    struct Loads<'a> {
        bound_requires: &'a HashSet<u32>,
        found: BTreeSet<String>,
        computed: bool,
    }
    impl Visit for Loads<'_> {
        fn visit_call_expr(&mut self, call: &CallExpr) {
            let specifier = call
                .args
                .first()
                .filter(|arg| arg.spread.is_none())
                .and_then(|arg| literal_specifier(&arg.expr));
            let require = matches!(&call.callee, Callee::Expr(callee)
                if matches!(&**callee, Expr::Ident(ident) if ident.sym == *"require"));
            if specifier.is_none() && (require || matches!(call.callee, Callee::Import(_))) {
                self.computed = true;
            }
            if let Some(specifier) = specifier {
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
        computed: false,
    };
    module.visit_with(&mut loads);
    (loads.found, loads.computed)
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
///
/// The framework-detect import sample reads through it too
/// ([`crate::framework_detector::ImportSample`], carrick#1757).
pub(crate) fn value_specifiers(module: &Module, used: impl Fn(&str) -> bool) -> BTreeSet<String> {
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
                    found.insert(import.src.value.to_string_lossy().into_owned());
                }
            }
            ModuleDecl::ExportAll(export) if !export.type_only => {
                found.insert(export.src.value.to_string_lossy().into_owned());
            }
            ModuleDecl::ExportNamed(export) if !export.type_only => {
                if let Some(src) = &export.src
                    && (export.specifiers.is_empty()
                        || export.specifiers.iter().any(|specifier| {
                            !matches!(specifier, ExportSpecifier::Named(named) if named.is_type_only)
                        }))
                {
                    found.insert(src.value.to_string_lossy().into_owned());
                }
            }
            ModuleDecl::TsImportEquals(decl) if !decl.is_type_only => {
                if let TsModuleRef::TsExternalModuleRef(external) = &decl.module_ref
                    && used(decl.id.sym.as_ref())
                {
                    found.insert(external.expr.value.to_string_lossy().into_owned());
                }
            }
            _ => {}
        }
    }
    found.extend(literal_loads(module));
    found
}

/// Every specifier `module` hands `import()` or `require` as a literal,
/// anywhere in the file: in a function, in a chain (`require("x").config()`),
/// or bound at module scope. A computed specifier (`require(name)`, a
/// template with a hole) names no module and is not here.
///
/// [`value_specifiers`] reads loads through it (carrick#1727).
/// [`module_loads`] answers another question (which loads bind nothing) and
/// keeps its own rules.
fn literal_loads(module: &Module) -> BTreeSet<String> {
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
    let mut loads = Loads {
        found: BTreeSet::new(),
    };
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
pub(crate) fn literal_specifier(expr: &Expr) -> Option<String> {
    match crate::graphql_document_sites::unwrap_expression(expr) {
        Expr::Lit(Lit::Str(literal)) => Some(literal.value.to_string_lossy().into_owned()),
        Expr::Tpl(tpl) if tpl.exprs.is_empty() => tpl.quasis.first().map(|quasi| {
            quasi
                .cooked
                .as_ref()
                .map(|cooked| cooked.to_string_lossy().into_owned())
                .unwrap_or_else(|| quasi.raw.to_string())
        }),
        _ => None,
    }
}

// FROZEN (old path, 2026-10-21): this function only (the package is the
// import specifier as written). Deletions and safety fixes only. Fixes for
// wrong or missing rows go in the compiler's position rows (the `call` or
// `member` row's `decl_package`, carrick#2413 slice E). Delete at carrick#2413
// slice F, once the unresolved-import (#2573) and rows-join budget (#2574)
// fallbacks count zero (carrick#2413).
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
                    shared: None,
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

/// What a body says of its names ([`Scope::body_names`]).
#[derive(Debug, Clone, Default)]
struct BodyNames {
    /// Declared once ([`Reassigned::declared_once`]).
    once: HashSet<String>,
    /// Assigned again anywhere in the body.
    written: HashSet<String>,
}

/// What an expression can read: the function's parameters and constants, the
/// enclosing class's fields, and the module's constants, each by the binding
/// an identifier resolves to (carrick#1648).
struct Scope<'a> {
    params: Vec<Option<BindingKey>>,
    /// Each binding that holds one key of an object parameter, with the
    /// parameter's position and the key (carrick#1950): a name a destructured
    /// parameter binds (`{ url }`, `{ url: target }`), or one the body unpacks
    /// from a plain parameter (`const { url } = options`). Never a binding
    /// the body assigns again, one with a default, a rest element or a
    /// nested pattern: each may hold something other than what the caller
    /// wrote under that key.
    keyed: HashMap<BindingKey, (usize, String)>,
    /// Each binding that holds one key of an object parameter or, where the
    /// caller's object leaves it out, the key's default (`{ version = 'v1' }`,
    /// `const { version = 'v1' } = options || {}`, carrick#3608), with the
    /// parameter's position and the key: read as that key by the values a
    /// request is composed of, and the default by the caller walk from
    /// [`FnIr::param_default_texts`]. Kept apart from `keyed`, whose
    /// readers (the guards) compare what the caller wrote under the key.
    defaulted: HashMap<BindingKey, (usize, String)>,
    /// The default each of [`Self::defaulted`] is given, as written, by the
    /// parameter's position and the key: what a request reads where the
    /// caller's object leaves the key out ([`FnIr::key_defaults`]).
    key_defaults: HashMap<(usize, String), Expr>,
    /// Each parameter, or key of an object parameter, the function gives a
    /// default (`prefix = '/api'`, `{ prefix = '/api' } = {}`): the
    /// parameter's position, the key (`None`: the parameter itself) and the
    /// default as written. Never a binding the body assigns again. Read by
    /// the receiver trace alone (carrick#2635), which states both what a
    /// caller hands in and the default.
    defaults: HashMap<BindingKey, (usize, Option<String>, Expr)>,
    /// The rest element of a parameter the body unpacks (`const { path,
    /// ...rest } = config`, or `= init ?? {}`), with the parameter's
    /// position and every key the pattern takes out (carrick#2519): it holds
    /// the caller's other keys. Never a binding declared twice, assigned
    /// again or written through ([`guards::WrittenThrough`], carrick#2674).
    rest_of: HashMap<BindingKey, (u8, usize, BTreeSet<String>)>,
    /// The plain parameters whose keys the body may read as the caller wrote
    /// them (`options.url`, `const { url } = options`): never one the body
    /// assigns again, nor one whose name the file writes through, nor one the
    /// function hands to `Object.assign` as its target (carrick#2674).
    key_objects: HashSet<BindingKey>,
    /// A builder's parameters the call hands one of the caller's
    /// [`Self::key_objects`] whole (`withDefaults(init)`), by the caller's
    /// position ([`Reader::builder_call`]): a spread of one in the builder's
    /// return is a spread of the caller's parameter ([`Reader::own_param`]),
    /// so the keys the caller's caller writes there stay read. Empty in
    /// every scope but a builder's.
    passed_params: HashMap<BindingKey, usize>,
    /// [`Captured::objects`]: an enclosing function's parameter whose keys
    /// this body reads (`props.endpoint` in a closure), read as
    /// [`Piece::Outer`] keys (carrick#1949).
    outer_objects: HashMap<BindingKey, (u8, usize)>,
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
    /// The HTTP verbs the function declares a plain parameter can be, by
    /// position (carrick#2049): `method: "POST" | "DELETE"`, or an alias of
    /// one the file declares. Never a parameter the body assigns again.
    param_verbs: HashMap<usize, Vec<String>>,
    /// What each local the body declares once can read as, one way per branch
    /// of the conditionals it holds (carrick#2050): only a local whose
    /// initialiser holds one is here.
    alts: HashMap<BindingKey, Alternatives>,
    /// Each `let` the body declares at its top level and writes again, with
    /// the ways the value resolver's flow says reach each of its reads
    /// (carrick#3679 S8): what [`Reader::alternatives`] reads it as.
    written_locals: HashMap<BindingKey, alternatives::WrittenLocal>,
    /// The names the function assigns again, anywhere in it: a test that
    /// reads one is not the same test twice (carrick#2051).
    reassigned: HashSet<String>,
    /// Where each local the body declares, and each the enclosing functions
    /// hold, comes from, as a guard reads it (carrick#2386). The send side
    /// never reads it.
    subjects: HashMap<BindingKey, Subject>,
    /// What each key of a destructured parameter, and each local the body
    /// declares once and never assigns again, may hold, the enclosing
    /// functions' included (carrick#2432). Read by discovery's value walk
    /// alone ([`resolve::ValueAt`]).
    held: HashMap<BindingKey, Vec<ValueAt>>,
    /// The functions the body declares once and never assigns again, by where
    /// each node starts, for what a `return` of the name hands back
    /// (carrick#2432 slice A2).
    declared_fns: HashMap<BindingKey, u32>,
    /// The names the body declares once, and those it assigns again, for the
    /// call walker's block declarators (carrick#3794): the same rule
    /// [`Reader::body`] reads its own top-level locals by. `None` at module
    /// scope, whose bindings the write census keys by binding.
    body_names: Option<BodyNames>,
    /// The names the function writes a member of (`url.pathname = …`),
    /// anywhere in it: a guard reads no such binding as what it was declared
    /// with (carrick#2386). The send side never reads it.
    written_through: HashSet<String>,
    fields: Option<&'a ClassFields>,
    module: &'a ModuleScope,
}

impl<'a> Scope<'a> {
    /// Where the class this scope is a member of starts: what
    /// `this.<field>` is read by ([`ValueAt::Field`]).
    fn class(&self) -> Option<u32> {
        self.fields.map(|fields| fields.class)
    }

    fn module(module: &'a ModuleScope) -> Self {
        Self {
            params: Vec::new(),
            keyed: HashMap::new(),
            defaulted: HashMap::new(),
            key_defaults: HashMap::new(),
            defaults: HashMap::new(),
            rest_of: HashMap::new(),
            key_objects: HashSet::new(),
            passed_params: HashMap::new(),
            outer_objects: HashMap::new(),
            locals: HashMap::new(),
            texts: HashMap::new(),
            local_receivers: HashMap::new(),
            let_makers: HashMap::new(),
            fetches: HashSet::new(),
            param_verbs: HashMap::new(),
            alts: HashMap::new(),
            written_locals: HashMap::new(),
            reassigned: HashSet::new(),
            written_through: HashSet::new(),
            subjects: HashMap::new(),
            held: HashMap::new(),
            declared_fns: HashMap::new(),
            body_names: None,
            fields: None,
            module,
        }
    }

    /// The position of the parameter `ident` names, when it names one.
    fn param_index(&self, ident: &Ident) -> Option<usize> {
        let key = ident_key(ident);
        self.params.iter().position(|p| p.as_ref() == Some(&key))
    }

    /// The parameters of `function` (a function or an arrow, whose
    /// parameters are `params`), as a scope reads them: each plain one by
    /// position, and what may be read through a key of one ([`Self::keyed`],
    /// [`Self::key_objects`], carrick#1950).
    ///
    /// A key is read only through a binding the function declares once and
    /// never assigns again, by `=`, by an update or as a destructuring
    /// target: any of those leaves it holding something the caller did not
    /// write.
    fn read_params<'p, N>(
        &mut self,
        params: impl IntoIterator<Item = &'p Pat>,
        function: &N,
        module: &ModuleScope,
        type_params: &HashSet<String>,
    ) where
        N: VisitWith<Declarations> + VisitWith<Reassigned> + VisitWith<guards::WrittenThrough>,
    {
        let declared = Declarations::of(function);
        let mut reassigned = Reassigned::default();
        function.visit_with(&mut reassigned);
        self.reassigned = reassigned.names.clone();
        self.written_through = guards::WrittenThrough::of(function);
        let settled = |binding: &BindingKey| {
            declared.binding_count(binding) == 1 && !reassigned.names.contains(&binding.0)
        };
        for (index, pat) in params.into_iter().enumerate() {
            let key = pat_key(pat);
            if let Some(key) = &key
                && settled(key)
                && !module.written_through(&key.0)
                && !self.written_through.contains(&key.0)
            {
                self.key_objects.insert(key.clone());
            }
            // `{ url }: Options = {}`: the pattern is the left side.
            let pattern = match pat {
                Pat::Assign(assign) => &*assign.left,
                other => other,
            };
            // `method: "POST" | "DELETE"`: every verb the request can send
            // (carrick#2049). An optional parameter may arrive as nothing.
            if let Pat::Ident(ident) = pattern
                && !ident.id.optional
                && let Some(annotation) = &ident.type_ann
                && key.as_ref().is_some_and(settled)
                && let Some(verbs) = module
                    .verb_aliases
                    .verbs_of(&annotation.type_ann, type_params)
            {
                self.param_verbs.insert(index, verbs);
            }
            // `prefix = '/api'`: the parameter, or one key of an object
            // parameter, with a default (carrick#2635).
            if let Pat::Assign(assign) = pat
                && let Some(binding) = &key
                && settled(binding)
            {
                self.defaults
                    .insert(binding.clone(), (index, None, (*assign.right).clone()));
            }
            self.params.push(key);
            if let Pat::Object(object) = pattern {
                for (binding, key) in keyed_names(object) {
                    if settled(&binding) {
                        self.keyed.insert(binding, (index, key));
                    }
                }
                // `{ version = 'v1' }`: the caller's key, or the default
                // (`ParamValues::default_texts` holds its text).
                for (binding, key, default) in defaulted_names(object) {
                    if settled(&binding) {
                        self.key_defaults
                            .insert((index, key.clone()), default.clone());
                        self.defaulted.insert(binding, (index, key));
                    }
                }
                for prop in &object.props {
                    if let ObjectPatProp::Assign(assign) = prop
                        && let Some(default) = &assign.value
                    {
                        let binding = ident_key(&assign.key.id);
                        if settled(&binding) {
                            self.defaults.insert(
                                binding,
                                (
                                    index,
                                    Some(assign.key.id.sym.to_string()),
                                    (**default).clone(),
                                ),
                            );
                        }
                    }
                }
            }
        }
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

    /// [`call_binding`](Self::call_binding) for a library site, which also
    /// reads a module binding that table dropped because a scope below
    /// declares its name again (carrick#3559). A use that names a binding a
    /// scope below declares is that binding's, so it reads nothing: the
    /// count is the rule the `fetch` check uses, and without the resolver
    /// every key is `(name, empty)`, so the count is not zero and the name
    /// stays dropped. Separate from `call_binding` because that feeds the
    /// old path's request summaries, which stay as they are.
    fn site_binding(&self, ident: &Ident) -> Option<ClientBinding> {
        self.call_binding(ident).or_else(|| {
            if self.param_index(ident).is_some() {
                return None;
            }
            let key = ident_key(ident);
            if self.local_receivers.contains_key(&key) || self.locals.contains_key(&key) {
                return None;
            }
            let dropped = self.module.dropped.get(ident.sym.as_ref())?;
            if self.module.declared_below.binding_count(&key) != 0 {
                return None;
            }
            dropped.site.clone()
        })
    }
}

struct Reader<'a> {
    source_map: &'a Lrc<SourceMap>,
    /// How many builder calls deep [`Reader::eval`] is reading
    /// (carrick#1562): a builder that calls itself stops at the cap.
    builder_depth: std::cell::Cell<usize>,
    /// The object literals some function of the module returns, by span
    /// ([`crate::return_flow::returned_objects`], carrick#3470).
    returned_objects: HashSet<(u32, u32)>,
    /// What each function of the module returns, by the span `(lo, hi)` of
    /// its node, where that is one call's value read one way
    /// ([`crate::return_flow::return_shapes`], carrick#3823).
    return_shapes: HashMap<(u32, u32), crate::return_flow::ReturnShape>,
    /// While above 0, [`Reader::eval`] keeps the pieces it reads each
    /// expression as in [`Self::arg_pieces`] ([`Reader::recording`]).
    recording_depth: std::cell::Cell<usize>,
    /// What [`FileIr::arg_pieces`] keeps, gathered as the file is read.
    arg_pieces: std::cell::RefCell<HashMap<(u32, u32), Vec<Piece>>>,
    /// Each function-local binding the scope holds ([`Scope::held`]), by
    /// its initialiser's span ([`FileIr::held_inits`], carrick#3538 M1).
    held_inits: std::cell::RefCell<HashMap<(u32, u32), HeldInit>>,
    /// Each declarator a nested block writes ([`FileIr::block_bindings`],
    /// carrick#3794).
    block_bindings: std::cell::RefCell<HashMap<(u32, String), BlockBinding>>,
    /// What each function returns, by where its node starts
    /// ([`FileIr::block_returns`], carrick#3794).
    function_returns: std::cell::RefCell<HashMap<u32, Vec<ValueAt>>>,
    /// The first guard read at each comparison, by its site `(lo, hi)`, as
    /// the walk records it ([`FnIr::guards`]): what a test on a written
    /// local's way reads as at a send further down ([`Reader::way_guard`],
    /// carrick#3679).
    way_tests: std::cell::RefCell<HashMap<(u32, u32), GuardIr>>,
}

/// The binding a function-local value is held in, for the procedures walk
/// (carrick#3538 M1): the name, and whether it is a `const` (one that is
/// not is never named a router's key path from).
#[derive(Debug, Clone, PartialEq, Eq)]
struct HeldInit {
    name: String,
    constant: bool,
}

/// A declarator no binding table row holds (carrick#3794): what its
/// initialiser may hold and whether it is a `const`.
#[derive(Debug, Clone)]
struct BlockBinding {
    init: Vec<ValueAt>,
    constant: bool,
}

impl Reader<'_> {
    /// Every way `expr` reads, one list of pieces each, where it holds a
    /// default or a conditional (carrick#2635); empty where it reads one
    /// way. `eval` reads the same expressions as one opaque value, which
    /// the send side keys rows on, so this is a separate reading, for the
    /// receiver trace alone.
    fn stated_ways(&self, expr: &Expr, scope: &Scope<'_>) -> Vec<Way> {
        // Most arguments state one value: read no further.
        let branching = match unwrap_value(expr) {
            Expr::Bin(bin) => matches!(bin.op, BinaryOp::NullishCoalescing | BinaryOp::LogicalOr),
            Expr::Cond(_) => true,
            Expr::Ident(ident) => scope.defaults.contains_key(&ident_key(ident)),
            _ => false,
        };
        if !branching {
            // A conditional reached through a local or joined with text
            // (`apiPrefix + "/x"`, `apiPrefix = base ? … : "/api"`): each
            // reading the send side reads it as (carrick#2386).
            return self
                .alternatives(expr, scope)
                .map(|alts| alts.chosen_texts())
                .unwrap_or_default()
                .into_iter()
                .map(Way::stated)
                .collect();
        }
        let ways = self.ways(expr, scope);
        if ways.len() > 1 { ways } else { Vec::new() }
    }

    fn ways(&self, expr: &Expr, scope: &Scope<'_>) -> Vec<Way> {
        match unwrap_value(expr) {
            // `opts.prefix ?? '/api'`: the left side, then the default.
            Expr::Bin(bin) if bin.op == BinaryOp::NullishCoalescing => {
                let mut ways = self.ways(&bin.left, scope);
                ways.extend(Way::defaulted(self.ways(&bin.right, scope)));
                ways
            }
            // `opts.prefix || '/api'`.
            Expr::Bin(bin) if bin.op == BinaryOp::LogicalOr => {
                let mut ways = self.ways(&bin.left, scope);
                ways.extend(Way::defaulted(self.ways(&bin.right, scope)));
                ways
            }
            // `opts.legacy ? '/legacy' : '/api'`: each branch.
            Expr::Cond(cond) => {
                let mut ways = self.ways(&cond.cons, scope);
                ways.extend(self.ways(&cond.alt, scope));
                ways
            }
            // A parameter with a default: what a caller hands in, then
            // the default.
            Expr::Ident(ident) if scope.defaults.contains_key(&ident_key(ident)) => {
                let (index, key, default) = &scope.defaults[&ident_key(ident)];
                let name = ident.sym.to_string();
                let own = match key {
                    Some(key) => Piece::ParamKey(*index, key.clone(), name),
                    None => Piece::Param(*index, name),
                };
                let mut ways = vec![Way::stated(vec![own])];
                ways.extend(Way::defaulted(self.ways(default, scope)));
                ways
            }
            other => vec![Way::stated(self.eval(other, scope).pieces())],
        }
    }

    /// What the receiver trace reads at a call whose callee is `callee`,
    /// handed `args` ([`InstanceIr`]). The receiver's value is read only
    /// for a name or a member read: a call's value is no parameter.
    fn instance_ir(
        &self,
        callee: Option<&Expr>,
        args: &[ExprOrSpread],
        scope: &Scope<'_>,
    ) -> InstanceIr {
        let object = callee.and_then(|callee| match unwrap_value(callee) {
            Expr::Member(member) => Some(&*member.obj),
            Expr::OptChain(OptChainExpr { base, .. }) => match &**base {
                OptChainBase::Member(member) => Some(&*member.obj),
                OptChainBase::Call(_) => None,
            },
            _ => None,
        });
        let receiver_value = object
            .filter(|object| matches!(unwrap_value(object), Expr::Ident(_) | Expr::Member(_)))
            .map(|object| self.eval(unwrap_value(object), scope));
        let bound = guards::Bound::default();
        let receiver = object
            .map(|object| self.value_of(object, scope, &bound))
            .unwrap_or_default();
        // Each argument is lowered on its own: one after a spread is still
        // the one written there (`app.use(...middleware, router)`).
        let args = args
            .iter()
            .map(|arg| {
                if arg.spread.is_some() {
                    return ArgHeld::default();
                }
                let span = unwrap_value(&arg.expr).span();
                let whole = self.value_of(&arg.expr, scope, &bound);
                // An object literal's plain keys (`{ app }`), as its
                // lowering states them.
                let keys = match whole.as_slice() {
                    [ValueAt::Object { fields, .. }] => fields.clone(),
                    _ => BTreeMap::new(),
                };
                let alts = match unwrap_value(&arg.expr) {
                    Expr::Object(object) => ArgAlts {
                        whole: Vec::new(),
                        keys: object
                            .props
                            .iter()
                            .filter_map(|prop| {
                                let PropOrSpread::Prop(prop) = prop else {
                                    return None;
                                };
                                let (name, value) = match &**prop {
                                    Prop::Shorthand(ident) => {
                                        (ident.sym.to_string(), Expr::Ident(ident.clone()))
                                    }
                                    Prop::KeyValue(entry) => (
                                        match &entry.key {
                                            PropName::Ident(name) => name.sym.to_string(),
                                            PropName::Str(text) => {
                                                text.value.to_string_lossy().into_owned()
                                            }
                                            _ => return None,
                                        },
                                        (*entry.value).clone(),
                                    ),
                                    _ => return None,
                                };
                                let ways = self.stated_ways(&value, scope);
                                (!ways.is_empty()).then_some((name, ways))
                            })
                            .collect(),
                    },
                    expr => ArgAlts {
                        whole: self.stated_ways(expr, scope),
                        keys: BTreeMap::new(),
                    },
                };
                ArgHeld {
                    whole,
                    keys,
                    span: Some((span.lo.0, span.hi.0)),
                    alts,
                }
            })
            .collect();
        InstanceIr {
            receiver_value,
            receiver,
            args,
            folds: Vec::new(),
        }
    }

    fn text(&self, span: Span) -> String {
        self.source_map
            .span_to_snippet(span)
            .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    }

    /// The text of the `new` expression `expr` is, when it is one: the text
    /// the reading of an unread construction stops at ([`Self::new_url`]),
    /// which a binding holding it keeps ([`bound`]).
    fn construction_text(&self, expr: &Expr) -> Option<String> {
        match crate::graphql_document_sites::unwrap_expression(expr) {
            Expr::New(new) => Some(self.text(new.span)),
            _ => None,
        }
    }

    /// What the function whose node is `span` returns
    /// ([`Reader::return_shapes`]).
    fn return_shape(&self, span: Span) -> Option<crate::return_flow::ReturnShape> {
        self.return_shapes.get(&(span.lo.0, span.hi.0)).cloned()
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

    /// The library sites a method's decorators write (carrick#3043): each
    /// decorator that calls an export (`@OnMessage("x")`) is a site of it in
    /// the decorate form, read in the module's scope, where a decorator is
    /// evaluated, and placed at the method's name, `name`, as a route
    /// decorator's row is. The method is the decorated handler, so the
    /// site is the method's own. `decorated` is the method's name when its
    /// key is an identifier or a string literal: what a decorator that names
    /// nothing leaves the method to name (SB1).
    fn decorator_sites(
        &self,
        decorators: &[Decorator],
        name: Span,
        decorated: Option<&str>,
        module: &ModuleScope,
        ir: &mut FnIr,
    ) {
        let scope = Scope::module(module);
        for decorator in decorators {
            let Expr::Call(call) =
                crate::graphql_document_sites::unwrap_expression(&decorator.expr)
            else {
                continue;
            };
            let Callee::Expr(callee) = &call.callee else {
                continue;
            };
            if let Some(mut site) =
                self.library_site(call.span, MakerForm::Decorate, callee, &call.args, &scope)
            {
                site.op_line = self.line(name);
                site.decorated = decorated.map(str::to_string);
                ir.library.push(site);
            } else if let Expr::Ident(factory) =
                crate::graphql_document_sites::unwrap_expression(callee)
            {
                // No export answers to the name: it may be a function of
                // the service that composes one ([`own_decorators`]).
                ir.own_decorators.push(own_decorators::OwnDecorator {
                    callee: factory.sym.to_string(),
                    call: self.site(call.span),
                    line: self.line(name),
                });
            }
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
        let passed = module.constructions.get(name).map(Vec::as_slice);
        let mut fields = self.class_fields(class, module, redeclared, passed);
        fields.query_builders =
            class_query_builders(class, &fields, module.subclass_methods.get(name));
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
        // A member's parameter typed through the class's own type parameter
        // is typed by whatever the caller passes (carrick#1782).
        let class_type_params = type_param_names(class.type_params.as_deref());
        let in_class = |mut ir: FnIr| {
            ir.type_params.extend(class_type_params.iter().cloned());
            ir
        };
        // Every maker write to a field, for the message-role field rule
        // (carrick#1665): the initialisers' and the constructor's here, each
        // member's as it is read.
        let mut field_writes = self.initialiser_and_constructor_writes(class, &fields, module);
        // Every value written to a field of an instance (carrick#3504 S4c):
        // the initialisers' here, each member's as it is read.
        let mut placed = self.initialiser_values(class, &fields, module);
        for member in &class.body {
            match member {
                ClassMember::Method(method) if !matches!(method.kind, MethodKind::Setter) => {
                    let Some(member_name) = prop_name(&method.key) else {
                        continue;
                    };
                    let mut ir =
                        self.function(&method.function, table(method.is_static), module, &none);
                    self.decorator_sites(
                        &method.function.decorators,
                        method.key.span(),
                        Some(&member_name),
                        module,
                        &mut ir,
                    );
                    library_sites::take_field_writes(&mut ir, &mut field_writes);
                    library_sites::take_field_values(&mut ir, false, &mut placed);
                    if method.kind == MethodKind::Getter && !method.is_static {
                        file.class_getters.insert(
                            (fields.class, member_name.clone()),
                            key(&member_name, false),
                        );
                    }
                    file.functions
                        .insert(key(&member_name, method.is_static), in_class(ir));
                }
                ClassMember::PrivateMethod(method)
                    if !matches!(method.kind, MethodKind::Setter) =>
                {
                    let member_name = format!("#{}", method.key.name);
                    let mut ir =
                        self.function(&method.function, table(method.is_static), module, &none);
                    self.decorator_sites(
                        &method.function.decorators,
                        method.key.span,
                        None,
                        module,
                        &mut ir,
                    );
                    library_sites::take_field_writes(&mut ir, &mut field_writes);
                    library_sites::take_field_values(&mut ir, false, &mut placed);
                    if method.kind == MethodKind::Getter && !method.is_static {
                        file.class_getters.insert(
                            (fields.class, member_name.clone()),
                            key(&member_name, false),
                        );
                    }
                    file.functions
                        .insert(key(&member_name, method.is_static), in_class(ir));
                }
                ClassMember::Constructor(ctor) => {
                    let pats: Vec<Pat> = ctor
                        .params
                        .iter()
                        .map(|param| match param {
                            ParamOrTsParamProp::Param(param) => param.pat.clone(),
                            ParamOrTsParamProp::TsParamProp(prop) => match &prop.param {
                                TsParamPropParam::Ident(ident) => Pat::Ident(ident.clone()),
                                TsParamPropParam::Assign(assign) => Pat::Assign(assign.clone()),
                            },
                        })
                        .collect();
                    file.constructor_defaults.extend(self.param_functions(
                        pats.iter(),
                        None,
                        module,
                        &none,
                    ));
                    // The body, read as a member's is, under its own key
                    // (carrick#3527): what a `new` or a `super(…)` runs. Its
                    // field writes are already counted from the constructor
                    // above.
                    if let Some(body) = &ctor.body {
                        let function = Function {
                            params: pats
                                .into_iter()
                                .map(|pat| Param {
                                    span: pat.span(),
                                    decorators: Vec::new(),
                                    pat,
                                })
                                .collect(),
                            decorators: Vec::new(),
                            span: ctor.span,
                            ctxt: ctor.ctxt,
                            body: Some(body.clone()),
                            is_generator: false,
                            is_async: false,
                            type_params: None,
                            return_type: None,
                            this_param: None,
                        };
                        let mut ir = self.function(&function, table(false), module, &none);
                        library_sites::take_field_writes(&mut ir, &mut Vec::new());
                        library_sites::take_field_values(&mut ir, true, &mut placed);
                        ir.constructor = true;
                        let constructor = format!("{name}.constructor");
                        let mut lines: Vec<u32> = class
                            .decorators
                            .iter()
                            .map(|decorator| decorator.span)
                            .chain([class.span, ident.span, ctor.span])
                            .map(|span| self.site(span).line)
                            .collect();
                        lines.sort_unstable();
                        lines.dedup();
                        for line in lines {
                            file.constructor_keys
                                .entry(line)
                                .or_default()
                                .push((name.to_string(), constructor.clone()));
                        }
                        file.functions.insert(constructor, in_class(ir));
                    }
                }
                ClassMember::ClassProp(prop) => {
                    let (Some(member_name), Some(init)) = (prop_name(&prop.key), &prop.value)
                    else {
                        continue;
                    };
                    let (mut ir, at) = match &**init {
                        Expr::Arrow(arrow) => (
                            self.arrow(arrow, table(prop.is_static), module, &none),
                            key(&member_name, prop.is_static),
                        ),
                        Expr::Fn(fn_expr) => (
                            self.function(&fn_expr.function, table(prop.is_static), module, &none),
                            key(&member_name, prop.is_static),
                        ),
                        // Any other initialiser is a body that runs when the
                        // class is built (carrick#3614 A4): read as the
                        // expression of a function no one calls, under a key
                        // no callee can name, so the calls it writes and the
                        // functions written inside it are read as any body's.
                        _ => {
                            let body = ArrowExpr {
                                span: init.span(),
                                ctxt: Default::default(),
                                params: Vec::new(),
                                body: Box::new(ArrowFunctionBody::Expr(init.clone())),
                                is_async: false,
                                is_generator: false,
                                type_params: None,
                                return_type: None,
                            };
                            (
                                self.arrow(&body, table(prop.is_static), module, &none),
                                initialiser_key(name, &member_name, prop.is_static),
                            )
                        }
                    };
                    library_sites::take_field_writes(&mut ir, &mut field_writes);
                    library_sites::take_field_values(&mut ir, false, &mut placed);
                    file.functions.insert(at, in_class(ir));
                }
                _ => {}
            }
        }
        library_sites::place_field_values(
            &ident_key(ident),
            &module.class_this,
            placed,
            &mut fields.origins,
        );
        let receivers =
            library_sites::field_receivers(&ident_key(ident), &module.class_this, field_writes);
        for (field, client) in receivers {
            file.field_receivers.insert((fields.class, field), client);
        }
        // A field is its constructor parameter, for the instance walk, only
        // where nothing else may set it (carrick#1670 T3).
        for (field, from) in &mut fields.origins {
            if from.parameter.is_some()
                && !library_sites::constructor_only(&ident_key(ident), &module.class_this, field)
            {
                from.parameter = None;
            }
        }
        if !fields.origins.is_empty() {
            let mut lines: Vec<u32> = class
                .decorators
                .iter()
                .map(|decorator| decorator.span)
                .chain([class.span, ident.span])
                .map(|span| self.site(span).line)
                .collect();
            lines.sort_unstable();
            lines.dedup();
            file.class_origins.insert(
                fields.class,
                ClassOrigin {
                    name: name.to_string(),
                    lines,
                    fields: std::mem::take(&mut fields.origins),
                    defaults: std::mem::take(&mut fields.ctor_defaults),
                },
            );
        }
    }

    /// For each class only this file can construct, the value every
    /// construction passes at each constructor position (carrick#2452).
    ///
    /// A class qualifies when its declaration is not published, and its
    /// binding is used only as the callee of `new` ([`BindingUse::only_constructed`]),
    /// is declared nowhere below module scope, is not reassigned and is no
    /// JSX tag. Every `new C(…)` in the file must be the whole initialiser
    /// of a module-scope variable or the `export default` expression, with
    /// no spread argument: a construction anywhere else may pass anything,
    /// and the class would be read with a value it does not hold.
    ///
    /// A position holds a value when every construction passes one there and
    /// they are equal: text with no parameter, query or unknown piece, or a
    /// closed object of such values.
    fn constructions(
        &self,
        module: &Module,
        module_scope: &ModuleScope,
        jsx: &HashSet<String>,
        reassigned: &Reassigned,
    ) -> HashMap<String, Vec<Option<Value>>> {
        /// Every `new <name>(…)` anywhere in the file.
        struct Constructed(HashMap<String, usize>);
        impl Visit for Constructed {
            fn visit_new_expr(&mut self, new: &NewExpr) {
                if let Expr::Ident(ident) =
                    crate::graphql_document_sites::unwrap_expression(&new.callee)
                {
                    *self.0.entry(ident.sym.to_string()).or_default() += 1;
                }
                new.visit_children_with(self);
            }
        }
        let mut anywhere = Constructed(HashMap::new());
        module.visit_with(&mut anywhere);

        // Each `new C(…)` that is the whole initialiser of a module-scope
        // variable, or the default export.
        let mut at_module_scope: HashMap<String, Vec<&NewExpr>> = HashMap::new();
        fn note<'m>(expr: &'m Expr, into: &mut HashMap<String, Vec<&'m NewExpr>>) {
            if let Expr::New(new) = crate::graphql_document_sites::unwrap_expression(expr)
                && let Expr::Ident(ident) =
                    crate::graphql_document_sites::unwrap_expression(&new.callee)
            {
                into.entry(ident.sym.to_string()).or_default().push(new);
            }
        }
        for item in &module.body {
            if let Some(var) = module_var_decl(item) {
                for declarator in &var.decls {
                    if let Some(init) = &declarator.init {
                        note(init, &mut at_module_scope);
                    }
                }
            }
            if let ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(export)) = item {
                note(&export.expr, &mut at_module_scope);
            }
        }

        let scope = Scope::module(module_scope);
        let mut found = HashMap::new();
        for item in &module.body {
            // Not `export class`: another module may construct it.
            let ModuleItem::Stmt(Stmt::Decl(Decl::Class(class))) = item else {
                continue;
            };
            let name = class.ident.sym.as_ref();
            let Some(constructions) = at_module_scope.get(name) else {
                continue;
            };
            if anywhere.0.get(name) != Some(&constructions.len())
                || !module_scope
                    .uses
                    .get(name)
                    .is_some_and(BindingUse::only_constructed)
                || module_scope.declared_below.declares_name(name)
                || reassigned.names.contains(name)
                || jsx.contains(name)
                || constructions
                    .iter()
                    .any(|new| new.args.iter().flatten().any(|arg| arg.spread.is_some()))
            {
                continue;
            }
            let passed: Vec<Vec<Value>> = constructions
                .iter()
                .map(|new| {
                    new.args
                        .iter()
                        .flatten()
                        .map(|arg| self.eval(&arg.expr, &scope))
                        .collect()
                })
                .collect();
            let positions = passed.iter().map(Vec::len).max().unwrap_or(0);
            let agreed = (0..positions)
                .map(|position| {
                    let first = passed[0].get(position)?;
                    (states_a_value(first)
                        && passed.iter().all(|args| args.get(position) == Some(first)))
                    .then(|| first.clone())
                })
                .collect();
            found.insert(name.to_string(), agreed);
        }
        found
    }

    /// Each field the class assigns exactly once, in its constructor's own
    /// statements or its initialiser, and nowhere else. A constructor
    /// parameter read into a field is opaque, unless every construction of a
    /// class only this file can construct passes one value at its position
    /// (`passed`, [`Reader::constructions`]).
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
        passed: Option<&[Option<Value>]>,
    ) -> ClassFields {
        let mut fields: HashMap<String, Value> = HashMap::new();
        let mut receivers: HashMap<String, ClientRef> = HashMap::new();
        let mut contested: HashSet<String> = HashSet::new();
        let mut nested_writes: HashSet<String> = HashSet::new();
        // Fields set to the platform's `fetch`, or to a constructor parameter
        // whose default it is (carrick#1562).
        let mut fetches: HashSet<String> = HashSet::new();
        // Fields set from a constructor parameter, read through the class's
        // constructions once every module is (carrick#2452).
        let mut origins: HashMap<String, constructions::FieldOrigin> = HashMap::new();
        let mut ctor_defaults = Vec::new();
        // The `new` expression each field is set to, where it is set to one
        // ([`bound`]).
        let mut constructions: HashMap<String, String> = HashMap::new();
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
                        if let Some(text) = self.construction_text(init) {
                            constructions.insert(name.clone(), text);
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
                        if let Some(text) = self.construction_text(init) {
                            constructions.insert(name.clone(), text);
                        }
                        let scope = Scope::module(module);
                        let value = self.eval(init, &scope);
                        let client = self.factory_call(init, &scope);
                        assign(name, value, client, &mut fields);
                    }
                }
                ClassMember::Constructor(ctor) => {
                    (origins, ctor_defaults) = self.ctor_origins(ctor, class.span.lo.0, module);
                    let mut scope = Scope::module(module);
                    let mut fetch_params: HashSet<BindingKey> = HashSet::new();
                    for (position, param) in ctor.params.iter().enumerate() {
                        // What every construction passes here, when they
                        // agree (carrick#2452).
                        let given = passed.and_then(|passed| passed.get(position)?.as_ref());
                        match param {
                            ParamOrTsParamProp::TsParamProp(prop) => match &prop.param {
                                TsParamPropParam::Ident(ident) => {
                                    let name = ident.id.sym.to_string();
                                    // A constructor parameter no call of the
                                    // method fills: the value the
                                    // constructions pass, else opaque.
                                    let value = match given {
                                        Some(given) => {
                                            scope
                                                .locals
                                                .insert(ident_key(&ident.id), given.clone());
                                            given.clone()
                                        }
                                        None => {
                                            Value::hole(name.clone(), HoleReason::UnreadExpression)
                                        }
                                    };
                                    assign(name, value, None, &mut fields);
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
                                if let Some(given) = given {
                                    bind_given(&param.pat, given, &mut scope.locals);
                                }
                            }
                        }
                    }
                    let Some(body) = &ctor.body else {
                        continue;
                    };
                    // A constructor parameter no construction states stays
                    // opaque: it is read by name, not as a position a call
                    // site could fill.
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
                                if let Some(text) = self.construction_text(value) {
                                    constructions.insert(name.clone(), text);
                                }
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
        origins.retain(|name, _| !contested.contains(name));
        for name in contested {
            receivers.remove(&name);
            fields.insert(
                name.clone(),
                Value::hole(format!("this.{name}"), HoleReason::Reassigned),
            );
        }
        let receivers = receivers
            .into_iter()
            .map(|(name, client)| {
                let client = module.with_uses(client, &format!("this.{name}"), None);
                (name, client)
            })
            .collect();
        // A field holding only a value the class is handed (`this.baseUrl =
        // baseUrl`, a parameter property) is written the way the request
        // writes it; one the class BUILDS is read through.
        // A field whose text is a constructor parameter links to the field,
        // which its class's constructions fill once every module is read
        // (carrick#2452).
        let values = fields
            .into_iter()
            .map(|(name, value)| {
                let mut value = bound(
                    format!("this.{name}"),
                    value,
                    constructions.get(&name).map(String::as_str),
                );
                if origins.get(&name).is_some_and(|from| from.text.is_some())
                    && let Value::Str(pieces) = &mut value
                    && let [Piece::Opaque { text, link, .. }] = pieces.as_mut_slice()
                    && *text == format!("this.{name}")
                    && link.is_none()
                {
                    *link = Some(Box::new(Link::Field(FieldLink {
                        class: class.span.lo.0,
                        field: name.clone(),
                    })));
                }
                (name, value)
            })
            .collect();
        ClassFields {
            class: class.span.lo.0,
            values,
            receivers,
            fetches,
            query_builders: HashSet::new(),
            origins,
            ctor_defaults,
        }
    }

    fn function(
        &self,
        function: &Function,
        fields: Option<&ClassFields>,
        module: &ModuleScope,
        captured: &Captured,
    ) -> FnIr {
        let mut fetches = captured.fetches.clone();
        for param in &function.params {
            fetch_bindings(&param.pat, module, &mut fetches);
        }
        let mut scope = Scope {
            params: Vec::new(),
            keyed: HashMap::new(),
            defaulted: HashMap::new(),
            key_defaults: HashMap::new(),
            defaults: HashMap::new(),
            rest_of: captured.rests.clone(),
            key_objects: HashSet::new(),
            passed_params: HashMap::new(),
            outer_objects: captured.objects.clone(),
            locals: captured.values.clone(),
            texts: captured.texts.clone(),
            local_receivers: inherited_receivers(captured, function),
            let_makers: HashMap::new(),
            fetches,
            param_verbs: HashMap::new(),
            alts: HashMap::new(),
            written_locals: HashMap::new(),
            reassigned: HashSet::new(),
            written_through: HashSet::new(),
            subjects: captured.subjects.clone(),
            held: captured.held.clone(),
            declared_fns: HashMap::new(),
            body_names: None,
            fields,
            module,
        };
        scope.read_params(
            function.params.iter().map(|param| &param.pat),
            function,
            module,
            &type_param_names(function.type_params.as_deref()),
        );
        let values = self.param_values(function.params.iter().map(|param| &param.pat), &scope);
        scope.held.extend(values.held);
        let param_functions = self.param_functions(
            function.params.iter().map(|param| &param.pat),
            fields,
            module,
            captured,
        );
        let mut ir = FnIr {
            params: ParamDecl::all(
                function.params.iter().map(|param| &param.pat),
                function.body.as_ref(),
                self.source_map,
            ),
            type_params: type_param_names(function.type_params.as_deref()),
            site: Some(self.site(function.span)),
            return_shape: self.return_shape(function.span),
            param_defaults: values.defaults,
            param_default_texts: values.default_texts,
            param_names: values.names,
            local_bindings: values.defaulted_names,
            param_functions,
            ..FnIr::default()
        };
        match &function.body {
            Some(body) => self.body(&body.stmts, &mut scope, &mut ir),
            None => ir.bodyless = true,
        }
        ir.key_defaults = self.key_defaults(&scope);
        ir.settle_returned(function.is_async, function.is_generator);
        self.keep_returns(function.span.lo.0, &ir);
        ir
    }

    /// FALLBACK (carrick#3794): what the function starting at `start`
    /// returns, for [`FileIr::block_returns`].
    fn keep_returns(&self, start: u32, ir: &FnIr) {
        if !ir.returned_values.is_empty() {
            self.function_returns
                .borrow_mut()
                .insert(start, ir.returned_values.clone());
        }
    }

    /// The text each default of [`Scope::key_defaults`] states, where it is
    /// text alone (carrick#3608).
    fn key_defaults(&self, scope: &Scope<'_>) -> BTreeMap<(usize, String), Vec<Piece>> {
        scope
            .key_defaults
            .iter()
            .filter_map(|(at, default)| match self.eval(default, scope) {
                Value::Str(pieces)
                    if !pieces.is_empty()
                        && pieces.iter().all(|piece| matches!(piece, Piece::Lit(_))) =>
                {
                    Some((at.clone(), pieces))
                }
                _ => None,
            })
            .collect()
    }

    fn arrow(
        &self,
        arrow: &ArrowExpr,
        fields: Option<&ClassFields>,
        module: &ModuleScope,
        captured: &Captured,
    ) -> FnIr {
        let mut fetches = captured.fetches.clone();
        for param in &arrow.params {
            fetch_bindings(param, module, &mut fetches);
        }
        let mut scope = Scope {
            params: Vec::new(),
            keyed: HashMap::new(),
            defaulted: HashMap::new(),
            key_defaults: HashMap::new(),
            defaults: HashMap::new(),
            rest_of: captured.rests.clone(),
            key_objects: HashSet::new(),
            passed_params: HashMap::new(),
            outer_objects: captured.objects.clone(),
            locals: captured.values.clone(),
            texts: captured.texts.clone(),
            local_receivers: inherited_receivers(captured, arrow),
            let_makers: HashMap::new(),
            fetches,
            param_verbs: HashMap::new(),
            alts: HashMap::new(),
            written_locals: HashMap::new(),
            reassigned: HashSet::new(),
            written_through: HashSet::new(),
            subjects: captured.subjects.clone(),
            held: captured.held.clone(),
            declared_fns: HashMap::new(),
            body_names: None,
            fields,
            module,
        };
        scope.read_params(
            &arrow.params,
            arrow,
            module,
            &type_param_names(arrow.type_params.as_deref()),
        );
        let values = self.param_values(&arrow.params, &scope);
        scope.held.extend(values.held);
        let param_functions = self.param_functions(&arrow.params, fields, module, captured);
        let mut ir = FnIr {
            params: ParamDecl::all(&arrow.params, Some(&*arrow.body), self.source_map),
            type_params: type_param_names(arrow.type_params.as_deref()),
            site: Some(self.site(arrow.span)),
            return_shape: self.return_shape(arrow.span),
            param_defaults: values.defaults,
            param_default_texts: values.default_texts,
            param_names: values.names,
            local_bindings: values.defaulted_names,
            param_functions,
            ..FnIr::default()
        };
        match &*arrow.body {
            ArrowFunctionBody::FunctionBody(block) => self.body(&block.stmts, &mut scope, &mut ir),
            // The expression body is what the arrow returns, where it starts
            // ([`BindingUses::visit_arrow_expr`] keys it the same way).
            ArrowFunctionBody::Expr(expr) => {
                let returned = self.returned_instances(expr, &scope);
                ir.returns.push((expr.span().lo.0, returned));
                ir.returned_values
                    .extend(self.value_of(expr, &scope, &guards::Bound::default()));
                ir.returns_tests.push(guards::is_test(expr));
                self.walk_returned(expr, &scope, &mut ir);
            }
        }
        ir.key_defaults = self.key_defaults(&scope);
        ir.settle_returned(arrow.is_async, arrow.is_generator);
        self.keep_returns(arrow.span.lo.0, &ir);
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
        let mut written_through = HashSet::new();
        for stmt in stmts {
            stmt.visit_with(&mut reassigned);
            stmt.visit_with(&mut declared);
            written_through.extend(guards::WrittenThrough::of(stmt));
        }
        // A parameter the body assigns again may hold anything.
        scope
            .fetches
            .retain(|(name, _)| !reassigned.names.contains(name));
        scope.body_names = Some(BodyNames {
            once: declared
                .names()
                .filter(|name| reassigned.declared_once(&declared, name))
                .map(str::to_string)
                .collect(),
            written: reassigned.names.clone(),
        });
        // A function declared in the body, once and never assigned again, is
        // the function a `return` of its name hands back, wherever the
        // declaration sits: it is hoisted (carrick#2432 slice A2). Only a
        // `return` reads it: a call of it in the body is a call of code this
        // pass does not read.
        for stmt in stmts {
            if let Stmt::Decl(Decl::Fn(declared_fn)) = stmt
                && !reassigned.names.contains(declared_fn.ident.sym.as_ref())
                && declared.binding_count(&ident_key(&declared_fn.ident)) == 1
            {
                scope.declared_fns.insert(
                    ident_key(&declared_fn.ident),
                    declared_fn.function.span.lo.0,
                );
            }
        }
        for (index, stmt) in stmts.iter().enumerate() {
            if let Stmt::Decl(Decl::Var(var)) = stmt {
                for declarator in &var.decls {
                    if let Some(init) = &declarator.init {
                        self.walk(init, scope, ir);
                    }
                    // Where the binding's value comes from, for a guard that
                    // reads it (carrick#2386).
                    self.bind_subjects(
                        &declarator.name,
                        declarator.init.as_deref(),
                        scope,
                        &reassigned,
                        &declared,
                    );
                    // What a local declared once and never assigned again may
                    // hold, for a call through it (carrick#2432).
                    if let (Pat::Ident(ident), Some(init)) = (&declarator.name, &declarator.init)
                        && !reassigned.names.contains(ident.id.sym.as_ref())
                        && declared.binding_count(&ident_key(&ident.id)) == 1
                    {
                        let values = self.value_of(init, scope, &guards::Bound::default());
                        // Another `new` holds nothing these readers state
                        // (only the guards read it, carrick#3504 S7b-2): the
                        // name stays a read the rows answer.
                        if values.iter().any(|value| {
                            !matches!(value, ValueAt::Unread | ValueAt::Constructed { .. })
                        }) {
                            scope.held.insert(ident_key(&ident.id), values);
                            // Which binding a value read through the name
                            // is held in (carrick#3538 M1).
                            let at = unwrap_value(init).span();
                            self.held_inits.borrow_mut().insert(
                                (at.lo.0, at.hi.0),
                                HeldInit {
                                    name: ident.id.sym.to_string(),
                                    constant: var.kind == VarDeclKind::Const,
                                },
                            );
                        }
                    }
                    // One the body writes again is every value written to
                    // it: the value resolver's binding table reads it, with
                    // the module's write census (carrick#3504 S1b). Every
                    // other local the body declares once is in the table for
                    // the text reader (S2): the scope holds it for the value
                    // readers (`values_in` returns `held` first, and a local
                    // not held has an initialiser that is all `Unread`), so
                    // they do not read this entry (`text_only`). A local in a
                    // block of this body is the call walker's
                    // ([`CallWalker::visit_var_decl`]), and one in a function
                    // written inside it is that function's own. Read only
                    // where the body declares the name once: the census keys
                    // writes by binding, the table by line and name; never
                    // `binding_count`, which counts assignment targets too.
                    let start = ir.local_bindings.len();
                    ir.local_bindings.extend(self.local_entries(
                        var,
                        declarator,
                        scope,
                        |name| reassigned.declared_once(&declared, name),
                        |name| reassigned.names.contains(name),
                        false,
                    ));
                    // A `let` written again: which writes reach each read
                    // (carrick#3679 S8), computed here, where the body is in
                    // hand. One computation answers both readers: its span
                    // projection goes to the binding table for the text
                    // reader, and the send side reads its ways at each send.
                    // A `var` is hoisted and stays the census's set.
                    if var.kind == VarDeclKind::Let
                        && let Pat::Ident(ident) = &declarator.name
                        && reassigned.names.contains(ident.id.sym.as_ref())
                        && let Some(entry) = ir.local_bindings[start..]
                            .iter_mut()
                            .find(|entry| entry.key == ident_key(&ident.id))
                    {
                        let found = resolve::flow::reaching(stmts, &entry.key);
                        entry.flow = found.as_ref().map(resolve::flow::Reaching::table);
                        if let Some(found) = &found {
                            scope
                                .written_locals
                                .insert(entry.key.clone(), alternatives::WrittenLocal::of(found));
                        }
                    }
                    // A `let` set again on every path to one of a few makers'
                    // instances, read where it is returned (carrick#1689).
                    // One never set again is a local receiver, below. Its
                    // reassignments are kept by name, so it is read only
                    // where the body declares the name once; its uses are
                    // its own binding's (carrick#3547 S2). A `var` declared twice
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
                        let key = ident_key(&ident.id);
                        let makers = makers
                            .into_iter()
                            .map(|client| scope.module.with_uses(client, name, Some(&key)))
                            .collect();
                        scope.let_makers.insert(key, makers);
                    }
                    // `const { url } = options`: each name holds one key of
                    // the parameter it is unpacked from (carrick#1950), where
                    // the body declares the name once and never assigns it
                    // again.
                    // `const { path, ...rest } = config` (or `= init ?? {}`):
                    // the rest holds the caller's other keys (carrick#2519),
                    // unless the body writes a key onto it (carrick#2674).
                    if let (Pat::Object(object), Some(init)) = (&declarator.name, &declarator.init)
                        && let Some(source) = param_or_empty(init)
                        && scope.key_objects.contains(&ident_key(source))
                        && let Some(index) = scope.param_index(source)
                        && let Some((rest, taken)) = rest_and_taken(object)
                        && !reassigned.names.contains(&rest.0)
                        && !written_through.contains(&rest.0)
                        && declared.binding_count(&rest) == 1
                    {
                        scope.rest_of.insert(rest, (0, index, taken));
                    }
                    // `const { version = 'v1' } = options || {}`: a key with
                    // a default holds the caller's key or, where the caller's
                    // object leaves it out, the default, whose text the
                    // function keeps for the caller walk (carrick#3608).
                    if let (Pat::Object(object), Some(init)) = (&declarator.name, &declarator.init)
                        && let Some(source) = param_or_empty(init)
                        && scope.key_objects.contains(&ident_key(source))
                        && let Some(index) = scope.param_index(source)
                    {
                        let settled = |binding: &BindingKey| {
                            !reassigned.names.contains(&binding.0)
                                && declared.binding_count(binding) == 1
                        };
                        for (binding, key) in keyed_names(object) {
                            if settled(&binding) {
                                scope.keyed.insert(binding, (index, key));
                            }
                        }
                        for (binding, key, default) in defaulted_names(object) {
                            if settled(&binding) {
                                ir.param_default_texts.push((
                                    index,
                                    vec![key.clone()],
                                    resolve::text_of(default, &|lo| lo),
                                ));
                                scope
                                    .key_defaults
                                    .insert((index, key.clone()), default.clone());
                                scope.defaulted.insert(binding, (index, key));
                            }
                        }
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
                            Value::hole(name.clone(), HoleReason::Reassigned)
                        } else {
                            bound_local(
                                name.clone(),
                                self.recording(|| self.eval(init, scope)),
                                self.construction_text(init).as_deref(),
                            )
                        };
                        if !unsettled && let Some(pieces) = library_sites::text_pieces(init, scope)
                        {
                            scope.texts.insert(key.clone(), pieces);
                        }
                        // What a conditional in it leaves it able to read as
                        // (carrick#2050): a request built from it states
                        // one row per branch. One written again is read
                        // through [`Scope::written_locals`] (carrick#3679).
                        let alts = if unsettled {
                            None
                        } else {
                            self.alternatives(init, scope)
                        };
                        scope.locals.insert(key.clone(), value);
                        match alts {
                            Some(alts) => {
                                scope.alts.insert(key.clone(), alts);
                            }
                            None => {
                                scope.alts.remove(&key);
                            }
                        }
                        scope.local_receivers.remove(&key);
                        if let Some(client) = client
                            && declared.name_count(&name) == 1
                        {
                            let client = scope.module.with_uses(client, &name, Some(&key));
                            scope.local_receivers.insert(key, client);
                        }
                    }
                }
                continue;
            }
            self.walk(stmt, scope, ir);
        }
        // The tests of parameters every path to each call takes, read once
        // the walk has recorded every comparison (carrick#4093): a request
        // after `if (verb === "GET") return;` is sent by no caller that
        // passes `"GET"`.
        let points: Vec<u32> = ir.calls.iter().map(|call| call.site.lo).collect();
        let tests = resolve::flow::tests_at(stmts, &points);
        for call in &mut ir.calls {
            if let Some(tests) = tests.get(&call.site.lo) {
                call.path_guards = self.path_guards(tests, scope);
            }
        }
    }

    /// The binding table entries one declarator states, read in `scope`:
    /// a plain name, and each plain key of an object pattern
    /// (`const { a, b: name } = init`, carrick#3504 S2b-3). Only a name the
    /// body declares once has one (`once`); `written` says the body assigns
    /// it again. `block` marks a declarator written in a block of the body,
    /// which [`Self::body`] never reads (carrick#3794).
    fn local_entries(
        &self,
        var: &VarDecl,
        declarator: &VarDeclarator,
        scope: &Scope<'_>,
        once: impl Fn(&str) -> bool,
        written: impl Fn(&str) -> bool,
        block: bool,
    ) -> Vec<resolve::LocalBinding> {
        let mut out = Vec::new();
        if let Pat::Ident(ident) = &declarator.name
            && once(ident.id.sym.as_ref())
        {
            out.push(resolve::LocalBinding {
                key: ident_key(&ident.id),
                line: self.source_map.lookup_char_pos(ident.id.span.lo).line as u32,
                init: declarator
                    .init
                    .as_deref()
                    .map(|init| self.value_of(init, scope, &guards::Bound::default()))
                    .unwrap_or_default(),
                kind: var.kind.into(),
                text: declarator
                    .init
                    .as_deref()
                    .map(|init| resolve::text_of(init, &|lo| lo)),
                reassigned: written(ident.id.sym.as_ref()),
                plain: declarator.init.is_some(),
                block,
                defaulted: false,
                flow: None,
                elements: declarator
                    .init
                    .as_deref()
                    .and_then(|init| resolve::list_of(init, &|lo| lo)),
                list_frozen: declarator
                    .init
                    .as_deref()
                    .is_some_and(resolve::written_as_const),
            });
        }
        // `const { a, b: name } = init`: each plain key is a local of its own
        // for the text reader, under the same once-declared rule.
        if let (Pat::Object(pattern), Some(init)) = (&declarator.name, declarator.init.as_deref()) {
            for member in resolve::pattern_members(pattern) {
                let name = member.bound.id.sym.as_ref();
                if !once(name) {
                    continue;
                }
                out.push(resolve::LocalBinding {
                    key: ident_key(&member.bound.id),
                    line: self
                        .source_map
                        .lookup_char_pos(member.bound.id.span.lo)
                        .line as u32,
                    init: self.value_of(&member.read(init), scope, &guards::Bound::default()),
                    kind: var.kind.into(),
                    text: Some(member.text(init, &|lo| lo)),
                    reassigned: written(name),
                    plain: false,
                    block,
                    defaulted: false,
                    flow: None,
                    elements: None,
                    list_frozen: false,
                });
            }
        }
        out
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
            waited: HashSet::new(),
            guard: guards::WalkState::default(),
            in_head: false,
            depth: 0,
        };
        node.visit_with(&mut walker);
    }

    /// [`Self::walk`] over what a function returns: an arrow's expression
    /// body, read as a `return` of it (carrick#2386).
    fn walk_returned(&self, expr: &Expr, scope: &Scope<'_>, ir: &mut FnIr) {
        let mut walker = CallWalker {
            reader: self,
            scope,
            ir,
            waited: HashSet::new(),
            guard: guards::WalkState::returned(),
            in_head: false,
            depth: 0,
        };
        expr.visit_with(&mut walker);
    }

    /// The value of an expression, as far as the source states it. While
    /// [`Self::recording`], the pieces of a text value are kept by the
    /// expression's span ([`FileIr::arg_pieces`]), once the whole value is
    /// read (an environment read's fallback is set at the `??`).
    fn eval(&self, expr: &Expr, scope: &Scope<'_>) -> Value {
        let value = self.eval_value(expr, scope);
        if self.recording_depth.get() > 0
            && let Value::Str(pieces) = &value
        {
            let span = expr.span();
            self.arg_pieces
                .borrow_mut()
                .insert((span.lo.0, span.hi.0), pieces.clone());
        }
        value
    }

    /// `read` with [`Self::eval`] keeping the pieces it reads
    /// (carrick#3504 B10-2a1): a call's arguments and a local's initialiser.
    fn recording<T>(&self, read: impl FnOnce() -> T) -> T {
        let depth = &self.recording_depth;
        depth.set(depth.get() + 1);
        let out = read();
        depth.set(depth.get() - 1);
        out
    }

    /// `read` with [`Self::eval`] keeping nothing: a builder's return read
    /// with a caller's arguments in its parameters is no reading of the
    /// expressions it is written as.
    fn unrecorded<T>(&self, read: impl FnOnce() -> T) -> T {
        let before = self.recording_depth.replace(0);
        let out = read();
        self.recording_depth.set(before);
        out
    }

    fn eval_value(&self, expr: &Expr, scope: &Scope<'_>) -> Value {
        match expr {
            Expr::Lit(Lit::Str(s)) => {
                Value::Str(vec![Piece::Lit(s.value.to_string_lossy().into_owned())])
            }
            Expr::Lit(Lit::Num(n)) => Value::Str(vec![Piece::Lit(n.value.to_string())]),
            Expr::Tpl(tpl) => Value::Str(self.template(tpl, scope)),
            Expr::Bin(bin) if bin.op == BinaryOp::Add => {
                let left = self.eval(&bin.left, scope);
                let right = self.eval(&bin.right, scope);
                match (&left, &right) {
                    (Value::Str(_), Value::Str(_)) => {
                        Value::Str(concat([left.pieces(), right.pieces()]))
                    }
                    _ => Value::hole(self.text(bin.span), HoleReason::UnreadExpression),
                }
            }
            // `process.env.URL ?? "http://localhost:3000"`: the left side is
            // the value the deployment supplies; the fallback is a default,
            // not the target.
            Expr::Bin(bin)
                if matches!(bin.op, BinaryOp::NullishCoalescing | BinaryOp::LogicalOr) =>
            {
                match self.eval(&bin.left, scope) {
                    // A key of a parameter (carrick#1950): a caller that
                    // writes no such key sends the fallback, which nothing
                    // here reads, so the value is not a caller's to state.
                    // It is the opaque value it was before keys were read,
                    // under the name the function reads it by.
                    Value::Str(pieces)
                        if pieces
                            .iter()
                            .any(|piece| matches!(piece, Piece::ParamKey(..))) =>
                    {
                        Value::Str(
                            pieces
                                .into_iter()
                                .map(|piece| match piece {
                                    Piece::ParamKey(_, _, name) => Piece::Opaque {
                                        text: name,
                                        end: Root::Hole(HoleReason::CallerArgMissing),
                                        link: None,
                                    },
                                    other => other,
                                })
                                .collect(),
                        )
                    }
                    // An environment read keeps the literal written beside
                    // it on its end (carrick#2406); the text is the read's.
                    Value::Str(mut pieces) => {
                        if let [
                            Piece::Opaque {
                                end: Root::Env { fallback, .. },
                                ..
                            },
                        ] = pieces.as_mut_slice()
                            && fallback.is_none()
                        {
                            *fallback = crate::env_alias::fallback_literal(expr);
                        }
                        Value::Str(pieces)
                    }
                    _ => Value::hole(self.text(bin.span), HoleReason::UnreadExpression),
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
                // One key of an object parameter (carrick#1950), or its
                // default (carrick#3608).
                if let Some((index, param_key)) =
                    scope.keyed.get(&key).or_else(|| scope.defaulted.get(&key))
                {
                    return Value::Str(vec![Piece::ParamKey(
                        *index,
                        param_key.clone(),
                        name.to_string(),
                    )]);
                }
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
                            under_param: None,
                            under_rest: None,
                            unsettled: BTreeSet::new(),
                            functions: BTreeSet::new(),
                            wide: true,
                        });
                    }
                    return value.clone();
                }
                self.linked(
                    Value::opaque(name.to_string(), self.ident_end(ident, scope)),
                    expr,
                    scope,
                )
            }
            Expr::Member(member) => self.member(expr, member, scope),
            Expr::Object(obj) => Value::Obj(self.object(obj, scope)),
            Expr::Call(call) => {
                // `JSON.stringify(body)` is the body it serialises.
                if let Some(operand) = library_sites::serialised_operand(call) {
                    return self.eval(operand, scope);
                }
                // `usersPath(id)`, `ENDPOINTS.users.byId(id)`: what the
                // builder returns, with the call's arguments in its
                // parameters (carrick#1562).
                let built = self.builder_call(call, scope);
                // `this.query(ref)`, `pageQuery(cursor)`: a query one of the
                // class's or the module's own functions builds
                // (carrick#1950). A builder's return that is all text the
                // source writes is kept as that text, as it always was; any
                // other is read here, since the builder rule has no way to
                // say "nothing, or a query".
                let all_written = matches!(&built, Some(Value::Str(pieces))
                    if pieces.iter().all(|piece| matches!(piece, Piece::Lit(_))));
                if !all_written && self.is_query_call(call, scope) {
                    return Value::Str(vec![Piece::Query]);
                }
                if let Some(value) = built {
                    return self.through(value, call, scope);
                }
                // `url.toString()` is the URL.
                if let Some(object) = to_string_object(call) {
                    let inner = self.eval(object, scope);
                    if matches!(inner, Value::Str(_)) {
                        return inner;
                    }
                }
                let text = self.text(call.span);
                match crate::env_alias::env_get_call_name(call) {
                    Some(name) => Value::opaque(
                        text.clone(),
                        Root::Env {
                            written: text,
                            name,
                            fallback: None,
                        },
                    ),
                    None => {
                        let hole = Value::hole(text, HoleReason::UnreadExpression);
                        let hole = self.linked(hole, expr, scope);
                        self.through(hole, call, scope)
                    }
                }
            }
            Expr::New(new) => self.new_url(new, scope),
            Expr::Paren(paren) => self.eval(&paren.expr, scope),
            Expr::TsAs(e) => self.eval(&e.expr, scope),
            Expr::TsNonNull(e) => self.eval(&e.expr, scope),
            Expr::TsConstAssertion(e) => self.eval(&e.expr, scope),
            Expr::TsSatisfies(e) => self.eval(&e.expr, scope),
            Expr::TsTypeAssertion(e) => self.eval(&e.expr, scope),
            Expr::Await(e) => self.eval(&e.arg, scope),
            // The branches read as a builder's returns do (carrick#2541).
            Expr::Cond(cond) => {
                let (cons, alt) = (self.eval(&cond.cons, scope), self.eval(&cond.alt, scope));
                let text = self.text(cond.span);
                match (&cons, &alt) {
                    (Value::Str(_), Value::Str(_)) => several_values(vec![cons, alt], text),
                    _ => None,
                }
                .unwrap_or_else(|| {
                    Value::hole(self.text(expr.span()), HoleReason::UnreadExpression)
                })
            }
            _ => Value::hole(self.text(expr.span()), HoleReason::UnreadExpression),
        }
    }

    fn template(&self, tpl: &Tpl, scope: &Scope<'_>) -> Vec<Piece> {
        let mut parts: Vec<Vec<Piece>> = Vec::new();
        for (index, quasi) in tpl.quasis.iter().enumerate() {
            let text = quasi
                .cooked
                .as_ref()
                .map(|cooked| cooked.to_string_lossy().into_owned())
                .unwrap_or_else(|| quasi.raw.to_string());
            parts.push(vec![Piece::Lit(text)]);
            if let Some(expr) = tpl.exprs.get(index) {
                parts.push(self.eval(expr, scope).pieces());
            }
        }
        concat(parts)
    }

    fn member(&self, expr: &Expr, member: &MemberExpr, scope: &Scope<'_>) -> Value {
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
                return Value::hole(format!("this.{name}"), HoleReason::UnreadExpression);
            }
        }
        // `url.href` on a URL value is the URL.
        if member_prop(member).as_deref() == Some("href") {
            let inner = self.eval(&member.obj, scope);
            if matches!(inner, Value::Str(_)) {
                return inner;
            }
        }
        // `options.url` on the function's own parameter: the key of the
        // object a caller writes there (carrick#1950).
        if let Expr::Ident(object) = crate::graphql_document_sites::unwrap_expression(&member.obj)
            && scope.key_objects.contains(&ident_key(object))
            && let Some(index) = scope.param_index(object)
            && let Some(key) = member_prop(member)
        {
            return Value::Str(vec![Piece::ParamKey(index, key, self.text(member.span))]);
        }
        // `props.endpoint` in a closure, on an enclosing function's parameter:
        // the key of the object that function's caller writes (carrick#1949).
        if let Expr::Ident(object) = crate::graphql_document_sites::unwrap_expression(&member.obj)
            && let Some((depth, index)) = scope.outer_objects.get(&ident_key(object))
            && let Some(key) = member_prop(member)
        {
            return Value::Str(vec![Piece::Outer(
                *depth,
                Box::new(Piece::ParamKey(*index, key, self.text(member.span))),
            )]);
        }
        // A key of an object the source writes as a literal.
        if let Some(key) = member_prop(member)
            && let Value::Obj(obj) = self.eval(&member.obj, scope)
            && let Some(value) = obj.fields.get(&key)
        {
            return value.clone();
        }
        // Where it comes from is the receive side's reading, environment
        // reads included (carrick#2406).
        self.linked(
            Value::opaque(self.text(member.span), self.member_end(member, scope)),
            expr,
            scope,
        )
    }

    /// `value`, the opaque piece `expr` reads as, with the module-scope
    /// binding it reads through (carrick#2447): an imported constant, an
    /// entry of an imported constant object, or a call of an imported
    /// builder whose arguments are text. The names are the ones the
    /// message-role readers follow ([`library_sites::text_pieces`]), so one
    /// reader decides what a name is, and [`RequestSummaryInputs::linked`]
    /// reads its value once every module is known.
    fn linked(&self, mut value: Value, expr: &Expr, scope: &Scope<'_>) -> Value {
        if let Some(pieces) = library_sites::text_pieces(expr, scope)
            && let [library_sites::TextPiece::Named(named)] = pieces.as_slice()
            && let Value::Str(inner) = &mut value
            && let [Piece::Opaque { link, .. }] = inner.as_mut_slice()
        {
            *link = Some(Box::new(Link::Named((**named).clone())));
        }
        value
    }

    /// `value`, the hole a call reads as (a body the builder rule cannot
    /// read, or returns that state no one path), keeping the parameters its
    /// arguments read (carrick#4022): a caller still chooses the path, so the
    /// request stays open until one fills them ([`Link::Through`]). A
    /// parameter that is a whole path segment of its argument is a path
    /// parameter, chosen by no caller, and is not kept. Only a call of a
    /// function by its name: a method's arguments act on its receiver
    /// (`'/items/:id'.replace(':id', id)` is the receiver's route). A hole
    /// that already links elsewhere, and any other value, is `value` as it
    /// is.
    fn through(&self, mut value: Value, call: &CallExpr, scope: &Scope<'_>) -> Value {
        let by_name = matches!(&call.callee, Callee::Expr(callee)
            if matches!(crate::graphql_document_sites::unwrap_expression(callee), Expr::Ident(_)));
        let Value::Str(pieces) = &mut value else {
            return value;
        };
        if !by_name {
            return value;
        }
        let [
            Piece::Opaque {
                end: Root::Hole(_),
                link: link @ None,
                ..
            },
        ] = pieces.as_mut_slice()
        else {
            return value;
        };
        if call.args.iter().any(|arg| arg.spread.is_some()) {
            return value;
        }
        let kept: Vec<Piece> = self.unrecorded(|| {
            call.args
                .iter()
                .flat_map(|arg| {
                    let pieces = self.eval(&arg.expr, scope).pieces();
                    pieces
                        .iter()
                        .enumerate()
                        .filter(|(position, piece)| {
                            piece.waits_on_caller() && !is_whole_segment(&pieces, *position)
                        })
                        .map(|(_, piece)| piece.clone())
                        .collect::<Vec<_>>()
                })
                .collect()
        });
        if !kept.is_empty() {
            *link = Some(Box::new(Link::Through(kept)));
        }
        value
    }

    /// Whether `call` is a call of a query builder the enclosing class or
    /// the module declares ([`function_builds_query`], carrick#1950): `this.query(…)`
    /// on a method of the class, or `pageQuery(…)` on a function the module
    /// binds. Whatever it is handed, it yields nothing or a query.
    ///
    /// Only where it is declared: a builder another module exports is read
    /// by nobody here, and one reached any other way (a parameter, a member
    /// of some object) is not a binding whose body this can name.
    fn is_query_call(&self, call: &CallExpr, scope: &Scope<'_>) -> bool {
        let Callee::Expr(callee) = &call.callee else {
            return false;
        };
        match crate::graphql_document_sites::unwrap_expression(callee) {
            Expr::Ident(ident) => scope.module.query_builders.contains(&ident_key(ident)),
            Expr::Member(member) if matches!(&*member.obj, Expr::This(_)) => this_field(member)
                .is_some_and(|name| {
                    scope
                        .fields
                        .is_some_and(|fields| fields.query_builders.contains(&name))
                }),
            _ => false,
        }
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
                Some(arg) => {
                    // `withDefaults(init)`: the caller's own options, whole.
                    // A spread of the parameter in the return keeps the keys
                    // the caller's caller writes there (carrick#1561 C11).
                    if let Some(passed) = self.own_param(&arg.expr, scope) {
                        inner.passed_params.insert(param.clone(), passed);
                    }
                    self.eval(&arg.expr, scope)
                }
                None => Value::Str(vec![Piece::Unknown]),
            };
            inner.locals.insert(param.clone(), value);
        }
        self.builder_depth.set(depth + 1);
        let values: Vec<Value> = self.unrecorded(|| {
            builder
                .returned
                .iter()
                .map(|returned| self.eval(returned, &inner))
                .collect()
        });
        self.builder_depth.set(depth);
        passed_options(&values).or_else(|| several_values(values, self.text(call.span)))
    }

    /// `new URL(input, base)`: the URL [`new_url::resolve_new_url`] reads
    /// (carrick#4124).
    fn new_url(&self, new: &NewExpr, scope: &Scope<'_>) -> Value {
        // FROZEN (old path, 2026-10-21): the by-name `URL` test on the next
        // line only. Deletions and safety fixes only. Fixes for wrong or
        // missing rows go in the compiler's position rows (the `URL`
        // identifier's row, carrick#2413 slice C). Delete at carrick#2413
        // slice F, once the unresolved-import (#2573) and rows-join budget
        // (#2574) fallbacks count zero (carrick#2413).
        let is_url = matches!(&*new.callee, Expr::Ident(ident) if ident.sym == *"URL");
        let args = new.args.as_deref().unwrap_or_default();
        if !is_url || args.iter().any(|arg| arg.spread.is_some()) {
            return Value::hole(self.text(new.span), HoleReason::UnreadExpression);
        }
        match args {
            [input] => self.eval(&input.expr, scope),
            [input, base] => {
                let input = self.eval(&input.expr, scope).pieces();
                // The base is read only where the input does not name the
                // URL on its own, and only for the text it writes: nothing
                // read in it is a site of its own here.
                let url = new_url::resolve_new_url(&input, Some(&[Piece::Unknown])).or_else(|| {
                    let base = self.unrecorded(|| self.eval(&base.expr, scope)).pieces();
                    new_url::resolve_new_url(&input, Some(&base))
                });
                match url {
                    Some(url) => Value::Str(url),
                    None => Value::hole(self.text(new.span), HoleReason::UnreadExpression),
                }
            }
            _ => Value::hole(self.text(new.span), HoleReason::UnreadExpression),
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
    ///
    /// One spread of the function's own parameter, in an object that is
    /// closed up to it, is the caller's argument there (carrick#2411): the
    /// keys before it are held ([`ObjValue::under_param`]) for a call to put
    /// that argument in place. Any later entry but a plain key drops the hold.
    fn object(&self, obj: &ObjectLit, scope: &Scope<'_>) -> ObjValue {
        let mut value = ObjValue::default();
        for prop in &obj.props {
            let prop = match prop {
                PropOrSpread::Spread(spread) => {
                    // A held object is open, so a second spread takes no hold.
                    value.under_param = None;
                    let first = !value.open && value.fields.is_empty();
                    value.under_rest = None;
                    match self.spread_branches(&spread.expr, scope) {
                        Some(branches) => value.spread(&branches),
                        None => {
                            if first {
                                value.under_rest = self.rest_of_param(&spread.expr, scope);
                            }
                            if !value.open {
                                value.under_param = self
                                    .own_param(&spread.expr, scope)
                                    .map(|index| (index, std::mem::take(&mut value.fields)));
                            }
                            value.fields.clear();
                            value.functions.clear();
                            value.open = true;
                            value.wide = true;
                        }
                    }
                    continue;
                }
                PropOrSpread::Prop(prop) => prop,
            };
            match &**prop {
                Prop::KeyValue(kv) => match prop_name(&kv.key) {
                    Some(key) => {
                        value.functions.remove(&key);
                        value.unsettled.remove(&key);
                        value.fields.insert(key, self.eval(&kv.value, scope));
                    }
                    None => {
                        value.fields.clear();
                        value.functions.clear();
                        value.open = true;
                        value.wide = true;
                        value.under_param = None;
                        value.under_rest = None;
                    }
                },
                Prop::Shorthand(ident) => {
                    let expr = Expr::Ident(ident.clone());
                    let key = ident.sym.to_string();
                    value.functions.remove(&key);
                    value.unsettled.remove(&key);
                    value.fields.insert(key, self.eval(&expr, scope));
                }
                // A method, a getter or a setter: its key holds no value this
                // pass reads. With a plain key it sets that key only; any
                // other form may set any key.
                other => {
                    match other {
                        Prop::Method(MethodProp { key, .. }) => match prop_name(key) {
                            Some(name) => {
                                value.fields.remove(&name);
                                value.unsettled.remove(&name);
                                value.functions.insert(name);
                            }
                            None => {
                                value.fields.clear();
                                value.functions.clear();
                                value.wide = true;
                            }
                        },
                        Prop::Getter(GetterProp { key, .. })
                        | Prop::Setter(SetterProp { key, .. }) => match prop_name(key) {
                            Some(name) => {
                                value.fields.remove(&name);
                                value.functions.remove(&name);
                                value.unsettled.insert(name);
                            }
                            None => {
                                value.fields.clear();
                                value.functions.clear();
                                value.wide = true;
                            }
                        },
                        _ => value.wide = true,
                    }
                    value.open = true;
                    value.under_param = None;
                    value.under_rest = None;
                }
            }
        }
        value
    }

    /// The parameter position and the keys taken out, when `expr` names the
    /// rest of a destructured parameter ([`Scope::rest_of`], carrick#2519).
    fn rest_of_param(
        &self,
        expr: &Expr,
        scope: &Scope<'_>,
    ) -> Option<(u8, usize, BTreeSet<String>)> {
        match crate::graphql_document_sites::unwrap_expression(expr) {
            Expr::Ident(ident) => scope.rest_of.get(&ident_key(ident)).cloned(),
            _ => None,
        }
    }

    /// The position of the function's own parameter `expr` names, when its
    /// keys are the ones the caller wrote ([`Scope::key_objects`]); in a
    /// builder, the caller's parameter one of its own holds whole
    /// ([`Scope::passed_params`]).
    fn own_param(&self, expr: &Expr, scope: &Scope<'_>) -> Option<usize> {
        match crate::graphql_document_sites::unwrap_expression(expr) {
            Expr::Ident(ident) if scope.key_objects.contains(&ident_key(ident)) => {
                scope.param_index(ident)
            }
            Expr::Ident(ident) => scope.passed_params.get(&ident_key(ident)).copied(),
            _ => None,
        }
    }

    /// Each argument of a call as a spread of the callee's parameter reads it
    /// (carrick#2411), by the rules [`Self::object`] reads that spread with.
    fn spread_args(&self, args: &[ExprOrSpread], scope: &Scope<'_>) -> Vec<SpreadArg> {
        let mut spread = false;
        args.iter()
            .map(|arg| {
                spread |= arg.spread.is_some();
                if spread {
                    SpreadArg::Unread
                } else if let Some(index) = self.own_param(&arg.expr, scope) {
                    SpreadArg::Param(index)
                } else if let Some(branches) = self.spread_branches(&arg.expr, scope) {
                    SpreadArg::Keys(branches)
                } else {
                    SpreadArg::Unread
                }
            })
            .collect()
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
        let mut init_only = false;
        for (index, arg) in call.args.iter().enumerate() {
            if let Expr::Object(obj) = &*arg.expr
                && is_request_options(obj)
            {
                if bag.is_some() {
                    return None;
                }
                if let Some(Value::Obj(value)) = args.get(index) {
                    bag = Some((index, value));
                    init_only = could_be_response_init(obj);
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
        // The same, with `init` an enclosing function's parameter, read in a
        // closure (carrick#1949).
        let outer_options = match (kind, bag, args.get(1)) {
            (RequestKind::Fetch, None, Some(Value::Str(pieces))) => match pieces.as_slice() {
                [Piece::Outer(depth, inner)] => match &**inner {
                    Piece::Param(index, _) => Some((*depth, *index)),
                    _ => None,
                },
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
            _ if outer_options.is_some() => {
                let (depth, index) = outer_options.unwrap_or_default();
                MethodValue::Outer(depth, index, "method".to_string())
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
                // `fetch(url)` sends a GET. `fetch(url, init)` with an init
                // this pass does not read (a call's value, a binding it does
                // not follow) sends whatever that holds, and a GET read off
                // it is a guess (carrick#1949: once a closure's URL could be
                // filled in, such guesses reached callers).
                None if kind == RequestKind::Fetch && call.args.len() > 1 => MethodValue::Unknown,
                None => MethodValue::Lit("GET".to_string()),
            },
        };

        let declared_methods = match &method {
            MethodValue::Param(index) => scope.param_verbs.get(index).cloned().unwrap_or_default(),
            _ => Vec::new(),
        };

        // A conditional that chooses the URL or the method is more than one
        // request (carrick#2050, carrick#2051). The method is read only
        // where the options say one last: a spread after it may replace it.
        let method_expr = match (kind, options, call.args.get(1)) {
            (RequestKind::Fetch, Some(obj), Some(arg)) if obj.fields.contains_key("method") => {
                alternatives::method_expr(&arg.expr)
            }
            _ => None,
        };
        let alternation = self.alternation(url_expr, method_expr.as_ref(), &url, &method, scope);

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
            // Only the bag says this is a request: not `fetch`, and no verb.
            init_only: kind == RequestKind::Bag && verb.is_none() && init_only,
            declared_methods,
            alternation,
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
        // An HTTP factory's options: exactly one object literal, or a
        // constant that holds one (carrick#2451).
        let options = match args {
            [options] => match crate::graphql_document_sites::unwrap_expression(&options.expr) {
                Expr::Object(literal) => Some(self.object(literal, scope)),
                Expr::Ident(ident) => self.options_constant(ident, scope),
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
            shared: None,
        })
    }

    /// The object literal a maker call's one argument names, when it is a
    /// `const` the file declares once, holds an object literal, and is handed
    /// to this call and nothing else: never written through, spread, aliased,
    /// returned or handed to another call (carrick#2451). Anything else
    /// reads no options, as before.
    fn options_constant(&self, ident: &Ident, scope: &Scope<'_>) -> Option<ObjValue> {
        let name = ident.sym.as_ref();
        if !scope.module.object_consts.contains(name)
            || !scope
                .module
                .uses
                .get(name)
                .is_some_and(|used| used.handed_only_to(ident.span.lo.0))
        {
            return None;
        }
        match self.eval(&Expr::Ident(ident.clone()), scope) {
            Value::Obj(options) => Some(options),
            _ => None,
        }
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
    /// collections, whose `get` and `delete` send nothing. Never in a file
    /// that binds the constructor's name itself (carrick#2281).
    fn is_builtin_collection(&self, receiver: &Expr, scope: &Scope<'_>) -> bool {
        let Value::Str(pieces) = self.eval(receiver, scope) else {
            return false;
        };
        let [Piece::Opaque { text, .. }] = pieces.as_slice() else {
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
        COLLECTIONS.contains(&name) && !scope.module.own_collections.contains(name)
    }
}

/// The language's own keyed collections: a call on one sends nothing
/// ([`Reader::is_builtin_collection`]).
const COLLECTIONS: &[&str] = &[
    "Map",
    "Set",
    "WeakMap",
    "URLSearchParams",
    "Headers",
    "FormData",
];

/// The value a binding holds, as a URL writes it.
///
/// A binding the source BUILDS — a literal path around a base — is read
/// through, which is the point of reading it. One holding only a value the
/// source does not state (`encodeURIComponent(name)`, a constructor argument)
/// says nothing its own name does not, and is written by that name, the way
/// the request itself writes it. An environment read and a construction keep
/// their text: the first names where the value comes from, the second what the
/// binding is.
///
/// `construction` is the text of the `new` expression the binding's
/// initialiser is, when it is one ([`Reader::construction_text`]): the value
/// keeps its text only where reading stopped at that construction. A value
/// read off one (`new URL(next, origin).searchParams.get("k")`) is not a
/// construction and is written by the binding's name.
fn bound(name: String, value: Value, construction: Option<&str>) -> Value {
    let Value::Str(pieces) = &value else {
        return value;
    };
    if pieces.iter().any(|piece| matches!(piece, Piece::Lit(_))) {
        return value;
    }
    match pieces.as_slice() {
        [Piece::Param(..) | Piece::Query] => value,
        // One key of a parameter (carrick#1950): still a caller's to fill
        // in, and where no caller does, written by this binding's name, as
        // the request writes it.
        [Piece::ParamKey(index, key, _)] => {
            Value::Str(vec![Piece::ParamKey(*index, key.clone(), name)])
        }
        [Piece::Opaque { text, .. }]
            if construction == Some(text.as_str()) || renders_as_written(text) =>
        {
            value
        }
        // Written by the binding's name, ending where the value it holds
        // does (carrick#2406).
        _ => {
            let end = end_of(pieces);
            Value::opaque(name, end)
        }
    }
}

/// [`bound`] for a local variable. A URL written once into a local out of
/// several pieces with no text and a parameter among them (`const target =
/// (opts.base ?? this.base) + path`, `getBase() + path`) keeps its pieces,
/// as the same expression written inline in the call does: the parameter is
/// the caller's to fill in, and collapsing the value into one piece named by
/// the local would drop it and the request with it. A class field is not
/// read this way (its own call is `bound`, where a constructor parameter
/// collapses and links through the field).
fn bound_local(name: String, value: Value, construction: Option<&str>) -> Value {
    if let Value::Str(pieces) = &value
        && pieces.len() > 1
        && !pieces.iter().any(|piece| matches!(piece, Piece::Lit(_)))
        && pieces
            .iter()
            .any(|piece| matches!(piece, Piece::Param(..) | Piece::ParamKey(..)))
    {
        return value;
    }
    bound(name, value, construction)
}

/// The value a construction passes, bound to the names `pat` introduces: a
/// plain parameter holds it, a destructured one the keys of a closed object it
/// states (carrick#2452). A key the object lacks, or a default, binds nothing.
fn bind_given(pat: &Pat, given: &Value, locals: &mut HashMap<BindingKey, Value>) {
    match pat {
        Pat::Ident(ident) => {
            locals.insert(ident_key(&ident.id), given.clone());
        }
        Pat::Assign(assign) => bind_given(&assign.left, given, locals),
        Pat::Object(object) => {
            if let Value::Obj(fields) = given {
                for (binding, key) in keyed_names(object) {
                    if let Some(value) = fields.fields.get(&key) {
                        locals.insert(binding, value.clone());
                    }
                }
            }
        }
        _ => {}
    }
}

/// Whether a value states what it holds, for a constructor parameter bound to
/// it (carrick#2452): text of literals and opaque names (a parameter, a
/// query or an unknown piece names no value), or a closed object of such
/// values.
fn states_a_value(value: &Value) -> bool {
    match value {
        Value::Str(pieces) => pieces
            .iter()
            .all(|piece| matches!(piece, Piece::Lit(_) | Piece::Opaque { .. })),
        Value::Obj(object) => {
            !object.open
                && object.under_param.is_none()
                && object.fields.values().all(states_a_value)
        }
        Value::Callback(_) => false,
    }
}

/// Whether a binding holding only `text` is written by that text rather than
/// by its own name ([`bound`]): anything spelled from a runtime's environment
/// object. A rendering rule, not the decision that a value is an environment
/// read: it keeps `Deno.env.get(variable)` and `Deno.env.toObject()`, which
/// [`crate::env_alias::env_read_name`] refuses.
fn renders_as_written(text: &str) -> bool {
    text.starts_with("process.env.")
        || text.starts_with("import.meta.env.")
        || text.starts_with("Deno.env.")
}

/// The body a value states, where it stands for one.
fn body_of(value: &Value) -> BodyValue {
    match value {
        Value::Obj(body) => BodyValue::Fields(body.clone()),
        Value::Str(pieces) => match pieces.as_slice() {
            [Piece::Param(index, _)] => BodyValue::Param(*index),
            [Piece::ParamKey(index, key, _)] => BodyValue::ParamKey(*index, key.clone()),
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

// FROZEN (old path, 2026-10-21): this function only (the global `fetch` read
// by name). Deletions and safety fixes only. Fixes for wrong or missing rows
// go in the compiler's position rows (the `ident` row's declaration in the
// default library, carrick#2413 slice C). Delete at carrick#2413 slice F, once
// the unresolved-import (#2573) and rows-join budget (#2574) fallbacks count
// zero (carrick#2413).
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
        [Piece::ParamKey(index, key, _)] => MethodValue::ParamKey(*index, key.clone()),
        _ => MethodValue::Unknown,
    }
}

/// The parameter `init` names: the identifier itself, or `param ?? {}` /
/// `param || {}` with an empty object literal on the right.
fn param_or_empty(init: &Expr) -> Option<&Ident> {
    match crate::graphql_document_sites::unwrap_expression(init) {
        Expr::Ident(ident) => Some(ident),
        Expr::Bin(bin)
            if matches!(bin.op, BinaryOp::NullishCoalescing | BinaryOp::LogicalOr)
                && matches!(
                    crate::graphql_document_sites::unwrap_expression(&bin.right),
                    Expr::Object(empty) if empty.props.is_empty()
                ) =>
        {
            match crate::graphql_document_sites::unwrap_expression(&bin.left) {
                Expr::Ident(ident) => Some(ident),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The rest binding of an object pattern whose other properties are all
/// plain keys, and those keys: `{ path, headers: h, ...rest }` gives `rest`
/// and `{path, headers}`. `None` with no rest, a computed key, or a rest that
/// is not a plain name.
fn rest_and_taken(object: &ObjectPat) -> Option<(BindingKey, BTreeSet<String>)> {
    let mut taken = BTreeSet::new();
    let mut rest = None;
    for prop in &object.props {
        match prop {
            ObjectPatProp::Assign(assign) => {
                taken.insert(assign.key.id.sym.to_string());
            }
            ObjectPatProp::KeyValue(kv) => {
                taken.insert(prop_name(&kv.key)?);
            }
            ObjectPatProp::Rest(rest_pat) => match &*rest_pat.arg {
                Pat::Ident(binding) => rest = Some(ident_key(&binding.id)),
                _ => return None,
            },
        }
    }
    Some((rest?, taken))
}

/// The bindings an object pattern introduces that hold exactly one key of the
/// object it unpacks, each with its key (carrick#1950): `{ url }` and
/// `{ url: target }`. Not one with a default (`{ url = "/x" }`), which holds
/// the default where the key is missing, nor a rest element, a computed key
/// or a nested pattern.
fn keyed_names(object: &ObjectPat) -> Vec<(BindingKey, String)> {
    object
        .props
        .iter()
        .filter_map(|prop| match prop {
            ObjectPatProp::Assign(assign) if assign.value.is_none() => {
                Some((ident_key(&assign.key.id), assign.key.id.sym.to_string()))
            }
            ObjectPatProp::KeyValue(kv) => match &*kv.value {
                Pat::Ident(binding) => Some((ident_key(&binding.id), prop_name(&kv.key)?)),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The bindings an object pattern introduces that hold one key of the object
/// it unpacks or, where the object leaves the key out, a default
/// (carrick#3608): `{ version = 'v1' }` and `{ version: v = 'v1' }`, each
/// with its key and the default as written. [`keyed_names`] gives the keys
/// with no default.
fn defaulted_names(object: &ObjectPat) -> Vec<(BindingKey, String, &Expr)> {
    object
        .props
        .iter()
        .filter_map(|prop| match prop {
            ObjectPatProp::Assign(assign) => Some((
                ident_key(&assign.key.id),
                assign.key.id.sym.to_string(),
                assign.value.as_deref()?,
            )),
            ObjectPatProp::KeyValue(kv) => match &*kv.value {
                Pat::Assign(assign) => match &*assign.left {
                    Pat::Ident(binding) => {
                        Some((ident_key(&binding.id), prop_name(&kv.key)?, &*assign.right))
                    }
                    _ => None,
                },
                _ => None,
            },
            ObjectPatProp::Rest(_) => None,
        })
        .collect()
}

/// The bindings `const <pattern> = init` binds to one key of a module object
/// literal, each with the key's value expression (carrick#3489 W2): `init`
/// names a module `const` object literal that nothing writes through, and the
/// literal has no spread and states the key once. A binding with a default,
/// a rest element, a computed key or a nested pattern is left out.
fn destructured_keys<'o>(
    pattern: &ObjectPat,
    init: &Expr,
    module: &'o ModuleScope,
) -> Vec<(BindingKey, &'o Expr)> {
    let Expr::Ident(source) = crate::graphql_document_sites::unwrap_expression(init) else {
        return Vec::new();
    };
    let Some(object) = module.object_lits.get(&ident_key(source)) else {
        return Vec::new();
    };
    if module.written_through(&source.sym) {
        return Vec::new();
    }
    let mut stated: HashMap<String, Vec<Option<&Expr>>> = HashMap::new();
    for prop in &object.props {
        let PropOrSpread::Prop(prop) = prop else {
            return Vec::new();
        };
        match &**prop {
            Prop::KeyValue(kv) => match prop_name(&kv.key) {
                Some(key) => stated.entry(key).or_default().push(Some(&kv.value)),
                None => return Vec::new(),
            },
            // A shorthand key states a name, not an expression to take.
            Prop::Shorthand(ident) => stated.entry(ident.sym.to_string()).or_default().push(None),
            _ => return Vec::new(),
        }
    }
    keyed_names(pattern)
        .into_iter()
        .filter_map(|(binding, key)| match stated.get(&key)?.as_slice() {
            [Some(value)] => Some((binding, *value)),
            _ => None,
        })
        .collect()
}

/// Whether a call of `function` yields nothing or a query, whatever it is
/// handed (carrick#1950): it is neither `async` nor a generator, its body
/// ends in a `return`, and every `return` in it hands back the empty string
/// or text that starts with a literal `?` ([`is_query_text`]).
///
/// Strict on purpose. A function with one other `return`, one whose value
/// arrives through a promise, and one that can run off its end (and so
/// return `undefined`) may put anything after the path, and is no builder.
fn function_builds_query(function: &Function) -> bool {
    !function.is_async
        && !function.is_generator
        && function
            .body
            .as_ref()
            .is_some_and(|body| returns_only_queries(&body.stmts))
}

/// The key a class field's initialiser that is not a function is lowered
/// under, beside the class's `Class.member` / `Class.static.member` keys
/// (carrick#3614 A4). It holds `<` and `>`, so no callee name spells it:
/// `this.field()` enters an arrow-valued field and never this body.
fn initialiser_key(class: &str, member: &str, is_static: bool) -> String {
    let statik = if is_static { "static " } else { "" };
    format!("{class}.<{statik}initialiser {member}>")
}

/// [`function_builds_query`] for an arrow, whose expression body is what it
/// returns.
fn arrow_builds_query(arrow: &ArrowExpr) -> bool {
    !arrow.is_async
        && !arrow.is_generator
        && match &*arrow.body {
            ArrowFunctionBody::Expr(returned) => is_query_text(returned),
            ArrowFunctionBody::FunctionBody(block) => returns_only_queries(&block.stmts),
        }
}

/// Whether a body's last statement is a `return` and every `return` it makes
/// (a nested function's are its own) hands back a query or the empty string.
fn returns_only_queries(stmts: &[Stmt]) -> bool {
    struct Returns {
        only_queries: bool,
    }
    impl Visit for Returns {
        fn visit_return_stmt(&mut self, returned: &ReturnStmt) {
            self.only_queries &= returned.arg.as_deref().is_some_and(is_query_text);
        }
        fn visit_function(&mut self, _: &Function) {}
        fn visit_arrow_expr(&mut self, _: &ArrowExpr) {}
        fn visit_class(&mut self, _: &Class) {}
        fn visit_getter_prop(&mut self, _: &GetterProp) {}
        fn visit_setter_prop(&mut self, _: &SetterProp) {}
    }
    if !matches!(stmts.last(), Some(Stmt::Return(_))) {
        return false;
    }
    let mut returns = Returns { only_queries: true };
    stmts.visit_with(&mut returns);
    returns.only_queries
}

/// Whether `expr` is the empty string, or text that starts with a literal `?`
/// ([`starts_with_query`]), on every branch of a conditional.
fn is_query_text(expr: &Expr) -> bool {
    match crate::graphql_document_sites::unwrap_expression(expr) {
        Expr::Lit(Lit::Str(text)) => text.value.is_empty() || starts_with_query(expr),
        Expr::Tpl(tpl) => {
            (tpl.exprs.is_empty() && tpl.quasis.iter().all(|quasi| quasi.raw.is_empty()))
                || starts_with_query(expr)
        }
        Expr::Cond(cond) => is_query_text(&cond.cons) && is_query_text(&cond.alt),
        _ => starts_with_query(expr),
    }
}

/// Whether `expr` is text whose first character is a `?` the source writes:
/// a string, a template, or a concatenation that one of them leads. The empty
/// string leads nothing, so what follows it would.
fn starts_with_query(expr: &Expr) -> bool {
    match crate::graphql_document_sites::unwrap_expression(expr) {
        Expr::Lit(Lit::Str(text)) => text.value.to_string_lossy().into_owned().starts_with('?'),
        Expr::Tpl(tpl) => tpl
            .quasis
            .first()
            .is_some_and(|quasi| quasi.raw.starts_with('?')),
        Expr::Bin(bin) if bin.op == BinaryOp::Add => starts_with_query(&bin.left),
        _ => false,
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
    /// The calls (span start, span end) whose result the source waits on,
    /// recorded before the call itself is visited ([`CallIr::waited`]).
    waited: HashSet<(u32, u32)>,
    /// The test being read and where functions written in an argument sit,
    /// for the guards (carrick#2386).
    guard: guards::WalkState,
    /// Reading a `for` loop's head: its declarators are bound again by each
    /// iteration and state no initialiser, so they are no block entry
    /// (carrick#3794). A `for…of` or `for…in` head's names are loop
    /// entries instead ([`Self::push_loop_entries`], carrick#2093).
    in_head: bool,
    /// How many blocks deep the walk is, a `switch` case included: a
    /// function declared at depth 0 is the body's own.
    depth: u32,
}

impl CallWalker<'_, '_, '_> {
    /// Record `expr` as waited on, when it is a call.
    fn wait_on(&mut self, expr: &Expr) {
        if let Expr::Call(call) = crate::graphql_document_sites::unwrap_expression(expr) {
            self.waited.insert((call.span.lo.0, call.span.hi.0));
        }
    }
}

impl Visit for CallWalker<'_, '_, '_> {
    // `await send(…)`: the source waits on what `send` returns
    // (carrick#1986). Recorded before the call is visited.
    fn visit_await_expr(&mut self, awaited: &AwaitExpr) {
        self.wait_on(&awaited.arg);
        awaited.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        // `send(…).then(…)`: a promise's own members wait on it, as `await`
        // does (carrick#1986). The receiver is visited below, through the
        // callee.
        if let Callee::Expr(callee) = &call.callee
            && let Expr::Member(member) = crate::graphql_document_sites::unwrap_expression(callee)
            && matches!(
                member_prop(member).as_deref(),
                Some("then" | "catch" | "finally")
            )
        {
            self.wait_on(&member.obj);
        }

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
        // `pathname.startsWith("/users")`: a guard (carrick#2386).
        self.read_starts_with(call);
        let call_site = self.reader.site(call.span);
        let callee_text = match &call.callee {
            Callee::Expr(callee) => self.reader.text(callee.span()),
            _ => String::new(),
        };
        let args = self.read_args(call_site, &callee_text, &call.args);
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
        // A call on one of the language's own collections sends nothing
        // either (carrick#2281): `table.set(name, handler)`.
        let inert = match &call.callee {
            Callee::Expr(callee) => match &**callee {
                Expr::Member(member) => {
                    matches!(&*member.obj, Expr::Ident(obj)
                        if INERT_GLOBALS.contains(&obj.sym.as_ref()))
                        || self.reader.is_builtin_collection(&member.obj, self.scope)
                }
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
        let bound = &self.guard.bound;
        let arg_subjects = self.reader.arg_subjects(&call.args, self.scope, bound);
        let callee_subject = match &call.callee {
            Callee::Expr(callee) => Some(self.reader.subject(callee, self.scope, bound)),
            _ => None,
        };
        let handed_names = self.reader.handed(&call.args, self.scope, bound);
        let names = self.reader.arg_names(&call.args, self.scope);
        let spread_args = self.reader.spread_args(&call.args, self.scope);
        let callee_values = match &call.callee {
            Callee::Expr(callee) => self.reader.callee_values(callee, self.scope, bound),
            _ => Vec::new(),
        };
        let callee_key = match &call.callee {
            Callee::Expr(callee) => self.reader.callee_key(callee, self.scope, bound),
            _ => None,
        };
        let arg_values = self.reader.arg_values(&call.args, self.scope, bound);
        let arg_texts = resolve::args_text(&call.args, &|lo| lo, self.scope.class());
        let language_call = match &call.callee {
            Callee::Expr(callee) => self
                .reader
                .language_call(callee, &call.args, self.scope, bound),
            _ => None,
        };
        self.ir.calls.push(CallIr {
            site: call_site,
            callee,
            args,
            request,
            waited: self.waited.contains(&(call.span.lo.0, call.span.hi.0)),
            inert,
            invokes_param,
            receiver,
            site_args,
            spread_from: library_sites::spread_from(&call.args),
            name_line,
            jsx: false,
            arg_subjects,
            callee_subject,
            callee_text,
            callee_values,
            callee_key,
            arg_values,
            arg_texts,
            rest_values: self.reader.rest_values(&call.args, self.scope, bound),
            arg_spans: arg_spans(&call.args),
            tested: self.guard.tested(call.span),
            handed_names,
            names,
            spread_args,
            receiver_spans: match &call.callee {
                Callee::Expr(callee) => discovery::receiver_spans(callee),
                _ => Vec::new(),
            },
            receiver_object: match &call.callee {
                Callee::Expr(callee) => discovery::receiver_object(callee),
                _ => None,
            },
            instance: self.reader.instance_ir(
                match &call.callee {
                    Callee::Expr(callee) => Some(&**callee),
                    _ => None,
                },
                &call.args,
                self.scope,
            ),
            constructs: matches!(call.callee, Callee::Super(_)),
            language_call,
            bound: Vec::new(),
            path_guards: Vec::new(),
        });
    }

    // `<Panel endpoint={url} />`: compiled, the element is a call that hands
    // the component one props object, and the call graph resolves it as one
    // (carrick#1149). Read as that call, so a prop fills in the component's
    // requests (carrick#1949). An intrinsic element (`<div>`) names no
    // binding, and an element the call graph does not resolve to one of the
    // service's components reads as nothing, as it always did.
    fn visit_jsx_opening_element(&mut self, element: &JSXOpeningElement) {
        let callee = match &element.name {
            JSXElementName::Ident(ident) if !crate::visitor::is_intrinsic_jsx_name(&ident.sym) => {
                Some(ident.sym.to_string())
            }
            JSXElementName::JSXMemberExpr(member) => Some(member.prop.sym.to_string()),
            _ => None,
        };
        if let Some(callee) = callee {
            let mut props = ObjValue::default();
            for attr in &element.attrs {
                let JSXAttrOrSpread::JSXAttr(attr) = attr else {
                    // A spread may put any key in the props.
                    props.open = true;
                    continue;
                };
                let JSXAttrName::Ident(name) = &attr.name else {
                    continue;
                };
                let value = match &attr.value {
                    Some(JSXAttrValue::Str(text)) => {
                        Value::Str(vec![Piece::Lit(text.value.to_string_lossy().into_owned())])
                    }
                    Some(JSXAttrValue::JSXExprContainer(JSXExprContainer {
                        expr: JSXExpr::Expr(expr),
                        ..
                    })) if !matches!(&**expr, Expr::Arrow(_) | Expr::Fn(_)) => {
                        self.reader.eval(expr, self.scope)
                    }
                    // A flag (`<Panel open />`), an element, or a function:
                    // nothing a URL is read from.
                    _ => Value::Str(vec![Piece::Unknown]),
                };
                props.fields.insert(name.sym.to_string(), value);
            }
            self.ir.calls.push(CallIr {
                site: self.reader.site(element.span),
                callee,
                args: vec![Value::Obj(props)],
                request: None,
                waited: false,
                inert: false,
                invokes_param: None,
                receiver: None,
                site_args: None,
                spread_from: None,
                name_line: self.reader.line(element.span),
                jsx: true,
                arg_subjects: Vec::new(),
                callee_subject: None,
                callee_text: String::new(),
                callee_values: Vec::new(),
                callee_key: None,
                arg_values: Vec::new(),
                arg_texts: Vec::new(),
                rest_values: Vec::new(),
                arg_spans: Vec::new(),
                tested: None,
                handed_names: Vec::new(),
                names: Vec::new(),
                spread_args: Vec::new(),
                receiver_spans: Vec::new(),
                receiver_object: None,
                instance: InstanceIr::default(),
                constructs: false,
                language_call: None,
                bound: Vec::new(),
                path_guards: Vec::new(),
            });
        }
        element.visit_children_with(self);
    }

    // A function written in the body and not handed to a call: its calls
    // state rows where they are written, and what it sends is not this
    // body's.
    fn visit_function(&mut self, function: &Function) {
        let mut detached =
            self.reader
                .function(function, None, self.scope.module, &self.captured());
        detached.written_in = self.guard.placed(function.span);
        self.ir.detached.push(detached);
    }

    fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
        let mut detached = self.reader.arrow(
            arrow,
            self.scope.fields,
            self.scope.module,
            &self.captured(),
        );
        detached.written_in = self.guard.placed(arrow.span);
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
        // The chains read a construction as a call (carrick#2386): what its
        // arguments name, and where each function written in them sits.
        if !builtin {
            self.record_construction(new);
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
        if let Some(arg) = ret.arg.as_deref() {
            // `return request`: a function this body declares is that
            // function (carrick#2432 slice A2).
            let declared = match crate::graphql_document_sites::unwrap_expression(arg) {
                Expr::Ident(ident) => self.scope.declared_fns.get(&ident_key(ident)).copied(),
                _ => None,
            };
            let values = match declared {
                Some(start) => vec![ValueAt::Function(start)],
                None => self.reader.value_of(arg, self.scope, &self.guard.bound),
            };
            self.ir.returned_values.extend(values);
        }
        self.ir.returns.push((ret.span.lo.0, returned));
        // A predicate's guard is read where it is returned (carrick#2386).
        self.ir
            .returns_tests
            .push(ret.arg.as_deref().is_some_and(guards::is_test));
        let in_return = std::mem::replace(&mut self.guard.in_return, true);
        ret.visit_children_with(self);
        self.guard.in_return = in_return;
    }

    // The guards (carrick#2386): each records what it reads, then visits the
    // node exactly as the default walk does.
    fn visit_if_stmt(&mut self, stmt: &IfStmt) {
        let test = self
            .guard
            .enter_test(&stmt.test, stmt.cons.span(), self.reader);
        stmt.test.visit_with(self);
        self.guard.leave_test(test);
        stmt.cons.visit_with(self);
        stmt.alt.visit_with(self);
    }

    fn visit_cond_expr(&mut self, cond: &CondExpr) {
        let test = self
            .guard
            .enter_test(&cond.test, cond.cons.span(), self.reader);
        cond.test.visit_with(self);
        self.guard.leave_test(test);
        cond.cons.visit_with(self);
        cond.alt.visit_with(self);
    }

    fn visit_bin_expr(&mut self, bin: &BinExpr) {
        self.read_comparison(bin);
        if bin.op == BinaryOp::LogicalAnd && !self.guard.testing() {
            // `pathname === "/x" && handle()`: the right side is the branch.
            let test = self
                .guard
                .enter_test(&bin.left, bin.right.span(), self.reader);
            bin.left.visit_with(self);
            self.guard.leave_test(test);
            bin.right.visit_with(self);
        } else {
            bin.visit_children_with(self);
        }
    }

    fn visit_unary_expr(&mut self, unary: &UnaryExpr) {
        let negated = self.guard.negated;
        if unary.op == UnaryOp::Bang {
            self.guard.negated = !negated;
        }
        unary.visit_children_with(self);
        self.guard.negated = negated;
    }

    fn visit_switch_stmt(&mut self, switch: &SwitchStmt) {
        self.read_switch(switch);
        switch.visit_children_with(self);
    }

    fn visit_member_expr(&mut self, member: &MemberExpr) {
        self.read_table_key(member);
        member.visit_children_with(self);
    }

    // A declarator a block of the body writes (carrick#3794): a binding
    // table entry, with its initialiser read in the body's scope as a
    // top-level local's is ([`Reader::local_entries`]). A body's own
    // top-level declarators are read by `body`, never visited here, and a
    // module's by `module_bindings`; a `for` head's are no block entry (a
    // `for…of` or `for…in` head's are loop entries, carrick#2093).
    //
    // FALLBACK (carrick#3794): also the declarator the procedures walk reads
    // while the table's entries are refused, which no binding table row
    // held. Delete the fallback once block-scoped and nested-function locals
    // are read through the table.
    fn visit_var_decl(&mut self, var: &VarDecl) {
        if !self.in_head {
            for declarator in &var.decls {
                self.push_block_entries(var, declarator);
            }
        }
        for declarator in &var.decls {
            if let (Pat::Ident(ident), Some(init)) = (&declarator.name, &declarator.init) {
                let line = self
                    .reader
                    .source_map
                    .lookup_char_pos(ident.id.span.lo)
                    .line as u32;
                let init = self
                    .reader
                    .value_of(init, self.scope, &guards::Bound::default());
                self.reader.block_bindings.borrow_mut().insert(
                    (line, ident.id.sym.to_string()),
                    BlockBinding {
                        init,
                        constant: var.kind == VarDeclKind::Const,
                    },
                );
            }
        }
        var.visit_children_with(self);
    }

    // A function declared in a body (carrick#3794): an entry holding the
    // function, which a reader reaches through where its node starts.
    fn visit_fn_decl(&mut self, decl: &FnDecl) {
        let name = decl.ident.sym.as_ref();
        let names = self.scope.body_names.as_ref();
        if names.is_none_or(|names| names.once.contains(name)) {
            let entry = resolve::LocalBinding {
                key: ident_key(&decl.ident),
                line: self
                    .reader
                    .source_map
                    .lookup_char_pos(decl.ident.span.lo)
                    .line as u32,
                init: vec![ValueAt::Function(decl.function.span.lo.0)],
                kind: resolve::BindingKind::Function,
                text: None,
                reassigned: names.is_some_and(|names| names.written.contains(name)),
                plain: false,
                block: self.depth > 0,
                defaulted: false,
                flow: None,
                elements: None,
                list_frozen: false,
            };
            self.push_entry(entry);
        }
        decl.visit_children_with(self);
    }

    fn visit_block_stmt(&mut self, block: &BlockStmt) {
        self.depth += 1;
        block.visit_children_with(self);
        self.depth -= 1;
    }

    fn visit_switch_case(&mut self, case: &SwitchCase) {
        self.depth += 1;
        case.visit_children_with(self);
        self.depth -= 1;
    }

    fn visit_for_stmt(&mut self, stmt: &ForStmt) {
        let in_head = std::mem::replace(&mut self.in_head, true);
        stmt.init.visit_with(self);
        self.in_head = in_head;
        stmt.test.visit_with(self);
        stmt.update.visit_with(self);
        stmt.body.visit_with(self);
    }

    fn visit_for_in_stmt(&mut self, stmt: &ForInStmt) {
        self.push_loop_entries(&stmt.left, None, stmt.span.lo.0);
        let in_head = std::mem::replace(&mut self.in_head, true);
        stmt.left.visit_with(self);
        self.in_head = in_head;
        stmt.right.visit_with(self);
        stmt.body.visit_with(self);
    }

    fn visit_for_of_stmt(&mut self, stmt: &ForOfStmt) {
        // Only a `const` head holds each element unchanged; an `await` loop
        // takes what the list's iterator settles to.
        let list = (!stmt.is_await
            && matches!(&stmt.left, ForHead::VarDecl(var) if var.kind == VarDeclKind::Const))
        .then_some(&*stmt.right);
        self.push_loop_entries(&stmt.left, list, stmt.span.lo.0);
        let in_head = std::mem::replace(&mut self.in_head, true);
        stmt.left.visit_with(self);
        self.in_head = in_head;
        stmt.right.visit_with(self);
        stmt.body.visit_with(self);
    }

    fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
        declarator.visit_children_with(self);
        self.bind_block(declarator);
    }

    // A request the body builds and returns (carrick#3470), read with the
    // scope the literal is written in.
    fn visit_object_lit(&mut self, object: &ObjectLit) {
        if self
            .reader
            .returned_objects
            .contains(&(object.span.lo.0, object.span.hi.0))
            && let Some(built) = descriptors::BuiltRequest::read(self.reader, object, self.scope)
        {
            self.ir.built_requests.push(built);
        }
        object.visit_children_with(self);
    }

    // `this.queue = new Queue("emails")`: a maker write, as the message-role
    // field rule reads it (carrick#1665). Never a summary's.
    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        if let Some(write) = self.reader.field_write(assign, self.scope) {
            self.ir.field_writes.push(write);
        }
        // `this.f = value` in an instance member: a write the class's field
        // reads (carrick#3504 S4c).
        if self.scope.fields.is_some()
            && resolve::is_placed_assign(assign.op)
            && let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = &assign.left
            && matches!(
                crate::graphql_document_sites::unwrap_expression(&member.obj),
                Expr::This(_)
            )
            && let Some(field) = this_field(member)
        {
            self.ir.field_values.push(resolve::FieldWrite {
                field,
                at: assign.span.lo.0,
                values: self
                    .reader
                    .value_of(&assign.right, self.scope, &guards::Bound::default()),
            });
        }
        // `name = value`, `name ??= value`, `name ||= value`: a write the
        // value resolver reads for a module binding (carrick#3504 S1, S2b-4).
        if resolve::is_placed_assign(assign.op)
            && matches!(
                assign.left,
                AssignTarget::Simple(SimpleAssignTarget::Ident(_))
            )
        {
            self.ir.binding_writes.push(resolve::BindingWrite {
                at: assign.span.lo.0,
                values: self
                    .reader
                    .value_of(&assign.right, self.scope, &guards::Bound::default()),
                text: resolve::text_of(&assign.right, &|lo| lo),
            });
        }
        // `({ a } = obj)`: each plain key writes the member `obj.a`, placed
        // at the property (carrick#3504 S2b-5).
        if let AssignTarget::Pat(target) = &assign.left
            && let Some(object) = resolve::plain_object_target(target)
        {
            for member in resolve::pattern_members(object) {
                // A key with a default writes no placed value: the values
                // readers read the member alone (carrick#3504 S2b-5).
                if member.default.is_some() {
                    continue;
                }
                let read = member.read(&assign.right);
                self.ir.binding_writes.push(resolve::BindingWrite {
                    at: member.span.lo.0,
                    values: self
                        .reader
                        .value_of(&read, self.scope, &guards::Bound::default()),
                    text: member.text(&assign.right, &|lo| lo),
                });
            }
        }
        assign.visit_children_with(self);
    }

    fn visit_opt_call(&mut self, call: &OptCall) {
        self.ir.unfollowed = true;
        // The arguments are read as a call's are (carrick#3389): a
        // listener written in place is a function of its own, and a name
        // is a value the site can state.
        let call_site = self.reader.site(call.span);
        let callee_text = self.reader.text(call.callee.span());
        let args = self.read_args(call_site, &callee_text, &call.args);
        call.callee.visit_with(self);
        self.record_optional_call(call, args);
        // The same call as a message role reads it, as a plain call is
        // (carrick#3389): `sock?.on(…)` is the site `sock.on(…)` is.
        if let Some(site) = self.reader.library_site(
            call.span,
            MakerForm::Call,
            &call.callee,
            &call.args,
            self.scope,
        ) {
            self.ir.library.push(site);
        }
    }

    fn visit_tagged_tpl(&mut self, tagged: &TaggedTpl) {
        self.ir.unfollowed = true;
        tagged.visit_children_with(self);
    }
}

impl CallWalker<'_, '_, '_> {
    /// The binding table entries of a declarator written in a block here: a
    /// name the body declares once, as [`Reader::body`] reads its own. At
    /// module scope, which has no body, the write census keys every entry
    /// by binding and none is refused here.
    fn push_block_entries(&mut self, var: &VarDecl, declarator: &VarDeclarator) {
        let names = self.scope.body_names.as_ref();
        let entries = self.reader.local_entries(
            var,
            declarator,
            self.scope,
            |name| names.is_none_or(|names| names.once.contains(name)),
            |name| names.is_some_and(|names| names.written.contains(name)),
            true,
        );
        for entry in entries {
            self.push_entry(entry);
        }
    }

    /// The binding table entries of a `for…of` or `for…in` head written
    /// here (carrick#2093): each name it binds, holding each element of
    /// `list` (a `const` head's list; `None` reads no list) at the position
    /// or key the pattern reads. Keyed by its declaration line, as every
    /// entry is: two loops that reuse a name are two entries.
    fn push_loop_entries(&mut self, head: &ForHead, list: Option<&Expr>, loop_at: u32) {
        let kind = match head {
            ForHead::VarDecl(var) => var.kind,
            _ => return,
        };
        for (bound, text) in resolve::loop_head_names(head, list, loop_at, &|lo| lo) {
            let entry = resolve::LocalBinding {
                key: ident_key(&bound.id),
                line: self
                    .reader
                    .source_map
                    .lookup_char_pos(bound.id.span.lo)
                    .line as u32,
                init: Vec::new(),
                kind: resolve::BindingKind::LoopElement,
                text: Some(text),
                // A `let` or `var` head the body may write again.
                reassigned: kind != VarDeclKind::Const,
                plain: false,
                block: true,
                defaulted: false,
                flow: None,
                elements: None,
                list_frozen: false,
            };
            self.push_entry(entry);
        }
    }

    /// Adds an entry to the walked body, once: a declaration the walk reaches
    /// twice would otherwise read as two on one line.
    fn push_entry(&mut self, entry: resolve::LocalBinding) {
        if !self
            .ir
            .local_bindings
            .iter()
            .any(|held| held.key == entry.key && held.line == entry.line)
        {
            self.ir.local_bindings.push(entry);
        }
    }

    /// What each argument of a call written here holds. A function written
    /// as an argument is read as a function of its own (nested), the rest as
    /// values; both a call (`send(…)`) and an optional call (`sock?.on(…)`,
    /// carrick#3389) read them so.
    fn read_args(
        &mut self,
        call_site: Site,
        callee_text: &str,
        args: &[ExprOrSpread],
    ) -> Vec<Value> {
        let mut values: Vec<Value> = Vec::with_capacity(args.len());
        for (index, arg) in args.iter().enumerate() {
            let written_in = WrittenIn::argument(call_site, callee_text, index);
            let value = match &*arg.expr {
                Expr::Arrow(arrow) => {
                    let mut nested = self.reader.arrow(
                        arrow,
                        self.scope.fields,
                        self.scope.module,
                        &self.captured(),
                    );
                    nested.written_in = Some(written_in);
                    self.ir.nested.push(nested);
                    Value::Callback(self.ir.nested.len() - 1)
                }
                Expr::Fn(fn_expr) => {
                    // A `function` expression binds its own `this`.
                    let mut nested = self.reader.function(
                        &fn_expr.function,
                        None,
                        self.scope.module,
                        &self.captured(),
                    );
                    nested.written_in = Some(written_in);
                    self.ir.nested.push(nested);
                    Value::Callback(self.ir.nested.len() - 1)
                }
                expr => {
                    // A function written inside the argument (`{ plugins:
                    // [{ configureServer() {} }] }`) sits at a key path in it.
                    self.guard.place_functions_in(expr, &written_in);
                    expr.visit_with(self);
                    self.reader.recording(|| self.reader.eval(expr, self.scope))
                }
            };
            values.push(value);
        }
        values
    }

    /// What a callback written here can read: this body's constants, its
    /// parameters and the keys it reads of them, and the library instances
    /// this body holds.
    ///
    /// The callback is not called with this body's parameters, so it reads
    /// each as [`Piece::Outer`]: a hole only a call of this body fills in
    /// (carrick#1949). Where a request reads one in a place that states a
    /// route without it, it is read as the name it is written by, as it
    /// always was ([`Effect::settle_outer`]).
    fn captured(&self) -> Captured {
        let mut values: HashMap<BindingKey, Value> = self
            .scope
            .locals
            .iter()
            .map(|(key, value)| (key.clone(), value.enclosed()))
            .collect();
        for (index, key) in self.scope.params.iter().enumerate() {
            if let Some(key) = key {
                values.insert(
                    key.clone(),
                    Value::Str(vec![Piece::Param(index, key.0.clone()).enclosed()]),
                );
            }
        }
        for (key, (index, param_key)) in self.scope.keyed.iter().chain(&self.scope.defaulted) {
            values.insert(
                key.clone(),
                Value::Str(vec![
                    Piece::ParamKey(*index, param_key.clone(), key.0.clone()).enclosed(),
                ]),
            );
        }
        let mut objects: HashMap<BindingKey, (u8, usize)> = self
            .scope
            .outer_objects
            .iter()
            .map(|(key, (depth, index))| (key.clone(), (depth.saturating_add(1), *index)))
            .collect();
        for key in &self.scope.key_objects {
            if let Some(index) = self
                .scope
                .params
                .iter()
                .position(|p| p.as_ref() == Some(key))
            {
                objects.insert(key.clone(), (1, index));
            }
        }
        let rests = self
            .scope
            .rest_of
            .iter()
            .map(|(key, (depth, index, taken))| {
                (
                    key.clone(),
                    (depth.saturating_add(1), *index, taken.clone()),
                )
            })
            .collect();
        Captured {
            values,
            objects,
            rests,
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
            subjects: guards::captured_subjects(self.scope, &self.guard.bound),
            held: self
                .scope
                .held
                .iter()
                .map(|(key, values)| (key.clone(), values.iter().map(ValueAt::enclosed).collect()))
                .collect(),
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
        PropName::Str(s) => Some(s.value.to_string_lossy().into_owned()),
        _ => None,
    }
}

/// The object of a `x.toString()` call: a member call named `toString` with
/// no arguments. The one rule for reading `url.toString()` as the URL; `eval`
/// and `Reader::alternatives` both read through it.
pub(super) fn to_string_object(call: &CallExpr) -> Option<&Expr> {
    let Callee::Expr(callee) = &call.callee else {
        return None;
    };
    let Expr::Member(member) = &**callee else {
        return None;
    };
    (member_prop(member).as_deref() == Some("toString") && call.args.is_empty())
        .then_some(&*member.obj)
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
    /// Span start of the call, in the file's own numbering
    /// ([`Site::span_start`]): what an endpoint row at the same call is
    /// read against (carrick#2383).
    span_start: u32,
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
    /// The body spreads the function's own parameter at this position, and
    /// these string keys are written before the spread
    /// ([`ObjValue::under_param`], carrick#2411): a caller's argument there
    /// is put in place before `body`'s keys, which are written after it.
    body_under: Option<(usize, BTreeMap<String, Vec<Piece>>)>,
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
    /// What each key of the function's own parameter the URL or the method
    /// reads holds where the caller's object leaves it out (carrick#3608):
    /// the key's default ([`FnIr::key_defaults`]), or, once a caller hands
    /// its own options on, the key it wrote before spreading them.
    key_defaults: BTreeMap<(usize, String), Vec<Piece>>,
    /// The tests of parameters the request is sent this way under, each
    /// decided at the call that fills its parameter with text
    /// (carrick#3679): one decided the way's way is spent, one decided the
    /// other way rules the effect out ([`Self::ruled_out`]).
    guards: Vec<WayGuard>,
    /// The tests of parameters on every path to the call that states the
    /// request (carrick#4093, [`CallIr::path_guards`]), decided like
    /// [`Self::guards`] at each call that fills the parameter. Unlike those,
    /// an open one holds no request open ([`Self::has_params`]): the URL is
    /// one, and the line that sends it still states it for the callers
    /// nothing here decides.
    path_guards: Vec<WayGuard>,
    /// A guard was decided against this way: no call of the function sends
    /// it.
    ruled_out: bool,
}

/// What a caller's argument holds under one key of the callee's parameter
/// ([`Effect::read_key`], carrick#3608).
enum KeyRead<'v> {
    /// The value the caller's object writes under the key.
    Written(&'v Value),
    /// The callee's fallback: the caller omits the argument, or writes a
    /// closed object without the key.
    Fallback(Vec<Piece>),
    /// A closed object without the key, and no fallback.
    Missing,
    /// The caller's own parameter at this position holds it (its options,
    /// passed on whole or spread), with what it holds where that leaves
    /// the key out.
    Passed(usize, Option<Vec<Piece>>),
    /// Anything else.
    Unread,
}

impl Effect {
    /// What `args` (with `omitted` true for a position a call leaves out)
    /// hold under key `key` of the parameter at `index` ([`KeyRead`]).
    fn read_key<'v>(
        &self,
        args: &'v [Value],
        omitted: &dyn Fn(usize) -> bool,
        index: usize,
        key: &str,
    ) -> KeyRead<'v> {
        let fallback = self.key_defaults.get(&(index, key.to_string())).cloned();
        match args.get(index) {
            Some(Value::Obj(object)) => match object.fields.get(key) {
                Some(value) => KeyRead::Written(value),
                None if object.unsettled.contains(key) || object.functions.contains(key) => {
                    KeyRead::Unread
                }
                None => match &object.under_param {
                    Some((position, before)) => match before.get(key) {
                        Some(Value::Str(pieces)) => {
                            KeyRead::Passed(*position, Some(pieces.clone()))
                        }
                        Some(_) => KeyRead::Unread,
                        None if fallback.is_some() => KeyRead::Passed(*position, fallback),
                        None => KeyRead::Unread,
                    },
                    None if !object.open && object.under_rest.is_none() => match fallback {
                        Some(fallback) => KeyRead::Fallback(fallback),
                        None => KeyRead::Missing,
                    },
                    None => KeyRead::Unread,
                },
            },
            Some(Value::Str(pieces)) => match pieces.as_slice() {
                [Piece::Param(outer, _)] => KeyRead::Passed(*outer, fallback),
                _ => KeyRead::Unread,
            },
            None if omitted(index) => match fallback {
                Some(fallback) => KeyRead::Fallback(fallback),
                None => KeyRead::Unread,
            },
            _ => KeyRead::Unread,
        }
    }

    /// The requests a shape sends: one, or one per way a conditional in its
    /// URL or method can go (carrick#2050, carrick#2051), or one that states
    /// no row where the source does not say which goes out
    /// ([`Alternation::Nothing`]).
    ///
    /// `key_defaults` are the sending function's ([`FnIr::key_defaults`]).
    fn from_shape(
        shape: &RequestShape,
        file: &Path,
        call: Site,
        key_defaults: &BTreeMap<(usize, String), Vec<Piece>>,
    ) -> Vec<Self> {
        let (body, body_param) = match &shape.body {
            BodyValue::Fields(obj) => (string_fields(obj), None),
            BodyValue::Param(index) => (BTreeMap::new(), Some((*index, None))),
            BodyValue::ParamKey(index, key) => (BTreeMap::new(), Some((*index, Some(key.clone())))),
            BodyValue::Unstated => (BTreeMap::new(), None),
        };
        let body_under = match &shape.body {
            BodyValue::Fields(obj) => held_strings(obj),
            _ => None,
        };
        let readings = match &shape.alternation {
            Alternation::Single => vec![(shape.method.clone(), shape.url.clone(), Vec::new())],
            Alternation::Rows(rows, _) => rows.clone(),
            // The request goes out; which one is not stated. It is still a
            // request the function sends, so a call of the function is not
            // one that sends nothing.
            Alternation::Nothing => vec![(MethodValue::Unknown, vec![Piece::Unknown], Vec::new())],
        };
        readings
            .into_iter()
            .map(|(method, url, guards)| {
                let mut effect = Effect {
                    site: RequestSite {
                        file: file.to_path_buf(),
                        line: call.line,
                        span_start: call.span_start,
                    },
                    method,
                    url,
                    body: body.clone(),
                    body_param: body_param.clone(),
                    body_under: body_under.clone(),
                    verb: shape.kind == RequestKind::Verb,
                    base: shape.base.clone(),
                    base_scope: shape.base_scope.clone(),
                    semantics: shape.semantics.clone(),
                    key_defaults: key_defaults.clone(),
                    guards,
                    path_guards: Vec::new(),
                    ruled_out: false,
                };
                effect.settle_keys();
                effect.settle_outer();
                effect
            })
            .collect()
    }

    /// This effect once per verb its method parameter is declared with
    /// (carrick#2049): the request line states every method the request can
    /// send. An effect whose method is anything else is itself.
    fn each_declared_method(&self, declared: &[String]) -> Vec<Effect> {
        if declared.is_empty() || !matches!(self.method, MethodValue::Param(_)) {
            return vec![self.clone()];
        }
        declared
            .iter()
            .map(|verb| Effect {
                method: MethodValue::Lit(verb.clone()),
                ..self.clone()
            })
            .collect()
    }

    /// A key of a parameter is a caller's to fill in only where the request
    /// states no route without it (carrick#1950).
    ///
    /// Where the URL states a route with every key read as the name the
    /// function reads it by (a base before a literal path, a whole path
    /// segment), the request is stated as it was before a key could be filled
    /// in: at its own line, with the key an opaque value under that name. So
    /// reading keys only adds rows, at callers of a wrapper that stated none.
    fn settle_keys(&mut self) {
        let keyed = |pieces: &[Piece]| {
            pieces
                .iter()
                .any(|piece| matches!(piece, Piece::ParamKey(..)))
        };
        if !keyed(&self.url) && !keyed(&self.base) {
            return;
        }
        let as_written = |pieces: &[Piece]| -> Vec<Piece> {
            pieces
                .iter()
                .map(|piece| match piece {
                    // No caller filled the key in.
                    Piece::ParamKey(_, _, name) => Piece::Opaque {
                        text: name.clone(),
                        end: Root::Hole(HoleReason::CallerArgMissing),
                        link: None,
                    },
                    other => other.clone(),
                })
                .collect()
        };
        let (url, base) = (as_written(&self.url), as_written(&self.base));
        if render_target(&join_base(&base, &url)).is_some() {
            self.url = url;
            self.base = base;
        }
    }

    /// An enclosing function's parameter is its caller's to fill in only
    /// where the request states no route without it (carrick#1949), the rule
    /// [`Self::settle_keys`] applies to a key.
    ///
    /// Where the URL states a route with each such parameter read as the
    /// name it is written by (a base before a literal path, a whole path
    /// segment), the request is stated at its own line as it always was. So
    /// reading a closure's parameters only adds rows, at the callers of the
    /// function it is written in.
    fn settle_outer(&mut self) {
        let outer = |pieces: &[Piece]| pieces.iter().any(|piece| matches!(piece, Piece::Outer(..)));
        if !outer(&self.url) && !outer(&self.base) {
            return;
        }
        let as_written = |pieces: &[Piece]| -> Vec<Piece> {
            pieces
                .iter()
                .map(|piece| match piece {
                    // No caller of the enclosing function filled it in.
                    Piece::Outer(..) => Piece::Opaque {
                        text: piece.parameter_name().unwrap_or_default().to_string(),
                        end: Root::Hole(HoleReason::CallerArgMissing),
                        link: None,
                    },
                    other => other.clone(),
                })
                .collect()
        };
        let (url, base) = (as_written(&self.url), as_written(&self.base));
        if render_target(&join_base(&base, &url)).is_some() {
            self.url = url;
            self.base = base;
        }
    }

    /// Whether the effect waits on a call of a function enclosing the one it
    /// is written in (carrick#1949).
    fn has_outer(&self) -> bool {
        matches!(self.method, MethodValue::Outer(..))
            || self
                .url
                .iter()
                .chain(&self.base)
                .any(|piece| matches!(piece, Piece::Outer(..)))
    }

    /// This effect as the function one out sends it: its enclosing
    /// parameters one function nearer, so that function's callers fill in
    /// the ones that are now its own (carrick#1949).
    fn lifted(&self) -> Effect {
        let lift = |pieces: &[Piece]| pieces.iter().map(Piece::lifted).collect::<Vec<_>>();
        let method = match &self.method {
            MethodValue::Outer(1, index, key) => MethodValue::ParamKey(*index, key.clone()),
            MethodValue::Outer(depth, index, key) => {
                MethodValue::Outer(depth - 1, *index, key.clone())
            }
            other => other.clone(),
        };
        Effect {
            site: self.site.clone(),
            method,
            url: lift(&self.url),
            body: self
                .body
                .iter()
                .map(|(key, pieces)| (key.clone(), lift(pieces)))
                .collect(),
            body_param: None,
            // A closure's body reads as it always did.
            body_under: None,
            verb: self.verb,
            base: lift(&self.base),
            base_scope: self.base_scope.clone(),
            semantics: self.semantics.clone(),
            // The closure's own defaults are no key of the enclosing one.
            key_defaults: BTreeMap::new(),
            guards: self
                .guards
                .iter()
                .map(|guard| WayGuard {
                    subject: lift(&guard.subject),
                    ..guard.clone()
                })
                .collect(),
            path_guards: self
                .path_guards
                .iter()
                .map(|guard| WayGuard {
                    subject: lift(&guard.subject),
                    ..guard.clone()
                })
                .collect(),
            ruled_out: self.ruled_out,
        }
    }

    /// This effect as stated through a call the tests `guards` lie on the
    /// path to (carrick#4093): each joins [`Self::path_guards`], and one the
    /// effect already holds taken the other way rules it out.
    fn on_path(mut self, guards: &[WayGuard]) -> Effect {
        for guard in guards {
            let held = self.guards.iter().chain(&self.path_guards);
            if held.clone().any(|other| other.contradicts(guard)) {
                self.ruled_out = true;
            } else if !held.clone().any(|other| other == guard) {
                self.path_guards.push(guard.clone());
            }
        }
        self
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
            piece.waits_on_caller()
                && !is_whole_segment(&self.url, position)
                // A hole of an unread call (carrick#4022) in a URL that
                // states a route all the same is no caller's to choose, as
                // the site walk reads it (`decided_sites::decides`).
                && (piece.through().is_none() || render_target(&self.url).is_none())
        }) || self.base.iter().any(Piece::is_parameter)
            // A test a caller decides is the caller's (carrick#3679).
            || self.guards.iter().any(WayGuard::is_open)
    }

    /// This effect as seen from a call passing `args`, which `spread_args`
    /// reads as a spread of the parameter at each position puts them in
    /// place.
    fn instantiate(&self, args: &[Value], spread_args: &[SpreadArg]) -> Effect {
        self.instantiate_omitting(args, spread_args, &|_| false)
    }

    /// [`Self::instantiate`], where `omitted` is true for a position the
    /// call leaves out ([`CallIr::omits`]), which holds its default.
    fn instantiate_omitting(
        &self,
        args: &[Value],
        spread_args: &[SpreadArg],
        omitted: &dyn Fn(usize) -> bool,
    ) -> Effect {
        // What the caller wrote where the callee reads `name`, at `position`
        // of the URL. An opaque value standing for a path segment is a path
        // parameter, and keeps the name the callee gives it. Leading the URL
        // it is the base, and the caller's own expression is what says where
        // the request goes.
        let written = |arg: &[Piece], position: usize, name: &str| match arg {
            // A hole of an unread call keeps the parameters it is open on
            // (carrick#4022).
            [piece @ Piece::Opaque { end, .. }] if position > 0 => vec![Piece::Opaque {
                text: name.to_string(),
                end: end.clone(),
                link: piece
                    .through()
                    .map(|inner| Box::new(Link::Through(inner.to_vec()))),
            }],
            _ => arg.to_vec(),
        };
        // Where the caller's argument for a parameter piece comes from: the
        // end of the one piece it writes there (carrick#2406).
        let arg_end = |piece: &Piece| -> Root {
            let hole = Root::Hole(HoleReason::UnreadExpression);
            match piece {
                Piece::Param(index, _) => match args.get(*index) {
                    Some(Value::Str(arg)) => end_of(arg),
                    _ => hole,
                },
                Piece::ParamKey(index, key, _) => match args.get(*index) {
                    Some(Value::Obj(object)) => match object.fields.get(key) {
                        Some(Value::Str(arg)) => end_of(arg),
                        _ => hole,
                    },
                    Some(Value::Str(arg)) => match arg.as_slice() {
                        [Piece::Param(outer, _)] => Root::Param(*outer, String::new()),
                        _ => hole,
                    },
                    _ => hole,
                },
                _ => hole,
            }
        };
        // What the caller writes for one parameter piece, as plain text: the
        // parameters a hole of an unread call holds (carrick#4022).
        let param_fill = |piece: &Piece| -> Option<Vec<Piece>> {
            match piece {
                Piece::Param(index, _) => {
                    Some(args.get(*index).map_or(vec![Piece::Unknown], Value::pieces))
                }
                Piece::ParamKey(index, key, name) => {
                    Some(match self.read_key(args, omitted, *index, key) {
                        KeyRead::Written(value) => value.pieces(),
                        KeyRead::Fallback(fallback) => fallback,
                        KeyRead::Passed(outer, _) => {
                            vec![Piece::ParamKey(outer, key.clone(), name.clone())]
                        }
                        KeyRead::Missing | KeyRead::Unread => vec![Piece::Unknown],
                    })
                }
                _ => None,
            }
        };
        let fill = |pieces: &[Piece]| -> Vec<Piece> {
            concat(
                pieces
                    .iter()
                    .enumerate()
                    .map(|(position, piece)| match piece {
                        // A path parameter stays one, under the callee's own
                        // name: the caller's argument is its value, not the
                        // route's.
                        Piece::Param(_, name) | Piece::ParamKey(_, _, name)
                            if is_whole_segment(pieces, position) =>
                        {
                            vec![Piece::Opaque {
                                text: name.clone(),
                                end: arg_end(piece),
                                link: None,
                            }]
                        }
                        Piece::Param(index, name) => match args.get(*index) {
                            Some(Value::Str(arg)) => written(arg, position, name),
                            Some(other) => other.pieces(),
                            None => vec![Piece::Unknown],
                        },
                        // A key of the object the caller writes there
                        // (carrick#1950). A key the object does not state (a
                        // spread or a computed key may hold it, or nothing
                        // does) and one holding anything but text say
                        // nothing. A caller that passes its own parameter on
                        // leaves the key to its own caller. A key the object
                        // leaves out that the callee gives a default is that
                        // default (carrick#3608).
                        Piece::ParamKey(index, key, name) => {
                            match self.read_key(args, omitted, *index, key) {
                                KeyRead::Written(Value::Str(arg)) => written(arg, position, name),
                                KeyRead::Fallback(fallback) => fallback,
                                KeyRead::Passed(outer, _) => {
                                    vec![Piece::ParamKey(outer, key.clone(), name.clone())]
                                }
                                _ => vec![Piece::Unknown],
                            }
                        }
                        // A hole of an unread call (carrick#4022): its
                        // parameters take what the caller writes, as text.
                        other => vec![
                            other
                                .refilled(&|inner| substituted(inner, &param_fill))
                                .unwrap_or_else(|| other.clone()),
                        ],
                    }),
            )
        };
        let method = match &self.method {
            MethodValue::Param(index) => match args.get(*index) {
                Some(Value::Str(pieces)) => method_of(pieces),
                _ => MethodValue::Unknown,
            },
            MethodValue::ParamKey(index, key) => match self.read_key(args, omitted, *index, key) {
                KeyRead::Written(Value::Str(pieces)) => method_of(pieces),
                KeyRead::Written(_) | KeyRead::Unread => MethodValue::Unknown,
                // The callee's own default (carrick#3608).
                KeyRead::Fallback(fallback) => method_of(&fallback),
                // Options stating no method and no body: the GET every
                // fetch-shaped client defaults to.
                KeyRead::Missing => match args.get(*index) {
                    Some(Value::Obj(obj)) if obj.fields.contains_key("body") => {
                        MethodValue::Unknown
                    }
                    _ => MethodValue::Lit("GET".to_string()),
                },
                KeyRead::Passed(outer, _) => MethodValue::ParamKey(outer, key.clone()),
            },
            other => other.clone(),
        };
        // What each key the caller's own parameter now holds falls back to,
        // a key a way's test reads among them.
        let mut key_defaults: BTreeMap<(usize, String), Vec<Piece>> = BTreeMap::new();
        let read_keys = self
            .url
            .iter()
            .chain(&self.base)
            .chain(self.guards.iter().flat_map(|guard| &guard.subject))
            .filter_map(|piece| match piece {
                Piece::ParamKey(index, key, _) => Some((*index, key.as_str())),
                _ => None,
            })
            .chain(match &self.method {
                MethodValue::ParamKey(index, key) => Some((*index, key.as_str())),
                _ => None,
            });
        for (index, key) in read_keys {
            if let KeyRead::Passed(outer, Some(fallback)) = self.read_key(args, omitted, index, key)
            {
                key_defaults.insert((outer, key.to_string()), fallback);
            }
        }
        let fill_keys = |keys: &BTreeMap<String, Vec<Piece>>| -> BTreeMap<String, Vec<Piece>> {
            keys.iter()
                .map(|(key, pieces)| (key.clone(), fill(pieces)))
                .collect()
        };
        // The object the caller writes is the body: one that spreads the
        // caller's own parameter holds its keys for the caller's callers
        // (carrick#2411). They are the caller's already.
        let (body, body_param, body_under) = match &self.body_param {
            Some((index, key)) => match (args.get(*index), key) {
                (Some(Value::Obj(obj)), None) => (string_fields(obj), None, held_strings(obj)),
                (Some(Value::Obj(obj)), Some(key)) => match obj.fields.get(key) {
                    Some(Value::Obj(body)) => (string_fields(body), None, held_strings(body)),
                    Some(Value::Str(pieces)) => match pieces.as_slice() {
                        [Piece::Param(outer, _)] => (BTreeMap::new(), Some((*outer, None)), None),
                        _ => (BTreeMap::new(), None, None),
                    },
                    _ => (BTreeMap::new(), None, None),
                },
                (Some(Value::Str(pieces)), key) => match pieces.as_slice() {
                    [Piece::Param(outer, _)] => {
                        (BTreeMap::new(), Some((*outer, key.clone())), None)
                    }
                    _ => (BTreeMap::new(), None, None),
                },
                _ => (BTreeMap::new(), None, None),
            },
            None => {
                let after = fill_keys(&self.body);
                match &self.body_under {
                    // The caller's argument in place of the spread
                    // (carrick#2411): the keys written before it stand unless
                    // it sets them, and the ones written after it go on top.
                    Some((index, held)) => match spread_args.get(*index) {
                        Some(SpreadArg::Keys(branches)) => {
                            let mut object = ObjValue {
                                fields: fill_keys(held)
                                    .into_iter()
                                    .map(|(key, pieces)| (key, Value::Str(pieces)))
                                    .collect(),
                                ..ObjValue::default()
                            };
                            object.spread(branches);
                            let mut body = string_fields(&object);
                            body.extend(after);
                            (body, None, None)
                        }
                        Some(SpreadArg::Param(outer)) => {
                            (after, None, Some((*outer, fill_keys(held))))
                        }
                        Some(SpreadArg::Unread) | None => (after, None, None),
                    },
                    None => (after, None, None),
                }
            }
        };
        let mut instantiated = Effect {
            site: self.site.clone(),
            method,
            url: fill(&self.url),
            body,
            body_param,
            body_under,
            verb: self.verb,
            base: fill(&self.base),
            base_scope: self.base_scope.clone(),
            semantics: self.semantics.clone(),
            key_defaults,
            guards: Vec::new(),
            path_guards: Vec::new(),
            ruled_out: self.ruled_out,
        };
        // Each test of a parameter against what the caller wrote there
        // (carrick#3679), a test on the path to the request's call too
        // (carrick#4093): text decides it, the caller's own parameter leaves
        // it to the caller's callers, and anything else leaves the way.
        let tests = (self.guards.iter().map(|guard| (guard, false)))
            .chain(self.path_guards.iter().map(|guard| (guard, true)));
        for (guard, on_path) in tests {
            let subject = concat(guard.subject.iter().map(|piece| match piece {
                Piece::Param(index, _) => match args.get(*index) {
                    Some(Value::Str(arg)) => arg.clone(),
                    _ => vec![Piece::Unknown],
                },
                // The key as the URL and the method read it: a key the
                // caller leaves out is the callee's default (carrick#3608).
                Piece::ParamKey(index, key, name) => {
                    match self.read_key(args, omitted, *index, key) {
                        KeyRead::Written(Value::Str(arg)) => arg.clone(),
                        KeyRead::Fallback(fallback) => fallback,
                        KeyRead::Passed(outer, _) => {
                            vec![Piece::ParamKey(outer, key.clone(), name.clone())]
                        }
                        _ => vec![Piece::Unknown],
                    }
                }
                other => vec![other.clone()],
            }));
            let filled = WayGuard {
                subject,
                ..guard.clone()
            };
            match filled.verdict() {
                Verdict::Open if on_path => instantiated.path_guards.push(filled),
                Verdict::Open => instantiated.guards.push(filled),
                Verdict::RuledOut => instantiated.ruled_out = true,
                Verdict::Kept | Verdict::Undecided => {}
            }
        }
        // A key of the caller's own parameter may have arrived in what it
        // wrote (`get(`${options.base}/users`)`), and so may an enclosing
        // function's.
        instantiated.settle_keys();
        instantiated.settle_outer();
        instantiated
    }

    /// This effect as a call at `site` in `file` sends it. Where the call
    /// fills the last hole the callee left open, the call is where the request
    /// is stated, and it is the request line everything further up reaches.
    fn at_call(&self, call: &CallIr, file: &Path) -> Effect {
        let site = call.site;
        let mut instantiated =
            self.instantiate_omitting(&call.args, &call.spread_args, &|index| call.omits(index));
        if self.has_params() && !instantiated.has_params() {
            instantiated.site = RequestSite {
                file: file.to_path_buf(),
                line: site.line,
                span_start: site.span_start,
            };
        }
        instantiated
    }

    /// This effect with every parameter left unknown: a callback's requests,
    /// whose arguments the callee supplies.
    fn detached(&self) -> Effect {
        self.instantiate(&[], &[])
    }
}

/// An object literal's string-valued keys.
fn string_fields(obj: &ObjValue) -> BTreeMap<String, Vec<Piece>> {
    string_values(&obj.fields)
}

/// The string-valued keys of `fields`.
fn string_values(fields: &BTreeMap<String, Value>) -> BTreeMap<String, Vec<Piece>> {
    fields
        .iter()
        .filter_map(|(key, value)| match value {
            Value::Str(pieces) => Some((key.clone(), pieces.clone())),
            _ => None,
        })
        .collect()
}

/// The parameter an object literal spreads and the string-valued keys
/// written before it ([`ObjValue::under_param`], carrick#2411).
fn held_strings(obj: &ObjValue) -> Option<(usize, BTreeMap<String, Vec<Piece>>)> {
    obj.under_param
        .as_ref()
        .map(|(index, held)| (*index, string_values(held)))
}

/// What a call to one function sends.
#[derive(Debug, Clone, Default)]
struct Summary {
    effects: BTreeSet<Effect>,
    /// The calls each effect was composed through, as `(file, line)`: the
    /// request's own call and every call between it and this body (C11 T1).
    /// Kept out of [`Effect`], which is compared and deduplicated: two
    /// chains to one request are one effect, and their lines are joined.
    /// A row stated from the effect passes through these lines
    /// ([`RequestSummaryIndex::placements`]), which is what a decided site
    /// on one of them yields to ([`decided_sites::Decided::yield_to_summaries`]).
    through: BTreeMap<Effect, BTreeSet<(PathBuf, u32)>>,
    /// Every call in the body resolved, every callee is complete, and every
    /// callback went to a callee that says what it does with it.
    complete: bool,
    /// Parameters the function invokes.
    invokes: BTreeSet<usize>,
    /// The function sends one request and what it returns
    /// ([`crate::return_flow::return_shapes`], the reader model rows use
    /// too, carrick#3823) is that request's parsed body unchanged, here or
    /// through a call to a function that does the same (carrick#1601). A
    /// call of it is worth the body, so a row stated at that call can be
    /// typed by the call's value.
    passes_body: bool,
    /// The function sends one request and returns that request's own value
    /// unread, as the request resolves (`return http.post<T>(url, body)`),
    /// here or through a call to a function that does the same
    /// ([`crate::return_flow::return_shapes`], carrick#3961). What a call of
    /// it receives is what the request line's call is worth, so the response
    /// read there is the response its callers hold. The platform's `fetch`
    /// resolves to a `Response`, not the body, so returning it is not this.
    returns_request_value: bool,
    /// The function hands out a function that may send (carrick#2281): its
    /// body writes one it does not call whose own summary is incomplete or
    /// states a request, or a callee it calls (or a callback that callee
    /// invokes) does. A call of it that writes a route may be binding that
    /// route into what it hands out, so that call is never silent
    /// ([`is_silent`]).
    hands_out: bool,
}

impl Summary {
    /// Add `effect`, composed through the calls at `through` ([`Self::through`]).
    fn state(&mut self, effect: Effect, through: BTreeSet<(PathBuf, u32)>) {
        self.through
            .entry(effect.clone())
            .or_default()
            .extend(through);
        self.effects.insert(effect);
    }

    /// The calls `effect` was composed through; empty for one never stated.
    fn through_of(&self, effect: &Effect) -> BTreeSet<(PathBuf, u32)> {
        self.through.get(effect).cloned().unwrap_or_default()
    }

    /// The calls `effect` was composed through, and the call at `line` of
    /// `file` that reaches it.
    fn through_call(&self, effect: &Effect, file: &Path, line: u32) -> BTreeSet<(PathBuf, u32)> {
        let mut through = self.through_of(effect);
        through.insert((file.to_path_buf(), line));
        through
    }
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
    /// The row is restated at a call to a function the service declares,
    /// which makes the request inside it (carrick#1601). The call at this
    /// site is that function's call, not the request's, so its value is
    /// whatever the function returns (a boolean, a token, a mapped object)
    /// and says nothing about the response body.
    pub at_caller: bool,
    /// A row at a caller whose function hands back the request's own value
    /// unread (carrick#3961): the call is worth what the request line's call
    /// is, so the response read at the request line is the one this call
    /// holds. Every other row at a caller holds the function's own value
    /// (an envelope, a field, a `Response`), which is no wire response.
    pub returns_request_value: bool,
    /// What the request body is at a row stated at a call to a function the
    /// service declares (carrick#1782): what that function does with its
    /// parameters says whether the call's arguments are the body. `None` on
    /// a request's own line, and where the site's own payload is the body.
    pub call_body: Option<CallBody>,
    /// The library-semantics claim ids the row was read through
    /// (carrick#1564), sorted. Empty on every other row.
    pub library_semantics: Vec<String>,
    /// For a row whose base was read in another module's scope (carrick#1568):
    /// that module's environment-read defaults, which describe the base as
    /// that module's own rows describe it. The file the row sits in has
    /// defaults of its own that say nothing about this base.
    pub base_fallbacks: Option<BTreeMap<String, String>>,
    /// The call that makes the request: its file and span start. A row at a
    /// caller reaches a request written elsewhere, and this is where
    /// ([`RequestSummaryIndex::withdraw_registrations`], carrick#2383).
    pub request_at: (PathBuf, u32),
    /// Where the target's base comes from (carrick#2406): the end of its
    /// first piece when that is a value the source names, `None` when the
    /// target opens with text the source writes. The row's base is read
    /// from this ([`crate::call_base::stated_base`]), not from its spelling.
    /// Last, so it orders only rows that agree on everything above.
    pub base_end: Option<Root>,
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
    /// The compiler's rows for the service's positions, bound to the bytes
    /// discovery parsed (carrick#2413); `None` when the service's walk left
    /// no rows or binding them ran past the budget.
    pub positions: Option<crate::position_rows::PositionIndex>,
    /// What each import binding reaches, read from [`Self::positions`] by
    /// [`Self::linked`] (carrick#2413 slice B): where the module-scope
    /// names, instances and getters another module declares are read from
    /// when the run walks rows. `None` without rows, and then they are read
    /// by name through [`Self::bindings`].
    pub(crate) links: Option<row_links::RowLinks>,
    /// With rows, the calls that stay on the call graph (carrick#2413 slice
    /// D), set by [`Self::linked`]: every other call reaches what its row
    /// names ([`Self::reach`]). `None` without rows, and then every call
    /// reaches what [`Self::sites`] names.
    pub(crate) row_calls: Option<row_calls::RowCalls>,
    /// Binding the rows ran past its budget in [`Self::linked`], which then
    /// dropped them: the one way a service that walked rows ends with
    /// [`Self::positions`] `None`. Counted per scan
    /// ([`crate::fallback_tally`]).
    pub(crate) rows_ran_out: bool,
    /// With the receiver trace on, the service's files only tests import
    /// ([`crate::test_only_files`], carrick#2775), as walked: the trace
    /// ignores the router mounts made in them. Empty with it off.
    pub(crate) test_only: HashSet<PathBuf>,
    /// With rows, the member calls that took no call edge and say why
    /// (carrick#813), paths as walked; set by discovery after the row edges
    /// are added. Written on the blob by [`crate::unfollowed_calls::attach`].
    pub(crate) unfollowed: Vec<crate::unfollowed_calls::UnfollowedMemberCall>,
}

impl RequestSummaryInputs {
    /// The inputs with every value that reads through a module-scope binding
    /// another module declares replaced by the text it holds (carrick#2447):
    /// an imported constant, an entry of an imported constant object
    /// (`PATHS.users`), a call of an imported builder handed text
    /// (`apiBase()`). The reading is the message roles' ([`LinkedNames`]), so
    /// a constant object some module may change, a builder called with a
    /// value no module states, and an import the scan cannot follow all stay
    /// the opaque value they were: nothing is guessed.
    ///
    /// Reached in the places a request's URL, base and body, a call's
    /// arguments and a client's options are kept. A piece anywhere else
    /// keeps its link unread, which changes nothing.
    ///
    /// A binding whose text the names cannot read hands the importer what its
    /// own module reads it as ([`Held`], carrick#2540).
    ///
    /// With rows, what each import reaches is read from them first
    /// ([`Self::links`]), and the reading follows it. Binding the rows of
    /// `service` may take `budget` (carrick#2564): past it, the service
    /// reads as with no rows, and the run log says so.
    pub fn linked(mut self, service: &str, budget: std::time::Duration) -> Self {
        if self.positions.is_some() {
            let mut join = rows_join::RowsJoin::start(service, budget);
            if let Some(reads) = self.bind_rows(&mut join) {
                // A class's constructions are found by the rows at each
                // `new` (carrick#2452), so a field reads through them only
                // here.
                let mut linked = self.read_through(reads);
                linked.read_fields();
                linked.read_language_calls();
                return linked;
            }
            self.positions = None;
            self.links = None;
            self.row_calls = None;
            self.rows_ran_out = true;
        }
        let reads = self.linked_reads(None).unwrap_or_default();
        self.read_through(reads)
    }

    /// Each call written as `f.call(…)` or `f.apply(…)` whose row names the
    /// language library's `call` or `apply` reads as a call of `f`
    /// (carrick#3504 S4d): the compiler's row decides it, so a user
    /// object's own `call` method (declared in the repo) stays a member
    /// call. Without rows nothing is decided.
    fn read_language_calls(&mut self) {
        let Some(positions) = self.positions.as_ref() else {
            return;
        };
        for (path, ir) in &mut self.files {
            ir.visit_calls(&mut |call| {
                let Some(language) = call.language_call.as_deref_mut() else {
                    return;
                };
                let member = if language.applied.is_some() {
                    "apply"
                } else {
                    "call"
                };
                language.decided = positions
                    .lookup(
                        path,
                        call.site.span_start,
                        call.site.span_end,
                        crate::position_rows::Kind::Call,
                    )
                    .ok()
                    .is_some_and(|row| {
                        row.symbol() == Some(member)
                            && row
                                .decl_file()
                                .is_some_and(|declared| declared.starts_with("lib:"))
                    });
            });
        }
    }

    /// The rows bound to the import links and the calls, and what the
    /// linked names read through them, each step before `join`'s deadline.
    /// `None` past it, with the rest of `self` as it was.
    fn bind_rows(&mut self, join: &mut rows_join::RowsJoin) -> Option<LinkedReads> {
        let deadline = join.deadline();
        let positions = self.positions.as_ref()?;
        let Some(links) = row_links::RowLinks::of(&self.files, &self.bindings, positions, deadline)
        else {
            join.run_out("import links");
            return None;
        };
        self.links = Some(links);
        join.step("import links");
        let Some(row_calls) = self.row_calls(deadline) else {
            join.run_out("calls");
            return None;
        };
        self.row_calls = Some(row_calls);
        join.step("calls");
        let Some(reads) = self.linked_reads(Some(deadline)) else {
            join.run_out("linked names");
            return None;
        };
        join.step("linked names");
        Some(reads)
    }

    /// What each linked piece of the files reads as: its text, or what its
    /// own module holds it as. `None` once `deadline` passes.
    fn linked_reads(&mut self, deadline: Option<std::time::Instant>) -> Option<LinkedReads> {
        let mut wanted: BTreeSet<(PathBuf, library_sites::NamedRef)> = BTreeSet::new();
        for (path, ir) in &mut self.files {
            ir.visit_pieces(path, &mut |scope, pieces| {
                for piece in pieces.iter() {
                    if let Piece::Opaque {
                        link: Some(link), ..
                    } = piece
                        && let Link::Named(named) = &**link
                    {
                        wanted.insert((scope.to_path_buf(), named.clone()));
                    }
                }
            });
        }
        if wanted.is_empty() {
            return Some(LinkedReads::default());
        }
        let names =
            library_sites::LinkedNames::link(&self.files, &self.bindings, self.links.as_ref())
                .until(deadline);
        let past = || deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline);
        let mut resolved = HashMap::new();
        let mut held = HashMap::new();
        let mut declared = HashMap::new();
        for (file, named) in wanted {
            if past() {
                return None;
            }
            if let Some(at) = names.declared_in(&file, &named) {
                declared.insert((file.clone(), named.clone()), at);
            }
            let piece = library_sites::TextPiece::Named(Box::new(named.clone()));
            if let Some(text) = names.text(&file, &[piece]) {
                resolved.insert((file, named), text);
            } else if let Some(found) = names.held(&file, &named) {
                held.insert((file, named), found.clone());
            }
        }
        // A read the deadline cut short is no reading.
        (!past()).then_some((resolved, held, declared))
    }

    /// The files with each linked piece read as `reads` says. First, each
    /// call's argument slot that holds a piece read whole as text records
    /// its pieces with the module that declares each such one
    /// ([`InstanceIr::folds`]), since the read joins them into one text.
    fn read_through(mut self, (resolved, held, declared): LinkedReads) -> Self {
        if resolved.is_empty() && held.is_empty() {
            return self;
        }
        let folded = |scope: &Path, piece: &Piece| -> Option<FoldPiece> {
            let Piece::Opaque {
                link: Some(link), ..
            } = piece
            else {
                return None;
            };
            let Link::Named(named) = &**link else {
                return None;
            };
            let key = (scope.to_path_buf(), named.clone());
            let text = match (resolved.get(&key), held.get(&key)) {
                (Some(text), _) | (None, Some(Held::Text(text))) => text,
                _ => return None,
            };
            Some(FoldPiece::Folded {
                text: text.clone(),
                from: declared.get(&key)?.clone(),
            })
        };
        for (path, ir) in &mut self.files {
            ir.visit_calls(&mut |call| {
                call.instance.folds = slot_folds(&call.args, &|piece| folded(path, piece));
            });
        }
        for (path, ir) in &mut self.files {
            ir.visit_pieces(path, &mut |scope, pieces| {
                let read = |piece: &Piece| match piece {
                    Piece::Opaque {
                        link: Some(link), ..
                    } => {
                        let Link::Named(named) = &**link else {
                            return None;
                        };
                        let key = (scope.to_path_buf(), named.clone());
                        match (resolved.get(&key), held.get(&key)) {
                            (Some(text), _) => Some(vec![Piece::Lit(text.clone())]),
                            (None, Some(Held::Text(text))) => Some(vec![Piece::Lit(text.clone())]),
                            (None, Some(Held::Tail(tail))) => {
                                Some(vec![piece.clone(), Piece::Lit(tail.clone())])
                            }
                            (None, Some(Held::SeveralPaths)) => {
                                let Piece::Opaque { text, .. } = piece else {
                                    return None;
                                };
                                Some(vec![Piece::Opaque {
                                    text: text.clone(),
                                    end: Root::Hole(HoleReason::SeveralPaths),
                                    link: None,
                                }])
                            }
                            (None, None) => None,
                        }
                    }
                    _ => None,
                };
                if pieces.iter().any(|piece| read(piece).is_some()) {
                    let parts: Vec<Vec<Piece>> = pieces
                        .drain(..)
                        .map(|piece| read(&piece).unwrap_or_else(|| vec![piece]))
                        .collect();
                    *pieces = concat(parts);
                }
            });
        }
        self
    }
}

/// Calls `visit` with each list of pieces among `value`, read in `scope`.
fn visit_value_pieces(
    scope: &Path,
    value: &mut Value,
    visit: &mut dyn FnMut(&Path, &mut Vec<Piece>),
) {
    match value {
        Value::Str(pieces) => visit(scope, pieces),
        Value::Obj(object) => visit_object_pieces(scope, object, visit),
        Value::Callback(_) => {}
    }
}

fn visit_object_pieces(
    scope: &Path,
    object: &mut ObjValue,
    visit: &mut dyn FnMut(&Path, &mut Vec<Piece>),
) {
    for value in object.fields.values_mut() {
        visit_value_pieces(scope, value, visit);
    }
    if let Some((_, held)) = &mut object.under_param {
        for value in held.values_mut() {
            visit_value_pieces(scope, value, visit);
        }
    }
}

fn visit_client_pieces(
    scope: &Path,
    client: &mut ClientRef,
    visit: &mut dyn FnMut(&Path, &mut Vec<Piece>),
) {
    if let Some(options) = client
        .instance
        .as_mut()
        .and_then(|instance| instance.options.as_mut())
    {
        visit_object_pieces(scope, options, visit);
    }
}

impl FnIr {
    /// Calls `visit` with each list of pieces this body's calls keep, read in
    /// `scope`, nested functions included.
    fn visit_pieces(&mut self, scope: &Path, visit: &mut dyn FnMut(&Path, &mut Vec<Piece>)) {
        for call in &mut self.calls {
            for arg in &mut call.args {
                visit_value_pieces(scope, arg, visit);
            }
            if let Some(applied) = call
                .language_call
                .as_deref_mut()
                .and_then(|language| language.applied.as_mut())
            {
                for arg in &mut applied.args {
                    visit_value_pieces(scope, arg, visit);
                }
            }
            for spread in &mut call.spread_args {
                if let SpreadArg::Keys(objects) = spread {
                    for object in objects {
                        visit_object_pieces(scope, object, visit);
                    }
                }
            }
            if let Some(receiver) = &mut call.receiver {
                match &mut receiver.client {
                    ClientBinding::Own(client) => visit_client_pieces(scope, client, visit),
                    ClientBinding::Imported {
                        package: Some(client),
                        ..
                    } => visit_client_pieces(scope, client, visit),
                    ClientBinding::Module(_) | ClientBinding::Imported { package: None, .. } => {}
                }
            }
            if let Some(request) = &mut call.request {
                visit(scope, &mut request.url);
                // A client's base was read in the module that declares it.
                let base_scope = request.base_scope.clone();
                visit(base_scope.as_deref().unwrap_or(scope), &mut request.base);
                if let BodyValue::Fields(object) = &mut request.body {
                    visit_object_pieces(scope, object, visit);
                }
                if let Alternation::Rows(rows, _) = &mut request.alternation {
                    for (_, url, _) in rows {
                        visit(scope, url);
                    }
                }
            }
        }
        for nested in self.nested.iter_mut().chain(&mut self.detached) {
            nested.visit_pieces(scope, visit);
        }
    }

    /// Calls `visit` with each call of this body, nested functions included.
    fn visit_calls(&mut self, visit: &mut dyn FnMut(&mut CallIr)) {
        for call in &mut self.calls {
            visit(call);
        }
        for nested in self.nested.iter_mut().chain(&mut self.detached) {
            nested.visit_calls(visit);
        }
    }
}

/// Each argument slot of `args` (an argument, or a key of an object-literal
/// one) whose text is pieces written at the call and at least one `folded`
/// reads as text through a constant ([`InstanceIr::folds`]).
fn slot_folds(args: &[Value], folded: &dyn Fn(&Piece) -> Option<FoldPiece>) -> Vec<SlotFold> {
    let pieces_of = |pieces: &[Piece]| -> Option<Vec<FoldPiece>> {
        let mut out = Vec::new();
        let mut any = false;
        for piece in pieces {
            match (folded(piece), piece) {
                (Some(fold), _) => {
                    any = true;
                    out.push(fold);
                }
                (None, Piece::Lit(text)) => out.push(FoldPiece::Lit(text.clone())),
                (None, _) => return None,
            }
        }
        any.then_some(out)
    };
    let mut out = Vec::new();
    for (arg, value) in args.iter().enumerate() {
        match value {
            Value::Str(pieces) => {
                if let Some(pieces) = pieces_of(pieces) {
                    out.push(SlotFold {
                        arg,
                        key: None,
                        pieces,
                    });
                }
            }
            Value::Obj(object) => {
                for (key, value) in &object.fields {
                    if let Value::Str(pieces) = value
                        && let Some(pieces) = pieces_of(pieces)
                    {
                        out.push(SlotFold {
                            arg,
                            key: Some(key.clone()),
                            pieces,
                        });
                    }
                }
            }
            Value::Callback(_) => {}
        }
    }
    out
}

impl FileIr {
    /// Calls `visit` with each call the file's bodies make.
    fn visit_calls(&mut self, visit: &mut dyn FnMut(&mut CallIr)) {
        for ir in self.functions.values_mut() {
            ir.visit_calls(visit);
        }
        self.module_level.visit_calls(visit);
    }

    /// Calls `visit` with each list of pieces the file keeps, with the file
    /// they were read in.
    fn visit_pieces(&mut self, path: &Path, visit: &mut dyn FnMut(&Path, &mut Vec<Piece>)) {
        for ir in self.functions.values_mut() {
            ir.visit_pieces(path, visit);
        }
        self.module_level.visit_pieces(path, visit);
        for client in self
            .module_clients
            .values_mut()
            .chain(self.field_receivers.values_mut())
        {
            visit_client_pieces(path, client, visit);
        }
        for pieces in self.arg_pieces.values_mut() {
            visit(path, pieces);
        }
    }
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
    /// (importer, local) for each import its rows name a declaration of the
    /// repository for (carrick#2413 slice E): whatever its specifier says,
    /// it is no package's client.
    in_repo: HashSet<(PathBuf, String)>,
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
    /// With rows (`links`, carrick#2413 slice B), an import is the
    /// declaration its rows name, and one is read through when the declaring
    /// module publishes nothing through `exports`; instead of a specifier
    /// that names nothing, a use no row follows that may change what it
    /// holds takes every imported instance away ([`RowLinks::open`]).
    fn link(
        files: &HashMap<PathBuf, FileIr>,
        bindings: &ImportedBindings,
        links: Option<&row_links::RowLinks>,
    ) -> Self {
        let mut linked = LinkedClients::default();
        for (file, ir) in files {
            for (name, client) in &ir.module_clients {
                linked
                    .clients
                    .insert((file.clone(), name.clone()), client.clone());
            }
            // A dropped `const` is no module client for another module
            // (`imports` link through `module_clients` only), so this entry
            // is read only by a library site in its own file
            // ([`Scope::site_binding`], carrick#3559).
            for (name, client) in &ir.dropped_clients {
                linked
                    .clients
                    .insert((file.clone(), name.clone()), client.clone());
            }
        }
        for (file, ir) in files {
            for (local, used) in &ir.imported {
                if let Some(links) = links {
                    let link = links.get(file, local);
                    if link.is_some_and(row_links::RowLink::in_repo) {
                        linked.in_repo.insert((file.clone(), local.clone()));
                    }
                    match link {
                        Some(row_links::RowLink::Declared(at, name)) => {
                            if let Some(declaring) = files.get(at)
                                && declaring.module_clients.contains_key(name)
                            {
                                let held = (at.clone(), name.clone());
                                linked.merge(&held, used);
                                if !declaring.names_commonjs_exports {
                                    linked.imports.insert((file.clone(), local.clone()), held);
                                }
                            }
                        }
                        Some(row_links::RowLink::Module(published)) => {
                            linked.merge_namespace(files, bindings, published, used);
                        }
                        _ => {}
                    }
                    continue;
                }
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
        // With rows, such a module matters only where a use no row follows
        // may change what it holds.
        linked.unfollowable |= match links {
            Some(links) => links.open(),
            None => !bindings.unresolved_in().is_empty(),
        };
        linked
    }

    /// Merge one module's uses of a binding into the client it names.
    fn merge(&mut self, held: &(PathBuf, String), used: &BindingUse) {
        if let Some(client) = self.clients.get_mut(held) {
            client.contested |= used.contests_client();
            client.called.extend(used.called.keys().cloned());
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
                        if used.called.contains_key(&Some(binding.published.clone())) {
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
                    None if self.in_repo.contains(&(file.to_path_buf(), local.clone())) => None,
                    None => package.as_ref().map(|client| (client, file)),
                }
            }
        }
    }
}

// FROZEN (old path, 2026-10-21): this function only, a by-name re-export walk
// (and the `unfollowable` switch it serves). Deletions and safety fixes only.
// Fixes for wrong or missing rows go in the compiler's position rows (the
// `ident` row's declaration, carrick#2413 slice B). Delete at carrick#2413
// slice F, once the unresolved-import (#2573) and rows-join budget (#2574)
// fallbacks count zero (carrick#2413).
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

/// Each file's calls by span, and what each reaches.
type ReachedByFile = HashMap<PathBuf, BTreeMap<(u32, u32), ReachedCallee>>;

/// [`RequestSummaryIndex::ruled_out`]: per file, by a call's span start.
type RuledOutByFile = HashMap<PathBuf, BTreeMap<u32, BTreeSet<(PathBuf, u32)>>>;

/// The function a call reaches by the compiler's rows, and the requests that
/// function sends (carrick#4120). Paths are as walked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachedCallee {
    /// The file that declares the function.
    pub file: PathBuf,
    /// Each request the function sends, directly or through what it calls:
    /// the file it is written in and the span start of its call, in the same
    /// numbering as `RequestMember::request_start`.
    pub requests: BTreeSet<(PathBuf, u32)>,
}

/// Everything the pass states, per file (keyed by the file as walked).
#[derive(Debug, Default)]
pub struct RequestSummaryIndex {
    rows: HashMap<PathBuf, BTreeMap<u32, Vec<SummaryRow>>>,
    silent: HashMap<PathBuf, BTreeSet<u32>>,
    /// The requests a call's text rules out and the call sends no other
    /// way (carrick#3679, carrick#4093), by the call's span start: the file
    /// each is written in and the span start of its call, as
    /// [`ReachedCallee::requests`] names them. Read by the imported-member
    /// join, which drops a member whose request is one of them.
    ruled_out: Box<RuledOutByFile>,
    /// The function each call of the service's files reaches by the
    /// compiler's rows, keyed by the call's span. Read by the imported-member
    /// join, which drops a member the compiler says the call does not reach
    /// (carrick#4120).
    reached: Box<ReachedByFile>,
    /// Requests this pass reached whose URL it could not state.
    pub undetermined: usize,
    /// The callers among them whose request is closed and does not render
    /// ([`CallerStop`]).
    caller_stops: Vec<CallerStop>,
    /// The call sites the call graph follows into a function of the
    /// repository that the compiler's rows cannot say the callee of, by the
    /// rows' reason (carrick#2557). Each is read as no request: neither a
    /// row nor a silent site.
    unfollowed: BTreeMap<String, usize>,
    /// The call sites the compiler's rows cannot follow because the import
    /// they call through did not resolve, read from the call graph as
    /// without rows (carrick#2557).
    fell_back: usize,
    /// What the requests whose method is a parameter declared as a set of
    /// verbs wait on (carrick#2049).
    methods: Box<DeclaredMethods>,
    /// The calls the rows and stops were composed through (C11 T1). Read
    /// by [`Self::placements`].
    chains: Box<Chains>,
    /// What says whether a request its own line leaves open is stated by its
    /// callers alone (carrick#3961), gathered while the rows are.
    ledger: Box<CallerLedger>,
    /// What each function hands its caller ([`ReturnChain`], carrick#4371).
    returned: Box<ReturnChains>,
}

/// What the callers of the requests whose own line leaves the path open
/// tell about them (carrick#3961): whether every call of every function
/// holding such a request open is one this pass read.
#[derive(Debug, Default)]
struct CallerLedger {
    /// The requests a caller closed and no row states there: a caller stop,
    /// or a row that did not render.
    stops: HashSet<(PathBuf, u32)>,
    /// The requests a function no key names holds open (a function written
    /// inside another, which its callers reach through a value).
    open: HashSet<(PathBuf, u32)>,
    /// The functions some call this pass composed reaches.
    seen: HashSet<(PathBuf, String)>,
    /// The functions some call this pass did not compose reaches and that
    /// has no call site of its own to check: a call through a parameter, or
    /// one whose callee only the call graph names.
    unseen: HashSet<(PathBuf, String)>,
    /// The calls this pass states no row for (a call made where a module is
    /// loaded, an optional call, a construction, a call in a constructor or
    /// in a callback), per function they reach, as `(file, line, span
    /// start)` (carrick#4121, carrick#4127). The fact layer may state a row
    /// at each.
    fact_calls: HashMap<(PathBuf, String), FactCalls>,
    /// The requests, by file and span start, whose every caller this pass
    /// read and stated ([`RequestSummaryIndex::answered_at_callers`]).
    closed: HashSet<(PathBuf, u32)>,
    /// The requests, by file and span start, whose every caller this pass
    /// read or is a call it states no row for, with at least one such call
    /// ([`RequestSummaryIndex::held_at_fact_calls`]).
    held: HashMap<(PathBuf, u32), HeldAtFactCalls>,
}

/// A request whose own line leaves the path open, and whose callers this
/// pass read save the calls it states no row for (carrick#4121,
/// carrick#4127): the fact layer may state those calls as rows of their own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldAtFactCalls {
    /// The request's own line.
    pub line: u32,
    /// The calls the fact layer may state, as `(file, line, span start)`.
    pub fact_calls: FactCalls,
}

/// Calls this pass states no row for, as `(file, line, span start)`
/// ([`HeldAtFactCalls::fact_calls`]).
pub type FactCalls = BTreeSet<(PathBuf, u32, u32)>;

/// The calls the summaries' rows and stops were composed through (C11 T1,
/// [`Summary::through`]).
#[derive(Debug, Default)]
struct Chains {
    /// Per row at a caller, the row's own call included: keyed by the
    /// row's request ([`SummaryRow::request_at`]) and the row's file and
    /// line.
    rows: HashMap<RowAt, BTreeSet<(PathBuf, u32)>>,
    /// The chains to callers saved as a stop ([`CallerStop`]): the request
    /// was closed there and does not render.
    stopped: Vec<StoppedChain>,
}

/// A chain to a caller the summaries saved as a stop ([`CallerStop`]).
#[derive(Debug)]
struct StoppedChain {
    /// The request's file and span start ([`SummaryRow::request_at`]).
    request_at: (PathBuf, u32),
    /// The calls the request was composed through to the caller, the
    /// caller's own call included ([`Summary::through`]).
    through: BTreeSet<(PathBuf, u32)>,
}

/// A summary row by its request ([`SummaryRow::request_at`]) and its own
/// file and line.
type RowAt = ((PathBuf, u32), (PathBuf, u32));

/// The verb a call settles for the requests it reaches, where none of their
/// URLs is closed at the call (carrick#2676): `openPoll({ url, request })`
/// into `fetch(options.url, { ...options.request, method: "GET" })`, with
/// the URL left to a caller this pass does not see.
///
/// It is no row of its own: the URL is not the source's to state here. It
/// says only what the request's verb is, so the model's row at the same
/// call takes it and keeps its target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatedVerb {
    pub span_start: u32,
    pub span_end: u32,
    pub line: u32,
    pub method: String,
}

/// The verbs one call settles for the requests whose URL it leaves open.
///
/// One verb only where every request the call reaches is still open, none
/// is a verb call (whose role nothing here states), and every one of them
/// has a literal verb at the call, the same one. Anything else states none.
#[derive(Debug, Default)]
struct OpenVerbs {
    verbs: BTreeSet<String>,
    unread: bool,
    closed: bool,
}

impl OpenVerbs {
    fn read(&mut self, effect: &Effect, method: &MethodValue) {
        match method {
            MethodValue::Lit(verb) if !effect.verb && is_http_method(&verb.to_uppercase()) => {
                self.verbs.insert(verb.to_uppercase());
            }
            _ => self.unread = true,
        }
    }

    fn verb(self) -> Option<String> {
        if self.unread || self.closed || self.verbs.len() != 1 {
            return None;
        }
        self.verbs.into_iter().next()
    }
}

/// What the requests whose method is a parameter declared as a set of verbs
/// wait on (carrick#2049).
#[derive(Debug, Default)]
struct DeclaredMethods {
    /// The verbs the calls of each request whose method is a parameter write
    /// for it, by the request's line.
    supplied: HashMap<RequestSite, BTreeSet<String>>,
    /// The requests whose method is a parameter that some call hands a value
    /// this pass does not read as a verb.
    unread: HashSet<RequestSite>,
    /// The rows a request line states for a method declared as a set of
    /// verbs, kept until every call of it has been read.
    pending: Vec<DeclaredRows>,
    /// The calls whose every request has its verb settled at the call but
    /// its URL still open ([`StatedVerb`], carrick#2676), by file.
    stated: HashMap<PathBuf, Vec<StatedVerb>>,
}

/// One row per verb a request's method parameter is declared with.
#[derive(Debug)]
struct DeclaredRows {
    file: PathBuf,
    span_start: u32,
    site: RequestSite,
    rows: Vec<SummaryRow>,
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

    /// What each function of the service hands its caller ([`ReturnChain`],
    /// carrick#4371), for the site pass.
    pub fn returned_records(&self) -> ReturnChains {
        (*self.returned).clone()
    }

    /// Where the rows of each request stand (carrick#2524): the request's
    /// file and span start ([`SummaryRow::request_at`]) to the file and line
    /// of every row stated for it, with the calls each row at a caller was
    /// composed through, and the chains to callers saved as a stop (C11 T1).
    pub fn placements(&self) -> decided_sites::SummaryPlacements {
        let mut placed = decided_sites::SummaryPlacements::default();
        for (file, sites) in &self.rows {
            for row in sites.values().flatten() {
                let at = (file.clone(), row.line);
                let through = self.chains.rows.get(&(row.request_at.clone(), at.clone()));
                placed.place(row.request_at.clone(), at, through);
            }
        }
        for chain in &self.chains.stopped {
            placed.stop(chain.request_at.clone(), &chain.through);
        }
        placed
    }

    /// Withdraw every row whose request is made by a call the source states
    /// as an endpoint (carrick#2383). `registered` holds, per file, the span
    /// starts of the calls an endpoint row sits at, from whatever source
    /// stated it. A call a file states as a route registers one: reading it
    /// as the request it is shaped like (a path, an options bag, a handler)
    /// would state it as a fact at every caller of the function it sits in.
    /// Returns how many rows went.
    pub fn withdraw_registrations(
        &mut self,
        registered: &HashMap<PathBuf, BTreeSet<u32>>,
        facts: &[(String, u32)],
    ) -> usize {
        let mut withdrawn = 0;
        for sites in self.rows.values_mut() {
            for rows in sites.values_mut() {
                let before = rows.len();
                rows.retain(|row| {
                    !registered
                        .get(&row.request_at.0)
                        .is_some_and(|spans| spans.contains(&row.request_at.1))
                        && !facts.iter().any(|(file, start)| {
                            row.request_at.0.ends_with(Path::new(file))
                                && row.request_at.1 == *start
                        })
                });
                withdrawn += before - rows.len();
            }
            sites.retain(|_, rows| !rows.is_empty());
        }
        self.rows.retain(|_, sites| !sites.is_empty());
        withdrawn
    }

    /// The requests written in `file` that their callers' rows state, by
    /// span start (carrick#3961): the request's own line leaves its path to
    /// the callers, so it states no row, and every call of every function
    /// holding it open was read and stated a row (no caller stop, no caller
    /// out of this pass's sight). A row stated at the request's own line by
    /// any other source is the same request stated once more.
    pub fn answered_at_callers(&self, file: &Path) -> BTreeSet<u32> {
        self.ledger
            .closed
            .iter()
            .filter(|(at_file, _)| at_file == file)
            .filter(|(at_file, start)| {
                !self
                    .rows
                    .get(at_file)
                    .is_some_and(|sites| sites.contains_key(start))
            })
            .filter(|at| {
                self.rows
                    .values()
                    .flat_map(|sites| sites.values().flatten())
                    .any(|row| row.request_at == **at && row.reaches_request.is_none())
            })
            .map(|(_, start)| *start)
            .collect()
    }

    /// The requests written in `file`, by span start, that leave their path
    /// open and whose every caller this pass read and stated, save direct
    /// calls this pass states no row for, of which there is at least one
    /// (carrick#4121, carrick#4127). Whether the request line's row goes is
    /// the site fold's to decide, once it knows whether a fact row stands at
    /// each call. A row stated at the request's own line by any other source
    /// keeps it out, as in [`Self::answered_at_callers`].
    pub fn held_at_fact_calls(&self, file: &Path) -> BTreeMap<u32, HeldAtFactCalls> {
        self.ledger
            .held
            .iter()
            .filter(|((at_file, _), _)| at_file == file)
            .filter(|((at_file, start), _)| {
                !self
                    .rows
                    .get(at_file)
                    .is_some_and(|sites| sites.contains_key(start))
            })
            .map(|((_, start), held)| (*start, held.clone()))
            .collect()
    }

    /// The call sites, by file and span start, whose rows state the request
    /// at `request_at` (its file and span start) as a request their callee
    /// leaves open ([`Self::answered_at_callers`], carrick#3961).
    pub fn stating_callers(&self, request_at: &(PathBuf, u32)) -> BTreeSet<(PathBuf, u32)> {
        self.rows
            .iter()
            .flat_map(|(file, sites)| {
                sites
                    .iter()
                    .filter(|(_, rows)| {
                        rows.iter().any(|row| {
                            row.request_at == *request_at && row.reaches_request.is_none()
                        })
                    })
                    .map(move |(start, _)| (file.clone(), *start))
            })
            .collect()
    }

    /// Call sites of `file` whose callee provably sends nothing, by span
    /// start.
    pub fn silent(&self, file: &Path) -> Option<&BTreeSet<u32>> {
        self.silent.get(file)
    }

    /// The requests each call of `file` rules out ([`Self::ruled_out`]),
    /// by the call's span start.
    pub fn ruled_out(&self, file: &Path) -> Option<&BTreeMap<u32, BTreeSet<(PathBuf, u32)>>> {
        self.ruled_out.get(file)
    }

    /// The function each call of `file` reaches by the compiler's rows,
    /// keyed by the call's `(span_start, span_end)`. A call the rows do not
    /// answer (no rows, or read from the call graph) has no entry.
    pub fn reached(&self, file: &Path) -> Option<&BTreeMap<(u32, u32), ReachedCallee>> {
        self.reached.get(file)
    }

    /// Record what the call reaches, when the rows say it.
    fn record_reach(
        &mut self,
        inputs: &RequestSummaryInputs,
        file: &Path,
        call: &CallIr,
        callee_file: &Path,
        callee: &Summary,
    ) {
        if call.jsx || !inputs.rows_answer(file, call.site.lo, call.site.hi) {
            return;
        }
        self.reached.entry(file.to_path_buf()).or_default().insert(
            (call.site.span_start, call.site.span_end),
            ReachedCallee {
                file: callee_file.to_path_buf(),
                requests: callee
                    .effects
                    .iter()
                    .map(|effect| (effect.site.file.clone(), effect.site.span_start))
                    .collect(),
            },
        );
    }

    /// The verbs the calls of `file` settle for requests whose URL they
    /// leave open ([`StatedVerb`], carrick#2676).
    pub fn verbs(&self, file: &Path) -> &[StatedVerb] {
        self.methods
            .stated
            .get(file)
            .map(Vec::as_slice)
            .unwrap_or_default()
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

    /// The call sites the summaries read as no request because the
    /// compiler's rows cannot say what the call graph follows, by the rows'
    /// reason (carrick#2557).
    pub fn unfollowed(&self) -> &BTreeMap<String, usize> {
        &self.unfollowed
    }

    /// The callers of a function of the service whose request the send side
    /// could not compose, in the order the pass met them (carrick#3688 F).
    pub fn caller_stops(&self) -> Vec<CallerStop> {
        self.caller_stops.clone()
    }

    pub fn unfollowed_count(&self) -> usize {
        self.unfollowed.values().sum()
    }

    /// The call sites read from the call graph because the compiler could
    /// not resolve the import they call through (carrick#2557).
    pub fn fell_back(&self) -> usize {
        self.fell_back
    }
}

struct Composer<'a> {
    files: &'a HashMap<PathBuf, FileIr>,
    inputs: &'a RequestSummaryInputs,
    semantics: &'a LibrarySemantics,
    clients: LinkedClients,
    memo: HashMap<(PathBuf, String), Summary>,
    in_progress: HashSet<(PathBuf, String)>,
    /// Each function's [`ReturnChain`], once read.
    returned: HashMap<(PathBuf, String), Option<ReturnChain>>,
    returning: HashSet<(PathBuf, String)>,
}

/// The call a chain of returns ends at ([`ReturnChain::end`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnEnd {
    /// The file as walked (a key of [`RequestSummaryInputs::files`]).
    pub file: PathBuf,
    pub line: u32,
    /// SWC's span of the call.
    pub span_start: u32,
    pub span_end: u32,
    /// The call is the platform's `fetch`.
    pub fetch: bool,
}

/// What a function hands its caller, read down its returns (carrick#4371,
/// READY-4371 R1): the returned call is followed while its callee is a
/// function this pass reads, each parse or member read on the way is a step
/// (in the order made, the end's value first), and the call the chain stops
/// at is the end. [`crate::return_flow::hands_back_body`] says whether the
/// steps hand back the end's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnChain {
    pub steps: Vec<crate::return_flow::Step>,
    pub end: ReturnEnd,
}

/// Every function's [`ReturnChain`], by its file as walked and its key.
#[derive(Debug, Clone, Default)]
pub struct ReturnChains {
    by_function: HashMap<(PathBuf, String), Option<ReturnChain>>,
}

impl ReturnChains {
    /// `None` for a function this pass did not read (another service's);
    /// `Some(None)` for one whose return is no chain of returned calls: a
    /// literal, an operator, an object, a function, nothing, or a call of a
    /// parameter.
    pub fn of(&self, file: &Path, key: &str) -> Option<Option<&ReturnChain>> {
        self.by_function
            .get(&(file.to_path_buf(), key.to_string()))
            .map(Option::as_ref)
    }

    /// [`Self::of`] for the function `key` of the file ending in `file`.
    #[cfg(test)]
    pub(crate) fn find(&self, file: &str, key: &str) -> Option<Option<&ReturnChain>> {
        self.by_function
            .iter()
            .find(|((path, name), _)| path.ends_with(file) && name == key)
            .map(|(_, chain)| chain.as_ref())
    }
}

impl Composer<'_> {
    /// `key`'s [`ReturnChain`] in `file`, memoised. `None` on a cycle.
    fn returned(&mut self, file: &Path, key: &str) -> Option<ReturnChain> {
        let id = (file.to_path_buf(), key.to_string());
        if let Some(known) = self.returned.get(&id) {
            return known.clone();
        }
        let files = self.files;
        let ir = files.get(file)?.functions.get(key)?;
        if !self.returning.insert(id.clone()) {
            return None;
        }
        let record = self.returned_of(file, ir);
        self.returning.remove(&id);
        self.returned.insert(id, record.clone());
        record
    }

    fn returned_of(&mut self, file: &Path, ir: &FnIr) -> Option<ReturnChain> {
        let shape = ir.return_shape.as_ref()?;
        let call = ir
            .calls
            .iter()
            .find(|call| call.site.lo == shape.base.lo.0 && call.site.hi == shape.base.hi.0)?;
        if call.invokes_param.is_some() {
            return None;
        }
        let files = self.files;
        let mut record = match self.target(file, call) {
            Some((callee_file, key))
                if files
                    .get(&callee_file)
                    .is_some_and(|read| read.functions.contains_key(&key)) =>
            {
                self.returned(&callee_file, &key)?
            }
            _ => ReturnChain {
                steps: Vec::new(),
                end: ReturnEnd {
                    file: file.to_path_buf(),
                    line: call.site.line,
                    span_start: call.site.span_start,
                    span_end: call.site.span_end,
                    fetch: self
                        .request_of(file, call)
                        .is_some_and(|request| request.kind == RequestKind::Fetch),
                },
            },
        };
        record.steps.extend(shape.step());
        Some(record)
    }

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

    /// What a call this pass did not compose sends, as [`shape_of`] reads it,
    /// less a request read only off an options bag a response's init could
    /// equally be (carrick#1986).
    ///
    /// `redirect(path, { headers })` and `request(path, { headers })` are the
    /// same call, written the same way: one builds a response and sends
    /// nothing, the other sends a request. The bag cannot tell them apart, so
    /// such a call is a request only where something else the source states
    /// says so:
    ///
    /// - the source waits on its result (`await`, `.then`, `.catch`,
    ///   `.finally`): a request is answered later, a response is built at
    ///   once and returned or thrown;
    /// - the call graph resolves the callee to a function the repository
    ///   defines, outside this service (an own one is composed instead): it
    ///   is code of this repository, not a package's export, and its options
    ///   are read as they always were.
    ///
    /// Anywhere else nothing is stated, and the call is one this pass cannot
    /// read: the body is not complete, so no model row is withdrawn on it.
    ///
    /// With rows, a call they cannot say the callee of is still read as the
    /// request its shape states, as without rows: the rows fail closed on
    /// composing it ([`Self::target`]), never on the request it makes
    /// (carrick#2413 slice D). Its body is not complete ([`Self::compose`]).
    fn request_of<'c>(&self, file: &Path, call: &'c CallIr) -> Option<Cow<'c, RequestShape>> {
        shape_of(call, file, self.semantics, &self.clients).filter(|request| {
            !request.init_only
                || call.waited
                || self
                    .inputs
                    .call_target(file, call.site.lo, call.site.hi)
                    .is_some()
        })
    }

    /// The definition a call resolves to. Without rows, the call graph's
    /// answer for the call's span start, which a chain's outer call shares
    /// with its inner one (carrick#1695). With rows, the call's own.
    fn target(&self, file: &Path, call: &CallIr) -> Option<(PathBuf, String)> {
        if self.inputs.row_calls.is_none() || call.jsx {
            return self.inputs.sites.target(file, call.site.lo).cloned();
        }
        match self.inputs.reach(file, call.site.lo, call.site.hi) {
            row_calls::Reach::Function(file, key) | row_calls::Reach::OutsideService(file, key) => {
                Some((file, key))
            }
            row_calls::Reach::NoFunction | row_calls::Reach::Unknown(_) => None,
        }
    }

    /// Whether the rows cannot say what a call reaches (carrick#2413 slice
    /// D): it may be any function, so it is not composed, and the body it
    /// sits in is not complete, whatever request its shape states.
    fn unknown(&self, file: &Path, call: &CallIr) -> bool {
        self.inputs.row_calls.is_some()
            && !call.jsx
            && matches!(
                self.inputs.reach(file, call.site.lo, call.site.hi),
                row_calls::Reach::Unknown(_)
            )
    }

    /// Why the rows cannot say the callee of a call the call graph follows
    /// into a function of the repository (carrick#2557): the site the rows
    /// take from the summaries. `None` without rows, for a call the rows
    /// answer, and for one the call graph does not follow either.
    fn unfollowed(&self, file: &Path, call: &CallIr) -> Option<HoleReason> {
        if self.inputs.row_calls.is_none() || call.jsx {
            return None;
        }
        let row_calls::Reach::Unknown(reason) = self.inputs.reach(file, call.site.lo, call.site.hi)
        else {
            return None;
        };
        self.inputs.sites.target(file, call.site.lo).map(|_| reason)
    }

    /// The callee a call resolves to, when it is a function this pass read.
    fn callee(&mut self, file: &Path, call: &CallIr) -> Option<(PathBuf, Summary)> {
        let (target_file, key) = self.target(file, call)?;
        let summary = self.summary(&target_file, &key)?;
        Some((target_file, summary))
    }

    fn compose(&mut self, file: &Path, ir: &FnIr) -> Summary {
        let mut summary = Summary {
            effects: BTreeSet::new(),
            through: BTreeMap::new(),
            complete: !ir.bodyless && !ir.unfollowed,
            invokes: ir.invoked_params.clone(),
            passes_body: false,
            returns_request_value: false,
            hands_out: false,
        };
        // What the functions written in this body send through its
        // parameters (carrick#1949), kept apart until `passes_body` is read:
        // that is a statement about the body's own request.
        let mut lifted: Lifted = BTreeMap::new();
        for call in &ir.calls {
            if call.invokes_param.is_some() {
                continue;
            }
            match self.callee(file, call) {
                // A JSX element fills in what its props reach of the
                // component's requests, and nothing else: the component's
                // other requests and its completeness are its own, as they
                // were while an element read as nothing (carrick#1949).
                Some((_, callee)) if call.jsx => {
                    for effect in callee.effects.iter().filter(|effect| effect.has_params()) {
                        let sent = effect.at_call(call, file).on_path(&call.path_guards);
                        if !sent.ruled_out {
                            summary.state(
                                ir.settle_forward(sent),
                                callee.through_call(effect, file, call.site.line),
                            );
                        }
                    }
                }
                Some((_, callee)) => {
                    summary.complete &= callee.complete;
                    summary.hands_out |= callee.hands_out;
                    for effect in &callee.effects {
                        // A way this call's text rules out is never sent
                        // from here (carrick#3679). Nor is one a test on the
                        // path to the call already took the other way
                        // (carrick#4093).
                        let sent = effect.at_call(call, file).on_path(&call.path_guards);
                        if !sent.ruled_out {
                            summary.state(
                                ir.settle_forward(sent),
                                callee.through_call(effect, file, call.site.line),
                            );
                        }
                    }
                    for (index, arg) in call.args.iter().enumerate() {
                        match arg {
                            Value::Callback(nested) if callee.invokes.contains(&index) => {
                                let callback = self.compose(file, &ir.nested[*nested]);
                                summary.complete &= callback.complete;
                                summary.hands_out |= callback.hands_out;
                                for effect in &callback.effects {
                                    let sent =
                                        effect.detached().lifted().on_path(&call.path_guards);
                                    if !sent.ruled_out {
                                        summary.state(sent, callback.through_of(effect));
                                    }
                                }
                            }
                            Value::Callback(nested) => {
                                join_lifted(
                                    &mut lifted,
                                    self.lifted_from(file, &ir.nested[*nested]),
                                );
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
                None if call.jsx => {}
                None => {
                    if let Some(request) = self.request_of(file, call) {
                        for effect in
                            Effect::from_shape(&request, file, call.site, &ir.key_defaults)
                        {
                            // The tests on the path to the send, for the
                            // callers to decide (carrick#4093).
                            let effect = effect.on_path(&call.path_guards);
                            if effect.ruled_out {
                                continue;
                            }
                            summary.state(
                                ir.settle_forward(effect),
                                BTreeSet::from([(file.to_path_buf(), call.site.line)]),
                            );
                        }
                        if self.unknown(file, call) {
                            summary.complete = false;
                        }
                    } else if !call.inert {
                        summary.complete = false;
                    }
                    // A callback handed to a callee this pass cannot read may
                    // or may not run. What it sends through this body's
                    // parameters is still this body's to state (carrick#1949),
                    // and the body is still never complete.
                    for arg in &call.args {
                        if let Value::Callback(nested) = arg {
                            let callback = self.compose(file, &ir.nested[*nested]);
                            if !callback.effects.is_empty() || !callback.complete {
                                summary.complete = false;
                            }
                            join_lifted(&mut lifted, lift_outer(&callback));
                        }
                    }
                }
            }
        }
        // A function written in the body and not handed to a call (a closure
        // held in a local, an object's method handed to a package, a
        // function returned): what it sends through this body's parameters.
        // Whether and when it runs is not this body's to say, so it never
        // makes the body complete, and what it sends without them is stated
        // where it is written. One that may send is handed out
        // ([`Summary::hands_out`]).
        for detached in &ir.detached {
            let written = self.compose(file, detached);
            summary.hands_out |= !written.complete || !written.effects.is_empty();
            join_lifted(&mut lifted, lift_outer(&written));
        }
        summary.passes_body = summary.effects.len() == 1
            && ir
                .return_shape
                .as_ref()
                .is_some_and(|shape| self.hands_back_body(file, ir, shape));
        summary.returns_request_value = summary.effects.len() == 1
            && ir
                .return_shape
                .as_ref()
                .is_some_and(|shape| self.hands_back_request_value(file, ir, shape));
        for (effect, through) in lifted {
            summary.state(ir.settle_forward(effect), through);
        }
        summary
    }

    /// What a function written inside another sends through the enclosing
    /// function's parameters (carrick#1949).
    fn lifted_from(&mut self, file: &Path, nested: &FnIr) -> Lifted {
        lift_outer(&self.compose(file, nested))
    }
}

/// The requests a function written inside another sends through the
/// enclosing function's parameters, as the enclosing function sends them
/// (carrick#1949). The inner function's own parameters are no caller's to
/// fill in: nothing here calls it.
fn lift_outer(summary: &Summary) -> Lifted {
    let mut lifted = Lifted::new();
    for effect in &summary.effects {
        let detached = effect.detached();
        if detached.has_outer() {
            lifted
                .entry(detached.lifted())
                .or_default()
                .extend(summary.through_of(effect));
        }
    }
    lifted
}

/// Lifted effects ([`lift_outer`]) with the calls each was composed through
/// ([`Summary::through`]).
type Lifted = BTreeMap<Effect, BTreeSet<(PathBuf, u32)>>;

/// Join `more` into `lifted`, the calls of an effect both hold joined.
fn join_lifted(lifted: &mut Lifted, more: Lifted) {
    for (effect, through) in more {
        lifted.entry(effect).or_default().extend(through);
    }
}

impl Composer<'_> {
    /// Whether what `ir` returns ([`FnIr::return_shape`]) is its request's
    /// parsed body unchanged (carrick#1601): the response a request this
    /// body makes answers with, parsed, or what a call of a function that
    /// does the same returns. One member of a value is not the body.
    fn hands_back_body(
        &mut self,
        file: &Path,
        ir: &FnIr,
        shape: &crate::return_flow::ReturnShape,
    ) -> bool {
        use crate::return_flow::ShapeKind;
        let Some(call) = ir.calls.iter().find(|call| {
            call.site.lo == shape.base.lo.0
                && call.site.hi == shape.base.hi.0
                && call.invokes_param.is_none()
        }) else {
            return false;
        };
        match (shape.kind, self.callee(file, call)) {
            (ShapeKind::Parsed, None) => self.request_of(file, call).is_some(),
            (ShapeKind::Value, Some((_, callee))) => callee.passes_body,
            _ => false,
        }
    }

    /// Whether what `ir` returns ([`FnIr::return_shape`]) is its request's
    /// own value unread (carrick#3961, [`Summary::returns_request_value`]):
    /// the call that makes the request, handed back whole, other than the
    /// platform's `fetch`, or what a call of a function that does the same
    /// returns. A parse, one member or any other value is not.
    fn hands_back_request_value(
        &mut self,
        file: &Path,
        ir: &FnIr,
        shape: &crate::return_flow::ReturnShape,
    ) -> bool {
        use crate::return_flow::ShapeKind;
        if shape.kind != ShapeKind::Value {
            return false;
        }
        let Some(call) = ir.calls.iter().find(|call| {
            call.site.lo == shape.base.lo.0
                && call.site.hi == shape.base.hi.0
                && call.invokes_param.is_none()
        }) else {
            return false;
        };
        match self.callee(file, call) {
            None => self
                .request_of(file, call)
                .is_some_and(|request| request.kind != RequestKind::Fetch),
            Some((_, callee)) => callee.returns_request_value,
        }
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
        inputs,
        semantics,
        clients: LinkedClients::link(files, &inputs.bindings, inputs.links.as_ref()),
        memo: HashMap::new(),
        in_progress: HashSet::new(),
        returned: HashMap::new(),
        returning: HashSet::new(),
    };
    let mut index = RequestSummaryIndex::default();

    let mut paths: Vec<&PathBuf> = files.keys().collect();
    paths.sort();
    for path in &paths {
        let mut keys: Vec<&String> = files[*path].functions.keys().collect();
        keys.sort();
        for key in keys {
            let ir = &files[*path].functions[key];
            // A constructor body is read by the fact layer alone
            // (carrick#3527): the frozen summaries stay as they were.
            if ir.constructor {
                continue;
            }
            emit(&mut composer, path, ir, &mut index, true);
        }
    }
    // What each function hands its caller (carrick#4371): read by the site
    // pass, which reads a wrapper site's response where the chain says.
    for path in &paths {
        let mut keys: Vec<&String> = files[*path].functions.keys().collect();
        keys.sort();
        for key in keys {
            let record = composer.returned(path, key);
            index
                .returned
                .by_function
                .insert((path.to_path_buf(), key.clone()), record);
        }
    }
    // The calls made where a module is loaded can send nothing too
    // (carrick#2281): `export const list = bind(local, "orders.list")`. Only
    // whether each is silent is read here. A row stated at module level would
    // be a row no function's call states, which this pass does not make.
    for path in &paths {
        let module_level = &files[*path].module_level;
        for call in &module_level.calls {
            if call.invokes_param.is_some() {
                continue;
            }
            if let Some((callee_file, callee)) = composer.callee(path, call) {
                index.record_reach(inputs, path, call, &callee_file, &callee);
                if is_silent(&mut composer, path, module_level, call, &callee) {
                    index
                        .silent
                        .entry(path.to_path_buf())
                        .or_default()
                        .insert(call.site.span_start);
                }
            }
        }
    }
    // A request whose method is a parameter declared as a set of verbs states
    // the verbs its calls write when every call writes one (carrick#2049),
    // and every verb the declaration allows otherwise: a call nothing here
    // reads, or no call at all, may send any of them.
    for pending in std::mem::take(&mut index.methods.pending) {
        let written = index
            .methods
            .supplied
            .get(&pending.site)
            .filter(|verbs| !verbs.is_empty() && !index.methods.unread.contains(&pending.site));
        let stated: Vec<SummaryRow> = pending
            .rows
            .into_iter()
            .filter(|row| written.is_none_or(|verbs| verbs.contains(&row.method)))
            .collect();
        if !stated.is_empty() {
            index
                .rows
                .entry(pending.file)
                .or_default()
                .entry(pending.span_start)
                .or_default()
                .extend(stated);
        }
    }
    for sites in index.rows.values_mut() {
        for rows in sites.values_mut() {
            merge_rows(rows);
        }
    }
    index.ledger.closed = closed_at_callers(&mut composer, &paths, &mut index.ledger);
    index
}

/// The requests whose own line leaves the path open and whose every caller
/// this pass read (carrick#3961): each function holding one open is a
/// function a key names, some call this pass composed reaches it, and no
/// call this pass did not compose does; no caller closed it without a row;
/// and the request is written in a function a key names, holding it open.
/// A request whose other callers are read so but which a call this pass
/// states no row for also reaches (made where a module is loaded, in a
/// constructor or a callback, or optional) is not closed: it goes to
/// `ledger.held` (carrick#4121, carrick#4127). A call through a parameter
/// has no call site to check and keeps the request unread.
fn closed_at_callers(
    composer: &mut Composer<'_>,
    paths: &[&PathBuf],
    ledger: &mut CallerLedger,
) -> HashSet<(PathBuf, u32)> {
    let files = composer.files;
    for path in paths {
        let file = &files[*path];
        // A call this pass states no row for is one the fact layer may
        // state a row for (carrick#4121, carrick#4127): it is named with its
        // call site, not unseen. Only a call through a parameter, which has
        // no call site of its own, stays unseen.
        let module_level = &file.module_level;
        for call in &module_level.calls {
            note_call(composer, path, call, ledger);
        }
        for call in module_level
            .optional_calls
            .iter()
            .chain(&module_level.constructions)
        {
            note_call(composer, path, call, ledger);
        }
        for inner in module_level
            .nested
            .iter()
            .chain(&module_level.detached)
            .chain(&module_level.param_functions)
        {
            note_calls_from(composer, path, inner, ledger);
        }
        for ir in &file.constructor_defaults {
            note_calls_from(composer, path, ir, ledger);
        }
        for ir in file.functions.values() {
            if ir.constructor {
                note_calls_from(composer, path, ir, ledger);
            } else {
                note_calls_unread(composer, path, ir, ledger);
            }
        }
    }
    let mut holders: HashMap<(PathBuf, u32), Vec<(PathBuf, String)>> = HashMap::new();
    let mut lines: HashMap<(PathBuf, u32), u32> = HashMap::new();
    let mut written_in: HashSet<(PathBuf, u32)> = HashSet::new();
    for path in paths {
        let mut keys: Vec<&String> = files[*path].functions.keys().collect();
        keys.sort();
        for key in keys {
            let ir = &files[*path].functions[key];
            if ir.constructor {
                continue;
            }
            let Some(summary) = composer.summary(path, key) else {
                continue;
            };
            for effect in summary.effects.iter().filter(|effect| effect.has_params()) {
                let at = (effect.site.file.clone(), effect.site.span_start);
                if effect.site.file == **path
                    && ir
                        .calls
                        .iter()
                        .any(|call| call.site.span_start == effect.site.span_start)
                {
                    written_in.insert(at.clone());
                }
                lines.insert(at.clone(), effect.site.line);
                holders
                    .entry(at)
                    .or_default()
                    .push(((*path).clone(), key.clone()));
            }
        }
    }
    // A request no caller closed without a row, held open only by functions
    // whose every call is one this pass composed, or a call it states no row
    // for (carrick#4121, carrick#4127).
    let readable = |at: &(PathBuf, u32), held: &[(PathBuf, String)]| {
        written_in.contains(at)
            && !ledger.stops.contains(at)
            && !ledger.open.contains(at)
            && held.iter().all(|holder| {
                (ledger.seen.contains(holder) || ledger.fact_calls.contains_key(holder))
                    && !ledger.unseen.contains(holder)
            })
    };
    let mut closed = HashSet::new();
    let mut held_at_fact_calls = HashMap::new();
    for (at, held) in &holders {
        if !readable(at, held) {
            continue;
        }
        let fact_calls: FactCalls = held
            .iter()
            .filter_map(|holder| ledger.fact_calls.get(holder))
            .flatten()
            .cloned()
            .collect();
        if fact_calls.is_empty() {
            closed.insert(at.clone());
        } else {
            held_at_fact_calls.insert(
                at.clone(),
                HeldAtFactCalls {
                    line: lines.get(at).copied().unwrap_or_default(),
                    fact_calls,
                },
            );
        }
    }
    ledger.held = held_at_fact_calls;
    closed
}

/// Names `call`, which this pass states no row for, in the ledger: with its
/// call site for the fact check, or as unseen when it goes through a
/// parameter and has none (carrick#4127).
fn note_call(composer: &Composer<'_>, file: &Path, call: &CallIr, ledger: &mut CallerLedger) {
    let Some(target) = composer.target(file, call) else {
        return;
    };
    if call.invokes_param.is_some() {
        ledger.unseen.insert(target);
    } else {
        ledger.fact_calls.entry(target).or_default().insert((
            file.to_path_buf(),
            call.site.line,
            call.site.span_start,
        ));
    }
}

/// Every call in `ir`, or in a function written in it, named by
/// [`note_call`].
fn note_calls_from(composer: &Composer<'_>, file: &Path, ir: &FnIr, ledger: &mut CallerLedger) {
    for call in ir
        .calls
        .iter()
        .chain(&ir.optional_calls)
        .chain(&ir.constructions)
    {
        note_call(composer, file, call, ledger);
    }
    for inner in ir
        .nested
        .iter()
        .chain(&ir.detached)
        .chain(&ir.param_functions)
    {
        note_calls_from(composer, file, inner, ledger);
    }
}

/// Every call in `ir` that [`emit`] does not read, named by [`note_call`]:
/// an optional call, a construction, and any call in a function written as
/// a parameter's default.
fn note_calls_unread(composer: &Composer<'_>, file: &Path, ir: &FnIr, ledger: &mut CallerLedger) {
    for call in ir.optional_calls.iter().chain(&ir.constructions) {
        note_call(composer, file, call, ledger);
    }
    for inner in &ir.param_functions {
        note_calls_from(composer, file, inner, ledger);
    }
    for inner in ir.nested.iter().chain(&ir.detached) {
        note_calls_unread(composer, file, inner, ledger);
    }
}

/// One row per statement at a site, whatever end its base was reached by:
/// the end says what kind of base it is, not which row it is (carrick#2406).
/// Where the merged rows' ends disagree, no one of them is the base's, so
/// the row keeps the hole an unread value ends at, which states the base
/// as injected rather than as any one of the ends.
fn merge_rows(rows: &mut Vec<SummaryRow>) {
    rows.sort();
    rows.dedup_by(|later, kept| {
        let same = SummaryRow {
            base_end: None,
            ..later.clone()
        } == SummaryRow {
            base_end: None,
            ..kept.clone()
        };
        if same && later.base_end != kept.base_end {
            kept.base_end = Some(Root::Hole(HoleReason::UnreadExpression));
        }
        same
    });
}

/// Whether a call of `callee`, written in `ir`, provably sends nothing
/// (carrick#1555): the callee is complete and states no request, no callback
/// it invokes states one, and the call is not an element (which reads only
/// what its props fill in). A callee that hands out a function that may send
/// is not silent at a call that writes a route (carrick#2281): the call may be
/// binding that route into what it hands out.
fn is_silent(
    composer: &mut Composer<'_>,
    file: &Path,
    ir: &FnIr,
    call: &CallIr,
    callee: &Summary,
) -> bool {
    if call.jsx || !callee.complete || !callee.effects.is_empty() {
        return false;
    }
    for (index, arg) in call.args.iter().enumerate() {
        if let Value::Callback(nested) = arg
            && callee.invokes.contains(&index)
            && !composer
                .compose(file, &ir.nested[*nested])
                .effects
                .is_empty()
        {
            return false;
        }
    }
    !(callee.hands_out && call.args.iter().any(writes_a_route))
}

/// Whether an argument writes a route-shaped string ([`is_route_shaped`]),
/// itself or at any depth inside an object literal.
fn writes_a_route(value: &Value) -> bool {
    match value {
        Value::Str(pieces) => is_route_shaped(pieces),
        Value::Obj(object) => object.fields.values().any(writes_a_route),
        Value::Callback(_) => false,
    }
}

fn emit(
    composer: &mut Composer<'_>,
    file: &Path,
    ir: &FnIr,
    index: &mut RequestSummaryIndex,
    top_level: bool,
) {
    let files = composer.files;
    for call in &ir.calls {
        if call.invokes_param.is_some() {
            continue;
        }
        if !call.jsx && composer.inputs.fell_back(file, call.site.lo, call.site.hi) {
            index.fell_back += 1;
        }
        let mut rows: Vec<SummaryRow> = Vec::new();
        match composer.callee(file, call) {
            Some((callee_file, callee)) => {
                index.record_reach(composer.inputs, file, call, &callee_file, &callee);
                if let Some(target) = composer.target(file, call) {
                    index.ledger.seen.insert(target);
                }
                let callee_ir = composer
                    .target(file, call)
                    .and_then(|(target, key)| files.get(&target)?.functions.get(&key));
                let mut open_verbs = OpenVerbs::default();
                // The requests this call's text rules out, and those it
                // sends (carrick#4093).
                let mut ruled_out: BTreeSet<(PathBuf, u32)> = BTreeSet::new();
                let mut sent: BTreeSet<(PathBuf, u32)> = BTreeSet::new();
                for effect in &callee.effects {
                    // A JSX element states only what its props fill in
                    // (carrick#1949).
                    if call.jsx && !effect.has_params() {
                        continue;
                    }
                    let instantiated =
                        effect.instantiate_omitting(&call.args, &call.spread_args, &|index| {
                            call.omits(index)
                        });
                    let request = (effect.site.file.clone(), effect.site.span_start);
                    if instantiated.ruled_out {
                        ruled_out.insert(request);
                        continue;
                    }
                    sent.insert(request);
                    // What this call writes for a method that is a parameter
                    // (carrick#2049).
                    if matches!(effect.method, MethodValue::Param(_)) {
                        match &instantiated.method {
                            MethodValue::Lit(verb) => {
                                index
                                    .methods
                                    .supplied
                                    .entry(effect.site.clone())
                                    .or_default()
                                    .insert(verb.clone());
                            }
                            _ => {
                                index.methods.unread.insert(effect.site.clone());
                            }
                        }
                    }
                    if instantiated.has_params() {
                        // Still open: a caller further up fills it. The verb
                        // may be settled here all the same (carrick#2676).
                        open_verbs.read(effect, &instantiated.method);
                        // A function written inside another holds it open
                        // for callers no key names (carrick#3961).
                        if !top_level {
                            index
                                .ledger
                                .open
                                .insert((effect.site.file.clone(), effect.site.span_start));
                        }
                        continue;
                    }
                    open_verbs.closed = true;
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
                    // A function that hands back its one request's parsed
                    // body makes its call worth that body (carrick#1601).
                    // One that hands back the request's own value unread
                    // makes its call worth what the request line's call is
                    // (carrick#3961).
                    let at = if callee.passes_body {
                        RowSite::PassThrough
                    } else if callee.returns_request_value {
                        RowSite::CallerOfRequestValue
                    } else {
                        RowSite::Caller
                    };
                    // The call hands the function its parameters
                    // (carrick#1782). A function that states the whole
                    // request names one endpoint, and a parameter it sends
                    // unchanged is declared as that endpoint's body. One the
                    // caller hands a path is a transport, whose body
                    // parameter is declared for every endpoint it reaches:
                    // there the site's own payload is the better reading.
                    let call_body = callee_ir.and_then(|callee| callee.call_body(effect));
                    let call_body = match (&reaches, call_body) {
                        (Some(_), call_body) => call_body,
                        (None, Some(CallBody::Built)) => Some(CallBody::Built),
                        (None, _) => None,
                    };
                    match row(&instantiated, file, files, call, reaches, at, call_body) {
                        Some(row) => {
                            index
                                .chains
                                .rows
                                .entry((row.request_at.clone(), (file.to_path_buf(), row.line)))
                                .or_default()
                                .extend(callee.through_call(effect, file, call.site.line));
                            rows.push(row);
                        }
                        None => {
                            index.undetermined += 1;
                            index.ledger.stops.insert((
                                instantiated.site.file.clone(),
                                instantiated.site.span_start,
                            ));
                            // The request is closed at this caller and does
                            // not render: the caller is a site the send side
                            // could not compose (carrick#3688 F).
                            if let Some((reason, also)) = unrendered(&instantiated, file) {
                                // The summaries answered this caller with a
                                // stop (C11 T1): a site on the same chain
                                // yields to it (`RequestSummaryIndex::placements`).
                                index.chains.stopped.push(StoppedChain {
                                    request_at: (
                                        instantiated.site.file.clone(),
                                        instantiated.site.span_start,
                                    ),
                                    through: callee.through_call(effect, file, call.site.line),
                                });
                                let stop = CallerStop {
                                    file: file.to_path_buf(),
                                    line: call.site.line,
                                    span_start: call.site.span_start,
                                    span_end: call.site.span_end,
                                    callee: call.callee.clone(),
                                    request_at: (
                                        instantiated.site.file.clone(),
                                        instantiated.site.span_start,
                                    ),
                                    reason,
                                    also,
                                };
                                if !index.caller_stops.contains(&stop) {
                                    index.caller_stops.push(stop);
                                }
                            }
                        }
                    }
                }
                let ruled_out: BTreeSet<_> = ruled_out.difference(&sent).cloned().collect();
                if !ruled_out.is_empty() {
                    index
                        .ruled_out
                        .entry(file.to_path_buf())
                        .or_default()
                        .insert(call.site.span_start, ruled_out);
                }
                if let Some(method) = open_verbs.verb() {
                    index
                        .methods
                        .stated
                        .entry(file.to_path_buf())
                        .or_default()
                        .push(StatedVerb {
                            span_start: call.site.span_start,
                            span_end: call.site.span_end,
                            line: call.site.line,
                            method,
                        });
                }
                if is_silent(composer, file, ir, call, &callee) {
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
                // A function of the repository this pass did not compose
                // (carrick#3961): its callers are not all read.
                if let Some(target) = composer
                    .target(file, call)
                    .or_else(|| composer.inputs.sites.target(file, call.site.lo).cloned())
                {
                    index.ledger.unseen.insert(target);
                }
                let request = composer.request_of(file, call);
                if request.is_none()
                    && let Some(reason) = composer.unfollowed(file, call)
                {
                    *index
                        .unfollowed
                        .entry(Stop::from(reason).as_str().to_string())
                        .or_default() += 1;
                }
                if let Some(mut request) = request {
                    // A function written inside another has callers this pass
                    // does not resolve, so what its declaration allows is not
                    // what it sends (carrick#2049): its calls are the model's
                    // to read.
                    if !top_level && !request.declared_methods.is_empty() {
                        request.to_mut().declared_methods.clear();
                    }
                    let library = request.kind == RequestKind::Library;
                    if library
                        || (request.kind != RequestKind::Verb
                            && (!request.url_inline || request.fans_out_inline()))
                    {
                        for effect in
                            Effect::from_shape(&request, file, call.site, &ir.key_defaults)
                        {
                            let site = effect.site.clone();
                            let declared = matches!(effect.method, MethodValue::Param(_))
                                && !request.declared_methods.is_empty();
                            let mut verbs: Vec<SummaryRow> = Vec::new();
                            for effect in effect.each_declared_method(&request.declared_methods) {
                                if !effect.has_params() {
                                    let at = if library {
                                        RowSite::Library
                                    } else {
                                        RowSite::Own
                                    };
                                    match row(&effect, file, composer.files, call, None, at, None) {
                                        Some(row) if declared => verbs.push(row),
                                        Some(row) => rows.push(row),
                                        None => {
                                            index.undetermined += 1;
                                        }
                                    }
                                }
                            }
                            // Which of the declared verbs the line states waits
                            // on every call of the request having been read.
                            if declared && !verbs.is_empty() {
                                index.methods.pending.push(DeclaredRows {
                                    file: file.to_path_buf(),
                                    span_start: call.site.span_start,
                                    site,
                                    rows: verbs,
                                });
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
        emit(composer, file, nested, index, false);
    }
}

/// A call of a function the service declares whose request is closed at the
/// call and does not render as a route (carrick#3688 F): the send side could
/// not compose it into a row. Saved as an unread site
/// ([`crate::unread_sites::from_scan`]); never a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerStop {
    pub file: PathBuf,
    pub line: u32,
    pub span_start: u32,
    pub span_end: u32,
    /// The call as written.
    pub callee: String,
    /// The file and span start of the request the callee sends: the labelled
    /// call the callee's request reaches stands there.
    pub request_at: (PathBuf, u32),
    /// The first hole in the request's URL, or `unread` when it holds none.
    pub reason: Stop,
    /// The URL's other holes, distinct, in order.
    pub also: Vec<Stop>,
}

/// Why `effect`, closed at its caller, states no row for want of a URL that
/// renders: the first hole its URL holds ([`Stop`]), and the others. A URL
/// that holds no hole and still does not render (a piece the source does
/// not write, a parameter that is not a whole segment) is `unread`.
/// `None` where the effect is not closed on a literal method, or the row is
/// refused for another reason ([`row`]).
fn unrendered(effect: &Effect, file: &Path) -> Option<(Stop, Vec<Stop>)> {
    if !matches!(effect.method, MethodValue::Lit(_)) || !base_reads_the_same_in(effect, file) {
        return None;
    }
    let joined = join_base(&effect.base, &effect.url);
    if render_target(&joined).is_some() {
        return None;
    }
    let mut holes: Vec<Stop> = Vec::new();
    for piece in &joined {
        if let Piece::Opaque {
            end: Root::Hole(reason),
            ..
        } = piece
        {
            let stop = Stop::from(*reason);
            if !holes.contains(&stop) {
                holes.push(stop);
            }
        }
    }
    let mut holes = holes.into_iter();
    let first = holes.next().unwrap_or(Stop::Unread);
    Some((first, holes.take(MAX_ALSO).collect()))
}

/// The most "also" words a [`CallerStop`] carries, as an unread site does.
const MAX_ALSO: usize = 4;

/// Where a row's call expression sits relative to the request it states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowSite {
    /// The request primitive itself (`fetch(...)`).
    Own,
    /// A call through a verified library client: the call is the request.
    Library,
    /// A call to a function the service declares, which makes the request
    /// inside it and hands back something else (carrick#1601).
    Caller,
    /// A call to a function the service declares, which makes one request
    /// and hands back its parsed body unchanged: the call's value is the
    /// body ([`Summary::passes_body`]).
    PassThrough,
    /// A call to a function the service declares, which makes one request
    /// and hands back that request's own value unread: the call's value is
    /// what the request line's call is worth
    /// ([`Summary::returns_request_value`], carrick#3961).
    CallerOfRequestValue,
}

/// The row an effect supports at `site`, when its URL and method are both
/// stated.
fn row(
    effect: &Effect,
    file: &Path,
    files: &HashMap<PathBuf, FileIr>,
    call: &CallIr,
    reaches_request: Option<String>,
    at: RowSite,
    call_body: Option<CallBody>,
) -> Option<SummaryRow> {
    let site = call.site;
    let MethodValue::Lit(method) = &effect.method else {
        return None;
    };
    if !base_reads_the_same_in(effect, file) {
        return None;
    }
    let joined = join_base(&effect.base, &effect.url);
    let target = render_target(&joined)?;
    let base_end = match joined.first() {
        Some(Piece::Opaque { end, .. }) => Some(end.clone()),
        _ => None,
    };
    let body_literals = effect
        .body
        .iter()
        .filter_map(|(key, pieces)| match pieces.as_slice() {
            [Piece::Lit(value)] => Some((key.clone(), value.clone())),
            _ => None,
        })
        .collect();
    Some(SummaryRow {
        request_at: (effect.site.file.clone(), effect.site.span_start),
        callee: call.callee.clone(),
        span_start: site.span_start,
        span_end: site.span_end,
        line: site.line,
        method: method.clone(),
        target,
        body_literals,
        reaches_request,
        own_site: at == RowSite::Own,
        at_caller: matches!(at, RowSite::Caller | RowSite::CallerOfRequestValue),
        returns_request_value: at == RowSite::CallerOfRequestValue,
        call_body,
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
        base_end,
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
            // An environment read written as the read itself; one a binding
            // renamed is written by a name another file may bind otherwise.
            Piece::Opaque {
                text,
                end: Root::Env { written, .. },
                ..
            } => text == written,
            Piece::Opaque { .. }
            | Piece::Param(..)
            | Piece::ParamKey(..)
            | Piece::ParamKeyOr(..)
            | Piece::Outer(..)
            | Piece::Query
            | Piece::Unknown => false,
        }),
        _ => true,
    }
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
        init_only: false,
        declared_methods: Vec::new(),
        alternation: Alternation::Single,
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
    if base.iter().any(|piece| match piece {
        Piece::Lit(text) => text.contains(['?', '#']),
        Piece::Query => true,
        _ => false,
    }) {
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
/// other opaque value stands for a whole segment. A query one of the
/// service's own functions builds may end it (carrick#1950), and is not
/// written: it may be nothing, and it is no part of the route.
fn render_target(url: &[Piece]) -> Option<String> {
    let url = route_of(url);
    let url = url.as_ref();
    if url.iter().enumerate().any(|(position, piece)| match piece {
        Piece::Unknown => true,
        // A value that reads as several paths is no one path (carrick#2541).
        Piece::Opaque {
            end: Root::Hole(HoleReason::SeveralPaths),
            ..
        } => true,
        Piece::Param(..) | Piece::ParamKey(..) | Piece::ParamKeyOr(..) | Piece::Outer(..) => {
            !is_whole_segment(url, position)
        }
        // Text after a query that may be empty would continue the path.
        Piece::Query => position + 1 != url.len(),
        _ => false,
    }) {
        return None;
    }
    let (base, path) = match url.first()? {
        Piece::Opaque { text: base, .. } => (Some(base.as_str()), &url[1..]),
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
            Piece::Opaque { text: name, .. }
            | Piece::Param(_, name)
            | Piece::ParamKey(_, _, name)
            | Piece::ParamKeyOr(_, _, name, _) => {
                if !is_whole_segment(path, position) {
                    return None;
                }
                rendered.push_str(&format!("${{{name}}}"));
            }
            Piece::Outer(..) => {
                if !is_whole_segment(path, position) {
                    return None;
                }
                let name = piece.parameter_name().unwrap_or_default();
                rendered.push_str(&format!("${{{name}}}"));
            }
            Piece::Query => {}
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

/// The route part of a URL whose query holds a value the source does not
/// write (carrick#2519): `/groups/${id}?workspace=${workspace}` or
/// `/items?${params}`. Text from the first literal `?` on is the query, not
/// the route, so a value there is no path parameter: the URL is cut before
/// the `?`. A query written all in literal text is kept as it was, so no
/// target the source states whole changes.
fn route_of(url: &[Piece]) -> Cow<'_, [Piece]> {
    let Some((index, at)) = url
        .iter()
        .enumerate()
        .find_map(|(index, piece)| match piece {
            Piece::Lit(text) => text.find('?').map(|at| (index, at)),
            _ => None,
        })
    else {
        return Cow::Borrowed(url);
    };
    if url[index + 1..]
        .iter()
        .all(|piece| matches!(piece, Piece::Lit(_)))
    {
        return Cow::Borrowed(url);
    }
    let mut route = url[..index].to_vec();
    if let Piece::Lit(text) = &url[index]
        && at > 0
    {
        route.push(Piece::Lit(text[..at].to_string()));
    }
    Cow::Owned(route)
}

/// Whether the piece at `position` stands for a whole path segment: it
/// follows a literal ending in `/`, and ends at the next `/`, `?`, `#`, a
/// built query ([`Piece::Query`]) or the end of the URL.
fn is_whole_segment(url: &[Piece], position: usize) -> bool {
    let after_slash = position > 0
        && matches!(url.get(position - 1), Some(Piece::Lit(prev)) if prev.ends_with('/'));
    let segment_ends = match url.get(position + 1) {
        None | Some(Piece::Query) => true,
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
        Piece::Opaque {
            text: text.to_string(),
            end: Root::Hole(HoleReason::UnreadExpression),
            link: None,
        }
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

    /// carrick#2519: a value written in the query is no path parameter; the
    /// route is the path before the `?`. A query all in literal text is kept.
    #[test]
    fn a_value_in_the_query_is_cut_from_the_route() {
        assert_eq!(
            render_target(&[
                lit("/groups/"),
                Piece::Param(0, "id".to_string()),
                lit("?workspace="),
                Piece::Param(1, "workspace".to_string()),
            ])
            .as_deref(),
            Some("/groups/${id}")
        );
        assert_eq!(
            render_target(&[opaque("base"), lit("/items?"), opaque("params.toString()")])
                .as_deref(),
            Some("${base}/items")
        );
        assert_eq!(
            render_target(&[lit("/items?limit=10")]).as_deref(),
            Some("/items?limit=10")
        );
        // A value before the `?` still has to be a whole segment.
        assert_eq!(
            render_target(&[lit("/v"), opaque("version"), lit("?x="), opaque("y")]),
            None
        );
        assert_eq!(render_target(&[lit("?q="), opaque("q")]), None);
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

    fn keyed(index: usize, key: &str) -> Piece {
        Piece::ParamKey(index, key.to_string(), key.to_string())
    }

    /// A request whose URL is `url`, sent with `POST`.
    fn effect(url: Vec<Piece>) -> Effect {
        Effect {
            site: RequestSite {
                file: PathBuf::from("src/client.ts"),
                line: 1,
                span_start: 0,
            },
            method: MethodValue::Lit("POST".to_string()),
            url,
            body: BTreeMap::new(),
            body_param: None,
            body_under: None,
            verb: false,
            base: Vec::new(),
            base_scope: None,
            semantics: BTreeSet::new(),
            key_defaults: BTreeMap::new(),
            guards: Vec::new(),
            path_guards: Vec::new(),
            ruled_out: false,
        }
    }

    fn object(fields: &[(&str, Vec<Piece>)], open: bool) -> Value {
        Value::Obj(ObjValue {
            fields: fields
                .iter()
                .map(|(key, pieces)| (key.to_string(), Value::Str(pieces.clone())))
                .collect(),
            open,
            under_param: None,
            under_rest: None,
            unsettled: BTreeSet::new(),
            functions: BTreeSet::new(),
            wide: open,
        })
    }

    /// carrick#1949: an enclosing function's parameter waits on that
    /// function's caller only where the request states no route without it,
    /// and lifting moves it one function out.
    #[test]
    fn an_enclosing_parameter_waits_only_where_no_route_is_stated_without_it() {
        let outer = |name: &str| Piece::Param(0, name.to_string()).enclosed();

        let mut whole = effect(vec![outer("endpoint")]);
        whole.settle_outer();
        assert!(whole.has_outer());
        assert_eq!(
            whole.lifted().url,
            vec![Piece::Param(0, "endpoint".to_string())]
        );

        let mut based = effect(vec![outer("endpoint"), lit("/refresh")]);
        based.settle_outer();
        assert!(!based.has_outer());
        assert_eq!(
            render_target(&based.url).as_deref(),
            Some("${endpoint}/refresh")
        );

        let twice = Piece::ParamKey(1, "url".to_string(), "url".to_string())
            .enclosed()
            .enclosed();
        assert_eq!(
            twice.lifted(),
            Piece::ParamKey(1, "url".to_string(), "url".to_string()).enclosed()
        );
    }

    /// carrick#1950: a built query ends the path, and the target leaves it
    /// out.
    #[test]
    fn a_built_query_ends_the_path_and_is_left_out_of_the_target() {
        assert_eq!(
            render_target(&[
                opaque("this.base"),
                lit("/sessions/open/"),
                Piece::Param(0, "kind".to_string()),
                Piece::Query,
            ])
            .as_deref(),
            Some("${this.base}/sessions/open/${kind}")
        );
        assert_eq!(
            render_target(&[lit("/sessions"), Piece::Query]).as_deref(),
            Some("/sessions")
        );
        // Text after a query that may be empty would continue the path.
        assert_eq!(
            render_target(&[lit("/sessions/"), opaque("id"), Piece::Query, lit("/tail")]),
            None
        );
        // A query is no route, and no base.
        assert_eq!(render_target(&[opaque("base"), Piece::Query]), None);
        assert_eq!(render_target(&[Piece::Query, lit("/sessions")]), None);
        assert_eq!(
            join_base(&[lit("/api"), Piece::Query], &[lit("/sessions")]),
            vec![Piece::Unknown]
        );
    }

    /// carrick#1950: a key of a parameter waits on a caller only where the
    /// request states no route without it.
    #[test]
    fn a_key_waits_on_a_caller_only_where_the_url_states_no_route_without_it() {
        // The whole URL, or a path glued after a base: no route as written.
        for url in [
            vec![keyed(0, "url")],
            vec![opaque("process.env.API"), keyed(0, "path")],
            vec![keyed(0, "base"), keyed(0, "path")],
        ] {
            let mut waiting = effect(url.clone());
            waiting.settle_keys();
            assert_eq!(waiting.url, url);
            assert!(waiting.has_params(), "{url:?}");
            assert_eq!(render_target(&waiting.url), None, "{url:?}");
        }

        // A base before a literal path, and a whole path segment: the route
        // is stated as it was before a key could be filled in, with the key
        // an opaque value under the name the function reads it by.
        let mut based = effect(vec![keyed(0, "base"), lit("/users/"), keyed(0, "id")]);
        based.settle_keys();
        let unfilled = |text: &str| Piece::Opaque {
            text: text.to_string(),
            end: Root::Hole(HoleReason::CallerArgMissing),
            link: None,
        };
        assert_eq!(
            based.url,
            vec![unfilled("base"), lit("/users/"), unfilled("id")]
        );
        assert!(!based.has_params());
        assert_eq!(
            render_target(&based.url).as_deref(),
            Some("${base}/users/${id}")
        );

        // A positional parameter still leaves the request open, and the key
        // beside it is filled in with it.
        let mut mixed = effect(vec![
            Piece::Param(0, "base".to_string()),
            lit("/users/"),
            keyed(1, "id"),
        ]);
        mixed.settle_keys();
        assert!(mixed.has_params());
        assert_eq!(mixed.url[2], keyed(1, "id"));
    }

    /// carrick#1950: the object a caller writes fills the key in, and
    /// anything the object does not state says nothing.
    #[test]
    fn a_key_is_filled_only_from_what_the_callers_object_states() {
        let wrapper = effect(vec![keyed(0, "url")]);
        let route = vec![opaque("process.env.API"), lit("/users")];

        // The key, written.
        let filled = wrapper.instantiate(&[object(&[("url", route.clone())], false)], &[]);
        assert_eq!(filled.url, route);
        assert!(!filled.has_params());
        // Written after a spread: an open object still states the key.
        let after_spread = wrapper.instantiate(&[object(&[("url", route.clone())], true)], &[]);
        assert_eq!(after_spread.url, route);

        // Missing, or overwritten by a spread or a computed key (which empty
        // the object's known keys).
        for args in [
            vec![object(&[("body", vec![lit("x")])], false)],
            vec![object(&[], true)],
            // Not an object at all.
            vec![Value::Str(vec![lit("/users")])],
            vec![Value::Str(vec![opaque("request")])],
            // No argument.
            vec![],
        ] {
            assert_eq!(
                wrapper.instantiate(&args, &[]).url,
                vec![Piece::Unknown],
                "{args:?}"
            );
        }
        // A key that holds an object, not text.
        let nested = Value::Obj(ObjValue {
            fields: BTreeMap::from([("url".to_string(), object(&[], false))]),
            open: false,
            under_param: None,
            under_rest: None,
            unsettled: BTreeSet::new(),
            functions: BTreeSet::new(),
            wide: false,
        });
        assert_eq!(
            wrapper.instantiate(&[nested], &[]).url,
            vec![Piece::Unknown]
        );
    }

    /// carrick#1950: a caller that passes its own parameter on leaves the key
    /// to its own caller, and the site that writes the object states the row.
    #[test]
    fn a_key_passed_on_stays_open_for_the_next_caller() {
        let wrapper = effect(vec![keyed(0, "url")]);
        let passed_on = wrapper.instantiate(
            &[Value::Str(vec![Piece::Param(1, "options".to_string())])],
            &[],
        );
        assert_eq!(
            passed_on.url,
            vec![Piece::ParamKey(1, "url".to_string(), "url".to_string())]
        );
        assert!(passed_on.has_params());
        let route = vec![lit("/users")];
        let filled = passed_on.instantiate(
            &[
                Value::Str(vec![lit("first")]),
                object(&[("url", route.clone())], false),
            ],
            &[],
        );
        assert_eq!(filled.url, route);
    }

    /// The functions of `source` that [`module_query_builders`] reads as
    /// query builders, by name, sorted.
    fn query_builders_in(source: &str) -> Vec<String> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("input.ts");
        std::fs::write(&path, source).expect("write file");
        let cm: Lrc<SourceMap> = Default::default();
        let handler = swc_common::errors::Handler::with_tty_emitter(
            swc_common::errors::ColorConfig::Never,
            true,
            false,
            Some(cm.clone()),
        );
        let module = crate::parser::parse_file(&path, &cm, &handler).expect("parsed module");
        let mut reassigned = Reassigned::default();
        module.visit_with(&mut reassigned);
        let mut names: Vec<String> = module_query_builders(&module, &reassigned)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        names.sort();
        names
    }

    /// carrick#1950: a query builder is a function whose every `return` is
    /// the empty string or text that starts with a literal `?`.
    #[test]
    fn a_function_whose_every_return_is_a_query_or_nothing_builds_a_query() {
        assert_eq!(
            query_builders_in(
                "function conditional(c?: string) { return c ? `?c=${c}` : \"\"; }\n\
                 function early(c?: string) { if (!c) { return \"\"; } return \"?c=\" + c; }\n\
                 function joined(parts: string[]) {\n\
                   return parts.length === 0 ? '' : '?' + parts.join('&');\n\
                 }\n\
                 const arrow = (c?: string) => (c ? `?c=${c}` : ``);\n\
                 const block = (c: string) => { return `?c=${c}`; };\n\
                 export function exported(c: string) { return (`?c=${c}` as string); }\n",
            ),
            [
                "arrow",
                "block",
                "conditional",
                "early",
                "exported",
                "joined"
            ]
        );
    }

    /// carrick#1950: one `return` that is anything else, a value that
    /// arrives through a promise, and a body that can end without returning
    /// are each no query builder.
    #[test]
    fn a_function_that_may_return_anything_else_builds_no_query() {
        assert_eq!(
            query_builders_in(
                "function path(c?: string) { return c ? `/${c}` : \"\"; }\n\
                 function held(c: string) { const q = `?c=${c}`; return q; }\n\
                 function oneOther(c?: string) { if (!c) { return c; } return `?c=${c}`; }\n\
                 function emptyFirst(c: string) { return \"\" + c; }\n\
                 function fallsOff(c?: string) { if (c) { return `?c=${c}`; } }\n\
                 function bare(c?: string) { if (!c) { return; } return `?c=${c}`; }\n\
                 function recursive(c: string[]) {\n\
                   if (c.length > 1) { return recursive(c.slice(1)); }\n\
                   return `?c=${c[0]}`;\n\
                 }\n\
                 function called(c: string) { return encode(`?c=${c}`); }\n\
                 async function promised(c: string) { return `?c=${c}`; }\n\
                 function* generated(c: string) { return `?c=${c}`; }\n\
                 const promisedArrow = async (c: string) => `?c=${c}`;\n\
                 let rebound = (c: string) => `?c=${c}`;\n\
                 function replaced(c: string) { return `?c=${c}`; }\n\
                 replaced = path;\n\
                 function fragment(c: string) { return `#${c}`; }\n",
            ),
            Vec::<String>::new()
        );
    }

    /// carrick#1950: a nested function's `return` is its own, so it neither
    /// makes nor breaks the function it is written in.
    #[test]
    fn a_nested_functions_return_is_not_the_builders() {
        assert_eq!(
            query_builders_in(
                "function outer(parts: string[]) {\n\
                   const shown = parts.map((part) => { return part.trim(); });\n\
                   return shown.length === 0 ? \"\" : \"?\" + shown.join(\"&\");\n\
                 }\n",
            ),
            ["outer"]
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
                ..ObjValue::default()
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
                ..ObjValue::default()
            },
            ObjValue {
                fields: BTreeMap::from([("op".to_string(), Value::Str(vec![lit("x")]))]),
                ..ObjValue::default()
            },
        ]);
        assert_eq!(same.fields.get("op"), Some(&Value::Str(vec![lit("x")])));
        assert!(!same.open, "every branch states the same value");
    }

    /// The file IR `source` reads as.
    pub(super) fn file_ir_of(source: &str) -> FileIr {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("input.ts");
        std::fs::write(&path, source).expect("write file");
        let cm: Lrc<SourceMap> = Default::default();
        let handler = swc_common::errors::Handler::with_tty_emitter(
            swc_common::errors::ColorConfig::Never,
            true,
            false,
            Some(cm.clone()),
        );
        let module = crate::parser::parse_file(&path, &cm, &handler).expect("parsed module");
        extract_file_ir(&module, &cm, &HashSet::new())
    }

    /// carrick#3504 B10-2a2: the dry run's name census reads the term the
    /// site path reads: past `this` for a decided `call`, past the arguments
    /// a `bind` filled.
    #[test]
    fn the_dry_run_reads_the_argument_the_site_path_reads() {
        use decided_sites::{ArgSlot, dry_run_term};
        let file = file_ir_of(
            "declare const send: (p: string, q: string) => void;\n\
             export function viaCall(name: string) { send.call(this, name, 'x'); }\n\
             export function viaBind(name: string) { send('/p', name); }\n",
        );
        let slot = |arg| ArgSlot { arg, key: None };

        let mut call = file.functions["viaCall"]
            .read_calls()
            .map(|(call, _)| call.clone())
            .find(|call| call.language_call.is_some())
            .expect("the call");
        call.language_call.as_mut().expect("language call").decided = true;
        assert_eq!(
            dry_run_term(&call, &slot(0)).as_ref(),
            call.arg_texts[1].as_ref(),
            "slot 0 of a decided `call` is the argument after `this`"
        );
        assert_eq!(
            call.passed_arg(0).is_some(),
            dry_run_term(&call, &slot(0)).is_some()
        );

        // `send.bind(null, '/p')(name)`: one argument bound ahead.
        let mut call = file.functions["viaBind"]
            .read_calls()
            .map(|(call, _)| call.clone())
            .find(|call| call.arg_texts.len() == 2)
            .expect("the call");
        call.bound = vec![Value::Str(vec![lit("/p")])];
        assert_eq!(dry_run_term(&call, &slot(0)), None, "bound, not written");
        assert_eq!(
            dry_run_term(&call, &slot(1)).as_ref(),
            call.arg_texts[0].as_ref(),
            "slot 1 is the call's first own argument"
        );
    }

    /// carrick#3559 (R2): without the resolver every binding key is
    /// `(name, empty)`, so a use cannot be told from a declaration below and
    /// the count check fails closed: a dropped module name stays dropped.
    #[test]
    fn without_the_resolver_a_dropped_name_stays_dropped() {
        let source = "import { Queue } from \"@fixture/queue\";\n\
            const parcels = new Queue(\"parcels\");\n\
            export async function ship() {\n\
            \x20 await parcels.add(\"job\", {});\n\
            }\n\
            export function label(parcels: string) {\n\
            \x20 return { url: parcels };\n\
            }\n";
        let file = super::file_ir_of(Path::new("src/ship.ts"), source).expect("parses");
        let held = file.functions["ship"].library.iter().any(|site| {
            matches!(&site.binding, library_sites::SiteBinding::Client(binding)
                if matches!(**binding, ClientBinding::Module(_)))
        });
        assert!(!held, "{:#?}", file.functions["ship"].library);
    }

    /// The body object the first request `function` makes in `source` sends.
    fn request_body(source: &str, function: &str) -> ObjValue {
        let file = file_ir_of(source);
        let ir = &file.functions[function];
        match ir.calls.iter().find_map(|call| call.request.as_ref()) {
            Some(RequestShape {
                body: BodyValue::Fields(obj),
                ..
            }) => obj.clone(),
            other => panic!("{function} sends no object body: {other:?}"),
        }
    }

    /// What the object `literal` (written as a request body) states about
    /// `a`, `b` and `c`, in that order.
    fn key_states(literal: &str) -> Vec<String> {
        let source = format!(
            "export async function send(k: string) {{\n  return fetch(\"https://api.example.com/x\", {{ method: \"POST\", body: JSON.stringify({literal}) }});\n}}\n"
        );
        let body = request_body(&source, "send");
        ["a", "b", "c"]
            .iter()
            .map(|key| match body.key(key) {
                KeyState::Known(_) => "known",
                KeyState::Function => "function",
                KeyState::Unread => "unread",
                KeyState::Absent => "absent",
            })
            .map(str::to_string)
            .collect()
    }

    /// carrick#2741: a method, getter or setter with a plain key sets that
    /// key only, so every other absent key is still absent; a computed key
    /// or a spread may set any key.
    #[test]
    fn a_method_sets_its_own_key_and_no_other() {
        assert_eq!(
            key_states("{ a() {}, b: 1 }"),
            ["function", "known", "absent"]
        );
        assert_eq!(
            key_states("{ get a() { return \"x\"; } }"),
            ["unread", "absent", "absent"]
        );
        assert_eq!(
            key_states("{ set a(v) {}, b() {} }"),
            ["unread", "function", "absent"]
        );
        assert_eq!(key_states("{ [k]: 1 }"), ["unread", "unread", "unread"]);
        assert_eq!(
            key_states("{ [k]() {}, b: 1 }"),
            ["unread", "known", "unread"]
        );
        // The last writer of a key wins.
        assert_eq!(
            key_states("{ a() {}, a: 1 }"),
            ["known", "absent", "absent"]
        );
        assert_eq!(
            key_states("{ a: 1, a() {} }"),
            ["function", "absent", "absent"]
        );
        // A spread the source does not state may overwrite the method.
        assert_eq!(
            key_states("{ a() {}, ...k }"),
            ["unread", "unread", "unread"]
        );
        // A method does not widen the object's body: it stays `open`.
        assert!(request_body(
            "export async function send() {\n  return fetch(\"https://api.example.com/x\", { method: \"POST\", body: JSON.stringify({ a() {}, b: 1 }) });\n}\n",
            "send"
        )
        .open);
    }

    const SPREAD_WRAPPERS: &str = r#"
const URL = "https://api.example.com/rpc";
let counter = 0;
export async function held(op: string, args: object) {
  return fetch(URL, { method: "POST", body: JSON.stringify({ op, ...args, mode: "x" }) });
}
export async function twice(op: string, first: object, second: object) {
  return fetch(URL, { method: "POST", body: JSON.stringify({ op, ...first, ...second }) });
}
export async function keyOf(op: string, options: { extra: object }) {
  return fetch(URL, { method: "POST", body: JSON.stringify({ op, ...options.extra }) });
}
export async function reassigned(op: string, given: object) {
  given = { ...given };
  return fetch(URL, { method: "POST", body: JSON.stringify({ op, ...given }) });
}
export async function writtenThrough(op: string, stamped: { stamp?: number }) {
  stamped.stamp = counter;
  return fetch(URL, { method: "POST", body: JSON.stringify({ op, ...stamped }) });
}
export async function computed(op: string, keyed: object, name: string) {
  return fetch(URL, { method: "POST", body: JSON.stringify({ op, ...keyed, [name]: "y" }) });
}
export async function opened(op: string, rest: object, late: object) {
  return fetch(URL, { method: "POST", body: JSON.stringify({ ...rest.inner, op, ...late }) });
}
"#;

    /// carrick#2740: a module-scope arrow under a wrapper that only states a
    /// type is the binding's function, read once: its call is the function's,
    /// and module scope does not read the same body again.
    /// carrick#3356: each function member of a module `const` object nothing
    /// writes through is a function of the module under the extractor's key,
    /// read once: module scope does not read its body again, and still reads
    /// every other entry of the object.
    #[test]
    fn an_object_members_function_is_read_once_under_its_key() {
        let source = r#"
export const catalog = {
  async search(q: string) {
    return fetch(`https://api.example.com/catalog?q=${q}`);
  },
  list: () => fetch("https://api.example.com/catalog"),
  v1: { read: function () { return fetch("https://api.example.com/v1"); } },
  ready: warm(),
};
function warm() { return true; }
export const written = { send() { return fetch("https://api.example.com/w"); } };
written.send = () => Promise.resolve(new Response());
"#;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("input.ts");
        std::fs::write(&path, source).expect("write file");
        let cm: Lrc<SourceMap> = Default::default();
        let handler = swc_common::errors::Handler::with_tty_emitter(
            swc_common::errors::ColorConfig::Never,
            true,
            false,
            Some(cm.clone()),
        );
        let module = crate::parser::parse_file(&path, &cm, &handler).expect("parsed module");
        let keys: HashSet<String> = [
            "catalog.search",
            "catalog.list",
            "catalog.v1.read",
            "written.send",
            "warm",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let file = extract_file_ir(&module, &cm, &keys);
        for key in ["catalog.search", "catalog.list", "catalog.v1.read"] {
            let member = file
                .functions
                .get(key)
                .unwrap_or_else(|| panic!("{key} is a function: {:?}", file.functions.keys()));
            assert_eq!(member.calls.len(), 1, "{key}: {:#?}", member.calls);
            assert!(
                member.calls[0].request.is_some(),
                "{key}: {:#?}",
                member.calls
            );
        }
        assert!(
            !file.functions.contains_key("written.send"),
            "an object the file writes through holds no function of the module"
        );
        let module_calls: Vec<&str> = file
            .module_level
            .calls
            .iter()
            .map(|call| call.callee.as_str())
            .collect();
        assert_eq!(module_calls, vec!["warm"], "{:#?}", file.module_level);
        assert!(
            file.module_level.nested.is_empty(),
            "module scope read a member's body again: {:#?}",
            file.module_level
        );
        assert_eq!(
            file.module_level.detached.len(),
            2,
            "only the written-through object's two functions stay detached: {:#?}",
            file.module_level.detached
        );
    }

    /// carrick#3370: each function member of a default-exported object (a
    /// worker's `fetch`, its `queue` handler) is a function of the module
    /// under the extractor's `default.<member>` key, read once, as a module
    /// `const` object's are (carrick#3356): module scope does not read its
    /// body again, and still reads every other entry of the object.
    #[test]
    fn a_default_exported_objects_members_are_functions_of_the_module() {
        let source = r#"
type Handler = Record<string, unknown>;
function api(path: string) { return fetch(`https://api.example.com${path}`); }
function announce(topic: string) { return queue.send(topic); }
function warm() { return true; }
export default {
  async fetch(request: Request) {
    return api("/health");
  },
  queue: async () => announce("jobs.nightly"),
  admin: { purge: function () { return api("/purge"); } },
  ready: warm(),
} satisfies Handler;
"#;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("input.ts");
        std::fs::write(&path, source).expect("write file");
        let cm: Lrc<SourceMap> = Default::default();
        let handler = swc_common::errors::Handler::with_tty_emitter(
            swc_common::errors::ColorConfig::Never,
            true,
            false,
            Some(cm.clone()),
        );
        let module = crate::parser::parse_file(&path, &cm, &handler).expect("parsed module");
        let keys: HashSet<String> = [
            "default.fetch",
            "default.queue",
            "default.admin.purge",
            "api",
            "announce",
            "warm",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let file = extract_file_ir(&module, &cm, &keys);
        for (key, callee) in [
            ("default.fetch", "api"),
            ("default.queue", "announce"),
            ("default.admin.purge", "api"),
        ] {
            let member = file
                .functions
                .get(key)
                .unwrap_or_else(|| panic!("{key} is a function: {:?}", file.functions.keys()));
            let callees: Vec<&str> = member.calls.iter().map(|c| c.callee.as_str()).collect();
            assert_eq!(callees, vec![callee], "{key}: {:#?}", member.calls);
        }
        let module_calls: Vec<&str> = file
            .module_level
            .calls
            .iter()
            .map(|call| call.callee.as_str())
            .collect();
        assert_eq!(module_calls, vec!["warm"], "{:#?}", file.module_level);
        assert!(
            file.module_level.nested.is_empty() && file.module_level.detached.is_empty(),
            "module scope read a member's body again: {:#?}",
            file.module_level
        );
    }

    #[test]
    fn a_type_wrapped_module_arrow_is_its_bindings_function_read_once() {
        let source = r#"
type Sender = () => Promise<Response>;
export const send = (async () => {
  return fetch("https://api.example.com/x", { method: "PUT" });
}) satisfies Sender;
"#;
        let file = file_ir_of(source);
        let send = file.functions.get("send").expect("send is a function");
        assert_eq!(send.calls.len(), 1, "{:#?}", send.calls);
        assert!(send.calls[0].request.is_some(), "{:#?}", send.calls);
        assert!(
            file.module_level.calls.is_empty()
                && file.module_level.nested.is_empty()
                && file.module_level.detached.is_empty(),
            "module scope read the body again: {:#?}",
            file.module_level
        );
    }

    /// carrick#2411: a spread of the function's own parameter holds the keys
    /// written before it; `fields` and `open` read as they always did.
    #[test]
    fn a_spread_of_its_own_parameter_holds_the_keys_before_it() {
        let body = request_body(SPREAD_WRAPPERS, "held");
        assert!(body.open);
        assert_eq!(
            body.fields,
            BTreeMap::from([("mode".to_string(), Value::Str(vec![lit("x")]))])
        );
        assert_eq!(
            body.under_param,
            Some((
                1,
                BTreeMap::from([(
                    "op".to_string(),
                    Value::Str(vec![Piece::Param(0, "op".to_string())])
                )])
            ))
        );
    }

    /// carrick#2411: a second parameter spread, a spread of a parameter's
    /// key, a parameter assigned again or written through, a computed key
    /// after the spread, and a spread into an object already open hold
    /// nothing.
    #[test]
    fn a_spread_the_hold_cannot_prove_holds_nothing() {
        for function in [
            "twice",
            "keyOf",
            "reassigned",
            "writtenThrough",
            "computed",
            "opened",
        ] {
            let body = request_body(SPREAD_WRAPPERS, function);
            assert_eq!(body.under_param, None, "{function}: {body:?}");
            assert!(body.open, "{function}");
            assert!(!body.fields.contains_key("op"), "{function}: {body:?}");
        }
    }

    /// carrick#2411: each argument as a spread of the callee's parameter
    /// reads it: a literal's keys, the caller's own parameter, and nothing
    /// for anything else or any position from a spread argument on.
    #[test]
    fn a_call_reads_each_argument_as_a_spread_puts_it_in_place() {
        let source = r#"
import { send } from "./send";
export async function caller(op: string, passed: object, flag: boolean, list: unknown[]) {
  const local = {};
  await send("a", {}, flag ? { id: "1" } : {}, passed, local, build());
  await send("a", ...list, {});
}
function build() { return {}; }
"#;
        let file = file_ir_of(source);
        let calls: Vec<&CallIr> = file.functions["caller"]
            .calls
            .iter()
            .filter(|call| call.callee == "send")
            .collect();
        let empty = ObjValue::default();
        let id = ObjValue {
            fields: BTreeMap::from([("id".to_string(), Value::Str(vec![lit("1")]))]),
            ..ObjValue::default()
        };
        assert_eq!(
            calls[0].spread_args,
            vec![
                SpreadArg::Unread,
                SpreadArg::Keys(vec![empty.clone()]),
                SpreadArg::Keys(vec![id, empty]),
                SpreadArg::Param(1),
                SpreadArg::Unread,
                SpreadArg::Unread,
            ]
        );
        assert_eq!(
            calls[1].spread_args,
            vec![SpreadArg::Unread, SpreadArg::Unread, SpreadArg::Unread]
        );
    }

    /// `{ op, ...args, mode: "x" }`, sent by a function whose parameters are
    /// `(op, args)`.
    fn held_effect() -> Effect {
        Effect {
            body: BTreeMap::from([("mode".to_string(), vec![lit("x")])]),
            body_under: Some((
                1,
                BTreeMap::from([("op".to_string(), vec![Piece::Param(0, "op".to_string())])]),
            )),
            ..effect(vec![lit("https://api.example.com/rpc")])
        }
    }

    fn body_of_keys(pairs: &[(&str, &str)]) -> BTreeMap<String, Vec<Piece>> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), vec![lit(value)]))
            .collect()
    }

    /// carrick#2411: a literal in place of the spread keeps the keys before
    /// it unless it sets them, and the keys after it go on top.
    #[test]
    fn a_literal_argument_is_read_in_place_of_the_spread() {
        let args = [
            Value::Str(vec![lit("purge")]),
            object(&[("id", vec![lit("7")])], false),
        ];
        let empty = ObjValue::default();
        let id = ObjValue {
            fields: BTreeMap::from([("id".to_string(), Value::Str(vec![lit("7")]))]),
            ..ObjValue::default()
        };
        let filled =
            held_effect().instantiate(&args, &[SpreadArg::Unread, SpreadArg::Keys(vec![id])]);
        assert_eq!(
            filled.body,
            body_of_keys(&[("op", "purge"), ("id", "7"), ("mode", "x")])
        );
        assert_eq!(filled.body_under, None);

        // The literal sets the held key: the runtime overwrite.
        let evict = ObjValue {
            fields: BTreeMap::from([
                ("op".to_string(), Value::Str(vec![lit("evict")])),
                ("mode".to_string(), Value::Str(vec![lit("ignored")])),
            ]),
            ..ObjValue::default()
        };
        let overwritten = held_effect().instantiate(
            &args,
            &[SpreadArg::Unread, SpreadArg::Keys(vec![evict, empty])],
        );
        assert_eq!(
            overwritten.body,
            body_of_keys(&[("mode", "x")]),
            "a key only one branch sets is unknown, and one written after the spread stands"
        );
        let evict = ObjValue {
            fields: BTreeMap::from([("op".to_string(), Value::Str(vec![lit("evict")]))]),
            ..ObjValue::default()
        };
        let overwritten =
            held_effect().instantiate(&args, &[SpreadArg::Unread, SpreadArg::Keys(vec![evict])]);
        assert_eq!(
            overwritten.body,
            body_of_keys(&[("op", "evict"), ("mode", "x")])
        );
    }

    /// carrick#2411: the caller's own parameter in place of the spread holds
    /// the filled keys one function up; anything else drops them.
    #[test]
    fn a_parameter_argument_holds_the_keys_and_anything_else_drops_them() {
        let args = [
            Value::Str(vec![lit("purge")]),
            Value::Str(vec![Piece::Param(2, "scope".to_string())]),
        ];
        let passed = held_effect().instantiate(&args, &[SpreadArg::Unread, SpreadArg::Param(2)]);
        assert_eq!(passed.body, body_of_keys(&[("mode", "x")]));
        assert_eq!(
            passed.body_under,
            Some((2, body_of_keys(&[("op", "purge")])))
        );
        assert!(!passed.has_params(), "a held body never moves a row");

        for spread_args in [
            vec![SpreadArg::Unread, SpreadArg::Unread],
            vec![SpreadArg::Unread],
            vec![],
        ] {
            let dropped = held_effect().instantiate(&args, &spread_args);
            assert_eq!(
                dropped.body,
                body_of_keys(&[("mode", "x")]),
                "{spread_args:?}"
            );
            assert_eq!(dropped.body_under, None, "{spread_args:?}");
        }
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

    /// carrick#1690 split a member read whose value is only tested out of
    /// the member read it was, and a binding that is a `&&` or `||` operand
    /// in a test position out of the operand it was: a message role reads
    /// each as reading the binding and keeping nothing of it, and an HTTP
    /// client and an object constant are contested by it exactly as before.
    /// A member read used as a value is a member read still.
    #[test]
    fn a_member_read_only_tested_still_contests_an_http_client() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("input.ts");
        std::fs::write(
            &path,
            "if (api.ready) {}\n\
             while (lib.a.b) {}\n\
             do {} while (bus?.open);\n\
             for (; queue[key].busy; ) {}\n\
             const picked = opts.mode ? 1 : 2;\n\
             const off = !this.client.connected;\n\
             if (sock.recovered || retrying) {}\n\
             if (!(left.x && (right as any).y)) {}\n\
             if (pair && flag) {}\n\
             const kept = held.value;\n\
             const either = other.value || fallback;\n\
             const maybe = nullish.value ?? fallback;\n\
             const chosen = flag ? branch.value : null;\n",
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
        for name in [
            "api",
            "lib",
            "bus",
            "queue",
            "opts",
            "this.client",
            "sock",
            "left",
            "right",
        ] {
            let used = &uses.uses[name];
            assert!(used.member_tested, "{name} has a member read only tested");
            assert!(!used.member_read, "{name} has no member read as a value");
            assert!(used.contests_client(), "{name} contests an HTTP client");
            assert!(
                !used.contests_message(),
                "{name} is no use to a message role"
            );
            assert!(
                used.keeps_object(),
                "{name} keeps an object constant's key set, as a member read did"
            );
        }
        assert!(uses.uses["retrying"].tested, "an operand of a tested `||`");
        assert!(
            uses.uses["key"].other,
            "a computed key in a tested chain is a value"
        );
        let pair = &uses.uses["pair"];
        assert!(pair.tested, "an operand of a tested `&&`");
        assert!(pair.contests_client(), "pair contests an HTTP client");
        assert!(!pair.contests_message(), "pair is no use to a message role");
        assert!(!pair.keeps_object(), "pair is no object constant's key set");
        for name in ["held", "other", "nullish", "branch"] {
            let used = &uses.uses[name];
            assert!(used.member_read, "{name} has a member read as a value");
            assert!(!used.member_tested, "{name} is not only tested");
            assert!(used.contests_message(), "{name} contests a message role");
        }
        assert!(
            uses.uses["fallback"].other,
            "an operand of a `||` or `??` used as a value is a value"
        );
        let mut merged = BindingUse::default();
        merged.merge(&uses.uses["api"]);
        assert!(merged.member_tested);
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

    /// carrick#3547 S2: the uses by name are every binding's uses of the
    /// name and the uses with no binding, together, so keeping the by-binding
    /// view beside them leaves the by-name view as it was. Two functions'
    /// bindings of one name stay apart.
    #[test]
    fn the_uses_by_name_are_every_binding_of_the_name_together() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("input.ts");
        std::fs::write(
            &path,
            "import { Queue } from \"@fixture/queue\";\n\
             export function ship() {\n\
             \x20 const parcels = new Queue(\"p\");\n\
             \x20 parcels.add(\"j\", {});\n\
             \x20 return parcels;\n\
             }\n\
             export function label(parcels: Queue) {\n\
             \x20 keep({ parcels });\n\
             \x20 parcels.tasks.obliterate();\n\
             \x20 if (parcels.paused) { (parcels as any).name = \"x\"; }\n\
             }\n\
             export class Holder {\n\
             \x20 parcels = 1;\n\
             \x20 hand() { return keep(this.parcels); }\n\
             }\n\
             export import inner = parcels.inner;\n\
             export { label };\n\
             declare function keep(value: unknown): void;\n",
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
        uses.settle_aliases(&HashSet::new());
        for (name, used) in &uses.uses {
            let mut together = uses.unbound.get(name).cloned().unwrap_or_default();
            for ((bound, _), each) in &uses.bindings {
                if bound == name {
                    together.merge(each);
                }
            }
            assert_eq!(format!("{together:?}"), format!("{used:?}"), "{name}");
        }
        let parcels = uses
            .bindings
            .keys()
            .filter(|(name, _)| name == "parcels")
            .count();
        assert_eq!(parcels, 2, "{:#?}", uses.bindings.keys());
        assert!(uses.unbound.contains_key("parcels"), "the alias root");
        assert!(uses.unbound.contains_key("this.parcels"));
    }

    /// The URL the first request `function` makes in `source` reads as,
    /// with its base joined (carrick#2406).
    fn request_url(source: &str, function: &str) -> Vec<Piece> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("input.ts");
        std::fs::write(&path, source).expect("write file");
        let cm: Lrc<SourceMap> = Default::default();
        let handler = swc_common::errors::Handler::with_tty_emitter(
            swc_common::errors::ColorConfig::Never,
            true,
            false,
            Some(cm.clone()),
        );
        let module = crate::parser::parse_file(&path, &cm, &handler).expect("parsed module");
        let file = extract_file_ir(&module, &cm, &HashSet::new());
        let Some(ir) = file.functions.get(function) else {
            let names: Vec<&String> = file.functions.keys().collect();
            panic!("no function {function} among {names:?}");
        };
        ir.calls
            .iter()
            .find_map(|call| call.request.as_ref())
            .map(|shape| join_base(&shape.base, &shape.url))
            .unwrap_or_else(|| panic!("{function} makes no request"))
    }

    /// carrick#3614 A4: a class field initialiser that is not a function is a
    /// body the readers read, under a key no callee can name; an arrow-valued
    /// field keeps its own key.
    #[test]
    fn a_class_field_initialiser_that_is_a_call_is_a_body() {
        let source = "function watch<T>(f: () => T) { return f; }\n\
                      export class Store {\n\
                      \x20 refresh = watch(() => fetch(\"/api/items\"));\n\
                      \x20 static seed = watch(() => fetch(\"/api/seed\"));\n\
                      \x20 arrow = () => fetch(\"/api/arrow\");\n\
                      }\n";
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("input.ts");
        std::fs::write(&path, source).expect("write file");
        let cm: Lrc<SourceMap> = Default::default();
        let handler = swc_common::errors::Handler::with_tty_emitter(
            swc_common::errors::ColorConfig::Never,
            true,
            false,
            Some(cm.clone()),
        );
        let module = crate::parser::parse_file(&path, &cm, &handler).expect("parsed module");
        let file = extract_file_ir(&module, &cm, &HashSet::new());
        let requests = |key: &str| {
            let ir = file
                .functions
                .get(key)
                .unwrap_or_else(|| panic!("no {key} among {:?}", file.functions.keys()));
            // The callback the call is handed is a nested function.
            fn count(ir: &FnIr) -> usize {
                ir.calls
                    .iter()
                    .filter(|call| call.request.is_some())
                    .count()
                    + ir.nested
                        .iter()
                        .chain(&ir.detached)
                        .map(count)
                        .sum::<usize>()
            }
            count(ir)
        };
        assert_eq!(requests(&initialiser_key("Store", "refresh", false)), 1);
        assert_eq!(requests(&initialiser_key("Store", "seed", true)), 1);
        assert_eq!(requests("Store.arrow"), 1);
        assert!(!file.functions.contains_key("Store.refresh"));
        assert!(!file.functions.contains_key("Store.seed"));
    }

    /// carrick#2447: a builder with locals before its `return`, a `throw`
    /// path, or several returns that write one expression is the value it
    /// returns; a local a block declares again keeps its own value.
    #[test]
    fn a_builder_with_locals_and_one_returned_value_composes() {
        for body in [
            "const root = \"/api\"; return root + \"/v1\";",
            "const root = \"/api\"; if (!root) { throw new Error(\"x\"); } return root + \"/v1\";",
            "if (flag()) { return \"/api\" + \"/v1\"; } else { const v = \"/v1\"; return \"/api\" + v; }",
        ] {
            let url = request_url(
                &format!(
                    "function base() {{ {body} }}\n\
                     export function f() {{ return fetch(base() + \"/x\"); }}\n"
                ),
                "f",
            );
            assert_eq!(url, vec![lit("/api/v1/x")], "{body}");
        }
        let shadowed = request_url(
            "function base() { const v = \"/outer\"; { const v = \"/inner\"; } return v; }\n\
             export function f() { return fetch(base() + \"/x\"); }\n",
            "f",
        );
        assert_eq!(shadowed, vec![lit("/outer/x")]);
    }

    /// carrick#2447: a `const` an `if` branch declares is visible only in
    /// that branch, so the `return` after it reads the outer binding.
    #[test]
    fn a_branch_scoped_constant_is_not_read_after_its_branch() {
        let url = request_url(
            "function base() { const p = \"/x\"; if (flag()) { const p = \"/y\"; throw new Error(p); } return p; }\n\
             export function f() { return fetch(base() + \"/r\"); }\n",
            "f",
        );
        assert_eq!(url, vec![lit("/x/r")]);
        // The scope holds without the resolver's contexts too: unresolved,
        // both `p` share one key, and only the block scoping keeps them apart.
        let (_, module) = crate::swc_scanner::parse_standalone_module(
            Path::new("input.ts"),
            "function base() { const p = \"/x\"; if (flag()) { const p = \"/y\"; throw new Error(p); } return p; }\n",
        )
        .expect("parsed module");
        let Some(ModuleItem::Stmt(Stmt::Decl(Decl::Fn(base)))) = module.body.first() else {
            panic!("the module declares base first");
        };
        let returned = returned_values(&base.function.body.as_ref().expect("body").stmts);
        assert!(
            matches!(returned.as_deref(), Some([Expr::Lit(Lit::Str(text))]) if text.value.to_string_lossy() == "/x"),
            "{returned:?}"
        );
    }

    /// carrick#2447: a body that returns different values, can fall off its
    /// end, or does anything but declare constants states no value.
    #[test]
    fn a_builder_that_states_no_one_value_stays_a_hole() {
        for body in [
            "if (flag()) { return \"/a\"; } return \"/b\";",
            "if (flag()) { return \"/a\"; }",
            "let v = \"/a\"; return v;",
            "for (;;) { } return \"/a\";",
            "const v = \"/a\"; sideEffect(); return v;",
        ] {
            let url = request_url(
                &format!(
                    "function base() {{ {body} }}\n\
                     export function f() {{ return fetch(base() + \"/x\"); }}\n"
                ),
                "f",
            );
            assert!(
                matches!(url.first(), Some(Piece::Opaque { text, .. }) if text == "base()"),
                "{body}: {url:?}"
            );
        }
    }

    /// A call statement on a body-local `const` (`query.set(k, v)`, guarded
    /// or not) does not stop a builder when the return reads that local only
    /// after the `?`: the route is the text before it.
    #[test]
    fn a_query_local_changed_before_the_return_keeps_the_builder() {
        for body in [
            "const query = new URLSearchParams(); if (flag()) { query.set(\"k\", \"v\"); } return `/a/${id}/b?${query}`;",
            "const query = new URLSearchParams(); query.set(\"k\", \"v\"); return `/a/${id}/b?${query.toString()}`;",
            "const query = new URLSearchParams(); query.set(\"k\", \"v\"); return \"/a/\" + id + \"/b?\" + query;",
        ] {
            let url = request_url(
                &format!(
                    "function url(id: string) {{ {body} }}\n\
                     export function f(id: string) {{ return fetch(url(id)); }}\n"
                ),
                "f",
            );
            assert!(
                matches!(url.first(), Some(Piece::Lit(text)) if text.starts_with("/a/")),
                "{body}: {url:?}"
            );
        }
    }

    /// A call statement the path reads from, or on anything that is not a
    /// body-local `const`, still states no value.
    #[test]
    fn a_statement_the_path_reads_still_stops_the_builder() {
        for body in [
            "const parts = [\"a\"]; parts.push(id); return `/${parts.join(\"/\")}`;",
            "const query = new URLSearchParams(); query.set(\"k\", \"v\"); return `/a/${query}/b`;",
            "const p = `/a/${id}`; console.log(p); return p;",
            "const query = new URLSearchParams(); query.set(\"k\", \"v\"); const p = `/a/${query}`; return p;",
        ] {
            let url = request_url(
                &format!(
                    "function url(id: string) {{ {body} }}\n\
                     export function f(id: string) {{ return fetch(url(id)); }}\n"
                ),
                "f",
            );
            assert!(
                matches!(url.first(), Some(Piece::Opaque { text, .. }) if text.starts_with("url(")),
                "{body}: {url:?}"
            );
        }
    }

    /// carrick#2536: a module constant set to a call of a function the
    /// module declares is what the function returns. A declaration is
    /// hoisted, so a constant written above it reads it too.
    #[test]
    fn a_constant_set_by_a_declared_function_reads_what_it_returns() {
        for (source, target) in [
            (
                "function base() { return \"/api/v1\"; }\n\
                 const API = base();\n\
                 export function f() { return fetch(API + \"/x\"); }\n",
                "/api/v1/x",
            ),
            (
                "const API = base();\n\
                 function base() { return process.env.ORIGIN + \"/api/v1\"; }\n\
                 export function f() { return fetch(API + \"/x\"); }\n",
                "${process.env.ORIGIN}/api/v1/x",
            ),
        ] {
            let url = request_url(source, "f");
            assert_eq!(render_target(&url).as_deref(), Some(target), "{source}");
        }
    }

    /// carrick#2536: returns that differ only in a value the source does not
    /// write, before one literal path every return ends in, read as a hole
    /// for that value with the path after it. The hole is the call, as it
    /// was before; only the path is added.
    #[test]
    fn returns_that_end_in_one_written_path_keep_it_after_a_hole() {
        for source in [
            "function base() { const m = readMount(); if (m) { return m + \"/svc/v2\"; } return \"/svc/v2\"; }\n\
             export function f() { return fetch(base() + \"/x\"); }\n",
            "function base() { if (flag()) { return process.env.ROOT + \"/svc/v2\"; } return `${origin()}/svc/v2`; }\n\
             export function f() { return fetch(base() + \"/x\"); }\n",
            "const base = () => { if (flag()) { return readMount() + \"/svc/v2\"; } return \"/svc/v2\"; };\n\
             export function f() { return fetch(base() + \"/x\"); }\n",
            "function base() { if (flag()) { return readMount() + \"/svc/v2\"; } return \"/svc/v2\"; }\n\
             const ROOT = base();\n\
             export function f() { return fetch(ROOT + \"/x\"); }\n",
        ] {
            let url = request_url(source, "f");
            assert_eq!(
                first_end(&url),
                (
                    &Root::Hole(HoleReason::UnreadExpression),
                    Some("${base()}/svc/v2/x".to_string())
                ),
                "{source}"
            );
        }
    }

    /// carrick#2536, must not verify: no path is claimed that every return
    /// does not write whole after only values the source does not write.
    /// Returns ending in different text (even with a common tail, `/api`),
    /// a varying part the source writes (`/admin`, `/t/`), a path with no
    /// leading slash or no segment, text the call hands the function, and a
    /// parameter a caller may fill with text all leave the call the one hole
    /// it was before.
    #[test]
    fn returns_that_do_not_end_in_one_written_path_stay_a_hole() {
        for (declaration, params, call) in [
            (
                "function base() { if (flag()) { return \"/api/v1\"; } return \"/api/v2\"; }",
                "",
                "base()",
            ),
            (
                "function base() { if (flag()) { return \"/v1/api\"; } return \"/v2/api\"; }",
                "",
                "base()",
            ),
            (
                "function base() { if (flag()) { return readMount() + \"/admin/api\"; } return \"/api\"; }",
                "",
                "base()",
            ),
            (
                "function base() { if (flag()) { return `/t/${tenant()}/api`; } return \"/api\"; }",
                "",
                "base()",
            ),
            (
                "function base() { if (flag()) { return readMount() + \"api/v1\"; } return \"api/v1\"; }",
                "",
                "base()",
            ),
            (
                "function base() { if (flag()) { return readMount() + \"/\"; } return \"/\"; }",
                "",
                "base()",
            ),
            (
                "function base(p: string) { if (p) { return p + \"/api\"; } return \"/api\"; }",
                "",
                "base(\"/admin\")",
            ),
            (
                "function base(p: string) { if (p) { return p + \"/api\"; } return \"/api\"; }",
                "prefix: string",
                "base(prefix)",
            ),
        ] {
            let url = request_url(
                &format!(
                    "{declaration}\nexport function f({params}) {{ return fetch({call} + \"/x\"); }}\n"
                ),
                "f",
            );
            assert!(
                matches!(url.as_slice(), [Piece::Opaque { text, .. }, Piece::Lit(rest)] if text == call && rest == "/x"),
                "{declaration}: {url:?}"
            );
        }
    }

    fn several_paths(url: &[Piece]) -> bool {
        url.iter().any(|piece| {
            matches!(
                piece,
                Piece::Opaque {
                    end: Root::Hole(HoleReason::SeveralPaths),
                    ..
                }
            )
        })
    }

    /// carrick#2541: a value that reads as more than one written path (a
    /// builder's returns, a conditional's branches) is no one path, wherever
    /// it is held: inline, in a module constant, in a local, or in a local
    /// built from it with no text of its own. The URL states no target.
    #[test]
    fn a_value_that_reads_as_several_paths_states_no_target() {
        let two = "function base() { if (flag()) { return \"/svc/v1\"; } return \"/svc/v2\"; }\n";
        for source in [
            format!("{two}export function f() {{ return fetch(base() + \"/x\"); }}\n"),
            format!("{two}const ROOT = base();\nexport function f() {{ return fetch(ROOT + \"/x\"); }}\n"),
            format!("{two}export function f() {{ const root = base(); return fetch(root + \"/x\"); }}\n"),
            format!("{two}export function f() {{ const u = `${{base()}}${{tail()}}`; return fetch(u + \"/x\"); }}\n"),
            "function base() { if (flag()) { return readMount(); } return \"/api\"; }\nexport function f() { return fetch(base() + \"/x\"); }\n".to_string(),
            "function base() { if (flag()) { return \"https://a.example/v1\"; } return \"/v1\"; }\nexport function f() { return fetch(base() + \"/x\"); }\n".to_string(),
            "const ROOT = flag() ? \"/svc/v1\" : \"/svc/v2\";\nexport function f() { return fetch(ROOT + \"/x\"); }\n".to_string(),
            "const ROOT = flag() ? readMount() : \"/api\";\nexport function f() { return fetch(ROOT + \"/x\"); }\n".to_string(),
            "export function f(local: boolean) { const base = local ? readMount() : \"/api\"; return fetch(`${base}/x`); }\n".to_string(),
            format!("{two}const ROOT = flag() ? base() : readMount();\nexport function f() {{ return fetch(ROOT + \"/x\"); }}\n"),
        ] {
            let url = request_url(&source, "f");
            assert!(several_paths(&url), "{source}: {url:?}");
            assert_eq!(render_target(&url), None, "{source}: {url:?}");
        }
    }

    /// carrick#2541: a conditional reads as a builder's returns do
    /// (carrick#2536): branches that are one value are that value; branches
    /// that end in one written path after values the source does not write
    /// keep it after a hole.
    #[test]
    fn a_conditional_reads_as_a_builder_does() {
        for (source, target) in [
            (
                "const ROOT = flag() ? \"/svc/v2\" : \"/svc/v2\";\nexport function f() { return fetch(ROOT + \"/x\"); }\n",
                "/svc/v2/x",
            ),
            (
                "const ROOT = flag() ? readMount() + \"/svc/v2\" : \"/svc/v2\";\nexport function f() { return fetch(ROOT + \"/x\"); }\n",
                "${flag() ? readMount() + \"/svc/v2\" : \"/svc/v2\"}/svc/v2/x",
            ),
        ] {
            assert_eq!(
                render_target(&request_url(source, "f")).as_deref(),
                Some(target),
                "{source}"
            );
        }
    }

    /// carrick#2541, must not verify: readings that write no path (origins,
    /// one segment, values nothing here reads, a bare `/`) are the hole they
    /// were, and the URL renders as before.
    #[test]
    fn a_value_that_writes_no_path_reads_as_before() {
        for (source, target) in [
            (
                "function base() { if (flag()) { return \"https://a.example\"; } return \"http://localhost:3000\"; }\nexport function f() { return fetch(base() + \"/x\"); }\n",
                "${base()}/x",
            ),
            (
                "const BASE = flag() ? \"http://localhost:3000\" : process.env.API_URL;\nexport function f() { return fetch(BASE + \"/x\"); }\n",
                "${flag() ? \"http://localhost:3000\" : process.env.API_URL}/x",
            ),
            (
                "function base() { if (flag()) { return readA(); } return readB(); }\nexport function f() { return fetch(base() + \"/x\"); }\n",
                "${base()}/x",
            ),
            (
                "function kind() { if (flag()) { return \"a\"; } return \"b\"; }\nexport function f() { return fetch(\"/items/\" + kind() + \"/x\"); }\n",
                "/items/${kind()}/x",
            ),
            (
                "function base() { if (flag()) { return readMount() + \"/\"; } return \"/\"; }\nexport function f() { return fetch(base() + \"/x\"); }\n",
                "${base()}/x",
            ),
        ] {
            let url = request_url(source, "f");
            assert!(!several_paths(&url), "{source}: {url:?}");
            assert_eq!(render_target(&url).as_deref(), Some(target), "{source}");
        }
    }

    /// The end of the URL's first piece, which must be opaque, and the
    /// target the URL renders.
    fn first_end(url: &[Piece]) -> (&Root, Option<String>) {
        let Some(Piece::Opaque { end, .. }) = url.first() else {
            panic!("the URL leads with an opaque piece: {url:?}");
        };
        (end, render_target(url))
    }

    fn env(written: &str, name: &str, fallback: Option<&str>) -> Root {
        Root::Env {
            written: written.to_string(),
            name: name.to_string(),
            fallback: fallback.map(str::to_string),
        }
    }

    /// carrick#2406: an environment read ends at the variable it names, in
    /// each runtime's spelling, with the `??` literal beside it kept on the
    /// end and left out of the target.
    #[test]
    fn an_environment_read_ends_at_its_variable_with_its_fallback() {
        let url = request_url(
            "export function list() {\n\
             \x20 return fetch(`${process.env.API ?? \"http://localhost:4000\"}/users`);\n\
             }\n",
            "list",
        );
        assert_eq!(
            first_end(&url),
            (
                &env("process.env.API", "API", Some("http://localhost:4000")),
                Some("${process.env.API}/users".to_string())
            )
        );
        for (read, name) in [
            ("import.meta.env.VITE_API", "VITE_API"),
            ("process.env[\"API\"]", "API"),
            ("Deno.env.get(\"API\")", "API"),
        ] {
            let source =
                format!("export function list() {{ return fetch(`${{{read}}}/users`); }}\n");
            let url = request_url(&source, "list");
            assert_eq!(
                first_end(&url),
                (&env(read, name, None), Some(format!("${{{read}}}/users")))
            );
        }
    }

    /// carrick#2406: a name the file neither declares nor imports ends as a
    /// global, an import binding at the import, and a call result where
    /// reading stopped.
    #[test]
    fn a_global_an_import_and_a_call_result_end_where_the_receive_side_says() {
        let source = "import { apiBase } from \"./config\";\n\
                      import * as settings from \"./settings\";\n\
                      export function global() { return fetch(`${API_BASE}/users`); }\n\
                      export function imported() { return fetch(`${apiBase}/users`); }\n\
                      export function member() { return fetch(`${settings.base}/users`); }\n\
                      export function called() { return fetch(`${baseUrl()}/users`); }\n";
        assert_eq!(
            first_end(&request_url(source, "global")),
            (
                &Root::Global("API_BASE".to_string()),
                Some("${API_BASE}/users".to_string())
            )
        );
        assert_eq!(
            first_end(&request_url(source, "imported")),
            (
                &Root::Import {
                    local: "apiBase".to_string(),
                    specifier: "./config".to_string(),
                    export: "apiBase".to_string(),
                },
                Some("${apiBase}/users".to_string())
            )
        );
        assert_eq!(
            first_end(&request_url(source, "member")),
            (
                &Root::Import {
                    local: "settings".to_string(),
                    specifier: "./settings".to_string(),
                    export: "*".to_string(),
                },
                Some("${settings.base}/users".to_string())
            )
        );
        assert_eq!(
            first_end(&request_url(source, "called")),
            (
                &Root::Hole(HoleReason::UnreadExpression),
                Some("${baseUrl()}/users".to_string())
            )
        );
    }

    /// carrick#2406: a field the class writes more than once ends as
    /// reassigned, one the field table holds nothing for where reading
    /// stopped, and a binding set again as reassigned. Each keeps its text.
    #[test]
    fn a_contested_field_an_unread_field_and_an_unsettled_binding_end_at_holes() {
        let source = "export class Client {\n\
                      \x20 constructor(base: string) { this.base = base; }\n\
                      \x20 rebase(next: string) { this.base = next; }\n\
                      \x20 list() { return fetch(`${this.base}/users`); }\n\
                      \x20 other() { return fetch(`${this.origin}/users`); }\n\
                      }\n\
                      export function moved(flag: boolean) {\n\
                      \x20 let base = \"http://a\";\n\
                      \x20 if (flag) { base = \"http://b\"; }\n\
                      \x20 return fetch(`${base}/users`);\n\
                      }\n";
        assert_eq!(
            first_end(&request_url(source, "Client.list")),
            (
                &Root::Hole(HoleReason::Reassigned),
                Some("${this.base}/users".to_string())
            )
        );
        assert_eq!(
            first_end(&request_url(source, "Client.other")),
            (
                &Root::Hole(HoleReason::UnreadExpression),
                Some("${this.origin}/users".to_string())
            )
        );
        assert_eq!(
            first_end(&request_url(source, "moved")),
            (
                &Root::Hole(HoleReason::Reassigned),
                Some("${base}/users".to_string())
            )
        );
    }

    /// carrick#2452: a class only this file constructs reads each constructor
    /// parameter as the value its one construction passes.
    #[test]
    fn a_constructor_parameter_reads_as_the_value_the_construction_passes() {
        let property = "class Api {\n\
                        \x20 constructor(private base: string) {}\n\
                        \x20 users() { return fetch(`${this.base}/users`); }\n\
                        }\n\
                        export const api = new Api(\"/api/v1\");\n";
        assert_eq!(
            render_target(&request_url(property, "Api.users")).as_deref(),
            Some("/api/v1/users")
        );

        let assigned = "class Items {\n\
                        \x20 private base: string;\n\
                        \x20 constructor(base: string) { this.base = base; }\n\
                        \x20 list() { return fetch(this.base + \"/items\"); }\n\
                        }\n\
                        export const items = new Items(process.env.ITEMS_URL!);\n";
        let url = request_url(assigned, "Items.list");
        assert_eq!(
            first_end(&url).0,
            &env("process.env.ITEMS_URL", "ITEMS_URL", None)
        );

        let options = "class Api {\n\
                       \x20 private base: string;\n\
                       \x20 constructor(opts: { baseUrl: string }) { this.base = opts.baseUrl; }\n\
                       \x20 x() { return fetch(`${this.base}/x`); }\n\
                       }\n\
                       const api = new Api({ baseUrl: \"/v3\" });\n";
        assert_eq!(
            render_target(&request_url(options, "Api.x")).as_deref(),
            Some("/v3/x")
        );

        let destructured = "class Api {\n\
                            \x20 private base: string;\n\
                            \x20 constructor({ baseUrl }: { baseUrl: string }) { this.base = baseUrl; }\n\
                            \x20 x() { return fetch(`${this.base}/x`); }\n\
                            }\n\
                            export default new Api({ baseUrl: \"/v4\" });\n";
        assert_eq!(
            render_target(&request_url(destructured, "Api.x")).as_deref(),
            Some("/v4/x")
        );
    }

    /// oss1109 T1: a binding holding a value read off a construction is
    /// written by its own name, as any other opaque value is; a binding (or
    /// a field) holding the construction itself keeps its text.
    #[test]
    fn a_binding_read_off_a_construction_is_written_by_its_name() {
        let read_off = "export function load(next: string) {\n\
                        \x20 const id = new URL(next, location.origin).searchParams.get(\"k\");\n\
                        \x20 return fetch(`${api}/sessions/${id}`);\n\
                        }\n";
        assert_eq!(
            render_target(&request_url(read_off, "load")).as_deref(),
            Some("${api}/sessions/${id}")
        );

        let bare = "export function load() {\n\
                    \x20 const stamp = new Stamp();\n\
                    \x20 return fetch(`${api}/stamps/${stamp}`);\n\
                    }\n";
        assert_eq!(
            render_target(&request_url(bare, "load")).as_deref(),
            Some("${api}/stamps/${new Stamp()}")
        );

        let field = "class Api {\n\
                     \x20 stamp = new Stamp();\n\
                     \x20 read = new Stamp().value();\n\
                     \x20 a() { return fetch(`${api}/a/${this.stamp}`); }\n\
                     \x20 b() { return fetch(`${api}/b/${this.read}`); }\n\
                     }\n";
        assert_eq!(
            render_target(&request_url(field, "Api.a")).as_deref(),
            Some("${api}/a/${new Stamp()}")
        );
        assert_eq!(
            render_target(&request_url(field, "Api.b")).as_deref(),
            Some("${api}/b/${this.read}")
        );
    }

    /// carrick#2452: a construction this pass cannot account for leaves the
    /// parameter as it was read before, the field's own name.
    #[test]
    fn a_class_another_construction_may_reach_keeps_its_field_opaque() {
        let body = "  constructor(private base: string) {}\n\
                    \x20 users() { return fetch(`${this.base}/users`); }\n\
                    }\n";
        let kept = Some("${this.base}/users");
        let cases = [
            // The constructions disagree.
            format!("class Api {{\n{body}const a = new Api(\"/a\");\nconst b = new Api(\"/b\");\n"),
            // Published: another module may construct it.
            format!("export class Api {{\n{body}const a = new Api(\"/a\");\n"),
            format!("class Api {{\n{body}const a = new Api(\"/a\");\nexport {{ Api }};\n"),
            // Constructed inside a function.
            format!("class Api {{\n{body}export function make() {{ return new Api(\"/a\"); }}\n"),
            // Handed to a call, or extended.
            format!("class Api {{\n{body}const a = new Api(\"/a\");\nregister(Api);\n"),
            format!("class Api {{\n{body}const a = new Api(\"/a\");\nclass Sub extends Api {{}}\n"),
            // A spread argument, or a construction leaving the position out.
            format!("class Api {{\n{body}const a = new Api(...args);\n"),
            format!("class Api {{\n{body}const a = new Api();\n"),
        ];
        for source in cases {
            assert_eq!(
                render_target(&request_url(&source, "Api.users")).as_deref(),
                kept,
                "{source}"
            );
        }
    }

    /// carrick#2406: a key of a parameter under `??` is the caller's to
    /// leave out, so it ends as a missing argument.
    #[test]
    fn a_parameter_key_under_a_fallback_ends_as_a_missing_argument() {
        let url = request_url(
            "export function list(opts: { base?: string }) {\n\
             \x20 return fetch(`${opts.base ?? \"http://localhost\"}/users`);\n\
             }\n",
            "list",
        );
        assert_eq!(
            first_end(&url),
            (
                &Root::Hole(HoleReason::CallerArgMissing),
                Some("${opts.base}/users".to_string())
            )
        );
    }

    /// carrick#2406: a key or an enclosing parameter no caller filled ends
    /// as a missing argument; a path parameter renamed at a call carries
    /// the end of what the caller wrote there.
    #[test]
    fn renamed_pieces_carry_an_end_by_rule() {
        let missing = Root::Hole(HoleReason::CallerArgMissing);
        let mut keyed_effect = effect(vec![keyed(0, "base"), lit("/users")]);
        keyed_effect.settle_keys();
        assert!(
            matches!(keyed_effect.url.first(), Some(Piece::Opaque { end, .. }) if *end == missing)
        );
        let mut outer = effect(vec![
            Piece::Param(0, "base".to_string()).enclosed(),
            lit("/x"),
        ]);
        outer.settle_outer();
        assert!(matches!(outer.url.first(), Some(Piece::Opaque { end, .. }) if *end == missing));

        let segment = effect(vec![lit("/users/"), Piece::Param(0, "id".to_string())]);
        let imported = Root::Import {
            local: "userId".to_string(),
            specifier: "./ids".to_string(),
            export: "userId".to_string(),
        };
        let cases = [
            (vec![lit("42")], Root::Literal("42".to_string())),
            (
                vec![Piece::Opaque {
                    text: "userId".to_string(),
                    end: imported.clone(),
                    link: None,
                }],
                imported,
            ),
            (
                vec![Piece::Param(1, "who".to_string())],
                Root::Param(1, "who".to_string()),
            ),
            (
                vec![lit("a"), opaque("b")],
                Root::Hole(HoleReason::UnreadExpression),
            ),
        ];
        for (arg, end) in cases {
            let filled = segment.instantiate(&[Value::Str(arg)], &[]);
            assert_eq!(
                filled.url.last(),
                Some(&Piece::Opaque {
                    text: "id".to_string(),
                    end,
                    link: None,
                })
            );
            assert_eq!(render_target(&filled.url).as_deref(), Some("/users/${id}"));
        }
    }

    /// carrick#2406: an environment read is decided by its spelling alone,
    /// so a file that imports its own `process` still reads
    /// `process.env.API` as the variable. The receive side reads it the same
    /// way now (`guards`'s `an_environment_read_on_the_receive_side_names_
    /// its_variable`); it used to require `process` to be the global.
    #[test]
    fn the_send_side_reads_an_environment_spelling_without_a_shadow_check() {
        let url = request_url(
            "import process from \"./fake-process\";\n\
             export function list() { return fetch(`${process.env.API}/users`); }\n",
            "list",
        );
        assert_eq!(
            first_end(&url),
            (
                &env("process.env.API", "API", None),
                Some("${process.env.API}/users".to_string())
            )
        );
    }

    /// carrick#2406: two rows that state the same thing at a site merge into
    /// one. Where their bases end apart (an environment read and a
    /// parameter, or two variables), the merged row's base is no one of them:
    /// it ends at a hole, which states it injected, not as the end that
    /// sorts first.
    #[test]
    fn merged_rows_whose_base_ends_disagree_keep_no_one_end() {
        let row = |end: Root| SummaryRow {
            callee: "list".to_string(),
            span_start: 10,
            span_end: 20,
            line: 2,
            method: "GET".to_string(),
            target: "${base}/users".to_string(),
            body_literals: BTreeMap::new(),
            reaches_request: None,
            own_site: false,
            at_caller: false,
            returns_request_value: false,
            call_body: None,
            library_semantics: Vec::new(),
            base_fallbacks: None,
            request_at: (PathBuf::from("src/client.ts"), 40),
            base_end: Some(end),
        };
        let a = env("process.env.A", "A", None);
        let b = env("process.env.B", "B", None);
        let param = Root::Param(0, "base".to_string());
        let hole = Some(Root::Hole(HoleReason::UnreadExpression));
        for ends in [
            vec![a.clone(), param.clone()],
            vec![a.clone(), b.clone()],
            vec![param.clone(), a.clone(), a.clone()],
        ] {
            let mut rows: Vec<SummaryRow> = ends.into_iter().map(row).collect();
            merge_rows(&mut rows);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].base_end, hole);
        }
        let mut agreeing = vec![row(a.clone()), row(a.clone())];
        merge_rows(&mut agreeing);
        assert_eq!(agreeing.len(), 1);
        assert_eq!(agreeing[0].base_end, Some(a));
    }

    /// carrick#2406: where the AST recogniser and the text test it replaced
    /// disagree. A string index that is no identifier was no environment
    /// read to the text test and is one now; a wrapped read was one to both,
    /// as the piece's text is the read inside the wrapper.
    #[test]
    fn the_environment_recogniser_reads_the_ast_not_the_text() {
        let base_in_another_file = |base: Vec<Piece>| {
            let mut effect = effect(vec![lit("/users")]);
            effect.base = base;
            effect.base_scope = Some(PathBuf::from("src/client.ts"));
            base_reads_the_same_in(&effect, Path::new("src/use.ts"))
        };
        for (source, read, name) in [
            // Old answer: not an environment read (the text test refused
            // `MY-VAR`).
            (
                "process.env[\"MY-VAR\"]",
                "process.env[\"MY-VAR\"]",
                "MY-VAR",
            ),
            // Old answer: an environment read, by the text inside the cast.
            ("(process.env.API as string)", "process.env.API", "API"),
            ("process.env.API!", "process.env.API", "API"),
        ] {
            let source =
                format!("export function list() {{ return fetch(`${{{source}}}/users`); }}\n");
            let url = request_url(&source, "list");
            let (end, _) = first_end(&url);
            assert_eq!(end, &env(read, name, None), "{source}");
            assert!(base_in_another_file(url[..1].to_vec()), "{source}");
        }
        // A binding renamed from a read is written by its own name, which
        // another file may bind otherwise: its base is not stated there.
        let renamed = Piece::Opaque {
            text: "API".to_string(),
            end: env("process.env[\"MY-VAR\"]", "MY-VAR", None),
            link: None,
        };
        assert!(!base_in_another_file(vec![renamed]));
    }

    /// carrick#2406 (row X1): what the deleted text test (`reads_the_environment`)
    /// answered on its own cases. A base written as an environment read, in
    /// each runtime's spelling, reads the same in every file, so a client
    /// declared in one module states its row at a call in another. A base
    /// that is a field, a call result or an import does not.
    #[test]
    fn an_environment_read_base_reads_the_same_in_another_file() {
        let in_another_file = |base: &str| {
            let source =
                format!("export function list() {{ return fetch(`${{{base}}}/users`); }}\n");
            let url = request_url(&source, "list");
            let mut effect = effect(vec![lit("/users")]);
            effect.base = url[..1].to_vec();
            effect.base_scope = Some(PathBuf::from("src/client.ts"));
            (
                base_reads_the_same_in(&effect, Path::new("src/use.ts")),
                base_reads_the_same_in(&effect, Path::new("src/client.ts")),
            )
        };
        for base in [
            "process.env.API",
            "process.env[\"API\"]",
            "import.meta.env.VITE_API",
            "Deno.env.get(\"API\")",
        ] {
            assert_eq!(in_another_file(base), (true, true), "{base}");
        }
        for base in ["config.apiUrl", "readBase()", "API_BASE"] {
            assert_eq!(in_another_file(base), (false, true), "{base}");
        }
    }

    /// The summaries of a service written from `files` (path, source).
    fn summarized(files: &[(&str, &str)]) -> (tempfile::TempDir, RequestSummaryIndex) {
        let dir = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(dir.path().join("package.json"), "{\"name\":\"held\"}")
            .expect("the manifest is writable");
        for (path, source) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("the tree");
            std::fs::write(path, source).expect("the source is writable");
        }
        let inputs = crate::engine::discover_request_inputs(dir.path());
        let index = summarize(&inputs, &LibrarySemantics::default());
        (dir, index)
    }

    /// The key the summaries hold `file` (repo-relative) under.
    fn key_of(index: &RequestSummaryIndex, file: &str) -> PathBuf {
        index
            .ledger
            .seen
            .iter()
            .chain(index.ledger.fact_calls.keys())
            .map(|(path, _)| path.clone())
            .chain(index.ledger.held.keys().map(|(path, _)| path.clone()))
            .find(|path| path.ends_with(file))
            .unwrap_or_else(|| panic!("{file} is read"))
    }

    const NOTIFY: &str = "export function notify(endpoint: string) {\n\
                          \x20 return fetch(`/v1${endpoint}`, { method: 'POST' });\n\
                          }\n";

    /// carrick#4121: a wrapper called inside a function and directly where a
    /// module loads is held at the module-level call, by the request's span,
    /// and not answered at its callers: the fold decides once it knows
    /// whether the call's fact row stands.
    #[test]
    fn a_request_a_module_level_call_reaches_is_held_at_that_call() {
        let (_dir, index) = summarized(&[
            ("src/net/notify.ts", NOTIFY),
            (
                "src/alerts.ts",
                "import { notify } from './net/notify';\n\
                 export const fire = () => notify('/fire');\n\
                 export const eager = notify('/eager');\n",
            ),
        ]);
        let notify = key_of(&index, "src/net/notify.ts");
        assert_eq!(index.answered_at_callers(&notify), BTreeSet::new());
        let held = index.held_at_fact_calls(&notify);
        assert_eq!(held.len(), 1, "{held:?}");
        let (_, at) = held.iter().next().unwrap();
        assert_eq!(at.line, 2);
        let calls: Vec<(String, u32)> = at
            .fact_calls
            .iter()
            .map(|(file, line, _)| (file.to_string_lossy().into_owned(), *line))
            .collect();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(
            calls[0].0.ends_with("src/alerts.ts") && calls[0].1 == 3,
            "{calls:?}"
        );
    }

    /// carrick#4127: a call written inside a callback has a call site of its
    /// own, which the fact layer may state: the request is held at it, with
    /// the module-level call, and not answered.
    #[test]
    fn a_module_level_call_inside_a_callback_is_held_at_that_call() {
        let (_dir, index) = summarized(&[
            ("src/net/notify.ts", NOTIFY),
            (
                "src/alerts.ts",
                "import { notify } from './net/notify';\n\
                 export const fire = () => notify('/fire');\n\
                 export const eager = notify('/eager');\n\
                 setTimeout(() => notify('/tick'));\n",
            ),
        ]);
        let notify = key_of(&index, "src/net/notify.ts");
        let held = index.held_at_fact_calls(&notify);
        let (_, at) = held.iter().next().unwrap();
        let lines: Vec<u32> = at.fact_calls.iter().map(|(_, line, _)| *line).collect();
        assert_eq!(lines, vec![3, 4], "{held:?}");
        assert_eq!(index.answered_at_callers(&notify), BTreeSet::new());
    }

    /// A call through a parameter, written in a constructor, has no call
    /// site of its own to check: a request it may reach is neither held nor
    /// answered (carrick#4127).
    #[test]
    fn a_call_through_a_parameter_is_not_held() {
        let (_dir, index) = summarized(&[
            ("src/net/notify.ts", NOTIFY),
            (
                "src/alerts.ts",
                "import { notify } from './net/notify';\n\
                 export const fire = () => notify('/fire');\n\
                 export const eager = notify('/eager');\n\
                 export class Pager {\n\
                 \x20 constructor(notify: (endpoint: string) => void) {\n\
                 \x20   notify('/page');\n\
                 \x20 }\n\
                 }\n",
            ),
        ]);
        let notify = key_of(&index, "src/net/notify.ts");
        assert_eq!(index.held_at_fact_calls(&notify), BTreeMap::new());
        assert_eq!(index.answered_at_callers(&notify), BTreeSet::new());
    }
}
