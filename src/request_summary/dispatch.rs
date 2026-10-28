//! How a hand-written dispatch reaches the request (carrick#2474 R4a, R4b):
//! a request's path compared with a literal (`url.pathname === "/orders"`,
//! `case "/health":`, `ROUTES[pathname]`), and the method compared beside it
//! or around it.
//!
//! A path comparison's chain ([`super::guards`]) reaches the request through
//! one of two arms:
//!
//! - **label**: the first call the comparing function is handed to is a
//!   labelled call discovery found, whose op is `receive`, and the store's
//!   label for it names the handler slot the function sits in, the
//!   parameter that is the request, and the member path the comparison
//!   reads (`request.path`, `request.url`, or the URL's `pathname`). The
//!   path is forced.
//! - **entry point**: the chain ends at a function the module offers that
//!   nothing in the repository calls, and is handed to no call. Whether the
//!   comparison chooses a route is asked: no label names the request's
//!   members.
//!
//! The method is a method literal compared, not negated, with the same
//! request's method member (carrick#2693: a literal compared with another
//! value, such as a body's `action`, chooses no method): a top-level `&&`
//! operand of the same test, or a guard (an `if` test operand or a `case`)
//! whose branch holds the path comparison. For a
//! predicate the comparison is first looked for in the predicate, then at
//! the caller's call that tests it. A `startsWith` literal states a scope,
//! never a route. Every comparison that reaches no handler is counted by a
//! reason from a closed list.
//!
//! [`handler_records`] (in `dispatch/records.rs`) groups the comparisons by
//! the request handler they sit in, as the record a model is asked about;
//! [`crate::site_rows::dispatch`] reads the answers into rows.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use super::Site;
use super::decided_sites::{ArgSlot, DiscoverySite, LabelledEnd, SiteLabel};
use super::guards::{
    ChainEnd, ChainFacts, Chains, EntryAt, GuardBranch, GuardChain, GuardIr, GuardPosition,
    Handoff, Step, key_steps, read_steps,
};
use crate::services::type_sidecar::RequestAt;

mod records;
pub use records::{ComparisonPlace, HandlerRecords, HoleReach, LineRange, handler_records};

/// The request parameter a comparison reads, and the accesses down from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequestRead {
    pub param: usize,
    pub steps: Vec<String>,
}

/// What a route or a method reaches the request through: the handoff call
/// (the function it is written in, its SWC span) and parameter, or the
/// entry function and parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Anchor {
    Call(usize, u32, u32, usize),
    Entry(usize, usize),
}

/// A chain that reached the request: how, and what it reads.
#[derive(Clone)]
struct Reached {
    anchor: Anchor,
    request: RequestRead,
    /// The label's method member path, as steps (label arm only).
    method_steps: Option<Vec<Step>>,
}

/// A method literal compared in a function, with its chains.
struct MethodGuard<'g> {
    guard: &'g GuardIr,
    method: String,
    chains: Vec<(GuardChain, ChainFacts)>,
}

/// Where a method is looked for: the node, the comparison's span, the
/// branch of the test it stands in, and whether it is a top-level `&&`
/// operand there.
struct Decision {
    node: usize,
    span: (u32, u32),
    branch: Option<(u32, u32)>,
    conjunct: bool,
}

fn count(counts: &mut BTreeMap<String, usize>, key: &str) {
    *counts.entry(key.to_string()).or_default() += 1;
}

/// What the end of a chain is called, for a reason: a hole's reason, or the
/// end's kind.
fn end_kind(end: &ChainEnd) -> String {
    let word = |value: serde_json::Result<serde_json::Value>| {
        value
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_default()
    };
    match end {
        ChainEnd::Hole { reason, .. } => word(serde_json::to_value(reason)),
        ChainEnd::EntryPoint { .. } => "entry_point".to_string(),
        ChainEnd::Literal { .. } => "literal".to_string(),
        ChainEnd::Env { .. } => "env".to_string(),
        ChainEnd::Global { .. } => "global".to_string(),
        ChainEnd::ExternalImport { .. } => "external_import".to_string(),
    }
}

/// Whether `steps` read the request's path: its `path`, its `url`, or the
/// `pathname` of a URL made from its `url` (the URL standard's own member).
fn reads_path(steps: &[Step], request: &RequestAt) -> bool {
    let steps = read_steps(steps);
    if let Some(path) = &request.path
        && steps == key_steps(path).as_slice()
    {
        return true;
    }
    let Some(url) = &request.url else {
        return false;
    };
    let url = key_steps(url);
    if steps == url.as_slice() {
        return true;
    }
    let mut parsed = url;
    parsed.push(Step::Url);
    parsed.push(Step::Key("pathname".to_string()));
    steps == parsed.as_slice()
}

/// Whether a label's handler slot is where the function was handed in.
fn slot_is(slot: &ArgSlot, arg: usize, path: &[String]) -> bool {
    slot.arg == arg
        && match &slot.key {
            None => path.is_empty(),
            Some(key) => path.len() == 1 && path[0] == *key,
        }
}

