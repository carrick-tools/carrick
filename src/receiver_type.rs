//! Which class a local binding's value IS, as the file itself states it
//! (carrick#776).
//!
//! `call_graph` resolves `obj.foo()` by resolving `obj`, and a receiver bound
//! to an instance used to resolve to nothing at all:
//!
//! ```ignore
//! import type { ApiClient } from "@scope/core/v3";
//!
//! function readRun(runId: string, client: ApiClient) {
//!   return client.subscribeToRun(runId);
//! }
//! ```
//!
//! Nothing here needs inferring. The file DECLARES what `client` is, and a
//! declared type is a statement in the source exactly as an import is. This
//! pass records those statements: local binding -> the type identifier written
//! for it. Resolving that identifier to a class is `call_graph`'s job, and it
//! uses the same import walk it already uses for a class named directly, so a
//! type that names nothing in this repo simply resolves to nothing.
//!
//! The call graph reads these type expressions per lexical binding in
//! `visitor`, so captured receivers survive ownership attribution and
//! same-named declarations in nested scopes remain distinct. The collector
//! below also supports folding declarations and class fields into one table.
//!
//! What is read, and nothing else:
//!
//! - A binding whose annotation is a plain type reference — `client:
//!   ApiClient`, `client: Client<Run>`. The type ARGUMENTS are ignored: a
//!   generic instantiation names the same class its head does. A qualified
//!   name (`ns.Client`) is not read: the head names a namespace, not the
//!   class, and following it is the namespace walk's job, not this one's.
//! - A `const`/`let`/`var` initialised with `new X()`, where `X` is a plain
//!   identifier. The construction states the class as flatly as an annotation
//!   does.
//! - Parameters wherever they sit — a function, a method, an arrow — because
//!   the shape this pass exists for is a helper that takes the client it calls.
//!
//! Only identifier bindings are read: a destructured or rest pattern binds a
//! part of a value, and which part decides what the value is.
//!
//! Within one scope, a name bound twice to different types is ambiguous and
//! dropped rather than picked between, and so is a name bound once with a type
//! and once without. Mirrors [`crate::receiver_origin`], which answers the
//! same question for a value that traces back to an import rather than to a
//! declared type.
//!
//! A CLASS FIELD is the same statement one level out (carrick#782). `this.x`
//! is not a local binding — it belongs to the class, not to the method — so
//! [`class_field_types`] reads the class body once and the table is keyed by
//! class rather than by function. It reads exactly what a field DECLARES: an
//! annotated instance property, a `#private` one, or a constructor parameter
//! property. A field left unannotated states nothing, whatever its
//! initialiser, and is recorded as contested so a same-named annotation
//! elsewhere in the class cannot answer for it.

use std::collections::HashMap;

use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

/// Local binding name -> the type identifier declared for it.
pub type ReceiverTypes = HashMap<String, String>;

/// Which class each FIELD of one class is declared to be (carrick#782).
///
/// Instance fields only: `this.x` inside a method names an instance member,
/// and a static field is reached through the class name, which
/// [`crate::call_graph`] already resolves without this table. A private field
/// is keyed `#name`, matching how a `this.#x()` call site is recorded.
pub fn class_field_types(class: &Class) -> ReceiverTypes {
    let mut collector = ReceiverTypeCollector::default();
    for member in &class.body {
        match member {
            ClassMember::ClassProp(prop) if !prop.is_static => {
                let PropName::Ident(key) = &prop.key else {
                    continue;
                };
                let declared = prop
                    .type_ann
                    .as_deref()
                    .and_then(annotated_type_ident)
                    .map(|ident| ident.sym.to_string());
                collector.record(key.sym.to_string(), declared);
            }
            ClassMember::PrivateProp(prop) if !prop.is_static => {
                let declared = prop
                    .type_ann
                    .as_deref()
                    .and_then(annotated_type_ident)
                    .map(|ident| ident.sym.to_string());
                collector.record(format!("#{}", prop.key.name), declared);
            }
            // `constructor(private readonly client: ApiClient)` declares a
            // field in the same breath as the parameter. The defaulted form
            // (`= new ApiClient()`) declares the same field.
            ClassMember::Constructor(constructor) => {
                for param in &constructor.params {
                    let ParamOrTsParamProp::TsParamProp(prop) = param else {
                        continue;
                    };
                    let binding = match &prop.param {
                        TsParamPropParam::Ident(ident) => Some(ident),
                        TsParamPropParam::Assign(assign) => match &*assign.left {
                            Pat::Ident(ident) => Some(ident),
                            _ => None,
                        },
                    };
                    let Some(binding) = binding else {
                        continue;
                    };
                    let declared = binding
                        .type_ann
                        .as_deref()
                        .and_then(annotated_type_ident)
                        .map(|ident| ident.sym.to_string());
                    collector.record(binding.id.sym.to_string(), declared);
                }
            }
            _ => {}
        }
    }
    collector.finish()
}

