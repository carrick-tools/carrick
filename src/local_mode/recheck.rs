//! Re-judge one edited file against the index, before it is committed
//! (carrick#1036).
//!
//! `check` and `touch` read what the last `index` computed. That answer is the
//! wrong one the moment an agent changes a producer's response type: the
//! verdict it prints was reached before the edit, and only a full `index` —
//! minutes, cold — would judge the working tree against the consumers the
//! index already knows. This module is the narrow path between the two.
//!
//! Two phases, both of which already exist:
//!
//! 1. **The edited file's service, re-scanned.** The same subprocess `index`
//!    runs per repo, with no model and no upload: deterministic
//!    extraction plus the sidecar's type capture, over the working tree. Its one
//!    network request is a read-only ask to the library store with package
//!    names, versions, import specifiers and the TypeScript library globals the
//!    code uses, no source (`engine/mod.rs` `read_from_library_store`,
//!    `library_store.rs` `RegistryPackage`). It
//!    re-reads the edited file's own service and the services that hold an
//!    operation reading a type declared in that file ([`Reach`]); every other
//!    service stands in at its newest known version, the copy the last
//!    re-check of it left or else the index's ([`Plan`], carrick#3198). The
//!    blob the last index wrote for that repo is handed in as `previous_data`,
//!    so the hosted model answers for the files the edit did not touch replay
//!    exactly as they do on an incremental scan.
//! 2. **The join, over blobs.** Every other service comes from the blobs
//!    already on disk (`.carrick/repos`) and the cached hosted snapshot; the
//!    join reads them, runs the v2 type check, and hands back the same
//!    [`LocalJoin`] the indexer folds. Nothing is downloaded. The type
//!    checker's finished answers are kept per index under
//!    `.carrick/judged/<index generation>/`, and a re-read service's blob under
//!    `.carrick/working/<index generation>/`; both are replaced by the next
//!    index, and neither is the index itself.
//!
//! It also answers with the functions the edited file declares that the index
//! does not hold (carrick#1330). No hook reads them since the end-of-task
//! reuse nudge was removed (carrick#3436; `docs/reference/task-skills.md`,
//! "What the hooks deliver"). Those
//! come from one parse of the file before either phase starts, and are kept on
//! a miss too (carrick#2849): on a large monorepo the re-scan alone outlasts
//! the budget, and the nudge must not depend on it.
//!
//! A deadline covers both phases. On a miss the caller keeps the indexed
//! answer and says how old it is; the re-check never blocks an edit and never
//! leaves a scan running behind one.

use std::cell::RefCell;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use super::checkout::Tree;
use super::contract::{NotReread, RecheckScope};
use super::index::{build, join_command, read_blobs, repo_label, scan_command, service_id};
use super::read_model::{IndexedItem, IndexedRepo, IndexedService};
use super::workspace::Workspace;
use crate::engine::type_compat_v2::JUDGE_ANSWERS_ENV;

/// How long the whole re-check may take before the caller keeps the indexed
/// answer instead.
///
/// The post-edit hook is killed by the harness at fifteen seconds and the
/// plugin's own limit for one CLI call sits below that, so this is a budget for
/// the answer, not for the process: whatever it does not finish by, it stops
/// doing. Overridable so a test can force the degrade path with certainty.
pub const BUDGET_ENV: &str = "CARRICK_RECHECK_BUDGET_MS";
const DEFAULT_BUDGET: Duration = Duration::from_millis(10_000);

/// How much of the re-check ran, in the answer's own words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ran {
    /// Both halves: the file was re-extracted and the type check reached a
    /// verdict on at least one of the rows it declares.
    ExtractionAndTypes,
    /// The file was re-extracted and joined, and no row of it carries a type
    /// verdict: nothing here pairs with anything, or the pairs it has were not
    /// both resolved. The rows are fresh either way; which of the two it is,
    /// is in each row's own detail, because both arrive as `not_checked`.
    Extraction,
    /// Neither: the answer below is the indexed one.
    None,
}

impl Ran {
    pub fn as_str(self) -> &'static str {
        match self {
            Ran::ExtractionAndTypes => "extraction+types",
            Ran::Extraction => "extraction",
            Ran::None => "none",
        }
    }
}

/// What a re-check produced for one file.
pub struct Fresh {
    /// The rows the working tree states, in the shape the index holds them.
    pub items: Vec<IndexedItem>,
    /// The calls of the file the re-scan considered and did not state as a
    /// row, at the lines the working tree has (carrick#3688 U4).
    pub unread: Vec<super::read_model::IndexedUnread>,
    /// Functions the re-extracted file declares that the blobs the index holds
    /// do not (carrick#1330), in line order.
    pub new_functions: Vec<super::contract::NewFunction>,
    pub ran: Ran,
    /// What the type check of this re-join did (carrick#3204).
    pub type_check: super::read_model::TypeCheckSummary,
    pub elapsed: Duration,
    /// Which services were re-read and which were not.
    pub scope: RecheckScope,
    /// Rows outside the edited file that this edit moved.
    pub moved: Vec<super::contract::MovedRow>,
}

/// How many new functions one answer carries.
///
/// The consumer is one `find_similar` call, which takes twenty entries; a list
/// longer than that would be cut by the caller anyway, and cutting it here
/// keeps the cut in one place.
const MAX_NEW_FUNCTIONS: usize = 20;

/// Why a re-check did not produce one.
pub struct Degraded {
    pub reason: String,
    /// The functions the edited file declares that the index does not hold,
    /// named from a parse of the file before the re-scan started
    /// (carrick#2849). A re-scan that misses its budget leaves the verdicts
    /// at their indexed age, but what the edit added is known either way.
    pub new_functions: Vec<super::contract::NewFunction>,
    pub elapsed: Duration,
    /// The re-scan was not started, because an earlier one of the edited
    /// file's service missed its budget against the same index
    /// ([`ServiceMisses`]).
    pub known_miss: bool,
    /// The re-scan was not started because this call was asked not to start
    /// one (`check --recheck --no-rescan`): the edit needs a re-scan, and the
    /// caller will make it later, under its own budget.
    pub deferred: bool,
    /// The re-scan or re-join ran past the budget and was stopped there
    /// (carrick#3860): the answer is the indexed one because time ran out,
    /// not because nothing needed reading.
    pub deadline: bool,
    /// Which services were to be re-read, once that was decided.
    pub scope: Option<RecheckScope>,
}

/// The budget for one re-check.
pub fn budget() -> Duration {
    budget_from(std::env::var(BUDGET_ENV).ok().as_deref())
}

/// The same, from a value rather than the environment, so the default and the
/// parse are testable without a process-wide variable.
fn budget_from(raw: Option<&str>) -> Duration {
    raw.and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_BUDGET)
}

/// The services one re-check reads, decided from the index (carrick#3198).
///
/// The edited file's own service, plus every service that holds an operation
/// reading a type declared in that file (`uses[file].types[].operations`,
/// same repo only). Every other service of the repo is judged at its newest
/// known version, never re-read.
pub struct Reach {
    /// The service the file belongs to: the one a miss is remembered for.
    pub owner: String,
    /// The owner first, then the others, in index order.
    pub reread: Vec<String>,
    /// The repo's services as the index lists them.
    services: Vec<IndexedService>,
    /// Indexed files per re-read service, for the miss reason.
    files: std::collections::BTreeMap<String, usize>,
}

impl Reach {
    /// `None` when `owner` is no service of this repo: nothing reads the file.
    pub fn of(repo: &IndexedRepo, relative: &str, owner: &str) -> Option<Self> {
        repo.services.iter().find(|service| service.name == owner)?;
        let mut reread = vec![owner.to_string()];
        let reading = repo
            .uses
            .as_ref()
            .and_then(|uses| uses.get(relative))
            .into_iter()
            .flat_map(|uses| &uses.types)
            .flat_map(|used| &used.operations)
            .map(|operation| operation.service.as_str());
        for service in repo.services.iter().map(|service| service.name.as_str()) {
            let reads = reading.clone().any(|name| name == service);
            if reads && !reread.iter().any(|known| known == service) {
                reread.push(service.to_string());
            }
        }
        let files = reread
            .iter()
            .filter_map(|name| {
                let service = repo.services.iter().find(|service| &service.name == name)?;
                let count = repo
                    .files
                    .keys()
                    .filter(|file| service.covers(file))
                    .count();
                Some((name.clone(), count))
            })
            .collect();
        Some(Self {
            owner: owner.to_string(),
            reread,
            services: repo.services.clone(),
            files,
        })
    }
}

/// The environment variable that prints each phase's timing line to stderr.
/// `check` is a read command and installs no log, so the lines go to the debug
/// log when a caller installed one and to stderr when this is set; unset, a
/// re-check prints nothing more than before.
pub const TIMING_ENV: &str = "CARRICK_RECHECK_TIMING";

/// When the per-edit re-check in this process began, for the offsets in the
/// timing lines.
static RUN_STARTED: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

/// Mark `started` as the beginning of this command, for the offsets in the
/// timing lines.
pub(super) fn start_timing(started: Instant) {
    if let Ok(mut at) = RUN_STARTED.lock() {
        *at = Some(started);
    }
}

/// Say that `phase` took from `began` until now, and where in the command that
/// ended: one line per phase, `recheck-timing <phase> took <ms> ms, ended at
/// <ms> ms`. Both checks call it: the per-edit re-check and the several-file
/// `check --changed`, whose parent writes no log of its own.
pub(super) fn took(phase: &str, began: Instant) {
    let now = Instant::now();
    let at = RUN_STARTED
        .lock()
        .ok()
        .and_then(|started| *started)
        .map_or(0, |started| now.duration_since(started).as_millis());
    let line = format!(
        "recheck-timing {phase} took {} ms, ended at {at} ms",
        now.duration_since(began).as_millis()
    );
    tracing::debug!("{line}");
    if std::env::var_os(TIMING_ENV).is_some() {
        crate::errln!("carrick: {line}");
    }
}

