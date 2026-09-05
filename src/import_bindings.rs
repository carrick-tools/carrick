//! Resolve an imported binding to the module that actually defines it.
//!
//! A mount site names its child by whatever local binding the importing file
//! happens to use (`fastify.register(sessionsRoutes)`, `app.use(userRouter)`).
//! Attributing the mounted routes to that binding needs the module the binding
//! came FROM, and a barrel breaks the naive answer:
//!
//! ```text
//! routes.ts:   export { default as sessionsRoutes } from "./modules/sessions/sessions.routes.js";
//!              export { default as logsRoutes }     from "./modules/logs/logs.routes.js";
//! plugin.ts:   import { sessionsRoutes, logsRoutes } from "./routes.js";
//! ```
//!
//! Every one of those bindings has the SAME import specifier, and each module
//! usually names its own plugin identically (`const routes = ...; export
//! default routes`). Anything that keys on the specifier, on the local symbol
//! name, or on a substring of the file path collapses all of them onto one
//! module — last write wins.
//!
//! This module resolves the pair that is actually unique: **(file, exported
//! name)**. It reads each module's export table with SWC and follows
//! re-export hops — `export { default as X } from`, `export { a as b } from`,
//! and `export * from` — until it reaches the module that declares the
//! binding. Purely structural: no framework knowledge, no naming heuristics.
//!
//! Non-relative specifiers (packages, tsconfig path aliases) are out of scope
//! and resolve to `None`, because reaching them needs the sidecar's tsconfig
//! knowledge; the caller falls back to its previous behaviour there.
//!
//! One published form names no binding at all: `export * as queues from
//! "./queues.js"` publishes a name standing for a whole MODULE. It is carried
//! in its own table and answered by
//! [`resolve_namespace_export`](BindingResolver::resolve_namespace_export),
//! never mixed into the value walk — a namespace object is not a router to
//! mount or a class to read a controller off, so every value question about
//! such a name still answers `None` (carrick#679).

use crate::agents::file_orchestrator::FileOrchestrator;
use crate::parser::parse_file;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use swc_common::{
    SourceMap,
    errors::{ColorConfig, Handler},
    sync::Lrc,
};
use swc_ecma_ast::{
    Decl, DefaultDecl, ExportSpecifier, Expr, ModuleDecl, ModuleExportName, ModuleItem, Pat,
};

/// The export name a default export is published under. Not a valid
/// identifier, so it can never collide with a named export in the same table.
const DEFAULT_EXPORT: &str = "default";

/// How many re-export hops a single binding may be followed through. A barrel
/// in front of a barrel is two; a deeper chain is indistinguishable from a
/// mis-resolution and each hop costs a file parse.
const MAX_HOPS: usize = 8;

/// Hard cap on modules parsed while resolving one binding, so a wide
/// `export *` fan-out cannot turn one import into an unbounded parse storm.
const MAX_VISITS: usize = 64;

/// Where an imported binding is defined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBinding {
    /// Canonical path of the module that declares the binding.
    pub file: PathBuf,
    /// The local symbol the module declares it as (`export default routes` →
    /// `routes`). `None` when the export has no nameable local binding —
    /// `export default async (server) => {}` — which is common enough that
    /// callers must treat identity as file-level and use this only to break
    /// ties between two bindings resolved to the same file.
    pub local_name: Option<String>,
}

/// One module's export table, as written in its source.
#[derive(Debug, Default, Clone)]
struct ModuleExports {
    /// Exported name → the local binding it names in THIS module.
    local: HashMap<String, Option<String>>,
    /// Exported name → (module specifier, name inside that module).
    forwarded: HashMap<String, (String, String)>,
    /// `export * from "./x"` specifiers, in source order.
    stars: Vec<String>,
    /// `export * as ns from "./m"` — exported name → the specifier whose whole
    /// module the name stands for. Kept apart from [`local`](Self::local) and
    /// [`forwarded`](Self::forwarded) because the name binds no VALUE: there
    /// is nothing in `./m` to resolve it to, only `./m` itself (carrick#679).
    namespaces: HashMap<String, String>,
    /// Exported names this module declares as a TYPE (`export type X = …`,
    /// `export { type X }`). Kept apart from `local` so a module that exports
    /// a type and a value under one name cannot have either shadow the other
    /// (carrick#670).
    type_local: HashSet<String>,
    /// Type-only re-exports: exported name → (module specifier, name inside
    /// that module). `export type { X } from "./m"`, `export { type X } from
    /// "./m"`.
    type_forwarded: HashMap<String, (String, String)>,
}

