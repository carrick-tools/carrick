//! Client-library semantics: how a data-fetching package builds its requests,
//! as framework detection states it and as the package's own type
//! declarations confirm it (carrick#1564).
//!
//! A request made through a library client is written in two places the
//! source can read, and one it cannot:
//!
//! ```ignore
//! import http from "@fixture/http";
//! const api = http.create({ baseURL: "/api/v1" }); // the base
//! api.post("/orders", { action: "create" });      // the path and the body
//! ```
//!
//! What `create` does with `baseURL`, and that `post` sends a `POST` with its
//! second argument as the body, is knowledge about the LIBRARY. It is not
//! hard-coded here: framework detection answers it per `package@major` as a
//! structured `client_semantics` list beside `data_fetchers`, and every claim
//! in that answer is checked against the package's own declarations by the
//! type sidecar before a request summary uses it. Every literal in a row still
//! comes from the user's source; the library's answer only says where to read
//! them.
//!
//! This module owns the three steps between the answer and the summaries:
//!
//! 1. **The wire shape**, parsed leniently: an element that does not parse is
//!    dropped on its own, and nothing here can fail a detection.
//! 2. **The claims**, derived from the answer with ids the model never
//!    emits, and the checks that pair each claim with the receivers it is
//!    checked on ([`derive_claims`]).
//! 3. **The verified surface** ([`LibrarySemantics`]): only the
//!    `(claim, receiver)` pairs the sidecar verified, and an instance
//!    receiver only when its factory claim verified too. A claim that failed
//!    or could not be checked is dropped, never demoted: the site stays what
//!    it is without it.
//!
//! The contract both repos read is pinned on carrick#1564; the reference is
//! `docs/reference/client-semantics.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use tracing::debug;

use crate::services::type_sidecar::{
    SemanticsCheck, SemanticsClaim, SemanticsRequestArgs, SemanticsResult, SemanticsVerbArgs,
    SemanticsVerdict, TypeSidecar,
};

/// The methods a verb claim may state, upper case.
const HTTP_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// The longest `member`, `export` or key the contract admits.
const MAX_NAME: usize = 64;

/// The receiver a claim is checked on when it is the export itself.
const EXPORT_RECEIVER: &str = "export";

/// Whether detection answered a package on this call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SemanticsStatus {
    /// From the model or the cloud's cache.
    Answered,
    /// Not answered on this call (a limit, the deadline, a model failure).
    /// The scanner asks again on its next scan.
    Pending,
    /// Not asked: a non-registry range, or over the per-request cap.
    Skipped,
}

/// One `data_fetchers` package, as framework detection answered it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientSemanticsEntry {
    pub package: String,
    /// The major version the manifest range names, or `None` when it names no
    /// single major.
    pub major: Option<u64>,
    pub status: SemanticsStatus,
    /// Empty unless answered.
    pub clients: Vec<ClientSemantics>,
}

/// One client a package exports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientSemantics {
    /// `"default"` or a named export.
    pub export: String,
    pub factories: Vec<FactorySemantics>,
    pub verbs: Vec<VerbSemantics>,
    pub requests: Vec<RequestSemantics>,
}

/// `export.member(opts)` builds an instance whose base URL is
/// `opts[base_url_key]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FactorySemantics {
    pub member: String,
    pub base_url_key: String,
}

/// `receiver.member(path, …)` sends `method` to `path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerbSemantics {
    pub member: String,
    pub method: String,
    pub args: SemanticsVerbArgs,
    /// Where the body sits in the options, for `path_options` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_key: Option<String>,
}

/// A request call: `receiver.member(config)`, `receiver.member(path,
/// options)`, or the receiver itself called when `member` is `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequestSemantics {
    pub member: Option<String>,
    pub args: SemanticsRequestArgs,
    /// Present exactly when `args` is `config`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_key: Option<String>,
    pub method_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_key: Option<String>,
}

/// `DetectionResult.client_semantics`, read leniently: a key that is absent,
/// `null` or not an array is `None` ("never asked"), and an element that does
/// not parse is dropped on its own. Never an error: a serde failure on the
/// detection would defer the whole service's model phase.
pub fn deserialize_entries<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<ClientSemanticsEntry>>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(parse_entries(value.as_ref()))
}

/// The entries in `value`, or `None` when it is not an array.
fn parse_entries(value: Option<&Value>) -> Option<Vec<ClientSemanticsEntry>> {
    let array = value?.as_array()?;
    let mut dropped = 0usize;
    let entries: Vec<ClientSemanticsEntry> = array
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
        debug!("client_semantics: dropped {dropped} element(s) that did not parse");
    }
    Some(entries)
}

