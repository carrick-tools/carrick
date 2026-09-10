//! Deno configuration selection and the inputs that invalidate scanner caches.
//! The type sidecar owns module graph preparation and type resolution.
use crate::{
    config::Config,
    packages::{deno_workspace_manifest_paths, read_json_config},
};
use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    process::Command,
};

pub fn manifest_at(directory: &Path) -> Option<PathBuf> {
    ["deno.json", "deno.jsonc"]
        .into_iter()
        .map(|name| directory.join(name))
        .find(|p| p.is_file())
}

/// An explicit TypeScript config opts out of Deno discovery, as in the sidecar.
pub fn service_manifest(root: &Path, service: &Config) -> Option<PathBuf> {
    let directory = root.join(service.directory.as_deref().unwrap_or("."));
    if let Some(config) = &service.tsconfig {
        let path = directory.join(config);
        return path
            .file_name()
            .is_some_and(|name| name == "deno.json" || name == "deno.jsonc")
            .then_some(path);
    }
    let configs: Vec<_> = directory
        .ancestors()
        .take_while(|p| p.starts_with(root))
        .filter_map(manifest_at)
        .collect();
    let nearest = configs.first()?;
    if nearest.parent() != Some(directory.as_path()) && directory.join("package.json").is_file() {
        let declared = configs.iter().any(|config| {
            read_json_config(config)
                .ok()
                .and_then(|json| json.get("workspace").cloned())
                .and_then(|members| members.as_array().cloned())
                .is_some_and(|members| {
                    members
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .any(|member| {
                            let member_root = config.parent().unwrap().join(member);
                            match (directory.canonicalize(), member_root.canonicalize()) {
                                (Ok(directory), Ok(member_root)) => {
                                    directory.starts_with(member_root)
                                }
                                _ => false,
                            }
                        })
                })
        });
        if !declared {
            return None;
        }
    }
    Some(nearest.clone())
}

/// Run only the runtime version command, never application code or tasks.
pub fn require_runtime(root: &Path, services: &[Config]) -> Result<(), String> {
    if let Some(config) = services.iter().find_map(|s| service_manifest(root, s)) {
        let output = Command::new("deno").arg("--version").output();
        let supported = output.is_ok_and(|out| {
            out.status.success()
                && String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .next()
                    .and_then(|line| line.strip_prefix("deno "))
                    .and_then(|version| version.split_whitespace().next())
                    .and_then(|version| semver::Version::parse(version).ok())
                    .is_some_and(|version| version >= semver::Version::new(2, 9, 4))
        });
        if !supported {
            return Err(format!(
                "Deno is required to scan {}. Install or upgrade to Deno 2.9.4 or newer and make `deno` available on PATH. Carrick reads the existing Deno configuration; no generated tsconfig is required.",
                config.display()
            ));
        }
    }
    Ok(())
}

pub(crate) fn import_map_path(
    config: &Path,
    json: &serde_json::Value,
) -> io::Result<Option<PathBuf>> {
    let Some(value) = json.get("importMap") else {
        return Ok(None);
    };
    let Some(path) = value.as_str() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: importMap must be a local file path", config.display()),
        ));
    };
    if path.contains("://") || path.starts_with("data:") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{}: Carrick requires a local importMap file so dependency changes can invalidate its cache",
                config.display()
            ),
        ));
    }
    Ok(Some(config.parent().unwrap_or(Path::new(".")).join(path)))
}

/// Include inherited manifests and all declared workspace exports: a sibling's
/// export change can change a service's types without changing its own config.
pub fn resolution_inputs(root: &Path, manifests: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    if !manifests.iter().any(|p| {
        p.file_name()
            .is_some_and(|name| name == "deno.json" || name == "deno.jsonc")
    }) {
        return Ok(manifests.to_vec());
    }
    let mut configs: BTreeSet<PathBuf> = deno_workspace_manifest_paths(root)?.into_iter().collect();
    for manifest in manifests {
        for directory in manifest
            .parent()
            .into_iter()
            .flat_map(Path::ancestors)
            .take_while(|p| p.starts_with(root))
        {
            if let Some(config) = manifest_at(directory) {
                configs.insert(config);
            }
        }
    }
    let mut inputs: BTreeSet<PathBuf> = manifests.iter().cloned().collect();
    for config in configs {
        let json = read_json_config(&config)?;
        if let Some(map) = import_map_path(&config, &json)? {
            inputs.insert(map);
        }
        let lock = match json.get("lock") {
            Some(serde_json::Value::Bool(false)) => None,
            Some(serde_json::Value::String(path)) => Some(path.as_str()),
            Some(serde_json::Value::Object(lock)) => lock
                .get("path")
                .and_then(serde_json::Value::as_str)
                .or(Some("deno.lock")),
            _ => Some("deno.lock"),
        };
        if let Some(lock) = lock {
            inputs.insert(config.parent().unwrap().join(lock));
        }
        inputs.insert(config);
    }
    Ok(inputs.into_iter().collect())
}
