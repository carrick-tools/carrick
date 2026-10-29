//! `carrick check --changed`: every file the work in hand touches, judged
//! against the index in one pass, as part of running the tests.
//!
//! The work in hand is everything that differs from where the checkout left
//! its repo's main branch: the commits on the branch since the merge base,
//! plus whatever is uncommitted, plus untracked source. Each repo holding such
//! a file is re-scanned once over its working tree, and every re-scanned repo
//! is joined once against the blobs the index already holds for everything
//! else ([`super::recheck::rejoin`], the same two phases `check --recheck`
//! runs for one file). The answer is about the changed files only: a file the
//! work did not touch is re-extracted with its repo but never reported, with
//! one exception — a call in an untouched file that the index matched and that
//! now reaches no route, because the work removed or moved the route it
//! called.
//!
//! The exit code is the one read in local mode that a verdict moves (see
//! `docs/local-mode-output.md`, "check --changed"): 1 when a problem is a fact,
//! 0 otherwise. A problem resting on a model candidate does not fail the run
//! and is counted on the summary line, listed only with `--all`. A check that
//! covered none of the files it was asked about, or could not run, exits
//! [`EXIT_NOTHING_COVERED`] (carrick#2850): a pass it did not look at is not
//! a pass. So does a run where nothing fails but a problem looks new only
//! because the index was written by another scanner build, which may not
//! have read that row (carrick#2911): the run names the build and says to
//! run `carrick refresh` on the main branch.
//!
//! `carrick check a.ts b.ts` runs the same pass over the files named instead
//! of the files changed ([`Selection::Named`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::join::{JoinedOperation, Role};
use super::read_model::{IndexedItem, LocalIndex, Source};

pub const SCHEMA: &str = "carrick.check-changed/0";

/// How long the whole pass may take before it stops and says it did not
/// finish. A ceiling, not the target: a typical branch is meant to finish in
/// well under half of it. Overridable so a test can force the miss.
pub const BUDGET_ENV: &str = "CARRICK_CHECK_BUDGET_MS";
// Five minutes: a 35-service monorepo measured 148 s on a release build
// (carrick#2826), and a check that stops short answers nothing.
const DEFAULT_BUDGET: Duration = Duration::from_secs(300);

/// One answer of `check --changed`.
#[derive(Serialize, Debug, Clone)]
pub struct ChangedOutput {
    pub schema: String,
    pub workspace: String,
    /// Changed source files checked, across every repo.
    pub files: usize,
    /// Routes and calls the working tree states in those files.
    pub routes: usize,
    pub calls: usize,
    /// Services the join read: the re-scanned ones and every one the index
    /// already held.
    pub services: usize,
    pub repos: Vec<ChangedRepo>,
    pub problems: Vec<Problem>,
    /// How many of `problems` fail the run.
    pub failing: usize,
    pub elapsed_ms: u64,
    /// Why nothing was checked, when nothing was. The run then exits
    /// [`EXIT_NOTHING_COVERED`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_checked: Option<String>,
    /// The scanner build that wrote the index, when it is not this one
    /// (carrick#2911). The problems marked `other_build` rest on it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_scanner: Option<String>,
}

/// One repo of the workspace, and what changed in it.
#[derive(Serialize, Debug, Clone)]
pub struct ChangedRepo {
    pub repo: String,
    /// The main branch compared against, and the merge base it was compared
    /// at. Absent when the repo has none, and then `skipped` says why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Repo-relative.
    pub files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    /// How many files the caller asked about that this entry could not check.
    /// Zero when `skipped` is not set.
    #[serde(skip_serializing_if = "is_zero")]
    pub unchecked: usize,
    /// No base could be found to diff against, so nothing in this repo was
    /// looked at. `skipped` carries the reason; the run exits
    /// [`EXIT_NOTHING_COVERED`] when no repo covered a file.
    #[serde(skip)]
    pub no_base: bool,
}

fn is_zero(count: &usize) -> bool {
    *count == 0
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ProblemKind {
    /// The pair's types or verbs do not agree.
    Incompatible,
    /// A call no producer in the workspace serves.
    NoRoute,
}

/// One problem, at the line it is written on.
#[derive(Serialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Problem {
    pub kind: ProblemKind,
    /// Workspace-relative, so it opens from where the command ran.
    pub file: String,
    pub line: Option<u32>,
    /// `route` or `call`.
    pub role: String,
    pub method: String,
    pub path: String,
    /// The other side, as `service file:line`. Empty for a call that reaches
    /// no route.
    pub other: Vec<String>,
    pub reason: String,
    /// `fact`, `candidate` or `not_stated`: whether the rows behind this are
    /// ones the source states.
    pub source: String,
    /// The index held this problem before the change began: same file, same
    /// operation. Printed, and never fails the run.
    pub already_indexed: bool,
    /// A call whose base is set at run time, so the check cannot tell a
    /// service of this workspace from an outside API. Printed, and never
    /// fails the run.
    pub unconfirmed: bool,
    /// In a file the work did not touch: a call the index matched whose
    /// route the work took away. Listed when it fails, and with `--all`;
    /// never counted on the summary line, which is about the edited files.
    #[serde(skip)]
    pub outside_the_change: bool,
    /// New only because the index lacks the row, and the index was written by
    /// another scanner build, which may simply not have read it
    /// (carrick#2911). Printed, never fails, and the run exits
    /// [`EXIT_NOTHING_COVERED`] when nothing else fails.
    pub other_build: bool,
    pub fails: bool,
}

/// The budget for one pass.
fn budget() -> Duration {
    std::env::var(BUDGET_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_BUDGET)
}

/// Which files the pass checks: every one the work changed since the main
/// branch, or the ones the caller named (`carrick check a.ts b.ts`). Either
/// way the files are grouped by repo, each repo is re-scanned once and one
/// join judges them all.
#[derive(Clone, Copy)]
pub enum Selection<'a> {
    Changed,
    Named(&'a [PathBuf]),
}

/// The exit code of a run that covered none of the files it was asked about,
/// or could not run at all. Distinct from 1 (a problem is a fact), so a caller
/// reading 0 or 1 never mistakes "nothing was looked at" for a pass
/// (carrick#2850). Hooks do not run this pass and keep their own exit codes.
pub const EXIT_NOTHING_COVERED: i32 = 2;

