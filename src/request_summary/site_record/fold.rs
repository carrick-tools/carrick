//! The one text a name's expression always is, where the code states every
//! part of it and the compiler names each declaration it reads
//! (carrick#3479).
//!
//! The compiler types many constants wider than their value: a `let` is
//! `string`, an enum member is its enum, an object literal's entry is
//! `string`, a built-in's result is `string`. Each still has one value the
//! language fixes:
//!
//! - a name the compiler types as one string literal is that literal;
//! - a `const`, or a `let` its module never assigns again (an importer
//!   cannot), is its initialiser, read at the declaration the compiler's row
//!   names;
//! - an enum member is its initialiser;
//! - an entry of a `const` object literal is its value, where the module
//!   only ever reads the object's entries (no write, no other use, no
//!   export), and a computed key is the text its expression folds to;
//! - `join`, `concat`, `replace`, `replaceAll` and `String.raw` over folded
//!   parts, each only where the compiler's row says the call is the default
//!   library's (standard built-ins are language, ruled 2026-10-24), and
//!   templates and `+` over folded parts.
//!
//! Anything else (a parameter, a call of the service's own, a value read at
//! runtime, a regular expression, a replacement that names a `$` pattern)
//! folds to nothing, and the site stays as it was.

use std::path::Path;

use swc_common::Span;
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use super::{Sources, in_repo, literals_of_type};
use crate::position_rows::{Kind, PositionIndex};

/// The most declarations one fold reads through.
const MAX_STEPS: usize = 32;

/// A declaration a name reads through.
enum Declared {
    /// A variable's initialiser, with whether the module may change what
    /// the variable's object holds.
    Value { init: Expr, object_kept: bool },
    /// An enum's members.
    Enum(Vec<TsEnumMember>),
}

/// Folds expressions written in the files of one service.
pub(super) struct Fold<'s, 'r> {
    sources: &'s mut Sources<'r>,
    positions: &'s PositionIndex,
    root: &'s Path,
    steps: usize,
}

impl<'s, 'r> Fold<'s, 'r> {
    pub(super) fn new(
        sources: &'s mut Sources<'r>,
        positions: &'s PositionIndex,
        root: &'s Path,
    ) -> Self {
        Fold {
            sources,
            positions,
            root,
            steps: 0,
        }
    }

