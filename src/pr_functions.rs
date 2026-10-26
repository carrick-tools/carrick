//! The functions a pull request added, edited or removed (carrick#2285).
//!
//! A PR run describes every function whose intent hash misses main's cache,
//! and that is the wrong list to call "changed": the hash covers the callees'
//! descriptions, so the caller of an edited function misses without being
//! touched. The blob holds no hash of a body alone. So the functions are
//! named from the trees: each file git reports changed is read as its base
//! commit holds it and as the PR has it, both copies go through the extractor
//! discovery uses ([`FunctionDefinitionExtractor`]), and the definitions are
//! compared by name and by the source text of their spans.
//!
//! - in the PR's copy only: added;
//! - in both, and the text differs: edited;
//! - in both, same text: not listed, wherever its lines now sit;
//! - in the base copy only: removed.
//!
//! The text is compared as written, so a reformat reads as an edit. A copy
//! that does not parse states no definitions, as it states no rows in the
//! index. The module-scope row is not a function and is never listed.
//!
//! Each listed function is then joined to the row this run holds for it, by
//! the file path the uploaded blob carries and the name the index spells it
//! with, which is the key the cloud joins main's row by. The intent, the
//! callees and the callers come from those rows.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use swc_common::errors::Handler;
use swc_common::sync::Lrc;
use swc_common::{FileName, SourceMap, SourceMapper, Spanned};
use swc_ecma_visit::VisitWith;
use tracing::debug;

use crate::cloud_storage::CloudRepoData;
use crate::config::Config;
use crate::findings::{
    ChangedFunction, FunctionChange, FunctionCounts, FunctionRef, FunctionsBlock, FunctionsUnknown,
    MAX_CHANGED_FUNCTIONS, MAX_FUNCTION_EDGES, MAX_REMOVED_FUNCTIONS, NamedFunctions,
};
use crate::visitor::{FunctionDefinition, FunctionDefinitionExtractor, MODULE_SCOPE_KEY};

/// One definition as one copy of a file states it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stated {
    pub name: String,
    /// The source text of the definition's span.
    pub text: String,
    pub line: u32,
    pub end_line: u32,
    pub exported: bool,
    /// The extractor named it for an anonymous callback; no one wrote the name.
    pub anonymous: bool,
}

/// The definitions one copy of `path` states, by name. `label` names the copy
/// for the source map. Empty when the copy does not parse.
///
/// The extractor keys a file's definitions by name, so a name stated twice in
/// one file is one definition here, the one the index holds.
pub fn stated_in(path: &Path, label: String, source: String) -> BTreeMap<String, Stated> {
    let source_map: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_emitter_writer(Box::new(std::io::sink()), None);
    let Some(module) =
        crate::parser::parse_source(path, FileName::Custom(label), source, &source_map, &handler)
    else {
        return BTreeMap::new();
    };
    let mut extractor = FunctionDefinitionExtractor::new(path.to_path_buf(), source_map.clone());
    module.visit_with(&mut extractor);
    extractor.finalize_exports();
    extractor
        .function_definitions
        .into_iter()
        .filter(|(key, _)| key != MODULE_SCOPE_KEY)
        .filter_map(|(key, definition)| {
            let anonymous = extractor.synthetic_names.contains(&key);
            let span = definition.node_type.span();
            if span.is_dummy() {
                return None;
            }
            let text = source_map.span_to_snippet(span).ok()?;
            Some((
                definition.name.clone(),
                Stated {
                    name: definition.name,
                    text,
                    line: definition.line_number,
                    end_line: definition.end_line,
                    exported: definition.is_exported,
                    anonymous,
                },
            ))
        })
        .collect()
}

/// How one file's definitions differ between its two copies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDiff {
    pub added: Vec<Stated>,
    pub edited: Vec<Stated>,
    pub removed: Vec<String>,
}

/// Compare the definitions of a file's two copies by name and text.
pub fn diff_definitions(
    base: &BTreeMap<String, Stated>,
    head: &BTreeMap<String, Stated>,
) -> FileDiff {
    let mut diff = FileDiff::default();
    for (name, stated) in head {
        match base.get(name) {
            None => diff.added.push(stated.clone()),
            Some(before) if before.text != stated.text => diff.edited.push(stated.clone()),
            Some(_) => {}
        }
    }
    diff.removed = base
        .keys()
        .filter(|name| !head.contains_key(*name))
        .cloned()
        .collect();
    diff
}

/// One function the PR added or edited, before it is joined to its row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedChange {
    /// The file's path under the scan root, `/`-separated: the form the
    /// uploaded blob carries.
    pub file: String,
    /// The file's path at the base commit, in the same form, when git
    /// reports it renamed.
    pub previous_file: Option<String>,
    pub change: FunctionChange,
    pub stated: Stated,
}

/// What the trees say the PR changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Named {
    pub changed: Vec<NamedChange>,
    pub removed: Vec<FunctionRef>,
}

