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
//! call that hands it over. A function handed to a library export is not:
//! until the store says which exports run what they are handed, none is
//! taken to.
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

use super::row_calls::Reach;
use super::values::ValueAt;
use super::*;
use crate::library_claims::{ExportClaims, applies, runtime_packages_of};
use crate::library_store::LibraryAnswer;
use crate::library_store::RUNTIME_TYPES;
use crate::position_rows::{
    FLAG_DECLARATION_UNNAMED, FLAG_MODULE_DEFAULT, FLAG_PUBLISHED, Kind, Row,
};
use crate::services::type_sidecar::{
    ClaimOn, LibraryCheck, LibraryClaim, LibraryOp, LibraryRole, MakeForm, SemanticsResult,
    SemanticsVerdict,
};

/// The stop for a walk that reaches a definition the extractor keys and
/// this pass read no body of.
const NO_BODY: &str = "no_body";

/// Why a call that entered no code was not followed ([`Discovery::unfollowed`]).
const VALUE_CALLEE: &str = "value_callee";
const OUTSIDE_SERVICE: &str = "outside_service";
const PACKAGE_UNMATCHED: &str = "package_unmatched";
const UNANSWERED_PACKAGE: &str = "unanswered_package";

/// Why a walk through a callee that is a value reached no one labelled
/// export (carrick#2432): its values disagree or one reaches none, it holds
/// more than [`MAX_VALUES`], nothing binds it, or each is a function written
/// at a caller (what runs it decides).
const SEVERAL_VALUES: &str = "several_values";
const VALUES_OVER_CAP: &str = "values_over_cap";
const VALUE_NEVER_BOUND: &str = "value_never_bound";
const INJECTED_CALLEE: &str = "injected_callee";

/// How many values a callee may hold before the walk stops reading them.
const MAX_VALUES: usize = 16;

/// The hop a walk records where it enters a function through a value.
use super::decided_sites::VALUE_HOP;

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
    pub stop: Option<String>,
    /// The binding the labelled call is made on (carrick#2474 R1). Only
    /// with the receiver trace on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<ReceiverAt>,
    /// How the labelled call was matched to its export. Only with the
    /// receiver trace on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reached: Option<ReachedBy>,
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
}

/// One function a walk entered.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChainHop {
    /// The definition key, or `<callback>` for a function written at the
    /// call and run by the callee.
    pub function: String,
    pub file: String,
    pub line: u32,
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
    pub counts: BTreeMap<String, usize>,
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
    /// The packages with an export of a network role.
    networked: HashSet<&'a str>,
    /// The exports the store labelled with no network role.
    none: HashSet<(&'a str, &'a str)>,
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
            answered: None,
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
        Labels::of_answered(&answer.exports, &answer.answered_packages)
    }

    /// [`Labels::of_answer`] from the exports and the packages answered, as
    /// the store's first answer to a scan holds them (carrick#2613).
    pub fn of_answered(exports: &'a [ExportClaims], answered: &'a BTreeSet<String>) -> Self {
        Labels {
            answered: Some(answered.iter().map(String::as_str).collect()),
            ..Labels::of(exports)
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
    fn package_reason(&self, key: &str) -> Option<&'static str> {
        let unanswered = |answered: &HashSet<&str>| {
            if is_language_key(key) {
                !answered.iter().any(|known| is_language_key(known))
            } else {
                !answered.contains(key)
            }
        };
        if !is_language_key(key) && self.networked.contains(key) {
            return Some(PACKAGE_UNMATCHED);
        }
        self.answered
            .as_ref()
            .is_some_and(unanswered)
            .then_some(UNANSWERED_PACKAGE)
    }

    /// [`Self::of`], with the receiver trace on.
    pub fn traced(exports: &'a [ExportClaims], declarations: &'a MemberDeclarations) -> Self {
        Labels {
            declarations: Some(declarations),
            ..Self::of(exports)
        }
    }

    fn get(&self, package: &str, export: &str) -> Option<&'a ExportClaims> {
        self.by_name.get(&(package, export)).copied()
    }

    /// The labelled global the language's own library declares as `name`,
    /// as the store keys one: under a default-library key (`lib.dom`,
    /// `lib:<file>`), whichever declaration file the compiler read it from.
    /// The first such key, in order, when several label it.
    fn global(&self, name: &str) -> Option<&'a ExportClaims> {
        self.by_name
            .iter()
            .filter(|((package, export), _)| *export == name && is_language_key(package))
            .min_by_key(|((package, _), _)| *package)
            .map(|(_, claims)| *claims)
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
/// specifier ([`crate::library_store::request`]).
fn specifier_keys(specifier: &str) -> Vec<String> {
    if specifier.starts_with("node:") {
        return vec![specifier.to_string()];
    }
    let package = super::library_sites::package_name(specifier);
    let mut keys = vec![package.to_string()];
    if !specifier.starts_with('.') && !specifier.contains('/') {
        keys.push(format!("node:{specifier}"));
    }
    keys
}

/// Each object the callee's member chain is read on, innermost first: its
/// span (discovery numbering) and whether it is a member access. An
/// optional member (`socket?.emit`) is read as a member. The walk stops at
/// anything but a name or a member access (`this`, a call).
pub(crate) fn receiver_spans(callee: &Expr) -> Vec<(u32, u32, bool)> {
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
            out.push((ident.span.lo.0, ident.span.hi.0, false));
            break;
        }
        at = member_of(object);
        if at.is_some() {
            let span = object.span();
            out.push((span.lo.0, span.hi.0, true));
        }
    }
    out
}

