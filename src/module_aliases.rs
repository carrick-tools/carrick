//! The import aliases a repo's own config files declare (carrick#1104).
//!
//! A specifier like `@/slots/availability.ts` is neither relative nor a
//! package name. Which file it names is stated by config, and this module
//! reads that config into plain data:
//!
//! - tsconfig / jsconfig `compilerOptions.paths` and `baseUrl`, through the
//!   `extends` chain (Bun reads the same fields);
//! - package.json `imports` (`#subpath` keys, with conditions and one `*`);
//! - Deno `imports` and `scopes`, in `deno.json(c)` or the `importMap` file a
//!   config names, for the workspace root and every member.
//!
//! It answers "what path does this config map the specifier to", never "does
//! the file exist": probing the candidates against the tree is
//! [`crate::workspace_resolver::WorkspaceIndex`]'s job, so there is one
//! extension and index-file rule for every kind of specifier.
//!
//! An alias defined only in code (a bundler's `resolve.alias`, a Babel plugin
//! option) is not config and is not read. The call graph counts the imports it
//! could not resolve instead of dropping them silently; see
//! `docs/reference/module-resolution.md` for the precedence rules, the
//! compiler-parity check, and the limits.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::packages::read_jsonc_config;
use crate::workspace_resolver::normalize;

/// How many `extends` hops are followed before a chain is treated as a cycle.
const MAX_EXTENDS_DEPTH: usize = 16;

/// Config file names read here, in the order a directory is checked for the
/// config that governs it. `tsconfig.json` shadows `jsconfig.json` in one
/// directory, as it does for the TypeScript language service.
pub(crate) const TS_CONFIG_NAMES: [&str; 2] = ["tsconfig.json", "jsconfig.json"];

/// A tsconfig `paths` map in declaration order: pattern -> its targets.
pub(crate) type PathsMap = Vec<(String, Vec<String>)>;

/// What one tsconfig states about non-relative specifiers, after its `extends`
/// chain is applied. Every path is repo-relative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TsPathMapping {
    /// The resolved `baseUrl`, relative to the config that declared it.
    pub base_url: Option<PathBuf>,
    /// The `paths` map of the nearest config in the chain that sets one. The
    /// map is replaced whole by a nearer layer, never merged.
    pub paths: PathsMap,
    /// What `paths` targets resolve against: `baseUrl` when one is set,
    /// otherwise the directory of the config that declared `paths`.
    pub paths_base: PathBuf,
}

/// One Deno import map, scoped to the files it applies to.
#[derive(Debug, Clone)]
struct DenoImportMap {
    /// Importers under this path use the map.
    scope: PathBuf,
    /// Targets resolve against this directory: the declaring config's, or the
    /// external import map file's.
    base: PathBuf,
    /// Key -> its local-path target, or `None` for a registry or URL target.
    /// Kept rather than dropped: a key that maps to a package still decides
    /// the lookup, it just names no file in this repo.
    entries: BTreeMap<String, Option<String>>,
    /// A `scopes` block, which Deno consults before the plain `imports`.
    scoped: bool,
}

/// The config-declared aliases of one repo.
#[derive(Debug, Clone, Default)]
pub(crate) struct ModuleAliases {
    /// Directory -> the mapping of the tsconfig/jsconfig in it (`None` when
    /// the config and its chain set neither `paths` nor `baseUrl`, which still
    /// governs the directory and stops the walk up).
    ts_configs: BTreeMap<PathBuf, Option<TsPathMapping>>,
    /// The service's `carrick.json` `tsconfig`, governing every file under the
    /// service directory.
    service_ts: Option<(PathBuf, Option<TsPathMapping>)>,
    /// package.json directory -> its `imports` field.
    package_imports: BTreeMap<PathBuf, Option<serde_json::Value>>,
    /// Deno import maps, most specific scope first.
    deno_maps: Vec<DenoImportMap>,
    /// `extends` values that named no config on disk, so the chain stopped
    /// there. Reported by the caller as a limit.
    unfollowed_extends: BTreeSet<String>,
}

/// A config-stated answer for one specifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AliasTarget {
    /// Repo-relative paths to probe, first existing file wins.
    Paths(Vec<PathBuf>),
    /// A conditions object from package.json `imports`: every leaf is a
    /// spelling of the same module, relative to `dir`, with the `*`
    /// substitution already applied.
    Leaves { dir: PathBuf, leaves: Vec<String> },
}

