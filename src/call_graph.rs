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
//! - `this.field.foo(...)` — the receiver is a field of the enclosing class,
//!   and the class body declares which class the field is
//!   ([`crate::receiver_type::class_field_types`], carrick#782). The field's
//!   class identifier is then resolved exactly as one named directly at the
//!   call site is. An unannotated field declares nothing and produces no edge.
//! - `obj.foo(...)` — `obj` as a class in the same file (`Class.staticFn()`),
//!   an imported class, a namespace import (`import * as ns`), a named
//!   import of a namespace RE-export (`export * as ns from "./m"` in the
//!   module it comes from, carrick#679), or a receiver the file DECLARES the
//!   class of — `client: ApiClient`, `const client = new ApiClient()`
//!   ([`crate::receiver_type`], carrick#776). Anything else produces no edge.
//!
//! An import is followed through a relative specifier and through a WORKSPACE
//! PACKAGE one (`@scope/core/v3`), because in a monorepo the class a package
//! publishes is the one its siblings actually call and a relative-only walk
//! cannot reach it (carrick#776). The package's own manifest says which file
//! the specifier names ([`crate::workspace_resolver`]); an external
//! dependency has no source in this repo and still produces no edge. A target
//! outside the service being scanned is read on demand, since only this
//! service's files are in the per-file index — and the edge it produces
//! carries the sibling's file, which is exactly what the cross-service join
//! needs to invert it.
//!
//! Resolution is keyed on **(file, definition key)** throughout, taken from the
//! per-file extractor output rather than from the merged function map, so a
//! correct call edge never depends on which file happened to be walked last.
//!
//! The same per-file output is what [`merge_definitions`] merges into the one
//! map the rest of the scan reads. That merge is collision-aware (#582): two
//! files defining the same key each keep a row, instead of the second silently
//! overwriting the first.

use crate::agents::file_orchestrator::FileOrchestrator;
use crate::import_bindings::{BindingResolver, ResolvedBinding};
use crate::parser::parse_file;
use crate::receiver_type::ReceiverTypes;
use crate::visitor::{
    CalleeRef, CalleeShape, FunctionCallRef, FunctionDefinition, FunctionDefinitionExtractor,
    ImportedSymbol, SymbolKind,
};
use crate::workspace_resolver::{Resolution, WorkspaceIndex};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use swc_common::{
    SourceMap,
    errors::{ColorConfig, Handler},
    sync::Lrc,
};
use swc_ecma_visit::VisitWith;
use tracing::debug;

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
    /// Definition key → the classes that definition declares its own local
    /// bindings to be, for the receivers that are instances rather than
    /// classes (carrick#776). Scoped per definition, like `callees`, because a
    /// name means different things in different functions.
    pub declared_types: HashMap<String, ReceiverTypes>,
    /// Class name → the classes that class declares its own FIELDS to be, for
    /// a `this.field.foo()` receiver (carrick#782). Keyed by class, not by
    /// definition: a field belongs to the class and every method sees it.
    pub field_types: HashMap<String, ReceiverTypes>,
}

/// Where a resolved call lands.
///
/// Owns its path rather than borrowing the per-file index: a target may sit in
/// a workspace sibling that is not in the index at all and was parsed on
/// demand to answer this one call.
struct Target {
    file: PathBuf,
    key: String,
    line: u32,
}

/// The merged definition map, plus the table that translates a
/// (definition key, defining file) pair into the key the map actually holds.
pub struct MergedDefinitions {
    pub definitions: HashMap<String, FunctionDefinition>,
    pub keys: RekeyIndex,
}

/// Merged-map keys for the definition keys that more than one file defines.
///
/// Empty on almost every scan: an entry appears only when two files define the
/// same `foo` or `Class.member`.
#[derive(Debug, Default)]
pub struct RekeyIndex {
    by_key: HashMap<String, HashMap<PathBuf, String>>,
}

impl RekeyIndex {
    /// The key the merged map holds for `key` as defined in `file`. Returns
    /// `key` unchanged when nothing collided with it, which is the case for
    /// every definition on a repo with no same-named functions.
    pub fn merged_key<'a>(&'a self, key: &'a str, file: &Path) -> &'a str {
        self.by_key
            .get(key)
            .and_then(|by_file| by_file.get(file))
            .map(String::as_str)
            .unwrap_or(key)
    }
}

/// The separator between a colliding definition key and the file that
/// disambiguates it. `@` cannot occur in a definition key (identifiers and the
/// `.` that joins a class to its member), so `<key>@<path>` is injective and a
/// plain key can never be mistaken for a re-keyed one.
const FILE_QUALIFIER: char = '@';