/// The binding a call's callee is read through: the callee itself, or the
/// name its member chain starts at.
fn root_name(call: &CallIr) -> Option<&str> {
    match call.receiver_spans.last() {
        None => Some(call.callee_text.as_str()),
        Some((_, _, false)) => call.callee_text.split(['.', '?']).next().map(str::trim),
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

/// The switch for the receiver trace (carrick#2474 R1): `1` only. Off, and
/// never set by the cloud runner, so with it off every scan is byte-identical
/// to one without it.
pub const RECEIVER_TRACE_SWITCH: &str = "CARRICK_RECEIVER_TRACE";

/// Whether the receiver trace is on in this process.
pub fn receiver_trace_enabled() -> bool {
    std::env::var(RECEIVER_TRACE_SWITCH).ok().as_deref() == Some("1")
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
}

/// One op a member declaration stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredOp {
    /// The store's key for the package, as [`Labels`] keys an export.
    pub package: String,
    pub export: String,
    pub member: String,
    pub op: LibraryOp,
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

/// The receiver id the instances a maker claim builds are read through, as
/// [`LibrarySite::receiver_ids`] names a site's.
fn make_receiver(form: MakeForm, member: Option<&str>) -> String {
    super::library_sites::maker_receiver_id(form == MakeForm::New, member)
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
            claim: claim.clone(),
        };
        let mut receivers = vec!["export".to_string()];
        for (id, claim) in &claims {
            if let LibraryClaim::Make { form, member, .. } = claim {
                receivers.push(make_receiver(*form, member.as_deref()));
                ask.checks.push(check(id, "export", claim));
                ask.ops.push(None);
                ask.scopes.push(None);
            }
        }
        // Every receiver a claim's `on` or `of` applies to.
        let on_receivers = |on: Option<ClaimOn>, of: Option<&str>| -> Vec<String> {
            let mut all = receivers.clone();
            all.extend(of.map(str::to_string));
            all.sort();
            all.dedup();
            all.retain(|receiver| applies(on, of, receiver));
            all
        };
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
    /// The scope claims verified, each by claim id on the receiver id it
    /// was asked on, with the member declarations (`decl_file`,
    /// `decl_line`) the verdict names (carrick#2474 R2b, carrick#2636):
    /// a call whose row resolves to one of them is a call of that scope
    /// member, whatever holds its receiver.
    pub verified_scopes: HashMap<AskedScope, Vec<(String, u32)>>,
    /// The checks asked.
    pub checks: usize,
    /// Of those, the ones the verifier did not reach in its time limit.
    pub not_reached: usize,
}

impl MemberDeclarations {
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
                out.verified_scopes.insert(scope.clone(), declarations);
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
    several: bool,
}

/// What a call or a body reaches.
#[derive(Debug, Clone, Default)]
struct Outcome<'a> {
    ends: Vec<Found>,
    /// The first stop, when the walk entered code and found nothing there.
    stop: Option<(Vec<ChainHop>, String)>,
    /// The shallowest definition still being read that the walk came back
    /// to: until it is done, this answer is not final.
    cycle_at: Option<usize>,
    /// Somewhere in the code the walk read, a call could not be followed
    /// (one of [`Discovery::unfollowed`]'s reasons): a walk that found no
    /// site has not read everything the call runs (carrick#2613).
    missed: bool,
    /// Each callee whose value a caller of the body decides (carrick#2432).
    open: Vec<Open<'a>>,
}

/// The call that invokes a value, where it is written.
#[derive(Debug, Clone, Copy)]
struct Invoke<'a> {
    file: &'a Path,
    call: &'a CallIr,
    construction: bool,
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
}

/// One value a callee may hold, read (carrick#2432).
#[derive(Debug, Clone)]
enum Held<'a> {
    /// A parameter of the body it is read in: how many functions out, its
    /// position and keys.
    Param(u8, usize, Vec<String>),
    /// A function written at a caller, or a parameter of a function written
    /// in the body: what runs it decides, which no caller here states.
    Callback,
    /// What the value reaches when the call invoking it runs it.
    Reached(Outcome<'a>),
}

impl<'a> Outcome<'a> {
    fn stopped(reason: impl Into<String>) -> Self {
        Outcome {
            stop: Some((Vec::new(), reason.into())),
            ..Outcome::default()
        }
    }

