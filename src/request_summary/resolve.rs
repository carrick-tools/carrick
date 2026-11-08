//! The value resolver (carrick#3504): what a value may hold, read where it
//! is written and followed to where it is decided.
//!
//! Design: carrick-cloud
//! `docs/internal/decisions/2026-11-07-one-value-resolver.md` (sections 2.1
//! to 2.7 and 3). Step S0 lifted discovery's value walk (carrick#2432) here
//! unchanged, so every later reader grows from one walk. Step S1 added the
//! binding table and the one write census (section 2.4): a module binding
//! is its initialiser and every value written to it ([`ModuleBinding`]).
//!
//! Two halves:
//!
//! - **Lowering** ([`Reader::value_of`] and its siblings), at extract time:
//!   a callee that is a value rather than a definition (a parameter, a key
//!   of one, a parameter's default, a local set to `a ?? b`, a function an
//!   in-repo factory returns) becomes a [`ValueAt`] term. Nothing decides
//!   what a value is by its name: a name or a member is kept as its span,
//!   and the compiler's row there says what it holds.
//! - **Resolving** ([`Resolver`]), after the compiler rows are read: a term
//!   is followed through what holds it: what each caller passes at that
//!   position and key, the default when a caller passes nothing there, each
//!   side of a fallback, each construction of a class whose field it is,
//!   what an in-repo factory returns, `f.bind(x)` as `f`. The resolver never
//!   collapses the values it finds (the decision record's D4): the consumer
//!   applies its own agreement policy (discovery's is `agree`).
//!
//! The consumer turns each resolved end (a labelled export, an in-repo
//! definition, a function written in place) into its own result through
//! [`Follow`], at the point the walk reaches it, so the resolver's guards
//! (`fields_reading`, the returns cap) wrap that work exactly as they did
//! inside discovery.

use super::row_calls::Reach;
use super::*;
use crate::position_rows::{Kind, Row};
use std::cell::{Cell, RefCell};

mod documents;
mod text;
mod text_reader;

pub(crate) use documents::{DocumentEnd, DocumentLookup, document_calls};
pub(super) use text::{pattern_members, text_of};
pub(super) use text_reader::TextValue;

/// How many object literals deep an argument's keys are read (the decision
/// record's object-depth cap, D7).
const MAX_VALUE_DEPTH: usize = 4;

/// How many values one query holds, and how many in-repo factories' returns
/// one value may be read through (D7 sets the second at 8 hops; S0 keeps
/// today's 16 so nothing moves).
pub(super) const MAX_VALUES: usize = 16;

/// How many values one query reads before it answers that it cannot say
/// (the decision record's step cap, D7, set from the S0 census: at most 917
/// steps over 57,099 queries in the largest repo read). A query over it
/// answers one value nothing reads, never the part it read.
const MAX_QUERY_STEPS: usize = 2048;

/// A binding and every value written to it (the decision record's section
/// 2.4): a module-scope `const`, `let` or `var` (carrick#3504 S1), or a
/// function's own `let` or `var` its body writes again (S1b). Its
/// initialiser and each assignment the write census places. Keyed in
/// [`FileIr::module_bindings`] by the 1-based line its name is declared on
/// and the name, as a compiler row names a declaration.
#[derive(Debug, Clone, Default)]
pub(super) struct ModuleBinding {
    /// The function whose body declares it, by its key in
    /// [`FileIr::functions`]; `None` for a module binding. A parameter its
    /// initialiser reads is that function's, read at its callers.
    pub(super) declared_in: Option<String>,
    /// What the initialiser may hold, read in module scope. Empty when the
    /// declaration has none.
    pub(super) init: Vec<ValueAt>,
    /// Where each plain assignment to the binding starts (`name = value`),
    /// anywhere in the module: its value is in the [`BindingWrite`] of the
    /// body it is written in.
    pub(super) writes: Vec<u32>,
    /// Something writes the binding in a form whose value no reader states:
    /// a compound or logical assignment, an update, a destructuring target,
    /// a loop head, a second declaration, or `with` or `eval` in the module
    /// (soundness rule 1). An importer cannot assign a module's binding
    /// (rule 2), so the module's own census is complete.
    pub(super) unplaceable: bool,
    /// How it is declared.
    pub(super) kind: BindingKind,
    /// What the initialiser states as text ([`text::text_of`], carrick#3504
    /// S2), or an enum's members. `None` for a declaration with no
    /// initialiser.
    pub(super) text: Option<ValueAt>,
    /// The module only ever reads the object the binding holds through its
    /// entries: nothing writes the binding, an entry of it, or exports it,
    /// and every reference to it is an entry read. Only then is an entry's
    /// value the one its literal states.
    pub(super) entries_kept: bool,
    /// The binding is in the table for the text reader alone: a function's
    /// own local its body never writes again. The scope already holds it for
    /// every other reader (`scope.held`), so [`Resolver::binding_values`]
    /// does not answer for it.
    pub(super) text_only: bool,
}

impl ModuleBinding {
    /// Whether the value readers (discovery's callee walk, the receiver
    /// walk) read this entry: an enum holds no value, and a text-only local
    /// is held by its scope.
    pub(super) fn values_read(&self) -> bool {
        !matches!(self.kind, BindingKind::Enum | BindingKind::StaticField) && !self.text_only
    }
}

/// How a binding is declared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum BindingKind {
    #[default]
    Const,
    Let,
    Var,
    /// A TypeScript enum: `text` holds its members.
    Enum,
    /// A `static` property of a class declared at module scope, keyed by the
    /// property key's line and name (carrick#3504 S2b-6): `text` is its
    /// initialiser as text, `None` where the class or property may change
    /// after it is declared.
    StaticField,
}

impl From<VarDeclKind> for BindingKind {
    fn from(kind: VarDeclKind) -> Self {
        match kind {
            VarDeclKind::Const => Self::Const,
            VarDeclKind::Let => Self::Let,
            VarDeclKind::Var => Self::Var,
        }
    }
}

/// A function's own `let`, `var` or `const` declared once in its body
/// (carrick#3504 S1b, S2), with its initialiser read in the body's scope:
/// the binding table takes it under the function's key, with the census's
/// writes.
#[derive(Debug, Clone)]
pub(super) struct LocalBinding {
    pub(super) key: BindingKey,
    /// The 1-based line its name is declared on.
    pub(super) line: u32,
    pub(super) init: Vec<ValueAt>,
    pub(super) kind: BindingKind,
    /// What the initialiser states as text (carrick#3504 S2).
    pub(super) text: Option<ValueAt>,
    /// The body writes the name again: the value readers read it (S1b).
    /// Otherwise the scope holds it and only the text reader needs it here.
    pub(super) reassigned: bool,
}

/// A placed assignment `name = value`, `name ??= value` or `name ||= value`
/// written directly in a body (not in a function written inside it), with
/// what the value may hold read in that body's scope.
#[derive(Debug, Clone)]
pub(super) struct BindingWrite {
    /// Where the assignment starts.
    pub(super) at: u32,
    pub(super) values: Vec<ValueAt>,
    /// What the value states as text (carrick#3504 S2b-4): the text reader
    /// reads a written `let` as its initialiser and each write.
    pub(super) text: ValueAt,
}

/// Every write of every binding in a module, by binding (the one write
/// census, section 2.4). Syntax alone: it places where each plain
/// assignment starts and marks every other form of write.
#[derive(Default)]
struct WriteCensus {
    writes: HashMap<BindingKey, Vec<u32>>,
    unplaceable: HashSet<BindingKey>,
    declared: HashMap<BindingKey, usize>,
    /// `with` or a direct `eval` may write any binding.
    opaque: bool,
    /// How many times a binding is named, its declaration's own name
    /// included (carrick#3504 S2, the escape census).
    references: HashMap<BindingKey, usize>,
    /// How many of those are the object of an entry read (`R.a`, `R[k]`).
    entry_reads: HashMap<BindingKey, usize>,
    /// An entry of the binding is written, updated, deleted or called as a
    /// method (`R.a = x`, `R.a++`, `delete R.a`, `R.list.push(x)`): the
    /// object it holds may change.
    entry_escapes: HashSet<BindingKey>,
    /// How many of the references are `new C(…)` (carrick#3504 S2b-6).
    constructions: HashMap<BindingKey, usize>,
    /// How many of the references are `class D extends C`.
    extensions: HashMap<BindingKey, usize>,
    /// The key of every member something writes, updates or deletes
    /// anywhere in the module (`X.P = v`, `this.P++`, `delete X.P`, a
    /// pattern target): a static field with such a key may change.
    written_keys: HashSet<String>,
    /// A member with a computed key is written: any static field may change.
    written_computed: bool,
}

/// Every binding a pattern writes.
#[derive(Default)]
struct PatternNames(Vec<BindingKey>);

impl Visit for PatternNames {
    fn visit_binding_ident(&mut self, binding: &BindingIdent) {
        self.0.push(ident_key(&binding.id));
    }
}

/// The name a wrapped assignment target writes (`(s)`, `s!`, `s as T`),
/// when the wrappers hold a bare name and no member.
struct WrappedName(Option<BindingKey>);

impl Visit for WrappedName {
    fn visit_ident(&mut self, ident: &Ident) {
        self.0.get_or_insert_with(|| ident_key(ident));
    }
    fn visit_member_expr(&mut self, _: &MemberExpr) {}
    fn visit_ts_type(&mut self, _: &TsType) {}
}

impl WriteCensus {
    fn mark(&mut self, pat: &impl VisitWith<PatternNames>) {
        let mut names = PatternNames::default();
        pat.visit_with(&mut names);
        self.unplaceable.extend(names.0);
    }

    /// Notes that an entry of the binding at the root of `expr` may change.
    fn escape(&mut self, expr: &Expr) {
        if let Some(key) = entry_root(expr) {
            self.entry_escapes.insert(key);
        }
    }

    /// Whether the module only ever reads the object `key` holds through
    /// its entries, and no importer or other holder can change it
    /// (carrick#3504 S2): the binding is not exported, nothing writes it or
    /// one of its entries, and every reference but its declaration is an
    /// entry read. An entry passed on (`f(R.a)`) is a string leaf to the
    /// text reader: no callee can change it.
    fn entries_kept(&self, key: &BindingKey, exported: bool) -> bool {
        let count = |counts: &HashMap<BindingKey, usize>| counts.get(key).copied().unwrap_or(0);
        !exported
            && !self.is_unplaceable(key)
            && !self.writes.contains_key(key)
            && !self.entry_escapes.contains(key)
            && count(&self.references) == count(&self.entry_reads) + 1
    }

    /// Notes that the member `expr` names (if it is one) is written.
    fn write_member(&mut self, expr: &Expr) {
        let mut expr = expr;
        loop {
            match expr {
                Expr::Paren(paren) => expr = &paren.expr,
                Expr::TsNonNull(inner) => expr = &inner.expr,
                Expr::TsAs(inner) => expr = &inner.expr,
                _ => break,
            }
        }
        if let Expr::Member(member) = expr {
            match &member.prop {
                MemberProp::Ident(prop) => {
                    self.written_keys.insert(prop.sym.to_string());
                }
                MemberProp::PrivateName(_) => {}
                MemberProp::Computed(_) => self.written_computed = true,
            }
        }
    }

    /// Whether nothing in the module changes the static field `key` of the
    /// class `class` after its declaration (carrick#3504 S2b-6): the class
    /// is not exported, nothing writes it or one of its entries, and every
    /// reference to it is an entry read, a construction or an `extends`; and
    /// no member with that key is written anywhere (`D.P = v` on a subclass,
    /// `this.P = v` in a static method).
    fn static_field_kept(&self, class: &BindingKey, key: &str, exported: bool) -> bool {
        let count = |counts: &HashMap<BindingKey, usize>| counts.get(class).copied().unwrap_or(0);
        !exported
            && !self.is_unplaceable(class)
            && !self.writes.contains_key(class)
            && !self.entry_escapes.contains(class)
            && !self.written_computed
            && !self.written_keys.contains(key)
            && count(&self.references)
                == count(&self.entry_reads)
                    + count(&self.constructions)
                    + count(&self.extensions)
                    + 1
    }

    /// A write the readers cannot place: a compound or `&&=` assignment,
    /// an update, a destructuring target, a loop head, a second
    /// declaration, or `with` or `eval` in the module (soundness rule 1).
    fn is_unplaceable(&self, key: &BindingKey) -> bool {
        self.opaque
            || self.unplaceable.contains(key)
            || self.declared.get(key).copied().unwrap_or(0) > 1
    }
}

