//! The call edge of a member call that goes through a declaration with no
//! body (an interface member, a function-typed property, an abstract
//! member), when TypeScript names exactly one implementation of it
//! (carrick#813, fix G).
//!
//! The compiler's `call` row names the declaration (`Store.read`), which is
//! no definition, so [`crate::call_graph::add_row_edges`] states no edge for
//! the call. The analysis sidecar's `implementations` action
//! ([`crate::services::type_sidecar::TypeSidecar::implementations`]) says
//! which bodies implement that declaration, over the program the service
//! already builds. One question per declaration, not per call.
//!
//! - **One implementation** that joins to a definition of the service gives
//!   a plain edge, the same one a class-method edge is: the compiler names
//!   the one body in the program.
//! - **Several, or none the service defines, or more than the sidecar's
//!   cap:** no edge. The call stays on the unfollowed rows with the reason
//!   ([`crate::unfollowed_calls`]): an edge to every candidate would put a
//!   request on a caller that never sends it.
//! - **Counting.** Entries in test sources are dropped, then every entry
//!   counts, whether or not it joins to a definition: a value TypeScript
//!   could not name (a destructured binding) beside one definition is two
//!   implementations, not one.
//! - **Bounded.** Declarations are asked in chunks of [`CHUNK`]; once the
//!   pass has spent its budget on a service, the calls not yet asked keep
//!   their `interface_member` row.
//!
//! carrick#2432 slice B reads the same answers next: it asks once per
//! declaration through this module, never a second time.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tracing::debug;

use crate::call_graph::EdgelessCall;
use crate::request_summary::{CallReach, RequestSummaryInputs};
use crate::services::type_sidecar::{ImplementationsAnswer, ImplementationsQuery, TypeSidecar};
use crate::unfollowed_calls::{
    IMPLEMENTATIONS_OVER_CAP, SEVERAL_IMPLEMENTATIONS, UnfollowedMemberCall,
};

/// What the pass may spend on one service.
pub(crate) const IMPLEMENTATIONS_BUDGET: Duration = Duration::from_secs(30);

/// Declarations asked in one request.
const CHUNK: usize = 64;

/// Whoever answers `implementations` questions: the analysis sidecar, or a
/// stub in the tests.
pub(crate) trait AskImplementations {
    fn ask(
        &self,
        queries: &[ImplementationsQuery],
        scan_root: &Path,
        files: &[PathBuf],
    ) -> Result<Vec<ImplementationsAnswer>, String>;

    /// Drop the programs the asks kept; called once after the last chunk.
    fn release(&self);
}

impl AskImplementations for TypeSidecar {
    fn ask(
        &self,
        queries: &[ImplementationsQuery],
        scan_root: &Path,
        files: &[PathBuf],
    ) -> Result<Vec<ImplementationsAnswer>, String> {
        self.implementations(queries, scan_root, files)
            .map_err(|e| e.to_string())
    }

    fn release(&self) {
        if let Err(e) = self.release_implementations() {
            debug!("implementations: the release failed: {e}");
        }
    }
}

/// What the program says of one declaration's implementations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Exactly one, and it is a definition of the service: its file as
    /// walked and key.
    One(PathBuf, String),
    /// Two or more.
    Several,
    /// More than the sidecar lists (its cap).
    OverCap,
    /// None, one that is no definition of the service, a reason the sidecar
    /// gave for searching nothing, or the ask failed.
    NoEdge,
    /// Not asked: the budget was spent first.
    Unasked,
}

/// The outcome for each call that was edgeless, by its span as the call
/// graph keys it.
#[derive(Debug, Default)]
pub(crate) struct ImplementationTargets {
    by_call: HashMap<(PathBuf, u32, u32), Outcome>,
    /// Declarations in the questions sent.
    asked: usize,
    /// Declarations that came back answered.
    answered: usize,
    /// Declarations in chunks whose ask erred or came back with a wrong
    /// count of answers: asked, never answered.
    failed: usize,
    /// Why the first such chunk failed.
    first_error: Option<String>,
    /// Declarations left unasked by the budget.
    unasked: usize,
    took_ms: u64,
    /// Answers by the sidecar's reason for searching nothing.
    reasons: BTreeMap<String, usize>,
    /// The first [`NAMED_PER_REASON`] questions for each such reason: the
    /// question's file and offset, then the declaration's file and line.
    named: BTreeMap<String, Vec<(String, u32, PathBuf, u32)>>,
}