/// Re-extract `relative` in `repo`, re-join it against every blob the index
/// already holds, and answer with the rows the working tree states.
///
/// `index_generation` names the index this runs against (its `indexed_at`):
/// a re-scan that missed its budget against it is remembered per service
/// there, and every later call for a file of the same service against the same
/// generation skips the re-scan and answers at once ([`ServiceMisses`]).
///
/// `rescan: false` is the probe: after the known-miss gate and before
/// [`Plan::make`] it returns a [`Degraded`] marked `deferred`, so it reads the
/// index and parses the one file and starts no scan, no temporary generation
/// and records no miss. A miss that holds still answers as a known miss.
pub fn run(
    workspace: &Workspace,
    tree: &Tree,
    relative: &str,
    index_generation: &str,
    reach: &Reach,
    before: &super::read_model::LocalIndex,
    rescan: bool,
) -> Result<Fresh, Box<Degraded>> {
    let started = Instant::now();
    start_timing(started);
    let budget = budget();
    let deadline = started + budget;
    // The index is read once, here, and named twice: for what the edit added
    // and for the re-join. What the edit added does not wait on the re-scan
    // (carrick#2849): on a large repo the re-scan alone outlasts the budget,
    // and the reuse nudge would then never hear of a function the edit wrote.
    let held = match Held::read(workspace) {
        Ok(held) => {
            took("held-read", started);
            held
        }
        Err(reason) => {
            return Err(Box::new(Degraded {
                reason,
                new_functions: Vec::new(),
                elapsed: started.elapsed(),
                known_miss: false,
                deferred: false,
                deadline: false,
                scope: None,
            }));
        }
    };
    let began = Instant::now();
    let added = added_functions(&held, tree, relative);
    took("added-functions", began);
    let began = Instant::now();
    let new_functions = with_leads(&held, tree, relative, added);
    took("leads", began);
    let began = Instant::now();
    let misses = ServiceMisses::path(workspace);
    let known = ServiceMisses::read(&misses);
    took("known-miss-read", began);
    if let Some(miss) = known.holds(index_generation, &reach.owner, budget) {
        return Err(Box::new(Degraded {
            reason: miss_reason(&reach.owner, miss),
            new_functions,
            elapsed: started.elapsed(),
            known_miss: true,
            deferred: false,
            deadline: false,
            scope: None,
        }));
    }
    if !rescan {
        return Err(Box::new(Degraded {
            reason: DEFERRED_REASON.to_string(),
            new_functions,
            elapsed: started.elapsed(),
            known_miss: false,
            deferred: true,
            deadline: false,
            scope: None,
        }));
    }
    let began = Instant::now();
    let plan = Plan::make(workspace, &tree.checkout, reach, index_generation, &held);
    took("plan-make", began);
    let generation = std::env::temp_dir().join(format!("carrick-recheck-{}", uuid::Uuid::new_v4()));
    let outcome = inside(
        workspace,
        held,
        (tree, relative),
        &generation,
        deadline,
        &plan,
        before,
    );
    // The temporary generation goes on every path, including the one where a
    // phase was killed at the deadline: a hook that leaves a directory behind
    // per edit is a hook that fills a disk in an afternoon.
    let began = Instant::now();
    let _ = std::fs::remove_dir_all(&generation);
    took("remove-staged-dir", began);
    match outcome {
        Ok(fresh) => Ok(Fresh {
            new_functions,
            elapsed: started.elapsed(),
            ..fresh
        }),
        Err(reason) => {
            // Only a miss of the budget is remembered: a phase that failed
            // for another reason before the deadline says nothing about
            // whether this service fits, and remembering it would switch the
            // re-check off on a service where it does. The scan does not say
            // which of the services it was reading had not finished, so only
            // the edited file's own service is recorded: a reader service
            // re-read beside it is tried again on its own next edit, where a
            // miss is its own. A lost record costs one more re-check; a wrong
            // one switches a service off for the whole index.
            let missed = Instant::now() >= deadline;
            if missed {
                ServiceMisses::record(
                    &misses,
                    index_generation,
                    &plan.miss_of(&reach.owner, started.elapsed(), budget),
                );
            }
            Err(Box::new(Degraded {
                reason,
                new_functions,
                elapsed: started.elapsed(),
                known_miss: false,
                deferred: false,
                deadline: missed,
                scope: Some(plan.scope.clone()),
            }))
        }
    }
}

/// Said when a probe found an edit that needs a re-scan. Agent-facing, though
/// a caller that asked for the probe replaces it with its own line.
const DEFERRED_REASON: &str = "this edit needs a re-scan, which this call was asked not to start";

/// The reason a re-check that skipped its re-scan gives.
fn miss_reason(service: &str, miss: &Miss) -> String {
    let took = if miss.elapsed_ms >= 1000 {
        format!("{} s", miss.elapsed_ms / 1000)
    } else {
        format!("{} ms", miss.elapsed_ms)
    };
    format!(
        "re-reading {service} ({} indexed files) took longer than {took} against this index, so \
         edits in {service} are not re-checked until `carrick refresh`. Run `carrick check \
         --changed` before you finish.",
        miss.files
    )
}

/// One service's re-read that missed its budget.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
struct Miss {
    /// How long the re-check ran before it was stopped.
    elapsed_ms: u64,
    /// The budget the re-check ran under. A caller with a longer budget than
    /// this one's is not told the service does not fit: it may. No default:
    /// a file written before this field existed fails to parse and reads as no
    /// misses (carrick#3198).
    budget_ms: u64,
    /// Indexed files of the service.
    files: usize,
}

/// The services whose re-check missed its budget against one index generation
/// (`.carrick/recheck-misses.json`).
///
/// On a large service the re-scan alone outlasts the budget every time, so
/// every edit would wait out the whole budget for an answer that never comes.
/// The first miss is remembered here, keyed to the index it ran against; until
/// `index` or `refresh` writes a new one, a re-check of a file of that service
/// skips the re-scan and answers at once with the parse and the indexed
/// verdicts. A miss in one service leaves every other service of the repo
/// re-checked. A new generation does not match, so the re-check is tried again
/// after it. A service that fits never misses and is never written here. The
/// reader-facing side (`known_miss` on the `recheck` block) is
/// `docs/local-mode-output.md`.
#[derive(serde::Serialize, serde::Deserialize, Default, Debug, PartialEq, Eq)]
struct ServiceMisses {
    /// The `indexed_at` of the index the misses were against.
    generation: String,
    /// Service ids, as the index names them.
    services: std::collections::BTreeMap<String, Miss>,
}

impl ServiceMisses {
    fn path(workspace: &Workspace) -> std::path::PathBuf {
        workspace.index_dir().join("recheck-misses.json")
    }

