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
//! and is counted on the summary line, listed only with `--all`, except a row
//! outside the changed files that this change moved: listed on a plain run, up
//! to ten (carrick#2998). A check that
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

use crate::findings::{BreakStatus, EdgeSource, VerdictState, mismatch_break_status};

use super::checkout::Tree;
use super::join::{JoinedOperation, Role};
use super::read_model::{IndexedItem, IndexedRepo, LocalIndex, Source};
use super::reads_same::{LineMap, reads_same_since};

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
    /// Matched routes and calls in the changed files whose pair the type
    /// check reached a verdict on.
    pub type_checked: usize,
    /// Matched routes and calls in the changed files whose pair could not be
    /// judged: no verdict, or one side without a usable type (carrick#3012).
    /// When this lists rows, `type_checked` is 0 and nothing fails, the run
    /// exits [`EXIT_NOTHING_COVERED`].
    pub not_type_checked: Vec<NotTypeChecked>,
    /// Rows in files the work did not touch whose verdict this change moved
    /// from compatible to a mismatch (carrick#2998). Rows with no earlier
    /// verdict are not counted here. Not part
    /// of `routes`, `calls` or `type_checked`, which stay about the edited
    /// files.
    pub outside_moved: usize,
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
    /// Whether this repo's changed files were re-read (`ran`), or read the
    /// same as the index's copy and were answered from it (`not_needed`,
    /// carrick#3245). Absent for a repo with no changed file to judge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rescan: Option<Rescan>,
}

/// See [`ChangedRepo::rescan`].
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Rescan {
    Ran,
    NotNeeded,
}

/// Set to `always` to re-scan every changed repo even when nothing the scan
/// reads moved: the answer such a check gives without the re-scan must equal
/// this one's (carrick#3245).
pub const RESCAN_ENV: &str = "CARRICK_CHECK_RESCAN";

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
    /// A call row read from a request the code builds and returns
    /// (`request_descriptor`, carrick#3470), not from a call seen sent.
    /// Counted on the summary line as its own kind, never as a model
    /// candidate.
    #[serde(skip)]
    pub descriptor: bool,
    /// The index held this problem before the change began: same file, same
    /// operation. Printed, and never fails the run.
    pub already_indexed: bool,
    /// A call whose base is set at run time, so the check cannot tell a
    /// service of this workspace from an outside API. Printed, and never
    /// fails the run.
    pub unconfirmed: bool,
    /// In a file the work did not touch: a call the index matched whose
    /// route the work took away (its producer's file changed since the
    /// index), or a pair whose verdict the work moved to a
    /// mismatch. Listed when it fails, and with `--all`; counted on the
    /// summary line only through `outside_moved`.
    pub outside_the_change: bool,
    /// Outside the change, and the index held no resolved, compatible row
    /// to move from: it may have resolved for a reason unrelated to the edit
    /// (carrick#2961). Never fails.
    #[serde(skip)]
    pub not_judged_before: bool,
    /// Outside the change, and the index held this pair resolved and
    /// compatible: this change moved it to a mismatch. Listed on a plain
    /// run, up to [`MAX_MOVED_LISTED`], whether or not it fails.
    #[serde(skip)]
    pub moved_outside: bool,
    /// What the pair reads now: the verdict's result (`type_mismatch`,
    /// `method_mismatch`) or `no_route`. Not printed here: the per-edit
    /// re-check's `moved` list states it (`recheck.rs`, `moved_rows`).
    #[serde(skip)]
    pub after: String,
    /// New only because the index lacks the row, and the index was written by
    /// another scanner build, which may simply not have read it
    /// (carrick#2911). Printed, never fails, and the run exits
    /// [`EXIT_NOTHING_COVERED`] when nothing else fails.
    pub other_build: bool,
    pub fails: bool,
}

/// A matched route or call in a changed file whose pair the type check could
/// not judge, so a type change on it cannot be seen. Named, never counted as
/// a pass (carrick#3012).
#[derive(Serialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NotTypeChecked {
    /// Workspace-relative, so it opens from where the command ran.
    pub file: String,
    pub line: Option<u32>,
    /// `route` or `call`.
    pub role: String,
    pub method: String,
    pub path: String,
    /// The verdict's own detail, or that there was none.
    pub reason: String,
    /// A pair whose call rows are all read from a request the code builds
    /// and returns (carrick#3470): the call itself, or every caller of the
    /// route (carrick#3532). Its matched method and path are a check, so it
    /// never makes the run read as "nothing type-checked".
    #[serde(skip)]
    pub descriptor: bool,
}

/// The words a customer reads about pairs the check could not judge. Kept
/// here, in one place; they wait on David's approval (carrick#3012).
/// The words a customer reads about rows outside the changed files whose
/// verdict the change moved (carrick#2998). In one place; they wait on
/// David's approval.
fn outside_moved_clause(count: usize) -> String {
    format!(
        "; {} outside the changed files whose verdict this change moved",
        plural(count, "row", "rows")
    )
}
/// How many rows outside the changed files that this change moved are listed
/// on a plain run; the rest are counted and `--all` lists them.
const MAX_MOVED_LISTED: usize = 10;
const OUTSIDE_FAILING_NOTE: &str = " [outside the changed files: this change moved its verdict]";
const OUTSIDE_NOT_JUDGED_NOTE: &str =
    " [outside the changed files: not judged before, does not fail]";
fn outside_not_judged_counted(count: usize) -> String {
    format!("{count} outside the changed files not judged before")
}

const NOT_TYPE_CHECKED_SUFFIX: &str = "not type-checked (one side has no usable type)";
const NOTHING_TYPE_CHECKED: &str = "not checked: no matched route or call in the changed files has a usable type on both sides, so a type change cannot be seen";

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
    /// is new (carrick#2911), or when the changed files hold matched pairs and
    /// the check judged none of them (carrick#3012); else 0, including when some files were covered
    /// and others skipped (the skipped ones are listed).
    pub fn exit_code(&self) -> i32 {
        if self.not_checked.is_some() || (self.files == 0 && self.nothing_asked_covered()) {
            EXIT_NOTHING_COVERED
        } else if self.failing > 0 {
            1
        } else if self.other_build() > 0 || self.nothing_type_checked() {
            EXIT_NOTHING_COVERED
        } else {
            0
        }
    }

    /// The changed files hold matched pairs and the check judged none of them
    /// (carrick#3012). A pass would read the same, so it is not one. A pair
    /// whose call is a built request (carrick#3470) does not count: matching
    /// its method and path is the check it can have.
    fn nothing_type_checked(&self) -> bool {
        self.type_checked == 0 && self.not_type_checked.iter().any(|row| !row.descriptor)
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
    let started_at_ms = now_ms();
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
    let exit = output.exit_code();
    if matches!(selection, Selection::Changed) && !output.workspace.is_empty() {
        record_check_marker(Path::new(&output.workspace), started_at_ms, now_ms(), exit);
    }
    exit
}

