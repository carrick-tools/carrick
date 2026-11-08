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
//! Which tsconfig or jsconfig governs a file is one rule, the language
//! service's ([`Governing`], carrick#3497), which the type sidecar applies
//! too, so call edges and types read a file under the same config;
//! `docs/reference/module-resolution.md` ("Which config governs a file")
//! states it.
//!
//! An alias defined only in code (a bundler's `resolve.alias`, a Babel plugin
//! option) is not config and is not read. The call graph counts the imports it
//! could not resolve instead of dropping them silently; see
//! `docs/reference/module-resolution.md` for the precedence rules, the
//! compiler-parity check, and the limits.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::packages::read_jsonc_config;
use crate::tsconfig_program::{ProjectGraph, canonical};
use crate::workspace_resolver::normalize;

/// How many `extends` hops are followed before a chain is treated as a cycle.
const MAX_EXTENDS_DEPTH: usize = 16;

/// Config file names read here, in the order a directory is checked for the
/// config that governs it. In one directory `tsconfig.json` is asked first
/// and a `jsconfig.json` beside it is asked when the tsconfig lists the file
/// nowhere, as the TypeScript language service does.
pub(crate) const TS_CONFIG_NAMES: [&str; 2] = ["tsconfig.json", "jsconfig.json"];

/// The names a service root is searched for, in order (carrick#3497). The
/// type sidecar's `SERVICE_ROOT_NAMES` is the same list.
const SERVICE_ROOT_NAMES: [&str; 3] = ["tsconfig.json", "tsconfig.build.json", "tsconfig.app.json"];

/// The service an alias index is built for (carrick#3497): the files under
/// `dir` take the config [`Governing::governing_config`] picks, every other
/// file the nearest config above it. Both paths are repo-relative.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ServiceScope {
    /// The service directory; the repo root is the empty path.
    pub dir: PathBuf,
    /// The `tsconfig` the service's `carrick.json` names, joined onto `dir`.
    pub named: Option<PathBuf>,
}

/// The service config, the fallback in `docs/reference/module-resolution.md`
/// (carrick#3497), as the type sidecar's `findTsConfig` and `findServiceTsconfig`: the `named`
/// config when it is a file and not a Deno config; else the first of
/// [`SERVICE_ROOT_NAMES`] in the service directory; else, when a directory
/// above the service holds a `tsconfig.json` (up to the repo root
/// inclusive), the service root's own `jsconfig.json` if it has one,
/// otherwise that ancestor config. This is the fallback of [`Governing`],
/// for a file no config inside the service lists. Every path is relative to
/// `repo_root`; the repo root itself is the empty path.
pub(crate) fn service_config(
    repo_root: &Path,
    service_dir: &Path,
    named: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(named) = named {
        let deno = named
            .file_name()
            .is_some_and(|name| name == "deno.json" || name == "deno.jsonc");
        if !deno && repo_root.join(named).is_file() {
            return Some(normalize(named));
        }
    }
    if let Some(name) = SERVICE_ROOT_NAMES
        .iter()
        .find(|name| repo_root.join(service_dir).join(name).is_file())
    {
        return Some(service_dir.join(name));
    }
    let own_jsconfig = service_dir.join("jsconfig.json");
    let own_jsconfig = repo_root
        .join(&own_jsconfig)
        .is_file()
        .then_some(own_jsconfig);
    let mut dir = service_dir;
    while let Some(parent) = dir.parent() {
        if repo_root.join(parent).join("tsconfig.json").is_file() {
            return Some(own_jsconfig.unwrap_or_else(|| parent.join("tsconfig.json")));
        }
        dir = parent;
    }
    None
}