    /// Unreadable, absent or written in another shape is "no misses": a lost
    /// record costs one more re-check, never a wrong answer.
    fn read(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// The miss that silences a re-check under `budget`: one recorded under
    /// a budget at least as long. A miss at 10 s says nothing about a call
    /// that has 180 s.
    fn holds(&self, generation: &str, service: &str, budget: Duration) -> Option<&Miss> {
        if generation.is_empty() || self.generation != generation {
            return None;
        }
        self.services
            .get(service)
            .filter(|miss| u128::from(miss.budget_ms) >= budget.as_millis())
    }

    /// Add `misses` for `generation`, dropping the misses of any older one.
    fn record(path: &Path, generation: &str, misses: &[(String, Miss)]) {
        if generation.is_empty() {
            return;
        }
        let mut held = Self::read(path);
        if held.generation != generation {
            held = Self {
                generation: generation.to_string(),
                services: Default::default(),
            };
        }
        for (service, miss) in misses {
            held.services.insert(service.clone(), miss.clone());
        }
        if let Ok(text) = serde_json::to_string_pretty(&held) {
            let _ = std::fs::write(path, text);
        }
    }
}

/// The folder a kind of kept state (`judged` or `working`) uses for one index
/// generation: `.carrick/<kind>/<generation>/`. `None` for an index with no
/// generation to key it by.
pub(super) fn kept_dir(
    workspace: &Workspace,
    kind: &str,
    generation: &str,
) -> Option<std::path::PathBuf> {
    if generation.is_empty() {
        return None;
    }
    let name: String = generation
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    Some(kept_dir_of(
        &workspace.index_dir(),
        kind,
        &name,
        &build_id(),
    ))
}

/// `.carrick/<kind>/<generation>.<build>/`. Kept state is only ever read by
/// the build that wrote it: a different scanner build builds different pairs
/// and rows from the same index.
fn kept_dir_of(index_dir: &Path, kind: &str, generation: &str, build: &str) -> std::path::PathBuf {
    index_dir.join(kind).join(format!("{generation}.{build}"))
}

/// The running binary's identity for kept state: its version, its stamped
/// commit and whether the tree was dirty. An unstamped (debug) build says so.
fn build_id() -> String {
    let stamp = crate::cloud_storage::ScannerBuild::current().map_or_else(
        || "unstamped".to_string(),
        |build| {
            format!(
                "{}{}",
                build.commit,
                if build.dirty { "-dirty" } else { "" }
            )
        },
    );
    format!("{}-{stamp}", env!("CARGO_PKG_VERSION"))
}

/// Remove every generation of `kind` except `keep`: a new index replaces what
/// the old one left.
fn prune_other_generations(kind_dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(kind_dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        if entry.path() != keep {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Put the type-check answers an index build kept in `built` where the
/// re-check finds them, replacing whatever an older index left, and clear the
/// older index's re-read services.
pub(super) fn adopt_index_state(workspace: &Workspace, built: &Path, generation: &str) {
    let Some(judged) = kept_dir(workspace, "judged", generation) else {
        return;
    };
    if let Some(parent) = judged.parent() {
        let _ = std::fs::create_dir_all(parent);
        prune_other_generations(parent, &judged);
    }
    if built.is_dir() {
        let _ = std::fs::remove_dir_all(&judged);
        let _ = std::fs::rename(built, &judged);
    }
    let _ = std::fs::remove_dir_all(workspace.index_dir().join("working"));
}

/// What one re-check decided before it started: which services it re-reads
/// and which blob stands in for each of the others.
struct Plan {
    generation: String,
    services: Vec<String>,
    /// Digest of each re-read service's edits since the index; absent where
    /// the service has none.
    digests: std::collections::BTreeMap<String, String>,
    /// The copy a service's last re-check left, for each service outside the
    /// scope whose edits since then are exactly the ones it was read for.
    working: std::collections::BTreeMap<String, crate::cloud_storage::CloudRepoData>,
    files: std::collections::BTreeMap<String, usize>,
    scope: RecheckScope,
}

/// A re-read service's blob, kept with the digest of the edits it was read
/// for. One file, written whole, so the pair is never torn.
#[derive(serde::Serialize, serde::Deserialize)]
struct WorkingCopy {
    digest: String,
    blob: crate::cloud_storage::CloudRepoData,
}

impl Plan {
    fn make(
        workspace: &Workspace,
        repo: &Path,
        reach: &Reach,
        generation: &str,
        held: &Held,
    ) -> Self {
        let began = Instant::now();
        let mut readers = readers_of_the_edit(repo, reach, held);
        took("plan-make/readers-of-the-edit", began);
        let began = Instant::now();
        let mut listings: std::collections::BTreeMap<String, Option<HashSet<String>>> =
            Default::default();
        let mut digests = std::collections::BTreeMap::new();
        let mut working = std::collections::BTreeMap::new();
        let mut scope = RecheckScope {
            reread: reach.reread.clone(),
            not_reread: Vec::new(),
            stale: Vec::new(),
        };
        for service in &reach.services {
            let changed = listings
                .entry(service.commit.clone())
                .or_insert_with(|| super::query::changed_since(repo, &service.commit));
            // `None`: git cannot say what changed. `Some(None)`: nothing did.
            let digest = changed
                .as_ref()
                .map(|changed| service_digest(repo, changed, service, &reach.services));
            if reach.reread.contains(&service.name) {
                if let Some(Some(digest)) = digest {
                    digests.insert(service.name.clone(), digest);
                }
                continue;
            }
            let operations = service.routes + service.calls;
            // A service that reads a path edited since its index is judged at
            // a version that predates the edit, whatever else is known of it:
            // its own files' digest does not cover the import.
            let reader = readers.remove(&service.name);
            let reason = match digest {
                _ if reader.is_some() => {
                    if let Some(Some(digest)) = digest
                        && let Some(blob) =
                            read_working(workspace, generation, &service.name, &digest)
                    {
                        working.insert(service.name.clone(), blob);
                    }
                    scope.stale.push(service.name.clone());
                    reader.as_deref().unwrap_or_default()
                }
                Some(None) => "no edits since the index; judged at its indexed version",
                Some(Some(digest)) => {
                    match read_working(workspace, generation, &service.name, &digest) {
                        Some(blob) => {
                            working.insert(service.name.clone(), blob);
                            "judged at your last re-check of it"
                        }
                        None => {
                            scope.stale.push(service.name.clone());
                            "edited since the index and not re-checked; judged against its indexed version"
                        }
                    }
                }
                None => {
                    scope.stale.push(service.name.clone());
                    "git could not say what changed in it; judged against its indexed version"
                }
            };
            scope.not_reread.push(NotReread {
                service: service.name.clone(),
                operations,
                reason: reason.to_string(),
            });
        }
        took("plan-make/digests-and-working-reads", began);
        Self {
            generation: generation.to_string(),
            services: reach.reread.clone(),
            digests,
            working,
            files: reach.files.clone(),
            scope,
        }
    }

    /// The miss to remember when this re-check ran out of time: the edited
    /// file's own service only, never a reader service re-read beside it.
    fn miss_of(&self, owner: &str, elapsed: Duration, budget: Duration) -> Vec<(String, Miss)> {
        self.services
            .iter()
            .filter(|service| service.as_str() == owner)
            .map(|service| {
                (
                    service.clone(),
                    Miss {
                        elapsed_ms: elapsed.as_millis() as u64,
                        budget_ms: budget.as_millis() as u64,
                        files: self.files.get(service).copied().unwrap_or(0),
                    },
                )
            })
            .collect()
    }

    /// Keep a re-read service's fresh blob for the next re-check. A service
    /// with no edits since the index has nothing to keep: its indexed blob is
    /// the newest there is.
    fn keep(&self, workspace: &Workspace, blob: &crate::cloud_storage::CloudRepoData) {
        let id = service_id(blob);
        let Some(digest) = self.digests.get(&id) else {
            return;
        };
        let Some(dir) = kept_dir(workspace, "working", &self.generation) else {
            return;
        };
        if let Some(parent) = dir.parent() {
            prune_other_generations(parent, &dir);
        }
        let copy = WorkingCopy {
            digest: digest.clone(),
            blob: blob.clone(),
        };
        let Ok(bytes) = serde_json::to_vec(&copy) else {
            return;
        };
        let kept = dir.join(working_name(&id));
        let staged = dir.join(format!("{}.{}.tmp", working_name(&id), std::process::id()));
        let _ = std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&staged, bytes))
            .and_then(|()| std::fs::rename(&staged, &kept));
    }
}

/// The file name a service's re-read copy is kept under.
fn working_name(service: &str) -> String {
    let safe: String = service
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{safe}.json")
}

/// The copy `service`'s last re-check left, when it was read for exactly the
/// edits `digest` names.
fn read_working(
    workspace: &Workspace,
    generation: &str,
    service: &str,
    digest: &str,
) -> Option<crate::cloud_storage::CloudRepoData> {
    let path = kept_dir(workspace, "working", generation)?.join(working_name(service));
    let copy: WorkingCopy = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (copy.digest == digest && service_id(&copy.blob) == service).then_some(copy.blob)
}

/// The reason for each service outside the re-read scope that reads a path
/// edited since its index, by service name.
///
/// The type-use record lists the operations that read a type the edited file
/// declares; it does not list one that reads a constant, a wrapper or a type
/// it could not trace. The import closure the change scope already walks
/// ([`crate::change_scope::untouched_since`]) names those. A service's own
/// edits are left out of the listing: the digest already accounts for them
/// (its last re-check's copy, or stale). A service whose closure cannot be
/// walked (no stored config on this machine) is named too: it is not known to
/// be untouched.
fn readers_of_the_edit(
    repo: &Path,
    reach: &Reach,
    held: &Held,
) -> std::collections::BTreeMap<String, String> {
    let repo_path = repo.to_string_lossy().to_string();
    let mut readers = std::collections::BTreeMap::new();
    // One listing of what changed, shared by every service's view of it.
    let listing = crate::change_scope::ChangedSince::new(&repo_path, &[], &[]);
    for service in reach
        .services
        .iter()
        .filter(|service| !reach.reread.contains(&service.name))
    {
        let config = held
            .local
            .iter()
            .find(|blob| service_id(blob) == service.name)
            .and_then(|blob| blob.config_json.as_deref())
            .and_then(|json| serde_json::from_str::<crate::config::Config>(json).ok());
        let Some(config) = config else {
            readers.insert(
                service.name.clone(),
                "its config is not on this machine, so what it reads is unknown; judged at its indexed version"
                    .to_string(),
            );
            continue;
        };
        let own = service.clone();
        let changes = listing.view(move |path| own.covers(path));
        let read = crate::change_scope::untouched_since(
            &repo_path,
            std::slice::from_ref(&config),
            &changes,
            &service.commit,
        );
        if let Some(Some(path)) = read.first() {
            readers.insert(
                service.name.clone(),
                format!("reads {path}, edited since the index; judged at its indexed version"),
            );
        }
    }
    readers
}

/// Whether no service of the repo reads any of `paths`, by the rule that
/// carries a service under `--changed-only`
/// ([`crate::change_scope::untouched_since`]): an edited test file that still
/// loads what it loaded, a file outside every service's roots and import
/// closure. Such an edit changes no row, so the index's answer stands and no
/// re-scan is owed (the scan decides what it reads, not a second classifier).
///
/// `false` whenever that rule cannot say: the repo's services are not all at
/// one commit and clean, a service's config is not on this machine, or the
/// workspace cannot be read. Then the caller takes the path it always took.
pub(super) fn no_service_reads(
    workspace_root: &Path,
    tree: &Tree,
    indexed: &IndexedRepo,
    paths: &HashSet<String>,
) -> bool {
    let Some(commit) = indexed.services.first().map(|service| &service.commit) else {
        return false;
    };
    if commit.is_empty()
        || indexed
            .services
            .iter()
            .any(|service| &service.commit != commit || service.dirty)
    {
        return false;
    }
    let Ok(workspace) = Workspace::load(workspace_root) else {
        return false;
    };
    let Ok(local) = read_blobs(&workspace.blobs_dir()) else {
        return false;
    };
    let name = repo_label(&tree.repo);
    let hosted = super::hosted::cached(&workspace).local_blobs(&tree.repo);
    let blobs: Vec<&crate::cloud_storage::CloudRepoData> = local
        .iter()
        .filter(|blob| blob.repo_name == name)
        .chain(hosted.iter())
        .collect();
    let mut configs = Vec::new();
    for service in &indexed.services {
        let config = blobs
            .iter()
            .find(|blob| service_id(blob) == service.name)
            .and_then(|blob| blob.config_json.as_deref())
            .and_then(|json| serde_json::from_str::<crate::config::Config>(json).ok());
        let Some(config) = config else {
            return false;
        };
        configs.push(config);
    }
    let repo_path = tree.checkout.to_string_lossy().to_string();
    let listing = crate::change_scope::ChangedSince::new(&repo_path, &[], &[]);
    let kept = paths.clone();
    let only = listing.view(move |path| !kept.contains(path));
    crate::change_scope::untouched_since(&repo_path, &configs, &only, commit)
        .iter()
        .all(Option::is_none)
}

/// What the working tree holds, since the index, of the files `service` reads:
/// a hash of each changed file's path and bytes. A change no service covers is
/// in every service's digest only when it is a repo input
/// ([`crate::change_scope::is_repo_input`]: a manifest, a lockfile,
/// `carrick.json`, a GraphQL schema). A root file a service reads by name (its
/// tsconfig chain) is named by [`readers_of_the_edit`], which walks that
/// service's inputs, and markdown, skill files and hook config at the root are
/// read by no service (carrick#3465). `None` when none of its files changed.
fn service_digest(
    repo: &Path,
    changed: &HashSet<String>,
    service: &IndexedService,
    all: &[IndexedService],
) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut files: Vec<&String> = changed
        .iter()
        .filter(|file| {
            service.covers(file)
                || (crate::change_scope::is_repo_input(file)
                    && !all.iter().any(|other| other.covers(file)))
        })
        .collect();
    if files.is_empty() {
        return None;
    }
    files.sort();
    let mut hash = Sha256::new();
    for file in files {
        hash.update((file.len() as u64).to_le_bytes());
        hash.update(file.as_bytes());
        match std::fs::read(repo.join(file)) {
            Ok(bytes) => {
                hash.update([1u8]);
                hash.update((bytes.len() as u64).to_le_bytes());
                hash.update(bytes);
            }
            // A deleted file is part of the edit.
            Err(_) => hash.update([0u8]),
        }
    }
    Some(format!("{:x}", hash.finalize()))
}

/// What the last `index` left on disk, read once per re-check: the local
/// blobs (`.carrick/repos`) and the cached hosted snapshot. Nothing is
/// downloaded: a re-check that waited on the network would miss its budget on
/// the wire rather than on the work.
struct Held {
    local: Vec<crate::cloud_storage::CloudRepoData>,
    hosted: super::hosted::HostedInput,
}

impl Held {
    fn read(workspace: &Workspace) -> Result<Self, String> {
        Ok(Self {
            local: read_blobs(&workspace.blobs_dir())?,
            hosted: super::hosted::cached(workspace),
        })
    }
}

/// The functions `relative` declares in the working tree that the index does
/// not hold for it, from one parse of the file (carrick#2849).
///
/// The parse is the extractor discovery runs ([`crate::pr_functions::stated_in`]),
/// so a name here is spelled as the index spells it. What the index holds is
/// BOTH records of this repo, because neither is present on every install:
/// `index` keeps only the blobs of services it scanned locally in
/// `.carrick/repos` (the write there filters out every service the hosted
/// snapshot answers for), and the hosted half is empty on a machine that is
/// not signed in. A union is also the truer reading of "the index": the hosted
/// blob is this repo on its default branch, which is what a nudge built on
/// this tells the agent it was compared against. Either one alone would
/// report every function in an edited file as new on the install the other
/// covers.
fn added_functions(held: &Held, tree: &Tree, relative: &str) -> Vec<Added> {
    let name = repo_label(&tree.repo);
    let hosted = held.hosted.local_blobs(&tree.repo);
    let repo = tree.checkout.as_path();
    let blobs: Vec<crate::cloud_storage::CloudRepoData> = held
        .local
        .iter()
        .filter(|blob| blob.repo_name == name)
        .chain(hosted.iter())
        .cloned()
        .collect();
    // The services the scan would resolve for this repo, the way `index`
    // resolves them. A repo they cannot be resolved for names nothing: the
    // question "would the scan read this file" has no answer.
    let Ok(services) = crate::service_derivation::resolve_with_hosted(repo, Some(&blobs)) else {
        return Vec::new();
    };
    let indexed = functions_in(blobs.iter(), relative);
    additions(&services, repo, relative, &indexed)
}

/// One function the edit added, with what the lead is looked up by.
type Added = (super::contract::NewFunction, super::lead::Query);

/// Attach to each added function the indexed function it looks like
/// ([`super::lead`]), read from every blob `held` has, in every repo. The
/// index is built once per call and only when the edit added a function: an
/// edit that adds none pays nothing here.
fn with_leads(
    held: &Held,
    tree: &Tree,
    relative: &str,
    added: Vec<Added>,
) -> Vec<super::contract::NewFunction> {
    if added.is_empty() {
        return Vec::new();
    }
    let index = super::lead::LeadIndex::build(
        held.local
            .iter()
            .map(|blob| (blob.repo_name.clone(), blob))
            .chain(held.hosted.every_blob()),
    );
    let repo = repo_label(&tree.repo);
    added
        .into_iter()
        .map(|(function, query)| super::contract::NewFunction {
            lead: index.lead(&query, &repo, relative),
            ..function
        })
        .collect()
}

/// What an edit to `relative` adds that the index would hold.
///
/// Only a file a service's walk reads counts
/// ([`crate::file_finder::service_would_read`], the scan's own include and
/// exclude decision, which skips test sources): the index holds nothing for
/// any other file, so every function in one would read as new. Within such a
/// file, only a function someone named counts: the callbacks the extractor
/// names for itself (`it_handler`) are indexed but cannot be imported, so
/// they are not reuse candidates.
///
/// Each comes with the lookup the lead reads, from the same one parse: the
/// definition's tokens and written return type, and the names its body calls
/// (the call graph has not filled `calls` on a single parse).
fn additions(
    services: &[crate::config::Config],
    repo: &Path,
    relative: &str,
    indexed: &std::collections::BTreeSet<String>,
) -> Vec<Added> {
    let path = repo.join(relative);
    let skip = crate::packages::MANIFEST_SKIP_DIRS;
    let repo_path = repo.to_string_lossy();
    if !services
        .iter()
        .any(|service| crate::file_finder::service_would_read(&repo_path, service, &skip, &path))
    {
        return Vec::new();
    }
    // A file that cannot be read (deleted by the edit, or not script) states
    // no functions, as it states no rows.
    let Some(source) = crate::parser::read_source(&path) else {
        return Vec::new();
    };
    let Some(extracted) =
        crate::pr_functions::extracted_in(Path::new(relative), relative.to_string(), source)
    else {
        return Vec::new();
    };
    let found = new_functions(
        indexed,
        extracted
            .stated()
            .into_values()
            .filter(|function| !function.anonymous),
    );
    let extractor = &extracted.extractor;
    found
        .into_iter()
        .map(|function| {
            // The stated name came from this parse, so its definition is
            // here; a lookup without one has nothing to score and finds none.
            let query = extractor
                .function_definitions
                .iter()
                .find(|(_, definition)| definition.name == function.name)
                .map_or_else(
                    || super::lead::Query::named(&function.name),
                    |(key, definition)| {
                        let callees = extractor
                            .callee_refs
                            .get(key)
                            .into_iter()
                            .flatten()
                            .map(|callee| callee.name.clone())
                            .collect();
                        super::lead::Query::of(definition, callees)
                    },
                );
            (function, query)
        })
        .collect()
}

fn inside(
    workspace: &Workspace,
    held: Held,
    (tree, relative): (&Tree, &str),
    generation: &Path,
    deadline: Instant,
    plan: &Plan,
    before: &super::read_model::LocalIndex,
) -> Result<Fresh, String> {
    let name = repo_label(&tree.repo);
    let rejoined = rejoin_held(
        workspace,
        held,
        std::slice::from_ref(tree),
        generation,
        deadline,
        None,
        Some(plan),
        &plan.generation,
    )?;
    // A re-scan that could not replay the index's model answers has lost every
    // row only the model stated: not a pass, as in the plain check's `answer`
    // (carrick#3413).
    let lost = super::changed::answers_lost(&rejoined.index, std::iter::once(&name));
    if !lost.is_empty() {
        return Err(lost.join(" "));
    }
    if let Some(sentence) = library_unavailable(&rejoined.index, [&name]) {
        return Err(sentence.to_string());
    }

    let mut items = rejoined
        .index
        .repos
        .iter()
        .find(|rebuilt| rebuilt.name == name)
        .and_then(|rebuilt| rebuilt.files.get(relative))
        .cloned()
        .unwrap_or_default();
    let unread = rejoined
        .index
        .repos
        .iter()
        .find(|rebuilt| rebuilt.name == name)
        .and_then(|rebuilt| rebuilt.unread.get(relative))
        .cloned()
        .unwrap_or_default();
    let (moved, no_route) = judge_edit(workspace, before, &rejoined, tree, relative, plan);
    state_no_route(&mut items, &no_route);
    // No rows where the index held some is an answer, not a failure: the edit
    // took the last route out of this file, and the fresh (empty) list is what
    // says so.
    let ran = if items.iter().any(states_a_type_verdict) {
        Ran::ExtractionAndTypes
    } else {
        Ran::Extraction
    };
    Ok(Fresh {
        items,
        unread,
        // Filled in by `run`, from the parse that did not wait on this.
        new_functions: Vec::new(),
        ran,
        type_check: super::index::type_check_summary(
            &rejoined.join,
            &[],
            &std::collections::BTreeMap::new(),
        ),
        elapsed: Duration::default(),
        scope: plan.scope.clone(),
        moved,
    })
}

/// Say, on the edited file's own call rows, that no service serves them: the
/// rows `check --changed` lists for this file without `--all`. The rebuilt
/// index stores no verdict for a call with no counterpart, so the item would
/// read `result: null`, "nothing pairs", and the answer `judge` computed
/// would be dropped. The shape is `producer_removed`'s: `not_checked` for the
/// type layer, `no_route` for the routing fact.
fn state_no_route(items: &mut [IndexedItem], no_route: &[super::changed::Problem]) {
    for problem in no_route {
        for item in items.iter_mut().filter(|item| {
            item.kind == super::read_model::ItemKind::Call
                && item.verdict.is_none()
                && item.line == problem.line
                && item.method == problem.method
                && item.path == problem.path
        }) {
            let note = if problem.other_build {
                " (not checked: the index is from another scanner build)"
            } else {
                ""
            };
            item.verdict = Some(super::read_model::StoredVerdict {
                state: "not_checked".to_string(),
                result: Some("no_route".to_string()),
                detail: format!("{}{note}", problem.reason),
            });
        }
    }
}

/// The rows outside the edited file that this edit moved, by the rule
/// `check --changed` applies to the files it was given (`changed.rs`,
/// `judge`; carrick#2998): a pair the index held compatible that now
/// mismatches, and a call the index matched that now reaches no route. A
/// pair the index never judged is listed with `not_judged_before` and is not
/// a break.
///
/// Rows of the edited repo and of every repo holding a re-read service are
/// read; a call in any repo counts when the index matched it. The rows of the
/// edited file itself are the `items` of the answer, so they are left out of
/// the moved list; its calls that reach no route and that a plain
/// `check --changed` lists come back apart, for [`state_no_route`].
fn judge_edit(
    workspace: &Workspace,
    before: &super::read_model::LocalIndex,
    rejoined: &Rejoined,
    tree: &Tree,
    relative: &str,
    plan: &Plan,
) -> (Vec<super::contract::MovedRow>, Vec<super::changed::Problem>) {
    let edited = repo_label(&tree.repo);
    let mut changed: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    changed.insert(edited, std::iter::once(relative.to_string()).collect());
    for indexed in &rejoined.index.repos {
        if indexed
            .services
            .iter()
            .any(|service| plan.scope.reread.contains(&service.name))
        {
            changed.entry(indexed.name.clone()).or_default();
        }
    }
    let wanted: std::collections::BTreeMap<String, &std::collections::BTreeSet<String>> = changed
        .iter()
        .map(|(name, files)| (name.clone(), files))
        .collect();
    let judged = super::changed::judge(
        &workspace.root,
        before,
        before.written_by_this_build(),
        &rejoined.index,
        &rejoined.join,
        &wanted,
        &super::changed::touched_since_index(before, std::slice::from_ref(tree)),
    );
    let (outside, inside): (Vec<_>, Vec<_>) = judged
        .problems
        .into_iter()
        .partition(|problem| problem.outside_the_change);
    let no_route = inside
        .into_iter()
        .filter(|problem| {
            problem.kind == super::changed::ProblemKind::NoRoute && problem.listed_without_all()
        })
        .collect();
    let moved = outside
        .into_iter()
        .map(|problem| super::contract::MovedRow {
            file: problem.file,
            line: problem.line,
            role: problem.role,
            method: problem.method,
            path: problem.path,
            before: match (problem.kind, problem.not_judged_before) {
                (super::changed::ProblemKind::NoRoute, _) => "matched",
                (_, true) => "not_judged",
                (_, false) => "compatible",
            }
            .to_string(),
            after: problem.after,
            counterparts: problem.other,
            reason: problem.reason,
            not_judged_before: problem.not_judged_before,
            other_build: problem.other_build,
        })
        .collect();
    (moved, no_route)
}

/// What a check says when the re-scan could not read the library store
/// (carrick#3249). The scan then holds no row a library package states, so a
/// route registered through a server package does not exist locally: a pass
/// would read "checked 0 routes", or fail every caller of the missing routes
/// as having no route. The first sentence names the sign-in command
/// `carrick --help` documents; the second is for a store that could not be
/// reached (network error, timeout, 5xx). Nothing a signed-in, online run
/// prints changes, and the CI runner never reaches this: it authenticates with
/// OIDC and does not run a local-mode check.
pub const LIBRARY_NOT_SIGNED_IN: &str =
    "library facts unavailable: this machine is not signed in. Run carrick login.";
pub const LIBRARY_UNREACHABLE: &str =
    "library facts unavailable: Carrick's library store could not be reached.";

/// What a re-scan of `repos` could not read of the library store, as the
/// sentence a check prints. A not-signed-in mark counts only while this
/// machine still holds no credential: a service carried from an earlier
/// unsigned `refresh` keeps its mark until the next refresh, and must not
/// outlive the sign-in.
pub(super) fn library_unavailable<'a>(
    rebuilt: &super::read_model::LocalIndex,
    repos: impl IntoIterator<Item = &'a String>,
) -> Option<&'static str> {
    use crate::library_store::AskFailure;
    let repos: std::collections::BTreeSet<&String> = repos.into_iter().collect();
    let failures: Vec<AskFailure> = rebuilt
        .repos
        .iter()
        .filter(|repo| repos.contains(&repo.name))
        .flat_map(|repo| repo.services.iter())
        .filter_map(|service| service.boundary.as_ref()?.library_ask_failed)
        .collect();
    if failures.contains(&AskFailure::NotSignedIn)
        && crate::credentials::CloudAuth::detect().is_err()
    {
        Some(LIBRARY_NOT_SIGNED_IN)
    } else if failures.contains(&AskFailure::Unreachable) {
        Some(LIBRARY_UNREACHABLE)
    } else {
        None
    }
}

