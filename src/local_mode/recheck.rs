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
//!    operation reading a type declared in that file, or for a file in no
//!    service's directory the services that read it ([`Reach`]); every other
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
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use super::checkout::Tree;
use super::contract::{NotReread, RecheckScope};
use super::index::{build, join_command, read_blobs, repo_label, scan_command, service_id};
use super::kept_reads::{self, Built, Inputs, RepoReads, similar_of};
use super::read_model::{IndexedItem, IndexedRepo, IndexedService};
use super::similar::Similar;
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
    /// The compiler engine was ready for this re-scan ([`engine_ready_of`],
    /// carrick#3948).
    pub engine_ready: bool,
}

/// Whether the compiler engine was ready for the re-scan that built `index`
/// for repo `name`: none of the `reread` services records that the engine was
/// not found (`spawn`) or missed its ready budget (`init`). A service carried
/// from the index was not part of this re-scan, so its record says nothing. A
/// died-mid-run engine (`resolve`, `capture`) was ready, so it is not a "not
/// ready" here.
pub(super) fn engine_ready_of(
    index: &super::read_model::LocalIndex,
    name: &str,
    reread: &[String],
) -> bool {
    !index
        .repos
        .iter()
        .filter(|repo| repo.name == name)
        .flat_map(|repo| repo.services.iter())
        .filter(|service| reread.contains(&service.name))
        .any(engine_lost)
}

/// Whether the boundary `service` was re-scanned with records that the
/// compiler engine was not found or never became ready (`spawn`, `init`).
fn engine_lost(service: &super::read_model::IndexedService) -> bool {
    service
        .boundary
        .as_ref()
        .and_then(|boundary| boundary.types_degraded.as_ref())
        .is_some_and(|degraded| matches!(degraded.stage.as_str(), "spawn" | "init"))
}

/// What a check says when a re-scanned service ran with no compiler engine
/// (carrick#4816). That scan asked the library store nothing, so every row a
/// library package states is missing: a pass would read a socket emit or a
/// route call as `no_route`, which is a result the run could not read. Same
/// rule as [`library_unavailable`]: only a service this run re-scanned
/// (`rescanned`) stops it, and a carried one is untouched.
pub const ENGINE_UNAVAILABLE: &str =
    "the compiler engine was not found or was not ready in time to re-read the edited code.";

pub(super) fn engine_unavailable<'a>(
    rebuilt: &super::read_model::LocalIndex,
    repos: impl IntoIterator<Item = &'a String>,
    rescanned: &std::collections::BTreeSet<String>,
) -> Option<&'static str> {
    let repos: std::collections::BTreeSet<&String> = repos.into_iter().collect();
    rebuilt
        .repos
        .iter()
        .filter(|repo| repos.contains(&repo.name))
        .flat_map(|repo| repo.services.iter())
        .filter(|service| rescanned.contains(&service.name))
        .any(engine_lost)
        .then_some(ENGINE_UNAVAILABLE)
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
    /// The local read model was being rebuilt (`.carrick/repos` is not the
    /// model `index.json` names), so nothing was judged: the background
    /// runner keeps the edit queued and asks again.
    pub rebuilding: bool,
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

/// Who a re-check's miss is remembered for (carrick#4412).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    /// The service whose directory holds the edited file: a miss silences
    /// later re-checks of every file of that service.
    Service(String),
    /// An edited file in no service's directory: a miss silences later
    /// re-checks of this file only, never a service that reads it.
    File { repo: String, file: String },
}

impl Owner {
    /// The key a file's miss is kept under.
    fn file_key(repo: &str, file: &str) -> String {
        format!("{repo}/{file}")
    }
}

/// The services one re-check reads, decided from the index (carrick#3198,
/// carrick#4412).
///
/// A file in a service's directory re-reads that service plus every service
/// that holds an operation reading a type declared in the file
/// (`uses[file].types[].operations`, same repo only). A file in no service's
/// directory re-reads the services that read it. Every other service of the
/// repo is judged at its newest known version, never re-read.
pub struct Reach {
    /// Who a miss is remembered for.
    pub owner: Owner,
    /// The re-read services: the owner first for a service's file, else in
    /// index order.
    pub reread: Vec<String>,
    /// The repo's services as the index lists them.
    services: Vec<IndexedService>,
    /// Indexed files per re-read service, for the miss reason.
    files: std::collections::BTreeMap<String, usize>,
}

/// The services holding an operation that reads a type declared in `relative`.
fn type_readers<'a>(
    repo: &'a IndexedRepo,
    relative: &str,
) -> impl Iterator<Item = &'a str> + Clone {
    repo.uses
        .as_ref()
        .and_then(|uses| uses.get(relative))
        .into_iter()
        .flat_map(|uses| &uses.types)
        .flat_map(|used| &used.operations)
        .map(|operation| operation.service.as_str())
}

/// Indexed files per service of `reread`, for the miss reason.
fn indexed_files(
    repo: &IndexedRepo,
    reread: &[String],
) -> std::collections::BTreeMap<String, usize> {
    reread
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
        .collect()
}

impl Reach {
    /// The services one re-check of `relative` reads (carrick#3198,
    /// carrick#4412). A file in a service's directory is that service's
    /// (`service_row`: its rows' service, else the directory's): that service
    /// first, then every service holding an operation that reads a type
    /// declared in the file. A file in no service's directory is no service's,
    /// rows or not: the services the walk names (`readers`,
    /// [`services_reading`]) and those type readers, in index order, or every
    /// service of the repo when the walk could not say (`None`).
    /// `None` only for a repo the index holds with no service: nothing can
    /// read the file there.
    pub fn for_edit(
        repo: &IndexedRepo,
        relative: &str,
        service_row: Option<&IndexedService>,
        readers: Option<&[String]>,
    ) -> Option<Self> {
        if repo.services.is_empty() {
            return None;
        }
        Some(match repo.service_for(relative).and(service_row) {
            Some(owner) => Self::owned(repo, relative, owner),
            None => Self::shared(repo, relative, readers),
        })
    }

    fn owned(repo: &IndexedRepo, relative: &str, owner: &IndexedService) -> Self {
        let mut reread = vec![owner.name.clone()];
        let reading = type_readers(repo, relative);
        for service in repo.services.iter().map(|service| service.name.as_str()) {
            let reads = reading.clone().any(|name| name == service);
            if reads && !reread.iter().any(|known| known == service) {
                reread.push(service.to_string());
            }
        }
        Self {
            owner: Owner::Service(owner.name.clone()),
            files: indexed_files(repo, &reread),
            reread,
            services: repo.services.clone(),
        }
    }

    fn shared(repo: &IndexedRepo, relative: &str, readers: Option<&[String]>) -> Self {
        let reading = type_readers(repo, relative);
        let reread: Vec<String> = repo
            .services
            .iter()
            .map(|service| service.name.as_str())
            .filter(|name| {
                readers.is_none_or(|readers| readers.iter().any(|reader| reader == name))
                    || reading.clone().any(|reader| reader == *name)
            })
            .map(str::to_string)
            .collect();
        Self {
            owner: Owner::File {
                repo: repo.name.clone(),
                file: relative.to_string(),
            },
            files: indexed_files(repo, &reread),
            reread,
            services: repo.services.clone(),
        }
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
/// timing lines. The per-file answer ([`super::query::answer`]) and `check
/// --changed` each call it once, first.
pub(super) fn start_timing(started: Instant) {
    if let Ok(mut at) = RUN_STARTED.lock() {
        *at = Some(started);
    }
}

/// Say that `phase` took from `began` until now, and where in the command that
/// ended: one line per phase, `recheck-timing <phase> took <ms> ms, ended at
/// <ms> ms`. Both checks call it: the per-file answer (its probe and its
/// re-check) and the several-file `check --changed`, whose parent writes no
/// log of its own.
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
/// and records no miss. A miss that holds still answers as a known miss. The
/// probe reads the kept probe file ([`super::kept_reads`]) when it is current,
/// and the blobs when it is not; it writes nothing. The full re-check writes
/// the file when it is not current, after its blob read.
pub fn run(
    workspace: &Workspace,
    tree: &Tree,
    relative: &str,
    index_generation: &str,
    reach: &Reach,
    before: &super::read_model::LocalIndex,
    (rescan, probe): (bool, &mut ProbeReads),
) -> Result<Fresh, Box<Degraded>> {
    let started = Instant::now();
    let budget = budget();
    let deadline = started + budget;
    // The index is read once, here, and named twice: for what the edit added
    // and for the re-join. What the edit added does not wait on the re-scan
    // (carrick#2849): on a large repo the re-scan alone outlasts the budget,
    // and the reuse nudge would then never hear of a function the edit wrote.
    //
    // A probe reads the kept probe file instead when it is current, and no
    // blob (carrick#4034). The inputs are taken before any blob is read, so a
    // file written from this read is only kept when nothing moved under it.
    let inputs = probe
        .inputs
        .get_or_insert_with(|| Inputs::now(workspace))
        .clone();
    let kept = match (&inputs, rescan) {
        (Some(now), false) => probe.kept_of(workspace, index_generation, now, &tree.repo),
        _ => None,
    };
    let source = match kept {
        Some(reads) => Source::Kept(reads),
        None if !rescan => match probe.held_of(workspace, before, started) {
            Ok(held) => Source::Shared(held),
            // The read model changed under the probe: the full re-check
            // answers once it is whole.
            Err(reason) if reason == REBUILDING => {
                return Err(Box::new(deferred(Vec::new(), started)));
            }
            Err(reason) => return Err(Box::new(unread(reason, started))),
        },
        None => match Held::read(workspace, before) {
            Ok(held) => {
                took("held-read", started);
                Source::Read(Box::new(held))
            }
            // The full re-check judges nothing either, and says so: the
            // runner keeps the edit queued and asks again.
            Err(reason) if reason == REBUILDING => {
                return Err(Box::new(Degraded {
                    rebuilding: true,
                    ..unread(reason, started)
                }));
            }
            Err(reason) => return Err(Box::new(unread(reason, started))),
        },
    };
    // The full re-check keeps the probe file when it is not current: it has
    // just read every blob, and a probe never writes.
    let built = match (&source, &inputs) {
        (Source::Read(held), Some(now))
            if rescan && !kept_reads::is_current(workspace, index_generation, now, &tree.repo) =>
        {
            let paths: Vec<PathBuf> = before
                .repos
                .iter()
                .map(|repo| PathBuf::from(&repo.path))
                .collect();
            let built = Built::of(held, &paths);
            kept_reads::keep(
                workspace,
                index_generation,
                now,
                &built.repos,
                &built.similar,
            );
            Some(built)
        }
        _ => None,
    };
    let (built_repos, built_similar) = match built {
        Some(built) => (built.repos, Some(built.similar)),
        None => (Vec::new(), None),
    };
    let began = Instant::now();
    let reads: Rc<RepoReads> = match &source {
        Source::Kept(reads) => Rc::clone(reads),
        Source::Shared(held) => probe.reads_of(held, &tree.repo),
        Source::Read(held) => Rc::new(
            built_repos
                .iter()
                .find(|reads| reads.path() == tree.repo)
                .cloned()
                .unwrap_or_else(|| RepoReads::of(held, &tree.repo)),
        ),
    };
    let added = added_functions(&reads, tree, relative);
    took("added-functions", began);
    let began = Instant::now();
    let checkout_roots: Vec<PathBuf> = before
        .repos
        .iter()
        .map(|repo| PathBuf::from(&repo.path))
        .collect();
    let new_functions = with_similar(tree, relative, added, &checkout_roots, || {
        match (built_similar, &source) {
            (Some(similar), _) => Rc::new(similar),
            (None, Source::Read(held)) => Rc::new(similar_of(held)),
            (None, Source::Shared(held)) => probe.similar_of(held),
            // A kept probe file read, and the ranker beside it is not current:
            // the blobs give it, as they did before the probe kept anything.
            (None, Source::Kept(_)) => {
                probe.kept_similar(workspace, before, index_generation, inputs.as_ref())
            }
        }
    });
    took("similar", began);
    let began = Instant::now();
    let misses = ServiceMisses::path(workspace);
    let known = ServiceMisses::read(&misses);
    took("known-miss-read", began);
    // A re-check started from a linked worktree keeps its misses under that
    // worktree, so a slow worktree never switches the main checkout off and
    // the reverse (carrick#5039).
    let linked =
        (!super::checkout::same_dir(&tree.checkout, &tree.repo)).then_some(tree.checkout.as_path());
    if let Some(miss) = known.holds(index_generation, &reach.owner, budget, linked) {
        return Err(Box::new(Degraded {
            reason: miss_reason_in(&reach.owner, miss, linked.is_some()),
            new_functions,
            elapsed: started.elapsed(),
            known_miss: true,
            deferred: false,
            deadline: false,
            rebuilding: false,
            scope: None,
        }));
    }
    // A probe stops here, and only a probe can hold the kept probe file
    // instead of the blobs.
    let Source::Read(held) = source else {
        return Err(Box::new(deferred(new_functions, started)));
    };
    let began = Instant::now();
    let plan = Plan::make(
        workspace,
        (&tree.repo, &tree.checkout),
        before,
        reach,
        index_generation,
        &held,
    );
    took("plan-make", began);
    let generation = std::env::temp_dir().join(format!("carrick-recheck-{}", uuid::Uuid::new_v4()));
    let outcome = inside(
        workspace,
        *held,
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
            // miss is its own. A file in no service's directory is its own
            // owner: its miss silences that file and no service that reads
            // it. A lost record costs one more re-check; a wrong one
            // switches a service off for the whole index.
            let missed = Instant::now() >= deadline;
            if missed && let Some(miss) = plan.miss_of(&reach.owner, started.elapsed(), budget) {
                ServiceMisses::record(&misses, index_generation, &reach.owner, miss, linked);
            }
            Err(Box::new(Degraded {
                reason,
                new_functions,
                elapsed: started.elapsed(),
                known_miss: false,
                deferred: false,
                deadline: missed,
                rebuilding: false,
                scope: Some(plan.scope.clone()),
            }))
        }
    }
}

/// What a call read of the index: the blobs, or (a probe only) the kept probe
/// file standing in for them.
enum Source {
    Kept(Rc<RepoReads>),
    /// A probe's blob read, shared by every file of one `--each` call.
    Shared(Rc<Held>),
    /// The full re-check's own read: consumed by the re-scan.
    Read(Box<Held>),
}

/// What one `--each` probe call reads once and hands to every file it answers
/// (carrick#4389): the kept file's inputs and repo reads, the blobs when the
/// kept file is not current, and the ranker. A fresh value reads as the
/// single-file probe does. The full re-check never shares one: it consumes its
/// read, and a miss recorded by one file must stand before the next asks.
#[derive(Default)]
pub struct ProbeReads {
    inputs: Option<Option<Inputs>>,
    kept: std::collections::HashMap<PathBuf, Option<Rc<RepoReads>>>,
    held: Option<Result<Rc<Held>, String>>,
    reads: std::collections::HashMap<PathBuf, Rc<RepoReads>>,
    similar: Option<Rc<Similar>>,
}

impl ProbeReads {
    fn kept_of(
        &mut self,
        workspace: &Workspace,
        generation: &str,
        now: &Inputs,
        repo: &Path,
    ) -> Option<Rc<RepoReads>> {
        self.kept
            .entry(repo.to_path_buf())
            .or_insert_with(|| kept_reads::read(workspace, generation, now, repo).map(Rc::new))
            .clone()
    }

