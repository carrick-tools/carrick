//! What a function compares a request value against, and where that value
//! comes from (carrick#2386): the receive side of the request summaries.
//!
//! A service that dispatches by hand states each route it serves as a
//! comparison of something it was handed: `pathname === "/items"`, `case
//! "/health":`, `pathname.startsWith("/items/")`, `ROUTES[pathname]`. Those
//! are the language's own comparison forms, so they are read with no library
//! or framework named. What this pass adds over [`crate::compared_route_literals`]
//! is the chain: the value compared is read as a [`Subject`] (a root and the
//! accesses down from it), and a root that is a parameter is followed out of
//! the function, the way the send side composes a wrapper's request over the
//! call graph:
//!
//! - into each in-repo caller, with that caller's argument in its place;
//! - into the call a function is handed to (a callback, a function passed by
//!   name, or one written inside an argument), whose callee then names where
//!   the parameter comes from: a parameter of the enclosing function, a
//!   package export, or a global;
//! - and, with no caller and no call it is handed to, to the function itself
//!   when the module offers it (an entry point) or to a hole.
//!
//! The same positions name operations other than routes: `case "READY":`
//! on a message field, `HANDLERS[event.type]`. So any string literal is read,
//! and each chain states its [`GuardShape`]: a route path, another name, or
//! a constant this pass cannot read. A constant (`url.pathname ===
//! HEALTH_ROUTE`, `msg.kind === Kinds.STOP`) is read by the value flow the
//! request summaries read names with ([`LinkedNames`], carrick#1562), once
//! every module's uses of it are known. A hop into a call also states the
//! text at the call's other arguments (`child.on("message", <fn>)`), and a
//! chain that stops inside its own function states the calls that function
//! is handed to.
//!
//! Every chain ends at one of a short list of kinds ([`ChainEnd`]), or at a
//! hole with a reason from a closed list ([`HoleReason`]). Nothing here names
//! what a package or a global is; the end says which one it is, and what it
//! means is the model's to say.
//!
//! The reading happens in the same walk as the send side's
//! ([`super::CallWalker`]) and is kept apart from it: every field it fills is
//! one the send side never reads. Nothing outside the tests and the offline
//! census reads the chains yet.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use swc_common::{Span, Spanned};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use super::library_sites::{LinkedNames, TextPiece, text_pieces};
use super::{
    CallIr, CallWalker, FileIr, FnIr, ImportBinding, Piece, Reader, Reassigned,
    RequestSummaryInputs, Scope, Site, Value, keyed_names, member_prop, prop_name,
};
use crate::binding_scope::{BindingKey, Declarations, ident_key};
use crate::env_alias::{env_get_call_name, env_member_name};
use crate::graphql_document_sites::unwrap_expression;
use crate::swc_scanner::is_producer_route_path;
use crate::type_manifest::is_http_method;
use crate::visitor::FunctionDefinition;
use crate::wrapper_request_shape::literal_string;

/// How many caller paths one guard is followed down before the rest are
/// counted as one [`HoleReason::FanOutCap`] chain.
const MAX_CHAINS_PER_GUARD: usize = 16;

/// How deep into an argument's objects and arrays a function written there
/// is placed ([`WalkState::place_functions_in`]).
const MAX_PLACEMENT_DEPTH: usize = 6;

/// The String methods that take no argument and hand back the text changed:
/// a comparison of what they return compares the text they were called on.
const STRING_METHODS: &[&str] = &[
    "toLowerCase",
    "toUpperCase",
    "toLocaleLowerCase",
    "toLocaleUpperCase",
    "trim",
    "trimStart",
    "trimEnd",
    "normalize",
    "toString",
    "valueOf",
];

/// Why a chain stops before it reaches an end. A closed list: every chain
/// that does not end at a kind of [`ChainEnd`] says which of these stopped
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HoleReason {
    /// The binding is assigned again, or declared more than once.
    Reassigned,
    /// An expression this pass does not read: a computed member with a
    /// non-literal key, a call result other than a String method, `this.x`,
    /// a spread, `await`.
    UnreadExpression,
    /// A value imported from a module of this service.
    InRepoValueImport,
    /// A value imported through a specifier no module the scan reads
    /// resolves: an alias no config maps, a missing target, an undeclared
    /// package ([`crate::call_graph::ImportedBinding::Unresolved`]). It may
    /// be a module of this service or a package; which is not known.
    UnresolvedImport,
    /// The function is handed to a function this repository defines.
    CallbackIntoOwnFunction,
    /// The function is handed to a call whose callee is none of a
    /// parameter, an import or a global.
    CalleeUnread,
    /// A call that names an operation calls a function this repository
    /// defines: what it does with the names is that function's.
    OwnFunction,
    /// A named function nothing calls, hands on or exports.
    NoCallerNotExported,
    /// A function written in a body that is held in a local, returned or
    /// assigned rather than handed to a call.
    DetachedUnplaced,
    /// A caller written in a file this service does not read.
    OutsideService,
    /// The caller passes nothing this pass reads in the position.
    CallerArgMissing,
    /// More caller paths than [`MAX_CHAINS_PER_GUARD`].
    FanOutCap,
    /// The chain reaches a function it already passed through.
    Recursion,
    /// No compiler row answers the position: the service has no rows file
    /// (its walk failed, died or timed out), the file is not in it, or it
    /// holds no row of the kind there (carrick#2413).
    RowsMissing,
    /// The file's bytes hash otherwise than the ones its rows were read
    /// from.
    RowsStale,
    /// The walk read no rows for the file: no program holds it, or it could
    /// not be read.
    NotInProgram,
    /// The type checker threw at the position.
    CheckerThrew,
    /// The compiler names no declaration for a name that is not an import,
    /// or types it as the error type.
    UnresolvedName,
    /// A member or a call whose receiver the compiler types as `any`, with
    /// no declaration.
    UntypedReceiver,
    /// A value that reads as more than one written path: a builder whose
    /// returns, or a conditional whose branches, write different path text
    /// (carrick#2541). No one path is its, so a URL holding it states no
    /// row. It serialises only into guard chain ends and never into a row.
    SeveralPaths,
}

/// Where a compared value comes from, before any access is applied to it.
/// The send side's opaque pieces end at one too ([`super::Piece::Opaque`],
/// carrick#2406). `pub`, not `pub(crate)`, because [`super::Piece`] is public.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Root {
    /// The function's own parameter, by position and name.
    Param(usize, String),
    /// A parameter of an enclosing function, `depth` functions out.
    Outer(u8, usize, String),
    /// An environment read: the text as written (`process.env.BASE`), the
    /// variable it names ([`crate::env_alias::env_read_name`]), and the
    /// `??` / `||` literal written beside it, where the send side read one.
    Env {
        written: String,
        name: String,
        fallback: Option<String>,
    },
    /// A name the file neither declares nor imports.
    Global(String),
    /// An import binding: its local name, the specifier as written and the
    /// export it names (`*` for a namespace import).
    Import {
        local: String,
        specifier: String,
        export: String,
    },
    /// Text the source writes.
    Literal(String),
    Hole(HoleReason),
}

/// One access from a [`Root`] down to the value compared.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Step {
    /// `.url`, `.pathname`, a destructured key, or a computed literal key.
    Key(String),
    /// `new URL(x)` or `new URL(x, base)`, read as the URL `x` states.
    Url,
    /// A String method called with no argument (`.toLowerCase()`).
    Call(String),
    /// `new <Global>(x, …)` for any other global constructor, read through
    /// its first argument.
    New(String),
}

impl Step {
    pub(super) fn render(&self) -> String {
        match self {
            Self::Key(key) => key.clone(),
            Self::Url => "URL".to_string(),
            Self::Call(method) => format!("{method}()"),
            Self::New(name) => format!("new {name}"),
        }
    }
}

/// A compared value: where it comes from, and the accesses down from there.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Subject {
    root: Root,
    steps: Vec<Step>,
    /// For a hole, the source text it stopped at (`this.client`,
    /// `parse(raw)`), when one expression is where it stopped.
    text: Option<String>,
}

impl Subject {
    fn of(root: Root) -> Self {
        Self {
            root,
            steps: Vec::new(),
            text: None,
        }
    }

    fn hole(reason: HoleReason) -> Self {
        Self::of(Root::Hole(reason))
    }

    /// A hole that names no text yet stopped at `expr`: the innermost
    /// expression read names it first.
    fn stopped_at(mut self, text: impl FnOnce() -> String) -> Self {
        if matches!(self.root, Root::Hole(_)) && self.text.is_none() {
            self.text = Some(text());
        }
        self
    }

    /// One more access. A hole stays the hole it is.
    fn then(mut self, step: Step) -> Self {
        if !matches!(self.root, Root::Hole(_)) {
            self.steps.push(step);
        }
        self
    }

    /// The subject as a function written inside this one reads it: each
    /// parameter is an enclosing one, one function further out.
    pub(super) fn enclosed(&self) -> Self {
        let root = match &self.root {
            Root::Param(index, name) => Root::Outer(1, *index, name.clone()),
            Root::Outer(depth, index, name) => {
                Root::Outer(depth.saturating_add(1), *index, name.clone())
            }
            other => other.clone(),
        };
        Self {
            root,
            steps: self.steps.clone(),
            text: self.text.clone(),
        }
    }

    /// `self` with a callee's subject's accesses after it: what the callee
    /// compares, read from what this call hands it.
    fn joined(&self, steps: &[Step]) -> Self {
        let mut joined = self.clone();
        for step in steps {
            joined = joined.then(step.clone());
        }
        joined
    }

    fn rendered_steps(&self) -> Vec<String> {
        self.steps.iter().map(Step::render).collect()
    }

    /// The parameter the value is, as how many functions out (0: the
    /// reader's own), its position and the keys read down from it, when it
    /// is one read through keys alone (carrick#2432).
    pub(super) fn parameter(&self) -> Option<(u8, usize, Vec<String>)> {
        let (depth, index) = match &self.root {
            Root::Param(index, _) => (0, *index),
            Root::Outer(depth, index, _) => (*depth, *index),
            _ => return None,
        };
        let keys = self
            .steps
            .iter()
            .map(|step| match step {
                Step::Key(key) => Some(key.clone()),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        Some((depth, index, keys))
    }

    /// The root the value comes from, as one opaque piece ends at it
    /// (carrick#2406). A literal with an access after it is no literal the
    /// piece holds, so it ends where reading stopped.
    fn end(self) -> Root {
        match self.root {
            Root::Literal(_) if !self.steps.is_empty() => Root::Hole(HoleReason::UnreadExpression),
            root => root,
        }
    }
}

/// Where a guard's literal stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardPosition {
    /// An operand of `===`, `==`, `!==` or `!=`.
    Equality,
    /// The test of a `switch` `case`, compared with the discriminant.
    Case,
    /// The first argument of `.startsWith(`.
    StartsWith,
    /// A key of an object-literal table the value indexes (`ROUTES[path]`).
    TableKey,
}

/// What a guard guards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GuardBranch {
    /// The `if` consequent, the `case` statements, the ternary consequent or
    /// the right side of `&&`, as a span in the discovery source map.
    Span(u32, u32),
    /// The table's value at the key.
    Value { text: String, line: u32 },
    /// The guard is returned: in a predicate, its branch is its caller's.
    Returned,
    /// Nothing the guard decides is read.
    None,
}

/// What a guard's literal side states: text, or a constant the value flow
/// reads once every module's uses of it are known (carrick#1562).
#[derive(Debug, Clone)]
pub(super) enum GuardLiteral {
    Text(String),
    Named {
        pieces: Vec<TextPiece>,
        /// The source text, kept when the constant does not read as text.
        written: String,
        constant: Option<Constant>,
    },
}

/// The import a constant a guard compares with is read through, when it
/// does not read as text: the specifier as written, the export it names,
/// and the entries walked into it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Constant {
    pub specifier: String,
    pub export: String,
    pub path: Vec<String>,
}

/// What a guard's literal is: a route path, any other name, or a constant
/// this pass cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardShape {
    /// The text passes [`is_producer_route_path`].
    Path,
    Name,
    Unresolved,
}

/// The shape of text a guard or a call states.
fn shape_of(text: &str) -> GuardShape {
    if is_producer_route_path(text) {
        GuardShape::Path
    } else {
        GuardShape::Name
    }
}

/// One literal a function compares a value against.
#[derive(Debug, Clone)]
pub(super) struct GuardIr {
    pub(super) literal: GuardLiteral,
    pub(super) position: GuardPosition,
    /// Written as `!==` or `!=`, or under `!`.
    pub(super) negated: bool,
    /// The comparison: the binary expression, the `case`, the `startsWith`
    /// call or the indexing member.
    pub(super) site: Site,
    subject: Subject,
    pub(super) subject_text: String,
    /// The other operands of the `&&` test it stands in, as text and line,
    /// unfiltered: a method test rides along without this pass naming it.
    conjoined: Vec<(String, u32)>,
    pub(super) branch: GuardBranch,
    /// The comparison is one of its test's top-level `&&` operands (not
    /// under `||` or `!`): what pairs a method with a path (carrick#2474
    /// R4a). Never serialised.
    pub(super) conjunct: bool,
    /// For a table key, the key as written: where its route stands, on a
    /// span of its own (two keys of one table are two places; carrick#2474
    /// R4a, R4b). Never serialised.
    pub(super) literal_at: Option<Site>,
}

/// A call an `if`, a ternary or a `&&` tests: the branch its result guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Tested {
    branch: (u32, u32),
    /// The test reads the call's result under an odd number of `!`.
    negated: bool,
    /// The call is one of the test's top-level `&&` operands (carrick#2474
    /// R4a).
    conjunct: bool,
}

/// Where a function written in a call's argument sits: the call, the
/// argument, and the keys from the argument down to the function (empty for
/// the argument itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WrittenIn {
    pub(super) site: Site,
    callee: String,
    pub(super) arg: usize,
    pub(super) path: Vec<String>,
    /// The call is a construction (`new X(…)`), kept in
    /// [`FnIr::constructions`].
    constructed: bool,
}

impl WrittenIn {
    pub(super) fn argument(site: Site, callee: &str, arg: usize) -> Self {
        Self {
            site,
            callee: callee.to_string(),
            arg,
            path: Vec::new(),
            constructed: false,
        }
    }
}

/// The text one argument of a call states, or one field of an object-literal
/// argument does, as the value flow reads names ([`text_pieces`]), with no
/// parameter in it: what a call that names an operation names. Read once
/// every module's uses are known ([`LinkedNames`]).
#[derive(Debug, Clone)]
pub(super) struct ArgName {
    arg: usize,
    field: Option<String>,
    pieces: Vec<TextPiece>,
    /// The source text, kept when a piece is a constant, for one that does
    /// not read as text.
    written: String,
    constant: Option<Constant>,
}

/// One entry of an object-literal table a module declares.
#[derive(Debug, Clone)]
struct TableEntry {
    key: String,
    /// The key as written: where its route stands.
    key_site: Site,
    value_text: String,
    value_line: u32,
}

/// What a guard reads a name through at module scope.
#[derive(Debug, Default)]
pub(super) struct ModuleGuards {
    /// Each import binding: its specifier as written and the export it
    /// names (`*` for a namespace import).
    imports: HashMap<String, (String, String)>,
    /// Each module-scope `const` object literal, by the name declared once
    /// in the file, with its entries.
    tables: HashMap<String, Vec<TableEntry>>,
    /// Every declaration in the file, imports included: a name not here is
    /// a global.
    declared: Declarations,
    /// The extractor's definition keys for the file: a name a call hands on
    /// that is one of them is a function the module declares.
    definition_keys: HashSet<String>,
}

impl ModuleGuards {
    pub(super) fn of(
        module: &Module,
        imports: &HashMap<String, ImportBinding>,
        definition_keys: &HashSet<String>,
        reader: &Reader<'_>,
    ) -> Self {
        let declared = Declarations::of(module);
        let mut tables = HashMap::new();
        for item in &module.body {
            let Some(var) = super::module_var_decl(item) else {
                continue;
            };
            if var.kind != VarDeclKind::Const {
                continue;
            }
            for declarator in &var.decls {
                let (Pat::Ident(ident), Some(init)) = (&declarator.name, &declarator.init) else {
                    continue;
                };
                let name = ident.id.sym.to_string();
                let Expr::Object(object) = unwrap_expression(init) else {
                    continue;
                };
                if declared.name_count(&name) != 1 {
                    continue;
                }
                let entries = object
                    .props
                    .iter()
                    .filter_map(|prop| match prop {
                        PropOrSpread::Prop(prop) => match &**prop {
                            Prop::KeyValue(kv) => Some(TableEntry {
                                key: prop_name(&kv.key)?,
                                key_site: reader.site(kv.key.span()),
                                value_text: reader.text(kv.value.span()),
                                value_line: reader.line(kv.value.span()),
                            }),
                            _ => None,
                        },
                        PropOrSpread::Spread(_) => None,
                    })
                    .collect();
                tables.insert(name, entries);
            }
        }
        Self {
            imports: imports
                .iter()
                .map(|(local, binding)| {
                    let export = if binding.namespace {
                        "*".to_string()
                    } else {
                        binding.export.clone()
                    };
                    (local.clone(), (binding.specifier.clone(), export))
                })
                .collect(),
            tables,
            declared,
            definition_keys: definition_keys.clone(),
        }
    }
}

/// Whether `expr` is a test: what a predicate returns.
pub(super) fn is_test(expr: &Expr) -> bool {
    match unwrap_expression(expr) {
        Expr::Bin(bin) => matches!(
            bin.op,
            BinaryOp::EqEq
                | BinaryOp::NotEq
                | BinaryOp::EqEqEq
                | BinaryOp::NotEqEq
                | BinaryOp::LogicalAnd
                | BinaryOp::LogicalOr
        ),
        Expr::Unary(unary) => unary.op == UnaryOp::Bang,
        Expr::Call(_) | Expr::Lit(Lit::Bool(_)) => true,
        _ => false,
    }
}