/// What a reason says when git gave none: a "not checked" line is never bare.
fn reason_or_default(reason: &str) -> String {
    let one = one_line(reason);
    if one.is_empty() {
        "git gave no reason".to_string()
    } else {
        one
    }
}

impl ChangedOutput {
    /// Some repo had files no check reached, or had no base to diff against.
    fn nothing_asked_covered(&self) -> bool {
        self.unchecked() > 0 || self.repos.iter().any(|repo| repo.no_base)
    }

    /// Files the caller asked about that no repo could check.
    pub fn unchecked(&self) -> usize {
        self.repos.iter().map(|repo| repo.unchecked).sum()
    }

    /// The process exit code: [`EXIT_NOTHING_COVERED`] when the run could not
    /// run, or asked about files and covered none; 1 when a problem is a
    /// fact; [`EXIT_NOTHING_COVERED`] when nothing fails but a problem rests
    /// on an index another scanner build wrote, which cannot say whether it
    /// is new (carrick#2911); else 0, including when some files were covered
    /// and others skipped (the skipped ones are listed).
    pub fn exit_code(&self) -> i32 {
        if self.not_checked.is_some() || (self.files == 0 && self.nothing_asked_covered()) {
            EXIT_NOTHING_COVERED
        } else if self.failing > 0 {
            1
        } else if self.other_build() > 0 {
            EXIT_NOTHING_COVERED
        } else {
            0
        }
    }

    /// Problems the index, written by another scanner build, cannot date.
    fn other_build(&self) -> usize {
        self.problems.iter().filter(|p| p.other_build).count()
    }
}

/// Run the pass and print it. Returns the exit code. `all` lists the problems
/// that rest on rows that are not facts; without it they are only counted.
/// `--json` always lists every problem, each with its `source` and `fails`.
pub fn run(root: Option<&Path>, selection: Selection, json: bool, all: bool) -> i32 {
    let started = Instant::now();
    let mut output = answer(root, started, selection);
    output.elapsed_ms = started.elapsed().as_millis() as u64;
    if json {
        match serde_json::to_string_pretty(&output) {
            Ok(text) => crate::outln!("{text}"),
            Err(e) => crate::errln!("carrick: could not serialize the answer: {e}"),
        }
    } else {
        crate::out!("{}", output.render(all));
    }
    output.exit_code()
}

fn not_checked(workspace: &str, reason: impl Into<String>) -> ChangedOutput {
    ChangedOutput {
        schema: SCHEMA.to_string(),
        workspace: workspace.to_string(),
        files: 0,
        routes: 0,
        calls: 0,
        services: 0,
        repos: Vec::new(),
        problems: Vec::new(),
        failing: 0,
        elapsed_ms: 0,
        not_checked: Some(reason.into()),
        index_scanner: None,
    }
}

fn answer(root: Option<&Path>, started: Instant, selection: Selection) -> ChangedOutput {
    let first = match selection {
        Selection::Named(files) => files.first().map(PathBuf::as_path),
        Selection::Changed => None,
    };
    let Some(root) = super::workspace::locate(root, first) else {
        return not_checked(
            "",
            "no Carrick index above this directory. Run `carrick init` in the folder holding \
             your repos.",
        );
    };
    let shown = root.display().to_string();
    let index = match super::query::read_index(&root) {
        Ok(index) => index,
        Err(failure) => return not_checked(&shown, failure.message()),
    };
    let workspace = match super::workspace::Workspace::load(&root) {
        Ok(workspace) => workspace,
        Err(e) => return not_checked(&shown, e),
    };

    let (repos, changed) = match selection {
        Selection::Changed => changed_files(&workspace, &index),
        Selection::Named(files) => named_files(&root, &index, files),
    };
    let files: usize = changed.values().map(|(_, files)| files.len()).sum();
    let mut output = ChangedOutput {
        repos,
        files,
        ..not_checked(&shown, "")
    };
    output.not_checked = None;
    // What the index holds, until a join says how many it read.
    output.services = index.repos.iter().map(|repo| repo.services.len()).sum();
    if files == 0 {
        return output;
    }

    let generation =
        std::env::temp_dir().join(format!("carrick-check-changed-{}", uuid::Uuid::new_v4()));
    let rescan: Vec<&Path> = changed.values().map(|(path, _)| path.as_path()).collect();
    // Said on stderr while it works: on a large repo the pass runs for
    // minutes, and a caller that reads silence as a hang kills it.
    let narrator = super::recheck::Narrator::to_stderr(started);
    let rejoined = super::recheck::rejoin(
        &workspace,
        &rescan,
        &generation,
        started + budget(),
        Some(&narrator),
    );
    // The temporary generation goes on every path, as the single-file
    // re-check's does.
    let _ = std::fs::remove_dir_all(&generation);
    let rejoined = match rejoined {
        Ok(rejoined) => rejoined,
        Err(reason) => {
            output.not_checked = Some(format!("the check did not finish: {reason}"));
            return output;
        }
    };
    output.services = rejoined.services;
    // A re-scan that could not replay the index's model answers has lost every
    // row only the model stated, so what it judged is not the whole service
    // (carrick#3000). That is not a pass.
    let lost = answers_lost(&rejoined.index, changed.keys());
    if !lost.is_empty() {
        output.not_checked = Some(lost.join(" "));
    }

    let wanted: BTreeMap<String, &BTreeSet<String>> = changed
        .iter()
        .map(|(name, (_, files))| (name.clone(), files))
        .collect();
    let same_build = index.written_by_this_build();
    if !same_build {
        output.index_scanner = Some(index.scanner_shown());
    }
    let judged = judge(
        &root,
        &index,
        same_build,
        &rejoined.index,
        &rejoined.join,
        &wanted,
    );
    output.routes = judged.routes;
    output.calls = judged.calls;
    output.failing = judged.problems.iter().filter(|p| p.fails).count();
    output.problems = judged.problems;
    output
}

/// Every repo's changed source files, keyed by repo name, with the repos
/// listed as the answer states them.
type Changed = BTreeMap<String, (PathBuf, BTreeSet<String>)>;

