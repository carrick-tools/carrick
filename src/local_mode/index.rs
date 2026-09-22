//! `carrick index` and `carrick refresh`: build the local read model.
//!
//! Two phases, the same two the offline cross-repo harness uses, driven as
//! subprocesses of this binary:
//!
//! 1. **Per repo, isolated.** Each repo is scanned on its own with the
//!    cross-repo download forced empty, so no sibling's data reaches a repo's
//!    own scan. Hosted answers are handed in only as previous_data. This is
//!    the slow phase, and it is where `refresh` does its work for one service.
//! 2. **Join.** One more run reads every blob back, builds the analyzer over
//!    all of them, runs the type check, and writes what it found to
//!    a temporary join file inside the current build directory.
//!
//! The indexer then folds the blobs (boundaries, commits) and the join
//! (operations, edges, verdicts) into `.carrick/index.json`. Read-only commands
//! consume it after checking the local credential identity.
//!
//! Subprocesses rather than in-process calls for the same reason the harness
//! uses them: a scan keeps process-global state (the health counters, the
//! sidecar it spawns for one repo's tsconfig), and five repos in one process
//! would share it.

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::cloud_storage::CloudRepoData;

use super::join::{JoinedFinding, JoinedMatch, JoinedOperation, LocalJoin, Role};
use super::read_model::{
    Counterpart, IndexedItem, IndexedRepo, IndexedService, ItemKind, LocalIndex,
    READ_MODEL_VERSION, Source, StoredVerdict,
};
use super::workspace::Workspace;

/// What one `index` or `refresh` did, for the summary the command prints.
pub struct IndexOutcome {
    pub index: LocalIndex,
    pub scanned: Vec<String>,
    pub elapsed_secs: f64,
    /// What the hosted read downloaded and what it reused unchanged. `None`
    /// when no hosted metadata was read at all.
    pub hosted_download: Option<String>,
    /// The repos a `--dispatch` build asked to hand over and did not, and why
    /// (carrick#1251). Empty on every other pass.
    pub not_dispatched: Vec<(String, crate::progress::NotDispatched)>,
    /// What the scans left for a later run, in their own sentences. Empty on
    /// a build that finished everything it started (carrick#1315).
    pub pending: Vec<String>,
    /// Where this build's wall clock went, summed over every repo it scanned
    /// and the join over them. Printed, and kept so the next build's opening
    /// line is a measurement rather than a guess (carrick#1452).
    pub timing: crate::scan_timing::Split,
}

/// What one repo's scan is doing on a resume: where its collected answers are,
/// and whether the index it builds is worth uploading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resumption {
    /// The answer bundle this repo's scan joins by content. `None` for a repo
    /// that was never handed over: it has no job to collect and is scanned the
    /// ordinary way, because the index the resume writes covers the workspace
    /// and a repo left out of it reads as a repo with nothing in it.
    pub answers: Option<PathBuf>,
    /// The cloud already serves a newer index for this repo — CI indexed it
    /// while the job ran. The scan still builds the local read model, because
    /// that is what `carrick check` answers from, but it does not replace a
    /// newer stored index with an older one (carrick#1229, R8).
    pub superseded: bool,
}

/// Which of the four passes a build is.
///
/// They differ in what each repo's scan subprocess is asked to do, and nothing
/// else; every one of them ends in the same join and the same read model,
/// except the one that deliberately does not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pass {
    /// `refresh`: deterministic rows only. Asks no model and pays for nothing.
    Facts,
    /// `carrick index`: the inferred scan.
    Infer,
    /// `carrick index --dispatch`: build every prompt, hand them to Carrick
    /// Cloud as a job, and write no index. The answers arrive later.
    Dispatch,
    /// `carrick resume`: the inferred scan with a job's answers in hand. Files
    /// whose content still matches take their answer from the job; anything
    /// that has changed since is analysed now.
    Resume(BTreeMap<PathBuf, Resumption>),
}

impl Pass {
    /// Whether this pass asks the model anything at all.
    pub fn infers(&self) -> bool {
        !matches!(self, Pass::Facts)
    }

    /// Whether what this pass takes is what the next `carrick index` should be
    /// told to expect (carrick#1452).
    ///
    /// Only the inferred full-workspace pass. `refresh` asks no model, so its
    /// wait is a fraction of the one being estimated; `--dispatch` writes no
    /// index and does none of the model's work here; and a `resume` scans the
    /// repos whose answers came back and leaves the rest alone, so its counts
    /// cover part of the tree. Each of those would leave the next run quoting
    /// a time for work it is not about to do.
    pub fn measures_the_tree(&self) -> bool {
        matches!(self, Pass::Infer)
    }
}

/// What a build produced: an index, or a set of jobs somebody will collect.
pub enum Built {
    Indexed(Box<IndexOutcome>),
    /// Nothing was indexed because the analysis is happening elsewhere. One
    /// entry per repo handed over.
    Dispatched {
        jobs: Vec<crate::analysis_job::Dispatched>,
        /// The repos in the same build that were not handed over, and why.
        /// They wrote blobs this build throws away, so they are scanned again
        /// by the `carrick resume` that collects the jobs (carrick#1251).
        not_dispatched: Vec<(String, crate::progress::NotDispatched)>,
    },
}

