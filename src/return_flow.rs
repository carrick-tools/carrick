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
/// Anything else is [`HandBack::Other`], except a member read over an
/// unlabelled end, which is [`HandBack::Undecided`]: the label decides it.
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
    if *end == EndRead::Unlabelled && reads_member {
        return HandBack::Undecided;
    }
    HandBack::Other
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
    for _ in 0..MAX_NAME_HOPS {
        expr = match expr {
            Expr::Await(awaited) => &awaited.arg,
            Expr::Paren(paren) => &paren.expr,
            Expr::TsAs(cast) => &cast.expr,
            Expr::TsTypeAssertion(cast) => &cast.expr,
            Expr::TsConstAssertion(cast) => &cast.expr,
            Expr::TsNonNull(cast) => &cast.expr,
            Expr::TsSatisfies(cast) => &cast.expr,
            Expr::Ident(ident) => {
                let name: &str = &ident.sym;
                match scope.given.get(name).map(Vec::as_slice) {
                    Some([only]) => {
                        if scope.parts.contains_key(name) {
                            part = true;
                            key = scope.part_keys.get(name).cloned();
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
            (vec![], &EndRead::PlatformFetch, HandBack::Other),
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
