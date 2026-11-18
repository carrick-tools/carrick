//! Which calls in a function hand their value to what the function returns
//! (carrick#1146).
//!
//! A function that makes several requests answers its caller with one of
//! them: the one whose value reaches its `return`. A request awaited and
//! dropped (a probe before every call) or one whose value only goes into
//! another call's arguments (a token handed to the request that is returned)
//! is a side request: the caller never reads it.
//!
//! Read per function, from the syntax alone, inside the function's own body
//! (a function written inside it is read on its own):
//!
//! - every `return` argument and an arrow's expression body is returned;
//! - a returned value flows through `await`, parentheses, type assertions,
//!   member reads (`res.data`), a call's receiver (`res.json()`), both arms
//!   of a conditional, `??` / `||` / `&&`, object and array literals, the
//!   last expression of a sequence, a template's interpolations, and every
//!   value a local binding is initialised or assigned with;
//! - a call reached that way is RETURNED, and its arguments carry the
//!   returned value on to the calls inside them, which are recorded as
//!   reaching the return only THROUGH that call.
//!
//! Every other call is DISCARDED. Nothing here names a library or a symbol.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use swc_common::Span;
use swc_ecma_ast::{
    ArrowExpr, ArrowFunctionBody, AssignTarget, BinaryOp, CallExpr, Callee, Class, Expr, Function,
    MemberProp, OptChainBase, Pat, Prop, PropOrSpread, ReturnStmt, SimpleAssignTarget,
    VarDeclarator,
};
use swc_ecma_visit::{Visit, VisitWith};

/// How a call's value reaches the return of the function it is written in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flow {
    /// The function returns it, or something read off it.
    Returned,
    /// Only as an argument to these calls (their spans), each of which
    /// reaches the return.
    Through(Vec<Span>),
    /// Never: awaited and dropped, held and not returned, or nothing returns.
    Discarded,
}

/// Every call written in `module`, by its span, with how its value reaches
/// the return of the innermost function it is written in. A call at module
/// level is in no function and is not listed. In source order, so a reader
/// that keys two calls to one entry keeps the same one every run (carrick#2759).
pub fn call_flows(module: &swc_ecma_ast::Module) -> Vec<(Span, Flow)> {
    let mut reader = Reader::default();
    module.visit_with(&mut reader);
    let mut flows: Vec<(Span, Flow)> = reader.flows.into_iter().collect();
    flows.sort_by_key(|(span, _)| (span.lo, span.hi));
    flows
}

#[derive(Default)]
struct Reader {
    flows: HashMap<Span, Flow>,
}

impl Reader {
    fn read_body(&mut self, returned: Vec<&Expr>, body: &dyn VisitWithScope) {
        let mut scope = Scope::default();
        body.collect(&mut scope);
        let mut marks = Marks::default();
        for expr in returned {
            marks.flow(expr, None, &scope);
        }
        for span in scope.calls {
            let flow = if marks.returned.contains(&span) {
                Flow::Returned
            } else if let Some(through) = marks.through.get(&span) {
                let mut through: Vec<Span> = through.iter().copied().collect();
                through.sort_by_key(|span| (span.lo, span.hi));
                Flow::Through(through)
            } else {
                Flow::Discarded
            };
            self.flows.insert(span, flow);
        }
    }
}

impl Visit for Reader {
    fn visit_function(&mut self, function: &Function) {
        if let Some(body) = &function.body {
            let mut returns = Returns::default();
            body.visit_with(&mut returns);
            self.read_body(returns.exprs.iter().collect(), &BodyRef::Block(body));
        }
        function.visit_children_with(self);
    }

    fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
        match &*arrow.body {
            ArrowFunctionBody::FunctionBody(block) => {
                let mut returns = Returns::default();
                block.visit_with(&mut returns);
                self.read_body(returns.exprs.iter().collect(), &BodyRef::Block(block));
            }
            ArrowFunctionBody::Expr(expr) => {
                self.read_body(vec![&**expr], &BodyRef::Expr(expr));
            }
        }
        arrow.visit_children_with(self);
    }
}

/// A function body, as [`Scope::default`] reads it.
enum BodyRef<'a> {
    Block(&'a swc_ecma_ast::FunctionBody),
    Expr(&'a Expr),
}

trait VisitWithScope {
    fn collect(&self, scope: &mut Scope);
}

impl VisitWithScope for BodyRef<'_> {
    fn collect(&self, scope: &mut Scope) {
        match self {
            BodyRef::Block(block) => block.visit_with(scope),
            BodyRef::Expr(expr) => expr.visit_with(scope),
        }
    }
}

/// The `return` arguments of one body, never a nested function's.
#[derive(Default)]
struct Returns {
    exprs: Vec<Expr>,
}

impl Visit for Returns {
    fn visit_return_stmt(&mut self, stmt: &ReturnStmt) {
        if let Some(arg) = &stmt.arg {
            self.exprs.push((**arg).clone());
        }
        stmt.visit_children_with(self);
    }
    fn visit_function(&mut self, _: &Function) {}
    fn visit_arrow_expr(&mut self, _: &ArrowExpr) {}
    fn visit_class(&mut self, _: &Class) {}
}

/// One body's calls and what each local name is given, never a nested
/// function's.
#[derive(Default)]
struct Scope {
    calls: Vec<Span>,
    given: HashMap<String, Vec<Expr>>,
    /// The names bound by taking a value apart (`const { data } = await
    /// call()`), with the value they are read from: each is a part of it, not
    /// the value itself.
    parts: HashMap<String, Vec<Expr>>,
    /// For a name taken from an object pattern's own key (`{ data }`,
    /// `{ data: body }`), that key; a name bound deeper has none.
    part_keys: HashMap<String, String>,
}

impl Visit for Scope {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        self.calls.push(call.span);
        call.visit_children_with(self);
    }
    fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
        if let Some(init) = &declarator.init {
            let whole = matches!(declarator.name, Pat::Ident(_));
            self.part_keys.extend(object_keys(&declarator.name));
            for name in pattern_names(&declarator.name) {
                if !whole {
                    self.parts
                        .entry(name.clone())
                        .or_default()
                        .push((**init).clone());
                }
                self.given.entry(name).or_default().push((**init).clone());
            }
        }
        declarator.visit_children_with(self);
    }
    fn visit_assign_expr(&mut self, assign: &swc_ecma_ast::AssignExpr) {
        if let AssignTarget::Simple(SimpleAssignTarget::Ident(ident)) = &assign.left {
            self.given
                .entry(ident.id.sym.to_string())
                .or_default()
                .push((*assign.right).clone());
        }
        assign.visit_children_with(self);
    }
    fn visit_function(&mut self, _: &Function) {}
    fn visit_arrow_expr(&mut self, _: &ArrowExpr) {}
    fn visit_class(&mut self, _: &Class) {}
}

/// The names an object pattern binds straight off one of its own keys, with
/// that key: `data` for `{ data }`, `body` to `data` for `{ data: body }`.
fn object_keys(pat: &Pat) -> Vec<(String, String)> {
    let Pat::Object(object) = pat else {
        return Vec::new();
    };
    object
        .props
        .iter()
        .filter_map(|prop| match prop {
            swc_ecma_ast::ObjectPatProp::Assign(assign) => {
                let name = assign.key.id.sym.to_string();
                Some((name.clone(), name))
            }
            swc_ecma_ast::ObjectPatProp::KeyValue(kv) => {
                let Pat::Ident(ident) = &*kv.value else {
                    return None;
                };
                let key = match &kv.key {
                    swc_ecma_ast::PropName::Ident(key) => key.sym.to_string(),
                    swc_ecma_ast::PropName::Str(key) => key.value.to_string_lossy().into_owned(),
                    _ => return None,
                };
                Some((ident.id.sym.to_string(), key))
            }
            swc_ecma_ast::ObjectPatProp::Rest(_) => None,
        })
        .collect()
}

