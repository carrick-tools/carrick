//! Handler records (carrick#2474 R4b, T3a): each request handler and every
//! path and method comparison inside it, as one record a model reads. The
//! code finds where; the model says what each comparison means there.
//!
//! - **The handler** is found by code, never by name: an *entry* (a keyed
//!   function the module exports that nothing in the repository calls or
//!   hands on), or a *label* handler (the function handed in the handler
//!   slot of a call discovery labels with op `receive`).
//! - **Its comparisons** are the path comparisons whose chain reaches it
//!   (the label arm or the entry arm, as [`super`] reads them), a label
//!   that names no request member, and those whose chain stops at a hole in
//!   the handler or in a function written inside it. Callers are not
//!   followed: a hole in a function the handler calls has no handler (the
//!   census's what-if arm alone attaches it, [`HoleReach::OneCallBelow`]).
//! - **A comparison several handlers reach** is asked once, in a *shared*
//!   record of the function it is written in, listing those handlers.
//! - **The slot rule:** a value the code proves is forced (the path on a
//!   proven label read, the method when exactly one qualifies); the rest is
//!   asked. The method is the code's fact, never the model's choice
//!   (carrick#2474, ruling of 2026-10-23 on the live test): a comparison no
//!   one method qualifies for is `no_method`, so a forced path with no
//!   forced method is asked nothing (a `no_method` lead), and an asked one
//!   is asked only whether it is a route.
//! - **Where a route stands** ([`ComparisonPlace`]): its comparison's line,
//!   and whether a row may be written there (every handler reaching it
//!   serves the compared path itself).
//!
//! Every position sits under a key named `at`, and code is numbered as a
//! site's `point.code` is: the cloud's cache key drops both, so an edit
//! above a handler asks nothing again. Record-only: a scan with the
//! receiver trace on counts the records and drops them.
//!
//! Plan: carrick#2386 comment 6341789125, sections 1-2. Wire shape:
//! `tests/fixtures/site_questions/contract.json` (`handler`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use serde::Serialize;

use super::super::RequestSummaryInputs;
use super::super::decided_sites::{DiscoverySite, LabelledEnd, SiteLabel};
use super::super::guards::{
    Branch, ChainEnd, ChainFacts, Chains, GuardBranch, GuardChain, GuardIr, GuardPosition,
    GuardShape,
};
use super::super::site_record::numbered;
use super::{MethodGuard, Reached, Reader, count, end_kind, method_of, method_reaching};
use crate::site_questions::{
    CodeWindow, ComparisonHole, ComparisonPosition, ComparisonRead, ForcedRoute, HandlerComparison,
    HandlerRecord, HandlerRef, HandlerSubject, MAX_SHARED_HANDLERS, MethodLiteral, ScopePrefix,
    SubjectKind, SubjectLabel,
};
use crate::type_manifest::is_http_method;

/// A function up to this many lines is sent whole; a longer one as its
/// first line and a window around each comparison.
const WHOLE_LINES: u32 = 120;
/// Lines kept before and after a comparison in a window.
const BEFORE: u32 = 4;
const AFTER: u32 = 2;

/// Where a hole's comparison may attach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoleReach {
    /// In the handler, or in a function written inside it: what a scan asks.
    Nested,
    /// Also in a function one call below a handler: the census's what-if
    /// arm alone (not asked in v1, carrick#2386 decision 3).
    OneCallBelow,
}

/// A service's handler records, sorted, and what attaches to none.
#[derive(Debug, Default, Serialize)]
pub struct HandlerRecords {
    pub records: Vec<HandlerRecord>,
    /// Per record, in the same order, the values the code settles for the
    /// comparisons nothing is asked of (path and method forced, or a path
    /// forced with no method stated): the fact rows and `no_method` leads
    /// of T3b. The wire's `forced` names asked comparisons only.
    pub settled: Vec<BTreeMap<String, ForcedRoute>>,
    /// Per record, in the same order, where each comparison's route would
    /// stand (T3b), by id.
    pub places: Vec<Vec<ComparisonPlace>>,
    /// Chains (per guard for `unresolved_constant`) of path comparisons
    /// that attach to no handler, by reason: R4a's closed list, plus
    /// `hole_outside_handler`.
    pub not_attached: BTreeMap<String, usize>,
    /// `records`, `records_shared`, `records_asked`, `handlers`,
    /// `comparisons`, `asked`, `forced` (path and method both forced),
    /// `no_method` (path forced, no method stated), `holes` (asked
    /// comparisons read from a hole), `methods_in_branch` (a method
    /// compared inside a path comparison's branch: one pick, one route),
    /// `holes_one_call_below` (the what-if arm's added chains).
    pub counts: BTreeMap<String, usize>,
}

