//! Routes registered in a loop over values the source holds (carrick#2093).
//!
//! ```ignore
//! export const PLAN_ACTIONS = ["create", "preview", "apply"] as const;
//!
//! for (const action of PLAN_ACTIONS) {
//!   app.post(`/teams/:id/plan/actions/${action}`, handler);
//! }
//! ```
//!
//! One registration call states three routes. The model reads the call as one
//! row and writes the hole however it likes: `${action}` (which no route
//! matcher reads as a placeholder) or `:action` (a one-segment catch-all that
//! routes every call with no literal agreement). Either way the index holds a
//! row no call matches, and every call to one of the three routes reads as
//! reaching none.
//!
//! This module reads, from the AST alone, the registrations whose path is a
//! template every hole of which is a loop binding over an array the source
//! states as literals. The orchestrator replaces the model's row at such a
//! call with one row per value (`FileOrchestrator::expand_loop_registrations`).
//!
//! What is read is narrow, and anything else is left to the model's row:
//!
//! - **Loops.** `for (const <pattern> of X)`, and `X.forEach(cb)` with an
//!   inline arrow or function expression whose first parameter is the
//!   pattern. `for…in`, `let`/`var` bindings, `for await`, `.map`, index
//!   loops and the `Object.*` iterators are not read.
//! - **Pattern.** An identifier, an array pattern of identifiers (elisions
//!   allowed), or an object pattern of `key` / `key: name` properties. A
//!   default, a rest element, a nested or computed pattern is not read.
//! - **Iterable.** After unwrapping parentheses and type assertions: an array
//!   literal; an identifier bound (by [`BindingKey`], so a shadow is a
//!   different binding) to a `const` declarator initialised with an array
//!   literal; or an identifier bound by a named import. The imported array is
//!   resolved after the scan by the orchestrator, against the declaring
//!   module's top-level `const` ([`top_level_const_arrays`]).
//! - **Elements.** Every element gives each bound name a string literal (or a
//!   template with no substitution). A spread, an elision or any other element
//!   makes the whole loop unread.
//! - **Writes.** A loop over a binding the module writes through
//!   (`X[i] = …`, `X.push(…)`, any Array mutator) is unread, and so is a loop
//!   whose own binding is assigned (a `forEach` parameter can be).
//! - **Holes.** Every `${…}` is a bare identifier bound by an enclosing loop
//!   read above. A member read, a call, or any other name leaves the row as
//!   the model stated it.
//!
//! Rows are the product over the loops the holes read (outer loop first, each
//! loop in element order), equal paths folded, and at most [`MAX_ROWS`].

use std::collections::{HashMap, HashSet};

use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use crate::binding_scope::{BindingKey, ident_key};
use crate::graphql_document_sites::unwrap_expression;
use crate::request_summary::literal_specifier;

/// The most rows one registration may state. Past this the loop is more
/// likely a table of something other than routes, and the model's row stays.
pub const MAX_ROWS: usize = 64;

/// The ECMAScript `Array.prototype` methods that change an array in place.
const ARRAY_MUTATORS: &[&str] = &[
    "push",
    "pop",
    "shift",
    "unshift",
    "splice",
    "sort",
    "reverse",
    "fill",
    "copyWithin",
];

/// Every template registration in one file, keyed by the call's
/// `span.lo.0` (the unit `CandidateTarget::span_start` carries): what it
/// states, or why it is left as the model wrote it.
pub type LoopTemplates = HashMap<u32, Result<LoopTemplate, String>>;

/// Which part of a loop's element a bound name reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// `for (const x of …)`: the element itself.
    Whole,
    /// `for (const [a, b] of …)`: the element's item at this index.
    Index(usize),
    /// `for (const { a } of …)`: the element's property of this name.
    Key(String),
}

/// One element of an array literal, as far as a loop pattern can read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Element {
    Literal(String),
    /// An array literal: each item's literal, `None` where it is not one.
    Tuple(Vec<Option<String>>),
    /// An object literal of plain `key: value` properties: each value's
    /// literal, `None` where it is not one.
    Record(Vec<(String, Option<String>)>),
    /// Anything else.
    Other,
}

/// Where a loop's values come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopSource {
    /// One entry per element, in source order; each holds one value per
    /// [`Loop::parts`] entry.
    Stated(Vec<Vec<String>>),
    /// A named import, resolved after the scan.
    Imported {
        specifier: String,
        exported_name: String,
    },
}

/// One loop a template's holes read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loop {
    pub parts: Vec<Part>,
    pub source: LoopSource,
}

/// A registration whose path is a template over loop bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopTemplate {
    /// The template's cooked literal parts; one more than `holes`.
    pub quasis: Vec<String>,
    /// For each hole: (index into `loops`, index into that loop's `parts`).
    pub holes: Vec<(usize, usize)>,
    /// The loops the holes read, outermost first.
    pub loops: Vec<Loop>,
}

