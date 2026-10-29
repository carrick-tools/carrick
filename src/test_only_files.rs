//! The files only tests import (carrick#2775).
//!
//! The scan leaves test sources unread ([`crate::file_finder::find_files`])
//! because they never serve production. A helper that builds its own app for
//! the integration suite is the same thing under a name that escapes the
//! path conventions: no test directory, no `.test` or `.spec` suffix. What
//! marks it is who imports it. A source file is test-only when it has at
//! least one importer and every importer is a test source or another
//! test-only file.
//!
//! No entry rule: a file nothing imports is never test-only, and a
//! production entry that only a test imports reads as test-only. Program
//! membership from the tsconfig is the stronger signal and is not used, for
//! the reason `file_finder` gives (carrick#588).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::workspace_resolver::WorkspaceIndex;

/// The aliases that govern a service's module resolution
/// ([`crate::config::Config::alias_tsconfig`]): the one input of
/// [`of`] that varies between the services of a repo.
pub type AliasKey = Option<(PathBuf, PathBuf)>;

/// What the scan read of each file, once per process: the specifiers it
/// loads. Every service of a repo walks the same files, and a parse is the
/// cost (carrick#2786).
fn loaded_specifiers_of(reader: &mut crate::parser::ModuleReader, file: &Path) -> Arc<Vec<String>> {
    static READ: OnceLock<Mutex<HashMap<PathBuf, Arc<Vec<String>>>>> = OnceLock::new();
    let cache = READ.get_or_init(Default::default);
    if let Some(known) = cache.lock().ok().and_then(|held| held.get(file).cloned()) {
        return known;
    }
    #[cfg(test)]
    parse_count::bump(file);
    let read = Arc::new(reader.loaded_specifiers(file));
    if let Ok(mut held) = cache.lock() {
        held.insert(file.to_path_buf(), read.clone());
    }
    read
}

/// How many times each file was parsed in this process, for the test that
/// holds a second service to the first one's reads.
#[cfg(test)]
mod parse_count {
    use super::*;

    static PARSED: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();

    pub(super) fn bump(file: &Path) {
        *PARSED
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .entry(file.to_path_buf())
            .or_default() += 1;
    }

    pub(super) fn of(file: &Path) -> usize {
        PARSED
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .get(file)
            .copied()
            .unwrap_or(0)
    }
}

/// The files of `walked` that only tests import, as walked.
///
/// Importers are read from every source file below `repo_root` outside the
/// ignored folders, test sources and other services' files included, so a
/// sibling package's production code importing a helper (by relative path,
/// alias or workspace package name) keeps it in play. A specifier is read
/// wherever the file loads a module: `import`, `export ... from`,
/// `import x = require()`, and `require` or `import()` with a literal.
///
/// The set is worked out once per `(repo_root, alias, ignore_patterns)` in a
/// process, and a file is parsed once whatever the alias: `workspace` must be
/// the index built for `alias`, so services with the same aliases share one
/// answer (carrick#2786).
pub fn of(
    repo_root: &Path,
    walked: &[PathBuf],
    workspace: &WorkspaceIndex,
    alias: &AliasKey,
    ignore_patterns: &[&str],
) -> HashSet<PathBuf> {
    type Key = (PathBuf, AliasKey, Vec<String>);
    static SETS: OnceLock<Mutex<HashMap<Key, Arc<HashSet<PathBuf>>>>> = OnceLock::new();
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let key: Key = (
        repo_root.to_path_buf(),
        alias.clone(),
        ignore_patterns.iter().map(|p| p.to_string()).collect(),
    );
    // Held through the work: a second service asking for the same set waits
    // for the first instead of reading the tree again.
    let mut sets = SETS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let only = sets
        .entry(key)
        .or_insert_with(|| Arc::new(only_tests_import(repo_root, workspace, ignore_patterns)))
        .clone();
    drop(sets);
    walked
        .iter()
        .filter(|file| only.contains(&canonical(file)))
        .cloned()
        .collect()
}