/// What one re-extraction of some repos produced, joined against everything
/// else the index already holds.
pub(super) struct Rejoined {
    /// The read model the indexer would write for this working tree.
    pub index: super::read_model::LocalIndex,
    /// The join it was folded from, for a reader that needs what the read
    /// model does not carry (an edge's source, the calls that reach no route).
    pub join: super::LocalJoin,
    /// How many distinct services the join read, re-scanned and retained.
    pub services: usize,
}

/// Re-scan each of `repos` over its working tree, and join the result against
/// every other blob the index holds: one scan per repo and ONE join, however
/// many files changed. `check --recheck` asks for one repo; `check --changed`
/// asks for every repo with a changed file.
///
/// Two phases, both bounded by `deadline`:
///
/// 1. Each repo re-scanned, with no model, no upload and no network. The blob
///    the last index wrote for it is handed in as `previous_data`, so the
///    hosted answers for files the edit did not touch replay as they do on an
///    incremental scan.
/// 2. The join over every blob, re-scanned and retained, with the type check.
///
/// `narrator` is where the pass says what it is doing while it does it: the
/// several-file check runs for minutes on a large repo, and a caller that kills
/// a command after some seconds of silence read the silence as a hang. The
/// per-edit re-check passes `None`: it answers inside a hook's budget and a
/// line on its stderr has no reader.
pub(super) fn rejoin(
    workspace: &Workspace,
    repos: &[Tree],
    generation: &Path,
    deadline: Instant,
    narrator: Option<&Narrator>,
    index_generation: &str,
) -> Result<Rejoined, String> {
    let began = Instant::now();
    let held = Held::read(workspace)?;
    took("held-read", began);
    rejoin_held(
        workspace,
        held,
        repos,
        generation,
        deadline,
        narrator,
        None,
        index_generation,
    )
}

