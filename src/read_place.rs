//! Where the reader read a value: the place stored on a manifest entry whose
//! ask points at a value (carrick#4244).
//!
//! # The rule
//!
//! A manifest entry's `evidence.span_start` and `evidence.span_end` name the
//! node the type text was read from, as TypeScript positions: UTF-16 code
//! units counted from the start of the file (a leading byte-order mark is not
//! counted), `[start, end)`. That is the unit the compiler engine locates by
//! and the unit `contractPlace` reads in the cloud, so a reader that wants the
//! place of a contract row needs no conversion.
//!
//! One writer, [`store_read_places`], sets it. It takes the first ask for the
//! entry's alias that names a value:
//!
//! - any ask on a consumer entry (a call, a call's payload, a document);
//! - an `Expression` or `FunctionParam` ask on a non-HTTP entry (a pub/sub or
//!   socket payload, a subscriber's message).
//!
//! The ask's own span is the place. An ask located by text carries none, so
//! the place is the node the compiler engine's answer read (its
//! `source_location`), converted to an offset.
//!
//! # What is never placed
//!
//! - An HTTP producer's request or response. The compiler engine follows the
//!   handler from the registration, so the ask's span is the registration
//!   call, not a value; the value places are a separate question (carrick#4244
//!   T3).
//! - A call result asked by line alone (no span, no text): its answer names
//!   the result node, not the call (a named gap).
//! - A call whose result label went unanswered (carrick#4154): it has no
//!   body, so it has no place.
//! - GraphQL resolver returns, RPC procedures and queues (named gaps).
//!
//! A stored blob without a place reads "not placed", never a default.

use std::collections::HashMap;
use std::path::Path;

use crate::cloud_storage::{ManifestRole, TypeManifestEntry};
use crate::operation::Protocol;
use crate::services::type_sidecar::{InferKind, InferRequestItem, InferredType, SourceLocation};

/// Write the place of every entry whose ask points at a value, into its
/// evidence. `infer` is the asks as the capture sent them (absolute paths),
/// `answers` the compiler engine's answers to them, and `read_file` reads a
/// file's text for an answer's line and column offsets.
///
/// A call result also keeps the call's text and line as the ask had them, so
/// a later `--changed-only` run retypes the same call from the stored blob
/// (carrick#2567).
pub(crate) fn store_read_places(
    manifest: &mut [TypeManifestEntry],
    infer: &[InferRequestItem],
    answers: &[InferredType],
    read_file: &dyn Fn(&str) -> Option<String>,
) {
    let mut asks: HashMap<&str, Vec<&InferRequestItem>> = HashMap::new();
    for ask in infer {
        if let Some(alias) = ask.alias.as_deref() {
            asks.entry(alias).or_default().push(ask);
        }
    }
    let mut answered: HashMap<&str, &InferredType> = HashMap::new();
    for answer in answers {
        answered.entry(answer.alias.as_str()).or_insert(answer);
    }
    for entry in manifest.iter_mut() {
        let Some(ask) = asks
            .get(entry.type_alias.as_str())
            .and_then(|asks| asks.iter().find(|ask| names_a_value(entry, ask)))
        else {
            continue;
        };
        if ask.infer_kind == InferKind::CallResult {
            entry.evidence.expression_text = ask.expression_text.clone();
            entry.evidence.expression_line = ask.expression_line;
        }
        let place = match (ask.span_start, ask.span_end) {
            (Some(start), Some(end)) => Some((start, end)),
            // A call result asked by line alone stays unplaced, as on main:
            // its answer names the result node, and a replay by that span
            // would retype the result, not the call.
            _ if ask.infer_kind == InferKind::CallResult && ask.expression_text.is_none() => None,
            _ => answered
                .get(entry.type_alias.as_str())
                .and_then(|answer| answer_place(answer, &ask.file_path, read_file)),
        };
        if let Some((start, end)) = place {
            entry.evidence.span_start = Some(start);
            entry.evidence.span_end = Some(end);
        }
    }
}