/// The class identifier a type annotation names, or `None` when the
/// annotation is anything but an unqualified type reference.
pub(crate) fn annotated_type_ident(type_ann: &TsTypeAnn) -> Option<&Ident> {
    match &*type_ann.type_ann {
        TsType::TsTypeRef(reference) => match &reference.type_name {
            TsEntityName::Ident(ident) => Some(ident),
            TsEntityName::TsQualifiedName(_) => None,
        },
        _ => None,
    }
}

/// The class identifier a `new X()` initialiser names, through the wrappers
/// that pass a value along without replacing it.
pub(crate) fn constructed_type_ident(expr: &Expr) -> Option<&Ident> {
    match expr {
        Expr::New(new_expr) => match &*new_expr.callee {
            Expr::Ident(ident) => Some(ident),
            _ => None,
        },
        Expr::Await(await_expr) => constructed_type_ident(&await_expr.arg),
        Expr::Paren(paren) => constructed_type_ident(&paren.expr),
        Expr::TsAs(as_expr) => constructed_type_ident(&as_expr.expr),
        Expr::TsNonNull(non_null) => constructed_type_ident(&non_null.expr),
        Expr::TsSatisfies(satisfies) => constructed_type_ident(&satisfies.expr),
        _ => None,
    }
}

/// Reads one function's declared receiver types. Drive it over the function's
/// own parameters with [`record_pat`](Self::record_pat) and over its body with
/// `visit_with`, then take the table with [`finish`](Self::finish).
///
/// `None` against a name marks it contested: declared twice differently within
/// the scope, or declared once and left untyped once.
#[derive(Default)]
pub struct ReceiverTypeCollector {
    types: HashMap<String, Option<String>>,
}

impl ReceiverTypeCollector {
    /// Record what one binding pattern declares. Non-identifier patterns
    /// declare nothing and are skipped rather than recorded as contested: a
    /// destructured parameter binds names this pass has no statement about.
    pub fn record_pat(&mut self, pat: &Pat) {
        if let Pat::Ident(ident) = pat {
            let declared = ident
                .type_ann
                .as_deref()
                .and_then(annotated_type_ident)
                .map(|ident| ident.sym.to_string());
            self.record(ident.id.sym.to_string(), declared);
        }
    }

    /// The names this scope declares a class for.
    pub fn finish(self) -> ReceiverTypes {
        self.types
            .into_iter()
            .filter_map(|(name, declared)| declared.map(|type_name| (name, type_name)))
            .collect()
    }

    fn record(&mut self, name: String, type_name: Option<String>) {
        match self.types.get(&name) {
            Some(existing) if existing.as_deref() == type_name.as_deref() => {}
            Some(_) => {
                self.types.insert(name, None);
            }
            None => {
                self.types.insert(name, type_name);
            }
        }
    }
}

impl Visit for ReceiverTypeCollector {
    fn visit_param(&mut self, param: &Param) {
        self.record_pat(&param.pat);
        param.visit_children_with(self);
    }