/// Where `check --changed` leaves its marker, under the workspace's `.carrick/`.
const CHECK_MARKER_FILE: &str = "last-check-changed.json";

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// Leave the time a `check --changed` began and ended, and its exit code, in
/// `<root>/.carrick/last-check-changed.json`, written whole or not at all. The
/// npm hooks read it (`npm/carrick/src/hook/checked.ts`, same file name and
/// fields) so a late line stops telling an agent to run the command it has
/// just run (carrick#3357). Only a workspace that already has a `.carrick/`
/// gets one; a failed write is not a failure of the check.
fn record_check_marker(root: &Path, started_at_ms: u64, finished_at_ms: u64, exit: i32) {
    let dir = root.join(".carrick");
    if !dir.is_dir() {
        return;
    }
    let body = serde_json::json!({
        "started_at_ms": started_at_ms,
        "finished_at_ms": finished_at_ms,
        "exit": exit,
    });
    let Ok(json) = serde_json::to_vec_pretty(&body) else {
        return;
    };
    let file = dir.join(CHECK_MARKER_FILE);
    let pending = file.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&pending, json).is_ok() {
        let _ = std::fs::rename(&pending, &file);
    }
    let _ = std::fs::remove_file(pending);
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
        type_checked: 0,
        not_type_checked: Vec::new(),
        outside_moved: 0,
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
    // Said on stderr while it works: on a large repo the pass runs for
    // minutes, and a caller that reads silence as a hang kills it.
    let narrator = super::recheck::Narrator::to_stderr(started);
    // A repo whose changed files read the same as the copy the index was
    // built from is not re-scanned: the index already holds its answer
    // (carrick#3245).
    let always = std::env::var(RESCAN_ENV).is_ok_and(|value| value == "always");
    let mut unmoved: BTreeMap<String, BTreeMap<String, LineMap>> = BTreeMap::new();
    let mut rescan: Vec<Tree> = Vec::new();
    let mut rescanned: BTreeSet<&String> = BTreeSet::new();
    for (name, (tree, _)) in &changed {
        let lines = if always {
            None
        } else {
            index
                .repos
                .iter()
                .find(|indexed| &indexed.name == name)
                .and_then(|indexed| reads_the_same(&root, tree, indexed))
        };
        match lines {
            Some(lines) => {
                narrator.phase(
                    Instant::now(),
                    format!("nothing the scan reads changed in {name}"),
                );
                unmoved.insert(name.clone(), lines);
            }
            None => {
                rescan.push(tree.clone());
                rescanned.insert(name);
            }
        }
    }
    for entry in &mut output.repos {
        let name = super::index::repo_label(Path::new(&entry.repo));
        if changed.contains_key(&name) {
            entry.rescan = Some(if unmoved.contains_key(&name) {
                Rescan::NotNeeded
            } else {
                Rescan::Ran
            });
        }
    }
    // With nothing to re-scan this is the join alone, over the index's own
    // blobs (carrick#3245). The scope names projecting the index's rows
    // into the join's shape and starting no child at all; the index does not
    // carry the join's `unmatched.injected` (which call bases are set at run
    // time), and `judge` prints it, so the join runs, and the answer stays
    // the re-scan's.
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
    let lost = answers_lost(&rejoined.index, rescanned.iter().copied());
    if !lost.is_empty() {
        output.not_checked = Some(lost.join(" "));
    } else if let Some(sentence) =
        super::recheck::library_unavailable(&rejoined.index, changed.keys())
    {
        // The re-scan asked the library store and got no answer: a route
        // registered through a server package is missing, so "checked 0
        // routes" is not a pass, and a call to it is not a call to no route
        // (carrick#3249).
        output.not_checked = Some(sentence.to_string());
    }

    let wanted: BTreeMap<String, &BTreeSet<String>> = changed
        .iter()
        .map(|(name, (_, files))| (name.clone(), files))
        .collect();
    let trees: Vec<Tree> = changed.values().map(|(tree, _)| tree.clone()).collect();
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
        &touched_since_index(&index, &trees),
    );
    let judged = remap_lines(judged, &root, &rejoined.index, &unmoved);
    output.routes = judged.routes;
    output.calls = judged.calls;
    output.failing = judged.problems.iter().filter(|p| p.fails).count();
    output.type_checked = judged.type_checked;
    output.not_type_checked = judged.not_type_checked;
    output.outside_moved = judged.outside_moved;
    output.problems = judged.problems;
    output
}

/// The files each repo changed since the index was written, by repo label.
/// A repo with no entry, or `None`, is one git could not list: every file in
/// it counts as touched (carrick#3411).
pub(super) type Touched = BTreeMap<String, Option<BTreeSet<String>>>;

/// What each repo of the index changed since the commit it was indexed at
/// (untracked files, deletions and renames-away included). A repo whose
/// services carry no commit, or that git cannot list, is `None`. A repo in
/// `trees` is read from the checkout named there (carrick#3595); every other
/// from its own folder.
pub(super) fn touched_since_index(index: &LocalIndex, trees: &[Tree]) -> Touched {
    index
        .repos
        .iter()
        .map(|repo| {
            let commit = repo
                .services
                .iter()
                .map(|service| service.commit.as_str())
                .find(|commit| !commit.is_empty());
            let checkout = trees
                .iter()
                .find(|tree| super::checkout::same_dir(&tree.repo, Path::new(&repo.path)))
                .map_or(Path::new(&repo.path), |tree| tree.checkout.as_path());
            let files = commit
                .and_then(|commit| super::query::changed_since(checkout, commit))
                .map(|files| files.into_iter().collect());
            (repo.name.clone(), files)
        })
        .collect()
}

/// Every repo's changed source files, keyed by repo name, with the checkout
/// they were read from.
type Changed = BTreeMap<String, (Tree, BTreeSet<String>)>;

/// The changed files of every repo. Run inside a linked worktree of a repo,
/// that repo's changes are the worktree's: git runs there (carrick#3595).
fn changed_files(
    workspace: &super::workspace::Workspace,
    index: &LocalIndex,
) -> (Vec<ChangedRepo>, Changed) {
    let mut listed = Vec::new();
    let mut changed = Changed::new();
    let cwd = std::env::current_dir().ok();
    for repo in &workspace.repos {
        let name = super::index::repo_label(repo);
        let tree = cwd
            .as_deref()
            .map_or_else(|| Tree::at(repo), |cwd| Tree::from_dir(repo, cwd));
        let shown = repo.to_string_lossy().to_string();
        let checkout = tree.checkout.clone();
        let repo = &checkout;
        let text = checkout.to_string_lossy().to_string();
        let mut entry = ChangedRepo {
            repo: shown,
            main_branch: None,
            base: None,
            files: Vec::new(),
            skipped: None,
            unchecked: 0,
            no_base: false,
            rescan: None,
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
            changed.insert(name, (tree, files));
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
            rescan: None,
        });
    };
    for file in files {
        let Some((repo, relative, tree)) = index.locate_file(file) else {
            skip(&mut listed, file, "not in a repo of this workspace");
            continue;
        };
        if super::workspace::excluded_repo(root, &tree.repo) {
            skip(&mut listed, file, "not in a repo of this workspace");
            continue;
        }
        if !crate::file_finder::is_scanned_source(&tree.checkout.join(&relative), &tree.checkout) {
            skip(&mut listed, file, "not a source file the scan reads");
            continue;
        }
        if !repo.services.is_empty() && !repo.services.iter().any(|s| s.covers(&relative)) {
            skip(&mut listed, file, "no indexed service covers it");
            continue;
        }
        changed
            .entry(repo.name.clone())
            .or_insert_with(|| (tree, BTreeSet::new()))
            .1
            .insert(relative);
    }
    let mut repos: Vec<ChangedRepo> = changed
        .iter()
        .map(|(_, (tree, files))| ChangedRepo {
            repo: tree.repo.to_string_lossy().to_string(),
            main_branch: None,
            base: None,
            files: files.iter().cloned().collect(),
            skipped: None,
            unchecked: 0,
            no_base: false,
            rescan: None,
        })
        .collect();
    repos.extend(listed);
    (repos, changed)
}

/// The sentence for each re-scanned repo whose scan could not replay the
/// index's model answers (carrick#3000), in repo order.
pub(super) fn answers_lost<'a>(
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