/// [`rejoin`], over an index already read. Both checks keep the type checker's
/// answers per index and build (`index_generation`, the index's `indexed_at`),
/// so a run that moves no type finds the index's own answer, or the last run's,
/// in `.carrick/judged`.
///
/// `plan` is the per-edit re-check's: it narrows the re-scan to the services
/// it names, stands the newest known copy in for the rest, keeps what the
/// scan read, and keeps the type checker's answers per index. `None` is the
/// several-file check, which re-reads whatever the change scope reaches and
/// keeps nothing.
#[allow(clippy::too_many_arguments)]
fn rejoin_held(
    workspace: &Workspace,
    held: Held,
    repos: &[Tree],
    generation: &Path,
    deadline: Instant,
    narrator: Option<&Narrator>,
    plan: Option<&Plan>,
    index_generation: &str,
) -> Result<Rejoined, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("no carrick binary to re-scan with: {e}"))?;
    let began = Instant::now();
    let blobs = generation.join("repos");
    // The re-scans and the re-join judge the same stubs and pairs; the folder
    // is where the first answer waits for the others.
    let judged = kept_dir(workspace, "judged", index_generation)
        .unwrap_or_else(|| generation.join("judged"));
    std::fs::create_dir_all(&blobs).map_err(|e| format!("{}: {e}", blobs.display()))?;
    let names: std::collections::BTreeSet<String> =
        repos.iter().map(|tree| repo_label(&tree.repo)).collect();
    let mut services = std::collections::BTreeSet::new();

    // Everything except the re-scanned repos, exactly as the last index left it.
    let Held {
        local: retained,
        hosted,
    } = held;
    let mut indexed = Vec::new();
    for (position, blob) in retained.into_iter().enumerate() {
        if names.contains(&blob.repo_name) {
            indexed.push(blob);
            continue;
        }
        services.insert(service_id(&blob));
        write_blob(&blobs.join(format!("retained-{position}.json")), &blob)?;
    }
    // The hosted services, from the snapshot on disk.
    let mut remote_services = std::collections::BTreeMap::new();
    for (position, (remote, blob)) in hosted.remote_blobs().into_iter().enumerate() {
        services.insert(service_id(&blob));
        remote_services.insert(service_id(&blob), remote);
        write_blob(&blobs.join(format!("hosted-{position}.json")), &blob)?;
    }
    took("stage/retained-and-hosted-blobs", began);
    let began = Instant::now();

    // Phase 1, one scan per repo, analysing only the services whose inputs
    // changed (`--changed-only`, carrick#2826). `previous` is what the last
    // index held for that repo: the hosted snapshot's copy of each service,
    // and the local blob of every service the snapshot does not hold. It is
    // what an unchanged file replays its hosted answers from, and what the
    // change scope carries an untouched service from.
    //
    // Every repo's `previous` is staged on disk before the first scan starts,
    // so the hosted snapshot and the retained blobs are not held for the
    // minutes the children run (carrick#2948): each repo reads back only its
    // own pair once its scan has finished.
    for (position, tree) in repos.iter().enumerate() {
        let name = repo_label(&tree.repo);
        let mut previous_data = hosted.local_blobs(&tree.repo);
        write_json(&staged(generation, "hosted", position), &previous_data)?;
        let hosted_ids: std::collections::BTreeSet<String> =
            previous_data.iter().map(service_id).collect();
        let local: Vec<&crate::cloud_storage::CloudRepoData> = indexed
            .iter()
            .filter(|blob| blob.repo_name == name)
            .collect();
        previous_data.extend(
            local
                .iter()
                .filter(|blob| !hosted_ids.contains(&service_id(blob)))
                .map(|blob| (*blob).clone()),
        );
        // A service outside the scope stands in at the copy its last re-check
        // left, so this one judges against the edits already made to it.
        if let Some(plan) = plan {
            for blob in &mut previous_data {
                if let Some(newer) = plan.working.get(&service_id(blob)) {
                    *blob = newer.clone();
                }
            }
        }
        write_json(&staged(generation, "previous", position), &previous_data)?;
    }
    drop(hosted);
    drop(indexed);
    took("stage/hosted-and-previous-lists", began);

    for (position, tree) in repos.iter().enumerate() {
        let name = repo_label(&tree.repo);
        let previous = staged(generation, "previous", position);
        let scan_dir = generation.join(format!("scan-{position}"));
        let mut scan = rescan_command(&exe, tree, &scan_dir, &previous);
        scan.env(JUDGE_ANSWERS_ENV, &judged);
        if let Some(plan) = plan {
            scan.env(
                crate::change_scope::RECHECK_SERVICES_ENV,
                serde_json::to_string(&plan.services).map_err(|e| e.to_string())?,
            );
        }
        // The rows the last `refresh` (or an earlier check, below) kept stand
        // in for a walk of a carried service. The scan only reads them; the
        // walks it had to do are kept after it succeeds.
        let kept = workspace
            .index_dir()
            .join(crate::position_rows::POSITIONS_DIR);
        scan.env(crate::position_rows::KEPT_DIR_ENV, &kept);
        if let Some(narrator) = narrator {
            // The scan states its per-service counts on stderr only when a
            // parent asks; the narrator is that parent here.
            scan.env(crate::progress::PROGRESS_ENV, "1");
            narrator.repo(Instant::now(), &name, position + 1, repos.len());
        }
        took("scan-starts", Instant::now());
        let began = Instant::now();
        bounded(scan, "re-scan", deadline, narrator)?;
        took("scan", began);
        let began = Instant::now();
        let fresh = read_blobs(&scan_dir)?;
        if let Some(plan) = plan {
            for blob in &fresh {
                plan.keep(workspace, blob);
            }
        }
        took("after-scan/read-fresh-and-keep", began);
        let began = Instant::now();
        let hosted_data = read_staged(&staged(generation, "hosted", position))?;
        let previous_data = read_staged(&previous)?;
        took("after-scan/read-staged-lists", began);
        let began = Instant::now();
        let carried =
            super::index::carried_services(&tree.checkout, &hosted_data, &previous_data, &fresh)?;
        // A check writes kept state under `.carrick` (carrick#3245): the rows
        // of the carried services it walked, bound on use by build stamp,
        // commit and every file's hash ([`crate::position_rows::kept_rows`]),
        // so the next check reads them instead of walking again. Never an
        // analysed service's rows.
        super::index::keep_carried_walks(&scan_dir, &kept, &carried);
        took("after-scan/carried-and-kept-walks", began);
        let began = Instant::now();
        for (offset, blob) in fresh.iter().chain(&carried).enumerate() {
            services.insert(service_id(blob));
            write_blob(&blobs.join(format!("local-{position}-{offset}.json")), blob)?;
        }
        took("after-scan/write-local-blobs", began);
    }

    // Phase 2. The join reads the blobs back and runs the type check over
    // them; it analyses no source, so its cost is the workspace's pairs. The
    // repo it is pointed at names the run, and reads its services from the
    // previous generation its own re-scan read (carrick#3366): every blob is
    // in `blobs`.
    // It reads the anchor's checkout as its re-scan did, under the same name.
    let anchor = repos
        .first()
        .map_or(workspace.root.as_path(), |tree| tree.checkout.as_path());
    let anchor_previous = (!repos.is_empty()).then(|| staged(generation, "previous", 0));
    let out = generation.join("join.json");
    let mut join = join_command(&exe, anchor, &blobs, &out, anchor_previous.as_deref());
    if let Some(tree) = repos.first() {
        join.env(crate::utils::LOCAL_REPO_NAME_ENV, repo_label(&tree.repo));
    }
    join.env(JUDGE_ANSWERS_ENV, &judged);
    join.env_remove(crate::progress::PROGRESS_ENV);
    if let Some(narrator) = narrator {
        narrator.phase(
            Instant::now(),
            format!(
                "comparing routes and calls across {}",
                plural(services.len(), "service", "services")
            ),
        );
    }
    let began = Instant::now();
    bounded(join, "re-join", deadline, narrator)?;
    took("join", began);
    let began = Instant::now();
    let text = std::fs::read_to_string(&out)
        .map_err(|e| format!("the re-join wrote no result to {}: {e}", out.display()))?;
    let joined: super::LocalJoin =
        serde_json::from_str(&text).map_err(|e| format!("could not read the re-join: {e}"))?;

    // The same fold the indexer performs, over the same shapes, so a re-checked
    // row and an indexed row can never be built two different ways.
    let index = build(workspace, &blobs, &joined, &remote_services)?;
    took("fold", began);
    Ok(Rejoined {
        index,
        join: joined,
        services: services.len(),
    })
}