fn changed_files(
    workspace: &super::workspace::Workspace,
    index: &LocalIndex,
) -> (Vec<ChangedRepo>, Changed) {
    let mut listed = Vec::new();
    let mut changed = Changed::new();
    for repo in &workspace.repos {
        let name = super::index::repo_label(repo);
        let text = repo.to_string_lossy().to_string();
        let mut entry = ChangedRepo {
            repo: text.clone(),
            main_branch: None,
            base: None,
            files: Vec::new(),
            skipped: None,
            unchecked: 0,
            no_base: false,
        };
        let Some(indexed) = index.repos.iter().find(|indexed| indexed.name == name) else {
            // The files the work changed here are ones the caller asked
            // about, so count them: a repo the index does not hold is a
            // check that covered none of them.
            let mut reason = "not in the index, or the index is not bound to this folder; run \
                 `carrick init` or `carrick refresh`"
                .to_string();
            match crate::git_state::main_base(&text) {
                Ok((_, base)) => {
                    if let Some(paths) = super::query::changed_since(repo, &base) {
                        entry.files = paths
                            .into_iter()
                            .filter(|file| {
                                crate::file_finder::is_scanned_source(&repo.join(file), repo)
                            })
                            .collect();
                        entry.unchecked = entry.files.len();
                    }
                }
                Err(why) => {
                    entry.no_base = true;
                    reason = format!("{reason}. Also: {}", reason_or_default(&why));
                }
            }
            entry.skipped = Some(reason);
            listed.push(entry);
            continue;
        };
        let (branch, base) = match crate::git_state::main_base(&text) {
            Ok(found) => found,
            Err(reason) => {
                entry.no_base = true;
                entry.skipped = Some(reason_or_default(&reason));
                listed.push(entry);
                continue;
            }
        };
        let Some(paths) = super::query::changed_since(repo, &base) else {
            entry.skipped = Some(format!("git could not list the changes since {base}"));
            listed.push(entry);
            continue;
        };
        // Source the scan reads, in a service this index covers. A repo whose
        // services are all hosted lists none locally, and then every source
        // file in it is the repo's.
        let sources: BTreeSet<String> = paths
            .into_iter()
            .filter(|file| crate::file_finder::is_scanned_source(&repo.join(file), repo))
            .collect();
        let files: BTreeSet<String> = sources
            .iter()
            .filter(|file| {
                indexed.services.is_empty()
                    || indexed.services.iter().any(|service| service.covers(file))
            })
            .cloned()
            .collect();
        if files.len() < sources.len() {
            entry.unchecked = sources.len() - files.len();
            entry.skipped = Some(format!(
                "{} changed file(s) are in no indexed service; run `carrick init` or \
                 `carrick refresh`",
                entry.unchecked
            ));
        }
        entry.main_branch = Some(branch);
        entry.base = Some(base);
        entry.files = files.iter().cloned().collect();
        if !files.is_empty() {
            changed.insert(name, (repo.clone(), files));
        }
        listed.push(entry);
    }
    (listed, changed)
}

/// The files the caller named, grouped by the repo the index holds them in,
/// as [`changed_files`] groups the changed ones. A file no repo of the
/// workspace holds, or that the scan does not read, is listed as skipped and
/// the rest are still checked.
fn named_files(root: &Path, index: &LocalIndex, files: &[PathBuf]) -> (Vec<ChangedRepo>, Changed) {
    let mut listed: Vec<ChangedRepo> = Vec::new();
    let mut changed = Changed::new();
    let skip = |listed: &mut Vec<ChangedRepo>, file: &Path, reason: &str| {
        listed.push(ChangedRepo {
            repo: file.display().to_string(),
            main_branch: None,
            base: None,
            files: Vec::new(),
            skipped: Some(format!("{reason}; run `carrick init` or `carrick refresh`")),
            unchecked: 1,
            no_base: false,
        });
    };
    for file in files {
        let Some((repo, relative)) = index.locate_file(file) else {
            skip(&mut listed, file, "not in a repo of this workspace");
            continue;
        };
        let repo_path = PathBuf::from(&repo.path);
        if super::workspace::excluded_repo(root, &repo_path) {
            skip(&mut listed, file, "not in a repo of this workspace");
            continue;
        }
        if !crate::file_finder::is_scanned_source(&repo_path.join(&relative), &repo_path) {
            skip(&mut listed, file, "not a source file the scan reads");
            continue;
        }
        if !repo.services.is_empty() && !repo.services.iter().any(|s| s.covers(&relative)) {
            skip(&mut listed, file, "no indexed service covers it");
            continue;
        }
        changed
            .entry(repo.name.clone())
            .or_insert_with(|| (repo_path, BTreeSet::new()))
            .1
            .insert(relative);
    }
    let mut repos: Vec<ChangedRepo> = changed
        .iter()
        .map(|(_, (path, files))| ChangedRepo {
            repo: path.to_string_lossy().to_string(),
            main_branch: None,
            base: None,
            files: files.iter().cloned().collect(),
            skipped: None,
            unchecked: 0,
            no_base: false,
        })
        .collect();
    repos.extend(listed);
    (repos, changed)
}

/// The sentence for each re-scanned repo whose scan could not replay the
/// index's model answers (carrick#3000), in repo order.
fn answers_lost<'a>(
    rebuilt: &LocalIndex,
    scanned: impl Iterator<Item = &'a String>,
) -> Vec<String> {
    let scanned: BTreeSet<&String> = scanned.collect();
    rebuilt
        .repos
        .iter()
        .filter(|repo| scanned.contains(&repo.name))
        .flat_map(|repo| repo.services.iter())
        .filter_map(|service| {
            service
                .boundary
                .as_ref()?
                .model_answers_unavailable
                .as_ref()
        })
        .map(super::query::answers_unavailable_sentence)
        .collect()
}

struct Judged {
    routes: usize,
    calls: usize,
    problems: Vec<Problem>,
}