/// Resolver with a per-file export-table cache. Parsing is the expensive part
/// and barrels are re-read by every importer, so one resolver should be built
/// per pass and reused across every binding it resolves.
pub struct BindingResolver {
    source_map: Lrc<SourceMap>,
    handler: Handler,
    exports: HashMap<PathBuf, ModuleExports>,
}

impl Default for BindingResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl BindingResolver {
    pub fn new() -> Self {
        let source_map: Lrc<SourceMap> = Default::default();
        // Quiet, non-colour diagnostics: a barrel that fails to parse is a
        // resolution miss, not something to shout about mid-scan.
        let handler =
            Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(source_map.clone()));
        Self {
            source_map,
            handler,
            exports: HashMap::new(),
        }
    }

    /// Resolve the binding `local_binding` that `importer` imports from
    /// `specifier` to the module that declares it.
    ///
    /// `local_binding` is the name the IMPORTING file uses. Two export names
    /// are tried against the target module, in order: the binding name itself
    /// (`import { sessionsRoutes } from "./routes.js"` — the usual named
    /// import, where local and exported names agree) and then `default`
    /// (`import sessionsRoutes from "./sessions.routes.js"`). A renaming
    /// import (`import { a as b }`) is not recoverable from a mount site
    /// alone and resolves to `None` rather than to a guess.
    pub fn resolve(
        &mut self,
        importer: &Path,
        specifier: &str,
        local_binding: &str,
    ) -> Option<ResolvedBinding> {
        let target = FileOrchestrator::resolve_relative_import(importer, specifier)?;
        if local_binding != DEFAULT_EXPORT
            && let Some(found) = self.follow(&target, local_binding)
        {
            return Some(found);
        }
        self.follow(&target, DEFAULT_EXPORT)
    }

    /// Resolve one named export of `file` to the module that declares it.
    ///
    /// The same walk [`resolve`](Self::resolve) performs, entered from the
    /// module side rather than from an importing file: the caller already
    /// holds the entry module and asks what a given export of it resolves to.
    /// `export_name` is the name as PUBLISHED (`default` for a default
    /// export), not a local binding.
    pub fn resolve_export(&mut self, file: &Path, export_name: &str) -> Option<ResolvedBinding> {
        self.follow(file, export_name)
    }

    /// Resolve a TYPE imported from `specifier` to the module that DECLARES
    /// it, following the same re-export hops values follow (carrick#670).
    ///
    /// Types live in their own declaration space, so this walks the type
    /// tables and never the value ones: a module that exports a type and a
    /// value under one name resolves each independently. The caller gets the
    /// declaring file and reads the declaration itself — deciding what a given
    /// type MEANS is the caller's job, not the resolver's.
    pub fn resolve_type(
        &mut self,
        importer: &Path,
        specifier: &str,
        exported_name: &str,
    ) -> Option<PathBuf> {
        let target = FileOrchestrator::resolve_relative_import(importer, specifier)?;
        let mut visited: HashSet<PathBuf> = HashSet::new();
        visited.insert(target.clone());
        self.follow_type(target, exported_name.to_string(), 0, &mut visited)
    }

    fn follow_type(
        &mut self,
        file: PathBuf,
        export_name: String,
        hops: usize,
        visited: &mut HashSet<PathBuf>,
    ) -> Option<PathBuf> {
        if hops > MAX_HOPS || visited.len() > MAX_VISITS {
            return None;
        }
        let exports = self.exports_of(&file)?;

        if let Some((specifier, upstream)) = exports.type_forwarded.get(&export_name).cloned() {
            let next = FileOrchestrator::resolve_relative_import(&file, &specifier)?;
            if !visited.insert(next.clone()) {
                return None; // circular re-export
            }
            return self.follow_type(next, upstream, hops + 1, visited);
        }

        if exports.type_local.contains(&export_name) {
            return Some(file);
        }

        // A plain `export * from "./m"` republishes types as well as values.
        for specifier in exports.stars.clone() {
            let Some(next) = FileOrchestrator::resolve_relative_import(&file, &specifier) else {
                continue;
            };
            if !visited.insert(next.clone()) {
                continue;
            }
            if let Some(found) = self.follow_type(next, export_name.clone(), hops + 1, visited) {
                return Some(found);
            }
        }

        None
    }

    /// Every name `file` publishes, deduplicated and sorted.
    ///
    /// Locally declared exports and one-hop re-exports are read off the
    /// module's own table; `export * from "./m"` is followed so a barrel
    /// reports what it republishes rather than reporting nothing. The star
    /// walk is bounded by the same hop and visit caps a single resolution
    /// gets, and a star never republishes a default (mirroring
    /// [`follow_inner`](Self::follow_inner)).
    ///
    /// A namespace re-export is listed like any other name: `export * as
    /// queues from "./queues.js"` publishes `queues`, and a caller that wants
    /// what stands behind it asks
    /// [`resolve_namespace_export`](Self::resolve_namespace_export) —
    /// [`resolve_export`](Self::resolve_export) answers `None` for it, because
    /// a module is not a value binding (carrick#679).
    pub fn export_names(&mut self, file: &Path) -> Vec<String> {
        let mut names = Vec::new();
        let mut visited: HashSet<PathBuf> = HashSet::new();
        visited.insert(file.to_path_buf());
        self.collect_export_names(file.to_path_buf(), 0, &mut visited, &mut names);
        names.sort();
        names.dedup();
        names
    }

    fn collect_export_names(
        &mut self,
        file: PathBuf,
        hops: usize,
        visited: &mut HashSet<PathBuf>,
        names: &mut Vec<String>,
    ) {
        if hops > MAX_HOPS || visited.len() > MAX_VISITS {
            return;
        }
        let Some(exports) = self.exports_of(&file) else {
            return;
        };
        names.extend(exports.local.keys().cloned());
        names.extend(exports.forwarded.keys().cloned());
        names.extend(exports.namespaces.keys().cloned());
        for specifier in exports.stars.clone() {
            let Some(next) = FileOrchestrator::resolve_relative_import(&file, &specifier) else {
                continue;
            };
            if !visited.insert(next.clone()) {
                continue;
            }
            let mut republished = Vec::new();
            self.collect_export_names(next, hops + 1, visited, &mut republished);
            // A star republishes named exports only, never a default.
            names.extend(republished.into_iter().filter(|n| n != DEFAULT_EXPORT));
        }
    }

    /// Follow `export_name` out of `file` through re-export hops to the
    /// module that declares it.
    fn follow(&mut self, file: &Path, export_name: &str) -> Option<ResolvedBinding> {
        let mut visited: HashSet<PathBuf> = HashSet::new();
        visited.insert(file.to_path_buf());
        self.follow_inner(file.to_path_buf(), export_name.to_string(), 0, &mut visited)
    }

    fn follow_inner(
        &mut self,
        file: PathBuf,
        export_name: String,
        hops: usize,
        visited: &mut HashSet<PathBuf>,
    ) -> Option<ResolvedBinding> {
        if hops > MAX_HOPS || visited.len() > MAX_VISITS {
            return None;
        }

        let exports = self.exports_of(&file)?;

        // `export { x as y } from "./m"` — the binding lives one hop away.
        if let Some((specifier, upstream_name)) = exports.forwarded.get(&export_name).cloned() {
            let next = FileOrchestrator::resolve_relative_import(&file, &specifier)?;
            if !visited.insert(next.clone()) {
                return None; // circular re-export
            }
            return self.follow_inner(next, upstream_name, hops + 1, visited);
        }

        // Declared here.
        if let Some(local_name) = exports.local.get(&export_name).cloned() {
            return Some(ResolvedBinding { file, local_name });
        }

        // `export * from "./m"` re-publishes every NAMED export of the target
        // but never its default, so a default lookup stops here.
        if export_name != DEFAULT_EXPORT {
            for specifier in exports.stars.clone() {
                let Some(next) = FileOrchestrator::resolve_relative_import(&file, &specifier)
                else {
                    continue;
                };
                if !visited.insert(next.clone()) {
                    continue;
                }
                if let Some(found) = self.follow_inner(next, export_name.clone(), hops + 1, visited)
                {
                    return Some(found);
                }
            }
        }

        None
    }

    /// The module a namespace re-export of `file` stands for (carrick#679).
    ///
    /// `export * as queues from "./queues.js"` publishes the NAME `queues`
    /// bound to the whole of `queues.js`, so it answers with a module rather
    /// than with a [`ResolvedBinding`]: there is no single value in the target
    /// to resolve it to. What the caller does with the module is its own
    /// business — `sdk_surface` publishes a member per function it exports,
    /// `call_graph` resolves `queues.list(...)` against its definitions.
    ///
    /// The name itself is followed through the same hops a value takes: a
    /// barrel that stars over the re-exporting module republishes the
    /// namespace binding, and `export { queues as teams } from` forwards it.
    /// A name that is not a namespace re-export resolves to `None`, which is
    /// what every value export is.
    pub fn resolve_namespace_export(&mut self, file: &Path, export_name: &str) -> Option<PathBuf> {
        let mut visited: HashSet<PathBuf> = HashSet::new();
        visited.insert(file.to_path_buf());
        self.follow_namespace(file.to_path_buf(), export_name.to_string(), 0, &mut visited)
    }

    fn follow_namespace(
        &mut self,
        file: PathBuf,
        export_name: String,
        hops: usize,
        visited: &mut HashSet<PathBuf>,
    ) -> Option<PathBuf> {
        if hops > MAX_HOPS || visited.len() > MAX_VISITS {
            return None;
        }
        let exports = self.exports_of(&file)?;

        // Declared here: `export * as ns from "./m"`.
        if let Some(specifier) = exports.namespaces.get(&export_name).cloned() {
            return FileOrchestrator::resolve_relative_import(&file, &specifier);
        }

        // `export { ns as alias } from "./m"` forwards the binding one hop.
        if let Some((specifier, upstream)) = exports.forwarded.get(&export_name).cloned() {
            let next = FileOrchestrator::resolve_relative_import(&file, &specifier)?;
            if !visited.insert(next.clone()) {
                return None; // circular re-export
            }
            return self.follow_namespace(next, upstream, hops + 1, visited);
        }

        // A value declared here is not a namespace, and a star never
        // republishes a default.
        if exports.local.contains_key(&export_name) || export_name == DEFAULT_EXPORT {
            return None;
        }

        for specifier in exports.stars.clone() {
            let Some(next) = FileOrchestrator::resolve_relative_import(&file, &specifier) else {
                continue;
            };
            if !visited.insert(next.clone()) {
                continue;
            }
            if let Some(found) = self.follow_namespace(next, export_name.clone(), hops + 1, visited)
            {
                return Some(found);
            }
        }

        None
    }

    fn exports_of(&mut self, file: &Path) -> Option<&ModuleExports> {
        if !self.exports.contains_key(file) {
            let module = parse_file(file, &self.source_map, &self.handler)?;
            self.exports
                .insert(file.to_path_buf(), collect_exports(&module));
        }
        self.exports.get(file)
    }
}

