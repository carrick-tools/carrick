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
    let cm: Lrc<SourceMap> = Default::default();
    let handler = swc_common::errors::Handler::with_tty_emitter(
        swc_common::errors::ColorConfig::Never,
        false,
        false,
        Some(cm.clone()),
    );
    let module = crate::parser::parse_file(&root.join(file), &cm, &handler)?;
    holder_in(&cm, &module, line)
}

/// [`router_holder`] over a module already parsed with `cm`: the same rule,
/// for a caller that holds the parse.
pub(crate) fn holder_in(cm: &SourceMap, module: &Module, line: u32) -> Option<String> {
    let mut find = Find {
        cm,
        line,
        enclosing: Vec::new(),
        found: None,
    };
    module.visit_with(&mut find);
    find.found
}

struct Find<'a> {
    cm: &'a SourceMap,
    line: u32,
    /// The named declarations around the node being visited, outermost first.
    enclosing: Vec<String>,
    found: Option<String>,
}

impl Find<'_> {
    fn within(&mut self, name: Option<String>, visit: impl FnOnce(&mut Self)) {
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
            Pat::Ident(binding) => Some(binding.id.sym.to_string()),
            _ => None,
        };
        self.within(name, |this| declarator.visit_children_with(this));
    }

    fn visit_fn_decl(&mut self, decl: &FnDecl) {
        if self.found.is_some() {
            return;
        }
        let name = Some(decl.ident.sym.to_string());
        self.within(name, |this| decl.visit_children_with(this));
    }

    fn visit_export_default_expr(&mut self, export: &ExportDefaultExpr) {
        if self.found.is_some() {
            return;
        }
        self.within(Some("default".to_string()), |this| {
            export.visit_children_with(this)
        });
    }

    fn visit_export_default_decl(&mut self, export: &ExportDefaultDecl) {
        if self.found.is_some() {
            return;
        }
        let name = match &export.decl {
            DefaultDecl::Fn(function) => Some(
                function
                    .ident
                    .as_ref()
                    .map_or_else(|| "default".to_string(), |ident| ident.sym.to_string()),
            ),
            _ => None,
        };
        self.within(name, |this| export.visit_children_with(this));
    }

    fn visit_prop(&mut self, prop: &Prop) {
        if self.found.is_some() {
            return;
        }
        if matches!(prop, Prop::KeyValue(_) | Prop::Shorthand(_))
            && self.cm.lookup_char_pos(prop.span().lo).line as u32 == self.line
        {
            self.found = Some(self.enclosing.last().cloned().unwrap_or_default());
            return;
        }
        prop.visit_children_with(self);
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

    #[test]
    fn no_named_declaration_is_empty_and_no_property_is_none() {
        assert_eq!(
            holder("t.router({\n  shelf: 1,\n});\n", 2).as_deref(),
            Some("")
        );
        assert_eq!(holder("export const a = 1;\n", 1), None);
    }
}
