//! Which import a local binding's value came out of (carrick#666).
//!
//! A service client is often not imported. It is asked for:
//!
//! ```ignore
//! import { apiClientManager } from "@scope/core/v3";
//!
//! function retrieveThing(id: string) {
//!   const apiClient = apiClientManager.clientOrThrow();
//!   return apiClient.retrieveThing(id);
//! }
//! ```
//!
//! `apiClient` is a local, so nothing about the call site says where the
//! object came from — and the member it calls is declared in another package
//! entirely. The one thing the file DOES state is that the value came out of
//! `apiClientManager`, and `apiClientManager` is an imported binding with a
//! specifier behind it. That is what this pass records: local name -> the
//! specifier its value traces back to.
//!
//! It exists to CONSTRAIN a join, never to widen one. `list`, `get` and
//! `create` are what every client calls its methods, so a member name matched
//! across a package boundary needs a reason to believe the receiver belongs to
//! that package, and this is the reason. See
//! `FileOrchestrator::resolve_package_surface_members`.
//!
//! What is read, and nothing else:
//!
//! - Every imported binding names its own specifier. A receiver that IS the
//!   import is the trivial case of the same fact.
//! - A `const`/`let`/`var` bound to an expression whose ROOT identifier is an
//!   imported binding takes that binding's specifier. The root is reached
//!   through calls, `await`, member access, `new`, parentheses and TypeScript
//!   assertions — the forms that pass a value along without replacing where it
//!   came from. An initialiser rooted at anything else states no origin.
//! - Declarators are read wherever they sit, not just at the top level: the
//!   shape this pass exists for is a `const` inside the function that uses it.
//! - A name bound twice to different origins is ambiguous and is dropped
//!   rather than picked between, and so is a name that is bound once from an
//!   import and once from anything else. A shadowed import is not the import.
//! - Destructuring binds no origin. `const { client } = await getClient()`
//!   states that SOME field of the result is the value, and which field it is
//!   decides what the value is; the two-ring member join already reaches that
//!   shape by module (carrick#655) and does not need this one.
//!
//! Two scopes are read, for two callers. [`collect_receiver_origins`] folds a
//! whole module into one table, which is what the HTTP-candidate join has
//! always consumed (carrick#666). [`ReceiverOriginCollector`] answers the same
//! question per FUNCTION for the call graph (carrick#781): seeded with the
//! module scope every body can see — its value imports and its top-level
//! declarators — then walked over one body. `client` is what half the
//! functions in a file call their local, so a module-wide fold drops the name
//! in exactly the files that use it most. On that per-function path a
//! PARAMETER of the name shadows the import and is recorded as no origin at
//! all, wherever the parameter sits: a nested function's call sites are folded
//! into the enclosing function's, so a parameter the enclosing scope cannot
//! see would otherwise have the join answer for a receiver that is something
//! else. The module-wide fold keeps the reading it has always had — parameters
//! do not shadow there — because its rows are the HTTP-candidate join's and
//! moving them is not this ticket's business.

use std::collections::HashMap;

use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

/// Local binding name -> the module specifier its value came from.
pub type ReceiverOrigins = HashMap<String, String>;

/// Read a whole module's receiver origins as one table (carrick#666).
pub fn collect_receiver_origins(module: &Module) -> ReceiverOrigins {
    let mut collector = ReceiverOriginCollector::default();
    collector.record_module_scope(module);
    module.visit_with(&mut collector);
    collector.finish()
}

/// The identifier an expression's value traces back to, following the forms
/// that pass a value along rather than replacing it.
fn origin_root(expr: &Expr) -> Option<&Ident> {
    match expr {
        Expr::Ident(ident) => Some(ident),
        Expr::Await(await_expr) => origin_root(&await_expr.arg),
        Expr::Paren(paren) => origin_root(&paren.expr),
        Expr::TsAs(as_expr) => origin_root(&as_expr.expr),
        Expr::TsNonNull(non_null) => origin_root(&non_null.expr),
        Expr::TsSatisfies(satisfies) => origin_root(&satisfies.expr),
        Expr::Member(member) => origin_root(&member.obj),
        Expr::New(new_expr) => origin_root(&new_expr.callee),
        Expr::Call(call) => match &call.callee {
            Callee::Expr(callee) => origin_root(callee),
            _ => None,
        },
        _ => None,
    }
}

/// Reads one scope's receiver origins. Seed it with the module scope through
/// [`record_module_scope`](Self::record_module_scope), clone it per function,
/// walk that function's body with `visit_with`, then take the table with
/// [`finish`](Self::finish).
///
/// `None` against a name marks its origin contested: bound from two different
/// specifiers, or bound from something that is not an import at all.
#[derive(Clone, Default)]
pub struct ReceiverOriginCollector {
    origins: HashMap<String, Option<String>>,
    /// Whether a parameter encountered while walking shadows what the scope
    /// binds. On for the per-function reading (carrick#781), where a nested
    /// function's parameter has to be seen because its call sites are folded
    /// into the enclosing function's. Off for the module-wide fold, which has
    /// answered for a file as a whole since carrick#666 and whose rows a
    /// stricter rule would silently move.
    shadow_params: bool,
}

