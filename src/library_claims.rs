//! Library claims in one shape for every protocol (carrick#1616, PROTOTYPE for
//! the $0 slice of the design record "One pipeline for library claims, every
//! protocol", 2026-10-01). Not for merge.
//!
//! Framework detection answers each package once, per major version: for each
//! export, a role from a closed list (the only thing that picks the code
//! path), a side for sockets, and five lists:
//!
//! - `makes`: how an instance is made (`call` or `new`, on the export itself
//!   or one of its members) and where the maker reads a base, a name or a
//!   handler;
//! - `scopes`: members that return a receiver bound to a name (unused in the
//!   slice);
//! - `ops`: members that act on the wire, with where their name, payload,
//!   handler and acknowledgement sit;
//! - `reserved`: names the library emits itself;
//! - `patterns`: the library's wildcard characters.
//!
//! The scanner derives the claim ids, sends every claim to the type sidecar
//! (`verify_library_claims`, which checks shape against the package's own
//! declarations), and reads call sites only through the claims that verified
//! ([`ClaimSurface`]). A behaviour claim (the role, the side, `patterns`, an
//! unverified `reserved` name) only ever removes rows.
//!
//! HTTP entries are converted to the `client_semantics` shape the HTTP reader
//! already reads ([`http_entries`]), so HTTP rows are what they were.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer};
use serde_json::Value;
use tracing::debug;

use crate::client_semantics::{
    ClientSemantics, ClientSemanticsEntry, FactorySemantics, RequestSemantics, SemanticsStatus,
    VerbSemantics,
};
use crate::request_summary::{MakerForm, maker_receiver};
use crate::services::type_sidecar::{
    BoundName, ClaimSlot, LibraryCheck, LibraryClaim, LibraryOp, LibraryRole, MakesForm, OpName,
    SemanticsRequestArgs, SemanticsResult, SemanticsVerbArgs, SemanticsVerdict, SocketSide,
    TypeSidecar,
};

/// The longest member, export or key the shape admits.
const MAX_NAME: usize = 64;

/// The closed list of roles an export is classified into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    HttpClient,
    GraphqlClient,
    Broker,
    InProcessBus,
    Socket,
    ServerFramework,
    None,
}

/// Which end of a socket an export makes. A behaviour claim: never checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Client,
    Server,
    Both,
}

/// Argument `arg` (0-based), or with `key`, that key of the object there.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
pub struct Slot {
    pub arg: u32,
    #[serde(default)]
    pub key: Option<String>,
}

/// Where an op's name comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NameSource {
    /// A slot of the call itself.
    Slot(Slot),
    /// Bound to the instance by its maker's `name` slot.
    Maker,
    /// Bound to the receiver by a scope member (not read in the slice).
    Scope,
}

/// What an op does on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    Request,
    Send,
    Receive,
    Execute,
    Define,
    Mount,
}

/// Which receivers an op acts on: the export, the instances its makers make,
/// or both. Not in the design record's shape; without it every op is checked
/// on every receiver, and an op whose slots differ between the export and its
/// instances (`streams.append(name, value)` against `stream.append(value)`)
/// could verify on the receiver it was never claimed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpOn {
    Export,
    Instance,
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Makes {
    pub form: MakerForm,
    pub member: Option<String>,
    pub base: Option<Slot>,
    pub name: Option<Slot>,
    pub handler: Option<Slot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Op {
    pub op: OpKind,
    pub member: Option<String>,
    pub name: Option<NameSource>,
    pub payload: Option<Slot>,
    pub handler: Option<Slot>,
    pub ack: Option<Slot>,
    /// HTTP only: an options object at this argument.
    pub options: Option<Slot>,
    /// HTTP only: the method the member always sends.
    pub method: Option<String>,
    /// HTTP only: where the method sits.
    pub method_key: Option<Slot>,
    pub on: OpOn,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Reserved {
    pub name: String,
    pub member: String,
    pub at: Slot,
}

/// One export's claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportClaims {
    pub export: String,
    pub role: Role,
    pub side: Option<Side>,
    pub makes: Vec<Makes>,
    pub ops: Vec<Op>,
    pub reserved: Vec<Reserved>,
    pub patterns: Vec<String>,
}

/// One package, as framework detection answered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryClaimsEntry {
    pub package: String,
    pub major: Option<u64>,
    pub status: SemanticsStatus,
    pub exports: Vec<ExportClaims>,
}

// ---------------------------------------------------------------------------
// The wire, read leniently
// ---------------------------------------------------------------------------

/// `DetectionResult.library_claims`, read one element at a time: an element
/// that does not parse is dropped on its own and nothing here can fail a
/// detection.
pub fn deserialize_entries<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<LibraryClaimsEntry>>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(parse_entries(value.as_ref()))
}

