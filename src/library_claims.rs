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
//! - every other role, HTTP included, states nothing here.
//!
//! A site states a row only when nothing contests its receiver (calls are
//! classified by the claims' own lists, amendment 2 B1), every receiver it
//! may be made through reads the same op and name ([`LibrarySite::fold`],
//! amendment 3), and the name is a literal that is neither a wildcard nor a
//! name the library emits itself. Where to send the claims, and how the
//! store's answer is parsed, is carrick#1664's.
//!
//! The rules are in `docs/reference/client-semantics.md`, "Message roles".

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::operation::{PubsubRole, SocketDirection};
use crate::request_summary::{
    LibrarySite, LibrarySites, MakerForm, MemberUse, MemberWire, On, Selector, SiteMaker,
    SiteReceiver,
};
use crate::services::type_sidecar::{
    BoundName, ClaimOn, ClaimSlot, LibraryCheck, LibraryClaim, LibraryOp, LibraryRole, MakeForm,
    NameScope, NameScopeKind, OpName, SemanticsResult, SemanticsVerdict,
};

/// Which end of a socket an export makes. A classification the types
/// cannot carry: it only ever picks a direction, never a fact of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Client,
    Server,
    Both,
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
    #[serde(default)]
    pub side: Option<Side>,
    #[serde(default)]
    pub claims: Vec<LibraryClaim>,
    #[serde(default)]
    pub calls: Vec<CallClaim>,
    /// The library's wildcard characters: a name holding one is a pattern,
    /// never one name.
    #[serde(default)]
    pub patterns: Vec<String>,
}

/// The roles whose rows this module writes.
fn is_message_role(role: LibraryRole) -> bool {
    matches!(
        role,
        LibraryRole::Broker | LibraryRole::Socket | LibraryRole::InProcessBus
    )
}

impl ExportClaims {
    /// Every claim with the id the scanner derives for it:
    /// `<specifier>@<major>:<export>:<kind>:…`, the convention HTTP claim
    /// ids follow (`<package>@<major>:<export>:factory:<member>`). A claim id
    /// stated twice, differently, names nothing one can verify, so both are
    /// dropped. Scope claims are left out: no reader reads the receivers a
    /// scope returns yet (carrick#1678).
    pub fn claims_by_id(&self) -> Vec<(String, &LibraryClaim)> {
        let prefix = format!(
            "{}@{}:{}",
            self.specifier,
            major(&self.version),
            self.export
        );
        let mut stated: Vec<(String, &LibraryClaim)> = Vec::new();
        let mut twice: HashSet<String> = HashSet::new();
        for claim in &self.claims {
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
        if used.form == MakerForm::New {
            let made = self.claims.iter().any(|claim| {
                matches!(claim, LibraryClaim::Make { form: MakeForm::New, member, .. }
                    if receiver == "export" && used.path.is_empty() && *member == used.member)
            });
            return if made {
                MemberWire::OnWire
            } else {
                MemberWire::Unlisted
            };
        }
        let claimed = self.claims.iter().any(|claim| match claim {
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

/// The id of one claim under `prefix`; `None` for a scope claim.
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
            ..
        } => Some(format!(
            "{prefix}:op:{}:{}{}",
            op_name(*op),
            hops(path, member.as_deref()),
            selector(on, of)
        )),
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
        LibraryClaim::Scope { .. } => None,
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
fn applies(on: Option<ClaimOn>, of: Option<&str>, receiver: &str) -> bool {
    let instance = receiver.starts_with("instance:");
    match (on, of) {
        (None, Some(of)) => of == receiver,
        (Some(ClaimOn::Export), None) => receiver == "export",
        (Some(ClaimOn::Instance), None) => instance,
        (Some(ClaimOn::Both), None) => receiver == "export" || instance,
        _ => false,
    }
}

/// The site API's selector for a claim's own `on`, `of`, `path` and
/// `member`.
fn selector<'a>(
    on: Option<ClaimOn>,
    of: Option<&'a str>,
    path: &'a [String],
    member: Option<&'a str>,
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
    for export in exports.iter().filter(|export| is_message_role(export.role)) {
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
            let on: Vec<&str> = match claim {
                LibraryClaim::Make { .. } => vec!["export"],
                LibraryClaim::Op { on, of, .. } | LibraryClaim::Reserved { on, of, .. } => {
                    receivers
                        .iter()
                        .map(String::as_str)
                        .filter(|receiver| applies(*on, of.as_deref(), receiver))
                        .collect()
                }
                LibraryClaim::Scope { .. } => Vec::new(),
            };
            for receiver in on {
                checks.push(LibraryCheck {
                    claim_id: id.clone(),
                    package: export.specifier.clone(),
                    export: export.export.clone(),
                    role: export.role,
                    receiver: receiver.to_string(),
                    claim: claim.clone(),
                });
            }
        }
    }
    checks
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
    pub name_scope: NameScope,
    /// Every claim the row rests on, each maker of a set included, sorted.
    pub claim_ids: Vec<String>,
    /// A definition (`task({ id, run })`): where a model route at exactly
    /// this span is withdrawn.
    pub definition: bool,
}

/// Every row a service's library sites state through verified claims.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryRows {
    pub rows: Vec<LibraryRow>,
}

/// Read every library site through the claims that verify
/// ([`checks`], then `verify`). `verify` answers one verdict per check, in
/// order; an error, or any other answer, states nothing. No message-role
/// export, or no site through one, asks nothing.
pub fn read(
    sites: &LibrarySites,
    exports: &[ExportClaims],
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
            return LibraryRows::default();
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
    let mut stated: Vec<(LibraryRow, ReceiverKey)> = Vec::new();
    for export in exports.iter().filter(|export| is_message_role(export.role)) {
        let claims = export.claims_by_id();
        for site in sites
            .sites
            .iter()
            .filter(|site| site.specifier == export.specifier && site.export == export.export)
        {
            if site.contest(|on, used| export.classify(on, used)).is_some() {
                continue;
            }
            let reading = Reading {
                site,
                export,
                claims: &claims,
                verified: &verified,
            };
            if let Some(row) = reading.row() {
                stated.push((row, ReceiverKey::of(site)));
            }
        }
    }
    LibraryRows {
        rows: heard_in_process(stated, exports),
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

/// One site, read through its export's claims.
struct Reading<'a> {
    site: &'a LibrarySite,
    export: &'a ExportClaims,
    claims: &'a [(String, &'a LibraryClaim)],
    verified: &'a HashSet<(String, String)>,
}

