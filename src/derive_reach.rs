//! Which of a service's files its declared entry points and its narrowed
//! program reach, and the rest as units, for `carrick derive --candidates`
//! (carrick#2397, the scanner half of carrick-cloud#2348).
//!
//! A generated config narrows what a scan reads by structure, never by size.
//! This module states the structure and decides nothing: the **reached set**
//! is every file the service's declared roots load, directly or through
//! another, and those are never offered. What reach cannot place is grouped
//! into **units** (a directory whose every file is unreached, or a stray
//! file), each listed with the evidence a judge reads and the exact `exclude`
//! pattern that would leave it out. No rule here leaves a file out: a
//! directory of request handlers a platform finds by location looks to every
//! structural rule like a scripts folder, so whether a unit is read is a
//! judgement, made elsewhere, and a unit nobody judges is read.
//!
//! Roots, all walk files ([`crate::file_finder::find_service_files`]):
//! - the service's own manifest entries (`main`, `module`, `browser`, `types`,
//!   `typings`, every `bin` value, every `exports` target) and the files its
//!   `scripts` name;
//! - every file under the service's `include` roots;
//! - the files its governing tsconfig/jsconfig lists, only when that program
//!   leaves some of the service's files out ("narrows");
//! - every file a routing convention the service's dependencies select maps
//!   to a route ([`crate::file_based_router`]; carrick-cloud#2348 amendment 1).
//!
//! The output is a stored key's input, so it is deterministic: repo-relative
//! or unit-relative `/` paths, every collection ordered.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::Serialize;
use swc_common::{SourceMap, errors::Handler, sync::Lrc};
use swc_ecma_ast::{AssignTarget, Expr, Module, ModuleDecl, ModuleItem, SimpleAssignTarget, Stmt};

use crate::config::Config;
use crate::file_finder::SOURCE_EXTENSIONS;
use crate::tsconfig_program::ProgramRules;
use crate::workspace_resolver::{WorkspaceIndex, normalize};

/// Units listed per service; `truncated.directories` counts the rest.
pub const MAX_UNITS: usize = 300;
/// Units listed per repository, over its services in derive order.
pub const MAX_REPO_UNITS: usize = 2_000;
/// Paths listed per unit.
pub const MAX_PATHS: usize = 10;
/// Files sampled per unit, and lines per sample.
pub const MAX_SAMPLES: usize = 3;
pub const SAMPLE_LINES: usize = 3;
/// Characters kept of a sampled line.
pub const SAMPLE_CHARS: usize = 160;
/// Manifests listed per unit.
pub const MAX_MANIFESTS: usize = 5;
/// Characters kept of a manifest's `description`.
pub const DESCRIPTION_CHARS: usize = 200;
/// Entries listed in a unit's `imports_from`.
pub const MAX_IMPORTS_FROM: usize = 10;

/// What the service's governing program says about its files.
#[derive(Debug, Serialize, PartialEq, Eq, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum Program {
    /// The program leaves at least one of the service's files out; what it
    /// lists are roots.
    Narrows,
    /// The program lists every file of the service, so it states nothing.
    WholeTree,
    /// No tsconfig or jsconfig governs the service.
    #[default]
    None,
}

#[derive(Debug, Serialize, PartialEq, Eq, Default)]
pub struct Reach {
    pub program: Program,
    /// Files the manifest's entry fields name.
    pub manifest_roots: usize,
    /// Files the manifest's `scripts` name.
    pub script_roots: usize,
    /// Files a routing convention maps to a route.
    pub route_roots: usize,
    /// Files the roots reach, the roots included.
    pub reached: usize,
    /// The service's other files.
    pub unclaimed: usize,
}

#[derive(Debug, Serialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum UnitKind {
    Dir,
    File,
}

