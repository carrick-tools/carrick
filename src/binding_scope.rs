//! A binding read by its scope, never by its name alone (carrick#1648).
//!
//! The parser runs swc's resolver over every module, so every identifier
//! carries the syntax context of the scope that declares the binding it
//! names: a `const url` in a function and another in a block inside it are
//! two [`BindingKey`]s with one name. A pass that reads what a binding holds
//! keys it by [`BindingKey`], so a read in a block that declares the name
//! again never reaches the outer value.
//!
//! A table keyed by name alone (the env-alias and literal-base maps in
//! [`crate::env_alias`]) describes one binding only where the file declares
//! that name once. A pass that states a fact through such a table asks
//! [`Declarations::is_sole_binding`] first.

use std::collections::HashMap;

use swc_common::SyntaxContext;
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

/// A binding's resolver identity: its name and syntax context.
pub(crate) type BindingKey = (String, SyntaxContext);

/// The binding an identifier names.
pub(crate) fn ident_key(ident: &Ident) -> BindingKey {
    (ident.sym.to_string(), ident.ctxt)
}

/// The binding a parameter introduces, when it is a plain identifier.
/// Destructured and rest parameters bind no single name and hold a position no
/// argument can be read from.
pub(crate) fn pat_key(pat: &Pat) -> Option<BindingKey> {
    match pat {
        Pat::Ident(ident) => Some(ident_key(&ident.id)),
        // `path = "/default"`: the binding is the left side.
        Pat::Assign(assign) => pat_key(&assign.left),
        _ => None,
    }
}

/// Every declaration in a subtree: a variable, a parameter, a function or
/// class name, an import, an enum. Names in type positions bind nothing and
/// are not counted.
#[derive(Debug, Default)]
pub(crate) struct Declarations {
    /// How many times each name is declared, in any scope.
    names: HashMap<String, usize>,
    /// How many times each binding is declared. A `var` written twice in one
    /// function is one binding declared twice.
    bindings: HashMap<BindingKey, usize>,
}

impl Declarations {
    /// Every declaration under `node`.
    pub(crate) fn of<N: VisitWith<Self>>(node: &N) -> Self {
        let mut declarations = Self::default();
        node.visit_with(&mut declarations);
        declarations
    }

    fn declare(&mut self, ident: &Ident) {
        *self.names.entry(ident.sym.to_string()).or_default() += 1;
        *self.bindings.entry(ident_key(ident)).or_default() += 1;
    }

    /// How many times `name` is declared.
    pub(crate) fn name_count(&self, name: &str) -> usize {
        self.names.get(name).copied().unwrap_or(0)
    }

    /// Whether `name` is declared at all.
    pub(crate) fn declares_name(&self, name: &str) -> bool {
        self.names.contains_key(name)
    }

    /// How many times the binding `key` is declared.
    pub(crate) fn binding_count(&self, key: &BindingKey) -> usize {
        self.bindings.get(key).copied().unwrap_or(0)
    }

    /// Whether `ident` names the one declaration of its name in the subtree
    /// these were read from: what a table keyed by name says about that name
    /// is then about this binding. An identifier no declaration binds (a
    /// global) is not.
    pub(crate) fn is_sole_binding(&self, ident: &Ident) -> bool {
        self.name_count(ident.sym.as_ref()) == 1 && self.binding_count(&ident_key(ident)) == 1
    }
}

impl Visit for Declarations {
    fn visit_binding_ident(&mut self, ident: &BindingIdent) {
        self.declare(&ident.id);
    }

    fn visit_fn_decl(&mut self, decl: &FnDecl) {
        self.declare(&decl.ident);
        decl.visit_children_with(self);
    }

    fn visit_class_decl(&mut self, decl: &ClassDecl) {
        self.declare(&decl.ident);
        decl.visit_children_with(self);
    }

    // `const h = function api() { … }`: the name is bound inside the
    // function.
    fn visit_fn_expr(&mut self, expr: &FnExpr) {
        if let Some(ident) = &expr.ident {
            self.declare(ident);
        }
        expr.visit_children_with(self);
    }

    fn visit_class_expr(&mut self, expr: &ClassExpr) {
        if let Some(ident) = &expr.ident {
            self.declare(ident);
        }
        expr.visit_children_with(self);
    }