/// The problems in the changed files, read off the rebuilt index and the join
/// it came from.
///
/// `same_build` says whether `before` was written by this scanner build. When
/// it was not, `before` lacking a row is no evidence the row is new: that
/// build may not have read it (carrick#2911). A problem whose newness rests
/// only on that silence is marked `other_build` and does not fail. What
/// `before` did read still decides: a call it matched that now reaches no
/// route fails whichever build wrote it.
fn judge(
    root: &Path,
    before: &LocalIndex,
    same_build: bool,
    rebuilt: &LocalIndex,
    join: &super::LocalJoin,
    changed: &BTreeMap<String, &BTreeSet<String>>,
) -> Judged {
    // Which repo each service belongs to, and where that repo is, so a row's
    // repo-relative file can be tested against the changed set and printed
    // relative to the workspace.
    let mut repo_of: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for repo in &rebuilt.repos {
        for service in &repo.services {
            repo_of.insert(
                service.name.as_str(),
                (repo.name.as_str(), repo.path.as_str()),
            );
        }
    }
    let is_changed = |repo: &str, file: &str| changed.get(repo).is_some_and(|f| f.contains(file));
    let item_of = |repo: &str, operation: &JoinedOperation| -> Option<&IndexedItem> {
        rebuilt
            .repos
            .iter()
            .find(|indexed| indexed.name == repo)?
            .files
            .get(&operation.file)?
            .iter()
            .find(|item| item.key == operation.key && item.line == operation.line)
    };

    let strict = states_dispatch(before);
    let mut judged = Judged {
        routes: 0,
        calls: 0,
        problems: Vec::new(),
    };
    for operation in &join.operations {
        let Some(&(repo, repo_path)) = repo_of.get(operation.service.as_str()) else {
            continue;
        };
        if !is_changed(repo, &operation.file) {
            continue;
        }
        match operation.role {
            Role::Producer => judged.routes += 1,
            Role::Consumer => judged.calls += 1,
        }
        let Some(item) = item_of(repo, operation) else {
            continue;
        };
        let Some(verdict) = item.verdict.as_ref() else {
            continue;
        };
        if !matches!(
            verdict.result.as_deref(),
            Some("type_mismatch" | "method_mismatch")
        ) {
            continue;
        }
        let finding = super::index::finding_for(operation, &join.findings);
        // A finding resting only on rows the source states, whose verdict was
        // reached or needs none: the rule the cloud enforces on (cloud#599).
        // A verdict drawn from an edge with no finding behind it states no
        // source, so it is reported and does not fail the run.
        let fact = finding.is_some_and(|finding| {
            finding.edge_source.as_deref() == Some("fact")
                && finding.verdict_state.as_deref() != Some("unresolved")
        });
        // Not this work's when the index already held the same row of this
        // file with a mismatch: it was there before the change began.
        let already = indexed_rows(before, repo, &operation.file).any(|old| {
            same_operation(old, operation, strict)
                && old
                    .verdict
                    .as_ref()
                    .and_then(|verdict| verdict.result.as_deref())
                    .is_some_and(|result| matches!(result, "type_mismatch" | "method_mismatch"))
        });
        let named: Vec<&String> = finding
            .map(|f| f.call_sites.iter().collect())
            .unwrap_or_default();
        let other: Vec<String> = item
            .counterparts
            .iter()
            .filter(|counterpart| {
                operation.role == Role::Consumer
                    || named.is_empty()
                    || named.iter().any(|site| {
                        counterpart
                            .line
                            .is_some_and(|line| **site == format!("{}:{line}", counterpart.file))
                    })
            })
            .map(|counterpart| match counterpart.line {
                Some(line) => format!("{} {}:{line}", counterpart.service, counterpart.file),
                None => format!("{} {}", counterpart.service, counterpart.file),
            })
            .collect();
        judged.problems.push(Problem {
            kind: ProblemKind::Incompatible,
            file: shown(root, repo_path, &operation.file),
            line: operation.line,
            role: item.kind.as_str().to_string(),
            method: operation.method.clone(),
            path: operation.path.clone(),
            other,
            reason: one_line(&verdict.detail),
            source: finding
                .and_then(|finding| finding.edge_source.clone())
                .unwrap_or_else(|| "not_stated".to_string()),
            already_indexed: already,
            unconfirmed: false,
            outside_the_change: false,
            other_build: fact && !already && !same_build,
            fails: fact && !already && same_build,
        });
    }

    for unmatched in &join.unmatched {
        for site in &unmatched.call_sites {
            let (file, line) = super::split_location(site);
            for operation in join.operations.iter().filter(|operation| {
                operation.role == Role::Consumer && operation.file == file && operation.line == line
            }) {
                let Some(&(repo, repo_path)) = repo_of.get(operation.service.as_str()) else {
                    continue;
                };
                // A call in a file the work did not touch is reported only
                // when the index matched it: the work took its route away.
                if !is_changed(repo, &file) && !was_matched(before, strict, repo, operation) {
                    continue;
                }
                let source = Source::of(operation.resolution_source);
                // Already unmatched in the index: the same call in the same
                // file had no producer before the change began. Its line may
                // have moved, so the key and the file decide.
                let already = indexed_rows(before, repo, &file).any(|old| {
                    old.kind == super::read_model::ItemKind::Call
                        && same_operation(old, operation, strict)
                        && old.counterparts.is_empty()
                });
                // A base set at run time could be one of this workspace's
                // services or an outside API, and the scanner sees only the
                // expression: printed, never failing, new or not. Unless the
                // index matched this call: a workspace route served it, so
                // the base is the workspace's and losing it is a fact.
                let matched = was_matched(before, strict, repo, operation);
                let unconfirmed = unmatched.injected.iter().any(|at| at == site) && !matched;
                let new_fact = source == Source::Fact && !already && !unconfirmed;
                let other_build = new_fact && !same_build && !matched;
                judged.problems.push(Problem {
                    kind: ProblemKind::NoRoute,
                    file: shown(root, repo_path, &file),
                    line,
                    role: "call".to_string(),
                    method: operation.method.clone(),
                    path: operation.path.clone(),
                    other: Vec::new(),
                    reason: if is_changed(repo, &file) {
                        "no service in this workspace serves it".to_string()
                    } else {
                        "the index matched this call, and no service serves it now".to_string()
                    },
                    source: source.as_str().to_string(),
                    already_indexed: already,
                    unconfirmed,
                    outside_the_change: !is_changed(repo, &file),
                    other_build,
                    fails: new_fact && !other_build,
                });
            }
        }
    }
    judged.problems.sort();
    judged.problems.dedup();
    judged
}