    /// The same, as seen from a call one function further up.
    fn under(mut self, hop: ChainHop) -> Self {
        for found in &mut self.ends {
            found.chain.insert(0, hop.clone());
        }
        if let Some((chain, _)) = &mut self.stop {
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
        }
        self.missed |= other.missed;
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

/// The compiler's `call` row for `call` in `file`, from `inputs` (which
/// holds `file`).
fn row_at<'a>(inputs: &'a RequestSummaryInputs, file: &Path, call: &CallIr) -> Option<&'a Row> {
    let positions = inputs.positions.as_ref()?;
    let ir = inputs.files.get(file)?;
    positions
        .lookup(
            file,
            ir.swc_start(call.site.lo),
            ir.swc_start(call.site.hi),
            Kind::Call,
        )
        .ok()
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
    /// Each function's callers, through what the rows say each call reaches:
    /// what fills a parameter a callee's value is read from (carrick#2432).
    callers: super::decided_sites::Calls<'a>,
    /// Each class of the service read through its constructions, with them
    /// (carrick#2452): what fills a field its constructor sets from a
    /// parameter.
    constructed: super::constructions::Constructed<'a>,
    memo: HashMap<(PathBuf, String), Outcome<'a>>,
    /// The definitions being read, by how deep.
    reading: HashMap<(PathBuf, String), usize>,
    /// Calls whose member declaration stands for more than one op
    /// (carrick#2474 R1): no site.
    ambiguous: std::cell::Cell<usize>,
    /// How many own factories' returns the value being read is inside.
    returns_depth: usize,
    /// The fields being read through their constructions (carrick#2452): a
    /// construction that passes a field of a class whose field is being
    /// read (`new Fork({ send: this.send })` in a method of `Fork`) adds
    /// nothing.
    fields_reading: HashSet<(&'a Path, u32, String)>,
}

impl<'a> Walker<'a> {
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
            })
            .unwrap_or_default();
        ChainHop {
            function: key.to_string(),
            file: self.rel(file),
            line,
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
        let op = (!invoke.construction)
            .then(|| {
                claims.claims.iter().find_map(|claim| match claim {
                    LibraryClaim::Op {
                        op,
                        member: claimed,
                        ..
                    } if claimed.as_deref() == member => Some(*op),
                    _ => None,
                })
            })
            .flatten();
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
        }
    }

    /// The call row at `call`, when there are rows to read.
    fn row(&self, file: &Path, call: &CallIr) -> Option<&Row> {
        row_at(self.inputs_for(file), file, call)
    }

    /// The binding `call` is made on (carrick#2474 R1): the row at the
    /// object its member is read on, `ident` for a name, `member` for a
    /// member access, `call` for a call's value, and what holds it. A name
    /// is a parameter when a function of its declaring file binds that name
    /// at the line the row names. `None` for a call with no receiver.
    fn receiver_of(&self, file: &Path, call: &CallIr) -> Option<ReceiverAt> {
        let (lo, hi, form) = call.receiver_object?;
        let (kind, row_kind) = match form {
            ObjectForm::Name => (ReceiverKind::Binding, Some(Kind::Ident)),
            ObjectForm::Member => (ReceiverKind::Field, Some(Kind::Member)),
            ObjectForm::Call => (ReceiverKind::CallResult, Some(Kind::Call)),
            ObjectForm::Other => (ReceiverKind::Other, None),
        };
        let row = row_kind.and_then(|row_kind| {
            let positions = self.inputs.positions.as_ref()?;
            let ir = self.inputs.files.get(file)?;
            positions
                .lookup(file, ir.swc_start(lo), ir.swc_start(hi), row_kind)
                .ok()
        });
        let parameter = form == ObjectForm::Name
            && row.is_some_and(|row| {
                if in_repo(row)
                    && let (Some(symbol), Some(decl_file), Some(line)) =
                        (row.symbol(), row.decl_file(), row.decl_line())
                    && let Some(positions) = self.inputs.positions.as_ref()
                    && let Some(declared) = self.inputs.files.get(&positions.path_of(decl_file))
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
    ///   one op is no site, and is counted.
    fn library_end(
        &self,
        file: &Path,
        call: &CallIr,
        construction: bool,
    ) -> Option<(SiteEnd, ReachedBy)> {
        let row = self.row(file, call)?;
        let symbol = row.symbol()?;
        if in_repo(row) {
            return None;
        }
        let invoke = Invoke {
            file,
            call,
            construction,
        };
        let end = |package: &str, export: &str, member: Option<&str>, claims: &ExportClaims| {
            self.end_at(invoke, package, export, member, claims)
        };
        let packages = store_keys(row, self.labels.exports);
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
                    let op = claims.claims.iter().find_map(|claim| match claim {
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
        if !packages.is_empty()
            && let Some(positions) = inputs.positions.as_ref()
        {
            for (lo, hi, member) in &call.receiver_spans {
                let kind = if *member { Kind::Member } else { Kind::Ident };
                let Ok(receiver) =
                    positions.lookup(file, ir.swc_start(*lo), ir.swc_start(*hi), kind)
                else {
                    break;
                };
                for name in [receiver.symbol(), receiver.type_text().and_then(type_root)]
                    .into_iter()
                    .flatten()
                {
                    for package in &packages {
                        if let Some(claims) = self.labels.get(package, name) {
                            return Some((
                                end(package, name, Some(symbol), claims),
                                ReachedBy::Name,
                            ));
                        }
                    }
                }
            }
        }
        // `app.get(…)` on an app handed in as a parameter, held in a field
        // or returned by a call: the member's declaration is one a verified
        // op claim resolves to (carrick#2474 R1).
        let declarations = self.labels.declarations.filter(|_| !construction)?;
        let ops = declarations.at(row.decl_file()?, row.decl_line()?);
        match ops {
            [] => None,
            [one] => {
                let claims = self.labels.get(&one.package, &one.export)?;
                Some((
                    SiteEnd {
                        op: Some(one.op),
                        ..end(&one.package, &one.export, Some(&one.member), claims)
                    },
                    ReachedBy::Declaration,
                ))
            }
            _ => {
                self.ambiguous.set(self.ambiguous.get() + 1);
                None
            }
        }
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
    fn unfollowed(&self, file: &Path, call: &CallIr, construction: bool) -> Option<&'static str> {
        match self.reach(file, call) {
            Reach::OutsideService(..) => Some(OUTSIDE_SERVICE),
            Reach::NoFunction => {
                let row = self.row(file, call)?;
                if in_repo(row) {
                    // A class built here has a constructor with no body to
                    // read; a value that is no definition is the gap.
                    return (!construction).then_some(VALUE_CALLEE);
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
                let reasons: Vec<Option<&'static str>> = keys
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

    /// What `call`, written in `body` of `file`, reaches.
    fn call(
        &mut self,
        file: &'a Path,
        body: &'a FnIr,
        call: &'a CallIr,
        construction: bool,
    ) -> Outcome<'a> {
        if let Some((end, reached)) = self.library_end(file, call, construction) {
            let traced = self.labels.declarations.is_some();
            return Outcome {
                ends: vec![Found {
                    chain: Vec::new(),
                    end,
                    receiver: traced.then(|| self.receiver_of(file, call)).flatten(),
                    reached: traced.then_some(reached),
                    several: false,
                }],
                ..Outcome::default()
            };
        }
        let invoke = Invoke {
            file,
            call,
            construction,
        };
        // A callee that is a value (carrick#2432): followed through each
        // value it may hold, before the interface member that types it. A
        // member the compiler declares in a package or the language's
        // library (`res.json()` on a parameter) is that library's
        // ([`Self::declared_here`]).
        if !call.callee_values.is_empty()
            && !construction
            && !self.unbound_fields(file, call)
            && self.declared_here(file, call)
        {
            let mut values = Vec::new();
            for value in &call.callee_values {
                values.extend(self.read_value(file, value, invoke, false));
            }
            return self.agree(values, invoke, Vec::new());
        }
        if call.invokes_param.is_some() {
            return Outcome::stopped(INJECTED_CALLEE);
        }
        match self.reach(file, call) {
            Reach::Function(target, key) => {
                let hop = self.hop(&target, &key);
                let id = (target, key);
                let callee = self.function(&id.0, &id.1);
                let mut out = match self.reading.get(&id) {
                    // The walk came back to a definition it is reading.
                    Some(depth) => Outcome {
                        cycle_at: Some(*depth),
                        ..Outcome::stopped("cycle")
                    },
                    None => {
                        let inner = self.definition(&id);
                        self.bind(inner, invoke, callee).under(hop.clone())
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
                for (index, arg) in call.args.iter().enumerate() {
                    if let Value::Callback(nested) = arg
                        && runs.contains(&index)
                        && let Some(callback) = body.nested.get(*nested)
                    {
                        let written = ChainHop {
                            function: "<callback>".to_string(),
                            file: self.rel(file),
                            line: callback
                                .site
                                .as_ref()
                                .map_or(call.site.line, |site| site.line),
                        };
                        let inner = self.body(file, callback);
                        let inner = self.lifted(inner).under(written).under(hop.clone());
                        out.absorb(inner);
                    }
                }
                if !out.ends.is_empty() {
                    out.stop = None;
                }
                out
            }
            Reach::OutsideService(..) => Outcome::default(),
            Reach::NoFunction if self.interface_member(file, call) => {
                Outcome::stopped("interface_member")
            }
            Reach::NoFunction => Outcome::default(),
            Reach::Unknown(reason) => Outcome::stopped(super::row_calls::hole_key(reason)),
        }
    }

    /// What the definition `id` reaches, read once it is final.
    fn definition(&mut self, id: &(PathBuf, String)) -> Outcome<'a> {
        if let Some(known) = self.memo.get(id) {
            return known.clone();
        }
        // A definition the extractor keys whose body this pass did not read
        // (an object's method, a function written inside another): what it
        // calls is not known, so the walk stops there and says so.
        let Some((file, ir)) = self.function(&id.0, &id.1) else {
            return Outcome::stopped(NO_BODY);
        };
        if ir.bodyless {
            return Outcome::stopped("interface_member");
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
                    .is_some_and(|(_, reason)| reason == "cycle");
            if !cycle_stop {
                self.memo.insert(id.clone(), out.clone());
            }
        }
        out
    }

    /// What the calls `body` makes reach.
    fn body(&mut self, file: &'a Path, body: &'a FnIr) -> Outcome<'a> {
        let mut out = Outcome::default();
        for (call, construction) in body.read_calls() {
            let mut inner = self.call(file, body, call, construction);
            if self.misses(file, body, call, construction, &inner) {
                inner.missed = true;
            }
            out.absorb(inner);
        }
        if !out.ends.is_empty() {
            out.stop = None;
        }
        out
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
            let read = self.body(file, callback);
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
                |end: Option<SiteEnd>, chain: Vec<ChainHop>, stop: Option<String>| DiscoverySite {
                    file: self.rel(file),
                    line: call.site.line,
                    span_start: call.site.span_start,
                    span_end: call.site.span_end,
                    callee: call.callee_text.clone(),
                    end,
                    chain,
                    stop,
                    receiver: None,
                    reached: None,
                };
            if outcome.ends.is_empty() {
                let Some((chain, stop)) = outcome.stop else {
                    match self.unfollowed(file, call, construction) {
                        Some(reason) => {
                            *discovery
                                .counts
                                .entry(format!("unfollowed/{reason}"))
                                .or_default() += 1;
                            discovery.unfollowed.push(record(
                                None,
                                Vec::new(),
                                Some(reason.to_string()),
                            ));
                        }
                        None => {
                            *discovery.counts.entry("no_site".to_string()).or_default() += 1;
                            let at = (self.rel(file), call.site.span_start, call.site.span_end);
                            if missed {
                                discovery.missed_inside.push(at);
                            } else {
                                discovery.finished.push(at);
                            }
                        }
                    }
                    continue;
                };
                // A call whose own row says nothing: the walk never entered
                // code. Listed with the hole the row names.
                if chain.is_empty()
                    && ![
                        INJECTED_CALLEE,
                        "interface_member",
                        SEVERAL_VALUES,
                        VALUES_OVER_CAP,
                        VALUE_NEVER_BOUND,
                    ]
                    .contains(&stop.as_str())
                {
                    *discovery
                        .counts
                        .entry(format!("unread/{stop}"))
                        .or_default() += 1;
                    discovery.unfollowed.push(record(None, chain, Some(stop)));
                    continue;
                }
                *discovery.counts.entry(format!("stop/{stop}")).or_default() += 1;
                discovery.sites.push(record(None, chain, Some(stop)));
                continue;
            }
            let mut sibling_routes = 0;
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
                    continue;
                }
                // One of the labelled exports a callee's disagreeing values
                // reach: the end is kept, with why it is no site.
                let stop = found.several.then(|| SEVERAL_VALUES.to_string());
                let count = match &stop {
                    Some(stop) => format!("stop/{stop}"),
                    None => format!("site/{}", role_key(found.end.role)),
                };
                *discovery.counts.entry(count).or_default() += 1;
                discovery.sites.push(DiscoverySite {
                    receiver: found.receiver,
                    reached: found.reached,
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
                    discovery.missed_inside.push(at);
                }
            }
        }
        for inner in body.nested.iter().chain(&body.detached) {
            self.sites(file, key, depth.saturating_add(1), inner, discovery);
        }
    }

    /// The definition `key` of `file`, the file as `inputs` keys it.
    fn function(&self, file: &Path, key: &str) -> Option<(&'a Path, &'a FnIr)> {
        let (path, ir) = self.inputs_for(file).files.get_key_value(file)?;
        Some((path.as_path(), ir.functions.get(key)?))
    }

    /// The function written in `file` whose node starts at `start`, at any
    /// depth of any body.
    fn written_function(&self, file: &Path, start: u32) -> Option<&'a FnIr> {
        fn find(ir: &FnIr, start: u32) -> Option<&FnIr> {
            if ir.site.as_ref().is_some_and(|site| site.lo == start) {
                return Some(ir);
            }
            ir.nested
                .iter()
                .chain(&ir.detached)
                .find_map(|inner| find(inner, start))
        }
        let ir = self.inputs_for(file).files.get(file)?;
        let mut keys: Vec<&String> = ir.functions.keys().collect();
        keys.sort();
        keys.into_iter()
            .map(|key| &ir.functions[key])
            .chain(std::iter::once(&ir.module_level))
            .find_map(|body| find(body, start))
    }

    /// What `value`, read in a body of `file`, reaches when `invoke` runs it
    /// (carrick#2432): a parameter of that body is left for its callers; a
    /// labelled export is the labelled call `invoke` itself; a function of
    /// the repository is read as a call of it is, `invoke` filling its
    /// parameters. `at_caller` reads a function written in place as a
    /// callback, whatever runs it filling it.
    fn read_value(
        &mut self,
        file: &'a Path,
        value: &ValueAt,
        invoke: Invoke<'a>,
        at_caller: bool,
    ) -> Vec<Held<'a>> {
        let nothing = || vec![Held::Reached(Outcome::default())];
        match value {
            ValueAt::Param { depth, index, keys } => {
                vec![Held::Param(*depth, *index, keys.clone())]
            }
            ValueAt::Field {
                lo,
                hi,
                class,
                field,
            } => match self.field_values(file, *class, field, invoke) {
                Some(held) => held,
                None => {
                    let read = ValueAt::Read {
                        lo: *lo,
                        hi: *hi,
                        member: true,
                    };
                    self.read_value(file, &read, invoke, at_caller)
                }
            },
            ValueAt::Read { lo, hi, member } => {
                let kind = if *member { Kind::Member } else { Kind::Ident };
                if let Some(end) = self.value_end(file, *lo, *hi, kind, invoke) {
                    return vec![Held::Reached(Outcome {
                        ends: vec![Found {
                            chain: Vec::new(),
                            end,
                            receiver: None,
                            reached: None,
                            several: false,
                        }],
                        ..Outcome::default()
                    })];
                }
                match self.inputs_for(file).row_names(file, *lo, *hi, kind) {
                    Reach::Function(target, key) => {
                        vec![Held::Reached(self.enter_value(target, key, invoke))]
                    }
                    _ => nothing(),
                }
            }
            ValueAt::Returned { lo, hi, member } => {
                // `f.bind(this)` is `f`: the member the compiler's row names
                // as the language library's `bind` returns its object.
                if let Some(call) = member
                    && call.args == 1
                    && self.is_library_bind(file, call.lo, call.hi)
                {
                    let mut out = Vec::new();
                    for object in &call.object {
                        out.extend(self.read_value(file, object, invoke, at_caller));
                    }
                    if !out.is_empty() {
                        return out;
                    }
                }
                let Reach::Function(target, key) = self.inputs_for(file).reach(file, *lo, *hi)
                else {
                    return nothing();
                };
                let Some((factory_file, factory)) = self.function(&target, &key) else {
                    return nothing();
                };
                if self.returns_depth >= MAX_VALUES {
                    return nothing();
                }
                self.returns_depth += 1;
                let mut out = Vec::new();
                for returned in &factory.returned_values {
                    match returned {
                        // A parameter of the factory: its own call fills it.
                        ValueAt::Param { .. } => out.extend(nothing()),
                        other => out.extend(self.read_value(factory_file, other, invoke, false)),
                    }
                }
                self.returns_depth -= 1;
                if out.is_empty() { nothing() } else { out }
            }
            ValueAt::Function(_) if at_caller => vec![Held::Callback],
            ValueAt::Function(start) => match self.written_function(file, *start) {
                Some(function) => {
                    let line = function.site.as_ref().map_or(0, |site| site.line);
                    let hop = ChainHop {
                        function: VALUE_HOP.to_string(),
                        file: self.rel(file),
                        line,
                    };
                    let inner = self.body(file, function);
                    vec![Held::Reached(
                        self.bind(inner, invoke, Some((file, function))).under(hop),
                    )]
                }
                None => nothing(),
            },
            ValueAt::Object { .. } | ValueAt::Unread => nothing(),
        }
    }

    /// Whether the member read at `lo..hi` in `file` is a `bind` the
    /// language's own library declares: the compiler's row there names a
    /// declaration in a `lib:` file.
    fn is_library_bind(&self, file: &Path, lo: u32, hi: u32) -> bool {
        let inputs = self.inputs_for(file);
        let (Some(positions), Some(ir)) = (inputs.positions.as_ref(), inputs.files.get(file))
        else {
            return false;
        };
        positions
            .lookup(file, ir.swc_start(lo), ir.swc_start(hi), Kind::Member)
            .ok()
            .is_some_and(|row| {
                row.symbol() == Some("bind")
                    && row
                        .decl_file()
                        .is_some_and(|declared| declared.starts_with("lib:"))
            })
    }

    /// The labelled call `invoke` is when the name or member read at
    /// `lo..hi` in `file` holds a labelled export: by the name its package
    /// publishes the declaration the compiler's row there names, or a
    /// global of the language's own library or the runtime's types.
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
        None
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
                ..Outcome::stopped("cycle")
            };
        }
        let inner = self.definition(&id);
        let callee = self.function(&id.0, &id.1);
        self.bind(inner, invoke, callee).under(hop)
    }

    /// The constructions of the class whose span starts at `class` in
    /// `file`, and where its constructor sets `field` from, when the class
    /// is read through its constructions (carrick#2452). A class no file of
    /// the service constructs, and that nothing refused, comes back with no
    /// constructions: its constructor alone is read (carrick#2808).
    fn field_origin(
        &self,
        file: &'a Path,
        class: u32,
        field: &str,
    ) -> Option<(
        &'a super::constructions::ClassOrigin,
        &'a super::constructions::FieldOrigin,
        Vec<super::decided_sites::Placed<'a>>,
    )> {
        let built = match self.constructed.built.get(&(file, class)) {
            Some(built) => built.clone(),
            None if self.constructed.refused.contains(&(file, class)) => return None,
            None => Vec::new(),
        };
        let origin = self.inputs.files.get(file)?.class_origins.get(&class)?;
        let from = origin.fields.get(field)?;
        Some((origin, from, built))
    }

    /// What `field` of the class whose span starts at `class` in `file` may
    /// hold when `invoke` runs it (carrick#2452): each value the constructor
    /// sets it from, a parameter read at every construction as a call's
    /// argument is, and from the construction's own callers where it hands
    /// its own parameter on. `None` when the class is not read through its
    /// constructions: the member is read as itself.
    fn field_values(
        &mut self,
        file: &'a Path,
        class: u32,
        field: &str,
        invoke: Invoke<'a>,
    ) -> Option<Vec<Held<'a>>> {
        let (origin, from, built) = self.field_origin(file, class, field)?;
        let reading = (file, class, field.to_string());
        if !self.fields_reading.insert(reading.clone()) {
            return Some(Vec::new());
        }
        let defaults: Defaults<'a> = (file, origin.defaults.as_slice());
        let mut out = Vec::new();
        for value in &from.values {
            match value {
                ValueAt::Param {
                    depth: 0,
                    index,
                    keys,
                } => {
                    // No construction in the service (carrick#2808): the
                    // parameter holds its default, as a function nothing
                    // calls does.
                    if built.is_empty() {
                        out.extend(self.passed(None, *index, keys, Some(defaults), invoke));
                    }
                    for placed in &built {
                        let at = Invoke {
                            file: placed.file,
                            call: placed.call,
                            construction: true,
                        };
                        for held in self.passed(Some(at), *index, keys, Some(defaults), invoke) {
                            match held {
                                Held::Param(out_depth, index, keys) => {
                                    out.extend(self.callers_pass(
                                        (placed.file, placed.key, placed.depth),
                                        (out_depth, index, &keys),
                                        invoke,
                                        &mut Vec::new(),
                                    ));
                                }
                                other => out.push(other),
                            }
                        }
                    }
                }
                // A parameter of a function the constructor is written in.
                ValueAt::Param { .. } => out.push(Held::Reached(Outcome::default())),
                other => out.extend(self.read_value(file, other, invoke, false)),
            }
        }
        self.fields_reading.remove(&reading);
        Some(out)
    }

    /// Whether every value `call`'s callee may hold is a field whose class
    /// is not read through its constructions: the callee is read as itself,
    /// as a member is.
    fn unbound_fields(&self, file: &'a Path, call: &CallIr) -> bool {
        call.callee_values.iter().all(|value| {
            matches!(value, ValueAt::Field { class, field, .. }
                if self.field_origin(file, *class, field).is_none())
        })
    }

    /// `inner`, what a function `invoke` calls reaches, with each value its
    /// own parameters hold filled from what `invoke` passes there, or the
    /// default `callee` gives. What is left open is a parameter of the body
    /// that holds `invoke`.
    fn bind(
        &mut self,
        mut inner: Outcome<'a>,
        invoke: Invoke<'a>,
        callee: Option<(&'a Path, &'a FnIr)>,
    ) -> Outcome<'a> {
        for open in std::mem::take(&mut inner.open) {
            let mut values = open.known;
            for (depth, index, keys) in open.refs {
                if depth != 0 {
                    // A parameter of the function the callee is written in:
                    // the callee's own caller does not fill it.
                    values.push(Held::Reached(Outcome::default()));
                    continue;
                }
                values.extend(self.passed(
                    Some(invoke),
                    index,
                    &keys,
                    defaults_of(callee),
                    open.invoke,
                ));
            }
            let settled = self.agree(values, open.invoke, open.chain);
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
                let settled = self.agree(open.known, open.invoke, open.chain);
                inner.absorb(settled);
            } else {
                open.refs = refs;
                inner.open.push(open);
            }
        }
        inner
    }

    /// What the call `at` hands its callee's parameter `index` at `keys`:
    /// each value it writes there, read where `at` is written, or the
    /// callee's default where it writes nothing (or there is no call, `at`
    /// `None`). Empty when nothing is handed and there is no default.
    /// `defaults` are the callee's parameters' defaults, read in its file
    /// (a function's [`FnIr::param_defaults`], or a constructor's).
    fn passed(
        &mut self,
        at: Option<Invoke<'a>>,
        index: usize,
        keys: &[String],
        defaults: Option<Defaults<'a>>,
        invoke: Invoke<'a>,
    ) -> Vec<Held<'a>> {
        let mut written: Vec<ValueAt> = Vec::new();
        let mut defaulted = at.is_none();
        if let Some(at) = at {
            let args = &at.call.arg_values;
            // A spread (an empty entry) at or before the position moves it.
            let spread = args.iter().take(index + 1).any(Vec::is_empty);
            match args.get(index) {
                _ if spread => written.push(ValueAt::Unread),
                Some(values) => {
                    for value in values {
                        match value.at(keys) {
                            Some(values) => written.extend(values),
                            None => defaulted = true,
                        }
                    }
                }
                None => defaulted = true,
            }
        }
        written.dedup();
        let mut out = Vec::new();
        if let Some(at) = at {
            for value in &written {
                out.extend(self.read_value(at.file, value, invoke, true));
            }
        }
        if defaulted && let Some((file, defaults)) = defaults {
            for (position, path, values) in defaults {
                if *position != index || !keys.starts_with(path) {
                    continue;
                }
                for value in values {
                    for value in value.at(&keys[path.len()..]).unwrap_or_default() {
                        match value {
                            // A default read from another parameter.
                            ValueAt::Param { .. } => out.push(Held::Reached(Outcome::default())),
                            other => out.extend(self.read_value(file, &other, invoke, false)),
                        }
                    }
                }
            }
        }
        out
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
                }],
                ..Outcome::default()
            };
        }
        if known.is_empty() {
            return Outcome::stopped(VALUE_NEVER_BOUND).below(&chain);
        }
        if known.len() > MAX_VALUES {
            return Outcome::stopped(VALUES_OVER_CAP).below(&chain);
        }
        if known.iter().all(|value| matches!(value, Held::Callback)) {
            return Outcome::stopped(INJECTED_CALLEE).below(&chain);
        }
        let mut outcomes: Vec<Outcome<'a>> = known
            .into_iter()
            .map(|value| match value {
                Held::Reached(outcome) => outcome,
                _ => Outcome::default(),
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
                    found.several = true;
                }
                outcome.stop = None;
            }
            out.absorb(outcome);
        }
        if !agree && out.ends.is_empty() {
            out.stop = Some((Vec::new(), SEVERAL_VALUES.to_string()));
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
        for open in std::mem::take(&mut outcome.open) {
            let mut values = open.known;
            for (out, index, keys) in &open.refs {
                values.extend(self.callers_pass(
                    (file, key, depth),
                    (*out, *index, keys),
                    open.invoke,
                    &mut Vec::new(),
                ));
            }
            let settled = self.agree(values, open.invoke, open.chain);
            outcome.absorb(settled);
        }
        if !outcome.ends.is_empty() {
            outcome.stop = None;
        }
        outcome
    }

    /// The values the parameter `param` (how many functions out of the body
    /// `at`, its position and keys) holds, read at every caller of the
    /// definition that body is in, and from theirs where a caller passes a
    /// parameter of its own. `passing` holds the definitions on the way: a
    /// cycle ends.
    fn callers_pass(
        &mut self,
        at: (&'a Path, Option<&'a str>, u8),
        param: (u8, usize, &[String]),
        invoke: Invoke<'a>,
        passing: &mut Vec<(PathBuf, String)>,
    ) -> Vec<Held<'a>> {
        let (file, key, depth) = at;
        let (out, index, keys) = param;
        // A parameter of a function written in the body: what runs it
        // fills it.
        if out < depth {
            return vec![Held::Callback];
        }
        let Some(key) = key.filter(|_| out == depth) else {
            return vec![Held::Reached(Outcome::default())];
        };
        let id = (file.to_path_buf(), key.to_string());
        if passing.contains(&id) {
            return Vec::new();
        }
        passing.push(id);
        let callee = defaults_of(self.function(file, key));
        let callers: Vec<_> = self.callers.callers_of(file, key).to_vec();
        let mut values = Vec::new();
        if callers.is_empty() {
            // Nothing calls it: what it holds is its default, if any.
            values.extend(self.passed(None, index, keys, callee, invoke));
        }
        for caller in callers {
            let at = Invoke {
                file: caller.file,
                call: caller.call,
                construction: false,
            };
            for value in self.passed(Some(at), index, keys, callee, invoke) {
                match value {
                    Held::Param(out, index, keys) => values.extend(self.callers_pass(
                        (caller.file, caller.key, caller.depth),
                        (out, index, &keys),
                        invoke,
                        passing,
                    )),
                    other => values.push(other),
                }
            }
        }
        passing.pop();
        values
    }
}