pub fn parse_entries(value: Option<&Value>) -> Option<Vec<LibraryClaimsEntry>> {
    let array = value?.as_array()?;
    let mut dropped = 0usize;
    let entries: Vec<LibraryClaimsEntry> = array
        .iter()
        .filter_map(|element| {
            let entry = parse_entry(element, &mut dropped);
            if entry.is_none() {
                dropped += 1;
            }
            entry
        })
        .collect();
    if dropped > 0 {
        debug!("library_claims: dropped {dropped} element(s) that did not parse");
    }
    Some(entries)
}

fn parse_entry(value: &Value, dropped: &mut usize) -> Option<LibraryClaimsEntry> {
    let object = value.as_object()?;
    let package = object.get("package")?.as_str()?;
    if package.is_empty() {
        return None;
    }
    let major = match object.get("major") {
        None | Some(Value::Null) => None,
        Some(major) => Some(major.as_u64()?),
    };
    let status: SemanticsStatus = serde_json::from_value(object.get("status")?.clone()).ok()?;
    let exports = match status {
        SemanticsStatus::Answered => elements(object.get("exports")?, dropped, parse_export),
        SemanticsStatus::Pending | SemanticsStatus::Skipped => Vec::new(),
    };
    Some(LibraryClaimsEntry {
        package: package.to_string(),
        major,
        status,
        exports,
    })
}

fn parse_export(value: &Value, dropped: &mut usize) -> Option<ExportClaims> {
    let object = value.as_object()?;
    let list = |key: &str| object.get(key).cloned().unwrap_or(Value::Array(Vec::new()));
    let role: Role = serde_json::from_value(object.get("role")?.clone()).ok()?;
    let side = match object.get("side") {
        None | Some(Value::Null) => None,
        Some(side) => Some(serde_json::from_value(side.clone()).ok()?),
    };
    Some(ExportClaims {
        export: identifier(object.get("export")?)?,
        role,
        side,
        makes: elements(&list("makes"), dropped, |v, _| parse_makes(v)),
        ops: elements(&list("ops"), dropped, |v, _| parse_op(v)),
        reserved: elements(&list("reserved"), dropped, |v, _| parse_reserved(v)),
        patterns: elements(&list("patterns"), dropped, |v, _| {
            v.as_str().filter(|p| !p.is_empty()).map(str::to_string)
        }),
    })
}

fn parse_makes(value: &Value) -> Option<Makes> {
    let object = value.as_object()?;
    let form = match object.get("form")?.as_str()? {
        "call" => MakerForm::Call,
        "new" => MakerForm::New,
        _ => return None,
    };
    Some(Makes {
        form,
        member: optional_identifier(object.get("member"))?,
        base: optional_slot(object.get("base"))?,
        name: optional_slot(object.get("name"))?,
        handler: optional_slot(object.get("handler"))?,
    })
}

fn parse_op(value: &Value) -> Option<Op> {
    let object = value.as_object()?;
    let op: OpKind = serde_json::from_value(object.get("op")?.clone()).ok()?;
    let name = match object.get("name") {
        None | Some(Value::Null) => None,
        Some(Value::Object(name)) if name.contains_key("bound") => {
            match name.get("bound")?.as_str()? {
                "maker" => Some(NameSource::Maker),
                "scope" => Some(NameSource::Scope),
                _ => return None,
            }
        }
        Some(slot) => Some(NameSource::Slot(parse_slot(slot)?)),
    };
    let method = match object.get("method") {
        None | Some(Value::Null) => None,
        Some(method) => {
            let method = method.as_str()?;
            if !crate::type_manifest::is_http_method(method) {
                return None;
            }
            Some(method.to_ascii_uppercase())
        }
    };
    // An op names the receivers it acts on; a name bound by the maker is an
    // instance's, and anything else defaults to the export alone.
    let on = match object.get("on") {
        None | Some(Value::Null) => match name {
            Some(NameSource::Maker) => OpOn::Instance,
            _ => OpOn::Export,
        },
        Some(on) => serde_json::from_value(on.clone()).ok()?,
    };
    if name == Some(NameSource::Maker) && on != OpOn::Instance {
        return None;
    }
    Some(Op {
        op,
        member: optional_identifier(object.get("member"))?,
        name,
        payload: optional_slot(object.get("payload"))?,
        handler: optional_slot(object.get("handler"))?,
        ack: optional_slot(object.get("ack"))?,
        options: optional_slot(object.get("options"))?,
        method,
        method_key: optional_slot(object.get("method_key"))?,
        on,
    })
}