/// Whether any row of the index states a dispatch case. An index written
/// before rows carried the case states none anywhere; one that does, states
/// the case on every row it has one for.
fn states_dispatch(index: &LocalIndex) -> bool {
    index
        .repos
        .iter()
        .flat_map(|repo| repo.files.values().flatten())
        .any(|item| item.dispatch.is_some())
}

/// Whether an indexed row is the same operation as a joined one: the key
/// (method and path, GraphQL field, socket event or topic) and the dispatch
/// case (carrick#831), which is part of a row's identity beside it. The cases
/// of one dispatching route are different operations, so a sibling's state
/// says nothing about this one.
///
/// `strict` says the index states dispatch cases (see [`states_dispatch`]):
/// then a row with none is a plain row. When it does not, the index predates
/// the field, its silence is no evidence, and the key alone decides.
fn same_operation(old: &IndexedItem, operation: &JoinedOperation, strict: bool) -> bool {
    old.key == operation.key && (!strict || old.dispatch == operation.dispatch)
}

/// The rows the index, before this pass, held for one file of one repo.
fn indexed_rows<'a>(
    before: &'a LocalIndex,
    repo: &str,
    file: &str,
) -> impl Iterator<Item = &'a IndexedItem> {
    before
        .repos
        .iter()
        .find(|indexed| indexed.name == repo)
        .and_then(|indexed| indexed.files.get(file))
        .into_iter()
        .flatten()
}

/// Whether the index, before this pass, matched this call to a producer.
fn was_matched(before: &LocalIndex, strict: bool, repo: &str, operation: &JoinedOperation) -> bool {
    before
        .repos
        .iter()
        .find(|indexed| indexed.name == repo)
        .and_then(|indexed| indexed.files.get(&operation.file))
        .is_some_and(|items| {
            items.iter().any(|item| {
                same_operation(item, operation, strict)
                    && item.line == operation.line
                    && !item.counterparts.is_empty()
            })
        })
}

/// A repo-relative file as a path from the workspace root.
fn shown(root: &Path, repo_path: &str, file: &str) -> String {
    let repo = Path::new(repo_path);
    match repo.strip_prefix(root) {
        Ok(relative) if relative.as_os_str().is_empty() => file.to_string(),
        Ok(relative) => format!("{}/{file}", relative.to_string_lossy()),
        Err(_) => repo.join(file).to_string_lossy().to_string(),
    }
}

/// How long one reason may run on its line before it is cut.
const MAX_REASON_CHARS: usize = 300;

/// A compiler message on one line, cut to a length a terminal row holds.
fn one_line(text: &str) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::findings::truncate_chars(&joined, MAX_REASON_CHARS)
}

impl ChangedOutput {
    /// The summary line, then one line per problem. A problem that rests on
    /// rows that are not facts does not fail the run; unless `all` is set it
    /// is counted on the summary line ("plus N unconfirmed") and not listed.
    pub fn render(&self, all: bool) -> String {
        if let Some(reason) = &self.not_checked {
            return format!(
                "carrick check: not checked: {}\n",
                reason_or_default(reason)
            );
        }
        if self.files == 0 && self.repos.iter().any(|repo| repo.no_base) {
            let mut out = String::from(
                "carrick check: not checked: no base to compare against, so no change was looked at\n",
            );
            out.push_str(&self.skipped_lines());
            return out;
        }
        if self.files == 0 && self.unchecked() > 0 {
            let mut out = format!(
                "carrick check: not checked: covered none of the {} asked about\n",
                plural(self.unchecked(), "file", "files")
            );
            out.push_str(&self.skipped_lines());
            return out;
        }
        let listed: Vec<&Problem> = self
            .problems
            .iter()
            .filter(|problem| all || problem.fails || problem.other_build)
            .collect();
        // Each problem not listed, counted by why, in the order `--all`
        // prints the reasons in. A file the work did not touch adds to this
        // line only by failing, which lists it: the summary is about the
        // edited files.
        let mut unlisted: BTreeMap<Unlisted, usize> = BTreeMap::new();
        for problem in &self.problems {
            if let (false, false, Some(why)) = (all, problem.outside_the_change, problem.unlisted())
            {
                *unlisted.entry(why).or_default() += 1;
            }
        }
        let incompatible = listed
            .iter()
            .filter(|p| p.kind == ProblemKind::Incompatible)
            .count();
        let no_route = listed.len() - incompatible;
        let mut out = format!(
            "carrick check: checked {} and {} in {} against {}: {incompatible} incompatible, {no_route} {}",
            plural(self.routes, "route", "routes"),
            plural(self.calls, "call", "calls"),
            plural(self.files, "changed file", "changed files"),
            plural(self.services, "service", "services"),
            if no_route == 1 {
                "reaches no route"
            } else {
                "reach no route"
            },
        );
        let advisory = listed.len() - self.failing;
        if advisory > 0 {
            out.push_str(&format!(" ({advisory} of them do not fail the check)"));
        }
        if !unlisted.is_empty() {
            let counts: Vec<String> = unlisted
                .iter()
                .map(|(why, count)| why.counted(*count))
                .collect();
            out.push_str(&format!(
                ", plus {}, run with --all to list them",
                join_and(&counts)
            ));
        }
        out.push('\n');
        for problem in listed {
            let at = match problem.line {
                Some(line) => format!("{}:{line}", problem.file),
                None => problem.file.clone(),
            };
            let what = match problem.kind {
                ProblemKind::Incompatible => "incompatible",
                ProblemKind::NoRoute => "no route",
            };
            let with = if problem.other.is_empty() {
                String::new()
            } else {
                format!(" with {}", problem.other.join(", "))
            };
            let note = if problem.other_build {
                " [not checked: the index is from another scanner build]"
            } else {
                problem.unlisted().map_or("", Unlisted::note)
            };
            out.push_str(&format!(
                "{at}  {what}  {} {} {}{with}: {}{note}\n",
                problem.role, problem.method, problem.path, problem.reason
            ));
        }
        let other_build = self.other_build();
        if other_build > 0 {
            // `refresh` indexes the working tree, so one run on a branch would
            // take the branch's own new problems in as already indexed.
            out.push_str(&format!(
                "not checked: {} the index was written by carrick {}, and this is carrick {}, \
                 which may read them differently. Run `carrick refresh` on the main branch, then \
                 check again.\n",
                plural(
                    other_build,
                    "problem above looks new only because",
                    "problems above look new only because"
                ),
                self.index_scanner.as_deref().unwrap_or("another build"),
                super::read_model::shown_build(
                    env!("CARGO_PKG_VERSION"),
                    crate::cloud_storage::ScannerBuild::current().as_ref()
                ),
            ));
        }
        out.push_str(&self.skipped_lines());
        out
    }