/// The binding at the root of an entry chain: `R` in `R.a`, `R.a.b`,
/// `R.list[0]`, `(R.a)!`.
fn entry_root(expr: &Expr) -> Option<BindingKey> {
    match expr {
        Expr::Member(member) => member_root(member),
        Expr::Paren(paren) => entry_root(&paren.expr),
        Expr::TsNonNull(inner) => entry_root(&inner.expr),
        _ => None,
    }
}

/// [`entry_root`] of a member read.
fn member_root(member: &MemberExpr) -> Option<BindingKey> {
    match &*member.obj {
        Expr::Ident(ident) => Some(ident_key(ident)),
        other => entry_root(other),
    }
}

/// The assignment operators whose right side is a value the binding holds:
/// `=`, and `??=` and `||=`, which keep the earlier value or take the new
/// one (carrick#3504 S2b-4). `&&=`, `+=` and the other compound forms
/// compute a value no reader states.
pub(super) fn is_placed_assign(op: AssignOp) -> bool {
    matches!(
        op,
        AssignOp::Assign | AssignOp::NullishAssign | AssignOp::OrAssign
    )
}

impl Visit for WriteCensus {
    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        match &assign.left {
            AssignTarget::Simple(SimpleAssignTarget::Ident(binding)) => {
                let key = ident_key(&binding.id);
                if is_placed_assign(assign.op) {
                    self.writes.entry(key).or_default().push(assign.span.lo.0);
                } else {
                    self.unplaceable.insert(key);
                }
            }
            AssignTarget::Pat(pat) => self.mark(pat),
            // `R.a = x`: an entry of `R` changes.
            AssignTarget::Simple(SimpleAssignTarget::Member(member)) => {
                if let Some(key) = member_root(member) {
                    self.entry_escapes.insert(key);
                }
                self.write_member(&Expr::Member(member.clone()));
            }
            // `(s) = …`, `s! = …`, `(s as T) = …`: the binding is written in
            // a form no reader places. A member target writes a property,
            // not the binding.
            AssignTarget::Simple(
                SimpleAssignTarget::Paren(_)
                | SimpleAssignTarget::TsAs(_)
                | SimpleAssignTarget::TsSatisfies(_)
                | SimpleAssignTarget::TsNonNull(_)
                | SimpleAssignTarget::TsTypeAssertion(_)
                | SimpleAssignTarget::TsInstantiation(_),
            ) => {
                let mut names = WrappedName(None);
                assign.left.visit_with(&mut names);
                if let Some(key) = names.0 {
                    self.unplaceable.insert(key);
                }
            }
            _ => {}
        }
        assign.visit_children_with(self);
    }

    fn visit_update_expr(&mut self, update: &UpdateExpr) {
        if let Expr::Ident(ident) = crate::graphql_document_sites::unwrap_expression(&update.arg) {
            self.unplaceable.insert(ident_key(ident));
        }
        self.escape(&update.arg);
        self.write_member(&update.arg);
        update.visit_children_with(self);
    }

    fn visit_new_expr(&mut self, new: &NewExpr) {
        if let Expr::Ident(ident) = &*new.callee {
            *self.constructions.entry(ident_key(ident)).or_default() += 1;
        }
        new.visit_children_with(self);
    }

    fn visit_class(&mut self, class: &Class) {
        if let Some(Expr::Ident(ident)) = class.super_class.as_deref() {
            *self.extensions.entry(ident_key(ident)).or_default() += 1;
        }
        class.visit_children_with(self);
    }

    fn visit_pat(&mut self, pat: &Pat) {
        // `[X.P] = …`, `({ a: X.P } = …)`, `for (X.P of …)`.
        if let Pat::Expr(expr) = pat {
            self.write_member(expr);
        }
        pat.visit_children_with(self);
    }

    fn visit_unary_expr(&mut self, unary: &UnaryExpr) {
        if unary.op == UnaryOp::Delete {
            self.escape(&unary.arg);
            self.write_member(&unary.arg);
        }
        unary.visit_children_with(self);
    }

    fn visit_member_expr(&mut self, member: &MemberExpr) {
        if let Expr::Ident(ident) = &*member.obj {
            *self.entry_reads.entry(ident_key(ident)).or_default() += 1;
        }
        member.visit_children_with(self);
    }

    fn visit_ident(&mut self, ident: &Ident) {
        *self.references.entry(ident_key(ident)).or_default() += 1;
    }

    fn visit_for_head(&mut self, head: &ForHead) {
        if let ForHead::Pat(pat) = head {
            self.mark(&**pat);
        }
        head.visit_children_with(self);
    }

    fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
        let mut names = PatternNames::default();
        declarator.name.visit_with(&mut names);
        for name in names.0 {
            *self.declared.entry(name).or_default() += 1;
        }
        // `const { a } = OBJ` reads keys of `OBJ`, as `OBJ.a` does
        // (carrick#3504 S2b-3).
        if let (Pat::Object(_), Some(init)) = (&declarator.name, &declarator.init)
            && let Expr::Ident(source) = crate::graphql_document_sites::unwrap_expression(init)
        {
            *self.entry_reads.entry(ident_key(source)).or_default() += 1;
        }
        declarator.visit_children_with(self);
    }

    fn visit_with_stmt(&mut self, with: &WithStmt) {
        self.opaque = true;
        with.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        if let Callee::Expr(callee) = &call.callee {
            if matches!(&**callee, Expr::Ident(ident) if ident.sym == "eval") {
                self.opaque = true;
            }
            // `R.list.push(x)`, `R.reverse()`: a method called on an entry
            // may change the object. An entry passed as an argument is not
            // counted: the text reader reads only string entries, which no
            // callee can change.
            self.escape(callee);
        }
        call.visit_children_with(self);
    }

    fn visit_ts_type(&mut self, _: &TsType) {}
}

/// Each module-scope binding a plain name declares, with its initialiser
/// read in module scope and every write the census finds (carrick#3504 S1),
/// each function's own `let` its body writes again, under the function's key
/// (S1b), and what each states as text (S2): every plain-name local a top
/// level definition's own body declares once, and every enum at any depth.
/// A local in a function written inside that body, and a declaration in a
/// block at module scope, are unread. Two bindings declared on one line with
/// one name are each unplaceable: no row says which one a use means.
pub(super) fn module_bindings(
    module: &Module,
    source_map: &Lrc<SourceMap>,
    reader: &Reader<'_>,
    scope: &Scope<'_>,
    functions: &HashMap<String, FnIr>,
) -> HashMap<(u32, String), ModuleBinding> {
    let mut census = WriteCensus::default();
    module.visit_with(&mut census);
    // Discovery numbering is the extract's own: spans need no shift.
    let identity = |lo: u32| lo;
    let mut out: HashMap<(u32, String), ModuleBinding> = HashMap::new();
    for item in &module.body {
        if let Some((class, exported)) = module_class_decl(item) {
            static_fields(class, exported, &census, source_map, &mut out);
            continue;
        }
        let Some(var) = module_var_decl(item) else {
            continue;
        };
        let exported = matches!(item, ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(_)));
        for declarator in &var.decls {
            // `const { a, b: name } = init`: each plain key is a binding of
            // its own (carrick#3504 S2b-3). The text reader reads its text;
            // the value entry is the member read, held for the value
            // readers that take destructured bindings later (S4).
            if let (Pat::Object(pattern), Some(init)) =
                (&declarator.name, declarator.init.as_deref())
            {
                for member in pattern_members(pattern) {
                    let key = ident_key(&member.bound.id);
                    let line = source_map.lookup_char_pos(member.bound.id.span.lo).line as u32;
                    let read = member.read(init);
                    let binding = ModuleBinding {
                        declared_in: None,
                        init: reader.value_of(&read, scope, &guards::Bound::default()),
                        writes: census.writes.get(&key).cloned().unwrap_or_default(),
                        unplaceable: census.is_unplaceable(&key),
                        kind: var.kind.into(),
                        text: member.text(init, &identity),
                        entries_kept: false,
                        text_only: true,
                    };
                    out.insert((line, member.bound.id.sym.to_string()), binding);
                }
                continue;
            }
            let Pat::Ident(ident) = &declarator.name else {
                continue;
            };
            let key = ident_key(&ident.id);
            let line = source_map.lookup_char_pos(ident.id.span.lo).line as u32;
            let init = declarator.init.as_deref();
            let binding = ModuleBinding {
                declared_in: None,
                init: init
                    .map(|init| reader.value_of(init, scope, &guards::Bound::default()))
                    .unwrap_or_default(),
                writes: census.writes.get(&key).cloned().unwrap_or_default(),
                unplaceable: census.is_unplaceable(&key),
                kind: var.kind.into(),
                text: init.map(|init| text_of(init, &identity)),
                entries_kept: census.entries_kept(&key, exported),
                text_only: false,
            };
            out.insert((line, ident.id.sym.to_string()), binding);
        }
    }
    let mut enums = Enums {
        source_map,
        found: Vec::new(),
    };
    module.visit_with(&mut enums);
    for (at, text) in enums.found {
        out.entry(at).or_insert_with(|| ModuleBinding {
            kind: BindingKind::Enum,
            text: Some(text),
            ..ModuleBinding::default()
        });
    }
    let mut keys: Vec<&String> = functions.keys().collect();
    keys.sort();
    for function in keys {
        for local in &functions[function].local_bindings {
            let binding = ModuleBinding {
                declared_in: Some(function.clone()),
                init: local.init.clone(),
                writes: census.writes.get(&local.key).cloned().unwrap_or_default(),
                unplaceable: census.is_unplaceable(&local.key),
                kind: local.kind,
                text: local.text.clone(),
                entries_kept: census.entries_kept(&local.key, false),
                text_only: !local.reassigned,
            };
            match out.entry((local.line, local.key.0.clone())) {
                std::collections::hash_map::Entry::Occupied(mut taken) => {
                    taken.get_mut().unplaceable = true;
                }
                std::collections::hash_map::Entry::Vacant(free) => {
                    free.insert(binding);
                }
            }
        }
    }
    out
}

/// The class a module item declares, and whether it is exported inline.
fn module_class_decl(item: &ModuleItem) -> Option<(&ClassDecl, bool)> {
    match item {
        ModuleItem::Stmt(Stmt::Decl(Decl::Class(class))) => Some((class, false)),
        ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => match &export.decl {
            Decl::Class(class) => Some((class, true)),
            _ => None,
        },
        _ => None,
    }
}

/// A plain (identifier) key a class member names.
fn plain_key(key: &PropName) -> Option<&str> {
    match key {
        PropName::Ident(ident) => Some(&ident.sym),
        _ => None,
    }
}

/// Each `static` property with a plain key and an initialiser the class
/// declares, as a [`BindingKind::StaticField`] entry keyed by the key's line
/// and name (carrick#3504 S2b-6). Its text is set only where nothing changes
/// the field after the declaration ([`WriteCensus::static_field_kept`]), the
/// property is the only static member with its key, and the class has no
/// decorator, static block or accessor; else the entry has no text.
fn static_fields(
    class: &ClassDecl,
    exported: bool,
    census: &WriteCensus,
    source_map: &Lrc<SourceMap>,
    out: &mut HashMap<(u32, String), ModuleBinding>,
) {
    let class_key = ident_key(&class.ident);
    let body = &class.class;
    let plain = body.decorators.is_empty()
        && !body.body.iter().any(|member| {
            matches!(
                member,
                ClassMember::StaticBlock(_) | ClassMember::AutoAccessor(_)
            )
        });
    let static_members_named = |name: &str| {
        body.body
            .iter()
            .filter(|member| match member {
                ClassMember::ClassProp(prop) => {
                    prop.is_static && plain_key(&prop.key) == Some(name)
                }
                ClassMember::Method(method) => {
                    method.is_static && plain_key(&method.key) == Some(name)
                }
                _ => false,
            })
            .count()
    };
    for member in &body.body {
        let ClassMember::ClassProp(prop) = member else {
            continue;
        };
        let (true, Some(name), Some(value)) = (prop.is_static, plain_key(&prop.key), &prop.value)
        else {
            continue;
        };
        let line = source_map.lookup_char_pos(prop.key.span().lo).line as u32;
        let kept = plain
            && !prop.declare
            && prop.decorators.is_empty()
            && static_members_named(name) == 1
            && census.static_field_kept(&class_key, name, exported);
        out.insert(
            (line, name.to_string()),
            ModuleBinding {
                kind: BindingKind::StaticField,
                text: kept.then(|| text_of(value, &|lo| lo)),
                ..ModuleBinding::default()
            },
        );
    }
}