fn pattern_names(pat: &Pat) -> Vec<String> {
    match pat {
        Pat::Ident(ident) => vec![ident.id.sym.to_string()],
        Pat::Array(array) => array
            .elems
            .iter()
            .flatten()
            .flat_map(pattern_names)
            .collect(),
        Pat::Object(object) => object
            .props
            .iter()
            .flat_map(|prop| match prop {
                swc_ecma_ast::ObjectPatProp::KeyValue(kv) => pattern_names(&kv.value),
                swc_ecma_ast::ObjectPatProp::Assign(assign) => vec![assign.key.id.sym.to_string()],
                swc_ecma_ast::ObjectPatProp::Rest(rest) => pattern_names(&rest.arg),
            })
            .collect(),
        Pat::Assign(assign) => pattern_names(&assign.left),
        Pat::Rest(rest) => pattern_names(&rest.arg),
        _ => Vec::new(),
    }
}

/// The calls a body's returned values reach.
#[derive(Default)]
struct Marks {
    /// Reached as the returned value, or something read off it.
    returned: HashSet<Span>,
    /// Reached only inside these calls' arguments.
    through: HashMap<Span, HashSet<Span>>,
    /// Names already followed, per the call whose arguments they were met in.
    followed: HashSet<(String, Option<Span>)>,
}

impl Marks {
    /// Follow a value that reaches the return. `via`: the outermost call
    /// whose arguments it was met in, if any.
    fn flow(&mut self, expr: &Expr, via: Option<Span>, scope: &Scope) {
        match expr {
            Expr::Call(call) => {
                match via {
                    None => {
                        self.returned.insert(call.span);
                    }
                    Some(outer) => {
                        self.through.entry(call.span).or_default().insert(outer);
                    }
                }
                if let Callee::Expr(callee) = &call.callee {
                    self.flow_callee(callee, via, scope);
                }
                let via = via.or(Some(call.span));
                for arg in &call.args {
                    self.flow(&arg.expr, via, scope);
                }
            }
            Expr::OptChain(chain) => match &*chain.base {
                OptChainBase::Member(member) => self.flow(&member.obj, via, scope),
                OptChainBase::Call(call) => {
                    self.flow_callee(&call.callee, via, scope);
                    for arg in &call.args {
                        self.flow(&arg.expr, via.or(Some(chain.span)), scope);
                    }
                }
            },
            Expr::Await(awaited) => self.flow(&awaited.arg, via, scope),
            Expr::Paren(paren) => self.flow(&paren.expr, via, scope),
            Expr::TsAs(cast) => self.flow(&cast.expr, via, scope),
            Expr::TsTypeAssertion(cast) => self.flow(&cast.expr, via, scope),
            Expr::TsConstAssertion(cast) => self.flow(&cast.expr, via, scope),
            Expr::TsNonNull(cast) => self.flow(&cast.expr, via, scope),
            Expr::TsSatisfies(cast) => self.flow(&cast.expr, via, scope),
            Expr::Member(member) => self.flow(&member.obj, via, scope),
            Expr::Cond(cond) => {
                self.flow(&cond.cons, via, scope);
                self.flow(&cond.alt, via, scope);
            }
            Expr::Bin(bin)
                if matches!(
                    bin.op,
                    BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::NullishCoalescing
                ) =>
            {
                self.flow(&bin.left, via, scope);
                self.flow(&bin.right, via, scope);
            }
            Expr::Object(object) => {
                for prop in &object.props {
                    match prop {
                        PropOrSpread::Spread(spread) => self.flow(&spread.expr, via, scope),
                        PropOrSpread::Prop(prop) => match &**prop {
                            Prop::KeyValue(kv) => self.flow(&kv.value, via, scope),
                            Prop::Shorthand(ident) => self.follow(&ident.sym, via, scope),
                            _ => {}
                        },
                    }
                }
            }
            Expr::Array(array) => {
                for elem in array.elems.iter().flatten() {
                    self.flow(&elem.expr, via, scope);
                }
            }
            Expr::Seq(seq) => {
                if let Some(last) = seq.exprs.last() {
                    self.flow(last, via, scope);
                }
            }
            Expr::Tpl(tpl) => {
                for expr in &tpl.exprs {
                    self.flow(expr, via, scope);
                }
            }
            Expr::New(new) => {
                for arg in new.args.iter().flatten() {
                    self.flow(&arg.expr, via, scope);
                }
            }
            Expr::Assign(assign) => self.flow(&assign.right, via, scope),
            Expr::Ident(ident) => self.follow(&ident.sym, via, scope),
            _ => {}
        }
    }

    /// A call's receiver is what the call reads: `res.json()` reads `res`.
    fn flow_callee(&mut self, callee: &Expr, via: Option<Span>, scope: &Scope) {
        if let Expr::Member(member) = callee {
            self.flow(&member.obj, via, scope);
        }
    }

    fn follow(&mut self, name: &str, via: Option<Span>, scope: &Scope) {
        if !self.followed.insert((name.to_string(), via)) {
            return;
        }
        for given in scope.given.get(name).into_iter().flatten() {
            self.flow(given, via, scope);
        }
    }
}

/// How much of a request's value a function hands back (carrick#2377).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    /// A call's value, whole: `return this.send(() => sdk(..))`.
    Value,
    /// The parse of a call's value: `return response.json()`.
    Parsed,
    /// One member of a call's value: `return (await call()).data`.
    Member,
}

/// What a function returns, when that is one call's value read one way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnShape {
    pub kind: ShapeKind,
    /// The call whose value is read.
    pub base: Span,
    /// For [`ShapeKind::Member`], the member read (`data` in `res.data`, or
    /// the key a destructured name is taken from, `const { data } = …`);
    /// `None` where the source names none (an array pattern). Read by
    /// [`hands_back_body`]'s steps (carrick#4371). In memory only.
    pub member: Option<String>,
}

impl ReturnShape {
    /// The read this shape makes off its base call's value, as a
    /// [`hands_back_body`] step: none for a whole value.
    pub fn step(&self) -> Option<Step> {
        match self.kind {
            ShapeKind::Value => None,
            // `is_parse` reads `.json()` alone.
            ShapeKind::Parsed => Some(Step::Parse("json".to_string())),
            ShapeKind::Member => Some(Step::Member(self.member.clone().unwrap_or_default())),
        }
    }
}

/// One read a function makes off a returned call's value on its way back to
/// its caller (carrick#4371): the parse by name, or one member by name. An
/// unnamed member is the empty name, which no label names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Step {
    Parse(String),
    Member(String),
}

/// What is known of the call a chain of returns ends at, for
/// [`hands_back_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndRead {
    /// The store's verified result label for the call (carrick#2819).
    Labelled(crate::services::type_sidecar::ResultShape),
    /// No verified label: what the call resolves to is not stated.
    Unlabelled,
    /// The platform's `fetch`, with no label: it resolves to a `Response`,
    /// whose body is its `.json()`.
    PlatformFetch,
}

/// Whether a chain of returns hands its caller the body ([`hands_back_body`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HandBack {
    /// The caller holds the response body: read it at the caller's call.
    Body,
    /// The caller holds something else (an envelope, a field, a flag): the
    /// response is read where the request is made.
    Other,
    /// A member is read off a call no verified label describes: whether
    /// that member is the body is not known (READY-4371 R10).
    Undecided,
}