    /// The one text `expr`, written in `file`, always is.
    pub(super) fn text(&mut self, file: &str, expr: &Expr) -> Option<String> {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            return None;
        }
        match super::super::unwrap_value(expr) {
            Expr::Lit(Lit::Str(lit)) => lit.value.as_str().map(str::to_string),
            Expr::Tpl(tpl) => self.template(file, tpl, |quasi| {
                quasi
                    .cooked
                    .as_ref()
                    .and_then(|cooked| cooked.as_str())
                    .map(str::to_string)
            }),
            Expr::Bin(bin) if bin.op == BinaryOp::Add => {
                let left = self.text(file, &bin.left)?;
                Some(left + &self.text(file, &bin.right)?)
            }
            Expr::Ident(ident) => self.ident(file, ident),
            Expr::Member(member) => self.member(file, member),
            Expr::Call(call) => self.built_in(file, call),
            Expr::TaggedTpl(tagged) => {
                // `String.raw`: the template's raw text.
                let Expr::Member(tag) = &*tagged.tag else {
                    return None;
                };
                let (Expr::Ident(object), MemberProp::Ident(prop)) = (&*tag.obj, &tag.prop) else {
                    return None;
                };
                if object.sym != "String"
                    || prop.sym != "raw"
                    || !self.default_library(file, tagged.span)
                {
                    return None;
                }
                self.template(file, &tagged.tpl, |quasi| Some(quasi.raw.to_string()))
            }
            _ => None,
        }
    }

    fn template(
        &mut self,
        file: &str,
        tpl: &Tpl,
        quasi_text: impl Fn(&TplElement) -> Option<String>,
    ) -> Option<String> {
        let mut text = String::new();
        for (index, quasi) in tpl.quasis.iter().enumerate() {
            text.push_str(&quasi_text(quasi)?);
            if let Some(expr) = tpl.exprs.get(index) {
                text.push_str(&self.text(file, expr)?);
            }
        }
        Some(text)
    }

    /// A name: the one literal the compiler types it as, else the value of
    /// the declaration its row names.
    fn ident(&mut self, file: &str, ident: &Ident) -> Option<String> {
        let row = self
            .positions
            .lookup(
                &self.root.join(file),
                ident.span.lo.0,
                ident.span.hi.0,
                Kind::Ident,
            )
            .ok()?;
        if let [one] = row
            .type_text()
            .map(literals_of_type)
            .unwrap_or_default()
            .as_slice()
        {
            return Some(one.clone());
        }
        let (decl_file, decl_line) = (row.decl_file()?.to_string(), row.decl_line()?);
        match self.declared(&decl_file, decl_line, ident.sym.as_ref())? {
            Declared::Value { init, .. } => self.text(&decl_file, &init),
            Declared::Enum(_) => None,
        }
    }

    /// An entry read off an enum or a `const` object literal.
    fn member(&mut self, file: &str, member: &MemberExpr) -> Option<String> {
        let key = match &member.prop {
            MemberProp::Ident(name) => name.sym.to_string(),
            MemberProp::Computed(computed) => self.text(file, &computed.expr)?,
            MemberProp::PrivateName(_) => return None,
        };
        // An `as const` object's entry the compiler already types as one
        // literal.
        if let Ok(row) = self.positions.lookup(
            &self.root.join(file),
            member.span.lo.0,
            member.span.hi.0,
            Kind::Member,
        ) && let [one] = row
            .type_text()
            .map(literals_of_type)
            .unwrap_or_default()
            .as_slice()
        {
            return Some(one.clone());
        }
        let Expr::Ident(object) = super::super::unwrap_value(&member.obj) else {
            return None;
        };
        let row = self
            .positions
            .lookup(
                &self.root.join(file),
                object.span.lo.0,
                object.span.hi.0,
                Kind::Ident,
            )
            .ok()?;
        let (decl_file, decl_line) = (row.decl_file()?.to_string(), row.decl_line()?);
        match self.declared(&decl_file, decl_line, object.sym.as_ref())? {
            Declared::Enum(members) => {
                let init = members.iter().find_map(|entry| {
                    let name = match &entry.id {
                        TsEnumMemberId::Ident(ident) => ident.sym.to_string(),
                        TsEnumMemberId::Str(text) => text.value.as_str()?.to_string(),
                    };
                    (name == key).then(|| entry.init.clone())?
                })?;
                self.text(&decl_file, &init)
            }
            Declared::Value { init, object_kept } => {
                let Expr::Object(object) = super::super::unwrap_value(&init) else {
                    return None;
                };
                if !object_kept {
                    return None;
                }
                // The last entry with the key wins, as in the language; a
                // spread may hold any key.
                let mut found = None;
                for prop in &object.props {
                    let PropOrSpread::Prop(prop) = prop else {
                        return None;
                    };
                    let Prop::KeyValue(entry) = &**prop else {
                        continue;
                    };
                    let name = match &entry.key {
                        PropName::Ident(name) => Some(name.sym.to_string()),
                        PropName::Str(text) => text.value.as_str().map(str::to_string),
                        PropName::Computed(computed) => {
                            Some(self.text(&decl_file, &computed.expr)?)
                        }
                        PropName::Num(_) | PropName::BigInt(_) => None,
                    };
                    if name.as_deref() == Some(key.as_str()) {
                        found = Some((*entry.value).clone());
                    }
                }
                self.text(&decl_file, &found?)
            }
        }
    }

    /// `join`, `concat`, `replace` and `replaceAll` of the default library,
    /// over folded parts.
    fn built_in(&mut self, file: &str, call: &CallExpr) -> Option<String> {
        let Callee::Expr(callee) = &call.callee else {
            return None;
        };
        let Expr::Member(member) = &**callee else {
            return None;
        };
        let MemberProp::Ident(method) = &member.prop else {
            return None;
        };
        if call.args.iter().any(|arg| arg.spread.is_some()) {
            return None;
        }
        let method = method.sym.as_ref();
        if !matches!(method, "join" | "concat" | "replace" | "replaceAll")
            || !self.default_library(file, call.span)
        {
            return None;
        }
        let mut args = Vec::with_capacity(call.args.len());
        for arg in &call.args {
            args.push(self.text(file, &arg.expr)?);
        }
        match method {
            "join" => {
                let Expr::Array(array) = super::super::unwrap_value(&member.obj) else {
                    return None;
                };
                let separator = match args.as_slice() {
                    [] => ",".to_string(),
                    [one] => one.clone(),
                    _ => return None,
                };
                let mut parts = Vec::with_capacity(array.elems.len());
                for element in &array.elems {
                    let element = element.as_ref()?;
                    if element.spread.is_some() {
                        return None;
                    }
                    parts.push(self.text(file, &element.expr)?);
                }
                Some(parts.join(&separator))
            }
            "concat" => Some(self.text(file, &member.obj)? + &args.concat()),
            _ => {
                let [pattern, replacement] = args.as_slice() else {
                    return None;
                };
                // A replacement's `$` patterns read the match: not stated.
                if replacement.contains('$') {
                    return None;
                }
                let text = self.text(file, &member.obj)?;
                Some(match method {
                    "replace" => text.replacen(pattern.as_str(), replacement, 1),
                    _ => text.replace(pattern.as_str(), replacement),
                })
            }
        }
    }

    /// Whether the compiler's row for the call at `span` names a
    /// declaration of the default library.
    fn default_library(&self, file: &str, span: Span) -> bool {
        self.positions
            .lookup(&self.root.join(file), span.lo.0, span.hi.0, Kind::Call)
            .ok()
            .and_then(|row| row.decl_file())
            .is_some_and(|decl| decl.starts_with("lib:"))
    }

    /// The declaration of `name` at `line` of the repo file `file`: a
    /// `const`, a `let` the module never assigns again, or an enum.
    fn declared(&mut self, file: &str, line: u32, name: &str) -> Option<Declared> {
        if !in_repo(file) {
            return None;
        }
        let parsed = self.sources.get(file)?;
        let mut find = FindDeclaration {
            name,
            line,
            at_line: &|pos| parsed.line(pos),
            exporting: false,
            found: None,
        };
        parsed.module.visit_with(&mut find);
        match find.found? {
            Found::Enum(members) => Some(Declared::Enum(members)),
            Found::Var {
                kind,
                init,
                exported,
            } => {
                let uses = Uses::of(&parsed.module, name);
                let kept = match kind {
                    VarDeclKind::Const => true,
                    VarDeclKind::Let => !uses.assigned,
                    VarDeclKind::Var => false,
                };
                kept.then_some(Declared::Value {
                    init,
                    object_kept: !exported && !uses.assigned && uses.only_read_entries,
                })
            }
        }
    }
}