/// For a repo whose every change since the index's commit reads the same to
/// the scan, where each changed source file's lines went; `None` when any
/// change could move what the scan reads, or the comparison cannot be made.
///
/// The commit is the one every indexed service of the repo was read at, from
/// a clean tree. A path the published input rule does not read is no change
/// to the scan. A path it reads must be a source file whose copy at that
/// commit and whose copy on disk both parse, are equal ignoring positions,
/// and hold the same type-bearing comments in the same places; a manifest, a
/// lockfile, a config, an added or deleted file, or a commit this clone lacks
/// takes the re-scan (carrick#3245), unless no service reads the file
/// ([`super::recheck::no_service_reads`]: a test file that loads what it
/// loaded).
fn reads_the_same(
    workspace_root: &Path,
    tree: &Tree,
    indexed: &IndexedRepo,
) -> Option<BTreeMap<String, LineMap>> {
    let repo = tree.checkout.as_path();
    // A repo the hosted index stood in for (`read_from_hosted`) is covered
    // too: its blobs were written beside the scanned ones, so the join below
    // retains them, and they describe the hosted commit from a clean tree.
    let commit = &indexed.services.first()?.commit;
    if commit.is_empty()
        || indexed
            .services
            .iter()
            .any(|service| &service.commit != commit || service.dirty)
    {
        return None;
    }
    let mut lines = BTreeMap::new();
    let mut unmapped = std::collections::HashSet::new();
    for file in super::query::changed_since(repo, commit)? {
        if !crate::change_scope::read_by_input_rule(&file) {
            continue;
        }
        match reads_same_since(repo, commit, &file) {
            Some(map) => {
                lines.insert(file, map);
            }
            None => {
                unmapped.insert(file);
            }
        }
    }
    // A change the scan reads differently is still no change when no service
    // reads the file (the carry rule `--changed-only` uses, one walk for all).
    if !unmapped.is_empty()
        && !super::recheck::no_service_reads(workspace_root, tree, indexed, &unmapped)
    {
        return None;
    }
    Some(lines)
}

/// The judged lines, in the edited files' own numbering. A check answered
/// from the index reads rows at the lines the index wrote them, which the
/// edit may have moved; each line in a file that read the same goes where its
/// node went. Reported lines on the other side (`other`) move the same way.
fn remap_lines(
    mut judged: Judged,
    root: &Path,
    rebuilt: &LocalIndex,
    unmoved: &BTreeMap<String, BTreeMap<String, LineMap>>,
) -> Judged {
    if unmoved.is_empty() {
        return judged;
    }
    // Workspace-shown file -> its map, and service -> repo, for the `other`
    // sites that name a service and a repo-relative file.
    let mut shown_maps: BTreeMap<String, &LineMap> = BTreeMap::new();
    let mut service_repo: BTreeMap<&str, &str> = BTreeMap::new();
    for repo in &rebuilt.repos {
        for service in &repo.services {
            service_repo.insert(service.name.as_str(), repo.name.as_str());
        }
        for (file, map) in unmoved.get(&repo.name).into_iter().flatten() {
            shown_maps.insert(shown(root, &repo.path, file), map);
        }
    }
    let move_line = |file: &str, line: Option<u32>| match (shown_maps.get(file), line) {
        (Some(map), Some(line)) => Some(map.map(line)),
        _ => line,
    };
    for problem in &mut judged.problems {
        problem.line = move_line(&problem.file, problem.line);
        for other in &mut problem.other {
            let Some((service, site)) = other.split_once(' ') else {
                continue;
            };
            let (file, line) = super::split_location(site);
            let Some(map) = service_repo
                .get(service)
                .and_then(|repo| unmoved.get(*repo))
                .and_then(|files| files.get(&file))
            else {
                continue;
            };
            if let Some(line) = line {
                *other = format!("{service} {file}:{}", map.map(line));
            }
        }
    }
    for row in &mut judged.not_type_checked {
        row.line = move_line(&row.file, row.line);
    }
    judged.problems.sort();
    judged.problems.dedup();
    judged.not_type_checked.sort();
    judged.not_type_checked.dedup();
    judged
}

pub(super) struct Judged {
    routes: usize,
    calls: usize,
    pub(super) problems: Vec<Problem>,
    type_checked: usize,
    not_type_checked: Vec<NotTypeChecked>,
    /// Rows outside the changed files that this change moved from compatible
    /// to a mismatch (carrick#2998).
    outside_moved: usize,
}

/// What the index held, before this pass, for a mismatching row of a file the
/// work did not touch.
enum OutsideBefore {
    /// The same row carried a mismatch already.
    Mismatch,
    /// The same row was resolved and compatible: this change broke the pair.
    Agreed,
    /// No such row, no verdict, or an unresolved one: it may have resolved
    /// for a reason unrelated to the edit (carrick#2961).
    NotJudged,
}

/// The earlier state of the row at the same line, in the same file, with the
/// same key. An untouched file's lines do not move.
fn outside_before(
    before: &LocalIndex,
    strict: bool,
    repo: &str,
    operation: &JoinedOperation,
) -> OutsideBefore {
    let Some(old) = indexed_rows(before, repo, &operation.file)
        .find(|old| same_operation(old, operation, strict) && old.line == operation.line)
    else {
        return OutsideBefore::NotJudged;
    };
    match old.verdict.as_ref() {
        Some(verdict)
            if matches!(
                verdict.result.as_deref(),
                Some("type_mismatch" | "method_mismatch")
            ) =>
        {
            OutsideBefore::Mismatch
        }
        Some(verdict)
            if verdict.state == "resolved" && verdict.result.as_deref() == Some("compatible") =>
        {
            OutsideBefore::Agreed
        }
        _ => OutsideBefore::NotJudged,
    }
}