    fn skipped_lines(&self) -> String {
        let mut out = String::new();
        for repo in &self.repos {
            if let Some(reason) = &repo.skipped {
                out.push_str(&format!(
                    "not checked: {}: {}\n",
                    repo.repo,
                    reason_or_default(reason)
                ));
            }
        }
        out
    }
}

/// Why a problem does not fail the run, so is listed only with `--all`: the
/// note `--all` prints beside it and the count the summary line gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Unlisted {
    AlreadyIndexed,
    Unconfirmed,
    Candidate,
    NotStated,
}

impl Unlisted {
    fn note(self) -> &'static str {
        match self {
            Unlisted::AlreadyIndexed => " [already in the index, does not fail]",
            Unlisted::Unconfirmed => " [unconfirmed: the base is set at run time, does not fail]",
            Unlisted::Candidate => " [model candidate, does not fail]",
            Unlisted::NotStated => " [source not stated, does not fail]",
        }
    }

    fn counted(self, count: usize) -> String {
        match self {
            Unlisted::AlreadyIndexed => format!("{count} already in the index"),
            Unlisted::Unconfirmed => format!("{count} unconfirmed"),
            Unlisted::Candidate => plural(count, "model candidate", "model candidates"),
            Unlisted::NotStated => format!("{count} with no stated source"),
        }
    }
}

impl Problem {
    /// Why this problem is listed only with `--all`; `None` when it fails
    /// the run. A problem the index already held says so first, whatever
    /// else holds for it.
    fn unlisted(&self) -> Option<Unlisted> {
        match (self.fails, self.source.as_str()) {
            (true, _) => None,
            // Listed without `--all`: it is why the run exits 2.
            _ if self.other_build => None,
            _ if self.already_indexed => Some(Unlisted::AlreadyIndexed),
            _ if self.unconfirmed => Some(Unlisted::Unconfirmed),
            (false, "candidate") => Some(Unlisted::Candidate),
            (false, _) => Some(Unlisted::NotStated),
        }
    }
}