/// Every enum a module declares, at any depth, by the line and name of its
/// id, with its members as text.
struct Enums<'m> {
    source_map: &'m Lrc<SourceMap>,
    found: Vec<((u32, String), ValueAt)>,
}

impl Visit for Enums<'_> {
    fn visit_ts_enum_decl(&mut self, decl: &TsEnumDecl) {
        let line = self.source_map.lookup_char_pos(decl.id.span.lo).line as u32;
        self.found.push((
            (line, decl.id.sym.to_string()),
            text::enum_text(decl, &|lo| lo),
        ));
        decl.visit_children_with(self);
    }
}

/// One value a callee, an argument, a parameter's default or a `return` may
/// hold, read where it is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ValueAt {
    /// A parameter, `depth` functions out from the body that reads it (0: its
    /// own), at `keys` inside it: a caller decides what it holds.
    Param {
        depth: u8,
        index: usize,
        keys: Vec<String>,
    },
    /// A name or a member read here, by its span (discovery numbering): the
    /// compiler's row there says what it holds. `holds` is what the receiver
    /// walk reads a name as ([`Holds`]); every other reader ignores it.
    Read {
        lo: u32,
        hi: u32,
        member: bool,
        holds: Holds,
    },
    /// `this.<field>` (spanning `lo..hi`) in a member of the class whose
    /// span starts at `class`, a field its constructor sets from a parameter
    /// (carrick#2452): what each construction of the class passes there.
    /// Read as the member it is where the class is not read through its
    /// constructions.
    Field {
        lo: u32,
        hi: u32,
        class: u32,
        field: String,
    },
    /// What the call spanning `lo..hi` returns. A call of a member
    /// (`f.bind(x)`) carries the member read's span and what its object may
    /// hold, for a member the language's own library declares to return its
    /// object (carrick#2432 slice A2). `out`: how many functions the body
    /// that reads the value is inside the one the call is written in
    /// (carrick#3504 S0b): a parameter that body's maker call passes is
    /// that many functions further out.
    ///
    /// `made` is the instance the call builds where a maker or an own
    /// factory makes it ([`Reader::written_instance`]): the receiver walk
    /// ends there. Every other reader ignores it.
    Returned {
        lo: u32,
        hi: u32,
        out: u8,
        member: Option<Box<MemberCall>>,
        made: Option<Box<ClientRef>>,
    },
    /// A `new` of a maker ([`Reader::written_instance`]) spanning `lo..hi`:
    /// the instance it builds, which only the receiver walk reads. Every
    /// other reader reads it as [`Self::Unread`]; any other `new` is one.
    New {
        lo: u32,
        hi: u32,
        made: Box<ClientRef>,
    },
    /// A function written here, by where its node starts.
    Function(u32),
    /// An object literal: each key's values, and whether a spread or a
    /// computed key may put any other key in it.
    Object {
        fields: BTreeMap<String, Vec<ValueAt>>,
        open: bool,
    },
    /// Text a name states (carrick#3504 S2, [`text::text_of`]): only the
    /// text reader ([`Resolver::text_values`]) reads the variants below.
    /// Discovery's lowering never produces them, and every other reader
    /// treats them as [`Self::Unread`].
    ///
    /// A string literal's text.
    Lit(String),
    /// Parts joined: a template's quasis and expressions, or `a + b`.
    Concat(Vec<ValueAt>),
    /// A member read, `object.key`, spanning `lo..hi` (the key is a
    /// [`Self::Lit`] for `.name`, else what the computed key states).
    Member {
        lo: u32,
        hi: u32,
        object: Box<ValueAt>,
        key: Box<ValueAt>,
    },
    /// `object.method(args)` spanning `lo..hi`, for `join`, `concat`,
    /// `replace` and `replaceAll` (and `raw` for ``String.raw`…` ``, whose
    /// object is the template's raw text): the text reader applies it only
    /// where the compiler's row says it is the language library's.
    BuiltIn {
        lo: u32,
        hi: u32,
        method: String,
        object: Box<ValueAt>,
        args: Vec<ValueAt>,
    },
    /// A choice the code writes in place (`c ? a : b`, `a ?? b`, `a || b`):
    /// every branch is a value the name may take, never collapsed.
    Alt { branches: Vec<ValueAt> },
    /// An object literal's entries in source order; a key may be computed.
    Entries(Vec<(ValueAt, ValueAt)>),
    /// An array literal's elements, the object of a `join`.
    Elements(Vec<ValueAt>),
    /// Anything else.
    Unread,
}

/// What the receiver walk reads a name as, decided where the name is read
/// (carrick#3504 S3a). Only the walk reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) enum Holds {
    /// A member read, a parameter, a name a function declares, or a name no
    /// declaration the module states binds: no instance the walk names.
    #[default]
    Nothing,
    /// An import, or a binding the module declares (a `const`, a `let` other
    /// than a lazy client a getter builds, a destructured key, a module
    /// instance a scope below declares again): the declaration the
    /// compiler's row names is read for the instance it holds.
    Declared,
    /// The library instance the scope holds in the name ([`Scope::receiver`]):
    /// a maker's or an own factory's.
    Made(Box<ClientRef>),
}

/// A call of a member, `object.method(…)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MemberCall {
    /// The member read `object.method`, by its span (discovery numbering).
    pub(super) lo: u32,
    pub(super) hi: u32,
    /// How many arguments the call passes.
    pub(super) args: usize,
    /// What the object may hold.
    pub(super) object: Vec<ValueAt>,
}

impl ValueAt {
    /// The same value, as a function written inside the one that reads it
    /// reads it: each parameter is one function further out.
    pub(super) fn enclosed(&self) -> Self {
        match self {
            Self::Param { depth, index, keys } => Self::Param {
                depth: depth.saturating_add(1),
                index: *index,
                keys: keys.clone(),
            },
            Self::Object { fields, open } => Self::Object {
                fields: fields
                    .iter()
                    .map(|(key, values)| (key.clone(), values.iter().map(Self::enclosed).collect()))
                    .collect(),
                open: *open,
            },
            Self::Returned {
                lo,
                hi,
                out,
                member,
                made,
            } => Self::Returned {
                lo: *lo,
                hi: *hi,
                out: out.saturating_add(1),
                member: member.as_ref().map(|call| {
                    Box::new(MemberCall {
                        object: call.object.iter().map(Self::enclosed).collect(),
                        ..(**call).clone()
                    })
                }),
                made: made.clone(),
            },
            other => other.clone(),
        }
    }

    /// The values at `keys` inside this one, read where it is written. `None`
    /// when a closed object literal holds nothing under a key: the caller
    /// wrote nothing there.
    pub(super) fn at(&self, path: &[String]) -> Option<Vec<ValueAt>> {
        let Some((key, rest)) = path.split_first() else {
            return Some(vec![self.clone()]);
        };
        match self {
            Self::Param { depth, index, keys } => Some(vec![Self::Param {
                depth: *depth,
                index: *index,
                keys: keys.iter().chain(path).cloned().collect(),
            }]),
            Self::Object { fields, open } => match fields.get(key) {
                Some(values) => {
                    let mut out = Vec::new();
                    for value in values {
                        out.extend(value.at(rest).unwrap_or_else(|| vec![Self::Unread]));
                    }
                    Some(out)
                }
                None if *open => Some(vec![Self::Unread]),
                None => None,
            },
            // A key of a value read elsewhere has no span of its own to read
            // a row at.
            _ => Some(vec![Self::Unread]),
        }
    }
}

/// What the receiver walk reads the name `ident` as, where the scope holds
/// no value for it ([`Holds`]): the instance the scope holds in it, else a
/// binding the module declares or imports, else nothing (a parameter, or a
/// name a function declares).
fn name_holds(ident: &Ident, scope: &Scope<'_>) -> Holds {
    if scope.param_index(ident).is_some() {
        return Holds::Nothing;
    }
    if let Some(client) = scope.receiver(ident)
        && client.instance.is_some()
    {
        return Holds::Made(Box::new(client.clone()));
    }
    let key = ident_key(ident);
    let name = ident.sym.as_ref();
    if scope.locals.contains_key(&key) || scope.local_receivers.contains_key(&key) {
        return Holds::Nothing;
    }
    let module = &scope.module;
    let declared = module.imports.contains(name)
        || module.shadowed_clients.contains(name)
        || module.destructured.contains(name)
        || module.consts.contains_key(&key)
        // A module `let` (carrick#3489 W5) other than a lazy client a getter
        // builds (carrick#1790): no call is read through that one.
        || (module.lets.contains(&key) && !module.lazy_clients.contains_key(&key));
    if declared {
        Holds::Declared
    } else {
        Holds::Nothing
    }
}

/// Each key of a destructured parameter, with the parameter's position and
/// the key (`{ send = fetch }`, `{ fetch: fetchImpl }`), and each default a
/// parameter or one of its keys is given, read in the function's own scope.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ParamValues {
    pub(super) held: HashMap<BindingKey, Vec<ValueAt>>,
    /// `(position, keys, the default's values)`.
    pub(super) defaults: Vec<(usize, Vec<String>, Vec<ValueAt>)>,
}

