//! What a message role states through verified library claims
//! (carrick#1662, build ticket 5 of carrick#1616).
//!
//! The library store answers, per imported package, what each export is
//! (`role`, `side`) and how it is used (`makes`, `scopes`, `ops`, `reserved`,
//! `calls`, `patterns`). The scanner derives a claim id for every claim
//! ([`ExportClaims::claims_by_id`]), sends one check per claim and receiver
//! the service actually calls through ([`checks`]) to the type sidecar's
//! `verify_library_claims`, and reads every library site through the claims
//! that verified ([`read`]). The wire is pinned on carrick#1564 (comment
//! 5937606126, section 4) and its amendments 2 (comment 5939543981) and 3
//! (comment 5943318714); in-process rows are service-scoped (D4, comment
//! 5940360418).
//!
//! One row writer per role, and the role is the only branch:
//! - `broker` and `in_process_bus`: a `send` is a publisher row, a `receive`
//!   a subscriber row, and (broker) a definition, the export's maker called
//!   with a name and a handler, is a subscriber row;
//! - `socket`: the export's `side` and the op give the direction, and a
//!   `receive` is the listener;
//! - `server_framework`: only a `receive` op in the `decorate` form that
//!   speaks GraphQL (contract amendment 8): the decorated method is the
//!   producer of the root field its decorator names, else its own name;
//! - every other role, and a server framework's HTTP ops, state nothing here.
//!
//! A site states a row only when nothing contests its receiver (calls are
//! classified by the claims' own lists, amendment 2 B1), every receiver it
//! may be made through reads the same op and name ([`LibrarySite::fold`],
//! amendment 3), and the name is a literal that is neither a wildcard nor a
//! name the library emits itself. Where to send the claims, and how the
//! store's answer is parsed, is carrick#1664's.
//!
//! The rules are in `docs/reference/client-semantics.md`, "Message roles".

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::name_census::{LibraryNames, NameOutcome};
use crate::operation::{GraphqlOperationKind, PubsubRole, SocketDirection};
use crate::request_summary::decided_sites::reserved_on;
use crate::request_summary::site_record::kind_and_role;
use crate::request_summary::{
    CallerChain, LibrarySite, LibrarySites, MakerForm, MemberUse, MemberWire, NameFallback,
    NamePlace, On, PlacedName, Selector, SiteArg, SiteMaker, SiteNames, SiteReceiver, SlotName,
    Stop,
};
use crate::services::type_sidecar::{
    BoundName, ClaimOn, ClaimProtocol, ClaimSlot, Collects, LibraryCheck, LibraryClaim, LibraryOp,
    LibraryRole, MakeForm, NameEntry, NameScope, NameScopeKind, OpForm, OpName, ScopeForm,
    SemanticsResult, SemanticsVerdict,
};
use crate::site_rows::RowSide;

/// Which end of a socket an export makes. A classification the types
/// cannot carry: it only ever picks a direction, never a fact of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Client,
    Server,
    Both,
}

impl Side {
    /// The word the label and the row writers spell a side with.
    pub fn word(self) -> &'static str {
        match self {
            Side::Client => "client",
            Side::Server => "server",
            Side::Both => "both",
        }
    }
}

/// How an export's side is decided (label structure v2, carrick#2434). A
/// class one package uses on both ends (a socket a client constructs with a
/// URL, and a server is handed by its connection event) has no side of its
/// own: the store keeps its class label and `by_origin`, and the scanner
/// decides each receiver's side from how the code got it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideRule {
    ByOrigin,
}

/// What a listed call that is no op, scope or maker does to its receiver
/// (contract amendment 2, B1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallKind {
    /// Sends and receives nothing, and cannot change where later operations
    /// go (`quit`, a lifecycle `on("error")`).
    OffWire,
    /// Can change a name, a prefix, a base or a channel later operations on
    /// the same receiver use.
    Mutator,
}

/// One element of an export's `calls` list (contract amendment 2, B1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallClaim {
    #[serde(default)]
    pub member: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
    #[serde(default)]
    pub on: Option<ClaimOn>,
    #[serde(default)]
    pub of: Option<String>,
    pub kind: CallKind,
}

/// One export's claims, as the store answers them for the package and
/// version the service installed (contract section 2, as amendment 2 changes
/// it). `claims` holds the `makes`, `scopes`, `ops` and `reserved` lists,
/// each element tagged by its kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportClaims {
    /// The registry package.
    pub package: String,
    /// Its installed version.
    pub version: String,
    /// The specifier as the service imports it (`pkg` or `pkg/sub`).
    pub specifier: String,
    /// `"default"` or the named export.
    pub export: String,
    pub role: LibraryRole,
    /// The classifier's own option, kept beside the contract role it maps
    /// to (`request_runner` is an `http_client`). Carried, never branched on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub side: Option<Side>,
    /// Label v2: the side is decided per receiver from its origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_rule: Option<SideRule>,
    /// Label v2: the type-only packages whose declarations this package's
    /// surface re-exports (`@types/<name>` and types-only core packages). A
    /// declaration found in one of them is this package's
    /// ([`runtime_packages_of`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub type_packages: Vec<String>,
    #[serde(default)]
    pub claims: Vec<LibraryClaim>,
    #[serde(default)]
    pub calls: Vec<CallClaim>,
    /// The library's wildcard characters: a name holding one is a pattern,
    /// never one name.
    #[serde(default)]
    pub patterns: Vec<String>,
    /// Amendment 7: the `type:` ids each receiver id of this export holds
    /// (`{ "<receiver id>": ["type:…"] }`), as the store serves them. The
    /// parse boundary has already lowered the elements on those types onto the
    /// receiver ids ([`lower_type_receivers`]); the map is kept for the trace.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub receiver_types: BTreeMap<String, Vec<String>>,
}

/// The packages a declaration found in `declaring` stands for: every package
/// whose answered `type_packages` lists it, in answer order, or, when none
/// does, by the DefinitelyTyped convention, the package an `@types/*` name
/// types. Anything else is itself.
///
/// A type-only package is often in several packages' lists: a package whose
/// surface extends another's types lists the other's type packages too. Each
/// of them is a package the declaration may stand for, and the store's labels
/// on any of them may name it; which one holds is the verifier's to judge, so
/// none is picked here (carrick#2661: the first match took a package's own
/// declarations away from it).
pub fn runtime_packages_of(declaring: &str, exports: &[ExportClaims]) -> Vec<String> {
    let mut packages: Vec<String> = Vec::new();
    for export in exports
        .iter()
        .filter(|export| export.type_packages.iter().any(|types| types == declaring))
    {
        if !packages.contains(&export.package) {
            packages.push(export.package.clone());
        }
    }
    if packages.is_empty() {
        packages.push(crate::engine::type_compat_v2::types_package_runtime_name(
            declaring,
        ));
    }
    packages
}

/// The roles whose rows this module writes.
fn is_message_role(role: LibraryRole) -> bool {
    matches!(
        role,
        LibraryRole::Broker | LibraryRole::Socket | LibraryRole::InProcessBus
    )
}

/// A GraphQL root field written as a method decorator (contract amendment 8
/// on carrick#2434): a `receive` op in the `decorate` form that speaks
/// GraphQL.
fn is_graphql_decorator(claim: &LibraryClaim) -> bool {
    matches!(
        claim,
        LibraryClaim::Op {
            op: LibraryOp::Receive,
            protocol: Some(ClaimProtocol::Graphql),
            ..
        }
    ) && claim.is_decorate_form_op()
}

/// Whether this module reads the export's sites: a message role's, or a
/// server framework's GraphQL root-field decorators. The rest of a server
/// framework's claims (its HTTP routes) are read elsewhere.
fn is_read_export(export: &ExportClaims) -> bool {
    is_message_role(export.role)
        || (export.role == LibraryRole::ServerFramework
            && export.claims.iter().any(is_graphql_decorator))
}

/// Whether the claim is one this module reads on `export`: every claim of a
/// message role, and only the GraphQL decorators of a server framework.
fn is_read_claim(export: &ExportClaims, claim: &LibraryClaim) -> bool {
    is_message_role(export.role) || is_graphql_decorator(claim)
}

impl ExportClaims {
    /// Every claim with the id the scanner derives for it:
    /// `<specifier>@<major>:<export>:<kind>:…`, the convention HTTP claim
    /// ids follow (`<package>@<major>:<export>:factory:<member>`). A claim id
    /// stated twice, differently, names nothing one can verify, so both are
    /// dropped. Scope claims are left out: the library check reads no
    /// receiver a scope returns. The receiver trace does (carrick#1678): an
    /// op claimed on `<base>>scope:<member>` is read at the call made on what
    /// the scope member returned ([`scope_step`]).
    pub fn claims_by_id(&self) -> Vec<(String, &LibraryClaim)> {
        self.by_id(false)
    }

    /// [`Self::claims_by_id`] with the scope claims in too
    /// (`<prefix>:scope:<path.member>:name@<slot>:<binds>[:on/of…]`, where
    /// `<binds>` is `runs@<slot>><param>`, `mounts@<slot>` or `returns` and
    /// a slot is `<arg>` or `<arg>.<key>`; a procedure router's member spells
    /// `collects@<slot>/<join>` in the name's place, #2474 R6a, a merging
    /// member `merges@<arg>`, amendment 9, a pass-through `passes`, and a
    /// name-less binding scope its `<binds>` alone, where a handler slot
    /// spells `handle@<slot>`, K1): what the
    /// receiver trace sends in
    /// its own request (carrick#2474 R2b, carrick#1678), never the library
    /// check's.
    pub fn claims_with_scopes_by_id(&self) -> Vec<(String, &LibraryClaim)> {
        self.by_id(true)
    }

    fn by_id(&self, scopes: bool) -> Vec<(String, &LibraryClaim)> {
        let prefix = format!(
            "{}@{}:{}",
            self.specifier,
            major(&self.version),
            self.export
        );
        let mut stated: Vec<(String, &LibraryClaim)> = Vec::new();
        let mut twice: HashSet<String> = HashSet::new();
        for claim in &self.claims {
            if !scopes && matches!(claim, LibraryClaim::Scope { .. }) {
                continue;
            }
            // Written as a set or an override, or a scope written as a
            // decorator (amendment 7): no call site is written that way. An
            // op written as a construction of its export is read at the
            // `new` (carrick#3439), and one written as a method decorator
            // where the decorator is applied (carrick#3043).
            if !claim.is_call_form() && claim.op_site_form().is_none() {
                continue;
            }
            let Some(id) = claim_id(&prefix, claim) else {
                continue;
            };
            match stated.iter().find(|(known, _)| *known == id) {
                Some((_, known)) if *known != claim => {
                    twice.insert(id);
                }
                Some(_) => {}
                None => stated.push((id, claim)),
            }
        }
        stated.retain(|(id, _)| !twice.contains(id));
        stated
    }

    /// The maker claim, with its id, that builds `maker`'s instances: same
    /// form, same member.
    fn make_for<'c>(
        claims: &'c [(String, &'c LibraryClaim)],
        maker: &SiteMaker,
    ) -> Option<(&'c str, &'c LibraryClaim)> {
        claims.iter().find_map(|(id, claim)| match claim {
            LibraryClaim::Make { form, member, .. }
                if same_form(*form, maker.form) && *member == maker.member =>
            {
                Some((id.as_str(), *claim))
            }
            _ => None,
        })
    }

    /// What one member use on `receiver` is, by this export's own lists
    /// (contract amendment 2, B1): a claimed maker, op or scope member is on
    /// the wire; a listed `off_wire` call is off it; a `mutator` changes
    /// names; anything the lists do not name is unlisted, and contests.
    fn classify(&self, receiver: &str, used: &MemberUse) -> MemberWire {
        if used.form == MakerForm::Decorate {
            // A method decorator an op is written as (`@OnMessage("x")`,
            // carrick#3043). A decorator no claim is written as is unlisted.
            let claimed = self.claims.iter().any(|claim| match claim {
                LibraryClaim::Op {
                    member,
                    path,
                    on,
                    of,
                    ..
                } if claim.is_decorate_form_op() => {
                    applies(*on, of.as_deref(), receiver)
                        && *path == used.path
                        && *member == used.member
                }
                _ => false,
            });
            return if claimed {
                MemberWire::OnWire
            } else {
                MemberWire::Unlisted
            };
        }
        if used.form == MakerForm::New {
            // A maker the export is built with, or an op written as the
            // construction itself (`new SendMessageCommand({ ... })`).
            let made = self.claims.iter().any(|claim| match claim {
                LibraryClaim::Make {
                    form: MakeForm::New,
                    member,
                    ..
                } => receiver == "export" && used.path.is_empty() && *member == used.member,
                LibraryClaim::Op {
                    member,
                    path,
                    on,
                    of,
                    ..
                } if claim.is_new_form_op() => {
                    applies(*on, of.as_deref(), receiver)
                        && *path == used.path
                        && *member == used.member
                }
                _ => false,
            });
            return if made {
                MemberWire::OnWire
            } else {
                MemberWire::Unlisted
            };
        }
        let claimed = self
            .claims
            .iter()
            .filter(|claim| claim.is_call_form())
            .any(|claim| match claim {
                LibraryClaim::Make {
                    form: MakeForm::Call,
                    member,
                    ..
                } => receiver == "export" && used.path.is_empty() && *member == used.member,
                LibraryClaim::Op {
                    member,
                    path,
                    on,
                    of,
                    ..
                } => {
                    applies(*on, of.as_deref(), receiver)
                        && *path == used.path
                        && *member == used.member
                }
                LibraryClaim::Scope {
                    member,
                    path,
                    on,
                    of,
                    ..
                } => {
                    applies(*on, of.as_deref(), receiver)
                        && *path == used.path
                        && Some(member) == used.member.as_ref()
                }
                _ => false,
            });
        if claimed {
            return MemberWire::OnWire;
        }
        let listed = self.calls.iter().find(|call| {
            applies(call.on, call.of.as_deref(), receiver)
                && call.path == used.path
                && call.member == used.member
        });
        match listed.map(|call| call.kind) {
            Some(CallKind::OffWire) => MemberWire::OffWire,
            Some(CallKind::Mutator) => MemberWire::ChangesName,
            None => MemberWire::Unlisted,
        }
    }
}

/// The major version a claim id carries: the leading number of `version`,
/// or `x` when it has none.
fn major(version: &str) -> &str {
    let digits = version
        .trim_start_matches(['^', '~', '=', 'v'])
        .split('.')
        .next()
        .unwrap_or_default();
    if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
        digits
    } else {
        "x"
    }
}

/// `form` as the claims spell a maker's form, against the site's.
fn same_form(form: MakeForm, site: MakerForm) -> bool {
    matches!(
        (form, site),
        (MakeForm::Call, MakerForm::Call) | (MakeForm::New, MakerForm::New)
    )
}

/// Whether `of` names a type (`type:…` or `type`) rather than a receiver.
fn is_type_receiver(of: &str) -> bool {
    of == "type" || of.starts_with("type:")
}

/// Lower the elements on `type:` receivers onto the receiver ids the site
/// walk reads (carrick#2434 amendment 7, carrick#3049). For each `op`,
/// `scope` and `reserved` element whose `of` is a type, every receiver id of
/// `receiver_types` whose list holds that type gets a copy with `of` set to
/// the id and `on` absent; the original stays. Makers sit on `export` and are
/// never lowered. A copy whose claim id an earlier element or copy already
/// holds is not added; returns how many were not.
pub(crate) fn lower_type_receivers(
    claims: &mut Vec<LibraryClaim>,
    receiver_types: &BTreeMap<String, Vec<String>>,
) -> usize {
    if receiver_types.is_empty() {
        return 0;
    }
    let mut known: HashSet<String> = claims
        .iter()
        .filter_map(|claim| claim_id("", claim))
        .collect();
    let mut copies: Vec<LibraryClaim> = Vec::new();
    let mut shadowed = 0;
    for claim in claims.iter() {
        let (LibraryClaim::Op { of: Some(of), .. }
        | LibraryClaim::Scope { of: Some(of), .. }
        | LibraryClaim::Reserved { of: Some(of), .. }) = claim
        else {
            continue;
        };
        if !is_type_receiver(of) {
            continue;
        }
        for (receiver, types) in receiver_types {
            if !types.contains(of) {
                continue;
            }
            let mut copy = claim.clone();
            if let LibraryClaim::Op { on, of, .. }
            | LibraryClaim::Scope { on, of, .. }
            | LibraryClaim::Reserved { on, of, .. } = &mut copy
            {
                *on = None;
                *of = Some(receiver.clone());
            }
            let fresh = claim_id("", &copy).is_none_or(|id| known.insert(id));
            if fresh {
                copies.push(copy);
            } else {
                shadowed += 1;
            }
        }
    }
    claims.extend(copies);
    shadowed
}

/// The id of one claim under `prefix`.
fn claim_id(prefix: &str, claim: &LibraryClaim) -> Option<String> {
    let hops = |path: &[String], member: Option<&str>| {
        let mut hops: Vec<&str> = path.iter().map(String::as_str).collect();
        hops.push(member.unwrap_or("()"));
        hops.join(".")
    };
    let selector = |on: &Option<ClaimOn>, of: &Option<String>| match (on, of) {
        (Some(on), None) => format!(":on:{}", on_name(*on)),
        (None, Some(of)) => format!(":of:{of}"),
        (Some(on), Some(of)) => format!(":on:{}:of:{of}", on_name(*on)),
        (None, None) => String::new(),
    };
    match claim {
        LibraryClaim::Make { form, member, .. } => Some(format!(
            "{prefix}:make:{}:{}",
            match form {
                MakeForm::Call => "call",
                MakeForm::New => "new",
            },
            member.as_deref().unwrap_or("()")
        )),
        LibraryClaim::Op {
            op,
            member,
            path,
            on,
            of,
            form,
            when,
            ..
        } => Some(format!(
            "{prefix}:op:{}:{}{}{}{}",
            op_name(*op),
            hops(path, member.as_deref()),
            selector(on, of),
            match form {
                Some(form) if *form != OpForm::Call => format!(":form:{}", form.as_str()),
                _ => String::new(),
            },
            match when {
                Some(when) => format!(":when:{}", when.literal),
                None => String::new(),
            }
        )),
        // Asked only beside the claim it sits on, under that claim's id.
        LibraryClaim::Result { .. } => None,
        LibraryClaim::Reserved {
            member,
            name,
            path,
            on,
            of,
            ..
        } => Some(format!(
            "{prefix}:reserved:{}:{name}{}",
            hops(path, Some(member)),
            selector(on, of)
        )),
        LibraryClaim::Scope {
            member,
            form,
            name,
            path,
            on,
            of,
            runs,
            yields,
            mounts,
            collects,
            merges,
            passes,
            ..
        } => {
            // A slot that takes a handler spells `handle@` where one that
            // takes an instance spells `mounts@` (K1), so a claim and its
            // handle twin are two ids, not one stated twice.
            let mounted = |mounts: &ClaimSlot| {
                format!(
                    "{}@{}",
                    if mounts.handle { "handle" } else { "mounts" },
                    slot(mounts)
                )
            };
            // What it binds (carrick#2474 R0's fields): two scope claims of
            // one member on one receiver that read different slots are two
            // claims (carrick#2474 R2b).
            let binds = match (runs, yields, mounts) {
                (Some(runs), Some(yields), None) => {
                    format!("runs@{}>{}", slot(runs), yields.param)
                }
                (None, None, Some(mounts)) => mounted(mounts),
                (None, None, None) => "returns".to_string(),
                (runs, yields, mounts) => format!(
                    "runs@{}>{}:mounts@{}",
                    runs.as_ref().map(slot).unwrap_or_default(),
                    yields
                        .as_ref()
                        .map(|yields| yields.param.to_string())
                        .unwrap_or_default(),
                    mounts.as_ref().map(mounted).unwrap_or_default()
                ),
            };
            // A procedure router's member has no name (#2474 R6a, contract
            // amendment 3): its id spells the collection in the name's
            // place, so a named scope's id stays byte for byte as it was. A
            // claim with both reaches the verifier, which refuses it.
            let collected = |collects: &Collects| match &collects.key {
                Some(key) => format!("collects@{}.{key}/{}", collects.arg, collects.join),
                None => format!("collects@{}/{}", collects.arg, collects.join),
            };
            // Merged collections (amendment 9) spell `merges@<arg>` there.
            let named = match (name, collects, merges) {
                (Some(name), None, None) => format!("name@{}:{binds}", slot(name)),
                (None, Some(collects), None) => collected(collects),
                (Some(name), Some(collects), None) => {
                    format!("name@{}:{binds}:{}", slot(name), collected(collects))
                }
                (None, None, Some(merges)) => format!("merges@{}", slot(merges)),
                (Some(name), None, Some(merges)) => {
                    format!("name@{}:{binds}:merges@{}", slot(name), slot(merges))
                }
                (None, Some(collects), Some(merges)) => {
                    format!("{}:merges@{}", collected(collects), slot(merges))
                }
                (Some(name), Some(collects), Some(merges)) => format!(
                    "name@{}:{binds}:{}:merges@{}",
                    slot(name),
                    collected(collects),
                    slot(merges)
                ),
                // A name-less scope (K1): a pass-through spells `passes`;
                // one that binds spells its binding alone. One that does
                // neither names nothing.
                (None, None, None) if *passes => "passes".to_string(),
                (None, None, None) if binds != "returns" => binds,
                (None, None, None) => return None,
            };
            // A pass-through beside another field is refused by the
            // verifier; its id still holds the flag.
            let named = if *passes && named != "passes" {
                format!("{named}:passes")
            } else {
                named
            };
            Some(format!(
                "{prefix}:scope:{}:{named}{}{}",
                hops(path, Some(member)),
                selector(on, of),
                match form {
                    Some(ScopeForm::Decorate) => ":form:decorate",
                    Some(ScopeForm::Call) | None => "",
                }
            ))
        }
    }
}

/// A slot as a claim id spells it: `<arg>` or `<arg>.<key>`.
fn slot(slot: &ClaimSlot) -> String {
    match &slot.key {
        Some(key) => format!("{}.{key}", slot.arg),
        None => slot.arg.to_string(),
    }
}

fn on_name(on: ClaimOn) -> &'static str {
    match on {
        ClaimOn::Export => "export",
        ClaimOn::Instance => "instance",
        ClaimOn::Both => "both",
    }
}

fn op_name(op: LibraryOp) -> &'static str {
    match op {
        LibraryOp::Request => "request",
        LibraryOp::Send => "send",
        LibraryOp::Receive => "receive",
        LibraryOp::Execute => "execute",
    }
}

/// Whether an element with `on` or `of` applies to the receiver `receiver`
/// (contract amendment 2, B3): `of` names exactly one receiver, `on` the
/// export, every instance, or both. An element carrying both, or neither,
/// applies to nothing.
pub(crate) fn applies(on: Option<ClaimOn>, of: Option<&str>, receiver: &str) -> bool {
    let instance = receiver.starts_with("instance:");
    match (on, of) {
        (None, Some(of)) => of == receiver,
        (Some(ClaimOn::Export), None) => receiver == "export",
        (Some(ClaimOn::Instance), None) => instance,
        (Some(ClaimOn::Both), None) => receiver == "export" || instance,
        _ => false,
    }
}

/// The base receiver and the scope member of a receiver id that names what a
/// scope member returns (`instance:()>scope:at` is `instance:()` and `at`);
/// `None` for any other id (carrick#1678).
pub(crate) fn scope_step(receiver: &str) -> Option<(&str, &str)> {
    receiver.split_once(">scope:")
}

/// The site API's selector for a claim's own `on`, `of`, `path` and
/// `member`.
fn selector<'a>(
    on: Option<ClaimOn>,
    of: Option<&'a str>,
    path: &'a [String],
    member: Option<&'a str>,
    form: MakerForm,
) -> Selector<'a> {
    Selector {
        on: on.map(|on| match on {
            ClaimOn::Export => On::Export,
            ClaimOn::Instance => On::Instance,
            ClaimOn::Both => On::Both,
        }),
        of,
        path,
        member,
        form,
    }
}

/// The checks the service needs, one per claim and receiver it calls
/// through, in one request (the verifier judges an instance op together with
/// its maker): every maker claim on `export`, and every op and reserved name
/// on each receiver it applies to among those the service's sites are made
/// through ([`LibrarySite::receiver_ids`]). An export no site calls through
/// asks nothing.
pub fn checks(sites: &LibrarySites, exports: &[ExportClaims]) -> Vec<LibraryCheck> {
    let mut checks = Vec::new();
    for export in exports.iter().filter(|export| is_read_export(export)) {
        let receivers: BTreeSet<String> = sites
            .sites
            .iter()
            .filter(|site| site.specifier == export.specifier && site.export == export.export)
            .flat_map(LibrarySite::receiver_ids)
            .collect();
        if receivers.is_empty() {
            continue;
        }
        for (id, claim) in export.claims_by_id() {
            if !is_read_claim(export, claim) {
                continue;
            }
            let on: Vec<&str> = match claim {
                LibraryClaim::Make { .. } => vec!["export"],
                LibraryClaim::Op { on, of, .. } | LibraryClaim::Reserved { on, of, .. } => {
                    receivers
                        .iter()
                        .map(String::as_str)
                        .filter(|receiver| applies(*on, of.as_deref(), receiver))
                        .collect()
                }
                LibraryClaim::Scope { .. } | LibraryClaim::Result { .. } => Vec::new(),
            };
            for receiver in on {
                checks.push(LibraryCheck {
                    claim_id: id.clone(),
                    package: export.specifier.clone(),
                    export: export.export.clone(),
                    role: export.role,
                    receiver: receiver.to_string(),
                    claim: claim.without_result(),
                });
            }
        }
    }
    checks
}

/// The checks an `infer` request reads route handlers' parameters through
/// (carrick#2674): of every server export, its makers (on `export`) and
/// scopes, the receivers ops are read on, its `send` ops that carry a
/// payload slot, and its `receive` ops that say where the request is. Each
/// is checked on the one receiver its own `on` or `of` names: the sidecar
/// matches a handler parameter's members by declaration, so no call site
/// picks the receiver.
pub fn server_checks(exports: &[ExportClaims]) -> Vec<LibraryCheck> {
    let mut checks = Vec::new();
    for export in exports
        .iter()
        .filter(|export| export.role == LibraryRole::ServerFramework)
    {
        for (id, claim) in export.claims_with_scopes_by_id() {
            let receiver = match claim {
                LibraryClaim::Make { .. } => Some("export".to_string()),
                LibraryClaim::Scope { on, of, .. }
                | LibraryClaim::Op {
                    op: LibraryOp::Send,
                    payload: Some(_),
                    on,
                    of,
                    ..
                }
                | LibraryClaim::Op {
                    op: LibraryOp::Receive,
                    request: Some(_),
                    on,
                    of,
                    ..
                }
                | LibraryClaim::Op {
                    op: LibraryOp::Receive,
                    returns: Some(_),
                    on,
                    of,
                    ..
                } => own_receiver(*on, of.as_deref()),
                _ => None,
            };
            if let Some(receiver) = receiver {
                checks.push(LibraryCheck {
                    claim_id: id,
                    package: export.specifier.clone(),
                    export: export.export.clone(),
                    role: export.role,
                    receiver,
                    claim: claim.without_result(),
                });
            }
        }
    }
    checks
}

