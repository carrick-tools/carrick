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
//! 0 otherwise. A problem resting on a model candidate is printed and does not
//! fail the run, and neither does a check that could not run: it says why on
//! its summary line, because a test run must not fail on a missing index.

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
const DEFAULT_BUDGET: Duration = Duration::from_secs(120);

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
    /// Why nothing was checked, when nothing was. The run then exits 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_checked: Option<String>,
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

/// Run the pass and print it. Returns the exit code.
pub fn run(root: Option<&Path>, json: bool) -> i32 {
    let started = Instant::now();
    let mut output = answer(root, started);
    output.elapsed_ms = started.elapsed().as_millis() as u64;
    if json {
        match serde_json::to_string_pretty(&output) {
            Ok(text) => crate::outln!("{text}"),
            Err(e) => crate::errln!("carrick: could not serialize the answer: {e}"),
        }
    } else {
        crate::out!("{}", output.render());
    }
    if output.failing > 0 { 1 } else { 0 }
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
    }
}

fn answer(root: Option<&Path>, started: Instant) -> ChangedOutput {
    let Some(root) = super::workspace::locate(root, None) else {
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

    let (repos, changed) = changed_files(&workspace, &index);
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
    let rejoined = super::recheck::rejoin(&workspace, &rescan, &generation, started + budget());
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

    let wanted: BTreeMap<String, &BTreeSet<String>> = changed
        .iter()
        .map(|(name, (_, files))| (name.clone(), files))
        .collect();
    let judged = judge(&root, &index, &rejoined.index, &rejoined.join, &wanted);
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
        };
        let Some(indexed) = index.repos.iter().find(|indexed| indexed.name == name) else {
            entry.skipped = Some("not in the index; run `carrick refresh`".to_string());
            listed.push(entry);
            continue;
        };
        let (branch, base) = match crate::git_state::main_base(&text) {
            Ok(found) => found,
            Err(reason) => {
                entry.skipped = Some(reason);
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
        let files: BTreeSet<String> = paths
            .into_iter()
            .filter(|file| crate::file_finder::is_scanned_source(&repo.join(file), repo))
            .filter(|file| {
                indexed.services.is_empty()
                    || indexed.services.iter().any(|service| service.covers(file))
            })
            .collect();
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

struct Judged {
    routes: usize,
    calls: usize,
    problems: Vec<Problem>,
}

/// The problems in the changed files, read off the rebuilt index and the join
/// it came from.
fn judge(
    root: &Path,
    before: &LocalIndex,
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
        let fails = finding.is_some_and(|finding| {
            finding.edge_source.as_deref() == Some("fact")
                && finding.verdict_state.as_deref() != Some("unresolved")
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
            fails,
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
                if !is_changed(repo, &file) && !was_matched(before, repo, operation) {
                    continue;
                }
                let source = Source::of(operation.resolution_source);
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
                    fails: source == Source::Fact,
                });
            }
        }
    }
    judged.problems.sort();
    judged.problems.dedup();
    judged
}

/// Whether the index, before this pass, matched this call to a producer.
fn was_matched(before: &LocalIndex, repo: &str, operation: &JoinedOperation) -> bool {
    before
        .repos
        .iter()
        .find(|indexed| indexed.name == repo)
        .and_then(|indexed| indexed.files.get(&operation.file))
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.key == operation.key
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
    /// The summary line, then one line per problem.
    pub fn render(&self) -> String {
        if let Some(reason) = &self.not_checked {
            return format!("carrick check: not checked: {reason}\n");
        }
        let incompatible = self
            .problems
            .iter()
            .filter(|p| p.kind == ProblemKind::Incompatible)
            .count();
        let no_route = self.problems.len() - incompatible;
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
        let advisory = self.problems.len() - self.failing;
        if advisory > 0 {
            out.push_str(&format!(
                " ({advisory} of them rest on rows that are not facts and do not fail the check)"
            ));
        }
        out.push('\n');
        for problem in &self.problems {
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
            let note = match (problem.fails, problem.source.as_str()) {
                (true, _) => "",
                (false, "candidate") => " [model candidate, does not fail]",
                (false, _) => " [source not stated, does not fail]",
            };
            out.push_str(&format!(
                "{at}  {what}  {} {} {}{with}: {}{note}\n",
                problem.role, problem.method, problem.path, problem.reason
            ));
        }
        for repo in &self.repos {
            if let Some(reason) = &repo.skipped {
                out.push_str(&format!("not checked: {}: {reason}\n", repo.repo));
            }
        }
        out
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
            fails,
        }
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
        let text = output.render();
        let first = text.lines().next().unwrap();
        assert_eq!(
            first,
            "carrick check: checked 1 route and 2 calls in 3 changed files against 2 services: \
             1 incompatible, 1 reaches no route (1 of them rest on rows that are not facts and \
             do not fail the check)"
        );
        assert!(text.contains("web/src/a.ts:4  no route  call GET /a: no service"));
        assert!(text.contains("[model candidate, does not fail]"));
    }

    #[test]
    fn a_check_that_could_not_run_says_so_on_one_line() {
        let text = not_checked("/w", "no index").render();
        assert_eq!(text, "carrick check: not checked: no index\n");
    }

    #[test]
    fn a_compiler_message_is_one_line_and_cut() {
        assert_eq!(one_line("a\n  b\tc"), "a b c");
        assert!(one_line(&"x ".repeat(400)).chars().count() <= MAX_REASON_CHARS);
    }
}
