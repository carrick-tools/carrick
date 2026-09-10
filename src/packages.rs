use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
};

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

/// The manifest facts shared by package loading and workspace resolution.
/// Deno spells dependencies as import-map targets; normalizing them here keeps
/// every scanner pass on the same identity rules.
#[derive(Debug, Clone)]
pub struct ManifestFacts {
    pub package: PackageJson,
    pub main: Option<String>,
    pub exports: Option<serde_json::Value>,
    /// Deno workspace member directories, exactly as declared by this config.
    pub workspace: Vec<String>,
}

pub(crate) fn read_json_config(path: &Path) -> Result<serde_json::Value, io::Error> {
    let content = std::fs::read_to_string(path)?;
    let json_text = if path.extension().is_some_and(|ext| ext == "jsonc") {
        strip_jsonc_syntax(&content).map_err(|message| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to parse {}: {}", path.display(), message),
            )
        })?
    } else {
        content
    };
    let json: serde_json::Value = serde_json::from_str(&json_text).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Failed to parse {}: {}", path.display(), e),
        )
    })?;

    Ok(json)
}

pub fn read_manifest(path: &Path) -> Result<ManifestFacts, io::Error> {
    let json = read_json_config(path)?;

    if path.file_name().is_some_and(|name| name == "package.json") {
        let package = serde_json::from_value(json.clone()).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to parse {}: {}", path.display(), e),
            )
        })?;
        return Ok(ManifestFacts {
            package,
            main: json
                .get("main")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            exports: json.get("exports").cloned(),
            workspace: Vec::new(),
        });
    }

    let mut dependencies = HashMap::new();
    let external_map = crate::deno_support::import_map_path(path, &json)?
        .map(|path| read_json_config(&path))
        .transpose()?;
    for map in std::iter::once(&json).chain(external_map.as_ref()) {
        let imports = map.get("imports").and_then(serde_json::Value::as_object);
        let scopes = map.get("scopes").and_then(serde_json::Value::as_object);
        for entries in imports.into_iter().chain(
            scopes
                .into_iter()
                .flat_map(|scopes| scopes.values().filter_map(serde_json::Value::as_object)),
        ) {
            for target in entries.values().filter_map(serde_json::Value::as_str) {
                if let Some((name, spec)) = deno_registry_identity(target) {
                    dependencies.entry(name).or_insert(spec);
                }
            }
        }
    }
    let package = PackageJson {
        name: json
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        version: json
            .get("version")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        dependencies,
        dev_dependencies: HashMap::new(),
        peer_dependencies: HashMap::new(),
        optional_dependencies: HashMap::new(),
        resolutions: HashMap::new(),
    };
    let workspace = match json.get("workspace") {
        None => Vec::new(),
        Some(serde_json::Value::Array(members)) => members
            .iter()
            .enumerate()
            .map(|(index, member)| {
                member.as_str().map(str::to_owned).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "Failed to parse {}: workspace member {} must be a string",
                            path.display(),
                            index
                        ),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Failed to parse {}: workspace must be an array of strings",
                    path.display()
                ),
            ));
        }
    };
    Ok(ManifestFacts {
        package,
        main: None,
        exports: json.get("exports").cloned(),
        workspace,
    })
}

fn deno_registry_identity(target: &str) -> Option<(String, String)> {
    let specifier = target
        .strip_prefix("npm:")
        .or_else(|| target.strip_prefix("jsr:"))?;
    let package_end = if specifier.starts_with('@') {
        specifier
            .find('/')
            .and_then(|slash| {
                specifier[slash + 1..]
                    .find('/')
                    .map(|tail| slash + 1 + tail)
            })
            .unwrap_or(specifier.len())
    } else {
        specifier.find('/').unwrap_or(specifier.len())
    };
    let package_and_version = &specifier[..package_end];
    let version_at = package_and_version
        .char_indices()
        .filter_map(|(index, ch)| (index > 0 && ch == '@').then_some(index))
        .next_back();
    let (name, spec) = version_at.map_or((package_and_version, "*"), |at| {
        (&package_and_version[..at], &package_and_version[at + 1..])
    });
    let valid_name = !name.is_empty()
        && name.is_ascii()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'@' | b'/' | b'-' | b'_' | b'.')
        })
        && (!name.starts_with('@')
            || name
                .strip_prefix('@')
                .and_then(|scoped| scoped.split_once('/'))
                .is_some_and(|(scope, package)| !scope.is_empty() && !package.is_empty()));
    valid_name.then(|| {
        (
            name.to_string(),
            if spec.is_empty() {
                "*".to_string()
            } else {
                spec.to_string()
            },
        )
    })
}