/// The one test of whether the reads `steps` make off the value of the call
/// a chain of returns ends at hand back that call's response body
/// (carrick#4371 R2). The call's result is described by `end`:
///
/// | `end` | body when `steps` is |
/// |---|---|
/// | labelled `member: [m]` | `[Member(m)]` |
/// | labelled `parse: p` | `[Parse(p)]` |
/// | labelled `whole`, or unlabelled | `[]` |
/// | the platform `fetch`, unlabelled | `[Parse("json")]` |
///
/// Anything else is [`HandBack::Other`], except:
///
/// - a member read over an unlabelled end is [`HandBack::Undecided`]: the
///   label decides it (READY-4371 R10);
/// - the platform `fetch`'s whole value is [`HandBack::Undecided`] too. It
///   resolves to a `Response`, which the labelled call's own read cannot
///   type (`machinery_envelope`); whoever holds it reads the body off it
///   (`.json()`, `.arrayBuffer()`), and the deciding call is where that
///   read is followed. A member of a `Response` (`ok`, `status`) is never
///   its body: other.
///
/// Its readers: the site pass, which reads a wrapper site's response at the
/// labelled call unless the wrapper hands the body back
/// ([`crate::request_summary::site_record`]'s payload slots). The request
/// summaries' flags and call-through's walk take it next (READY-4371 T2,
/// T3), so the question has this one answer.
pub fn hands_back_body(steps: &[Step], end: &EndRead) -> HandBack {
    use crate::services::type_sidecar::ResultShape;
    let body = match (end, steps) {
        (EndRead::Labelled(ResultShape::Member([wanted])), [Step::Member(read)]) => read == wanted,
        (EndRead::Labelled(ResultShape::Parse(wanted)), [Step::Parse(read)]) => read == wanted,
        (EndRead::Labelled(ResultShape::Whole(_)) | EndRead::Unlabelled, []) => true,
        (EndRead::PlatformFetch, [Step::Parse(read)]) => read == "json",
        _ => false,
    };
    if body {
        return HandBack::Body;
    }
    let reads_member = steps.iter().any(|step| matches!(step, Step::Member(_)));
    match end {
        EndRead::Unlabelled if reads_member => HandBack::Undecided,
        EndRead::PlatformFetch if steps.is_empty() => HandBack::Undecided,
        _ => HandBack::Other,
    }
}

/// Every function of `module`, by the span of its node, with what it returns:
/// `Some` where every `return` but the ones that hand back no body
/// (`undefined`, `null`, `void 0`, a bare `return;`) is one and the same call's
/// value, whole, parsed or one member of it. Several returns are one shape
/// when each hands back the same call's value whole, or each its parse.
/// `None` for anything else: an object or array, a template, an operator, a
/// conditional, two members read, two member returns, returns that read
/// different calls or read one call differently, or none.
/// In source order.
///
/// A returned name is followed to the one value it is given: its
/// initialiser, or the single assignment of a name declared without one.
///
/// Two readers ask it, so "does this function hand back its request's body"
/// has one answer (carrick#3823): [`crate::call_through`] for model rows,
/// and the request summaries' `passes_body` for summary rows.
pub fn return_shapes(module: &swc_ecma_ast::Module) -> Vec<(Span, Option<ReturnShape>)> {
    let mut reader = ShapeReader::default();
    module.visit_with(&mut reader);
    reader.shapes.sort_by_key(|(span, _)| (span.lo, span.hi));
    reader.shapes
}

#[derive(Default)]
struct ShapeReader {
    shapes: Vec<(Span, Option<ReturnShape>)>,
}

impl ShapeReader {
    fn read(&mut self, node: Span, returned: Vec<&Expr>, body: &dyn VisitWithScope) {
        let mut scope = Scope::default();
        body.collect(&mut scope);
        self.shapes.push((node, shape_of(&returned, &scope)));
    }
}

impl Visit for ShapeReader {
    fn visit_function(&mut self, function: &Function) {
        if let Some(body) = &function.body {
            let mut returns = Returns::default();
            body.visit_with(&mut returns);
            self.read(
                function.span,
                returns.exprs.iter().collect(),
                &BodyRef::Block(body),
            );
        }
        function.visit_children_with(self);
    }

    fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
        match &*arrow.body {
            ArrowFunctionBody::FunctionBody(block) => {
                let mut returns = Returns::default();
                block.visit_with(&mut returns);
                self.read(
                    arrow.span,
                    returns.exprs.iter().collect(),
                    &BodyRef::Block(block),
                );
            }
            ArrowFunctionBody::Expr(expr) => {
                self.read(arrow.span, vec![&**expr], &BodyRef::Expr(expr));
            }
        }
        arrow.visit_children_with(self);
    }
}

/// Every object literal in `module` that is a function's return value, as
/// the span `(lo, hi)` of the literal (carrick#3470): a `return` argument or
/// an arrow's expression body, through the wrappers that do not change it
/// and a local name given that one value, read as [`return_shapes`] reads a
/// returned name. A literal taken apart (`const { a } = {...}`) or nested in
/// another value is not one.
pub fn returned_objects(module: &swc_ecma_ast::Module) -> HashSet<(u32, u32)> {
    #[derive(Default)]
    struct ObjectReader {
        objects: HashSet<(u32, u32)>,
    }
    impl ObjectReader {
        fn read(&mut self, returned: Vec<&Expr>, body: &dyn VisitWithScope) {
            let mut scope = Scope::default();
            body.collect(&mut scope);
            for expr in returned {
                let value = resolve(expr, &scope);
                if let (Expr::Object(object), false) = (value.expr, value.part) {
                    self.objects.insert((object.span.lo.0, object.span.hi.0));
                }
            }
        }
    }
    impl Visit for ObjectReader {
        fn visit_function(&mut self, function: &Function) {
            if let Some(body) = &function.body {
                let mut returns = Returns::default();
                body.visit_with(&mut returns);
                self.read(returns.exprs.iter().collect(), &BodyRef::Block(body));
            }
            function.visit_children_with(self);
        }

        fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
            match &*arrow.body {
                ArrowFunctionBody::FunctionBody(block) => {
                    let mut returns = Returns::default();
                    block.visit_with(&mut returns);
                    self.read(returns.exprs.iter().collect(), &BodyRef::Block(block));
                }
                ArrowFunctionBody::Expr(expr) => self.read(vec![&**expr], &BodyRef::Expr(expr)),
            }
            arrow.visit_children_with(self);
        }
    }
    let mut reader = ObjectReader::default();
    module.visit_with(&mut reader);
    reader.objects
}

/// The calls whose value a function may hand back (carrick#4270), by how it
/// hands them back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeldReturn {
    /// Calls whose value may be the function's value whole.
    pub bare: Vec<Span>,
    /// Calls whose value may sit inside the function's value: a member, an
    /// element, an argument of a call or `new`.
    pub held: Vec<Span>,
}

