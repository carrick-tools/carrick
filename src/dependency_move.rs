//! What a dependency-only manifest or lockfile change moved, and whether a
//! service loads any of it (carrick#3115). Contract:
//! `docs/reference/runner.md`.
//!
//! A change since a stored commit is dependency-only when every repository
//! input it touches is a modified `package.json` that differs in nothing but
//! its four dependency maps and its top-level `version`, or a modified
//! lockfile this module reads (`pnpm-lock.yaml` v6 and v9,
//! `package-lock.json` and `npm-shrinkwrap.json` v2 and v3), each copy read
//! and parsed. A manifest whose dependency maps moved must have its install
//! root's lockfile among them, so the install is frozen to what moved. No
//! moved name may be a workspace package. Anything else is not read, and the
//! caller analyses every service as before.
//!
//! What moved is kept per install root (the directory holding the lockfile):
//! every name a manifest under it re-specified, and every package whose
//! lockfile entry was added, removed or changed. A service loads a moved
//! package when the package is reachable, through the lockfile's name graph,
//! from a package its files, its tests or its tsconfig chain name: hoisting
//! makes a package any manifest declares importable everywhere, and a
//! lockfile moves packages no manifest names.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::change_scope::{
    ClosureReads, ServiceInputs, is_gitignore, is_repo_input, parent_of, under,
};
use crate::config::Config;
use crate::git_state::ChangedPath;
use crate::request_summary::package_name;

/// Every lockfile name a package manager writes. A directory holding one is
/// an install root.
const LOCKFILE_NAMES: [&str; 7] = [
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
    "deno.lock",
];

/// The manifest maps that declare a dependency.
const DEPENDENCY_MAPS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];

/// The maps of an installed lockfile entry that name what it loads.
const ENTRY_DEPENDENCY_MAPS: [&str; 3] =
    ["dependencies", "optionalDependencies", "peerDependencies"];

/// What a dependency-only change means for one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyReach {
    /// The change is not one this rule reads: every service is analysed.
    Unread,
    /// The service loads `package`, which `file` moved.
    Reaches { file: String, package: String },
    /// The service loads nothing the change moved.
    Nothing,
}

/// What a dependency-only change moved, per install root.
#[derive(Debug, Default)]
pub struct DependencyMove {
    roots: Vec<InstallRoot>,
    /// Every workspace package's name and directory relative to the scan
    /// root: what a service's files load from the workspace is read through
    /// its closure, not through the lockfile.
    workspace: BTreeMap<String, PathBuf>,
}

#[derive(Debug)]
struct InstallRoot {
    /// Top-relative, `""` for the top level.
    dir: String,
    /// Each moved package, with the first changed file that moved it.
    moved: BTreeMap<String, String>,
    /// Each package name to the names its entries load, in either copy of
    /// the lockfile, every version merged.
    graph: HashMap<String, BTreeSet<String>>,
}