enum Found {
    /// `exported`: an importer may write the object's entries.
    Var {
        kind: VarDeclKind,
        init: Expr,
        exported: bool,
    },
    Enum(Vec<TsEnumMember>),
}

/// The declaration of `name` on `line`.
struct FindDeclaration<'f> {
    name: &'f str,
    line: u32,
    at_line: &'f dyn Fn(u32) -> u32,
    /// Inside an `export` declaration.
    exporting: bool,
    found: Option<Found>,
}

impl FindDeclaration<'_> {
    fn on_line(&self, span: Span) -> bool {
        (self.at_line)(span.lo.0) == self.line
    }
}

impl Visit for FindDeclaration<'_> {
    fn visit_export_decl(&mut self, export: &ExportDecl) {
        self.exporting = true;
        export.visit_children_with(self);
        self.exporting = false;
    }

    fn visit_var_decl(&mut self, decl: &VarDecl) {
        for declarator in &decl.decls {
            if let Pat::Ident(binding) = &declarator.name
                && binding.id.sym == self.name
                && self.on_line(binding.id.span)
                && let Some(init) = &declarator.init
            {
                self.found = Some(Found::Var {
                    kind: decl.kind,
                    init: (**init).clone(),
                    exported: self.exporting,
                });
                return;
            }
        }
        decl.visit_children_with(self);
    }

    fn visit_ts_enum_decl(&mut self, decl: &TsEnumDecl) {
        if decl.id.sym == self.name && self.on_line(decl.id.span) {
            self.found = Some(Found::Enum(decl.members.clone()));
            return;
        }
        decl.visit_children_with(self);
    }
}