/// What the program says about one unit's files.
#[derive(Debug, Serialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum UnitProgram {
    /// The service's narrowing program lists none of them.
    Outside,
    /// A config inside the unit lists them.
    Own,
    WholeTree,
    None,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Unit {
    /// Service-relative, `/`-separated.
    pub unit: String,
    pub kind: UnitKind,
    /// The gitignore pattern, anchored at the service directory, that leaves
    /// out exactly this unit's walk files: checked by
    /// [`crate::file_finder::Exclusion::check`] and against the walk.
    pub pattern: String,
    /// Walk files in the unit. For logs and harnesses, never for a judge.
    pub files: usize,
    /// Up to [`MAX_PATHS`], unit-relative, sorted.
    pub paths: Vec<String>,
    pub program: UnitProgram,
    pub manifests: Vec<UnitManifest>,
    /// Files with an ES or CommonJS export.
    pub exporting_files: usize,
    /// The first path segment of each reached file the unit loads, and each
    /// other unit it loads; sorted, up to [`MAX_IMPORTS_FROM`].
    pub imports_from: Vec<String>,
    pub samples: Vec<Sample>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct UnitManifest {
    /// Unit-relative.
    pub path: String,
    pub name: Option<String>,
    /// Cut to [`DESCRIPTION_CHARS`].
    pub description: Option<String>,
    pub private: bool,
    /// It declares an entry (`main`, `module`, `browser`, `types`, `typings`,
    /// `bin` or `exports`).
    pub has_entries: bool,
    /// The other manifests in the repository that list its `name` as a
    /// dependency, repo-relative, sorted.
    pub depended_on_by: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Sample {
    /// Unit-relative.
    pub file: String,
    /// The first [`SAMPLE_LINES`] non-empty lines, verbatim (comments
    /// included: a generated file says so in its header), each cut to
    /// [`SAMPLE_CHARS`].
    pub head: Vec<String>,
}

/// One service's reach and every unit offered, sorted by path.
#[derive(Debug, Default)]
pub struct ServiceReach {
    pub reach: Reach,
    pub units: Vec<Unit>,
}

/// The reach and the offered units of `service` in `repo` (canonical), over
/// the walk's `files` (sorted, deduplicated, each joined onto `repo`).
pub fn service_reach(repo: &Path, service: &Config, files: &[PathBuf]) -> ServiceReach {
    if files.is_empty() {
        return ServiceReach::default();
    }
    let service_root = canonical(&repo.join(service.directory.as_deref().unwrap_or("")));
    let modules = WorkspaceIndex::build_with_aliases(repo, service.alias_scope(repo).as_ref());
    let walk = Walk::new(files);
    let internal = modules.internal_package_dirs();
    let mut roots = vec![false; files.len()];
    let mut reach = Reach::default();

    // A `package.json`, or a Deno config: its `exports` are entries and its
    // `tasks` are its scripts.
    let manifest = crate::file_finder::find_service_manifest(repo, service).and_then(|path| {
        let json = crate::packages::read_json_config(&path).ok()?;
        Some((path.parent()?.to_path_buf(), json))
    });
    if let Some((dir, json)) = &manifest {
        let outputs = build_outputs(repo, dir, &internal);
        let entries: BTreeSet<usize> = manifest_entries(json)
            .iter()
            .flat_map(|target| walk.resolve(dir, target, &outputs))
            .collect();
        let scripts: BTreeSet<usize> = script_tokens(json)
            .iter()
            .flat_map(|token| walk.resolve(dir, token, &outputs))
            .collect();
        reach.manifest_roots = entries.len();
        reach.script_roots = scripts.len();
        for index in entries.into_iter().chain(scripts) {
            roots[index] = true;
        }
    }

    for include in &service.include {
        let include_root = canonical(&repo.join(include));
        for (index, file) in files.iter().enumerate() {
            if file.starts_with(&include_root) {
                roots[index] = true;
            }
        }
    }

    if let Some(config) = governing_config(repo, &service_root, service) {
        let rules = ProgramRules::new(repo, &config, &internal);
        let config_dir = config.parent().unwrap_or(repo).to_path_buf();
        let matched: Vec<bool> = files.iter().map(|file| rules.lists(file)).collect();
        let narrows = files
            .iter()
            .zip(&matched)
            .any(|(file, listed)| file.starts_with(&config_dir) && !listed);
        if rules.is_empty() {
            reach.program = Program::None;
        } else if narrows {
            reach.program = Program::Narrows;
            for (index, listed) in matched.into_iter().enumerate() {
                roots[index] |= listed;
            }
        } else {
            reach.program = Program::WholeTree;
        }
    }

    let mut dependencies: BTreeSet<String> = manifest
        .as_ref()
        .map(|(_, json)| dependency_names(json))
        .unwrap_or_default();
    dependencies.extend(crate::packages::workspace_root_dependency_names(
        repo,
        &service_root,
    ));
    let dependencies: Vec<String> = dependencies.into_iter().collect();
    let conventions = crate::file_based_router::builtin_conventions(&dependencies);
    if !conventions.is_empty() {
        for (index, file) in files.iter().enumerate() {
            let Ok(relative) = file.strip_prefix(&service_root) else {
                continue;
            };
            if crate::file_based_router::derive_route(relative, &conventions).is_some() {
                roots[index] = true;
                reach.route_roots += 1;
            }
        }
    }

    let paths: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
    let reached = crate::file_finder::import_closure(&paths, |index| roots[index], &modules);
    reach.reached = reached.iter().filter(|reached| **reached).count();
    reach.unclaimed = files.len() - reach.reached;

    let units = offered_units(
        repo,
        &service_root,
        files,
        &reached,
        &walk,
        &modules,
        &internal,
        reach.program,
    );
    ServiceReach { reach, units }
}

/// The service's files, looked up by the path a resolver answers with.
struct Walk {
    by_path: HashMap<PathBuf, usize>,
    sorted: Vec<(String, usize)>,
}

impl Walk {
    fn new(files: &[PathBuf]) -> Self {
        let by_path = files
            .iter()
            .enumerate()
            .map(|(index, file)| (canonical(file), index))
            .collect();
        let mut sorted: Vec<(String, usize)> = files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.to_string_lossy().into_owned(), index))
            .collect();
        sorted.sort();
        Walk { by_path, sorted }
    }

    fn index(&self, path: &Path) -> Option<usize> {
        self.by_path.get(&canonical(path)).copied()
    }

    /// The walk files a manifest target names, relative to `dir`: as
    /// written, with each source extension appended, or as a directory's
    /// `index`. A target with a `*` is a subpath pattern and names every walk
    /// file under its static prefix. A target in build output names the
    /// source it was built from ([`BuildOutput`]).
    fn resolve(&self, dir: &Path, target: &str, outputs: &[BuildOutput]) -> Vec<usize> {
        if let Some(star) = target.find('*') {
            let prefix = normalize(&dir.join(&target[..star]));
            let mut prefix = prefix.to_string_lossy().into_owned();
            if target[..star].ends_with('/') && !prefix.ends_with('/') {
                prefix.push('/');
            }
            if prefix.is_empty() {
                return Vec::new();
            }
            let start = self
                .sorted
                .partition_point(|(path, _)| path.as_str() < prefix.as_str());
            return self.sorted[start..]
                .iter()
                .take_while(|(path, _)| path.starts_with(&prefix))
                .map(|(_, index)| *index)
                .collect();
        }
        let base = normalize(&dir.join(target));
        if let Some(found) = self.source_at(&base) {
            return vec![found];
        }
        outputs
            .iter()
            .flat_map(|output| output.sources_of(&base))
            .find_map(|stem| self.source_at(&stem))
            .into_iter()
            .collect()
    }

    /// The walk file at `base` as written, with a source extension
    /// appended, or as a directory's `index`.
    fn source_at(&self, base: &Path) -> Option<usize> {
        let mut candidates = vec![base.to_path_buf()];
        for extension in SOURCE_EXTENSIONS {
            candidates.push(PathBuf::from(format!("{}.{extension}", base.display())));
        }
        for extension in SOURCE_EXTENSIONS {
            candidates.push(base.join(format!("index.{extension}")));
        }
        candidates
            .iter()
            .filter(|candidate| candidate.is_file())
            .find_map(|candidate| self.index(candidate))
    }
}

