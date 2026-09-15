//! Manifest index and module resolution for the outbound-call scan.
//!
//! Two questions the egress scan asks of a module specifier, and nothing else:
//! is it an external package, and if it is workspace-internal, which file does
//! it name? Both answers come from the repo's own manifests and its own file
//! tree, so there is no vendor list and no naming convention anywhere in here.
//!
//! The external universe is deliberately repo-wide. npm hoisting makes any
//! package declared by any manifest in the tree importable from any file in it,
//! and in a monorepo the manifest that declares a service client is almost
//! never the manifest of the service that ends up shipping the call — it is the
//! manifest of the shared package holding the wrapper. Scoping the universe to
//! one service's `package.json` therefore answers "external?" with "no" for
//! most real wrappers.
//!
//! This is not a Node resolver. Conditions, `browser` fields, and
//! `node_modules` lookups are all out of scope: the scan needs to know which
//! SOURCE file in this repo a specifier names, and the candidate list below is
//! what the repo's own layout proves. A workspace member's `exports` map is
//! read, because for a package that publishes subpaths it is the only statement
//! anywhere of which file `@scope/core/v3` names — but it is read as a set of
//! spellings of one module, not as a condition algorithm
//! ([`WorkspaceIndex::resolve_exports`]).
//!
//! An index built by [`WorkspaceIndex::build_with_aliases`] also reads the
//! aliases the repo's config declares (tsconfig `paths`/`baseUrl`, package.json
//! `imports`, Deno import maps) and resolves `@/x` through them
//! ([`crate::module_aliases`], carrick#1104). [`WorkspaceIndex::build`] does
//! not: the analyzer-input path still uses it, and resolving more specifiers
//! there would change what the model is asked (carrick#474). How the two are
//! meant to converge is in `docs/reference/module-resolution.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use crate::agents::file_orchestrator::FileOrchestrator;
use crate::module_aliases::{
    AliasTarget, ModuleAliases, TS_CONFIG_NAMES, collect_string_leaves, match_pattern,
};
use crate::packages::{MANIFEST_SKIP_DIRS, deno_workspace_manifest_paths, read_manifest};

/// Source extensions a specifier may resolve to, in the order they are tried.
/// TypeScript first: in a repo that has both, the `.ts` file is the source and
/// the `.js` file is build output that the walk usually excludes anyway.
const SOURCE_EXTENSIONS: [&str; 4] = ["ts", "tsx", "js", "jsx"];

/// What a module specifier names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// A package the repo's manifests declare as an external dependency.
    ///
    /// `package` is the declared name, so a subpath specifier still reports the
    /// package. `subpath` is what the specifier named under it — `edge` for
    /// `pkg/edge`, `None` for the root — which the egress scan records
    /// alongside the package rather than discarding, because a vendor's edge
    /// entry point and its node entry point are different destinations.
    External {
        package: String,
        subpath: Option<String>,
    },
    /// A file inside this repo, repo-relative.
    Internal(PathBuf),
    /// Node builtins, absolute paths, assets, and anything the repo's own
    /// manifests and file tree do not account for.
    Unresolved,
}

/// One workspace member: where it lives and what its manifest says its entry
/// point is.
#[derive(Debug, Clone)]
struct InternalPackage {
    /// Repo-relative directory holding the `package.json`.
    dir: PathBuf,
    /// The manifest's `main`, when it has one. Used only for a bare import of
    /// the package with no subpath, and only when `exports` states nothing for
    /// it.
    main: Option<String>,
    /// The manifest's `exports` field, verbatim. This is the only thing in a
    /// manifest that says which FILE a subpath specifier names, and a workspace
    /// package that publishes subpaths (`@scope/core/v3`) states them nowhere
    /// else: the subpath is a name the manifest maps, not a directory under the
    /// package root. See [`WorkspaceIndex::resolve_exports`] for what is read
    /// off it.
    exports: Option<serde_json::Value>,
}

/// The repo's manifests, reduced to what specifier resolution needs.
#[derive(Debug, Clone)]
pub struct WorkspaceIndex {
    repo_root: PathBuf,
    /// `repo_root` canonicalized, so an importer spelled through a resolved
    /// symlink still maps to its repo-relative path.
    canonical_root: PathBuf,
    /// Every package name any manifest declares as a runtime dependency, minus
    /// the workspace's own package names.
    external_packages: BTreeSet<String>,
    /// Workspace package name -> its directory.
    internal_packages: BTreeMap<String, InternalPackage>,
    /// The aliases the repo's config declares. `None` for an index built by
    /// [`WorkspaceIndex::build`], whose answers feed analyzer inputs and are
    /// pinned until carrick#474 moves them.
    aliases: Option<ModuleAliases>,
}

impl WorkspaceIndex {
    /// Read every `package.json` in the tree once, and derive both halves from
    /// the same walk so the two can never disagree about which names are
    /// internal.
    ///
    /// `dependencies`, `peerDependencies`, and `optionalDependencies` are the
    /// three maps whose contents can be present at runtime.
    /// `devDependencies` stays out: a build or test tool calling out is not
    /// service egress, and a package a wrapper genuinely uses at runtime is
    /// declared as a runtime dependency by the package that holds the wrapper,
    /// even when the root manifest also lists it as a dev dependency.
    pub fn build(repo_root: &Path) -> Self {
        Self::build_inner(repo_root, None)
    }

