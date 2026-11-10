//! What a conditional leaves a request's URL or method able to read as
//! (carrick#2050, carrick#2051).
//!
//! ```ignore
//! const segment = kind === "draft" ? `drafts/${id}` : `posts/${id}`;
//! await fetch(`/api/content/${segment}/publish`, { method: "POST" });
//!
//! const method = existing ? "PATCH" : "POST";
//! const url = existing ? `/api/notes/${existing.id}` : "/api/notes";
//! await fetch(url, { method });
//! ```
//!
//! A value read through a conditional is not one value. [`Reader::eval`]
//! keeps the expression's text as an opaque value, which a URL then reads as
//! one path parameter: here that is a path one segment shorter than either
//! route the branches write, and a method taken from one branch beside a URL
//! taken from the other.
//!
//! This reads the branches themselves. A value is a set of readings
//! ([`Alternatives`]), each holding the branch every test took in it, and a
//! request built from one states one row per reading:
//!
//! - **A conditional whose branches can both be read is two readings.**
//!   Templates and `+` carry them through, and so does a local the function
//!   declares once. A test written twice is one choice: its branches are
//!   taken together. A test and its negation (`!x` beside `x`, `a !== b`
//!   beside `a === b`) are one choice with the branches swapped.
//! - **Two different tests on one name may not be independent.** `kind ===
//!   "a"` and `kind === "b"` in two parts of one URL cannot both hold, so
//!   their cross product holds readings the source cannot send, and no row is
//!   stated. Tests on names unrelated to each other are independent and
//!   combine freely, and a test written inside a branch of another only
//!   refines that branch (`mode === "a" ? A : mode === "b" ? B : C`).
//! - **One test is one choice** (carrick#2051). A method and a URL chosen by
//!   the same test (the same stable expression: a name, a member of one, a
//!   comparison or negation of those) pair branch with branch:
//!   `!existing ? "POST" : "PATCH"` beside `existing ? … : …` pairs `POST`
//!   with the URL's alternate. A test that calls a function, or reads a name
//!   the function assigns again, is not the same test twice.
//! - **Tests that differ pair nothing.** When both the method and the URL
//!   choose and no test is shared, or two different tests on one name are
//!   split between them, the source does not say which method goes with
//!   which URL, and no row is stated.
//! - **A value that may hold a `/` is no path parameter.** Where one branch
//!   writes a path and the other is a value nothing here read, the slot may
//!   span segments, so the request states no row rather than a placeholder
//!   one. A conditional that leads the URL is a base, which the row was
//!   always allowed to read as opaque text, and is read as before when no
//!   branch writes a path; one whose branches write different paths states
//!   no row (carrick#2541).
//! - **A reading that differs from another only in a query is the same
//!   route.**
//!
//! Everything else is read exactly as before: this module only answers where
//! a conditional is reachable from the URL or the method.

use std::collections::{BTreeMap, BTreeSet};

use swc_common::{Span, Spanned};
use swc_ecma_ast::*;

use super::guards::{HoleReason, Root};
use super::resolve::PlacedWrites;
use super::{MethodValue, Piece, Reader, Scope, Value, concat, ident_key, render_target};
use crate::binding_scope::BindingKey;
use crate::type_manifest::is_http_method;

/// How many readings a value may have before it is left as the opaque text it
/// always was.
const MAX_READINGS: usize = 8;

/// The branch each test took.
type Taken = BTreeMap<String, usize>;

/// One reading of a value: the branches taken to get it, and the text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reading {
    taken: Taken,
    pieces: Vec<Piece>,
    /// The tests of the function's parameters this reading holds under, one
    /// per `if` around a write of a local it reads ([`WayGuard`]): a caller
    /// that fills the parameter with text decides each.
    guards: BTreeSet<WayGuard>,
    /// The reading is of a local no write has set on this way: `q || path`
    /// reads `path` there.
    unwritten: bool,
}

impl Reading {
    fn of(pieces: Vec<Piece>) -> Self {
        Reading {
            taken: Taken::new(),
            pieces,
            guards: BTreeSet::new(),
            unwritten: false,
        }
    }
}

/// What a test of a parameter compares it with (carrick#3679, the ruling R3
/// forms): `===` / `!==` against text, or a prefix (`startsWith`, an
/// anchored literal pattern), as [`super::guards`] reads the test.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum WayTest {
    Equals(String),
    StartsWith(String),
}

/// A test one way of a request holds under (carrick#3679): the request is
/// sent this way only where `subject`, a parameter piece the callers fill,
/// passes `test` (`holds`) or fails it (`!holds`). Decided at link time, at
/// the call that fills the parameter with text ([`Self::verdict`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct WayGuard {
    pub(super) subject: Vec<Piece>,
    pub(super) test: WayTest,
    pub(super) holds: bool,
}

/// What a filled [`WayGuard`] says about its way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Verdict {
    /// The test goes the way's way: the guard is spent.
    Kept,
    /// The test goes the other way: the way is never sent.
    RuledOut,
    /// A caller further up fills the subject.
    Open,
    /// The subject is filled with a value whose text nothing states: the way
    /// stays, the guard says nothing more.
    Undecided,
}