impl LoopTemplate {
    /// The paths this registration states, in order, equal paths folded.
    /// `Err` names why it states none the scan can stand behind.
    pub fn paths(&self) -> Result<Vec<String>, String> {
        let mut sizes = Vec::with_capacity(self.loops.len());
        for each in &self.loops {
            match &each.source {
                LoopSource::Stated(elements) => sizes.push(elements.len()),
                LoopSource::Imported { exported_name, .. } => {
                    return Err(format!("the imported `{exported_name}` was not resolved"));
                }
            }
        }
        if sizes.contains(&0) {
            return Err("a loop is over an empty array".to_string());
        }
        let product = sizes
            .iter()
            .try_fold(1usize, |acc, size| acc.checked_mul(*size));
        if product.is_none_or(|product| product > MAX_ROWS * MAX_ROWS) {
            return Err(format!("it would state more than {MAX_ROWS} routes"));
        }
        let mut paths: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut choice = vec![0usize; sizes.len()];
        loop {
            let mut path = self.quasis[0].clone();
            for (hole, (loop_index, part)) in self.holes.iter().enumerate() {
                let LoopSource::Stated(elements) = &self.loops[*loop_index].source else {
                    unreachable!("checked above");
                };
                path.push_str(&elements[choice[*loop_index]][*part]);
                path.push_str(&self.quasis[hole + 1]);
            }
            if seen.insert(path.clone()) {
                paths.push(path);
                if paths.len() > MAX_ROWS {
                    return Err(format!("it would state more than {MAX_ROWS} routes"));
                }
            }
            // Advance the innermost loop first, so the outer loop varies
            // slowest and rows read in the order the loops run.
            let mut index = sizes.len();
            loop {
                if index == 0 {
                    return Ok(paths);
                }
                index -= 1;
                choice[index] += 1;
                if choice[index] < sizes[index] {
                    break;
                }
                choice[index] = 0;
            }
        }
    }
}

/// Each element's value for each part, or `Err` naming why the loop is
/// unread. Any element that does not give every part a literal makes the
/// whole loop unread.
pub fn element_values(elements: &[Element], parts: &[Part]) -> Result<Vec<Vec<String>>, String> {
    elements
        .iter()
        .map(|element| {
            parts
                .iter()
                .map(|part| {
                    part_value(element, part)
                        .ok_or_else(|| "an element is not a literal for every name".to_string())
                })
                .collect()
        })
        .collect()
}

fn part_value(element: &Element, part: &Part) -> Option<String> {
    match (element, part) {
        (Element::Literal(value), Part::Whole) => Some(value.clone()),
        (Element::Tuple(items), Part::Index(index)) => items.get(*index).cloned().flatten(),
        (Element::Record(props), Part::Key(key)) => {
            let mut found = props.iter().filter(|(name, _)| name == key);
            let (_, value) = found.next()?;
            // A key written twice is read by whichever comes last; leave it.
            if found.next().is_some() {
                return None;
            }
            value.clone()
        }
        _ => None,
    }
}

/// The elements of an array literal, or `None` when it has a spread or an
/// elision.
fn array_elements(array: &ArrayLit) -> Option<Vec<Element>> {
    array
        .elems
        .iter()
        .map(|item| match item {
            Some(ExprOrSpread { spread: None, expr }) => Some(element_of(expr)),
            _ => None,
        })
        .collect()
}

fn element_of(expr: &Expr) -> Element {
    if let Some(value) = literal_specifier(expr) {
        return Element::Literal(value);
    }
    match unwrap_expression(expr) {
        Expr::Array(array) => Element::Tuple(
            array
                .elems
                .iter()
                .map(|item| match item {
                    Some(ExprOrSpread { spread: None, expr }) => literal_specifier(expr),
                    _ => None,
                })
                .collect(),
        ),
        Expr::Object(object) => {
            let mut props = Vec::with_capacity(object.props.len());
            for prop in &object.props {
                let PropOrSpread::Prop(prop) = prop else {
                    return Element::Other;
                };
                let Prop::KeyValue(KeyValueProp { key, value }) = &**prop else {
                    return Element::Other;
                };
                let Some(name) = plain_key(key) else {
                    return Element::Other;
                };
                props.push((name, literal_specifier(value)));
            }
            Element::Record(props)
        }
        _ => Element::Other,
    }
}

fn plain_key(key: &PropName) -> Option<String> {
    match key {
        PropName::Ident(ident) => Some(ident.sym.to_string()),
        PropName::Str(value) => Some(value.value.to_string_lossy().into_owned()),
        _ => None,
    }
}

