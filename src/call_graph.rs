//! Resolve collected call sites to the functions they actually call.
//!
//! The scanner used to build its call graph by substring-searching each
//! function's raw body text for every function name in the repo (#581). A name
//! that occurred in a string literal, a template interpolation or a comment
//! became a call edge, and a bare name matched a same-named function in an
//! unrelated file because nothing consulted the file's imports. Callee lists —
//! the input to every reverse-caller and blast-radius answer — were therefore
//! full of edges the code does not contain.
//!
//! This module replaces that with structural resolution. [`CalleeRef`]s come
//! from the AST (see `visitor::CalleeCollector`), so text can never produce
//! one, and each is resolved against the importing file's own scope:
//!
//! - `foo(...)` — a definition in the SAME file wins; otherwise the file's
//!   import bindings are followed to the defining module (through barrels, via
//!   [`BindingResolver`]) and matched against that module's definitions.
//!   Nothing else. A name that resolves to neither is a global, a builtin or a
//!   package function, and produces no edge.
//! - `this.foo(...)` — the enclosing class's `Class.foo` key, in the same
//!   file. A same-named method on another class cannot match.
//! - `obj.foo(...)` — `obj` as a class in the same file (`Class.staticFn()`),
//!   an imported class, or a namespace import (`import * as ns`). Anything
//!   else produces no edge.
//!
//! Resolution is keyed on **(file, definition key)** throughout, taken from the
//! per-file extractor output rather than from the merged function map. The
//! merged map is keyed by definition name alone, so two same-named functions
//! in different files collapse onto one row (#582); resolving against it would
//! make a correct call edge depend on which file happened to be walked last.

use crate::agents::file_orchestrator::FileOrchestrator;
use crate::import_bindings::{BindingResolver, ResolvedBinding};
use crate::visitor::{
    CalleeRef, CalleeShape, FunctionCallRef, FunctionDefinition, ImportedSymbol, SymbolKind,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The export name a default export is published under (mirrors
/// `import_bindings`; not a valid identifier, so it cannot collide).
const DEFAULT_EXPORT: &str = "default";

/// Everything one source file contributes to call resolution.
///
/// Built per file during discovery, before the per-file definition maps are
/// merged, so same-named definitions in different files are still distinct.
#[derive(Debug, Default)]
pub struct FileCallIndex {
    /// The file as walked (NOT canonicalized): what `FunctionDefinition`s are
    /// stamped with and what call edges must report, so path relativization at
    /// the cloud boundary still strips the repo root.
    pub path: PathBuf,
    /// Definition key (`foo`, `Class.method`, `Class.static.method`) → the
    /// line it is defined on.
    pub definitions: HashMap<String, u32>,
    /// Definition key → the call sites inside its body.
    pub callees: HashMap<String, Vec<CalleeRef>>,
    /// Local binding → the import that introduced it.
    pub imports: HashMap<String, ImportedSymbol>,
}

/// Where a resolved call lands.
struct Target<'a> {
    file: &'a Path,
    key: String,
    line: u32,
}

/// Resolve every collected call site and write the results onto
/// `FunctionDefinition::calls`.
///
/// `per_file` is keyed by CANONICAL path: import specifiers resolve to
/// canonical paths, and a repo reached through a symlinked directory would
/// otherwise miss every cross-file lookup silently.
///
/// A definition whose merged row belongs to a different file (the #582
/// collapse) is skipped rather than given another file's edges.
pub fn resolve_call_edges(
    function_definitions: &mut HashMap<String, FunctionDefinition>,
    per_file: &HashMap<PathBuf, FileCallIndex>,
) {
    let mut resolver = CallResolver::new(per_file);

    // Sorted so a resolution cache built while walking one file cannot make a
    // later file's result depend on HashMap iteration order.
    let mut files: Vec<&PathBuf> = per_file.keys().collect();
    files.sort();

    for canonical in files {
        let index = &per_file[canonical];
        let mut callers: Vec<&String> = index.callees.keys().collect();
        callers.sort();

        for caller_key in callers {
            // Only definitions that survived the merge under this file's name
            // get edges; the rest have no row to hang them on.
            match function_definitions.get(caller_key) {
                Some(def) if def.file_path == index.path => {}
                _ => continue,
            }

            let mut edges: Vec<FunctionCallRef> = Vec::new();
            for callee in &index.callees[caller_key] {
                let Some(target) = resolver.resolve(index, callee) else {
                    continue;
                };
                // Direct recursion is not a dependency: it would make the
                // function its own topological predecessor.
                if target.file == index.path && target.key == *caller_key {
                    continue;
                }
                edges.push(FunctionCallRef {
                    name: target.key,
                    file_path: target.file.to_string_lossy().to_string(),
                    line_number: target.line,
                    call_site_line: callee.line,
                });
            }

            dedupe_edges(&mut edges);

            if let Some(def) = function_definitions.get_mut(caller_key) {
                def.calls = edges;
            }
        }
    }
}

/// One entry per called function, reporting its FIRST call site, in a stable
/// order. A function called three times is one edge, not three.
fn dedupe_edges(edges: &mut Vec<FunctionCallRef>) {
    edges.sort_by(|a, b| {
        (&a.file_path, &a.name, a.call_site_line).cmp(&(&b.file_path, &b.name, b.call_site_line))
    });
    edges.dedup_by(|a, b| a.file_path == b.file_path && a.name == b.name);
    edges.sort_by(|a, b| (&a.name, &a.file_path).cmp(&(&b.name, &b.file_path)));
}

/// Resolves call sites against the per-file index, caching import lookups.
struct CallResolver<'a> {
    per_file: &'a HashMap<PathBuf, FileCallIndex>,
    bindings: BindingResolver,
    /// (importer, local binding) → the module that declares it. Every function
    /// in a file that calls the same import would otherwise re-walk the barrel
    /// chain; `None` is cached too, so an unresolvable specifier is paid for
    /// once.
    resolved_imports: HashMap<(PathBuf, String), Option<ResolvedBinding>>,
}

