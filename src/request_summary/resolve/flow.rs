//! Which writes of a function's own `let` reach each read of it, and under
//! which `if` tests (carrick#3679 S8, the one-value-resolver decision
//! record's S8 row). One answerer for "which writes reach this read": it runs
//! over the body at extract time, where the AST is in hand (section 2.4), and
//! its span projection ([`WrittenReads`]) goes into the binding table, where
//! [`super::Resolver::written_at`] answers the text reader at link time.
//!
//! It answers which values reach, never which one a caller picks: a way's
//! tests are handed on, and each consumer decides them in its own value form
//! (D4). The text reader takes the union of the ways.
//!
//! The rules:
//! - A statement that is a whole `local = rhs` replaces every way that
//!   reaches it; `local ??= rhs` and `local ||= rhs` add a way and keep the
//!   rest. Reads in `rhs` see the ways before the write.
//! - An `if` reads each branch under its test; a missing `else` is the state
//!   under the test's negation. A way that comes through both branches
//!   untouched is one way, without that test (dropping a test that holds on
//!   every path is sound; attaching one that does not hold is not).
//! - `return` and `throw` end a branch: it passes nothing on. A read no path
//!   reaches has no entry.
//! - A write of the local in any other form or place (inside an `if` test, a
//!   call argument, a loop, `try`, `switch`, a labelled statement, a function
//!   written in the body, `++`, a compound operator, a destructuring) refuses
//!   the whole local: [`reaching`] answers `None` and every reader keeps the
//!   census's set. Any other statement passes the ways through.
//! - A read inside a function written in the body has no entry.
//! - A read reached by more than [`MAX_VALUES`] ways has no entry; so does
//!   every read after the ways pass the cap, until a `=` replaces them.

use super::MAX_VALUES;
use crate::binding_scope::{BindingKey, ident_key};
use crate::graphql_document_sites::unwrap_expression;
use std::collections::BTreeMap;
use swc_common::Spanned;
use swc_ecma_ast::{
    ArrowExpr, AssignExpr, AssignOp, AssignTarget, Class, Constructor, Decl, Expr, ForHead,
    ForInStmt, ForOfStmt, Function, GetterProp, Ident, Lit, SetterProp, SimpleAssignTarget, Stmt,
    UnaryOp, UpdateExpr,
};
use swc_ecma_visit::{Visit, VisitWith};

/// Where a way's value comes from.
#[derive(Debug, Clone, Copy)]
pub(in crate::request_summary) enum Source<'a> {
    /// The declaration's initialiser.
    Init(&'a Expr),
    /// No initialiser, or one that is `null`, `undefined` or `void 0`.
    Unwritten,
    /// A placed assignment (`=`, `??=`, `||=`).
    Write(&'a AssignExpr),
}

/// One path to a read: where the value comes from, and every `if` test on
/// the path with the branch taken (the decision record's
/// `Hop::Branch { test, taken }`).
#[derive(Debug, Clone)]
pub(in crate::request_summary) struct Way<'a> {
    pub(in crate::request_summary) from: Source<'a>,
    pub(in crate::request_summary) tests: Vec<(&'a Expr, bool)>,
}

impl Way<'_> {
    /// The way as spans: what the binding table keeps.
    fn at(&self) -> WayAt {
        WayAt {
            from: match self.from {
                Source::Init(_) => WayFrom::Init,
                Source::Unwritten => WayFrom::Unwritten,
                Source::Write(assign) => WayFrom::Write(assign.span.lo.0, assign.op),
            },
            tests: self
                .tests
                .iter()
                .map(|(test, holds)| {
                    let span = test.span();
                    (span.lo.0, span.hi.0, *holds)
                })
                .collect(),
        }
    }
}

/// Where a way's value comes from, by span (the binding table's form).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::request_summary) enum WayFrom {
    /// The binding's initialiser ([`super::ModuleBinding::text`]).
    Init,
    /// No value: no initialiser, or a nullish one.
    Unwritten,
    /// The placed write starting here, with its operator
    /// ([`super::BindingWrite::at`]).
    Write(u32, AssignOp),
}

/// [`Way`] by span: each test as `(lo, hi, holds)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::request_summary) struct WayAt {
    pub(in crate::request_summary) from: WayFrom,
    pub(in crate::request_summary) tests: Vec<(u32, u32, bool)>,
}