    /// [`WorkspaceIndex::build`], plus the aliases the repo's config files
    /// declare (carrick#1104). `service_tsconfig` is the service directory and
    /// the `tsconfig` its `carrick.json` entry names, both relative to the
    /// repo root; it governs every file under that directory in place of the
    /// nearest config.
    ///
    /// Used by the call graph, whose edges feed no analyzer input. The
    /// analyzer-input path keeps [`WorkspaceIndex::build`] until carrick#474.
    pub fn build_with_aliases(repo_root: &Path, service_tsconfig: Option<(&Path, &Path)>) -> Self {
        Self::build_inner(repo_root, Some(service_tsconfig))
    }

    fn build_inner(repo_root: &Path, aliases: Option<Option<(&Path, &Path)>>) -> Self {
        let mut declared: BTreeSet<String> = BTreeSet::new();
        let mut internal_names: BTreeSet<String> = BTreeSet::new();
        let mut internal_packages: BTreeMap<String, InternalPackage> = BTreeMap::new();

        let mut names: Vec<&str> = vec!["package.json"];
        if aliases.is_some() {
            names.extend(["deno.json", "deno.jsonc"]);
            names.extend(TS_CONFIG_NAMES);
        }
        let tree = tree_files(repo_root, &names);

        for manifest in manifest_paths(repo_root, &tree) {
            let Ok(facts) = read_manifest(&manifest) else {
                continue;
            };
            declared.extend(facts.package.dependencies.keys().cloned());
            declared.extend(facts.package.peer_dependencies.keys().cloned());
            declared.extend(facts.package.optional_dependencies.keys().cloned());
            let Some(name) = facts.package.name.as_deref() else {
                continue;
            };
            internal_names.insert(name.to_string());
            let Some(dir) = manifest
                .parent()
                .and_then(|d| d.strip_prefix(repo_root).ok())
                .map(Path::to_path_buf)
            else {
                continue;
            };
            let main = facts.main;
            let exports = facts.exports;
            // Two directories can declare the same package name — a vendored
            // copy, a fork kept alongside the original. Keeping the
            // lexicographically smallest directory makes the choice a property
            // of the tree rather than of walk order, which is the same
            // determinism requirement carrick#512 tracks for the manifest walk.
            match internal_packages.get(name) {
                Some(existing) if existing.dir <= dir => {}
                _ => {
                    internal_packages
                        .insert(name.to_string(), InternalPackage { dir, main, exports });
                }
            }
        }

        // A workspace member declared as a sibling's dependency (`workspace:*`)
        // is an internal call, not egress.
        for name in &internal_names {
            declared.remove(name);
        }

        let aliases = aliases.map(|service_tsconfig| {
            let configs: Vec<PathBuf> = tree
                .iter()
                .filter_map(|path| path.strip_prefix(repo_root).ok().map(Path::to_path_buf))
                .collect();
            let package_dirs: BTreeMap<String, PathBuf> = internal_packages
                .iter()
                .map(|(name, package)| (name.clone(), package.dir.clone()))
                .collect();
            ModuleAliases::build(repo_root, &configs, &package_dirs, service_tsconfig)
        });

        WorkspaceIndex {
            repo_root: repo_root.to_path_buf(),
            canonical_root: repo_root
                .canonicalize()
                .unwrap_or_else(|_| repo_root.to_path_buf()),
            external_packages: declared,
            internal_packages,
            aliases,
        }
    }

    /// `extends` values an alias-reading index could not follow to a config
    /// on disk, sorted. Empty for an index built without aliases.
    pub fn unfollowed_extends(&self) -> Vec<String> {
        self.aliases
            .as_ref()
            .map(|aliases| aliases.unfollowed_extends().cloned().collect())
            .unwrap_or_default()
    }

    /// The source file `specifier` names as written in `importer`, as the
    /// path the call graph keys files by: canonical when that stays inside
    /// the repo, else the plain join.
    ///
    /// The one module resolver the call-edge passes share (carrick#1104). A
    /// relative specifier goes through
    /// [`FileOrchestrator::resolve_relative_import`], so every relative answer
    /// is the one it always was; anything else goes through
    /// [`WorkspaceIndex::resolve`], which is where aliases and package names
    /// are read. `importer` may be absolute or repo-relative.
    pub fn resolve_module_path(&self, importer: &Path, specifier: &str) -> Option<PathBuf> {
        if let Some(target) = FileOrchestrator::resolve_relative_import(importer, specifier) {
            return Some(target);
        }
        match self.resolve(importer, specifier) {
            Resolution::Internal(relative) => self.source_path(&relative),
            _ => None,
        }
    }

    /// A repo-relative file as the call graph keys it. A package directory
    /// that is itself a symlink out of the tree canonicalizes to a path the
    /// cloud boundary cannot strip the repo root from, and an absolute path in
    /// the blob is a locator nothing can invert; the plain join is under the
    /// root by construction.
    pub fn source_path(&self, relative: &Path) -> Option<PathBuf> {
        let target = self.repo_root.join(relative);
        match target.canonicalize() {
            Ok(canonical) if canonical.starts_with(&self.repo_root) => Some(canonical),
            _ => target.is_file().then_some(target),
        }
    }