/// The join's wire string back to the shared type; absent stays absent.
fn edge_source_stated(wire: Option<&str>) -> Option<EdgeSource> {
    match wire {
        Some("fact") => Some(EdgeSource::Fact),
        Some(_) => Some(EdgeSource::Candidate),
        None => None,
    }
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
pub(super) fn judge(
    root: &Path,
    before: &LocalIndex,
    same_build: bool,
    rebuilt: &LocalIndex,
    join: &super::LocalJoin,
    changed: &BTreeMap<String, &BTreeSet<String>>,
    touched: &Touched,
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
    // Whether a file changed since the index was written. A repo git could
    // not list counts every file as changed (carrick#3411).
    let is_touched = |repo: &str, file: &str| match touched.get(repo) {
        Some(Some(files)) => files.contains(file),
        _ => true,
    };
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
        type_checked: 0,
        not_type_checked: Vec::new(),
        outside_moved: 0,
    };
    for operation in &join.operations {
        let Some(&(repo, repo_path)) = repo_of.get(operation.service.as_str()) else {
            continue;
        };
        let in_change = is_changed(repo, &operation.file);
        // A row in a file the work did not touch is read only when its repo
        // was re-scanned in this run (carrick#2998, rule 1).
        if !in_change && !changed.contains_key(repo) {
            continue;
        }
        if in_change {
            match operation.role {
                Role::Producer => judged.routes += 1,
                Role::Consumer => judged.calls += 1,
            }
        }
        let Some(item) = item_of(repo, operation) else {
            continue;
        };
        let mismatch = item.verdict.as_ref().is_some_and(|verdict| {
            matches!(
                verdict.result.as_deref(),
                Some("type_mismatch" | "method_mismatch")
            )
        });
        // Outside the change only a rebuilt mismatch is a candidate (rule 2).
        if !in_change && !mismatch {
            continue;
        }
        // A matched pair the check could not judge is named, not passed
        // (carrick#3012): no verdict, or a side without a usable type.
        let unjudged = item
            .verdict
            .as_ref()
            .is_none_or(|verdict| verdict.state == "unresolved");
        if !mismatch && unjudged && !item.counterparts.is_empty() {
            judged.not_type_checked.push(NotTypeChecked {
                file: shown(root, repo_path, &operation.file),
                line: operation.line,
                role: item.kind.as_str().to_string(),
                method: operation.method.clone(),
                path: operation.path.clone(),
                reason: item
                    .verdict
                    .as_ref()
                    .map_or_else(|| "no verdict".to_string(), |v| one_line(&v.detail)),
                descriptor: match operation.role {
                    Role::Consumer => built_request(operation),
                    Role::Producer => only_built_requests(&item.counterparts, join),
                },
            });
            continue;
        }
        let Some(verdict) = item.verdict.as_ref() else {
            continue;
        };
        if !mismatch {
            if verdict.state == "resolved" && !item.counterparts.is_empty() {
                judged.type_checked += 1;
            }
            continue;
        }
        if in_change {
            judged.type_checked += 1;
        }
        let finding = super::index::finding_for(operation, &join.findings);
        // A finding resting only on rows the source states, whose verdict was
        // reached or needs none: the rule the cloud enforces on (cloud#599).
        // A verdict drawn from an edge with no finding behind it states no
        // source, so it is reported and does not fail the run.
        let fact_inputs = finding.map(|finding| {
            (
                edge_source_stated(finding.edge_source.as_deref()),
                finding.verdict_state.as_deref() == Some("unresolved"),
            )
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
        // A row outside the change is the work's only if the index held the
        // same row, at the same line, with a pair that agreed (rules 3 to 6).
        let outside = if in_change {
            None
        } else {
            match outside_before(before, strict, repo, operation) {
                // It carried a mismatch before the change: not this work's.
                OutsideBefore::Mismatch => continue,
                other => Some(other),
            }
        };
        let moved_here = matches!(outside, Some(OutsideBefore::Agreed));
        let already_here = already && in_change;
        // The one break answer, with this run's own "before" as main's side:
        // the index held the mismatch (on main), or held the pair agreeing
        // (this change broke it), or said nothing about it (not placed). A source
        // is passed as stated: no source (an SDK-mediated edge) stays `None`
        // and the shared function enforces it, as the PR comment does.
        let certain = fact_inputs.is_some_and(|(edge_source, unresolved)| {
            let on_main = if already_here {
                Some(true)
            } else if in_change || moved_here {
                Some(false)
            } else {
                None
            };
            mismatch_break_status(
                edge_source,
                unresolved.then_some(VerdictState::Unresolved),
                on_main,
                None,
            ) == BreakStatus::Certain
        });
        // Outside the change a sibling's mismatch at another line says
        // nothing about this row; `outside_before` decided on the line.
        let already = already && in_change;
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
            descriptor: built_request(operation),
            already_indexed: already,
            unconfirmed: false,
            outside_the_change: !in_change,
            not_judged_before: matches!(outside, Some(OutsideBefore::NotJudged)),
            moved_outside: moved_here,
            after: verdict.result.clone().unwrap_or_default(),
            other_build: certain && !same_build,
            fails: certain && same_build,
        });
        // Only a pair the index held compatible moved; one with no earlier
        // verdict is listed and counted apart, as "not judged before".
        if moved_here {
            judged.outside_moved += 1;
        }
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
                // when the index matched it and one of its producers sits in
                // a file changed since the index: the work took its route
                // away. Otherwise the re-scan alone stopped pairing it, and
                // that is drift, not a break (carrick#3411).
                if !is_changed(repo, &file) {
                    if !was_matched(before, strict, repo, operation) {
                        continue;
                    }
                    let producers: Vec<&super::read_model::Counterpart> =
                        indexed_rows(before, repo, &file)
                            .filter(|old| {
                                same_operation(old, operation, strict)
                                    && old.line == operation.line
                                    && !old.counterparts.is_empty()
                            })
                            .flat_map(|old| old.counterparts.iter())
                            .collect();
                    let producer_touched = producers.iter().any(|counterpart| {
                        repo_of.get(counterpart.service.as_str()).is_none_or(
                            |&(producer_repo, _)| {
                                is_touched(producer_repo, &counterpart.file)
                                    || rekeyed(before, rebuilt, strict, producer_repo, counterpart)
                            },
                        )
                    });
                    if !producer_touched {
                        note_drift(&file, line, operation, &producers);
                        continue;
                    }
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
                    descriptor: built_request(operation),
                    already_indexed: already,
                    unconfirmed,
                    outside_the_change: !is_changed(repo, &file),
                    not_judged_before: false,
                    moved_outside: false,
                    after: "no_route".to_string(),
                    other_build,
                    fails: new_fact && !other_build,
                });
            }
        }
    }
    // A call in a file the work did not touch, kept alive only by
    // documentation (carrick#3192). The routes package states its operations
    // in a handler-less table that the scan folds onto the handler row with
    // the same method and path. When the work moves the handler's path, the
    // table row has no partner and stays a route, so the call still matches
    // and the unmatched loop above sees nothing. The call is named when the
    // index matched it to producers that all sit in changed files, and every
    // producer it matches now is a documentation row.
    for operation in join
        .operations
        .iter()
        .filter(|operation| operation.role == Role::Consumer)
    {
        let Some(&(repo, repo_path)) = repo_of.get(operation.service.as_str()) else {
            continue;
        };
        if is_changed(repo, &operation.file) {
            continue;
        }
        let Some(old) = indexed_rows(before, repo, &operation.file).find(|old| {
            same_operation(old, operation, strict)
                && old.line == operation.line
                && !old.counterparts.is_empty()
        }) else {
            continue;
        };
        let producers_changed = old.counterparts.iter().all(|counterpart| {
            repo_of
                .get(counterpart.service.as_str())
                .is_some_and(|&(producer_repo, _)| is_changed(producer_repo, &counterpart.file))
        });
        if !producers_changed {
            continue;
        }
        let served_by: Vec<&JoinedOperation> = join
            .matches
            .iter()
            .filter(|edge| {
                edge.consumer_service == operation.service
                    && edge.consumer_key == operation.key
                    && edge.consumer_file.as_deref() == Some(operation.file.as_str())
                    && edge.consumer_line == operation.line
            })
            .flat_map(|edge| {
                join.operations.iter().filter(|producer| {
                    producer.role == Role::Producer
                        && producer.service == edge.producer_service
                        && producer.key == edge.producer_key
                })
            })
            .collect();
        let documentation_only = !served_by.is_empty()
            && served_by.iter().all(|producer| {
                crate::agents::FileOrchestrator::is_documentation_route(
                    producer.resolution_source,
                    producer.handler.as_deref(),
                )
            });
        if !documentation_only {
            continue;
        }
        let document = served_by[0];
        let document_at = repo_of.get(document.service.as_str()).map_or_else(
            || document.file.clone(),
            |&(_, path)| shown(root, path, &document.file),
        );
        let document_line = document.line.map_or(String::new(), |l| format!(":{l}"));
        // The handler that served this call, read again at the same line and
        // the same verb under another path: it moved, and the documentation
        // is all that still states the old route.
        let moved_to = old.counterparts.iter().find_map(|counterpart| {
            let &(producer_repo, producer_path) = repo_of.get(counterpart.service.as_str())?;
            let row = indexed_rows(rebuilt, producer_repo, &counterpart.file).find(|row| {
                row.kind == super::read_model::ItemKind::Route
                    && row.source == Source::Fact
                    && row.line == counterpart.line
                    && row.method == operation.method
                    && row.key != operation.key
            })?;
            Some((
                shown(root, producer_path, &counterpart.file),
                counterpart.line,
                row.path.clone(),
            ))
        });
        let confirmed = moved_to.is_some();
        let reason = match moved_to {
            Some((file, line, path)) => format!(
                "the handler at {file}{} now serves {path}; only the documentation at \
                 {document_at}{document_line} still states this route",
                line.map_or(String::new(), |l| format!(":{l}")),
            ),
            None => format!(
                "the handler that served this call was not read after the change; only the \
                 documentation at {document_at}{document_line} still states this route"
            ),
        };
        judged.problems.push(Problem {
            kind: ProblemKind::NoRoute,
            file: shown(root, repo_path, &operation.file),
            line: operation.line,
            role: "call".to_string(),
            method: operation.method.clone(),
            path: operation.path.clone(),
            other: Vec::new(),
            reason,
            source: Source::Fact.as_str().to_string(),
            descriptor: false,
            already_indexed: false,
            unconfirmed: !confirmed,
            outside_the_change: true,
            other_build: false,
            not_judged_before: false,
            moved_outside: false,
            after: "no_route".to_string(),
            fails: confirmed,
        });
    }
    judged.problems.sort();
    judged.problems.dedup();
    judged.not_type_checked.sort();
    judged.not_type_checked.dedup();
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