impl WayGuard {
    /// Whether a caller still fills the subject.
    pub(super) fn is_open(&self) -> bool {
        self.subject.iter().any(Piece::is_parameter)
    }

    /// The guard against what its subject now holds. The text up to the
    /// first piece that is not literal decides a prefix test once it is as
    /// long as the prefix, and an equality once it differs from the text.
    pub(super) fn verdict(&self) -> Verdict {
        if self.is_open() {
            return Verdict::Open;
        }
        let mut lead = String::new();
        let mut whole = true;
        for piece in &self.subject {
            match piece {
                Piece::Lit(text) => lead.push_str(text),
                _ => {
                    whole = false;
                    break;
                }
            }
        }
        let passes = match &self.test {
            WayTest::StartsWith(prefix) if lead.starts_with(prefix.as_str()) => true,
            WayTest::StartsWith(prefix) if whole || !prefix.starts_with(&lead) => false,
            WayTest::Equals(text) if whole => lead == *text,
            WayTest::Equals(text) if !text.starts_with(&lead) => false,
            _ => return Verdict::Undecided,
        };
        if passes == self.holds {
            Verdict::Kept
        } else {
            Verdict::RuledOut
        }
    }

    /// The same test, taken the other way.
    fn contradicts(&self, other: &WayGuard) -> bool {
        self.subject == other.subject && self.test == other.test && self.holds != other.holds
    }
}

/// Every guard of two readings, when none is taken both ways.
fn guards_together(a: &BTreeSet<WayGuard>, b: &BTreeSet<WayGuard>) -> Option<BTreeSet<WayGuard>> {
    if a.iter()
        .any(|ours| b.iter().any(|theirs| ours.contradicts(theirs)))
    {
        return None;
    }
    Some(a.union(b).cloned().collect())
}

/// The writes of each local a set of readings rests on, where they start
/// (discovery numbering): the module's write census must place exactly
/// these, or the readings are not every value the local may hold
/// ([`super::resolve::PlacedWrites`]).
pub(super) type Writes = BTreeMap<BindingKey, BTreeSet<u32>>;

fn merged_writes(a: &Writes, b: &Writes) -> Writes {
    let mut writes = a.clone();
    for (local, placed) in b {
        writes
            .entry(local.clone())
            .or_default()
            .extend(placed.iter().copied());
    }
    writes
}

/// What a value reads as, one way per combination of branches that can be
/// taken together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Alternatives {
    readings: Vec<Reading>,
    /// The conditional is where the value starts: a base, when the value is
    /// a URL.
    leading: bool,
    /// One branch writes a path and another is a value nothing read, so the
    /// slot may span segments.
    poisoned: bool,
    /// The names each stable test reads, by the test's key.
    tests: Tests,
    /// Two different tests in it read one name.
    entangled: bool,
    /// The writes of the locals written more than once that it reads
    /// (carrick#3679): non-empty only for a value read through one.
    writes: Writes,
}

/// The names each stable test reads, by the test's key.
type Tests = BTreeMap<String, BTreeSet<String>>;

/// Whether two names may hold the same value: equal, or one a member of the
/// other.
fn related(a: &str, b: &str) -> bool {
    let nested = |outer: &str, inner: &str| {
        inner
            .strip_prefix(outer)
            .is_some_and(|rest| rest.starts_with('.') || rest.starts_with("?."))
    };
    a == b || nested(a, b) || nested(b, a)
}

/// Every test two values read, with the names each one reads.
fn tests_of(a: &Tests, b: &Tests) -> Tests {
    let mut merged = a.clone();
    for (key, names) in b {
        merged
            .entry(key.clone())
            .or_default()
            .extend(names.iter().cloned());
    }
    merged
}

/// Whether two readings, taken together, each rest on a test the other does
/// not, and the two read one name. Such tests are not independent
/// (`kind === "a"` beside `kind === "b"`), so the pair may be one the source
/// cannot send. A test inside another's branch never makes such a pair: the
/// readings of an else-if chain on one name each hold the outer test.
fn entangled(a: &Taken, b: &Taken, tests: &Tests) -> bool {
    let names = |key: &String| tests.get(key).into_iter().flatten();
    a.keys().filter(|key| !b.contains_key(*key)).any(|ours| {
        b.keys()
            .filter(|key| !a.contains_key(*key))
            .any(|theirs| names(ours).any(|name| names(theirs).any(|other| related(name, other))))
    })
}

/// The branches two readings take, when no test is taken both ways.
fn together(a: &Taken, b: &Taken) -> Option<Taken> {
    let mut merged = a.clone();
    for (test, branch) in b {
        match merged.get(test) {
            Some(prior) if prior != branch => return None,
            _ => {
                merged.insert(test.clone(), *branch);
            }
        }
    }
    Some(merged)
}