    /// `path` relative to the repo root, whether it was given absolute (as
    /// walked, or canonicalized) or already relative.
    fn repo_relative(&self, path: &Path) -> PathBuf {
        if path.is_relative() {
            return path.to_path_buf();
        }
        path.strip_prefix(&self.repo_root)
            .or_else(|_| path.strip_prefix(&self.canonical_root))
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| path.to_path_buf())
    }

    /// Whether the repo declares any workspace member at all. No members means
    /// no specifier can name one, so the caller can skip the package-surface
    /// walk entirely.
    pub fn has_internal_packages(&self) -> bool {
        !self.internal_packages.is_empty()
    }

    /// Whether any external package is declared at all. An empty universe means
    /// no specifier can resolve external, so the caller can skip the scan.
    pub fn has_external_packages(&self) -> bool {
        !self.external_packages.is_empty()
    }

    /// Resolve `specifier` as written in `from_file` (repo-relative).
    ///
    /// Relative first, then the aliases the repo's config declares (only in an
    /// index built with them), then workspace members, then the external
    /// universe: a workspace member shadows an external package of the same
    /// name, which is what the `internal_names` subtraction in
    /// [`WorkspaceIndex::build`] already decided. An alias is tried before
    /// package names because that is the order the compiler and Deno apply
    /// them in: `paths` and an import map are consulted before any package
    /// lookup.
    pub fn resolve(&self, from_file: &Path, specifier: &str) -> Resolution {
        if specifier.starts_with("./") || specifier.starts_with("../") {
            let base = match from_file.parent() {
                Some(dir) => normalize(&dir.join(specifier)),
                None => normalize(Path::new(specifier)),
            };
            return match self.resolve_file(&base) {
                Some(file) => Resolution::Internal(file),
                None => Resolution::Unresolved,
            };
        }

        if let Some(file) = self.resolve_alias(from_file, specifier) {
            return Resolution::Internal(file);
        }

        if let Some(name) = longest_match(specifier, self.internal_packages.keys()) {
            let package = &self.internal_packages[&name];
            let subpath = specifier[name.len()..].trim_start_matches('/');
            let key = if subpath.is_empty() {
                ".".to_string()
            } else {
                format!("./{}", subpath)
            };
            let resolved = self.resolve_exports(package, &key).or_else(|| {
                if subpath.is_empty() {
                    self.resolve_package_entry(package)
                } else {
                    self.resolve_file(&normalize(&package.dir.join(subpath)))
                }
            });
            return match resolved {
                Some(file) => Resolution::Internal(file),
                None => Resolution::Unresolved,
            };
        }

        match longest_match(specifier, self.external_packages.iter()) {
            Some(package) => {
                let subpath = specifier[package.len()..].trim_start_matches('/');
                let subpath = (!subpath.is_empty()).then(|| subpath.to_string());
                Resolution::External { package, subpath }
            }
            None => Resolution::Unresolved,
        }
    }

    /// The file the repo's config aliases `specifier` to, when the index reads
    /// aliases and one names an existing source file.
    fn resolve_alias(&self, from_file: &Path, specifier: &str) -> Option<PathBuf> {
        let aliases = self.aliases.as_ref()?;
        let from_file = self.repo_relative(from_file);
        aliases
            .resolve(&from_file, specifier)
            .into_iter()
            .find_map(|target| match target {
                AliasTarget::Paths(candidates) => candidates
                    .iter()
                    .find_map(|candidate| self.resolve_file(candidate)),
                AliasTarget::Leaves { dir, leaves } => self.pick_source_leaf(&dir, leaves),
            })
    }

    /// The source file a manifest's `exports` field names for one specifier
    /// key (`"."` for the package itself, `"./v3"` for a subpath).
    ///
    /// This is not a Node conditional-exports implementation and does not try
    /// to be one. Conditions (`import`/`require`/`types`/vendor-specific ones)
    /// select between spellings of the SAME module, and the question here is
    /// only which file in this repo that module is. So every string leaf under
    /// the matched key is a candidate, sorted so the answer is a property of
    /// the manifest rather than of map iteration order, and the first that
    /// names an existing file wins — with two orderings laid over it:
    ///
    /// - Declaration files are dropped outright. A `types` condition points at
    ///   a `.d.ts`, which states the module's shape and none of its behaviour;
    ///   reading requests off one would find nothing and shadow the source that
    ///   has them.
    /// - A TypeScript hit is preferred over any other. `main` and the default
    ///   conditions point at build output, which in a repo that commits its
    ///   `dist` exists alongside the source and would otherwise win on sort
    ///   order; the source is what the scan reads.
    ///
    /// Wildcard keys (`"./*": "./src/*.ts"`) are expanded only in an index
    /// that reads aliases, where the pattern is applied the way Node applies
    /// it ([`crate::module_aliases::match_pattern`]). The manifest-only index
    /// keeps its earlier answer, the fallback below (the subpath as a
    /// directory under the package root), because its answers are analyzer
    /// input (carrick#474).
    fn resolve_exports(&self, package: &InternalPackage, key: &str) -> Option<PathBuf> {
        let exports = package.exports.as_ref()?;
        let (entry, substitution) = match exports {
            // `"exports": "./index.js"` — the whole field is the "." entry.
            serde_json::Value::String(_) if key == "." => (exports, None),
            serde_json::Value::Object(map) => {
                if map.keys().any(|k| k.starts_with('.')) {
                    match map.get(key) {
                        Some(entry) => (entry, None),
                        None if self.aliases.is_some() => {
                            let (pattern, captured) =
                                match_pattern(map.keys().map(String::as_str), key)?;
                            (&map[pattern], Some(captured))
                        }
                        None => return None,
                    }
                } else if key == "." {
                    // A conditions object with no subpath keys IS the "."
                    // entry (`"exports": { "import": "./index.js" }`).
                    (exports, None)
                } else {
                    return None;
                }
            }
            _ => return None,
        };

        let mut leaves: Vec<String> = Vec::new();
        collect_string_leaves(entry, 0, &mut leaves);
        if let Some(captured) = substitution {
            leaves = leaves
                .into_iter()
                .map(|leaf| leaf.replace('*', &captured))
                .collect();
        }
        self.pick_source_leaf(&package.dir, leaves)
    }

    /// One module's spellings under `dir`, reduced to the source file: sorted
    /// so the answer is a property of the config, declaration files dropped,
    /// and a TypeScript hit preferred over build output.
    fn pick_source_leaf(&self, dir: &Path, mut leaves: Vec<String>) -> Option<PathBuf> {
        leaves.sort_unstable();
        leaves.dedup();

        let mut fallback: Option<PathBuf> = None;
        for leaf in &leaves {
            if is_declaration_file(leaf) {
                continue;
            }
            let Some(file) = self.resolve_file(&normalize(&dir.join(leaf))) else {
                continue;
            };
            if file
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "ts" || e == "tsx")
            {
                return Some(file);
            }
            fallback.get_or_insert(file);
        }
        fallback
    }

    /// A bare import of a workspace member: its declared `main` if it has one,
    /// then the two index conventions. `main` is run through the same candidate
    /// list because it habitually points at build output (`dist/index.js`) that
    /// the source tree spells `.ts`.
    fn resolve_package_entry(&self, package: &InternalPackage) -> Option<PathBuf> {
        if let Some(main) = &package.main
            && let Some(file) = self.resolve_file(&normalize(&package.dir.join(main)))
        {
            return Some(file);
        }
        self.resolve_file(&package.dir.join("index"))
            .or_else(|| self.resolve_file(&package.dir.join("src/index")))
    }

    /// The candidate list, first hit wins: the path as written when it already
    /// names an existing source file; the path with each source extension
    /// appended; a written `.js`/`.jsx` swapped for its TypeScript counterpart
    /// (the ESM-import-specifier convention, where the specifier names the
    /// emitted file); and finally the directory's index file.
    fn resolve_file(&self, base: &Path) -> Option<PathBuf> {
        let has_source_extension = base
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| SOURCE_EXTENSIONS.contains(&e));
        if has_source_extension && self.exists(base) {
            return Some(base.to_path_buf());
        }

        let name = base.file_name().and_then(|n| n.to_str())?.to_string();
        let parent = base.parent().map(Path::to_path_buf).unwrap_or_default();

        for extension in SOURCE_EXTENSIONS {
            let candidate = parent.join(format!("{}.{}", name, extension));
            if self.exists(&candidate) {
                return Some(candidate);
            }
        }

        for (written, source) in [("js", "ts"), ("js", "tsx"), ("jsx", "tsx"), ("jsx", "ts")] {
            if let Some(stem) = name.strip_suffix(&format!(".{}", written)) {
                let candidate = parent.join(format!("{}.{}", stem, source));
                if self.exists(&candidate) {
                    return Some(candidate);
                }
            }
        }

        for extension in SOURCE_EXTENSIONS {
            let candidate = base.join(format!("index.{}", extension));
            if self.exists(&candidate) {
                return Some(candidate);
            }
        }

        None
    }

    fn exists(&self, relative: &Path) -> bool {
        self.repo_root.join(relative).is_file()
    }
}

