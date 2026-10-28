//! Dispatch rows (carrick#2474 R4b T3b): the handler records the receiver
//! trace builds ([`crate::request_summary::dispatch::handler_records`]),
//! asked through [`crate::site_questions::ask_handlers_service`], become HTTP
//! endpoint rows at their comparison's line, or possible calls.
//!
//! - **The method is the code's fact** (ruling of 2026-10-23 on the live
//!   test): a row's method is the one the record forces, never the model's.
//!   An answer is read for its choice alone (`route`, `none`, `cant_tell`);
//!   its `method` and `path` are never read. A route the code chooses no
//!   method for is a possible call (`no_method`), never an `ALL` row.
//! - **The path** is the compared literal, as written.
//! - **Rows only where the compared path is the served path**
//!   ([`ComparisonPlace::served`]); elsewhere a possible call
//!   (`owner_unknown`) until a route's prefix is composed (R3).
//! - **Provenance:** a comparison the code settles whole (path and method)
//!   is a fact (`library_claim`); an answered one is `model` with its
//!   [`SiteInference`], in band at [`BAND`] with both orders agreeing.
//! - **Possible calls**, with today's reasons only: no answer
//!   (`unanswered`), `cant_tell` or any answer below the band
//!   (`below_band`), method unstated (`no_method`), a path the handler does
//!   not see whole (`owner_unknown`). `none` in band states nothing.
//! - **The fold** ([`super::fold_rows`]): a dispatch row also replaces a
//!   reader endpoint row in its handler with the same key, at another line
//!   ([`SiteRow::within`]); a reader row of another route the handler
//!   serves (`/api/:id` beside `/api/one`) stands.
//!
//! Plan: carrick#2386 comment 6341789125, sections 1-2; ruling:
//! carrick#2474 comment 6358002149.

use std::collections::{BTreeMap, HashSet};

use super::{BAND, InferenceBy, RowSide, SiteInference, SiteRow};
use crate::agents::file_analyzer_agent::ResolutionSource;
use crate::operation::OperationKey;
use crate::possible_calls::{LabelledEndRef, PossibleCall, Unread, UnreadReason};
use crate::request_summary::dispatch::{ComparisonPlace, HandlerRecords};
use crate::request_summary::site_record::{AnchorAt, TypeAnchors};
use crate::site_questions::{
    HandlerAnswers, HandlerQuestion, HandlerRecord, MAX_ASKED_COMPARISONS, RouteAnswer,
    RouteChoice, SubjectKind,
};

/// A record's site id: its kind, subject and key. A record asking more than
/// [`MAX_ASKED_COMPARISONS`] goes in parts, `#1`, `#2`, ….
fn record_id(record: &HandlerRecord) -> String {
    let kind = match record.subject.kind {
        SubjectKind::Entry => "entry",
        SubjectKind::Label => "label",
        SubjectKind::Shared => "shared",
    };
    format!(
        "handler:{kind}:{}:{}",
        record.subject.at, record.subject.key
    )
}

/// The site ids `record` is asked under, with the ids each part asks.
fn parts(record: &HandlerRecord) -> Vec<(String, Vec<u32>)> {
    let id = record_id(record);
    let chunks: Vec<&[u32]> = record.asked.chunks(MAX_ASKED_COMPARISONS).collect();
    match chunks[..] {
        [] => Vec::new(),
        [only] => vec![(id, only.to_vec())],
        _ => chunks
            .iter()
            .enumerate()
            .map(|(part, ids)| (format!("{id}#{}", part + 1), ids.to_vec()))
            .collect(),
    }
}

/// The questions a service's handler records ask: one site per record that
/// asks a comparison, a larger one in parts with the same record and its
/// `forced` cut to the part's ids.
pub fn handler_questions(records: &HandlerRecords) -> Vec<HandlerQuestion> {
    let mut out = Vec::new();
    for record in &records.records {
        for (site_id, asked) in parts(record) {
            let mut handler = record.clone();
            handler
                .forced
                .retain(|id, _| asked.iter().any(|asked| asked.to_string() == *id));
            handler.asked = asked;
            out.push(HandlerQuestion { site_id, handler });
        }
    }
    out
}

