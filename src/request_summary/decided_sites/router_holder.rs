//! The router a procedure is declared in, named the same way by every side
//! that keys a procedure (`ProcedureKey.router.binding`).
//!
//! The rule has one implementation, [`router_holder`]. The server rows call
//! it on the property of a procedure's first key; the client reader calls it
//! on the declaration its proxy member resolves to. Both then hold the same
//! binding for the same router, so a call and a served procedure pair by
//! exact key. A second reader of this question would let the two sides
//! disagree.
//!
//! The holder is the innermost named declaration that encloses the
//! object-literal property: a variable declarator's identifier, a function
//! declaration's name, `default` for an `export default` expression or an
//! anonymous default function, and `""` when none is named.
//!
//! A procedure named by the variable that holds it (contract amendment 13,
//! `name: { bound: "binding" }`) is keyed by the same rule, applied twice
//! ([`holder_of_call`]): the innermost named declaration around the op's
//! call is the procedure's key path, and the one around that declaration is
//! its router binding (`""` at the module's top level).

use std::path::Path;

use swc_common::{SourceMap, Spanned, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

/// The binding that holds the object-literal property (key-value or
/// shorthand) starting on `line` (1-based) of `file` (repo-relative,
/// `/`-separated, under `root`): `Some("")` when no declaration around it is
/// named, `None` when the file does not parse or no such property starts on
/// that line.
pub fn router_holder(root: &Path, file: &str, line: u32) -> Option<String> {
    let (cm, module) = parse(root, file)?;
    holder_in(&cm, &module, line)
}

/// The file `file` under `root`, parsed on its own: SWC's spans in it are
/// the ones the decided sites key a call by.
fn parse(root: &Path, file: &str) -> Option<(Lrc<SourceMap>, Module)> {
    let cm: Lrc<SourceMap> = Default::default();
    let handler = swc_common::errors::Handler::with_tty_emitter(
        swc_common::errors::ColorConfig::Never,
        false,
        false,
        Some(cm.clone()),
    );
    let module = crate::parser::parse_file(&root.join(file), &cm, &handler)?;
    Some((cm, module))
}

/// [`router_holder`] over a module already parsed with `cm`: the same rule,
/// for a caller that holds the parse.
pub(crate) fn holder_in(cm: &SourceMap, module: &Module, line: u32) -> Option<String> {
    let mut find = Find {
        cm,
        target: Target::Property(line),
        enclosing: Vec::new(),
        found: None,
    };
    module.visit_with(&mut find);
    find.found.map(|enclosing| {
        enclosing
            .last()
            .map(|(name, _)| name.clone())
            .unwrap_or_default()
    })
}

/// The key of a procedure named by its binding ([`holder_of_call`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldCall {
    /// The innermost named declaration around the op's call: the key path.
    pub key: String,
    /// The innermost named declaration around that one: the router binding
    /// (`""` where none is).
    pub binding: String,
    /// The line (1-based) the key's declared name is written on: where the
    /// compiler places the declaration a call of the procedure's variable
    /// resolves to.
    pub line: u32,
}

/// The key a procedure named by its binding is served under (contract
/// amendment 13): the innermost named declaration around the call whose
/// SWC span is `span` in `file` parsed on its own, and the innermost named
/// declaration around that one (`""` where none is). `None` when the file
/// does not parse, no call has that span, or no named declaration holds it:
/// the procedure has no name the code writes.
pub fn holder_of_call(root: &Path, file: &str, span: (u32, u32)) -> Option<HeldCall> {
    let (cm, module) = parse(root, file)?;
    let mut find = Find {
        cm: &cm,
        target: Target::Call(span),
        enclosing: Vec::new(),
        found: None,
    };
    module.visit_with(&mut find);
    let mut enclosing = find.found?;
    let (key, line) = enclosing.pop()?;
    Some(HeldCall {
        key,
        binding: enclosing.pop().map(|(name, _)| name).unwrap_or_default(),
        line,
    })
}

/// What [`Find`] looks for.
enum Target {
    /// An object-literal property starting on this line (1-based).
    Property(u32),
    /// The call with this SWC span.
    Call((u32, u32)),
}

struct Find<'a> {
    cm: &'a SourceMap,
    target: Target,
    /// The named declarations around the node being visited, outermost
    /// first, each with the line its name is written on.
    enclosing: Vec<(String, u32)>,
    /// The named declarations around the target, outermost first.
    found: Option<Vec<(String, u32)>>,
}

impl Find<'_> {
    fn line(&self, at: swc_common::BytePos) -> u32 {
        self.cm.lookup_char_pos(at).line as u32
    }

    fn within(&mut self, name: Option<(String, u32)>, visit: impl FnOnce(&mut Self)) {
        match name {
            Some(name) => {
                self.enclosing.push(name);
                visit(self);
                self.enclosing.pop();
            }
            None => visit(self),
        }
    }
}