/// A TypeScript declaration file: shape without behaviour.
fn is_declaration_file(path: &str) -> bool {
    [".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|suffix| path.ends_with(suffix))
}

/// Every file under `repo_root` named one of `names`, in walk order, skipping
/// dependency installs and build output. One walk serves the manifests and,
/// for an alias-reading index, the config files beside them.
fn tree_files(repo_root: &Path, names: &[&str]) -> Vec<PathBuf> {
    walkdir::WalkDir::new(repo_root)
        .sort_by_file_name()
        .follow_links(true)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0
                || !(e.file_type().is_dir()
                    && e.file_name()
                        .to_str()
                        .is_some_and(|n| MANIFEST_SKIP_DIRS.contains(&n)))
        })
        .flatten()
        .filter(|e| {
            e.file_type().is_file() && e.file_name().to_str().is_some_and(|n| names.contains(&n))
        })
        .map(|e| e.path().to_path_buf())
        .collect()
}

/// Every package or Deno manifest under `repo_root`, sorted. `tree` is the
/// walk from [`tree_files`]. Sorted so the duplicate-name tiebreak in
/// [`WorkspaceIndex::build`] sees a stable sequence whatever the filesystem
/// hands back.
fn manifest_paths(repo_root: &Path, tree: &[PathBuf]) -> Vec<PathBuf> {
    let mut manifests: Vec<PathBuf> = tree
        .iter()
        .filter(|path| path.file_name().is_some_and(|n| n == "package.json"))
        .cloned()
        .collect();
    match deno_workspace_manifest_paths(repo_root) {
        Ok(deno_manifests) => manifests.extend(deno_manifests),
        Err(error) => tracing::warn!("Ignoring Deno workspace manifests: {}", error),
    }
    manifests.sort();
    manifests.dedup();
    manifests
}

/// The longest name in `names` that `specifier` either equals or sits under as
/// a subpath. Ties break on the name itself so the result is a property of the
/// inputs rather than of iteration order.
fn longest_match<'a>(specifier: &str, names: impl Iterator<Item = &'a String>) -> Option<String> {
    names
        .filter(|name| specifier == name.as_str() || specifier.starts_with(&format!("{}/", name)))
        .max_by_key(|name| (name.len(), name.as_str()))
        .cloned()
}

