//! The type capture reads a payload where the fact layer places it (phase 3,
//! step 1; part of carrick#2474).
//!
//! Each decided site's record carries a request slot and a response slot
//! ([`PayloadSlots`]): the expression that holds each payload, filled where
//! the site's own call or handler states it. Every type
//! manifest entry at a site whose slot is filled is typed at that slot: its
//! infer request is built from the slot, and any request the model reader's
//! locator gave the same alias is dropped. An entry the reader stated with
//! no locator at all (carrick-cloud#2522) gets a typed side this way where it
//! was a placeholder. The sidecar still prints the type and the check still
//! judges it; only the place the type is read moves.
//!
//! - **Join.** A manifest entry joins the record whose call is on the same
//!   file and line, of the same protocol (an HTTP client's site for an HTTP
//!   entry), on the same side (a call for a consumer, a handler for a
//!   producer). A line with two such sites joins neither.
//! - **Which slot.** HTTP: the request entry reads the request slot and the
//!   response entry the response slot. Any other protocol states one payload,
//!   under its response entry: the request slot.
//! - **A route handler's parameter is not its body** (carrick#4197). An HTTP
//!   handler is handed the server's request object, so its request entry is
//!   read as the body at the route's registration call instead, where the
//!   sidecar follows the handler to the member it reads off that object.
//! - **Not moved.** A symbol the reader named still publishes its alias (the
//!   capture takes symbol anchors first), unless the file's own imports and
//!   declarations do not name it (below); a slot left empty leaves the
//!   reader's locator in place.
//! - **Pub/sub, once a slot types the alias** (carrick#2472 C3). The reader's
//!   payload locator, whose text the envelope guard tests for the topic, is
//!   not consulted: no borrow witness is computed from it, and a symbol the
//!   file does not import or declare (the borrow the guard blocked from being
//!   demoted to inference) asks nothing, so the slot decides. An alias no slot
//!   types keeps the guard.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use crate::cloud_storage::{ManifestRole, ManifestTypeKind, TypeManifestEntry};
use crate::operation::OperationKey;
use crate::request_summary::site_record::{AnchorAt, PayloadAt, PayloadSlots, SiteRecord};
use crate::services::type_sidecar::{InferKind, InferRequestItem, ThroughCall};

/// Which side of an operation a site is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Side {
    Call,
    Handler,
}

/// One site's slots, with what the join compares.
#[derive(Debug, Clone)]
struct Site {
    http: bool,
    side: Side,
    /// The site's own call.
    call: AnchorAt,
    slots: PayloadSlots,
}

/// A service's payload slots by the file and line of each site's call.
#[derive(Debug, Clone, Default)]
pub struct SitePayloads {
    by_line: BTreeMap<(String, u32), Vec<Site>>,
}

impl SitePayloads {
    /// The slots of `records`, each under its call's file and line.
    pub fn of(records: &[SiteRecord]) -> Self {
        let mut by_line: BTreeMap<(String, u32), Vec<Site>> = BTreeMap::new();
        for record in records {
            let side = if record.anchors.handler.is_some() {
                Side::Handler
            } else {
                Side::Call
            };
            let http = record
                .state
                .libraries
                .as_array()
                .and_then(|libraries| libraries.first())
                .and_then(|library| library["labels"].as_array())
                .and_then(|labels| labels.first())
                .and_then(|label| label["role"].as_str())
                .is_some_and(|role| role == "http_client" || role == "server_framework");
            by_line
                .entry((record.anchors.call.file.clone(), record.anchors.call.line))
                .or_default()
                .push(Site {
                    http,
                    side,
                    call: record.anchors.call.clone(),
                    slots: record.payloads.clone(),
                });
        }
        Self { by_line }
    }

    pub fn is_empty(&self) -> bool {
        self.by_line.is_empty()
    }