/// One config answer, and the key that gave it (carrick#1273).
///
/// The key is what a reader recognises: it is the line they wrote in their own
/// config, so a target that is not on disk can be reported against the mapping
/// that claims it rather than against each of the specifiers that went through
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AliasMatch {
    /// The key as written — an import-map key, a package.json `imports` key,
    /// or a tsconfig `paths` pattern.
    ///
    /// `None` for the `baseUrl` fallback, which declares no key: `baseUrl` is
    /// a search root rather than a claim that any particular path exists, so a
    /// specifier that misses under it was never claimed by anything and must
    /// not be reported as a mapping whose target is missing.
    pub declared_by: Option<String>,
    pub target: AliasTarget,
}

impl ModuleAliases {
    /// Read every config the tree holds. `configs` is the repo-relative list
    /// of `package.json`, `deno.json(c)`, `tsconfig.json` and `jsconfig.json`
    /// files the manifest walk found; `service_tsconfig` is the service
    /// directory and the config `carrick.json` names for it, both
    /// repo-relative.
    pub(crate) fn build(
        repo_root: &Path,
        configs: &[PathBuf],
        internal_package_dirs: &BTreeMap<String, PathBuf>,
        service_tsconfig: Option<(&Path, &Path)>,
    ) -> Self {
        let mut aliases = ModuleAliases::default();
        let mut ts_dirs: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
        for config in configs {
            let Some(name) = config.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let dir = config.parent().map(Path::to_path_buf).unwrap_or_default();
            match name {
                "package.json" => {
                    let imports = read_jsonc_config(&repo_root.join(config))
                        .ok()
                        .and_then(|json| json.get("imports").cloned());
                    aliases.package_imports.insert(dir, imports);
                }
                "deno.json" | "deno.jsonc" => aliases.read_deno_config(repo_root, config),
                _ if TS_CONFIG_NAMES.contains(&name) => {
                    // tsconfig.json wins over jsconfig.json in one directory.
                    match ts_dirs.get(&dir) {
                        Some(existing)
                            if existing.file_name() == Some("tsconfig.json".as_ref()) => {}
                        _ => {
                            ts_dirs.insert(dir, config.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        for (dir, config) in ts_dirs {
            let mapping = aliases.ts_mapping(repo_root, &config, internal_package_dirs);
            aliases.ts_configs.insert(dir, mapping);
        }
        if let Some((service_dir, tsconfig)) = service_tsconfig {
            let config = normalize(&service_dir.join(tsconfig));
            if repo_root.join(&config).is_file() {
                let mapping = aliases.ts_mapping(repo_root, &config, internal_package_dirs);
                aliases.service_ts = Some((service_dir.to_path_buf(), mapping));
            }
        }
        // Deepest scope first; within one scope a `scopes` block before the
        // plain `imports`; then by path so the order is a property of the tree.
        aliases.deno_maps.sort_by(|a, b| {
            b.scope
                .components()
                .count()
                .cmp(&a.scope.components().count())
                .then(b.scoped.cmp(&a.scoped))
                .then(a.scope.cmp(&b.scope))
                .then(a.base.cmp(&b.base))
        });
        aliases
    }

    /// `extends` values the chain could not follow, sorted.
    pub(crate) fn unfollowed_extends(&self) -> impl Iterator<Item = &String> {
        self.unfollowed_extends.iter()
    }

    /// What config maps `specifier` to, as written in `from_file`
    /// (repo-relative). Deno import maps first, because in a Deno project they
    /// are the resolver; then package.json `imports` for a `#` specifier; then
    /// tsconfig `paths`, then `baseUrl`. `None` when no config maps it to a
    /// path in this repo.
    ///
    /// The first config that matches decides for the first two: an import map
    /// entry that names a registry package, or a `#` key whose target does not
    /// exist, ends the lookup. A `paths` pattern whose targets all miss falls
    /// through to `baseUrl`, which is what the compiler does.
    pub(crate) fn resolve(&self, from_file: &Path, specifier: &str) -> Vec<AliasMatch> {
        if let Some((key, matched)) = self
            .deno_maps
            .iter()
            .filter(|map| from_file.starts_with(&map.scope))
            .find_map(|map| match_import_map(map, specifier))
        {
            return matched
                .map(|path| AliasMatch {
                    declared_by: Some(key.clone()),
                    target: AliasTarget::Paths(vec![path]),
                })
                .into_iter()
                .collect();
        }

        if specifier.starts_with('#') {
            return self
                .package_import(from_file, specifier)
                .into_iter()
                .collect();
        }

        let mapping = match &self.service_ts {
            Some((dir, mapping)) if from_file.starts_with(dir) => mapping.as_ref(),
            _ => self.nearest_ts_mapping(from_file),
        };
        let Some(mapping) = mapping else {
            return Vec::new();
        };
        let mut targets = Vec::new();
        if let Some((pattern, substitution)) =
            match_pattern(mapping.paths.iter().map(|(p, _)| p.as_str()), specifier)
        {
            let candidates = mapping
                .paths
                .iter()
                .find(|(p, _)| p == pattern)
                .map(|(_, targets)| targets.as_slice())
                .unwrap_or_default()
                .iter()
                .map(|target| {
                    normalize(
                        &mapping
                            .paths_base
                            .join(target.replacen('*', &substitution, 1)),
                    )
                })
                .collect();
            targets.push(AliasMatch {
                declared_by: Some(pattern.to_string()),
                target: AliasTarget::Paths(candidates),
            });
        }
        if let Some(base_url) = &mapping.base_url {
            targets.push(AliasMatch {
                declared_by: None,
                target: AliasTarget::Paths(vec![normalize(&base_url.join(specifier))]),
            });
        }
        targets
    }

    /// The governing tsconfig mapping: the nearest directory, from the
    /// importer's own up to the repo root, that holds a tsconfig or jsconfig.
    fn nearest_ts_mapping(&self, from_file: &Path) -> Option<&TsPathMapping> {
        from_file
            .ancestors()
            .skip(1)
            .find_map(|dir| self.ts_configs.get(dir))
            .and_then(Option::as_ref)
    }

    /// Node's `PACKAGE_IMPORTS_RESOLVE`: the importer's nearest package.json
    /// alone decides, whether or not it declares `imports`.
    fn package_import(&self, from_file: &Path, specifier: &str) -> Option<AliasMatch> {
        let (dir, imports) = from_file
            .ancestors()
            .skip(1)
            .find_map(|dir| self.package_imports.get(dir).map(|imports| (dir, imports)))?;
        let map = imports.as_ref()?.as_object()?;
        let (key, substitution) = match_pattern(map.keys().map(String::as_str), specifier)?;
        let mut leaves = Vec::new();
        collect_string_leaves(&map[key], 0, &mut leaves);
        let leaves = leaves
            .into_iter()
            // A bare target names a package, not a file in this directory.
            .filter(|leaf| leaf.starts_with("./"))
            .map(|leaf| leaf.replace('*', &substitution))
            .collect::<Vec<_>>();
        (!leaves.is_empty()).then(|| AliasMatch {
            declared_by: Some(key.to_string()),
            target: AliasTarget::Leaves {
                dir: dir.to_path_buf(),
                leaves,
            },
        })
    }

    fn read_deno_config(&mut self, repo_root: &Path, config: &Path) {
        let Ok(json) = read_jsonc_config(&repo_root.join(config)) else {
            return;
        };
        let dir = config.parent().map(Path::to_path_buf).unwrap_or_default();
        self.add_import_map(&json, &dir, &dir);
        let Ok(Some(external)) =
            crate::deno_support::import_map_path(&repo_root.join(config), &json)
        else {
            return;
        };
        let Ok(relative) = external.strip_prefix(repo_root).map(normalize) else {
            return;
        };
        if let Ok(map) = read_jsonc_config(&external) {
            let base = relative.parent().map(Path::to_path_buf).unwrap_or_default();
            self.add_import_map(&map, &dir, &base);
        }
    }

    /// `scope` is the declaring config's directory; `base` is where the map's
    /// relative targets and scope keys resolve.
    fn add_import_map(&mut self, map: &serde_json::Value, scope: &Path, base: &Path) {
        if let Some(imports) = map.get("imports").and_then(serde_json::Value::as_object) {
            self.deno_maps.push(DenoImportMap {
                scope: scope.to_path_buf(),
                base: base.to_path_buf(),
                entries: import_map_entries(imports),
                scoped: false,
            });
        }
        if let Some(scopes) = map.get("scopes").and_then(serde_json::Value::as_object) {
            for (key, entries) in scopes {
                let (Some(entries), true) = (
                    entries.as_object(),
                    key.starts_with("./") || key.starts_with("../"),
                ) else {
                    continue;
                };
                self.deno_maps.push(DenoImportMap {
                    scope: normalize(&base.join(key)),
                    base: base.to_path_buf(),
                    entries: import_map_entries(entries),
                    scoped: true,
                });
            }
        }
    }

    /// Apply one tsconfig's `extends` chain and return what it states about
    /// `paths` and `baseUrl`.
    fn ts_mapping(
        &mut self,
        repo_root: &Path,
        config: &Path,
        internal_package_dirs: &BTreeMap<String, PathBuf>,
    ) -> Option<TsPathMapping> {
        let mut base_url: Option<PathBuf> = None;
        let mut paths: Option<(PathsMap, PathBuf)> = None;
        // Nearest first: the config itself, then what it extends, depth-first
        // in the order TypeScript applies a list (a later entry overrides an
        // earlier one, so it is visited first here).
        let mut pending: Vec<(PathBuf, usize)> = vec![(config.to_path_buf(), 0)];
        let mut visited: BTreeSet<PathBuf> = BTreeSet::new();
        while let Some((file, depth)) = pending.pop() {
            if depth > MAX_EXTENDS_DEPTH || !visited.insert(file.clone()) {
                continue;
            }
            let Ok(json) = read_jsonc_config(&repo_root.join(&file)) else {
                continue;
            };
            let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
            let options = json.get("compilerOptions");
            if base_url.is_none()
                && let Some(url) = options
                    .and_then(|o| o.get("baseUrl"))
                    .and_then(|v| v.as_str())
            {
                base_url = Some(normalize(&dir.join(url)));
            }
            if paths.is_none()
                && let Some(map) = options
                    .and_then(|o| o.get("paths"))
                    .and_then(|v| v.as_object())
            {
                let entries = map
                    .iter()
                    .map(|(pattern, targets)| {
                        let targets = targets
                            .as_array()
                            .map(|t| {
                                t.iter()
                                    .filter_map(|v| v.as_str().map(str::to_owned))
                                    .collect()
                            })
                            .unwrap_or_default();
                        (pattern.clone(), targets)
                    })
                    .collect();
                paths = Some((entries, dir.clone()));
            }
            let extends: Vec<&str> = match json.get("extends") {
                Some(serde_json::Value::String(one)) => vec![one.as_str()],
                Some(serde_json::Value::Array(many)) => {
                    many.iter().filter_map(|v| v.as_str()).collect()
                }
                _ => Vec::new(),
            };
            // Pushed in declaration order so the LAST entry pops first: it
            // overrides the earlier ones, and nearest-first reading keeps the
            // first value seen.
            for spec in extends {
                match extends_target(repo_root, &dir, spec, internal_package_dirs) {
                    Some(next) => pending.push((next, depth + 1)),
                    None => {
                        self.unfollowed_extends.insert(spec.to_string());
                    }
                }
            }
        }
        if base_url.is_none() && paths.is_none() {
            return None;
        }
        let (paths, declared_in) = paths.unwrap_or_default();
        Some(TsPathMapping {
            paths_base: base_url.clone().unwrap_or(declared_in),
            base_url,
            paths,
        })
    }
}

/// The config file an `extends` value names, repo-relative. A relative or
/// absolute value is a path from the extending config (`.json` appended when
/// the file as written does not exist). A package value is looked up as a
/// workspace member first, then under the nearest `node_modules`.
fn extends_target(
    repo_root: &Path,
    dir: &Path,
    spec: &str,
    internal_package_dirs: &BTreeMap<String, PathBuf>,
) -> Option<PathBuf> {
    let existing = |candidate: PathBuf| -> Option<PathBuf> {
        let candidate = normalize(&candidate);
        if repo_root.join(&candidate).is_file() {
            return Some(candidate);
        }
        let with_json = PathBuf::from(format!("{}.json", candidate.display()));
        repo_root.join(&with_json).is_file().then_some(with_json)
    };
    let package_config = |package_dir: PathBuf, subpath: &str| -> Option<PathBuf> {
        if subpath.is_empty() {
            return existing(package_dir.join("tsconfig.json"));
        }
        existing(package_dir.join(subpath))
            .or_else(|| existing(package_dir.join(subpath).join("tsconfig.json")))
    };

    if spec.starts_with("./") || spec.starts_with("../") {
        return existing(dir.join(spec));
    }
    if let Some(name) = internal_package_dirs
        .keys()
        .filter(|name| spec == name.as_str() || spec.starts_with(&format!("{name}/")))
        .max_by_key(|name| name.len())
    {
        let subpath = spec[name.len()..].trim_start_matches('/');
        return package_config(internal_package_dirs[name].clone(), subpath);
    }
    dir.ancestors().find_map(|ancestor| {
        let installed = ancestor.join("node_modules").join(spec);
        existing(installed.clone()).or_else(|| existing(installed.join("tsconfig.json")))
    })
}

/// An import map's string entries, each target kept only when it is a local
/// path.
fn import_map_entries(
    map: &serde_json::Map<String, serde_json::Value>,
) -> BTreeMap<String, Option<String>> {
    map.iter()
        .filter_map(|(key, target)| {
            let target = target.as_str()?;
            let local =
                (target.starts_with("./") || target.starts_with("../")).then(|| target.to_string());
            Some((key.clone(), local))
        })
        .collect()
}

/// An import map lookup (the WHATWG import map rules Deno follows): `None`
/// when no key matches, `Some(None)` when the matching key maps somewhere
/// outside this repo, `Some(Some(path))` when it maps to a local path. An
/// exact key beats a prefix key, the longest prefix key wins, and only a key
/// ending in `/` is a prefix.
/// The import-map key that claims `specifier`, and the path it names. The key
/// is returned even when the path is `None` — an entry naming a registry
/// package still ends the lookup, and a caller reporting a target that is not
/// on disk needs the key to report it against (carrick#1273).
fn match_import_map<'a>(
    map: &'a DenoImportMap,
    specifier: &str,
) -> Option<(&'a String, Option<PathBuf>)> {
    if let Some((key, target)) = map.entries.get_key_value(specifier) {
        return Some((key, target.as_ref().map(|t| normalize(&map.base.join(t)))));
    }
    let (key, target) = map
        .entries
        .iter()
        .filter(|(key, _)| key.ends_with('/') && specifier.starts_with(key.as_str()))
        .max_by_key(|(key, _)| key.len())?;
    Some((
        key,
        target
            .as_ref()
            .filter(|t| t.ends_with('/'))
            .map(|t| normalize(&map.base.join(t).join(&specifier[key.len()..]))),
    ))
}

/// The key a specifier matches, and what its `*` captured. An exact key
/// beats a pattern; among patterns the longest prefix wins, then the longest
/// key (Node's `PATTERN_KEY_COMPARE`, TypeScript's `matchPatternOrExact`).
/// A key with more than one `*` matches nothing.
pub(crate) fn match_pattern<'a>(
    keys: impl Iterator<Item = &'a str>,
    specifier: &str,
) -> Option<(&'a str, String)> {
    let mut best: Option<(&'a str, String)> = None;
    for key in keys {
        match key.matches('*').count() {
            0 if key == specifier => return Some((key, String::new())),
            1 => {
                let (prefix, suffix) = key.split_once('*').unwrap_or_default();
                if specifier.len() >= prefix.len() + suffix.len()
                    && specifier.starts_with(prefix)
                    && specifier.ends_with(suffix)
                {
                    let better = match &best {
                        None => true,
                        Some((current, _)) => {
                            let current_prefix =
                                current.split_once('*').map(|(p, _)| p.len()).unwrap_or(0);
                            (prefix.len(), key.len()) > (current_prefix, current.len())
                        }
                    };
                    if better {
                        let captured =
                            specifier[prefix.len()..specifier.len() - suffix.len()].to_string();
                        best = Some((key, captured));
                    }
                }
            }
            _ => {}
        }
    }
    best
}

/// Every string leaf under a conditions value, in document order, bounded by
/// a depth no real manifest reaches. Shared with the `exports` reader in
/// [`crate::workspace_resolver`].
pub(crate) fn collect_string_leaves(
    value: &serde_json::Value,
    depth: usize,
    out: &mut Vec<String>,
) {
    const MAX_CONDITION_DEPTH: usize = 8;
    if depth > MAX_CONDITION_DEPTH {
        return;
    }
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Object(map) => {
            for nested in map.values() {
                collect_string_leaves(nested, depth + 1, out);
            }
        }
        serde_json::Value::Array(items) => {
            for nested in items {
                collect_string_leaves(nested, depth + 1, out);
            }
        }
        _ => {}
    }
}