struct Reader<'r, 'a> {
    chains: &'r Chains<'a>,
    discovered: &'r [DiscoverySite],
    labels: &'r [SiteLabel],
}

impl Reader<'_, '_> {
    /// How `chain` reaches the request, or why it states no route.
    fn reached(&self, chain: &GuardChain, facts: &ChainFacts) -> Result<Reached, String> {
        if let Some(handoff) = &facts.handoff {
            return self.labelled(chain, handoff);
        }
        match (&chain.end, &facts.entry) {
            (ChainEnd::EntryPoint { .. }, Some(entry)) => Ok(Reached {
                anchor: Anchor::Entry(entry.node, entry.param),
                request: RequestRead {
                    param: entry.param,
                    steps: entry.steps.iter().map(Step::render).collect(),
                },
                method_steps: None,
            }),
            (ChainEnd::Hole { .. }, _) => Err(format!("hole:{}", end_kind(&chain.end))),
            (end, _) => Err(format!("end:{}", end_kind(end))),
        }
    }

    /// The labelled end discovery found at the handoff call.
    fn end_at(&self, file: &str, handoff: &Handoff) -> Option<&LabelledEnd> {
        self.end_at_site(file, &handoff.site)
    }

    /// The labelled end at the handoff call, its file read off the caller.
    fn end_of(&self, handoff: &Handoff) -> Option<&LabelledEnd> {
        let file = self.chains.rel(self.chains.nodes[handoff.caller].file);
        self.end_at(&file, handoff)
    }

    /// The labelled end discovery found at the call `site` in `file`.
    fn end_at_site(&self, file: &str, site: &Site) -> Option<&LabelledEnd> {
        self.discovered.iter().find_map(|found| {
            found.end.as_ref().filter(|end| {
                end.file == file
                    && end.span_start == site.span_start
                    && end.span_end == site.span_end
            })
        })
    }

    /// The labelled `receive` call written in `caller` at `site` whose
    /// label's handler slot is `arg` and `path`, the slot a function is
    /// handed in (carrick#2474 R4b): the function is that label's handler,
    /// whether or not the label names the request's members.
    fn receive_at(
        &self,
        caller: usize,
        site: &Site,
        arg: usize,
        path: &[String],
    ) -> Option<&LabelledEnd> {
        let file = self.chains.rel(self.chains.nodes[caller].file);
        let end = self.end_at_site(&file, site)?;
        if end.op.as_deref() != Some("receive") {
            return None;
        }
        handler_at(self.labels, end, arg, path).then_some(end)
    }

    /// The label arm: the handoff call is a labelled `receive` whose label
    /// names the handler slot, the request parameter and the path read.
    fn labelled(&self, chain: &GuardChain, handoff: &Handoff) -> Result<Reached, String> {
        let (_, request) = self.request_of(chain, handoff)?;
        if !reads_path(&handoff.steps, request) {
            return Err("steps_not_path".to_string());
        }
        Ok(Reached {
            anchor: Anchor::Call(
                handoff.caller,
                handoff.site.span_start,
                handoff.site.span_end,
                handoff.param,
            ),
            request: RequestRead {
                param: handoff.param,
                steps: handoff.steps.iter().map(Step::render).collect(),
            },
            method_steps: request.method.as_deref().map(key_steps),
        })
    }

    /// Whether a method literal's `chain` reads the request's method member
    /// (carrick#2693): on the label arm the member the label names, read off
    /// the label's request parameter; at an entry point the request's own
    /// `method` ([`reads_entry_method`]). A chain that ends at a hole reads
    /// no member the code shows.
    fn reads_method(&self, chain: &GuardChain, facts: &ChainFacts) -> bool {
        if let Some(handoff) = &facts.handoff {
            return self.request_of(chain, handoff).is_ok_and(|(_, request)| {
                request.method.as_deref().is_some_and(|method| {
                    read_steps(&handoff.steps) == key_steps(method).as_slice()
                })
            });
        }
        matches!(chain.end, ChainEnd::EntryPoint { .. })
            && facts.entry.as_ref().is_some_and(reads_entry_method)
    }

    /// The labelled `receive` call a chain is handed to and the request its
    /// one label names, at the handler slot and request parameter the chain
    /// was handed in.
    fn request_of(
        &self,
        chain: &GuardChain,
        handoff: &Handoff,
    ) -> Result<(&LabelledEnd, &RequestAt), String> {
        let chains = self.chains;
        let file = chains.rel(chains.nodes[handoff.caller].file);
        let Some(end) = self.end_at(&file, handoff) else {
            return Err(format!("handoff_unlabelled:{}", end_kind(&chain.end)));
        };
        if end.op.as_deref() != Some("receive") {
            return Err("handoff_not_receive".to_string());
        }
        let (handler, request) = request_read(self.labels, end)?;
        if !handler
            .as_ref()
            .is_some_and(|slot| slot_is(slot, handoff.arg, &handoff.path))
        {
            return Err("handler_slot_mismatch".to_string());
        }
        if request.param as usize != handoff.param {
            return Err("request_param_mismatch".to_string());
        }
        Ok((end, request))
    }
}