/// The config each file of one service is read under (carrick#3497): the
/// one the TypeScript language service picks, the rule in
/// `docs/reference/module-resolution.md` ("Which config governs a file").
/// The nearest config above the file, inside the service, whose program
/// (or a project it references) lists the file; else the service config.
/// The type sidecar's `GoverningConfigs` is the same rule. Built once per
/// service, so every question about a file is a lookup.
#[derive(Debug, Clone)]
pub(crate) struct Governing {
    /// The service directory, repo-relative.
    dir: PathBuf,
    /// The fallback ([`service_config`]); `None` for none.
    service: Option<PathBuf>,
    /// Directory under the service -> the tsconfig and the jsconfig in it
    /// (those that exist, in [`TS_CONFIG_NAMES`] order), repo-relative.
    per_dir: BTreeMap<PathBuf, Vec<PathBuf>>,
    /// Config -> the projects it and its `references` reach, for every
    /// config above that could be read.
    graphs: BTreeMap<PathBuf, ProjectGraph>,
    canonical_root: PathBuf,
}

impl Governing {
    /// `config_dirs` is directory -> the tsconfig and jsconfig in it (tsconfig
    /// first), repo-relative, as the manifest walk found them.
    pub(crate) fn new(
        repo_root: &Path,
        scope: &ServiceScope,
        config_dirs: &BTreeMap<PathBuf, Vec<PathBuf>>,
        internal_package_dirs: &BTreeMap<String, PathBuf>,
    ) -> Self {
        let service = service_config(repo_root, &scope.dir, scope.named.as_deref());
        let per_dir: BTreeMap<PathBuf, Vec<PathBuf>> = config_dirs
            .iter()
            .filter(|(dir, _)| dir.starts_with(&scope.dir))
            .map(|(dir, config)| (dir.clone(), config.clone()))
            .collect();
        let canonical_root = canonical(repo_root);
        let graphs = service
            .iter()
            .chain(per_dir.values().flatten())
            .filter_map(|config| {
                let graph = ProjectGraph::new(
                    &canonical_root,
                    &canonical(&repo_root.join(config)),
                    internal_package_dirs,
                )?;
                Some((config.clone(), graph))
            })
            .collect();
        Governing {
            dir: scope.dir.clone(),
            service,
            per_dir,
            graphs,
            canonical_root,
        }
    }

    /// Whether `file` (repo-relative) is the service's own.
    pub(crate) fn covers(&self, file: &Path) -> bool {
        file.starts_with(&self.dir)
    }

    /// The config that governs `file` (repo-relative, under the service
    /// directory), repo-relative; `None` for no config.
    pub(crate) fn governing_config(&self, file: &Path) -> Option<PathBuf> {
        let absolute = self.canonical_root.join(file);
        if let Some(owner) = self.nearest_listing(file, &absolute) {
            return Some(owner);
        }
        let service = self.service.as_ref()?;
        let owner = self
            .graphs
            .get(service)
            .and_then(|graph| graph.owner_of(&absolute))
            .and_then(|owner| self.relative(owner));
        Some(owner.unwrap_or_else(|| service.clone()))
    }

    /// Every config [`Self::governing_config`] can answer, repo-relative.
    pub(crate) fn configs(&self) -> BTreeSet<PathBuf> {
        let mut configs: BTreeSet<PathBuf> = self
            .service
            .iter()
            .chain(self.per_dir.values().flatten())
            .cloned()
            .collect();
        for graph in self.graphs.values() {
            configs.extend(graph.configs().filter_map(|config| self.relative(config)));
        }
        configs
    }

    /// The owner, repo-relative, under the nearest config from the file's
    /// directory up to the service directory inclusive whose program or a
    /// project it references lists the file (a solution config hands the
    /// file to the reference that lists it); a config that lists it nowhere
    /// is passed over. The climb stops after a `node_modules` directory.
    fn nearest_listing(&self, file: &Path, absolute: &Path) -> Option<PathBuf> {
        for dir in file.ancestors().skip(1) {
            if !dir.starts_with(&self.dir) {
                return None;
            }
            let owner = self.per_dir.get(dir).and_then(|configs| {
                configs.iter().find_map(|config| {
                    let owner = self.graphs.get(config)?.owner_of(absolute)?;
                    self.relative(owner)
                })
            });
            if owner.is_some() {
                return owner;
            }
            if dir == self.dir || dir.file_name() == Some("node_modules".as_ref()) {
                return None;
            }
        }
        None
    }