    /// Arrow parameters are `Pat`s with no `Param` wrapper around them.
    fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
        for pat in &arrow.params {
            self.record_pat(pat);
        }
        arrow.visit_children_with(self);
    }

    fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
        if let Pat::Ident(ident) = &declarator.name {
            let declared = ident
                .type_ann
                .as_deref()
                .and_then(annotated_type_ident)
                .or_else(|| declarator.init.as_deref().and_then(constructed_type_ident))
                .map(|ident| ident.sym.to_string());
            self.record(ident.id.sym.to_string(), declared);
        }
        declarator.visit_children_with(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swc_scanner::parse_standalone_module;
    use std::path::Path;

    /// Walks the whole module as one scope. Production keys the table per
    /// function (see `FunctionDefinitionExtractor`); these cases exercise the
    /// rules that hold within any one scope.
    fn types(source: &str) -> ReceiverTypes {
        let (_, module) = parse_standalone_module(Path::new("types.ts"), source).expect("parses");
        let mut collector = ReceiverTypeCollector::default();
        module.visit_with(&mut collector);
        collector.finish()
    }

    #[test]
    fn a_typed_parameter_states_its_class() {
        let types = types(
            r#"
            import type { ApiClient } from "@scope/core/v3";
            function readRun(runId: string, client: ApiClient) {
              return client.subscribeToRun(runId);
            }
            "#,
        );
        assert_eq!(types.get("client").map(String::as_str), Some("ApiClient"));
        // A keyword type is not a type reference and names no class.
        assert_eq!(types.get("runId"), None);
    }

    #[test]
    fn a_generic_instantiation_names_the_class_its_head_names() {
        let types = types("function run(client: ApiClient<Run>) { return client.list(); }");
        assert_eq!(types.get("client").map(String::as_str), Some("ApiClient"));
    }

    #[test]
    fn an_arrow_parameter_is_read_too() {
        let types = types("export const run = (client: ApiClient) => client.list();");
        assert_eq!(types.get("client").map(String::as_str), Some("ApiClient"));
    }

    #[test]
    fn a_constructed_local_states_its_class() {
        let types = types(r#"const client = new ApiClient("http://x");"#);
        assert_eq!(types.get("client").map(String::as_str), Some("ApiClient"));
    }

    #[test]
    fn an_annotation_wins_over_the_initialiser() {
        let types = types("const client: ApiClient = new CachingClient();");
        assert_eq!(types.get("client").map(String::as_str), Some("ApiClient"));
    }

    #[test]
    fn a_qualified_type_name_states_nothing() {
        let types = types("function run(client: core.ApiClient) { return client.list(); }");
        assert_eq!(types.get("client"), None);
    }

    #[test]
    fn a_name_declared_two_different_ways_in_one_scope_is_dropped() {
        let types = types(
            r#"
            function run(client: ApiClient) {
              const inner = (client: BatchClient) => client.list();
              return inner;
            }
            "#,
        );
        assert_eq!(types.get("client"), None);
    }

    #[test]
    fn a_name_typed_once_and_untyped_once_in_one_scope_is_dropped() {
        let types = types(
            r#"
            function run(client: ApiClient) {
              const client = useClient();
              return client.list();
            }
            "#,
        );
        assert_eq!(types.get("client"), None);
    }

    #[test]
    fn a_destructured_binding_states_nothing() {
        let types = types("function run({ client }: Deps) { return client.list(); }");
        assert_eq!(types.get("client"), None);
    }

    /// Walk the module and read the fields of its first class declaration.
    fn fields(source: &str) -> ReceiverTypes {
        let (_, module) = parse_standalone_module(Path::new("fields.ts"), source).expect("parses");
        let class = module
            .body
            .iter()
            .find_map(|item| match item {
                ModuleItem::Stmt(Stmt::Decl(Decl::Class(class))) => Some(&class.class),
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => match &export.decl {
                    Decl::Class(class) => Some(&class.class),
                    _ => None,
                },
                _ => None,
            })
            .expect("a class declaration");
        class_field_types(class)
    }

    #[test]
    fn an_annotated_field_states_its_class() {
        let fields = fields(
            r#"
            export class Manager {
              private readonly client: ApiClient;
              #cache: CacheClient;
              static shared: SharedClient;
            }
            "#,
        );
        assert_eq!(fields.get("client").map(String::as_str), Some("ApiClient"));
        assert_eq!(
            fields.get("#cache").map(String::as_str),
            Some("CacheClient")
        );
        // A static field is reached through the class name, not `this`.
        assert_eq!(fields.get("shared"), None);
    }

    #[test]
    fn a_constructor_parameter_property_states_its_class() {
        let fields = fields(
            r#"
            export class Manager {
              constructor(
                private readonly client: ApiClient,
                public cache: CacheClient = new CacheClient(),
                plain: ApiClient,
              ) {}
            }
            "#,
        );
        assert_eq!(fields.get("client").map(String::as_str), Some("ApiClient"));
        assert_eq!(fields.get("cache").map(String::as_str), Some("CacheClient"));
        // A plain parameter is not a field: no modifier, so nothing is
        // assigned to `this`.
        assert_eq!(fields.get("plain"), None);
    }

    #[test]
    fn an_unannotated_field_states_nothing() {
        let fields = fields(
            r#"
            export class Manager {
              client = new ApiClient();
              other;
              constructor(private readonly deps) {}
            }
            "#,
        );
        assert_eq!(fields.get("client"), None);
        assert_eq!(fields.get("other"), None);
        assert_eq!(fields.get("deps"), None);
    }

    #[test]
    fn a_field_declared_two_different_ways_is_dropped() {
        let fields = fields(
            r#"
            export class Manager {
              client: ApiClient;
              constructor(private readonly client: BatchClient) {}
            }
            "#,
        );
        assert_eq!(fields.get("client"), None);
    }

    #[test]
    fn a_field_annotated_once_and_left_bare_once_is_dropped() {
        let fields = fields(
            r#"
            export class Manager {
              client: ApiClient;
              constructor(public client = makeClient()) {}
            }
            "#,
        );
        assert_eq!(fields.get("client"), None);
    }

    #[test]
    fn the_same_type_declared_twice_survives() {
        let types = types(
            r#"
            function run(client: ApiClient) {
              const again: ApiClient = client;
              return again.list();
            }
            "#,
        );
        assert_eq!(types.get("client").map(String::as_str), Some("ApiClient"));
        assert_eq!(types.get("again").map(String::as_str), Some("ApiClient"));
    }
}