/// Whether the rebuilt index reads a route at an old producer's file and line
/// under another operation than the index held there (carrick#3412). The file
/// is unchanged since the index, so its lines did not move: a route still read
/// at that line under another path was re-keyed by something on its mount
/// chain, which the work changed. Any route the index held there that is still
/// read under its own key and dispatch case means nothing re-keyed it. No
/// route read there at all is no evidence (a re-scan that did not re-extract
/// the handler reads the same, carrick#3411), so it is false.
fn rekeyed(
    before: &LocalIndex,
    rebuilt: &LocalIndex,
    strict: bool,
    repo: &str,
    producer: &super::read_model::Counterpart,
) -> bool {
    let Some(line) = producer.line else {
        return false;
    };
    let at_line = |item: &&IndexedItem| {
        item.kind == super::read_model::ItemKind::Route && item.line == Some(line)
    };
    let old: Vec<&IndexedItem> = indexed_rows(before, repo, &producer.file)
        .filter(at_line)
        .collect();
    let new: Vec<&IndexedItem> = indexed_rows(rebuilt, repo, &producer.file)
        .filter(at_line)
        .collect();
    !old.is_empty()
        && !new.is_empty()
        && !old.iter().any(|old| {
            new.iter()
                .any(|new| old.key == new.key && (!strict || old.dispatch == new.dispatch))
        })
}

/// The environment variable that prints each `rescan-drift` line on stderr.
pub(super) const DRIFT_ENV: &str = "CARRICK_RECHECK_DRIFT";