/// What [`reaching`] found, by span: the ways at each read, keyed by where
/// the read's identifier starts, and where every placed write the flow saw
/// starts (in the census's numbering, `assign.span.lo`), for the census
/// cross-check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::request_summary) struct WrittenReads {
    pub(in crate::request_summary) reads: BTreeMap<u32, Vec<WayAt>>,
    pub(in crate::request_summary) writes: Vec<u32>,
}

/// What [`reaching`] found: the ways at each read, keyed by where the read's
/// identifier starts, and where each placed write starts, dead code
/// included.
#[derive(Debug, Clone, Default)]
pub(in crate::request_summary) struct Reaching<'a> {
    pub(in crate::request_summary) reads: BTreeMap<u32, Vec<Way<'a>>>,
    pub(in crate::request_summary) writes: Vec<u32>,
}

impl Reaching<'_> {
    /// The span projection the binding table keeps.
    pub(in crate::request_summary) fn table(&self) -> WrittenReads {
        WrittenReads {
            reads: self
                .reads
                .iter()
                .map(|(at, ways)| (*at, ways.iter().map(Way::at).collect()))
                .collect(),
            writes: self.writes.clone(),
        }
    }
}

/// Which writes of the local `key`, declared once at the top level of
/// `stmts` (a function body), reach each of its reads. `None` where the body
/// writes it in a form the flow does not place, or never declares it there.
pub(in crate::request_summary) fn reaching<'a>(
    stmts: &'a [Stmt],
    key: &BindingKey,
) -> Option<Reaching<'a>> {
    let mut flow = Flow {
        key,
        out: Reaching::default(),
        refused: false,
        declared: false,
    };
    flow.block(stmts, State::Dead, &[]);
    (flow.declared && !flow.refused).then_some(flow.out)
}

/// The ways that reach a point of the body.
#[derive(Debug, Clone)]
enum State<'a> {
    Live(Vec<Way<'a>>),
    /// No path reaches here (or the local is not declared yet).
    Dead,
    /// More ways than [`MAX_VALUES`] reach here.
    Over,
}

impl<'a> State<'a> {
    fn under(&self, test: &'a Expr, holds: bool) -> Self {
        match self {
            Self::Live(ways) => Self::Live(
                ways.iter()
                    .map(|way| {
                        let mut way = way.clone();
                        way.tests.push((test, holds));
                        way
                    })
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn capped(ways: Vec<Way<'a>>) -> Self {
        if ways.len() > MAX_VALUES {
            Self::Over
        } else {
            Self::Live(ways)
        }
    }
}

/// Whether two ways come from the same place under the same tests.
fn same(a: &Way<'_>, b: &Way<'_>) -> bool {
    let from = |way: &Way<'_>| match way.from {
        Source::Init(init) => (0, init.span().lo.0),
        Source::Unwritten => (1, 0),
        Source::Write(assign) => (2, assign.span.lo.0),
    };
    from(a) == from(b)
        && a.tests.len() == b.tests.len()
        && a.tests
            .iter()
            .zip(&b.tests)
            .all(|((x, p), (y, q))| p == q && x.span() == y.span())
}

/// The ways after an `if` on `test`: those of each branch, and one way
/// without the test for each that came through both branches untouched.
fn joined<'a>(taken: State<'a>, other: State<'a>, test: &'a Expr) -> State<'a> {
    let (taken, mut other) = match (taken, other) {
        (State::Dead, state) | (state, State::Dead) => return state,
        (State::Over, _) | (_, State::Over) => return State::Over,
        (State::Live(taken), State::Live(other)) => (taken, other),
    };
    let ends_with = |way: &Way<'_>, holds: bool| {
        way.tests
            .last()
            .is_some_and(|(last, last_holds)| *last_holds == holds && last.span() == test.span())
    };
    let mut out: Vec<Way<'a>> = Vec::new();
    for way in taken {
        let mut stripped = way.clone();
        stripped.tests.pop();
        let twin = ends_with(&way, true)
            .then(|| {
                other.iter().position(|candidate| {
                    if !ends_with(candidate, false) {
                        return false;
                    }
                    let mut candidate = candidate.clone();
                    candidate.tests.pop();
                    same(&candidate, &stripped)
                })
            })
            .flatten();
        let way = match twin {
            Some(at) => {
                other.remove(at);
                stripped
            }
            None => way,
        };
        if !out.iter().any(|kept| same(kept, &way)) {
            out.push(way);
        }
    }
    for way in other {
        if !out.iter().any(|kept| same(kept, &way)) {
            out.push(way);
        }
    }
    State::capped(out)
}