fn parse_entry(value: &Value, dropped: &mut usize) -> Option<ClientSemanticsEntry> {
    let object = value.as_object()?;
    let package = object.get("package")?.as_str()?;
    if package.is_empty() {
        return None;
    }
    let major = match object.get("major")? {
        Value::Null => None,
        major => Some(major.as_u64()?),
    };
    let status: SemanticsStatus = serde_json::from_value(object.get("status")?.clone()).ok()?;
    let clients = match status {
        SemanticsStatus::Answered => elements(object.get("clients")?, dropped, parse_client),
        // Empty unless answered, whatever the element says.
        SemanticsStatus::Pending | SemanticsStatus::Skipped => Vec::new(),
    };
    Some(ClientSemanticsEntry {
        package: package.to_string(),
        major,
        status,
        clients,
    })
}

fn parse_client(value: &Value, dropped: &mut usize) -> Option<ClientSemantics> {
    let object = value.as_object()?;
    let export = identifier(object.get("export")?)?;
    let list = |key: &str| object.get(key).cloned().unwrap_or(Value::Array(Vec::new()));
    Some(ClientSemantics {
        export,
        factories: elements(&list("factories"), dropped, |v, _| parse_factory(v)),
        verbs: elements(&list("verbs"), dropped, |v, _| parse_verb(v)),
        requests: elements(&list("requests"), dropped, |v, _| parse_request(v)),
    })
}

fn parse_factory(value: &Value) -> Option<FactorySemantics> {
    let object = value.as_object()?;
    Some(FactorySemantics {
        member: identifier(object.get("member")?)?,
        base_url_key: key(object.get("base_url_key")?)?,
    })
}

fn parse_verb(value: &Value) -> Option<VerbSemantics> {
    let object = value.as_object()?;
    let method = object.get("method")?.as_str()?;
    if !HTTP_METHODS.contains(&method) {
        return None;
    }
    let args: SemanticsVerbArgs = serde_json::from_value(object.get("args")?.clone()).ok()?;
    let body_key = optional_key(object.get("body_key"))?;
    // A body key names a property of the options bag, which only the
    // `(path, options)` form has.
    if body_key.is_some() && args != SemanticsVerbArgs::PathOptions {
        return None;
    }
    Some(VerbSemantics {
        member: identifier(object.get("member")?)?,
        method: method.to_string(),
        args,
        body_key,
    })
}

fn parse_request(value: &Value) -> Option<RequestSemantics> {
    let object = value.as_object()?;
    let member = match object.get("member")? {
        Value::Null => None,
        member => Some(identifier(member)?),
    };
    let args: SemanticsRequestArgs = serde_json::from_value(object.get("args")?.clone()).ok()?;
    let url_key = optional_key(object.get("url_key"))?;
    // The URL is a key of the config exactly when the call takes a config.
    if url_key.is_some() != (args == SemanticsRequestArgs::Config) {
        return None;
    }
    Some(RequestSemantics {
        member,
        args,
        url_key,
        method_key: key(object.get("method_key")?)?,
        body_key: optional_key(object.get("body_key"))?,
    })
}

/// Every element of `value` that parses; the rest are dropped and counted.
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

/// A member or export name: a JavaScript identifier of at most 64 characters.
fn identifier(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    let mut chars = text.chars();
    let first = chars.next()?;
    let valid = text.len() <= MAX_NAME
        && (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    valid.then(|| text.to_string())
}

/// An option key: non-empty, at most 64 characters, no whitespace.
fn key(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    let valid =
        !text.is_empty() && text.len() <= MAX_NAME && !text.chars().any(char::is_whitespace);
    valid.then(|| text.to_string())
}

/// An optional key: `Some(None)` when absent or null, `None` when present
/// and invalid.
fn optional_key(value: Option<&Value>) -> Option<Option<String>> {
    match value {
        None | Some(Value::Null) => Some(None),
        Some(value) => key(value).map(Some),
    }
}

// ---------------------------------------------------------------------------
// Claims
// ---------------------------------------------------------------------------

/// The claims one client states, keyed by id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ClientClaims {
    /// Factory member -> its claim id and base-URL key.
    factories: BTreeMap<String, (String, String)>,
    /// Every claim id -> the claim, factories included.
    claims: BTreeMap<String, SemanticsClaim>,
}

