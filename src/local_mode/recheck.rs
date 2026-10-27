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
//! 1. **One repo, re-scanned.** The same subprocess `index` runs per repo, with
//!    no model, no upload and no network: deterministic extraction plus the
//!    sidecar's type capture, over the working tree. The blob the last index
//!    wrote for that repo is handed in as `previous_data`, so the hosted model
//!    answers for the files the edit did not touch replay exactly as they do on
//!    an incremental scan.
//! 2. **The join, over blobs.** Every other service comes from the blobs
//!    already on disk (`.carrick/repos`) and the cached hosted snapshot; the
//!    join reads them, runs the v2 type check, and hands back the same
//!    [`LocalJoin`] the indexer folds. Nothing is downloaded and nothing is
//!    written outside a temporary directory this module deletes.
//!
//! It also answers with the functions the edited file declares that the index
//! does not hold, which is what the end-of-task reuse nudge speaks from
//! (carrick#1330; `docs/reference/task-skills.md`, "The reuse nudge"). Those
//! come from one parse of the file before either phase starts, and are kept on
//! a miss too (carrick#2849): on a large monorepo the re-scan alone outlasts
//! the budget, and the nudge must not depend on it.
//!
//! A deadline covers both phases. On a miss the caller keeps the indexed
//! answer and says how old it is; the re-check never blocks an edit and never
//! leaves a scan running behind one.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::index::{build, join_command, read_blobs, repo_label, scan_command, service_id};
use super::read_model::IndexedItem;
use super::workspace::Workspace;

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
    /// Functions the re-extracted file declares that the blobs the index holds
    /// do not (carrick#1330), in line order.
    pub new_functions: Vec<super::contract::NewFunction>,
    pub ran: Ran,
    pub elapsed: Duration,
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
    /// The re-scan was not started, because an earlier one of this repo
    /// missed its budget against the same index ([`KnownMisses`]).
    pub known_miss: bool,
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

/// Re-extract `relative` in `repo`, re-join it against every blob the index
/// already holds, and answer with the rows the working tree states.
///
/// `index_generation` names the index this runs against (its `indexed_at`):
/// a re-scan of `repo` that missed its budget against it is remembered there,
/// and every later call against the same generation skips the re-scan and
/// answers at once ([`KnownMisses`]).
pub fn run(
    workspace: &Workspace,
    repo: &Path,
    relative: &str,
    index_generation: &str,
) -> Result<Fresh, Degraded> {
    let started = Instant::now();
    let deadline = started + budget();
    // The index is read once, here, and named twice: for what the edit added
    // and for the re-join. What the edit added does not wait on the re-scan
    // (carrick#2849): on a large repo the re-scan alone outlasts the budget,
    // and the reuse nudge would then never hear of a function the edit wrote.
    let held = match Held::read(workspace) {
        Ok(held) => held,
        Err(reason) => {
            return Err(Degraded {
                reason,
                new_functions: Vec::new(),
                elapsed: started.elapsed(),
                known_miss: false,
            });
        }
    };
    let new_functions = added_functions(&held, repo, relative);
    let misses = KnownMisses::path(workspace);
    if KnownMisses::read(&misses).holds(index_generation, repo) {
        return Err(Degraded {
            reason: KNOWN_MISS_REASON.to_string(),
            new_functions,
            elapsed: started.elapsed(),
            known_miss: true,
        });
    }
    let generation = std::env::temp_dir().join(format!("carrick-recheck-{}", uuid::Uuid::new_v4()));
    let outcome = inside(workspace, held, repo, relative, &generation, deadline);
    // The temporary generation goes on every path, including the one where a
    // phase was killed at the deadline: a hook that leaves a directory behind
    // per edit is a hook that fills a disk in an afternoon.
    let _ = std::fs::remove_dir_all(&generation);
    match outcome {
        Ok(fresh) => Ok(Fresh {
            new_functions,
            elapsed: started.elapsed(),
            ..fresh
        }),
        Err(reason) => {
            // Only a miss of the budget is remembered: a phase that failed
            // for another reason before the deadline says nothing about
            // whether this repo fits, and remembering it would switch the
            // re-check off on a repo where it does.
            if Instant::now() >= deadline {
                KnownMisses::record(&misses, index_generation, repo);
            }
            Err(Degraded {
                reason,
                new_functions,
                elapsed: started.elapsed(),
                known_miss: false,
            })
        }
    }
}

/// The reason a re-check that skipped its re-scan gives.
pub const KNOWN_MISS_REASON: &str = "an earlier re-check of this repo ran past its budget against \
     this index, so this one did not start a re-scan; per-edit verdicts here are the index's \
     until the next `carrick refresh`, and `carrick check --changed` gives fresh ones";