fn parse_reserved(value: &Value) -> Option<Reserved> {
    let object = value.as_object()?;
    let name = object.get("name")?.as_str()?;
    if name.is_empty() {
        return None;
    }
    Some(Reserved {
        name: name.to_string(),
        member: identifier(object.get("member")?)?,
        at: parse_slot(object.get("at")?)?,
    })
}

fn parse_slot(value: &Value) -> Option<Slot> {
    let object = value.as_object()?;
    let arg = u32::try_from(object.get("arg")?.as_u64()?).ok()?;
    let key = match object.get("key") {
        None | Some(Value::Null) => None,
        Some(key) => Some(key_text(key)?),
    };
    Some(Slot { arg, key })
}

fn optional_slot(value: Option<&Value>) -> Option<Option<Slot>> {
    match value {
        None | Some(Value::Null) => Some(None),
        Some(value) => parse_slot(value).map(Some),
    }
}

fn optional_identifier(value: Option<&Value>) -> Option<Option<String>> {
    match value {
        None | Some(Value::Null) => Some(None),
        Some(value) => identifier(value).map(Some),
    }
}

fn identifier(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    let mut chars = text.chars();
    let first = chars.next()?;
    let valid = text.len() <= MAX_NAME
        && (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    valid.then(|| text.to_string())
}

fn key_text(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    let valid =
        !text.is_empty() && text.len() <= MAX_NAME && !text.chars().any(char::is_whitespace);
    valid.then(|| text.to_string())
}

fn elements<T>(
    value: &Value,
    dropped: &mut usize,
    parse: impl Fn(&Value, &mut usize) -> Option<T>,
) -> Vec<T> {
    let Some(array) = value.as_array() else {
        *dropped += 1;
        return Vec::new();
    };
    let mut out = Vec::new();
    for element in array {
        match parse(element, dropped) {
            Some(parsed) => out.push(parsed),
            None => *dropped += 1,
        }
    }
    out
}

// ---------------------------------------------------------------------------
// HTTP: the shape the HTTP reader already reads
// ---------------------------------------------------------------------------

/// The `http_client` exports of `entries`, in the `client_semantics` shape
/// (carrick#1564), so the request summaries read HTTP exactly as they did.
/// A factory is a `makes` entry with a keyed base at argument 0; a verb is a
/// `request` op with a fixed method and the path at argument 0, its body
/// either the second argument or a key of an options object there; a request
/// call is a `request` op with a method key.
///
/// An HTTP op that fits none of those forms is dropped, as the old shape
/// would have refused it.
pub fn http_entries(entries: &[LibraryClaimsEntry]) -> Vec<ClientSemanticsEntry> {
    let mut out = Vec::new();
    for entry in entries {
        let clients: Vec<ClientSemantics> = entry
            .exports
            .iter()
            .filter(|export| export.role == Role::HttpClient)
            .map(|export| ClientSemantics {
                export: export.export.clone(),
                factories: export
                    .makes
                    .iter()
                    .filter_map(|makes| match (makes.form, &makes.member, &makes.base) {
                        (
                            MakerForm::Call,
                            Some(member),
                            Some(Slot {
                                arg: 0,
                                key: Some(key),
                            }),
                        ) => Some(FactorySemantics {
                            member: member.clone(),
                            base_url_key: key.clone(),
                        }),
                        _ => None,
                    })
                    .collect(),
                verbs: export.ops.iter().filter_map(http_verb).collect(),
                requests: export.ops.iter().filter_map(http_request).collect(),
            })
            .collect();
        let has_http = entry
            .exports
            .iter()
            .any(|export| export.role == Role::HttpClient);
        if entry.status != SemanticsStatus::Answered || has_http {
            out.push(ClientSemanticsEntry {
                package: entry.package.clone(),
                major: entry.major,
                status: entry.status,
                clients,
            });
        }
    }
    out
}

fn http_verb(op: &Op) -> Option<VerbSemantics> {
    if op.op != OpKind::Request || op.method_key.is_some() {
        return None;
    }
    let member = op.member.clone()?;
    let method = op.method.clone()?;
    if op.name != Some(NameSource::Slot(Slot { arg: 0, key: None })) {
        return None;
    }
    let (args, body_key) = match (&op.options, &op.payload) {
        (None, Some(Slot { arg: 1, key: None })) => (SemanticsVerbArgs::PathBody, None),
        (Some(Slot { arg: 1, key: None }), None) => (SemanticsVerbArgs::PathOptions, None),
        (
            Some(Slot { arg: 1, key: None }),
            Some(Slot {
                arg: 1,
                key: Some(key),
            }),
        ) => (SemanticsVerbArgs::PathOptions, Some(key.clone())),
        _ => return None,
    };
    Some(VerbSemantics {
        member,
        method,
        args,
        body_key,
    })
}

fn http_request(op: &Op) -> Option<RequestSemantics> {
    if op.op != OpKind::Request || op.method.is_some() {
        return None;
    }
    let method_key = op.method_key.as_ref()?;
    let name = match &op.name {
        Some(NameSource::Slot(slot)) => slot,
        _ => return None,
    };
    let (args, url_key) = match (name, method_key.arg) {
        (
            Slot {
                arg: 0,
                key: Some(url),
            },
            0,
        ) => (SemanticsRequestArgs::Config, Some(url.clone())),
        (Slot { arg: 0, key: None }, 1) => (SemanticsRequestArgs::PathOptions, None),
        _ => return None,
    };
    let body_key = match &op.payload {
        None => None,
        Some(Slot {
            arg,
            key: Some(key),
        }) if *arg == method_key.arg => Some(key.clone()),
        Some(_) => return None,
    };
    Some(RequestSemantics {
        member: op.member.clone(),
        args,
        url_key,
        method_key: method_key.key.clone()?,
        body_key,
    })
}

// ---------------------------------------------------------------------------
// Claims
// ---------------------------------------------------------------------------

/// What one claim states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    Makes(Makes),
    Op(Op),
    Reserved(Reserved),
}