/// Every claim framework detection made, grouped by the client that makes it,
/// with the checks the sidecar runs on them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DerivedClaims {
    /// `(package, export)` -> its claims.
    clients: BTreeMap<(String, String), ClientClaims>,
}

impl DerivedClaims {
    /// One check per `(claim_id, receiver)`, in claim-id order: a factory on
    /// the export, every other claim on the export and on one instance per
    /// factory of the same client.
    pub fn checks(&self) -> Vec<SemanticsCheck> {
        let mut checks = Vec::new();
        for ((package, export), client) in &self.clients {
            for (claim_id, claim) in &client.claims {
                let receivers: Vec<String> = match claim {
                    SemanticsClaim::Factory { .. } => vec![EXPORT_RECEIVER.to_string()],
                    _ => std::iter::once(EXPORT_RECEIVER.to_string())
                        .chain(client.factories.keys().map(|f| instance_receiver(f)))
                        .collect(),
                };
                for receiver in receivers {
                    checks.push(SemanticsCheck {
                        claim_id: claim_id.clone(),
                        package: package.clone(),
                        export: export.clone(),
                        receiver,
                        claim: claim.clone(),
                    });
                }
            }
        }
        checks
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }
}

/// The receiver string for an instance built by factory `member`.
pub fn instance_receiver(member: &str) -> String {
    format!("instance:{member}")
}

/// The claims every answered entry makes, with the contract's ids:
/// `<package>@<major|x>:<export>:<kind>:<member|()>`, plus `:<args>` on the
/// two request kinds.
///
/// A claim id stated twice with the same claim is one claim. Stated twice
/// with different claims (two `get` verbs with different methods), nothing
/// says which is meant, so both are dropped. A package answered by more than
/// one entry is dropped whole for the same reason.
pub fn derive_claims(entries: &[ClientSemanticsEntry]) -> DerivedClaims {
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
        // Claims per client, with the ids stated twice differently.
        let mut stated: BTreeMap<(String, String), BTreeMap<String, Vec<SemanticsClaim>>> =
            BTreeMap::new();
        for client in &entry.clients {
            let prefix = format!("{}@{major}:{}", entry.package, client.export);
            let claims = stated
                .entry((entry.package.clone(), client.export.clone()))
                .or_default();
            let mut state = |id: String, claim: SemanticsClaim| {
                claims.entry(id).or_default().push(claim);
            };
            for factory in &client.factories {
                state(
                    format!("{prefix}:factory:{}", factory.member),
                    SemanticsClaim::Factory {
                        member: factory.member.clone(),
                        base_url_key: factory.base_url_key.clone(),
                    },
                );
            }
            for verb in &client.verbs {
                state(
                    format!("{prefix}:verb:{}", verb.member),
                    SemanticsClaim::Verb {
                        member: verb.member.clone(),
                        method: verb.method.clone(),
                    },
                );
                state(
                    format!("{prefix}:verb_body:{}", verb.member),
                    SemanticsClaim::VerbBody {
                        member: verb.member.clone(),
                        args: verb.args,
                        body_key: verb.body_key.clone(),
                    },
                );
            }
            for request in &client.requests {
                let member = request.member.as_deref().unwrap_or("()");
                let args = request_args_name(request.args);
                state(
                    format!("{prefix}:request:{member}:{args}"),
                    SemanticsClaim::Request {
                        member: request.member.clone(),
                        args: request.args,
                        url_key: request.url_key.clone(),
                        method_key: request.method_key.clone(),
                    },
                );
                if let Some(body_key) = &request.body_key {
                    state(
                        format!("{prefix}:request_body:{member}:{args}"),
                        SemanticsClaim::RequestBody {
                            member: request.member.clone(),
                            args: request.args,
                            body_key: body_key.clone(),
                        },
                    );
                }
            }
        }
        for (client_key, claims) in stated {
            let client = derived.clients.entry(client_key).or_default();
            for (id, mut versions) in claims {
                versions.dedup();
                if versions.windows(2).any(|pair| pair[0] != pair[1]) {
                    debug!("client_semantics: {id} was stated twice differently; dropped");
                    continue;
                }
                let claim = versions.remove(0);
                if let SemanticsClaim::Factory {
                    member,
                    base_url_key,
                } = &claim
                {
                    client
                        .factories
                        .insert(member.clone(), (id.clone(), base_url_key.clone()));
                }
                client.claims.insert(id, claim);
            }
        }
    }
    derived
        .clients
        .retain(|_, client| !client.claims.is_empty());
    derived
}