/// The repos whose re-check missed its budget against one index generation
/// (`.carrick/recheck-misses.json`).
///
/// On a large monorepo the re-scan alone outlasts the budget every time, so
/// every edit would wait out the whole budget for an answer that never comes.
/// The first miss is remembered here, keyed to the index it ran against; until
/// `index` or `refresh` writes a new one, a re-check of that repo skips the
/// re-scan and answers at once with the parse and the indexed verdicts. A new
/// generation does not match, so the re-check is tried again after it. A repo
/// that fits never misses and is never written here. The reader-facing side
/// (`known_miss` on the `recheck` block) is `docs/local-mode-output.md`.
#[derive(serde::Serialize, serde::Deserialize, Default, Debug, PartialEq, Eq)]
struct KnownMisses {
    /// The `indexed_at` of the index the misses were against.
    generation: String,
    /// Absolute repo paths, as the workspace names them.
    repos: Vec<String>,
}

impl KnownMisses {
    fn path(workspace: &Workspace) -> std::path::PathBuf {
        workspace.index_dir().join("recheck-misses.json")
    }

    /// Unreadable or absent is "no misses": a lost record costs one more
    /// re-check, never a wrong answer.
    fn read(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn holds(&self, generation: &str, repo: &Path) -> bool {
        !generation.is_empty()
            && self.generation == generation
            && self.repos.iter().any(|known| Path::new(known) == repo)
    }

    /// Add `repo` for `generation`, dropping the misses of any older one.
    fn record(path: &Path, generation: &str, repo: &Path) {
        if generation.is_empty() {
            return;
        }
        let mut misses = Self::read(path);
        if misses.generation != generation {
            misses = Self {
                generation: generation.to_string(),
                repos: Vec::new(),
            };
        }
        let repo = repo.to_string_lossy().to_string();
        if !misses.repos.contains(&repo) {
            misses.repos.push(repo);
        }
        if let Ok(text) = serde_json::to_string_pretty(&misses) {
            let _ = std::fs::write(path, text);
        }
    }
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
fn added_functions(held: &Held, repo: &Path, relative: &str) -> Vec<super::contract::NewFunction> {
    let name = repo_label(repo);
    let hosted = held.hosted.local_blobs(repo);
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
    named_additions(&services, repo, relative, &indexed)
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
fn named_additions(
    services: &[crate::config::Config],
    repo: &Path,
    relative: &str,
    indexed: &std::collections::BTreeSet<String>,
) -> Vec<super::contract::NewFunction> {
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
    let stated = crate::pr_functions::stated_in(Path::new(relative), relative.to_string(), source);
    new_functions(
        indexed,
        stated.into_values().filter(|function| !function.anonymous),
    )
}

fn inside(
    workspace: &Workspace,
    held: Held,
    repo: &Path,
    relative: &str,
    generation: &Path,
    deadline: Instant,
) -> Result<Fresh, String> {
    let name = repo_label(repo);
    let rejoined = rejoin_held(workspace, held, &[repo], generation, deadline)?;

    let items = rejoined
        .index
        .repos
        .iter()
        .find(|rebuilt| rebuilt.name == name)
        .and_then(|rebuilt| rebuilt.files.get(relative))
        .cloned()
        .unwrap_or_default();
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
        // Filled in by `run`, from the parse that did not wait on this.
        new_functions: Vec::new(),
        ran,
        elapsed: Duration::default(),
    })
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
pub(super) fn rejoin(
    workspace: &Workspace,
    repos: &[&Path],
    generation: &Path,
    deadline: Instant,
) -> Result<Rejoined, String> {
    rejoin_held(
        workspace,
        Held::read(workspace)?,
        repos,
        generation,
        deadline,
    )
}

/// [`rejoin`], over an index already read.
fn rejoin_held(
    workspace: &Workspace,
    held: Held,
    repos: &[&Path],
    generation: &Path,
    deadline: Instant,
) -> Result<Rejoined, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("no carrick binary to re-scan with: {e}"))?;
    let blobs = generation.join("repos");
    std::fs::create_dir_all(&blobs).map_err(|e| format!("{}: {e}", blobs.display()))?;
    let names: std::collections::BTreeSet<String> =
        repos.iter().map(|repo| repo_label(repo)).collect();
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

