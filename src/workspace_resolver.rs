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
//! This is not a Node resolver. `exports` maps, conditions, `browser` fields,
//! `paths` from tsconfig, and `node_modules` lookups are all out of scope: the
//! scan needs to know which SOURCE file in this repo a specifier names, and the
//! candidate list below is what the repo's own layout proves.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use crate::packages::MANIFEST_SKIP_DIRS;

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
    /// the package with no subpath.
    main: Option<String>,
}

/// The repo's manifests, reduced to what specifier resolution needs.
#[derive(Debug, Clone)]
pub struct WorkspaceIndex {
    repo_root: PathBuf,
    /// Every package name any manifest declares as a runtime dependency, minus
    /// the workspace's own package names.
    external_packages: BTreeSet<String>,
    /// Workspace package name -> its directory.
    internal_packages: BTreeMap<String, InternalPackage>,
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
        let mut declared: BTreeSet<String> = BTreeSet::new();
        let mut internal_names: BTreeSet<String> = BTreeSet::new();
        let mut internal_packages: BTreeMap<String, InternalPackage> = BTreeMap::new();

        for manifest in manifest_paths(repo_root) {
            let Ok(text) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            for field in ["dependencies", "peerDependencies", "optionalDependencies"] {
                if let Some(map) = json.get(field).and_then(|v| v.as_object()) {
                    declared.extend(map.keys().cloned());
                }
            }
            let Some(name) = json.get("name").and_then(|n| n.as_str()) else {
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
            let main = json
                .get("main")
                .and_then(|m| m.as_str())
                .map(str::to_string);
            // Two directories can declare the same package name — a vendored
            // copy, a fork kept alongside the original. Keeping the
            // lexicographically smallest directory makes the choice a property
            // of the tree rather than of walk order, which is the same
            // determinism requirement carrick#512 tracks for the manifest walk.
            match internal_packages.get(name) {
                Some(existing) if existing.dir <= dir => {}
                _ => {
                    internal_packages.insert(name.to_string(), InternalPackage { dir, main });
                }
            }
        }

        // A workspace member declared as a sibling's dependency (`workspace:*`)
        // is an internal call, not egress.
        for name in &internal_names {
            declared.remove(name);
        }

        WorkspaceIndex {
            repo_root: repo_root.to_path_buf(),
            external_packages: declared,
            internal_packages,
        }
    }

    /// Whether any external package is declared at all. An empty universe means
    /// no specifier can resolve external, so the caller can skip the scan.
    pub fn has_external_packages(&self) -> bool {
        !self.external_packages.is_empty()
    }

    /// Resolve `specifier` as written in `from_file` (repo-relative).
    ///
    /// Relative first, then workspace members, then the external universe:
    /// a workspace member shadows an external package of the same name, which
    /// is what the `internal_names` subtraction in [`WorkspaceIndex::build`]
    /// already decided.
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

        if let Some(name) = longest_match(specifier, self.internal_packages.keys()) {
            let package = &self.internal_packages[&name];
            let subpath = specifier[name.len()..].trim_start_matches('/');
            let resolved = if subpath.is_empty() {
                self.resolve_package_entry(package)
            } else {
                self.resolve_file(&normalize(&package.dir.join(subpath)))
            };
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

/// Every `package.json` under `repo_root`, sorted, skipping dependency installs
/// and build output. Sorted so the duplicate-name tiebreak in
/// [`WorkspaceIndex::build`] sees a stable sequence whatever the filesystem
/// hands back.
fn manifest_paths(repo_root: &Path) -> Vec<PathBuf> {
    let walker = walkdir::WalkDir::new(repo_root)
        .follow_links(true)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0
                || !(e.file_type().is_dir()
                    && e.file_name()
                        .to_str()
                        .is_some_and(|n| MANIFEST_SKIP_DIRS.contains(&n)))
        });
    let mut manifests: Vec<PathBuf> = walker
        .flatten()
        .filter(|e| e.file_type().is_file() && e.file_name() == "package.json")
        .map(|e| e.path().to_path_buf())
        .collect();
    manifests.sort();
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
fn normalize(path: &Path) -> PathBuf {
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
    fn a_repo_declaring_nothing_has_no_external_packages() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("package.json"), r#"{"name":"bare"}"#).unwrap();
        assert!(!WorkspaceIndex::build(repo.path()).has_external_packages());
    }
}