/// Questions the run-log line names for each reason.
const NAMED_PER_REASON: usize = 10;

/// The 1-based line of a UTF-16 offset in `file`, or the offset itself when
/// the file cannot be read.
fn line_of(file: &str, offset: u32) -> String {
    let Ok(text) = std::fs::read_to_string(file) else {
        return format!("@{offset}");
    };
    let mut units = 0u32;
    let mut line = 1u32;
    for ch in text.chars() {
        if units >= offset {
            break;
        }
        units += u32::try_from(ch.len_utf16()).unwrap_or(1);
        if ch == '\n' {
            line += 1;
        }
    }
    line.to_string()
}

impl ImplementationTargets {
    /// The one implementation of the declaration the call at `lo..hi` goes
    /// through, as an edge target.
    pub(crate) fn target_at(&self, file: &Path, lo: u32, hi: u32) -> Option<(PathBuf, String)> {
        match self.outcome_at(file, lo, hi)? {
            Outcome::One(file, key) => Some((file.clone(), key.clone())),
            _ => None,
        }
    }

    pub(crate) fn outcome_at(&self, file: &Path, lo: u32, hi: u32) -> Option<&Outcome> {
        self.by_call.get(&(file.to_path_buf(), lo, hi))
    }

    /// The run-log line for one service.
    pub(crate) fn summary(&self) -> String {
        format!(
            "declarations asked: {}, answered: {}, failed: {}{}, left unasked: {}, \
             took {} ms, searched nothing by reason: {:?}{}",
            self.asked,
            self.answered,
            self.failed,
            self.first_error
                .as_ref()
                .map(|e| format!(" (first error: {e})"))
                .unwrap_or_default(),
            self.unasked,
            self.took_ms,
            self.reasons,
            self.named_questions()
        )
    }

    /// ` [reason: file:line -> declared_file:line, ...]` for each reason.
    fn named_questions(&self) -> String {
        self.named
            .iter()
            .map(|(reason, questions)| {
                let shown: Vec<String> = questions
                    .iter()
                    .map(|(file, start, declared, line)| {
                        format!(
                            "{file}:{} -> {}:{line}",
                            line_of(file, *start),
                            declared.display()
                        )
                    })
                    .collect();
                format!(" [{reason}: {}]", shown.join(", "))
            })
            .collect()
    }
}

/// What the answer to one question means for the calls through the
/// declaration. `path_of` names a row's repo-relative file as discovery
/// walks it; `definition_at` is the service's definition a declaration at
/// a file and line is.
pub(crate) fn classify(
    answer: &ImplementationsAnswer,
    scan_root: &Path,
    test_only: &HashSet<PathBuf>,
    path_of: impl Fn(&str) -> PathBuf,
    definition_at: impl Fn(&Path, Option<u32>, &str) -> CallReach,
) -> Outcome {
    if answer.reason.is_some() {
        return Outcome::NoEdge;
    }
    if answer.too_many {
        return Outcome::OverCap;
    }
    let kept: Vec<_> = answer
        .implementations
        .iter()
        .filter(|entry| {
            let Some(file) = entry.decl_file.as_deref() else {
                return true;
            };
            let path = path_of(file);
            !(crate::file_finder::is_test_source(&path, scan_root) || test_only.contains(&path))
        })
        .collect();
    match kept.as_slice() {
        [] => Outcome::NoEdge,
        [one] => match (one.decl_file.as_deref(), one.symbol.as_deref()) {
            (Some(file), Some(symbol)) => {
                match definition_at(&path_of(file), one.decl_line, symbol) {
                    CallReach::Function(file, key) => Outcome::One(file, key),
                    _ => Outcome::NoEdge,
                }
            }
            _ => Outcome::NoEdge,
        },
        _ => Outcome::Several,
    }
}

/// Ask the sidecar which bodies implement the declaration each of `calls`
/// goes through, one question per declaration, within `budget`.
pub(crate) fn ask(
    calls: &[EdgelessCall],
    inputs: &RequestSummaryInputs,
    asker: &dyn AskImplementations,
    budget: Duration,
) -> ImplementationTargets {
    let Some(positions) = inputs.positions.as_ref() else {
        return ImplementationTargets::default();
    };
    ask_with(
        calls,
        |call| {
            let (start, end) = inputs.call_row_span(&call.file, call.lo, call.hi)?;
            Some(ImplementationsQuery {
                file: call.file.to_string_lossy().into_owned(),
                start,
                end,
            })
        },
        asker,
        positions.scan_root(),
        &positions.files(),
        budget,
        |answer| {
            classify(
                answer,
                positions.scan_root(),
                &inputs.test_only,
                |file| positions.path_of(file),
                |file, line, symbol| inputs.definition_at(file, line, symbol),
            )
        },
    )
}