impl<'a> CallResolver<'a> {
    fn new(per_file: &'a HashMap<PathBuf, FileCallIndex>) -> Self {
        Self {
            per_file,
            bindings: BindingResolver::new(),
            resolved_imports: HashMap::new(),
        }
    }

    fn resolve(&mut self, index: &'a FileCallIndex, callee: &CalleeRef) -> Option<Target<'a>> {
        match &callee.shape {
            CalleeShape::Bare => self.resolve_bare(index, &callee.name),
            // `this.x()` is the enclosing class's own member, so it can only
            // be defined in this file. A same-named method on another class
            // has a different key and never matches.
            CalleeShape::ThisMember(class) => {
                let key = format!("{class}.{}", callee.name);
                same_file(index, key)
            }
            CalleeShape::Member(object) => self.resolve_member(index, object, &callee.name),
        }
    }

    /// `foo(...)`: a same-file definition, else the module the file imports
    /// `foo` from, else nothing.
    fn resolve_bare(&mut self, index: &'a FileCallIndex, name: &str) -> Option<Target<'a>> {
        if let Some(target) = same_file(index, name.to_string()) {
            return Some(target);
        }

        let symbol = index.imports.get(name)?;
        let binding = self.resolve_import(&index.path, symbol)?;
        let target_index = self.per_file.get(&binding.file)?;

        // The module may declare the export under a different local name
        // (`function impl() {}; export { impl as helper }`), so try the name
        // the defining module used first, then the published names.
        let candidates = [
            binding.local_name.clone(),
            Some(symbol.imported_name.clone()),
            Some(name.to_string()),
        ];
        candidates
            .into_iter()
            .flatten()
            .find_map(|key| same_file(target_index, key))
    }

    /// `obj.foo(...)`: `obj` as a class in this file, an imported class, or a
    /// namespace import. An `obj` bound to a class INSTANCE is not resolved —
    /// that needs `new X()` tracking — and produces no edge rather than a
    /// guess.
    fn resolve_member(
        &mut self,
        index: &'a FileCallIndex,
        object: &str,
        name: &str,
    ) -> Option<Target<'a>> {
        // `Class.staticFn()` on a class declared in this file.
        if let Some(target) = member_keys(object, name)
            .into_iter()
            .find_map(|key| same_file(index, key))
        {
            return Some(target);
        }

        let symbol = index.imports.get(object)?;
        let binding = self.resolve_import(&index.path, symbol)?;
        let target_index = self.per_file.get(&binding.file)?;

        if matches!(symbol.kind, SymbolKind::Namespace) {
            // `import * as helpers` → `helpers.foo()` is the module's own
            // top-level `foo`.
            return same_file(target_index, name.to_string());
        }

        let class = binding
            .local_name
            .clone()
            .unwrap_or_else(|| symbol.imported_name.clone());
        member_keys(&class, name)
            .into_iter()
            .find_map(|key| same_file(target_index, key))
    }

    /// The module that declares `symbol` as imported by `importer`, or `None`
    /// for a package/alias specifier (out of scope: reaching those needs the
    /// sidecar's tsconfig knowledge) or an unresolvable export.
    fn resolve_import(
        &mut self,
        importer: &Path,
        symbol: &ImportedSymbol,
    ) -> Option<ResolvedBinding> {
        let cache_key = (importer.to_path_buf(), symbol.local_name.clone());
        if let Some(cached) = self.resolved_imports.get(&cache_key) {
            return cached.clone();
        }

        let resolved = FileOrchestrator::resolve_relative_import(importer, &symbol.source)
            .and_then(|target| match symbol.kind {
                // A namespace import names the module itself, not one export.
                SymbolKind::Namespace => Some(ResolvedBinding {
                    file: target,
                    local_name: None,
                }),
                SymbolKind::Default => self.bindings.resolve_export(&target, DEFAULT_EXPORT),
                SymbolKind::Named => self.bindings.resolve_export(&target, &symbol.imported_name),
            });

        self.resolved_imports.insert(cache_key, resolved.clone());
        resolved
    }
}

/// Exact-key lookup in one file's definitions. Keys are matched whole, so a
/// bare `foo()` can never reach a `Class.foo` method.
fn same_file(index: &FileCallIndex, key: String) -> Option<Target<'_>> {
    let line = *index.definitions.get(&key)?;
    Some(Target {
        file: &index.path,
        key,
        line,
    })
}

