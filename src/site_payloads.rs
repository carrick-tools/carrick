//! The type capture reads a payload where the fact layer places it (phase 3,
//! step 1; part of carrick#2474).
//!
//! Each decided site's record carries a request slot and a response slot
//! ([`PayloadSlots`]): the expression that holds each payload, filled where
//! the site's own call or handler states it. With the switch on, every type
//! manifest entry at a site whose slot is filled is typed at that slot: its
//! infer request is built from the slot, and any request the model reader's
//! locator gave the same alias is dropped. An entry the reader stated with
//! no locator at all (carrick-cloud#2522) gets a typed side this way where it
//! was a placeholder. The sidecar still prints the type and the check still
//! judges it; only the place the type is read moves.
//!
//! - **Switch.** Nothing here runs unless `CARRICK_SITE_PAYLOADS=1`. Off,
//!   no record is built for it and no request moves, so every blob is
//!   byte-identical to a scan without this module.
//! - **Join.** A manifest entry joins the record whose call is on the same
//!   file and line, of the same protocol (an HTTP client's site for an HTTP
//!   entry), on the same side (a call for a consumer, a handler for a
//!   producer). A line with two such sites joins neither.
//! - **Which slot.** HTTP: the request entry reads the request slot and the
//!   response entry the response slot. Any other protocol states one payload,
//!   under its response entry: the request slot.
//! - **Not moved.** A symbol the reader named still publishes its alias (the
//!   capture takes symbol anchors first); a slot left empty leaves the
//!   reader's locator in place.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::cloud_storage::{ManifestRole, ManifestTypeKind, TypeManifestEntry};
use crate::operation::OperationKey;
use crate::request_summary::site_record::{PayloadAt, PayloadSlots, SiteRecord};
use crate::services::type_sidecar::{InferKind, InferRequestItem};

/// The environment variable that turns slot-anchored capture on.
pub const SWITCH: &str = "CARRICK_SITE_PAYLOADS";

/// Whether slot-anchored capture is on for this run.
pub fn enabled() -> bool {
    enabled_from(std::env::var(SWITCH).ok().as_deref())
}

/// Whether a value of [`SWITCH`] turns it on: `1` only.
pub fn enabled_from(value: Option<&str>) -> bool {
    value == Some("1")
}

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
    /// the sidecar's numbering.
    pub fn requests(
        &self,
        entries: &[TypeManifestEntry],
        repo_path: &str,
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
            if !seen.insert(entry.type_alias.clone()) {
                continue;
            }
            let at = slot.at();
            let source = sources
                .entry(at.file.clone())
                .or_insert_with(|| std::fs::read_to_string(root.join(&at.file)).ok());
            let Some(source) = source.as_deref() else {
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
            requests.push(InferRequestItem {
                file_path: root.join(&at.file).to_string_lossy().into_owned(),
                line_number: at.line,
                span_start: Some(crate::utils::sidecar_position(source, at.span_start)),
                span_end: Some(crate::utils::sidecar_position(source, at.span_end)),
                expression_text: None,
                expression_line: None,
                infer_kind,
                alias: Some(entry.type_alias.clone()),
                param_name,
                member,
                result: None,
            });
        }
        requests
    }
}

/// `infer` with every request for an alias `slotted` types dropped, and
/// `slotted` added after the rest. Empty `slotted` leaves `infer` as it was.
pub fn take_over(infer: &mut Vec<InferRequestItem>, slotted: &[InferRequestItem]) {
    if slotted.is_empty() {
        return;
    }
    let aliases: BTreeSet<&str> = slotted
        .iter()
        .filter_map(|request| request.alias.as_deref())
        .collect();
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