/// Merge the per-file definition maps into the single map the rest of the scan
/// reads, giving every same-named definition its own row.
///
/// The map used to be a plain `extend` per file, keyed by definition key alone
/// (`foo`, `Class.member`). Two files defining `foo` — or two `MFAController`
/// classes in a controller-per-resource layout — collapsed onto one row, last
/// writer wins, and the loser's methods vanished from the index along with
/// their call edges (#582).
///
/// A key claimed by more than one file is re-keyed PER FILE as
/// `<key>@<repo-relative path>`, the incumbent included, so neither row wins by
/// walk order. A key claimed by one file — nearly every key, on nearly every
/// repo — is stored byte-identically to before, so the intent cache, the
/// embedding sidecar and the cloud rows keyed by it do not churn.
///
/// `FunctionDefinition::name` deliberately keeps the PLAIN key even when the
/// row is re-keyed. That field is what the index displays, filters and embeds,
/// and `get_callers` matches a qualified name only when it matches whole — so
/// a path-bearing name would make the row unfindable by the name it has in the
/// source. Two rows sharing a name are told apart there by `file_path`, which
/// those tools already compare. The same holds for `FunctionCallRef`: a callee
/// ref is a `(name, file_path)` locator, never a merged-map key. Use
/// [`RekeyIndex::merged_key`] to go from a locator to a key.
pub fn merge_definitions(
    per_file: Vec<(PathBuf, HashMap<String, FunctionDefinition>)>,
    repo_root: &str,
) -> MergedDefinitions {
    // Sorted so the merged map, and the log line below, never depend on the
    // order files came off the walker.
    let mut per_file = per_file;
    per_file.sort_by(|a, b| a.0.cmp(&b.0));

    let mut owners: HashMap<&str, Vec<&Path>> = HashMap::new();
    for (path, definitions) in &per_file {
        for key in definitions.keys() {
            owners.entry(key.as_str()).or_default().push(path.as_path());
        }
    }

    let mut colliding: HashMap<String, Vec<PathBuf>> = owners
        .into_iter()
        .filter(|(_, files)| files.len() > 1)
        .map(|(key, files)| {
            (
                key.to_string(),
                files.into_iter().map(Path::to_path_buf).collect(),
            )
        })
        .collect();

    let mut keys = RekeyIndex::default();
    let mut definitions: HashMap<String, FunctionDefinition> = HashMap::new();
    for (path, file_definitions) in per_file {
        let relative = crate::engine::repo_relative(&path.to_string_lossy(), repo_root);
        for (key, definition) in file_definitions {
            if colliding.contains_key(&key) {
                let merged = format!("{key}{FILE_QUALIFIER}{relative}");
                keys.by_key
                    .entry(key)
                    .or_default()
                    .insert(path.clone(), merged.clone());
                definitions.insert(merged, definition);
            } else {
                definitions.insert(key, definition);
            }
        }
    }

    if !colliding.is_empty() {
        let mut collided: Vec<(String, Vec<PathBuf>)> = colliding.drain().collect();
        collided.sort_by(|a, b| a.0.cmp(&b.0));
        let mut rekeyed = 0usize;
        for (key, files) in &collided {
            rekeyed += files.len();
            debug!(
                "Definition key '{}' is defined in {} files: {}",
                key,
                files.len(),
                files
                    .iter()
                    .map(|f| f.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        debug!(
            "re-keyed {} colliding definitions across {} definition keys",
            rekeyed,
            collided.len()
        );
    }

    MergedDefinitions { definitions, keys }
}

/// Resolve every collected call site and write the results onto
/// `FunctionDefinition::calls`.
///
/// `per_file` is keyed by CANONICAL path: import specifiers resolve to
/// canonical paths, and a repo reached through a symlinked directory would
/// otherwise miss every cross-file lookup silently.
///
/// `keys` comes from [`merge_definitions`]: a caller whose definition key
/// collided with another file's is stored under a re-keyed row, and looking it
/// up under its plain key would find nothing and silently drop its edges.
///
/// `workspace` is the repo's manifest index. `per_file` holds only the service
/// being scanned, so a call into a sibling workspace package would otherwise
/// resolve to nothing however plainly the source names it (carrick#776); the
/// index says which file the package specifier means, and the target is parsed
/// on demand.
pub fn resolve_call_edges(
    function_definitions: &mut HashMap<String, FunctionDefinition>,
    per_file: &HashMap<PathBuf, FileCallIndex>,
    keys: &RekeyIndex,
    workspace: &WorkspaceIndex,
    repo_root: &Path,
) {
    let mut resolver = CallResolver::new(per_file, workspace, repo_root);

    // Sorted so a resolution cache built while walking one file cannot make a
    // later file's result depend on HashMap iteration order.
    let mut files: Vec<&PathBuf> = per_file.keys().collect();
    files.sort();

    for canonical in files {
        let index = &per_file[canonical];
        let mut callers: Vec<&String> = index.callees.keys().collect();
        callers.sort();

        for caller_key in callers {
            // The row this file's definition was merged under — its plain key,
            // or the re-keyed one when another file defines the same key. The
            // file check is the invariant that makes the translation right, not
            // a filter: a row reached this way is always this file's.
            let caller_row = keys.merged_key(caller_key, &index.path);
            match function_definitions.get(caller_row) {
                Some(def) if def.file_path == index.path => {}
                _ => continue,
            }

            let mut edges: Vec<FunctionCallRef> = Vec::new();
            for callee in &index.callees[caller_key] {
                let Some(target) = resolver.resolve(index, caller_key, callee) else {
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

            if let Some(def) = function_definitions.get_mut(caller_row) {
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
    workspace: &'a WorkspaceIndex,
    repo_root: PathBuf,
    bindings: BindingResolver,
    /// (importer, local binding) → the module that declares it. Every function
    /// in a file that calls the same import would otherwise re-walk the barrel
    /// chain; `None` is cached too, so an unresolvable specifier is paid for
    /// once.
    resolved_imports: HashMap<(PathBuf, String), Option<ResolvedBinding>>,
    /// Definition tables for files OUTSIDE the scanned service, parsed the
    /// first time an edge reaches one. `None` for a file that does not parse,
    /// so a broken sibling costs one parse rather than one per call site.
    external: HashMap<PathBuf, Option<HashMap<String, u32>>>,
    /// Quiet diagnostics for those parses: a sibling that fails to parse is a
    /// resolution miss, not something to shout about mid-scan.
    source_map: Lrc<SourceMap>,
    handler: Handler,
}

impl<'a> CallResolver<'a> {
    fn new(
        per_file: &'a HashMap<PathBuf, FileCallIndex>,
        workspace: &'a WorkspaceIndex,
        repo_root: &Path,
    ) -> Self {
        let source_map: Lrc<SourceMap> = Default::default();
        let handler =
            Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(source_map.clone()));
        Self {
            per_file,
            workspace,
            repo_root: repo_root.to_path_buf(),
            bindings: BindingResolver::new(),
            resolved_imports: HashMap::new(),
            external: HashMap::new(),
            source_map,
            handler,
        }
    }

    /// The first key that names a definition in `file`, whether the file is in
    /// the scanned service or a workspace sibling read on demand.
    fn lookup_in_file(
        &mut self,
        file: &Path,
        keys: impl IntoIterator<Item = String>,
    ) -> Option<Target> {
        if let Some(index) = self.per_file.get(file) {
            let path = index.path.clone();
            return keys
                .into_iter()
                .find_map(|key| index.definitions.get(&key).map(|line| (key, *line)))
                .map(|(key, line)| Target {
                    file: path,
                    key,
                    line,
                });
        }
        if !self.external.contains_key(file) {
            let definitions = parse_file(file, &self.source_map, &self.handler).map(|module| {
                let mut extractor =
                    FunctionDefinitionExtractor::new(file.to_path_buf(), self.source_map.clone());
                module.visit_with(&mut extractor);
                extractor.finalize_exports();
                extractor
                    .function_definitions
                    .iter()
                    .map(|(key, def)| (key.clone(), def.line_number))
                    .collect::<HashMap<String, u32>>()
            });
            self.external.insert(file.to_path_buf(), definitions);
        }
        let definitions = self.external.get(file)?.as_ref()?;
        keys.into_iter()
            .find_map(|key| definitions.get(&key).map(|line| (key, *line)))
            .map(|(key, line)| Target {
                file: file.to_path_buf(),
                key,
                line,
            })
    }

    fn resolve(
        &mut self,
        index: &'a FileCallIndex,
        caller_key: &str,
        callee: &CalleeRef,
    ) -> Option<Target> {
        match &callee.shape {
            CalleeShape::Bare => self.resolve_bare(index, &callee.name),
            // `this.x()` is the enclosing class's own member, so it can only
            // be defined in this file. A same-named method on another class
            // has a different key and never matches.
            CalleeShape::ThisMember(class) => {
                let key = format!("{class}.{}", callee.name);
                same_file(index, key)
            }
            // `this.field.x()`: the class body says what the field is, and
            // that class identifier resolves like any other (carrick#782).
            CalleeShape::ThisFieldMember { class, field } => {
                let declared = index.field_types.get(class)?.get(field)?.clone();
                self.resolve_class_member(index, &declared, &callee.name)
            }
            CalleeShape::Member(object) => {
                self.resolve_member(index, caller_key, object, &callee.name)
            }
        }
    }

    /// `foo(...)`: a same-file definition, else the module the file imports
    /// `foo` from, else nothing.
    fn resolve_bare(&mut self, index: &'a FileCallIndex, name: &str) -> Option<Target> {
        if let Some(target) = same_file(index, name.to_string()) {
            return Some(target);
        }

        let symbol = index.imports.get(name)?.clone();
        let binding = self.resolve_import(&index.path, &symbol)?;

        // The module may declare the export under a different local name
        // (`function impl() {}; export { impl as helper }`), so try the name
        // the defining module used first, then the published names.
        let candidates: Vec<String> = [
            binding.local_name.clone(),
            Some(symbol.imported_name.clone()),
            Some(name.to_string()),
        ]
        .into_iter()
        .flatten()
        .collect();
        self.lookup_in_file(&binding.file, candidates)
    }

    /// `obj.foo(...)`: `obj` as a class in this file, an imported class, a
    /// namespace import, or a receiver whose class the file declares.
    ///
    /// The three receiver forms are tried in the order the file states them
    /// most directly. A receiver the file says nothing about — a destructured
    /// binding, the result of an untyped call — still produces no edge rather
    /// than a guess.
    fn resolve_member(
        &mut self,
        index: &'a FileCallIndex,
        caller_key: &str,
        object: &str,
        name: &str,
    ) -> Option<Target> {
        // `obj` names the class itself: `Class.staticFn()`, or an imported
        // class or namespace.
        if let Some(target) = self.resolve_class_member(index, object, name) {
            return Some(target);
        }

        // `obj` is an INSTANCE whose class the file declares — a typed
        // parameter, a `new X()` local. The class name is then resolved
        // exactly as a class named directly at the call site would be
        // (carrick#776).
        let declared = index.declared_types.get(caller_key)?.get(object)?.clone();
        if declared == object {
            return None;
        }
        self.resolve_class_member(index, &declared, name)
    }

    /// `class.foo(...)` where `class` is a binding that NAMES a class or a
    /// module: declared here, imported, or a namespace.
    fn resolve_class_member(
        &mut self,
        index: &'a FileCallIndex,
        class: &str,
        name: &str,
    ) -> Option<Target> {
        // `Class.staticFn()` on a class declared in this file.
        if let Some(target) = member_keys(class, name)
            .into_iter()
            .find_map(|key| same_file(index, key))
        {
            return Some(target);
        }

        let symbol = index.imports.get(class)?.clone();

        // `import { queues } from "./index.js"` where the entry writes
        // `export * as queues from "./queues.js"`: the name binds a MODULE one
        // hop away, so `queues.list()` is that module's own `list`. Tried
        // first because the value walk below answers `None` for such a name
        // and caches that answer (carrick#679).
        if let Some(target) = self.resolve_namespace_member(&index.path, &symbol, name) {
            return Some(target);
        }

        let binding = self.resolve_import(&index.path, &symbol)?;

        if matches!(symbol.kind, SymbolKind::Namespace) {
            // `import * as helpers` → `helpers.foo()` is the module's own
            // top-level `foo`.
            return self.lookup_in_file(&binding.file, [name.to_string()]);
        }

        let declaring_class = binding
            .local_name
            .clone()
            .unwrap_or_else(|| symbol.imported_name.clone());
        self.lookup_in_file(&binding.file, member_keys(&declaring_class, name))
    }

    /// `ns.foo(...)` where `ns` is a NAMED import of a namespace re-export
    /// (`export * as ns from "./m"` in the module it comes from).
    ///
    /// The importing file names neither `./m` nor `foo`, so nothing else here
    /// can reach the definition: the receiver is resolved to the module the
    /// namespace stands for, and `foo` is then followed out of that module the
    /// way any export is — a group is commonly a barrel, so the function is
    /// often declared one hop further on.
    fn resolve_namespace_member(
        &mut self,
        importer: &Path,
        symbol: &ImportedSymbol,
        name: &str,
    ) -> Option<Target> {
        if !matches!(symbol.kind, SymbolKind::Named) {
            return None;
        }
        let target = FileOrchestrator::resolve_relative_import(importer, &symbol.source)?;
        let module = self
            .bindings
            .resolve_namespace_export(&target, &symbol.imported_name)?;
        let binding = self.bindings.resolve_export(&module, name)?;
        // The declaring module may name it differently (`function impl() {};
        // export { impl as list }`).
        let candidates: Vec<String> = binding
            .local_name
            .clone()
            .into_iter()
            .chain([name.to_string()])
            .collect();
        self.lookup_in_file(&binding.file, candidates)
    }

    /// The module that declares `symbol` as imported by `importer`, or `None`
    /// for an external package, a tsconfig path alias (out of scope: reaching
    /// those needs the sidecar's tsconfig knowledge) or an unresolvable
    /// export.
    ///
    /// A WORKSPACE package specifier is not out of scope: the repo's own
    /// manifests say which file `@scope/core/v3` names, and the file is in
    /// this checkout (carrick#776). It is tried only after the relative
    /// resolver declines, so a relative specifier keeps the answer it always
    /// had.
    fn resolve_import(
        &mut self,
        importer: &Path,
        symbol: &ImportedSymbol,
    ) -> Option<ResolvedBinding> {
        let cache_key = (importer.to_path_buf(), symbol.local_name.clone());
        if let Some(cached) = self.resolved_imports.get(&cache_key) {
            return cached.clone();
        }

        let resolved = self
            .resolve_specifier(importer, &symbol.source)
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

    /// The file a specifier names: relative first, then the workspace package
    /// the repo's manifests declare.
    fn resolve_specifier(&self, importer: &Path, specifier: &str) -> Option<PathBuf> {
        if let Some(target) = FileOrchestrator::resolve_relative_import(importer, specifier) {
            return Some(target);
        }
        let Resolution::Internal(relative) = self.workspace.resolve(importer, specifier) else {
            return None;
        };
        let target = self.repo_root.join(relative);
        // Canonical when that stays inside the repo — it is what `per_file` is
        // keyed by, so an in-service target is found rather than re-parsed.
        // A package directory that is itself a symlink out of the tree
        // canonicalizes to a path the cloud boundary cannot strip the repo root
        // from, and an absolute path in the blob is a locator nothing can
        // invert; the plain join is under the root by construction.
        match target.canonicalize() {
            Ok(canonical) if canonical.starts_with(&self.repo_root) => Some(canonical),
            _ => target.is_file().then_some(target),
        }
    }
}

/// Exact-key lookup in one file's definitions. Keys are matched whole, so a
/// bare `foo()` can never reach a `Class.foo` method.
fn same_file(index: &FileCallIndex, key: String) -> Option<Target> {
    let line = *index.definitions.get(&key)?;
    Some(Target {
        file: index.path.clone(),
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
        scan_service(files, "")
    }

    /// The same, with the scan scoped to ONE service: only files whose
    /// relative path starts with `scope` are indexed, exactly as a multi-service
    /// `carrick.json` scopes discovery. Everything else is written to disk and
    /// reachable only the way production reaches it — through the repo's own
    /// manifests.
    fn scan_service(
        files: &[(&str, &str)],
        scope: &str,
    ) -> (TempDir, HashMap<String, FunctionDefinition>) {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical tempdir");
        let mut paths = Vec::new();
        for (name, source) in files {
            let path = root.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("mkdir");
            }
            fs::write(&path, source).expect("write");
            if name.starts_with(scope) && !name.ends_with(".json") {
                paths.push(path);
            }
        }

        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
        let mut per_file_definitions: Vec<(PathBuf, HashMap<String, FunctionDefinition>)> =
            Vec::new();
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
                declared_types: functions.declared_types,
                field_types: functions.field_types,
            };
            per_file.insert(path.clone(), index);
            per_file_definitions.push((path.clone(), functions.function_definitions));
        }

        // The same merge discovery runs, so a test can never pass against a
        // merge production does not perform.
        let MergedDefinitions {
            mut definitions,
            keys,
        } = merge_definitions(per_file_definitions, &root.to_string_lossy());

        let workspace = WorkspaceIndex::build(&root);
        resolve_call_edges(&mut definitions, &per_file, &keys, &workspace, &root);
        (dir, definitions)
    }

    /// The file where a caller's first edge lands, repo-relative-ish (the
    /// tempdir root is stripped by suffix matching at the assert).
    fn callee_files(defs: &HashMap<String, FunctionDefinition>, caller: &str) -> Vec<String> {
        defs.get(caller)
            .unwrap_or_else(|| panic!("no definition for {caller}"))
            .calls
            .iter()
            .map(|c| c.file_path.clone())
            .collect()
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
        // Two files define `helper`, so each keeps its own row (#582) and the
        // plain key is gone. The edge must point at the imported one.
        assert!(!defs.contains_key("helper"));
        assert_eq!(defs["helper@alpha.ts"].file_path, root.join("alpha.ts"));
        assert_eq!(defs["helper@beta.ts"].file_path, root.join("beta.ts"));

        let calls = &defs["useHelper"].calls;
        assert_eq!(calls.len(), 1, "exactly one edge, got {calls:?}");
        assert_eq!(calls[0].name, "helper");
        assert_eq!(
            PathBuf::from(&calls[0].file_path),
            root.join("alpha.ts"),
            "the edge must point at the IMPORTED helper"
        );
    }

    /// Two files, each with a class of the same name — the controller-per-
    /// resource layout #582 was found in. Both classes' methods keep a row of
    /// their own, with their own file and line, and each method's edges stay
    /// inside its own file. Before the collision-aware merge the index held one
    /// `MFAController.post`, and the caller inside the losing one was invisible
    /// to every reverse-caller answer.
    #[test]
    fn same_named_classes_in_two_files_both_keep_rows() {
        let (dir, defs) = scan(&[
            (
                "login/mfa.ts",
                "export function isValidRedirect(url: string) {\n                   return url.startsWith(\"/\");\n}\n                 export class MFAController {\n                   post(url: string) {\n    return isValidRedirect(url);\n  }\n}\n",
            ),
            (
                "register/mfa.ts",
                "export function isRegistered(user: string) {\n                   return user.length > 0;\n}\n\n                 export class MFAController {\n                   post(user: string) {\n    return isRegistered(user);\n  }\n}\n",
            ),
        ]);

        let root = dir.path().canonicalize().unwrap();
        assert!(
            !defs.contains_key("MFAController.post"),
            "a key two files claim is held per file, never under the bare key"
        );

        let login = &defs["MFAController.post@login/mfa.ts"];
        let register = &defs["MFAController.post@register/mfa.ts"];
        assert_eq!(login.file_path, root.join("login/mfa.ts"));
        assert_eq!(register.file_path, root.join("register/mfa.ts"));
        assert_eq!(login.line_number, 5);
        assert_eq!(register.line_number, 6, "each row carries its own line");
        assert_eq!(
            (login.name.as_str(), register.name.as_str()),
            ("MFAController.post", "MFAController.post"),
            "the NAME stays plain: it is what the index displays, filters and \
             embeds, and it is compared against `file_path` to tell the two apart"
        );

        assert_eq!(
            callee_names(&defs, "MFAController.post@login/mfa.ts"),
            vec!["isValidRedirect".to_string()],
            "a re-keyed row still gets its edges"
        );
        assert_eq!(
            callee_names(&defs, "MFAController.post@register/mfa.ts"),
            vec!["isRegistered".to_string()]
        );
        assert_eq!(
            PathBuf::from(&defs["MFAController.post@login/mfa.ts"].calls[0].file_path),
            root.join("login/mfa.ts"),
        );

        // Nothing else collided, so every other key is byte-identical to what
        // the old merge produced — the property that keeps cached intents,
        // embedding vectors and cloud rows from churning.
        assert!(defs.contains_key("isValidRedirect"));
        assert!(defs.contains_key("isRegistered"));
        assert_eq!(
            defs.keys().filter(|k| k.contains('@')).count(),
            2,
            "only the colliding key is re-keyed; got {:?}",
            defs.keys().collect::<Vec<_>>()
        );
    }

    /// The same collapse, with free functions rather than class members: two
    /// files defining `helper` are two functions, not one.
    #[test]
    fn same_named_free_functions_both_keep_rows() {
        let (dir, defs) = scan(&[
            (
                "a.ts",
                "export function helper(n: number) {\n  return n + 1;\n}\n",
            ),
            (
                "nested/b.ts",
                "export function helper(n: number) {\n  return n - 1;\n}\n",
            ),
        ]);

        let root = dir.path().canonicalize().unwrap();
        assert_eq!(defs.len(), 2, "got {:?}", defs.keys().collect::<Vec<_>>());
        assert_eq!(defs["helper@a.ts"].file_path, root.join("a.ts"));
        assert_eq!(
            defs["helper@nested/b.ts"].file_path,
            root.join("nested/b.ts"),
            "the qualifier is the repo-relative path, so it survives the \
             relativization the cloud payload goes through"
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

    /// carrick#776: the receiver is an INSTANCE, and the class it is an
    /// instance of is stated by the parameter's own type annotation. The class
    /// is published by a sibling WORKSPACE package under a manifest subpath,
    /// and that package is not in the scanned service's file list at all.
    #[test]
    fn a_typed_receiver_resolves_through_a_workspace_subpath() {
        let (dir, defs) = scan_service(
            &[
                (
                    "package.json",
                    r#"{"name":"root","private":true,"workspaces":["packages/*"]}"#,
                ),
                (
                    "packages/core/package.json",
                    r#"{"name":"@fixture/core","exports":{"./v2":{"import":{"@fixture/source":"./src/v2/index.ts","default":"./dist/v2/index.js"}}}}"#,
                ),
                (
                    "packages/core/src/v2/index.ts",
                    "export * from \"./client.js\";\n",
                ),
                (
                    "packages/core/src/v2/client.ts",
                    "export class RunClient {\n  subscribeToRun(id: string) {\n    return id;\n  }\n}\n",
                ),
                (
                    "packages/app/package.json",
                    r#"{"name":"@fixture/app","dependencies":{"@fixture/core":"workspace:*"}}"#,
                ),
                (
                    "packages/app/src/reader.ts",
                    "import type { RunClient } from \"@fixture/core/v2\";\n\
                     export function readRun(id: string, client: RunClient) {\n  \
                     return client.subscribeToRun(id);\n}\n",
                ),
            ],
            "packages/app",
        );

        assert_eq!(callee_names(&defs, "readRun"), ["RunClient.subscribeToRun"]);
        assert!(
            callee_files(&defs, "readRun")[0].ends_with("packages/core/src/v2/client.ts"),
            "the edge must point at the sibling package's source, not its build output: {:?}",
            callee_files(&defs, "readRun")
        );
        drop(dir);
    }

    /// A receiver constructed on the spot states its class as flatly as an
    /// annotation does.
    #[test]
    fn a_constructed_receiver_resolves_to_its_class() {
        let (dir, defs) = scan(&[
            (
                "client.ts",
                "export class RunClient {\n  fetchStream(id: string) {\n    return id;\n  }\n}\n",
            ),
            (
                "reader.ts",
                "import { RunClient } from \"./client\";\n\
                 export function readStream(id: string) {\n  \
                 const client = new RunClient();\n  \
                 return client.fetchStream(id);\n}\n",
            ),
        ]);

        assert_eq!(callee_names(&defs, "readStream"), ["RunClient.fetchStream"]);
        drop(dir);
    }

    /// A name the file states nothing about still resolves to nothing, and a
    /// name bound to two classes in one scope resolves to neither.
    #[test]
    fn an_undeclared_or_contested_receiver_resolves_to_nothing() {
        let (dir, defs) = scan(&[
            (
                "client.ts",
                "export class RunClient {\n  subscribeToRun(id: string) {\n    return id;\n  }\n}\n\
                 export class BatchClient {\n  subscribeToRun(id: string) {\n    return id;\n  }\n}\n",
            ),
            (
                "reader.ts",
                "import { RunClient, BatchClient } from \"./client\";\n\
                 export function readUnbound(id: string, client) {\n  \
                 return client.subscribeToRun(id);\n}\n\
                 export function readContested(id: string, client: RunClient) {\n  \
                 const inner = (client: BatchClient) => client.subscribeToRun(id);\n  \
                 return inner;\n}\n",
            ),
        ]);

        assert!(callee_names(&defs, "readUnbound").is_empty());
        assert!(callee_names(&defs, "readContested").is_empty());
        drop(dir);
    }

    /// An EXTERNAL package has no source in this repo, so a receiver typed by
    /// one produces no edge however plainly the file names the class.
    #[test]
    fn a_receiver_typed_by_an_external_package_resolves_to_nothing() {
        let (dir, defs) = scan_service(
            &[
                (
                    "package.json",
                    r#"{"name":"root","private":true,"workspaces":["packages/*"]}"#,
                ),
                (
                    "packages/app/package.json",
                    r#"{"name":"@fixture/app","dependencies":{"vendor-runs":"^1.0.0"}}"#,
                ),
                (
                    "packages/app/src/reader.ts",
                    "import type { VendorClient } from \"vendor-runs\";\n\
                     export function readRun(id: string, client: VendorClient) {\n  \
                     return client.subscribeToRun(id);\n}\n",
                ),
            ],
            "packages/app",
        );

        assert!(callee_names(&defs, "readRun").is_empty());
        drop(dir);
    }

    /// A bare call into a sibling workspace package resolves too: the gap was
    /// the specifier, not the call shape.
    #[test]
    fn a_bare_call_resolves_through_a_workspace_package_name() {
        let (dir, defs) = scan_service(
            &[
                (
                    "package.json",
                    r#"{"name":"root","private":true,"workspaces":["packages/*"]}"#,
                ),
                (
                    "packages/core/package.json",
                    r#"{"name":"@fixture/core","main":"./src/index.ts"}"#,
                ),
                (
                    "packages/core/src/index.ts",
                    "export function formatRun(id: string) {\n  return id;\n}\n",
                ),
                (
                    "packages/app/package.json",
                    r#"{"name":"@fixture/app","dependencies":{"@fixture/core":"workspace:*"}}"#,
                ),
                (
                    "packages/app/src/reader.ts",
                    "import { formatRun } from \"@fixture/core\";\n\
                     export function readRun(id: string) {\n  return formatRun(id);\n}\n",
                ),
            ],
            "packages/app",
        );

        assert_eq!(callee_names(&defs, "readRun"), ["formatRun"]);
        drop(dir);
    }

    /// `this.field.member()` inside a class: the class body declares the
    /// field, through a constructor parameter property, and the class it names
    /// is published by a sibling workspace package (carrick#782).
    #[test]
    fn a_this_field_receiver_resolves_through_the_class_body() {
        let (dir, defs) = scan_service(
            &[
                (
                    "package.json",
                    r#"{"name":"root","private":true,"workspaces":["packages/*"]}"#,
                ),
                (
                    "packages/core/package.json",
                    r#"{"name":"@fixture/core","exports":{"./v2":"./src/v2/client.ts"}}"#,
                ),
                (
                    "packages/core/src/v2/client.ts",
                    "export class RunClient {\n  fetchStream(id: string) {\n    return id;\n  }\n}\n",
                ),
                (
                    "packages/app/package.json",
                    r#"{"name":"@fixture/app","dependencies":{"@fixture/core":"workspace:*"}}"#,
                ),
                (
                    "packages/app/src/manager.ts",
                    "import type { RunClient } from \"@fixture/core/v2\";\n\
                     export class RunMetadataManager {\n  \
                     constructor(private readonly apiClient: RunClient) {}\n  \
                     read(id: string) {\n    \
                     return this.apiClient.fetchStream(id);\n  }\n}\n",
                ),
            ],
            "packages/app",
        );

        assert_eq!(
            callee_names(&defs, "RunMetadataManager.read"),
            ["RunClient.fetchStream"]
        );
        assert!(
            callee_files(&defs, "RunMetadataManager.read")[0]
                .ends_with("packages/core/src/v2/client.ts"),
            "the edge must point at the sibling package's source: {:?}",
            callee_files(&defs, "RunMetadataManager.read")
        );
        drop(dir);
    }

    /// An annotated property declares the field just as a parameter property
    /// does, and a private one is reached under its `#` key.
    #[test]
    fn an_annotated_property_and_a_private_one_both_resolve() {
        let (dir, defs) = scan(&[
            (
                "client.ts",
                "export class RunClient {\n  fetchStream(id: string) {\n    return id;\n  }\n}\n",
            ),
            (
                "manager.ts",
                "import type { RunClient } from \"./client\";\n\
                 export class Manager {\n  \
                 private client: RunClient;\n  \
                 #backup: RunClient;\n  \
                 read(id: string) {\n    \
                 return this.client.fetchStream(id);\n  }\n  \
                 readBackup(id: string) {\n    \
                 return this.#backup.fetchStream(id);\n  }\n}\n",
            ),
        ]);

        assert_eq!(
            callee_names(&defs, "Manager.read"),
            ["RunClient.fetchStream"]
        );
        assert_eq!(
            callee_names(&defs, "Manager.readBackup"),
            ["RunClient.fetchStream"]
        );
        drop(dir);
    }

    /// The negatives this shape must keep: a field the class body leaves
    /// unannotated states nothing however it is initialised, a chain one level
    /// deeper is not read at all, and a `this` inside a nested class is that
    /// class's `this`, not the outer one's.
    #[test]
    fn an_unannotated_field_and_a_deeper_chain_resolve_to_nothing() {
        let (dir, defs) = scan(&[
            (
                "client.ts",
                "export class RunClient {\n  fetchStream(id: string) {\n    return id;\n  }\n}\n",
            ),
            (
                "manager.ts",
                "import { RunClient } from \"./client\";\n\
                 export class Manager {\n  \
                 client = new RunClient();\n  \
                 deps: Deps;\n  \
                 readUnannotated(id: string) {\n    \
                 return this.client.fetchStream(id);\n  }\n  \
                 readDeeper(id: string) {\n    \
                 return this.deps.client.fetchStream(id);\n  }\n}\n",
            ),
        ]);

        assert!(callee_names(&defs, "Manager.readUnannotated").is_empty());
        assert!(callee_names(&defs, "Manager.readDeeper").is_empty());
        drop(dir);
    }

    /// A field of the OUTER class is not in scope for a class declared inside
    /// a method: `this` there is the inner class's.
    #[test]
    fn a_this_field_inside_a_nested_class_resolves_to_nothing() {
        let (dir, defs) = scan(&[
            (
                "client.ts",
                "export class RunClient {\n  fetchStream(id: string) {\n    return id;\n  }\n}\n",
            ),
            (
                "manager.ts",
                "import type { RunClient } from \"./client\";\n\
                 export class Manager {\n  \
                 private client: RunClient;\n  \
                 read(id: string) {\n    \
                 return class Inner {\n      \
                 run() {\n        \
                 return this.client.fetchStream(id);\n      }\n    };\n  }\n}\n",
            ),
        ]);

        assert!(callee_names(&defs, "Manager.read").is_empty());
        drop(dir);
    }
}