/// A function's lines in its file, repo-relative.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct LineRange {
    pub file: String,
    pub first: u32,
    pub last: u32,
}

/// Where one comparison's route stands (carrick#2474 R4b T3b), beside the
/// record: nothing here is on the wire.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ComparisonPlace {
    pub id: u32,
    /// Repo-relative, `/`-separated.
    pub file: String,
    /// The compared literal's line (a table key's own line).
    pub line: u32,
    /// The comparison's span, in SWC's numbering.
    pub span_start: u32,
    pub span_end: u32,
    /// Whether the compared path is the path served: every handler that
    /// reaches the comparison is an entry, or the handler of a labelled
    /// `receive` with no `name` (path) slot; and a hole attached through
    /// nesting sits in no function that is handed to a call. Otherwise a
    /// route here needs a prefix the code does not compose: a lead.
    pub served: bool,
    /// The labelled `receive` the one handler is handed to, if it is one.
    pub label: Option<SubjectLabel>,
    /// The functions a reader row agreeing with the route folds within:
    /// each handler's, and the comparison's own.
    pub within: Vec<LineRange>,
}

/// What a handler is.
#[derive(Debug, Clone)]
enum Handler {
    Entry,
    /// The labelled `receive` call it is handed to, and whether a label of
    /// it names a path slot (the call registers the handler under a path).
    Label(SubjectLabel, bool),
}

impl Handler {
    /// Whether the handler sees the request's whole path.
    fn serves_whole_path(&self) -> bool {
        match self {
            Self::Entry => true,
            Self::Label(_, path_slot) => !path_slot,
        }
    }
}

/// How one chain of a comparison reaches its handler, best first.
#[derive(Clone)]
enum Arm {
    /// The label arm with the path read proven.
    Proven(Reached, ChainFacts),
    /// The entry arm.
    Entry(Reached, ChainFacts),
    /// The label arm, where the label names no request member.
    Unread(ChainFacts),
    /// A hole in the handler or in a function written inside it. `handed`:
    /// the hole sits in a function between it and the handler that is
    /// handed to a call (or attaches one call below), so nothing proves how
    /// that function is registered.
    Hole {
        reason: String,
        text: Option<String>,
        facts: ChainFacts,
        handed: bool,
    },
}

impl Arm {
    fn rank(&self) -> u8 {
        match self {
            Self::Proven(..) => 0,
            Self::Entry(..) => 1,
            Self::Unread(..) => 2,
            Self::Hole { .. } => 3,
        }
    }
}

/// The handlers a chain attaches to, and how.
struct Attached {
    to: Vec<(usize, Handler)>,
    arm: Arm,
    /// Attached one call below a handler (the what-if arm).
    below: bool,
}

/// One comparison and the handlers it reaches.
struct Compared<'g> {
    node: usize,
    guard: &'g GuardIr,
    literal: String,
    arms: BTreeMap<usize, Arm>,
}

/// A method or `startsWith` literal and the handlers it reaches.
struct Stated<'g> {
    node: usize,
    guard: &'g GuardIr,
    literal: String,
    handlers: BTreeSet<usize>,
}

