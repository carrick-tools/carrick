//! Files that define a GraphQL schema in code (carrick#1157).
//!
//! A code-first schema is built by calls on one value, a schema builder,
//! created from a library in one module and imported by every module that adds
//! fields to it. Those field modules often export nothing and make no HTTP
//! call, so the gatekeeper raises no candidate and they were skipped before
//! the file-analyzer saw them. The schema's fields were indexed from the
//! printed SDL, and no resolver location was ever joined to them.
//!
//! Which library builds the schema is not decided here. Framework detection
//! names the packages a service uses on its `data_fetchers` channel. This
//! module answers a structural question about one file: does it call, at
//! module scope, through a value that came out of one of those packages? The
//! value may be:
//!
//! - bound in the file itself (`const builder = new SchemaBuilder()` with
//!   `SchemaBuilder` imported from a listed package, or the import itself);
//! - imported from a same-repo module that binds it that way, followed by
//!   [`BindingResolver`] and through modules that import the value and export
//!   it again.
//!
//! Module scope is what separates building a schema from using a client. A
//! schema is registered when its modules load (`kit.queryFields(...)` as a
//! statement, `export const schema = kit.toSchema()`), while an HTTP or SDK
//! client from the same detection list is called inside the function that
//! handles a request. A client constructed at module scope (`new Client()`) is
//! not a call.
//!
//! The caller decides what a positive answer is worth. A file is admitted to
//! the model for its schema fields only when the service has schema fields to
//! link it to (see `FileOrchestrator`), so a client app that imports an HTTP
//! client instance in every component is not admitted for GraphQL.

use crate::import_bindings::BindingResolver;
use crate::parser::parse_file;
use crate::receiver_origin::{ReceiverOrigins, collect_receiver_origins, origin_root};
use crate::workspace_resolver::WorkspaceIndex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use swc_common::{
    SourceMap,
    errors::{ColorConfig, Handler},
    sync::Lrc,
};
use swc_ecma_ast::{
    Callee, Decl, Expr, ImportDecl, ImportSpecifier, Module, ModuleDecl, ModuleExportName,
    ModuleItem, Stmt,
};

/// Reads which files call through a value from a listed package, caching what
/// it reads of every module it follows an import into.
pub struct PackageValueReader {
    source_map: Lrc<SourceMap>,
    handler: Handler,
    resolver: BindingResolver,
    /// Module → its receiver origins and its named imports.
    modules: HashMap<PathBuf, (ReceiverOrigins, HashMap<String, String>)>,
}

/// How many modules an imported value is followed through before the answer
/// is "no": a module that declares it, behind a re-exporting module or two.
const MAX_IMPORT_HOPS: usize = 4;

impl PackageValueReader {
    /// `workspace` resolves aliased and package specifiers; without it only
    /// relative specifiers are followed.
    pub fn new(workspace: Option<WorkspaceIndex>) -> Self {
        let source_map: Lrc<SourceMap> = Default::default();
        let handler =
            Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(source_map.clone()));
        let resolver = match workspace {
            Some(workspace) => BindingResolver::with_workspace(workspace),
            None => BindingResolver::new(),
        };
        Self {
            source_map,
            handler,
            resolver,
            modules: HashMap::new(),
        }
    }

    /// Whether a call at module scope in `module`, the source of `file`, goes
    /// through a value that came out of one of `packages`.
    pub fn calls_package_value_at_module_scope(
        &mut self,
        file: &Path,
        module: &Module,
        packages: &[String],
    ) -> bool {
        if packages.is_empty() {
            return false;
        }
        let origins = collect_receiver_origins(module);
        let imports = named_imports(module);
        for receiver in &module_scope_call_receivers(module) {
            let Some(specifier) = origins.get(receiver) else {
                continue;
            };
            if is_listed(specifier, packages) {
                return true;
            }
            // The receiver IS an import from a module of this repo: follow it
            // to the module that binds it.
            let Some(imported) = imports.get(receiver) else {
                continue;
            };
            if self.import_comes_from(file, specifier, imported, packages) {
                return true;
            }
        }
        false
    }

    /// Whether the binding `importer` imports as `imported` from `specifier`
    /// is bound, where it is declared, to a value from a listed package. A
    /// module that imports the value and exports it again (`import { kit }
    /// from "./kit"; export { kit }`) declares nothing itself, so its import is
    /// followed in turn, up to [`MAX_IMPORT_HOPS`] modules.
    fn import_comes_from(
        &mut self,
        importer: &Path,
        specifier: &str,
        imported: &str,
        packages: &[String],
    ) -> bool {
        let mut importer = importer.to_path_buf();
        let mut specifier = specifier.to_string();
        let mut imported = imported.to_string();
        for _ in 0..MAX_IMPORT_HOPS {
            let Some(binding) = self.resolver.resolve(&importer, &specifier, &imported) else {
                return false;
            };
            let Some(local_name) = binding.local_name else {
                return false;
            };
            let (origins, imports) = self.module_facts(&binding.file);
            let Some(origin) = origins.get(&local_name) else {
                return false;
            };
            if is_listed(origin, packages) {
                return true;
            }
            let Some(next) = imports.get(&local_name) else {
                return false;
            };
            specifier = origin.clone();
            imported = next.clone();
            importer = binding.file;
        }
        false
    }

    fn module_facts(&mut self, file: &Path) -> (ReceiverOrigins, HashMap<String, String>) {
        if !self.modules.contains_key(file) {
            let facts = parse_file(file, &self.source_map, &self.handler)
                .map(|module| (collect_receiver_origins(&module), named_imports(&module)))
                .unwrap_or_default();
            self.modules.insert(file.to_path_buf(), facts);
        }
        self.modules[file].clone()
    }
}