/// [`ask`] with the placing of a call's question and the reading of an
/// answer given.
fn ask_with(
    calls: &[EdgelessCall],
    place: impl Fn(&EdgelessCall) -> Option<ImplementationsQuery>,
    asker: &dyn AskImplementations,
    scan_root: &Path,
    files: &[PathBuf],
    budget: Duration,
    read: impl Fn(&ImplementationsAnswer) -> Outcome,
) -> ImplementationTargets {
    let started = Instant::now();
    let mut targets = ImplementationTargets::default();

    // The calls through each declaration, in file and span order.
    let mut by_declaration: BTreeMap<(&Path, u32), Vec<&EdgelessCall>> = BTreeMap::new();
    for call in calls {
        by_declaration
            .entry((call.declared.0.as_path(), call.declared.1))
            .or_default()
            .push(call);
    }
    // One question per declaration, put by the first call whose row the
    // index can place.
    let mut questions: Vec<(Vec<&EdgelessCall>, ImplementationsQuery)> = Vec::new();
    for (_, mut group) in by_declaration {
        group.sort_by(|a, b| (&a.file, a.lo, a.hi).cmp(&(&b.file, b.lo, b.hi)));
        match group.iter().find_map(|call| place(call)) {
            Some(query) => questions.push((group, query)),
            None => set(&mut targets, &group, &Outcome::NoEdge),
        }
    }

    for chunk in questions.chunks(CHUNK) {
        if started.elapsed() >= budget {
            targets.unasked += chunk.len();
            for (group, _) in chunk {
                set(&mut targets, group, &Outcome::Unasked);
            }
            continue;
        }
        targets.asked += chunk.len();
        let queries: Vec<ImplementationsQuery> = chunk.iter().map(|(_, q)| q.clone()).collect();
        let answers = match asker.ask(&queries, scan_root, files) {
            Ok(answers) if answers.len() == chunk.len() => Some(answers),
            Ok(answers) => {
                let why = format!("{} answers to {} questions", answers.len(), chunk.len());
                debug!("implementations: {why}, none read");
                targets.first_error.get_or_insert(why);
                None
            }
            Err(e) => {
                debug!("implementations: the ask failed: {e}");
                targets.first_error.get_or_insert(e);
                None
            }
        };
        let Some(answers) = answers else {
            targets.failed += chunk.len();
            for (group, _) in chunk {
                set(&mut targets, group, &Outcome::NoEdge);
            }
            continue;
        };
        targets.answered += chunk.len();
        for ((group, query), answer) in chunk.iter().zip(&answers) {
            if let Some(reason) = &answer.reason {
                *targets.reasons.entry(reason.clone()).or_default() += 1;
                let named = targets.named.entry(reason.clone()).or_default();
                if named.len() < NAMED_PER_REASON {
                    if let Some(first) = group.first() {
                        named.push((
                            query.file.clone(),
                            query.start,
                            first.declared.0.clone(),
                            first.declared.1,
                        ));
                    }
                }
            }
            set(&mut targets, group, &read(answer));
        }
    }
    // However the pass ended, the programs the asks kept are dropped once.
    if !questions.is_empty() {
        asker.release();
    }
    targets.took_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    targets
}

fn set(targets: &mut ImplementationTargets, group: &[&EdgelessCall], outcome: &Outcome) {
    for call in group {
        targets
            .by_call
            .insert((call.file.clone(), call.lo, call.hi), outcome.clone());
    }
}