struct Flow<'k, 'a> {
    key: &'k BindingKey,
    out: Reaching<'a>,
    refused: bool,
    declared: bool,
}

impl<'a> Flow<'_, 'a> {
    fn block(
        &mut self,
        stmts: &'a [Stmt],
        mut state: State<'a>,
        path: &[(&'a Expr, bool)],
    ) -> State<'a> {
        for stmt in stmts {
            state = self.stmt(stmt, state, path);
        }
        state
    }

    fn stmt(&mut self, stmt: &'a Stmt, state: State<'a>, path: &[(&'a Expr, bool)]) -> State<'a> {
        match stmt {
            Stmt::Decl(Decl::Var(var)) => {
                let mut state = state;
                for declarator in &var.decls {
                    let own = matches!(
                        &declarator.name,
                        swc_ecma_ast::Pat::Ident(name) if ident_key(&name.id) == *self.key
                    );
                    if !own {
                        self.uses(declarator, &state);
                        continue;
                    }
                    if let Some(init) = &declarator.init {
                        self.uses(&**init, &state);
                    }
                    self.declared = true;
                    let from = match declarator.init.as_deref() {
                        Some(init) if !nullish(init) => Source::Init(init),
                        _ => Source::Unwritten,
                    };
                    state = State::Live(vec![Way {
                        from,
                        tests: path.to_vec(),
                    }]);
                }
                state
            }
            Stmt::Expr(statement) => {
                if let Expr::Assign(assign) = unwrap_expression(&statement.expr)
                    && let AssignTarget::Simple(SimpleAssignTarget::Ident(target)) = &assign.left
                    && ident_key(&target.id) == *self.key
                {
                    self.uses(&*assign.right, &state);
                    self.out.writes.push(assign.span.lo.0);
                    let way = Way {
                        from: Source::Write(assign),
                        tests: path.to_vec(),
                    };
                    return match (assign.op, state) {
                        (_, State::Dead) => State::Dead,
                        (AssignOp::Assign, _) => State::Live(vec![way]),
                        (AssignOp::NullishAssign | AssignOp::OrAssign, State::Live(mut ways)) => {
                            ways.push(way);
                            State::capped(ways)
                        }
                        (AssignOp::NullishAssign | AssignOp::OrAssign, State::Over) => State::Over,
                        (_, state) => {
                            self.refused = true;
                            state
                        }
                    };
                }
                self.uses(&*statement.expr, &state);
                state
            }
            Stmt::Block(block) => self.block(&block.stmts, state, path),
            Stmt::If(branch) => {
                let test: &'a Expr = &branch.test;
                self.uses(test, &state);
                let mut taken_path = path.to_vec();
                taken_path.push((test, true));
                let taken = self.stmt(&branch.cons, state.under(test, true), &taken_path);
                let mut other_path = path.to_vec();
                other_path.push((test, false));
                let other = match &branch.alt {
                    Some(alt) => self.stmt(alt, state.under(test, false), &other_path),
                    None => state.under(test, false),
                };
                joined(taken, other, test)
            }
            Stmt::Return(exit) => {
                if let Some(arg) = &exit.arg {
                    self.uses(&**arg, &state);
                }
                State::Dead
            }
            Stmt::Throw(exit) => {
                self.uses(&*exit.arg, &state);
                State::Dead
            }
            other => {
                self.uses(other, &state);
                state
            }
        }
    }

    /// Records every read of the local in `node` (outside functions written
    /// in it) with the ways in `state`; a write of it anywhere in `node`
    /// refuses the local.
    fn uses<N: for<'k> VisitWith<Uses<'k>>>(&mut self, node: &N, state: &State<'a>) {
        let mut uses = Uses {
            key: self.key,
            reads: Vec::new(),
            writes: false,
            nested: 0,
        };
        node.visit_with(&mut uses);
        self.refused |= uses.writes;
        if let State::Live(ways) = state {
            for read in uses.reads {
                self.out.reads.insert(read, ways.clone());
            }
        }
    }
}

/// `null`, `undefined` or `void 0`: a declaration that holds no value.
fn nullish(init: &Expr) -> bool {
    match unwrap_expression(init) {
        Expr::Lit(Lit::Null(_)) => true,
        Expr::Ident(ident) => ident.sym == "undefined",
        Expr::Unary(unary) => unary.op == UnaryOp::Void && matches!(&*unary.arg, Expr::Lit(_)),
        _ => false,
    }
}

/// The reads and writes of one local in a node.
struct Uses<'k> {
    key: &'k BindingKey,
    /// Where each read outside a function written in the node starts.
    reads: Vec<u32>,
    /// The node writes the local, anywhere.
    writes: bool,
    /// How many functions written in the node the visit is inside.
    nested: usize,
}

impl Uses<'_> {
    fn mentions<N: for<'k> VisitWith<Mentions<'k>>>(&self, node: &N) -> bool {
        let mut mentions = Mentions {
            key: self.key,
            found: false,
        };
        node.visit_with(&mut mentions);
        mentions.found
    }

    fn inside<N: VisitWith<Self>>(&mut self, node: &N) {
        self.nested += 1;
        node.visit_children_with(self);
        self.nested -= 1;
    }
}

impl Visit for Uses<'_> {
    fn visit_ident(&mut self, ident: &Ident) {
        if self.nested == 0 && ident_key(ident) == *self.key {
            self.reads.push(ident.span.lo.0);
        }
    }

    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        match &assign.left {
            AssignTarget::Simple(SimpleAssignTarget::Ident(target)) => {
                self.writes |= ident_key(&target.id) == *self.key;
                // A compound operator reads the target too.
                if assign.op != AssignOp::Assign {
                    target.id.visit_with(self);
                }
            }
            // `x.p = …` writes a property, not the binding.
            AssignTarget::Simple(SimpleAssignTarget::Member(member)) => member.visit_with(self),
            other => {
                self.writes |= self.mentions(other);
                other.visit_with(self);
            }
        }
        assign.right.visit_with(self);
    }

    fn visit_update_expr(&mut self, update: &UpdateExpr) {
        match unwrap_expression(&update.arg) {
            Expr::Ident(target) if ident_key(target) == *self.key => self.writes = true,
            _ => update.arg.visit_with(self),
        }
    }

    fn visit_for_of_stmt(&mut self, stmt: &ForOfStmt) {
        if let ForHead::Pat(pat) = &stmt.left {
            self.writes |= self.mentions(&**pat);
        }
        stmt.visit_children_with(self);
    }

    fn visit_for_in_stmt(&mut self, stmt: &ForInStmt) {
        if let ForHead::Pat(pat) = &stmt.left {
            self.writes |= self.mentions(&**pat);
        }
        stmt.visit_children_with(self);
    }

    fn visit_function(&mut self, function: &Function) {
        self.inside(function);
    }

    fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
        self.inside(arrow);
    }

    fn visit_class(&mut self, class: &Class) {
        self.inside(class);
    }

    fn visit_constructor(&mut self, constructor: &Constructor) {
        self.inside(constructor);
    }

    fn visit_getter_prop(&mut self, getter: &GetterProp) {
        self.inside(getter);
    }

    fn visit_setter_prop(&mut self, setter: &SetterProp) {
        self.inside(setter);
    }
}