impl Reader<'_> {
    /// What `expr` may hold, each side of a fallback (`a ?? b`, `a || b`)
    /// one value.
    pub(super) fn value_of(
        &self,
        expr: &Expr,
        scope: &Scope<'_>,
        bound: &guards::Bound,
    ) -> Vec<ValueAt> {
        self.values_in(expr, scope, bound, 0)
    }

    fn values_in(
        &self,
        expr: &Expr,
        scope: &Scope<'_>,
        bound: &guards::Bound,
        depth: usize,
    ) -> Vec<ValueAt> {
        if depth > MAX_VALUE_DEPTH {
            return vec![ValueAt::Unread];
        }
        let expr = crate::graphql_document_sites::unwrap_expression(expr);
        match expr {
            Expr::Ident(ident) => {
                if let Some(held) = scope.held.get(&ident_key(ident)) {
                    return held.clone();
                }
                match self.subject(expr, scope, bound).parameter() {
                    Some((depth, index, keys)) => vec![ValueAt::Param { depth, index, keys }],
                    None => vec![ValueAt::Read {
                        lo: ident.span.lo.0,
                        hi: ident.span.hi.0,
                        member: false,
                        holds: name_holds(ident, scope),
                    }],
                }
            }
            // `await createApp()`: what the awaited expression holds.
            Expr::Await(awaited) => self.values_in(&awaited.arg, scope, bound, depth + 1),
            // `new Queue(…)`: the instance a maker builds; any other `new`
            // holds nothing a reader states.
            Expr::New(_) => match self.written_instance(expr, scope) {
                Some(made) => vec![ValueAt::New {
                    lo: expr.span().lo.0,
                    hi: expr.span().hi.0,
                    made: Box::new(made),
                }],
                None => vec![ValueAt::Unread],
            },
            Expr::Member(member) => {
                // A field of the class the member is written in. One its
                // constructor sets from a parameter is what each
                // construction passes there; any other is read as the member
                // it is ([`Resolver::field_values`] answers `None`), and the
                // receiver walk reads what the class writes to it.
                if matches!(
                    crate::graphql_document_sites::unwrap_expression(&member.obj),
                    Expr::This(_)
                ) && let Some(fields) = scope.fields
                    && let Some(field) = this_field(member)
                {
                    return vec![ValueAt::Field {
                        lo: member.span.lo.0,
                        hi: member.span.hi.0,
                        class: fields.class,
                        field,
                    }];
                }
                let key = match &member.prop {
                    MemberProp::Ident(prop) => Some(prop.sym.to_string()),
                    MemberProp::Computed(computed) => match &*computed.expr {
                        Expr::Lit(Lit::Str(text)) => {
                            Some(text.value.to_string_lossy().into_owned())
                        }
                        _ => None,
                    },
                    MemberProp::PrivateName(_) => None,
                };
                if let Some(key) = key
                    && !matches!(
                        crate::graphql_document_sites::unwrap_expression(&member.obj),
                        Expr::This(_)
                    )
                {
                    let base = self.values_in(&member.obj, scope, bound, depth + 1);
                    if !base.is_empty()
                        && base
                            .iter()
                            .all(|value| matches!(value, ValueAt::Param { .. }))
                    {
                        return base
                            .iter()
                            .filter_map(|value| value.at(std::slice::from_ref(&key)))
                            .flatten()
                            .collect();
                    }
                }
                vec![ValueAt::Read {
                    lo: member.span.lo.0,
                    hi: member.span.hi.0,
                    member: true,
                    holds: Holds::Nothing,
                }]
            }
            Expr::Bin(bin)
                if matches!(bin.op, BinaryOp::NullishCoalescing | BinaryOp::LogicalOr) =>
            {
                let mut out = self.values_in(&bin.left, scope, bound, depth + 1);
                out.extend(self.values_in(&bin.right, scope, bound, depth + 1));
                out
            }
            Expr::Arrow(arrow) => vec![ValueAt::Function(arrow.span.lo.0)],
            Expr::Fn(function) => vec![ValueAt::Function(function.function.span.lo.0)],
            Expr::Call(call) if matches!(call.callee, Callee::Expr(_)) => {
                let member = match &call.callee {
                    Callee::Expr(callee) => {
                        match crate::graphql_document_sites::unwrap_expression(callee) {
                            Expr::Member(member) if matches!(member.prop, MemberProp::Ident(_)) => {
                                Some(Box::new(MemberCall {
                                    lo: member.span.lo.0,
                                    hi: member.span.hi.0,
                                    args: call.args.len(),
                                    object: self.values_in(&member.obj, scope, bound, depth + 1),
                                }))
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                vec![ValueAt::Returned {
                    lo: call.span.lo.0,
                    hi: call.span.hi.0,
                    out: 0,
                    member,
                    made: self.written_instance(expr, scope).map(Box::new),
                }]
            }
            Expr::Object(object) => {
                let mut fields: BTreeMap<String, Vec<ValueAt>> = BTreeMap::new();
                let mut open = false;
                for prop in &object.props {
                    let PropOrSpread::Prop(prop) = prop else {
                        open = true;
                        continue;
                    };
                    match &**prop {
                        Prop::KeyValue(kv) => match prop_name(&kv.key) {
                            Some(key) => {
                                fields.insert(
                                    key,
                                    self.values_in(&kv.value, scope, bound, depth + 1),
                                );
                            }
                            None => open = true,
                        },
                        Prop::Shorthand(ident) => {
                            let value = Expr::Ident(ident.clone());
                            fields.insert(
                                ident.sym.to_string(),
                                self.values_in(&value, scope, bound, depth + 1),
                            );
                        }
                        Prop::Method(method) => match prop_name(&method.key) {
                            Some(key) => {
                                fields.insert(
                                    key,
                                    vec![ValueAt::Function(method.function.span.lo.0)],
                                );
                            }
                            None => open = true,
                        },
                        _ => open = true,
                    }
                }
                vec![ValueAt::Object { fields, open }]
            }
            _ => vec![ValueAt::Unread],
        }
    }

    /// What each argument may hold, by position. A spread is an empty entry
    /// and ends the list: it moves every position from it on.
    pub(super) fn arg_values(
        &self,
        args: &[ExprOrSpread],
        scope: &Scope<'_>,
        bound: &guards::Bound,
    ) -> Vec<Vec<ValueAt>> {
        let mut out = Vec::new();
        for arg in args {
            if arg.spread.is_some() {
                out.push(Vec::new());
                break;
            }
            out.push(self.value_of(&arg.expr, scope, bound));
        }
        out
    }

    /// What a call's callee may hold, when it is a value: a parameter, a key
    /// of one, a local holding another value, or each side of a fallback.
    /// Empty for a callee read as itself (a function's name, a member of an
    /// object the module declares, `this.<member>`): the rows' call target
    /// says what that reaches.
    pub(super) fn callee_values(
        &self,
        callee: &Expr,
        scope: &Scope<'_>,
        bound: &guards::Bound,
    ) -> Vec<ValueAt> {
        let span = crate::graphql_document_sites::unwrap_expression(callee).span();
        let values = self.value_of(callee, scope, bound);
        let itself = |value: &ValueAt| matches!(value, ValueAt::Read { lo, hi, .. } if *lo == span.lo.0 && *hi == span.hi.0);
        if values.iter().all(itself) {
            return Vec::new();
        }
        values
    }

    /// A call written as `f.call(…)` or `f.apply(…)`: what `f` may hold and,
    /// for `apply`, the elements of the array it hands on (carrick#3504
    /// S4d). Undecided: only the compiler's row says the member is the
    /// language's ([`super::RequestSummaryInputs::read_language_calls`]).
    pub(super) fn language_call(
        &self,
        callee: &Expr,
        args: &[ExprOrSpread],
        scope: &Scope<'_>,
        bound: &guards::Bound,
    ) -> Option<Box<super::LanguageCall>> {
        let Expr::Member(member) = crate::graphql_document_sites::unwrap_expression(callee) else {
            return None;
        };
        let MemberProp::Ident(prop) = &member.prop else {
            return None;
        };
        let applied = match prop.sym.as_ref() {
            "call" => None,
            "apply" => Some(self.applied(args.get(1), scope, bound)),
            _ => return None,
        };
        Some(Box::new(super::LanguageCall {
            callee: self.value_of(&member.obj, scope, bound),
            applied,
            decided: false,
        }))
    }

    /// What `apply`'s second argument hands on by position: each element of
    /// an array literal written there, unread from a spread or a hole on.
    fn applied(
        &self,
        arg: Option<&ExprOrSpread>,
        scope: &Scope<'_>,
        bound: &guards::Bound,
    ) -> super::Applied {
        let elements = match arg {
            Some(ExprOrSpread { spread: None, expr }) => {
                match crate::graphql_document_sites::unwrap_expression(expr) {
                    Expr::Array(array) => Some(&array.elems),
                    _ => None,
                }
            }
            _ => None,
        };
        // No argument at all hands nothing: every position is unwritten.
        if arg.is_none() {
            return super::Applied::default();
        }
        let Some(elements) = elements else {
            return super::Applied {
                args: Vec::new(),
                open_from: Some(0),
                arg_values: vec![Vec::new()],
            };
        };
        let mut out = super::Applied::default();
        for element in elements {
            let Some(ExprOrSpread { spread: None, expr }) = element else {
                out.open_from = Some(out.args.len());
                out.arg_values.push(Vec::new());
                break;
            };
            out.args.push(
                match crate::graphql_document_sites::unwrap_expression(expr) {
                    // A function written in place is no text.
                    Expr::Arrow(_) | Expr::Fn(_) => Value::Str(vec![Piece::Unknown]),
                    expr => self.eval(expr, scope),
                },
            );
            out.arg_values.push(self.value_of(expr, scope, bound));
        }
        out
    }

    /// The functions written as a parameter's default (`doFetch = (url, init)
    /// => fetch(url, init)`), positional or at a destructured key, read the
    /// way [`Self::param_values`] records them as [`ValueAt::Function`]. No
    /// body states them (carrick#2813), so the value walk finds them here.
    pub(super) fn param_functions<'p>(
        &self,
        params: impl IntoIterator<Item = &'p Pat>,
        fields: Option<&ClassFields>,
        module: &ModuleScope,
        captured: &Captured,
    ) -> Vec<FnIr> {
        let mut out = Vec::new();
        let mut read = |expr: &Expr| match expr {
            Expr::Arrow(arrow) => out.push(self.arrow(arrow, fields, module, captured)),
            Expr::Fn(function) => {
                out.push(self.function(&function.function, fields, module, captured));
            }
            _ => {}
        };
        for pat in params {
            let pattern = match pat {
                Pat::Assign(assign) => {
                    if matches!(&*assign.left, Pat::Ident(_)) {
                        read(&assign.right);
                    }
                    &*assign.left
                }
                other => other,
            };
            let Pat::Object(object) = pattern else {
                continue;
            };
            for prop in &object.props {
                match prop {
                    ObjectPatProp::Assign(assign) => {
                        if let Some(default) = assign.value.as_deref() {
                            read(default);
                        }
                    }
                    ObjectPatProp::KeyValue(kv) => {
                        if let Pat::Assign(assign) = &*kv.value
                            && matches!(&*assign.left, Pat::Ident(_))
                            && prop_name(&kv.key).is_some()
                        {
                            read(&assign.right);
                        }
                    }
                    ObjectPatProp::Rest(_) => {}
                }
            }
        }
        out
    }

    /// The keys each destructured parameter holds and the defaults the
    /// parameters are given, read in `scope` (the function's own, its
    /// parameters read). A binding the body assigns again holds nothing it
    /// was handed.
    pub(super) fn param_values<'p>(
        &self,
        params: impl IntoIterator<Item = &'p Pat>,
        scope: &Scope<'_>,
    ) -> ParamValues {
        let mut out = ParamValues::default();
        let bound = guards::Bound::default();
        for (index, pat) in params.into_iter().enumerate() {
            let pattern = match pat {
                Pat::Assign(assign) => {
                    if matches!(&*assign.left, Pat::Ident(_)) {
                        out.defaults.push((
                            index,
                            Vec::new(),
                            self.value_of(&assign.right, scope, &bound),
                        ));
                    }
                    &*assign.left
                }
                other => other,
            };
            let Pat::Object(object) = pattern else {
                continue;
            };
            for prop in &object.props {
                let (binding, key, default) = match prop {
                    ObjectPatProp::Assign(assign) => (
                        &assign.key.id,
                        assign.key.id.sym.to_string(),
                        assign.value.as_deref(),
                    ),
                    ObjectPatProp::KeyValue(kv) => {
                        let Some(key) = prop_name(&kv.key) else {
                            continue;
                        };
                        match &*kv.value {
                            Pat::Ident(binding) => (&binding.id, key, None),
                            Pat::Assign(assign) => match &*assign.left {
                                Pat::Ident(binding) => (&binding.id, key, Some(&*assign.right)),
                                _ => continue,
                            },
                            _ => continue,
                        }
                    }
                    ObjectPatProp::Rest(_) => continue,
                };
                let held = if scope.reassigned.contains(binding.sym.as_ref()) {
                    vec![ValueAt::Unread]
                } else {
                    vec![ValueAt::Param {
                        depth: 0,
                        index,
                        keys: vec![key.clone()],
                    }]
                };
                out.held.insert(ident_key(binding), held);
                if let Some(default) = default {
                    out.defaults
                        .push((index, vec![key], self.value_of(default, scope, &bound)));
                }
            }
        }
        out
    }
}

/// The call that invokes a value, where it is written.
#[derive(Debug, Clone, Copy)]
pub(super) struct Invoke<'a> {
    pub(super) file: &'a Path,
    pub(super) call: &'a CallIr,
    pub(super) construction: bool,
}

/// One value a position may hold, read (carrick#2432). `R` is what the
/// consumer made of a resolved end ([`Follow`]).
#[derive(Debug, Clone)]
pub(super) enum Held<R> {
    /// A parameter of the body it is read in: how many functions out, its
    /// position and keys. The callers of that body decide it.
    Param(u8, usize, Vec<String>),
    /// A function written at a caller, or a parameter of a function written
    /// in the body: what runs it decides, which no caller here states.
    Callback,
    /// An end the consumer followed; `R::default()` when the value holds
    /// nothing the resolver can read.
    Reached(R),
}

/// What a consumer makes of each end the resolver reaches, at the point it
/// reaches it.
pub(super) trait Follow<'a> {
    type Reached: Default;

    /// The name or member read at `lo..hi` in `file`, when the consumer
    /// knows what it is from the compiler's row there (discovery: a
    /// labelled export). `None` reads the row as a definition.
    fn export(
        &mut self,
        file: &'a Path,
        lo: u32,
        hi: u32,
        kind: Kind,
        invoke: Invoke<'a>,
    ) -> Option<Self::Reached>;

    /// The value is the in-repo definition `key` of `target`.
    fn definition(&mut self, target: PathBuf, key: String, invoke: Invoke<'a>) -> Self::Reached;

    /// The value is `function`, written in place in `file`. `maker`: the
    /// call of the factory that returned it, when the function is written
    /// directly in that factory's body (carrick#3504 S0b).
    fn written(
        &mut self,
        file: &'a Path,
        function: &'a FnIr,
        invoke: Invoke<'a>,
        maker: Option<Maker<'a>>,
    ) -> Self::Reached;
}