impl ReceiverOriginCollector {
    /// A collector for ONE function: parameters shadow, wherever they sit.
    pub fn per_function() -> Self {
        Self {
            origins: HashMap::new(),
            shadow_params: true,
        }
    }

    /// Everything a function body can see before its own statements: the
    /// module's value imports, then its top-level declarators. Nested bodies
    /// are deliberately not walked — that is the caller's one body, recorded
    /// after this seed.
    pub fn record_module_scope(&mut self, module: &Module) {
        for item in &module.body {
            match item {
                ModuleItem::ModuleDecl(ModuleDecl::Import(import)) => self.record_import(import),
                ModuleItem::Stmt(Stmt::Decl(Decl::Var(var))) => self.record_var_decl(var),
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => {
                    if let Decl::Var(var) = &export.decl {
                        self.record_var_decl(var);
                    }
                }
                _ => {}
            }
        }
    }

    /// Record a binding this scope introduces that is NOT an import — a
    /// parameter, most of all. A shadowed import is not the import.
    pub fn record_shadow(&mut self, pat: &Pat) {
        if let Pat::Ident(ident) = pat {
            self.record(ident.id.sym.to_string(), None);
        }
    }

    /// The names this scope traces back to a specifier.
    pub fn finish(self) -> ReceiverOrigins {
        self.origins
            .into_iter()
            .filter_map(|(name, origin)| origin.map(|specifier| (name, specifier)))
            .collect()
    }

    fn record_import(&mut self, import: &ImportDecl) {
        if import.type_only {
            return;
        }
        for spec in &import.specifiers {
            let local = match spec {
                ImportSpecifier::Named(named) => {
                    if named.is_type_only {
                        continue;
                    }
                    named.local.sym.to_string()
                }
                ImportSpecifier::Default(default) => default.local.sym.to_string(),
                ImportSpecifier::Namespace(ns) => ns.local.sym.to_string(),
            };
            self.record(local, Some(import.src.value.to_string()));
        }
    }

    fn record_var_decl(&mut self, var: &VarDecl) {
        for declarator in &var.decls {
            self.record_declarator(declarator);
        }
    }

    fn record_declarator(&mut self, declarator: &VarDeclarator) {
        if let Pat::Ident(ident) = &declarator.name {
            let specifier = declarator
                .init
                .as_deref()
                .and_then(origin_root)
                .map(|root| root.sym.to_string())
                .and_then(|root| self.origins.get(&root).cloned().flatten());
            self.record(ident.id.sym.to_string(), specifier);
        }
    }

    fn record(&mut self, name: String, specifier: Option<String>) {
        match self.origins.get(&name) {
            Some(existing) if existing.as_deref() == specifier.as_deref() => {}
            Some(_) => {
                self.origins.insert(name, None);
            }
            None => {
                self.origins.insert(name, specifier);
            }
        }
    }
}