/// Collapse `.` and `..` lexically. Paths here are repo-relative and may not
/// exist yet (the candidate list is about to test several spellings), so this
/// cannot go through `canonicalize`.
///
/// Shared with [`crate::service_derivation`], which resolves import-map
/// targets against a member directory for the same reason: one normalisation,
/// not two that can disagree.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/external-call-candidates")
    }

    fn index() -> WorkspaceIndex {
        WorkspaceIndex::build(&fixture_root())
    }

    fn resolve(from: &str, specifier: &str) -> Resolution {
        index().resolve(Path::new(from), specifier)
    }

    fn workspace_package_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workspace-package-client")
    }

    fn resolve_in_workspace_package_fixture(specifier: &str) -> Resolution {
        WorkspaceIndex::build(&workspace_package_root())
            .resolve(Path::new("packages/sdk/src/widgets.ts"), specifier)
    }

    #[test]
    fn a_subpath_exports_key_names_the_source_it_maps_to() {
        // `@fixture/core` publishes `./v2` only through its `exports` map;
        // there is no `packages/core/v2` directory for the fallback to find.
        assert_eq!(
            resolve_in_workspace_package_fixture("@fixture/core/v2"),
            Resolution::Internal(PathBuf::from("packages/core/src/v2/index.ts"))
        );
    }

    #[test]
    fn the_typescript_source_beats_the_committed_build_output() {
        // The same key also names `./dist/index.d.ts` (a declaration file) and
        // `./dist/index.js` (build output), both of which exist on disk.
        assert_eq!(
            resolve_in_workspace_package_fixture("@fixture/core"),
            Resolution::Internal(PathBuf::from("packages/core/src/index.ts"))
        );
    }

    #[test]
    fn a_package_with_no_exports_map_still_resolves_through_main() {
        assert_eq!(
            resolve_in_workspace_package_fixture("@fixture/other"),
            Resolution::Internal(PathBuf::from("packages/other/src/index.ts"))
        );
    }

    #[test]
    fn an_exports_key_the_manifest_does_not_publish_resolves_to_nothing() {
        assert_eq!(
            resolve_in_workspace_package_fixture("@fixture/core/v9"),
            Resolution::Unresolved
        );
    }

    #[test]
    fn relative_specifier_resolves_to_a_sibling_file() {
        assert_eq!(
            resolve("apps/api/src/no-rows.ts", "./helper"),
            Resolution::Internal(PathBuf::from("apps/api/src/helper.ts"))
        );
    }

    #[test]
    fn parent_segments_are_collapsed() {
        assert_eq!(
            resolve("apps/api/src/nested/deep.ts", "../helper"),
            Resolution::Internal(PathBuf::from("apps/api/src/helper.ts"))
        );
    }

    #[test]
    fn internal_package_name_resolves_through_its_manifest_main() {
        // `@fixture/internal-lib` declares `src/index.ts` as its main.
        assert_eq!(
            resolve("apps/api/src/no-rows.ts", "@fixture/internal-lib"),
            Resolution::Internal(PathBuf::from("packages/internal-lib/src/index.ts"))
        );
    }

    #[test]
    fn internal_package_subpath_resolves_under_the_package_dir() {
        assert_eq!(
            resolve("apps/api/src/no-rows.ts", "@fixture/mail-kit/transport"),
            Resolution::Internal(PathBuf::from("packages/mail-kit/transport.ts"))
        );
    }

    /// A specifier written as the emitted `.js` file resolves to the TypeScript
    /// source it is emitted from.
    #[test]
    fn written_js_extension_swaps_to_the_typescript_source() {
        assert_eq!(
            resolve("apps/api/src/no-rows.ts", "./helper.js"),
            Resolution::Internal(PathBuf::from("apps/api/src/helper.ts"))
        );
    }

    #[test]
    fn directory_specifier_resolves_to_its_index_file() {
        assert_eq!(
            resolve("apps/worker/src/entry.ts", "./barrel"),
            Resolution::Internal(PathBuf::from("apps/worker/src/barrel/index.ts"))
        );
    }

    /// A package with no `main` falls back to `index.*` beside the manifest,
    /// then `src/index.*`.
    #[test]
    fn package_without_main_falls_back_to_index() {
        assert_eq!(
            resolve("apps/worker/src/entry.ts", "@fixture/mail-kit"),
            Resolution::Internal(PathBuf::from("packages/mail-kit/index.ts"))
        );
    }

    #[test]
    fn declared_dependency_resolves_external_by_name_and_by_subpath() {
        assert_eq!(
            resolve("apps/api/src/direct-call.ts", "courier-sdk"),
            Resolution::External {
                package: "courier-sdk".to_string(),
                subpath: None
            }
        );
        assert_eq!(
            resolve("apps/api/src/direct-call.ts", "courier-sdk/edge"),
            Resolution::External {
                package: "courier-sdk".to_string(),
                subpath: Some("edge".to_string())
            }
        );
    }

    /// Everything past the package name is the subpath, however many segments
    /// it has, so a deep entry point is not confused with the shallow one it
    /// sits under.
    #[test]
    fn a_deep_subpath_is_kept_whole() {
        assert_eq!(
            resolve("apps/api/src/direct-call.ts", "courier-sdk/edge/runtime"),
            Resolution::External {
                package: "courier-sdk".to_string(),
                subpath: Some("edge/runtime".to_string())
            }
        );
    }

    /// A dependency only the root manifest declares is still external
    /// everywhere: hoisting makes it importable from any file in the tree.
    #[test]
    fn root_only_dependency_is_in_the_external_universe() {
        assert_eq!(
            resolve("packages/doc-kit/sign.ts", "pdf-toolkit"),
            Resolution::External {
                package: "pdf-toolkit".to_string(),
                subpath: None
            }
        );
    }

    #[test]
    fn dev_dependency_is_not_external() {
        assert_eq!(
            resolve("apps/api/src/dev-only.ts", "bench-harness"),
            Resolution::Unresolved
        );
    }

    #[test]
    fn builtins_and_absolute_paths_are_unresolved() {
        for specifier in ["fs", "node:fs/promises", "/etc/config", "crypto"] {
            assert_eq!(
                resolve("apps/api/src/no-rows.ts", specifier),
                Resolution::Unresolved,
                "{} should not resolve",
                specifier
            );
        }
    }

    #[test]
    fn a_relative_specifier_naming_no_file_is_unresolved() {
        assert_eq!(
            resolve("apps/api/src/no-rows.ts", "./nowhere"),
            Resolution::Unresolved
        );
    }

    /// Duplicate package names keep the lexicographically smallest directory,
    /// so the choice does not depend on walk order.
    #[test]
    fn duplicate_package_name_keeps_the_smallest_directory() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        std::fs::write(root.join("package.json"), r#"{"name":"root-app"}"#).unwrap();
        for dir in ["zeta-copy", "alpha-copy"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(
                root.join(dir).join("package.json"),
                r#"{"name":"@dup/shared"}"#,
            )
            .unwrap();
            std::fs::write(root.join(dir).join("index.ts"), "export const x = 1;\n").unwrap();
        }
        let index = WorkspaceIndex::build(root);
        assert_eq!(
            index.resolve(Path::new("app.ts"), "@dup/shared"),
            Resolution::Internal(PathBuf::from("alpha-copy/index.ts"))
        );
    }

    /// Installed packages are not workspace members, and their manifests must
    /// not contribute to either half of the index.
    #[test]
    fn installed_manifests_are_skipped() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"root-app","dependencies":{"courier-sdk":"^1.0.0"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("node_modules/courier-sdk")).unwrap();
        std::fs::write(
            root.join("node_modules/courier-sdk/package.json"),
            r#"{"name":"courier-sdk","dependencies":{"hoisted-only":"^1.0.0"}}"#,
        )
        .unwrap();
        let index = WorkspaceIndex::build(root);
        assert_eq!(
            index.resolve(Path::new("app.ts"), "courier-sdk"),
            Resolution::External {
                package: "courier-sdk".to_string(),
                subpath: None
            }
        );
        assert_eq!(
            index.resolve(Path::new("app.ts"), "hoisted-only"),
            Resolution::Unresolved
        );
    }

    #[test]
    fn vite_artifacts_do_not_declare_workspace_packages() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"root-app","dependencies":{"cached-vendor":"^1.0.0"}}"#,
        )
        .unwrap();
        let cache = root.join("packages/api/.vite/deps");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("package.json"),
            r#"{"name":"cached-vendor","main":"./index.ts","dependencies":{"cache-only":"^1.0.0"}}"#).unwrap();
        std::fs::write(cache.join("index.ts"), "export const cached = true;").unwrap();
        let index = WorkspaceIndex::build(root);
        assert_eq!(
            index.resolve(Path::new("app.ts"), "cached-vendor"),
            Resolution::External {
                package: "cached-vendor".to_string(),
                subpath: None
            }
        );
        assert_eq!(
            index.resolve(Path::new("app.ts"), "cache-only"),
            Resolution::Unresolved
        );
    }

    #[test]
    fn a_repo_declaring_nothing_has_no_external_packages() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("package.json"), r#"{"name":"bare"}"#).unwrap();
        assert!(!WorkspaceIndex::build(repo.path()).has_external_packages());
    }

    /// Write `(relative path, contents)` pairs under a fresh repo root.
    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let path = repo.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        repo
    }

    fn aliased(repo: &tempfile::TempDir, from: &str, specifier: &str) -> Resolution {
        WorkspaceIndex::build_with_aliases(repo.path(), None).resolve(Path::new(from), specifier)
    }

    fn internal(path: &str) -> Resolution {
        Resolution::Internal(PathBuf::from(path))
    }

    /// The analyzer-input index never reads aliases (carrick#474): the same
    /// tree answers differently only through `build_with_aliases`.
    #[test]
    fn the_manifest_only_index_reads_no_alias() {
        let repo = tree(&[
            (
                "tsconfig.json",
                r#"{"compilerOptions":{"paths":{"@/*":["./src/*"]}}}"#,
            ),
            ("deno.json", r#"{"imports":{"$/":"./src/"}}"#),
            (
                "package.json",
                r##"{"name":"app","imports":{"#db":"./src/db.ts"}}"##,
            ),
            ("src/db.ts", "export const db = 1;"),
        ]);
        let manifest_only = WorkspaceIndex::build(repo.path());
        for specifier in ["@/db", "$/db.ts", "#db"] {
            assert_eq!(
                manifest_only.resolve(Path::new("src/app.ts"), specifier),
                Resolution::Unresolved
            );
            assert_eq!(
                aliased(&repo, "src/app.ts", specifier),
                internal("src/db.ts")
            );
        }
    }

    /// `paths` inherited through `extends` resolves against the `baseUrl` the
    /// base declares, relative to the base's own directory.
    #[test]
    fn tsconfig_paths_inherited_through_extends_resolve_against_the_base_url() {
        let repo = tree(&[
            (
                "tsconfig.base.json",
                "{\n  // comments and trailing commas are valid tsconfig\n  \"compilerOptions\": {\"baseUrl\": \".\", \"paths\": {\"@/*\": [\"apps/api/src/*\"],},},\n}",
            ),
            (
                "apps/api/tsconfig.json",
                r#"{"extends":"../../tsconfig.base.json"}"#,
            ),
            ("apps/api/src/slots/availability.ts", "export const a = 1;"),
        ]);
        assert_eq!(
            aliased(
                &repo,
                "apps/api/src/deliveries/plan.ts",
                "@/slots/availability"
            ),
            internal("apps/api/src/slots/availability.ts")
        );
    }

    /// With no `baseUrl` anywhere, `paths` targets resolve against the config
    /// that declared `paths`, not the one that extends it.
    #[test]
    fn paths_without_base_url_resolve_against_the_declaring_config() {
        let repo = tree(&[
            (
                "config/tsconfig.paths.json",
                r#"{"compilerOptions":{"paths":{"@lib/*":["../lib/*"]}}}"#,
            ),
            (
                "apps/api/tsconfig.json",
                r#"{"extends":"../../config/tsconfig.paths"}"#,
            ),
            ("lib/clock.ts", "export const now = 1;"),
        ]);
        assert_eq!(
            aliased(&repo, "apps/api/src/x.ts", "@lib/clock"),
            internal("lib/clock.ts")
        );
    }

    /// A nearer layer's `paths` replaces the inherited map whole, and a later
    /// `extends` entry overrides an earlier one.
    #[test]
    fn nearer_paths_replace_inherited_paths_and_later_extends_win() {
        let repo = tree(&[
            (
                "a.json",
                r#"{"compilerOptions":{"paths":{"@a/*":["./from-a/*"],"@x/*":["./from-a/*"]}}}"#,
            ),
            (
                "b.json",
                r#"{"compilerOptions":{"paths":{"@x/*":["./from-b/*"]}}}"#,
            ),
            ("tsconfig.json", r#"{"extends":["./a.json","./b.json"]}"#),
            ("from-a/m.ts", ""),
            ("from-b/m.ts", ""),
        ]);
        assert_eq!(aliased(&repo, "src/x.ts", "@x/m"), internal("from-b/m.ts"));
        assert_eq!(aliased(&repo, "src/x.ts", "@a/m"), Resolution::Unresolved);
    }

    /// `extends` naming a workspace package reads the config that package
    /// holds; one naming nothing on disk is reported.
    #[test]
    fn extends_through_a_workspace_package_and_an_unfollowed_extends_is_reported() {
        let repo = tree(&[
            (
                "packages/tsconfig/package.json",
                r#"{"name":"@acme/tsconfig"}"#,
            ),
            (
                "packages/tsconfig/base.json",
                r#"{"compilerOptions":{"baseUrl":"../..","paths":{"@shared/*":["packages/shared/src/*"]}}}"#,
            ),
            (
                "apps/api/tsconfig.json",
                r#"{"extends":["@acme/tsconfig/base.json","@missing/config"]}"#,
            ),
            ("packages/shared/src/money.ts", ""),
        ]);
        let index = WorkspaceIndex::build_with_aliases(repo.path(), None);
        assert_eq!(
            index.resolve(Path::new("apps/api/src/x.ts"), "@shared/money"),
            internal("packages/shared/src/money.ts")
        );
        assert_eq!(
            index.unfollowed_extends(),
            vec!["@missing/config".to_string()]
        );
    }

    /// The nearest config governs; a jsconfig serves where no tsconfig sits;
    /// a service's named tsconfig overrides the nearest for its files.
    #[test]
    fn the_governing_config_is_the_nearest_unless_the_service_names_one() {
        let repo = tree(&[
            (
                "tsconfig.json",
                r#"{"compilerOptions":{"paths":{"@/*":["./root/*"]}}}"#,
            ),
            (
                "web/jsconfig.json",
                r#"{"compilerOptions":{"baseUrl":"src"}}"#,
            ),
            (
                "api/tsconfig.app.json",
                r#"{"compilerOptions":{"paths":{"@/*":["./src/*"]}}}"#,
            ),
            ("root/m.ts", ""),
            ("web/src/utils/m.js", ""),
            ("api/src/m.ts", ""),
        ]);
        assert_eq!(aliased(&repo, "lib/x.ts", "@/m"), internal("root/m.ts"));
        // A nearer config with no `paths` governs: the root map does not leak in.
        assert_eq!(
            aliased(&repo, "web/src/x.js", "@/m"),
            Resolution::Unresolved
        );
        assert_eq!(
            aliased(&repo, "web/src/x.js", "utils/m"),
            internal("web/src/utils/m.js")
        );
        let service = WorkspaceIndex::build_with_aliases(
            repo.path(),
            Some((Path::new("api"), Path::new("tsconfig.app.json"))),
        );
        assert_eq!(
            service.resolve(Path::new("api/src/x.ts"), "@/m"),
            internal("api/src/m.ts")
        );
    }

    /// package.json `imports`: exact and pattern keys, conditions preferring
    /// the TypeScript source, decided by the nearest package.json alone.
    #[test]
    fn package_imports_resolve_from_the_nearest_manifest() {
        let repo = tree(&[
            (
                "apps/api/package.json",
                r##"{"name":"api","imports":{"#db":{"types":"./dist/db.d.ts","default":"./dist/db.js","source":"./src/db.ts"},"#queues/*":"./src/queues/*.ts","#vendor":"left-pad"}}"##,
            ),
            ("apps/api/src/nested/package.json", r#"{"name":"nested"}"#),
            ("apps/api/src/db.ts", ""),
            ("apps/api/dist/db.js", ""),
            ("apps/api/src/queues/pickup.ts", ""),
        ]);
        assert_eq!(
            aliased(&repo, "apps/api/src/x.ts", "#db"),
            internal("apps/api/src/db.ts")
        );
        assert_eq!(
            aliased(&repo, "apps/api/src/x.ts", "#queues/pickup"),
            internal("apps/api/src/queues/pickup.ts")
        );
        assert_eq!(
            aliased(&repo, "apps/api/src/x.ts", "#vendor"),
            Resolution::Unresolved
        );
        // The nearer manifest declares no `imports`, so nothing is inherited.
        assert_eq!(
            aliased(&repo, "apps/api/src/nested/x.ts", "#db"),
            Resolution::Unresolved
        );
    }

    /// Deno import maps: a member's map before the root's, `scopes` before
    /// `imports`, a registry target decides without naming a file, and an
    /// external `importMap` resolves against its own directory.
    #[test]
    fn deno_import_maps_apply_by_scope() {
        let repo = tree(&[
            (
                "deno.jsonc",
                r#"{ // root
                "workspace": ["./apps/api"],
                "imports": {"@/": "./shared/", "@db": "./shared/db.ts", "hono": "npm:hono@^4"},
                "scopes": {"./apps/api/legacy/": {"@db": "./shared/legacy-db.ts"}}
            }"#,
            ),
            (
                "apps/api/deno.json",
                r#"{"name":"@acme/api","importMap":"./maps/import_map.json"}"#,
            ),
            (
                "apps/api/maps/import_map.json",
                r#"{"imports":{"@/":"../src/"}}"#,
            ),
            ("shared/db.ts", ""),
            ("shared/legacy-db.ts", ""),
            ("shared/clock.ts", ""),
            ("apps/api/src/clock.ts", ""),
            ("hono.ts", ""),
        ]);
        assert_eq!(
            aliased(&repo, "apps/api/src/x.ts", "@/clock.ts"),
            internal("apps/api/src/clock.ts")
        );
        assert_eq!(
            aliased(&repo, "tools/x.ts", "@/clock.ts"),
            internal("shared/clock.ts")
        );
        assert_eq!(
            aliased(&repo, "apps/api/src/x.ts", "@db"),
            internal("shared/db.ts")
        );
        assert_eq!(
            aliased(&repo, "apps/api/legacy/x.ts", "@db"),
            internal("shared/legacy-db.ts")
        );
        // The key maps to a registry package, so the root `hono.ts` is not it.
        assert!(matches!(
            aliased(&repo, "tools/x.ts", "hono"),
            Resolution::External { .. }
        ));
    }

    /// A wildcard `exports` key is expanded in the alias-reading index only.
    #[test]
    fn wildcard_exports_expand_only_in_the_alias_reading_index() {
        let repo = tree(&[
            (
                "packages/ui/package.json",
                r#"{"name":"@acme/ui","exports":{"./*":"./src/components/*.ts"}}"#,
            ),
            ("packages/ui/src/components/button.ts", ""),
        ]);
        assert_eq!(
            aliased(&repo, "apps/web/x.ts", "@acme/ui/button"),
            internal("packages/ui/src/components/button.ts")
        );
        assert_eq!(
            WorkspaceIndex::build(repo.path())
                .resolve(Path::new("apps/web/x.ts"), "@acme/ui/button"),
            Resolution::Unresolved
        );
    }

    #[test]
    fn deno_manifests_supply_internal_exports_and_registry_identities() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        std::fs::create_dir_all(root.join("packages/core")).unwrap();
        std::fs::create_dir_all(root.join("packages/unlisted")).unwrap();
        std::fs::write(
            root.join("deno.jsonc"),
            r#"{
            "name":"root", "workspace":["./packages/core"],
            "imports":{"client":"npm:@vendor/client@^2.0.0/http"}
        }"#,
        )
        .unwrap();
        std::fs::write(
            root.join("packages/core/deno.json"),
            r#"{
            "name":"@local/core", "exports":{".":"./mod.ts"}
        }"#,
        )
        .unwrap();
        std::fs::write(root.join("packages/core/mod.ts"), "export const value = 1;").unwrap();
        std::fs::write(
            root.join("packages/unlisted/deno.json"),
            r#"{"name":"@local/unlisted","exports":"./mod.ts"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("packages/unlisted/mod.ts"),
            "export const hidden = 1;",
        )
        .unwrap();

        let index = WorkspaceIndex::build(root);
        assert_eq!(
            index.resolve(Path::new("app.ts"), "@local/core"),
            Resolution::Internal(PathBuf::from("packages/core/mod.ts"))
        );
        assert_eq!(
            index.resolve(Path::new("app.ts"), "@vendor/client/http"),
            Resolution::External {
                package: "@vendor/client".into(),
                subpath: Some("http".into())
            }
        );
        assert_eq!(
            index.resolve(Path::new("app.ts"), "@local/unlisted"),
            Resolution::Unresolved
        );
    }
}