/// Every handler record the guards in `inputs` state, read against
/// discovery's `discovered` sites and the store's `labels`, files relative
/// to `root`.
pub fn handler_records(
    inputs: &RequestSummaryInputs,
    discovered: &[DiscoverySite],
    labels: &[SiteLabel],
    root: &Path,
    reach: HoleReach,
) -> HandlerRecords {
    let chains = Chains::new(inputs, root);
    let reader = Reader {
        chains: &chains,
        discovered,
        labels,
    };
    let attacher = Attacher {
        chains: &chains,
        reader: &reader,
        reach,
    };
    let mut out = HandlerRecords::default();
    let mut methods: HashMap<usize, Vec<MethodGuard<'_>>> = HashMap::new();
    let mut paths: Vec<(usize, &GuardIr, String)> = Vec::new();
    for (node, at) in chains.nodes.iter().enumerate() {
        for guard in &at.ir.guards {
            let (text, shape, _) = chains.literal(node, &guard.literal);
            match shape {
                GuardShape::Unresolved => count(&mut out.not_attached, "unresolved_constant"),
                GuardShape::Path => paths.push((node, guard, text)),
                GuardShape::Name if is_http_method(&text) => {
                    methods.entry(node).or_default().push(MethodGuard {
                        guard,
                        method: text.trim().to_uppercase(),
                        chains: chains.traced(node, guard),
                    })
                }
                GuardShape::Name => {}
            }
        }
    }
    let mut handlers: BTreeMap<usize, Handler> = BTreeMap::new();
    // Each method guard (by address) and the handlers its chains reach
    // reading the request's method member: only those decide a method for a
    // comparison that has no anchor of its own (carrick#2693).
    let mut method_handlers: HashMap<usize, BTreeSet<usize>> = HashMap::new();
    let mut stated_methods: Vec<Stated<'_>> = Vec::new();
    let mut nodes: Vec<&usize> = methods.keys().collect();
    nodes.sort();
    for node in nodes {
        for candidate in &methods[node] {
            let mut decides = BTreeSet::new();
            for (chain, facts) in &candidate.chains {
                if chain.negated {
                    continue;
                }
                let reads_method = reader.reads_method(chain, facts);
                for (handler, kind) in attacher.attach_method(*node, chain, facts) {
                    handlers.entry(handler).or_insert(kind);
                    if reads_method {
                        decides.insert(handler);
                    }
                }
            }
            if paths.iter().any(|(at, path, _)| {
                at == node
                    && path.position != GuardPosition::StartsWith
                    && matches!(path.branch, GuardBranch::Span(lo, hi)
                        if lo <= candidate.guard.site.lo && candidate.guard.site.hi <= hi)
            }) {
                count(&mut out.counts, "methods_in_branch");
            }
            // A record shows the model only the methods the code states: a
            // literal compared with another value is no method (carrick#2693).
            if !decides.is_empty() {
                stated_methods.push(Stated {
                    node: *node,
                    guard: candidate.guard,
                    literal: candidate.method.clone(),
                    handlers: decides.clone(),
                });
            }
            method_handlers.insert(address(candidate.guard), decides);
        }
    }
    let mut compared: Vec<Compared<'_>> = Vec::new();
    let mut scopes: Vec<Stated<'_>> = Vec::new();
    for (node, guard, text) in &paths {
        let mut arms: BTreeMap<usize, Arm> = BTreeMap::new();
        let mut reached = BTreeSet::new();
        for (chain, facts) in chains.traced(*node, guard) {
            if chain.negated {
                count(&mut out.not_attached, "negated");
                continue;
            }
            let attached = match attacher.attach(*node, &chain, &facts) {
                Ok(attached) => attached,
                Err(reason) => {
                    count(&mut out.not_attached, &reason);
                    continue;
                }
            };
            if text.contains('?') || text.contains('#') {
                count(&mut out.not_attached, "literal_has_query");
                continue;
            }
            if attached.below {
                count(&mut out.counts, "holes_one_call_below");
            }
            for (handler, kind) in attached.to {
                handlers.entry(handler).or_insert(kind);
                reached.insert(handler);
                let better = arms
                    .get(&handler)
                    .is_none_or(|kept| attached.arm.rank() < kept.rank());
                if better {
                    arms.insert(handler, attached.arm.clone());
                }
            }
        }
        if guard.position == GuardPosition::StartsWith {
            if !reached.is_empty() {
                scopes.push(Stated {
                    node: *node,
                    guard,
                    literal: text.clone(),
                    handlers: reached,
                });
            }
        } else if !arms.is_empty() {
            compared.push(Compared {
                node: *node,
                guard,
                literal: text.clone(),
                arms,
            });
        }
    }
    // A comparison one handler reaches is that handler's; one several reach
    // is asked once, in a record of the function it is written in.
    let mut own: BTreeMap<usize, Vec<&Compared<'_>>> = BTreeMap::new();
    let mut shared: BTreeMap<usize, Vec<&Compared<'_>>> = BTreeMap::new();
    for comparison in &compared {
        if comparison.arms.len() == 1 {
            let handler = *comparison.arms.keys().next().expect("one arm");
            own.entry(handler).or_default().push(comparison);
        } else {
            shared.entry(comparison.node).or_default().push(comparison);
        }
    }
    let builder = Builder {
        chains: &chains,
        methods: &methods,
        method_handlers: &method_handlers,
        handlers: &handlers,
        texts: std::cell::RefCell::new(HashMap::new()),
    };
    let mut records: Vec<(Order, HandlerRecord, Vec<ComparisonPlace>)> = Vec::new();
    for (handler, comparisons) in &own {
        let reached = BTreeSet::from([*handler]);
        let (kind, label) = match &handlers[handler] {
            Handler::Entry => (SubjectKind::Entry, None),
            Handler::Label(label, _) => (SubjectKind::Label, Some(label.clone())),
        };
        let keep = |item: &Stated<'_>| item.handlers.contains(handler);
        records.push(builder.build(
            *handler,
            kind,
            label,
            &reached,
            comparisons,
            &stated_where(&stated_methods, keep),
            &stated_where(&scopes, keep),
            &mut out.counts,
        ));
    }
    for (subject, comparisons) in &shared {
        let reached: BTreeSet<usize> = comparisons
            .iter()
            .flat_map(|comparison| comparison.arms.keys().copied())
            .collect();
        let keep =
            |item: &Stated<'_>| item.node == *subject && !item.handlers.is_disjoint(&reached);
        records.push(builder.build(
            *subject,
            SubjectKind::Shared,
            None,
            &reached,
            comparisons,
            &stated_where(&stated_methods, keep),
            &stated_where(&scopes, keep),
            &mut out.counts,
        ));
    }
    records.sort_by(|(a, ..), (b, ..)| a.cmp(b));
    count_records(&mut out.counts, &records);
    // The wire's `forced` names asked comparisons only; the values of one
    // nothing is asked of stay beside the record.
    for (_, mut record, places) in records {
        out.places.push(places);
        let (forced, settled): (BTreeMap<_, _>, BTreeMap<_, _>) =
            std::mem::take(&mut record.forced)
                .into_iter()
                .partition(|(id, _)| record.asked.iter().any(|asked| asked.to_string() == *id));
        record.forced = forced;
        out.records.push(record);
        out.settled.push(settled);
    }
    out
}