/// The call of an own factory that made the function a value holds
/// (carrick#3504 S0b, rule 2.7's per-caller fan-out through a closure):
/// what fills a parameter of the factory the function reads.
#[derive(Debug, Clone, Copy)]
pub(super) struct Maker<'a> {
    /// The factory call, where it is written.
    pub(super) call: Invoke<'a>,
    /// The factory, with the file it is written in.
    pub(super) factory: (&'a Path, &'a FnIr),
    /// How many functions the body that runs the made function is inside
    /// the one the factory call is written in: a parameter the call passes
    /// is that many further out. `None` where the call is not written in a
    /// body around the one that runs the function: read through a module
    /// binding (written in another function), or returned by another
    /// factory (`return make(o)`, written in that factory). A parameter it
    /// passes is then read as nothing, as before the step.
    pub(super) out: Option<u8>,
}

/// A callee's parameters' defaults, with the file they are read in.
pub(super) type Defaults<'a> = (&'a Path, &'a [(usize, Vec<String>, Vec<ValueAt>)]);

/// Whether `function` is written directly in `factory`'s body, one function
/// in: a parameter it reads one function out is the factory's.
pub(super) fn written_in(factory: &FnIr, function: &FnIr) -> bool {
    factory
        .nested
        .iter()
        .chain(&factory.detached)
        .any(|inner| std::ptr::eq(inner, function))
}

pub(super) fn defaults_of<'a>(callee: Option<(&'a Path, &'a FnIr)>) -> Option<Defaults<'a>> {
    callee.map(|(file, callee)| (file, callee.param_defaults.as_slice()))
}

/// One per service run, built after the compiler rows are read: the one
/// caller index and the constructions, and the guards one query holds.
pub(super) struct Resolver<'a> {
    inputs: &'a RequestSummaryInputs,
    /// The run's other services, in declared order (carrick#2618): a file of
    /// one of them is read with that service's own inputs.
    siblings: &'a [&'a RequestSummaryInputs],
    /// Each function's callers, through what the rows say each call reaches:
    /// what fills a parameter a value is read from (carrick#2432).
    /// Built on first use: the text reader never asks.
    callers: std::cell::OnceCell<super::decided_sites::Calls<'a>>,
    /// Each class of the service read through its constructions, with them
    /// (carrick#2452): what fills a field its constructor sets from a
    /// parameter.
    /// Built on first use: the text reader never asks.
    constructed: std::cell::OnceCell<super::constructions::Constructed<'a>>,
    /// How many own factories' returns the value being read is inside.
    returns_depth: Cell<usize>,
    /// The fields being read through their constructions (carrick#2452): a
    /// construction that passes a field of a class whose field is being
    /// read (`new Fork({ send: this.send })` in a method of `Fork`) adds
    /// nothing.
    fields_reading: RefCell<HashSet<(&'a Path, u32, String)>>,
    /// The step census that sets the decision record's per-query step cap
    /// (D7). It counts and never stops a read.
    census: Cell<Census>,
    /// How many resolver calls are open, and the step count when the
    /// outermost one opened.
    open: Cell<(usize, usize)>,
    /// The open query has read more than [`MAX_QUERY_STEPS`] values: every
    /// read until it closes holds nothing, and it answers so.
    capped: Cell<bool>,
    /// The step cap: [`MAX_QUERY_STEPS`], lowered by tests.
    cap: usize,
    /// The module bindings being read (carrick#3504 S1): a binding written
    /// with its own value adds nothing (soundness rule 7).
    bindings_reading: RefCell<HashSet<(PathBuf, u32, String)>>,
    /// The declarations being read for their text (carrick#3504 S2): a
    /// value that reads itself holds no text.
    texts_reading: RefCell<HashSet<(PathBuf, u32, String)>>,
}

/// What the resolver read (carrick#3504 S0): every value read, how many
/// queries (outermost resolver calls, the ends their consumer followed
/// inside them included), and the most values one query read.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Census {
    pub(super) steps: usize,
    pub(super) queries: usize,
    pub(super) max_query_steps: usize,
    /// The queries that read more than [`MAX_QUERY_STEPS`] values and
    /// answered that they cannot say.
    pub(super) capped_queries: usize,
}

/// One open resolver call; the outermost one closing ends a query.
struct Query<'r, 'a>(&'r Resolver<'a>);

impl Drop for Query<'_, '_> {
    fn drop(&mut self) {
        let (depth, start) = self.0.open.get();
        self.0.open.set((depth - 1, start));
        if depth == 1 {
            let mut census = self.0.census.get();
            census.queries += 1;
            census.max_query_steps = census.max_query_steps.max(census.steps - start);
            if self.0.capped.get() {
                census.capped_queries += 1;
                self.0.capped.set(false);
            }
            self.0.census.set(census);
        }
    }
}

