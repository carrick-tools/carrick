//! Result labels reach the scan (carrick#2819, part of the result-shape
//! chain on carrick-cloud#2639).
//!
//! The library store may label an op (or a hook maker) with which part of
//! what the call returns holds the response body (`result`: the whole
//! value, one member, or a parse method). The scanner does not trust the
//! label from the store alone: for each labelled claim whose end discovery
//! finds, it asks the sidecar one `result` check (carrick#2817), which
//! verifies the claimed part against the signatures the compiler reads.
//! A verified result is indexed by the labelled call's file and SWC span, so
//! the type request built for that call carries it (carrick#2818 reads it).
//!
//! Op and maker checks are always sent without `result`
//! ([`crate::services::type_sidecar::LibraryClaim::without_result`]), so
//! their verdicts do not depend on it, and a refused result never fails the
//! claim it sits on. Protocol-blind: any op or maker the store labels.

use std::collections::BTreeMap;

use crate::library_claims::ExportClaims;
use crate::request_summary::discovery::{Discovery, SiteEnd, claim_receivers};
use crate::services::type_sidecar::{
    LibraryCheck, LibraryClaim, LibraryOp, LibraryRole, MakeForm, ResultShape, SemanticsResult,
    SemanticsVerdict, not_reached_in_time,
};

/// The call a labelled claim stands for, as discovery's end names it: the
/// export, the member called, the op (`None` for a maker) and whether the
/// maker is constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AskedEnd {
    package: String,
    export: String,
    member: Option<String>,
    op: Option<LibraryOp>,
    new: bool,
}

impl AskedEnd {
    fn names(&self, end: &SiteEnd) -> bool {
        self.package == end.package
            && self.export == end.export
            && self.member == end.member
            && self.op == end.op
            && self.new == end.new
    }
}

/// The `result` checks of one service, and the end and result each one
/// stands for (`ends` and `results` line up with `checks`).
#[derive(Debug, Default, Clone)]
pub struct ResultAsk {
    pub checks: Vec<LibraryCheck>,
    ends: Vec<AskedEnd>,
    results: Vec<ResultShape>,
}

/// One `result` check for each labelled claim whose end `discovery` found,
/// on every receiver the claim applies to. A claim no discovered call
/// reaches asks nothing.
pub fn result_checks(exports: &[ExportClaims], discovery: &Discovery) -> ResultAsk {
    let mut ask = ResultAsk::default();
    let reached: Vec<&SiteEnd> = discovery
        .sites
        .iter()
        .filter_map(|site| site.end.as_ref())
        .collect();
    for export in exports
        .iter()
        .filter(|export| export.role != LibraryRole::None)
    {
        let claims = export.claims_by_id();
        for (id, claim) in &claims {
            let Some(check) = claim.result_check() else {
                continue;
            };
            let (end, receivers) = match claim {
                LibraryClaim::Op {
                    op, member, on, of, ..
                } => (
                    AskedEnd {
                        package: export.package.clone(),
                        export: export.export.clone(),
                        member: member.clone(),
                        op: Some(*op),
                        new: false,
                    },
                    claim_receivers(&claims, *on, of.as_deref()),
                ),
                LibraryClaim::Make { form, member, .. } => (
                    AskedEnd {
                        package: export.package.clone(),
                        export: export.export.clone(),
                        member: member.clone(),
                        op: None,
                        new: *form == MakeForm::New,
                    },
                    vec!["export".to_string()],
                ),
                _ => continue,
            };
            if !reached.iter().any(|reached| end.names(reached)) {
                continue;
            }
            let LibraryClaim::Result { result, .. } = &check else {
                continue;
            };
            for receiver in receivers {
                ask.checks.push(LibraryCheck {
                    claim_id: id.clone(),
                    package: export.specifier.clone(),
                    export: export.export.clone(),
                    role: export.role,
                    receiver,
                    claim: check.clone(),
                });
                ask.ends.push(end.clone());
                ask.results.push(result.clone());
            }
        }
    }
    ask
}

/// The verified results of a service, by the labelled call they belong to.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ResultIndex {
    /// Keyed by file and the labelled call's SWC span.
    by_span: BTreeMap<(String, u32, u32), ResultShape>,
    /// Keyed by file and line, only where the line holds exactly one
    /// discovered end.
    by_line: BTreeMap<(String, u32), ResultShape>,
    /// The checks asked.
    pub checks: usize,
    /// Of those, the ones the verifier did not reach in its time limit.
    pub not_reached: usize,
}

