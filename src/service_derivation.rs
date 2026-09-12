//! Service selection shared by CI, local indexing and the npm init preview.
//! Explicit configuration wins; inferred package boundaries remain editable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::packages::{MANIFEST_SKIP_DIRS, deno_workspace_manifest_paths, read_manifest};

#[derive(Debug, Serialize)]
pub struct ServiceDerivation {
    pub reason: String,
    pub services: Vec<Config>,
    /// One entry per service, in the same order, carrying what the member's
    /// own manifest says about it (carrick#994).
    pub members: Vec<MemberFacts>,
    /// The exact proposal init may create with exclusive-create semantics.
    pub config: serde_json::Value,
    pub warnings: Vec<String>,
}

/// The manifest facts that decide application from library.
///
/// Every workspace member is derived as a service here, and the whole
/// application-versus-library decision then sat in the scaffold instructions,
/// so the agent had to re-derive it by walking imports (carrick#994). These
/// are the facts that decision is actually made on, read from what the member
/// declares about itself and from what its siblings declare about it.
///
/// Every field is a statement, never a guess: `false` and `[]` mean the
/// manifest declares nothing, not that nothing is there. Field names are
/// snake_case, unlike the `carrick.json` keys they sit beside, because these
/// are derived facts rather than configuration anyone writes.
#[derive(Debug, Serialize, Clone, Default, PartialEq, Eq)]
pub struct MemberFacts {
    /// `private: true`: this package is not published.
    pub private: bool,
    /// A declared `bin`, which is an executable rather than an import target.
    pub bin: bool,
    /// A declared `main`.
    pub main: bool,
    /// A declared `exports`, the modern statement of an import surface.
    pub exports: bool,
    /// Deployment descriptors in the member's own directory, in a fixed order.
    pub deploy_config: Vec<String>,
    /// The other members that declare a dependency on this one, by the name
    /// this proposal gives them.
    pub workspace_dependents: Vec<String>,
}

impl ServiceDerivation {
    /// The `services` array of `carrick.derive/0`: each service's
    /// configuration and the facts about the member it came from, in one
    /// object per service.
    ///
    /// Additive, so the schema tag does not move. The facts are deliberately
    /// absent from `config`, which is the `carrick.json` skeleton someone
    /// writes: `private` and `bin` are things a repository states about
    /// itself, never configuration Carrick accepts.
    pub fn service_documents(&self) -> Vec<serde_json::Value> {
        #[derive(Serialize)]
        struct ServiceDocument<'a> {
            #[serde(flatten)]
            config: &'a Config,
            #[serde(flatten)]
            facts: &'a MemberFacts,
        }
        self.services
            .iter()
            .zip(self.members.iter())
            .map(|(config, facts)| {
                serde_json::to_value(ServiceDocument { config, facts })
                    .expect("a service and its facts both serialize as JSON objects")
            })
            .collect()
    }
}

/// Deployment descriptors, read only in the member's own directory: one of
/// these beside a package is the strongest statement in a repository that it
/// is deployed rather than imported.
const DEPLOY_CONFIG: [&str; 5] = [
    "netlify.toml",
    "vercel.json",
    "serverless.yml",
    "wrangler.toml",
    "Dockerfile",
];

#[derive(Deserialize)]
struct PnpmWorkspace {
    #[serde(default)]
    packages: Vec<String>,
}

/// Read the package-manager declarations without inventing a Node manifest.
pub fn workspace_patterns(root: &Path) -> Result<Vec<(String, Vec<String>)>, String> {
    let mut sources = Vec::new();
    let package = root.join("package.json");
    if package.try_exists().map_err(|e| e.to_string())? {
        let text = std::fs::read_to_string(&package).map_err(|e| e.to_string())?;
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("Failed to parse {}: {e}", package.display()))?;
        if let Some(workspaces) = value.get("workspaces") {
            let patterns = if workspaces.is_object() {
                workspaces.get("packages").ok_or_else(|| {
                    format!("{}: workspaces needs a packages array", package.display())
                })?
            } else {
                workspaces
            };
            let patterns: Vec<String> = serde_json::from_value(patterns.clone())
                .map_err(|e| format!("{}: invalid workspaces: {e}", package.display()))?;
            sources.push(("npm workspaces".to_string(), patterns));
        }
    }
    let pnpm = root.join("pnpm-workspace.yaml");
    if pnpm.try_exists().map_err(|e| e.to_string())? {
        let text = std::fs::read_to_string(&pnpm).map_err(|e| e.to_string())?;
        let workspace: PnpmWorkspace = serde_yaml_ng::from_str(&text)
            .map_err(|e| format!("Failed to parse {}: {e}", pnpm.display()))?;
        sources.push(("pnpm workspaces".to_string(), workspace.packages));
    }
    Ok(sources)
}