/// One claim on one receiver of one module specifier: the unit the sidecar
/// verifies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedCheck {
    pub claim_id: String,
    /// The module specifier, as the service imports it (a package or one of
    /// its subpaths).
    pub specifier: String,
    pub export: String,
    pub role: Role,
    pub side: Option<Side>,
    pub receiver: String,
    pub claim: Claim,
}

/// Everything about one (specifier, export) a reading needs whether or not
/// its claims verified: the role, the side, the names it reserves, and its
/// wildcard characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportFacts {
    pub package: String,
    pub role: Role,
    pub side: Option<Side>,
    /// (member, name) the library emits itself, verified or not: behaviour
    /// that only ever removes a row.
    pub reserved: BTreeSet<(String, String)>,
    pub patterns: Vec<String>,
}

/// Every check a service's claims need, and the facts per export.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DerivedClaims {
    pub checks: Vec<DerivedCheck>,
    pub exports: BTreeMap<(String, String), ExportFacts>,
}

impl DerivedClaims {
    pub fn is_empty(&self) -> bool {
        self.checks.is_empty()
    }
}

fn form_name(form: MakerForm) -> &'static str {
    match form {
        MakerForm::Call => "call",
        MakerForm::New => "new",
    }
}

fn op_name(op: OpKind) -> &'static str {
    match op {
        OpKind::Request => "request",
        OpKind::Send => "send",
        OpKind::Receive => "receive",
        OpKind::Execute => "execute",
        OpKind::Define => "define",
        OpKind::Mount => "mount",
    }
}

fn on_name(on: OpOn) -> &'static str {
    match on {
        OpOn::Export => "export",
        OpOn::Instance => "instance",
        OpOn::Both => "both",
    }
}

