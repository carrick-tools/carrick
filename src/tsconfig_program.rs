//! Which files a tsconfig's program lists, and which project of a
//! `references` graph owns a file (carrick#3497).
//!
//! One membership reader for the scanner: the alias index hands a file to the
//! project that owns it ([`ProjectGraph`], the type sidecar's
//! `project-references.ts` rule), and derive reach reads the union of a
//! config's programs ([`ProgramRules`]). See
//! `docs/reference/module-resolution.md` for where this sits in the rule.
//!
//! Every path here is absolute and canonical: callers pass a canonical repo
//! root and configs under it.
//!
//! A project's membership ([`Program::lists`]) applies the compiler's
//! extension rule: a JavaScript file is listed only under `allowJs` (on by
//! default in a `jsconfig.json`). Derive reach reads the globs alone, so a
//! JavaScript service under a TypeScript-only config keeps its reach. Known
//! limit: the compiler's default `exclude` (`node_modules`, `outDir`) is not
//! applied.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::workspace_resolver::normalize;

/// How deep `references` are followed: the bound `extends` has.
const MAX_REFERENCE_DEPTH: usize = 16;

/// The files one config's own program lists: its `files`, and its `include`
/// minus its `exclude`, each key decided by the nearest config in its
/// `extends` chain that states it. `references` are not part of it.
#[derive(Debug, Clone)]
pub(crate) struct Program {
    /// The config, absolute.
    pub config: PathBuf,
    files: BTreeSet<PathBuf>,
    include: Vec<glob::Pattern>,
    exclude: Vec<glob::Pattern>,
    /// The configs its own `references` name, absolute, in declared order.
    references: Vec<PathBuf>,
    /// Whether the program takes JavaScript files (`allowJs`).
    allow_js: bool,
}

/// The extensions a program takes with and without `allowJs`, as the
/// compiler's `getSupportedExtensions` lists them.
const TS_EXTENSIONS: [&str; 4] = ["ts", "tsx", "mts", "cts"];
const JS_EXTENSIONS: [&str; 4] = ["js", "jsx", "mjs", "cjs"];

impl Program {
    /// Whether this config's own program lists `file` (absolute), as the
    /// compiler's file list does: named in its `files`, or a source file of a
    /// kind it takes under its `include` minus its `exclude`.
    pub(crate) fn lists(&self, file: &Path) -> bool {
        if self.files.contains(file) {
            return true;
        }
        let extension = file.extension().and_then(|e| e.to_str()).unwrap_or("");
        let takes = TS_EXTENSIONS.contains(&extension)
            || (self.allow_js && JS_EXTENSIONS.contains(&extension));
        takes && self.matches(file)
    }