    fn held_of(
        &mut self,
        workspace: &Workspace,
        index: &super::read_model::LocalIndex,
        started: Instant,
    ) -> Result<Rc<Held>, String> {
        self.held
            .get_or_insert_with(|| {
                let read = Held::read(workspace, index);
                if read.is_ok() {
                    took("held-read", started);
                }
                read.map(Rc::new)
            })
            .clone()
    }

    fn reads_of(&mut self, held: &Held, repo: &Path) -> Rc<RepoReads> {
        Rc::clone(
            self.reads
                .entry(repo.to_path_buf())
                .or_insert_with(|| Rc::new(RepoReads::of(held, repo))),
        )
    }

    fn similar_of(&mut self, held: &Held) -> Rc<Similar> {
        Rc::clone(
            self.similar
                .get_or_insert_with(|| Rc::new(similar_of(held))),
        )
    }

    /// The ranker beside a current kept file, else the blobs' (read once).
    fn kept_similar(
        &mut self,
        workspace: &Workspace,
        index: &super::read_model::LocalIndex,
        generation: &str,
        inputs: Option<&Inputs>,
    ) -> Rc<Similar> {
        if let Some(similar) = &self.similar {
            return Rc::clone(similar);
        }
        let kept = inputs
            .and_then(|now| kept_reads::read_similar(workspace, generation, now))
            .map(Rc::new);
        let similar =
            kept.unwrap_or_else(|| match self.held_of(workspace, index, Instant::now()) {
                Ok(held) => Rc::new(similar_of(&held)),
                Err(_) => Rc::new(Similar::build(std::iter::empty())),
            });
        self.similar = Some(Rc::clone(&similar));
        similar
    }
}

/// The refusal when the blobs could not be read.
fn unread(reason: String, started: Instant) -> Degraded {
    Degraded {
        reason,
        new_functions: Vec::new(),
        elapsed: started.elapsed(),
        known_miss: false,
        deferred: false,
        deadline: false,
        rebuilding: false,
        scope: None,
    }
}

/// What a probe answers for an edit that needs a re-scan.
fn deferred(new_functions: Vec<super::contract::NewFunction>, started: Instant) -> Degraded {
    Degraded {
        reason: DEFERRED_REASON.to_string(),
        new_functions,
        elapsed: started.elapsed(),
        known_miss: false,
        deferred: true,
        deadline: false,
        rebuilding: false,
        scope: None,
    }
}

/// Said when a probe found an edit that needs a re-scan. Agent-facing, though
/// a caller that asked for the probe replaces it with its own line.
const DEFERRED_REASON: &str = "this edit needs a re-scan, which this call was asked not to start";

/// The reason a main-checkout re-check that skipped its re-scan gives.
#[cfg(test)]
fn miss_reason(owner: &Owner, miss: &Miss) -> String {
    miss_reason_in(owner, miss, false)
}

/// The same, for a re-check started in a linked worktree when `worktree` is
/// set: the sentence says the miss is that worktree's, as main's is not.
fn miss_reason_in(owner: &Owner, miss: &Miss, worktree: bool) -> String {
    let place = if worktree { " in this worktree" } else { "" };
    let took = if miss.elapsed_ms >= 1000 {
        format!("{} s", miss.elapsed_ms / 1000)
    } else {
        format!("{} ms", miss.elapsed_ms)
    };
    match owner {
        Owner::Service(service) => format!(
            "re-reading {service} ({} indexed files) took longer than {took} against this index, so \
             edits in {service} are not re-checked{place} until `carrick refresh`. Run `carrick check \
             --changed` before you finish.",
            miss.files
        ),
        Owner::File { file, .. } => format!(
            "re-reading services that read {file} ({} indexed files) took longer than {took} \
             against this index, so edits to {file} are not re-checked{place} until `carrick refresh`. \
             Run `carrick check --changed` before you finish.",
            miss.files
        ),
    }
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
    /// Files in no service's directory, keyed `<repo>/<file>` (carrick#4412).
    /// Absent from a misses file written before it existed: no file misses.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    files: std::collections::BTreeMap<String, Miss>,
    /// Misses of re-checks started from a linked worktree, keyed by the
    /// worktree's canonical path (carrick#5039). Absent when no worktree
    /// missed, so main's file is unchanged then. A main miss never holds for a
    /// worktree and a worktree miss never holds for main.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    checkouts: std::collections::BTreeMap<String, CheckoutMisses>,
}

/// One linked worktree's misses: the same two maps main keeps.
#[derive(serde::Serialize, serde::Deserialize, Default, Debug, PartialEq, Eq)]
struct CheckoutMisses {
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    services: std::collections::BTreeMap<String, Miss>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    files: std::collections::BTreeMap<String, Miss>,
}

/// The key a worktree's misses are filed under.
fn checkout_key(checkout: &Path) -> String {
    checkout
        .canonicalize()
        .unwrap_or_else(|_| checkout.to_path_buf())
        .to_string_lossy()
        .into_owned()
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
    ///
    /// `checkout` is `Some` for a re-check started in a linked worktree: only
    /// that worktree's own misses count.
    fn holds(
        &self,
        generation: &str,
        owner: &Owner,
        budget: Duration,
        checkout: Option<&Path>,
    ) -> Option<&Miss> {
        if generation.is_empty() || self.generation != generation {
            return None;
        }
        let (services, files) = match checkout {
            None => (&self.services, &self.files),
            Some(checkout) => {
                let held = self.checkouts.get(&checkout_key(checkout))?;
                (&held.services, &held.files)
            }
        };
        match owner {
            Owner::Service(service) => services.get(service),
            Owner::File { repo, file } => files.get(&Owner::file_key(repo, file)),
        }
        .filter(|miss| u128::from(miss.budget_ms) >= budget.as_millis())
    }

    /// Add the miss of `owner` for `generation`, dropping the misses of any
    /// older one.
    fn record(path: &Path, generation: &str, owner: &Owner, miss: Miss, checkout: Option<&Path>) {
        if generation.is_empty() {
            return;
        }
        let mut held = Self::read(path);
        if held.generation != generation {
            held = Self {
                generation: generation.to_string(),
                ..Default::default()
            };
        }
        let (services, files) = match checkout {
            None => (&mut held.services, &mut held.files),
            Some(checkout) => {
                let entry = held.checkouts.entry(checkout_key(checkout)).or_default();
                (&mut entry.services, &mut entry.files)
            }
        };
        match owner {
            Owner::Service(service) => services.insert(service.clone(), miss),
            Owner::File { repo, file } => files.insert(Owner::file_key(repo, file), miss),
        };
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
    Some(kept_dir_of(
        &workspace.index_dir(),
        kind,
        &generation_name(generation),
        &build_id(),
    ))
}

/// The folder-name form of an index generation: anything a path should not
/// carry becomes `_`.
pub(super) fn generation_name(generation: &str) -> String {
    generation
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
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
    /// Each service's own edits since the index and who reads an edit.
    copies: Copies,
    /// The copy a service's last re-check left, for each service outside the
    /// scope whose edits since then are exactly the ones it was read for.
    working: std::collections::BTreeMap<String, crate::cloud_storage::CloudRepoData>,
    /// The blobs that stand in for the re-scan when every re-read service is
    /// current ([`Copies::current`]).
    current: Option<Vec<crate::cloud_storage::CloudRepoData>>,
    files: std::collections::BTreeMap<String, usize>,
    scope: RecheckScope,
}

/// A re-read service's blob, kept with the digest of the edits it was read
/// for. One file, written whole, so the pair is never torn.
#[derive(serde::Serialize, serde::Deserialize)]
struct WorkingCopy {
    digest: String,
    /// Whether the service read no path edited outside its own directories
    /// when it was read ([`readers_of_the_edit`]). The digest covers only its
    /// own files, so a copy read beside an outside edit that was undone since
    /// would match the digest and still hold the other content: only a whole
    /// copy may stand in for a re-scan ([`current_copy`]).
    whole: bool,
    /// The hosted snapshot the copy was read with
    /// ([`super::hosted::HostedInput::source_key`]); `None` signed out. A copy
    /// stands in only under the same snapshot, and never signed out: the
    /// join would read other hosted services than the copy was judged
    /// against, or none.
    hosted: Option<String>,
    blob: crate::cloud_storage::CloudRepoData,
    /// The carried services the same scan wrote because its join moved
    /// their pair outcomes (`index::carried_services`): the join reads them
    /// in place of their indexed blobs when this copy stands in.
    carried: Vec<crate::cloud_storage::CloudRepoData>,
}

/// What one check knows of one repo's services before it re-scans: the
/// digest of each service's own edits since the index and which services
/// read a path edited outside their own directories. Both checks decide from
/// it which services are current ([`Copies::current`]); the per-edit
/// re-check keeps what its scan read ([`Copies::keep`]). `check --changed`
/// keeps nothing: its copies were never whole, so their one use was as a
/// re-check's stand-in, labelled after a scan an edit could have run beside.
#[derive(Default)]
struct Copies {
    repo: std::path::PathBuf,
    services: Vec<IndexedService>,
    generation: String,
    /// The services of the repo with edits of their own since the index.
    digests: std::collections::BTreeMap<String, String>,
    /// The services that read a path edited outside their own directories,
    /// or whose reads are unknown here.
    readers: std::collections::BTreeSet<String>,
    /// Whether `readers` was walked. Unwalked, no service is current and no
    /// copy kept is whole: a cold `--changed` walks nothing of its own.
    walked: bool,
    /// The services git could not say what changed in.
    unlisted: std::collections::BTreeSet<String>,
    /// The hosted snapshot this check reads
    /// ([`super::hosted::HostedInput::source_key`]); `None` signed out.
    hosted: Option<String>,
}

impl Copies {
    /// Whether any of `services` has a copy kept for this index and build.
    fn any_kept(workspace: &Workspace, generation: &str, services: &[IndexedService]) -> bool {
        kept_dir(workspace, "working", generation).is_some_and(|dir| {
            services
                .iter()
                .any(|service| dir.join(working_name(&service.name)).is_file())
        })
    }

    /// Digests of every service of `services` in `repo`; `readers`, when
    /// walked, names the ones that read a path edited outside their own
    /// directories ([`readers_of_the_edit`]).
    fn of(
        repo: &Path,
        services: &[IndexedService],
        generation: &str,
        readers: Option<std::collections::BTreeSet<String>>,
        hosted: Option<String>,
    ) -> Self {
        let (digests, unlisted) = digests_of(repo, services, |_| true);
        Self {
            repo: repo.to_path_buf(),
            services: services.to_vec(),
            generation: generation.to_string(),
            digests,
            walked: readers.is_some(),
            readers: readers.unwrap_or_default(),
            unlisted,
            hosted,
        }
    }

    /// Whether a walk of who reads the edits could make this repo current:
    /// git listed every service, and every service with edits of its own has
    /// a copy [`current_copy`] takes, short of the walk. Otherwise the repo
    /// is re-scanned whatever the walk would say, so `--changed` does not
    /// walk.
    fn worth_walking(&self, workspace: &Workspace) -> bool {
        self.unlisted.is_empty()
            && !self.digests.is_empty()
            && self.digests.iter().all(|(service, digest)| {
                current_copy(
                    workspace,
                    &self.generation,
                    service,
                    Some(digest),
                    false,
                    self.hosted.as_deref(),
                )
                .is_some()
            })
    }

    /// `None`: git cannot say what changed in `service`. `Some(None)`:
    /// nothing did.
    fn digest(&self, service: &str) -> Option<Option<&String>> {
        (!self.unlisted.contains(service)).then(|| self.digests.get(service))
    }

    /// The services a `--changed` scan of the repo would read for its edits:
    /// each with edits of its own and each reading an edited path outside
    /// its own directories. Every one must be current for the repo to be
    /// joined without a re-scan.
    fn reached(&self) -> Vec<String> {
        self.digests
            .keys()
            .chain(&self.readers)
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// The blobs the join reads in place of a re-scan of `reached`, when
    /// every one of them is current ([`current_copy`]): each service's copy
    /// and the carried services its scan wrote. A carried copy never
    /// replaces a service with edits of its own, and one service carried
    /// differently by two copies is not known: then nothing is current, as
    /// it is whenever git could not say what changed or `reached` is empty.
    fn current(
        &self,
        workspace: &Workspace,
        reached: &[String],
    ) -> Option<Vec<crate::cloud_storage::CloudRepoData>> {
        if !self.walked || !self.unlisted.is_empty() || reached.is_empty() {
            return None;
        }
        let mut own = Vec::new();
        let mut carried: std::collections::BTreeMap<String, crate::cloud_storage::CloudRepoData> =
            Default::default();
        for service in reached {
            let copy = current_copy(
                workspace,
                &self.generation,
                service,
                self.digests.get(service),
                self.readers.contains(service),
                self.hosted.as_deref(),
            )?;
            own.push(copy.blob);
            for blob in copy.carried {
                let id = service_id(&blob);
                if self.digests.contains_key(&id) {
                    continue;
                }
                match carried.get(&id) {
                    Some(seen)
                        if serde_json::to_vec(seen).ok() != serde_json::to_vec(&blob).ok() =>
                    {
                        return None;
                    }
                    Some(_) => {}
                    None => {
                        carried.insert(id, blob);
                    }
                }
            }
        }
        own.extend(carried.into_values());
        Some(own)
    }

    /// Keep what one re-check's scan of this repo wrote: each service it
    /// analysed with edits of its own, with its digest, whether it was
    /// whole, the hosted snapshot it was read with, and every carried
    /// service the scan wrote. `analysed` is the list the scan was told to
    /// analyse; a service on it is never a carried one.
    ///
    /// A copy is kept only when the service's own edits are what they were
    /// before the scan: an edit made while it ran, then undone, would leave a
    /// copy labelled with the undone digest and holding the edit; then the
    /// service's copy goes. A written service with edits of its own that the
    /// scan did not analyse (a consumer whose pair outcomes it moved) keeps
    /// its copy marked not whole: it is older than what this scan wrote for
    /// it, so it never stands in for a re-scan ([`current_copy`]), and the
    /// next re-check still judges against it as its last re-check left it.
    fn keep(
        &self,
        workspace: &Workspace,
        fresh: &[crate::cloud_storage::CloudRepoData],
        analysed: &[String],
    ) {
        let carried: Vec<crate::cloud_storage::CloudRepoData> = fresh
            .iter()
            .filter(|blob| {
                let id = service_id(blob);
                !self.digests.contains_key(&id)
                    && !self.readers.contains(&id)
                    && !analysed.contains(&id)
            })
            .cloned()
            .collect();
        let began = Instant::now();
        let (after, _) = digests_of(&self.repo, &self.services, |service| {
            self.digests.contains_key(service) && analysed.iter().any(|name| name == service)
        });
        took("after-scan/own-edit-digests", began);
        for blob in fresh {
            let id = service_id(blob);
            let Some(digest) = self.digests.get(&id) else {
                continue;
            };
            if !analysed.contains(&id) {
                if let Some(mut older) = read_working(workspace, &self.generation, &id, digest) {
                    older.whole = false;
                    keep_working(workspace, &self.generation, &older);
                }
                continue;
            }
            if after.get(&id) != Some(digest) {
                forget_working(workspace, &self.generation, &id);
                continue;
            }
            keep_working(
                workspace,
                &self.generation,
                &WorkingCopy {
                    digest: digest.clone(),
                    whole: self.walked && !self.readers.contains(&id),
                    hosted: self.hosted.clone(),
                    blob: blob.clone(),
                    carried: carried.clone(),
                },
            );
        }
    }
}

/// The digest of each of `services` that `wanted` names and has edits of its
/// own since the index ([`service_digest`]), and the services git could not
/// list, from one listing per distinct indexed commit.
fn digests_of(
    repo: &Path,
    services: &[IndexedService],
    wanted: impl Fn(&str) -> bool,
) -> (
    std::collections::BTreeMap<String, String>,
    std::collections::BTreeSet<String>,
) {
    let mut listings: std::collections::BTreeMap<String, Option<HashSet<String>>> =
        Default::default();
    let mut digests = std::collections::BTreeMap::new();
    let mut unlisted = std::collections::BTreeSet::new();
    for service in services.iter().filter(|service| wanted(&service.name)) {
        let changed = listings
            .entry(service.commit.clone())
            .or_insert_with(|| super::query::changed_since(repo, &service.commit));
        match changed {
            Some(changed) => {
                if let Some(digest) = service_digest(repo, changed, service, services) {
                    digests.insert(service.name.clone(), digest);
                }
            }
            None => {
                unlisted.insert(service.name.clone());
            }
        }
    }
    (digests, unlisted)
}

/// The one rule for "this service's blob is current", read by both checks
/// (`check --changed` and the per-edit re-check): the service has edits of
/// its own, reads no path edited outside its own directories, and its last
/// re-check kept a whole copy read for exactly those edits under the hosted
/// snapshot this check reads. Such a service is joined from that copy
/// instead of re-scanned. Anything else is re-scanned, and so is every
/// service of a machine that is signed out (`hosted` is `None`).
fn current_copy(
    workspace: &Workspace,
    generation: &str,
    service: &str,
    digest: Option<&String>,
    reads_outside: bool,
    hosted: Option<&str>,
) -> Option<WorkingCopy> {
    if reads_outside {
        return None;
    }
    let hosted = hosted?;
    read_working(workspace, generation, service, digest?).filter(|copy| {
        copy.whole
            && copy.hosted.as_deref() == Some(hosted)
            && !std::iter::once(&copy.blob)
                .chain(&copy.carried)
                .any(read_short)
    })
}

/// Whether the scan that wrote `blob` was short of an answer a re-scan now
/// may get: the library store or the model's replayed answers. Such a copy
/// never stands in for a re-scan; the re-scan says what is missing, or
/// reads it.
fn read_short(blob: &crate::cloud_storage::CloudRepoData) -> bool {
    blob.boundary.as_ref().is_some_and(|boundary| {
        boundary.library_ask_failed.is_some() || boundary.model_answers_unavailable.is_some()
    })
}

/// Write one service's copy where [`read_working`] finds it.
fn keep_working(workspace: &Workspace, generation: &str, copy: &WorkingCopy) {
    let id = service_id(&copy.blob);
    let Some(dir) = kept_dir(workspace, "working", generation) else {
        return;
    };
    if let Some(parent) = dir.parent() {
        prune_other_generations(parent, &dir);
    }
    let Ok(bytes) = serde_json::to_vec(copy) else {
        return;
    };
    let kept = dir.join(working_name(&id));
    let staged = dir.join(format!("{}.{}.tmp", working_name(&id), std::process::id()));
    let _ = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(&staged, bytes))
        .and_then(|()| std::fs::rename(&staged, &kept));
}

/// Remove `service`'s kept copy, so nothing stands in for it.
fn forget_working(workspace: &Workspace, generation: &str, service: &str) {
    if let Some(dir) = kept_dir(workspace, "working", generation) {
        let _ = std::fs::remove_file(dir.join(working_name(service)));
    }
}

impl Plan {
    fn make(
        workspace: &Workspace,
        (edited_repo, repo): (&Path, &Path),
        index: &super::read_model::LocalIndex,
        reach: &Reach,
        generation: &str,
        held: &Held,
    ) -> Self {
        let began = Instant::now();
        // Every service's, the re-read ones too: a re-read service that reads
        // an outside edit is never current, and its copy is not whole.
        let mut readers = readers_of_the_edit(repo, reach.services.iter(), held);
        took("plan-make/readers-of-the-edit", began);
        let began = Instant::now();
        let copies = Copies::of(
            repo,
            &reach.services,
            generation,
            Some(readers.keys().cloned().collect()),
            held.hosted.source_key(),
        );
        let mut working = std::collections::BTreeMap::new();
        let mut scope = RecheckScope {
            reread: reach.reread.clone(),
            not_reread: Vec::new(),
            stale: Vec::new(),
        };
        for service in &reach.services {
            let digest = copies.digest(&service.name);
            if reach.reread.contains(&service.name) {
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
                        && let Some(copy) =
                            read_working(workspace, generation, &service.name, digest)
                    {
                        working.insert(service.name.clone(), copy.blob);
                    }
                    scope.stale.push(service.name.clone());
                    reader.as_deref().unwrap_or_default()
                }
                Some(None) => "no edits since the index; judged at its indexed version",
                Some(Some(digest)) => {
                    match read_working(workspace, generation, &service.name, digest) {
                        Some(copy) => {
                            working.insert(service.name.clone(), copy.blob);
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
        // Every other repository of the workspace: a service there with edits
        // since its index is compared as indexed too, and no re-check reads it
        // (carrick#4819). The same listing as the edited repo's
        // ([`digests_of`]); one git status-level read per repo, no scan. A repo
        // git cannot list reads as changed, as an unlisted service does above.
        let began_others = Instant::now();
        for other in index
            .repos
            .iter()
            .filter(|other| !super::checkout::same_dir(Path::new(&other.path), edited_repo))
        {
            let (changed, unlisted) = digests_of(Path::new(&other.path), &other.services, |_| true);
            scope.stale.extend(changed.into_keys().chain(unlisted));
        }
        took("plan-make/other-repos-stale", began_others);
        let current = copies.current(workspace, &reach.reread);
        took("plan-make/digests-and-working-reads", began);
        Self {
            generation: generation.to_string(),
            services: reach.reread.clone(),
            copies,
            working,
            current,
            files: reach.files.clone(),
            scope,
        }
    }

    /// The miss to remember when this re-check ran out of time: the edited
    /// file's own service only, never a reader service re-read beside it; or,
    /// for a file in no service's directory, that file (carrick#4412).
    fn miss_of(&self, owner: &Owner, elapsed: Duration, budget: Duration) -> Option<Miss> {
        let files = match owner {
            Owner::Service(name) => {
                if !self.services.contains(name) {
                    return None;
                }
                self.files.get(name).copied().unwrap_or(0)
            }
            Owner::File { .. } => self.files.values().sum(),
        };
        Some(Miss {
            elapsed_ms: elapsed.as_millis() as u64,
            budget_ms: budget.as_millis() as u64,
            files,
        })
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
) -> Option<WorkingCopy> {
    let path = kept_dir(workspace, "working", generation)?.join(working_name(service));
    let copy: WorkingCopy = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (copy.digest == digest && service_id(&copy.blob) == service).then_some(copy)
}

/// The reason for each of `services` that reads a path edited since its
/// index, by service name.
///
/// The type-use record lists the operations that read a type the edited file
/// declares; it does not list one that reads a constant, a wrapper or a type
/// it could not trace. The import closure the change scope already walks
/// ([`crate::change_scope::untouched_since`]) names those. A service's own
/// edits are left out of the listing: the digest already accounts for them
/// (its last re-check's copy, or stale). A change outside the repo's folder is
/// no service's own edit, so it stays in the listing (carrick#4390). A service whose closure cannot be
/// walked (no stored config on this machine) is named too: it is not known to
/// be untouched.
fn readers_of_the_edit<'a>(
    repo: &Path,
    services: impl Iterator<Item = &'a IndexedService>,
    held: &Held,
) -> std::collections::BTreeMap<String, String> {
    let repo_path = repo.to_string_lossy().to_string();
    let mut readers = std::collections::BTreeMap::new();
    // One listing of what changed, shared by every service's view of it.
    let listing = crate::change_scope::ChangedSince::new(&repo_path, &[], &[]);
    for service in services {
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
        // A service's own edits by the repo folder's path. A change outside the
        // folder is no service's own, so it stays, and the walk names a service
        // that reads it (carrick#4390).
        let changes = listing.view(move |path| path.is_some_and(|path| own.covers(path)));
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

/// The services of the repo that read any of `paths`, in index order, by the
/// rule that carries a service under `--changed-only`
/// ([`crate::change_scope::untouched_since`]): its roots (directory and
/// `include` roots), its import closure, and the packages and tsconfig chain
/// it reads. Empty when none does: an edited test file that still loads what
/// it loaded, a file outside every service's roots and import closure. Such an
/// edit changes no row, so the index's answer stands and no re-scan is owed
/// (the scan decides what it reads, not a second classifier). It is the one
/// answerer of "which services read this file" in local mode (carrick#4412).
///
/// `None` whenever that rule cannot say: the repo's services are not all at
/// one commit and clean, a service's config is not on this machine, or the
/// workspace cannot be read. Then the caller takes the path it always took.
/// When its walk runs it states a `no-service-reads` timing line
/// ([`TIMING_ENV`]).
pub(super) fn services_reading(
    workspace_root: &Path,
    tree: &Tree,
    indexed: &IndexedRepo,
    paths: &HashSet<String>,
) -> Option<Vec<String>> {
    let began = Instant::now();
    let commit = indexed.clean_commit()?;
    let Ok(workspace) = Workspace::load(workspace_root) else {
        return None;
    };
    let Ok(local) = read_blobs(&workspace.blobs_dir()) else {
        return None;
    };
    // A model being rebuilt answers nothing here: read against no blobs,
    // every file would read as one no service reads.
    if !holds_repo(&local, indexed) {
        return None;
    }
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
        configs.push(config?);
    }
    let repo_path = tree.checkout.to_string_lossy().to_string();
    let listing = crate::change_scope::ChangedSince::new(&repo_path, &[], &[]);
    let kept = paths.clone();
    // The edited paths are the repo folder's (`locate_file`,
    // `query::changed_since`). Git lists from its top level, and the view
    // compares by the folder's form. A change outside the folder is not an
    // edited file (carrick#4390).
    let only = listing.view(move |path| !path.is_some_and(|path| kept.contains(path)));
    let read = crate::change_scope::untouched_since(&repo_path, &configs, &only, commit);
    took("no-service-reads", began);
    // `configs` is built in `indexed.services` order, so the zip lines up.
    Some(
        indexed
            .services
            .iter()
            .zip(read)
            .filter_map(|(service, read)| read.map(|_| service.name.clone()))
            .collect(),
    )
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
pub(super) struct Held {
    pub(super) local: Vec<crate::cloud_storage::CloudRepoData>,
    pub(super) hosted: super::hosted::HostedInput,
}

impl Held {
    /// The blobs `index` names, or [`REBUILDING`] when `.carrick/repos` is not
    /// the read model `index` was built with: missing, or a service at another
    /// commit. A rebuild swaps the folder whole (`index::run_generation`), so
    /// a reader that meets one mid-swap, or reads the new blobs before the new
    /// index, judges nothing rather than one generation against another.
    pub(super) fn read(
        workspace: &Workspace,
        index: &super::read_model::LocalIndex,
    ) -> Result<Self, String> {
        let local = read_blobs(&workspace.blobs_dir())?;
        if !index.repos.iter().all(|repo| holds_repo(&local, repo)) {
            return Err(REBUILDING.to_string());
        }
        Ok(Self {
            local,
            hosted: super::hosted::cached(workspace),
        })
    }
}

/// Whether `local` (`.carrick/repos`) holds the blob of every service the
/// index names for `repo`, at the commit it names: the read model `index.json`
/// was built with, for that repo.
pub(super) fn holds_repo(
    local: &[crate::cloud_storage::CloudRepoData],
    repo: &super::read_model::IndexedRepo,
) -> bool {
    repo.services.iter().all(|service| {
        local.iter().any(|blob| {
            blob.repo_name == repo.name
                && service_id(blob) == service.name
                && blob.commit_hash == service.commit
        })
    })
}

/// Why a reader judged nothing: the local index changed under it. User-facing
/// only inside `check --changed`'s "the check did not finish"; the probe and
/// the full re-check answer deferred instead, so the edit stays queued.
/// Wording from the copy pass: a retry is right, and `carrick refresh` mends a
/// mismatch that lasts.
pub(super) const REBUILDING: &str = "the local index changed while this check read it; try again, and run `carrick refresh` if it happens again";

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
fn added_functions(reads: &RepoReads, tree: &Tree, relative: &str) -> Vec<Added> {
    let repo = tree.checkout.as_path();
    let configs = reads.configs();
    // The services the scan would resolve for this repo, the way `index`
    // resolves them. A repo they cannot be resolved for names nothing: the
    // question "would the scan read this file" has no answer.
    let Ok(services) = crate::service_derivation::resolve_with_configs(repo, Some(&configs)) else {
        return Vec::new();
    };
    let indexed = reads.declared_in(relative);
    additions(&services, repo, relative, &indexed)
}

/// One function the edit added, with what its similar functions are looked up by.
type Added = (super::contract::NewFunction, super::similar::Query);

/// Attach to each added function the indexed functions it looks like
/// ([`super::similar`]). `ranker` gives the ranker over every blob, in every
/// repo: built from the blobs, or read from the kept probe file
/// ([`super::kept_reads`]). It is asked once per call and only when the edit
/// added a function: an edit that adds none pays nothing here.
fn with_similar<R: std::borrow::Borrow<Similar>>(
    tree: &Tree,
    relative: &str,
    added: Vec<Added>,
    roots: &[PathBuf],
    ranker: impl FnOnce() -> R,
) -> Vec<super::contract::NewFunction> {
    if added.is_empty() {
        return Vec::new();
    }
    let index = ranker();
    let index: &Similar = index.borrow();
    let repo = repo_label(&tree.repo);
    added
        .into_iter()
        .map(|(function, query)| super::contract::NewFunction {
            similar: index
                .similar(&query, &repo, relative)
                .into_iter()
                .map(|row| row_with_checkout(row, roots))
                .collect(),
            ..function
        })
        .collect()
}

/// `row`, told where this machine has its repo checked out: the root among
/// `roots` (the workspace's repos) whose folder name is the row's repo.
fn row_with_checkout(
    row: super::contract::SimilarFunction,
    roots: &[PathBuf],
) -> super::contract::SimilarFunction {
    let checkout = roots
        .iter()
        .find(|root| repo_label(root) == row.repo)
        .map(|root| root.to_string_lossy().into_owned());
    super::contract::SimilarFunction { checkout, ..row }
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
/// Each comes with the lookup the ranking reads, from the same one parse: the
/// definition's tokens and composed signature, and the names its body calls
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
                    || super::similar::Query::named(&function.name),
                    |(key, definition)| {
                        let callees = extractor
                            .callee_refs
                            .get(key)
                            .into_iter()
                            .flatten()
                            .map(|callee| callee.name.clone())
                            .collect();
                        super::similar::Query::of(definition, callees)
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
    let rescan = Rescans {
        listed: Some(plan.services.as_slice()),
        stand_in: Some(&plan.working),
        keep: std::iter::once((name.clone(), &plan.copies)).collect(),
        current: Default::default(),
    };
    // Every re-read service current: the join from its copies, kept only
    // when it judged no pair; otherwise the re-scan, as with no copy.
    // `Err` carries the answer the copies gave; `Ok`, what the re-scan reads.
    let held = match plan.current.as_deref() {
        Some(set) => {
            let how = Rescans {
                listed: rescan.listed,
                stand_in: rescan.stand_in,
                keep: Default::default(),
                current: std::iter::once((name.clone(), set)).collect(),
            };
            let joined = rejoin_held(
                workspace,
                held,
                std::slice::from_ref(tree),
                generation,
                deadline,
                None,
                &how,
                &plan.generation,
            );
            match joined {
                Ok(rejoined) => Err(rejoined),
                Err(_) => {
                    let _ = std::fs::remove_dir_all(generation);
                    Ok(Held::read(workspace, before)?)
                }
            }
        }
        None => Ok(held),
    };
    let rejoined = match held {
        Err(rejoined) => rejoined,
        Ok(held) => rejoin_held(
            workspace,
            held,
            std::slice::from_ref(tree),
            generation,
            deadline,
            None,
            &rescan,
            &plan.generation,
        )?,
    };
    // A re-scan that could not replay the index's model answers has lost every
    // row only the model stated: not a pass, as in the plain check's `answer`
    // (carrick#3413).
    let lost = super::changed::answers_lost(&rejoined.index, std::iter::once(&name));
    if !lost.is_empty() {
        return Err(lost.join(" "));
    }
    if let Some(sentence) = engine_unavailable(&rejoined.index, [&name], &rejoined.rescanned)
        .or_else(|| library_unavailable(&rejoined.index, [&name], &rejoined.rescanned))
    {
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
    let (moved, inside) = judge_edit(workspace, before, &rejoined, tree, relative, plan);
    state_judged(&mut items, &inside);
    state_library_missing(
        &mut items,
        &library_marked(&rejoined.index, &rejoined.rescanned),
    );
    // No rows where the index held some is an answer, not a failure: the edit
    // took the last route out of this file, and the fresh (empty) list is what
    // says so.
    let ran = if items.iter().any(states_a_type_verdict) {
        Ran::ExtractionAndTypes
    } else {
        Ran::Extraction
    };
    state_stale_counterparts(&mut items, &plan.scope.stale);
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
        engine_ready: engine_ready_of(&rejoined.index, &name, &plan.scope.reread),
    })
}

/// Copy the judge's answer onto the edited file's own rows: each in-file
/// problem's `stated` onto the item it is about, matched by kind, line,
/// method, path and dispatch case, so the hook states a row only when the
/// rule `check --changed` lists on holds for it (`changed.rs`,
/// `Problem::listed_without_all`). A row the index already held as a
/// mismatch, or one only the model states, is not stated.
///
/// A call no service serves also gets its verdict here: the rebuilt index
/// stores no verdict for a call with no counterpart, so the item would read
/// `result: null`, "nothing pairs", and the answer `judge` computed would be
/// dropped. The shape is `producer_removed`'s: `not_checked` for the type
/// layer, `no_route` for the routing fact. Only a stated one is written, as
/// before.
fn state_judged(items: &mut [IndexedItem], inside: &[super::changed::Problem]) {
    for problem in inside {
        let no_route = problem.kind == super::changed::ProblemKind::NoRoute;
        for item in items.iter_mut().filter(|item| {
            item.kind.as_str() == problem.role
                && item.line == problem.line
                && item.method == problem.method
                && item.path == problem.path
                && item.dispatch == problem.dispatch
                && (!no_route || item.verdict.is_none())
        }) {
            if no_route {
                if !problem.stated {
                    continue;
                }
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
            item.stated = problem.stated;
        }
    }
}

/// A row this run computed whose counterpart sits in a service that has changed
/// since the index and that this run did not re-read (`scope.stale`) was
/// compared against that service's indexed code, not its working tree
/// (carrick#4773). The comparison stays; the detail says which service it
/// leaves out and points at `check --changed`, which re-reads every changed
/// service. A `compatible` result is not confirmed, so its state reads
/// `not_checked` (the state [`state_library_missing`] uses) with the result
/// kept; a mismatch keeps its state, because it is a finding either way.
/// Any protocol: the rule reads services, never the row's kind.
fn state_stale_counterparts(items: &mut [IndexedItem], stale: &[String]) {
    for item in items.iter_mut() {
        let mut named: Vec<&str> = Vec::new();
        for counterpart in &item.counterparts {
            let service = counterpart.service.as_str();
            if stale.iter().any(|s| s == service) && !named.contains(&service) {
                named.push(service);
            }
        }
        let Some(verdict) = item.verdict.as_mut() else {
            continue;
        };
        if named.is_empty() {
            continue;
        }
        let (who, changed, re_read, uses) = match named.as_slice() {
            [one] => (
                (*one).to_string(),
                "has changed since it was indexed",
                "did not re-read it",
                "its",
            ),
            many => (
                many.join(", "),
                "have changed since they were indexed",
                "did not re-read them",
                "their",
            ),
        };
        verdict.detail = format!(
            "{} ({who} {changed} and this check {re_read}, so the verdict uses {uses} indexed \
             code. `carrick check --changed` gives the current verdict.)",
            verdict.detail
        );
        if verdict.state != "not_checked" && verdict.result.as_deref() == Some("compatible") {
            verdict.state = "not_checked".to_string();
        }
    }
}

/// A row paired with a service whose library rows are missing from the index
/// reads not checked, with that reason, in place of the verdict the pair held
/// (carrick#4807). The shape is [`state_judged`]'s `not_checked`, and `stated`
/// stays false: the judge states nothing for it.
fn state_library_missing(items: &mut [IndexedItem], marked: &std::collections::BTreeSet<String>) {
    for item in items.iter_mut() {
        if item.counterparts.is_empty() {
            continue;
        }
        let side = std::iter::once(item.service.as_str())
            .chain(item.counterparts.iter().map(|c| c.service.as_str()))
            .find(|service| marked.contains(*service));
        if let Some(service) = side {
            item.verdict = Some(super::read_model::StoredVerdict {
                state: "not_checked".to_string(),
                result: None,
                detail: library_missing_reason(service),
            });
            item.stated = false;
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
/// the moved list; its own problems come back apart, for [`state_judged`].
/// A moved row carries `stated` by the same rule as an item.
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
        &library_marked(&rejoined.index, &rejoined.rescanned),
    );
    let (outside, inside): (Vec<_>, Vec<_>) = judged
        .problems
        .into_iter()
        .partition(|problem| problem.outside_the_change);
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
            stated: problem.stated,
        })
        .collect();
    (moved, inside)
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
/// sentence a check prints. Only a service this run re-scanned (`rescanned`)
/// can stop the check: its own ask failed. A mark stored on a service the run
/// carried is read by [`library_marked`] instead (carrick#4807). A
/// not-signed-in mark counts only while this machine still holds no
/// credential: a service carried from an earlier unsigned `refresh` keeps its
/// mark until the next refresh, and must not outlive the sign-in.
pub(super) fn library_unavailable<'a>(
    rebuilt: &super::read_model::LocalIndex,
    repos: impl IntoIterator<Item = &'a String>,
    rescanned: &std::collections::BTreeSet<String>,
) -> Option<&'static str> {
    use crate::library_store::AskFailure;
    let repos: std::collections::BTreeSet<&String> = repos.into_iter().collect();
    let failures: Vec<AskFailure> = rebuilt
        .repos
        .iter()
        .filter(|repo| repos.contains(&repo.name))
        .flat_map(|repo| repo.services.iter())
        .filter(|service| rescanned.contains(&service.name))
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

/// The services of `rebuilt` this run carried whose stored boundary says their
/// library rows are missing from the index (carrick#4807). A pair with one of
/// them on either side is not judged: its library rows are not there to pair
/// against. A re-scanned service's own failed ask stops the check instead
/// ([`library_unavailable`]), and the sign-in rule is that function's.
pub(super) fn library_marked(
    rebuilt: &super::read_model::LocalIndex,
    rescanned: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeSet<String> {
    use crate::library_store::AskFailure;
    let signed_out = crate::credentials::CloudAuth::detect().is_err();
    rebuilt
        .repos
        .iter()
        .flat_map(|repo| repo.services.iter())
        .filter(|service| !rescanned.contains(&service.name))
        .filter(|service| {
            match service
                .boundary
                .as_ref()
                .and_then(|boundary| boundary.library_ask_failed)
            {
                Some(AskFailure::Unreachable) => true,
                Some(AskFailure::NotSignedIn) => signed_out,
                None => false,
            }
        })
        .map(|service| service.name.clone())
        .collect()
}

/// What a pair says when a service on one side has no library rows in the
/// index: not a mismatch, not a match, and not a missing route.
pub(super) fn library_missing_reason(service: &str) -> String {
    format!("not checked: {service} was indexed without its library facts.")
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
    /// The services this run asked the library store for, as opposed to the
    /// ones it carried: only their own failed ask stops a check
    /// ([`library_unavailable`]).
    pub rescanned: std::collections::BTreeSet<String>,
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
///
/// `index` is what the check read. With `current` (per repo, the services
/// the reads-the-same walk named, where it ran), a repo whose every reached
/// service is current ([`Copies::current`]), and none of whose other
/// services the re-scan would read again ([`rescan_rereads_beyond`]), is
/// joined from the kept copies without a scan. The answer stands only when
/// the join settled every pair it read; otherwise the whole pass runs again
/// as it would with no copy at all. It keeps no copy of its own. The second
/// value names the repos joined from copies.
pub(super) fn rejoin(
    workspace: &Workspace,
    index: &super::read_model::LocalIndex,
    (repos, current): (
        &[Tree],
        Option<&std::collections::BTreeMap<String, Vec<String>>>,
    ),
    generation: &Path,
    deadline: Instant,
    narrator: Option<&Narrator>,
    index_generation: &str,
) -> Result<(Rejoined, std::collections::BTreeSet<String>), String> {
    let began = Instant::now();
    let held = Held::read(workspace, index)?;
    took("held-read", began);
    let began = Instant::now();
    let hosted = held.hosted.source_key();
    let mut sets = std::collections::BTreeMap::new();
    for (tree, walked) in repos
        .iter()
        .filter_map(|tree| Some((tree, current?.get(&repo_label(&tree.repo)))))
    {
        let name = repo_label(&tree.repo);
        let Some(indexed) = index.repos.iter().find(|repo| repo.name == name) else {
            continue;
        };
        // A cold check reads no digest and walks nothing of its own.
        if !Copies::any_kept(workspace, index_generation, &indexed.services) {
            continue;
        }
        let mut repo_copies = Copies::of(
            &tree.checkout,
            &indexed.services,
            index_generation,
            None,
            hosted.clone(),
        );
        // Only a repo whose every edited service has a current copy can be
        // current. Then the services with edits of their own are walked, and
        // the rest are read from the walk reads-the-same already ran, where
        // it ran: a service with no edits of its own that reads a changed
        // file reads an outside one.
        if !repo_copies.worth_walking(workspace) {
            continue;
        }
        let began = Instant::now();
        let others = walked.is_none();
        let mut readers: std::collections::BTreeSet<String> = readers_of_the_edit(
            &tree.checkout,
            indexed
                .services
                .iter()
                .filter(|service| others || repo_copies.digests.contains_key(&service.name)),
            &held,
        )
        .into_keys()
        .collect();
        readers.extend(
            walked
                .into_iter()
                .flatten()
                .filter(|service| !repo_copies.digests.contains_key(*service))
                .cloned(),
        );
        repo_copies.readers = readers;
        repo_copies.walked = true;
        took("check/current-copies/readers-walk", began);
        let reached = repo_copies.reached();
        let Some(set) = repo_copies.current(workspace, &reached) else {
            continue;
        };
        let began = Instant::now();
        let rereads = rescan_rereads_beyond(&held, tree, &reached);
        took("check/current-copies/stored-gate", began);
        if !rereads {
            sets.insert(name, set);
        }
    }
    took("check/current-copies", began);
    if sets.is_empty() {
        let rejoined = rejoin_held(
            workspace,
            held,
            repos,
            generation,
            deadline,
            narrator,
            &Rescans::default(),
            index_generation,
        )?;
        return Ok((rejoined, Default::default()));
    }
    let how = Rescans {
        current: sets
            .iter()
            .map(|(name, set)| (name.clone(), set.as_slice()))
            .collect(),
        ..Rescans::default()
    };
    if let Ok(rejoined) = rejoin_held(
        workspace,
        held,
        repos,
        generation,
        deadline,
        narrator,
        &how,
        index_generation,
    ) {
        return Ok((rejoined, sets.into_keys().collect()));
    }
    // The copies did not stand in: the pass runs as it would without them.
    // A repo the first attempt scanned is scanned again inside the same
    // deadline: reading its finished scan back is `rejoin_held`'s to offer.
    let _ = std::fs::remove_dir_all(generation);
    let held = Held::read(workspace, index)?;
    let rejoined = rejoin_held(
        workspace,
        held,
        repos,
        generation,
        deadline,
        narrator,
        &Rescans::default(),
        index_generation,
    )?;
    Ok((rejoined, Default::default()))
}

/// Whether the `--changed-only` scan of `tree` would read again a service
/// outside `reached`, the services the edits reach: the change scope's gate
/// on each one's stored blob ([`crate::change_scope::stored_gate`]: no blob,
/// another cache, read from uncommitted changes, owed work, another config),
/// asked as the scan's child asks it, of the services it resolves against
/// the same previous list. Such a repo is re-scanned: its copies would be
/// joined beside a blob the re-scan would not keep. `true` when the services
/// cannot be resolved. The per-edit re-check does not ask it: its scan is
/// told its list and carries every other service whatever its blob says.
fn rescan_rereads_beyond(held: &Held, tree: &Tree, reached: &[String]) -> bool {
    let name = repo_label(&tree.repo);
    let previous = previous_list(
        held.hosted.local_blobs(&tree.repo),
        held.local.iter().filter(|blob| blob.repo_name == name),
    );
    let Ok(configs) =
        crate::service_derivation::resolve_with_hosted(&tree.checkout, Some(&previous))
    else {
        return true;
    };
    // The re-scan is a facts-only pass (`rescan_command`): it asks no model.
    let this_run = crate::change_scope::ThisRun {
        scanner_version: env!("CARGO_PKG_VERSION"),
        cache_version: crate::engine::CACHE_VERSION,
        asks_model: false,
    };
    configs.iter().any(|config| {
        let stored = previous
            .iter()
            .find(|blob| blob.service_name == config.service_name);
        let id = stored
            .map(service_id)
            .or_else(|| config.service_name.clone())
            .unwrap_or_else(|| name.clone());
        !reached.contains(&id)
            && crate::change_scope::stored_gate(
                stored,
                this_run,
                serde_json::to_string(config).ok().as_deref(),
            )
            .is_err()
    })
}

/// What the last index held for one repo, as its re-scan reads it
/// (`previous`): the hosted snapshot's copy of each service, and the local
/// blob of every service the snapshot does not hold.
fn previous_list<'a>(
    hosted: Vec<crate::cloud_storage::CloudRepoData>,
    local: impl Iterator<Item = &'a crate::cloud_storage::CloudRepoData>,
) -> Vec<crate::cloud_storage::CloudRepoData> {
    let hosted_ids: std::collections::BTreeSet<String> = hosted.iter().map(service_id).collect();
    let mut previous = hosted;
    previous.extend(
        local
            .filter(|blob| !hosted_ids.contains(&service_id(blob)))
            .cloned(),
    );
    previous
}

/// What one [`rejoin_held`] does besides re-scanning each repo it is handed.
#[derive(Default)]
struct Rescans<'a> {
    /// The services the scan is told to analyse: the per-edit re-check's.
    /// `None` leaves the scan's own change scope to decide.
    listed: Option<&'a [String]>,
    /// The newest known copy of a service outside the scope, standing in for
    /// its indexed blob in every repo's previous list.
    stand_in: Option<&'a std::collections::BTreeMap<String, crate::cloud_storage::CloudRepoData>>,
    /// Per repo, by label: what its scan keeps for the next check. Only the
    /// per-edit re-check keeps, under its `listed` services.
    keep: std::collections::BTreeMap<String, &'a Copies>,
    /// Per repo, by label: the blobs that stand in for its scan. Such a repo
    /// is not scanned, and the answer stands only when the join settled every
    /// pair; otherwise the rejoin fails and the caller re-scans.
    current: std::collections::BTreeMap<String, &'a [crate::cloud_storage::CloudRepoData]>,
}

/// Why a rejoin from kept copies was not kept.
const NOT_CURRENT: &str = "a kept copy did not stand in for the re-scan";

/// Whether a join over kept copies stands for the re-scan. It cannot retype
/// a pair (only a consumer a scan read can be), so it stands only when it
/// ran the type check and settled every pair it read: no count (the settle
/// off, or only verifying) is not that.
fn copies_stand(type_check: &super::JoinTypeCheck, settle_misses: Option<usize>) -> bool {
    *type_check == super::JoinTypeCheck::Ran && settle_misses == Some(0)
}

/// [`rejoin`], over an index already read. Both checks keep the type checker's
/// answers per index and build (`index_generation`, the index's `indexed_at`),
/// so a run that moves no type finds the index's own answer, or the last run's,
/// in `.carrick/judged`.
///
/// `how` says what else the pass does ([`Rescans`]): the per-edit re-check
/// narrows the re-scan to the services it names and stands the newest known
/// copy in for the rest, and keeps what its scan read; both checks may join a
/// repo from its current copies instead of scanning it.
#[allow(clippy::too_many_arguments)]
fn rejoin_held(
    workspace: &Workspace,
    held: Held,
    repos: &[Tree],
    generation: &Path,
    deadline: Instant,
    narrator: Option<&Narrator>,
    how: &Rescans,
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
    let mut rescanned = std::collections::BTreeSet::new();

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
        let hosted_data = hosted.local_blobs(&tree.repo);
        write_json(&staged(generation, "hosted", position), &hosted_data)?;
        let mut previous_data = previous_list(
            hosted_data,
            indexed.iter().filter(|blob| blob.repo_name == name),
        );
        // A service outside the scope stands in at the copy its last re-check
        // left, so this one judges against the edits already made to it.
        if let Some(stand_in) = how.stand_in {
            for blob in &mut previous_data {
                if let Some(newer) = stand_in.get(&service_id(blob)) {
                    *blob = newer.clone();
                }
            }
        }
        // A repo joined from its current copies: each copy takes its
        // service's place, as a scan that wrote them would leave it. A copy
        // of a service the list does not hold is not that.
        for newer in how.current.get(&name).copied().unwrap_or_default() {
            let id = service_id(newer);
            let place = previous_data
                .iter_mut()
                .find(|blob| service_id(blob) == id)
                .ok_or_else(|| NOT_CURRENT.to_string())?;
            *place = newer.clone();
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
        // The rows the last `refresh` (or an earlier check, below) kept stand
        // in for a walk of a carried service. The scan only reads them; the
        // walks it had to do are kept after it succeeds.
        let kept = workspace
            .index_dir()
            .join(crate::position_rows::POSITIONS_DIR);
        // A repo joined from its current copies starts no scan: its copies
        // already stand in its previous list, and it writes nothing fresh.
        let fresh = if how.current.contains_key(&name) {
            Vec::new()
        } else {
            let mut scan = rescan_command(&exe, tree, &scan_dir, &previous);
            scan.env(JUDGE_ANSWERS_ENV, &judged);
            if let Some(listed) = how.listed {
                scan.env(
                    crate::change_scope::RECHECK_SERVICES_ENV,
                    serde_json::to_string(listed).map_err(|e| e.to_string())?,
                );
            }
            scan.env(crate::position_rows::KEPT_DIR_ENV, &kept);
            if let Some(narrator) = narrator {
                // The scan states its per-service counts on stderr only when
                // a parent asks; the narrator is that parent here.
                scan.env(crate::progress::PROGRESS_ENV, "1");
                narrator.repo(Instant::now(), &name, position + 1, repos.len());
            }
            took("scan-starts", Instant::now());
            let began = Instant::now();
            bounded(scan, "re-scan", deadline, narrator)?;
            took("scan", began);
            let began = Instant::now();
            let fresh = read_blobs(&scan_dir)?;
            if let (Some(copies), Some(listed)) = (how.keep.get(&name), how.listed) {
                copies.keep(workspace, &fresh, listed);
            }
            took("after-scan/read-fresh-and-keep", began);
            fresh
        };
        let began = Instant::now();
        let hosted_data = read_staged(&staged(generation, "hosted", position))?;
        let previous_data = read_staged(&previous)?;
        took("after-scan/read-staged-lists", began);
        // A service this run asked the library store for: a fresh blob the
        // scan analysed, not one it only wrote as it carried it.
        rescanned.extend(
            fresh
                .iter()
                .filter(|blob| {
                    how.listed
                        .is_none_or(|listed| listed.contains(&service_id(blob)))
                })
                .filter(|blob| {
                    !previous_data
                        .iter()
                        .any(|stored| written_as_carried(blob, stored))
                })
                .map(service_id),
        );
        let began = Instant::now();
        let carried =
            super::index::carried_services(&tree.checkout, &hosted_data, &previous_data, &fresh)?;
        // A check writes kept state under `.carrick` (carrick#3245): the rows
        // of the carried services it walked, bound on use by build stamp,
        // commit and every file's hash ([`crate::position_rows::kept_rows`]),
        // so the next check reads them instead of walking again. Never an
        // analysed service's rows.
        // The scan also writes a carried service whose pairs the join moved, so
        // it is in `fresh`, not `carried`, though it was walked as a carried
        // one: with a re-check's list, every written service it did not list
        // is one of those. `--changed` names no list, so it keeps only `carried`.
        let written_carried: Vec<crate::cloud_storage::CloudRepoData> = carried
            .iter()
            .chain(fresh.iter().filter(|blob| {
                how.listed
                    .is_some_and(|listed| !listed.contains(&service_id(blob)))
            }))
            .cloned()
            .collect();
        super::index::keep_carried_walks(&scan_dir, &kept, &written_carried);
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
    if !how.current.is_empty() && !copies_stand(&joined.type_check, joined.settle_misses) {
        return Err(NOT_CURRENT.to_string());
    }

    // The same fold the indexer performs, over the same shapes, so a re-checked
    // row and an indexed row can never be built two different ways.
    let index = build(workspace, &blobs, &joined, &remote_services)?;
    took("fold", began);
    Ok(Rejoined {
        index,
        join: joined,
        services: services.len(),
        rescanned,
    })
}

/// Whether `written` is `stored` carried by a `--changed-only` scan and
/// written because its join moved: the same blob with this run's verdicts,
/// SDK edges and stamps (`change_scope::refreshed_carried`). A service the scan
/// analysed states its own run, so it differs. Used to tell a mark this run's
/// ask wrote from a mark the stored blob already held (carrick#4807).
fn written_as_carried(
    written: &crate::cloud_storage::CloudRepoData,
    stored: &crate::cloud_storage::CloudRepoData,
) -> bool {
    if service_id(written) != service_id(stored) {
        return false;
    }
    let (Ok(mut written), Ok(mut stored)) =
        (serde_json::to_value(written), serde_json::to_value(stored))
    else {
        return false;
    };
    for blob in [&mut written, &mut stored] {
        let Some(blob) = blob.as_object_mut() else {
            return false;
        };
        for key in [
            "compat_verdicts",
            "pair_outcomes",
            "sdk_edges",
            "sdk_unresolved",
            "commit_hash",
            "dirty",
            "last_updated",
            "scanner_build",
        ] {
            blob.remove(key);
        }
        if let Some(boundary) = blob.get_mut("boundary").and_then(|b| b.as_object_mut()) {
            for key in ["commit_hash", "files_attempted", "sdk_unresolved"] {
                boundary.remove(key);
            }
        }
    }
    written == stored
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
            similar: Vec::new(),
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
    /// carrick#3948: the re-check says whether the compiler engine was ready
    /// from the re-scan's own record: not found (`spawn`) and past its ready
    /// budget (`init`) are not ready; an engine that died while reading
    /// (`resolve`) or no record at all is.
    #[test]
    fn a_re_scan_is_engine_ready_unless_it_records_spawn_or_init() {
        fn index(
            boundary: Option<crate::boundary::ServiceBoundary>,
        ) -> super::super::read_model::LocalIndex {
            let mut index: super::super::read_model::LocalIndex =
                serde_json::from_value(serde_json::json!({
                    "version": 1, "scanner_version": "0", "indexed_at": "", "repos": [{
                        "path": "/w/web", "name": "web",
                        "services": [{
                            "name": "web", "commit": "", "indexed_at": "", "boundary": null,
                            "routes": 0, "calls": 0
                        }],
                        "files": {}
                    }]
                }))
                .expect("an index");
            index.repos[0].services[0].boundary = boundary;
            index
        }
        let degraded = |stage: &str| {
            index(Some(crate::boundary::ServiceBoundary {
                types_degraded: Some(crate::cloud_storage::TypeDegradation {
                    stage: stage.to_string(),
                    detail: "x".to_string(),
                }),
                ..Default::default()
            }))
        };
        let read = vec!["web".to_string()];
        assert!(!engine_ready_of(&degraded("spawn"), "web", &read));
        assert!(!engine_ready_of(&degraded("init"), "web", &read));
        assert!(engine_ready_of(&degraded("resolve"), "web", &read));
        assert!(engine_ready_of(&index(None), "web", &read));
        assert!(engine_ready_of(
            &index(Some(Default::default())),
            "web",
            &read
        ));
        assert!(
            engine_ready_of(&degraded("spawn"), "another-repo", &read),
            "another repo's record says nothing about this re-scan"
        );
        assert!(
            engine_ready_of(&degraded("spawn"), "web", &["other".to_string()]),
            "a service this re-check did not re-read says nothing about it"
        );
    }

    use super::*;

    /// A route row of the edited file, at line 12, as the rebuilt index holds
    /// it: a type mismatch, for the dispatch case `dispatch`.
    fn mismatch_row(dispatch: Option<&str>) -> IndexedItem {
        serde_json::from_value(serde_json::json!({
            "kind": "route", "service": "api", "key": "http|POST|/orders", "dispatch": dispatch,
            "method": "POST", "path": "/orders", "line": 12, "col": null, "source": "fact",
            "resolution_source": null, "evidence": null, "counterparts": [],
            "verdict": {"state": "resolved", "result": "type_mismatch", "detail": "differs"}
        }))
        .expect("an item")
    }

    /// A call row at line 30 with no counterpart and no verdict.
    fn unmatched_call() -> IndexedItem {
        serde_json::from_value(serde_json::json!({
            "kind": "call", "service": "api", "key": "http|GET|/gone", "dispatch": null,
            "method": "GET", "path": "/gone", "line": 30, "col": null, "source": "fact",
            "resolution_source": null, "evidence": null, "counterparts": [], "verdict": null
        }))
        .expect("an item")
    }

    /// The judge's problem for one of those rows.
    fn judged_problem(
        kind: super::super::changed::ProblemKind,
        item: &IndexedItem,
        stated: bool,
    ) -> super::super::changed::Problem {
        super::super::changed::Problem {
            kind,
            file: "api/orders.ts".to_string(),
            line: item.line,
            role: item.kind.as_str().to_string(),
            method: item.method.clone(),
            path: item.path.clone(),
            other: Vec::new(),
            reason: "no service in this workspace serves it".to_string(),
            source: "fact".to_string(),
            descriptor: false,
            already_indexed: !stated,
            unconfirmed: false,
            outside_the_change: false,
            not_judged_before: false,
            moved_outside: false,
            after: String::new(),
            other_build: false,
            fails: stated,
            stated,
            dispatch: item.dispatch.clone(),
        }
    }

    /// The edited file's items carry the judge's `stated`, matched by kind,
    /// line, method, path and dispatch case: a stated mismatch is marked, one
    /// the judge did not state (already in the index, or model-only) is not,
    /// and a sibling dispatch case at the same line, method and path is not
    /// marked by its sibling's problem. A call no service serves gets its
    /// `no_route` verdict and the mark, as before.
    #[test]
    fn state_judged_marks_only_the_rows_the_judge_stated() {
        use super::super::changed::ProblemKind;
        let case_a = mismatch_row(Some("{\"field\":\"type\",\"value\":\"a\"}"));
        let case_b = mismatch_row(Some("{\"field\":\"type\",\"value\":\"b\"}"));
        let plain = mismatch_row(None);
        let call = unmatched_call();
        let mut items = vec![case_a.clone(), case_b.clone(), call.clone()];
        let inside = vec![
            judged_problem(ProblemKind::Incompatible, &case_a, true),
            judged_problem(ProblemKind::Incompatible, &case_b, false),
            judged_problem(ProblemKind::NoRoute, &call, true),
        ];
        state_judged(&mut items, &inside);
        assert!(items[0].stated, "case a: {:?}", items[0]);
        assert!(!items[1].stated, "case b: {:?}", items[1]);
        assert!(items[2].stated, "call: {:?}", items[2]);
        let verdict = items[2].verdict.as_ref().expect("a no_route verdict");
        assert_eq!(verdict.result.as_deref(), Some("no_route"));

        // A problem the judge did not state leaves its row unmarked, and an
        // unstated no-route call gets no verdict.
        let mut held = vec![plain.clone(), call.clone()];
        state_judged(
            &mut held,
            &[
                judged_problem(ProblemKind::Incompatible, &plain, false),
                judged_problem(ProblemKind::NoRoute, &call, false),
            ],
        );
        assert!(!held[0].stated && !held[1].stated, "{held:?}");
        assert!(held[1].verdict.is_none(), "{:?}", held[1]);
    }

    /// carrick#4773: a compatible row whose counterpart sits in a changed,
    /// not re-read service names it and stops reading as confirmed; a row
    /// whose counterparts are all current is untouched. A socket listener and
    /// a mismatch are covered: the rule reads services, not protocols.
    #[test]
    fn state_stale_counterparts_marks_only_rows_paired_with_a_stale_service() {
        let paired = |kind: &str, result: &str, service: &str| -> IndexedItem {
            serde_json::from_value(serde_json::json!({
                "kind": kind, "service": "hub", "key": "socket|CLIENT->SERVER|tick", "dispatch": null,
                "method": "CLIENT->SERVER", "path": "tick", "line": 5, "col": null, "source": "fact",
                "resolution_source": null, "evidence": null,
                "counterparts": [{"role": "consumer", "service": service, "file": "a.ts", "line": 1}],
                "verdict": {"state": "resolved", "result": result, "detail": "both sides agree"}
            }))
            .expect("an item")
        };
        let stale = vec!["emitter".to_string()];
        let mut items = vec![
            paired("route", "compatible", "emitter"),
            paired("route", "compatible", "steady"),
            paired("route", "type_mismatch", "emitter"),
        ];
        let before = items[1].clone();
        state_stale_counterparts(&mut items, &stale);
        let marked = items[0].verdict.as_ref().expect("a verdict");
        assert_eq!(marked.state, "not_checked");
        assert_eq!(marked.result.as_deref(), Some("compatible"));
        assert_eq!(
            marked.detail,
            "both sides agree (emitter has changed since it was indexed and this check did not \
             re-read it, so the verdict uses its indexed code. `carrick check --changed` gives \
             the current verdict.)"
        );
        assert_eq!(items[1].verdict, before.verdict, "a current counterpart");
        let mismatch = items[2].verdict.as_ref().expect("a verdict");
        assert_eq!(mismatch.state, "resolved", "a mismatch stays a finding");
        assert!(
            mismatch
                .detail
                .contains("`carrick check --changed` gives the current verdict.")
        );
    }

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

    fn svc(name: &str) -> Owner {
        Owner::Service(name.to_string())
    }

    /// A miss is remembered per service and per index generation: another
    /// service of the same repo, or the same service against a newer index,
    /// tries the re-check again (carrick#3198).
    #[test]
    fn a_timed_out_re_check_remembers_only_the_edited_files_service() {
        let plan = Plan {
            generation: "g1".to_string(),
            services: vec!["types-owner".to_string(), "big-reader".to_string()],
            copies: Copies::default(),
            working: Default::default(),
            current: None,
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
        let recorded = plan.miss_of(&svc("types-owner"), TEN_S, TEN_S);
        assert_eq!(
            recorded,
            Some(Miss {
                elapsed_ms: 10_000,
                budget_ms: 10_000,
                files: 3
            }),
            "a reader re-read beside the edited service is tried again on its own edit"
        );
    }

    #[test]
    fn a_worktrees_miss_never_holds_for_main_and_the_reverse() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("recheck-misses.json");
        let worktree = dir.path().join("wt");
        let other = dir.path().join("other-wt");
        std::fs::create_dir_all(&worktree).expect("a worktree folder");
        std::fs::create_dir_all(&other).expect("a second worktree folder");

        ServiceMisses::record(&path, "g1", &svc("big"), miss(40), Some(&worktree));
        let read = ServiceMisses::read(&path);
        assert!(
            read.holds("g1", &svc("big"), TEN_S, None).is_none(),
            "a worktree's miss must not switch main off"
        );
        assert_eq!(
            read.holds("g1", &svc("big"), TEN_S, Some(&worktree)),
            Some(&miss(40))
        );
        assert!(
            read.holds("g1", &svc("big"), TEN_S, Some(&other)).is_none(),
            "nor another worktree"
        );

        let main_only = dir.path().join("main-only.json");
        ServiceMisses::record(&main_only, "g1", &svc("big"), miss(40), None);
        let read = ServiceMisses::read(&main_only);
        assert!(
            read.holds("g1", &svc("big"), TEN_S, Some(&worktree))
                .is_none()
        );
        assert!(read.holds("g1", &svc("big"), TEN_S, None).is_some());
        assert!(
            !std::fs::read_to_string(&main_only)
                .expect("the file")
                .contains("checkouts"),
            "main's file is unchanged when only main missed"
        );
    }

    #[test]
    fn a_service_miss_holds_for_its_service_and_generation_only() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("recheck-misses.json");

        assert!(
            ServiceMisses::read(&path)
                .holds("g1", &svc("big"), TEN_S, None)
                .is_none()
        );
        ServiceMisses::record(&path, "g1", &svc("big"), miss(40), None);
        ServiceMisses::record(&path, "g1", &svc("big"), miss(40), None);
        let read = ServiceMisses::read(&path);
        assert_eq!(read.holds("g1", &svc("big"), TEN_S, None), Some(&miss(40)));
        assert_eq!(read.services.len(), 1, "recorded once");
        assert!(
            read.holds("g1", &svc("small"), TEN_S, None).is_none(),
            "a sibling service of the same repo still re-checks"
        );
        assert!(
            read.holds("g2", &svc("big"), TEN_S, None).is_none(),
            "a new index tries again"
        );

        // A miss against a newer index replaces the older generation's.
        ServiceMisses::record(&path, "g2", &svc("small"), miss(3), None);
        let read = ServiceMisses::read(&path);
        assert!(read.holds("g2", &svc("small"), TEN_S, None).is_some());
        assert!(read.holds("g2", &svc("big"), TEN_S, None).is_none());
        assert!(read.holds("g1", &svc("big"), TEN_S, None).is_none());

        // No generation means no record either way.
        ServiceMisses::record(&path, "", &svc("big"), miss(40), None);
        assert!(
            ServiceMisses::read(&path)
                .holds("", &svc("big"), TEN_S, None)
                .is_none()
        );

        // The file the repo-wide rule wrote reads as no misses.
        std::fs::write(&path, r#"{"generation":"g2","repos":["/w/big"]}"#).unwrap();
        assert!(
            ServiceMisses::read(&path)
                .holds("g2", &svc("big"), TEN_S, None)
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
        ServiceMisses::record(&path, "g1", &svc("big"), miss(40), None);
        let read = ServiceMisses::read(&path);
        assert!(read.holds("g1", &svc("big"), TEN_S, None).is_some());
        assert!(
            read.holds("g1", &svc("big"), Duration::from_millis(5_000), None)
                .is_some()
        );
        assert!(
            read.holds("g1", &svc("big"), long, None).is_none(),
            "a miss at 10 s says nothing about a call with 180 s"
        );
        let at_cap = Miss {
            budget_ms: 180_000,
            ..miss(40)
        };
        ServiceMisses::record(&path, "g1", &svc("big"), at_cap, None);
        assert!(
            ServiceMisses::read(&path)
                .holds("g1", &svc("big"), long, None)
                .is_some()
        );
        assert!(
            ServiceMisses::read(&path)
                .holds("g1", &svc("big"), TEN_S, None)
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
                .holds("g1", &svc("big"), TEN_S, None)
                .is_none()
        );
    }

    /// The reason a skipped re-check gives names the service, how much of it
    /// the index holds, and the command that gives a fresh answer.
    #[test]
    fn the_miss_reason_names_the_service_and_the_fresh_read() {
        let reason = miss_reason(&svc("catalog"), &miss(12));
        assert!(
            reason.contains("re-reading catalog (12 indexed files)"),
            "{reason}"
        );
        assert!(reason.contains("took longer than 10 s"), "{reason}");
        assert!(reason.contains("check --changed"), "{reason}");
    }

    fn shared() -> Owner {
        Owner::File {
            repo: "shop".to_string(),
            file: "shared/types.ts".to_string(),
        }
    }

    /// A shop of three services at one commit: catalog and ledger include
    /// `shared/`, inventory does not.
    fn shop() -> IndexedRepo {
        let service = |name: &str, include: serde_json::Value| {
            serde_json::json!({
                "name": name, "directory": name, "include": include,
                "commit": "c1", "dirty": false, "indexed_at": "t",
                "routes": 0, "calls": 0,
            })
        };
        serde_json::from_value(serde_json::json!({
            "name": "shop",
            "path": "/w/shop",
            "services": [
                service("catalog", serde_json::json!(["shared"])),
                service("inventory", serde_json::json!([])),
                service("ledger", serde_json::json!(["shared"])),
            ],
            "files": {},
        }))
        .expect("an indexed repo")
    }

    /// A file in no service's directory re-reads the services the walk names,
    /// in index order, and every service when the walk could not say.
    #[test]
    fn a_shared_edit_re_reads_its_readers_in_index_order_and_every_service_when_unknown() {
        let repo = shop();
        let reach = Reach::for_edit(
            &repo,
            "shared/types.ts",
            None,
            Some(&["ledger".to_string(), "catalog".to_string()]),
        )
        .expect("a reach");
        assert_eq!(reach.reread, vec!["catalog", "ledger"]);
        assert_eq!(reach.owner, shared());
        let reach = Reach::for_edit(&repo, "shared/types.ts", None, None).expect("a reach");
        assert_eq!(reach.reread, vec!["catalog", "inventory", "ledger"]);
    }

    #[test]
    fn rows_never_make_a_service_the_owner_of_a_file_outside_its_directory() {
        let repo = shop();
        let catalog = repo.services[0].clone();
        let reach = Reach::for_edit(
            &repo,
            "shared/types.ts",
            Some(&catalog),
            Some(&["catalog".to_string(), "ledger".to_string()]),
        )
        .expect("a reach");
        assert_eq!(reach.owner, shared());
        let reach =
            Reach::for_edit(&repo, "catalog/app/x.ts", Some(&catalog), None).expect("a reach");
        assert_eq!(reach.owner, svc("catalog"));
        assert_eq!(reach.reread, vec!["catalog"]);
    }

    #[test]
    fn a_shared_files_miss_holds_for_that_file_and_silences_no_service() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("recheck-misses.json");
        ServiceMisses::record(&path, "g1", &shared(), miss(5), None);
        let read = ServiceMisses::read(&path);
        assert_eq!(read.holds("g1", &shared(), TEN_S, None), Some(&miss(5)));
        let other = Owner::File {
            repo: "shop".to_string(),
            file: "shared/other.ts".to_string(),
        };
        assert!(read.holds("g1", &other, TEN_S, None).is_none());
        assert!(read.holds("g1", &svc("catalog"), TEN_S, None).is_none());
        assert!(read.holds("g2", &shared(), TEN_S, None).is_none());
        assert!(read.services.is_empty());
    }

    #[test]
    fn a_timed_out_re_check_of_a_shared_file_is_remembered_for_the_file() {
        let plan = Plan {
            generation: "g1".to_string(),
            services: vec!["catalog".to_string(), "ledger".to_string()],
            copies: Copies::default(),
            working: Default::default(),
            current: None,
            files: [("catalog".to_string(), 3), ("ledger".to_string(), 2)]
                .into_iter()
                .collect(),
            scope: RecheckScope {
                reread: vec!["catalog".to_string(), "ledger".to_string()],
                not_reread: Vec::new(),
                stale: Vec::new(),
            },
        };
        assert_eq!(
            plan.miss_of(&shared(), TEN_S, TEN_S),
            Some(Miss {
                elapsed_ms: 10_000,
                budget_ms: 10_000,
                files: 5
            })
        );
    }

    #[test]
    fn the_miss_reason_for_a_shared_file_names_the_file_and_the_fresh_read() {
        let reason = miss_reason(&shared(), &miss(5));
        assert!(reason.contains("shared/types.ts"), "{reason}");
        assert!(reason.contains("carrick refresh"), "{reason}");
        assert!(reason.contains("carrick check --changed"), "{reason}");
    }

    #[test]
    fn a_miss_file_without_files_still_holds_its_service_misses() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("recheck-misses.json");
        std::fs::write(
            &path,
            r#"{"generation":"g1","services":{"big":{"elapsed_ms":10000,"budget_ms":10000,"files":40}}}"#,
        )
        .unwrap();
        assert!(
            ServiceMisses::read(&path)
                .holds("g1", &svc("big"), TEN_S, None)
                .is_some()
        );
    }

    #[test]
    fn a_repo_with_no_indexed_service_has_no_reach() {
        let mut repo = shop();
        repo.services.clear();
        assert!(Reach::for_edit(&repo, "x.ts", None, None).is_none());
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

    /// The hosted snapshot every copy below was read with.
    const SNAPSHOT: &str = "snapshot-one";

    /// A committed repo of two services, `api` and `web`, and a workspace
    /// beside it (so its kept state is not an untracked file of the repo).
    /// `web_commit` is the commit `web` was indexed at.
    struct TwoServices {
        repo: tempfile::TempDir,
        _root: tempfile::TempDir,
        workspace: Workspace,
        services: Vec<IndexedService>,
    }

    fn two_services(web_commit: Option<&str>) -> TwoServices {
        let (repo, head) = crate::git_state::tests::committed_repo(&[
            ("api/a.ts", "export const a = 1;\n"),
            ("web/b.ts", "export const b = 1;\n"),
        ]);
        let root = tempfile::tempdir().unwrap();
        let workspace = Workspace {
            root: root.path().to_path_buf(),
            repos: Vec::new(),
            missing: Vec::new(),
            repos_detected_by: String::new(),
            repos_added: Vec::new(),
            repos_excluded: Vec::new(),
            parent_proposal: None,
        };
        let service = |name: &str, commit: &str| -> IndexedService {
            serde_json::from_value(serde_json::json!({
                "name": name, "directory": name, "commit": commit, "dirty": false,
                "indexed_at": "t", "routes": 0, "calls": 0,
            }))
            .expect("an indexed service")
        };
        TwoServices {
            services: vec![
                service("api", &head),
                service("web", web_commit.unwrap_or(&head)),
            ],
            repo,
            _root: root,
            workspace,
        }
    }

    impl TwoServices {
        fn write(&self, relative: &str, text: &str) {
            std::fs::write(self.repo.path().join(relative), text).unwrap();
        }

        fn copies(&self) -> Copies {
            Copies::of(
                self.repo.path(),
                &self.services,
                "g1",
                Some(Default::default()),
                Some(SNAPSHOT.to_string()),
            )
        }

        fn kept(&self, copies: &Copies, service: &str) -> Option<WorkingCopy> {
            read_working(&self.workspace, "g1", service, &copies.digests[service])
        }

        /// What a re-check of `service` kept for `copies`' digest, carrying
        /// `carried`.
        fn keep_copy(
            &self,
            copies: &Copies,
            service: &str,
            carried: Vec<crate::cloud_storage::CloudRepoData>,
        ) {
            keep_working(
                &self.workspace,
                "g1",
                &WorkingCopy {
                    digest: copies.digests[service].clone(),
                    whole: true,
                    hosted: Some(SNAPSHOT.to_string()),
                    blob: blob_of(service, "abc"),
                    carried,
                },
            );
        }
    }

    fn blob_of(service: &str, commit: &str) -> crate::cloud_storage::CloudRepoData {
        serde_json::from_value(serde_json::json!({
            "repo_name": "repo", "service_name": service, "endpoints": [], "calls": [],
            "mounts": [], "apps": {}, "imported_handlers": [], "function_definitions": {},
            "last_updated": "2026-11-15T10:00:00Z", "commit_hash": commit, "file_results": {},
        }))
        .expect("a blob")
    }

    /// PR #4634 review, B4: the re-check of `web` also writes `api` (its join moved
    /// `api`'s pair outcomes) without analysing it. `api`'s copy from its own
    /// earlier re-check is older than that: it is kept, for the re-check to
    /// judge against, but marked not whole, so no copy-only answer reads it.
    #[test]
    fn a_written_edited_service_the_scan_did_not_analyse_keeps_a_copy_that_never_stands_in() {
        let two = two_services(None);
        two.write("api/a.ts", "export const a = 2;\n");
        two.write("web/b.ts", "export const b = 2;\n");
        let copies = two.copies();
        two.keep_copy(&copies, "api", Vec::new());
        let api = ["api".to_string()];
        assert!(
            copies.current(&two.workspace, &api).is_some(),
            "api's own copy stands in"
        );

        copies.keep(
            &two.workspace,
            &[blob_of("web", "abc"), blob_of("api", "abc")],
            &["web".to_string()],
        );
        let older = two.kept(&copies, "api").expect("api's older copy is kept");
        assert!(!older.whole, "marked not whole");
        assert!(
            copies.current(&two.workspace, &api).is_none(),
            "so it stands in for no re-scan"
        );
        assert!(two.kept(&copies, "web").is_some(), "web's is kept");
    }

    /// PR #4634 review, N1: an edit made while the re-check's scan ran keeps no
    /// copy, so undoing it leaves nothing labelled with the undone digest
    /// that holds the edit. Edits that held still keep one.
    #[test]
    fn a_copy_is_kept_only_when_its_own_edits_held_still_through_the_scan() {
        let two = two_services(None);
        two.write("api/a.ts", "export const a = 2;\n");
        let copies = two.copies();
        two.write("api/a.ts", "export const a = 3;\n");
        copies.keep(
            &two.workspace,
            &[blob_of("api", "abc")],
            &["api".to_string()],
        );
        two.write("api/a.ts", "export const a = 2;\n");
        assert!(
            two.kept(&copies, "api").is_none(),
            "edited during the scan: no copy"
        );

        copies.keep(
            &two.workspace,
            &[blob_of("api", "abc")],
            &["api".to_string()],
        );
        assert!(two.kept(&copies, "api").is_some(), "held still: kept");
    }

    /// A repo where git cannot say what changed in one service is never
    /// current, whatever copies the others hold.
    #[test]
    fn a_repo_with_a_service_git_cannot_list_is_never_current() {
        let two = two_services(Some("0000000000000000000000000000000000000000"));
        two.write("api/a.ts", "export const a = 2;\n");
        let copies = two.copies();
        assert!(copies.unlisted.contains("web"), "git cannot list web");
        two.keep_copy(&copies, "api", Vec::new());
        assert!(
            copies
                .current(&two.workspace, &["api".to_string()])
                .is_none()
        );
    }

    /// One service carried differently by two copies is not known: nothing
    /// is current. Carried the same, both copies stand in.
    #[test]
    fn two_copies_that_carry_one_service_differently_are_not_current() {
        let two = two_services(None);
        two.write("api/a.ts", "export const a = 2;\n");
        two.write("web/b.ts", "export const b = 2;\n");
        let copies = two.copies();
        let both = ["api".to_string(), "web".to_string()];
        two.keep_copy(&copies, "api", vec![blob_of("lib", "one")]);
        two.keep_copy(&copies, "web", vec![blob_of("lib", "two")]);
        assert!(copies.current(&two.workspace, &both).is_none());

        two.keep_copy(&copies, "web", vec![blob_of("lib", "one")]);
        let set = copies
            .current(&two.workspace, &both)
            .expect("carried the same, they stand in");
        assert_eq!(set.len(), 3, "api, web and the one lib");
    }

    /// The join over copies stands only when it ran the type check and
    /// settled every pair: a skipped type check, no settle count (the settle
    /// off or verifying) or a pair left to judge re-scans.
    #[test]
    fn copies_stand_only_on_a_type_check_that_settled_every_pair() {
        use super::super::JoinTypeCheck;
        assert!(copies_stand(&JoinTypeCheck::Ran, Some(0)));
        assert!(!copies_stand(&JoinTypeCheck::Ran, Some(1)));
        assert!(!copies_stand(&JoinTypeCheck::Ran, None));
        let skipped = JoinTypeCheck::Skipped {
            reason: "the compiler engine is unavailable".to_string(),
        };
        assert!(!copies_stand(&skipped, Some(0)));
    }
}

#[cfg(test)]
#[path = "recheck_similar_tests.rs"]
mod similar_tests;