/// The non-HTTP checks the answered entries make, for every specifier the
/// service imports that is the package or one of its subpaths. HTTP exports
/// are read through [`http_entries`] instead.
///
/// Ids are the scanner's: `<specifier>@<major|x>:<export>:makes:<call|new>:<member|()>`,
/// `...:ops:<op>:<member|()>:<on>`, `...:reserved:<member>:<name>`. A claim
/// id stated twice differently is dropped, as is a package answered twice and
/// an export classified twice.
pub fn derive(entries: &[LibraryClaimsEntry], specifiers: &BTreeSet<String>) -> DerivedClaims {
    let mut per_package: BTreeMap<&str, usize> = BTreeMap::new();
    for entry in entries {
        *per_package.entry(entry.package.as_str()).or_default() += 1;
    }
    let mut derived = DerivedClaims::default();
    for entry in entries {
        if entry.status != SemanticsStatus::Answered || per_package[entry.package.as_str()] > 1 {
            continue;
        }
        let major = entry
            .major
            .map(|major| major.to_string())
            .unwrap_or_else(|| "x".to_string());
        let mut exports_seen: BTreeMap<&str, usize> = BTreeMap::new();
        for export in &entry.exports {
            *exports_seen.entry(export.export.as_str()).or_default() += 1;
        }
        let modules = specifiers.iter().filter(|specifier| {
            *specifier == &entry.package
                || specifier
                    .strip_prefix(entry.package.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        });
        for specifier in modules {
            for export in &entry.exports {
                if export.role == Role::HttpClient || exports_seen[export.export.as_str()] > 1 {
                    continue;
                }
                derive_export(&mut derived, entry, specifier, &major, export);
            }
        }
    }
    derived
}

fn derive_export(
    derived: &mut DerivedClaims,
    entry: &LibraryClaimsEntry,
    specifier: &str,
    major: &str,
    export: &ExportClaims,
) {
    let prefix = format!("{specifier}@{major}:{}", export.export);
    let instance_receivers: Vec<String> = export
        .makes
        .iter()
        .map(|makes| maker_receiver(makes.form, makes.member.as_deref()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut stated: BTreeMap<String, Vec<(Vec<String>, Claim)>> = BTreeMap::new();
    let mut state = |id: String, receivers: Vec<String>, claim: Claim| {
        stated.entry(id).or_default().push((receivers, claim));
    };
    for makes in &export.makes {
        state(
            format!(
                "{prefix}:makes:{}:{}",
                form_name(makes.form),
                makes.member.as_deref().unwrap_or("()")
            ),
            vec!["export".to_string()],
            Claim::Makes(makes.clone()),
        );
    }
    for op in &export.ops {
        let receivers: Vec<String> = match op.on {
            OpOn::Export => vec!["export".to_string()],
            OpOn::Instance => instance_receivers.clone(),
            OpOn::Both => std::iter::once("export".to_string())
                .chain(instance_receivers.iter().cloned())
                .collect(),
        };
        state(
            format!(
                "{prefix}:ops:{}:{}:{}",
                op_name(op.op),
                op.member.as_deref().unwrap_or("()"),
                on_name(op.on)
            ),
            receivers,
            Claim::Op(op.clone()),
        );
    }
    for reserved in &export.reserved {
        state(
            format!("{prefix}:reserved:{}:{}", reserved.member, reserved.name),
            std::iter::once("export".to_string())
                .chain(instance_receivers.iter().cloned())
                .collect(),
            Claim::Reserved(reserved.clone()),
        );
    }
    for (claim_id, mut versions) in stated {
        versions.dedup();
        if versions.len() > 1 {
            debug!("library_claims: {claim_id} was stated twice differently; dropped");
            continue;
        }
        let (receivers, claim) = versions.remove(0);
        for receiver in receivers {
            derived.checks.push(DerivedCheck {
                claim_id: claim_id.clone(),
                specifier: specifier.to_string(),
                export: export.export.clone(),
                role: export.role,
                side: export.side,
                receiver,
                claim: claim.clone(),
            });
        }
    }
    derived.exports.insert(
        (specifier.to_string(), export.export.clone()),
        ExportFacts {
            package: entry.package.clone(),
            role: export.role,
            side: export.side,
            reserved: export
                .reserved
                .iter()
                .map(|reserved| (reserved.member.clone(), reserved.name.clone()))
                .collect(),
            patterns: export.patterns.clone(),
        },
    );
}

// ---------------------------------------------------------------------------
// The verified surface
// ---------------------------------------------------------------------------

/// A `makes` claim that verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedMakes {
    pub claim_id: String,
    pub makes: Makes,
}

/// An `ops` claim that verified on one receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedOp {
    pub claim_id: String,
    pub op: Op,
}

/// What one (specifier, export) verifiably does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceExport {
    pub facts: ExportFacts,
    pub makes: Vec<VerifiedMakes>,
    /// Receiver -> its verified ops.
    pub ops: BTreeMap<String, Vec<VerifiedOp>>,
    /// Every (receiver, member) an op or a definition (a maker with a name
    /// and a handler) is claimed on, verified or not: the calls a reading
    /// reaches, whatever it then states.
    pub claimed: BTreeSet<(String, Option<String>)>,
}

impl SurfaceExport {
    /// Whether a maker or an op is claimed on `member` of `receiver`.
    pub fn claims(&self, receiver: &str, member: Option<&str>) -> bool {
        self.claimed
            .contains(&(receiver.to_string(), member.map(str::to_string)))
    }

    /// The verified maker of `form` on `member`.
    pub fn maker(&self, form: MakerForm, member: Option<&str>) -> Option<&VerifiedMakes> {
        self.makes
            .iter()
            .find(|makes| makes.makes.form == form && makes.makes.member.as_deref() == member)
    }

    /// The verified ops `receiver` has on `member` (`None`: the receiver
    /// itself called).
    pub fn ops_on<'a>(
        &'a self,
        receiver: &str,
        member: Option<&'a str>,
    ) -> impl Iterator<Item = &'a VerifiedOp> + 'a {
        self.ops
            .get(receiver)
            .into_iter()
            .flatten()
            .filter(move |op| op.op.member.as_deref() == member)
    }

    /// Whether `member` is a verified op or maker of `receiver`.
    pub fn names(&self, receiver: &str, member: Option<&str>) -> bool {
        self.ops_on(receiver, member).next().is_some()
            || (receiver == "export"
                && self
                    .makes
                    .iter()
                    .any(|makes| makes.makes.member.as_deref() == member))
    }
}