/// Whether a node names the local anywhere.
struct Mentions<'k> {
    key: &'k BindingKey,
    found: bool,
}

impl Visit for Mentions<'_> {
    fn visit_ident(&mut self, ident: &Ident) {
        self.found |= ident_key(ident) == *self.key;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swc_common::{FileName, Globals, Mark, SourceMap, SourceMapper, sync::Lrc};
    use swc_ecma_parser::{Syntax, TsSyntax, parse_file_as_module};
    use swc_ecma_transforms_base::resolver;
    use swc_ecma_visit::VisitMutWith;

    /// The ways at each read of `p` in the body of the first function of
    /// `source`, as `(source, tests)` by read order: a source is `init`,
    /// `unwritten` or the write's text; a test is its text, `!` before it
    /// when it fails.
    fn ways(source: &str) -> Option<Vec<Vec<(String, Vec<String>)>>> {
        let map: Lrc<SourceMap> = Default::default();
        let file = map.new_source_file(
            Lrc::new(FileName::Custom("t.ts".into())),
            source.to_string(),
        );
        let module = parse_file_as_module(
            &file,
            Syntax::Typescript(TsSyntax::default()),
            Default::default(),
            None,
            &mut Vec::new(),
        )
        .unwrap();
        let mut module = module;
        swc_common::GLOBALS.set(&Globals::new(), || {
            module.visit_mut_with(&mut resolver(Mark::new(), Mark::new(), true));
        });
        let function = module
            .body
            .iter()
            .find_map(|item| match item {
                swc_ecma_ast::ModuleItem::Stmt(Stmt::Decl(Decl::Fn(f))) => Some(f),
                _ => None,
            })
            .unwrap();
        let stmts = &function.function.body.as_ref().unwrap().stmts;
        let key = stmts
            .iter()
            .find_map(|stmt| match stmt {
                Stmt::Decl(Decl::Var(var)) => var.decls.iter().find_map(|d| match &d.name {
                    swc_ecma_ast::Pat::Ident(name) if name.id.sym == "p" => {
                        Some(ident_key(&name.id))
                    }
                    _ => None,
                }),
                _ => None,
            })
            .unwrap();
        let text = |span: swc_common::Span| map.span_to_snippet(span).unwrap();
        let found = reaching(stmts, &key)?;
        Some(
            found
                .reads
                .values()
                .map(|ways| {
                    ways.iter()
                        .map(|way| {
                            let from = match way.from {
                                Source::Init(_) => "init".to_string(),
                                Source::Unwritten => "unwritten".to_string(),
                                Source::Write(assign) => text(assign.right.span()),
                            };
                            let tests = way
                                .tests
                                .iter()
                                .map(|(test, holds)| {
                                    format!(
                                        "{}{}",
                                        if *holds { "" } else { "!" },
                                        text(test.span())
                                    )
                                })
                                .collect();
                            (from, tests)
                        })
                        .collect()
                })
                .collect(),
        )
    }

    fn way(from: &str, tests: &[&str]) -> (String, Vec<String>) {
        (
            from.to_string(),
            tests.iter().map(|test| test.to_string()).collect(),
        )
    }

    #[test]
    fn a_way_carries_the_tests_on_its_path() {
        let got = ways(
            r#"function f(a: boolean, b: boolean) {
                let p = "/a";
                if (a) { p = "/b"; } else if (b) { return; }
                use(p);
            }"#,
        )
        .unwrap();
        assert_eq!(
            got,
            [vec![way("\"/b\"", &["a"]), way("init", &["!a", "!b"])]]
        );
    }

    #[test]
    fn an_if_that_leaves_the_local_alone_adds_no_test() {
        let got = ways(
            r#"function f(a: boolean, b: boolean) {
                let p = "/a";
                if (b) p = "/b";
                if (a) { log(); } else { log(); }
                if (a) log();
                use(p);
            }"#,
        )
        .unwrap();
        assert_eq!(got, [vec![way("\"/b\"", &["b"]), way("init", &["!b"])]]);
    }

    #[test]
    fn a_read_in_a_write_sees_the_ways_before_it() {
        let got = ways(
            r#"function f(a: boolean) {
                let p = "/a";
                if (a) p = p + "?q=1";
                use(p);
            }"#,
        )
        .unwrap();
        assert_eq!(
            got,
            [
                vec![way("init", &["a"])],
                vec![way("p + \"?q=1\"", &["a"]), way("init", &["!a"])],
            ]
        );
    }

    #[test]
    fn a_null_initialiser_is_unwritten_and_a_logical_assign_keeps_it() {
        let got = ways(
            r#"function f() {
                let p: string | null = null;
                p ??= "/b";
                use(p);
            }"#,
        )
        .unwrap();
        assert_eq!(got, [vec![way("unwritten", &[]), way("\"/b\"", &[])]]);
    }

    #[test]
    fn a_read_inside_a_function_or_after_every_branch_exits_has_no_entry() {
        let got = ways(
            r#"function f(a: boolean) {
                let p = "/a";
                const g = () => use(p);
                if (a) { p = "/b"; return; } else { throw new Error(); }
                use(p);
            }"#,
        )
        .unwrap();
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn a_write_the_flow_does_not_place_refuses_the_local() {
        for body in [
            "for (const x of xs) p = x;",
            "if ((p = \"/b\")) {}",
            "use((p = \"/b\"));",
            "p += \"/b\";",
            "p++;",
            "[p] = xs;",
            "const g = () => { p = \"/b\"; };",
            "try { p = \"/b\"; } catch {}",
        ] {
            let source =
                format!("function f(xs: string[]) {{ let p: any = \"/a\"; {body} use(p); }}");
            assert!(ways(&source).is_none(), "{body}");
        }
    }

    #[test]
    fn past_the_cap_a_read_has_no_entry_until_a_write_replaces_every_way() {
        let writes: String = (0..=MAX_VALUES)
            .map(|n| format!("p ??= \"/{n}\";"))
            .collect();
        let source =
            format!("function f() {{ let p: any = \"/a\"; {writes} use(p); p = \"/z\"; use(p); }}");
        let got = ways(&source).unwrap();
        assert_eq!(got, [vec![way("\"/z\"", &[])]]);
    }
}