/// How a module uses one name, read in every scope (a name declared again
/// in an inner scope counts as the same: that only ever folds less).
struct Uses<'n> {
    name: &'n str,
    /// The name is the target of an assignment, an update or a loop head.
    assigned: bool,
    /// Every reference is the object of an entry read that nothing writes
    /// or deletes.
    only_read_entries: bool,
    /// Identifiers with the name, past its declaration.
    references: usize,
    /// Entry reads off it.
    entry_reads: usize,
}

impl<'n> Uses<'n> {
    fn of(module: &Module, name: &'n str) -> Self {
        let mut uses = Uses {
            name,
            assigned: false,
            only_read_entries: true,
            references: 0,
            entry_reads: 0,
        };
        module.visit_with(&mut uses);
        // The declaration's own binding is one identifier.
        uses.only_read_entries &= uses.references == uses.entry_reads + 1;
        uses
    }

    fn is_name(&self, expr: &Expr) -> bool {
        matches!(expr, Expr::Ident(ident) if ident.sym == self.name)
    }

    /// An entry read at any depth: a member chain whose root is the name
    /// (`R.a`, `R.a.b`, `R.list[0]`).
    fn entry_of_name(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Member(member) => self.is_name(&member.obj) || self.entry_of_name(&member.obj),
            Expr::Paren(paren) => self.entry_of_name(&paren.expr),
            Expr::TsNonNull(inner) => self.entry_of_name(&inner.expr),
            _ => false,
        }
    }

    /// A mutating call: an entry read that is the callee's object
    /// (`R.list.push(x)`, `R.reverse()`) may change the object. An entry
    /// passed as an argument is not counted: the fold reads only string
    /// entries, which no callee can change (`f(R.a)`, `http.post(R[K])`).
    fn callee_is_entry(&mut self, callee: &Expr) {
        if self.entry_of_name(callee) {
            self.only_read_entries = false;
        }
    }
}

/// The binding names a pattern assigns.
struct Bound<'n> {
    name: &'n str,
    found: bool,
}

impl Visit for Bound<'_> {
    fn visit_binding_ident(&mut self, binding: &BindingIdent) {
        self.found |= binding.id.sym == self.name;
    }
    fn visit_expr(&mut self, _: &Expr) {}
}

