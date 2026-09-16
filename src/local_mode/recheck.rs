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
    pub ran: Ran,
    pub elapsed: Duration,
}

/// Why a re-check did not produce one.
pub struct Degraded {
    pub reason: String,
    pub elapsed: Duration,
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
pub fn run(workspace: &Workspace, repo: &Path, relative: &str) -> Result<Fresh, Degraded> {
    let started = Instant::now();
    let deadline = started + budget();
    let generation = std::env::temp_dir().join(format!("carrick-recheck-{}", uuid::Uuid::new_v4()));
    let outcome = inside(workspace, repo, relative, &generation, deadline);
    // The temporary generation goes on every path, including the one where a
    // phase was killed at the deadline: a hook that leaves a directory behind
    // per edit is a hook that fills a disk in an afternoon.
    let _ = std::fs::remove_dir_all(&generation);
    match outcome {
        Ok(fresh) => Ok(Fresh {
            elapsed: started.elapsed(),
            ..fresh
        }),
        Err(reason) => Err(Degraded {
            reason,
            elapsed: started.elapsed(),
        }),
    }
}

fn inside(
    workspace: &Workspace,
    repo: &Path,
    relative: &str,
    generation: &Path,
    deadline: Instant,
) -> Result<Fresh, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("no carrick binary to re-scan with: {e}"))?;
    let blobs = generation.join("repos");
    std::fs::create_dir_all(&blobs).map_err(|e| format!("{}: {e}", blobs.display()))?;

    // Everything except the edited repo, exactly as the last index left it.
    let name = repo_label(repo);
    let retained = read_blobs(&workspace.blobs_dir())?;
    for (position, blob) in retained
        .iter()
        .filter(|blob| blob.repo_name != name)
        .enumerate()
    {
        write_blob(&blobs.join(format!("retained-{position}.json")), blob)?;
    }

    // The hosted services, from the snapshot on disk. No request is made: a
    // re-check that waited on the network would miss its budget on the wire
    // rather than on the work.
    let hosted = super::hosted::cached(workspace);
    let mut remote_services = std::collections::BTreeMap::new();
    for (position, (remote, blob)) in hosted.remote_blobs().into_iter().enumerate() {
        remote_services.insert(service_id(&blob), remote);
        write_blob(&blobs.join(format!("hosted-{position}.json")), &blob)?;
    }

    // Phase 1. `previous_data` is what the last index held for this repo, so
    // an unchanged file replays its hosted answers instead of losing them.
    let previous = generation.join("previous.json");
    std::fs::write(
        &previous,
        serde_json::to_vec(&hosted.local_blobs(repo)).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let scan_dir = generation.join("scan");
    let mut scan = scan_command(&exe, repo, &scan_dir, &previous, &super::index::Pass::Facts);
    scan.env(super::SKIP_SIGNATURES_ENV, "1")
        .env_remove(crate::progress::PROGRESS_ENV);
    bounded(scan, "re-scan", deadline)?;
    for (position, blob) in read_blobs(&scan_dir)?.iter().enumerate() {
        write_blob(&blobs.join(format!("local-{position}.json")), blob)?;
    }

    // Phase 2. The join reads the blobs back and runs the type check over
    // them; it analyses no source, so its cost is the workspace's pairs.
    let out = generation.join("join.json");
    let mut join = join_command(&exe, repo, &blobs, &out);
    join.env_remove(crate::progress::PROGRESS_ENV);
    bounded(join, "re-join", deadline)?;
    let text = std::fs::read_to_string(&out)
        .map_err(|e| format!("the re-join wrote no result to {}: {e}", out.display()))?;
    let joined: super::LocalJoin =
        serde_json::from_str(&text).map_err(|e| format!("could not read the re-join: {e}"))?;

    // The same fold the indexer performs, over the same shapes, so a re-checked
    // row and an indexed row can never be built two different ways.
    let rebuilt = build(workspace, &blobs, &joined, &remote_services)?;
    let items = rebuilt
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
        ran,
        elapsed: Duration::default(),
    })
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
            // Killing the scan closes the pipes the sidecar it spawned reads,
            // which is what ends that process too; waiting for the child here
            // is what keeps it from being reaped by the shell later.
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("the {what} ran past the budget"));
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