/// Remove JSONC comments without touching comment markers inside strings.
fn strip_jsonc_syntax(input: &str) -> Result<String, &'static str> {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else if ch == '"' {
            in_string = true;
            out.push(ch);
        } else if ch == '/' && chars.peek() == Some(&'/') {
            out.push(' ');
            chars.next();
            for comment in chars.by_ref() {
                if comment == '\n' {
                    out.push('\n');
                    break;
                }
            }
        } else if ch == '/' && chars.peek() == Some(&'*') {
            out.push(' ');
            chars.next();
            let mut previous = '\0';
            let mut closed = false;
            for comment in chars.by_ref() {
                if comment == '\n' {
                    out.push('\n');
                }
                if previous == '*' && comment == '/' {
                    closed = true;
                    break;
                }
                previous = comment;
            }
            if !closed {
                return Err("unterminated block comment");
            }
        } else {
            out.push(ch);
        }
    }
    let mut normalized = String::with_capacity(out.len());
    let mut chars = out.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(ch) = chars.next() {
        if in_string {
            normalized.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            normalized.push(ch);
            continue;
        }
        if ch == ',' {
            let mut lookahead = chars.clone();
            if lookahead
                .find(|next| !next.is_whitespace())
                .is_some_and(|next| matches!(next, '}' | ']'))
            {
                continue;
            }
        }
        normalized.push(ch);
    }
    Ok(normalized)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    pub name: String,
    /// The cleaned version: range operators stripped and ranges collapsed to a
    /// single version (see `clean_version_spec`). Lossy by design — `^3.0.0`
    /// and a `3.0.0` pin both clean to `3.0.0`.
    pub version: String,
    /// The dependency specifier exactly as written in package.json (`^3.0.0`,
    /// `~1.2.3`, `>=2 <3`, `workspace:*`, a git URL). The cloud uses this raw
    /// form for semver-range conflict analysis, which `version` cannot answer
    /// because the operators are already gone by then. `default` for payloads
    /// serialised before the field existed.
    #[serde(default)]
    pub spec: String,
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
pub const MANIFEST_SKIP_DIRS: [&str; 6] = [
    "node_modules",
    "dist",
    "build",
    ".next",
    ".vite",
    ".carrick",
];

/// Deno configs that belong to the declared workspace, starting at the root.
/// Nested configs outside a `workspace` list are intentionally absent.
pub fn deno_workspace_manifest_paths(repo_root: &Path) -> Result<Vec<PathBuf>, io::Error> {
    let canonical_root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let mut pending = deno_manifest_at(repo_root).into_iter().collect::<Vec<_>>();
    let mut seen = std::collections::BTreeSet::new();
    let mut manifests = Vec::new();
    while let Some(manifest) = pending.pop() {
        let identity = manifest.canonicalize().unwrap_or_else(|_| manifest.clone());
        if !seen.insert(identity) {
            continue;
        }
        manifests.push(manifest.clone());
        let facts = read_manifest(&manifest)?;
        let declaring_dir = manifest.parent().unwrap_or(repo_root);
        for member in facts.workspace {
            let member_dir = declaring_dir.join(&member);
            let canonical_member = member_dir.canonicalize().map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "Deno workspace member '{}' declared by {} cannot be read: {}",
                        member,
                        manifest.display(),
                        e
                    ),
                )
            })?;
            if canonical_member.strip_prefix(&canonical_root).is_err() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "Deno workspace member '{}' declared by {} is outside the repository",
                        member,
                        manifest.display()
                    ),
                ));
            }
            let member_manifest = deno_manifest_at(&member_dir).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "Deno workspace member '{}' declared by {} has no deno.json or deno.jsonc",
                        member,
                        manifest.display()
                    ),
                )
            })?;
            pending.push(member_manifest);
        }
    }
    manifests.sort();
    Ok(manifests)
}