    fn visit_import_default_specifier(&mut self, specifier: &ImportDefaultSpecifier) {
        self.declare(&specifier.local);
    }

    fn visit_import_named_specifier(&mut self, specifier: &ImportNamedSpecifier) {
        self.declare(&specifier.local);
    }

    fn visit_import_star_as_specifier(&mut self, specifier: &ImportStarAsSpecifier) {
        self.declare(&specifier.local);
    }

    fn visit_ts_import_equals_decl(&mut self, decl: &TsImportEqualsDecl) {
        self.declare(&decl.id);
    }

    fn visit_ts_enum_decl(&mut self, decl: &TsEnumDecl) {
        self.declare(&decl.id);
        decl.visit_children_with(self);
    }

    // A parameter named in a type (`(api: Api) => void`) binds nothing.
    fn visit_ts_type(&mut self, _: &TsType) {}

    fn visit_ts_interface_body(&mut self, _: &TsInterfaceBody) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_file;
    use swc_common::{
        SourceMap,
        errors::{ColorConfig, Handler},
        sync::Lrc,
    };

    /// The module `source` parses to, with the resolver run.
    fn module(source: &str) -> Module {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("input.ts");
        std::fs::write(&path, source).expect("write file");
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, true, false, Some(cm.clone()));
        parse_file(&path, &cm, &handler).expect("parsed module")
    }

    /// Every identifier named `name` the module reads as a value, in source
    /// order.
    fn uses_of(module: &Module, name: &str) -> Vec<Ident> {
        struct Uses<'a> {
            name: &'a str,
            found: Vec<Ident>,
        }
        impl Visit for Uses<'_> {
            fn visit_expr(&mut self, expr: &Expr) {
                if let Expr::Ident(ident) = expr
                    && ident.sym == *self.name
                {
                    self.found.push(ident.clone());
                }
                expr.visit_children_with(self);
            }
        }
        let mut uses = Uses {
            name,
            found: Vec::new(),
        };
        module.visit_with(&mut uses);
        uses.found
    }

    #[test]
    fn a_name_declared_once_is_the_binding_every_use_names() {
        let module = module(
            "import { BASE } from \"./config\";\n\
             type Handler = (BASE: string) => void;\n\
             export const read = () => fetch(`${BASE}/users`);\n",
        );
        let declarations = Declarations::of(&module);
        let [use_site] = uses_of(&module, "BASE").try_into().expect("one use");
        assert!(
            declarations.is_sole_binding(&use_site),
            "an import is a declaration and a parameter in a type binds nothing"
        );
    }

    #[test]
    fn a_name_declared_again_in_a_block_has_no_sole_binding() {
        let module = module(
            "const USERS = \"/api/users\";\n\
             export function load(admin: boolean) {\n\
             \x20 if (admin) {\n\
             \x20   const USERS = \"/api/admins\";\n\
             \x20   return fetch(USERS);\n\
             \x20 }\n\
             \x20 return fetch(USERS);\n\
             }\n",
        );
        let declarations = Declarations::of(&module);
        let uses = uses_of(&module, "USERS");
        assert_eq!(uses.len(), 2);
        assert_ne!(
            ident_key(&uses[0]),
            ident_key(&uses[1]),
            "the resolver gives the block's binding a context of its own"
        );
        assert!(
            uses.iter()
                .all(|use_site| !declarations.is_sole_binding(use_site))
        );
        assert_eq!(declarations.name_count("USERS"), 2);
    }

    #[test]
    fn a_global_is_not_a_sole_binding() {
        let module = module(
            "export function a() {\n\
             \x20 const API = process.env.API_URL;\n\
             \x20 return API;\n\
             }\n\
             export function b() {\n\
             \x20 return fetch(API);\n\
             }\n",
        );
        let declarations = Declarations::of(&module);
        let uses = uses_of(&module, "API");
        assert_eq!(uses.len(), 2);
        assert!(declarations.is_sole_binding(&uses[0]));
        assert!(
            !declarations.is_sole_binding(&uses[1]),
            "`API` in `b` binds nothing in this file; the declaration in `a` is not it"
        );
    }
}