/// What changed in the manifests and lockfiles between commit `from` and
/// `to` (another commit, or the tree at `top` when `None`), or `None` when
/// the change is not dependency-only. `changed` lists every path that
/// differs, top-relative.
pub fn dependency_move(
    repo_path: &str,
    top: &Path,
    from: &str,
    to: Option<&str>,
    changed: &[ChangedPath],
) -> Option<DependencyMove> {
    let read_to = |path: &str| match to {
        Some(commit) => crate::git_state::file_at_commit(repo_path, commit, path),
        None => std::fs::read_to_string(top.join(path)).ok(),
    };
    let mut manifests: Vec<(&str, BTreeSet<String>)> = Vec::new();
    let mut locks: BTreeMap<&str, (&str, Lockfile, Lockfile)> = BTreeMap::new();
    // A `.gitignore` is judged by its own rule (carrick#3113).
    let repo_inputs = changed
        .iter()
        .filter(|change| is_repo_input(&change.path) && !is_gitignore(&change.path));
    for change in repo_inputs {
        if change.status != 'M' {
            return None;
        }
        let name = change.path.rsplit('/').next().unwrap_or(&change.path);
        let format = match name {
            "package.json" => None,
            "package-lock.json" | "npm-shrinkwrap.json" => Some(Lockfile::npm as fn(&str) -> _),
            "pnpm-lock.yaml" => Some(Lockfile::pnpm as fn(&str) -> _),
            _ => return None,
        };
        let before = crate::git_state::file_at_commit(repo_path, from, &change.path)?;
        let after = read_to(&change.path)?;
        match format {
            None => manifests.push((&change.path, manifest_moves(&before, &after)?)),
            Some(parse) => {
                let dir = parent_of(&change.path);
                let lock = (change.path.as_str(), parse(&before)?, parse(&after)?);
                if locks.insert(dir, lock).is_some() {
                    return None;
                }
            }
        }
    }

    let workspace = crate::workspace_resolver::WorkspaceIndex::build(Path::new(repo_path))
        .internal_package_dirs();
    // The scan root's directory under the top level, to place the workspace
    // packages beside the lockfiles' importers.
    let scan_root = Path::new(repo_path)
        .canonicalize()
        .ok()
        .zip(top.canonicalize().ok())
        .and_then(|(scan, top)| {
            scan.strip_prefix(top)
                .ok()
                .map(|dir| dir.to_string_lossy().replace('\\', "/"))
        })
        .unwrap_or_default();
    let importer_names: HashMap<String, &str> = workspace
        .iter()
        .map(|(name, dir)| {
            let dir = dir.to_string_lossy().replace('\\', "/");
            (join(&scan_root, &dir), name.as_str())
        })
        .collect();
    let mut roots: BTreeMap<&str, InstallRoot> = BTreeMap::new();
    for (dir, (file, before, after)) in &locks {
        let mut root = InstallRoot {
            dir: dir.to_string(),
            moved: BTreeMap::new(),
            graph: HashMap::new(),
        };
        for package in before.moved_in(after)? {
            root.moved.insert(package, file.to_string());
        }
        for lock in [before, after] {
            for (name, loads) in &lock.graph {
                root.graph
                    .entry(name.clone())
                    .or_default()
                    .extend(loads.iter().cloned());
            }
            // A workspace package an installed package loads loads what its
            // own entry declares.
            for (importer, dependency) in lock.importers.keys() {
                if let Some(name) = importer_names.get(&join(dir, importer)) {
                    root.graph
                        .entry(name.to_string())
                        .or_default()
                        .insert(dependency.clone());
                }
            }
        }
        roots.insert(dir, root);
    }
    for (path, moved) in manifests {
        if moved.is_empty() {
            continue;
        }
        let dir = install_root(repo_path, top, to, path, &locks)?;
        let root = roots.get_mut(dir)?;
        for package in moved {
            root.moved
                .entry(package)
                .or_insert_with(|| path.to_string());
        }
    }
    if roots.values().any(|root| {
        root.moved
            .keys()
            .any(|package| workspace.contains_key(package))
    }) {
        return None;
    }
    Some(DependencyMove {
        roots: roots.into_values().collect(),
        workspace,
    })
}

/// The changed lockfile's directory that installs the manifest at `path`:
/// the nearest directory at or above it holding a lockfile in the tree `to`
/// names. `None` when that directory holds no changed lockfile this module
/// read, or more than one lockfile.
fn install_root<'a>(
    repo_path: &str,
    top: &Path,
    to: Option<&str>,
    path: &str,
    locks: &BTreeMap<&'a str, (&str, Lockfile, Lockfile)>,
) -> Option<&'a str> {
    let mut dir = parent_of(path);
    loop {
        let candidates: Vec<String> = LOCKFILE_NAMES.iter().map(|name| join(dir, name)).collect();
        let present: Vec<&String> = match to {
            None => candidates
                .iter()
                .filter(|candidate| top.join(candidate).is_file())
                .collect(),
            Some(commit) => {
                let held =
                    crate::git_state::paths_at_commit(repo_path, commit, &candidates).ok()?;
                candidates
                    .iter()
                    .filter(|candidate| held.contains(*candidate))
                    .collect()
            }
        };
        match present.as_slice() {
            [] if dir.is_empty() => return None,
            [] => dir = parent_of(dir),
            [lock] => {
                let (root, (file, _, _)) = locks.get_key_value(dir)?;
                return (file == lock).then_some(*root);
            }
            _ => return None,
        }
    }
}