/// Read a module's export table from its AST.
///
/// Type-only exports are skipped throughout: they bind nothing at runtime, so
/// they can never be the plugin or router a mount site registers.
fn collect_exports(module: &swc_ecma_ast::Module) -> ModuleExports {
    let mut exports = ModuleExports::default();

    for item in &module.body {
        let ModuleItem::ModuleDecl(decl) = item else {
            continue;
        };
        match decl {
            // `export const routes = ...`, `export function f() {}`, `export class C {}`
            ModuleDecl::ExportDecl(export) => match &export.decl {
                Decl::Fn(f) => {
                    let name = f.ident.sym.to_string();
                    exports.local.insert(name.clone(), Some(name));
                }
                Decl::Class(c) => {
                    let name = c.ident.sym.to_string();
                    exports.local.insert(name.clone(), Some(name));
                }
                Decl::Var(var) => {
                    for declarator in &var.decls {
                        if let Pat::Ident(ident) = &declarator.name {
                            let name = ident.id.sym.to_string();
                            exports.local.insert(name.clone(), Some(name));
                        }
                    }
                }
                // `export type X = …` — a type export, tracked separately
                // from the value table (carrick#670).
                Decl::TsTypeAlias(alias) => {
                    exports.type_local.insert(alias.id.sym.to_string());
                }
                _ => {}
            },
            // `export default function routes() {}`, `export default class C {}`
            ModuleDecl::ExportDefaultDecl(export) => match &export.decl {
                DefaultDecl::Fn(f) => {
                    exports.local.insert(
                        DEFAULT_EXPORT.to_string(),
                        f.ident.as_ref().map(|i| i.sym.to_string()),
                    );
                }
                DefaultDecl::Class(c) => {
                    exports.local.insert(
                        DEFAULT_EXPORT.to_string(),
                        c.ident.as_ref().map(|i| i.sym.to_string()),
                    );
                }
                DefaultDecl::TsInterfaceDecl(_) => {}
            },
            // `export default routes;`, `export default async (s) => {};`
            ModuleDecl::ExportDefaultExpr(export) => {
                let local = match &*export.expr {
                    Expr::Ident(ident) => Some(ident.sym.to_string()),
                    _ => None,
                };
                exports.local.insert(DEFAULT_EXPORT.to_string(), local);
            }
            ModuleDecl::ExportNamed(named) => {
                // Type-only specifiers feed the TYPE tables, never the value
                // ones: `export type { X } from "./m"` republishes a type.
                for spec in &named.specifiers {
                    let ExportSpecifier::Named(spec) = spec else {
                        continue;
                    };
                    if !named.type_only && !spec.is_type_only {
                        continue;
                    }
                    let upstream = export_name_string(&spec.orig);
                    let exported = spec
                        .exported
                        .as_ref()
                        .map(export_name_string)
                        .unwrap_or_else(|| upstream.clone());
                    match &named.src {
                        Some(src) => {
                            exports
                                .type_forwarded
                                .insert(exported, (src.value.to_string(), upstream));
                        }
                        None => {
                            exports.type_local.insert(exported);
                        }
                    }
                }
                if named.type_only {
                    continue;
                }
                match &named.src {
                    // `export { a as b } from "./m"`, `export { default as X } from "./m"`
                    Some(src) => {
                        let specifier = src.value.to_string();
                        for spec in &named.specifiers {
                            match spec {
                                ExportSpecifier::Named(spec) if !spec.is_type_only => {
                                    let upstream = export_name_string(&spec.orig);
                                    let exported = spec
                                        .exported
                                        .as_ref()
                                        .map(export_name_string)
                                        .unwrap_or_else(|| upstream.clone());
                                    exports
                                        .forwarded
                                        .insert(exported, (specifier.clone(), upstream));
                                }
                                // `export v from "./m"` — the default under a new name.
                                ExportSpecifier::Default(spec) => {
                                    exports.forwarded.insert(
                                        spec.exported.sym.to_string(),
                                        (specifier.clone(), DEFAULT_EXPORT.to_string()),
                                    );
                                }
                                // `export * as ns from "./m"` binds the whole
                                // module under one name (carrick#679).
                                ExportSpecifier::Namespace(spec) => {
                                    exports
                                        .namespaces
                                        .insert(export_name_string(&spec.name), specifier.clone());
                                }
                                _ => {}
                            }
                        }
                    }
                    // `export { routes as default }`, `export { router }` — local.
                    None => {
                        for spec in &named.specifiers {
                            if let ExportSpecifier::Named(spec) = spec
                                && !spec.is_type_only
                            {
                                let local = export_name_string(&spec.orig);
                                let exported = spec
                                    .exported
                                    .as_ref()
                                    .map(export_name_string)
                                    .unwrap_or_else(|| local.clone());
                                exports.local.insert(exported, Some(local));
                            }
                        }
                    }
                }
            }
            ModuleDecl::ExportAll(export) if !export.type_only => {
                exports.stars.push(export.src.value.to_string());
            }
            _ => {}
        }
    }

    exports
}