impl Alternatives {
    fn single(pieces: Vec<Piece>) -> Self {
        Self {
            readings: vec![Reading::of(pieces)],
            leading: false,
            poisoned: false,
            tests: Tests::new(),
            entangled: false,
            writes: Writes::new(),
        }
    }

    /// A local written more than once, declared with no value (carrick#3679):
    /// one reading, unwritten, resting on the local's writes.
    pub(super) fn unwritten(local: BindingKey) -> Self {
        let mut readings = Self::single(Vec::new());
        readings.readings[0].unwritten = true;
        readings.writes.insert(local, BTreeSet::new());
        readings
    }

    /// The readings as those of the local `local`, whose writes so far start
    /// at `placed`.
    pub(super) fn of_local(mut self, local: &BindingKey, placed: &BTreeSet<u32>) -> Self {
        self.writes
            .entry(local.clone())
            .or_default()
            .extend(placed.iter().copied());
        self
    }

    /// Each reading where the test `guard` goes the way `holds` says; every
    /// reading as it is where the test is not one a caller decides.
    pub(super) fn under(&self, guard: Option<&WayGuard>, holds: bool) -> Self {
        let mut out = self.clone();
        let Some(guard) = guard else {
            return out;
        };
        let guard = WayGuard {
            holds: guard.holds == holds,
            ..guard.clone()
        };
        out.readings.retain_mut(|reading| {
            if reading.guards.iter().any(|held| held.contradicts(&guard)) {
                return false;
            }
            reading.guards.insert(guard.clone());
            true
        });
        out
    }

    /// The readings of either branch of an `if` (carrick#3679). `None` past
    /// [`MAX_READINGS`], or where no reading is left.
    pub(super) fn either(&self, other: &Alternatives) -> Option<Alternatives> {
        let mut readings = self.readings.clone();
        for reading in &other.readings {
            if !readings.contains(reading) {
                readings.push(reading.clone());
            }
        }
        if readings.is_empty() || readings.len() > MAX_READINGS {
            return None;
        }
        Some(Alternatives {
            readings,
            leading: self.leading || other.leading,
            poisoned: self.poisoned || other.poisoned,
            tests: tests_of(&self.tests, &other.tests),
            entangled: self.entangled || other.entangled,
            writes: merged_writes(&self.writes, &other.writes),
        })
    }

    /// `self || right` and `self ?? right`, where `self` reads a local
    /// written more than once: a reading no write set is `right`'s, and any
    /// other is its own (text a write sets is never empty text here).
    fn or_else(&self, right: &Alternatives) -> Option<Alternatives> {
        let mut readings = Vec::new();
        for left in &self.readings {
            if !left.unwritten {
                readings.push(left.clone());
                continue;
            }
            for taken in &right.readings {
                let (Some(branches), Some(guards)) = (
                    together(&left.taken, &taken.taken),
                    guards_together(&left.guards, &taken.guards),
                ) else {
                    continue;
                };
                readings.push(Reading {
                    taken: branches,
                    pieces: taken.pieces.clone(),
                    guards,
                    unwritten: taken.unwritten,
                });
            }
        }
        let mut joined = self.either(right)?;
        if readings.is_empty() || readings.len() > MAX_READINGS {
            return None;
        }
        joined.readings = readings;
        Some(joined)
    }

    /// Whether it reads a local written more than once (carrick#3679).
    pub(super) fn is_written(&self) -> bool {
        !self.writes.is_empty()
    }

    /// Each reading's text, where a conditional is in it and the source
    /// states which ways can be taken together: the ways a receiver trace
    /// slot reads (carrick#2386). Empty otherwise. A reading that holds a
    /// value nothing read (`poisoned`) is still returned: a reader of these
    /// ways takes only the readings written whole. Not the ways of a local
    /// written more than once: only the send side reads those.
    pub(super) fn chosen_texts(&self) -> Vec<Vec<Piece>> {
        if !self.chooses() || self.entangled || self.is_written() {
            return Vec::new();
        }
        self.readings
            .iter()
            .map(|reading| reading.pieces.clone())
            .collect()
    }

    /// Whether a conditional is in it, or a local written more than once.
    fn chooses(&self) -> bool {
        self.readings.len() > 1 || self.poisoned || self.is_written()
    }

    fn writes_nothing(&self) -> bool {
        self.readings
            .iter()
            .all(|reading| reading.pieces.is_empty())
    }

    /// Whether some reading writes a `/`.
    fn writes_a_path(&self) -> bool {
        self.readings.iter().any(|reading| {
            reading
                .pieces
                .iter()
                .any(|piece| matches!(piece, Piece::Lit(text) if text.contains('/')))
        })
    }

    /// Whether some reading is a value nothing here read: no text of its
    /// own, and a piece the source names, a caller supplies or nothing says.
    fn holds_an_unread_value(&self) -> bool {
        self.readings.iter().any(|reading| {
            !reading.pieces.is_empty()
                && !reading
                    .pieces
                    .iter()
                    .any(|piece| matches!(piece, Piece::Lit(_)))
        })
    }