    // Phase 1, one scan per repo, analysing only the services whose inputs
    // changed (`--changed-only`, carrick#2826). `previous` is what the last
    // index held for that repo: the hosted snapshot's copy of each service,
    // and the local blob of every service the snapshot does not hold. It is
    // what an unchanged file replays its hosted answers from, and what the
    // change scope carries an untouched service from.
    for (position, repo) in repos.iter().enumerate() {
        let name = repo_label(repo);
        let mut previous_data = hosted.local_blobs(repo);
        let hosted_data = previous_data.clone();
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
        let previous = generation.join(format!("previous-{position}.json"));
        std::fs::write(
            &previous,
            serde_json::to_vec(&previous_data).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let scan_dir = generation.join(format!("scan-{position}"));
        let scan = rescan_command(&exe, repo, &scan_dir, &previous);
        bounded(scan, "re-scan", deadline)?;
        let fresh = read_blobs(&scan_dir)?;
        let carried = super::index::carried_services(repo, &hosted_data, &previous_data, &fresh)?;
        for (offset, blob) in fresh.iter().chain(&carried).enumerate() {
            services.insert(service_id(blob));
            write_blob(&blobs.join(format!("local-{position}-{offset}.json")), blob)?;
        }
    }

    // Phase 2. The join reads the blobs back and runs the type check over
    // them; it analyses no source, so its cost is the workspace's pairs. The
    // repo it is pointed at only names the run: every blob is in `blobs`.
    let anchor = repos.first().copied().unwrap_or(&workspace.root);
    let out = generation.join("join.json");
    let mut join = join_command(&exe, anchor, &blobs, &out);
    join.env_remove(crate::progress::PROGRESS_ENV);
    bounded(join, "re-join", deadline)?;
    let text = std::fs::read_to_string(&out)
        .map_err(|e| format!("the re-join wrote no result to {}: {e}", out.display()))?;
    let joined: super::LocalJoin =
        serde_json::from_str(&text).map_err(|e| format!("could not read the re-join: {e}"))?;

    // The same fold the indexer performs, over the same shapes, so a re-checked
    // row and an indexed row can never be built two different ways.
    let index = build(workspace, &blobs, &joined, &remote_services)?;
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
fn rescan_command(exe: &Path, repo: &Path, scan_dir: &Path, previous: &Path) -> Command {
    let mut scan = scan_command(
        exe,
        repo,
        scan_dir,
        previous,
        None,
        &super::index::Pass::Facts,
    );
    scan.arg("--changed-only")
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
/// next one before anybody looked (carrick#748). Output is dropped rather than
/// rendered — a read-only command prints its answer on stdout and nothing else,
/// and this one runs behind an edit where a spinner has no reader.
fn bounded(mut command: Command, what: &str, deadline: Instant) -> Result<(), String> {
    if Instant::now() >= deadline {
        return Err(format!("no budget left for the {what}"));
    }
    let mut child = command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start the {what}: {e}"))?;
    // A signal that ends this command reaches the phase too (carrick#1379).
    super::index::waiting_on(Some(child.id()));
    let outcome = wait_within(&mut child, what, deadline);
    super::index::waiting_on(None);
    outcome
}

/// Wait for `child` until `deadline`, and stop it there.
fn wait_within(
    child: &mut std::process::Child,
    what: &str,
    deadline: Instant,
) -> Result<(), String> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!("the {what} exited {:?}", status.code()));
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
            Path::new("/repo"),
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
            crate::engine::service_workers::width_with(5, Some(12_452), 12, Some(3072)),
            2
        );
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
        let error = bounded(command, "re-scan", Instant::now()).unwrap_err();
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
        assert!(bounded(command, "re-scan", Instant::now() + Duration::from_secs(30)).is_ok());
    }

    #[test]
    fn a_phase_that_fails_says_what_it_exited_with() {
        let command = Command::new("false");
        let error = bounded(command, "re-scan", Instant::now() + Duration::from_secs(30))
            .expect_err("a failing phase is not a success");
        assert!(error.contains("exited"), "{error}");
    }

    /// A miss is remembered per repo and per index generation: another repo,
    /// or the same repo against a newer index, tries the re-check again.
    #[test]
    fn a_known_miss_holds_for_its_repo_and_generation_only() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("recheck-misses.json");
        let big = Path::new("/w/big");
        let small = Path::new("/w/small");

        assert!(!KnownMisses::read(&path).holds("g1", big), "nothing yet");
        KnownMisses::record(&path, "g1", big);
        KnownMisses::record(&path, "g1", big);
        let read = KnownMisses::read(&path);
        assert!(read.holds("g1", big));
        assert_eq!(read.repos.len(), 1, "recorded once");
        assert!(!read.holds("g1", small), "a sibling repo still re-checks");
        assert!(!read.holds("g2", big), "a new index tries again");

        // A miss against a newer index replaces the older generation's.
        KnownMisses::record(&path, "g2", small);
        let read = KnownMisses::read(&path);
        assert!(read.holds("g2", small));
        assert!(!read.holds("g2", big));
        assert!(!read.holds("g1", big));

        // No generation means no record either way.
        KnownMisses::record(&path, "", big);
        assert!(!KnownMisses::read(&path).holds("", big));
    }

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn names(found: Vec<super::super::contract::NewFunction>) -> Vec<String> {
        found.into_iter().map(|f| f.name).collect()
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
                names(named_additions(&services, dir.path(), file, &none)),
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
            names(named_additions(&services, dir.path(), "src/a.ts", &indexed)),
            vec!["added".to_string()]
        );
    }
}