/// Every function of `module`, by the span of its node, with the calls whose
/// value it may hand back, whole or held inside its value (carrick#4270).
/// In source order.
///
/// Read from the syntax alone. Every `return` argument and an arrow's
/// expression body is walked, keeping its position (whole or held) through
/// `await`, parentheses, type assertions, both arms of a conditional, `??` /
/// `||` / `&&`, the last expression of a sequence, an assignment's value and
/// every value a local name is given. What sits under an object literal's
/// member, shorthand or spread, an array's element, or an argument of a call
/// or `new` is held. A method call's receiver keeps the call's position (a
/// call made on a value is a way of handing it on: `res.clone()`), and so
/// does what an inline callback handed to a call returns, whose first
/// parameter is the receiver's value (`fetch(u).then((res) => ({ res }))`).
/// A member read, a body read ([`BODY_READS`]), a name taken apart
/// (`const { data } = ...`), a template or any other operator ends the walk:
/// none of them hands the value on.
pub fn held_returns(module: &swc_ecma_ast::Module) -> Vec<(Span, HeldReturn)> {
    #[derive(Default)]
    struct HeldReader {
        held: Vec<(Span, HeldReturn)>,
    }
    impl HeldReader {
        fn read(&mut self, node: Span, returned: Vec<&Expr>, body: &dyn VisitWithScope) {
            let mut scope = Scope::default();
            body.collect(&mut scope);
            let frame = Frame {
                id: 0,
                scope,
                params: Vec::new(),
                receiver: None,
                parent: None,
            };
            let mut walk = HeldWalk::default();
            for expr in returned {
                walk.walk(expr, false, &frame);
            }
            self.held.push((node, walk.finish()));
        }
    }
    impl Visit for HeldReader {
        fn visit_function(&mut self, function: &Function) {
            if let Some(body) = &function.body {
                let mut returns = Returns::default();
                body.visit_with(&mut returns);
                self.read(
                    function.span,
                    returns.exprs.iter().collect(),
                    &BodyRef::Block(body),
                );
            }
            function.visit_children_with(self);
        }

        fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
            match &*arrow.body {
                ArrowFunctionBody::FunctionBody(block) => {
                    let mut returns = Returns::default();
                    block.visit_with(&mut returns);
                    self.read(
                        arrow.span,
                        returns.exprs.iter().collect(),
                        &BodyRef::Block(block),
                    );
                }
                ArrowFunctionBody::Expr(expr) => {
                    self.read(arrow.span, vec![&**expr], &BodyRef::Expr(expr));
                }
            }
            arrow.visit_children_with(self);
        }
    }
    let mut reader = HeldReader::default();
    module.visit_with(&mut reader);
    reader.held.sort_by_key(|(span, _)| (span.lo, span.hi));
    reader.held
}

/// The platform's reads of a response's body (`res.json()`, `res.text()`):
/// what they hand back is the body, never the response. The compiler
/// engine's list (platform-body-read.ts).
const BODY_READS: [&str; 6] = ["json", "text", "blob", "arrayBuffer", "formData", "bytes"];

/// One function body being walked by [`HeldWalk`]: its own names, and for an
/// inline callback, the body it is written in.
struct Frame<'p> {
    /// Tells two frames' names apart in [`HeldWalk::followed`].
    id: usize,
    scope: Scope,
    /// A callback's own parameter names: never the enclosing body's names.
    params: Vec<String>,
    /// For a callback handed to a method call: its first parameter's name and
    /// the receiver whose value it is given, read in `parent`.
    receiver: Option<(String, &'p Expr)>,
    parent: Option<&'p Frame<'p>>,
}

/// The calls one function's returned values reach ([`held_returns`]).
#[derive(Default)]
struct HeldWalk {
    bare: Vec<Span>,
    held: Vec<Span>,
    /// Names already followed, per frame and position.
    followed: HashSet<(String, usize, bool)>,
    frames: usize,
}

impl HeldWalk {
    fn finish(mut self) -> HeldReturn {
        for spans in [&mut self.bare, &mut self.held] {
            spans.sort_by_key(|span| (span.lo, span.hi));
            spans.dedup();
        }
        HeldReturn {
            bare: self.bare,
            held: self.held,
        }
    }

    /// Walk a value the function may hand back: whole, or `held` inside it.
    fn walk(&mut self, expr: &Expr, held: bool, frame: &Frame) {
        match expr {
            Expr::Await(awaited) => self.walk(&awaited.arg, held, frame),
            Expr::Paren(paren) => self.walk(&paren.expr, held, frame),
            Expr::TsAs(cast) => self.walk(&cast.expr, held, frame),
            Expr::TsTypeAssertion(cast) => self.walk(&cast.expr, held, frame),
            Expr::TsConstAssertion(cast) => self.walk(&cast.expr, held, frame),
            Expr::TsNonNull(cast) => self.walk(&cast.expr, held, frame),
            Expr::TsSatisfies(cast) => self.walk(&cast.expr, held, frame),
            Expr::Cond(cond) => {
                self.walk(&cond.cons, held, frame);
                self.walk(&cond.alt, held, frame);
            }
            Expr::Bin(bin)
                if matches!(
                    bin.op,
                    BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::NullishCoalescing
                ) =>
            {
                self.walk(&bin.left, held, frame);
                self.walk(&bin.right, held, frame);
            }
            Expr::Seq(seq) => {
                if let Some(last) = seq.exprs.last() {
                    self.walk(last, held, frame);
                }
            }
            Expr::Assign(assign) => self.walk(&assign.right, held, frame),
            Expr::Ident(ident) => self.follow(&ident.sym, held, frame),
            Expr::Object(object) => {
                for prop in &object.props {
                    match prop {
                        PropOrSpread::Spread(spread) => self.walk(&spread.expr, true, frame),
                        PropOrSpread::Prop(prop) => match &**prop {
                            Prop::KeyValue(kv) => self.walk(&kv.value, true, frame),
                            Prop::Shorthand(ident) => self.follow(&ident.sym, true, frame),
                            _ => {}
                        },
                    }
                }
            }
            Expr::Array(array) => {
                for elem in array.elems.iter().flatten() {
                    self.walk(&elem.expr, true, frame);
                }
            }
            Expr::New(new) => {
                for arg in new.args.iter().flatten() {
                    self.walk(&arg.expr, true, frame);
                }
            }
            Expr::Call(call) => self.call(call, held, frame),
            _ => {}
        }
    }

    fn call(&mut self, call: &CallExpr, held: bool, frame: &Frame) {
        let Callee::Expr(callee) = &call.callee else {
            return;
        };
        let receiver = match &**callee {
            Expr::Member(member) => {
                if matches!(&member.prop, MemberProp::Ident(name) if BODY_READS.contains(&&*name.sym))
                {
                    return;
                }
                Some(&*member.obj)
            }
            _ => None,
        };
        if held {
            self.held.push(call.span);
        } else {
            self.bare.push(call.span);
        }
        if let Some(receiver) = receiver {
            self.walk(receiver, held, frame);
        }
        for arg in &call.args {
            match &*arg.expr {
                Expr::Arrow(arrow) => {
                    let returned: Vec<&Expr>;
                    let mut returns = Returns::default();
                    let body = match &*arrow.body {
                        ArrowFunctionBody::FunctionBody(block) => {
                            block.visit_with(&mut returns);
                            returned = returns.exprs.iter().collect();
                            BodyRef::Block(block)
                        }
                        ArrowFunctionBody::Expr(expr) => {
                            returned = vec![&**expr];
                            BodyRef::Expr(expr)
                        }
                    };
                    let params: Vec<&Pat> = arrow.params.iter().collect();
                    self.callback(&params, &body, returned, receiver, held, frame);
                }
                Expr::Fn(function) => {
                    let Some(block) = &function.function.body else {
                        continue;
                    };
                    let mut returns = Returns::default();
                    block.visit_with(&mut returns);
                    let params: Vec<&Pat> =
                        function.function.params.iter().map(|p| &p.pat).collect();
                    self.callback(
                        &params,
                        &BodyRef::Block(block),
                        returns.exprs.iter().collect(),
                        receiver,
                        held,
                        frame,
                    );
                }
                expr => self.walk(expr, true, frame),
            }
        }
    }