/// The unfollowed rows once the implementations are known: a call that took
/// an edge writes no row, and a call through a declaration with several
/// implementations, or more than the sidecar lists, says so. Any other row
/// stays as it was. `calls` name their rows by index into `rows`.
pub(crate) fn settle(
    rows: Vec<UnfollowedMemberCall>,
    calls: &[EdgelessCall],
    targets: &ImplementationTargets,
) -> Vec<UnfollowedMemberCall> {
    let mut gone: HashSet<usize> = HashSet::new();
    let mut reasons: HashMap<usize, &'static str> = HashMap::new();
    for call in calls {
        // A call with no row has nothing to settle.
        let Some(row) = call.row else { continue };
        match targets.outcome_at(&call.file, call.lo, call.hi) {
            Some(Outcome::One(..)) => {
                gone.insert(row);
            }
            Some(Outcome::Several) => {
                reasons.insert(row, SEVERAL_IMPLEMENTATIONS);
            }
            Some(Outcome::OverCap) => {
                reasons.insert(row, IMPLEMENTATIONS_OVER_CAP);
            }
            _ => {}
        }
    }
    rows.into_iter()
        .enumerate()
        .filter(|(index, _)| !gone.contains(index))
        .map(|(index, mut row)| {
            if let Some(reason) = reasons.get(&index) {
                row.reason = (*reason).to_string();
            }
            row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::type_sidecar::NamedDeclaration;
    use std::cell::RefCell;

    const ROOT: &str = "/repo";

    fn entry(file: &str, line: u32, symbol: &str) -> NamedDeclaration {
        NamedDeclaration {
            symbol: Some(symbol.to_string()),
            decl_file: Some(file.to_string()),
            decl_line: Some(line),
            decl_package: None,
        }
    }

    fn answer(implementations: Vec<NamedDeclaration>) -> ImplementationsAnswer {
        ImplementationsAnswer {
            declaration: None,
            implementations,
            too_many: false,
            reason: None,
        }
    }

    /// The service defines `read` in `src/a.ts` and `src/b.ts` at line 1,
    /// and `src/__tests__/t.ts` holds a test double.
    fn read(answer: &ImplementationsAnswer) -> Outcome {
        classify(
            answer,
            Path::new(ROOT),
            &HashSet::new(),
            |file| Path::new(ROOT).join(file),
            |file, line, symbol| {
                let defined = file.ends_with("src/a.ts") || file.ends_with("src/b.ts");
                if defined && line == Some(1) {
                    CallReach::Function(file.to_path_buf(), symbol.to_string())
                } else {
                    CallReach::NoFunction
                }
            },
        )
    }

    #[test]
    fn one_definition_is_the_implementation() {
        assert_eq!(
            read(&answer(vec![entry("src/a.ts", 1, "read")])),
            Outcome::One(PathBuf::from("/repo/src/a.ts"), "read".to_string())
        );
    }

    #[test]
    fn a_test_double_beside_one_definition_is_not_counted() {
        let outcome = read(&answer(vec![
            entry("src/__tests__/t.ts", 1, "read"),
            entry("src/a.ts", 1, "read"),
        ]));
        assert!(matches!(outcome, Outcome::One(..)), "{outcome:?}");
    }

    #[test]
    fn two_definitions_are_several() {
        let outcome = read(&answer(vec![
            entry("src/a.ts", 1, "read"),
            entry("src/b.ts", 1, "read"),
        ]));
        assert_eq!(outcome, Outcome::Several);
    }

    #[test]
    fn a_binding_beside_a_definition_is_several() {
        // Line 9 of `src/a.ts` is a destructured binding: no definition, but
        // the program names it, so the call may reach it.
        let outcome = read(&answer(vec![
            entry("src/a.ts", 9, "read"),
            entry("src/b.ts", 1, "read"),
        ]));
        assert_eq!(outcome, Outcome::Several);
    }

    #[test]
    fn more_than_the_cap_is_over_the_cap() {
        let mut over = answer(vec![]);
        over.too_many = true;
        assert_eq!(read(&over), Outcome::OverCap);
    }

    #[test]
    fn a_reason_for_searching_nothing_is_no_edge() {
        let mut none = answer(vec![entry("src/a.ts", 1, "read")]);
        none.reason = Some("has_body".to_string());
        assert_eq!(read(&none), Outcome::NoEdge);
    }

    #[test]
    fn one_entry_that_is_no_definition_of_the_service_is_no_edge() {
        assert_eq!(
            read(&answer(vec![entry("src/c.ts", 1, "read")])),
            Outcome::NoEdge
        );
        assert_eq!(read(&answer(vec![])), Outcome::NoEdge);
    }

    fn call(file: &str, lo: u32, row: usize, declared: (&str, u32)) -> EdgelessCall {
        EdgelessCall {
            file: PathBuf::from(file),
            lo,
            hi: lo + 5,
            row: Some(row),
            declared: (PathBuf::from(declared.0), declared.1),
        }
    }

    fn place(call: &EdgelessCall) -> Option<ImplementationsQuery> {
        Some(ImplementationsQuery {
            file: call.file.to_string_lossy().into_owned(),
            start: call.lo,
            end: call.hi,
        })
    }

    /// Answers one definition for every question and keeps what it was asked.
    struct Stub {
        asked: RefCell<Vec<Vec<ImplementationsQuery>>>,
        files: RefCell<Vec<Vec<PathBuf>>>,
        released: std::cell::Cell<usize>,
        fail_first: bool,
    }

    impl AskImplementations for Stub {
        fn ask(
            &self,
            queries: &[ImplementationsQuery],
            _: &Path,
            files: &[PathBuf],
        ) -> Result<Vec<ImplementationsAnswer>, String> {
            self.files.borrow_mut().push(files.to_vec());
            let first = self.asked.borrow().is_empty();
            self.asked.borrow_mut().push(queries.to_vec());
            if self.fail_first && first {
                return Err("the sidecar died".to_string());
            }
            Ok(queries
                .iter()
                .map(|_| answer(vec![entry("src/a.ts", 1, "read")]))
                .collect())
        }

        fn release(&self) {
            self.released.set(self.released.get() + 1);
        }
    }

    #[test]
    fn three_calls_through_one_declaration_ask_one_question() {
        let stub = Stub {
            asked: RefCell::new(Vec::new()),
            files: RefCell::new(Vec::new()),
            released: std::cell::Cell::new(0),
            fail_first: false,
        };
        let calls = vec![
            call("/repo/src/z.ts", 30, 2, ("/repo/src/p.ts", 4)),
            call("/repo/src/y.ts", 20, 1, ("/repo/src/p.ts", 4)),
            call("/repo/src/y.ts", 10, 0, ("/repo/src/p.ts", 4)),
        ];
        let targets = ask_with(
            &calls,
            place,
            &stub,
            Path::new(ROOT),
            &[],
            Duration::from_secs(60),
            read,
        );
        let asked = stub.asked.borrow();
        assert_eq!(asked.len(), 1);
        // Put by the first call in file and span order.
        assert_eq!(asked[0].len(), 1);
        assert_eq!(
            (asked[0][0].file.as_str(), asked[0][0].start),
            ("/repo/src/y.ts", 10)
        );
        for c in &calls {
            assert!(targets.target_at(&c.file, c.lo, c.hi).is_some());
        }
    }

    #[test]
    fn a_failed_chunk_is_no_edge_and_the_scan_goes_on() {
        let stub = Stub {
            asked: RefCell::new(Vec::new()),
            files: RefCell::new(Vec::new()),
            released: std::cell::Cell::new(0),
            fail_first: true,
        };
        // 65 declarations: two chunks, the first fails.
        let calls: Vec<_> = (0..65)
            .map(|i| {
                call(
                    "/repo/src/y.ts",
                    i * 10,
                    i as usize,
                    ("/repo/src/p.ts", i + 1),
                )
            })
            .collect();
        let targets = ask_with(
            &calls,
            place,
            &stub,
            Path::new(ROOT),
            &[],
            Duration::from_secs(60),
            read,
        );
        assert_eq!(stub.asked.borrow().len(), 2);
        assert_eq!(
            targets.outcome_at(Path::new("/repo/src/y.ts"), 0, 5),
            Some(&Outcome::NoEdge)
        );
        assert!(
            targets
                .target_at(Path::new("/repo/src/y.ts"), 640, 645)
                .is_some()
        );
        assert_eq!(targets.answered, 1);
        assert_eq!(targets.failed, 64);
        let line = targets.summary();
        assert!(
            line.contains("failed: 64 (first error: the sidecar died)"),
            "{line}"
        );
    }

    /// The debug line names each question that searched nothing: the call's
    /// file and line, then the declaration's, at most ten per reason.
    #[test]
    fn the_summary_names_the_questions_that_searched_nothing() {
        struct Nothing;
        impl AskImplementations for Nothing {
            fn ask(
                &self,
                queries: &[ImplementationsQuery],
                _: &Path,
                _: &[PathBuf],
            ) -> Result<Vec<ImplementationsAnswer>, String> {
                Ok(queries
                    .iter()
                    .map(|_| ImplementationsAnswer {
                        declaration: None,
                        implementations: vec![],
                        too_many: false,
                        reason: Some("not_in_program".to_string()),
                    })
                    .collect())
            }
            fn release(&self) {}
        }
        let dir = std::env::temp_dir().join(format!("impl-summary-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("script.mjs");
        std::fs::write(&file, "a\nb\nread();\n").unwrap();
        // 12 declarations, each asked from the call on line 3 (offset 4).
        let calls: Vec<_> = (0..12)
            .map(|i| EdgelessCall {
                file: file.clone(),
                lo: 4 + i * 100,
                hi: 9 + i * 100,
                row: Some(i as usize),
                declared: (PathBuf::from("/repo/src/p.ts"), i + 1),
            })
            .collect();
        let stub = Nothing;
        let targets = ask_with(
            &calls,
            |call| {
                Some(ImplementationsQuery {
                    file: call.file.to_string_lossy().into_owned(),
                    start: 4,
                    end: 9,
                })
            },
            &stub,
            Path::new(ROOT),
            &[],
            Duration::from_secs(60),
            read,
        );
        let line = targets.summary();
        let shown = format!("{}:3 -> /repo/src/p.ts:1", file.display());
        assert!(line.contains(&shown), "{line}");
        assert!(line.contains("/repo/src/p.ts:10"), "{line}");
        assert!(!line.contains("/repo/src/p.ts:11"), "{line}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_spent_budget_asks_nothing_and_leaves_every_call_unasked() {
        let stub = Stub {
            asked: RefCell::new(Vec::new()),
            files: RefCell::new(Vec::new()),
            released: std::cell::Cell::new(0),
            fail_first: false,
        };
        let calls = vec![call("/repo/src/y.ts", 10, 0, ("/repo/src/p.ts", 4))];
        let targets = ask_with(
            &calls,
            place,
            &stub,
            Path::new(ROOT),
            &[],
            Duration::ZERO,
            read,
        );
        assert!(stub.asked.borrow().is_empty());
        assert_eq!(
            targets.outcome_at(Path::new("/repo/src/y.ts"), 10, 15),
            Some(&Outcome::Unasked)
        );
        assert_eq!(targets.unasked, 1);
    }

    /// carrick#3220: the programs the asks kept are released once, however
    /// the pass ends, and every chunk carries the files.
    #[test]
    fn the_kept_programs_are_released_once_on_each_ending() {
        let files = vec![PathBuf::from("/repo/src/y.ts")];
        // 65 declarations are two chunks; one is a single chunk.
        for (declarations, fail_first, budget, chunks) in [
            (65, false, Duration::from_secs(60), 2),
            (1, true, Duration::from_secs(60), 1),
            (1, false, Duration::ZERO, 0),
        ] {
            let stub = Stub {
                asked: RefCell::new(Vec::new()),
                files: RefCell::new(Vec::new()),
                released: std::cell::Cell::new(0),
                fail_first,
            };
            let calls: Vec<_> = (0..declarations)
                .map(|i| {
                    call(
                        "/repo/src/y.ts",
                        i * 10,
                        i as usize,
                        ("/repo/src/p.ts", i + 1),
                    )
                })
                .collect();
            ask_with(&calls, place, &stub, Path::new(ROOT), &files, budget, read);
            assert_eq!(
                stub.released.get(),
                1,
                "{declarations} {fail_first} {budget:?}"
            );
            assert_eq!(stub.asked.borrow().len(), chunks);
            assert!(stub.files.borrow().iter().all(|f| f == &files));
        }
    }

    fn row(reason: &str) -> UnfollowedMemberCall {
        UnfollowedMemberCall {
            file: "src/c.ts".to_string(),
            line: 1,
            caller: "f".to_string(),
            member: "m".to_string(),
            receiver: "r".to_string(),
            reason: reason.to_string(),
            declared_at: Some("src/p.ts:4".to_string()),
        }
    }

    #[test]
    fn settling_drops_the_row_of_an_edge_and_names_the_reason_of_a_several() {
        let mut targets = ImplementationTargets::default();
        let one = call("/repo/src/c.ts", 1, 0, ("/repo/src/p.ts", 4));
        let several = call("/repo/src/c.ts", 2, 1, ("/repo/src/p.ts", 5));
        let over = call("/repo/src/c.ts", 3, 2, ("/repo/src/p.ts", 6));
        set(
            &mut targets,
            &[&one],
            &Outcome::One(PathBuf::from("/repo/src/a.ts"), "read".to_string()),
        );
        set(&mut targets, &[&several], &Outcome::Several);
        set(&mut targets, &[&over], &Outcome::OverCap);
        let rows = vec![row("interface_member"); 4];
        let settled = settle(rows, &[one, several, over], &targets);
        let reasons: Vec<_> = settled.iter().map(|r| r.reason.as_str()).collect();
        assert_eq!(
            reasons,
            [
                "several_implementations",
                "implementations_over_cap",
                "interface_member"
            ]
        );
    }
}