impl Visit for Find<'_> {
    fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
        if self.found.is_some() {
            return;
        }
        let name = match &declarator.name {
            Pat::Ident(binding) => {
                Some((binding.id.sym.to_string(), self.line(binding.id.span.lo)))
            }
            _ => None,
        };
        self.within(name, |this| declarator.visit_children_with(this));
    }

    fn visit_fn_decl(&mut self, decl: &FnDecl) {
        if self.found.is_some() {
            return;
        }
        let name = Some((decl.ident.sym.to_string(), self.line(decl.ident.span.lo)));
        self.within(name, |this| decl.visit_children_with(this));
    }

    fn visit_export_default_expr(&mut self, export: &ExportDefaultExpr) {
        if self.found.is_some() {
            return;
        }
        let line = self.line(export.span.lo);
        self.within(Some(("default".to_string(), line)), |this| {
            export.visit_children_with(this)
        });
    }

    fn visit_export_default_decl(&mut self, export: &ExportDefaultDecl) {
        if self.found.is_some() {
            return;
        }
        let line = self.line(export.span.lo);
        let name = match &export.decl {
            DefaultDecl::Fn(function) => Some(function.ident.as_ref().map_or_else(
                || ("default".to_string(), line),
                |ident| (ident.sym.to_string(), self.line(ident.span.lo)),
            )),
            _ => None,
        };
        self.within(name, |this| export.visit_children_with(this));
    }

    fn visit_prop(&mut self, prop: &Prop) {
        if self.found.is_some() {
            return;
        }
        if let Target::Property(line) = self.target
            && matches!(prop, Prop::KeyValue(_) | Prop::Shorthand(_))
            && self.cm.lookup_char_pos(prop.span().lo).line as u32 == line
        {
            self.found = Some(self.enclosing.clone());
            return;
        }
        prop.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        if self.found.is_some() {
            return;
        }
        if let Target::Call(span) = self.target
            && (call.span.lo.0, call.span.hi.0) == span
        {
            self.found = Some(self.enclosing.clone());
            return;
        }
        call.visit_children_with(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder(source: &str, line: u32) -> Option<String> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("r.ts"), source).unwrap();
        router_holder(dir.path(), "r.ts", line)
    }

    #[test]
    fn a_router_is_held_by_the_innermost_named_declaration() {
        // The binding the router call is assigned to.
        assert_eq!(
            holder("export const a = t.router({\n  shelf: 1,\n});\n", 2).as_deref(),
            Some("a")
        );
        // A table another binding hands to the router: the table holds it.
        assert_eq!(
            holder(
                "const routes = {\n  shelf: 1,\n};\nexport const b = t.router(routes);\n",
                2
            )
            .as_deref(),
            Some("routes")
        );
        // Built inside a function declaration: the function holds it.
        assert_eq!(
            holder("function make() {\n  return t.router({\n    shelf: 1,\n  });\n}\nexport const c = make();\n", 3)
                .as_deref(),
            Some("make")
        );
        // An `export default` expression.
        assert_eq!(
            holder("export default t.router({\n  shelf: 1,\n});\n", 2).as_deref(),
            Some("default")
        );
        // A shorthand key.
        assert_eq!(
            holder(
                "const shelf = 1;\nexport const d = t.router({\n  shelf,\n});\n",
                3
            )
            .as_deref(),
            Some("d")
        );
    }

    #[test]
    fn two_routers_in_one_file_sharing_a_first_key_hold_distinct_bindings() {
        let source = "export const first = t.router({\n  shelf: 1,\n});\nexport const second = t.router({\n  shelf: 2,\n});\n";
        assert_eq!(holder(source, 2).as_deref(), Some("first"));
        assert_eq!(holder(source, 5).as_deref(), Some("second"));
    }

    /// The call's key: `(key path, router binding)` for the call written as
    /// the text `call` in `source`.
    fn call_key(source: &str, call: &str) -> Option<(String, String)> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("r.ts"), source).unwrap();
        // A file parsed on its own starts at SWC's position 1.
        let lo = source.find(call).unwrap() as u32 + 1;
        let hi = lo + call.len() as u32;
        holder_of_call(dir.path(), "r.ts", (lo, hi)).map(|held| (held.key, held.binding))
    }

    /// The line a key's declared name is written on, where a call of the
    /// procedure's variable resolves to: not the line its call starts on.
    #[test]
    fn a_call_key_names_the_line_its_declaration_is_written_on() {
        let call = "make().handler(() => 1)";
        let source = format!("export const\n  later =\n  {call};\n");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("r.ts"), &source).unwrap();
        let lo = source.find(call).unwrap() as u32 + 1;
        let held = holder_of_call(dir.path(), "r.ts", (lo, lo + call.len() as u32)).unwrap();
        assert_eq!((held.key.as_str(), held.line), ("later", 2));
    }

    /// Amendment 13: a procedure is keyed by the declaration that holds its
    /// call, and its router by the declaration around that one.
    #[test]
    fn a_call_is_keyed_by_the_declaration_that_holds_it() {
        let call = "make().handler(() => 1)";
        // At the module's top level: no router binding.
        assert_eq!(
            call_key(&format!("export const listNotes = {call};\n"), call),
            Some(("listNotes".into(), String::new()))
        );
        // Inside a function declaration: that function is the router.
        assert_eq!(
            call_key(
                &format!("function group() {{\n  const inner = {call};\n  return inner;\n}}\n"),
                call
            ),
            Some(("inner".into(), "group".into()))
        );
        // Handed to another call: still the declaration that holds it.
        assert_eq!(
            call_key(&format!("export const wrapped = wrap({call});\n"), call),
            Some(("wrapped".into(), String::new()))
        );
        // No named declaration holds it, and no call has the span.
        assert_eq!(call_key(&format!("{call};\n"), call), None);
        assert_eq!(
            call_key(&format!("export const a = {call};\n"), "handler"),
            None
        );
    }

    #[test]
    fn no_named_declaration_is_empty_and_no_property_is_none() {
        assert_eq!(
            holder("t.router({\n  shelf: 1,\n});\n", 2).as_deref(),
            Some("")
        );
        assert_eq!(holder("export const a = 1;\n", 1), None);
    }
}