/// Resolve once, including the validation CI applies before scanning any files.
pub fn resolve(root: &Path) -> Result<ServiceDerivation, String> {
    let config_path = root.join("carrick.json");
    // symlink_metadata also sees broken symlinks: an existing path is never
    // mistaken for permission to infer and replace an explicit declaration.
    match std::fs::symlink_metadata(&config_path) {
        Ok(_) => {
            let services = Config::load_services(vec![config_path.clone()]).map_err(|e| {
                if e.kind() == std::io::ErrorKind::InvalidInput {
                    e.to_string()
                } else {
                    format!("Failed to parse {}: {e}", config_path.display())
                }
            })?;
            validate(root, &services)?;
            let members = member_facts(root, &services);
            return Ok(ServiceDerivation {
                reason: "carrick.json".into(),
                config: serde_json::Value::Null,
                members,
                services,
                warnings: Vec::new(),
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("Could not read {}: {e}", config_path.display())),
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let sources = workspace_patterns(&root)?;
    let mut members: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    let mut reasons = sources
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    if !sources.is_empty() {
        let options = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: true,
        };
        let mut positive = Vec::new();
        let mut negative = Vec::new();
        for (_, patterns) in &sources {
            for raw in patterns {
                let pattern = raw
                    .strip_prefix('!')
                    .unwrap_or(raw)
                    .trim_start_matches("./")
                    .trim_end_matches('/');
                if pattern.is_empty()
                    || Path::new(pattern).is_absolute()
                    || pattern.contains(['{', '}', '\\'])
                    || Path::new(pattern)
                        .components()
                        .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
                {
                    return Err(format!(
                        "Unsupported workspace pattern '{raw}'; use repository-relative glob paths or an explicit carrick.json."
                    ));
                }
                let compiled = glob::Pattern::new(pattern)
                    .map_err(|e| format!("Invalid workspace pattern '{raw}': {e}"))?;
                if raw.starts_with('!') {
                    negative.push(compiled);
                } else {
                    positive.push((raw, compiled, false));
                }
            }
        }
        let walker = walkdir::WalkDir::new(&root)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| {
                entry.depth() == 0
                    || !entry.file_type().is_dir()
                    || entry.file_name().to_str().is_some_and(|name| {
                        !name.starts_with('.')
                            && !MANIFEST_SKIP_DIRS.contains(&name)
                            && !["target", "out", "coverage"].contains(&name)
                    })
            });
        for entry in walker {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_type().is_file() || entry.file_name() != "package.json" {
                continue;
            }
            let directory = entry.path().parent().expect("manifest parent");
            let relative = directory.strip_prefix(&root).map_err(|e| e.to_string())?;
            let relative = if relative.as_os_str().is_empty() {
                Path::new(".")
            } else {
                relative
            };
            let mut matched = false;
            for (_, pattern, seen) in &mut positive {
                if pattern.matches_path_with(relative, options) {
                    *seen = true;
                    matched = true;
                }
            }
            if matched
                && !negative
                    .iter()
                    .any(|p| p.matches_path_with(relative, options))
            {
                members.insert(directory.to_path_buf(), entry.path().to_path_buf());
            }
        }
        for (raw, _, seen) in positive {
            if !seen {
                return Err(format!(
                    "Workspace pattern '{raw}' matched no package manifests; correct it or declare services in carrick.json."
                ));
            }
        }
    }
    let deno = deno_workspace_manifest_paths(&root).map_err(|e| e.to_string())?;
    let has_deno = !deno.is_empty();
    if has_deno {
        reasons.push("Deno manifests".into());
        for manifest in deno {
            let directory = manifest
                .parent()
                .expect("manifest parent")
                .canonicalize()
                .map_err(|e| e.to_string())?;
            let facts = read_manifest(&manifest).map_err(|e| e.to_string())?;
            // A workspace-only root does not constitute an extra service.
            if directory == root && !facts.workspace.is_empty() {
                continue;
            }
            members.entry(directory).or_insert(manifest);
        }
    }
    if members.is_empty() {
        if !sources.is_empty() && sources.iter().any(|(_, patterns)| !patterns.is_empty()) {
            return Err("Workspace patterns select no services; declare the intended services in carrick.json.".into());
        }
        members.insert(root.clone(), root.join("package.json"));
    }
    let mut names = BTreeSet::new();
    let mut services = Vec::new();
    for (directory, manifest) in members {
        let relative = directory.strip_prefix(&root).map_err(|e| e.to_string())?;
        let facts = if manifest.is_file() {
            Some(read_manifest(&manifest).map_err(|e| e.to_string())?)
        } else {
            None
        };
        // A plain root service is identified by its repo, as before. Package
        // names identify workspace members without changing root cache keys.
        let name = facts
            .and_then(|facts| facts.package.name)
            .filter(|_| !relative.as_os_str().is_empty())
            .or_else(|| {
                (!relative.as_os_str().is_empty())
                    .then(|| relative.to_string_lossy().replace('\\', "/"))
            });
        if let Some(name) = &name
            && !names.insert(name.clone())
        {
            return Err(format!(
                "Derived service name '{name}' is duplicated; declare unique names in carrick.json."
            ));
        }
        let tsconfig = if crate::deno_support::service_manifest(
            &root,
            &Config {
                directory: Some(relative.to_string_lossy().into_owned()),
                ..Config::default()
            },
        )
        .is_some()
        {
            None
        } else {
            nearest_tsconfig(&root, &directory)
        };
        services.push(Config {
            service_name: name,
            directory: (!relative.as_os_str().is_empty())
                .then(|| relative.to_string_lossy().replace('\\', "/")),
            tsconfig,
            ..Config::default()
        });
    }
    validate(&root, &services)?;
    let config = serde_json::json!({ "services": services });
    let mut warnings = Vec::new();
    if services.len() > 1 {
        warnings.push("Workspace packages are proposed as services. Review service boundaries and shared source includes in carrick.json.".into());
    }
    if has_deno {
        warnings.push("Deno services use their existing manifests and require Deno on PATH for type resolution.".into());
    }
    let members = member_facts(&root, &services);
    Ok(ServiceDerivation {
        reason: if reasons.is_empty() {
            "single repository".into()
        } else {
            reasons.join(" + ")
        },
        members,
        services,
        config,
        warnings,
    })
}

/// The manifest a service's own directory holds, npm's before Deno's.
fn member_manifest(root: &Path, service: &Config) -> Option<PathBuf> {
    let directory = root.join(service.directory.as_deref().unwrap_or("."));
    let package = directory.join("package.json");
    if package.is_file() {
        return Some(package);
    }
    crate::deno_support::manifest_at(&directory)
}

/// What this proposal calls a service, so a dependent names it the same way.
fn label(service: &Config) -> String {
    service
        .service_name
        .clone()
        .or_else(|| service.directory.clone())
        .unwrap_or_else(|| ".".to_string())
}

/// What every member declares about itself, and what its siblings declare
/// about it (carrick#994).
///
/// Manifests only. `derive` runs inside `carrick init`, before anything has
/// been scanned, so a dependency edge here is one a package manager already
/// records: a dependency entry naming another member's package, or a Deno
/// import-map target resolving inside another member's directory. Walking
/// imports would answer more and would make a first run pay for a parse it
/// has nowhere to put.
///
/// Returns one entry per service, in the same order.
pub fn member_facts(root: &Path, services: &[Config]) -> Vec<MemberFacts> {
    let manifests: Vec<Option<crate::packages::ManifestFacts>> = services
        .iter()
        .map(|service| member_manifest(root, service).and_then(|path| read_manifest(&path).ok()))
        .collect();
    let directories: Vec<PathBuf> = services
        .iter()
        .map(|service| {
            crate::workspace_resolver::normalize(
                &root.join(service.directory.as_deref().unwrap_or(".")),
            )
        })
        .collect();
    let mut by_package: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, manifest) in manifests.iter().enumerate() {
        if let Some(name) = manifest
            .as_ref()
            .and_then(|facts| facts.package.name.as_deref())
        {
            by_package.entry(name).or_insert(index);
        }
    }

    let mut facts: Vec<MemberFacts> = services
        .iter()
        .enumerate()
        .map(|(index, service)| {
            let manifest = manifests[index].as_ref();
            let directory = root.join(service.directory.as_deref().unwrap_or("."));
            MemberFacts {
                private: manifest.is_some_and(|facts| facts.private),
                bin: manifest.is_some_and(|facts| facts.bin),
                main: manifest.is_some_and(|facts| facts.main.is_some()),
                exports: manifest.is_some_and(|facts| facts.exports.is_some()),
                deploy_config: DEPLOY_CONFIG
                    .iter()
                    .filter(|name| directory.join(name).is_file())
                    .map(|name| (*name).to_string())
                    .collect(),
                workspace_dependents: Vec::new(),
            }
        })
        .collect();

    for (index, manifest) in manifests.iter().enumerate() {
        let Some(manifest) = manifest else { continue };
        let mut depends_on: BTreeSet<usize> = BTreeSet::new();
        let package = &manifest.package;
        for name in package
            .dependencies
            .keys()
            .chain(package.dev_dependencies.keys())
            .chain(package.peer_dependencies.keys())
            .chain(package.optional_dependencies.keys())
        {
            if let Some(&other) = by_package.get(name.as_str()) {
                depends_on.insert(other);
            }
        }
        for target in &manifest.local_imports {
            let resolved =
                crate::workspace_resolver::normalize(&directories[index].join(target.as_str()));
            // The deepest member the target sits in, so a package inside
            // another package's directory takes its own import.
            if let Some((other, _)) = directories
                .iter()
                .enumerate()
                .filter(|(_, directory)| resolved.starts_with(directory))
                .max_by_key(|(_, directory)| directory.as_os_str().len())
            {
                depends_on.insert(other);
            }
        }
        depends_on.remove(&index);
        for other in depends_on {
            facts[other]
                .workspace_dependents
                .push(label(&services[index]));
        }
    }
    for entry in &mut facts {
        entry.workspace_dependents.sort();
        entry.workspace_dependents.dedup();
    }
    facts
}