/// The V8 heap each of a re-scan's sidecars may grow to, in MB, unless the
/// caller set [`crate::services::type_sidecar::MAX_OLD_SPACE_ENV`] itself.
///
/// The scan sizes how many services it analyses at once from the free memory
/// and this cap ([`crate::engine::service_workers`]), so the cap is what lets
/// a re-scan run its services side by side without asking for more memory
/// than the machine has: every sidecar is held to it. The default cap is for
/// the inferred scan of a whole repo; a no-model re-scan of the services a
/// change reaches peaked at 2.0 GB resident for its largest process on a
/// 35-service monorepo (carrick#2826), so 3 GB leaves room and lets two to
/// four services run at once on a laptop.
const RESCAN_HEAP_CAP_MB: u64 = 3072;

/// The subprocess one repo's re-scan runs as: the facts-only scan, analysing
/// only the services whose inputs changed (`--changed-only`), with no
/// signatures, no progress lines and the re-scan's heap cap.
///
/// It scans the checkout the files are read from under the indexed repo's
/// name ([`crate::utils::LOCAL_REPO_NAME_ENV`]), so a scan of a linked
/// worktree writes blobs the re-join reads as that repo's (carrick#3595).
fn rescan_command(exe: &Path, tree: &Tree, scan_dir: &Path, previous: &Path) -> Command {
    let mut scan = scan_command(
        exe,
        &tree.checkout,
        scan_dir,
        previous,
        None,
        &super::index::Pass::Facts,
    );
    scan.arg("--changed-only")
        .env(crate::utils::LOCAL_REPO_NAME_ENV, repo_label(&tree.repo))
        .env(super::SKIP_SIGNATURES_ENV, "1")
        .env_remove(crate::progress::PROGRESS_ENV);
    if let Some(cap) = rescan_heap_cap(
        std::env::var_os(crate::services::type_sidecar::MAX_OLD_SPACE_ENV).as_deref(),
    ) {
        scan.env(crate::services::type_sidecar::MAX_OLD_SPACE_ENV, cap);
    }
    scan
}

/// The heap cap a re-scan sets, given what the caller's environment holds:
/// [`RESCAN_HEAP_CAP_MB`], or nothing when the caller chose a cap (or turned
/// it off) itself.
fn rescan_heap_cap(caller: Option<&std::ffi::OsStr>) -> Option<String> {
    caller.is_none().then(|| RESCAN_HEAP_CAP_MB.to_string())
}

/// Whether a blob's record of a function is about the file being re-checked.
///
/// `file_path` in a blob is repo-relative, and so is `relative`; the separator
/// is normalised on both sides because a blob written on Windows spells it
/// with a backslash and the index's own paths use `/`.
fn declared_in(function: &crate::visitor::FunctionDefinition, relative: &str) -> bool {
    // `<module>` is the file's own top-level call sites recorded under a
    // synthetic definition (carrick#965). It is not a function anybody wrote,
    // so a file that gains its first import would otherwise report one.
    if function.name == crate::visitor::MODULE_SCOPE_KEY {
        return false;
    }
    let declared = function.file_path.to_string_lossy().replace('\\', "/");
    declared == relative.replace('\\', "/")
}

/// The names one set of blobs records for this file.
fn functions_in<'a>(
    blobs: impl Iterator<Item = &'a crate::cloud_storage::CloudRepoData>,
    relative: &str,
) -> std::collections::BTreeSet<String> {
    blobs
        .flat_map(|blob| blob.function_definitions.values())
        .filter(|function| declared_in(function, relative))
        .map(|function| function.name.clone())
        .collect()
}

/// What the parse states in this file that `indexed` does not name.
///
/// A name, not a body: the index holds no hash of the source, so a function
/// that was renamed reads as new and one that was rewritten under its old name
/// does not. That limit is stated wherever this list is rendered, because it
/// decides what the answer means. The parse keys definitions by name, so each
/// name comes once; `<module>` never comes ([`crate::pr_functions::stated_in`]).
fn new_functions(
    indexed: &std::collections::BTreeSet<String>,
    stated: impl Iterator<Item = crate::pr_functions::Stated>,
) -> Vec<super::contract::NewFunction> {
    let mut found: Vec<super::contract::NewFunction> = stated
        .filter(|function| !indexed.contains(&function.name))
        .map(|function| super::contract::NewFunction {
            name: function.name,
            line: function.line,
            lead: None,
        })
        .collect();
    // Line order, so the list reads down the file rather than out of a hash
    // map, and two runs over one tree produce the same answer.
    found.sort_by(|left, right| left.line.cmp(&right.line).then(left.name.cmp(&right.name)));
    found.truncate(MAX_NEW_FUNCTIONS);
    found
}

/// Whether the type check reached this row: a verdict it attempted, in either
/// direction, as opposed to a row no pair bears on.
fn states_a_type_verdict(item: &IndexedItem) -> bool {
    item.verdict
        .as_ref()
        .is_some_and(|verdict| verdict.state != "not_checked")
}

/// Where repo `position`'s staged `kind` list ("previous" or "hosted") waits.
fn staged(generation: &Path, kind: &str, position: usize) -> std::path::PathBuf {
    generation.join(format!("{kind}-{position}.json"))
}

fn write_json(path: &Path, blobs: &[crate::cloud_storage::CloudRepoData]) -> Result<(), String> {
    std::fs::write(path, serde_json::to_vec(blobs).map_err(|e| e.to_string())?)
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn read_staged(path: &Path) -> Result<Vec<crate::cloud_storage::CloudRepoData>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn write_blob(path: &Path, blob: &crate::cloud_storage::CloudRepoData) -> Result<(), String> {
    std::fs::write(path, serde_json::to_vec(blob).map_err(|e| e.to_string())?)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// How often a waiting phase looks at the clock.
const POLL: Duration = Duration::from_millis(50);

/// Run one phase, and stop it at the deadline.
///
/// The deadline is checked BEFORE the phase starts as well as while it runs: a
/// second phase that begins with no budget left would spend the whole of the
/// next one before anybody looked (carrick#748). The child's output is not
/// rendered — a read-only command prints its answer on stdout and nothing else.
/// With a `narrator`, the child's stderr is read for its progress updates,
/// which the narrator turns into the few lines a waiting caller sees; without
/// one, both streams are dropped.
fn bounded(
    mut command: Command,
    what: &str,
    deadline: Instant,
    narrator: Option<&Narrator>,
) -> Result<(), String> {
    if Instant::now() >= deadline {
        return Err(format!("no budget left for the {what}"));
    }
    let stderr = if narrator.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    let mut child = command
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .map_err(|e| format!("could not start the {what}: {e}"))?;
    let updates = child.stderr.take().map(read_updates);
    // A signal that ends this command reaches the phase too (carrick#1379).
    super::index::waiting_on(Some(child.id()));
    let outcome = wait_within(&mut child, what, deadline, narrator, updates.as_ref());
    super::index::waiting_on(None);
    outcome
}

/// Wait for `child` until `deadline`, and stop it there, narrating its
/// updates as they arrive when there is a narrator.
fn wait_within(
    child: &mut std::process::Child,
    what: &str,
    deadline: Instant,
    narrator: Option<&Narrator>,
    updates: Option<&Receiver<crate::progress::Update>>,
) -> Result<(), String> {
    loop {
        if let Some(narrator) = narrator {
            if let Some(updates) = updates {
                while let Ok(update) = updates.try_recv() {
                    narrator.update(Instant::now(), &update);
                }
            }
            narrator.quiet(Instant::now());
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                // `ExitStatus`'s own words ("exit status: 1", or the
                // signal), never Rust's `Some(1)` (carrick#3366).
                return Err(format!("the {what} stopped ({status})"));
            }
            Ok(None) => {}
            Err(e) => return Err(format!("could not wait for the {what}: {e}")),
        }
        if Instant::now() >= deadline {
            stop(child);
            return Err(format!("the {what} ran past the budget"));
        }
        std::thread::sleep(POLL);
    }
}

/// How long a phase stopped at its deadline has to end its own sidecars.
const STOP_GRACE: Duration = Duration::from_secs(1);

/// End a phase that ran past its deadline: SIGTERM first, so the scan ends the
/// type sidecars it started, then SIGKILL. A SIGKILL alone left them running:
/// a sidecar building a program does not notice its pipes close until that
/// work ends. Waiting here is what keeps the child from being reaped by the
/// shell later.
fn stop(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // SAFETY: a signal to a child this process spawned and has not reaped.
        if unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) } == 0 {
            let asked = Instant::now();
            while asked.elapsed() < STOP_GRACE {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(POLL);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// The progress updates in a child's stderr, read on a thread of their own.
///
/// The thread reads to the end whatever happens to the receiver: a pipe nobody
/// drains fills, and a child blocked on a full stderr is the hang this exists
/// to rule out. Every other line is dropped, as it was when stderr was null.
fn read_updates(stderr: std::process::ChildStderr) -> Receiver<crate::progress::Update> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(update) = crate::progress::parse(&line) {
                let _ = sender.send(update);
            }
        }
    });
    receiver
}

/// The shortest gap between two lines that report counts. A phase change
/// always prints at once; this keeps a file loop from printing per file.
const NARRATION_GAP: Duration = Duration::from_secs(5);

/// The longest a narrated pass stays silent. Under the silence limit of any
/// caller that reads quiet as hung (an agent harness killed a check at 110
/// seconds of it), and long enough that a fast pass prints only its phases.
const NARRATION_HEARTBEAT: Duration = Duration::from_secs(10);

/// What `carrick check --changed` (and the several-file check) says on stderr
/// while it works: one line per phase and per service, counts at most every
/// [`NARRATION_GAP`], and a line saying it is still working after
/// [`NARRATION_HEARTBEAT`] of nothing. Never stdout, which carries the answer,
/// and never `--json`'s.
pub(super) struct Narrator {
    started: Instant,
    state: RefCell<Narration>,
    sink: RefCell<Box<dyn Write>>,
}

