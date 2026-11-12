//! The instance a decided site's labelled call is made on, per deciding
//! caller (carrick#2474 R2a). Record-only, with the receiver trace on.
//!
//! The receiver is read as the body that writes the labelled call reads it
//! ([`super::super::InstanceIr`]): a parameter, a key of one, or an
//! enclosing function's, is filled at each call above it exactly as the
//! name slot is ([`super::filled`], [`super::to_definition`]): first down
//! the decided site's own chain, then up every caller of the function that
//! still holds it ([`super::Calls::callers_of`]). There is no agreement
//! rule: each caller that hands an instance in gives its own end, so a
//! helper handed two apps serves both. A definition walked once is not
//! walked again for the same site, and one reached again while its own
//! callers are being walked ends there, whatever value it is handed (a
//! function that calls itself with a longer one, carrick#2748): that
//! ends every cycle, with no hop limit.
//!
//! Each end is one of:
//!
//! - **a root**: the result of a maker the store labels (`make()`,
//!   `new Server()`), at the maker call's `file:line`, read where the
//!   value resolver's lowering holds it ([`super::super::ValueAt`],
//!   carrick#3504 S3): a binding the library-site reader holds an instance
//!   in, a maker or own factory call (followed into what the factory
//!   returns), a local declared once (read through to its value), or a name
//!   whose declaration the compiler names;
//! - **a scope** a labelled member opens (carrick#2474 R2b): the parameter
//!   of a function a member labelled with `runs` and `yields` runs with a
//!   scoped instance (`app.register(plugin, { prefix })`), under the prefix
//!   its `name` slot states, whose own instance is the member's receiver,
//!   walked the same way ([`InstanceAt::Scope`]);
//! - **a handed instance** (carrick#1670): the parameter of a function a
//!   socket library's own call runs (`hub.on("connection", (peer) => …)`),
//!   which that call's export hands in on the side the store states for it
//!   ([`InstanceAt::Handed`]); [`origin_side`] folds it, and a root, into the
//!   side of a `by_origin` socket;
//! - **a hole**, with its reason ([`InstanceHole`]).
//!
//! A receiver that is a field of a class (`this.peer`) is the constructor
//! parameter it is set from when nothing else may set it (carrick#1670):
//! the instance is what each `new` of the class hands in there, walked as a
//! call hands in a parameter ([`InstanceWalk::field`]).
//!
//! An instance handed to a member labelled with `mounts` is served under
//! that member's prefix: those edges are read once per service and joined
//! into each site's composed path ([`super::composed`]).

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::super::constructions::Constructed;
use super::super::{
    CallIr, ClientRef, FoldPiece, Holds, KeyState, Link, MadeBy, MakerForm, Piece,
    RequestSummaryInputs, Root, Value, ValueAt, Way,
};
use super::boundaries::{Boundary, FnAt};
use super::{Calls, Placed, filled, holds_parameter, relative, to_definition};
use crate::address;
use crate::position_rows::{FLAG_ANY, FLAG_DECLARATION_UNNAMED, Kind};

/// A maker the store labels: the package (as the store keys it), the
/// export, the member (`None`: the export itself) and whether it is
/// constructed, and the slot of its call that holds the name its instance
/// is made with, when the claim states one (read for an op whose name is
/// bound to its maker, carrick#3527).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MakerLabel {
    pub package: String,
    pub export: String,
    pub member: Option<String>,
    pub new: bool,
    pub name: Option<super::ArgSlot>,
}

impl MakerLabel {
    /// Every maker the settled claims label.
    pub fn from_exports(exports: &[crate::library_claims::ExportClaims]) -> Vec<MakerLabel> {
        use crate::services::type_sidecar::{LibraryClaim, MakeForm};
        let mut makers = Vec::new();
        for export in exports {
            for claim in &export.claims {
                if let LibraryClaim::Make {
                    form, member, name, ..
                } = claim
                {
                    let maker = MakerLabel {
                        package: export.package.clone(),
                        export: export.export.clone(),
                        member: member.clone(),
                        new: *form == MakeForm::New,
                        name: name.as_ref().map(|slot| super::ArgSlot {
                            arg: slot.arg as usize,
                            key: slot.key.clone(),
                        }),
                    };
                    if !makers.contains(&maker) {
                        makers.push(maker);
                    }
                }
            }
        }
        makers
    }
}

/// What a scope member binds (carrick#2474 R0's fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeBinds {
    /// It runs the function at `arg` (`key`) and hands it the scoped
    /// instance at its parameter `param`.
    Runs {
        arg: usize,
        key: Option<String>,
        param: usize,
    },
    /// The instance at `arg` (`key`) is served under the prefix. With
    /// `handle`, the slot takes a handler (K1): what is handed there serves
    /// only where it is itself an instance a label reads (a pass-through's
    /// receiver, a maker's root); anything else is plain middleware.
    Mounts {
        arg: usize,
        key: Option<String>,
        handle: bool,
    },
    /// It returns the scoped instance.
    Returns,
    /// Its value is its receiver itself, served at the receiver's own
    /// address (K1: `router.routes()`).
    Passes,
}

impl ScopeBinds {
    /// Whether `other` binds the same slot the same way, a handle twin
    /// counting as its instance-rule claim: both read one argument, and one
    /// edge stands for both (K1).
    fn same_as(&self, other: &ScopeBinds) -> bool {
        match (self, other) {
            (
                ScopeBinds::Mounts { arg, key, .. },
                ScopeBinds::Mounts {
                    arg: other_arg,
                    key: other_key,
                    ..
                },
            ) => arg == other_arg && key == other_key,
            _ => self == other,
        }
    }
}

/// A scope member the store labels (carrick#2474 R2b): the package (as the
/// store keys it), the export, the member, the receivers the trace's own
/// request verified the claim on, the slot its prefix sits in, and what it
/// binds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeLabel {
    pub package: String,
    pub export: String,
    pub member: String,
    /// Each receiver id the verifier verified the claim on, with the member
    /// declarations (`decl_file`, `decl_line`) its verdict names
    /// (carrick#2636).
    verified_on: Vec<(String, Vec<(String, u32)>)>,
    /// The slot the prefix sits in; `None` for a scope that binds no prefix
    /// (K1: a name-less mount or `runs`, served at its parent's address,
    /// and a pass-through).
    pub name: Option<(usize, Option<String>)>,
    pub binds: ScopeBinds,
}

impl ScopeLabel {
    /// Every scope claim of `exports` whose member is one path hop off the
    /// receiver, on each receiver the trace's own request verified it on
    /// ([`super::super::discovery::declaration_ask`]), by claim id: a claim
    /// verified on no receiver is no label, whatever its siblings on the
    /// same member were verified as.
    pub fn from_exports(
        exports: &[crate::library_claims::ExportClaims],
        verified: &HashMap<super::super::discovery::AskedScope, Vec<(String, u32)>>,
    ) -> Vec<ScopeLabel> {
        use crate::services::type_sidecar::LibraryClaim;
        let mut labels = Vec::new();
        for export in exports {
            for (claim_id, claim) in export.claims_with_scopes_by_id() {
                let LibraryClaim::Scope {
                    member,
                    name,
                    path,
                    runs,
                    yields,
                    mounts,
                    collects,
                    merges,
                    passes,
                    ..
                } = claim
                else {
                    continue;
                };
                if !path.is_empty() {
                    continue;
                }
                let binds = match (runs, yields, mounts, *passes) {
                    (Some(runs), Some(yields), None, false) => ScopeBinds::Runs {
                        arg: runs.arg as usize,
                        key: runs.key.clone(),
                        param: yields.param as usize,
                    },
                    (None, None, Some(mounts), false) => ScopeBinds::Mounts {
                        arg: mounts.arg as usize,
                        key: mounts.key.clone(),
                        handle: mounts.handle,
                    },
                    (None, None, None, false) => ScopeBinds::Returns,
                    (None, None, None, true) if name.is_none() => ScopeBinds::Passes,
                    _ => continue,
                };
                // A procedure router's member has no name (#2474 R6a): it
                // binds no prefix, and R6c reads its collection. A name-less
                // scope is a label only where it binds or passes (K1): a
                // name-less `returns` would open a scope on what a procedure
                // router's member returns.
                if name.is_none()
                    && (collects.is_some() || merges.is_some() || binds == ScopeBinds::Returns)
                {
                    continue;
                }
                let mut verified_on: Vec<(String, Vec<(String, u32)>)> = verified
                    .iter()
                    .filter(|(asked, _)| {
                        asked.package == export.package
                            && asked.export == export.export
                            && asked.claim_id == claim_id
                    })
                    .map(|(asked, declarations)| (asked.receiver.clone(), declarations.clone()))
                    .collect();
                if verified_on.is_empty() {
                    continue;
                }
                verified_on.sort();
                let label = ScopeLabel {
                    package: export.package.clone(),
                    export: export.export.clone(),
                    member: member.clone(),
                    verified_on,
                    name: name
                        .as_ref()
                        .map(|name| (name.arg as usize, name.key.clone())),
                    binds,
                };
                if !labels.contains(&label) {
                    labels.push(label);
                }
            }
        }
        labels
    }

    /// Whether the claim applies to an instance of `package`'s `export`
    /// read through `receiver`: it was verified there (the trace asks a
    /// claim only on the receivers its `on` or `of` applies to).
    fn applies(&self, package: &str, export: &str, receiver: &str) -> bool {
        self.package == package
            && self.export == export
            && self.verified_on.iter().any(|(at, _)| at == receiver)
    }

    /// The receiver ids the claim was verified on whose verdict names the
    /// member declaration `declared`.
    fn verified_at<'s>(&'s self, declared: &'s (String, u32)) -> impl Iterator<Item = &'s str> {
        self.verified_on
            .iter()
            .filter(move |(_, declarations)| declarations.contains(declared))
            .map(|(receiver, _)| receiver.as_str())
    }

    /// Whether `other` opens the same scope at a call: the same member, the
    /// prefix in the same slot, and the same binding (carrick#3185), a
    /// handle twin counting as its instance-rule claim (K1).
    pub(super) fn binds_as(&self, other: &ScopeLabel) -> bool {
        self.member == other.member && self.name == other.name && self.binds.same_as(&other.binds)
    }

    /// Whether `other` is this claim or its handle twin (K1): the same
    /// export of the same package, binding the same way. One mount edge
    /// stands for both.
    pub(super) fn twin_of(&self, other: &ScopeLabel) -> bool {
        self.package == other.package && self.export == other.export && self.binds_as(other)
    }

    /// Whether this label and `other` may both apply at one call where
    /// only the member declaration says which apply (K1): they bind the
    /// same way, or both mount and read different slots (a path-first and
    /// a path-less `use`), so the slot that holds an instance decides.
    fn agrees_with(&self, other: &ScopeLabel) -> bool {
        if self.binds_as(other) {
            return true;
        }
        match (&self.binds, &other.binds) {
            (
                ScopeBinds::Mounts { arg, key, .. },
                ScopeBinds::Mounts {
                    arg: other_arg,
                    key: other_key,
                    ..
                },
            ) => self.member == other.member && (arg, key) != (other_arg, other_key),
            _ => false,
        }
    }