fn request_args_name(args: SemanticsRequestArgs) -> &'static str {
    match args {
        SemanticsRequestArgs::Config => "config",
        SemanticsRequestArgs::PathOptions => "path_options",
    }
}

// ---------------------------------------------------------------------------
// The verified surface
// ---------------------------------------------------------------------------

/// A factory whose claim verified: what its instances take their base from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedFactory {
    pub claim_id: String,
    pub base_url_key: String,
}

/// A verb member whose claim verified on a receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedVerb {
    pub claim_id: String,
    pub method: String,
    /// Where its body sits, when that claim verified too.
    pub body: Option<VerifiedVerbBody>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedVerbBody {
    pub claim_id: String,
    pub args: SemanticsVerbArgs,
    pub body_key: Option<String>,
}

/// A request member (or the receiver itself) whose claim verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRequest {
    pub claim_id: String,
    pub member: Option<String>,
    pub args: SemanticsRequestArgs,
    pub url_key: Option<String>,
    pub method_key: String,
    /// The claim id and key of its body, when that claim verified too.
    pub body: Option<(String, String)>,
}

/// What one receiver of one client verifiably does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReceiverSurface {
    verbs: BTreeMap<String, VerifiedVerb>,
    /// In claim-id order.
    requests: Vec<VerifiedRequest>,
}

impl ReceiverSurface {
    pub fn verb(&self, member: &str) -> Option<&VerifiedVerb> {
        self.verbs.get(member)
    }

    /// The verified requests made through `member` (`None`: the receiver
    /// itself called), in claim-id order.
    pub fn requests<'a>(
        &'a self,
        member: Option<&'a str>,
    ) -> impl Iterator<Item = &'a VerifiedRequest> + 'a {
        self.requests
            .iter()
            .filter(move |request| request.member.as_deref() == member)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct VerifiedClient {
    factories: BTreeMap<String, VerifiedFactory>,
    /// `"export"` or `"instance:<factory>"` -> its surface.
    receivers: BTreeMap<String, ReceiverSurface>,
}

/// The library semantics a request summary may use: exactly the
/// `(claim, receiver)` pairs the sidecar verified.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibrarySemantics {
    /// `(package, export)` -> what it verifiably does.
    clients: BTreeMap<(String, String), VerifiedClient>,
}

impl LibrarySemantics {
    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    /// The verified factory `member` of `package`'s `export`.
    pub fn factory(&self, package: &str, export: &str, member: &str) -> Option<&VerifiedFactory> {
        self.client(package, export)?.factories.get(member)
    }

    /// What `receiver` (`"export"` or `"instance:<factory>"`) of `package`'s
    /// `export` verifiably does. An instance receiver is present only when
    /// its factory verified.
    pub fn surface(&self, package: &str, export: &str, receiver: &str) -> Option<&ReceiverSurface> {
        self.client(package, export)?.receivers.get(receiver)
    }

    fn client(&self, package: &str, export: &str) -> Option<&VerifiedClient> {
        self.clients.get(&(package.to_string(), export.to_string()))
    }

    /// The surface the verdicts support. A pair with no verdict, or one that
    /// is not `verified`, contributes nothing.
    pub fn from_verdicts(derived: &DerivedClaims, results: &[SemanticsResult]) -> Self {
        let verified: BTreeSet<(&str, &str)> = results
            .iter()
            .filter(|result| result.verdict == SemanticsVerdict::Verified)
            .map(|result| (result.claim_id.as_str(), result.receiver.as_str()))
            .collect();
        Self::build(derived, |claim_id, receiver| {
            verified.contains(&(claim_id, receiver))
        })
    }

    /// The surface as if every claim verified. For tests of what the
    /// summaries do with a verified claim, apart from the sidecar.
    #[cfg(test)]
    pub(crate) fn all_verified(entries: &[ClientSemanticsEntry]) -> Self {
        Self::build(&derive_claims(entries), |_, _| true)
    }