impl Reading<'_> {
    fn verified(&self, id: &str, receiver: &str) -> bool {
        self.verified
            .contains(&(id.to_string(), receiver.to_string()))
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
        name_scope: NameScope,
        claim_ids: BTreeSet<String>,
        definition: bool,
    ) -> LibraryRow {
        LibraryRow {
            file: self.site.file.clone(),
            line: self.site.line,
            span_start: self.site.span_start,
            span_end: self.site.span_end,
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
                    name_scope,
                    ..
                } if *member == self.site.member => Some((id, name, handler, name_scope)),
                _ => None,
            })?;
        if !self.verified(id, "export") || !self.site.supplies(slot_arg(handler), slot_key(handler))
        {
            return None;
        }
        let name = self
            .site
            .literal(slot_arg(name_slot), slot_key(name_slot))?;
        if !self.name_ok(name, None) {
            return None;
        }
        Some(self.stated(
            LibraryRowKind::Pubsub(PubsubRole::Subscriber),
            name.to_string(),
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
                    return None;
                }
                claim_ids.push(id.to_string());
                Some((maker, make))
            }
            None => None,
        };
        let mut read: Option<(LibraryRowKind, String, NameScope)> = None;
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
                ..
            } = claim
            else {
                continue;
            };
            let selected = selector(*on, of.as_deref(), path, member.as_deref());
            if !self.site.selected_by(through, &selected) || !self.verified(id, &receiver) {
                continue;
            }
            // Every other part the claim positions must be in the call: an
            // overload with fewer arguments can put the payload where the
            // name was claimed.
            let supplied = [payload, handler, ack]
                .into_iter()
                .flatten()
                .all(|slot| self.site.supplies(slot_arg(slot), slot_key(slot)));
            if !supplied {
                return None;
            }
            let (text, scope) = match name.as_ref()? {
                OpName::Slot(slot) => (
                    self.site.literal(slot_arg(slot), slot_key(slot))?,
                    name_scope.clone(),
                ),
                OpName::Bound {
                    bound: BoundName::Maker,
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
                    (
                        maker.literal(slot_arg(slot), slot_key(slot))?,
                        name_scope.clone(),
                    )
                }
                // A name a scope member binds is carrick#1678's.
                OpName::Bound {
                    bound: BoundName::Scope,
                } => return None,
            };
            if !self.name_ok(text, Some(&receiver)) {
                return None;
            }
            let kind = row_kind(self.export.role, *op, self.export.side)?;
            let this = (kind, text.to_string(), scoped(self.export.role, scope));
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

    /// A name one row can state: not empty, no wildcard character of the
    /// library's, and not a name the library emits itself on this receiver
    /// (`receiver`; a definition is on the export).
    fn name_ok(&self, name: &str, receiver: Option<&str>) -> bool {
        if name.is_empty()
            || self
                .export
                .patterns
                .iter()
                .any(|pattern| !pattern.is_empty() && name.contains(pattern.as_str()))
        {
            return false;
        }
        let receiver = receiver.unwrap_or("export");
        !self.claims.iter().any(|(id, claim)| {
            matches!(claim, LibraryClaim::Reserved { member, name: reserved, path, .. }
                if reserved == name
                    && Some(member) == self.site.member.as_ref()
                    && *path == self.site.path)
                && self.verified(id, receiver)
        })
    }
}

/// What a call states through one receiver.
struct Through {
    kind: LibraryRowKind,
    name: String,
    name_scope: NameScope,
    claim_ids: Vec<String>,
}