impl ResultIndex {
    /// The results `verdicts` verify for `ask`, on the ends of `discovery`.
    /// A verdict counts only for the check at its own position that it names.
    /// An end whose verified results disagree gets none; so does a line that
    /// holds more than one end.
    pub fn of(ask: &ResultAsk, verdicts: &[SemanticsResult], discovery: &Discovery) -> Self {
        let mut index = ResultIndex {
            checks: ask.checks.len(),
            not_reached: not_reached_in_time(verdicts),
            ..ResultIndex::default()
        };
        if verdicts.len() != ask.checks.len() {
            return index;
        }
        let verified: Vec<(&AskedEnd, &ResultShape)> = ask
            .checks
            .iter()
            .zip(&ask.ends)
            .zip(&ask.results)
            .zip(verdicts)
            .filter(|(((check, _), _), verdict)| {
                verdict.verdict == SemanticsVerdict::Verified
                    && verdict.claim_id == check.claim_id
                    && verdict.receiver == check.receiver
            })
            .map(|(((_, end), result), _)| (end, result))
            .collect();
        // Every discovered end, once per labelled call.
        let mut ends: BTreeMap<(&str, u32, u32), &SiteEnd> = BTreeMap::new();
        for end in discovery.sites.iter().filter_map(|site| site.end.as_ref()) {
            ends.entry((end.file.as_str(), end.span_start, end.span_end))
                .or_insert(end);
        }
        let mut on_line: BTreeMap<(&str, u32), Vec<&SiteEnd>> = BTreeMap::new();
        for end in ends.values() {
            on_line
                .entry((end.file.as_str(), end.line))
                .or_default()
                .push(end);
        }
        for end in ends.values() {
            let mut results: Vec<&ResultShape> = Vec::new();
            for (_, result) in verified.iter().filter(|(asked, _)| asked.names(end)) {
                if !results.contains(result) {
                    results.push(result);
                }
            }
            let [result] = results[..] else {
                continue;
            };
            index.by_span.insert(
                (end.file.clone(), end.span_start, end.span_end),
                result.clone(),
            );
            if on_line[&(end.file.as_str(), end.line)].len() == 1 {
                index
                    .by_line
                    .insert((end.file.clone(), end.line), result.clone());
            }
        }
        index
    }

    /// The verified result of the labelled call at `file` with SWC span
    /// `span`; with no span, of the one end on `line`. A span that matches no end names
    /// a different call, so the line is not tried.
    pub fn at(&self, file: &str, span: Option<(u32, u32)>, line: u32) -> Option<&ResultShape> {
        match span {
            Some((start, end)) => self.by_span.get(&(file.to_string(), start, end)),
            None => self.by_line.get(&(file.to_string(), line)),
        }
    }

    /// An index holding `result` for the call at `file` with SWC span
    /// `span`, as a test of what reads it needs one.
    #[cfg(test)]
    pub(crate) fn labelled(file: &str, span: (u32, u32), result: ResultShape) -> Self {
        let mut index = ResultIndex::default();
        index
            .by_span
            .insert((file.to_string(), span.0, span.1), result);
        index
    }