/// Where one config at a manifest's directory emits (`outDir`) and the
/// source directories it emits from: its `rootDir`, else the static prefix
/// of each `include` entry, else its own directory. Each key is decided by the
/// nearest config in the `extends` chain that states it.
struct BuildOutput {
    out_dir: PathBuf,
    source_dirs: Vec<PathBuf>,
}

impl BuildOutput {
    /// The source stems (no extension) a path under `out_dir` was built from:
    /// the same relative path under each source directory, its output
    /// extension (`.js`, `.mjs`, `.cjs`, `.jsx`, `.d.ts`, `.d.mts`, `.d.cts`)
    /// removed.
    fn sources_of(&self, built: &Path) -> Vec<PathBuf> {
        let Ok(relative) = built.strip_prefix(&self.out_dir) else {
            return Vec::new();
        };
        let text = relative.to_string_lossy();
        let stem = [".d.ts", ".d.mts", ".d.cts", ".js", ".mjs", ".cjs", ".jsx"]
            .iter()
            .find_map(|extension| text.strip_suffix(extension))
            .unwrap_or(&text);
        self.source_dirs.iter().map(|dir| dir.join(stem)).collect()
    }
}

/// The [`BuildOutput`] of every `tsconfig*.json` at `dir`, in name order.
fn build_outputs(
    repo: &Path,
    dir: &Path,
    internal: &BTreeMap<String, PathBuf>,
) -> Vec<BuildOutput> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut configs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("tsconfig") && name.ends_with(".json"))
        })
        .collect();
    configs.sort();
    configs
        .iter()
        .filter_map(|config| {
            let relative = config.strip_prefix(repo).ok()?;
            let (chain, _) = crate::module_aliases::tsconfig_chain(repo, relative, internal);
            let option = |key: &str| -> Option<PathBuf> {
                chain.iter().find_map(|(declared_in, json)| {
                    let value = json.get("compilerOptions")?.get(key)?.as_str()?;
                    let base = repo.join(declared_in.parent().unwrap_or(Path::new("")));
                    Some(normalize(&base.join(value)))
                })
            };
            let out_dir = option("outDir")?;
            let source_dirs = match option("rootDir") {
                Some(root_dir) => vec![root_dir],
                None => chain
                    .iter()
                    .find_map(|(declared_in, json)| {
                        let entries = json.get("include")?.as_array()?;
                        let base = repo.join(declared_in.parent().unwrap_or(Path::new("")));
                        Some(
                            entries
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(|entry| {
                                    normalize(&base.join(crate::change_scope::static_prefix(entry)))
                                })
                                .collect::<Vec<_>>(),
                        )
                    })
                    .unwrap_or_else(|| vec![config.parent().unwrap_or(dir).to_path_buf()]),
            };
            Some(BuildOutput {
                out_dir,
                source_dirs,
            })
        })
        .collect()
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Every target a manifest's entry fields name.
fn manifest_entries(json: &serde_json::Value) -> Vec<String> {
    let mut targets = Vec::new();
    for key in ["main", "module", "browser", "types", "typings"] {
        if let Some(target) = json.get(key).and_then(serde_json::Value::as_str) {
            targets.push(target.to_string());
        }
    }
    match json.get("bin") {
        Some(serde_json::Value::String(one)) => targets.push(one.clone()),
        Some(serde_json::Value::Object(many)) => targets.extend(
            many.values()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string),
        ),
        _ => {}
    }
    if let Some(exports) = json.get("exports") {
        string_leaves(exports, &mut targets);
    }
    targets
}