fn slot_arg(slot: &ClaimSlot) -> usize {
    slot.arg as usize
}

fn slot_key(slot: &ClaimSlot) -> Option<&str> {
    slot.key.as_deref()
}

/// The row an op states for a role. A socket's direction is the export's
/// side's; one that serves both sides, or says nothing, states nothing. A
/// `request` or an `execute` is no message row.
fn row_kind(role: LibraryRole, op: LibraryOp, side: Option<Side>) -> Option<LibraryRowKind> {
    use LibraryRowKind::{Pubsub, Socket};
    use SocketDirection::{ClientToServer, ServerToClient};
    Some(match (role, op, side) {
        (LibraryRole::Broker | LibraryRole::InProcessBus, LibraryOp::Send, _) => {
            Pubsub(PubsubRole::Publisher)
        }
        (LibraryRole::Broker | LibraryRole::InProcessBus, LibraryOp::Receive, _) => {
            Pubsub(PubsubRole::Subscriber)
        }
        (LibraryRole::Socket, LibraryOp::Send, Some(Side::Client)) => Socket {
            direction: ClientToServer,
            listener: false,
        },
        (LibraryRole::Socket, LibraryOp::Receive, Some(Side::Client)) => Socket {
            direction: ServerToClient,
            listener: true,
        },
        (LibraryRole::Socket, LibraryOp::Send, Some(Side::Server)) => Socket {
            direction: ServerToClient,
            listener: false,
        },
        (LibraryRole::Socket, LibraryOp::Receive, Some(Side::Server)) => Socket {
            direction: ClientToServer,
            listener: true,
        },
        _ => return None,
    })
}

/// The scope a row is written with: an in-process bus's name never leaves
/// its service (D4, carrick#1564 comment 5940360418), whatever the claim
/// says; a claim that says nothing is read as `service` too, the direction
/// that only ever loses an edge.
fn scoped(role: LibraryRole, scope: Option<NameScope>) -> NameScope {
    match (role, scope) {
        (LibraryRole::InProcessBus, scope) => NameScope {
            scope: NameScopeKind::Service,
            namespace: scope.and_then(|scope| scope.namespace),
        },
        (_, Some(scope)) => scope,
        (_, None) => NameScope {
            scope: NameScopeKind::Service,
            namespace: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::request_summary::library_sites;

    /// A manifest declaring every package the tests import.
    const MANIFEST: &str = "{ \"name\": \"service\", \"dependencies\": { \"@fixture/jobs\": \"^1.0.0\", \"@fixture/queue\": \"^2.0.0\", \"@fixture/bus\": \"^1.0.0\", \"@fixture/socket\": \"^1.0.0\", \"@fixture/http\": \"^1.0.0\" } }\n";

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
        read(sites, exports, verifier(&asked, verified)).rows
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
            NameScope {
                scope: NameScopeKind::Global,
                namespace: None
            }
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

        let failed = read(&sites, &[bus()], |_| Err("sidecar gone".to_string()));
        assert!(failed.rows.is_empty());
        let short = read(&sites, &[bus()], |_| Ok(Vec::new()));
        assert!(short.rows.is_empty());
        // Every verdict one place along: each names another check.
        let swapped = read(&sites, &[bus()], |checks| {
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
                })
                .collect())
        });
        assert!(
            swapped.rows.is_empty(),
            "verdicts out of order verify nothing"
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
        assert_eq!(rows[0].name_scope.namespace.as_deref(), Some("queue"));
        let unverified_maker =
            rows_of(&sites, &[queue], |check| !check.claim_id.contains(":make:"));
        assert!(unverified_maker.is_empty(), "the maker must verify");
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
        assert_eq!(rows[0].name_scope.scope, NameScopeKind::Service);
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
        assert!(kinds(Some("both")).is_empty());
        assert!(kinds(None).is_empty());
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
            assert_eq!(row.name_scope.scope, NameScopeKind::Service);
            assert_eq!(row.name_scope.namespace.as_deref(), Some("event"));
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

    /// Only the message roles state rows here: an HTTP client's export, even
    /// with claims that would read as a send, asks and states nothing.
    #[test]
    fn an_http_client_states_nothing_here() {
        let mut http = bus();
        http.role = LibraryRole::HttpClient;
        let sites = sites_of(&[("src/orders.ts", BUS_SERVICE)]);
        let asked = std::cell::RefCell::new(Vec::new());
        let rows = read(&sites, &[http], verifier(&asked, all));
        assert!(rows.rows.is_empty());
        assert!(
            asked.borrow().is_empty(),
            "nothing is asked for an HTTP export"
        );
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
        let scopes: Vec<NameScope> = rows_of(&sites, &[queue("instance:new")], all)
            .into_iter()
            .map(|row| row.name_scope)
            .collect();
        assert_eq!(
            scopes,
            vec![NameScope {
                scope: NameScopeKind::Service,
                namespace: None
            }],
            "a claim that states no scope is read as `service`"
        );
        assert!(names("instance:new:Cluster").is_empty());
    }
}
