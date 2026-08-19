use semver::Version;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io, path::PathBuf};

/// Cap on the dependency names sent to cloud tasks. The cloud caps at 500
/// server-side; staying under it keeps requests deterministic.
pub const DEPENDENCY_NAME_CAP: usize = 500;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PackageJson {
    pub name: Option<String>,
    pub version: Option<String>,
    #[serde(default)]
    pub dependencies: HashMap<String, String>,
    #[serde(default)]
    #[serde(rename = "devDependencies")]
    pub dev_dependencies: HashMap<String, String>,
    #[serde(default)]
    #[serde(rename = "peerDependencies")]
    pub peer_dependencies: HashMap<String, String>,
    /// npm `optionalDependencies`. Deliberately NOT folded into
    /// [`Packages::resolve_dependencies`]: `merged_dependencies` drives the
    /// cloud dependency list and the synthetic type-check install, and an
    /// optional dependency is by definition allowed to be absent there. It is
    /// read only where "could this package be present at runtime?" is the
    /// question — see `crate::external_call_candidates`.
    #[serde(default)]
    #[serde(rename = "optionalDependencies")]
    pub optional_dependencies: HashMap<String, String>,
    /// yarn/pnpm `resolutions` (version-override map). Keys may be plain names
    /// or `name@range` selectors; values may be `npm:<real-name>@<range>`
    /// aliases that remap a locally-invented dependency name (e.g. MetaMask's
    /// `@types/readable-stream-2` → `npm:@types/readable-stream@^2.3.15`) to a
    /// real registry package. The synthetic type-check install must apply
    /// these aliases or the invented name 404s.
    #[serde(default)]
    pub resolutions: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    pub source_path: PathBuf,
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Packages {
    pub package_jsons: Vec<PackageJson>,
    pub source_paths: Vec<PathBuf>,
    pub merged_dependencies: HashMap<String, PackageInfo>,
    /// Package names declared by ANY package.json in the scanned repo tree —
    /// not just the service-scoped ones in `package_jsons`. A monorepo's
    /// shared workspace package (`@meridian/contracts` under
    /// `packages/contracts/`) is not a service, so its package.json is never
    /// loaded into `package_jsons`; this set is how it is still recognized as
    /// internal (registry-unresolvable). `default` for CloudRepoData
    /// payloads persisted before the field existed.
    #[serde(default)]
    pub internal_names: std::collections::HashSet<String>,
}

/// Directories excluded when walking a repo tree for manifests: dependency
/// installs and build output are not the project's own manifests. The walk
/// root itself is always traversed, even when its basename matches (a repo
/// legitimately named `build` is still a repo).
pub const MANIFEST_SKIP_DIRS: [&str; 4] = ["node_modules", "dist", "build", ".next"];

/// Names declared by every package.json under `repo_root` (workspace members
/// included), skipping dependency/build directories. Used to recognize
/// workspace-internal packages that must not be treated as registry deps.
pub fn collect_internal_package_names(
    repo_root: &std::path::Path,
) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let walker = walkdir::WalkDir::new(repo_root)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0
                || !(e.file_type().is_dir()
                    && e.file_name()
                        .to_str()
                        .is_some_and(|n| MANIFEST_SKIP_DIRS.contains(&n)))
        });
    for entry in walker.flatten() {
        if entry.file_type().is_file()
            && entry.file_name() == "package.json"
            && let Ok(text) = std::fs::read_to_string(entry.path())
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
            && let Some(name) = json.get("name").and_then(|n| n.as_str())
        {
            names.insert(name.to_string());
        }
    }
    names
}