/// What a module declares and writes, read once before the walk.
#[derive(Default)]
struct Tables {
    /// Every `const` declarator of a plain name: its array literal's
    /// elements, or `None` when its initialiser is anything else.
    consts: HashMap<BindingKey, Option<Vec<Element>>>,
    /// Every named import: (specifier, exported name).
    imports: HashMap<BindingKey, (String, String)>,
    /// Every binding the module writes through or assigns.
    written: HashSet<BindingKey>,
}

impl Tables {
    fn of(module: &Module) -> Self {
        let mut tables = Self::default();
        module.visit_with(&mut tables);
        tables
    }

    fn write_root(&mut self, expr: &Expr) {
        if let Some(root) = root_ident(expr) {
            self.written.insert(ident_key(root));
        }
    }

    /// Every binding a destructuring target assigns or writes through.
    fn write_pat<N: for<'a> VisitWith<Targets<'a>>>(&mut self, pat: &N) {
        pat.visit_with(&mut Targets(self));
    }
}

/// The targets of a destructuring assignment.
struct Targets<'a>(&'a mut Tables);

impl Visit for Targets<'_> {
    fn visit_binding_ident(&mut self, ident: &BindingIdent) {
        self.0.written.insert(ident_key(&ident.id));
    }
    fn visit_expr(&mut self, expr: &Expr) {
        self.0.write_root(expr);
    }
    // A default value is read, not written.
    fn visit_assign_pat(&mut self, pat: &AssignPat) {
        pat.left.visit_with(self);
    }
}

/// The identifier a member chain is rooted at: `X` for `X`, `X[0].a`.
fn root_ident(expr: &Expr) -> Option<&Ident> {
    match unwrap_expression(expr) {
        Expr::Ident(ident) => Some(ident),
        Expr::Member(member) => root_ident(&member.obj),
        _ => None,
    }
}

impl Visit for Tables {
    fn visit_var_decl(&mut self, decl: &VarDecl) {
        if decl.kind == VarDeclKind::Const {
            for declarator in &decl.decls {
                if let (Pat::Ident(name), Some(init)) = (&declarator.name, &declarator.init) {
                    let elements = match unwrap_expression(init) {
                        Expr::Array(array) => array_elements(array),
                        _ => None,
                    };
                    self.consts.insert(ident_key(&name.id), elements);
                }
            }
        }
        decl.visit_children_with(self);
    }

    fn visit_import_decl(&mut self, decl: &ImportDecl) {
        if decl.type_only {
            return;
        }
        let specifier = decl.src.value.to_string_lossy().into_owned();
        for each in &decl.specifiers {
            let ImportSpecifier::Named(named) = each else {
                continue;
            };
            if named.is_type_only {
                continue;
            }
            let exported = match &named.imported {
                Some(ModuleExportName::Ident(ident)) => ident.sym.to_string(),
                Some(ModuleExportName::Str(value)) => value.value.to_string_lossy().into_owned(),
                None => named.local.sym.to_string(),
            };
            self.imports
                .insert(ident_key(&named.local), (specifier.clone(), exported));
        }
    }

    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        match &assign.left {
            AssignTarget::Simple(SimpleAssignTarget::Ident(ident)) => {
                self.written.insert(ident_key(&ident.id));
            }
            AssignTarget::Simple(SimpleAssignTarget::Member(member)) => {
                if let Some(root) = root_ident(&member.obj) {
                    self.written.insert(ident_key(root));
                }
            }
            AssignTarget::Simple(SimpleAssignTarget::Paren(paren)) => self.write_root(&paren.expr),
            AssignTarget::Pat(AssignTargetPat::Array(array)) => self.write_pat(array),
            AssignTarget::Pat(AssignTargetPat::Object(object)) => self.write_pat(object),
            _ => {}
        }
        assign.visit_children_with(self);
    }

    fn visit_update_expr(&mut self, update: &UpdateExpr) {
        self.write_root(&update.arg);
        update.visit_children_with(self);
    }

    fn visit_unary_expr(&mut self, unary: &UnaryExpr) {
        if unary.op == UnaryOp::Delete {
            self.write_root(&unary.arg);
        }
        unary.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        if let Callee::Expr(callee) = &call.callee
            && let Expr::Member(member) = unwrap_expression(callee)
            && let MemberProp::Ident(name) = &member.prop
            && ARRAY_MUTATORS.contains(&name.sym.as_ref())
        {
            self.write_root(&member.obj);
        }
        call.visit_children_with(self);
    }

    fn visit_for_of_stmt(&mut self, stmt: &ForOfStmt) {
        if let ForHead::Pat(pat) = &stmt.left {
            self.write_pat(&**pat);
        }
        stmt.visit_children_with(self);
    }

    fn visit_for_in_stmt(&mut self, stmt: &ForInStmt) {
        if let ForHead::Pat(pat) = &stmt.left {
            self.write_pat(&**pat);
        }
        stmt.visit_children_with(self);
    }
}