    /// This, then `next`, in every combination that can be taken together.
    fn then(&self, next: &Alternatives) -> Option<Alternatives> {
        let tests = tests_of(&self.tests, &next.tests);
        let mut tangled = self.entangled || next.entangled;
        let mut readings = Vec::new();
        for a in &self.readings {
            for b in &next.readings {
                if let (Some(taken), Some(guards)) = (
                    together(&a.taken, &b.taken),
                    guards_together(&a.guards, &b.guards),
                ) {
                    tangled |= entangled(&a.taken, &b.taken, &tests);
                    // A local no write set, joined to text, is text no
                    // route is written as.
                    let pieces = if a.unwritten || b.unwritten {
                        vec![Piece::Unknown]
                    } else {
                        concat([a.pieces.clone(), b.pieces.clone()])
                    };
                    readings.push(Reading {
                        taken,
                        pieces,
                        guards,
                        unwritten: false,
                    });
                }
            }
        }
        if readings.is_empty() || readings.len() > MAX_READINGS {
            return None;
        }
        Some(Alternatives {
            readings,
            leading: self.leading || (self.writes_nothing() && next.leading),
            poisoned: self.poisoned || next.poisoned,
            tests,
            entangled: tangled,
            writes: merged_writes(&self.writes, &next.writes),
        })
    }

    /// Each reading as a method, when every one is a literal verb.
    fn as_methods(&self) -> Option<Vec<(Taken, MethodValue)>> {
        self.readings
            .iter()
            .map(|reading| match reading.pieces.as_slice() {
                [Piece::Lit(verb)] if is_http_method(verb) => Some((
                    reading.taken.clone(),
                    MethodValue::Lit(verb.trim().to_uppercase()),
                )),
                _ => None,
            })
            .collect()
    }
}

/// How a request reads once its conditionals are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Alternation {
    /// No conditional is in the URL or the method: the request reads as it
    /// always did.
    Single,
    /// One request per way the conditionals can go, each with the tests of
    /// parameters it holds under, and the writes of the locals written more
    /// than once the URL reads (carrick#3679): the module's write census
    /// keeps the rows only where it places exactly these
    /// ([`Self::census_checked`]).
    Rows(Vec<(MethodValue, Vec<Piece>, Vec<WayGuard>)>, Writes),
    /// The source does not say which request goes out: none is stated.
    Nothing,
}

impl Alternation {
    /// The rows, where the module's write census places exactly the writes
    /// of each local written more than once that they read (carrick#3679):
    /// one written anywhere else (inside a function written in the body, in
    /// a loop, as a compound or destructuring target) may hold anything when
    /// the request goes out, so the request reads as it did before any write
    /// was read, a hole.
    pub(super) fn census_checked(&mut self, placed: &PlacedWrites) {
        let Alternation::Rows(_, writes) = self else {
            return;
        };
        let unplaced = writes.iter().any(|(local, read)| {
            placed
                .of(local)
                .is_none_or(|census| census.iter().copied().collect::<BTreeSet<_>>() != *read)
        });
        if unplaced {
            *self = Alternation::Single;
        }
    }
}