    /// Whether the member returns the scoped instance and was verified on
    /// the receiver `receiver` of `package`'s `export`: a call of it on an
    /// instance read through `receiver` returns the object an op claimed on
    /// `<receiver>>scope:<member>` is called on (carrick#1678).
    pub(super) fn returns_on(&self, package: &str, export: &str, receiver: &str) -> bool {
        self.binds == ScopeBinds::Returns && self.applies(package, export, receiver)
    }

    /// Whether the claim applies to the instance `end` ends at.
    pub(super) fn applies_to(&self, end: &InstanceEnd) -> bool {
        match &end.end {
            InstanceAt::Root {
                package,
                export,
                receiver,
                ..
            }
            | InstanceAt::Scope {
                package,
                export,
                receiver,
                ..
            } => self.applies(package, export, receiver),
            InstanceAt::Hole { .. } | InstanceAt::Handed { .. } => false,
        }
    }
}

/// The prefix a scope or mount call states in its label's `name` slot
/// (carrick#2474 R2b).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "prefix", rename_all = "snake_case")]
pub enum Prefix {
    /// The call writes nothing there: it binds no prefix.
    Unstated,
    /// Every value the call states. All are recorded (#2474 Q2).
    Stated { values: Vec<String> },
    /// A value this pass does not read as text, as written where one
    /// expression is.
    Unread {
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
}

/// Why an instance walk ends at no maker. It has no spelling of its own: it
/// serialises through [`crate::request_summary::Stop`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InstanceHole {
    /// The function that holds the receiver is called by nothing in the
    /// service, and handed to no library call.
    EntryPoint,
    /// The receiver is a parameter of a function written as an argument
    /// of a call this service makes to its own code, or one written
    /// anywhere else: what fills it is whatever runs it.
    CallbackParameter,
    /// The value handed in is typed `any`.
    UntypedReceiver,
    /// The value handed in is declared outside the service's files: a
    /// package, or a file the scan does not read.
    OutsideService,
    /// A library call runs the function that holds the receiver (handed
    /// to it as an argument, or written as one), and no scope label that
    /// applies to its receiver runs it there.
    RunsByLibrary,
    /// The function that holds the receiver is written under a key of an
    /// object literal handed to a call (`{ extra: (app) => … }`): what calls
    /// it is the function that reads that key, which waits on
    /// carrick#2432's injected-callee step.
    CallbackOption,
    /// A value the walk does not read: a field, a call's value that is no
    /// maker's and no own factory's, a maker the store does not label, a
    /// conditional.
    Unread,
}

/// Where one instance walk ends.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "end", rename_all = "snake_case")]
pub enum InstanceAt {
    /// A labelled maker's result, at the maker call.
    Root {
        file: String,
        line: u32,
        package: String,
        export: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        member: Option<String>,
        /// The receiver id its instances are read through
        /// (`instance:()`, `instance:new`).
        receiver: String,
        /// How the maker call is written and what it was handed: where a
        /// client's base is read when the library-site reader holds no
        /// instance at the site (carrick#3488). `None` where the walk did
        /// not read the maker call. Never part of a record's state.
        #[serde(skip)]
        made: Option<MakerCall>,
    },
    /// A scope a labelled member opens (carrick#2474 R2b), at the member's
    /// call: the scoped instance is of the same export as the member's
    /// receiver, read through the same receiver id, and served under
    /// `prefix` below each of `parents`, the member's receiver's ends.
    Scope {
        file: String,
        line: u32,
        member: String,
        #[serde(flatten)]
        prefix: Prefix,
        /// What the prefix slot states as an address part's value
        /// ([`InstanceWalk::value_of`]).
        #[serde(skip)]
        value: address::Value,
        package: String,
        export: String,
        receiver: String,
        parents: Vec<InstanceEnd>,
    },
    /// A function a socket library's call runs, at that call: the parameter
    /// that holds the instance is one the call's export hands in, on `side`
    /// of the connection (carrick#1670). The store labels the call (`export`
    /// is the instance handed in when the op's `yields` names it, else the
    /// call's own export), so the instance has no maker in this service.
    Handed {
        file: String,
        line: u32,
        package: String,
        export: String,
        side: String,
    },
    Hole {
        reason: InstanceHole,
        /// What the walk stopped at, as written, where one expression is.
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// For an entry point or a callback option, where the chain leaves
        /// the service, as another service's facts name it (carrick#2474
        /// R5a).
        #[serde(skip_serializing_if = "Option::is_none")]
        boundary: Option<Boundary>,
    },
}

/// A labelled maker call a walk ended at: its form and its arguments, as a
/// library claim's slot reads them (carrick#3488).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MakerCall {
    pub form: MakerForm,
    pub args: Vec<super::super::SiteArg>,
    /// The maker call's span, in the file the root names: where a name
    /// bound to the maker is read (carrick#3527).
    pub span: (u32, u32),
}

/// One instance end of a decided site: the call that hands the instance
/// in (`file:line`, the deciding caller), or where the walk stopped, and
/// what it ends at.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct InstanceEnd {
    pub at: String,
    #[serde(flatten)]
    pub end: InstanceAt,
}

impl InstanceEnd {
    /// The hole reason, or `None` for a root, a scope or a handed instance.
    pub fn hole(&self) -> Option<InstanceHole> {
        match &self.end {
            InstanceAt::Hole { reason, .. } => Some(*reason),
            InstanceAt::Root { .. } | InstanceAt::Scope { .. } | InstanceAt::Handed { .. } => None,
        }
    }
}

/// A library call handed a function, with the argument it is handed at.
type Host<'a> = (Placed<'a>, usize);

/// What every site's instance walk of one service reads, built once per
/// service (carrick#2474 R2b): the library calls that may run a function,
/// and the receiver ends of each scope call already walked.
pub(super) struct WalkIndex<'a> {
    /// By file and function name: the library calls written in that file
    /// handed the function by name (`server.register(plugin)`).
    by_name: HashMap<(PathBuf, String), Vec<Host<'a>>>,
    /// By the declaration (`decl_file`, `decl_line`) the compiler resolves
    /// an argument to: the calls of a `runs` scope member handed it
    /// (`app.register(items)`, `items` imported).
    by_declaration: HashMap<(String, u32), Vec<Host<'a>>>,
    /// The socket calls the store labels, by file and span: what a function
    /// one of them runs is handed in (carrick#1670).
    socket_hosts: HashMap<HostSpan, HostLabel>,
    /// The ends of a scope call's own receiver, by the scope calls being
    /// walked around it (outermost first) and the call: a walk reached
    /// again from another site reads them here.
    parents: RefCell<HashMap<ParentsKey, Vec<InstanceEnd>>>,
    /// The service's own inputs and the constructions in them, read when a
    /// field first needs them ([`InstanceWalk::field`]).
    inputs: &'a RequestSummaryInputs,
    constructed: OnceCell<Constructed<'a>>,
}

/// A call by its file and span.
type HostSpan = (PathBuf, u32, u32);

/// A scope call, with the scope calls walked around it.
type ParentsKey = (Vec<HostSpan>, HostSpan);

/// A socket call the store labels, read for what it hands the function it
/// runs (carrick#1670).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostLabel {
    pub package: String,
    pub export: String,
    /// The side the store states for the call's export; `None` for a
    /// `by_origin` export.
    pub side: Option<String>,
    /// What the op's handler is handed, when the store states it.
    pub yields: Option<crate::services::type_sidecar::Yields>,
}

impl HostLabel {
    /// Every discovered call of a socket export the store labels, by the
    /// file (joined to `root`) and span of the labelled call.
    pub(super) fn of_discovered(
        root: &Path,
        discovered: &[super::DiscoverySite],
        labels: &[super::SiteLabel],
    ) -> HashMap<HostSpan, HostLabel> {
        let mut hosts = HashMap::new();
        for site in discovered {
            let Some(end) = site.end.as_ref().filter(|_| site.stop.is_none()) else {
                continue;
            };
            let Some(label) = super::SiteLabel::find(labels, end).filter(|l| l.role == "socket")
            else {
                continue;
            };
            hosts.insert(
                (root.join(&end.file), end.span_start, end.span_end),
                HostLabel {
                    package: label.package.clone(),
                    export: label.export.clone(),
                    side: label.side.clone(),
                    yields: label.yields.clone(),
                },
            );
        }
        hosts
    }
}

impl<'a> WalkIndex<'a> {
    pub(super) fn of(
        calls: &Calls<'a>,
        inputs: &'a RequestSummaryInputs,
        scopes: &[ScopeLabel],
        socket_hosts: HashMap<HostSpan, HostLabel>,
    ) -> Self {
        let mut index = WalkIndex {
            by_name: HashMap::new(),
            by_declaration: HashMap::new(),
            socket_hosts,
            parents: RefCell::new(HashMap::new()),
            inputs,
            constructed: OnceCell::new(),
        };
        let runs: HashSet<&str> = scopes
            .iter()
            .filter(|label| matches!(label.binds, ScopeBinds::Runs { .. }))
            .map(|label| label.member.as_str())
            .collect();
        for placed in calls.by_span.values() {
            // Read off the syntax first: the compiler's row is looked up
            // only for a call handed a function by name, or a `runs`
            // member's.
            let names: Vec<(usize, &str)> = {
                let mut names: Vec<(usize, &str)> = Vec::new();
                for (arg, keys, name) in &placed.call.handed {
                    if keys.is_empty() && !names.iter().any(|(_, known)| known == name) {
                        names.push((*arg, name.as_str()));
                    }
                }
                names
            };
            let runs_member = runs.contains(placed.call.callee.as_str());
            if names.is_empty() && !runs_member {
                continue;
            }
            if !library_call(inputs, placed.file, placed.call) {
                continue;
            }
            for (arg, name) in names {
                index
                    .by_name
                    .entry((placed.file.to_path_buf(), name.to_string()))
                    .or_default()
                    .push((*placed, arg));
            }
            if !runs_member {
                continue;
            }
            let (Some(positions), Some(ir)) =
                (inputs.positions.as_ref(), inputs.files.get(placed.file))
            else {
                continue;
            };
            for (arg, held) in placed.call.instance.args.iter().enumerate() {
                let Some((lo, hi)) = held.span else {
                    continue;
                };
                let Ok(row) =
                    positions.lookup(placed.file, ir.swc_start(lo), ir.swc_start(hi), Kind::Ident)
                else {
                    continue;
                };
                if let (Some(decl_file), Some(decl_line)) = (row.decl_file(), row.decl_line()) {
                    index
                        .by_declaration
                        .entry((decl_file.to_string(), decl_line))
                        .or_default()
                        .push((*placed, arg));
                }
            }
        }
        for hosts in index
            .by_name
            .values_mut()
            .chain(index.by_declaration.values_mut())
        {
            sort_hosts(hosts);
        }
        index
    }
}