impl<'a> Resolver<'a> {
    pub(super) fn new(
        inputs: &'a RequestSummaryInputs,
        siblings: &'a [&'a RequestSummaryInputs],
    ) -> Self {
        Resolver {
            inputs,
            siblings,
            callers: std::cell::OnceCell::new(),
            constructed: std::cell::OnceCell::new(),
            returns_depth: Cell::new(0),
            fields_reading: RefCell::new(HashSet::new()),
            census: Cell::new(Census::default()),
            open: Cell::new((0, 0)),
            capped: Cell::new(false),
            cap: MAX_QUERY_STEPS,
            bindings_reading: RefCell::new(HashSet::new()),
            texts_reading: RefCell::new(HashSet::new()),
        }
    }

    /// Each function's callers ([`Self::callers`]'s index).
    fn callers(&self) -> &super::decided_sites::Calls<'a> {
        self.callers
            .get_or_init(|| super::decided_sites::Calls::through_reach(self.inputs))
    }

    /// Each class read through its constructions.
    fn constructed(&self) -> &super::constructions::Constructed<'a> {
        self.constructed
            .get_or_init(|| super::constructions::constructed(self.inputs))
    }

    /// The same resolver with a lower step cap.
    #[cfg(test)]
    pub(super) fn with_cap(mut self, cap: usize) -> Self {
        self.cap = cap;
        self
    }

    /// Whether the open query is over the step cap: what it reads now is
    /// not all the value holds, so no consumer keeps it.
    pub(super) fn capping(&self) -> bool {
        self.capped.get()
    }

    /// Counts one value read; `true` once the open query is over the cap.
    fn step(&self) -> bool {
        let mut census = self.census.get();
        census.steps += 1;
        self.census.set(census);
        if census.steps - self.open.get().1 > self.cap {
            self.capped.set(true);
        }
        self.capped.get()
    }

    /// What a resolver call answers: what it read, or, when it is the
    /// outermost call of a query over the cap, one value nothing reads.
    fn settle<R: Default>(&self, out: Vec<Held<R>>) -> Vec<Held<R>> {
        if self.open.get().0 == 1 && self.capped.get() {
            return vec![Held::Reached(R::default())];
        }
        out
    }

    /// What the resolver has read so far.
    pub(super) fn census(&self) -> Census {
        self.census.get()
    }

    /// Opens a resolver call for the census.
    fn enter(&self) -> Query<'_, 'a> {
        let (depth, start) = self.open.get();
        let start = if depth == 0 {
            self.capped.set(false);
            self.census.get().steps
        } else {
            start
        };
        self.open.set((depth + 1, start));
        Query(self)
    }

    /// The inputs that hold `file`: this service's, else the first sibling's
    /// that does.
    pub(super) fn inputs_for(&self, file: &Path) -> &'a RequestSummaryInputs {
        super::discovery::inputs_holding(self.inputs, self.siblings, file)
    }

    /// The definition `key` of `file`, the file as `inputs` keys it.
    pub(super) fn function(&self, file: &Path, key: &str) -> Option<(&'a Path, &'a FnIr)> {
        let (path, ir) = self.inputs_for(file).files.get_key_value(file)?;
        Some((path.as_path(), ir.functions.get(key)?))
    }

    /// The function written in `file` whose node starts at `start`, at any
    /// depth of any body.
    pub(super) fn written_function(&self, file: &Path, start: u32) -> Option<&'a FnIr> {
        fn find(ir: &FnIr, start: u32) -> Option<&FnIr> {
            if ir.site.as_ref().is_some_and(|site| site.lo == start) {
                return Some(ir);
            }
            ir.nested
                .iter()
                .chain(&ir.detached)
                .chain(&ir.param_functions)
                .find_map(|inner| find(inner, start))
        }
        let ir = self.inputs_for(file).files.get(file)?;
        let mut keys: Vec<&String> = ir.functions.keys().collect();
        keys.sort();
        keys.into_iter()
            .map(|key| &ir.functions[key])
            .chain(std::iter::once(&ir.module_level))
            .chain(&ir.constructor_defaults)
            .find_map(|body| find(body, start))
    }

    /// The call written in `file` whose node starts at `start` (discovery
    /// numbering, as [`ValueAt::Returned`] holds it), at any depth of any
    /// body.
    pub(super) fn written_call(&self, file: &Path, start: u32) -> Option<&'a CallIr> {
        fn find(ir: &FnIr, start: u32) -> Option<&CallIr> {
            ir.read_calls()
                .map(|(call, _)| call)
                .find(|call| call.site.lo == start)
                .or_else(|| {
                    ir.nested
                        .iter()
                        .chain(&ir.detached)
                        .chain(&ir.param_functions)
                        .find_map(|inner| find(inner, start))
                })
        }
        let ir = self.inputs_for(file).files.get(file)?;
        let mut keys: Vec<&String> = ir.functions.keys().collect();
        keys.sort();
        keys.into_iter()
            .map(|key| &ir.functions[key])
            .chain(std::iter::once(&ir.module_level))
            .chain(&ir.constructor_defaults)
            .find_map(|body| find(body, start))
    }

    /// What `value`, read in a body of `file`, holds when `invoke` runs it
    /// (carrick#2432): a parameter of that body is left for its callers; a
    /// name or member is what `follow` says its row is, else the in-repo
    /// definition the row names; a function written in place is that
    /// function. `at_caller` reads a function written in place as a
    /// callback, whatever runs it filling it.
    pub(super) fn read_value<F: Follow<'a>>(
        &self,
        follow: &mut F,
        file: &'a Path,
        value: &ValueAt,
        invoke: Invoke<'a>,
        at_caller: bool,
    ) -> Vec<Held<F::Reached>> {
        let _query = self.enter();
        if self.step() {
            return self.settle(Vec::new());
        }
        let out = self.read_in(follow, file, value, invoke, at_caller);
        self.settle(out)
    }

    fn read_in<F: Follow<'a>>(
        &self,
        follow: &mut F,
        file: &'a Path,
        value: &ValueAt,
        invoke: Invoke<'a>,
        at_caller: bool,
    ) -> Vec<Held<F::Reached>> {
        let nothing = || vec![Held::Reached(F::Reached::default())];
        match value {
            ValueAt::Param { depth, index, keys } => {
                vec![Held::Param(*depth, *index, keys.clone())]
            }
            ValueAt::Field {
                lo,
                hi,
                class,
                field,
            } => match self.field_values(follow, file, *class, field, invoke) {
                Some(held) => held,
                None => {
                    let read = ValueAt::Read {
                        lo: *lo,
                        hi: *hi,
                        member: true,
                        holds: Holds::Nothing,
                    };
                    self.read_value(follow, file, &read, invoke, at_caller)
                }
            },
            ValueAt::Read { lo, hi, member, .. } => {
                let kind = if *member { Kind::Member } else { Kind::Ident };
                if let Some(reached) = follow.export(file, *lo, *hi, kind, invoke) {
                    return vec![Held::Reached(reached)];
                }
                match self.inputs_for(file).row_names(file, *lo, *hi, kind) {
                    Reach::Function(target, key) => {
                        vec![Held::Reached(follow.definition(target, key, invoke))]
                    }
                    // A name the compiler declares as a module binding is
                    // every value written to it (carrick#3504 S1).
                    _ if *member => nothing(),
                    _ => self
                        .row_at(file, *lo, *hi, kind)
                        .and_then(|row| self.declared_binding(row))
                        .and_then(|(path, line, symbol)| {
                            self.binding_values(follow, path, line, &symbol, invoke)
                        })
                        .unwrap_or_else(nothing),
                }
            }
            ValueAt::Returned {
                lo, hi, out, member, ..
            } => {
                // `f.bind(this)` is `f`: the member the compiler's row names
                // as the language library's `bind` returns its object.
                if let Some(call) = member
                    && call.args == 1
                    && self.is_library_bind(file, call.lo, call.hi)
                {
                    let mut out = Vec::new();
                    for object in &call.object {
                        out.extend(self.read_value(follow, file, object, invoke, at_caller));
                    }
                    if !out.is_empty() {
                        return out;
                    }
                }
                let Reach::Function(target, key) = self.inputs_for(file).reach(file, *lo, *hi)
                else {
                    return nothing();
                };
                let Some((factory_file, factory)) = self.function(&target, &key) else {
                    return nothing();
                };
                if self.returns_depth.get() >= MAX_VALUES {
                    return nothing();
                }
                // The factory call itself fills the parameters of the
                // factory a function it returns reads (carrick#3504 S0b).
                let maker = self.written_call(file, *lo).map(|call| Maker {
                    call: Invoke {
                        file,
                        call,
                        construction: false,
                    },
                    factory: (factory_file, factory),
                    out: (self.bindings_reading.borrow().is_empty()
                        && self.returns_depth.get() == 0)
                        .then_some(*out),
                });
                self.returns_depth.set(self.returns_depth.get() + 1);
                let mut held = Vec::new();
                for returned in &factory.returned_values {
                    match returned {
                        // A parameter of the factory: its own call fills it.
                        ValueAt::Param { .. } => held.extend(nothing()),
                        // A function the factory's own body writes: one
                        // function in, so its parameters are the factory's.
                        ValueAt::Function(start) => {
                            match self
                                .written_function(factory_file, *start)
                                .filter(|function| written_in(factory, function))
                            {
                                Some(function) => held.push(Held::Reached(follow.written(
                                    factory_file,
                                    function,
                                    invoke,
                                    maker,
                                ))),
                                None => held.extend(self.read_value(
                                    follow,
                                    factory_file,
                                    returned,
                                    invoke,
                                    false,
                                )),
                            }
                        }
                        other => {
                            held.extend(self.read_value(follow, factory_file, other, invoke, false))
                        }
                    }
                }
                self.returns_depth.set(self.returns_depth.get() - 1);
                if held.is_empty() { nothing() } else { held }
            }
            ValueAt::Function(_) if at_caller => vec![Held::Callback],
            ValueAt::Function(start) => match self.written_function(file, *start) {
                Some(function) => vec![Held::Reached(follow.written(file, function, invoke, None))],
                None => nothing(),
            },
            // The text variants are the names lowering's: this walk reads
            // none of them (carrick#3504 S2).
            ValueAt::Object { .. }
            | ValueAt::Lit(_)
            | ValueAt::Concat(_)
            | ValueAt::Member { .. }
            | ValueAt::BuiltIn { .. }
            | ValueAt::Alt { .. }
            | ValueAt::Entries(_)
            | ValueAt::Elements(_)
            // A maker's `new` is the receiver walk's alone.
            | ValueAt::New { .. }
            | ValueAt::Unread => nothing(),
        }
    }

    /// Whether the member read at `lo..hi` in `file` is a `bind` the
    /// language's own library declares: the compiler's row there names a
    /// declaration in a `lib:` file.
    fn is_library_bind(&self, file: &Path, lo: u32, hi: u32) -> bool {
        let inputs = self.inputs_for(file);
        let (Some(positions), Some(ir)) = (inputs.positions.as_ref(), inputs.files.get(file))
        else {
            return false;
        };
        positions
            .lookup(file, ir.swc_start(lo), ir.swc_start(hi), Kind::Member)
            .ok()
            .is_some_and(|row| {
                row.symbol() == Some("bind")
                    && row
                        .decl_file()
                        .is_some_and(|declared| declared.starts_with("lib:"))
            })
    }

    /// The constructions of the class whose span starts at `class` in
    /// `file`, and where its constructor sets `field` from, when the class
    /// is read through its constructions (carrick#2452). A class no file of
    /// the service constructs, and that nothing refused, comes back with no
    /// constructions: its constructor alone is read (carrick#2808).
    fn field_origin(
        &self,
        file: &'a Path,
        class: u32,
        field: &str,
    ) -> Option<(
        &'a super::constructions::ClassOrigin,
        &'a super::constructions::FieldOrigin,
        Vec<super::decided_sites::Placed<'a>>,
    )> {
        let constructed = self.constructed();
        let built = match constructed.built.get(&(file, class)) {
            Some(built) => built.clone(),
            None if constructed.refused.contains(&(file, class)) => return None,
            None => Vec::new(),
        };
        let origin = self.inputs.files.get(file)?.class_origins.get(&class)?;
        let from = origin.fields.get(field)?;
        Some((origin, from, built))
    }

    /// What `field` of the class whose span starts at `class` in `file` may
    /// hold when `invoke` runs it (carrick#2452): each value the constructor
    /// sets it from, a parameter read at every construction as a call's
    /// argument is, and from the construction's own callers where it hands
    /// its own parameter on. `None` when the class is not read through its
    /// constructions: the member is read as itself.
    pub(super) fn field_values<F: Follow<'a>>(
        &self,
        follow: &mut F,
        file: &'a Path,
        class: u32,
        field: &str,
        invoke: Invoke<'a>,
    ) -> Option<Vec<Held<F::Reached>>> {
        let _query = self.enter();
        let (origin, from, built) = self.field_origin(file, class, field)?;
        let reading = (file, class, field.to_string());
        if !self.fields_reading.borrow_mut().insert(reading.clone()) {
            return Some(Vec::new());
        }
        let defaults: Defaults<'a> = (file, origin.defaults.as_slice());
        let mut out = Vec::new();
        for value in &from.values {
            match value {
                ValueAt::Param {
                    depth: 0,
                    index,
                    keys,
                } => {
                    // No construction in the service (carrick#2808): the
                    // parameter holds its default, as a function nothing
                    // calls does.
                    if built.is_empty() {
                        out.extend(self.passed(follow, None, *index, keys, Some(defaults), invoke));
                    }
                    for placed in &built {
                        let at = Invoke {
                            file: placed.file,
                            call: placed.call,
                            construction: true,
                        };
                        for held in
                            self.passed(follow, Some(at), *index, keys, Some(defaults), invoke)
                        {
                            match held {
                                Held::Param(out_depth, index, keys) => {
                                    out.extend(self.callers_pass(
                                        follow,
                                        (placed.file, placed.key, placed.depth),
                                        (out_depth, index, &keys),
                                        invoke,
                                        &mut Vec::new(),
                                    ));
                                }
                                other => out.push(other),
                            }
                        }
                    }
                }
                // A parameter of a function the constructor is written in.
                ValueAt::Param { .. } => out.push(Held::Reached(F::Reached::default())),
                other => out.extend(self.read_value(follow, file, other, invoke, false)),
            }
        }
        self.fields_reading.borrow_mut().remove(&reading);
        Some(self.settle(out))
    }

    /// Whether every value `call`'s callee may hold is a field whose class
    /// is not read through its constructions: the callee is read as itself,
    /// as a member is.
    pub(super) fn unbound_fields(&self, file: &'a Path, call: &CallIr) -> bool {
        call.callee_values.iter().all(|value| {
            matches!(value, ValueAt::Field { class, field, .. }
                if self.field_origin(file, *class, field).is_none())
        })
    }

    /// What the call `at` hands its callee's parameter `index` at `keys`:
    /// each value it writes there, read where `at` is written, or the
    /// callee's default where it writes nothing (or there is no call, `at`
    /// `None`). Empty when nothing is handed and there is no default.
    /// `defaults` are the callee's parameters' defaults, read in its file
    /// (a function's [`FnIr::param_defaults`], or a constructor's).
    pub(super) fn passed<F: Follow<'a>>(
        &self,
        follow: &mut F,
        at: Option<Invoke<'a>>,
        index: usize,
        keys: &[String],
        defaults: Option<Defaults<'a>>,
        invoke: Invoke<'a>,
    ) -> Vec<Held<F::Reached>> {
        let _query = self.enter();
        let mut written: Vec<ValueAt> = Vec::new();
        let mut defaulted = at.is_none();
        if let Some(at) = at {
            // Shifted past `this` for the language's `call` and `apply`
            // (carrick#3504 S4d).
            let args = at.call.passed_values();
            // A spread (an empty entry) at or before the position moves it.
            let spread = args.iter().take(index + 1).any(Vec::is_empty);
            match args.get(index) {
                _ if spread => written.push(ValueAt::Unread),
                Some(values) => {
                    for value in values {
                        match value.at(keys) {
                            Some(values) => written.extend(values),
                            None => defaulted = true,
                        }
                    }
                }
                None => defaulted = true,
            }
        }
        written.dedup();
        let mut out = Vec::new();
        if let Some(at) = at {
            for value in &written {
                out.extend(self.read_value(follow, at.file, value, invoke, true));
            }
        }
        if defaulted && let Some((file, defaults)) = defaults {
            for (position, path, values) in defaults {
                if *position != index || !keys.starts_with(path) {
                    continue;
                }
                for value in values {
                    for value in value.at(&keys[path.len()..]).unwrap_or_default() {
                        match value {
                            // A default read from another parameter.
                            ValueAt::Param { .. } => out.push(Held::Reached(F::Reached::default())),
                            other => {
                                out.extend(self.read_value(follow, file, &other, invoke, false))
                            }
                        }
                    }
                }
            }
        }
        self.settle(out)
    }

    /// The values the parameter `param` (how many functions out of the body
    /// `at`, its position and keys) holds, read at every caller of the
    /// definition that body is in, and from theirs where a caller passes a
    /// parameter of its own. `passing` holds the definitions on the way: a
    /// cycle that passes the parameter on unchanged adds nothing (the
    /// decision record's soundness rule 7).
    pub(super) fn callers_pass<F: Follow<'a>>(
        &self,
        follow: &mut F,
        at: (&'a Path, Option<&'a str>, u8),
        param: (u8, usize, &[String]),
        invoke: Invoke<'a>,
        passing: &mut Vec<(PathBuf, String)>,
    ) -> Vec<Held<F::Reached>> {
        let _query = self.enter();
        let (file, key, depth) = at;
        let (out, index, keys) = param;
        // A parameter of a function written in the body: what runs it
        // fills it.
        if out < depth {
            return vec![Held::Callback];
        }
        let Some(key) = key.filter(|_| out == depth) else {
            return vec![Held::Reached(F::Reached::default())];
        };
        let id = (file.to_path_buf(), key.to_string());
        if passing.contains(&id) {
            return Vec::new();
        }
        passing.push(id);
        let callee = defaults_of(self.function(file, key));
        let callers: Vec<_> = self.callers().callers_of(file, key).to_vec();
        let mut values = Vec::new();
        if callers.is_empty() {
            // Nothing calls it: what it holds is its default, if any.
            values.extend(self.passed(follow, None, index, keys, callee, invoke));
        }
        for caller in callers {
            let at = Invoke {
                file: caller.file,
                call: caller.call,
                construction: false,
            };
            for value in self.passed(follow, Some(at), index, keys, callee, invoke) {
                match value {
                    Held::Param(out, index, keys) => values.extend(self.callers_pass(
                        follow,
                        (caller.file, caller.key, caller.depth),
                        (out, index, &keys),
                        invoke,
                        passing,
                    )),
                    other => values.push(other),
                }
            }
        }
        passing.pop();
        self.settle(values)
    }

    /// The compiler row of the kind `kind` at `lo..hi` (discovery numbering)
    /// in `file`.
    fn row_at(&self, file: &Path, lo: u32, hi: u32, kind: Kind) -> Option<&'a Row> {
        let inputs = self.inputs_for(file);
        let ir = inputs.files.get(file)?;
        inputs
            .positions
            .as_ref()?
            .lookup(file, ir.swc_start(lo), ir.swc_start(hi), kind)
            .ok()
    }

    /// The module binding `row` names, when its declaration is one a file
    /// of the run declares at module scope: the file as the inputs key it,
    /// the declaration's line and its name.
    pub(super) fn declared_binding(&self, row: &Row) -> Option<(&'a Path, u32, String)> {
        if !super::discovery::in_repo(row) {
            return None;
        }
        let (decl_file, line, symbol) = (row.decl_file()?, row.decl_line()?, row.symbol()?);
        let declaring = self.inputs.positions.as_ref()?.path_of(decl_file);
        let (path, ir) = self
            .inputs_for(&declaring)
            .files
            .get_key_value(&declaring)?;
        ir.module_bindings
            .contains_key(&(line, symbol.to_string()))
            .then(|| (path.as_path(), line, symbol.to_string()))
    }

    /// Every value the module binding `symbol`, declared on `line` of `file`,
    /// may hold when `invoke` runs it (carrick#3504 S1, soundness rule 1):
    /// its initialiser and each value an assignment writes, a write of the
    /// writing function's parameter read at that function's callers. A
    /// write the census cannot place, and a parameter no caller fills, add
    /// one value nothing reads, so no consumer agrees on the rest alone.
    /// `None` where `file` declares no such binding.
    pub(super) fn binding_values<F: Follow<'a>>(
        &self,
        follow: &mut F,
        file: &'a Path,
        line: u32,
        symbol: &str,
        invoke: Invoke<'a>,
    ) -> Option<Vec<Held<F::Reached>>> {
        let _query = self.enter();
        let ir = self.inputs_for(file).files.get(file)?;
        let binding = ir
            .module_bindings
            .get(&(line, symbol.to_string()))
            .filter(|binding| binding.values_read())?;
        let nothing = || Held::Reached(F::Reached::default());
        let reading = (file.to_path_buf(), line, symbol.to_string());
        if !self.bindings_reading.borrow_mut().insert(reading.clone()) {
            return Some(Vec::new());
        }
        let mut out = Vec::new();
        // A write no reader states: the binding may hold anything besides
        // the values below.
        if binding.unplaceable {
            out.push(nothing());
        }
        // A value written in the body of the function `key` of `file`: a
        // parameter of it is what its callers pass there.
        let mut written = |out: &mut Vec<Held<F::Reached>>,
                           value: &ValueAt,
                           key: Option<&'a str>| match (value, key) {
            (
                ValueAt::Param {
                    depth: 0,
                    index,
                    keys,
                },
                Some(key),
            ) => {
                let filled = self.callers_pass(
                    follow,
                    (file, Some(key), 0),
                    (0, *index, keys),
                    invoke,
                    &mut Vec::new(),
                );
                if filled.is_empty() {
                    out.push(nothing());
                }
                out.extend(filled);
            }
            (ValueAt::Param { .. }, _) => out.push(nothing()),
            (other, _) => out.extend(self.read_value(follow, file, other, invoke, false)),
        };
        for value in &binding.init {
            written(&mut out, value, binding.declared_in.as_deref());
        }
        for at in &binding.writes {
            let Some((key, write)) = placed_write(ir, *at) else {
                out.push(nothing());
                continue;
            };
            for value in &write.values {
                written(&mut out, value, key);
            }
        }
        self.bindings_reading.borrow_mut().remove(&reading);
        Some(self.settle(out))
    }
}