/// Say that a call outside the change was not reported: the index paired it,
/// no producer file of it changed since, and the re-scan stopped pairing it
/// (carrick#3411). One line to the debug log; also to stderr under
/// [`DRIFT_ENV`].
fn note_drift(
    file: &str,
    line: Option<u32>,
    operation: &JoinedOperation,
    producers: &[&super::read_model::Counterpart],
) {
    let producers = producers
        .iter()
        .map(|p| match p.line {
            Some(at) => format!("{} {}:{at}", p.service, p.file),
            None => format!("{} {}", p.service, p.file),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let at = line.map_or(String::new(), |l| format!(":{l}"));
    let message = format!(
        "rescan-drift {file}{at} {} {} left with no route; old producers unchanged since the index: {producers}",
        operation.method, operation.path
    );
    tracing::debug!("{message}");
    if std::env::var_os(DRIFT_ENV).is_some() {
        crate::errln!("carrick: {message}");
    }
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
        // A row outside the changed files that this change moved is shown
        // whether or not it fails, so the agent sees every likely break
        // (carrick#2998). Past the cap the rest are counted, not printed.
        let mut moved_shown = 0;
        let mut moved_hidden = 0;
        let mut counted: Vec<&Problem> = Vec::new();
        let mut listed: Vec<&Problem> = Vec::new();
        for problem in &self.problems {
            let always = all || problem.fails || problem.other_build;
            if always {
                counted.push(problem);
                listed.push(problem);
            } else if problem.moved_outside {
                counted.push(problem);
                if moved_shown < MAX_MOVED_LISTED {
                    moved_shown += 1;
                    listed.push(problem);
                } else {
                    moved_hidden += 1;
                }
            }
        }
        // Each problem not listed, counted by why, in the order `--all`
        // prints the reasons in. A file the work did not touch adds to this
        // line only by failing, which lists it: the summary is about the
        // edited files.
        let mut unlisted: BTreeMap<Unlisted, usize> = BTreeMap::new();
        for problem in &self.problems {
            if let (false, Some(why)) = (all, problem.unlisted()) {
                // A file the work did not touch counts only when its verdict
                // was not judged before; the rest lists by failing.
                if problem.outside_the_change && why != Unlisted::OutsideNotJudged {
                    continue;
                }
                *unlisted.entry(why).or_default() += 1;
            }
        }
        let incompatible = counted
            .iter()
            .filter(|p| p.kind == ProblemKind::Incompatible)
            .count();
        let no_route = counted.len() - incompatible;
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
        let advisory = counted.len().saturating_sub(self.failing);
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
        if !self.not_type_checked.is_empty() {
            out.push_str(&format!(
                ", {} {NOT_TYPE_CHECKED_SUFFIX}, run with --all to list them",
                self.not_type_checked.len()
            ));
        }
        if self.outside_moved > 0 {
            out.push_str(&outside_moved_clause(self.outside_moved));
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
            } else if problem.outside_the_change
                && problem.kind == ProblemKind::Incompatible
                && problem.fails
            {
                OUTSIDE_FAILING_NOTE
            } else {
                problem.unlisted().map_or("", Unlisted::note)
            };
            out.push_str(&format!(
                "{at}  {what}  {} {} {}{with}: {}{note}\n",
                problem.role, problem.method, problem.path, problem.reason
            ));
        }
        if moved_hidden > 0 {
            out.push_str(&format!(
                "and {moved_hidden} more: `carrick check --changed --all` lists them\n"
            ));
        }
        if all {
            for row in &self.not_type_checked {
                let at = match row.line {
                    Some(line) => format!("{}:{line}", row.file),
                    None => row.file.clone(),
                };
                out.push_str(&format!(
                    "{at}  not type-checked  {} {} {}: {}\n",
                    row.role, row.method, row.path, row.reason
                ));
            }
        }
        if self.nothing_type_checked() {
            out.push_str(NOTHING_TYPE_CHECKED);
            out.push('\n');
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
    /// A call read from a request the code builds and returns (carrick#3470).
    BuiltRequest,
    NotStated,
    OutsideNotJudged,
}

impl Unlisted {
    fn note(self) -> &'static str {
        match self {
            Unlisted::AlreadyIndexed => " [already in the index, does not fail]",
            Unlisted::Unconfirmed => " [unconfirmed: the base is set at run time, does not fail]",
            Unlisted::Candidate => " [model candidate, does not fail]",
            Unlisted::BuiltRequest => " [built request not seen sent, does not fail]",
            Unlisted::NotStated => " [source not stated, does not fail]",
            Unlisted::OutsideNotJudged => OUTSIDE_NOT_JUDGED_NOTE,
        }
    }

    fn counted(self, count: usize) -> String {
        match self {
            Unlisted::AlreadyIndexed => format!("{count} already in the index"),
            Unlisted::Unconfirmed => format!("{count} unconfirmed"),
            Unlisted::Candidate => plural(count, "model candidate", "model candidates"),
            Unlisted::BuiltRequest => plural(
                count,
                "built request not seen sent",
                "built requests not seen sent",
            ),
            Unlisted::NotStated => format!("{count} with no stated source"),
            Unlisted::OutsideNotJudged => outside_not_judged_counted(count),
        }
    }
}

impl Problem {
    /// Why this problem is listed only with `--all`; `None` when it fails
    /// the run. A problem the index already held says so first, whatever
    /// else holds for it.
    /// A problem a plain `check --changed` lists: it fails the run, or rests
    /// on an index another build wrote. The per-edit re-check states the
    /// edited file's own no-route calls by this same rule (`recheck.rs`).
    pub(super) fn listed_without_all(&self) -> bool {
        self.unlisted().is_none()
    }

    fn unlisted(&self) -> Option<Unlisted> {
        match (self.fails, self.source.as_str()) {
            (true, _) => None,
            // Listed without `--all`: it is why the run exits 2.
            _ if self.other_build => None,
            _ if self.not_judged_before => Some(Unlisted::OutsideNotJudged),
            _ if self.already_indexed => Some(Unlisted::AlreadyIndexed),
            _ if self.unconfirmed => Some(Unlisted::Unconfirmed),
            _ if self.descriptor => Some(Unlisted::BuiltRequest),
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

/// A call row read from a request the code builds and returns, not from a
/// call seen sent (carrick#3470).
fn built_request(operation: &JoinedOperation) -> bool {
    operation.role == Role::Consumer
        && operation.resolution_source
            == Some(crate::agents::file_analyzer_agent::ResolutionSource::RequestDescriptor)
}

/// Every call row a route pairs with is a built request (carrick#3532): its
/// matched method and path are the check such a pair can have, as for the
/// call side. A counterpart with no call row of its own, or sharing its line
/// with a call that is not built, keeps the pair counted as unjudged.
fn only_built_requests(
    counterparts: &[super::read_model::Counterpart],
    join: &super::LocalJoin,
) -> bool {
    counterparts.iter().all(|counterpart| {
        let mut calls = join
            .operations
            .iter()
            .filter(|operation| {
                operation.role == Role::Consumer
                    && operation.service == counterpart.service
                    && operation.file == counterpart.file
                    && operation.line == counterpart.line
            })
            .peekable();
        calls.peek().is_some() && calls.all(built_request)
    })
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_leaves_its_times_and_exit_where_the_hooks_read_them() {
        let root = tempfile::tempdir().unwrap();
        record_check_marker(root.path(), 10, 20, 0);
        assert!(
            !root.path().join(".carrick").exists(),
            "a workspace with no .carrick/ is not given one"
        );
        std::fs::create_dir(root.path().join(".carrick")).unwrap();
        record_check_marker(root.path(), 10, 20, 1);
        let body =
            std::fs::read_to_string(root.path().join(".carrick/last-check-changed.json")).unwrap();
        let marker: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(marker["started_at_ms"], 10);
        assert_eq!(marker["finished_at_ms"], 20);
        assert_eq!(marker["exit"], 1);
        let left: Vec<_> = std::fs::read_dir(root.path().join(".carrick"))
            .unwrap()
            .collect();
        assert_eq!(left.len(), 1, "no temp file is left beside it");
    }

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
            descriptor: false,
            already_indexed: false,
            unconfirmed: false,
            outside_the_change: false,
            not_judged_before: false,
            moved_outside: false,
            after: String::new(),
            other_build: false,
            fails,
        }
    }

    /// Rows outside the changed files whose verdict the change moved: the
    /// summary says so, a failing one carries its note, and one not judged
    /// before adds only to its own count (carrick#2998).
    #[test]
    fn outside_rows_are_said_on_the_summary_and_noted() {
        let mut broken = problem(ProblemKind::Incompatible, true);
        broken.outside_the_change = true;
        let mut unjudged = problem(ProblemKind::Incompatible, false);
        unjudged.outside_the_change = true;
        unjudged.not_judged_before = true;
        unjudged.path = "/u".to_string();
        let output = |problems: Vec<Problem>, failing: usize, moved: usize| ChangedOutput {
            calls: 1,
            files: 1,
            services: 2,
            problems,
            failing,
            outside_moved: moved,
            not_checked: None,
            ..not_checked("/w", "")
        };

        let text = output(vec![broken.clone(), unjudged.clone()], 1, 2).render(false);
        let summary = text.lines().next().unwrap();
        assert!(
            summary
                .contains(", plus 1 outside the changed files not judged before, run with --all"),
            "{text}"
        );
        assert!(
            summary.ends_with("; 2 rows outside the changed files whose verdict this change moved"),
            "{text}"
        );
        assert!(
            text.contains("[outside the changed files: this change moved its verdict]"),
            "{text}"
        );
        assert!(
            !text.contains("not judged before, does not fail]"),
            "{text}"
        );

        let all = output(vec![unjudged.clone()], 0, 1).render(true);
        assert!(
            all.contains("[outside the changed files: not judged before, does not fail]"),
            "{all}"
        );
        assert!(
            all.lines()
                .next()
                .unwrap()
                .ends_with("; 1 row outside the changed files whose verdict this change moved"),
            "{all}"
        );
        let none = output(vec![], 0, 0).render(false);
        assert!(!none.contains("outside the changed files"), "{none}");
    }

    /// A row outside the changed files that this change moved is listed on a
    /// plain run with its note even when it does not fail, counted on the
    /// summary as `--all` counts it, and cut at ten with an overflow line.
    #[test]
    fn moved_rows_outside_the_change_are_listed_without_all() {
        let moved = |n: usize| {
            let mut row = problem(ProblemKind::Incompatible, false);
            row.outside_the_change = true;
            row.moved_outside = true;
            row.path = format!("/m{n:02}");
            row
        };
        let output = |problems: Vec<Problem>| ChangedOutput {
            calls: 0,
            files: 1,
            services: 2,
            outside_moved: problems.len(),
            problems,
            failing: 0,
            not_checked: None,
            ..not_checked("/w", "")
        };

        let few = output((0..3).map(moved).collect()).render(false);
        let summary = few.lines().next().unwrap();
        assert!(
            summary.contains("3 incompatible, 0 reach no route (3 of them do not fail the check)"),
            "{few}"
        );
        assert_eq!(
            few.matches("[model candidate, does not fail]").count(),
            3,
            "{few}"
        );
        assert!(!few.contains("more:"), "{few}");

        let many = output((0..13).map(moved).collect());
        let text = many.render(false);
        assert!(
            text.lines().next().unwrap().contains("13 incompatible"),
            "{text}"
        );
        assert_eq!(
            text.matches("[model candidate, does not fail]").count(),
            10,
            "{text}"
        );
        assert!(
            text.contains("and 3 more: `carrick check --changed --all` lists them\n"),
            "{text}"
        );
        let all = many.render(true);
        assert_eq!(
            all.matches("[model candidate, does not fail]").count(),
            13,
            "{all}"
        );
        assert!(!all.contains("more:"), "{all}");
    }

    /// Every count the plain summary line prints is backed on a plain run:
    /// the rows are listed, or the output says how to list them. A count
    /// with no row in this table fails, so a new count must say how its rows
    /// are reached (carrick#2998).
    #[test]
    fn every_summary_count_lists_its_rows_or_says_how() {
        let mut moved = problem(ProblemKind::Incompatible, false);
        moved.file = "web/src/other.ts".to_string();
        moved.outside_the_change = true;
        moved.moved_outside = true;
        let mut moved_late = moved.clone();
        moved_late.path = "/m2".to_string();
        let mut inside = problem(ProblemKind::Incompatible, true);
        inside.path = "/i".to_string();
        let mut no_route = problem(ProblemKind::NoRoute, true);
        no_route.path = "/n".to_string();
        let mut unconfirmed = problem(ProblemKind::NoRoute, false);
        unconfirmed.unconfirmed = true;
        unconfirmed.path = "/u".to_string();
        let unchecked = NotTypeChecked {
            file: "web/src/a.ts".to_string(),
            line: Some(9),
            role: "call".to_string(),
            method: "GET".to_string(),
            path: "/t".to_string(),
            reason: "no verdict".to_string(),
            descriptor: false,
        };
        let out = ChangedOutput {
            calls: 1,
            files: 1,
            services: 2,
            problems: vec![inside, no_route, unconfirmed, moved, moved_late],
            failing: 2,
            outside_moved: 2,
            not_type_checked: vec![unchecked],
            not_checked: None,
            ..not_checked("/w", "")
        };
        let text = out.render(false);
        let summary = text.lines().next().unwrap();
        let counts = summary
            .split_once(" services: ")
            .or_else(|| summary.split_once(" service: "));
        let counts = counts.unwrap_or_else(|| panic!("{summary}")).1;
        let body: Vec<&str> = text.lines().skip(1).collect();
        let clauses: Vec<&str> = counts.split([',', ';']).map(str::trim).collect();
        // (what the clause counts, the rows a plain run lists for it)
        let table: [(&str, usize); 4] = [
            (
                " incompatible",
                body.iter()
                    .filter(|l| l.contains("  incompatible  "))
                    .count(),
            ),
            (
                " reach no route",
                body.iter().filter(|l| l.contains("  no route  ")).count(),
            ),
            (
                " outside the changed files whose verdict this change moved",
                body.iter()
                    .filter(|l| l.starts_with("web/src/other.ts"))
                    .count()
                    + body
                        .iter()
                        .filter_map(|l| l.strip_prefix("and "))
                        .filter_map(|l| l.split_whitespace().next()?.parse::<usize>().ok())
                        .sum::<usize>(),
            ),
            (" not type-checked", 0),
        ];
        let mut seen = 0;
        for (at, clause) in clauses.iter().enumerate() {
            let Some((number, rest)) = clause.split_once(' ') else {
                continue;
            };
            let Ok(count) = number.parse::<usize>() else {
                continue;
            };
            let rest = format!(" {rest}");
            let Some((_, listed)) = table.iter().find(|(key, _)| {
                rest.contains(key)
                    || (key == &" reach no route" && rest.contains(" reaches no route"))
            }) else {
                panic!(
                    "the summary counts `{clause}` with no row in this table: list its rows or point to --all\n{text}"
                );
            };
            seen += 1;
            // The clause right after it says how to list the rows.
            let pointer = clauses
                .get(at + 1)
                .is_some_and(|next| next.starts_with("run with --all"));
            assert!(
                count == 0 || *listed >= count || pointer,
                "`{clause}`: {listed} listed, no pointer\n{text}"
            );
        }
        assert_eq!(seen, 4, "{text}");
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
        let judged = judge(
            Path::new("/w"),
            &before,
            true,
            &rebuilt,
            &join,
            &changed,
            &Touched::new(),
        );
        assert_eq!(judged.problems.len(), 1, "{:?}", judged.problems);
        judged.problems.into_iter().next().unwrap()
    }

    /// One untouched call at `client.ts:10` the index paired with `h.ts` in
    /// service `web`, now unmatched; `touched` says which files moved since
    /// the index.
    fn judged_untouched_call(touched: Touched) -> Vec<Problem> {
        let before = index_of(vec![gone_call()]);
        judged_untouched_call_against(before, index_of(Vec::new()), touched)
    }

    /// The call at `client.ts:10` paired with the route at `h.ts:1`.
    fn gone_call() -> serde_json::Value {
        serde_json::json!({
            "kind": "call", "service": "web", "key": "http|GET|/gone", "dispatch": null,
            "method": "GET", "path": "/gone", "line": 10, "col": null, "source": "fact",
            "resolution_source": null, "evidence": null,
            "counterparts": [{"role": "producer", "service": "web", "file": "h.ts", "line": 1}],
            "verdict": null
        })
    }

    /// A route row at `h.ts:1` under `key`.
    fn route_at_h(key: &str) -> serde_json::Value {
        let path = key.rsplit('|').next().unwrap();
        serde_json::json!({
            "kind": "route", "service": "web", "key": key, "dispatch": null,
            "method": "GET", "path": path, "line": 1, "col": null, "source": "fact",
            "resolution_source": null, "evidence": null,
            "counterparts": [], "verdict": null
        })
    }

    /// An index of service `web` whose rows sit in the files `files` names.
    fn index_with_files(files: serde_json::Value) -> LocalIndex {
        serde_json::from_value(serde_json::json!({
            "version": 1, "scanner_version": "0", "indexed_at": "", "repos": [{
                "path": "/w/web", "name": "web",
                "services": [{
                    "name": "web", "commit": "", "indexed_at": "", "boundary": null,
                    "routes": 0, "calls": 0
                }],
                "files": files
            }]
        }))
        .expect("an index")
    }

    /// The untouched call at `client.ts:10`, now unmatched, judged against the
    /// index `before` held and the one the re-scan `rebuilt`.
    fn judged_untouched_call_against(
        before: LocalIndex,
        rebuilt: LocalIndex,
        touched: Touched,
    ) -> Vec<Problem> {
        let join: super::super::LocalJoin = serde_json::from_value(serde_json::json!({
            "scanner_version": "0",
            "operations": [{
                "role": "consumer", "service": "web", "key": "http|GET|/gone",
                "method": "GET", "path": "/gone", "file": "client.ts", "line": 10,
                "resolution_source": null, "handler": null, "dispatch": null
            }],
            "matches": [], "findings": [],
            "unmatched": [{
                "method": "GET", "path": "/gone",
                "call_sites": ["client.ts:10"], "injected": []
            }],
            "type_check": {"state": "ran"}
        }))
        .expect("a join");
        let mut join = join;
        join.operations[0].resolution_source =
            Some(crate::agents::file_analyzer_agent::ResolutionSource::RequestSpec);
        let edited: BTreeSet<String> = ["edited.ts".to_string()].into();
        let changed: BTreeMap<String, &BTreeSet<String>> = [("web".to_string(), &edited)].into();
        judge(
            Path::new("/w"),
            &before,
            true,
            &rebuilt,
            &join,
            &changed,
            &touched,
        )
        .problems
    }

    /// A call outside the change whose old producers all sit in files unchanged
    /// since the index lost its route to the re-scan, not to the work
    /// (carrick#3411).
    #[test]
    fn an_untouched_call_whose_producer_file_is_unchanged_is_drift_not_a_problem() {
        let touched: Touched = [("web".to_string(), Some(["edited.ts".to_string()].into()))].into();
        let problems = judged_untouched_call(touched);
        assert!(problems.is_empty(), "{problems:?}");
    }

    /// The same call, with its producer's file among those changed since the
    /// index: the work took the route away.
    #[test]
    fn an_untouched_call_whose_producer_file_changed_is_still_reported() {
        let touched: Touched = [(
            "web".to_string(),
            Some(["edited.ts".to_string(), "h.ts".to_string()].into()),
        )]
        .into();
        let problems = judged_untouched_call(touched);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].kind, ProblemKind::NoRoute);
        assert!(
            problems[0].outside_the_change && problems[0].fails,
            "{problems:?}"
        );
    }

    /// Only `edited.ts` changed since the index: the producer's `h.ts` did not.
    fn only_edited_touched() -> Touched {
        [("web".to_string(), Some(["edited.ts".to_string()].into()))].into()
    }

    /// The index held the call and its route at `h.ts:1`; `rebuilt_h` is what
    /// the re-scan reads in `h.ts`.
    fn judged_against_rebuilt_h(rebuilt_h: Vec<serde_json::Value>) -> Vec<Problem> {
        let before = index_with_files(serde_json::json!({
            "client.ts": [gone_call()],
            "h.ts": [route_at_h("http|GET|/gone")]
        }));
        let rebuilt = index_with_files(serde_json::json!({ "h.ts": rebuilt_h }));
        judged_untouched_call_against(before, rebuilt, only_edited_touched())
    }

    /// The producer's file is unchanged, but the re-scan reads its route at
    /// the same line under another path: something on its mount chain moved
    /// it, so the work took the call's route away (carrick#3412).
    #[test]
    fn an_untouched_call_whose_unchanged_producer_was_rekeyed_is_reported() {
        let problems = judged_against_rebuilt_h(vec![route_at_h("http|GET|/moved")]);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].kind, ProblemKind::NoRoute);
        assert!(
            problems[0].outside_the_change && problems[0].fails,
            "{problems:?}"
        );
    }

    /// The producer's route is not read at its line at all: that reads the
    /// same as a re-scan that failed to re-extract it, so it stays drift.
    #[test]
    fn an_untouched_call_whose_unchanged_producer_vanished_is_still_drift() {
        let problems = judged_against_rebuilt_h(Vec::new());
        assert!(problems.is_empty(), "{problems:?}");
    }

    /// The route at that line is still read under its old key beside another
    /// one: nothing re-keyed it, so the lost pairing is drift.
    #[test]
    fn an_untouched_call_whose_unchanged_producer_kept_its_key_is_still_drift() {
        let problems = judged_against_rebuilt_h(vec![
            route_at_h("http|GET|/gone"),
            route_at_h("http|GET|/moved"),
        ]);
        assert!(problems.is_empty(), "{problems:?}");
    }

    /// A row outside the changed files counts as moved only when the index
    /// held it resolved and compatible. One the index never judged is reported
    /// and does not fail, and is not "moved" (carrick#2998).
    #[test]
    fn only_a_row_the_index_held_compatible_counts_as_moved() {
        let row = |line: u32, key: &str, verdict: serde_json::Value| {
            serde_json::json!({
                "kind": "call", "service": "web", "key": key, "dispatch": null,
                "method": "GET", "path": key.rsplit('|').next().unwrap(),
                "line": line, "col": null, "source": "fact",
                "resolution_source": null, "evidence": null,
                "counterparts": [{"role": "producer", "service": "api", "file": "h.ts", "line": 1}],
                "verdict": verdict
            })
        };
        let compatible =
            serde_json::json!({"state": "resolved", "result": "compatible", "detail": ""});
        let mismatch = serde_json::json!({"state": "resolved", "result": "type_mismatch", "detail": "differs"});
        let before = index_of(vec![
            row(10, "http|GET|/was-fine", compatible),
            row(20, "http|GET|/never-judged", serde_json::Value::Null),
        ]);
        let rebuilt = index_of(vec![
            row(10, "http|GET|/was-fine", mismatch.clone()),
            row(20, "http|GET|/never-judged", mismatch),
        ]);
        let operation = |line: u32, path: &str| {
            serde_json::json!({
                "role": "consumer", "service": "web", "key": format!("http|GET|{path}"),
                "method": "GET", "path": path, "file": "client.ts", "line": line,
                "resolution_source": null, "handler": null, "dispatch": null
            })
        };
        let join: super::super::LocalJoin = serde_json::from_value(serde_json::json!({
            "scanner_version": "0",
            "operations": [operation(10, "/was-fine"), operation(20, "/never-judged")],
            "matches": [], "findings": [], "unmatched": [],
            "type_check": {"state": "ran"}
        }))
        .expect("a join");
        let edited: BTreeSet<String> = ["edited.ts".to_string()].into();
        let changed: BTreeMap<String, &BTreeSet<String>> = [("web".to_string(), &edited)].into();

        let judged = judge(
            Path::new("/w"),
            &before,
            true,
            &rebuilt,
            &join,
            &changed,
            &Touched::new(),
        );

        assert_eq!(judged.problems.len(), 2, "{:?}", judged.problems);
        assert_eq!(judged.outside_moved, 1, "{:?}", judged.problems);
        let unjudged = judged
            .problems
            .iter()
            .filter(|p| p.not_judged_before)
            .count();
        assert_eq!(unjudged, 1, "{:?}", judged.problems);
    }

    /// One consumer call at `client.ts:10`, edited in this change and now a
    /// type mismatch, whose finding states `edge_source` (`None`: the
    /// SDK-mediated case, which states none). Whether the run fails on it.
    fn mismatch_fails(edge_source: Option<&str>) -> bool {
        let row = |verdict: serde_json::Value| {
            serde_json::json!({
                "kind": "call", "service": "web", "key": "http|GET|/a", "dispatch": null,
                "method": "GET", "path": "/a", "line": 10, "col": null, "source": "fact",
                "resolution_source": null, "evidence": null,
                "counterparts": [{"role": "producer", "service": "api", "file": "h.ts", "line": 1}],
                "verdict": verdict
            })
        };
        let before = index_of(vec![row(
            serde_json::json!({"state": "resolved", "result": "compatible", "detail": ""}),
        )]);
        let rebuilt = index_of(vec![row(
            serde_json::json!({"state": "resolved", "result": "type_mismatch", "detail": "differs"}),
        )]);
        let join: super::super::LocalJoin = serde_json::from_value(serde_json::json!({
            "scanner_version": "0",
            "operations": [{
                "role": "consumer", "service": "web", "key": "http|GET|/a",
                "method": "GET", "path": "/a", "file": "client.ts", "line": 10,
                "resolution_source": null, "handler": null, "dispatch": null
            }],
            "matches": [], "unmatched": [],
            "findings": [{
                "kind": "type_mismatch", "service": "web", "method": "GET", "path": "/a",
                "call_sites": ["client.ts:10"], "detail": "differs",
                "verdict_state": null, "direction": null, "expected_type": null,
                "actual_type": null, "edge_source": edge_source
            }],
            "type_check": {"state": "ran"}
        }))
        .expect("a join");
        let edited: BTreeSet<String> = ["client.ts".to_string()].into();
        let changed: BTreeMap<String, &BTreeSet<String>> = [("web".to_string(), &edited)].into();
        let judged = judge(
            Path::new("/w"),
            &before,
            true,
            &rebuilt,
            &join,
            &changed,
            &Touched::new(),
        );
        assert_eq!(judged.problems.len(), 1, "{:?}", judged.problems);
        judged.problems[0].fails
    }

    /// An SDK-mediated mismatch states no source by design: the PR comment
    /// reads it as a break, so the local check fails on it too (one answer,
    /// `mismatch_break_status`).
    #[test]
    fn a_mismatch_stating_no_source_broken_by_the_change_fails() {
        assert!(mismatch_fails(None));
    }

    /// A source the model stated is a candidate and is reported, not enforced;
    /// a fact fails.
    #[test]
    fn a_model_stated_mismatch_does_not_fail_and_a_fact_does() {
        assert!(!mismatch_fails(Some("candidate")));
        assert!(mismatch_fails(Some("fact")));
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

    /// A call read from a request the code builds and returns is counted as
    /// its own kind, never as a model candidate (carrick#3470).
    #[test]
    fn built_requests_are_counted_as_their_own_kind() {
        let built = |path: &str| {
            let mut row = problem(ProblemKind::NoRoute, false);
            row.descriptor = true;
            row.path = path.to_string();
            row
        };
        let one = ChangedOutput {
            routes: 0,
            calls: 1,
            files: 1,
            services: 2,
            problems: vec![built("/b")],
            failing: 0,
            not_checked: None,
            ..not_checked("/w", "")
        };
        assert_eq!(
            one.render(false).lines().next().unwrap(),
            "carrick check: checked 0 routes and 1 call in 1 changed file against 2 services: \
             0 incompatible, 0 reach no route, plus 1 built request not seen sent, \
             run with --all to list them"
        );
        assert!(
            one.render(true)
                .contains("GET /b: no service in this workspace serves it [built request not seen sent, does not fail]"),
            "{}",
            one.render(true)
        );
        let mut candidate = problem(ProblemKind::NoRoute, false);
        candidate.path = "/c".to_string();
        let mixed = ChangedOutput {
            problems: vec![built("/b"), built("/d"), candidate],
            ..one
        };
        assert_eq!(
            mixed.render(false).lines().next().unwrap(),
            "carrick check: checked 0 routes and 1 call in 1 changed file against 2 services: \
             0 incompatible, 0 reach no route, plus 1 model candidate and \
             2 built requests not seen sent, run with --all to list them"
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
            rescan: None,
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
            rescan: None,
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