/// Both keys a `Class.member` can be stored under: the plain one, and the
/// `Class.static.member` form the extractor uses when a static and an instance
/// member share a name.
fn member_keys(class: &str, member: &str) -> [String; 2] {
    [
        format!("{class}.{member}"),
        format!("{class}.static.{member}"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_file;
    use crate::visitor::{FunctionDefinitionExtractor, ImportSymbolExtractor};
    use std::fs;
    use swc_common::sync::Lrc;
    use swc_common::{
        SourceMap,
        errors::{ColorConfig, Handler},
    };
    use swc_ecma_visit::VisitWith;
    use tempfile::TempDir;

    /// Parse each `(relative path, source)` pair into the same shape discovery
    /// builds: the merged definition map plus the per-file call index.
    fn scan(files: &[(&str, &str)]) -> (TempDir, HashMap<String, FunctionDefinition>) {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical tempdir");
        let mut paths = Vec::new();
        for (name, source) in files {
            let path = root.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("mkdir");
            }
            fs::write(&path, source).expect("write");
            paths.push(path);
        }

        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
        let mut definitions: HashMap<String, FunctionDefinition> = HashMap::new();
        let mut per_file: HashMap<PathBuf, FileCallIndex> = HashMap::new();

        for path in &paths {
            let module = parse_file(path, &cm, &handler).expect("parse");

            let mut imports = ImportSymbolExtractor::new();
            module.visit_with(&mut imports);

            let mut functions = FunctionDefinitionExtractor::new(path.clone(), cm.clone());
            module.visit_with(&mut functions);
            functions.finalize_exports();

            let index = FileCallIndex {
                path: path.clone(),
                definitions: functions
                    .function_definitions
                    .iter()
                    .map(|(key, def)| (key.clone(), def.line_number))
                    .collect(),
                callees: functions.callee_refs,
                imports: imports.imported_symbols,
            };
            per_file.insert(path.clone(), index);
            definitions.extend(functions.function_definitions);
        }

        resolve_call_edges(&mut definitions, &per_file);
        (dir, definitions)
    }

    fn callee_names(defs: &HashMap<String, FunctionDefinition>, caller: &str) -> Vec<String> {
        defs.get(caller)
            .unwrap_or_else(|| panic!("no definition for {caller}"))
            .calls
            .iter()
            .map(|c| c.name.clone())
            .collect()
    }

    /// Two files define `helper`; a third imports ONE of them and calls it.
    /// Exactly one edge, pointing at the imported file — never at whichever
    /// same-named definition happened to win the merged map.
    #[test]
    fn bare_call_follows_imports_not_names() {
        let (dir, defs) = scan(&[
            (
                "alpha.ts",
                "export function helper(n: number) {\n  return n + 1;\n}\n",
            ),
            (
                "beta.ts",
                "export function helper(n: number) {\n  return n - 1;\n}\n",
            ),
            (
                "caller.ts",
                "import { helper } from \"./alpha\";\n\
                 export function useHelper(n: number) {\n  const doubled = n * 2;\n  return helper(doubled);\n}\n",
            ),
        ]);

        let root = dir.path().canonicalize().unwrap();
        // The merged map is keyed by name, so the later file's `helper` is the
        // row it holds (#582). The edge must still point at the imported one.
        assert_eq!(defs["helper"].file_path, root.join("beta.ts"));

        let calls = &defs["useHelper"].calls;
        assert_eq!(calls.len(), 1, "exactly one edge, got {calls:?}");
        assert_eq!(calls[0].name, "helper");
        assert_eq!(
            PathBuf::from(&calls[0].file_path),
            root.join("alpha.ts"),
            "the edge must point at the IMPORTED helper"
        );
    }

    /// A bare name that is defined nowhere the file can see — a different
    /// file's function it does not import — is not an edge.
    #[test]
    fn bare_call_without_an_import_is_not_an_edge() {
        let (_dir, defs) = scan(&[
            (
                "lib.ts",
                "export function helper(n: number) {\n  return n + 1;\n}\n",
            ),
            (
                "caller.ts",
                "export function useHelper(n: number) {\n  const doubled = n * 2;\n  return helper(doubled);\n}\n",
            ),
        ]);

        assert!(
            callee_names(&defs, "useHelper").is_empty(),
            "a repo-wide name match must not produce an edge"
        );
    }

    /// A same-file definition wins without needing an import.
    #[test]
    fn bare_call_resolves_within_the_file() {
        let (_dir, defs) = scan(&[(
            "app.ts",
            "function helper(n: number) {\n  return n + 1;\n}\n\
             export function useHelper(n: number) {\n  const doubled = n * 2;\n  return helper(doubled);\n}\n",
        )]);

        assert_eq!(callee_names(&defs, "useHelper"), vec!["helper".to_string()]);
    }

    /// Globals and builtins resolve to nothing.
    #[test]
    fn unresolvable_identifiers_produce_no_edge() {
        let (_dir, defs) = scan(&[(
            "app.ts",
            "export function readConfig() {\n  const raw = JSON.parse(\"{}\");\n  setTimeout(() => {}, 10);\n  return structuredClone(raw);\n}\n",
        )]);

        assert!(callee_names(&defs, "readConfig").is_empty());
    }

    /// A name that only ever appears inside a string or template literal is
    /// not a call. This is the `formatOrientation` shape from #581: a body
    /// that slices a string and returns a template naming three other
    /// functions.
    #[test]
    fn string_and_template_mentions_are_never_edges() {
        let (_dir, defs) = scan(&[(
            "format.ts",
            "export function ask() {\n  return \"asked\";\n}\n\
             export function skeleton() {\n  return \"bones\";\n}\n\
             export function $() {\n  return \"dollar\";\n}\n\
             export function formatOrientation(text: string) {\n\
             \x20 const head = text.slice(0, 10);\n\
             \x20 // ask() and skeleton() are named here in a comment too\n\
             \x20 return `run ask then skeleton, then $ ${head}`;\n}\n",
        )]);

        assert!(
            callee_names(&defs, "formatOrientation").is_empty(),
            "string, template and comment mentions must produce no callees, got {:?}",
            callee_names(&defs, "formatOrientation")
        );
    }

    /// `this.method()` binds to the enclosing class. A same-named method on a
    /// different class in the same file is not a candidate.
    #[test]
    fn this_calls_bind_to_the_enclosing_class() {
        let (_dir, defs) = scan(&[(
            "controllers.ts",
            "export class OrderController {\n\
             \x20 create(payload: string) {\n    const trimmed = payload.trim();\n    return this.persist(trimmed);\n  }\n\
             \x20 persist(value: string) {\n    const stamped = value + \"!\";\n    return stamped;\n  }\n}\n\
             export class UserController {\n\
             \x20 persist(value: string) {\n    const upper = value.toUpperCase();\n    return upper;\n  }\n}\n",
        )]);

        assert_eq!(
            callee_names(&defs, "OrderController.create"),
            vec!["OrderController.persist".to_string()],
            "this.persist() must not reach UserController.persist"
        );
    }

    /// A private method call keeps its `#` in the key.
    #[test]
    fn this_calls_resolve_private_methods() {
        let (_dir, defs) = scan(&[(
            "job.ts",
            "export class Job {\n\
             \x20 async run() {\n    const started = Date.now();\n    await this.#reload();\n    return started;\n  }\n\
             \x20 async #reload() {\n    const now = Date.now();\n    return now;\n  }\n}\n",
        )]);

        assert_eq!(
            callee_names(&defs, "Job.run"),
            vec!["Job.#reload".to_string()]
        );
    }

    /// `Class.staticMethod()` on a class declared in the same file resolves —
    /// the shape the deleted text matcher covered via "class evidence".
    #[test]
    fn static_calls_through_the_class_name_resolve() {
        let (_dir, defs) = scan(&[(
            "presenter.ts",
            "export class Presenter {\n\
             \x20 static isFinished(status: string) {\n    const done = status === \"done\";\n    return done;\n  }\n}\n\
             export function serialiseRun(status: string) {\n  const label = status.trim();\n  return Presenter.isFinished(label);\n}\n",
        )]);

        assert_eq!(
            callee_names(&defs, "serialiseRun"),
            vec!["Presenter.isFinished".to_string()]
        );
    }

    /// A namespace import calls the module's own top-level functions.
    #[test]
    fn namespace_imports_resolve_through_the_module() {
        let (_dir, defs) = scan(&[
            (
                "helpers.ts",
                "export function normalise(value: string) {\n  return value.trim();\n}\n",
            ),
            (
                "caller.ts",
                "import * as helpers from \"./helpers\";\n\
                 export function run(value: string) {\n  const raw = value + \"\";\n  return helpers.normalise(raw);\n}\n",
            ),
        ]);

        assert_eq!(callee_names(&defs, "run"), vec!["normalise".to_string()]);
    }

    /// A barrel re-export is followed to the module that declares the binding.
    #[test]
    fn imports_are_followed_through_barrels() {
        let (dir, defs) = scan(&[
            (
                "lib/impl.ts",
                "export function compute(n: number) {\n  return n * 3;\n}\n",
            ),
            ("lib/index.ts", "export { compute } from \"./impl\";\n"),
            (
                "caller.ts",
                "import { compute } from \"./lib\";\n\
                 export function run(n: number) {\n  const doubled = n * 2;\n  return compute(doubled);\n}\n",
            ),
        ]);

        let calls = &defs["run"].calls;
        assert_eq!(calls.len(), 1);
        assert_eq!(
            PathBuf::from(&calls[0].file_path),
            dir.path().canonicalize().unwrap().join("lib/impl.ts"),
        );
    }

    /// `line_number` locates the callee's definition; `call_site_line` locates
    /// the call in the caller's file.
    #[test]
    fn call_site_line_is_the_callers_line() {
        let (_dir, defs) = scan(&[(
            "app.ts",
            // 1: blank marker comment
            "// header\n\
             function helper(n: number) {\n  return n + 1;\n}\n\
             export function useHelper(n: number) {\n  const doubled = n * 2;\n  return helper(doubled);\n}\n",
        )]);

        let call = &defs["useHelper"].calls[0];
        assert_eq!(call.line_number, 2, "callee is defined on line 2");
        assert_eq!(call.call_site_line, 7, "the call is written on line 7");
    }

    /// A function called several times is one edge, reported at its first
    /// call site.
    #[test]
    fn repeated_calls_collapse_to_one_edge() {
        let (_dir, defs) = scan(&[(
            "app.ts",
            "function helper(n: number) {\n  return n + 1;\n}\n\
             export function useHelper(n: number) {\n  const a = helper(n);\n  const b = helper(a);\n  return a + b;\n}\n",
        )]);

        let calls = &defs["useHelper"].calls;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].call_site_line, 5);
    }

    /// Optional calls (`obj?.method()`) are real calls.
    #[test]
    fn optional_calls_are_collected() {
        let (_dir, defs) = scan(&[(
            "app.ts",
            "export class Runner {\n\
             \x20 start(flag: boolean) {\n    const ready = flag === true;\n    return this.finish?.(ready);\n  }\n\
             \x20 finish(ready: boolean) {\n    const done = ready;\n    return done;\n  }\n}\n",
        )]);

        assert_eq!(
            callee_names(&defs, "Runner.start"),
            vec!["Runner.finish".to_string()]
        );
    }

    /// Direct recursion is not a dependency edge.
    #[test]
    fn self_recursion_is_not_an_edge() {
        let (_dir, defs) = scan(&[(
            "app.ts",
            "export function countdown(n: number): number {\n  if (n <= 0) {\n    return 0;\n  }\n  return countdown(n - 1);\n}\n",
        )]);

        assert!(callee_names(&defs, "countdown").is_empty());
    }
}
