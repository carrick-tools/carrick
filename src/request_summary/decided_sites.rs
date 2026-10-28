//! Where a discovered call's name is decided (carrick#2433, part of
//! carrick#2441): one request is one site, at the call that decides its path
//! or name, for every protocol.
//!
//! Discovery (carrick#2431) lists every call that reaches a labelled library
//! export, each with its chain down to the labelled call. Most of those calls
//! are the same request seen one level higher: a page calling a helper that
//! calls `fetch`. The rule is the request summaries' (a row stands at "the
//! site that fills the last hole a wrapper leaves open",
//! [`crate::agents::file_analyzer_agent::ResolutionSource::RequestSummary`]),
//! applied to the name slot of every protocol:
//!
//! - **The slot** is the argument, or one option key of it, the label of the
//!   labelled export names ([`ArgSlot`], the store's op `name`): the path or
//!   URL for HTTP, the event, topic, channel, job or message name otherwise.
//!   A label that names no slot (a channel that sends whatever object it is
//!   handed) leaves the labelled call itself as the site.
//! - **Composed down the chain.** The slot's value at the labelled call is
//!   kept as the summaries keep a URL ([`Piece`]): literal text, the
//!   enclosing function's parameters, and values this pass does not read.
//!   Each hop up fills the parameters with the arguments its call writes.
//! - **The reported site** is the lowest call whose value holds no
//!   parameter piece. A helper called from N sites with N literals gives N
//!   sites. An environment read or a base is an opaque piece, not a
//!   parameter: the environment decides it, not a caller.
//! - **A slot that keeps a parameter up to an entry point** (a function
//!   nothing calls) is a lead, `name_never_bound`. A parameter of a callback
//!   whose own caller is not this code abstains (`callback_parameter`): what
//!   fills it is whatever runs the callback.
//! - **A handler table** registered in a loop (`for (const [name, handler]
//!   of Object.entries(table)) bus.on(name, handler)` over a module constant
//!   object) gives one site per key, at the line of the function that
//!   handles it ([`loop_table`]).
//!
//! Discovery's chain is consumed as it is: the site's call, the call inside
//! each function the chain enters that calls the next, and the labelled
//! call. Only where a chain cannot be placed (a hop into a callback, two
//! calls of the next function in one body) or stops short (its top call
//! still holds a parameter and no listed site calls into its function) does
//! this pass walk up the call graph itself ([`Walk::up`]).
//!
//! Each site also says where its payload is bound ([`PayloadBinding`],
//! carrick#2472 C3), so a row the fact layer adds can be typed at its own
//! call.
//!
//! Record-only: nothing in a scan reads it (carrick#2441 lane 1). It reads
//! the request summaries' inputs and changes none of them, so no summary row
//! moves.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use swc_common::{SourceMap, Span, Spanned, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use super::row_calls::Reach;
use super::{CallIr, FnIr, KeyState, Piece, RequestSummaryInputs, Value, concat, render_target};

pub mod boundaries;
mod composed;
mod instances;
mod procedures;
pub use boundaries::RouteBoundaries;
pub use composed::Composed;
pub use instances::{
    InstanceAt, InstanceEnd, InstanceHole, MakerLabel, Prefix, ScopeBinds, ScopeLabel,
};
pub use procedures::{Procedure, ProcedureLabels};

/// The hop discovery records where a walk enters a function through a value
/// the call runs (carrick#2432, [`super::discovery`]).
pub(super) const VALUE_HOP: &str = "<value>";

/// The labelled call a discovery site reaches: the store key of the package
/// (`lib:lib.dom.d.ts` for the language's own library), the export and member, the
/// store role and op, and where the call is.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LabelledEnd {
    pub package: String,
    pub export: String,
    #[serde(default)]
    pub member: Option<String>,
    pub role: String,
    #[serde(default)]
    pub op: Option<String>,
    pub file: String,
    pub line: u32,
    pub span_start: u32,
    pub span_end: u32,
    /// The call constructs its callee (`new m(…)`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub new: bool,
}

impl LabelledEnd {
    /// `file:line`.
    pub fn at(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }
}

/// One function a discovery chain enters: its definition key, file and
/// line. `<callback>` is a function written at the call and run by the
/// callee.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainHop {
    pub function: String,
    pub file: String,
    pub line: u32,
}

/// One discovery site (carrick#2431's record): a call whose callee reaches
/// a labelled export, with the chain down to it, outermost first. Files are
/// repo-relative; spans are SWC's in the file parsed on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverySite {
    pub file: String,
    pub line: u32,
    pub span_start: u32,
    pub span_end: u32,
    #[serde(default)]
    pub callee: String,
    /// `None` when the walk ended with no labelled export.
    #[serde(default)]
    pub end: Option<LabelledEnd>,
    #[serde(default)]
    pub chain: Vec<ChainHop>,
    #[serde(default)]
    pub stop: Option<String>,
    /// The binding the labelled call is made on (carrick#2474 R1), with the
    /// receiver trace on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<super::discovery::ReceiverAt>,
    /// How discovery matched the labelled call, with the receiver trace on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reached: Option<super::discovery::ReachedBy>,
}

/// Discovery's census file: `{ "sites": [...] }`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DiscoveryFile {
    pub sites: Vec<DiscoverySite>,
}

/// An argument of a labelled call, or one option key of it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArgSlot {
    pub arg: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// Where a label says an HTTP member's method is read (label structure v2,
/// carrick#2434): an argument or one option key of it, the method sent when
/// none is given, or the one method the member always sends.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MethodLabel {
    #[serde(default)]
    pub from: Option<ArgSlot>,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub literal: Option<String>,
}

/// One op of a labelled export, as the library store answers it: the
/// export, its role and side, the op and member, and the slots its calls
/// fill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteLabel {
    pub package: String,
    pub export: String,
    #[serde(default)]
    pub member: Option<String>,
    pub role: String,
    #[serde(default)]
    pub op: Option<String>,
    #[serde(default)]
    pub side: Option<String>,
    #[serde(default)]
    pub name: Option<ArgSlot>,
    #[serde(default)]
    pub payload: Option<ArgSlot>,
    #[serde(default)]
    pub handler: Option<ArgSlot>,
    /// Label v2's method fields; a cassette until the store's rebuild.
    #[serde(default)]
    pub method: Option<MethodLabel>,
    /// The names the library emits itself on this op's member, which the
    /// store settled (carrick#2426): a call that spells one states no row.
    #[serde(default)]
    pub reserved: Vec<String>,
    /// Set on a label of a maker that runs a function the call supplies (a
    /// label with no `op`): the slot and the form it is claimed for.
    #[serde(default)]
    pub runs: Option<RunsMake>,
    /// A `receive` op's route fields (#2474 R0): where the handler's request
    /// holds its path and method. Read by the dispatch routes (R4a); no
    /// site's state carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<crate::services::type_sidecar::RequestAt>,
    /// The verifier verified this `receive` op but dropped its `request`
    /// (carrick#2979): the dispatch read takes no path or method from it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub request_dropped: bool,
}

/// A maker that runs the function its call supplies at `slot`
/// (carrick#2426): the function holds the request, found at its own site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunsMake {
    pub slot: ArgSlot,
    /// The maker is constructed (`new`), not called.
    #[serde(default)]
    pub new: bool,
}

impl SiteLabel {
    /// Every op the store's `/library-claims` answer labels, one label each.
    /// A maker (a construction of the export) labels no op and no name.
    pub fn from_store_answer(answer: &serde_json::Value) -> Vec<SiteLabel> {
        #[derive(Deserialize)]
        struct Op {
            op: String,
            #[serde(default)]
            member: Option<String>,
            #[serde(default)]
            name: Option<ArgSlot>,
            #[serde(default)]
            payload: Option<ArgSlot>,
            #[serde(default)]
            handler: Option<ArgSlot>,
            #[serde(default)]
            method: Option<MethodLabel>,
            #[serde(default)]
            request: Option<crate::services::type_sidecar::RequestAt>,
        }
        #[derive(Deserialize)]
        struct Export {
            export: String,
            role: String,
            #[serde(default)]
            side: Option<String>,
            #[serde(default)]
            ops: Vec<Op>,
        }
        #[derive(Deserialize)]
        struct Package {
            package: String,
            #[serde(default)]
            exports: Vec<Export>,
        }
        let packages: Vec<Package> = answer
            .get("library_claims")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        let mut labels = Vec::new();
        for package in packages {
            for export in package.exports {
                for op in export.ops {
                    labels.push(SiteLabel {
                        package: package.package.clone(),
                        export: export.export.clone(),
                        member: op.member,
                        role: export.role.clone(),
                        op: Some(op.op),
                        side: export.side.clone(),
                        name: op.name,
                        payload: op.payload,
                        handler: op.handler,
                        method: op.method,
                        reserved: Vec::new(),
                        runs: None,
                        request: op.request,
                        request_dropped: false,
                    });
                }
            }
        }
        labels
    }

    /// Every op the claims the scan settled label (carrick#2439), one label
    /// each, as [`Self::from_store_answer`] reads them off the store's raw
    /// answer: the scan holds the claims, already read and grouped by export.
    /// A maker that runs a function its call supplies labels no op, and
    /// carries `runs`; a scope labels nothing. A reserved name rides on the
    /// labels of the op it applies to.
    pub fn from_exports(exports: &[crate::library_claims::ExportClaims]) -> Vec<SiteLabel> {
        use crate::services::type_sidecar::{LibraryClaim, MakeForm, OpName};
        let word = |value: serde_json::Result<serde_json::Value>| -> Option<String> {
            value.ok()?.as_str().map(str::to_string)
        };
        let slot = |slot: &crate::services::type_sidecar::ClaimSlot| ArgSlot {
            arg: slot.arg as usize,
            key: slot.key.clone(),
        };
        let mut labels = Vec::new();
        for export in exports {
            let role = word(serde_json::to_value(export.role)).unwrap_or_default();
            let side = export
                .side
                .and_then(|side| word(serde_json::to_value(side)));
            for claim in &export.claims {
                if let LibraryClaim::Make {
                    form,
                    member,
                    runs: Some(runs),
                    ..
                } = claim
                {
                    labels.push(SiteLabel {
                        package: export.package.clone(),
                        export: export.export.clone(),
                        member: member.clone(),
                        role: role.clone(),
                        op: None,
                        side: side.clone(),
                        name: None,
                        payload: None,
                        handler: None,
                        method: None,
                        reserved: Vec::new(),
                        runs: Some(RunsMake {
                            slot: slot(runs),
                            new: *form == MakeForm::New,
                        }),
                        request: None,
                        request_dropped: false,
                    });
                    continue;
                }
                let LibraryClaim::Op {
                    op,
                    member,
                    path,
                    on,
                    of,
                    name,
                    payload,
                    handler,
                    method,
                    method_key,
                    method_default,
                    request,
                    ..
                } = claim
                else {
                    continue;
                };
                let method = (method.is_some() || method_key.is_some() || method_default.is_some())
                    .then(|| MethodLabel {
                        from: method_key.as_ref().map(slot),
                        default: method_default.clone(),
                        literal: method.clone(),
                    });
                labels.push(SiteLabel {
                    package: export.package.clone(),
                    export: export.export.clone(),
                    member: member.clone(),
                    role: role.clone(),
                    op: word(serde_json::to_value(op)),
                    side: side.clone(),
                    name: match name {
                        Some(OpName::Slot(name)) => Some(slot(name)),
                        _ => None,
                    },
                    payload: payload.as_ref().map(slot),
                    handler: handler.as_ref().map(slot),
                    method,
                    reserved: reserved_on(&export.claims, member.as_deref(), path, (*on, of)),
                    runs: None,
                    request: request.as_deref().cloned(),
                    request_dropped: false,
                });
            }
        }
        labels
    }

    /// The op label for `end` among `labels`: the same package, export and
    /// member. A maker's label (no op) is [`Self::runner`]'s.
    pub fn find<'l>(labels: &'l [SiteLabel], end: &LabelledEnd) -> Option<&'l SiteLabel> {
        labels.iter().find(|label| {
            label.op.is_some()
                && label.package == end.package
                && label.export == end.export
                && label.member == end.member
        })
    }

    /// The slot the call `end` names is handed a function in, when the store
    /// settled that the maker it calls runs that function (carrick#2426).
    pub fn runner<'l>(labels: &'l [SiteLabel], end: &LabelledEnd) -> Option<&'l ArgSlot> {
        labels.iter().find_map(|label| {
            let runs = label.runs.as_ref()?;
            (label.package == end.package
                && label.export == end.export
                && label.member == end.member
                && runs.new == end.new)
                .then_some(&runs.slot)
        })
    }
}

/// The receivers a claim applies to: the export, instances, or both; `of`
/// names one receiver.
fn receivers(on: Option<crate::services::type_sidecar::ClaimOn>, of: Option<&str>) -> (bool, bool) {
    use crate::services::type_sidecar::ClaimOn;
    match (of, on) {
        (Some("export"), _) => (true, false),
        (Some(_), _) => (false, true),
        (None, Some(ClaimOn::Export)) => (true, false),
        (None, Some(ClaimOn::Instance)) => (false, true),
        (None, _) => (true, true),
    }
}

/// The names the store settled the library emits itself on the member `member`
/// reached through `path`, where an op claimed on `selector` acts
/// (carrick#2426): the reserved claims of the same member and path whose
/// receivers overlap the op's, so one claimed on another receiver never hides
/// a name.
fn reserved_on(
    claims: &[crate::services::type_sidecar::LibraryClaim],
    member: Option<&str>,
    path: &[String],
    selector: (
        Option<crate::services::type_sidecar::ClaimOn>,
        &Option<String>,
    ),
) -> Vec<String> {
    use crate::services::type_sidecar::LibraryClaim;
    let (op_on, op_of) = selector;
    let mut names: Vec<String> = Vec::new();
    for claim in claims {
        let LibraryClaim::Reserved {
            member: reserved_member,
            name,
            path: reserved_path,
            on,
            of,
            ..
        } = claim
        else {
            continue;
        };
        if Some(reserved_member.as_str()) != member || reserved_path != path {
            continue;
        }
        let (a, b) = (
            receivers(op_on, op_of.as_deref()),
            receivers(*on, of.as_deref()),
        );
        let same_instance = match (op_of, of) {
            (Some(one), Some(other)) => one == other,
            _ => true,
        };
        if ((a.0 && b.0) || (a.1 && b.1)) && same_instance && !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}

/// Where the payload a decided site sends is bound (carrick#2472 C3): an
/// argument of the deciding call (one option key of it), or a call below it
/// where a wrapper builds the value, `file:line`, with that call's argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum PayloadBinding {
    Arg {
        arg: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    BuiltInside {
        /// `file:line` of the call that builds it.
        built_inside: String,
        /// That call's SWC span.
        span_start: u32,
        span_end: u32,
        arg: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
}

/// Where the value a deciding call's own option key holds is read, or that
/// nothing is written there (carrick#2674 P3-1a). Scanner-side only: it is
/// read by the payload slots alone, so it is never part of a site's state, its
/// sha or a cache key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotBinding {
    /// The deciding call's argument `arg` writes `key` as a plain property.
    Written { arg: usize, key: String },
    /// The deciding call's argument `arg` is an object literal that states
    /// `key` absent (no spread, no computed key, no method): the call sends
    /// nothing there.
    Unwritten { arg: usize, key: String },
}

/// The function that handles a receive site or one key of a handler table:
/// its repo-relative file, line and span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerAt {
    pub file: String,
    pub line: u32,
    pub span_start: u32,
    pub span_end: u32,
    /// The name written at the call when the handler is handed by name
    /// (`get('/x', list)`); `None` for a function written in place.
    pub name: Option<String>,
}

/// One call between a decided site and its labelled call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Via {
    /// `file:line`, repo-relative.
    pub at: String,
    /// The callee as written.
    pub callee: String,
}

/// A call where a name is decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecidedSite {
    /// Repo-relative, `/`-separated.
    pub file: String,
    pub line: u32,
    pub span_start: u32,
    pub span_end: u32,
    pub end: LabelledEnd,
    /// The slot's value at this call, composed down the chain. Empty where
    /// the label names no slot.
    pub name: Vec<Piece>,
    /// The key of a handler table this site handles.
    pub key: Option<String>,
    /// The arguments of this call the slot's value is read from: the
    /// label's at the labelled call itself.
    pub name_args: BTreeSet<usize>,
    /// The calls below this one down to the labelled call, outermost
    /// first: each is written inside the function the call above enters.
    pub via: Vec<Via>,
    /// Where the payload is bound: at the label's payload slot, or for a
    /// receive at its handler slot (the payload arrives in the handler's
    /// first parameter).
    pub payload: Option<PayloadBinding>,
    /// Where the payload's option key is read at the deciding call, carried
    /// up through a spread or a destructured rest of the caller's options
    /// (carrick#2674 P3-1a). `payload` stays as it is: it feeds the forced
    /// `payload_argument` slot and `anchors.payload_at`, which are on the
    /// wire. Read only by the payload slots.
    pub payload_slot: Option<SlotBinding>,
    /// The function that handles a receive site, or a handler table's key.
    pub handler: Option<HandlerAt>,
    /// What the label's method slot holds here, when it names one.
    pub method: Option<MethodValue>,
    /// The binding the labelled call is made on (carrick#2474 R1), with the
    /// receiver trace on: where the registration is made, wherever the name
    /// is decided.
    pub receiver: Option<super::discovery::ReceiverAt>,
    /// How discovery matched the labelled call, with the receiver trace on.
    pub reached: Option<super::discovery::ReachedBy>,
    /// With the receiver trace on, the instance the labelled call is made
    /// on, one end per caller that hands one in (carrick#2474 R2a). Read by
    /// nothing; never part of a record's state.
    pub instances: Vec<InstanceEnd>,
    /// With the receiver trace on, the full path the route is served at
    /// per instance end, every scope and mount prefix joined from the top
    /// down (carrick#2474 R2b). Read by nothing; never part of a record's
    /// state.
    pub composed: Vec<Composed>,
    /// Whether `composed` stopped at its cap
    /// ([`composed::MAX_COMPOSED`] chains, and paths): some paths are
    /// left out.
    pub composed_trimmed: bool,
    /// With the receiver trace on, for a procedure op's site (its label
    /// names it by its key, carrick#2474 R6c), the paths the adapters that
    /// serve its router serve it at ([`procedures`]); `None` for every
    /// other site. Never part of a record's state.
    pub procedure: Option<Procedure>,
}