/// The test an `if`, a ternary or a `&&` is reading.
#[derive(Debug)]
pub(super) struct TestScope {
    branch: (u32, u32),
    /// Each operand of the test's top-level `&&` chain: span, text, line.
    conjuncts: Vec<(u32, u32, String, u32)>,
    /// Each call whose result the test reads directly (through `!`, `&&`,
    /// `||` and parentheses only), with whether it is negated.
    calls: HashMap<(u32, u32), bool>,
}

impl TestScope {
    /// Whether `span` is exactly one of the test's top-level `&&` operands.
    fn is_conjunct(&self, span: Span) -> bool {
        self.conjuncts
            .iter()
            .any(|(lo, hi, _, _)| *lo == span.lo.0 && *hi == span.hi.0)
    }

    fn of(test: &Expr, branch: Span, reader: &Reader<'_>) -> Self {
        let mut conjuncts = Vec::new();
        conjuncts_of(test, reader, &mut conjuncts);
        let mut calls = HashMap::new();
        tested_calls(test, false, &mut calls);
        Self {
            branch: (branch.lo.0, branch.hi.0),
            conjuncts,
            calls,
        }
    }
}

fn conjuncts_of(expr: &Expr, reader: &Reader<'_>, out: &mut Vec<(u32, u32, String, u32)>) {
    match unwrap_expression(expr) {
        Expr::Bin(bin) if bin.op == BinaryOp::LogicalAnd => {
            conjuncts_of(&bin.left, reader, out);
            conjuncts_of(&bin.right, reader, out);
        }
        other => {
            let span = other.span();
            out.push((span.lo.0, span.hi.0, reader.text(span), reader.line(span)));
        }
    }
}

fn tested_calls(expr: &Expr, negated: bool, out: &mut HashMap<(u32, u32), bool>) {
    match unwrap_expression(expr) {
        Expr::Bin(bin) if matches!(bin.op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) => {
            tested_calls(&bin.left, negated, out);
            tested_calls(&bin.right, negated, out);
        }
        Expr::Unary(unary) if unary.op == UnaryOp::Bang => tested_calls(&unary.arg, !negated, out),
        Expr::Call(call) => {
            out.insert((call.span.lo.0, call.span.hi.0), negated);
        }
        _ => {}
    }
}

/// The guard state of one [`CallWalker`] walk.
#[derive(Debug, Default)]
pub(super) struct WalkState {
    test: Option<TestScope>,
    /// Under an odd number of `!`.
    pub(super) negated: bool,
    /// Inside a `return`, or an arrow's expression body.
    pub(super) in_return: bool,
    /// Each function written inside a call's argument, by where its node
    /// starts.
    placed: HashMap<u32, WrittenIn>,
    /// The bindings declared in the blocks the walk has read: a body's own
    /// statements are bound into its scope instead ([`Reader::bind_subjects`]).
    pub(super) bound: Bound,
}

impl WalkState {
    pub(super) fn returned() -> Self {
        Self {
            in_return: true,
            ..Self::default()
        }
    }

    pub(super) fn testing(&self) -> bool {
        self.test.is_some()
    }

    /// Start reading `test`, whose branch is `branch`; hands back the test
    /// read before, for [`Self::leave_test`].
    pub(super) fn enter_test(
        &mut self,
        test: &Expr,
        branch: Span,
        reader: &Reader<'_>,
    ) -> Option<TestScope> {
        self.test.replace(TestScope::of(test, branch, reader))
    }

    pub(super) fn leave_test(&mut self, previous: Option<TestScope>) {
        self.test = previous;
    }

    /// The branch a call written at `span` guards, when the test reads its
    /// result directly.
    pub(super) fn tested(&self, span: Span) -> Option<Tested> {
        let test = self.test.as_ref()?;
        let negated = test.calls.get(&(span.lo.0, span.hi.0))?;
        Some(Tested {
            branch: test.branch,
            negated: *negated,
            conjunct: test.is_conjunct(span),
        })
    }

    /// Record where each function written inside `expr`, an argument, sits:
    /// the argument itself, or a value inside the objects and arrays it
    /// writes.
    pub(super) fn place_functions_in(&mut self, expr: &Expr, written_in: &WrittenIn) {
        let start = match unwrap_expression(expr) {
            Expr::Arrow(arrow) => arrow.span.lo.0,
            Expr::Fn(function) => function.function.span.lo.0,
            _ => return place(expr, written_in, &mut Vec::new(), &mut self.placed, 0),
        };
        self.placed.insert(start, written_in.clone());
    }

    /// Where the function whose node is `span` sits in an argument, if it
    /// sits in one.
    pub(super) fn placed(&mut self, span: Span) -> Option<WrittenIn> {
        self.placed.remove(&span.lo.0)
    }
}

fn place(
    expr: &Expr,
    written_in: &WrittenIn,
    path: &mut Vec<String>,
    out: &mut HashMap<u32, WrittenIn>,
    depth: usize,
) {
    if depth > MAX_PLACEMENT_DEPTH {
        return;
    }
    let at = |start: u32, path: &[String], out: &mut HashMap<u32, WrittenIn>| {
        out.insert(
            start,
            WrittenIn {
                path: path.to_vec(),
                ..written_in.clone()
            },
        );
    };
    let value = |expr: &Expr, path: &mut Vec<String>, out: &mut HashMap<u32, WrittenIn>| {
        match unwrap_expression(expr) {
            Expr::Arrow(arrow) => at(arrow.span.lo.0, path, out),
            Expr::Fn(function) => at(function.function.span.lo.0, path, out),
            other @ (Expr::Object(_) | Expr::Array(_)) => {
                place(other, written_in, path, out, depth + 1)
            }
            _ => {}
        }
    };
    match unwrap_expression(expr) {
        Expr::Object(object) => {
            for prop in &object.props {
                let PropOrSpread::Prop(prop) = prop else {
                    continue;
                };
                match &**prop {
                    Prop::KeyValue(kv) => {
                        if let Some(key) = prop_name(&kv.key) {
                            path.push(key);
                            value(&kv.value, path, out);
                            path.pop();
                        }
                    }
                    Prop::Method(method) => {
                        if let Some(key) = prop_name(&method.key) {
                            path.push(key);
                            at(method.function.span.lo.0, path, out);
                            path.pop();
                        }
                    }
                    _ => {}
                }
            }
        }
        Expr::Array(array) => {
            for (index, element) in array.elems.iter().enumerate() {
                if let Some(element) = element
                    && element.spread.is_none()
                {
                    path.push(index.to_string());
                    value(&element.expr, path, out);
                    path.pop();
                }
            }
        }
        _ => {}
    }
}

