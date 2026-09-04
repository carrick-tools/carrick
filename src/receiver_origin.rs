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

use std::collections::HashMap;

use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

/// Local binding name -> the module specifier its value came from.
pub type ReceiverOrigins = HashMap<String, String>;

/// Read a module's receiver origins.
pub fn collect_receiver_origins(module: &Module) -> ReceiverOrigins {
    let mut visitor = OriginVisitor::default();
    for item in &module.body {
        if let ModuleItem::ModuleDecl(ModuleDecl::Import(import)) = item {
            if import.type_only {
                continue;
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
                visitor.record(local, Some(import.src.value.to_string()));
            }
        }
    }
    module.visit_with(&mut visitor);
    visitor
        .origins
        .into_iter()
        .filter_map(|(name, origin)| origin.map(|specifier| (name, specifier)))
        .collect()
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

/// `None` marks a name whose origin is contested: bound from two different
/// specifiers, or bound from something that is not an import at all.
#[derive(Default)]
struct OriginVisitor {
    origins: HashMap<String, Option<String>>,
}

impl OriginVisitor {
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

impl Visit for OriginVisitor {
    fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
        if let Pat::Ident(ident) = &declarator.name {
            let specifier = declarator
                .init
                .as_deref()
                .and_then(origin_root)
                .map(|root| root.sym.to_string())
                .and_then(|root| self.origins.get(&root).cloned().flatten());
            self.record(ident.id.sym.to_string(), specifier);
        }
        declarator.visit_children_with(self);
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
