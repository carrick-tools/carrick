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
//!   taken together.
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

use std::collections::BTreeMap;

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
        let mut readings = Vec::new();
        for a in &self.readings {
            for b in &next.readings {
                if let Some(taken) = together(&a.taken, &b.taken) {
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
        let test = self.test_key(&cond.test, scope, cond.span);
        let mut readings = Vec::new();
        for (branch, side) in [consequent, alternate].iter().enumerate() {
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
        })
    }

    /// The name of a test: its text when it is one the function reads the same
    /// way every time, and otherwise a name only this conditional has.
    fn test_key(&self, test: &Expr, scope: &Scope<'_>, at: Span) -> String {
        if is_stable(test, scope) {
            format!("t:{}", self.text(test.span()))
        } else {
            format!("@{}", at.lo.0)
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