    /// The infer request each of `entries` takes from its site's slot, in
    /// the entries' order. Files are joined relative to `repo_path`; a file
    /// that cannot be read gives no request, since its span cannot be put in
    /// the sidecar's numbering. A consumer response entry at a caller row in
    /// `awaiting_body` takes no request: the answer for its own call decides
    /// whether it stays (carrick#4213 T0).
    pub fn requests(
        &self,
        entries: &[TypeManifestEntry],
        repo_path: &str,
        results: &crate::result_shapes::ResultIndex,
        awaiting_body: &HashSet<(&str, u32, String)>,
    ) -> Vec<InferRequestItem> {
        let root = Path::new(repo_path);
        let mut sources: BTreeMap<String, Option<String>> = BTreeMap::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut requests = Vec::new();
        for entry in entries {
            let file = crate::site_rows::relative(root, &entry.file_path);
            let http = matches!(entry.key, OperationKey::Http { .. });
            let side = match entry.role {
                ManifestRole::Consumer => Side::Call,
                ManifestRole::Producer => Side::Handler,
            };
            if entry.role == ManifestRole::Consumer
                && entry.type_kind == ManifestTypeKind::Response
                && entry.key.as_http().is_some_and(|(method, _)| {
                    awaiting_body.contains(&(
                        entry.file_path.as_str(),
                        entry.line_number,
                        method.to_string(),
                    ))
                })
            {
                continue;
            }
            let Some(sites) = self.by_line.get(&(file.clone(), entry.line_number)) else {
                continue;
            };
            let mut joined = sites
                .iter()
                .filter(|site| site.http == http && site.side == side);
            let (Some(site), None) = (joined.next(), joined.next()) else {
                continue;
            };
            let slot = match (http, entry.type_kind) {
                (true, ManifestTypeKind::Request) | (false, ManifestTypeKind::Response) => {
                    site.slots.request.as_ref()
                }
                (true, ManifestTypeKind::Response) => site.slots.response.as_ref(),
                (false, ManifestTypeKind::Request) => None,
            };
            let Some(slot) = slot else {
                continue;
            };
            // The question is about a request's options: no other protocol's
            // payload is asked it, so the reader's request stands there.
            if matches!(slot, PayloadAt::Unwritten { .. }) && !http {
                continue;
            }
            // A call whose verb sends no body is not asked the object its
            // wrapper builds (carrick#4128): the same verbs a producer is
            // never asked a request body for (carrick#4159).
            if entry.type_kind == ManifestTypeKind::Request
                && matches!(slot, PayloadAt::Value(at) if outside(at, &site.call))
                && entry.key.as_http().is_some_and(|(method, _)| {
                    !crate::type_manifest::producer_reads_request_body(method)
                })
            {
                continue;
            }
            if !seen.insert(entry.type_alias.clone()) {
                continue;
            }
            // carrick#4197: an HTTP route handler's parameter is the request
            // object the server hands it (`req`, a context), never the body.
            // The body is what the handler reads off it, and the request read
            // at the registration call follows the handler to that read, or
            // decides the route takes none. Another protocol's handler is run
            // with its payload, so its parameter stays the slot.
            // The request asked at the call is marked: where the sidecar
            // gives it no answer, its span names no payload (carrick#4204).
            let registration;
            let (slot, at_registration) = match slot {
                PayloadAt::Param { .. } if http => {
                    registration = PayloadAt::Value(site.call.clone());
                    (&registration, true)
                }
                _ => (slot, false),
            };
            let at = slot.at();
            let Some((span_start, span_end)) =
                source_of(root, &at.file, &mut sources).map(|source| {
                    (
                        crate::utils::sidecar_position(source, at.span_start),
                        crate::utils::sidecar_position(source, at.span_end),
                    )
                })
            else {
                continue;
            };
            let (infer_kind, param_name, member) = match slot {
                PayloadAt::Value(_) if http => (InferKind::RequestBody, None, None),
                PayloadAt::Value(_) => (InferKind::Expression, None, None),
                PayloadAt::Result(_) => (InferKind::CallResult, None, None),
                PayloadAt::Param { name, .. } => {
                    (InferKind::FunctionParam, Some(name.clone()), None)
                }
                // The options literal states the key absent: ask the
                // sidecar to confirm it (carrick#2674 P3-1a).
                PayloadAt::Unwritten { key, .. } => {
                    (InferKind::RequestConfig, None, Some(key.clone()))
                }
            };
            // A call's result takes the store's label for the call it reads
            // (carrick#2910): the request replaces the reader's and the site
            // row's, which carry it.
            let result = match slot {
                PayloadAt::Result(at) => results.at_anchor(at, repo_path),
                _ => None,
            };
            // The label's check died unanswered (carrick#4154).
            let result_unanswered = match slot {
                PayloadAt::Result(at) => results.unanswered_at_anchor(at, repo_path),
                _ => false,
            };
            // A value read outside the site's own call (the body a wrapper
            // builds, carrick#4128) names the call: a type parameter of the
            // wrapper is read as that call instantiates it.
            let through_call = match slot {
                PayloadAt::Value(_) if outside(at, &site.call) => {
                    through(root, &site.call, &mut sources)
                }
                _ => None,
            };
            requests.push(InferRequestItem {
                file_path: root.join(&at.file).to_string_lossy().into_owned(),
                line_number: at.line,
                span_start: Some(span_start),
                span_end: Some(span_end),
                expression_text: None,
                expression_line: None,
                infer_kind,
                alias: Some(entry.type_alias.clone()),
                param_name,
                member,
                result,
                route_path: None,
                at_registration,
                result_unanswered,
                through_call,
            });
        }
        requests
    }
}

/// `file`'s text under `root`, read once into `sources`.
fn source_of<'a>(
    root: &Path,
    file: &str,
    sources: &'a mut BTreeMap<String, Option<String>>,
) -> Option<&'a str> {
    sources
        .entry(file.to_string())
        .or_insert_with(|| std::fs::read_to_string(root.join(file)).ok())
        .as_deref()
}

/// `at` lies outside `call`: in another file, or not within its span.
fn outside(at: &AnchorAt, call: &AnchorAt) -> bool {
    at.file != call.file || at.span_start < call.span_start || at.span_end > call.span_end
}

/// `call` as the sidecar numbers it; `None` where its file cannot be read.
fn through(
    root: &Path,
    call: &AnchorAt,
    sources: &mut BTreeMap<String, Option<String>>,
) -> Option<ThroughCall> {
    let source = source_of(root, &call.file, sources)?;
    Some(ThroughCall {
        file_path: root.join(&call.file).to_string_lossy().into_owned(),
        span_start: crate::utils::sidecar_position(source, call.span_start),
        span_end: crate::utils::sidecar_position(source, call.span_end),
    })
}

/// The aliases `slotted` types: the manifest entries whose payload the
/// fact layer places, which the model reader's locator and symbol no longer
/// decide (carrick#2472 C3).
pub fn slotted_aliases(slotted: &[InferRequestItem]) -> BTreeSet<String> {
    slotted
        .iter()
        .filter_map(|request| request.alias.clone())
        .collect()
}

/// `infer` with every request for an alias `slotted` types dropped, and
/// `slotted` added after the rest. Empty `slotted` leaves `infer` as it was.
pub fn take_over(infer: &mut Vec<InferRequestItem>, slotted: &[InferRequestItem]) {
    if slotted.is_empty() {
        return;
    }
    let aliases = slotted_aliases(slotted);
    infer.retain(|request| {
        request
            .alias
            .as_deref()
            .is_none_or(|alias| !aliases.contains(alias))
    });
    infer.extend_from_slice(slotted);
}

#[cfg(test)]
mod tests;
