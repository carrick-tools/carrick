//! Discovery (carrick#2431, step 1 of the fact layer): every call that
//! reaches a library export the store labels with a network role, at any
//! depth, for every protocol.
//!
//! A call is a site when its callee, followed through the compiler's
//! per-position rows (its `call` row, that declaration's definition, the
//! calls in its body, and so on, [`RequestSummaryInputs::reach`]), reaches a
//! call or `new` on a labelled export: the export itself (`fetch(…)`, `new
//! Socket(…)`), or a member of something whose row names a labelled export
//! by its symbol or its type (`this.child.send(…)` on a `ChildProcess`), or
//! a member of an instance one of a labelled export's makers built, as the
//! library-site reader reads it (`const app = pkg(); app.post(…)`, matched
//! on the op the store labels on that receiver, carrick#2506). An
//! export is matched by the name the store keys it by: the name the module
//! publishes it as (`default`, `export =`, a renamed export) for a call
//! through an import binding, and for a global of the language's own library
//! its name under a default-library key, wherever its types declare it. An
//! optional call (`socket?.emit(…)`) is a call. A
//! function handed to an in-repo callee that runs it is followed from the
//! call that hands it over. A function written at a call into a library is
//! followed when the compiler's row says the call returns what the function
//! returns (`FLAG_RUNS_HANDED`, carrick#2634): a tracing runner, a retry
//! helper, `then`. A function of the repository handed there by name
//! (`then(handle)`) is followed the same way (carrick#3442). Other library
//! calls are not: a listener registration or
//! a store initialiser keeps what it is handed for later.
//!
//! A call into a function declared in a file of another service of the run
//! (carrick#2618, [`discover_with`]) is followed in that file with that
//! service's own IR and rows: the chain names the sibling's files, and the
//! record is this service's, at its own call.
//!
//! There is no hop limit. A cycle ends the walk, and each definition's
//! answer is kept once it does not depend on a definition still being read.
//!
//! A walk that enters code and ends with no labelled export records why:
//! a [`HoleReason`], `interface_member`, `injected_callee`, `cycle` or
//! `no_body` (a definition whose body this pass did not read).
//!
//! A callee that is a value the repository declares (a parameter, a key of
//! one, a parameter's default, a local or an inline `a ?? b`, a function an
//! in-repo factory returns) is followed through each value it may hold
//! (carrick#2432, [`super::values`]): what each caller passes at that
//! position and key, one caller up at a time, the default where a caller
//! passes nothing. A value the compiler's row names as a labelled export is
//! that export, called at the call that runs the value; a function of the
//! repository is entered under a `<value>` hop. Values that all reach the
//! same labelled exports are those ends. Values that disagree, or one that
//! reaches none, keep their ends with the stop `several_values` (decided
//! sites make the call a lead); more than [`MAX_VALUES`] is
//! `values_over_cap`; none at all is `value_never_bound`; only functions
//! written at the callers stay `injected_callee`.
//!
//! A call that reached nothing and entered no code says why, on
//! [`Discovery::unfollowed`], and never in `sites`: `value_callee` (an in-repo
//! value that is not a definition: a parameter, a field, a returned
//! function), `outside_service` (a function of this repo in no file of any
//! service of the run), `package_unmatched` (a call declared in a package the
//! store answered with network-role exports, that matched none of them),
//! `unanswered_package` (a call declared in a package the answer holds
//! nothing for), or the hole the call's own row names. A call into an export
//! the store labels with no network role, and a walk into in-repo code that
//! finds no call to follow, stay `no_site`.
//!
//! Record-only: nothing in a scan calls this. Its tests and the offline
//! census (`discovery_census`) do. Inside request summaries, as ruling D4
//! has it: no second tracer.
//!
//! The record per site is pinned by `tests/fixtures/discovery/` (its
//! `README.md` and `sites.expected.json`), which #2433 and #2435 read.

use serde::{Deserialize, Serialize};

use super::implementations::{Followed, Implementations};
use super::resolve::{
    Follow, Invoke, MAX_VALUES, Maker, Resolver, defaults_of, note_unfilled,
    unread_where_spread_unfilled,
};
use super::row_calls::Reach;
use super::*;
use crate::library_claims::{ExportClaims, applies, runtime_packages_of, scope_step};
use crate::library_store::LibraryAnswer;
use crate::library_store::RUNTIME_TYPES;
use crate::position_rows::{
    FLAG_DECLARATION_UNNAMED, FLAG_DECLARED_TYPE, FLAG_ERROR_TYPE, FLAG_MODULE_DEFAULT,
    FLAG_PUBLISHED, FLAG_RUNS_HANDED, FLAG_TOO_MANY, Kind, Row,
};
use crate::services::type_sidecar::{
    ClaimOn, LibraryCheck, LibraryClaim, LibraryOp, LibraryRole, MakeForm, SemanticsResult,
    SemanticsVerdict,
};

/// The two words [`Walker::agree`] stops with: for several values or
/// implementations that disagree, and for more of them than it follows.
#[derive(Clone, Copy)]
struct Plural {
    several: Stop,
    over_cap: Stop,
}

const OF_VALUES: Plural = Plural {
    several: Stop::SeveralValues,
    over_cap: Stop::ValuesOverCap,
};
const OF_IMPLEMENTATIONS: Plural = Plural {
    several: Stop::SeveralImplementations,
    over_cap: Stop::ImplementationsOverCap,
};

/// The hop a walk records where it enters a function through a value.
use super::decided_sites::{BoundCall, CALLBACK_HOP, HopMaker, VALUE_HOP, handed_by};
use super::stop::Stop;

/// One discovered site, or one walk that entered code and stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverySite {
    /// Repo-relative, `/` separators.
    pub file: String,
    pub line: u32,
    /// The call's SWC span in the file parsed on its own.
    pub span_start: u32,
    pub span_end: u32,
    /// The callee as written.
    pub callee: String,
    /// The labelled call the walk reached; `None` when it stopped.
    pub end: Option<SiteEnd>,
    /// Each in-repo function the walk entered, outermost first.
    pub chain: Vec<ChainHop>,
    /// Why a walk that entered code reached no labelled export.
    pub stop: Option<Stop>,
    /// The other ways the callee's values stopped, when `stop` is one of
    /// several (carrick#3688 R3). Distinct, at most four.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<Stop>,
    /// The binding the labelled call is made on (carrick#2474 R1). Only
    /// with the receiver trace on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<ReceiverAt>,
    /// How the labelled call was matched to its export. Only with the
    /// receiver trace on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reached: Option<ReachedBy>,
    /// The calls on the way whose callee was a `bind` with bound arguments
    /// (carrick#3504 S4d-b).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bound: Vec<BoundCall>,
}

/// The labelled call a site reaches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteEnd {
    /// The store's key for the package (`lib:lib.dom.d.ts`, `node:child_process`).
    pub package: String,
    pub export: String,
    /// The member called on the export or its instance; `None` for the
    /// export itself.
    pub member: Option<String>,
    pub role: LibraryRole,
    /// The claim's op for that member; `None` for a construction.
    pub op: Option<LibraryOp>,
    /// Where the labelled call is.
    pub file: String,
    pub line: u32,
    pub span_start: u32,
    pub span_end: u32,
    /// The call constructs its callee (`new m(…)`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub new: bool,
    /// The scope member whose result the labelled call is made on, when its
    /// op is claimed on that result (`instance:()>scope:at` is `at`;
    /// carrick#1678). Only a call a verified op resolves to by its
    /// declaration states one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// The procedure a client call names when its op is named by the path
    /// walked on a proxy (`client.shelf.list.query()`, name `bound: path`):
    /// the key path and the router its first key belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub procedure: Option<ProcedureAt>,
}

/// The procedure a client call names (carrick#3186): its key
/// path, and the router's repo-relative file and binding, as
/// [`super::decided_sites::router_holder`] names it. The repository is the
/// blob's, added where the row is written.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProcedureAt {
    pub path: String,
    pub file: String,
    pub binding: String,
}

impl ProcedureAt {
    /// The pairing key in `repo`.
    pub fn key(&self, repo: &str) -> carrick_match::ProcedureKey {
        carrick_match::ProcedureKey {
            path: self.path.clone(),
            router: carrick_match::RouterId {
                repo: repo.to_string(),
                file: self.file.clone(),
                binding: self.binding.clone(),
            },
        }
    }
}

/// One function a walk entered.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChainHop {
    /// The definition key, or `<callback>` for a function written at the
    /// call and run by the callee.
    pub function: String,
    pub file: String,
    pub line: u32,
    /// See [`super::decided_sites::ChainHop::made`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made: Option<HopMaker>,
}

/// The sites of a service, sorted, and how the walks ended, for the census.
#[derive(Debug, Default)]
pub struct Discovery {
    pub sites: Vec<DiscoverySite>,
    /// The calls the walk could not follow, each with its reason as `stop`
    /// (`end` and `chain` empty). Not sites: nothing decided reads them.
    pub unfollowed: Vec<DiscoverySite>,
    /// The calls whose walk ended with nothing missed and no site (`no_site`),
    /// by file and SWC span: the calls the fact layer reads as followed to
    /// the end (carrick#2613).
    pub finished: Vec<(String, u32, u32)>,
    /// The calls whose walk found no site and passed a call it could not
    /// follow on the way (`no_site` all the same): not followed to the end.
    pub missed_inside: Vec<(String, u32, u32)>,
    /// The line and the callee as written of each `missed_inside` call, by
    /// file and SWC span: what an unread site states of it
    /// ([`crate::unread_sites`], carrick#3688 U3).
    pub missed_at: BTreeMap<(String, u32, u32), (u32, String)>,
    /// The `missed_inside` calls whose walk reached only server-framework
    /// ends in a sibling's code, by file and SWC span, each with the first
    /// such end: the call registers a route this service serves, so its
    /// unread site is an HTTP one ([`crate::unread_sites`], carrick#2474 R5b).
    pub sibling_routes_at: BTreeMap<(String, u32, u32), SiteEnd>,
    pub counts: BTreeMap<String, usize>,
    /// For each call recorded with no end (a stopped site or `missed_inside`),
    /// by file and SWC span: the store keys of the packages the walk passed a
    /// call into that the answer holds nothing for. A walk that passed one is
    /// not final until the store answers it (carrick#2813): the fact layer
    /// lists the call for the reader when the key is still pending.
    pub unanswered_at: BTreeMap<(String, u32, u32), BTreeSet<String>>,
    /// The calls, by file and SWC span, whose member declaration the walk
    /// read for an op the receiver trace's ask got no answer for
    /// ([`MemberDeclarations::answered`], carrick#3991): the answer would
    /// have decided them. Their unread sites say `unanswered` beside their
    /// own reason ([`crate::unread_sites::from_scan`]).
    pub declarations_unanswered: BTreeSet<(String, u32, u32)>,
    /// Each call of the service's own files and the functions of the
    /// repository the walk entered from it, sorted by file and span: the
    /// definition a call names, each implementation of an interface member
    /// it follows, and each function a value callee may hold, at the call
    /// that runs it, whichever phase read it (the walk, or the settle at a
    /// definition's callers). Library ends are not among them. What the call
    /// graph writes as edges where resolution and the rows state none
    /// ([`crate::call_graph::plan_edges`]).
    pub entered: Vec<Entered>,
}

/// One call and the functions of the repository the walk entered from it
/// ([`Discovery::entered`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entered {
    /// The file as the service's inputs key it (as walked).
    pub file: PathBuf,
    /// The call's span in the discovery source map: the key the call graph
    /// reads a call site with.
    pub lo: u32,
    pub hi: u32,
    pub line: u32,
    pub targets: BTreeSet<EnteredTarget>,
}

/// A function of the repository a call entered, and how the walk reached it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnteredTarget {
    pub file: PathBuf,
    pub function: EnteredFunction,
    pub kind: EnteredKind,
}

/// Which function of its file a call entered.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum EnteredFunction {
    /// A definition, by the key the service's inputs hold it under.
    Keyed(String),
    /// A function written as a value (an arrow a factory returns, a
    /// function in an object), by the line it opens on.
    Written { line: u32 },
}

/// How the walk entered a function from a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EnteredKind {
    /// The definition the call names.
    Named,
    /// The constructor a `new` runs.
    Constructor,
    /// An implementation of the interface member the call goes through.
    Implementation,
    /// A function the call's callee, a value, may hold.
    Value,
    /// A named function handed to a library call that runs what it is
    /// handed (`FLAG_RUNS_HANDED`, carrick#3442).
    Handed,
}

/// The labelled exports of the store's answer, by the store's package key
/// and the export's name. An export labelled `none` is no network export.
pub struct Labels<'a> {
    exports: &'a [ExportClaims],
    by_name: HashMap<(&'a str, &'a str), &'a ExportClaims>,
    /// With the receiver trace on (carrick#2474 R1), the member
    /// declarations verified op claims resolve to: a call to one is a site
    /// whatever holds its receiver, and every site names its receiver.
    declarations: Option<&'a MemberDeclarations>,
    /// Every package the store answered, with or without an export; `None`
    /// when only the exports are known ([`Labels::of`]), where no package
    /// can be told unanswered.
    answered: Option<HashSet<&'a str>>,
    /// The packages the scan ended without an answer for ([`crate::library_store::Settled::unanswered`]),
    /// for the settled pass; `None` leaves [`Self::answered`] to decide.
    unready: Option<HashSet<&'a str>>,
    /// The packages with an export of a network role.
    networked: HashSet<&'a str>,
    /// The exports the store labelled with no network role.
    none: HashSet<(&'a str, &'a str)>,
    /// What the sidecar found for the calls through an interface member
    /// (carrick#2432 slice B); `None` leaves each one a stop.
    implementations: Option<&'a Implementations>,
}

impl<'a> Labels<'a> {
    pub fn of(exports: &'a [ExportClaims]) -> Self {
        let by_name = exports
            .iter()
            .filter(|export| export.role != LibraryRole::None)
            .map(|export| ((export.package.as_str(), export.export.as_str()), export))
            .collect();
        Labels {
            exports,
            by_name,
            declarations: None,
            implementations: None,
            answered: None,
            unready: None,
            networked: exports
                .iter()
                .filter(|export| export.role != LibraryRole::None)
                .map(|export| export.package.as_str())
                .collect(),
            none: exports
                .iter()
                .filter(|export| export.role == LibraryRole::None)
                .map(|export| (export.package.as_str(), export.export.as_str()))
                .collect(),
        }
    }

    /// [`Labels::of`], with the packages the store answered, so a call into
    /// a package it did not answer (still building, skipped, not asked) is
    /// told from one it answered with nothing to label.
    pub fn of_answer(answer: &'a LibraryAnswer) -> Self {
        Labels::of(&answer.exports).answered(&answer.answered_packages)
    }

    /// These labels with the packages the store answered, as its first
    /// answer to a scan holds them (carrick#2613): a call into a package it
    /// did not answer is [`Stop::UnansweredPackage`].
    pub fn answered(self, answered: &'a BTreeSet<String>) -> Self {
        Labels {
            answered: Some(answered.iter().map(String::as_str).collect()),
            ..self
        }
    }

    /// Whether the store labelled `export` of `package` with no network role.
    fn labelled_none(&self, package: &str, export: &str) -> bool {
        self.none.contains(&(package, export))
    }

    /// Why a call declared in the package the store keys `key` and matching
    /// no label was not followed, when the answer says nothing for it. The
    /// default library is never `package_unmatched`: its calls are matched
    /// by their global's name, and a call it does not label is no client
    /// call. It is unanswered only when no default-library key is answered.
    fn package_reason(&self, key: &str) -> Option<Stop> {
        let unanswered = |answered: &HashSet<&str>| {
            if is_language_key(key) {
                !answered.iter().any(|known| is_language_key(known))
            } else {
                !answered.contains(key)
            }
        };
        if !is_language_key(key) && self.networked.contains(key) {
            return Some(Stop::PackageUnmatched);
        }
        if let Some(unready) = &self.unready {
            // One language key is one whole default-library file: never unready.
            return (!is_language_key(key) && unready.contains(key))
                .then_some(Stop::UnansweredPackage);
        }
        self.answered
            .as_ref()
            .is_some_and(unanswered)
            .then_some(Stop::UnansweredPackage)
    }

    /// These labels for the settled pass, with the packages the scan ended
    /// without an answer for: a call into one is not followed, for
    /// [`Stop::UnansweredPackage`]. For labels with no answered set
    /// ([`Self::answered`] is the first answer's).
    pub fn ended_without(self, unready: &'a BTreeSet<String>) -> Self {
        debug_assert!(self.answered.is_none());
        Labels {
            unready: Some(unready.iter().map(String::as_str).collect()),
            ..self
        }
    }

    /// These labels, following a call through an interface member into the
    /// implementations `implementations` holds for it.
    pub fn following(self, implementations: &'a Implementations) -> Self {
        Labels {
            implementations: Some(implementations),
            ..self
        }
    }

    /// These labels with the receiver trace on (carrick#2474 R1), for the
    /// first answer's discovery and the settled one alike (carrick#4077).
    pub fn tracing(self, declarations: &'a MemberDeclarations) -> Self {
        Labels {
            declarations: Some(declarations),
            ..self
        }
    }

    fn get(&self, package: &str, export: &str) -> Option<&'a ExportClaims> {
        self.by_name.get(&(package, export)).copied()
    }

    /// The labelled global the language's own library declares as `name`,
    /// as the store keys one: under a default-library key
    /// (`lib:<file>`), whichever declaration file the compiler read it from.
    /// The first such key, in order, when several label it.
    fn global(&self, name: &str) -> Option<&'a ExportClaims> {
        self.by_name
            .iter()
            .filter(|((package, export), _)| *export == name && is_language_key(package))
            .min_by_key(|((package, _), _)| *package)
            .map(|(_, claims)| *claims)
    }
}

/// How many other ways a site's values stopped it keeps beside its reason
/// (carrick#3688 R3).
const MAX_ALSO: usize = 4;

/// The one stop a callee's values that all reached nothing agree on
/// (carrick#3688 U2): the way every one stopped, when they all stopped the
/// same way and say so; otherwise `several`, with the distinct ways named
/// (at most [`MAX_ALSO`]). The resolver never collapses: this is
/// discovery's policy.
fn agreed_stop(reasons: &[Option<Stop>], several: Stop) -> (Stop, Vec<Stop>) {
    let ways: BTreeSet<Stop> = reasons.iter().flatten().copied().collect();
    match (ways.iter().next(), ways.len()) {
        (Some(one), 1) if reasons.iter().all(Option::is_some) => (*one, Vec::new()),
        _ => (
            several,
            ways.into_iter()
                .filter(|way| *way != several)
                .take(MAX_ALSO)
                .collect(),
        ),
    }
}

/// The store's key for a global the runtime's types package declares
/// (`node:web-globals/fetch` for `fetch`), the key the scanner asks
/// ([`runtime_globals`]). None for any other row.
fn runtime_global_key(row: &Row) -> Option<String> {
    (row.decl_package() == Some(RUNTIME_TYPES))
        .then(|| store_keys(row, &[]).into_iter().next())
        .flatten()
}

/// Whether a call is made on a bare name the file holds no binding for
/// (`fetch(…)`), so it is read as the language's or the runtime's global.
fn is_free_name(ir: &FileIr, call: &CallIr) -> bool {
    call.receiver_spans.is_empty()
        && root_name(call).is_some_and(|root| !ir.bindings.contains_key(root))
}

/// The packages a service's calls reach, as the compiler resolves them
/// (carrick#2588): for each `call` row whose declaration is outside the
/// repository, the package it is declared in, by the name it is installed
/// under (`@types/x` types `x`). A call on an instance handed in as a
/// parameter typed by a type-only import (`import type { App } from
/// 'framework'`) names its package here and nowhere in the library sites.
/// The language's own library and the runtime's types are asked by the
/// globals the code uses ([`runtime_globals`]), so neither is here.
pub fn called_packages(inputs: &RequestSummaryInputs) -> BTreeSet<String> {
    let Some(positions) = inputs.positions.as_ref() else {
        return BTreeSet::new();
    };
    positions
        .call_rows()
        .filter(|row| !in_repo(row))
        .filter_map(|row| row.decl_package())
        .filter(|package| *package != RUNTIME_TYPES)
        .map(crate::engine::type_compat_v2::types_package_runtime_name)
        .collect()
}

/// The globals a service's calls use that the runtime's types package
/// declares, by the store's key for the file they are declared in
/// (carrick#2445): a Node program that loads no DOM library has its
/// `fetch` from `web-globals/fetch.d.ts` under the runtime's types, not
/// from a default-library file. Read with the same test discovery reads a
/// global by ([`Walker::library_end`]).
pub fn runtime_globals(inputs: &RequestSummaryInputs) -> BTreeMap<String, BTreeSet<String>> {
    fn walk(
        inputs: &RequestSummaryInputs,
        file: &Path,
        ir: &FileIr,
        body: &FnIr,
        out: &mut BTreeMap<String, BTreeSet<String>>,
    ) {
        let Some(positions) = inputs.positions.as_ref() else {
            return;
        };
        for (call, _) in body.read_calls() {
            if !is_free_name(ir, call) {
                continue;
            }
            let Ok(row) = positions.lookup(
                file,
                ir.swc_start(call.site.lo),
                ir.swc_start(call.site.hi),
                Kind::Call,
            ) else {
                continue;
            };
            if let (Some(symbol), Some(key)) = (row.symbol(), runtime_global_key(row)) {
                out.entry(key).or_default().insert(symbol.to_string());
            }
        }
        for inner in body.nested.iter().chain(&body.detached) {
            walk(inputs, file, ir, inner, out);
        }
    }
    let mut out = BTreeMap::new();
    for (path, ir) in &inputs.files {
        for body in ir.functions.values().chain([&ir.module_level]) {
            walk(inputs, path, ir, body, &mut out);
        }
    }
    out
}

/// Whether a store key names the language's own library.
fn is_language_key(package: &str) -> bool {
    package.starts_with("lib.") || package.starts_with("lib:")
}

/// The store's keys for the package a specifier imports: the runtime's
/// module (`node:<module>`) for a `node:` specifier, and for a bare one both
/// the runtime's module of that name and the package, as the store asks a
/// specifier ([`crate::library_store::request`]). A Deno `npm:` specifier
/// is only its package (carrick#4070).
fn specifier_keys(specifier: &str) -> Vec<String> {
    if specifier.starts_with("node:") {
        return vec![specifier.to_string()];
    }
    let package = super::library_sites::package_name(specifier);
    let mut keys = vec![package.to_string()];
    if !specifier.starts_with('.') && !specifier.starts_with("npm:") && !specifier.contains('/') {
        keys.push(format!("node:{specifier}"));
    }
    keys
}

/// Each object the callee's member chain is read on, innermost first: its
/// span (discovery numbering), whether it is a member access, and the key
/// that member reads as the code writes it (`.name`, `['name']`; `None` for
/// the root name and any other computed key, carrick#3778 S0). An optional
/// member (`socket?.emit`) is read as a member. The walk stops at anything
/// but a name or a member access (`this`, a call).
pub(crate) fn receiver_spans(callee: &Expr) -> Vec<(u32, u32, bool, Option<String>)> {
    fn member_of(expr: &Expr) -> Option<&MemberExpr> {
        match crate::graphql_document_sites::unwrap_expression(expr) {
            Expr::Member(member) => Some(member),
            Expr::OptChain(OptChainExpr { base, .. }) => match &**base {
                OptChainBase::Member(member) => Some(member),
                OptChainBase::Call(_) => None,
            },
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut at = member_of(callee);
    while let Some(member) = at {
        let object = crate::graphql_document_sites::unwrap_expression(&member.obj);
        if let Expr::Ident(ident) = object {
            out.push((ident.span.lo.0, ident.span.hi.0, false, None));
            break;
        }
        at = member_of(object);
        if let Some(member) = at {
            let span = object.span();
            out.push((
                span.lo.0,
                span.hi.0,
                true,
                super::resolve::static_key(member),
            ));
        }
    }
    out
}

/// The binding a call's callee is read through: the callee itself, or the
/// name its member chain starts at.
fn root_name(call: &CallIr) -> Option<&str> {
    match call.receiver_spans.last() {
        None => Some(call.callee_text.as_str()),
        Some((_, _, false, _)) => call
            .callee_text
            .split(['.', '?', '['])
            .next()
            .map(str::trim),
        Some(_) => None,
    }
    .filter(|root| super::row_calls::is_identifier(root))
}

/// How the object a callee's member is read on is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObjectForm {
    /// A name (`app` in `app.get(…)`).
    Name,
    /// A member access (`this.app`, `deps.app`).
    Member,
    /// A call's value (`build()` in `build().get(…)`).
    Call,
    /// Anything else (`this`, `new X()`, a literal).
    Other,
}

/// The object the callee's member is read on: its span (discovery
/// numbering) and how it is written. `None` when the callee is no member
/// access.
pub(crate) fn receiver_object(callee: &Expr) -> Option<(u32, u32, ObjectForm)> {
    use crate::graphql_document_sites::unwrap_expression;
    let object = match unwrap_expression(callee) {
        Expr::Member(member) => &member.obj,
        Expr::OptChain(OptChainExpr { base, .. }) => match &**base {
            OptChainBase::Member(member) => &member.obj,
            OptChainBase::Call(_) => return None,
        },
        _ => return None,
    };
    let object = unwrap_expression(object);
    let form = match object {
        Expr::Ident(_) => ObjectForm::Name,
        Expr::Member(_) => ObjectForm::Member,
        Expr::Call(_) => ObjectForm::Call,
        Expr::OptChain(OptChainExpr { base, .. }) => match &**base {
            OptChainBase::Member(_) => ObjectForm::Member,
            OptChainBase::Call(_) => ObjectForm::Call,
        },
        _ => ObjectForm::Other,
    };
    let span = object.span();
    Some((span.lo.0, span.hi.0, form))
}

/// What holds the receiver a labelled call is made on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiverKind {
    /// A name a declaration binds that is no parameter (`const app = …`).
    Binding,
    /// A function's parameter, a callback's included.
    Parameter,
    /// A member read off another object (`this.app`).
    Field,
    /// A call's value (`build().get(…)`).
    CallResult,
    /// Anything else (`this`).
    Other,
}

/// The binding a labelled call is made on (carrick#2474 R1): the compiler
/// row at the receiver's span, as it names the receiver's declaration, and
/// what holds it. Recorded only with the receiver trace on; never part of a
/// record's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiverAt {
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub decl_file: Option<String>,
    #[serde(default)]
    pub decl_line: Option<u32>,
    pub kind: ReceiverKind,
}

/// How discovery matched a labelled call to its export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReachedBy {
    /// A name: the export's published name, an import binding, a global,
    /// or a receiver's symbol or type named like the export.
    Name,
    /// An instance one of the export's makers built, as the library-site
    /// reader holds it.
    Instance,
    /// The declaration the call's member resolves to, which a verified op
    /// claim resolves to on a receiver the claim applies to.
    Declaration,
    /// The export the compiler types the callee as (`send: typeof fetch`),
    /// where the callee's values are all holes (carrick#2448 T3b).
    Type,
}

/// One op a member declaration stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredOp {
    /// The store's key for the package, as [`Labels`] keys an export.
    pub package: String,
    pub export: String,
    pub member: String,
    pub op: LibraryOp,
    /// The scope member the receiver the op was asked on is the result of
    /// (carrick#1678): the same member declared on another type is another
    /// op.
    pub scope: Option<String>,
}

/// The verify request the receiver trace sends: each labelled op claim whose
/// member a `call` row of that package names, on every receiver the claim
/// applies to, with the export's maker claims the verifier judges an
/// instance op with. `ops` lines up with `checks`: the op each op check
/// stands for, `None` for a maker. Each labelled scope claim whose member a
/// `call` row names is asked too (carrick#2474 R2b, carrick#1678), on the
/// same receivers, after every op check (the verifier judges in request
/// order under a time limit, so near it a scope goes unchecked, never an
/// op): `scopes` lines up with `checks` the same way.
#[derive(Debug, Default, Clone)]
pub struct DeclarationAsk {
    pub checks: Vec<LibraryCheck>,
    pub ops: Vec<Option<DeclaredOp>>,
    pub scopes: Vec<Option<AskedScope>>,
}

/// One scope claim the trace asks, by its claim id, on one receiver id
/// (carrick#2474 R2b).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AskedScope {
    /// The store's key for the package.
    pub package: String,
    pub export: String,
    pub claim_id: String,
    pub receiver: String,
}

/// What the declaration ask returned for one scope check (carrick#4155).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeAnswer {
    /// Verified, with the member declarations (`decl_file`, `decl_line`)
    /// the verdict names (carrick#2636): a call whose row resolves to one
    /// of them is a call of that scope member, whatever holds its receiver.
    Verified(Vec<(String, u32)>),
    /// Asked, and the ask got no answer ([`MemberDeclarations::answered`]):
    /// a call of the member is one the answer would have decided, so the
    /// decided pass saves the sites it leaves open `unanswered`
    /// ([`super::decided_sites::Decided::scopes_unanswered`]).
    Unanswered,
}

impl ScopeAnswer {
    /// The member declarations a verified check names; `None` for a check
    /// with no answer.
    pub fn verified(&self) -> Option<&[(String, u32)]> {
        match self {
            ScopeAnswer::Verified(declarations) => Some(declarations),
            ScopeAnswer::Unanswered => None,
        }
    }
}

/// The receiver id the instances a maker claim builds are read through, as
/// [`LibrarySite::receiver_ids`] names a site's.
fn make_receiver(form: MakeForm, member: Option<&str>) -> String {
    super::library_sites::maker_receiver_id(form == MakeForm::New, member)
}

/// Every receiver a claim's `on` or `of` applies to among the export itself
/// and the instances `claims`' makers build, sorted.
pub(crate) fn claim_receivers(
    claims: &[(String, &LibraryClaim)],
    on: Option<ClaimOn>,
    of: Option<&str>,
) -> Vec<String> {
    let mut all = vec!["export".to_string()];
    for (_, claim) in claims {
        if let LibraryClaim::Make { form, member, .. } = claim {
            all.push(make_receiver(*form, member.as_deref()));
        }
    }
    all.extend(of.map(str::to_string));
    all.sort();
    all.dedup();
    all.retain(|receiver| applies(on, of, receiver));
    all
}

/// The ask for the service `inputs` reads, under the store's answer
/// `exports`. Nothing without rows, or with no member any op names.
pub fn declaration_ask(inputs: &RequestSummaryInputs, exports: &[ExportClaims]) -> DeclarationAsk {
    let mut ask = DeclarationAsk::default();
    let Some(positions) = inputs.positions.as_ref() else {
        return ask;
    };
    let called: HashSet<(String, String)> = positions
        .call_rows()
        .chain(positions.member_rows())
        .filter(|row| !in_repo(row))
        .flat_map(|row| {
            let symbol = row.symbol().map(str::to_string);
            store_keys(row, exports)
                .into_iter()
                .filter_map(move |key| Some((key, symbol.clone()?)))
        })
        .collect();
    // The scope checks, asked once every export's op checks are in.
    let mut scope_checks: Vec<(LibraryCheck, AskedScope)> = Vec::new();
    for export in exports
        .iter()
        .filter(|export| export.role != LibraryRole::None)
    {
        let claims = export.claims_with_scopes_by_id();
        let wanted: Vec<&(String, &LibraryClaim)> = claims
            .iter()
            .filter(|(_, claim)| match claim {
                LibraryClaim::Op {
                    member: Some(member),
                    ..
                }
                | LibraryClaim::Scope { member, .. } => {
                    called.contains(&(export.package.clone(), member.clone()))
                }
                _ => false,
            })
            .collect();
        if wanted.is_empty() {
            continue;
        }
        let check = |id: &str, receiver: &str, claim: &LibraryClaim| LibraryCheck {
            claim_id: id.to_string(),
            package: export.specifier.clone(),
            export: export.export.clone(),
            role: export.role,
            receiver: receiver.to_string(),
            claim: claim.without_result(),
        };
        for (id, claim) in &claims {
            if matches!(claim, LibraryClaim::Make { .. }) {
                ask.checks.push(check(id, "export", claim));
                ask.ops.push(None);
                ask.scopes.push(None);
            }
        }
        let on_receivers = |on: Option<ClaimOn>, of: Option<&str>| claim_receivers(&claims, on, of);
        for (id, claim) in wanted {
            match claim {
                LibraryClaim::Op {
                    op,
                    member: Some(member),
                    on,
                    of,
                    ..
                } => {
                    for receiver in on_receivers(*on, of.as_deref()) {
                        ask.checks.push(check(id, &receiver, claim));
                        ask.ops.push(Some(DeclaredOp {
                            package: export.package.clone(),
                            export: export.export.clone(),
                            member: member.clone(),
                            op: *op,
                            scope: scope_step(&receiver).map(|(_, step)| step.to_string()),
                        }));
                        ask.scopes.push(None);
                    }
                }
                LibraryClaim::Scope { on, of, .. } => {
                    for receiver in on_receivers(*on, of.as_deref()) {
                        scope_checks.push((
                            check(id, &receiver, claim),
                            AskedScope {
                                package: export.package.clone(),
                                export: export.export.clone(),
                                claim_id: id.clone(),
                                receiver,
                            },
                        ));
                    }
                }
                _ => {}
            }
        }
    }
    for (check, scope) in scope_checks {
        ask.checks.push(check);
        ask.ops.push(None);
        ask.scopes.push(Some(scope));
    }
    ask
}

/// Each member declaration a verified op claim resolves to, with the ops it
/// stands for (carrick#2474 R1), and how the ask went.
#[derive(Debug, Default, Clone)]
pub struct MemberDeclarations {
    by_declaration: HashMap<(String, u32), Vec<DeclaredOp>>,
    /// The scope claims the ask verified or got no answer for, each by
    /// claim id on the receiver id it was asked on (carrick#2474 R2b,
    /// carrick#4155). A claim the verifier refuted is not here.
    pub scope_answers: HashMap<AskedScope, ScopeAnswer>,
    /// The checks asked.
    pub checks: usize,
    /// Of those, the ones the verifier did not reach in its time limit.
    pub not_reached: usize,
    /// The op checks' (store key, member) pairs when the ask got no answer
    /// (it failed, or came back short): a call whose `call` row names one
    /// is a call the answer would have decided, saved `unanswered` beside
    /// its own reason ([`Discovery::declarations_unanswered`], carrick#3991).
    /// Empty when the verifier answered, including answers that verified
    /// nothing.
    unanswered: HashSet<(String, String)>,
}

impl MemberDeclarations {
    /// The declarations the verifier's `answer` to `ask` names. An `Err`,
    /// or an answer with a verdict count other than the checks', indexes
    /// nothing and keeps the op checks (carrick#3991) and the scope checks
    /// (carrick#4155) it left open.
    pub fn answered<E: std::fmt::Display>(
        ask: &DeclarationAsk,
        answer: Result<Vec<SemanticsResult>, E>,
    ) -> Self {
        let verdicts = match answer {
            Ok(verdicts) if verdicts.len() == ask.checks.len() => {
                return MemberDeclarations::of(ask, &verdicts);
            }
            Ok(verdicts) => {
                tracing::warn!(
                    asked = ask.checks.len(),
                    answered = verdicts.len(),
                    "receiver trace: the declaration answer does not match the ask; none indexed"
                );
                verdicts
            }
            Err(error) => {
                tracing::warn!(%error, "receiver trace: the declaration ask failed; none indexed");
                Vec::new()
            }
        };
        MemberDeclarations {
            unanswered: ask
                .ops
                .iter()
                .flatten()
                .map(|op| (op.package.clone(), op.member.clone()))
                .collect(),
            // The scope checks it left open (carrick#4155).
            scope_answers: ask
                .scopes
                .iter()
                .flatten()
                .map(|scope| (scope.clone(), ScopeAnswer::Unanswered))
                .collect(),
            ..MemberDeclarations::of(ask, &verdicts)
        }
    }

    /// Whether the ask got no answer for the op check `row`'s declaration
    /// and symbol stand for.
    fn left_open(&self, row: &Row, exports: &[ExportClaims]) -> bool {
        let Some(symbol) = row.symbol() else {
            return false;
        };
        !self.unanswered.is_empty()
            && !in_repo(row)
            && store_keys(row, exports)
                .into_iter()
                .any(|key| self.unanswered.contains(&(key, symbol.to_string())))
    }

    /// The declarations the verdicts for `ask` name. A verdict counts only
    /// for the check at its own position that it names.
    pub fn of(ask: &DeclarationAsk, verdicts: &[SemanticsResult]) -> Self {
        let mut out = MemberDeclarations {
            checks: ask.checks.len(),
            not_reached: crate::services::type_sidecar::not_reached_in_time(verdicts),
            ..MemberDeclarations::default()
        };
        if verdicts.len() != ask.checks.len() {
            return out;
        }
        for (((check, op), scope), verdict) in ask
            .checks
            .iter()
            .zip(&ask.ops)
            .zip(&ask.scopes)
            .zip(verdicts)
        {
            if verdict.verdict != SemanticsVerdict::Verified
                || verdict.claim_id != check.claim_id
                || verdict.receiver != check.receiver
            {
                continue;
            }
            if let Some(scope) = scope {
                let mut declarations: Vec<(String, u32)> = verdict
                    .declarations
                    .iter()
                    .map(|declared| (declared.decl_file.clone(), declared.decl_line))
                    .collect();
                declarations.sort();
                declarations.dedup();
                out.scope_answers
                    .insert(scope.clone(), ScopeAnswer::Verified(declarations));
            }
            let Some(op) = op else {
                continue;
            };
            for declared in &verdict.declarations {
                let ops = out
                    .by_declaration
                    .entry((declared.decl_file.clone(), declared.decl_line))
                    .or_default();
                if !ops.contains(op) {
                    ops.push(op.clone());
                }
            }
        }
        out
    }

    /// The ops the declaration at `decl_file:decl_line` stands for.
    fn at(&self, decl_file: &str, decl_line: u32) -> &[DeclaredOp] {
        self.by_declaration
            .get(&(decl_file.to_string(), decl_line))
            .map_or(&[], Vec::as_slice)
    }

    /// How many declarations are indexed.
    pub fn len(&self) -> usize {
        self.by_declaration.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_declaration.is_empty()
    }
}

/// The store's keys for the package a row's declaration is in: the default
/// library's file, as the store keys it (`lib:lib.dom.d.ts`), a runtime module (`node:<module>`, the file
/// the runtime's types declare it in), or every package the declaring package
/// stands for ([`runtime_packages_of`]). Empty when the row names no
/// declaration.
pub(super) fn store_keys(row: &Row, exports: &[ExportClaims]) -> Vec<String> {
    let Some(file) = row.decl_file() else {
        return Vec::new();
    };
    if file.starts_with("lib:") {
        return vec![file.to_string()];
    }
    let Some(package) = row.decl_package() else {
        return Vec::new();
    };
    if package == RUNTIME_TYPES {
        let module = file
            .rsplit_once(&format!("{RUNTIME_TYPES}/"))
            .and_then(|(_, module)| module.strip_suffix(".d.ts"));
        return module
            .map(|module| format!("node:{}", module.strip_suffix("/index").unwrap_or(module)))
            .into_iter()
            .collect();
    }
    runtime_packages_of(package, exports)
}

/// The names a row's declaration is published as, as the store keys an
/// export: `default` and `export =` for its module's whole-module target,
/// and its own name when its module exports it by that name or it is a
/// global. None for a member of a class or interface, whatever its name,
/// and none for a row that does not say (carrick#2508).
fn published_names(row: &Row) -> Vec<&str> {
    let mut names = Vec::new();
    if row.flags() & FLAG_MODULE_DEFAULT != 0 {
        names.extend(["default", "export ="]);
    }
    if row.flags() & FLAG_PUBLISHED != 0
        && let Some(symbol) = row.symbol()
        && !names.contains(&symbol)
    {
        names.push(symbol);
    }
    names
}

/// Whether a row's declaration is a file of the repository.
pub(super) fn in_repo(row: &Row) -> bool {
    row.flags() & FLAG_DECLARATION_UNNAMED == 0
        && row.decl_file().is_some_and(|file| {
            !file.starts_with("lib:")
                && !file.starts_with("pkg:")
                && !file.split('/').any(|part| part == "node_modules")
        })
}

/// What [`Walker::procedure_end`] reads at a call on a procedure client.
enum ProcedureCall {
    /// A procedure call: its end, and the reason when its key cannot be
    /// made.
    Site(Box<SiteEnd>, Option<Stop>),
    /// A typed client whose keys are all present, calling an op its type
    /// lacks (`client.p.mutate()` on a query): no procedure call, and no
    /// call on an untyped client either (carrick#3847 reads only a router
    /// type that does not resolve).
    TypedWithoutKey,
}

/// The export of the store's `package` that claims a call-form op on
/// `member` of the type `type_name` the package declares (`of:
/// "type:<package>#<type_name>"`), named by the path walked on that type
/// (`name: { bound: "path" }`), and the op's join.
fn path_op<'e>(
    exports: &'e [ExportClaims],
    package: &str,
    type_name: &str,
    member: &str,
) -> Option<(&'e ExportClaims, Option<String>)> {
    exports
        .iter()
        .filter(|export| export.package == package)
        .find_map(|export| path_op_of(export, type_name, member))
}

/// The one export, of any package in the store, that claims a path-named
/// call-form op on `member` of the type `type_name` its own package declares
/// ([`path_op`]), and the op's join: for a call the compiler gives no
/// declaration to name a package by, a call of a key the client's type lacks
/// (carrick#3778 S0). `None` when no export or more than one claims it.
fn sole_path_op<'e>(
    exports: &'e [ExportClaims],
    type_name: &str,
    member: &str,
) -> Option<(&'e ExportClaims, Option<String>)> {
    let mut claimed = exports
        .iter()
        .filter_map(|export| path_op_of(export, type_name, member));
    let first = claimed.next()?;
    claimed.next().is_none().then_some(first)
}

/// The join of the path-named call-form op `export` claims on `member` of
/// the type `type_name` its own package declares (`of:
/// "type:<package>#<type_name>"`), when it claims one.
fn path_op_of<'e>(
    export: &'e ExportClaims,
    type_name: &str,
    member: &str,
) -> Option<(&'e ExportClaims, Option<String>)> {
    use crate::services::type_sidecar::{BoundName, OpName};
    if export.role == LibraryRole::None {
        return None;
    }
    export.claims.iter().find_map(|claim| match claim {
        LibraryClaim::Op {
            member: Some(claimed),
            of: Some(of),
            name,
            ..
        } if claim.is_call_form() && claimed == member => {
            let (module, held) = of.strip_prefix("type:")?.split_once('#')?;
            let join = match name.as_deref()? {
                OpName::Bound {
                    bound: BoundName::Path,
                    join,
                    ..
                } => join.clone(),
                _ => return None,
            };
            (held == type_name && specifier_keys(module).contains(&export.package))
                .then_some((export, join))
        }
        _ => None,
    })
}

/// Whether `claim` is a call-form op on `member` that names its operation by
/// the key path walked on the receiver ([`path_op`]'s claim, any type).
fn is_path_named_call(claim: &LibraryClaim, member: &str) -> bool {
    use crate::services::type_sidecar::{BoundName, OpName};
    match claim {
        LibraryClaim::Op {
            member: Some(claimed),
            name,
            ..
        } => {
            claim.is_call_form()
                && claimed == member
                && matches!(
                    name.as_deref(),
                    Some(OpName::Bound {
                        bound: BoundName::Path,
                        ..
                    })
                )
        }
        _ => false,
    }
}

/// Whether `claims` state `member` as a call-form op of the export.
fn claims_member(claims: &ExportClaims, member: &str) -> bool {
    claims.claims.iter().any(|claim| {
        claim.is_call_form()
            && matches!(claim, LibraryClaim::Op { member: Some(claimed), path, .. }
                if claimed == member && path.is_empty())
    })
}

/// The name a type's text starts with (`Broker`, `typeof Socket`).
fn type_root(text: &str) -> Option<&str> {
    let text = text.strip_prefix("typeof ").unwrap_or(text);
    let end = text
        .char_indices()
        .find(|(_, c)| !(c.is_alphanumeric() || *c == '_' || *c == '$'))
        .map_or(text.len(), |(at, _)| at);
    let name = &text[..end];
    (!name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit())).then_some(name)
}

/// One labelled call a walk reached, with the functions it entered below
/// where the walk started.
#[derive(Debug, Clone)]
struct Found {
    chain: Vec<ChainHop>,
    end: SiteEnd,
    /// With the receiver trace on: the labelled call's receiver and how it
    /// was matched.
    receiver: Option<ReceiverAt>,
    reached: Option<ReachedBy>,
    /// Reached through a callee whose values reach different labelled
    /// exports (carrick#2432): one of several, so no site.
    several: Option<Stop>,
    /// The calls the walk read through a `bind` with bound arguments
    /// (carrick#3504 S4d-b).
    bound: Vec<BoundCall>,
}

/// What a call or a body reaches.
#[derive(Debug, Clone, Default)]
struct Outcome<'a> {
    ends: Vec<Found>,
    /// The first stop, when the walk entered code and found nothing there.
    stop: Option<(Vec<ChainHop>, Stop)>,
    /// With `stop`: the file of the call it was read at, so a stop read in a
    /// sibling's code can be told from one read in this service's own
    /// (carrick#4474).
    stop_read_in: Option<&'a Path>,
    /// A stop the walk read at a call in a sibling's file, beside the ends it
    /// found: no site walk of this service reads that call, so the call
    /// that entered the sibling is where it is saved (carrick#4474). The
    /// ends are real, and the stop would otherwise be dropped with `stop`.
    beside: Option<(Vec<ChainHop>, Stop)>,
    /// With `stop`: the other ways the callee's values stopped, when they
    /// stopped in several (carrick#3688 U2, R3). Distinct, at most
    /// [`MAX_ALSO`].
    also: Vec<Stop>,
    /// With `stop`: it is what the callee's values agree on ([`Walker::agree`]),
    /// so the walk read values even where it entered no function.
    valued: bool,
    /// The shallowest definition still being read that the walk came back
    /// to: until it is done, this answer is not final.
    cycle_at: Option<usize>,
    /// Somewhere in the code the walk read, a call could not be followed
    /// (one of [`Discovery::unfollowed`]'s reasons): a walk that found no
    /// site has not read everything the call runs (carrick#2613).
    missed: bool,
    /// The store keys of every package the walk passed a call into that the
    /// answer holds nothing for (`unanswered_package`), when that call's own
    /// walk found nothing (carrick#2813).
    unanswered: BTreeSet<String>,
    /// The walk passed a call the receiver trace's dead ask would have
    /// decided ([`Discovery::declarations_unanswered`]), when that call's
    /// own walk found nothing (carrick#3991).
    declarations_open: bool,
    /// The walk passed, in a sibling's file, a call chained on a maker's
    /// value (`new Queue(q).add`, `new BroadcastChannel(n).postMessage`) that
    /// the maker's export claims as an op, and read nothing at it
    /// (carrick#4445). The ends found elsewhere are real; this call is not
    /// read, so the call that entered the sibling is `missed_inside`.
    passed_op: bool,
    /// Each callee whose value a caller of the body decides (carrick#2432).
    open: Vec<Open<'a>>,
    /// The outcome of a value the resolver read to a labelled export or a
    /// function (carrick#2448 T3b), not a hole: a hole is the default
    /// outcome, which a function that reaches nothing also is.
    known: bool,
}

/// A callee that is a value some of whose values are parameters of the body
/// walked (carrick#2432): what the call `invoke` runs is one of the values
/// the callers pass there, or one of `known`.
#[derive(Debug, Clone)]
struct Open<'a> {
    /// Each parameter: how many functions out of the body walked (0: its
    /// own), its position and the keys read down from it.
    refs: Vec<(u8, usize, Vec<String>)>,
    /// The values already read.
    known: Vec<Held<'a>>,
    invoke: Invoke<'a>,
    /// The functions entered from where the walk started down to the body
    /// that holds `invoke`.
    chain: Vec<ChainHop>,
    /// A ref came from a key a caller writes only through a spread of its
    /// own parameter (ruling R-SP): when the refs' callers hold nothing,
    /// the key keeps the reading it had before the rule (carrick#4025).
    spread: bool,
}

/// One value a callee may hold, read (carrick#2432): `Reached` is what the
/// value reaches when the call invoking it runs it.
type Held<'a> = super::resolve::Held<Outcome<'a>>;

impl<'a> Outcome<'a> {
    /// A stop read at a call written in `file`.
    fn stopped(reason: Stop, file: &'a Path) -> Self {
        Outcome {
            stop: Some((Vec::new(), reason)),
            stop_read_in: Some(file),
            ..Outcome::default()
        }
    }

    /// The walk found ends, so the stop it also read is not the call's: a
    /// stop read at a call in a sibling's file, whose walk is final, is kept
    /// beside the ends instead of dropped (carrick#4474). `own` says whether
    /// a file is one of this service's.
    fn drop_stop_for_ends(&mut self, own: impl Fn(&Path) -> bool) {
        if let Some(read_in) = self.stop_read_in
            && self.cycle_at.is_none()
            && self.beside.is_none()
            && !own(read_in)
        {
            self.beside = self.stop.take();
        }
        self.stop = None;
        self.stop_read_in = None;
    }

    /// The same, as seen from a call one function further up.
    fn under(mut self, hop: ChainHop) -> Self {
        for found in &mut self.ends {
            found.chain.insert(0, hop.clone());
        }
        if let Some((chain, _)) = &mut self.stop {
            chain.insert(0, hop.clone());
        }
        if let Some((chain, _)) = &mut self.beside {
            chain.insert(0, hop.clone());
        }
        for open in &mut self.open {
            open.chain.insert(0, hop.clone());
        }
        self
    }

    /// The same, seen from where `chain` starts.
    fn below(mut self, chain: &[ChainHop]) -> Self {
        for hop in chain.iter().rev() {
            self = self.under(hop.clone());
        }
        self
    }

    fn absorb(&mut self, other: Outcome<'a>) {
        // One end per labelled call and label: a call that runs a value may
        // run one of several labelled exports (carrick#2432).
        let key = |end: &SiteEnd| {
            (
                end.file.clone(),
                end.span_start,
                end.span_end,
                end.package.clone(),
                end.export.clone(),
                end.member.clone(),
            )
        };
        for found in other.ends {
            let at = key(&found.end);
            match self.ends.iter_mut().find(|known| key(&known.end) == at) {
                Some(known) if known.chain.len() > found.chain.len() => *known = found,
                Some(_) => {}
                None => self.ends.push(found),
            }
        }
        if self.stop.is_none() {
            self.stop = other.stop;
            self.stop_read_in = other.stop_read_in;
            self.also = other.also;
            self.valued = other.valued;
        }
        if self.beside.is_none() {
            self.beside = other.beside;
        }
        self.missed |= other.missed;
        self.unanswered.extend(other.unanswered);
        self.declarations_open |= other.declarations_open;
        self.passed_op |= other.passed_op;
        self.cycle_at = match (self.cycle_at, other.cycle_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.open.extend(other.open);
    }

    /// The labelled exports the outcome reaches, as the store names them.
    fn labels(&self) -> BTreeSet<(String, String, Option<String>)> {
        self.ends
            .iter()
            .map(|found| {
                (
                    found.end.package.clone(),
                    found.end.export.clone(),
                    found.end.member.clone(),
                )
            })
            .collect()
    }
}

/// The inputs that hold `file`: `inputs`, else the first of `siblings` that
/// does (carrick#2618).
pub(super) fn inputs_holding<'a>(
    inputs: &'a RequestSummaryInputs,
    siblings: &[&'a RequestSummaryInputs],
    file: &Path,
) -> &'a RequestSummaryInputs {
    if inputs.files.contains_key(file) {
        return inputs;
    }
    siblings
        .iter()
        .copied()
        .find(|sibling| sibling.files.contains_key(file))
        .unwrap_or(inputs)
}

/// What the call `call` in `file` reaches, [`RequestSummaryInputs::reach`] of
/// the service that holds `file`. A function of another service of the run
/// is read in that service's files, from the declaration the call's row names
/// (or, when the rows say nothing, the key the call graph names); with no
/// such service the call stays outside the run. The one resolver the
/// discovery walk and the decided-sites pass share (carrick#2618).
pub(super) fn reach_across(
    inputs: &RequestSummaryInputs,
    siblings: &[&RequestSummaryInputs],
    file: &Path,
    call: &CallIr,
) -> Reach {
    let holder = inputs_holding(inputs, siblings, file);
    let reach = holder.reach(file, call.site.lo, call.site.hi);
    // A `new C(…)` or a `super(…)` runs C's constructor body, which no
    // definition the call graph keys holds (carrick#3527).
    if call.constructs
        && !matches!(reach, Reach::Function(..))
        && let Some(constructor) = constructed_across(inputs, siblings, file, call)
    {
        return Reach::Function(constructor.0, constructor.1);
    }
    let Reach::OutsideService(declaring, name) = &reach else {
        return reach;
    };
    let owner = inputs_holding(inputs, siblings, declaring);
    if !owner.files.contains_key(declaring.as_path()) {
        return reach;
    }
    let named = row_at(holder, file, call).and_then(|row| {
        let positions = holder.positions.as_ref()?;
        (positions.path_of(row.decl_file()?) == *declaring && row.symbol() == Some(name.as_str()))
            .then(|| owner.definition_at(declaring, row.decl_line(), name))
    });
    match named {
        Some(Reach::NoFunction) | None => {
            let keyed = owner.files[declaring.as_path()]
                .functions
                .contains_key(name);
            if keyed {
                Reach::Function(declaring.clone(), name.clone())
            } else {
                Reach::NoFunction
            }
        }
        Some(read) => read,
    }
}

/// Whether `call`, written in `body`, hands its callee (code of the run,
/// whose invoked parameters are `runs`) a function the callee does not run:
/// a callback argument at a position it never invokes, or a function written
/// inside an argument (`{ handler: async () => … }`), which the callee can
/// only keep (carrick#3527).
fn keeps_handed(body: &FnIr, call: &CallIr, construction: bool, runs: &BTreeSet<usize>) -> bool {
    let kept_callback = call
        .args
        .iter()
        .enumerate()
        .any(|(index, arg)| matches!(arg, Value::Callback(_)) && !runs.contains(&index));
    kept_callback
        || body.detached.iter().any(|function| {
            function.written_in.as_ref().is_some_and(|written| {
                written.site == call.site && written.constructed == construction
            })
        })
}

/// The constructor body a `new C(…)` or `super(…)` in `file` runs, in the
/// service of the run that holds C's declaration: the file and its key
/// (carrick#3527). Joined on the class the call's row names, at the line it
/// declares it on ([`FileIr::constructor_at`]). `None` for a class with no
/// constructor body, one declared outside the run, and anything else.
pub(super) fn constructed_across(
    inputs: &RequestSummaryInputs,
    siblings: &[&RequestSummaryInputs],
    file: &Path,
    call: &CallIr,
) -> Option<(PathBuf, String)> {
    if !call.constructs {
        return None;
    }
    let holder = inputs_holding(inputs, siblings, file);
    let row = row_at(holder, file, call)?;
    if !in_repo(row) {
        return None;
    }
    let declaring = holder.positions.as_ref()?.path_of(row.decl_file()?);
    let key = inputs_holding(inputs, siblings, &declaring)
        .files
        .get(&declaring)?
        .constructor_at(row.decl_line()?, row.symbol()?)?;
    Some((declaring, key))
}

/// The compiler's `call` row for `call` in `file`, from `inputs` (which
/// holds `file`).
fn row_at<'a>(inputs: &'a RequestSummaryInputs, file: &Path, call: &CallIr) -> Option<&'a Row> {
    row_of_kind(inputs, file, call, Kind::Call)
}

/// The row of `kind` at the call's span.
fn row_of_kind<'a>(
    inputs: &'a RequestSummaryInputs,
    file: &Path,
    call: &CallIr,
    kind: Kind,
) -> Option<&'a Row> {
    let positions = inputs.positions.as_ref()?;
    let ir = inputs.files.get(file)?;
    positions
        .lookup(
            file,
            ir.swc_start(call.site.lo),
            ir.swc_start(call.site.hi),
            kind,
        )
        .ok()
}

/// The binding `call`, written in `file`, is made on (carrick#2474 R1): the
/// row at the object its member is read on, `ident` for a name, `member` for
/// a member access, `call` for a call's value, and what holds it. A name is
/// a parameter when a function of its declaring file binds that name at the
/// line the row names. `None` for a call with no receiver. The decided
/// sites read the receiver of a scope call the same way (carrick#1678).
pub(super) fn receiver_at(
    inputs: &RequestSummaryInputs,
    file: &Path,
    call: &CallIr,
) -> Option<ReceiverAt> {
    let (lo, hi, form) = call.receiver_object?;
    let (kind, row_kind) = match form {
        ObjectForm::Name => (ReceiverKind::Binding, Some(Kind::Ident)),
        ObjectForm::Member => (ReceiverKind::Field, Some(Kind::Member)),
        ObjectForm::Call => (ReceiverKind::CallResult, Some(Kind::Call)),
        ObjectForm::Other => (ReceiverKind::Other, None),
    };
    let row = row_kind.and_then(|row_kind| {
        let positions = inputs.positions.as_ref()?;
        let ir = inputs.files.get(file)?;
        positions
            .lookup(file, ir.swc_start(lo), ir.swc_start(hi), row_kind)
            .ok()
    });
    let parameter = form == ObjectForm::Name
        && row.is_some_and(|row| {
            if in_repo(row)
                && let (Some(symbol), Some(decl_file), Some(line)) =
                    (row.symbol(), row.decl_file(), row.decl_line())
                && let Some(positions) = inputs.positions.as_ref()
                && let Some(declared) = inputs.files.get(&positions.path_of(decl_file))
            {
                declared
                    .declaration_lines
                    .params
                    .contains(&(line, symbol.to_string()))
            } else {
                false
            }
        });
    Some(ReceiverAt {
        symbol: row.and_then(|row| row.symbol()).map(str::to_string),
        decl_file: row.and_then(|row| row.decl_file()).map(str::to_string),
        decl_line: row.and_then(|row| row.decl_line()),
        kind: if parameter {
            ReceiverKind::Parameter
        } else {
            kind
        },
    })
}

struct Walker<'a> {
    inputs: &'a RequestSummaryInputs,
    /// The run's other services, in declared order (carrick#2618): the walk
    /// reads a file of one of them with that service's own inputs.
    siblings: &'a [&'a RequestSummaryInputs],
    labels: &'a Labels<'a>,
    root: &'a Path,
    /// Each call the library-site reader reads as made through an instance
    /// a package export's maker built (`const app = pkg(); app.post(…)`),
    /// by its file and SWC span ([`super::library_sites`]).
    instances: HashMap<(&'a Path, u32, u32), &'a LibrarySite>,
    /// What a callee that is a value may hold (carrick#2432, carrick#3504):
    /// the walk follows each end it reaches ([`Follow`]).
    values: &'a Resolver<'a>,
    memo: HashMap<(PathBuf, String), Outcome<'a>>,
    /// What each function written at a call reaches, by the function, read
    /// once final (carrick#3767): the walk of a call and the check for what
    /// it passed ([`Self::misses`]) both read the same callback, and each
    /// level of callbacks inside it would double the work again.
    callbacks: HashMap<*const FnIr, Outcome<'a>>,
    /// The definitions being read, by how deep.
    reading: HashMap<(PathBuf, String), usize>,
    /// Calls whose member declaration stands for more than one op
    /// (carrick#2474 R1, [`Self::one_op`]): no site.
    ambiguous: std::cell::Cell<usize>,
    /// The calls [`Discovery::declarations_unanswered`] lists.
    declarations_unanswered: std::cell::RefCell<BTreeSet<(String, u32, u32)>>,
    /// What each call of the service's own files entered, by file and span
    /// ([`Discovery::entered`]).
    entered: BTreeMap<(PathBuf, u32, u32), EnteredAt>,
}

/// A call's line and what it entered ([`Walker::entered`]).
type EnteredAt = (u32, BTreeSet<EnteredTarget>);

impl<'a> Walker<'a> {
    /// Note that `call` in `file` entered `function` of `target`, when the
    /// call is written in one of the service's own files.
    fn enter(
        &mut self,
        file: &Path,
        call: &CallIr,
        target: &Path,
        function: EnteredFunction,
        kind: EnteredKind,
    ) {
        if !self.inputs.files.contains_key(file) {
            return;
        }
        self.entered
            .entry((file.to_path_buf(), call.site.lo, call.site.hi))
            .or_insert_with(|| (call.site.line, BTreeSet::new()))
            .1
            .insert(EnteredTarget {
                file: target.to_path_buf(),
                function,
                kind,
            });
    }

    /// The inputs that hold `file`: this service's, else the first sibling's
    /// that does. Every read keyed by a file goes through this, so a body the
    /// walk enters in a sibling is read with the sibling's own IR and rows.
    fn inputs_for(&self, file: &Path) -> &'a RequestSummaryInputs {
        inputs_holding(self.inputs, self.siblings, file)
    }

    /// What the call `call` in `file` reaches ([`reach_across`]).
    fn reach(&self, file: &Path, call: &CallIr) -> Reach {
        reach_across(self.inputs, self.siblings, file, call)
    }

    fn rel(&self, file: &Path) -> String {
        let relative = file.strip_prefix(self.root).unwrap_or(file);
        relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    }

    fn hop(&self, file: &Path, key: &str) -> ChainHop {
        // A function written inside another is entered as a value is, at
        // the line its node starts on (carrick#2448 T3c): what decided
        // sites places a `<value>` hop by.
        if self.values.function(file, key).is_none()
            && let Some((_, written)) = self.written_inside(file, key)
            && let Some(site) = &written.site
        {
            return ChainHop {
                function: VALUE_HOP.to_string(),
                file: self.rel(file),
                line: site.line,
                made: None,
            };
        }
        let line = self
            .inputs_for(file)
            .files
            .get(file)
            .and_then(|ir| {
                ir.definition_lines
                    .iter()
                    .filter(|(_, keys)| keys.iter().any(|known| known == key))
                    .map(|(line, _)| *line)
                    .min()
                    // A constructor body, at its class (carrick#3527).
                    .or_else(|| {
                        ir.constructor_keys
                            .iter()
                            .filter(|(_, keys)| keys.iter().any(|(_, known)| known == key))
                            .map(|(line, _)| *line)
                            .min()
                    })
            })
            .unwrap_or_default();
        ChainHop {
            function: key.to_string(),
            file: self.rel(file),
            line,
            made: None,
        }
    }

    /// The labelled call `invoke` is, on `export` (and `member` of it) of
    /// `package`, as `claims` label it.
    fn end_at(
        &self,
        invoke: Invoke<'_>,
        package: &str,
        export: &str,
        member: Option<&str>,
        claims: &ExportClaims,
    ) -> SiteEnd {
        // A construction states an op only where the claim is written as a
        // `new` of this member (carrick#3439 T1); a maker's `new` is no op.
        let op = if invoke.construction {
            claims.claims.iter().find_map(|claim| match claim {
                LibraryClaim::Op {
                    op,
                    member: claimed,
                    path,
                    ..
                } if claim.is_new_form_op() && claimed.as_deref() == member && path.is_empty() => {
                    Some(*op)
                }
                _ => None,
            })
        } else {
            claims
                .claims
                .iter()
                .filter(|claim| claim.is_call_form())
                .find_map(|claim| match claim {
                    // An op claimed on what a scope member returns is read
                    // only where a verified declaration says so
                    // (carrick#1678), never by the member's name.
                    LibraryClaim::Op {
                        op,
                        member: claimed,
                        of,
                        ..
                    } if claimed.as_deref() == member
                        && of.as_deref().is_none_or(|of| scope_step(of).is_none()) =>
                    {
                        Some(*op)
                    }
                    _ => None,
                })
        };
        SiteEnd {
            package: package.to_string(),
            export: export.to_string(),
            member: member.map(str::to_string),
            role: claims.role,
            op,
            file: self.rel(invoke.file),
            line: invoke.call.site.line,
            span_start: invoke.call.site.span_start,
            span_end: invoke.call.site.span_end,
            new: invoke.construction,
            scope: None,
            procedure: None,
        }
    }

    /// The call row at `call`, when there are rows to read.
    fn row(&self, file: &Path, call: &CallIr) -> Option<&Row> {
        row_at(self.inputs_for(file), file, call)
    }

    /// The labelled export the compiler types `call`'s callee as
    /// (carrick#2448 T3b): the `typed_decl` row beside the call row names the
    /// function the callee's type is (`send: typeof fetch`), published under
    /// the store's key or a global of the language's library. Only where
    /// every value the resolver gave the callee is a hole: a value it read
    /// to a function or an export decides, whatever the callee is typed as.
    fn through_type(&self, invoke: Invoke<'_>, values: &[Held<'_>]) -> Option<Outcome<'a>> {
        let holes = !values.is_empty()
            && values.iter().all(|value| match value {
                Held::Reached(outcome) => !outcome.known,
                Held::Stop(_) => true,
                Held::Param(..) | Held::Callback => false,
            });
        if !holes || invoke.construction {
            return None;
        }
        let row = row_of_kind(
            self.inputs_for(invoke.file),
            invoke.file,
            invoke.call,
            Kind::TypedDecl,
        )?;
        let symbol = row.symbol()?;
        if in_repo(row) {
            return None;
        }
        let from_store = store_keys(row, self.labels.exports)
            .into_iter()
            .find_map(|package| {
                published_names(row).into_iter().find_map(|export| {
                    self.labels
                        .get(&package, export)
                        .map(|claims| self.end_at(invoke, &package, export, None, claims))
                })
            });
        let end = from_store.or_else(|| {
            let global = row
                .decl_file()
                .is_some_and(|declared| declared.starts_with("lib:"))
                .then(|| self.labels.global(symbol))
                .flatten()?;
            Some(self.end_at(invoke, &global.package, symbol, None, global))
        })?;
        Some(Outcome {
            ends: vec![Found {
                chain: Vec::new(),
                end,
                receiver: None,
                reached: Some(ReachedBy::Type),
                several: None,
                bound: Vec::new(),
            }],
            ..Outcome::default()
        })
    }

    /// The binding `call` is made on ([`receiver_at`]).
    fn receiver_of(&self, file: &Path, call: &CallIr) -> Option<ReceiverAt> {
        receiver_at(self.inputs, file, call)
    }

    /// The member a call reads through a computed key the code states as
    /// one text (`lib[k](…)`, `const k = "post"`): the compiler's row names
    /// the key, so the text is the member (carrick#3504 S4b).
    fn keyed_member(&self, file: &Path, call: &CallIr) -> Option<String> {
        match call.callee_key.as_ref()? {
            ValueAt::KeyOf { key, .. } if !matches!(**key, ValueAt::Lit(_)) => {
                self.values.one_text(file, key).ok().map(|(text, _)| text)
            }
            _ => None,
        }
    }

    /// A client procedure call (carrick#3186): `P.m1….mk.t(…)`,
    /// where `P`'s type is a proxy the store labels with a call-form op on
    /// member `t` named by the path walked on it (`name: { bound: "path" }`,
    /// claimed on the proxy's type, `of: "type:<package>#<Type>"`). The
    /// compiler's printed type of each object on the callee's chain says
    /// which ones are the proxy: the outermost is `P`, and every member read
    /// on one of them is a key of the path. The site's end is that op, and it
    /// names the procedure: the key path joined by the op's `join`, and the
    /// router [`super::decided_sites::router_holder`] finds at the first
    /// key's declaration.
    ///
    /// A proxy call whose key cannot be made is no site: its end is kept
    /// with the reason (K5), so the call is a lead that says why. The first
    /// key is declared outside the repository's source
    /// (`procedure_router_outside_repo`) or at more than one place
    /// (`procedure_router_ambiguous`), or a key is one the code does not
    /// name (`procedure_path_unread`). `None` for any other call.
    ///
    /// A call of a key the client's type lacks (carrick#3778 S0, a
    /// procedure the router no longer has): the compiler gives the call row
    /// no symbol and an error type, so the member is the callee's own name
    /// and the op is the one export of the store that claims it on the
    /// proxy's type ([`sole_path_op`]). Such a call is a procedure call only
    /// where [`Self::procedure_at`] finds a key absent; a call whose keys
    /// are all present (another op on a present procedure) is none
    /// ([`ProcedureCall::TypedWithoutKey`]).
    fn procedure_end(&self, file: &Path, call: &CallIr) -> Option<ProcedureCall> {
        let row = self.row(file, call)?;
        let absent = row.symbol().is_none() && row.flags() & FLAG_ERROR_TYPE != 0;
        let member = match row.symbol() {
            Some(symbol) => symbol,
            None if absent => call.callee.as_str(),
            None => return None,
        };
        if in_repo(row) {
            return None;
        }
        let packages = store_keys(row, self.labels.exports);
        let inputs = self.inputs_for(file);
        let ir = inputs.files.get(file)?;
        let positions = inputs.positions.as_ref()?;
        let at = |index: usize| {
            let (lo, hi, member, _) = call.receiver_spans[index];
            let kind = if member { Kind::Member } else { Kind::Ident };
            positions
                .lookup(file, ir.swc_start(lo), ir.swc_start(hi), kind)
                .ok()
        };
        let proxy_type = |index: usize| at(index)?.type_text().and_then(type_root);
        // The package alias the receiver was declared as, when the compiler
        // engine wrote one (`FLAG_DECLARED_TYPE`): the printed type of a
        // proxy is what its alias expands to, so the alias is read from the
        // declaration. Written at the proxy's own span only.
        let declared = |index: usize| {
            let (lo, hi, _, _) = call.receiver_spans[index];
            positions
                .receiver_declarations(file, ir.swc_start(lo), ir.swc_start(hi))
                .unwrap_or_default()
                .iter()
                .find(|row| row.flags() & FLAG_DECLARED_TYPE != 0)
        };
        // The innermost object on the chain whose type the store labels
        // with a path-named op on this member: by the printed type, else by
        // the declared one. `printed` is the root the proxy's own chain
        // prints as, none for a declared match. A call of an absent key
        // (S0) has no package of its own, so its op is the store's one
        // claim on the printed type ([`sole_path_op`]); it is read by the
        // printed type only.
        let (inner, export, join, printed) = (0..call.receiver_spans.len()).find_map(|index| {
            if let Some(root) = proxy_type(index) {
                let found = if absent {
                    sole_path_op(self.labels.exports, root, member)
                } else {
                    packages.iter().find_map(|package| {
                        let (export, join) = path_op(self.labels.exports, package, root, member)?;
                        Some((export, join))
                    })
                };
                if let Some((export, join)) = found {
                    return Some((index, export, join, Some(root)));
                }
            }
            if absent {
                return None;
            }
            let row = declared(index)?;
            let name = row.symbol()?;
            store_keys(row, self.labels.exports)
                .iter()
                .find_map(|package| {
                    let (export, join) = path_op(self.labels.exports, package, name, member)?;
                    Some((export, join))
                })
                .map(|(export, join)| (index, export, join, None))
        })?;
        let proxy = match printed {
            Some(root) => (inner..call.receiver_spans.len())
                .take_while(|index| proxy_type(*index) == Some(root))
                .last()
                .unwrap_or(inner),
            None => inner,
        };
        let invoke = Invoke {
            file,
            call,
            construction: false,
            bound: 0,
        };
        let claims = self.labels.get(&export.package, &export.export)?;
        let end = self.end_at(
            invoke,
            &export.package,
            &export.export,
            Some(member),
            claims,
        );
        match self.procedure_at(file, call, ir, positions, proxy, join.as_deref(), absent) {
            Ok(Some(procedure)) => Some(ProcedureCall::Site(
                Box::new(SiteEnd {
                    procedure: Some(procedure),
                    ..end
                }),
                None,
            )),
            Ok(None) if absent => Some(ProcedureCall::TypedWithoutKey),
            Ok(None) => None,
            Err(reason) => Some(ProcedureCall::Site(Box::new(end), Some(reason))),
        }
    }

    /// A client procedure call whose router type does not resolve
    /// (carrick#3847): the compiler types the call, and every key read on
    /// the client, as `any`, so [`Self::procedure_end`] finds no proxy and no
    /// key to name. The call's own site still says it is one: the receiver is
    /// an instance a maker builds, the members read on it are keys, and the
    /// member called is a path-named op of the maker's export. The end is
    /// kept with the reason, so the call is a lead that says why.
    fn untyped_procedure_end(&self, file: &Path, call: &CallIr) -> Option<(SiteEnd, Stop)> {
        let site = self
            .instances
            .get(&(file, call.site.span_start, call.site.span_end))?;
        let member = site.member.as_deref()?;
        if site.path.is_empty() {
            return None;
        }
        let exports = if site.export == "default" {
            vec!["default", "export ="]
        } else {
            vec![site.export.as_str()]
        };
        let invoke = Invoke {
            file,
            call,
            construction: false,
            bound: 0,
        };
        specifier_keys(&site.specifier).iter().find_map(|key| {
            exports.iter().find_map(|export| {
                let claims = self.labels.get(key, export)?;
                claims
                    .claims
                    .iter()
                    .any(|claim| is_path_named_call(claim, member))
                    .then(|| {
                        (
                            self.end_at(invoke, key, export, Some(member), claims),
                            Stop::UntypedReceiver,
                        )
                    })
            })
        })
    }

    /// The procedure `call` names through the proxy at `proxy` on its chain
    /// ([`Self::procedure_end`]): the keys read on the proxy, outermost
    /// first, joined by `join`, and the router its first key's declaration
    /// is held by.
    ///
    /// With `absent` (the call row has no symbol and an error type), a key
    /// whose row has no symbol and an error type is one the client's type
    /// lacks: its text is the key the code writes, and every key read on it
    /// is absent too. The first key absent names no router
    /// (`procedure_first_key_absent`). `None` when no key is absent: the
    /// call is no procedure call. Any other key with no symbol (an index
    /// signature, a client typed `any`) is `procedure_path_unread`.
    #[allow(clippy::too_many_arguments)]
    fn procedure_at(
        &self,
        file: &Path,
        call: &CallIr,
        ir: &FileIr,
        positions: &crate::position_rows::PositionIndex,
        proxy: usize,
        join: Option<&str>,
        absent: bool,
    ) -> Result<Option<ProcedureAt>, Stop> {
        let at = |index: usize| {
            let (lo, hi, _, _) = call.receiver_spans[index];
            positions
                .lookup(file, ir.swc_start(lo), ir.swc_start(hi), Kind::Member)
                .ok()
        };
        // The proxy itself is called: no key is read on it. A key the
        // compiler names no symbol for (`client[k]`) is no name either.
        if proxy == 0 {
            return if absent {
                Ok(None)
            } else {
                Err(Stop::ProcedurePathUnread)
            };
        }
        let mut keys: Vec<String> = Vec::with_capacity(proxy);
        let mut lacks = false;
        for index in (0..proxy).rev() {
            let row = at(index);
            if let Some(symbol) = row.and_then(Row::symbol) {
                keys.push(symbol.to_string());
                continue;
            }
            let lacking = absent && row.is_some_and(|row| row.flags() & FLAG_ERROR_TYPE != 0);
            match (lacking, &call.receiver_spans[index].3) {
                (true, Some(_)) if index == proxy - 1 => {
                    return Err(Stop::ProcedureFirstKeyAbsent);
                }
                (true, Some(key)) => {
                    lacks = true;
                    keys.push(key.clone());
                }
                _ => return Err(Stop::ProcedurePathUnread),
            }
        }
        if absent && !lacks {
            return Ok(None);
        }
        // The first key is present: an absent or unread one has stopped.
        let first = at(proxy - 1).ok_or(Stop::ProcedurePathUnread)?;
        let (lo, hi, _, _) = call.receiver_spans[proxy - 1];
        if positions
            .union_declarations(file, ir.swc_start(lo), ir.swc_start(hi))
            .is_ok_and(|declared| declared.len() > 1)
        {
            return Err(Stop::ProcedureRouterAmbiguous);
        }
        let (router_file, line) = in_repo(first)
            .then(|| first.decl_file().zip(first.decl_line()))
            .flatten()
            .ok_or(Stop::ProcedureRouterOutsideRepo)?;
        let binding = super::decided_sites::router_holder(self.root, router_file, line)
            .ok_or(Stop::ProcedureRouterOutsideRepo)?;
        Ok(Some(ProcedureAt {
            path: keys.join(join.unwrap_or(".")),
            file: router_file.to_string(),
            binding,
        }))
    }

    /// The labelled export `call` is made on, when it is one, and how it
    /// was matched:
    ///
    /// - the name the callee's package publishes the callee's declaration
    ///   as, when the row says: its own name, or `default` / `export =` for
    ///   the module's whole-module target; never a member's name;
    /// - the name the module publishes the binding the callee is read
    ///   through as (`default`, `io`, `export =`), as the store keys the
    ///   export: the compiler names the declaration (`axios`, `lookup`);
    /// - a global of the language's own library, by its name, wherever its
    ///   types declare it;
    /// - a receiver's symbol or type in the callee's package;
    /// - with the receiver trace on, a member whose declaration a verified
    ///   op claim resolves to, whatever holds the receiver
    ///   ([`MemberDeclarations`]). A declaration that stands for more than
    ///   one op ([`Self::one_op`]) is no site, and is counted.
    fn library_end(
        &self,
        file: &Path,
        call: &CallIr,
        construction: bool,
    ) -> Option<(SiteEnd, ReachedBy)> {
        let row = self.row(file, call)?;
        // `lib[k](…)`: the compiler's row names the key `k`, not the member,
        // so the member is the text the key states (carrick#3504 S4b).
        let keyed = self.keyed_member(file, call);
        let symbol = match keyed.as_deref() {
            Some(member) => member,
            None => row.symbol()?,
        };
        if in_repo(row) && keyed.is_none() {
            return None;
        }
        let invoke = Invoke {
            file,
            call,
            construction,
            bound: 0,
        };
        let end = |package: &str, export: &str, member: Option<&str>, claims: &ExportClaims| {
            self.end_at(invoke, package, export, member, claims)
        };
        // A keyed member has no declaration of its own to name a package.
        let packages = if keyed.is_none() {
            store_keys(row, self.labels.exports)
        } else {
            Vec::new()
        };
        for package in &packages {
            for export in published_names(row) {
                if let Some(claims) = self.labels.get(package, export) {
                    return Some((end(package, export, None, claims), ReachedBy::Name));
                }
            }
        }
        let inputs = self.inputs_for(file);
        let ir = inputs.files.get(file)?;
        // The binding the callee is read through: the callee itself, or the
        // name its member chain starts at.
        let root = root_name(call);
        let binding = root.and_then(|root| ir.bindings.get(root));
        if let Some(binding) = binding {
            let member = (!call.receiver_spans.is_empty()).then_some(symbol);
            let (exports, member) = match (binding.namespace, call.receiver_spans.len()) {
                // `ns.fork()`: the member, as written, is the export.
                (true, 1) => (vec![call.callee.as_str()], None),
                (true, _) => (Vec::new(), None),
                (false, _) if binding.export == "default" => (vec!["default", "export ="], member),
                (false, _) => (vec![binding.export.as_str()], member),
            };
            for key in specifier_keys(&binding.specifier) {
                for export in &exports {
                    if let Some(claims) = self.labels.get(&key, export) {
                        return Some((end(&key, export, member, claims), ReachedBy::Name));
                    }
                }
            }
        }
        if keyed.is_some() {
            return None;
        }
        // `fetch(…)`, `new WebSocket(…)`: a name no binding of the file
        // holds, declared by the language's own library or the runtime's
        // types, is the language's global.
        if is_free_name(ir, call)
            && (row
                .decl_file()
                .is_some_and(|declared| declared.starts_with("lib:"))
                || row.decl_package() == Some(RUNTIME_TYPES))
            && let Some(claims) = self.labels.global(symbol)
        {
            return Some((end(&claims.package, symbol, None, claims), ReachedBy::Name));
        }
        // `app.post(…)` where `const app = pkg()`: a member the store labels
        // on the receiver that maker builds (`instance:()`), the instance
        // read by the library-site reader, wherever it is held.
        if let Some(site) = self
            .instances
            .get(&(file, call.site.span_start, call.site.span_end))
            && site.path.is_empty()
            && let Some(member) = site.member.as_deref()
        {
            let receivers = site.receiver_ids();
            let exports = if site.export == "default" {
                vec!["default", "export ="]
            } else {
                vec![site.export.as_str()]
            };
            for key in specifier_keys(&site.specifier) {
                for export in &exports {
                    let Some(claims) = self.labels.get(&key, export) else {
                        continue;
                    };
                    let op = claims
                        .claims
                        .iter()
                        .filter(|claim| claim.is_call_form())
                        .find_map(|claim| match claim {
                            LibraryClaim::Op {
                                op,
                                member: claimed,
                                path,
                                on,
                                of,
                                ..
                            } if claimed.as_deref() == Some(member)
                                && path.is_empty()
                                && receivers
                                    .iter()
                                    .any(|receiver| applies(*on, of.as_deref(), receiver)) =>
                            {
                                Some(*op)
                            }
                            _ => None,
                        });
                    if let Some(op) = op {
                        return Some((
                            SiteEnd {
                                op: Some(op),
                                ..end(&key, export, Some(member), claims)
                            },
                            ReachedBy::Instance,
                        ));
                    }
                }
            }
        }
        if let Some(positions) = inputs.positions.as_ref() {
            for (lo, hi, member, _) in &call.receiver_spans {
                let kind = if *member { Kind::Member } else { Kind::Ident };
                let Ok(receiver) =
                    positions.lookup(file, ir.swc_start(*lo), ir.swc_start(*hi), kind)
                else {
                    break;
                };
                let root = receiver.type_text().and_then(type_root);
                for name in [receiver.symbol(), root].into_iter().flatten() {
                    for package in &packages {
                        if let Some(claims) = self.labels.get(package, name) {
                            return Some((
                                end(package, name, Some(symbol), claims),
                                ReachedBy::Name,
                            ));
                        }
                    }
                }
                // A member the class inherits is declared in its base's
                // package, so `packages` is the base's. The class the
                // receiver holds is declared elsewhere, and the compiler
                // engine writes it at the receiver as `receiver_type` rows
                // (one per class of an intersection or union; an alias is
                // already resolved). Read only where that export's claims
                // state this member as an op (carrick#3390). A class the
                // store does not label states nothing; two that both claim
                // the member state nothing, and are counted.
                let held = positions
                    .receiver_declarations(file, ir.swc_start(*lo), ir.swc_start(*hi))
                    .unwrap_or_default();
                if held.iter().any(|row| row.flags() & FLAG_TOO_MANY != 0) {
                    continue;
                }
                let mut claiming: Vec<(String, &str)> = Vec::new();
                for row in held {
                    // A declared type is how `procedure_end` reads a proxy;
                    // widening this read to it is its own measured change.
                    if row.flags() & FLAG_DECLARED_TYPE != 0 {
                        continue;
                    }
                    let Some(name) = row.symbol() else {
                        continue;
                    };
                    for package in store_keys(row, self.labels.exports) {
                        let stated = self
                            .labels
                            .get(&package, name)
                            .is_some_and(|claims| claims_member(claims, symbol));
                        if stated && !claiming.contains(&(package.clone(), name)) {
                            claiming.push((package, name));
                        }
                    }
                }
                match claiming.as_slice() {
                    [] => {}
                    [(package, name)] => {
                        if let Some(claims) = self.labels.get(package, name) {
                            return Some((
                                end(package, name, Some(symbol), claims),
                                ReachedBy::Name,
                            ));
                        }
                    }
                    _ => self.ambiguous.set(self.ambiguous.get() + 1),
                }
            }
        }
        // `app.get(…)` on an app handed in as a parameter, held in a field
        // or returned by a call: the member's declaration is one a verified
        // op claim resolves to (carrick#2474 R1).
        self.declared_op(row, invoke)
            .map(|end| (end, ReachedBy::Declaration))
    }

    /// The call `invoke` is when the member `row` reads is declared where a
    /// verified op claim resolves it ([`MemberDeclarations`]), whatever
    /// holds the receiver. A declaration that stands for more than one op
    /// ([`Self::one_op`]) is no site, and is counted. A construction states
    /// no declared op.
    fn declared_op(&self, row: &Row, invoke: Invoke<'_>) -> Option<SiteEnd> {
        let declarations = self.labels.declarations.filter(|_| !invoke.construction)?;
        if declarations.left_open(row, self.labels.exports) {
            self.declarations_unanswered.borrow_mut().insert((
                self.rel(invoke.file),
                invoke.call.site.span_start,
                invoke.call.site.span_end,
            ));
        }
        let ops = declarations.at(row.decl_file()?, row.decl_line()?);
        if ops.is_empty() {
            return None;
        }
        let Some(one) = self.one_op(ops) else {
            self.ambiguous.set(self.ambiguous.get() + 1);
            return None;
        };
        let claims = self.labels.get(&one.package, &one.export)?;
        Some(SiteEnd {
            op: Some(one.op),
            scope: one.scope.clone(),
            procedure: None,
            ..self.end_at(invoke, &one.package, &one.export, Some(&one.member), claims)
        })
    }

    /// The one op the ops a member declaration stands for are (carrick#3185):
    /// the only one, or the first of several that differ only in the export
    /// whose claims they are, where each of those exports claims the member
    /// the same way (the store labels a package's factory under every name
    /// it is published as, `default` and a named export, and both name the
    /// member's declaration), or where one claims it on the export and the
    /// other on its type, which differ only in the receiver they name
    /// (carrick#3044 S2). An op on what a scope member returns is one only
    /// alone: its export also picks the scope label it is read under.
    /// `None` where they disagree.
    fn one_op<'d>(&self, ops: &'d [DeclaredOp]) -> Option<&'d DeclaredOp> {
        let (first, rest) = ops.split_first()?;
        if rest.is_empty() {
            return Some(first);
        }
        let claimed = |op: &DeclaredOp| -> Option<Vec<LibraryClaim>> {
            let claims = self.labels.get(&op.package, &op.export)?;
            Some(
                claims
                    .claims
                    .iter()
                    .filter(|claim| {
                        matches!(claim, LibraryClaim::Op { member: Some(member), .. } if *member == op.member)
                    })
                    .map(|claim| claim.without_picker().without_receiver())
                    .collect(),
            )
        };
        let same = |a: &[LibraryClaim], b: &[LibraryClaim]| {
            a.iter().all(|claim| b.contains(claim)) && b.iter().all(|claim| a.contains(claim))
        };
        let reading = claimed(first)?;
        let agree = first.scope.is_none()
            && rest.iter().all(|op| {
                op.package == first.package
                    && op.member == first.member
                    && op.op == first.op
                    && op.scope.is_none()
                    && claimed(op).is_some_and(|other| same(&reading, &other))
            });
        agree.then_some(first)
    }

    /// Whether the callee is a value the repository declares: the call row
    /// names a declaration in one of its files (a parameter, a key of a type
    /// it writes, a local), or the callee is no name at all (`(a ?? b)(…)`),
    /// which no row names. A name or member the compiler cannot type stays
    /// what the rows say of it.
    fn declared_here(&self, file: &Path, call: &CallIr) -> bool {
        call.callee.is_empty() || self.row(file, call).is_some_and(in_repo)
    }

    /// Whether the call row names a member an interface or type literal of
    /// the service declares: no body here implements it.
    fn interface_member(&self, file: &Path, call: &CallIr) -> bool {
        let Some(row) = self.row(file, call) else {
            return false;
        };
        let (Some(symbol), Some(decl_file), Some(line)) =
            (row.symbol(), row.decl_file(), row.decl_line())
        else {
            return false;
        };
        let Some(positions) = self.inputs_for(file).positions.as_ref() else {
            return false;
        };
        let declared_in = positions.path_of(decl_file);
        in_repo(row)
            && self
                .inputs_for(&declared_in)
                .files
                .get(&declared_in)
                .and_then(|declared| declared.declaration_lines.signatures.get(&line))
                .is_some_and(|name| name == symbol)
    }

    /// Why `call`, which reached no labelled export and entered no code, was
    /// not followed: its callee is an in-repo value that is no definition, a
    /// function outside the service, or a member of a package the answer does
    /// not match or does not hold. `None` when nothing was missed.
    fn unfollowed(&self, file: &Path, call: &CallIr, construction: bool) -> Option<Stop> {
        match self.reach(file, call) {
            Reach::OutsideService(..) => Some(Stop::OutsideService),
            Reach::NoFunction => {
                let row = self.row(file, call)?;
                if in_repo(row) {
                    // A class built here has a constructor with no body to
                    // read; a value that is no definition is the gap.
                    return (!construction).then_some(Stop::ValueCallee);
                }
                // Missed only when every package the declaration stands for
                // misses it: one labelled none, or answered and matched,
                // reads it.
                let keys = store_keys(row, self.labels.exports);
                if keys
                    .iter()
                    .any(|key| self.labelled_none_export(file, call, row, key))
                {
                    return None;
                }
                let reasons: Vec<Option<Stop>> = keys
                    .iter()
                    .map(|key| self.labels.package_reason(key))
                    .collect();
                if reasons.iter().any(Option::is_none) {
                    return None;
                }
                reasons.into_iter().flatten().next()
            }
            _ => None,
        }
    }

    /// Whether the store labelled the export `call` is made on with no
    /// network role: by the name its package publishes the callee's
    /// declaration as, or the name the file's binding imports it by.
    fn labelled_none_export(&self, file: &Path, call: &CallIr, row: &Row, key: &str) -> bool {
        if published_names(row)
            .into_iter()
            .any(|export| self.labels.labelled_none(key, export))
        {
            return true;
        }
        let binding = root_name(call)
            .and_then(|root| self.inputs_for(file).files.get(file)?.bindings.get(root));
        binding.is_some_and(|binding| {
            specifier_keys(&binding.specifier).iter().any(|package| {
                [binding.export.as_str(), "default", "export ="]
                    .into_iter()
                    .any(|export| self.labels.labelled_none(package, export))
            })
        })
    }

    /// The definition each implementation in `found`, the sidecar's answer
    /// for a call in `file`, is: `None` for one the walk cannot enter (no
    /// symbol, or a declaration that is no function). One only tests import
    /// (a double) is left out. `None` when `file` has no rows.
    fn implementation_targets(
        &self,
        file: &Path,
        found: &[crate::services::type_sidecar::NamedDeclaration],
    ) -> Option<Vec<Option<(PathBuf, String)>>> {
        let positions = self.inputs_for(file).positions.as_ref()?;
        let mut targets = Vec::new();
        for named in found {
            let (Some(symbol), Some(decl_file)) =
                (named.symbol.as_deref(), named.decl_file.as_deref())
            else {
                targets.push(None);
                continue;
            };
            let declaring = positions.path_of(decl_file);
            let holder = self.inputs_for(&declaring);
            if holder.test_only.contains(&declaring) {
                continue;
            }
            targets.push(
                match holder.definition_at(&declaring, named.decl_line, symbol) {
                    Reach::Function(target, key) => Some((target, key)),
                    _ => None,
                },
            );
        }
        Some(targets)
    }

    /// The implementations the sidecar found for each call of the service
    /// through an interface member that the walk entered nothing from
    /// (carrick#2432 slice B), noted as entered without walking them: a
    /// member call on a parameter reads its values first, and one no caller
    /// fills stops there (`value_never_bound`) before its implementations
    /// are read. Only what [`Discovery::entered`] holds moves; no site, stop
    /// or end does.
    fn note_unread_implementations(&mut self) {
        let Some(implementations) = self.labels.implementations else {
            return;
        };
        let inputs = self.inputs;
        for (file, call, _) in super::row_calls::every_call(&inputs.files) {
            if self
                .entered
                .contains_key(&(file.to_path_buf(), call.site.lo, call.site.hi))
                || !self.interface_member(file, call)
            {
                continue;
            }
            let Some(Followed::Found(found)) = implementations.at(file, call.site.lo, call.site.hi)
            else {
                continue;
            };
            for (target, key) in self
                .implementation_targets(file, found)
                .into_iter()
                .flatten()
                .flatten()
            {
                self.enter(
                    file,
                    call,
                    &target,
                    EnteredFunction::Keyed(key),
                    EnteredKind::Implementation,
                );
            }
        }
    }

    /// What the call `call` through an interface member reaches, read through
    /// the implementations the sidecar found for it (carrick#2432 slice B):
    /// each is entered as the definition it is, and they agree on one
    /// labelled end or the call is a lead. An implementation only tests
    /// import (a double) is not one. `None` when the sidecar found none to
    /// follow, or the walk can enter none, so the call stays an
    /// `interface_member` stop.
    fn through_implementations(
        &mut self,
        file: &'a Path,
        call: &'a CallIr,
        invoke: Invoke<'a>,
    ) -> Option<Outcome<'a>> {
        let followed = self
            .labels
            .implementations?
            .at(file, call.site.lo, call.site.hi)?;
        let found = match followed {
            Followed::OverCap => return Some(Outcome::stopped(Stop::ImplementationsOverCap, file)),
            Followed::Found(found) => found,
        };
        let targets = self.implementation_targets(file, found)?;
        let mut values = Vec::new();
        // How many the walk entered. One it cannot enter (no symbol, or a
        // declaration that is no function: a `.d.ts` or a property) reaches
        // nothing, so beside an implementation that does reach a labelled
        // end it is a disagreement and the call is a lead, as for a value
        // that reaches none. When none can be entered the call keeps the
        // `interface_member` stop instead of reading as no site.
        let mut entered = 0;
        for target in targets {
            let reached = match target {
                Some((target, key)) => {
                    entered += 1;
                    self.enter(
                        file,
                        call,
                        &target,
                        EnteredFunction::Keyed(key.clone()),
                        EnteredKind::Implementation,
                    );
                    self.enter_value(target, key, invoke)
                }
                None => Outcome::default(),
            };
            values.push(Held::Reached(reached));
        }
        if entered == 0 {
            return None;
        }
        Some(self.agree(values, invoke, Vec::new(), OF_IMPLEMENTATIONS))
    }

    /// What `call`, written in `body` of `file`, reaches.
    fn call(
        &mut self,
        file: &'a Path,
        body: &'a FnIr,
        call: &'a CallIr,
        construction: bool,
    ) -> Outcome<'a> {
        let invoke = Invoke {
            file,
            call,
            construction,
            bound: 0,
        };
        // `f.call(t, a)` or `f.apply(t, [a])`, the row naming the language's
        // member (carrick#3504 S4d): the call runs `f`, followed through
        // each value it may hold, its parameters filled from the arguments
        // shifted past `this` ([`CallIr::passed_values`]). Read before the
        // labelled export the member chain starts at, whose member this
        // never is (`http.post.call(…)` is no `call` of `http`).
        if !construction && let Some(callee) = call.language_callee() {
            let resolver = self.values;
            let mut values = Vec::new();
            for value in callee {
                values.extend(resolver.read_value(self, file, value, invoke, false));
            }
            return self.agree(values, invoke, Vec::new(), OF_VALUES);
        }
        let procedure = (!construction)
            .then(|| self.procedure_end(file, call))
            .flatten();
        // A typed client's call of an op its type lacks is no call on an
        // untyped client: the untyped reader is for a router type that does
        // not resolve.
        let typed_without_key = matches!(procedure, Some(ProcedureCall::TypedWithoutKey));
        let procedure = match procedure {
            Some(ProcedureCall::Site(end, stop)) => Some((*end, stop)),
            Some(ProcedureCall::TypedWithoutKey) | None => None,
        };
        let mut several = procedure.as_ref().and_then(|(_, stop)| *stop);
        let mut found = procedure
            .map(|(end, _)| (end, ReachedBy::Type))
            .or_else(|| self.library_end(file, call, construction));
        if found.is_none()
            && !construction
            && !typed_without_key
            && let Some((end, stop)) = self.untyped_procedure_end(file, call)
        {
            several = Some(stop);
            found = Some((end, ReachedBy::Instance));
        }
        if let Some((end, reached)) = found {
            let traced = self.labels.declarations.is_some();
            return Outcome {
                ends: vec![Found {
                    chain: Vec::new(),
                    end,
                    receiver: traced.then(|| self.receiver_of(file, call)).flatten(),
                    reached: traced.then_some(reached),
                    several,
                    bound: Vec::new(),
                }],
                ..Outcome::default()
            };
        }
        // A callee that is a value (carrick#2432): followed through each
        // value it may hold, before the interface member that types it. A
        // member the compiler declares in a package or the language's
        // library (`res.json()` on a parameter) is that library's
        // ([`Self::declared_here`]).
        if !call.callee_values.is_empty()
            && !construction
            && !self.values.unbound_fields(file, call)
            && self.declared_here(file, call)
        {
            let resolver = self.values;
            let mut values = Vec::new();
            for value in &call.callee_values {
                values.extend(resolver.read_value(self, file, value, invoke, false));
            }
            if let Some(typed) = self.through_type(invoke, &values) {
                return typed;
            }
            return self.agree(values, invoke, Vec::new(), OF_VALUES);
        }
        if call.invokes_param.is_some() {
            return Outcome::stopped(Stop::InjectedCallee, file);
        }
        let reach = self.reach(file, call);
        // A member of an object the compiler declares in the repository and
        // no body implements, or one whose entry names no definition of its
        // file (`{ a: send }`): the function written at that key of each
        // object the callee's object may hold (carrick#2448, #3504 S4b).
        // Read before the module binding the row names, which for `d[k]()`
        // is the key's, not the callee's.
        if matches!(
            reach,
            Reach::NoFunction | Reach::Unknown(HoleReason::UnreadExpression)
        ) && !construction
            && let Some(values) = self.key_callee(file, call, invoke)
        {
            if let Some(typed) = self.through_type(invoke, &values) {
                return typed;
            }
            return self.agree(values, invoke, Vec::new(), OF_VALUES);
        }
        // A callee the compiler declares as a module binding (`const s =
        // send; s(…)`, a `let` written elsewhere): every value written to it
        // (carrick#3504 S1).
        if reach == Reach::NoFunction
            && !construction
            && let Some(values) = self.binding_callee(file, call, invoke)
        {
            if let Some(typed) = self.through_type(invoke, &values) {
                return typed;
            }
            return self.agree(values, invoke, Vec::new(), OF_VALUES);
        }
        match reach {
            Reach::Function(target, key) => {
                let hop = self.hop(&target, &key);
                let kind = if construction {
                    EnteredKind::Constructor
                } else {
                    EnteredKind::Named
                };
                self.enter(
                    file,
                    call,
                    &target,
                    EnteredFunction::Keyed(key.clone()),
                    kind,
                );
                let id = (target, key);
                let callee = self.function(&id.0, &id.1);
                let mut out = match self.reading.get(&id) {
                    // The walk came back to a definition it is reading.
                    Some(depth) => Outcome {
                        cycle_at: Some(*depth),
                        ..Outcome::stopped(Stop::Recursion, file)
                    },
                    None => {
                        let inner = self.definition(&id);
                        self.bind(inner, invoke, callee, None).under(hop.clone())
                    }
                };
                // A sibling's code is read without the instances its makers
                // build (carrick#2618): a walk into it that finds no site
                // has not read everything the call runs, as one that passed
                // a call it could not follow has not. The fact layer keeps
                // the candidate there, as it did when the call was outside
                // the service.
                if !self.inputs.files.contains_key(&id.0) {
                    out.missed = true;
                }
                // A function written at the call that the callee runs.
                let runs = callee
                    .map(|(_, callee)| callee.invoked_params.clone())
                    .unwrap_or_default();
                out.absorb(
                    self.handed(file, body, call, Some(&hop), |index| runs.contains(&index)),
                );
                // A function written at the call that the callee keeps and
                // does not run (a callback it only stores, a `handler` in an
                // object argument) runs later, from wherever the callee put
                // it: the walk has not read everything the call does
                // (carrick#3527).
                if keeps_handed(body, call, construction, &runs) {
                    out.missed = true;
                }
                if !out.ends.is_empty() {
                    out.drop_stop_for_ends(|read_in| self.inputs.files.contains_key(read_in));
                }
                out
            }
            Reach::OutsideService(..) => Outcome::default(),
            Reach::NoFunction if self.interface_member(file, call) => self
                .through_implementations(file, call, invoke)
                .unwrap_or_else(|| Outcome::stopped(Stop::InterfaceMember, file)),
            // A call into a library whose declaration returns what the
            // function it is handed returns (carrick#2634): the compiler's
            // row says so, and the function written at the call is walked.
            Reach::NoFunction
                if self
                    .row(file, call)
                    .is_some_and(|row| !in_repo(row) && row.flags() & FLAG_RUNS_HANDED != 0) =>
            {
                let mut out = self.handed(file, body, call, None, |_| true);
                out.absorb(self.handed_by_name(file, call));
                if !out.ends.is_empty() {
                    out.drop_stop_for_ends(|read_in| self.inputs.files.contains_key(read_in));
                }
                out
            }
            // No value read: the callee is a hole, typed as what it may be.
            Reach::NoFunction => self
                .through_type(invoke, &[Held::Reached(Outcome::default())])
                .unwrap_or_default(),
            Reach::Unknown(reason) => Outcome::stopped(Stop::from(reason), file),
        }
    }

    /// What the key `call`'s callee reads, `object.key`, holds in each
    /// object its object may hold (carrick#2448): `None` where the rows give
    /// the call no in-repo declaration, or the object is not one this reads.
    fn key_callee(
        &mut self,
        file: &'a Path,
        call: &'a CallIr,
        invoke: Invoke<'a>,
    ) -> Option<Vec<Held<'a>>> {
        let key = call.callee_key.as_ref()?;
        if !self.row(file, call).is_some_and(in_repo) {
            return None;
        }
        let resolver = self.values;
        resolver.key_values(self, file, key, invoke)
    }

    /// What the module binding `call`'s row names as its callee may hold
    /// (carrick#3504 S1): `None` where the row names no module binding of
    /// the run.
    fn binding_callee(
        &mut self,
        file: &'a Path,
        call: &'a CallIr,
        invoke: Invoke<'a>,
    ) -> Option<Vec<Held<'a>>> {
        let resolver = self.values;
        let (path, line, symbol) = resolver.declared_binding(self.row(file, call)?)?;
        resolver.binding_values(self, path, line, &symbol, invoke)
    }

    /// What the functions written at `call` reach, each at an argument
    /// position `runs` accepts, under a `<callback>` hop at its own line and
    /// under `hop` (the callee) when the callee is code of the run.
    fn handed(
        &mut self,
        file: &'a Path,
        body: &'a FnIr,
        call: &'a CallIr,
        hop: Option<&ChainHop>,
        runs: impl Fn(usize) -> bool,
    ) -> Outcome<'a> {
        let mut out = Outcome::default();
        for (index, arg) in call.args.iter().enumerate() {
            if let Value::Callback(nested) = arg
                && runs(index)
                && let Some(callback) = body.nested.get(*nested)
            {
                let written = ChainHop {
                    function: CALLBACK_HOP.to_string(),
                    file: self.rel(file),
                    line: callback
                        .site
                        .as_ref()
                        .map_or(call.site.line, |site| site.line),
                    made: None,
                };
                let inner = self.callback(file, callback);
                let mut inner = self.lifted(inner).under(written);
                if let Some(hop) = hop {
                    inner = inner.under(hop.clone());
                }
                out.absorb(inner);
            }
        }
        out
    }

    /// What the functions of the repository `call`, a library call that runs
    /// what it is handed, is handed by name reach (carrick#3442): each
    /// argument that is a name or member whose row names a definition of
    /// the run (`then(handle)`, `run(work)`), walked as a function written
    /// there is ([`Self::handed`]): under a `<callback>` hop at the call's
    /// line, its own parameters filled by the library, which no caller here
    /// states. Any other argument reads nothing here.
    fn handed_by_name(&mut self, file: &'a Path, call: &'a CallIr) -> Outcome<'a> {
        let mut out = Outcome::default();
        // The definitions the call hands on as a whole argument: the one
        // record ([`handed_by`], what `Calls::handed_on` holds, carrick#4176),
        // so the walk, the guard chains and the call graph's `handed` edge
        // follow the same rows.
        let mut handed: Vec<(usize, PathBuf, String)> =
            handed_by(self.inputs_for(file), file, call)
                .into_iter()
                .filter(|(_, _, index, keys)| {
                    keys.is_empty() && !matches!(call.args.get(*index), Some(Value::Callback(_)))
                })
                .map(|(target, key, index, _)| (index, target, key))
                .collect();
        // A member read (`deps.g`) is no name in the record (carrick#4176
        // R7): read here, as before.
        for (index, values) in call.arg_values.iter().enumerate() {
            if matches!(call.args.get(index), Some(Value::Callback(_))) {
                continue;
            }
            for value in values {
                let ValueAt::Read {
                    lo,
                    hi,
                    member: true,
                    ..
                } = value
                else {
                    continue;
                };
                if let Reach::Function(target, key) =
                    self.inputs_for(file)
                        .row_names(file, *lo, *hi, Kind::Member)
                {
                    handed.push((index, target, key));
                }
            }
        }
        handed.sort_by_key(|(index, ..)| *index);
        for (_, target, key) in handed {
            self.enter(
                file,
                call,
                &target,
                EnteredFunction::Keyed(key.clone()),
                EnteredKind::Handed,
            );
            let hop = self.hop(&target, &key);
            let id = (target, key);
            let inner = match self.reading.get(&id) {
                Some(depth) => Outcome {
                    cycle_at: Some(*depth),
                    ..Outcome::stopped(Stop::Recursion, file)
                },
                None => self.definition(&id),
            };
            let written = ChainHop {
                function: CALLBACK_HOP.to_string(),
                file: self.rel(file),
                line: call.site.line,
                made: None,
            };
            out.absorb(self.lifted(inner).under(hop).under(written));
        }
        out
    }

    /// What the definition `id` reaches, read once it is final.
    fn definition(&mut self, id: &(PathBuf, String)) -> Outcome<'a> {
        if let Some(known) = self.memo.get(id) {
            return known.clone();
        }
        // A definition the extractor keys whose body this pass did not read
        // (no keyed body and no function written where the key's node
        // starts): what it calls is not known, so the walk stops there and
        // says so.
        let Some((file, ir)) = self.function(&id.0, &id.1) else {
            // No file to say where: the stop is dropped beside ends, as it
            // was before carrick#4474.
            return Outcome {
                stop: Some((Vec::new(), Stop::NoBody)),
                ..Outcome::default()
            };
        };
        if ir.bodyless {
            return Outcome::stopped(Stop::InterfaceMember, file);
        }
        let depth = self.reading.len();
        self.reading.insert(id.clone(), depth);
        let mut out = self.body(file, ir);
        self.reading.remove(id);
        if out.cycle_at.is_none_or(|at| at >= depth) {
            out.cycle_at = None;
            // A cycle's stop names the functions down to where the walk came
            // back, which depends on where it started: read it again from
            // each start.
            let cycle_stop = out.ends.is_empty()
                && out
                    .stop
                    .as_ref()
                    .is_some_and(|(_, reason)| *reason == Stop::Recursion);
            // A definition read inside a resolver query over its step cap
            // is not all it reaches (carrick#3504 S1).
            if !cycle_stop && !self.values.capping() {
                self.memo.insert(id.clone(), out.clone());
            }
        }
        out
    }

    /// What the function written at a call, `callback`, reaches: [`Self::body`],
    /// read once it is final, as [`Self::definition`] keeps a definition's.
    fn callback(&mut self, file: &'a Path, callback: &'a FnIr) -> Outcome<'a> {
        let at = std::ptr::from_ref(callback);
        if let Some(known) = self.callbacks.get(&at) {
            return known.clone();
        }
        let depth = self.reading.len();
        let mut out = self.body(file, callback);
        if out.cycle_at.is_none_or(|cycle| cycle >= depth) {
            out.cycle_at = None;
            let cycle_stop = out.ends.is_empty()
                && out
                    .stop
                    .as_ref()
                    .is_some_and(|(_, reason)| *reason == Stop::Recursion);
            if !cycle_stop && !self.values.capping() {
                self.callbacks.insert(at, out.clone());
            }
        }
        out
    }

    /// What the calls `body` makes reach.
    fn body(&mut self, file: &'a Path, body: &'a FnIr) -> Outcome<'a> {
        let mut out = Outcome::default();
        let mut inners: Vec<(&'a CallIr, Outcome<'a>)> = Vec::new();
        for (call, construction) in body.read_calls() {
            let mut inner = self.call(file, body, call, construction);
            if self.misses(file, body, call, construction, &inner) {
                inner.missed = true;
            }
            if inner.ends.is_empty()
                && inner.stop.is_none()
                && self.unfollowed(file, call, construction) == Some(Stop::UnansweredPackage)
            {
                inner.unanswered.extend(self.unanswered_keys(file, call));
            }
            if inner.ends.is_empty() && self.declaration_left_open(file, call) {
                inner.declarations_open = true;
            }
            inners.push((call, inner));
        }
        // The maker ends (no op: a construction or a call) each call's own
        // walk found, by the call's span: the value a chained call reads.
        let made: HashMap<(u32, u32), Vec<(&str, &str)>> = inners
            .iter()
            .map(|(call, inner)| {
                let makers = inner
                    .ends
                    .iter()
                    .filter(|found| found.end.op.is_none())
                    .map(|found| (found.end.package.as_str(), found.end.export.as_str()))
                    .collect();
                ((call.site.lo, call.site.hi), makers)
            })
            .collect();
        let passed: Vec<bool> = inners
            .iter()
            .map(|(call, inner)| {
                // In a sibling's file only: the own service's chained calls
                // are read by the receiver trace.
                !self.inputs.files.contains_key(file)
                    && inner.ends.is_empty()
                    && inner.stop.is_none()
                    && self.chained_on_maker_op(file, call, &made)
            })
            .collect();
        for ((_, mut inner), passed_op) in inners.into_iter().zip(passed) {
            inner.passed_op |= passed_op;
            out.absorb(inner);
        }
        if !out.ends.is_empty() {
            out.drop_stop_for_ends(|read_in| self.inputs.files.contains_key(read_in));
        }
        out
    }

    /// Whether `call` is chained on the value of a call in the same body that
    /// `made` holds a maker end for, and that maker's export claims the call's
    /// member as an op (carrick#4445). The receiver is linked to the maker: a
    /// member name alone would give `window.addEventListener` a row beside a
    /// `BroadcastChannel` construction, because the DOM lib is one store key.
    fn chained_on_maker_op(
        &self,
        file: &Path,
        call: &CallIr,
        made: &HashMap<(u32, u32), Vec<(&str, &str)>>,
    ) -> bool {
        let Some(makers) = call
            .receiver_object
            .and_then(|(lo, hi, _)| made.get(&(lo, hi)))
        else {
            return false;
        };
        let Some(symbol) = self.row(file, call).and_then(|row| row.symbol()) else {
            return false;
        };
        makers.iter().any(|(package, export)| {
            self.labels
                .get(package, export)
                .is_some_and(|claims| claims_member(claims, symbol))
        })
    }

    /// Whether the walk read `call`'s member declaration for an op the
    /// receiver trace's dead ask left open ([`Self::declared_op`]).
    fn declaration_left_open(&self, file: &Path, call: &CallIr) -> bool {
        self.declarations_unanswered.borrow().contains(&(
            self.rel(file),
            call.site.span_start,
            call.site.span_end,
        ))
    }

    /// The store keys of `call`'s declaration that the answer holds nothing
    /// for.
    fn unanswered_keys(&self, file: &Path, call: &CallIr) -> Vec<String> {
        let Some(row) = self.row(file, call) else {
            return Vec::new();
        };
        store_keys(row, self.labels.exports)
            .into_iter()
            .filter(|key| self.labels.package_reason(key) == Some(Stop::UnansweredPackage))
            .collect()
    }

    /// Whether `call`, whose walk gave `outcome` and found no site, passed
    /// something it did not read (carrick#2613): a call it could not follow,
    /// or a function written at the call that no walk entered from it (a
    /// callback handed to a package's function) and that itself reaches a
    /// labelled export or stops short.
    fn misses(
        &mut self,
        file: &'a Path,
        body: &'a FnIr,
        call: &CallIr,
        construction: bool,
        outcome: &Outcome,
    ) -> bool {
        if !outcome.ends.is_empty() || outcome.stop.is_some() {
            return false;
        }
        if outcome.missed || self.unfollowed(file, call, construction).is_some() {
            return true;
        }
        call.args.iter().any(|arg| {
            let Value::Callback(nested) = arg else {
                return false;
            };
            let Some(callback) = body.nested.get(*nested) else {
                return false;
            };
            let read = self.callback(file, callback);
            !read.ends.is_empty() || read.stop.is_some() || read.missed
        })
    }

    /// The records for every call `body` and the functions written in it
    /// make. `body` is `depth` functions inside the definition `key` of
    /// `file` (`None` at module level): a value a parameter of that
    /// definition holds is read from its callers.
    fn sites(
        &mut self,
        file: &'a Path,
        key: Option<&'a str>,
        depth: u8,
        body: &'a FnIr,
        discovery: &mut Discovery,
    ) {
        for (call, construction) in body.read_calls() {
            if call.jsx {
                continue;
            }
            let outcome = self.call(file, body, call, construction);
            let outcome = self.settle_at_callers(outcome, file, key, depth);
            let missed = self.misses(file, body, call, construction, &outcome);
            let record =
                |end: Option<SiteEnd>, chain: Vec<ChainHop>, stop: Option<Stop>| DiscoverySite {
                    file: self.rel(file),
                    line: call.site.line,
                    span_start: call.site.span_start,
                    span_end: call.site.span_end,
                    callee: call.callee_text.clone(),
                    end,
                    chain,
                    stop,
                    also: Vec::new(),
                    receiver: None,
                    reached: None,
                    bound: Vec::new(),
                };
            if outcome.ends.is_empty() {
                // A walk that passed a call the dead declaration ask would
                // have decided is a call that ask left open (carrick#3991).
                if outcome.declarations_open {
                    self.declarations_unanswered.borrow_mut().insert((
                        self.rel(file),
                        call.site.span_start,
                        call.site.span_end,
                    ));
                }
                if !outcome.unanswered.is_empty() {
                    discovery.unanswered_at.insert(
                        (self.rel(file), call.site.span_start, call.site.span_end),
                        outcome.unanswered.clone(),
                    );
                }
                // A walk that entered code (a value's functions included)
                // and missed something inside is `missed_inside`, whatever
                // the call's own row reads (carrick#2813): the row's hole
                // would hide that the walk passed code it did not finish.
                let entered_missed = outcome.missed && outcome.stop.is_none();
                let (also, valued) = (outcome.also, outcome.valued);
                let Some((chain, stop)) = outcome.stop else {
                    let own = if entered_missed {
                        None
                    } else {
                        self.unfollowed(file, call, construction)
                    };
                    match own {
                        Some(reason) => {
                            *discovery
                                .counts
                                .entry(format!("unfollowed/{}", reason.as_str()))
                                .or_default() += 1;
                            discovery
                                .unfollowed
                                .push(record(None, Vec::new(), Some(reason)));
                        }
                        None => {
                            *discovery.counts.entry("no_site".to_string()).or_default() += 1;
                            let at = (self.rel(file), call.site.span_start, call.site.span_end);
                            if missed {
                                discovery
                                    .missed_at
                                    .insert(at.clone(), (call.site.line, call.callee_text.clone()));
                                discovery.missed_inside.push(at);
                            } else {
                                discovery.finished.push(at);
                            }
                        }
                    }
                    continue;
                };
                // A call whose own row says nothing: the walk never entered
                // code. Listed with the hole the row names. A stop the
                // callee's values agree on is the walk's, whatever its word.
                if chain.is_empty()
                    && !valued
                    && ![
                        Stop::InjectedCallee,
                        Stop::InterfaceMember,
                        Stop::SeveralValues,
                        Stop::ValuesOverCap,
                        Stop::SeveralImplementations,
                        Stop::ImplementationsOverCap,
                        Stop::ValueNeverBound,
                    ]
                    .contains(&stop)
                {
                    *discovery
                        .counts
                        .entry(format!("unread/{}", stop.as_str()))
                        .or_default() += 1;
                    discovery.unfollowed.push(record(None, chain, Some(stop)));
                    continue;
                }
                *discovery
                    .counts
                    .entry(format!("stop/{}", stop.as_str()))
                    .or_default() += 1;
                discovery.sites.push(DiscoverySite {
                    also,
                    ..record(None, chain, Some(stop))
                });
                continue;
            }
            // A walk that found ends but passed, in a sibling's file, a send
            // on a maker's value it read nothing at: the call passes through
            // a call Carrick did not read (carrick#4445), beside its sites.
            if outcome.passed_op {
                *discovery
                    .counts
                    .entry("missed_inside/beside_ends".to_string())
                    .or_default() += 1;
                let at = (self.rel(file), call.site.span_start, call.site.span_end);
                discovery
                    .missed_at
                    .insert(at.clone(), (call.site.line, call.callee_text.clone()));
                discovery.missed_inside.push(at);
            }
            let mut sibling_routes = 0;
            let mut sibling_end: Option<SiteEnd> = None;
            for found in outcome.ends {
                // A route registered in a sibling's code (a plugin another
                // service declares, registered here) is served by this
                // process, and the receiver trace writes it at the join point
                // (carrick#2474 R5b): a walk into a sibling states requests,
                // not routes. The call keeps its candidate as an unread one.
                if found.end.role == LibraryRole::ServerFramework
                    && !self
                        .inputs
                        .files
                        .contains_key(&self.root.join(&found.end.file))
                {
                    sibling_routes += 1;
                    sibling_end.get_or_insert_with(|| found.end.clone());
                    continue;
                }
                // One of the labelled exports a callee's disagreeing values
                // reach: the end is kept, with why it is no site.
                let stop = found.several;
                let count = match &stop {
                    Some(stop) => format!("stop/{}", stop.as_str()),
                    None => format!("site/{}", role_key(found.end.role)),
                };
                *discovery.counts.entry(count).or_default() += 1;
                discovery.sites.push(DiscoverySite {
                    receiver: found.receiver,
                    reached: found.reached,
                    bound: found.bound,
                    ..record(Some(found.end), found.chain, stop)
                });
            }
            if sibling_routes > 0 {
                *discovery
                    .counts
                    .entry("sibling_routes".to_string())
                    .or_default() += sibling_routes;
                let at = (self.rel(file), call.site.span_start, call.site.span_end);
                if !discovery.sites.iter().any(|site| {
                    site.file == at.0 && site.span_start == at.1 && site.span_end == at.2
                }) {
                    discovery
                        .missed_at
                        .insert(at.clone(), (call.site.line, call.callee_text.clone()));
                    if let Some(end) = sibling_end {
                        discovery.sibling_routes_at.insert(at.clone(), end);
                    }
                    discovery.missed_inside.push(at);
                }
            }
            // A stop the walk read in a sibling's code, beside the ends it
            // found there: no site walk of this service reads that call, so
            // it is saved at this one, as a call with no end beside it
            // saves it (carrick#4474).
            if let Some((chain, stop)) = outcome.beside {
                *discovery
                    .counts
                    .entry(format!("stop/{}/beside_ends", stop.as_str()))
                    .or_default() += 1;
                discovery.sites.push(record(None, chain, Some(stop)));
            }
        }
        for inner in body.nested.iter().chain(&body.detached) {
            self.sites(file, key, depth.saturating_add(1), inner, discovery);
        }
    }

    /// The definition `key` of `file`, the file as `inputs` keys it: its
    /// keyed body, else the function written inside another body that the
    /// key names ([`Self::written_inside`]).
    fn function(&self, file: &Path, key: &str) -> Option<(&'a Path, &'a FnIr)> {
        self.values
            .function(file, key)
            .or_else(|| self.written_inside(file, key))
    }

    /// The function the extractor keys as `key` of `file` when it is written
    /// inside another body, which keys no body of its own (carrick#2448
    /// T3c): `const request = async (o) => …` in a factory, a function
    /// declared in a function, an object's method written there. Found by
    /// where the key's function node starts, never by its name.
    fn written_inside(&self, file: &Path, key: &str) -> Option<(&'a Path, &'a FnIr)> {
        let (path, ir) = self.inputs_for(file).files.get_key_value(file)?;
        let (Some(start), _) = *ir.definitions.get(key)? else {
            return None;
        };
        Some((path.as_path(), self.values.written_function(file, start)?))
    }

    /// The labelled call `invoke` is when the name or member read at
    /// `lo..hi` in `file` holds a labelled export: by the name its package
    /// publishes the declaration the compiler's row there names, a
    /// global of the language's own library or the runtime's types, or a
    /// member whose declaration a verified op claim resolves to.
    fn value_end(
        &self,
        file: &Path,
        lo: u32,
        hi: u32,
        kind: Kind,
        invoke: Invoke<'_>,
    ) -> Option<SiteEnd> {
        let inputs = self.inputs_for(file);
        let positions = inputs.positions.as_ref()?;
        let ir = inputs.files.get(file)?;
        let row = positions
            .lookup(file, ir.swc_start(lo), ir.swc_start(hi), kind)
            .ok()?;
        let symbol = row.symbol()?;
        if in_repo(row) {
            return None;
        }
        for package in store_keys(row, self.labels.exports) {
            for export in published_names(row) {
                if let Some(claims) = self.labels.get(&package, export) {
                    return Some(self.end_at(invoke, &package, export, None, claims));
                }
            }
        }
        // A global the language's library declares, read by its name or as a
        // member of the global object (`globalThis.fetch`): the compiler's
        // row marks the declaration published under that name, which a
        // member of a class or an interface never is (carrick#2432 slice A2).
        if (kind == Kind::Ident || row.flags() & FLAG_PUBLISHED != 0)
            && (row
                .decl_file()
                .is_some_and(|declared| declared.starts_with("lib:"))
                || row.decl_package() == Some(RUNTIME_TYPES))
            && let Some(claims) = self.labels.global(symbol)
        {
            return Some(self.end_at(invoke, &claims.package, symbol, None, claims));
        }
        // `const post = http.post.bind(http)`: a member whose declaration a
        // verified op claim resolves to, held as a value.
        self.declared_op(row, invoke)
    }

    /// What `invoke` reaches when the value it runs is the definition `key`
    /// of `target`: read as a call of it is, `invoke` filling its
    /// parameters, under a `<value>` hop.
    fn enter_value(&mut self, target: PathBuf, key: String, invoke: Invoke<'a>) -> Outcome<'a> {
        let hop = ChainHop {
            function: VALUE_HOP.to_string(),
            ..self.hop(&target, &key)
        };
        let id = (target, key);
        if let Some(depth) = self.reading.get(&id) {
            return Outcome {
                cycle_at: Some(*depth),
                ..Outcome::stopped(Stop::Recursion, invoke.file)
            };
        }
        let inner = self.definition(&id);
        let callee = self.function(&id.0, &id.1);
        self.bind(inner, invoke, callee, None).under(hop)
    }

    /// `inner`, what a function `invoke` calls reaches, with each value its
    /// own parameters hold filled from what `invoke` passes there, or the
    /// default `callee` gives. A parameter one function out is the
    /// factory's that `maker` called to make the function (carrick#3504
    /// S0b), filled from what that call passes. What is left open is a
    /// parameter of the body that holds `invoke`.
    fn bind(
        &mut self,
        mut inner: Outcome<'a>,
        invoke: Invoke<'a>,
        callee: Option<(&'a Path, &'a FnIr)>,
        maker: Option<Maker<'a>>,
    ) -> Outcome<'a> {
        for open in std::mem::take(&mut inner.open) {
            let mut values = open.known;
            let mut spread = open.spread;
            for (depth, index, keys) in open.refs {
                if depth == 1
                    && let Some(maker) = maker
                {
                    let resolver = self.values;
                    let filled = resolver.passed(
                        self,
                        Some(maker.call),
                        index,
                        &keys,
                        defaults_of(Some(maker.factory)),
                        open.invoke,
                    );
                    for value in filled {
                        values.push(match (value, maker.out) {
                            // A parameter of the body the maker call is
                            // written in, `out` functions out of this one.
                            (Held::Param(depth, index, keys), Some(out)) => {
                                Held::Param(depth.saturating_add(out), index, keys)
                            }
                            (Held::Param(..), None) => Held::Reached(Outcome::default()),
                            (other, _) => other,
                        });
                    }
                    continue;
                }
                if depth != 0 {
                    // A parameter of the function the callee is written in:
                    // the callee's own caller does not fill it.
                    values.push(Held::Reached(Outcome::default()));
                    continue;
                }
                let resolver = self.values;
                values.extend(resolver.passed_noting(
                    self,
                    Some(invoke),
                    index,
                    &keys,
                    defaults_of(callee),
                    open.invoke,
                    &mut spread,
                ));
            }
            let mut settled = self.agree(values, open.invoke, open.chain, OF_VALUES);
            for open in &mut settled.open {
                open.spread |= spread;
            }
            inner.absorb(settled);
        }
        inner
    }

    /// `inner`, what a function written in a body reaches, as that body
    /// reads it: its own parameters are whatever runs it fills, and each
    /// parameter of an enclosing function is one function nearer.
    fn lifted(&mut self, mut inner: Outcome<'a>) -> Outcome<'a> {
        for mut open in std::mem::take(&mut inner.open) {
            let mut refs = Vec::new();
            for (depth, index, keys) in open.refs {
                match depth.checked_sub(1) {
                    Some(depth) => refs.push((depth, index, keys)),
                    None => open.known.push(Held::Callback),
                }
            }
            if refs.is_empty() {
                let settled = self.agree(open.known, open.invoke, open.chain, OF_VALUES);
                inner.absorb(settled);
            } else {
                open.refs = refs;
                inner.open.push(open);
            }
        }
        inner
    }

    /// One outcome for the values a callee may hold (carrick#2432), the
    /// call `invoke` running it: every value reaches the same labelled
    /// exports, as the store names them, and those are its ends. Values
    /// that reach different ones, or one that reaches none, are one of
    /// several (`several_values`); no value at all is `value_never_bound`;
    /// more than [`MAX_VALUES`] is `values_over_cap`; only callbacks stay
    /// `injected_callee`, what runs them decides. A parameter among them
    /// stays open for the callers of the body that holds `invoke`.
    fn agree(
        &mut self,
        values: Vec<Held<'a>>,
        invoke: Invoke<'a>,
        chain: Vec<ChainHop>,
        plural: Plural,
    ) -> Outcome<'a> {
        let (refs, known): (Vec<_>, Vec<_>) = values
            .into_iter()
            .partition(|value| matches!(value, Held::Param(..)));
        if !refs.is_empty() {
            let refs = refs
                .into_iter()
                .filter_map(|value| match value {
                    Held::Param(depth, index, keys) => Some((depth, index, keys)),
                    _ => None,
                })
                .collect();
            return Outcome {
                open: vec![Open {
                    refs,
                    known,
                    invoke,
                    chain,
                    spread: false,
                }],
                ..Outcome::default()
            };
        }
        if known.is_empty() {
            return Outcome::stopped(Stop::ValueNeverBound, invoke.file).below(&chain);
        }
        if known.len() > MAX_VALUES {
            return Outcome::stopped(plural.over_cap, invoke.file).below(&chain);
        }
        if known.iter().all(|value| matches!(value, Held::Callback)) {
            return Outcome::stopped(Stop::InjectedCallee, invoke.file).below(&chain);
        }
        // Why each value reached nothing, where it says: the resolver's
        // reason, a callback's, or the stop the walk met inside it. A value
        // that reached nothing and says no reason is `None`.
        let mut reasons: Vec<Option<Stop>> = Vec::new();
        let mut outcomes: Vec<Outcome<'a>> = known
            .into_iter()
            .map(|value| match value {
                Held::Reached(outcome) => {
                    if outcome.ends.is_empty() {
                        reasons.push(outcome.stop.as_ref().map(|(_, stop)| *stop));
                    }
                    outcome
                }
                Held::Stop(stop) => {
                    reasons.push(Some(stop));
                    Outcome::default()
                }
                Held::Callback => {
                    reasons.push(Some(Stop::InjectedCallee));
                    Outcome::default()
                }
                Held::Param(..) => Outcome::default(),
            })
            .collect();
        if outcomes.len() == 1 {
            return outcomes.remove(0).below(&chain);
        }
        let first = outcomes[0].labels();
        let agree = !first.is_empty() && outcomes.iter().all(|outcome| outcome.labels() == first);
        let mut out = Outcome::default();
        for mut outcome in outcomes {
            if !agree {
                for found in &mut outcome.ends {
                    found.several = Some(plural.several);
                }
                outcome.stop = None;
            }
            out.absorb(outcome);
        }
        if !agree && out.ends.is_empty() {
            // No value reached a labelled export: every one stopped one
            // way is that way; several ways, or one that says none, are
            // one of several, with the ways named (carrick#3688 U2, R3).
            let (stop, also) = agreed_stop(&reasons, plural.several);
            out.stop = Some((Vec::new(), stop));
            out.stop_read_in = Some(invoke.file);
            out.also = also;
            out.valued = true;
        }
        out.below(&chain)
    }

    /// `outcome`, read at a call of the body `depth` functions inside the
    /// definition `key` of `file`, with each value left open read from what
    /// the definition's callers pass, one caller up at a time until a value
    /// is no parameter. A cycle ends.
    fn settle_at_callers(
        &mut self,
        mut outcome: Outcome<'a>,
        file: &'a Path,
        key: Option<&'a str>,
        depth: u8,
    ) -> Outcome<'a> {
        let resolver = self.values;
        for open in std::mem::take(&mut outcome.open) {
            let mut values = open.known;
            let mut unfilled = 0;
            let mut handed = false;
            for (out, index, keys) in &open.refs {
                let before = unfilled;
                let held = resolver.callers_pass_noting(
                    self,
                    (file, key, depth),
                    (*out, *index, keys),
                    open.invoke,
                    &mut Vec::new(),
                    &mut unfilled,
                    &mut handed,
                );
                note_unfilled(&mut unfilled, before, open.spread, &held);
                values.extend(held);
            }
            // A key written through a spread of a parameter its callers do
            // not fill, and nothing else held: each write keeps the reading
            // it had before ruling R-SP (carrick#4025).
            unread_where_spread_unfilled(&mut values, unfilled);
            // Nothing held, and a function on the way is handed on as a
            // value to a function of the service: the call that runs it is
            // not read, and may bind the callee (carrick#4025). Nothing binds
            // it is no answer. The callee's values decide it, so it is kept
            // among the sites, as `value_never_bound` was.
            if values.is_empty() && handed {
                let stopped = Outcome {
                    valued: true,
                    ..Outcome::stopped(Stop::CallbackIntoOwnFunction, open.invoke.file)
                };
                outcome.absorb(stopped.below(&open.chain));
                continue;
            }
            let settled = self.agree(values, open.invoke, open.chain, OF_VALUES);
            outcome.absorb(settled);
        }
        if !outcome.ends.is_empty() {
            outcome.drop_stop_for_ends(|read_in| self.inputs.files.contains_key(read_in));
        }
        outcome
    }
}

/// What the walk makes of each end the value resolver reaches: a labelled
/// export is the labelled call `invoke` itself; a function of the
/// repository is read as a call of it is, `invoke` filling its parameters.
impl<'a> Follow<'a> for Walker<'a> {
    type Reached = Outcome<'a>;

    fn export(
        &mut self,
        file: &'a Path,
        lo: u32,
        hi: u32,
        kind: Kind,
        invoke: Invoke<'a>,
    ) -> Option<Outcome<'a>> {
        let end = self.value_end(file, lo, hi, kind, invoke)?;
        Some(Outcome {
            ends: vec![Found {
                chain: Vec::new(),
                end,
                receiver: None,
                reached: None,
                several: None,
                bound: Vec::new(),
            }],
            known: true,
            ..Outcome::default()
        })
    }

    fn bound(
        &mut self,
        mut reached: Outcome<'a>,
        invoke: Invoke<'a>,
        bind: (&'a Path, &CallIr),
    ) -> Outcome<'a> {
        let (bind_file, bind) = bind;
        let at = BoundCall {
            file: self.rel(invoke.file),
            span_start: invoke.call.site.span_start,
            span_end: invoke.call.site.span_end,
            bind_file: self.rel(bind_file),
            bind_start: bind.site.span_start,
            bind_end: bind.site.span_end,
        };
        for found in &mut reached.ends {
            found.bound.push(at.clone());
        }
        reached
    }

    fn definition(&mut self, target: PathBuf, key: String, invoke: Invoke<'a>) -> Outcome<'a> {
        self.enter(
            invoke.file,
            invoke.call,
            &target,
            EnteredFunction::Keyed(key.clone()),
            EnteredKind::Value,
        );
        Outcome {
            known: true,
            ..self.enter_value(target, key, invoke)
        }
    }

    fn written(
        &mut self,
        file: &'a Path,
        function: &'a FnIr,
        invoke: Invoke<'a>,
        maker: Option<Maker<'a>>,
    ) -> Outcome<'a> {
        let line = function.site.as_ref().map_or(0, |site| site.line);
        if function.site.is_some() {
            self.enter(
                invoke.file,
                invoke.call,
                file,
                EnteredFunction::Written { line },
                EnteredKind::Value,
            );
        }
        let hop = ChainHop {
            function: VALUE_HOP.to_string(),
            file: self.rel(file),
            line,
            made: maker.map(|maker| HopMaker {
                file: self.rel(maker.call.file),
                span_start: maker.call.call.site.span_start,
                span_end: maker.call.call.site.span_end,
            }),
        };
        let inner = self.body(file, function);
        Outcome {
            known: true,
            ..self
                .bind(inner, invoke, Some((file, function)), maker)
                .under(hop)
        }
    }
}

fn role_key(role: LibraryRole) -> String {
    serde_json::to_value(role)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The most functions a call's callee may hold for the call graph to write
/// an edge to each: the walk's own cap on a value's values.
pub const MAX_ENTERED_PER_CALL: usize = MAX_VALUES;

/// What each call of the service `inputs` reads entered, read by a walk
/// that labels nothing (no store answer, no siblings), following the
/// interface members `implementations` answers: deterministic over the code
/// and its rows, whatever the store holds. The call graph's edges
/// ([`crate::call_graph::plan_edges`]). Empty without rows.
pub fn entered_calls(
    inputs: &RequestSummaryInputs,
    implementations: &Implementations,
    root: &Path,
) -> Vec<Entered> {
    let labels = Labels::of(&[]).following(implementations);
    discover_with(inputs, &[], &labels, root).entered
}

/// Every site of the service `inputs` reads, with the store's answer
/// `labels`, paths relative to `root`. Nothing without rows.
pub fn discover(inputs: &RequestSummaryInputs, labels: &Labels<'_>, root: &Path) -> Discovery {
    discover_with(inputs, &[], labels, root)
}

/// [`discover`], with the run's other services `siblings` in declared order
/// (carrick#2618): a call into a function declared in a file of one of them
/// is followed in that file, read with the sibling's own IR and rows. Records
/// are written for this service's own files only.
pub fn discover_with(
    inputs: &RequestSummaryInputs,
    siblings: &[&RequestSummaryInputs],
    labels: &Labels<'_>,
    root: &Path,
) -> Discovery {
    let mut discovery = Discovery::default();
    if inputs.positions.is_none() {
        return discovery;
    }
    let library = super::library_sites(inputs);
    let instances = library
        .sites
        .iter()
        .filter(|site| site.origin.is_none() && !site.makers().is_empty())
        .map(|site| ((site.file.as_path(), site.span_start, site.span_end), site))
        .collect();
    let values = Resolver::new(inputs, siblings);
    let mut walker = Walker {
        inputs,
        siblings,
        labels,
        root,
        instances,
        values: &values,
        memo: HashMap::new(),
        callbacks: HashMap::new(),
        reading: HashMap::new(),
        ambiguous: std::cell::Cell::new(0),
        declarations_unanswered: std::cell::RefCell::new(BTreeSet::new()),
        entered: BTreeMap::new(),
    };
    let mut paths: Vec<&PathBuf> = inputs.files.keys().collect();
    paths.sort();
    for path in paths {
        let file = &inputs.files[path];
        let mut keys: Vec<&String> = file.functions.keys().collect();
        keys.sort();
        for key in keys {
            walker.sites(
                path,
                Some(key.as_str()),
                0,
                &file.functions[key],
                &mut discovery,
            );
        }
        walker.sites(path, None, 0, &file.module_level, &mut discovery);
    }
    // The value resolver's step census (carrick#3504 S0): what sets its
    // step cap, read on the replay box. Off by default; never in the blob.
    if std::env::var_os("CARRICK_RESOLVER_CENSUS").is_some() {
        let census = values.census();
        crate::errln!(
            "carrick-resolver-census root={} files={} queries={} max_query_steps={} steps={} capped_queries={}",
            root.display(),
            inputs.files.len(),
            census.queries,
            census.max_query_steps,
            census.steps,
            census.capped_queries
        );
    }
    walker.note_unread_implementations();
    discovery.entered = std::mem::take(&mut walker.entered)
        .into_iter()
        .map(|((file, lo, hi), (line, targets))| Entered {
            file,
            lo,
            hi,
            line,
            targets,
        })
        .collect();
    discovery.declarations_unanswered = walker.declarations_unanswered.take();
    let ambiguous = walker.ambiguous.get();
    if ambiguous > 0 {
        discovery
            .counts
            .insert("declaration_ambiguous".to_string(), ambiguous);
    }
    discovery
        .sites
        .sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
    discovery.sites.dedup();
    discovery
        .unfollowed
        .sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
    discovery.unfollowed.dedup();
    discovery.finished.sort();
    discovery.finished.dedup();
    discovery.missed_inside.sort();
    discovery.missed_inside.dedup();
    discovery
}

fn sort_key(site: &DiscoverySite) -> (&str, u32, u32, Option<(&str, u32, u32)>) {
    (
        &site.file,
        site.span_start,
        site.span_end,
        site.end
            .as_ref()
            .map(|end| (end.file.as_str(), end.span_start, end.span_end)),
    )
}

#[cfg(test)]
mod tests {
    use super::super::fixture_walk::walked;
    use super::*;

    /// carrick#2504: a declaration in a default-library file is keyed as the
    /// store keys the language's library, `lib:<file>`, not by the file's
    /// stem.
    #[test]
    fn a_default_library_declaration_is_keyed_lib_colon_file() {
        let row = |decl: &str| {
            Row(
                crate::position_rows::Kind::Call,
                0,
                5,
                Some("fetch".into()),
                Some(decl.into()),
                Some(1),
                None,
                None,
                0,
            )
        };
        assert_eq!(
            store_keys(&row("lib:lib.dom.d.ts"), &[]),
            ["lib:lib.dom.d.ts"]
        );
        assert_eq!(
            store_keys(&row("lib:lib.es2015.promise.d.ts"), &[]),
            ["lib:lib.es2015.promise.d.ts"]
        );
    }

    /// `levels` callbacks, each handed to a promise's `then` inside the last,
    /// the innermost reaching a call.
    fn nested_callbacks(levels: usize) -> String {
        let mut body = "return fetch(\"/api/leaf\");".to_string();
        for level in 0..levels {
            body = format!("return Promise.resolve({level}).then((v{level}) => {{ {body} }});");
        }
        format!("export function go() {{\n  {body}\n}}\n")
    }

    /// carrick#3767: a callback handed to a call is walked once, so the time
    /// discovery takes grows with the nesting, not with two to its power.
    #[test]
    fn nested_callbacks_are_walked_once() {
        let read = |levels: usize| {
            let source = nested_callbacks(levels);
            let on = walked(
                "discovery-nested-callbacks",
                ".",
                "web",
                &[("src/index.ts", &source)],
                true,
            )?;
            let started = std::time::Instant::now();
            let discovery = discover(&on.inputs, &Labels::of(&[]), &on.repo);
            Some((discovery, started.elapsed()))
        };
        let (Some((small, _)), Some((deep, took))) = (read(3), read(24)) else {
            return;
        };
        // Every call is read, to the end, at either depth: a call for each
        // level's `resolve` and `then`, and the one inside.
        for (found, levels) in [(&small, 3), (&deep, 24)] {
            assert_eq!(found.finished.len(), 2 * levels + 1);
            assert!(found.missed_inside.is_empty());
            assert!(found.sites.is_empty());
            assert_eq!(found.counts["no_site"], 2 * levels + 1);
        }
        assert!(took < std::time::Duration::from_secs(3), "took {took:?}");
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/discovery")
            .join(name)
    }

    fn answer() -> LibraryAnswer {
        let text = std::fs::read_to_string(fixture("library-claims.json")).unwrap();
        let answer = crate::library_store::parse_answer(&text);
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        answer
    }

    fn claims() -> Vec<ExportClaims> {
        answer().exports
    }

    /// carrick#3778 S0: a call the compiler gives no declaration names its op
    /// by the proxy's type alone, so only the one export of the store that
    /// claims that op names it; two exports claiming it name none.
    #[test]
    fn a_path_op_two_exports_claim_names_no_op_by_type_alone() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/procedure-absent/library-claims.json");
        let answer = crate::library_store::parse_answer(&std::fs::read_to_string(path).unwrap());
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        let mut exports = answer.exports;
        let (one, join) =
            sole_path_op(&exports, "ClientOf", "query").expect("one export claims it");
        assert_eq!(
            (one.package.as_str(), one.export.as_str(), join.as_deref()),
            ("fixture-procs", "createClient", Some("."))
        );
        assert!(sole_path_op(&exports, "ClientOf", "subscribe").is_none());
        assert!(sole_path_op(&exports, "HooksOf", "query").is_none());
        let mut twin = exports
            .iter()
            .find(|export| export.export == "createClient")
            .unwrap()
            .clone();
        twin.export = "createTwin".to_string();
        exports.push(twin);
        assert!(sole_path_op(&exports, "ClientOf", "query").is_none());
        assert!(path_op(&exports, "fixture-procs", "ClientOf", "query").is_some());
    }

    fn sites_of(discovery: &Discovery) -> serde_json::Value {
        serde_json::json!({ "sites": discovery.sites })
    }

    /// carrick#2431: every call that reaches a labelled export, for every
    /// protocol, at any depth, is a site with its end and its chain: an HTTP
    /// helper three files deep, a seven-hop chain, an arrow method, a message
    /// through an in-repo bus over a child-process channel, a socket send
    /// through a session class, a broker publish behind a wrapper, a
    /// callback an in-repo tracer runs. A closure a library's store
    /// initialiser is handed is not followed, a cycle ends, and an interface
    /// member and an injected callee are stops. The answer key is the shared
    /// fixture #2433 and #2435 read.
    #[test]
    fn every_call_that_reaches_a_labelled_export_is_a_site() {
        let Some(on) = walked("discovery", "services/web", "web", &[], true) else {
            return;
        };
        let exports = claims();
        let discovery = discover(&on.inputs, &Labels::of(&exports), &on.repo);
        let expected: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(fixture("sites.expected.json")).unwrap())
                .unwrap();
        let got = sites_of(&discovery);
        if got["sites"] != expected["sites"] {
            let got: Vec<String> = discovery
                .sites
                .iter()
                .map(|site| serde_json::to_string(site).unwrap())
                .collect();
            let want: Vec<String> = expected["sites"]
                .as_array()
                .unwrap()
                .iter()
                .map(|site| {
                    serde_json::to_string(
                        &serde_json::from_value::<DiscoverySite>(site.clone()).unwrap(),
                    )
                    .unwrap()
                })
                .collect();
            let extra: Vec<&String> = got.iter().filter(|s| !want.contains(s)).collect();
            let missing: Vec<&String> = want.iter().filter(|s| !got.contains(s)).collect();
            panic!("extra: {extra:#?}\nmissing: {missing:#?}");
        }
    }

    /// carrick#2432 slice B: a call through an interface member is followed
    /// into the implementations the sidecar finds for it. One implementing
    /// class sending to a labelled end is a site; two that send to different
    /// ends are a lead (`several_implementations`, each end kept); a table's
    /// entry calling a member of a module-level interface-typed value reads
    /// that member's implementation. Without the ask each call is the
    /// `interface_member` stop it was.
    #[test]
    fn a_call_through_an_interface_member_follows_its_implementations() {
        let Some(on) = walked("implementations", "services/web", "web", &[], true) else {
            return;
        };
        let script = super::super::fixture_walk::sidecar().expect("built above");
        let exports = claims();
        let asked = super::super::fixture_walk::implementations_of(&script, &on, "services/web");
        let walk = |follow: Option<&Implementations>| {
            let labels = match follow {
                Some(found) => Labels::of(&exports).following(found),
                None => Labels::of(&exports),
            };
            discover(&on.inputs, &labels, &on.repo)
        };
        // Each record of the call `callee` in `file`, as `end line/stop`.
        let records = |discovery: &Discovery, file: &str, callee: &str| {
            let mut out: Vec<String> = discovery
                .sites
                .iter()
                .filter(|site| site.file.ends_with(file) && site.callee == callee)
                .map(|site| {
                    let end = site.end.as_ref().map_or("-".to_string(), |end| {
                        format!(
                            "{}.{}{} @{}",
                            end.package,
                            end.export,
                            end.member
                                .as_deref()
                                .map_or(String::new(), |member| format!(".{member}")),
                            end.line
                        )
                    });
                    format!("{end} / {}", site.stop.map_or("site", Stop::as_str))
                })
                .collect();
            out.sort();
            out
        };
        let with = walk(Some(&asked));
        assert_eq!(
            records(&with, "one-impl.ts", "notifier.notify"),
            ["lib:lib.dom.d.ts.fetch @7 / site"],
            "one implementation sending to a labelled end is a site"
        );
        assert_eq!(
            records(&with, "two-impls.ts", "channels[kind].push"),
            [
                "fixture-socket.Socket.send @17 / several_implementations",
                "lib:lib.dom.d.ts.fetch @9 / several_implementations",
            ],
            "two implementations reaching different ends are a lead, each end kept"
        );
        assert_eq!(
            records(&with, "handler-table.ts", "live.send"),
            ["lib:lib.dom.d.ts.fetch @7 / site"],
            "a table entry's call through an interface member is followed"
        );
        let without = walk(None);
        for (file, callee) in [
            ("one-impl.ts", "notifier.notify"),
            ("two-impls.ts", "channels[kind].push"),
            ("handler-table.ts", "live.send"),
        ] {
            assert_eq!(
                records(&without, file, callee),
                ["- / interface_member"],
                "without the ask {file} {callee} stops as it did"
            );
        }
    }

    /// The unread sites of `on`'s walk with no implementations followed, after
    /// `ask` answered the implementations question as given.
    fn unread_after_ask(
        on: &super::super::fixture_walk::Walked,
        ask: Result<Vec<crate::services::type_sidecar::ImplementationsAnswer>, &'static str>,
    ) -> Vec<crate::unread_sites::UnreadSite> {
        let exports = claims();
        let asked = super::super::implementations::implementations_of(&on.inputs, |_| ask);
        let discovery = discover(&on.inputs, &Labels::of(&exports), &on.repo);
        let unread = crate::unread_sites::from_scan(
            &discovery,
            &crate::request_summary::decided_sites::Decided::default(),
            &[],
            &[],
            &on.repo,
        );
        crate::unread_sites::with_unanswered_implementations(unread, &asked.unanswered, &on.repo)
    }

    const ASKED_CALLS: [(&str, &str); 3] = [
        ("one-impl.ts", "notifier.notify"),
        ("two-impls.ts", "channels[kind].push"),
        ("handler-table.ts", "live.send"),
    ];

    /// An ask that died saves each call through an interface member
    /// `unanswered` beside `interface_member`: nothing was searched.
    #[test]
    fn a_failed_implementations_ask_saves_its_sites_unanswered() {
        let on = walked("implementations", "services/web", "web", &[], true)
            .expect("the implementations fixture walks: a skipped test proves nothing");
        let unread = unread_after_ask(&on, Err("process died"));
        for (file, callee) in ASKED_CALLS {
            let rows: Vec<_> = unread
                .iter()
                .filter(|row| row.file.ends_with(file) && row.callee.as_deref() == Some(callee))
                .collect();
            assert_eq!(rows.len(), 1, "{file} {callee} is one unread site");
            assert_eq!(rows[0].reason, Stop::InterfaceMember);
            assert_eq!(rows[0].also, [Stop::Unanswered], "{file} {callee}");
        }
    }

    /// An ask that answered and found nothing is a real `interface_member`
    /// stop: it adds nothing.
    #[test]
    fn an_answered_ask_that_found_none_adds_nothing() {
        let on = walked("implementations", "services/web", "web", &[], true)
            .expect("the implementations fixture walks: a skipped test proves nothing");
        let count = on.inputs.implementation_queries().len();
        assert!(count >= ASKED_CALLS.len(), "the fixture asks its calls");
        let none = || crate::services::type_sidecar::ImplementationsAnswer {
            declaration: None,
            implementations: Vec::new(),
            too_many: false,
            reason: None,
        };
        let unread = unread_after_ask(&on, Ok((0..count).map(|_| none()).collect()));
        for (file, callee) in ASKED_CALLS {
            let rows: Vec<_> = unread
                .iter()
                .filter(|row| row.file.ends_with(file) && row.callee.as_deref() == Some(callee))
                .collect();
            assert_eq!(rows.len(), 1, "{file} {callee} is one unread site");
            assert_eq!(rows[0].reason, Stop::InterfaceMember);
            assert!(
                rows[0].also.is_empty(),
                "{file} {callee}: {:?}",
                rows[0].also
            );
        }
    }

    /// A row at an asked span whose primary reason is not `interface_member`
    /// is left alone.
    #[test]
    fn a_row_with_another_reason_at_an_asked_span_is_untouched() {
        let on = walked("implementations", "services/web", "web", &[], true)
            .expect("the implementations fixture walks: a skipped test proves nothing");
        let asked = super::super::implementations::implementations_of(&on.inputs, |_| {
            Err::<Vec<_>, _>("process died")
        });
        let exports = claims();
        let discovery = discover(&on.inputs, &Labels::of(&exports), &on.repo);
        let mut unread = crate::unread_sites::from_scan(
            &discovery,
            &crate::request_summary::decided_sites::Decided::default(),
            &[],
            &[],
            &on.repo,
        );
        unread.retain(|row| row.reason == Stop::InterfaceMember);
        unread.truncate(1);
        assert_eq!(unread.len(), 1, "the walk saved an interface_member site");
        unread[0].reason = Stop::MissedInside;
        let kept = crate::unread_sites::with_unanswered_implementations(
            unread,
            &asked.unanswered,
            &on.repo,
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].reason, Stop::MissedInside);
        assert!(kept[0].also.is_empty());
    }

    /// The receiver-trace fixture's unread sites, with `declarations` as the
    /// trace's answer.
    fn unread_traced(
        on: &super::super::fixture_walk::Walked,
        exports: &[ExportClaims],
        declarations: &MemberDeclarations,
    ) -> Vec<crate::unread_sites::UnreadSite> {
        let discovery = discover(
            &on.inputs,
            &Labels::of(exports).tracing(declarations),
            &on.repo,
        );
        crate::unread_sites::from_scan(
            &discovery,
            &crate::request_summary::decided_sites::Decided::default(),
            &[],
            &[],
            &on.repo,
        )
    }

    /// The receiver-trace fixture walked with the verifier's real answer,
    /// and its declaration ask.
    fn traced_with_ask() -> (
        super::super::fixture_walk::Walked,
        Vec<ExportClaims>,
        MemberDeclarations,
        DeclarationAsk,
    ) {
        let (on, exports, declarations) = traced_fixture()
            .expect("the receiver-trace fixture walks: a skipped test proves nothing");
        let ask = declaration_ask(&on.inputs, &exports);
        assert!(
            ask.ops.iter().flatten().count() > 0,
            "the fixture asks op checks"
        );
        (on, exports, declarations, ask)
    }

    /// carrick#3991: a declaration ask that died after its retry saves each
    /// call it would have decided `unanswered` beside the call's own reason.
    /// Every site the real answer reads and the dead ask leaves unread says
    /// so, and its reason is the walk's.
    /// The unread sites with the real answer and with a dead ask: the rows
    /// only the dead ask leaves unread (each must say `unanswered`, its
    /// reason the walk's), and the rows marked `unanswered` that the real
    /// answer leaves unread too, as `file:line callee`.
    fn dead_ask_moves(
        on: &super::super::fixture_walk::Walked,
        exports: &[ExportClaims],
        answered: &MemberDeclarations,
    ) -> (Vec<crate::unread_sites::UnreadSite>, Vec<String>) {
        let ask = declaration_ask(&on.inputs, exports);
        let failed = MemberDeclarations::answered(&ask, Err::<Vec<_>, _>("process died"));
        let read = unread_traced(on, exports, answered);
        let unread = unread_traced(on, exports, &failed);
        let in_read = |row: &crate::unread_sites::UnreadSite| {
            read.iter().any(|held| {
                (held.file.as_str(), held.span_start, held.span_end)
                    == (row.file.as_str(), row.span_start, row.span_end)
            })
        };
        let opened: Vec<_> = unread.iter().filter(|row| !in_read(row)).cloned().collect();
        for row in &opened {
            assert!(
                row.also.contains(&Stop::Unanswered),
                "{}:{} {:?} {:?}",
                row.file,
                row.line,
                row.callee,
                row.reason
            );
            assert_ne!(row.reason, Stop::Unanswered, "the walk's reason stays");
        }
        let mut also_unread: Vec<String> = unread
            .iter()
            .filter(|row| in_read(row) && row.also.contains(&Stop::Unanswered))
            .map(|row| {
                format!(
                    "{}:{} {}",
                    row.file.rsplit('/').next().unwrap_or(&row.file),
                    row.line,
                    row.callee.as_deref().unwrap_or("")
                )
            })
            .collect();
        also_unread.sort();
        (opened, also_unread)
    }

    #[test]
    fn a_failed_declaration_ask_saves_its_sites_unanswered() {
        let (on, exports, answered, ask) = traced_with_ask();
        let (opened, also_unread) = dead_ask_moves(&on, &exports, &answered);
        // Marked though the real answer leaves them unread too: calls whose
        // declaration the ask decides (`request.get` is the package's other
        // `get`), and the walks that pass one.
        assert_eq!(also_unread, ["negatives.ts:5 request.get"]);
        let failed = MemberDeclarations::answered(&ask, Err::<Vec<_>, _>("process died"));
        let unread = unread_traced(&on, &exports, &failed);
        for (file, callee) in [
            ("helpers.ts", "app.get"),
            ("client.ts", "client.get"),
            ("routes.ts", "this.app.get"),
        ] {
            assert!(
                opened
                    .iter()
                    .any(|row| row.file.ends_with(file) && row.callee.as_deref() == Some(callee)),
                "{file} {callee} is a site the dead ask leaves unread: {opened:?}"
            );
        }
        // A short answer is no answer either.
        let short = MemberDeclarations::answered(&ask, Ok::<_, &str>(Vec::new()));
        assert_eq!(
            unread_traced(&on, &exports, &short),
            unread,
            "a short answer saves the same sites"
        );
    }

    /// carrick#3991 on the held-member reader (`value_end`): a labelled
    /// member held as a value and run elsewhere is a call the dead ask would
    /// have decided too.
    #[test]
    fn a_failed_declaration_ask_saves_held_member_sites_unanswered() {
        let (on, exports, answered) = traced_fixture_in("callee-members", "services/web", "web")
            .expect("the callee-members fixture walks: a skipped test proves nothing");
        let (opened, also_unread) = dead_ask_moves(&on, &exports, &answered);
        assert!(
            opened.iter().any(|row| row.file.ends_with("m-bind.ts")),
            "the bound member's call is a site the dead ask leaves unread: {opened:?}"
        );
        assert_eq!(also_unread, Vec::<String>::new(), "{also_unread:?}");
    }

    /// An ask that answered adds nothing, whatever it verified.
    #[test]
    fn an_answered_declaration_ask_adds_nothing() {
        let (on, exports, answered, _) = traced_with_ask();
        assert!(!answered.is_empty(), "the verifier answered the fixture");
        for row in unread_traced(&on, &exports, &answered) {
            assert!(
                !row.also.contains(&Stop::Unanswered),
                "{}:{} {:?}",
                row.file,
                row.line,
                row.callee
            );
        }
    }

    /// A call the ask was not for keeps its row as it was: an in-repo
    /// class's own `get`, a local interface's `get` and an untyped
    /// receiver's. A call of the same package's other `get` (`request.get`)
    /// was asked for: its declaration is one the answer decides, so it says
    /// `unanswered` too.
    #[test]
    fn a_failed_declaration_ask_leaves_calls_it_was_not_for_alone() {
        let (on, exports, answered, ask) = traced_with_ask();
        let failed = MemberDeclarations::answered(&ask, Err::<Vec<_>, _>("process died"));
        let read = unread_traced(&on, &exports, &answered);
        let unread = unread_traced(&on, &exports, &failed);
        let at = |rows: &[crate::unread_sites::UnreadSite], callee: &str| {
            rows.iter()
                .find(|row| {
                    row.file.ends_with("negatives.ts") && row.callee.as_deref() == Some(callee)
                })
                .cloned()
        };
        for callee in ["new Local().get", "shape.get", "target.get"] {
            assert_eq!(at(&unread, callee), at(&read, callee), "{callee}");
            if let Some(row) = at(&unread, callee) {
                assert!(!row.also.contains(&Stop::Unanswered), "{callee}");
            }
        }
        let request = at(&unread, "request.get").expect("request.get is an unread site");
        assert!(request.also.contains(&Stop::Unanswered), "{request:?}");
    }

    /// A traced fixture through the whole site pass, with `declarations` as
    /// the trace's answer: each saved site and possible call as the blob
    /// carries it, and each decided site's composed and procedure paths, by
    /// span.
    #[allow(clippy::type_complexity)]
    fn traced_rows(
        on: &super::super::fixture_walk::Walked,
        exports: &[ExportClaims],
        declarations: &MemberDeclarations,
    ) -> (
        Vec<crate::unread_sites::UnreadSite>,
        BTreeMap<(String, u32, u32), Vec<String>>,
    ) {
        use super::super::decided_sites::{
            MakerLabel, ProcedureLabels, ScopeLabel, SiteLabel, decided_sites_traced_with,
        };
        let discovery = discover(
            &on.inputs,
            &Labels::of(exports).tracing(declarations),
            &on.repo,
        );
        let discovered: Vec<super::super::decided_sites::DiscoverySite> = discovery
            .sites
            .iter()
            .map(|site| serde_json::from_value(serde_json::to_value(site).unwrap()).unwrap())
            .collect();
        let labels = SiteLabel::from_exports(exports);
        let decided = decided_sites_traced_with(
            &on.inputs,
            &[],
            &on.repo,
            &discovered,
            &labels,
            &MakerLabel::from_exports(exports),
            &ScopeLabel::from_exports(exports, &declarations.scope_answers),
            &ProcedureLabels::from_exports(exports, &declarations.scope_answers),
        );
        let records =
            super::super::site_record::site_records(&on.inputs, &[], &on.repo, &decided, &labels);
        let rows = crate::site_rows::of(&decided, &records, &labels, &Default::default());
        let unread = crate::unread_sites::from_scan(&discovery, &decided, &[], &[], &on.repo);
        let mut data: crate::cloud_storage::CloudRepoData =
            serde_json::from_value(serde_json::json!({
                "repo_name": "r",
                "endpoints": [], "calls": [], "mounts": [], "apps": {},
                "imported_handlers": [], "function_definitions": {},
                "config_json": null, "package_json": null, "packages": null,
                "last_updated": "2026-11-13T00:00:00Z", "commit_hash": "abc"
            }))
            .unwrap();
        let listed: Vec<crate::possible_calls::PossibleCall> =
            rows.leads.iter().chain(&rows.routes).cloned().collect();
        crate::unread_sites::attach(&mut data, unread, &listed, &rows.scopes_unanswered);
        let mut sites: BTreeMap<(String, u32, u32), Vec<String>> = BTreeMap::new();
        for site in &decided.sites {
            let composed: Vec<String> = site
                .composed
                .iter()
                .map(|composed| format!("{:?} {}", composed.path, composed.end))
                .collect();
            let procedure = site
                .procedure
                .as_ref()
                .map(|procedure| format!("{:?} {:?}", procedure.full_paths, procedure.stop));
            sites
                .entry((site.file.clone(), site.span_start, site.span_end))
                .or_default()
                .push(format!(
                    "{:?} {composed:?} {procedure:?} trimmed={}",
                    site.key, site.composed_trimmed
                ));
        }
        (data.unread_sites.unwrap_or_default(), sites)
    }

    /// carrick#4155: the declaration ask carries the scope checks too, so a
    /// dead ask leaves the decided pass without the scope members that bind
    /// a route's prefix and a procedure router's keys. Run with the real
    /// answer and with a dead ask: each span where the dead run saves
    /// another site, or decides another path, says `unanswered` beside its
    /// own reason, and no other span does. The dead run's moves, as
    /// `file:line`, and the spans marked that did not move.
    fn dead_ask_scope_moves(
        on: &super::super::fixture_walk::Walked,
        exports: &[ExportClaims],
        answered: &MemberDeclarations,
    ) -> (Vec<String>, Vec<String>) {
        let ask = declaration_ask(&on.inputs, exports);
        assert!(
            ask.scopes.iter().flatten().count() > 0,
            "the fixture asks scope checks"
        );
        let dead = MemberDeclarations::answered(&ask, Err::<Vec<_>, _>("process died"));
        let (real_rows, real_sites) = traced_rows(on, exports, answered);
        let (dead_rows, dead_sites) = traced_rows(on, exports, &dead);
        let rows_at = |rows: &[crate::unread_sites::UnreadSite], at: &(String, u32, u32)| {
            let mut found: Vec<String> = rows
                .iter()
                .filter(|row| (&row.file, row.span_start, row.span_end) == (&at.0, at.1, at.2))
                .map(|row| format!("{} {} {:?}", row.reason.as_str(), row.slot, row.callee))
                .collect();
            found.sort();
            found
        };
        let mut spans: BTreeSet<(String, u32, u32)> = BTreeSet::new();
        for row in real_rows.iter().chain(&dead_rows) {
            spans.insert((row.file.clone(), row.span_start, row.span_end));
        }
        spans.extend(real_sites.keys().cloned());
        spans.extend(dead_sites.keys().cloned());
        let line_of = |at: &(String, u32, u32)| {
            dead_rows
                .iter()
                .chain(&real_rows)
                .find(|row| (&row.file, row.span_start, row.span_end) == (&at.0, at.1, at.2))
                .map_or(0, |row| row.line)
        };
        let mut unmarked: Vec<String> = Vec::new();
        let mut marked_unmoved: Vec<String> = Vec::new();
        let mut moved: Vec<String> = Vec::new();
        for at in &spans {
            let here: Vec<&crate::unread_sites::UnreadSite> = dead_rows
                .iter()
                .filter(|row| (&row.file, row.span_start, row.span_end) == (&at.0, at.1, at.2))
                .collect();
            let marked = !here.is_empty()
                && here.iter().all(|row| {
                    row.also.contains(&Stop::Unanswered) && row.reason != Stop::Unanswered
                });
            let moves = rows_at(&real_rows, at) != rows_at(&dead_rows, at)
                || real_sites.get(at) != dead_sites.get(at);
            let name = format!("{}:{}", at.0, line_of(at));
            if moves {
                moved.push(name.clone());
                if !marked {
                    unmarked.push(format!(
                        "{name} real {:?} {:?} / dead {:?} {:?}",
                        rows_at(&real_rows, at),
                        real_sites.get(at),
                        here.iter()
                            .map(|row| format!("{} also {:?}", row.reason.as_str(), row.also))
                            .collect::<Vec<_>>(),
                        dead_sites.get(at)
                    ));
                }
            } else if here.iter().any(|row| row.also.contains(&Stop::Unanswered)) {
                marked_unmoved.push(name);
            }
        }
        assert!(
            unmarked.is_empty(),
            "{} of {} moved span(s) not marked unanswered:\n{}",
            unmarked.len(),
            moved.len(),
            unmarked.join("\n")
        );
        moved.sort();
        marked_unmoved.sort();
        (moved, marked_unmoved)
    }

    /// carrick#4155 on scope members that mount a router, run a plugin and
    /// return a scoped app (`receiver-scopes`). The routers a `route` mount
    /// places keep their sites in a dead run, served relative to a mount
    /// the trace cannot place: those say `unanswered`. The other ten moves
    /// are the op half's (carrick#3991), marked before this fix.
    #[test]
    fn a_failed_declaration_ask_saves_scoped_route_sites_unanswered() {
        let (on, exports, answered) = traced_fixture_named("receiver-scopes")
            .expect("the receiver-scopes fixture walks: a skipped test proves nothing");
        let (moved, marked_unmoved) = dead_ask_scope_moves(&on, &exports, &answered);
        assert_eq!(moved.len(), 17, "{moved:#?}");
        for mounted in [
            "src/admin.ts:8",
            "src/aliased.ts:8",
            "src/extra.ts:9",
            "src/reports.ts:9",
            "src/shared.ts:8",
            "src/users.ts:8",
            "src/users.ts:9",
        ] {
            assert!(
                moved.iter().any(|at| at == mounted),
                "{mounted}: {moved:#?}"
            );
        }
        assert_eq!(marked_unmoved, Vec::<String>::new(), "moved: {moved:#?}");
    }

    /// carrick#4155 on procedure router members that collect and merge
    /// (`procedure-router`). A dead ask leaves every procedure op unread at
    /// discovery, so each site it moves is the op half's (carrick#3991).
    /// Marked though it does not move: `t.procedure.stream`, a member the
    /// label states no method for, whose declaration the ask decides.
    #[test]
    fn a_failed_declaration_ask_saves_procedure_sites_unanswered() {
        let (on, exports, answered) = traced_fixture_in("procedure-router", ".", "api")
            .expect("the procedure-router fixture walks: a skipped test proves nothing");
        let (moved, marked_unmoved) = dead_ask_scope_moves(&on, &exports, &answered);
        assert_eq!(moved.len(), 17, "{moved:#?}");
        assert_eq!(marked_unmoved, ["src/router.ts:16"], "moved: {moved:#?}");
    }

    /// carrick#2432 slice B: an implementation the walk cannot enter (one
    /// only a `.d.ts` declares, or a property that is no function) is not
    /// silently dropped. A call whose only implementations are like that
    /// keeps the `interface_member` stop.
    #[test]
    fn an_implementation_the_walk_cannot_enter_keeps_the_stop() {
        let Some(on) = walked("implementations", "services/web", "web", &[], true) else {
            return;
        };
        let script = super::super::fixture_walk::sidecar().expect("built above");
        let exports = claims();
        let asked = super::super::fixture_walk::implementations_of(&script, &on, "services/web");
        let labels = Labels::of(&exports).following(&asked);
        let with = discover(&on.inputs, &labels, &on.repo);
        for (file, callee) in [
            ("declared-only.ts", "sink.emit"),
            ("property-impl.ts", "hook.fire"),
        ] {
            let records: Vec<String> = with
                .sites
                .iter()
                .filter(|site| site.file.ends_with(file) && site.callee == callee)
                .map(|site| {
                    site.stop
                        .map_or("site".to_string(), |stop| stop.as_str().to_string())
                })
                .collect();
            assert_eq!(
                records,
                ["interface_member"],
                "{file} {callee}: the only implementation cannot be entered"
            );
        }
    }

    /// carrick#2613 (T2): a call that reached nothing and entered no code says
    /// why, on `unfollowed`: a function a factory returns, an in-repo
    /// function outside the service, a member of a package the store answered
    /// that none of its labels matched, and a call into a package the answer
    /// holds nothing for. A body that calls only local helpers and the
    /// language's own library is `no_site`, and no record of any kind. The
    /// answer key is the fixture's `unfollowed` list.
    #[test]
    fn every_call_it_cannot_follow_says_why() {
        let Some(on) = walked("discovery", "services/web", "web", &[], true) else {
            return;
        };
        let answer = answer();
        let discovery = discover(&on.inputs, &Labels::of_answer(&answer), &on.repo);
        let expected: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(fixture("sites.expected.json")).unwrap())
                .unwrap();
        assert_eq!(
            serde_json::to_value(&discovery.unfollowed).unwrap(),
            expected["unfollowed"],
            "{}",
            serde_json::to_string(&discovery.unfollowed).unwrap()
        );
        for file in ["unfollowed.ts", "passed-in.ts"] {
            let in_file = |records: &[DiscoverySite]| {
                records
                    .iter()
                    .filter(|site| site.file.ends_with(file))
                    .map(|site| format!("{} {}", site.line, site.callee))
                    .collect::<Vec<_>>()
            };
            let sited = in_file(&discovery.sites);
            let unlisted = in_file(&discovery.unfollowed);
            assert!(
                sited.iter().all(|record| !unlisted.contains(record)),
                "no call is both a site and unfollowed: {sited:?} {unlisted:?}"
            );
        }
        assert!(
            !discovery
                .unfollowed
                .iter()
                .chain(&discovery.sites)
                .any(|site| site.file.ends_with("unfollowed.ts") && site.callee == "double"),
            "a call to a local helper that finds nothing is no_site"
        );
        assert!(
            discovery.counts.get("no_site").is_some_and(|n| *n > 0),
            "{:?}",
            discovery.counts
        );
        // Labels that know only the exports cannot tell a package the store
        // did not answer from one it answered with nothing to label.
        let blind = discover(&on.inputs, &Labels::of(&answer.exports), &on.repo);
        assert!(
            blind
                .unfollowed
                .iter()
                .all(|site| site.stop != Some(Stop::UnansweredPackage))
        );
    }

    /// carrick#2448 T1: a member call on an object an in-repo factory returns
    /// enters the function written at that key. `r1` sends through arrows in
    /// the returned object; a write through the held object (`n1/escaped`)
    /// and a client that sends nothing (`n1/cache`) enter nothing. The answer
    /// key is `tests/fixtures/generated-client/README.md`.
    #[test]
    fn a_member_call_on_an_object_a_factory_returns_enters_the_function_at_that_key() {
        let Some(on) = walked("generated-client", "", "", &[], true) else {
            return;
        };
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/generated-client/__llm__/library-claims/default.json"),
        )
        .unwrap();
        let answer = crate::library_store::parse_answer(&text);
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        let discovery = discover(&on.inputs, &Labels::of_answer(&answer), &on.repo);
        let at = |file: &str, line: u32| -> Vec<&DiscoverySite> {
            discovery
                .sites
                .iter()
                .filter(|site| site.file.ends_with(file) && site.line == line)
                .collect()
        };
        for (line, arrow) in [(5, 10), (8, 11), (10, 10)] {
            let [site] = at("r1/sdk.ts", line)[..] else {
                panic!(
                    "a site at r1/sdk.ts:{line}: {:?} {:?}",
                    discovery.sites, discovery.unfollowed
                );
            };
            let end = site.end.as_ref().expect("the call reaches fetch");
            assert_eq!(
                (end.package.as_str(), end.export.as_str()),
                ("lib:lib.dom.d.ts", "fetch")
            );
            assert!(end.file.ends_with("r1/client.ts"), "{end:?}");
            // `get` and `post` are written at lines 10 and 11 of the client.
            let hops: Vec<(&str, u32)> = site
                .chain
                .iter()
                .map(|hop| (hop.function.as_str(), hop.line))
                .collect();
            assert!(
                hops.contains(&(VALUE_HOP, arrow)),
                "the chain goes through the arrow at the key: {hops:?}"
            );
        }
        assert!(
            at("n1/escaped.ts", 11)
                .iter()
                .all(|site| site.end.is_none()),
            "an object written through is a hole: {:?}",
            at("n1/escaped.ts", 11)
        );
        assert!(
            at("n1/cache.ts", 19).iter().all(|site| site.end.is_none()),
            "a client that sends nothing has no end"
        );
    }

    /// Discovery of `tests/fixtures/spread-absent-key`, its sites at
    /// `file:line` by their stops, and every site and unfollowed call for a
    /// failure message.
    fn spread_absent_key() -> Option<(Discovery, String)> {
        let on = walked("spread-absent-key", "", "", &[], true)?;
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/spread-absent-key/__llm__/library-claims/default.json"),
        )
        .unwrap();
        let answer = crate::library_store::parse_answer(&text);
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        let discovery = discover(&on.inputs, &Labels::of_answer(&answer), &on.repo);
        let all = format!("{:?} {:?}", discovery.sites, discovery.unfollowed);
        Some((discovery, all))
    }

    /// The stops of the sites discovery keeps at `file:line`.
    fn stops_at(discovery: &Discovery, file: &str, line: u32) -> Vec<Option<Stop>> {
        discovery
            .sites
            .iter()
            .filter(|site| site.file.ends_with(file) && site.line == line)
            .map(|site| site.stop)
            .collect()
    }

    /// Ruling R-SP (carrick#2448, #3504): a key read through object spreads
    /// is absent where every spread source resolves to an object that does
    /// not write it. The hooks call the SDK with
    /// `{ ...options, signal, throwOnError }`, `options` the hook's own
    /// parameter whose callers never write `client`, so
    /// `options?.client ?? client` holds the module client alone and the
    /// calls reach the client's `fetch` with no `several_values` stop. The
    /// answer key is `tests/fixtures/spread-absent-key/README.md`.
    #[test]
    fn a_key_no_spread_source_writes_is_absent() {
        let Some((discovery, all)) = spread_absent_key() else {
            return;
        };
        for (file, line) in [
            ("src/sdk.ts", 7),
            ("src/sdk.ts", 14),
            ("src/sdk.ts", 22),
            ("hooks/queries.ts", 8),
            ("hooks/queries.ts", 15),
            ("hooks/queries.ts", 22),
        ] {
            let stops = stops_at(&discovery, file, line);
            assert!(
                !stops.is_empty() && stops.iter().all(Option::is_none),
                "{file}:{line} reaches the client with no stop: {stops:?}\n{all}"
            );
        }
    }

    /// Ruling R-SP, must-stay: a spread source the resolver does not read
    /// (`...JSON.parse(raw)`) may write the key, so it stays unread and the
    /// receiver keeps two values. The answer key is
    /// `tests/fixtures/spread-absent-key/README.md`.
    #[test]
    fn a_key_an_unread_spread_source_may_write_keeps_several_values() {
        let Some((discovery, all)) = spread_absent_key() else {
            return;
        };
        for line in [8, 15] {
            let stops = stops_at(&discovery, "unread/stored.ts", line);
            assert!(
                !stops.is_empty() && stops.iter().all(|stop| *stop == Some(Stop::SeveralValues)),
                "unread/stored.ts:{line} keeps several values: {stops:?}\n{all}"
            );
        }
    }

    /// Ruling R-SP, must-stay: the sources are parameters of callbacks a
    /// library runs (`key[0]`, `fnOptions`). Nothing in the repository passes
    /// them, so they cannot be resolved here and `client` stays unread; the
    /// receiver keeps two values. Reading what a library passes to a callback
    /// is a store question, filed under the V1 shape hold. The answer key is
    /// `tests/fixtures/spread-absent-key/README.md`.
    #[test]
    fn a_key_a_library_callback_parameter_spread_keeps_several_values() {
        let Some((discovery, all)) = spread_absent_key() else {
            return;
        };
        for line in [17, 21] {
            let stops = stops_at(&discovery, "src/callbacks.ts", line);
            assert!(
                !stops.is_empty() && stops.iter().all(|stop| *stop == Some(Stop::SeveralValues)),
                "callbacks.ts:{line} keeps several values: {stops:?}\n{all}"
            );
        }
    }

    /// Ruling R-SP: a named-binding spread source that writes the key
    /// (`call({ ...base, signal })`, `const base = { run: send }` local to the caller) is read, so the
    /// key reads that value: `options.run(...)` enters `send` and stops
    /// nowhere. Before the rule the key was unread and the call stopped. The
    /// answer key is
    /// `tests/fixtures/spread-absent-key/README.md`.
    #[test]
    fn a_key_a_spread_source_writes_is_read() {
        let Some((discovery, all)) = spread_absent_key() else {
            return;
        };
        let stops = stops_at(&discovery, "writes/same.ts", 7);
        assert!(
            !stops.is_empty() && stops.iter().all(Option::is_none),
            "writes/same.ts:7 enters `send` with no stop: {stops:?}\n{all}"
        );
    }

    /// Ruling R-SP, must-stay: a spread source that writes another client
    /// (`{ ...base, signal }`, `base = { client: offline }`) carries what it
    /// writes, so the receiver holds both clients and keeps two values. The
    /// answer key is `tests/fixtures/spread-absent-key/README.md`.
    #[test]
    fn a_key_a_spread_source_writes_another_client_keeps_several_values() {
        let Some((discovery, all)) = spread_absent_key() else {
            return;
        };
        for line in [16, 23] {
            let stops = stops_at(&discovery, "writes/offline.ts", line);
            assert!(
                !stops.is_empty() && stops.iter().all(|stop| *stop == Some(Stop::SeveralValues)),
                "writes/offline.ts:{line} keeps several values: {stops:?}\n{all}"
            );
        }
    }

    /// Ruling R-SP, must-stay (v1-agent on carrick#4025): `args` reaches
    /// `check` only through spreads (of the parameter, and of a local binding
    /// that spreads it), and the gate holding it is run only through a value
    /// (`deps.gate(...)`), so the resolver finds no caller of it, though one
    /// writes `log`. The rule must not state `value_never_bound` there: each
    /// `log.warn` keeps the reason a spread the resolver cannot read gives
    /// (`checkParsed`, the same read through `...JSON.parse(raw)`). The
    /// answer key is `tests/fixtures/spread-absent-key/README.md`.
    #[test]
    fn a_key_through_a_parameter_run_only_as_a_value_keeps_the_unread_reason() {
        let Some((discovery, all)) = spread_absent_key() else {
            return;
        };
        // A callee the walk stops at with no labelled end is listed among
        // the calls it did not follow, or as a site.
        let unfollowed_at = |line: u32| -> Vec<Option<Stop>> {
            discovery
                .sites
                .iter()
                .chain(&discovery.unfollowed)
                .filter(|site| site.file.ends_with("through_value/gates.ts") && site.line == line)
                .map(|site| site.stop)
                .collect()
        };
        let twin = unfollowed_at(35);
        assert!(
            !twin.is_empty() && !twin.contains(&Some(Stop::ValueNeverBound)),
            "gates.ts:35 states the unread reason: {twin:?}\n{all}"
        );
        for line in [9, 23] {
            let stops = unfollowed_at(line);
            assert_eq!(
                stops, twin,
                "gates.ts:{line} keeps the twin's reason, not value_never_bound\n{all}"
            );
        }
    }

    /// carrick#4025: a callee read from a parameter of a function that is
    /// run only through a value (`{ g: gate }` handed on, run as
    /// `deps.g(args)`) is saved, and never as `value_never_bound`: the call
    /// that runs the function writes the parameter, though no call names
    /// the function. Same module and across modules. The answer key is
    /// `tests/fixtures/value-run-param/README.md`.
    #[test]
    fn a_callee_read_from_a_parameter_of_a_function_run_only_through_a_value_is_never_value_never_bound()
     {
        let Some(on) = walked("value-run-param", "", "", &[], true) else {
            return;
        };
        let discovery = discover(&on.inputs, &Labels::of(&[]), &on.repo);
        let all = format!("{:?} {:?}", discovery.sites, discovery.unfollowed);
        for (file, line) in [("src/local.ts", 8), ("src/gates.ts", 7)] {
            let stops: Vec<Option<Stop>> = discovery
                .sites
                .iter()
                .chain(&discovery.unfollowed)
                .filter(|site| site.file.ends_with(file) && site.line == line)
                .map(|site| site.stop)
                .collect();
            assert!(
                !stops.is_empty() && !stops.contains(&Some(Stop::ValueNeverBound)),
                "{file}:{line} is saved with a reason other than value_never_bound: \
                 {stops:?}\n{all}"
            );
            // Kept among the sites, as `value_never_bound` was, with the
            // handoff as its reason.
            let kept: Vec<Option<Stop>> = discovery
                .sites
                .iter()
                .filter(|site| site.file.ends_with(file) && site.line == line)
                .map(|site| site.stop)
                .collect();
            assert_eq!(
                kept,
                vec![Some(Stop::CallbackIntoOwnFunction)],
                "{file}:{line} is a site stopped as callback_into_own_function\n{all}"
            );
        }
    }

    /// Ruling R-SP, must-stay (v1-agent on carrick#3856, #4025): where a
    /// spread source writes a key from a parameter whose callers the
    /// resolver does not find (a function run only through a value, a method
    /// another service calls), each such write keeps the reading it had
    /// before the rule, so the site states main's reason and never
    /// `value_never_bound`. `meters.ts` spreads a local object that writes
    /// `log` from a parameter; `refusals.ts` spreads the parameter in the
    /// gate and in an arrow inside it; `panel.ts` builds a class with
    /// `{ token, ...params }` and calls its field; `relay.ts` passes the key
    /// through two spreads. `builder.ts` is the rule's read: `handle` read
    /// through the builder method's `{ ...options, tag }`, `options` a
    /// parameter of a function written inside the builder, is a callback
    /// what runs that method fills. The answer key is
    /// `tests/fixtures/spread-absent-key/README.md`.
    #[test]
    fn a_key_a_spread_source_writes_from_an_unfilled_parameter_keeps_mains_reason() {
        let Some((discovery, all)) = spread_absent_key() else {
            return;
        };
        let at = |file: &str, line: u32| -> (Vec<Option<Stop>>, Vec<Option<Stop>>) {
            let of = |sites: &Vec<DiscoverySite>| -> Vec<Option<Stop>> {
                sites
                    .iter()
                    .filter(|site| site.file.ends_with(file) && site.line == line)
                    .map(|site| site.stop)
                    .collect()
            };
            (of(&discovery.sites), of(&discovery.unfollowed))
        };
        let unread = (vec![Some(Stop::UnreadExpression)], Vec::new());
        for (file, line) in [
            ("through_value/meters.ts", 15),
            ("through_value/refusals.ts", 9),
        ] {
            assert_eq!(at(file, line), unread, "{file}:{line}\n{all}");
        }
        assert_eq!(
            at("through_value/panel.ts", 19),
            (Vec::new(), vec![Some(Stop::ValueCallee)]),
            "panel.ts:19\n{all}"
        );
        // The calls into the meter stop inside it, as before the rule: no
        // site of their own.
        for line in [23, 24, 33] {
            assert_eq!(
                at("through_value/meters.ts", line),
                (Vec::new(), Vec::new()),
                "meters.ts:{line}\n{all}"
            );
        }
        // Two spreads on the way up are one unread write, as before the
        // rule: the twin's reading.
        assert_eq!(
            at("through_value/relay.ts", 9),
            at("through_value/gates.ts", 35),
            "relay.ts:9\n{all}"
        );
        assert_eq!(
            at("through_value/builder.ts", 8),
            (vec![Some(Stop::InjectedCallee)], Vec::new()),
            "builder.ts:8\n{all}"
        );
        // A key no spread source writes reads as a literal that does not
        // write it.
        assert_eq!(
            at("through_value/clock.ts", 8),
            at("through_value/clock.ts", 13),
            "clock.ts:8\n{all}"
        );
    }

    /// The `(file, line)` of each labelled end of the sites discovery keeps
    /// at `file:line`, with the stops beside them.
    fn ends_at(
        discovery: &Discovery,
        file: &str,
        line: u32,
    ) -> (Vec<(String, String)>, Vec<Option<Stop>>) {
        let sites: Vec<&DiscoverySite> = discovery
            .sites
            .iter()
            .filter(|site| site.file.ends_with(file) && site.line == line)
            .collect();
        let ends = sites
            .iter()
            .filter_map(|site| site.end.as_ref())
            .map(|end| (end.package.clone(), end.export.clone()))
            .collect();
        (ends, sites.iter().map(|site| site.stop).collect())
    }

    /// Ruling R-SP, must-keep (v1-agent on carrick#3856): an action is
    /// handed the members a factory returns, by one caller as they are and
    /// by another as `{ ...tableDeps(), mint }`. Each member sends a command
    /// built from a labelled export of the table client, one inside a
    /// callback a paging helper runs. Read through the spread, both callers
    /// hold the same members, so each `deps.*` call keeps its labelled end
    /// and stops nowhere. The answer key is
    /// `tests/fixtures/spread-absent-key/README.md`.
    #[test]
    fn a_member_read_through_a_spread_of_a_factory_keeps_its_labelled_end() {
        let Some((discovery, all)) = spread_absent_key() else {
            return;
        };
        for (line, export) in [(9, "GetRow"), (10, "QueryRows")] {
            let (ends, stops) = ends_at(&discovery, "deps_spread/action.ts", line);
            assert!(
                ends.contains(&("fixture-table".to_string(), export.to_string()))
                    && ends.iter().all(|(package, _)| package == "fixture-table")
                    && stops.iter().all(Option::is_none),
                "action.ts:{line} reaches `{export}` with no stop: {ends:?} {stops:?}\n{all}"
            );
        }
    }

    /// A callee whose one caller hands it a value the resolver does not
    /// read holds one stop, and the call is saved with a reason, never
    /// dropped (v1-agent on carrick#3856): `agree` passes a lone held stop
    /// up as no stop, and the call's own row names the hole. The answer key
    /// is `tests/fixtures/spread-absent-key/README.md`.
    #[test]
    fn a_callee_holding_one_stop_is_saved_with_a_reason() {
        let Some((discovery, all)) = spread_absent_key() else {
            return;
        };
        let saved = |line: u32| -> Vec<(Option<SiteEnd>, Option<Stop>)> {
            discovery
                .sites
                .iter()
                .chain(&discovery.unfollowed)
                .filter(|site| site.file.ends_with("deps_spread/action.ts") && site.line == line)
                .map(|site| (site.end.clone(), site.stop))
                .collect()
        };
        assert_eq!(
            (saved(15), saved(20)),
            (
                vec![(None, Some(Stop::ValueCallee))],
                vec![(None, Some(Stop::ValueCallee))]
            ),
            "action.ts:15 and :20 are saved with a reason\n{all}"
        );
    }

    /// carrick#2448 T2: the method a maker call binds. `get` and `post` of
    /// the client are `makeMethodFn('GET')` and `makeMethodFn('POST')`; the
    /// arrow they return reads `method` from its factory, so the site's method
    /// is what the maker call passes. The answer key is
    /// `tests/fixtures/generated-client/README.md`.
    #[test]
    fn a_method_a_maker_call_binds_is_the_sites_method() {
        use super::super::decided_sites::{MethodValue, SiteLabel, decided_sites};
        let Some(on) = walked("generated-client", "", "", &[], true) else {
            return;
        };
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/generated-client/__llm__/library-claims/default.json"),
        )
        .unwrap();
        let answer = crate::library_store::parse_answer(&text);
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        let discovery = discover(&on.inputs, &Labels::of_answer(&answer), &on.repo);
        let discovered: Vec<super::super::decided_sites::DiscoverySite> = discovery
            .sites
            .iter()
            .map(|site| serde_json::from_value(serde_json::to_value(site).unwrap()).unwrap())
            .collect();
        let decided = decided_sites(
            &on.inputs,
            &on.repo,
            &discovered,
            &SiteLabel::from_exports(&answer.exports),
        );
        for (line, method) in [(5, "GET"), (8, "POST")] {
            let at: Vec<_> = decided
                .sites
                .iter()
                .filter(|site| site.file.ends_with("r2/sdk.ts") && site.line == line)
                .collect();
            let [site] = at[..] else {
                panic!(
                    "a decided site at r2/sdk.ts:{line}: {:?} {:?}",
                    decided
                        .sites
                        .iter()
                        .map(|s| (&s.file, s.line))
                        .collect::<Vec<_>>(),
                    discovery
                        .sites
                        .iter()
                        .map(|s| (&s.file, s.line, &s.stop))
                        .collect::<Vec<_>>()
                );
            };
            assert_eq!(
                site.method,
                Some(MethodValue::Pieces(vec![Piece::Lit(method.to_string())])),
                "r2/sdk.ts:{line}"
            );
            // The URL goes through `{ ...options, method }` to the send.
            assert_eq!(
                site.name,
                vec![Piece::Lit("/r2/items".to_string())],
                "r2/sdk.ts:{line}"
            );
        }
    }

    /// carrick#2448 T3b: the faithful generated shape sends through
    /// `const _fetch = opts.fetch!`, where `opts` is handed to hooks, so every
    /// value the resolver gives `_fetch` is a hole. The compiler types the
    /// call's callee as a labelled export (`fetch?: typeof fetch`), and the
    /// call row says so: that export is the end, reached through its type.
    /// The answer key is `tests/fixtures/generated-client/README.md`.
    #[test]
    fn a_send_through_a_value_typed_as_fetch_reaches_fetch() {
        let Some(on) = walked("generated-client", "", "", &[], true) else {
            return;
        };
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/generated-client/__llm__/library-claims/default.json"),
        )
        .unwrap();
        let answer = crate::library_store::parse_answer(&text);
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        let discovery = discover(&on.inputs, &Labels::of_answer(&answer), &on.repo);
        let sites: Vec<&DiscoverySite> = discovery
            .sites
            .iter()
            .filter(|site| site.file.ends_with("r3/client/client.ts") && site.line == 38)
            .collect();
        let [site] = sites[..] else {
            panic!(
                "a site at r3/client/client.ts:38: {:?} {:?}",
                discovery.sites, discovery.unfollowed
            );
        };
        let end = site
            .end
            .as_ref()
            .expect("the call reaches fetch by its type");
        assert_eq!(
            (end.package.as_str(), end.export.as_str()),
            ("lib:lib.dom.d.ts", "fetch")
        );
        assert!(end.file.ends_with("r3/client/client.ts"), "{end:?}");
        assert_eq!(end.line, 38, "{end:?}");
    }

    /// carrick#2448 T3c: a function the extractor keys that is written
    /// inside another (`const request = async (options) => …` inside
    /// `createClient`) has no keyed body; the walk enters the function
    /// written there, under a `<value>` hop at its line, and reaches the send
    /// at `client.ts:38` rather than stopping `no_body`. The send keeps its
    /// own site. The answer key is `tests/fixtures/generated-client/README.md`.
    #[test]
    fn a_call_into_a_function_written_inside_another_enters_it() {
        let Some(on) = walked("generated-client", "", "", &[], true) else {
            return;
        };
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/generated-client/__llm__/library-claims/default.json"),
        )
        .unwrap();
        let answer = crate::library_store::parse_answer(&text);
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        let discovery = discover(&on.inputs, &Labels::of_answer(&answer), &on.repo);
        let at = |file: &str, line: u32| -> Vec<&DiscoverySite> {
            discovery
                .sites
                .iter()
                .filter(|site| site.file.ends_with(file) && site.line == line)
                .collect()
        };
        for (file, line) in [
            ("r3/sdk.ts", 11),
            ("r3/sdk.ts", 18),
            ("r3/sdk.ts", 25),
            ("r3/client/client.ts", 43),
        ] {
            let [site] = at(file, line)[..] else {
                panic!(
                    "a site at {file}:{line}: {:?} {:?}",
                    discovery.sites, discovery.unfollowed
                );
            };
            assert_eq!(site.stop, None, "{file}:{line}: {site:?}");
            let end = site.end.as_ref().expect("the call reaches fetch");
            assert_eq!(
                (end.package.as_str(), end.export.as_str()),
                ("lib:lib.dom.d.ts", "fetch"),
                "{file}:{line}"
            );
            assert!(end.file.ends_with("r3/client/client.ts"), "{end:?}");
            assert_eq!(end.line, 38, "{file}:{line}: {end:?}");
            let hops: Vec<(&str, u32)> = site
                .chain
                .iter()
                .map(|hop| (hop.function.as_str(), hop.line))
                .collect();
            assert_eq!(
                hops.last(),
                Some(&(VALUE_HOP, 31)),
                "{file}:{line}: the walk enters `request` as a value"
            );
        }
        // The send keeps its own site, with no chain.
        let [send] = at("r3/client/client.ts", 38)[..] else {
            panic!("one site at the send: {:?}", discovery.sites);
        };
        assert!(send.chain.is_empty(), "{send:?}");
    }

    /// carrick#2448 T3c, the other forms: a function declared inside a
    /// factory, and a method of an object literal the factory holds, each
    /// sending through `fetch`. A call through what the factory returns
    /// enters the inner function as a value and reaches the send.
    #[test]
    fn a_function_declared_or_held_inside_a_factory_is_entered() {
        let client = "export const make = (base: string) => {\n\
            \x20 async function send(path: string) {\n\
            \x20   return fetch(base + path);\n\
            \x20 }\n\
            \x20 const api = {\n\
            \x20   async load(path: string) {\n\
            \x20     return fetch(base + path);\n\
            \x20   },\n\
            \x20 };\n\
            \x20 const list = () => send('/a');\n\
            \x20 const read = () => api.load('/b');\n\
            \x20 return { list, read };\n\
            };\n";
        let use_ = "import { make } from './inner';\n\
            const c = make('http://localhost');\n\
            export const a = () => c.list();\n\
            export const b = () => c.read();\n";
        let Some(on) = walked(
            "generated-client",
            "",
            "",
            &[("src/t3c/inner.ts", client), ("src/t3c/use.ts", use_)],
            true,
        ) else {
            return;
        };
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/generated-client/__llm__/library-claims/default.json"),
        )
        .unwrap();
        let answer = crate::library_store::parse_answer(&text);
        let discovery = discover(&on.inputs, &Labels::of_answer(&answer), &on.repo);
        for (line, send, entered) in [(10, 3, 2), (11, 7, 6)] {
            let at: Vec<&DiscoverySite> = discovery
                .sites
                .iter()
                .filter(|site| site.file.ends_with("t3c/inner.ts") && site.line == line)
                .collect();
            let [site] = at[..] else {
                panic!(
                    "a site at t3c/inner.ts:{line}: {:?} {:?}",
                    discovery.sites, discovery.unfollowed
                );
            };
            assert_eq!(site.stop, None, "t3c/inner.ts:{line}: {site:?}");
            let end = site.end.as_ref().expect("the call reaches fetch");
            assert_eq!(end.export, "fetch", "t3c/inner.ts:{line}");
            assert_eq!(end.line, send, "t3c/inner.ts:{line}: {end:?}");
            let hops: Vec<(&str, u32)> = site
                .chain
                .iter()
                .map(|hop| (hop.function.as_str(), hop.line))
                .collect();
            assert_eq!(
                hops.last(),
                Some(&(VALUE_HOP, entered)),
                "t3c/inner.ts:{line}: {hops:?}"
            );
        }
    }

    /// carrick#2448 T1, soundness rule 2.3.3 across modules: the object a
    /// module binding holds is read at a key only where no importer hands it
    /// on. A module that passes `client` to a call may have it changed, so
    /// the calls through it enter nothing.
    #[test]
    fn an_object_an_importer_hands_on_is_not_read_at_its_key() {
        let Some(on) = walked(
            "generated-client",
            "",
            "",
            &[(
                "src/r1/leak.ts",
                "import { client } from './client';\nexport const leak = (use: (c: unknown) => void) => use(client);\n",
            )],
            true,
        ) else {
            return;
        };
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/generated-client/__llm__/library-claims/default.json"),
        )
        .unwrap();
        let answer = crate::library_store::parse_answer(&text);
        let discovery = discover(&on.inputs, &Labels::of_answer(&answer), &on.repo);
        let reached: Vec<u32> = discovery
            .sites
            .iter()
            .filter(|site| site.file.ends_with("r1/sdk.ts") && site.end.is_some())
            .map(|site| site.line)
            .collect();
        // The calls through the module binding (`client.get`, line 10) are
        // contested; the ones through `options.client ?? client` read
        // `client` the same way.
        assert!(reached.is_empty(), "{reached:?}");
    }

    /// carrick#2432 slice A2: a factory that returns a function it declares,
    /// which sends through `options.fetch ?? globalThis.fetch.bind(globalThis)`.
    /// A call through what the factory returned is a site whose end is the
    /// global `fetch`, reached through the value hop at the declared
    /// function; a factory returning a function that sends nothing is no
    /// site. The answer key is
    /// `tests/fixtures/discovery-factory-function/README.md`.
    #[test]
    fn a_function_a_factory_declares_and_returns_is_read_through_a_bound_fetch() {
        let Some(on) = walked(
            "discovery-factory-function",
            "services/web",
            "web",
            &[],
            true,
        ) else {
            return;
        };
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/discovery-factory-function/library-claims.json"),
        )
        .unwrap();
        let answer = crate::library_store::parse_answer(&text);
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        let discovery = discover(&on.inputs, &Labels::of_answer(&answer), &on.repo);
        let at = |file: &str, line: u32| -> Vec<&DiscoverySite> {
            discovery
                .sites
                .iter()
                .filter(|site| site.file.ends_with(file) && site.line == line)
                .collect()
        };
        for line in [5, 10] {
            let [site] = at("items.ts", line)[..] else {
                panic!(
                    "a site at items.ts:{line}: {:?} {:?}",
                    discovery.sites, discovery.unfollowed
                );
            };
            let end = site.end.as_ref().expect("the call reaches fetch");
            assert_eq!(
                (end.package.as_str(), end.export.as_str()),
                ("lib:lib.dom.d.ts", "fetch")
            );
            assert!(end.file.ends_with("http.ts"), "{end:?}");
            let hops: Vec<(&str, u32)> = site
                .chain
                .iter()
                .map(|hop| (hop.function.as_str(), hop.line))
                .collect();
            assert!(
                hops.contains(&(VALUE_HOP, 9)),
                "the chain goes through the declared function: {hops:?}"
            );
        }
        assert!(
            at("plain.ts", 10).iter().all(|site| site.end.is_none()),
            "a factory's function that sends nothing is no site"
        );
        // `options.fetch.bind(options)` holds the factory's parameter. The
        // closure that reads it is one function further in, so the parameter
        // is not the closure's own first one, which the caller fills with the
        // global `fetch`: the call reaches no labelled export.
        assert!(
            at("bound_items.ts", 9)
                .iter()
                .all(|site| site.end.is_none()),
            "a captured parameter inside a bound object is read one function out: {:?}",
            discovery.sites
        );
    }

    /// carrick#3504 S0b: a function a factory returns, called where the
    /// factory's result is held, reads the factory's parameters from the call
    /// that made it, whatever depth the maker call is written at. A key the
    /// maker call does not write adds nothing; one it fills with a function
    /// that sends nothing keeps the call undecided. The answer key is
    /// `tests/fixtures/factory-closure/README.md`.
    #[test]
    fn a_function_a_factory_returns_reads_the_factorys_parameter_from_the_call_that_made_it() {
        let Some(on) = walked("factory-closure", "services/web", "web", &[], true) else {
            return;
        };
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/factory-closure/library-claims.json"),
        )
        .unwrap();
        let answer = crate::library_store::parse_answer(&text);
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        let discovery = discover(&on.inputs, &Labels::of_answer(&answer), &on.repo);
        let at = |file: &str, line: u32| -> Vec<&DiscoverySite> {
            discovery
                .sites
                .iter()
                .chain(&discovery.unfollowed)
                .filter(|site| site.file.ends_with(file) && site.line == line)
                .collect()
        };
        let show = |file: &str, line: u32| -> String {
            at(file, line)
                .iter()
                .map(|site| {
                    format!(
                        "{}:{} end={:?} stop={:?}",
                        site.file,
                        site.line,
                        site.end
                            .as_ref()
                            .map(|end| (end.package.as_str(), end.export.as_str())),
                        site.stop
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        };
        let decided = |file: &str, line: u32, package: &str, export: &str| -> bool {
            let sites = at(file, line);
            !sites.is_empty()
                && sites.iter().all(|site| {
                    site.stop.is_none()
                        && site.end.as_ref().is_some_and(|end| {
                            (end.package.as_str(), end.export.as_str()) == (package, export)
                        })
                })
        };
        let mut wrong = Vec::new();
        for (file, line) in [
            // The factory's parameter, read in the function it returns.
            ("s-closure-coalesce.ts", 8),
            ("s-closure-coalesce-arrow.ts", 7),
            ("s-closure-param.ts", 7),
            ("s-closure-param-key.ts", 7),
            ("s-closure-param-local.ts", 8),
            // The maker call passes the outer factory's own parameter.
            ("s-two-level.ts", 17),
            ("t-real.ts", 26),
            // Already decided: a factory local, a direct send, a closed URL.
            ("s-closure-local.ts", 7),
            ("s-two-level-direct.ts", 11),
            ("z-literal-fn.ts", 5),
        ] {
            if !decided(file, line, "@fixture/send", "send") {
                wrong.push(format!("{file}:{line} not decided: {}", show(file, line)));
            }
        }
        if !decided("b-closure-coalesce.ts", 5, "@fixture/pub", "publish") {
            wrong.push(format!(
                "b-closure-coalesce.ts:5 not decided: {}",
                show("b-closure-coalesce.ts", 5)
            ));
        }
        if at("b-closure-coalesce.ts", 8)
            .iter()
            .any(|site| site.stop == Some(Stop::InjectedCallee))
        {
            wrong.push(format!(
                "b-closure-coalesce.ts:8 injected: {}",
                show("b-closure-coalesce.ts", 8)
            ));
        }
        // The maker call fills `send` with a function that sends nothing:
        // the values disagree.
        let several = |file: &str, line: u32| -> bool {
            let sites = at(file, line);
            !sites.is_empty()
                && sites
                    .iter()
                    .all(|site| site.stop == Some(Stop::SeveralValues))
        };
        if !several("s-closure-coalesce-filled.ts", 9) {
            wrong.push(format!(
                "s-closure-coalesce-filled.ts:9 not several: {}",
                show("s-closure-coalesce-filled.ts", 9)
            ));
        }
        // The maker call writes a module binding in another function: its
        // parameter is no parameter of the body that calls the binding, so
        // what that body's caller passes does not decide it.
        if !several("c-binding-maker.ts", 11) {
            wrong.push(format!(
                "c-binding-maker.ts:11 not several: {}",
                show("c-binding-maker.ts", 11)
            ));
        }
        // The maker call is in another factory that returns its result: its
        // parameter is `outer`'s, which `f`'s call fills with `quiet`.
        if !several("d-nested-factory.ts", 10) {
            wrong.push(format!(
                "d-nested-factory.ts:10 not several: {}",
                show("d-nested-factory.ts", 10)
            ));
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// carrick#2813 S1: a walk that passed a call into a package the store has
    /// not answered records the package, whether it ends at a wrapper chain
    /// or at a value callee a parameter's fallback decides, and a walk that
    /// read code and missed inside is `missed_inside` even when the call's
    /// own row reads `value_callee`. A walk over answered packages, and a
    /// site, record none. The answer key is
    /// `tests/fixtures/discovery-pending-package/README.md`.
    #[test]
    fn a_walk_that_passed_a_package_the_store_has_not_answered_records_it() {
        let Some(on) = walked(
            "discovery-pending-package",
            "services/web",
            "web",
            &[],
            true,
        ) else {
            return;
        };
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/discovery-pending-package");
        let answer = crate::library_store::parse_answer(
            &std::fs::read_to_string(fixture.join("library-claims.json")).unwrap(),
        );
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        let discovery = discover(&on.inputs, &Labels::of_answer(&answer), &on.repo);
        let span = |file: &str, needle: &str| -> (String, u32, u32) {
            let text =
                std::fs::read_to_string(fixture.join("repo/services/web/src").join(file)).unwrap();
            let lo = text.find(needle).unwrap() as u32 + 1;
            (
                format!("services/web/src/{file}"),
                lo,
                lo + needle.len() as u32,
            )
        };
        let keys = |file: &str, needle: &str| -> Vec<String> {
            discovery
                .unanswered_at
                .get(&span(file, needle))
                .map(|keys| keys.iter().cloned().collect())
                .unwrap_or_default()
        };
        let pending = vec!["fixture-pending".to_string()];
        // A wrapper chain: `postEvent` -> `send` -> a call into the package.
        assert_eq!(keys("events.ts", "send(\"/events\", body)"), pending);
        // A value callee: `deps.send ?? sendViaClient`.
        assert_eq!(keys("inject.ts", "doSend(\"/notify\", body)"), pending);
        // Both are walks that read code and missed inside, never `finished`
        // and never recorded as the call's own row.
        for (file, needle) in [
            ("events.ts", "send(\"/events\", body)"),
            ("inject.ts", "doSend(\"/notify\", body)"),
        ] {
            assert!(
                discovery.missed_inside.contains(&span(file, needle)),
                "{file}: missed_inside {:?} unfollowed {:?} sites {:?}",
                discovery.missed_inside,
                discovery.unfollowed,
                discovery.sites
            );
        }
        // The controls: a walk over an answered package with no export, and
        // a site, pass no unanswered package.
        assert!(keys("answered.ts", "render(body)").is_empty());
        assert!(
            discovery
                .finished
                .contains(&span("answered.ts", "render(body)")),
            "{:?}",
            discovery.finished
        );
        assert!(keys("global.ts", "request(\"/items\")").is_empty());
        assert!(
            discovery
                .sites
                .iter()
                .any(|site| site.file == "services/web/src/global.ts" && site.end.is_some())
        );
        // A call inside the package's wrapper that goes straight to it is
        // the direct call Rule 4 lists: it keeps its own record.
        assert!(
            !discovery
                .unanswered_at
                .contains_key(&span("client.ts", "transmit(path, body)")),
            "a direct call records no passed package"
        );
    }

    /// What a pass over `discovery-pending-package` reads, as one string per
    /// field the settled pass could move.
    fn pending_package_record(discovery: &Discovery) -> Vec<String> {
        vec![
            format!("{:?}", discovery.sites),
            format!("{:?}", discovery.unfollowed),
            format!("{:?}", discovery.finished),
            format!("{:?}", discovery.missed_inside),
            format!("{:?}", discovery.unanswered_at),
            format!("{:?}", discovery.entered),
            format!("{:?}", discovery.counts),
        ]
    }

    /// carrick#3684: the settled pass reads the packages the scan ended
    /// without (`Labels::ended_without`), as the first pass reads the ones
    /// the answer does not hold. The answer key is
    /// `tests/fixtures/discovery-pending-package/README.md`.
    #[test]
    fn the_settled_pass_reads_a_package_the_scan_ended_without() {
        let Some(on) = walked(
            "discovery-pending-package",
            "services/web",
            "web",
            &[],
            true,
        ) else {
            return;
        };
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/discovery-pending-package");
        let answer = crate::library_store::parse_answer(
            &std::fs::read_to_string(fixture.join("library-claims.json")).unwrap(),
        );
        let unready: BTreeSet<String> = ["fixture-pending".to_string()].into();
        let plain = discover(&on.inputs, &Labels::of(&answer.exports), &on.repo);
        let settled = discover(
            &on.inputs,
            &Labels::of(&answer.exports).ended_without(&unready),
            &on.repo,
        );
        let span = |file: &str, needle: &str| -> (String, u32, u32) {
            let text =
                std::fs::read_to_string(fixture.join("repo/services/web/src").join(file)).unwrap();
            let lo = text.find(needle).unwrap() as u32 + 1;
            (
                format!("services/web/src/{file}"),
                lo,
                lo + needle.len() as u32,
            )
        };
        let pending: BTreeSet<String> = ["fixture-pending".to_string()].into();
        for (file, needle) in [
            ("events.ts", "send(\"/events\", body)"),
            ("inject.ts", "doSend(\"/notify\", body)"),
        ] {
            let at = span(file, needle);
            assert!(settled.missed_inside.contains(&at), "{file} missed_inside");
            assert_eq!(settled.unanswered_at.get(&at), Some(&pending), "{file}");
            assert!(
                !plain.missed_inside.contains(&at),
                "{file} on the plain pass"
            );
        }
        let direct = span("client.ts", "transmit(path, body)");
        assert!(
            settled.unfollowed.iter().any(|site| (
                site.file.clone(),
                site.span_start,
                site.span_end
            ) == direct
                && site.stop == Some(Stop::UnansweredPackage)),
            "{:?}",
            settled.unfollowed
        );
        assert!(
            settled
                .finished
                .contains(&span("answered.ts", "render(body)"))
        );
        assert!(
            settled
                .sites
                .iter()
                .any(|site| site.file == "services/web/src/global.ts" && site.end.is_some())
        );
        let with_end = |discovery: &Discovery| -> Vec<String> {
            discovery
                .sites
                .iter()
                .filter(|site| site.end.is_some())
                .map(|site| format!("{site:?}"))
                .collect()
        };
        assert_eq!(with_end(&settled), with_end(&plain));
        assert_eq!(
            format!("{:?}", settled.entered),
            format!("{:?}", plain.entered)
        );
    }

    /// carrick#3992 T2a, step 0: a call through a `#private` field and one
    /// through a public field, both holding a client from a package the
    /// answer holds no entry for, are read by `package_reason` once the scan
    /// ends without that package. The answer key is
    /// `tests/fixtures/discovery-absent-package/README.md`.
    #[test]
    fn a_client_in_a_package_the_answer_lacks_is_unanswered_when_the_scan_ends_without_it() {
        let Some(on) = walked("discovery-absent-package", "services/web", "web", &[], true) else {
            return;
        };
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/discovery-absent-package");
        let answer = crate::library_store::parse_answer(
            &std::fs::read_to_string(fixture.join("library-claims.json")).unwrap(),
        );
        let unready: BTreeSet<String> = ["fixture-graph".to_string()].into();
        let plain = discover(&on.inputs, &Labels::of(&answer.exports), &on.repo);
        let settled = discover(
            &on.inputs,
            &Labels::of(&answer.exports).ended_without(&unready),
            &on.repo,
        );
        let span = |file: &str, needle: &str| -> (String, u32, u32) {
            let text =
                std::fs::read_to_string(fixture.join("repo/services/web/src").join(file)).unwrap();
            let lo = text.find(needle).unwrap() as u32 + 1;
            (
                format!("services/web/src/{file}"),
                lo,
                lo + needle.len() as u32,
            )
        };
        let held = |discovery: &Discovery, at: &(String, u32, u32)| {
            discovery.unfollowed.iter().any(|site| {
                (site.file.clone(), site.span_start, site.span_end) == *at
                    && site.stop == Some(Stop::UnansweredPackage)
            })
        };
        let context = format!(
            "unfollowed {:?}\nfinished {:?}\nmissed_inside {:?}",
            settled.unfollowed, settled.finished, settled.missed_inside
        );
        for needle in [
            "this.#graph.api(\"/me\").get()",
            "this.open.api(\"/me/messages\").get()",
        ] {
            let at = span("mailbox.ts", needle);
            assert!(
                held(&settled, &at),
                "{needle} on the settled pass\n{context}"
            );
            assert!(!held(&plain, &at), "{needle} on the plain pass");
        }
        assert!(
            settled
                .finished
                .contains(&span("answered.ts", "render(body)")),
            "an answered package with nothing to label stays silent"
        );
    }

    /// carrick#3684: with nothing unready the settled pass is the plain one.
    #[test]
    fn with_nothing_unready_the_settled_pass_is_unchanged() {
        let Some(on) = walked(
            "discovery-pending-package",
            "services/web",
            "web",
            &[],
            true,
        ) else {
            return;
        };
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/discovery-pending-package");
        let answer = crate::library_store::parse_answer(
            &std::fs::read_to_string(fixture.join("library-claims.json")).unwrap(),
        );
        let none = BTreeSet::new();
        assert_eq!(
            pending_package_record(&discover(
                &on.inputs,
                &Labels::of(&answer.exports),
                &on.repo
            )),
            pending_package_record(&discover(
                &on.inputs,
                &Labels::of(&answer.exports).ended_without(&none),
                &on.repo
            )),
        );
    }

    /// carrick#3684: a default-library key is one whole file, so it is never
    /// unready in the settled pass, whatever the set holds.
    #[test]
    fn a_language_key_is_never_unready() {
        let Some(on) = walked("discovery", "services/web", "web", &[], true) else {
            return;
        };
        let keys: BTreeSet<String> = on
            .inputs
            .positions
            .as_ref()
            .map(|positions| {
                positions
                    .call_rows()
                    .flat_map(|row| store_keys(row, &[]))
                    .filter(|key| is_language_key(key))
                    .collect()
            })
            .unwrap_or_default();
        assert!(!keys.is_empty(), "the fixture's rows name a language key");
        let plain = discover(&on.inputs, &Labels::of(&[]), &on.repo);
        let settled = discover(&on.inputs, &Labels::of(&[]).ended_without(&keys), &on.repo);
        assert_eq!(
            pending_package_record(&plain),
            pending_package_record(&settled)
        );
    }

    /// carrick#3527: a call into code of the run that hands its callee a
    /// function the callee keeps and never runs (an object argument holding
    /// `handler`, or a callback argument it only stores) has not been read
    /// whole: the function runs later, from wherever the callee put it. Such
    /// a call is `missed_inside`, in a constructor and in a method alike. A
    /// callee that runs its callback, and a library callee, stay `finished`.
    /// The answer key is `tests/fixtures/discovery-handed-registry/README.md`.
    #[test]
    fn a_call_that_hands_its_callee_a_function_it_keeps_is_not_finished() {
        let Some(on) = walked(
            "discovery-handed-registry",
            "services/relay",
            "relay",
            &[],
            true,
        ) else {
            return;
        };
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/discovery-handed-registry");
        let answer = crate::library_store::parse_answer(
            &std::fs::read_to_string(fixture.join("library-claims.json")).unwrap(),
        );
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        let discovery = discover(&on.inputs, &Labels::of_answer(&answer), &on.repo);
        let span = |file: &str, needle: &str| -> (String, u32, u32) {
            let text = std::fs::read_to_string(fixture.join("repo/services/relay/src").join(file))
                .unwrap();
            let lo = text.find(needle).unwrap() as u32 + 1;
            (
                format!("services/relay/src/{file}"),
                lo,
                lo + needle.len() as u32,
            )
        };
        for needle in [
            // In the constructor.
            "this.add(\"/gauges\", \"GET\", { handler: async ({ reply }) => reply.text(\"ok\") })",
            // In a method.
            "this.add(\"/pulse\", \"GET\", { handler: async ({ reply }) => reply.text(\"up\") })",
            "this.addFn(\"/beat\", async ({ reply }) => reply.text(\"b\"))",
        ] {
            let at = span("relay-server.ts", needle);
            assert!(
                discovery.missed_inside.contains(&at) && !discovery.finished.contains(&at),
                "{needle}: missed_inside {:?} finished {:?}",
                discovery.missed_inside,
                discovery.finished
            );
        }
        // C1: the callee runs the callback it is handed. Its `fn()` stops
        // the walk there, as before: never `missed_inside`.
        let runs = span(
            "relay-server.ts",
            "this.runNow(() => {\n      this.count += 1;\n    })",
        );
        assert!(!discovery.missed_inside.contains(&runs));
        assert!(
            discovery.sites.iter().any(|site| site.file == runs.0
                && site.span_start == runs.1
                && site.stop == Some(Stop::InjectedCallee)),
            "{:?}",
            discovery.sites
        );
        // C2: a library callee handed an object holding a function.
        let library = span("configure.ts", "configure({ onIdle: () => {} })");
        // C3: a function written in the caller's body, not at the call.
        let elsewhere = span("relay-server.ts", "this.record(\"/note\")");
        for at in [library, elsewhere] {
            assert!(
                discovery.finished.contains(&at),
                "{at:?}: finished {:?} missed_inside {:?} unfollowed {:?}",
                discovery.finished,
                discovery.missed_inside,
                discovery.unfollowed
            );
        }
    }

    /// Discovery over `tests/fixtures/callee-values` with its one label.
    fn callee_values_discovery() -> Option<Discovery> {
        let (_, discovery) = fixture_discovery("callee-values")?;
        Some(discovery)
    }

    /// Discovery over the `services/web` service of a fixture that carries
    /// `library-claims.json`, with the walk it ran over.
    fn fixture_discovery(fixture: &str) -> Option<(super::super::fixture_walk::Walked, Discovery)> {
        let on = walked(fixture, "services/web", "web", &[], true)?;
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(fixture)
                .join("library-claims.json"),
        )
        .unwrap();
        let answer = crate::library_store::parse_answer(&text);
        let discovery = discover(&on.inputs, &Labels::of_answer(&answer), &on.repo);
        Some((on, discovery))
    }

    /// What the walk records at `file:line`: `site` for a call decided to
    /// one end, else each stop (a site kept with a stop, or the reason the
    /// call was not followed).
    fn recorded_at(discovery: &Discovery, file: &str, line: u32) -> Vec<String> {
        let mut out: Vec<String> = discovery
            .sites
            .iter()
            .chain(&discovery.unfollowed)
            .filter(|site| site.file.ends_with(file) && site.line == line)
            .map(|site| match (&site.end, &site.stop) {
                (Some(end), None) => format!("site {}", end.export),
                (_, Some(stop)) => stop.as_str().to_string(),
                (None, None) => "none".to_string(),
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// carrick#3504 S1b (#3485 row 2): a callee held in a function's own
    /// `let` that the body writes again is every value written to it, read
    /// through the same binding table and write census as a module
    /// binding: its initialiser and each assignment, a value of the
    /// function's parameter read at its callers. Values that reach
    /// different ends, a parameter no caller fills, and a write inside a
    /// callback keep the call undecided. The answer key is
    /// `tests/fixtures/callee-values/README.md`.
    #[test]
    fn a_callee_held_in_a_function_local_let_is_each_value_written_to_it() {
        let Some(discovery) = callee_values_discovery() else {
            return;
        };
        let decided = vec!["site fetch".to_string()];
        let several = vec!["several_values".to_string()];
        let wrong: Vec<String> = [
            (
                "f-local-agree.ts",
                8,
                &decided,
                "two values that reach one end",
            ),
            (
                "f-local-assigned.ts",
                6,
                &decided,
                "declared bare, written once",
            ),
            (
                "f-local-param-called.ts",
                8,
                &decided,
                "a parameter its one caller fills",
            ),
            (
                "f-local-init-param.ts",
                8,
                &decided,
                "initialised from a filled parameter",
            ),
            (
                "f-local-let.ts",
                8,
                &several,
                "one write to a function that sends nothing",
            ),
            (
                "f-local-param.ts",
                8,
                &several,
                "a parameter no caller fills",
            ),
            (
                "f-local-nested.ts",
                8,
                &several,
                "a write inside a callback",
            ),
        ]
        .into_iter()
        .filter_map(|(file, line, want, form)| {
            let got = recorded_at(&discovery, file, line);
            (got != *want).then(|| format!("{file}:{line} ({form}): {got:?}, want {want:?}"))
        })
        .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// carrick#3504 S4b (#3485 row 3): a callee read off an entry of an
    /// object literal the module or function keeps (`d["a"]()`, `d[k]()` for
    /// a `const` key, `d.a()`) is the value written at that key. An object
    /// something writes through, an exported one and an open one keep the
    /// call undecided. The answer key is
    /// `tests/fixtures/callee-values/README.md`.
    #[test]
    fn a_callee_read_off_a_kept_objects_entry_is_that_entrys_value() {
        let Some(discovery) = callee_values_discovery() else {
            return;
        };
        let at = |file: &str, line: u32| recorded_at(&discovery, file, line);
        let decided = ["site fetch".to_string()];
        let wrong: Vec<String> = [
            ("f-keyed.ts", 6, "a string-literal key"),
            ("f-keyed-const.ts", 7, "a const key"),
            ("f-entry-dot.ts", 6, "a dotted key"),
            ("f-entry-method.ts", 10, "a method written in the object"),
            ("f-local-keyed.ts", 5, "a function-local object"),
            (
                "f-keyed-exported.ts",
                6,
                "an exported object no importer writes",
            ),
        ]
        .into_iter()
        .filter_map(|(file, line, form)| {
            let got = at(file, line);
            (got != decided).then(|| format!("{file}:{line} ({form}): {got:?}"))
        })
        .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
        for (file, line, form) in [
            ("f-keyed-written.ts", 10, "an entry the module writes"),
            (
                "f-keyed-contested.ts",
                6,
                "an exported object an importer writes",
            ),
            ("f-keyed-spread.ts", 5, "an open object"),
        ] {
            assert_ne!(
                at(file, line),
                decided,
                "{file}:{line} ({form}) is undecided"
            );
        }
    }

    /// carrick#3504 S4b-2: a name that holds an object's entry (`const g =
    /// d.a`, `d["a"]`, one inside a function, one written into another
    /// object) is that entry's value, read as `d.a` is. An entry something
    /// writes and an open object keep the call undecided. The answer key is
    /// `tests/fixtures/callee-values/README.md`.
    #[test]
    fn a_callee_held_in_an_alias_of_an_entry_is_that_entrys_value() {
        let Some(discovery) = callee_values_discovery() else {
            return;
        };
        let at = |file: &str, line: u32| recorded_at(&discovery, file, line);
        let decided = ["site fetch".to_string()];
        let wrong: Vec<String> = [
            ("f-alias-dot.ts", 7, "a dotted alias"),
            ("f-alias-keyed.ts", 7, "a keyed alias"),
            ("f-alias-local.ts", 7, "an alias inside a function"),
            (
                "f-alias-into-object.ts",
                7,
                "an alias written into another object",
            ),
        ]
        .into_iter()
        .filter_map(|(file, line, form)| {
            let got = at(file, line);
            (got != decided).then(|| format!("{file}:{line} ({form}): {got:?}"))
        })
        .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
        for (file, line, form) in [
            ("f-alias-written.ts", 11, "an entry the module writes"),
            ("f-alias-spread.ts", 6, "an open object"),
        ] {
            assert_ne!(
                at(file, line),
                decided,
                "{file}:{line} ({form}) is undecided"
            );
        }
    }

    /// carrick#3504 S4b-3a: an object a module keeps to itself is read
    /// whatever the service's exports are. An import used only as a
    /// shorthand property leaves the service open, which contests every
    /// export; it does not contest a binding no other module can reach.
    /// The answer key is `tests/fixtures/callee-open/README.md`.
    #[test]
    fn a_private_objects_entry_is_read_when_an_import_is_unread() {
        let Some((on, discovery)) = fixture_discovery("callee-open") else {
            return;
        };
        assert!(
            on.inputs.links.as_ref().unwrap().open(),
            "the fixture's service must be open for this test to mean anything"
        );
        assert_eq!(
            recorded_at(&discovery, "f-keyed-open.ts", 6),
            ["site fetch".to_string()]
        );
        assert_ne!(
            recorded_at(&discovery, "f-keyed-exported-open.ts", 6),
            ["site fetch".to_string()],
            "an exported object stays contested in an open service"
        );
    }

    /// carrick#3504 S4c: a callee held in an instance field is the field's
    /// initialiser and every value written to it in any member, the
    /// constructor's included, and `this.g` read in one is that field. A
    /// field the class cannot place every write of (`this` handed out)
    /// holds nothing a reader states, and a write of a member's parameter is
    /// unread. The answer key is `tests/fixtures/callee-values/README.md`.
    #[test]
    fn a_callee_held_in_a_field_is_its_initialiser_and_every_write() {
        let Some(discovery) = callee_values_discovery() else {
            return;
        };
        let decided = vec!["site fetch".to_string()];
        let several = vec!["several_values".to_string()];
        let wrong: Vec<String> = [
            ("f-field-init.ts", 7, true, "a property initialiser"),
            ("f-field-copy.ts", 8, true, "f = this.g, g = send"),
            ("f-field-ctor-copy.ts", 13, true, "ctor this.f = this.g"),
            (
                "f-field-method-agree.ts",
                11,
                true,
                "a method writes the same end",
            ),
            (
                "f-field-method-write.ts",
                15,
                false,
                "a method writes another",
            ),
            (
                "f-field-param.ts",
                11,
                false,
                "a method writes its parameter",
            ),
            (
                "f-field-escape.ts",
                15,
                true,
                "ctor sets it, this handed to a call",
            ),
            (
                "f-field-escape.ts",
                23,
                false,
                "a method alone writes it, this handed to a call",
            ),
        ]
        .into_iter()
        .filter_map(|(file, line, is_decided, form)| {
            let got = recorded_at(&discovery, file, line);
            let ok = if is_decided {
                got == decided
            } else {
                got != decided && (got == several || file == "f-field-escape.ts")
            };
            (!ok).then(|| format!("{file}:{line} ({form}): {got:?}"))
        })
        .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// carrick#3504 S1 (#3485 rows 1-2): a callee held in a module `const`
    /// alias is the function the alias names, and one held in a module `let`
    /// is every value written to it, its initialiser and each assignment.
    /// Values that reach different ends, a write of an exported setter's
    /// parameter, a write the census cannot place (a destructuring target,
    /// a write inside a callback) keep the call undecided. The answer key is
    /// `tests/fixtures/callee-values/README.md`.
    #[test]
    fn a_callee_held_in_a_module_const_or_let_is_each_value_written_to_it() {
        let Some(discovery) = callee_values_discovery() else {
            return;
        };
        let at = |file: &str, line: u32| recorded_at(&discovery, file, line);
        let decided = ["site fetch".to_string()];
        assert_eq!(at("f-direct.ts", 4), decided, "the control");
        assert_eq!(at("f-alias.ts", 6), decided, "a module const alias");
        assert_eq!(at("f-let-never.ts", 6), decided, "a let never written");
        assert_eq!(
            at("f-let-agree.ts", 10),
            decided,
            "a let whose two values reach one end"
        );
        let several = ["several_values".to_string()];
        assert_eq!(
            at("f-let.ts", 10),
            several,
            "a let one write sets to a function that sends nothing"
        );
        assert_eq!(
            at("f-let-param.ts", 10),
            several,
            "a let an exported setter writes its parameter to"
        );
        assert_eq!(
            at("f-let-unplaced.ts", 10),
            several,
            "a let a destructuring writes"
        );
        assert_eq!(
            at("f-let-nested.ts", 12),
            several,
            "a let a callback writes"
        );
    }

    /// carrick#3504 S4a (#3485 row 4): a labelled export's member held as a
    /// value, bound (`http.post.bind(http)`) or read bare (`http.post`),
    /// ends at the op the member's declaration names, at the call that runs
    /// the value. HTTP and a broker's publish; an own function bound with
    /// only a `this` was followed already (`m-own-bind.ts`, the pin). The
    /// answer key is `tests/fixtures/callee-members/README.md`.
    #[test]
    fn a_labelled_members_value_reaches_its_op() {
        let Some((on, exports, declarations)) =
            traced_fixture_in("callee-members", "services/web", "web")
        else {
            return;
        };
        let discovery = discover(
            &on.inputs,
            &Labels::of(&exports).tracing(&declarations),
            &on.repo,
        );
        let wrong: Vec<String> = [
            ("m-bind.ts", 6, "site http"),
            ("m-extract.ts", 6, "site http"),
            // S4a pin for S4b-2: a library member held, not an object entry.
            ("m-alias-member.ts", 6, "site http"),
            ("m-bind-inline.ts", 4, "site http"),
            ("m-publish-bind.ts", 6, "site bus"),
            ("m-publish-keyed.ts", 7, "site bus"),
            ("m-computed.ts", 6, "site http"),
            ("m-keyed-arrow.ts", 6, "site http"),
            // `deps.post` is a read, not a call: only a callee is read off
            // an entry (S4b), so the union stays undecided.
            ("m-fallback.ts", 9, "several_values"),
            ("m-own-bind.ts", 10, "site http"),
        ]
        .into_iter()
        .filter_map(|(file, line, want)| {
            let got = recorded_at(&discovery, file, line);
            (got != [want.to_string()]).then(|| format!("{file}:{line} want {want}, got {got:?}"))
        })
        .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// carrick#3504 S4d: `f.call(t, a)` and `f.apply(t, [a])`, the row
    /// naming the language's member, run `f` with the arguments shifted past
    /// `this`: an own function's parameter, or a labelled member's name
    /// slot, is filled from the argument after `this`. A user object's own
    /// `call` method shifts nothing. The answer key is
    /// `tests/fixtures/callee-members/README.md`.
    #[test]
    fn a_call_or_apply_fills_the_shifted_parameter() {
        let Some((on, exports, declarations)) =
            traced_fixture_in("callee-members", "services/web", "web")
        else {
            return;
        };
        let scopes = super::super::decided_sites::ScopeLabel::from_exports(
            &exports,
            &declarations.scope_answers,
        );
        let (discovery, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let name_at = |file: &str, line: u32| -> Vec<String> {
            let mut out: Vec<String> = decided
                .sites
                .iter()
                .filter(|site| site.file.ends_with(file) && site.line == line)
                .map(|site| {
                    site.name
                        .iter()
                        .map(|piece| match piece {
                            Piece::Lit(text) => text.clone(),
                            _ => "?".to_string(),
                        })
                        .collect()
                })
                .collect();
            out.sort();
            out
        };
        let wrong: Vec<String> = [
            ("m-call.ts", 4, "site http", "/p/m-call"),
            ("m-own-call.ts", 8, "site http", "/p/m-own-call"),
            ("m-own-apply.ts", 8, "site http", "/p/m-own-apply"),
            ("m-publish-call.ts", 8, "site bus", "m-publish-call"),
            ("m-rpc-call.ts", 12, "site http", "/p/m-rpc-call"),
        ]
        .into_iter()
        .filter_map(|(file, line, want, name)| {
            let got = recorded_at(&discovery, file, line);
            let named = name_at(file, line);
            (got != [want.to_string()] || named != [name.to_string()])
                .then(|| format!("{file}:{line} want {want} {name}, got {got:?} {named:?}"))
        })
        .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// carrick#3504 S4d-b: `f.bind(t, a)` binds `a` ahead of the arguments
    /// of the call that runs the result: an HTTP request and a broker
    /// publish name what the bind states, a call's own argument follows the
    /// bound ones, and `f.bind(t)` binds nothing. The answer key is
    /// `tests/fixtures/callee-members/README.md`.
    #[test]
    fn bound_arguments_fill_the_leading_parameters() {
        let Some((on, exports, declarations)) =
            traced_fixture_in("callee-members", "services/web", "web")
        else {
            return;
        };
        let scopes = super::super::decided_sites::ScopeLabel::from_exports(
            &exports,
            &declarations.scope_answers,
        );
        let (discovery, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let name_at = |file: &str, line: u32| -> Vec<String> {
            let mut out: Vec<String> = decided
                .sites
                .iter()
                .filter(|site| site.file.ends_with(file) && site.line == line)
                .map(|site| {
                    site.name
                        .iter()
                        .map(|piece| match piece {
                            Piece::Lit(text) => text.clone(),
                            _ => "?".to_string(),
                        })
                        .collect()
                })
                .collect();
            out.sort();
            out
        };
        let wrong: Vec<String> = [
            ("m-bound-http.ts", 9, "site http", "/p/m-bound-http"),
            ("m-bound-publish.ts", 9, "site bus", "m-bound-publish"),
            ("m-bound-split.ts", 9, "site http", "/p/m-bound-split"),
            ("m-own-bind-this.ts", 9, "site http", "/p/m-own-bind-this"),
        ]
        .into_iter()
        .filter_map(|(file, line, want, name)| {
            let got = recorded_at(&discovery, file, line);
            let named = name_at(file, line);
            (got != [want.to_string()] || named != [name.to_string()])
                .then(|| format!("{file}:{line} want {want} {name}, got {got:?} {named:?}"))
        })
        .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// carrick#2813 slice S2: a function written as a parameter's default
    /// (`doFetch = (url, init) => fetch(url, init)`), positional, at a
    /// destructured key or on a constructor, is read as the value the
    /// parameter holds. The call that invokes the value ends at the `fetch`
    /// the default calls, through a value hop at the default's line. The
    /// answer key is `tests/fixtures/discovery-default-function/README.md`.
    #[test]
    fn a_function_written_as_a_parameter_default_is_read_as_its_value() {
        let Some(on) = walked(
            "discovery-default-function",
            "services/web",
            "web",
            &[],
            true,
        ) else {
            return;
        };
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/discovery-default-function/library-claims.json"),
        )
        .unwrap();
        let answer = crate::library_store::parse_answer(&text);
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        let discovery = discover(&on.inputs, &Labels::of_answer(&answer), &on.repo);
        let at = |file: &str, line: u32| -> Vec<&DiscoverySite> {
            discovery
                .sites
                .iter()
                .filter(|site| site.file.ends_with(file) && site.line == line)
                .collect()
        };
        // (file, the call that invokes the value, the default's line)
        for (file, call, default) in [
            ("arrow.ts", 7, 5),
            ("destructured.ts", 7, 5),
            ("ctor.ts", 10, 6),
            ("several.ts", 4, 3),
        ] {
            let [site] = at(file, call)[..] else {
                panic!(
                    "a site at {file}:{call}: {:?} {:?}",
                    discovery.sites, discovery.unfollowed
                );
            };
            let end = site.end.as_ref().expect("the call reaches fetch");
            assert_eq!(
                (end.package.as_str(), end.export.as_str()),
                ("lib:lib.dom.d.ts", "fetch"),
                "{file}"
            );
            assert_eq!(end.line, default, "{file}: the end is the default's call");
            let hops: Vec<(&str, u32)> = site
                .chain
                .iter()
                .map(|hop| (hop.function.as_str(), hop.line))
                .collect();
            assert!(
                hops.contains(&(VALUE_HOP, default)),
                "{file}: the chain goes through the default: {hops:?}"
            );
        }
        // One caller fills nothing and another passes its own function: the
        // other value decides, and an end is kept.
        let [several] = at("several.ts", 4)[..] else {
            unreachable!("asserted above")
        };
        assert_eq!(several.stop, Some(Stop::SeveralValues));
        // The control: the default is the global `fetch` itself, a site on
        // main, and it stays one.
        let [control] = at("control.ts", 4)[..] else {
            panic!("a site at control.ts:4: {:?}", discovery.sites);
        };
        assert_eq!(
            control.end.as_ref().map(|end| end.export.as_str()),
            Some("fetch")
        );
        // The call inside a default is no site of its own: only the call that
        // invokes the value is one (arrow.ts:5 has the default's call).
        assert!(
            discovery
                .sites
                .iter()
                .filter(|site| site.file.ends_with("arrow.ts"))
                .all(|site| site.callee != "fetch"),
            "the default's own call is not walked as a site: {:?}",
            discovery.sites
        );
        // A default that calls nothing labelled ends nowhere.
        assert!(
            discovery
                .sites
                .iter()
                .filter(|site| site.file.ends_with("plain.ts"))
                .all(|site| site.end.is_none()),
            "{:?}",
            discovery.sites
        );
    }

    /// carrick#2808: a class no file of its service constructs, and that
    /// `constructed` did not refuse, is read through its constructor alone:
    /// a parameter holds its default, and a `??` fallback is the global
    /// `fetch` or the package's `publish`. A field with no fallback holds
    /// nothing, and a class the service constructs is still read through its
    /// constructions. The answer key is
    /// `tests/fixtures/discovery-unconstructed-class/README.md`.
    #[test]
    fn a_class_its_service_never_constructs_reads_its_fields_from_the_constructor() {
        let Some(on) = walked(
            "discovery-unconstructed-class",
            "services/client",
            "client",
            &[],
            true,
        ) else {
            return;
        };
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/discovery-unconstructed-class/library-claims.json"),
        )
        .unwrap();
        let answer = crate::library_store::parse_answer(&text);
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        let discovery = discover(&on.inputs, &Labels::of_answer(&answer), &on.repo);
        let at = |file: &str, line: u32| -> Vec<&DiscoverySite> {
            discovery
                .sites
                .iter()
                .filter(|site| site.file.ends_with(file) && site.line == line)
                .collect()
        };
        for line in [13, 17] {
            let [site] = at("items.ts", line)[..] else {
                panic!(
                    "a site at items.ts:{line}: {:?} {:?}",
                    discovery.sites, discovery.unfollowed
                );
            };
            let end = site.end.as_ref().expect("the call reaches fetch");
            assert_eq!(
                (end.package.as_str(), end.export.as_str()),
                ("lib:lib.dom.d.ts", "fetch")
            );
        }
        let [site] = at("events.ts", 13)[..] else {
            panic!(
                "a site at events.ts:13: {:?} {:?}",
                discovery.sites, discovery.unfollowed
            );
        };
        let end = site.end.as_ref().expect("the call reaches publish");
        assert_eq!(
            (end.package.as_str(), end.export.as_str()),
            ("fixture-bus", "publish")
        );
        assert!(
            at("bare.ts", 11).iter().all(|site| site.end.is_none()),
            "a field with no fallback holds nothing"
        );
        // The control: the service constructs this class with a function
        // that sends nothing, so it is read through that construction, as
        // before: two values, the stub and the default `fetch`.
        let [local] = at("local.ts", 11)[..] else {
            panic!("a site at local.ts:11: {:?}", discovery.sites);
        };
        assert_eq!(local.stop, Some(Stop::SeveralValues));
        assert_eq!(
            local.end.as_ref().map(|end| end.export.as_str()),
            Some("fetch")
        );
    }

    /// carrick#3688 U2: a callee whose every value the resolver reads to
    /// nothing for one reason states that reason, not `several_values`
    /// (`unread_twice.ts`: both callers spread). Values that stop in
    /// different ways, or one that says no reason, stay `several_values`
    /// and name the ways they stopped (`spread.ts`, R3). The answer key is
    /// `tests/fixtures/resolve-values/README.md`.
    #[test]
    fn a_callee_every_value_of_which_stops_one_way_states_that_way() {
        let Some(on) = walked("resolve-values", "services/web", "web", &[], true) else {
            return;
        };
        let discovery = discover(&on.inputs, &Labels::of(&[]), &on.repo);
        let at = |file: &str, line: u32| -> Vec<&DiscoverySite> {
            discovery
                .sites
                .iter()
                .filter(|site| site.file.ends_with(file) && site.line == line)
                .collect()
        };
        let [twice] = at("unread_twice.ts", 4)[..] else {
            panic!(
                "a site at unread_twice.ts:4: {:?} {:?}",
                discovery.sites, discovery.unfollowed
            );
        };
        assert_eq!(
            (twice.end.as_ref(), twice.stop, twice.also.as_slice()),
            (None, Some(Stop::UnreadExpression), &[][..])
        );
        let [mixed] = at("spread.ts", 8)[..] else {
            panic!(
                "a site at spread.ts:8: {:?} {:?}",
                discovery.sites, discovery.unfollowed
            );
        };
        assert_eq!(
            (mixed.end.as_ref(), mixed.stop, mixed.also.as_slice()),
            (
                None,
                Some(Stop::SeveralValues),
                &[Stop::UnreadExpression][..]
            )
        );
    }

    /// The seven-hop site has seven hops, and no site sits at the store
    /// initialiser's call.
    #[test]
    fn the_seven_hop_site_and_no_site_at_an_initialiser() {
        let Some(on) = walked("discovery", "services/web", "web", &[], true) else {
            return;
        };
        let exports = claims();
        let discovery = discover(&on.inputs, &Labels::of(&exports), &on.repo);
        let start = discovery
            .sites
            .iter()
            .find(|site| site.file.ends_with("deep.ts") && site.line == 30)
            .expect("the seven-hop site");
        assert_eq!(start.chain.len(), 7);
        assert!(
            !discovery
                .sites
                .iter()
                .any(|site| site.callee == "createStore"),
            "a closure a library only stores is not followed"
        );
    }

    /// A package's declaration for the library calls that run, keep or
    /// register what they are handed (carrick#2634).
    const RUNNER_DECLARATIONS: &str = r#"export interface Ctx { readonly id: string }
export interface Store<T> { get(): T }
export declare function within<A extends unknown[], F extends (...args: A) => ReturnType<F>>(ctx: Ctx, fn: F, ...args: A): ReturnType<F>;
export declare function keep<T>(init: () => T): Store<T>;
export declare function on(event: string, listener: () => void): void;
"#;

    const RUNNER_SOURCE: &str = r#"import { within, keep, on } from "fixture-runner";
const ctx = { id: "c" };
export async function sendWithin(url: string): Promise<Response> {
  return within(ctx, () => fetch(url));
}
export function loadWithin(): Promise<Response> {
  return sendWithin("/within");
}
export function chained(): Promise<Response> {
  return Promise.resolve("/then").then((path) => fetch(path));
}
export const kept = keep(() => fetch("/kept"));
export function listen(): void {
  on("ready", () => { void fetch("/ready"); });
}
"#;

    /// carrick#2634: a function written at a call into a library whose
    /// declaration returns what that function returns (a tracing runner, a
    /// promise's `then`) is walked, under a `<callback>` hop and none for the
    /// library; a function the library keeps or registers is not.
    #[test]
    fn a_function_a_library_call_runs_is_walked() {
        let Some(on) = walked(
            "discovery",
            "services/web",
            "web",
            &[
                (
                    "node_modules/fixture-runner/package.json",
                    r#"{ "name": "fixture-runner", "version": "1.0.0", "types": "index.d.ts" }"#,
                ),
                (
                    "node_modules/fixture-runner/index.d.ts",
                    RUNNER_DECLARATIONS,
                ),
                ("src/run-library.ts", RUNNER_SOURCE),
            ],
            true,
        ) else {
            return;
        };
        let exports = claims();
        let discovery = discover(&on.inputs, &Labels::of(&exports), &on.repo);
        let here: Vec<(&str, Vec<&str>)> = discovery
            .sites
            .iter()
            .filter(|site| site.file.ends_with("run-library.ts"))
            .map(|site| {
                (
                    site.callee.as_str(),
                    site.chain.iter().map(|hop| hop.function.as_str()).collect(),
                )
            })
            .collect();
        let at = |callee: &str| -> Vec<Vec<&str>> {
            here.iter()
                .filter(|(name, _)| name.rsplit('.').next() == Some(callee))
                .map(|(_, chain)| chain.clone())
                .collect()
        };
        assert_eq!(at("within"), vec![vec!["<callback>"]], "{here:?}");
        assert_eq!(
            at("sendWithin"),
            vec![vec!["sendWithin", "<callback>"]],
            "{here:?}"
        );
        assert_eq!(at("then"), vec![vec!["<callback>"]], "{here:?}");
        assert!(at("keep").is_empty() && at("on").is_empty(), "{here:?}");
        assert_eq!(
            at("fetch").len(),
            4,
            "each fetch written inside a function is a site of its own: {here:?}"
        );
    }

    /// The store's answer for the packages the published-name and global
    /// cases add, beside the fixture's own.
    const PUBLISHED_CLAIMS: &str = r#"{
  "library_claims": [
    {
      "package": "fixture-http", "version": "1.0.0", "status": "answered",
      "exports": [
        { "specifier": "fixture-http", "export": "default", "role": "http_client", "side": "client",
          "makes": [], "scopes": [],
          "ops": [ { "op": "request", "member": "get", "on": "export", "name": { "arg": 0 }, "picker": "fixture-picker" } ],
          "reserved": [], "calls": [], "patterns": [] }
      ]
    },
    {
      "package": "fixture-io", "version": "1.0.0", "status": "answered",
      "exports": [
        { "specifier": "fixture-io", "export": "io", "role": "socket", "side": "client",
          "makes": [ { "form": "call", "member": null, "picker": "fixture-picker" } ], "scopes": [],
          "ops": [], "reserved": [], "calls": [], "patterns": [] }
      ]
    },
    {
      "package": "fixture-pipe", "version": "1.0.0", "status": "answered",
      "exports": [
        { "specifier": "fixture-pipe", "export": "export =", "role": "socket", "side": "client",
          "makes": [ { "form": "new", "member": null, "picker": "fixture-picker" } ], "scopes": [],
          "ops": [], "reserved": [], "calls": [], "patterns": [] }
      ]
    }
  ]
}"#;

    /// The fixture's store answer and [`PUBLISHED_CLAIMS`].
    fn all_claims() -> Vec<ExportClaims> {
        let mut exports = claims();
        let added = crate::library_store::parse_answer(PUBLISHED_CLAIMS);
        assert_eq!(added.dropped, 0, "every added claim parses");
        exports.extend(added.exports);
        exports
    }

    /// The records discovery writes for `file` (repo-relative), each as
    /// `line callee -> package export member` or `line callee stop <reason>`.
    fn records_in(discovery: &Discovery, file: &str) -> Vec<String> {
        discovery
            .sites
            .iter()
            .filter(|site| site.file == file)
            .map(|site| match &site.end {
                Some(end) => format!(
                    "{} {} -> {} {} {}",
                    site.line,
                    site.callee,
                    end.package,
                    end.export,
                    end.member.as_deref().unwrap_or("-")
                ),
                None => format!(
                    "{} {} stop {}",
                    site.line,
                    site.callee,
                    site.stop.map_or("-", Stop::as_str)
                ),
            })
            .collect()
    }

    fn texts(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    /// carrick#2431: the store keys an export by the name the module
    /// publishes it as (`default`, a renamed `io`, `export =`), and the rows
    /// name the declaration (`http`, `lookup`, `Pipe`). A call through an
    /// import binding is matched on what the binding imports.
    #[test]
    fn a_call_through_a_binding_matches_the_published_export() {
        let edit = [
            (
                "node_modules/fixture-http/package.json",
                r#"{ "name": "fixture-http", "version": "1.0.0", "types": "index.d.ts" }"#,
            ),
            (
                "node_modules/fixture-http/index.d.ts",
                "export interface HttpStatic {\n  get(url: string): Promise<unknown>;\n}\ndeclare const http: HttpStatic;\nexport default http;\n",
            ),
            (
                "node_modules/fixture-io/package.json",
                r#"{ "name": "fixture-io", "version": "1.0.0", "types": "index.d.ts" }"#,
            ),
            (
                "node_modules/fixture-io/index.d.ts",
                "export interface Channel {\n  emit(name: string, data: unknown): void;\n}\ndeclare function lookup(url: string): Channel;\nexport { lookup as io };\n",
            ),
            (
                "node_modules/fixture-pipe/package.json",
                r#"{ "name": "fixture-pipe", "version": "1.0.0", "types": "index.d.ts" }"#,
            ),
            (
                "node_modules/fixture-pipe/index.d.ts",
                "declare class Pipe {\n  constructor(url: string);\n  write(data: unknown): void;\n}\nexport = Pipe;\n",
            ),
            (
                "src/published.ts",
                "import http from \"fixture-http\";\nimport { io } from \"fixture-io\";\nimport Pipe from \"fixture-pipe\";\n\nexport function readPublished(): Promise<unknown> {\n  return http.get(\"/published\");\n}\n\nexport function openChannel(): void {\n  io(\"wss://published\");\n}\n\nexport function openPipe(): Pipe {\n  return new Pipe(\"wss://pipe\");\n}\n",
            ),
            (
                "src/published-user.ts",
                "import { openPipe, readPublished } from \"./published\";\n\nexport function readBoth(): void {\n  readPublished();\n  openPipe();\n}\n",
            ),
        ];
        let Some(on) = walked("discovery", "services/web", "web", &edit, true) else {
            return;
        };
        let exports = all_claims();
        let discovery = discover(&on.inputs, &Labels::of(&exports), &on.repo);
        assert_eq!(
            records_in(&discovery, "services/web/src/published.ts"),
            texts(&[
                "6 http.get -> fixture-http default get",
                "10 io -> fixture-io io -",
                "14 Pipe -> fixture-pipe export = -",
            ])
        );
        assert_eq!(
            records_in(&discovery, "services/web/src/published-user.ts"),
            texts(&[
                "4 readPublished -> fixture-http default get",
                "5 openPipe -> fixture-pipe export = -",
            ])
        );
    }

    /// The store's answer for the export-shape cases: a package with a
    /// top-level `json` export, and an `export =` factory's default.
    const EXPORT_SHAPE_CLAIMS: &str = r#"{
  "library_claims": [
    {
      "package": "fixture-rest", "version": "1.0.0", "status": "answered",
      "exports": [
        { "specifier": "fixture-rest", "export": "json", "role": "server_framework", "side": "server",
          "makes": [], "scopes": [], "ops": [], "reserved": [], "calls": [], "patterns": [] }
      ]
    },
    {
      "package": "fixture-make", "version": "1.0.0", "status": "answered",
      "exports": [
        { "specifier": "fixture-make", "export": "default", "role": "server_framework", "side": "server",
          "makes": [ { "form": "call", "member": null, "picker": "fixture-picker" } ], "scopes": [],
          "ops": [], "reserved": [], "calls": [], "patterns": [] }
      ]
    }
  ]
}"#;

    /// carrick#2508: a declaration is matched by name only when its package
    /// publishes it under that name. An interface's `json` method is not the
    /// package's top-level `json` export, however the call is written; the
    /// top-level export is, imported directly or through a local barrel. An
    /// `export =` factory re-exported by a local barrel as a named binding,
    /// so that no import names the package, is the package's `default`.
    #[test]
    fn a_member_never_matches_an_export_by_name_and_a_barrel_default_is_the_default() {
        let edit = [
            (
                "node_modules/fixture-rest/package.json",
                r#"{ "name": "fixture-rest", "version": "1.0.0", "types": "index.d.ts" }"#,
            ),
            (
                "node_modules/fixture-rest/index.d.ts",
                "export interface Res {\n  json(body: unknown): void;\n}\nexport declare function json(): (req: unknown, res: Res) => void;\n",
            ),
            (
                "node_modules/fixture-make/package.json",
                r#"{ "name": "fixture-make", "version": "1.0.0", "types": "index.d.ts" }"#,
            ),
            (
                "node_modules/fixture-make/index.d.ts",
                "interface App {\n  post(path: string, handler: () => void): void;\n}\ndeclare function make(): App;\nexport = make;\n",
            ),
            (
                "src/rest-barrel.ts",
                "export { json as parse } from \"fixture-rest\";\nexport { default as makeApp } from \"fixture-make\";\n",
            ),
            (
                "src/rest.ts",
                "import { json, type Res } from \"fixture-rest\";\nimport { makeApp, parse } from \"./rest-barrel\";\n\nexport function reply(res: Res): void {\n  res.json({});\n}\n\nexport function parsers(): void {\n  json();\n  parse();\n}\n\nexport function boot(): void {\n  makeApp();\n}\n",
            ),
        ];
        let Some(on) = walked("discovery", "services/web", "web", &edit, true) else {
            return;
        };
        let mut exports = claims();
        let added = crate::library_store::parse_answer(EXPORT_SHAPE_CLAIMS);
        assert_eq!(added.dropped, 0, "every added claim parses");
        exports.extend(added.exports);
        let discovery = discover(&on.inputs, &Labels::of(&exports), &on.repo);
        assert_eq!(
            records_in(&discovery, "services/web/src/rest.ts"),
            texts(&[
                "9 json -> fixture-rest json -",
                "10 parse -> fixture-rest json -",
                "14 makeApp -> fixture-make default -",
            ])
        );
    }

    /// carrick#2508: the publication bits are flag bits. A row with neither
    /// reads as a declaration nothing says is published: no name to match it
    /// by.
    #[test]
    fn a_row_without_the_publication_bits_reads_and_names_nothing() {
        let text = concat!(
            r#"{"v":7,"typescript":"5.9.3","service":"api","files":1}"#,
            "\n",
            r#"{"f":"src/a.ts","h":"00","r":[["call",0,7,"greet","node_modules/p/index.d.ts",1,"p","string",0],["call",9,16,"make","node_modules/p/index.d.ts",3,"p","App",96]]}"#,
            "\n"
        );
        let rows = crate::position_rows::read_rows(text.as_bytes()).expect("an old row reads");
        let [old, new] = rows.files[0].r.as_slice() else {
            panic!("two rows");
        };
        assert!(published_names(old).is_empty());
        assert_eq!(published_names(new), vec!["default", "export =", "make"]);
    }

    /// carrick#2431: a global of the language's own library is keyed as the
    /// store keys the language's globals, wherever the compiler read its
    /// declaration: without the DOM library, the runtime's types declare
    /// `fetch` in a file of their own, and the call is the same site.
    #[test]
    fn a_global_the_runtime_declares_is_the_language_s() {
        let edit = [
            (
                "tsconfig.json",
                r#"{
  "compilerOptions": {
    "target": "es2022",
    "module": "esnext",
    "moduleResolution": "bundler",
    "strict": true,
    "skipLibCheck": true,
    "lib": ["es2022"],
    "types": ["node"]
  },
  "include": ["src/**/*"]
}
"#,
            ),
            (
                "node_modules/@types/node/index.d.ts",
                "/// <reference path=\"child_process.d.ts\" />\n/// <reference path=\"web-globals/fetch.d.ts\" />\n",
            ),
            (
                "node_modules/@types/node/web-globals/fetch.d.ts",
                "export {};\ndeclare global {\n  function fetch(input: string, init?: unknown): Promise<unknown>;\n}\n",
            ),
            (
                "src/node-fetch.ts",
                "export function readNodeGlobal(): Promise<unknown> {\n  return fetch(\"/node-global\");\n}\n",
            ),
        ];
        let Some(on) = walked("discovery", "services/web", "web", &edit, true) else {
            return;
        };
        let exports = claims();
        let discovery = discover(&on.inputs, &Labels::of(&exports), &on.repo);
        assert_eq!(
            records_in(&discovery, "services/web/src/node-fetch.ts"),
            texts(&["2 fetch -> lib:lib.dom.d.ts fetch -"])
        );
    }

    /// carrick#2431: an optional call is a call. A function declared inside
    /// another (`loadInner`) keys no body of its own, and the walk enters
    /// the function written there as a value (carrick#2448 T3c), where it
    /// stopped `no_body` before. A constant object's member is a function of
    /// the module, read like any other, so the walk follows it
    /// (carrick#3356).
    #[test]
    fn a_function_declared_inside_another_is_entered_and_an_optional_call_is_a_site() {
        let edit = [
            (
                "src/table.ts",
                "export function readTable(): Promise<Response> {\n  function loadInner(): Promise<Response> {\n    return fetch(\"/table\");\n  }\n  return loadInner();\n}\n\nexport const table = {\n  load(): Promise<Response> {\n    return fetch(\"/entry\");\n  },\n};\n\nexport function readEntry(): Promise<Response> {\n  return table.load();\n}\n",
            ),
            (
                "src/maybe.ts",
                "import { Socket } from \"fixture-socket\";\n\nexport function maybeSend(socket: Socket | undefined): void {\n  socket?.send(\"ping\", {});\n}\n",
            ),
        ];
        let Some(on) = walked("discovery", "services/web", "web", &edit, true) else {
            return;
        };
        let exports = claims();
        let discovery = discover(&on.inputs, &Labels::of(&exports), &on.repo);
        assert_eq!(
            records_in(&discovery, "services/web/src/table.ts"),
            texts(&[
                "3 fetch -> lib:lib.dom.d.ts fetch -",
                "5 loadInner -> lib:lib.dom.d.ts fetch -",
                "10 fetch -> lib:lib.dom.d.ts fetch -",
                "15 table.load -> lib:lib.dom.d.ts fetch -",
            ])
        );
        assert_eq!(
            records_in(&discovery, "services/web/src/maybe.ts"),
            texts(&["4 socket?.send -> fixture-socket Socket send"])
        );
    }

    /// A route registered through a helper in a sibling service is this
    /// service's own route: the call's unread site is `missed_inside` as
    /// before, and says it is an HTTP one, with the sibling's registration
    /// as its labelled end. A plain missed call says neither. The answer key
    /// is `tests/fixtures/sibling-route-helper/README.md`.
    #[test]
    fn a_route_registered_through_a_sibling_helper_is_saved_as_http() {
        let Some(on) = super::super::fixture_walk::walked_services(
            "sibling-route-helper",
            &[("services/api", "api"), ("packages/kit", "kit")],
            true,
        ) else {
            return;
        };
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sibling-route-helper");
        let answer = crate::library_store::parse_answer(
            &std::fs::read_to_string(fixture.join("library-claims.json")).unwrap(),
        );
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        let labels = Labels::of_answer(&answer);
        let [api, kit] = on.inputs.as_slice() else {
            panic!("two services");
        };
        let discovery = discover_with(api, &[kit], &labels, &on.repo);
        let rows = crate::unread_sites::from_scan(
            &discovery,
            &crate::request_summary::decided_sites::Decided::default(),
            &[],
            &[],
            &on.repo,
        );
        let row = rows
            .iter()
            .find(|row| row.file == "services/api/src/server.ts" && row.line == 4)
            .unwrap_or_else(|| panic!("no unread site at server.ts:4: {rows:#?}"));
        assert_eq!(row.reason, Stop::MissedInside, "{row:?}");
        assert_eq!(row.kind.as_deref(), Some("http"), "{row:?}");
        assert_eq!(row.role.as_deref(), Some("producer"), "{row:?}");
        let end = row.labelled_end.as_ref().expect("the sibling's end");
        assert_eq!(
            (
                end.package.as_str(),
                end.export.as_str(),
                end.member.as_deref()
            ),
            ("fixture-route-server", "route", None),
        );
        // Every other unread site of the service keeps no kind.
        assert!(
            rows.iter()
                .filter(|other| other.line != 4)
                .all(|other| other.kind.is_none()),
            "{rows:#?}"
        );
    }

    /// carrick#2618: a call into a function declared in a file of another
    /// service of the run is followed in that file, read with that service's
    /// own IR and rows. The record is the calling service's, at its own call,
    /// and the chain names the sibling's files. A file of no service stays
    /// `outside_service`, and the sibling's own files are never recorded.
    /// Without siblings the same calls are `outside_service`. The answer key
    /// is `tests/fixtures/discovery-siblings/README.md`.
    #[test]
    fn a_call_into_a_sibling_service_is_a_site_through_its_code() {
        let Some(on) = super::super::fixture_walk::walked_services(
            "discovery-siblings",
            &[("services/app", "app"), ("packages/client", "client")],
            true,
        ) else {
            return;
        };
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/discovery-siblings/library-claims.json"),
        )
        .unwrap();
        let answer = crate::library_store::parse_answer(&text);
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        let labels = Labels::of_answer(&answer);
        let [app, client] = on.inputs.as_slice() else {
            panic!("two services");
        };
        let file = "services/app/src/main.ts";
        let chain_of = |discovery: &Discovery, line: u32| -> Vec<String> {
            discovery
                .sites
                .iter()
                .filter(|site| site.file == file && site.line == line)
                .flat_map(|site| {
                    let end = site.end.as_ref().expect("a site reaches fetch");
                    let mut parts: Vec<String> = site
                        .chain
                        .iter()
                        .map(|hop| format!("{} {}", hop.function, hop.file))
                        .collect();
                    parts.push(format!("{} {} {}", end.package, end.export, end.file));
                    parts
                })
                .collect()
        };
        let with = discover_with(app, &[client], &labels, &on.repo);
        let http = "packages/client/src/http.ts";
        assert_eq!(
            chain_of(&with, 5),
            texts(&[
                &format!("getItem {http}"),
                &format!("send {http}"),
                &format!("lib:lib.dom.d.ts fetch {http}")
            ])
        );
        assert_eq!(
            chain_of(&with, 9),
            texts(&[
                &format!("postTo {http}"),
                &format!("send {http}"),
                &format!("lib:lib.dom.d.ts fetch {http}")
            ])
        );
        assert_eq!(
            chain_of(&with, 14),
            texts(&[
                &format!("ItemsClient.remove {http}"),
                &format!("send {http}"),
                &format!("lib:lib.dom.d.ts fetch {http}")
            ])
        );
        assert!(
            with.sites.iter().all(|site| site.line != 13),
            "no record at the construction"
        );
        assert!(
            with.sites.iter().all(|site| site.line != 18),
            "format() reaches no labelled call"
        );
        // A walk into a sibling that finds no site is not followed to the
        // end (its instances are not read): `format(n)` is missed inside, not
        // finished, so the fact layer keeps a candidate there. Without the
        // sibling it was unfollowed, which keeps the candidate too.
        let text = std::fs::read_to_string(on.repo.join(file)).unwrap();
        let at = |needle: &str| {
            // SWC numbers a file's bytes from 1.
            let lo = text.find(needle).unwrap() as u32 + 1;
            (file.to_string(), lo, lo + needle.len() as u32)
        };
        assert_eq!(with.missed_inside, vec![at("format(n)")]);
        // The construction reads a class with no constructor: nothing to miss.
        assert_eq!(with.finished, vec![at("new ItemsClient()")]);
        let unfollowed: Vec<(u32, Option<&str>)> = with
            .unfollowed
            .iter()
            .map(|site| (site.line, site.stop.map(Stop::as_str)))
            .collect();
        assert_eq!(unfollowed, vec![(22, Some("outside_service"))]);
        for record in with.sites.iter().chain(&with.unfollowed) {
            assert!(
                !record.file.starts_with("packages/"),
                "no record in a sibling's file: {}",
                record.file
            );
        }
        // The same walk with no siblings: every call into a package file is
        // outside the service.
        let alone = discover_with(app, &[], &labels, &on.repo);
        assert!(alone.sites.is_empty());
        let alone: Vec<u32> = alone.unfollowed.iter().map(|site| site.line).collect();
        assert_eq!(alone, vec![5, 9, 13, 14, 18, 22]);
        // The sibling's own walk records its own call.
        let own = discover_with(client, &[app], &labels, &on.repo);
        assert!(
            records_in(&own, http).contains(&"4 fetch -> lib:lib.dom.d.ts fetch -".to_string()),
            "{:?}",
            records_in(&own, http)
        );
    }

    /// carrick#4445: a walk into a sibling's function that passes a send on
    /// the value of a maker's call (`new Queue(q).add`, `createClient(b).get`,
    /// `new BroadcastChannel(n).postMessage`) it reads nothing at saves the
    /// call as `missed_inside`, beside any site it has. The receiver is linked
    /// to the maker: a construction beside `target.addEventListener` or
    /// `window.addEventListener` is not a lost send, a held local is not
    /// linked (carrick#4472), and `close()` is no op. The answer key is
    /// `tests/fixtures/sibling-chained-sends/README.md`.
    #[test]
    fn a_send_lost_in_a_sibling_s_function_is_saved_as_missed_inside() {
        let Some(on) = super::super::fixture_walk::walked_services(
            "sibling-chained-sends",
            &[("services/app", "app"), ("packages/kit", "kit")],
            true,
        ) else {
            return;
        };
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/sibling-chained-sends/library-claims.json"),
        )
        .unwrap();
        let answer = crate::library_store::parse_answer(&text);
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        let labels = Labels::of_answer(&answer);
        let [app, kit] = on.inputs.as_slice() else {
            panic!("two services");
        };
        let found = discover_with(app, &[kit], &labels, &on.repo);
        let mut missed: Vec<(String, u32)> = found
            .missed_at
            .iter()
            .filter(|((file, _, _), _)| file.starts_with("services/app/"))
            .map(|((file, _, _), (line, _))| (file.clone(), *line))
            .collect();
        missed.sort();
        missed.dedup();
        assert_eq!(
            missed,
            [2, 4, 7, 9]
                .map(|line| ("services/app/src/relay.ts".to_string(), line))
                .to_vec(),
            "{:?}",
            found.missed_inside
        );
        // Line 4 states one of its two sends and loses the other: it keeps
        // its site.
        assert!(
            found
                .sites
                .iter()
                .any(|site| site.file == "services/app/src/relay.ts" && site.line == 4),
            "{:?}",
            found.sites
        );
    }

    /// carrick#4256: the calls into a sibling whose walk wrote no rows stop
    /// `walk_lost`, where the same calls into a sibling that was never walked
    /// stop `rows_missing` (today's reading, which this pins). The reaching
    /// service's own rows are the walked ones in both.
    #[test]
    fn a_call_into_a_sibling_with_a_lost_walk_stops_walk_lost() {
        let stops_into_client = |index: fn(&Path) -> crate::position_rows::PositionIndex| {
            let on = super::super::fixture_walk::walked_services_over(
                "discovery-siblings",
                &[("services/app", "app"), ("packages/client", "client")],
                true,
                Some(("client", index)),
            )?;
            let text = std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/discovery-siblings/library-claims.json"),
            )
            .unwrap();
            let answer = crate::library_store::parse_answer(&text);
            let labels = Labels::of_answer(&answer);
            let [app, client] = on.inputs.as_slice() else {
                panic!("two services");
            };
            let with = discover_with(app, &[client], &labels, &on.repo);
            let mut stops: Vec<(u32, Option<&str>)> = with
                .sites
                .iter()
                .filter(|site| site.end.is_none())
                .chain(&with.unfollowed)
                .filter(|site| site.file == "services/app/src/main.ts")
                .filter(|site| [5, 9, 14, 18].contains(&site.line))
                .map(|site| (site.line, site.stop.map(Stop::as_str)))
                .collect();
            stops.sort();
            stops.dedup();
            Some(stops)
        };
        let Some(missing) = stops_into_client(crate::position_rows::PositionIndex::missing) else {
            return;
        };
        let lost = stops_into_client(crate::position_rows::PositionIndex::walk_lost)
            .expect("the sidecar is built");
        let lines: Vec<u32> = missing.iter().map(|(line, _)| *line).collect();
        assert_eq!(lines, vec![5, 9, 14, 18], "{missing:?}");
        assert!(
            missing
                .iter()
                .all(|(_, stop)| *stop == Some("rows_missing")),
            "{missing:?}"
        );
        assert_eq!(
            lost,
            missing
                .iter()
                .map(|(line, _)| (*line, Some("walk_lost")))
                .collect::<Vec<_>>()
        );
    }

    /// carrick#2618: a labelled call a walk reaches in a sibling's code is a
    /// site only when it sends. The same fixture with the store labelling
    /// the export a server framework (a route the sibling registers, which
    /// the receiver trace writes at the join point) leaves no site: the
    /// calls into the sibling stay unread (`missed_inside`), so the fact
    /// layer keeps their candidates.
    #[test]
    fn a_route_registered_in_a_sibling_is_no_site() {
        let Some(on) = super::super::fixture_walk::walked_services(
            "discovery-siblings",
            &[("services/app", "app"), ("packages/client", "client")],
            true,
        ) else {
            return;
        };
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/discovery-siblings/library-claims.json"),
        )
        .unwrap()
        .replace("\"http_client\"", "\"server_framework\"");
        let answer = crate::library_store::parse_answer(&text);
        assert_eq!(answer.dropped, 0);
        let labels = Labels::of_answer(&answer);
        let [app, client] = on.inputs.as_slice() else {
            panic!("two services");
        };
        let with = discover_with(app, &[client], &labels, &on.repo);
        assert!(with.sites.is_empty(), "{:?}", with.sites);
        assert_eq!(with.counts.get("sibling_routes"), Some(&3));
        let file = "services/app/src/main.ts";
        let source = std::fs::read_to_string(on.repo.join(file)).unwrap();
        let at = |needle: &str| {
            let lo = source.find(needle).unwrap() as u32 + 1;
            (file.to_string(), lo, lo + needle.len() as u32)
        };
        for needle in [
            "getItem(id)",
            "postTo(\"/api/events\", { at: 1 })",
            "items.remove(id)",
        ] {
            assert!(
                with.missed_inside.contains(&at(needle)),
                "{needle}: {:?}",
                with.missed_inside
            );
        }
    }

    /// Without rows there is nothing to walk: no site.
    #[test]
    fn without_rows_nothing_is_discovered() {
        let Some(off) = walked("discovery", "services/web", "web", &[], false) else {
            return;
        };
        let exports = claims();
        assert!(
            discover(&off.inputs, &Labels::of(&exports), &off.repo)
                .sites
                .is_empty()
        );
    }

    /// The receiver-trace fixture (carrick#2474 R1), read with rows, its
    /// store answer, and the member declarations the real verifier returns
    /// for the trace's ask.
    fn traced_fixture() -> Option<(
        super::super::fixture_walk::Walked,
        Vec<ExportClaims>,
        MemberDeclarations,
    )> {
        traced_fixture_named("receiver-trace")
    }

    /// The fixture `name`'s service `web` walked with rows, its store
    /// answer, and the declarations and scopes the trace's own verify
    /// request returns for it.
    fn traced_fixture_named(
        name: &str,
    ) -> Option<(
        super::super::fixture_walk::Walked,
        Vec<ExportClaims>,
        MemberDeclarations,
    )> {
        traced_fixture_in(name, ".", "web")
    }

    /// [`traced_fixture_named`] for the service `service` in `dir`.
    fn traced_fixture_in(
        name: &str,
        dir: &str,
        service: &str,
    ) -> Option<(
        super::super::fixture_walk::Walked,
        Vec<ExportClaims>,
        MemberDeclarations,
    )> {
        traced_fixture_edited(name, dir, service, |_| {})
    }

    /// [`traced_fixture_in`] with the store answer edited by `edit` before
    /// the trace asks it.
    fn traced_fixture_edited(
        name: &str,
        dir: &str,
        service: &str,
        edit: impl FnOnce(&mut Vec<ExportClaims>),
    ) -> Option<(
        super::super::fixture_walk::Walked,
        Vec<ExportClaims>,
        MemberDeclarations,
    )> {
        traced_fixture_files(name, dir, service, &[], edit)
    }

    /// [`traced_fixture_edited`] with `files` written into the service
    /// before it is walked.
    fn traced_fixture_files(
        name: &str,
        dir: &str,
        service: &str,
        files: &[(&str, &str)],
        edit: impl FnOnce(&mut Vec<ExportClaims>),
    ) -> Option<(
        super::super::fixture_walk::Walked,
        Vec<ExportClaims>,
        MemberDeclarations,
    )> {
        let on = walked(name, dir, service, files, true)?;
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("tests/fixtures/{name}/library-claims.json")),
        )
        .unwrap();
        let answer = crate::library_store::parse_answer(&text);
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        let mut exports = answer.exports;
        edit(&mut exports);
        let ask = declaration_ask(&on.inputs, &exports);
        let script = super::super::fixture_walk::sidecar()?;
        let sidecar = crate::services::TypeSidecar::spawn(&script).expect("spawn the sidecar");
        sidecar.start_init(&on.repo, None);
        sidecar
            .wait_ready(std::time::Duration::from_secs(120))
            .expect("the sidecar is ready");
        let verdicts = sidecar
            .verify_library_claims(&on.repo, &ask.checks)
            .expect("the verifier answers")
            .verdicts;
        let verdicts = with_scope_declarations(name, &ask, verdicts);
        let declarations = MemberDeclarations::of(&ask, &verdicts);
        Some((on, exports, declarations))
    }

    /// One verified scope verdict's declarations, as a cassette holds them.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct ScopeDeclarations {
        claim_id: String,
        receiver: String,
        declarations: Vec<crate::services::type_sidecar::SemanticsDeclaration>,
    }

    /// `verdicts` with each verified scope verdict's declarations
    /// (carrick#2636): the live verifier's where it names them, else the
    /// fixture's cassette `scope-declarations.json`, recorded from a
    /// verifier that does. Where both name them they must agree. With
    /// `CARRICK_RECORD_SCOPE_DECLARATIONS=1` the live ones are written to the
    /// cassette.
    fn with_scope_declarations(
        name: &str,
        ask: &DeclarationAsk,
        mut verdicts: Vec<SemanticsResult>,
    ) -> Vec<SemanticsResult> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("tests/fixtures/{name}/scope-declarations.json"));
        let live: Vec<ScopeDeclarations> = ask
            .scopes
            .iter()
            .zip(&verdicts)
            .filter(|(scope, verdict)| {
                scope.is_some()
                    && verdict.verdict == SemanticsVerdict::Verified
                    && !verdict.declarations.is_empty()
            })
            .map(|(_, verdict)| ScopeDeclarations {
                claim_id: verdict.claim_id.clone(),
                receiver: verdict.receiver.clone(),
                declarations: verdict.declarations.clone(),
            })
            .collect();
        if std::env::var("CARRICK_RECORD_SCOPE_DECLARATIONS").as_deref() == Ok("1") {
            assert!(!live.is_empty(), "the verifier names no scope declarations");
            std::fs::write(&path, serde_json::to_string_pretty(&live).unwrap() + "\n").unwrap();
            return verdicts;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            return verdicts;
        };
        let cassette: Vec<ScopeDeclarations> = serde_json::from_str(&text).unwrap();
        for known in &live {
            assert!(
                cassette.contains(known),
                "the cassette disagrees with the verifier: {known:?}"
            );
        }
        for (scope, verdict) in ask.scopes.iter().zip(&mut verdicts) {
            if scope.is_none()
                || verdict.verdict != SemanticsVerdict::Verified
                || !verdict.declarations.is_empty()
            {
                continue;
            }
            if let Some(held) = cassette
                .iter()
                .find(|held| held.claim_id == verdict.claim_id && held.receiver == verdict.receiver)
            {
                verdict.declarations = held.declarations.clone();
            }
        }
        verdicts
    }

    /// Each site as `file:line callee -> export member op reached receiver`.
    fn traced_records(discovery: &Discovery) -> Vec<String> {
        discovery
            .sites
            .iter()
            .filter_map(|site| {
                let end = site.end.as_ref()?;
                let op = end
                    .op
                    .map(|op| {
                        serde_json::to_value(op)
                            .unwrap()
                            .as_str()
                            .unwrap()
                            .to_string()
                    })
                    .unwrap_or_else(|| "-".to_string());
                let receiver = site.receiver.as_ref().map_or("-".to_string(), |at| {
                    format!(
                        "{}:{}@{}:{}",
                        serde_json::to_value(at.kind).unwrap().as_str().unwrap(),
                        at.symbol.as_deref().unwrap_or("-"),
                        at.decl_file.as_deref().unwrap_or("-"),
                        at.decl_line.unwrap_or_default()
                    )
                });
                let reached = site.reached.map_or("-".to_string(), |by| {
                    serde_json::to_value(by)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_string()
                });
                Some(format!(
                    "{}:{} {} -> {} {} {} {} {}",
                    site.file,
                    site.line,
                    site.callee,
                    end.export,
                    end.member.as_deref().unwrap_or("-"),
                    op,
                    reached,
                    receiver
                ))
            })
            .collect()
    }

    /// carrick#2474 R1: with the receiver trace on, a call whose member's
    /// declaration a verified op claim resolves to is a site with that op,
    /// whatever holds the receiver: a parameter (b), a callback's parameter
    /// (c), a field set from a constructor parameter (d), a call's value
    /// (e), and an HTTP client handed in as a parameter. The control (a)
    /// is found as before, by its instance. The request's own `get` (f), an
    /// in-repo class (g), an untyped receiver (h) and a local structural
    /// copy of the app's type (i) are no site. Each site names its
    /// receiver.
    #[test]
    fn a_call_on_a_labelled_member_s_declaration_is_a_site_whatever_holds_it() {
        let Some((on, exports, declarations)) = traced_fixture() else {
            return;
        };
        assert!(!declarations.is_empty(), "the verifier names declarations");
        let discovery = discover(
            &on.inputs,
            &Labels::of(&exports).tracing(&declarations),
            &on.repo,
        );
        let got = traced_records(&discovery);
        // A call into a function that reaches a labelled call is a site too
        // (`users(app)`), naming the labelled call's receiver. The factory's
        // result (e) is an instance the library-site reader already holds.
        let want = texts(&[
            "src/app.ts:12 make -> default - - name -",
            "src/app.ts:13 app.get -> default get receive instance binding:app@src/app.ts:12",
            "src/app.ts:16 users -> default get receive declaration parameter:app@src/users.ts:7",
            "src/app.ts:19 withApp -> default post receive declaration parameter:inner@src/app.ts:19",
            "src/app.ts:19 withApp -> default - - name -",
            "src/app.ts:19 inner.post -> default post receive declaration parameter:inner@src/app.ts:19",
            "src/app.ts:22 new Routes(app).mount -> default get receive declaration field:app@src/routes.ts:8",
            "src/app.ts:25 build -> default - - name -",
            "src/app.ts:25 build().get -> default get receive instance call_result:build@src/build.ts:3",
            "src/build.ts:4 make -> default - - name -",
            "src/client.ts:5 client.get -> default get request declaration parameter:client@src/client.ts:4",
            // carrick#2474 R2a's instance shapes.
            "src/helpers.ts:10 app.get -> default get receive declaration parameter:app@src/helpers.ts:9",
            "src/helpers.ts:15 app.get -> default get receive declaration parameter:app@src/helpers.ts:14",
            "src/helpers.ts:18 three -> default get receive declaration parameter:app@src/helpers.ts:14",
            "src/helpers.ts:21 two -> default get receive declaration parameter:app@src/helpers.ts:14",
            "src/helpers.ts:27 pong -> default get receive declaration parameter:app@src/helpers.ts:25",
            "src/helpers.ts:29 app.get -> default get receive declaration parameter:app@src/helpers.ts:25",
            "src/helpers.ts:32 ping -> default get receive declaration parameter:app@src/helpers.ts:25",
            "src/helpers.ts:37 app.get -> default get receive declaration parameter:app@src/helpers.ts:36",
            "src/helpers.ts:42 deps.app.get -> default get receive declaration field:app@src/helpers.ts:41",
            "src/helpers.ts:47 scoped.get -> default get receive declaration parameter:scoped@src/helpers.ts:46",
            "src/helpers.ts:51 make -> default - - name -",
            "src/helpers.ts:52 make -> default - - name -",
            "src/helpers.ts:53 health -> default get receive declaration parameter:app@src/helpers.ts:9",
            "src/helpers.ts:54 health -> default get receive declaration parameter:app@src/helpers.ts:9",
            "src/helpers.ts:55 health -> default get receive declaration parameter:app@src/helpers.ts:9",
            "src/helpers.ts:55 build -> default - - name -",
            "src/helpers.ts:56 health -> default get receive declaration parameter:app@src/helpers.ts:9",
            "src/helpers.ts:57 one -> default get receive declaration parameter:app@src/helpers.ts:14",
            "src/helpers.ts:58 ping -> default get receive declaration parameter:app@src/helpers.ts:25",
            "src/helpers.ts:59 keyed -> default get receive declaration field:app@src/helpers.ts:41",
            "src/helpers.ts:61 inner.get -> default get receive declaration parameter:inner@src/helpers.ts:61",
            "src/helpers.ts:62 health -> default get receive declaration parameter:app@src/helpers.ts:9",
            "src/helpers.ts:62 build -> default - - name -",
            "src/routes.ts:11 this.app.get -> default get receive declaration field:app@src/routes.ts:8",
            "src/users.ts:8 app.get -> default get receive declaration parameter:app@src/users.ts:7",
            "src/with-app.ts:4 make -> default - - name -",
        ]);
        assert_eq!(got, want, "{got:#?}");
        for line in [
            "negatives.ts:5",
            "negatives.ts:15",
            "negatives.ts:20",
            "negatives.ts:28",
        ] {
            assert!(
                !got.iter().any(|record| record.contains(line)),
                "{line} is no site"
            );
        }
        assert!(
            discovery
                .counts
                .keys()
                .any(|key| key.ends_with("untyped_receiver")),
            "the untyped receiver is a hole: {:?}",
            discovery.counts
        );
    }

    /// carrick#2474 R1: with the trace off, discovery on the fixture is what
    /// it was: the trace's own records add only sites and fields, and every
    /// site found without it is the same record, with no receiver. A walk
    /// that stopped with nothing found (a cycle through a helper whose
    /// receiver is a parameter) may find the labelled call with it on.
    #[test]
    fn with_the_trace_off_discovery_is_unchanged() {
        let Some((on, exports, declarations)) = traced_fixture() else {
            return;
        };
        let off = discover(&on.inputs, &Labels::of(&exports), &on.repo);
        let traced = discover(
            &on.inputs,
            &Labels::of(&exports).tracing(&declarations),
            &on.repo,
        );
        let text = serde_json::to_string(&off.sites).unwrap();
        assert!(!text.contains("\"receiver\"") && !text.contains("\"reached\""));
        let plain: Vec<DiscoverySite> = traced
            .sites
            .iter()
            .cloned()
            .map(|site| DiscoverySite {
                receiver: None,
                reached: None,
                ..site
            })
            .collect();
        for site in off.sites.iter().filter(|site| site.end.is_some()) {
            assert!(plain.contains(site), "{site:?} is kept with the trace on");
        }
        assert_eq!(
            off.sites
                .iter()
                .filter(|site| site.end.is_some())
                .map(|site| format!("{}:{}", site.file, site.line))
                .collect::<Vec<_>>(),
            texts(&[
                "src/app.ts:12",
                "src/app.ts:13",
                "src/app.ts:19",
                "src/app.ts:25",
                "src/app.ts:25",
                "src/build.ts:4",
                "src/helpers.ts:51",
                "src/helpers.ts:52",
                "src/helpers.ts:55",
                "src/helpers.ts:62",
                "src/with-app.ts:4"
            ]),
            "the trace's sites are not found without it"
        );
    }

    /// The decided sites and records of `discovery`.
    fn records_of(
        on: &super::super::fixture_walk::Walked,
        exports: &[ExportClaims],
        discovery: &Discovery,
    ) -> (
        super::super::decided_sites::Decided,
        Vec<super::super::site_record::SiteRecord>,
    ) {
        let discovered: Vec<super::super::decided_sites::DiscoverySite> = discovery
            .sites
            .iter()
            .map(|site| serde_json::from_value(serde_json::to_value(site).unwrap()).unwrap())
            .collect();
        let labels = super::super::decided_sites::SiteLabel::from_exports(exports);
        let decided =
            super::super::decided_sites::decided_sites(&on.inputs, &on.repo, &discovered, &labels);
        let records =
            super::super::site_record::site_records(&on.inputs, &[], &on.repo, &decided, &labels);
        (decided, records)
    }

    /// carrick#2474 R1: each decided site and record names the binding its
    /// labelled call is made on, wherever its name is decided; with the
    /// trace off none does. Every record found without the trace keeps its
    /// state byte for byte with it on, so no site's sha moves.
    #[test]
    fn decided_sites_and_records_name_their_receiver_and_keep_today_s_state() {
        let Some((on, exports, declarations)) = traced_fixture() else {
            return;
        };
        let off = discover(&on.inputs, &Labels::of(&exports), &on.repo);
        let traced = discover(
            &on.inputs,
            &Labels::of(&exports).tracing(&declarations),
            &on.repo,
        );
        let (decided_off, records_off) = records_of(&on, &exports, &off);
        let (decided_on, records_on) = records_of(&on, &exports, &traced);
        assert!(decided_off.sites.iter().all(|site| site.receiver.is_none()));
        assert!(records_off.iter().all(|record| record.receiver.is_none()));
        let kind_at = |file: &str, line: u32| {
            records_on
                .iter()
                .find(|record| record.anchors.call.file == file && record.anchors.call.line == line)
                .and_then(|record| record.receiver.as_ref())
                .map(|receiver| receiver.kind)
        };
        assert_eq!(kind_at("src/users.ts", 8), Some(ReceiverKind::Parameter));
        assert_eq!(kind_at("src/routes.ts", 11), Some(ReceiverKind::Field));
        assert_eq!(kind_at("src/app.ts", 19), Some(ReceiverKind::Parameter));
        assert_eq!(kind_at("src/app.ts", 25), Some(ReceiverKind::CallResult));
        assert_eq!(kind_at("src/app.ts", 13), Some(ReceiverKind::Binding));
        assert!(
            decided_on
                .sites
                .iter()
                .filter(|site| site.end.op.as_deref() == Some("receive"))
                .all(|site| site.receiver.is_some())
        );
        assert!(!records_off.is_empty());
        for record in &records_off {
            let same = records_on
                .iter()
                .find(|other| other.anchors == record.anchors)
                .expect("a record found without the trace is found with it");
            assert_eq!(
                serde_json::to_string(&same.state).unwrap(),
                serde_json::to_string(&record.state).unwrap()
            );
        }
    }

    /// Each site of `decided`, with the receiver trace's instance
    /// walk, as `file:line <- at root|hole …`, one line per instance end.
    fn instance_ends(decided: &super::super::decided_sites::Decided) -> Vec<String> {
        use super::super::decided_sites::InstanceAt;
        let mut out = Vec::new();
        for site in &decided.sites {
            for end in &site.instances {
                let what = match &end.end {
                    InstanceAt::Root { file, line, .. } => format!("root {file}:{line}"),
                    InstanceAt::Scope { file, line, .. } => format!("scope {file}:{line}"),
                    InstanceAt::Handed { file, line, .. } => format!("handed {file}:{line}"),
                    InstanceAt::Hole { reason, .. } => serde_json::to_value(reason)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_string(),
                };
                out.push(format!("{}:{} <- {} {what}", site.file, site.line, end.at));
            }
        }
        out.sort();
        out
    }

    /// carrick#2474 R2a: each decided site names the instance its labelled
    /// call is made on, one end per caller that hands one in. A receiver
    /// that is a parameter, or a key of one, is filled at each caller and
    /// walked up through helpers to the labelled maker whose result it is:
    /// held in a binding (a module's or a function's), built by an own
    /// factory in another module (called twice, two ends), or one helper
    /// handed two apps (two roots). A cycle ends, giving the root its other
    /// caller hands in.
    /// The holes: a function nothing calls (`entry_point`), a callback
    /// this code runs (`callback_parameter`), a function a library call
    /// runs, by name or written in place (`runs_by_library`), an app a
    /// package declares (`outside_service`). A field set from one
    /// constructor parameter is that parameter (carrick#1670 T3): the
    /// instance the `new` hands in.
    #[test]
    fn each_deciding_caller_gives_its_own_instance_end() {
        let Some((on, exports, declarations)) = traced_fixture() else {
            return;
        };
        let (discovery, decided) = decided_traced(&on, &exports, &declarations, &[]);
        let labels = super::super::decided_sites::SiteLabel::from_exports(&exports);
        let got = instance_ends(&decided);
        let want = texts(&[
            "src/app.ts:13 <- src/app.ts:13 root src/app.ts:12",
            "src/app.ts:19 <- src/app.ts:19 callback_parameter",
            "src/app.ts:25 <- src/app.ts:25 root src/build.ts:4",
            "src/client.ts:5 <- src/client.ts:5 entry_point",
            "src/helpers.ts:10 <- src/helpers.ts:53 root src/helpers.ts:51",
            "src/helpers.ts:10 <- src/helpers.ts:54 root src/helpers.ts:52",
            "src/helpers.ts:10 <- src/helpers.ts:55 root src/build.ts:4",
            "src/helpers.ts:10 <- src/helpers.ts:56 outside_service",
            "src/helpers.ts:10 <- src/helpers.ts:62 root src/build.ts:4",
            "src/helpers.ts:15 <- src/helpers.ts:57 root src/helpers.ts:51",
            "src/helpers.ts:29 <- src/helpers.ts:58 root src/helpers.ts:51",
            "src/helpers.ts:37 <- src/helpers.ts:37 entry_point",
            "src/helpers.ts:42 <- src/helpers.ts:59 root src/helpers.ts:51",
            "src/helpers.ts:47 <- src/helpers.ts:47 runs_by_library",
            "src/helpers.ts:61 <- src/helpers.ts:61 runs_by_library",
            "src/routes.ts:11 <- src/app.ts:22 root src/app.ts:12",
            "src/users.ts:8 <- src/app.ts:16 root src/app.ts:12",
        ]);
        assert_eq!(got, want, "{got:#?}");
        // Without the makers, no site names an instance; with them, every
        // record keeps its state byte for byte, so no site's sha moves.
        let (untraced, records) = records_of(&on, &exports, &discovery);
        assert!(untraced.sites.iter().all(|site| site.instances.is_empty()));
        let traced =
            super::super::site_record::site_records(&on.inputs, &[], &on.repo, &decided, &labels);
        assert_eq!(
            serde_json::to_string(
                &traced
                    .iter()
                    .map(|record| &record.state)
                    .collect::<Vec<_>>()
            )
            .unwrap(),
            serde_json::to_string(
                &records
                    .iter()
                    .map(|record| &record.state)
                    .collect::<Vec<_>>()
            )
            .unwrap()
        );
    }

    /// carrick#4070: an instance made by a maker imported through a Deno
    /// `npm:` specifier (`npm:fixture-server@1`) is that labelled maker's,
    /// as one imported bare is: the store keys the package by its name. The
    /// compiler resolves the specifier here through a `paths` entry, as
    /// Deno resolves it to the package it installed.
    #[test]
    fn an_instance_made_through_a_deno_npm_specifier_is_its_labelled_maker_s() {
        let files = [
            (
                "tsconfig.json",
                r#"{
  "compilerOptions": {
    "target": "es2022",
    "module": "commonjs",
    "moduleResolution": "node",
    "strict": true,
    "skipLibCheck": true,
    "types": [],
    "paths": { "npm:fixture-server@1": ["./node_modules/fixture-server"] }
  },
  "include": ["src/**/*"]
}
"#,
            ),
            (
                "src/deno-spelled.ts",
                "import make from \"npm:fixture-server@1\";\n\nexport const denoApp = make();\ndenoApp.get(\"/from-npm-spec\", () => undefined);\n",
            ),
        ];
        let Some((on, exports, declarations)) =
            traced_fixture_files("receiver-trace", ".", "web", &files, |_| {})
        else {
            return;
        };
        let (_, decided) = decided_traced(&on, &exports, &declarations, &[]);
        let got: Vec<String> = instance_ends(&decided)
            .into_iter()
            .filter(|end| end.starts_with("src/deno-spelled.ts"))
            .collect();
        assert_eq!(
            got,
            texts(&["src/deno-spelled.ts:4 <- src/deno-spelled.ts:4 root src/deno-spelled.ts:3"]),
            "{:#?}",
            instance_ends(&decided)
        );
    }

    /// Each route site of `decided` as `file:line <- path`, one line per
    /// composed path, with `(end)` after a path relative to a hole, and `?`
    /// for a path that is not text; with `edges`, each path's scope and
    /// mount edges from the top down (`… via file:line member prefix > …`).
    fn composed_lines(decided: &super::super::decided_sites::Decided, edges: bool) -> Vec<String> {
        let mut out = Vec::new();
        for site in decided
            .sites
            .iter()
            .filter(|site| site.end.op.as_deref() == Some("receive"))
        {
            for composed in &site.composed {
                let path = composed.path.as_deref().unwrap_or("?");
                let what = match composed.end.as_str() {
                    "top" => path.to_string(),
                    end => format!("{path} ({end})"),
                };
                let mut line = format!("{}:{} <- {what}", site.file, site.line);
                if edges {
                    line = format!("{line} via {}", composed.edges().join(" > "));
                }
                out.push(line);
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// The fixture service `on`'s sites, discovered with the receiver trace
    /// and decided with it, under the scope labels `scopes`.
    fn decided_traced(
        on: &super::super::fixture_walk::Walked,
        exports: &[ExportClaims],
        declarations: &MemberDeclarations,
        scopes: &[super::super::decided_sites::ScopeLabel],
    ) -> (Discovery, super::super::decided_sites::Decided) {
        use super::super::decided_sites::{MakerLabel, SiteLabel, decided_sites_traced};
        let discovery = discover(
            &on.inputs,
            &Labels::of(exports).tracing(declarations),
            &on.repo,
        );
        let discovered: Vec<super::super::decided_sites::DiscoverySite> = discovery
            .sites
            .iter()
            .map(|site| serde_json::from_value(serde_json::to_value(site).unwrap()).unwrap())
            .collect();
        let decided = decided_sites_traced(
            &on.inputs,
            &on.repo,
            &discovered,
            &SiteLabel::from_exports(exports),
            &MakerLabel::from_exports(exports),
            scopes,
            &super::super::decided_sites::ProcedureLabels::from_exports(
                exports,
                &declarations.scope_answers,
            ),
        );
        (discovery, decided)
    }

    /// carrick#2748: a function that calls itself with a longer value each
    /// time (`nest(`${path}/child`)`) hands the library a value no earlier
    /// walk of it read. The instance walk of what a library call is handed
    /// (carrick#2474 R3 S0) ends that cycle the way the site walk does,
    /// instead of walking the callers again for every longer value until
    /// the stack runs out.
    #[test]
    fn a_function_that_calls_itself_with_a_longer_value_ends_the_walk() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-recursive-arg")
        else {
            return;
        };
        use super::super::decided_sites::ScopeLabel;
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let got = composed_lines(&decided, false);
        let want = texts(&["src/index.ts:20 <- /tree", "src/index.ts:8 <- /health"]);
        assert_eq!(got, want, "{got:#?}");
    }

    /// carrick#3488: a client the library-site reader does not hold (a
    /// parameter, a constructor parameter property) is walked to the maker
    /// call each caller hands in. That root is where its base is read: a
    /// helper handed two instances ends at both; a class nothing constructs
    /// ends at no root.
    #[test]
    fn a_client_held_in_a_parameter_ends_at_the_maker_its_caller_hands_in() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-forms-client-base")
        else {
            return;
        };
        let (_, decided) = decided_traced(&on, &exports, &declarations, &[]);
        let got: Vec<String> = instance_ends(&decided)
            .into_iter()
            .filter(|line| {
                [
                    "m-param.ts",
                    "m-param-property.ts",
                    "m-param-property-di.ts",
                    "m-two-bases.ts",
                ]
                .iter()
                .any(|file| line.starts_with(&format!("src/{file}:")))
            })
            .collect();
        assert_eq!(
            got,
            texts(&[
                "src/m-param-property-di.ts:2 <- src/m-param-property-di.ts:2 unread",
                "src/m-param-property.ts:2 <- src/m-param-property.ts:3 root src/m-param-property.ts:3",
                "src/m-param.ts:2 <- src/m-param.ts:3 root src/m-param.ts:3",
                "src/m-two-bases.ts:2 <- src/m-two-bases.ts:3 root src/m-two-bases.ts:3",
                "src/m-two-bases.ts:2 <- src/m-two-bases.ts:4 root src/m-two-bases.ts:4",
            ]),
            "{got:#?}"
        );
    }

    /// A route written on a parameter a nested callback captures is read
    /// where the parameter is declared: composed under the scope the
    /// owner's host opens, as a route written in the owner itself.
    #[test]
    fn a_route_on_a_parameter_a_nested_callback_captures_is_composed_under_its_owners_scope() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-captured") else {
            return;
        };
        use super::super::decided_sites::ScopeLabel;
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let got = composed_lines(&decided, false);
        let want = texts(&[
            "src/index.ts:12 <- /cap/each",
            "src/index.ts:14 <- /cap/later",
            "src/index.ts:21 <- /cap/direct",
        ]);
        assert_eq!(got, want, "{got:#?}");
    }

    /// carrick#2474 R2b: with the receiver trace on, each route site names
    /// the full path it is served at, per instance end: the prefixes of the
    /// scopes a labelled member opens (a plugin it runs, handed by name,
    /// written in place, or inside another plugin) and of the mounts an
    /// instance is handed to (a router another module builds, one mounted
    /// twice, one an own factory returns, a mount made on a helper's
    /// parameter, and a scope a member returns), joined from the top down
    /// with the route's own path. A member run with no options binds no
    /// prefix. A function written under an option key is a hole
    /// (`callback_option`) until carrick#2432 lands. Only the scope claims
    /// the trace's own request verified count.
    #[test]
    fn each_route_is_composed_under_its_scopes_and_mounts() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-scopes") else {
            return;
        };
        use super::super::decided_sites::{ScopeLabel, SiteLabel};
        let labels = SiteLabel::from_exports(&exports);
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let mut verified: Vec<&str> = scopes.iter().map(|scope| scope.member.as_str()).collect();
        verified.sort();
        assert_eq!(verified, vec!["basePath", "register", "route"]);
        let (discovery, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let got = composed_lines(&decided, false);
        let want = texts(&[
            "src/admin.ts:8 <- /admin/stats",
            // A router exported under another name than its local, mounted
            // in another module: composed through that mount, not served at
            // the top (carrick#2474 R3).
            "src/aliased.ts:8 <- /api/v1/w",
            "src/extra.ts:9 <- /a/pong",
            "src/extra.ts:9 <- /b/pong",
            "src/index.ts:14 <- /bare",
            "src/index.ts:19 <- /health",
            "src/index.ts:26 <- /in/inline",
            "src/index.ts:37 <- /v2/x",
            "src/index.ts:38 <- /v3/y",
            // Handed to a member no scope claim names (`attach`): the trace
            // cannot say where it is served, so the path is relative
            // (carrick#2474 R3 S0).
            "src/index.ts:46 <- /s (mount_unverified)",
            "src/items.ts:12 <- /api/items",
            "src/items.ts:8 <- /api/deep/leaf",
            // Waits on carrick#2432's injected-callee step.
            "src/options.ts:16 <- /extra (callback_option)",
            "src/reports.ts:9 <- /r/daily",
            "src/shared.ts:8 <- /a/ping",
            "src/shared.ts:8 <- /b/ping",
            "src/users.ts:8 <- /v1/users",
            "src/users.ts:9 <- /v1/users/:id",
            // A prefix the helper takes as a parameter: each call that
            // fills it states a value (carrick#2635).
            "src/versioned.ts:10 <- /v10/ping (entry_point)",
            "src/versioned.ts:10 <- /v9/ping (entry_point)",
            // A default stands for the caller that omits the value only.
            "src/versioned.ts:22 <- /given/pong (entry_point)",
            "src/versioned.ts:22 <- /omit-default/pong (entry_point)",
            "src/versioned.ts:28 <- /only/pang (entry_point)",
        ]);
        assert_eq!(got, want, "{got:#?}");
        // No scope claim verified: no scope or mount edge, every plugin's
        // route a hole and every router the app is handed (`route`) served
        // somewhere the trace cannot say, not at the top (carrick#2474 R3
        // S0).
        let (_, unverified) = decided_traced(
            &on,
            &exports,
            &declarations,
            &ScopeLabel::from_exports(&exports, &HashMap::new()),
        );
        let bare = composed_lines(&unverified, false);
        for kept in [
            "src/users.ts:8 <- / (mount_unverified)",
            "src/aliased.ts:8 <- /w (mount_unverified)",
            "src/index.ts:46 <- /s (mount_unverified)",
            "src/index.ts:19 <- /health",
            "src/index.ts:14 <- /bare (runs_by_library)",
            "src/index.ts:37 <- /x (unread)",
        ] {
            assert!(bare.contains(&kept.to_string()), "{bare:#?}");
        }
        // Records keep their state byte for byte, so no site's sha moves.
        let (_, records) = records_of(&on, &exports, &discovery);
        let traced =
            super::super::site_record::site_records(&on.inputs, &[], &on.repo, &decided, &labels);
        assert_eq!(
            serde_json::to_string(
                &traced
                    .iter()
                    .map(|record| &record.state)
                    .collect::<Vec<_>>()
            )
            .unwrap(),
            serde_json::to_string(
                &records
                    .iter()
                    .map(|record| &record.state)
                    .collect::<Vec<_>>()
            )
            .unwrap()
        );
        // The routes served from the top are the ones R3 writes
        // (carrick#2474 R3 S1); the rest are counted by their end.
        let routes = crate::site_rows::traced(&decided, &traced);
        let mut served: Vec<String> = routes
            .routes
            .iter()
            .map(|route| {
                format!(
                    "{}:{} {} {} <- {}",
                    route.file,
                    route.line,
                    route.method,
                    route.path,
                    route.full_paths.join(" ")
                )
            })
            .collect();
        served.sort();
        assert_eq!(
            served,
            texts(&[
                "src/admin.ts:8 GET /stats <- /admin/stats",
                "src/aliased.ts:8 GET /w <- /api/v1/w",
                "src/extra.ts:9 POST /pong <- /a/pong /b/pong",
                "src/index.ts:14 GET /bare <- /bare",
                "src/index.ts:19 GET /health <- /health",
                "src/index.ts:26 GET /inline <- /in/inline",
                "src/index.ts:37 GET /x <- /v2/x",
                "src/index.ts:38 GET /y <- /v3/y",
                "src/items.ts:12 GET /items <- /api/items",
                "src/items.ts:8 GET /leaf <- /api/deep/leaf",
                "src/reports.ts:9 GET /daily <- /r/daily",
                "src/shared.ts:8 GET /ping <- /a/ping /b/ping",
                "src/users.ts:8 GET / <- /v1/users",
                "src/users.ts:9 POST /:id <- /v1/users/:id",
            ]),
            "{served:#?}"
        );
        assert_eq!(
            routes.untraced,
            std::collections::BTreeMap::from([
                ("hole_callback_option".to_string(), 1),
                ("hole_entry_point".to_string(), 3),
                ("mount_unverified".to_string(), 1),
            ])
        );
    }

    /// K1 (carrick#2434): `receiver-mount-pathless`'s sites, decided under
    /// the scope labels the live verifier verified there, with those labels.
    fn pathless_decided() -> Option<(
        super::super::decided_sites::Decided,
        Vec<super::super::decided_sites::ScopeLabel>,
    )> {
        use super::super::decided_sites::ScopeLabel;
        let (on, exports, declarations) = traced_fixture_named("receiver-mount-pathless")?;
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        Some((decided, scopes))
    }

    /// The composed lines of `decided` written in `file`.
    fn lines_in(
        decided: &super::super::decided_sites::Decided,
        file: &str,
        edges: bool,
    ) -> Vec<String> {
        composed_lines(decided, edges)
            .into_iter()
            .filter(|line| line.starts_with(&format!("src/{file}:")))
            .collect()
    }

    /// K1: `items.routes()`, a member a verified pass-through label names,
    /// is `items` itself for the walk, so `api.use("/v1", items.routes())`
    /// serves `items` under `/v1`. Without the label the call's value is a
    /// hole and `items` is served at the top, its prefix lost.
    #[test]
    fn a_pass_through_member_mounted_under_a_path_serves_its_receiver_there() {
        let Some((decided, _)) = pathless_decided() else {
            return;
        };
        assert_eq!(
            lines_in(&decided, "items.ts", false),
            texts(&[
                "src/items.ts:10 <- /v1/items.remove",
                "src/items.ts:8 <- /v1/items.list",
                "src/items.ts:9 <- /v1/items.add",
            ])
        );
    }

    /// K1: a router handed to `use` with no path, under a verified
    /// name-less `mounts`, is served at its parent's own address.
    #[test]
    fn a_router_mounted_with_no_path_is_served_at_its_parent_address() {
        let Some((decided, _)) = pathless_decided() else {
            return;
        };
        assert_eq!(
            lines_in(&decided, "plain.ts", false),
            texts(&["src/plain.ts:13 <- /plain/a"])
        );
    }

    /// K1 (carrick#3955): an app and a router made on one line are two
    /// instances. The app using the router's `routes()` mounts the router,
    /// not itself, so the routes under it compose, not end `recursion`.
    #[test]
    fn two_instances_made_on_one_line_are_two_roots() {
        let Some((decided, _)) = pathless_decided() else {
            return;
        };
        assert_eq!(
            lines_in(&decided, "oneline.ts", true),
            texts(&[
                "src/oneline.ts:10 <- /v1/s.remove via src/oneline.ts:16 use - > src/oneline.ts:15 use /v1",
                "src/oneline.ts:11 <- /v1/s.create via src/oneline.ts:16 use - > src/oneline.ts:15 use /v1",
                "src/oneline.ts:9 <- /v1/s.list via src/oneline.ts:16 use - > src/oneline.ts:15 use /v1",
            ])
        );
    }

    /// K1: an app whose `use` takes middleware only, handed a router's
    /// `routes()`, serves the router at the app's own address: one edge
    /// with no prefix, so the router is no longer the top itself.
    #[test]
    fn an_app_using_a_routers_middleware_serves_the_router_at_its_own_address() {
        let Some((decided, _)) = pathless_decided() else {
            return;
        };
        assert_eq!(
            lines_in(&decided, "api.ts", true),
            texts(&["src/api.ts:10 <- /ping via src/api.ts:16 use -"])
        );
    }

    /// K1: a path-first and a path-less `use` on one member are two labels;
    /// each reads its own call, and neither takes the other's.
    #[test]
    fn a_path_first_and_a_path_less_use_on_one_member_each_read_their_own_call() {
        let Some((decided, _)) = pathless_decided() else {
            return;
        };
        assert_eq!(
            lines_in(&decided, "both.ts", true),
            texts(&[
                "src/both.ts:11 <- /two via src/both.ts:14 use -",
                "src/both.ts:9 <- /x/one via src/both.ts:13 use /x",
            ])
        );
        // The same on routers of an export no scope claim names, where the
        // `use` declaration the call resolves to says which claims apply.
        assert_eq!(
            lines_in(&decided, "declared.ts", true),
            texts(&[
                "src/declared.ts:11 <- /y/one via src/declared.ts:14 use /y",
                "src/declared.ts:13 <- /two via src/declared.ts:15 use -",
            ])
        );
    }

    /// K1: where a `mounts` claim and its handle twin both verify on a
    /// call, the call makes one edge per slot, not one per claim: each
    /// route under it composes once.
    #[test]
    fn a_slot_both_twins_verify_on_makes_one_edge() {
        let Some((decided, scopes)) = pathless_decided() else {
            return;
        };
        use super::super::decided_sites::ScopeBinds;
        let twins: Vec<bool> = scopes
            .iter()
            .filter(|label| label.package == "fixture-hrouter" && label.name == Some((0, None)))
            .filter_map(|label| match label.binds {
                ScopeBinds::Mounts { arg: 1, handle, .. } => Some(handle),
                _ => None,
            })
            .collect();
        assert_eq!(twins, vec![false, true], "both twins verified: {scopes:#?}");
        // Seven twin-verified mounts deep: one edge each would compose one
        // chain; one per claim would compose 2^7 and spend the chain budget.
        for (file, line) in [
            ("src/plain.ts", 13),
            ("src/both.ts", 9),
            ("src/nest.ts", 24),
        ] {
            let site = decided
                .sites
                .iter()
                .find(|site| site.file == file && site.line == line)
                .expect("the route is decided");
            let chains: Vec<Vec<String>> = site
                .composed
                .iter()
                .map(|composed| composed.edges())
                .collect();
            assert_eq!(chains.len(), 1, "{file}:{line}: {chains:#?}");
            assert!(!site.composed_trimmed, "{file}:{line}: trimmed");
        }
        assert_eq!(
            lines_in(&decided, "nest.ts", false),
            texts(&["src/nest.ts:24 <- /a/b/c/d/e/f/g/z"])
        );
    }

    /// K1 ruling: a name-less mount label whose slot sits past a value that
    /// could be a path (`router.use("/v1", sub.routes())`, where the only
    /// verified `use` label is a name-less handle mount at argument 1)
    /// never serves `sub` at the parent's address: with no named scope to
    /// read `/v1`, `sub` is a mount the trace cannot place.
    #[test]
    fn a_name_less_mount_past_a_path_never_serves_at_the_parent_address() {
        use super::super::decided_sites::{ScopeBinds, ScopeLabel};
        use crate::services::type_sidecar::LibraryClaim;
        let Some((on, exports, declarations)) =
            traced_fixture_edited("receiver-mount-pathless", ".", "web", |exports| {
                for export in exports
                    .iter_mut()
                    .filter(|export| export.package == "fixture-mw-router")
                {
                    for claim in &mut export.claims {
                        if let LibraryClaim::Scope { member, name, .. } = claim
                            && member == "use"
                        {
                            *name = None;
                        }
                    }
                }
            })
        else {
            return;
        };
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        assert!(
            scopes.iter().any(|label| {
                label.package == "fixture-mw-router"
                    && label.name.is_none()
                    && label.binds
                        == ScopeBinds::Mounts {
                            arg: 1,
                            key: None,
                            handle: true,
                        }
            }),
            "the name-less handle mount past the path verified: {scopes:#?}"
        );
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        assert_eq!(
            lines_in(&decided, "items.ts", false),
            texts(&[
                "src/items.ts:10 <- /items.remove (mount_unverified)",
                "src/items.ts:8 <- /items.list (mount_unverified)",
                "src/items.ts:9 <- /items.add (mount_unverified)",
            ])
        );
        // A router of another package handed there is no more the top.
        assert_eq!(
            lines_in(&decided, "cross.ts", false),
            texts(&["src/cross.ts:11 <- /c (mount_unverified)"])
        );
    }

    /// K1: plain middleware handed to a handle mount is no instance: no
    /// edge, no lead or stop at the call, and the app's own routes stay at
    /// the top, not `mount_unverified`.
    #[test]
    fn an_unlabelled_argument_under_a_handle_mount_saves_no_site() {
        let Some((decided, _)) = pathless_decided() else {
            return;
        };
        assert_eq!(
            lines_in(&decided, "middleware.ts", true),
            texts(&["src/middleware.ts:15 <- /m/ok via "])
        );
        let at_use =
            |file: &str, line: u32| file == "src/middleware.ts" && (line == 13 || line == 14);
        assert!(
            !decided
                .leads
                .iter()
                .any(|lead| at_use(&lead.file, lead.line)),
            "{:#?}",
            decided.leads
        );
        assert!(
            !decided
                .stops
                .iter()
                .any(|stop| at_use(&stop.file, stop.line)),
            "{:#?}",
            decided.stops
        );
    }

    /// carrick#2386: a route's own path written through a conditional or a
    /// default (`base ? \`${base}api\` : "/api"`, `base ?? "/"`) is served at
    /// each way the code writes whole, read the way a mount's prefix is. A
    /// way led by a value read at runtime states nothing, and a path with no
    /// way written whole stays unread. Two reads state the same routes.
    #[test]
    fn a_route_path_written_through_a_conditional_is_served_at_each_whole_way() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-route-prefix")
        else {
            return;
        };
        use super::super::decided_sites::{ScopeLabel, SiteLabel};
        let labels = SiteLabel::from_exports(&exports);
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let served = |decided: &super::super::decided_sites::Decided| {
            let records = super::super::site_record::site_records(
                &on.inputs,
                &[],
                &on.repo,
                decided,
                &labels,
            );
            let routes = crate::site_rows::traced(decided, &records);
            let mut served: Vec<String> = routes
                .routes
                .iter()
                .map(|route| {
                    format!(
                        "{}:{} {} {} <- {}",
                        route.file,
                        route.line,
                        route.method,
                        route.path,
                        route.full_paths.join(" ")
                    )
                })
                .collect();
            served.sort();
            let guessed: Vec<String> = routes
                .guessed
                .iter()
                .map(|site| format!("{}:{} {}", site.file, site.line, site.method))
                .collect();
            (served, routes.untraced, guessed)
        };
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let (got, untraced, guessed) = served(&decided);
        assert_eq!(
            got,
            texts(&[
                "src/index.ts:15 GET /api/session <- /api/session",
                "src/index.ts:16 POST /api/session/setup <- /api/session/setup",
                "src/index.ts:18 GET / <- /",
                "src/index.ts:20 GET /health <- /health",
                // The base normalised through conditionals on one name.
                "src/normalised.ts:16 GET /v2/status <- /v2/status",
            ]),
            "{got:#?}"
        );
        // The base with no way written whole (`${base}health`) states no
        // path: a guessed site (ruling on PR #3182), not an unread prefix.
        assert!(untraced.is_empty(), "{untraced:#?}");
        assert_eq!(guessed, texts(&["src/index.ts:22 GET"]));
        let (_, again) = decided_traced(&on, &exports, &declarations, &scopes);
        assert_eq!(served(&again), (got, untraced, guessed));
    }

    /// carrick#3191: a route whose path is a binding with a literal default
    /// is served at the path the default gives, whichever way the default is
    /// written (a local's `||` or `??`, a destructuring default, a default on
    /// a key of a destructured parameter; in a function or a module
    /// constant), and so is a
    /// template or `+` built on one. A default read at runtime, and a local
    /// assigned again, state no path: each stays a guessed site.
    #[test]
    fn a_route_path_held_in_a_binding_with_a_literal_default_is_served_there() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-route-default")
        else {
            return;
        };
        use super::super::decided_sites::{ScopeLabel, SiteLabel};
        let labels = SiteLabel::from_exports(&exports);
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let records =
            super::super::site_record::site_records(&on.inputs, &[], &on.repo, &decided, &labels);
        let routes = crate::site_rows::traced(&decided, &records);
        let mut served: Vec<String> = routes
            .routes
            .iter()
            .map(|route| {
                format!(
                    "{}:{} {} {}",
                    route.file,
                    route.line,
                    route.method,
                    route.full_paths.join(" ")
                )
            })
            .collect();
        served.sort();
        assert_eq!(
            served,
            texts(&[
                "src/index.ts:15 GET /widgets",
                "src/index.ts:16 GET /widgets/:widgetId",
                "src/index.ts:23 GET /gadgets",
                "src/index.ts:24 GET /gadgets/:gadgetId",
                "src/index.ts:31 GET /parts",
                "src/index.ts:32 GET /parts/:partId",
                // A whole parameter's default (`toolsAt = "/tools"`, lines
                // 38-39) is not read yet: with no caller the walk up to the
                // callers that fill it leaves a lead, not a site
                // (carrick#3616).
                "src/index.ts:45 GET /bolts",
                "src/index.ts:46 GET /bolts/:boltId",
                "src/index.ts:53 GET /nuts",
                "src/module.ts:13 GET /sprockets",
                "src/module.ts:14 GET /springs/:springId",
            ]),
            "{served:#?}"
        );
        let mut guessed: Vec<String> = routes
            .guessed
            .iter()
            .map(|site| format!("{}:{} {}", site.file, site.line, site.method))
            .collect();
        guessed.sort();
        assert_eq!(
            guessed,
            texts(&["src/index.ts:60 GET", "src/index.ts:64 GET"]),
            "{guessed:#?}"
        );
    }

    /// Ruling on PR #3182 (2026-11-02): a route whose own path the code
    /// writes in no whole way (a base joined with text, a base rewritten by
    /// a call, a local holding a value read at runtime, a loop over values
    /// the compiler types as `string`) is a guessed site: no route is
    /// served there, and the reader's row of its method is withdrawn. A
    /// literal route beside them is served as before.
    #[test]
    fn a_route_path_written_in_no_whole_way_is_a_guessed_site() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-route-guessed")
        else {
            return;
        };
        use super::super::decided_sites::{ScopeLabel, SiteLabel};
        let labels = SiteLabel::from_exports(&exports);
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let records =
            super::super::site_record::site_records(&on.inputs, &[], &on.repo, &decided, &labels);
        let routes = crate::site_rows::traced(&decided, &records);
        let served: Vec<String> = routes
            .routes
            .iter()
            .map(|route| {
                format!(
                    "{}:{} {} {}",
                    route.file,
                    route.line,
                    route.method,
                    route.full_paths.join(" ")
                )
            })
            .collect();
        assert_eq!(
            served,
            texts(&["src/index.ts:19 GET /", "src/index.ts:21 GET /ready"]),
            "{served:#?}"
        );
        let mut guessed: Vec<String> = routes
            .guessed
            .iter()
            .map(|site| format!("{}:{} {}", site.file, site.line, site.method))
            .collect();
        guessed.sort();
        assert_eq!(
            guessed,
            texts(&[
                "src/index.ts:14 GET",
                "src/index.ts:16 POST",
                "src/table.ts:17 GET",
            ]),
            "{guessed:#?}"
        );
    }

    /// carrick#3237: a route path that writes its first segment whole before
    /// a value the code does not read (`/docs/spec${tail}`) is not a guessed
    /// site: no route is served there
    /// yet, it is counted as `unread_own_part`, and the reader's row stands.
    /// A path that starts with its value (`${head}health`) stays guessed.
    #[test]
    fn a_route_path_that_writes_its_first_segment_is_not_a_guessed_site() {
        let Some((on, exports, declarations)) =
            traced_fixture_in("receiver-route-stated-head", "packages/routes", "routes")
        else {
            return;
        };
        use super::super::decided_sites::{ScopeLabel, SiteLabel};
        let labels = SiteLabel::from_exports(&exports);
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let records =
            super::super::site_record::site_records(&on.inputs, &[], &on.repo, &decided, &labels);
        let routes = crate::site_rows::traced(&decided, &records);
        let mut guessed: Vec<String> = routes
            .guessed
            .iter()
            .map(|site| format!("{}:{} {}", site.file, site.line, site.method))
            .collect();
        guessed.sort();
        assert_eq!(
            guessed,
            texts(&["packages/routes/src/families.ts:21 GET"]),
            "{:#?}",
            routes.guessed
        );
        // `/docs/spec${tail}` alone: the loop over the stated `KINDS` at
        // `:14` is read, one way per element (carrick#2093), and its chains
        // end at the plugin's parameter like the package's other routes.
        assert_eq!(
            routes.untraced.get("unread_own_part"),
            Some(&1),
            "{:#?}",
            routes.untraced
        );
    }

    /// carrick#3213: a route path written as a template over a constant
    /// another package exports, where that constant is itself a template
    /// over exported constants (`${LOGIN_PATH}/:id` with `LOGIN_PATH =
    /// `${AUTH_BASE}/login``), folds across the packages, so the route is
    /// served at its whole path and is no guessed site. A constant whose
    /// template holds a value read at runtime does not fold: that site stays
    /// guessed.
    #[test]
    fn a_route_path_over_a_nested_template_constant_from_another_package_is_served() {
        let Some((on, exports, declarations)) = traced_fixture_in(
            "receiver-route-nested-template",
            "packages/server",
            "server",
        ) else {
            return;
        };
        use super::super::decided_sites::{ScopeLabel, SiteLabel};
        let labels = SiteLabel::from_exports(&exports);
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let records =
            super::super::site_record::site_records(&on.inputs, &[], &on.repo, &decided, &labels);
        let routes = crate::site_rows::traced(&decided, &records);
        let mut served: Vec<String> = routes
            .routes
            .iter()
            .map(|route| {
                format!(
                    "{}:{} {} {}",
                    route.file,
                    route.line,
                    route.method,
                    route.full_paths.join(" ")
                )
            })
            .collect();
        served.sort();
        assert_eq!(
            served,
            texts(&[
                "packages/server/src/main.ts:10 POST /account/profile/:section",
                "packages/server/src/main.ts:12 GET /auth/reset",
                "packages/server/src/main.ts:13 GET /auth/reset/:token",
                "packages/server/src/main.ts:8 GET /auth/login",
                "packages/server/src/main.ts:9 GET /auth/login/:id",
            ]),
            "{served:#?}"
        );
        let guessed: Vec<String> = routes
            .guessed
            .iter()
            .map(|site| format!("{}:{} {}", site.file, site.line, site.method))
            .collect();
        assert_eq!(
            guessed,
            texts(&["packages/server/src/main.ts:11 GET"]),
            "{:#?}",
            routes.guessed
        );
        // Each record states the name the trace serves, whole: forced, not
        // a lead. The one that does not fold is a lead.
        let mut names: Vec<String> = records
            .iter()
            .map(|record| {
                format!(
                    "{} {:?} {}",
                    record.anchors.call.line,
                    record.bucket,
                    record
                        .state
                        .forced
                        .get(&crate::site_questions::Slot::Name)
                        .map_or("-", String::as_str)
                )
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            texts(&[
                "10 Forced /account/profile/:section",
                "11 Lead -",
                "12 Forced /auth/reset",
                "13 Forced /auth/reset/:token",
                "8 Forced /auth/login",
                "9 Forced /auth/login/:id",
            ])
        );
    }

    /// carrick#3479: a name stated through a constant the compiler types
    /// wider than its value (a `let` never reassigned, an enum or const enum
    /// member, an entry under a computed key, a built-in over such parts) is
    /// forced whole on every protocol: HTTP calls and routes, pub/sub topics
    /// and socket events. A name with a part that does not fold is forced
    /// nowhere.
    #[test]
    fn a_name_stated_through_constants_is_forced_on_every_protocol() {
        let Some((on, exports, declarations)) =
            traced_fixture_in("constant-names", ".", "constant-names")
        else {
            return;
        };
        use super::super::decided_sites::{ScopeLabel, SiteLabel};
        let labels = SiteLabel::from_exports(&exports);
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let records =
            super::super::site_record::site_records(&on.inputs, &[], &on.repo, &decided, &labels);
        let mut forced: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for record in &records {
            let file = record
                .state
                .point
                .at
                .rsplit_once(':')
                .map_or(record.state.point.at.as_str(), |(file, _)| file);
            let entry = forced.entry(file.to_string()).or_default();
            if let Some(name) = record.state.forced.get(&crate::site_questions::Slot::Name) {
                entry.insert(name.clone());
            }
        }
        let key: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/constant-names/rows.expected.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let mut wrong = Vec::new();
        for (file, want) in key["names"].as_object().unwrap() {
            let want = BTreeSet::from([want.as_str().unwrap().to_string()]);
            let got = forced.get(file);
            if got != Some(&want) {
                wrong.push(format!("{file}: want {want:?}, got {got:?}"));
            }
        }
        for file in key["unnamed"].as_array().unwrap() {
            let file = file.as_str().unwrap();
            match forced.get(file) {
                Some(got) if got.is_empty() => {}
                got => wrong.push(format!("{file}: want a site forcing no name, got {got:?}")),
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// carrick#3527, as it reads today (the guard S2 widens reach past): a
    /// queue name handed to a function by its caller, or written as an enum
    /// member at the library call, is forced; a name read from the opaque
    /// `this.topic` of a base class is a site with no forced name; and no
    /// record forces a job name (`job`, `direct-job`) as the queue's name.
    #[test]
    fn a_topic_name_read_today_stays_read() {
        let Some((on, exports, declarations)) =
            traced_fixture_in("queue-enum-base-class-one", ".", "jobs")
        else {
            panic!("the sidecar is not built: cd src/sidecar && npm ci && npm run build");
        };
        use super::super::decided_sites::{ScopeLabel, SiteLabel};
        let labels = SiteLabel::from_exports(&exports);
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let records =
            super::super::site_record::site_records(&on.inputs, &[], &on.repo, &decided, &labels);
        // Each record's point, with the name it forces (None: forced nowhere).
        let mut forced: BTreeMap<String, Vec<Option<String>>> = BTreeMap::new();
        for record in &records {
            forced
                .entry(record.state.point.at.clone())
                .or_default()
                .push(
                    record
                        .state
                        .forced
                        .get(&crate::site_questions::Slot::Name)
                        .cloned(),
                );
        }
        let at = |point: &str| forced.get(point).cloned();
        assert_eq!(
            at("src/c3-param.ts:9"),
            Some(vec![Some("alpha-jobs".to_string())]),
            "{forced:#?}"
        );
        assert_eq!(
            at("src/c1-direct.ts:10"),
            Some(vec![Some("beta-jobs".to_string())]),
            "{forced:#?}"
        );
        assert_eq!(
            at("src/consumer-base.ts:9"),
            Some(vec![None]),
            "{forced:#?}"
        );
        for names in forced.values() {
            for name in names.iter().flatten() {
                assert!(
                    name != "job" && name != "direct-job",
                    "a job name is forced as a queue name: {forced:#?}"
                );
            }
        }
    }

    /// carrick#3527 S2: a constructor body is read as a function. A queue
    /// opened by a literal inside a constructor (`new Worker('ctor-literal-jobs')`)
    /// is a site forcing that name, reached from `new CtorWorker()`; before,
    /// nothing in a constructor body was a call of any function.
    #[test]
    fn a_queue_opened_in_a_constructor_chain_is_a_site() {
        let Some((on, exports, declarations)) =
            traced_fixture_in("queue-enum-base-class-one", ".", "jobs")
        else {
            panic!("the sidecar is not built: cd src/sidecar && npm ci && npm run build");
        };
        use super::super::decided_sites::{ScopeLabel, SiteLabel};
        let labels = SiteLabel::from_exports(&exports);
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let records =
            super::super::site_record::site_records(&on.inputs, &[], &on.repo, &decided, &labels);
        let mut forced: BTreeMap<String, Vec<Option<String>>> = BTreeMap::new();
        for record in &records {
            forced
                .entry(record.state.point.at.clone())
                .or_default()
                .push(
                    record
                        .state
                        .forced
                        .get(&crate::site_questions::Slot::Name)
                        .cloned(),
                );
        }
        assert_eq!(
            forced.get("src/c2-ctor-literal.ts:5").cloned(),
            Some(vec![Some("ctor-literal-jobs".to_string())]),
            "{forced:#?}"
        );
    }

    /// carrick#3527 S4: an op whose name the store binds to its maker
    /// (`new Queue(name).add(job, data)`, `name: { bound: maker }`) reads the
    /// name at the maker call its instance was made by, composed up that
    /// maker's callers. Where the maker runs inside a constructor chain
    /// (`super(Topic.Alpha)`), each send is joined to the construction its
    /// receiver is (`new AlphaProducer().add(...)`), so one wrapper serving
    /// two queues names each send by its own. The job name (`'job'`,
    /// `'direct-job'`) is never the queue's name.
    #[test]
    fn a_maker_bound_queue_name_is_read_at_its_construction() {
        let Some((on, exports, declarations)) =
            traced_fixture_in("queue-enum-base-class-one", ".", "jobs")
        else {
            panic!("the sidecar is not built: cd src/sidecar && npm ci && npm run build");
        };
        use super::super::decided_sites::{ScopeLabel, SiteLabel};
        let labels = SiteLabel::from_exports(&exports);
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let records =
            super::super::site_record::site_records(&on.inputs, &[], &on.repo, &decided, &labels);
        let mut forced: BTreeMap<String, Vec<Option<String>>> = BTreeMap::new();
        for record in &records {
            forced
                .entry(record.state.point.at.clone())
                .or_default()
                .push(
                    record
                        .state
                        .forced
                        .get(&crate::site_questions::Slot::Name)
                        .cloned(),
                );
        }
        let leads: Vec<String> = decided
            .leads
            .iter()
            .map(|lead| format!("{}:{} {:?}", lead.file, lead.line, lead.reason))
            .collect();
        for (point, name) in [
            ("src/send.ts:5", "alpha-jobs"),
            ("src/send.ts:9", "gamma-jobs"),
            ("src/c1-direct.ts:7", "beta-jobs"),
            ("src/c4-literal.ts:6", "literal-jobs"),
        ] {
            assert_eq!(
                forced.get(point).cloned(),
                Some(vec![Some(name.to_string())]),
                "{point}: {forced:#?}\nleads: {leads:#?}"
            );
        }
        // The wrapper's own send is not a site of its own beside the joined
        // ones: the name is decided only where the receiver's construction is.
        assert_eq!(forced.get("src/queue-client.ts:21"), None, "{forced:#?}");
        for names in forced.values() {
            for name in names.iter().flatten() {
                assert!(
                    name != "job" && name != "direct-job",
                    "a job name is forced as a queue name: {forced:#?}"
                );
            }
        }
    }

    /// carrick#4245 T3: a send whose queue name is bound at the maker
    /// (`new Queue(q).add(job, data)`) reads that name at the maker's
    /// argument, where it was decided, not at the send's own arguments (the
    /// job name and the payload). In a loop over a stated list, plain or
    /// `as const`, that is one name per element, keyed as the workers made
    /// over the same list are; through a function that makes the queue
    /// (`open(q).add(...)`) the name is read at the call that hands `q` in.
    /// A literal stays forced, and a list the resolver does not read leaves a
    /// lead with the resolver's own reason.
    #[test]
    fn a_maker_bound_queue_name_in_a_loop_is_read_at_the_maker() {
        let sends = "import { Queue, Worker } from 'bullmq'; import { flip } from './openers';\n\
const SHIPPING = ['ship-created', 'ship-cancelled'] as const;\n\
const BILLING = ['bill-opened', 'bill-closed'];\n\
export async function pushShipping() { for (const q of SHIPPING) await new Queue(q).add('task', { n: 1 }); }\n\
export async function pushBilling() { for (const q of BILLING) await new Queue(q).add('task', { n: 1 }); }\n\
export function workShipping() { for (const q of SHIPPING) new Worker(q, async () => {}); }\n\
export async function pushOne() { await new Queue('one-q').add('task', { n: 1 }); }\n\
function open(name: string) { return new Queue(name); }\n\
export async function pushOpened() { for (const q of BILLING) await open(q).add('task', { n: 1 }); }\n\
declare function listQueues(): string[];\n\
export async function pushListed() { for (const q of listQueues()) await new Queue(q).add('task', { n: 1 }); }\n\
export async function pushHeld() { for (const q of SHIPPING) { const held = new Queue(q); await held.add('task', { n: 1 }); } }\n\
function openTwice(name: string) { return new Queue(name); }\n\
export async function pushTwiceShipping() { for (const q of SHIPPING) await openTwice(q).add('task', { n: 1 }); }\n\
export async function pushTwiceBilling() { for (const q of BILLING) await openTwice(q).add('task', { n: 1 }); }\n\
export async function pushFlip() { await flip.add('task', { n: 1 }); }\n";
        let openers = "import { Queue } from 'bullmq';\n\
const EVEN = 'flip-even';\n\
const ODD = 'flip-odd';\n\
export const flip = new Queue(Date.now() % 2 === 0 ? EVEN : ODD);\n";
        let Some((on, exports, declarations)) = traced_fixture_files(
            "queue-enum-base-class-one",
            ".",
            "jobs",
            &[("src/sends.ts", sends), ("src/openers.ts", openers)],
            |_| {},
        ) else {
            panic!("the sidecar is not built: cd src/sidecar && npm ci && npm run build");
        };
        use super::super::decided_sites::{ScopeLabel, SiteLabel};
        use crate::site_questions::{Answered, SiteAnswers, Slot, SlotAnswer, SlotValue};
        let labels = SiteLabel::from_exports(&exports);
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let records =
            super::super::site_record::site_records(&on.inputs, &[], &on.repo, &decided, &labels);
        let at = |line: u32| format!("src/sends.ts:{line}");
        let shown: Vec<String> = records
            .iter()
            .filter(|record| record.state.point.at.starts_with("src/sends.ts:"))
            .map(|record| {
                format!(
                    "{} {:?} asked={:?} forced={:?} names={:?} unread={:?}",
                    record.state.point.at,
                    record.bucket,
                    record.state.asked,
                    record.state.forced.get(&Slot::Name),
                    record.names,
                    record.unread
                )
            })
            .collect();
        let record_at = |line: u32| {
            let point = at(line);
            records
                .iter()
                .find(|record| record.state.point.at == point)
                .unwrap_or_else(|| panic!("a record at {point}: {shown:#?}"))
        };
        for (line, names) in [
            (4, ["ship-cancelled", "ship-created"]),
            (5, ["bill-closed", "bill-opened"]),
            (6, ["ship-cancelled", "ship-created"]),
            (9, ["bill-closed", "bill-opened"]),
        ] {
            let record = record_at(line);
            assert_eq!(record.state.asked, vec![Slot::Kind], "{line}: {shown:#?}");
            assert_eq!(record.names, names, "{line}: {shown:#?}");
            // The loop variable is one compiler name, read once.
            let compiler = record.compiler().expect("the record's compiler part");
            assert!(
                compiler
                    .names
                    .iter()
                    .filter(|name| name.text == "q")
                    .count()
                    <= 1,
                "{line}: {:#?}",
                compiler.names
            );
        }
        assert_eq!(
            record_at(7)
                .state
                .forced
                .get(&Slot::Name)
                .map(String::as_str),
            Some("one-q"),
            "{shown:#?}"
        );
        let listed = record_at(11);
        assert!(listed.names.is_empty(), "{shown:#?}");
        assert_eq!(
            listed.unread,
            Some(super::super::stop::Stop::UnreadExpression),
            "{shown:#?}"
        );
        // One function makes the queue for two lists: its parameter's
        // values are decided at two calls, so neither send states the
        // other list's names: no record states any, and a lead says why
        // (carrick#4418).
        for line in [14, 15] {
            let point = at(line);
            assert!(
                !records.iter().any(|record| record.state.point.at == point),
                "{line}: {shown:#?}"
            );
            let reasons: Vec<_> = decided
                .leads
                .iter()
                .filter(|lead| lead.file == "src/sends.ts" && lead.line == line)
                .map(|lead| lead.reason)
                .collect();
            assert_eq!(
                reasons,
                [super::super::stop::Stop::SeveralOperations],
                "{line}: {shown:#?}"
            );
        }
        // A maker in another file: its argument is read in that file, one
        // row per branch of the choice it writes.
        assert_eq!(record_at(16).names, ["flip-even", "flip-odd"], "{shown:#?}");
        // The job name is never the queue's name.
        for record in &records {
            assert!(
                !record.names.iter().any(|name| name == "task")
                    && record.state.forced.get(&Slot::Name).map(String::as_str) != Some("task"),
                "a job name read as a queue name: {shown:#?}"
            );
        }
        // Answered `topic`, the sends over the list key as its workers do.
        let mut answers = SiteAnswers::default();
        for record in &records {
            let mut answered = Answered::default();
            if record.state.asked.contains(&Slot::Kind) {
                answered.kind = Some(SlotAnswer {
                    value: Some(SlotValue::Text("topic".to_string())),
                    confidence: 0.95,
                    orders_agree: true,
                });
            }
            answers
                .answered
                .insert(crate::site_rows::site_id(&record.anchors), answered);
        }
        let rows = crate::site_rows::of(&decided, &records, &labels, &answers);
        let keys_at = |line: u32, side: crate::site_rows::RowSide| {
            let mut keys: Vec<String> = rows
                .rows
                .iter()
                .filter(|row| row.file == "src/sends.ts" && row.line == line && row.side == side)
                .map(|row| format!("{:?}", row.key))
                .collect();
            keys.sort();
            keys
        };
        let sent = keys_at(4, crate::site_rows::RowSide::Call);
        assert_eq!(sent.len(), 2, "{:#?}", rows.rows);
        assert_eq!(
            keys_at(6, crate::site_rows::RowSide::Endpoint),
            sent,
            "{:#?}",
            rows.rows
        );
    }

    /// carrick#4418: values that agree on a name's pieces but were decided
    /// at two or more calls (one opener called from two loops) are not one
    /// name, and no one call states them. The send is a lead whose reason
    /// is the resolver's own: the first hole it meets reading a value, else
    /// `several_operations`. No question was asked of the model for it, so
    /// it is never `unanswered`. A name that reads the environment keeps
    /// `env_value`.
    #[test]
    fn maker_values_pooled_at_two_calls_are_a_lead_with_the_resolver_s_reason() {
        use super::super::stop::Stop;
        let pooled = "import { Queue } from 'bullmq';\n\
const SHIPPING = ['ship-created', 'ship-cancelled'] as const;\n\
const BILLING = ['bill-opened', 'bill-closed'];\n\
function open(name: string) { return new Queue(name); }\n\
export async function pushOpened() { for (const q of BILLING) await open(q).add('task', { n: 1 }); }\n\
function openTwice(name: string) { return new Queue(name); }\n\
export async function pushTwiceShipping() { for (const q of SHIPPING) await openTwice(q).add('task', { n: 1 }); }\n\
export async function pushTwiceBilling() { for (const q of BILLING) await openTwice(q).add('task', { n: 1 }); }\n\
declare function loadConfig(): { name: string };\n\
function openCfg(name: string) { return new Queue(name); }\n\
export async function pushCfgA() { const cfg = loadConfig(); await openCfg(cfg.name).add('task', { n: 1 }); }\n\
export async function pushCfgB() { const cfg = loadConfig(); await openCfg(cfg.name).add('task', { n: 1 }); }\n\
function openSame(name: string) { return new Queue(name); }\n\
export async function pushSameA() { for (const q of SHIPPING) await openSame(q).add('task', { n: 1 }); }\n\
export async function pushSameB() { for (const q of SHIPPING) await openSame(q).add('task', { n: 1 }); }\n\
function openLit(name: string) { return new Queue(name); }\n\
export async function pushLitA() { await openLit('lit-a').add('task', { n: 1 }); }\n\
export async function pushLitB() { await openLit('lit-b').add('task', { n: 1 }); }\n\
function openDiff(name: string) { return new Queue(name); }\n\
export async function pushDiffA() { for (const a of SHIPPING) await openDiff(a).add('task', { n: 1 }); }\n\
export async function pushDiffB() { for (const b of BILLING) await openDiff(b).add('task', { n: 1 }); }\n\
declare const process: { env: Record<string, string> };\n\
function openEnv(name: string) { return new Queue(name); }\n\
export async function pushEnvA() { await openEnv(process.env.JOB_QUEUE).add('task', { n: 1 }); }\n\
export async function pushEnvB() { await openEnv(process.env.JOB_QUEUE).add('task', { n: 1 }); }\n\
function openEnvOne(name: string) { return new Queue(name); }\n\
export async function pushEnvOne() { await openEnvOne(process.env.JOB_QUEUE).add('task', { n: 1 }); }\n";
        let Some((on, exports, declarations)) = traced_fixture_files(
            "queue-enum-base-class-one",
            ".",
            "jobs",
            &[("src/pooled.ts", pooled)],
            |_| {},
        ) else {
            panic!("the sidecar is not built: cd src/sidecar && npm ci && npm run build");
        };
        use super::super::decided_sites::{ScopeLabel, SiteLabel};
        let labels = SiteLabel::from_exports(&exports);
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let records =
            super::super::site_record::site_records(&on.inputs, &[], &on.repo, &decided, &labels);
        let calls = crate::possible_calls::from_sites(&decided, &records);
        let shown: Vec<String> = records
            .iter()
            .filter(|record| record.state.point.at.starts_with("src/pooled.ts:"))
            .map(|record| {
                format!(
                    "{} {:?} names={:?} unread={:?}",
                    record.state.point.at, record.bucket, record.names, record.unread
                )
            })
            .collect();
        let reason_at = |line: u32| -> Option<Stop> {
            calls
                .iter()
                .find(|call| call.file_path == "src/pooled.ts" && call.line_number == line)
                .map(|call| call.unread.reason)
        };
        let has_record = |line: u32| {
            let point = format!("src/pooled.ts:{line}");
            records.iter().any(|record| record.state.point.at == point)
        };
        // One opener, one caller: the names are read at that call.
        assert!(has_record(5), "{shown:#?}");
        assert_eq!(reason_at(5), None, "{shown:#?}");
        // Two loops over different lists through one opener, and the same
        // list looped twice: the send states no name, and several
        // operations say why.
        for line in [7, 8, 14, 15] {
            assert!(!has_record(line), "{line}: {shown:#?}");
            assert_eq!(
                reason_at(line),
                Some(Stop::SeveralOperations),
                "{line}: {shown:#?}"
            );
        }
        // A field of a call result at two calls: the resolver cannot read it.
        for line in [11, 12] {
            assert!(!has_record(line), "{line}: {shown:#?}");
            assert_eq!(
                reason_at(line),
                Some(Stop::UnreadExpression),
                "{line}: {shown:#?}"
            );
        }
        // Two literals and two different identifiers through one opener stay
        // several operations.
        for line in [17, 18, 20, 21] {
            assert_eq!(
                reason_at(line),
                Some(Stop::SeveralOperations),
                "{line}: {shown:#?}"
            );
        }
        // An environment read keeps its own word, at two calls and at one.
        for line in [24, 25, 27] {
            assert_eq!(reason_at(line), Some(Stop::EnvValue), "{line}: {shown:#?}");
        }
        // No question was asked for a site that states no name here.
        for call in calls
            .iter()
            .filter(|call| call.file_path == "src/pooled.ts")
        {
            assert_ne!(call.unread.reason, Stop::Unanswered, "{shown:#?}");
        }
    }

    /// The `queue-enum-base-class` sender's decided sites and records, with
    /// the shared package `queue-kit` as a sibling, and the enum package
    /// `topics` too where `with_topics`. The engine reaches siblings through
    /// call declarations only, so a scan reaches `queue-kit` alone.
    fn queue_kit_sender_sites(
        with_topics: bool,
    ) -> (
        super::super::decided_sites::Decided,
        Vec<super::super::site_record::SiteRecord>,
    ) {
        let services = [
            ("services/sender", "sender"),
            ("packages/queue-kit", "queue-kit"),
            ("packages/topics", "topics"),
        ];
        let Some(on) =
            super::super::fixture_walk::walked_services("queue-enum-base-class", &services, true)
        else {
            panic!("the sidecar is not built: cd src/sidecar && npm ci && npm run build");
        };
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/queue-enum-base-class/library-claims.json"),
        )
        .unwrap();
        let answer = crate::library_store::parse_answer(&text);
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        let exports = answer.exports;
        let configs =
            crate::config::Config::load_services(vec![on.repo.join("carrick.json")]).unwrap();
        let config = configs
            .iter()
            .find(|config| config.service_name.as_deref() == Some("sender"))
            .expect("carrick.json declares the sender");
        let script = super::super::fixture_walk::sidecar().expect("the compiler engine is built");
        let [own, kit, topics] = on.inputs.as_slice() else {
            panic!("three services");
        };
        let declarations = super::super::fixture_walk::census_declarations(
            &script, &on.repo, config, own, &exports,
        );
        let siblings: Vec<&RequestSummaryInputs> = if with_topics {
            vec![kit, topics]
        } else {
            vec![kit]
        };
        use super::super::decided_sites::{
            MakerLabel, ProcedureLabels, ScopeLabel, SiteLabel, decided_sites_traced_with,
        };
        let discovery = discover_with(
            own,
            &siblings,
            &Labels::of(&exports).tracing(&declarations),
            &on.repo,
        );
        let discovered: Vec<super::super::decided_sites::DiscoverySite> = discovery
            .sites
            .iter()
            .map(|site| serde_json::from_value(serde_json::to_value(site).unwrap()).unwrap())
            .collect();
        let labels = SiteLabel::from_exports(&exports);
        let decided = decided_sites_traced_with(
            own,
            &siblings,
            &on.repo,
            &discovered,
            &labels,
            &MakerLabel::from_exports(&exports),
            &ScopeLabel::from_exports(&exports, &declarations.scope_answers),
            &ProcedureLabels::from_exports(&exports, &declarations.scope_answers),
        );
        let records =
            super::super::site_record::site_records(own, &siblings, &on.repo, &decided, &labels);
        (decided, records)
    }

    /// carrick#3527 S5: the instance walk reads the service that holds each
    /// file, so a send through a wrapper declared as a sibling service
    /// (`new AlphaProducer().add(...)`, the queue made two files away in the
    /// shared package) ends at the maker and is named by its construction.
    /// No site or lead stands in the package's files, and the job name is
    /// never the queue's name.
    #[test]
    fn a_maker_bound_name_made_in_a_sibling_package_is_read_at_the_send() {
        let (decided, records) = queue_kit_sender_sites(true);
        let mut forced: BTreeMap<String, Vec<Option<String>>> = BTreeMap::new();
        for record in &records {
            forced
                .entry(record.state.point.at.clone())
                .or_default()
                .push(
                    record
                        .state
                        .forced
                        .get(&crate::site_questions::Slot::Name)
                        .cloned(),
                );
        }
        let leads: Vec<String> = decided
            .leads
            .iter()
            .map(|lead| format!("{}:{} {:?}", lead.file, lead.line, lead.reason))
            .collect();
        for (point, name) in [
            ("services/sender/src/send.ts:4", "alpha-jobs"),
            ("services/sender/src/send.ts:8", "gamma-jobs"),
        ] {
            assert_eq!(
                forced.get(point).cloned(),
                Some(vec![Some(name.to_string())]),
                "{point}: {forced:#?}\nleads: {leads:#?}"
            );
        }
        assert!(
            forced.keys().all(|point| !point.starts_with("packages/")),
            "a site stands in a sibling's file: {forced:#?}"
        );
        assert!(
            leads.iter().all(|lead| !lead.starts_with("packages/")),
            "a lead stands in a sibling's file: {leads:#?}"
        );
        for names in forced.values() {
            for name in names.iter().flatten() {
                assert!(
                    name != "job" && name != "direct-job",
                    "a job name is forced as a queue name: {forced:#?}"
                );
            }
        }
    }

    /// carrick#4419: a maker-bound name decided at a call in a sibling
    /// service's file is read there, through the resolver every
    /// cross-service read uses, so the send is a lead with the resolver's
    /// reason, not a lead that was never asked about. With only the shared
    /// package as a sibling, as a scan reaches it, the enum the name reads
    /// is declared in a service the run never enters. This test hands in its
    /// sibling set by hand, so it guards the reason only: the binary test
    /// `queue_enum_base_class_test` runs the engine's own set.
    #[test]
    fn a_maker_bound_name_decided_in_a_sibling_is_read_there() {
        use super::super::stop::Stop;
        let (_, records) = queue_kit_sender_sites(false);
        for line in [4, 8] {
            let point = format!("services/sender/src/send.ts:{line}");
            let found: Vec<_> = records
                .iter()
                .filter(|record| record.state.point.at == point)
                .collect();
            let [record] = found.as_slice() else {
                panic!("one record at {point}: {records:#?}");
            };
            assert_eq!(
                record.unread,
                Some(Stop::OutsideService),
                "{point}: {:?}",
                record.names
            );
            assert_eq!(
                record.state.forced.get(&crate::site_questions::Slot::Name),
                None,
                "{point}"
            );
            assert!(
                !record.names.iter().any(|name| name == "job"),
                "{point}: {:?}",
                record.names
            );
        }
    }

    /// carrick#3527: the walk up a maker-bound name's callers is capped
    /// (`MAX_COMPOSED` calls visited). Past the cap, here 128 paths through
    /// seven levels of parameter-passing helpers, the send stays one site
    /// with its name asked, never dropped, and says the cap stopped it.
    #[test]
    fn a_maker_bound_name_past_the_walk_cap_stays_unnamed() {
        let Some((on, exports, declarations)) =
            traced_fixture_in("queue-maker-fan-out", ".", "jobs")
        else {
            panic!("the sidecar is not built: cd src/sidecar && npm ci && npm run build");
        };
        use super::super::decided_sites::{ScopeLabel, SiteLabel};
        let labels = SiteLabel::from_exports(&exports);
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let records =
            super::super::site_record::site_records(&on.inputs, &[], &on.repo, &decided, &labels);
        let mut forced: BTreeMap<String, Vec<Option<String>>> = BTreeMap::new();
        for record in &records {
            forced
                .entry(record.state.point.at.clone())
                .or_default()
                .push(
                    record
                        .state
                        .forced
                        .get(&crate::site_questions::Slot::Name)
                        .cloned(),
                );
        }
        assert_eq!(
            forced.get("src/open.ts:8").cloned(),
            Some(vec![None]),
            "{forced:#?}"
        );
        let sites: Vec<_> = decided
            .sites
            .iter()
            .filter(|site| site.file == "src/open.ts" && site.line == 8)
            .collect();
        assert_eq!(sites.len(), 1, "{:#?}", decided.sites);
        assert!(sites[0].composed_trimmed, "{:#?}", sites[0]);
    }

    /// carrick#1678: a verb called on the object a scope member returns
    /// (`items.at("/items").get(list)`) is a route whose path is the scope
    /// call's argument and whose instance is the scope call's receiver: the
    /// scope's path is the route's own path, never also a prefix. The verb is
    /// the scoped op's (`get` of `Route`), not the router's `get` of the same
    /// name, which would read the handler as the path.
    #[test]
    fn a_route_on_the_object_a_scope_member_returns_is_composed_under_its_router() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-scopes-chain")
        else {
            return;
        };
        use super::super::decided_sites::{ScopeLabel, SiteLabel};
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let got = composed_lines(&decided, false);
        // The scope's path is the route's own path, served under the
        // router's mount. A path the code cannot read composes none.
        let want = texts(&[
            // Across lines: the site is the call, at the chain's first line.
            "src/items.ts:27 <- /api/multi",
            "src/items.ts:25 <- /api/items",
            // A module-scope constant bound to the scope call.
            "src/items.ts:32 <- /api/one",
            // A helper's path parameter: each caller states one.
            "src/items.ts:37 <- /api/p1",
            "src/items.ts:38 <- /api/p2",
            // The first verb of a chain of two; the second waits on slice B.
            "src/items.ts:40 <- /api/items/:id",
            "src/items.ts:56 <- ?",
        ]);
        let mut want = want;
        want.sort();
        assert_eq!(got, want, "{got:#?}");
        // A constant local to a function, a `let` written twice and a result
        // handed to a helper state no scope call: leads, never sites.
        let leads: Vec<String> = decided
            .leads
            .iter()
            .map(|lead| format!("{}:{} {:?}", lead.file, lead.line, lead.reason))
            .collect();
        assert_eq!(
            leads,
            texts(&[
                "src/items.ts:40 NameNeverBound",
                "src/items.ts:44 NameNeverBound",
                "src/items.ts:49 NameNeverBound",
                "src/items.ts:52 NameNeverBound",
            ])
        );
        // The rows the trace writes: the verb is the scoped op's, the owner
        // is the scope call's receiver (`items`, or `r` inside the helper),
        // never the scope member (`at`), and the name is the scope call's
        // argument, forced by the code with no call argument to ask for.
        let labels = SiteLabel::from_exports(&exports);
        let records =
            super::super::site_record::site_records(&on.inputs, &[], &on.repo, &decided, &labels);
        let routes = crate::site_rows::traced(&decided, &records);
        let mut served: Vec<String> = routes
            .routes
            .iter()
            .map(|route| {
                format!(
                    "{}:{} {} {} <- {} owner {}",
                    route.file,
                    route.line,
                    route.method,
                    route.path,
                    route.full_paths.join(" "),
                    route.owner.as_deref().unwrap_or("-"),
                )
            })
            .collect();
        served.sort();
        assert_eq!(
            served,
            texts(&[
                "src/items.ts:25 GET /items <- /api/items owner items",
                "src/items.ts:27 POST /multi <- /api/multi owner items",
                "src/items.ts:32 GET /one <- /api/one owner items",
                "src/items.ts:37 GET /p1 <- /api/p1 owner r",
                "src/items.ts:38 GET /p2 <- /api/p2 owner r",
                "src/items.ts:40 GET /items/:id <- /api/items/:id owner items",
            ]),
            "{served:#?}"
        );
        let first = records
            .iter()
            .find(|record| record.state.point.at == "src/items.ts:25")
            .expect("a record at the first chain");
        assert_eq!(
            first.state.forced.get(&crate::site_questions::Slot::Name),
            Some(&"/items".to_string())
        );
        assert_eq!(
            first.basis.get(&crate::site_questions::Slot::Name),
            Some(&super::super::site_record::Basis::Code)
        );
        assert!(
            !first
                .state
                .forced
                .contains_key(&crate::site_questions::Slot::NameArgument)
                && !first
                    .state
                    .asked
                    .contains(&crate::site_questions::Slot::NameArgument),
            "no argument of the verb holds the name"
        );
    }

    /// carrick#2775: the mounts made in a file only tests import are not
    /// read. The helper `harness.ts` (no test name, imported by one test
    /// file alone) hands the router to a member no label names and mounts it
    /// under another prefix; the router is served at its production mount
    /// alone (T1) and the run counts the two mounts it ignored (T4). A
    /// helper a production file also imports keeps its path-less mount
    /// unverified (T2). Only `harness.ts` is test-only (T3).
    #[test]
    fn a_mount_in_a_file_only_tests_import_is_ignored() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-mount-test-helper")
        else {
            return;
        };
        let mut only: Vec<String> = on
            .inputs
            .test_only
            .iter()
            .map(|path| path.strip_prefix(&on.repo).unwrap().display().to_string())
            .collect();
        only.sort();
        assert_eq!(only, vec!["src/edge.ts", "src/harness.ts", "src/worker.ts"]);
        let scopes = super::super::decided_sites::ScopeLabel::from_exports(
            &exports,
            &declarations.scope_answers,
        );
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        assert_eq!(
            composed_lines(&decided, false),
            texts(&[
                "src/api.ts:8 <- /v1/items",
                "src/api.ts:9 <- /v1/items",
                "src/edge.ts:8 <- /v2/ping",
                "src/jobs.ts:8 <- /jobs (mount_unverified)",
            ])
        );
        assert_eq!(
            decided.test_only_mounts,
            texts(&["src/harness.ts:7", "src/harness.ts:9"])
        );
    }

    /// The composed paths of the route sites of the fixture `name`, with the
    /// receiver trace on and the scope claims its own request verified.
    fn composed_fixture(name: &str, dir: &str, service: &str) -> Option<Vec<String>> {
        let (on, exports, declarations) = traced_fixture_in(name, dir, service)?;
        let scopes = super::super::decided_sites::ScopeLabel::from_exports(
            &exports,
            &declarations.scope_answers,
        );
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        Some(composed_lines(&decided, true))
    }

    /// carrick#3489 W1: a class field that every write in its class sets to
    /// one maker's instance (an initialiser, or the constructor) is that
    /// instance: the site's end is the maker, and so is the end of a
    /// sub-router handed to a mount by the field (`this.router`). A field
    /// written twice, with two makers' arguments, or by a class that lets
    /// `this` escape, is read for no instance, so its end is unread; a field
    /// the constructor takes as a parameter is still that parameter's, and an
    /// app a package builds itself is still outside this service.
    #[test]
    fn a_field_a_maker_initialises_or_the_constructor_writes_is_that_instance() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-forms-routes")
        else {
            return;
        };
        let scopes = super::super::decided_sites::ScopeLabel::from_exports(
            &exports,
            &declarations.scope_answers,
        );
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let got = without_files(
            without_files(
                without_files(instance_ends(&decided), MODULE_ALIAS_FILES),
                GETTER_FILES,
            ),
            CALL_RESULT_FILES,
        );
        assert_eq!(
            got,
            texts(&[
                "src/ri-class-field.ts:2 <- src/ri-class-field.ts:2 root src/ri-class-field.ts:2",
                "src/ri-param-property.ts:3 <- src/ri-param-property.ts:4 root src/ri-param-property.ts:4",
                "src/s-ctor-assign.ts:2 <- src/s-ctor-assign.ts:2 root src/s-ctor-assign.ts:2",
                "src/s-escape.ts:3 <- src/s-escape.ts:3 unread",
                "src/s-mount.ts:6 <- src/s-mount.ts:6 root src/s-mount.ts:4",
                "src/s-shared.ts:2 <- src/s-shared.ts:2 outside_service",
                "src/s-twice.ts:2 <- src/s-twice.ts:2 unread",
            ]),
            "{got:#?}"
        );
    }

    /// The `receiver-forms-routes` files carrick#3489 W2 reads.
    const MODULE_ALIAS_FILES: &[&str] = &[
        "src/ri-const-alias.ts:",
        "src/s-destructured.ts:",
        "src/s-local-alias.ts:",
    ];

    /// The `receiver-forms-routes` files carrick#3489 W4 reads.
    const GETTER_FILES: &[&str] = &["src/ri-getter.ts:", "src/s-getter-builds.ts:"];

    /// The `receiver-forms-routes` files carrick#3504 S3c reads.
    const CALL_RESULT_FILES: &[&str] = &["src/s-call-result.ts:"];

    /// `ends` without the lines of the files named by `prefixes`.
    fn without_files(ends: Vec<String>, prefixes: &[&str]) -> Vec<String> {
        ends.into_iter()
            .filter(|end| !prefixes.iter().any(|prefix| end.starts_with(prefix)))
            .collect()
    }

    /// `ends` for the files named by `prefixes` alone.
    fn only_files(ends: Vec<String>, prefixes: &[&str]) -> Vec<String> {
        ends.into_iter()
            .filter(|end| prefixes.iter().any(|prefix| end.starts_with(prefix)))
            .collect()
    }

    /// carrick#3489 W2: a module-scope `const` that names another module
    /// binding holds what it holds, and so does a key of a module object
    /// literal destructured into a `const`: the site's end is the maker.
    /// An alias declared once inside a function reads through to its source
    /// too: the receiver is the value resolver's lowering (carrick#3504 S3a).
    #[test]
    fn a_module_alias_or_destructured_key_holds_its_source_instance() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-forms-routes")
        else {
            return;
        };
        let scopes = super::super::decided_sites::ScopeLabel::from_exports(
            &exports,
            &declarations.scope_answers,
        );
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let got = only_files(instance_ends(&decided), MODULE_ALIAS_FILES);
        assert_eq!(
            got,
            texts(&[
                "src/ri-const-alias.ts:4 <- src/ri-const-alias.ts:4 root src/ri-const-alias.ts:2",
                "src/s-destructured.ts:4 <- src/s-destructured.ts:4 root src/s-destructured.ts:2",
                // A function's own alias reads through to its source
                // (carrick#3504 S3a).
                "src/s-local-alias.ts:3 <- src/s-local-alias.ts:3 root src/s-local-alias.ts:2",
            ]),
            "{got:#?}"
        );
    }

    /// carrick#3489 W5 (carrick#3504 S1): a module `let` holds every value
    /// written to it. Its initialiser's maker is a root; a write of an
    /// exported setter's parameter, which nothing in the service calls, is
    /// an entry point; a write the census cannot place (a destructuring
    /// target) leaves the binding unread. The answer key is
    /// `tests/fixtures/receiver-let-routes/README.md`.
    #[test]
    fn a_module_let_ends_at_every_value_written_to_it() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-let-routes") else {
            return;
        };
        let scopes = super::super::decided_sites::ScopeLabel::from_exports(
            &exports,
            &declarations.scope_answers,
        );
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let got = instance_ends(&decided);
        assert_eq!(
            got,
            texts(&[
                "src/s-let-never.ts:3 <- src/s-let-never.ts:3 root src/s-let-never.ts:2",
                "src/s-let-unplaced.ts:4 <- src/s-let-unplaced.ts:4 unread",
                "src/s-let.ts:4 <- src/s-let.ts:4 entry_point",
                "src/s-let.ts:4 <- src/s-let.ts:4 root src/s-let.ts:2",
            ]),
            "{got:#?}"
        );
    }

    /// carrick#3489 W4: a class getter holds what it returns: a getter that
    /// returns a module-scope instance ends at that instance's maker, and one
    /// that builds the instance ends at the maker it calls.
    #[test]
    fn a_getter_holds_what_it_returns() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-forms-routes")
        else {
            return;
        };
        let scopes = super::super::decided_sites::ScopeLabel::from_exports(
            &exports,
            &declarations.scope_answers,
        );
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let got = only_files(instance_ends(&decided), GETTER_FILES);
        assert_eq!(
            got,
            texts(&[
                "src/ri-getter.ts:3 <- src/ri-getter.ts:3 root src/ri-getter.ts:2",
                "src/s-getter-builds.ts:2 <- src/s-getter-builds.ts:2 root src/s-getter-builds.ts:2",
            ]),
            "{got:#?}"
        );
    }

    /// carrick#3504 S3c: a receiver that is the call of an own function
    /// holds what the function returns: `getApp().get(..)` where `getApp`
    /// returns a module-scope instance ends at that instance's maker.
    #[test]
    fn a_function_result_receiver_holds_what_the_function_returns() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-forms-routes")
        else {
            return;
        };
        let scopes = super::super::decided_sites::ScopeLabel::from_exports(
            &exports,
            &declarations.scope_answers,
        );
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let got = only_files(instance_ends(&decided), CALL_RESULT_FILES);
        assert_eq!(
            got,
            texts(&[
                "src/s-call-result.ts:4 <- src/s-call-result.ts:4 root src/s-call-result.ts:2",
            ]),
            "{got:#?}"
        );
    }

    /// carrick#3185: a package publishes its factory as the module, as a
    /// named export and as `default`, and the store labels the same route
    /// ops under both names but the `register` scope under the named one
    /// alone. The app is made through `default`. A route registered on a
    /// plugin's own app (a parameter, written in place or handed by name,
    /// and one plugin down) resolves to a member declaration both exports'
    /// ops name the same way: it is one op, so a site; and the `register`
    /// call on the `default` app resolves to the declaration the named
    /// export's scope was verified at, so it opens that scope. Every route
    /// takes its full path, the nested one the version its caller states.
    #[test]
    fn routes_on_a_plugin_app_of_a_factory_published_under_two_names_take_their_paths() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-scopes-aliased")
        else {
            return;
        };
        let scopes = super::super::decided_sites::ScopeLabel::from_exports(
            &exports,
            &declarations.scope_answers,
        );
        let (discovery, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        assert_eq!(discovery.counts.get("declaration_ambiguous"), None);
        let got = composed_lines(&decided, false);
        assert_eq!(
            got,
            texts(&[
                "src/catalog.ts:11 <- /api/products",
                "src/catalog.ts:14 <- /api/v2/orders",
                "src/index.ts:12 <- /internal/status",
                "src/index.ts:7 <- /health",
            ]),
            "{got:#?}"
        );
    }

    /// carrick#3044 S2: a package labels its client under the export that
    /// holds an instance (`http`, `on: export`) and under the type of that
    /// instance (`Client`, `of: type`); both ops stand on the one
    /// declaration `Client.post`. They read the member the same way, so they
    /// are one op: a call whose receiver is `a ?? http` or a function's
    /// result is a site by the member's declaration, and none is ambiguous.
    #[test]
    fn an_export_label_and_a_type_label_on_one_declaration_are_one_op() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-forms-typed-label")
        else {
            return;
        };
        let discovery = discover(
            &on.inputs,
            &Labels::of(&exports).tracing(&declarations),
            &on.repo,
        );
        assert_eq!(discovery.counts.get("declaration_ambiguous"), None);
        let mut got: Vec<String> = discovery
            .sites
            .iter()
            .filter(|site| site.end.is_some())
            .map(|site| {
                let reached = site.reached.map_or("-".to_string(), |by| {
                    serde_json::to_value(by)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_string()
                });
                format!("{} {reached}", site.file)
            })
            .collect();
        got.sort();
        assert_eq!(
            got,
            texts(&[
                "src/t-call-result-receiver.ts declaration",
                "src/t-coalesce-receiver.ts declaration",
                "src/t-typed-const.ts name",
            ]),
            "{got:#?}"
        );
    }

    /// carrick#3207: the routes package registers its plugin on an app it is
    /// handed; the host makes that app through the package's `default` export
    /// while the store labels `register` on the named export alone. The
    /// host's `register` call records a call-in for the plugin all the same:
    /// the labels of the app's package verified at the member declaration the
    /// call resolves to apply to the end, as they do to a scope a service
    /// opens itself (carrick#3185). The call-in is what the plugin's route
    /// segments join at.
    #[test]
    fn a_plugin_registered_on_an_app_made_through_the_other_export_records_its_call_in() {
        use super::super::decided_sites::ScopeLabel;
        let Some((on, exports, declarations)) = traced_fixture_in(
            "receiver-scopes-default-export",
            "packages/server",
            "server",
        ) else {
            return;
        };
        let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let lines = boundary_lines(&decided.boundaries);
        let call_ins: Vec<&String> = lines
            .iter()
            .filter(|line| line.starts_with("call_in "))
            .collect();
        assert_eq!(
            call_ins,
            vec![
                "call_in packages/routes/src/index.ts:11 p0 at packages/server/src/main.ts:8 [-] <- (top)"
            ],
            "{lines:#?}"
        );
    }

    /// carrick#2474 R2b on carrick#2325's tree (`d565a091`, its instances
    /// typed against a made-up plugin-server package): each route takes
    /// the prefix of every plugin scope above it, with an empty model
    /// answer. Nothing calls the top plugins, so each path is relative to
    /// the app they are handed (`entry_point`), as the closed PR's answer
    /// key reads them.
    #[test]
    fn the_inline_plugin_tree_composes_its_prefixes() {
        let Some(got) = composed_fixture("receiver-scopes-inline", ".", "web") else {
            return;
        };
        let want = texts(&[
            "src/admin-routes.ts:3 <- /admin/users (entry_point) via ",
            "src/admin-routes.ts:3 <- /admin/users (unread) via ",
            "src/callbacks.ts:6 <- /ids (entry_point) via ",
            "src/chain.ts:3 <- /api/v1/leaf (entry_point) via src/helper-depth-3.ts:5 register /api/v1",
            "src/chain.ts:3 <- /api/v1/leaf (entry_point) via src/helper-depth-4.ts:5 register /api/v1",
            "src/errors.ts:10 <- /internal/status (entry_point) via src/status-index.ts:5 register /internal",
            "src/handler.ts:4 <- /me (entry_point) via ",
            "src/helper-local.ts:4 <- /api/v1/local (entry_point) via src/helper-local.ts:8 register /api/v1",
            // A default (`opts.prefix ?? '/api/v1'`, `||`, a parameter
            // default) and a conditional state each of their values, one
            // route each (carrick#2635, carrick#2474 Q2). A parameter takes
            // what each caller hands in (here the options `register` hands
            // the plugin it runs: `nullish.ts` states `/root`, so its default
            // does not apply); the default stands where a caller omits the
            // value or none is known. A parameter nothing fills and no
            // default states is unread.
            "src/item-detail.ts:3 <- /api/v1/items/:id (entry_point) via src/documented.ts:5 register /api/v1 > src/documented.ts:7 register -",
            "src/item-detail.ts:3 <- /api/v1/items/:id (entry_point) via src/two-options.ts:5 register /api/v1 > src/two-options.ts:7 register -",
            "src/items.ts:3 <- /a/items (entry_point) via src/two-scopes.ts:6 register /a > src/two-scopes.ts:8 register -",
            "src/items.ts:3 <- /api/v1/items (entry_point) via src/silent.ts:5 register /api/v1 > src/silent.ts:7 register -",
            "src/items.ts:3 <- /api/v2/items (entry_point) via src/either.ts:5 register /api/v2 > src/either.ts:7 register -",
            "src/items.ts:3 <- /api/v3/items (entry_point) via src/ternary.ts:5 register /api/v3|/legacy > src/ternary.ts:7 register -",
            "src/items.ts:3 <- /api/v4/items (entry_point) via src/param-default.ts:5 register /api/v4 > src/param-default.ts:7 register -",
            "src/items.ts:3 <- /api/v5/items (entry_point) via src/const-prefix.ts:7 register /api/v5 > src/const-prefix.ts:9 register -",
            // A spread no key of which is `prefix` leaves the slot stated
            // as absent; one that may set it, or a spread of a value, leaves
            // it unread (carrick#2474 R5e).
            "src/items.ts:3 <- /items (entry_point) via src/spread-absent.ts:5 register - > src/spread-absent.ts:7 register -",
            "src/items.ts:3 <- /legacy/items (entry_point) via src/ternary.ts:5 register /api/v3|/legacy > src/ternary.ts:7 register -",
            "src/items.ts:3 <- /root/root/items (entry_point) via src/index.ts:5 register /root > src/nullish.ts:5 register /root > src/nullish.ts:7 register -",
            "src/items.ts:3 <- ? (entry_point) via src/spread-sets.ts:5 register ? > src/spread-sets.ts:7 register -",
            "src/items.ts:3 <- ? (entry_point) via src/spread-value.ts:5 register ? > src/spread-value.ts:7 register -",
            // The unread prefix renders as its address part's unknown piece,
            // as written (address chain S1).
            "src/items.ts:3 <- ? (entry_point) via src/unread.ts:5 register ?opts.prefix > src/unread.ts:7 register -",
            "src/nullish.ts:8 <- /root/root/health (entry_point) via src/index.ts:5 register /root > src/nullish.ts:5 register /root",
            "src/orders.ts:3 <- /b/orders (entry_point) via src/two-scopes.ts:12 register /b > src/two-scopes.ts:14 register -",
            "src/orders.ts:3 <- /orders (entry_point) via src/no-options.ts:5 register - > src/no-options.ts:6 register -",
            "src/orders.ts:3 <- /v2/orders (entry_point) via src/group.ts:5 group /v2 > src/group.ts:5 register -",
            "src/pair.ts:4 <- /api/v1/left (entry_point) via src/helper-pair.ts:5 register /api/v1",
            "src/pair.ts:8 <- /api/v1/right (entry_point) via src/helper-pair.ts:5 register /api/v1",
            "src/reports.ts:11 <- /api/v1/feed (entry_point) via src/documented-modes.ts:5 register /api/v1 > src/documented-modes.ts:7 register -",
            "src/reports.ts:15 <- /api/v1/reports (entry_point) via src/documented-modes.ts:5 register /api/v1 > src/documented-modes.ts:7 register -",
            "src/reports.ts:5 <- /api/v1/reports/:id (entry_point) via src/documented-modes.ts:5 register /api/v1 > src/documented-modes.ts:7 register -",
            "src/reports.ts:9 <- /api/v1/reports/:id/lines (entry_point) via src/documented-modes.ts:5 register /api/v1 > src/documented-modes.ts:7 register -",
            "src/reports.ts:9 <- /api/v1/reports/:id/notes (entry_point) via src/documented-modes.ts:5 register /api/v1 > src/documented-modes.ts:7 register -",
            "src/request-hooks.ts:9 <- /hook-status (entry_point) via ",
            "src/team-items.ts:3 <- /a/teams/:id/items (entry_point) via src/helper-two-scopes-chain.ts:5 register /a",
            "src/team-items.ts:3 <- /a/teams/:id/items (entry_point) via src/helper-two-scopes.ts:5 register /a",
            "src/team-items.ts:3 <- /a/teams/:id/items (entry_point) via src/relay-same-name-a.ts:10 register /a",
            "src/team-items.ts:3 <- /api/v1/teams/:id/items (entry_point) via src/helper-both.ts:5 register /api/v1",
            "src/team-items.ts:3 <- /api/v1/teams/:id/items (entry_point) via src/helper-chain.ts:5 register /api/v1",
            "src/team-items.ts:3 <- /api/v1/teams/:id/items (entry_point) via src/helper-first.ts:5 register /api/v1",
            "src/team-items.ts:3 <- /api/v1/teams/:id/items (entry_point) via src/helper-other-instance.ts:6 register /api/v1",
            "src/team-items.ts:3 <- /api/v1/teams/:id/items (entry_point) via src/helper-plugin.ts:5 register /api/v1 > src/helper-plugin.ts:8 register -",
            "src/team-items.ts:3 <- /b/teams/:id/items (entry_point) via src/helper-two-scopes-chain.ts:11 register /b",
            "src/team-items.ts:3 <- /b/teams/:id/items (entry_point) via src/helper-two-scopes.ts:11 register /b",
            "src/team-items.ts:3 <- /root/api/v1/teams/:id/items (entry_point) via src/relay-index.ts:5 register /root > src/relay-local.ts:11 register /api/v1",
            "src/team-items.ts:3 <- /teams/:id/items (runs_by_library) via ",
            "src/team-items.ts:3 <- /teams/:id/items (unread) via ",
            "src/team-notes.ts:4 <- /api/v1/teams/:id/notes (entry_point) via src/helper-both.ts:5 register /api/v1",
            "src/team-notes.ts:4 <- /api/v1/teams/:id/notes (entry_point) via src/helper-second.ts:5 register /api/v1",
            "src/team-notes.ts:4 <- /b/teams/:id/notes (entry_point) via src/relay-same-name-b.ts:10 register /b",
            "src/team-notes.ts:4 <- /teams/:id/notes (unread) via ",
            "src/twin.ts:4 <- /api/v1/items/:id (entry_point) via ",
        ]);
        assert_eq!(got, want, "{got:#?}");
    }

    /// carrick#2474 R2b on carrick#2353's tree (`93cf47ec`, typed the same
    /// way): a function written under an option key that the plugin calls
    /// with its scope (`opts.registerExtra(api)`) is a hole,
    /// `callback_option`, in each service, until carrick#2432's
    /// injected-callee step reads the key out of the object literal; the
    /// path below it composes. Waits on carrick#2432.
    #[test]
    fn the_callback_option_tree_records_its_holes() {
        let Some(server) =
            composed_fixture("receiver-scopes-callback", "packages/server", "server")
        else {
            return;
        };
        // A route a function under an option key registers: relative to
        // the option's hole. Waits on carrick#2432.
        let want =
            texts(&["packages/server/src/extra.ts:7 <- /teams/:id/audit (callback_option) via "]);
        assert_eq!(server, want, "{server:#?}");
        let Some(routes) =
            composed_fixture("receiver-scopes-callback", "packages/routes", "routes")
        else {
            return;
        };
        // Each function another service hands its scope is an entry point
        // here; the package's own plugin reads its prefix off its options
        // (`opts.prefix ?? '/api/v1'`), recorded unread.
        let want = texts(&[
            "packages/routes/src/built-routes.ts:3 <- /teams/:id/built (entry_point) via ",
            "packages/routes/src/computed-routes.ts:3 <- /teams/:id/computed (entry_point) via ",
            "packages/routes/src/legacy.ts:3 <- /teams/:id/legacy (entry_point) via ",
            "packages/routes/src/notes.ts:7 <- /teams/:id/notes (entry_point) via ",
            "packages/routes/src/orders.ts:3 <- /api/v1/orders (entry_point) via packages/routes/src/plugin.ts:13 register /api/v1 > packages/routes/src/plugin.ts:15 register -",
            "packages/routes/src/root-routes.ts:3 <- /teams/:id/root (entry_point) via ",
            "packages/routes/src/shared.ts:5 <- /admin/keys (entry_point) via ",
            "packages/routes/src/shared.ts:9 <- /public/keys (entry_point) via ",
            "packages/routes/src/spread-routes.ts:3 <- /teams/:id/spread (entry_point) via ",
            "packages/routes/src/tags.ts:3 <- /teams/:id/tags (entry_point) via ",
            "packages/routes/src/twice-routes.ts:3 <- /teams/:id/twice (entry_point) via ",
        ]);
        assert_eq!(routes, want, "{routes:#?}");
    }

    /// One service's boundary facts as text (carrick#2474 R5a): `route
    /// file:line METHOD path <- below @ boundary`, `call_in function pN at
    /// call [host prefix] <- above`, `hand_out function pN.key argN <-
    /// instance`.
    fn boundary_lines(boundaries: &super::super::decided_sites::RouteBoundaries) -> Vec<String> {
        use super::super::decided_sites::Prefix;
        use super::super::decided_sites::boundaries::{Above, Boundary, End, FnAt, Part};
        let prefix = |prefix: &Prefix| match prefix {
            Prefix::Unstated => "-".to_string(),
            Prefix::Stated { values } => values.join("|"),
            Prefix::Unread { .. } => "?".to_string(),
        };
        let function = |at: &FnAt| format!("{}:{}", at.decl_file, at.decl_line);
        let boundary = |at: &Boundary| match at {
            Boundary::EntryParam { function: f, param } => {
                format!("entry {} p{param}", function(f))
            }
            Boundary::CallbackOption {
                host_at,
                arg,
                key,
                param,
                runs,
                runs_arg,
            } => format!(
                "callback {host_at} arg{arg}.{key} p{param} runs {} {runs_arg:?}",
                function(runs)
            ),
        };
        let parts = |parts: &[Part]| {
            parts
                .iter()
                .map(|part| match part {
                    Part::Edge { edge } => edge.clone(),
                    Part::Prefix { prefix: p } => format!("[{}]", prefix(p)),
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let above = |aboves: &[Above]| {
            aboves
                .iter()
                .map(|above| {
                    let end = match &above.end {
                        End::Top => "top".to_string(),
                        End::Boundary { boundary: at } => boundary(at),
                        End::Hole { reason } => reason.as_str().to_string(),
                        End::Handed => "handed".to_string(),
                        End::MountUnverified => "mount_unverified".to_string(),
                    };
                    format!("({end}) {}", parts(&above.parts))
                        .trim_end()
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join(" ; ")
        };
        let mut out = Vec::new();
        for route in &boundaries.routes {
            out.push(
                format!(
                    "route {}:{} {} {} <- {} @ {}",
                    route.file,
                    route.line,
                    route.method,
                    route.path,
                    parts(&route.below),
                    boundary(&route.at)
                )
                .replace("<-  @", "<- @"),
            );
        }
        for call_in in &boundaries.call_ins {
            out.push(format!(
                "call_in {} p{} at {}{} <- {}",
                function(&call_in.function),
                call_in.param,
                call_in.at,
                call_in
                    .host_prefix
                    .as_ref()
                    .map(|p| format!(" [{}]", prefix(p)))
                    .unwrap_or_default(),
                above(&call_in.above)
            ));
        }
        for hand_out in &boundaries.hand_outs {
            out.push(format!(
                "hand_out {} p{}.{} arg{} <- {}",
                function(&hand_out.function),
                hand_out.param,
                hand_out.key,
                hand_out.arg,
                above(&hand_out.instance)
            ));
        }
        out
    }

    /// carrick#2474 R5b: `receiver-scopes-callback` read across its two
    /// services. Every host registers on `start(app, deps)`'s own parameter,
    /// which nothing in the repository's services calls, so no route has a
    /// top to compose from: each ends `unmatched_boundary`, and none is
    /// written. A callback whose key a later computed key or spread may set
    /// again is not read as the function written there (the host's call-in
    /// ends `callback_option`, not at a boundary), and a callback in options
    /// built elsewhere is a callback parameter.
    #[test]
    fn the_callback_tree_resolves_across_its_services() {
        use super::super::decided_sites::ScopeLabel;
        use super::super::decided_sites::boundaries::{FAN_OUT, resolve};
        let mut facts = Vec::new();
        for service in ["routes", "server"] {
            let Some((on, exports, declarations)) = traced_fixture_in(
                "receiver-scopes-callback",
                &format!("packages/{service}"),
                service,
            ) else {
                return;
            };
            let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
            let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
            facts.push((service, decided.boundaries));
        }
        let services: Vec<(&str, &super::super::decided_sites::RouteBoundaries)> =
            facts.iter().map(|(name, facts)| (*name, facts)).collect();
        let resolved: Vec<String> = resolve(&services, FAN_OUT)
            .iter()
            .map(|resolved| {
                format!(
                    "{}:{} {} -> {} unread_hosts={} {}",
                    resolved.file,
                    resolved.line,
                    resolved.method,
                    resolved.paths.join("|"),
                    resolved.unread_hosts,
                    resolved.untraced.as_deref().unwrap_or("")
                )
                .replace("->  ", "-> ")
                .trim_end()
                .to_string()
            })
            .collect();
        let want: Vec<String> = texts(&[
            "packages/routes/src/built-routes.ts:3 GET -> unread_hosts=2 hole_callback_parameter",
            // A computed key after the written one.
            "packages/routes/src/computed-routes.ts:3 GET -> unread_hosts=1 hole_callback_option",
            "packages/routes/src/legacy.ts:3 GET -> unread_hosts=1 unmatched_boundary",
            "packages/routes/src/notes.ts:7 GET -> unread_hosts=1 unmatched_boundary",
            "packages/routes/src/orders.ts:3 GET -> unread_hosts=2 unmatched_boundary",
            "packages/routes/src/root-routes.ts:3 GET -> unread_hosts=1 unmatched_boundary",
            "packages/routes/src/shared.ts:5 GET -> unread_hosts=1 unmatched_boundary",
            "packages/routes/src/shared.ts:9 GET -> unread_hosts=1 unmatched_boundary",
            // A spread after the written key.
            "packages/routes/src/spread-routes.ts:3 GET -> unread_hosts=1 hole_callback_option",
            "packages/routes/src/tags.ts:3 GET -> unread_hosts=1 unmatched_boundary",
            "packages/routes/src/twice-routes.ts:3 GET -> unread_hosts=2 unmatched_boundary",
            "packages/server/src/extra.ts:7 GET -> unread_hosts=1 unmatched_boundary",
        ]);
        assert_eq!(resolved, want, "{resolved:#?}");
    }

    /// carrick#2474 R5a: with the receiver trace on, each service records
    /// where its route chains leave it, without reading another service's
    /// code. The receiving package's routes end at its plugin's parameter
    /// and at its admin helper's (nothing in its service calls either), and
    /// the plugin hands its scope to `opts.extra`. The hosts record each
    /// call into the package: through `register`, with the prefix
    /// `register`'s own slot states there, and the plain call of the admin
    /// helper from the callback the main host writes under `extra`, whose
    /// own route ends at that callback option. No host's `basePrefix` is
    /// read (2026-10-14 ruling 1). Read across the four services, the
    /// routes resolve by the facts alone.
    #[test]
    fn the_receiver_trace_records_boundaries_across_services() {
        use super::super::decided_sites::ScopeLabel;
        use super::super::decided_sites::boundaries::{FAN_OUT, FanOut, resolve};
        let mut facts = Vec::new();
        for service in ["routes", "server", "app", "demo"] {
            let Some((on, exports, declarations)) = traced_fixture_in(
                "receiver-scopes-across",
                &format!("packages/{service}"),
                service,
            ) else {
                return;
            };
            let scopes = ScopeLabel::from_exports(&exports, &declarations.scope_answers);
            let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
            facts.push((service, decided.boundaries));
        }
        let lines = |service: &str| {
            boundary_lines(
                &facts
                    .iter()
                    .find(|(name, _)| *name == service)
                    .expect("the service is read")
                    .1,
            )
        };
        let routes = lines("routes");
        let server = lines("server");
        let app = lines("app");
        let demo = lines("demo");
        // The receiving package: its admin helper and its plugin are entry
        // points; the plugin's own default prefix stands (nothing in its
        // service hands it a value), and it hands its scope to `extra`.
        let want_routes = texts(&[
            "route packages/routes/src/admin.ts:5 GET /admin/users <- @ entry packages/routes/src/admin.ts:4 p0",
            "route packages/routes/src/admin.ts:6 POST /admin/users <- @ entry packages/routes/src/admin.ts:4 p0",
            "route packages/routes/src/orders.ts:4 GET /orders <- packages/routes/src/plugin.ts:12 register /api/v1 [/api/v1] packages/routes/src/plugin.ts:14 register - [-] @ entry packages/routes/src/plugin.ts:11 p0",
            "hand_out packages/routes/src/plugin.ts:11 p1.extra arg0 <- (entry packages/routes/src/plugin.ts:11 p0) packages/routes/src/plugin.ts:12 register /api/v1 [/api/v1]",
        ]);
        assert_eq!(routes, want_routes, "{routes:#?}");
        // The main host: its own routes end at the callback option it
        // writes under `extra`, the plain call of the admin helper hands that
        // callback's scope in, and `register` runs the plugin on the root.
        // `register`'s own `prefix` slot is unstated: no branch of the
        // conditional spread sets it (carrick#2474 R5e).
        let want_server = texts(&[
            "route packages/server/src/audit.ts:6 GET /audit/events <- @ callback packages/server/src/server.ts:15 arg1.extra p0 runs packages/routes/src/plugin.ts:11 Some(0)",
            "route packages/server/src/audit.ts:7 GET /audit/events/:id <- @ callback packages/server/src/server.ts:15 arg1.extra p0 runs packages/routes/src/plugin.ts:11 Some(0)",
            "call_in packages/routes/src/admin.ts:4 p0 at packages/server/src/server.ts:19 <- (callback packages/server/src/server.ts:15 arg1.extra p0 runs packages/routes/src/plugin.ts:11 Some(0))",
            "call_in packages/routes/src/plugin.ts:11 p0 at packages/server/src/server.ts:15 [-] <- (top)",
        ]);
        assert_eq!(server, want_server, "{server:#?}");
        // A closed literal that writes no `prefix`: unstated.
        let want_app = texts(&[
            "call_in packages/routes/src/plugin.ts:11 p0 at packages/app/src/main.ts:8 [-] <- (top)",
        ]);
        assert_eq!(app, want_app, "{app:#?}");
        // A spread of a value the source does not state: unread.
        let want_demo = texts(&[
            "call_in packages/routes/src/plugin.ts:11 p0 at packages/demo/src/main.ts:8 [?] <- (top)",
        ]);
        assert_eq!(demo, want_demo, "{demo:#?}");
        // Ruling 1: no host's own prefix option crosses into a record.
        for line in routes.iter().chain(&server).chain(&app).chain(&demo) {
            assert!(
                !line.contains("${base}") && !line.contains("api/v1api"),
                "{line}"
            );
        }
        let services: Vec<(&str, &super::super::decided_sites::RouteBoundaries)> =
            facts.iter().map(|(name, facts)| (*name, facts)).collect();
        let resolved_under = |fan_out: FanOut| -> Vec<String> {
            resolve(&services, fan_out)
                .iter()
                .map(|resolved| {
                    format!(
                        "{}:{} {} -> {} unread_hosts={} {}",
                        resolved.file,
                        resolved.line,
                        resolved.method,
                        resolved.paths.join("|"),
                        resolved.unread_hosts,
                        resolved.untraced.as_deref().unwrap_or("")
                    )
                    .replace("->  ", "-> ")
                    .trim_end()
                    .to_string()
                })
                .collect()
        };
        assert_eq!(FAN_OUT, FanOut::StatedHosts);
        // The callback routes, filled only by the main host, compose under
        // the plugin's own scope from the facts alone. The orders route takes
        // the paths the first two hosts state; the third's prefix is not read
        // (Q5 (ii)). The package's own app serves it at its root as well.
        let resolved = resolved_under(FAN_OUT);
        let want_resolved = texts(&[
            "packages/routes/src/admin.ts:5 GET -> /api/v1/admin/users unread_hosts=0",
            "packages/routes/src/admin.ts:6 POST -> /api/v1/admin/users unread_hosts=0",
            "packages/routes/src/orders.ts:4 GET -> /api/v1/orders|/orders unread_hosts=1",
            "packages/server/src/audit.ts:6 GET -> /api/v1/audit/events unread_hosts=0",
            "packages/server/src/audit.ts:7 GET -> /api/v1/audit/events/:id unread_hosts=0",
        ]);
        assert_eq!(resolved, want_resolved, "{resolved:#?}");
        // Under Q5 (i) the orders route keeps today's reading.
        let every = resolved_under(FanOut::EveryHost);
        assert!(
            every.contains(
                &"packages/routes/src/orders.ts:4 GET -> unread_hosts=1 unread_host".to_string()
            ),
            "{every:#?}"
        );
    }

    /// Each route site's instance ends, in the order the walk gives them:
    /// `file:line <- at root|scope [call] package export receiver [prefix]` or
    /// `<- at <hole>`.
    fn ends_in_order(decided: &super::super::decided_sites::Decided) -> Vec<String> {
        use super::super::decided_sites::{InstanceAt, Prefix};
        let mut out = Vec::new();
        for site in decided
            .sites
            .iter()
            .filter(|site| site.end.op.as_deref() == Some("receive"))
        {
            for end in &site.instances {
                let what = match &end.end {
                    InstanceAt::Root {
                        package,
                        export,
                        receiver,
                        ..
                    } => format!("root {package} {export} {receiver}"),
                    InstanceAt::Scope {
                        file,
                        line,
                        package,
                        export,
                        receiver,
                        prefix,
                        ..
                    } => format!(
                        "scope {file}:{line} {package} {export} {receiver} {}",
                        match prefix {
                            Prefix::Stated { values } => values.join("|"),
                            Prefix::Unstated => "-".to_string(),
                            Prefix::Unread { .. } => "?".to_string(),
                        }
                    ),
                    InstanceAt::Handed {
                        package,
                        export,
                        side,
                        ..
                    } => {
                        format!("handed {package} {export} {side}")
                    }
                    InstanceAt::Hole { .. } => format!("{:?}", end.hole()),
                };
                out.push(format!("{}:{} <- {} {what}", site.file, site.line, end.at));
            }
        }
        out
    }

    /// carrick#2474 R2b, carrick#2636: a scope call on a server handed in
    /// by whatever runs the plugin (every end above it a hole) opens the
    /// scope of the one label the trace verified whose verdict names the
    /// member declaration the call resolves to, on the receiver it was
    /// verified on. Two exports of one package with a `register` each give
    /// one scope, the right export's, with the receiver its instances are
    /// read through (`instance:new` for a constructed one); a server whose
    /// types a package of their own declares (`type_packages`) opens its
    /// scope too. Declarations the verifier of this branch does not name
    /// yet come from the fixture's cassette (carrick#2636).
    #[test]
    fn a_scope_under_unknown_parents_opens_by_the_declaration_its_call_resolves_to() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-scopes-hosts")
        else {
            return;
        };
        let scopes = super::super::decided_sites::ScopeLabel::from_exports(
            &exports,
            &declarations.scope_answers,
        );
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let mut got: Vec<String> = ends_in_order(&decided)
            .into_iter()
            .filter(|end| end.contains("src/plugins.ts"))
            .collect();
        got.sort();
        assert_eq!(
            got,
            texts(&[
                "src/items.ts:4 <- src/items.ts:4 scope src/plugins.ts:11 fixture-twin-server default instance:() /app",
                "src/list.ts:4 <- src/list.ts:4 scope src/plugins.ts:16 fixture-twin-server Admin instance:new /admin",
                "src/things.ts:4 <- src/things.ts:4 scope src/plugins.ts:21 fixture-split-server default instance:() /hub",
            ]),
            "{:#?}",
            ends_in_order(&decided)
        );
    }

    /// carrick#2661: a package whose surface extends a server's types lists
    /// the server's type-only package too. Answered before the server, it
    /// must not take the server's declarations away from it: the scope its
    /// member's call resolves to is still asked, verified and opened.
    #[test]
    fn a_types_package_another_package_also_lists_keeps_its_scope() {
        let Some((on, exports, declarations)) =
            traced_fixture_edited("receiver-scopes-hosts", ".", "web", |exports| {
                let mut extension = exports
                    .iter()
                    .find(|export| export.package == "fixture-split-server")
                    .expect("the split server is answered")
                    .clone();
                extension.package = "fixture-split-extension".into();
                extension.specifier = "fixture-split-extension".into();
                extension.claims.clear();
                extension.type_packages = vec![
                    "fixture-split-types".into(),
                    "fixture-split-extension-types".into(),
                ];
                exports.insert(0, extension);
            })
        else {
            return;
        };
        let hub = |scope: &AskedScope| {
            scope.package == "fixture-split-server" && scope.receiver == "instance:()"
        };
        assert!(
            declarations.scope_answers.keys().any(hub),
            "{:#?}",
            declarations.scope_answers
        );
        let scopes = super::super::decided_sites::ScopeLabel::from_exports(
            &exports,
            &declarations.scope_answers,
        );
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let ends = ends_in_order(&decided);
        assert!(
            ends.iter().any(|end| end
                == "src/things.ts:4 <- src/things.ts:4 scope src/plugins.ts:21 fixture-split-server default instance:() /hub"),
            "{ends:#?}"
        );
    }

    /// carrick#2474 R2b: the library calls that run a function are walked
    /// in file and source order, whatever order the service's calls are
    /// indexed in: a chained call starts where the call it is made on
    /// does, so two decided runs give their ends in one order.
    #[test]
    fn chained_scope_calls_give_their_ends_in_one_order() {
        let Some((on, exports, declarations)) = traced_fixture_named("receiver-scopes-hosts")
        else {
            return;
        };
        let scopes = super::super::decided_sites::ScopeLabel::from_exports(
            &exports,
            &declarations.scope_answers,
        );
        let first = ends_in_order(&decided_traced(&on, &exports, &declarations, &scopes).1);
        assert!(
            first
                .iter()
                .any(|end| end.starts_with("src/chained.ts") && end.contains("/two")),
            "{first:#?}"
        );
        for _ in 0..16 {
            let again = ends_in_order(&decided_traced(&on, &exports, &declarations, &scopes).1);
            assert_eq!(again, first);
        }
    }

    /// carrick#2474 R1: the ask holds each export's maker on `export` and
    /// each op whose member a call row names, on every receiver its `of`
    /// or `on` applies to, whether or not a library site uses it.
    #[test]
    fn the_declaration_ask_names_each_called_op_on_its_receivers() {
        let Some(on) = walked("receiver-trace", ".", "web", &[], true) else {
            return;
        };
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/receiver-trace/library-claims.json"),
        )
        .unwrap();
        let exports = crate::library_store::parse_answer(&text).exports;
        let ask = declaration_ask(&on.inputs, &exports);
        let got: Vec<String> = ask
            .checks
            .iter()
            .map(|check| format!("{} {}", check.claim_id, check.receiver))
            .collect();
        assert_eq!(
            got,
            texts(&[
                "fixture-server@1:default:make:call:() export",
                "fixture-server@1:default:op:receive:get:of:instance:() instance:()",
                "fixture-server@1:default:op:receive:post:of:instance:() instance:()",
                "fixture-fetcher@1:default:make:call:() export",
                "fixture-fetcher@1:default:op:request:get:of:instance:() instance:()",
            ]),
            "{got:#?}"
        );
        assert_eq!(ask.ops.iter().filter(|op| op.is_some()).count(), 3);
    }

    /// carrick#2474 R2b: the verifier judges in request order under a time
    /// limit, so the scope checks the trace adds come after every op check
    /// R1 asks: near the limit, the scopes go unchecked, never an op.
    #[test]
    fn the_declaration_ask_puts_every_scope_check_after_the_op_checks() {
        let Some(on) = walked("receiver-scopes", ".", "web", &[], true) else {
            return;
        };
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/receiver-scopes/library-claims.json"),
        )
        .unwrap();
        let exports = crate::library_store::parse_answer(&text).exports;
        let ask = declaration_ask(&on.inputs, &exports);
        let last_op = ask.ops.iter().rposition(Option::is_some);
        let first_scope = ask.scopes.iter().position(Option::is_some);
        assert!(
            last_op.is_some() && first_scope.is_some(),
            "{:#?}",
            ask.checks
        );
        assert!(last_op < first_scope, "{:#?}", ask.checks);
    }

    /// The rows file of each service in `dir`, by the service the file's
    /// header names. A rows file this build cannot read (another format
    /// version) stops the census: it used to be skipped, and a census over
    /// no rows read 0 of 43 services' files without saying so (carrick#1666).
    fn positions_by_service(dir: &str) -> BTreeMap<String, PathBuf> {
        let mut by_service = BTreeMap::new();
        for dir_entry in std::fs::read_dir(dir).expect("the positions dir reads") {
            let path = dir_entry.expect("dir entry").path();
            if path.to_string_lossy().ends_with(".jsonl.gz") {
                let rows = crate::position_rows::read_rows_file(&path)
                    .unwrap_or_else(|why| panic!("{}: {why}", path.display()));
                by_service.insert(rows.header.service.clone(), path);
            }
        }
        by_service
    }

    /// A rows file of another format version stops the census instead of
    /// being skipped, and a readable one joins by the service its header
    /// names, not by its file name.
    #[test]
    fn the_census_stops_on_a_rows_file_it_cannot_read() {
        use std::io::Write;
        let write = |dir: &Path, name: &str, version: u32| {
            let mut gz = flate2::write::GzEncoder::new(
                std::fs::File::create(dir.join(name)).unwrap(),
                flate2::Compression::fast(),
            );
            writeln!(
                gz,
                "{{\"v\":{version},\"typescript\":\"6.0.2\",\"service\":\"api\",\"files\":0}}"
            )
            .unwrap();
        };
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "repo__api.jsonl.gz",
            crate::position_rows::FORMAT_VERSION,
        );
        let joined = positions_by_service(dir.path().to_str().unwrap());
        assert_eq!(joined.keys().collect::<Vec<_>>(), vec!["api"]);
        write(
            dir.path(),
            "repo__old.jsonl.gz",
            crate::position_rows::FORMAT_VERSION - 1,
        );
        let stopped = std::panic::catch_unwind(|| {
            positions_by_service(dir.path().to_str().unwrap());
        });
        assert!(stopped.is_err());
    }

    /// The offline census over flag-on rows: every site per repo, by role,
    /// with the stops, written as JSON.
    ///
    /// `CARRICK_DISCOVERY_CENSUS='<label>=<repo>=<positions dir>=<store
    /// answer>[;…]' [CARRICK_DISCOVERY_CENSUS_OUT=<file>] cargo test --lib
    /// discovery_census -- --ignored --nocapture`
    ///
    /// The sites are discovered with the receiver trace (carrick#2474 R1):
    /// the built sidecar verifies each service's declaration ask from the
    /// service's root, as a scan does, and every site names its receiver and
    /// how it was reached.
    #[test]
    #[ignore]
    fn discovery_census() {
        let Ok(spec) = std::env::var("CARRICK_DISCOVERY_CENSUS") else {
            return;
        };
        let script = Some(super::super::fixture_walk::sidecar().expect("the built sidecar"));
        let mut all: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        for entry in spec.split(';').filter(|entry| !entry.is_empty()) {
            let parts: Vec<&str> = entry.splitn(4, '=').collect();
            let [label, root, positions, answer] = parts.as_slice() else {
                panic!("<label>=<repo>=<positions dir>=<store answer>: {entry}");
            };
            let answer = crate::library_store::parse_answer(
                &std::fs::read_to_string(answer).expect("the store answer reads"),
            );
            let labels = Labels::of_answer(&answer);
            let root = std::fs::canonicalize(root).expect("the repo exists");
            let config = root.join("carrick.json");
            let services = if config.exists() {
                crate::config::Config::load_services(vec![config]).expect("carrick.json loads")
            } else {
                vec![crate::config::Config::default()]
            };
            let by_service = positions_by_service(positions);
            let mut sites: Vec<DiscoverySite> = Vec::new();
            let mut unfollowed: Vec<DiscoverySite> = Vec::new();
            let mut finished: Vec<(String, u32, u32)> = Vec::new();
            let mut missed_inside: Vec<(String, u32, u32)> = Vec::new();
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            let joined = services
                .iter()
                .filter(|service| {
                    by_service.contains_key(service.service_name.as_deref().unwrap_or("(root)"))
                })
                .count();
            crate::outln!(
                "== {label}: {joined} of {} service(s) joined a rows file",
                services.len()
            );
            // Every service's inputs are kept: each walk reads the others'
            // files with their own IR and rows (carrick#2618).
            let service_inputs: Vec<RequestSummaryInputs> = services
                .iter()
                .map(|service| {
                    let name = service
                        .service_name
                        .clone()
                        .unwrap_or_else(|| "(root)".to_string());
                    let index = match by_service.get(&name) {
                        Some(path) => crate::position_rows::PositionIndex::of(
                            &root,
                            crate::position_rows::read_rows_file(path).expect("rows read"),
                        ),
                        None => crate::position_rows::PositionIndex::missing(&root),
                    };
                    crate::engine::discover_service_inputs(&root, service, Some(index))
                })
                .collect();
            for (at, inputs) in service_inputs.iter().enumerate() {
                let siblings: Vec<&RequestSummaryInputs> = service_inputs
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != at)
                    .map(|(_, other)| other)
                    .collect();
                let service = &services[at];
                let declarations = script.as_ref().map(|script| {
                    super::super::fixture_walk::census_declarations(
                        script,
                        &root,
                        service,
                        inputs,
                        &answer.exports,
                    )
                });
                let traced;
                let labels = match &declarations {
                    Some(declarations) => {
                        traced = Labels {
                            declarations: Some(declarations),
                            ..Labels::of_answer(&answer)
                        };
                        &traced
                    }
                    None => &labels,
                };
                let discovery = discover_with(inputs, &siblings, labels, &root);
                for (key, count) in discovery.counts {
                    *counts.entry(key).or_default() += count;
                }
                sites.extend(discovery.sites);
                unfollowed.extend(discovery.unfollowed);
                finished.extend(discovery.finished);
                missed_inside.extend(discovery.missed_inside);
            }
            crate::outln!("== {label}: {} record(s)", sites.len());
            for (key, count) in &counts {
                crate::outln!("  {key}: {count}");
            }
            all.insert(
                label.to_string(),
                serde_json::json!({
                    "counts": counts,
                    "sites": sites,
                    "unfollowed": unfollowed,
                    "finished": finished,
                    "missed_inside": missed_inside,
                }),
            );
        }
        if let Ok(out) = std::env::var("CARRICK_DISCOVERY_CENSUS_OUT") {
            std::fs::write(&out, serde_json::to_string(&all).unwrap()).expect("the census writes");
        }
    }

    /// The library-store request each service of a repo sends with site
    /// questions on, as a scan builds it: the packages its library sites
    /// import, at their installed versions, and the default-library files
    /// the rows name. Offline, for answering the census from a store copy.
    ///
    /// `CARRICK_LIBRARY_ASK_CENSUS='<label>=<repo>=<positions dir>[;…]'
    /// CARRICK_LIBRARY_ASK_CENSUS_OUT=<file> cargo test --lib
    /// library_ask_census -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn library_ask_census() {
        let Ok(spec) = std::env::var("CARRICK_LIBRARY_ASK_CENSUS") else {
            return;
        };
        let mut all: BTreeMap<String, BTreeMap<String, serde_json::Value>> = BTreeMap::new();
        for entry in spec.split(';').filter(|entry| !entry.is_empty()) {
            let parts: Vec<&str> = entry.splitn(3, '=').collect();
            let [label, root, positions] = parts.as_slice() else {
                panic!("<label>=<repo>=<positions dir>: {entry}");
            };
            let root = std::fs::canonicalize(root).expect("the repo exists");
            let config = root.join("carrick.json");
            let services = if config.exists() {
                crate::config::Config::load_services(vec![config]).expect("carrick.json loads")
            } else {
                vec![crate::config::Config::default()]
            };
            let by_service = positions_by_service(positions);
            for service in &services {
                let name = service
                    .service_name
                    .clone()
                    .unwrap_or_else(|| "(root)".to_string());
                let index = match by_service.get(&name) {
                    Some(path) => crate::position_rows::PositionIndex::of(
                        &root,
                        crate::position_rows::read_rows_file(path).expect("rows read"),
                    ),
                    None => crate::position_rows::PositionIndex::missing(&root),
                };
                let inputs = crate::engine::discover_service_inputs(&root, service, Some(index));
                let service_root = root.join(service.directory.as_deref().unwrap_or("."));
                let specifiers: BTreeSet<String> = super::super::library_sites(&inputs)
                    .packages()
                    .into_values()
                    .flatten()
                    .collect();
                let asked = crate::library_store::request(
                    &specifiers,
                    &crate::library_store::Install {
                        service_root: &service_root,
                        repo_root: &root,
                        home: None,
                        yarn_registry_env: None,
                        npm_registry_env: None,
                        bun_registry_env: None,
                        xdg_config_home: None,
                    },
                );
                let library = inputs
                    .positions
                    .as_ref()
                    .and_then(|positions| positions.default_library_globals())
                    .map(|(typescript, globals)| {
                        crate::library_store::default_library_packages(typescript, globals)
                    })
                    .unwrap_or_default();
                let request = crate::library_store::Asked::with_default_library(asked, library)
                    .map(|asked| {
                        asked
                            .with_roles(&crate::library_store::NETWORK_ROLES)
                            .request
                    });
                all.entry(label.to_string())
                    .or_default()
                    .insert(name, serde_json::to_value(request).unwrap());
            }
        }
        let out = std::env::var("CARRICK_LIBRARY_ASK_CENSUS_OUT").expect("an out file");
        std::fs::write(out, serde_json::to_string_pretty(&all).unwrap()).expect("the asks write");
    }
}