impl Reader<'_> {
    /// What `expr` reads as, when a conditional is reachable from it.
    pub(super) fn alternatives(&self, expr: &Expr, scope: &Scope<'_>) -> Option<Alternatives> {
        match expr {
            Expr::Paren(e) => self.alternatives(&e.expr, scope),
            Expr::TsAs(e) => self.alternatives(&e.expr, scope),
            Expr::TsNonNull(e) => self.alternatives(&e.expr, scope),
            Expr::TsConstAssertion(e) => self.alternatives(&e.expr, scope),
            Expr::TsSatisfies(e) => self.alternatives(&e.expr, scope),
            Expr::TsTypeAssertion(e) => self.alternatives(&e.expr, scope),
            Expr::Await(e) => self.alternatives(&e.arg, scope),
            Expr::Cond(cond) => self.conditional(cond, scope),
            Expr::Tpl(tpl) => {
                let mut readings = Alternatives::single(Vec::new());
                let mut chooses = false;
                for (index, quasi) in tpl.quasis.iter().enumerate() {
                    let text = quasi
                        .cooked
                        .as_ref()
                        .map(|cooked| cooked.to_string_lossy().into_owned())
                        .unwrap_or_else(|| quasi.raw.to_string());
                    readings = readings.then(&Alternatives::single(vec![Piece::Lit(text)]))?;
                    if let Some(expr) = tpl.exprs.get(index) {
                        let part = self.reading_of(expr, scope)?;
                        chooses |= part.chooses();
                        readings = readings.then(&part)?;
                    }
                }
                chooses.then_some(readings)
            }
            Expr::Bin(bin) if bin.op == BinaryOp::Add => {
                let left = self.reading_of(&bin.left, scope)?;
                let right = self.reading_of(&bin.right, scope)?;
                if !left.chooses() && !right.chooses() {
                    return None;
                }
                left.then(&right)
            }
            Expr::Ident(ident) if scope.param_index(ident).is_none() => {
                scope.alts.get(&ident_key(ident)).cloned()
            }
            // `q || path` over a local written more than once: where no
            // write set `q`, the value is `path` (carrick#3679).
            Expr::Bin(bin)
                if matches!(bin.op, BinaryOp::LogicalOr | BinaryOp::NullishCoalescing) =>
            {
                let left = self
                    .alternatives(&bin.left, scope)
                    .filter(Alternatives::is_written)?;
                left.or_else(&self.reading_of(&bin.right, scope)?)
            }
            _ => None,
        }
    }

    /// The readings of an expression that is text, whether or not a
    /// conditional is in it.
    pub(super) fn reading_of(&self, expr: &Expr, scope: &Scope<'_>) -> Option<Alternatives> {
        if let Some(readings) = self.alternatives(expr, scope) {
            return Some(readings);
        }
        match self.eval(expr, scope) {
            Value::Str(pieces) => Some(Alternatives::single(pieces)),
            Value::Obj(_) | Value::Callback(_) => None,
        }
    }

    fn conditional(&self, cond: &CondExpr, scope: &Scope<'_>) -> Option<Alternatives> {
        let consequent = self.reading_of(&cond.cons, scope)?;
        let alternate = self.reading_of(&cond.alt, scope)?;
        // Two values nothing read: the slot is a value, as it always was.
        if consequent.holds_an_unread_value()
            && alternate.holds_an_unread_value()
            && !consequent.writes_a_path()
            && !alternate.writes_a_path()
        {
            return None;
        }
        let poisoned = consequent.poisoned
            || alternate.poisoned
            || (consequent.writes_a_path() && alternate.holds_an_unread_value())
            || (alternate.writes_a_path() && consequent.holds_an_unread_value());
        let (test, swapped, names) = self.test_key(&cond.test, scope, cond.span);
        // The branches are exclusive, and a test inside one only refines it
        // (`mode === "a" ? A : mode === "b" ? B : C`), so neither is a cross
        // product: only a test entangled within a branch entangles this.
        let mut tests = tests_of(&consequent.tests, &alternate.tests);
        tests.entry(test.clone()).or_default().extend(names);
        let entangled = consequent.entangled || alternate.entangled;
        let writes = merged_writes(&consequent.writes, &alternate.writes);
        let mut readings = Vec::new();
        for (written, side) in [consequent, alternate].iter().enumerate() {
            // The branch of the test as it is keyed: a negation's consequent
            // is the alternate of the test it negates.
            let branch = written ^ usize::from(swapped);
            for reading in &side.readings {
                let mut taken = reading.taken.clone();
                if taken.get(&test).is_some_and(|prior| *prior != branch) {
                    continue;
                }
                taken.insert(test.clone(), branch);
                readings.push(Reading {
                    taken,
                    ..reading.clone()
                });
            }
        }
        if readings.is_empty() || readings.len() > MAX_READINGS {
            return None;
        }
        Some(Alternatives {
            readings,
            leading: true,
            poisoned,
            tests,
            entangled,
            writes,
        })
    }

    /// The name of a test, whether its branches are swapped against the test
    /// that name stands for, and the names it reads. A test the function reads
    /// the same way every time is named by the test it affirms: a leading
    /// `!`, `!=` and `!==` are read as the test they negate with the branches
    /// swapped. Any other test has a name only this conditional has and
    /// reads no name.
    fn test_key(
        &self,
        test: &Expr,
        scope: &Scope<'_>,
        at: Span,
    ) -> (String, bool, BTreeSet<String>) {
        if !is_stable(test, scope) {
            return (format!("@{}", at.lo.0), false, BTreeSet::new());
        }
        let (text, swapped) = self.affirmed_test(test);
        let mut names = BTreeSet::new();
        self.names_read(test, &mut names);
        (format!("t:{text}"), swapped, names)
    }

    /// A stable test as the test it affirms, and whether it negates it.
    fn affirmed_test(&self, test: &Expr) -> (String, bool) {
        match test {
            Expr::Paren(e) => self.affirmed_test(&e.expr),
            Expr::Unary(unary) if unary.op == UnaryOp::Bang => {
                let (text, swapped) = self.affirmed_test(&unary.arg);
                (text, !swapped)
            }
            Expr::Bin(bin)
                if matches!(
                    bin.op,
                    BinaryOp::EqEq | BinaryOp::NotEq | BinaryOp::EqEqEq | BinaryOp::NotEqEq
                ) =>
            {
                let (op, swapped) = match bin.op {
                    BinaryOp::EqEq => ("==", false),
                    BinaryOp::NotEq => ("==", true),
                    BinaryOp::EqEqEq => ("===", false),
                    _ => ("===", true),
                };
                let side = |expr: &Expr| self.text(expr.unwrap_parens().span());
                (
                    format!("{} {op} {}", side(&bin.left), side(&bin.right)),
                    swapped,
                )
            }
            other => (self.text(other.span()), false),
        }
    }

    /// The names a stable test reads: each name, and each member of one, in
    /// it.
    fn names_read(&self, test: &Expr, names: &mut BTreeSet<String>) {
        match test {
            Expr::Ident(_) | Expr::Member(_) => {
                names.insert(self.text(test.span()));
            }
            Expr::Paren(e) => self.names_read(&e.expr, names),
            Expr::TsNonNull(e) => self.names_read(&e.expr, names),
            Expr::Unary(unary) => self.names_read(&unary.arg, names),
            Expr::Bin(bin) => {
                self.names_read(&bin.left, names);
                self.names_read(&bin.right, names);
            }
            _ => {}
        }
    }

    /// The requests a call sends when a conditional chooses its URL or its
    /// method (carrick#2050, carrick#2051). `url` and `method` are what the
    /// call read as before.
    pub(super) fn alternation(
        &self,
        url_expr: Option<&Expr>,
        method_expr: Option<&Expr>,
        url: &[Piece],
        method: &MethodValue,
        scope: &Scope<'_>,
    ) -> Alternation {
        // A method read through a local written more than once is read as
        // it was: its ways' guards are the URL's alone (carrick#3679).
        let method_readings = method_expr
            .and_then(|expr| self.alternatives(expr, scope))
            .filter(|readings| !readings.is_written());
        let methods = method_readings
            .as_ref()
            .and_then(Alternatives::as_methods)
            .filter(|methods| methods.len() > 1);
        // Two different tests on one name: some combinations of their
        // branches cannot be taken together, and nothing here says which.
        if methods.is_some() && method_readings.as_ref().is_some_and(|m| m.entangled) {
            return Alternation::Nothing;
        }
        let mut urls = url_expr.and_then(|expr| self.alternatives(expr, scope));
        if let Some(readings) = &urls {
            // A reading that states a path states one with no empty segment
            // in it; one that waits on a caller is a caller's to settle.
            let states = |pieces: &[Piece]| {
                pieces.iter().any(Piece::is_parameter)
                    || render_target(pieces)
                        .is_some_and(|target| !target.replace("://", "").contains("//"))
            };
            if readings.poisoned && !readings.leading {
                return Alternation::Nothing;
            }
            // A base chosen by a test, or a branch that states no route of
            // its own: read as it was before a conditional was.
            if readings.poisoned || !readings.readings.iter().all(|r| states(&r.pieces)) {
                urls = None;
            } else if readings.entangled {
                // Two different tests on one name in the URL.
                return Alternation::Nothing;
            }
        }
        if urls.is_none() && methods.is_none() {
            return Alternation::Single;
        }
        // Both choose, and no test is shared: nothing in the source says
        // which method goes with which URL.
        if let (Some(urls), Some(methods)) = (&urls, &methods) {
            let tests = |taken: &mut dyn Iterator<Item = &Taken>| -> BTreeSet<String> {
                taken.flat_map(|taken| taken.keys().cloned()).collect()
            };
            let in_url = tests(&mut urls.readings.iter().map(|reading| &reading.taken));
            let in_method = tests(&mut methods.iter().map(|(taken, _)| taken));
            if in_url.is_disjoint(&in_method) {
                return Alternation::Nothing;
            }
        }
        // Every test the method and the URL read, for the pairs below.
        let tests = tests_of(
            &urls
                .as_ref()
                .map(|urls| urls.tests.clone())
                .unwrap_or_default(),
            &method_readings
                .as_ref()
                .map(|readings| readings.tests.clone())
                .unwrap_or_default(),
        );
        let writes = urls
            .as_ref()
            .map(|urls| urls.writes.clone())
            .unwrap_or_default();
        let url_rows: Vec<(Taken, Vec<Piece>, Vec<WayGuard>)> = match urls {
            Some(urls) => urls
                .readings
                .into_iter()
                .map(|reading| {
                    (
                        reading.taken,
                        reading.pieces,
                        reading.guards.into_iter().collect(),
                    )
                })
                .collect(),
            None => vec![(Taken::new(), url.to_vec(), Vec::new())],
        };
        let method_rows: Vec<(Taken, MethodValue)> =
            methods.unwrap_or_else(|| vec![(Taken::new(), method.clone())]);
        // Readings that differ only in a query or a fragment reach one route.
        // The one without the query is the one that is kept.
        // Ways under different guards are kept apart: a caller decides each.
        type Route = (MethodValue, Vec<Piece>, Vec<WayGuard>);
        let mut routes: BTreeMap<Route, Vec<Piece>> = BTreeMap::new();
        for (url_taken, pieces, guards) in &url_rows {
            for (method_taken, verb) in &method_rows {
                if together(url_taken, method_taken).is_none() {
                    continue;
                }
                // Two different tests on one name split between the method
                // and the URL: this pair may be one the source cannot send.
                if entangled(url_taken, method_taken, &tests) {
                    return Alternation::Nothing;
                }
                routes
                    .entry((verb.clone(), without_query(pieces), guards.clone()))
                    .and_modify(|held| {
                        if pieces < held {
                            held.clone_from(pieces);
                        }
                    })
                    .or_insert_with(|| pieces.clone());
            }
        }
        if routes.is_empty() {
            return Alternation::Nothing;
        }
        Alternation::Rows(
            routes
                .into_iter()
                .map(|((verb, _, guards), pieces)| (verb, pieces, guards))
                .collect(),
            writes,
        )
    }
}