/// Whether a `receive` label claimed on `end`'s receiver names the handler
/// slot `arg` and `path`.
fn handler_at(labels: &[SiteLabel], end: &LabelledEnd, arg: usize, path: &[String]) -> bool {
    labels.iter().any(|label| {
        label.op.as_deref() == Some("receive")
            && label.is_on(end)
            && label
                .handler
                .as_ref()
                .is_some_and(|slot| slot_is(slot, arg, path))
    })
}

/// The one (handler slot, request) pair the `receive` labels claimed on
/// `end`'s receiver agree on, or why they name none.
fn request_read<'l>(
    labels: &'l [SiteLabel],
    end: &LabelledEnd,
) -> Result<(&'l Option<ArgSlot>, &'l RequestAt), String> {
    let mut agreed: Vec<(&Option<ArgSlot>, &Option<RequestAt>)> = Vec::new();
    for label in labels
        .iter()
        .filter(|label| label.op.as_deref() == Some("receive") && label.is_on(end))
    {
        let read = (&label.handler, &label.request);
        if !agreed.contains(&read) {
            agreed.push(read);
        }
    }
    if agreed.len() > 1 {
        return Err("label_ambiguous".to_string());
    }
    match agreed.first().copied() {
        Some((handler, Some(request))) => Ok((handler, request)),
        _ => Err("label_without_request".to_string()),
    }
}

/// Whether a `receive` label claimed on `end`'s receiver names a path slot
/// (the call takes the path the handler is registered under).
pub(super) fn names_path(labels: &[SiteLabel], end: &LabelledEnd) -> bool {
    labels.iter().any(|label| {
        label.op.as_deref() == Some("receive") && label.is_on(end) && label.name.is_some()
    })
}

/// Whether an entry point's chain reads its request's `method`: the member
/// the web platform's `Request` and Node's `IncomingMessage` both hold the
/// method at (String methods after it dropped).
fn reads_entry_method(entry: &EntryAt) -> bool {
    read_steps(&entry.steps) == [Step::Key("method".to_string())]
}

/// The method a path guard's route states: looked for at the comparison,
/// then, for a predicate's comparison, at the caller's call that tests it.
/// `None` when the code chooses no one method (none qualifies, or several,
/// such as `case "GET": case "HEAD":` sharing one branch or methods that
/// disagree).
fn method_of(
    node: usize,
    guard: &GuardIr,
    facts: &ChainFacts,
    reached: &Reached,
    methods: &HashMap<usize, Vec<MethodGuard<'_>>>,
) -> Option<String> {
    method_reaching(node, guard, facts, methods, &|candidate| {
        candidate
            .chains
            .iter()
            .any(|(_, facts)| reaches_same(facts, reached))
    })
}