impl Reader<'_> {
    /// Where the value of `expr` comes from, as far as the source states it.
    /// `bound` holds the bindings the walk declared in the blocks it is in
    /// ([`CallWalker::bind_block`]).
    pub(super) fn subject(&self, expr: &Expr, scope: &Scope<'_>, bound: &Bound) -> Subject {
        self.subject_of(expr, scope, bound)
            .stopped_at(|| self.text(expr.span()))
    }

    fn subject_of(&self, expr: &Expr, scope: &Scope<'_>, bound: &Bound) -> Subject {
        match unwrap_expression(expr) {
            expr @ (Expr::Lit(Lit::Str(_)) | Expr::Tpl(_)) => match literal_string(expr) {
                Some(text) => Subject::of(Root::Literal(text)),
                None => Subject::hole(HoleReason::UnreadExpression),
            },
            Expr::Ident(ident) => self.ident_subject(ident, scope, bound),
            Expr::MetaProp(_) => Subject::of(Root::Global("import.meta".to_string())),
            Expr::Member(member) => self.member_subject(member, scope, bound),
            Expr::OptChain(chain) => match &*chain.base {
                OptChainBase::Member(member) => self.member_subject(member, scope, bound),
                OptChainBase::Call(_) => Subject::hole(HoleReason::UnreadExpression),
            },
            Expr::New(new) => {
                let Expr::Ident(callee) = &*new.callee else {
                    return Subject::hole(HoleReason::UnreadExpression);
                };
                let args = new.args.as_deref().unwrap_or_default();
                let Some(first) = args.first() else {
                    return Subject::hole(HoleReason::UnreadExpression);
                };
                if first.spread.is_some()
                    || !matches!(
                        self.ident_subject(callee, scope, bound).root,
                        Root::Global(_)
                    )
                {
                    return Subject::hole(HoleReason::UnreadExpression);
                }
                let name = callee.sym.to_string();
                if name != "URL" {
                    return self
                        .subject(&first.expr, scope, bound)
                        .then(Step::New(name));
                }
                // `new URL(".", base)`: a relative literal names a path only
                // the base decides, so no path is stated.
                if args.len() > 1
                    && let Some(text) = literal_string(unwrap_expression(&first.expr))
                    && !text.starts_with('/')
                    && !text.contains("://")
                {
                    return Subject::hole(HoleReason::UnreadExpression);
                }
                self.subject(&first.expr, scope, bound).then(Step::Url)
            }
            Expr::Call(call) => {
                let Callee::Expr(callee) = &call.callee else {
                    return Subject::hole(HoleReason::UnreadExpression);
                };
                if let Expr::Member(member) = unwrap_expression(callee)
                    && call.args.is_empty()
                    && let Some(method) = member_prop(member)
                    && STRING_METHODS.contains(&method.as_str())
                {
                    return self
                        .subject(&member.obj, scope, bound)
                        .then(Step::Call(method));
                }
                if let Some(name) = env_get_call_name(call) {
                    return Subject::of(Root::Env {
                        written: self.text(call.span),
                        name,
                        fallback: None,
                    });
                }
                Subject::hole(HoleReason::UnreadExpression)
            }
            Expr::Bin(bin)
                if matches!(bin.op, BinaryOp::NullishCoalescing | BinaryOp::LogicalOr) =>
            {
                self.subject(&bin.left, scope, bound)
            }
            _ => Subject::hole(HoleReason::UnreadExpression),
        }
    }

    fn member_subject(&self, member: &MemberExpr, scope: &Scope<'_>, bound: &Bound) -> Subject {
        // `this.<field>` is where reading stops, not `this`.
        if matches!(unwrap_expression(&member.obj), Expr::This(_)) {
            return Subject::hole(HoleReason::UnreadExpression)
                .stopped_at(|| self.text(member.span));
        }
        // An environment read is decided by its spelling, as every other
        // reader of one decides it ([`crate::env_alias::env_read_name`]),
        // the send side included: `process` bound by an import of the
        // runtime module or by the file is read the same (carrick#2406).
        if let Some(name) = env_member_name(member) {
            return Subject::of(Root::Env {
                written: self.text(member.span),
                name,
                fallback: None,
            });
        }
        let base = self.subject(&member.obj, scope, bound);
        match &member.prop {
            MemberProp::Ident(prop) => base.then(Step::Key(prop.sym.to_string())),
            MemberProp::Computed(computed) => match literal_string(&computed.expr) {
                Some(key) => base.then(Step::Key(key)),
                None => Subject::hole(HoleReason::UnreadExpression),
            },
            MemberProp::PrivateName(_) => Subject::hole(HoleReason::UnreadExpression),
        }
    }

    fn ident_subject(&self, ident: &Ident, scope: &Scope<'_>, bound: &Bound) -> Subject {
        let name = ident.sym.to_string();
        let key = ident_key(ident);
        if let Some(index) = scope.param_index(ident) {
            if scope.reassigned.contains(&name) || scope.written_through.contains(&name) {
                return Subject::hole(HoleReason::Reassigned);
            }
            return Subject::of(Root::Param(index, name));
        }
        if let Some((index, param_key)) = scope.keyed.get(&key) {
            return Subject::of(Root::Param(*index, String::new()))
                .then(Step::Key(param_key.clone()));
        }
        if let Some(subject) = bound.get(&key).or_else(|| scope.subjects.get(&key)) {
            return subject.clone();
        }
        // A binding of this function the walk read no declaration of, set
        // somewhere after it is declared.
        if scope.reassigned.contains(&name) {
            return Subject::hole(HoleReason::Reassigned);
        }
        if scope.locals.contains_key(&key) {
            return Subject::hole(HoleReason::UnreadExpression);
        }
        if let Some(Value::Str(pieces)) = scope.module.consts.get(&key)
            && let Some(text) = literal_text(pieces)
        {
            return Subject::of(Root::Literal(text));
        }
        let guards = &scope.module.guards;
        if let Some((specifier, export)) = guards.imports.get(&name)
            && !scope.module.declared_below.declares_name(&name)
        {
            return Subject::of(Root::Import {
                local: name,
                specifier: specifier.clone(),
                export: export.clone(),
            });
        }
        if guards.declared.declares_name(&name) {
            return Subject::hole(HoleReason::UnreadExpression);
        }
        // FROZEN (old path, 2026-10-21): this fallback only (a name the file
        // neither declares nor imports is read as a global, which a typo or an
        // ambient name also lands on). Deletions and safety fixes only. Fixes
        // for wrong or missing rows go in the compiler's position rows (a
        // default-library or ambient declaration, carrick#2413 slice C).
        // Delete at carrick#2413 slice F, once the unresolved-import (#2573)
        // and rows-join budget (#2574) fallbacks count zero (carrick#2413).
        Subject::of(Root::Global(name))
    }

    /// Where a name the send side keeps opaque comes from (carrick#2406):
    /// the root this reader gives it.
    pub(super) fn ident_end(&self, ident: &Ident, scope: &Scope<'_>) -> Root {
        self.ident_subject(ident, scope, &Bound::default()).end()
    }

    /// Where a member the send side keeps opaque comes from (carrick#2406),
    /// an environment read included.
    pub(super) fn member_end(&self, member: &MemberExpr, scope: &Scope<'_>) -> Root {
        self.member_subject(member, scope, &Bound::default()).end()
    }

    /// Each argument's subject, `None` for a function written in place and
    /// for every position from a spread on.
    pub(super) fn arg_subjects(
        &self,
        args: &[ExprOrSpread],
        scope: &Scope<'_>,
        bound: &Bound,
    ) -> Vec<Option<Subject>> {
        let mut spread = false;
        args.iter()
            .map(|arg| {
                spread |= arg.spread.is_some();
                match &*arg.expr {
                    _ if spread => None,
                    Expr::Arrow(_) | Expr::Fn(_) => None,
                    expr => Some(self.subject(expr, scope, bound)),
                }
            })
            .collect()
    }

    /// Each name of a function the module declares that an argument hands
    /// on, before any spread: the argument itself, or a value inside an
    /// object or array it writes (`{ skip: skipHealth }`), with the keys
    /// down to it.
    pub(super) fn handed(
        &self,
        args: &[ExprOrSpread],
        scope: &Scope<'_>,
        bound: &Bound,
    ) -> Vec<(usize, Vec<String>, String)> {
        let mut handed = Vec::new();
        for (index, arg) in args.iter().enumerate() {
            if arg.spread.is_some() {
                break;
            }
            let mut found = Vec::new();
            named_functions(&arg.expr, &mut Vec::new(), &mut found, 0);
            for (path, ident) in found {
                let name = ident.sym.to_string();
                let key = ident_key(ident);
                // A binding that holds a value (a subject) is not the
                // function of that name; one that holds a function is.
                if scope.module.guards.definition_keys.contains(&name)
                    && scope.param_index(ident).is_none()
                    && !scope.subjects.contains_key(&key)
                    && !bound.contains_key(&key)
                {
                    handed.push((index, path, name));
                }
            }
        }
        handed
    }

    /// The text each argument before any spread states, and each field of an
    /// object-literal argument, as the value flow reads names: a literal, a
    /// constant, or text built from them, never a parameter (what a caller
    /// decides). A function written in place states none.
    pub(super) fn arg_names(&self, args: &[ExprOrSpread], scope: &Scope<'_>) -> Vec<ArgName> {
        let mut names = Vec::new();
        for (index, arg) in args.iter().enumerate() {
            if arg.spread.is_some() {
                break;
            }
            match unwrap_expression(&arg.expr) {
                Expr::Arrow(_) | Expr::Fn(_) => {}
                Expr::Object(object) => {
                    for prop in &object.props {
                        let PropOrSpread::Prop(prop) = prop else {
                            continue;
                        };
                        match &**prop {
                            Prop::KeyValue(kv) => {
                                if let Some(key) = prop_name(&kv.key) {
                                    names.extend(self.arg_name(index, Some(key), &kv.value, scope));
                                }
                            }
                            Prop::Shorthand(ident) => {
                                let value = Expr::Ident(ident.clone());
                                let key = Some(ident.sym.to_string());
                                names.extend(self.arg_name(index, key, &value, scope));
                            }
                            _ => {}
                        }
                    }
                }
                expr => names.extend(self.arg_name(index, None, expr, scope)),
            }
        }
        names
    }

    fn arg_name(
        &self,
        arg: usize,
        field: Option<String>,
        expr: &Expr,
        scope: &Scope<'_>,
    ) -> Option<ArgName> {
        let pieces = text_pieces(expr, scope)?;
        if names_a_param(&pieces) {
            return None;
        }
        let literal = pieces
            .iter()
            .all(|piece| matches!(piece, TextPiece::Lit(_)));
        Some(ArgName {
            arg,
            field,
            constant: constant_of(&pieces, scope),
            written: if literal {
                String::new()
            } else {
                self.text(expr.span())
            },
            pieces,
        })
    }

    /// Record where each binding `pat` declares from `init` comes from, by
    /// the rule `scope.locals` keeps: a binding the body declares once and
    /// never assigns again, nor writes a member of, holds what it was
    /// declared with. One declared with no value is set later, if at all.
    pub(super) fn bind_subjects(
        &self,
        pat: &Pat,
        init: Option<&Expr>,
        scope: &mut Scope<'_>,
        reassigned: &Reassigned,
        declared: &Declarations,
    ) {
        let settled = |key: &BindingKey| {
            !reassigned.names.contains(&key.0) && declared.binding_count(key) == 1
        };
        let bound = Bound::new();
        for (key, subject) in self.declared_subjects(pat, init, scope, &bound, settled) {
            scope.subjects.insert(key, subject);
        }
    }

    /// The subject of each binding `pat` declares from `init`, `settled`
    /// saying which bindings hold what they were declared with.
    fn declared_subjects(
        &self,
        pat: &Pat,
        init: Option<&Expr>,
        scope: &Scope<'_>,
        bound: &Bound,
        settled: impl Fn(&BindingKey) -> bool,
    ) -> Vec<(BindingKey, Subject)> {
        let settled = |key: &BindingKey| settled(key) && !scope.written_through.contains(&key.0);
        match (pat, init) {
            (Pat::Ident(ident), None) => {
                let key = ident_key(&ident.id);
                let reason = if scope.reassigned.contains(&key.0) {
                    HoleReason::Reassigned
                } else {
                    HoleReason::UnreadExpression
                };
                vec![(key, Subject::hole(reason))]
            }
            // A function is no compared value: the name hands it on
            // ([`Reader::handed`]).
            (Pat::Ident(_), Some(init))
                if matches!(unwrap_expression(init), Expr::Arrow(_) | Expr::Fn(_)) =>
            {
                Vec::new()
            }
            (Pat::Ident(ident), Some(init)) => {
                let key = ident_key(&ident.id);
                let subject = if settled(&key) {
                    self.subject(init, scope, bound)
                } else {
                    Subject::hole(HoleReason::Reassigned)
                };
                vec![(key, subject)]
            }
            (Pat::Object(object), Some(init)) => {
                let base = self.subject(init, scope, bound);
                keyed_names(object)
                    .into_iter()
                    .map(|(binding, key)| {
                        let subject = if settled(&binding) {
                            base.clone().then(Step::Key(key))
                        } else {
                            Subject::hole(HoleReason::Reassigned)
                        };
                        (binding, subject)
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }
}

/// Bindings declared in the blocks a walk is in, with their subjects.
pub(super) type Bound = HashMap<BindingKey, Subject>;

/// Each identifier `expr` hands on as a value, itself or inside the objects
/// and arrays it writes, with the keys down to it.
fn named_functions<'e>(
    expr: &'e Expr,
    path: &mut Vec<String>,
    out: &mut Vec<(Vec<String>, &'e Ident)>,
    depth: usize,
) {
    if depth > MAX_PLACEMENT_DEPTH {
        return;
    }
    match unwrap_expression(expr) {
        Expr::Ident(ident) => out.push((path.clone(), ident)),
        Expr::Object(object) => {
            for prop in &object.props {
                let PropOrSpread::Prop(prop) = prop else {
                    continue;
                };
                match &**prop {
                    Prop::Shorthand(ident) => {
                        path.push(ident.sym.to_string());
                        out.push((path.clone(), ident));
                        path.pop();
                    }
                    Prop::KeyValue(kv) => {
                        if let Some(key) = prop_name(&kv.key) {
                            path.push(key);
                            named_functions(&kv.value, path, out, depth + 1);
                            path.pop();
                        }
                    }
                    _ => {}
                }
            }
        }
        Expr::Array(array) => {
            for (index, element) in array.elems.iter().enumerate() {
                if let Some(element) = element
                    && element.spread.is_none()
                {
                    path.push(index.to_string());
                    named_functions(&element.expr, path, out, depth + 1);
                    path.pop();
                }
            }
        }
        _ => {}
    }
}

/// The names a function writes a member of, anywhere in it: `x.a = …`,
/// `x[k] = …`, `x.a.b += …`, `x.a++`, `delete x.a`.
#[derive(Default)]
pub(super) struct WrittenThrough {
    names: HashSet<String>,
}

impl WrittenThrough {
    pub(super) fn of<N: VisitWith<Self>>(node: &N) -> HashSet<String> {
        let mut written = Self::default();
        node.visit_with(&mut written);
        written.names
    }

    fn write(&mut self, member: &MemberExpr) {
        let mut object = &*member.obj;
        while let Expr::Member(inner) = unwrap_expression(object) {
            object = &inner.obj;
        }
        if let Expr::Ident(ident) = unwrap_expression(object) {
            self.names.insert(ident.sym.to_string());
        }
    }
}

impl Visit for WrittenThrough {
    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        if let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = &assign.left {
            self.write(member);
        }
        assign.visit_children_with(self);
    }

    fn visit_update_expr(&mut self, update: &UpdateExpr) {
        if let Expr::Member(member) = unwrap_expression(&update.arg) {
            self.write(member);
        }
        update.visit_children_with(self);
    }

    fn visit_unary_expr(&mut self, unary: &UnaryExpr) {
        if unary.op == UnaryOp::Delete
            && let Expr::Member(member) = unwrap_expression(&unary.arg)
        {
            self.write(member);
        }
        unary.visit_children_with(self);
    }
}

/// Whether a parameter is among `pieces`, or among the arguments a builder
/// in them is called with: a value the caller decides, not a constant.
fn names_a_param(pieces: &[TextPiece]) -> bool {
    pieces.iter().any(|piece| match piece {
        TextPiece::Lit(_) => false,
        TextPiece::Param(_) => true,
        TextPiece::Named(named) => named.args.iter().flatten().any(|arg| names_a_param(arg)),
    })
}

/// The import a constant is read through, when `pieces` is one named piece
/// the module imports.
fn constant_of(pieces: &[TextPiece], scope: &Scope<'_>) -> Option<Constant> {
    let [TextPiece::Named(named)] = pieces else {
        return None;
    };
    let (specifier, export) = scope.module.guards.imports.get(&named.binding)?;
    Some(Constant {
        specifier: specifier.clone(),
        export: export.clone(),
        path: named.path.clone(),
    })
}

/// The text of `pieces` when every piece is literal.
fn literal_text(pieces: &[Piece]) -> Option<String> {
    pieces
        .iter()
        .map(|piece| match piece {
            Piece::Lit(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.concat())
}

/// What a function written inside the one `scope` reads holds where each
/// value comes from: this body's own subjects (those of the blocks the walk
/// is in, `bound`, included), its parameters and the keys it reads of them,
/// each one function further out.
pub(super) fn captured_subjects(scope: &Scope<'_>, bound: &Bound) -> HashMap<BindingKey, Subject> {
    let mut subjects: HashMap<BindingKey, Subject> = scope
        .subjects
        .iter()
        .chain(bound)
        .map(|(key, subject)| (key.clone(), subject.enclosed()))
        .collect();
    for (index, key) in scope.params.iter().enumerate() {
        if let Some(key) = key {
            let subject =
                if scope.reassigned.contains(&key.0) || scope.written_through.contains(&key.0) {
                    Subject::hole(HoleReason::Reassigned)
                } else {
                    Subject::of(Root::Outer(1, index, key.0.clone()))
                };
            subjects.insert(key.clone(), subject);
        }
    }
    for (key, (index, param_key)) in &scope.keyed {
        subjects.insert(
            key.clone(),
            Subject::of(Root::Outer(1, *index, String::new())).then(Step::Key(param_key.clone())),
        );
    }
    subjects
}

impl CallWalker<'_, '_, '_> {
    /// A declaration inside a block of the body (`if (…) { const url = new
    /// URL(req.url); … }`), bound by the body's rule: settled when the
    /// function never assigns the name again nor writes a member of it.
    pub(super) fn bind_block(&mut self, declarator: &VarDeclarator) {
        let reassigned = &self.scope.reassigned;
        let subjects = self.reader.declared_subjects(
            &declarator.name,
            declarator.init.as_deref(),
            self.scope,
            &self.guard.bound,
            |key| !reassigned.contains(&key.0),
        );
        self.guard.bound.extend(subjects);
    }

    /// The text `expr` states: a string literal, a template with no
    /// interpolation, or a name the scope reads as one.
    fn plain_literal(&self, expr: &Expr) -> Option<String> {
        match literal_string(unwrap_expression(expr)) {
            Some(text) => Some(text),
            None => match unwrap_expression(expr) {
                Expr::Ident(_) => match self.reader.eval(expr, self.scope) {
                    Value::Str(pieces) => literal_text(&pieces),
                    _ => None,
                },
                _ => None,
            },
        }
    }

    /// The constant `expr` names, as the value flow reads names
    /// ([`text_pieces`]): an import, an entry of a constant object, a
    /// builder called with text, or text built from them, with no parameter
    /// anywhere in it.
    fn named_literal(&self, expr: &Expr) -> Option<GuardLiteral> {
        let pieces = text_pieces(expr, self.scope)?;
        if names_a_param(&pieces)
            || !pieces
                .iter()
                .any(|piece| matches!(piece, TextPiece::Named(_)))
        {
            return None;
        }
        Some(GuardLiteral::Named {
            constant: constant_of(&pieces, self.scope),
            pieces,
            written: self.reader.text(expr.span()),
        })
    }

    /// `new X(…)` of a constructor that is not the language's own: kept as a
    /// call is, apart from the calls, with each function written in its
    /// arguments placed in it.
    pub(super) fn record_construction(&mut self, new: &NewExpr) {
        let site = self.reader.site(new.span);
        let callee_text = self.reader.text(new.callee.span());
        let args = new.args.as_deref().unwrap_or_default();
        for (index, arg) in args.iter().enumerate() {
            let written_in = WrittenIn {
                constructed: true,
                ..WrittenIn::argument(site, &callee_text, index)
            };
            self.guard.place_functions_in(&arg.expr, &written_in);
        }
        let spread = args.iter().any(|arg| arg.spread.is_some());
        let callee = match unwrap_expression(&new.callee) {
            Expr::Ident(ident) => ident.sym.to_string(),
            Expr::Member(member) => member_prop(member).unwrap_or_default(),
            _ => String::new(),
        };
        let bound = &self.guard.bound;
        // What each argument states, for the fields a class's constructor
        // sets from its parameters (carrick#2452). A function written in
        // place is no text.
        let values = args
            .iter()
            .map(|arg| match unwrap_expression(&arg.expr) {
                Expr::Arrow(_) | Expr::Fn(_) => Value::Str(vec![Piece::Unknown]),
                expr => self.reader.eval(expr, self.scope),
            })
            .collect();
        let construction = CallIr {
            site,
            callee,
            args: values,
            request: None,
            waited: false,
            inert: false,
            invokes_param: None,
            receiver: None,
            site_args: (!spread).then(|| {
                args.iter()
                    .map(|arg| super::library_sites::site_arg(&arg.expr, self.scope))
                    .collect()
            }),
            spread_from: None,
            name_line: self.reader.line(new.callee.span()),
            jsx: false,
            arg_subjects: self.reader.arg_subjects(args, self.scope, bound),
            callee_subject: Some(self.reader.subject(&new.callee, self.scope, bound)),
            callee_text,
            callee_values: Vec::new(),
            arg_values: self.reader.arg_values(args, self.scope, bound),
            tested: None,
            handed: self.reader.handed(args, self.scope, bound),
            names: self.reader.arg_names(args, self.scope),
            spread_args: Vec::new(),
            receiver_spans: super::discovery::receiver_spans(&new.callee),
            receiver_object: super::discovery::receiver_object(&new.callee),
            instance: super::InstanceIr::default(),
        };
        self.ir.constructions.push(construction);
    }

    /// An optional call (`socket?.emit(…)`), read as a call is, for
    /// discovery and the rows' call targets alone (carrick#2431): its
    /// callee, where it is written and the objects its member chain reads.
    /// The summaries compose none of these.
    pub(super) fn record_optional_call(&mut self, call: &OptCall) {
        let callee = match unwrap_expression(&call.callee) {
            Expr::Ident(ident) => ident.sym.to_string(),
            Expr::Member(member) => member_prop(member).unwrap_or_default(),
            Expr::OptChain(OptChainExpr { base, .. }) => match &**base {
                OptChainBase::Member(member) => member_prop(member).unwrap_or_default(),
                OptChainBase::Call(_) => String::new(),
            },
            _ => String::new(),
        };
        self.ir.optional_calls.push(CallIr {
            site: self.reader.site(call.span),
            callee,
            args: Vec::new(),
            request: None,
            waited: false,
            inert: false,
            // `produce?.()`: the body invokes a parameter.
            invokes_param: match unwrap_expression(&call.callee) {
                Expr::Ident(ident) => self.scope.param_index(ident),
                _ => None,
            },
            receiver: None,
            site_args: None,
            spread_from: None,
            name_line: self.reader.line(call.callee.span()),
            jsx: false,
            arg_subjects: Vec::new(),
            callee_subject: None,
            callee_text: self.reader.text(call.callee.span()),
            callee_values: self
                .reader
                .callee_values(&call.callee, self.scope, &self.guard.bound),
            arg_values: self
                .reader
                .arg_values(&call.args, self.scope, &self.guard.bound),
            tested: None,
            handed: Vec::new(),
            names: Vec::new(),
            spread_args: Vec::new(),
            receiver_spans: super::discovery::receiver_spans(&call.callee),
            receiver_object: super::discovery::receiver_object(&call.callee),
            instance: self.reader.instance_ir(Some(&call.callee), &[], self.scope),
        });
    }

    /// The literal one operand states.
    fn guard_literal(&self, expr: &Expr) -> Option<GuardLiteral> {
        match self.plain_literal(expr) {
            Some(text) => Some(GuardLiteral::Text(text)),
            None => self.named_literal(expr),
        }
    }

    /// Which operand of a comparison is its literal, and the other, the
    /// value compared. Text wins over a constant; of two texts, the one
    /// route path does; two of a kind compare no value.
    fn comparison_sides<'e>(
        &self,
        left: &'e Expr,
        right: &'e Expr,
    ) -> Option<(GuardLiteral, &'e Expr)> {
        match (self.plain_literal(left), self.plain_literal(right)) {
            (Some(text), None) => Some((GuardLiteral::Text(text), right)),
            (None, Some(text)) => Some((GuardLiteral::Text(text), left)),
            (Some(l), Some(r)) => match (is_producer_route_path(&l), is_producer_route_path(&r)) {
                (true, false) => Some((GuardLiteral::Text(l), right)),
                (false, true) => Some((GuardLiteral::Text(r), left)),
                _ => None,
            },
            (None, None) => match (self.named_literal(left), self.named_literal(right)) {
                (Some(named), None) => Some((named, right)),
                (None, Some(named)) => Some((named, left)),
                _ => None,
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record_guard(
        &mut self,
        literal: GuardLiteral,
        position: GuardPosition,
        subject: &Expr,
        span: Span,
        negated: bool,
        branch: Option<GuardBranch>,
        literal_at: Option<Site>,
    ) {
        // `typeof x === "string"` compares a type tag the language names,
        // never a value the code was handed.
        if matches!(unwrap_expression(subject), Expr::Unary(unary) if unary.op == UnaryOp::TypeOf) {
            return;
        }
        let branch = branch.unwrap_or(match &self.guard.test {
            Some(test) => GuardBranch::Span(test.branch.0, test.branch.1),
            None if self.guard.in_return => GuardBranch::Returned,
            None => GuardBranch::None,
        });
        let conjoined = match &self.guard.test {
            Some(test) if test.conjuncts.len() > 1 => test
                .conjuncts
                .iter()
                .filter(|(lo, hi, _, _)| !(*lo <= span.lo.0 && span.hi.0 <= *hi))
                .map(|(_, _, text, line)| (text.clone(), *line))
                .collect(),
            _ => Vec::new(),
        };
        let guard = GuardIr {
            literal,
            position,
            negated: self.guard.negated ^ negated,
            site: self.reader.site(span),
            subject: self.reader.subject(subject, self.scope, &self.guard.bound),
            subject_text: self.reader.text(subject.span()),
            conjoined,
            branch,
            conjunct: self
                .guard
                .test
                .as_ref()
                .is_some_and(|test| test.is_conjunct(span)),
            literal_at,
        };
        self.ir.guards.push(guard);
    }

    /// `pathname === "/items"`, `msg.type === "READY"`: either operand a
    /// literal, the other the value compared.
    pub(super) fn read_comparison(&mut self, bin: &BinExpr) {
        let negated = match bin.op {
            BinaryOp::EqEq | BinaryOp::EqEqEq => false,
            BinaryOp::NotEq | BinaryOp::NotEqEq => true,
            _ => return,
        };
        let Some((literal, subject)) = self.comparison_sides(&bin.left, &bin.right) else {
            return;
        };
        self.record_guard(
            literal,
            GuardPosition::Equality,
            subject,
            bin.span,
            negated,
            None,
            None,
        );
    }

    /// `pathname.startsWith("/items/")`.
    pub(super) fn read_starts_with(&mut self, call: &CallExpr) {
        let Callee::Expr(callee) = &call.callee else {
            return;
        };
        let Expr::Member(member) = unwrap_expression(callee) else {
            return;
        };
        if member_prop(member).as_deref() != Some("startsWith") {
            return;
        }
        let Some(first) = call.args.first().filter(|arg| arg.spread.is_none()) else {
            return;
        };
        if let Some(literal) = self.guard_literal(&first.expr) {
            self.record_guard(
                literal,
                GuardPosition::StartsWith,
                &member.obj,
                call.span,
                false,
                None,
                None,
            );
        }
    }

    /// `case "/health":`, compared with the discriminant. An empty case
    /// guards the statements of the next case that has any.
    pub(super) fn read_switch(&mut self, switch: &SwitchStmt) {
        for (index, case) in switch.cases.iter().enumerate() {
            let Some(test) = &case.test else {
                continue;
            };
            let Some(literal) = self.guard_literal(test) else {
                continue;
            };
            let branch = switch.cases[index..]
                .iter()
                .find(|later| !later.cons.is_empty())
                .map_or(GuardBranch::None, |later| {
                    let first = later.cons.first().map(Spanned::span).unwrap_or_default();
                    let last = later.cons.last().map(Spanned::span).unwrap_or_default();
                    GuardBranch::Span(first.lo.0, last.hi.0)
                });
            self.record_guard(
                literal,
                GuardPosition::Case,
                &switch.discriminant,
                case.span,
                false,
                Some(branch),
                None,
            );
        }
    }

    /// `ROUTES[pathname]`, where `ROUTES` is a module-scope object literal:
    /// one guard per key.
    pub(super) fn read_table_key(&mut self, member: &MemberExpr) {
        let MemberProp::Computed(computed) = &member.prop else {
            return;
        };
        let Expr::Ident(table) = unwrap_expression(&member.obj) else {
            return;
        };
        let key = ident_key(table);
        if self.scope.param_index(table).is_some()
            || self.scope.locals.contains_key(&key)
            || self.scope.subjects.contains_key(&key)
            || self.guard.bound.contains_key(&key)
            || self
                .scope
                .module
                .declared_below
                .declares_name(table.sym.as_ref())
        {
            return;
        }
        let Some(entries) = self.scope.module.guards.tables.get(table.sym.as_ref()) else {
            return;
        };
        for entry in entries.clone() {
            self.record_guard(
                GuardLiteral::Text(entry.key),
                GuardPosition::TableKey,
                &computed.expr,
                member.span,
                false,
                Some(GuardBranch::Value {
                    text: entry.value_text,
                    line: entry.value_line,
                }),
                Some(entry.key_site),
            );
        }
    }
}

impl FileIr {
    /// Keep each definition the extractor keyed in this file, with where its
    /// function node starts and whether the module exports it: what names a
    /// detached function (an exported object's member) and what makes a
    /// function an entry point (carrick#2386).
    pub fn place_definitions(&mut self, definitions: &HashMap<String, FunctionDefinition>) {
        self.definition_lines = super::row_calls::definition_lines(
            definitions
                .iter()
                .map(|(key, definition)| (key.as_str(), definition)),
        );
        self.declaration_lines.place(
            definitions
                .iter()
                .map(|(key, definition)| (key.as_str(), definition)),
        );
        self.definitions = definitions
            .iter()
            .map(|(key, definition)| {
                let start = super::row_calls::node_start(definition);
                (key.clone(), (start, definition.is_exported))
            })
            .collect();
    }
}

// ---------------------------------------------------------------------------
// The chains.

/// Where a guard's comparison is written.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ChainSite {
    pub file: String,
    pub line: u32,
}

/// Another operand of the `&&` test a guard stands in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Conjoined {
    pub text: String,
    pub line: u32,
}

/// One step a chain takes out of the function it was in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Hop {
    /// Into a caller: the call, and the argument position read.
    Call {
        file: String,
        line: u32,
        callee: String,
        arg: usize,
    },
    /// Into the call the function is handed to: the callee as written, the
    /// argument it is passed in and the keys down to it, the parameter the
    /// chain was following, the accesses from that parameter, and the text
    /// of the call's other arguments.
    HandedTo {
        file: String,
        line: u32,
        callee: String,
        arg: usize,
        path: Vec<String>,
        param: usize,
        steps: Vec<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        args: Vec<HopArg>,
        /// The call is a construction (`new X(…)`).
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        constructed: bool,
    },
    /// Out to an enclosing function, whose parameter it is.
    Enclosed { file: String, line: u32 },
}

/// The text a call states at one of its arguments, read as the value flow
/// reads names ([`LinkedNames`]): `child.on("message", <fn>)` states
/// `"message"` at argument 0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HopArg {
    pub arg: usize,
    pub text: String,
}

/// The call a function is handed to, where a chain that stops inside that
/// function took no hop out of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Within {
    pub file: String,
    pub line: u32,
    pub callee: String,
    pub arg: usize,
    pub path: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<HopArg>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub constructed: bool,
}