/// Which paths, as the blob carries them, are a scanned service's source.
pub struct Scope<'a> {
    /// A path in the PR's tree.
    pub head: &'a dyn Fn(&str) -> bool,
    /// A path only the base commit holds: a deleted file, or the old path of
    /// a renamed one.
    pub base: &'a dyn Fn(&str) -> bool,
}

/// `path` under `root`, in the form a row's `file_path` uploads in: the
/// segments below the root, joined by `/`. `None` for a path outside it.
///
/// Read segment by segment, never as text, so a `.` segment counts for
/// nothing wherever it sits: a scan root of `.` with a service directory of
/// `.` walks `././src/a.ts`, and that is `src/a.ts`.
fn blob_path(path: &Path, root: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    Some(
        relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// Name the functions that differ between commits `base` and `head`, for the
/// files `scope` counts. Both sides are read from git: a PR is its commits,
/// and a file a build step wrote or rewrote in the working tree is not one.
///
/// A file is compared with its own earlier copy, and a renamed file with the
/// copy at its old path. A file added whole states every function as added,
/// one deleted whole states every function as removed. A file that moved
/// into or out of scope is added or removed whole, because the index holds
/// it on one side only.
///
/// `Err` means git could not answer: a commit this clone does not hold, or a
/// file in it that it cannot read.
pub fn name_changes(
    repo_path: &str,
    base: &str,
    head: &str,
    scope: &Scope<'_>,
) -> Result<Named, String> {
    let canonical = |path: PathBuf| path.canonicalize().unwrap_or(path);
    let top = canonical(crate::git_state::toplevel(repo_path)?);
    let root = canonical(PathBuf::from(repo_path));
    // A path git names, in the form the blob carries. `None` for one outside
    // the scan root, which no row is at.
    let as_blob = |path: &str| blob_path(&top.join(path), &root);
    let copy_at = |commit: &str, path: &str| -> Result<BTreeMap<String, Stated>, String> {
        let source = crate::git_state::file_at(repo_path, commit, path)?;
        Ok(stated_in(
            Path::new(path),
            format!("{commit}:{path}"),
            source,
        ))
    };
    let base_copy = |path: &str| copy_at(base, path);
    let head_copy = |path: &str| copy_at(head, path);

    let mut named = Named::default();
    for change in crate::git_state::changed_files(repo_path, base, head)? {
        let (new, old) = match change.status {
            // A copy leaves its source where it was, so it is a new file.
            'A' | 'C' => (Some(change.path.as_str()), None),
            'D' => (None, Some(change.path.as_str())),
            'R' => (Some(change.path.as_str()), change.previous.as_deref()),
            'M' | 'T' => (Some(change.path.as_str()), Some(change.path.as_str())),
            // Nothing else names a file with two readable copies.
            _ => continue,
        };
        let new =
            new.and_then(|path| Some((path, as_blob(path).filter(|file| (scope.head)(file))?)));
        let old = old.and_then(|path| {
            let file = as_blob(path)?;
            let scoped = match &new {
                // The same path on both sides is in scope on both or neither.
                Some((new_path, _)) if *new_path == path => true,
                Some(_) | None => (scope.base)(&file),
            };
            scoped.then_some((path, file))
        });
        // A modified file out of scope in the PR's tree is out of scope.
        if new.is_none() && matches!(change.status, 'M' | 'T') {
            continue;
        }
        let before = match &old {
            Some((path, _)) => base_copy(path)?,
            None => BTreeMap::new(),
        };
        let after = match &new {
            Some((path, _)) => head_copy(path)?,
            None => BTreeMap::new(),
        };
        let diff = diff_definitions(&before, &after);
        if let Some((_, file)) = &new {
            let previous_file = old
                .as_ref()
                .map(|(_, file)| file.clone())
                .filter(|previous| previous != file);
            for (change, stated) in diff
                .added
                .into_iter()
                .map(|stated| (FunctionChange::Added, stated))
                .chain(
                    diff.edited
                        .into_iter()
                        .map(|stated| (FunctionChange::Edited, stated)),
                )
            {
                named.changed.push(NamedChange {
                    file: file.clone(),
                    previous_file: previous_file.clone(),
                    change,
                    stated,
                });
            }
        }
        if let Some((_, file)) = &old {
            named
                .removed
                .extend(diff.removed.into_iter().map(|name| FunctionRef {
                    file: file.clone(),
                    name,
                }));
        }
    }
    Ok(named)
}

/// A row's place in the index: the key the cloud joins by.
fn key_of(definition: &FunctionDefinition) -> (String, String) {
    (
        definition.file_path.to_string_lossy().into_owned(),
        definition.name.clone(),
    )
}

/// Sorted by file then name, one entry per function, at most
/// [`MAX_FUNCTION_EDGES`].
fn edges(refs: impl Iterator<Item = FunctionRef>) -> Vec<FunctionRef> {
    let sorted: BTreeSet<FunctionRef> = refs.collect();
    sorted.into_iter().take(MAX_FUNCTION_EDGES).collect()
}

/// Join what the trees named to the rows this run holds, and apply the caps.
///
/// `rows` is every function row of this repo's services, analysed or
/// carried, with its path in the blob's form. A listed function takes its
/// intent, its export flag and its callees from its own row, and its callers
/// from every row whose `calls` names it. The module-scope row is no
/// function and is neither joined nor named as a caller.
pub fn join_rows<'a>(
    named: Named,
    rows: impl Iterator<Item = &'a FunctionDefinition> + Clone,
) -> NamedFunctions {
    let functions = rows.filter(|row| row.name != MODULE_SCOPE_KEY);
    let by_key: HashMap<(String, String), &FunctionDefinition> =
        functions.clone().map(|row| (key_of(row), row)).collect();
    let mut callers: HashMap<(String, String), Vec<FunctionRef>> = HashMap::new();
    for row in functions {
        let (file, name) = key_of(row);
        for call in &row.calls {
            callers
                .entry((call.file_path.clone(), call.name.clone()))
                .or_default()
                .push(FunctionRef {
                    file: file.clone(),
                    name: name.clone(),
                });
        }
    }

    let mut changed: Vec<ChangedFunction> =
        named
            .changed
            .into_iter()
            .map(|change| {
                let key = (change.file, change.stated.name);
                let row = by_key.get(&key);
                ChangedFunction {
                    line: change.stated.line,
                    end_line: change.stated.end_line,
                    change: change.change,
                    exported: row.map_or(change.stated.exported, |row| row.is_exported),
                    intent: row
                        .and_then(|row| row.intent.clone())
                        .filter(|intent| !intent.trim().is_empty()),
                    previous_file: change.previous_file,
                    calls: edges(row.into_iter().flat_map(|row| &row.calls).map(|call| {
                        FunctionRef {
                            file: call.file_path.clone(),
                            name: call.name.clone(),
                        }
                    })),
                    called_by: edges(callers.get(&key).into_iter().flatten().cloned()),
                    file: key.0,
                    name: key.1,
                }
            })
            .collect();
    changed.sort_by(|a, b| {
        (a.change, !a.exported, &a.file, a.line, &a.name).cmp(&(
            b.change,
            !b.exported,
            &b.file,
            b.line,
            &b.name,
        ))
    });
    let mut removed: Vec<FunctionRef> = named
        .removed
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let count = |change: FunctionChange| changed.iter().filter(|f| f.change == change).count();
    let mut counts = FunctionCounts {
        added: count(FunctionChange::Added),
        edited: count(FunctionChange::Edited),
        removed: removed.len(),
        without_intent: 0,
        omitted: changed.len().saturating_sub(MAX_CHANGED_FUNCTIONS),
    };
    changed.truncate(MAX_CHANGED_FUNCTIONS);
    removed.truncate(MAX_REMOVED_FUNCTIONS);
    counts.without_intent = changed.iter().filter(|f| f.intent.is_none()).count();
    NamedFunctions {
        changed,
        removed,
        counts,
    }
}

/// The `functions` member of a PR run's result: what the PR's commits
/// changed since they left `base`'s history, joined to the rows the run holds
/// in `data`.
///
/// A path in the PR's tree is a service's source when the service's walk
/// reads it. A path only the base holds has no walk to ask, so it is judged
/// by the walk's path rules ([`crate::file_finder::service_would_read`]).
///
/// Never an error: a run that cannot name the functions says why in the
/// block, and its result is posted all the same.
pub fn block_for_run(
    repo_path: &str,
    base: Option<&str>,
    services: &[Config],
    data: &[CloudRepoData],
) -> FunctionsBlock {
    let Some(base) = base else {
        return FunctionsBlock::Unknown {
            unknown: FunctionsUnknown::NoBaseCommit,
        };
    };
    let skip = crate::packages::MANIFEST_SKIP_DIRS;
    // Walked on first use: a PR that touches no source file never asks.
    let walked: std::cell::OnceCell<BTreeSet<String>> = std::cell::OnceCell::new();
    let in_head = |file: &str| {
        walked
            .get_or_init(|| {
                services
                    .iter()
                    .flat_map(|service| {
                        crate::file_finder::find_service_files(repo_path, service, &skip).0
                    })
                    // The same form the diff's paths take (`as_blob`), or a
                    // changed file is never found among the walked ones.
                    .filter_map(|path| blob_path(&path, Path::new(repo_path)))
                    .collect()
            })
            .contains(file)
    };
    let in_base = |file: &str| {
        let path = Path::new(repo_path).join(file);
        services
            .iter()
            .any(|service| crate::file_finder::service_would_read(repo_path, service, &skip, &path))
    };
    // Only a path that could be source is worth a walk or a read.
    let source = |file: &str| crate::file_finder::has_source_extension(Path::new(file));
    let scope = Scope {
        head: &|file| source(file) && in_head(file),
        base: &|file| source(file) && in_base(file),
    };
    // What the PR changed is what its commits changed since they left the
    // base branch: the checked-out commit against its merge base with `base`.
    // On a merge checkout that is `base` itself. Where the checkout is the
    // PR's own head, `base` may hold commits the branch does not, and a diff
    // against it would read each of those backwards as the PR's: a function
    // main removed since would read as one the PR added.
    let named = crate::git_state::head_commit(repo_path).and_then(|head| {
        let from = crate::git_state::merge_base(repo_path, base, &head)?;
        name_changes(repo_path, &from, &head, &scope)
    });
    match named {
        Ok(named) => FunctionsBlock::Named(join_rows(
            named,
            data.iter()
                .flat_map(|service| service.function_definitions.values()),
        )),
        Err(reason) => {
            debug!("The functions this PR changed are not named: {reason}");
            FunctionsBlock::Unknown {
                unknown: FunctionsUnknown::BaseUnavailable,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_state::tests::{committed_repo, git_in};
    use serde_json::json;

    const A_BASE: &str = "\
export function f(x: number): number {
  return x + 1;
}

export function g(): string {
  return \"g\";
}

function h(): void {}
";

    /// `f`'s body changes, a comment is inserted above `g` so its lines move,
    /// `h` is deleted and `k` is new.
    const A_HEAD: &str = "\
export function f(x: number): number {
  return x + 2;
}

// A note about g,
// two lines long.
export function g(): string {
  return \"g\";
}

export function k(): number {
  return 3;
}
";

    const B: &str = "\
export function p(): number {
  return 1;
}

export const q = (): number => 2;

export class Shelf {
  count(): number {
    return p() + q();
  }
}
";

    const D: &str = "\
import { f } from \"./a\";

export function m(): number {
  return f(1);
}
";

    const E: &str = "\
import { k } from \"./a\";

export function n(): number {
  return k();
}
";

    /// The base tree, and its commit.
    fn base_repo() -> (tempfile::TempDir, String) {
        committed_repo(&[
            ("package.json", r#"{ "name": "fixture", "private": true }"#),
            ("a.ts", A_BASE),
            ("b.ts", B),
            ("d.ts", D),
        ])
    }

    /// The PR's tree on top of [`base_repo`]: `a.ts` edited, `b.ts` renamed
    /// to `c.ts` untouched, `d.ts` untouched, `e.ts` new.
    fn pr_tree(repo: &Path) -> String {
        std::fs::write(repo.join("a.ts"), A_HEAD).unwrap();
        git_in(repo, &["mv", "b.ts", "c.ts"]);
        std::fs::write(repo.join("e.ts"), E).unwrap();
        commit_all(repo)
    }

    /// Commit the working tree, and name the commit.
    fn commit_all(repo: &Path) -> String {
        git_in(repo, &["add", "-A"]);
        git_in(repo, &["commit", "-qm", "next"]);
        git_in(repo, &["rev-parse", "HEAD"])
    }

    fn everything() -> Scope<'static> {
        Scope {
            head: &|_| true,
            base: &|_| true,
        }
    }

    fn names(named: &Named) -> Vec<(String, String, FunctionChange)> {
        let mut names: Vec<_> = named
            .changed
            .iter()
            .map(|change| {
                (
                    change.file.clone(),
                    change.stated.name.clone(),
                    change.change,
                )
            })
            .collect();
        names.sort();
        names
    }

    fn at(file: &str, name: &str) -> FunctionRef {
        FunctionRef {
            file: file.to_string(),
            name: name.to_string(),
        }
    }

    /// The ticket's fixture: an edit, a move, a deletion, an addition, a
    /// renamed file, an untouched caller and a new file.
    #[test]
    fn the_trees_name_what_the_pr_added_edited_and_removed() {
        let (repo, base) = base_repo();
        let head = pr_tree(repo.path());
        let root = repo.path().to_str().unwrap();
        let named = name_changes(root, &base, &head, &everything()).unwrap();
        assert_eq!(
            names(&named),
            vec![
                ("a.ts".to_string(), "f".to_string(), FunctionChange::Edited),
                ("a.ts".to_string(), "k".to_string(), FunctionChange::Added),
                ("e.ts".to_string(), "n".to_string(), FunctionChange::Added),
            ],
            "g only moved, m was not touched, and c.ts is b.ts renamed"
        );
        assert_eq!(named.removed, vec![at("a.ts", "h")]);
        assert!(named.changed.iter().all(|c| c.previous_file.is_none()));
    }

    fn listed(block: &FunctionsBlock) -> (Vec<(String, String, FunctionChange)>, Vec<FunctionRef>) {
        let FunctionsBlock::Named(named) = block else {
            panic!("{block:?}")
        };
        let mut changed: Vec<_> = named
            .changed
            .iter()
            .map(|f| (f.file.clone(), f.name.clone(), f.change))
            .collect();
        changed.sort();
        (changed, named.removed.clone())
    }

    /// A branch behind its base. Main has since deleted `h` and edited `f`;
    /// the PR only adds `e.ts`. Against main's tip the PR's tree would read
    /// as adding `h` back and editing `f`. It did neither: what it changed is
    /// what its own commits changed since they left main's history.
    #[test]
    fn a_branch_behind_its_base_names_only_its_own_changes() {
        let (repo, branch_point) = base_repo();
        let path = repo.path();
        let root = path.to_str().unwrap();
        // The PR's branch, from the commit main was at.
        git_in(path, &["checkout", "-q", "-b", "pr"]);
        std::fs::write(path.join("e.ts"), E.replace("return k();", "return 1;")).unwrap();
        commit_all(path);
        // Main moves on.
        git_in(path, &["checkout", "-q", "main"]);
        std::fs::write(
            path.join("a.ts"),
            A_BASE
                .replace("return x + 1;", "return x + 5;")
                .replace("function h(): void {}\n", ""),
        )
        .unwrap();
        let main_tip = commit_all(path);
        // The run checks out the PR's head and is told main's tip.
        git_in(path, &["checkout", "-q", "pr"]);
        let services = [Config::default()];
        let own = (
            vec![("e.ts".to_string(), "n".to_string(), FunctionChange::Added)],
            Vec::new(),
        );
        assert_eq!(
            listed(&block_for_run(root, Some(&main_tip), &services, &[])),
            own
        );

        // A merge checkout descends from the base, so the base is the merge
        // base and the same changes are named.
        assert_eq!(
            listed(&block_for_run(root, Some(&branch_point), &services, &[])),
            own
        );
        git_in(path, &["merge", "-q", "--no-edit", "main"]);
        assert_eq!(
            listed(&block_for_run(root, Some(&main_tip), &services, &[])),
            own
        );
    }

    /// Only what is committed is the PR's: a file a build step generated
    /// before the scan, and a tracked file one rewrote, are not named.
    #[test]
    fn what_the_working_tree_holds_uncommitted_is_not_the_prs() {
        let (repo, base) = base_repo();
        pr_tree(repo.path());
        let root = repo.path().to_str().unwrap();
        let services = [Config::default()];
        let committed = listed(&block_for_run(root, Some(&base), &services, &[]));
        assert_eq!(committed.0.len(), 3, "{committed:?}");

        std::fs::write(
            repo.path().join("generated.ts"),
            "export function made(): void {}\n",
        )
        .unwrap();
        std::fs::write(repo.path().join("d.ts"), D.replace("f(1)", "f(2)")).unwrap();
        assert_eq!(
            listed(&block_for_run(root, Some(&base), &services, &[])),
            committed
        );
    }

    /// A renamed file is compared with the copy at its old path, and an
    /// edited function in it says where main holds it.
    #[test]
    fn a_function_edited_in_a_renamed_file_names_its_old_path() {
        let (repo, base) = base_repo();
        git_in(repo.path(), &["mv", "b.ts", "c.ts"]);
        std::fs::write(
            repo.path().join("c.ts"),
            B.replace("return 1;", "return 10;"),
        )
        .unwrap();
        let head = commit_all(repo.path());
        let root = repo.path().to_str().unwrap();
        let named = name_changes(root, &base, &head, &everything()).unwrap();
        assert_eq!(
            names(&named),
            vec![("c.ts".to_string(), "p".to_string(), FunctionChange::Edited)]
        );
        assert_eq!(named.changed[0].previous_file.as_deref(), Some("b.ts"));
        assert!(named.removed.is_empty());
    }

    /// A file that moves into scope is new to the index whole, and one that
    /// moves out of it leaves the index whole.
    #[test]
    fn a_file_that_crosses_the_scope_is_added_or_removed_whole() {
        let (repo, base) = base_repo();
        std::fs::create_dir_all(repo.path().join("vendor")).unwrap();
        git_in(repo.path(), &["mv", "b.ts", "vendor/b.ts"]);
        let head = commit_all(repo.path());
        let root = repo.path().to_str().unwrap();
        let outside = |file: &str| !file.starts_with("vendor/");
        let moved_out = Scope {
            head: &outside,
            base: &outside,
        };
        let named = name_changes(root, &base, &head, &moved_out).unwrap();
        assert!(named.changed.is_empty(), "{named:?}");
        assert_eq!(
            named.removed,
            vec![at("b.ts", "Shelf.count"), at("b.ts", "p"), at("b.ts", "q")]
        );

        let inside = |file: &str| file.starts_with("vendor/");
        let moved_in = Scope {
            head: &inside,
            base: &inside,
        };
        let named = name_changes(root, &base, &head, &moved_in).unwrap();
        assert_eq!(
            names(&named),
            vec![
                (
                    "vendor/b.ts".to_string(),
                    "Shelf.count".to_string(),
                    FunctionChange::Added
                ),
                (
                    "vendor/b.ts".to_string(),
                    "p".to_string(),
                    FunctionChange::Added
                ),
                (
                    "vendor/b.ts".to_string(),
                    "q".to_string(),
                    FunctionChange::Added
                ),
            ]
        );
        assert!(named.changed.iter().all(|c| c.previous_file.is_none()));
        assert!(named.removed.is_empty());
    }

    /// A scan rooted below the repository's top level names a file by its
    /// path under the scan root, which is the path its row carries, and says
    /// nothing of a file outside the root.
    #[test]
    fn a_file_is_named_by_its_path_under_the_scan_root() {
        let (repo, base) = committed_repo(&[
            ("svc/package.json", r#"{ "name": "svc" }"#),
            ("svc/src/a.ts", A_BASE),
            ("other/a.ts", A_BASE),
        ]);
        std::fs::write(repo.path().join("svc/src/a.ts"), A_HEAD).unwrap();
        std::fs::write(repo.path().join("other/a.ts"), A_HEAD).unwrap();
        let head = commit_all(repo.path());
        let root = repo.path().join("svc");
        let named = name_changes(root.to_str().unwrap(), &base, &head, &everything()).unwrap();
        assert_eq!(
            names(&named),
            vec![
                (
                    "src/a.ts".to_string(),
                    "f".to_string(),
                    FunctionChange::Edited
                ),
                (
                    "src/a.ts".to_string(),
                    "k".to_string(),
                    FunctionChange::Added
                ),
            ]
        );
        assert_eq!(named.removed, vec![at("src/a.ts", "h")]);
    }

    /// A copy that does not parse states no definitions, as it states no rows.
    #[test]
    fn a_copy_that_does_not_parse_states_nothing() {
        let (repo, base) = committed_repo(&[("a.ts", "export function f( {\n")]);
        std::fs::write(repo.path().join("a.ts"), A_BASE).unwrap();
        let head = commit_all(repo.path());
        let root = repo.path().to_str().unwrap();
        let named = name_changes(root, &base, &head, &everything()).unwrap();
        assert_eq!(named.changed.len(), 3, "{named:?}");
        assert!(
            named
                .changed
                .iter()
                .all(|c| c.change == FunctionChange::Added)
        );
    }

    fn block_json(block: &FunctionsBlock) -> serde_json::Value {
        serde_json::to_value(block).unwrap()
    }

    #[test]
    fn a_run_that_cannot_read_the_base_says_so_and_nothing_else() {
        let (repo, _) = base_repo();
        pr_tree(repo.path());
        let root = repo.path().to_str().unwrap();
        let services = [Config::default()];
        assert_eq!(
            block_json(&block_for_run(root, None, &services, &[])),
            json!({ "unknown": "no_base_commit" })
        );
        // A well-formed commit id this clone does not hold.
        let absent = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            block_json(&block_for_run(root, Some(absent), &services, &[])),
            json!({ "unknown": "base_unavailable" })
        );
    }

    /// The scope a run uses: the service's own walk for the PR's tree, and
    /// the walk's path rules for a file only the base holds.
    #[test]
    fn a_run_names_only_what_a_service_reads() {
        let (repo, base) = committed_repo(&[
            ("package.json", r#"{ "name": "fixture", "private": true }"#),
            ("a.ts", A_BASE),
            ("old.ts", "export function gone(): void {}\n"),
            ("old.test.ts", "export function probe(): void {}\n"),
            ("notes.md", "notes\n"),
        ]);
        std::fs::write(repo.path().join("a.ts"), A_HEAD).unwrap();
        std::fs::write(
            repo.path().join("a.test.ts"),
            "export function check(): void {}\n",
        )
        .unwrap();
        std::fs::write(repo.path().join("notes.md"), "more notes\n").unwrap();
        std::fs::remove_file(repo.path().join("old.ts")).unwrap();
        std::fs::remove_file(repo.path().join("old.test.ts")).unwrap();
        commit_all(repo.path());
        let root = repo.path().to_str().unwrap();
        let block = block_for_run(root, Some(&base), &[Config::default()], &[]);
        let FunctionsBlock::Named(named) = &block else {
            panic!("{block:?}")
        };
        let listed: Vec<(&str, &str)> = named
            .changed
            .iter()
            .map(|f| (f.file.as_str(), f.name.as_str()))
            .collect();
        assert_eq!(listed, vec![("a.ts", "k"), ("a.ts", "f")]);
        assert_eq!(named.removed, vec![at("a.ts", "h"), at("old.ts", "gone")]);
        assert_eq!(
            serde_json::to_value(named.counts).unwrap(),
            json!({ "added": 1, "edited": 1, "removed": 2, "without_intent": 2, "omitted": 0 })
        );
    }

    /// A path is named by its segments below the root, however many `.`
    /// segments the root and the service directory put in front of it. A
    /// scan of `.` whose service directory is `.` walks `././src/a.ts`.
    #[test]
    fn a_walked_path_is_named_by_its_segments_below_the_root() {
        let named = |path: &str, root: &str| blob_path(Path::new(path), Path::new(root));
        assert_eq!(named("././src/a.ts", "."), Some("src/a.ts".to_string()));
        assert_eq!(named("./src/a.ts", "."), Some("src/a.ts".to_string()));
        assert_eq!(
            named("/repo/./pkg/./src/a.ts", "/repo"),
            Some("pkg/src/a.ts".to_string())
        );
        assert_eq!(
            named("site/./src/a.ts", "site/"),
            Some("src/a.ts".to_string())
        );
        assert_eq!(named("/elsewhere/a.ts", "/repo"), None);
    }

    /// A service directory written with `.` segments reads the same files,
    /// and a function changed in one of them is named by the path its row
    /// carries.
    #[test]
    fn a_service_directory_written_with_dots_names_its_functions() {
        let (repo, base) = committed_repo(&[
            ("package.json", r#"{ "name": "fixture", "private": true }"#),
            ("pkg/a.ts", A_BASE),
            ("pkg/old.ts", "export function gone(): void {}\n"),
        ]);
        std::fs::write(repo.path().join("pkg/a.ts"), A_HEAD).unwrap();
        std::fs::remove_file(repo.path().join("pkg/old.ts")).unwrap();
        commit_all(repo.path());
        let service = Config {
            directory: Some("./pkg/.".to_string()),
            ..Config::default()
        };
        let block = block_for_run(repo.path().to_str().unwrap(), Some(&base), &[service], &[]);
        let FunctionsBlock::Named(named) = &block else {
            panic!("{block:?}")
        };
        let listed: Vec<(&str, &str)> = named
            .changed
            .iter()
            .map(|f| (f.file.as_str(), f.name.as_str()))
            .collect();
        assert_eq!(listed, vec![("pkg/a.ts", "k"), ("pkg/a.ts", "f")]);
        assert_eq!(
            named.removed,
            vec![at("pkg/a.ts", "h"), at("pkg/old.ts", "gone")]
        );
    }

    /// One function row as the blob holds it.
    fn row(
        file: &str,
        name: &str,
        exported: bool,
        intent: Option<&str>,
        calls: &[(&str, &str)],
    ) -> FunctionDefinition {
        serde_json::from_value(json!({
            "name": name,
            "file_path": file,
            "node_type": "Placeholder",
            "arguments": [],
            "is_exported": exported,
            "line_number": 1,
            "intent": intent,
            "calls": calls
                .iter()
                .map(|(file, name)| json!({ "name": name, "file_path": file, "line_number": 1 }))
                .collect::<Vec<_>>(),
        }))
        .expect("a function row reads back")
    }

    fn change(file: &str, name: &str, change: FunctionChange, line: u32) -> NamedChange {
        NamedChange {
            file: file.to_string(),
            previous_file: None,
            change,
            stated: Stated {
                name: name.to_string(),
                text: String::new(),
                line,
                end_line: line + 2,
                exported: false,
                anonymous: false,
            },
        }
    }

    /// The fixture's rows: `m` calls `f`, `n` calls `k`, and the file's own
    /// top level calls `f` too.
    #[test]
    fn a_listed_function_takes_its_intent_and_edges_from_the_rows() {
        let rows = [
            row("a.ts", "f", true, Some("Adds two to a number."), &[]),
            row("a.ts", "k", true, None, &[]),
            row("d.ts", "m", true, Some("Calls f."), &[("a.ts", "f")]),
            row("e.ts", "n", false, Some("  "), &[("a.ts", "k")]),
            row("d.ts", MODULE_SCOPE_KEY, false, None, &[("a.ts", "f")]),
        ];
        let named = Named {
            changed: vec![
                change("e.ts", "n", FunctionChange::Added, 3),
                change("a.ts", "f", FunctionChange::Edited, 1),
                change("a.ts", "k", FunctionChange::Added, 11),
            ],
            removed: vec![at("a.ts", "h")],
        };
        let block = FunctionsBlock::Named(join_rows(named, rows.iter()));
        assert_eq!(
            block_json(&block),
            json!({
                "changed": [
                    {
                        "file": "a.ts", "name": "k", "line": 11, "end_line": 13,
                        "change": "added", "exported": true,
                        "calls": [],
                        "called_by": [{ "file": "e.ts", "name": "n" }],
                    },
                    {
                        "file": "e.ts", "name": "n", "line": 3, "end_line": 5,
                        "change": "added", "exported": false,
                        "calls": [{ "file": "a.ts", "name": "k" }],
                        "called_by": [],
                    },
                    {
                        "file": "a.ts", "name": "f", "line": 1, "end_line": 3,
                        "change": "edited", "exported": true,
                        "intent": "Adds two to a number.",
                        "calls": [],
                        "called_by": [{ "file": "d.ts", "name": "m" }],
                    },
                ],
                "removed": [{ "file": "a.ts", "name": "h" }],
                "counts": { "added": 2, "edited": 1, "removed": 1, "without_intent": 2, "omitted": 0 },
            })
        );
    }

    #[test]
    fn the_lists_are_capped_and_the_counts_are_not() {
        let named =
            Named {
                changed: (0..120)
                    .map(|i| change("big.ts", &format!("f{i:03}"), FunctionChange::Added, i + 1))
                    .chain((0..5).map(|i| {
                        change("big.ts", &format!("e{i}"), FunctionChange::Edited, 500 + i)
                    }))
                    .collect(),
                removed: (0..250)
                    .map(|i| at("gone.ts", &format!("r{i:03}")))
                    .collect(),
            };
        // One function with more callers than an entry lists.
        let rows: Vec<FunctionDefinition> = (0..60)
            .map(|i| {
                row(
                    "callers.ts",
                    &format!("c{i:02}"),
                    false,
                    None,
                    &[("big.ts", "f000")],
                )
            })
            .collect();
        let block = join_rows(named, rows.iter());
        assert_eq!(block.changed.len(), MAX_CHANGED_FUNCTIONS);
        assert!(
            block
                .changed
                .iter()
                .all(|f| f.change == FunctionChange::Added),
            "added functions are listed before edited ones"
        );
        assert_eq!(block.changed[0].name, "f000");
        assert_eq!(block.changed[0].called_by.len(), MAX_FUNCTION_EDGES);
        assert_eq!(block.changed[0].called_by[0], at("callers.ts", "c00"));
        assert_eq!(block.removed.len(), MAX_REMOVED_FUNCTIONS);
        assert_eq!(
            serde_json::to_value(block.counts).unwrap(),
            json!({ "added": 120, "edited": 5, "removed": 250, "without_intent": 100, "omitted": 25 })
        );
    }

    /// A measurement, not a check: what the rule names for each of the last
    /// commits on a clone's first-parent history, each against its parent.
    /// It needs no model and no scan, only git and the parser.
    ///
    /// `CARRICK_MEASURE_REPO=<clone> cargo test --lib pr_functions::tests::measure -- --ignored --nocapture`
    /// (`CARRICK_MEASURE_COMMITS` sets how many, 20 by default).
    #[test]
    #[ignore = "a measurement over a clone named by CARRICK_MEASURE_REPO"]
    fn measure_what_the_rule_names_over_history() {
        let Ok(repo) = std::env::var("CARRICK_MEASURE_REPO") else {
            return;
        };
        let commits: usize = std::env::var("CARRICK_MEASURE_COMMITS")
            .ok()
            .and_then(|count| count.parse().ok())
            .unwrap_or(20);
        let service = Config::default();
        let skip = crate::packages::MANIFEST_SKIP_DIRS;
        let read = |file: &str| {
            crate::file_finder::service_would_read(
                &repo,
                &service,
                &skip,
                &Path::new(&repo).join(file),
            )
        };
        let scope = Scope {
            head: &read,
            base: &read,
        };
        let log = git_in(
            Path::new(&repo),
            &[
                "log",
                "--first-parent",
                "--format=%H",
                "-n",
                &commits.to_string(),
            ],
        );
        println!("commit   added edited removed  files      ms");
        let (mut all_files, mut all_ms) = (0usize, 0u128);
        for commit in log.lines() {
            let parent = git_in(Path::new(&repo), &["rev-parse", &format!("{commit}^")]);
            // The changed source files the rule reads, for the cost of each.
            let files = crate::git_state::changed_files(&repo, &parent, commit)
                .map(|changed| {
                    changed
                        .iter()
                        .filter(|change| {
                            read(&change.path) || change.previous.as_deref().is_some_and(read)
                        })
                        .count()
                })
                .unwrap_or_default();
            let started = std::time::Instant::now();
            let named = name_changes(&repo, &parent, commit, &scope);
            let took = started.elapsed().as_millis();
            all_files += files;
            all_ms += took;
            match named {
                Ok(named) => {
                    let count = |change: FunctionChange| {
                        named.changed.iter().filter(|c| c.change == change).count()
                    };
                    println!(
                        "{} {:>5} {:>6} {:>7} {:>6} {:>7}",
                        &commit[..8],
                        count(FunctionChange::Added),
                        count(FunctionChange::Edited),
                        named.removed.len(),
                        files,
                        took
                    );
                    // The names, for a count made by hand to be checked
                    // against.
                    if std::env::var("CARRICK_MEASURE_NAMES").is_ok() {
                        for change in &named.changed {
                            println!(
                                "    {:?} {} {}",
                                change.change, change.file, change.stated.name
                            );
                        }
                        for removed in &named.removed {
                            println!("    Removed {} {}", removed.file, removed.name);
                        }
                    }
                }
                Err(reason) => println!("{} could not be read: {reason}", &commit[..8]),
            }
        }
        println!("total: {all_files} changed source file(s) in {all_ms} ms");
    }
}