/// Every string at any depth of an `exports` value (conditions nest).
fn string_leaves(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(target) => out.push(target.clone()),
        serde_json::Value::Array(items) => items.iter().for_each(|item| string_leaves(item, out)),
        serde_json::Value::Object(map) => map.values().for_each(|item| string_leaves(item, out)),
        _ => {}
    }
}

/// The tokens of every `scripts` command (a Deno config's `tasks`, written
/// as a string or as `{ "command": … }`), split on whitespace and shell
/// operators, quotes stripped.
fn script_tokens(json: &serde_json::Value) -> Vec<String> {
    ["scripts", "tasks"]
        .into_iter()
        .filter_map(|key| json.get(key).and_then(serde_json::Value::as_object))
        .flat_map(|commands| commands.values())
        .filter_map(|command| {
            command
                .as_str()
                .or_else(|| command.get("command").and_then(serde_json::Value::as_str))
        })
        .flat_map(|command| {
            command
                .split(|c: char| {
                    c.is_whitespace() || matches!(c, '&' | '|' | ';' | '(' | ')' | '=')
                })
                .map(|token| token.trim_matches(|c| c == '"' || c == '\'').to_string())
                .filter(|token| !token.is_empty())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The dependency names a manifest declares, every kind.
fn dependency_names(json: &serde_json::Value) -> BTreeSet<String> {
    [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ]
    .into_iter()
    .filter_map(|key| json.get(key).and_then(serde_json::Value::as_object))
    .flat_map(|map| map.keys().cloned())
    .collect()
}

/// The config that governs `service`: the one carrick.json names, else the
/// nearest `tsconfig.json` at or above its directory, or a `jsconfig.json`
/// at a level that holds no `tsconfig.json`.
fn governing_config(repo: &Path, service_root: &Path, service: &Config) -> Option<PathBuf> {
    if let Some(named) = &service.tsconfig {
        // A member's config names one above it (`../../tsconfig.json`).
        let path = canonical(&service_root.join(named));
        let deno = path
            .file_name()
            .is_some_and(|name| name == "deno.json" || name == "deno.jsonc");
        return (path.is_file() && !deno).then_some(path);
    }
    service_root
        .ancestors()
        .take_while(|dir| dir.starts_with(repo))
        .find_map(|dir| {
            ["tsconfig.json", "jsconfig.json"]
                .into_iter()
                .map(|name| dir.join(name))
                .find(|path| path.is_file())
        })
}

/// One file's loads and whether it exports anything, from one parse.
struct FileFacts {
    loads: Vec<String>,
    exports: bool,
}

fn file_facts(file: &Path) -> FileFacts {
    let source_map: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_emitter_writer(Box::new(std::io::sink()), None);
    match crate::parser::parse_file(file, &source_map, &handler) {
        Some(module) => FileFacts {
            loads: crate::parser::module_loads(&module),
            exports: exports_anything(&module),
        },
        None => FileFacts {
            loads: Vec::new(),
            exports: false,
        },
    }
}

/// An ES export (declarations, `export default`, `export *`, `export =`) or a
/// CommonJS one (`module.exports`, `exports.<name>` at module top level).
fn exports_anything(module: &Module) -> bool {
    module.body.iter().any(|item| match item {
        ModuleItem::ModuleDecl(declaration) => matches!(
            declaration,
            ModuleDecl::ExportDecl(_)
                | ModuleDecl::ExportNamed(_)
                | ModuleDecl::ExportDefaultDecl(_)
                | ModuleDecl::ExportDefaultExpr(_)
                | ModuleDecl::ExportAll(_)
                | ModuleDecl::TsExportAssignment(_)
        ),
        ModuleItem::Stmt(Stmt::Expr(statement)) => match statement.expr.as_ref() {
            Expr::Assign(assignment) => match &assignment.left {
                AssignTarget::Simple(SimpleAssignTarget::Member(target)) => {
                    crate::commonjs::assignment_target(target).is_some()
                }
                _ => false,
            },
            _ => false,
        },
        _ => false,
    })
}

/// The units below `service_root` and which of them nothing outside loads.
#[allow(clippy::too_many_arguments)]
fn offered_units(
    repo: &Path,
    service_root: &Path,
    files: &[PathBuf],
    reached: &[bool],
    walk: &Walk,
    modules: &WorkspaceIndex,
    internal: &BTreeMap<String, PathBuf>,
    service_program: Program,
) -> Vec<Unit> {
    // Service-relative paths of the files under the service directory.
    let relative: Vec<Option<String>> = files
        .iter()
        .map(|file| {
            file.strip_prefix(service_root)
                .ok()
                .map(|path| path.to_string_lossy().replace('\\', "/"))
        })
        .collect();
    // Every directory below the service directory that holds a reached file.
    let mut claimed_dirs: BTreeSet<String> = BTreeSet::new();
    for (index, path) in relative.iter().enumerate() {
        let Some(path) = path else { continue };
        if !reached[index] {
            continue;
        }
        let segments: Vec<&str> = path.split('/').collect();
        for depth in 1..segments.len() {
            claimed_dirs.insert(segments[..depth].join("/"));
        }
    }
    // Each unclaimed file's unit: its outermost directory below the service
    // directory that holds no reached file, else the file itself.
    let mut unit_of: Vec<Option<usize>> = vec![None; files.len()];
    let mut units: BTreeMap<String, (UnitKind, Vec<usize>)> = BTreeMap::new();
    for (index, path) in relative.iter().enumerate() {
        let Some(path) = path else { continue };
        if reached[index] {
            continue;
        }
        let segments: Vec<&str> = path.split('/').collect();
        let unit = (1..segments.len())
            .map(|depth| segments[..depth].join("/"))
            .find(|dir| !claimed_dirs.contains(dir))
            .map(|dir| (dir, UnitKind::Dir))
            .unwrap_or_else(|| (path.clone(), UnitKind::File));
        units
            .entry(unit.0)
            .or_insert_with(|| (unit.1, Vec::new()))
            .1
            .push(index);
    }
    let names: Vec<String> = units.keys().cloned().collect();
    for (id, (_, members)) in units.values().enumerate() {
        for &index in members {
            unit_of[index] = Some(id);
        }
    }

    // What each unclaimed file loads, from one parse each. A reached file
    // loads only reached files, by construction of the closure.
    let mut facts: HashMap<usize, (Vec<usize>, bool)> = HashMap::new();
    for (index, unit) in unit_of.iter().enumerate() {
        if unit.is_none() {
            continue;
        }
        let FileFacts { loads, exports } = file_facts(&files[index]);
        let targets: Vec<usize> = loads
            .iter()
            .filter_map(|specifier| modules.resolve_module_path(&files[index], specifier))
            .filter_map(|target| walk.index(&target))
            .collect();
        facts.insert(index, (targets, exports));
    }
    let mut loaded_from_outside = vec![false; names.len()];
    for (index, (targets, _)) in &facts {
        let source = unit_of[*index];
        for target in targets {
            if let Some(target_unit) = unit_of[*target]
                && Some(target_unit) != source
            {
                loaded_from_outside[target_unit] = true;
            }
        }
    }

    let representatives = Representatives::new(&relative);
    let mut repo_manifests: Option<Vec<(String, BTreeSet<String>)>> = None;
    let mut offered = Vec::new();
    for (id, (name, (kind, members))) in units.into_iter().enumerate() {
        if loaded_from_outside[id] {
            continue;
        }
        let pattern = unit_pattern(&name, kind);
        if !representatives.is_exact(service_root, &pattern, &name, kind) {
            continue;
        }
        let unit_relative = |index: usize| -> String {
            let path = relative[index].as_deref().unwrap_or("");
            match kind {
                UnitKind::Dir => path[name.len() + 1..].to_string(),
                UnitKind::File => path.rsplit('/').next().unwrap_or(path).to_string(),
            }
        };
        let mut all_paths: Vec<(String, usize)> = members
            .iter()
            .map(|&index| (unit_relative(index), index))
            .collect();
        all_paths.sort();
        let paths: Vec<String> = all_paths
            .iter()
            .take(MAX_PATHS)
            .map(|(path, _)| path.clone())
            .collect();
        let samples = all_paths
            .iter()
            .take(MAX_SAMPLES)
            .map(|(path, index)| Sample {
                file: path.clone(),
                head: head_lines(&files[*index]),
            })
            .collect();
        let exporting_files = members
            .iter()
            .filter(|index| facts.get(index).is_some_and(|(_, exports)| *exports))
            .count();
        let mut imports_from: BTreeSet<String> = BTreeSet::new();
        for index in &members {
            for &target in facts
                .get(index)
                .map(|(targets, _)| targets.as_slice())
                .unwrap_or(&[])
            {
                if reached[target] {
                    let segment = match &relative[target] {
                        Some(path) => path.split('/').next().unwrap_or("").to_string(),
                        None => files[target]
                            .strip_prefix(repo)
                            .ok()
                            .and_then(|path| path.iter().next())
                            .map(|segment| segment.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    };
                    imports_from.insert(segment);
                } else if let Some(other) = unit_of[target]
                    && other != id
                {
                    imports_from.insert(names[other].clone());
                }
            }
        }
        imports_from.remove("");
        let unit_dir = service_root.join(&name);
        let (configs, manifest_paths) = match kind {
            UnitKind::Dir => configs_and_manifests(&unit_dir),
            UnitKind::File => (Vec::new(), Vec::new()),
        };
        let own = configs.iter().any(|config| {
            let rules = ProgramRules::new(repo, config, internal);
            members.iter().any(|&index| rules.lists(&files[index]))
        });
        let program = unit_program(service_program, own);
        let manifests = manifest_paths
            .iter()
            .take(MAX_MANIFESTS)
            .filter_map(|path| {
                let json = crate::packages::read_json_config(path).ok()?;
                let name = json
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let own = path
                    .strip_prefix(repo)
                    .map(|path| path.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                let depended_on_by = match &name {
                    Some(name) => repo_manifests
                        .get_or_insert_with(|| all_manifests(repo))
                        .iter()
                        .filter(|(path, dependencies)| *path != own && dependencies.contains(name))
                        .map(|(path, _)| path.clone())
                        .collect(),
                    None => Vec::new(),
                };
                Some(UnitManifest {
                    path: path
                        .strip_prefix(&unit_dir)
                        .map(|path| path.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_default(),
                    name,
                    description: json
                        .get("description")
                        .and_then(serde_json::Value::as_str)
                        .map(|text| text.chars().take(DESCRIPTION_CHARS).collect()),
                    private: json
                        .get("private")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    has_entries: [
                        "main", "module", "browser", "types", "typings", "bin", "exports",
                    ]
                    .iter()
                    .any(|key| json.get(*key).is_some()),
                    depended_on_by,
                })
            })
            .collect();
        offered.push(Unit {
            unit: name,
            kind,
            pattern,
            files: members.len(),
            paths,
            program,
            manifests,
            exporting_files,
            imports_from: imports_from.into_iter().take(MAX_IMPORTS_FROM).collect(),
            samples,
        });
    }
    offered
}

/// The unit's program fact, given the service's: a config inside the unit
/// that lists its files wins.
fn unit_program(service: Program, own: bool) -> UnitProgram {
    if own {
        return UnitProgram::Own;
    }
    match service {
        Program::Narrows => UnitProgram::Outside,
        Program::WholeTree => UnitProgram::WholeTree,
        Program::None => UnitProgram::None,
    }
}

/// The gitignore pattern anchored at the service directory that names
/// `unit`, every special character escaped.
fn unit_pattern(unit: &str, kind: UnitKind) -> String {
    let mut pattern = String::from("/");
    let segments: Vec<String> = unit
        .split('/')
        .map(|segment| {
            let mut escaped = String::new();
            for (position, c) in segment.chars().enumerate() {
                if matches!(c, '\\' | '[' | ']' | '*' | '?')
                    || (position == 0 && matches!(c, '!' | '#'))
                {
                    escaped.push('\\');
                }
                escaped.push(c);
            }
            escaped
        })
        .collect();
    pattern.push_str(&segments.join("/"));
    match kind {
        UnitKind::Dir => pattern.push('/'),
        UnitKind::File => {
            // Trailing spaces end a gitignore line unless escaped.
            let trimmed = pattern.trim_end_matches(' ').len();
            let spaces = pattern.len() - trimmed;
            pattern.truncate(trimmed);
            pattern.push_str(&"\\ ".repeat(spaces));
        }
    }
    pattern
}

/// For each unit depth, the distinct prefixes of that depth over the
/// service's files (a directory when the file sits deeper). An anchored
/// pattern with no `**` decides a path by its first segments only, so
/// checking one path per prefix checks every walk file.
struct Representatives {
    paths: Vec<Vec<String>>,
}

impl Representatives {
    fn new(relative: &[Option<String>]) -> Self {
        Representatives {
            paths: relative
                .iter()
                .flatten()
                .map(|path| path.split('/').map(str::to_string).collect())
                .collect(),
        }
    }

    /// Whether `pattern` alone passes [`crate::file_finder::Exclusion::check`]
    /// and, rooted at the service directory, leaves out exactly `unit`.
    fn is_exact(&self, service_root: &Path, pattern: &str, unit: &str, kind: UnitKind) -> bool {
        let patterns = [pattern.to_string()];
        if crate::file_finder::Exclusion::check(&patterns).is_err() {
            return false;
        }
        let Ok(exclusion) = crate::file_finder::Exclusion::build(service_root, &patterns) else {
            return false;
        };
        let depth = unit.split('/').count();
        let mut seen: BTreeSet<(String, bool)> = BTreeSet::new();
        for segments in &self.paths {
            let is_dir = segments.len() > depth;
            let prefix = segments[..segments.len().min(depth)].join("/");
            if !seen.insert((prefix.clone(), is_dir)) {
                continue;
            }
            let wanted = prefix == unit && is_dir == (kind == UnitKind::Dir);
            if exclusion.excludes(&service_root.join(&prefix), is_dir) != wanted {
                return false;
            }
        }
        true
    }
}

/// The first [`SAMPLE_LINES`] non-empty lines of `file`, each cut to
/// [`SAMPLE_CHARS`].
fn head_lines(file: &Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(file) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&bytes)
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.trim().is_empty())
        .take(SAMPLE_LINES)
        .map(|line| line.chars().take(SAMPLE_CHARS).collect())
        .collect()
}

/// Whether a directory is one no walk enters for manifests or configs.
fn skipped(entry: &walkdir::DirEntry) -> bool {
    entry.depth() > 0
        && entry.file_type().is_dir()
        && entry.file_name().to_str().is_some_and(|name| {
            name.starts_with('.') || crate::packages::MANIFEST_SKIP_DIRS.contains(&name)
        })
}

/// The tsconfig/jsconfig files and the `package.json` files under `dir`,
/// each sorted.
fn configs_and_manifests(dir: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut configs = Vec::new();
    let mut manifests = Vec::new();
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_entry(|entry| !skipped(entry))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        match entry.file_name().to_str() {
            Some("tsconfig.json" | "jsconfig.json") => configs.push(entry.into_path()),
            Some("package.json") => manifests.push(entry.into_path()),
            _ => {}
        }
    }
    configs.sort();
    manifests.sort();
    (configs, manifests)
}

/// Every `package.json` in the repository, repo-relative, with the names it
/// depends on.
fn all_manifests(repo: &Path) -> Vec<(String, BTreeSet<String>)> {
    let (_, manifests) = configs_and_manifests(repo);
    manifests
        .into_iter()
        .filter_map(|path| {
            let json = crate::packages::read_json_config(&path).ok()?;
            let relative = path
                .strip_prefix(repo)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            Some((relative, dependency_names(&json)))
        })
        .collect()
}

/// Cap one repository's listed units at [`MAX_REPO_UNITS`] over its services
/// in order, counting what is cut on the service it belongs to.
pub fn cap_repo_units(services: &mut [crate::derive_candidates::Candidates]) {
    let mut left = MAX_REPO_UNITS;
    for service in services {
        let keep = service.directories.len().min(left);
        let cut = service.directories.len() - keep;
        service.directories.truncate(keep);
        service.truncated.directories += cut;
        left -= keep;
    }
}