/// The surface a service's verdicts support (carrick#1616).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaimSurface {
    exports: BTreeMap<(String, String), SurfaceExport>,
}

impl ClaimSurface {
    pub fn is_empty(&self) -> bool {
        self.exports.is_empty()
    }

    pub fn export(&self, specifier: &str, export: &str) -> Option<&SurfaceExport> {
        self.exports
            .get(&(specifier.to_string(), export.to_string()))
    }

    /// The surface `verified` supports: a `(claim_id, receiver)` pair it does
    /// not hold contributes nothing, and an op on an instance receiver counts
    /// only where the maker that makes the instance verified too.
    pub fn build(derived: &DerivedClaims, verified: impl Fn(&str, &str) -> bool) -> Self {
        let mut surface = ClaimSurface::default();
        for (key, facts) in &derived.exports {
            let checks: Vec<&DerivedCheck> = derived
                .checks
                .iter()
                .filter(|check| check.specifier == key.0 && check.export == key.1)
                .collect();
            let makes: Vec<VerifiedMakes> = checks
                .iter()
                .filter_map(|check| match &check.claim {
                    Claim::Makes(makes) if verified(&check.claim_id, &check.receiver) => {
                        Some(VerifiedMakes {
                            claim_id: check.claim_id.clone(),
                            makes: makes.clone(),
                        })
                    }
                    _ => None,
                })
                .collect();
            let verified_instances: BTreeSet<String> = makes
                .iter()
                .map(|makes| maker_receiver(makes.makes.form, makes.makes.member.as_deref()))
                .collect();
            let mut ops: BTreeMap<String, Vec<VerifiedOp>> = BTreeMap::new();
            for check in &checks {
                let Claim::Op(op) = &check.claim else {
                    continue;
                };
                if !verified(&check.claim_id, &check.receiver) {
                    continue;
                }
                if check.receiver != "export" && !verified_instances.contains(&check.receiver) {
                    continue;
                }
                ops.entry(check.receiver.clone())
                    .or_default()
                    .push(VerifiedOp {
                        claim_id: check.claim_id.clone(),
                        op: op.clone(),
                    });
            }
            // A maker is a row site only when it is a definition: it makes
            // under a name and registers a handler.
            let claimed = checks
                .iter()
                .filter_map(|check| match &check.claim {
                    Claim::Makes(makes) if makes.name.is_some() && makes.handler.is_some() => {
                        Some((check.receiver.clone(), makes.member.clone()))
                    }
                    Claim::Op(op) => Some((check.receiver.clone(), op.member.clone())),
                    Claim::Makes(_) | Claim::Reserved(_) => None,
                })
                .collect();
            surface.exports.insert(
                key.clone(),
                SurfaceExport {
                    facts: facts.clone(),
                    makes,
                    ops,
                    claimed,
                },
            );
        }
        surface
    }