/// Where a chain ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChainEnd {
    /// A function the module offers that nothing in the repository calls.
    EntryPoint {
        file: String,
        line: u32,
        function: String,
        param: usize,
        steps: Vec<String>,
    },
    Literal {
        text: String,
        steps: Vec<String>,
    },
    Env {
        name: String,
        steps: Vec<String>,
    },
    Global {
        name: String,
        steps: Vec<String>,
    },
    /// An import the call graph does not follow into the repository, named
    /// by its specifier whether or not a manifest declares it.
    ExternalImport {
        specifier: String,
        export: String,
        steps: Vec<String>,
        /// With rows, the package the compiler says the export is declared
        /// in (carrick#2413 slice E), which a type re-export or an alias
        /// can make another than the specifier's.
        #[serde(skip_serializing_if = "Option::is_none")]
        package: Option<String>,
    },
    Hole {
        reason: HoleReason,
        file: String,
        line: u32,
        /// The source text the chain stopped at, when one expression is
        /// where it stopped.
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
}

/// What the guarded branch does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Branch {
    /// A call written in the branch, with the definition the call graph
    /// resolves it to (`file:key`), where it resolves one.
    Call {
        callee: String,
        file: String,
        line: u32,
        target: Option<String>,
    },
    /// A table's value at the key.
    Value {
        text: String,
        file: String,
        line: u32,
    },
}

/// One literal's chain to one end: the object a later step hands the model
/// that classifies it, whichever model that is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GuardChain {
    /// The text compared with, or the source text of a constant this pass
    /// cannot read.
    pub literal: String,
    pub shape: GuardShape,
    /// The import an unread constant comes through.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constant: Option<Constant>,
    pub position: GuardPosition,
    pub negated: bool,
    pub site: ChainSite,
    pub subject: String,
    pub conjoined: Vec<Conjoined>,
    pub hops: Vec<Hop>,
    /// For a chain that stops at a hole with no hop, the calls the function
    /// it stops in is handed to.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub within: Vec<Within>,
    pub end: ChainEnd,
    pub branch: Vec<Branch>,
}

/// One function the chains move through.
pub(super) struct Node<'a> {
    pub(super) file: &'a Path,
    /// The extractor's key, when it keys the function.
    pub(super) key: Option<String>,
    exported: bool,
    pub(super) ir: &'a FnIr,
    /// The function this one is written in.
    pub(super) parent: Option<usize>,
}

/// A chain being followed.
#[derive(Clone)]
struct Trace {
    subject: Subject,
    hops: Vec<Hop>,
    /// `None` while a predicate's branch waits on its caller.
    branch: Option<Vec<Branch>>,
    negated: bool,
    /// The functions passed through.
    passed: Vec<usize>,
    /// What the chain's facts are, kept for the routes a dispatch states
    /// (carrick#2474 R4a) and never serialised.
    facts: ChainFacts,
}

/// What a chain passed on its way to its end, read off the trace itself
/// rather than the rendered chain (carrick#2474 R4a): the first call the
/// chain is handed to, the entry point it ends at, and the call that
/// decided a predicate's branch.
#[derive(Debug, Clone, Default)]
pub(super) struct ChainFacts {
    pub(super) handoff: Option<Handoff>,
    pub(super) entry: Option<EntryAt>,
    pub(super) decided_at: Option<DecidedAt>,
    /// The function the trace was last in: where a chain that ends at a
    /// hole stopped (carrick#2474 R4b), which places the hole in a request
    /// handler or in a function written inside one.
    pub(super) stopped: Option<usize>,
}

/// The first call a chain's function is handed to.
#[derive(Debug, Clone)]
pub(super) struct Handoff {
    /// The function handed over.
    pub(super) handed: usize,
    /// The function the call is written in.
    pub(super) caller: usize,
    /// The call.
    pub(super) site: Site,
    /// The argument the function is passed in, and the keys down to it.
    pub(super) arg: usize,
    pub(super) path: Vec<String>,
    /// The parameter the chain was following, and the accesses from it.
    pub(super) param: usize,
    pub(super) steps: Vec<Step>,
}

/// The function a chain ends at as an entry point.
#[derive(Debug, Clone)]
pub(super) struct EntryAt {
    pub(super) node: usize,
    pub(super) param: usize,
    pub(super) steps: Vec<Step>,
}

/// The caller's call that decided a predicate's branch: the function it is
/// written in, its span (discovery numbering), the branch its result guards
/// and whether it is a top-level `&&` operand of its test.
#[derive(Debug, Clone, Copy)]
pub(super) struct DecidedAt {
    pub(super) node: usize,
    pub(super) span: (u32, u32),
    pub(super) branch: (u32, u32),
    pub(super) conjunct: bool,
}