/// A specifier names a listed package when it is the package or a subpath of
/// it, the matching convention the data-fetcher import recall uses.
fn is_listed(specifier: &str, packages: &[String]) -> bool {
    packages
        .iter()
        .any(|package| specifier == package || specifier.starts_with(&format!("{package}/")))
}

/// Local name → the name the module publishes it under (`default` for a
/// default import), for value imports that name a binding. A namespace import
/// names a module, not a binding, and is left out.
fn named_imports(module: &Module) -> HashMap<String, String> {
    let mut imports = HashMap::new();
    for item in &module.body {
        let ModuleItem::ModuleDecl(ModuleDecl::Import(ImportDecl {
            specifiers,
            type_only: false,
            ..
        })) = item
        else {
            continue;
        };
        for specifier in specifiers {
            match specifier {
                ImportSpecifier::Named(named) if !named.is_type_only => {
                    let imported = match &named.imported {
                        Some(ModuleExportName::Ident(ident)) => ident.sym.to_string(),
                        Some(ModuleExportName::Str(s)) => s.value.to_string(),
                        None => named.local.sym.to_string(),
                    };
                    imports.insert(named.local.sym.to_string(), imported);
                }
                ImportSpecifier::Default(default) => {
                    imports.insert(default.local.sym.to_string(), "default".to_string());
                }
                _ => {}
            }
        }
    }
    imports
}