    fn relative(&self, absolute: &Path) -> Option<PathBuf> {
        absolute
            .strip_prefix(&self.canonical_root)
            .ok()
            .map(Path::to_path_buf)
    }
}

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
    /// The rule for the service's own files (carrick#3497); files outside it,
    /// and every file when there is no service, take the nearest config.
    governing: Option<Governing>,
    /// Config -> its mapping, for every config `governing` can answer.
    governing_mappings: BTreeMap<PathBuf, Option<TsPathMapping>>,
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

/// One mapping key, and the directories its targets live in (carrick#1254).
///
/// The alternatives are a disjunction, as they are for the compiler: a
/// `paths` pattern with two targets is satisfied by either, so a caller
/// reports the mapping only when none of them exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaimedDirectory {
    /// The key as written in the config.
    pub declared_by: String,
    /// Repo-relative directories, sorted, any one of which existing satisfies
    /// the claim.
    pub directories: Vec<PathBuf>,
}

/// The repo-relative directory a mapping target lives in: the target up to
/// its last separator, with anything from the `*` onward dropped, resolved
/// against the base the declaring config resolves targets against. A target
/// that names a file directly in `base` claims `base`.
fn target_directory(base: &Path, target: &str) -> PathBuf {
    let literal = target.split('*').next().unwrap_or_default();
    match literal.rfind('/') {
        Some(cut) => normalize(&base.join(&literal[..=cut])),
        None => normalize(base),
    }
}

/// One config answer, and the key that gave it (carrick#1273).
///
/// The key is what a reader recognises: it is the line they wrote in their own
/// config, so a target that is not on disk can be reported against the mapping
/// that claims it rather than against each of the specifiers that went through
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AliasMatch {
    /// The mapping that claimed the specifier.
    ///
    /// `None` for the `baseUrl` fallback, which declares no key: `baseUrl` is
    /// a search root rather than a claim that any particular path exists, so a
    /// specifier that misses under it was never claimed by anything and must
    /// not be reported as a mapping whose target is missing.
    pub declared: Option<Declared>,
    pub target: AliasTarget,
}

/// What one config line says, as it is written there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Declared {
    /// The key: an import-map key, a package.json `imports` key, or a
    /// tsconfig `paths` pattern.
    pub key: String,
    /// The path the key names, before the specifier's own segment is appended
    /// — the declared target with its `*` removed, so `./src/generated/*`
    /// roots at `src/generated/`.
    ///
    /// The MAPPING's target, not one import's resolved file: every specifier
    /// under the key shares it, which is what makes it the thing to report a
    /// missing target against.
    pub target_root: String,
}

/// [`Declared::target_root`] for one declared target, relative to the base it
/// resolves against.
///
/// A `String` rather than a `PathBuf` because the trailing separator is what
/// says the mapping names a directory rather than a file, and a `PathBuf`
/// drops it.
fn target_root(base: &Path, declared: &str) -> String {
    let stripped = declared.split('*').next().unwrap_or(declared);
    let names_a_directory = stripped.ends_with('/');
    let mut root = normalize(&base.join(stripped))
        .to_string_lossy()
        .replace('\\', "/");
    if names_a_directory && !root.ends_with('/') {
        root.push('/');
    }
    root
}