/// Per server package specifier, the type-only packages the store says its
/// surface re-exports (`type_packages`): where a reply's send members are
/// declared (carrick#2674).
pub fn server_type_packages(
    exports: &[ExportClaims],
) -> std::collections::BTreeMap<String, Vec<String>> {
    exports
        .iter()
        .filter(|export| {
            export.role == LibraryRole::ServerFramework && !export.type_packages.is_empty()
        })
        .map(|export| (export.specifier.clone(), export.type_packages.clone()))
        .collect()
}

/// The one receiver an element's own `on` or `of` names: `of` itself, or
/// the export for `on` `export` or `both`. `on` `instance` names no one
/// receiver.
fn own_receiver(on: Option<ClaimOn>, of: Option<&str>) -> Option<String> {
    match (on, of) {
        (None, Some(of)) => Some(of.to_string()),
        (Some(ClaimOn::Export | ClaimOn::Both), None) => Some("export".to_string()),
        _ => None,
    }
}

/// What a library row states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LibraryRowKind {
    /// A named message to or from a broker or an in-process bus: a
    /// subscriber registers a handler, a publisher sends.
    Pubsub(PubsubRole),
    /// A socket event: the side that registers is the listener.
    Socket {
        direction: SocketDirection,
        listener: bool,
    },
    /// A GraphQL root field written as a method decorator: the decorated
    /// method resolves it, so the row is a producer.
    Graphql(GraphqlOperationKind),
}

/// One row a verified library claim states at a call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryRow {
    pub file: PathBuf,
    /// The line of the member the call names: where the row is stated.
    pub line: u32,
    /// The call's span, in SWC numbering.
    pub span_start: u32,
    pub span_end: u32,
    pub kind: LibraryRowKind,
    /// The topic or event, exactly as the source writes it.
    pub name: String,
    /// `None`: no scope, the name crosses services ([`scoped`]).
    pub name_scope: Option<NameScope>,
    /// Every claim the row rests on, each maker of a set included, sorted.
    pub claim_ids: Vec<String>,
    /// A definition (`task({ id, run })`): where a model route at exactly
    /// this span is withdrawn.
    pub definition: bool,
}

impl LibraryRow {
    /// The row's `name_scope` as the index writes it (carrick#1663's
    /// [`carrick_match::NameScope`]): the claim's scope spelled `global` or
    /// `service`, and its namespace; `None` for a row that states no scope.
    pub fn wire_scope(&self) -> Option<carrick_match::NameScope> {
        self.name_scope.as_ref().map(wire_scope_of)
    }

    /// The operation key and the side of the index the row is on, as the
    /// engine writes it.
    pub fn operation(&self) -> (crate::operation::OperationKey, RowSide) {
        use crate::operation::OperationKey;
        match self.kind {
            LibraryRowKind::Pubsub(PubsubRole::Subscriber) => {
                (OperationKey::pubsub(self.name.clone()), RowSide::Endpoint)
            }
            LibraryRowKind::Pubsub(PubsubRole::Publisher) => {
                (OperationKey::pubsub(self.name.clone()), RowSide::Call)
            }
            LibraryRowKind::Socket {
                direction,
                listener,
            } => (
                OperationKey::socket(self.name.clone(), direction),
                if listener {
                    RowSide::Endpoint
                } else {
                    RowSide::Call
                },
            ),
            LibraryRowKind::Graphql(kind) => (
                OperationKey::graphql(kind, self.name.clone()),
                RowSide::Endpoint,
            ),
        }
    }
}

/// Every row a service's library sites state through verified claims.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryRows {
    pub rows: Vec<LibraryRow>,
    /// How the reader came by the name at each site it read a name slot of
    /// (carrick#3504 B10-0). Log-only: no row reads it.
    pub names: LibraryNames,
    /// Each verified site whose name slot the text reader reads as no name,
    /// with why (carrick#3504 B10-3), and each library call read and then
    /// dropped with no row and no other lead (carrick#3947, carrick#3946):
    /// a claim answered `failed` (`uncorroborated`), a check the verifier's
    /// time budget did not reach (`unanswered`), or a verified op whose name
    /// slot reads no name (`caller_arg_missing`, `unread_expression`,
    /// `unread`, `rows_missing`). Saved as unread sites
    /// ([`crate::unread_sites::from_scan`]), never a row.
    pub leads: Vec<LibraryLead>,
    /// The sites `read` would have read when verification returned no
    /// verdict at all: saved by the engine as unread sites, reason
    /// `unanswered`. Empty whenever a verdict came back.
    pub unanswered: Vec<LibrarySite>,
}

/// Every site a verified reading of `export` would read: a site of the
/// export that nothing in the service contests. The reading loop and the
/// no-verdict save both ask this.
fn readable_sites<'a>(
    sites: &'a LibrarySites,
    export: &'a ExportClaims,
) -> impl Iterator<Item = &'a LibrarySite> {
    sites
        .sites
        .iter()
        .filter(move |site| site.specifier == export.specifier && site.export == export.export)
        .filter(move |site| site.contest(|on, used| export.classify(on, used)).is_none())
}

/// A verified library site the reader states no name at (carrick#3504
/// B10-3), placed where the name would be stated: the library call, or the
/// outermost call of the caller chain the text reader read it through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryLead {
    pub file: PathBuf,
    pub line: u32,
    /// In SWC numbering.
    pub span_start: u32,
    pub span_end: u32,
    pub reason: Stop,
    /// The package, export and member the labelled library call names.
    pub package: String,
    pub export: String,
    pub member: Option<String>,
    /// The protocol (`pubsub`, `socket`, `graphql`), when the role says one.
    pub kind: Option<&'static str>,
    /// `producer` or `consumer`, when the op says one.
    pub role: Option<&'static str>,
}

/// Read every library site through the claims that verify
/// ([`checks`], then `verify`). `verify` answers one verdict per check, in
/// order; an error, or any other answer, states nothing. No message-role
/// export, or no site through one, asks nothing. A name slot is read
/// through `names` (carrick#3504 B10-3); with none, as each site's own
/// arguments state it.
pub fn read(
    sites: &LibrarySites,
    exports: &[ExportClaims],
    names: Option<&SiteNames<'_>>,
    verify: impl FnOnce(&[LibraryCheck]) -> Result<Vec<SemanticsResult>, String>,
) -> LibraryRows {
    let checks = checks(sites, exports);
    if checks.is_empty() {
        return LibraryRows::default();
    }
    // A verdict counts only for the check at its own position that it names
    // (the sidecar client also refuses an answer of another length).
    let verdicts = match verify(&checks) {
        Ok(verdicts) => verdicts,
        Err(error) => {
            warn!(%error, "library claims: verification failed; no rows");
            let unanswered: Vec<LibrarySite> = exports
                .iter()
                .filter(|export| is_read_export(export))
                .flat_map(|export| readable_sites(sites, export))
                .cloned()
                .collect();
            debug!(
                saved = unanswered.len(),
                "library claims: no verdict; saved sites"
            );
            return LibraryRows {
                unanswered,
                ..LibraryRows::default()
            };
        }
    };
    let verified: HashSet<(String, String)> = checks
        .iter()
        .zip(&verdicts)
        .filter(|(check, verdict)| {
            verdict.verdict == SemanticsVerdict::Verified
                && verdict.claim_id == check.claim_id
                && verdict.receiver == check.receiver
        })
        .map(|(check, _)| (check.claim_id.clone(), check.receiver.clone()))
        .collect();
    // The word each check that did not verify saves a dropped call with
    // (carrick#3947, carrick#3946), where the verdict at its position names
    // it: `failed` is `uncorroborated`, a check the verifier's time budget
    // did not reach `unanswered`. Any other `unchecked` saves nothing.
    let unverified_words: HashMap<(String, String), Stop> = checks
        .iter()
        .zip(&verdicts)
        .filter(|(check, verdict)| {
            verdict.claim_id == check.claim_id && verdict.receiver == check.receiver
        })
        .filter_map(|(check, verdict)| {
            let word = if verdict.verdict == SemanticsVerdict::Failed {
                Stop::Uncorroborated
            } else if verdict.not_reached_in_time() {
                Stop::Unanswered
            } else {
                return None;
            };
            Some(((check.claim_id.clone(), check.receiver.clone()), word))
        })
        .collect();
    let mut stated: Vec<(LibraryRow, ReceiverKey)> = Vec::new();
    let mut outcomes = LibraryNames::default();
    let mut leads: Vec<LibraryLead> = Vec::new();
    for export in exports.iter().filter(|export| is_read_export(export)) {
        let claims = export.claims_by_id();
        let of_export: Vec<&LibrarySite> = readable_sites(sites, export).collect();
        // The reading of the site at `index`, at `at`; with `today`, as
        // today's reading reads it (no text reader).
        let reading = |index: usize, at: Option<&(NamePlace, String)>, today: bool| Reading {
            site: of_export[index],
            export,
            claims: &claims,
            verified: &verified,
            names: if today { None } else { names },
            at: at.cloned(),
            outcome: Cell::new(None),
            placed: RefCell::new(BTreeSet::new()),
            leads: RefCell::new(Vec::new()),
            led: RefCell::new(Vec::new()),
            sites: &sites.sites,
            unverified: RefCell::new(Vec::new()),
            unnamed: Cell::new(None),
        };
        // Library calls first: each states its rows at itself and at the
        // outermost call of each caller chain the text reader read the name
        // through, and its leads. `named` is every name a library call
        // states at a caller place, `led` every caller chain it saves a lead
        // at the outermost call of (two chains can end at one call: one
        // chain's lead never stands for another's).
        let mut read_sites: Vec<SiteRead> = vec![SiteRead::default(); of_export.len()];
        let mut named: HashSet<(PathBuf, u32, PathBuf, u32, String)> = HashSet::new();
        let mut led: HashSet<(PathBuf, u32, CallerChain)> = HashSet::new();
        for (index, site) in of_export.iter().enumerate() {
            if site.origin.is_some() {
                continue;
            }
            let here = reading(index, None, false);
            let mut rows: Vec<LibraryRow> = here.row().into_iter().collect();
            for place in here.placed.take() {
                rows.extend(reading(index, Some(&place), false).row());
            }
            let site_leads = here.leads.take();
            let call = (site.file.clone(), site.span_start);
            for row in &rows {
                named.insert((
                    call.0.clone(),
                    call.1,
                    row.file.clone(),
                    row.span_start,
                    row.name.clone(),
                ));
            }
            for chain in here.led.take() {
                led.insert((call.0.clone(), call.1, chain));
            }
            let unstated = here
                .unstated(&unverified_words)
                .map(|(reason, op)| here.lead_of(None, reason, op));
            read_sites[index] = SiteRead {
                rows,
                outcome: here.outcome.get(),
                leads: site_leads,
                unstated,
            };
        }
        // Then each site a caller fills. Where its library call states no
        // lead on its chain and not today's name there (a part the claim
        // positions is written at the caller, not at the library call:
        // `send({ topic, messages })` handed on whole; or a chain that stops
        // nearer the library call than this caller on a hole today's reading
        // does not answer, its lead saved where it stops), today's reading
        // of the caller stands, counted `caller_today`.
        for (index, site) in of_export.iter().enumerate() {
            let Some(origin) = &site.origin else {
                continue;
            };
            let here = reading(index, None, false);
            let mut read = SiteRead {
                rows: here.row().into_iter().collect(),
                outcome: here.outcome.get(),
                leads: here.leads.take(),
                unstated: None,
            };
            let on_chain = (origin.file.clone(), origin.span_start, site.chain.clone());
            if read.rows.is_empty() && names.is_some() && !led.contains(&on_chain) {
                let today = reading(index, None, true);
                if let Some(row) = today.row() {
                    let stated = (
                        origin.file.clone(),
                        origin.span_start,
                        site.file.clone(),
                        site.span_start,
                        row.name.clone(),
                    );
                    if named.contains(&stated) {
                        // Another chain states the same name at this call:
                        // no row is lost, so no `lead_today_stated`.
                        if read.outcome == Some(NameOutcome::LeadTodayStated) {
                            read.outcome = Some(NameOutcome::Filled);
                        }
                    } else {
                        read.rows = vec![row];
                        read.outcome = Some(NameOutcome::CallerToday);
                    }
                }
            }
            read_sites[index] = read;
        }
        // The library calls a caller they fill states a row at.
        let filled: HashSet<(&PathBuf, u32)> = of_export
            .iter()
            .zip(&read_sites)
            .filter(|(_, read)| !read.rows.is_empty())
            .filter_map(|(site, _)| site.origin.as_ref())
            .map(|origin| (&origin.file, origin.span_start))
            .collect();
        for (site, read) in of_export.iter().zip(&read_sites) {
            // A library call read and dropped (carrick#3947, carrick#3946):
            // no row here or at a caller, and no lead.
            let Some(lead) = &read.unstated else {
                continue;
            };
            if read.rows.is_empty()
                && read.leads.is_empty()
                && !filled.contains(&(&site.file, site.span_start))
                && !leads.contains(lead)
            {
                leads.push(lead.clone());
            }
        }
        for (site, read) in of_export.iter().zip(read_sites) {
            if let Some(outcome) = read.outcome {
                outcomes.note(outcome);
            }
            for row in read.rows {
                stated.push((row, ReceiverKey::of(site)));
            }
            for lead in read.leads {
                if !leads.contains(&lead) {
                    leads.push(lead);
                }
            }
        }
    }
    LibraryRows {
        rows: heard_in_process(stated, exports),
        names: outcomes,
        leads,
        unanswered: Vec::new(),
    }
}

/// The receiver a row was read through, for the in-process rule: the
/// export, or the makers of its instance by where each is written.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ReceiverKey {
    specifier: String,
    export: String,
    makers: Vec<(PathBuf, u32)>,
}

impl ReceiverKey {
    fn of(site: &LibrarySite) -> Self {
        ReceiverKey {
            specifier: site.specifier.clone(),
            export: site.export.clone(),
            makers: site
                .makers()
                .iter()
                .map(|maker| (maker.file.clone(), maker.span_start))
                .collect(),
        }
    }
}

/// The rows, less every in-process subscriber no emitter of the same
/// literal on the same receiver in the service speaks to (design section
/// 9): a listener to a name only the library emits, or one nothing here
/// sends, states no contract.
fn heard_in_process(
    stated: Vec<(LibraryRow, ReceiverKey)>,
    exports: &[ExportClaims],
) -> Vec<LibraryRow> {
    let in_process = |key: &ReceiverKey| {
        exports.iter().any(|export| {
            export.role == LibraryRole::InProcessBus
                && export.specifier == key.specifier
                && export.export == key.export
        })
    };
    let sent: HashSet<(&ReceiverKey, &str)> = stated
        .iter()
        .filter(|(row, _)| row.kind == LibraryRowKind::Pubsub(PubsubRole::Publisher))
        .map(|(row, key)| (key, row.name.as_str()))
        .collect();
    let heard: Vec<bool> = stated
        .iter()
        .map(|(row, key)| {
            row.kind != LibraryRowKind::Pubsub(PubsubRole::Subscriber)
                || !in_process(key)
                || sent.contains(&(key, row.name.as_str()))
        })
        .collect();
    stated
        .into_iter()
        .zip(heard)
        .filter_map(|((row, _), heard)| heard.then_some(row))
        .collect()
}

/// What [`read`] read at one site: its rows, how its name slot was read,
/// and its leads.
#[derive(Debug, Clone, Default)]
struct SiteRead {
    rows: Vec<LibraryRow>,
    outcome: Option<NameOutcome>,
    leads: Vec<LibraryLead>,
    /// At a library call: the site saved when the call states no row and
    /// no lead, and no caller it fills states one (carrick#3947).
    unstated: Option<LibraryLead>,
}

/// One site, read through its export's claims.
struct Reading<'a, 'n> {
    site: &'a LibrarySite,
    export: &'a ExportClaims,
    claims: &'a [(String, &'a LibraryClaim)],
    verified: &'a HashSet<(String, String)>,
    /// The text reader's reading of a name slot (B10-3).
    names: Option<&'a SiteNames<'n>>,
    /// `None`: the reading at the library call itself. Otherwise the
    /// outermost call of a caller chain the text reader read the name
    /// through, with the name read there: only that name is stated, and
    /// the row is stated there. Two chains that settle at one call with two
    /// names are two readings.
    at: Option<(NamePlace, String)>,
    /// How the first name slot the reading reached was read (B10-0).
    outcome: Cell<Option<NameOutcome>>,
    /// The places, each with a name, the name slots read at the library
    /// call name at a caller.
    placed: RefCell<BTreeSet<(NamePlace, String)>>,
    /// The leads the reading at the library call saved.
    leads: RefCell<Vec<LibraryLead>>,
    /// The caller chains the reading at the library call saved a lead at
    /// the outermost call of.
    led: RefCell<Vec<CallerChain>>,
    /// Every site of the service: the callers today's reading fills are
    /// sites of their own ([`LibrarySite::origin`]).
    sites: &'a [LibrarySite],
    /// Each check of a claim that would state a message row here and did
    /// not verify, with the claim's op (carrick#3947): why a call the
    /// reader reads may state no row.
    unverified: RefCell<Vec<(String, String, LibraryOp)>>,
    /// The word for the first name slot a verified op read no name at,
    /// saving no lead, with the op (carrick#3947).
    unnamed: Cell<Option<(Stop, LibraryOp)>>,
}

impl Reading<'_, '_> {
    fn verified(&self, id: &str, receiver: &str) -> bool {
        self.verified
            .contains(&(id.to_string(), receiver.to_string()))
    }

    /// Notes how a name slot was read, unless one is noted already.
    fn noted(&self, outcome: NameOutcome) {
        if self.outcome.get().is_none() {
            self.outcome.set(Some(outcome));
        }
    }

    /// Notes a check of the claim `id` on `receiver`, for `op`, that did not
    /// verify.
    fn unverified(&self, id: &str, receiver: &str, op: LibraryOp) {
        self.unverified
            .borrow_mut()
            .push((id.to_string(), receiver.to_string(), op));
    }

    /// Notes the word for a name slot read as no name, unless one is noted.
    fn unnamed(&self, reason: Stop, op: LibraryOp) {
        if self.unnamed.get().is_none() {
            self.unnamed.set(Some((reason, op)));
        }
    }

    /// Why the call states no row, with the op the reason is for, when the
    /// reader read it and dropped it (carrick#3947, carrick#3946): a name
    /// slot a verified op read no name at, then a check the verifier's time
    /// budget did not reach (`unanswered`), then a check answered `failed`
    /// (`uncorroborated`), as `words` spells each check. `None` when no
    /// claim that states a message row applied here, or none of its checks
    /// has a word.
    fn unstated(&self, words: &HashMap<(String, String), Stop>) -> Option<(Stop, LibraryOp)> {
        if let Some(unnamed) = self.unnamed.get() {
            return Some(unnamed);
        }
        let unverified = self.unverified.borrow();
        let worded: Vec<(Stop, LibraryOp)> = unverified
            .iter()
            .filter_map(|(id, receiver, op)| {
                words
                    .get(&(id.clone(), receiver.clone()))
                    .map(|word| (*word, *op))
            })
            .collect();
        worded
            .iter()
            .find(|(word, _)| *word == Stop::Unanswered)
            .or_else(|| worded.first())
            .copied()
    }

    /// The op of the first claim on `through` that would state a message
    /// row at this call: what a maker that did not verify keeps from it.
    fn message_op(&self, through: Option<&SiteMaker>) -> Option<LibraryOp> {
        let side = self.export.side.filter(|_| self.export.side_rule.is_none());
        self.claims.iter().find_map(|(_, claim)| {
            let LibraryClaim::Op {
                op,
                member,
                path,
                on,
                of,
                payload,
                handler,
                ack,
                protocol,
                method,
                ..
            } = claim
            else {
                return None;
            };
            let selected = selector(
                *on,
                of.as_deref(),
                path,
                member.as_deref(),
                claim.op_site_form().unwrap_or(MakerForm::Call),
            );
            (self.site.selected_by(through, &selected)
                && row_kind(self.export.role, *op, side, *protocol, method.as_deref()).is_some()
                && [payload, handler, ack]
                    .into_iter()
                    .flatten()
                    .all(|slot| self.site.supplies(slot_arg(slot), slot_key(slot))))
            .then_some(*op)
        })
    }

    /// The row the site states, if any: a definition, or an op read the same
    /// through every receiver the call may be made through.
    fn row(&self) -> Option<LibraryRow> {
        if let Some(row) = self.definition() {
            return Some(row);
        }
        let mut claim_ids: BTreeSet<String> = BTreeSet::new();
        let (kind, name, name_scope) = self.site.fold(|through| {
            let read = self.through(through)?;
            claim_ids.extend(read.claim_ids);
            Some((read.kind, read.name, read.name_scope))
        })?;
        Some(self.stated(kind, name, name_scope, claim_ids, false))
    }

    fn stated(
        &self,
        kind: LibraryRowKind,
        name: String,
        name_scope: Option<NameScope>,
        claim_ids: BTreeSet<String>,
        definition: bool,
    ) -> LibraryRow {
        let (file, line, span_start, span_end) = self.place();
        LibraryRow {
            file,
            line,
            span_start,
            span_end,
            kind,
            name,
            name_scope,
            claim_ids: claim_ids.into_iter().collect(),
            definition,
        }
    }

    /// A broker definition: the export's own maker, called with a name and a
    /// handler (`task({ id, run })`), registers the handler under the name,
    /// so it is the subscriber.
    fn definition(&self) -> Option<LibraryRow> {
        if self.export.role != LibraryRole::Broker
            || self.site.receiver != SiteReceiver::Export
            || self.site.form != MakerForm::Call
            || !self.site.path.is_empty()
        {
            return None;
        }
        let (id, name_slot, handler, name_scope) =
            self.claims.iter().find_map(|(id, claim)| match claim {
                LibraryClaim::Make {
                    form: MakeForm::Call,
                    member,
                    name: Some(name),
                    handler: Some(handler),
                    runs: None,
                    name_scope,
                    ..
                } if *member == self.site.member => Some((id, name, handler, name_scope)),
                _ => None,
            })?;
        if !self.site.supplies(slot_arg(handler), slot_key(handler)) {
            return None;
        }
        if !self.verified(id, "export") {
            self.unverified(id, "export", LibraryOp::Receive);
            return None;
        }
        let name = self.name(slot_arg(name_slot), slot_key(name_slot), LibraryOp::Receive)?;
        if !self.name_ok(&name, (Some(ClaimOn::Export), &None)) {
            return None;
        }
        Some(self.stated(
            LibraryRowKind::Pubsub(PubsubRole::Subscriber),
            name,
            scoped(self.export.role, name_scope.clone()),
            BTreeSet::from([id.clone()]),
            true,
        ))
    }

    /// What the call states made through one receiver (`None`: the export):
    /// the receiver's maker verified, and every op element that applies to
    /// it and verified reading one op and one name. `None` when its maker or
    /// every applicable op is unverified, or the elements disagree.
    fn through(&self, through: Option<&SiteMaker>) -> Option<Through> {
        let receiver = through.map_or_else(|| "export".to_string(), SiteMaker::receiver_id);
        let mut claim_ids: Vec<String> = Vec::new();
        let maker = match through {
            Some(maker) => {
                let (id, make) = ExportClaims::make_for(self.claims, maker)?;
                if !self.verified(id, "export") {
                    if let Some(op) = self.message_op(through) {
                        self.unverified(id, "export", op);
                    }
                    return None;
                }
                claim_ids.push(id.to_string());
                Some((maker, make))
            }
            None => None,
        };
        let mut read: Option<(LibraryRowKind, String, Option<NameScope>)> = None;
        for (id, claim) in self.claims {
            let LibraryClaim::Op {
                op,
                member,
                path,
                on,
                of,
                name,
                payload,
                handler,
                ack,
                name_scope,
                protocol,
                method,
                ..
            } = claim
            else {
                continue;
            };
            let selected = selector(
                *on,
                of.as_deref(),
                path,
                member.as_deref(),
                claim.op_site_form().unwrap_or(MakerForm::Call),
            );
            if !self.site.selected_by(through, &selected) {
                continue;
            }
            // A `by_origin` export has no side of its own (see `row_kind`).
            let side = self.export.side.filter(|_| self.export.side_rule.is_none());
            let kind = row_kind(self.export.role, *op, side, *protocol, method.as_deref());
            // Every other part the claim positions must be in the call: an
            // overload with fewer arguments can put the payload where the
            // name was claimed.
            let supplied = [payload, handler, ack]
                .into_iter()
                .flatten()
                .all(|slot| self.site.supplies(slot_arg(slot), slot_key(slot)));
            if !self.verified(id, &receiver) {
                // Only a claim that would state a message row here is a
                // reason the call states none (carrick#3947).
                if kind.is_some() && supplied {
                    self.unverified(id, &receiver, *op);
                }
                continue;
            }
            if !supplied {
                return None;
            }
            // Read before the name: an op that is no message row saves no
            // lead.
            let kind = kind?;
            let (text, scope) = match name.as_deref()? {
                OpName::Slot(slot) => (
                    self.name(slot_arg(slot), slot_key(slot), *op)?,
                    name_scope.clone(),
                ),
                // A maker-bound name is the one the maker was handed, where
                // it is written (B10-3, Q-B3b): it is stated at the library
                // call only.
                OpName::Bound {
                    bound: BoundName::Maker,
                    ..
                } => {
                    let (maker, make) = maker?;
                    let LibraryClaim::Make {
                        name: Some(slot),
                        name_scope,
                        ..
                    } = make
                    else {
                        return None;
                    };
                    if self.at.is_some() {
                        return None;
                    }
                    self.noted(match maker.literal(slot_arg(slot), slot_key(slot)) {
                        Some(_) => NameOutcome::Made,
                        None if maker.args.get(slot_arg(slot)).is_some_and(|arg| arg.spread) => {
                            NameOutcome::StopSpread
                        }
                        None => NameOutcome::StopUnread,
                    });
                    let Some(made) = maker.literal(slot_arg(slot), slot_key(slot)) else {
                        self.unnamed(slot_stop(&maker.args, slot_arg(slot), slot_key(slot)), *op);
                        return None;
                    };
                    (made.to_string(), name_scope.clone())
                }
                // The name of the member a decorator is applied to
                // (amendment 8), stated at the library call only.
                OpName::Bound {
                    bound: BoundName::Decorated,
                    ..
                } => {
                    if self.at.is_some() {
                        return None;
                    }
                    self.noted(match self.site.decorated {
                        Some(_) => NameOutcome::Literal,
                        None => NameOutcome::StopUnread,
                    });
                    // A computed key names nothing.
                    let Some(decorated) = self.site.decorated.clone() else {
                        self.unnamed(Stop::UnreadExpression, *op);
                        return None;
                    };
                    (decorated, None)
                }
                // The first entry the call fills (amendment 8).
                OpName::First(first) => (self.first(&first.first, *op)?, None),
                // A name a scope member binds is carrick#1678's; one a
                // router key binds is #2474 R6c's.
                // The names amendment 7 adds are read from H3 on.
                OpName::Bound { .. } | OpName::Parts { .. } | OpName::Literal { .. } => {
                    return None;
                }
            };
            if !self.name_ok(&text, (*on, of)) {
                return None;
            }
            // A GraphQL field pairs by its root type and name under a served
            // schema: it carries no scope.
            let scope = match kind {
                LibraryRowKind::Graphql(_) => None,
                _ => scoped(self.export.role, scope),
            };
            let this = (kind, text, scope);
            match &read {
                Some(seen) if *seen != this => return None,
                _ => read = Some(this),
            }
            claim_ids.push(id.clone());
        }
        let (kind, name, name_scope) = read?;
        Some(Through {
            kind,
            name,
            name_scope,
            claim_ids,
        })
    }