/// [`method_of`], with `reaches` saying whether a method guard reaches the
/// request the path comparison does (carrick#2474 R4b: a hole's comparison
/// has no anchor, only the handler both attach to).
fn method_reaching(
    node: usize,
    guard: &GuardIr,
    facts: &ChainFacts,
    methods: &HashMap<usize, Vec<MethodGuard<'_>>>,
    reaches: &dyn Fn(&MethodGuard<'_>) -> bool,
) -> Option<String> {
    let own = Decision {
        node,
        span: (guard.site.lo, guard.site.hi),
        branch: match guard.branch {
            GuardBranch::Span(lo, hi) => Some((lo, hi)),
            _ => None,
        },
        conjunct: guard.conjunct,
    };
    let found = qualifying(&own, reaches, methods);
    let found = match (found.is_empty(), &guard.branch, facts.decided_at) {
        (true, GuardBranch::Returned, Some(at)) => qualifying(
            &Decision {
                node: at.node,
                span: at.span,
                branch: Some(at.branch),
                conjunct: at.conjunct,
            },
            reaches,
            methods,
        ),
        _ => found,
    };
    let mut each = found;
    each.sort();
    each.dedup();
    match each.len() {
        1 => each.pop(),
        _ => None,
    }
}

/// The methods of the method guards of `at`'s node that qualify for it: an
/// operand of the same `&&` test, or a guard whose branch encloses it.
fn qualifying(
    at: &Decision,
    reaches: &dyn Fn(&MethodGuard<'_>) -> bool,
    methods: &HashMap<usize, Vec<MethodGuard<'_>>>,
) -> Vec<String> {
    let mut found = Vec::new();
    for candidate in methods.get(&at.node).into_iter().flatten() {
        let guard = candidate.guard;
        if guard.negated {
            continue;
        }
        let same_test = guard.conjunct
            && at.conjunct
            && at
                .branch
                .is_some_and(|(lo, hi)| guard.branch == GuardBranch::Span(lo, hi));
        let encloses = (guard.conjunct || guard.position == GuardPosition::Case)
            && matches!(guard.branch, GuardBranch::Span(lo, hi)
                if lo <= at.span.0 && at.span.1 <= hi);
        if !(same_test || encloses) || !reaches(candidate) {
            continue;
        }
        found.push(candidate.method.clone());
    }
    found
}

/// Whether a method guard's chain reaches the request the route does: the
/// same handoff call and parameter, reading the label's method member; or
/// the same entry function and parameter, reading its `method`
/// ([`reads_entry_method`], carrick#2693).
fn reaches_same(facts: &ChainFacts, reached: &Reached) -> bool {
    match &reached.anchor {
        Anchor::Call(caller, start, end, param) => {
            let (Some(handoff), Some(method)) = (&facts.handoff, &reached.method_steps) else {
                return false;
            };
            handoff.caller == *caller
                && handoff.param == *param
                && handoff.site.span_start == *start
                && handoff.site.span_end == *end
                && read_steps(&handoff.steps) == method.as_slice()
        }
        Anchor::Entry(node, param) => {
            facts.handoff.is_none()
                && facts.entry.as_ref().is_some_and(|entry| {
                    entry.node == *node && entry.param == *param && reads_entry_method(entry)
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::discovery::{Labels, discover};
    use super::super::fixture_walk::{Walked, walked};
    use super::*;
    use crate::library_claims::ExportClaims;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/dispatch-routes")
            .join(name)
    }

    /// The fixture's store answer, with `edit` applied to its raw JSON.
    fn claims(edit: impl FnOnce(&mut serde_json::Value)) -> Vec<ExportClaims> {
        let mut answer: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(fixture("library-claims.json")).unwrap())
                .unwrap();
        edit(&mut answer);
        let answer = crate::library_store::parse_answer(&answer.to_string());
        assert_eq!(answer.dropped, 0, "every fixture claim parses");
        answer.exports
    }

    /// Discovery's sites on `walked` under `exports`, read as the engine
    /// hands them on.
    fn discovered(walked: &Walked, exports: &[ExportClaims]) -> Vec<DiscoverySite> {
        discover(&walked.inputs, &Labels::of(exports), &walked.repo)
            .sites
            .iter()
            .map(|site| serde_json::from_value(serde_json::to_value(site).unwrap()).unwrap())
            .collect()
    }

    /// carrick#1678: a router's `get` (handler arg 1, a path) and the `get`
    /// of what its `at` returns (handler arg 0) are two labels; each call
    /// reads its own, whichever order the store lists them in.
    fn scoped_pair() -> (SiteLabel, SiteLabel, LabelledEnd, LabelledEnd) {
        let label = |scope: Option<&str>, handler: usize, name: bool, param: u32| SiteLabel {
            package: "fixture-router".into(),
            export: "Router".into(),
            member: Some("get".into()),
            role: "server_framework".into(),
            op: Some("receive".into()),
            side: None,
            name: name.then_some(ArgSlot { arg: 0, key: None }),
            payload: None,
            handler: Some(ArgSlot {
                arg: handler,
                key: None,
            }),
            method: None,
            reserved: vec![],
            runs: None,
            request: Some(RequestAt {
                param,
                path: Some(vec!["path".into()]),
                url: None,
                method: None,
                body: None,
            }),
            scope: scope.map(|step| super::super::decided_sites::LabelScope {
                base: "instance:()".into(),
                step: step.into(),
                names: false,
            }),
        };
        let end = |scope: Option<&str>| LabelledEnd {
            package: "fixture-router".into(),
            export: "Router".into(),
            member: Some("get".into()),
            role: "server_framework".into(),
            op: Some("receive".into()),
            file: "a.ts".into(),
            line: 1,
            span_start: 0,
            span_end: 1,
            new: false,
            scope: scope.map(str::to_string),
        };
        (
            label(None, 1, true, 0),
            label(Some("at"), 0, false, 1),
            end(None),
            end(Some("at")),
        )
    }

    #[test]
    fn a_router_get_and_a_scoped_get_read_their_own_labels() {
        let (router, scoped, router_end, scoped_end) = scoped_pair();
        let path = Vec::<String>::new();
        for labels in [
            vec![router.clone(), scoped.clone()],
            vec![scoped.clone(), router.clone()],
        ] {
            // request_of's read: the one request each call's label names.
            let (handler, request) = request_read(&labels, &router_end).unwrap();
            assert_eq!((handler.as_ref().unwrap().arg, request.param), (1, 0));
            let (handler, request) = request_read(&labels, &scoped_end).unwrap();
            assert_eq!((handler.as_ref().unwrap().arg, request.param), (0, 1));
            // receive_at's handler slot.
            assert!(handler_at(&labels, &router_end, 1, &path));
            assert!(!handler_at(&labels, &router_end, 0, &path));
            assert!(handler_at(&labels, &scoped_end, 0, &path));
            assert!(!handler_at(&labels, &scoped_end, 1, &path));
            // label_handler's path slot: only the router's get names one.
            assert!(names_path(&labels, &router_end));
            assert!(!names_path(&labels, &scoped_end));
        }
    }

    #[test]
    fn without_a_scoped_label_the_router_label_is_read_as_before() {
        let (router, _, router_end, _) = scoped_pair();
        let labels = vec![router];
        assert!(request_read(&labels, &router_end).is_ok());
        assert!(handler_at(&labels, &router_end, 1, &[]));
        assert!(names_path(&labels, &router_end));
    }

    fn the_fixture() -> Option<Walked> {
        walked("dispatch-routes", ".", "web", &[], true)
    }

    /// Two reads of one tree state the same records.
    #[test]
    fn two_reads_of_one_tree_state_the_same_records() {
        let (Some(first), Some(second)) = (the_fixture(), the_fixture()) else {
            return;
        };
        let exports = claims(|_| {});
        assert_eq!(
            records_of(&first, &exports, HoleReach::Nested),
            records_of(&second, &exports, HoleReach::Nested)
        );
    }

    fn records_of(
        walked: &Walked,
        exports: &[ExportClaims],
        reach: HoleReach,
    ) -> serde_json::Value {
        let labels = SiteLabel::from_exports(exports);
        let records = handler_records(
            &walked.inputs,
            &discovered(walked, exports),
            &labels,
            &walked.repo,
            reach,
        );
        serde_json::to_value(&records).unwrap()
    }

    fn records_key() -> serde_json::Value {
        serde_json::from_str(
            &std::fs::read_to_string(fixture("records.expected.json")).expect("the key reads"),
        )
        .expect("the key is JSON")
    }

    /// carrick#2474 R4b: one record per request handler, holding every
    /// comparison only it reaches (a hole in it or in a function written
    /// inside it among them), and one shared record per function for a
    /// comparison several handlers reach. What the code proves is forced;
    /// the rest is asked.
    #[test]
    fn each_handler_asks_what_it_does_not_state() {
        let Some(on) = the_fixture() else {
            return;
        };
        let got = records_of(&on, &claims(|_| {}), HoleReach::Nested);
        assert_eq!(
            got,
            records_key(),
            "records:\n{}",
            serde_json::to_string_pretty(&got).unwrap()
        );
        // Every record's asked ids name its comparisons; the wire type reads
        // each record back unchanged.
        for record in got["records"].as_array().unwrap() {
            let typed: crate::site_questions::HandlerRecord =
                serde_json::from_value(record.clone()).unwrap();
            assert_eq!(&serde_json::to_value(&typed).unwrap(), record);
            assert!(
                typed
                    .asked
                    .iter()
                    .all(|id| typed.comparisons.iter().any(|c| c.id == *id))
            );
            // `forced` names asked comparisons only, as the cloud checks.
            assert!(
                typed
                    .forced
                    .keys()
                    .all(|id| typed.asked.iter().any(|asked| asked.to_string() == *id)),
                "{}",
                typed.subject.at
            );
        }
    }

    /// A record as the cloud keys it (`withoutPositions` in the
    /// site-questions lambda): each `at` without its line, each `code`
    /// string without its line numbers.
    fn without_positions(value: &serde_json::Value, key: Option<&str>) -> serde_json::Value {
        use serde_json::Value;
        let line = regex::Regex::new(r"^(.+?):\d+(?::\d+)?$").unwrap();
        let number = regex::Regex::new(r"(?m)^\d+:( |$)").unwrap();
        match value {
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|item| without_positions(item, None))
                    .collect(),
            ),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), without_positions(v, Some(k))))
                    .collect(),
            ),
            Value::String(text) if matches!(key, Some("at" | "declaration")) => {
                Value::String(line.replace(text, "$1").into_owned())
            }
            Value::String(text) if key == Some("code") => {
                Value::String(number.replace_all(text, "").into_owned())
            }
            other => other.clone(),
        }
    }

    /// Lines added above a handler, and above a function its comparison is
    /// written in, move no record's key: every position is under `at`.
    #[test]
    fn a_record_moves_no_key_with_an_edit_above_it() {
        let read = |name: &str| {
            std::fs::read_to_string(fixture("repo").join(name)).expect("the fixture reads")
        };
        let edge = format!("\n\n{}", read("src/edge.ts"));
        let report = format!("// a note\n\n{}", read("src/report.ts"));
        let (Some(before), Some(after)) = (
            the_fixture(),
            walked(
                "dispatch-routes",
                ".",
                "web",
                &[
                    ("src/edge.ts", edge.as_str()),
                    ("src/report.ts", report.as_str()),
                ],
                true,
            ),
        ) else {
            return;
        };
        let exports = claims(|_| {});
        let before = records_of(&before, &exports, HoleReach::Nested);
        let after = records_of(&after, &exports, HoleReach::Nested);
        assert_ne!(before, after, "the edit moves positions");
        assert_eq!(
            without_positions(&before["records"], None),
            without_positions(&after["records"], None)
        );
    }

    /// What the answer key is written against: today's chains and
    /// discovery's sites on the fixture, and this pass's output.
    #[test]
    #[ignore = "prints the fixture's chains, sites and records"]
    fn probe() {
        let Some(on) = the_fixture() else {
            return;
        };
        let exports = claims(|_| {});
        let chains = super::super::guards::guard_chains(&on.inputs, &on.repo);
        crate::outln!("{}", serde_json::to_string_pretty(&chains).unwrap());
        crate::outln!(
            "{}",
            serde_json::to_string_pretty(&discovered(&on, &exports)).unwrap()
        );
        crate::outln!(
            "{}",
            serde_json::to_string_pretty(&records_of(&on, &exports, HoleReach::Nested)).unwrap()
        );
    }
}