/// A callee's parameters' defaults, with the file they are read in.
type Defaults<'a> = (&'a Path, &'a [(usize, Vec<String>, Vec<ValueAt>)]);

fn defaults_of<'a>(callee: Option<(&'a Path, &'a FnIr)>) -> Option<Defaults<'a>> {
    callee.map(|(file, callee)| (file, callee.param_defaults.as_slice()))
}

fn role_key(role: LibraryRole) -> String {
    serde_json::to_value(role)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
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
    let mut walker = Walker {
        inputs,
        siblings,
        labels,
        root,
        instances,
        callers: super::decided_sites::Calls::through_reach(inputs),
        constructed: super::constructions::constructed(inputs),
        memo: HashMap::new(),
        reading: HashMap::new(),
        ambiguous: std::cell::Cell::new(0),
        returns_depth: 0,
        fields_reading: HashSet::new(),
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
                .all(|site| site.stop.as_deref() != Some(UNANSWERED_PACKAGE))
        );
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
                ("lib.dom", "fetch")
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
                ("lib.dom", "fetch")
            );
        }
        for line in [13] {
            let [site] = at("events.ts", line)[..] else {
                panic!(
                    "a site at events.ts:{line}: {:?} {:?}",
                    discovery.sites, discovery.unfollowed
                );
            };
            let end = site.end.as_ref().expect("the call reaches publish");
            assert_eq!(
                (end.package.as_str(), end.export.as_str()),
                ("fixture-bus", "publish")
            );
        }
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
        assert_eq!(local.stop.as_deref(), Some("several_values"));
        assert_eq!(
            local.end.as_ref().map(|end| end.export.as_str()),
            Some("fetch")
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
                    site.stop.as_deref().unwrap_or("-")
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
            r#"{"v":4,"typescript":"5.9.3","service":"api","files":1}"#,
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
            texts(&["2 fetch -> lib.dom fetch -"])
        );
    }

    /// carrick#2431: a walk that reaches a definition whose body this pass
    /// did not read stops there and says so; an optional call is a call.
    #[test]
    fn an_unread_body_is_a_stop_and_an_optional_call_is_a_site() {
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
                "3 fetch -> lib.dom fetch -",
                "5 loadInner stop no_body",
                "10 fetch -> lib.dom fetch -",
                "15 table.load stop no_body",
            ])
        );
        assert_eq!(
            records_in(&discovery, "services/web/src/maybe.ts"),
            texts(&["4 socket?.send -> fixture-socket Socket send"])
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
                &format!("lib.dom fetch {http}")
            ])
        );
        assert_eq!(
            chain_of(&with, 9),
            texts(&[
                &format!("postTo {http}"),
                &format!("send {http}"),
                &format!("lib.dom fetch {http}")
            ])
        );
        assert_eq!(
            chain_of(&with, 14),
            texts(&[
                &format!("ItemsClient.remove {http}"),
                &format!("send {http}"),
                &format!("lib.dom fetch {http}")
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
            .map(|site| (site.line, site.stop.as_deref()))
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
            records_in(&own, http).contains(&"4 fetch -> lib.dom fetch -".to_string()),
            "{:?}",
            records_in(&own, http)
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
        let on = walked(name, dir, service, &[], true)?;
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
            &Labels::traced(&exports, &declarations),
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
            &Labels::traced(&exports, &declarations),
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
            super::super::site_record::site_records(&on.inputs, &on.repo, &decided, &labels);
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
            &Labels::traced(&exports, &declarations),
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
    /// package declares (`outside_service`), and a field (`unread`).
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
            "src/routes.ts:11 <- src/routes.ts:11 unread",
            "src/users.ts:8 <- src/app.ts:16 root src/app.ts:12",
        ]);
        assert_eq!(got, want, "{got:#?}");
        // Without the makers, no site names an instance; with them, every
        // record keeps its state byte for byte, so no site's sha moves.
        let (untraced, records) = records_of(&on, &exports, &discovery);
        assert!(untraced.sites.iter().all(|site| site.instances.is_empty()));
        let traced =
            super::super::site_record::site_records(&on.inputs, &on.repo, &decided, &labels);
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
                    line = format!("{line} via {}", composed.edges.join(" > "));
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
        let discovery = discover(&on.inputs, &Labels::traced(exports, declarations), &on.repo);
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
                &declarations.verified_scopes,
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
        let scopes = ScopeLabel::from_exports(&exports, &declarations.verified_scopes);
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        let got = composed_lines(&decided, false);
        let want = texts(&["src/index.ts:20 <- /tree", "src/index.ts:8 <- /health"]);
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
        let scopes = ScopeLabel::from_exports(&exports, &declarations.verified_scopes);
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
            super::super::site_record::site_records(&on.inputs, &on.repo, &decided, &labels);
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
            &declarations.verified_scopes,
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
            &declarations.verified_scopes,
        );
        let (_, decided) = decided_traced(&on, &exports, &declarations, &scopes);
        Some(composed_lines(&decided, true))
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
            "src/items.ts:3 <- ? (entry_point) via src/unread.ts:5 register ? > src/unread.ts:7 register -",
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
                        End::Hole { reason } => reason.clone(),
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
            let scopes = ScopeLabel::from_exports(&exports, &declarations.verified_scopes);
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
            let scopes = ScopeLabel::from_exports(&exports, &declarations.verified_scopes);
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
            &declarations.verified_scopes,
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
            declarations.verified_scopes.keys().any(hub),
            "{:#?}",
            declarations.verified_scopes
        );
        let scopes = super::super::decided_sites::ScopeLabel::from_exports(
            &exports,
            &declarations.verified_scopes,
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
            &declarations.verified_scopes,
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

    /// The offline census over flag-on rows: every site per repo, by role,
    /// with the stops, written as JSON.
    ///
    /// `CARRICK_DISCOVERY_CENSUS='<label>=<repo>=<positions dir>=<store
    /// answer>[;…]' [CARRICK_DISCOVERY_CENSUS_OUT=<file>] cargo test --lib
    /// discovery_census -- --ignored --nocapture`
    ///
    /// With `CARRICK_RECEIVER_TRACE=1` the sites are discovered with the
    /// receiver trace (carrick#2474 R1): the built sidecar verifies each
    /// service's declaration ask from the service's root, as a scan does,
    /// and every site names its receiver and how it was reached.
    #[test]
    #[ignore]
    fn discovery_census() {
        let Ok(spec) = std::env::var("CARRICK_DISCOVERY_CENSUS") else {
            return;
        };
        let script = receiver_trace_enabled()
            .then(|| super::super::fixture_walk::sidecar().expect("the built sidecar"));
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
            let mut by_service: BTreeMap<String, PathBuf> = BTreeMap::new();
            for dir_entry in std::fs::read_dir(positions).expect("the positions dir reads") {
                let path = dir_entry.expect("dir entry").path();
                if path.to_string_lossy().ends_with(".jsonl.gz")
                    && let Ok(rows) = crate::position_rows::read_rows_file(&path)
                {
                    by_service.insert(rows.header.service.clone(), path);
                }
            }
            let mut sites: Vec<DiscoverySite> = Vec::new();
            let mut unfollowed: Vec<DiscoverySite> = Vec::new();
            let mut finished: Vec<(String, u32, u32)> = Vec::new();
            let mut missed_inside: Vec<(String, u32, u32)> = Vec::new();
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
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
            let mut by_service: BTreeMap<String, PathBuf> = BTreeMap::new();
            for dir_entry in std::fs::read_dir(positions).expect("the positions dir reads") {
                let path = dir_entry.expect("dir entry").path();
                if path.to_string_lossy().ends_with(".jsonl.gz")
                    && let Ok(rows) = crate::position_rows::read_rows_file(&path)
                {
                    by_service.insert(rows.header.service.clone(), path);
                }
            }
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
                            .with_roles(crate::library_store::roles_for(true))
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
