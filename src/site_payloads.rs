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
use crate::services::type_sidecar::{InferKind, InferRequestItem, SymbolRequest, ThroughCall};

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

/// The request slot of an HTTP site that yielded to the request summaries'
/// rows (carrick#4260), and the rows whose own send it is.
#[derive(Debug, Clone)]
struct Carried {
    /// The request the site ends at: its file and line, repo-relative.
    request: (String, u32),
    /// The rows (repo-relative file and line) the site's send is.
    rows: Vec<(String, u32)>,
    request_slot: PayloadAt,
}

/// A summary row at a caller a yielded site's slot types: the manifest key
/// of its request entry, and the row's own call, which the slot is read
/// through.
#[derive(Debug, Clone)]
struct CarriedRow {
    file_path: String,
    line: u32,
    method: String,
    path: String,
    site: Site,
}

/// A service's payload slots by the file and line of each site's call.
#[derive(Debug, Clone, Default)]
pub struct SitePayloads {
    by_line: BTreeMap<(String, u32), Vec<Site>>,
    carried: Vec<Carried>,
    rows: Vec<CarriedRow>,
}

impl SitePayloads {
    /// Take the request slot of each site that yielded to summary rows
    /// (carrick#4260). `records` are the yielded sites' records, each with the
    /// index of its site in `yielded`
    /// ([`crate::request_summary::site_record::yielded_records`]). A site
    /// that yielded through a saved stop alone has no rows and hands on
    /// nothing; a site with no request slot (a verb that sends no body)
    /// hands on nothing. A yielded site's response slot is not carried.
    pub fn carry(
        &mut self,
        yielded: &[crate::request_summary::decided_sites::Yielded],
        records: Vec<(usize, SiteRecord)>,
    ) {
        for (index, record) in records {
            let Some(site) = yielded.get(index) else {
                continue;
            };
            let Some(slot) = record.payloads.request else {
                continue;
            };
            if site.rows.is_empty() {
                continue;
            }
            self.carried.push(Carried {
                request: (site.site.end.file.clone(), site.site.end.line),
                rows: site.rows.clone(),
                request_slot: slot,
            });
        }
    }

    /// The carried slots joined to the summary rows they type: each row at a
    /// caller the request summaries stated (`file_results`) whose request is
    /// the one a yielded site ends at, on the row's own line, and which has no
    /// typed request of its own (its function builds the body, or its body
    /// reading is none; a row that sends a parameter unchanged, or states
    /// its request line's types, keeps them). The row's request entry is
    /// keyed by its file, line, verb and canonical path, so another row on
    /// the same line does not join. A row two slots reach takes none.
    pub fn joined_to_rows(
        &self,
        file_results: &std::collections::HashMap<
            String,
            crate::agents::file_analyzer_agent::FileAnalysisResult,
        >,
        repo_path: &str,
        normalizer: &crate::url_normalizer::UrlNormalizer,
    ) -> Self {
        use crate::agents::file_analyzer_agent::ResolutionSource;
        use crate::forwarded_body::CallBody;
        let mut joined = self.clone();
        joined.rows.clear();
        if self.carried.is_empty() {
            return joined;
        }
        let root = Path::new(repo_path);
        for (file_path, result) in file_results {
            let file = crate::site_rows::relative(root, file_path);
            for call in &result.data_calls {
                if call.resolution_source != Some(ResolutionSource::RequestSummary)
                    || call.request_line.is_some()
                    || !matches!(call.call_body, Some(CallBody::Built) | None)
                {
                    continue;
                }
                let (Some(reaches), Some(start), Some(end), Some(method)) = (
                    call.reaches_request.as_deref(),
                    call.call_expression_span_start,
                    call.call_expression_span_end,
                    call.method.as_deref(),
                ) else {
                    continue;
                };
                let Some((request_file, request_line)) = reaches.rsplit_once(':') else {
                    continue;
                };
                let Ok(request_line) = request_line.parse::<u32>() else {
                    continue;
                };
                let request = (crate::site_rows::relative(root, request_file), request_line);
                let line = if call.line_number <= 0 {
                    1
                } else {
                    call.line_number as u32
                };
                let mut reaching = self.carried.iter().filter(|carried| {
                    carried.request == request && carried.rows.contains(&(file.clone(), line))
                });
                let (Some(carried), None) = (reaching.next(), reaching.next()) else {
                    continue;
                };
                joined.rows.push(CarriedRow {
                    file_path: file_path.clone(),
                    line,
                    method: crate::type_manifest::normalize_manifest_method(method),
                    path: crate::agents::file_orchestrator::FileOrchestrator::canonical_call_path(
                        normalizer, call,
                    ),
                    site: Site {
                        http: true,
                        side: Side::Call,
                        call: AnchorAt {
                            file: file.clone(),
                            line,
                            span_start: start,
                            span_end: end,
                        },
                        slots: PayloadSlots {
                            request: Some(carried.request_slot.clone()),
                            response: None,
                        },
                    },
                });
            }
        }
        // What the summaries' own request reading still decides (carrick#4260
        // R8): caller rows typed from a parameter's declaration. When this
        // reads zero over the fixed corpus, the request half of `CallBody` goes.
        let param_rows = file_results
            .values()
            .flat_map(|result| &result.data_calls)
            .filter(|call| {
                call.reaches_request.is_some() && matches!(call.call_body, Some(CallBody::Param(_)))
            })
            .count();
        tracing::debug!(
            carried = joined.carried.len(),
            rows = joined.rows.len(),
            param_rows,
            "site payloads: slots carried to summary rows, and caller rows still typed by a parameter's declaration"
        );
        joined
    }