/// The handler records and dispatch rows on real repos (carrick#2474 R4b):
/// offline. Each repo needs a flag-on scan's rows (`<blobs>/positions`),
/// discovery's sites for it written by `discovery_census` with
/// `CARRICK_RECEIVER_TRACE=1`, the store's answer for its packages, and the
/// scan's blobs:
///
/// `CARRICK_DISPATCH_CENSUS='<label>=<repo>=<positions dir>=<sites.json>=<store answer>=<blobs dir>[;…]'
/// [CARRICK_DISPATCH_CENSUS_OUT=<file>] [CARRICK_DISPATCH_CENSUS_RECORDS_DIR=<dir>]
/// [CARRICK_DISPATCH_CENSUS_ANSWERS_DIR=<dir>]
/// cargo test --lib dispatch_census -- --ignored --nocapture`
///
/// With a records dir, each repository's asked handler records are written
/// to `<dir>/<label>.json` as the site-questions pack runner reads them
/// (each site under its scan id), and the what-if arm's added records to
/// `<dir>/<label>.what-if.json`. With an answers dir holding
/// `<label>.json`, a `/site-questions` response body for those records,
/// the census reads them into dispatch rows and possible calls, and sets
/// the rows beside the blob's endpoint rows. The out file carries other
/// repositories' paths and the records their code: both stay private.
#[cfg(test)]
mod census {
    use std::collections::{BTreeMap, HashSet};
    use std::path::Path;