    /// Where this reading states a row: the library call, or the caller
    /// chain's outermost call it reads (file, line, SWC span).
    fn place(&self) -> (PathBuf, u32, u32, u32) {
        match &self.at {
            Some((at, _)) => (at.file.clone(), at.line, at.span_start, at.span_end),
            None => (
                self.site.file.clone(),
                self.site.line,
                self.site.span_start,
                self.site.span_end,
            ),
        }
    }

    /// The name the slot `arg` (its entry `key`) states at this reading's
    /// place (carrick#3504 B10-3), read through the text reader
    /// ([`SiteNames`]). At the library call, a name the text reader places
    /// at a caller is noted there for its own reading, and a value it
    /// states no name for is saved as a lead. A slot the text reader leaves
    /// to today's reading, and every slot read without it, reads the site's
    /// own arguments. A site a caller fills is its library call again, so
    /// a slot the text reader reads states nothing there.
    fn name(&self, arg: usize, key: Option<&str>, op: LibraryOp) -> Option<String> {
        let today = || self.site.literal(arg, key).map(str::to_string);
        let read = match read_slot(self.site, arg, key) {
            // The call does not fill the slot, or fills it with no name.
            SlotRead::Next => None,
            _ => self.names.and_then(|names| names.read(self.site, arg, key)),
        };
        let placed = match read {
            Some(SlotName::Read(placed)) => placed,
            today_read => {
                if self.at.is_some() {
                    return None;
                }
                self.noted(match today_read {
                    Some(SlotName::Today(NameFallback::NoRows(_))) => NameOutcome::NoRows,
                    _ => site_outcome(self.site, arg, key),
                });
                let name = today();
                if name.is_none() {
                    self.unnamed(
                        match today_read {
                            Some(SlotName::Today(NameFallback::NoRows(reason))) => {
                                Stop::from(reason)
                            }
                            _ => slot_stop(&self.site.args, arg, key),
                        },
                        op,
                    );
                }
                return name;
            }
        };
        if self.site.origin.is_some() {
            if self.at.is_some() {
                return None;
            }
            let text = today()?;
            // Only the text reader's value on this site's own caller chain
            // decides here: read out to this call (`own`), or stopped nearer
            // the library call (`short`: a prefix of the chain, placed where
            // it stopped, or the library call's own value, the empty chain).
            // Another chain that ends at this call decides nothing.
            let own = |value: &PlacedName| {
                value.chain == self.site.chain
                    && value.place.as_ref().is_some_and(|place| {
                        place.file == self.site.file && place.span_start == self.site.span_start
                    })
            };
            let short = |value: &PlacedName| {
                (value.place.is_some() || value.chain.is_empty())
                    && value.chain.len() < self.site.chain.len()
                    && self.site.chain.starts_with(&value.chain)
            };
            // Stopped short: the value nearest this call (the empty chain
            // is a prefix of every chain).
            let value = placed.iter().find(|value| own(value)).or_else(|| {
                placed
                    .iter()
                    .filter(|value| short(value))
                    .max_by_key(|value| value.chain.len())
            });
            match value.map(|value| (own(value), &value.name)) {
                // The library call's reading states today's name here itself.
                Some((true, Ok(name))) if *name == text => return None,
                // The reader's name differs from today's here: today's name
                // stands, counted (Q-B3f), and the place is logged with both
                // names for the compare to name.
                Some((true, Ok(name))) => {
                    tracing::debug!(
                        file = %self.site.file.display(),
                        line = self.site.line,
                        reader = %name,
                        today = %text,
                        "library names: the text reader's name differs from today's"
                    );
                    self.noted(NameOutcome::TextDiffers);
                    return Some(text);
                }
                // The chain stops in a hole today's reading answers (an own
                // builder written on the way, the step cap), here or one call
                // nearer the library call: today's reading stands here.
                Some((_, Err(reason))) => {
                    if let Some(answered) = today_answers(self.site, *reason, arg, key) {
                        self.noted(answered);
                        return Some(text);
                    }
                }
                _ => {}
            }
            // A caller today's reading fills, where the text reader states no
            // name on its chain here: counted as the row it does not keep
            // (`caller_today` below restates it unless the chain's lead is
            // saved here).
            self.noted(NameOutcome::LeadTodayStated);
            return None;
        }
        if let Some((at, name)) = &self.at {
            let read_here = placed
                .iter()
                .any(|value| value.place.as_ref() == Some(at) && value.name.as_ref() == Ok(name));
            return read_here.then(|| name.clone());
        }
        let mut here = None;
        let mut outcome = None;
        let mut differs = false;
        for PlacedName { place, chain, name } in placed {
            match (place, name) {
                (None, Ok(_)) if self.kind_asked() && today().is_none() => {
                    outcome.get_or_insert(NameOutcome::KindAsked);
                }
                (None, Ok(text)) => {
                    outcome.get_or_insert(if self.site.linked {
                        NameOutcome::Linked
                    } else {
                        NameOutcome::Literal
                    });
                    // Where the reader's text differs from today's, today's
                    // name is the one stated (Q-B3f).
                    let own = today();
                    let todays: Vec<&str> = own.as_deref().into_iter().collect();
                    if text_differs(&todays, &text) {
                        differs = true;
                        tracing::debug!(
                            file = %self.site.file.display(),
                            line = self.site.line,
                            reader = %text,
                            today = ?todays,
                            "library names: the text reader's name differs from today's"
                        );
                    }
                    here = reader_name(&todays, text).or_else(today);
                }
                (Some(place), Ok(text)) => {
                    // Today's reading of this chain's caller there.
                    let todays: Vec<&str> = self
                        .on_chain(&place, &chain)
                        .filter_map(|site| site.literal(arg, key))
                        .collect();
                    if self.kind_asked() && todays.is_empty() {
                        outcome.get_or_insert(NameOutcome::KindAsked);
                        continue;
                    }
                    // A name per element of a stated list is placed at the
                    // library call itself (carrick#4245): read there, not
                    // filled by a caller.
                    let at_call =
                        place.file == self.site.file && place.span_start == self.site.span_start;
                    outcome.get_or_insert(match (at_call, self.site.linked) {
                        (false, _) => NameOutcome::Filled,
                        (true, true) => NameOutcome::Linked,
                        (true, false) => NameOutcome::Literal,
                    });
                    // No row where it differs: the chain's caller site
                    // states today's name there and counts `text_differs`
                    // (Q-B3f).
                    if let Some(text) = reader_name(&todays, text) {
                        self.placed.borrow_mut().insert((place, text));
                    }
                }
                (Some(place), Err(reason)) => {
                    // A hole where today's reading of this chain's caller
                    // states the name and answers the hole: the caller's own
                    // reading states it (see the caller arm above), so no
                    // lead.
                    let kept = self.on_chain(&place, &chain).find_map(|site| {
                        site.literal(arg, key)
                            .and_then(|_| today_answers(site, reason, arg, key))
                    });
                    match kept {
                        Some(kept) => {
                            outcome.get_or_insert(kept);
                        }
                        None => {
                            self.lead(Some(place), reason, op);
                            self.led.borrow_mut().push(chain);
                        }
                    }
                }
                // A hole at the call itself today's reading answers (the
                // call writes an own builder's call) keeps today's name, here
                // or at each caller that fills it (the caller arm above).
                (None, Err(reason)) => {
                    let today_reads = today().is_some()
                        || self.sites.iter().any(|site| {
                            site.origin.as_ref().is_some_and(|origin| {
                                origin.file == self.site.file
                                    && origin.span_start == self.site.span_start
                            }) && site.literal(arg, key).is_some()
                        });
                    match today_answers(self.site, reason, arg, key).filter(|_| today_reads) {
                        Some(answered) => {
                            outcome.get_or_insert(answered);
                            here = today();
                        }
                        None => self.lead(None, reason, op),
                    }
                }
            }
        }
        // A text the reader reads where today's reading reads another at
        // the call itself is counted first: it is a name that moves.
        if differs {
            outcome = Some(NameOutcome::TextDiffers);
        }
        self.noted(match outcome {
            Some(outcome) => outcome,
            None if today().is_some() => NameOutcome::LeadTodayStated,
            None => NameOutcome::Lead,
        });
        here
    }

    /// The site a caller fills at `place` with this library call
    /// ([`LibrarySite::origin`]) through the caller chain `chain`: today's
    /// reading of the same chain the text reader read there. Two chains
    /// that end at one call are two sites there.
    fn on_chain<'s>(
        &'s self,
        place: &'s NamePlace,
        chain: &'s CallerChain,
    ) -> impl Iterator<Item = &'s LibrarySite> {
        self.sites.iter().filter(move |site| {
            site.origin.as_ref().is_some_and(|origin| {
                origin.file == self.site.file && origin.span_start == self.site.span_start
            }) && site.specifier == self.site.specifier
                && site.export == self.site.export
                && site.file == place.file
                && site.span_start == place.span_start
                && site.chain == *chain
        })
    }

    /// Whether the label leaves the kind open (a broker carries topics and
    /// job names alike, an in-process channel events or messages): the
    /// site path's kind rule ([`kind_and_role`]). A name the text reader
    /// reads there that today's reading did not then states no row: the
    /// site path asks the kind, and its answer states the row (B10-3,
    /// Q-B3e), so a job name is never stated as a queue name.
    fn kind_asked(&self) -> bool {
        let role = serde_json::to_value(self.export.role)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_default();
        kind_and_role(&role, None).0.is_none()
    }

    /// The name a `first` list reads at a call (contract amendment 8): each
    /// entry in order, the first the call fills, read as [`Self::name`]
    /// reads a slot. `None` when an entry states no name, or none fills.
    fn first(&self, first: &[NameEntry], op: LibraryOp) -> Option<String> {
        for entry in first {
            match entry {
                NameEntry::Decorated(_) => {
                    if self.at.is_some() {
                        return None;
                    }
                    self.noted(match self.site.decorated {
                        Some(_) => NameOutcome::Literal,
                        None => NameOutcome::StopUnread,
                    });
                    if self.site.decorated.is_none() {
                        self.unnamed(Stop::UnreadExpression, op);
                    }
                    return self.site.decorated.clone();
                }
                NameEntry::Slot(slot) => {
                    let (arg, key) = (slot.arg as usize, slot.key.as_deref());
                    if !matches!(read_slot(self.site, arg, key), SlotRead::Next) {
                        return self.name(arg, key, op);
                    }
                }
            }
        }
        if self.at.is_none() {
            self.noted(NameOutcome::StopUnread);
            self.unnamed(Stop::CallerArgMissing, op);
        }
        None
    }

    /// Saves a lead of `reason` at `place` (the library call when `None`),
    /// once.
    fn lead(&self, place: Option<NamePlace>, reason: Stop, op: LibraryOp) {
        let lead = self.lead_of(place, reason, op);
        let mut leads = self.leads.borrow_mut();
        if !leads.contains(&lead) {
            leads.push(lead);
        }
    }

    /// The lead of `reason` at `place` (the library call when `None`).
    fn lead_of(&self, place: Option<NamePlace>, reason: Stop, op: LibraryOp) -> LibraryLead {
        let (file, line, span_start, span_end) = match place {
            Some(place) => (place.file, place.line, place.span_start, place.span_end),
            None => self.place(),
        };
        let role = serde_json::to_value(self.export.role)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_default();
        LibraryLead {
            file,
            line,
            span_start,
            span_end,
            reason,
            package: self.export.package.clone(),
            export: self.export.export.clone(),
            member: self.site.member.clone(),
            kind: crate::possible_calls::kind_of(&role),
            role: crate::possible_calls::role_of(&role, Some(op_name(op))),
        }
    }

    /// A name one row can state: not empty, no wildcard character of the
    /// library's, and not a name the library emits itself where the op
    /// claimed on `selector` acts (a definition is on the export), as the
    /// site labels answer it ([`reserved_on`]).
    fn name_ok(&self, name: &str, selector: (Option<ClaimOn>, &Option<String>)) -> bool {
        if name.is_empty()
            || self
                .export
                .patterns
                .iter()
                .any(|pattern| !pattern.is_empty() && name.contains(pattern.as_str()))
        {
            return false;
        }
        !reserved_on(
            &self.export.claims,
            self.site.member.as_deref(),
            &self.site.path,
            selector,
        )
        .iter()
        .any(|reserved| reserved == name)
    }
}

/// What a call states through one receiver.
struct Through {
    kind: LibraryRowKind,
    name: String,
    name_scope: Option<NameScope>,
    claim_ids: Vec<String>,
}

/// What one `first` slot reads at a call (contract amendment 8).
enum SlotRead {
    /// A string literal: the name.
    Named,
    /// The call does not fill the slot, or fills it with a value that is no
    /// string: the next entry.
    Next,
    /// A value the source does not state as a string (an identifier, a call,
    /// a spread): nothing is read, as a name that might be overridden by it
    /// would be wrong.
    Stop,
}

fn read_slot(site: &LibrarySite, arg: usize, key: Option<&str>) -> SlotRead {
    read_args(&site.args, arg, key)
}

/// The word a name slot that reads no name is saved with (carrick#3947): a
/// slot the call fills with nothing that can be the name
/// `caller_arg_missing`, a spread `unread_expression`, any other value
/// `unread`.
fn slot_stop(args: &[SiteArg], arg: usize, key: Option<&str>) -> Stop {
    match read_args(args, arg, key) {
        SlotRead::Next => Stop::CallerArgMissing,
        _ if args.get(arg).is_some_and(|arg| arg.spread) => Stop::UnreadExpression,
        _ => Stop::Unread,
    }
}

fn read_args(args: &[SiteArg], arg: usize, key: Option<&str>) -> SlotRead {
    let Some(arg) = args.get(arg) else {
        return SlotRead::Next;
    };
    // A spread moves every position after it, and may hold the key.
    if arg.spread {
        return SlotRead::Stop;
    }
    // A value the source states and that cannot be the name.
    let other = arg.function || arg.non_string;
    match key {
        None => match &arg.text {
            Some(_) => SlotRead::Named,
            None if other || arg.object.is_some() => SlotRead::Next,
            None => SlotRead::Stop,
        },
        Some(key) => match &arg.object {
            Some(object) => match object.fields.get(key) {
                Some(Some(_)) => SlotRead::Named,
                Some(None) => SlotRead::Stop,
                None if object.open => SlotRead::Stop,
                None => SlotRead::Next,
            },
            None if other || arg.text.is_some() => SlotRead::Next,
            None => SlotRead::Stop,
        },
    }
}

/// Whether today's reading answers a hole the text reader read at `site`'s
/// slot `arg` (its entry `key`), and as what (B10-3): the step cap stopped
/// the chain (`capped`), or the library call or a caller on the way writes
/// the slot as a call of an own builder (`r1_builder`, Q-B3a's builder
/// rule, until F4). Any other hole is a lead. The one place the builder
/// rule is decided: at the library call (`site` the call) and per caller
/// chain (`site` the chain's caller-filled site).
fn today_answers(
    site: &LibrarySite,
    reason: Stop,
    arg: usize,
    key: Option<&str>,
) -> Option<NameOutcome> {
    if reason == Stop::FanOutCap {
        Some(NameOutcome::Capped)
    } else if site.builder_written(arg, key) {
        Some(NameOutcome::R1Builder)
    } else {
        None
    }
}

/// Whether the text reader's `text` differs from what today's reading
/// states at the same place (`todays`, one text per site there): none of
/// them is it. `false` where today's reading states nothing there.
fn text_differs(todays: &[&str], text: &str) -> bool {
    !todays.is_empty() && !todays.contains(&text)
}

/// The text reader's `text` where it states the name: not where it differs
/// from today's (Q-B3f: one name at one site, today's, counted
/// `text_differs` until the compare traces the site true).
fn reader_name(todays: &[&str], text: String) -> Option<String> {
    (!text_differs(todays, &text)).then_some(text)
}

/// How a site's own argument at `arg` (or its `key`) reads as a name
/// (carrick#3504 B10-0): where the text came from when there is one, and
/// why there is none when there is not.
fn site_outcome(site: &LibrarySite, arg: usize, key: Option<&str>) -> NameOutcome {
    match read_slot(site, arg, key) {
        SlotRead::Named if site.origin.is_some() => NameOutcome::Filled,
        SlotRead::Named if site.linked => NameOutcome::Linked,
        SlotRead::Named => NameOutcome::Literal,
        SlotRead::Stop if site.args.get(arg).is_some_and(|arg| arg.spread) => {
            NameOutcome::StopSpread
        }
        SlotRead::Next | SlotRead::Stop => NameOutcome::StopUnread,
    }
}

fn slot_arg(slot: &ClaimSlot) -> usize {
    slot.arg as usize
}

fn slot_key(slot: &ClaimSlot) -> Option<&str> {
    slot.key.as_deref()
}

/// The row an op states for a role. A socket's direction is the export's
/// side's; one that serves both sides is `UNKNOWN` (carrick#3498), and one that
/// says nothing states nothing. An
/// export the store marks `by_origin` is passed no side: its side is decided
/// per receiver at a decided site (docs/reference/client-semantics.md,
/// "Message roles"). A `request` or an `execute` is no message row.
fn row_kind(
    role: LibraryRole,
    op: LibraryOp,
    side: Option<Side>,
    protocol: Option<ClaimProtocol>,
    method: Option<&str>,
) -> Option<LibraryRowKind> {
    use LibraryRowKind::{Graphql, Pubsub, Socket};
    Some(match (role, op, side) {
        // A GraphQL root field: the op's `method` is its root type, and any
        // other word states nothing (contract amendment 8).
        (LibraryRole::ServerFramework, LibraryOp::Receive, _)
            if protocol == Some(ClaimProtocol::Graphql) =>
        {
            Graphql(match method? {
                "QUERY" => GraphqlOperationKind::Query,
                "MUTATION" => GraphqlOperationKind::Mutation,
                "SUBSCRIPTION" => GraphqlOperationKind::Subscription,
                _ => return None,
            })
        }
        (LibraryRole::Broker | LibraryRole::InProcessBus, LibraryOp::Send, _) => {
            Pubsub(PubsubRole::Publisher)
        }
        (LibraryRole::Broker | LibraryRole::InProcessBus, LibraryOp::Receive, _) => {
            Pubsub(PubsubRole::Subscriber)
        }
        (LibraryRole::Socket, op @ (LibraryOp::Send | LibraryOp::Receive), side) => {
            let row = if op == LibraryOp::Send {
                RowSide::Call
            } else {
                RowSide::Endpoint
            };
            Socket {
                direction: crate::message_key::socket_direction(side.map(Side::word), row)?,
                listener: row == RowSide::Endpoint,
            }
        }
        _ => return None,
    })
}

/// The scope a row is written with: an in-process bus's name never leaves
/// its service (D4, carrick#1564 comment 5940360418), whatever the claim
/// says. Else the claim's scope. A claim that says nothing on a
/// cross-process role (a broker, a socket) writes no scope: the name crosses
/// services ([`crate::message_key::name_scope`], carrick#3531).
pub(crate) fn scoped(role: LibraryRole, scope: Option<NameScope>) -> Option<NameScope> {
    let claimed = scope.map(|scope| wire_scope_of(&scope));
    let written =
        crate::message_key::name_scope(role == LibraryRole::InProcessBus, claimed, None, None)?;
    Some(NameScope {
        scope: if written.scope == carrick_match::GLOBAL_SCOPE {
            NameScopeKind::Global
        } else {
            NameScopeKind::Service
        },
        namespace: written.namespace,
    })
}