/// `path` under the top-relative directory `dir`; `""` and `.` name `dir`.
fn join(dir: &str, path: &str) -> String {
    match (dir, path) {
        (dir, "" | ".") => dir.to_string(),
        ("", path) => path.to_string(),
        (dir, path) => format!("{dir}/{path}"),
    }
}

/// The names a manifest re-specified in its dependency maps, or `None` when
/// either copy is not a JSON object or they differ in anything but those
/// maps and the top-level `version`.
fn manifest_moves(before: &str, after: &str) -> Option<BTreeSet<String>> {
    let (Value::Object(mut before), Value::Object(mut after)) = (
        serde_json::from_str::<Value>(before).ok()?,
        serde_json::from_str::<Value>(after).ok()?,
    ) else {
        return None;
    };
    let mut moved = BTreeSet::new();
    for map in DEPENDENCY_MAPS {
        let (before, after) = (
            object_or_empty(before.remove(map))?,
            object_or_empty(after.remove(map))?,
        );
        moved.extend(differing_keys(&before, &after));
    }
    before.remove("version");
    after.remove("version");
    (before == after).then_some(moved)
}

/// A map value, `{}` when absent, `None` when it is not an object.
fn object_or_empty(value: Option<Value>) -> Option<Map<String, Value>> {
    match value {
        None => Some(Map::new()),
        Some(Value::Object(map)) => Some(map),
        Some(_) => None,
    }
}

/// The keys of either map whose values differ, an absent key included.
fn differing_keys<'a, K: Ord + Clone + 'a, V: PartialEq + 'a>(
    before: impl IntoIterator<Item = (&'a K, &'a V)> + Copy,
    after: impl IntoIterator<Item = (&'a K, &'a V)> + Copy,
) -> BTreeSet<K> {
    let before: BTreeMap<&K, &V> = before.into_iter().collect();
    let after: BTreeMap<&K, &V> = after.into_iter().collect();
    before
        .keys()
        .chain(after.keys())
        .filter(|key| before.get(*key) != after.get(*key))
        .map(|key| (*key).clone())
        .collect()
}

/// One copy of a lockfile, as far as this rule reads it.
#[derive(Debug, Default)]
struct Lockfile {
    /// Everything outside the entries (the format version, settings,
    /// overrides, patches): a difference there is not read.
    header: Map<String, Value>,
    /// Each installed entry by its key, with the package it installs.
    entries: BTreeMap<String, (String, Value)>,
    /// Each root or workspace package's (an importer's) dependency, by
    /// importer directory relative to the lockfile and name.
    importers: BTreeMap<(String, String), Value>,
    /// What else each importer states.
    importer_rest: BTreeMap<String, Value>,
    /// Each package name to the names its entries load.
    graph: BTreeMap<String, BTreeSet<String>>,
}

impl Lockfile {
    /// The packages `after` moved: entries added, removed or changed, and
    /// importer dependencies re-specified. `None` when the copies differ in
    /// anything else.
    fn moved_in(&self, after: &Lockfile) -> Option<BTreeSet<String>> {
        if self.header != after.header || self.importer_rest != after.importer_rest {
            return None;
        }
        let mut moved = BTreeSet::new();
        for key in differing_keys(&self.entries, &after.entries) {
            for lock in [self, after] {
                moved.extend(lock.entries.get(&key).map(|(name, _)| name.clone()));
            }
        }
        moved.extend(
            differing_keys(&self.importers, &after.importers)
                .into_iter()
                .map(|(_, name)| name),
        );
        Some(moved)
    }

    fn loads(&mut self, name: &str, entry: &Value) {
        let loads = self.graph.entry(name.to_string()).or_default();
        for map in ENTRY_DEPENDENCY_MAPS {
            if let Some(Value::Object(dependencies)) = entry.get(map) {
                loads.extend(dependencies.keys().cloned());
            }
        }
    }