pub(super) struct Chains<'a> {
    inputs: &'a RequestSummaryInputs,
    root: &'a Path,
    /// What each constant a guard or a call argument names reads as, with
    /// every module's uses of it known.
    names: LinkedNames<'a>,
    pub(super) nodes: Vec<Node<'a>>,
    /// (file, span start, span end) of each call -> (node, call index).
    calls: HashMap<(&'a Path, u32, u32), (usize, usize)>,
    /// The same for each construction.
    constructions: HashMap<(&'a Path, u32, u32), (usize, usize)>,
    /// Each definition's in-repo callers: (file, span start, span end).
    callers: HashMap<(PathBuf, String), Vec<CallerSite>>,
    /// (file, function name) -> each call that hands it on.
    handed: HashMap<(&'a Path, String), Vec<HandedAt>>,
}

/// A call site that reaches a definition: its file, span start and end.
type CallerSite = (PathBuf, u32, u32);

/// One call or construction of a node, by its index in [`FnIr::calls`] or
/// [`FnIr::constructions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum At {
    Call(usize),
    New(usize),
}

/// A call that hands a function on: (node, the call, argument, keys down to
/// the function inside the argument).
type HandedAt = (usize, At, usize, Vec<String>);

/// Every chain the guards in `inputs` state, sorted, with each file named
/// relative to `root`.
pub fn guard_chains(inputs: &RequestSummaryInputs, root: &Path) -> Vec<GuardChain> {
    let chains = Chains::new(inputs, root);
    let mut out = Vec::new();
    for (index, node) in chains.nodes.iter().enumerate() {
        for guard in &node.ir.guards {
            out.extend(chains.of_guard(index, guard));
        }
    }
    out.sort_by_cached_key(|chain| {
        (
            chain.site.clone(),
            chain.literal.clone(),
            chain.position,
            serde_json::to_string(chain).unwrap_or_default(),
        )
    });
    out
}

impl<'a> Chains<'a> {
    pub(super) fn new(inputs: &'a RequestSummaryInputs, root: &'a Path) -> Self {
        let mut chains = Self {
            inputs,
            root,
            names: LinkedNames::link(&inputs.files, &inputs.bindings, inputs.links.as_ref()),
            nodes: Vec::new(),
            calls: HashMap::new(),
            constructions: HashMap::new(),
            callers: HashMap::new(),
            handed: HashMap::new(),
        };
        let mut paths: Vec<&PathBuf> = inputs.files.keys().collect();
        paths.sort();
        for path in paths {
            let file = &inputs.files[path];
            let by_start: HashMap<u32, (&String, bool)> = file
                .definitions
                .iter()
                .filter_map(|(key, (start, exported))| Some(((*start)?, (key, *exported))))
                .collect();
            let mut keys: Vec<&String> = file.functions.keys().collect();
            keys.sort();
            for key in keys {
                let exported = file.definitions.get(key).is_some_and(|(_, e)| *e);
                chains.collect(
                    path,
                    Some(key.clone()),
                    exported,
                    &file.functions[key],
                    None,
                    &by_start,
                );
            }
            chains.collect(path, None, false, &file.module_level, None, &by_start);
        }
        for (index, node) in chains.nodes.iter().enumerate() {
            let every = (node.ir.calls.iter().enumerate())
                .map(|(i, call)| (At::Call(i), call))
                .chain(
                    (node.ir.constructions.iter().enumerate()).map(|(i, call)| (At::New(i), call)),
                );
            for (at, call) in every {
                let (map, i) = match at {
                    At::Call(i) => (&mut chains.calls, i),
                    At::New(i) => (&mut chains.constructions, i),
                };
                map.insert((node.file, call.site.lo, call.site.hi), (index, i));
                for (arg, path, name) in &call.handed {
                    chains
                        .handed
                        .entry((node.file, name.clone()))
                        .or_default()
                        .push((index, at, *arg, path.clone()));
                }
            }
        }
        if inputs.row_calls.is_some() {
            // With rows, each call and construction reaches what its row
            // names (carrick#2413 slice D).
            let mut every: Vec<(&'a Path, u32, u32)> = chains
                .calls
                .keys()
                .chain(chains.constructions.keys())
                .copied()
                .collect();
            every.sort();
            every.dedup();
            for (file, lo, hi) in every {
                if let Some(target) = inputs.call_target(file, lo, hi) {
                    chains
                        .callers
                        .entry(target)
                        .or_default()
                        .push((file.to_path_buf(), lo, hi));
                }
            }
        } else {
            for (file, lo, hi, target) in inputs.sites.every_site() {
                chains.callers.entry(target.clone()).or_default().push((
                    file.to_path_buf(),
                    lo,
                    hi,
                ));
            }
        }
        for callers in chains.callers.values_mut() {
            callers.sort();
        }
        chains
    }

    fn collect(
        &mut self,
        file: &'a Path,
        key: Option<String>,
        exported: bool,
        ir: &'a FnIr,
        parent: Option<usize>,
        by_start: &HashMap<u32, (&String, bool)>,
    ) {
        let index = self.nodes.len();
        self.nodes.push(Node {
            file,
            key,
            exported,
            ir,
            parent,
        });
        for child in ir.nested.iter().chain(&ir.detached) {
            let (key, exported) = child
                .site
                .and_then(|site| by_start.get(&site.lo))
                .map_or((None, false), |(key, exported)| {
                    (Some((*key).clone()), *exported)
                });
            self.collect(file, key, exported, child, Some(index), by_start);
        }
    }

    pub(super) fn rel(&self, path: &Path) -> String {
        path.strip_prefix(self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }

    pub(super) fn line_of(&self, node: usize) -> u32 {
        self.nodes[node].ir.site.map_or(0, |site| site.line)
    }

    /// Whether `open` ends a chain in `node` at an entry point: a keyed
    /// function the module exports that nothing in the repository calls
    /// and that is handed to no call (carrick#2474 R4b).
    pub(super) fn is_entry(&self, node: usize) -> bool {
        let current = &self.nodes[node];
        current.exported
            && current.key.as_ref().is_some_and(|key| {
                self.callers
                    .get(&(current.file.to_path_buf(), key.clone()))
                    .is_none_or(|callers| callers.is_empty())
            })
            && self.handed_at(node).is_empty()
    }

    /// Each call `node` is handed to (carrick#2474 R4b): the function the
    /// call is written in, the call, the argument and the keys down to the
    /// function inside it.
    pub(super) fn handed_calls(&self, node: usize) -> Vec<(usize, Site, usize, Vec<String>)> {
        self.handed_at(node)
            .into_iter()
            .map(|(caller, at, arg, path)| (caller, self.call(caller, at).site, arg, path))
            .collect()
    }

    /// The functions one call above `node` (carrick#2474 R4b, the census's
    /// what-if arm alone): its in-repo callers and the functions that hand
    /// it on.
    pub(super) fn one_call_up(&self, node: usize) -> Vec<usize> {
        let current = &self.nodes[node];
        let mut up = Vec::new();
        if let Some(key) = &current.key
            && let Some(callers) = self.callers.get(&(current.file.to_path_buf(), key.clone()))
        {
            for (file, lo, hi) in callers {
                if let Some(&(caller, _)) = self.calls.get(&(file.as_path(), *lo, *hi)) {
                    up.push(caller);
                }
            }
        }
        up.extend(self.handed_at(node).into_iter().map(|(caller, ..)| caller));
        up.sort();
        up.dedup();
        up
    }

    fn hole(&self, reason: HoleReason, node: usize) -> ChainEnd {
        ChainEnd::Hole {
            reason,
            file: self.rel(self.nodes[node].file),
            line: self.line_of(node),
            text: None,
        }
    }

    /// The literal a guard compares with, its shape, and the import an
    /// unread constant comes through. A constant that reads as no text, a
    /// builder's call among them, is kept as written: the hole and its
    /// reason are what the model reads.
    pub(super) fn literal(
        &self,
        node: usize,
        literal: &GuardLiteral,
    ) -> (String, GuardShape, Option<Constant>) {
        match literal {
            GuardLiteral::Text(text) => (text.clone(), shape_of(text), None),
            GuardLiteral::Named {
                pieces,
                written,
                constant,
            } => match self.names.text(self.nodes[node].file, pieces) {
                Some(text) => {
                    let shape = shape_of(&text);
                    (text, shape, None)
                }
                None => (written.clone(), GuardShape::Unresolved, constant.clone()),
            },
        }
    }

    /// The text `call`, written in `file`, states at each argument other
    /// than `arg`.
    fn other_args(&self, file: &Path, call: &CallIr, arg: usize) -> Vec<HopArg> {
        let Some(site_args) = &call.site_args else {
            return Vec::new();
        };
        site_args
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != arg)
            .filter_map(|(index, site_arg)| {
                let text = match (&site_arg.text, &site_arg.holes) {
                    (Some(text), _) => text.clone(),
                    (None, Some(pieces)) => self.names.text(file, pieces)?,
                    (None, None) => return None,
                };
                Some(HopArg { arg: index, text })
            })
            .collect()
    }

    /// Each call `node` is handed to: by its name, or written in one of the
    /// call's arguments.
    fn handed_at(&self, node: usize) -> Vec<HandedAt> {
        let current = &self.nodes[node];
        let mut handed: Vec<HandedAt> = Vec::new();
        if let Some(key) = &current.key
            && let Some(calls) = self.handed.get(&(current.file, key.clone()))
        {
            handed.extend(calls.iter().cloned());
        }
        if let Some(written_in) = &current.ir.written_in {
            let (map, at): (_, fn(usize) -> At) = if written_in.constructed {
                (&self.constructions, At::New)
            } else {
                (&self.calls, At::Call)
            };
            if let Some(&(n, c)) = map.get(&(current.file, written_in.site.lo, written_in.site.hi))
            {
                handed.push((n, at(c), written_in.arg, written_in.path.clone()));
            }
        }
        handed
    }

    /// The call or construction `at` of `node`.
    fn call(&self, node: usize, at: At) -> &'a CallIr {
        let ir = self.nodes[node].ir;
        match at {
            At::Call(index) => &ir.calls[index],
            At::New(index) => &ir.constructions[index],
        }
    }

    /// The calls `node` is handed to, as a chain that stops inside it with
    /// no hop states them.
    fn within(&self, node: usize) -> Vec<Within> {
        self.handed_at(node)
            .into_iter()
            .map(|(caller, at, arg, path)| {
                let file = self.nodes[caller].file;
                let call = self.call(caller, at);
                Within {
                    file: self.rel(file),
                    line: call.site.line,
                    callee: call.callee_text.clone(),
                    arg,
                    path,
                    args: self.other_args(file, call, arg),
                    constructed: matches!(at, At::New(_)),
                }
            })
            .collect()
    }

    /// The calls written in `node` between `lo` and `hi`, in source order.
    pub(super) fn branch_calls(&self, node: usize, (lo, hi): (u32, u32)) -> Vec<Branch> {
        let node = &self.nodes[node];
        let mut calls: Vec<_> = node
            .ir
            .calls
            .iter()
            .filter(|call| lo <= call.site.lo && call.site.hi <= hi)
            .collect();
        calls.sort_by_key(|call| (call.site.lo, call.site.hi));
        calls
            .into_iter()
            .map(|call| Branch::Call {
                callee: call.callee.clone(),
                file: self.rel(node.file),
                line: call.site.line,
                target: self
                    .inputs
                    .call_target(node.file, call.site.lo, call.site.hi)
                    .map(|(file, key)| format!("{}:{key}", self.rel(&file))),
            })
            .collect()
    }

    fn of_guard(&self, node: usize, guard: &GuardIr) -> Vec<GuardChain> {
        self.traced(node, guard)
            .into_iter()
            .map(|(chain, _)| chain)
            .collect()
    }

    /// Each chain of `guard`, written in `node`, with the facts its trace
    /// passed (carrick#2474 R4a): [`Self::of_guard`]'s chains, in the same
    /// order.
    pub(super) fn traced(&self, node: usize, guard: &GuardIr) -> Vec<(GuardChain, ChainFacts)> {
        let (literal, shape, constant) = self.literal(node, &guard.literal);
        let returns = &self.nodes[node].ir.returns_tests;
        let is_predicate = !returns.is_empty() && returns.iter().all(|test| *test);
        let branch = match &guard.branch {
            GuardBranch::Span(lo, hi) => Some(self.branch_calls(node, (*lo, *hi))),
            GuardBranch::Value { text, line } => Some(vec![Branch::Value {
                text: text.clone(),
                file: self.rel(self.nodes[node].file),
                line: *line,
            }]),
            GuardBranch::Returned if is_predicate => None,
            GuardBranch::Returned | GuardBranch::None => Some(Vec::new()),
        };
        let trace = Trace {
            subject: guard.subject.clone(),
            hops: Vec::new(),
            branch,
            negated: guard.negated,
            passed: Vec::new(),
            facts: ChainFacts::default(),
        };
        let mut ends = Ends::default();
        self.follow(node, trace, &mut ends);
        ends.found
            .into_iter()
            .map(|(trace, end)| {
                let chain = GuardChain {
                    literal: literal.clone(),
                    shape,
                    constant: constant.clone(),
                    within: if trace.hops.is_empty() && matches!(end, ChainEnd::Hole { .. }) {
                        self.within(node)
                    } else {
                        Vec::new()
                    },
                    position: guard.position,
                    negated: trace.negated,
                    site: ChainSite {
                        file: self.rel(self.nodes[node].file),
                        line: guard.site.line,
                    },
                    subject: guard.subject_text.clone(),
                    conjoined: guard
                        .conjoined
                        .iter()
                        .map(|(text, line)| Conjoined {
                            text: text.clone(),
                            line: *line,
                        })
                        .collect(),
                    hops: trace.hops,
                    end,
                    branch: trace.branch.unwrap_or_default(),
                };
                (chain, trace.facts)
            })
            .collect()
    }

    /// Follow `trace`, whose subject is read in `node`, to its ends.
    /// The hole a chain stops at on an import (carrick#2406): the in-repo
    /// import hole for a module of this service, its own reason for a
    /// specifier nothing resolves, and `None` for a package the manifests
    /// declare, which is an external import.
    ///
    /// With rows (carrick#2413 slice E), the declaration the import's rows
    /// name decides: one of the repository is the in-repo import hole, and
    /// one the rows do not give is its own reason.
    fn import_hole(&self, file: &Path, local: &str) -> Option<HoleReason> {
        use crate::call_graph::ImportedBinding;
        if let Some(links) = &self.inputs.links {
            return match links.get(file, local)? {
                super::row_links::RowLink::Hole(reason) => Some(*reason),
                link if link.in_repo() => Some(HoleReason::InRepoValueImport),
                _ => None,
            };
        }
        // FROZEN (old path, 2026-10-21): the branch without rows below only
        // (the import read by its specifier through the SWC resolver).
        // Deletions and safety fixes only. Fixes for wrong or missing rows go in
        // the compiler's position rows (the import's declaration and
        // `decl_package`, carrick#2413 slice E). Delete at carrick#2413 slice
        // F, once the unresolved-import (#2573) and rows-join budget (#2574)
        // fallbacks count zero (carrick#2413).
        match self.inputs.bindings.get(file, local)? {
            ImportedBinding::Unresolved => Some(HoleReason::UnresolvedImport),
            ImportedBinding::Binding { .. }
            | ImportedBinding::Module(_)
            | ImportedBinding::Unfollowable => Some(HoleReason::InRepoValueImport),
        }
    }

    /// The package the rows say an external import's export is declared in
    /// (carrick#2413 slice E). `None` without rows.
    fn import_package(&self, file: &Path, local: &str) -> Option<String> {
        match self.inputs.links.as_ref()?.get(file, local)? {
            super::row_links::RowLink::Outside(package) => package.clone(),
            _ => None,
        }
    }

    fn follow(&self, node: usize, mut trace: Trace, ends: &mut Ends) {
        trace.facts.stopped = Some(node);
        if ends.full() {
            ends.cap(self.hole(HoleReason::FanOutCap, node), trace);
            return;
        }
        let steps = trace.subject.rendered_steps();
        match trace.subject.root.clone() {
            Root::Literal(text) => ends.push(trace, ChainEnd::Literal { text, steps }),
            Root::Env { name, .. } => ends.push(trace, ChainEnd::Env { name, steps }),
            Root::Global(name) => ends.push(trace, ChainEnd::Global { name, steps }),
            Root::Import {
                local,
                specifier,
                export,
            } => {
                let end = match self.import_hole(self.nodes[node].file, &local) {
                    Some(reason) => self.hole(reason, node),
                    None => ChainEnd::ExternalImport {
                        package: self.import_package(self.nodes[node].file, &local),
                        specifier,
                        export,
                        steps,
                    },
                };
                ends.push(trace, end);
            }
            Root::Hole(reason) => {
                let end = ChainEnd::Hole {
                    reason,
                    file: self.rel(self.nodes[node].file),
                    line: self.line_of(node),
                    text: trace.subject.text.clone(),
                };
                ends.push(trace, end);
            }
            Root::Outer(depth, index, name) => {
                let mut at = node;
                for _ in 0..depth {
                    match self.nodes[at].parent {
                        Some(parent) => at = parent,
                        None => {
                            let end = self.hole(HoleReason::UnreadExpression, node);
                            ends.push(trace, end);
                            return;
                        }
                    }
                }
                trace.hops.push(Hop::Enclosed {
                    file: self.rel(self.nodes[at].file),
                    line: self.line_of(at),
                });
                trace.subject.root = Root::Param(index, name);
                self.open(at, index, trace, ends);
            }
            Root::Param(index, _) => self.open(node, index, trace, ends),
        }
    }

    /// Where `node`'s parameter `index` comes from: its callers, the calls
    /// it is handed to, or the function itself.
    fn open(&self, node: usize, index: usize, mut trace: Trace, ends: &mut Ends) {
        trace.facts.stopped = Some(node);
        if trace.passed.contains(&node) {
            let end = self.hole(HoleReason::Recursion, node);
            ends.push(trace, end);
            return;
        }
        trace.passed.push(node);
        let current = &self.nodes[node];
        let callers = current
            .key
            .as_ref()
            .and_then(|key| self.callers.get(&(current.file.to_path_buf(), key.clone())))
            .filter(|callers| !callers.is_empty());
        if let Some(callers) = callers {
            for (file, lo, hi) in callers {
                if ends.full() {
                    ends.cap(self.hole(HoleReason::FanOutCap, node), trace.clone());
                    return;
                }
                if !self.inputs.files.contains_key(file) {
                    let end = ChainEnd::Hole {
                        reason: HoleReason::OutsideService,
                        file: self.rel(file),
                        line: 0,
                        text: None,
                    };
                    ends.push(trace.clone(), end);
                    continue;
                }
                let Some(&(caller, call_index)) = self.calls.get(&(file.as_path(), *lo, *hi))
                else {
                    let end = self.hole(HoleReason::CallerArgMissing, node);
                    ends.push(trace.clone(), end);
                    continue;
                };
                let call = &self.nodes[caller].ir.calls[call_index];
                let mut next = trace.clone();
                next.hops.push(Hop::Call {
                    file: self.rel(file),
                    line: call.site.line,
                    callee: call.callee.clone(),
                    arg: index,
                });
                if next.branch.is_none()
                    && let Some(tested) = call.tested
                {
                    next.branch = Some(self.branch_calls(caller, tested.branch));
                    next.negated ^= tested.negated;
                    next.facts.decided_at = Some(DecidedAt {
                        node: caller,
                        span: (call.site.lo, call.site.hi),
                        branch: tested.branch,
                        conjunct: tested.conjunct,
                    });
                }
                match call.arg_subjects.get(index).cloned().flatten() {
                    Some(arg) => {
                        next.subject = arg.joined(&trace.subject.steps);
                        self.follow(caller, next, ends);
                    }
                    None => {
                        let end = ChainEnd::Hole {
                            reason: HoleReason::CallerArgMissing,
                            file: self.rel(file),
                            line: call.site.line,
                            text: None,
                        };
                        ends.push(next, end);
                    }
                }
            }
            return;
        }
        // Handed to a call: by name, or written in one of its arguments.
        let handed = self.handed_at(node);
        if !handed.is_empty() {
            for (caller, at, arg, path) in handed {
                self.handed_to(node, caller, at, arg, path, index, trace.clone(), ends);
            }
            return;
        }
        let end = match &current.key {
            Some(key) if current.exported => {
                trace.facts.entry = Some(EntryAt {
                    node,
                    param: index,
                    steps: trace.subject.steps.clone(),
                });
                ChainEnd::EntryPoint {
                    file: self.rel(current.file),
                    line: self.line_of(node),
                    function: key.clone(),
                    param: index,
                    steps: trace.subject.rendered_steps(),
                }
            }
            Some(_) => self.hole(HoleReason::NoCallerNotExported, node),
            None => self.hole(HoleReason::DetachedUnplaced, node),
        };
        ends.push(trace, end);
    }

    /// `handed`'s parameter `param`, followed into the call `at` of `node`
    /// that `handed` is passed to.
    #[allow(clippy::too_many_arguments)]
    fn handed_to(
        &self,
        handed: usize,
        node: usize,
        at: At,
        arg: usize,
        path: Vec<String>,
        param: usize,
        mut trace: Trace,
        ends: &mut Ends,
    ) {
        let file = self.nodes[node].file;
        let call = self.call(node, at);
        if trace.facts.handoff.is_none() {
            trace.facts.handoff = Some(Handoff {
                handed,
                caller: node,
                site: call.site,
                arg,
                path: path.clone(),
                param,
                steps: trace.subject.steps.clone(),
            });
        }
        trace.hops.push(Hop::HandedTo {
            file: self.rel(file),
            line: call.site.line,
            callee: call.callee_text.clone(),
            arg,
            path,
            param,
            steps: trace.subject.rendered_steps(),
            args: self.other_args(file, call, arg),
            constructed: matches!(at, At::New(_)),
        });
        let at_call = |reason, text: Option<String>| ChainEnd::Hole {
            reason,
            file: self.rel(file),
            line: call.site.line,
            text,
        };
        match self.inputs.reach(file, call.site.lo, call.site.hi) {
            super::row_calls::Reach::Function(..) | super::row_calls::Reach::OutsideService(..) => {
                ends.push(trace, at_call(HoleReason::CallbackIntoOwnFunction, None));
                return;
            }
            // The rows cannot say whose the callee is (carrick#2413 slice D).
            super::row_calls::Reach::Unknown(reason) => {
                ends.push(trace, at_call(reason, None));
                return;
            }
            super::row_calls::Reach::NoFunction => {}
        }
        let Some(callee) = call.callee_subject.clone() else {
            ends.push(trace, at_call(HoleReason::CalleeUnread, None));
            return;
        };
        let steps = callee.rendered_steps();
        match &callee.root {
            Root::Param(..) | Root::Outer(..) => {
                trace.subject = callee;
                self.follow(node, trace, ends);
            }
            // A callee imported from a module of this service is the
            // in-repo import hole, as `follow` and a named call's callee
            // label it (carrick#2406).
            Root::Import {
                local,
                specifier,
                export,
            } => {
                let end = match self.import_hole(file, local) {
                    Some(reason) => at_call(reason, Some(local.clone())),
                    None => ChainEnd::ExternalImport {
                        specifier: specifier.clone(),
                        export: export.clone(),
                        steps,
                        package: self.import_package(file, local),
                    },
                };
                ends.push(trace, end);
            }
            Root::Global(name) => {
                let end = ChainEnd::Global {
                    name: name.clone(),
                    steps,
                };
                ends.push(trace, end);
            }
            // The callee's own hole names where reading it stopped.
            _ => {
                let text = callee.text.clone();
                ends.push(trace, at_call(HoleReason::CalleeUnread, text));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The calls that name an operation.

/// What a named call is: a name and a function (`bus.on("item", fn)`,
/// `table.set(Ops.SAVE, fn)`), a name and no function (`sender.send("DONE",
/// data)`), or a construction that names one (`new Channel({ handlers: {
/// PING: fn } })`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallStart {
    Registration,
    Emission,
    Construction,
}

/// Where a call's name is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NameFrom {
    /// The argument itself.
    Argument,
    /// A field of an object-literal argument (`{ type: "START" }`).
    Field,
    /// The key a function sits under in an object argument (`{ handlers: {
    /// PING: fn } }`).
    Key,
}

/// One name a call states: text, or a constant the value flow cannot read,
/// kept as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallName {
    pub arg: usize,
    pub from: NameFrom,
    /// The field, or the keys down to the function.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
    pub text: String,
    pub shape: GuardShape,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constant: Option<Constant>,
}

/// One function a call is handed: written in an argument, or named there.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CallFunction {
    pub arg: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
    /// The function's name, for one the module declares and the call hands
    /// on by name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Where a named call's callee comes from: the hops out of the function the
/// call is written in, and the end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallChain {
    pub hops: Vec<Hop>,
    pub end: ChainEnd,
}

/// The package receiver the library-site reader places a call on
/// ([`super::library_sites`]): the import, each receiver id it may be made
/// through, and the member path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LibraryReceiver {
    pub specifier: String,
    pub export: String,
    pub receivers: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
}

/// One call that names an operation in its arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NamedCall {
    pub start: CallStart,
    pub site: ChainSite,
    /// The callee as written.
    pub callee: String,
    /// The member called, when the callee is a member.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// A call on a runtime global, or one of the language's collections,
    /// that sends nothing.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub inert: bool,
    /// An argument is spread: positions after it are not read.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub spread: bool,
    pub names: Vec<CallName>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub functions: Vec<CallFunction>,
    pub chains: Vec<CallChain>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub library: Vec<LibraryReceiver>,
}

/// (file, whether a construction, span start, span end) of a call -> each
/// function written in its arguments: the argument and the keys down to it.
type Placed<'a> = HashMap<(&'a Path, bool, u32, u32), Vec<(usize, Vec<String>)>>;

/// (file, span start, span end), SWC numbering -> the package receivers the
/// library-site reader places the call there on.
type Receivers = HashMap<(PathBuf, u32, u32), Vec<LibraryReceiver>>;