/// The root identifier of the callee of every call that runs when the module
/// loads: an expression statement, a declarator initialiser, or a default
/// export, at module scope. `builder` for `builder.mutationFields(...)` and
/// for `export const schema = builder.toSchema()`. Calls inside a function,
/// including a callback passed to one of these calls, are not read.
fn module_scope_call_receivers(module: &Module) -> HashSet<String> {
    let mut expressions: Vec<&Expr> = Vec::new();
    for item in &module.body {
        match item {
            ModuleItem::Stmt(Stmt::Expr(statement)) => expressions.push(&statement.expr),
            ModuleItem::Stmt(Stmt::Decl(Decl::Var(var))) => {
                expressions.extend(var.decls.iter().filter_map(|d| d.init.as_deref()));
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => {
                if let Decl::Var(var) = &export.decl {
                    expressions.extend(var.decls.iter().filter_map(|d| d.init.as_deref()));
                }
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(export)) => {
                expressions.push(&export.expr);
            }
            _ => {}
        }
    }
    expressions
        .into_iter()
        .filter_map(|expr| match unwrap_value(expr) {
            Expr::Call(call) => match &call.callee {
                Callee::Expr(callee) => origin_root(callee).map(|root| root.sym.to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Look through the forms that pass a call's value along unchanged.
fn unwrap_value(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(inner) => unwrap_value(&inner.expr),
        Expr::Await(inner) => unwrap_value(&inner.arg),
        Expr::TsAs(inner) => unwrap_value(&inner.expr),
        Expr::TsSatisfies(inner) => unwrap_value(&inner.expr),
        Expr::TsNonNull(inner) => unwrap_value(&inner.expr),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILDER: &str = r#"import SchemaKit from "@example/schema-kit";
import type { Context } from "./context";

export const kit = new SchemaKit<{ Context: Context }>({});

kit.queryType({});
"#;

    /// A field module: no export, no HTTP, one import of the builder.
    const FIELDS: &str = r#"import { kit } from "../kit";
import { listParcels } from "./store";

// Colis — requêtes
kit.queryFields((t) => ({
  parcels: t.field({ type: ["Parcel"], resolve: () => listParcels() }),
}));
"#;

    fn write(root: &Path, rel: &str, content: &str) -> PathBuf {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    fn calls_package_value(
        reader: &mut PackageValueReader,
        file: &Path,
        packages: &[&str],
    ) -> bool {
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
        let module = parse_file(file, &cm, &handler).expect("parses");
        let packages: Vec<String> = packages.iter().map(|p| p.to_string()).collect();
        reader.calls_package_value_at_module_scope(file, &module, &packages)
    }

    #[test]
    fn a_field_module_calls_through_a_builder_imported_from_the_module_that_creates_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/kit.ts", BUILDER);
        let fields = write(root, "src/parcels/queries.ts", FIELDS);
        let mut reader = PackageValueReader::new(None);

        assert!(calls_package_value(
            &mut reader,
            &fields,
            &["@example/schema-kit"]
        ));
        assert!(
            !calls_package_value(&mut reader, &fields, &["@example/http-client"]),
            "a package the builder does not come from admits nothing"
        );
    }

    #[test]
    fn the_module_that_creates_the_builder_calls_through_it() {
        let tmp = tempfile::tempdir().unwrap();
        let kit = write(tmp.path(), "src/kit.ts", BUILDER);
        let mut reader = PackageValueReader::new(None);

        assert!(calls_package_value(
            &mut reader,
            &kit,
            &["@example/schema-kit"]
        ));
    }

    #[test]
    fn a_builder_reached_through_a_re_export_and_a_ts_extension_specifier() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/kit.ts", BUILDER);
        write(
            root,
            "src/schema.ts",
            "import { kit } from './kit.ts';\nexport { kit };\nexport const schema = kit.toSchema();\n",
        );
        let fields = write(
            root,
            "src/parcels/queries.ts",
            &FIELDS.replace("\"../kit\"", "\"../schema.ts\""),
        );
        let mut reader = PackageValueReader::new(None);

        assert!(calls_package_value(
            &mut reader,
            &fields,
            &["@example/schema-kit"]
        ));
    }

    #[test]
    fn a_detected_client_called_inside_a_function_is_not_a_schema_builder() {
        // The same detection list names HTTP and SDK clients. A client is
        // constructed at module scope and called when a request is handled.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "src/mail/client.ts",
            "import { MailClient } from '@example/mail';\nexport const mail = new MailClient({ region: 'eu' });\n",
        );
        let direct = write(
            root,
            "src/mail/notify.ts",
            "import { MailClient } from '@example/mail';\n\nconst client = new MailClient({ region: 'eu' });\n\nexport async function notify(to: string) {\n  await client.send({ to });\n}\n",
        );
        let through_module = write(
            root,
            "src/parcels/dispatched.ts",
            "import { mail } from '../mail/client';\n\nexport const onDispatched = (to: string) => mail.send({ to });\n",
        );
        let mut reader = PackageValueReader::new(None);

        assert!(!calls_package_value(
            &mut reader,
            &direct,
            &["@example/mail"]
        ));
        assert!(!calls_package_value(
            &mut reader,
            &through_module,
            &["@example/mail"]
        ));
    }

    #[test]
    fn an_imported_builder_only_named_in_a_type_position_is_not_a_call() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/kit.ts", BUILDER);
        let types = write(
            root,
            "src/parcels/types.ts",
            "import type { kit } from '../kit';\nexport type Kit = typeof kit;\nexport const label = String('parcel');\n",
        );
        let mut reader = PackageValueReader::new(None);

        assert!(!calls_package_value(
            &mut reader,
            &types,
            &["@example/schema-kit"]
        ));
    }

    #[test]
    fn an_aliased_builder_import_resolves_through_the_repo_config() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "tsconfig.json",
            r#"{"compilerOptions":{"baseUrl":".","paths":{"@/*":["src/*"]}}}"#,
        );
        write(root, "src/kit.ts", BUILDER);
        let fields = write(
            root,
            "src/parcels/queries.ts",
            &FIELDS.replace("\"../kit\"", "\"@/kit\""),
        );

        let mut relative_only = PackageValueReader::new(None);
        assert!(!calls_package_value(
            &mut relative_only,
            &fields,
            &["@example/schema-kit"]
        ));
        let mut with_aliases =
            PackageValueReader::new(Some(WorkspaceIndex::build_with_aliases(root, None)));
        assert!(calls_package_value(
            &mut with_aliases,
            &fields,
            &["@example/schema-kit"]
        ));
    }
}