    fn build(derived: &DerivedClaims, verified: impl Fn(&str, &str) -> bool) -> Self {
        let mut semantics = LibrarySemantics::default();
        for (client_key, client) in &derived.clients {
            let mut out = VerifiedClient::default();
            for (member, (claim_id, base_url_key)) in &client.factories {
                if verified(claim_id, EXPORT_RECEIVER) {
                    out.factories.insert(
                        member.clone(),
                        VerifiedFactory {
                            claim_id: claim_id.clone(),
                            base_url_key: base_url_key.clone(),
                        },
                    );
                }
            }
            let receivers = std::iter::once(EXPORT_RECEIVER.to_string())
                .chain(out.factories.keys().map(|f| instance_receiver(f)));
            for receiver in receivers {
                let surface = surface_on(client, &receiver, &verified);
                if surface != ReceiverSurface::default() {
                    out.receivers.insert(receiver, surface);
                }
            }
            if !out.factories.is_empty() || !out.receivers.is_empty() {
                semantics.clients.insert(client_key.clone(), out);
            }
        }
        semantics
    }
}

/// What `receiver` verifiably does, from one client's claims.
fn surface_on(
    client: &ClientClaims,
    receiver: &str,
    verified: &impl Fn(&str, &str) -> bool,
) -> ReceiverSurface {
    let mut surface = ReceiverSurface::default();
    // Bodies first, so the verb or request they belong to can pick them up.
    let mut verb_bodies: BTreeMap<&str, VerifiedVerbBody> = BTreeMap::new();
    let mut request_bodies: BTreeMap<(Option<&str>, &str), (String, String)> = BTreeMap::new();
    for (claim_id, claim) in &client.claims {
        if !verified(claim_id, receiver) {
            continue;
        }
        match claim {
            SemanticsClaim::VerbBody {
                member,
                args,
                body_key,
            } => {
                verb_bodies.insert(
                    member,
                    VerifiedVerbBody {
                        claim_id: claim_id.clone(),
                        args: *args,
                        body_key: body_key.clone(),
                    },
                );
            }
            SemanticsClaim::RequestBody {
                member,
                args,
                body_key,
            } => {
                request_bodies.insert(
                    (member.as_deref(), request_args_name(*args)),
                    (claim_id.clone(), body_key.clone()),
                );
            }
            _ => {}
        }
    }
    for (claim_id, claim) in &client.claims {
        if !verified(claim_id, receiver) {
            continue;
        }
        match claim {
            SemanticsClaim::Verb { member, method } => {
                surface.verbs.insert(
                    member.clone(),
                    VerifiedVerb {
                        claim_id: claim_id.clone(),
                        method: method.clone(),
                        body: verb_bodies.remove(member.as_str()),
                    },
                );
            }
            SemanticsClaim::Request {
                member,
                args,
                url_key,
                method_key,
            } => surface.requests.push(VerifiedRequest {
                claim_id: claim_id.clone(),
                member: member.clone(),
                args: *args,
                url_key: url_key.clone(),
                method_key: method_key.clone(),
                body: request_bodies.remove(&(member.as_deref(), request_args_name(*args))),
            }),
            _ => {}
        }
    }
    surface
}

// ---------------------------------------------------------------------------
// Verification and the re-ask rule
// ---------------------------------------------------------------------------