/// What [`Reader::written_ways`] reads one local's writes with.
pub(super) struct WriteFlow<'f, 's> {
    /// The local written more than once.
    pub(super) local: &'f BindingKey,
    pub(super) scope: &'f Scope<'s>,
    /// The guards the walk of the body read ([`super::FnIr::guards`]).
    pub(super) guards: &'f [super::guards::GuardIr],
    /// The guards of the `if` tests the statement sits under, each with the
    /// branch taken.
    pub(super) path: Vec<(Option<WayGuard>, bool)>,
    /// Where each write read so far starts, for the write census
    /// ([`Alternation::census_checked`]).
    pub(super) placed: &'f mut BTreeSet<u32>,
}

impl Reader<'_> {
    /// What the local `flow.local`, written more than once, may hold after
    /// `stmt` runs, given what it may hold before it (`state`) under the
    /// tests of `flow.path` (carrick#3679, the ruling R3): one way per
    /// write. A write replaces every way it is reached on; an `if` keeps
    /// each branch's ways, under its test where a caller decides the test
    /// ([`WayGuard`]) and unguarded where it is any other test.
    ///
    /// `None` where a write is in any other statement or expression (a loop,
    /// a `try`, a `switch`, a write inside a call or a nested function), or
    /// its value reads the local itself: what the local holds there is not
    /// read.
    pub(super) fn written_ways(
        &self,
        stmt: &Stmt,
        state: Alternatives,
        flow: &mut WriteFlow<'_, '_>,
    ) -> Option<Alternatives> {
        let local = flow.local;
        if !writes_local(stmt, local) {
            return Some(state);
        }
        match stmt {
            Stmt::Expr(statement) => {
                let Expr::Assign(assign) = statement.expr.unwrap_parens() else {
                    return None;
                };
                let AssignTarget::Simple(SimpleAssignTarget::Ident(target)) = &assign.left else {
                    return None;
                };
                if assign.op != AssignOp::Assign
                    || ident_key(&target.id) != *local
                    || reads_local(&assign.right, local)
                {
                    return None;
                }
                flow.placed.insert(assign.span.lo.0);
                let mut written = self.reading_of(&assign.right, flow.scope)?;
                // A value read from a local this pass holds no ways for (one
                // the same statement writes too) says nothing of this one.
                let unread = written.readings.iter().any(|reading| {
                    reading.pieces.iter().any(|piece| {
                        matches!(
                            piece,
                            Piece::Opaque {
                                end: Root::Hole(HoleReason::Reassigned),
                                ..
                            }
                        )
                    })
                });
                if unread {
                    return None;
                }
                for (guard, holds) in &flow.path {
                    written = written.under(guard.as_ref(), *holds);
                }
                (!written.readings.is_empty()).then_some(written)
            }
            Stmt::Block(block) => {
                let mut state = state;
                for inner in &block.stmts {
                    state = self.written_ways(inner, state, flow)?;
                }
                Some(state)
            }
            Stmt::If(branching) => {
                let guard = self.way_guard(&branching.test, flow.scope, flow.guards);
                let mut taken = |stmt: Option<&Stmt>, holds: bool| {
                    let state = state.under(guard.as_ref(), holds);
                    let Some(stmt) = stmt else {
                        return Some(state);
                    };
                    flow.path.push((guard.clone(), holds));
                    let out = self.written_ways(stmt, state, flow);
                    flow.path.pop();
                    out
                };
                let consequent = taken(Some(&branching.cons), true)?;
                let alternate = taken(branching.alt.as_deref(), false)?;
                consequent.either(&alternate)
            }
            _ => None,
        }
    }

    /// The test of an `if` as one a caller decides (the ruling R3): the
    /// guard [`super::guards`] read at it compares the function's own
    /// parameter, or one key of it, that the body never assigns, with text
    /// by `===` / `!==`, or tests a prefix of it (`startsWith`, an anchored
    /// literal pattern), under any number of `!`. `None` for any other test.
    fn way_guard(
        &self,
        test: &Expr,
        scope: &Scope<'_>,
        guards: &[super::guards::GuardIr],
    ) -> Option<WayGuard> {
        let mut inner = test;
        loop {
            match inner {
                Expr::Paren(paren) => inner = &paren.expr,
                Expr::Unary(unary) if unary.op == UnaryOp::Bang => inner = &unary.arg,
                _ => break,
            }
        }
        let span = inner.span();
        let guard = guards
            .iter()
            .find(|guard| guard.site.lo == span.lo.0 && guard.site.hi == span.hi.0)?;
        let ((depth, index, keys), test) = guard.parameter_test()?;
        let name = &scope.params.get(index)?.as_ref()?.0;
        if depth != 0 || scope.reassigned.contains(name) {
            return None;
        }
        let subject = match keys.as_slice() {
            [] => Piece::Param(index, name.clone()),
            [key] => Piece::ParamKey(index, key.clone(), guard.subject_text.clone()),
            _ => return None,
        };
        Some(WayGuard {
            subject: vec![subject],
            test,
            holds: !guard.negated,
        })
    }
}