/// One enclosing loop during the walk.
struct Frame {
    bindings: Bindings,
    /// The loop, or why it is unread.
    read: Result<Loop, String>,
}

struct Collector<'a> {
    tables: &'a Tables,
    frames: Vec<Frame>,
    out: LoopTemplates,
}

/// Read every template registration in `module`. See the module docs.
pub fn collect_loop_templates(module: &Module) -> LoopTemplates {
    let tables = Tables::of(module);
    let mut collector = Collector {
        tables: &tables,
        frames: Vec::new(),
        out: HashMap::new(),
    };
    module.visit_with(&mut collector);
    collector.out
}

/// The names a pattern binds, whatever its shape.
fn bound_keys(pat: &Pat) -> Vec<BindingKey> {
    struct Names(Vec<BindingKey>);
    impl Visit for Names {
        fn visit_binding_ident(&mut self, ident: &BindingIdent) {
            self.0.push(ident_key(&ident.id));
        }
        // A default value binds nothing.
        fn visit_assign_pat(&mut self, pat: &AssignPat) {
            pat.left.visit_with(self);
        }
    }
    let mut names = Names(Vec::new());
    pat.visit_with(&mut names);
    names.0
}

/// The names a loop pattern binds, each with the index of the part it reads.
type Bindings = Vec<(BindingKey, usize)>;

/// A loop pattern's bindings and parts, or why it is not read.
fn pattern_parts(pat: &Pat) -> Result<(Bindings, Vec<Part>), String> {
    let mut bindings = Vec::new();
    let mut parts = Vec::new();
    let mut bind = |key: BindingKey, part: Part| {
        bindings.push((key, parts.len()));
        parts.push(part);
    };
    match pat {
        Pat::Ident(ident) => bind(ident_key(&ident.id), Part::Whole),
        Pat::Array(array) => {
            for (index, item) in array.elems.iter().enumerate() {
                match item {
                    None => {}
                    Some(Pat::Ident(ident)) => bind(ident_key(&ident.id), Part::Index(index)),
                    Some(_) => {
                        return Err("the loop's array pattern holds more than names".to_string());
                    }
                }
            }
        }
        Pat::Object(object) => {
            for prop in &object.props {
                match prop {
                    ObjectPatProp::KeyValue(KeyValuePatProp { key, value }) => {
                        let (Some(name), Pat::Ident(ident)) = (plain_key(key), &**value) else {
                            return Err(
                                "the loop's object pattern holds more than names".to_string()
                            );
                        };
                        bind(ident_key(&ident.id), Part::Key(name));
                    }
                    ObjectPatProp::Assign(AssignPatProp {
                        key, value: None, ..
                    }) => bind(ident_key(&key.id), Part::Key(key.id.sym.to_string())),
                    _ => {
                        return Err("the loop's object pattern has a default or a rest".to_string());
                    }
                }
            }
        }
        _ => return Err("the loop's binding is not a name or a plain pattern".to_string()),
    }
    Ok((bindings, parts))
}