impl Visit for ReceiverOriginCollector {
    fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
        self.record_declarator(declarator);
        declarator.visit_children_with(self);
    }

    /// A parameter is a binding that is not the import, wherever it sits. Call
    /// sites inside a nested function are folded into the enclosing one, so a
    /// parameter that shadows an origin has to be seen from out here or the
    /// join answers for a receiver that is something else entirely. Only on
    /// the per-function path — see `shadow_params`.
    fn visit_param(&mut self, param: &Param) {
        if self.shadow_params {
            self.record_shadow(&param.pat);
        }
        param.visit_children_with(self);
    }

    /// Arrow parameters are `Pat`s with no `Param` wrapper around them.
    fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
        if self.shadow_params {
            for pat in &arrow.params {
                self.record_shadow(pat);
            }
        }
        arrow.visit_children_with(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swc_scanner::parse_standalone_module;
    use std::path::Path;

    fn origins(source: &str) -> ReceiverOrigins {
        let (_, module) = parse_standalone_module(Path::new("origins.ts"), source).expect("parses");
        collect_receiver_origins(&module)
    }

    #[test]
    fn a_local_from_a_call_on_an_import_takes_the_imports_specifier() {
        let origins = origins(
            r#"
            import { manager } from "@scope/core/v3";
            function run(id: string) {
              const client = manager.clientOrThrow();
              return client.retrieveThing(id);
            }
            "#,
        );
        assert_eq!(
            origins.get("client").map(String::as_str),
            Some("@scope/core/v3")
        );
        assert_eq!(
            origins.get("manager").map(String::as_str),
            Some("@scope/core/v3")
        );
    }

    #[test]
    fn an_awaited_call_and_a_member_chain_both_carry_the_origin() {
        let origins = origins(
            r#"
            import factory from "@scope/core";
            async function run() {
              const client = await factory.build().http;
              return client;
            }
            "#,
        );
        assert_eq!(
            origins.get("client").map(String::as_str),
            Some("@scope/core")
        );
    }

    #[test]
    fn a_local_from_a_constructor_on_an_import_takes_the_specifier() {
        let origins = origins(
            r#"
            import { ApiClient } from "@scope/core";
            const client = new ApiClient("http://x");
            "#,
        );
        assert_eq!(
            origins.get("client").map(String::as_str),
            Some("@scope/core")
        );
    }

    #[test]
    fn a_local_from_a_non_import_root_has_no_origin() {
        let origins = origins(
            r#"
            import { manager } from "@scope/core";
            function build() { return manager; }
            const client = build();
            "#,
        );
        assert!(!origins.contains_key("client"));
    }

    #[test]
    fn a_name_bound_from_two_different_specifiers_is_dropped() {
        let origins = origins(
            r#"
            import { a } from "@scope/one";
            import { b } from "@scope/two";
            function first() { const client = a.make(); return client; }
            function second() { const client = b.make(); return client; }
            "#,
        );
        assert!(!origins.contains_key("client"));
    }

    #[test]
    fn an_import_shadowed_by_a_local_is_dropped() {
        let origins = origins(
            r#"
            import { client } from "@scope/core";
            function run() { const client = makeSomethingElse(); return client; }
            "#,
        );
        assert!(!origins.contains_key("client"));
    }

    #[test]
    fn a_destructured_binding_carries_no_origin() {
        let origins = origins(
            r#"
            import { getProjectClient } from "@scope/core";
            async function run() {
              const { client } = await getProjectClient("ref");
              return client;
            }
            "#,
        );
        assert!(!origins.contains_key("client"));
    }

    /// The per-function reading (carrick#781): the module scope seeds the
    /// table, then one function's own body is recorded on top of it.
    fn scope_origins(source: &str, function: &str) -> ReceiverOrigins {
        let (_, module) = parse_standalone_module(Path::new("origins.ts"), source).expect("parses");
        let mut collector = ReceiverOriginCollector::default();
        collector.record_module_scope(&module);
        let declaration = module
            .body
            .iter()
            .find_map(|item| match item {
                ModuleItem::Stmt(Stmt::Decl(Decl::Fn(decl))) if decl.ident.sym == *function => {
                    Some(decl)
                }
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => match &export.decl {
                    Decl::Fn(decl) if decl.ident.sym == *function => Some(decl),
                    _ => None,
                },
                _ => None,
            })
            .expect("the function");
        for param in &declaration.function.params {
            collector.record_shadow(&param.pat);
        }
        if let Some(body) = &declaration.function.body {
            body.visit_with(&mut collector);
        }
        collector.finish()
    }

    /// A nested parameter is invisible to the module-wide fold and shadowing
    /// on the per-function path: the two readings, stated.
    #[test]
    fn a_nested_parameter_shadows_only_on_the_per_function_path() {
        let source = r#"
            import { manager } from "@scope/core/v3";
            export function run(id: string) {
              const client = manager.clientOrThrow();
              const inner = (client: Other) => client.list(id);
              return inner;
            }
        "#;
        let (_, module) = parse_standalone_module(Path::new("origins.ts"), source).expect("parses");

        // Module-wide (carrick#666): unchanged, the parameter is not a binding
        // this reading records at all.
        assert_eq!(
            collect_receiver_origins(&module)
                .get("client")
                .map(String::as_str),
            Some("@scope/core/v3")
        );

        // Per function (carrick#781): the nested parameter shadows it, because
        // the nested call site is folded into this function's.
        let mut collector = ReceiverOriginCollector::per_function();
        collector.record_module_scope(&module);
        module.visit_with(&mut collector);
        assert!(!collector.finish().contains_key("client"));
    }

    #[test]
    fn one_body_is_read_on_top_of_the_module_scope() {
        let source = r#"
            import { manager } from "@scope/core/v3";
            const shared = manager.clientOrThrow();
            export function first() {
              const client = manager.clientOrThrow();
              return client;
            }
            export function second(client) {
              return client;
            }
            export function third() {
              const client = makeSomethingElse();
              return shared;
            }
        "#;
        // The module-scope const is visible to every body.
        for function in ["first", "second", "third"] {
            assert_eq!(
                scope_origins(source, function)
                    .get("shared")
                    .map(String::as_str),
                Some("@scope/core/v3"),
                "{function} must see the module-scope binding"
            );
        }
        // A local in one function says nothing about the same name in another.
        assert_eq!(
            scope_origins(source, "first")
                .get("client")
                .map(String::as_str),
            Some("@scope/core/v3")
        );
        // A parameter shadows whatever the module scope binds.
        assert!(!scope_origins(source, "second").contains_key("client"));
        // A local bound from something that is not an import has no origin.
        assert!(!scope_origins(source, "third").contains_key("client"));
    }

    #[test]
    fn a_type_only_import_states_no_origin() {
        let origins = origins(
            r#"
            import type { ApiClient } from "@scope/core";
            "#,
        );
        assert!(!origins.contains_key("ApiClient"));
    }
}