    pub fn len(&self) -> usize {
        self.by_span.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_span.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request_summary::discovery::{DiscoverySite, Labels, discover_with};
    use crate::request_summary::fixture_walk;
    use crate::services::type_sidecar::{ResultForm, SemanticsVerdict};

    const CASSETTE: &str = include_str!(
        "../tests/fixtures/result-shape-client/repo/__llm__/library-claims/default.json"
    );

    fn exports() -> Vec<ExportClaims> {
        crate::library_store::parse_answer(CASSETTE).exports
    }

    fn end(member: Option<&str>, op: Option<LibraryOp>, line: u32, span: (u32, u32)) -> SiteEnd {
        SiteEnd {
            package: "fixture-reply-client".to_string(),
            export: "api".to_string(),
            member: member.map(str::to_string),
            role: LibraryRole::HttpClient,
            op,
            file: "src/client.ts".to_string(),
            line,
            span_start: span.0,
            span_end: span.1,
            new: false,
        }
    }

    fn discovery_of_ends(ends: Vec<SiteEnd>) -> Discovery {
        Discovery {
            sites: ends
                .into_iter()
                .map(|end| DiscoverySite {
                    file: end.file.clone(),
                    line: end.line,
                    span_start: end.span_start,
                    span_end: end.span_end,
                    callee: String::new(),
                    end: Some(end),
                    chain: Vec::new(),
                    stop: None,
                    receiver: None,
                    reached: None,
                })
                .collect(),
            ..Discovery::default()
        }
    }

    fn verified(ask: &ResultAsk) -> Vec<SemanticsResult> {
        ask.checks
            .iter()
            .map(|check| SemanticsResult {
                claim_id: check.claim_id.clone(),
                receiver: check.receiver.clone(),
                verdict: SemanticsVerdict::Verified,
                reason: None,
                declarations: Vec::new(),
                request_dropped: None,
            })
            .collect()
    }

    /// The store's label is read off the wire, a result the build cannot
    /// read is dropped without the claim it sits on, and a claim's
    /// op or maker check never carries it.
    #[test]
    fn a_result_is_read_and_never_sent_with_its_op_or_maker() {
        let exports = exports();
        let result_of = |export: &str| {
            exports
                .iter()
                .find(|known| known.export == export)
                .unwrap()
                .claims
                .iter()
                .map(|claim| (claim.without_result(), claim.result_check()))
                .collect::<Vec<_>>()
        };
        for (sent, check) in result_of("api").into_iter().chain(result_of("useLoad")) {
            let text = serde_json::to_string(&sent).unwrap();
            assert!(!text.contains("\"result\""), "{text}");
            assert!(matches!(check, Some(LibraryClaim::Result { .. })));
        }
        let mut answer: serde_json::Value = serde_json::from_str(CASSETTE).unwrap();
        let ops = &mut answer["library_claims"][0]["exports"][0]["ops"];
        assert_eq!(ops[1]["result"], serde_json::json!({ "whole": true }));
        ops[1]["result"] = serde_json::json!({ "whole": false });
        let unreadable = crate::library_store::parse_answer(&answer.to_string()).exports;
        let api = unreadable.iter().find(|e| e.export == "api").unwrap();
        assert_eq!(api.claims.len(), 3, "the op is still read");
        assert_eq!(
            api.claims
                .iter()
                .filter(|c| c.result_check().is_some())
                .count(),
            2,
            "the unreadable result is dropped"
        );
        match exports
            .iter()
            .find(|known| known.export == "useLoad")
            .unwrap()
            .claims[0]
            .result_check()
        {
            Some(LibraryClaim::Result { form, member, .. }) => {
                assert_eq!(form, ResultForm::Make);
                assert_eq!(member, None);
            }
            other => panic!("{other:?}"),
        }
    }

    /// Two labelled calls on one line each keep their own result; the line
    /// names neither. A line with one labelled call names it.
    #[test]
    fn disagreeing_ends_on_one_line_give_nothing() {
        let get = end(Some("get"), Some(LibraryOp::Request), 4, (10, 30));
        let send = end(Some("send"), Some(LibraryOp::Request), 4, (40, 60));
        let alone = end(Some("get"), Some(LibraryOp::Request), 9, (90, 99));
        let discovery = discovery_of_ends(vec![get.clone(), send.clone(), alone.clone()]);
        let exports = exports();
        let ask = result_checks(&exports, &discovery);
        let index = ResultIndex::of(&ask, &verified(&ask), &discovery);
        let data = ResultShape::Member(["data".to_string()]);
        let whole = ResultShape::Whole(true);
        assert_eq!(index.at("src/client.ts", Some((10, 30)), 4), Some(&data));
        assert_eq!(index.at("src/client.ts", Some((40, 60)), 4), Some(&whole));
        // No span that matches names none; with no span the line holds two ends.
        assert_eq!(index.at("src/client.ts", Some((1, 2)), 4), None);
        assert_eq!(index.at("src/client.ts", None, 4), None);
        // A span that matches none is a different call: no line fallback.
        assert_eq!(index.at("src/client.ts", Some((1, 2)), 9), None);
        // With no span, the one end on its line is found by line.
        assert_eq!(index.at("src/client.ts", None, 9), Some(&data));
        assert_eq!(index.at("other.ts", Some((10, 30)), 4), None);
    }

    /// A refused or unreached result is not indexed; a verdict counts only
    /// for the check it names.
    #[test]
    fn a_refused_result_is_not_indexed() {
        let get = end(Some("get"), Some(LibraryOp::Request), 4, (10, 30));
        let discovery = discovery_of_ends(vec![get]);
        let exports = exports();
        let ask = result_checks(&exports, &discovery);
        assert_eq!(
            ask.checks.len(),
            1,
            "only the claim a call reaches is asked"
        );
        let mut refused = verified(&ask);
        refused[0].verdict = SemanticsVerdict::Failed;
        refused[0].reason = Some("result_not_candidate".to_string());
        assert!(ResultIndex::of(&ask, &refused, &discovery).is_empty());
        let mut unreached = verified(&ask);
        unreached[0].verdict = SemanticsVerdict::Unchecked;
        assert!(ResultIndex::of(&ask, &unreached, &discovery).is_empty());
        let mut other = verified(&ask);
        other[0].claim_id = "another".to_string();
        assert!(ResultIndex::of(&ask, &other, &discovery).is_empty());
        assert!(ResultIndex::of(&ask, &[], &discovery).is_empty());
        assert!(!ResultIndex::of(&ask, &verified(&ask), &discovery).is_empty());
    }

    /// End to end on the fixture, with the real sidecar: the compiler's
    /// verdicts decide which labelled results reach the index.
    #[test]
    fn the_sidecar_verifies_the_fixtures_results() {
        let Some(on) = fixture_walk::walked("result-shape-client", ".", "web", &[], true) else {
            return;
        };
        let script = fixture_walk::sidecar().expect("the built sidecar");
        let exports = exports();
        let labels = Labels::of(&exports);
        let discovery = discover_with(&on.inputs, &[], &labels, &on.repo);
        let ask = result_checks(&exports, &discovery);
        // The op and maker checks of the same store answer are those of an
        // answer that labels no result, so their verdicts do not move.
        let unlabelled: Vec<ExportClaims> = exports
            .iter()
            .cloned()
            .map(|mut export| {
                export.claims = export
                    .claims
                    .iter()
                    .map(LibraryClaim::without_result)
                    .collect();
                export
            })
            .collect();
        let sent = crate::request_summary::discovery::declaration_ask(&on.inputs, &exports).checks;
        assert!(!sent.is_empty());
        assert_eq!(
            serde_json::to_string(&sent).unwrap(),
            serde_json::to_string(
                &crate::request_summary::discovery::declaration_ask(&on.inputs, &unlabelled).checks
            )
            .unwrap()
        );
        let sidecar = crate::services::TypeSidecar::spawn(&script).expect("spawn the sidecar");
        sidecar.set_scan_root(&on.repo);
        sidecar.start_init(&on.repo, None);
        sidecar
            .wait_ready(std::time::Duration::from_secs(300))
            .expect("the sidecar is ready");
        let verdicts = sidecar
            .verify_library_claims(&on.repo, &ask.checks)
            .expect("the verify request answers")
            .verdicts;
        let index = ResultIndex::of(&ask, &verdicts, &discovery);
        let at = |file: &str, line: u32| -> Option<ResultShape> {
            let labelled = discovery
                .sites
                .iter()
                .filter_map(|site| site.end.as_ref())
                .find(|end| end.file == file && end.line == line)
                .unwrap_or_else(|| panic!("no end at {file}:{line}: {:#?}", discovery.sites));
            index
                .at(file, Some((labelled.span_start, labelled.span_end)), line)
                .cloned()
        };
        let data = Some(ResultShape::Member(["data".to_string()]));
        assert_eq!(at("src/client.ts", 9), data, "a reply wrapper");
        assert_eq!(
            at("src/client.ts", 15),
            Some(ResultShape::Whole(true)),
            "the whole return"
        );
        assert_eq!(
            at("src/client.ts", 20),
            None,
            "a member no signature offers is refused"
        );
        assert_eq!(at("src/hook.ts", 6), data, "a hook's state union");
        // Two labelled calls on one line: each keeps its span's result, and
        // the line alone names neither.
        let both: Vec<&SiteEnd> = discovery
            .sites
            .iter()
            .filter_map(|site| site.end.as_ref())
            .filter(|end| end.file == "src/client.ts" && end.line == 25)
            .collect();
        assert_eq!(both.len(), 2, "{both:#?}");
        for end in both {
            assert_eq!(
                index
                    .at("src/client.ts", Some((end.span_start, end.span_end)), 25)
                    .cloned(),
                data
            );
        }
        assert_eq!(index.at("src/client.ts", None, 25), None);
    }
}