    /// What an inline callback returns keeps the position of the call it
    /// is handed to, its first parameter given the receiver's value.
    fn callback(
        &mut self,
        params: &[&Pat],
        body: &dyn VisitWithScope,
        returned: Vec<&Expr>,
        receiver: Option<&Expr>,
        held: bool,
        frame: &Frame,
    ) {
        let mut scope = Scope::default();
        body.collect(&mut scope);
        self.frames += 1;
        let first = params.first().and_then(|param| match param {
            Pat::Ident(ident) => Some(ident.id.sym.to_string()),
            _ => None,
        });
        let child = Frame {
            id: self.frames,
            scope,
            params: params
                .iter()
                .flat_map(|param| pattern_names(param))
                .collect(),
            receiver: first.zip(receiver),
            parent: Some(frame),
        };
        for expr in returned {
            self.walk(expr, held, &child);
        }
    }

    /// Follow a name to every value it is given, in the frame that binds it.
    fn follow(&mut self, name: &str, held: bool, frame: &Frame) {
        if !self.followed.insert((name.to_string(), frame.id, held)) {
            return;
        }
        // A name taken apart is a member of its value, not the value.
        if frame.scope.parts.contains_key(name) {
            return;
        }
        if let Some(given) = frame.scope.given.get(name) {
            for value in given {
                self.walk(value, held, frame);
            }
            return;
        }
        if let Some((first, receiver)) = &frame.receiver
            && first.as_str() == name
        {
            if let Some(parent) = frame.parent {
                self.walk(receiver, held, parent);
            }
            return;
        }
        if frame.params.iter().any(|param| param == name) {
            return;
        }
        if let Some(parent) = frame.parent {
            self.follow(name, held, parent);
        }
    }
}

fn shape_of(returned: &[&Expr], scope: &Scope) -> Option<ReturnShape> {
    let mut shapes = Vec::new();
    for expr in returned {
        match classify(expr, scope)? {
            Returned::NoBody => {}
            Returned::Shape(shape) => shapes.push(shape),
        }
    }
    // Several returns that hand back the same call's value, or its parse,
    // are one shape (carrick#3823): `if (ok) return (await res.json()) as T;
    // return res.json();` hands back one parse. Two member reads stay two:
    // a shape does not say which member.
    match shapes.split_first() {
        Some((first, [])) => Some(first.clone()),
        Some((first, rest))
            if first.kind != ShapeKind::Member && rest.iter().all(|shape| shape == first) =>
        {
            Some(first.clone())
        }
        _ => None,
    }
}

enum Returned {
    /// `undefined`, `null`, `void 0`: no body is handed back.
    NoBody,
    Shape(ReturnShape),
}

/// A value with the wrappers that do not change it taken off and the names
/// followed to what they are given.
struct Resolved<'a> {
    expr: &'a Expr,
    /// Read off a bigger value by taking it apart.
    part: bool,
    /// For a part taken off an object pattern's own key, that key.
    key: Option<String>,
}

/// Names followed at most this far.
const MAX_NAME_HOPS: usize = 8;

fn resolve<'a>(expr: &'a Expr, scope: &'a Scope) -> Resolved<'a> {
    let mut expr = expr;
    let mut part = false;
    let mut key = None;
    // The key names the call's member only when one part hop leads to the
    // call: `const { data } = await call(); const { items } = data` holds
    // `data.items`, which no one key names (carrick#4513).
    let mut part_hops = 0;
    for _ in 0..MAX_NAME_HOPS {
        expr = match expr {
            Expr::Await(awaited) => &awaited.arg,
            Expr::Paren(paren) => &paren.expr,
            Expr::TsAs(cast) => &cast.expr,
            Expr::TsTypeAssertion(cast) => &cast.expr,
            Expr::TsConstAssertion(cast) => &cast.expr,
            Expr::TsNonNull(cast) => &cast.expr,
            Expr::TsSatisfies(cast) => &cast.expr,
            // `call.catch(() => null)`: the call's value or no body, as a
            // `return null` in a `catch` block hands back (carrick#4513).
            Expr::Call(call) => match no_body_catch(call) {
                Some(caught) => caught,
                None => break,
            },
            Expr::Ident(ident) => {
                let name: &str = &ident.sym;
                match scope.given.get(name).map(Vec::as_slice) {
                    Some([only]) => {
                        if scope.parts.contains_key(name) {
                            part = true;
                            part_hops += 1;
                            key = match part_hops {
                                1 => scope.part_keys.get(name).cloned(),
                                _ => None,
                            };
                        }
                        only
                    }
                    _ => break,
                }
            }
            _ => break,
        };
    }
    Resolved { expr, part, key }
}

fn classify(returned: &Expr, scope: &Scope) -> Option<Returned> {
    // `return body || null`, `return body ?? undefined`: the value or no
    // body, as a second `return null` would hand back (carrick#4513).
    if let Expr::Bin(bin) = returned
        && matches!(
            bin.op,
            swc_ecma_ast::BinaryOp::LogicalOr | swc_ecma_ast::BinaryOp::NullishCoalescing
        )
        && matches!(classify(&bin.right, scope), Some(Returned::NoBody))
    {
        return classify(&bin.left, scope);
    }
    let value = resolve(returned, scope);
    let base_of = |obj: &Expr| -> Option<Span> {
        let obj = resolve(obj, scope);
        match obj.expr {
            // A parse or a member of a value already read once is two reads.
            Expr::Call(call) if !obj.part && !is_parse(call) => Some(call.span),
            _ => None,
        }
    };
    let shape = |kind, base| {
        Some(Returned::Shape(ReturnShape {
            kind,
            base,
            member: None,
        }))
    };
    let member = |base, name: Option<String>| {
        Some(Returned::Shape(ReturnShape {
            kind: ShapeKind::Member,
            base,
            member: name,
        }))
    };
    match value.expr {
        Expr::Ident(ident) if &*ident.sym == "undefined" => Some(Returned::NoBody),
        Expr::Lit(swc_ecma_ast::Lit::Null(_)) => Some(Returned::NoBody),
        Expr::Unary(unary) if unary.op == swc_ecma_ast::UnaryOp::Void => Some(Returned::NoBody),
        // `const { data } = await call(); return data`
        Expr::Call(call) if value.part => member(call.span, value.key.clone()),
        _ if value.part => None,
        Expr::Call(call) => match &call.callee {
            Callee::Expr(callee) if is_parse(call) => {
                let Expr::Member(read) = &**callee else {
                    return None;
                };
                shape(ShapeKind::Parsed, base_of(&read.obj)?)
            }
            _ => shape(ShapeKind::Value, call.span),
        },
        Expr::OptChain(chain) => match &*chain.base {
            OptChainBase::Member(read) if reads_one_member(&read.prop) => {
                member(base_of(&read.obj)?, member_name(&read.prop))
            }
            OptChainBase::Call(_) => shape(ShapeKind::Value, chain.span),
            _ => None,
        },
        Expr::Member(read) if reads_one_member(&read.prop) => {
            member(base_of(&read.obj)?, member_name(&read.prop))
        }
        _ => None,
    }
}

/// The name of a property [`reads_one_member`] accepts.
fn member_name(prop: &MemberProp) -> Option<String> {
    match prop {
        MemberProp::Ident(name) => Some(name.sym.to_string()),
        MemberProp::Computed(computed) => match &*computed.expr {
            Expr::Lit(swc_ecma_ast::Lit::Str(text)) => {
                Some(text.value.to_string_lossy().into_owned())
            }
            _ => None,
        },
        MemberProp::PrivateName(_) => None,
    }
}

