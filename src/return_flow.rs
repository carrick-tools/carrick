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
}

impl Visit for Scope {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        self.calls.push(call.span);
        call.visit_children_with(self);
    }
    fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
        if let Some(init) = &declarator.init {
            let whole = matches!(declarator.name, Pat::Ident(_));
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReturnShape {
    pub kind: ShapeKind,
    /// The call whose value is read.
    pub base: Span,
}

/// Every function of `module`, by the span of its node, with what it returns:
/// `Some` where every `return` but the ones that hand back no body
/// (`undefined`, `null`, `void 0`, a bare `return;`) is one and the same call's
/// value, whole, parsed or one member of it. `None` for anything else: an
/// object or array, a template, an operator, a conditional, two members read,
/// two returns, or none. In source order.
///
/// A returned name is followed to the one value it is given: its
/// initialiser, or the single assignment of a name declared without one.
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

fn shape_of(returned: &[&Expr], scope: &Scope) -> Option<ReturnShape> {
    let mut shapes = Vec::new();
    for expr in returned {
        match classify(expr, scope)? {
            Returned::NoBody => {}
            Returned::Shape(shape) => shapes.push(shape),
        }
    }
    match shapes.as_slice() {
        [one] => Some(*one),
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
}

/// Names followed at most this far.
const MAX_NAME_HOPS: usize = 8;

fn resolve<'a>(expr: &'a Expr, scope: &'a Scope) -> Resolved<'a> {
    let mut expr = expr;
    let mut part = false;
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
                        part |= scope.parts.contains_key(name);
                        only
                    }
                    _ => break,
                }
            }
            _ => break,
        };
    }
    Resolved { expr, part }
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
    let shape = |kind, base| Some(Returned::Shape(ReturnShape { kind, base }));
    match value.expr {
        Expr::Ident(ident) if &*ident.sym == "undefined" => Some(Returned::NoBody),
        Expr::Lit(swc_ecma_ast::Lit::Null(_)) => Some(Returned::NoBody),
        Expr::Unary(unary) if unary.op == swc_ecma_ast::UnaryOp::Void => Some(Returned::NoBody),
        // `const { data } = await call(); return data`
        Expr::Call(call) if value.part => shape(ShapeKind::Member, call.span),
        _ if value.part => None,
        Expr::Call(call) => match &call.callee {
            Callee::Expr(callee) if is_parse(call) => {
                let Expr::Member(member) = &**callee else {
                    return None;
                };
                shape(ShapeKind::Parsed, base_of(&member.obj)?)
            }
            _ => shape(ShapeKind::Value, call.span),
        },
        Expr::OptChain(chain) => match &*chain.base {
            OptChainBase::Member(member) if reads_one_member(&member.prop) => {
                shape(ShapeKind::Member, base_of(&member.obj)?)
            }
            OptChainBase::Call(_) => shape(ShapeKind::Value, chain.span),
            _ => None,
        },
        Expr::Member(member) if reads_one_member(&member.prop) => {
            shape(ShapeKind::Member, base_of(&member.obj)?)
        }
        _ => None,
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
        shape.map(|shape| (shape.kind, cm.span_to_snippet(shape.base).unwrap()))
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