    /// `package-lock.json` or `npm-shrinkwrap.json`, version 2 or 3: its
    /// `packages` map, keyed by install path.
    fn npm(text: &str) -> Option<Lockfile> {
        let Value::Object(mut header) = serde_json::from_str::<Value>(text).ok()? else {
            return None;
        };
        if !matches!(
            header.get("lockfileVersion").and_then(Value::as_u64),
            Some(2 | 3)
        ) {
            return None;
        }
        let Some(Value::Object(packages)) = header.remove("packages") else {
            return None;
        };
        // The version 1 mirror npm 7 and later do not read, and the root
        // package's own name and version.
        for key in ["dependencies", "name", "version"] {
            header.remove(key);
        }
        let mut lock = Lockfile {
            header,
            ..Lockfile::default()
        };
        for (key, entry) in packages {
            match key.rfind("node_modules/") {
                Some(at) => {
                    let name = key[at + "node_modules/".len()..].to_string();
                    lock.loads(&name, &entry);
                    lock.entries.insert(key, (name, entry));
                }
                None => {
                    let Value::Object(mut entry) = entry else {
                        return None;
                    };
                    for map in DEPENDENCY_MAPS {
                        for (name, spec) in object_or_empty(entry.remove(map))? {
                            lock.importers.insert((key.clone(), name), spec);
                        }
                    }
                    entry.remove("version");
                    lock.importer_rest.insert(key, Value::Object(entry));
                }
            }
        }
        Some(lock)
    }

    /// `pnpm-lock.yaml`, version 6 or 9: its `importers` (or a single
    /// project's top-level dependency maps), `packages` and `snapshots`.
    fn pnpm(text: &str) -> Option<Lockfile> {
        let Value::Object(mut header) = serde_yaml_ng::from_str::<Value>(text).ok()? else {
            return None;
        };
        let version = header.get("lockfileVersion").and_then(Value::as_str)?;
        if !(version.starts_with("6.") || version.starts_with("9.")) {
            return None;
        }
        let mut importers = object_or_empty(header.remove("importers"))?;
        let mut single = Map::new();
        for map in DEPENDENCY_MAPS {
            if let Some(dependencies) = header.remove(map) {
                single.insert(map.to_string(), dependencies);
            }
        }
        if !single.is_empty()
            && importers
                .insert(".".to_string(), Value::Object(single))
                .is_some()
        {
            return None;
        }
        let packages = object_or_empty(header.remove("packages"))?;
        let snapshots = object_or_empty(header.remove("snapshots"))?;
        let mut lock = Lockfile {
            header,
            ..Lockfile::default()
        };
        for (importer, entry) in importers {
            let Value::Object(mut entry) = entry else {
                return None;
            };
            for map in DEPENDENCY_MAPS {
                for (name, resolved) in object_or_empty(entry.remove(map))? {
                    lock.alias(&name, &resolved);
                    lock.importers.insert((importer.clone(), name), resolved);
                }
            }
            lock.importer_rest.insert(importer, Value::Object(entry));
        }
        for (section, entries) in [("packages", packages), ("snapshots", snapshots)] {
            for (key, entry) in entries {
                let name = pnpm_package_name(&key)?.to_string();
                lock.loads(&name, &entry);
                for map in ENTRY_DEPENDENCY_MAPS {
                    if let Some(Value::Object(dependencies)) = entry.get(map) {
                        for (dependency, resolved) in dependencies {
                            lock.alias(dependency, resolved);
                        }
                    }
                }
                lock.entries
                    .insert(format!("{section} {key}"), (name, entry));
            }
        }
        Some(lock)
    }