/// Whether `ask` points at the value `entry`'s type was read from.
fn names_a_value(entry: &TypeManifestEntry, ask: &InferRequestItem) -> bool {
    if ask.result_unanswered || ask.at_registration {
        return false;
    }
    // The place sits beside `evidence.file_path`, so an ask in another file
    // (a wrapper's body) is not this entry's place.
    if !Path::new(&ask.file_path).ends_with(entry.evidence.file_path.trim_start_matches("./")) {
        return false;
    }
    if entry.role == ManifestRole::Consumer {
        return !matches!(
            ask.infer_kind,
            InferKind::RequestConfig | InferKind::ReceiverType
        );
    }
    entry.key.protocol() != Protocol::Http
        && matches!(
            ask.infer_kind,
            InferKind::Expression | InferKind::FunctionParam
        )
}

/// The `[start, end)` positions of the node an answer read, when it read one
/// in the file the ask named.
fn answer_place(
    answer: &InferredType,
    ask_file: &str,
    read_file: &dyn Fn(&str) -> Option<String>,
) -> Option<(u32, u32)> {
    let location = &answer.source_location;
    if !same_file(&location.file_path, ask_file) {
        return None;
    }
    let text = read_file(&location.file_path)?;
    location_offsets(&text, location)
}

fn same_file(a: &str, b: &str) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

/// A location as TypeScript positions in `text`. The compiler engine counts a
/// column from the start of the node's first line, for both ends, in UTF-16
/// units (`getNodeLocation`), so a node that spans lines ends at its first
/// line's offset plus its end column.
fn location_offsets(text: &str, location: &SourceLocation) -> Option<(u32, u32)> {
    let (start, end) = (location.start_column?, location.end_column?);
    let line_start = line_start_utf16(text, location.start_line)?;
    (end >= start).then_some((line_start + start, line_start + end))
}