impl ModuleAliases {
    /// Read every config the tree holds. `configs` is the repo-relative list
    /// of `package.json`, `deno.json(c)`, `tsconfig.json` and `jsconfig.json`
    /// files the manifest walk found; `scope` is the service the index is
    /// built for, if any.
    pub(crate) fn build(
        repo_root: &Path,
        configs: &[PathBuf],
        internal_package_dirs: &BTreeMap<String, PathBuf>,
        scope: Option<&ServiceScope>,
    ) -> Self {
        let mut aliases = ModuleAliases::default();
        let mut ts_dirs: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
        // Every tsconfig and jsconfig per directory, in `TS_CONFIG_NAMES` order.
        let mut governing_dirs: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
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
                    let beside = governing_dirs.entry(dir.clone()).or_default();
                    beside.push(config.clone());
                    beside.sort_by_key(|c| {
                        TS_CONFIG_NAMES
                            .iter()
                            .position(|n| c.file_name() == Some((*n).as_ref()))
                    });
                    // The nearest mapping of a file outside every service
                    // reads one config per directory: tsconfig.json first.
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
        for (dir, config) in &ts_dirs {
            let mapping = aliases.ts_mapping(repo_root, config, internal_package_dirs);
            aliases.ts_configs.insert(dir.clone(), mapping);
        }
        if let Some(scope) = scope {
            let governing =
                Governing::new(repo_root, scope, &governing_dirs, internal_package_dirs);
            for config in governing.configs() {
                let mapping = aliases.ts_mapping(repo_root, &config, internal_package_dirs);
                aliases.governing_mappings.insert(config, mapping);
            }
            aliases.governing = Some(governing);
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

    /// Every mapping these configs declare, as the directory its target lives
    /// in (carrick#1254).
    ///
    /// The same claims [`Self::resolve`] answers a specifier with, read the
    /// other way round: without a specifier to ask about, so a caller can stat
    /// them before a scan starts rather than discover them one failed import
    /// at a time. One entry per key, because every import through a mapping
    /// fails identically and the key is the line the user wrote.
    ///
    /// A DIRECTORY rather than the target itself: which file a target names
    /// depends on extension and index-file rules that are
    /// [`crate::workspace_resolver::WorkspaceIndex`]'s, and a caller that
    /// guessed them here would refuse a mapping that resolves perfectly well.
    /// A directory that is not there cannot hold any spelling of the file.
    /// `baseUrl` declares no key and is not a claim, so it is not here.
    pub(crate) fn claimed_directories(&self) -> Vec<ClaimedDirectory> {
        let mut claims: BTreeMap<String, BTreeSet<PathBuf>> = BTreeMap::new();
        for map in &self.deno_maps {
            for (key, target) in &map.entries {
                // A key that names a registry package claims no path here.
                if let Some(target) = target {
                    claims
                        .entry(key.clone())
                        .or_default()
                        .insert(target_directory(&map.base, target));
                }
            }
        }
        for (dir, imports) in &self.package_imports {
            let Some(object) = imports.as_ref().and_then(serde_json::Value::as_object) else {
                continue;
            };
            for (key, value) in object {
                let mut leaves = Vec::new();
                collect_string_leaves(value, 0, &mut leaves);
                for leaf in leaves.iter().filter(|leaf| leaf.starts_with("./")) {
                    claims
                        .entry(key.clone())
                        .or_default()
                        .insert(target_directory(dir, leaf));
                }
            }
        }
        let ts = self
            .ts_configs
            .values()
            .chain(self.governing_mappings.values())
            .filter_map(Option::as_ref);
        for mapping in ts {
            for (pattern, targets) in &mapping.paths {
                for target in targets {
                    claims
                        .entry(pattern.clone())
                        .or_default()
                        .insert(target_directory(&mapping.paths_base, target));
                }
            }
        }
        claims
            .into_iter()
            .map(|(declared_by, directories)| ClaimedDirectory {
                declared_by,
                directories: directories.into_iter().collect(),
            })
            .collect()
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
        if let Some((key, declared_target, matched)) = self
            .deno_maps
            .iter()
            .filter(|map| from_file.starts_with(&map.scope))
            .find_map(|map| {
                match_import_map(map, specifier)
                    .map(|(key, entry, resolved)| (key, (map.base.clone(), entry), resolved))
            })
        {
            return matched
                .map(|path| AliasMatch {
                    declared: Some(Declared {
                        key: key.clone(),
                        target_root: target_root(&declared_target.0, &declared_target.1),
                    }),
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

        let mapping = match &self.governing {
            Some(governing) if governing.covers(from_file) => governing
                .governing_config(from_file)
                .and_then(|config| self.governing_mappings.get(&config))
                .and_then(Option::as_ref),
            _ => self.nearest_ts_mapping(from_file),
        };
        let Some(mapping) = mapping else {
            return Vec::new();
        };
        let mut targets = Vec::new();
        if let Some((pattern, substitution)) =
            match_pattern(mapping.paths.iter().map(|(p, _)| p.as_str()), specifier)
        {
            let declared_targets = mapping
                .paths
                .iter()
                .find(|(p, _)| p == pattern)
                .map(|(_, targets)| targets.as_slice())
                .unwrap_or_default();
            let candidates = declared_targets
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
                // The first declared target is the one the pattern is reported
                // against: a `paths` entry states its targets in preference
                // order, so it is the one the config says to use.
                declared: declared_targets.first().map(|declared| Declared {
                    key: pattern.to_string(),
                    target_root: target_root(&mapping.paths_base, declared),
                }),
                target: AliasTarget::Paths(candidates),
            });
        }
        if let Some(base_url) = &mapping.base_url {
            targets.push(AliasMatch {
                declared: None,
                target: AliasTarget::Paths(vec![normalize(&base_url.join(specifier))]),
            });
        }
        targets
    }

    /// The config the service's rule reads `from_file` (repo-relative) under,
    /// repo-relative; `None` for a file outside the service, or with no
    /// service or no config (carrick#3497).
    pub(crate) fn governing_config(&self, from_file: &Path) -> Option<PathBuf> {
        self.governing
            .as_ref()
            .filter(|governing| governing.covers(from_file))?
            .governing_config(from_file)
    }

    /// The mapping for a file outside the service (rule D2 of carrick#3497):
    /// the nearest directory, from the importer's own up to the repo root,
    /// that holds a tsconfig or jsconfig. See
    /// `docs/reference/module-resolution.md`.
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
        let declared_leaves = leaves
            .into_iter()
            // A bare target names a package, not a file in this directory.
            .filter(|leaf| leaf.starts_with("./"))
            .collect::<Vec<_>>();
        let leaves = declared_leaves
            .iter()
            .map(|leaf| leaf.replace('*', &substitution))
            .collect::<Vec<_>>();
        (!leaves.is_empty()).then(|| AliasMatch {
            declared: declared_leaves.first().map(|declared| Declared {
                key: key.to_string(),
                target_root: target_root(dir, declared),
            }),
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
        let (chain, unfollowed) = tsconfig_chain(repo_root, config, internal_package_dirs);
        self.unfollowed_extends.extend(unfollowed);
        for (file, json) in chain {
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

/// Every config a tsconfig's `extends` chain reads, `config` first, each
/// repo-relative, and the `extends` values it could not follow
/// (carrick#2227). The same walk [`ModuleAliases`] applies to read `paths`.
pub(crate) fn tsconfig_chain(
    repo_root: &Path,
    config: &Path,
    internal_package_dirs: &BTreeMap<String, PathBuf>,
) -> (Vec<(PathBuf, serde_json::Value)>, Vec<String>) {
    let mut chain = Vec::new();
    let mut unfollowed = Vec::new();
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
        let extends: Vec<String> = match json.get("extends") {
            Some(serde_json::Value::String(one)) => vec![one.clone()],
            Some(serde_json::Value::Array(many)) => many
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        };
        // Pushed in declaration order so the LAST entry pops first: it
        // overrides the earlier ones, and nearest-first reading keeps the
        // first value seen.
        for spec in extends {
            match extends_target(repo_root, &dir, &spec, internal_package_dirs) {
                Some(next) => pending.push((next, depth + 1)),
                None => unfollowed.push(spec),
            }
        }
        chain.push((file, json));
    }
    (chain, unfollowed)
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
) -> Option<(&'a String, String, Option<PathBuf>)> {
    if let Some((key, target)) = map.entries.get_key_value(specifier) {
        return Some((
            key,
            target.clone().unwrap_or_default(),
            target.as_ref().map(|t| normalize(&map.base.join(t))),
        ));
    }
    let (key, target) = map
        .entries
        .iter()
        .filter(|(key, _)| key.ends_with('/') && specifier.starts_with(key.as_str()))
        .max_by_key(|(key, _)| key.len())?;
    Some((
        key,
        target.clone().unwrap_or_default(),
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

#[cfg(test)]
mod tests {
    //! The one rule for which config governs a file (carrick#3497), a test
    //! per step. `tests/module_resolution_parity_test.rs` holds the same
    //! layouts against the type sidecar's built rule.

    use super::*;
    use crate::workspace_resolver::WorkspaceIndex;

    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let path = repo.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        repo
    }

    fn governing(
        repo: &tempfile::TempDir,
        dir: &str,
        named: Option<&str>,
        file: &str,
    ) -> Option<String> {
        let scope = ServiceScope {
            dir: PathBuf::from(dir),
            named: named.map(PathBuf::from),
        };
        WorkspaceIndex::build_with_aliases(repo.path(), Some(&scope))
            .governing_config(Path::new(file))
            .map(|config| config.to_string_lossy().replace('\\', "/"))
    }

    #[test]
    fn the_nearest_config_that_lists_the_file_governs_it() {
        let repo = tree(&[
            ("tsconfig.json", r#"{"include":["src"]}"#),
            ("src/invoicing/tsconfig.json", "{}"),
        ]);
        assert_eq!(
            governing(&repo, "", None, "src/invoicing/a.ts").as_deref(),
            Some("src/invoicing/tsconfig.json"),
            "a nested config inside the service governs its own files"
        );
        assert_eq!(
            governing(&repo, "", None, "src/b.ts").as_deref(),
            Some("tsconfig.json")
        );
    }

    #[test]
    fn a_jsconfig_beside_a_tsconfig_that_does_not_list_the_file_governs_it() {
        let repo = tree(&[
            ("web/tsconfig.json", r#"{"include":["src/**/*.ts"]}"#),
            ("web/jsconfig.json", "{}"),
            ("web/src/a.js", ""),
            ("web/src/b.ts", ""),
        ]);
        assert_eq!(
            governing(&repo, "", None, "web/src/a.js").as_deref(),
            Some("web/jsconfig.json"),
            "the tsconfig lists no JavaScript"
        );
        assert_eq!(
            governing(&repo, "", None, "web/src/b.ts").as_deref(),
            Some("web/tsconfig.json"),
            "a tsconfig that lists the file stays first"
        );
    }

    #[test]
    fn a_config_that_does_not_list_the_file_is_passed_over() {
        let repo = tree(&[
            ("web/jsconfig.json", "{}"),
            ("web/src/tsconfig.json", r#"{"include":["other"]}"#),
            ("web/src/lib/tsconfig.json", "{}"),
        ]);
        assert_eq!(
            governing(&repo, "web", None, "web/src/a.ts").as_deref(),
            Some("web/jsconfig.json"),
            "outside its include"
        );
        assert_eq!(
            governing(&repo, "web", None, "web/src/lib/a.js").as_deref(),
            Some("web/jsconfig.json"),
            "a JavaScript file under a config without allowJs"
        );
        assert_eq!(
            governing(&repo, "web", None, "web/src/lib/a.ts").as_deref(),
            Some("web/src/lib/tsconfig.json")
        );
    }

    #[test]
    fn the_service_config_is_the_fallback_named_first() {
        let repo = tree(&[
            ("web/tsconfig.json", r#"{"include":["src"]}"#),
            ("web/tsconfig.custom.json", r#"{"include":["src"]}"#),
        ]);
        assert_eq!(
            governing(
                &repo,
                "web",
                Some("web/tsconfig.custom.json"),
                "web/scripts/a.ts"
            )
            .as_deref(),
            Some("web/tsconfig.custom.json"),
            "no config above lists it: the named one"
        );
        assert_eq!(
            governing(
                &repo,
                "web",
                Some("web/tsconfig.custom.json"),
                "web/src/a.ts"
            )
            .as_deref(),
            Some("web/tsconfig.json"),
            "the nearest config that lists it outranks the named one"
        );
        assert_eq!(
            governing(&repo, "web", Some("web/gone.json"), "web/scripts/a.ts").as_deref(),
            Some("web/tsconfig.json")
        );
        let repo = tree(&[
            ("tsconfig.json", "{}"),
            ("web/tsconfig.app.json", r#"{"include":["src"]}"#),
        ]);
        assert_eq!(
            governing(&repo, "web", None, "web/scripts/a.ts").as_deref(),
            Some("web/tsconfig.app.json"),
            "a service root name outranks an ancestor"
        );
    }

    #[test]
    fn an_ancestor_tsconfig_is_the_fallback_unless_the_service_has_a_jsconfig() {
        let repo = tree(&[("tsconfig.json", "{}"), ("api/package.json", "{}")]);
        assert_eq!(
            governing(&repo, "api", None, "api/src/a.ts").as_deref(),
            Some("tsconfig.json")
        );
        let repo = tree(&[("tsconfig.json", "{}"), ("web/jsconfig.json", "{}")]);
        assert_eq!(
            governing(&repo, "web", None, "web/src/a.js").as_deref(),
            Some("web/jsconfig.json")
        );
    }

    #[test]
    fn each_file_takes_its_nearest_config_within_the_service() {
        let repo = tree(&[
            ("jsconfig.json", "{}"),
            ("web/jsconfig.json", "{}"),
            ("web/src/feature/tsconfig.json", "{}"),
            ("api/src/a.ts", ""),
        ]);
        assert_eq!(
            governing(&repo, "web", None, "web/src/a.js").as_deref(),
            Some("web/jsconfig.json")
        );
        assert_eq!(
            governing(&repo, "web", None, "web/src/feature/a.ts").as_deref(),
            Some("web/src/feature/tsconfig.json")
        );
        // Nothing within the service, and an ancestor jsconfig is not read.
        assert_eq!(governing(&repo, "api", None, "api/src/a.ts"), None);
    }

    #[test]
    fn a_solution_config_hands_each_file_to_the_reference_that_lists_it() {
        let repo = tree(&[
            (
                "web/tsconfig.json",
                r#"{"files":[],"references":[{"path":"./tsconfig.app.json"},{"path":"./tsconfig.node.json"}]}"#,
            ),
            (
                "web/tsconfig.node.json",
                r#"{"include":["vite.config.ts"]}"#,
            ),
            // The app config lists `src/shared` and references `shared`,
            // which lists it too: the reference owns it.
            (
                "web/tsconfig.app.json",
                r#"{"include":["src"],"references":[{"path":"./src/shared"}]}"#,
            ),
            ("web/src/shared/tsconfig.json", r#"{"include":["."]}"#),
        ]);
        let of = |file: &str| governing(&repo, "web", None, file);
        assert_eq!(of("web/src/a.ts").as_deref(), Some("web/tsconfig.app.json"));
        assert_eq!(
            of("web/src/shared/a.ts").as_deref(),
            Some("web/src/shared/tsconfig.json")
        );
        assert_eq!(
            of("web/vite.config.ts").as_deref(),
            Some("web/tsconfig.node.json")
        );
        assert_eq!(
            of("web/scripts/a.ts").as_deref(),
            Some("web/tsconfig.json"),
            "no project lists it: the chosen config"
        );
    }

    #[test]
    fn a_file_outside_the_service_takes_the_nearest_config() {
        let repo = tree(&[
            (
                "web/tsconfig.app.json",
                r#"{"compilerOptions":{"paths":{"@/*":["./src/*"]}}}"#,
            ),
            (
                "lib/tsconfig.json",
                r#"{"compilerOptions":{"paths":{"@/*":["./own/*"]}}}"#,
            ),
            ("lib/own/m.ts", ""),
            ("web/src/m.ts", ""),
        ]);
        let scope = ServiceScope {
            dir: PathBuf::from("web"),
            named: None,
        };
        let index = WorkspaceIndex::build_with_aliases(repo.path(), Some(&scope));
        assert_eq!(index.governing_config(Path::new("lib/x.ts")), None);
        assert_eq!(
            index.resolve(Path::new("lib/x.ts"), "@/m"),
            crate::workspace_resolver::Resolution::Internal(PathBuf::from("lib/own/m.ts"))
        );
        assert_eq!(
            index.resolve(Path::new("web/src/x.ts"), "@/m"),
            crate::workspace_resolver::Resolution::Internal(PathBuf::from("web/src/m.ts"))
        );
    }
}