impl<'a> WalkIndex<'a> {
    /// The calls of a `runs` scope member handed a name the compiler
    /// resolves to the declaration `(decl_file, decl_line)`, by that
    /// declaration (carrick#2474 R5a).
    /// Every construction of the class whose span starts at `class` in
    /// `file`, when the class is read through its constructions
    /// ([`super::super::constructions::constructed`]).
    fn constructions<'s>(&'s self, file: &Path, class: u32) -> Option<&'s [Placed<'a>]> {
        self.constructed
            .get_or_init(|| super::super::constructions::constructed(self.inputs))
            .built
            .iter()
            .find(|((built_in, at), _)| *built_in == file && *at == class)
            .map(|(_, built)| built.as_slice())
    }

    pub(super) fn runs_hosts(&self) -> impl Iterator<Item = (&(String, u32), &[Host<'a>])> {
        self.by_declaration
            .iter()
            .map(|(declared, hosts)| (declared, hosts.as_slice()))
    }
}

/// `hosts` in file and source order: by file, span start, span end and
/// argument, since a chained call starts where the call it is made on does
/// (`app.register(a).register(b)`).
fn sort_hosts(hosts: &mut [Host<'_>]) {
    hosts.sort_by(|a, b| {
        (
            a.0.file,
            a.0.call.site.span_start,
            a.0.call.site.span_end,
            a.1,
        )
            .cmp(&(
                b.0.file,
                b.0.call.site.span_start,
                b.0.call.site.span_end,
                b.1,
            ))
    });
}

/// One site's instance walk.
pub(super) struct InstanceWalk<'w, 'a> {
    pub(super) calls: &'w Calls<'a>,
    pub(super) inputs: &'w RequestSummaryInputs,
    pub(super) root: &'w Path,
    pub(super) makers: &'w [MakerLabel],
    pub(super) scopes: &'w [ScopeLabel],
    index: &'w WalkIndex<'a>,
    /// The scope calls whose receiver is being walked, outermost first: a
    /// scope call reached again inside its own walk ends it (a plugin that
    /// registers itself).
    hosts: Vec<(PathBuf, u32, u32)>,
    ends: Vec<InstanceEnd>,
    /// The definitions walked, with the receiver as their own body reads
    /// it: a definition reached again adds nothing (a cycle, or a second
    /// path to it).
    walked: HashSet<(PathBuf, String, Vec<Piece>)>,
    /// The definitions whose callers are being walked, outermost first: one
    /// reached again inside its own walk ends it, as the site walk's `path`
    /// does, even where the value it is handed has grown (carrick#2748).
    passing: Vec<(PathBuf, String)>,
    /// The own factories being followed, so a factory that returns its own
    /// call ends. Two calls of one factory each give their end.
    factories: HashSet<(PathBuf, String)>,
    /// The fields being followed to their constructions, by file, class and
    /// field: one reached again inside its own walk ends it (a field passed
    /// back into its own class).
    fielding: Vec<(PathBuf, u32, String)>,
}

impl<'w, 'a> InstanceWalk<'w, 'a> {
    pub(super) fn new(
        calls: &'w Calls<'a>,
        inputs: &'w RequestSummaryInputs,
        root: &'w Path,
        makers: &'w [MakerLabel],
        scopes: &'w [ScopeLabel],
        index: &'w WalkIndex<'a>,
    ) -> Self {
        InstanceWalk {
            calls,
            inputs,
            root,
            makers,
            scopes,
            index,
            hosts: Vec::new(),
            ends: Vec::new(),
            walked: HashSet::new(),
            passing: Vec::new(),
            factories: HashSet::new(),
            fielding: Vec::new(),
        }
    }

    fn at(&self, placed: &Placed<'_>) -> String {
        format!(
            "{}:{}",
            relative(self.root, placed.file),
            placed.call.site.line
        )
    }

    fn push(&mut self, at: String, end: InstanceAt) {
        let end = InstanceEnd { at, end };
        if !self.ends.contains(&end) {
            self.ends.push(end);
        }
    }

    fn hole(&mut self, at: String, reason: InstanceHole, text: Option<String>) {
        self.push(
            at,
            InstanceAt::Hole {
                reason,
                text,
                boundary: None,
            },
        );
    }

    /// A hole where the chain leaves the service at `boundary`, when the
    /// walk can name it (carrick#2474 R5a).
    fn boundary_hole(&mut self, at: String, reason: InstanceHole, boundary: Option<Boundary>) {
        self.push(
            at,
            InstanceAt::Hole {
                reason,
                text: None,
                boundary,
            },
        );
    }

    /// The declaration of the function `key` of `file`, as the rows name
    /// it: the file repo-relative and the line its declaration starts on.
    pub(super) fn fn_at(&self, file: &Path, key: &str) -> Option<FnAt> {
        let (line, _) = self.inputs.files.get(file)?.declared_at.get(key)?;
        Some(FnAt {
            decl_file: relative(self.root, file),
            decl_line: *line,
        })
    }

    /// The instance ends of the site whose calls, from the deciding call
    /// (first) to the labelled call (last), are `down`. Empty when the
    /// labelled call is made on no receiver the syntax names (a bare
    /// call).
    pub(super) fn of(self, down: &[Placed<'a>]) -> Vec<InstanceEnd> {
        let last = down[down.len() - 1];
        let instance = &last.call.instance;
        self.of_value(down, instance.receiver_value.as_ref(), &instance.receiver)
    }

    /// The instance ends of the site whose calls are `down`, when its
    /// labelled call is made on the result of the scope call `scope`
    /// (carrick#1678): the instance is the scope call's receiver's, which
    /// `scope` reads in the body of the labelled call's function.
    pub(super) fn of_scoped(self, down: &[Placed<'a>], scope: &Placed<'a>) -> Vec<InstanceEnd> {
        let instance = &scope.call.instance;
        self.of_value(down, instance.receiver_value.as_ref(), &instance.receiver)
    }

    /// The instance ends of what `placed`'s call is handed at `arg`
    /// (`key`): what a mount member serves (carrick#2474 R2b).
    pub(super) fn of_arg(
        self,
        placed: Placed<'a>,
        arg: usize,
        key: Option<&str>,
    ) -> Vec<InstanceEnd> {
        let value = placed.call.args.get(arg);
        let held = placed.call.instance.args.get(arg);
        let (value, held) = match key {
            None => (value, held.map(|held| held.whole.as_slice())),
            Some(key) => (
                match value {
                    Some(Value::Obj(object)) => object.fields.get(key),
                    _ => None,
                },
                held.and_then(|held| held.keys.get(key)).map(Vec::as_slice),
            ),
        };
        self.of_value(&[placed], value, held.unwrap_or_default())
    }

    /// Whether a name-less mount label stands at `placed`'s call (K1
    /// ruling): it applies only where the call's first argument cannot be
    /// a path. A string, a template, or any value the walk does not read as
    /// an instance (a library instance or a scope) could be one, and a
    /// router served past an unread path would be served at its parent's
    /// address. There only a named scope, whose name slot reads the path,
    /// applies; with none, what is handed there is a mount the trace cannot
    /// place (`mount_unverified`), never the parent's address. An object or
    /// a function written in place is no path.
    pub(super) fn name_less_stands(self, placed: Placed<'a>) -> bool {
        match placed.call.args.first() {
            Some(Value::Obj(_) | Value::Callback(_)) => return true,
            None if placed.call.instance.args.is_empty() => return true,
            Some(Value::Str(_)) | None => {}
        }
        self.of_arg(placed, 0, None)
            .iter()
            .any(|end| matches!(end.end, InstanceAt::Root { .. } | InstanceAt::Scope { .. }))
    }

    fn of_value(
        mut self,
        down: &[Placed<'a>],
        value: Option<&Value>,
        held: &[ValueAt],
    ) -> Vec<InstanceEnd> {
        let last = down[down.len() - 1];
        let Some(value) = value else {
            let at = self.at(&last);
            self.value(held, last.file, at);
            return self.ends;
        };
        let mut pieces = value.pieces();
        if !holds_parameter(&pieces) {
            let at = self.at(&last);
            if !self.value(held, last.file, at.clone()) && !self.field(last.file, &pieces) {
                self.hole(at, InstanceHole::Unread, opaque_text(&pieces));
            }
            return self.ends;
        }
        // Down the site's own chain first: the deciding caller's value is
        // the one this site was decided by.
        for k in (0..down.len() - 1).rev() {
            let below = down[k + 1];
            let Some(own) = to_definition(&pieces, below.depth) else {
                self.callback(&below, &pieces);
                return self.ends;
            };
            let caller = down[k];
            pieces = filled(&own, caller.call);
            if !holds_parameter(&pieces) {
                self.handed(&caller, &own, &pieces);
                return self.ends;
            }
        }
        self.up(down[0], pieces);
        self.ends
    }

    /// Walk up every caller of the definition `placed` is written in,
    /// whose receiver, as `placed`'s body reads it, is `pieces`.
    fn up(&mut self, placed: Placed<'a>, pieces: Vec<Piece>) {
        let Some(own) = to_definition(&pieces, placed.depth) else {
            self.callback(&placed, &pieces);
            return;
        };
        let Some(key) = placed.key else {
            let at = self.at(&placed);
            self.hole(at, InstanceHole::Unread, None);
            return;
        };
        let at = self.at(&placed);
        self.up_from(placed.file, key, own, at);
    }

    /// Walk up every caller of the definition `key` of `file`, whose
    /// receiver, as its own body reads it, is `own`; `at` names the walk's
    /// position for its ends.
    fn up_from(&mut self, file: &Path, key: &str, own: Vec<Piece>, at: String) {
        let here = (file.to_path_buf(), key.to_string());
        if self.passing.contains(&here) {
            return;
        }
        if !self
            .walked
            .insert((file.to_path_buf(), key.to_string(), own.clone()))
        {
            return;
        }
        let callers = self.calls.callers_of(file, key);
        // A library member may run the function as well as code calling
        // it: each scope a labelled member opens for it is an end too
        // (carrick#2474 R2b).
        let hosts = self.hosts_of(file, key);
        let param = match own.as_slice() {
            [Piece::Param(index, _)] => Some(*index),
            _ => None,
        };
        let mut scoped = false;
        for (host, arg) in &hosts {
            scoped |= self.scope(host, *arg, param, at.clone())
                || self.handed_by(host.file, host.call, param, at.clone());
        }
        if callers.is_empty() {
            match (hosts.is_empty(), scoped) {
                (true, _) => {
                    // Another service may call it (carrick#2474 R5a).
                    let boundary = param.zip(self.fn_at(file, key)).map(|(param, function)| {
                        Boundary::EntryParam {
                            function,
                            param: param as u32,
                        }
                    });
                    self.boundary_hole(at, InstanceHole::EntryPoint, boundary);
                }
                (false, false) => self.hole(at, InstanceHole::RunsByLibrary, None),
                (false, true) => {}
            }
            return;
        }
        self.passing.push(here);
        for caller in callers {
            self.filled_by(*caller, &own);
        }
        self.passing.pop();
    }

    /// What `caller` hands in at the parameter `own` reads: walked up when
    /// it is the caller's own parameter, else the instance it holds.
    fn filled_by(&mut self, caller: Placed<'a>, own: &[Piece]) {
        let value = filled(own, caller.call);
        if holds_parameter(&value) {
            self.up(caller, value);
        } else {
            self.handed(&caller, own, &value);
        }
    }

    /// The receiver is a parameter of the function `placed` is written in,
    /// several functions in from its definition: a callback. A library
    /// call that runs it is R2b's; anything else is a callback parameter.
    ///
    /// A function written as a scope member's `runs` argument opens a scope
    /// (carrick#2474 R2b); one written under a key of an object literal is
    /// a callback option.
    fn callback(&mut self, placed: &Placed<'a>, pieces: &[Piece]) {
        // Every parameter the receiver holds belongs to a callback that
        // encloses this one: read it where it was declared, from that
        // callback's own host, one callback out at a time. `up` refuses
        // again at the next callback in when the pieces are still captured.
        if !pieces
            .iter()
            .any(|piece| matches!(piece, Piece::Param(..) | Piece::ParamKey(..)))
            && pieces.iter().any(|piece| matches!(piece, Piece::Outer(..)))
            && let Some(host) = placed.host
            && let Some(outer) =
                self.calls
                    .at_span(placed.file, host.site.span_start, host.site.span_end)
        {
            let lifted = pieces.iter().map(Piece::lifted).collect();
            self.up(outer, lifted);
            return;
        }
        let at = self.at(placed);
        let written = placed.written_in;
        if let Some(written) = written.filter(|written| !written.path.is_empty()) {
            // Where the function is written under one key and its own
            // parameter holds the instance, the function the host runs or
            // calls reads that key (carrick#2474 R5a).
            // The call the object literal is written in: `host` names only
            // a call handed the function itself.
            let host = self
                .calls
                .at_span(placed.file, written.site.span_start, written.site.span_end)
                .map(|host| host.call);
            // A key a later spread or computed key may set again is not
            // read as the function written there (carrick#2474 R5b).
            let key_stands = |host: &CallIr, key: &str| match host.args.get(written.arg) {
                Some(Value::Obj(object)) => !matches!(object.key(key), KeyState::Unread),
                _ => true,
            };
            let boundary = match (pieces, written.path.as_slice(), host) {
                ([Piece::Param(param, _)], [key], Some(host)) if key_stands(host, key) => self
                    .runs_of(placed.file, host)
                    .map(|(runs, runs_arg)| Boundary::CallbackOption {
                        host_at: self.at_call(placed.file, host),
                        arg: written.arg as u32,
                        key: key.clone(),
                        param: *param as u32,
                        runs,
                        runs_arg,
                    }),
                _ => None,
            };
            self.boundary_hole(at, InstanceHole::CallbackOption, boundary);
            return;
        }
        let reason = match placed.host {
            Some(host) if self.library_call(placed.file, host) => {
                // The callback's own parameter, read in its own body.
                let param = match pieces {
                    [Piece::Param(index, _)] => Some(*index),
                    _ => None,
                };
                let host_call = host;
                let host =
                    self.calls
                        .at_span(placed.file, host.site.span_start, host.site.span_end);
                if let (Some(host), Some(written)) = (host, written)
                    && self.scope(&host, written.arg, param, at.clone())
                {
                    return;
                }
                if self.handed_by(placed.file, host_call, param, at.clone()) {
                    return;
                }
                InstanceHole::RunsByLibrary
            }
            _ => InstanceHole::CallbackParameter,
        };
        self.hole(at, reason, None);
    }

    /// The scope a labelled member opens at `host` for the function it is
    /// handed at `arg`, whose parameter `param` holds the instance: a scope
    /// end under each of the host's receiver's ends a `runs` label applies
    /// to. False where no label applies.
    fn scope(&mut self, host: &Placed<'a>, arg: usize, param: Option<usize>, at: String) -> bool {
        let Some(param) = param else {
            return false;
        };
        let labels: Vec<&'w ScopeLabel> = self
            .scopes
            .iter()
            .filter(|label| {
                label.member == host.call.callee
                    && matches!(&label.binds, ScopeBinds::Runs { arg: runs, key: None, param: yields }
                        if *runs == arg && *yields == param)
            })
            .collect();
        self.open(host, labels, at)
    }

    /// The instance a socket library's call hands the function it runs
    /// (carrick#1670): an end at `call`, written in `file`, where the store
    /// labels `call` as a socket op and states the side the instance it
    /// hands is on. The op's `yields` states it for the parameter `param`
    /// (a different parameter is not the instance); an op that names no
    /// `yields` hands in the instance of its own export, on that export's
    /// side. False where the call is no labelled socket call, or states no
    /// side (a `by_origin` export hands in nothing of its own).
    fn handed_by(&mut self, file: &Path, call: &CallIr, param: Option<usize>, at: String) -> bool {
        let span = (file.to_path_buf(), call.site.span_start, call.site.span_end);
        let Some(host) = self.index.socket_hosts.get(&span) else {
            return false;
        };
        let (export, side) = match (&host.yields, &host.side) {
            (Some(yields), _) if param == Some(yields.param as usize) => {
                (yields.export.clone(), yields.side.clone())
            }
            (Some(_), _) => return false,
            (None, Some(side)) => (host.export.clone(), side.clone()),
            (None, None) => return false,
        };
        if !matches!(side.as_str(), "client" | "server") {
            return false;
        }
        self.push(
            at,
            InstanceAt::Handed {
                file: relative(self.root, file),
                line: call.site.line,
                package: host.package.clone(),
                export,
                side,
            },
        );
        true
    }

    /// The value of the call at `span` in `file`: the scope a member a
    /// `returns` label names opens (`app.basePath("/v2")`), or a hole.
    fn returned(&mut self, file: &Path, span: (u32, u32), at: String) {
        let host = self.calls.inputs_for(file).files.get(file).and_then(|ir| {
            self.calls
                .at_span(file, ir.swc_start(span.0), ir.swc_start(span.1))
        });
        let Some(host) = host else {
            self.hole(at, InstanceHole::Unread, None);
            return;
        };
        self.returned_at(host, at);
    }

    /// The value of `host`'s call: its receiver, where a pass-through
    /// label names the member (K1), the scope a member a `returns` label
    /// names opens there, or a hole.
    fn returned_at(&mut self, host: Placed<'a>, at: String) {
        if self.library_call(host.file, host.call) && self.passes(&host) {
            return;
        }
        let labels: Vec<&'w ScopeLabel> = self
            .scopes
            .iter()
            .filter(|label| label.member == host.call.callee && label.binds == ScopeBinds::Returns)
            .collect();
        if !self.library_call(host.file, host.call) || !self.open(&host, labels, at.clone()) {
            self.hole(
                at,
                InstanceHole::Unread,
                Some(host.call.callee_text.clone()),
            );
        }
    }

    /// The value of a pass-through member's call at `host` (K1): each of
    /// its receiver's ends a pass-through label of the member applies to,
    /// as the receiver's own walk reads it, so `parent.use("/v1",
    /// sub.routes())` serves `sub` under `/v1`. Where every end above is a
    /// hole (a router handed in), those holes, when the member declaration
    /// the call resolves to is one a pass-through label was verified at.
    /// False where no label applies.
    fn passes(&mut self, host: &Placed<'a>) -> bool {
        let labels: Vec<&'w ScopeLabel> = self
            .scopes
            .iter()
            .filter(|label| label.member == host.call.callee && label.binds == ScopeBinds::Passes)
            .collect();
        if labels.is_empty() {
            return false;
        }
        let here = (
            host.file.to_path_buf(),
            host.call.site.span_start,
            host.call.site.span_end,
        );
        if self.hosts.contains(&here) {
            return false;
        }
        let parents = self.parents(host, here);
        if !parents.is_empty() && parents.iter().all(|end| end.hole().is_some()) {
            let Some(declared) = self.declaration_of(host) else {
                return false;
            };
            if !labels
                .iter()
                .any(|label| label.verified_at(&declared).next().is_some())
            {
                return false;
            }
            self.pass(parents);
            return true;
        }
        let applying: Vec<InstanceEnd> = self
            .applying(host, &labels, &parents)
            .into_iter()
            .flatten()
            .collect();
        let passed = !applying.is_empty();
        self.pass(applying);
        passed
    }

    /// The receiver's own `ends`, as its walk read them, as this value's.
    fn pass(&mut self, ends: Vec<InstanceEnd>) {
        for end in ends {
            if !self.ends.contains(&end) {
                self.ends.push(end);
            }
        }
    }

    /// A scope end under each of `host`'s receiver's ends one of `labels`
    /// applies to. False where none applies.
    fn open(&mut self, host: &Placed<'a>, labels: Vec<&'w ScopeLabel>, at: String) -> bool {
        if labels.is_empty() {
            return false;
        }
        let here = (
            host.file.to_path_buf(),
            host.call.site.span_start,
            host.call.site.span_end,
        );
        if self.hosts.contains(&here) {
            return false;
        }
        let parents = self.parents(host, here);
        // Where every end above is a hole (a plugin's own app, handed in by
        // whatever runs it), the member declaration the call resolves to
        // says which scope opens: exactly one label verified on exactly one
        // receiver whose verdict names it (carrick#2636), or none. The path
        // below is relative to those holes.
        if !parents.is_empty() && parents.iter().all(|end| end.hole().is_some()) {
            let Some(declared) = self.declaration_of(host) else {
                return false;
            };
            let matched: Vec<(&ScopeLabel, &str)> = labels
                .iter()
                .flat_map(|label| label.verified_at(&declared).map(move |at| (*label, at)))
                .collect();
            let [(label, receiver)] = matched.as_slice() else {
                return false;
            };
            let prefix = self.prefix_of(host, &label.name, None);
            let scope = InstanceAt::Scope {
                file: relative(self.root, host.file),
                line: host.call.site.line,
                member: label.member.clone(),
                value: self.value_of(host, &label.name, &prefix),
                prefix,
                package: label.package.clone(),
                export: label.export.clone(),
                receiver: receiver.to_string(),
                parents,
            };
            self.push(at, scope);
            return true;
        }
        let applying = self.applying(host, &labels, &parents);
        let mut opened = false;
        for (label, applies) in labels.into_iter().zip(applying) {
            let Some((package, export, receiver)) = applies.first().and_then(identity) else {
                continue;
            };
            // A parameter in the prefix is filled by whatever hands this
            // instance in. Where the parents name every call that hands it
            // in (a caller of the function, or the member that runs it),
            // each gets the values its own call fills; otherwise a parent
            // may stand for several calls (the walk reads a definition
            // once), so each reads every caller's and host's. Parents that
            // read the same share one scope end (carrick#2635).
            let origin_of = |parent: &InstanceEnd| match &parent.end {
                InstanceAt::Scope { file, line, .. } => format!("{file}:{line}"),
                InstanceAt::Root { .. } | InstanceAt::Hole { .. } | InstanceAt::Handed { .. } => {
                    parent.at.clone()
                }
            };
            let handers = self.handers(host);
            let paired = !handers.is_empty()
                && handers
                    .iter()
                    .all(|hander| applies.iter().any(|parent| origin_of(parent) == *hander));
            let mut groups: Vec<(Prefix, Vec<InstanceEnd>)> = Vec::new();
            for parent in applies {
                let origin = origin_of(&parent);
                let prefix = self.prefix_of(host, &label.name, paired.then_some(origin.as_str()));
                match groups.iter_mut().find(|(known, _)| *known == prefix) {
                    Some((_, parents)) => parents.push(parent),
                    None => groups.push((prefix, vec![parent])),
                }
            }
            for (prefix, parents) in groups {
                self.push(
                    at.clone(),
                    InstanceAt::Scope {
                        file: relative(self.root, host.file),
                        line: host.call.site.line,
                        member: label.member.clone(),
                        value: self.value_of(host, &label.name, &prefix),
                        prefix,
                        package: package.clone(),
                        export: export.clone(),
                        receiver: receiver.clone(),
                        parents,
                    },
                );
            }
            opened = true;
        }
        opened
    }

    /// The function `host`, a call written in `file`, runs or calls, as the
    /// rows name its declaration in the repository: for a library member a
    /// `runs` label names, the function handed at the label's `runs`
    /// argument (with that argument); for a call of the repository's own
    /// code, the callee (carrick#2474 R5a).
    fn runs_of(&self, file: &Path, host: &CallIr) -> Option<(FnAt, Option<u32>)> {
        let holder = self.calls.inputs_for(file);
        let positions = holder.positions.as_ref()?;
        let ir = holder.files.get(file)?;
        let named = |row: &crate::position_rows::Row| {
            if !super::super::discovery::in_repo(row) {
                return None;
            }
            Some(FnAt {
                decl_file: row.decl_file()?.to_string(),
                decl_line: row.decl_line()?,
            })
        };
        if self.library_call(file, host) {
            let runs = self.scopes.iter().find_map(|label| match &label.binds {
                ScopeBinds::Runs { arg, key: None, .. } if label.member == host.callee => {
                    Some(*arg)
                }
                _ => None,
            })?;
            let (lo, hi) = host.instance.args.get(runs)?.span?;
            let row = positions
                .lookup(file, ir.swc_start(lo), ir.swc_start(hi), Kind::Ident)
                .ok()?;
            return Some((named(row)?, Some(runs as u32)));
        }
        let row = positions
            .lookup(
                file,
                ir.swc_start(host.site.lo),
                ir.swc_start(host.site.hi),
                Kind::Call,
            )
            .ok()?;
        Some((named(row)?, None))
    }

    /// The ends of `host`'s own receiver, walked inside this walk's scope
    /// calls and `here`, `host`'s span: read from the index once walked.
    fn parents(&self, host: &Placed<'a>, here: HostSpan) -> Vec<InstanceEnd> {
        let key = (self.hosts.clone(), here.clone());
        if let Some(parents) = self.index.parents.borrow().get(&key) {
            return parents.clone();
        }
        let parents = self.nested().with_host(here).of(&[*host]);
        self.index.parents.borrow_mut().insert(key, parents.clone());
        parents
    }

    /// For each of `labels` (the labels of `host`'s member), the `ends` of
    /// `host`'s receiver it applies to: those of the export it names, else,
    /// for an end of an export no label names (the app `default` makes,
    /// where the store labels the scope on the named export of the same
    /// factory), the one label of the end's package whose verdict names the
    /// member declaration the call resolves to, where every label verified
    /// there binds the same way (carrick#3185, carrick#3207). Every gate
    /// that reads a scope label against a call's receiver (the scope a call
    /// opens, the call-ins at a join point, mount edges, adapter mounts)
    /// asks here, so none keeps its own copy of the rule.
    pub(super) fn applying(
        &self,
        host: &Placed<'_>,
        labels: &[&ScopeLabel],
        ends: &[InstanceEnd],
    ) -> Vec<Vec<InstanceEnd>> {
        let declared = self.declaration_of(host);
        // Per end, the labels it applies by declaration: the first of each
        // binding verified there, where every two bindings agree (K1: a
        // path-first and a path-less mount read different slots).
        let by_declaration: Vec<Vec<usize>> = ends
            .iter()
            .map(|end| {
                if labels.iter().any(|label| label.applies_to(end)) {
                    return Vec::new();
                }
                let (Some((package, _, _)), Some(declared)) = (identity(end), declared.as_ref())
                else {
                    return Vec::new();
                };
                let mut kept: Vec<usize> = Vec::new();
                for at in (0..labels.len()).filter(|&at| {
                    labels[at].package == package
                        && labels[at].verified_at(declared).next().is_some()
                }) {
                    if !kept.iter().any(|&known| labels[known].binds_as(labels[at])) {
                        kept.push(at);
                    }
                }
                let agree = kept.iter().all(|&one| {
                    kept.iter()
                        .all(|&other| labels[one].agrees_with(labels[other]))
                });
                if agree { kept } else { Vec::new() }
            })
            .collect();
        labels
            .iter()
            .enumerate()
            .map(|(index, label)| {
                ends.iter()
                    .zip(&by_declaration)
                    .filter(|(end, declared)| label.applies_to(end) || declared.contains(&index))
                    .map(|(end, _)| end.clone())
                    .collect()
            })
            .collect()
    }

    /// The member declaration (`decl_file`, `decl_line`) `host`'s call
    /// resolves to, as the compiler's call row names it.
    fn declaration_of(&self, host: &Placed<'_>) -> Option<(String, u32)> {
        let holder = self.calls.inputs_for(host.file);
        let positions = holder.positions.as_ref()?;
        let ir = holder.files.get(host.file)?;
        let row = positions
            .lookup(
                host.file,
                ir.swc_start(host.call.site.lo),
                ir.swc_start(host.call.site.hi),
                Kind::Call,
            )
            .ok()?;
        Some((row.decl_file()?.to_string(), row.decl_line()?))
    }

    /// A fresh walk for another value, inside this one's scope calls.
    pub(super) fn nested(&self) -> InstanceWalk<'w, 'a> {
        let mut walk = InstanceWalk::new(
            self.calls,
            self.inputs,
            self.root,
            self.makers,
            self.scopes,
            self.index,
        );
        walk.hosts = self.hosts.clone();
        walk
    }

    pub(super) fn with_host(mut self, host: (PathBuf, u32, u32)) -> Self {
        self.hosts.push(host);
        self
    }

    /// The caller `caller` hands in `value`, its argument at the parameter
    /// (or key of one) `own` reads: the instance that argument holds.
    fn handed(&mut self, caller: &Placed<'_>, own: &[Piece], value: &[Piece]) {
        let at = self.at(caller);
        let args = &caller.call.instance.args;
        let held = match own {
            [Piece::Param(index, _)] => args.get(*index).map(|arg| arg.whole.as_slice()),
            [Piece::ParamKey(index, key, _)] => args
                .get(*index)
                .and_then(|arg| arg.keys.get(key))
                .map(Vec::as_slice),
            _ => None,
        };
        if !self.value(held.unwrap_or_default(), caller.file, at.clone())
            && !self.field(caller.file, value)
        {
            self.hole(at, InstanceHole::Unread, opaque_text(value));
        }
    }

    /// A field of a class of `file` that is one constructor parameter, and
    /// nothing else (carrick#1670 T3, [`FieldOrigin::parameter`]), is that
    /// parameter: the instance is what each `new` of the class hands in
    /// there, walked as a call hands in a parameter. A field read any other
    /// way, or of a class not read through its constructions, ends nothing
    /// here: `false`.
    ///
    /// [`FieldOrigin::parameter`]: super::super::constructions::FieldOrigin
    fn field(&mut self, file: &Path, pieces: &[Piece]) -> bool {
        let [
            Piece::Opaque {
                link: Some(link), ..
            },
        ] = pieces
        else {
            return false;
        };
        let Link::Field(link) = &**link else {
            return false;
        };
        self.field_parameter(file, link.class, &link.field)
    }

    /// [`Self::field`] for the field `field` of the class `class`.
    fn field_parameter(&mut self, file: &Path, class: u32, field: &str) -> bool {
        let Some(position) = self
            .calls
            .inputs_for(file)
            .files
            .get(file)
            .and_then(|ir| ir.class_origins.get(&class))
            .and_then(|origin| origin.fields.get(field))
            .and_then(|from| from.parameter)
        else {
            return false;
        };
        let index = self.index;
        let Some(built) = index
            .constructions(file, class)
            .filter(|built| !built.is_empty())
        else {
            return false;
        };
        let here = (file.to_path_buf(), class, field.to_string());
        if self.fielding.contains(&here) {
            return true;
        }
        self.fielding.push(here);
        let own = [Piece::Param(position, field.to_string())];
        for construction in built {
            // The call graph's own entry for it: where a function it is
            // written in sits, which the construction index does not keep.
            let construction = self
                .calls
                .at_span(
                    construction.file,
                    construction.call.site.span_start,
                    construction.call.site.span_end,
                )
                .unwrap_or(*construction);
            self.filled_by(construction, &own);
        }
        self.fielding.pop();
        true
    }

    /// What `values`, the value resolver's lowering of an expression read in
    /// `file`, ends at (carrick#3504 S3): a maker or own factory's instance,
    /// a name whose declaration the compiler names, a scope a labelled
    /// member returns, or a field of the class. `false` where it names no
    /// instance, and the caller reads on (a field the pieces link, then a
    /// hole).
    fn value(&mut self, values: &[ValueAt], file: &Path, at: String) -> bool {
        match one_reading(values) {
            Some(
                ValueAt::Read {
                    holds: Holds::Made(client),
                    ..
                }
                | ValueAt::Returned {
                    made: Some(client), ..
                }
                | ValueAt::New { made: client, .. },
            ) => self.client(client, file, at),
            Some(ValueAt::Read {
                lo,
                hi,
                member: false,
                holds: Holds::Declared,
            }) => self.imported(file, (*lo, *hi), at),
            Some(ValueAt::Returned {
                lo,
                hi,
                member: Some(_),
                made: None,
                ..
            }) => self.returned(file, (*lo, *hi), at),
            Some(ValueAt::Field { class, field, .. }) => self.class_field(file, *class, field, at),
            _ => return false,
        }
        true
    }

    /// The instance a field of a class of `file` holds (carrick#3489 W1): the
    /// maker every write in the class sets it to
    /// ([`FileIr::field_receivers`], carrick#1665), else the constructor
    /// parameter it is ([`Self::field_parameter`]), else no instance the
    /// syntax names.
    ///
    /// [`FileIr::field_receivers`]: super::super::FileIr
    fn class_field(&mut self, file: &Path, class: u32, field: &str, at: String) {
        let made = self
            .calls
            .inputs_for(file)
            .files
            .get(file)
            .and_then(|ir| ir.field_receivers.get(&(class, field.to_string())))
            .cloned();
        if let Some(client) = made {
            self.client(&client, file, at);
        } else if !self.field_parameter(file, class, field) && !self.getter(file, class, field, &at)
        {
            self.hole(at, InstanceHole::Unread, Some(format!("this.{field}")));
        }
    }

    /// The getter `field` names in the class (carrick#3489 W4): what it
    /// returns is the instance. An own factory's makers are read as a call's
    /// are; any other return is read where it is written (a name the
    /// compiler declares, or a field of the class), and a return nothing
    /// reads is a hole. `false` where the class has no such getter.
    fn getter(&mut self, file: &Path, class: u32, field: &str, at: &str) -> bool {
        let Some(key) = self
            .calls
            .inputs_for(file)
            .files
            .get(file)
            .and_then(|ir| ir.class_getters.get(&(class, field.to_string())))
            .cloned()
        else {
            return false;
        };
        self.returns(file, key, at, Some(format!("this.{field}")));
        true
    }

    /// What the function `key` of `file` returns is the instance at `at`
    /// (carrick#3504 S3c): an own factory's makers are read as a call's are;
    /// any other return is read where it is written (a name the compiler
    /// declares, or a field of the class), and a return nothing reads is a
    /// hole carrying `unread`. A function already being followed ends
    /// nothing again.
    fn returns(&mut self, file: &Path, key: String, at: &str, unread: Option<String>) {
        let here = (file.to_path_buf(), key.clone());
        if !self.factories.insert(here.clone()) {
            return;
        }
        let function = self
            .calls
            .inputs_for(file)
            .files
            .get(file)
            .and_then(|ir| ir.functions.get(&key));
        let (makers, values) = match function {
            Some(function) => (
                function
                    .returned
                    .as_ref()
                    .map(|returned| returned.makers.clone()),
                function.returned_values.clone(),
            ),
            None => (None, Vec::new()),
        };
        if let Some(makers) = makers {
            for maker in makers {
                self.client(&maker, file, at.to_string());
            }
        } else if values.is_empty() {
            self.hole(at.to_string(), InstanceHole::Unread, unread);
        } else {
            for value in values {
                match value {
                    ValueAt::Read { lo, hi, .. } => self.imported(file, (lo, hi), at.to_string()),
                    ValueAt::Field { class, field, .. } => {
                        self.class_field(file, class, &field, at.to_string())
                    }
                    _ => self.hole(at.to_string(), InstanceHole::Unread, None),
                }
            }
        }
        self.factories.remove(&here);
    }

    /// The maker of `client`, an instance read in `file`: a labelled maker
    /// is a root; an own factory call is followed into what the factory
    /// returns.
    fn client(&mut self, client: &ClientRef, file: &Path, at: String) {
        let Some(instance) = &client.instance else {
            self.hole(at, InstanceHole::Unread, None);
            return;
        };
        match &instance.made_by {
            MadeBy::Export => {
                let package = super::super::package_name(&client.package).to_string();
                let new = instance.form == MakerForm::New;
                let labelled = self.makers.iter().any(|maker| {
                    maker.package == package
                        && maker.export == client.export
                        && maker.member == instance.member
                        && maker.new == new
                });
                if labelled {
                    self.push(
                        at,
                        InstanceAt::Root {
                            file: relative(self.root, file),
                            line: instance.site.line,
                            package,
                            export: client.export.clone(),
                            member: instance.member.clone(),
                            receiver: super::super::library_sites::maker_receiver_id(
                                new,
                                instance.member.as_deref(),
                            ),
                            made: Some(MakerCall {
                                form: instance.form,
                                args: instance.args.clone(),
                                span: (instance.site.span_start, instance.site.span_end),
                            }),
                        },
                    );
                } else {
                    self.hole(at, InstanceHole::Unread, Some(client.package.clone()));
                }
            }
            MadeBy::Call(_) => {
                let placed =
                    self.calls
                        .at_span(file, instance.site.span_start, instance.site.span_end);
                let target = placed
                    .and_then(|placed| self.calls.target(&placed))
                    .cloned();
                // A member call the call graph does not resolve into this
                // service: a scope a labelled member returns, if any
                // (carrick#2474 R2b).
                if let (None, Some(placed)) = (&target, placed) {
                    self.returned_at(placed, at);
                    return;
                }
                match target {
                    Some((target_file, key)) => self.returns(&target_file, key, &at, None),
                    None => self.hole(at, InstanceHole::Unread, None),
                }
            }
        }
    }

    /// The name an import binds at `span` in `file`: the module-scope
    /// instance the declaration the compiler names holds.
    fn imported(&mut self, file: &Path, span: (u32, u32), at: String) {
        let holder = self.calls.inputs_for(file);
        let Some(positions) = holder.positions.as_ref() else {
            self.hole(at, InstanceHole::Unread, None);
            return;
        };
        let Some(ir) = holder.files.get(file) else {
            self.hole(at, InstanceHole::Unread, None);
            return;
        };
        let Ok(row) = positions.lookup(
            file,
            ir.swc_start(span.0),
            ir.swc_start(span.1),
            Kind::Ident,
        ) else {
            self.hole(at, InstanceHole::Unread, None);
            return;
        };
        if row.flags() & FLAG_ANY != 0 || row.type_text() == Some("any") {
            self.hole(
                at,
                InstanceHole::UntypedReceiver,
                row.symbol().map(str::to_string),
            );
            return;
        }
        if !super::super::discovery::in_repo(row) {
            self.hole(
                at,
                InstanceHole::OutsideService,
                row.decl_file().map(str::to_string),
            );
            return;
        }
        let (Some(decl_file), Some(line), Some(symbol)) =
            (row.decl_file(), row.decl_line(), row.symbol())
        else {
            self.hole(at, InstanceHole::Unread, None);
            return;
        };
        let declaring = positions.path_of(decl_file);
        let Some(declared) = self.calls.inputs_for(&declaring).files.get(&declaring) else {
            self.hole(
                at,
                InstanceHole::OutsideService,
                Some(decl_file.to_string()),
            );
            return;
        };
        let client = declared
            .declared_at
            .get(symbol)
            .filter(|(starts, _)| *starts == line)
            .and_then(|(_, name)| declared.module_instance(name))
            .cloned();
        match client {
            Some(client) => self.client(&client, &declaring, at),
            None if self.module_binding(&declaring, line, symbol, &at) => {}
            None => self.hole(at, InstanceHole::Unread, Some(symbol.to_string())),
        }
    }

    /// The instances the module binding `symbol`, declared on `line` of
    /// `file`, holds (carrick#3489 W5, carrick#3504 S1): each value written
    /// to it, its initialiser and every assignment, is an end (the walk's
    /// per-end rule). A write of the writing function's parameter walks up
    /// that function's callers; a write the census cannot place, or whose
    /// value names no instance, is a hole. `false` where the file declares
    /// no such binding.
    fn module_binding(&mut self, file: &Path, line: u32, symbol: &str, at: &str) -> bool {
        let inputs = self.calls.inputs_for(file);
        let Some(ir) = inputs.files.get(file) else {
            return false;
        };
        // A function's own `let` (carrick#3504 S1b) is read for callees
        // only: a receiver held in one is the walk's other readers' until
        // the resolver owns receivers (S4).
        let Some(binding) = ir
            .module_bindings
            .get(&(line, symbol.to_string()))
            .filter(|binding| binding.declared.module_scope() && binding.values_read())
        else {
            return false;
        };
        let here = (file.to_path_buf(), format!("{line}:{symbol}"));
        if !self.factories.insert(here.clone()) {
            return true;
        }
        let unread = Some(symbol.to_string());
        if binding.unplaceable {
            self.hole(at.to_string(), InstanceHole::Unread, unread);
        } else {
            // An initialiser that names no instance is a hole; with none, the
            // binding holds nothing until written.
            if !self.value(&binding.init, file, at.to_string()) && !binding.init.is_empty() {
                self.hole(at.to_string(), InstanceHole::Unread, unread.clone());
            }
            for start in &binding.writes {
                let placed = super::super::resolve::placed_write(ir, *start);
                let own = placed.and_then(|(key, write)| {
                    let key = key?;
                    let function = ir.functions.get(key)?;
                    let own = super::super::resolve::written_param(function, &write.values)?;
                    Some((key, own))
                });
                // A write of the writing function's parameter: walk up its
                // callers.
                if let Some((key, own)) = own {
                    self.up_from(file, key, own, at.to_string());
                    continue;
                }
                let ended = placed
                    .is_some_and(|(_, write)| self.value(&write.values, file, at.to_string()));
                if !ended {
                    self.hole(at.to_string(), InstanceHole::Unread, unread.clone());
                }
            }
        }
        self.factories.remove(&here);
        true
    }

    /// Whether `call`, written in `file`, is a library call
    /// ([`library_call`]).
    pub(super) fn library_call(&self, file: &Path, call: &CallIr) -> bool {
        library_call(self.calls.inputs_for(file), file, call)
    }

    /// The library calls that may run the function `key` of `file`, each
    /// with the argument it is handed at, in file and source order: those
    /// in `file` handed it by name (`server.register(plugin)`), then the
    /// calls of a `runs` scope member in another file handed a name the
    /// compiler resolves to it (`app.register(items)`, `items` imported;
    /// carrick#2474 R2b).
    pub(super) fn hosts_of(&self, file: &Path, key: &str) -> Vec<Host<'a>> {
        let mut hosts: Vec<Host<'a>> = self
            .index
            .by_name
            .get(&(file.to_path_buf(), key.to_string()))
            .cloned()
            .unwrap_or_default();
        let line = self
            .inputs
            .files
            .get(file)
            .and_then(|declared| declared.declared_at.get(key))
            .map(|(line, _)| *line);
        if let Some(line) = line
            && let Some(by_declaration) = self
                .index
                .by_declaration
                .get(&(relative(self.root, file), line))
        {
            hosts.extend(
                by_declaration
                    .iter()
                    .filter(|(placed, _)| placed.file != file)
                    .copied(),
            );
        }
        hosts
    }
}

/// The one value an expression's lowering holds, or `None` where it holds
/// several (`a ?? b`, a local written two ways): such a receiver or argument
/// reads as naming no instance until the walk ends at each value
/// (carrick#3504 S3b deletes this guard).
pub(super) fn one_reading(values: &[ValueAt]) -> Option<&ValueAt> {
    match values {
        [one] => Some(one),
        _ => None,
    }
}

/// Whether `call`, written in `file`, is a library call: its callee is
/// declared outside the repository, as the compiler names it.
pub(super) fn library_call(inputs: &RequestSummaryInputs, file: &Path, call: &CallIr) -> bool {
    let (Some(positions), Some(ir)) = (inputs.positions.as_ref(), inputs.files.get(file)) else {
        return false;
    };
    positions
        .lookup(
            file,
            ir.swc_start(call.site.lo),
            ir.swc_start(call.site.hi),
            Kind::Call,
        )
        .is_ok_and(library_row)
}

/// Whether a call row's declaration is outside the repository. One the rows
/// could not name a file for is outside too (carrick#2474 R2b).
fn library_row(row: &crate::position_rows::Row) -> bool {
    (row.decl_file().is_some() || row.flags() & FLAG_DECLARATION_UNNAMED != 0)
        && !super::super::discovery::in_repo(row)
}

/// The package, export and receiver id of the instance `end` ends at.
pub(super) fn identity(end: &InstanceEnd) -> Option<(String, String, String)> {
    match &end.end {
        InstanceAt::Root {
            package,
            export,
            receiver,
            ..
        }
        | InstanceAt::Scope {
            package,
            export,
            receiver,
            ..
        } => Some((package.clone(), export.clone(), receiver.clone())),
        InstanceAt::Hole { .. } | InstanceAt::Handed { .. } => None,
    }
}

/// The side of the connection a `by_origin` socket of `package` is on,
/// from the instance ends of the site it is called at (carrick#1670; the rule
/// is stated in docs/reference/client-semantics.md, "Message roles"): the
/// side a socket library's call hands it in on, or the client's where a
/// maker of the package made it. Every end must give the same side. A hole,
/// an end of another package, a scope, or two sides: none, and the site
/// states no direction.
pub fn origin_side(ends: &[InstanceEnd], package: &str) -> Option<&'static str> {
    let mut side: Option<&'static str> = None;
    for end in ends {
        let this = match &end.end {
            InstanceAt::Handed {
                package: handed,
                side,
                ..
            } if handed == package => match side.as_str() {
                "client" => "client",
                "server" => "server",
                _ => return None,
            },
            InstanceAt::Root { package: made, .. } if made == package => "client",
            _ => return None,
        };
        if side.is_some_and(|known| known != this) {
            return None;
        }
        side = Some(this);
    }
    side
}

/// The text of `pieces` where every piece is literal.
fn literal(pieces: &[Piece]) -> Option<String> {
    pieces
        .iter()
        .map(|piece| match piece {
            Piece::Lit(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// What a parameter reads as at one call that fills it (carrick#2635).
enum Fill {
    /// What the call hands in, in the terms of the caller's own body.
    Pieces(Vec<Piece>),
    /// The call hands nothing there: the parameter takes its default, or
    /// holds nothing.
    Absent,
    /// The call hands something this pass does not read.
    Unread,
}

/// `own` (a function's parameters by position, and keys of them) as a call
/// fills it, where `arg_at` gives the argument the call writes at a
/// position and `positional` says no spread moves them. A parameter that is
/// all `own` holds can be absent; mixed with other pieces it cannot be read.
fn fill<'v>(own: &[Piece], positional: bool, arg_at: impl Fn(usize) -> Option<&'v Value>) -> Fill {
    let alone = positional && own.len() == 1;
    let absent = || if alone { Fill::Absent } else { Fill::Unread };
    let mut out: Vec<Piece> = Vec::new();
    for piece in own {
        match piece {
            Piece::Param(index, _) => match arg_at(*index) {
                Some(value) => out.extend(value.pieces()),
                None => return absent(),
            },
            Piece::ParamKey(index, key, name) => match arg_at(*index) {
                Some(Value::Obj(object)) => match object.fields.get(key) {
                    Some(value) => out.extend(value.pieces()),
                    None if !object.open
                        && object.under_param.is_none()
                        && object.under_rest.is_none() =>
                    {
                        return absent();
                    }
                    None => return Fill::Unread,
                },
                // The caller hands its own parameter on whole: its key
                // waits on the caller's caller.
                Some(Value::Str(pieces)) => match pieces.as_slice() {
                    [Piece::Param(passed, _)] => {
                        out.push(Piece::ParamKey(*passed, key.clone(), name.clone()));
                    }
                    _ => return Fill::Unread,
                },
                Some(Value::Callback(_)) => return Fill::Unread,
                None => return absent(),
            },
            other => out.push(other.clone()),
        }
    }
    Fill::Pieces(super::concat([out]))
}

/// The slot `name` of `placed`'s call, as the body that writes it reads it
/// (an argument, or a key of an object-literal one).
fn slot_pieces(placed: &Placed<'_>, name: &(usize, Option<String>)) -> Result<Vec<Piece>, Prefix> {
    let (arg, key) = name;
    let value = match (placed.call.args.get(*arg), key) {
        (None, _) => return Err(Prefix::Unstated),
        (Some(value), None) => value,
        (Some(Value::Obj(object)), Some(key)) => match object.key(key) {
            KeyState::Known(value) => value,
            KeyState::Unread | KeyState::Function => return Err(Prefix::Unread { text: None }),
            KeyState::Absent => return Err(Prefix::Unstated),
        },
        (Some(_), Some(_)) => return Err(Prefix::Unread { text: None }),
    };
    Ok(value.pieces())
}

/// The ways the slot `name` of `placed`'s call reads, where the source
/// states more than one (a default or a conditional, carrick#2635).
fn slot_alternatives<'p>(
    placed: &'p Placed<'_>,
    name: &(usize, Option<String>),
) -> Option<&'p Vec<Way>> {
    let (arg, key) = name;
    let held = placed.call.instance.args.get(*arg)?;
    let ways = match key {
        None => &held.alts.whole,
        Some(key) => held.alts.keys.get(key)?,
    };
    (!ways.is_empty()).then_some(ways)
}

/// The paths a route's own path slot `name` states where the source writes
/// it through a conditional or a default (carrick#2386): each way the code
/// writes whole. `apiPrefix + "/x"` with `apiPrefix = base ? \`${base}api\` :
/// "/api"` is served at `/api/x` when `base` is unset; the way led by `base`
/// is a path the scan cannot read and adds nothing. Empty where no way is
/// written whole, or where more than [`MAX_PREFIX_VALUES`] are. Unlike a
/// prefix ([`InstanceWalk::prefix_of`]), one unread way does not unread the
/// rest: each value returned is a path the route is served at.
pub(super) fn whole_ways(placed: &Placed<'_>, name: &(usize, Option<String>)) -> Vec<String> {
    let Some(ways) = slot_alternatives(placed, name) else {
        return Vec::new();
    };
    let values: BTreeSet<String> = ways
        .iter()
        .filter_map(|way| literal(&way.pieces))
        .filter(|text| !text.is_empty())
        .collect();
    if values.len() > MAX_PREFIX_VALUES {
        return Vec::new();
    }
    values.into_iter().collect()
}

/// The most values one prefix slot states: beyond it the slot reads as
/// unread.
const MAX_PREFIX_VALUES: usize = 16;

/// How many callers up a parameter is followed.
const MAX_PREFIX_HOPS: usize = 6;

/// What reading one prefix slot has found so far (carrick#2635).
#[derive(Default)]
struct Reading {
    /// The definitions walked, with the parameter as their body reads it.
    walked: HashSet<(PathBuf, String, Vec<Piece>)>,
    /// The text stated.
    values: BTreeSet<String>,
    /// A value a default stands for is missing: a caller omits it, or none
    /// is known.
    missing: bool,
}

impl<'w, 'a> InstanceWalk<'w, 'a> {
    /// The prefix `placed`'s call states in the slot `name` (an argument, or
    /// a key of an object-literal one): every value it states, each way a
    /// default or a conditional reads, and a parameter as each caller (or
    /// the member that runs the function) fills it (carrick#2635). Text
    /// where every piece is literal. `origin` (`file:line` of the call that
    /// hands the instance in) narrows the callers and hosts to that one.
    pub(super) fn prefix_of(
        &self,
        placed: &Placed<'a>,
        name: &Option<(usize, Option<String>)>,
        origin: Option<&str>,
    ) -> Prefix {
        // A scope with no name binds no prefix (K1).
        let Some(name) = name else {
            return Prefix::Unstated;
        };
        let pieces = match slot_pieces(placed, name) {
            Ok(pieces) => pieces,
            Err(prefix) => return prefix,
        };
        if let Some(text) = literal(&pieces) {
            return Prefix::Stated { values: vec![text] };
        }
        let ways = slot_alternatives(placed, name)
            .cloned()
            .unwrap_or_else(|| vec![Way::stated(pieces.clone())]);
        let mut reading = Reading::default();
        if self.settle(*placed, ways, origin, &mut reading, 0)
            && !reading.values.is_empty()
            && reading.values.len() <= MAX_PREFIX_VALUES
        {
            return Prefix::Stated {
                values: reading.values.into_iter().collect(),
            };
        }
        Prefix::Unread {
            text: opaque_text(&pieces),
        }
    }

    /// What the slot `name` of `placed`'s call states as an address part's
    /// value (`docs/reference/address-chain.md`), where `prefix` is what
    /// [`Self::prefix_of`] read there: each stated value as its pieces, a
    /// piece read through a constant with the module that declares it, and
    /// a value not read as text as unknown pieces with why. A value read
    /// through the callers of a parameter is stated as its text.
    pub(super) fn value_of(
        &self,
        placed: &Placed<'a>,
        name: &Option<(usize, Option<String>)>,
        prefix: &Prefix,
    ) -> address::Value {
        let Some(name) = name else {
            return address::Value::Unstated;
        };
        match prefix {
            Prefix::Unstated => address::Value::Unstated,
            Prefix::Stated { values } => address::Value::Text {
                values: values
                    .iter()
                    .map(|value| {
                        self.folded_pieces(placed, name, value).unwrap_or_else(|| {
                            vec![address::Piece::Lit {
                                text: value.clone(),
                            }]
                        })
                    })
                    .collect(),
            },
            Prefix::Unread { text } => {
                let conditional = slot_alternatives(placed, name).is_some();
                let pieces = match slot_pieces(placed, name) {
                    Ok(pieces) if !pieces.is_empty() => pieces
                        .iter()
                        .map(|piece| address_piece(piece, conditional))
                        .collect(),
                    _ => vec![address::Piece::Unknown {
                        written: text.clone().unwrap_or_default(),
                        reason: address::UnknownReason::Unread,
                        reads: Vec::new(),
                    }],
                };
                address::Value::Text {
                    values: vec![pieces],
                }
            }
        }
    }

    /// The pieces of `value`, the text the slot `name` of `placed`'s call
    /// states, where that text holds a piece read through a constant
    /// ([`super::super::InstanceIr::folds`]); `None` where none does.
    fn folded_pieces(
        &self,
        placed: &Placed<'a>,
        name: &(usize, Option<String>),
        value: &str,
    ) -> Option<Vec<address::Piece>> {
        let (arg, key) = name;
        let fold = placed
            .call
            .instance
            .folds
            .iter()
            .find(|fold| fold.arg == *arg && fold.key == *key)?;
        let text: String = fold
            .pieces
            .iter()
            .map(|piece| match piece {
                FoldPiece::Lit(text) | FoldPiece::Folded { text, .. } => text.as_str(),
            })
            .collect();
        (text == value).then(|| {
            fold.pieces
                .iter()
                .map(|piece| match piece {
                    FoldPiece::Lit(text) => address::Piece::Lit { text: text.clone() },
                    FoldPiece::Folded { text, from } => address::Piece::Folded {
                        text: text.clone(),
                        from: relative(self.root, from),
                    },
                })
                .collect()
        })
    }

    /// Add the text each of `ways` (read in `placed`'s body) states to
    /// `values`. A parameter takes what each caller of its function hands
    /// in, and what each member that runs the function hands it; one
    /// nothing hands anything adds nothing (an entry point). False when a
    /// value is not text this pass reads.
    ///
    /// A default (`fallback`) is stated only where the value written before
    /// it is missing: a caller omits it, or no caller is known (`missing`).
    fn settle(
        &self,
        placed: Placed<'a>,
        ways: Vec<Way>,
        origin: Option<&str>,
        reading: &mut Reading,
        hops: usize,
    ) -> bool {
        for defaults in [false, true] {
            if defaults && !reading.missing {
                break;
            }
            for way in ways.iter().filter(|way| way.fallback == defaults) {
                if !self.settle_way(placed, &way.pieces, origin, reading, hops) {
                    return false;
                }
            }
        }
        true
    }

    /// [`Self::settle`] for one way.
    fn settle_way(
        &self,
        placed: Placed<'a>,
        way: &[Piece],
        origin: Option<&str>,
        reading: &mut Reading,
        hops: usize,
    ) -> bool {
        {
            if !holds_parameter(way) {
                return match literal(way) {
                    Some(text) => {
                        reading.values.insert(text);
                        true
                    }
                    None => false,
                };
            }
            let (Some(own), Some(key)) = (to_definition(way, placed.depth), placed.key) else {
                return false;
            };
            if hops >= MAX_PREFIX_HOPS {
                return false;
            }
            if !reading
                .walked
                .insert((placed.file.to_path_buf(), key.to_string(), own.clone()))
            {
                return true;
            }
            let from_origin = |file: &Path, call: &CallIr| {
                origin.is_none_or(|origin| origin == self.at_call(file, call))
            };
            let mut handed = false;
            for caller in self.calls.callers_of(placed.file, key) {
                if !from_origin(caller.file, caller.call) {
                    continue;
                }
                handed = true;
                let step = fill(&own, caller.call.site_args.is_some(), |index| {
                    caller.call.args.get(index)
                });
                if !self.settled(*caller, step, reading, hops) {
                    return false;
                }
            }
            for (host, arg) in self.hosts_of(placed.file, key) {
                if !from_origin(host.file, host.call) {
                    continue;
                }
                handed = true;
                // The member hands the function its scoped instance at
                // `param`; the parameters after it are the member's
                // arguments after the function's (`register(plugin,
                // options)` runs `plugin(instance, options)`).
                let Some(param) = self.scopes.iter().find_map(|label| match &label.binds {
                    ScopeBinds::Runs {
                        arg: runs,
                        key: None,
                        param,
                    } if label.member == host.call.callee && *runs == arg => Some(*param),
                    _ => None,
                }) else {
                    return false;
                };
                if super::parameter_positions(&own)
                    .iter()
                    .any(|position| *position <= param)
                {
                    return false;
                }
                let step = fill(&own, host.call.site_args.is_some(), |index| {
                    host.call.args.get(arg + index - param)
                });
                if !self.settled(host, step, reading, hops) {
                    return false;
                }
            }
            // Nothing is known to fill it: a default stands.
            if !handed {
                reading.missing = true;
            }
            true
        }
    }

    /// One caller's or host's fill of a parameter: absent adds nothing and
    /// leaves the default to stand, a value adds what it states.
    fn settled(&self, caller: Placed<'a>, step: Fill, reading: &mut Reading, hops: usize) -> bool {
        match step {
            Fill::Absent => {
                reading.missing = true;
                true
            }
            Fill::Unread => false,
            Fill::Pieces(pieces) => {
                self.settle(caller, vec![Way::stated(pieces)], None, reading, hops + 1)
            }
        }
    }

    /// Every call (`file:line`) that is a caller of the function `placed`
    /// is written in, or a member that runs it.
    fn handers(&self, placed: &Placed<'a>) -> Vec<String> {
        let Some(key) = placed.key else {
            return Vec::new();
        };
        let mut out: Vec<String> = self
            .calls
            .callers_of(placed.file, key)
            .iter()
            .map(|caller| self.at_call(caller.file, caller.call))
            .collect();
        out.extend(
            self.hosts_of(placed.file, key)
                .into_iter()
                .map(|(host, _)| self.at_call(host.file, host.call)),
        );
        out
    }

    /// `file:line` of `call`, as an instance end names the call that hands
    /// it in.
    pub(super) fn at_call(&self, file: &Path, call: &CallIr) -> String {
        format!("{}:{}", relative(self.root, file), call.site.line)
    }
}

/// One piece of a prefix slot this pass does not read as text, as an
/// address piece: text the call writes stays text, and anything else is
/// unknown with why. `conditional` says the slot is written through a
/// conditional or a default.
fn address_piece(piece: &Piece, conditional: bool) -> address::Piece {
    use address::UnknownReason;
    let unknown =
        |written: &str, reason: UnknownReason, reads: Vec<String>| address::Piece::Unknown {
            written: written.to_string(),
            reason,
            reads,
        };
    match piece {
        Piece::Lit(text) => address::Piece::Lit { text: text.clone() },
        Piece::Opaque {
            end: Root::Env { name, .. },
            text,
            ..
        } => unknown(text, UnknownReason::RuntimeEnv, vec![name.clone()]),
        _ if conditional => {
            let written = match piece {
                Piece::Opaque { text, .. } => text.as_str(),
                _ => "",
            };
            unknown(written, UnknownReason::Conditional, Vec::new())
        }
        Piece::Param(_, name) | Piece::ParamKey(_, _, name) | Piece::ParamKeyOr(_, _, name, _) => {
            unknown(name, UnknownReason::Parameter, Vec::new())
        }
        Piece::Outer(_, inner) => match &**inner {
            Piece::Param(_, name)
            | Piece::ParamKey(_, _, name)
            | Piece::ParamKeyOr(_, _, name, _) => {
                unknown(name, UnknownReason::Parameter, Vec::new())
            }
            _ => unknown("", UnknownReason::Parameter, Vec::new()),
        },
        Piece::Opaque {
            end: Root::Param(..) | Root::Outer(..),
            text,
            ..
        } => unknown(text, UnknownReason::Parameter, Vec::new()),
        Piece::Opaque { text, .. } => unknown(text, UnknownReason::Unread, Vec::new()),
        Piece::Query | Piece::Unknown => unknown("", UnknownReason::Unread, Vec::new()),
    }
}

/// The text of a value that is one opaque piece, as written.
fn opaque_text(pieces: &[Piece]) -> Option<String> {
    match pieces {
        [Piece::Opaque { text, .. }] => Some(text.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::super::discovery::AskedScope;
    use super::*;
    use crate::position_rows::Row;

    fn handed(package: &str, side: &str) -> InstanceEnd {
        InstanceEnd {
            at: "src/a.ts:1".to_string(),
            end: InstanceAt::Handed {
                file: "src/a.ts".to_string(),
                line: 1,
                package: package.to_string(),
                export: "Hub".to_string(),
                side: side.to_string(),
            },
        }
    }

    fn made(package: &str) -> InstanceEnd {
        InstanceEnd {
            at: "src/b.ts:2".to_string(),
            end: InstanceAt::Root {
                file: "src/b.ts".to_string(),
                line: 2,
                package: package.to_string(),
                export: "Peer".to_string(),
                member: None,
                receiver: "instance:new".to_string(),
                made: None,
            },
        }
    }

    fn hole() -> InstanceEnd {
        InstanceEnd {
            at: "src/c.ts:3".to_string(),
            end: InstanceAt::Hole {
                reason: InstanceHole::EntryPoint,
                text: None,
                boundary: None,
            },
        }
    }

    /// carrick#1670: the side a `by_origin` socket is on is where its
    /// instance came from.
    #[test]
    fn a_handed_instance_is_the_side_it_is_handed_in_on_and_a_made_one_is_the_client() {
        assert_eq!(origin_side(&[handed("rt", "server")], "rt"), Some("server"));
        assert_eq!(origin_side(&[handed("rt", "client")], "rt"), Some("client"));
        assert_eq!(origin_side(&[made("rt")], "rt"), Some("client"));
        // Ends that agree are one side.
        assert_eq!(
            origin_side(&[handed("rt", "server"), handed("rt", "server")], "rt"),
            Some("server")
        );
        assert_eq!(
            origin_side(&[handed("rt", "client"), made("rt")], "rt"),
            Some("client")
        );
    }

    /// carrick#1670: no end, a hole, an end of another package, a side the
    /// store does not state as client or server, or two sides: no side.
    #[test]
    fn an_origin_that_is_not_one_side_of_the_same_package_is_no_side() {
        assert_eq!(origin_side(&[], "rt"), None);
        assert_eq!(origin_side(&[handed("rt", "server"), hole()], "rt"), None);
        assert_eq!(origin_side(&[handed("other", "server")], "rt"), None);
        assert_eq!(origin_side(&[made("other")], "rt"), None);
        assert_eq!(origin_side(&[handed("rt", "both")], "rt"), None);
        assert_eq!(
            origin_side(&[handed("rt", "server"), made("rt")], "rt"),
            None
        );
        assert_eq!(
            origin_side(&[handed("rt", "server"), handed("other", "server")], "rt"),
            None
        );
    }

    /// carrick#2474 R2b: a scope label is the claim the trace verified, by
    /// claim id: a sibling claim on the same member that the verifier
    /// refuted gives no label, whatever its sibling was verified as.
    #[test]
    fn a_refuted_scope_claim_is_no_label_beside_a_verified_sibling() {
        let exports: Vec<crate::library_claims::ExportClaims> =
            serde_json::from_value(serde_json::json!([{
                "package": "fixture-server", "version": "2.1.0", "specifier": "fixture-server",
                "export": "default", "role": "server_framework",
                "claims": [
                    { "kind": "make", "form": "call", "member": null },
                    { "kind": "scope", "member": "use", "of": "instance:()",
                      "name": { "arg": 0 }, "mounts": { "arg": 1 } },
                    { "kind": "scope", "member": "use", "of": "instance:()",
                      "name": { "arg": 1 }, "mounts": { "arg": 0 } }
                ]
            }]))
            .expect("claims in the store's shape");
        let ids: Vec<String> = exports[0]
            .claims_with_scopes_by_id()
            .into_iter()
            .filter(|(id, _)| id.contains(":scope:"))
            .map(|(id, _)| id)
            .collect();
        assert_eq!(ids.len(), 2, "{ids:?}");
        // Only the claim that mounts the instance at 1 under the name at 0
        // is verified.
        let verified: HashMap<AskedScope, Vec<(String, u32)>> = HashMap::from([(
            AskedScope {
                package: "fixture-server".to_string(),
                export: "default".to_string(),
                claim_id: ids[0].clone(),
                receiver: "instance:()".to_string(),
            },
            Vec::new(),
        )]);
        let labels = ScopeLabel::from_exports(&exports, &verified);
        assert_eq!(labels.len(), 1, "{labels:#?}");
        assert_eq!(labels[0].name, Some((0, None)));
        assert_eq!(
            labels[0].binds,
            ScopeBinds::Mounts {
                arg: 1,
                key: None,
                handle: false
            }
        );
    }

    /// carrick#2474 R2b: a call row whose declaration the rows could not
    /// name a file for (flagged unnamed) is a library call; one with no
    /// declaration at all, or one in the repository, is not.
    #[test]
    fn a_call_row_with_an_unnamed_declaration_is_a_library_call() {
        let row = |decl_file: Option<&str>, flags: u16| {
            Row(
                Kind::Call,
                0,
                1,
                Some("register".to_string()),
                decl_file.map(str::to_string),
                decl_file.map(|_| 3),
                None,
                None,
                flags,
            )
        };
        assert!(library_row(&row(None, FLAG_DECLARATION_UNNAMED)));
        assert!(!library_row(&row(None, 0)));
        assert!(library_row(&row(
            Some("node_modules/fixture-server/index.d.ts"),
            0
        )));
        assert!(!library_row(&row(Some("src/server.ts"), 0)));
    }
}