/// `<value>.json()`: the platform's parse of a response.
fn is_parse(call: &CallExpr) -> bool {
    call.args.is_empty()
        && matches!(
            &call.callee,
            Callee::Expr(callee) if matches!(
                &**callee,
                Expr::Member(member)
                    if matches!(&member.prop, MemberProp::Ident(name) if &*name.sym == "json")
            )
        )
}

/// The value `.catch` is called on, where the handler hands back no body:
/// `null`, `undefined`, `void 0`, or nothing (`() => {}`, a bare `return`).
fn no_body_catch(call: &CallExpr) -> Option<&Expr> {
    let Callee::Expr(callee) = &call.callee else {
        return None;
    };
    let Expr::Member(member) = &**callee else {
        return None;
    };
    if !matches!(&member.prop, MemberProp::Ident(name) if &*name.sym == "catch") {
        return None;
    }
    let [handler] = call.args.as_slice() else {
        return None;
    };
    if handler.spread.is_some() {
        return None;
    }
    let no_body = |expr: &Expr| match expr {
        Expr::Ident(ident) => &*ident.sym == "undefined",
        Expr::Lit(swc_ecma_ast::Lit::Null(_)) => true,
        Expr::Unary(unary) => unary.op == swc_ecma_ast::UnaryOp::Void,
        _ => false,
    };
    let block_no_body = |stmts: &[swc_ecma_ast::Stmt]| {
        stmts.iter().all(|stmt| match stmt {
            swc_ecma_ast::Stmt::Return(ret) => ret.arg.as_deref().is_none_or(no_body),
            swc_ecma_ast::Stmt::Empty(_) => true,
            _ => false,
        })
    };
    let hands_back_none = match &*handler.expr {
        Expr::Arrow(arrow) => match &*arrow.body {
            ArrowFunctionBody::Expr(expr) => no_body(expr),
            ArrowFunctionBody::FunctionBody(block) => block_no_body(&block.stmts),
        },
        Expr::Fn(function) => function
            .function
            .body
            .as_ref()
            .is_some_and(|block| block_no_body(&block.stmts)),
        _ => false,
    };
    hands_back_none.then_some(&*member.obj)
}

