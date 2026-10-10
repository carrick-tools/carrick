//! What a conditional leaves a request's URL able to read as (carrick#2050).
//!
//! ```ignore
//! const segment = kind === "draft" ? `drafts/${id}` : `posts/${id}`;
//! await fetch(`/api/content/${segment}/publish`, { method: "POST" });
//! ```
//!
//! A value read through a conditional is not one value. [`Reader::eval`]
//! keeps the expression's text as an opaque value, which a URL then reads as
//! one path parameter: here that is a path one segment shorter than either
//! route the branches write.
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
//! - **A value that may hold a `/` is no path parameter.** Where one branch
//!   writes a path and the other is a value nothing here read, the slot may
//!   span segments, so the request states no row rather than a placeholder
//!   one. A conditional that leads the URL is a base, which the row was
//!   always allowed to read as opaque text, and is read as before.
//! - **A reading that differs from another only in a query is the same
//!   route.**
//!
//! Everything else is read exactly as before: this module only answers where
//! a conditional is reachable from the URL.

use std::collections::{BTreeMap, BTreeSet};

use swc_common::{Span, Spanned};
use swc_ecma_ast::*;

use super::{MethodValue, Piece, Reader, Scope, Value, concat, ident_key, render_target};

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
            readings: vec![Reading {
                taken: Taken::new(),
                pieces,
            }],
            leading: false,
            poisoned: false,
            tests: Tests::new(),
            entangled: false,
        }
    }

    /// Whether a conditional is in it.
    fn chooses(&self) -> bool {
        self.readings.len() > 1 || self.poisoned
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
                if let Some(taken) = together(&a.taken, &b.taken) {
                    tangled |= entangled(&a.taken, &b.taken, &tests);
                    readings.push(Reading {
                        taken,
                        pieces: concat([a.pieces.clone(), b.pieces.clone()]),
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
        })
    }
}

/// How a request reads once its conditionals are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Alternation {
    /// No conditional is in the URL or the method: the request reads as it
    /// always did.
    Single,
    /// One request per way the conditionals can go.
    Rows(Vec<(MethodValue, Vec<Piece>)>),
    /// The source does not say which request goes out: none is stated.
    Nothing,
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
            _ => None,
        }
    }

    /// The readings of an expression that is text, whether or not a
    /// conditional is in it.
    fn reading_of(&self, expr: &Expr, scope: &Scope<'_>) -> Option<Alternatives> {
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
                    pieces: reading.pieces.clone(),
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

    /// The requests a call sends when a conditional chooses its URL
    /// (carrick#2050). `method` is what the call read its method as.
    pub(super) fn alternation(
        &self,
        url_expr: Option<&Expr>,
        method: &MethodValue,
        scope: &Scope<'_>,
    ) -> Alternation {
        let Some(readings) = url_expr.and_then(|expr| self.alternatives(expr, scope)) else {
            return Alternation::Single;
        };
        // A reading that states a path states one with no empty segment in
        // it; one that waits on a caller is a caller's to settle.
        let states = |pieces: &[Piece]| {
            pieces.iter().any(Piece::is_parameter)
                || render_target(pieces)
                    .is_some_and(|target| !target.replace("://", "").contains("//"))
        };
        if readings.poisoned && !readings.leading {
            return Alternation::Nothing;
        }
        // A base chosen by a test, or a branch that states no route of its
        // own: read as it was before a conditional was.
        if readings.poisoned || !readings.readings.iter().all(|r| states(&r.pieces)) {
            return Alternation::Single;
        }
        // Two different tests on one name: some combinations of their
        // branches cannot be taken together, and nothing here says which.
        if readings.entangled {
            return Alternation::Nothing;
        }
        // Readings that differ only in a query or a fragment reach one route.
        // The one without the query is the one that is kept.
        let mut routes: BTreeMap<Vec<Piece>, Vec<Piece>> = BTreeMap::new();
        for reading in readings.readings {
            routes
                .entry(without_query(&reading.pieces))
                .and_modify(|held| {
                    if reading.pieces < *held {
                        held.clone_from(&reading.pieces);
                    }
                })
                .or_insert(reading.pieces);
        }
        Alternation::Rows(
            routes
                .into_values()
                .map(|pieces| (method.clone(), pieces))
                .collect(),
        )
    }
}

/// The pieces of a URL up to its query or fragment.
fn without_query(url: &[Piece]) -> Vec<Piece> {
    let mut route = Vec::new();
    for piece in url {
        match piece {
            Piece::Lit(text) => match text.find(['?', '#']) {
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