/// Check every claim `entries` make against the packages installed under
/// `from_dir`, and return the surface the verified ones support.
///
/// Runs on every scan and is never cached: the answer depends on
/// `node_modules`, which the blob does not see. Any failure of the sidecar
/// itself gives an empty surface, so every site stays what it is without
/// library semantics.
pub fn verify(
    sidecar: &TypeSidecar,
    from_dir: &Path,
    entries: &[ClientSemanticsEntry],
) -> LibrarySemantics {
    let derived = derive_claims(entries);
    if derived.is_empty() {
        return LibrarySemantics::default();
    }
    let checks = derived.checks();
    let results = match sidecar.verify_client_semantics(from_dir, &checks) {
        Ok(results) => results,
        // Nothing for the user to act on: every call keeps the reading it
        // has without the semantics, and the next scan checks again.
        Err(error) => {
            debug!(
                "Client-library semantics could not be checked ({error}); calls through library clients stay model candidates this scan"
            );
            return LibrarySemantics::default();
        }
    };
    if !answers_every_check(&checks, &results) {
        debug!(
            "Client-library semantics: the sidecar answered {} result(s) for {} check(s), not one per check in order; nothing is verified this scan",
            results.len(),
            checks.len()
        );
        return LibrarySemantics::default();
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for result in &results {
        let label = match (&result.verdict, &result.reason) {
            (SemanticsVerdict::Verified, _) => "verified".to_string(),
            (verdict, reason) => {
                format!("{verdict:?} ({})", reason.as_deref().unwrap_or("no reason")).to_lowercase()
            }
        };
        *counts.entry(label).or_default() += 1;
    }
    debug!(
        "Client-library semantics: {} check(s): {}",
        checks.len(),
        counts
            .iter()
            .map(|(label, count)| format!("{count} {label}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    LibrarySemantics::from_verdicts(&derived, &results)
}

/// Whether `results` is one verdict per check, for the same `(claim_id,
/// receiver)` pair, in request order, which is what the sidecar answers. A
/// batch that is not says nothing reliable about any pair in it: a verdict
/// matched to the wrong pair would verify a claim nobody checked.
fn answers_every_check(checks: &[SemanticsCheck], results: &[SemanticsResult]) -> bool {
    checks.len() == results.len()
        && checks.iter().zip(results).all(|(check, result)| {
            check.claim_id == result.claim_id && check.receiver == result.receiver
        })
}

/// Whether a cached detection should be asked again for its library
/// semantics: it was never asked (`None`) or an entry is still `pending`,
/// and at least one of its data fetchers is installed, so an answer could
/// be verified and used.
pub fn wants_reask(
    semantics: Option<&[ClientSemanticsEntry]>,
    data_fetchers: &[String],
    service_root: &Path,
    repo_root: &Path,
) -> bool {
    let unanswered = match semantics {
        None => true,
        Some(entries) => entries
            .iter()
            .any(|entry| entry.status == SemanticsStatus::Pending),
    };
    unanswered
        && data_fetchers
            .iter()
            .any(|package| installed(package, service_root, repo_root))
}

/// Whether `package` has an installed manifest in a `node_modules` at or
/// above `service_root`, no higher than `repo_root`.
fn installed(package: &str, service_root: &Path, repo_root: &Path) -> bool {
    service_root
        .ancestors()
        .take_while(|dir| dir.starts_with(repo_root))
        .any(|dir| {
            dir.join("node_modules")
                .join(package)
                .join("package.json")
                .is_file()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The contract sample, byte for byte (carrick#1564).
    const SAMPLE: &str = include_str!(
        "../tests/fixtures/client-semantics/__llm__/framework-detect/framework-detect.json"
    );

    fn sample_entries() -> Vec<ClientSemanticsEntry> {
        let value: Value = serde_json::from_str(SAMPLE).expect("the sample is JSON");
        parse_entries(value.get("client_semantics")).expect("the sample carries the field")
    }

    fn ids(derived: &DerivedClaims) -> Vec<String> {
        derived
            .clients
            .values()
            .flat_map(|client| client.claims.keys().cloned())
            .collect()
    }

    #[test]
    fn the_sample_derives_the_golden_claim_ids() {
        let derived = derive_claims(&sample_entries());
        assert_eq!(
            ids(&derived),
            vec![
                "@fixture/http@1:default:factory:create",
                "@fixture/http@1:default:request:():config",
                "@fixture/http@1:default:request:request:config",
                "@fixture/http@1:default:request_body:():config",
                "@fixture/http@1:default:request_body:request:config",
                "@fixture/http@1:default:verb:get",
                "@fixture/http@1:default:verb:post",
                "@fixture/http@1:default:verb_body:get",
                "@fixture/http@1:default:verb_body:post",
                "fixture-prefix-http@2:default:factory:create",
                "fixture-prefix-http@2:default:request:():path_options",
                "fixture-prefix-http@2:default:request_body:():path_options",
                "fixture-prefix-http@2:default:verb:get",
                "fixture-prefix-http@2:default:verb:post",
                "fixture-prefix-http@2:default:verb_body:get",
                "fixture-prefix-http@2:default:verb_body:post",
            ],
            "pending and skipped entries state no claim"
        );
    }

    #[test]
    fn a_factory_is_checked_on_the_export_and_everything_else_once_per_instance() {
        let checks = derive_claims(&sample_entries()).checks();
        let on = |claim_id: &str| -> Vec<String> {
            checks
                .iter()
                .filter(|check| check.claim_id == claim_id)
                .map(|check| check.receiver.clone())
                .collect()
        };
        assert_eq!(on("@fixture/http@1:default:factory:create"), vec!["export"]);
        assert_eq!(
            on("@fixture/http@1:default:verb:get"),
            vec!["export", "instance:create"]
        );
        let request = checks
            .iter()
            .find(|check| check.claim_id == "@fixture/http@1:default:request:():config")
            .expect("the callable receiver is claimed");
        assert_eq!(
            request.claim,
            SemanticsClaim::Request {
                member: None,
                args: SemanticsRequestArgs::Config,
                url_key: Some("url".to_string()),
                method_key: "method".to_string(),
            }
        );
        assert_eq!(request.package, "@fixture/http");
        assert_eq!(request.export, "default");
    }

    #[test]
    fn two_factories_give_two_instance_receivers() {
        let mut entries = sample_entries();
        entries[0].clients[0].factories.push(FactorySemantics {
            member: "extend".to_string(),
            base_url_key: "baseURL".to_string(),
        });
        let checks = derive_claims(&entries).checks();
        let receivers: Vec<&str> = checks
            .iter()
            .filter(|check| check.claim_id == "@fixture/http@1:default:verb:post")
            .map(|check| check.receiver.as_str())
            .collect();
        assert_eq!(
            receivers,
            vec!["export", "instance:create", "instance:extend"]
        );
    }

    #[test]
    fn a_claim_stated_twice_the_same_is_one_and_stated_differently_is_none() {
        let mut entries = sample_entries();
        let verbs = &mut entries[0].clients[0].verbs;
        // `get` again, identically: one claim.
        verbs.push(verbs[0].clone());
        // `post` again, with another method: nothing says which is meant.
        verbs.push(VerbSemantics {
            member: "post".to_string(),
            method: "PUT".to_string(),
            args: SemanticsVerbArgs::PathBody,
            body_key: None,
        });
        let derived = derive_claims(&entries);
        let ids = ids(&derived);
        assert!(ids.contains(&"@fixture/http@1:default:verb:get".to_string()));
        assert!(!ids.contains(&"@fixture/http@1:default:verb:post".to_string()));
        // Its body claim was stated the same both times, so it stands.
        assert!(ids.contains(&"@fixture/http@1:default:verb_body:post".to_string()));
        let checks = derived.checks();
        assert_eq!(
            checks
                .iter()
                .filter(|check| check.claim_id == "@fixture/http@1:default:verb:get")
                .count(),
            2,
            "one check per receiver, not per statement"
        );
    }

    #[test]
    fn a_package_answered_twice_states_nothing() {
        let mut entries = sample_entries();
        let mut again = entries[0].clone();
        again.major = Some(2);
        entries.push(again);
        let ids = ids(&derive_claims(&entries));
        assert!(ids.iter().all(|id| !id.starts_with("@fixture/http")));
        assert!(ids.iter().any(|id| id.starts_with("fixture-prefix-http@2")));
    }

    #[test]
    fn a_null_major_is_addressed_as_x() {
        let mut entries = sample_entries();
        entries[0].major = None;
        let ids = ids(&derive_claims(&entries));
        assert!(ids.contains(&"@fixture/http@x:default:factory:create".to_string()));
    }

    #[test]
    fn one_malformed_element_drops_on_its_own() {
        let mut value: Value = serde_json::from_str(SAMPLE).unwrap();
        let semantics = value["client_semantics"].as_array_mut().unwrap();
        // A verb with a method that is not one, beside two good ones.
        semantics[0]["clients"][0]["verbs"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({ "member": "fetch", "method": "FETCH", "args": "path_body" }));
        // A config request with no url key.
        semantics[1]["clients"][0]["requests"]
            .as_array_mut()
            .unwrap()
            .push(
                serde_json::json!({ "member": "send", "args": "config", "method_key": "method" }),
            );
        // An entry that is not one at all.
        semantics.push(serde_json::json!("not an entry"));

        let entries = parse_entries(value.get("client_semantics")).unwrap();
        assert_eq!(entries.len(), 4, "the bad entry goes, the four stay");
        assert_eq!(entries, sample_entries(), "and every bad element with it");
    }

    #[test]
    fn a_verb_body_key_needs_the_options_form() {
        let verb = serde_json::json!({
            "member": "post", "method": "POST", "args": "path_body", "body_key": "json"
        });
        assert_eq!(parse_verb(&verb), None);
    }

    #[test]
    fn names_past_the_limits_are_refused() {
        assert_eq!(identifier(&Value::String("a".repeat(65))), None);
        assert_eq!(identifier(&Value::String("9lives".into())), None);
        assert_eq!(key(&Value::String("base url".into())), None);
        assert_eq!(key(&Value::String(String::new())), None);
        assert_eq!(
            identifier(&Value::String("$get_2".into())).as_deref(),
            Some("$get_2")
        );
    }

    #[test]
    fn only_verified_pairs_reach_the_surface_and_an_instance_needs_its_factory() {
        let derived = derive_claims(&sample_entries());
        let verdict = |claim_id: &str, receiver: &str, verdict: SemanticsVerdict| SemanticsResult {
            claim_id: claim_id.to_string(),
            receiver: receiver.to_string(),
            verdict,
            reason: None,
        };
        let results = vec![
            verdict(
                "@fixture/http@1:default:verb:get",
                "export",
                SemanticsVerdict::Verified,
            ),
            verdict(
                "@fixture/http@1:default:verb:get",
                "instance:create",
                SemanticsVerdict::Verified,
            ),
            // The factory failed: its instances reach nothing, whatever their
            // own verdicts say.
            verdict(
                "@fixture/http@1:default:factory:create",
                "export",
                SemanticsVerdict::Failed,
            ),
            verdict(
                "@fixture/http@1:default:verb:post",
                "export",
                SemanticsVerdict::Unchecked,
            ),
            verdict(
                "fixture-prefix-http@2:default:factory:create",
                "export",
                SemanticsVerdict::Verified,
            ),
            verdict(
                "fixture-prefix-http@2:default:verb:post",
                "instance:create",
                SemanticsVerdict::Verified,
            ),
        ];
        let semantics = LibrarySemantics::from_verdicts(&derived, &results);

        let export = semantics
            .surface("@fixture/http", "default", "export")
            .expect("the export verified a verb");
        assert!(export.verb("get").is_some());
        assert!(export.verb("post").is_none(), "unchecked is dropped");
        assert!(
            semantics
                .factory("@fixture/http", "default", "create")
                .is_none()
        );
        assert!(
            semantics
                .surface("@fixture/http", "default", "instance:create")
                .is_none(),
            "an instance of a factory that failed states nothing"
        );

        let prefix = semantics
            .surface("fixture-prefix-http", "default", "instance:create")
            .expect("the prefix instance verified");
        let post = prefix.verb("post").expect("post verified on the instance");
        assert_eq!(post.method, "POST");
        assert_eq!(post.body, None, "its body claim has no verdict");
        assert_eq!(
            semantics
                .factory("fixture-prefix-http", "default", "create")
                .map(|factory| factory.base_url_key.as_str()),
            Some("prefixUrl")
        );
    }

    #[test]
    fn a_batch_that_is_not_one_verdict_per_check_in_order_verifies_nothing() {
        let checks = derive_claims(&sample_entries()).checks();
        let verified = |check: &SemanticsCheck| SemanticsResult {
            claim_id: check.claim_id.clone(),
            receiver: check.receiver.clone(),
            verdict: SemanticsVerdict::Verified,
            reason: None,
        };
        let exact: Vec<SemanticsResult> = checks.iter().map(verified).collect();
        assert!(answers_every_check(&checks, &exact));

        // One short.
        assert!(!answers_every_check(&checks, &exact[1..]));
        // One too many.
        let mut long = exact.clone();
        long.push(exact[0].clone());
        assert!(!answers_every_check(&checks, &long));
        // Every pair answered, two of them in each other's place.
        let mut swapped = exact.clone();
        swapped.swap(0, 1);
        assert!(!answers_every_check(&checks, &swapped));
        // A receiver the request did not name.
        let mut renamed = exact;
        renamed[0].receiver = "instance:other".to_string();
        assert!(!answers_every_check(&checks, &renamed));
        // No checks, no results: nothing to disagree about.
        assert!(answers_every_check(&[], &[]));
    }

    #[test]
    fn a_reask_is_wanted_only_for_an_unanswered_detection_with_an_installed_fetcher() {
        let repo = tempfile::tempdir().unwrap();
        let service = repo.path().join("services/api");
        std::fs::create_dir_all(&service).unwrap();
        let fetchers = vec!["@fixture/http".to_string()];
        let answered = sample_entries()
            .into_iter()
            .filter(|entry| entry.status != SemanticsStatus::Pending)
            .collect::<Vec<_>>();

        // Nothing installed: an answer could not be verified.
        assert!(!wants_reask(None, &fetchers, &service, repo.path()));

        // Installed at the repo root, above the service.
        let manifest = repo.path().join("node_modules/@fixture/http/package.json");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(&manifest, "{}").unwrap();
        assert!(wants_reask(None, &fetchers, &service, repo.path()));
        assert!(wants_reask(
            Some(&sample_entries()),
            &fetchers,
            &service,
            repo.path()
        ));
        assert!(!wants_reask(
            Some(&answered),
            &fetchers,
            &service,
            repo.path()
        ));
        assert!(!wants_reask(None, &[], &service, repo.path()));
    }
}