/// Whether `stmt` writes the local `local` anywhere, inside a function
/// written in it too.
pub(super) fn writes_local(stmt: &Stmt, local: &BindingKey) -> bool {
    use swc_ecma_visit::{Visit, VisitWith};
    struct Writes<'k> {
        local: &'k BindingKey,
        found: bool,
    }
    impl Visit for Writes<'_> {
        fn visit_assign_expr(&mut self, assign: &AssignExpr) {
            if let AssignTarget::Simple(SimpleAssignTarget::Ident(target)) = &assign.left {
                self.found |= ident_key(&target.id) == *self.local;
            }
            assign.visit_children_with(self);
        }
        fn visit_update_expr(&mut self, update: &UpdateExpr) {
            if let Expr::Ident(target) = &*update.arg {
                self.found |= ident_key(target) == *self.local;
            }
            update.visit_children_with(self);
        }
    }
    let mut writes = Writes {
        local,
        found: false,
    };
    stmt.visit_with(&mut writes);
    writes.found
}

/// Whether `expr` names the local `local`.
fn reads_local(expr: &Expr, local: &BindingKey) -> bool {
    use swc_ecma_visit::{Visit, VisitWith};
    struct Reads<'k> {
        local: &'k BindingKey,
        found: bool,
    }
    impl Visit for Reads<'_> {
        fn visit_ident(&mut self, ident: &Ident) {
            self.found |= ident_key(ident) == *self.local;
        }
    }
    let mut reads = Reads {
        local,
        found: false,
    };
    expr.visit_with(&mut reads);
    reads.found
}