/// The line ranges of a function on lines `start..=end` that its code shows:
/// the whole function up to [`WHOLE_LINES`] lines; a longer one as its first
/// line and [`BEFORE`] lines before and [`AFTER`] after each line in
/// `marks`, ranges that touch or overlap merged.
fn windows(start: u32, end: u32, marks: &BTreeSet<u32>) -> Vec<(u32, u32)> {
    if end - start < WHOLE_LINES {
        return vec![(start, end)];
    }
    let mut ranges: Vec<(u32, u32)> = vec![(start, start)];
    for line in marks.iter().filter(|line| **line >= start && **line <= end) {
        ranges.push((
            line.saturating_sub(BEFORE).max(start),
            (line + AFTER).min(end),
        ));
    }
    ranges.sort();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (lo, hi) in ranges {
        match merged.last_mut() {
            Some(last) if lo <= last.1 + 1 => last.1 = last.1.max(hi),
            _ => merged.push((lo, hi)),
        }
    }
    merged
}

/// The literals in `list` that `keep` keeps: function, guard and text.
fn stated_where<'g>(
    list: &[Stated<'g>],
    keep: impl Fn(&Stated<'g>) -> bool,
) -> Vec<(usize, &'g GuardIr, String)> {
    list.iter()
        .filter(|item| keep(item))
        .map(|item| (item.node, item.guard, item.literal.clone()))
        .collect()
}

/// A guard's identity while this pass runs.
fn address(guard: &GuardIr) -> usize {
    guard as *const GuardIr as usize
}

/// The record counts, and the handlers the records name.
fn count_records(
    counts: &mut BTreeMap<String, usize>,
    records: &[(Order, HandlerRecord, Vec<ComparisonPlace>)],
) {
    let mut reached: BTreeSet<String> = BTreeSet::new();
    for (_, record, _) in records {
        count(counts, "records");
        if record.subject.kind == SubjectKind::Shared {
            count(counts, "records_shared");
            for handler in record.subject.handlers.iter().flatten() {
                reached.insert(handler.at.clone());
            }
        } else {
            reached.insert(record.subject.at.clone());
        }
        if !record.asked.is_empty() {
            count(counts, "records_asked");
        }
    }
    counts.insert("handlers".to_string(), reached.len());
}

struct Attacher<'r, 'a> {
    chains: &'r Chains<'a>,
    reader: &'r Reader<'r, 'a>,
    reach: HoleReach,
}

impl Attacher<'_, '_> {
    /// The handlers `chain`, of a guard written in `node`, reaches, or why
    /// it reaches none.
    fn attach(
        &self,
        node: usize,
        chain: &GuardChain,
        facts: &ChainFacts,
    ) -> Result<Attached, String> {
        if let Some(handoff) = &facts.handoff {
            return match self.reader.labelled(chain, handoff) {
                Ok(reached) => {
                    let end = self.reader.end_of(handoff).expect("the label arm");
                    Ok(Attached {
                        to: vec![(handoff.handed, self.label_handler(end))],
                        arm: Arm::Proven(reached, facts.clone()),
                        below: false,
                    })
                }
                // The label names no request member: the handler is still the
                // label's when it is handed in the label's handler slot.
                Err(reason) if reason == "label_without_request" => match self.reader.receive_at(
                    handoff.caller,
                    &handoff.site,
                    handoff.arg,
                    &handoff.path,
                ) {
                    Some(end) => Ok(Attached {
                        to: vec![(handoff.handed, self.label_handler(end))],
                        arm: Arm::Unread(facts.clone()),
                        below: false,
                    }),
                    None => Err("handler_slot_mismatch".to_string()),
                },
                Err(reason) => Err(reason),
            };
        }
        if let (ChainEnd::EntryPoint { .. }, Some(entry)) = (&chain.end, &facts.entry) {
            let reached = self.reader.reached(chain, facts)?;
            return Ok(Attached {
                to: vec![(entry.node, Handler::Entry)],
                arm: Arm::Entry(reached, facts.clone()),
                below: false,
            });
        }
        let ChainEnd::Hole { text, .. } = &chain.end else {
            return Err(format!("end:{}", end_kind(&chain.end)));
        };
        let arm = |handed: bool| Arm::Hole {
            reason: end_kind(&chain.end),
            text: text.clone(),
            facts: facts.clone(),
            handed,
        };
        let stopped = facts.stopped.unwrap_or(node);
        if let Some(found) = self.enclosing(stopped) {
            let handed = self.handed_between(stopped, found.0);
            return Ok(Attached {
                to: vec![found],
                arm: arm(handed),
                below: false,
            });
        }
        if self.reach == HoleReach::OneCallBelow {
            let mut to: Vec<(usize, Handler)> = Vec::new();
            for up in self.chains.one_call_up(stopped) {
                if let Some(found) = self.enclosing(up)
                    && !to.iter().any(|(handler, _)| *handler == found.0)
                {
                    to.push(found);
                }
            }
            if !to.is_empty() {
                return Ok(Attached {
                    to,
                    arm: arm(true),
                    below: true,
                });
            }
        }
        Err("hole_outside_handler".to_string())
    }

    /// The handlers a method literal's `chain` reaches: a path read is not
    /// asked of it, so on the label arm the handler slot is enough. The
    /// literal decides a method only where the chain also reads the
    /// request's method member ([`Reader::reads_method`]).
    fn attach_method(
        &self,
        node: usize,
        chain: &GuardChain,
        facts: &ChainFacts,
    ) -> Vec<(usize, Handler)> {
        if let Some(handoff) = &facts.handoff {
            return self
                .reader
                .receive_at(handoff.caller, &handoff.site, handoff.arg, &handoff.path)
                .map(|end| vec![(handoff.handed, self.label_handler(end))])
                .unwrap_or_default();
        }
        self.attach(node, chain, facts)
            .map(|attached| attached.to)
            .unwrap_or_default()
    }

    /// `node` itself or the nearest function it is written in that is a
    /// request handler.
    fn enclosing(&self, node: usize) -> Option<(usize, Handler)> {
        let mut at = Some(node);
        while let Some(current) = at {
            if self.chains.is_entry(current) {
                return Some((current, Handler::Entry));
            }
            for (caller, site, arg, path) in self.chains.handed_calls(current) {
                if let Some(end) = self.reader.receive_at(caller, &site, arg, &path) {
                    return Some((current, self.label_handler(end)));
                }
            }
            at = self.chains.nodes[current].parent;
        }
        None
    }

    /// The handler of a labelled `receive` call: with a path slot when any
    /// `receive` label of that member names one (the call takes the path
    /// the handler is registered under).
    fn label_handler(&self, end: &LabelledEnd) -> Handler {
        let path_slot = super::names_path(self.reader.labels, end);
        Handler::Label(subject_label(end), path_slot)
    }

    /// Whether a function from `stopped` up to `handler` (not the handler
    /// itself) is handed to a call: a function written inside a handler and
    /// handed on is registered somewhere the code does not show.
    fn handed_between(&self, stopped: usize, handler: usize) -> bool {
        let mut at = Some(stopped);
        while let Some(current) = at
            && current != handler
        {
            if !self.chains.handed_calls(current).is_empty() {
                return true;
            }
            at = self.chains.nodes[current].parent;
        }
        false
    }
}

fn subject_label(end: &LabelledEnd) -> SubjectLabel {
    SubjectLabel {
        package: end.package.clone(),
        export: end.export.clone(),
        member: end.member.clone(),
    }
}

/// Where a record sorts: its subject's file and line, kind, key.
type Order = (String, u32, u8, String);

struct Builder<'b, 'a> {
    chains: &'b Chains<'a>,
    methods: &'b HashMap<usize, Vec<MethodGuard<'b>>>,
    method_handlers: &'b HashMap<usize, BTreeSet<usize>>,
    handlers: &'b BTreeMap<usize, Handler>,
    /// Each file's text, read once.
    texts: std::cell::RefCell<HashMap<std::path::PathBuf, String>>,
}

impl Builder<'_, '_> {
    fn text(&self, node: usize) -> String {
        let file = self.chains.nodes[node].file;
        self.texts
            .borrow_mut()
            .entry(file.to_path_buf())
            .or_insert_with(|| crate::parser::read_source(file).unwrap_or_default())
            .clone()
    }

    /// The first and last line of `node`'s function.
    fn lines(&self, node: usize, text: &str) -> (u32, u32) {
        let last = text.lines().count().max(1) as u32;
        match self.chains.nodes[node].ir.site {
            Some(site) => {
                let end = (site
                    .span_end
                    .saturating_sub(crate::swc_scanner::SWC_SPAN_BASE)
                    as usize)
                    .min(text.len());
                let end_line = 1 + text.as_bytes()[..end]
                    .iter()
                    .filter(|byte| **byte == b'\n')
                    .count() as u32;
                (site.line, end_line.max(site.line))
            }
            None => (1, last),
        }
    }

    /// `node`'s function, numbered: whole up to [`WHOLE_LINES`], else its
    /// first line and a window around each line in `marks`.
    fn code(&self, node: usize, marks: &BTreeSet<u32>) -> String {
        let text = self.text(node);
        let (start, end) = self.lines(node, &text);
        windows(start, end, marks)
            .iter()
            .map(|(lo, hi)| numbered(&text, *lo, *hi))
            .collect::<Vec<_>>()
            .join("\n...\n")
    }

    fn at(&self, node: usize, line: u32) -> String {
        format!("{}:{line}", self.chains.rel(self.chains.nodes[node].file))
    }

    fn params(&self, node: usize) -> Vec<String> {
        self.chains.nodes[node]
            .ir
            .params
            .iter()
            .map(|param| {
                param
                    .as_ref()
                    .map_or_else(|| "(pattern)".to_string(), |decl| decl.key.0.clone())
            })
            .collect()
    }

    /// The path and method `arm`, of `comparison` reaching `handler`, forces.
    fn forced(
        &self,
        comparison: &Compared<'_>,
        handler: usize,
        arm: &Arm,
    ) -> (Option<String>, Option<String>) {
        let by_handler = |facts: &ChainFacts| {
            method_reaching(
                comparison.node,
                comparison.guard,
                facts,
                self.methods,
                &|candidate: &MethodGuard<'_>| {
                    self.method_handlers
                        .get(&address(candidate.guard))
                        .is_some_and(|reached| reached.contains(&handler))
                },
            )
        };
        match arm {
            Arm::Proven(reached, facts) => (
                Some(comparison.literal.clone()),
                method_of(
                    comparison.node,
                    comparison.guard,
                    facts,
                    reached,
                    self.methods,
                ),
            ),
            Arm::Entry(reached, facts) => (
                None,
                method_of(
                    comparison.node,
                    comparison.guard,
                    facts,
                    reached,
                    self.methods,
                ),
            ),
            Arm::Unread(facts) | Arm::Hole { facts, .. } => (None, by_handler(facts)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        &self,
        subject: usize,
        kind: SubjectKind,
        label: Option<SubjectLabel>,
        reached: &BTreeSet<usize>,
        comparisons: &[&Compared<'_>],
        methods: &[(usize, &GuardIr, String)],
        scopes: &[(usize, &GuardIr, String)],
        counts: &mut BTreeMap<String, usize>,
    ) -> (Order, HandlerRecord, Vec<ComparisonPlace>) {
        let line_of = |guard: &GuardIr| guard.literal_at.unwrap_or(guard.site).line;
        let mut ordered: Vec<&Compared<'_>> = comparisons.to_vec();
        ordered.sort_by_cached_key(|comparison| {
            (
                self.chains.rel(self.chains.nodes[comparison.node].file),
                line_of(comparison.guard),
                comparison.guard.site.span_start,
                comparison.literal.clone(),
            )
        });
        // Lines to show, per function.
        let mut marks: BTreeMap<usize, BTreeSet<u32>> = BTreeMap::new();
        for comparison in &ordered {
            marks
                .entry(comparison.node)
                .or_default()
                .insert(line_of(comparison.guard));
        }
        for (node, guard, _) in methods.iter().chain(scopes) {
            marks.entry(*node).or_default().insert(guard.site.line);
        }
        let mut entries: Vec<HandlerComparison> = Vec::new();
        let mut places: Vec<ComparisonPlace> = Vec::new();
        let mut forced: BTreeMap<String, ForcedRoute> = BTreeMap::new();
        let mut asked: Vec<u32> = Vec::new();
        for (id, comparison) in ordered.iter().enumerate() {
            let id = id as u32;
            let mut paths: Vec<Option<String>> = Vec::new();
            let mut verbs: Vec<Option<String>> = Vec::new();
            let mut read: Option<ComparisonRead> = None;
            let mut hole: Option<ComparisonHole> = None;
            for (handler, arm) in &comparison.arms {
                let (path, method) = self.forced(comparison, *handler, arm);
                paths.push(path);
                verbs.push(method);
                match arm {
                    Arm::Proven(reached, _) | Arm::Entry(reached, _) if read.is_none() => {
                        let params = self.params(*handler);
                        read = Some(ComparisonRead {
                            param: params
                                .get(reached.request.param)
                                .cloned()
                                .unwrap_or_else(|| reached.request.param.to_string()),
                            steps: reached.request.steps.clone(),
                        });
                    }
                    Arm::Hole { reason, text, .. } if hole.is_none() => {
                        hole = Some(ComparisonHole {
                            reason: reason.clone(),
                            text: text.clone(),
                        });
                    }
                    _ => {}
                }
            }
            // A value is forced only when every handler's chain forces it.
            let agreed = |values: Vec<Option<String>>| -> Option<String> {
                let first = values.first()?.clone()?;
                values
                    .iter()
                    .all(|value| value.as_deref() == Some(first.as_str()))
                    .then_some(first)
            };
            let path = agreed(paths);
            let method = agreed(verbs);
            if read.is_some() {
                hole = None;
            }
            if hole.is_some() {
                count(counts, "holes");
            }
            count(counts, "comparisons");
            // The method is the code's fact (ruling of 2026-10-23): a path
            // the code forces with no one method qualifying is asked
            // nothing, whatever methods the record states elsewhere.
            let is_asked = match (&path, &method) {
                (Some(_), Some(_)) => {
                    count(counts, "forced");
                    false
                }
                (Some(_), None) => {
                    count(counts, "no_method");
                    false
                }
                _ => true,
            };
            places.push(self.place(id, comparison));
            if is_asked {
                count(counts, "asked");
                asked.push(id);
            }
            if path.is_some() || method.is_some() {
                forced.insert(id.to_string(), ForcedRoute { path, method });
            }
            entries.push(HandlerComparison {
                id,
                at: self.at(comparison.node, line_of(comparison.guard)),
                literal: comparison.literal.clone(),
                position: match comparison.guard.position {
                    GuardPosition::Case => ComparisonPosition::Case,
                    GuardPosition::TableKey => ComparisonPosition::TableKey,
                    GuardPosition::Equality | GuardPosition::StartsWith => {
                        ComparisonPosition::Equality
                    }
                },
                expression: comparison.guard.subject_text.clone(),
                read,
                hole,
            });
        }
        let mut stated: Vec<((String, u32), MethodLiteral)> = methods
            .iter()
            .map(|(node, guard, literal)| {
                (
                    (
                        self.chains.rel(self.chains.nodes[*node].file),
                        guard.site.line,
                    ),
                    MethodLiteral {
                        literal: literal.clone(),
                        at: self.at(*node, guard.site.line),
                    },
                )
            })
            .collect();
        stated.sort_by(|(a, x), (b, y)| (a, &x.literal).cmp(&(b, &y.literal)));
        stated.dedup_by(|(_, later), (_, kept)| later == kept);
        let mut prefixes: Vec<((String, u32), ScopePrefix)> = scopes
            .iter()
            .map(|(node, guard, literal)| {
                let delegates = match &guard.branch {
                    GuardBranch::Span(lo, hi) => self
                        .chains
                        .branch_calls(*node, (*lo, *hi))
                        .into_iter()
                        .filter_map(|branch| match branch {
                            Branch::Call { callee, .. } => Some(callee),
                            Branch::Value { .. } => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                (
                    (
                        self.chains.rel(self.chains.nodes[*node].file),
                        guard.site.line,
                    ),
                    ScopePrefix {
                        prefix: literal.clone(),
                        at: self.at(*node, guard.site.line),
                        delegates,
                    },
                )
            })
            .collect();
        prefixes.sort_by(|(a, x), (b, y)| (a, &x.prefix).cmp(&(b, &y.prefix)));
        prefixes.dedup_by(|(_, later), (_, kept)| later == kept);
        // A function written inside the subject is shown in the subject's
        // code, not again in a window of its own.
        let mut subject_marks = marks.remove(&subject).unwrap_or_default();
        marks.retain(|node, lines| {
            let inside = self.written_inside(*node, subject);
            if inside {
                subject_marks.append(lines);
            }
            !inside
        });
        let mut windows: Vec<((String, u32), CodeWindow)> = marks
            .iter()
            .map(|(node, lines)| {
                let line = self.chains.line_of(*node);
                (
                    (self.chains.rel(self.chains.nodes[*node].file), line),
                    CodeWindow {
                        at: self.at(*node, line),
                        code: self.code(*node, lines),
                    },
                )
            })
            .collect();
        windows.sort_by(|(a, _), (b, _)| a.cmp(b));
        let handlers = (kind == SubjectKind::Shared).then(|| {
            let mut refs: Vec<((String, u32), HandlerRef)> = reached
                .iter()
                .map(|handler| {
                    let line = self.chains.line_of(*handler);
                    (
                        (self.chains.rel(self.chains.nodes[*handler].file), line),
                        HandlerRef {
                            at: self.at(*handler, line),
                            key: self.key(*handler),
                        },
                    )
                })
                .collect();
            refs.sort_by(|(a, x), (b, y)| (a, &x.key).cmp(&(b, &y.key)));
            refs.into_iter()
                .take(MAX_SHARED_HANDLERS)
                .map(|(_, handler)| handler)
                .collect::<Vec<_>>()
        });
        let line = self.chains.line_of(subject);
        let file = self.chains.rel(self.chains.nodes[subject].file);
        let order = (
            file,
            line,
            match kind {
                SubjectKind::Entry => 0,
                SubjectKind::Label => 1,
                SubjectKind::Shared => 2,
            },
            self.key(subject),
        );
        let record = HandlerRecord {
            subject: HandlerSubject {
                at: self.at(subject, line),
                kind,
                key: self.key(subject),
                params: self.params(subject),
                code: self.code(subject, &subject_marks),
                label,
                handler_count: handlers.as_ref().map(|_| reached.len() as u32),
                handlers,
            },
            code: windows.into_iter().map(|(_, window)| window).collect(),
            comparisons: entries,
            methods: stated.into_iter().map(|(_, method)| method).collect(),
            scopes: prefixes.into_iter().map(|(_, scope)| scope).collect(),
            asked,
            forced,
        };
        (order, record, places)
    }

    /// The lines of `node`'s function.
    fn range(&self, node: usize) -> LineRange {
        let (first, last) = self.lines(node, &self.text(node));
        LineRange {
            file: self.chains.rel(self.chains.nodes[node].file),
            first,
            last,
        }
    }

    /// Where comparison `id`'s route stands ([`ComparisonPlace`]).
    fn place(&self, id: u32, comparison: &Compared<'_>) -> ComparisonPlace {
        let guard = comparison.guard;
        let served = comparison.arms.iter().all(|(handler, arm)| {
            self.handlers[handler].serves_whole_path()
                && !matches!(arm, Arm::Hole { handed: true, .. })
        });
        let label = match comparison.arms.keys().collect::<Vec<_>>()[..] {
            [only] => match &self.handlers[only] {
                Handler::Label(label, _) => Some(label.clone()),
                Handler::Entry => None,
            },
            _ => None,
        };
        let mut within: Vec<LineRange> = comparison
            .arms
            .keys()
            .chain([&comparison.node])
            .map(|node| self.range(*node))
            .collect();
        within.sort();
        within.dedup();
        // A table key stands on its own span: two keys of one lookup are two
        // places, never one possible call.
        let at = guard.literal_at.unwrap_or(guard.site);
        ComparisonPlace {
            id,
            file: self.chains.rel(self.chains.nodes[comparison.node].file),
            line: at.line,
            span_start: at.span_start,
            span_end: at.span_end,
            served,
            label,
            within,
        }
    }

    /// Whether `node` is written inside `outer`, at any depth.
    fn written_inside(&self, node: usize, outer: usize) -> bool {
        let mut at = self.chains.nodes[node].parent;
        while let Some(current) = at {
            if current == outer {
                return true;
            }
            at = self.chains.nodes[current].parent;
        }
        false
    }

    fn key(&self, node: usize) -> String {
        self.chains.nodes[node]
            .key
            .clone()
            .unwrap_or_else(|| "(anonymous)".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A function under 120 lines is shown whole; a longer one as its first
    /// line and 4 lines before and 2 after each comparison, overlapping
    /// windows merged and the last cut at the function's end.
    #[test]
    fn a_long_function_is_shown_in_windows_around_its_comparisons() {
        let marks = BTreeSet::from([5, 50, 53, 199]);
        assert_eq!(windows(10, 129, &marks), vec![(10, 129)]);
        assert_eq!(windows(1, 200, &marks), vec![(1, 7), (46, 55), (195, 200)]);
        // A mark outside the function shows nothing more.
        assert_eq!(
            windows(100, 300, &BTreeSet::from([5, 150])),
            vec![(100, 100), (146, 152)]
        );
    }
}