/// A property named in the source: `.data` or `["data"]`.
fn reads_one_member(prop: &MemberProp) -> bool {
    match prop {
        MemberProp::Ident(_) => true,
        MemberProp::Computed(computed) => {
            matches!(&*computed.expr, Expr::Lit(swc_ecma_ast::Lit::Str(_)))
        }
        MemberProp::PrivateName(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swc_common::{FileName, SourceMap, SourceMapper, sync::Lrc};
    use swc_ecma_parser::{Syntax, TsSyntax, parse_file_as_module};

    /// Every call of `source`, by its text, with its flow.
    fn flows(source: &str) -> Vec<(String, Flow)> {
        let cm: Lrc<SourceMap> = Default::default();
        let file = cm.new_source_file(
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
        .expect("parses");
        let text = |span: Span| cm.span_to_snippet(span).unwrap();
        let mut out: Vec<(String, Flow)> = call_flows(&module)
            .into_iter()
            .map(|(span, flow)| (text(span), flow))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    #[test]
    fn calls_are_listed_in_source_order() {
        let source = "function f(a) { one(a); one(a); two(a); one(a); three(one(a)); return 1; }\n";
        for _ in 0..20 {
            let cm: Lrc<SourceMap> = Default::default();
            let file = cm.new_source_file(
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
            .expect("parses");
            let starts: Vec<_> = call_flows(&module)
                .into_iter()
                .map(|(span, _)| span.lo)
                .collect();
            let mut sorted = starts.clone();
            sorted.sort();
            assert_eq!(starts, sorted);
        }
    }

    fn flow_of<'a>(flows: &'a [(String, Flow)], text: &str) -> &'a Flow {
        &flows
            .iter()
            .find(|(call, _)| call == text)
            .unwrap_or_else(|| panic!("no call {text}: {flows:?}"))
            .1
    }

    #[test]
    fn a_probe_awaited_before_the_returned_call_is_discarded() {
        let flows = flows(
            "async function invoke(call) {\n  await probe();\n  let result;\n  try { result = await call(); } catch (e) { throw e; }\n  return result.data;\n}\n",
        );
        assert_eq!(flow_of(&flows, "probe()"), &Flow::Discarded);
        assert_eq!(flow_of(&flows, "call()"), &Flow::Returned);
    }

    #[test]
    fn a_token_handed_to_the_returned_call_reaches_the_return_only_through_it() {
        let flows = flows(
            "async function f(id) {\n  const token = await loadToken();\n  return getRun({ id, token });\n}\n",
        );
        assert_eq!(flow_of(&flows, "getRun({ id, token })"), &Flow::Returned);
        let Flow::Through(through) = flow_of(&flows, "loadToken()") else {
            panic!("{flows:?}");
        };
        assert_eq!(through.len(), 1);
    }

    #[test]
    fn a_parsed_body_and_a_callback_body_are_each_their_own_functions_return() {
        let flows = flows(
            "async function f(url) {\n  const res = await fetch(url);\n  audit(url);\n  return res.json();\n}\nconst g = () => send(() => getRun());\n",
        );
        assert_eq!(flow_of(&flows, "fetch(url)"), &Flow::Returned);
        assert_eq!(flow_of(&flows, "res.json()"), &Flow::Returned);
        assert_eq!(flow_of(&flows, "audit(url)"), &Flow::Discarded);
        assert_eq!(flow_of(&flows, "send(() => getRun())"), &Flow::Returned);
        assert_eq!(flow_of(&flows, "getRun()"), &Flow::Returned);
    }

    #[test]
    fn nothing_returned_discards_every_call() {
        let flows = flows("async function save(x) {\n  await client.saveRun(x);\n}\n");
        assert_eq!(flow_of(&flows, "client.saveRun(x)"), &Flow::Discarded);
    }

    /// What the first function of `source` returns, as its kind and the text
    /// of the call it reads.
    fn shape(source: &str) -> Option<(ShapeKind, String)> {
        let cm: Lrc<SourceMap> = Default::default();
        let file = cm.new_source_file(
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
        .expect("parses");
        let shapes = return_shapes(&module);
        let (_, shape) = shapes.first().expect("a function");
        shape
            .as_ref()
            .map(|shape| (shape.kind, cm.span_to_snippet(shape.base).unwrap()))
    }

    fn of(kind: ShapeKind, base: &str) -> Option<(ShapeKind, String)> {
        Some((kind, base.to_string()))
    }

    #[test]
    fn a_returned_call_is_a_value() {
        assert_eq!(
            shape("function f(o) { return this.send(() => sdk(o)); }"),
            of(ShapeKind::Value, "this.send(() => sdk(o))")
        );
        assert_eq!(
            shape("const f = (o) => (o.client ?? t).get({ url });"),
            of(ShapeKind::Value, "(o.client ?? t).get({ url })")
        );
    }

    #[test]
    fn a_parse_of_a_call_is_parsed_through_a_name_or_an_await() {
        assert_eq!(
            shape("async function f(u) { const r = await fetch(u); return r.json(); }"),
            of(ShapeKind::Parsed, "fetch(u)")
        );
        assert_eq!(
            shape("async function f(u) { return (await fetch(u)).json(); }"),
            of(ShapeKind::Parsed, "fetch(u)")
        );
    }

    #[test]
    fn one_member_of_a_calls_value_is_a_member() {
        for source in [
            "async function f(o) { return (await call(o)).data; }",
            "async function f(o) { const r = await call(o); return r.data as T; }",
            "async function f(o) { const r = await call(o); return r?.data; }",
            "async function f(o) { const r = await call(o); return r[\"data\"]; }",
            "async function f(o) { const { data } = await call(o); return data; }",
            "async function f(o) { const { data: body } = await call(o); return body; }",
        ] {
            assert_eq!(shape(source), of(ShapeKind::Member, "call(o)"), "{source}");
        }
    }

    /// The first function's shape of `source`, whole.
    fn whole_shape(source: &str) -> Option<ReturnShape> {
        let cm: Lrc<SourceMap> = Default::default();
        let file = cm.new_source_file(
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
        .expect("parses");
        return_shapes(&module).remove(0).1
    }

    /// carrick#4371 R4: a member read names its member, and a destructured
    /// name the key it is taken from; a parse is `json`; a whole value
    /// reads nothing.
    #[test]
    fn a_member_read_names_its_member_and_a_destructured_name_its_key() {
        for (source, wanted) in [
            (
                "async function f(o) { return (await call(o)).data; }",
                "data",
            ),
            (
                "async function f(o) { const r = await call(o); return r?.ok; }",
                "ok",
            ),
            (
                "async function f(o) { const r = await call(o); return r[\"items\"]; }",
                "items",
            ),
            (
                "async function f(o) { const { data } = await call(o); return data; }",
                "data",
            ),
            (
                "async function f(o) { const { data: body } = await call(o); return body; }",
                "data",
            ),
        ] {
            let step = whole_shape(source).and_then(|shape| shape.step());
            assert_eq!(step, Some(Step::Member(wanted.to_string())), "{source}");
        }
        let parsed = whole_shape("async function f(u) { return (await fetch(u)).json(); }");
        assert_eq!(
            parsed.and_then(|shape| shape.step()),
            Some(Step::Parse("json".to_string()))
        );
        let whole = whole_shape("function f(o) { return call(o); }");
        assert_eq!(whole.and_then(|shape| shape.step()), None);
    }

    /// carrick#4513: a name destructured twice holds a member of a member,
    /// which no one key names; a value or no body reads as the value.
    #[test]
    fn a_chained_destructure_names_no_key_and_a_fallback_to_null_is_the_value() {
        let chained = whole_shape(
            "async function f(o) { const { data } = await call(o); const { items } = data; return items; }",
        );
        assert_eq!(
            chained.and_then(|shape| shape.step()),
            Some(Step::Member(String::new()))
        );
        for source in [
            "async function f(o) { const r = await call(o); return r.data || null; }",
            "async function f(o) { const r = await call(o); return r.data ?? undefined; }",
        ] {
            let step = whole_shape(source).and_then(|shape| shape.step());
            assert_eq!(step, Some(Step::Member("data".to_string())), "{source}");
        }
        // A rejection handler that hands back no body is a no-body path.
        // The chain ends at the request, `call(o)`, as an uncaught return's does.
        let whole = whole_shape("function f(o) { return call(o); }").expect("a call's value");
        for source in [
            "function f(o) { return call(o).catch(() => null); }",
            "function f(o) { return call(o).catch(() => undefined); }",
            "function f(o) { return call(o).catch(() => {}); }",
        ] {
            assert_eq!(whole_shape(source), Some(whole.clone()), "{source}");
        }
        let member = whole_shape("async function f(o) { const r = await call(o); return r.data; }")
            .expect("a member");
        let caught_member = whole_shape(
            "async function f(o) { const r = await call(o).catch(() => null); return r.data; }",
        );
        assert_eq!(caught_member, Some(member));
        // One that hands back a value of its own is that value.
        let own_value = whole_shape("function f(o) { return call(o).catch(() => []); }")
            .expect("a call's value");
        assert_ne!(own_value.base, whole.base);
        // A fallback to a value of the function's own is not the value.
        let own = whole_shape(
            "async function f(o) { const r = await call(o); return r.data || 'none'; }",
        );
        assert_eq!(own, None);
    }

    /// carrick#4371 R2: the one accept test, row by row.
    #[test]
    fn hands_back_body_reads_the_steps_against_the_ends_label() {
        use crate::services::type_sidecar::ResultShape;
        let member = |name: &str| Step::Member(name.to_string());
        let parse = |name: &str| Step::Parse(name.to_string());
        let data = EndRead::Labelled(ResultShape::Member(["data".to_string()]));
        let text = EndRead::Labelled(ResultShape::Parse("text".to_string()));
        let whole = EndRead::Labelled(ResultShape::Whole(true));
        for (steps, end, wanted) in [
            (vec![member("data")], &data, HandBack::Body),
            (vec![member("status")], &data, HandBack::Other),
            (vec![], &data, HandBack::Other),
            (vec![parse("text")], &text, HandBack::Body),
            (vec![parse("json")], &text, HandBack::Other),
            (vec![], &whole, HandBack::Body),
            (vec![member("ok")], &whole, HandBack::Other),
            (vec![], &EndRead::Unlabelled, HandBack::Body),
            (
                vec![member("data")],
                &EndRead::Unlabelled,
                HandBack::Undecided,
            ),
            (vec![parse("json")], &EndRead::Unlabelled, HandBack::Other),
            (vec![parse("json")], &EndRead::PlatformFetch, HandBack::Body),
            (vec![], &EndRead::PlatformFetch, HandBack::Undecided),
            (vec![member("ok")], &EndRead::PlatformFetch, HandBack::Other),
            (vec![member("data"), member("x")], &data, HandBack::Other),
        ] {
            assert_eq!(
                hands_back_body(&steps, end),
                wanted,
                "{steps:?} over {end:?}"
            );
        }
    }

    #[test]
    fn a_name_assigned_once_after_its_declaration_is_followed() {
        assert_eq!(
            shape(
                "async function f(call) { let result; try { result = await call(); } catch (e) { throw e; } return result.data; }"
            ),
            of(ShapeKind::Member, "call()")
        );
    }

    #[test]
    fn returns_that_hand_back_no_body_are_ignored() {
        assert_eq!(
            shape(
                "async function f<T>(call) { const r = await call(); if (r.data === null) return undefined as T; if (!r) return; if (r.x) return null; if (r.y) return void 0; return r.data as T; }"
            ),
            of(ShapeKind::Member, "call()")
        );
    }

    /// carrick#3823: two returns that each parse the one response, or each
    /// hand back the one value, are one shape.
    #[test]
    fn returns_that_read_one_call_the_same_way_are_one_shape() {
        assert_eq!(
            shape(
                "async function f<T>(u) { const r = await fetch(u); if (r.status === 200) { return (await r.json()) as T; } return (await r.json()) as T; }"
            ),
            of(ShapeKind::Parsed, "fetch(u)")
        );
        assert_eq!(
            shape("async function f(u) { const r = await call(u); if (u) return r; return r; }"),
            of(ShapeKind::Value, "call(u)")
        );
    }

    #[test]
    fn anything_else_returned_is_no_shape() {
        for source in [
            "function f(o) { return { data: sdk(o) }; }",
            "function f(o) { return [sdk(o)]; }",
            "function f(o) { return `${sdk(o)}`; }",
            "function f(o) { return sdk(o) || 1; }",
            "function f(o) { return o ? sdk(o) : sdk(1); }",
            "async function f(o) { return (await sdk(o)).data.items; }",
            "async function f(o) { return (await fetch(o)).json().data; }",
            "async function f(o) { return (await sdk(o)).data.json(); }",
            "async function f(o) { if (o) return sdk(1); return sdk(2); }",
            "async function f(o) { const r = await sdk(o); if (o) return r.data; return r.items; }",
            "async function f(o) { const r = await fetch(o); if (o) return r.json(); return r; }",
            "async function f(o) { await sdk(o); }",
            "async function f(o) { return o.data; }",
            "async function f(o) { let r = sdk(1); r = sdk(2); return r.data; }",
            "async function f(o) { const { a } = o; return a; }",
            "async function f(o) { return undefined; }",
        ] {
            assert_eq!(shape(source), None, "{source}");
        }
    }

    /// The calls the first function of `source` may hand back, by their
    /// text: `(bare, held)`.
    fn held(source: &str) -> (Vec<String>, Vec<String>) {
        let cm: Lrc<SourceMap> = Default::default();
        let file = cm.new_source_file(
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
        .expect("parses");
        let held = held_returns(&module);
        let (_, held) = held.first().expect("a function");
        let texts = |spans: &[Span]| {
            let mut texts: Vec<String> = spans
                .iter()
                .map(|span| cm.span_to_snippet(*span).unwrap())
                .collect();
            texts.sort();
            texts.dedup();
            texts
        };
        (texts(&held.bare), texts(&held.held))
    }

    fn texts(calls: &[&str]) -> Vec<String> {
        let mut texts: Vec<String> = calls.iter().map(|call| call.to_string()).collect();
        texts.sort();
        texts
    }

    /// carrick#4270: what each wrapper the compiler engine was measured on
    /// hands back, whole or held. The engine keeps a caller's answer only
    /// where the transport response sits inside the value, so these are the
    /// shapes the ask is gated on.
    #[test]
    fn a_transport_inside_a_returned_value_is_held() {
        let cases: [(&str, &[&str], &[&str]); 13] = [
            (
                "async function f(u) { const res = await fetch(u); return { res, json: (await res.json()) as R }; }",
                &[],
                &["fetch(u)"],
            ),
            (
                "async function f(u, e) { const res = await fetch(u); if (e) { return { res, first: (await res.json()) as R }; } return { res, second: (await res.json()) as R }; }",
                &[],
                &["fetch(u)"],
            ),
            (
                "async function f(u) { const res = await fetch(u); return { res, body: await res.text() }; }",
                &[],
                &["fetch(u)"],
            ),
            (
                "async function f(u) { const res = await fetch(u); return pack(res, (await res.json()) as R); }\nfunction pack(res, json) { return { res, json }; }",
                &["pack(res, (await res.json()) as R)"],
                &["fetch(u)"],
            ),
            (
                "async function f(u) { const res = await fetch(u); return new Holder(res, (await res.json()) as R); }",
                &[],
                &["fetch(u)"],
            ),
            (
                "async function f(u) { const res = await fetch(u); const extra = { at: Date.now() }; return { ...extra, res, json: (await res.json()) as R }; }",
                &[],
                &["Date.now()", "fetch(u)"],
            ),
            (
                "async function f(u) { const res = await fetch(u); return { res: res.clone(), json: (await res.json()) as R }; }",
                &[],
                &["fetch(u)", "res.clone()"],
            ),
            (
                "async function f(u) { const res = await send(u); return { res, json: (await res.json()) as R }; }",
                &[],
                &["send(u)"],
            ),
            (
                "async function f(u): Promise<Out> { const res = await fetch(u); return { res, json: (await res.json()) as R }; }",
                &[],
                &["fetch(u)"],
            ),
            (
                "async function f(u) { const res = await fetch(u); return [res, (await res.json()) as R] as const; }",
                &[],
                &["fetch(u)"],
            ),
            (
                "async function f(u) { const res = await fetch(u); const json = (await res.json()) as R; return { meta: { res }, json }; }",
                &[],
                &["fetch(u)"],
            ),
            (
                "async function f(u) { const res = await fetch(u); return handle(res); }",
                &["handle(res)"],
                &["fetch(u)"],
            ),
            (
                "async function f(u) { return fetch(u).then(async (res) => ({ res, json: (await res.json()) as R })); }",
                &[
                    "fetch(u)",
                    "fetch(u).then(async (res) => ({ res, json: (await res.json()) as R }))",
                ],
                &["fetch(u)"],
            ),
        ];
        for (source, bare, held_calls) in cases {
            assert_eq!(held(source), (texts(bare), texts(held_calls)), "{source}");
        }
    }

    /// carrick#4270: a wrapper that hands back a call's value whole, reads a
    /// member or the body, or hands back none of its calls holds no call.
    #[test]
    fn a_transport_handed_back_whole_or_read_is_not_held() {
        let cases: [(&str, &[&str]); 12] = [
            (
                "async function f(u) { return callPlain(u); }",
                &["callPlain(u)"],
            ),
            (
                "async function f(u) { return retry(() => callPlain(u)); }",
                &["callPlain(u)", "retry(() => callPlain(u))"],
            ),
            ("async function f(u) { return fetch(u); }", &["fetch(u)"]),
            (
                "async function f(u) { const res = await fetch(u); if (!res.ok) throw new Error('bad'); return res; }",
                &["fetch(u)"],
            ),
            (
                "async function f(u) { return fetch(u).then((r) => r.json() as Promise<R>); }",
                &["fetch(u)", "fetch(u).then((r) => r.json() as Promise<R>)"],
            ),
            (
                "async function f(u) { return fetch(u).then((r) => r.json() as Promise<R>).catch(() => null); }",
                &[
                    "fetch(u)",
                    "fetch(u).then((r) => r.json() as Promise<R>)",
                    "fetch(u).then((r) => r.json() as Promise<R>).catch(() => null)",
                ],
            ),
            (
                "async function f(u) { return retry(() => fetch(u)); }",
                &["fetch(u)", "retry(() => fetch(u))"],
            ),
            (
                "async function f(u) { const res = await fetch(u); const json = (await res.json()) as R; return { status: res.status, json }; }",
                &[],
            ),
            (
                "async function f(u) { const res = await fetch(u); const json = (await res.json()) as R; return json.data; }",
                &[],
            ),
            (
                "async function f(u) { const res = await fetch(u); return { headers: res.headers, json: (await res.json()) as R }; }",
                &[],
            ),
            (
                "async function f(u) { const res = await fetch(u); return { ok: true, data: (await res.json()) as R }; }",
                &[],
            ),
            (
                "async function f(u) { const { data } = await fetch(u); return { data }; }",
                &[],
            ),
        ];
        for (source, bare) in cases {
            assert_eq!(held(source), (texts(bare), Vec::new()), "{source}");
        }
    }

    /// carrick#4270: a callback's first parameter is the value of the
    /// receiver it was handed to, and a callback's own names shadow the
    /// enclosing body's.
    #[test]
    fn a_callbacks_parameter_is_its_receivers_value() {
        assert_eq!(
            held(
                "async function f(u) { const res = await fetch(u); return load(u).then((res) => ({ res })); }"
            ),
            (
                texts(&["load(u)", "load(u).then((res) => ({ res }))"]),
                texts(&["load(u)"])
            )
        );
        assert_eq!(
            held("async function f(u) { return run((res) => ({ res })); }"),
            (texts(&["run((res) => ({ res }))"]), Vec::new())
        );
    }

    #[test]
    fn a_nested_functions_return_is_its_own() {
        let cm: Lrc<SourceMap> = Default::default();
        let file = cm.new_source_file(
            Lrc::new(FileName::Custom("t.ts".into())),
            "function f(o) { const g = () => other(o); return send(g); }".to_string(),
        );
        let module = parse_file_as_module(
            &file,
            Syntax::Typescript(TsSyntax::default()),
            Default::default(),
            None,
            &mut Vec::new(),
        )
        .expect("parses");
        let bases: Vec<String> = return_shapes(&module)
            .into_iter()
            .map(|(_, shape)| cm.span_to_snippet(shape.unwrap().base).unwrap())
            .collect();
        assert_eq!(bases, ["send(g)", "other(o)"]);
    }
}