impl DecidedSite {
    /// The name as text: every piece literal, or for HTTP a target the
    /// summaries would write (an opaque base before a literal path).
    pub fn name_text(&self) -> Option<String> {
        if let Some(key) = &self.key {
            return Some(key.clone());
        }
        let literal: Option<String> = self
            .name
            .iter()
            .map(|piece| match piece {
                Piece::Lit(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        match literal {
            Some(text) if !text.is_empty() => Some(text),
            _ if self.end.role == "http_client" => {
                named_base(&self.name).and_then(|name| render_target(&name))
            }
            _ => None,
        }
    }
}

/// The URL with its leading value written the way a key can carry it
/// (carrick#2524). A value this pass stopped reading at an expression (a
/// constant whose initializer is a call) carries that expression's source
/// text. Where the text reads an environment variable
/// (`(process.env.API || "…").replace(…)`), the key would keep that text
/// verbatim, as an environment base it cannot classify: the base is written
/// as the one read it holds (`process.env.API`), as every other environment
/// base is, or, holding more than one, the name is unstated (`None`). Any
/// other leading value is kept as it is.
fn named_base(pieces: &[Piece]) -> Option<std::borrow::Cow<'_, [Piece]>> {
    use std::borrow::Cow;
    let Some(Piece::Opaque {
        text,
        end: super::Root::Hole(reason),
        ..
    }) = pieces.first()
    else {
        return Some(Cow::Borrowed(pieces));
    };
    // A value that reads as several paths is never renamed to the one
    // environment read its text holds: no row is keyed on it (carrick#2541).
    if *reason == super::HoleReason::SeveralPaths || is_reference(text) {
        return Some(Cow::Borrowed(pieces));
    }
    let reads = env_reads(text);
    match reads.as_slice() {
        [] => Some(Cow::Borrowed(pieces)),
        [read] => {
            let mut named = pieces.to_vec();
            named[0] = Piece::Opaque {
                text: read.clone(),
                end: super::Root::Env {
                    written: read.clone(),
                    name: read.rsplit('.').next().unwrap_or(read).to_string(),
                    fallback: None,
                },
                link: None,
            };
            Some(Cow::Owned(named))
        }
        _ => None,
    }
}

/// Every distinct environment read `text` writes, `process.env.NAME` or
/// `import.meta.env.NAME`, as written.
fn env_reads(text: &str) -> Vec<String> {
    let mut reads: Vec<String> = Vec::new();
    for prefix in ["process.env.", "import.meta.env."] {
        let mut rest = text;
        while let Some(at) = rest.find(prefix) {
            let after = &rest[at + prefix.len()..];
            let name: String = after
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                .collect();
            if !name.is_empty() {
                let read = format!("{prefix}{name}");
                if !reads.contains(&read) {
                    reads.push(read);
                }
            }
            rest = after;
        }
    }
    reads
}

/// `name`, `a.b.c`, `this.#field`, `a?.b`: a name and the members read off
/// it, and nothing else.
fn is_reference(text: &str) -> bool {
    let text = text.strip_prefix("this.").unwrap_or(text);
    !text.is_empty()
        && text.split('.').all(|part| {
            let part = part.strip_suffix('?').unwrap_or(part);
            let part = part.strip_prefix('#').unwrap_or(part);
            let mut chars = part.chars();
            chars
                .next()
                .is_some_and(|first| first.is_alphabetic() || first == '_' || first == '$')
                && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        })
}

/// Why a discovered call names nothing a caller decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Undecided {
    /// The slot keeps a parameter up to a function nothing calls.
    NameNeverBound,
    /// The slot holds a parameter of a callback this code does not call.
    CallbackParameter,
    /// The call reaches an export the store labels with a network role and
    /// no op (carrick#2515): the store says nothing about what its members
    /// do, so no slot can be read. The call is listed, not named.
    NoOpClaim,
    /// The callee is a value whose values reach different labelled exports,
    /// or one of them none (carrick#2432): which one runs is the caller's.
    SeveralValues,
    /// Walks from different branches of one function reach the same labelled
    /// call and settle at this one, each with a different operation
    /// (carrick#2816): which runs depends on the call's arguments.
    SeveralOperations,
}

/// A discovered call whose name no call of this code decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecidedLead {
    pub file: String,
    pub line: u32,
    pub span_start: u32,
    pub span_end: u32,
    /// Functions the walk entered from this call (0 when it is at the call
    /// that holds the open name).
    pub depth: u32,
    pub end: LabelledEnd,
    pub reason: Undecided,
}

/// What [`decided_sites`] finds.
#[derive(Debug, Default)]
pub struct Decided {
    pub sites: Vec<DecidedSite>,
    pub leads: Vec<DecidedLead>,
    /// Discovery sites whose labelled call this pass could not place in the
    /// summaries' inputs, by `file:line`.
    pub unplaced: Vec<String>,
    /// Sites whose chain was walked up the call graph rather than read from
    /// discovery's chain (a hop into a callback, an ambiguous hop, or a
    /// chain that stops short), by `file:line`.
    pub walked: Vec<String>,
    /// Calls that state no row because they hand a maker the function it
    /// runs (carrick#2426), by `file:line`.
    pub ran: Vec<String>,
    /// Sites that state no row because their name is one the library emits
    /// itself (carrick#2426), by `file:line`.
    pub reserved: Vec<String>,
    /// Calls above a decided site on its chain (carrick#2614).
    pub through: Vec<Through>,
    /// With the receiver trace on, where the service's route chains leave
    /// it (carrick#2474 R5a). Read by nothing yet; never part of a record's
    /// state.
    pub boundaries: RouteBoundaries,
    /// With the receiver trace on, each value the procedure pass stopped
    /// at with no procedure to count it on (carrick#2474 R6c), once per
    /// adapter.
    pub procedure_stops: usize,
    /// With the receiver trace on, the router mounts it ignored because a
    /// file only tests import makes them (carrick#2775), by `file:line`.
    /// Counted in the run log.
    pub test_only_mounts: Vec<String>,
}

/// A discovered call whose request is decided lower down its chain
/// (carrick#2614): `await login(…)` where `login`'s own call writes the path.
/// Also each call of the function whose body writes a decided site
/// ([`callers_above`], carrick#2704).
/// The call is not a second request; the site at `site_file:site_line` is.
/// Recorded only where each hop from this call down to the site is the
/// definition [`RequestSummaryInputs::reach`] reads off the compiler's `call`
/// row (or, for a bare call of a function the same file declares, SWC's exact
/// binding), never the unresolved-import fallback to the call graph's name
/// join, so a call into another service or an unresolved one is never
/// recorded.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Through {
    /// Repo-relative, `/`-separated.
    pub file: String,
    pub line: u32,
    pub site_file: String,
    pub site_line: u32,
    /// The caller's line holds a call besides the through call and the calls
    /// written around it (carrick#2704): a reader row there may be that
    /// other call's, so a disagreeing one is not folded.
    pub other_call: bool,
}

/// The [`Through`] from `chain[0]` to the site at `chain[to]`, when `to` is
/// below it and every hop between is one the compiler's rows resolve.
fn through(
    inputs: &RequestSummaryInputs,
    root: &Path,
    chain: &[Placed<'_>],
    to: usize,
) -> Option<Through> {
    if to == 0 || to >= chain.len() || inputs.row_calls.is_none() {
        return None;
    }
    let resolved = chain[..=to].windows(2).all(|pair| {
        let (call, into) = (&pair[0], &pair[1]);
        let (file, lo, hi) = (call.file, call.call.site.lo, call.call.site.hi);
        !inputs.fell_back(file, lo, hi)
            && matches!(
                inputs.reach(file, lo, hi),
                Reach::Function(target, key)
                    if target == into.file && into.key == Some(key.as_str())
            )
    });
    resolved.then(|| Through {
        file: relative(root, chain[0].file),
        line: chain[0].call.site.line,
        site_file: relative(root, chain[to].file),
        site_line: chain[to].call.site.line,
        other_call: false,
    })
}

/// The calls of the function whose body writes each decided site, as
/// [`Through`]s to that site (carrick#2704). A site decided where discovery
/// listed no call above it (a walk up the callers, or a request written in a
/// function handed to a package: `span(name, () => http.get(path))`) still
/// has callers that make its request, and a reader row at one is that
/// request read again. A site written in a function nested in its owner
/// counts only where that function is an argument of a call: one the owner
/// returns or stores is not run by calling the owner. Each hop is checked as
/// [`through`] checks it.
fn callers_above(
    inputs: &RequestSummaryInputs,
    root: &Path,
    calls: &Calls<'_>,
    sites: &[DecidedSite],
) -> Vec<Through> {
    let mut out = Vec::new();
    for site in sites.iter().filter(|site| site.key.is_none()) {
        let Some(placed) = calls.at_span(&root.join(&site.file), site.span_start, site.span_end)
        else {
            continue;
        };
        let Some(key) = placed.key else {
            continue;
        };
        if placed.depth > 0 && placed.host.is_none() {
            continue;
        }
        for caller in calls.callers_of(placed.file, key) {
            if let Some(mut through) = through(inputs, root, &[*caller, placed], 1) {
                through.other_call = calls.other_call_on_line(caller);
                out.push(through);
            }
        }
    }
    out
}

/// One call, where it is written: its file, the definition key of the
/// function whose body holds it (`None` at module level), how many functions
/// deep inside that one it is, and the body that writes it.
#[derive(Clone, Copy)]
pub(super) struct Placed<'a> {
    pub(super) file: &'a Path,
    pub(super) key: Option<&'a str>,
    pub(super) depth: u8,
    pub(super) call: &'a CallIr,
    /// For a call inside a function written as a call's argument, that
    /// call: what runs the innermost function the call is written in
    /// (carrick#2474 R2a). `None` at depth 0, and for a function written
    /// anywhere else.
    pub(super) host: Option<&'a CallIr>,
    /// Where the innermost function the call is written in sits in a
    /// call's argument (carrick#2474 R2b): the argument, and the keys down
    /// to it. `None` at depth 0, and for a function written anywhere else.
    pub(super) written_in: Option<&'a super::guards::WrittenIn>,
}

/// The definition a call reaches, as a callers index reads it: its file and
/// key.
type Target = Option<(PathBuf, String)>;