fn nearest_tsconfig(root: &Path, directory: &Path) -> Option<String> {
    let mut prefix = PathBuf::new();
    for ancestor in directory.ancestors() {
        if !ancestor.starts_with(root) {
            break;
        }
        if ancestor.join("tsconfig.json").is_file() {
            return Some(
                prefix
                    .join("tsconfig.json")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
        prefix.push("..");
    }
    None
}

fn validate(root: &Path, services: &[Config]) -> Result<(), String> {
    for service in services {
        let label = service
            .service_name
            .as_deref()
            .or(service.directory.as_deref())
            .unwrap_or("<unnamed>");
        if let Some(directory) = &service.directory
            && !root.join(directory).is_dir()
        {
            return Err(format!(
                "Service '{label}' in {} declares directory '{directory}', which does not exist under '{}'",
                root.join("carrick.json").display(),
                root.display()
            ));
        }
        for include in &service.include {
            if !root.join(include).exists() {
                return Err(format!(
                    "Service '{label}' declares include path '{include}', which does not exist under '{}'",
                    root.display()
                ));
            }
        }
        if let Some(tsconfig) = &service.tsconfig {
            let path = root
                .join(service.directory.as_deref().unwrap_or("."))
                .join(tsconfig);
            if !path.is_file() {
                return Err(format!(
                    "Service '{label}' declares tsconfig '{}', which does not exist",
                    path.display()
                ));
            }
            if path
                .file_name()
                .is_some_and(|name| name == "deno.json" || name == "deno.jsonc")
            {
                let directory = root.join(service.directory.as_deref().unwrap_or("."));
                let nearest = directory
                    .ancestors()
                    .take_while(|p| p.starts_with(root))
                    .find_map(crate::deno_support::manifest_at);
                let selected = path.canonicalize().map_err(|e| e.to_string())?;
                if !nearest.is_some_and(|nearest| {
                    nearest.file_name() == path.file_name()
                        && nearest.canonicalize().ok().as_ref() == Some(&selected)
                }) {
                    return Err(format!(
                        "Service '{label}' must select its nearest Deno manifest (deno.json takes precedence over deno.jsonc). Alternate Deno config '{}' is not supported; omit tsconfig to use the service manifest, or select an ordinary TypeScript config.",
                        path.display()
                    ));
                }
            }
        }
    }
    Ok(())
}
