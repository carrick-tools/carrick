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
    /// The exact proposal init may create with exclusive-create semantics.
    pub config: serde_json::Value,
    pub warnings: Vec<String>,
}

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
            return Ok(ServiceDerivation {
                reason: "carrick.json".into(),
                config: serde_json::Value::Null,
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
    Ok(ServiceDerivation {
        reason: if reasons.is_empty() {
            "single repository".into()
        } else {
            reasons.join(" + ")
        },
        services,
        config,
        warnings,
    })
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