impl Visit for Uses<'_> {
    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        match &assign.left {
            AssignTarget::Simple(SimpleAssignTarget::Ident(binding)) => {
                self.assigned |= binding.id.sym == self.name;
            }
            AssignTarget::Simple(SimpleAssignTarget::Member(member)) => {
                if self.is_name(&member.obj) || self.entry_of_name(&member.obj) {
                    self.only_read_entries = false;
                }
            }
            AssignTarget::Pat(pat) => {
                let mut bound = Bound {
                    name: self.name,
                    found: false,
                };
                pat.visit_with(&mut bound);
                self.assigned |= bound.found;
            }
            _ => {}
        }
        assign.visit_children_with(self);
    }

    fn visit_update_expr(&mut self, update: &UpdateExpr) {
        if self.is_name(&update.arg) {
            self.assigned = true;
        }
        if self.entry_of_name(&update.arg) {
            self.only_read_entries = false;
        }
        update.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        let callee = match &call.callee {
            Callee::Expr(expr) => Some(&**expr),
            _ => None,
        };
        if let Some(callee) = callee {
            self.callee_is_entry(callee);
        }
        call.visit_children_with(self);
    }

    fn visit_unary_expr(&mut self, unary: &UnaryExpr) {
        if unary.op == UnaryOp::Delete && self.entry_of_name(&unary.arg) {
            self.only_read_entries = false;
        }
        unary.visit_children_with(self);
    }

    fn visit_for_head(&mut self, head: &ForHead) {
        if let ForHead::Pat(pat) = head {
            let mut bound = Bound {
                name: self.name,
                found: false,
            };
            pat.visit_with(&mut bound);
            self.assigned |= bound.found;
        }
        head.visit_children_with(self);
    }

    fn visit_member_expr(&mut self, member: &MemberExpr) {
        if self.is_name(&member.obj) {
            self.entry_reads += 1;
        }
        member.visit_children_with(self);
    }

    fn visit_ident(&mut self, ident: &Ident) {
        if ident.sym == self.name {
            self.references += 1;
        }
    }

    // A shorthand entry (`{ R }`) or an export specifier hands the binding
    // on: an identifier the visitor counts, never an entry read.
    fn visit_ts_type(&mut self, _: &TsType) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uses(source: &str, name: &str) -> (bool, bool) {
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
        let uses = Uses::of(&module, name);
        (uses.assigned, uses.only_read_entries)
    }

    /// carrick#3479: a `let` folds only where nothing assigns it again, in
    /// any form; an object's entries only where every use reads one.
    #[test]
    fn a_binding_keeps_its_value_only_where_nothing_changes_it() {
        assert_eq!(uses("let P = 'a'; f(P);", "P"), (false, false));
        assert!(uses("let P = 'a'; P = 'b';", "P").0);
        assert!(uses("let P = 'a'; P += 'b';", "P").0);
        assert!(uses("let P = 'a'; P++;", "P").0);
        assert!(uses("let P = 'a'; [P] = ['b'];", "P").0);
        assert!(uses("let P = 'a'; ({ P } = { P: 'b' });", "P").0);
        assert!(uses("let P = 'a'; for (P of ['b']) {}", "P").0);
        assert_eq!(
            uses("const R = { a: 'x' }; const u = R.a, v = R['a'];", "R"),
            (false, true)
        );
        // Nested writes and mutating calls change an entry too.
        for src in [
            "const R = { a: { b: 'x' } }; R.a.b = 'y';",
            "const R = { a: { b: 1 } }; R.a.b++;",
            "const R = { a: { b: 'x' } }; delete R.a.b;",
            "const R = { a: ['x'] }; R.a.push('y');",
            "const R = ['x']; R.splice(0, 1);",
            "const R = ['x']; R.reverse();",
        ] {
            assert!(!uses(src, "R").1, "{src}");
        }
        // An entry passed on is a string leaf to the fold: still read-only.
        assert!(uses("const R = { a: 'x' }; f(R.a); g(R['a']);", "R").1);
        assert!(!uses("const R = { a: 'x' }; R.a = 'y';", "R").1);
        assert!(!uses("const R = { a: 'x' }; delete R.a;", "R").1);
        assert!(!uses("const R = { a: 'x' }; f(R);", "R").1);
        assert!(!uses("const R = { a: 'x' }; export { R };", "R").1);
        assert!(!uses("const R = { a: 'x' }; g({ R });", "R").1);
    }
}