/// The pieces of a URL up to its query or fragment.
fn without_query(url: &[Piece]) -> Vec<Piece> {
    let mut route = Vec::new();
    for piece in url {
        match piece {
            Piece::Lit(text) => match text.find(['?', '#']) {
                // A query written as a piece of its own (`${path}?q=1`)
                // leaves no text before it.
                Some(0) => break,
                Some(at) => {
                    route.push(Piece::Lit(text[..at].to_string()));
                    break;
                }
                None => route.push(piece.clone()),
            },
            other => route.push(other.clone()),
        }
    }
    route
}

/// Whether a test reads the same way each time the function runs it: names
/// the function does not assign again, members of them, literals, and
/// negations, comparisons and `&&`/`||` of those.
fn is_stable(expr: &Expr, scope: &Scope<'_>) -> bool {
    match expr {
        Expr::Ident(ident) => !scope.reassigned.contains(ident.sym.as_ref()),
        Expr::Lit(_) => true,
        Expr::Paren(e) => is_stable(&e.expr, scope),
        Expr::TsNonNull(e) => is_stable(&e.expr, scope),
        Expr::Unary(unary) => unary.op == UnaryOp::Bang && is_stable(&unary.arg, scope),
        Expr::Member(member) => {
            matches!(member.prop, MemberProp::Ident(_)) && is_stable(&member.obj, scope)
        }
        Expr::Bin(bin) => {
            matches!(
                bin.op,
                BinaryOp::EqEq
                    | BinaryOp::NotEq
                    | BinaryOp::EqEqEq
                    | BinaryOp::NotEqEq
                    | BinaryOp::Lt
                    | BinaryOp::LtEq
                    | BinaryOp::Gt
                    | BinaryOp::GtEq
                    | BinaryOp::LogicalAnd
                    | BinaryOp::LogicalOr
                    | BinaryOp::NullishCoalescing
            ) && is_stable(&bin.left, scope)
                && is_stable(&bin.right, scope)
        }
        _ => false,
    }
}

/// The expression a call's options object writes its `method` as, when it
/// writes one last.
pub(super) fn method_expr(options: &Expr) -> Option<Expr> {
    let Expr::Object(object) = options.unwrap_parens() else {
        return None;
    };
    object.props.iter().rev().find_map(|prop| match prop {
        PropOrSpread::Prop(prop) => match &**prop {
            Prop::KeyValue(pair) if prop_name(&pair.key).as_deref() == Some("method") => {
                Some((*pair.value).clone())
            }
            Prop::Shorthand(ident) if ident.sym == *"method" => Some(Expr::Ident(ident.clone())),
            _ => None,
        },
        PropOrSpread::Spread(_) => None,
    })
}

fn prop_name(key: &PropName) -> Option<String> {
    match key {
        PropName::Ident(ident) => Some(ident.sym.to_string()),
        PropName::Str(text) => Some(text.value.to_string_lossy().into_owned()),
        _ => None,
    }
}