/// The parameter of `function` a write hands a binding whole, when `values`
/// (what the write's value may hold) is that one parameter: as the
/// function's body reads it, by its own name (`next` in `set(next) { app =
/// next; }`). `None` for a parameter that binds no single name (a
/// destructuring or a rest).
pub(super) fn written_param(function: &FnIr, values: &[ValueAt]) -> Option<Vec<Piece>> {
    let [
        ValueAt::Param {
            depth: 0,
            index,
            keys,
        },
    ] = values
    else {
        return None;
    };
    if !keys.is_empty() {
        return None;
    }
    let name = &function.params.get(*index)?.as_ref()?.key.0;
    Some(vec![Piece::Param(*index, name.clone())])
}

/// The plain assignment starting at `at`, written directly in a definition
/// of `ir` (with its key) or at module level (`None`). `None` for one
/// written inside a function written in a body: what runs that function
/// decides when it writes (an S1 gap; the binding holds nothing readable).
pub(super) fn placed_write(ir: &FileIr, at: u32) -> Option<(Option<&str>, &BindingWrite)> {
    fn find(body: &FnIr, at: u32) -> Option<&BindingWrite> {
        body.binding_writes.iter().find(|write| write.at == at)
    }
    let mut keys: Vec<&String> = ir.functions.keys().collect();
    keys.sort();
    keys.into_iter()
        .find_map(|key| find(&ir.functions[key], at).map(|write| (Some(key.as_str()), write)))
        .or_else(|| find(&ir.module_level, at).map(|write| (None, write)))
}

#[cfg(test)]
mod tests {
    //! The resolver's API on the forms discovery's value walk followed
    //! before the lift (carrick#3504 S0), read with a follower that names
    //! each end, so later steps have a harness. Fixtures:
    //! `discovery-default-function`, `discovery-factory-function` and
    //! `resolve-values` (its README is the answer key).

    use super::super::fixture_walk::walked;
    use super::*;

    /// What the test follower makes of an end.
    #[derive(Debug, Default, Clone, PartialEq, Eq, PartialOrd, Ord)]
    enum End {
        /// Nothing the resolver can read.
        #[default]
        Nothing,
        /// An in-repo definition, by its key.
        Definition(String),
        /// A function written in place, by its line.
        Written(u32),
    }

    /// Names each end; knows no labels.
    struct Ends;

    impl<'a> Follow<'a> for Ends {
        type Reached = End;

        fn export(&mut self, _: &'a Path, _: u32, _: u32, _: Kind, _: Invoke<'a>) -> Option<End> {
            None
        }

        fn definition(&mut self, _: PathBuf, key: String, _: Invoke<'a>) -> End {
            End::Definition(key)
        }

        fn written(
            &mut self,
            _: &'a Path,
            function: &'a FnIr,
            _: Invoke<'a>,
            _: Option<Maker<'a>>,
        ) -> End {
            End::Written(function.site.as_ref().map_or(0, |site| site.line))
        }
    }

    /// One value, as a test compares it.
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    enum Got {
        Param(u8, usize, Vec<String>),
        Callback,
        End(End),
    }

    fn got(held: Vec<Held<End>>) -> Vec<Got> {
        let mut out: Vec<Got> = held
            .into_iter()
            .map(|held| match held {
                Held::Param(depth, index, keys) => Got::Param(depth, index, keys),
                Held::Callback => Got::Callback,
                Held::Reached(end) => Got::End(end),
            })
            .collect();
        out.sort();
        out
    }