    /// The carried row a consumer request `entry` is the entry of.
    fn carried_row(&self, entry: &TypeManifestEntry) -> Option<&CarriedRow> {
        if entry.role != ManifestRole::Consumer || entry.type_kind != ManifestTypeKind::Request {
            return None;
        }
        let (method, path) = entry.key.as_http()?;
        let mut matching = self.rows.iter().filter(|row| {
            row.file_path == entry.file_path
                && row.line == entry.line_number
                && row.method == method
                && row.path == path
        });
        match (matching.next(), matching.next()) {
            (Some(row), None) => Some(row),
            _ => None,
        }
    }

    /// A slot types `entry`: a request entry of a row a yielded site's send
    /// is, which [`drop_call_through_entries`](crate::engine) keeps.
    pub fn types_row(&self, entry: &TypeManifestEntry) -> bool {
        self.carried_row(entry).is_some()
    }

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
        Self {
            by_line,
            ..Self::default()
        }
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
            // A row a yielded site's send is takes that site's slot, read
            // through the row's own call (carrick#4260).
            let site = match self.carried_row(entry) {
                Some(row) => &row.site,
                None => {
                    let Some(sites) = self.by_line.get(&(file.clone(), entry.line_number)) else {
                        continue;
                    };
                    let mut joined = sites
                        .iter()
                        .filter(|site| site.http == http && site.side == side);
                    let (Some(site), None) = (joined.next(), joined.next()) else {
                        continue;
                    };
                    site
                }
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
            // An absent payload argument is stated for HTTP alone
            // (carrick#4165); the other protocols are carrick#4227.
            if matches!(slot, PayloadAt::Unwritten { .. } | PayloadAt::Absent { .. }) && !http {
                continue;
            }
            // A verb that sends no body states it already (carrick#4163):
            // the fact decides the verbs that can send one.
            if matches!(slot, PayloadAt::Absent { .. })
                && entry.key.as_http().is_some_and(|(method, _)| {
                    !crate::type_manifest::producer_reads_request_body(method)
                })
            {
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
                // No call writes the body argument: the scanner answers it
                // and the sidecar is never asked
                // ([`InferRequestItem::answered_no_body`], carrick#4165).
                PayloadAt::Absent { .. } => (InferKind::RequestConfig, None, None),
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

/// `explicit` with every model symbol dropped for an alias the scanner
/// answers as sending no body (carrick#4165,
/// [`InferRequestItem::answered_no_body`]): no call writes a body there, so
/// no symbol names one, and the answer is the alias's only one.
pub fn drop_answered_symbols(explicit: &mut Vec<SymbolRequest>, slotted: &[InferRequestItem]) {
    let answered: BTreeSet<&str> = slotted
        .iter()
        .filter(|request| request.answered_no_body())
        .filter_map(|request| request.alias.as_deref())
        .collect();
    if answered.is_empty() {
        return;
    }
    explicit.retain(|request| {
        request
            .alias
            .as_deref()
            .is_none_or(|alias| !answered.contains(alias))
    });
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