/// `a`, `a and b`, `a, b and c`.
fn join_and(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problem(kind: ProblemKind, fails: bool) -> Problem {
        Problem {
            kind,
            file: "web/src/a.ts".to_string(),
            line: Some(4),
            role: "call".to_string(),
            method: "GET".to_string(),
            path: "/a".to_string(),
            other: Vec::new(),
            reason: "no service in this workspace serves it".to_string(),
            source: if fails { "fact" } else { "candidate" }.to_string(),
            already_indexed: false,
            unconfirmed: false,
            outside_the_change: false,
            other_build: false,
            fails,
        }
    }

    /// A file the work did not touch never adds to the summary of the edited
    /// files: its call that does not fail is listed with `--all` and counted
    /// nowhere else. One that fails (the work took its route away) is listed.
    #[test]
    fn an_untouched_file_adds_nothing_to_the_summary_unless_it_fails() {
        let mut elsewhere = problem(ProblemKind::NoRoute, false);
        elsewhere.file = "web/src/other.ts".to_string();
        elsewhere.reason = "the index matched this call, and no service serves it now".to_string();
        elsewhere.outside_the_change = true;
        let mut broken = elsewhere.clone();
        broken.path = "/c".to_string();
        broken.source = "fact".to_string();
        broken.fails = true;
        let output = |problems: Vec<Problem>, failing: usize| ChangedOutput {
            routes: 0,
            calls: 1,
            files: 1,
            services: 2,
            problems,
            failing,
            not_checked: None,
            ..not_checked("/w", "")
        };

        let quiet = output(vec![elsewhere.clone()], 0).render(false);
        assert_eq!(
            quiet,
            "carrick check: checked 0 routes and 1 call in 1 changed file against 2 services: \
             0 incompatible, 0 reach no route\n"
        );
        let everything = output(vec![elsewhere.clone()], 0).render(true);
        assert!(everything.contains("web/src/other.ts:4"), "{everything}");

        let failing = output(vec![elsewhere, broken], 1).render(false);
        assert!(
            failing.starts_with(
                "carrick check: checked 0 routes and 1 call in 1 changed file against 2 services: \
                 0 incompatible, 1 reaches no route\n"
            ),
            "{failing}"
        );
        assert!(
            failing.contains("web/src/other.ts:4  no route  call GET /c"),
            "{failing}"
        );
    }

    /// carrick#2911: a problem the index from another build cannot date is
    /// listed without `--all`, exits 2 when nothing fails, and never hides a
    /// failure.
    #[test]
    fn a_problem_another_builds_index_cannot_date_exits_two_unless_one_fails() {
        let mut undated = problem(ProblemKind::NoRoute, false);
        undated.source = "fact".to_string();
        undated.other_build = true;
        let output = ChangedOutput {
            calls: 1,
            files: 1,
            problems: vec![undated],
            not_checked: None,
            index_scanner: Some("0.3.1".to_string()),
            ..not_checked("/w", "")
        };
        assert_eq!(output.exit_code(), EXIT_NOTHING_COVERED);
        let text = output.render(false);
        assert!(
            text.contains(
                "web/src/a.ts:4  no route  call GET /a: no service in this workspace serves it \
                 [not checked: the index is from another scanner build]"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "not checked: 1 problem above looks new only because the index was written by \
                 carrick 0.3.1, and this is carrick "
            ),
            "{text}"
        );
        let failing = ChangedOutput {
            problems: vec![
                problem(ProblemKind::Incompatible, true),
                output.problems[0].clone(),
            ],
            failing: 1,
            ..output
        };
        assert_eq!(failing.exit_code(), 1);
    }

    /// One service `web` in repo `web`, and a call row at `client.ts:<line>`.
    fn call_row(line: u32, dispatch: Option<&str>, served: bool) -> serde_json::Value {
        serde_json::json!({
            "kind": "call", "service": "web", "key": "http|POST|/types/check",
            "dispatch": dispatch, "method": "POST", "path": "/types/check",
            "line": line, "col": null, "source": "fact",
            "resolution_source": null, "evidence": null,
            "counterparts": if served {
                serde_json::json!([{"role": "producer", "service": "api", "file": "h.ts", "line": 1}])
            } else {
                serde_json::json!([])
            },
            "verdict": null
        })
    }

    fn index_of(rows: Vec<serde_json::Value>) -> LocalIndex {
        serde_json::from_value(serde_json::json!({
            "version": 1, "scanner_version": "0", "indexed_at": "", "repos": [{
                "path": "/w/web", "name": "web",
                "services": [{
                    "name": "web", "commit": "", "indexed_at": "", "boundary": null,
                    "routes": 0, "calls": 0
                }],
                "files": {"client.ts": rows}
            }]
        }))
        .expect("an index")
    }

    fn dispatch_key(value: &str) -> String {
        crate::dispatch::Dispatch {
            location: crate::dispatch::DispatchLocation::Body,
            field: "action".to_string(),
            value: value.to_string(),
        }
        .key()
    }

    /// The judge's verdict on the one call at `client.ts:322` that reaches no
    /// route, sending `sent`, against the rows the index held before.
    fn judged_call(before: Vec<serde_json::Value>, sent: Option<&str>) -> Problem {
        let before = index_of(before);
        let sent = sent.map(dispatch_key);
        let join: super::super::LocalJoin = serde_json::from_value(serde_json::json!({
            "scanner_version": "0",
            "operations": [{
                "role": "consumer", "service": "web", "key": "http|POST|/types/check",
                "method": "POST", "path": "/types/check", "file": "client.ts",
                "line": 322, "resolution_source": null, "handler": null,
                "dispatch": sent
            }],
            "matches": [], "findings": [],
            "unmatched": [{
                "method": "POST", "path": "/types/check",
                "call_sites": ["client.ts:322"], "injected": []
            }],
            "type_check": {"state": "ran"}
        }))
        .expect("a join");
        let rebuilt = index_of(Vec::new());
        let files: BTreeSet<String> = ["client.ts".to_string()].into();
        let changed: BTreeMap<String, &BTreeSet<String>> = [("web".to_string(), &files)].into();
        // The row's source decides whether it fails: a resolved call is a fact.
        let mut join = join;
        join.operations[0].resolution_source =
            Some(crate::agents::file_analyzer_agent::ResolutionSource::RequestSpec);
        let judged = judge(Path::new("/w"), &before, true, &rebuilt, &join, &changed);
        assert_eq!(judged.problems.len(), 1, "{:?}", judged.problems);
        judged.problems.into_iter().next().unwrap()
    }

    /// A dispatch case renamed in the handler and the config leaves the call
    /// sending the old value with no route. A sibling call to the same path
    /// was already unmatched in the index; it is not this call.
    #[test]
    fn a_renamed_dispatch_case_is_not_already_indexed_because_a_sibling_was() {
        // A dispatch row elsewhere in the index: it states cases, so the
        // dispatch-less sibling is a plain call.
        let mut elsewhere = call_row(10, Some(&dispatch_key("other")), true);
        elsewhere["path"] = "/other".into();
        elsewhere["key"] = "http|POST|/other".into();
        let before = vec![elsewhere, call_row(280, None, false)];
        let problem = judged_call(before, Some("old-name"));
        assert!(!problem.already_indexed, "{problem:?}");
        assert!(problem.fails, "{problem:?}");
    }

    /// A call sending a value no case answers is a different operation from
    /// one sending a value that is also unmatched.
    #[test]
    fn a_wrong_dispatch_value_is_not_already_indexed_because_another_value_was() {
        let before = vec![call_row(280, Some(&dispatch_key("other")), false)];
        let problem = judged_call(before, Some("wrong"));
        assert!(!problem.already_indexed, "{problem:?}");
        assert!(problem.fails, "{problem:?}");
    }

    /// The same case unmatched before and now is old, and a plain call is
    /// judged as it always was.
    #[test]
    fn the_same_dispatch_case_or_a_plain_call_unmatched_before_is_already_indexed() {
        let same = judged_call(
            vec![call_row(280, Some(&dispatch_key("old-name")), false)],
            Some("old-name"),
        );
        assert!(same.already_indexed && !same.fails, "{same:?}");
        let plain = judged_call(vec![call_row(280, None, false)], None);
        assert!(plain.already_indexed && !plain.fails, "{plain:?}");
        // An index with no case anywhere predates the field: it still dates
        // the same key, so an upgrade invents no new problem.
        let old_index = judged_call(vec![call_row(280, None, false)], Some("old-name"));
        assert!(
            old_index.already_indexed && !old_index.fails,
            "{old_index:?}"
        );
    }

    /// A call the index matched under one case is not matched under another.
    #[test]
    fn a_call_matched_under_one_dispatch_case_was_not_matched_under_another() {
        let before = index_of(vec![call_row(322, Some(&dispatch_key("a")), true)]);
        let operation = |case: &str| -> JoinedOperation {
            serde_json::from_value(serde_json::json!({
                "role": "consumer", "service": "web", "key": "http|POST|/types/check",
                "method": "POST", "path": "/types/check", "file": "client.ts",
                "line": 322, "resolution_source": null, "handler": null,
                "dispatch": dispatch_key(case)
            }))
            .unwrap()
        };
        assert!(was_matched(&before, true, "web", &operation("a")));
        assert!(!was_matched(&before, true, "web", &operation("b")));
    }

    #[test]
    fn the_summary_counts_every_problem_and_names_the_ones_that_do_not_fail() {
        let output = ChangedOutput {
            routes: 1,
            calls: 2,
            files: 3,
            services: 2,
            problems: vec![
                problem(ProblemKind::Incompatible, true),
                problem(ProblemKind::NoRoute, false),
            ],
            failing: 1,
            not_checked: None,
            ..not_checked("/w", "")
        };
        let text = output.render(true);
        let first = text.lines().next().unwrap();
        assert_eq!(
            first,
            "carrick check: checked 1 route and 2 calls in 3 changed files against 2 services: \
             1 incompatible, 1 reaches no route (1 of them do not fail the check)"
        );
        assert!(text.contains("web/src/a.ts:4  no route  call GET /a: no service"));
        assert!(text.contains("[model candidate, does not fail]"));
    }

    #[test]
    fn problems_that_are_not_facts_are_counted_and_listed_only_with_all() {
        let mut candidate = problem(ProblemKind::NoRoute, false);
        candidate.path = "/b".to_string();
        let output = ChangedOutput {
            routes: 0,
            calls: 2,
            files: 1,
            services: 2,
            problems: vec![problem(ProblemKind::Incompatible, true), candidate],
            failing: 1,
            not_checked: None,
            ..not_checked("/w", "")
        };
        let quiet = output.render(false);
        assert_eq!(
            quiet.lines().next().unwrap(),
            "carrick check: checked 0 routes and 2 calls in 1 changed file against 2 services: \
             1 incompatible, 0 reach no route, plus 1 model candidate, run with --all to list them"
        );
        assert_eq!(quiet.lines().count(), 2, "the fact only:\n{quiet}");
        assert!(!quiet.contains("GET /b"), "{quiet}");
        let everything = output.render(true);
        assert!(everything.contains("GET /b"), "{everything}");
        assert!(
            !everything.contains("unconfirmed"),
            "nothing is hidden, so nothing is counted as hidden:\n{everything}"
        );
    }

    /// The count of problems not listed names why each is not listed, with
    /// the reason `--all` prints beside it: a problem the index already held
    /// is not "unconfirmed".
    #[test]
    fn the_hidden_count_names_why_each_problem_is_not_listed() {
        let mut held = problem(ProblemKind::NoRoute, false);
        held.source = "fact".to_string();
        held.already_indexed = true;
        let mut held_too = held.clone();
        held_too.path = "/c".to_string();
        let mut runtime_base = problem(ProblemKind::NoRoute, false);
        runtime_base.source = "fact".to_string();
        runtime_base.unconfirmed = true;
        runtime_base.path = "/d".to_string();
        let mut candidate = problem(ProblemKind::NoRoute, false);
        candidate.path = "/e".to_string();
        let mut unstated = problem(ProblemKind::NoRoute, false);
        unstated.source = "not_stated".to_string();
        unstated.path = "/f".to_string();
        let only_held = ChangedOutput {
            routes: 0,
            calls: 2,
            files: 1,
            services: 2,
            problems: vec![held.clone(), held_too.clone()],
            failing: 0,
            not_checked: None,
            ..not_checked("/w", "")
        };
        assert_eq!(
            only_held.render(false).lines().next().unwrap(),
            "carrick check: checked 0 routes and 2 calls in 1 changed file against 2 services: \
             0 incompatible, 0 reach no route, plus 2 already in the index, \
             run with --all to list them"
        );
        let mixed = ChangedOutput {
            problems: vec![held, held_too, runtime_base, candidate, unstated],
            ..only_held
        };
        assert_eq!(
            mixed.render(false).lines().next().unwrap(),
            "carrick check: checked 0 routes and 2 calls in 1 changed file against 2 services: \
             0 incompatible, 0 reach no route, plus 2 already in the index, 1 unconfirmed, \
             1 model candidate and 1 with no stated source, run with --all to list them"
        );
    }

    #[test]
    fn a_check_that_could_not_run_says_so_on_one_line() {
        let text = not_checked("/w", "no index").render(false);
        assert_eq!(text, "carrick check: not checked: no index\n");
    }

    #[test]
    fn nothing_covered_exits_two_and_a_partial_cover_exits_by_the_covered_files() {
        let skipped = ChangedRepo {
            repo: "x.ts".to_string(),
            main_branch: None,
            base: None,
            files: Vec::new(),
            skipped: Some("not in a repo of this workspace".to_string()),
            unchecked: 1,
            no_base: false,
        };
        let none = ChangedOutput {
            repos: vec![skipped.clone()],
            ..not_checked("/w", "")
        };
        let none = ChangedOutput {
            not_checked: None,
            ..none
        };
        assert_eq!(none.exit_code(), EXIT_NOTHING_COVERED);
        assert!(
            none.render(false).starts_with(
                "carrick check: not checked: covered none of the 1 file asked about\n"
            )
        );
        let some = ChangedOutput {
            files: 1,
            ..none.clone()
        };
        assert_eq!(some.exit_code(), 0);
        let failing = ChangedOutput { failing: 1, ..some };
        assert_eq!(failing.exit_code(), 1);
        assert_eq!(
            not_checked("/w", "no index").exit_code(),
            EXIT_NOTHING_COVERED
        );
        let nothing_asked = ChangedOutput {
            not_checked: None,
            ..not_checked("/w", "")
        };
        assert_eq!(
            nothing_asked.exit_code(),
            0,
            "no changed file asked about nothing"
        );
    }

    #[test]
    fn every_not_checked_line_carries_a_reason() {
        let bare = ChangedRepo {
            repo: "r".to_string(),
            main_branch: None,
            base: None,
            files: Vec::new(),
            skipped: Some(String::new()),
            unchecked: 0,
            no_base: true,
        };
        let output = ChangedOutput {
            repos: vec![bare],
            ..not_checked("/w", "")
        };
        let output = ChangedOutput {
            not_checked: None,
            ..output
        };
        assert_eq!(output.exit_code(), EXIT_NOTHING_COVERED);
        for line in output.render(false).lines() {
            assert!(!line.trim_end().ends_with("not checked:"), "{line}");
            assert!(!line.ends_with(": "), "{line}");
        }
        assert!(
            not_checked("/w", "")
                .render(false)
                .contains("git gave no reason")
        );
    }

    #[test]
    fn a_compiler_message_is_one_line_and_cut() {
        assert_eq!(one_line("a\n  b\tc"), "a b c");
        assert!(one_line(&"x ".repeat(400)).chars().count() <= MAX_REASON_CHARS);
    }
}