#[derive(Default)]
struct Narration {
    /// What the pass is doing, as the still-working line names it.
    doing: String,
    /// The repo being re-read, which names a single-service update.
    repo: String,
    /// The service the last count was about, and its place.
    service: Option<(String, usize)>,
    /// The services of this phase that have reported and not finished, by
    /// their place, with the total they are among.
    active: std::collections::BTreeMap<usize, String>,
    active_total: usize,
    said_at: Option<Instant>,
}

impl Narration {
    /// What the pass is at, for the still-working line: every service that
    /// has reported and not finished when several run side by side, so the
    /// line never names a service that is long done.
    fn still_doing(&self) -> String {
        if let (true, Some((index, name))) = (self.active_total > 1, self.active.first_key_value())
            && self.active.len() == 1
        {
            return format!("re-reading {name} ({index}/{})", self.active_total);
        }
        if self.active.len() < 2 {
            return self.doing.clone();
        }
        let names: Vec<&str> = self.active.values().map(String::as_str).collect();
        let shown = names[..names.len().min(4)].join(", ");
        let more = names.len().saturating_sub(4);
        let tail = if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        };
        format!(
            "re-reading {} of {} services: {shown}{tail}",
            names.len(),
            self.active_total
        )
    }
}

impl Narrator {
    /// A narrator that writes to this process's stderr.
    pub(super) fn to_stderr(started: Instant) -> Self {
        Self::new(started, Box::new(std::io::stderr()))
    }

    fn new(started: Instant, sink: Box<dyn Write>) -> Self {
        Self {
            started,
            state: RefCell::new(Narration::default()),
            sink: RefCell::new(sink),
        }
    }

    fn say(&self, now: Instant, text: &str) {
        let mut sink = self.sink.borrow_mut();
        // A closed stderr is a caller that stopped reading, not a failure of
        // the check.
        let _ = writeln!(sink, "carrick check: {text}");
        let _ = sink.flush();
        self.state.borrow_mut().said_at = Some(now);
    }

    /// A phase starts: said at once.
    pub(super) fn phase(&self, now: Instant, doing: String) {
        {
            let mut state = self.state.borrow_mut();
            state.doing = doing.clone();
            state.service = None;
            state.active.clear();
        }
        self.say(now, &format!("{doing}..."));
    }

    /// One repo's re-scan starts, `position` of `total`.
    fn repo(&self, now: Instant, name: &str, position: usize, total: usize) {
        self.state.borrow_mut().repo = name.to_string();
        let doing = if total > 1 {
            format!("re-reading {total} changed repos ({position}/{total}: {name})")
        } else {
            format!("re-reading {name}")
        };
        self.phase(now, doing);
    }

    /// A count from the re-scan: said when it is about a service not yet
    /// named, and otherwise at most every [`NARRATION_GAP`].
    fn update(&self, now: Instant, update: &crate::progress::Update) {
        let unit = match update.phase {
            crate::progress::Phase::Files => "files",
            crate::progress::Phase::Intents => "functions",
        };
        let (doing, fresh, due) = {
            let mut state = self.state.borrow_mut();
            let doing = if update.service_total > 1 {
                format!(
                    "re-reading {} ({}/{})",
                    update.service, update.service_index, update.service_total
                )
            } else {
                format!("re-reading {}", state.repo)
            };
            let place = Some((update.service.clone(), update.service_index));
            let fresh = state.service != place;
            state.service = place;
            state.doing = doing.clone();
            state.active_total = update.service_total;
            if update.total > 0 && update.done >= update.total {
                state.active.remove(&update.service_index);
            } else {
                state
                    .active
                    .insert(update.service_index, update.service.clone());
            }
            let due = state
                .said_at
                .is_none_or(|said| now.duration_since(said) >= NARRATION_GAP);
            (doing, fresh, due)
        };
        if fresh || due {
            self.say(
                now,
                &format!("{doing}: {} of {} {unit}...", update.done, update.total),
            );
        }
    }