impl Packages {
    /// Every package name this service's own `package.json` files declare, in
    /// any of the three dependency maps, deduplicated and sorted.
    ///
    /// Read straight off `package_jsons` rather than from
    /// `merged_dependencies`: that map is built through version parsing, and a
    /// name is a name whatever its version spec says (`workspace:*`,
    /// `catalog:`, a git URL). Callers that ask "does this service declare X?"
    /// must not be answered by the version resolver.
    pub fn declared_dependency_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .package_jsons
            .iter()
            .flat_map(|pkg| {
                pkg.dependencies
                    .keys()
                    .chain(pkg.dev_dependencies.keys())
                    .chain(pkg.peer_dependencies.keys())
            })
            .cloned()
            .collect();
        names.sort();
        names.dedup();
        names
    }

    pub fn new(package_json_paths: Vec<PathBuf>) -> Result<Self, io::Error> {
        let mut packages = Packages::default();

        for path in package_json_paths {
            let content = std::fs::read_to_string(&path)?;
            let package_json: PackageJson = serde_json::from_str(&content).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Failed to parse {}: {}", path.display(), e),
                )
            })?;

            packages.package_jsons.push(package_json);
            packages.source_paths.push(path);
        }

        packages.resolve_dependencies();
        Ok(packages)
    }

    /// Resolves dependencies across all package.json files, choosing the highest version for conflicts
    pub fn resolve_dependencies(&mut self) {
        for (idx, package_json) in self.package_jsons.iter().enumerate() {
            let source_path = &self.source_paths[idx];

            // Process all dependency types
            let all_deps = [
                &package_json.dependencies,
                &package_json.dev_dependencies,
                &package_json.peer_dependencies,
            ];

            for deps in all_deps {
                for (name, version_spec) in deps {
                    let clean_version = self.clean_version_spec(version_spec);

                    match self.merged_dependencies.get(name) {
                        Some(existing) => {
                            // Compare versions and keep the higher one
                            if self.should_update_version(&existing.version, &clean_version) {
                                self.merged_dependencies.insert(
                                    name.clone(),
                                    PackageInfo {
                                        name: name.clone(),
                                        version: clean_version,
                                        source_path: source_path.clone(),
                                    },
                                );
                            }
                        }
                        None => {
                            self.merged_dependencies.insert(
                                name.clone(),
                                PackageInfo {
                                    name: name.clone(),
                                    version: clean_version,
                                    source_path: source_path.clone(),
                                },
                            );
                        }
                    }
                }
            }
        }
    }

    /// Cleans version specifications to extract actual version numbers
    fn clean_version_spec(&self, version_spec: &str) -> String {
        // Remove common prefixes like ^, ~, >=, etc.
        let cleaned = version_spec
            .trim_start_matches('^')
            .trim_start_matches('~')
            .trim_start_matches(">=")
            .trim_start_matches("<=")
            .trim_start_matches('>')
            .trim_start_matches('<')
            .trim_start_matches('=');

        // Handle ranges like "1.0.0 - 2.0.0" by taking the higher version
        if let Some(_dash_pos) = cleaned.find(" - ") {
            let versions: Vec<&str> = cleaned.split(" - ").collect();
            if versions.len() == 2 {
                return versions[1].trim().to_string();
            }
        }

        // Handle "|| " separated versions by taking the first valid one
        if let Some(_or_pos) = cleaned.find(" || ") {
            let versions: Vec<&str> = cleaned.split(" || ").collect();
            if !versions.is_empty() {
                return versions[0].trim().to_string();
            }
        }

        cleaned.trim().to_string()
    }

    /// Determines if we should update to a new version (chooses higher version)
    fn should_update_version(&self, existing: &str, new: &str) -> bool {
        match (Version::parse(existing), Version::parse(new)) {
            (Ok(existing_ver), Ok(new_ver)) => new_ver > existing_ver,
            (Ok(_), Err(_)) => false, // Keep existing if new is invalid
            (Err(_), Ok(_)) => true,  // Use new if existing is invalid
            (Err(_), Err(_)) => {
                // Fallback to string comparison if both are invalid semver
                new > existing
            }
        }
    }

    /// Gets all merged dependencies
    pub fn get_dependencies(&self) -> &HashMap<String, PackageInfo> {
        &self.merged_dependencies
    }

    /// Merged dependency names cleaned for cloud requests: the cloud drops
    /// entries with whitespace or longer than 256 chars and caps the list at
    /// [`DEPENDENCY_NAME_CAP`], so filter here and send only well-formed
    /// names, sorted for determinism.
    pub fn cleaned_dependency_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .merged_dependencies
            .keys()
            .filter(|name| {
                !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_whitespace)
            })
            .cloned()
            .collect();
        names.sort();
        names.truncate(DEPENDENCY_NAME_CAP);
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_internal_package_names_walks_tree_and_skips_dep_dirs() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(
            repo.path().join("package.json"),
            r#"{ "name": "platform-monorepo" }"#,
        )
        .unwrap();
        std::fs::create_dir_all(repo.path().join("packages/contracts")).unwrap();
        std::fs::write(
            repo.path().join("packages/contracts/package.json"),
            r#"{ "name": "@meridian/contracts", "version": "0.1.0" }"#,
        )
        .unwrap();
        // Installed dependency — must NOT be treated as internal.
        std::fs::create_dir_all(repo.path().join("node_modules/koa")).unwrap();
        std::fs::write(
            repo.path().join("node_modules/koa/package.json"),
            r#"{ "name": "koa" }"#,
        )
        .unwrap();

        let names = collect_internal_package_names(repo.path());
        assert!(names.contains("platform-monorepo"));
        assert!(names.contains("@meridian/contracts"));
        assert!(
            !names.contains("koa"),
            "node_modules packages are not internal"
        );
    }

    /// Regression anchor on the real corpus-3 fixture: the workspace member
    /// that 404'd the type-check npm install must be recognized as internal.
    #[test]
    fn collect_internal_package_names_finds_corpus3_contracts_package() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/xrepo-corpus-3/platform-monorepo");
        let names = collect_internal_package_names(&fixture);
        assert!(names.contains("@meridian/contracts"));
        assert!(names.contains("catalog-api"));
    }
}