/// What the dispatch rows came to: rows, possible calls, and counts.
#[derive(Debug, Default)]
pub struct DispatchRows {
    pub rows: Vec<SiteRow>,
    pub leads: Vec<PossibleCall>,
    /// `rows_fact`, `rows_model`, `rows_below_band`, `not_a_route`, `records_unanswered`.
    pub counts: BTreeMap<String, usize>,
}

fn count(counts: &mut BTreeMap<String, usize>, what: &str) {
    *counts.entry(what.to_string()).or_default() += 1;
}

/// The possible call a comparison is when it states no row.
fn lead(place: &ComparisonPlace, reason: UnreadReason) -> PossibleCall {
    PossibleCall {
        file_path: place.file.clone(),
        line_number: place.line,
        span_start: place.span_start,
        span_end: place.span_end,
        kind: Some("http".to_string()),
        role: Some("producer".to_string()),
        labelled_end: place.label.as_ref().map(|label| LabelledEndRef {
            package: label.package.clone(),
            export: label.export.clone(),
            member: label.member.clone(),
        }),
        unread: Unread {
            slot: "path".to_string(),
            reason,
        },
        trace_depth: 0,
    }
}

/// What a comparison states: `T`, or the possible call it is instead.
type Stated<T> = Result<T, Box<PossibleCall>>;

/// [`lead`] as what a comparison states instead of a row.
fn instead(place: &ComparisonPlace, reason: UnreadReason) -> Box<PossibleCall> {
    Box::new(lead(place, reason))
}

/// The row a comparison states with `method`, an HTTP method the code
/// compared.
fn row(
    place: &ComparisonPlace,
    method: &str,
    path: &str,
    inference: Option<SiteInference>,
) -> SiteRow {
    let in_band = inference
        .as_ref()
        .is_none_or(|answer| answer.confidence >= BAND && answer.orders_agree);
    SiteRow {
        file: place.file.clone(),
        line: place.line,
        key: OperationKey::http(method, path),
        side: RowSide::Endpoint,
        source: match inference {
            None => ResolutionSource::LibraryClaim,
            Some(_) => ResolutionSource::Model,
        },
        inference,
        in_band,
        name_scope: None,
        // A dispatch row requests no type: its record carries no anchors
        // but its comparison.
        anchors: TypeAnchors {
            call: AnchorAt {
                file: place.file.clone(),
                line: place.line,
                span_start: place.span_start,
                span_end: place.span_end,
            },
            payload: None,
            payload_at: None,
            handler: None,
            key: None,
        },
        lead: lead(place, UnreadReason::BelowBand),
        needs_reader: false,
        through: false,
        within: place.within.clone(),
    }
}

/// The rows and possible calls `records` come to with `answers`.
pub fn dispatch_rows(records: &HandlerRecords, answers: &HandlerAnswers) -> DispatchRows {
    let mut out = DispatchRows::default();
    let mut stated: HashSet<(String, u32, OperationKey)> = HashSet::new();
    for (index, record) in records.records.iter().enumerate() {
        let settled = &records.settled[index];
        let places = &records.places[index];
        // Every part's answers, by comparison id; a part with no answer
        // leaves its ids unanswered.
        let mut answered: BTreeMap<u32, &RouteAnswer> = BTreeMap::new();
        for (site_id, _) in parts(record) {
            match answers.answered.get(&site_id) {
                Some(routes) => {
                    for route in &routes.routes {
                        answered.insert(route.id, route);
                    }
                }
                None => count(&mut out.counts, "records_unanswered"),
            }
        }
        for place in places {
            let id = place.id.to_string();
            let path = record
                .comparisons
                .iter()
                .find(|comparison| comparison.id == place.id)
                .map(|comparison| comparison.literal.as_str())
                .unwrap_or_default();
            let stated_row = if record.asked.contains(&place.id) {
                let method = record
                    .forced
                    .get(&id)
                    .and_then(|forced| forced.method.as_deref());
                asked(
                    place,
                    method,
                    path,
                    answered.get(&place.id).copied(),
                    answers,
                    &mut out,
                )
            } else {
                let forced = settled.get(&id).cloned().unwrap_or_default();
                let path = forced.path.as_deref().unwrap_or(path);
                match (place.served, forced.method.as_deref()) {
                    (false, _) => Err(instead(place, UnreadReason::OwnerUnknown)),
                    (true, None) => Err(instead(place, UnreadReason::NoMethod)),
                    (true, Some(method)) => Ok(Some(row(place, method, path, None))),
                }
            };
            match stated_row {
                Ok(Some(site_row)) => {
                    if stated.insert((site_row.file.clone(), site_row.line, site_row.key.clone())) {
                        // A row out of band ends as a possible call or an
                        // alternative once folded, so it is not a model row.
                        let what = match site_row.inference {
                            None => "rows_fact",
                            Some(_) if site_row.in_band => "rows_model",
                            Some(_) => "rows_below_band",
                        };
                        count(&mut out.counts, what);
                        out.rows.push(site_row);
                    }
                }
                Ok(None) => {}
                Err(possible) => out.leads.push(*possible),
            }
        }
    }
    out
}