/// Index every repo in the workspace, or re-index the one holding `only`.
///
/// An inferring pass turns each per-repo scan into a laptop scan: model
/// analysis through Carrick Cloud, an upload, and the same payload written
/// into this build's cache directory. The join phase never infers — it
/// analyses nothing, it reads blobs back.
pub fn run(workspace: &Workspace, only: Option<&str>, pass: &Pass) -> Result<Built, String> {
    let started = Instant::now();
    // Build into a fresh generation. A failed scan leaves the last complete
    // read model intact; account changes never inherit previous local rows.
    let requested = only
        .map(|name| repo_for_service(workspace, &workspace.blobs_dir(), name))
        .transpose()?;
    // The word `carrick status` said for the last scan is history the moment
    // this build starts: whatever it wrote, this run is writing a newer index
    // (carrick#1007 item 4).
    super::scan_state::forget_finished(&workspace.index_dir());
    let hosted = super::hosted::refresh(workspace);
    let previous = LocalIndex::read(&workspace.index_file()).ok();
    let same_sources = previous.as_ref().is_some_and(|index| {
        index.hosted_source_key == hosted.source_key()
            && std::fs::read(workspace.index_dir().join("blobs-source.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Option<String>>(&bytes).ok())
                == Some(index.hosted_source_key.clone())
            && index
                .repos
                .iter()
                .map(|r| PathBuf::from(&r.path))
                .collect::<Vec<_>>()
                == workspace.repos
    });
    let targets = match pass {
        // A resume scans what the caller listed and nothing else: the repos
        // whose answers are in hand, and the repos that were never handed
        // over. A repo whose job is still running is absent from the map and
        // is not analysed here, which is the whole reason it was handed over.
        Pass::Resume(resuming) => workspace
            .repos
            .iter()
            .filter(|repo| resuming.contains_key(*repo))
            .cloned()
            .collect(),
        _ if same_sources => requested
            .map(|path| vec![path])
            .unwrap_or_else(|| workspace.repos.clone()),
        _ => workspace.repos.clone(),
    };
    let generation = workspace
        .index_dir()
        .join(format!("build-{}", uuid::Uuid::new_v4()));
    let blobs = generation.join("repos");
    std::fs::create_dir_all(&blobs).map_err(|e| format!("{}: {e}", blobs.display()))?;
    super::workspace::write_self_ignore(&workspace.index_dir())
        .map_err(|e| format!("could not write the .carrick/.gitignore: {e}"))?;
    let result = run_generation(
        workspace,
        &hosted,
        &generation,
        &blobs,
        &targets,
        started,
        pass,
    );
    let _ = std::fs::remove_dir_all(&generation);
    result
}

#[allow(clippy::too_many_arguments)]
fn run_generation(
    workspace: &Workspace,
    hosted: &super::hosted::HostedInput,
    generation: &Path,
    blobs: &Path,
    targets: &[PathBuf],
    started: Instant,
    pass: &Pass,
) -> Result<Built, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("could not find the carrick binary to run a scan with: {e}"))?;

    // A scoped refresh can retain unrequested local scans only when the
    // workspace paths and authenticated hosted sources are unchanged.
    let retained = if targets.len() < workspace.repos.len() {
        read_blobs(&workspace.blobs_dir())?
    } else {
        Vec::new()
    };
    for (position, blob) in retained
        .into_iter()
        .filter(|b| {
            workspace
                .repos
                .iter()
                .any(|p| repo_label(p) == b.repo_name && !targets.contains(p))
        })
        .enumerate()
    {
        std::fs::write(
            blobs.join(format!("retained-{position}.json")),
            serde_json::to_vec(&blob).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    let mut scanned = Vec::new();
    // The repos this run scanned WITH the model and uploaded. A laptop scan
    // that returns here has written its blob to the cloud — the tee propagates
    // the cloud's error, so a refused upload fails the scan — and that is the
    // one fact the pre-scan hosted snapshot cannot know (carrick#1007 item 1).
    let mut uploaded: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    // The receipt, written as each block lands rather than at the end: a run
    // killed after an upload has still uploaded that repo, and this is the
    // record of it (carrick#995). Nothing printed reads it; `carrick status
    // --json` carries it for whoever parses that (carrick#1236).
    let mut spend = crate::scan_spend::RunSpend::default();
    // The repos whose analysis is happening in the cloud. A build with any of
    // these writes no index: it has nothing to write one from, and a thinner
    // index over the top of the last one would read as an answer.
    let mut handed_over: Vec<(PathBuf, crate::analysis_job::Dispatched)> = Vec::new();
    // And the repos this build asked to hand over that did not. A warm cache
    // is the ordinary case for every scan after the first, so this is the
    // ordinary outcome of `--dispatch` — and it used to be silent, which is
    // indistinguishable from the flag doing nothing (carrick#1251).
    let mut not_dispatched: Vec<(String, crate::progress::NotDispatched)> = Vec::new();
    // And what they left behind, so the build's closing line can say it
    // (carrick#1315).
    let mut pending: Vec<String> = Vec::new();
    // Where this build's wall clock went, summed as each scan reports its own
    // (carrick#1452). The reader waited on all of them, so the record the next
    // build quotes is the whole build and not one repo of it.
    let mut timing = crate::scan_timing::Split::default();
    // How many services this build is about, and how many are behind it, so
    // the one line a reader is watching counts the workspace rather than
    // restarting at each repo (carrick#1365). Best effort: the same resolution
    // `carrick derive` runs, and a repo it cannot resolve leaves every child
    // counting its own services, as they did.
    let service_counts = workspace_service_counts(targets);
    let mut services_done = 0usize;
    for (position, repo) in targets.iter().enumerate() {
        let name = repo_label(repo);
        let workspace_position = service_counts
            .as_ref()
            .map(|counts| (services_done, counts.iter().sum::<usize>()));
        if let Some(counts) = &service_counts {
            services_done += counts.get(position).copied().unwrap_or(0);
        }
        let previous = generation.join("previous.json");
        std::fs::write(
            &previous,
            serde_json::to_vec(&hosted.local_blobs(repo)).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        // Scanner cache filenames derive from repo/service labels. Keep each
        // scanner in its own directory so user labels cannot overwrite a
        // retained or hosted blob in the join input.
        let scan_dir = generation.join(format!("scan-{position}"));
        let report = scan_repo(
            &exe,
            repo,
            &scan_dir,
            &previous,
            &name,
            pass,
            workspace_position,
        )?;
        pending.extend(report.pending.iter().cloned());
        if let Some(split) = &report.timing {
            timing.add(split);
        }
        // A repo that handed its prompts over wrote no blob. One that was
        // asked to and found nothing for the model says nothing here: it
        // indexed itself, in the seconds it takes to state facts nobody has to
        // be asked about.
        if let Some(dispatched) = report.dispatched {
            handed_over.push((repo.clone(), dispatched));
            continue;
        }
        if let Some(reason) = report.not_dispatched {
            not_dispatched.push((name.clone(), reason));
        }
        // "Uploaded" drives the enrichment this repo's services are shown
        // with, so a resume that deliberately kept its index to itself must
        // not claim one.
        let kept_local = match pass {
            Pass::Resume(resuming) => resuming.get(repo).is_some_and(|r| r.superseded),
            _ => false,
        };
        if pass.infers() && !kept_local {
            uploaded.insert(repo.clone());
        }
        if let Some(paid) = report.spend {
            spend.record(&name, paid);
            spend.write(&workspace.last_scan_file());
            // And into the detached build's own state file, so the one
            // artefact a killed scan leaves behind says what it had spent.
            super::scan_state::spent(&spend);
        }
        for (service, blob) in read_blobs(&scan_dir)?.into_iter().enumerate() {
            std::fs::write(
                blobs.join(format!("local-{position}-{service}.json")),
                serde_json::to_vec(&blob).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        }
        scanned.push(name);
    }

    // Handed over, so there is nothing here to join: the answers are being
    // produced elsewhere and the index arrives when they are collected.
    if !handed_over.is_empty() {
        for (repo, dispatched) in &handed_over {
            super::jobs::record(
                &workspace.index_dir(),
                super::jobs::Job {
                    repo: dispatched.repo.clone(),
                    path: repo.to_string_lossy().into_owned(),
                    job_id: dispatched.job_id.clone(),
                    commit: dispatched.commit.clone(),
                    analyze_rows: dispatched.analyze_rows,
                    submitted_at: chrono::Utc::now().to_rfc3339(),
                },
            )?;
        }
        return Ok(Built::Dispatched {
            jobs: handed_over
                .into_iter()
                .map(|(_, dispatched)| dispatched)
                .collect(),
            not_dispatched,
        });
    }

    // Hosted-only repositories enter the existing matcher/type judge, but
    // have no local repo in build() and therefore cannot create local items.
    let mut remote_services = BTreeMap::new();
    let mut service_names = std::collections::HashSet::new();
    for blob in read_blobs(blobs)? {
        let service = service_id(&blob);
        if !service_names.insert(service.clone()) {
            return Err(format!(
                "local service identity '{service}' is ambiguous in this workspace"
            ));
        }
    }
    for (position, (remote, blob)) in hosted.remote_blobs().into_iter().enumerate() {
        let service = service_id(&blob);
        if !service_names.insert(service.clone()) {
            return Err(format!(
                "hosted service identity '{service}' is ambiguous in this workspace"
            ));
        }
        remote_services.insert(service, remote);
        std::fs::write(
            blobs.join(format!("hosted-{position}.json")),
            serde_json::to_vec(&blob).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    let join_target = workspace
        .repos
        .first()
        .ok_or_else(|| "the workspace resolved to no repos".to_string())?;
    // The join reads every blob back and asks nobody anything, so its wait is
    // part of the local read the next build's opening line quotes.
    let join_started = Instant::now();
    let join = join(&exe, join_target, blobs, &generation.join("join.json"))?;
    timing.local_secs += join_started.elapsed().as_secs_f64();

    let mut index = build(workspace, blobs, &join, &remote_services)?;
    index.hosted_checked_at = hosted.checked_at();
    index.hosted_source_key = hosted.source_key();
    (index.hosted_identity, index.hosted_workspace) = hosted.identity();
    let scanned_blobs = read_blobs(blobs)?;
    for repo in &mut index.repos {
        for service in &mut repo.services {
            if let Some(blob) = scanned_blobs
                .iter()
                .find(|b| b.repo_name == repo.name && service_id(b) == service.name)
            {
                let path = PathBuf::from(&repo.path);
                service.enrichment = if uploaded.contains(&path) {
                    hosted.uploaded(&path, blob)
                } else {
                    hosted.service(&path, blob)
                };
            }
        }
        for item in repo.files.values_mut().flatten() {
            for counterpart in &mut item.counterparts {
                counterpart.remote = remote_services.get(&counterpart.service).cloned();
            }
        }
    }
    // Keep only local blobs in the persistent repo directory. Hosted inputs
    // remain in their credential-bound snapshot, never an implicit join seed.
    let persistent = workspace.blobs_dir();
    let source_marker = workspace.index_dir().join("blobs-source.json");
    match std::fs::remove_file(&source_marker) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    if persistent.exists() {
        std::fs::remove_dir_all(&persistent).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(&persistent).map_err(|e| e.to_string())?;
    for (position, blob) in scanned_blobs
        .iter()
        .filter(|b| {
            !remote_services.contains_key(&service_id(b))
                && index.repos.iter().any(|r| r.name == b.repo_name)
        })
        .enumerate()
    {
        std::fs::write(
            persistent.join(format!("{position}.json")),
            serde_json::to_vec(blob).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    index
        .write(&workspace.index_file())
        .map_err(|e| format!("could not write {}: {e}", workspace.index_file().display()))?;
    // The marker is written last. An interrupted replacement cannot combine a
    // new account's read model with the preceding account's retained blobs.
    std::fs::write(
        source_marker,
        serde_json::to_vec(&index.hosted_source_key).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    // The hand-off has been folded in; leaving it would invite a reader to
    // treat a stale copy of the join as the index.
    let _ = std::fs::remove_file(workspace.join_file());

    Ok(Built::Indexed(Box::new(IndexOutcome {
        index,
        scanned,
        elapsed_secs: started.elapsed().as_secs_f64(),
        hosted_download: hosted.download_line(),
        not_dispatched,
        pending,
        timing,
    })))
}

/// The repo holding a named service, for `refresh --service`. The blobs know
/// which repo each service belongs to, so a refresh names a service and the
/// index answers with its repo.
fn repo_for_service(workspace: &Workspace, blobs: &Path, service: &str) -> Result<PathBuf, String> {
    let known = read_blobs(blobs)?;
    let owner = known
        .iter()
        .find(|blob| service_id(blob) == service)
        .map(|blob| blob.repo_name.clone());
    let Some(repo_name) = owner else {
        let names: Vec<String> = known.iter().map(service_id).collect();
        return Err(format!(
            "no indexed service named '{service}'. This workspace holds: {}",
            if names.is_empty() {
                "nothing yet — run `carrick index` first".to_string()
            } else {
                names.join(", ")
            }
        ));
    };
    workspace
        .repos
        .iter()
        .find(|repo| repo_label(repo) == repo_name)
        .cloned()
        .ok_or_else(|| {
            format!("service '{service}' was indexed from repo '{repo_name}', which the workspace no longer lists")
        })
}

/// How many services each repo of this build declares, or None.
///
/// The same resolution the proposal is derived from, which reads manifests and
/// globs and parses nothing: a `carrick.json`, an npm or pnpm workspace, or a
/// single service. All or nothing — a total that is missing a repo is a total
/// the count would overshoot, and no count is better than a wrong one.
fn workspace_service_counts(repos: &[PathBuf]) -> Option<Vec<usize>> {
    repos
        .iter()
        .map(|repo| {
            crate::service_derivation::resolve(repo)
                .ok()
                .map(|derived| derived.services.len().max(1))
        })
        .collect()
}

/// Phase 1 for one repo, and what its scan cost when it was a paid one.
fn scan_repo(
    exe: &Path,
    repo: &Path,
    blobs: &Path,
    previous: &Path,
    label: &str,
    pass: &Pass,
    workspace_position: Option<(usize, usize)>,
) -> Result<ScanReport, String> {
    let mut command = scan_command(exe, repo, blobs, previous, pass);
    if let Some((offset, total)) = workspace_position {
        command
            .env(crate::progress::OFFSET_ENV, offset.to_string())
            .env(crate::progress::TOTAL_ENV, total.to_string());
    }
    run_scan(
        command,
        &format!("scan of {label}"),
        Reporting {
            working: format!("indexing {label}"),
            done: format!("indexed {label}"),
        },
        HEARTBEAT,
    )
}

/// The subprocess one repo's phase-1 scan runs as.
///
/// Built separately from the spawn so a test can read back what a laptop scan
/// asks for and what a facts-only one does: the difference between them is
/// entirely in this environment, and it is the difference between a free pass
/// and a paid one.
pub(super) fn scan_command(
    exe: &Path,
    repo: &Path,
    blobs: &Path,
    previous: &Path,
    pass: &Pass,
) -> Command {
    let mut command = Command::new(exe);
    command
        .arg(repo)
        .env(crate::logging::RUN_ID_ENV, crate::logging::run_id())
        .env(
            crate::logging::RUN_PHASE_ENV,
            format!("scan of {}", repo_label(repo)),
        )
        .env(crate::cloud_storage::CACHE_DIR_ENV, blobs)
        .env(crate::cloud_storage::ISOLATE_ENV, "1")
        .env(super::hosted::PREVIOUS_ENV, previous)
        // The scan is a subprocess whose output this indexer swallows, so it
        // is asked for the one thing worth showing: how far through each
        // service it is (carrick#955).
        .env(crate::progress::PROGRESS_ENV, "1")
        .env_remove("CARRICK_OUTPUT_JSON")
        .env_remove(super::JOIN_OUT_ENV);
    // Whatever the pass, the two flags that decide what this scan may do are
    // set here and nowhere else.
    command
        .env_remove(crate::analysis_channel::DISPATCH_ENV)
        .env_remove(crate::analysis_channel::ANSWERS_ENV)
        .env_remove(super::SKIP_UPLOAD_ENV);
    if pass.infers() {
        // A laptop scan asks the model, generates intents, and uploads — the
        // same index a CI scan writes. The two flags a facts-only pass sets
        // are the two that would make it something less.
        command
            .env(crate::cloud_storage::LAPTOP_SCAN_ENV, "1")
            .env_remove(super::NO_MODEL_ENV)
            .env_remove("CARRICK_SKIP_INTENTS");
    } else {
        command
            .env(super::NO_MODEL_ENV, "1")
            .env("CARRICK_SKIP_INTENTS", "1")
            .env_remove(crate::cloud_storage::LAPTOP_SCAN_ENV);
    }
    match pass {
        Pass::Dispatch => {
            command.env(crate::analysis_channel::DISPATCH_ENV, "1");
        }
        Pass::Resume(resuming) => {
            if let Some(resumption) = resuming.get(repo) {
                if let Some(answers) = &resumption.answers {
                    command.env(crate::analysis_channel::ANSWERS_ENV, answers);
                }
                if resumption.superseded {
                    command.env(super::SKIP_UPLOAD_ENV, "1");
                }
            }
        }
        Pass::Facts | Pass::Infer => {}
    }
    // The build has already checked every repo for a tree it cannot type, so
    // a scan it starts must not check again and reach a different answer: the
    // build's decision travels with the child (carrick#1254). Set either way,
    // so an inherited variable cannot allow a scan this build refused.
    if crate::preflight::allowed() {
        command.env(crate::preflight::ALLOW_ENV, "1");
    } else {
        command.env_remove(crate::preflight::ALLOW_ENV);
    }
    // Always: the ambient CI context would name every repo in the workspace
    // after the one whose shell this ran in, and on a laptop scan its OIDC
    // variables would select the wrong credential entirely.
    strip_ci_env(&mut command);
    no_colour(&mut command);
    command
}

/// Phase 2: join every blob, and hand the result back.
fn join(exe: &Path, repo: &Path, blobs: &Path, out: &Path) -> Result<LocalJoin, String> {
    run_scan(
        join_command(exe, repo, blobs, out),
        "workspace join",
        Reporting {
            working: "joining the workspace".to_string(),
            done: "joined the workspace".to_string(),
        },
        HEARTBEAT,
    )?;

    let text = std::fs::read_to_string(out)
        .map_err(|e| format!("the join wrote no result to {}: {e}", out.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("could not read the join result: {e}"))
}

/// The subprocess the join phase runs as, built apart from the spawn for the
/// same reason [`scan_command`] is: what this phase asks for is entirely in
/// its environment, and a test reads it back from here.
pub(super) fn join_command(exe: &Path, repo: &Path, blobs: &Path, out: &Path) -> Command {
    let mut command = Command::new(exe);
    command
        .arg(repo)
        .env(crate::logging::RUN_ID_ENV, crate::logging::run_id())
        .env(crate::logging::RUN_PHASE_ENV, "workspace join")
        .env(crate::cloud_storage::CACHE_DIR_ENV, blobs)
        .env(super::NO_MODEL_ENV, "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        // The join reads every blob back and re-reads this repo, so it is the
        // second phase worth a count rather than a silence (carrick#955).
        .env(crate::progress::PROGRESS_ENV, "1")
        .env(super::JOIN_OUT_ENV, out)
        .env_remove(crate::cloud_storage::ISOLATE_ENV)
        .env_remove(super::hosted::PREVIOUS_ENV)
        // Never a laptop scan: this phase analyses nothing and uploads
        // nothing, it reads blobs back and joins them.
        .env_remove(crate::cloud_storage::LAPTOP_SCAN_ENV)
        .env_remove("CARRICK_OUTPUT_JSON");
    strip_ci_env(&mut command);
    no_colour(&mut command);
    command
}

/// What one repo's scan said on the way past: what it paid, and whether it
/// handed its prompts over instead of answering them here.
#[derive(Debug, Default, PartialEq)]
pub(super) struct ScanReport {
    pub spend: Option<crate::scan_spend::ScanSpend>,
    pub dispatched: Option<crate::analysis_job::Dispatched>,
    /// Set only on a `--dispatch` pass, and only when the scan did not hand
    /// over: why it did not (carrick#1251). `None` on every other pass, and on
    /// a dispatch that worked.
    pub not_dispatched: Option<crate::progress::NotDispatched>,
    /// What this scan left for a later run, in the sentence the scan stated.
    /// Printed as it arrives, and carried out of here so the build's summary
    /// can end on it: a run that landed six services and deferred a seventh
    /// has a next step, and it is not the ordinary one (carrick#1315).
    pub pending: Vec<String>,
    /// Where this scan's wall clock went. `None` from a scan that did not
    /// finish, or one that measured nothing (carrick#1452).
    pub timing: Option<crate::scan_timing::Split>,
}

/// What one phase of a build calls itself while it runs and once it is done.
struct Reporting {
    working: String,
    done: String,
}

/// Run one scan subprocess, and say what it printed if it failed.
///
/// The scan's stdout is dropped — it is a report that means nothing here — and
/// its stderr is read line by line rather than collected at the end, because
/// the progress lines it carries are worth something only while the scan is
/// still running (carrick#955).
///
/// What is kept of a failure is its HEAD and its tail, not the tail alone. A
/// Rust panic prints `thread '...' panicked at <file:line>` and then a
/// backtrace note, and a scan that panics keeps logging on the way down: the
/// last twelve lines were the shutdown noise and the sentence naming the
/// panic had already been evicted, which is how carrick#936 reached a user as
/// "the scan of <path> failed" with no cause anywhere.
fn run_scan(
    mut command: Command,
    what: &str,
    reporting: Reporting,
    heartbeat: std::time::Duration,
) -> Result<ScanReport, String> {
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start the {what}: {e}"))?;
    // From here until the wait below, this is the child a signal to the build
    // has to reach (carrick#1379).
    RUNNING_SCAN.store(child.id() as i32, std::sync::atomic::Ordering::Relaxed);
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("the {what} produced no stderr to read"))?;
    let bar = crate::logging::spinner(&reporting.working);
    // And to a third, when this build is itself a child: the npm wrapper draws
    // the terminal, and a spinner rewriting a pipe is what reached a first run
    // as padded fragments on one line (carrick#1315).
    crate::progress::report_phase(&reporting.working, crate::progress::PhaseState::Started);
    // The same update, to the two places that can be waiting on it: the
    // spinner a person is watching, and the state file `carrick status` reads
    // for a scan nobody is watching at all (carrick#992). The second is a
    // no-op unless this build was detached.
    super::scan_state::note(&reporting.working, None);
    let mut head: Vec<String> = Vec::with_capacity(KEPT_LINES);
    let mut tail: VecDeque<String> = VecDeque::with_capacity(KEPT_LINES);
    // The sentences that explain the failure, wherever in the output they fell.
    let mut causes: VecDeque<String> = VecDeque::with_capacity(KEPT_CAUSES);
    let mut dropped = 0usize;
    // What this scan paid, if it paid anything. It crosses on the same
    // channel as the progress, and for the same reason: this process swallows
    // the scan's output, so a figure it does not lift out is a figure nobody
    // ever sees (carrick#995).
    let mut spend = None;
    // And where its wall clock went, on the same channel (carrick#1452).
    let mut timing = None;
    // Whether this scan handed its prompts to Carrick Cloud rather than
    // answering them here (carrick#1229). It crosses on the same channel as
    // the spend, and for the same reason: this process swallows the scan's
    // output, and a scan that dispatched writes no blob for the caller to find.
    let mut dispatched = None;
    // And why it did not, when it was asked to and did not. The scan that
    // reports this exits 0 with an index, so nothing downstream needs the
    // line — the reader does (carrick#1251).
    let mut not_dispatched = None;
    // What the scan still owes, when it landed some services and left others
    // pending. A scan in that state exits 0, so its output would otherwise be
    // dropped with the rest, and this is the sentence that says to re-run.
    let mut pending: Vec<String> = Vec::new();
    // Why the scan failed, in the one sentence it states on the same channel.
    // It leads the error, so every reader that shows a failure in one line
    // (`carrick status`, the SessionStart hook) shows that sentence rather
    // than the first line of the scan's log (carrick#1103).
    let mut failure: Option<crate::progress::Failure> = None;
    // The last count and the last notice, so either one arriving redraws the
    // bar with both.
    let mut last_update: Option<crate::progress::Update> = None;
    let mut notice: Option<String> = None;
    // The child's stderr is read on a thread of its own so that this loop can
    // wake up when the child says NOTHING. A scan's quiet stretches are its
    // long ones — a model call, a type check — and the log's pulse used to be
    // driven entirely by lines arriving, so it stopped exactly when a reader
    // most needed it and the last thing it had said was a part-finished count
    // (carrick#1007 item 3).
    let (lines, from_child) = std::sync::mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    loop {
        let line = match from_child.recv_timeout(heartbeat) {
            Ok(line) => line,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Same phase, same counts, new moment: the state file's
                // `updated_at` moves and the log gets its pulse.
                super::scan_state::note(&reporting.working, None);
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if let Some(update) = crate::progress::parse(&line) {
            // Passed on unchanged when this build has a parent of its own: the
            // counts a spinner here draws are the counts that renderer draws
            // (carrick#1315).
            forward(&line);
            bar.set_message(bar_message(
                &reporting.working,
                Some(&update),
                notice.as_deref(),
            ));
            super::scan_state::note(&reporting.working, Some(&update));
            last_update = Some(update);
            continue;
        }
        // Why the scan is slow, said by the scan (carrick#1122). It rides
        // beside the counts until the next one replaces it.
        if let Some(text) = crate::progress::parse_notice(&line) {
            forward(&line);
            bar.set_message(bar_message(
                &reporting.working,
                last_update.as_ref(),
                Some(&text),
            ));
            super::scan_state::notice(&text);
            notice = Some(text);
            continue;
        }
        if let Some(reported) = crate::scan_spend::parse(&line) {
            spend = Some(reported);
            continue;
        }
        // Where this scan's wall clock went. It crosses on the same channel as
        // the spend, and for the same reason: this process swallows the scan's
        // output, so a figure it does not lift out is a figure the next build
        // cannot state (carrick#1452).
        if let Some(reported) = crate::scan_timing::parse(&line) {
            timing = Some(reported);
            continue;
        }
        if let Some(reported) = crate::progress::parse_dispatched(&line) {
            dispatched = Some(reported);
            continue;
        }
        if let Some(reason) = crate::progress::parse_not_dispatched(&line) {
            not_dispatched = Some(reason);
            continue;
        }
        if let Some(statement) = crate::progress::parse_pending(&line) {
            pending.push(statement);
            continue;
        }
        if let Some(stated) = crate::progress::parse_failure(&line) {
            failure = Some(stated);
            continue;
        }
        // Kept clean from here on: what this loop keeps is read back by a
        // JSON reader and by a log file, neither of which renders escapes
        // (carrick#1023 item 6).
        let line = strip_ansi(&line);
        if names_a_failure(&line) {
            if causes.len() == KEPT_CAUSES {
                causes.pop_front();
            }
            causes.push_back(line.clone());
        }
        if head.len() < KEPT_LINES {
            head.push(line);
            continue;
        }
        if tail.len() == KEPT_LINES {
            tail.pop_front();
            dropped += 1;
        }
        tail.push_back(line);
    }
    let _ = reader.join();
    let waited = child.wait();
    // Cleared before anything can leave this function: a signal arriving
    // between phases has no child to reach, and a pid the system has already
    // reused is not one to send anything to.
    RUNNING_SCAN.store(0, std::sync::atomic::Ordering::Relaxed);
    let status = waited.map_err(|e| format!("could not wait for the {what}: {e}"))?;
    if status.success() {
        if pending.is_empty() {
            crate::logging::finish_spinner(&bar, &reporting.done);
            crate::progress::report_phase(&reporting.done, crate::progress::PhaseState::Done);
        } else {
            crate::logging::finish_spinner_warn(&bar, &reporting.done);
            crate::progress::report_phase(&reporting.done, crate::progress::PhaseState::Warned);
            for statement in &pending {
                crate::errln!("carrick: {statement}");
            }
        }
        return Ok(ScanReport {
            spend,
            dispatched,
            not_dispatched,
            pending,
            timing,
        });
    }
    bar.finish_and_clear();
    let reason = match failure {
        Some(failure) => failure.reason,
        None => format!("it exited ({status}) without saying why"),
    };
    Err(format!(
        "the {what} failed: {reason}\n{}",
        failure_excerpt(head, causes, tail, dropped)
    ))
}

/// Pass a marker line on to this build's own parent, unchanged.
///
/// A build is a parent to its scans and, under the npm wrapper, a child
/// itself. The counts and notices it renders on a spinner are exactly what
/// that renderer needs, and re-encoding them here would give the two readers
/// two different lines to keep in step (carrick#1315). Nothing is written when
/// nobody is reading.
fn forward(line: &str) {
    if crate::progress::parent_is_reading() {
        crate::errln!("{line}");
    }
}

/// The spinner's message: the phase, its counts, and why it is slow when the
/// scan has said.
fn bar_message(
    working: &str,
    update: Option<&crate::progress::Update>,
    notice: Option<&str>,
) -> String {
    let mut message = working.to_string();
    if let Some(update) = update {
        message.push_str(&format!(": {}", update.render()));
    }
    if let Some(notice) = notice {
        message.push_str(&format!(" ({notice})"));
    }
    message
}

/// How long this process waits on a silent child before saying where it is.
/// Shorter than the log's own pulse, so the pulse decides how often a line is
/// written and this only decides how often it is offered one.
const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(5);

/// The scan subprocess this build is waiting on, or zero (carrick#1379).
///
/// A build runs its phases one at a time — every repo's scan and then the
/// join, all through [`run_scan`] — so there is exactly one such child at any
/// moment, and exactly one answer to keep. Process-global for the reason
/// [`crate::shutdown`]'s own flag is: the code that has to read it sits in
/// `main`, on the other side of the `spawn_blocking` the build runs on, and
/// one process is one build.
static RUNNING_SCAN: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Send `signal` to the scan this build is waiting on, and say whether there
/// was one (carrick#1379).
///
/// A terminal sends Ctrl-C to the whole foreground process group, so both this
/// process and its scan hear it and each reports itself. `kill <build pid>`
/// does not: only this process hears it, writes `interrupted by SIGTERM` into
/// its scan record and exits, while the scan it was waiting on carries on
/// running. Nothing is stranded — the child opened its own scan and closes it
/// the normal way — but for as long as it runs the two disagree, and a second
/// `carrick index` in that window is refused by a slot whose run the user was
/// told had stopped.
///
/// **Only SIGTERM is forwarded**, and that is a decision about what the kernel
/// has already done rather than about what the signals mean. SIGINT and SIGHUP
/// are both delivered to the whole foreground process group — Ctrl-C by the
/// line discipline, a hangup by the kernel when the controlling terminal goes
/// — so the child has them already, and a second one arriving while it makes
/// its own bounded report is read as "go now" and abandons that report
/// (carrick#1235). SIGTERM is the one signal a terminal never delivers to the
/// group, so forwarding it can never be a duplicate. The cost of the
/// exclusion is an explicit `kill -HUP <build pid>`, which leaves today's
/// behaviour.
pub(crate) fn forward_to_running_scan(signal: crate::shutdown::Shutdown) -> bool {
    if !travels(signal) {
        return false;
    }
    let pid = RUNNING_SCAN.load(std::sync::atomic::Ordering::Relaxed);
    if pid <= 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // A pid that has already exited is an error this does not act on: the
        // build is on its way out either way, and the only thing the answer
        // decides is whether to wait for a child that is already gone.
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            return false;
        }
        true
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Whether a signal the build was sent has to be passed on, or the child has
/// it already. See [`forward_to_running_scan`] for the reasoning.
fn travels(signal: crate::shutdown::Shutdown) -> bool {
    matches!(signal, crate::shutdown::Shutdown::Terminate)
}

/// How long the build waits for a scan it has just signalled.
///
/// At least the child's own report budget, because the forward is what ASKS
/// for that report and a shorter wait would take back the thing it just asked
/// for. The margin on top is for the child's exit itself: the report is the
/// long half, and what follows it is dropping a sidecar and closing a file.
pub(crate) fn forwarded_exit_grace() -> std::time::Duration {
    crate::shutdown::INTERRUPTION_REPORT_BUDGET + std::time::Duration::from_secs(2)
}

/// How many lines of a failed child's stderr are kept from each end.
const KEPT_LINES: usize = 12;
/// How many lines naming a failure are lifted out of the middle. The LAST
/// twelve, not the first: a scan that prints a dozen `error` lines on its way
/// to the one that killed it would otherwise evict exactly the line this cap
/// exists to keep.
const KEPT_CAUSES: usize = 12;
/// What the Rust runtime prints when a thread dies, whatever the message is.
const PANIC_MARKER: &str = "panicked at";

/// Whether a line from a failed scan is one that says what went wrong.
///
/// The head and the tail are position, not meaning: a scan of five services
/// spends its middle on them, so the twelve lines a scan of the fifth failed
/// on were exactly the ones elided — the excerpt kept the startup banner and
/// the shutdown noise and dropped the cause (carrick#1023 item 8). Matched
/// case-insensitively, because the same word arrives from the scanner, from
/// `tsc` and from a runtime in three spellings.
fn names_a_failure(line: &str) -> bool {
    let lowered = line.to_ascii_lowercase();
    lowered.contains(PANIC_MARKER) || lowered.contains("failed") || lowered.contains("error")
}

/// The lines of a failed scan worth repeating: the first twelve, anything that
/// named a failure, the last twelve, and a count of what was dropped between
/// them.
fn failure_excerpt(
    head: Vec<String>,
    causes: VecDeque<String>,
    tail: VecDeque<String>,
    dropped: usize,
) -> String {
    let mut lines = head;
    let middle: Vec<String> = causes
        .into_iter()
        .filter(|cause| !lines.contains(cause) && !tail.contains(cause))
        .collect();
    if dropped > 0 {
        lines.push(format!(
            "... {dropped} line(s) not shown{} ...",
            if middle.is_empty() { "" } else { ", except" }
        ));
    }
    lines.extend(middle);
    lines.extend(tail);
    lines.join("\n")
}

/// A line of child output with its terminal control sequences removed.
///
/// What a scan prints is kept for two readers that have no terminal to
/// interpret them: the JSON `error` string `carrick status` serves, and the
/// `.carrick/scan-<id>.log` a detached run writes to a file. Both carried raw
/// escapes (carrick#1023 item 6). The producer was not identified — nothing in
/// this repo colours its output off a TTY — so this strips them wherever they
/// came from, and [`no_colour`] asks every child not to write them.
fn strip_ansi(line: &str) -> String {
    let mut clean = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            clean.push(character);
            continue;
        }
        // CSI (`ESC [`) runs to a byte in `@`-`~`; every other escape is two
        // characters, so dropping the one that follows is the whole of it.
        match chars.next() {
            Some('[') => {
                for following in chars.by_ref() {
                    if ('@'..='~').contains(&following) {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC runs to BEL or ST (`ESC \`).
                while let Some(following) = chars.next() {
                    if following == '\u{7}' {
                        break;
                    }
                    if following == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    clean
}

/// Tell a child not to colour what it prints.
///
/// Every child this module starts writes to a pipe or to a log file, and
/// neither reader renders escapes. `NO_COLOR` is the convention most tools
/// honour; the two force variables are removed because a caller's environment
/// can carry them and they override it.
fn no_colour(command: &mut Command) {
    command.env("NO_COLOR", "1");
    command.env_remove("FORCE_COLOR");
    command.env_remove("CLICOLOR_FORCE");
}

/// Strip the ambient CI context, exactly as the offline harness does. Without
/// it, `GITHUB_REPOSITORY` names every repo in the workspace after the one
/// whose shell this ran in, and every blob clobbers the last.
fn strip_ci_env(command: &mut Command) {
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "GITHUB_EVENT_PATH",
        "CI",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        command.env_remove(var);
    }
}

/// The directory name a scan of this path records as the repo name.
pub(super) fn repo_label(repo: &Path) -> String {
    repo.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| repo.to_string_lossy().into_owned())
}

/// `service_name ?? repo_name`, the identity every cross-repo surface uses.
pub(super) fn service_id(blob: &CloudRepoData) -> String {
    blob.service_name
        .clone()
        .unwrap_or_else(|| blob.repo_name.clone())
}

/// The service root this blob was scanned with, from the config it carries.
/// `None` for a single-service repo and for a blob whose config does not
/// parse — both mean "this service is not confined to a subdirectory".
fn service_directory(blob: &CloudRepoData) -> Option<String> {
    let config = blob.config_json.as_deref()?;
    let parsed: serde_json::Value = serde_json::from_str(config).ok()?;
    parsed
        .get("directory")?
        .as_str()
        .map(|directory| directory.trim_matches('/').to_string())
        .filter(|directory| !directory.is_empty())
}

/// The extra source roots this service was scanned with. Empty for a blob
/// whose config declares none and for one whose config does not parse.
fn service_include(blob: &CloudRepoData) -> Vec<String> {
    let Some(config) = blob.config_json.as_deref() else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(config) else {
        return Vec::new();
    };
    parsed
        .get("include")
        .and_then(|include| include.as_array())
        .map(|roots| {
            roots
                .iter()
                .filter_map(|root| root.as_str())
                .map(|root| root.trim_matches('/').to_string())
                .filter(|root| !root.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Every blob in the cache dir, in a stable order.
pub(super) fn read_blobs(blobs: &Path) -> Result<Vec<CloudRepoData>, String> {
    let Ok(entries) = std::fs::read_dir(blobs) else {
        return Ok(Vec::new());
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    paths.sort();
    let mut blobs = Vec::new();
    for path in paths {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let blob: CloudRepoData = serde_json::from_str(&text)
            .map_err(|e| format!("could not parse {}: {e}", path.display()))?;
        blobs.push(blob);
    }
    Ok(blobs)
}

/// Fold the blobs and the join into the read model.
pub(super) fn build(
    workspace: &Workspace,
    blobs_dir: &Path,
    join: &LocalJoin,
    remotes: &BTreeMap<String, String>,
) -> Result<LocalIndex, String> {
    let blobs = read_blobs(blobs_dir)?;
    let now = timestamp();

    let mut repos: Vec<IndexedRepo> = Vec::new();
    for repo_path in &workspace.repos {
        // Two repos with one directory name are refused by `Workspace::load`,
        // before any of them is scanned: by the time the second scan has
        // finished, the first repo's blob is already gone.
        let name = repo_label(repo_path);
        let services: Vec<IndexedService> = blobs
            .iter()
            .filter(|blob| blob.repo_name == name && !remotes.contains_key(&service_id(blob)))
            .map(|blob| IndexedService {
                enrichment: Default::default(),
                name: service_id(blob),
                directory: service_directory(blob),
                include: service_include(blob),
                commit: blob.commit_hash.clone(),
                indexed_at: blob.last_updated.to_rfc3339(),
                boundary: blob.boundary.clone(),
                routes: blob.endpoints.len(),
                calls: blob.calls.len(),
                functions: blob.function_definitions.len(),
                types: blob
                    .bundled_types
                    .as_deref()
                    .map(crate::type_manifest::dts_servable_types)
                    .unwrap_or(0),
            })
            .collect();
        repos.push(IndexedRepo {
            path: repo_path.to_string_lossy().into_owned(),
            name,
            services,
            files: BTreeMap::new(),
        });
    }

    // Which repo each service belongs to, so an operation lands in the right
    // repo's file map.
    let mut owner: BTreeMap<String, usize> = BTreeMap::new();
    for (position, repo) in repos.iter().enumerate() {
        for service in &repo.services {
            owner.insert(service.name.clone(), position);
        }
    }

    // Where each producer operation is written, keyed as an edge names it.
    let mut producers: BTreeMap<(String, String), Vec<&JoinedOperation>> = BTreeMap::new();
    for operation in &join.operations {
        if operation.role == Role::Producer {
            producers
                .entry((operation.service.clone(), operation.key.clone()))
                .or_default()
                .push(operation);
        }
    }

    for operation in &join.operations {
        let Some(&position) = owner.get(&operation.service) else {
            // A service in the join that belongs to no repo in the workspace
            // is a blob left over from a workspace that has changed; the full
            // index clears them, so this is only reachable mid-refresh.
            continue;
        };
        let counterparts = counterparts_for(operation, &join.matches, &producers);
        let verdict = verdict_for(operation, &join.matches, &join.findings);
        // The two type texts come off the same finding the verdict does, so a
        // row can never state a verdict from one pair and types from another.
        let typed = finding_for(operation, &join.findings);
        let item = IndexedItem {
            kind: match operation.role {
                Role::Producer => ItemKind::Route,
                Role::Consumer => ItemKind::Call,
            },
            service: operation.service.clone(),
            key: operation.key.clone(),
            method: operation.method.clone(),
            path: operation.path.clone(),
            line: operation.line,
            col: None,
            source: Source::of(operation.resolution_source),
            resolution_source: operation.resolution_source,
            evidence: evidence_for(operation),
            counterparts,
            verdict,
            expected_type: typed.and_then(|finding| finding.expected_type.clone()),
            actual_type: typed.and_then(|finding| finding.actual_type.clone()),
            direction: typed.and_then(|finding| finding.direction.clone()),
        };
        repos[position]
            .files
            .entry(operation.file.clone())
            .or_default()
            .push(item);
    }

    for repo in &mut repos {
        for items in repo.files.values_mut() {
            items.sort_by(|a, b| {
                (a.line, &a.kind.as_str(), &a.method, &a.path).cmp(&(
                    b.line,
                    &b.kind.as_str(),
                    &b.method,
                    &b.path,
                ))
            });
        }
    }

    Ok(LocalIndex {
        hosted_identity: None,
        hosted_workspace: None,
        repos_detected_by: Some(
            match workspace.repos_detected_by.as_str() {
                "carrick.json" => "carrick_json",
                "sibling repositories" => "siblings",
                "single repository" => "single_repo",
                "workspace manifest" => "workspace_manifest",
                _ => "workspace_overrides",
            }
            .to_string(),
        ),
        repos_added: workspace.repos_added.clone(),
        repos_excluded: workspace.repos_excluded.clone(),
        hosted_checked_at: None,
        hosted_source_key: None,
        version: READ_MODEL_VERSION,
        scanner_version: join.scanner_version.clone(),
        indexed_at: now,
        repos,
    })
}

/// The other side of this operation's contract, across every repo indexed.
///
/// A producer names its consumers by the edges its key carries. A consumer
/// names its producers through the edges its own CALL SITE carries: an edge
/// records where the consumer's call is, so two calls to the same operation in
/// one service each answer for themselves rather than sharing one list (#260).
fn counterparts_for(
    operation: &JoinedOperation,
    matches: &[JoinedMatch],
    producers: &BTreeMap<(String, String), Vec<&JoinedOperation>>,
) -> Vec<Counterpart> {
    let mut found: Vec<Counterpart> = Vec::new();
    match operation.role {
        Role::Producer => {
            for edge in matches {
                if edge.producer_service != operation.service || edge.producer_key != operation.key
                {
                    continue;
                }
                found.push(Counterpart {
                    remote: None,
                    role: consumer_role(&edge.relationship).to_string(),
                    service: edge.consumer_service.clone(),
                    file: edge.consumer_file.clone().unwrap_or_default(),
                    line: edge.consumer_line,
                });
            }
        }
        Role::Consumer => {
            for edge in matches {
                if edge.consumer_service != operation.service || edge.consumer_key != operation.key
                {
                    continue;
                }
                if !edge_is_at(edge, operation) {
                    continue;
                }
                // One key can be served by more than one site (carrick#718),
                // and each of them is a real answer to "where does this go".
                let sites = producers
                    .get(&(edge.producer_service.clone(), edge.producer_key.clone()))
                    .cloned()
                    .unwrap_or_default();
                if sites.is_empty() {
                    found.push(Counterpart {
                        remote: None,
                        role: producer_role(&edge.relationship).to_string(),
                        service: edge.producer_service.clone(),
                        file: String::new(),
                        line: None,
                    });
                }
                for site in sites {
                    found.push(Counterpart {
                        remote: None,
                        role: producer_role(&edge.relationship).to_string(),
                        service: site.service.clone(),
                        file: site.file.clone(),
                        line: site.line,
                    });
                }
            }
        }
    }
    found.sort_by(|a, b| (&a.service, &a.file, a.line).cmp(&(&b.service, &b.file, b.line)));
    found.dedup();
    found
}

/// Whether an edge is recorded at this consumer row's own site. An edge with
/// no location recorded belongs to every row on its key — that is what the
/// absence means, and dropping it would lose a real counterpart.
fn edge_is_at(edge: &JoinedMatch, operation: &JoinedOperation) -> bool {
    let Some(file) = edge.consumer_file.as_deref() else {
        return true;
    };
    if file != operation.file {
        return false;
    }
    match (edge.consumer_line, operation.line) {
        (Some(edge_line), Some(row_line)) => edge_line == row_line,
        _ => true,
    }
}

/// In a shared external contract neither side serves the other, so neither
/// label is true and the row says `peer` instead (#379).
fn consumer_role(relationship: &str) -> &'static str {
    if relationship == "shared_external_contract" {
        "peer"
    } else {
        "consumer"
    }
}

fn producer_role(relationship: &str) -> &'static str {
    if relationship == "shared_external_contract" {
        "peer"
    } else {
        "producer"
    }
}

/// What the type check concluded about this row, at index time.
///
/// A type mismatch is stated with the compiler's own reason; a method mismatch
/// comes from the finding that named this call site. A pair the check never
/// evaluated gets no verdict at all rather than a reassuring one — `check`
/// renders that absence as "not checked".
fn verdict_for(
    operation: &JoinedOperation,
    matches: &[JoinedMatch],
    findings: &[JoinedFinding],
) -> Option<StoredVerdict> {
    // A finding first, where one names this row. The report's own detail has
    // been through the alias -> display-name pass, so it says `Widget` where
    // the raw pair outcome says `Endpoint_44785e_Response_At0c682a`. Same
    // verdict, readable by whoever reads it.
    if let Some(finding) = finding_for(operation, findings) {
        return Some(StoredVerdict {
            // The finding states its own verdict state (carrick#727) in the
            // same three words this contract prints, so it is carried rather
            // than re-derived. The fallback is for a finding that states none:
            // a wrong verb is a routing fact and no type verdict bears on it.
            state: finding
                .verdict_state
                .clone()
                .unwrap_or_else(|| match finding.kind.as_str() {
                    "type_mismatch" => "resolved".to_string(),
                    _ => "not_checked".to_string(),
                }),
            result: Some(finding.kind.clone()),
            detail: finding.detail.clone(),
        });
    }

    let mut verdict: Option<StoredVerdict> = None;
    for edge in matches {
        let mine = match operation.role {
            Role::Producer => {
                edge.producer_service == operation.service && edge.producer_key == operation.key
            }
            Role::Consumer => {
                edge.consumer_service == operation.service
                    && edge.consumer_key == operation.key
                    && edge_is_at(edge, operation)
            }
        };
        if !mine {
            continue;
        }
        match edge.type_verdict {
            Some(crate::operation::TypeVerdict::Incompatible) => {
                return Some(StoredVerdict {
                    state: "resolved".to_string(),
                    result: Some("type_mismatch".to_string()),
                    detail: edge.mismatch_reason.clone().unwrap_or_else(|| {
                        "the compiler found the two types incompatible".to_string()
                    }),
                });
            }
            Some(crate::operation::TypeVerdict::Compatible) => {
                verdict = Some(StoredVerdict {
                    state: "resolved".to_string(),
                    result: Some("compatible".to_string()),
                    detail: "the compiler compared both sides and found them compatible"
                        .to_string(),
                });
            }
            // A verdict was attempted and a side of the pair was not
            // resolvable — `any`, `unknown`, or a type the capture could not
            // reach. Distinct from never having been compared, and the
            // difference is the whole reason this row is kept: a reader must
            // not read "nothing was claimed" as "nothing is wrong".
            Some(crate::operation::TypeVerdict::Unverifiable) if verdict.is_none() => {
                verdict = Some(StoredVerdict {
                    state: "unresolved".to_string(),
                    result: None,
                    detail: "the pair was compared and a side of it did not resolve to a \
                             usable type, so nothing is claimed about it"
                        .to_string(),
                });
            }
            // A pair already compared successfully keeps that verdict: one
            // unresolvable peer does not erase a real comparison.
            Some(crate::operation::TypeVerdict::Unverifiable) => {}
            None => {}
        }
    }
    verdict
}

/// The finding this row's verdict and type texts both come from, or none.
///
/// One selector, used by both: a row that took its verdict from one finding
/// and its types from another would state a pair that was never compared.
fn finding_for<'a>(
    operation: &JoinedOperation,
    findings: &'a [JoinedFinding],
) -> Option<&'a JoinedFinding> {
    findings
        .iter()
        .find(|finding| finding_names(finding, operation))
}

/// Whether a finding is about THIS row: the consumer site it names, or the
/// operation a producer serves.
fn finding_names(finding: &JoinedFinding, operation: &JoinedOperation) -> bool {
    match operation.role {
        Role::Consumer => {
            let site = match operation.line {
                Some(line) => format!("{}:{line}", operation.file),
                None => operation.file.clone(),
            };
            // A finding names the consumer's service only when the report had
            // one to name; absent, the call site is the whole attribution, and
            // requiring a service that was never stated drops every verdict.
            let service_agrees = finding
                .service
                .as_deref()
                .is_none_or(|named| named == operation.service);
            service_agrees
                && finding
                    .call_sites
                    .iter()
                    .any(|call_site| call_site == &site || call_site.starts_with(&site))
        }
        // A finding names the PRODUCER's method and path, and the consumer's
        // service. So a producer row matches on the operation it serves, which
        // is the identity the finding was raised about.
        Role::Producer => {
            finding.method.eq_ignore_ascii_case(&operation.method) && finding.path == operation.path
        }
    }
}

/// One line naming what the row was read off. Written for a model reading a
/// terminal: the pass that stated the row, and the handler it names.
fn evidence_for(operation: &JoinedOperation) -> Option<String> {
    let source = operation.resolution_source.map(|source| {
        serde_json::to_value(source)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("{source:?}"))
    });
    match (source, operation.handler.as_deref()) {
        (Some(source), Some(handler)) if !handler.is_empty() => {
            Some(format!("{source}, handler {handler}"))
        }
        (Some(source), _) => Some(source),
        (None, Some(handler)) if !handler.is_empty() => Some(format!("handler {handler}")),
        (None, _) => None,
    }
}

/// RFC 3339, to the second.
fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exactly one pass measures the tree the way the next `carrick index`
    /// will read it. A pass added to this set that does less work would make
    /// the opening line quote a time for work that is not about to happen
    /// (carrick#1452).
    #[test]
    fn only_the_inferred_full_pass_leaves_a_time_for_the_next_run() {
        assert!(Pass::Infer.measures_the_tree());
        assert!(!Pass::Facts.measures_the_tree());
        assert!(!Pass::Dispatch.measures_the_tree());
        assert!(!Pass::Resume(BTreeMap::new()).measures_the_tree());
    }

    /// carrick#1379: only SIGTERM is passed on, and the reason is what the
    /// kernel has already done rather than what the signals mean.
    ///
    /// SIGINT and SIGHUP reach the whole foreground process group, so the
    /// child has them already and a second one arriving while it makes its
    /// bounded report is read as "go now" and abandons that report.
    ///
    /// Asserted on the predicate rather than on [`forward_to_running_scan`],
    /// which reads [`RUNNING_SCAN`]: every other test in this binary that
    /// runs a subprocess writes that global, so a test that reached the kill
    /// could signal another test's child. What a forward does to a real one
    /// is proven at the built binary, in `tests/interrupted_scan_test.rs`.
    #[test]
    fn only_a_signal_a_terminal_never_sends_to_the_group_is_forwarded() {
        assert!(travels(crate::shutdown::Shutdown::Terminate));
        assert!(!travels(crate::shutdown::Shutdown::Interrupt));
        assert!(!travels(crate::shutdown::Shutdown::Hangup));
    }

    /// The build's wait covers the report the forward just asked for.
    ///
    /// A grace shorter than [`crate::shutdown::INTERRUPTION_REPORT_BUDGET`]
    /// would signal the scan and then leave before it could say it had
    /// stopped, which is the state this whole change exists to close.
    #[test]
    fn the_wait_for_a_signalled_scan_covers_its_own_report() {
        assert!(forwarded_exit_grace() > crate::shutdown::INTERRUPTION_REPORT_BUDGET);
    }

    /// What the phase-1 subprocess is asked to be, read off the command
    /// itself. The whole difference between a free pass and a paid one lives
    /// in this environment, so it is asserted rather than described.
    fn env_of(command: &Command) -> BTreeMap<String, Option<String>> {
        command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    fn scan_env(pass: &Pass) -> BTreeMap<String, Option<String>> {
        env_of(&scan_command(
            Path::new("/bin/carrick"),
            Path::new("/repos/api"),
            Path::new("/build/repos"),
            Path::new("/build/previous.json"),
            pass,
        ))
    }

    /// The indexer swallows a scan's output, so what a paid scan cost reaches
    /// this process only if this loop lifts it out — and it must come out of
    /// the stream rather than into the failure tail (carrick#995).
    #[test]
    fn the_indexer_lifts_the_figure_out_of_the_scan_it_swallowed() {
        let _serialised = SCAN_STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let spend = crate::scan_spend::ScanSpend {
            schema: crate::scan_spend::SCHEMA.to_string(),
            scan_id: "scan_01J".to_string(),
            first_index: true,
            priced: true,
            usd: Some(4.32),
            ..Default::default()
        };
        let mut command = Command::new("sh");
        command.arg("-c").arg(format!(
            "echo 'analysing' >&2; echo '@carrick-spend {}' >&2",
            serde_json::to_string(&spend).unwrap()
        ));

        let lifted = run_scan(
            command,
            "scan of /repos/api",
            Reporting {
                working: "indexing api".to_string(),
                done: "indexed api".to_string(),
            },
            HEARTBEAT,
        )
        .unwrap();
        assert_eq!(lifted.spend, Some(spend));
        assert_eq!(lifted.dispatched, None);
        assert_eq!(lifted.not_dispatched, None);
    }

    /// The same lift, for the scan that was asked to dispatch and did not.
    /// That scan exits 0 with an index, so nothing downstream misses this
    /// line — which is exactly why it was silent for so long. Without this
    /// branch the marker falls into the failure tail nobody reads, and
    /// `--dispatch` goes back to being indistinguishable from a flag that does
    /// nothing (carrick#1251).
    #[test]
    fn the_indexer_lifts_out_a_dispatch_that_did_not_happen() {
        let _serialised = SCAN_STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut command = Command::new("sh");
        command.arg("-c").arg(format!(
            "echo 'indexing' >&2; echo '@carrick-not-dispatched {}' >&2",
            serde_json::to_string(&crate::progress::NotDispatched::NothingToAnalyse).unwrap()
        ));
        let lifted = run_scan(
            command,
            "scan of /repos/api",
            Reporting {
                working: "indexing api".to_string(),
                done: "indexed api".to_string(),
            },
            HEARTBEAT,
        )
        .unwrap();
        assert_eq!(
            lifted.not_dispatched,
            Some(crate::progress::NotDispatched::NothingToAnalyse)
        );
        assert_eq!(lifted.dispatched, None);
    }

    /// A free pass reports nothing, and nothing is not a scan that cost zero.
    #[test]
    fn a_scan_that_states_no_figure_reports_none() {
        let _serialised = SCAN_STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut command = Command::new("sh");
        command.arg("-c").arg("echo 'analysing' >&2");
        let lifted = run_scan(
            command,
            "scan of /repos/api",
            Reporting {
                working: "indexing api".to_string(),
                done: "indexed api".to_string(),
            },
            HEARTBEAT,
        )
        .unwrap();
        assert_eq!(lifted, ScanReport::default());
    }

    /// The default is unchanged and costs nothing: no model, no intents, no
    /// upload. `carrick refresh` runs from a session-start hook and this is
    /// the command it runs.
    #[test]
    fn a_scan_without_inference_asks_for_no_model_and_no_intents() {
        let env = scan_env(&Pass::Facts);
        assert_eq!(env.get(super::super::NO_MODEL_ENV), Some(&Some("1".into())));
        assert_eq!(env.get("CARRICK_SKIP_INTENTS"), Some(&Some("1".into())));
        assert_eq!(
            env.get(crate::cloud_storage::LAPTOP_SCAN_ENV),
            Some(&None),
            "the laptop flag is explicitly cleared, not merely absent"
        );
    }

    /// A laptop scan asks the model, generates intents and uploads — the same
    /// index a CI scan writes. The two flags that make a pass facts-only are
    /// removed rather than set to "0", because the readers test for presence.
    #[test]
    fn a_scan_with_inference_drops_the_two_flags_that_make_it_facts_only() {
        let env = scan_env(&Pass::Infer);
        assert_eq!(
            env.get(crate::cloud_storage::LAPTOP_SCAN_ENV),
            Some(&Some("1".into()))
        );
        assert_eq!(env.get(super::super::NO_MODEL_ENV), Some(&None));
        assert_eq!(env.get("CARRICK_SKIP_INTENTS"), Some(&None));
    }

    /// A build the flag allowed hands that decision to every scan it starts,
    /// and a build that was not allowed clears the variable rather than
    /// letting an inherited one through (carrick#1254).
    #[test]
    #[serial_test::serial(allow_unprepared)]
    fn a_build_hands_its_unprepared_decision_to_the_scans_it_starts() {
        assert_eq!(
            scan_env(&Pass::Infer).get(crate::preflight::ALLOW_ENV),
            Some(&None),
            "the default is a scan that checks the tree for itself"
        );
        // SAFETY: serialised with every other test that reads this variable.
        unsafe { std::env::set_var(crate::preflight::ALLOW_ENV, "1") };
        let allowed = scan_env(&Pass::Infer);
        unsafe { std::env::remove_var(crate::preflight::ALLOW_ENV) };
        assert_eq!(
            allowed.get(crate::preflight::ALLOW_ENV),
            Some(&Some("1".into()))
        );
    }

    /// The three passes that are not the ordinary one, read back from the
    /// environment they hand the scan. The difference between them is entirely
    /// here, which is why this file tests it (carrick#1229).
    #[test]
    fn each_pass_asks_the_scan_for_exactly_what_it_is() {
        let dispatch = scan_env(&Pass::Dispatch);
        assert_eq!(
            dispatch.get(crate::analysis_channel::DISPATCH_ENV),
            Some(&Some("1".into()))
        );
        assert_eq!(
            dispatch.get(crate::analysis_channel::ANSWERS_ENV),
            Some(&None),
            "a dispatch has no answers to replay"
        );
        assert_eq!(
            dispatch.get(crate::cloud_storage::LAPTOP_SCAN_ENV),
            Some(&Some("1".into())),
            "and it still asks the model for detection and guidance"
        );

        let resume = scan_env(&Pass::Resume(BTreeMap::from([(
            PathBuf::from("/repos/api"),
            Resumption {
                answers: Some(PathBuf::from("/build/answers.ndjson.gz")),
                superseded: true,
            },
        )])));
        assert_eq!(
            resume.get(crate::analysis_channel::ANSWERS_ENV),
            Some(&Some("/build/answers.ndjson.gz".into()))
        );
        assert_eq!(
            resume.get(super::super::SKIP_UPLOAD_ENV),
            Some(&Some("1".into())),
            "a resume the cloud has moved past keeps its index to itself"
        );
        assert_eq!(
            resume.get(crate::analysis_channel::DISPATCH_ENV),
            Some(&None)
        );

        // A repo in the same resume that was never handed over is scanned the
        // ordinary way: no answers, and its index is uploaded.
        let untouched = scan_env(&Pass::Resume(BTreeMap::new()));
        assert_eq!(
            untouched.get(crate::analysis_channel::ANSWERS_ENV),
            Some(&None)
        );
        assert_eq!(untouched.get(super::super::SKIP_UPLOAD_ENV), Some(&None));

        for pass in [Pass::Facts, Pass::Infer] {
            let env = scan_env(&pass);
            for key in [
                crate::analysis_channel::DISPATCH_ENV,
                crate::analysis_channel::ANSWERS_ENV,
                super::super::SKIP_UPLOAD_ENV,
            ] {
                assert_eq!(
                    env.get(key),
                    Some(&None),
                    "{key} is cleared, not merely absent, on {pass:?}"
                );
            }
        }
    }

    /// Both variants strip the ambient CI context. On a laptop scan that is
    /// not tidiness: leaving `ACTIONS_ID_TOKEN_REQUEST_URL` in place would
    /// select the OIDC credential inside a scan that has none to mint.
    #[test]
    fn every_scan_strips_the_ci_context() {
        for pass in [Pass::Facts, Pass::Infer] {
            let env = scan_env(&pass);
            for key in ["ACTIONS_ID_TOKEN_REQUEST_URL", "GITHUB_REPOSITORY", "CI"] {
                assert_eq!(env.get(key), Some(&None), "{key} survived {pass:?}");
            }
        }
    }

    /// The cache directory and the previous generation are handed over the
    /// same way either way: a laptop scan's cross-repo download is the
    /// isolated local one, so this file is its only previous generation and
    /// without it every laptop rescan would be a cold, paid one.
    #[test]
    fn both_variants_carry_the_cache_dir_and_the_previous_generation() {
        for pass in [Pass::Facts, Pass::Infer] {
            let env = scan_env(&pass);
            assert_eq!(
                env.get(crate::cloud_storage::CACHE_DIR_ENV),
                Some(&Some("/build/repos".into()))
            );
            assert_eq!(
                env.get(crate::cloud_storage::ISOLATE_ENV),
                Some(&Some("1".into()))
            );
            assert_eq!(
                env.get(super::super::hosted::PREVIOUS_ENV),
                Some(&Some("/build/previous.json".into()))
            );
        }
    }

    /// One build, one run id. Every subprocess is handed this process's id and
    /// the name of the phase it is, so the second banner in a build's output
    /// reads as a phase of one run rather than as a second run with a key
    /// nothing can join (carrick#997 item 2).
    #[test]
    fn every_subprocess_carries_this_run_and_says_which_phase_it_is() {
        let expected = Some(&Some(crate::logging::run_id().to_string()));
        for pass in [Pass::Facts, Pass::Infer] {
            let env = scan_env(&pass);
            assert_eq!(env.get(crate::logging::RUN_ID_ENV), expected);
            assert_eq!(
                env.get(crate::logging::RUN_PHASE_ENV),
                Some(&Some("scan of api".into()))
            );
        }
        let env = env_of(&join_command(
            Path::new("/bin/carrick"),
            Path::new("/repos/api"),
            Path::new("/build/repos"),
            Path::new("/build/join.json"),
        ));
        assert_eq!(env.get(crate::logging::RUN_ID_ENV), expected);
        assert_eq!(
            env.get(crate::logging::RUN_PHASE_ENV),
            Some(&Some("workspace join".into()))
        );
    }

    /// A scan that panics keeps logging on the way down, so the last twelve
    /// lines were shutdown noise and the sentence naming the panic had been
    /// evicted before anyone read the failure (carrick#936).
    #[test]
    fn a_failed_scan_reports_the_panic_it_died_of_wherever_it_fell() {
        let _serialised = SCAN_STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            "for i in $(seq 1 20); do echo \"starting step $i\" >&2; done; \
             echo \"thread 'main' panicked at src/engine/mod.rs:1045: capacity overflow\" >&2; \
             for i in $(seq 1 40); do echo \"shutting down $i\" >&2; done; exit 1",
        );
        let error = run_scan(
            command,
            "scan of /repos/api",
            Reporting {
                working: "indexing api".to_string(),
                done: "indexed api".to_string(),
            },
            HEARTBEAT,
        )
        .expect_err("the child exited non-zero");

        assert!(
            error.contains("panicked at src/engine/mod.rs:1045"),
            "{error}"
        );
        // The two ends are still there, and the middle says how much is not.
        assert!(error.contains("starting step 1\n"), "{error}");
        assert!(error.contains("shutting down 40"), "{error}");
        assert!(error.contains("line(s) not shown"), "{error}");
        // And a bounded excerpt: the whole 61 lines are not repeated.
        assert!(error.lines().count() < 35, "{error}");
    }

    /// The elided middle is where a multi-service scan does its work, so the
    /// line that named the failure was the line the excerpt dropped: the head
    /// was the banner, the tail was the shutdown, and the service that broke
    /// was in neither (carrick#1023 item 8). Terminal control sequences go too:
    /// the excerpt is read back out of a JSON string and out of a log file,
    /// and neither renders them (item 6).
    #[test]
    fn a_failed_scan_keeps_the_lines_that_name_the_failure_and_no_escapes() {
        let _serialised = SCAN_STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            "for i in $(seq 1 20); do echo \"starting step $i\" >&2; done; \
             printf '\\033[31manalysis of gateway failed: deno resolve\\033[0m\\n' >&2; \
             echo 'error: LedgerEntry could not be resolved' >&2; \
             for i in $(seq 1 40); do echo \"shutting down $i\" >&2; done; exit 1",
        );
        let failure = run_scan(
            command,
            "scan of /repos/api",
            Reporting {
                working: "indexing api".to_string(),
                done: "indexed api".to_string(),
            },
            HEARTBEAT,
        )
        .expect_err("the child exited non-zero");

        assert!(
            failure.contains("analysis of gateway failed: deno resolve"),
            "the line that named the failure survives the elision:\n{failure}"
        );
        assert!(
            failure.contains("error: LedgerEntry could not be resolved"),
            "and so does the one that named the cause:\n{failure}"
        );
        assert!(
            !failure.contains('\u{1b}'),
            "no escape reaches the JSON body or the log:\n{failure:?}"
        );
        assert!(failure.contains("line(s) not shown"), "{failure}");
    }

    /// The excerpt is text by the time anything reads it, whatever the child
    /// wrote (carrick#1023 item 6).
    #[test]
    fn escape_sequences_are_stripped_from_a_kept_line() {
        assert_eq!(strip_ansi("\u{1b}[31mfailed\u{1b}[0m: two"), "failed: two");
        assert_eq!(strip_ansi("\u{1b}[?25lhidden cursor"), "hidden cursor");
        assert_eq!(
            strip_ansi("\u{1b}]0;a title\u{7}indexing"),
            "indexing",
            "an OSC title runs to its terminator, not to the end of the line"
        );
        assert_eq!(strip_ansi("plain"), "plain", "text is left alone");
    }

    /// The bar carries what the scan said about why it is slow beside its
    /// counts, and a notice before any count still shows (carrick#1122).
    #[test]
    fn the_bar_shows_why_the_scan_is_slow() {
        let update = crate::progress::Update {
            service: "api".to_string(),
            service_index: 1,
            service_total: 1,
            phase: crate::progress::Phase::Files,
            done: 40,
            total: 120,
        };
        assert_eq!(
            bar_message(
                "indexing api",
                Some(&update),
                Some("model busy: slowing analyze-file to 4 requests at a time")
            ),
            "indexing api: 40 of 120 files (model busy: slowing analyze-file to 4 requests at a time)"
        );
        assert_eq!(
            bar_message(
                "indexing api",
                None,
                Some("gateway busy: pacing model requests to 5 a second")
            ),
            "indexing api (gateway busy: pacing model requests to 5 a second)"
        );
        assert_eq!(
            bar_message("indexing api", Some(&update), None),
            "indexing api: 40 of 120 files"
        );
    }

    /// A notice line from the child is lifted out of the stream, not kept in a
    /// failure's excerpt as log text.
    #[test]
    fn a_notice_from_the_child_is_not_part_of_the_excerpt() {
        let _serialised = SCAN_STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            "echo '@carrick-notice {\"text\":\"model busy: slowing analyze-file to 4 requests at a time\"}' >&2; \
             echo 'plain log line' >&2; exit 1",
        );
        let error = run_scan(
            command,
            "scan of api",
            Reporting {
                working: "indexing api".to_string(),
                done: "indexed api".to_string(),
            },
            HEARTBEAT,
        )
        .expect_err("the child exited non-zero");
        assert!(!error.contains("@carrick-notice"), "{error}");
        assert!(error.contains("plain log line"), "{error}");
    }

    /// The sentence a failing scan states on the progress channel leads the
    /// error, ahead of its log, and the marker line itself is not part of the
    /// excerpt (carrick#1103).
    #[test]
    fn a_failed_scan_leads_with_the_reason_it_stated() {
        let _serialised = SCAN_STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            "echo 'Using TeeStorage (laptop scan: cloud upload + local cache)' >&2; \
             echo '@carrick-failure {\"stage\":\"discovery\",\"reason\":\"A scan of acme/api is already running.\"}' >&2; \
             exit 1",
        );
        let error = run_scan(
            command,
            "scan of api",
            Reporting {
                working: "indexing api".to_string(),
                done: "indexed api".to_string(),
            },
            HEARTBEAT,
        )
        .expect_err("the child exited non-zero");
        assert_eq!(
            error.lines().next(),
            Some("the scan of api failed: A scan of acme/api is already running.")
        );
        assert!(
            error.contains("TeeStorage"),
            "the excerpt is kept for --json: {error}"
        );
        assert!(!error.contains("@carrick-failure"), "{error}");

        // A scan that dies without stating a reason still leads with a sentence.
        let mut silent = Command::new("sh");
        silent
            .arg("-c")
            .arg("echo 'thread main panicked at x' >&2; exit 101");
        let error = run_scan(
            silent,
            "scan of api",
            Reporting {
                working: "indexing api".to_string(),
                done: "indexed api".to_string(),
            },
            HEARTBEAT,
        )
        .expect_err("the child exited non-zero");
        let first = error.lines().next().unwrap_or_default();
        assert!(
            first.starts_with("the scan of api failed: it exited ("),
            "{error}"
        );
        assert!(first.ends_with(") without saying why"), "{error}");
    }

    /// A failure short enough to state in full is stated in full: nothing is
    /// dropped and no "not shown" line appears.
    #[test]
    fn a_short_failure_is_repeated_whole() {
        let _serialised = SCAN_STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("echo 'could not read carrick.json' >&2; exit 2");
        let error = run_scan(
            command,
            "scan of /repos/api",
            Reporting {
                working: "indexing api".to_string(),
                done: "indexed api".to_string(),
            },
            HEARTBEAT,
        )
        .expect_err("the child exited non-zero");
        assert!(error.ends_with("could not read carrick.json"), "{error}");
        assert!(!error.contains("not shown"), "{error}");
    }

    /// Every test that drives [`run_scan`] writes through the one process-wide
    /// scan-state record, so two of them at once are two phases fighting over
    /// it (carrick#592's family). They take this in turn instead.
    static SCAN_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A scan's long stretches are its quiet ones — a model call, a type
    /// check — and the log's pulse used to be driven entirely by lines
    /// arriving from the child. So the detached log stopped moving exactly
    /// when a reader most needed it, on a part-finished count, while `carrick
    /// status` was reporting a different service (carrick#1007 item 3).
    #[test]
    fn a_silent_child_still_moves_the_scan_state() {
        let _serialised = SCAN_STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().unwrap();
        super::super::scan_state::begin(dir.path(), "heartbt1", dir.path(), true);
        let state = super::super::scan_state::state_file(dir.path(), "heartbt1");
        let at_start = std::fs::read_to_string(&state).unwrap();

        let update = crate::progress::Update {
            service: "ledger".to_string(),
            service_index: 2,
            service_total: 2,
            phase: crate::progress::Phase::Files,
            done: 1,
            total: 2,
        };
        let mut command = Command::new("sh");
        // One count, then a silence longer than the state file's own write
        // gap: the shape of a scan waiting on the model. Nothing arrives from
        // the child in that window, so before this fix nothing was written in
        // it either.
        command.arg("-c").arg(format!(
            "echo '@carrick-progress {}' >&2; sleep 2.5",
            serde_json::to_string(&update).unwrap()
        ));
        run_scan(
            command,
            "scan of /repos/ledger",
            Reporting {
                working: "indexing ledger".to_string(),
                done: "indexed ledger".to_string(),
            },
            std::time::Duration::from_millis(100),
        )
        .unwrap();

        let after = std::fs::read_to_string(&state).unwrap();
        assert_ne!(at_start, after, "the state file never moved");
        assert!(
            !after.contains("\"phase\": \"starting\""),
            "the pulse never replaced the starting record: {after}"
        );
        let read: super::super::scan_state::ScanState = serde_json::from_str(&after).unwrap();
        assert_eq!(read.phase, "indexing ledger");
        // The pulse states where the scan is; it must not erase where it got
        // to, which is the only count a reader has.
        assert_eq!(read.progress, Some(update));
        super::super::scan_state::finish(None);
    }

    /// A first `carrick index` in the foreground records its scan before
    /// anything has made `.carrick`, and an interrupted one leaves a failed
    /// record naming the signal rather than `running` with a dead pid
    /// (carrick#1132).
    #[test]
    fn a_first_foreground_scan_is_recorded_and_an_interrupted_one_says_why() {
        let _serialised = SCAN_STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let workspace = tempfile::tempdir().unwrap();
        let index_dir = workspace.path().join(".carrick");
        assert!(!index_dir.exists());

        super::super::scan_state::begin(&index_dir, "fground1", workspace.path(), true);
        let running = super::super::scan_state::read_all(&index_dir);
        assert_eq!(running.len(), 1, "no record was written: {running:?}");
        assert!(running[0].is_running(), "{running:?}");

        super::super::scan_state::interrupted("SIGINT");
        let ended = super::super::scan_state::read_all(&index_dir);
        assert_eq!(ended.len(), 1, "{ended:?}");
        assert_eq!(
            ended[0].status,
            super::super::scan_state::ScanStatus::Failed,
            "{ended:?}"
        );
        assert_eq!(
            ended[0].reason(),
            Some("interrupted by SIGINT before anything was uploaded; run the command again.")
        );
        assert!(ended[0].finished_at.is_some(), "{ended:?}");
        // The record is closed: a second signal writes nothing over it.
        super::super::scan_state::interrupted("SIGTERM");
        assert_eq!(super::super::scan_state::read_all(&index_dir), ended);
    }
}