/// The UTF-16 offset a 1-based line starts at. Lines end at `\n`, `\r\n`,
/// `\r`, U+2028 and U+2029, as TypeScript counts them; a leading byte-order
/// mark is dropped, as TypeScript drops it.
fn line_start_utf16(text: &str, line: u32) -> Option<u32> {
    if line == 0 {
        return None;
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut current = 1;
    let mut offset = 0u32;
    let mut chars = text.chars().peekable();
    while current < line {
        let c = chars.next()?;
        offset += c.len_utf16() as u32;
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                    offset += 1;
                }
                current += 1;
            }
            '\n' | '\u{2028}' | '\u{2029}' => current += 1,
            _ => {}
        }
    }
    Some(offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_storage::{ManifestTypeKind, ManifestTypeState, TypeEvidence};
    use crate::operation::OperationKey;

    fn entry(
        key: OperationKey,
        role: ManifestRole,
        kind: ManifestTypeKind,
        alias: &str,
        infer_kind: InferKind,
    ) -> TypeManifestEntry {
        TypeManifestEntry {
            key,
            role,
            type_kind: kind,
            type_alias: alias.to_string(),
            file_path: "src/a.ts".to_string(),
            line_number: 1,
            is_explicit: false,
            type_state: ManifestTypeState::Implicit,
            evidence: TypeEvidence {
                file_path: "src/a.ts".to_string(),
                span_start: None,
                span_end: None,
                expression_text: None,
                expression_line: None,
                line_number: 1,
                infer_kind,
                is_explicit: false,
                type_state: ManifestTypeState::Implicit,
            },
            resolved_definition: None,
            expanded_definition: None,
            primary_type_symbol: None,
            defined_in: None,
            any_provenance: Vec::new(),
            unwidened_definition: None,
            unwidened_out_of_budget: false,
            response_modes: None,
            stated_values: Default::default(),
            v1_before_demotion: None,
        }
    }

    fn ask(alias: &str, kind: InferKind, span: Option<(u32, u32)>) -> InferRequestItem {
        InferRequestItem {
            member: None,
            result: None,
            file_path: "/repo/src/a.ts".to_string(),
            line_number: 1,
            span_start: span.map(|s| s.0),
            span_end: span.map(|s| s.1),
            expression_text: None,
            expression_line: None,
            infer_kind: kind,
            alias: Some(alias.to_string()),
            param_name: None,
            route_path: None,
            at_registration: false,
            result_unanswered: false,
            through_call: None,
        }
    }

    fn answer(alias: &str, location: SourceLocation) -> InferredType {
        InferredType {
            alias: alias.to_string(),
            type_string: "unknown".to_string(),
            is_explicit: false,
            source_location: location,
            infer_kind: InferKind::CallResult,
            primary_type_symbol: None,
            array_depth: None,
            primary_type_symbol_source: None,
            declaring_package: None,
            member_return_type: None,
            any_provenance: Vec::new(),
            unwidened_type_string: None,
            unwidened_out_of_budget: false,
            stated_body: None,
            printed_names: Vec::new(),
            raw_text_read: false,
            absent_body_tolerated: false,
            response_modes: None,
            model_symbol_relation: None,
            machinery: false,
            anchor_machinery: false,
            read_spans: Vec::new(),
            body_member: None,
        }
    }

    fn no_file(_: &str) -> Option<String> {
        None
    }

    fn at(start_line: u32, start: u32, end: u32) -> SourceLocation {
        SourceLocation {
            file_path: "/repo/src/a.ts".to_string(),
            start_line,
            end_line: start_line,
            start_column: Some(start),
            end_column: Some(end),
        }
    }

    #[test]
    fn every_value_ask_stores_the_node_it_read() {
        let http = |role, kind, alias, infer| {
            entry(
                OperationKey::http("POST", "/orders"),
                role,
                kind,
                alias,
                infer,
            )
        };
        let topic = OperationKey::pubsub("orders.created");
        let mut manifest = vec![
            http(
                ManifestRole::Consumer,
                ManifestTypeKind::Request,
                "ConsumerBody",
                InferKind::RequestBody,
            ),
            http(
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                "ConsumerResult",
                InferKind::CallResult,
            ),
            entry(
                topic,
                ManifestRole::Consumer,
                ManifestTypeKind::Request,
                "Publish",
                InferKind::Expression,
            ),
            http(
                ManifestRole::Producer,
                ManifestTypeKind::Request,
                "ProducerBody",
                InferKind::RequestBody,
            ),
            http(
                ManifestRole::Producer,
                ManifestTypeKind::Response,
                "ProducerResponse",
                InferKind::ResponseBody,
            ),
        ];
        let infer = vec![
            ask("ConsumerBody", InferKind::RequestBody, Some((10, 20))),
            ask("ConsumerResult", InferKind::CallResult, Some((30, 40))),
            ask("Publish", InferKind::Expression, Some((50, 60))),
            ask("ProducerBody", InferKind::RequestBody, Some((70, 80))),
            ask("ProducerResponse", InferKind::ResponseBody, Some((90, 99))),
        ];
        store_read_places(&mut manifest, &infer, &[], &no_file);
        let places: Vec<_> = manifest
            .iter()
            .map(|e| (e.evidence.span_start, e.evidence.span_end))
            .collect();
        assert_eq!(
            places,
            vec![
                (Some(10), Some(20)),
                (Some(30), Some(40)),
                (Some(50), Some(60)),
                (None, None),
                (None, None),
            ],
            "consumer and payload asks place; an HTTP producer's registration ask never does"
        );
    }

    #[test]
    fn a_text_located_call_takes_the_place_its_answer_read() {
        // Line 2 starts after "é;\n" = 3 UTF-16 units; the call sits at
        // columns 4..12 of it.
        let source = "é;\n    api.go();\n";
        let read = |_: &str| Some(source.to_string());
        let mut manifest = vec![entry(
            OperationKey::http("GET", "/x"),
            ManifestRole::Consumer,
            ManifestTypeKind::Response,
            "ByText",
            InferKind::CallResult,
        )];
        let mut text_ask = ask("ByText", InferKind::CallResult, None);
        text_ask.expression_text = Some("api.go()".to_string());
        text_ask.expression_line = Some(2);
        let answers = [answer("ByText", at(2, 4, 12))];
        store_read_places(&mut manifest, &[text_ask], &answers, &read);
        let evidence = &manifest[0].evidence;
        assert_eq!(
            (evidence.span_start, evidence.span_end),
            (Some(3 + 4), Some(3 + 12))
        );
        assert_eq!(evidence.expression_text.as_deref(), Some("api.go()"));
    }

    #[test]
    fn stored_call_locators_replay_the_same_calls() {
        use crate::engine::type_compat_v2::{consumer_call_locators, stored_call_locators};
        let call = |alias: &str, unanswered: bool| {
            let mut a = ask(alias, InferKind::CallResult, None);
            a.expression_text = Some("api.go()".to_string());
            a.expression_line = Some(2);
            a.result_unanswered = unanswered;
            a
        };
        let consumer = |alias: &str| {
            entry(
                OperationKey::http("GET", "/x"),
                ManifestRole::Consumer,
                ManifestTypeKind::Response,
                alias,
                InferKind::CallResult,
            )
        };
        let infer = [call("Live", false), call("Gone", true)];
        let mut manifest = vec![consumer("Live"), consumer("Gone")];
        let source = "\n    api.go();\n";
        let read = |_: &str| Some(source.to_string());
        let answers = [answer("Live", at(2, 4, 12))];
        store_read_places(&mut manifest, &infer, &answers, &read);
        let mut now: Vec<_> = stored_call_locators(&manifest, Path::new("/repo"))
            .into_keys()
            .collect();
        now.sort();
        let mut before: Vec<_> = consumer_call_locators(&infer).into_keys().collect();
        before.sort();
        assert_eq!(
            now, before,
            "the stored blob replays the calls the scan retyped"
        );
        assert_eq!(now, vec!["Live".to_string()]);
    }

    /// The span stored on a text-located call names the node its answer read,
    /// not the call the text finds, so the replay keeps locating it by text.
    #[test]
    fn a_text_located_call_replays_to_the_same_node() {
        use crate::engine::type_compat_v2::{consumer_call_locators, stored_call_locators};
        let mut text_ask = ask("T", InferKind::CallResult, None);
        text_ask.expression_text = Some("api.go()".to_string());
        text_ask.expression_line = Some(2);
        let mut manifest = vec![entry(
            OperationKey::http("GET", "/x"),
            ManifestRole::Consumer,
            ManifestTypeKind::Response,
            "T",
            InferKind::CallResult,
        )];
        let read = |_: &str| Some("\n    api.go();\n".to_string());
        store_read_places(
            &mut manifest,
            &[text_ask.clone()],
            &[answer("T", at(2, 4, 12))],
            &read,
        );
        assert_eq!(manifest[0].evidence.span_start, Some(5));
        let stored = stored_call_locators(&manifest, Path::new("/repo"));
        let scanned = consumer_call_locators(&[text_ask]);
        assert_eq!(stored["T"].span_start, None);
        assert_eq!(stored["T"].span_start, scanned["T"].span_start);
        assert_eq!(stored["T"].expression_text, scanned["T"].expression_text);
        assert_eq!(stored["T"].expression_line, scanned["T"].expression_line);
    }

    /// A call result asked by line alone (no span, no text) is not located
    /// on main, and stays out of the replay: an answer-derived span would aim
    /// the replay at the result node, not the call.
    #[test]
    fn a_line_only_call_result_stays_unplaced_and_unreplayed() {
        use crate::engine::type_compat_v2::stored_call_locators;
        let mut manifest = vec![entry(
            OperationKey::http("GET", "/x"),
            ManifestRole::Consumer,
            ManifestTypeKind::Response,
            "LineOnly",
            InferKind::CallResult,
        )];
        let line_only = ask("LineOnly", InferKind::CallResult, None);
        let read = |_: &str| Some("\n    api.go();\n".to_string());
        let answers = [answer("LineOnly", at(2, 4, 12))];
        store_read_places(&mut manifest, &[line_only], &answers, &read);
        assert_eq!(manifest[0].evidence.span_start, None);
        assert!(stored_call_locators(&manifest, Path::new("/repo")).is_empty());
    }

    #[test]
    fn an_unanswered_call_result_is_not_placed() {
        let mut manifest = vec![entry(
            OperationKey::http("GET", "/x"),
            ManifestRole::Consumer,
            ManifestTypeKind::Response,
            "Gone",
            InferKind::CallResult,
        )];
        let mut gone = ask("Gone", InferKind::CallResult, Some((1, 2)));
        gone.result_unanswered = true;
        store_read_places(&mut manifest, &[gone], &[], &no_file);
        assert_eq!(manifest[0].evidence.span_start, None);
    }

    #[test]
    fn line_starts_count_utf16_units_and_every_terminator() {
        let text = "\u{feff}a😀\r\nb\rc\u{2028}d\ne";
        // a(1) 😀(2) \r\n(2) -> line 2 at 5; b\r -> line 3 at 7; c  -> 4 at 9; d\n -> 5 at 11.
        assert_eq!(line_start_utf16(text, 1), Some(0));
        assert_eq!(line_start_utf16(text, 2), Some(5));
        assert_eq!(line_start_utf16(text, 3), Some(7));
        assert_eq!(line_start_utf16(text, 4), Some(9));
        assert_eq!(line_start_utf16(text, 5), Some(11));
        assert_eq!(line_start_utf16(text, 6), None);
    }
}