/// An asked comparison's row, nothing, or the possible call it is. Only
/// the answer's choice is read; the method is `method`, the code's.
fn asked(
    place: &ComparisonPlace,
    method: Option<&str>,
    path: &str,
    answer: Option<&RouteAnswer>,
    answers: &HandlerAnswers,
    out: &mut DispatchRows,
) -> Stated<Option<SiteRow>> {
    let Some(answer) = answer else {
        return Err(instead(place, UnreadReason::Unanswered));
    };
    let in_band = answer.confidence >= BAND && answer.orders_agree;
    match answer.choice {
        RouteChoice::None if in_band => {
            count(&mut out.counts, "not_a_route");
            Ok(None)
        }
        RouteChoice::None | RouteChoice::CantTell => Err(instead(place, UnreadReason::BelowBand)),
        RouteChoice::Route => match (place.served, method) {
            (true, Some(method)) => Ok(Some(row(
                place,
                method,
                path,
                Some(SiteInference {
                    by: InferenceBy::SiteQuestion,
                    question_set: answers.question_set.clone(),
                    confidence: answer.confidence,
                    orders_agree: answer.orders_agree,
                }),
            ))),
            _ if !in_band => Err(instead(place, UnreadReason::BelowBand)),
            (false, _) => Err(instead(place, UnreadReason::OwnerUnknown)),
            (true, None) => Err(instead(place, UnreadReason::NoMethod)),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;
    use crate::analyzer::ApiEndpointDetails;
    use crate::request_summary::dispatch::LineRange;
    use crate::site_questions::{
        ComparisonPosition, ForcedRoute, HandlerComparison, HandlerSubject, RoutesAnswered,
        Unanswered,
    };
    use crate::site_rows::{SiteRows, fold_rows};

    const FILE: &str = "src/handler.ts";

    /// A comparison of `literal` at `FILE:line`.
    fn comparison(id: u32, line: u32, literal: &str) -> HandlerComparison {
        HandlerComparison {
            id,
            at: format!("{FILE}:{line}"),
            literal: literal.to_string(),
            position: ComparisonPosition::Equality,
            expression: "path".to_string(),
            read: None,
            hole: None,
        }
    }

    /// One entry handler on lines 10-30 of `FILE`, comparing each
    /// `(line, literal, forced method)`: asked, the method forced where the
    /// code qualifies one. `served` for every comparison.
    fn handler(compared: &[(u32, &str, Option<&str>)], served: bool) -> HandlerRecords {
        let comparisons: Vec<HandlerComparison> = compared
            .iter()
            .enumerate()
            .map(|(id, (line, literal, _))| comparison(id as u32, *line, literal))
            .collect();
        let forced = compared
            .iter()
            .enumerate()
            .filter_map(|(id, (_, _, method))| {
                Some((
                    id.to_string(),
                    ForcedRoute {
                        path: None,
                        method: Some(method.as_ref()?.to_string()),
                    },
                ))
            })
            .collect();
        let places = compared
            .iter()
            .enumerate()
            .map(|(id, (line, _, _))| ComparisonPlace {
                id: id as u32,
                file: FILE.to_string(),
                line: *line,
                span_start: line * 100,
                span_end: line * 100 + 10,
                served,
                label: None,
                within: vec![LineRange {
                    file: FILE.to_string(),
                    first: 10,
                    last: 30,
                }],
            })
            .collect();
        HandlerRecords {
            records: vec![HandlerRecord {
                subject: HandlerSubject {
                    at: format!("{FILE}:10"),
                    kind: SubjectKind::Entry,
                    key: "handle".to_string(),
                    params: vec!["request".to_string()],
                    code: String::new(),
                    label: None,
                    handlers: None,
                    handler_count: None,
                },
                code: Vec::new(),
                comparisons,
                methods: Vec::new(),
                scopes: Vec::new(),
                asked: (0..compared.len() as u32).collect(),
                forced,
            }],
            settled: vec![BTreeMap::new()],
            places: vec![places],
            ..HandlerRecords::default()
        }
    }

    fn answer(id: u32, choice: RouteChoice, method: Option<&str>, confidence: f64) -> RouteAnswer {
        RouteAnswer {
            id,
            choice,
            method: method.map(str::to_string),
            path: None,
            confidence,
            orders_agree: true,
        }
    }

    /// `routes` as the answer to `records`' one record.
    fn answered(records: &HandlerRecords, routes: Vec<RouteAnswer>) -> HandlerAnswers {
        let mut answers = HandlerAnswers {
            question_set: Some("qs-h".to_string()),
            ..HandlerAnswers::default()
        };
        answers
            .answered
            .insert(record_id(&records.records[0]), RoutesAnswered { routes });
        answers
    }

    fn reasons(rows: &DispatchRows) -> Vec<(u32, UnreadReason)> {
        rows.leads
            .iter()
            .map(|lead| (lead.line_number, lead.unread.reason))
            .collect()
    }

    fn keys(rows: &DispatchRows) -> Vec<(u32, OperationKey, bool)> {
        rows.rows
            .iter()
            .map(|row| (row.line, row.key.clone(), row.in_band))
            .collect()
    }

    /// In band, a route answer with the code's method is a `model` row at
    /// its comparison's line. Below the band it is a row out of band (the
    /// fold makes it a possible call or an alternative). `cant_tell` and a
    /// `none` below the band are possible calls `below_band`; `none` in band
    /// states nothing; a route with no method the code chooses is
    /// `no_method`; a record with no answer leaves its comparisons
    /// `unanswered`.
    #[test]
    fn dispatch_answers_become_rows_at_their_comparison() {
        let records = handler(
            &[
                (12, "/one", Some("GET")),
                (14, "/two", Some("POST")),
                (16, "/three", Some("GET")),
                (18, "/four", None),
                (20, "/five", Some("GET")),
                (22, "/six", Some("GET")),
            ],
            true,
        );
        let answers = answered(
            &records,
            vec![
                answer(0, RouteChoice::Route, Some("GET"), 0.95),
                answer(1, RouteChoice::Route, Some("POST"), 0.6),
                answer(2, RouteChoice::CantTell, None, 0.9),
                answer(3, RouteChoice::Route, None, 0.95),
                answer(4, RouteChoice::None, None, 0.9),
                answer(5, RouteChoice::None, None, 0.4),
            ],
        );
        let rows = dispatch_rows(&records, &answers);
        assert_eq!(
            keys(&rows),
            vec![
                (12, OperationKey::http("GET", "/one"), true),
                (14, OperationKey::http("POST", "/two"), false),
            ]
        );
        assert!(
            rows.rows
                .iter()
                .all(|row| row.source == ResolutionSource::Model
                    && row.side == RowSide::Endpoint
                    && row.inference.as_ref().unwrap().question_set.as_deref() == Some("qs-h"))
        );
        assert_eq!(
            reasons(&rows),
            vec![
                (16, UnreadReason::BelowBand),
                (18, UnreadReason::NoMethod),
                (22, UnreadReason::BelowBand),
            ]
        );
        assert_eq!(rows.counts.get("not_a_route"), Some(&1));
        assert_eq!(rows.counts.get("rows_model"), Some(&1));
        assert_eq!(rows.counts.get("rows_below_band"), Some(&1));
        let lead = &rows.leads[0];
        assert_eq!(lead.kind.as_deref(), Some("http"));
        assert_eq!(lead.role.as_deref(), Some("producer"));
        assert_eq!(lead.unread.slot, "path");
        // No answer: every comparison unanswered, no row.
        let rows = dispatch_rows(&records, &HandlerAnswers::default());
        assert!(rows.rows.is_empty());
        assert!(
            rows.leads
                .iter()
                .all(|lead| lead.unread.reason == UnreadReason::Unanswered)
        );
        assert_eq!(rows.leads.len(), 6);
    }

    /// The method is the code's fact (ruling of 2026-10-23): an answer's
    /// method is never read. A route the code forces `GET` for is a `GET`
    /// row whatever method the answer names; a route the code chooses no
    /// method for is `no_method`, even when the answer names one in band.
    #[test]
    fn the_model_never_names_the_method() {
        let records = handler(&[(12, "/one", Some("GET")), (14, "/two", None)], true);
        let answers = answered(
            &records,
            vec![
                answer(0, RouteChoice::Route, Some("POST"), 0.99),
                answer(1, RouteChoice::Route, Some("GET"), 0.99),
            ],
        );
        let rows = dispatch_rows(&records, &answers);
        assert_eq!(
            keys(&rows),
            vec![(12, OperationKey::http("GET", "/one"), true)]
        );
        assert_eq!(reasons(&rows), vec![(14, UnreadReason::NoMethod)]);
    }

    /// A route whose compared path is not the path served (a handler
    /// registered under a path, or a function written inside it and handed
    /// on) is a possible call `owner_unknown`, whatever the answer.
    #[test]
    fn a_route_under_a_path_slot_is_a_lead() {
        let records = handler(&[(12, "/stats", Some("GET"))], false);
        let answers = answered(&records, vec![answer(0, RouteChoice::Route, None, 0.99)]);
        let rows = dispatch_rows(&records, &answers);
        assert!(rows.rows.is_empty());
        assert_eq!(reasons(&rows), vec![(12, UnreadReason::OwnerUnknown)]);
        // Settled by the code (path and method forced, nothing asked): the
        // same.
        let mut settled = records.clone_settled();
        let rows = dispatch_rows(&settled, &HandlerAnswers::default());
        assert!(rows.rows.is_empty());
        assert_eq!(reasons(&rows), vec![(12, UnreadReason::OwnerUnknown)]);
        settled.places[0][0].served = true;
        let rows = dispatch_rows(&settled, &HandlerAnswers::default());
        assert_eq!(
            keys(&rows),
            vec![(12, OperationKey::http("GET", "/stats"), true)]
        );
        assert_eq!(rows.rows[0].source, ResolutionSource::LibraryClaim);
        assert!(rows.rows[0].inference.is_none());
    }

    impl HandlerRecords {
        /// The record with every comparison settled: path and method forced,
        /// nothing asked.
        fn clone_settled(&self) -> HandlerRecords {
            let record = &self.records[0];
            let settled = record
                .comparisons
                .iter()
                .map(|comparison| {
                    let id = comparison.id.to_string();
                    (
                        id.clone(),
                        ForcedRoute {
                            path: Some(comparison.literal.clone()),
                            method: record.forced.get(&id).and_then(|f| f.method.clone()),
                        },
                    )
                })
                .collect();
            let mut record = record.clone();
            record.asked.clear();
            record.forced.clear();
            HandlerRecords {
                records: vec![record],
                settled: vec![settled],
                places: self.places.clone(),
                ..HandlerRecords::default()
            }
        }
    }

    fn reader(key: OperationKey, line: u32) -> ApiEndpointDetails {
        let mut row: ApiEndpointDetails = serde_json::from_value(json!({
            "owner": null,
            "key": key,
            "params": [],
            "request_body": null,
            "response_body": null,
            "handler_name": null,
            "request_type": null,
            "response_type": null,
            "file_path": format!("{FILE}:{line}"),
        }))
        .unwrap();
        row.resolution_source = Some(ResolutionSource::Model);
        row
    }

    fn make(key: OperationKey, file: &Path, line: u32, _: RowSide) -> ApiEndpointDetails {
        let mut row = reader(key, line);
        row.file_path = format!("{}:{line}", file.display()).into();
        row.resolution_source = None;
        row
    }

    /// carrick#2139's shape: the reader states both routes of one handler
    /// on its first line. In band, each dispatch row replaces the reader row
    /// in its handler that agrees on its key, at the other line; a reader
    /// row that agrees on no dispatch row, and one outside the handler,
    /// stand.
    #[test]
    fn a_dispatch_row_replaces_an_agreeing_reader_row_in_its_handler() {
        let records = handler(
            &[
                (12, "/api/one", Some("GET")),
                (20, "/api/two", Some("POST")),
            ],
            true,
        );
        let answers = answered(
            &records,
            vec![
                answer(0, RouteChoice::Route, None, 0.98),
                answer(1, RouteChoice::Route, None, 0.98),
            ],
        );
        let dispatch = dispatch_rows(&records, &answers);
        let site = SiteRows {
            enabled: true,
            rows: dispatch.rows,
            ..SiteRows::default()
        };
        let mut endpoints = vec![
            reader(OperationKey::http("GET", "/api/one"), 10),
            reader(OperationKey::http("POST", "/api/two"), 10),
            reader(OperationKey::http("GET", "/api/other"), 10),
            reader(OperationKey::http("GET", "/api/one"), 40),
        ];
        let fold = fold_rows(&mut Vec::new(), &mut endpoints, &site, Path::new("."), make);
        let mut left: Vec<(String, OperationKey)> = endpoints
            .iter()
            .map(|row| {
                (
                    row.file_path.to_string_lossy().into_owned(),
                    row.key.clone(),
                )
            })
            .collect();
        left.sort_by_key(|(file, _)| file.clone());
        assert_eq!(
            left,
            vec![
                (
                    format!("{FILE}:10"),
                    OperationKey::http("GET", "/api/other")
                ),
                (format!("{FILE}:12"), OperationKey::http("GET", "/api/one")),
                (format!("{FILE}:20"), OperationKey::http("POST", "/api/two")),
                (format!("{FILE}:40"), OperationKey::http("GET", "/api/one")),
            ]
        );
        // The manifest drops the reader rows where they stood.
        let mut withdrawn: Vec<(String, u32, OperationKey)> = fold
            .withdrawn
            .iter()
            .map(|(file, line, key, _)| (file.clone(), *line, key.clone()))
            .collect();
        withdrawn.sort_by_key(|(_, line, key)| (*line, format!("{key:?}")));
        assert_eq!(
            withdrawn,
            vec![
                (FILE.to_string(), 10, OperationKey::http("GET", "/api/one")),
                (FILE.to_string(), 10, OperationKey::http("POST", "/api/two")),
            ]
        );
        // Below the band nothing is withdrawn.
        let low = answered(
            &records,
            vec![
                answer(0, RouteChoice::Route, None, 0.5),
                answer(1, RouteChoice::Route, None, 0.5),
            ],
        );
        let site = SiteRows {
            enabled: true,
            rows: dispatch_rows(&records, &low).rows,
            ..SiteRows::default()
        };
        let mut endpoints = vec![
            reader(OperationKey::http("GET", "/api/one"), 10),
            reader(OperationKey::http("POST", "/api/two"), 10),
        ];
        let fold = fold_rows(&mut Vec::new(), &mut endpoints, &site, Path::new("."), make);
        assert_eq!(endpoints.len(), 2);
        assert!(fold.withdrawn.is_empty());
    }

    /// carrick#2693: the fold takes a reader row of the dispatch row's own
    /// key only. A reader row of a parameter or wildcard route in the same
    /// handler (`GET /api/:id`, `GET /api/*` beside a compared `GET
    /// /api/one`) is another route the handler serves, and stands.
    #[test]
    fn a_dispatch_row_folds_only_its_own_key() {
        let records = handler(&[(12, "/api/one", Some("GET"))], true);
        let answers = answered(&records, vec![answer(0, RouteChoice::Route, None, 0.98)]);
        let site = SiteRows {
            enabled: true,
            rows: dispatch_rows(&records, &answers).rows,
            ..SiteRows::default()
        };
        let mut endpoints = vec![
            reader(OperationKey::http("GET", "/api/one"), 10),
            reader(OperationKey::http("GET", "/api/:id"), 10),
            reader(OperationKey::http("GET", "/api/*"), 11),
        ];
        let fold = fold_rows(&mut Vec::new(), &mut endpoints, &site, Path::new("."), make);
        let mut left: Vec<(String, OperationKey)> = endpoints
            .iter()
            .map(|row| {
                (
                    row.file_path.to_string_lossy().into_owned(),
                    row.key.clone(),
                )
            })
            .collect();
        left.sort_by_key(|(file, key)| (file.clone(), format!("{key:?}")));
        assert_eq!(
            left,
            vec![
                (format!("{FILE}:10"), OperationKey::http("GET", "/api/:id")),
                (format!("{FILE}:11"), OperationKey::http("GET", "/api/*")),
                (format!("{FILE}:12"), OperationKey::http("GET", "/api/one")),
            ]
        );
        let withdrawn: Vec<(u32, OperationKey)> = fold
            .withdrawn
            .iter()
            .map(|(_, line, key, _)| (*line, key.clone()))
            .collect();
        assert_eq!(withdrawn, vec![(10, OperationKey::http("GET", "/api/one"))]);
    }

    /// A record asking more than [`MAX_ASKED_COMPARISONS`] goes in parts
    /// under ids of their own, each part's `forced` cut to its ids, and the
    /// parts' answers read together.
    #[test]
    fn a_large_record_is_asked_in_parts() {
        let compared: Vec<(u32, String, Option<&str>)> = (0..20)
            .map(|n| (11 + n, format!("/r{n}"), Some("GET")))
            .collect();
        let borrowed: Vec<(u32, &str, Option<&str>)> = compared
            .iter()
            .map(|(line, literal, method)| (*line, literal.as_str(), *method))
            .collect();
        let records = handler(&borrowed, true);
        let questions = handler_questions(&records);
        assert_eq!(questions.len(), 2);
        let id = record_id(&records.records[0]);
        assert_eq!(questions[0].site_id, format!("{id}#1"));
        assert_eq!(questions[1].site_id, format!("{id}#2"));
        assert_eq!(questions[0].handler.asked.len(), MAX_ASKED_COMPARISONS);
        assert_eq!(questions[1].handler.asked, vec![16, 17, 18, 19]);
        assert!(
            questions[1]
                .handler
                .forced
                .keys()
                .all(|id| ["16", "17", "18", "19"].contains(&id.as_str()))
        );
        let mut answers = HandlerAnswers::default();
        for question in &questions {
            answers.answered.insert(
                question.site_id.clone(),
                RoutesAnswered {
                    routes: question
                        .handler
                        .asked
                        .iter()
                        .map(|id| answer(*id, RouteChoice::Route, None, 0.9))
                        .collect(),
                },
            );
        }
        assert_eq!(dispatch_rows(&records, &answers).rows.len(), 20);
        // One part failed: its comparisons alone are unanswered.
        answers.answered.remove(&questions[1].site_id);
        answers.unanswered.insert(
            questions[1].site_id.clone(),
            Unanswered {
                code: "model_error".to_string(),
                retriable: false,
            },
        );
        let rows = dispatch_rows(&records, &answers);
        assert_eq!(rows.rows.len(), 16);
        assert_eq!(rows.leads.len(), 4);
        // A record that asks nothing is not a question.
        assert!(handler_questions(&records.clone_settled()).is_empty());
    }
}