impl Collector<'_> {
    /// What a loop over `iterable` with pattern `pat` reads, as a frame.
    fn frame(&self, pat: &Pat, iterable: &Expr) -> Frame {
        let (bindings, parts) = match pattern_parts(pat) {
            Ok(read) => read,
            Err(reason) => {
                return Frame {
                    bindings: bound_keys(pat).into_iter().map(|key| (key, 0)).collect(),
                    read: Err(reason),
                };
            }
        };
        let read = if bindings
            .iter()
            .any(|(key, _)| self.tables.written.contains(key))
        {
            Err("the loop's binding is assigned".to_string())
        } else {
            self.source(iterable, &parts)
                .map(|source| Loop { parts, source })
        };
        Frame { bindings, read }
    }

    fn source(&self, iterable: &Expr, parts: &[Part]) -> Result<LoopSource, String> {
        match unwrap_expression(iterable) {
            Expr::Array(array) => {
                let elements = array_elements(array)
                    .ok_or_else(|| "the array has a spread or a hole".to_string())?;
                element_values(&elements, parts).map(LoopSource::Stated)
            }
            Expr::Ident(ident) => {
                let key = ident_key(ident);
                let name = &ident.sym;
                if self.tables.written.contains(&key) {
                    return Err(format!("the source writes through `{name}`"));
                }
                if let Some(elements) = self.tables.consts.get(&key) {
                    let elements = elements.as_ref().ok_or_else(|| {
                        format!("`{name}` is not a const array literal of plain elements")
                    })?;
                    return element_values(elements, parts).map(LoopSource::Stated);
                }
                if let Some((specifier, exported_name)) = self.tables.imports.get(&key) {
                    return Ok(LoopSource::Imported {
                        specifier: specifier.clone(),
                        exported_name: exported_name.clone(),
                    });
                }
                Err(format!("`{name}` is not a const array or a named import"))
            }
            _ => Err("the loop is over something other than an array or a name".to_string()),
        }
    }

    fn walk_in(&mut self, frame: Frame, body: &dyn Fn(&mut Self)) {
        self.frames.push(frame);
        body(self);
        self.frames.pop();
    }

    /// What a template first argument states, or why it is left alone.
    fn template(&self, tpl: &Tpl) -> Result<LoopTemplate, String> {
        let quasis = tpl
            .quasis
            .iter()
            .map(|quasi| {
                quasi
                    .cooked
                    .as_ref()
                    .map(|cooked| cooked.to_string_lossy().into_owned())
                    .ok_or_else(|| "the template has an invalid escape".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        // (frame index, part index) per hole.
        let mut reads: Vec<(usize, usize)> = Vec::with_capacity(tpl.exprs.len());
        for expr in &tpl.exprs {
            let Expr::Ident(ident) = &**expr else {
                return Err("a hole is not a bare name".to_string());
            };
            let key = ident_key(ident);
            let name = &ident.sym;
            let Some((frame_index, part)) =
                self.frames
                    .iter()
                    .enumerate()
                    .rev()
                    .find_map(|(index, frame)| {
                        frame
                            .bindings
                            .iter()
                            .find(|(bound, _)| *bound == key)
                            .map(|(_, part)| (index, *part))
                    })
            else {
                return Err(format!(
                    "hole `${{{name}}}` is not bound by an enclosing loop"
                ));
            };
            if let Err(reason) = &self.frames[frame_index].read {
                return Err(format!(
                    "hole `${{{name}}}` is not a loop over values the source holds: {reason}"
                ));
            }
            reads.push((frame_index, part));
        }
        let mut used: Vec<usize> = reads.iter().map(|(frame, _)| *frame).collect();
        used.sort_unstable();
        used.dedup();
        let loops = used
            .iter()
            .map(|frame| {
                self.frames[*frame]
                    .read
                    .clone()
                    .expect("checked per hole above")
            })
            .collect();
        let holes = reads
            .iter()
            .map(|(frame, part)| {
                let position = used.binary_search(frame).expect("listed above");
                (position, *part)
            })
            .collect();
        Ok(LoopTemplate {
            quasis,
            holes,
            loops,
        })
    }
}

/// The callback a `.forEach(cb)` call hands its elements to, with its first
/// parameter: `(callee object, pattern)`.
fn for_each(call: &CallExpr) -> Option<(&Expr, &Pat)> {
    let Callee::Expr(callee) = &call.callee else {
        return None;
    };
    let Expr::Member(member) = unwrap_expression(callee) else {
        return None;
    };
    let MemberProp::Ident(name) = &member.prop else {
        return None;
    };
    if name.sym.as_ref() != "forEach" {
        return None;
    }
    let first = call.args.first().filter(|arg| arg.spread.is_none())?;
    let pat = match unwrap_expression(&first.expr) {
        Expr::Arrow(arrow) => arrow.params.first()?,
        Expr::Fn(function) => &function.function.params.first()?.pat,
        _ => return None,
    };
    Some((&member.obj, pat))
}

impl Visit for Collector<'_> {
    fn visit_for_of_stmt(&mut self, stmt: &ForOfStmt) {
        stmt.right.visit_with(self);
        let frame = match &stmt.left {
            _ if stmt.is_await => None,
            ForHead::VarDecl(decl) if decl.kind == VarDeclKind::Const && decl.decls.len() == 1 => {
                Some(self.frame(&decl.decls[0].name, &stmt.right))
            }
            ForHead::VarDecl(decl) => Some(Frame {
                bindings: decl
                    .decls
                    .iter()
                    .flat_map(|declarator| bound_keys(&declarator.name))
                    .map(|key| (key, 0))
                    .collect(),
                read: Err("the loop's binding is `let` or `var`".to_string()),
            }),
            _ => None,
        };
        match frame {
            Some(frame) => self.walk_in(frame, &|this| stmt.body.visit_with(this)),
            None => stmt.body.visit_with(self),
        }
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        if let Some((iterable, pat)) = for_each(call) {
            call.callee.visit_with(self);
            let frame = self.frame(pat, iterable);
            self.walk_in(frame, &|this| call.args.visit_with(this));
            return;
        }
        if let Some(first) = call.args.first()
            && first.spread.is_none()
            && let Expr::Tpl(tpl) = unwrap_expression(&first.expr)
            && !tpl.exprs.is_empty()
        {
            self.out.insert(call.span.lo.0, self.template(tpl));
        }
        call.visit_children_with(self);
    }
}

/// An array a module declares at top level: its elements, or why a loop over
/// it is unread.
pub type DeclaredArray = Result<Vec<Element>, String>;

/// Every top-level `const NAME = [ … ]` (exported or not) a module declares,
/// by name. Read for the module an imported loop array resolves to.
pub fn top_level_const_arrays(module: &Module) -> HashMap<String, DeclaredArray> {
    let tables = Tables::of(module);
    let mut arrays = HashMap::new();
    for item in &module.body {
        let decl = match item {
            ModuleItem::Stmt(Stmt::Decl(Decl::Var(decl))) => decl,
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl {
                decl: Decl::Var(decl),
                ..
            })) => decl,
            _ => continue,
        };
        if decl.kind != VarDeclKind::Const {
            continue;
        }
        for declarator in &decl.decls {
            let Pat::Ident(name) = &declarator.name else {
                continue;
            };
            let key = ident_key(&name.id);
            let read = if tables.written.contains(&key) {
                Err(format!(
                    "the declaring module writes through `{}`",
                    name.id.sym
                ))
            } else {
                tables.consts.get(&key).cloned().flatten().ok_or_else(|| {
                    format!(
                        "`{}` is not a const array literal of plain elements",
                        name.id.sym
                    )
                })
            };
            arrays.insert(name.id.sym.to_string(), read);
        }
    }
    arrays
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_source;
    use std::path::Path;
    use swc_common::{
        FileName, SourceMap,
        errors::{ColorConfig, Handler},
        sync::Lrc,
    };

    fn parse(source: &str) -> Module {
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
        let path = Path::new("routes.ts");
        parse_source(
            path,
            FileName::Real(path.to_path_buf()),
            source.to_string(),
            &cm,
            &handler,
        )
        .expect("parses")
    }

    /// The one template registration in `source`.
    fn only(source: &str) -> Result<LoopTemplate, String> {
        let templates = collect_loop_templates(&parse(source));
        assert_eq!(templates.len(), 1, "{templates:?}");
        templates.into_values().next().unwrap()
    }

    fn paths(source: &str) -> Vec<String> {
        only(source).expect("read").paths().expect("states paths")
    }

    fn unread(source: &str) -> String {
        match only(source) {
            Ok(template) => template
                .paths()
                .expect_err(&format!("{template:?} should not state paths")),
            Err(reason) => reason,
        }
    }

    #[test]
    fn a_same_file_const_array_states_one_path_per_element() {
        assert_eq!(
            paths(
                "const KINDS = ['daily', 'weekly'] as const;
                 for (const kind of KINDS) { app.get(`/reports/${kind}`, h); }"
            ),
            ["/reports/daily", "/reports/weekly"]
        );
    }

    #[test]
    fn an_inline_array_literal_is_read() {
        assert_eq!(
            paths("for (const v of ['a', `b`]) app.get(`/x/${v}`, h);"),
            ["/x/a", "/x/b"]
        );
    }

    #[test]
    fn a_for_each_callback_is_read() {
        assert_eq!(
            paths(
                "const PAGES = ['up', 'down'];
                 PAGES.forEach((page) => app.get(`/status/${page}`, h));"
            ),
            ["/status/up", "/status/down"]
        );
        assert_eq!(
            paths(
                "const PAGES = ['up'];
                 PAGES.forEach(function (page) { app.get(`/status/${page}`, h); });"
            ),
            ["/status/up"]
        );
    }

    #[test]
    fn nested_loops_take_the_product_outer_loop_first() {
        assert_eq!(
            paths(
                "for (const v of ['v1', 'v2']) for (const r of ['users', 'teams']) app.get(`/${v}/${r}`, h);"
            ),
            ["/v1/users", "/v1/teams", "/v2/users", "/v2/teams"]
        );
    }

    #[test]
    fn the_product_is_over_loops_not_holes() {
        assert_eq!(
            paths("for (const v of ['a', 'b']) app.get(`/${v}/${v}`, h);"),
            ["/a/a", "/b/b"]
        );
    }

    #[test]
    fn a_loop_no_hole_reads_adds_no_rows() {
        assert_eq!(
            paths(
                "for (const unused of ['1', '2', '3']) for (const r of ['a']) app.get(`/${r}`, h);"
            ),
            ["/a"]
        );
    }

    #[test]
    fn an_array_pattern_reads_each_position() {
        assert_eq!(
            paths(
                "for (const [file, , type] of [['a.svg', 0, 'image/svg'], ['b.png', 1, 'image/png']] as const) {
                   if (ok(file)) { app.get(`/${file}`, () => type); }
                 }"
            ),
            ["/a.svg", "/b.png"]
        );
    }

    #[test]
    fn an_object_pattern_reads_each_key() {
        assert_eq!(
            paths(
                "const ROUTES = [{ name: 'a', v: 'x' }, { name: 'b', v: 'y' }];
                 for (const { name, v: version } of ROUTES) app.get(`/${version}/${name}`, h);"
            ),
            ["/x/a", "/y/b"]
        );
    }

    #[test]
    fn equal_paths_fold() {
        assert_eq!(
            paths("for (const v of ['a', 'a', 'b']) app.get(`/${v}`, h);"),
            ["/a", "/b"]
        );
    }

    #[test]
    fn a_named_import_is_left_for_the_orchestrator() {
        let template = only(
            "import { ACTIONS as A } from './actions.js';
             for (const action of A) app.post(`/x/${action}`, h);",
        )
        .expect("read");
        assert_eq!(
            template.loops[0].source,
            LoopSource::Imported {
                specifier: "./actions.js".to_string(),
                exported_name: "ACTIONS".to_string(),
            }
        );
        assert!(template.paths().is_err());
    }

    #[test]
    fn more_than_the_cap_abstains() {
        let values: Vec<String> = (0..=MAX_ROWS).map(|n| format!("'v{n}'")).collect();
        let source = format!(
            "for (const v of [{}]) app.get(`/${{v}}`, h);",
            values.join(", ")
        );
        assert!(unread(&source).contains("more than"));
        let at_cap: Vec<String> = (0..MAX_ROWS).map(|n| format!("'v{n}'")).collect();
        let source = format!(
            "for (const v of [{}]) app.get(`/${{v}}`, h);",
            at_cap.join(", ")
        );
        assert_eq!(paths(&source).len(), MAX_ROWS);
    }

    #[test]
    fn an_empty_array_abstains() {
        assert!(unread("for (const v of []) app.get(`/${v}`, h);").contains("empty"));
    }

    #[test]
    fn loops_that_are_not_read_abstain() {
        for source in [
            "for (let v of ['a']) app.get(`/${v}`, h);",
            "for (var v of ['a']) app.get(`/${v}`, h);",
            "for (const v in { a: 1 }) app.get(`/${v}`, h);",
            "['a'].map((v) => app.get(`/${v}`, h));",
            "Object.keys({ a: 1 }).forEach((v) => app.get(`/${v}`, h));",
            "Object.values({ a: 'a' }).forEach((v) => app.get(`/${v}`, h));",
            "for (let i = 0; i < 2; i++) app.get(`/${i}`, h);",
            "async function f() { for await (const v of ['a']) app.get(`/${v}`, h); }",
        ] {
            unread(source);
        }
    }

    #[test]
    fn patterns_with_a_default_rest_or_computed_key_abstain() {
        for source in [
            "for (const [a = 'x'] of [['a']]) app.get(`/${a}`, h);",
            "for (const [...a] of [['a']]) app.get(`/${a}`, h);",
            "for (const { a = 'x' } of [{ a: 'a' }]) app.get(`/${a}`, h);",
            "for (const { ...a } of [{ a: 'a' }]) app.get(`/${a}`, h);",
            "for (const { ['a']: a } of [{ a: 'a' }]) app.get(`/${a}`, h);",
            "for (const [[a]] of [[['a']]]) app.get(`/${a}`, h);",
            "['a'].forEach((v = 'x') => app.get(`/${v}`, h));",
        ] {
            unread(source);
        }
    }

    #[test]
    fn iterables_the_source_does_not_state_abstain() {
        for source in [
            "for (const v of list()) app.get(`/${v}`, h);",
            "function f(opts) { for (const p of opts.paths) app.get(`/m${p}`, h); }",
            "function f(paths = ['a']) { for (const p of paths) app.get(`/${p}`, h); }",
            "import ACTIONS from './a.js'; for (const v of ACTIONS) app.get(`/${v}`, h);",
            "import * as NS from './a.js'; for (const v of NS.ACTIONS) app.get(`/${v}`, h);",
            "let LATER = ['a']; for (const v of LATER) app.get(`/${v}`, h);",
            "const A = ['a']; const B = A; for (const v of B) app.get(`/${v}`, h);",
        ] {
            unread(source);
        }
    }

    #[test]
    fn elements_that_are_not_literals_abstain() {
        for source in [
            "const BASE = ['a']; const MIXED = [...BASE, 'z']; for (const v of MIXED) app.get(`/${v}`, h);",
            "for (const v of ['a', , 'b']) app.get(`/${v}`, h);",
            // A spread is read as its own elements, never as one element.
            "for (const [v] of [...['ab']]) app.get(`/${v}`, h);",
            "const X = 'x'; for (const v of ['a', X]) app.get(`/${v}`, h);",
            "for (const v of ['a', `b${c}`]) app.get(`/${v}`, h);",
            "for (const [a, b] of [['a']]) app.get(`/${a}/${b}`, h);",
            "for (const { a } of [{ b: 'b' }]) app.get(`/${a}`, h);",
            "const a = 'a'; for (const { a } of [{ a }]) app.get(`/${a}`, h);",
            "for (const { a } of [{ a: 'a', ...rest }]) app.get(`/${a}`, h);",
        ] {
            unread(source);
        }
    }

    #[test]
    fn writes_through_the_array_abstain() {
        for write in [
            "GROWN.push('b');",
            "GROWN[0] = 'z';",
            "GROWN.length = 0;",
            "GROWN.splice(0, 1);",
            "GROWN.sort();",
            "GROWN.reverse();",
            "GROWN.fill('q');",
            "GROWN.copyWithin(0, 1);",
            "GROWN.unshift('q');",
            "GROWN.pop();",
            "GROWN.shift();",
            "delete GROWN[0];",
            "GROWN[0]++;",
        ] {
            let source = format!(
                "const GROWN = ['a', 'b']; {write} for (const v of GROWN) app.get(`/${{v}}`, h);"
            );
            assert!(unread(&source).contains("writes through"), "{write}");
        }
        // A write to an element's own array is a write through the array.
        unread("const T = [['a']]; T[0].push('b'); for (const [v] of T) app.get(`/${v}`, h);");
    }

    #[test]
    fn a_callback_that_assigns_its_binding_abstains() {
        unread("['a'].forEach((v) => { v = 'z'; app.get(`/${v}`, h); });");
    }

    #[test]
    fn a_shadow_is_a_different_binding() {
        // The block's own `ACTIONS` is not the outer array.
        unread(
            "const ACTIONS = ['a'];
             { const ACTIONS = load(); for (const v of ACTIONS) app.get(`/${v}`, h); }",
        );
        // The outer array is read where the inner one is not in scope.
        assert_eq!(
            paths(
                "const ACTIONS = ['a'];
                 { const ACTIONS = load(); use(ACTIONS); }
                 for (const v of ACTIONS) app.get(`/${v}`, h);"
            ),
            ["/a"]
        );
        // A name the loop body declares again is not the loop's binding.
        unread("for (const v of ['a']) { const w = (v) => app.get(`/${v}`, h); }");
    }

    #[test]
    fn holes_that_are_not_loop_bindings_abstain() {
        for source in [
            "const V = 'v1'; for (const r of ['a']) app.get(`/${V}/${r}`, h);",
            "for (const r of ['a']) app.get(`/${r.toUpperCase()}`, h);",
            "for (const r of [{ a: 'a' }]) app.get(`/${r.a}`, h);",
            "app.get(`/${anything}`, h);",
        ] {
            unread(source);
        }
    }

    #[test]
    fn a_top_level_const_array_is_read_for_importers() {
        let arrays = top_level_const_arrays(&parse(
            "export const ACTIONS = ['create', 'apply'] as const;
             const LOCAL = ['x'];
             export const GROWN = ['a']; GROWN.push('b');
             export const CALLED = load();
             export let LATER = ['a'];
             function f() { const INNER = ['no']; }",
        ));
        assert_eq!(
            arrays["ACTIONS"],
            Ok(vec![
                Element::Literal("create".to_string()),
                Element::Literal("apply".to_string())
            ])
        );
        assert!(arrays["LOCAL"].is_ok());
        assert!(arrays["GROWN"].is_err());
        assert!(arrays["CALLED"].is_err());
        assert!(!arrays.contains_key("LATER"));
        assert!(!arrays.contains_key("INNER"));
    }

    #[test]
    fn the_key_is_the_call_span_start_a_candidate_carries() {
        let source = "import { ACTIONS } from './a.js';
const KINDS = ['d', 'w'];
export async function routes(app: any) {
  for (const kind of KINDS) {
    app.get(`/reports/${kind}`, async () => []);
  }
  ACTIONS.forEach((action) => app.post(`/x/${action}`, async () => ({})));
}
";
        let path = Path::new("routes.ts");
        let scanned = crate::swc_scanner::SwcScanner::new().scan_content(path, source, &[], &[]);
        assert_eq!(scanned.loop_templates.len(), 2);
        for (span, template) in &scanned.loop_templates {
            assert!(template.is_ok(), "{template:?}");
            assert!(
                scanned
                    .candidates
                    .iter()
                    .any(|candidate| candidate.span_start == *span),
                "no candidate starts at {span}: {:?}",
                scanned
                    .candidates
                    .iter()
                    .map(|candidate| (candidate.line_number, candidate.span_start))
                    .collect::<Vec<_>>()
            );
        }
    }
}