    /// The call at `line` of the file whose path ends with `file` whose
    /// callee is a value, with the definition it is written in and how many
    /// functions inside it.
    fn call_at<'a>(
        inputs: &'a RequestSummaryInputs,
        file: &str,
        line: u32,
    ) -> (&'a Path, Option<&'a str>, u8, &'a CallIr) {
        fn find(body: &FnIr, depth: u8, line: u32) -> Option<(u8, &CallIr)> {
            body.read_calls()
                .map(|(call, _)| call)
                .find(|call| call.site.line == line && !call.callee_values.is_empty())
                .map(|call| (depth, call))
                .or_else(|| {
                    body.nested
                        .iter()
                        .chain(&body.detached)
                        .find_map(|inner| find(inner, depth + 1, line))
                })
        }
        let (path, ir) = inputs
            .files
            .iter()
            .find(|(path, _)| path.ends_with(file))
            .unwrap_or_else(|| panic!("{file} is read"));
        ir.functions
            .iter()
            .find_map(|(key, body)| {
                find(body, 0, line).map(|(depth, call)| (Some(key.as_str()), depth, call))
            })
            .or_else(|| find(&ir.module_level, 0, line).map(|(depth, call)| (None, depth, call)))
            .map(|(key, depth, call)| (path.as_path(), key, depth, call))
            .unwrap_or_else(|| panic!("a call whose callee is a value at {file}:{line}"))
    }

    /// Every value the callee of the call at `file:line` holds: each callee
    /// value read, and each parameter among them read at every caller, as
    /// discovery's walk settles a call's open values.
    fn callee<'a>(
        resolver: &Resolver<'a>,
        inputs: &'a RequestSummaryInputs,
        file: &str,
        line: u32,
    ) -> Vec<Got> {
        let (path, key, depth, call) = call_at(inputs, file, line);
        let invoke = Invoke {
            file: path,
            call,
            construction: false,
        };
        let mut out = Vec::new();
        for value in &call.callee_values {
            for held in resolver.read_value(&mut Ends, path, value, invoke, false) {
                match held {
                    Held::Param(out_depth, index, keys) => out.extend(resolver.callers_pass(
                        &mut Ends,
                        (path, key, depth),
                        (out_depth, index, &keys),
                        invoke,
                        &mut Vec::new(),
                    )),
                    other => out.push(other),
                }
            }
        }
        got(out)
    }

    fn written(line: u32) -> Got {
        Got::End(End::Written(line))
    }

    fn definition(key: &str) -> Got {
        Got::End(End::Definition(key.to_string()))
    }

    /// A parameter holds what each caller passes there, or its default
    /// where a caller passes nothing (positional, destructured, a
    /// constructor parameter a field is set from). A function a caller
    /// passes from a local it wrote is a callback. A name whose row names
    /// no in-repo definition (the global `fetch`) is nothing this follower
    /// can read: discovery's follower reads it as a labelled export.
    #[test]
    fn a_parameter_holds_each_callers_argument_or_its_default() {
        let Some(on) = walked(
            "discovery-default-function",
            "services/web",
            "web",
            &[],
            true,
        ) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let read = |file: &str, line: u32| callee(&resolver, &on.inputs, file, line);
        assert_eq!(read("arrow.ts", 7), [written(5)], "positional default");
        assert_eq!(
            read("destructured.ts", 7),
            [written(5)],
            "destructured key's default"
        );
        assert_eq!(
            read("ctor.ts", 10),
            [written(6)],
            "field set from a constructor default"
        );
        assert_eq!(
            read("several.ts", 4),
            [Got::Callback, written(3)],
            "one caller passes nothing, the other its own function"
        );
        assert_eq!(
            read("control.ts", 4),
            [Got::End(End::Nothing)],
            "the global itself"
        );
        // Five queries above opened from outside the resolver; each reads at
        // least the callee value itself.
        let census = resolver.census();
        assert!(census.queries >= 5, "{census:?}");
        assert!(census.max_query_steps >= 1, "{census:?}");
        assert!(census.steps >= census.max_query_steps, "{census:?}");
    }

    /// What an in-repo factory returns is read through its call, and
    /// `f.bind(x)` is `f`: here the bound function is a factory parameter's
    /// key, so it is read at the factory's caller.
    #[test]
    fn a_factory_return_and_a_bound_function_are_read_through() {
        let Some(on) = walked(
            "discovery-factory-function",
            "services/web",
            "web",
            &[],
            true,
        ) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let read = |file: &str, line: u32| callee(&resolver, &on.inputs, file, line);
        assert_eq!(
            read("items.ts", 5),
            [written(9)],
            "the factory's returned function"
        );
        assert_eq!(
            read("plain.ts", 10),
            [written(2)],
            "a factory that sends nothing"
        );
        assert_eq!(
            read("bound.ts", 10),
            [definition("stub")],
            "`options.fetch.bind(options)`"
        );
    }

    /// A function that passes its own parameter to itself unchanged adds no
    /// value (the decision record's soundness rule 7, pass-through half). A
    /// spread at or before the position makes the caller's value unread
    /// (rule 5). A function nothing calls holds its default.
    #[test]
    fn a_pass_through_cycle_adds_nothing_and_a_spread_is_unread() {
        let Some(on) = walked("resolve-values", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let read = |file: &str, line: u32| callee(&resolver, &on.inputs, file, line);
        assert_eq!(read("cycle.ts", 12), [definition("sendItems")]);
        assert_eq!(
            read("spread.ts", 8),
            [Got::End(End::Nothing), definition("sendGo")]
        );
        assert_eq!(read("spread.ts", 20), [definition("sendGo")]);
    }

    /// The census of the module binding `s` in `source`: how many plain
    /// assignments it places, and whether another write makes it
    /// unplaceable.
    fn census_of(source: &str) -> (usize, bool) {
        let (census, key) = census_over(source, "s");
        let placed = census.writes.get(&key).map_or(0, Vec::len);
        (placed, census.is_unplaceable(&key))
    }

    /// The write census over `source`, with the key of the first binding
    /// named `name` (outermost declared first, as the parser resolves it).
    fn census_over(source: &str, name: &str) -> (WriteCensus, BindingKey) {
        let cm: swc_common::sync::Lrc<swc_common::SourceMap> = Default::default();
        let handler = swc_common::errors::Handler::with_tty_emitter(
            swc_common::errors::ColorConfig::Never,
            false,
            false,
            Some(cm.clone()),
        );
        let module = crate::parser::parse_source(
            Path::new("a.ts"),
            swc_common::FileName::Anon,
            source.to_string(),
            &cm,
            &handler,
            None,
        )
        .unwrap();
        let mut census = WriteCensus::default();
        module.visit_with(&mut census);
        // The binding the module's own (first) declaration of `name` makes
        // is the one with the lowest syntax context among those declared.
        let key = census
            .declared
            .keys()
            .filter(|(declared, _)| declared == name)
            .min_by_key(|(_, ctxt)| ctxt.as_u32())
            .cloned()
            .unwrap_or_else(|| panic!("`{name}` is declared"));
        (census, key)
    }

    /// Whether the module keeps the object `name` holds through its entries
    /// (the census's escape rule, carrick#3504 S2), and whether anything
    /// assigns the name again.
    fn kept_of(source: &str, name: &str) -> (bool, bool) {
        let (census, key) = census_over(source, name);
        let assigned = census.writes.contains_key(&key) || census.is_unplaceable(&key);
        (assigned, census.entries_kept(&key, false))
    }

    /// carrick#3504 S2, the name fold's escape census moved onto the write
    /// census (carrick#3479's cases): a `let` is read only where nothing
    /// assigns it again, in any form; an object's entries only where every
    /// use reads one. The census is keyed by binding: an inner scope that
    /// declares the name again blocks nothing.
    #[test]
    fn a_binding_keeps_its_entries_only_where_nothing_changes_them() {
        assert_eq!(kept_of("let P = 'a'; f(P);", "P"), (false, false));
        for source in [
            "let P = 'a'; P = 'b';",
            "let P = 'a'; P += 'b';",
            "let P = 'a'; P++;",
            "let P = 'a'; [P] = ['b'];",
            "let P = 'a'; ({ P } = { P: 'b' });",
            "let P = 'a'; for (P of ['b']) {}",
        ] {
            assert!(kept_of(source, "P").0, "{source}");
        }
        assert_eq!(
            kept_of("const R = { a: 'x' }; const u = R.a, v = R['a'];", "R"),
            (false, true)
        );
        // Nested writes and mutating calls change an entry too.
        for source in [
            "const R = { a: { b: 'x' } }; R.a.b = 'y';",
            "const R = { a: { b: 1 } }; R.a.b++;",
            "const R = { a: { b: 'x' } }; delete R.a.b;",
            "const R = { a: ['x'] }; R.a.push('y');",
            "const R = ['x']; R.splice(0, 1);",
            "const R = ['x']; R.reverse();",
            "const R = { a: 'x' }; R.a = 'y';",
            "const R = { a: 'x' }; delete R.a;",
            "const R = { a: 'x' }; f(R);",
            "const R = { a: 'x' }; export { R };",
            "const R = { a: 'x' }; g({ R });",
            "const R = { a: 'x' }; eval('1');",
        ] {
            assert!(!kept_of(source, "R").1, "{source}");
        }
        // An entry passed on is a string leaf to the text reader: still
        // read-only.
        assert!(kept_of("const R = { a: 'x' }; f(R.a); g(R['a']);", "R").1);
        // An inner `R` that is written or escapes is another binding.
        assert!(
            kept_of(
                "const R = { a: 'x' }; const u = R.a; \
                 function f() { const R = { a: 'y' }; R.a = 'z'; g(R); }",
                "R"
            )
            .1
        );
        // An exported declaration is never kept.
        let (census, key) = census_over("export const R = { a: 'x' };", "R");
        assert!(!census.entries_kept(&key, true));
    }

    /// carrick#3504 S1 (soundness rule 1): the census places each plain
    /// assignment of a binding, anywhere in the module, and marks every
    /// other form of write, so the binding is never read as its initialiser
    /// alone. A write of a property, or of a name an inner scope declares
    /// again, is no write of the binding.
    #[test]
    fn the_write_census_places_plain_writes_and_marks_every_other_write() {
        assert_eq!(census_of("let s = a;"), (0, false));
        assert_eq!(census_of("let s = a; s = b;"), (1, false));
        assert_eq!(census_of("let s = a; s ??= b; s ||= c;"), (2, false));
        assert_eq!(
            census_of("let s = a; function f() { s = b; } const g = () => { s = c; };"),
            (2, false)
        );
        assert_eq!(census_of("let s = a; s.x = b;"), (0, false));
        assert_eq!(census_of("let s = a; function f(s) { s = b; }"), (0, false));
        for source in [
            "let s = a; s &&= b;",
            "let s = a; s += b;",
            "let s = a; s++;",
            "let s = a; [s] = [b];",
            "let s = a; ({ s } = o);",
            "let s = a; for (s of list) {}",
            "let s = a; (s) = b;",
            "let s = a; s! = b;",
            "let s = a; (s as any) = b;",
            "var s = a; var s = b;",
            "let s = a; eval('s = b');",
        ] {
            assert!(census_of(source).1, "{source}");
        }
    }

    /// carrick#3504 S1b: a setter that writes its own parameter to a module
    /// binding (`set(next) { app = next; }`) hands the binding that
    /// parameter, named as the setter's body names it (`next`), not by the
    /// binding's name (`app`).
    #[test]
    fn a_setter_write_names_the_setter_parameter() {
        let Some(on) = walked("receiver-let-routes", ".", "web", &[], false) else {
            return;
        };
        let ir = &on.inputs.files[&on.path("src/s-let.ts")];
        let binding = &ir.module_bindings[&(2, "app".to_string())];
        let [start] = binding.writes.as_slice() else {
            panic!("one write: {:?}", binding.writes);
        };
        let (Some(key), write) = placed_write(ir, *start).expect("the write is placed") else {
            panic!("the write is in a function");
        };
        assert_eq!(
            written_param(&ir.functions[key], &write.values),
            Some(vec![Piece::Param(0, "next".to_string())])
        );
    }

    /// Every call written in `ir`, at any depth.
    fn calls_of(ir: &FileIr) -> Vec<&CallIr> {
        fn walk<'f>(body: &'f FnIr, out: &mut Vec<&'f CallIr>) {
            out.extend(body.read_calls().map(|(call, _)| call));
            for inner in body.nested.iter().chain(&body.detached) {
                walk(inner, out);
            }
        }
        let mut out = Vec::new();
        walk(&ir.module_level, &mut out);
        for body in ir.functions.values() {
            walk(body, &mut out);
        }
        out
    }

    /// carrick#3504 S3a: the receiver trace reads the value resolver's one
    /// lowering. A maker call and a maker's `new` carry the instance they
    /// build (`made`), as does a name the scope holds one in; a field of the
    /// class is a `Field` even where no constructor parameter sets it (and
    /// discovery still reads it as the member it is); a call of a member
    /// that builds nothing is `Returned` with its member; `await` is read
    /// through; and a local declared once reads as its initialiser.
    #[test]
    fn the_receiver_lowering_reads_what_held_read() {
        let source = "\
import { make, Make } from 'pkg';
declare function getApp(): any;
async function createApp() { return make(); }
const a = make();
const b = new Make();
class C { f = make(); m() { this.f.get('/f'); } n() { this.f(); } }
const r = getApp().basePath('/v2');
a.get('/a');
b.get('/b');
r.get('/r');
async function main() { (await createApp()).get('/w'); const l = a; l.get('/l'); }
function twice(x: boolean) { let w: any = 1; if (x) { w = 2; } w.get('/t'); }
";
        let path = PathBuf::from("src/lowering.ts");
        let inputs = RequestSummaryInputs {
            files: HashMap::from([(
                path.clone(),
                super::super::file_ir_of(&path, source).expect("the source parses"),
            )]),
            ..RequestSummaryInputs::default()
        };
        let ir = &inputs.files[&path];
        let calls = calls_of(ir);
        let receiver = |callee: &str| -> Vec<ValueAt> {
            calls
                .iter()
                .find(|call| call.callee_text == callee)
                .unwrap_or_else(|| panic!("a call of `{callee}`: {calls:#?}"))
                .instance
                .receiver
                .clone()
        };
        let made = |value: &ValueAt| match value {
            ValueAt::Read { holds, .. } => matches!(holds, Holds::Made(_)),
            ValueAt::Returned { made, .. } => made.is_some(),
            ValueAt::New { .. } => true,
            _ => false,
        };
        let init =
            |line: u32, name: &str| ir.module_bindings[&(line, name.to_string())].init.clone();

        // The maker call and the maker's `new` carry what they build.
        let [a_init] = init(4, "a").try_into().expect("one value");
        assert!(
            matches!(a_init, ValueAt::Returned { member: None, .. }) && made(&a_init),
            "{a_init:?}"
        );
        let [b_init] = init(5, "b").try_into().expect("one value");
        assert!(matches!(b_init, ValueAt::New { .. }), "{b_init:?}");
        // A name the module holds an instance in carries it.
        for callee in ["a.get", "b.get"] {
            let [value] = receiver(callee).try_into().expect("one value");
            assert!(
                matches!(value, ValueAt::Read { member: false, .. }) && made(&value),
                "{callee}: {value:?}"
            );
        }
        // A call of a member that builds nothing: the scope it may return.
        let [r_init] = init(7, "r").try_into().expect("one value");
        assert!(
            matches!(
                r_init,
                ValueAt::Returned {
                    member: Some(_),
                    ..
                }
            ) && !made(&r_init),
            "{r_init:?}"
        );
        let [r_value] = receiver("r.get").try_into().expect("one value");
        assert!(
            matches!(
                r_value,
                ValueAt::Read {
                    member: false,
                    holds: Holds::Declared,
                    ..
                }
            ),
            "{r_value:?}"
        );
        // A name a function declares and its scope does not hold names no
        // instance (a local written twice).
        let [twice] = receiver("w.get").try_into().expect("one value");
        assert!(
            matches!(
                twice,
                ValueAt::Read {
                    holds: Holds::Nothing,
                    ..
                }
            ),
            "{twice:?}"
        );
        // A field of the class no constructor parameter sets.
        assert!(
            ir.class_origins
                .values()
                .all(|origin| !origin.fields.contains_key("f")),
            "no origin entry for `f`"
        );
        assert!(
            matches!(receiver("this.f.get").as_slice(), [ValueAt::Field { field, .. }] if field == "f"),
            "{:?}",
            receiver("this.f.get")
        );
        // Discovery reads that field's call as the member it is.
        let field_call = calls
            .iter()
            .find(|call| call.callee_text == "this.f")
            .expect("a call of `this.f`");
        assert!(Resolver::new(&inputs, &[]).unbound_fields(&path, field_call));
        // `await` is read through to the own factory's call.
        let [awaited] = receiver("(await createApp()).get")
            .try_into()
            .expect("one value");
        assert!(
            matches!(awaited, ValueAt::Returned { member: None, .. }) && made(&awaited),
            "{awaited:?}"
        );
        // A local declared once reads as its initialiser: the module's `a`.
        let [local] = receiver("l.get").try_into().expect("one value");
        assert!(
            matches!(local, ValueAt::Read { member: false, .. }) && made(&local),
            "{local:?}"
        );
    }

    /// carrick#3504 S1: a query that reads more values than the step cap
    /// answers one value nothing reads, never the part it read, and the
    /// census counts it. The next query starts under the cap again.
    #[test]
    fn a_query_over_the_step_cap_answers_that_it_cannot_say() {
        let Some(on) = walked("resolve-values", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]).with_cap(1);
        assert_eq!(
            callee(&resolver, &on.inputs, "cycle.ts", 12),
            [Got::End(End::Nothing)]
        );
        assert_eq!(resolver.census().capped_queries, 1);
        assert_eq!(
            callee(&resolver, &on.inputs, "spread.ts", 20),
            [definition("sendGo")]
        );
        assert_eq!(resolver.census().capped_queries, 1);
        let uncapped = Resolver::new(&on.inputs, &[]);
        assert_eq!(
            callee(&uncapped, &on.inputs, "cycle.ts", 12),
            [definition("sendItems")]
        );
        assert_eq!(uncapped.census().capped_queries, 0);
    }
}