fn export_name_string(name: &ModuleExportName) -> String {
    match name {
        ModuleExportName::Ident(ident) => ident.sym.to_string(),
        ModuleExportName::Str(s) => s.value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Write a small module tree and return its canonicalized root.
    fn workspace(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        for (relative, content) in files {
            let path = dir.path().join(relative);
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            fs::write(&path, content).expect("write");
        }
        let root = dir.path().canonicalize().expect("canonicalize root");
        (dir, root)
    }

    #[test]
    fn resolves_default_reexport_through_a_barrel_to_its_own_module() {
        let (_dir, root) = workspace(&[
            (
                "src/routes.ts",
                r#"
export { default as sessionsRoutes } from "./modules/sessions/sessions.routes.js";
export { default as logsRoutes } from "./modules/logs/logs.routes.js";
"#,
            ),
            (
                "src/modules/sessions/sessions.routes.ts",
                "const routes = async (server) => {};\nexport default routes;\n",
            ),
            (
                "src/modules/logs/logs.routes.ts",
                "const logsRoutes = async (server) => {};\nexport default logsRoutes;\n",
            ),
            (
                "src/plugin.ts",
                "import { sessionsRoutes } from './routes.js';\n",
            ),
        ]);

        let mut resolver = BindingResolver::new();
        let importer = root.join("src/plugin.ts");

        let sessions = resolver
            .resolve(&importer, "./routes.js", "sessionsRoutes")
            .expect("sessionsRoutes resolves");
        assert_eq!(
            sessions.file,
            root.join("src/modules/sessions/sessions.routes.ts")
        );
        assert_eq!(sessions.local_name.as_deref(), Some("routes"));

        let logs = resolver
            .resolve(&importer, "./routes.js", "logsRoutes")
            .expect("logsRoutes resolves");
        assert_eq!(logs.file, root.join("src/modules/logs/logs.routes.ts"));
        assert_eq!(logs.local_name.as_deref(), Some("logsRoutes"));
    }

    #[test]
    fn resolves_renaming_reexport_and_star_barrel() {
        let (_dir, root) = workspace(&[
            (
                "src/index.ts",
                r#"
export { router as userRouter } from "./users.js";
export * from "./health.js";
"#,
            ),
            ("src/users.ts", "export const router = 1;\n"),
            ("src/health.ts", "export const healthRouter = 1;\n"),
            ("src/app.ts", "import { userRouter } from './index.js';\n"),
        ]);

        let mut resolver = BindingResolver::new();
        let importer = root.join("src/app.ts");

        let user = resolver
            .resolve(&importer, "./index.js", "userRouter")
            .expect("renamed re-export resolves");
        assert_eq!(user.file, root.join("src/users.ts"));
        assert_eq!(user.local_name.as_deref(), Some("router"));

        let health = resolver
            .resolve(&importer, "./index.js", "healthRouter")
            .expect("star re-export resolves");
        assert_eq!(health.file, root.join("src/health.ts"));
        assert_eq!(health.local_name.as_deref(), Some("healthRouter"));
    }

    #[test]
    fn resolves_direct_default_import_without_a_barrel() {
        let (_dir, root) = workspace(&[
            (
                "src/modules/a.routes.ts",
                "const routes = async (server) => {};\nexport default routes;\n",
            ),
            (
                "src/plugin.ts",
                "import aRoutes from './modules/a.routes.js';\n",
            ),
        ]);

        let mut resolver = BindingResolver::new();
        let binding = resolver
            .resolve(
                &root.join("src/plugin.ts"),
                "./modules/a.routes.js",
                "aRoutes",
            )
            .expect("default import resolves");
        assert_eq!(binding.file, root.join("src/modules/a.routes.ts"));
        assert_eq!(binding.local_name.as_deref(), Some("routes"));
    }

    #[test]
    fn anonymous_default_export_resolves_to_the_file_with_no_local_name() {
        let (_dir, root) = workspace(&[
            ("src/a.routes.ts", "export default async (server) => {};\n"),
            ("src/plugin.ts", "import aRoutes from './a.routes.js';\n"),
        ]);

        let mut resolver = BindingResolver::new();
        let binding = resolver
            .resolve(&root.join("src/plugin.ts"), "./a.routes.js", "aRoutes")
            .expect("anonymous default resolves to its file");
        assert_eq!(binding.file, root.join("src/a.routes.ts"));
        assert_eq!(binding.local_name, None);
    }

    #[test]
    fn star_reexport_does_not_carry_default_and_cycles_terminate() {
        let (_dir, root) = workspace(&[
            ("src/a.ts", "export * from \"./b.js\";\n"),
            (
                "src/b.ts",
                "export * from \"./a.js\";\nconst r = 1;\nexport default r;\n",
            ),
            ("src/app.ts", "import x from './a.js';\n"),
        ]);

        let mut resolver = BindingResolver::new();
        assert_eq!(
            resolver.resolve(&root.join("src/app.ts"), "./a.js", "x"),
            None,
            "`export *` must not republish a default, and the cycle must terminate"
        );
    }

    #[test]
    fn non_relative_specifier_is_out_of_scope() {
        let (_dir, root) = workspace(&[("src/app.ts", "import x from '@acme/routes';\n")]);
        let mut resolver = BindingResolver::new();
        assert_eq!(
            resolver.resolve(&root.join("src/app.ts"), "@acme/routes", "x"),
            None
        );
    }

    #[test]
    fn type_only_reexport_binds_nothing() {
        let (_dir, root) = workspace(&[
            (
                "src/index.ts",
                "export type { Routes } from \"./types.js\";\n",
            ),
            ("src/types.ts", "export type Routes = string;\n"),
            ("src/app.ts", "import { Routes } from './index.js';\n"),
        ]);

        let mut resolver = BindingResolver::new();
        assert_eq!(
            resolver.resolve(&root.join("src/app.ts"), "./index.js", "Routes"),
            None
        );
    }

    /// One entry publishing every re-export form at once, so admitting a new
    /// one (carrick#679) cannot quietly move the others. This table is the
    /// shared export table EVERY pass reads — mount resolution, the class
    /// controller join, wrapper/SDK surface walking and call-edge resolution —
    /// so each of those has a test of its own pinned against a tree of this
    /// shape.
    ///
    /// `groups` is a namespace in front of a module of plain functions (the
    /// shape the ticket is about), `models` a namespace in front of a barrel
    /// of classes, and `loop` a namespace onto a module that stars back to
    /// this entry.
    fn every_export_form() -> (tempfile::TempDir, PathBuf) {
        workspace(&[
            (
                "src/index.ts",
                r#"
export * as groups from "./groups.js";
export * as models from "./models/index.js";
export * as loop from "./cycle.js";
export * from "./health.js";
export { router as userRouter } from "./users.js";
export const version = "1";
export type { Shape } from "./shapes.js";
"#,
            ),
            (
                "src/groups.ts",
                "export function list() {}\nexport function retrieve(id: string) { return id; }\n",
            ),
            (
                "src/models/index.ts",
                "export { Widget } from \"./widget.js\";\n",
            ),
            (
                "src/models/widget.ts",
                "export class Widget { build() {} }\n",
            ),
            ("src/cycle.ts", "export * from \"./index.js\";\n"),
            ("src/health.ts", "export const healthRouter = 1;\n"),
            ("src/users.ts", "export const router = 1;\n"),
            ("src/shapes.ts", "export type Shape = string;\n"),
            ("src/app.ts", "import { version } from './index.js';\n"),
        ])
    }

    /// PIN (carrick#679). The forms that already resolved must resolve to the
    /// same module and local name once the namespace form is admitted, and a
    /// namespace must not leak into the value walk as a binding: `resolve` and
    /// `resolve_export` answer about VALUES, and a namespace object is not one.
    #[test]
    fn every_export_form_resolves_to_the_module_that_declares_it() {
        let (_dir, root) = every_export_form();
        let mut resolver = BindingResolver::new();
        let entry = root.join("src/index.ts");
        let importer = root.join("src/app.ts");

        // `export * from "./health.js"`
        let health = resolver
            .resolve(&importer, "./index.js", "healthRouter")
            .expect("star re-export resolves");
        assert_eq!(health.file, root.join("src/health.ts"));
        assert_eq!(health.local_name.as_deref(), Some("healthRouter"));

        // `export { router as userRouter } from "./users.js"`
        let user = resolver
            .resolve(&importer, "./index.js", "userRouter")
            .expect("renaming re-export resolves");
        assert_eq!(user.file, root.join("src/users.ts"));
        assert_eq!(user.local_name.as_deref(), Some("router"));

        // A plain named export of the entry itself.
        let version = resolver
            .resolve_export(&entry, "version")
            .expect("plain named export resolves");
        assert_eq!(version.file, entry);
        assert_eq!(version.local_name.as_deref(), Some("version"));

        // The namespace names bind a module, so the value walk answers
        // nothing for them — a mount site or a controller binding naming one
        // must resolve to no module rather than to the wrong one.
        for namespace in ["groups", "models", "loop"] {
            assert_eq!(
                resolver.resolve_export(&entry, namespace),
                None,
                "`export * as {namespace}` is not a value binding"
            );
            assert_eq!(
                resolver.resolve(&importer, "./index.js", namespace),
                None,
                "`export * as {namespace}` is not a value binding"
            );
        }
    }

    /// PIN (carrick#679). A namespace re-export publishes a VALUE name;
    /// admitting it must not put anything into the type space `socket_io`'s
    /// `resolve_type` walks. `models.Widget` as a type annotation is out of
    /// scope, the same way a package specifier is.
    #[test]
    fn a_namespace_reexport_publishes_no_type() {
        let (_dir, root) = every_export_form();
        let mut resolver = BindingResolver::new();
        let importer = root.join("src/app.ts");

        // The type form that does resolve, unchanged.
        assert_eq!(
            resolver.resolve_type(&importer, "./index.js", "Shape"),
            Some(root.join("src/shapes.ts"))
        );
        // The namespace names, which do not.
        for namespace in ["groups", "models", "loop"] {
            assert_eq!(
                resolver.resolve_type(&importer, "./index.js", namespace),
                None
            );
        }
    }

    /// A namespace re-export publishes its NAME, so the surface walk can see
    /// it at all. Every other name the entry publishes stays listed.
    #[test]
    fn export_names_lists_a_namespace_reexport_beside_the_other_forms() {
        let (_dir, root) = every_export_form();
        let mut resolver = BindingResolver::new();

        let names = resolver.export_names(&root.join("src/index.ts"));
        assert_eq!(
            names,
            vec![
                "groups".to_string(),
                "healthRouter".to_string(),
                "loop".to_string(),
                "models".to_string(),
                "userRouter".to_string(),
                "version".to_string(),
            ],
            "every published name, namespaces included"
        );
    }

    /// The resolution the ticket asks for: the name stands for a MODULE, so it
    /// answers with one rather than with a binding. A barrel in front of the
    /// module is followed like any other hop, and a cycle terminates.
    #[test]
    fn a_namespace_reexport_resolves_to_the_module_it_stands_for() {
        let (_dir, root) = every_export_form();
        let mut resolver = BindingResolver::new();
        let entry = root.join("src/index.ts");

        assert_eq!(
            resolver.resolve_namespace_export(&entry, "groups"),
            Some(root.join("src/groups.ts"))
        );
        assert_eq!(
            resolver.resolve_namespace_export(&entry, "models"),
            Some(root.join("src/models/index.ts")),
            "the barrel itself is the namespace; what it republishes is read \
             from it like any other module"
        );
        assert_eq!(
            resolver.resolve_namespace_export(&entry, "loop"),
            Some(root.join("src/cycle.ts"))
        );

        // A value export is not a namespace.
        assert_eq!(resolver.resolve_namespace_export(&entry, "version"), None);
        assert_eq!(
            resolver.resolve_namespace_export(&entry, "userRouter"),
            None
        );
    }

    /// A namespace re-export is republished by a star and forwarded by name,
    /// so it has to be followable through the same hops a value takes.
    #[test]
    fn a_namespace_reexport_is_reachable_through_a_barrel() {
        let (_dir, root) = workspace(&[
            (
                "src/public.ts",
                "export * from \"./index.js\";\nexport { groups as teams } from \"./index.js\";\n",
            ),
            ("src/index.ts", "export * as groups from \"./groups.js\";\n"),
            ("src/groups.ts", "export function list() {}\n"),
        ]);

        let mut resolver = BindingResolver::new();
        let public = root.join("src/public.ts");

        assert_eq!(
            resolver.resolve_namespace_export(&public, "groups"),
            Some(root.join("src/groups.ts")),
            "a star republishes a namespace binding like any other named export"
        );
        assert_eq!(
            resolver.resolve_namespace_export(&public, "teams"),
            Some(root.join("src/groups.ts")),
            "and a rename forwards it"
        );
        assert!(
            resolver
                .export_names(&public)
                .contains(&"teams".to_string())
        );
    }
}