    /// Nothing new: after [`NARRATION_HEARTBEAT`] of silence, say the pass is
    /// still at it and for how long.
    fn quiet(&self, now: Instant) {
        let (doing, due) = {
            let state = self.state.borrow();
            let due = state
                .said_at
                .is_none_or(|said| now.duration_since(said) >= NARRATION_HEARTBEAT);
            (state.still_doing(), due)
        };
        if due && !doing.is_empty() {
            let seconds = now.duration_since(self.started).as_secs();
            self.say(now, &format!("still {doing} ({seconds}s so far)..."));
        }
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("{count} {one}")
    } else {
        format!("{count} {many}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// carrick#2826: a re-scan holds every sidecar to its own heap cap, which
    /// is what the scan sizes its width from, so the services a change
    /// reaches run side by side within the memory the machine has. A cap the
    /// caller chose is left alone.
    #[test]
    fn a_re_scan_caps_each_sidecar_unless_the_caller_chose_a_cap() {
        assert_eq!(rescan_heap_cap(None), Some("3072".to_string()));
        assert_eq!(rescan_heap_cap(Some(std::ffi::OsStr::new("8192"))), None);
        assert_eq!(rescan_heap_cap(Some(std::ffi::OsStr::new("0"))), None);
        let command = rescan_command(
            Path::new("carrick"),
            &Tree::at(Path::new("/repo")),
            Path::new("/out"),
            Path::new("/previous.json"),
        );
        assert!(
            command.get_args().any(|arg| arg == "--changed-only"),
            "only the services a change reaches are analysed"
        );
        if std::env::var_os(crate::services::type_sidecar::MAX_OLD_SPACE_ENV).is_none() {
            assert!(command.get_envs().any(|(key, value)| {
                key == crate::services::type_sidecar::MAX_OLD_SPACE_ENV
                    && value == Some(std::ffi::OsStr::new("3072"))
            }));
        }
        // What that cap buys on a laptop with 12 GB free: two services at
        // once where the default cap allows one.
        assert_eq!(
            crate::engine::service_workers::at_once_with(12_452, 12, Some(3072)),
            2
        );
    }

    /// Kept judge answers are the writing build's alone: the same index under
    /// another build is another folder, so a build never reads answers another
    /// one kept, and this build's own folder names its build.
    #[test]
    fn kept_answers_are_keyed_by_the_build_that_kept_them() {
        let index_dir = Path::new("/w/.carrick");
        let one = kept_dir_of(index_dir, "judged", "2026-11-11T00:00:00Z", "0.3.153-abc");
        let other = kept_dir_of(index_dir, "judged", "2026-11-11T00:00:00Z", "0.3.154-def");
        assert_ne!(one, other);
        let workspace = Workspace {
            root: Path::new("/w").to_path_buf(),
            repos: Vec::new(),
            missing: Vec::new(),
            repos_detected_by: String::new(),
            repos_added: Vec::new(),
            repos_excluded: Vec::new(),
            parent_proposal: None,
        };
        let here = kept_dir(&workspace, "judged", "2026-11-11T00:00:00Z").expect("a generation");
        assert!(
            here.to_string_lossy()
                .ends_with(&format!(".{}", build_id())),
            "{here:?}"
        );
    }

    /// A re-scan of a linked worktree reads the worktree and names itself as
    /// the indexed repo (carrick#3595).
    #[test]
    fn a_re_scan_of_a_worktree_reads_it_under_the_repos_name() {
        let tree = Tree {
            repo: std::path::PathBuf::from("/work/api"),
            checkout: std::path::PathBuf::from("/work/api/.claude/worktrees/b"),
        };
        let command = rescan_command(
            Path::new("carrick"),
            &tree,
            Path::new("/out"),
            Path::new("/previous.json"),
        );
        assert!(
            command
                .get_args()
                .any(|arg| arg == "/work/api/.claude/worktrees/b")
        );
        assert!(command.get_envs().any(|(key, value)| {
            key == crate::utils::LOCAL_REPO_NAME_ENV && value == Some(std::ffi::OsStr::new("api"))
        }));
    }

    #[test]
    fn the_budget_is_ten_seconds_unless_a_caller_says_otherwise() {
        assert_eq!(budget_from(None), DEFAULT_BUDGET);
        assert_eq!(budget_from(Some("250")), Duration::from_millis(250));
        // A value this build cannot read is the default, never zero: a
        // mistyped variable must not silently switch the feature off.
        assert_eq!(budget_from(Some("soon")), DEFAULT_BUDGET);
    }

    #[test]
    fn a_phase_with_no_budget_left_is_never_started() {
        // `sleep 30` would outlast any test, so reaching the error at once is
        // the proof that nothing was spawned.
        let mut command = Command::new("sleep");
        command.arg("30");
        let started = Instant::now();
        let error = bounded(command, "re-scan", Instant::now(), None).unwrap_err();
        assert!(error.contains("no budget left"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_phase_that_runs_long_is_killed_at_the_deadline() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let started = Instant::now();
        let error = bounded(
            command,
            "re-join",
            Instant::now() + Duration::from_millis(200),
            None,
        )
        .unwrap_err();
        assert!(error.contains("past the budget"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the deadline did not stop it: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_phase_that_finishes_inside_the_budget_is_a_success() {
        let command = Command::new("true");
        assert!(
            bounded(
                command,
                "re-scan",
                Instant::now() + Duration::from_secs(30),
                None
            )
            .is_ok()
        );
    }

    #[test]
    fn a_phase_that_fails_says_what_it_exited_with() {
        let command = Command::new("false");
        let error = bounded(
            command,
            "re-scan",
            Instant::now() + Duration::from_secs(30),
            None,
        )
        .expect_err("a failing phase is not a success");
        // In the words an agent reads, never Rust's `Some(1)` (carrick#3366).
        assert_eq!(error, "the re-scan stopped (exit status: 1)");
    }

    /// What a [`Narrator`] wrote, readable after it wrote it.
    #[derive(Clone, Default)]
    struct Said(std::rc::Rc<RefCell<Vec<u8>>>);

    impl Write for Said {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Said {
        fn lines(&self) -> Vec<String> {
            String::from_utf8(self.0.borrow().clone())
                .expect("UTF-8")
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    fn files(service: &str, index: usize, total: usize, done: usize) -> crate::progress::Update {
        crate::progress::Update {
            service: service.to_string(),
            service_index: index,
            service_total: total,
            phase: crate::progress::Phase::Files,
            done,
            total: 400,
        }
    }

    /// A phase and a new service are said at once; counts for the same
    /// service wait out the gap, so a file loop is a few lines, not one per
    /// file.
    #[test]
    fn a_narrated_pass_says_each_phase_and_service_and_throttles_counts() {
        let said = Said::default();
        let start = Instant::now();
        let narrator = Narrator::new(start, Box::new(said.clone()));
        narrator.repo(start, "api", 1, 2);
        narrator.update(start, &files("orders", 1, 2, 10));
        narrator.update(start + Duration::from_secs(1), &files("orders", 1, 2, 20));
        narrator.update(start + Duration::from_secs(6), &files("orders", 1, 2, 300));
        narrator.update(start + Duration::from_secs(7), &files("billing", 2, 2, 1));
        narrator.phase(
            start + Duration::from_secs(8),
            format!(
                "comparing routes and calls across {}",
                plural(5, "service", "services")
            ),
        );
        assert_eq!(
            said.lines(),
            vec![
                "carrick check: re-reading 2 changed repos (1/2: api)...",
                "carrick check: re-reading orders (1/2): 10 of 400 files...",
                "carrick check: re-reading orders (1/2): 300 of 400 files...",
                "carrick check: re-reading billing (2/2): 1 of 400 files...",
                "carrick check: comparing routes and calls across 5 services...",
            ]
        );
    }

    /// Services re-read side by side report in turn; the still-working line
    /// names the ones that have not finished, not the first one to report.
    #[test]
    fn the_still_line_names_the_services_in_progress() {
        let said = Said::default();
        let start = Instant::now();
        let narrator = Narrator::new(start, Box::new(said.clone()));
        narrator.repo(start, "api", 1, 1);
        narrator.update(start, &files("a", 1, 4, 5));
        narrator.update(start, &files("b", 2, 4, 5));
        narrator.update(start, &files("c", 3, 4, 5));
        narrator.update(start, &files("a", 1, 4, 400));
        narrator.quiet(start + Duration::from_secs(10));
        narrator.update(start + Duration::from_secs(11), &files("b", 2, 4, 400));
        narrator.quiet(start + Duration::from_secs(21));
        let still: Vec<String> = said
            .lines()
            .into_iter()
            .filter(|line| line.contains("still"))
            .collect();
        assert_eq!(
            still,
            vec![
                "carrick check: still re-reading 2 of 4 services: b, c (10s so far)...",
                "carrick check: still re-reading c (3/4) (21s so far)...",
            ]
        );
    }

    /// A repo of one service is named by the repo, and a stretch with nothing
    /// new says the pass is still at it once per heartbeat, never sooner.
    #[test]
    fn a_silent_stretch_says_the_pass_is_still_working() {
        let said = Said::default();
        let start = Instant::now();
        let narrator = Narrator::new(start, Box::new(said.clone()));
        narrator.repo(start, "api", 1, 1);
        narrator.update(start, &files("(root)", 1, 1, 3));
        narrator.quiet(start + Duration::from_secs(9));
        narrator.quiet(start + Duration::from_secs(10));
        narrator.quiet(start + Duration::from_secs(15));
        narrator.quiet(start + Duration::from_secs(20));
        assert_eq!(
            said.lines(),
            vec![
                "carrick check: re-reading api...",
                "carrick check: re-reading api: 3 of 400 files...",
                "carrick check: still re-reading api (10s so far)...",
                "carrick check: still re-reading api (20s so far)...",
            ]
        );
    }

    /// A child that writes progress is read as it runs, and its stderr is
    /// drained to the end: the narrated phase still finishes.
    #[test]
    fn a_narrated_phase_reads_the_childs_updates_and_finishes() {
        let said = Said::default();
        let start = Instant::now();
        let narrator = Narrator::new(start, Box::new(said.clone()));
        narrator.repo(start, "api", 1, 1);
        let update = serde_json::to_string(&files("orders", 1, 2, 7)).expect("JSON");
        let mut command = Command::new("sh");
        command.arg("-c").arg(format!(
            "i=0; while [ $i -lt 10000 ]; do echo noise line past a pipe buffer $i >&2; \
             i=$((i+1)); done; \
             echo '@carrick-progress {update}' >&2"
        ));
        bounded(
            command,
            "re-scan",
            Instant::now() + Duration::from_secs(30),
            Some(&narrator),
        )
        .expect("the phase finishes");
        // The last update can land after the exit is seen; what matters is
        // that the run finished and nothing but narration was written.
        assert!(
            said.lines()
                .iter()
                .all(|line| line.starts_with("carrick check: ")),
            "{:?}",
            said.lines()
        );
    }

    fn miss(files: usize) -> Miss {
        Miss {
            elapsed_ms: 10_000,
            budget_ms: 10_000,
            files,
        }
    }

    const TEN_S: Duration = Duration::from_millis(10_000);

    /// A miss is remembered per service and per index generation: another
    /// service of the same repo, or the same service against a newer index,
    /// tries the re-check again (carrick#3198).
    #[test]
    fn a_timed_out_re_check_remembers_only_the_edited_files_service() {
        let plan = Plan {
            generation: "g1".to_string(),
            services: vec!["types-owner".to_string(), "big-reader".to_string()],
            digests: Default::default(),
            working: Default::default(),
            files: [
                ("types-owner".to_string(), 3),
                ("big-reader".to_string(), 900),
            ]
            .into_iter()
            .collect(),
            scope: RecheckScope {
                reread: vec!["types-owner".to_string(), "big-reader".to_string()],
                not_reread: Vec::new(),
                stale: Vec::new(),
            },
        };
        let recorded = plan.miss_of("types-owner", TEN_S, TEN_S);
        assert_eq!(
            recorded,
            vec![(
                "types-owner".to_string(),
                Miss {
                    elapsed_ms: 10_000,
                    budget_ms: 10_000,
                    files: 3
                }
            )],
            "a reader re-read beside the edited service is tried again on its own edit"
        );
    }

    #[test]
    fn a_service_miss_holds_for_its_service_and_generation_only() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("recheck-misses.json");

        assert!(
            ServiceMisses::read(&path)
                .holds("g1", "big", TEN_S)
                .is_none()
        );
        ServiceMisses::record(&path, "g1", &[("big".to_string(), miss(40))]);
        ServiceMisses::record(&path, "g1", &[("big".to_string(), miss(40))]);
        let read = ServiceMisses::read(&path);
        assert_eq!(read.holds("g1", "big", TEN_S), Some(&miss(40)));
        assert_eq!(read.services.len(), 1, "recorded once");
        assert!(
            read.holds("g1", "small", TEN_S).is_none(),
            "a sibling service of the same repo still re-checks"
        );
        assert!(
            read.holds("g2", "big", TEN_S).is_none(),
            "a new index tries again"
        );

        // A miss against a newer index replaces the older generation's.
        ServiceMisses::record(&path, "g2", &[("small".to_string(), miss(3))]);
        let read = ServiceMisses::read(&path);
        assert!(read.holds("g2", "small", TEN_S).is_some());
        assert!(read.holds("g2", "big", TEN_S).is_none());
        assert!(read.holds("g1", "big", TEN_S).is_none());

        // No generation means no record either way.
        ServiceMisses::record(&path, "", &[("big".to_string(), miss(40))]);
        assert!(ServiceMisses::read(&path).holds("", "big", TEN_S).is_none());

        // The file the repo-wide rule wrote reads as no misses.
        std::fs::write(&path, r#"{"generation":"g2","repos":["/w/big"]}"#).unwrap();
        assert!(
            ServiceMisses::read(&path)
                .holds("g2", "big", TEN_S)
                .is_none()
        );
    }

    /// A miss records the budget it ran under, and silences only a call whose
    /// budget is no longer: a 10 s miss must not switch off the 180 s runner.
    #[test]
    fn a_miss_under_a_shorter_budget_does_not_hold_for_a_longer_call() {
        let long = Duration::from_millis(180_000);
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("recheck-misses.json");
        ServiceMisses::record(&path, "g1", &[("big".to_string(), miss(40))]);
        let read = ServiceMisses::read(&path);
        assert!(read.holds("g1", "big", TEN_S).is_some());
        assert!(
            read.holds("g1", "big", Duration::from_millis(5_000))
                .is_some()
        );
        assert!(
            read.holds("g1", "big", long).is_none(),
            "a miss at 10 s says nothing about a call with 180 s"
        );
        let at_cap = Miss {
            budget_ms: 180_000,
            ..miss(40)
        };
        ServiceMisses::record(&path, "g1", &[("big".to_string(), at_cap)]);
        assert!(
            ServiceMisses::read(&path)
                .holds("g1", "big", long)
                .is_some()
        );
        assert!(
            ServiceMisses::read(&path)
                .holds("g1", "big", TEN_S)
                .is_some()
        );
    }

    /// A file written before misses named their budget has no `budget_ms`; it
    /// reads as no misses rather than guessing one.
    #[test]
    fn a_miss_file_without_budgets_reads_as_no_misses() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("recheck-misses.json");
        std::fs::write(
            &path,
            r#"{"generation":"g1","services":{"big":{"elapsed_ms":10000,"files":40}}}"#,
        )
        .unwrap();
        assert!(
            ServiceMisses::read(&path)
                .holds("g1", "big", TEN_S)
                .is_none()
        );
    }

    /// The reason a skipped re-check gives names the service, how much of it
    /// the index holds, and the command that gives a fresh answer.
    #[test]
    fn the_miss_reason_names_the_service_and_the_fresh_read() {
        let reason = miss_reason("catalog", &miss(12));
        assert!(
            reason.contains("re-reading catalog (12 indexed files)"),
            "{reason}"
        );
        assert!(reason.contains("took longer than 10 s"), "{reason}");
        assert!(reason.contains("check --changed"), "{reason}");
    }

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn names(found: Vec<Added>) -> Vec<String> {
        found.into_iter().map(|(f, _)| f.name).collect()
    }

    /// Test sources are not in the index, so an edit to one adds nothing,
    /// whatever functions or callbacks it states.
    #[test]
    fn an_edit_to_a_test_file_names_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "package.json", "{\"name\":\"svc\"}");
        write(
            dir.path(),
            "src/a.test.ts",
            "function helper() {}\nit('x', () => { helper(); });\ndescribe('y', () => {});\n",
        );
        write(
            dir.path(),
            "tests/b.ts",
            "export function inTestsDir() {}\n",
        );
        let services = vec![crate::config::Config::default()];
        let none = std::collections::BTreeSet::new();
        for file in ["src/a.test.ts", "tests/b.ts"] {
            assert_eq!(
                names(additions(&services, dir.path(), file, &none)),
                Vec::<String>::new(),
                "{file}"
            );
        }
    }

    /// In a file the scan reads, one added named function is named, and the
    /// extractor's own callback names are not.
    #[test]
    fn one_added_named_function_in_a_service_file_names_exactly_it() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "package.json", "{\"name\":\"svc\"}");
        write(
            dir.path(),
            "src/a.ts",
            "export function kept() {}\nexport function added() {}\n\
             [1].forEach((x) => { kept(); });\nemitter.on('data', (c) => { kept(); });\n",
        );
        let services = vec![crate::config::Config::default()];
        let indexed: std::collections::BTreeSet<String> =
            ["kept".to_string()].into_iter().collect();
        assert_eq!(
            names(additions(&services, dir.path(), "src/a.ts", &indexed)),
            vec!["added".to_string()]
        );
    }
}

#[cfg(test)]
#[path = "recheck_lead_tests.rs"]
mod lead_tests;