    use serde::Serialize;

    use super::super::decided_sites::{DiscoveryFile, SiteLabel, relative};
    use super::{HoleReach, handler_records};
    use crate::config::Config;
    use crate::position_rows::PositionIndex;
    use crate::site_questions::{HandlerAnswers, HandlerQuestion};
    use crate::site_rows::dispatch::{dispatch_rows, handler_questions};

    #[derive(Debug, Default, Serialize)]
    struct RepoCensus {
        /// With answers: each dispatch row as `file:line METHOD path source
        /// confidence`, and each possible call as `file:line reason`.
        rows: Vec<String>,
        leads: Vec<String>,
        leads_by_reason: BTreeMap<String, usize>,
        not_a_route: usize,
        /// The blob's HTTP endpoint rows in files that hold a dispatch row.
        endpoint_rows: usize,
        /// Dispatch rows a blob row states: same method, the row's path ends
        /// with the dispatch row's, same line.
        same_line: Vec<String>,
        /// Same method and path, another line (the #2139 shape).
        other_line: Vec<String>,
        /// Dispatch rows no blob row states.
        extras: Vec<String>,
        /// Rows in those files no dispatch row states.
        unstated_rows: Vec<String>,
        /// The handler records (carrick#2474 R4b): their counts, what
        /// attaches to no handler, and per record `at kind key asked/all`.
        records: BTreeMap<String, usize>,
        records_not_attached: BTreeMap<String, usize>,
        per_record: Vec<String>,
        /// The what-if arm: holes one call below a handler attached too.
        what_if: BTreeMap<String, usize>,
        records_ms: u128,
    }

    /// Questions as the site-questions request's `sites`.
    fn sites_of(questions: Vec<HandlerQuestion>) -> Vec<serde_json::Value> {
        questions
            .into_iter()
            .map(|question| serde_json::to_value(question).expect("a question serialises"))
            .collect()
    }