    /// Every claim as if it verified: what the reader reaches apart from the
    /// sidecar. For tests, and for the slice's report of the reader's reach.
    pub fn all_verified(derived: &DerivedClaims) -> Self {
        Self::build(derived, |_, _| true)
    }
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

/// Set to `1` to read every derived claim as verified, skipping the sidecar:
/// what the reader reaches apart from verification (prototype only, for the
/// slice's report). Never set in a real scan.
pub const ASSUME_VERIFIED_ENV: &str = "CARRICK_SLICE_ASSUME_VERIFIED";

/// What one service's verification came to.
#[derive(Debug, Clone, Default)]
pub struct Verification {
    pub surface: ClaimSurface,
    /// One verdict per check, in check order; empty when nothing was asked
    /// or the sidecar failed.
    pub verdicts: Vec<SemanticsResult>,
    pub checks: Vec<DerivedCheck>,
    /// Wall time of the sidecar call.
    pub elapsed: std::time::Duration,
    /// The sidecar failed outright, and why.
    pub error: Option<String>,
}

impl Verification {
    /// The verdicts, by `verified` or `<verdict> (<reason>)`.
    pub fn by_reason(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for result in &self.verdicts {
            let label = match (&result.verdict, &result.reason) {
                (SemanticsVerdict::Verified, _) => "verified".to_string(),
                (verdict, reason) => format!(
                    "{} ({})",
                    format!("{verdict:?}").to_lowercase(),
                    reason.as_deref().unwrap_or("no reason")
                ),
            };
            *counts.entry(label).or_default() += 1;
        }
        counts
    }
}

/// The sidecar's wire form of one derived check.
pub fn wire_check(check: &DerivedCheck) -> LibraryCheck {
    let slot = |slot: &Option<Slot>| {
        slot.as_ref().map(|slot| ClaimSlot {
            arg: slot.arg,
            key: slot.key.clone(),
        })
    };
    let claim = match &check.claim {
        Claim::Makes(makes) => LibraryClaim::Makes {
            form: match makes.form {
                MakerForm::Call => MakesForm::Call,
                MakerForm::New => MakesForm::New,
            },
            member: makes.member.clone(),
            base: slot(&makes.base),
            name: slot(&makes.name),
            handler: slot(&makes.handler),
        },
        Claim::Op(op) => LibraryClaim::Ops {
            op: match op.op {
                OpKind::Request => LibraryOp::Request,
                OpKind::Send => LibraryOp::Send,
                OpKind::Receive => LibraryOp::Receive,
                OpKind::Execute => LibraryOp::Execute,
                OpKind::Define => LibraryOp::Define,
                OpKind::Mount => LibraryOp::Mount,
            },
            member: op.member.clone(),
            name: op.name.as_ref().map(|name| match name {
                NameSource::Slot(at) => OpName::Slot(ClaimSlot {
                    arg: at.arg,
                    key: at.key.clone(),
                }),
                NameSource::Maker => OpName::Bound {
                    bound: BoundName::Maker,
                },
                NameSource::Scope => OpName::Bound {
                    bound: BoundName::Scope,
                },
            }),
            payload: slot(&op.payload),
            handler: slot(&op.handler),
            ack: slot(&op.ack),
            options: slot(&op.options),
            method: op.method.clone(),
            method_key: slot(&op.method_key),
        },
        Claim::Reserved(reserved) => LibraryClaim::Reserved {
            name: reserved.name.clone(),
            member: reserved.member.clone(),
            at: ClaimSlot {
                arg: reserved.at.arg,
                key: reserved.at.key.clone(),
            },
        },
    };
    LibraryCheck {
        claim_id: check.claim_id.clone(),
        package: check.specifier.clone(),
        export: check.export.clone(),
        role: match check.role {
            Role::HttpClient => LibraryRole::HttpClient,
            Role::GraphqlClient => LibraryRole::GraphqlClient,
            Role::Broker => LibraryRole::Broker,
            Role::InProcessBus => LibraryRole::InProcessBus,
            Role::Socket => LibraryRole::Socket,
            Role::ServerFramework => LibraryRole::ServerFramework,
            Role::None => LibraryRole::None,
        },
        side: check.side.map(|side| match side {
            Side::Client => SocketSide::Client,
            Side::Server => SocketSide::Server,
            Side::Both => SocketSide::Both,
        }),
        receiver: check.receiver.clone(),
        claim,
    }
}

/// Check every non-HTTP claim `entries` make for the modules the service
/// imports (`specifiers`), in one request, and return the surface the
/// verified ones support. Never cached; any sidecar failure verifies
/// nothing, so every site stays what it is without the claims.
pub fn verify(
    sidecar: Option<&TypeSidecar>,
    from_dir: &std::path::Path,
    entries: &[LibraryClaimsEntry],
    specifiers: &BTreeSet<String>,
) -> Verification {
    let derived = derive(entries, specifiers);
    let mut verification = Verification {
        checks: derived.checks.clone(),
        ..Verification::default()
    };
    if derived.is_empty() {
        return verification;
    }
    if std::env::var(ASSUME_VERIFIED_ENV).as_deref() == Ok("1") {
        verification.surface = ClaimSurface::all_verified(&derived);
        return verification;
    }
    let Some(sidecar) = sidecar else {
        verification.error = Some("no sidecar".to_string());
        return verification;
    };
    let checks: Vec<LibraryCheck> = derived.checks.iter().map(wire_check).collect();
    let started = std::time::Instant::now();
    let results = sidecar.verify_library_claims(from_dir, &checks);
    verification.elapsed = started.elapsed();
    match results {
        Ok(results) => {
            let verified: BTreeSet<(String, String)> = results
                .iter()
                .filter(|result| result.verdict == SemanticsVerdict::Verified)
                .map(|result| (result.claim_id.clone(), result.receiver.clone()))
                .collect();
            verification.surface = ClaimSurface::build(&derived, |claim_id, receiver| {
                verified.contains(&(claim_id.to_string(), receiver.to_string()))
            });
            verification.verdicts = results;
        }
        Err(error) => {
            debug!("library claims could not be checked ({error}); nothing is verified");
            verification.error = Some(error.to_string());
        }
    }
    verification
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<LibraryClaimsEntry> {
        let value = serde_json::json!([
            {
                "package": "@fixture/tasks", "major": 1, "status": "answered",
                "exports": [
                    { "export": "task", "role": "broker",
                      "makes": [{ "form": "call", "member": null,
                                  "name": { "arg": 0, "key": "id" },
                                  "handler": { "arg": 0, "key": "run" } }],
                      "ops": [{ "op": "send", "member": "trigger",
                                "name": { "bound": "maker" }, "payload": { "arg": 0 } }] },
                    { "export": "tasks", "role": "broker",
                      "ops": [{ "op": "send", "member": "trigger",
                                "name": { "arg": 0 }, "payload": { "arg": 1 } }],
                      "patterns": ["*"] }
                ]
            },
            { "package": "@fixture/http", "major": 1, "status": "answered",
              "exports": [{ "export": "default", "role": "http_client",
                "makes": [{ "form": "call", "member": "create", "base": { "arg": 0, "key": "baseURL" } }],
                "ops": [
                    { "op": "request", "member": "get", "method": "GET", "name": { "arg": 0 }, "options": { "arg": 1 }, "on": "both" },
                    { "op": "request", "member": "post", "method": "POST", "name": { "arg": 0 }, "payload": { "arg": 1 }, "on": "both" },
                    { "op": "request", "member": "request", "name": { "arg": 0, "key": "url" },
                      "method_key": { "arg": 0, "key": "method" }, "payload": { "arg": 0, "key": "data" }, "on": "both" }
                ] }] },
            { "package": "fixture-slow", "major": 3, "status": "pending", "exports": [] },
            "not an entry"
        ]);
        parse_entries(Some(&value)).expect("an array")
    }

    #[test]
    fn a_bad_element_drops_alone() {
        assert_eq!(sample().len(), 3);
    }

    #[test]
    fn checks_go_to_the_receivers_the_op_names_for_every_imported_subpath() {
        let specifiers: BTreeSet<String> =
            ["@fixture/tasks", "@fixture/tasks/v1", "@fixture/tasksx"]
                .into_iter()
                .map(str::to_string)
                .collect();
        let derived = derive(&sample(), &specifiers);
        let on = |id: &str| -> Vec<(String, String)> {
            derived
                .checks
                .iter()
                .filter(|check| check.claim_id == id)
                .map(|check| (check.specifier.clone(), check.receiver.clone()))
                .collect()
        };
        assert_eq!(
            on("@fixture/tasks@1:task:makes:call:()"),
            vec![("@fixture/tasks".into(), "export".into())]
        );
        assert_eq!(
            on("@fixture/tasks@1:task:ops:send:trigger:instance"),
            vec![("@fixture/tasks".into(), "instance:()".into())]
        );
        assert_eq!(
            on("@fixture/tasks/v1@1:tasks:ops:send:trigger:export"),
            vec![("@fixture/tasks/v1".into(), "export".into())]
        );
        assert!(
            derived
                .checks
                .iter()
                .all(|check| check.specifier != "@fixture/tasksx"),
            "a package name prefix is not a subpath"
        );
        assert!(
            derived
                .checks
                .iter()
                .all(|check| check.role != Role::HttpClient),
            "HTTP goes through the client-semantics reader"
        );
    }

    #[test]
    fn an_instance_op_needs_its_maker_verified() {
        let specifiers: BTreeSet<String> = ["@fixture/tasks".to_string()].into();
        let derived = derive(&sample(), &specifiers);
        let surface = ClaimSurface::build(&derived, |id, _| !id.contains(":makes:"));
        let task = surface.export("@fixture/tasks", "task").unwrap();
        assert!(task.maker(MakerForm::Call, None).is_none());
        assert_eq!(task.ops_on("instance:()", Some("trigger")).count(), 0);
        let all = ClaimSurface::all_verified(&derived);
        let task = all.export("@fixture/tasks", "task").unwrap();
        assert_eq!(task.ops_on("instance:()", Some("trigger")).count(), 1);
    }

    #[test]
    fn http_exports_convert_to_the_client_semantics_shape() {
        let http = http_entries(&sample());
        let entry = http
            .iter()
            .find(|entry| entry.package == "@fixture/http")
            .unwrap();
        let client = &entry.clients[0];
        assert_eq!(client.factories[0].base_url_key, "baseURL");
        assert_eq!(client.verbs.len(), 2);
        assert_eq!(client.verbs[0].args, SemanticsVerbArgs::PathOptions);
        assert_eq!(client.verbs[1].args, SemanticsVerbArgs::PathBody);
        assert_eq!(client.requests[0].url_key.as_deref(), Some("url"));
        assert_eq!(client.requests[0].body_key.as_deref(), Some("data"));
        assert!(
            http.iter().any(|entry| entry.package == "fixture-slow"),
            "a pending entry stays pending"
        );
        assert!(
            http.iter().all(|entry| entry.package != "@fixture/tasks"),
            "a package with no HTTP export is not an HTTP client"
        );
    }
}
