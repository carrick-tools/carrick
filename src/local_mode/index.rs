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
    /// What this run paid Carrick Cloud, one entry per repo that was scanned
    /// with `--infer`. Empty on the free pass, which pays for nothing.
    pub spend: crate::scan_spend::RunSpend,
}

/// Index every repo in the workspace, or re-index the one holding `only`.
///
/// `infer` turns each per-repo scan into a laptop scan: model analysis through
/// Carrick Cloud, an upload, and the same payload written into this build's
/// cache directory. The join phase never infers — it analyses nothing, it
/// reads blobs back.
pub fn run(workspace: &Workspace, only: Option<&str>, infer: bool) -> Result<IndexOutcome, String> {
    let started = Instant::now();
    // Build into a fresh generation. A failed scan leaves the last complete
    // read model intact; account changes never inherit previous local rows.
    let requested = only
        .map(|name| repo_for_service(workspace, &workspace.blobs_dir(), name))
        .transpose()?;
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
    let targets = if same_sources {
        requested
            .map(|path| vec![path])
            .unwrap_or_else(|| workspace.repos.clone())
    } else {
        workspace.repos.clone()
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
        infer,
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
    infer: bool,
) -> Result<IndexOutcome, String> {
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
    // The receipt, written as each figure lands rather than at the end: the
    // money is spent at the upload, so a run killed after paying still leaves
    // the record of it behind (carrick#995).
    let mut spend = crate::scan_spend::RunSpend::default();
    for (position, repo) in targets.iter().enumerate() {
        let name = repo_label(repo);
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
        if let Some(paid) = scan_repo(&exe, repo, &scan_dir, &previous, &name, infer)? {
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
    let join = join(&exe, join_target, blobs, &generation.join("join.json"))?;

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
                service.enrichment = hosted.service(Path::new(&repo.path), blob);
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

    Ok(IndexOutcome {
        index,
        scanned,
        elapsed_secs: started.elapsed().as_secs_f64(),
        spend,
    })
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

/// Phase 1 for one repo, and what its scan cost when it was a paid one.
fn scan_repo(
    exe: &Path,
    repo: &Path,
    blobs: &Path,
    previous: &Path,
    label: &str,
    infer: bool,
) -> Result<Option<crate::scan_spend::ScanSpend>, String> {
    let command = scan_command(exe, repo, blobs, previous, infer);
    run_scan(
        command,
        &format!("scan of {}", repo.display()),
        Reporting {
            working: format!("indexing {label}"),
            done: format!("indexed {label}"),
        },
    )
}

/// The subprocess one repo's phase-1 scan runs as.
///
/// Built separately from the spawn so a test can read back what a laptop scan
/// asks for and what a facts-only one does: the difference between them is
/// entirely in this environment, and it is the difference between a free pass
/// and a paid one.
fn scan_command(exe: &Path, repo: &Path, blobs: &Path, previous: &Path, infer: bool) -> Command {
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
    if infer {
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
    // Always: the ambient CI context would name every repo in the workspace
    // after the one whose shell this ran in, and on a laptop scan its OIDC
    // variables would select the wrong credential entirely.
    strip_ci_env(&mut command);
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
    )?;

    let text = std::fs::read_to_string(out)
        .map_err(|e| format!("the join wrote no result to {}: {e}", out.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("could not read the join result: {e}"))
}

/// The subprocess the join phase runs as, built apart from the spawn for the
/// same reason [`scan_command`] is: what this phase asks for is entirely in
/// its environment, and a test reads it back from here.
fn join_command(exe: &Path, repo: &Path, blobs: &Path, out: &Path) -> Command {
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
    command
}

/// Run one scan subprocess, and say what it printed if it failed.
///
/// The scan's stdout is dropped — it is a report that means nothing here — and
/// its stderr is read line by line rather than collected at the end, for two
/// reasons: the progress lines it carries are worth something only while the
/// scan is still running (carrick#955), and the useful half of a failure is
/// still the last few lines, which are kept as they go past.
/// What one phase of a build calls itself while it runs and once it is done.
struct Reporting {
    working: String,
    done: String,
}

fn run_scan(
    mut command: Command,
    what: &str,
    reporting: Reporting,
) -> Result<Option<crate::scan_spend::ScanSpend>, String> {
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start the {what}: {e}"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("the {what} produced no stderr to read"))?;
    let bar = crate::logging::spinner(&reporting.working);
    // The same update, to the two places that can be waiting on it: the
    // spinner a person is watching, and the state file `carrick status` reads
    // for a scan nobody is watching at all (carrick#992). The second is a
    // no-op unless this build was detached.
    super::scan_state::note(&reporting.working, None);
    let mut tail: VecDeque<String> = VecDeque::with_capacity(12);
    // What this scan paid, if it paid anything. It crosses on the same
    // channel as the progress, and for the same reason: this process swallows
    // the scan's output, so a figure it does not lift out is a figure nobody
    // ever sees (carrick#995).
    let mut spend = None;
    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
        if let Some(update) = crate::progress::parse(&line) {
            bar.set_message(format!("{}: {}", reporting.working, update.render()));
            super::scan_state::note(&reporting.working, Some(&update));
            continue;
        }
        if let Some(reported) = crate::scan_spend::parse(&line) {
            spend = Some(reported);
            continue;
        }
        if tail.len() == 12 {
            tail.pop_front();
        }
        tail.push_back(line);
    }
    let status = child
        .wait()
        .map_err(|e| format!("could not wait for the {what}: {e}"))?;
    if status.success() {
        crate::logging::finish_spinner(&bar, &reporting.done);
        return Ok(spend);
    }
    bar.finish_and_clear();
    Err(format!(
        "the {what} failed:\n{}",
        tail.into_iter().collect::<Vec<_>>().join("\n")
    ))
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
fn service_id(blob: &CloudRepoData) -> String {
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
fn read_blobs(blobs: &Path) -> Result<Vec<CloudRepoData>, String> {
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
fn build(
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
    if let Some(finding) = findings
        .iter()
        .find(|finding| finding_names(finding, operation))
    {
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

    fn scan_env(infer: bool) -> BTreeMap<String, Option<String>> {
        env_of(&scan_command(
            Path::new("/bin/carrick"),
            Path::new("/repos/api"),
            Path::new("/build/repos"),
            Path::new("/build/previous.json"),
            infer,
        ))
    }

    /// The indexer swallows a scan's output, so what a paid scan cost reaches
    /// this process only if this loop lifts it out — and it must come out of
    /// the stream rather than into the failure tail (carrick#995).
    #[test]
    fn the_indexer_lifts_the_figure_out_of_the_scan_it_swallowed() {
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
        )
        .unwrap();
        assert_eq!(lifted, Some(spend));
    }

    /// A free pass reports nothing, and nothing is not a scan that cost zero.
    #[test]
    fn a_scan_that_states_no_figure_reports_none() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("echo 'analysing' >&2");
        let lifted = run_scan(
            command,
            "scan of /repos/api",
            Reporting {
                working: "indexing api".to_string(),
                done: "indexed api".to_string(),
            },
        )
        .unwrap();
        assert!(lifted.is_none());
    }

    /// The default is unchanged and costs nothing: no model, no intents, no
    /// upload. `carrick refresh` runs from a session-start hook and this is
    /// the command it runs.
    #[test]
    fn a_scan_without_inference_asks_for_no_model_and_no_intents() {
        let env = scan_env(false);
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
        let env = scan_env(true);
        assert_eq!(
            env.get(crate::cloud_storage::LAPTOP_SCAN_ENV),
            Some(&Some("1".into()))
        );
        assert_eq!(env.get(super::super::NO_MODEL_ENV), Some(&None));
        assert_eq!(env.get("CARRICK_SKIP_INTENTS"), Some(&None));
    }

    /// Both variants strip the ambient CI context. On a laptop scan that is
    /// not tidiness: leaving `ACTIONS_ID_TOKEN_REQUEST_URL` in place would
    /// select the OIDC credential inside a scan that has none to mint.
    #[test]
    fn every_scan_strips_the_ci_context() {
        for infer in [false, true] {
            let env = scan_env(infer);
            for key in ["ACTIONS_ID_TOKEN_REQUEST_URL", "GITHUB_REPOSITORY", "CI"] {
                assert_eq!(env.get(key), Some(&None), "{key} survived infer={infer}");
            }
        }
    }

    /// The cache directory and the previous generation are handed over the
    /// same way either way: a laptop scan's cross-repo download is the
    /// isolated local one, so this file is its only previous generation and
    /// without it every laptop rescan would be a cold, paid one.
    #[test]
    fn both_variants_carry_the_cache_dir_and_the_previous_generation() {
        for infer in [false, true] {
            let env = scan_env(infer);
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
        for infer in [false, true] {
            let env = scan_env(infer);
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
}