/// Every call and construction in `inputs` that names an operation in its
/// arguments, with the chain its callee is followed down, sorted, each file
/// named relative to `root`. A JSX element names no operation, and a
/// `startsWith` test is a guard.
pub fn named_calls(inputs: &RequestSummaryInputs, root: &Path) -> Vec<NamedCall> {
    let chains = Chains::new(inputs, root);
    let mut receivers: Receivers = HashMap::new();
    for site in super::library_sites::library_sites(inputs).sites {
        // A site a caller fills is the library call inside the function the
        // caller calls, not a call at the caller's span.
        if site.origin.is_some() {
            continue;
        }
        receivers
            .entry((site.file.clone(), site.span_start, site.span_end))
            .or_default()
            .push(LibraryReceiver {
                receivers: site.receiver_ids(),
                specifier: site.specifier,
                export: site.export,
                path: site.path,
                member: site.member,
            });
    }
    let mut placed: Placed<'_> = HashMap::new();
    for node in &chains.nodes {
        if let Some(written_in) = &node.ir.written_in {
            placed
                .entry((
                    node.file,
                    written_in.constructed,
                    written_in.site.lo,
                    written_in.site.hi,
                ))
                .or_default()
                .push((written_in.arg, written_in.path.clone()));
        }
    }
    let mut out = Vec::new();
    for (index, node) in chains.nodes.iter().enumerate() {
        let tests: HashSet<(u32, u32)> = node
            .ir
            .guards
            .iter()
            .filter(|guard| guard.position == GuardPosition::StartsWith)
            .map(|guard| (guard.site.lo, guard.site.hi))
            .collect();
        for (call_index, call) in node.ir.calls.iter().enumerate() {
            if call.jsx || tests.contains(&(call.site.lo, call.site.hi)) {
                continue;
            }
            out.extend(chains.named_call(index, At::Call(call_index), &placed, &receivers));
        }
        for call_index in 0..node.ir.constructions.len() {
            out.extend(chains.named_call(index, At::New(call_index), &placed, &receivers));
        }
    }
    out.sort_by_cached_key(|call| {
        (
            call.site.clone(),
            call.callee.clone(),
            serde_json::to_string(call).unwrap_or_default(),
        )
    });
    out
}

impl<'a> Chains<'a> {
    /// The call `at` of `node` as a named call, when it names one.
    fn named_call(
        &self,
        node: usize,
        at: At,
        placed: &Placed<'_>,
        receivers: &Receivers,
    ) -> Option<NamedCall> {
        let file = self.nodes[node].file;
        let call = self.call(node, at);
        let constructed = matches!(at, At::New(_));
        let mut functions: Vec<CallFunction> = placed
            .get(&(file, constructed, call.site.lo, call.site.hi))
            .into_iter()
            .flatten()
            .map(|(arg, path)| CallFunction {
                arg: *arg,
                path: path.clone(),
                name: None,
            })
            .chain(call.handed.iter().map(|(arg, path, name)| CallFunction {
                arg: *arg,
                path: path.clone(),
                name: Some(name.clone()),
            }))
            .collect();
        functions.sort();
        functions.dedup();
        let mut names: Vec<CallName> = call
            .names
            .iter()
            .filter_map(|name| self.call_name(file, name))
            .collect();
        // The key a function sits under names it; an array's index does
        // not.
        names.extend(functions.iter().filter_map(|function| {
            let key = function.path.last()?;
            if key.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            Some(CallName {
                arg: function.arg,
                from: NameFrom::Key,
                path: function.path.clone(),
                text: key.clone(),
                shape: shape_of(key),
                constant: None,
            })
        }));
        if names.is_empty() {
            return None;
        }
        names.sort_by(|a, b| (a.arg, &a.path, &a.text).cmp(&(b.arg, &b.path, &b.text)));
        let start = if constructed {
            CallStart::Construction
        } else if functions.is_empty() {
            CallStart::Emission
        } else {
            CallStart::Registration
        };
        let trace = Trace {
            subject: Subject::hole(HoleReason::UnreadExpression),
            hops: Vec::new(),
            branch: Some(Vec::new()),
            negated: false,
            passed: Vec::new(),
            facts: ChainFacts::default(),
        };
        let mut ends = Ends::default();
        self.callee_end(node, call, trace, &mut ends);
        Some(NamedCall {
            start,
            site: ChainSite {
                file: self.rel(file),
                line: call.site.line,
            },
            callee: call.callee_text.clone(),
            member: (!call.callee.is_empty() && call.callee != call.callee_text)
                .then(|| call.callee.clone()),
            inert: call.inert,
            spread: call.site_args.is_none(),
            names,
            functions,
            chains: ends
                .found
                .into_iter()
                .map(|(trace, end)| CallChain {
                    hops: trace.hops,
                    end,
                })
                .collect(),
            library: receivers
                .get(&(file.to_path_buf(), call.site.span_start, call.site.span_end))
                .cloned()
                .unwrap_or_default(),
        })
    }

    /// One name an argument states, read once every module's uses are
    /// known. A binding that is known to hold no text (an import of a
    /// function, a router, an instance a module declares) names nothing;
    /// a constant the value flow cannot read is kept as written.
    fn call_name(&self, file: &Path, name: &ArgName) -> Option<CallName> {
        let (from, path) = match &name.field {
            Some(field) => (NameFrom::Field, vec![field.clone()]),
            None => (NameFrom::Argument, Vec::new()),
        };
        if let Some(text) = self.names.text(file, &name.pieces) {
            return Some(CallName {
                arg: name.arg,
                from,
                path,
                shape: shape_of(&text),
                text,
                constant: None,
            });
        }
        let holds_no_text = name.pieces.iter().any(|piece| {
            matches!(piece, TextPiece::Named(named)
                if named.args.is_none()
                    && named.path.is_empty()
                    && self.names.holds_no_text(file, &named.binding))
        });
        if holds_no_text {
            return None;
        }
        Some(CallName {
            arg: name.arg,
            from,
            path,
            text: name.written.clone(),
            shape: GuardShape::Unresolved,
            constant: name.constant.clone(),
        })
    }

    /// Where a named call's callee comes from, as the hop into a call a
    /// function is handed to reads it ([`Self::handed_to`]): a parameter is
    /// followed out to its callers; a value imported from this service is
    /// a hole, not a package.
    fn callee_end(&self, node: usize, call: &CallIr, mut trace: Trace, ends: &mut Ends) {
        let file = self.nodes[node].file;
        let at_call = |reason, text: Option<String>| ChainEnd::Hole {
            reason,
            file: self.rel(file),
            line: call.site.line,
            text,
        };
        match self.inputs.reach(file, call.site.lo, call.site.hi) {
            super::row_calls::Reach::Function(..) | super::row_calls::Reach::OutsideService(..) => {
                ends.push(trace, at_call(HoleReason::OwnFunction, None));
                return;
            }
            // The rows cannot say whose the callee is (carrick#2413 slice D).
            super::row_calls::Reach::Unknown(reason) => {
                ends.push(trace, at_call(reason, None));
                return;
            }
            super::row_calls::Reach::NoFunction => {}
        }
        let Some(callee) = call.callee_subject.clone() else {
            ends.push(trace, at_call(HoleReason::CalleeUnread, None));
            return;
        };
        let steps = callee.rendered_steps();
        let end = match &callee.root {
            Root::Param(..) | Root::Outer(..) => {
                trace.subject = callee;
                self.follow(node, trace, ends);
                return;
            }
            Root::Import {
                local,
                specifier,
                export,
            } => match self.import_hole(file, local) {
                Some(reason) => at_call(reason, Some(local.clone())),
                None => ChainEnd::ExternalImport {
                    specifier: specifier.clone(),
                    export: export.clone(),
                    steps,
                    package: self.import_package(file, local),
                },
            },
            Root::Global(name) => ChainEnd::Global {
                name: name.clone(),
                steps,
            },
            _ => at_call(HoleReason::CalleeUnread, callee.text.clone()),
        };
        ends.push(trace, end);
    }
}

/// The ends one guard reaches, capped at [`MAX_CHAINS_PER_GUARD`].
#[derive(Default)]
struct Ends {
    found: Vec<(Trace, ChainEnd)>,
    capped: bool,
}

impl Ends {
    fn full(&self) -> bool {
        self.found.len() >= MAX_CHAINS_PER_GUARD
    }

    fn push(&mut self, trace: Trace, end: ChainEnd) {
        if self.full() {
            self.cap(end, trace);
            return;
        }
        self.found.push((trace, end));
    }

    /// One chain for every path past the cap.
    fn cap(&mut self, end: ChainEnd, trace: Trace) {
        if self.capped {
            return;
        }
        self.capped = true;
        let end = match end {
            hole @ ChainEnd::Hole {
                reason: HoleReason::FanOutCap,
                ..
            } => hole,
            _ => ChainEnd::Hole {
                reason: HoleReason::FanOutCap,
                file: String::new(),
                line: 0,
                text: None,
            },
        };
        self.found.push((trace, end));
    }
}

/// `steps` with the String methods after the last access dropped
/// (`url.toLowerCase()` reads `url`).
pub(super) fn read_steps(steps: &[Step]) -> &[Step] {
    let mut end = steps.len();
    while end > 0 && matches!(steps[end - 1], Step::Call(_)) {
        end -= 1;
    }
    &steps[..end]
}

/// A label's member path, as the steps a chain reads it with.
pub(super) fn key_steps(path: &[String]) -> Vec<Step> {
    path.iter().map(|key| Step::Key(key.clone())).collect()
}