    /// A pnpm dependency resolved under another name (`name: other@1.0.0`)
    /// loads that package.
    fn alias(&mut self, name: &str, resolved: &Value) {
        let version = match resolved {
            Value::String(version) => version.as_str(),
            Value::Object(entry) => entry.get("version").and_then(Value::as_str).unwrap_or(""),
            _ => "",
        };
        if version.starts_with(|c: char| c.is_ascii_digit())
            || version.starts_with("link:")
            || version.starts_with("file:")
        {
            return;
        }
        if let Some(target) = pnpm_package_name(version).filter(|target| *target != name) {
            self.graph
                .entry(name.to_string())
                .or_default()
                .insert(target.to_string());
        }
    }
}

/// The package a pnpm entry key names: `name@1.2.3(peer@2.0.0)` (v9),
/// `/@scope/name@1.2.3` (v6).
fn pnpm_package_name(key: &str) -> Option<&str> {
    let key = key.trim_start_matches('/');
    let key = key.split('(').next().unwrap_or(key);
    let at = if let Some(scoped) = key.strip_prefix('@') {
        scoped.find('@')? + 1
    } else {
        key.find('@')?
    };
    (at > 0).then(|| &key[..at])
}

impl DependencyMove {
    /// Whether the service whose inputs are `inputs` loads a moved package.
    /// `packages` names what it loads ([`loaded_packages`]), asked only when
    /// an install root holding one of its paths moved something the
    /// compiler does not read for every program.
    pub fn reach(
        &self,
        inputs: &ServiceInputs,
        packages: impl FnOnce(&DependencyMove) -> BTreeSet<String>,
    ) -> DependencyReach {
        let holding: Vec<&InstallRoot> = self
            .roots
            .iter()
            .filter(|root| !root.moved.is_empty() && holds(inputs, &root.dir))
            .collect();
        // The compiler reads these for every program under the root.
        for root in &holding {
            if let Some((package, file)) = root
                .moved
                .iter()
                .find(|(package, _)| *package == "typescript" || package.starts_with("@types/"))
            {
                return DependencyReach::Reaches {
                    file: file.clone(),
                    package: package.clone(),
                };
            }
        }
        if holding.is_empty() {
            return DependencyReach::Nothing;
        }
        let loaded = packages(self);
        for root in holding {
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            let mut queue: Vec<&str> = loaded.iter().map(String::as_str).collect();
            queue.reverse();
            while let Some(package) = queue.pop() {
                if !seen.insert(package) {
                    continue;
                }
                if let Some(file) = root.moved.get(package) {
                    return DependencyReach::Reaches {
                        file: file.clone(),
                        package: package.to_string(),
                    };
                }
                if let Some(loads) = root.graph.get(package) {
                    queue.extend(loads.iter().rev().map(String::as_str));
                }
            }
        }
        DependencyReach::Nothing
    }
}

/// Whether the install root at `dir` holds one of the service's roots,
/// directories or files.
fn holds(inputs: &ServiceInputs, dir: &str) -> bool {
    inputs
        .roots
        .iter()
        .map(|root| &root.dir)
        .chain(&inputs.dirs)
        .any(|path| under(path, dir) || under(dir, path))
        || inputs.files.iter().any(|file| under(file, dir))
}