/// The canonical files below `repo_root` that only tests import.
fn only_tests_import(
    repo_root: &Path,
    workspace: &WorkspaceIndex,
    ignore_patterns: &[&str],
) -> HashSet<PathBuf> {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut reader = crate::parser::ModuleReader::quiet();
    let mut importers: HashMap<PathBuf, HashSet<PathBuf>> = HashMap::new();
    let mut tests: HashSet<PathBuf> = HashSet::new();
    for file in crate::file_finder::source_files_below(repo_root, ignore_patterns) {
        let importer = canonical(&file);
        if crate::file_finder::is_test_source(&file, repo_root) {
            tests.insert(importer.clone());
        }
        for specifier in loaded_specifiers_of(&mut reader, &file).iter() {
            let Some(target) = workspace.resolve_module_path(&file, specifier) else {
                continue;
            };
            let target = canonical(&target);
            if target != importer {
                importers
                    .entry(target)
                    .or_default()
                    .insert(importer.clone());
            }
        }
    }
    let mut only: HashSet<PathBuf> = HashSet::new();
    loop {
        let before = only.len();
        for (file, by) in &importers {
            if !tests.contains(file)
                && !only.contains(file)
                && by
                    .iter()
                    .all(|who| tests.contains(who) || only.contains(who))
            {
                only.insert(file.clone());
            }
        }
        if only.len() == before {
            break;
        }
    }
    only
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn only_in(files: &[(&str, &str)]) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for (relative, text) in files {
            write(&root, relative, text);
        }
        let workspace = WorkspaceIndex::build_with_aliases(&root, None);
        let walked: Vec<PathBuf> = files
            .iter()
            .map(|(relative, _)| root.join(relative))
            .filter(|path| !crate::file_finder::is_test_source(path, &root))
            .collect();
        let mut out: Vec<String> = of(&root, &walked, &workspace, &None, &["node_modules"])
            .iter()
            .map(|path| path.strip_prefix(&root).unwrap().display().to_string())
            .collect();
        out.sort();
        out
    }

    /// A helper every importer of which is a test is test-only, through a
    /// helper it alone imports; one a production file also imports, by
    /// import, re-export or `require`, is not; and a file nothing imports is
    /// not.
    #[test]
    fn a_file_is_test_only_when_every_importer_is_a_test() {
        let got = only_in(&[
            ("src/a.integration.test.ts", "import './harness';\n"),
            ("src/harness.ts", "import './deep';\nexport const h = 1;\n"),
            ("src/deep.ts", "export const d = 1;\n"),
            ("src/b.test.ts", "import './shared';\nimport './barrel';\n"),
            ("src/shared.ts", "export const s = 1;\n"),
            ("src/prod.ts", "import { s } from './shared';\n"),
            ("src/barrel.ts", "export * from './reexported';\n"),
            ("src/reexported.ts", "export const r = 1;\n"),
            ("src/index.ts", "export * from './barrel';\n"),
            ("src/loaded.ts", "export const l = 1;\n"),
            ("src/c.test.ts", "const l = require('./loaded');\n"),
            ("src/loader.ts", "const l = require('./loaded');\n"),
            ("src/lonely.ts", "export const o = 1;\n"),
        ]);
        assert_eq!(got, vec!["src/deep.ts", "src/harness.ts"]);
    }

    /// Two services of one repo share the walk: the second call returns the
    /// same set and parses no file again, whether its aliases match the
    /// first's or not (carrick#2786).
    #[test]
    fn a_second_service_parses_no_file_again() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let files = [
            ("apps/a/src/app.ts", "export const a = 1;\n"),
            ("apps/b/src/app.ts", "import '../../../libs/helper';\n"),
            (
                "apps/b/src/app.test.ts",
                "import '../../../libs/only-tests';\n",
            ),
            ("libs/helper.ts", "export const h = 1;\n"),
            ("libs/only-tests.ts", "export const t = 1;\n"),
        ];
        for (relative, text) in files {
            write(&root, relative, text);
        }
        let walked: Vec<PathBuf> = files
            .iter()
            .map(|(relative, _)| root.join(relative))
            .filter(|path| !crate::file_finder::is_test_source(path, &root))
            .collect();
        let workspace = WorkspaceIndex::build_with_aliases(&root, None);
        let patterns = ["node_modules"];
        let first = of(&root, &walked, &workspace, &None, &patterns);
        assert_eq!(
            first,
            HashSet::from([root.join("libs/only-tests.ts")]),
            "the helper only a test imports"
        );
        let after_first: Vec<usize> = files
            .iter()
            .map(|(relative, _)| parse_count::of(&root.join(relative)))
            .collect();
        assert!(
            after_first.iter().all(|count| *count == 1),
            "{after_first:?}"
        );
        let other_alias = Some((
            PathBuf::from("apps/b"),
            PathBuf::from("apps/b/tsconfig.json"),
        ));
        for alias in [None, other_alias] {
            assert_eq!(of(&root, &walked, &workspace, &alias, &patterns), first);
        }
        let after_all: Vec<usize> = files
            .iter()
            .map(|(relative, _)| parse_count::of(&root.join(relative)))
            .collect();
        assert_eq!(after_all, after_first, "no file parsed twice");
    }
}