/// Every call of the service, by file and span, by the function whose body
/// holds it, and by the function of the service the call graph resolves it
/// to.
pub(super) struct Calls<'a> {
    /// The service's own inputs, and the siblings whose files are indexed
    /// (carrick#2618): a call is looked up in the call graph of the service
    /// that holds its file.
    own: &'a RequestSummaryInputs,
    siblings: Vec<&'a RequestSummaryInputs>,
    by_span: HashMap<(&'a Path, u32, u32), Placed<'a>>,
    /// The calls of the indexed sibling files, by file and span. Kept apart
    /// from [`Self::by_span`], which the instance and boundary reads walk as
    /// the service's own calls.
    aside: HashMap<(&'a Path, u32, u32), Placed<'a>>,
    by_owner: HashMap<(&'a Path, &'a str), Vec<Placed<'a>>>,
    callers: HashMap<(PathBuf, String), Vec<Placed<'a>>>,
    /// The definition of a sibling a call of this service reaches, by the
    /// call's file and span (carrick#2618).
    cross: HashMap<(&'a Path, u32, u32), (PathBuf, String)>,
}

impl<'a> Calls<'a> {
    fn index(inputs: &'a RequestSummaryInputs) -> Self {
        Self::build(inputs, &[], |source, file, call| {
            source
                .sites
                .target_at(file, call.site.lo, call.site.hi)
                .cloned()
        })
    }

    /// [`Self::index`] over this service's files and the files of the
    /// `siblings` the discovered chains enter (carrick#2618): a sibling's own
    /// calls take their edges from the sibling's call graph, and a call of
    /// this service whose row reaches a sibling's definition is a caller of
    /// that definition.
    fn index_with(
        inputs: &'a RequestSummaryInputs,
        siblings: &[&'a RequestSummaryInputs],
        entered: &HashSet<PathBuf>,
    ) -> Self {
        let entered: Vec<&'a RequestSummaryInputs> = siblings
            .iter()
            .copied()
            .filter(|sibling| sibling.files.keys().any(|file| entered.contains(file)))
            .collect();
        let mut calls = Self::build(inputs, &entered, |source, file, call| {
            source
                .sites
                .target_at(file, call.site.lo, call.site.hi)
                .cloned()
        });
        if entered.is_empty() {
            return calls;
        }
        let own: Vec<Placed<'a>> = calls.by_span.values().copied().collect();
        for placed in own {
            let Reach::Function(target, key) =
                super::discovery::reach_across(inputs, &entered, placed.file, placed.call)
            else {
                continue;
            };
            if inputs.files.contains_key(&target) {
                continue;
            }
            let at = (
                placed.file,
                placed.call.site.span_start,
                placed.call.site.span_end,
            );
            calls.cross.insert(at, (target.clone(), key.clone()));
            calls.callers.entry((target, key)).or_default().push(placed);
        }
        calls
    }

    /// The same index, each call's callers edge read from what the rows say
    /// it reaches ([`RequestSummaryInputs::reach`], rows first): the callers
    /// discovery's value walk reads (carrick#2432).
    pub(super) fn through_reach(inputs: &'a RequestSummaryInputs) -> Self {
        Self::build(inputs, &[], |source, file, call| {
            match source.reach(file, call.site.lo, call.site.hi) {
                super::row_calls::Reach::Function(target, key) => Some((target, key)),
                _ => None,
            }
        })
    }

    fn build(
        inputs: &'a RequestSummaryInputs,
        siblings: &[&'a RequestSummaryInputs],
        target: impl Fn(&RequestSummaryInputs, &Path, &CallIr) -> Target,
    ) -> Self {
        struct Source<'s, 'a> {
            source: &'a RequestSummaryInputs,
            own: bool,
            target: &'s dyn Fn(&RequestSummaryInputs, &Path, &CallIr) -> Target,
        }
        /// The file a walk reads, with the definitions the extractor keyed in
        /// it by where their function node starts.
        struct InFile<'s, 'a> {
            source: &'s Source<'s, 'a>,
            file: &'a Path,
            by_start: &'s HashMap<u32, &'a str>,
        }
        fn walk<'a>(
            in_file: &InFile<'_, 'a>,
            key: Option<&'a str>,
            depth: u8,
            ir: &'a FnIr,
            host: Option<&'a CallIr>,
            out: &mut Calls<'a>,
        ) {
            let (at, file) = (in_file.source, in_file.file);
            let written_in = if depth == 0 {
                None
            } else {
                ir.written_in.as_ref()
            };
            for call in ir.calls.iter().chain(&ir.constructions) {
                let placed = Placed {
                    file,
                    key,
                    depth,
                    call,
                    host,
                    written_in,
                };
                let span = (file, call.site.span_start, call.site.span_end);
                if at.own {
                    out.by_span.entry(span).or_insert(placed);
                } else {
                    out.aside.entry(span).or_insert(placed);
                }
                if let Some(key) = key {
                    out.by_owner.entry((file, key)).or_default().push(placed);
                }
                if let Some(target) = (at.target)(at.source, file, call) {
                    out.callers.entry(target).or_default().push(placed);
                }
            }
            for (index, inner) in ir.nested.iter().enumerate() {
                let host = ir.calls.iter().find(|call| {
                    call.args
                        .iter()
                        .any(|arg| matches!(arg, Value::Callback(at) if *at == index))
                });
                walk(in_file, key, depth.saturating_add(1), inner, host, out);
            }
            for inner in &ir.detached {
                // A function module-level code writes outside any call that
                // the extractor keys itself (a member of an exported object
                // literal, `itemsApi.replace`, carrick#2793) is read under its
                // own key, as a declaration is: its callers are the calls the
                // call graph resolves to that key, and its parameters are its
                // own. The chains key it the same way (`guards`,
                // carrick#2386). A function handed to a call stays its
                // caller's, whatever the extractor names it.
                let own = (key.is_none() && depth == 0)
                    .then_some(inner.site.as_ref())
                    .flatten()
                    .and_then(|site| in_file.by_start.get(&site.lo).copied());
                match own {
                    Some(own) => walk(in_file, Some(own), 0, inner, None, out),
                    None => walk(in_file, key, depth.saturating_add(1), inner, None, out),
                }
            }
        }
        let mut calls = Calls {
            own: inputs,
            siblings: siblings.to_vec(),
            by_span: HashMap::new(),
            aside: HashMap::new(),
            by_owner: HashMap::new(),
            callers: HashMap::new(),
            cross: HashMap::new(),
        };
        for (position, source) in std::iter::once(inputs)
            .chain(siblings.iter().copied())
            .enumerate()
        {
            let at = Source {
                source,
                own: position == 0,
                target: &target,
            };
            let mut paths: Vec<&PathBuf> = source.files.keys().collect();
            paths.sort();
            for path in paths {
                let file = &source.files[path];
                let by_start: HashMap<u32, &str> = file
                    .definitions
                    .iter()
                    .filter_map(|(key, (start, _))| Some(((*start)?, key.as_str())))
                    .collect();
                let in_file = InFile {
                    source: &at,
                    file: path,
                    by_start: &by_start,
                };
                let mut keys: Vec<&String> = file.functions.keys().collect();
                keys.sort();
                for key in keys {
                    walk(
                        &in_file,
                        Some(key.as_str()),
                        0,
                        &file.functions[key],
                        None,
                        &mut calls,
                    );
                }
                walk(&in_file, None, 0, &file.module_level, None, &mut calls);
            }
        }
        calls
    }

    fn at_span(&self, file: &Path, span_start: u32, span_end: u32) -> Option<Placed<'a>> {
        let at = (file, span_start, span_end);
        self.by_span
            .get(&at)
            .or_else(|| self.aside.get(&at))
            .copied()
    }

    /// Whether `file` is one of this service's own.
    fn is_own(&self, file: &Path) -> bool {
        self.own.files.contains_key(file)
    }

    /// Whether a call besides `placed` and the calls written around it sits
    /// on `placed`'s line in its file.
    fn other_call_on_line(&self, placed: &Placed<'a>) -> bool {
        let (start, end) = (placed.call.site.span_start, placed.call.site.span_end);
        self.by_span.values().any(|other| {
            other.file == placed.file
                && other.call.site.line == placed.call.site.line
                && (other.call.site.span_start, other.call.site.span_end) != (start, end)
                && !(other.call.site.span_start <= start && end <= other.call.site.span_end)
        })
    }

    fn target(&self, placed: &Placed<'a>) -> Option<&(PathBuf, String)> {
        let at = (
            placed.file,
            placed.call.site.span_start,
            placed.call.site.span_end,
        );
        if let Some(crossed) = self.cross.get(&at) {
            return Some(crossed);
        }
        let holder = if self.is_own(placed.file) {
            self.own
        } else {
            self.siblings
                .iter()
                .copied()
                .find(|sibling| sibling.files.contains_key(placed.file))
                .unwrap_or(self.own)
        };
        holder
            .sites
            .target_at(placed.file, placed.call.site.lo, placed.call.site.hi)
    }

    pub(super) fn callers_of(&self, file: &Path, key: &str) -> &[Placed<'a>] {
        self.callers
            .get(&(file.to_path_buf(), key.to_string()))
            .map_or(&[], Vec::as_slice)
    }
}

/// Whether any piece is a parameter a caller fills.
fn holds_parameter(pieces: &[Piece]) -> bool {
    pieces.iter().any(Piece::is_parameter)
}

/// Whether a call whose slot value is `pieces` decides the name, so it is
/// the site and its callers are not walked: the value holds no parameter, or
/// for HTTP it states a route the way the request summaries compose one
/// ([`render_target`]: a base, a literal segment, each parameter a whole
/// path segment, a value in the query cut off). A parameter that is the
/// whole path, or a prefix glued to one, leaves it to the callers
/// (carrick#2433, carrick#2519).
fn decides(pieces: &[Piece], end: &LabelledEnd) -> bool {
    !holds_parameter(pieces) || (end.role == "http_client" && render_target(pieces).is_some())
}

/// `pieces`, read where a call at `depth` inside its definition sits, as
/// that definition's own body reads them: every parameter must be the
/// definition's (an enclosing parameter exactly `depth` functions out).
/// `None` when one is a callback's own.
fn to_definition(pieces: &[Piece], depth: u8) -> Option<Vec<Piece>> {
    if depth == 0 {
        return Some(pieces.to_vec());
    }
    pieces
        .iter()
        .map(|piece| match piece {
            Piece::Outer(out, inner) if *out == depth => Some((**inner).clone()),
            piece if piece.is_parameter() => None,
            other => Some(other.clone()),
        })
        .collect()
}

/// The argument `call` writes at `index`, unless a spread before it moves
/// every position. With the receiver trace on, a spread leaves the
/// arguments before it as written ([`CallIr::spread_from`]).
fn argument(call: &CallIr, index: usize) -> Option<&Value> {
    match (&call.site_args, call.spread_from) {
        (Some(_), _) => call.args.get(index),
        (None, Some(from)) if index < from => call.args.get(index),
        _ => None,
    }
}

/// `pieces` (the callee's own, its parameters by position) with each
/// parameter filled from the arguments `call` writes. A position the call
/// does not write, or one after a spread, is not known.
fn filled(pieces: &[Piece], call: &CallIr) -> Vec<Piece> {
    concat(pieces.iter().map(|piece| {
        match piece {
            Piece::Param(index, _) => {
                argument(call, *index).map_or(vec![Piece::Unknown], Value::pieces)
            }
            Piece::ParamKey(index, key, name) => match argument(call, *index) {
                Some(Value::Obj(object)) => object
                    .fields
                    .get(key)
                    .map_or(vec![Piece::Unknown], Value::pieces),
                // The caller passes its own parameter on whole: its key waits on
                // the caller's caller.
                Some(Value::Str(pieces)) => match pieces.as_slice() {
                    [Piece::Param(passed, _)] => {
                        vec![Piece::ParamKey(*passed, key.clone(), name.clone())]
                    }
                    _ => vec![Piece::Unknown],
                },
                _ => vec![Piece::Unknown],
            },
            other => vec![other.clone()],
        }
    }))
}

/// One key of a value that is a whole parameter: of the function the call
/// is written in, or of one around it when the call sits in a function
/// written inside that one (`span(() => fetch(url, init))` reads `init`'s
/// `method` as a key of the enclosing function's parameter). `None` when
/// the value is anything else.
fn keyed(pieces: &[Piece], key: &str) -> Option<Piece> {
    match pieces {
        [Piece::Param(index, name)] => Some(Piece::ParamKey(*index, key.to_string(), name.clone())),
        [Piece::Outer(depth, inner)] => keyed(std::slice::from_ref(&**inner), key)
            .map(|piece| Piece::Outer(*depth, Box::new(piece))),
        _ => None,
    }
}

/// The argument positions `pieces` reads, as the callee's own parameters.
fn parameter_positions(pieces: &[Piece]) -> BTreeSet<usize> {
    pieces
        .iter()
        .filter_map(|piece| match piece {
            Piece::Param(index, _) | Piece::ParamKey(index, ..) => Some(*index),
            _ => None,
        })
        .collect()
}

/// The slot's value at the labelled call, as the body that writes it reads
/// it.
fn slot_value(call: &CallIr, slot: &ArgSlot) -> Vec<Piece> {
    let Some(arg) = argument(call, slot.arg) else {
        return vec![Piece::Unknown];
    };
    match (&slot.key, arg) {
        (None, value) => value.pieces(),
        (Some(key), Value::Obj(object)) => object
            .fields
            .get(key)
            .map_or(vec![Piece::Unknown], Value::pieces),
        (Some(key), Value::Str(pieces)) => {
            keyed(pieces, key).map_or(vec![Piece::Unknown], |piece| vec![piece])
        }
        _ => vec![Piece::Unknown],
    }
}

/// Where the payload `slot` names at the labelled call (the last of `down`)
/// is bound, read from the deciding call (the first): carried up while each
/// call hands on a parameter of its own function whole, and built inside the
/// first call that writes anything else there. `None` when a call hands
/// nothing there: it sends no payload.
fn payload_binding(root: &Path, down: &[Placed<'_>], slot: &ArgSlot) -> Option<PayloadBinding> {
    let (at, arg, key) = bound_at(down, slot)?;
    if at == 0 {
        return Some(PayloadBinding::Arg { arg, key });
    }
    let placed = &down[at];
    Some(PayloadBinding::BuiltInside {
        built_inside: format!("{}:{}", relative(root, placed.file), placed.call.site.line),
        span_start: placed.call.site.span_start,
        span_end: placed.call.site.span_end,
        arg,
        key,
    })
}

/// Where the value `slot` names at the labelled call (the last of `down`)
/// is written, as [`payload_binding`] reads it: the index in `down` of the
/// call that writes it, and that call's argument and option key.
fn bound_at(down: &[Placed<'_>], slot: &ArgSlot) -> Option<(usize, usize, Option<String>)> {
    walk_bound(down, slot, false)
}

/// The option key `slot` names, read at the deciding call (`down[0]`) by
/// carrying it up through every call that hands the caller's options on
/// (carrick#2674 P3-1a): a hop whose object does not write the key and holds
/// the caller's options under a spread of its own parameter or the rest of a
/// destructured one, as the method slot reads them ([`written_at`]). The
/// carry that stops below the deciding call gives nothing: a call below
/// builds the value.
fn payload_slot(down: &[Placed<'_>], slot: &ArgSlot) -> Option<SlotBinding> {
    let key = slot.key.as_ref()?;
    let (0, arg, Some(carried)) = walk_bound(down, slot, true)? else {
        return None;
    };
    let Value::Obj(object) = argument(down.first()?.call, arg)? else {
        return None;
    };
    match object.key(&carried) {
        KeyState::Known(_) => Some(SlotBinding::Written {
            arg,
            key: key.clone(),
        }),
        KeyState::Absent => Some(SlotBinding::Unwritten {
            arg,
            key: key.clone(),
        }),
        KeyState::Unread | KeyState::Function => None,
    }
}

/// The walk behind [`bound_at`]. With `options` it also carries an option
/// key through a hop's spread or rest ([`payload_slot`]), and returns `None`
/// where it would stop below the deciding call.
fn walk_bound(
    down: &[Placed<'_>],
    slot: &ArgSlot,
    options: bool,
) -> Option<(usize, usize, Option<String>)> {
    let whole_param = |value: &Value| match value {
        Value::Str(pieces) => match pieces.as_slice() {
            [Piece::Param(index, _)] => Some(*index),
            _ => None,
        },
        _ => None,
    };
    let mut arg = slot.arg;
    let mut key = slot.key.clone();
    for k in (1..down.len()).rev() {
        let placed = &down[k];
        if placed.call.site_args.is_some() && placed.call.args.get(arg).is_none() {
            return None;
        }
        let passed = match (&key, argument(placed.call, arg)) {
            (_, None) => None,
            (None, Some(value)) => whole_param(value).map(|index| (index, None)),
            (Some(field), Some(Value::Obj(object))) => object
                .fields
                .get(field)
                .and_then(whole_param)
                .map(|index| (index, None)),
            (Some(field), Some(value)) => {
                whole_param(value).map(|index| (index, Some(field.clone())))
            }
        };
        match passed.filter(|_| placed.depth == 0) {
            Some((index, field)) => {
                arg = index;
                key = field;
                continue;
            }
            None if !options => return Some((k, arg, key)),
            None => {}
        }
        // The caller's options under a spread or a rest, with the key not
        // written beside it (the method slot's two arms).
        let (Some(field), Some(Value::Obj(object))) = (&key, argument(placed.call, arg)) else {
            return None;
        };
        if object.fields.contains_key(field) {
            return None;
        }
        arg = match (&object.under_param, &object.under_rest) {
            (Some((index, before)), _) if placed.depth == 0 && !before.contains_key(field) => {
                *index
            }
            (None, Some((depth, index, taken)))
                if *depth == placed.depth && !taken.contains(field) =>
            {
                *index
            }
            _ => return None,
        };
    }
    let first = down.first()?;
    if first.call.site_args.is_some() && first.call.args.get(arg).is_none() {
        return None;
    }
    Some((0, arg, key))
}

/// What a label's method slot holds at a decided site (label v2,
/// carrick#2434).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodValue {
    /// No call down to the labelled one writes the slot (the argument is
    /// absent, or an object with no spread lacks the key): the label's
    /// default is sent.
    Unwritten,
    /// The slot's value as the deciding call's arguments write it.
    Pieces(Vec<Piece>),
}

/// The value `call` writes at argument `arg` (one option key of it).
fn written_at(call: &CallIr, arg: usize, key: Option<&str>) -> MethodValue {
    if call.site_args.is_some() && call.args.get(arg).is_none() {
        return MethodValue::Unwritten;
    }
    let Some(value) = argument(call, arg) else {
        return MethodValue::Pieces(vec![Piece::Unknown]);
    };
    match (key, value) {
        (None, value) => MethodValue::Pieces(value.pieces()),
        (Some(key), Value::Obj(object)) => match (object.fields.get(key), &object.under_param) {
            (Some(value), _) => MethodValue::Pieces(value.pieces()),
            (None, None) if !object.open => MethodValue::Unwritten,
            // A spread of the function's own parameter with no key written
            // after it: the key is whatever the caller's object puts there,
            // read at the caller as a key of that parameter. A key written
            // before the spread may be overwritten by it: not read.
            (None, Some((index, before))) if !before.contains_key(key) => MethodValue::Pieces(
                vec![Piece::ParamKey(*index, key.to_string(), key.to_string())],
            ),
            // The rest of a destructured parameter, spread first
            // (carrick#2519): it holds the caller's key unless the pattern
            // took the key out.
            (None, None) => match &object.under_rest {
                Some((depth, index, taken)) if !taken.contains(key) => {
                    let own = Piece::ParamKey(*index, key.to_string(), key.to_string());
                    MethodValue::Pieces(vec![match depth {
                        0 => own,
                        out => Piece::Outer(*out, Box::new(own)),
                    }])
                }
                _ => MethodValue::Pieces(vec![Piece::Unknown]),
            },
            (None, _) => MethodValue::Pieces(vec![Piece::Unknown]),
        },
        (Some(key), Value::Str(pieces)) => {
            MethodValue::Pieces(vec![keyed(pieces, key).unwrap_or(Piece::Unknown)])
        }
        _ => MethodValue::Pieces(vec![Piece::Unknown]),
    }
}

/// The method `slot` reads at the labelled call (the last of `down`),
/// composed up to the deciding call (the first) as the name is: each call
/// fills the parameters of the function below it.
fn method_value(down: &[Placed<'_>], slot: &ArgSlot) -> Option<MethodValue> {
    let last = down.last()?;
    let mut value = written_at(last.call, slot.arg, slot.key.as_deref());
    for k in (0..down.len() - 1).rev() {
        let MethodValue::Pieces(pieces) = &value else {
            break;
        };
        if !holds_parameter(pieces) {
            break;
        }
        let Some(own) = to_definition(pieces, down[k + 1].depth) else {
            return Some(MethodValue::Pieces(vec![Piece::Unknown]));
        };
        value = match own.as_slice() {
            [Piece::Param(index, _)] => written_at(down[k].call, *index, None),
            [Piece::ParamKey(index, key, _)] => written_at(down[k].call, *index, Some(key)),
            _ => MethodValue::Pieces(filled(&own, down[k].call)),
        };
    }
    Some(value)
}

/// The functions a module declares at its top level by name: function
/// declarations, and constants holding an arrow or a function expression.
fn module_functions(module: &Module) -> HashMap<String, Span> {
    let mut functions: HashMap<String, Span> = HashMap::new();
    for item in &module.body {
        let decl = match item {
            ModuleItem::Stmt(Stmt::Decl(decl)) => decl,
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => &export.decl,
            _ => continue,
        };
        match decl {
            Decl::Fn(function) => {
                functions.insert(function.ident.sym.to_string(), function.function.span);
            }
            Decl::Var(var) if var.kind == VarDeclKind::Const => {
                for declarator in &var.decls {
                    let Pat::Ident(name) = &declarator.name else {
                        continue;
                    };
                    match declarator.init.as_deref().map(super::unwrap_value) {
                        Some(Expr::Arrow(arrow)) => {
                            functions.insert(name.id.sym.to_string(), arrow.span);
                        }
                        Some(Expr::Fn(function)) => {
                            functions.insert(name.id.sym.to_string(), function.function.span);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    functions
}

/// The function `placed`'s call hands at argument `arg`: one written there,
/// or one the module declares under the name written there; otherwise the
/// argument itself.
fn handler_at(root: &Path, placed: &Placed<'_>, arg: usize) -> Option<HandlerAt> {
    let cm: Lrc<SourceMap> = Default::default();
    let handler = swc_common::errors::Handler::with_tty_emitter(
        swc_common::errors::ColorConfig::Never,
        false,
        false,
        Some(cm.clone()),
    );
    let module = crate::parser::parse_file(placed.file, &cm, &handler)?;
    let mut finder = CallAt {
        target: (placed.call.site.span_start, placed.call.site.span_end),
        args: None,
    };
    module.visit_with(&mut finder);
    let args = finder.args?;
    if args.iter().take(arg + 1).any(|arg| arg.spread.is_some()) {
        return None;
    }
    let written = args.get(arg)?;
    let span = match super::unwrap_value(&written.expr) {
        Expr::Arrow(arrow) => arrow.span,
        Expr::Fn(function) => function.function.span,
        Expr::Ident(name) => module_functions(&module)
            .get(name.sym.as_str())
            .copied()
            .unwrap_or(name.span),
        other => other.span(),
    };
    let name = match super::unwrap_value(&written.expr) {
        Expr::Ident(name) => Some(name.sym.to_string()),
        _ => None,
    };
    Some(HandlerAt {
        file: relative(root, placed.file),
        line: cm.lookup_char_pos(span.lo).line as u32,
        span_start: span.lo.0,
        span_end: span.hi.0,
        name,
    })
}

/// Finds the arguments of the call or construction at a span.
struct CallAt {
    target: (u32, u32),
    args: Option<Vec<ExprOrSpread>>,
}

impl Visit for CallAt {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        if (call.span.lo.0, call.span.hi.0) == self.target {
            self.args = Some(call.args.clone());
        }
        call.visit_children_with(self);
    }

    fn visit_new_expr(&mut self, new: &NewExpr) {
        if (new.span.lo.0, new.span.hi.0) == self.target {
            self.args = Some(new.args.clone().unwrap_or_default());
        }
        new.visit_children_with(self);
    }
}

/// `file` as the record names it: relative to `root`, `/`-separated.
pub(crate) fn relative(root: &Path, file: &Path) -> String {
    let relative = file.strip_prefix(root).unwrap_or(file);
    relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

struct Walk<'a, 'i> {
    calls: &'i Calls<'a>,
    inputs: &'i RequestSummaryInputs,
    root: &'i Path,
    /// With the receiver trace on, the makers the store labels: each site's
    /// instance is walked to them ([`instances`]).
    makers: Option<&'i [MakerLabel]>,
    /// With the receiver trace on, the scope members the store labels
    /// (carrick#2474 R2b).
    scopes: &'i [ScopeLabel],
    /// With the receiver trace on, what every instance walk of the service
    /// reads, built once.
    index: Option<&'i instances::WalkIndex<'a>>,
    out: &'i mut Decided,
    seen: HashSet<(String, u32, Option<String>, String)>,
    /// Where in `out.sites` the site of each id in `seen` was written.
    kept: HashMap<(String, u32, Option<String>, String), usize>,
    /// The ids two walks reached with different operations (carrick#2816):
    /// the call picks its operation when it runs, so its site is dropped
    /// when the walk ends ([`Walk::drop_withdrawn`]) and it is a lead.
    withdrawn: HashSet<(String, u32, Option<String>, String)>,
    /// The label of the labelled call being walked.
    label: Option<&'i SiteLabel>,
    /// The receiver of the labelled call being walked, and how discovery
    /// reached it.
    receiver: Option<super::discovery::ReceiverAt>,
    reached: Option<super::discovery::ReachedBy>,
}

impl<'a> Walk<'a, '_> {
    /// `down` is the calls from the site (first) to the labelled call
    /// (last).
    fn site(
        &mut self,
        down: &[Placed<'a>],
        end: &LabelledEnd,
        name: Vec<Piece>,
        name_args: BTreeSet<usize>,
    ) {
        let via = via_of(self.root, down);
        let placed = &down[0];
        // A site is a call of this service's own files (carrick#2618).
        if !self.calls.is_own(placed.file) {
            return;
        }
        let file = relative(self.root, placed.file);
        let id = (file.clone(), placed.call.site.span_start, None, end.at());
        let method = self
            .label
            .and_then(|label| label.method.as_ref())
            .and_then(|method| method.from.as_ref())
            .and_then(|slot| method_value(down, slot));
        if !self.seen.insert(id.clone()) {
            // Another walk reached this call and labelled end. The same
            // operation (a diamond) is one site; a different one means the
            // call chooses at run time, so it states none (carrick#2816).
            if !self.withdrawn.contains(&id)
                && let Some(&at) = self.kept.get(&id)
                && (self.out.sites[at].name != name || self.out.sites[at].method != method)
            {
                self.withdrawn.insert(id);
                self.lead(placed, end, Undecided::SeveralOperations);
            }
            return;
        }
        // A call into a sibling inside a client wrapper is a site like any
        // other: whether a caller of the wrapper states the request instead
        // is known only once call-through has stated that caller's row, at
        // the join point, which withdraws this one then (carrick#2618,
        // `call_through::state_leaves`).
        let label = self.label;
        if let Some(text) = literal_name(&name)
            && label.is_some_and(|label| label.reserved.contains(&text))
        {
            self.out.reserved.push(end.at());
            return;
        }
        let handler_slot = label.and_then(|label| label.handler.as_ref());
        let payload = match label.and_then(|label| label.payload.as_ref()) {
            Some(slot) => payload_binding(self.root, down, slot),
            None => handler_slot.and_then(|slot| payload_binding(self.root, down, slot)),
        };
        let payload_slot = label
            .and_then(|label| label.payload.as_ref())
            .and_then(|slot| payload_slot(down, slot));
        let handler = handler_slot
            .and_then(|slot| bound_at(down, slot))
            .and_then(|(at, arg, _)| handler_at(self.root, &down[at], arg));
        let instances = self.instances(down);
        self.kept.insert(id, self.out.sites.len());
        self.out.sites.push(DecidedSite {
            file,
            line: placed.call.site.line,
            span_start: placed.call.site.span_start,
            span_end: placed.call.site.span_end,
            end: end.clone(),
            name,
            key: None,
            name_args,
            via,
            payload,
            payload_slot,
            handler,
            method,
            receiver: self.receiver.clone(),
            reached: self.reached,
            instances,
            composed: Vec::new(),
            composed_trimmed: false,
            procedure: None,
        });
    }

    /// With the receiver trace on, the instance ends of the site whose
    /// calls are `down` ([`instances`]); none with it off.
    fn instances(&self, down: &[Placed<'a>]) -> Vec<InstanceEnd> {
        match (self.makers, self.index) {
            (Some(makers), Some(index)) => instances::InstanceWalk::new(
                self.calls,
                self.inputs,
                self.root,
                makers,
                self.scopes,
                index,
            )
            .of(down),
            _ => Vec::new(),
        }
    }

    fn lead(&mut self, placed: &Placed<'a>, end: &LabelledEnd, why: Undecided) {
        // A sibling's own call is a lead in that sibling's scan (carrick#2618).
        if !self.calls.is_own(placed.file) {
            return;
        }
        let lead = DecidedLead {
            file: relative(self.root, placed.file),
            line: placed.call.site.line,
            span_start: placed.call.site.span_start,
            span_end: placed.call.site.span_end,
            depth: u32::from(placed.depth),
            end: end.clone(),
            reason: why,
        };
        if !self.out.leads.contains(&lead) {
            self.out.leads.push(lead);
        }
    }

    /// Drops the sites two walks disagreed on ([`Walk::withdrawn`]).
    fn drop_withdrawn(&mut self) {
        let withdrawn = &self.withdrawn;
        self.out.sites.retain(|site| {
            !withdrawn.contains(&(site.file.clone(), site.span_start, None, site.end.at()))
        });
    }

    /// A call on an export the store labels with no op: a lead of its own
    /// reason, at the labelled call.
    fn no_op_claim(&mut self, end: &LabelledEnd) {
        let lead = DecidedLead {
            file: end.file.clone(),
            line: end.line,
            span_start: end.span_start,
            span_end: end.span_end,
            depth: 0,
            end: end.clone(),
            reason: Undecided::NoOpClaim,
        };
        if !self.out.leads.contains(&lead) {
            self.out.leads.push(lead);
        }
    }

    /// Whether the call `placed`, whose slot value is `pieces`, is the site
    /// ([`decides`]), unless a parameter of its function fills a path
    /// segment and every caller passes that parameter text the source writes
    /// whole: then each caller states the segment, and the callers are the
    /// sites (one per caller, with its literal). A parameter some caller
    /// fills with a value the code does not write (an id from data) stays a
    /// path parameter at this call.
    fn settles(&self, placed: &Placed<'a>, pieces: &[Piece], end: &LabelledEnd) -> bool {
        decides(pieces, end)
            && !(holds_parameter(pieces) && self.literal_everywhere(placed, pieces))
    }

    /// Whether some parameter in the route `pieces` states is passed literal
    /// text by every caller of the function `placed` is written in (and it
    /// has callers).
    fn literal_everywhere(&self, placed: &Placed<'a>, pieces: &[Piece]) -> bool {
        let (Some(own), Some(key)) = (to_definition(pieces, placed.depth), placed.key) else {
            return false;
        };
        let callers = self.calls.callers_of(placed.file, key);
        if callers.is_empty() {
            return false;
        }
        let literal = |caller: &Placed<'_>, index: usize| {
            matches!(argument(caller.call, index), Some(Value::Str(pieces))
                if !pieces.is_empty() && pieces.iter().all(|piece| matches!(piece, Piece::Lit(_))))
        };
        // Only a parameter in the route: one in a query cut from it
        // (`?page=${page}`) names no route, whoever fills it.
        super::route_of(&own)
            .iter()
            .filter_map(|piece| match piece {
                Piece::Param(index, _) => Some(*index),
                _ => None,
            })
            .any(|index| callers.iter().all(|caller| literal(caller, index)))
    }

    /// Walk up the call graph from `down[0]`, whose slot value `pieces` (as
    /// its own body reads it) is still open ([`decides`]): each caller of its
    /// definition fills it, and the lowest caller whose value decides it is
    /// the site. `path` holds the definitions already passed: a cycle ends.
    fn up(
        &mut self,
        down: &[Placed<'a>],
        pieces: &[Piece],
        end: &LabelledEnd,
        path: &mut Vec<(PathBuf, String)>,
    ) {
        let placed = down[0];
        let Some(own) = to_definition(pieces, placed.depth) else {
            self.lead(&placed, end, Undecided::CallbackParameter);
            return;
        };
        let Some(key) = placed.key else {
            // Module level holds no parameter of its own.
            self.lead(&placed, end, Undecided::NameNeverBound);
            return;
        };
        let callers = self.calls.callers_of(placed.file, key);
        if callers.is_empty() {
            self.lead(&placed, end, Undecided::NameNeverBound);
            return;
        }
        let here = (placed.file.to_path_buf(), key.to_string());
        if path.contains(&here) {
            return;
        }
        path.push(here);
        for caller in callers {
            let value = filled(&own, caller.call);
            let mut longer = vec![*caller];
            longer.extend_from_slice(down);
            // The boundary rule (carrick#2618): a sibling's call never states
            // the request, so the walk goes on to its callers.
            if self.calls.is_own(caller.file) && self.settles(caller, &value, end) {
                self.site(&longer, end, value, parameter_positions(&own));
            } else {
                self.up(&longer, &value, end, path);
            }
        }
        path.pop();
    }

    /// One site per key of the handler table the labelled call is handed in
    /// a loop, at the line of the function under each key.
    fn table(&mut self, placed: &Placed<'a>, end: &LabelledEnd, keys: Vec<TableKey>) {
        let file = relative(self.root, placed.file);
        let instances = self.instances(std::slice::from_ref(placed));
        for entry in keys {
            if self
                .label
                .is_some_and(|label| label.reserved.contains(&entry.key))
            {
                self.out.reserved.push(end.at());
                continue;
            }
            let id = (
                file.clone(),
                entry.handler.line,
                Some(entry.key.clone()),
                end.at(),
            );
            if !self.seen.insert(id) {
                continue;
            }
            self.out.sites.push(DecidedSite {
                file: file.clone(),
                line: entry.handler.line,
                span_start: placed.call.site.span_start,
                span_end: placed.call.site.span_end,
                end: end.clone(),
                name: vec![Piece::Lit(entry.key.clone())],
                key: Some(entry.key),
                name_args: BTreeSet::new(),
                via: Vec::new(),
                payload: None,
                payload_slot: None,
                handler: Some(entry.handler),
                method: None,
                receiver: self.receiver.clone(),
                reached: self.reached,
                instances: instances.clone(),
                composed: Vec::new(),
                composed_trimmed: false,
                procedure: None,
            });
        }
    }
}

/// The name `pieces` spell when the code writes all of it as text.
fn literal_name(pieces: &[Piece]) -> Option<String> {
    let mut text = String::new();
    for piece in pieces {
        match piece {
            Piece::Lit(part) => text.push_str(part),
            _ => return None,
        }
    }
    (!text.is_empty()).then_some(text)
}

/// Whether the labelled call `placed` supplies the slot `slot`: the argument
/// is written, and, for an option key, an object written at the call holds
/// the key. A spread before it, or a value the scan cannot read, supplies
/// nothing it can state.
fn supplies(call: &CallIr, slot: &ArgSlot) -> bool {
    match (argument(call, slot.arg), &slot.key) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(Value::Obj(object)), Some(key)) => object.fields.contains_key(key),
        (Some(_), Some(_)) => false,
    }
}

/// Each call below the first in `down`: its `file:line` and its callee as
/// written.
fn via_of(root: &Path, down: &[Placed<'_>]) -> Vec<Via> {
    down.iter()
        .skip(1)
        .map(|placed| Via {
            at: format!("{}:{}", relative(root, placed.file), placed.call.site.line),
            callee: placed.call.callee_text.clone(),
        })
        .collect()
}

/// Every call in `discovered` where its name is decided, and every one
/// whose name no call of this code decides, read with the labels the store
/// answers (`labels`). A labelled call no label answers is skipped, unless
/// its export has no op label at all: that is a lead (carrick#2515). Files
/// are named relative to `root`, the scanned repo's root.
pub fn decided_sites(
    inputs: &RequestSummaryInputs,
    root: &Path,
    discovered: &[DiscoverySite],
    labels: &[SiteLabel],
) -> Decided {
    decide(
        inputs,
        &[],
        root,
        discovered,
        labels,
        None,
        &[],
        &ProcedureLabels::default(),
    )
}

/// [`decided_sites`], with the run's other services `siblings` in declared
/// order (carrick#2618): a chain discovery followed into a sibling's files is
/// placed in them, and the request is stated at the call of this service's
/// own files that enters the sibling, never at a sibling's call.
pub fn decided_sites_with(
    inputs: &RequestSummaryInputs,
    siblings: &[&RequestSummaryInputs],
    root: &Path,
    discovered: &[DiscoverySite],
    labels: &[SiteLabel],
) -> Decided {
    decide(
        inputs,
        siblings,
        root,
        discovered,
        labels,
        None,
        &[],
        &ProcedureLabels::default(),
    )
}

/// [`decided_sites`] with the receiver trace on (carrick#2474 R2a): each
/// site also names the instance its labelled call is made on, walked to the
/// makers the store labels (`makers`), one end per caller that hands one in
/// ([`DecidedSite::instances`]), through the scopes the store's scope
/// members open (`scopes`), and the full path it is served at under them
/// and the mounts ([`DecidedSite::composed`], R2b), and each procedure op's
/// site the paths its router is served at (`procedures`, R6c).
pub fn decided_sites_traced(
    inputs: &RequestSummaryInputs,
    root: &Path,
    discovered: &[DiscoverySite],
    labels: &[SiteLabel],
    makers: &[MakerLabel],
    scopes: &[ScopeLabel],
    procedures: &ProcedureLabels,
) -> Decided {
    decide(
        inputs,
        &[],
        root,
        discovered,
        labels,
        Some(makers),
        scopes,
        procedures,
    )
}

/// [`decided_sites_traced`] with the run's other services `siblings`
/// ([`decided_sites_with`]).
#[allow(clippy::too_many_arguments)]
pub fn decided_sites_traced_with(
    inputs: &RequestSummaryInputs,
    siblings: &[&RequestSummaryInputs],
    root: &Path,
    discovered: &[DiscoverySite],
    labels: &[SiteLabel],
    makers: &[MakerLabel],
    scopes: &[ScopeLabel],
    procedures: &ProcedureLabels,
) -> Decided {
    decide(
        inputs,
        siblings,
        root,
        discovered,
        labels,
        Some(makers),
        scopes,
        procedures,
    )
}

#[allow(clippy::too_many_arguments)]
fn decide(
    inputs: &RequestSummaryInputs,
    siblings: &[&RequestSummaryInputs],
    root: &Path,
    discovered: &[DiscoverySite],
    labels: &[SiteLabel],
    makers: Option<&[MakerLabel]>,
    scopes: &[ScopeLabel],
    procedures: &ProcedureLabels,
) -> Decided {
    // The files the discovered chains enter or end in: the siblings that
    // hold one are indexed beside this service (carrick#2618).
    let entered_files: HashSet<PathBuf> = discovered
        .iter()
        .flat_map(|site| {
            site.chain
                .iter()
                .map(|hop| root.join(&hop.file))
                .chain(site.end.iter().map(|end| root.join(&end.file)))
        })
        .collect();
    let calls = if siblings.is_empty() {
        Calls::index(inputs)
    } else {
        Calls::index_with(inputs, siblings, &entered_files)
    };
    let index = makers.map(|_| instances::WalkIndex::of(&calls, inputs, scopes));
    let mut out = Decided::default();
    // The functions some listed site's chain enters: a site whose value
    // still holds a parameter is decided by those callers, which discovery
    // lists on their own.
    let entered: HashSet<(PathBuf, String)> = discovered
        .iter()
        .filter_map(|site| site.chain.first())
        .map(|hop| (root.join(&hop.file), hop.function.clone()))
        .collect();
    let mut tables: HashMap<(PathBuf, u32), Option<Vec<TableKey>>> = HashMap::new();
    let mut walked: Vec<String> = Vec::new();
    let mut unplaced: Vec<String> = Vec::new();
    let mut walk = Walk {
        calls: &calls,
        inputs,
        root,
        makers,
        scopes,
        index: index.as_ref(),
        out: &mut out,
        seen: HashSet::new(),
        kept: HashMap::new(),
        withdrawn: HashSet::new(),
        label: None,
        receiver: None,
        reached: None,
    };
    for site in discovered {
        let Some(end) = &site.end else {
            continue;
        };
        walk.receiver = site.receiver.clone();
        walk.reached = site.reached;
        // An end discovery keeps with a stop is one of the labelled exports
        // a callee's disagreeing values reach (carrick#2432): the call is a
        // lead, named by none of them.
        if site.stop.is_some() {
            if let Some(placed) =
                calls.at_span(&root.join(&site.file), site.span_start, site.span_end)
            {
                walk.lead(&placed, end, Undecided::SeveralValues);
            }
            continue;
        }
        let Some(label) = SiteLabel::find(labels, end) else {
            // A call that hands a maker the function it runs
            // (carrick#2426): the function holds the request, found at its
            // own site. A call that does not supply it stays a lead.
            if let Some(slot) = SiteLabel::runner(labels, end) {
                match calls.at_span(&root.join(&end.file), end.span_start, end.span_end) {
                    Some(placed) if supplies(placed.call, slot) => walk.out.ran.push(end.at()),
                    _ => walk.no_op_claim(end),
                }
                continue;
            }
            // An export the store labels with a role and no op at all
            // (carrick#2515): the call is a lead. An export with ops, and a
            // member of it none labels, is not.
            if !labels.iter().any(|label| {
                label.op.is_some() && label.package == end.package && label.export == end.export
            }) {
                walk.no_op_claim(end);
            }
            continue;
        };
        walk.label = Some(label);
        let Some(last) = calls.at_span(&root.join(&end.file), end.span_start, end.span_end) else {
            unplaced.push(format!("{}:{}", site.file, site.line));
            continue;
        };
        // A call above the labelled call, when that call is the site.
        let through_to_last = |walk: &mut Walk<'_, '_>| {
            if let Some(chain) = place_chain(&calls, root, site, &last)
                && let Some(through) = through(inputs, root, &chain, chain.len() - 1)
            {
                walk.out.through.push(through);
            }
        };
        // The boundary rule (carrick#2618): a labelled call in a sibling's
        // file is never the site. The request is stated at the call of this
        // service's files that enters the sibling.
        let last_is_own = calls.is_own(last.file);
        let Some(slot) = &label.name else {
            // The label names no slot: the labelled call is the site, or for
            // a sibling's call the call of this service that enters it.
            if last_is_own {
                walk.site(&[last], end, Vec::new(), BTreeSet::new());
                through_to_last(&mut walk);
            } else if let Some(chain) = place_chain(&calls, root, site, &last) {
                walk.site(&chain, end, Vec::new(), BTreeSet::new());
            } else {
                unplaced.push(format!("{}:{}", site.file, site.line));
            }
            continue;
        };
        let table = tables
            .entry((root.join(&end.file), end.span_start))
            .or_insert_with(|| loop_table(&root.join(&end.file), end, slot.arg))
            .clone();
        if let Some(keys) = table {
            // A handler table a sibling registers is stated in the
            // sibling's own scan.
            if last_is_own {
                walk.table(&last, end, keys);
            }
            continue;
        }
        let at_end = slot_value(last.call, slot);
        if last_is_own && walk.settles(&last, &at_end, end) {
            walk.site(&[last], end, at_end, BTreeSet::from([slot.arg]));
            through_to_last(&mut walk);
            continue;
        }
        // The site's call runs, as a value, the function that makes the
        // labelled call (carrick#2432): its arguments fill that function's
        // own parameters, which no call-graph caller does.
        if let [hop] = site.chain.as_slice()
            && hop.function == VALUE_HOP
            && !at_end.iter().any(|piece| matches!(piece, Piece::Outer(..)))
            && let Some(first) =
                calls.at_span(&root.join(&site.file), site.span_start, site.span_end)
        {
            let value = filled(&at_end, first.call);
            let down = [first, last];
            if walk.settles(&first, &value, end) {
                walk.site(&down, end, value, parameter_positions(&at_end));
            } else {
                walked.push(format!("{}:{}", site.file, site.line));
                walk.up(&down, &value, end, &mut Vec::new());
            }
            continue;
        }
        let Some(chain) = place_chain(&calls, root, site, &last) else {
            walked.push(format!("{}:{}", site.file, site.line));
            walk.up(&[last], &at_end, end, &mut Vec::new());
            continue;
        };
        // Compose up the chain: values[k] is the slot at chain[k], as the
        // body that writes chain[k] reads it.
        let n = chain.len();
        let mut values: Vec<Option<Vec<Piece>>> = vec![None; n];
        values[n - 1] = Some(at_end);
        let mut args: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
        args[n - 1] = BTreeSet::from([slot.arg]);
        for k in (0..n - 1).rev() {
            let below = &chain[k + 1];
            values[k] = values[k + 1]
                .as_ref()
                .and_then(|pieces| to_definition(pieces, below.depth))
                .map(|own| {
                    args[k] = parameter_positions(&own);
                    filled(&own, chain[k].call)
                });
        }
        let Some(top) = values[0].clone() else {
            walk.lead(&chain[0], end, Undecided::CallbackParameter);
            continue;
        };
        if walk.settles(&chain[0], &top, end) {
            // Only a call of this service's files can be the site lower down
            // (carrick#2618): a sibling's call that settles is not one.
            let below_open = n == 1
                || !calls.is_own(chain[1].file)
                || values[1]
                    .as_ref()
                    .is_none_or(|pieces| !walk.settles(&chain[1], pieces, end));
            if below_open {
                walk.site(&chain, end, top, args[0].clone());
            } else {
                // The site is the lowest call whose value still settles.
                let to = (1..n)
                    .take_while(|&k| {
                        calls.is_own(chain[k].file)
                            && values[k]
                                .as_ref()
                                .is_some_and(|pieces| walk.settles(&chain[k], pieces, end))
                    })
                    .last()
                    .unwrap_or(0);
                if let Some(through) = through(inputs, root, &chain, to) {
                    walk.out.through.push(through);
                }
            }
            continue;
        }
        let covered = chain[0]
            .key
            .filter(|_| chain[0].depth == 0)
            .is_some_and(|key| entered.contains(&(chain[0].file.to_path_buf(), key.to_string())));
        if !covered {
            walked.push(format!("{}:{}", site.file, site.line));
            walk.up(&chain, &top, end, &mut Vec::new());
        }
    }
    walk.drop_withdrawn();
    out.through
        .extend(callers_above(inputs, root, &calls, &out.sites));
    let mounts = match (makers, &index) {
        (Some(makers), Some(index)) => {
            let mounts = composed::mount_edges(&calls, inputs, root, makers, scopes, index);
            for site in &mut out.sites {
                (site.composed, site.composed_trimmed) =
                    composed::compose(&site.instances, site.name_text().as_deref(), &mounts);
            }
            procedures::Pass {
                calls: &calls,
                inputs,
                root,
                makers,
                scopes,
                index,
                mounts: &mounts,
                labels: procedures,
                site_labels: labels,
            }
            .run(&mut out, discovered);
            out.test_only_mounts = mounts.test_only.clone();
            mounts
        }
        _ => composed::Mounts::default(),
    };
    walked.sort();
    walked.dedup();
    out.walked = walked;
    out.unplaced = unplaced;
    out.ran.sort();
    out.reserved.sort();
    out.through.sort();
    out.through.dedup();
    out.sites.sort_by(|a, b| {
        (&a.file, a.line, &a.key, &a.end.file, a.end.line).cmp(&(
            &b.file,
            b.line,
            &b.key,
            &b.end.file,
            b.end.line,
        ))
    });
    out.leads
        .sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    if let (Some(makers), Some(index)) = (makers, &index) {
        out.boundaries = boundaries::Collect {
            calls: &calls,
            inputs,
            root,
            makers,
            scopes,
            index,
            mounts: &mounts,
        }
        .of(&out, labels);
    }
    out
}

/// Where the request summaries state each request's rows
/// ([`super::RequestSummaryIndex::placements`]): the request's file and span
/// start to the file and line of every row stated for it.
pub type SummaryPlacements = HashMap<(PathBuf, u32), HashSet<(PathBuf, u32)>>;

/// The base an instance's maker was handed, read through the maker's claim
/// ([`Decided::carry_instance_bases`]).
#[derive(Debug, PartialEq, Eq)]
enum MadeBase {
    /// No maker's claim names a base slot, or no maker was handed one.
    None,
    /// Every maker was handed this literal base.
    Text(String),
    /// A maker was handed a base the code does not state as text, the
    /// makers disagree, or the instance is used in a way that may change it.
    Unstated,
}

/// The base the instance `site` is called through was made with, as the
/// claims of its export say a maker's base is read (`create({ baseURL })`).
fn made_base(
    site: &super::LibrarySite,
    exports: &[crate::library_claims::ExportClaims],
) -> MadeBase {
    use crate::services::type_sidecar::{LibraryClaim, MakeForm};
    let super::SiteReceiver::Instance(makers) = &site.receiver else {
        return MadeBase::None;
    };
    let Some(export) = exports
        .iter()
        .find(|export| export.package == site.package() && export.export == site.export)
    else {
        return MadeBase::None;
    };
    let mut bases: BTreeSet<Option<&str>> = BTreeSet::new();
    for maker in makers {
        let slot = export.claims.iter().find_map(|claim| match claim {
            LibraryClaim::Make {
                form, member, base, ..
            } if *member == maker.member
                && matches!(
                    (form, maker.form),
                    (MakeForm::Call, super::MakerForm::Call)
                        | (MakeForm::New, super::MakerForm::New)
                ) =>
            {
                base.as_ref()
            }
            _ => None,
        });
        let base = match slot {
            Some(slot) if maker.supplies(slot.arg as usize, slot.key.as_deref()) => {
                match maker.literal(slot.arg as usize, slot.key.as_deref()) {
                    Some(text) => Some(text),
                    None => return MadeBase::Unstated,
                }
            }
            _ => None,
        };
        bases.insert(base);
    }
    match bases.into_iter().collect::<Vec<_>>().as_slice() {
        [None] | [] => MadeBase::None,
        [Some(_)] if site.contested => MadeBase::Unstated,
        [Some(text)] => MadeBase::Text((*text).to_string()),
        _ => MadeBase::Unstated,
    }
}

impl Decided {
    /// One request, one row (carrick#2524): an HTTP site whose request the
    /// summaries already state a row for stands only where one of those
    /// rows stands. Placement follows the summaries, which put a request's
    /// row where its last hole is filled; a site anywhere else would be a
    /// second key for the same request. Returns how many sites yielded.
    pub fn yield_to_summaries(&mut self, root: &Path, placed: &SummaryPlacements) -> usize {
        let placed: HashMap<(String, u32), HashSet<(String, u32)>> = placed
            .iter()
            .map(|((file, span), rows)| {
                (
                    (relative(root, file), *span),
                    rows.iter()
                        .map(|(file, line)| (relative(root, file), *line))
                        .collect(),
                )
            })
            .collect();
        let before = self.sites.len();
        self.sites.retain(|site| {
            site.end.role != "http_client"
                || placed
                    .get(&(site.end.file.clone(), site.end.span_start))
                    .is_none_or(|rows| rows.contains(&(site.file.clone(), site.line)))
        });
        before - self.sites.len()
    }

    /// The base an HTTP request's instance carries rides onto its site's
    /// path (carrick#2524): a call through an instance whose maker the
    /// claims say reads a base (`create({ baseURL: "https://…/v1" })`) goes
    /// to that base, so the base is joined to the path as the summaries join
    /// one. A base the code does not state as text, or one the path cannot
    /// be written on, leaves the site out: a host-free path would read as a
    /// call into this system. Returns how many sites were left out.
    pub fn carry_instance_bases(
        &mut self,
        root: &Path,
        library: &super::LibrarySites,
        exports: &[crate::library_claims::ExportClaims],
    ) -> usize {
        let at: HashMap<(String, u32, u32), &super::LibrarySite> = library
            .sites
            .iter()
            .filter(|site| site.origin.is_none())
            .map(|site| {
                (
                    (relative(root, &site.file), site.span_start, site.span_end),
                    site,
                )
            })
            .collect();
        let before = self.sites.len();
        self.sites.retain_mut(|site| {
            if site.end.role != "http_client" || site.key.is_some() || site.name.is_empty() {
                return true;
            }
            let Some(called) = at.get(&(
                site.end.file.clone(),
                site.end.span_start,
                site.end.span_end,
            )) else {
                return true;
            };
            match made_base(called, exports) {
                MadeBase::None => true,
                MadeBase::Unstated => false,
                MadeBase::Text(base) => {
                    site.name = super::join_base(&[Piece::Lit(base)], &site.name);
                    site.name_text().is_some()
                }
            }
        });
        before - self.sites.len()
    }
}

/// The calls a discovery site's chain names: the site's own first, then in
/// each function the chain enters the one call that calls the next, and
/// the labelled call last. `None` where a hop is a callback (no call of the
/// service enters it), or a body calls the next function more than once or
/// not at all.
fn place_chain<'a>(
    calls: &Calls<'a>,
    root: &Path,
    site: &DiscoverySite,
    last: &Placed<'a>,
) -> Option<Vec<Placed<'a>>> {
    let first = calls.at_span(&root.join(&site.file), site.span_start, site.span_end)?;
    if site.chain.is_empty() {
        return (first.call.site == last.call.site).then(|| vec![first]);
    }
    if site.chain.iter().any(|hop| hop.function.starts_with('<')) {
        return None;
    }
    // Discovery follows calls the call graph may not resolve (a member of a
    // call's result, through the compiler's rows): a call it leaves
    // unresolved enters the hop only when it calls the hop's function by its
    // own name (`openSession(url).notify` for `Session.notify`), so a log
    // line or a global function in the body is never the hop. A call it
    // resolves elsewhere is not this hop.
    let enters = |placed: &Placed<'a>, hop: &ChainHop| match calls.target(placed) {
        Some((file, key)) => file == &root.join(&hop.file) && key == &hop.function,
        None => called_name(&placed.call.callee_text) == called_name(&hop.function),
    };
    if !enters(&first, &site.chain[0]) {
        return None;
    }
    let mut chain = vec![first];
    for pair in site.chain.windows(2) {
        let (from, to) = (&pair[0], &pair[1]);
        let file = root.join(&from.file);
        let inside = calls
            .by_owner
            .get(&(file.as_path(), from.function.as_str()))?;
        let mut next = inside.iter().filter(|placed| enters(placed, to));
        let found = *next.next()?;
        if next.next().is_some() {
            return None;
        }
        chain.push(found);
    }
    let owner = site.chain.last()?;
    if last.key != Some(owner.function.as_str()) || last.file != root.join(&owner.file) {
        return None;
    }
    chain.push(*last);
    Some(chain)
}

/// The name a call reaches its function by, or a definition key names its
/// function by: the last `.` segment (`notify` for
/// `openSession(url).notify` and for `Session.notify`).
fn called_name(text: &str) -> &str {
    text.rsplit('.').next().unwrap_or(text)
}

/// One key of a handler table.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TableKey {
    key: String,
    handler: HandlerAt,
}

/// The keys of the handler table the labelled call at `end` registers in a
/// loop, when its name argument (`arg`) is the key a `for … of` over
/// `Object.entries(T)` or `Object.keys(T)` binds and `T` is a module-scope
/// `const` object literal whose every key is written: each key with the
/// function that handles it (a function the module declares under the
/// value's name, or one written as the value), or the key's own line.
/// `None` for anything else.
fn loop_table(file: &Path, end: &LabelledEnd, arg: usize) -> Option<Vec<TableKey>> {
    let cm: Lrc<SourceMap> = Default::default();
    let handler = swc_common::errors::Handler::with_tty_emitter(
        swc_common::errors::ColorConfig::Never,
        false,
        false,
        Some(cm.clone()),
    );
    let module = crate::parser::parse_file(file, &cm, &handler)?;
    let mut finder = LoopFinder {
        target: (end.span_start, end.span_end),
        arg,
        loops: Vec::new(),
        found: None,
    };
    module.visit_with(&mut finder);
    let table_name = finder.found?;
    let functions = module_functions(&module);
    let mut table: Option<&ObjectLit> = None;
    for item in &module.body {
        let decl = match item {
            ModuleItem::Stmt(Stmt::Decl(decl)) => decl,
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => &export.decl,
            _ => continue,
        };
        if let Decl::Var(var) = decl
            && var.kind == VarDeclKind::Const
        {
            for declarator in &var.decls {
                if let Pat::Ident(name) = &declarator.name
                    && name.id.sym == *table_name
                    && let Some(Expr::Object(object)) =
                        declarator.init.as_deref().map(super::unwrap_value)
                {
                    table = Some(object);
                }
            }
        }
    }
    let line = |span: Span| cm.lookup_char_pos(span.lo).line as u32;
    let at = |span: Span| HandlerAt {
        file: end.file.clone(),
        line: line(span),
        span_start: span.lo.0,
        span_end: span.hi.0,
        name: None,
    };
    let mut keys = Vec::new();
    for prop in &table?.props {
        let PropOrSpread::Prop(prop) = prop else {
            return None;
        };
        let Prop::KeyValue(entry) = &**prop else {
            return None;
        };
        let key = super::prop_name(&entry.key)?;
        let handler = match super::unwrap_value(&entry.value) {
            Expr::Ident(name) => functions
                .get(name.sym.as_str())
                .copied()
                .map_or_else(|| at(entry.span()), at),
            Expr::Arrow(arrow) => at(arrow.span),
            Expr::Fn(function) => at(function.function.span),
            _ => at(entry.span()),
        };
        keys.push(TableKey { key, handler });
    }
    (!keys.is_empty()).then_some(keys)
}

/// Finds the labelled call and the loop binding its name argument reads.
struct LoopFinder {
    target: (u32, u32),
    arg: usize,
    /// Each enclosing `for … of Object.entries(T)`: the name its key is
    /// bound to, and `T`.
    loops: Vec<(String, String)>,
    found: Option<String>,
}

impl Visit for LoopFinder {
    fn visit_for_of_stmt(&mut self, stmt: &ForOfStmt) {
        let bound = loop_key(stmt);
        if let Some(bound) = &bound {
            self.loops.push(bound.clone());
        }
        stmt.visit_children_with(self);
        if bound.is_some() {
            self.loops.pop();
        }
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        if (call.span.lo.0, call.span.hi.0) == self.target
            && let Some(Expr::Ident(name)) = call.args.get(self.arg).map(|arg| &*arg.expr)
            && arg_unspread(call, self.arg)
            && let Some((_, table)) = self
                .loops
                .iter()
                .rev()
                .find(|(key, _)| name.sym == *key.as_str())
        {
            self.found = Some(table.clone());
        }
        call.visit_children_with(self);
    }
}

fn arg_unspread(call: &CallExpr, upto: usize) -> bool {
    call.args
        .iter()
        .take(upto + 1)
        .all(|arg| arg.spread.is_none())
}

/// The key name and the table a `for (const [k, …] of Object.entries(T))`
/// or `for (const k of Object.keys(T))` binds.
fn loop_key(stmt: &ForOfStmt) -> Option<(String, String)> {
    let ForHead::VarDecl(var) = &stmt.left else {
        return None;
    };
    if var.kind != VarDeclKind::Const || var.decls.len() != 1 {
        return None;
    }
    let Expr::Call(call) = super::unwrap_value(&stmt.right) else {
        return None;
    };
    let Callee::Expr(callee) = &call.callee else {
        return None;
    };
    let Expr::Member(member) = &**callee else {
        return None;
    };
    let (Expr::Ident(object), MemberProp::Ident(method)) = (&*member.obj, &member.prop) else {
        return None;
    };
    if object.sym != *"Object" || call.args.len() != 1 {
        return None;
    }
    let Expr::Ident(table) = super::unwrap_value(&call.args[0].expr) else {
        return None;
    };
    let key = match (&var.decls[0].name, method.sym.as_str()) {
        (Pat::Array(array), "entries") => match array.elems.first() {
            Some(Some(Pat::Ident(key))) => key.id.sym.to_string(),
            _ => return None,
        },
        (Pat::Ident(key), "keys") => key.id.sym.to_string(),
        _ => return None,
    };
    Some((key, table.sym.to_string()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// carrick#2439: the scan's settled claims give one label per op, with
    /// its slots, method fields, role and side, as the store's raw answer
    /// does; a maker labels no op, and a bound name no name slot.
    #[test]
    fn labels_read_from_the_settled_claims() {
        let exports: Vec<crate::library_claims::ExportClaims> =
            serde_json::from_value(serde_json::json!([{
                "package": "@fixture/http",
                "version": "1.0.0",
                "specifier": "@fixture/http",
                "export": "createClient",
                "role": "http_client",
                "side": "client",
                "claims": [
                    { "kind": "op", "op": "request", "member": "post", "name": { "arg": 0 },
                      "payload": { "arg": 1 }, "method": "POST" },
                    { "kind": "op", "op": "request", "member": "send", "name": { "bound": "maker" },
                      "method_key": { "arg": 1, "key": "method" }, "method_default": "GET" },
                    { "kind": "make", "form": "call", "member": null }
                ]
            }]))
            .unwrap();
        let labels = SiteLabel::from_exports(&exports);
        assert_eq!(
            labels,
            vec![
                SiteLabel {
                    package: "@fixture/http".into(),
                    export: "createClient".into(),
                    member: Some("post".into()),
                    role: "http_client".into(),
                    op: Some("request".into()),
                    side: Some("client".into()),
                    name: Some(ArgSlot { arg: 0, key: None }),
                    payload: Some(ArgSlot { arg: 1, key: None }),
                    handler: None,
                    reserved: Vec::new(),
                    runs: None,
                    request: None,
                    request_dropped: false,
                    method: Some(MethodLabel {
                        from: None,
                        default: None,
                        literal: Some("POST".into()),
                    }),
                },
                SiteLabel {
                    package: "@fixture/http".into(),
                    export: "createClient".into(),
                    member: Some("send".into()),
                    role: "http_client".into(),
                    op: Some("request".into()),
                    side: Some("client".into()),
                    name: None,
                    payload: None,
                    handler: None,
                    reserved: Vec::new(),
                    runs: None,
                    request: None,
                    request_dropped: false,
                    method: Some(MethodLabel {
                        from: Some(ArgSlot {
                            arg: 1,
                            key: Some("method".into()),
                        }),
                        default: Some("GET".into()),
                        literal: None,
                    }),
                },
            ]
        );
    }

    /// carrick#2674: a receive op's `request.body` member path is read from
    /// both the settled claims and the store's raw answer, and written back
    /// out; a request with no body path serialises as it did before.
    #[test]
    fn a_request_body_path_is_read_from_both_label_sources() {
        let receive = |request: serde_json::Value| {
            serde_json::json!({ "kind": "op", "op": "receive", "member": "post",
              "name": { "arg": 0 }, "handler": { "arg": 1 }, "request": request })
        };
        let exports: Vec<crate::library_claims::ExportClaims> =
            serde_json::from_value(serde_json::json!([{
                "package": "@fixture/server",
                "version": "1.0.0",
                "specifier": "@fixture/server",
                "export": "server",
                "role": "server_framework",
                "side": "server",
                "claims": [
                    receive(serde_json::json!({ "param": 0, "path": ["path"], "body": ["body"] })),
                    receive(serde_json::json!({ "param": 0, "path": ["path"] }))
                ]
            }]))
            .unwrap();
        let from_claims = SiteLabel::from_exports(&exports);
        let answer = serde_json::json!({ "library_claims": [{
            "package": "@fixture/server",
            "exports": [{
                "export": "server", "role": "server_framework", "side": "server",
                "ops": [
                    { "op": "receive", "member": "post", "name": { "arg": 0 }, "handler": { "arg": 1 },
                      "request": { "param": 0, "path": ["path"], "body": ["body"] } },
                    { "op": "receive", "member": "post", "name": { "arg": 0 }, "handler": { "arg": 1 },
                      "request": { "param": 0, "path": ["path"] } }
                ]
            }]
        }]});
        let from_answer = SiteLabel::from_store_answer(&answer);
        for labels in [&from_claims, &from_answer] {
            let bodies: Vec<_> = labels
                .iter()
                .map(|label| label.request.as_ref().unwrap().body.clone())
                .collect();
            assert_eq!(bodies, vec![Some(vec!["body".to_string()]), None]);
        }
        let written = serde_json::to_value(from_answer[0].request.as_ref().unwrap()).unwrap();
        assert_eq!(
            written,
            serde_json::json!({ "param": 0, "path": ["path"], "body": ["body"] })
        );
        let unchanged = serde_json::to_value(from_answer[1].request.as_ref().unwrap()).unwrap();
        assert_eq!(
            unchanged,
            serde_json::json!({ "param": 0, "path": ["path"] })
        );
    }

    /// carrick#2515: a call on an export the store labels `graphql_client`
    /// with no op is a lead of its own reason and no site; a member the
    /// store left unlabelled on an export that has ops is neither.
    #[test]
    fn a_call_on_an_export_with_no_op_is_a_lead_not_a_vanishing() {
        let root = decided_fixture();
        let inputs = crate::engine::discover_request_inputs(&root);
        let exports: Vec<crate::library_claims::ExportClaims> =
            serde_json::from_value(serde_json::json!([
                { "package": "gql-thing", "version": "1.0.0", "specifier": "gql-thing",
                  "export": "Client", "role": "graphql_client", "side": "client", "claims": [] },
                { "package": "bus-thing", "version": "1.0.0", "specifier": "bus-thing",
                  "export": "Bus", "role": "broker", "side": "client",
                  "claims": [{ "kind": "op", "op": "send", "member": "publish",
                               "name": { "arg": 0 } }] }
            ]))
            .unwrap();
        let labels = SiteLabel::from_exports(&exports);
        let end = |package: &str, export: &str, member: &str, role: &str, line: u32| LabelledEnd {
            package: package.into(),
            export: export.into(),
            member: Some(member.into()),
            role: role.into(),
            op: None,
            file: "src/gql.ts".into(),
            line,
            span_start: line * 100,
            new: false,
            span_end: line * 100 + 20,
        };
        let sites = [
            bottom(
                &end("gql-thing", "Client", "query", "graphql_client", 3),
                "client.query",
            ),
            bottom(
                &end("gql-thing", "Client", "query", "graphql_client", 3),
                "client.query",
            ),
            bottom(&end("bus-thing", "Bus", "close", "broker", 4), "bus.close"),
        ];
        let decided = decided_sites(&inputs, &root, &sites, &labels);
        assert!(decided.sites.is_empty(), "{:?}", decided.sites);
        assert_eq!(
            decided
                .leads
                .iter()
                .map(|lead| (lead.file.as_str(), lead.line, lead.reason))
                .collect::<Vec<_>>(),
            vec![("src/gql.ts", 3, Undecided::NoOpClaim)]
        );
        // A GraphQL client's operations are labelled by class only: the lead
        // is counted here and not listed as a possible call (carrick#2519
        // round 2); `possible_calls` lists other exports with no op.
        let found = crate::possible_calls::from_sites(&decided, &[]);
        assert!(found.is_empty(), "{found:?}");
        assert_eq!(
            serde_json::to_value(crate::possible_calls::UnreadReason::NoOpClaim).unwrap(),
            "no_op_claim"
        );
    }

    const STATED: &str = "src/slots/stated.ts";

    /// The end of the call `call` of `slots/stated.ts` on `export` of `package`.
    fn stated_end(
        text: &str,
        package: &str,
        export: &str,
        member: Option<&str>,
        role: &str,
        call: &str,
    ) -> LabelledEnd {
        let (span_start, span_end) = span_of(text, call);
        LabelledEnd {
            package: package.into(),
            export: export.into(),
            member: member.map(str::to_string),
            role: role.into(),
            op: None,
            file: STATED.into(),
            line: line_of(text, call),
            span_start,
            span_end,
            new: false,
        }
    }

    /// carrick#2426: a call that hands a maker the function it runs states
    /// no row and is no lead (the function holds the request, found at its
    /// own site); a call that hands none it can read stays a lead; a maker
    /// claimed for `new` does not speak for a plain call; an export the store
    /// holds no such claim for stays a lead.
    #[test]
    fn a_call_that_supplies_the_function_a_maker_runs_is_not_a_lead() {
        let root = decided_fixture();
        let inputs = crate::engine::discover_request_inputs(&root);
        let text = std::fs::read_to_string(root.join(STATED)).unwrap();
        let exports: Vec<crate::library_claims::ExportClaims> =
            serde_json::from_value(serde_json::json!([
                { "package": "cache-kit", "version": "1.0.0", "specifier": "cache-kit",
                  "export": "useCached", "role": "http_client", "label": "request_runner",
                  "claims": [{ "kind": "make", "form": "call", "member": null,
                               "runs": { "arg": 0, "key": "load" } }] },
                { "package": "cache-kit", "version": "1.0.0", "specifier": "cache-kit",
                  "export": "Loader", "role": "http_client", "label": "request_runner",
                  "claims": [{ "kind": "make", "form": "new", "member": null,
                               "runs": { "arg": 0, "key": "load" } }] }
            ]))
            .unwrap();
        let labels = SiteLabel::from_exports(&exports);
        let cached =
            |call: &str| stated_end(&text, "cache-kit", "useCached", None, "http_client", call);
        let fetch = {
            let label = fetch_label();
            bottom(
                &end_for(&label, STATED, &text, "fetch(\"/api/x\")"),
                "fetch",
            )
        };
        let sites = [
            bottom(
                &cached("useCached({ key: [\"x\"], load: () => fetch(\"/api/x\") })"),
                "useCached",
            ),
            bottom(&cached("useCached(options)"), "useCached"),
            bottom(&cached("useCached({ ...options })"), "useCached"),
            fetch,
        ];
        let mut all = labels.clone();
        all.push(fetch_label());
        let decided = decided_sites(&inputs, &root, &sites, &all);
        assert_eq!(
            decided
                .leads
                .iter()
                .map(|lead| (lead.line, lead.reason))
                .collect::<Vec<_>>(),
            vec![
                (line_of(&text, "useCached(options)"), Undecided::NoOpClaim),
                (
                    line_of(&text, "useCached({ ...options })"),
                    Undecided::NoOpClaim
                ),
            ]
        );
        assert_eq!(
            decided.ran,
            vec![format!("{STATED}:{}", line_of(&text, "useCached({ key"))]
        );
        assert_eq!(
            decided
                .sites
                .iter()
                .map(|site| (site.line, site.name_text()))
                .collect::<Vec<_>>(),
            vec![(
                line_of(&text, "fetch(\"/api/x\")"),
                Some("/api/x".to_string())
            )]
        );
        // The form: the same member called without `new` is not the maker
        // claimed for `new`.
        let plain = stated_end(
            &text,
            "cache-kit",
            "Loader",
            None,
            "http_client",
            "useCached(options)",
        );
        assert_eq!(SiteLabel::runner(&labels, &plain), None);
        let constructed = LabelledEnd { new: true, ..plain };
        assert_eq!(
            SiteLabel::runner(&labels, &constructed),
            Some(&ArgSlot {
                arg: 0,
                key: Some("load".into())
            })
        );
        // Nothing claimed for the export: the call is a lead, as before.
        let unclaimed = bottom(&cached("useCached(options)"), "useCached");
        let decided = decided_sites(&inputs, &root, &[unclaimed], &[]);
        assert_eq!(decided.leads.len(), 1);
        assert!(decided.ran.is_empty());
    }

    /// carrick#2426: a decided site whose name is one the store settled the
    /// library emits itself, on the same member and path, states no row and
    /// is counted; any other name, and the same name on another member, stay.
    #[test]
    fn a_site_named_a_reserved_name_states_no_row() {
        let root = decided_fixture();
        let inputs = crate::engine::discover_request_inputs(&root);
        let text = std::fs::read_to_string(root.join(STATED)).unwrap();
        let exports: Vec<crate::library_claims::ExportClaims> =
            serde_json::from_value(serde_json::json!([
                { "package": "event-kit", "version": "1.0.0", "specifier": "event-kit",
                  "export": "link", "role": "socket", "side": "client",
                  "claims": [
                    { "kind": "op", "op": "receive", "member": "on", "name": { "arg": 0 },
                      "handler": { "arg": 1 }, "on": "instance" },
                    { "kind": "op", "op": "receive", "member": "once", "name": { "arg": 0 },
                      "handler": { "arg": 1 }, "on": "instance" },
                    { "kind": "reserved", "member": "on", "name": "open", "on": "instance" },
                    { "kind": "reserved", "member": "on", "name": "close", "on": "export" },
                    { "kind": "reserved", "member": "off", "name": "chat", "on": "instance" },
                    { "kind": "reserved", "member": "once", "name": "close", "on": "instance" }
                  ] }
            ]))
            .unwrap();
        let labels = SiteLabel::from_exports(&exports);
        let on = labels
            .iter()
            .find(|label| label.member.as_deref() == Some("on"))
            .unwrap();
        // `close` is claimed on the export, `open` on instances: only the
        // names that share the op's receivers count.
        assert_eq!(on.reserved, vec!["open".to_string()]);
        let end = |member: &str, call: &str| {
            stated_end(&text, "event-kit", "link", Some(member), "socket", call)
        };
        let sites = [
            bottom(&end("on", "conn.on(\"open\", () => {})"), "conn.on"),
            bottom(&end("on", "conn.on(\"chat\", () => {})"), "conn.on"),
            bottom(&end("once", "conn.once(\"close\", () => {})"), "conn.once"),
        ];
        let decided = decided_sites(&inputs, &root, &sites, &labels);
        assert_eq!(
            decided
                .sites
                .iter()
                .map(|site| site.name_text())
                .collect::<Vec<_>>(),
            vec![Some("chat".to_string())]
        );
        assert_eq!(
            decided.reserved,
            vec![
                format!("{STATED}:{}", line_of(&text, "conn.once(")),
                format!("{STATED}:{}", line_of(&text, "conn.on(\"open")),
            ]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
        );
    }

    pub(crate) fn manifest() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// The discovery fixture's repo, its service's summary inputs, its
    /// discovery sites and the store's labels.
    pub(crate) fn discovery_fixture() -> (
        PathBuf,
        RequestSummaryInputs,
        Vec<DiscoverySite>,
        Vec<SiteLabel>,
    ) {
        let fixture = manifest().join("tests/fixtures/discovery");
        let repo = std::fs::canonicalize(fixture.join("repo")).unwrap();
        let service = crate::config::Config::load_services(vec![repo.join("carrick.json")])
            .unwrap()
            .remove(0);
        // No walk: every read is the call graph's.
        let inputs = crate::engine::discover_service_inputs(&repo, &service, None);
        let text = std::fs::read_to_string(fixture.join("sites.expected.json")).unwrap();
        let sites = serde_json::from_str::<DiscoveryFile>(&text).unwrap().sites;
        let answer: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(fixture.join("library-claims.json")).unwrap(),
        )
        .unwrap();
        (repo, inputs, sites, SiteLabel::from_store_answer(&answer))
    }

    pub(crate) fn decided() -> Decided {
        let (repo, inputs, sites, labels) = discovery_fixture();
        decided_sites(&inputs, &repo, &sites, &labels)
    }

    const WEB: &str = "services/web/src/";

    fn at(decided: &Decided) -> Vec<(String, Option<String>)> {
        decided
            .sites
            .iter()
            .map(|site| {
                (
                    format!("{}:{}", site.file.trim_start_matches(WEB), site.line),
                    site.name_text(),
                )
            })
            .collect()
    }

    fn has(sites: &[(String, Option<String>)], at: &str, name: Option<&str>) -> bool {
        sites.contains(&(at.to_string(), name.map(str::to_string)))
    }

    /// A helper called from three pages with three literals is three sites,
    /// at the pages, and none at the helper.
    #[test]
    fn a_helper_called_with_literals_is_decided_at_each_caller() {
        let decided = decided();
        assert!(decided.unplaced.is_empty(), "{:?}", decided.unplaced);
        let sites = at(&decided);
        for (file, name) in [
            ("pages/a.ts:4", "/a"),
            ("pages/b.ts:4", "/b"),
            ("pages/c.ts:4", "/c"),
        ] {
            assert!(has(&sites, file, Some(name)), "{file} {name} in {sites:?}");
        }
        for wrapper in ["api.ts:2", "api.ts:6", "api.ts:10"] {
            assert!(
                !sites.iter().any(|(at, _)| at == wrapper),
                "no site at {wrapper}: {sites:?}"
            );
        }
    }

    /// A wrapper that prefixes `/v1` passes the path on: the prefix is
    /// composed into the caller's site.
    #[test]
    fn a_prefixing_wrapper_composes_its_prefix_into_the_site() {
        let decided = decided();
        let sites = at(&decided);
        assert!(
            has(&sites, "pages/things.ts:4", Some("/v1/things")),
            "{sites:?}"
        );
        let things = decided
            .sites
            .iter()
            .find(|site| site.file.ends_with("pages/things.ts"))
            .unwrap();
        assert_eq!(things.name_args, BTreeSet::from([0]));
        assert_eq!(things.end.at(), "services/web/src/api.ts:2");
        assert_eq!(
            things
                .via
                .iter()
                .map(|via| (via.at.as_str(), via.callee.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("services/web/src/api.ts:6", "api"),
                ("services/web/src/api.ts:2", "fetch")
            ]
        );
    }

    /// A socket send helper called with two constants is two sites, and the
    /// helper is none.
    #[test]
    fn a_socket_helper_called_with_constants_is_two_sites() {
        let sites = at(&decided());
        assert!(has(&sites, "emit.ts:13", Some("user.joined")), "{sites:?}");
        assert!(has(&sites, "emit.ts:17", Some("user.left")), "{sites:?}");
        assert!(!sites.iter().any(|(at, _)| at == "emit.ts:9"), "{sites:?}");
    }

    /// A handler table registered in a loop is one site per key, at the
    /// function that handles it.
    #[test]
    fn a_handler_table_is_one_site_per_key_at_its_handler() {
        let decided = decided();
        let sites = at(&decided);
        for (file, name) in [
            ("job-table.ts:5", "image.resize"),
            ("job-table.ts:9", "image.thumbnail"),
            ("job-table.ts:13", "image.archive"),
        ] {
            assert!(has(&sites, file, Some(name)), "{file} {name} in {sites:?}");
        }
        assert!(
            !sites.iter().any(|(at, _)| at == "job-table.ts:25"),
            "{sites:?}"
        );
        let resize = decided
            .sites
            .iter()
            .find(|site| site.key.as_deref() == Some("image.resize"))
            .unwrap();
        assert_eq!(resize.handler.as_ref().map(|at| at.line), Some(5));
    }

    /// A path parameter filled only at an entry point is a lead.
    #[test]
    fn a_name_left_open_to_an_entry_point_is_a_lead() {
        let decided = decided();
        assert_eq!(
            decided
                .leads
                .iter()
                .map(|lead| (
                    lead.file.trim_start_matches(WEB).to_string(),
                    lead.line,
                    lead.reason
                ))
                .collect::<Vec<_>>(),
            vec![
                ("api.ts".to_string(), 10, Undecided::NameNeverBound),
                // A callee whose values reach two labelled exports
                // (carrick#2432): one lead per export, at the call.
                ("values.ts".to_string(), 32, Undecided::SeveralValues),
                ("values.ts".to_string(), 32, Undecided::SeveralValues),
            ]
        );
    }

    /// A call whose name only an entry point fills is one possible call,
    /// at the call that holds the open name (carrick#2438).
    #[test]
    fn a_name_nothing_binds_is_a_possible_call() {
        let decided = decided();
        let found = crate::possible_calls::from_sites(&decided, &[]);
        // The others are a callee whose values disagree (carrick#2432).
        let (found, values): (Vec<_>, Vec<_>) = found
            .into_iter()
            .partition(|call| !call.file_path.ends_with("values.ts"));
        assert!(
            values
                .iter()
                .all(|call| call.unread.reason
                    == crate::possible_calls::UnreadReason::InjectedCallee),
            "{values:?}"
        );
        assert_eq!(found.len(), 1, "{found:?}");
        let call = &found[0];
        assert_eq!(call.file_path.trim_start_matches(WEB), "api.ts");
        assert_eq!(call.line_number, 10);
        assert!(call.span_end > call.span_start, "{call:?}");
        assert!(!call.file_path.starts_with('/'), "{call:?}");
        assert_eq!(
            call.unread.reason,
            crate::possible_calls::UnreadReason::NameNeverBound
        );
        assert!(call.labelled_end.is_some(), "{call:?}");
    }

    /// Each request is one site at the call that decides it, every protocol:
    /// the full list on the fixture.
    #[test]
    fn every_request_is_one_site_where_its_name_is_decided() {
        let sites = at(&decided());
        let expected: Vec<(&str, Option<&str>)> = vec![
            ("books-api.ts:4", Some("/books")),
            ("bus.ts:7", None),
            // An import from a sibling package the summaries' value flow
            // does not read: decided here, its text is the record's to state
            // from the compiler's rows (carrick#2435).
            ("choices.ts:10", None),
            ("choices.ts:14", None),
            ("choices.ts:18", None),
            ("client.ts:4", Some("/items")),
            ("deep.ts:26", Some("/deep")),
            ("emit.ts:13", Some("user.joined")),
            ("emit.ts:17", Some("user.left")),
            ("events.ts:6", Some("orders.created")),
            ("job-table.ts:5", Some("image.resize")),
            ("job-table.ts:9", Some("image.thumbnail")),
            ("job-table.ts:13", Some("image.archive")),
            ("pages/a.ts:4", Some("/a")),
            ("pages/b.ts:4", Some("/b")),
            ("pages/c.ts:4", Some("/c")),
            ("pages/things.ts:4", Some("/v1/things")),
            // A callee that is a value, followed to the labelled export it
            // holds (carrick#2432): at the call that runs it, and at each
            // caller that hands a different one.
            ("passed-in.ts:2", Some("/defaulted")),
            ("presence.ts:4", Some("user.joined")),
            ("store.ts:3", Some("/settings")),
            ("traced-load.ts:4", Some("/traced")),
            ("values.ts:6", Some("/api/events")),
            ("values.ts:19", Some("/api/sync")),
            ("values.ts:36", Some("/api/a")),
            ("values.ts:40", Some("/api/b")),
        ];
        let mut got: Vec<(String, Option<String>)> = sites;
        got.sort();
        let mut want: Vec<(String, Option<String>)> = expected
            .into_iter()
            .map(|(at, name)| (at.to_string(), name.map(str::to_string)))
            .collect();
        want.sort();
        assert_eq!(got, want);
    }

    /// Where discovery lists only the labelled calls, the pass walks up the
    /// call graph and decides the same sites, except where only discovery's
    /// chain reaches the caller (a member of a call's result, which the call
    /// graph does not resolve): there the walk ends at a lead.
    #[test]
    fn a_chain_that_stops_short_is_walked_up() {
        let (repo, inputs, sites, labels) = discovery_fixture();
        let full = decided_sites(&inputs, &repo, &sites, &labels);
        let bottoms: Vec<DiscoverySite> = sites
            .into_iter()
            .filter(|site| site.chain.is_empty())
            .collect();
        let walked = decided_sites(&inputs, &repo, &bottoms, &labels);
        let names = |decided: &Decided| {
            let mut names: Vec<(String, u32, Option<String>)> = decided
                .sites
                .iter()
                .map(|site| (site.file.clone(), site.line, site.name_text()))
                .collect();
            names.sort();
            names
        };
        // Discovery's chain alone reaches a member of a call's result, and
        // a caller that hands a callee its value (carrick#2432).
        let only_chain = |name: &(String, u32, Option<String>)| {
            name.0.ends_with("presence.ts") || (name.0.ends_with("values.ts") && name.1 >= 36)
        };
        let mut expected = names(&full);
        expected.retain(|name| !only_chain(name));
        assert_eq!(names(&walked), expected);
        assert!(names(&full).iter().any(only_chain));
        let leads: Vec<(String, u32)> = walked
            .leads
            .iter()
            .map(|lead| (lead.file.trim_start_matches(WEB).to_string(), lead.line))
            .collect();
        assert_eq!(
            leads,
            vec![
                ("api.ts".to_string(), 10),
                ("session.ts".to_string(), 7),
                ("values.ts".to_string(), 32),
                ("values.ts".to_string(), 32),
            ]
        );
        assert!(!walked.walked.is_empty());
    }

    /// The payload's binding at the deciding call: the argument the call
    /// hands the slot through, or the call below where a wrapper builds it.
    #[test]
    fn a_payload_is_bound_at_the_deciding_call_or_where_a_wrapper_builds_it() {
        let decided = decided();
        let payload = |file: &str, line: u32| {
            decided
                .sites
                .iter()
                .find(|site| site.file.ends_with(file) && site.line == line)
                .unwrap_or_else(|| panic!("{file}:{line}"))
                .payload
                .clone()
        };
        assert_eq!(
            payload("emit.ts", 13),
            Some(PayloadBinding::Arg { arg: 1, key: None })
        );
        assert_eq!(
            payload("presence.ts", 4),
            Some(PayloadBinding::Arg { arg: 1, key: None })
        );
        assert_eq!(
            payload("bus.ts", 7),
            Some(PayloadBinding::Arg { arg: 0, key: None })
        );
        // The store labels no payload for `fetch`.
        assert_eq!(payload("pages/b.ts", 4), None);

        // A wrapper that builds the payload itself, beside the caller that
        // decides the name.
        let fixture =
            std::fs::canonicalize(manifest().join("tests/fixtures/decided-sites")).unwrap();
        let inputs = crate::engine::discover_request_inputs(&fixture);
        let session = std::fs::read_to_string(fixture.join("src/realtime/session.ts")).unwrap();
        let call = "wire.emit(name, { at: Date.now() })";
        let start = session.find(call).unwrap() as u32 + crate::swc_scanner::SWC_SPAN_BASE;
        let end = LabelledEnd {
            package: "@fixture/wire".into(),
            export: "connect".into(),
            member: Some("emit".into()),
            role: "socket".into(),
            op: Some("send".into()),
            file: "src/realtime/session.ts".into(),
            line: 7,
            span_start: start,
            new: false,
            span_end: start + call.len() as u32,
        };
        let bottom = DiscoverySite {
            file: end.file.clone(),
            line: end.line,
            span_start: end.span_start,
            span_end: end.span_end,
            callee: "wire.emit".into(),
            end: Some(end),
            chain: Vec::new(),
            stop: None,
            receiver: None,
            reached: None,
        };
        let answer: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(fixture.join("library-claims.json")).unwrap(),
        )
        .unwrap();
        let decided = decided_sites(
            &inputs,
            &fixture,
            &[bottom],
            &SiteLabel::from_store_answer(&answer),
        );
        assert_eq!(
            decided
                .sites
                .iter()
                .map(|site| (
                    site.file.as_str(),
                    site.line,
                    site.name_text(),
                    site.payload.clone()
                ))
                .collect::<Vec<_>>(),
            vec![(
                "src/realtime/rooms.ts",
                4,
                Some("room:open".to_string()),
                Some(PayloadBinding::BuiltInside {
                    built_inside: "src/realtime/session.ts:7".to_string(),
                    span_start: start,
                    span_end: start + call.len() as u32,
                    arg: 1,
                    key: None
                })
            )]
        );
    }

    /// Every row a summary states for a request this pass decides sits on
    /// one of its decided sites' file and line: the rule is the summaries'
    /// rule, generalised, so where both read a request they agree on where
    /// it is decided.
    #[test]
    fn every_decided_http_site_sits_on_its_summary_row() {
        let (repo, inputs, sites, labels) = discovery_fixture();
        let decided = decided_sites(&inputs, &repo, &sites, &labels);
        let summaries = crate::request_summary::summarize(
            &inputs,
            &crate::client_semantics::LibrarySemantics::default(),
        );
        // The request each row reaches (its file and span start) -> the
        // file and line of every row stated for it.
        let mut rows: HashMap<(String, u32), HashSet<(String, u32)>> = HashMap::new();
        for file in inputs.files.keys() {
            for at in summaries.rows(file).into_iter().flat_map(|at| at.values()) {
                for row in at {
                    rows.entry((relative(&repo, &row.request_at.0), row.request_at.1))
                        .or_default()
                        .insert((relative(&repo, file), row.line));
                }
            }
        }
        // The request each decided HTTP site reaches -> the decided sites.
        let mut sites_of: HashMap<(String, u32), HashSet<(String, u32)>> = HashMap::new();
        for site in decided
            .sites
            .iter()
            .filter(|site| site.end.role == "http_client")
        {
            sites_of
                .entry((site.end.file.clone(), site.end.span_start))
                .or_default()
                .insert((site.file.clone(), site.line));
        }
        let mut compared = 0;
        for (request, lines) in &rows {
            let Some(decided) = sites_of.get(request) else {
                continue;
            };
            for line in lines {
                compared += 1;
                assert!(
                    decided.contains(line),
                    "the summary row at {line:?} is not a decided site of {request:?}: {decided:?}"
                );
            }
        }
        assert!(compared > 0, "no summary row compared: {rows:?}");
    }

    // -----------------------------------------------------------------------
    // Hand-built sites on the `decided-sites` fixture.

    pub(crate) fn decided_fixture() -> PathBuf {
        std::fs::canonicalize(manifest().join("tests/fixtures/decided-sites")).unwrap()
    }

    /// The SWC span of the first `call` in `text`.
    pub(crate) fn span_of(text: &str, call: &str) -> (u32, u32) {
        let start = text.find(call).unwrap() as u32 + crate::swc_scanner::SWC_SPAN_BASE;
        (start, start + call.len() as u32)
    }

    /// The 1-based line of the first `needle` in `text`.
    pub(crate) fn line_of(text: &str, needle: &str) -> u32 {
        text[..text.find(needle).unwrap()].matches('\n').count() as u32 + 1
    }

    /// A label of the fixture's `@fixture/wire` `connect` export.
    pub(crate) fn wire_label(
        member: &str,
        op: &str,
        payload: Option<usize>,
        handler: Option<usize>,
    ) -> SiteLabel {
        SiteLabel {
            package: "@fixture/wire".into(),
            export: "connect".into(),
            member: Some(member.into()),
            role: "socket".into(),
            op: Some(op.into()),
            side: Some("client".into()),
            name: Some(ArgSlot { arg: 0, key: None }),
            payload: payload.map(|arg| ArgSlot { arg, key: None }),
            handler: handler.map(|arg| ArgSlot { arg, key: None }),
            reserved: Vec::new(),
            runs: None,
            request: None,
            request_dropped: false,
            method: None,
        }
    }

    /// The language's own `fetch`, with label v2's method fields.
    pub(crate) fn fetch_label() -> SiteLabel {
        SiteLabel {
            package: "lib:lib.dom.d.ts".into(),
            export: "fetch".into(),
            member: None,
            role: "http_client".into(),
            op: Some("request".into()),
            side: Some("client".into()),
            name: Some(ArgSlot { arg: 0, key: None }),
            payload: None,
            handler: None,
            reserved: Vec::new(),
            runs: None,
            request: None,
            request_dropped: false,
            method: Some(MethodLabel {
                from: Some(ArgSlot {
                    arg: 1,
                    key: Some("method".into()),
                }),
                default: Some("GET".into()),
                literal: None,
            }),
        }
    }

    /// A send to a forked child process: its label names a payload and no
    /// name.
    pub(crate) fn child_send_label() -> SiteLabel {
        SiteLabel {
            package: "node:child_process".into(),
            export: "ChildProcess".into(),
            member: Some("send".into()),
            role: "in_process_bus".into(),
            op: Some("send".into()),
            side: Some("client".into()),
            name: None,
            payload: Some(ArgSlot { arg: 0, key: None }),
            handler: None,
            reserved: Vec::new(),
            runs: None,
            request: None,
            request_dropped: false,
            method: None,
        }
    }

    /// The labelled call `call` of `file` (its text `text`) under `label`.
    pub(crate) fn end_for(label: &SiteLabel, file: &str, text: &str, call: &str) -> LabelledEnd {
        let (span_start, span_end) = span_of(text, call);
        LabelledEnd {
            package: label.package.clone(),
            export: label.export.clone(),
            member: label.member.clone(),
            role: label.role.clone(),
            op: label.op.clone(),
            file: file.into(),
            line: line_of(text, call),
            span_start,
            new: false,
            span_end,
        }
    }

    /// A discovery site that is its own labelled call.
    pub(crate) fn bottom(end: &LabelledEnd, callee: &str) -> DiscoverySite {
        DiscoverySite {
            file: end.file.clone(),
            line: end.line,
            span_start: end.span_start,
            span_end: end.span_end,
            callee: callee.into(),
            end: Some(end.clone()),
            chain: Vec::new(),
            stop: None,
            receiver: None,
            reached: None,
        }
    }

    pub(crate) const ROOMS: &str = "src/slots/rooms.ts";

    /// The calls of `slots/rooms.ts` as discovery lists them, each with its
    /// label: `(call, callee, label)`.
    pub(crate) fn rooms_calls() -> Vec<(&'static str, &'static str, SiteLabel)> {
        let send = wire_label("emit", "send", Some(1), None);
        let on = wire_label("on", "receive", None, Some(1));
        vec![
            (
                "wire.emit(\"room:\" + roomId, { at: 1 })",
                "wire.emit",
                send.clone(),
            ),
            ("fetch(\"/items/1\", init)", "fetch", fetch_label()),
            (
                "wire.on(\"user.joined\", (data) => {\n    console.log(data);\n  })",
                "wire.on",
                on.clone(),
            ),
            ("wire.emit(topic, { shipped })", "wire.emit", send),
            (
                "fetch(\"/items\", { method: \"POST\" })",
                "fetch",
                fetch_label(),
            ),
            ("wire.on(\"user.left\", onLeft)", "wire.on", on),
            ("fetch(\"/items/3\", options)", "fetch", fetch_label()),
            ("worker.send(\"ping\")", "worker.send", child_send_label()),
        ]
    }

    /// The discovery sites and labels of `slots/rooms.ts`.
    pub(crate) fn rooms_sites(text: &str) -> (Vec<DiscoverySite>, Vec<SiteLabel>) {
        let mut sites = Vec::new();
        let mut labels: Vec<SiteLabel> = Vec::new();
        for (call, callee, label) in rooms_calls() {
            sites.push(bottom(&end_for(&label, ROOMS, text, call), callee));
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        (sites, labels)
    }

    /// A call the call graph leaves unresolved (a log line, a global
    /// function) in a hop's body is not the hop: the chain is placed through
    /// the call that enters the next function, and nothing is walked.
    #[test]
    fn an_unrelated_call_in_a_hop_body_is_not_the_hop() {
        let root = decided_fixture();
        let inputs = crate::engine::discover_request_inputs(&root);
        let session = std::fs::read_to_string(root.join("src/inert/session.ts")).unwrap();
        let presence = std::fs::read_to_string(root.join("src/inert/presence.ts")).unwrap();
        assert!(session.contains("console.log(name);"));
        let send = wire_label("emit", "send", Some(1), None);
        let end = end_for(
            &send,
            "src/inert/session.ts",
            &session,
            "wire.emit(name, data)",
        );
        let call = "openSession(\"ws://chat\").notify(\"user.joined\", { at: 1 })";
        let (span_start, span_end) = span_of(&presence, call);
        let site = DiscoverySite {
            file: "src/inert/presence.ts".into(),
            line: line_of(&presence, call),
            span_start,
            span_end,
            callee: "openSession(\"ws://chat\").notify".into(),
            end: Some(end),
            chain: vec![
                ChainHop {
                    function: "Session.notify".into(),
                    file: "src/inert/session.ts".into(),
                    line: line_of(&session, "notify(name"),
                },
                ChainHop {
                    function: "relay".into(),
                    file: "src/inert/session.ts".into(),
                    line: line_of(&session, "function relay"),
                },
            ],
            stop: None,
            receiver: None,
            reached: None,
        };
        let decided = decided_sites(&inputs, &root, &[site], &[send]);
        assert_eq!(
            decided
                .sites
                .iter()
                .map(|s| (s.file.as_str(), s.line, s.name_text()))
                .collect::<Vec<_>>(),
            vec![("src/inert/presence.ts", 4, Some("user.joined".to_string()))]
        );
        assert!(decided.leads.is_empty(), "{:?}", decided.leads);
        assert!(decided.walked.is_empty(), "{:?}", decided.walked);
    }

    /// A receive site names the function that handles it, written at the
    /// call or declared under the name the call hands, and where the
    /// payload arrives: the handler's argument.
    #[test]
    fn a_receive_site_is_anchored_at_its_handler() {
        let root = decided_fixture();
        let inputs = crate::engine::discover_request_inputs(&root);
        let text = std::fs::read_to_string(root.join(ROOMS)).unwrap();
        let (sites, labels) = rooms_sites(&text);
        let decided = decided_sites(&inputs, &root, &sites, &labels);
        let site = |name: &str| {
            decided
                .sites
                .iter()
                .find(|site| site.name_text().as_deref() == Some(name))
                .unwrap_or_else(|| panic!("{name}"))
        };
        let inline = "(data) => {\n    console.log(data);\n  }";
        let (start, end) = span_of(&text, inline);
        let joined = site("user.joined");
        assert_eq!(
            joined.handler,
            Some(HandlerAt {
                file: ROOMS.into(),
                line: line_of(&text, inline),
                span_start: start,
                span_end: end,
                name: None,
            })
        );
        assert_eq!(
            joined.payload,
            Some(PayloadBinding::Arg { arg: 1, key: None })
        );
        let left = site("user.left");
        assert_eq!(
            left.handler.as_ref().map(|at| (at.file.as_str(), at.line)),
            Some((ROOMS, line_of(&text, "function onLeft")))
        );
        assert_eq!(
            left.payload,
            Some(PayloadBinding::Arg { arg: 1, key: None })
        );
        // A send names no handler.
        assert_eq!(site("/items").handler, None);
    }

    pub(crate) const PATHS: &str = "src/slots/paths.ts";

    /// The fetches of `slots/paths.ts`, each its own discovery site, decided.
    pub(crate) fn paths_decided() -> (Decided, String) {
        let root = decided_fixture();
        let inputs = crate::engine::discover_request_inputs(&root);
        let text = std::fs::read_to_string(root.join(PATHS)).unwrap();
        let label = fetch_label();
        let sites: Vec<_> = [
            "fetch(`${process.env.API}${path}`, { method })",
            "fetch(url)",
            "fetch(`${API}${path}`)",
            "fetch(`/accounts/${id}`)",
            "fetch(path)",
            "fetch(`${process.env.API}${path}`, { ...init, credentials: \"include\" })",
            "fetch(`/groups/${id}?workspace=${workspace}`)",
            "fetch(`/users/${id}`)",
            "fetch(`/dashboard?page=${page}`)",
            "fetch(`${process.env.API}${path}`, { ...rest, credentials: \"include\" })",
            "fetch(`${process.env.API}${path}`, { ...requestInit, credentials: \"include\" })",
        ]
        .iter()
        .map(|call| bottom(&end_for(&label, PATHS, &text, call), "fetch"))
        .collect();
        (decided_sites(&inputs, &root, &sites, &[label]), text)
    }

    /// A request whose path keeps a parameter up to a function nothing calls
    /// is a site at the lowest call where the summaries' composition states
    /// a route, each such parameter a path parameter (carrick#2519): through
    /// a wrapper that adds an environment base, and through a local URL on a
    /// base the constructor sets. A path that states a route with a path
    /// parameter is decided at its own call, not at its callers; a value in
    /// the query is cut off; a wrapper's whole-path parameter is still
    /// filled by each caller; a whole path no caller states stays a lead.
    #[test]
    fn a_path_parameter_in_a_whole_segment_is_a_site_where_the_route_is_stated() {
        let (decided, text) = paths_decided();
        let line = |needle: &str| line_of(&text, needle);
        let sites: Vec<(u32, Option<String>)> = decided
            .sites
            .iter()
            .filter(|site| site.file == PATHS)
            .map(|site| (site.line, site.name_text()))
            .collect();
        for (at, name) in [
            (
                line("send(`/tasks/"),
                "${process.env.API}/tasks/${taskId}/status",
            ),
            (
                line("fetch(url)"),
                "${this.baseUrl}/resources/${resourceId}/files/${fileId}",
            ),
            (line("get(\"/orders\")"), "${process.env.API}/orders"),
            (line("get(\"/refunds\")"), "${process.env.API}/refunds"),
            (line("readOne(\"me\")"), "/accounts/me"),
            (line("fetch(`/users/${id}`)"), "/users/${id}"),
            (line("fetch(`/dashboard?page="), "/dashboard"),
            (line("sendNote({"), "${process.env.API}/notes/${id}"),
            (
                line("closeWith(`/notes/"),
                "${process.env.API}/notes/${id}/close",
            ),
            (
                line("call(`/archives/"),
                "${process.env.API}/archives/${id}",
            ),
            (line("fetch(`/groups/"), "/groups/${id}"),
        ] {
            assert!(
                sites.contains(&(at, Some(name.to_string()))),
                "{at} {name} in {sites:?}"
            );
        }
        assert_eq!(sites.len(), 11, "{sites:?}");
        // A segment every caller states is not a path parameter at the call;
        // one a caller fills from data is, and its callers are not sites.
        for not_a_site in [
            "fetch(`/accounts/${id}`)",
            "readUser(\"first\")",
            "readUser(user.id)",
        ] {
            assert!(
                !sites.iter().any(|(at, _)| *at == line(not_a_site)),
                "{not_a_site}: {sites:?}"
            );
        }
        // The method a caller writes into the rest of a destructured
        // options parameter.
        for (call, method) in [("sendNote({", "DELETE"), ("closeWith(`/notes/", "PATCH")] {
            let site = decided
                .sites
                .iter()
                .find(|site| site.line == line(call))
                .unwrap();
            assert!(
                matches!(&site.method, Some(MethodValue::Pieces(pieces)) if pieces == &vec![Piece::Lit(method.into())]),
                "{call}: {:?}",
                site.method
            );
        }
        // The method a caller writes into a spread options object.
        let archive = decided
            .sites
            .iter()
            .find(|site| site.line == line("call(`/archives/"))
            .unwrap();
        assert!(
            matches!(&archive.method, Some(MethodValue::Pieces(pieces)) if pieces == &vec![Piece::Lit("POST".into())]),
            "{:?}",
            archive.method
        );
        // The method the caller writes reaches the composed site.
        let status = decided
            .sites
            .iter()
            .find(|site| site.line == line("send(`/tasks/"))
            .unwrap();
        assert_eq!(status.name_args, BTreeSet::from([0]));
        assert!(
            matches!(&status.method, Some(MethodValue::Pieces(pieces)) if pieces == &vec![Piece::Lit("PUT".into())]),
            "{:?}",
            status.method
        );
        assert_eq!(
            decided
                .leads
                .iter()
                .map(|lead| (lead.line, lead.reason))
                .collect::<Vec<_>>(),
            vec![(line("fetch(path)"), Undecided::NameNeverBound)]
        );
    }

    /// The rest of a destructured parameter is read for a decided site's
    /// method only: the request summaries state no row through it, so no
    /// summary row, and no body, moves (carrick#2519).
    #[test]
    fn a_spread_rest_moves_no_summary_row() {
        let root = decided_fixture();
        let inputs = crate::engine::discover_request_inputs(&root);
        let text = std::fs::read_to_string(root.join(PATHS)).unwrap();
        let summaries = crate::request_summary::summarize(
            &inputs,
            &crate::client_semantics::LibrarySemantics::default(),
        );
        let lines: BTreeSet<u32> = inputs
            .files
            .keys()
            .filter(|file| file.ends_with(PATHS))
            .flat_map(|file| summaries.rows(file))
            .flat_map(|at| at.values())
            .flatten()
            .map(|row| row.line)
            .collect();
        for call in ["sendNote({", "closeWith(`/notes/"] {
            assert!(!lines.contains(&line_of(&text, call)), "{call}: {lines:?}");
        }
    }

    pub(crate) const PAYLOADS: &str = "src/slots/payloads.ts";

    /// The fetches of `slots/payloads.ts`.
    pub(crate) const PAYLOAD_CALLS: [&str; 9] = [
        "fetch(`${process.env.API}${path}`, { ...rest, credentials: \"include\" })",
        "fetch(`${process.env.API}${path}`, { ...tracedRest, credentials: \"include\" })",
        "fetch(`${process.env.API}${path}`, { ...requestInit, credentials: \"include\" })",
        "fetch(`${process.env.API}${path}`, { method: \"POST\", ...init })",
        "fetch(`${process.env.API}/direct/ping`, { method: \"POST\" })",
        "fetch(`${process.env.API}/direct/body`, { method: \"POST\", body })",
        "fetch(`${process.env.API}${path}`, { ...bodiless, credentials: \"include\" })",
        "fetch(`${process.env.API}${path}`, withDefaults(init))",
        "fetch(`${process.env.API}/direct/spread`, { ...getDefaults(), method: \"POST\" })",
    ];

    /// `fetch`, with the request's `body` key as its payload slot.
    pub(crate) fn payloads_label() -> SiteLabel {
        let mut label = fetch_label();
        label.payload = Some(ArgSlot {
            arg: 1,
            key: Some("body".into()),
        });
        label
    }

    fn payloads_decided() -> (Decided, String) {
        let (decided, text, _) = decided_at(PAYLOADS, &PAYLOAD_CALLS, &payloads_label());
        (decided, text)
    }

    /// carrick#2674 P3-1a: the option key a payload slot names is carried up
    /// through a wrapper's spread of its own parameter and the rest of a
    /// destructured one, at any closure depth for the rest, and read at the
    /// deciding call: written there, or stated absent by a literal that holds
    /// no spread. A wrapper that takes the key out, hands the options through
    /// a call, or a literal that spreads another object, is not read. The
    /// payload binding itself stays as it was.
    #[test]
    fn a_payload_key_is_carried_through_a_spread_and_a_rest_to_the_deciding_call() {
        let (decided, text) = payloads_decided();
        let slot_at = |needle: &str| {
            let line = line_of(&text, needle);
            let mut here = decided.sites.iter().filter(|site| site.line == line);
            let site = here.next().unwrap_or_else(|| panic!("no site at {needle}"));
            assert!(here.next().is_none(), "{needle}");
            site.payload_slot.clone()
        };
        let written = |arg| {
            Some(SlotBinding::Written {
                arg,
                key: "body".into(),
            })
        };
        let unwritten = |arg| {
            Some(SlotBinding::Unwritten {
                arg,
                key: "body".into(),
            })
        };
        // The rest of a destructured parameter.
        assert_eq!(slot_at("send({ path: \"/notes\""), written(0));
        assert_eq!(slot_at("send({ path: \"/ping\""), unwritten(0));
        // The same rest, in a closure.
        assert_eq!(slot_at("sendTraced({ path: \"/traced\""), written(0));
        assert_eq!(slot_at("sendTraced({ path: \"/traced/ping\""), unwritten(0));
        assert_eq!(
            slot_at("call(`/notes/${id}`, { method: \"DELETE\""),
            unwritten(1)
        );
        assert_eq!(slot_at("call(`/notes/${id}/title`"), written(1));
        // A spread of the function's own parameter.
        assert_eq!(slot_at("post(\"/logout\", {})"), unwritten(1));
        assert_eq!(slot_at("post(\"/tags\""), written(1));
        // A direct call that writes a closed literal.
        assert_eq!(slot_at("direct/ping`"), unwritten(1));
        assert_eq!(slot_at("direct/body`"), written(1));
        // Controls: nothing is read.
        assert_eq!(slot_at("call(\"/x\""), None);
        assert_eq!(slot_at("sendStripped({ path"), None);
        assert_eq!(slot_at("wrapped(\"/wrapped\""), None);
        assert_eq!(slot_at("direct/spread`"), None);
        // The payload binding is what it was: built inside the wrapper.
        for needle in ["send({ path: \"/ping\"", "post(\"/logout\", {})"] {
            let line = line_of(&text, needle);
            let site = decided.sites.iter().find(|site| site.line == line).unwrap();
            assert!(
                matches!(site.payload, Some(PayloadBinding::BuiltInside { .. })),
                "{needle}: {:?}",
                site.payload
            );
        }
    }

    /// carrick#2674 P3-1a review: a wrapper that writes a key onto the
    /// options it hands on (assigning a member, `Object.assign` onto it, or
    /// spreading it into a new object beside the key) does not hold what the
    /// caller wrote: neither the payload slot's key nor the method is read
    /// at the caller, so neither reads as unwritten there. Object.assign onto
    /// a new object only reads the rest, which still carries.
    #[test]
    fn a_key_written_onto_the_carried_options_ends_the_carry() {
        const WRITTEN: &str = "src/slots/written.ts";
        let calls = [
            "fetch(`${process.env.API}${path}/mut`, { ...mutRest, credentials: \"include\" })",
            "fetch(`${process.env.API}${path}/assign`, { ...assignRest, credentials: \"include\" })",
            "fetch(`${process.env.API}${path}/closure`, { ...closureRest, credentials: \"include\" })",
            "fetch(`${process.env.API}${path}/pm`, { method: \"POST\", ...init })",
            "fetch(`${process.env.API}${path}/pa`, { method: \"POST\", ...assignInit })",
            "fetch(`${process.env.API}${path}/inner`, { ...innerRest, credentials: \"include\" })",
            "fetch(`${process.env.API}${path}/mm`, { ...methodRest, credentials: \"include\" })",
            "fetch(`${process.env.API}${path}/ma`, { credentials: \"include\", ...methodInit })",
            "fetch(`${process.env.API}${path}/copy`, { ...copyRest, credentials: \"include\" })",
        ];
        let (decided, text, _) = decided_at(WRITTEN, &calls, &payloads_label());
        let sites_at = |needle: &str| {
            let line = line_of(&text, needle);
            let here: Vec<_> = decided
                .sites
                .iter()
                .filter(|site| site.line == line)
                .collect();
            assert!(!here.is_empty(), "no site at {needle}");
            here
        };
        for needle in [
            "sendMut({ path",
            "sendAssign({ path",
            "sendClosure({ path",
            "postMut(\"/pm\"",
            "postAssign(\"/pa\"",
        ] {
            for site in sites_at(needle) {
                assert_eq!(site.payload_slot, None, "{needle}");
            }
        }
        for needle in ["sendMethodMut({ path", "sendMethodAssign(\"/ma\""] {
            for site in sites_at(needle) {
                assert_eq!(
                    site.method,
                    Some(MethodValue::Pieces(vec![Piece::Unknown])),
                    "{needle}"
                );
            }
        }
        // A hop that spreads the rest beside a body decides the site: the
        // body is written there, and its caller is no site.
        assert!(
            !decided
                .sites
                .iter()
                .any(|site| site.line == line_of(&text, "sendOuter({ path"))
        );
        let hop = sites_at("sendInner({ ...outerRest");
        assert_eq!(hop.len(), 1);
        assert_eq!(
            hop[0].payload_slot,
            Some(SlotBinding::Written {
                arg: 0,
                key: "body".into(),
            })
        );
        // Control: the rest is only read by Object.assign.
        let copy = sites_at("sendCopy({ path");
        assert_eq!(copy.len(), 1);
        assert_eq!(
            copy[0].payload_slot,
            Some(SlotBinding::Unwritten {
                arg: 0,
                key: "body".into(),
            })
        );
    }

    // -----------------------------------------------------------------------
    // carrick#2524: a site row writes no source text, yields to the
    // summaries' placement, and carries its instance's base.

    const BASES: &str = "src/slots/bases.ts";
    const PLACEMENT: &str = "src/slots/placement.ts";
    const INSTANCES: &str = "src/slots/instances.ts";

    /// The decided sites of `file`'s `calls`, each its own labelled call
    /// under `label`, with the fixture's summary inputs.
    fn decided_at(
        file: &str,
        calls: &[&str],
        label: &SiteLabel,
    ) -> (Decided, String, RequestSummaryInputs) {
        let root = decided_fixture();
        let inputs = crate::engine::discover_request_inputs(&root);
        let text = std::fs::read_to_string(root.join(file)).unwrap();
        let sites: Vec<_> = calls
            .iter()
            .map(|call| bottom(&end_for(label, file, &text, call), "fetch"))
            .collect();
        let decided = decided_sites(&inputs, &root, &sites, std::slice::from_ref(label));
        (decided, text, inputs)
    }

    fn names_at(decided: &Decided, file: &str) -> Vec<(u32, Option<String>)> {
        decided
            .sites
            .iter()
            .filter(|site| site.file == file)
            .map(|site| (site.line, site.name_text()))
            .collect()
    }

    /// A base read through a constant whose initializer is an expression
    /// over one environment read is written as that read, never as the
    /// expression's source text; over two reads, the name is unstated. A
    /// constant that is the read itself is written as the read, and an
    /// expression with no read is kept as before (the normalizer reads it
    /// as an injected base).
    #[test]
    fn a_base_written_as_an_expression_s_text_is_no_name() {
        let (decided, text, _) = decided_at(
            BASES,
            &[
                "fetch(`${SEED_BASE}/sessions`, { method: \"POST\" })",
                "fetch(`${PLAIN_BASE}/sessions/end`, { method: \"POST\" })",
                "fetch(`${EITHER_BASE}/status`)",
                "fetch(`${TRIMMED}/ping`)",
            ],
            &fetch_label(),
        );
        let line = |needle: &str| line_of(&text, needle);
        assert_eq!(
            names_at(&decided, BASES),
            vec![
                (
                    line("fetch(`${SEED_BASE}"),
                    Some("${process.env.SEED_API}/sessions".to_string())
                ),
                (
                    line("fetch(`${PLAIN_BASE}"),
                    Some("${process.env.PLAIN_API}/sessions/end".to_string())
                ),
                (line("fetch(`${EITHER_BASE}"), None),
                (
                    line("fetch(`${TRIMMED}"),
                    Some("${settings.base.replace(/\\/$/, \"\")}/ping".to_string())
                ),
            ]
        );
        assert_eq!(
            env_reads("(process.env.A || import.meta.env.B || process.env.A).trim()"),
            vec!["process.env.A".to_string(), "import.meta.env.B".to_string()]
        );
        assert!(is_reference("this.baseUrl") && is_reference("config.api?.url"));
        assert!(is_reference("this.#base") && is_reference("$base"));
        assert!(!is_reference(
            "(process.env.API || \"x\").replace(/\\/$/, \"\")"
        ));
        assert!(!is_reference("base()") && !is_reference("a[0]") && !is_reference(""));
    }

    /// Where the summaries state a request's row at a caller (a query-only
    /// argument, a literal in the query), the site at the request itself
    /// yields: one request, one row. A request the summaries state no row
    /// for keeps its site.
    #[test]
    fn a_site_yields_to_the_summaries_placement() {
        let calls = ["fetch(`/api/notices?${query}`)", "fetch(`/accounts/${id}`)"];
        let (mut decided, text, inputs) = decided_at(PLACEMENT, &calls, &fetch_label());
        let line = |needle: &str| line_of(&text, needle);
        assert_eq!(
            decided.sites.len(),
            2,
            "{:?}",
            names_at(&decided, PLACEMENT)
        );
        let placed = crate::request_summary::summarize(
            &inputs,
            &crate::client_semantics::LibrarySemantics::default(),
        )
        .placements();
        let root = decided_fixture();
        assert_eq!(decided.yield_to_summaries(&root, &placed), 1);
        assert_eq!(
            names_at(&decided, PLACEMENT),
            vec![(
                line("fetch(`/accounts/"),
                Some("/accounts/${id}".to_string())
            )]
        );
        // The summaries' row for the notices request stands at its caller.
        let notices = placed
            .iter()
            .find(|((file, _), _)| relative(&root, file) == PLACEMENT)
            .map(|(_, rows)| rows.clone())
            .unwrap();
        assert_eq!(
            notices
                .iter()
                .map(|(file, at)| (relative(&root, file), *at))
                .collect::<Vec<_>>(),
            vec![(PLACEMENT.to_string(), line("notices(\"unread=1\")"))]
        );

        // The same on the paths fixture: a literal in the query only.
        let (mut paths, paths_text) = paths_decided();
        let before = paths.sites.len();
        let paths_placed = crate::request_summary::summarize(
            &crate::engine::discover_request_inputs(&root),
            &crate::client_semantics::LibrarySemantics::default(),
        )
        .placements();
        assert_eq!(paths.yield_to_summaries(&root, &paths_placed), 1);
        assert_eq!(paths.sites.len(), before - 1);
        assert!(
            !paths
                .sites
                .iter()
                .any(|site| site.line == line_of(&paths_text, "fetch(`/dashboard?page="))
        );
    }

    /// The label of the fixture's `@fixture/http` instance member `get`.
    fn http_get_label() -> SiteLabel {
        SiteLabel {
            package: "@fixture/http".into(),
            export: "default".into(),
            member: Some("get".into()),
            role: "http_client".into(),
            op: Some("request".into()),
            side: Some("client".into()),
            name: Some(ArgSlot { arg: 0, key: None }),
            payload: None,
            handler: None,
            reserved: Vec::new(),
            runs: None,
            request: None,
            request_dropped: false,
            method: Some(MethodLabel {
                from: None,
                default: None,
                literal: Some("GET".into()),
            }),
        }
    }

    /// The store's claims for `@fixture/http`: `create` reads a base from
    /// its options' `baseURL`.
    fn http_claims() -> Vec<crate::library_claims::ExportClaims> {
        vec![
            serde_json::from_value(serde_json::json!({
                "package": "@fixture/http", "version": "1.0.0", "specifier": "@fixture/http",
                "export": "default", "role": "http_client",
                "claims": [
                    { "kind": "make", "form": "call", "member": "create",
                      "base": { "arg": 0, "key": "baseURL" } },
                    { "kind": "op", "op": "request", "member": "get", "on": "both",
                      "name": { "arg": 0 }, "method": "GET" }
                ]
            }))
            .unwrap(),
        ]
    }

    /// A request through an instance whose maker was handed a literal base
    /// goes to that base: on a field a constructor sets and on a module
    /// instance alike. A base the code does not state leaves the site out;
    /// a maker handed no base leaves the path as it was.
    #[test]
    fn an_instance_base_rides_onto_the_site() {
        let calls = [
            "this.client.get(\"/invoices\")",
            "ledger.get(\"/entries\")",
            "audit.get(\"/events\")",
            "plain.get(\"/status\")",
        ];
        let (mut decided, text, inputs) = decided_at(INSTANCES, &calls, &http_get_label());
        let line = |needle: &str| line_of(&text, needle);
        assert_eq!(
            decided.sites.len(),
            4,
            "{:?}",
            names_at(&decided, INSTANCES)
        );
        let library = crate::request_summary::library_sites(&inputs);
        let root = decided_fixture();
        assert_eq!(
            decided.carry_instance_bases(&root, &library, &http_claims()),
            1
        );
        assert_eq!(
            names_at(&decided, INSTANCES),
            vec![
                (
                    line("this.client.get("),
                    Some("https://billing.example.com/v1/invoices".to_string())
                ),
                (
                    line("ledger.get("),
                    Some("https://ledger.example.com/api/entries".to_string())
                ),
                (line("plain.get("), Some("/status".to_string())),
            ]
        );
        // No claim names a maker's base: every path is as it was.
        let (mut unclaimed, _, _) = decided_at(INSTANCES, &calls, &http_get_label());
        assert_eq!(unclaimed.carry_instance_bases(&root, &library, &[]), 0);
        assert_eq!(names_at(&unclaimed, INSTANCES).len(), 4);
        assert_eq!(
            names_at(&unclaimed, INSTANCES)[0].1.as_deref(),
            Some("/invoices")
        );
    }

    /// carrick#2541: a leading value that reads as several paths is never
    /// renamed to the one environment read its text holds.
    #[test]
    fn a_base_that_reads_as_several_paths_names_no_site() {
        let pieces = [
            Piece::Opaque {
                text: "local ? \"/v1\" : process.env.API_URL".to_string(),
                end: super::super::Root::Hole(super::super::HoleReason::SeveralPaths),
                link: None,
            },
            Piece::Lit("/x".to_string()),
        ];
        assert_eq!(
            named_base(&pieces).and_then(|name| render_target(&name)),
            None
        );
    }

    /// The `discovery-siblings` fixture's app and client, walked with rows,
    /// and the sites discovery finds in the app with the client as its
    /// sibling (carrick#2618).
    fn sibling_sites(
        fixture: &str,
    ) -> Option<(
        super::super::fixture_walk::WalkedServices,
        Vec<DiscoverySite>,
        Vec<SiteLabel>,
    )> {
        let on = super::super::fixture_walk::walked_services(
            fixture,
            &[("services/app", "app"), ("packages/client", "client")],
            true,
        )?;
        let text = std::fs::read_to_string(
            manifest().join(format!("tests/fixtures/{fixture}/library-claims.json")),
        )
        .unwrap();
        let answer = crate::library_store::parse_answer(&text);
        let labels = super::super::discovery::Labels::of_answer(&answer);
        let [app, client] = on.inputs.as_slice() else {
            panic!("two services");
        };
        let discovery = super::super::discovery::discover_with(app, &[client], &labels, &on.repo);
        let sites = discovery
            .sites
            .iter()
            .map(|site| {
                serde_json::from_value(serde_json::to_value(site).unwrap()).expect("a site")
            })
            .collect();
        Some((on, sites, SiteLabel::from_exports(&answer.exports)))
    }

    /// carrick#2618: a request a sibling's code decides is a site at the
    /// call of this service that enters the sibling, with the sibling's
    /// hops in its `via`, and no site or lead stands in a sibling's file. The
    /// answer key is `tests/fixtures/discovery-siblings/README.md`.
    #[test]
    fn a_request_a_sibling_decides_is_a_site_at_this_service_s_call() {
        let Some((on, sites, labels)) = sibling_sites("discovery-siblings") else {
            return;
        };
        let [app, client] = on.inputs.as_slice() else {
            panic!("two services");
        };
        let decided = decided_sites_with(app, &[client], &on.repo, &sites, &labels);
        let found: Vec<(u32, Option<String>)> = decided
            .sites
            .iter()
            .map(|site| (site.line, site.name_text()))
            .collect();
        assert_eq!(
            found,
            vec![
                (5, Some("/v1/api/items/${id}".to_string())),
                (9, Some("/v1/api/events".to_string())),
                (14, Some("/v1/api/items/${id}".to_string())),
            ],
            "{:?}",
            decided.sites
        );
        for site in &decided.sites {
            assert_eq!(site.file, "services/app/src/main.ts");
            assert!(
                site.via
                    .iter()
                    .all(|via| via.at.starts_with("packages/client/")),
                "{:?}",
                site.via
            );
        }
        assert!(decided.unplaced.is_empty(), "{:?}", decided.unplaced);
        assert!(
            decided
                .leads
                .iter()
                .all(|lead| !lead.file.starts_with("packages/")),
            "{:?}",
            decided.leads
        );
        // Without siblings the same discovery sites are not placed: the
        // labelled call is in a file this service does not read.
        let alone = decided_sites(app, &on.repo, &sites, &labels);
        assert!(alone.sites.is_empty(), "{:?}", alone.sites);
        assert_eq!(alone.unplaced.len(), 3, "{:?}", alone.unplaced);
    }

    /// carrick#2816: a call that walks from different branches of one
    /// function to one labelled call, each branch a different request, is a
    /// lead (`SeveralOperations`) and no site; a call with one request still
    /// is a site. The answer key is `tests/fixtures/discovery-dispatcher/README.md`.
    #[test]
    fn a_call_that_reaches_several_operations_is_a_lead_not_the_first_branch() {
        let Some((on, sites, labels)) = sibling_sites("discovery-dispatcher") else {
            return;
        };
        let [app, client] = on.inputs.as_slice() else {
            panic!("two services");
        };
        let decided = decided_sites_with(app, &[client], &on.repo, &sites, &labels);
        let found: Vec<(String, u32, Option<String>)> = decided
            .sites
            .iter()
            .map(|site| (site.file.clone(), site.line, site.name_text()))
            .collect();
        assert_eq!(
            found,
            vec![(
                "services/app/src/main.ts".to_string(),
                16,
                Some("/items/42".to_string())
            )],
            "{:?}",
            decided.sites
        );
        let mut leads: Vec<(String, u32, Undecided)> = decided
            .leads
            .iter()
            .map(|lead| (lead.file.clone(), lead.line, lead.reason))
            .collect();
        leads.sort();
        let several =
            |file: &str, line: u32| (file.to_string(), line, Undecided::SeveralOperations);
        assert_eq!(
            leads,
            vec![
                several("services/app/src/local.ts", 16),
                several("services/app/src/main.ts", 4),
                several("services/app/src/main.ts", 8),
                several("services/app/src/main.ts", 12),
            ],
            "{:?}",
            decided.leads
        );
    }

    /// carrick#2618: a call into a sibling inside a client wrapper is a site
    /// like any other. Whether a caller of the wrapper states the request
    /// instead (the amendment ruled on #2448) is known only once
    /// call-through states that caller's row at the join point, which then
    /// withdraws the wrapper's (`call_through::state_leaves`); this pass
    /// never withholds it. A command that calls the sibling itself is a
    /// site, and so is each call of a wrapper that makes two. The answer key
    /// is `tests/fixtures/sibling-wrapper/README.md`.
    #[test]
    fn a_call_into_a_sibling_inside_a_client_wrapper_is_a_site() {
        let Some((on, sites, labels)) = sibling_sites("sibling-wrapper") else {
            return;
        };
        let [app, client] = on.inputs.as_slice() else {
            panic!("two services");
        };
        let decided = decided_sites_with(app, &[client], &on.repo, &sites, &labels);
        let found: Vec<(String, u32, Option<String>)> = decided
            .sites
            .iter()
            .map(|site| {
                (
                    site.file
                        .trim_start_matches("services/app/src/")
                        .to_string(),
                    site.line,
                    site.name_text(),
                )
            })
            .collect();
        assert_eq!(
            found,
            vec![
                (
                    "api.ts".to_string(),
                    6,
                    Some("/api/items/${id}".to_string())
                ),
                ("api.ts".to_string(), 10, Some("/api/status".to_string())),
                ("api.ts".to_string(), 14, Some("/api/version".to_string())),
                ("api.ts".to_string(), 15, Some("/api/items/1".to_string())),
                (
                    "commands.ts".to_string(),
                    21,
                    Some("/api/health".to_string())
                ),
            ],
            "{:?}",
            decided.sites
        );
    }
}