/// Every package `service` loads, workspace packages left out: the package
/// of each bare specifier its source files read (its tests included), every
/// file of its closure and every source file under the directories it reads
/// whole, and the packages its tsconfig chain names.
pub fn loaded_packages(
    repo_path: &str,
    top: &Path,
    service: &Config,
    inputs: &ServiceInputs,
    reads: &ClosureReads,
    moved: &DependencyMove,
) -> BTreeSet<String> {
    let skip = &crate::packages::MANIFEST_SKIP_DIRS;
    let mut files: Vec<PathBuf> = Vec::new();
    for root in &inputs.roots {
        files.extend(
            crate::file_finder::source_files_below(&top.join(&root.dir), skip)
                .into_iter()
                .filter(|file| {
                    let relative = file
                        .strip_prefix(top)
                        .map(|path| path.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_default();
                    !root.left_out.iter().any(|dir| under(&relative, dir))
                }),
        );
    }
    for dir in &inputs.dirs {
        files.extend(crate::file_finder::source_files_below(&top.join(dir), skip));
    }
    files.extend(
        inputs
            .files
            .iter()
            .map(|file| top.join(file))
            .filter(|file| crate::file_finder::has_source_extension(file)),
    );

    let mut packages: BTreeSet<String> = BTreeSet::new();
    let subpath_imports = SubpathImports::default();
    for file in &files {
        for specifier in reads.loaded_specifiers(file) {
            let specifier = specifier.split('?').next().unwrap_or(&specifier);
            if specifier.starts_with('#') {
                packages.extend(subpath_imports.of(top, file));
            } else if is_bare(specifier) {
                packages.insert(package_name(specifier).to_string());
            }
        }
    }
    packages.extend(tsconfig_packages(repo_path, service, &moved.workspace));
    packages.retain(|package| !moved.workspace.contains_key(package));
    packages
}

/// Whether a specifier names a package rather than a path.
fn is_bare(specifier: &str) -> bool {
    !(specifier.is_empty()
        || specifier.starts_with('.')
        || specifier.starts_with('/')
        || specifier.starts_with('#'))
}

/// The packages a `#` specifier can load: every package target of the
/// nearest manifest's `imports` map, read once per manifest.
#[derive(Default)]
struct SubpathImports(RefCell<HashMap<PathBuf, Vec<String>>>);

impl SubpathImports {
    fn of(&self, top: &Path, file: &Path) -> Vec<String> {
        let Some(manifest) = file
            .ancestors()
            .skip(1)
            .take_while(|dir| dir.starts_with(top))
            .map(|dir| dir.join("package.json"))
            .find(|manifest| manifest.is_file())
        else {
            return Vec::new();
        };
        self.0
            .borrow_mut()
            .entry(manifest.clone())
            .or_insert_with(|| {
                let imports = std::fs::read_to_string(&manifest)
                    .ok()
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                    .and_then(|json| json.get("imports").cloned());
                let mut targets = Vec::new();
                collect_package_targets(imports.as_ref(), &mut targets);
                targets
            })
            .clone()
    }
}

fn collect_package_targets(value: Option<&Value>, targets: &mut Vec<String>) {
    match value {
        Some(Value::String(target)) if is_bare(target) => {
            targets.push(package_name(target).to_string());
        }
        Some(Value::Array(values)) => {
            for value in values {
                collect_package_targets(Some(value), targets);
            }
        }
        Some(Value::Object(values)) => {
            for value in values.values() {
                collect_package_targets(Some(value), targets);
            }
        }
        _ => {}
    }
}

/// The packages the service's tsconfig chain names: a package it extends,
/// `compilerOptions.types`, `jsxImportSource` (and `react` for a React
/// `jsx` mode), and any `paths` target under `node_modules`.
fn tsconfig_packages(
    repo_path: &str,
    service: &Config,
    workspace: &BTreeMap<String, PathBuf>,
) -> BTreeSet<String> {
    let root = Path::new(repo_path);
    let service_root = match &service.directory {
        Some(dir) => root.join(dir),
        None => root.to_path_buf(),
    };
    let mut packages = BTreeSet::new();
    let Some(tsconfig) = crate::change_scope::service_tsconfig_path(root, &service_root, service)
    else {
        return packages;
    };
    let (chain, unfollowed) = crate::module_aliases::tsconfig_chain(root, &tsconfig, workspace);
    let mut extends: Vec<String> = unfollowed;
    for (_, json) in &chain {
        match json.get("extends") {
            Some(Value::String(one)) => extends.push(one.clone()),
            Some(Value::Array(many)) => {
                extends.extend(many.iter().filter_map(Value::as_str).map(str::to_string))
            }
            _ => {}
        }
        let Some(options) = json.get("compilerOptions") else {
            continue;
        };
        if let Some(Value::Array(types)) = options.get("types") {
            packages.extend(
                types
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|name| is_bare(name))
                    .map(|name| package_name(name).to_string()),
            );
        }
        if let Some(source) = options.get("jsxImportSource").and_then(Value::as_str) {
            packages.insert(package_name(source).to_string());
        }
        if let Some(mode) = options.get("jsx").and_then(Value::as_str)
            && mode.to_ascii_lowercase().starts_with("react")
        {
            packages.insert("react".to_string());
        }
        if let Some(Value::Object(paths)) = options.get("paths") {
            let targets = paths.values().filter_map(Value::as_array).flatten();
            for target in targets.filter_map(Value::as_str) {
                if let Some((_, installed)) = target.rsplit_once("node_modules/") {
                    packages.insert(package_name(installed).to_string());
                }
            }
        }
    }
    packages.extend(
        extends
            .iter()
            .filter(|spec| is_bare(spec))
            .map(|spec| package_name(spec).to_string()),
    );
    packages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pnpm_key_names_its_package() {
        for (key, name) in [
            ("lib-a@1.0.0", Some("lib-a")),
            ("/lib-a@1.0.0", Some("lib-a")),
            ("@scope/lib@1.0.0(peer@2.0.0)", Some("@scope/lib")),
            ("/@scope/lib@1.0.0", Some("@scope/lib")),
            ("/lib-a/1.0.0", None),
        ] {
            assert_eq!(pnpm_package_name(key), name, "{key}");
        }
    }

    #[test]
    fn a_manifest_move_names_the_dependencies_it_respecified() {
        let before = r#"{"name":"a","version":"1","dependencies":{"x":"1","y":"1"}}"#;
        let after =
            r#"{"name":"a","version":"2","dependencies":{"x":"2"},"devDependencies":{"y":"1"}}"#;
        assert_eq!(
            manifest_moves(before, after),
            Some(["x".to_string(), "y".to_string()].into())
        );
        for after in [
            r#"{"name":"a","version":"1","dependencies":{"x":"1","y":"1"},"type":"module"}"#,
            r#"{"name":"a","version":"1","dependencies":{"x":"1","y":"1"},"overrides":{"x":"2"}}"#,
            r#"{"name":"a","version":"1","dependencies":[]}"#,
            "[]",
        ] {
            assert_eq!(manifest_moves(before, after), None, "{after}");
        }
    }

    #[test]
    fn a_lockfile_outside_the_versions_read_is_not_read() {
        assert!(Lockfile::npm(r#"{"lockfileVersion":1,"dependencies":{}}"#).is_none());
        assert!(Lockfile::npm(r#"{"lockfileVersion":3,"packages":{}}"#).is_some());
        assert!(Lockfile::pnpm("lockfileVersion: '5.4'\n").is_none());
        assert!(Lockfile::pnpm("lockfileVersion: '6.0'\n").is_some());
        assert!(Lockfile::pnpm("lockfileVersion: '9.0'\n").is_some());
    }

    /// A pnpm v6 lockfile of a single project, with an aliased dependency:
    /// the alias loads the package it resolves to.
    #[test]
    fn a_pnpm_v6_single_project_lockfile_is_read() {
        let before = "lockfileVersion: '6.0'\n\ndependencies:\n  lib-x:\n    specifier: npm:lib-y@1.0.0\n    version: /lib-y@1.0.0\n\npackages:\n\n  /lib-y@1.0.0:\n    resolution: {integrity: sha512-a}\n    dependencies:\n      lib-z: 1.0.0\n\n  /lib-z@1.0.0:\n    resolution: {integrity: sha512-b}\n";
        let after = before.replace("sha512-b", "sha512-c");
        let (before, after) = (
            Lockfile::pnpm(before).unwrap(),
            Lockfile::pnpm(&after).unwrap(),
        );
        assert_eq!(before.moved_in(&after), Some(["lib-z".to_string()].into()));
        assert_eq!(before.graph["lib-x"], ["lib-y".to_string()].into());
        assert_eq!(before.graph["lib-y"], ["lib-z".to_string()].into());
        let changed_setting = Lockfile::pnpm(&format!(
            "{}settings:\n  autoInstallPeers: false\n",
            "lockfileVersion: '6.0'\n"
        ))
        .unwrap();
        assert_eq!(before.moved_in(&changed_setting), None);
    }
}