fn deno_manifest_at(dir: &Path) -> Option<PathBuf> {
    ["deno.json", "deno.jsonc"]
        .into_iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

/// Names declared by every package.json under `repo_root` (workspace members
/// included), skipping dependency/build directories. Used to recognize
/// workspace-internal packages that must not be treated as registry deps.
pub fn collect_internal_package_names(
    repo_root: &std::path::Path,
) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let walker = walkdir::WalkDir::new(repo_root)
        .sort_by_file_name()
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
        .filter(|entry| entry.file_type().is_file() && entry.file_name() == "package.json")
        .map(|entry| entry.path().to_path_buf())
        .collect();
    match deno_workspace_manifest_paths(repo_root) {
        Ok(deno_manifests) => manifests.extend(deno_manifests),
        Err(error) => tracing::warn!("Ignoring Deno workspace manifests: {}", error),
    }
    for manifest in manifests {
        if let Ok(facts) = read_manifest(&manifest)
            && let Some(name) = facts.package.name
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
            let package_json = read_manifest(&path)?.package;

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
                                        spec: version_spec.clone(),
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
                                    spec: version_spec.clone(),
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
    fn deno_jsonc_normalizes_registry_imports_and_keeps_workspace_facts() {
        let repo = tempfile::tempdir().unwrap();
        let manifest = repo.path().join("deno.jsonc");
        std::fs::write(
            &manifest,
            r#"{
            // aliases are not package identities
            "name": "@local/api",
            "workspace": ["./packages/core"],
            "exports": "./mod.ts",
            "imports": {
                "client": "npm:@vendor/client@^2.1.0/http",
                "utils": "jsr:@scope/utils@1.4.0/path",
                "local": "./src/local.ts",
                "url": "https://example.invalid/mod.ts",
            },
        }"#,
        )
        .unwrap();

        let facts = read_manifest(&manifest).unwrap();
        assert_eq!(facts.package.name.as_deref(), Some("@local/api"));
        assert_eq!(facts.workspace, vec!["./packages/core"]);
        assert_eq!(facts.exports, Some(serde_json::json!("./mod.ts")));
        assert_eq!(
            facts
                .package
                .dependencies
                .get("@vendor/client")
                .map(String::as_str),
            Some("^2.1.0")
        );
        assert_eq!(
            facts
                .package
                .dependencies
                .get("@scope/utils")
                .map(String::as_str),
            Some("1.4.0")
        );
        assert_eq!(facts.package.dependencies.len(), 2);
    }

    #[test]
    fn deno_registry_identity_is_total_for_scoped_unscoped_and_malformed_targets() {
        assert_eq!(
            deno_registry_identity("npm:client@2.0.0/http"),
            Some(("client".into(), "2.0.0".into()))
        );
        assert_eq!(
            deno_registry_identity("jsr:@scope/client@^2/http"),
            Some(("@scope/client".into(), "^2".into()))
        );
        assert_eq!(
            deno_registry_identity("npm:client"),
            Some(("client".into(), "*".into()))
        );
        for target in ["npm:", "jsr:", "npm:/subpath", "npm:🦕", "npm:@scope"] {
            let _ = deno_registry_identity(target);
        }
        assert_eq!(deno_registry_identity("npm:"), None);
        assert_eq!(deno_registry_identity("jsr:"), None);
        assert_eq!(deno_registry_identity("npm:🦕"), None);
        assert_eq!(deno_registry_identity("npm:@scope"), None);
    }

    #[test]
    fn jsonc_comments_preserve_token_boundaries_and_must_close() {
        assert!(
            serde_json::from_str::<serde_json::Value>(
                &strip_jsonc_syntax("[1/* x */,2,]").unwrap()
            )
            .is_ok()
        );
        assert!(
            serde_json::from_str::<serde_json::Value>(&strip_jsonc_syntax("[1/* x */2]").unwrap())
                .is_err()
        );
        assert_eq!(
            strip_jsonc_syntax("{/* never closes"),
            Err("unterminated block comment")
        );
    }

    #[test]
    fn deno_workspace_members_are_declared_and_missing_members_are_errors() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("listed")).unwrap();
        std::fs::create_dir_all(repo.path().join("unlisted")).unwrap();
        std::fs::write(repo.path().join("deno.json"), r#"{"workspace":["listed"]}"#).unwrap();
        std::fs::write(repo.path().join("listed/deno.json"), r#"{"name":"listed"}"#).unwrap();
        std::fs::write(
            repo.path().join("unlisted/deno.json"),
            r#"{"name":"unlisted"}"#,
        )
        .unwrap();

        let paths = deno_workspace_manifest_paths(repo.path()).unwrap();
        assert_eq!(
            paths,
            vec![
                repo.path().join("deno.json"),
                repo.path().join("listed/deno.json")
            ]
        );

        std::fs::write(
            repo.path().join("deno.json"),
            r#"{"workspace":["missing"]}"#,
        )
        .unwrap();
        let error = deno_workspace_manifest_paths(repo.path()).unwrap_err();
        assert!(error.to_string().contains("cannot be read"));
    }

    #[test]
    fn deno_workspace_rejects_wrong_member_shapes_and_terminates_cycles() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("child")).unwrap();
        std::fs::write(repo.path().join("deno.json"), r#"{"workspace":["child"]}"#).unwrap();
        std::fs::write(
            repo.path().join("child/deno.json"),
            r#"{"workspace":[".."]}"#,
        )
        .unwrap();
        assert_eq!(deno_workspace_manifest_paths(repo.path()).unwrap().len(), 2);

        for malformed in [r#"{"workspace":"child"}"#, r#"{"workspace":["child",7]}"#] {
            std::fs::write(repo.path().join("deno.json"), malformed).unwrap();
            assert!(
                deno_workspace_manifest_paths(repo.path())
                    .unwrap_err()
                    .to_string()
                    .contains("workspace")
            );
        }
    }

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

        std::fs::create_dir_all(repo.path().join("packages/contracts/.vite/deps")).unwrap();
        std::fs::write(
            repo.path()
                .join("packages/contracts/.vite/deps/package.json"),
            r#"{"name":"cached-vendor"}"#,
        )
        .unwrap();

        let names = collect_internal_package_names(repo.path());
        assert!(!names.contains("cached-vendor"));
        assert!(names.contains("platform-monorepo"));
        assert!(names.contains("@meridian/contracts"));
        assert!(
            !names.contains("koa"),
            "node_modules packages are not internal"
        );
    }

    /// `version` is the lossy cleaned form; `spec` must carry the declaration
    /// byte-for-byte so the cloud can tell a caret range from a pin. Expected
    /// versions are hardcoded rather than re-derived from `clean_version_spec`,
    /// so a regression in the cleaning is caught here too.
    #[test]
    fn resolve_dependencies_keeps_the_raw_spec_beside_the_cleaned_version() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("package.json");
        std::fs::write(
            &manifest,
            r#"{
                "name": "svc",
                "dependencies": {
                    "a": "^3.0.0",
                    "b": "~1.2.3",
                    "c": "1.0.0 - 2.0.0",
                    "d": "workspace:*"
                }
            }"#,
        )
        .unwrap();

        let packages = Packages::new(vec![manifest]).unwrap();
        let expected = [
            ("a", "3.0.0", "^3.0.0"),
            ("b", "1.2.3", "~1.2.3"),
            ("c", "2.0.0", "1.0.0 - 2.0.0"),
            ("d", "workspace:*", "workspace:*"),
        ];
        for (name, version, spec) in expected {
            let info = packages
                .merged_dependencies
                .get(name)
                .unwrap_or_else(|| panic!("{name} missing from merged_dependencies"));
            assert_eq!(info.version, version, "cleaned version for {name}");
            assert_eq!(info.spec, spec, "raw spec for {name}");
        }
    }

    /// When two manifests declare the same package the higher version wins —
    /// and `spec` must follow the winner, in both file orders (the update
    /// branch and the keep-existing branch).
    #[test]
    fn resolve_dependencies_keeps_the_winning_manifests_spec() {
        let dir = tempfile::tempdir().unwrap();
        let low = dir.path().join("low/package.json");
        let high = dir.path().join("high/package.json");
        std::fs::create_dir_all(low.parent().unwrap()).unwrap();
        std::fs::create_dir_all(high.parent().unwrap()).unwrap();
        std::fs::write(
            &low,
            r#"{ "name": "low", "dependencies": { "lodash": "^4.17.20" } }"#,
        )
        .unwrap();
        std::fs::write(
            &high,
            r#"{ "name": "high", "dependencies": { "lodash": "~4.17.30" } }"#,
        )
        .unwrap();

        for paths in [
            vec![low.clone(), high.clone()],
            vec![high.clone(), low.clone()],
        ] {
            let packages = Packages::new(paths.clone()).unwrap();
            let info = packages.merged_dependencies.get("lodash").unwrap();
            assert_eq!(info.version, "4.17.30", "order {paths:?}");
            assert_eq!(info.spec, "~4.17.30", "order {paths:?}");
            assert_eq!(info.source_path, high, "order {paths:?}");
        }
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