/// A claim's scope as the index writes it.
pub(crate) fn wire_scope_of(scope: &NameScope) -> carrick_match::NameScope {
    carrick_match::NameScope {
        scope: match scope.scope {
            NameScopeKind::Global => carrick_match::GLOBAL_SCOPE,
            NameScopeKind::Service => carrick_match::SERVICE_SCOPE,
        }
        .to_string(),
        namespace: scope.namespace.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::request_summary::library_sites;

    /// A manifest declaring every package the tests import.
    const MANIFEST: &str = "{ \"name\": \"service\", \"dependencies\": { \"@fixture/jobs\": \"^1.0.0\", \"@fixture/queue\": \"^2.0.0\", \"@fixture/bus\": \"^1.0.0\", \"@fixture/socket\": \"^1.0.0\", \"@fixture/http\": \"^1.0.0\", \"@fixture/gql\": \"^1.0.0\" } }\n";

    fn sites_of(files: &[(&str, &str)]) -> LibrarySites {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("package.json"), MANIFEST).expect("write manifest");
        for (name, source) in files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
            std::fs::write(&path, source).expect("write source");
        }
        let inputs = crate::engine::discover_request_inputs(dir.path());
        library_sites(&inputs)
    }

    /// One export's claims, written as the store answers them.
    fn export(value: serde_json::Value) -> ExportClaims {
        serde_json::from_value(value).expect("claims in the store's shape")
    }

    /// A verifier that answers `verified` for every check `verified` says,
    /// and `failed` for the rest, recording what it was asked.
    fn verifier<'a>(
        asked: &'a std::cell::RefCell<Vec<LibraryCheck>>,
        verified: impl Fn(&LibraryCheck) -> bool + 'a,
    ) -> impl FnOnce(&[LibraryCheck]) -> Result<Vec<SemanticsResult>, String> + 'a {
        move |checks| {
            asked.borrow_mut().extend(checks.iter().cloned());
            Ok(checks
                .iter()
                .map(|check| SemanticsResult {
                    claim_id: check.claim_id.clone(),
                    receiver: check.receiver.clone(),
                    verdict: if verified(check) {
                        SemanticsVerdict::Verified
                    } else {
                        SemanticsVerdict::Failed
                    },
                    reason: None,
                    declarations: Vec::new(),
                })
                .collect())
        }
    }

    fn all(_: &LibraryCheck) -> bool {
        true
    }

    fn rows_of(
        sites: &LibrarySites,
        exports: &[ExportClaims],
        verified: impl Fn(&LibraryCheck) -> bool,
    ) -> Vec<LibraryRow> {
        let asked = std::cell::RefCell::new(Vec::new());
        read(sites, exports, None, verifier(&asked, verified)).rows
    }

    /// carrick#2819: a result label rides the claim it sits on, and no
    /// check sends it: a labelled claim and the same claim unlabelled give
    /// the same check payload, from `checks` and from `server_checks`.
    #[test]
    fn checks_send_claims_without_their_result() {
        let labelled = |mut claim: serde_json::Value| {
            claim["result"] = json!({ "whole": true });
            claim
        };
        let bus_claims = vec![
            json!({ "kind": "op", "op": "send", "member": "publish", "on": "export",
                "name": { "arg": 0 }, "payload": { "arg": 1 } }),
            json!({ "kind": "make", "form": "call", "member": null }),
        ];
        let with = |claims: Vec<serde_json::Value>, role: &str, package: &str| {
            export(json!({
                "package": package, "version": "1.4.0", "specifier": package,
                "export": "bus", "role": role, "claims": claims
            }))
        };
        let payload = |checks: Vec<LibraryCheck>| serde_json::to_string(&checks).unwrap();

        let sites = sites_of(&[("src/orders.ts", BUS_SERVICE)]);
        let plain = with(bus_claims.clone(), "broker", "@fixture/bus");
        let marked = with(
            bus_claims.iter().cloned().map(labelled).collect(),
            "broker",
            "@fixture/bus",
        );
        assert!(marked.claims.iter().all(|c| c.result_check().is_some()));
        let sent = checks(&sites, &[marked]);
        assert!(!sent.is_empty());
        assert!(!payload(sent.clone()).contains("\"result\""));
        assert_eq!(payload(sent), payload(checks(&sites, &[plain])));

        let server_claims = vec![
            json!({ "kind": "make", "form": "call", "member": null }),
            json!({ "kind": "op", "op": "send", "member": "json", "on": "export",
                "payload": { "arg": 0 } }),
        ];
        let plain = with(server_claims.clone(), "server_framework", "fixture-server");
        let marked = with(
            server_claims.into_iter().map(labelled).collect(),
            "server_framework",
            "fixture-server",
        );
        assert!(marked.claims.iter().all(|c| c.result_check().is_some()));
        let sent = server_checks(&[marked]);
        assert!(!sent.is_empty());
        assert!(!payload(sent.clone()).contains("\"result\""));
        assert_eq!(payload(sent), payload(server_checks(&[plain])));
    }

    /// carrick#2674: the checks an `infer` request reads a route handler's
    /// parameters through: a server export's makers and scopes, its send ops
    /// that carry a payload slot, and its receive ops that say where the
    /// request is, each on the receiver its own `on` or `of` names. Other
    /// roles, sends with no payload slot, and receive ops without `request`
    /// ask nothing.
    #[test]
    fn server_checks_are_a_server_exports_makers_sends_and_request_receives() {
        let server = export(json!({
            "package": "fixture-server", "version": "2.1.0", "specifier": "fixture-server",
            "export": "default", "role": "server_framework",
            "claims": [
                { "kind": "make", "form": "call", "member": null },
                { "kind": "op", "op": "receive", "member": "post", "of": "instance:()",
                  "name": { "arg": 0 }, "handler": { "arg": 1 },
                  "request": { "param": 0, "path": ["path"] } },
                { "kind": "op", "op": "receive", "member": "get", "of": "instance:()",
                  "name": { "arg": 0 }, "handler": { "arg": 1 } },
                { "kind": "op", "op": "receive", "member": "put", "of": "instance:()",
                  "name": { "arg": 0 }, "handler": { "arg": 1 }, "returns": {} }
            ]
        }));
        let reply = export(json!({
            "package": "fixture-server", "version": "2.1.0", "specifier": "fixture-server",
            "export": "reply", "role": "server_framework",
            "claims": [
                { "kind": "op", "op": "send", "member": "json", "on": "export", "payload": { "arg": 0 } },
                { "kind": "op", "op": "send", "member": "end", "on": "export" },
                { "kind": "op", "op": "send", "member": "write", "on": "instance", "payload": { "arg": 0 } }
            ]
        }));
        let bus = export(json!({
            "package": "@fixture/bus", "version": "1.4.0", "specifier": "@fixture/bus",
            "export": "bus", "role": "broker",
            "claims": [
                { "kind": "op", "op": "send", "member": "publish", "on": "export",
                  "name": { "arg": 0 }, "payload": { "arg": 1 } }
            ]
        }));
        let mut reply = reply;
        reply.type_packages = vec!["fixture-server-core-types".into()];
        let mut bus = bus;
        bus.type_packages = vec!["fixture-bus-types".into()];
        assert_eq!(
            server_type_packages(&[reply.clone(), bus.clone()]),
            std::collections::BTreeMap::from([(
                "fixture-server".to_string(),
                vec!["fixture-server-core-types".to_string()]
            )])
        );
        let asked: Vec<(String, String, String)> = server_checks(&[server, reply, bus])
            .into_iter()
            .map(|check| (check.export, check.receiver, check.claim_id))
            .collect();
        assert_eq!(
            asked,
            vec![
                (
                    "default".to_string(),
                    "export".to_string(),
                    "fixture-server@2:default:make:call:()".to_string()
                ),
                (
                    "default".to_string(),
                    "instance:()".to_string(),
                    "fixture-server@2:default:op:receive:post:of:instance:()".to_string()
                ),
                (
                    "default".to_string(),
                    "instance:()".to_string(),
                    "fixture-server@2:default:op:receive:put:of:instance:()".to_string()
                ),
                (
                    "reply".to_string(),
                    "export".to_string(),
                    "fixture-server@2:reply:op:send:json:on:export".to_string()
                ),
            ]
        );
    }

    /// carrick#2977 B: a reply that is a type export carries its sends
    /// `of: "type"`; the verifier reads them on the receiver `type`. `on`
    /// never reaches that receiver.
    #[test]
    fn a_type_exports_send_is_checked_on_receiver_type() {
        let reply = export(json!({
            "package": "fixture-server", "version": "2.1.0", "specifier": "fixture-server",
            "export": "Reply", "role": "server_framework",
            "claims": [
                { "kind": "op", "op": "send", "member": "send", "of": "type", "payload": { "arg": 0 } }
            ]
        }));
        let asked: Vec<(String, String, String)> = server_checks(&[reply])
            .into_iter()
            .map(|check| (check.export, check.receiver, check.claim_id))
            .collect();
        assert_eq!(
            asked,
            vec![(
                "Reply".to_string(),
                "type".to_string(),
                "fixture-server@2:Reply:op:send:send:of:type".to_string()
            )]
        );
        assert!(applies(None, Some("type"), "type"));
        assert!(!applies(Some(ClaimOn::Instance), None, "type"));
        assert!(!applies(Some(ClaimOn::Both), None, "type"));
        assert!(!applies(Some(ClaimOn::Export), None, "type"));
    }

    /// A declaration found in a type-only package stands for the package
    /// whose answer lists it; an `@types/*` name keeps its convention; any
    /// other name is itself (carrick#2434).
    #[test]
    fn a_type_only_package_is_keyed_to_the_package_that_lists_it() {
        let mut core = export(json!({
            "package": "fixture-server", "version": "4.0.0", "specifier": "fixture-server",
            "export": "default", "role": "server_framework"
        }));
        core.type_packages = vec!["fixture-server-core-types".into()];
        let exports = vec![core];
        assert_eq!(
            runtime_packages_of("fixture-server-core-types", &exports),
            ["fixture-server"]
        );
        assert_eq!(
            runtime_packages_of("@types/scope__name", &exports),
            ["@scope/name"]
        );
        assert_eq!(runtime_packages_of("@types/plain", &[]), ["plain"]);
        assert_eq!(runtime_packages_of("other", &exports), ["other"]);
    }

    /// A type-only package two packages list (a middleware package whose
    /// surface extends a server package's types lists the server's type
    /// packages too) stands for both, in answer order, whichever comes
    /// first (carrick#2661).
    #[test]
    fn a_type_only_package_two_packages_list_stands_for_both() {
        let mut middleware = export(json!({
            "package": "fixture-middleware", "version": "1.0.0", "specifier": "fixture-middleware",
            "export": "default", "role": "server_framework"
        }));
        middleware.type_packages = vec![
            "fixture-server-core-types".into(),
            "fixture-middleware-types".into(),
        ];
        let mut server = export(json!({
            "package": "fixture-server", "version": "4.0.0", "specifier": "fixture-server",
            "export": "default", "role": "server_framework"
        }));
        server.type_packages = vec!["fixture-server-core-types".into()];
        let mut router = server.clone();
        router.export = "Router".into();
        let exports = vec![middleware, server, router];
        assert_eq!(
            runtime_packages_of("fixture-server-core-types", &exports),
            ["fixture-middleware", "fixture-server"]
        );
        assert_eq!(
            runtime_packages_of("fixture-middleware-types", &exports),
            ["fixture-middleware"]
        );
    }

    fn bus() -> ExportClaims {
        export(json!({
            "package": "@fixture/bus", "version": "1.4.0", "specifier": "@fixture/bus",
            "export": "bus", "role": "broker",
            "claims": [
                { "kind": "op", "op": "send", "member": "publish", "on": "export",
                  "name": { "arg": 0 }, "payload": { "arg": 1 },
                  "name_scope": { "scope": "global", "namespace": null } },
                { "kind": "op", "op": "receive", "member": "subscribe", "on": "export",
                  "name": { "arg": 0 }, "handler": { "arg": 1 },
                  "name_scope": { "scope": "global", "namespace": null } },
                { "kind": "reserved", "member": "subscribe", "name": "$connected", "on": "export" }
            ],
            "calls": [
                { "member": "close", "on": "export", "kind": "off_wire" },
                { "member": "setPrefix", "on": "export", "kind": "mutator" }
            ],
            "patterns": ["*"]
        }))
    }

    fn command_claims() -> ExportClaims {
        export(json!({
            "package": "@fixture/queue", "version": "2.0.0", "specifier": "@fixture/queue",
            "export": "SendCommand", "role": "broker",
            "claims": [
                { "kind": "op", "op": "send", "member": null, "on": "export", "form": "new",
                  "name": { "arg": 0, "key": "QueueUrl" }, "payload": { "arg": 0, "key": "Body" },
                  "name_scope": { "scope": "global", "namespace": null } }
            ]
        }))
    }

    /// carrick#3439 T1: an op claimed in the `new` form is read where the
    /// export is constructed, from the argument's key, and never where it is
    /// called. A literal name is a publisher row; a name held in a parameter
    /// states nothing; the check goes to the export receiver.
    #[test]
    fn a_new_form_op_states_a_row_at_the_construction() {
        let command = command_claims();
        let sites = sites_of(&[(
            "src/send.ts",
            "import { SendCommand } from \"@fixture/queue\";\n\
            export function literal() {\n\
            \x20 return new SendCommand({ QueueUrl: \"orders\", Body: \"x\" });\n\
            }\n\
            export function held(url: string) {\n\
            \x20 return new SendCommand({ QueueUrl: url, Body: \"x\" });\n\
            }\n\
            export function bodyless() {\n\
            \x20 return new SendCommand({ QueueUrl: \"no-body\" });\n\
            }\n",
        )]);
        let asked = std::cell::RefCell::new(Vec::new());
        let rows = read(&sites, &[command], None, verifier(&asked, all)).rows;
        let stated: Vec<(u32, LibraryRowKind, &str)> = rows
            .iter()
            .map(|row| (row.line, row.kind, row.name.as_str()))
            .collect();
        assert_eq!(
            stated,
            vec![(3, LibraryRowKind::Pubsub(PubsubRole::Publisher), "orders")],
            "{rows:#?}"
        );
        assert_eq!(
            rows[0].claim_ids,
            vec!["@fixture/queue@2:SendCommand:op:send:():on:export:form:new"]
        );
        assert!(
            asked
                .borrow()
                .iter()
                .all(|check| check.receiver == "export"),
            "{:?}",
            asked.borrow()
        );
        // A plain call of the same export is a use the claims do not list:
        // it contests the export, and no site states a row.
        let called = sites_of(&[(
            "src/call.ts",
            "import { SendCommand } from \"@fixture/queue\";\n\
            export function literal() {\n\
            \x20 return new SendCommand({ QueueUrl: \"orders\", Body: \"x\" });\n\
            }\n\
            export function called() {\n\
            \x20 return (SendCommand as any)({ QueueUrl: \"called\", Body: \"x\" });\n\
            }\n",
        )]);
        assert!(rows_of(&called, &[command_claims()], all).is_empty());
    }

    fn gateway_claims() -> ExportClaims {
        export(json!({
            "package": "@fixture/socket", "version": "1.2.0", "specifier": "@fixture/socket",
            "export": "OnMessage", "role": "socket", "side": "server",
            "claims": [
                { "kind": "op", "op": "receive", "member": null, "on": "export",
                  "form": "decorate", "name": { "arg": 0 } }
            ]
        }))
    }

    /// carrick#3043: an op claimed in the `decorate` form is read where the
    /// decorator is applied to a method: a socket listener, client to server
    /// (the export's side is the server's), at the method's name, checked on
    /// the export receiver. A plain call of the same export is a use the
    /// claims do not list: it contests the export, and no site states a row.
    #[test]
    fn a_decorate_form_op_states_a_listener_at_the_decorated_method() {
        let source = "import { OnMessage } from \"@fixture/socket\";\n\
            export class Lobby {\n\
            \x20 @OnMessage(\"lobby.enter\")\n\
            \x20 enter(body: unknown) { return body; }\n\
            }\n";
        let sites = sites_of(&[("src/lobby.ts", source)]);
        let asked = std::cell::RefCell::new(Vec::new());
        let rows = read(&sites, &[gateway_claims()], None, verifier(&asked, all)).rows;
        let stated: Vec<(u32, LibraryRowKind, &str)> = rows
            .iter()
            .map(|row| (row.line, row.kind, row.name.as_str()))
            .collect();
        assert_eq!(
            stated,
            vec![(
                4,
                LibraryRowKind::Socket {
                    direction: SocketDirection::ClientToServer,
                    listener: true
                },
                "lobby.enter"
            )],
            "{rows:#?}"
        );
        assert_eq!(
            rows[0].claim_ids,
            vec!["@fixture/socket@1:OnMessage:op:receive:():on:export:form:decorate"]
        );
        assert!(
            asked
                .borrow()
                .iter()
                .all(|check| check.receiver == "export"),
            "{:?}",
            asked.borrow()
        );
        let called = sites_of(&[(
            "src/lobby.ts",
            "import { OnMessage } from \"@fixture/socket\";\n\
            export class Lobby {\n\
            \x20 @OnMessage(\"lobby.enter\")\n\
            \x20 enter(body: unknown) { return body; }\n\
            }\n\
            export const byHand = OnMessage(\"lobby.called\");\n",
        )]);
        assert!(rows_of(&called, &[gateway_claims()], all).is_empty());
    }

    /// One root-field decorator export, labelled the way the store labels a
    /// code-first GraphQL library: the name is the first of argument 0, the
    /// options `name`, then the decorated method.
    fn root_field_claims(exported: &str, method: &str) -> ExportClaims {
        export(json!({
            "package": "@fixture/gql", "version": "1.0.0", "specifier": "@fixture/gql",
            "export": exported, "role": "server_framework",
            "claims": [
                { "kind": "op", "op": "receive", "member": null, "on": "export",
                  "form": "decorate", "protocol": "graphql", "method": method,
                  "name": { "first": [{ "arg": 0 }, { "arg": 1, "key": "name" },
                                      { "bound": "decorated" }] },
                  "key_labels": { "description": "not_name" } }
            ]
        }))
    }

    fn root_fields() -> Vec<ExportClaims> {
        vec![
            root_field_claims("Query", "QUERY"),
            root_field_claims("Mutation", "MUTATION"),
        ]
    }

    /// The rows a service of one file states through the root-field
    /// decorators, as (line, kind, name).
    fn root_field_rows(source: &str) -> Vec<(u32, LibraryRowKind, String)> {
        let sites = sites_of(&[("src/resolvers.ts", source)]);
        let mut rows = rows_of(&sites, &root_fields(), all);
        assert!(rows.iter().all(|row| row.name_scope.is_none()), "{rows:#?}");
        rows.sort_by_key(|row| row.line);
        rows.into_iter()
            .map(|row| (row.line, row.kind, row.name))
            .collect()
    }

    const QUERY: LibraryRowKind = LibraryRowKind::Graphql(GraphqlOperationKind::Query);
    const MUTATION: LibraryRowKind = LibraryRowKind::Graphql(GraphqlOperationKind::Mutation);

    /// Amendment 8: a `receive` op in the `decorate` form that speaks GraphQL
    /// is a producer of the root field the decorator names (argument 0), or
    /// else the decorated method's own name, of the root type the op's
    /// `method` says, at the method's name.
    #[test]
    fn a_decorate_form_graphql_op_states_a_producer_at_the_decorated_method() {
        let source = "import { Query, Mutation } from \"@fixture/gql\";\n\
            export class Resolver {\n\
            \x20 @Query(\"byString\")\n\
            \x20 lookup() { return 1; }\n\
            \x20 @Query()\n\
            \x20 everything() { return 2; }\n\
            \x20 @Mutation(() => Object)\n\
            \x20 rename() { return 3; }\n\
            \x20 @Query(5)\n\
            \x20 numbered() { return 4; }\n\
            }\n";
        assert_eq!(
            root_field_rows(source),
            vec![
                (4, QUERY, "byString".to_string()),
                (6, QUERY, "everything".to_string()),
                (8, MUTATION, "rename".to_string()),
                (10, QUERY, "numbered".to_string()),
            ]
        );
    }

    /// A `name` option the call writes outranks the decorated member's name,
    /// and so does argument 0; an options object at argument 0 holds no
    /// name there (this label reads it at argument 1).
    #[test]
    fn a_name_option_outranks_the_decorated_members_name() {
        let source = "import { Query, Mutation } from \"@fixture/gql\";\n\
            export class Resolver {\n\
            \x20 @Mutation(() => Object, { name: \"renameThing\", description: \"d\" })\n\
            \x20 rename() { return 1; }\n\
            \x20 @Query(\"first\", { name: \"second\" })\n\
            \x20 both() { return 2; }\n\
            \x20 @Query({ name: \"ignored\" } as any)\n\
            \x20 byMethod() { return 3; }\n\
            }\n";
        assert_eq!(
            root_field_rows(source),
            vec![
                (4, MUTATION, "renameThing".to_string()),
                (6, QUERY, "first".to_string()),
                (8, QUERY, "byMethod".to_string()),
            ]
        );
    }

    /// A name the source does not write out as a string states no row, and
    /// the method's name never fills in for it: a variable, a call, a
    /// spread, a computed method key, and an options object that might hold
    /// the key.
    #[test]
    fn a_name_option_that_is_not_a_literal_states_no_row() {
        let source = "import { Query, Mutation } from \"@fixture/gql\";\n\
            declare const dynamic: string;\n\
            declare const args: any[];\n\
            declare const options: any;\n\
            declare function named(): string;\n\
            const computed = \"c\";\n\
            export class Resolver {\n\
            \x20 @Mutation(() => Object, { name: dynamic })\n\
            \x20 a() { return 1; }\n\
            \x20 @Query(named())\n\
            \x20 b() { return 2; }\n\
            \x20 @Query(...args)\n\
            \x20 c() { return 3; }\n\
            \x20 @Query(() => Object, options)\n\
            \x20 d() { return 4; }\n\
            \x20 @Query(() => Object, { ...options })\n\
            \x20 e() { return 5; }\n\
            \x20 @Query()\n\
            \x20 [computed]() { return 6; }\n\
            \x20 @Query(dynamic)\n\
            \x20 f() { return 7; }\n\
            }\n";
        assert_eq!(root_field_rows(source), vec![]);
    }

    const BUS_SERVICE: &str = "import { bus } from \"@fixture/bus\";\n\
        export async function send(topic: string) {\n\
        \x20 await bus.publish(\"orders.created\", { id: 1 });\n\
        \x20 await bus.publish(topic, {});\n\
        \x20 await bus.publish(\"orders.*\", {});\n\
        \x20 await bus.publish(\"orders.lonely\");\n\
        \x20 bus.subscribe(\"orders.created\", (message) => {});\n\
        \x20 bus.subscribe(\"$connected\", () => {});\n\
        \x20 await bus.publish(\"\", {});\n\
        }\n";

    /// The name census (carrick#3504 B10-0) counts each site by how its name
    /// slot read: a literal, a constant, a parameter a caller fills, a spread
    /// and any other value the reader cannot read as text. It counts the
    /// reading, not the rows, so a site that states no row is counted too.
    #[test]
    fn the_name_census_counts_each_name_outcome() {
        let sites = sites_of(&[
            ("src/topics.ts", "export const KIND = \"orders.kind\";\n"),
            (
                "src/names.ts",
                "import { bus } from \"@fixture/bus\";\n\
                import { KIND } from \"./topics\";\n\
                export function literal() { bus.publish(\"orders.literal\", {}); }\n\
                export function linked() { bus.publish(KIND, {}); }\n\
                export function relay(topic: string) { bus.publish(topic, {}); }\n\
                export function caller() { relay(\"orders.filled\"); }\n\
                export function spread(...args: any[]) { bus.publish(...args, {}); }\n\
                export function unread() { bus.publish(compute(), {}); }\n",
            ),
        ]);
        let asked = std::cell::RefCell::new(Vec::new());
        let names = read(&sites, &[bus()], None, verifier(&asked, all)).names;
        assert_eq!(
            names.line(),
            "literal=1 linked=1 filled=1 made=0 no_rows=0 r1_builder=0 stop_spread=1 stop_unread=2 lead=0 lead_today_stated=0 kind_asked=0 capped=0 caller_today=0 text_differs=0",
            "{names:?}"
        );
    }

    /// Claim ids follow the HTTP convention, `<specifier>@<major>:<export>:…`,
    /// and a claim stated twice differently is dropped.
    #[test]
    fn claim_ids_name_the_export_and_the_element() {
        let ids: Vec<String> = bus().claims_by_id().into_iter().map(|(id, _)| id).collect();
        assert_eq!(
            ids,
            vec![
                "@fixture/bus@1:bus:op:send:publish:on:export",
                "@fixture/bus@1:bus:op:receive:subscribe:on:export",
                "@fixture/bus@1:bus:reserved:subscribe:$connected:on:export",
            ]
        );
        let mut twice = bus();
        twice.claims.push(
            serde_json::from_value(json!({
                "kind": "op", "op": "send", "member": "publish", "on": "export",
                "name": { "arg": 1 }, "payload": { "arg": 0 }
            }))
            .expect("an op"),
        );
        let ids: Vec<String> = twice.claims_by_id().into_iter().map(|(id, _)| id).collect();
        assert!(
            !ids.iter().any(|id| id.contains(":send:publish")),
            "{ids:?}"
        );
        assert_eq!(major("2.4.1"), "2");
        assert_eq!(major("^10.0.0"), "10");
        assert_eq!(major("latest"), "x");
    }

    /// Amendment 7 of carrick#2434 (carrick#3045): every new field of the
    /// grammar reads from the wire and writes back as it was read.
    #[test]
    fn amendment_seven_fields_round_trip() {
        let wire = [
            json!({ "kind": "op", "op": "send", "member": "publish", "on": "export",
                    "name": { "arg": 0, "at": ["messages[]", "topic"], "each": true,
                              "document": "graphql", "ref": "class" },
                    "payload": { "arg": 1, "at": ["value"] } }),
            json!({ "kind": "op", "op": "send", "member": "publish", "on": "export",
                    "name": { "parts": [{ "arg": 0 }, { "arg": 1, "key": "id" }], "join": "/" } }),
            json!({ "kind": "op", "op": "receive", "member": "on", "on": "export",
                    "name": { "literal": "connection" } }),
            json!({ "kind": "op", "op": "receive", "member": "on", "on": "export",
                    "name": { "bound": "path", "join": "." } }),
            json!({ "kind": "op", "op": "receive", "member": "on", "on": "export",
                    "name": { "bound": "export" } }),
            json!({ "kind": "op", "op": "receive", "member": "on", "on": "export",
                    "name": { "bound": "subscribed", "param": 0, "at": ["topic"] } }),
            json!({ "kind": "op", "op": "receive", "member": "get", "on": "export",
                    "form": "decorate", "protocol": "graphql",
                    "when": { "arg": 0, "key": "event", "literal": "message" },
                    "returns": { "result": { "member": ["data"] } } }),
            json!({ "kind": "op", "op": "receive", "member": "get", "on": "export",
                    "returns": {} }),
            json!({ "kind": "scope", "member": "controller", "form": "decorate",
                    "name": { "arg": 0, "ref": "function" },
                    "runs": { "arg": 1 },
                    "yields": { "param": 0, "export": "Ctx", "side": "server",
                                "package": "fixture-other" } }),
        ];
        for value in wire {
            let claim: LibraryClaim = serde_json::from_value(value.clone()).expect("a claim");
            assert_eq!(serde_json::to_value(&claim).unwrap(), value);
        }
        for form in ["call", "new", "set", "override", "decorate"] {
            let claim: LibraryClaim = serde_json::from_value(
                json!({ "kind": "op", "op": "send", "member": "m", "form": form }),
            )
            .expect("a form");
            assert_eq!(claim.is_call_form(), form == "call");
        }
        for protocol in ["http", "graphql", "rpc", "socket", "message"] {
            serde_json::from_value::<LibraryClaim>(
                json!({ "kind": "op", "op": "send", "member": "m", "protocol": protocol }),
            )
            .expect("a protocol");
        }
        assert!(
            serde_json::from_value::<LibraryClaim>(
                json!({ "kind": "op", "op": "send", "member": "m", "form": "returns" })
            )
            .is_err()
        );
    }

    /// Amendment 8 of carrick#2434: a decorated member's name and a `first`
    /// list read from the wire and write back as they were read; a `first`
    /// entry with any field the compiler engine's schema refuses is refused
    /// here too, so it never reaches a request it would fail whole.
    #[test]
    fn amendment_eight_names_round_trip() {
        let wire = [
            json!({ "kind": "op", "op": "receive", "member": null, "on": "export",
                    "form": "decorate", "protocol": "graphql", "method": "QUERY",
                    "name": { "bound": "decorated" } }),
            json!({ "kind": "op", "op": "receive", "member": null, "on": "export",
                    "form": "decorate", "protocol": "graphql", "method": "QUERY",
                    "name": { "first": [{ "arg": 0 }, { "arg": 1, "key": "name" },
                                        { "bound": "decorated" }] },
                    "key_labels": { "description": "not_name" } }),
        ];
        for value in wire {
            let claim: LibraryClaim = serde_json::from_value(value.clone()).expect("a claim");
            assert_eq!(serde_json::to_value(&claim).unwrap(), value);
        }
        let LibraryClaim::Op {
            name: Some(name), ..
        } = serde_json::from_value(json!({
            "kind": "op", "op": "receive", "member": null, "form": "decorate",
            "name": { "first": [{ "arg": 0 }, { "bound": "decorated" }] }
        }))
        .expect("a claim")
        else {
            panic!("an op with a name");
        };
        assert!(matches!(&*name, OpName::First(first) if first.first.len() == 2));
        for refused in [
            json!({ "first": [{ "arg": 0, "at": ["id"] }, { "bound": "decorated" }] }),
            json!({ "first": [{ "arg": 0, "document": "graphql" }, { "bound": "decorated" }] }),
            json!({ "first": [{ "arg": 0 }, { "bound": "maker" }] }),
            json!({ "first": [{ "arg": 0 }, { "bound": "decorated", "join": "." }] }),
            json!({ "first": [{ "arg": 0 }], "join": "." }),
        ] {
            assert!(
                serde_json::from_value::<LibraryClaim>(json!({
                    "kind": "op", "op": "receive", "member": null, "name": refused.clone()
                }))
                .is_err(),
                "{refused}"
            );
        }
    }

    /// A claim written without the new fields writes back byte for byte as it
    /// was read: the wire bytes of a claim the store holds today do not move.
    #[test]
    fn a_claim_without_amendment_seven_fields_serialises_byte_identical() {
        let claims = [
            r#"{"kind":"op","op":"send","member":"publish","on":"export","name":{"arg":1},"payload":{"arg":0}}"#,
            r#"{"kind":"op","op":"receive","member":"get","of":"instance:()","name":{"bound":"maker"},"handler":{"arg":1},"method":"GET","yields":{"param":1,"export":"Reply","side":"server"}}"#,
            r#"{"kind":"scope","member":"mount","name":{"arg":0,"key":"prefix"},"of":"instance:new"}"#,
            r#"{"kind":"make","form":"call","member":null,"name":{"arg":0}}"#,
        ];
        for text in claims {
            let claim: LibraryClaim = serde_json::from_str(text).expect("a claim");
            assert_eq!(serde_json::to_string(&claim).unwrap(), text);
        }
    }

    /// Amendment 7: `form` (not `call`) and `when` extend the id of the op
    /// they sit on; every other id is as it was.
    #[test]
    fn claim_ids_name_the_form_and_the_when_literal() {
        let server = export(json!({
            "package": "fixture-server", "version": "2.1.0", "specifier": "fixture-server",
            "export": "Server", "role": "server_framework",
            "claims": [
                { "kind": "op", "op": "receive", "member": "on", "of": "instance:new" },
                { "kind": "op", "op": "receive", "member": "on", "of": "instance:new",
                  "form": "call" },
                { "kind": "op", "op": "receive", "member": "on", "of": "instance:new",
                  "form": "set" },
                { "kind": "op", "op": "receive", "member": "on", "of": "instance:new",
                  "when": { "arg": 0, "literal": "message" } },
                { "kind": "op", "op": "receive", "member": "on", "of": "instance:new",
                  "form": "set", "when": { "arg": 0, "literal": "close" } },
                { "kind": "scope", "member": "use", "of": "instance:new", "name": { "arg": 0 } },
                { "kind": "scope", "member": "use", "of": "instance:new", "name": { "arg": 0 },
                  "form": "decorate" }
            ]
        }));
        let prefix = "fixture-server@2:Server";
        let claims = server
            .claims
            .iter()
            .map(|claim| claim_id(prefix, claim).unwrap());
        assert_eq!(
            claims.collect::<Vec<_>>(),
            vec![
                format!("{prefix}:op:receive:on:of:instance:new"),
                format!("{prefix}:op:receive:on:of:instance:new"),
                format!("{prefix}:op:receive:on:of:instance:new:form:set"),
                format!("{prefix}:op:receive:on:of:instance:new:when:message"),
                format!("{prefix}:op:receive:on:of:instance:new:form:set:when:close"),
                format!("{prefix}:scope:use:name@0:returns:of:instance:new"),
                format!("{prefix}:scope:use:name@0:returns:of:instance:new:form:decorate"),
            ]
        );
    }

    /// QW-1: `or_file` on a handler is read from the store, written back as
    /// stored, and moves no claim id.
    #[test]
    fn or_file_on_a_handler_keeps_the_claim_id() {
        let jobs = export(json!({
            "package": "fixture-jobs", "version": "5.1.0", "specifier": "fixture-jobs",
            "export": "Worker", "role": "broker",
            "claims": [
                { "kind": "op", "op": "receive", "member": null, "on": "export", "form": "new",
                  "name": { "arg": 0 }, "handler": { "arg": 1, "or_file": true } },
                { "kind": "op", "op": "receive", "member": null, "on": "export", "form": "new",
                  "name": { "arg": 0 }, "handler": { "arg": 1 } }
            ]
        }));
        let LibraryClaim::Op { handler, .. } = &jobs.claims[0] else {
            panic!("an op")
        };
        assert!(handler.as_ref().unwrap().or_file);
        let ids: Vec<String> = jobs
            .claims
            .iter()
            .map(|claim| claim_id("fixture-jobs@5:Worker", claim).unwrap())
            .collect();
        assert_eq!(ids[0], ids[1]);
        assert!(
            serde_json::to_string(&jobs.claims[0])
                .unwrap()
                .contains(r#""handler":{"arg":1,"or_file":true}"#)
        );
        assert!(
            serde_json::to_string(&jobs.claims[1])
                .unwrap()
                .contains(r#""handler":{"arg":1}"#)
        );
    }

    /// The readers of call sites do not meet a claim written as a set, an
    /// override or a scope decorator (amendment 7); an op written as a
    /// construction or a method decorator is met where it is written.
    #[test]
    fn call_site_readers_skip_claims_written_in_another_form() {
        let server = export(json!({
            "package": "fixture-server", "version": "2.1.0", "specifier": "fixture-server",
            "export": "Server", "role": "server_framework",
            "claims": [
                { "kind": "op", "op": "receive", "member": "on", "of": "instance:new" },
                { "kind": "op", "op": "receive", "member": "onerror", "of": "instance:new",
                  "form": "set" },
                { "kind": "scope", "member": "controller", "of": "instance:new",
                  "name": { "arg": 0 }, "form": "decorate" },
                { "kind": "op", "op": "send", "member": null, "on": "export", "form": "new",
                  "name": { "arg": 0, "key": "QueueUrl" } },
                { "kind": "op", "op": "receive", "member": null, "on": "export",
                  "form": "decorate", "name": { "arg": 0 } }
            ]
        }));
        let ids: Vec<String> = server
            .claims_with_scopes_by_id()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        // An op written as a construction of its export is read at the `new`
        // (carrick#3439), and one written as a method decorator where it is
        // applied (carrick#3043); a set and a scope decorator are still not
        // read.
        assert_eq!(
            ids,
            vec![
                "fixture-server@2:Server:op:receive:on:of:instance:new",
                "fixture-server@2:Server:op:send:():on:export:form:new",
                "fixture-server@2:Server:op:receive:():on:export:form:decorate"
            ]
        );
    }

    /// carrick#2474 R2b: a scope claim's id names its name slot and what it
    /// binds, so two scope claims on one member and one receiver that read
    /// different slots (`use(router)` and `use("/p", router)`) are two
    /// claims, each asked and verified on its own, and neither drops the
    /// other as stated twice.
    #[test]
    fn scope_claims_on_one_member_are_told_apart_by_slot_and_binding() {
        let server = export(json!({
            "package": "fixture-server", "version": "2.1.0", "specifier": "fixture-server",
            "export": "Server", "role": "server_framework",
            "claims": [
                { "kind": "scope", "member": "use", "of": "instance:new",
                  "name": { "arg": 1 }, "mounts": { "arg": 0 } },
                { "kind": "scope", "member": "use", "of": "instance:new",
                  "name": { "arg": 0 }, "mounts": { "arg": 1 } },
                { "kind": "scope", "member": "register", "of": "instance:new",
                  "name": { "arg": 1, "key": "prefix" }, "runs": { "arg": 0 },
                  "yields": { "param": 0, "export": "Server", "side": "server" } },
                { "kind": "scope", "member": "basePath", "of": "instance:new",
                  "name": { "arg": 0 } }
            ]
        }));
        let ids: Vec<String> = server
            .claims_with_scopes_by_id()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            ids,
            vec![
                "fixture-server@2:Server:scope:use:name@1:mounts@0:of:instance:new",
                "fixture-server@2:Server:scope:use:name@0:mounts@1:of:instance:new",
                "fixture-server@2:Server:scope:register:name@1.prefix:runs@0>0:of:instance:new",
                "fixture-server@2:Server:scope:basePath:name@0:returns:of:instance:new",
            ]
        );
    }

    /// #2474 R6a (contract amendment 3 on #2434): a router member has no
    /// name, so its id spells its collection in the name's place. The named
    /// scopes beside it keep their ids byte for byte, and a claim with both
    /// a name and a collection still gets an id, so the verifier refuses it
    /// rather than the reader losing it.
    #[test]
    fn a_router_member_s_id_spells_its_collection() {
        let kit = export(json!({
            "package": "fixture-procs", "version": "3.0.0", "specifier": "fixture-procs",
            "export": "init", "role": "server_framework",
            "claims": [
                { "kind": "scope", "member": "router", "of": "instance:create",
                  "collects": { "arg": 0, "join": "." } },
                { "kind": "scope", "member": "router", "of": "instance:create",
                  "collects": { "arg": 0, "key": "routes", "join": "/" } },
                { "kind": "scope", "member": "basePath", "of": "instance:create",
                  "name": { "arg": 0 } },
                { "kind": "scope", "member": "both", "of": "instance:create",
                  "name": { "arg": 0 }, "collects": { "arg": 1, "join": "." } },
                { "kind": "scope", "member": "mergeRouters", "of": "instance:create",
                  "merges": { "arg": 0, "each": true } },
                { "kind": "op", "op": "receive", "member": "query", "path": ["procedure"],
                  "of": "instance:create", "name": { "bound": "key" },
                  "handler": { "arg": 0 }, "method": "GET" }
            ]
        }));
        let ids: Vec<String> = kit
            .claims_with_scopes_by_id()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            ids,
            vec![
                "fixture-procs@3:init:scope:router:collects@0/.:of:instance:create",
                "fixture-procs@3:init:scope:router:collects@0.routes//:of:instance:create",
                "fixture-procs@3:init:scope:basePath:name@0:returns:of:instance:create",
                "fixture-procs@3:init:scope:both:name@0:returns:collects@1/.:of:instance:create",
                "fixture-procs@3:init:scope:mergeRouters:merges@0:of:instance:create",
                "fixture-procs@3:init:op:receive:procedure.query:of:instance:create",
            ]
        );
    }

    /// K1 (carrick#2434): a name-less binding scope spells its binding
    /// alone, a handle slot `handle@<slot>` where an instance mount spells
    /// `mounts@<slot>`, and a pass-through `passes`, so a claim and its
    /// handle twin are two ids. A named mount's id stays byte for byte, and
    /// a scope that binds nothing and carries nothing has none.
    #[test]
    fn a_name_less_scope_s_id_spells_its_binding_and_a_handle_its_flag() {
        let kit = export(json!({
            "package": "fixture-mw", "version": "1.0.0", "specifier": "fixture-mw",
            "export": "default", "role": "server_framework",
            "claims": [
                { "kind": "scope", "member": "use", "of": "instance:()",
                  "name": { "arg": 0 }, "mounts": { "arg": 1 } },
                { "kind": "scope", "member": "use", "of": "instance:()",
                  "name": { "arg": 0 }, "mounts": { "arg": 1, "handle": true } },
                { "kind": "scope", "member": "use", "of": "instance:()",
                  "mounts": { "arg": 0 } },
                { "kind": "scope", "member": "use", "of": "instance:()",
                  "mounts": { "arg": 0, "handle": true } },
                { "kind": "scope", "member": "routes", "of": "instance:()", "passes": true },
                { "kind": "scope", "member": "nothing", "of": "instance:()" }
            ]
        }));
        let ids: Vec<String> = kit
            .claims_with_scopes_by_id()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            ids,
            vec![
                "fixture-mw@1:default:scope:use:name@0:mounts@1:of:instance:()",
                "fixture-mw@1:default:scope:use:name@0:handle@1:of:instance:()",
                "fixture-mw@1:default:scope:use:mounts@0:of:instance:()",
                "fixture-mw@1:default:scope:use:handle@0:of:instance:()",
                "fixture-mw@1:default:scope:routes:passes:of:instance:()",
            ]
        );
    }

    /// A verified send is a publisher row and a verified receive a
    /// subscriber row, at the member's line, with the claim ids and the
    /// name's scope. A name that is no literal, holds a wildcard, is one the
    /// library emits itself, or is missing a part the claim positions states
    /// nothing.
    #[test]
    fn a_broker_s_verified_ops_state_rows() {
        let sites = sites_of(&[("src/orders.ts", BUS_SERVICE)]);
        let rows = rows_of(&sites, &[bus()], all);
        let stated: Vec<(u32, LibraryRowKind, &str)> = rows
            .iter()
            .map(|row| (row.line, row.kind, row.name.as_str()))
            .collect();
        assert_eq!(
            stated,
            vec![
                (
                    3,
                    LibraryRowKind::Pubsub(PubsubRole::Publisher),
                    "orders.created"
                ),
                (
                    7,
                    LibraryRowKind::Pubsub(PubsubRole::Subscriber),
                    "orders.created"
                ),
            ],
            "{rows:#?}"
        );
        assert_eq!(
            rows[0].claim_ids,
            vec!["@fixture/bus@1:bus:op:send:publish:on:export"]
        );
        assert_eq!(
            rows[0].name_scope,
            Some(NameScope {
                scope: NameScopeKind::Global,
                namespace: None
            })
        );
        assert!(!rows[0].definition);
    }

    /// Only checks that verified state rows: an unverified op, a verifier
    /// that fails, and one that answers another number of checks state
    /// nothing. Every check goes to a receiver a site uses, in one request.
    #[test]
    fn only_verified_claims_state_rows() {
        let sites = sites_of(&[("src/orders.ts", BUS_SERVICE)]);
        let asked = std::cell::RefCell::new(Vec::new());
        let rows = read(
            &sites,
            &[bus()],
            None,
            verifier(&asked, |check| !check.claim_id.contains(":send:")),
        );
        assert!(
            rows.rows
                .iter()
                .all(|row| row.kind == LibraryRowKind::Pubsub(PubsubRole::Subscriber)),
            "{rows:#?}"
        );
        assert!(
            asked
                .borrow()
                .iter()
                .all(|check| check.receiver == "export")
        );
        assert_eq!(asked.borrow().len(), 3);

        let failed = read(&sites, &[bus()], None, |_| Err("sidecar gone".to_string()));
        assert!(failed.rows.is_empty());
        let short = read(&sites, &[bus()], None, |_| Ok(Vec::new()));
        assert!(short.rows.is_empty());
        // Every verdict one place along: each names another check.
        let swapped = read(&sites, &[bus()], None, |checks| {
            Ok(checks
                .iter()
                .cycle()
                .skip(1)
                .take(checks.len())
                .map(|check| SemanticsResult {
                    claim_id: check.claim_id.clone(),
                    receiver: check.receiver.clone(),
                    verdict: SemanticsVerdict::Verified,
                    reason: None,
                    declarations: Vec::new(),
                })
                .collect())
        });
        assert!(
            swapped.rows.is_empty(),
            "verdicts out of order verify nothing"
        );
    }

    /// The saved sites of a reading: line, reason, kind and role.
    fn saved(rows: &LibraryRows) -> Vec<(u32, Stop, Option<&'static str>, Option<&'static str>)> {
        let mut saved: Vec<_> = rows
            .leads
            .iter()
            .map(|lead| (lead.line, lead.reason, lead.kind, lead.role))
            .collect();
        saved.sort();
        saved
    }

    /// carrick#3947, carrick#3946: a library call the reader reads and then
    /// states no row and no lead at is saved, once, with a reason: a claim
    /// the verifier answered `failed` is `uncorroborated`, one its time
    /// budget did not reach (`unchecked`, `budget`) is `unanswered`. Several
    /// calls ask one check, and the verdict decides before the name is read,
    /// so a wildcard and a reserved name are saved too. A call missing the
    /// payload the claim positions is no reading of the claim and saves
    /// nothing. A fully verified reading of the same calls states its rows
    /// and saves nothing.
    #[test]
    fn a_library_call_read_and_dropped_on_its_verdict_is_saved_with_a_reason() {
        let source = "import { bus } from \"@fixture/bus\";\n\
            export async function send() {\n\
            \x20 await bus.publish(\"orders.created\", { id: 1 });\n\
            \x20 await bus.publish(\"orders.shipped\", { id: 2 });\n\
            \x20 bus.subscribe(\"orders.created\", (message) => {});\n\
            \x20 await bus.publish(\"orders.*\", {});\n\
            \x20 await bus.publish(\"orders.lonely\");\n\
            \x20 bus.subscribe(\"$connected\", () => {});\n\
            }\n";
        let sites = sites_of(&[("src/orders.ts", source)]);
        let verdict = |check: &LibraryCheck| {
            if check.claim_id.contains(":send:") {
                (SemanticsVerdict::Failed, Some("not the declared member"))
            } else {
                (SemanticsVerdict::Unchecked, Some("budget"))
            }
        };
        let asked = std::cell::RefCell::new(0usize);
        let rows = read(&sites, &[bus()], None, |checks| {
            *asked.borrow_mut() = checks.len();
            Ok(checks
                .iter()
                .map(|check| {
                    let (verdict, reason) = verdict(check);
                    SemanticsResult {
                        claim_id: check.claim_id.clone(),
                        receiver: check.receiver.clone(),
                        verdict,
                        reason: reason.map(str::to_string),
                        declarations: Vec::new(),
                    }
                })
                .collect())
        });
        assert!(rows.rows.is_empty(), "{rows:#?}");
        assert_eq!(*asked.borrow(), 3, "fewer checks than sites");
        assert_eq!(
            saved(&rows),
            vec![
                (3, Stop::Uncorroborated, Some("pubsub"), Some("producer")),
                (4, Stop::Uncorroborated, Some("pubsub"), Some("producer")),
                (5, Stop::Unanswered, Some("pubsub"), Some("consumer")),
                (6, Stop::Uncorroborated, Some("pubsub"), Some("producer")),
                (8, Stop::Unanswered, Some("pubsub"), Some("consumer")),
            ],
            "{:?}",
            rows.leads
        );
        // The verified reading of the same sites states rows at 3, 4, 5 and
        // saves nothing: no row moves.
        let asked = std::cell::RefCell::new(Vec::new());
        let verified = read(&sites, &[bus()], None, verifier(&asked, all));
        let lines: Vec<u32> = verified.rows.iter().map(|row| row.line).collect();
        assert_eq!(lines, vec![3, 4, 5], "{verified:#?}");
        assert!(verified.leads.is_empty(), "{:?}", verified.leads);
        // Any other `unchecked`, and a verdict that names another check,
        // save nothing here.
        let other = read(&sites, &[bus()], None, |checks| {
            Ok(checks
                .iter()
                .map(|check| SemanticsResult {
                    claim_id: check.claim_id.clone(),
                    receiver: check.receiver.clone(),
                    verdict: SemanticsVerdict::Unchecked,
                    reason: Some("member_untyped".to_string()),
                    declarations: Vec::new(),
                })
                .collect())
        });
        assert!(other.leads.is_empty(), "{:?}", other.leads);
        let swapped = read(&sites, &[bus()], None, |checks| {
            Ok(checks
                .iter()
                .cycle()
                .skip(1)
                .take(checks.len())
                .map(|check| SemanticsResult {
                    claim_id: check.claim_id.clone(),
                    receiver: check.receiver.clone(),
                    verdict: SemanticsVerdict::Failed,
                    reason: None,
                    declarations: Vec::new(),
                })
                .collect())
        });
        assert!(swapped.leads.is_empty(), "{:?}", swapped.leads);
    }

    /// carrick#3947: a verified op at a call whose name slot reads no name,
    /// and saves no lead, is saved with the word for the slot: a spread is
    /// `unread_expression`, another value `unread`, a slot the call does
    /// not fill `caller_arg_missing`.
    #[test]
    fn a_verified_call_that_names_nothing_is_saved_with_a_reason() {
        let source = "import { bus } from \"@fixture/bus\";\n\
            export async function send(topic: string, rest: string[]) {\n\
            \x20 await bus.publish(\"orders.created\", { id: 1 });\n\
            \x20 await bus.publish(...rest, {});\n\
            \x20 await bus.publish(topic, {});\n\
            \x20 await bus.publish(7, {});\n\
            }\n";
        let sites = sites_of(&[("src/orders.ts", source)]);
        let asked = std::cell::RefCell::new(Vec::new());
        let rows = read(&sites, &[bus()], None, verifier(&asked, all));
        let lines: Vec<u32> = rows.rows.iter().map(|row| row.line).collect();
        assert_eq!(lines, vec![3], "{rows:#?}");
        assert_eq!(
            saved(&rows),
            vec![
                (4, Stop::UnreadExpression, Some("pubsub"), Some("producer")),
                (5, Stop::Unread, Some("pubsub"), Some("producer")),
                (6, Stop::CallerArgMissing, Some("pubsub"), Some("producer")),
            ],
            "{:?}",
            rows.leads
        );
    }

    /// A call the claims list as off the wire contests nothing; a mutator,
    /// and a member no list names, contest the receiver.
    #[test]
    fn calls_are_classified_by_the_claims_own_lists() {
        let mut claims = bus();
        claims.claims.push(
            serde_json::from_value(json!({
                "kind": "op", "op": "send", "member": "ack", "on": "instance",
                "name": { "arg": 0 }, "payload": { "arg": 1 }
            }))
            .expect("an instance op"),
        );
        let with = |extra: &str| {
            let source = format!("{BUS_SERVICE}export function stop() {{ {extra} }}\n");
            let sites = sites_of(&[("src/orders.ts", source.as_str())]);
            rows_of(&sites, std::slice::from_ref(&claims), all).len()
        };
        assert_eq!(with("bus.close();"), 2, "off the wire");
        assert_eq!(with("bus.setPrefix(\"staging\");"), 0, "a mutator");
        assert_eq!(with("bus.flush();"), 0, "a member no list names");
        assert_eq!(
            with("bus.ack();"),
            0,
            "a member claimed on instances only is unlisted on the export"
        );
    }

    /// A queue's maker binds the name its instance's ops send to (`{
    /// "bound": "maker" }`): the row rests on the op and the maker.
    #[test]
    fn a_name_bound_by_the_maker_is_the_maker_s_slot() {
        let queue = export(json!({
            "package": "@fixture/queue", "version": "2.0.0", "specifier": "@fixture/queue",
            "export": "Queue", "role": "broker",
            "claims": [
                { "kind": "make", "form": "new", "member": null, "name": { "arg": 0 },
                  "name_scope": { "scope": "global", "namespace": "queue" } },
                { "kind": "op", "op": "send", "member": "add", "on": "instance",
                  "name": { "bound": "maker" }, "payload": { "arg": 1 } }
            ]
        }));
        let sites = sites_of(&[(
            "src/mail.ts",
            "import { Queue } from \"@fixture/queue\";\n\
             const emails = new Queue(\"emails\");\n\
             export async function welcome() { await emails.add(\"welcome\", {}); }\n",
        )]);
        let rows = rows_of(&sites, std::slice::from_ref(&queue), all);
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].name, "emails");
        assert_eq!(rows[0].line, 3);
        assert_eq!(
            rows[0].claim_ids,
            vec![
                "@fixture/queue@2:Queue:make:new:()",
                "@fixture/queue@2:Queue:op:send:add:on:instance",
            ]
        );
        assert_eq!(
            rows[0]
                .name_scope
                .as_ref()
                .and_then(|scope| scope.namespace.as_deref()),
            Some("queue")
        );
        let unverified_maker =
            rows_of(&sites, &[queue], |check| !check.claim_id.contains(":make:"));
        assert!(unverified_maker.is_empty(), "the maker must verify");
    }

    /// carrick-cloud#3139: a consumer export that subscribes when
    /// constructed (`new Consumer({ queueUrl, handleMessage })`) is a receive
    /// in the `new` form: a subscriber row at the construction, beside the
    /// queue's publisher row on the same name and scope. A name held in a
    /// parameter states nothing. A verifier that refuses the receive (a
    /// scanner before it reads one) leaves the publisher row only.
    #[test]
    fn a_new_form_receive_states_a_subscriber_row() {
        let queue = export(json!({
            "package": "@fixture/queue", "version": "2.0.0", "specifier": "@fixture/queue",
            "export": "Queue", "role": "broker",
            "claims": [
                { "kind": "make", "form": "new", "member": null, "name": { "arg": 0 },
                  "name_scope": { "scope": "global", "namespace": "queue" } },
                { "kind": "op", "op": "send", "member": "add", "on": "instance",
                  "name": { "bound": "maker" }, "payload": { "arg": 1 } }
            ]
        }));
        let consumer = export(json!({
            "package": "@fixture/queue", "version": "2.0.0", "specifier": "@fixture/queue",
            "export": "Consumer", "role": "broker",
            "claims": [
                { "kind": "op", "op": "receive", "member": null, "on": "export", "form": "new",
                  "name": { "arg": 0, "key": "queueUrl" },
                  "handler": { "arg": 0, "key": "handleMessage" },
                  "key_labels": { "region": "not_name" },
                  "name_scope": { "scope": "global", "namespace": "queue" } }
            ]
        }));
        let sites = sites_of(&[(
            "src/jobs.ts",
            "import { Queue, Consumer } from \"@fixture/queue\";\n\
             export async function run() {\n\
             \x20 const orders = new Queue(\"orders\");\n\
             \x20 await orders.add(\"job\", {});\n\
             \x20 const c = new Consumer({ queueUrl: \"orders\", handleMessage: async () => {} });\n\
             \x20 c.start();\n\
             }\n\
             export function held(queue: string) {\n\
             \x20 return new Consumer({ queueUrl: queue, handleMessage: async () => {} });\n\
             }\n",
        )]);
        let exports = [queue, consumer];
        let rows = rows_of(&sites, &exports, all);
        let stated: Vec<(u32, LibraryRowKind, &str)> = rows
            .iter()
            .map(|row| (row.line, row.kind, row.name.as_str()))
            .collect();
        assert_eq!(
            stated,
            vec![
                (4, LibraryRowKind::Pubsub(PubsubRole::Publisher), "orders"),
                (5, LibraryRowKind::Pubsub(PubsubRole::Subscriber), "orders"),
            ],
            "{rows:#?}"
        );
        assert_eq!(rows[0].name_scope, rows[1].name_scope);
        assert!(rows[0].name_scope.is_some(), "{rows:#?}");
        assert!(
            rows[1].claim_ids[0].starts_with("@fixture/queue@2:Consumer:op:receive:"),
            "{rows:#?}"
        );
        assert!(rows[1].claim_ids[0].ends_with(":form:new"), "{rows:#?}");

        let refused = rows_of(&sites, &exports, |check| {
            !check.claim_id.contains(":op:receive:")
        });
        let kinds: Vec<LibraryRowKind> = refused.iter().map(|row| row.kind).collect();
        assert_eq!(
            kinds,
            vec![LibraryRowKind::Pubsub(PubsubRole::Publisher)],
            "{refused:#?}"
        );
    }

    /// carrick#3547 S2: a local queue's send keeps its publisher row when
    /// another function's parameter of the same name is handed on (here to
    /// a consumer, whose name a parameter states nothing). Before, the
    /// parameter's use contested the local by name and the row was lost.
    #[test]
    fn a_local_send_keeps_its_row_beside_a_parameter_of_its_name() {
        let queue = export(json!({
            "package": "@fixture/queue", "version": "2.0.0", "specifier": "@fixture/queue",
            "export": "Queue", "role": "broker",
            "claims": [
                { "kind": "make", "form": "new", "member": null, "name": { "arg": 0 },
                  "name_scope": { "scope": "global", "namespace": "queue" } },
                { "kind": "op", "op": "send", "member": "add", "on": "instance",
                  "name": { "bound": "maker" }, "payload": { "arg": 1 } }
            ]
        }));
        let consumer = export(json!({
            "package": "@fixture/queue", "version": "2.0.0", "specifier": "@fixture/queue",
            "export": "Consumer", "role": "broker",
            "claims": [
                { "kind": "op", "op": "receive", "member": null, "on": "export", "form": "new",
                  "name": { "arg": 0, "key": "queueUrl" },
                  "handler": { "arg": 0, "key": "handleMessage" },
                  "key_labels": { "region": "not_name" },
                  "name_scope": { "scope": "global", "namespace": "queue" } }
            ]
        }));
        let sites = sites_of(&[(
            "src/jobs.ts",
            "import { Queue, Consumer } from \"@fixture/queue\";\n\
             export async function run() {\n\
             \x20 const queue = new Queue(\"orders\");\n\
             \x20 await queue.add(\"job\", {});\n\
             }\n\
             export function held(queue: string) {\n\
             \x20 return new Consumer({ queueUrl: queue, handleMessage: async () => {} });\n\
             }\n",
        )]);
        let rows = rows_of(&sites, &[queue, consumer], all);
        let stated: Vec<(u32, LibraryRowKind, &str)> = rows
            .iter()
            .map(|row| (row.line, row.kind, row.name.as_str()))
            .collect();
        assert_eq!(
            stated,
            vec![(4, LibraryRowKind::Pubsub(PubsubRole::Publisher), "orders")],
            "{rows:#?}"
        );
        assert!(rows[0].name_scope.is_some(), "{rows:#?}");
    }

    /// carrick#3559: the same as above with the queue a module `const`
    /// (unexported): a function that declares a parameter of its name no
    /// longer takes the module's row away.
    #[test]
    fn a_module_send_keeps_its_row_beside_a_parameter_of_its_name() {
        let queue = export(json!({
            "package": "@fixture/queue", "version": "2.0.0", "specifier": "@fixture/queue",
            "export": "Queue", "role": "broker",
            "claims": [
                { "kind": "make", "form": "new", "member": null, "name": { "arg": 0 },
                  "name_scope": { "scope": "global", "namespace": "queue" } },
                { "kind": "op", "op": "send", "member": "add", "on": "instance",
                  "name": { "bound": "maker" }, "payload": { "arg": 1 } }
            ]
        }));
        let consumer = export(json!({
            "package": "@fixture/queue", "version": "2.0.0", "specifier": "@fixture/queue",
            "export": "Consumer", "role": "broker",
            "claims": [
                { "kind": "op", "op": "receive", "member": null, "on": "export", "form": "new",
                  "name": { "arg": 0, "key": "queueUrl" },
                  "handler": { "arg": 0, "key": "handleMessage" },
                  "key_labels": { "region": "not_name" },
                  "name_scope": { "scope": "global", "namespace": "queue" } }
            ]
        }));
        let sites = sites_of(&[(
            "src/jobs.ts",
            "import { Queue, Consumer } from \"@fixture/queue\";\n\
             const queue = new Queue(\"orders\");\n\
             export async function run() {\n\
             \x20 await queue.add(\"job\", {});\n\
             }\n\
             export function held(queue: string) {\n\
             \x20 return new Consumer({ queueUrl: queue, handleMessage: async () => {} });\n\
             }\n",
        )]);
        let rows = rows_of(&sites, &[queue, consumer], all);
        let stated: Vec<(u32, LibraryRowKind, &str)> = rows
            .iter()
            .map(|row| (row.line, row.kind, row.name.as_str()))
            .collect();
        assert_eq!(
            stated,
            vec![(4, LibraryRowKind::Pubsub(PubsubRole::Publisher), "orders")],
            "{rows:#?}"
        );
        assert!(rows[0].name_scope.is_some(), "{rows:#?}");
    }

    /// Carrick#3049: a queue export written on a `type:` receiver, with
    /// `receiver_types` naming the instance id that holds it, reads the same
    /// rows as today's form written on the instance id, once lowered. Without
    /// the lowering the type form states no row.
    #[test]
    fn a_type_receiver_row_reads_the_rows_of_the_instance_form() {
        let queue = |converted: bool| {
            let mut value = json!({
                "package": "@fixture/queue", "version": "2.0.0", "specifier": "@fixture/queue",
                "export": "Queue", "role": "broker",
                "claims": [
                    { "kind": "make", "form": "new", "member": null, "name": { "arg": 0 },
                      "name_scope": { "scope": "global", "namespace": "queue" } },
                    { "kind": "op", "op": "send", "member": "add",
                      "of": if converted { "type:q#Queue" } else { "instance:new" },
                      "name": { "bound": "maker" }, "payload": { "arg": 1 } }
                ]
            });
            if converted {
                value["receiver_types"] = json!({ "instance:new": ["type:q#Queue"] });
            }
            let mut export = export(value);
            let shadowed = lower_type_receivers(&mut export.claims, &export.receiver_types);
            assert_eq!(shadowed, 0);
            export
        };
        let sites = sites_of(&[(
            "src/mail.ts",
            "import { Queue } from \"@fixture/queue\";\n\
             const emails = new Queue(\"emails\");\n\
             export async function welcome() { await emails.add(\"welcome\", {}); }\n",
        )]);
        let today = rows_of(&sites, &[queue(false)], all);
        assert_eq!(today.len(), 1, "{today:#?}");
        assert_eq!(rows_of(&sites, &[queue(true)], all), today);
        let mut unlowered = queue(true);
        unlowered.claims.truncate(2);
        assert!(rows_of(&sites, &[unlowered], all).is_empty());
    }

    /// A definition (the export's maker called with a name and a handler)
    /// is the subscriber, and marks itself a definition.
    #[test]
    fn a_broker_definition_is_its_subscriber() {
        let jobs = export(json!({
            "package": "@fixture/jobs", "version": "3.1.0", "specifier": "@fixture/jobs",
            "export": "task", "role": "broker",
            "claims": [
                { "kind": "make", "form": "call", "member": null,
                  "name": { "arg": 0, "key": "id" }, "handler": { "arg": 0, "key": "run" },
                  "name_scope": { "scope": "service", "namespace": "task" } }
            ]
        }));
        let sites = sites_of(&[(
            "src/tasks.ts",
            "import { task } from \"@fixture/jobs\";\n\
             export const sendEmail = task({ id: \"send-email\", run: async () => {} });\n\
             export const noHandler = task({ id: \"no-handler\" });\n",
        )]);
        assert!(
            rows_of(&sites, std::slice::from_ref(&jobs), |check| {
                !check.claim_id.contains(":make:")
            })
            .is_empty(),
            "an unverified maker defines nothing"
        );
        let rows = rows_of(&sites, std::slice::from_ref(&jobs), all);
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].name, "send-email");
        assert!(rows[0].definition);
        assert_eq!(rows[0].kind, LibraryRowKind::Pubsub(PubsubRole::Subscriber));
        assert_eq!(
            rows[0].name_scope.as_ref().map(|scope| scope.scope),
            Some(NameScopeKind::Service)
        );
        let mut socket = jobs.clone();
        socket.role = LibraryRole::Socket;
        socket.side = Some(Side::Server);
        assert!(
            rows_of(&sites, &[socket], all).is_empty(),
            "only a broker's maker defines"
        );
    }

    /// A socket's direction is its export's side's; one that serves both
    /// sides, or says nothing, states nothing.
    #[test]
    fn a_socket_s_side_gives_the_direction() {
        let socket = |side: Option<&str>| {
            export(json!({
                "package": "@fixture/socket", "version": "4.0.0", "specifier": "@fixture/socket",
                "export": "client", "role": "socket", "side": side,
                "claims": [
                    { "kind": "op", "op": "send", "member": "emit", "on": "export",
                      "name": { "arg": 0 }, "payload": { "arg": 1 },
                      "name_scope": { "scope": "global", "namespace": null } },
                    { "kind": "op", "op": "receive", "member": "on", "on": "export",
                      "name": { "arg": 0 }, "handler": { "arg": 1 },
                      "name_scope": { "scope": "global", "namespace": null } }
                ]
            }))
        };
        let sites = sites_of(&[(
            "src/live.ts",
            "import { client } from \"@fixture/socket\";\n\
             export function go() {\n\
             \x20 client.emit(\"chat\", { text: \"hi\" });\n\
             \x20 client.on(\"chat\", (message) => {});\n\
             }\n",
        )]);
        let kinds = |side| -> Vec<LibraryRowKind> {
            rows_of(&sites, &[socket(side)], all)
                .into_iter()
                .map(|row| row.kind)
                .collect()
        };
        assert_eq!(
            kinds(Some("client")),
            vec![
                LibraryRowKind::Socket {
                    direction: SocketDirection::ClientToServer,
                    listener: false
                },
                LibraryRowKind::Socket {
                    direction: SocketDirection::ServerToClient,
                    listener: true
                },
            ]
        );
        assert_eq!(
            kinds(Some("server")),
            vec![
                LibraryRowKind::Socket {
                    direction: SocketDirection::ServerToClient,
                    listener: false
                },
                LibraryRowKind::Socket {
                    direction: SocketDirection::ClientToServer,
                    listener: true
                },
            ]
        );
        // A library that serves both ends gives the unknown direction, as the
        // site-row writer does (carrick#3498); one that says nothing, none.
        assert_eq!(
            kinds(Some("both")),
            vec![
                LibraryRowKind::Socket {
                    direction: SocketDirection::Unknown,
                    listener: false
                },
                LibraryRowKind::Socket {
                    direction: SocketDirection::Unknown,
                    listener: true
                },
            ]
        );
        assert!(kinds(None).is_empty());
    }

    /// A socket's event names are the contract between services: a claim
    /// that states no scope writes none, so a server's emit in one service
    /// pairs with a client's listener in another, and a client's emit with
    /// the server's handler (carrick#3531).
    #[test]
    fn a_silent_socket_claim_writes_no_scope_and_crosses_services() {
        let socket = |export_name: &str, side: &str| {
            export(json!({
                "package": "@fixture/socket", "version": "4.0.0", "specifier": "@fixture/socket",
                "export": export_name, "role": "socket", "side": side,
                "claims": [
                    { "kind": "op", "op": "send", "member": "emit", "on": "export",
                      "name": { "arg": 0 }, "payload": { "arg": 1 } },
                    { "kind": "op", "op": "receive", "member": "on", "on": "export",
                      "name": { "arg": 0 }, "handler": { "arg": 1 } }
                ]
            }))
        };
        let server = sites_of(&[(
            "src/hub.ts",
            "import { server } from \"@fixture/socket\";\n\
             export function go() {\n\
             \x20 server.emit(\"job:notify\", { id: 1 });\n\
             \x20 server.on(\"job:subscribe\", (id) => {});\n\
             }\n",
        )]);
        let client = sites_of(&[(
            "src/watch.ts",
            "import { client } from \"@fixture/socket\";\n\
             export function go() {\n\
             \x20 client.emit(\"job:subscribe\", 1);\n\
             \x20 client.on(\"job:notify\", (job) => {});\n\
             }\n",
        )]);
        let hub = rows_of(&server, &[socket("server", "server")], all);
        let watch = rows_of(&client, &[socket("client", "client")], all);
        assert_eq!(hub.len(), 2, "{hub:#?}");
        assert_eq!(watch.len(), 2, "{watch:#?}");
        let scope = |row: &LibraryRow| row.wire_scope();
        for row in hub.iter().chain(&watch) {
            assert_eq!(scope(row), None, "{row:#?}");
        }
        let pairs = |call: &LibraryRow, call_service, endpoint: &LibraryRow, endpoint_service| {
            let (call_scope, endpoint_scope) = (scope(call), scope(endpoint));
            call.name == endpoint.name
                && carrick_match::names_pair(
                    carrick_match::ScopedRow {
                        service: call_service,
                        name_scope: call_scope.as_ref(),
                    },
                    carrick_match::ScopedRow {
                        service: endpoint_service,
                        name_scope: endpoint_scope.as_ref(),
                    },
                )
        };
        // The server's emit reaches the client's listener; the client's
        // emit reaches the server's handler.
        assert!(pairs(&hub[0], "hub", &watch[1], "watch"));
        assert!(pairs(&watch[0], "watch", &hub[1], "hub"));
    }

    /// An export the store marks `by_origin` has no side of its own, so the
    /// side it states is never read as one (carrick#3011).
    #[test]
    fn a_by_origin_export_s_stated_side_is_never_its_side() {
        let socket = export(json!({
            "package": "@fixture/socket", "version": "4.0.0", "specifier": "@fixture/socket",
            "export": "client", "role": "socket", "side": "client", "side_rule": "by_origin",
            "claims": [
                { "kind": "op", "op": "send", "member": "emit", "on": "export",
                  "name": { "arg": 0 }, "payload": { "arg": 1 },
                  "name_scope": { "scope": "global", "namespace": null } },
                { "kind": "op", "op": "receive", "member": "on", "on": "export",
                  "name": { "arg": 0 }, "handler": { "arg": 1 },
                  "name_scope": { "scope": "global", "namespace": null } }
            ]
        }));
        let sites = sites_of(&[(
            "src/live.ts",
            "import { client } from \"@fixture/socket\";\n\
             export function go() {\n\
             \x20 client.emit(\"chat\", { text: \"hi\" });\n\
             \x20 client.on(\"chat\", (message) => {});\n\
             }\n",
        )]);
        assert!(rows_of(&sites, &[socket], all).is_empty());
    }

    /// An in-process bus's rows are service-scoped whatever the claim says
    /// (D4), and a listener nothing in the service sends to on the same
    /// receiver states nothing.
    #[test]
    fn an_in_process_bus_stays_in_its_service() {
        let bus = export(json!({
            "package": "@fixture/bus", "version": "1.0.0", "specifier": "@fixture/bus",
            "export": "bus", "role": "in_process_bus",
            "claims": [
                { "kind": "op", "op": "send", "member": "publish", "on": "export",
                  "name": { "arg": 0 }, "payload": { "arg": 1 },
                  "name_scope": { "scope": "global", "namespace": "event" } },
                { "kind": "op", "op": "receive", "member": "subscribe", "on": "export",
                  "name": { "arg": 0 }, "handler": { "arg": 1 },
                  "name_scope": { "scope": "global", "namespace": "event" } }
            ]
        }));
        let sites = sites_of(&[(
            "src/events.ts",
            "import { bus } from \"@fixture/bus\";\n\
             export function go() {\n\
             \x20 bus.publish(\"ready\", {});\n\
             \x20 bus.subscribe(\"ready\", () => {});\n\
             \x20 bus.subscribe(\"never-sent\", () => {});\n\
             }\n",
        )]);
        let rows = rows_of(&sites, &[bus], all);
        let stated: Vec<(u32, &str)> = rows
            .iter()
            .map(|row| (row.line, row.name.as_str()))
            .collect();
        assert_eq!(stated, vec![(3, "ready"), (4, "ready")], "{rows:#?}");
        for row in &rows {
            assert_eq!(
                row.name_scope,
                Some(NameScope {
                    scope: NameScopeKind::Service,
                    namespace: Some("event".to_string()),
                })
            );
        }
    }

    /// A receiver that is one of a few makers' instances states a fact only
    /// when every maker verified and every one reads the same name (contract
    /// amendment 3): the rows rest on every maker's claim.
    #[test]
    fn a_set_of_makers_states_a_row_only_when_every_maker_reads_the_same() {
        let queue = export(json!({
            "package": "@fixture/queue", "version": "2.0.0", "specifier": "@fixture/queue",
            "export": "Queue", "role": "broker",
            "claims": [
                { "kind": "make", "form": "new", "member": null, "name": { "arg": 0 } },
                { "kind": "make", "form": "new", "member": "Cluster", "name": { "arg": 0 } },
                { "kind": "op", "op": "send", "member": "add", "on": "instance",
                  "name": { "bound": "maker" }, "payload": { "arg": 1 } }
            ]
        }));
        let service = |cluster: &str| {
            let factory = format!(
                "import {{ Queue }} from \"@fixture/queue\";\n\
                 export function createQueue(flag: boolean) {{\n\
                 \x20 let made;\n\
                 \x20 if (flag) {{ made = new Queue.Cluster(\"{cluster}\"); }} else {{ made = new Queue(\"events\"); }}\n\
                 \x20 return made;\n\
                 }}\n"
            );
            sites_of(&[
                ("src/queues.ts", factory.as_str()),
                (
                    "src/use.ts",
                    "import { createQueue } from \"./queues\";\n\
                     export async function go() {\n\
                     \x20 const held = createQueue(true);\n\
                     \x20 await held.add(\"tick\", {});\n\
                     }\n",
                ),
            ])
        };
        let sites = service("events");
        let rows: Vec<LibraryRow> = rows_of(&sites, std::slice::from_ref(&queue), all)
            .into_iter()
            .filter(|row| row.file.ends_with("src/use.ts"))
            .collect();
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].name, "events");
        assert_eq!(
            rows[0].claim_ids,
            vec![
                "@fixture/queue@2:Queue:make:new:()",
                "@fixture/queue@2:Queue:make:new:Cluster",
                "@fixture/queue@2:Queue:op:send:add:on:instance",
            ]
        );
        let one_maker = rows_of(&sites, std::slice::from_ref(&queue), |check| {
            !check.claim_id.ends_with(":make:new:Cluster")
        });
        assert!(
            one_maker
                .iter()
                .all(|row| !row.file.ends_with("src/use.ts")),
            "a maker unverified: {one_maker:#?}"
        );
        let apart = service("other");
        let rows = rows_of(&apart, &[queue], all);
        assert!(
            rows.iter().all(|row| !row.file.ends_with("src/use.ts")),
            "each maker names another queue: {rows:#?}"
        );
    }

    /// Two verified op elements that apply to one call and read different
    /// names state nothing: the call names one topic or the claims disagree.
    #[test]
    fn op_elements_that_read_different_names_state_nothing() {
        let sites = sites_of(&[(
            "src/orders.ts",
            "import { bus } from \"@fixture/bus\";\n\
             export async function send() { await bus.publish(\"orders.created\", { topic: \"orders.v2\" }); }\n",
        )]);
        let control = rows_of(&sites, &[bus()], all);
        assert_eq!(control.len(), 1, "{control:#?}");
        let mut both = bus();
        both.claims.push(
            serde_json::from_value(json!({
                "kind": "op", "op": "send", "member": "publish", "of": "export",
                "name": { "arg": 1, "key": "topic" }, "payload": { "arg": 1 }
            }))
            .expect("an op"),
        );
        assert!(rows_of(&sites, &[both], all).is_empty());
    }

    /// A row's scope is written as the index reads it (carrick#1663's
    /// `NameScope`): `global` or `service`, with its namespace.
    #[test]
    fn a_row_s_scope_is_written_as_the_index_reads_it() {
        let sites = sites_of(&[("src/orders.ts", BUS_SERVICE)]);
        let rows = rows_of(&sites, &[bus()], all);
        assert_eq!(
            rows[0].wire_scope(),
            Some(carrick_match::NameScope {
                scope: "global".to_string(),
                namespace: None
            })
        );
        let service = LibraryRow {
            name_scope: Some(NameScope {
                scope: NameScopeKind::Service,
                namespace: Some("task".to_string()),
            }),
            ..rows[0].clone()
        };
        assert_eq!(
            service.wire_scope(),
            Some(carrick_match::NameScope {
                scope: "service".to_string(),
                namespace: Some("task".to_string())
            })
        );
        let silent = LibraryRow {
            name_scope: None,
            ..rows[0].clone()
        };
        assert_eq!(silent.wire_scope(), None);
    }

    /// Only the message roles state rows here: an HTTP client's export, even
    /// with claims that would read as a send, asks and states nothing.
    #[test]
    fn an_http_client_states_nothing_here() {
        let mut http = bus();
        http.role = LibraryRole::HttpClient;
        let sites = sites_of(&[("src/orders.ts", BUS_SERVICE)]);
        let asked = std::cell::RefCell::new(Vec::new());
        let rows = read(&sites, &[http], None, verifier(&asked, all));
        assert!(rows.rows.is_empty());
        assert!(
            asked.borrow().is_empty(),
            "nothing is asked for an HTTP export"
        );
    }

    /// carrick#2436: the store answers every network role, and a claim of a role other than the message roles (an HTTP
    /// client's, a server framework's) feeds discovery and the site rows
    /// only. Here it asks no check and states no library row: the rows are
    /// those of the message-role claims alone, byte for byte.
    #[test]
    fn claims_of_the_roles_site_questions_add_state_no_library_row() {
        let http = export(json!({
            "package": "@fixture/http", "version": "2.0.0", "specifier": "@fixture/http",
            "export": "http", "role": "http_client", "side": "client",
            "claims": [
                { "kind": "op", "op": "send", "member": "publish", "on": "export",
                  "name": { "arg": 0 }, "payload": { "arg": 1 } },
                { "kind": "op", "op": "request", "member": "get", "on": "export",
                  "name": { "arg": 0 }, "method": "GET" }
            ]
        }));
        let server = export(json!({
            "package": "@fixture/server", "version": "3.0.0", "specifier": "@fixture/server",
            "export": "app", "role": "server_framework", "side": "server",
            "claims": [
                { "kind": "op", "op": "receive", "member": "subscribe", "on": "export",
                  "name": { "arg": 0 }, "handler": { "arg": 1 } }
            ]
        }));
        let sites = sites_of(&[(
            "src/orders.ts",
            "import { bus } from \"@fixture/bus\";\n\
             import { http } from \"@fixture/http\";\n\
             import { app } from \"@fixture/server\";\n\
             export async function send() {\n\
             \x20 await bus.publish(\"orders.created\", { id: 1 });\n\
             \x20 await http.publish(\"orders.created\", { id: 1 });\n\
             \x20 await http.get(\"/orders\");\n\
             \x20 app.subscribe(\"orders.created\", (message) => {});\n\
             }\n",
        )]);
        let message = vec![bus()];
        let network = vec![bus(), http, server];
        let asked_message = std::cell::RefCell::new(Vec::new());
        let asked_network = std::cell::RefCell::new(Vec::new());
        let from_message = read(&sites, &message, None, verifier(&asked_message, all));
        let from_network = read(&sites, &network, None, verifier(&asked_network, all));
        assert!(!from_message.rows.is_empty(), "the bus states its rows");
        assert_eq!(from_network, from_message);
        assert_eq!(
            format!("{:?}", from_network.rows),
            format!("{:?}", from_message.rows)
        );
        assert_eq!(*asked_network.borrow(), *asked_message.borrow());
    }

    /// An op claimed on one receiver by its id (`of`) is read only through
    /// that receiver, and a name a scope member binds is carrick#1678's.
    #[test]
    fn of_names_one_receiver_and_a_scope_bound_name_states_nothing() {
        let queue = |of: &str| {
            export(json!({
                "package": "@fixture/queue", "version": "2.0.0", "specifier": "@fixture/queue",
                "export": "Queue", "role": "broker",
                "claims": [
                    { "kind": "make", "form": "new", "member": null, "name": { "arg": 0 } },
                    { "kind": "make", "form": "new", "member": "Cluster", "name": { "arg": 0 } },
                    { "kind": "op", "op": "send", "member": "add", "of": of,
                      "name": { "arg": 0 }, "payload": { "arg": 1 } },
                    { "kind": "op", "op": "send", "member": "emit", "on": "instance",
                      "name": { "bound": "scope" }, "payload": { "arg": 0 } }
                ]
            }))
        };
        let sites = sites_of(&[(
            "src/mail.ts",
            "import { Queue } from \"@fixture/queue\";\n\
             const emails = new Queue(\"emails\");\n\
             export async function welcome() {\n\
             \x20 await emails.add(\"welcome\", {});\n\
             \x20 emails.emit({});\n\
             }\n",
        )]);
        let names = |of| -> Vec<String> {
            rows_of(&sites, &[queue(of)], all)
                .into_iter()
                .map(|row| row.name)
                .collect()
        };
        assert_eq!(names("instance:new"), vec!["welcome".to_string()]);
        let scopes: Vec<Option<NameScope>> = rows_of(&sites, &[queue("instance:new")], all)
            .into_iter()
            .map(|row| row.name_scope)
            .collect();
        assert_eq!(
            scopes,
            vec![None],
            "a broker claim that states no scope writes none (carrick#3531)"
        );
        assert!(names("instance:new:Cluster").is_empty());
    }

    /// The `resolve-text` fixture's `web` service with `files` added (paths
    /// under the service) and every test package declared, read with the
    /// compiler's rows or with none, then through `exports` with every check
    /// verified and every name slot read through the text reader
    /// (carrick#3504 B10-3). `None` where the sidecar is not built.
    fn read_walked(
        files: &[(&str, &str)],
        exports: &[ExportClaims],
        with_rows: bool,
    ) -> Option<(LibraryRows, PathBuf)> {
        use crate::request_summary::fixture_walk::Rows;
        read_walked_over(
            files,
            exports,
            if with_rows {
                Rows::Walked
            } else {
                Rows::Absent
            },
        )
    }

    /// [`read_walked`] with the service's rows from `rows`.
    fn read_walked_over(
        files: &[(&str, &str)],
        exports: &[ExportClaims],
        rows: crate::request_summary::fixture_walk::Rows,
    ) -> Option<(LibraryRows, PathBuf)> {
        let mut edit: Vec<(&str, &str)> = vec![("package.json", MANIFEST)];
        edit.extend_from_slice(files);
        let on = crate::request_summary::fixture_walk::walked_over(
            "resolve-text",
            "services/web",
            "web",
            &edit,
            rows,
        )?;
        let sites = library_sites(&on.inputs);
        let names = SiteNames::of(&on.inputs);
        let asked = std::cell::RefCell::new(Vec::new());
        let rows = read(&sites, exports, Some(&names), verifier(&asked, all));
        Some((rows, on.repo.clone()))
    }

    /// The rows by file name, line and name, sorted.
    fn placed(rows: &LibraryRows) -> Vec<(String, u32, String)> {
        let mut out: Vec<(String, u32, String)> = rows
            .rows
            .iter()
            .map(|row| {
                let file = row.file.file_name().unwrap().to_string_lossy().into_owned();
                (file, row.line, row.name.clone())
            })
            .collect();
        out.sort();
        out
    }

    const NAMES_WRAPPER: &str = "import { bus } from \"@fixture/bus\";\n\
        export function send(topic: string) { bus.publish(topic, {}); }\n\
        export function sendOrder(kind: string) { send(`orders.${kind}`); }\n";

    /// A name the text reader reads through callers is stated at the
    /// outermost call of each caller chain, a constant at the call itself,
    /// and nothing falls back to today's reading (carrick#3504 B10-3).
    #[test]
    fn a_name_read_through_callers_is_stated_where_each_chain_settles() {
        let Some((rows, _)) = read_walked(
            &[
                ("src/wrapper.ts", NAMES_WRAPPER),
                (
                    "src/use.ts",
                    "import { bus } from \"@fixture/bus\";\n\
                     import { send, sendOrder } from \"./wrapper\";\n\
                     const TOPIC = \"orders.const\";\n\
                     export function a() {\n\
                     \x20 send(\"orders.direct\");\n\
                     \x20 sendOrder(\"shipped\");\n\
                     \x20 bus.publish(TOPIC, {});\n\
                     }\n",
                ),
            ],
            &[bus()],
            true,
        ) else {
            return;
        };
        assert_eq!(
            placed(&rows),
            vec![
                ("use.ts".to_string(), 5, "orders.direct".to_string()),
                ("use.ts".to_string(), 6, "orders.shipped".to_string()),
                ("use.ts".to_string(), 7, "orders.const".to_string()),
            ],
            "{rows:#?}"
        );
        assert_eq!(rows.names.count(NameOutcome::NoRows), 0, "{:?}", rows.names);
        assert_eq!(rows.names.count(NameOutcome::Filled), 1, "{:?}", rows.names);
        assert!(rows.leads.is_empty(), "{:?}", rows.leads);
    }

    /// A wrapper with seventeen callers names seventeen sites: the cap
    /// counts within one caller chain (carrick#3504 B10-2a2, Q6), so the text
    /// reader reads each caller's name (a pin: today's reading names them
    /// too).
    #[test]
    fn a_wrapper_with_seventeen_callers_names_seventeen_sites() {
        let mut calls =
            String::from("import { send } from \"./wrapper\";\nexport function many() {\n");
        for at in 0..17 {
            calls.push_str(&format!("  send(\"wide.{at}\");\n"));
        }
        calls.push_str("}\n");
        let Some((rows, _)) = read_walked(
            &[("src/wrapper.ts", NAMES_WRAPPER), ("src/many.ts", &calls)],
            &[bus()],
            true,
        ) else {
            return;
        };
        let named: BTreeSet<String> = rows.rows.iter().map(|row| row.name.clone()).collect();
        assert_eq!(named.len(), 17, "{rows:#?}");
        assert!(named.iter().all(|name| name.starts_with("wide.")));
        assert_eq!(rows.names.count(NameOutcome::NoRows), 0, "{:?}", rows.names);
        // Nothing calls `sendOrder`: its call of `send` is a lead.
        let leads: Vec<(u32, Stop)> = rows
            .leads
            .iter()
            .map(|lead| (lead.line, lead.reason))
            .collect();
        assert_eq!(leads, vec![(3, Stop::NameNeverBound)], "{:?}", rows.leads);
    }

    /// A name the text reader states no text for, with the compiler's rows,
    /// is a lead with the hole's word, never a row: an environment read
    /// beside a default, and a choice between two names at one place
    /// (`several_operations`). The leads go into the unread sites, slot
    /// `name` (carrick#3504 B10-3, Q-B3c).
    #[test]
    fn a_name_the_reader_cannot_read_is_a_lead_with_its_stop() {
        let Some((rows, repo)) = read_walked(
            &[(
                "src/leads.ts",
                "import { bus } from \"@fixture/bus\";\n\
                 declare const settings: { topic: string };\n\
                 export function fromSettings() { bus.publish(settings.topic ?? \"orders\", {}); }\n\
                 export function choose(flag: boolean) { bus.publish(flag ? \"a.one\" : \"a.two\", {}); }\n",
            )],
            &[bus()],
            true,
        ) else {
            return;
        };
        assert!(rows.rows.is_empty(), "{rows:#?}");
        let mut leads: Vec<(u32, Stop)> = rows
            .leads
            .iter()
            .map(|lead| (lead.line, lead.reason))
            .collect();
        leads.sort();
        assert_eq!(leads.len(), 2, "{:?}", rows.leads);
        assert_eq!(leads[0].0, 3);
        assert_ne!(leads[0].1, Stop::RowsMissing);
        assert_eq!(leads[1], (4, Stop::SeveralOperations));
        assert_eq!(rows.names.count(NameOutcome::NoRows), 0, "{:?}", rows.names);

        let unread = crate::unread_sites::from_scan(
            &crate::request_summary::discovery::Discovery::default(),
            &crate::request_summary::decided_sites::Decided::default(),
            &[],
            &rows.leads,
            &repo,
        );
        let mut saved: Vec<String> = unread
            .iter()
            .map(|site| {
                format!(
                    "{}:{} {} {:?} {:?}",
                    site.file, site.line, site.slot, site.kind, site.role
                )
            })
            .collect();
        saved.sort();
        let file = "services/web/src/leads.ts";
        assert_eq!(
            saved,
            vec![
                format!("{file}:3 name Some(\"pubsub\") Some(\"producer\")"),
                format!("{file}:4 name Some(\"pubsub\") Some(\"producer\")"),
            ]
        );
    }

    /// Where the walk gave the text reader no compiler rows, today's reading
    /// stands, counted `no_rows`, and no lead is saved (carrick#3504 B10-3,
    /// Q-B3a): a failed walk loses no row.
    #[test]
    fn a_slot_without_rows_keeps_today_s_name_counted_no_rows() {
        let Some((rows, _)) = read_walked(
            &[
                ("src/wrapper.ts", NAMES_WRAPPER),
                (
                    "src/use.ts",
                    "import { bus } from \"@fixture/bus\";\n\
                     import { send } from \"./wrapper\";\n\
                     const TOPIC = \"orders.const\";\n\
                     export function a() {\n\
                     \x20 send(\"orders.direct\");\n\
                     \x20 bus.publish(TOPIC, {});\n\
                     }\n",
                ),
            ],
            &[bus()],
            false,
        ) else {
            return;
        };
        assert_eq!(
            placed(&rows),
            vec![
                ("use.ts".to_string(), 5, "orders.direct".to_string()),
                ("use.ts".to_string(), 6, "orders.const".to_string()),
            ],
            "{rows:#?}"
        );
        // The wrapper's call, the caller that fills it, and the constant.
        assert_eq!(rows.names.count(NameOutcome::NoRows), 3, "{:?}", rows.names);
        assert!(rows.leads.is_empty(), "{:?}", rows.leads);
    }

    /// carrick#4256, carrick#4452: a service whose walk was asked and wrote
    /// no rows keeps today's reading of a name slot as one with no walk
    /// does: the same rows, the same `no_rows` count and the same leads,
    /// each lead saying `walk_lost` where the other says `rows_missing`.
    #[test]
    fn a_slot_over_a_lost_walk_keeps_today_s_names_and_says_walk_lost() {
        use crate::request_summary::fixture_walk::Rows;
        let files = [
            ("src/wrapper.ts", NAMES_WRAPPER),
            (
                "src/use.ts",
                "import { bus } from \"@fixture/bus\";\n\
                 import { send } from \"./wrapper\";\n\
                 const TOPIC = \"orders.const\";\n\
                 export function a() {\n\
                 \x20 send(\"orders.direct\");\n\
                 \x20 bus.publish(TOPIC, {});\n\
                 }\n",
            ),
        ];
        let Some((missing, _)) = read_walked_over(
            &files,
            &[bus()],
            Rows::Index(crate::position_rows::PositionIndex::missing),
        ) else {
            return;
        };
        let (lost, _) = read_walked_over(
            &files,
            &[bus()],
            Rows::Index(crate::position_rows::PositionIndex::walk_lost),
        )
        .expect("the sidecar is built");
        assert!(!placed(&missing).is_empty(), "{missing:#?}");
        assert_eq!(placed(&lost), placed(&missing), "{lost:#?}");
        assert!(missing.names.count(NameOutcome::NoRows) > 0);
        assert_eq!(
            lost.names.count(NameOutcome::NoRows),
            missing.names.count(NameOutcome::NoRows),
            "{:?}",
            lost.names
        );
        // The lookup's hole reaches the lead: a lost walk says `walk_lost`.
        let reasons = |rows: &LibraryRows| -> Vec<Stop> {
            rows.leads.iter().map(|lead| lead.reason).collect()
        };
        assert!(!missing.leads.is_empty(), "{:?}", missing.leads);
        let wanted: Vec<Stop> = reasons(&missing)
            .into_iter()
            .map(|reason| {
                if reason == Stop::RowsMissing {
                    Stop::WalkLost
                } else {
                    reason
                }
            })
            .collect();
        assert_eq!(reasons(&lost), wanted, "{:?}", lost.leads);
        assert!(reasons(&lost).contains(&Stop::WalkLost), "{:?}", lost.leads);
    }

    /// A name the maker that made the receiver was handed stays on the maker
    /// reading (`made`, carrick#3504 B10-3, Q-B3b): a factory called twice
    /// with two names names each holder's site with its own.
    #[test]
    fn a_maker_bound_queue_name_keeps_made_s_name() {
        let queue = export(json!({
            "package": "@fixture/queue", "version": "2.0.0", "specifier": "@fixture/queue",
            "export": "Queue", "role": "broker",
            "claims": [
                { "kind": "make", "form": "new", "member": null, "name": { "arg": 0 },
                  "name_scope": { "scope": "global", "namespace": "queue" } },
                { "kind": "op", "op": "send", "member": "add", "on": "instance",
                  "name": { "bound": "maker" }, "payload": { "arg": 1 } }
            ]
        }));
        let Some((rows, _)) = read_walked(
            &[(
                "src/mail.ts",
                "import { Queue } from \"@fixture/queue\";\n\
                 function createQueue(name: string) { return new Queue(name); }\n\
                 const emails = createQueue(\"emails\");\n\
                 const reports = createQueue(\"reports\");\n\
                 export async function welcome() { await emails.add(\"welcome\", {}); }\n\
                 export async function weekly() { await reports.add(\"weekly\", {}); }\n",
            )],
            &[queue],
            true,
        ) else {
            return;
        };
        assert_eq!(
            placed(&rows),
            vec![
                ("mail.ts".to_string(), 5, "emails".to_string()),
                ("mail.ts".to_string(), 6, "reports".to_string()),
            ],
            "{rows:#?}"
        );
        assert_eq!(rows.names.count(NameOutcome::Made), 2, "{:?}", rows.names);
        assert!(rows.leads.is_empty(), "{:?}", rows.leads);
    }

    /// A name an own builder returns keeps today's reading where the text
    /// reader holes the call (carrick#3504 B10-3, `r1_builder`, until F4),
    /// with the compiler's rows: a builder, a builder function, a builder in
    /// an object, one that declares a constant first, and one handed a
    /// parameter, filled at its caller.
    #[test]
    fn a_builder_name_keeps_today_s_reading_with_rows() {
        let Some((rows, _)) = read_walked(
            &[
                (
                    "src/topics.ts",
                    "export const PREFIX = \"svc\";\n\
                     export const topicFor = (kind: string) => `${PREFIX}.${kind}`;\n\
                     export function auditTopic(name: string) { return `audit.${name}`; }\n\
                     export const ROUTES = { byId: (id: string) => `jobs.${id}` };\n\
                     export function twoSteps(name: string) { const topic = `two.${name}`; return topic; }\n",
                ),
                (
                    "src/use.ts",
                    "import { bus } from \"@fixture/bus\";\n\
                     import { topicFor, auditTopic, ROUTES, twoSteps } from \"./topics\";\n\
                     export function a(kind: string) {\n\
                     \x20 bus.publish(topicFor(\"x\"), {});\n\
                     \x20 bus.publish(auditTopic(\"y\"), {});\n\
                     \x20 bus.publish(ROUTES.byId(\"7\"), {});\n\
                     \x20 bus.publish(twoSteps(\"z\"), {});\n\
                     \x20 bus.publish(topicFor(kind), {});\n\
                     }\n",
                ),
                (
                    "src/caller.ts",
                    "import { a } from \"./use\";\n\
                     export function go() { a(\"filled\"); }\n",
                ),
            ],
            &[bus()],
            true,
        ) else {
            return;
        };
        assert_eq!(
            placed(&rows),
            vec![
                ("caller.ts".to_string(), 2, "svc.filled".to_string()),
                ("use.ts".to_string(), 4, "svc.x".to_string()),
                ("use.ts".to_string(), 5, "audit.y".to_string()),
                ("use.ts".to_string(), 6, "jobs.7".to_string()),
                ("use.ts".to_string(), 7, "two.z".to_string()),
            ],
            "{rows:#?}"
        );
        // The five calls, and the caller that fills the one handed a
        // parameter.
        assert_eq!(
            rows.names.count(NameOutcome::R1Builder),
            6,
            "{:?}",
            rows.names
        );
        assert_eq!(rows.names.count(NameOutcome::NoRows), 0, "{:?}", rows.names);
        assert!(rows.leads.is_empty(), "{:?}", rows.leads);
    }

    /// An own builder's call the text reader holes, handed a parameter no
    /// caller fills with text, is a lead at the library call: today's
    /// reading states no name for it anywhere, so the builder rule keeps
    /// nothing (a pin of the rule decided once, in `today_answers`).
    #[test]
    fn a_builder_today_reads_nowhere_is_a_lead() {
        let Some((rows, _)) = read_walked(
            &[
                (
                    "src/topics.ts",
                    "export const topicFor = (kind: string) => `svc.${kind}`;\n",
                ),
                (
                    "src/use.ts",
                    "import { bus } from \"@fixture/bus\";\n\
                     import { topicFor } from \"./topics\";\n\
                     export function a(kind: string) { bus.publish(topicFor(kind), {}); }\n",
                ),
            ],
            &[bus()],
            true,
        ) else {
            return;
        };
        assert!(rows.rows.is_empty(), "{rows:#?}");
        assert_eq!(rows.leads.len(), 1, "{:?}", rows.leads);
        assert_eq!(rows.leads[0].line, 3, "{:?}", rows.leads);
        assert_eq!(rows.names.count(NameOutcome::Lead), 1, "{:?}", rows.names);
        assert_eq!(
            rows.names.count(NameOutcome::R1Builder),
            0,
            "{:?}",
            rows.names
        );
    }

    /// `files` read as [`read_walked`] reads them with the compiler's rows,
    /// twice: today's reading (no text reader) and the text reader's (its
    /// step cap at `cap`, when set).
    fn read_twice(
        files: &[(&str, &str)],
        exports: &[ExportClaims],
        cap: Option<usize>,
    ) -> Option<(
        crate::request_summary::fixture_walk::Walked,
        LibraryRows,
        LibraryRows,
    )> {
        let mut edit: Vec<(&str, &str)> = vec![("package.json", MANIFEST)];
        edit.extend_from_slice(files);
        let on = crate::request_summary::fixture_walk::walked(
            "resolve-text",
            "services/web",
            "web",
            &edit,
            true,
        )?;
        let sites = library_sites(&on.inputs);
        let asked = std::cell::RefCell::new(Vec::new());
        let today = read(&sites, exports, None, verifier(&asked, all));
        let names = match cap {
            Some(cap) => SiteNames::with_cap(&on.inputs, cap),
            None => SiteNames::of(&on.inputs),
        };
        let rowed = read(&sites, exports, Some(&names), verifier(&asked, all));
        Some((on, today, rowed))
    }

    /// Every row today's reading states, the text reader states too, at the
    /// same place.
    fn today_stands(today: &LibraryRows, rowed: &LibraryRows) {
        let read = placed(rowed);
        let lost: Vec<_> = placed(today)
            .into_iter()
            .filter(|row| !read.contains(row))
            .collect();
        assert!(
            lost.is_empty(),
            "rows today's reading states, lost: {lost:?}\n{:#?}\n{:?}",
            rowed.rows,
            rowed.names
        );
    }

    /// Two caller chains of the wrapper `send` in `wrapper` (beside the
    /// builder `topicFor`) that end at one call in `caller` (`src/use.ts`),
    /// each with its own stop: read as [`read_twice`] reads them, with
    /// every value the text reader reads that `to` names a new value for
    /// rewritten first, at its own place and chain (a differing text or the
    /// step cap, which no source here reaches offline). `None` where the
    /// sidecar is not built.
    fn two_chains(
        wrapper: &str,
        caller: &str,
        to: impl Fn(&crate::request_summary::PlacedName) -> Option<Result<String, Stop>>,
    ) -> Option<(LibraryRows, LibraryRows)> {
        two_chains_with(wrapper, caller, |names| names.rewrite(to))
    }

    /// [`two_chains`], with `change` applied to the values the text reader
    /// read before the second reading.
    fn two_chains_with(
        wrapper: &str,
        caller: &str,
        change: impl FnOnce(&SiteNames<'_>),
    ) -> Option<(LibraryRows, LibraryRows)> {
        let edit: Vec<(&str, &str)> = vec![
            ("package.json", MANIFEST),
            ("src/wrapper.ts", wrapper),
            (
                "src/topics.ts",
                "export const topicFor = (kind: string) => `svc.${kind}`;\n",
            ),
            ("src/use.ts", caller),
        ];
        let on = crate::request_summary::fixture_walk::walked(
            "resolve-text",
            "services/web",
            "web",
            &edit,
            true,
        )?;
        let sites = library_sites(&on.inputs);
        let asked = std::cell::RefCell::new(Vec::new());
        let today = read(&sites, &[bus()], None, verifier(&asked, all));
        let names = SiteNames::of(&on.inputs);
        for site in sites.sites.iter().filter(|site| site.origin.is_none()) {
            names.read(site, 0, None);
        }
        change(&names);
        let rowed = read(&sites, &[bus()], Some(&names), verifier(&asked, all));
        Some((today, rowed))
    }

    /// `from`'s text read as `to` on its chain.
    fn renamed(
        from: &'static str,
        to: Result<&'static str, Stop>,
    ) -> impl Fn(&crate::request_summary::PlacedName) -> Option<Result<String, Stop>> {
        move |value| (value.name.as_deref() == Ok(from)).then(|| to.map(str::to_string))
    }

    /// Leads by file name, line and stop.
    fn leads_of(rows: &LibraryRows) -> Vec<(String, u32, Stop)> {
        let mut out: Vec<(String, u32, Stop)> = rows
            .leads
            .iter()
            .map(|lead| {
                let file = lead
                    .file
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                (file, lead.line, lead.reason)
            })
            .collect();
        out.sort();
        out
    }

    const BUILDER_AND_PASSED: &str = "import { bus } from \"@fixture/bus\";\n\
        import { topicFor } from \"./topics\";\n\
        export function send(t: string) { bus.publish(t, {}); }\n\
        export function two(id: string, o: string) { send(topicFor(id)); send(o); }\n";

    const TWO_TEMPLATES: &str = "import { bus } from \"@fixture/bus\";\n\
        export function send(t: string) { bus.publish(t, {}); }\n\
        export function two(id: string) { send(`a.${id}`); send(`b.${id}`); }\n";

    /// A chain that stops at one call with a hole today's reading does not
    /// answer on that chain saves its lead there, even where another chain
    /// at that call holds a builder today's reading answers: a choice of two
    /// names (`several_operations`) and the step cap, beside
    /// `send(topicFor(id))` (review 2, finding 1).
    #[test]
    fn a_hole_on_one_chain_is_not_answered_by_another_chain_s_builder() {
        let caller = "import { two } from \"./wrapper\";\n\
                      export function go(f: boolean) { two(\"x\", f ? \"b.one\" : \"b.two\"); }\n";
        for (stop, to) in [
            (Stop::FanOutCap, Some(Stop::FanOutCap)),
            (Stop::SeveralOperations, None),
        ] {
            let Some((today, rowed)) = two_chains(BUILDER_AND_PASSED, caller, |value| {
                let several = value.name == Err(Stop::SeveralOperations);
                to.filter(|_| several).map(Err)
            }) else {
                return;
            };
            assert!(
                placed(&today).contains(&("use.ts".to_string(), 2, "svc.x".to_string())),
                "{today:#?}"
            );
            today_stands(&today, &rowed);
            assert!(
                leads_of(&rowed).contains(&("use.ts".to_string(), 2, stop)),
                "{stop:?}: {:?}",
                rowed.leads
            );
            assert!(
                rowed.names.count(NameOutcome::R1Builder) >= 1,
                "{:?}",
                rowed.names
            );
        }
    }

    /// Where the text reader's name differs from today's on two chains at
    /// one call, each chain's site counts `text_differs` and keeps today's
    /// name, and neither reader name is stated (review 2, finding 2;
    /// Q-B3f).
    #[test]
    fn each_differing_chain_counts_text_differs_at_its_site() {
        let caller = "import { two } from \"./wrapper\";\n\
                      export function go() { two(\"x\"); }\n";
        let Some((today, rowed)) = two_chains(TWO_TEMPLATES, caller, |value| {
            renamed("a.x", Ok("a.other"))(value).or_else(|| renamed("b.x", Ok("b.other"))(value))
        }) else {
            return;
        };
        today_stands(&today, &rowed);
        assert!(
            rowed.rows.iter().all(|row| !row.name.ends_with(".other")),
            "{:#?}",
            rowed.rows
        );
        assert_eq!(
            rowed.names.count(NameOutcome::TextDiffers),
            2,
            "{:?}",
            rowed.names
        );
        assert_eq!(
            rowed.names.count(NameOutcome::CallerToday),
            0,
            "{:?}",
            rowed.names
        );
    }

    /// A chain's lead at one call does not block another chain's fallback
    /// there: where one chain holes and the other's text differs from
    /// today's, the differing chain keeps today's row, counted
    /// `text_differs`, and the holed chain's lead is saved (review 2,
    /// finding 3).
    #[test]
    fn a_lead_on_one_chain_keeps_another_chain_s_row() {
        let caller = "import { two } from \"./wrapper\";\n\
                      export function go() { two(\"x\"); }\n";
        let Some((_, rowed)) = two_chains(TWO_TEMPLATES, caller, |value| {
            renamed("a.x", Err(Stop::UnreadExpression))(value)
                .or_else(|| renamed("b.x", Ok("b.other"))(value))
        }) else {
            return;
        };
        assert!(
            placed(&rowed).contains(&("use.ts".to_string(), 2, "b.x".to_string())),
            "{:#?}\n{:?}",
            rowed.rows,
            rowed.names
        );
        assert!(
            leads_of(&rowed).contains(&("use.ts".to_string(), 2, Stop::UnreadExpression)),
            "{:?}",
            rowed.leads
        );
        assert_eq!(
            rowed.names.count(NameOutcome::TextDiffers),
            1,
            "{:?}",
            rowed.names
        );
        // The holed chain's lead stops `caller_today` there (the `led`
        // check): today's `a.x` is dropped, counted `lead_today_stated`.
        assert!(
            !placed(&rowed).contains(&("use.ts".to_string(), 2, "a.x".to_string())),
            "{:#?}",
            rowed.rows
        );
        assert_eq!(
            rowed.names.count(NameOutcome::LeadTodayStated),
            1,
            "{:?}",
            rowed.names
        );
    }

    /// A caller site whose chain no value of the text reader answers (a
    /// hole at the library call itself, today's reading does not answer)
    /// falls back to today's row; where another chain at that call already
    /// states the same name, no row is lost, so the site is counted
    /// `filled`, not `lead_today_stated` (review 3, N3).
    #[test]
    fn a_name_another_chain_states_is_not_a_lost_row() {
        let caller = "import { two } from \"./wrapper\";\n\
                      export function go() { two(\"x\"); }\n";
        // Both chains read `a.x` today.
        let same = "import { bus } from \"@fixture/bus\";\n\
            export function send(t: string) { bus.publish(t, {}); }\n\
            export function two(id: string) { send(`a.${id}`); send(`a.${id}`); }\n";
        let holed = std::cell::Cell::new(false);
        let Some((today, rowed)) = two_chains_with(same, caller, |names| {
            names.rewrite_values(|value| {
                // The first chain's value becomes the library call's own
                // hole; the other chain keeps its own, which states the name.
                (!holed.replace(true))
                    .then(|| crate::request_summary::PlacedName {
                        place: None,
                        chain: Vec::new(),
                        name: Err(Stop::SeveralOperations),
                    })
                    .filter(|_| value.name.is_ok())
            });
        }) else {
            return;
        };
        assert!(
            placed(&today).contains(&("use.ts".to_string(), 2, "a.x".to_string())),
            "{today:#?}"
        );
        assert!(
            placed(&rowed).contains(&("use.ts".to_string(), 2, "a.x".to_string())),
            "{:#?}\n{:?}",
            rowed.rows,
            rowed.names
        );
        assert_eq!(
            rowed.names.count(NameOutcome::LeadTodayStated),
            0,
            "{:?}",
            rowed.names
        );
    }

    /// A name the text reader newly reads at a caller asks the kind, even
    /// where another library call of the same wrapper is filled there by
    /// today's reading (review 2, finding 4; Q-B3e).
    #[test]
    fn another_library_call_s_filled_caller_does_not_skip_the_kind_question() {
        let wrapper = "import { bus } from \"@fixture/bus\";\n\
                       export function w(a: string, b: string) {\n\
                       \x20 bus.publish(a, {});\n\
                       \x20 bus.publish(b, {});\n\
                       }\n";
        let caller = "import { w } from \"./wrapper\";\n\
                      export function go(f: boolean) { w(\"x\", f ? \"y\" : \"y\"); }\n";
        let Some((today, rowed)) = two_chains(wrapper, caller, |_| None) else {
            return;
        };
        assert!(
            placed(&today).contains(&("use.ts".to_string(), 2, "x".to_string())),
            "{today:#?}"
        );
        assert!(
            placed(&today).iter().all(|(_, _, name)| name != "y"),
            "today's reading reads no `y`: {today:#?}"
        );
        today_stands(&today, &rowed);
        assert!(
            placed(&rowed).iter().all(|(_, _, name)| name != "y"),
            "{:#?}",
            rowed.rows
        );
        assert!(
            rowed.names.count(NameOutcome::KindAsked) >= 1,
            "{:?}",
            rowed.names
        );
    }

    /// Two caller chains of one wrapper that settle at the same outermost
    /// call state both names there (review finding 1).
    #[test]
    fn two_chains_settling_at_one_call_state_both_names() {
        let Some((_, today, rowed)) = read_twice(
            &[
                (
                    "src/wrapper.ts",
                    "import { bus } from \"@fixture/bus\";\n\
                     export function send(t: string) { bus.publish(t, {}); }\n\
                     export function two(id: string) { send(`a.${id}`); send(`b.${id}`); }\n",
                ),
                (
                    "src/use.ts",
                    "import { two } from \"./wrapper\";\n\
                     export function go() { two(\"x\"); }\n",
                ),
            ],
            &[bus()],
            None,
        ) else {
            return;
        };
        let want = vec![
            ("use.ts".to_string(), 2, "a.x".to_string()),
            ("use.ts".to_string(), 2, "b.x".to_string()),
        ];
        assert_eq!(placed(&today), want, "{today:#?}");
        today_stands(&today, &rowed);
        assert_eq!(placed(&rowed), want);
        // Both are the text reader's rows, not today's kept.
        assert_eq!(
            rowed.names.count(NameOutcome::CallerToday),
            0,
            "{:?}",
            rowed.names
        );
    }

    /// A chain that stops at an own builder written between the library
    /// call and the caller that fills it (`send(topicFor(id))` inside
    /// `two`, filled by `two("x")`) keeps today's name at that caller, beside
    /// another chain's name there, and in a one-chain wrapper (`mid`); the
    /// lead stays at the builder's call. A name the library call writes
    /// itself, beside a payload a caller fills (`fixed`), states what today's
    /// reading states. No row is stated twice.
    #[test]
    fn a_chain_stopped_at_a_builder_between_keeps_today_s_name_at_its_caller() {
        let Some((_, today, rowed)) = read_twice(
            &[
                (
                    "src/wrapper.ts",
                    "import { bus } from \"@fixture/bus\";\n\
                     import { topicFor } from \"./topics\";\n\
                     export function send(t: string) { bus.publish(t, {}); }\n\
                     export function two(id: string) { send(`a.${id}`); send(topicFor(id)); }\n\
                     export function mid(k: string) { send(topicFor(k)); }\n\
                     export function fixed(d: unknown) { bus.publish(\"fixed.name\", d); }\n",
                ),
                (
                    "src/topics.ts",
                    "export const topicFor = (kind: string) => `svc.${kind}`;\n",
                ),
                (
                    "src/use.ts",
                    "import { two, mid, fixed } from \"./wrapper\";\n\
                     export function go() { two(\"x\"); }\n\
                     export function one() { mid(\"y\"); }\n\
                     export function payload() { fixed({}); }\n",
                ),
            ],
            &[bus()],
            None,
        ) else {
            return;
        };
        for want in [
            ("use.ts".to_string(), 2, "a.x".to_string()),
            ("use.ts".to_string(), 2, "svc.x".to_string()),
            ("use.ts".to_string(), 3, "svc.y".to_string()),
        ] {
            assert!(placed(&today).contains(&want), "{want:?} {today:#?}");
        }
        today_stands(&today, &rowed);
        let mut seen = placed(&rowed);
        seen.dedup();
        assert_eq!(seen, placed(&rowed), "no row twice");
    }

    /// A name in an object a wrapper hands on whole, beside another part
    /// the claim positions, is stated at the caller that writes the object:
    /// an op (`send({ topic, messages })`) and a definition
    /// (`task({ id, run })`) (review finding 2).
    #[test]
    fn a_name_in_an_object_handed_on_whole_is_stated_at_its_caller() {
        let producer = export(json!({
            "package": "@fixture/bus", "version": "1.4.0", "specifier": "@fixture/bus",
            "export": "producer", "role": "broker",
            "claims": [
                { "kind": "op", "op": "send", "member": "send", "on": "export",
                  "name": { "arg": 0, "key": "topic" }, "payload": { "arg": 0, "key": "messages" } }
            ]
        }));
        let jobs = export(json!({
            "package": "@fixture/jobs", "version": "3.1.0", "specifier": "@fixture/jobs",
            "export": "task", "role": "broker",
            "claims": [
                { "kind": "make", "form": "call", "member": null,
                  "name": { "arg": 0, "key": "id" }, "handler": { "arg": 0, "key": "run" },
                  "name_scope": { "scope": "service", "namespace": "task" } }
            ]
        }));
        let Some((_, today, rowed)) = read_twice(
            &[
                (
                    "src/wrapper.ts",
                    "import { producer } from \"@fixture/bus\";\n\
                     import { task } from \"@fixture/jobs\";\n\
                     export function send(r: { topic: string; messages: unknown[] }) { return producer.send(r); }\n\
                     export function define(o: { id: string; run: () => Promise<void> }) { return task(o); }\n",
                ),
                (
                    "src/use.ts",
                    "import { send, define } from \"./wrapper\";\n\
                     export function go() { send({ topic: \"orders\", messages: [] }); }\n\
                     export const job = define({ id: \"nightly\", run: async () => {} });\n",
                ),
            ],
            &[producer, jobs],
            None,
        ) else {
            return;
        };
        assert_eq!(
            placed(&today),
            vec![
                ("use.ts".to_string(), 2, "orders".to_string()),
                ("use.ts".to_string(), 3, "nightly".to_string()),
            ],
            "{today:#?}"
        );
        today_stands(&today, &rowed);
        assert_eq!(
            rowed.names.count(NameOutcome::CallerToday),
            2,
            "{:?}",
            rowed.names
        );
    }

    /// An own builder written at a wrapper's caller (`send(topicFor("x"))`)
    /// keeps today's name there, counted `r1_builder`, and saves no lead
    /// (review finding 3; Q-B3a's builder rule, until F4).
    #[test]
    fn a_builder_written_at_a_caller_keeps_today_s_name() {
        let Some((_, today, rowed)) = read_twice(
            &[
                ("src/wrapper.ts", NAMES_WRAPPER),
                (
                    "src/topics.ts",
                    "export const topicFor = (kind: string) => `svc.${kind}`;\n",
                ),
                (
                    "src/use.ts",
                    "import { send } from \"./wrapper\";\n\
                     import { topicFor } from \"./topics\";\n\
                     export function go() { send(topicFor(\"x\")); }\n",
                ),
            ],
            &[bus()],
            None,
        ) else {
            return;
        };
        assert!(
            placed(&today).contains(&("use.ts".to_string(), 3, "svc.x".to_string())),
            "{today:#?}"
        );
        today_stands(&today, &rowed);
        assert!(
            rowed
                .leads
                .iter()
                .all(|lead| !lead.file.ends_with("use.ts")),
            "{:?}",
            rowed.leads
        );
        assert!(
            rowed.names.count(NameOutcome::R1Builder) >= 1,
            "{:?}",
            rowed.names
        );
    }

    /// Past the resolver's step cap, each further caller of a wrapper keeps
    /// today's name, counted `capped`, and saves no lead (review finding
    /// 4): the cap counts across every caller of one slot.
    #[test]
    fn callers_past_the_step_cap_keep_today_s_names() {
        let mut calls =
            String::from("import { send } from \"./wrapper\";\nexport function many() {\n");
        for at in 0..17 {
            calls.push_str(&format!("  send(\"wide.{at}\");\n"));
        }
        calls.push_str("}\n");
        let Some((_, today, rowed)) = read_twice(
            &[("src/wrapper.ts", NAMES_WRAPPER), ("src/many.ts", &calls)],
            &[bus()],
            Some(8),
        ) else {
            return;
        };
        let wide = |rows: &LibraryRows| {
            rows.rows
                .iter()
                .filter(|row| row.name.starts_with("wide."))
                .count()
        };
        assert_eq!(wide(&today), 17, "{today:#?}");
        today_stands(&today, &rowed);
        assert!(
            rowed.names.count(NameOutcome::Capped) >= 1,
            "{:?}",
            rowed.names
        );
        // `sendOrder`'s call, which no caller fills, stays a lead.
        assert!(
            rowed
                .leads
                .iter()
                .all(|lead| !lead.file.ends_with("many.ts")),
            "{:?}",
            rowed.leads
        );
    }

    /// The four lost-row shapes in one service: every row today's reading
    /// states stands (review findings 1-4, together), read with the
    /// resolver's own step cap and with a cap of 8, which the wide wrapper
    /// passes (and which can stop the other shapes' chains first).
    #[test]
    fn the_four_lost_row_shapes_together_lose_no_row() {
        let producer = export(json!({
            "package": "@fixture/queue", "version": "2.0.0", "specifier": "@fixture/queue",
            "export": "producer", "role": "broker",
            "claims": [
                { "kind": "op", "op": "send", "member": "send", "on": "export",
                  "name": { "arg": 0, "key": "topic" }, "payload": { "arg": 0, "key": "messages" } }
            ]
        }));
        let mut calls = String::from(
            "import { send, two, handOn } from \"./wrapper\";\n\
             import { topicFor } from \"./topics\";\n\
             export function go() {\n\
             \x20 two(\"x\");\n\
             \x20 handOn({ topic: \"orders\", messages: [] });\n\
             \x20 send(topicFor(\"y\"));\n",
        );
        for at in 0..12 {
            calls.push_str(&format!("  send(\"wide.{at}\");\n"));
        }
        calls.push_str("}\n");
        let files = [
            (
                "src/wrapper.ts",
                "import { bus } from \"@fixture/bus\";\n\
                 import { producer } from \"@fixture/queue\";\n\
                 export function send(t: string) { bus.publish(t, {}); }\n\
                 export function two(id: string) { send(`a.${id}`); send(`b.${id}`); }\n\
                 export function handOn(r: { topic: string; messages: unknown[] }) { return producer.send(r); }\n",
            ),
            (
                "src/topics.ts",
                "export const topicFor = (kind: string) => `svc.${kind}`;\n",
            ),
            ("src/use.ts", calls.as_str()),
        ];
        for cap in [None, Some(8)] {
            let Some((_, today, rowed)) = read_twice(&files, &[bus(), producer.clone()], cap)
            else {
                return;
            };
            assert!(placed(&today).len() >= 16, "{today:#?}");
            today_stands(&today, &rowed);
            let mut seen = placed(&rowed);
            seen.dedup();
            assert_eq!(seen, placed(&rowed), "no row twice ({cap:?})");
        }
    }

    /// A text differs from today's only where today's reading states one
    /// and none of what it states is the text.
    #[test]
    fn a_text_differs_only_from_a_stated_other_text() {
        assert!(!text_differs(&[], "a"));
        assert!(!text_differs(&["a"], "a"));
        assert!(!text_differs(&["b", "a"], "a"));
        assert!(text_differs(&["b"], "a"));
    }

    /// Where the text reader's name differs from today's at a caller, the
    /// reader states no name there: today's is the only row, and the site
    /// counts `text_differs` (Q-B3f). No source shape the readers split on
    /// is reachable offline, so this pins the decision both call sites in
    /// `name()` use.
    #[test]
    fn a_differing_name_is_not_stated_beside_today_s() {
        assert_eq!(reader_name(&[], "a".into()), Some("a".into()));
        assert_eq!(reader_name(&["a"], "a".into()), Some("a".into()));
        assert_eq!(reader_name(&["b", "a"], "a".into()), Some("a".into()));
        assert_eq!(reader_name(&["b"], "a".into()), None);
    }

    /// A call the site path decides a name at saves no library lead beside
    /// it: the site pass's own reading stands (review finding 5).
    #[test]
    fn a_library_lead_at_a_decided_site_is_not_saved() {
        // The package's declarations, so the compiler names the export the
        // site path follows the call to.
        let files = [
            (
                "node_modules/@fixture/bus/package.json",
                "{ \"name\": \"@fixture/bus\", \"version\": \"1.4.0\", \"types\": \"index.d.ts\" }\n",
            ),
            (
                "node_modules/@fixture/bus/index.d.ts",
                "export declare const bus: { publish(topic: string, data: unknown): void };\n",
            ),
            (
                "src/leads.ts",
                "import { bus } from \"@fixture/bus\";\n\
                 declare const settings: { topic: string };\n\
                 export function fromSettings() { bus.publish(settings.topic ?? \"orders\", {}); }\n",
            ),
        ];
        let exports = [bus()];
        let Some((on, _, rowed)) = read_twice(&files, &exports, None) else {
            return;
        };
        assert_eq!(rowed.leads.len(), 1, "{:?}", rowed.leads);
        let discovery = crate::request_summary::discovery::discover(
            &on.inputs,
            &crate::request_summary::discovery::Labels::of(&exports),
            &on.repo,
        );
        let discovered: Vec<crate::request_summary::decided_sites::DiscoverySite> = discovery
            .sites
            .iter()
            .map(|site| serde_json::from_value(serde_json::to_value(site).unwrap()).unwrap())
            .collect();
        let labels = crate::request_summary::decided_sites::SiteLabel::from_exports(&exports);
        let decided = crate::request_summary::decided_sites::decided_sites(
            &on.inputs,
            &on.repo,
            &discovered,
            &labels,
        );
        let lead = &rowed.leads[0];
        let file = crate::site_rows::relative(&on.repo, &lead.file.to_string_lossy());
        assert!(
            decided.sites.iter().any(|site| site.file == file
                && site.span_start == lead.span_start
                && site.span_end == lead.span_end),
            "the site path decides the call: {:#?} {lead:?}",
            decided.sites
        );
        let unread = crate::unread_sites::from_scan(
            &crate::request_summary::discovery::Discovery::default(),
            &decided,
            &[],
            &rowed.leads,
            &on.repo,
        );
        assert!(unread.iter().all(|site| site.slot != "name"), "{unread:#?}");
    }

    /// The bus with a keyed name too: `add({ name })`.
    fn keyed_bus() -> ExportClaims {
        export(json!({
            "package": "@fixture/bus", "version": "1.4.0", "specifier": "@fixture/bus",
            "export": "bus", "role": "broker",
            "claims": [
                { "kind": "op", "op": "send", "member": "publish", "on": "export",
                  "name": { "arg": 0 }, "payload": { "arg": 1 } },
                { "kind": "op", "op": "send", "member": "add", "on": "export",
                  "name": { "arg": 0, "key": "name" } }
            ]
        }))
    }

    /// The rowed twins of `library_sites`' four pinned name tests
    /// (carrick#3504 B10-3, Q-B3d): with the compiler's rows the text reader
    /// reads each slot, and every row today's reading states is stated
    /// again, at the same place. `want` is the rows; the census must count
    /// no row today's reading states that the text reader leaves.
    fn rowed_twin(files: &[(&str, &str)], want: &[(&str, u32, &str)]) {
        let Some((rows, _)) = read_walked(files, &[keyed_bus()], true) else {
            return;
        };
        let want: Vec<(String, u32, String)> = want
            .iter()
            .map(|(file, line, name)| (file.to_string(), *line, name.to_string()))
            .collect();
        assert_eq!(placed(&rows), want, "{rows:#?}");
        assert_eq!(
            rows.names.count(NameOutcome::LeadTodayStated),
            0,
            "{:?} {:#?}",
            rows.names,
            rows.leads
        );
        assert_eq!(rows.names.count(NameOutcome::NoRows), 0, "{:?}", rows.names);
    }

    #[test]
    fn rowed_a_name_passed_as_a_parameter_is_read_where_the_caller_writes_it() {
        rowed_twin(
            &[
                (
                    "src/bus.ts",
                    "import { bus } from \"@fixture/bus\";\n\
                     export async function publish(topic: string, data: unknown) {\n\
                     \x20 await bus.publish(topic, data);\n\
                     }\n\
                     export async function publishOrder(kind: string, data: unknown) {\n\
                     \x20 await publish(`orders.${kind}`, data);\n\
                     }\n\
                     export async function audit(data: string) {\n\
                     \x20 await bus.publish(\"audit\", data);\n\
                     }\n\
                     export function enqueue(job: { name: string }) {\n\
                     \x20 bus.add(job);\n\
                     }\n",
                ),
                (
                    "src/use.ts",
                    "import { publish, publishOrder, audit, enqueue } from \"./bus\";\n\
                     const TOPIC = \"users.created\";\n\
                     export async function a(x: unknown, dynamic: string) {\n\
                     \x20 await publish(\"users.deleted\", x);\n\
                     \x20 await publish(TOPIC, x);\n\
                     \x20 await publishOrder(\"shipped\", x);\n\
                     \x20 await audit(\"payload\");\n\
                     \x20 await publish(dynamic, x);\n\
                     \x20 const args: [string] = [\"spread\"];\n\
                     \x20 await publish(...args, \"late\");\n\
                     \x20 enqueue({ name: \"welcome\" });\n\
                     }\n",
                ),
            ],
            &[
                ("bus.ts", 9, "audit"),
                ("use.ts", 4, "users.deleted"),
                ("use.ts", 5, "users.created"),
                ("use.ts", 6, "orders.shipped"),
                ("use.ts", 11, "welcome"),
            ],
        );
    }

    /// The constants module the rowed name twins read.
    const TOPICS_MODULE: &str = "export const TOPICS = { orders: { created: \"orders.created\" }, users: \"users\" } as const;\n\
         export const PREFIX = \"svc\";\n\
         export const topicFor = (kind: string) => `${PREFIX}.${kind}`;\n\
         export function auditTopic(name: string) { return `audit.${name}`; }\n\
         export const ROUTES = { byId: (id: string) => `jobs.${id}` };\n\
         export const MUTABLE = { a: \"mutable.a\" };\n\
         export function twoSteps(name: string) { const topic = `two.${name}`; return topic; }\n";

    #[test]
    fn rowed_a_name_read_through_a_map_a_constant_or_a_builder_is_its_text() {
        rowed_twin(
            &[
                ("src/topics.ts", TOPICS_MODULE),
                (
                    "src/other.ts",
                    "import { MUTABLE } from \"./topics\";\n\
                     MUTABLE.a = \"changed\";\n",
                ),
                (
                    "src/use.ts",
                    "import { bus } from \"@fixture/bus\";\n\
                     import { TOPICS, PREFIX, topicFor, auditTopic, ROUTES, MUTABLE, twoSteps } from \"./topics\";\n\
                     const LOCAL = { shipped: \"orders.shipped\" };\n\
                     const NESTED = { inner: { x: \"nested.x\" } };\n\
                     const alias = NESTED.inner;\n\
                     export function a(kind: string) {\n\
                     \x20 bus.publish(TOPICS.orders.created, {});\n\
                     \x20 bus.publish(PREFIX, {});\n\
                     \x20 bus.publish(topicFor(\"x\"), {});\n\
                     \x20 bus.publish(auditTopic(\"y\"), {});\n\
                     \x20 bus.publish(ROUTES.byId(\"7\"), {});\n\
                     \x20 bus.publish(LOCAL.shipped, {});\n\
                     \x20 bus.publish(MUTABLE.a, {});\n\
                     \x20 bus.publish(NESTED.inner.x, {});\n\
                     \x20 bus.publish(topicFor(kind), {});\n\
                     \x20 bus.publish(twoSteps(\"z\"), {});\n\
                     \x20 bus.publish(TOPICS.users, alias);\n\
                     }\n",
                ),
                (
                    "src/caller.ts",
                    "import { a } from \"./use\";\n\
                     export function go() { a(\"filled\"); }\n",
                ),
            ],
            &[
                ("caller.ts", 2, "svc.filled"),
                ("use.ts", 7, "orders.created"),
                ("use.ts", 8, "svc"),
                ("use.ts", 9, "svc.x"),
                ("use.ts", 10, "audit.y"),
                ("use.ts", 11, "jobs.7"),
                ("use.ts", 12, "orders.shipped"),
                ("use.ts", 16, "two.z"),
                ("use.ts", 17, "users"),
            ],
        );
    }

    /// A constant a builder's call initialises keeps today's reading too:
    /// its value is a call term the text reader holes (`r1_builder`).
    #[test]
    fn rowed_a_constant_a_builder_initialises_keeps_today_s_name() {
        rowed_twin(
            &[
                ("src/topics.ts", TOPICS_MODULE),
                (
                    "src/use.ts",
                    "import { bus } from \"@fixture/bus\";\n\
                     import { topicFor } from \"./topics\";\n\
                     const BUILT = topicFor(\"built\");\n\
                     export function a() { bus.publish(BUILT, {}); }\n",
                ),
            ],
            &[("use.ts", 4, "svc.built")],
        );
    }

    #[test]
    fn rowed_an_object_used_any_way_but_read_holds_no_name() {
        rowed_twin(
            &[
                (
                    "src/topics.ts",
                    "import { bus } from \"@fixture/bus\";\n\
                     import { register } from \"./registry\";\n\
                     const HANDED = { a: \"handed.a\" };\n\
                     const SPREAD = { a: \"spread.a\" };\n\
                     const RETURNED = { a: \"returned.a\" };\n\
                     const COMPUTED = { a: \"computed.a\" };\n\
                     const OPTIONAL = { inner: { a: \"optional.a\" } };\n\
                     const KEPT = { a: \"kept.a\" };\n\
                     const TESTED_INNER = { inner: { a: \"tested.a\" } };\n\
                     const TESTED_TEXT = { a: \"tested.text\" };\n\
                     register(HANDED);\n\
                     export const copy = { ...SPREAD };\n\
                     export function give() { return RETURNED; }\n\
                     export function pick(key: \"a\") { return COMPUTED[key]; }\n\
                     export function maybe() { return OPTIONAL?.inner; }\n\
                     export function check() { if (TESTED_INNER.inner || TESTED_TEXT.a) { return 1; } return 0; }\n\
                     export function send() {\n\
                     \x20 bus.publish(HANDED.a, {});\n\
                     \x20 bus.publish(SPREAD.a, {});\n\
                     \x20 bus.publish(RETURNED.a, {});\n\
                     \x20 bus.publish(COMPUTED.a, {});\n\
                     \x20 bus.publish(OPTIONAL.inner.a, {});\n\
                     \x20 bus.publish(KEPT.a, {});\n\
                     \x20 bus.publish(TESTED_INNER.inner.a, {});\n\
                     \x20 bus.publish(TESTED_TEXT.a, {});\n\
                     }\n",
                ),
                (
                    "src/registry.ts",
                    "export function register(value: unknown) {}\n",
                ),
            ],
            // The text reader's write census reads an object read by a
            // computed key as kept (a read changes nothing), so it reads
            // `computed.a`, which today's reading does not: a broker's name
            // read anew waits on the site path's kind question and states no
            // library row (Q-B3e).
            &[
                ("topics.ts", 23, "kept.a"),
                ("topics.ts", 25, "tested.text"),
            ],
        );
    }

    #[test]
    fn rowed_a_constant_object_any_module_may_change_is_read_as_nothing() {
        let user = "import { bus } from \"@fixture/bus\";\n\
                    import { TOPICS, PREFIX } from \"./topics\";\n\
                    export function a() {\n\
                    \x20 bus.publish(TOPICS.users, {});\n\
                    \x20 bus.publish(PREFIX, {});\n\
                    }\n";
        rowed_twin(
            &[("src/topics.ts", TOPICS_MODULE), ("src/use.ts", user)],
            &[("use.ts", 4, "users"), ("use.ts", 5, "svc")],
        );
        rowed_twin(
            &[
                ("src/topics.ts", TOPICS_MODULE),
                ("src/use.ts", user),
                (
                    "src/all.ts",
                    "import * as topics from \"./topics\";\n\
                     export const every = topics;\n",
                ),
            ],
            // The text reader reads the entry through the declaration its
            // rows name whatever a namespace import does with the module,
            // where today's reading reads nothing: a broker's name read
            // anew waits on the site path's kind question and states no
            // library row (Q-B3e). `PREFIX` today's reading reads.
            &[("use.ts", 5, "svc")],
        );
    }

    /// A socket export of `@fixture/socket` on `side`, listening with `on`
    /// and sending with `emit`, both named by argument 0.
    fn loop_socket(export_name: &str, side: &str) -> ExportClaims {
        export(json!({
            "package": "@fixture/socket", "version": "4.0.0", "specifier": "@fixture/socket",
            "export": export_name, "role": "socket", "side": side,
            "claims": [
                { "kind": "op", "op": "send", "member": "emit", "on": "export",
                  "name": { "arg": 0 }, "payload": { "arg": 1 } },
                { "kind": "op", "op": "receive", "member": "on", "on": "export",
                  "name": { "arg": 0 }, "handler": { "arg": 1 } }
            ]
        }))
    }

    /// A server that listens in a loop over a stated list, and a client
    /// that sends in a loop over the same list, imported.
    const LOOP_EVENTS: &str =
        "export const EVENTS = [\"order.created\", \"order.shipped\"] as const;\n";
    const LOOP_HUB: &str = "import { io } from \"@fixture/socket\";\n\
        import { EVENTS } from \"./events\";\n\
        export function listen() {\n\
        \x20 for (const ev of EVENTS) io.on(ev, () => {});\n\
        }\n\
        declare const later: string[];\n\
        export function unread() {\n\
        \x20 for (const ev of later) io.on(ev, () => {});\n\
        }\n\
        export function choose(flag: boolean) {\n\
        \x20 for (const ev of EVENTS) io.on(flag ? ev : `${ev}.x`, () => {});\n\
        }\n\
        export function outside(flag: boolean) {\n\
        \x20 io.on(flag ? \"a.one\" : \"a.two\", () => {});\n\
        \x20 io.on(\"a.fixed\", () => {});\n\
        }\n";
    const LOOP_CLIENT: &str = "import { socket } from \"@fixture/socket\";\n\
        import { EVENTS } from \"./events\";\n\
        export function tell() {\n\
        \x20 for (const ev of EVENTS) {\n\
        \x20   socket.emit(ev, {});\n\
        \x20 }\n\
        }\n";

    /// carrick#4245 (the socket half): a socket handler registered, and a
    /// socket event sent, in a loop over a stated list is one row per
    /// element at the call, and each listener pairs with its sender. A
    /// list the code does not state, a choice written inside the loop and a
    /// choice outside any loop keep their leads; a literal name keeps its
    /// row.
    #[test]
    fn a_socket_event_in_a_loop_over_a_stated_list_is_one_row_per_element() {
        let Some((rows, _)) = read_walked(
            &[
                ("src/events.ts", LOOP_EVENTS),
                ("src/hub.ts", LOOP_HUB),
                ("src/client.ts", LOOP_CLIENT),
            ],
            &[loop_socket("io", "server"), loop_socket("socket", "client")],
            true,
        ) else {
            return;
        };
        let at = |file: &str| -> Vec<(u32, String)> {
            let mut out: Vec<(u32, String)> = placed(&rows)
                .into_iter()
                .filter(|(of, _, _)| of == file)
                .map(|(_, line, name)| (line, name))
                .collect();
            out.sort();
            out
        };
        assert_eq!(
            at("hub.ts"),
            vec![
                (4, "order.created".to_string()),
                (4, "order.shipped".to_string()),
                (15, "a.fixed".to_string()),
            ],
            "{rows:#?}"
        );
        assert_eq!(
            at("client.ts"),
            vec![
                (5, "order.created".to_string()),
                (5, "order.shipped".to_string()),
            ],
            "{rows:#?}"
        );
        // Each element's listener pairs with its sender: one operation
        // each, an endpoint and a call.
        let ends = |side: RowSide| -> BTreeSet<String> {
            rows.rows
                .iter()
                .filter(|row| row.name.starts_with("order."))
                .map(|row| row.operation())
                .filter(|(_, of)| *of == side)
                .map(|(key, _)| format!("{key:?}"))
                .collect()
        };
        assert_eq!(ends(RowSide::Endpoint).len(), 2, "{rows:#?}");
        assert_eq!(ends(RowSide::Endpoint), ends(RowSide::Call), "{rows:#?}");
        let mut leads: Vec<(u32, Stop)> = rows
            .leads
            .iter()
            .map(|lead| (lead.line, lead.reason))
            .collect();
        leads.sort();
        assert_eq!(
            leads.iter().map(|(line, _)| *line).collect::<Vec<_>>(),
            vec![8, 11, 14],
            "{:?}",
            rows.leads
        );
        assert_ne!(leads[0].1, Stop::SeveralOperations, "{:?}", rows.leads);
        assert_eq!(leads[1].1, Stop::SeveralOperations, "{:?}", rows.leads);
        assert_eq!(leads[2].1, Stop::SeveralOperations, "{:?}", rows.leads);
    }

    /// carrick#4245, the pub/sub and queue half (open): a broker name read
    /// per element is a name read anew, so it states no library row and the
    /// site path's kind question decides (Q-B3e), as for any name the
    /// reader reads that today's reading does not.
    #[test]
    fn a_broker_name_in_a_loop_over_a_stated_list_waits_on_the_kind_question() {
        let Some((rows, _)) = read_walked(
            &[(
                "src/topics.ts",
                "import { bus } from \"@fixture/bus\";\n\
                 const TOPICS = [\"orders.created\", \"orders.shipped\"] as const;\n\
                 export function wire() {\n\
                 \x20 for (const t of TOPICS) bus.subscribe(t, () => {});\n\
                 \x20 for (const t of TOPICS) bus.publish(t, {});\n\
                 }\n",
            )],
            &[bus()],
            true,
        ) else {
            return;
        };
        assert!(rows.rows.is_empty(), "{rows:#?}");
        assert!(rows.leads.is_empty(), "{:?}", rows.leads);
        assert_eq!(
            rows.names.count(NameOutcome::KindAsked),
            2,
            "{:?}",
            rows.names
        );
    }
}