/// The HTTP methods the module-scope function `binding` compares the
/// request's method against, in source order, each once (carrick#2740).
///
/// `param` and `path` say where the function's request method is: the
/// parameter by position and the members down from it (`request.method` of a
/// route module's `({ request })`). A guard counts when it is an equality
/// test or a `case` against text naming an HTTP method, and its subject reads
/// exactly that member (String methods after it, like `toUpperCase()`, read
/// through). Polarity is not read: a file route has no path comparison, so
/// `if (request.method !== "PUT") throw` is as much the narrowing as
/// `=== "PUT"`.
///
/// The function's own guards count, and those of every function written
/// inside it, where the subject is the parameter as that depth reads it (a
/// function with a request parameter of its own compares another request).
/// Callers and callees are not followed. A constant imported from another
/// module reads nothing, and neither does a function built by a call
/// (`withAuth(async ({ request }) => ...)`) or a method read through
/// `String(request.method)`: the caller keeps the convention's default verb.
pub fn compared_methods(
    ir: &super::FileIr,
    binding: &str,
    param: usize,
    path: &[String],
) -> Vec<String> {
    fn walk(f: &FnIr, depth: u8, param: usize, want: &[Step], out: &mut Vec<(u32, String)>) {
        for guard in &f.guards {
            if !matches!(
                guard.position,
                GuardPosition::Equality | GuardPosition::Case
            ) {
                continue;
            }
            let GuardLiteral::Text(text) = &guard.literal else {
                continue;
            };
            if !is_http_method(text) {
                continue;
            }
            let at_depth = match &guard.subject.root {
                Root::Param(index, _) => depth == 0 && *index == param,
                Root::Outer(d, index, _) => depth > 0 && *d == depth && *index == param,
                _ => false,
            };
            if at_depth && read_steps(&guard.subject.steps) == want {
                out.push((guard.site.lo, text.trim().to_uppercase()));
            }
        }
        for inner in f.nested.iter().chain(&f.detached) {
            walk(inner, depth.saturating_add(1), param, want, out);
        }
    }

    let Some(function) = ir.functions.get(binding) else {
        return Vec::new();
    };
    let want = key_steps(path);
    let mut found = Vec::new();
    walk(function, 0, param, &want, &mut found);
    found.sort();
    let mut methods: Vec<String> = Vec::new();
    for (_, method) in found {
        if !methods.contains(&method) {
            methods.push(method);
        }
    }
    methods
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(path: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(path)
    }

    /// Which chains a key holds.
    #[derive(Clone, Copy)]
    enum Keyed {
        /// Every chain.
        All,
        /// The route-path chains, with their shape left out: the keys
        /// written for slice B, before any other literal was read.
        Paths,
    }

    /// The chains discovery reads in the fixture at `dir`, against the key
    /// written for it by hand.
    fn assert_chains(dir: &str, key: &str, keyed: Keyed) {
        let root = fixture(dir);
        let inputs = crate::engine::discover_request_inputs(&root);
        let chains: Vec<serde_json::Value> = guard_chains(&inputs, &root)
            .into_iter()
            .filter(|chain| matches!(keyed, Keyed::All) || chain.shape == GuardShape::Path)
            .map(|chain| {
                let mut value = serde_json::to_value(chain).expect("chains serialise");
                if matches!(keyed, Keyed::Paths)
                    && let Some(object) = value.as_object_mut()
                {
                    object.remove("shape");
                }
                value
            })
            .collect();
        let chains = serde_json::Value::Array(chains);
        let key_path = fixture(&format!("receive-guards/{key}.guards.json"));
        let expected: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&key_path).expect("the key is readable"))
                .expect("the key is JSON");
        assert_eq!(
            chains,
            expected,
            "chains for {dir}:\n{}",
            serde_json::to_string_pretty(&chains).unwrap_or_default()
        );
    }

    #[test]
    fn a_platform_worker_states_each_route_up_to_its_exported_handler() {
        assert_chains("hand-dispatch-worker", "hand-dispatch-worker", Keyed::Paths);
    }

    #[test]
    fn every_export_form_of_a_worker_reaches_its_handler_or_its_in_repo_caller() {
        assert_chains(
            "receive-guards/worker-variants",
            "worker-variants",
            Keyed::Paths,
        );
    }

    #[test]
    fn a_node_server_handler_ends_at_the_package_export_it_is_handed_to() {
        assert_chains("receive-guards/node-server", "node-server", Keyed::Paths);
    }

    #[test]
    fn a_dev_server_middleware_ends_at_the_config_export_through_its_plugin_hook() {
        assert_chains(
            "receive-guards/dev-middleware",
            "dev-middleware",
            Keyed::Paths,
        );
    }

    #[test]
    fn a_predicate_in_another_file_composes_through_its_callers() {
        assert_chains(
            "receive-guards/predicate-elsewhere",
            "predicate-elsewhere",
            Keyed::Paths,
        );
    }

    /// A message listener's switch states each message name; the
    /// registration's own name rides on the hop, or, where the chain stops
    /// inside the listener, on `within`. A constant read through an import
    /// states its text, and one from a package states where it comes from.
    #[test]
    fn a_message_switch_states_each_name_up_to_the_listener_it_is_handed_to() {
        assert_chains(
            "receive-guards/message-switch",
            "message-switch",
            Keyed::All,
        );
    }

    /// A route compared with an imported constant states the constant's
    /// text through a re-export; one from a package is kept as written.
    #[test]
    fn a_route_compared_with_an_imported_constant_states_its_text() {
        assert_chains(
            "receive-guards/constant-routes",
            "constant-routes",
            Keyed::All,
        );
    }

    /// A handler table indexed with a message field states each of its
    /// keys, route-shaped or not.
    #[test]
    fn a_handler_table_states_every_key() {
        assert_chains(
            "receive-guards/handler-tables",
            "handler-tables",
            Keyed::All,
        );
    }

    /// The named calls discovery reads in the fixture at `dir`, against the
    /// `.calls.json` key written for it by hand.
    fn assert_named_calls(dir: &str, key: &str) {
        let root = fixture(dir);
        let inputs = crate::engine::discover_request_inputs(&root);
        let calls = serde_json::to_value(named_calls(&inputs, &root)).expect("calls serialise");
        let key_path = fixture(&format!("receive-guards/{key}.calls.json"));
        let expected: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&key_path).expect("the key is readable"))
                .expect("the key is JSON");
        assert_eq!(
            calls,
            expected,
            "named calls for {dir}:\n{}",
            serde_json::to_string_pretty(&calls).unwrap_or_default()
        );
    }

    /// A child process's and a worker's `message` listeners are
    /// registrations; a send with a message field is an emission.
    #[test]
    fn a_message_listener_is_a_registration_under_its_event_name() {
        assert_named_calls("receive-guards/message-switch", "message-switch");
    }

    /// A route registered under an imported constant with spread handlers
    /// states the constant's text and reads the arguments before the spread.
    #[test]
    fn a_route_registered_under_an_imported_constant_states_its_text() {
        assert_named_calls("receive-guards/constant-routes", "constant-routes");
    }

    /// A handler table handed to a package's constructor names each
    /// handler by its key; a `Map` filled under a sibling file's constants
    /// states their text.
    #[test]
    fn a_handler_table_in_a_construction_names_each_handler_by_its_key() {
        assert_named_calls("receive-guards/handler-tables", "handler-tables");
    }

    /// Listeners and sends: on a field of a runtime emitter, on a package
    /// import, on a parameter followed to its caller, and on a package
    /// maker's instance, each with the receiver the library-site reader
    /// places it on.
    #[test]
    fn listeners_and_sends_follow_their_receiver() {
        assert_named_calls("receive-guards/listeners-and-sends", "listeners-and-sends");
    }

    /// The named calls of one service written from `files` (path, source),
    /// with `manifest` as its package.json.
    fn named_calls_in(manifest: &str, files: &[(&str, &str)]) -> Vec<NamedCall> {
        let dir = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(dir.path().join("package.json"), manifest)
            .expect("the manifest is writable");
        for (path, source) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("the tree");
            std::fs::write(path, source).expect("the source is writable");
        }
        let inputs = crate::engine::discover_request_inputs(dir.path());
        named_calls(&inputs, dir.path())
    }

    /// carrick#2406: the receive side decides an environment read by the
    /// AST recogniser the send side uses, and names the variable rather
    /// than its spelling. A file that binds its own `process` reads one too,
    /// as the send side reads it (`the_send_side_reads_an_environment_
    /// spelling_without_a_shadow_check`): the spelling decides on both.
    #[test]
    fn an_environment_read_on_the_receive_side_names_its_variable() {
        let chains = chains_in(&[
            (
                "src/check.ts",
                r#"import process from "./fake-process";

function check(path: string): boolean {
  return path === "/health";
}

export function dotted(): boolean {
  return check(process.env.API);
}
"#,
            ),
            (
                "src/fake-process.ts",
                "export default { env: { API: \"x\" } };\n",
            ),
            (
                "src/reads.ts",
                r#"function check(path: string): boolean {
  return path === "/ready";
}

export function dotted(): boolean {
  return check(process.env.API);
}

export function indexed(): boolean {
  return check(process.env["MY-VAR"]);
}

export function cast(): boolean {
  return check(process.env.API as string);
}

export function asserted(): boolean {
  return check(process.env.API!);
}

export function deno(): boolean {
  return check(Deno.env.get("API"));
}
"#,
            ),
        ]);
        let mut ready: Vec<String> = chains
            .iter()
            .filter(|(literal, _, _)| literal == "/ready")
            .map(|(_, _, end)| {
                if end["kind"] == "env" && end["steps"] == serde_json::json!([]) {
                    end["name"].as_str().unwrap_or_default().to_string()
                } else {
                    format!("not an environment read: {end}")
                }
            })
            .collect();
        ready.sort();
        // Old answers: `process.env.API` as written for the dotted, cast
        // and asserted reads and `Deno.env.get("API")` for the call; a
        // global `process` with steps `env`, `MY-VAR` for the index.
        assert_eq!(ready, ["API", "API", "API", "API", "MY-VAR"], "{chains:#?}");
        // Old answer: the import's hole, because `member_subject` required
        // `process` to be the global.
        let health = end_of(&chains, "/health");
        assert_eq!(
            (&health["kind"], &health["name"]),
            (&serde_json::json!("env"), &serde_json::json!("API")),
            "an imported `process` reads as the send side reads it: {health}"
        );
    }

    /// carrick#2406: a function handed to a callee imported from a module
    /// of this service ends at the in-repo import hole on the hop, as
    /// `follow` and a named call's callee label it, not as an external
    /// import.
    #[test]
    fn a_function_handed_to_an_in_repo_import_ends_at_the_in_repo_hole() {
        let chains = chains_in(&[
            (
                "src/registry.ts",
                r#"import { createRegistry } from "invented-registry";

export const registry = createRegistry();
"#,
            ),
            (
                "src/main.ts",
                r#"import { registry } from "./registry";

registry.add((path: string) => path === "/health");
"#,
            ),
        ]);
        assert_eq!(
            end_of(&chains, "/health"),
            &serde_json::json!({
                "kind": "hole", "reason": "in_repo_value_import",
                "file": "src/main.ts", "line": 3, "text": "registry"
            })
        );
    }

    /// carrick#2406: an import whose specifier nothing the scan reads
    /// resolves (an undeclared package, an alias no config maps) ends at a
    /// hole of its own, on the hop and on `follow`, not at the in-repo
    /// import hole: it may be a module of this service or a package. A
    /// package the manifest declares is an external import.
    #[test]
    fn an_import_nothing_resolves_ends_at_the_unresolved_import_hole() {
        let chains = chains_with(
            r#"{ "name": "probe", "dependencies": { "invented-declared": "1.0.0" } }"#,
            &[(
                "src/main.ts",
                r#"import { registry } from "invented-undeclared";
import { registry as declared } from "invented-declared";
import { mode } from "@/unmapped/mode";

registry.add((path: string) => path === "/hop-undeclared");
declared.add((path: string) => path === "/hop-declared");

function check(path: string): boolean {
  return path === "/follow-alias";
}

export function read(): boolean {
  return check(mode);
}
"#,
            )],
        );
        let reason = |literal: &str| end_of(&chains, literal)["reason"].clone();
        assert_eq!(
            reason("/hop-undeclared"),
            "unresolved_import",
            "{chains:#?}"
        );
        assert_eq!(reason("/follow-alias"), "unresolved_import", "{chains:#?}");
        assert_eq!(
            end_of(&chains, "/hop-declared")["kind"],
            "external_import",
            "{chains:#?}"
        );
    }

    /// A binding a module of the service declares as no name (a function,
    /// an instance) names nothing when handed to a call; a package's
    /// constant, and a builder's call that reads as no text, are kept as
    /// written, `unresolved`; a parameter is the caller's value, so names
    /// nothing.
    #[test]
    fn an_argument_names_text_or_a_constant_never_a_value_the_service_declares() {
        let calls = named_calls_in(
            r#"{ "name": "probe", "dependencies": { "invented-app": "1.0.0", "invented-protocol": "1.0.0" } }"#,
            &[
                (
                    "src/router.ts",
                    r#"export function router(): void {}
export const store = new Map<string, string>();
"#,
                ),
                (
                    "src/main.ts",
                    r#"import { app } from "invented-app";
import { Topics, topicFor } from "invented-protocol";
import { router, store } from "./router";

app.use(router);
app.use(store);
app.emit(Topics.CREATED);
app.emit(topicFor("updated"));

export function relay(topic: string): void {
  app.emit(topic);
}
"#,
                ),
            ],
        );
        let names: Vec<(u32, String, GuardShape, Option<Constant>)> = calls
            .iter()
            .flat_map(|call| {
                call.names.iter().map(|name| {
                    (
                        call.site.line,
                        name.text.clone(),
                        name.shape,
                        name.constant.clone(),
                    )
                })
            })
            .collect();
        assert_eq!(
            names,
            vec![
                (
                    7,
                    "Topics.CREATED".to_string(),
                    GuardShape::Unresolved,
                    Some(Constant {
                        specifier: "invented-protocol".to_string(),
                        export: "Topics".to_string(),
                        path: vec!["CREATED".to_string()],
                    })
                ),
                (
                    8,
                    r#"topicFor("updated")"#.to_string(),
                    GuardShape::Unresolved,
                    Some(Constant {
                        specifier: "invented-protocol".to_string(),
                        export: "topicFor".to_string(),
                        path: Vec::new(),
                    })
                ),
                // The builder's own call names its text argument.
                (8, "updated".to_string(), GuardShape::Name, None),
            ],
            "{calls:#?}"
        );
    }

    /// A function written in a construction's argument is handed to the
    /// construction, so a guard in it reaches the constructor's import
    /// rather than stopping unplaced.
    #[test]
    fn a_guard_in_a_function_handed_to_a_construction_reaches_the_constructor() {
        // The package is declared: an undeclared one may be a module of the
        // service, and ends at the in-repo import hole (carrick#2406).
        let manifest = r#"{ "name": "probe", "dependencies": { "invented-ipc": "1.0.0" } }"#;
        let chains = chains_with(
            manifest,
            &[(
                "src/server.ts",
                r#"import { Server } from "invented-ipc";

export const server = new Server({
  onMessage: (message: { type: string }) => {
    if (message.type === "PING") {
      return;
    }
  },
});
"#,
            )],
        );
        let (_, hops, end) = chains
            .iter()
            .find(|(literal, _, _)| literal == "PING")
            .expect("a chain for PING");
        assert_eq!(
            hops,
            &serde_json::json!([{
                "kind": "handed_to", "file": "src/server.ts", "line": 3,
                "callee": "Server", "arg": 0, "path": ["onMessage"], "param": 0,
                "steps": ["type"], "constructed": true
            }])
        );
        assert_eq!(
            end,
            &serde_json::json!({
                "kind": "external_import", "specifier": "invented-ipc",
                "export": "Server", "steps": []
            })
        );
    }

    /// The chains of one service written from `files` (path, source), each
    /// as (literal, hops, end) JSON.
    fn chains_in(files: &[(&str, &str)]) -> Vec<(String, serde_json::Value, serde_json::Value)> {
        chains_with(r#"{ "name": "probe" }"#, files)
    }

    /// [`chains_in`], with `manifest` as the service's package.json.
    fn chains_with(
        manifest: &str,
        files: &[(&str, &str)],
    ) -> Vec<(String, serde_json::Value, serde_json::Value)> {
        let dir = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(dir.path().join("package.json"), manifest)
            .expect("the manifest is writable");
        for (path, source) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("the tree");
            std::fs::write(path, source).expect("the source is writable");
        }
        let inputs = crate::engine::discover_request_inputs(dir.path());
        guard_chains(&inputs, dir.path())
            .into_iter()
            .map(|chain| {
                (
                    chain.literal,
                    serde_json::to_value(chain.hops).expect("hops serialise"),
                    serde_json::to_value(chain.end).expect("ends serialise"),
                )
            })
            .collect()
    }

    fn end_of<'c>(
        chains: &'c [(String, serde_json::Value, serde_json::Value)],
        literal: &str,
    ) -> &'c serde_json::Value {
        let ends: Vec<_> = chains.iter().filter(|(l, _, _)| l == literal).collect();
        assert_eq!(ends.len(), 1, "one chain for {literal}: {chains:#?}");
        &ends[0].2
    }

    /// A binding set after it is declared, or one whose members the body
    /// writes, holds no longer what it was declared with: the chain stops at
    /// `reassigned` rather than reading the declaration's value.
    #[test]
    fn a_binding_set_later_or_written_through_ends_reassigned() {
        let chains = chains_in(&[(
            "src/server.ts",
            r#"import { createServer } from "node:http";

createServer((req, res) => {
  let url: URL;
  try {
    url = new URL(req.url ?? "/", "http://localhost");
  } catch {
    return;
  }
  if (url.pathname === "/late") {
    res.end("late");
  }
});

export function rewritten(path: string): boolean {
  const url = new URL("https://placeholder.invalid");
  url.pathname = path;
  return url.pathname.startsWith("/written");
}
"#,
        )]);
        for literal in ["/late", "/written"] {
            let end = end_of(&chains, literal);
            assert_eq!(end["kind"], "hole", "{literal}: {end}");
            assert_eq!(end["reason"], "reassigned", "{literal}: {end}");
        }
    }

    /// `new URL(x, base)` states the path `x` names only where `x` names one
    /// itself: a relative literal resolves against the base, so no path is
    /// stated; a literal path is.
    #[test]
    fn a_relative_url_literal_states_no_path() {
        let chains = chains_in(&[(
            "src/base.ts",
            r#"export function atRoot(href: string): boolean {
  const pathname = new URL(".", href).pathname;
  return pathname === "/";
}

export function fixed(href: string): boolean {
  const pathname = new URL("/fixed", href).pathname;
  return pathname === "/fixed";
}
"#,
        )]);
        let relative = end_of(&chains, "/");
        assert_eq!(relative["kind"], "hole", "{relative}");
        assert_eq!(relative["reason"], "unread_expression", "{relative}");
        assert_eq!(
            end_of(&chains, "/fixed"),
            &serde_json::json!({ "kind": "literal", "text": "/fixed", "steps": ["URL", "pathname"] })
        );
    }

    /// A function handed on by name inside an object argument is followed
    /// to the call, as one written there is, with the keys down to it.
    #[test]
    fn a_function_named_inside_an_object_argument_is_handed_to_the_call() {
        // The package is declared: an undeclared one may be a module of the
        // service, and ends at the in-repo import hole (carrick#2406).
        let manifest = r#"{ "name": "probe", "dependencies": { "invented-logger": "1.0.0" } }"#;
        let chains = chains_with(
            manifest,
            &[(
                "src/log.ts",
                r#"import { logger } from "invented-logger";

function skipHealth(req: { path: string }): boolean {
  return req.path === "/health";
}

logger({ skip: skipHealth });
logger({ skipHealth });
"#,
            )],
        );
        let mut paths: Vec<serde_json::Value> = chains
            .iter()
            .map(|(literal, hops, end)| {
                assert_eq!(literal, "/health");
                assert_eq!(
                    end,
                    &serde_json::json!({
                        "kind": "external_import", "specifier": "invented-logger",
                        "export": "logger", "steps": []
                    })
                );
                hops[0]["path"].clone()
            })
            .collect();
        paths.sort_by_key(|path| path.to_string());
        assert_eq!(
            paths,
            vec![
                serde_json::json!(["skip"]),
                serde_json::json!(["skipHealth"])
            ]
        );
    }

    /// A binding declared inside a block of the body is read by the body's
    /// rule, so a guard on it reaches where the request comes from; one that
    /// holds a function is handed on by its name.
    #[test]
    fn a_binding_declared_in_a_block_is_read_like_one_in_the_body() {
        let chains = chains_in(&[(
            "src/server.ts",
            r#"import { createServer } from "node:http";

createServer((req, res) => {
  if (req.method === "GET") {
    const { pathname } = new URL(req.url ?? "/", "http://localhost");
    if (pathname === "/inner") {
      res.end("inner");
    }
  }
});

export function register(ready: boolean): void {
  if (ready) {
    const onRequest = (req: { url: string }) => req.url === "/hooked";
    createServer(onRequest);
  }
}
"#,
        )]);
        let (_, hops, end) = chains
            .iter()
            .find(|(literal, _, _)| literal == "/inner")
            .expect("a chain for /inner");
        assert_eq!(
            hops[0]["steps"],
            serde_json::json!(["url", "URL", "pathname"])
        );
        assert_eq!(
            end_of(&chains, "/hooked"),
            &serde_json::json!({
                "kind": "external_import", "specifier": "node:http",
                "export": "createServer", "steps": []
            })
        );
        assert_eq!(
            end,
            &serde_json::json!({
                "kind": "external_import", "specifier": "node:http",
                "export": "createServer", "steps": []
            })
        );
    }

    /// Of a comparison's two operands, written text is the literal over a
    /// value read through an import, and of two texts the route path is: the
    /// other side is the value compared, as before names were read.
    #[test]
    fn text_is_the_literal_side_over_an_imported_value() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{ "name": "probe", "dependencies": { "invented-config": "1.0.0" } }"#,
        )
        .expect("the manifest is writable");
        std::fs::create_dir_all(dir.path().join("src")).expect("the tree");
        std::fs::write(
            dir.path().join("src/mode.ts"),
            r#"import { config } from "invented-config";
import { HOME } from "invented-config";

export function mode(): string {
  if (config.mode === "dev") {
    return "dev";
  }
  if (HOME === "/home") {
    return "home";
  }
  return "prod";
}
"#,
        )
        .expect("the source is writable");
        let inputs = crate::engine::discover_request_inputs(dir.path());
        let chains = guard_chains(&inputs, dir.path());
        let read: Vec<(&str, GuardShape, &str, serde_json::Value)> = chains
            .iter()
            .map(|chain| {
                (
                    chain.literal.as_str(),
                    chain.shape,
                    chain.subject.as_str(),
                    serde_json::to_value(&chain.end).expect("ends serialise"),
                )
            })
            .collect();
        assert_eq!(
            read,
            vec![
                (
                    "dev",
                    GuardShape::Name,
                    "config.mode",
                    serde_json::json!({
                        "kind": "external_import", "specifier": "invented-config",
                        "export": "config", "steps": ["mode"]
                    })
                ),
                (
                    "/home",
                    GuardShape::Path,
                    "HOME",
                    serde_json::json!({
                        "kind": "external_import", "specifier": "invented-config",
                        "export": "HOME", "steps": []
                    })
                ),
            ]
        );
    }

    /// A builder called with text states the text it returns. One that
    /// reads as no text is kept as written, `unresolved`: what it returns is
    /// for the model to read, and silence would hide the guard. One handed a
    /// parameter is the caller's value, so the comparison states no guard.
    #[test]
    fn a_builder_that_reads_as_no_text_is_kept_unresolved() {
        let chains = chains_in(&[
            (
                "src/topics.ts",
                r#"export function topicFor(name: string): string {
  return `orders.${name}`;
}
"#,
            ),
            (
                "src/route.ts",
                r#"import { topicFor } from "./topics";
import { lookup } from "./lookup";

export function route(event: { topic: string }, kind: string): boolean {
  if (event.topic === topicFor("created")) {
    return true;
  }
  if (event.topic === lookup("updated")) {
    return true;
  }
  return event.topic === topicFor(kind);
}
"#,
            ),
            (
                "src/lookup.ts",
                r#"export function lookup(name: string): string {
  return name.length > 3 ? name : "short";
}
"#,
            ),
        ]);
        let literals: Vec<&str> = chains
            .iter()
            .map(|(literal, _, _)| literal.as_str())
            .collect();
        assert_eq!(
            literals,
            vec!["orders.created", r#"lookup("updated")"#],
            "{chains:#?}"
        );
    }

    /// A `typeof` test compares a type tag, not a value the code was handed,
    /// so it states no guard; the name compared beside it does.
    #[test]
    fn a_typeof_test_states_no_guard() {
        let chains = chains_in(&[(
            "src/check.ts",
            r#"export function check(value: unknown, message: { type: string }): boolean {
  if (typeof value === "string") {
    return true;
  }
  switch (typeof value) {
    case "number":
      return true;
  }
  return message.type === "ready";
}
"#,
        )]);
        let literals: Vec<&str> = chains
            .iter()
            .map(|(literal, _, _)| literal.as_str())
            .collect();
        assert_eq!(literals, vec!["ready"], "{chains:#?}");
    }

    /// A hole states the text it stopped at: a field of `this`, not
    /// `this`, and what a call returns.
    #[test]
    fn a_hole_states_the_text_it_stopped_at() {
        let chains = chains_in(&[(
            "src/feed.ts",
            r#"import { EventEmitter } from "node:events";

function connect(): EventEmitter {
  return new EventEmitter();
}

export class Feed {
  private events = new EventEmitter();

  listen(): void {
    this.events.on("item", (item: { kind: string }) => {
      if (item.kind === "added") {
        console.log(item);
      }
    });
    const socket = connect();
    socket.on("frame", (frame: { op: string }) => {
      if (frame.op === "ping") {
        console.log(frame);
      }
    });
  }
}
"#,
        )]);
        for (literal, text) in [("added", "this.events"), ("ping", "connect()")] {
            let end = end_of(&chains, literal);
            assert_eq!(end["kind"], "hole", "{literal}: {end}");
            assert_eq!(end["reason"], "callee_unread", "{literal}: {end}");
            assert_eq!(end["text"], text, "{literal}: {end}");
        }
    }

    /// A measurement, not a check: every chain the guards state over each
    /// root, read as one service with the default config, with the literals
    /// in comparison positions the pass read no guard for. Counts are by
    /// shape, by end, by hole reason and by the text a hole stops at, table
    /// keys apart. Offline and model-free.
    ///
    /// `CARRICK_GUARD_CENSUS=<root>[:<root>…] CARRICK_GUARD_CENSUS_OUT=<file>
    /// cargo test --lib request_summary::guards::tests::census -- --ignored --nocapture`
    #[test]
    #[ignore = "a measurement over the roots named by CARRICK_GUARD_CENSUS"]
    fn census() {
        use std::collections::BTreeMap;
        use std::io::Write;

        let Ok(roots) = std::env::var("CARRICK_GUARD_CENSUS") else {
            return;
        };
        let mut out = std::env::var("CARRICK_GUARD_CENSUS_OUT")
            .ok()
            .map(|path| std::fs::File::create(path).expect("the census output is writable"));
        for root in roots.split(':').filter(|root| !root.is_empty()) {
            let root = Path::new(root);
            let inputs = crate::engine::discover_request_inputs(root);
            let chains = guard_chains(&inputs, root);

            // The denominators: every literal in a comparison position, and
            // the route-shaped ones among them.
            let mut denominator: Vec<(String, u32, String, &'static str)> = Vec::new();
            let mut files: Vec<&PathBuf> = inputs.files.keys().collect();
            files.sort();
            for file in files {
                let Ok(content) = std::fs::read_to_string(file) else {
                    continue;
                };
                let Some((source_map, module)) =
                    crate::swc_scanner::parse_standalone_module(file, &content)
                else {
                    continue;
                };
                for found in crate::compared_route_literals::compared_literals(&module, &|_| true) {
                    let line = source_map.lookup_char_pos(found.span.lo).line as u32;
                    let position = match found.position {
                        crate::compared_route_literals::ComparisonPosition::Equality => "equality",
                        crate::compared_route_literals::ComparisonPosition::Case => "case",
                        crate::compared_route_literals::ComparisonPosition::StartsWith => {
                            "starts_with"
                        }
                    };
                    let rel = file
                        .strip_prefix(root)
                        .unwrap_or(file)
                        .to_string_lossy()
                        .into_owned();
                    denominator.push((rel, line, found.value, position));
                }
            }
            let read: HashSet<(String, u32, String)> = chains
                .iter()
                .map(|chain| {
                    (
                        chain.site.file.clone(),
                        chain.site.line,
                        chain.literal.clone(),
                    )
                })
                .collect();
            let unread: Vec<_> = denominator
                .iter()
                .filter(|(file, line, value, _)| {
                    !read.contains(&(file.clone(), *line, value.clone()))
                })
                .collect();
            let routes = denominator
                .iter()
                .filter(|(_, _, value, _)| is_producer_route_path(value))
                .count();
            let routes_unread = unread
                .iter()
                .filter(|(_, _, value, _)| is_producer_route_path(value))
                .count();

            // Each count is kept twice: table keys apart from the rest,
            // since one table multiplies by its callers.
            #[derive(Default, Debug)]
            struct Counts {
                guards: HashSet<(String, u32, String)>,
                chains: usize,
                shapes: BTreeMap<String, usize>,
                ends: BTreeMap<&'static str, usize>,
                holes: BTreeMap<String, usize>,
                hole_texts: BTreeMap<&'static str, usize>,
                within: usize,
                hop_args: usize,
            }
            let name = |value: serde_json::Value| value.as_str().unwrap_or_default().to_string();
            let text_kind = |text: &str| {
                if text.starts_with("this.") {
                    "this_field"
                } else if text.starts_with("new ") {
                    "construction"
                } else if text.starts_with("await ") {
                    "await"
                } else if text.ends_with(')') {
                    "call_result"
                } else if text.ends_with(']') {
                    "computed_member"
                } else if text
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
                {
                    "binding"
                } else {
                    "other"
                }
            };
            let (mut rest, mut tables) = (Counts::default(), Counts::default());
            for chain in &chains {
                let counts = if chain.position == GuardPosition::TableKey {
                    &mut tables
                } else {
                    &mut rest
                };
                counts.guards.insert((
                    chain.site.file.clone(),
                    chain.site.line,
                    chain.literal.clone(),
                ));
                counts.chains += 1;
                *counts
                    .shapes
                    .entry(name(serde_json::json!(chain.shape)))
                    .or_default() += 1;
                let kind = match &chain.end {
                    ChainEnd::EntryPoint { .. } => "entry_point",
                    ChainEnd::Literal { .. } => "literal",
                    ChainEnd::Env { .. } => "env",
                    ChainEnd::Global { .. } => "global",
                    ChainEnd::ExternalImport { .. } => "external_import",
                    ChainEnd::Hole { reason, text, .. } => {
                        *counts
                            .holes
                            .entry(name(serde_json::json!(reason)))
                            .or_default() += 1;
                        let kind = text.as_deref().map_or("none", text_kind);
                        *counts.hole_texts.entry(kind).or_default() += 1;
                        "hole"
                    }
                };
                *counts.ends.entry(kind).or_default() += 1;
                counts.within += usize::from(!chain.within.is_empty());
                counts.hop_args += usize::from(
                    chain
                        .hops
                        .iter()
                        .any(|hop| matches!(hop, Hop::HandedTo { args, .. } if !args.is_empty())),
                );
            }
            println!(
                "census {}: {} compared literal(s) ({} route), {} unread ({} route); starts: guard {}",
                root.display(),
                denominator.len(),
                routes,
                unread.len(),
                routes_unread,
                chains.len(),
            );
            for (label, counts) in [("guards", &rest), ("table keys", &tables)] {
                println!(
                    "  {label}: {} read, {} chain(s), {} with within, {} with hop args",
                    counts.guards.len(),
                    counts.chains,
                    counts.within,
                    counts.hop_args
                );
                println!("    shapes: {:?}", counts.shapes);
                println!("    ends: {:?}", counts.ends);
                println!("    holes: {:?}", counts.holes);
                println!("    hole texts: {:?}", counts.hole_texts);
            }

            // The named calls, by start, an inert emission apart from the
            // rest.
            let calls = named_calls(&inputs, root);
            #[derive(Default, Debug)]
            struct CallCounts {
                calls: usize,
                chains: usize,
                spread: usize,
                library: usize,
                shapes: BTreeMap<String, usize>,
                from: BTreeMap<String, usize>,
                ends: BTreeMap<&'static str, usize>,
                holes: BTreeMap<String, usize>,
                hole_texts: BTreeMap<&'static str, usize>,
            }
            let mut starts: BTreeMap<&'static str, CallCounts> = BTreeMap::new();
            for call in &calls {
                let start = match (call.start, call.inert) {
                    (CallStart::Registration, _) => "registration",
                    (CallStart::Emission, false) => "emission",
                    (CallStart::Emission, true) => "emission (inert)",
                    (CallStart::Construction, _) => "construction",
                };
                let counts = starts.entry(start).or_default();
                counts.calls += 1;
                counts.chains += call.chains.len();
                counts.spread += usize::from(call.spread);
                counts.library += usize::from(!call.library.is_empty());
                for named in &call.names {
                    *counts
                        .shapes
                        .entry(name(serde_json::json!(named.shape)))
                        .or_default() += 1;
                    *counts
                        .from
                        .entry(name(serde_json::json!(named.from)))
                        .or_default() += 1;
                }
                for chain in &call.chains {
                    let kind = match &chain.end {
                        ChainEnd::EntryPoint { .. } => "entry_point",
                        ChainEnd::Literal { .. } => "literal",
                        ChainEnd::Env { .. } => "env",
                        ChainEnd::Global { .. } => "global",
                        ChainEnd::ExternalImport { .. } => "external_import",
                        ChainEnd::Hole { reason, text, .. } => {
                            *counts
                                .holes
                                .entry(name(serde_json::json!(reason)))
                                .or_default() += 1;
                            let kind = text.as_deref().map_or("none", text_kind);
                            *counts.hole_texts.entry(kind).or_default() += 1;
                            "hole"
                        }
                    };
                    *counts.ends.entry(kind).or_default() += 1;
                }
            }
            println!("  named calls: {}", calls.len());
            for (start, counts) in &starts {
                println!(
                    "  {start}: {} call(s), {} chain(s), {} spread, {} with a library receiver",
                    counts.calls, counts.chains, counts.spread, counts.library
                );
                println!("    name shapes: {:?}", counts.shapes);
                println!("    names from: {:?}", counts.from);
                println!("    ends: {:?}", counts.ends);
                println!("    holes: {:?}", counts.holes);
                println!("    hole texts: {:?}", counts.hole_texts);
            }

            // The coverage check: every socket and pub/sub row the
            // structural passes write should have a named call at its line.
            // No client package lists are known offline, so only the rows
            // the passes write with none are here.
            let mut service_files: Vec<PathBuf> = inputs.files.keys().cloned().collect();
            service_files.sort();
            let sockets = crate::socket_io::scan_files(&service_files, &[]);
            let modules = crate::workspace_resolver::WorkspaceIndex::build_with_aliases(root, None);
            let bus = crate::event_emitter::scan_files(&service_files, &sockets, &modules, &[]);
            let at_call: HashSet<(String, u32)> = calls
                .iter()
                .map(|call| (call.site.file.clone(), call.site.line))
                .collect();
            let rows = (sockets.listeners.iter())
                .map(|op| ("socket listener", &op.key, &op.file_path, op.line))
                .chain(
                    (sockets.emitters.iter())
                        .map(|op| ("socket emitter", &op.key, &op.file_path, op.line)),
                )
                .chain(
                    (bus.subscribers.iter())
                        .map(|op| ("pubsub subscriber", &op.key, &op.file_path, op.line)),
                )
                .chain(
                    (bus.publishers.iter())
                        .map(|op| ("pubsub publisher", &op.key, &op.file_path, op.line)),
                );
            let mut row_count = 0;
            let mut misses = Vec::new();
            for (kind, key, file, line) in rows {
                row_count += 1;
                let rel = file
                    .strip_prefix(root)
                    .unwrap_or(file)
                    .to_string_lossy()
                    .into_owned();
                if !at_call.contains(&(rel.clone(), line)) {
                    misses.push(serde_json::json!({
                        "kind": kind, "key": format!("{key:?}"), "file": rel, "line": line,
                    }));
                }
            }
            println!(
                "  coverage: {row_count} socket and pub/sub row(s), {} with no named call at their line",
                misses.len()
            );

            if let Some(out) = out.as_mut() {
                for chain in &chains {
                    let line = serde_json::json!({ "root": root, "chain": chain });
                    writeln!(out, "{line}").expect("the census output is writable");
                }
                for call in &calls {
                    let line = serde_json::json!({ "root": root, "call": call });
                    writeln!(out, "{line}").expect("the census output is writable");
                }
                for miss in &misses {
                    let line = serde_json::json!({ "root": root, "coverage_miss": miss });
                    writeln!(out, "{line}").expect("the census output is writable");
                }
                for (file, line, value, position) in &unread {
                    let line = serde_json::json!({
                        "root": root,
                        "unread": { "file": file, "line": line, "literal": value, "position": position },
                    });
                    writeln!(out, "{line}").expect("the census output is writable");
                }
            }
        }
    }

    /// The methods `binding` of `src` compares `request.method` against,
    /// the way a route module hands its handler `({ request })`.
    fn compared(src: &str, binding: &str) -> Vec<String> {
        let ir = crate::request_summary::file_ir_of(Path::new("app/routes/x.ts"), src)
            .expect("the source parses");
        compared_methods(
            &ir,
            binding,
            0,
            &["request".to_string(), "method".to_string()],
        )
    }

    fn verbs(list: &[&str]) -> Vec<String> {
        list.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn compared_methods_reads_a_negated_comparison() {
        // The probe: the subject of `({ request })` is Param(0) + [request, method].
        let src = r#"export async function action({ request }) {
            if (request.method !== "PUT") throw new Error("no");
        }"#;
        assert_eq!(compared(src, "action"), verbs(&["PUT"]));
    }

    #[test]
    fn compared_methods_reads_both_polarities_in_source_order() {
        let src = r#"export async function action({ request }) {
            if (request.method === "DELETE") { return 1; }
            if (request.method !== "PUT") throw new Error("no");
        }"#;
        assert_eq!(compared(src, "action"), verbs(&["DELETE", "PUT"]));
    }

    #[test]
    fn compared_methods_reads_a_destructured_local() {
        let src = r#"export async function action({ request }) {
            const { method } = request;
            if (method !== "GET") throw new Error("no");
        }"#;
        assert_eq!(compared(src, "action"), verbs(&["GET"]));
    }

    #[test]
    fn compared_methods_reads_a_local_holding_the_member() {
        let src = r#"export async function action({ request }) {
            const verb = request.method;
            if (verb === "DELETE") return 1;
        }"#;
        assert_eq!(compared(src, "action"), verbs(&["DELETE"]));
    }

    #[test]
    fn compared_methods_reads_through_string_methods_on_a_local() {
        let src = r#"export async function action({ request }) {
            const verb = request.method.toLowerCase().trim();
            if (verb === "patch") return 1;
        }"#;
        assert_eq!(compared(src, "action"), verbs(&["PATCH"]));
    }

    #[test]
    fn compared_methods_reads_every_case_of_a_switch() {
        let src = r#"export async function action({ request }) {
            switch (request.method.toUpperCase()) {
                case "PUT": return 1;
                case "DELETE": return 2;
                default: throw new Error("no");
            }
        }"#;
        assert_eq!(compared(src, "action"), verbs(&["PUT", "DELETE"]));
    }

    #[test]
    fn compared_methods_reads_an_arrow_const_export() {
        let src = r#"export const action = async ({ request }) => {
            if (request.method !== "PUT") throw new Error("no");
        };"#;
        assert_eq!(compared(src, "action"), verbs(&["PUT"]));
    }

    #[test]
    fn compared_methods_reads_a_closure_over_the_outer_request_in_source_order() {
        let src = r#"export async function action({ request }) {
            const check = () => { if (request.method === "PUT") return 1; };
            if (request.method === "DELETE") return check();
        }"#;
        assert_eq!(compared(src, "action"), verbs(&["PUT", "DELETE"]));
    }

    #[test]
    fn compared_methods_ignores_a_function_inside_with_its_own_request() {
        let src = r#"export async function action({ request }) {
            const other = ({ request }) => request.method === "PUT";
            return other;
        }"#;
        assert_eq!(compared(src, "action"), Vec::<String>::new());
    }

    #[test]
    fn compared_methods_ignores_another_member_of_the_parameter() {
        let src = r#"export async function action({ request, params }) {
            if (params.method === "GET") return 1;
        }"#;
        assert_eq!(compared(src, "action"), Vec::<String>::new());
    }

    #[test]
    fn compared_methods_reads_a_same_file_text_constant() {
        // Deliberate widening: the old collector read literals only.
        let src = r#"const PUT = "PUT";
        export async function action({ request }) {
            if (request.method !== PUT) throw new Error("no");
        }"#;
        assert_eq!(compared(src, "action"), verbs(&["PUT"]));
    }

    #[test]
    fn compared_methods_ignores_what_is_not_the_request_method() {
        for body in [
            r#"const body = await request.json(); if (body.kind !== "PUT") return 1;"#,
            r#"if (request.method !== "QUERY") return 1;"#,
            r#"if (request.method.slice(0, 3) === "PUT") return 1;"#,
        ] {
            let src = format!("export async function action({{ request }}) {{ {body} }}");
            assert_eq!(compared(&src, "action"), Vec::<String>::new(), "{body}");
        }
    }

    #[test]
    fn compared_methods_returns_options_for_the_router_to_drop() {
        let src = r#"export async function action({ request }) {
            if (request.method === "OPTIONS") return 1;
        }"#;
        assert_eq!(compared(src, "action"), verbs(&["OPTIONS"]));
    }

    #[test]
    fn compared_methods_reads_each_binding_by_itself() {
        let src = r#"export async function loader({ request }) { return 1; }
        export async function action({ request }) {
            if (request.method !== "PUT") throw new Error("no");
        }"#;
        assert_eq!(compared(src, "loader"), Vec::<String>::new());
        assert_eq!(compared(src, "action"), verbs(&["PUT"]));
    }

    #[test]
    fn compared_methods_abstains_on_a_built_export() {
        let src = r#"export const action = withAuth(async ({ request }) => {
            if (request.method !== "PATCH") throw new Error("no");
        });"#;
        assert_eq!(compared(src, "action"), Vec::<String>::new());
    }

    #[test]
    fn compared_methods_abstains_on_a_stringified_method() {
        let src = r#"export async function action({ request }) {
            if (String(request.method) !== "DELETE") throw new Error("no");
        }"#;
        assert_eq!(compared(src, "action"), Vec::<String>::new());
    }

    #[test]
    fn compared_methods_abstains_when_no_function_is_bound() {
        let src = r#"export const action = 1;"#;
        assert_eq!(compared(src, "action"), Vec::<String>::new());
        assert_eq!(compared(src, "missing"), Vec::<String>::new());
    }
}