    /// Whether `file` (absolute) is in its `files`, or matches its `include`
    /// and not its `exclude`, whatever its extension.
    fn matches(&self, file: &Path) -> bool {
        if self.files.contains(file) {
            return true;
        }
        let options = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: false,
        };
        self.include
            .iter()
            .any(|pattern| pattern.matches_path_with(file, options))
            && !self
                .exclude
                .iter()
                .any(|pattern| pattern.matches_path_with(file, options))
    }

    /// Read one config. `None` when it is outside `repo` or cannot be read.
    fn read(repo: &Path, config: &Path, internal: &BTreeMap<String, PathBuf>) -> Option<Self> {
        let relative = config.strip_prefix(repo).ok()?;
        let (chain, _) = crate::module_aliases::tsconfig_chain(repo, relative, internal);
        let (_, own) = chain.first()?;
        let config_dir = config.parent().unwrap_or(repo).to_path_buf();
        // The nearest config that states a key decides it, its entries
        // relative to the config that declares it.
        let stated = |key: &str| -> Option<(PathBuf, Vec<String>)> {
            chain.iter().find_map(|(declared_in, json)| {
                let entries = json.get(key)?.as_array()?;
                Some((
                    repo.join(declared_in.parent().unwrap_or(Path::new(""))),
                    entries
                        .iter()
                        .filter_map(|entry| entry.as_str().map(str::to_string))
                        .collect(),
                ))
            })
        };
        // A `jsconfig.json` takes JavaScript unless its chain says otherwise.
        let allow_js = chain
            .iter()
            .find_map(|(_, json)| json.get("compilerOptions")?.get("allowJs")?.as_bool())
            .unwrap_or_else(|| config.file_name() == Some("jsconfig.json".as_ref()));
        let files = stated("files");
        let include = stated("include");
        let exclude = stated("exclude");
        let listed = files
            .as_ref()
            .map(|(base, entries)| {
                entries
                    .iter()
                    .map(|entry| canonical(&normalize(&base.join(entry))))
                    .collect()
            })
            .unwrap_or_default();
        let include = match (include, &files) {
            (Some((base, entries)), _) => patterns(&base, &entries),
            (None, Some(_)) => Vec::new(),
            (None, None) => patterns(&config_dir, &["**/*".to_string()]),
        };
        let exclude = exclude
            .map(|(base, entries)| patterns(&base, &entries))
            .unwrap_or_default();
        // `references` is never inherited through `extends`.
        let references = own
            .get("references")
            .and_then(serde_json::Value::as_array)
            .map(|references| {
                references
                    .iter()
                    .filter_map(|reference| {
                        let path = reference.get("path")?.as_str()?;
                        let target = normalize(&config_dir.join(path));
                        let target = if target.is_dir() {
                            target.join("tsconfig.json")
                        } else {
                            target
                        };
                        target.is_file().then(|| canonical(&target))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Program {
            config: config.to_path_buf(),
            files: listed,
            include,
            exclude,
            references,
            allow_js,
        })
    }
}

/// `config` and every config its `references` reach, in the compiler's search
/// order: the config, then its references depth-first in declared order, a
/// config reached twice visited once. Unreadable configs are skipped.
fn search_order(repo: &Path, config: &Path, internal: &BTreeMap<String, PathBuf>) -> Vec<Program> {
    fn visit(
        repo: &Path,
        config: &Path,
        depth: usize,
        internal: &BTreeMap<String, PathBuf>,
        visited: &mut BTreeSet<PathBuf>,
        order: &mut Vec<Program>,
    ) {
        let Some(program) = Program::read(repo, config, internal) else {
            return;
        };
        let references = program.references.clone();
        order.push(program);
        if depth >= MAX_REFERENCE_DEPTH {
            return;
        }
        for reference in references {
            if visited.insert(reference.clone()) {
                visit(repo, &reference, depth + 1, internal, visited, order);
            }
        }
    }
    let mut visited = BTreeSet::from([config.to_path_buf()]);
    let mut order = Vec::new();
    visit(repo, config, 0, internal, &mut visited, &mut order);
    order
}

/// The union of the programs a config and its `references` list: what derive
/// reach reads as the service's program.
#[derive(Default)]
pub(crate) struct ProgramRules {
    programs: Vec<Program>,
}

impl ProgramRules {
    pub(crate) fn new(repo: &Path, config: &Path, internal: &BTreeMap<String, PathBuf>) -> Self {
        ProgramRules {
            programs: search_order(repo, config, internal),
        }
    }

    /// No config was read.
    pub(crate) fn is_empty(&self) -> bool {
        self.programs.is_empty()
    }

    pub(crate) fn lists(&self, file: &Path) -> bool {
        self.programs.iter().any(|program| program.matches(file))
    }
}

/// The projects a config reaches through `references`, and the owner of each
/// file among them: the type sidecar's `ProjectGraph`.
#[derive(Debug, Clone)]
pub(crate) struct ProjectGraph {
    /// Search order; the config itself first.
    order: Vec<Program>,
    /// Per entry of `order`: the indexes its reference closure reaches,
    /// without itself.
    closures: Vec<Vec<usize>>,
}

impl ProjectGraph {
    /// `None` when the config cannot be read.
    pub(crate) fn new(
        repo: &Path,
        config: &Path,
        internal: &BTreeMap<String, PathBuf>,
    ) -> Option<Self> {
        let order = search_order(repo, config, internal);
        order.first()?;
        let index: BTreeMap<&Path, usize> = order
            .iter()
            .enumerate()
            .map(|(i, program)| (program.config.as_path(), i))
            .collect();
        let closures = order
            .iter()
            .map(|start| {
                let mut seen = BTreeSet::from([start.config.as_path()]);
                let mut found = Vec::new();
                let mut stack: Vec<&Path> = start
                    .references
                    .iter()
                    .rev()
                    .map(PathBuf::as_path)
                    .collect();
                while let Some(next) = stack.pop() {
                    if !seen.insert(next) {
                        continue;
                    }
                    let Some(&i) = index.get(next) else {
                        continue;
                    };
                    found.push(i);
                    stack.extend(order[i].references.iter().rev().map(PathBuf::as_path));
                }
                found
            })
            .collect();
        Some(ProjectGraph { order, closures })
    }

    /// The config that owns `file` (absolute): the first project in search
    /// order that lists it while no project in its own reference closure
    /// does. `None` when no project in the graph lists it.
    pub(crate) fn owner_of(&self, file: &Path) -> Option<&Path> {
        self.order
            .iter()
            .zip(&self.closures)
            .find(|(program, closure)| {
                program.lists(file) && !closure.iter().any(|&i| self.order[i].lists(file))
            })
            .map(|(program, _)| program.config.as_path())
    }

    /// Every config in the graph, in search order.
    pub(crate) fn configs(&self) -> impl Iterator<Item = &Path> {
        self.order.iter().map(|program| program.config.as_path())
    }
}

pub(crate) fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Glob patterns for tsconfig `include`/`exclude` entries declared in
/// `base`: an entry whose last segment has no wildcard and no extension names
/// a directory (`<entry>/**/*`). The declaring directory is escaped; the
/// entry's own wildcards stay wildcards.
fn patterns(base: &Path, entries: &[String]) -> Vec<glob::Pattern> {
    entries
        .iter()
        .filter_map(|entry| {
            let mut entry = entry.trim_start_matches("./").to_string();
            let mut dir = base.to_path_buf();
            while let Some(rest) = entry.strip_prefix("../") {
                dir.pop();
                entry = rest.to_string();
            }
            let entry = entry.trim_end_matches('/');
            let last = entry.rsplit('/').next().unwrap_or("");
            let names_dir = !last.contains(['*', '?']) && !last.contains('.');
            // `Path::join("")` leaves a trailing separator.
            let mut text = glob::Pattern::escape(dir.to_string_lossy().trim_end_matches('/'));
            if !entry.is_empty() && entry != "." {
                text.push('/');
                text.push_str(entry);
            }
            if names_dir || entry.is_empty() || entry == "." {
                text.push_str("/**/*");
            }
            glob::Pattern::new(&text).ok()
        })
        .collect()
}