    /// The blob's HTTP endpoint rows: (method, path, file, line).
    fn endpoint_rows(blobs: &Path) -> Vec<(String, String, String, u32)> {
        let mut rows = Vec::new();
        for entry in std::fs::read_dir(blobs).expect("the blobs dir reads") {
            let path = entry.expect("dir entry").path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Ok(blob) =
                serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&path).unwrap())
            else {
                continue;
            };
            for endpoint in blob["endpoints"].as_array().into_iter().flatten() {
                let key = &endpoint["key"];
                if key["protocol"] != "http" {
                    continue;
                }
                let (Some(method), Some(at), Some(file_line)) = (
                    key["method"].as_str(),
                    key["path"].as_str(),
                    endpoint["file_path"].as_str(),
                ) else {
                    continue;
                };
                let Some((file, line)) = file_line.rsplit_once(':') else {
                    continue;
                };
                rows.push((
                    method.to_uppercase(),
                    at.to_string(),
                    file.to_string(),
                    line.parse().unwrap_or_default(),
                ));
            }
        }
        rows.sort();
        rows.dedup();
        rows
    }

    fn census_repo(
        root: &Path,
        positions: &Path,
        sites: &Path,
        claims: &Path,
        blobs: &Path,
        answers: Option<&HandlerAnswers>,
    ) -> (RepoCensus, Vec<serde_json::Value>, Vec<serde_json::Value>) {
        let root = std::fs::canonicalize(root).expect("the repo exists");
        let config = root.join("carrick.json");
        let services = if config.exists() {
            Config::load_services(vec![config]).expect("carrick.json loads")
        } else {
            vec![Config::default()]
        };
        let discovered = serde_json::from_str::<DiscoveryFile>(
            &std::fs::read_to_string(sites).expect("the sites file reads"),
        )
        .expect("the sites file parses")
        .sites;
        let exports = crate::library_store::parse_answer(
            &std::fs::read_to_string(claims).expect("the claims read"),
        )
        .exports;
        let labels = SiteLabel::from_exports(&exports);
        let mut by_service = BTreeMap::new();
        for entry in std::fs::read_dir(positions).expect("the positions dir reads") {
            let path = entry.expect("dir entry").path();
            if path.to_string_lossy().ends_with(".jsonl.gz")
                && let Ok(rows) = crate::position_rows::read_rows_file(&path)
            {
                by_service.insert(rows.header.service.clone(), path);
            }
        }
        let mut census = RepoCensus::default();
        // Each dispatch row: (method, path, file, line).
        let mut stated_rows: Vec<(String, String, String, u32)> = Vec::new();
        let mut asked_sites: Vec<serde_json::Value> = Vec::new();
        let mut what_if_sites: Vec<serde_json::Value> = Vec::new();
        for service in &services {
            let label = service
                .service_name
                .clone()
                .unwrap_or_else(|| "(root)".to_string());
            let index = match by_service.get(&label) {
                Some(path) => PositionIndex::of(
                    &root,
                    crate::position_rows::read_rows_file(path).expect("rows read"),
                ),
                None => PositionIndex::missing(&root),
            };
            let inputs = crate::engine::discover_service_inputs(&root, service, Some(index));
            let files: HashSet<String> = inputs
                .files
                .keys()
                .map(|file| relative(&root, file))
                .collect();
            let mine: Vec<_> = discovered
                .iter()
                .filter(|site| files.contains(&site.file))
                .cloned()
                .collect();
            let started = std::time::Instant::now();
            let records = handler_records(&inputs, &mine, &labels, &root, HoleReach::Nested);
            census.records_ms += started.elapsed().as_millis();
            let again = handler_records(&inputs, &mine, &labels, &root, HoleReach::Nested);
            assert_eq!(
                serde_json::to_string(&records).unwrap(),
                serde_json::to_string(&again).unwrap(),
                "two reads of {label} state the same records"
            );
            let what_if = handler_records(&inputs, &mine, &labels, &root, HoleReach::OneCallBelow);
            for (what, n) in &records.counts {
                *census.records.entry(what.clone()).or_default() += n;
            }
            for (what, n) in &records.not_attached {
                *census.records_not_attached.entry(what.clone()).or_default() += n;
            }
            for (what, n) in &what_if.counts {
                *census.what_if.entry(what.clone()).or_default() += n;
            }
            for record in &records.records {
                census.per_record.push(format!(
                    "{} {:?} {} asked {}/{}",
                    record.subject.at,
                    record.subject.kind,
                    record.subject.key,
                    record.asked.len(),
                    record.comparisons.len()
                ));
            }
            asked_sites.extend(sites_of(handler_questions(&records)));
            let base: Vec<serde_json::Value> = records
                .records
                .iter()
                .map(|record| serde_json::to_value(record).unwrap())
                .collect();
            let added: Vec<_> = what_if
                .records
                .into_iter()
                .filter(|record| !base.contains(&serde_json::to_value(record).unwrap()))
                .collect();
            what_if_sites.extend(sites_of(handler_questions(&super::HandlerRecords {
                records: added,
                ..Default::default()
            })));
            let Some(answers) = answers else {
                continue;
            };
            let dispatch = dispatch_rows(&records, answers);
            census.not_a_route += dispatch.counts.get("not_a_route").copied().unwrap_or(0);
            for row in &dispatch.rows {
                let crate::operation::OperationKey::Http { method, path } = &row.key else {
                    continue;
                };
                census.rows.push(format!(
                    "{}:{} {method} {path} {:?} {}",
                    row.file,
                    row.line,
                    row.source,
                    row.inference
                        .as_ref()
                        .map_or("fact".to_string(), |answer| format!(
                            "{}{}",
                            answer.confidence,
                            if answer.orders_agree {
                                ""
                            } else {
                                " orders-disagree"
                            }
                        ))
                ));
                stated_rows.push((method.clone(), path.clone(), row.file.clone(), row.line));
            }
            for lead in &dispatch.leads {
                let reason = serde_json::to_value(lead.unread.reason)
                    .ok()
                    .and_then(|reason| reason.as_str().map(str::to_string))
                    .unwrap_or_default();
                census
                    .leads
                    .push(format!("{}:{} {reason}", lead.file_path, lead.line_number));
                *census.leads_by_reason.entry(reason).or_default() += 1;
            }
        }
        let rows = endpoint_rows(blobs);
        let route_files: HashSet<&str> = stated_rows.iter().map(|r| r.2.as_str()).collect();
        let in_files: Vec<_> = rows
            .iter()
            .filter(|(_, _, file, _)| route_files.contains(file.as_str()))
            .collect();
        census.endpoint_rows = in_files.len();
        let mut stated: HashSet<usize> = HashSet::new();
        for (route_method, route_path, route_file, route_line) in &stated_rows {
            let at = format!("{route_file}:{route_line} {route_method} {route_path}");
            let matching: Vec<(usize, bool)> = in_files
                .iter()
                .enumerate()
                .filter(|(_, (method, path, file, _))| {
                    file == route_file && method == route_method && path.ends_with(route_path)
                })
                .map(|(index, (_, _, _, line))| (index, line == route_line))
                .collect();
            if let Some((index, _)) = matching.iter().find(|(_, on_line)| *on_line) {
                stated.insert(*index);
                census.same_line.push(at);
            } else if let Some((index, _)) = matching.first() {
                stated.insert(*index);
                census.other_line.push(at);
            } else {
                census.extras.push(at);
            }
        }
        for (index, (method, path, file, line)) in in_files.iter().enumerate() {
            if !stated.contains(&index) {
                census
                    .unstated_rows
                    .push(format!("{file}:{line} {method} {path}"));
            }
        }
        (census, asked_sites, what_if_sites)
    }

    /// `sites` as a records file the site-questions pack runner reads.
    fn write_records(path: &Path, label: &str, sites: Vec<serde_json::Value>) {
        let file = serde_json::json!({
            "repo": label,
            "service": null,
            "question_set": null,
            "sites": sites,
        });
        std::fs::write(path, serde_json::to_string_pretty(&file).unwrap())
            .expect("the records write");
    }

    #[test]
    #[ignore = "a census over real repos: set CARRICK_DISPATCH_CENSUS"]
    fn dispatch_census() {
        let Ok(spec) = std::env::var("CARRICK_DISPATCH_CENSUS") else {
            crate::errln!("CARRICK_DISPATCH_CENSUS is not set");
            return;
        };
        let mut out: BTreeMap<String, RepoCensus> = BTreeMap::new();
        for repo in spec.split(';').filter(|part| !part.is_empty()) {
            let parts: Vec<&str> = repo.split('=').collect();
            let [label, root, positions, sites, claims, blobs] = parts.as_slice() else {
                panic!("expected <label>=<repo>=<positions>=<sites>=<answer>=<blobs>, got {repo}");
            };
            let answers = std::env::var("CARRICK_DISPATCH_CENSUS_ANSWERS_DIR")
                .ok()
                .and_then(|dir| {
                    std::fs::read_to_string(Path::new(&dir).join(format!("{label}.json"))).ok()
                })
                .map(|text| {
                    let (question_set, read) =
                        crate::site_questions::read_response::<HandlerQuestion>(&text);
                    let mut answers = HandlerAnswers {
                        question_set,
                        ..HandlerAnswers::default()
                    };
                    for (site_id, answer) in read {
                        match answer {
                            Ok(routes) => {
                                answers.answered.insert(site_id, routes);
                            }
                            Err(failed) => {
                                answers.unanswered.insert(site_id, failed);
                            }
                        }
                    }
                    answers
                });
            let (census, asked, what_if) = census_repo(
                Path::new(root),
                Path::new(positions),
                Path::new(sites),
                Path::new(claims),
                Path::new(blobs),
                answers.as_ref(),
            );
            // The records are the repository's code: private files only.
            if let Ok(dir) = std::env::var("CARRICK_DISPATCH_CENSUS_RECORDS_DIR") {
                let dir = Path::new(&dir);
                std::fs::create_dir_all(dir).expect("the records dir");
                write_records(&dir.join(format!("{label}.json")), label, asked);
                write_records(&dir.join(format!("{label}.what-if.json")), label, what_if);
            }
            out.insert(label.to_string(), census);
        }
        let text = serde_json::to_string_pretty(&out).expect("the census serialises");
        match std::env::var("CARRICK_DISPATCH_CENSUS_OUT") {
            Ok(path) => std::fs::write(path, &text).expect("the census writes"),
            Err(_) => crate::outln!("{text}"),
        }
    }
}
