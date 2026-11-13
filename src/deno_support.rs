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
///
/// A Deno app nobody declared that sits in no Deno workspace
/// ([`optional_app`]) does not stop the run: the scan leaves it out when
/// Deno is missing (carrick#3969).
pub fn require_runtime(root: &Path, services: &[Config], declared: bool) -> Result<(), String> {
    let required: Vec<Config> = services
        .iter()
        .filter(|service| !optional_app(root, service, declared))
        .cloned()
        .collect();
    require_runtime_with(root, &required, installed_deno_version)
}

/// Whether `service` is a Deno app the scan may leave out when Deno is
/// missing (carrick#3969): nobody declared it in `carrick.json`, it is read
/// as Deno ([`service_manifest`]), and its config is in no Deno workspace
/// ([`deno_workspace_manifest_paths`]). The Deno app a Node workspace glob
/// holds is one; a Deno workspace and its members still need Deno to scan.
pub fn optional_app(root: &Path, service: &Config, declared: bool) -> bool {
    if declared {
        return false;
    }
    let Some(manifest) = service_manifest(root, service) else {
        return false;
    };
    let identity = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let manifest = identity(&manifest);
    !deno_workspace_manifest_paths(root)
        .unwrap_or_default()
        .iter()
        .any(|member| identity(member) == manifest)
}

/// Whether a `deno` that can scan a Deno service (2.9.4 or newer) answers
/// on PATH.
pub fn runtime_supported() -> bool {
    supported(installed_deno_version())
}

fn supported(installed: Option<semver::Version>) -> bool {
    installed.is_some_and(|version| version >= semver::Version::new(2, 9, 4))
}

/// [`require_runtime`] with the installed version supplied, so a test can say
/// "no Deno on PATH" without editing the process's PATH under every other
/// test that spawns a program. `installed` is asked only when a service is a
/// Deno one.
pub(crate) fn require_runtime_with(
    root: &Path,
    services: &[Config],
    installed: impl FnOnce() -> Option<semver::Version>,
) -> Result<(), String> {
    if let Some(config) = services.iter().find_map(|s| service_manifest(root, s))
        && !supported(installed())
    {
        return Err(format!(
            "Deno is required to scan {}. Install or upgrade to Deno 2.9.4 or newer and make `deno` available on PATH. Carrick reads the existing Deno configuration; no generated tsconfig is required.",
            config.display()
        ));
    }
    Ok(())
}

/// The `deno` on PATH, by its own `--version` line, or `None` when there is
/// none that answers.
fn installed_deno_version() -> Option<semver::Version> {
    let out = Command::new("deno").arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("deno "))
        .and_then(|version| version.split_whitespace().next())
        .and_then(|version| semver::Version::parse(version).ok())
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

/// The lockfile a Deno `config` names: its `lock` string or `lock.path`, else
/// `deno.lock` beside it. `None` when it says `"lock": false`.
pub(crate) fn lock_path(config: &Path, json: &serde_json::Value) -> Option<PathBuf> {
    let lock = match json.get("lock") {
        Some(serde_json::Value::Bool(false)) => return None,
        Some(serde_json::Value::String(path)) => path.as_str(),
        Some(serde_json::Value::Object(lock)) => lock
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("deno.lock"),
        _ => "deno.lock",
    };
    Some(config.parent().unwrap_or(Path::new(".")).join(lock))
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
        if let Some(lock) = lock_path(&config, &json) {
            inputs.insert(lock);
        }
        inputs.insert(config);
    }
    Ok(inputs.into_iter().collect())
}
